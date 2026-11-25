"""`lapis-ble-prov`: the on-demand BLE provisioning process.

Started by lapis-daemon (via `lapis-ble-prov.service`) for an unpaired first
boot or an explicit pairing window, and gone again when provisioning finishes
or the window closes: no Bluetooth stack use and no Python interpreter at idle.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import signal
import sys
import time
from typing import Any

from lapis_daemon.api.auth import register_device_sync
from lapis_daemon.ble_prov.network_ctl import NetworkCtl
from lapis_daemon.ble_prov.session import ProvisioningSession
from lapis_daemon.config import effective_paths, load_config, read_env_file, write_env_file
from lapis_daemon.db import LapisDb
from lapis_daemon.system_status import run_cmd

WINDOW_S = 300  # advertise for five minutes...
VERIFIED_GRACE_S = 600  # ...but never cut off a phone that has proven the PIN
ADAPTER_WAIT_S = 30  # cold boot: bluetoothd may still be registering hci0
POWER_WAIT_S = 15
BLUEZ_CHECK_S = 5  # how often an open window checks that BlueZ is still there
# EX_TEMPFAIL: the unit restarts us (RestartForceExitStatus=75), bounded by its start limit.
EXIT_RETRY = 75
_window_opened = False  # set once the advertisement is registered
ADAPTER_IFACES = ("org.bluez.Adapter1", "org.bluez.GattManager1", "org.bluez.LEAdvertisingManager1")


def _log(message: str) -> None:
    print(f"lapis-ble-prov: {message}", flush=True)


class DaemonLink:
    """Best-effort NDJSON line writer to lapis-daemon's Unix socket."""

    def __init__(self, path: str) -> None:
        self.path = path
        self._writer: asyncio.StreamWriter | None = None
        self._drain: asyncio.Task[None] | None = None
        self.greeted = asyncio.Event()  # set once the daemon has accepted us and is reading

    async def _connect(self) -> None:
        reader, self._writer = await asyncio.open_unix_connection(self.path, limit=2**20)

        async def discard() -> None:  # the daemon broadcasts state to every client
            with contextlib.suppress(Exception):
                while await reader.readline():
                    self.greeted.set()

        self._drain = asyncio.create_task(discard())

    async def send(self, **msg: Any) -> None:
        for _ in range(2):
            try:
                if self._writer is None or self._writer.is_closing():
                    await self._connect()
                assert self._writer is not None
                self._writer.write(json.dumps(msg).encode() + b"\n")
                await self._writer.drain()
                return
            except OSError:
                self._writer = None

    def send_soon(self, **msg: Any) -> None:
        asyncio.get_running_loop().create_task(self.send(**msg))


class DeviceBackend:
    """Real implementation of `session.Backend`."""

    def __init__(self, link: DaemonLink, name: str, deadline: float) -> None:
        self.paths = effective_paths()
        self.link = link
        self.name = name
        self.deadline = deadline
        self.net = NetworkCtl(port=load_config(self.paths.config).api.port)
        self.db = LapisDb(self.paths.db_path)

    def _register(self, name: str, model: str) -> tuple[str, str]:
        self.db._init()  # idempotent; the daemon normally created the schema already
        return register_device_sync(self.db, name, model)

    async def register_device(self, name: str, model: str) -> tuple[str, str]:
        return await asyncio.to_thread(self._register, name, model)

    # The journal is the only trace of a pairing attempt on a device with no console
    # (the boot report copies it to the SD card). Step names and counts only: never a
    # PIN, an SSID, a password, a key or a token.

    async def wifi_scan(self) -> list[dict[str, Any]]:
        await self.net.unblock_radios()
        networks = await self.net.wifi_scan()
        _log(f"wifi scan: {len(networks)} networks")
        return networks

    async def wifi_connect(self, ssid: str, psk: str | None) -> dict[str, Any]:
        await self.net.unblock_radios()
        result = await self.net.wifi_connect(ssid, psk)
        _log(f"wifi join: {result['state']}")
        self.link.send_soon(type="refresh_status")
        return result

    def tailscale_up(self, authkey: str | None) -> None:
        self.net.tailscale_up(authkey)

    def tailscale_up_error(self) -> str | None:
        return self.net.tailscale_up_error()

    async def tailscale_status(self) -> dict[str, Any]:
        return await self.net.tailscale_status()

    async def tailscale_serve(self) -> bool:
        ok = await self.net.tailscale_serve()
        _log(f"tailscale serve: {'active' if ok else 'nothing stored (HTTPS not enabled?)'}")
        self.link.send_soon(type="refresh_status")
        return ok

    async def push_secrets(self, values: dict[str, str]) -> list[str]:
        merged = {**read_env_file(self.paths.secrets), **values}
        await asyncio.to_thread(write_env_file, self.paths.secrets, merged)
        self.link.send_soon(type="reload_secrets")
        return sorted(values)

    def status(self, status: str, pin: str) -> None:
        _log(f"status {status}")
        remaining = max(0, int(self.deadline - time.monotonic()))
        self.link.send_soon(
            type="pairing_event",
            mode="pin",
            pin=pin,
            device_name=self.name,
            status=status,
            expires_in=remaining,
            expires_at=int(time.time()) + remaining,
        )


