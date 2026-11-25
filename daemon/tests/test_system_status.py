from __future__ import annotations

import asyncio
import json
from pathlib import Path

import pytest

from lapis_daemon import system_status as st
from lapis_daemon.ble_prov.network_ctl import NetworkCtl, parse_wifi_list


def test_tailscale_status_parsing() -> None:
    running = json.dumps(
        {
            "BackendState": "Running",
            "AuthURL": "",
            "Self": {
                "TailscaleIPs": ["fd7a::1", "100.91.12.4"],
                "DNSName": "lapis-zero.tail.ts.net.",
            },
            "Peer": {"a": {"Online": True}, "b": {"Online": False}},
        }
    )
    assert st.parse_tailscale_status(running) == {
        "state": "connected",
        "ip": "100.91.12.4",
        "fqdn": "lapis-zero.tail.ts.net",
        "auth_url": None,
        "peers": 1,
    }
    login = json.dumps({"BackendState": "NeedsLogin", "AuthURL": "https://login.tailscale.com/a/x"})
    parsed = st.parse_tailscale_status(login)
    assert (parsed["state"], parsed["auth_url"]) == (
        "needs_login",
        "https://login.tailscale.com/a/x",
    )
    # Not installed / not running / garbage all degrade to "offline", never raise.
    for junk in ("", "not json", "{}"):
        assert st.parse_tailscale_status(junk)["state"] == "offline"


def test_wifi_memory_and_battery_parsing(tmp_path: Path) -> None:
    nm = "GENERAL.CONNECTION:HomeWiFi\nIP4.ADDRESS[1]:192.168.1.42/24\nIP4.ADDRESS[2]:10.0.0.2/8\n"
    assert st.parse_nmcli_device(nm) == {"ssid": "HomeWiFi", "ip": "192.168.1.42"}
    assert st.parse_nmcli_device("GENERAL.CONNECTION:--\n") == {"ssid": "", "ip": ""}
    wireless = (
        "Inter-| sta-|   Quality\n face | tus | link level noise\n wlan0: 0000   54.  -56.  -256\n"
    )
    assert st.parse_wireless_rssi(wireless) == -56 and st.parse_wireless_rssi("") is None
    mem = (
        "MemTotal: 507904 kB\nMemAvailable: 307200 kB\nSwapTotal: 507900 kB\nSwapFree: 497660 kB\n"
    )
    assert st.parse_meminfo(mem) == {
        "total_mb": 496,
        "available_mb": 300,
        "zram_total_mb": 495,
        "zram_used_mb": 10,
    }

    assert st.read_battery(tmp_path / "missing") is None
    bat = tmp_path / "bq27220-0"
    bat.mkdir()
    (tmp_path / "ac").mkdir()
    (tmp_path / "ac" / "type").write_text("Mains\n")
    for name, value in (
        ("type", "Battery"),
        ("capacity", "84"),
        ("voltage_now", "3912000"),
        ("status", "Discharging"),
    ):
        (bat / name).write_text(value + "\n")
    assert st.read_battery(tmp_path) == {"percent": 84, "voltage_v": 3.91, "charging": False}


def test_status_snapshot_and_missing_tools(monkeypatch) -> None:
    async def scenario() -> None:
        assert await st.run_cmd("definitely-not-a-real-binary-xyz") == (127, "")
        assert (await st.run_cmd("sleep", "5", timeout=0.05))[0] == 124

        async def not_installed(*argv: str, timeout: float = 5.0) -> tuple[int, str]:
            return 127, ""

        # A freshly flashed device: no nmcli answer, no tailscale. Must not raise.
        monkeypatch.setattr(st, "run_cmd", not_installed)
        monkeypatch.setattr(st, "read_battery", lambda: None)
        real = st.SystemStatus(mock=False, api_key_env="LAPIS_TEST_NO_SUCH_KEY")
        await real.refresh()
        snap = real.snapshot()
        assert snap["tailscale_state"] == "offline" and snap["api_key_ready"] is False
        assert snap["battery_pct"] is None

        mock = st.SystemStatus(mock=True)
        assert mock.snapshot()["battery_pct"] == 84
        telemetry = await mock.telemetry()
        assert telemetry["memory"]["total_mb"] == 496 and "auth_url" not in telemetry["tailscale"]

    asyncio.run(scenario())


