"""lapis-ble-v1: Python side of the cross-language vectors (Kotlin runs the same file)."""

from __future__ import annotations

import asyncio
import hashlib
import json
from pathlib import Path

import pytest
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey

from lapis_daemon.api.auth import hash_token
from lapis_daemon.ble_prov import crypto
from lapis_daemon.ble_prov.session import ProvisioningSession

VECTORS = json.loads(
    (
        Path(__file__).resolve().parents[2] / "tests" / "vectors" / "ble_crypto_vectors.json"
    ).read_text()
)
V = VECTORS["crypto_vector"]
D = V["derived"]


def _keys() -> crypto.SessionKeys:
    client = X25519PrivateKey.from_private_bytes(bytes.fromhex(V["client_priv_hex"]))
    device = X25519PrivateKey.from_private_bytes(bytes.fromhex(V["device_priv_hex"]))
    assert client.public_key().public_bytes_raw().hex() == V["client_pub_hex"]
    assert device.public_key().public_bytes_raw().hex() == V["device_pub_hex"]
    shared = crypto.compute_shared(client, bytes.fromhex(V["device_pub_hex"]))
    assert shared == crypto.compute_shared(device, bytes.fromhex(V["client_pub_hex"]))
    assert shared.hex() == V["shared_secret_hex"]
    return crypto.derive_keys(
        shared,
        bytes.fromhex(V["client_pub_hex"]),
        bytes.fromhex(V["device_pub_hex"]),
        bytes.fromhex(V["client_nonce_hex"]),
        bytes.fromhex(V["device_nonce_hex"]),
        V["pin"],
    )


def test_key_schedule_and_pin_proofs_match_vectors() -> None:
    assert VECTORS["version"] == 1 and V["proto_ver"] == crypto.PROTO_VER
    keys = _keys()
    assert len(keys.transcript) == 97
    assert keys.k_c2d.hex() == D["k_c2d_hex"]
    assert keys.k_d2c.hex() == D["k_d2c_hex"]
    assert keys.k_pin.hex() == D["k_pin_hex"]
    assert crypto.client_proof(keys).hex() == D["client_proof_hex"]
    assert crypto.server_proof(keys).hex() == D["server_proof_hex"]
    # A different PIN yields unrelated keys: that is what binds the session to the LCD.
    other = crypto.derive_keys(
        bytes.fromhex(V["shared_secret_hex"]),
        *(bytes.fromhex(V[k]) for k in ("client_pub_hex", "device_pub_hex")),
        *(bytes.fromhex(V[k]) for k in ("client_nonce_hex", "device_nonce_hex")),
        "482918",
    )
    assert other.k_c2d != keys.k_c2d and other.k_pin != keys.k_pin


def test_envelope_matches_vector_and_rejects_replay_and_tampering() -> None:
    keys = _keys()
    plaintext = V["plaintext_utf8"].encode()
    envelope = crypto.seal(keys.k_c2d, V["dir_byte"], V["seq"], plaintext)
    assert envelope.hex() == D["envelope_hex"]
    assert envelope[:4].hex() == "00000001" and len(envelope) == 4 + len(plaintext) + 16
    assert crypto.open_envelope(keys.k_c2d, crypto.DIR_C2D, envelope, 1) == plaintext

    with pytest.raises(crypto.EnvelopeError, match="sequence"):  # replay / out of order
        crypto.open_envelope(keys.k_c2d, crypto.DIR_C2D, envelope, 2)
    with pytest.raises(crypto.EnvelopeError, match="authentication"):  # wrong direction
        crypto.open_envelope(keys.k_c2d, crypto.DIR_D2C, envelope, 1)
    with pytest.raises(crypto.EnvelopeError, match="authentication"):  # wrong key
        crypto.open_envelope(keys.k_d2c, crypto.DIR_C2D, envelope, 1)
    flipped = envelope[:-1] + bytes([envelope[-1] ^ 1])
    with pytest.raises(crypto.EnvelopeError, match="authentication"):
        crypto.open_envelope(keys.k_c2d, crypto.DIR_C2D, flipped, 1)
    zero = bytes(4) + envelope[4:]
    with pytest.raises(crypto.EnvelopeError):  # seq 0 is never valid
        crypto.open_envelope(keys.k_c2d, crypto.DIR_C2D, zero, 0)
    with pytest.raises(crypto.EnvelopeError):
        crypto.seal(keys.k_c2d, crypto.DIR_C2D, 0, b"x")