async def find_adapter(bus: Any) -> Any:
    """The first child of /org/bluez that is an LE-peripheral-capable adapter; waits for it.

    Not `Adapter.get_first()`: bluez-peripheral treats every child of /org/bluez as an
    adapter, and BlueZ 5.82 also exports /org/bluez/test (SAP plugin), which has no Adapter1.
    """
    from bluez_peripheral.util import Adapter
    from dbus_next.errors import DBusError

    deadline = time.monotonic() + ADAPTER_WAIT_S
    while True:
        reason = "bluetoothd has no adapter"
        try:
            root = await asyncio.wait_for(bus.introspect("org.bluez", "/org/bluez"), 5)
            for node in sorted(n.name for n in root.nodes):
                path = f"/org/bluez/{node}"
                intro = await asyncio.wait_for(bus.introspect("org.bluez", path), 5)
                names = {i.name for i in intro.interfaces}
                if names.issuperset(ADAPTER_IFACES):
                    return Adapter(bus.get_proxy_object("org.bluez", path, intro))
                if ADAPTER_IFACES[0] in names:
                    reason = f"{node} is not ready for LE advertising"
        except TimeoutError:
            reason = "bluetoothd is not running or not answering"
        except DBusError as exc:
            reason = f"bluetoothd answered {exc.type}"
        if time.monotonic() >= deadline:
            raise RuntimeError(f"no usable Bluetooth adapter: {reason}")
        await asyncio.sleep(1)


async def _bluez_owner(bus: Any) -> str:
    """Unique bus name of the running bluetoothd ('' when it is gone)."""
    from dbus_next import Message

    reply = await bus.call(
        Message(
            destination="org.freedesktop.DBus",
            path="/org/freedesktop/DBus",
            interface="org.freedesktop.DBus",
            member="GetNameOwner",
            signature="s",
            body=["org.bluez"],
        )
    )
    return str(reply.body[0]) if reply.body and reply.error_name is None else ""


async def power_on(adapter: Any) -> None:
    """Power the adapter on. BlueZ answers Busy while it does that itself, Blocked under rfkill."""
    from dbus_next.errors import DBusError

    deadline = time.monotonic() + POWER_WAIT_S
    why = "the adapter stayed off"
    while not await adapter.get_powered():
        if time.monotonic() >= deadline:
            raise RuntimeError(f"cannot power on Bluetooth: {why}")
        try:
            await adapter.set_powered(True)
        except DBusError as exc:
            why = exc.type
            if exc.type.endswith(".Blocked"):
                await run_cmd("rfkill", "unblock", "bluetooth")
        await asyncio.sleep(1)


