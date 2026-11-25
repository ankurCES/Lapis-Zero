"""Live device status for `state_sync` (SPEC 6.1) and the telemetry endpoint.

Every probe is either a small file read or a short subprocess with a timeout, and
every one degrades to "unknown" instead of raising: this module runs on a device
whose Wi-Fi, Tailscale and battery may all be absent on first boot. The parsers
are pure functions so they are tested without the hardware.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import signal
import time
from pathlib import Path
from typing import Any

from lapis_daemon.config import HostsConfig, mock_hosts
from lapis_daemon.db import LapisDb

SERVICES = ("lapis-ui", "lapis-daemon", "tailscaled", "NetworkManager", "bluetooth")

# tailscale BackendState -> the vocabulary the UI and the app render.
TAILSCALE_STATES = {
    "Running": "connected",
    "NeedsLogin": "needs_login",
    "NeedsMachineAuth": "needs_approval",
    "Starting": "starting",
    "Stopped": "stopped",
}


async def run_cmd(*argv: str, timeout: float = 5.0, merge_stderr: bool = False) -> tuple[int, str]:
    """Run a command; (returncode, stdout). Missing binary or timeout -> (127|124, '').

    `merge_stderr` folds stderr into the returned text, for tools that explain a
    failure there (`tailscale up`). Nothing is ever logged here.
    """
    try:
        proc = await asyncio.create_subprocess_exec(
            *argv,
            stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.STDOUT if merge_stderr else asyncio.subprocess.DEVNULL,
            start_new_session=True,  # its own process group, so all of it can be killed
        )
    except (FileNotFoundError, PermissionError):
        return 127, ""
    try:
        out, _ = await asyncio.wait_for(proc.communicate(), timeout)
    except TimeoutError:
        await _kill_group(proc)
        return 124, ""
    except asyncio.CancelledError:
        # A cancelled caller must not leave the command running (`tailscale up`
        # would go on waiting for a login nobody is watching any more).
        await _kill_group(proc)
        raise
    return proc.returncode or 0, out.decode(errors="replace")


async def _kill_group(proc: asyncio.subprocess.Process) -> None:
    """Kill the command and anything it started; never wait on it for long.

    A wrapper script's child can outlive it holding our pipe, and `proc.wait()`
    only returns once that pipe closes, so the wait is bounded.
    """
    with contextlib.suppress(ProcessLookupError, PermissionError):
        os.killpg(proc.pid, signal.SIGKILL)
    with contextlib.suppress(TimeoutError):
        await asyncio.wait_for(proc.wait(), 2)


def _read(path: str | Path) -> str:
    try:
        return Path(path).read_text().strip()
    except OSError:
        return ""


def parse_tailscale_status(text: str) -> dict[str, Any]:
    """Reduce `tailscale status --json` to the fields Lapis shows."""
    try:
        data = json.loads(text)
    except ValueError:
        data = {}
    me = data.get("Self") or {}
    ips = [ip for ip in (me.get("TailscaleIPs") or []) if "." in ip]
    peers = data.get("Peer") or {}
    return {
        "state": TAILSCALE_STATES.get(data.get("BackendState", ""), "offline"),
        "ip": ips[0] if ips else "",
        "fqdn": (me.get("DNSName") or "").rstrip("."),
        "auth_url": data.get("AuthURL") or None,
        "peers": sum(1 for p in peers.values() if p.get("Online")),
    }


def parse_nmcli_device(text: str) -> dict[str, str]:
    """`nmcli -t -f GENERAL.CONNECTION,IP4.ADDRESS device show wlan0` -> ssid + ip."""
    ssid = ip = ""
    for line in text.splitlines():
        key, _, value = line.partition(":")
        if key == "GENERAL.CONNECTION" and value not in ("", "--"):
            ssid = value
        elif key.startswith("IP4.ADDRESS") and not ip:
            ip = value.split("/")[0]
    return {"ssid": ssid, "ip": ip}


def parse_wireless_rssi(text: str, iface: str = "wlan0") -> int | None:
    """Signal level in dBm from /proc/net/wireless, or None when not associated."""
    for line in text.splitlines():
        name, _, rest = line.partition(":")
        if name.strip() == iface:
            fields = rest.split()
            if len(fields) >= 3:
                try:
                    return int(float(fields[2]))
                except ValueError:
                    return None
    return None


def parse_meminfo(text: str) -> dict[str, int]:
    """MemTotal / MemAvailable / SwapTotal / SwapFree in MiB."""
    kb = {}
    for line in text.splitlines():
        key, _, rest = line.partition(":")
        parts = rest.split()
        if parts and parts[0].isdigit():
            kb[key] = int(parts[0])
    return {
        "total_mb": kb.get("MemTotal", 0) // 1024,
        "available_mb": kb.get("MemAvailable", 0) // 1024,
        "zram_total_mb": kb.get("SwapTotal", 0) // 1024,
        "zram_used_mb": (kb.get("SwapTotal", 0) - kb.get("SwapFree", 0)) // 1024,
    }


def read_battery(root: str | Path = "/sys/class/power_supply") -> dict[str, Any] | None:
    """BQ27220 via the power-supply class; None when there is no battery device."""
    try:
        entries = sorted(Path(root).iterdir())
    except OSError:
        return None
    for dev in entries:
        if _read(dev / "type") != "Battery":
            continue
        capacity = _read(dev / "capacity")
        voltage = _read(dev / "voltage_now")
        return {
            "percent": int(capacity) if capacity.isdigit() else None,
            "voltage_v": round(int(voltage) / 1_000_000, 2) if voltage.isdigit() else None,
            "charging": _read(dev / "status") in ("Charging", "Full"),
        }
    return None


def rss_mb(pid: int | str = "self") -> int:
    for line in _read(f"/proc/{pid}/status").splitlines():
        if line.startswith("VmRSS:"):
            return int(line.split()[1]) // 1024
    return 0


class SystemStatus:
    """Cached status snapshot, refreshed off the event loop's hot path."""

    def __init__(self, *, mock: bool = False, api_key_env: str = "ANTHROPIC_API_KEY") -> None:
        self.mock = mock
        self.api_key_env = api_key_env
        self.started = time.time()
        self.wifi = {"ssid": "", "ip": ""}
        self.tailscale: dict[str, Any] = parse_tailscale_status("")
        self.battery: dict[str, Any] | None = None
        if mock:
            self.wifi = {"ssid": "HomeWiFi", "ip": "192.168.1.42"}
            self.tailscale = {
                "state": "connected",
                "ip": "100.91.12.4",
                "fqdn": "lapis-zero.tailnet.ts.net",
                "auth_url": None,
                "peers": 3,
            }
            self.battery = {"percent": 84, "voltage_v": 3.9, "charging": False}

    async def refresh(self) -> None:
        """Re-probe Wi-Fi, Tailscale and the battery (no-op in mock mode)."""
        if self.mock:
            return
        (_, nm), (_, ts) = await asyncio.gather(
            run_cmd(
                "nmcli", "-t", "-f", "GENERAL.CONNECTION,IP4.ADDRESS", "device", "show", "wlan0"
            ),
            run_cmd("tailscale", "status", "--json"),
        )
        self.wifi = parse_nmcli_device(nm)
        self.tailscale = parse_tailscale_status(ts)
        self.battery = await asyncio.to_thread(read_battery)

    @property
    def api_key_ready(self) -> bool:
        return self.mock or bool(os.environ.get(self.api_key_env))

    def snapshot(self) -> dict[str, Any]:
        """The system half of `state_sync` (SPEC 6.1)."""
        battery = self.battery or {}
        return {
            "wifi_ssid": self.wifi["ssid"],
            "local_ip": self.wifi["ip"],
            "tailscale_state": self.tailscale["state"],
            "tailscale_ip": self.tailscale["ip"],
            "tailscale_fqdn": self.tailscale["fqdn"],
            "tailscale_auth_url": self.tailscale["auth_url"],
            "api_key_ready": self.api_key_ready,
            "battery_pct": battery.get("percent"),
            "battery_charging": bool(battery.get("charging")),
        }

    async def telemetry(self) -> dict[str, Any]:
        """Device cockpit payload (ANDROID_COMPANION_SPEC 5.2 `/v1/device/telemetry`)."""
        uptime = int(time.time() - self.started)
        if self.mock:
            return {
                "battery": self.battery,
                "cpu": {"temp_c": 47.5, "load1": 0.32},
                "memory": {
                    "total_mb": 496,
                    "available_mb": 301,
                    "zram_total_mb": 496,
                    "zram_used_mb": 0,
                    "daemon_rss_mb": rss_mb(),
                    "ui_rss_mb": 6,
                },
                "wifi": {**self.wifi, "rssi_dbm": -58},
                "tailscale": {k: v for k, v in self.tailscale.items() if k != "auth_url"},
                "uptime_sec": uptime,
                "services": dict.fromkeys(SERVICES, "active"),
            }
        temp = _read("/sys/class/thermal/thermal_zone0/temp")
        load = _read("/proc/loadavg").split()
        (_, ui_pid), (_, active) = await asyncio.gather(
            run_cmd("pidof", "lapis-ui"),
            run_cmd("systemctl", "is-active", *(f"{s}.service" for s in SERVICES)),
        )
        states = active.split()
        sys_uptime = _read("/proc/uptime").split()
        return {
            "battery": self.battery,
            "cpu": {
                "temp_c": round(int(temp) / 1000, 1) if temp.lstrip("-").isdigit() else None,
                "load1": float(load[0]) if load else None,
            },
            "memory": {
                **parse_meminfo(_read("/proc/meminfo")),
                "daemon_rss_mb": rss_mb(),
                "ui_rss_mb": rss_mb(ui_pid.split()[0]) if ui_pid.split() else 0,
            },
            "wifi": {**self.wifi, "rssi_dbm": parse_wireless_rssi(_read("/proc/net/wireless"))},
            "tailscale": {k: v for k, v in self.tailscale.items() if k != "auth_url"},
            "uptime_sec": int(float(sys_uptime[0])) if sys_uptime else uptime,
            "services": {
                s: (states[i] if i < len(states) else "unknown") for i, s in enumerate(SERVICES)
            },
        }


