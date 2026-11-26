"""Wi-Fi (NetworkManager) and Tailscale control used by provisioning and the daemon.

Thin wrappers over `nmcli` / `tailscale` / `rfkill`. The command runner and the
NetworkManager keyfile directory are injectable so the parsing and sequencing are
tested without the tools installed.

A Wi-Fi password never goes on a command line (any local process can read
/proc/<pid>/cmdline): it is written into a root-only NetworkManager keyfile.
"""

from __future__ import annotations

import asyncio
import contextlib
import os
import re
import tempfile
import uuid
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

from lapis_daemon.config import API_DEFAULT_PORT
from lapis_daemon.system_status import parse_nmcli_device, parse_tailscale_status, run_cmd

Runner = Callable[..., Awaitable[tuple[int, str]]]
NM_CONNECTIONS_DIR = "/etc/NetworkManager/system-connections"
NM_WIFI_TYPES = ("802-11-wireless", "wifi")  # TYPE as `nmcli -t connection show` prints it

# A Tailscale auth key, should one ever show up in CLI output we relay or log.
_TSKEY_RE = re.compile(r"tskey-[A-Za-z0-9_-]+")


def _split_terse(line: str) -> list[str]:
    """Split one `nmcli -t` line on unescaped colons (SSIDs may contain `\\:`)."""
    out, cur, esc = [], "", False
    for ch in line:
        if esc:
            cur += ch
            esc = False
        elif ch == "\\":
            esc = True
        elif ch == ":":
            out.append(cur)
            cur = ""
        else:
            cur += ch
    out.append(cur)
    return out


def parse_wifi_list(text: str) -> list[dict[str, Any]]:
    """`nmcli -t -f SSID,SIGNAL,SECURITY dev wifi list` -> strongest entry per SSID.

    NetworkManager reports signal as a 0-100 quality; the protocol carries an
    approximate dBm (quality/2 - 100), which is what phones show.
    """
    best: dict[str, dict[str, Any]] = {}
    for line in text.splitlines():
        parts = _split_terse(line)
        if len(parts) < 3 or not parts[0]:
            continue
        try:
            quality = int(parts[1])
        except ValueError:
            continue
        entry = {
            "ssid": parts[0],
            "rssi": quality // 2 - 100,
            "secured": parts[2] not in ("", "--"),
        }
        if parts[0] not in best or entry["rssi"] > best[parts[0]]["rssi"]:
            best[parts[0]] = entry
    return sorted(best.values(), key=lambda e: -e["rssi"])


def parse_saved(text: str) -> list[dict[str, Any]]:
    """`nmcli -t -f NAME,TYPE,ACTIVE connection show` -> the saved Wi-Fi profiles."""
    out = []
    for line in text.splitlines():
        parts = _split_terse(line)
        if len(parts) >= 3 and parts[0] and parts[1] in NM_WIFI_TYPES:
            out.append({"name": parts[0], "active": parts[2] == "yes"})
    return out


def key_mgmt_for(scan: str, ssid: str) -> str:
    """`sae` when the scan shows WPA3 and no WPA2/WPA1 for `ssid`, else `wpa-psk`.

    `scan` is `nmcli -t -f SSID,SECURITY dev wifi list`. A transition-mode network
    (WPA2 + WPA3) and one not in the scan (hidden, out of range) get `wpa-psk`.
    """
    seen: set[str] = set()
    for line in scan.splitlines():
        parts = _split_terse(line)
        if len(parts) >= 2 and parts[0] == ssid:
            seen.update(parts[1].split())
    return "sae" if "WPA3" in seen and not seen & {"WPA1", "WPA2"} else "wpa-psk"


def keyfile_escape(value: str) -> str:
    """GKeyFile string value: `\\` -> `\\\\`, space -> `\\s`; control characters refused."""
    if any(ord(ch) < 0x20 or ord(ch) == 0x7F for ch in value):
        raise ValueError("the network name or password contains control characters")
    return value.replace("\\", "\\\\").replace(" ", "\\s")


def render_keyfile(ssid: str, psk: str, conn_uuid: str, key_mgmt: str = "wpa-psk") -> str:
    """A NetworkManager keyfile for one WPA network (the SSID as a byte list: no escaping)."""
    ssid_bytes = "".join(f"{b};" for b in ssid.encode())
    return (
        f"[connection]\nid={keyfile_escape(ssid)}\nuuid={conn_uuid}\ntype=wifi\n"
        "autoconnect=true\n\n"
        f"[wifi]\nmode=infrastructure\nssid={ssid_bytes}\n\n"
        f"[wifi-security]\nkey-mgmt={key_mgmt}\npsk={keyfile_escape(psk)}\n\n"
        "[ipv4]\nmethod=auto\n\n[ipv6]\nmethod=auto\n"
    )


