# Lapis-Zero BLE Provisioning Protocol (`lapis-ble-v1`)

This appendix defines the exact byte-level wire protocol shared between `daemon/src/lapis_daemon/ble_prov/` (Python / BlueZ D-Bus) and `android/.../data/ble/` (Kotlin / Nordic BLE KTX). Both implementations **must** match this specification byte-for-byte and pass the shared vectors in `tests/vectors/ble_crypto_vectors.json`.

---

## 1. GATT Profile & Session Lifecycle

* **Advertised Local Name:** `LapisZero-<LAST4_MAC>` (uppercase hex, e.g., `LapisZero-4F2A`)
* **Primary Service UUID:** `7c4f0001-9a3b-4c6e-8e21-51a0c0de0001`
* **On-Demand Lifecycle:** `lapis-ble-prov.service` advertises only during unpaired first boot or an explicit 5-minute pairing window (`Menu -> Pair Android App`). It shuts down immediately after `finish_provisioning` or timeout.

| Characteristic | UUID | Properties | Purpose |
| :--- | :--- | :--- | :--- |
| `HANDSHAKE` | `7c4f0001-9a3b-4c6e-8e21-51a0c0de0002` | Write, Read | Unencrypted ephemeral X25519 public key & nonce exchange (`ClientHello` / `ServerHello`). |
| `CMD_TX` | `7c4f0001-9a3b-4c6e-8e21-51a0c0de0003` | Write Without Response, Write | Chunked encrypted commands from Android $\to$ CardputerZero. |
| `EVT_RX` | `7c4f0001-9a3b-4c6e-8e21-51a0c0de0004` | Notify | Chunked encrypted responses/events from CardputerZero $\to$ Android. |

### ATT MTU & Limits

1. Android requests ATT MTU $= 517$ immediately upon connection.
2. Maximum bytes per ATT packet payload is $P_{\max} = \text{ATT\_MTU} - 3$ (minimum $20\text{ bytes}$ at default MTU $23$).
3. Maximum chunk data bytes per frame is $D_{\max} = P_{\max} - 4 = \text{ATT\_MTU} - 7$ (e.g., $16\text{ bytes}$ at MTU $23$; $178\text{ bytes}$ at MTU $185$). **A frame never exceeds $512\text{ bytes}$** (the Bluetooth attribute-value limit, enforced by Android), so at MTU $517$ both sides send at most $508$ data bytes per frame, not $510$. Receivers accept any chunk size.
4. **CM0 RAM Guard:** Maximum reassembled logical message size is **$16{,}384\text{ bytes}$ ($16\text{ KiB}$)**. Discard incomplete reassembly buffers after $5\text{ s}$.

---

## 2. Layer 1: Binary Chunk Framing (`CMD_TX` & `EVT_RX`)

Every GATT write on `CMD_TX` and notification on `EVT_RX` carries a **4-byte fixed header** (big-endian) followed by $1 \dots D_{\max}$ bytes of slice data:

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|    msg_id     |   chunk_idx   |  total_chunks |     flags     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                      chunk_data (1..D_max bytes)              |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

| Offset | Field | Type | Rules |
| :--- | :--- | :--- | :--- |
| `0` | `msg_id` | `uint8` | Logical message ID ($0\dots 255$, wraps modulo $256$). Increments by $+1$ per logical message per direction. |
| `1` | `chunk_idx` | `uint8` | Zero-based chunk index ($0 \le \text{chunk\_idx} < \text{total\_chunks}$). Must arrive strictly in order. |
| `2` | `total_chunks` | `uint8` | Total chunks in this message ($1 \le \text{total\_chunks} \le 255$). |
| `3` | `flags` | `uint8` | Bitmask: `0x01` = AES-256-GCM encrypted envelope; `0x80` = transport abort/reset (body empty). All other bits reserved (`0`). |

