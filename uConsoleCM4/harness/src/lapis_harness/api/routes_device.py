"""The fleet SSH key and push-to-talk.

The rest of Lapis-Zero's device cockpit (telemetry, UI settings, the screen mirror,
power and pairing) is served by lapis-daemon, which owns the hardware.
"""

from __future__ import annotations

from typing import Any

from aiohttp import web


def _core(request: web.Request) -> Any:
    from lapis_harness.api.server import CORE

    return request.app[CORE]


async def ssh_key(request: web.Request) -> web.Response:
    """The harness's own SSH key: the public half only, as one authorized_keys line."""
    return web.json_response(await _core(request).device_ssh_key())


async def ptt(request: web.Request) -> web.Response:
    """`{"pressed": bool}`: a push-to-talk edge from the HUD, through the daemon."""
    from lapis_harness.api.server import body

    pressed = (await body(request)).get("pressed")
    if not isinstance(pressed, bool):
        raise ValueError("pressed must be true or false")
    voice = await _core(request).ptt(pressed)
    return web.json_response({"ok": True, "pressed": pressed, "voice": voice})


def setup(app: web.Application) -> None:
    r = app.router
    r.add_get("/v1/device/ssh-key", ssh_key)
    r.add_post("/v1/voice/ptt", ptt)
