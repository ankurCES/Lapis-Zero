"""Scratch verification of the proposed NetworkCtl changes (fake command runner)."""

from __future__ import annotations

import asyncio

import pytest

from lapis_daemon.ble_prov.network_ctl import NetworkCtl

LIST = ("nmcli", "-t", "-f", "SSID,SIGNAL,SECURITY", "dev", "wifi", "list")
STATE = ("nmcli", "-g", "GENERAL.STATE", "device", "show", "wlan0")
SHOW = ("nmcli", "-t", "-f", "GENERAL.CONNECTION,IP4.ADDRESS", "device", "show", "wlan0")


def make(states, shows=(), connect_rc=0):
    calls: list[tuple[str, ...]] = []
    states, shows = list(states), list(shows)

    async def run(*argv: str, timeout: float = 5.0) -> tuple[int, str]:
        calls.append(argv)
        if argv == STATE:
            return (0, states.pop(0)) if len(states) > 1 else (0, states[0])
        if argv == LIST:
            assert timeout == 20
            return 0, "HomeWiFi:72:WPA2\n"
        if argv == SHOW:
            return 0, (shows.pop(0) if len(shows) > 1 else shows[0])
        if argv[:6] == ("nmcli", "--wait", "30", "dev", "wifi", "connect"):
            return connect_rc, ""
        if argv[:3] == ("nmcli", "connection", "delete"):
            return 10, ""
        return 0, ""

    return NetworkCtl(run, poll=0), calls


def test_scan_waits_for_wlan0_after_radio_on() -> None:
    net, calls = make(["20 (unavailable)\n", "20 (unavailable)\n", "30 (disconnected)\n"])
    out = asyncio.run(net.wifi_scan())
    assert out == [{"ssid": "HomeWiFi", "rssi": -64, "secured": True}]
    assert calls[:3] == [STATE, STATE, STATE]
    assert calls[3] == ("nmcli", "dev", "wifi", "rescan") and calls[4] == LIST


def test_scan_reports_a_dead_radio_instead_of_an_empty_list() -> None:
    net, calls = make([""])  # nmcli: "Error: Device 'wlan0' not found." -> rc 10, no stdout
    with pytest.raises(RuntimeError, match="Wi-Fi is not available"):
        asyncio.run(net.wifi_scan())
    assert calls == [STATE] * 6


def test_connect_replaces_stale_profile_and_waits_for_ipv4() -> None:
    net, calls = make(
        ["100 (connected)\n"],
        [
            "GENERAL.CONNECTION:HomeWiFi\n",
            "GENERAL.CONNECTION:HomeWiFi\n",
            "GENERAL.CONNECTION:HomeWiFi\nIP4.ADDRESS[1]:192.168.1.42/24\n",
        ],
    )
    assert asyncio.run(net.wifi_connect("HomeWiFi", "pw")) == {
        "state": "connected",
        "local_ip": "192.168.1.42",
    }
    assert calls[0] == STATE
    assert calls[1] == ("nmcli", "connection", "delete", "id", "HomeWiFi")
    assert calls[2] == (
        "nmcli",
        "--wait",
        "30",
        "dev",
        "wifi",
        "connect",
        "HomeWiFi",
        "password",
        "pw",
    )
    assert calls[3:] == [SHOW, SHOW, SHOW]


def test_connect_failure_does_not_wait(capsys) -> None:
    net, calls = make(["30 (disconnected)\n"], ["GENERAL.CONNECTION:--\n"], connect_rc=4)
    assert asyncio.run(net.wifi_connect("HomeWiFi", "wrong")) == {"state": "failed", "local_ip": ""}
    assert calls.count(SHOW) == 1
    out = capsys.readouterr().out
    assert "rc=4" in out and "wrong" not in out and "HomeWiFi" not in out