* **Reassembly Rule:** Concatenate `chunk_data` for `chunk_idx = 0 .. total_chunks - 1` sharing the same `(msg_id, total_chunks, flags)`. On out-of-order `chunk_idx`, mismatched `total_chunks`, or size $> 16{,}384\text{ bytes}$, drop the buffer and emit a single reset frame (`flags = 0x80`).

---

## 3. Layer 2: X25519 + HKDF-SHA256 + AES-256-GCM Key Schedule

### 3.1 Ephemeral Handshake (`HANDSHAKE` `...0002`)

Before enabling encrypted messaging, both peers generate fresh **ephemeral X25519** keypairs (RFC 7748) and 16-byte CSPRNG nonces. CardputerZero generates a fresh 6-digit zero-padded ASCII PIN (`000000`--`999999`) via CSPRNG and renders it on the $320 \times 170$ LCD (`pairing.rs`).

1. **Android writes `ClientHello` ($49\text{ bytes}$) to `HANDSHAKE`:**
   `proto_ver (0x01, 1B) || pk_C (32B) || client_nonce (16B)`
2. **Android reads `ServerHello` ($49\text{ bytes}$) from `HANDSHAKE`:**
   `proto_ver (0x01, 1B) || pk_D (32B) || device_nonce (16B)`
3. **Shared Secret:**
   $$K_{\text{shared}} = \text{X25519}(sk_C, pk_D) = \text{X25519}(sk_D, pk_C) \in \{0,1\}^{256}$$
   Both peers **must** abort if $K_{\text{shared}}$ is all-zero (`0x00 * 32`, per RFC 7748 Section 6.1) or if `proto_ver != 0x01`.

### 3.2 Transcript & HKDF-SHA256 Key Derivation (RFC 5869)

Bind both public keys, both nonces, the protocol version, and the 6-digit ASCII PIN into the HKDF salt:

```text
transcript = 0x01 || pk_C (32B) || pk_D (32B) || client_nonce (16B) || device_nonce (16B)   # 97 bytes
salt       = SHA256(transcript || ASCII(pin))                                               # 32 bytes
PRK        = HKDF-Extract(salt=salt, IKM=K_shared)                                          # 32 bytes

k_c2d      = HKDF-Expand(PRK, info=b"lapis-ble-v1-c2d",         L=32)  # Android -> Device AES-256-GCM key
k_d2c      = HKDF-Expand(PRK, info=b"lapis-ble-v1-d2c",         L=32)  # Device -> Android AES-256-GCM key
k_pin      = HKDF-Expand(PRK, info=b"lapis-ble-v1-pin-confirm", L=32)  # PIN proof HMAC-SHA256 key
```

### 3.3 AES-256-GCM Encrypted Envelope (`flags = 0x01`)

Reassembled payload layout inside Layer 1 chunks:

```text
[ seq: uint32_be (4B) ] [ ciphertext (N bytes) ] [ gcm_tag (16B) ]
```

* **Direction Byte (`dir_byte`):** `0x01` for Android $\to$ Device (`CMD_TX`, key `k_c2d`); `0x02` for Device $\to$ Android (`EVT_RX`, key `k_d2c`).
* **Sequence Counter (`seq`):** Unsigned 32-bit big-endian integer starting at `1` per direction, incrementing by `+1` per encrypted message. Receivers **must** enforce `seq == expected_seq` (or strict `seq > last_seq`) to prevent replay and reject wrap-around (`seq == 0`).
* **12-Byte Nonce (Deterministic, Zero Collision):**
  `nonce = dir_byte (1B) || 0x00000000000000 (7B) || seq_be (4B)`
* **6-Byte AAD (Associated Authenticated Data):**
  `aad = 0x01 (1B) || dir_byte (1B) || seq_be (4B)`
* **Plaintext:** Compact UTF-8 JSON object (`separators=(",", ":")`).

---

## 4. Physical PIN Verification & Provisioning Command Flow

