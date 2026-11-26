//! lapis-ble-v1 Layer 2: X25519 handshake, HKDF-SHA256 key schedule, AES-256-GCM envelope.
//!
//! Byte-for-byte the protocol of Lapis-Zero (`BLE_PROVISIONING_PROTOCOL.md`), so the same
//! Android companion pairs with both devices. Frozen by `tests/vectors/ble_crypto_vectors.json`.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey, StaticSecret};

pub const PROTO_VER: u8 = 0x01;
pub const HELLO_LEN: usize = 49;
pub const TRANSCRIPT_LEN: usize = 97;
/// Android -> device, key `k_c2d`, characteristic CMD_TX.
pub const DIR_C2D: u8 = 0x01;
/// Device -> Android, key `k_d2c`, characteristic EVT_RX.
pub const DIR_D2C: u8 = 0x02;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("hello must be {HELLO_LEN} bytes")]
    HelloLength,
    #[error("unsupported protocol version {0}")]
    Version(u8),
    #[error("all-zero shared secret")]
    ZeroShared,
    #[error("envelope too short")]
    Short,
    #[error("unexpected sequence {got} (want {want})")]
    Sequence { got: u32, want: u32 },
    #[error("sequence counter exhausted")]
    Exhausted,
    #[error("authentication failed")]
    Auth,
}

/// `proto_ver || public key (32) || nonce (16)`: ClientHello and ServerHello alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hello {
    pub public: [u8; 32],
    pub nonce: [u8; 16],
}

impl Hello {
    pub fn encode(&self) -> [u8; HELLO_LEN] {
        let mut out = [0u8; HELLO_LEN];
        out[0] = PROTO_VER;
        out[1..33].copy_from_slice(&self.public);
        out[33..].copy_from_slice(&self.nonce);
        out
    }

    pub fn decode(data: &[u8]) -> Result<Self, CryptoError> {
        if data.len() != HELLO_LEN {
            return Err(CryptoError::HelloLength);
        }
        if data[0] != PROTO_VER {
            return Err(CryptoError::Version(data[0]));
        }
        let mut hello = Hello {
            public: [0; 32],
            nonce: [0; 16],
        };
        hello.public.copy_from_slice(&data[1..33]);
        hello.nonce.copy_from_slice(&data[33..]);
        Ok(hello)
    }
}

/// One side's ephemeral X25519 keypair and handshake nonce.
pub struct Ephemeral {
    secret: StaticSecret,
    pub hello: Hello,
}

impl Ephemeral {
    pub fn generate() -> Self {
        let mut private = [0u8; 32];
        let mut nonce = [0u8; 16];
        OsRng.fill_bytes(&mut private);
        OsRng.fill_bytes(&mut nonce);
        Self::from_parts(private, nonce)
    }

    /// Fixed keys: test vectors only.
    pub fn from_parts(private: [u8; 32], nonce: [u8; 16]) -> Self {
        let secret = StaticSecret::from(private);
        let public = PublicKey::from(&secret).to_bytes();
        Ephemeral {
            secret,
            hello: Hello { public, nonce },
        }
    }

    /// X25519 with the peer; refuses the all-zero output (RFC 7748 section 6.1).
    pub fn shared(&self, peer_public: &[u8; 32]) -> Result<[u8; 32], CryptoError> {
        let shared = self
            .secret
            .diffie_hellman(&PublicKey::from(*peer_public))
            .to_bytes();
        if shared.iter().all(|b| *b == 0) {
            return Err(CryptoError::ZeroShared);
        }
        Ok(shared)
    }
}

/// The three session keys and the transcript they are bound to. No `Debug`: keys never print.
#[derive(Clone)]
pub struct SessionKeys {
    pub c2d: [u8; 32],
    pub d2c: [u8; 32],
    pub pin: [u8; 32],
    pub transcript: [u8; TRANSCRIPT_LEN],
}

pub fn transcript(client: &Hello, device: &Hello) -> [u8; TRANSCRIPT_LEN] {
    let mut t = [0u8; TRANSCRIPT_LEN];
    t[0] = PROTO_VER;
    t[1..33].copy_from_slice(&client.public);
    t[33..65].copy_from_slice(&device.public);
    t[65..81].copy_from_slice(&client.nonce);
    t[81..97].copy_from_slice(&device.nonce);
    t
}

/// `SHA256(transcript || ASCII(pin))`.
pub fn salt(transcript: &[u8; TRANSCRIPT_LEN], pin: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(transcript);
    h.update(pin.as_bytes());
    h.finalize().into()
}

pub fn derive(shared: &[u8; 32], client: &Hello, device: &Hello, pin: &str) -> SessionKeys {
    let transcript = transcript(client, device);
    let hk = Hkdf::<Sha256>::new(Some(&salt(&transcript, pin)), shared);
    let expand = |info: &[u8]| {
        let mut out = [0u8; 32];
        // 32 bytes is far below HKDF-SHA256's 8160-byte limit: this cannot fail.
        let _ = hk.expand(info, &mut out);
        out
    };
    SessionKeys {
        c2d: expand(b"lapis-ble-v1-c2d"),
        d2c: expand(b"lapis-ble-v1-d2c"),
        pin: expand(b"lapis-ble-v1-pin-confirm"),
        transcript,
    }
}