# --- kept for the IPC contract tests ----------------------------------------


def build_state_sync(
    db: LapisDb | None,  # unused; kept so callers need not change
    hosts: HostsConfig,
    agent_state: str,
    active_host: str,
    active_task: dict[str, Any] | None,
    wifi_ssid: str,
    local_ip: str,
    tailscale_state: str,
    tailscale_ip: str,
    tailscale_auth_url: str | None,
    api_key_ready: bool,
    next_cron: str,
) -> dict[str, Any]:
    return {
        "type": "state_sync",
        "agent_state": agent_state,
        "active_host": active_host,
        "hosts": list(hosts.hosts),
        "active_task": active_task,
        "next_cron": next_cron,
        "wifi_ssid": wifi_ssid,
        "local_ip": local_ip,
        "tailscale_state": tailscale_state,
        "tailscale_ip": tailscale_ip,
        "tailscale_auth_url": tailscale_auth_url,
        "api_key_ready": api_key_ready,
    }


def mock_state() -> dict[str, Any]:
    return build_state_sync(
        None,
        mock_hosts(),
        agent_state="IDLE",
        active_host="macbook-pro",
        active_task=None,
        wifi_ssid="HomeWiFi",
        local_ip="192.168.1.42",
        tailscale_state="connected",
        tailscale_ip="100.91.12.4",
        tailscale_auth_url=None,
        api_key_ready=True,
        next_cron="08:00 Daily Digest",
    )
