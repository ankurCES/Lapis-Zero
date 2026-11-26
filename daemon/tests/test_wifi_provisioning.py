"""NetworkCtl against a fake command runner: scan, join (keyfile, never an argv), forget."""

from __future__ import annotations

import asyncio
import stat
from pathlib import Path

import pytest

from lapis_daemon.ble_prov.network_ctl import (
    NetworkCtl,
    key_mgmt_for,
    keyfile_escape,
    parse_saved,
    render_keyfile,
)

LIST = ("nmcli", "-t", "-f", "SSID,SIGNAL,SECURITY", "dev", "wifi", "list")
STATE = ("nmcli", "-g", "GENERAL.STATE", "device", "show", "wlan0")
SHOW = ("nmcli", "-t", "-f", "GENERAL.CONNECTION,IP4.ADDRESS", "device", "show", "wlan0")
SECURITY = ("nmcli", "-t", "-f", "SSID,SECURITY", "dev", "wifi", "list", "--rescan", "no")
PSK = "correct horse battery"


def make(states, shows=(), connect_rc=0, nm_dir: Path | str = "/nonexistent", security=""):
    calls: list[tuple[str, ...]] = []
    states, shows = list(states), list(shows)

    async def run(*argv: str, timeout: float = 5.0) -> tuple[int, str]:
        calls.append(argv)
        if argv == STATE:
            return (0, states.pop(0)) if len(states) > 1 else (0, states[0])
        if argv == LIST:
            assert timeout == 20
            return 0, "HomeWiFi:72:WPA2\n"
        if argv == SECURITY:
            return 0, security
        if argv == SHOW:
            return 0, (shows.pop(0) if len(shows) > 1 else shows[0])
        if argv[:6] == ("nmcli", "--wait", "30", "dev", "wifi", "connect"):
            return connect_rc, ""
        if argv[:5] == ("nmcli", "--wait", "30", "connection", "up"):
            return connect_rc, ""
        if argv[:3] == ("nmcli", "connection", "delete"):
            return 10, ""
        return 0, ""

    return NetworkCtl(run, poll=0, nm_dir=nm_dir), calls


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


def test_connect_writes_a_keyfile_replaces_stale_profile_and_waits_for_ipv4(
    tmp_path: Path,
) -> None:
    net, calls = make(
        ["100 (connected)\n"],
        [
            "GENERAL.CONNECTION:Home WiFi\n",
            "GENERAL.CONNECTION:Home WiFi\n",
            "GENERAL.CONNECTION:Home WiFi\nIP4.ADDRESS[1]:192.168.1.42/24\n",
        ],
        nm_dir=tmp_path,
        security="Home WiFi:WPA2 WPA3\nOther:WPA3\n",
    )
    assert asyncio.run(net.wifi_connect("Home WiFi", PSK)) == {
        "state": "connected",
        "local_ip": "192.168.1.42",
    }
    keyfile = tmp_path / f"lapis-{b'Home WiFi'.hex()}.nmconnection"
    assert stat.S_IMODE(keyfile.stat().st_mode) == 0o600
    text = keyfile.read_text()
    uuid = text.split("uuid=")[1].split("\n")[0]
    assert "key-mgmt=wpa-psk\n" in text and "psk=correct\\shorse\\sbattery\n" in text
    assert calls[:3] == [STATE, SECURITY, ("nmcli", "connection", "delete", "id", "Home WiFi")]
    assert calls[3] == ("nmcli", "connection", "load", str(keyfile))
    assert calls[4] == ("nmcli", "--wait", "30", "connection", "up", "uuid", uuid)
    assert calls[5:] == [SHOW, SHOW, SHOW]
    assert not any(PSK in arg or "horse" in arg for argv in calls for arg in argv)


