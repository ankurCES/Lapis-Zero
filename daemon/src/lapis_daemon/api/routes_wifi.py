"""Wi-Fi: status, scan, join, forget, and the phone Wi-Fi window over Bluetooth.

The password goes to NetworkManager's root-only store and nowhere else: never into a
response, the journal or an error message (validation names the field, never its value).
"""

from __future__ import annotations

import re
from typing import Any

from aiohttp import web

PSK_HEX_RE = re.compile(r"[0-9A-Fa-f]{64}")


def _core(request: web.Request) -> Any:
    from lapis_daemon.api.server import CORE

    return request.app[CORE]


def _by(request: web.Request) -> str:
    from lapis_daemon.api.server import DEVICE

    return str(request[DEVICE]["id"])


def valid_ssid(value: Any) -> str:
    if not isinstance(value, str) or not value:
        raise ValueError("ssid is required")
    if len(value.encode()) > 32:
        raise ValueError("ssid must be at most 32 bytes")
    return value


def valid_psk(value: Any) -> str | None:
    """None for an open network; a WPA passphrase (8..63 printable ASCII) or a 64-hex key."""
    if value is None or value == "":
        return None
    if isinstance(value, str) and (
        PSK_HEX_RE.fullmatch(value)
        or (8 <= len(value) <= 63 and all(" " <= ch <= "~" for ch in value))
    ):
        return value
    raise ValueError("psk must be 8 to 63 printable ASCII characters or 64 hex digits")


async def wifi(request: web.Request) -> web.Response:
    return web.json_response(await _core(request).wifi_info())


async def scan(request: web.Request) -> web.Response:
    from lapis_daemon.api.server import error

    try:
        networks = await _core(request).wifi_scan()
    except RuntimeError as exc:
        return error(503, "wifi_unavailable", str(exc))
    return web.json_response({"networks": networks})


async def connect(request: web.Request) -> web.Response:
    """Can take ~45 s; a phone on the old network may never see the answer."""
    from lapis_daemon.api.server import body

    data = await body(request)
    ssid, psk = valid_ssid(data.get("ssid")), valid_psk(data.get("psk"))
    return web.json_response(await _core(request).wifi_connect(ssid, psk, _by(request)))


async def forget(request: web.Request) -> web.Response:
    from lapis_daemon.api.server import body

    name = (await body(request)).get("name")
    if not isinstance(name, str) or not name:
        raise ValueError("name is required")
    return web.json_response(await _core(request).wifi_forget(name, _by(request)))


async def ble(request: web.Request) -> web.Response:
    from lapis_daemon.api.server import error

    core = _core(request)
    if await core.auth.count() == 0:
        return error(409, "not_paired", "No phone is paired with this device")
    try:
        await core.wifi_ble_open()
    except RuntimeError as exc:
        return error(503, "ble_unavailable", str(exc))
    return web.json_response(await core.wifi_info())


def setup(app: web.Application) -> None:
    r = app.router
    r.add_get("/v1/wifi", wifi)
    r.add_get("/v1/wifi/scan", scan)
    r.add_post("/v1/wifi/connect", connect)
    r.add_post("/v1/wifi/forget", forget)
    r.add_post("/v1/wifi/ble", ble)