def test_chunk_framing_vector_and_mtu_boundaries() -> None:
    f = VECTORS["chunk_framing_vector"]
    assert crypto.max_chunk_data(f["att_mtu"]) == f["max_chunk_data"] == 16
    assert crypto.max_chunk_data(185) == 178
    # Capped by the 512-byte attribute limit, not by MTU - 3 = 514.
    assert crypto.max_chunk_data(517) == 508 == crypto.MAX_ATT_VALUE - crypto.HEADER_LEN
    payload = bytes.fromhex(f["payload_hex"])
    frames = crypto.split_frames(f["msg_id"], f["flags"], payload, f["max_chunk_data"])
    assert [x.hex() for x in frames] == f["expected_frames_hex"]

    rx = crypto.Reassembler()
    assert [rx.feed(x) for x in frames] == [None, (f["flags"], payload)]
    for mtu in (23, 185, 517):
        big = bytes(range(256)) * 9  # 2304 bytes
        chunks = crypto.split_frames(200, crypto.FLAG_ENCRYPTED, big, crypto.max_chunk_data(mtu))
        assert all(len(c) <= mtu - 3 for c in chunks)
        out = [rx.feed(c) for c in chunks]
        assert out[:-1] == [None] * (len(chunks) - 1) and out[-1] == (crypto.FLAG_ENCRYPTED, big)


def test_reassembler_drops_bad_streams() -> None:
    rx = crypto.Reassembler()
    a, b, c = crypto.split_frames(1, 1, bytes(40), 16)
    assert rx.feed(a) is None
    with pytest.raises(crypto.FrameError, match="order"):
        rx.feed(c)  # skipped chunk 1
    with pytest.raises(crypto.FrameError):
        rx.feed(b)  # the buffer was dropped, so a continuation is out of order too
    assert rx.feed(a) is None
    with pytest.raises(crypto.FrameError):
        rx.feed(bytes([1, 1, 4, 1]) + bytes(16))  # total_chunks changed mid-message
    assert rx.feed(a) is None and rx.feed(crypto.reset_frame()) is None  # peer abort
    with pytest.raises(crypto.FrameError):
        rx.feed(b)
    assert rx.feed(a, now=100.0) is None
    with pytest.raises(crypto.FrameError):  # stale partial buffer is discarded after 5 s
        rx.feed(b, now=100.0 + crypto.REASSEMBLY_TIMEOUT_S + 0.1)
    for bad in (
        b"",
        b"\x01\x00",
        bytes([1, 0, 0, 1, 9]),
        bytes([1, 2, 2, 1, 9]),
        bytes([1, 0, 1, 2, 9]),
    ):
        with pytest.raises(crypto.FrameError):
            rx.feed(bad)
    assert crypto.reset_frame(7) == bytes([7, 0, 1, 0x80])
    with pytest.raises(crypto.FrameError, match="16 KiB"):
        crypto.split_frames(0, 1, bytes(crypto.MAX_MESSAGE + 1), 510)
    with pytest.raises(crypto.FrameError, match="255"):
        crypto.split_frames(0, 1, bytes(5000), 16)
    # A sender that ignores the cap is cut off on the receiving side.
    with pytest.raises(crypto.FrameError, match="16 KiB"):
        for idx in range(40):
            rx.feed(bytes([9, idx, 40, 1]) + bytes(510))


def test_handshake_rejects_bad_hellos_and_low_order_points() -> None:
    private, public = crypto.generate_keypair()
    hello = crypto.build_hello(public, bytes(16))
    assert len(hello) == crypto.HELLO_LEN == 49 and crypto.parse_hello(hello) == (public, bytes(16))
    with pytest.raises(crypto.HandshakeError, match="49"):
        crypto.parse_hello(hello[:-1])
    with pytest.raises(crypto.HandshakeError, match="version"):
        crypto.parse_hello(b"\x02" + hello[1:])
    with pytest.raises(crypto.HandshakeError):  # all-zero shared secret (RFC 7748 6.1)
        crypto.compute_shared(private, bytes(32))
    crypto.self_test()


