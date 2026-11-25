"""The BlueZ-facing start-up of lapis-ble-prov, against a fake D-Bus.

Nothing else in the suite touched this path, which is how a library/BlueZ
incompatibility reached the hardware: bluez-peripheral's `Adapter.get_first()`
treats every child of /org/bluez as an adapter, and BlueZ 5.82 also exports
/org/bluez/test (SAP plugin). The process crashed 0.25 s after every start.
"""

from __future__ import annotations

import asyncio
from types import SimpleNamespace as NS
from typing import Any

import pytest
from dbus_next.errors import DBusError

from lapis_daemon.ble_prov import service
from lapis_daemon.ble_prov.gatt_server import ProvisioningService

ADAPTER = list(service.ADAPTER_IFACES) + ["org.freedesktop.DBus.Properties"]


def intro(children: list[str] = (), interfaces: list[str] = ()) -> NS:
    return NS(nodes=[NS(name=c) for c in children], interfaces=[NS(name=i) for i in interfaces])


class FakeBus:
    """`tree[path]` is one introspection result, or a list consumed one call at a time."""

    def __init__(self, tree: dict[str, Any]) -> None:
        self.tree = tree

    async def introspect(self, _name: str, path: str) -> NS:
        node = self.tree[path]
        if isinstance(node, list):
            node = node.pop(0) if len(node) > 1 else node[0]
        if isinstance(node, Exception):
            raise node
        return node

    def get_proxy_object(self, _name: str, path: str, _intro: NS) -> NS:
        return NS(path=path, get_interface=lambda iface: NS(iface=iface))


@pytest.fixture
def no_sleep(monkeypatch: pytest.MonkeyPatch) -> None:
    async def instant(_seconds: float) -> None:
        return None

    monkeypatch.setattr(service.asyncio, "sleep", instant)


def test_the_sap_test_node_is_not_mistaken_for_an_adapter() -> None:
    bus = FakeBus(
        {
            "/org/bluez": intro(children=["test", "hci0"]),
            "/org/bluez/test": intro(interfaces=["org.bluez.SimAccessTest1"]),
            "/org/bluez/hci0": intro(interfaces=ADAPTER),
        }
    )
    adapter = asyncio.run(service.find_adapter(bus))
    assert adapter._proxy.path == "/org/bluez/hci0"


def test_adapter_that_registers_late_is_waited_for(no_sleep: None) -> None:
    # bluetoothd owns its bus name before hci0 exists, and LEAdvertisingManager1
    # appears one management round trip after Adapter1.
    bus = FakeBus(
        {
            "/org/bluez": [intro(), intro(children=["hci0"])],
            "/org/bluez/hci0": [
                intro(interfaces=["org.bluez.Adapter1"]),
                intro(interfaces=ADAPTER),
            ],
        }
    )
    assert asyncio.run(service.find_adapter(bus))._proxy.path == "/org/bluez/hci0"


def test_no_adapter_fails_with_a_reason_for_the_lcd(
    no_sleep: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(service, "ADAPTER_WAIT_S", 0)
    half_ready = FakeBus(
        {
            "/org/bluez": intro(children=["hci0"]),
            "/org/bluez/hci0": intro(interfaces=["org.bluez.Adapter1"]),
        }
    )
    with pytest.raises(RuntimeError, match="hci0 is not ready for LE advertising"):
        asyncio.run(service.find_adapter(half_ready))
    refused = FakeBus({"/org/bluez": DBusError("org.freedesktop.DBus.Error.ServiceUnknown", "")})
    with pytest.raises(RuntimeError, match="ServiceUnknown"):
        asyncio.run(service.find_adapter(refused))


def test_power_on_rides_out_busy_and_lifts_rfkill(
    no_sleep: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    ran: list[tuple[str, ...]] = []

    async def run_cmd(*argv: str, **_: object) -> tuple[int, str]:
        ran.append(argv)
        return 0, ""

    monkeypatch.setattr(service, "run_cmd", run_cmd)
    errors = [DBusError("org.bluez.Error.Busy", ""), DBusError("org.bluez.Error.Blocked", "")]
    powered = [False, False, False, True]

    class Adapter:
        async def get_powered(self) -> bool:
            return powered.pop(0)

        async def set_powered(self, on: bool) -> None:
            assert on
            if errors:
                raise errors.pop(0)

    asyncio.run(service.power_on(Adapter()))
    assert ran == [("rfkill", "unblock", "bluetooth")] and not errors

    # An adapter that never comes on is a bounded failure with the last reason.
    class Stuck:
        async def get_powered(self) -> bool:
            return False

        async def set_powered(self, on: bool) -> None:
            raise DBusError("org.bluez.Error.Blocked", "")

    monkeypatch.setattr(service, "POWER_WAIT_S", 0)
    with pytest.raises(RuntimeError, match=r"cannot power on Bluetooth: the adapter stayed off"):
        asyncio.run(service.power_on(Stuck()))

    # Already powered (BlueZ AutoEnable): nothing is written at all.
    class Powered:
        async def get_powered(self) -> bool:
            return True

    asyncio.run(service.power_on(Powered()))


def test_gatt_service_does_not_include_itself() -> None:
    assert ProvisioningService.Includes.prop_getter(ProvisioningService()) == []
