"""BlueZ GATT peripheral for lapis-ble-v1 (via bluez-peripheral / D-Bus).

Thin by design: every protocol decision lives in `session.py`. The three
characteristics carry no BlueZ encryption flags because the protocol brings its
own authenticated encryption bound to the PIN on the LCD; requiring link-layer
bonding as well would only add an OS pairing dialog.

Linux-only; imported lazily so the rest of the daemon runs anywhere.
"""

# The D-Bus signature strings below are evaluated by the library; postponed
# evaluation of annotations would break them, so no `from __future__` here.
# ruff: noqa: F722, F821, UP037

import asyncio

from bluez_peripheral.gatt.characteristic import CharacteristicFlags as Flags
from bluez_peripheral.gatt.characteristic import characteristic
from bluez_peripheral.gatt.service import Service
from dbus_next.constants import PropertyAccess
from dbus_next.errors import DBusError
from dbus_next.service import dbus_property

from lapis_daemon.ble_prov import crypto
from lapis_daemon.ble_prov.session import ProvisioningSession

SERVICE_UUID = "7c4f0001-9a3b-4c6e-8e21-51a0c0de0001"
HANDSHAKE_UUID = "7c4f0001-9a3b-4c6e-8e21-51a0c0de0002"
CMD_TX_UUID = "7c4f0001-9a3b-4c6e-8e21-51a0c0de0003"
EVT_RX_UUID = "7c4f0001-9a3b-4c6e-8e21-51a0c0de0004"
NOTIFY_GAP_S = 0.02  # let BlueZ drain between back-to-back notifications


class ProvisioningService(Service):
    def __init__(self) -> None:
        super().__init__(SERVICE_UUID, True)
        self.session: ProvisioningSession | None = None
        self.inbox: asyncio.Queue[bytes] = asyncio.Queue(maxsize=256)
        self.device_path: str | None = None

    @dbus_property(PropertyAccess.READ)
    def Includes(self) -> "ao":
        # bluez-peripheral 0.1.7 lists the service's own path here, and BlueZ then
        # publishes a primary service that includes itself. Nothing is included.
        return []

    def attach(self, session: ProvisioningSession) -> None:
        self.session = session

    def _seen(self, options) -> None:
        if getattr(options, "mtu", 0):
            self.session.mtu = int(options.mtu)
        if getattr(options, "device", None):
            self.device_path = options.device

    @characteristic(HANDSHAKE_UUID, Flags.READ | Flags.WRITE)
    def handshake(self, options):
        self._seen(options)
        return self.session.handshake_read()[options.offset :]

    @handshake.setter
    def handshake(self, value, options):
        self._seen(options)
        try:
            self.session.handshake_write(bytes(value))
        except crypto.HandshakeError as exc:
            raise DBusError("org.bluez.Error.Failed", str(exc)) from exc

    @characteristic(CMD_TX_UUID, Flags.WRITE | Flags.WRITE_WITHOUT_RESPONSE).setter
    def cmd_tx(self, value, options):
        self._seen(options)
        try:
            self.inbox.put_nowait(bytes(value))
        except asyncio.QueueFull as exc:
            raise DBusError("org.bluez.Error.Failed", "busy") from exc

    @characteristic(EVT_RX_UUID, Flags.NOTIFY)
    def evt_rx(self, options):
        return b""

    async def notify(self, frame: bytes) -> None:
        self.evt_rx.changed(frame)
        await asyncio.sleep(NOTIFY_GAP_S)

    async def pump(self) -> None:
        """Feed queued CMD_TX writes to the session, in arrival order."""
        while True:
            frame = await self.inbox.get()
            await self.session.on_frame(frame)
