"""Wi-Fi over the control plane, and the phone Wi-Fi window (protocol section 4.2)."""

from __future__ import annotations

import asyncio
import time
from pathlib import Path
from typing import Any

import pytest
from conftest import make_paths
from test_api import AUTH, TOKEN, client_for

from lapis_daemon import core as core_mod
from lapis_daemon.ble_prov import service
from lapis_daemon.ble_prov.network_ctl import NetworkCtl

PSK = "correct horse battery"
STATE = ("nmcli", "-g", "GENERAL.STATE", "device", "show", "wlan0")
SHOW = ("nmcli", "-t", "-f", "GENERAL.CONNECTION,IP4.ADDRESS", "device", "show", "wlan0")
SAVED = ("nmcli", "-t", "-f", "NAME,TYPE,ACTIVE", "connection", "show")


def test_wifi_endpoints_shape_and_validation(short_tmp: Path) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp)  # mock: never the developer's own Wi-Fi
        resp = await client.get("/v1/wifi", headers=AUTH)
        assert resp.status == 200 and await resp.json() == {
            "ssid": "HomeWiFi",
            "ip": "192.168.1.42",
            "rssi": -58,
            "saved": [{"name": "HomeWiFi", "active": True}],
            "ble": {"open": False, "expires_at": 0},
        }
        networks = (await (await client.get("/v1/wifi/scan", headers=AUTH)).json())["networks"]
        assert networks and all(set(n) == {"ssid", "rssi", "secured"} for n in networks)

        for bad in (
            {"psk": PSK},
            {"ssid": "", "psk": PSK},
            {"ssid": 7, "psk": PSK},
            {"ssid": "x" * 33, "psk": PSK},
            {"ssid": "é" * 17, "psk": PSK},  # 34 bytes in UTF-8
            {"ssid": "Home", "psk": "short"},
            {"ssid": "Home", "psk": "p" * 64},  # 64 characters must be hex
            {"ssid": "Home", "psk": "p" * 65},
            {"ssid": "Home", "psk": "café-password"},
            {"ssid": "Home", "psk": "tab\there-and-more"},
            {"ssid": "Home", "psk": 12345678},
        ):
            resp = await client.post("/v1/wifi/connect", json=bad, headers=AUTH)
            assert resp.status == 422, bad
            assert (await resp.json())["error"]["code"] == "invalid"
            if isinstance(bad.get("psk"), str) and len(bad["psk"]) > 5:
                assert bad["psk"] not in await resp.text(), "a refused psk is never echoed"
        for good in ({"ssid": "x" * 32, "psk": "a" * 63}, {"ssid": "Lab", "psk": "aB3" * 21 + "f"}):
            resp = await client.post("/v1/wifi/connect", json=good, headers=AUTH)
            assert resp.status == 200 and (await resp.json())["state"] == "connected"
        resp = await client.post(
            "/v1/wifi/connect", json={"ssid": "Cafe", "psk": None}, headers=AUTH
        )
        assert await resp.json() == {"state": "connected", "local_ip": "192.168.1.42"}

        names = [
            s["name"] for s in (await (await client.get("/v1/wifi", headers=AUTH)).json())["saved"]
        ]
        assert names == ["HomeWiFi", "x" * 32, "Lab", "Cafe"]
        resp = await client.post("/v1/wifi/forget", json={"name": "Nope"}, headers=AUTH)
        assert resp.status == 404 and (await resp.json())["error"]["code"] == "not_found"
        assert (await client.post("/v1/wifi/forget", json={}, headers=AUTH)).status == 422
        resp = await client.post("/v1/wifi/forget", json={"name": "Lab"}, headers=AUTH)
        body = await resp.json()
        assert resp.status == 200 and set(body) == {"ssid", "ip", "rssi", "saved", "ble"}
        assert "Lab" not in [s["name"] for s in body["saved"]]

        resp = await client.post("/v1/wifi/ble", headers=AUTH)
        ble = (await resp.json())["ble"]
        assert resp.status == 200 and ble["open"] is True
        assert time.time() + 290 < ble["expires_at"] <= time.time() + 300
        assert core.pairing is None, "the phone Wi-Fi window never shows a pairing screen"

        async def nobody() -> int:
            return 0

        core.auth.count = nobody  # (the bearer token itself is a paired phone)
        resp = await client.post("/v1/wifi/ble", headers=AUTH)
        assert resp.status == 409 and (await resp.json())["error"]["code"] == "not_paired"
        await client.close()
        await core.stop()

    asyncio.run(scenario())