def test_paired_vector_token_secret_keys_proofs_and_envelope() -> None:
    """Protocol 4.2: same handshake, `"tok:" + sha256(api_token)` in place of the PIN."""
    P, PD = VECTORS["paired_vector"], VECTORS["paired_vector"]["derived"]
    # The secret is exactly what paired_devices stores per phone: lowercase hex SHA-256.
    assert P["token_hash_hex"] == hash_token(P["api_token"]) == P["token_hash_hex"].lower()
    assert P["secret"] == "tok:" + P["token_hash_hex"] and len(P["secret"]) == 68

    pin_keys = _keys()
    keys = crypto.derive_keys(
        bytes.fromhex(V["shared_secret_hex"]),
        *(bytes.fromhex(V[k]) for k in ("client_pub_hex", "device_pub_hex")),
        *(bytes.fromhex(V[k]) for k in ("client_nonce_hex", "device_nonce_hex")),
        P["secret"],
    )
    salt = hashlib.sha256(keys.transcript + P["secret"].encode("ascii")).digest()
    assert salt.hex() == PD["salt_hex"]
    assert keys.k_c2d.hex() == PD["k_c2d_hex"] and keys.k_d2c.hex() == PD["k_d2c_hex"]
    assert keys.k_pin.hex() == PD["k_pin_hex"]
    assert crypto.client_proof(keys).hex() == PD["client_proof_hex"]
    assert crypto.server_proof(keys).hex() == PD["server_proof_hex"]
    plaintext = P["plaintext_utf8"].encode()
    assert json.loads(plaintext) == {"cmd": "token_verify", "proof": PD["client_proof_hex"]}
    envelope = crypto.seal(keys.k_c2d, V["dir_byte"], V["seq"], plaintext)
    assert envelope.hex() == PD["envelope_hex"]
    # The PIN's keys do not open it: that is how the device tells the two modes apart.
    with pytest.raises(crypto.EnvelopeError, match="authentication"):
        crypto.open_envelope(pin_keys.k_c2d, crypto.DIR_C2D, envelope, 1)


def test_session_answers_the_paired_vector() -> None:
    """The real session, given the vector's device key, opens its envelope and proves back."""
    P, PD = VECTORS["paired_vector"], VECTORS["paired_vector"]["derived"]
    sent: list[bytes] = []

    class Backend:
        def paired_hashes(self) -> list[tuple[str, str]]:
            return [("dev_07", P["token_hash_hex"])]

        def status(self, status: str, pin: str) -> None:
            raise AssertionError("a paired phone never touches the pairing screen")

    async def notify(frame: bytes) -> None:
        sent.append(frame)

    async def scenario() -> None:
        s = ProvisioningSession(Backend(), notify, allow_pin=False)  # type: ignore[arg-type]
        s._private = X25519PrivateKey.from_private_bytes(bytes.fromhex(V["device_priv_hex"]))
        s.public = bytes.fromhex(V["device_pub_hex"])
        s.device_nonce = bytes.fromhex(V["device_nonce_hex"])
        s.handshake_write(
            crypto.build_hello(
                bytes.fromhex(V["client_pub_hex"]), bytes.fromhex(V["client_nonce_hex"])
            )
        )
        envelope = bytes.fromhex(PD["envelope_hex"])
        for frame in crypto.split_frames(0, crypto.FLAG_ENCRYPTED, envelope, 508):
            await s.on_frame(frame)
        rx = crypto.Reassembler()
        (done,) = [d for d in (rx.feed(f) for f in sent) if d is not None]
        answer = crypto.open_envelope(bytes.fromhex(PD["k_d2c_hex"]), crypto.DIR_D2C, done[1], 1)
        assert json.loads(answer) == {
            "evt": "token_verified",
            "proof": PD["server_proof_hex"],
            "device_id": "dev_07",
        }

    asyncio.run(scenario())
