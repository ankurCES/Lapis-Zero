"""lapis-ble-v1 wire crypto and chunk framing (BLE_PROVISIONING_PROTOCOL.md).

Layer 1: 4-byte chunk header `msg_id | chunk_idx | total_chunks | flags`.
Layer 2: X25519 ECDH -> HKDF-SHA256 (salt binds both public keys, both nonces
and the 6-digit PIN) -> one AES-256-GCM key per direction + a PIN-proof key.

Must stay byte-for-byte identical to the Kotlin implementation; both are tested
against tests/vectors/ble_crypto_vectors.json.
"""

from __future__ import annotations

import hashlib
import hmac
import struct
import time
from dataclasses import dataclass

from cryptography.exceptions import InvalidTag
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey, X25519PublicKey
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.hkdf import HKDF

PROTO_VER = 1
HELLO_LEN = 49  # proto_ver (1) || public key (32) || nonce (16)
HEADER_LEN = 4
FLAG_ENCRYPTED = 0x01
FLAG_RESET = 0x80
DIR_C2D = 0x01  # Android -> device, key k_c2d, CMD_TX
DIR_D2C = 0x02  # device -> Android, key k_d2c, EVT_RX
MAX_MESSAGE = 16_384  # CM0 RAM guard on a reassembled message
REASSEMBLY_TIMEOUT_S = 5.0
DEFAULT_ATT_MTU = 23
# The Bluetooth Core Specification caps an attribute value at 512 bytes whatever the
# MTU, and Android enforces it. At MTU 517 the plain MTU-3 formula would give 514.
MAX_ATT_VALUE = 512


class FrameError(ValueError):
    """Malformed, out-of-order or oversized chunk: drop the buffer, send one reset frame."""


class HandshakeError(ValueError):
    """Bad hello, wrong protocol version, or an all-zero shared secret."""


class EnvelopeError(ValueError):
    """Replay, out-of-order sequence, or a GCM authentication failure."""


# -- Layer 1: chunk framing ----------------------------------------------------


def max_chunk_data(att_mtu: int) -> int:
    """D_max = min(ATT_MTU - 3, 512) - 4 (ATT header, attribute cap, our chunk header)."""
    return max(1, min(att_mtu - 3, MAX_ATT_VALUE) - HEADER_LEN)


