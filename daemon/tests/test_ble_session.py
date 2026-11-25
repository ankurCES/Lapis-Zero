"""lapis-ble-v1 session, driven by a fake phone that speaks the real wire format."""

from __future__ import annotations

import asyncio
import json
from typing import Any

import pytest

from lapis_daemon.ble_prov import crypto
from lapis_daemon.ble_prov import session as sess
from lapis_daemon.ble_prov.session import ProvisioningSession


class FakeBackend:
    def __init__(self) -> None:
        self.statuses: list[tuple[str, str]] = []
        self.registered: list[tuple[str, str]] = []
        self.wifi: list[tuple[str, str | None]] = []
        self.secrets: dict[str, str] = {}
        self.authkey: Any = "unset"
        self.served = False
        self.serve_ok = True
        self.up_error: str | None = None
        self.ts = [
            {
                "state": "needs_login",
                "auth_url": "https://login.tailscale.com/a/abc",
                "fqdn": "",
                "ip": "",
            },
            {
                "state": "needs_login",
                "auth_url": "https://login.tailscale.com/a/abc",
                "fqdn": "",
                "ip": "",
            },
            {
                "state": "connected",
                "auth_url": None,
                "fqdn": "lapis-zero.tail.ts.net",
                "ip": "100.1.2.3",
            },
        ]

    async def register_device(self, name: str, model: str) -> tuple[str, str]:
        self.registered.append((name, model))
        return "dev_01", "token-" + "x" * 37

    async def wifi_scan(self) -> list[dict[str, Any]]:
        return [{"ssid": "HomeWiFi", "rssi": -58, "secured": True}]

    async def wifi_connect(self, ssid: str, psk: str | None) -> dict[str, Any]:
        self.wifi.append((ssid, psk))
        if psk == "wrong":
            return {"state": "failed", "local_ip": ""}
        return {"state": "connected", "local_ip": "192.168.1.42"}

    def tailscale_up(self, authkey: str | None) -> None:
        self.authkey = authkey

    def tailscale_up_error(self) -> str | None:
        return self.up_error

    async def tailscale_status(self) -> dict[str, Any]:
        return self.ts.pop(0) if len(self.ts) > 1 else self.ts[0]

    async def tailscale_serve(self) -> bool:
        self.served = True
        return self.serve_ok

    async def push_secrets(self, values: dict[str, str]) -> list[str]:
        self.secrets.update(values)
        return sorted(values)

    def status(self, status: str, pin: str) -> None:
        self.statuses.append((status, pin))


class Phone:
    """The Android side, built only from the protocol document."""

    def __init__(self, mtu: int = 23) -> None:
        self.mtu = mtu
        self.backend = FakeBackend()
        self.notified: list[bytes] = []
        self.disconnects = 0
        self.session = ProvisioningSession(
            self.backend, self._notify, self._disconnect, poll_interval=0.001
        )
        self.session.mtu = mtu
        self.rx = crypto.Reassembler()
        self.tx_seq = 0
        self.rx_seq = 1
        self.msg_id = 0

    async def _notify(self, frame: bytes) -> None:
        assert len(frame) <= self.mtu - 3, "notification exceeds the ATT payload"
        self.notified.append(frame)

    async def _disconnect(self) -> None:
        self.disconnects += 1

    def handshake(self, pin: str) -> None:
        self.private, self.public = crypto.generate_keypair()
        self.nonce = bytes(range(16))
        self.session.handshake_write(crypto.build_hello(self.public, self.nonce))
        self.device_pub, self.device_nonce = crypto.parse_hello(self.session.handshake_read())
        self.rekey(pin)

    def rekey(self, pin: str) -> None:
        shared = crypto.compute_shared(self.private, self.device_pub)
        self.keys = crypto.derive_keys(
            shared, self.public, self.device_pub, self.nonce, self.device_nonce, pin
        )
        self.tx_seq, self.rx_seq = 0, 1

    def frames(self, command: dict[str, Any], seq: int | None = None) -> list[bytes]:
        if seq is None:
            self.tx_seq += 1
            seq = self.tx_seq
        body = json.dumps(command, separators=(",", ":")).encode()
        envelope = crypto.seal(self.keys.k_c2d, crypto.DIR_C2D, seq, body)
        frames = crypto.split_frames(
            self.msg_id, crypto.FLAG_ENCRYPTED, envelope, crypto.max_chunk_data(self.mtu)
        )
        self.msg_id = (self.msg_id + 1) % 256
        return frames

    async def send(self, command: dict[str, Any], seq: int | None = None) -> None:
        for frame in self.frames(command, seq):
            await self.session.on_frame(frame)

    def events(self) -> list[dict[str, Any]]:
        out = []
        frames, self.notified = self.notified, []
        for frame in frames:
            done = self.rx.feed(frame)
            if done is None:
                continue
            flags, payload = done
            assert flags == crypto.FLAG_ENCRYPTED
            plain = crypto.open_envelope(self.keys.k_d2c, crypto.DIR_D2C, payload, self.rx_seq)
            self.rx_seq += 1
            out.append(json.loads(plain))
        return out

    async def verify(self) -> dict[str, Any]:
        await self.send(
            {
                "cmd": "pin_verify",
                "proof": crypto.client_proof(self.keys).hex(),
                "device_name": "Pixel",
                "device_model": "Pixel 9 Pro",
            }
        )
        (event,) = self.events()
        return event