fn hmac_sha256(key: &[u8; 32], label: &[u8], transcript: &[u8]) -> [u8; 32] {
    // HMAC accepts keys of any length: new_from_slice cannot fail.
    let Ok(mut mac) = <Hmac<Sha256> as Mac>::new_from_slice(key) else {
        return [0; 32];
    };
    mac.update(label);
    mac.update(transcript);
    mac.finalize().into_bytes().into()
}

impl SessionKeys {
    pub fn client_proof(&self) -> [u8; 32] {
        hmac_sha256(&self.pin, b"client-pin-proof", &self.transcript)
    }

    pub fn server_proof(&self) -> [u8; 32] {
        hmac_sha256(&self.pin, b"server-pin-proof", &self.transcript)
    }

    /// Constant-time check of a client proof (hex or raw bytes are the caller's concern).
    pub fn verify_client_proof(&self, proof: &[u8]) -> bool {
        proof.len() == 32 && bool::from(self.client_proof().ct_eq(proof))
    }

    pub fn verify_server_proof(&self, proof: &[u8]) -> bool {
        proof.len() == 32 && bool::from(self.server_proof().ct_eq(proof))
    }
}

/// `dir_byte || 0x00 * 7 || seq_be`.
pub fn nonce(dir: u8, seq: u32) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[0] = dir;
    n[8..].copy_from_slice(&seq.to_be_bytes());
    n
}

/// `proto_ver || dir_byte || seq_be`.
pub fn aad(dir: u8, seq: u32) -> [u8; 6] {
    let mut a = [0u8; 6];
    a[0] = PROTO_VER;
    a[1] = dir;
    a[2..].copy_from_slice(&seq.to_be_bytes());
    a
}

/// `seq_be(4) || ciphertext || tag(16)`.
pub fn seal(key: &[u8; 32], dir: u8, seq: u32, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if seq == 0 {
        return Err(CryptoError::Exhausted);
    }
    let cipher = Aes256Gcm::new(key.into());
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce(dir, seq)),
            Payload {
                msg: plaintext,
                aad: &aad(dir, seq),
            },
        )
        .map_err(|_| CryptoError::Auth)?;
    let mut out = Vec::with_capacity(4 + ct.len());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Decrypts an envelope whose `seq` must be exactly `want` (no replay, no reordering).
pub fn open(key: &[u8; 32], dir: u8, envelope: &[u8], want: u32) -> Result<Vec<u8>, CryptoError> {
    if envelope.len() < 4 + 16 {
        return Err(CryptoError::Short);
    }
    let seq = u32::from_be_bytes([envelope[0], envelope[1], envelope[2], envelope[3]]);
    if seq == 0 || seq != want {
        return Err(CryptoError::Sequence { got: seq, want });
    }
    Aes256Gcm::new(key.into())
        .decrypt(
            Nonce::from_slice(&nonce(dir, seq)),
            Payload {
                msg: &envelope[4..],
                aad: &aad(dir, seq),
            },
        )
        .map_err(|_| CryptoError::Auth)
}

/// The device's end of an authenticated session: `seq` advances only on success.
pub struct DeviceChannel {
    keys: SessionKeys,
    rx_next: u32,
    tx_next: u32,
}

impl DeviceChannel {
    pub fn new(keys: SessionKeys) -> Self {
        DeviceChannel {
            keys,
            rx_next: 1,
            tx_next: 1,
        }
    }

    pub fn keys(&self) -> &SessionKeys {
        &self.keys
    }

    pub fn expected_rx(&self) -> u32 {
        self.rx_next
    }

    pub fn open(&mut self, envelope: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let pt = open(&self.keys.c2d, DIR_C2D, envelope, self.rx_next)?;
        self.rx_next = self.rx_next.checked_add(1).ok_or(CryptoError::Exhausted)?;
        Ok(pt)
    }

    /// Counts a message that authenticated but was refused (protocol 4.1: it consumes its seq).
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let env = seal(&self.keys.d2c, DIR_D2C, self.tx_next, plaintext)?;
        self.tx_next = self.tx_next.checked_add(1).ok_or(CryptoError::Exhausted)?;
        Ok(env)
    }
}

/// The phone's end, used by tests and the host-side simulator.
pub struct ClientChannel {
    keys: SessionKeys,
    rx_next: u32,
    tx_next: u32,
}

impl ClientChannel {
    pub fn new(keys: SessionKeys) -> Self {
        ClientChannel {
            keys,
            rx_next: 1,
            tx_next: 1,
        }
    }

    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let env = seal(&self.keys.c2d, DIR_C2D, self.tx_next, plaintext)?;
        self.tx_next = self.tx_next.checked_add(1).ok_or(CryptoError::Exhausted)?;
        Ok(env)
    }

    pub fn open(&mut self, envelope: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let pt = open(&self.keys.d2c, DIR_D2C, envelope, self.rx_next)?;
        self.rx_next = self.rx_next.checked_add(1).ok_or(CryptoError::Exhausted)?;
        Ok(pt)
    }

    /// Re-derive with a corrected PIN and resend from the same `seq` (protocol 4.1).
    pub fn rewind_tx(&mut self, keys: SessionKeys) {
        self.keys = keys;
        self.tx_next = 1;
    }
}
