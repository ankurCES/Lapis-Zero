"""Device cockpit: telemetry, LCD settings, screen mirror, power and pairing."""

from __future__ import annotations

import re
from typing import Any

from aiohttp import web

POWER_ACTIONS = ("reboot", "shutdown", "restart_daemon")
TZ_RE = re.compile(r"^[A-Za-z][A-Za-z0-9_+-]*(/[A-Za-z0-9_+-]+){0,2}$")
PAIRING_ACTIONS = {"start_ble_pairing": "ble", "start_qr_pairing": "qr"}


def _core(request: web.Request) -> Any:
    from lapis_daemon.api.server import CORE

    return request.app[CORE]


async def telemetry(request: web.Request) -> web.Response:
    core = _core(request)
    await core.status.refresh()
    return web.json_response(await core.status.telemetry())


async def patch_ui(request: web.Request) -> web.Response:
    from lapis_daemon.api.server import body

    data = await body(request)
    theme = data.get("theme")
    if theme not in (None, "dark", "paper"):
        raise ValueError("theme must be dark or paper")
    brightness = data.get("brightness")
    if brightness is not None and not (isinstance(brightness, int) and 0 <= brightness <= 100):
        raise ValueError("brightness must be an integer from 0 to 100")
    timezone = data.get("timezone")
    if timezone is not None and not (isinstance(timezone, str) and TZ_RE.match(timezone)):
        raise ValueError("timezone must be an IANA name such as America/Los_Angeles")
    try:
        ui = await _core(request).set_ui(
            theme, brightness, data.get("mute_audio"), bool(data.get("wake")), timezone
        )
    except KeyError as exc:  # zoneinfo.ZoneInfoNotFoundError
        raise ValueError(f"unknown timezone: {timezone}") from exc
    return web.json_response(ui)


async def screen(request: web.Request) -> web.Response:
    from lapis_daemon.api.server import error

    try:
        png = await _core(request).screen_png()
    except TimeoutError:
        return error(504, "ui_unavailable", "lapis-ui did not answer the screen request")
    return web.Response(body=png, content_type="image/png", headers={"Cache-Control": "no-store"})


async def ssh_key(request: web.Request) -> web.Response:
    """The device's own SSH key: the public half only, as one authorized_keys line."""
    return web.json_response(await _core(request).device_ssh_key())


async def power(request: web.Request) -> web.Response:
    from lapis_daemon.api.server import body

    core = _core(request)
    action = str((await body(request)).get("action", ""))
    if action in POWER_ACTIONS:
        await core.system_control(action)
    elif action in PAIRING_ACTIONS:
        await core.pairing_start(PAIRING_ACTIONS[action])
    elif action == "stop_pairing":
        await core.pairing_stop()
    else:
        raise ValueError(f"Unknown action: {action!r}")
    return web.json_response({"ok": True, "action": action}, status=202)


def setup(app: web.Application) -> None:
    r = app.router
    r.add_get("/v1/device/telemetry", telemetry)
    r.add_patch("/v1/device/ui", patch_ui)
    r.add_get("/v1/device/screen.png", screen)
    r.add_get("/v1/device/ssh-key", ssh_key)
    r.add_post("/v1/device/power", power)