def test_network_ctl_sequences_commands_without_the_tools() -> None:
    listing = "HomeWiFi:72:WPA2\nHomeWiFi:40:WPA2\nCafe\\:Guest:55:\n:30:WPA2\nOpen:20:--\n"
    assert parse_wifi_list(listing) == [
        {"ssid": "HomeWiFi", "rssi": -64, "secured": True},
        {"ssid": "Cafe:Guest", "rssi": -73, "secured": False},
        {"ssid": "Open", "rssi": -90, "secured": False},
    ]

    async def scenario() -> None:
        calls: list[tuple[str, ...]] = []

        serve_config = {"TCP": {"443": {"HTTPS": True}}, "proxy": "http://127.0.0.1:8421"}
        key_seen: list[str] = []

        async def run(*argv: str, timeout: float = 5.0, **_: object) -> tuple[int, str]:
            calls.append(argv)
            if argv[:2] == ("nmcli", "-t"):
                return 0, "GENERAL.CONNECTION:HomeWiFi\nIP4.ADDRESS[1]:192.168.1.42/24\n"
            if argv[:3] == ("tailscale", "status", "--json"):
                return 0, json.dumps(
                    {"BackendState": "Running", "Self": {"TailscaleIPs": ["100.1.1.1"]}}
                )
            if argv == ("tailscale", "serve", "status", "--json"):
                return 0, json.dumps(serve_config)
            if argv[:2] == ("tailscale", "up") and argv[-1].startswith("--auth-key=file:"):
                path = Path(argv[-1].removeprefix("--auth-key=file:"))
                key_seen.append(path.read_text())
                assert path.stat().st_mode & 0o777 == 0o600
                return 1, "backend error: invalid key: tskey-auth-abc does not exist\n"
            return 0, ""

        net = NetworkCtl(run, poll=0)
        assert await net.wifi_connect("HomeWiFi", "pw") == {
            "state": "connected",
            "local_ip": "192.168.1.42",
        }
        connect = ("nmcli", "--wait", "30", "dev", "wifi", "connect", "HomeWiFi", "password", "pw")
        stale = ("nmcli", "connection", "delete", "id", "HomeWiFi")
        # A saved profile for the SSID is always removed before nmcli joins it again.
        assert calls.index(stale) < calls.index(connect)
        status = await net.wait_tailscale(timeout=1, poll=0.01)
        assert status["state"] == "connected"
        # Already stored by tailscaled: confirmed from its status, nothing re-run.
        serve = ("tailscale", "serve", "--bg", "--https=443", "http://127.0.0.1:8421")
        assert calls[-1] == ("tailscale", "serve", "status", "--json") and serve not in calls
        # Nothing stored yet. Success is still what tailscaled reports afterwards, not
        # the CLI's exit status: without HTTPS certificates on the tailnet `serve`
        # exits 0 having configured nothing.
        serve_config.clear()
        assert await net.tailscale_serve() is False
        # Loopback target only, HTTPS on the tailnet, and never `funnel`.
        assert calls[-2] == serve
        assert not any("funnel" in " ".join(c) for c in calls)

        # The auth key travels in a 0600 file, never on the command line.
        assert net.tailscale_up_error() is None
        net.tailscale_up("tskey-auth-abc")
        for _ in range(5):
            await asyncio.sleep(0)
        up = calls[-1]
        assert up[:4] == ("tailscale", "up", "--ssh", "--timeout=300s")
        assert key_seen == ["tskey-auth-abc"] and "tskey-auth-abc" not in " ".join(up)
        assert not Path(up[-1].removeprefix("--auth-key=file:")).exists(), "removed afterwards"
        # A rejected key is reported at once, with the key itself redacted.
        assert net.tailscale_up_error() == "backend error: invalid key: tskey-... does not exist"

        # A key sent while an interactive login is pending replaces that login.
        waiting = asyncio.Event()

        async def blocking_run(*argv: str, **_: object) -> tuple[int, str]:
            calls.append(argv)
            if len(argv) == 4:  # `tailscale up --ssh --timeout=300s`, no key: waits for a login
                await waiting.wait()
            return 0, ""

        net = NetworkCtl(blocking_run, poll=0)
        net.tailscale_up()
        await asyncio.sleep(0)
        interactive = net._up
        net.tailscale_up()  # a second request without a key changes nothing
        assert net._up is interactive
        net.tailscale_up("tskey-auth-xyz")
        for _ in range(5):
            await asyncio.sleep(0)
        assert interactive.cancelled() and net._up is not interactive
        assert calls[-1][-1].startswith("--auth-key=file:")

    asyncio.run(scenario())


def test_run_cmd_kills_its_child_when_cancelled(tmp_path: Path) -> None:
    """A cancelled `tailscale up` must not keep waiting for a login nobody watches."""
    import os
    import shlex
    import sys

    from lapis_daemon.system_status import run_cmd

    pidfile = tmp_path / "pid"
    code = f"import os, time; open({str(pidfile)!r}, 'w').write(str(os.getpid())); time.sleep(60)"
    # Through a shell wrapper, like /usr/local/bin/tailscale on a Mac: the grandchild
    # inherits our pipe, and killing only the wrapper used to hang the caller.
    wrapper = f"{sys.executable} -c {shlex.quote(code)}; true"

    async def scenario() -> int:
        task = asyncio.create_task(run_cmd("/bin/sh", "-c", wrapper, timeout=60))
        while not (pidfile.exists() and pidfile.read_text()):
            await asyncio.sleep(0.02)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        return int(pidfile.read_text())

    pid = asyncio.run(scenario())
    with pytest.raises(ProcessLookupError):
        os.kill(pid, 0)