def test_connect_failure_deletes_the_new_profile_and_does_not_wait(
    capsys: pytest.CaptureFixture[str], tmp_path: Path
) -> None:
    net, calls = make(
        ["30 (disconnected)\n"], ["GENERAL.CONNECTION:--\n"], connect_rc=4, nm_dir=tmp_path
    )
    wrong = "wrong-password"
    assert asyncio.run(net.wifi_connect("HomeWiFi", wrong)) == {"state": "failed", "local_ip": ""}
    assert calls.count(SHOW) == 1
    uuid = calls[4][-1]
    assert calls[-1] == ("nmcli", "connection", "delete", "uuid", uuid)
    assert list(tmp_path.iterdir()) == []  # connect = save only on success
    assert not any(wrong in arg for argv in calls for arg in argv)
    out = capsys.readouterr().out
    assert "rc=4" in out and wrong not in out and "HomeWiFi" not in out


def test_open_network_joins_without_a_keyfile(tmp_path: Path) -> None:
    net, calls = make(["100 (connected)\n"], ["IP4.ADDRESS[1]:10.0.0.9/24\n"], nm_dir=tmp_path)
    assert asyncio.run(net.wifi_connect("Cafe", None))["state"] == "connected"
    assert ("nmcli", "--wait", "30", "dev", "wifi", "connect", "Cafe") in calls
    assert SECURITY not in calls and list(tmp_path.iterdir()) == []
    net, calls = make(["100 (connected)\n"], ["GENERAL.CONNECTION:--\n"], connect_rc=4)
    assert asyncio.run(net.wifi_connect("Cafe", None))["state"] == "failed"
    assert calls[-1] == ("nmcli", "connection", "delete", "id", "Cafe")


def test_keyfile_rendering_escaping_and_key_mgmt() -> None:
    text = render_keyfile("Caf\u00e9 \\net", "p a\\ss;#=x", "u-1", "sae")
    assert text == (
        "[connection]\nid=Caf\u00e9\\s\\\\net\nuuid=u-1\ntype=wifi\nautoconnect=true\n\n"
        "[wifi]\nmode=infrastructure\nssid=67;97;102;195;169;32;92;110;101;116;\n\n"
        "[wifi-security]\nkey-mgmt=sae\npsk=p\\sa\\\\ss;#=x\n\n"
        "[ipv4]\nmethod=auto\n\n[ipv6]\nmethod=auto\n"
    )
    for bad in ("line\nbreak", "tab\there", "nul\x00", "del\x7f"):
        with pytest.raises(ValueError, match="control characters") as exc:
            keyfile_escape(bad)
        assert bad not in str(exc.value)
    scan = "Home:WPA2\nLab:WPA3\nMixed:WPA2 WPA3\nOld:WPA1\nEsc\\:aped:WPA3\nLab:WPA3\n"
    assert key_mgmt_for(scan, "Lab") == "sae" and key_mgmt_for(scan, "Esc:aped") == "sae"
    for ssid in ("Home", "Mixed", "Old", "Hidden"):
        assert key_mgmt_for(scan, ssid) == "wpa-psk", ssid


def test_control_characters_are_refused_before_anything_changes(tmp_path: Path) -> None:
    net, calls = make(["100 (connected)\n"], ["GENERAL.CONNECTION:Home\n"], nm_dir=tmp_path)
    with pytest.raises(ValueError):
        asyncio.run(net.wifi_connect("Home", "bad\npassword"))
    assert not any(argv[:3] == ("nmcli", "connection", "delete") for argv in calls)
    assert list(tmp_path.iterdir()) == []


def test_saved_profiles_and_forget() -> None:
    out = "Home:802-11-wireless:yes\nWired:802-3-ethernet:no\nCaf\\:e:wifi:no\nlo:loopback:yes\n"
    assert parse_saved(out) == [
        {"name": "Home", "active": True},
        {"name": "Caf:e", "active": False},
    ]
    net, calls = make(["100 (connected)\n"])
    assert asyncio.run(net.wifi_forget("Home")) is False  # (the fake's delete exits 10)
    assert calls == [("nmcli", "connection", "delete", "id", "Home")]
