"""lapis-ble-v1 provisioning session: handshake, PIN proof, lockout, commands.

Transport-independent on purpose: the BlueZ GATT server feeds it bytes and gets
frames to notify back, and the tests drive the same object with a fake phone.
"""

from __future__ import annotations

import asyncio
import contextlib
import hmac
import json
import secrets
import time
from collections.abc import Awaitable, Callable
from typing import Any, Protocol

from lapis_daemon.ble_prov import crypto
from lapis_daemon.config import SECRET_NAME_RE

MAX_FAILURES = 3
LOCKOUT_COOLDOWN_S = 5.0
TAILSCALE_TIMEOUT_S = 300.0
HTTPS_NOT_ENABLED = (
    "Joined the tailnet, but HTTPS is not enabled for it. In the Tailscale admin console "
    "(DNS page) enable MagicDNS and HTTPS Certificates, then retry."
)


class Backend(Protocol):
    """What provisioning does to the device; faked in tests."""

    async def register_device(self, name: str, model: str) -> tuple[str, str]: ...
    async def wifi_scan(self) -> list[dict[str, Any]]: ...
    async def wifi_connect(self, ssid: str, psk: str | None) -> dict[str, Any]: ...
    def tailscale_up(self, authkey: str | None) -> None: ...
    def tailscale_up_error(self) -> str | None: ...
    async def tailscale_status(self) -> dict[str, Any]: ...
    async def tailscale_serve(self) -> bool: ...
    async def push_secrets(self, values: dict[str, str]) -> list[str]: ...
    def status(self, status: str, pin: str) -> None: ...