After exchanging `ClientHello` / `ServerHello` and enabling notifications on `EVT_RX`:

1. **User Enters 6-Digit LCD PIN** in the Android onboarding screen. Android derives `k_c2d`, `k_d2c`, `k_pin`.
2. **Client PIN Proof (`CMD_TX`, `seq = 1`):**
   * `client_proof = HMAC-SHA256(k_pin, b"client-pin-proof" || transcript)`
   * Android encrypts with `k_c2d` and sends:
     ```json
     {"cmd":"pin_verify","proof":"<64-char-hex>","device_name":"Pixel","device_model":"Pixel 9 Pro"}
     ```
3. **Device Verification & Rate-Limit Lockout:**
   * CardputerZero decrypts with `k_c2d` and constant-time compares `client_proof`.
   * **Lockout Rule:** Maximum **3 failed attempts** (GCM tag error or proof mismatch) per pairing session. On the 3rd failure, CardputerZero immediately rotates the 6-digit PIN, rotates `(sk_D, pk_D)` and `device_nonce`, updates `pairing.rs`, and disconnects BLE with a $5\text{ s}$ cooldown.
   * On success, CardputerZero computes:
     `server_proof = HMAC-SHA256(k_pin, b"server-pin-proof" || transcript)`
   * Mints a fresh 256-bit `api_token`, stores its SHA-256 hash in SQLite `paired_devices`, and responds on `EVT_RX` (`seq = 1`):
     ```json
     {"evt":"pin_verified","proof":"<64-char-hex>","device_id":"dev_01","api_token":"<token>"}
     ```
4. **Client Mutual Verification:** Android constant-time verifies `server_proof` and writes `api_token` into Android Keystore before issuing any provisioning commands.
5. **Provisioning Commands (All Encrypted, `seq >= 2`):**
   * `{"cmd":"wifi_scan"}` $\to$ `{"evt":"wifi_list","networks":[{"ssid":"...","rssi":-58,"secured":true}]}`
   * `{"cmd":"wifi_connect","ssid":"...","psk":"..."}` $\to$ `{"evt":"wifi_status","state":"connected","local_ip":"192.168.1.42"}`
   * `{"cmd":"tailscale_up","authkey":null}` $\to$ `{"evt":"tailscale_auth","url":"https://login.tailscale.com/a/..."}` $\to$ `{"evt":"ready","tailscale_fqdn":"lapis-zero.tailnet.ts.net","tailscale_ip":"100.x.y.z"}`
   * `{"cmd":"push_secrets","secrets":{"ANTHROPIC_API_KEY":"..."}}` $\to$ `{"evt":"secrets_saved","keys":["ANTHROPIC_API_KEY"]}`
   * `{"cmd":"finish_provisioning"}` $\to$ `{"evt":"bye"}` (CardputerZero stops `lapis-ble-prov.service`).

### 4.1 Implementation Notes (as built in `daemon/src/lapis_daemon/ble_prov/`)

* **Errors:** a command that fails or is unknown is answered with
  `{"evt":"error","cmd":"<command name>","message":"<text>"}`. The session stays usable.
* **`wifi_connect`** first answers `{"evt":"wifi_status","state":"connecting","local_ip":""}`, then
  `{"evt":"wifi_status","state":"connected"|"failed","local_ip":"..."}`.
* **`tailscale_up`** emits `tailscale_auth` at most once per distinct URL and `ready` when the node
  is running and `tailscale serve` is configured; after 300 s without success it emits an `error`
  event for `tailscale_up`.
* **Sequence numbers** advance only when a message authenticates. A wrong PIN derives the wrong
  keys, so the device sees a GCM failure and keeps expecting the same `seq` (`1` at that point):
  the phone re-derives keys from the same hellos with the corrected PIN and resends `pin_verify`
  with `seq = 1`. After the third failure the device rotates PIN, keypair and nonce, and the
  phone must redo the handshake.