def _write_private(path: Path, text: str) -> None:
    """0600 from the first byte (NetworkManager refuses a keyfile others can read)."""
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as fh:
        os.fchmod(fh.fileno(), 0o600)
        fh.write(text)


class NetworkCtl:
    def __init__(
        self,
        run: Runner = run_cmd,
        port: int = API_DEFAULT_PORT,
        poll: float = 1.0,
        nm_dir: str | Path = NM_CONNECTIONS_DIR,
    ) -> None:
        self.run = run
        self.port = port
        self.poll = poll  # seconds between state polls (0 in tests)
        self.nm_dir = Path(nm_dir)
        self._up: asyncio.Task[Any] | None = None

    async def unblock_radios(self) -> None:
        await self.run("rfkill", "unblock", "wifi")
        await self.run("rfkill", "unblock", "bluetooth")
        await self.run("nmcli", "radio", "wifi", "on")

    async def wifi_ready(self, tries: int = 6) -> bool:
        """Wait until NetworkManager can use wlan0 (state 30 "disconnected" or higher).

        Right after the radio is switched on the device stays "unavailable" while
        wpa_supplicant attaches; nmcli answers a scan in that window with an empty list.
        """
        for attempt in range(tries):
            _, out = await self.run("nmcli", "-g", "GENERAL.STATE", "device", "show", "wlan0")
            code = out.strip().split(" ", 1)[0]
            if code.isdigit() and int(code) >= 30:
                return True
            if attempt + 1 < tries:
                await asyncio.sleep(self.poll)
        return False

    async def wifi_scan(self) -> list[dict[str, Any]]:
        if not await self.wifi_ready():
            raise RuntimeError("Wi-Fi is not available on the device (wlan0 missing or blocked)")
        await self.run("nmcli", "dev", "wifi", "rescan", timeout=10)
        # nmcli itself waits up to 15 s for the scan: leave it room to print what it has.
        _, out = await self.run(
            "nmcli", "-t", "-f", "SSID,SIGNAL,SECURITY", "dev", "wifi", "list", timeout=20
        )
        return parse_wifi_list(out)[:20]

    def keyfile_path(self, ssid: str) -> Path:
        return self.nm_dir / f"lapis-{ssid.encode().hex()}.nmconnection"

    async def wifi_connect(self, ssid: str, psk: str | None) -> dict[str, Any]:
        """Join a network and keep its profile only if that worked.

        The PSK goes into a root-only keyfile that NetworkManager loads, never into
        an argv. ValueError (before anything changed) for a name or password with
        control characters.
        """
        await self.wifi_ready(tries=3)
        keyfile = text = None
        conn_uuid = str(uuid.uuid4())
        if psk:
            _, scan = await self.run(
                "nmcli", "-t", "-f", "SSID,SECURITY", "dev", "wifi", "list", "--rescan", "no",
                timeout=10,
            )  # fmt: skip
            text = render_keyfile(ssid, psk, conn_uuid, key_mgmt_for(scan, ssid))
        # nmcli 1.52 reuses a saved profile for the SSID but drops its key-mgmt when a
        # password is passed ("key-mgmt: property is missing"), so a retry after one wrong
        # password could never succeed: always start from a clean profile.
        await self.run("nmcli", "connection", "delete", "id", ssid, timeout=10)
        if text is not None:
            keyfile = self.keyfile_path(ssid)
            _write_private(keyfile, text)
            rc, _ = await self.run("nmcli", "connection", "load", str(keyfile), timeout=10)
            if rc == 0:
                rc, _ = await self.run(
                    "nmcli", "--wait", "30", "connection", "up", "uuid", conn_uuid, timeout=40
                )
            forget: tuple[str, ...] = ("nmcli", "connection", "delete", "uuid", conn_uuid)
        else:
            rc, _ = await self.run(
                "nmcli", "--wait", "30", "dev", "wifi", "connect", ssid, timeout=40
            )
            forget = ("nmcli", "connection", "delete", "id", ssid)
        status = await self.wifi_status()
        # NetworkManager reports "activated" as soon as IPv6 is up; DHCPv4 may trail it.
        for _ in range(10):
            if rc != 0 or status["ip"]:
                break
            await asyncio.sleep(self.poll)
            status = await self.wifi_status()
        if rc != 0 or not status["ip"]:
            print(f"lapis: wifi connect failed (nmcli rc={rc})", flush=True)
            # Connect = save only on success: a wrong password is not kept around.
            await self.run(*forget, timeout=10)
            if keyfile is not None:
                _unlink(str(keyfile))
            return {"state": "failed", "local_ip": ""}
        return {"state": "connected", "local_ip": status["ip"]}

    async def wifi_saved(self) -> list[dict[str, Any]]:
        _, out = await self.run("nmcli", "-t", "-f", "NAME,TYPE,ACTIVE", "connection", "show")
        return parse_saved(out)

    async def wifi_forget(self, name: str) -> bool:
        rc, _ = await self.run("nmcli", "connection", "delete", "id", name, timeout=10)
        return rc == 0

    async def wifi_status(self) -> dict[str, str]:
        _, out = await self.run(
            "nmcli", "-t", "-f", "GENERAL.CONNECTION,IP4.ADDRESS", "device", "show", "wlan0"
        )
        return parse_nmcli_device(out)

    async def tailscale_status(self) -> dict[str, Any]:
        _, out = await self.run("tailscale", "status", "--json")
        return parse_tailscale_status(out)

    def tailscale_up(self, authkey: str | None = None) -> None:
        """Start `tailscale up` in the background (it blocks until the node is authorized).

        Without an auth key it prints a login URL, which `tailscale status --json`
        exposes as AuthURL; callers poll `tailscale_status()` for it.
        """
        if self._up is not None and not self._up.done():
            if not authkey:
                return
            # A key sent while an interactive login is still pending replaces it;
            # ignoring it would look like five silent minutes and a timeout.
            self._up.cancel()
        argv = ["tailscale", "up", "--ssh", "--timeout=300s"]
        keyfile = None
        if authkey:
            # Not on the command line (any local process can read /proc/*/cmdline):
            # tailscale reads `file:` itself. 0600, on tmpfs where there is one.
            fd, keyfile = tempfile.mkstemp(
                prefix="lapis-tskey-", dir="/run" if os.access("/run", os.W_OK) else None
            )
            with os.fdopen(fd, "w") as fh:
                fh.write(authkey)
            argv.append(f"--auth-key=file:{keyfile}")
        self._up = asyncio.create_task(self.run(*argv, timeout=310, merge_stderr=True))
        if keyfile:
            self._up.add_done_callback(lambda _task, path=keyfile: _unlink(path))

    def tailscale_up_error(self) -> str | None:
        """Why the last `tailscale up` gave up (a rejected auth key, say), if it did.

        None while it is still running or after it succeeded. Without this a bad
        key looked like five silent minutes followed by a generic timeout.
        """
        task = self._up
        if task is None or not task.done() or task.cancelled() or task.exception():
            return None
        rc, out = task.result()
        if rc == 0:
            return None
        lines = [ln.strip() for ln in out.splitlines() if ln.strip()]
        reason = lines[-1] if lines else f"tailscale up failed (exit {rc})"
        return _TSKEY_RE.sub("tskey-...", reason)[:160]

    async def tailscale_serve(self) -> bool:
        """Expose the loopback control plane over tailnet HTTPS (never funnel).

        True only when tailscaled actually stored the proxy: without HTTPS
        certificates enabled on the tailnet the CLI prints a link and exits 0 (or
        blocks) having configured nothing, so its exit status proves nothing.
        """

        async def stored() -> bool:
            _, out = await self.run("tailscale", "serve", "status", "--json")
            return f"127.0.0.1:{self.port}" in out  # "{}" when nothing is configured

        # Already persisted by tailscaled (it survives reboots): nothing to do, and
        # no 20 s CLI call from a caller that asks again every minute.
        if await stored():
            return True
        target = f"http://127.0.0.1:{self.port}"
        await self.run("tailscale", "serve", "--bg", "--https=443", target, timeout=20)
        return await stored()

    async def wait_tailscale(self, timeout: float = 300, poll: float = 2.0) -> dict[str, Any]:
        """Poll until the node is Running (then enable serve) or `timeout` elapses."""
        deadline = asyncio.get_running_loop().time() + timeout
        status = await self.tailscale_status()
        while status["state"] != "connected" and asyncio.get_running_loop().time() < deadline:
            await asyncio.sleep(poll)
            status = await self.tailscale_status()
        if status["state"] == "connected":
            await self.tailscale_serve()
        return status

    async def stop(self) -> None:
        if self._up is not None and not self._up.done():
            self._up.cancel()


def _unlink(path: str) -> None:
    with contextlib.suppress(OSError):
        os.unlink(path)