def test_wifi_on_the_device_keeps_the_password_out_of_argv_journal_and_responses(
    short_tmp: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    calls: list[tuple[str, ...]] = []
    shows = ["GENERAL.CONNECTION:Old\n", "GENERAL.CONNECTION:Home\nIP4.ADDRESS[1]:10.1.2.3/24\n"]
    state = ["100 (connected)\n"]

    async def run(*argv: str, **_: Any) -> tuple[int, str]:
        calls.append(argv)
        if argv == STATE:
            return 0, state[0]
        if argv == SHOW:
            return 0, shows.pop(0) if len(shows) > 1 else shows[0]
        if argv == SAVED:
            return 0, "Home:802-11-wireless:yes\nOld:802-11-wireless:no\nWired:802-3-ethernet:no\n"
        if "SSID,SECURITY" in argv:
            return 0, "Home:WPA3\n"
        return 0, ""

    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        core.mock = False  # take the real path; every command lands in `calls`
        core.net = NetworkCtl(run, poll=0, nm_dir=short_tmp)
        try:
            resp = await client.post(
                "/v1/wifi/connect", json={"ssid": "Home", "psk": PSK}, headers=AUTH
            )
            text = await resp.text()
            assert resp.status == 200 and text == '{"state": "connected", "local_ip": "10.1.2.3"}'
            keyfile = short_tmp / f"lapis-{b'Home'.hex()}.nmconnection"
            assert "key-mgmt=sae\n" in keyfile.read_text()  # WPA3-only in the scan
            assert ("nmcli", "connection", "load", str(keyfile)) in calls
            assert not any("horse" in arg for argv in calls for arg in argv)

            info = await (await client.get("/v1/wifi", headers=AUTH)).json()
            assert (info["ssid"], info["ip"]) == ("Home", "10.1.2.3")
            assert info["saved"] == [
                {"name": "Home", "active": True},
                {"name": "Old", "active": False},
            ]
            assert info["rssi"] is None or isinstance(info["rssi"], int)

            resp = await client.post("/v1/wifi/forget", json={"name": "Old"}, headers=AUTH)
            assert resp.status == 200
            assert ("nmcli", "connection", "delete", "id", "Old") in calls
            resp = await client.post("/v1/wifi/forget", json={"name": "Wired"}, headers=AUTH)
            assert resp.status == 404  # only Wi-Fi profiles

            state[0] = "20 (unavailable)\n"
            resp = await client.get("/v1/wifi/scan", headers=AUTH)
            assert resp.status == 503 and (await resp.json())["error"]["code"] == "wifi_unavailable"
        finally:
            core.mock = True
            await client.close()
            await core.stop()

    asyncio.run(scenario())
    journal = capsys.readouterr().out
    assert "wifi_connect ssid='Home' state=connected by=dev_test" in journal
    assert "wifi_forget name='Old' by=dev_test" in journal
    assert "horse" not in journal and TOKEN not in journal


def test_phone_wifi_window_opens_once_per_outage_and_a_pin_window_replaces_it(
    short_tmp: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    ran: list[tuple[str, ...]] = []

    async def run_cmd(*argv: str, **_: object) -> tuple[int, str]:
        ran.append(argv)
        return 0, ""

    monkeypatch.setattr(core_mod, "run_cmd", run_cmd)
    starts = ("systemctl", "--no-block", "start", "lapis-ble-prov.service")

    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        core.mock = False
        mode = core.paths.ble_mode_file
        try:
            core.status.wifi = {"ssid": "", "ip": ""}  # Wi-Fi lost
            for t in (100.0, 115.0, 129.9):
                await core._watch_offline(t)
            assert ran == []
            await core._watch_offline(130.0)  # 30 s without a connection
            assert ran.count(starts) == 1 and mode.read_text() == "wifi\n"
            assert core.wifi_ble_state()["open"] and core.pairing is None
            await core._watch_offline(500.0)
            assert ran.count(starts) == 1, "once per outage"

            # The process reports its end; Wi-Fi comes back (re-armed), then goes again.
            core.on_wifi_ble({"type": "wifi_ble", "open": False})
            assert core.wifi_ble_state() == {"open": False, "expires_at": 0}
            core.status.wifi = {"ssid": "Home", "ip": "10.1.2.3"}
            await core._watch_offline(600.0)
            core.status.wifi = {"ssid": "", "ip": ""}
            await core._watch_offline(700.0)
            await core._watch_offline(730.0)
            assert ran.count(starts) == 2
            # A verified phone gets longer: the process says until when.
            core.on_wifi_ble({"type": "wifi_ble", "open": True, "expires_at": time.time() + 900})
            assert core.wifi_ble_state()["expires_at"] > time.time() + 800

            # A PIN window replaces it (same unit, restarted in PIN mode).
            ran.clear()
            await core.pairing_start("ble")
            assert mode.read_text() == "pin\n"
            assert ("systemctl", "--no-block", "restart", "lapis-ble-prov.service") in ran
            assert core.pairing["status"] == "Starting Bluetooth"
            assert core.wifi_ble_state()["open"]  # the PIN window serves paired phones too
            ran.clear()
            await client.post("/v1/wifi/ble", headers=AUTH)
            assert ran == [], "a PIN window already listens for paired phones"

            # A device nobody paired with does not open it.
            await core.pairing_stop()
            core.on_wifi_ble({"type": "wifi_ble", "open": False, "error": "no adapter"})
            await core.auth.revoke("dev_test")
            await core._watch_offline(1000.0)
            await core._watch_offline(1100.0)
            assert starts not in ran
        finally:
            core.mock = True
            await client.close()
            await core.stop()

    asyncio.run(scenario())


def test_the_ble_process_reads_its_window_mode(tmp_path: Path) -> None:
    paths = make_paths(tmp_path)
    assert service.window_mode(paths) == "pin"  # nothing written: a PIN window, as before
    for text, mode in (("wifi\n", "wifi"), ("pin\n", "pin"), ("junk", "pin")):
        paths.ble_mode_file.write_text(text)
        assert service.window_mode(paths) == mode
    assert service._closed("wifi") == {"type": "wifi_ble", "open": False}
    assert service._closed("pin", error="x") == {
        "type": "pairing_event",
        "mode": "off",
        "error": "x",
    }
