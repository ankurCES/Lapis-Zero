"""Generate tests/vectors/ble_crypto_vectors.json for lapis-ble-v1.

Verbatim from BLE_PROVISIONING_PROTOCOL.md section 5.2. Both the Python daemon
(daemon/tests/test_ble_crypto_vectors.py) and the Android app
(BleEnvelopeCryptoVectorTest) test against the frozen file this writes. Run it
from the repository root; never edit the derived values by hand.

    cd daemon && uv run python ../scripts/gen_ble_vectors.py   # (cwd-independent)
"""
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

# Paired-phone mode (protocol section 4.2): the same handshake, but the PIN in the salt is
# replaced by "tok:" + hex SHA-256 of the api_token the phone got when it paired.
API_TOKEN = "lapis-paired-phone-test-token-0123456789AB"
token_hash = hashlib.sha256(API_TOKEN.encode("ascii")).hexdigest()
p_secret = "tok:" + token_hash
p_salt = hashlib.sha256(transcript + p_secret.encode("ascii")).digest()

def p_expand(label: bytes) -> bytes:
    return HKDF(algorithm=hashes.SHA256(), length=32, salt=p_salt, info=label).derive(shared)

p_c2d, p_d2c, p_pin = p_expand(b"lapis-ble-v1-c2d"), p_expand(b"lapis-ble-v1-d2c"), p_expand(b"lapis-ble-v1-pin-confirm")
p_client = hmac.new(p_pin, b"client-pin-proof" + transcript, hashlib.sha256).digest()
p_server = hmac.new(p_pin, b"server-pin-proof" + transcript, hashlib.sha256).digest()
P_PLAINTEXT = ('{"cmd":"token_verify","proof":"' + p_client.hex() + '"}').encode()
p_envelope = seq_be + AESGCM(p_c2d).encrypt(nonce, P_PLAINTEXT, aad)

out = Path(__file__).resolve().parent.parent / "tests" / "vectors" / "ble_crypto_vectors.json"
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
    "paired_vector": {
        "comment": "crypto_vector keys, nonces, seq and dir; secret replaces the PIN",
        "api_token": API_TOKEN, "token_hash_hex": token_hash, "secret": p_secret,
        "plaintext_utf8": P_PLAINTEXT.decode("utf-8"),
        "derived": {
            "salt_hex": p_salt.hex(), "k_c2d_hex": p_c2d.hex(), "k_d2c_hex": p_d2c.hex(),
            "k_pin_hex": p_pin.hex(), "client_proof_hex": p_client.hex(),
            "server_proof_hex": p_server.hex(), "envelope_hex": p_envelope.hex(),
        },
    },
}, indent=2) + "\n")