@pytest.mark.parametrize("mtu", [23, 185, 517])
def test_full_provisioning_flow(mtu: int) -> None:
    async def scenario() -> None:
        phone = Phone(mtu)
        s = phone.session
        assert len(s.pin) == 6 and s.pin.isdigit()
        phone.handshake(s.pin)

        verified = await phone.verify()
        assert verified["evt"] == "pin_verified" and verified["device_id"] == "dev_01"
        # Mutual proof: the phone checks the device knew the PIN too.
        assert bytes.fromhex(verified["proof"]) == crypto.server_proof(phone.keys)
        assert verified["api_token"].startswith("token-") and s.verified
        assert phone.backend.registered == [("Pixel", "Pixel 9 Pro")]

        await phone.send({"cmd": "wifi_scan"})
        assert phone.events() == [
            {"evt": "wifi_list", "networks": [{"ssid": "HomeWiFi", "rssi": -58, "secured": True}]}
        ]
        await phone.send({"cmd": "wifi_connect", "ssid": "HomeWiFi", "psk": "wrong"})
        assert [e["state"] for e in phone.events()] == ["connecting", "failed"]
        await phone.send({"cmd": "wifi_connect", "ssid": "HomeWiFi", "psk": "hunter22"})
        assert phone.events()[-1] == {
            "evt": "wifi_status",
            "state": "connected",
            "local_ip": "192.168.1.42",
        }

        await phone.send({"cmd": "tailscale_up", "authkey": None})
        await asyncio.wait_for(s._tailscale, 2)
        assert phone.events() == [
            {"evt": "tailscale_auth", "url": "https://login.tailscale.com/a/abc"},  # once
            {
                "evt": "ready",
                "tailscale_fqdn": "lapis-zero.tail.ts.net",
                "tailscale_ip": "100.1.2.3",
            },
        ]
        assert phone.backend.authkey is None and phone.backend.served

        key = "sk-ant-" + "k" * 200  # forces multi-chunk even at MTU 185
        await phone.send({"cmd": "push_secrets", "secrets": {"ANTHROPIC_API_KEY": key}})
        assert phone.events() == [{"evt": "secrets_saved", "keys": ["ANTHROPIC_API_KEY"]}]
        assert phone.backend.secrets == {"ANTHROPIC_API_KEY": key}
        await phone.send({"cmd": "push_secrets", "secrets": {"bad name": "x"}})
        assert phone.events()[0]["evt"] == "error"
        await phone.send({"cmd": "make_coffee"})
        assert phone.events() == [
            {"evt": "error", "cmd": "make_coffee", "message": "unknown command"}
        ]

        await phone.send({"cmd": "finish_provisioning"})
        assert phone.events() == [{"evt": "bye"}] and s.done.is_set()
        assert [st for st, _ in phone.backend.statuses] == [
            "connected", "verified", "wifi", "wifi", "tailscale", "done",
        ]  # fmt: skip
        assert phone.disconnects == 0

    asyncio.run(scenario())


def test_tailscale_failures_are_reported_instead_of_a_false_ready() -> None:
    async def scenario() -> None:
        phone = Phone(185)
        s = phone.session
        phone.handshake(s.pin)
        await phone.verify()

        # Joined, but the tailnet has no HTTPS certificates: nothing is served.
        phone.backend.serve_ok = False
        await phone.send({"cmd": "tailscale_up", "authkey": None})
        await asyncio.wait_for(s._tailscale, 2)
        events = phone.events()
        assert not any(e["evt"] == "ready" for e in events)
        assert events[-1]["evt"] == "error" and events[-1]["cmd"] == "tailscale_up"
        assert "HTTPS Certificates" in events[-1]["message"]

        # A rejected auth key: no login URL to wait for, `tailscale up` already gave up.
        phone.backend.ts = [{"state": "needs_login", "auth_url": None, "fqdn": "", "ip": ""}]
        phone.backend.up_error = "backend error: invalid key: API key does not exist"
        await phone.send({"cmd": "tailscale_up", "authkey": "tskey-auth-bad"})
        await asyncio.wait_for(s._tailscale, 2)
        assert phone.events() == [
            {"evt": "error", "cmd": "tailscale_up", "message": phone.backend.up_error}
        ]

        # A key sent while tailscaled still holds an old login link: the link is stale,
        # the phone must not be sent to it.
        phone.backend.up_error = None
        stale = {"state": "needs_login", "auth_url": "https://login.tailscale.com/a/old"}
        joined = {"state": "connected", "auth_url": None, "fqdn": "lapis-zero.t.ts.net", "ip": "1"}
        phone.backend.ts = [{**stale, "fqdn": "", "ip": ""}, joined]
        phone.backend.serve_ok = True
        await phone.send({"cmd": "tailscale_up", "authkey": "tskey-auth-good"})
        await asyncio.wait_for(s._tailscale, 2)
        assert [e["evt"] for e in phone.events()] == ["ready"]
        assert phone.backend.authkey == "tskey-auth-good"

        # Already joined (a retry after enabling HTTPS, key still in the field): nothing
        # is re-submitted, the node only needs serve + ready.
        phone.backend.authkey = "unset"
        phone.backend.ts = [joined]
        await phone.send({"cmd": "tailscale_up", "authkey": "tskey-auth-good"})
        await asyncio.wait_for(s._tailscale, 2)
        assert [e["evt"] for e in phone.events()] == ["ready"]
        assert phone.backend.authkey == "unset", "tailscale up must not run on a joined node"

    asyncio.run(scenario())