class ProvisioningSession:
    def __init__(
        self,
        backend: Backend,
        notify: Callable[[bytes], Awaitable[None]],
        disconnect: Callable[[], Awaitable[None]] | None = None,
        *,
        tailscale_timeout: float = TAILSCALE_TIMEOUT_S,
        poll_interval: float = 2.0,
    ) -> None:
        self.backend = backend
        self._notify = notify
        self._disconnect = disconnect
        self.tailscale_timeout = tailscale_timeout
        self.poll_interval = poll_interval
        self.mtu = crypto.DEFAULT_ATT_MTU
        self.done = asyncio.Event()
        self.verified = False
        self.verified_at = 0.0
        self.failures = 0
        self.locked_until = 0.0
        self._rx = crypto.Reassembler()
        self._keys: crypto.SessionKeys | None = None
        self._rx_seq = 1
        self._tx_seq = 0
        self._tx_msg = 0
        self._tailscale: asyncio.Task[None] | None = None
        self._credentials = ("", "")
        self.pin = ""
        self.rotate()

    def rotate(self) -> None:
        """Fresh PIN, ephemeral keypair and device nonce (first use and after a lockout)."""
        self.pin = f"{secrets.randbelow(10**6):06d}"
        self._private, self.public = crypto.generate_keypair()
        self.device_nonce = secrets.token_bytes(16)
        self._keys = None
        self.verified = False
        self.failures = 0
        self._rx.reset()

    # -- HANDSHAKE characteristic ----------------------------------------------

    def handshake_read(self) -> bytes:
        """ServerHello."""
        return crypto.build_hello(self.public, self.device_nonce)

    def handshake_write(self, data: bytes) -> None:
        """ClientHello: derive the session keys (bound to the PIN on the LCD)."""
        if time.monotonic() < self.locked_until:
            raise crypto.HandshakeError("locked out; retry shortly")
        client_pub, client_nonce = crypto.parse_hello(bytes(data))
        shared = crypto.compute_shared(self._private, client_pub)
        self._keys = crypto.derive_keys(
            shared, client_pub, self.public, client_nonce, self.device_nonce, self.pin
        )
        self._rx_seq, self._tx_seq, self._tx_msg = 1, 0, 0
        self.verified = False
        self._rx.reset()
        self.backend.status("connected", self.pin)

    # -- CMD_TX / EVT_RX -------------------------------------------------------

    async def on_frame(self, frame: bytes) -> None:
        """One GATT write on CMD_TX."""
        if time.monotonic() < self.locked_until:
            return
        try:
            complete = self._rx.feed(bytes(frame))
        except crypto.FrameError:
            await self._notify(crypto.reset_frame())
            return
        if complete is None:
            return
        flags, payload = complete
        if not flags & crypto.FLAG_ENCRYPTED or self._keys is None:
            await self._notify(crypto.reset_frame())
            return
        try:
            plaintext = crypto.open_envelope(
                self._keys.k_c2d, crypto.DIR_C2D, payload, self._rx_seq
            )
            command = json.loads(plaintext)
            if not isinstance(command, dict):
                raise ValueError("command must be an object")
        except (crypto.EnvelopeError, ValueError):
            # A wrong PIN derives the wrong key, so it shows up right here.
            await self._failure()
            return
        self._rx_seq += 1
        await self._dispatch(command)

    async def _failure(self) -> None:
        if self.verified:
            return  # tampering after verification is dropped, never counted against the PIN
        self.failures += 1
        if self.failures < MAX_FAILURES:
            return
        self.locked_until = time.monotonic() + LOCKOUT_COOLDOWN_S
        self.rotate()
        self.backend.status("locked", self.pin)
        if self._disconnect is not None:
            with contextlib.suppress(Exception):
                await self._disconnect()

    async def _send(self, event: dict[str, Any]) -> None:
        assert self._keys is not None
        self._tx_seq += 1
        body = json.dumps(event, separators=(",", ":")).encode()
        envelope = crypto.seal(self._keys.k_d2c, crypto.DIR_D2C, self._tx_seq, body)
        frames = crypto.split_frames(
            self._tx_msg, crypto.FLAG_ENCRYPTED, envelope, crypto.max_chunk_data(self.mtu)
        )
        self._tx_msg = (self._tx_msg + 1) % 256
        for frame in frames:
            await self._notify(frame)

    async def _error(self, cmd: str, message: str) -> None:
        await self._send({"evt": "error", "cmd": cmd, "message": message})

    async def _dispatch(self, command: dict[str, Any]) -> None:
        assert self._keys is not None
        cmd = str(command.get("cmd", ""))
        if cmd == "pin_verify":
            await self._pin_verify(command)
            return
        if not self.verified:
            await self._failure()
            return
        try:
            if cmd == "wifi_scan":
                await self._send({"evt": "wifi_list", "networks": await self.backend.wifi_scan()})
            elif cmd == "wifi_connect":
                await self._wifi_connect(command)
            elif cmd == "tailscale_up":
                self._start_tailscale(command.get("authkey") or None)
            elif cmd == "push_secrets":
                await self._push_secrets(command)
            elif cmd == "finish_provisioning":
                await self._send({"evt": "bye"})
                self.backend.status("done", self.pin)
                self.done.set()
            else:
                await self._error(cmd, "unknown command")
        except Exception as exc:  # noqa: BLE001 - reported to the phone; the session survives
            await self._error(cmd, f"{type(exc).__name__}: {exc}"[:200])

    async def _pin_verify(self, command: dict[str, Any]) -> None:
        assert self._keys is not None
        try:
            proof = bytes.fromhex(str(command.get("proof", "")))
        except ValueError:
            proof = b""
        if not hmac.compare_digest(proof, crypto.client_proof(self._keys)):
            await self._failure()
            return
        if not self.verified:
            device_id, token = await self.backend.register_device(
                str(command.get("device_name", "Android")), str(command.get("device_model", ""))
            )
            self.verified, self.verified_at, self.failures = True, time.monotonic(), 0
            self._credentials = (device_id, token)
        device_id, token = self._credentials
        await self._send(
            {
                "evt": "pin_verified",
                "proof": crypto.server_proof(self._keys).hex(),
                "device_id": device_id,
                "api_token": token,
            }
        )
        self.backend.status("verified", self.pin)

    async def _wifi_connect(self, command: dict[str, Any]) -> None:
        ssid = str(command.get("ssid", ""))
        if not ssid:
            await self._error("wifi_connect", "ssid is required")
            return
        self.backend.status("wifi", self.pin)
        await self._send({"evt": "wifi_status", "state": "connecting", "local_ip": ""})
        result = await self.backend.wifi_connect(ssid, command.get("psk") or None)
        await self._send({"evt": "wifi_status", **result})

    def _start_tailscale(self, authkey: str | None) -> None:
        if self._tailscale is not None and not self._tailscale.done():
            self._tailscale.cancel()
        self.backend.status("tailscale", self.pin)
        self._tailscale = asyncio.create_task(self._watch_tailscale(authkey))

    async def _watch_tailscale(self, authkey: str | None = None) -> None:
        deadline = time.monotonic() + self.tailscale_timeout
        sent_url = None
        # Decide from one status read taken first. A node that is already joined only
        # needs serve + "ready": `tailscale up` with a key on it would restart the
        # backend and re-submit a key that may have been consumed.
        status = await self.backend.tailscale_status()
        if status["state"] != "connected":
            self.backend.tailscale_up(authkey)
        while time.monotonic() < deadline:
            if status["state"] == "connected":
                # "ready" promises the phone that https://<fqdn>/ works.
                if not await self.backend.tailscale_serve():
                    await self._error("tailscale_up", HTTPS_NOT_ENABLED)
                    return
                await self._send(
                    {
                        "evt": "ready",
                        "tailscale_fqdn": status["fqdn"],
                        "tailscale_ip": status["ip"],
                    }
                )
                return
            if status.get("auth_url") and not authkey:
                # (with a key, a login link still held by tailscaled is a stale one)
                if status["auth_url"] != sent_url:
                    sent_url = status["auth_url"]
                    await self._send({"evt": "tailscale_auth", "url": sent_url})
            elif why := self.backend.tailscale_up_error():
                # e.g. a rejected auth key: say so now, not after the full timeout.
                await self._error("tailscale_up", why)
                return
            await asyncio.sleep(self.poll_interval)
            status = await self.backend.tailscale_status()
        await self._error("tailscale_up", "timed out waiting for Tailscale to connect")

    async def _push_secrets(self, command: dict[str, Any]) -> None:
        values = command.get("secrets")
        if not isinstance(values, dict) or not values:
            await self._error("push_secrets", "secrets must be a non-empty object")
            return
        for name, value in values.items():
            ok = isinstance(value, str) and value and "\n" not in value and "\r" not in value
            if not SECRET_NAME_RE.match(str(name)) or not ok:
                await self._error("push_secrets", f"invalid secret {name!r}")
                return
        saved = await self.backend.push_secrets({str(k): v for k, v in values.items()})
        await self._send({"evt": "secrets_saved", "keys": saved})

    async def close(self) -> None:
        if self._tailscale is not None and not self._tailscale.done():
            self._tailscale.cancel()