def split_frames(msg_id: int, flags: int, payload: bytes, chunk: int) -> list[bytes]:
    """Split one logical message into GATT-sized frames."""
    if not payload:
        raise FrameError("empty payload")
    if len(payload) > MAX_MESSAGE:
        raise FrameError("message exceeds 16 KiB")
    total = -(-len(payload) // chunk)
    if total > 255:
        raise FrameError("message needs more than 255 chunks at this MTU")
    return [
        bytes([msg_id & 0xFF, idx, total, flags]) + payload[idx * chunk : (idx + 1) * chunk]
        for idx in range(total)
    ]


def reset_frame(msg_id: int = 0) -> bytes:
    """Transport abort: the peer drops whatever it was reassembling."""
    return bytes([msg_id & 0xFF, 0, 1, FLAG_RESET])


class Reassembler:
    """Rebuilds logical messages from in-order chunks; one message at a time."""

    def __init__(self) -> None:
        self._key: tuple[int, int, int] | None = None  # (msg_id, total, flags)
        self._buf = bytearray()
        self._next = 0
        self._started = 0.0

    def reset(self) -> None:
        self._key, self._next = None, 0
        self._buf.clear()

    def feed(self, frame: bytes, now: float | None = None) -> tuple[int, bytes] | None:
        """Returns (flags, payload) when a message completes, else None.

        Raises FrameError after discarding the partial buffer; the caller answers
        with a single `reset_frame()`.
        """
        now = time.monotonic() if now is None else now
        if self._key is not None and now - self._started > REASSEMBLY_TIMEOUT_S:
            self.reset()
        if len(frame) < HEADER_LEN:
            self.reset()
            raise FrameError("short frame")
        msg_id, idx, total, flags = frame[:HEADER_LEN]
        data = frame[HEADER_LEN:]
        if flags & FLAG_RESET:
            self.reset()
            return None
        if total < 1 or idx >= total or not data or flags & ~FLAG_ENCRYPTED:
            self.reset()
            raise FrameError("invalid chunk header")
        if idx == 0:
            self.reset()
            self._key, self._started = (msg_id, total, flags), now
        elif self._key != (msg_id, total, flags) or idx != self._next:
            self.reset()
            raise FrameError("chunk out of order")
        self._buf += data
        self._next = idx + 1
        if len(self._buf) > MAX_MESSAGE:
            self.reset()
            raise FrameError("message exceeds 16 KiB")
        if self._next < total:
            return None
        payload = bytes(self._buf)
        self.reset()
        return flags, payload


# -- Layer 2: handshake and key schedule ---------------------------------------


@dataclass(frozen=True)
class SessionKeys:
    k_c2d: bytes
    k_d2c: bytes
    k_pin: bytes
    transcript: bytes


def generate_keypair() -> tuple[X25519PrivateKey, bytes]:
    private = X25519PrivateKey.generate()
    return private, private.public_key().public_bytes_raw()


def build_hello(public_key: bytes, nonce: bytes) -> bytes:
    if len(public_key) != 32 or len(nonce) != 16:
        raise HandshakeError("hello needs a 32-byte key and a 16-byte nonce")
    return bytes([PROTO_VER]) + public_key + nonce


def parse_hello(data: bytes) -> tuple[bytes, bytes]:
    """(public key, nonce) from a ClientHello / ServerHello."""
    if len(data) != HELLO_LEN:
        raise HandshakeError(f"hello must be {HELLO_LEN} bytes")
    if data[0] != PROTO_VER:
        raise HandshakeError(f"unsupported protocol version {data[0]}")
    return data[1:33], data[33:49]


def compute_shared(private: X25519PrivateKey, peer_public: bytes) -> bytes:
    """X25519 shared secret; aborts on the all-zero output (RFC 7748 section 6.1)."""
    try:
        shared = private.exchange(X25519PublicKey.from_public_bytes(peer_public))
    except ValueError as exc:  # OpenSSL refuses low-order points itself
        raise HandshakeError("invalid peer public key") from exc
    if not any(shared):
        raise HandshakeError("all-zero shared secret")
    return shared


def derive_keys(
    shared: bytes,
    client_pub: bytes,
    device_pub: bytes,
    client_nonce: bytes,
    device_nonce: bytes,
    pin: str,
) -> SessionKeys:
    transcript = bytes([PROTO_VER]) + client_pub + device_pub + client_nonce + device_nonce
    salt = hashlib.sha256(transcript + pin.encode("ascii")).digest()

    def expand(label: bytes) -> bytes:
        return HKDF(algorithm=hashes.SHA256(), length=32, salt=salt, info=label).derive(shared)

    return SessionKeys(
        expand(b"lapis-ble-v1-c2d"),
        expand(b"lapis-ble-v1-d2c"),
        expand(b"lapis-ble-v1-pin-confirm"),
        transcript,
    )


def client_proof(keys: SessionKeys) -> bytes:
    return hmac.new(keys.k_pin, b"client-pin-proof" + keys.transcript, hashlib.sha256).digest()


def server_proof(keys: SessionKeys) -> bytes:
    return hmac.new(keys.k_pin, b"server-pin-proof" + keys.transcript, hashlib.sha256).digest()


def _nonce_aad(dir_byte: int, seq: int) -> tuple[bytes, bytes]:
    seq_be = struct.pack(">I", seq)
    return bytes([dir_byte]) + b"\x00" * 7 + seq_be, bytes([PROTO_VER, dir_byte]) + seq_be


def seal(key: bytes, dir_byte: int, seq: int, plaintext: bytes) -> bytes:
    """`seq_be(4) || ciphertext || tag(16)`; the nonce is deterministic per (dir, seq)."""
    if not 1 <= seq <= 0xFFFFFFFF:
        raise EnvelopeError("sequence counter exhausted")
    nonce, aad = _nonce_aad(dir_byte, seq)
    return struct.pack(">I", seq) + AESGCM(key).encrypt(nonce, plaintext, aad)


def open_envelope(key: bytes, dir_byte: int, envelope: bytes, expected_seq: int) -> bytes:
    """Decrypt and authenticate; `seq` must be exactly the next expected one."""
    if len(envelope) < 4 + 16:
        raise EnvelopeError("envelope too short")
    (seq,) = struct.unpack(">I", envelope[:4])
    if seq == 0 or seq != expected_seq:
        raise EnvelopeError(f"unexpected sequence {seq} (want {expected_seq})")
    nonce, aad = _nonce_aad(dir_byte, seq)
    try:
        return AESGCM(key).decrypt(nonce, envelope[4:], aad)
    except InvalidTag as exc:
        raise EnvelopeError("authentication failed") from exc


def self_test() -> None:
    """RFC 7748 section 6.1 keys through the whole stack; raises on any mismatch."""
    alice = bytes.fromhex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")
    bob = bytes.fromhex("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb")
    c_sk, d_sk = (X25519PrivateKey.from_private_bytes(k) for k in (alice, bob))
    c_pk, d_pk = (k.public_key().public_bytes_raw() for k in (c_sk, d_sk))
    shared = compute_shared(c_sk, d_pk)
    expected = "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742"
    assert shared.hex() == expected == compute_shared(d_sk, c_pk).hex()
    keys = derive_keys(shared, c_pk, d_pk, bytes(16), bytes(range(16)), "482917")
    envelope = seal(keys.k_c2d, DIR_C2D, 1, b"{}")
    assert open_envelope(keys.k_c2d, DIR_C2D, envelope, 1) == b"{}"
    frames = split_frames(7, FLAG_ENCRYPTED, envelope, max_chunk_data(DEFAULT_ATT_MTU))
    rx = Reassembler()
    assert [rx.feed(f) for f in frames][-1] == (FLAG_ENCRYPTED, envelope)