* **A message that authenticates but is refused** (wrong proof, or any command before
  `pin_verify` succeeded) counts as a failed attempt and does consume its `seq`. A replayed or
  out-of-order frame after verification is dropped silently and is not counted.
* **Reset frames** (`flags = 0x80`) are `msg_id 00 01 80`: `chunk_idx = 0`, `total_chunks = 1`, no body.
* **Pairing window:** advertising stops after 300 s; a phone that has already verified the PIN
  gets up to 600 s from that moment to finish Wi-Fi and Tailscale setup.

---

## 5. Cross-Language Deterministic Test Vectors (`tests/vectors/ble_crypto_vectors.json`)

Both Python (`daemon/tests/test_ble_crypto_vectors.py`) and Kotlin (`android/.../BleEnvelopeCryptoVectorTest.kt`) **must** test against the same frozen JSON file generated from RFC 7748 Section 6.1 keys and fixed inputs below.

### 5.1 Fixed Inputs & Verified Framing Vector

The X25519 keypairs and shared secret below are verbatim **RFC 7748 Section 6.1** test vectors; the chunking vector is exact at `att_mtu = 23` ($D_{\max} = 16$).

```json
{
  "version": 1,
  "chunk_framing_vector": {
    "att_mtu": 23,
    "max_chunk_data": 16,
    "msg_id": 7,
    "flags": 1,
    "payload_hex": "000102030405060708090a0b0c0d0e0f10111213",
    "expected_frames_hex": [
      "07000201000102030405060708090a0b0c0d0e0f",
      "0701020110111213"
    ]
  },
  "crypto_vector": {
    "proto_ver": 1,
    "pin": "482917",
    "client_priv_hex": "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a",
    "client_pub_hex":  "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a",
    "device_priv_hex": "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb",
    "device_pub_hex":  "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f",
    "shared_secret_hex": "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742",
    "client_nonce_hex": "000102030405060708090a0b0c0d0e0f",
    "device_nonce_hex": "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
    "seq": 1,
    "dir_byte": 1,
    "plaintext_utf8": "{\"cmd\":\"wifi_scan\"}",
    "derived": {
      "salt_hex": "<generated_by_scripts/gen_ble_vectors.py>",
      "k_c2d_hex": "<generated_by_scripts/gen_ble_vectors.py>",
      "k_d2c_hex": "<generated_by_scripts/gen_ble_vectors.py>",
      "k_pin_hex": "<generated_by_scripts/gen_ble_vectors.py>",
      "client_proof_hex": "<generated_by_scripts/gen_ble_vectors.py>",
      "server_proof_hex": "<generated_by_scripts/gen_ble_vectors.py>",
      "nonce_hex": "010000000000000000000001",
      "aad_hex": "010100000001",
      "envelope_hex": "<generated_by_scripts/gen_ble_vectors.py>"
    }
  }
}
```

### 5.2 Reference Vector Generator (`scripts/gen_ble_vectors.py`)

Run this script once during **Phase A1** to populate and commit `tests/vectors/ble_crypto_vectors.json`. Never edit derived hex values by hand.

