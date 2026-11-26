"""lapis-ble-v1 provisioning session: handshake, PIN or paired-phone proof, lockout, commands.

Transport-independent on purpose: the BlueZ GATT server feeds it bytes and gets
frames to notify back, and the tests drive the same object with a fake phone.

Two kinds of phone (BLE_PROVISIONING_PROTOCOL.md section 4.2): a new one proves the PIN
on the LCD, a paired one proves its api_token (keys bound to "tok:" + its SHA-256 hash
instead of the PIN). The first envelope that opens with one of the candidate key sets
decides which kind this session is.
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
PAIRED_SECRET_PREFIX = "tok:"  # + lowercase hex SHA-256 of the api_token (protocol 4.2)
# All a paired phone may do: change the Wi-Fi. Everything else needs the PIN.
PAIRED_COMMANDS = ("wifi_scan", "wifi_connect", "finish_provisioning")
NOT_ALLOWED_PAIRED = "not allowed for a paired phone"
LOCKOUT_COOLDOWN_S = 5.0
TAILSCALE_TIMEOUT_S = 300.0
HTTPS_NOT_ENABLED = (
    "Joined the tailnet, but HTTPS is not enabled for it. In the Tailscale admin console "
    "(DNS page) enable MagicDNS and HTTPS Certificates, then retry."
)


class Backend(Protocol):
    """What provisioning does to the device; faked in tests."""

    async def register_device(self, name: str, model: str) -> tuple[str, str]: ...
    def paired_hashes(self) -> list[tuple[str, str]]: ...  # (device id, token hash), not revoked
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
        allow_pin: bool = True,
    ) -> None:
        self.backend = backend
        # False in the phone Wi-Fi window: paired phones only, and no pairing screen.
        self.allow_pin = allow_pin
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
        self._keys: crypto.SessionKeys | None = None  # set once the first envelope opens
        self._candidates: list[tuple[str, crypto.SessionKeys]] = []  # ("" = the PIN, device id)
        self.paired_device_id = ""  # set when a paired phone's keys opened the session
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
        self._keys, self._candidates, self.paired_device_id = None, [], ""
        self.verified = False
        self.failures = 0
        self._rx.reset()

    # -- HANDSHAKE characteristic ----------------------------------------------

    def handshake_read(self) -> bytes:
        """ServerHello."""
        return crypto.build_hello(self.public, self.device_nonce)

    def handshake_write(self, data: bytes) -> None:
        """ClientHello: derive every key set the phone may use (the PIN's, each paired phone's)."""
        if time.monotonic() < self.locked_until:
            raise crypto.HandshakeError("locked out; retry shortly")
        client_pub, client_nonce = crypto.parse_hello(bytes(data))
        shared = crypto.compute_shared(self._private, client_pub)

        def keys(secret: str) -> crypto.SessionKeys:
            return crypto.derive_keys(
                shared, client_pub, self.public, client_nonce, self.device_nonce, secret
            )

        self._candidates = [("", keys(self.pin))] if self.allow_pin else []
        self._candidates += [
            (device_id, keys(PAIRED_SECRET_PREFIX + token_hash))
            for device_id, token_hash in self.backend.paired_hashes()
        ]
        self._keys, self.paired_device_id = None, ""
        self._rx_seq, self._tx_seq, self._tx_msg = 1, 0, 0
        self.verified = False
        self._rx.reset()
        self._status("connected")  # (not yet known to be a new phone; undone if it is not)

    def _status(self, status: str) -> None:
        """Pairing-screen progress: only in a PIN window, and never for a paired phone."""
        if self.allow_pin and not self.paired_device_id:
            self.backend.status(status, self.pin)

    def _open(self, payload: bytes) -> bytes:
        if self._keys is not None:
            return crypto.open_envelope(self._keys.k_c2d, crypto.DIR_C2D, payload, self._rx_seq)
        for device_id, keys in self._candidates:
            try:
                plaintext = crypto.open_envelope(keys.k_c2d, crypto.DIR_C2D, payload, self._rx_seq)
            except crypto.EnvelopeError:
                continue
            # The first key set that opens decides the session's mode for good.
            self._keys, self.paired_device_id, self._candidates = keys, device_id, []
            if device_id and self.allow_pin:
                self.backend.status("waiting", self.pin)  # the PIN screen as it was
            return plaintext
        raise crypto.EnvelopeError("no key set opens this envelope")

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
        if not flags & crypto.FLAG_ENCRYPTED or (self._keys is None and not self._candidates):
            await self._notify(crypto.reset_frame())
            return
        try:
            plaintext = self._open(payload)
            command = json.loads(plaintext)
            if not isinstance(command, dict):
                raise ValueError("command must be an object")
        except (crypto.EnvelopeError, ValueError):
            # A wrong PIN (or an unknown, revoked token) derives the wrong key: it shows up here.
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
        self._status("locked")
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
        paired = bool(self.paired_device_id)
        if cmd in ("pin_verify", "token_verify"):
            if cmd == "token_verify" and paired:
                await self._token_verify(command)
            elif cmd == "pin_verify" and not paired:
                await self._pin_verify(command)
            else:  # the other mode's proof: pin_verify on token keys, or the reverse
                await self._failure()
            return
        if not self.verified:
            await self._failure()
            return
        if paired and cmd not in PAIRED_COMMANDS:
            await self._error(cmd, NOT_ALLOWED_PAIRED)
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
                if paired and self.allow_pin:
                    # A PIN window stays open for the new phone it was opened for.
                    self._keys, self.paired_device_id, self.verified = None, "", False
                else:
                    self._status("done")
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
        self._status("verified")

    async def _token_verify(self, command: dict[str, Any]) -> None:
        """A paired phone proves its token; it gets the server proof, never a new token."""
        assert self._keys is not None
        try:
            proof = bytes.fromhex(str(command.get("proof", "")))
        except ValueError:
            proof = b""
        if not hmac.compare_digest(proof, crypto.client_proof(self._keys)):
            await self._failure()
            return
        if not self.verified:
            self.verified, self.verified_at, self.failures = True, time.monotonic(), 0
        await self._send(
            {
                "evt": "token_verified",
                "proof": crypto.server_proof(self._keys).hex(),
                "device_id": self.paired_device_id,
            }
        )

    async def _wifi_connect(self, command: dict[str, Any]) -> None:
        ssid = str(command.get("ssid", ""))
        if not ssid:
            await self._error("wifi_connect", "ssid is required")
            return
        self._status("wifi")
        await self._send({"evt": "wifi_status", "state": "connecting", "local_ip": ""})
        result = await self.backend.wifi_connect(ssid, command.get("psk") or None)
        await self._send({"evt": "wifi_status", **result})

    def _start_tailscale(self, authkey: str | None) -> None:
        if self._tailscale is not None and not self._tailscale.done():
            self._tailscale.cancel()
        self._status("tailscale")
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