def test_wrong_pin_then_right_pin_keeps_seq_one() -> None:
    async def scenario() -> None:
        phone = Phone()
        s = phone.session
        wrong = "000000" if s.pin != "000000" else "111111"
        phone.handshake(wrong)
        await phone.send({"cmd": "pin_verify", "proof": crypto.client_proof(phone.keys).hex()})
        assert phone.notified == [] and s.failures == 1 and not s.verified
        # Same hellos, corrected PIN, seq starts over at 1.
        phone.rekey(s.pin)
        assert (await phone.verify())["evt"] == "pin_verified"
        assert s.failures == 0

    asyncio.run(scenario())


def test_three_failures_rotate_pin_and_keys_and_disconnect(monkeypatch: pytest.MonkeyPatch) -> None:
    async def scenario() -> None:
        phone = Phone()
        s = phone.session
        old_pin, old_pub, old_nonce = s.pin, s.public, s.device_nonce
        phone.handshake("999999" if s.pin != "999999" else "888888")
        for _ in range(3):
            await phone.send({"cmd": "pin_verify", "proof": "00" * 32}, seq=1)
        assert phone.disconnects == 1 and not s.verified
        assert phone.backend.statuses[-1][0] == "locked"
        assert s.public != old_pub and s.device_nonce != old_nonce
        assert phone.backend.statuses[-1][1] == s.pin  # the LCD shows the new PIN
        assert phone.backend.registered == []
        # 5 s cooldown: no new handshake, frames ignored.
        with pytest.raises(crypto.HandshakeError, match="locked"):
            phone.handshake(s.pin)
        await s.on_frame(b"\x00\x00\x01\x01junk")
        assert phone.notified == []
        # After the cooldown the *old* PIN no longer works (unless the dice repeat it).
        monkeypatch.setattr(sess, "LOCKOUT_COOLDOWN_S", 0.0)
        s.locked_until = 0.0
        phone.handshake(s.pin)
        assert (await phone.verify())["evt"] == "pin_verified"
        assert old_pin == old_pin  # keep the name used

    asyncio.run(scenario())


def test_replay_wrong_proof_and_commands_before_verification() -> None:
    async def scenario() -> None:
        phone = Phone(185)
        s = phone.session
        phone.handshake(s.pin)
        # Right key, wrong proof: counts as a failure.
        await phone.send({"cmd": "pin_verify", "proof": "ab" * 32})
        assert s.failures == 1 and phone.notified == []
        # A provisioning command before pin_verify is refused the same way. Both
        # messages authenticated, so the sequence advanced (seq 1, 2; verify is 3).
        await phone.send({"cmd": "wifi_scan"})
        assert s.failures == 2 and not phone.backend.registered

        await phone.verify()
        first = phone.frames({"cmd": "wifi_scan"})
        for frame in first:
            await s.on_frame(frame)
        assert len(phone.events()) == 1
        for frame in first:  # replayed ciphertext: dropped, and not held against the PIN
            await s.on_frame(frame)
        assert phone.notified == [] and s.failures == 0 and s.verified

        # Out-of-order chunks: one reset frame, nothing else.
        a, b, c = crypto.split_frames(9, 1, bytes(500), crypto.max_chunk_data(185))
        await s.on_frame(a)
        await s.on_frame(c)
        assert phone.notified == [crypto.reset_frame()]
        phone.notified.clear()
        # An unencrypted message is never acted on.
        await s.on_frame(bytes([3, 0, 1, 0]) + b'{"cmd":"finish_provisioning"}')
        assert phone.notified == [crypto.reset_frame()] and not s.done.is_set()

    asyncio.run(scenario())


def test_bad_hello_is_rejected() -> None:
    s = Phone().session
    with pytest.raises(crypto.HandshakeError):
        s.handshake_write(b"\x01" + bytes(10))
    with pytest.raises(crypto.HandshakeError):
        s.handshake_write(b"\x01" + bytes(32) + bytes(16))  # low-order point
    assert len(s.handshake_read()) == 49