async def run() -> int:
    from bluez_peripheral.advert import Advertisement
    from bluez_peripheral.util import get_message_bus

    from lapis_daemon.ble_prov.gatt_server import SERVICE_UUID, ProvisioningService

    paths = effective_paths()
    link = DaemonLink(str(paths.socket_path))

    bus = await get_message_bus()
    adapter = await find_adapter(bus)
    await power_on(adapter)
    raw_address = await adapter.get_address()
    address = raw_address.replace(":", "")
    name = f"LapisZero-{address[-4:].upper()}"
    await adapter.set_alias(name)

    started = time.monotonic()
    service = ProvisioningService()
    backend = DeviceBackend(link, name, started + WINDOW_S)

    async def disconnect() -> None:
        if service.device_path is None:
            return
        introspection = await bus.introspect("org.bluez", service.device_path)
        device = bus.get_proxy_object("org.bluez", service.device_path, introspection)
        await device.get_interface("org.bluez.Device1").call_disconnect()

    session = ProvisioningSession(backend, service.notify, disconnect)
    service.attach(session)
    await service.register(bus, adapter=adapter)
    advert = Advertisement(name, [SERVICE_UUID], 0, WINDOW_S)
    await advert.register(bus, adapter)
    global _window_opened
    _window_opened = True
    pump = asyncio.create_task(service.pump())
    asyncio.get_running_loop().add_signal_handler(signal.SIGTERM, session.done.set)
    backend.status("waiting", session.pin)
    print(f"lapis-ble-prov: advertising as {name} for {WINDOW_S}s", flush=True)

    bluez_owner = await _bluez_owner(bus)
    next_check = started + BLUEZ_CHECK_S
    try:
        while not session.done.is_set():
            # A PIN on the LCD while BlueZ has restarted or the radio is off cannot work.
            if time.monotonic() >= next_check:
                next_check = time.monotonic() + BLUEZ_CHECK_S
                if await _bluez_owner(bus) != bluez_owner or not await adapter.get_powered():
                    raise RuntimeError("Bluetooth stopped during the pairing window")
            limit = started + WINDOW_S
            if session.verified:
                limit = max(limit, session.verified_at + VERIFIED_GRACE_S)
            if time.monotonic() >= limit:
                print("lapis-ble-prov: pairing window closed", flush=True)
                break
            with contextlib.suppress(TimeoutError):
                await asyncio.wait_for(session.done.wait(), 1.0)
        await asyncio.sleep(0.5)  # let the final `bye` notification leave
    finally:
        pump.cancel()
        await session.close()
        with contextlib.suppress(Exception):
            await service.unregister()
        await link.send(type="pairing_event", mode="off")
        bus.disconnect()
    return 0


async def _report_failure(message: str) -> None:
    link = DaemonLink(str(effective_paths().socket_path))
    await asyncio.wait_for(link.send(type="pairing_event", mode="off", error=message), 3)
    # The daemon writes its state to every new client before it reads from it; exiting
    # before that greeting arrives resets the connection and the report is never read.
    await asyncio.wait_for(link.greeted.wait(), 3)
    await asyncio.sleep(0.2)  # let it finish that greeting before the socket closes


def main() -> int:
    try:
        return asyncio.run(run())
    except Exception as exc:  # noqa: BLE001 - one line in the journal, then exit
        # DBusError carries the BlueZ error name in .type (org.bluez.Error.Failed, ...).
        message = f"{getattr(exc, 'type', type(exc).__name__)}: {exc}"
        if isinstance(exc, RuntimeError):
            message = str(exc)  # our own, already worded for the LCD
        print(f"lapis-ble-prov: {message}", file=sys.stderr, flush=True)
        # No adapter, rfkill, BlueZ down: say so on the LCD instead of failing silently.
        with contextlib.suppress(Exception):
            asyncio.run(_report_failure(message[:80]))
        # Only a start-up failure is retried by the unit. A window that was open and
        # then died stays closed until someone asks again: BLE is strictly on demand.
        return 1 if _window_opened else EXIT_RETRY


if __name__ == "__main__":
    raise SystemExit(main())