```python
import hashlib, hmac, json, struct
from pathlib import Path
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey, X25519PublicKey
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.hkdf import HKDF

PROTO_VER = b"\x01"
PIN = "482917"
CLIENT_PRIV = bytes.fromhex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")
DEVICE_PRIV = bytes.fromhex("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb")
CLIENT_NONCE = bytes.fromhex("000102030405060708090a0b0c0d0e0f")
DEVICE_NONCE = bytes.fromhex("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff")
PLAINTEXT = b'{"cmd":"wifi_scan"}'
SEQ, DIR_BYTE = 1, 1

c_sk = X25519PrivateKey.from_private_bytes(CLIENT_PRIV)
d_sk = X25519PrivateKey.from_private_bytes(DEVICE_PRIV)
c_pk, d_pk = c_sk.public_key().public_bytes_raw(), d_sk.public_key().public_bytes_raw()
shared = c_sk.exchange(X25519PublicKey.from_public_bytes(d_pk))

transcript = PROTO_VER + c_pk + d_pk + CLIENT_NONCE + DEVICE_NONCE
salt = hashlib.sha256(transcript + PIN.encode("ascii")).digest()

def expand(label: bytes) -> bytes:
    return HKDF(algorithm=hashes.SHA256(), length=32, salt=salt, info=label).derive(shared)

k_c2d = expand(b"lapis-ble-v1-c2d")
k_d2c = expand(b"lapis-ble-v1-d2c")
k_pin = expand(b"lapis-ble-v1-pin-confirm")
client_proof = hmac.new(k_pin, b"client-pin-proof" + transcript, hashlib.sha256).digest()
server_proof = hmac.new(k_pin, b"server-pin-proof" + transcript, hashlib.sha256).digest()

seq_be = struct.pack(">I", SEQ)
nonce = bytes([DIR_BYTE]) + (b"\x00" * 7) + seq_be
aad = PROTO_VER + bytes([DIR_BYTE]) + seq_be
ct_and_tag = AESGCM(k_c2d).encrypt(nonce, PLAINTEXT, aad)
envelope = seq_be + ct_and_tag

out = Path("tests/vectors/ble_crypto_vectors.json")
out.parent.mkdir(parents=True, exist_ok=True)
out.write_text(json.dumps({
    "version": 1,
    "chunk_framing_vector": {
        "att_mtu": 23, "max_chunk_data": 16, "msg_id": 7, "flags": 1,
        "payload_hex": "000102030405060708090a0b0c0d0e0f10111213",
        "expected_frames_hex": [
            "07000201000102030405060708090a0b0c0d0e0f",
            "0701020110111213",
        ],
    },
    "crypto_vector": {
        "proto_ver": 1, "pin": PIN,
        "client_priv_hex": CLIENT_PRIV.hex(), "client_pub_hex": c_pk.hex(),
        "device_priv_hex": DEVICE_PRIV.hex(), "device_pub_hex": d_pk.hex(),
        "shared_secret_hex": shared.hex(),
        "client_nonce_hex": CLIENT_NONCE.hex(), "device_nonce_hex": DEVICE_NONCE.hex(),
        "seq": SEQ, "dir_byte": DIR_BYTE, "plaintext_utf8": PLAINTEXT.decode("utf-8"),
        "derived": {
            "salt_hex": salt.hex(), "k_c2d_hex": k_c2d.hex(),
            "k_d2c_hex": k_d2c.hex(), "k_pin_hex": k_pin.hex(),
            "client_proof_hex": client_proof.hex(), "server_proof_hex": server_proof.hex(),
            "nonce_hex": nonce.hex(), "aad_hex": aad.hex(), "envelope_hex": envelope.hex(),
        },
    },
}, indent=2) + "\n")
```

### 5.3 Kotlin Interoperability Notes (`android/`)

* **X25519:** Use `com.google.crypto.tink:tink-android` (`X25519.computeSharedSecret(privateKey, peerPublicKey)`) or JDK/Conscrypt `KeyAgreement.getInstance("XDH")` (API 33+). Prefer **Tink's `X25519`** because `minSdk = 29` (Android 10) lacks built-in `XDH` in JCA.
* **HKDF-SHA256:** `com.google.crypto.tink.subtle.Hkdf.computeHkdf("HmacSha256", sharedSecret, salt, info, 32)` matches Python `cryptography`'s `HKDF` output byte-for-byte.
* **AES-256-GCM:** Standard `Cipher.getInstance("AES/GCM/NoPadding")` with `GCMParameterSpec(128, nonce)` and `cipher.updateAAD(aad)` appends the 16-byte authentication tag directly after ciphertext—identical to Python `AESGCM.encrypt()`.