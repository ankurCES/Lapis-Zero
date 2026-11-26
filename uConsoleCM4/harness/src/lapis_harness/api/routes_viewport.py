"""The HUD's live view of a fleet host (FLEET workspace): fresh frames and input.

Both work whether or not a task runs on the host. Every frame and every input is put to
the Policy Gate first, with the runner's tool names (`screenshot`, `mouse_click`, ...),
and runs only if it was allowed.
"""

from __future__ import annotations

from typing import Any

from aiohttp import web

from lapis_harness.agent.runner import INPUT_TOOLS


def _core(request: web.Request) -> Any:
    from lapis_harness.api.server import CORE

    return request.app[CORE]


def _host(request: web.Request) -> str:
    host_id = request.match_info["host_id"]
    if not _core(request).is_host(host_id):
        raise LookupError("Unknown host")
    return host_id


async def screen(request: web.Request) -> web.Response:
    """`GET /v1/hosts/{id}/screen.jpg`: a frame captured now, kept in the frame ring."""
    from lapis_harness.api.server import error, source

    host_id = _host(request)
    try:
        shot, frame = await _core(request).host_screen(host_id, source(request))
    except PermissionError as exc:
        return error(403, "denied", str(exc))
    headers = {
        "X-Lapis-Width": str(frame.width),
        "X-Lapis-Height": str(frame.height),
        "Cache-Control": "no-store",
    }
    if shot is not None:
        headers["X-Lapis-Screenshot-Id"] = str(shot)
    return web.Response(body=frame.jpeg, content_type="image/jpeg", headers=headers)


async def host_input(request: web.Request) -> web.Response:
    """`POST /v1/hosts/{id}/input`: `{action: click|move|scroll|type|key, x, y, ...}`."""
    from lapis_harness.api.server import body, source

    host_id = _host(request)
    data = await body(request)
    action = data.get("action")
    if action not in INPUT_TOOLS:
        raise ValueError(f"action must be one of {', '.join(INPUT_TOOLS)}")
    for key in ("x", "y", "dx", "dy"):
        if key in data and not (isinstance(data[key], int) and not isinstance(data[key], bool)):
            raise ValueError(f"{key} must be an integer")
    for key in ("button", "text", "combo"):
        if key in data and not isinstance(data[key], str):
            raise ValueError(f"{key} must be a string")
    if data.get("button", "left") not in ("left", "right", "middle"):
        raise ValueError("button must be left, right or middle")
    outcome = await _core(request).host_input(host_id, action, data, source(request))
    answer: dict[str, Any] = {"ok": outcome.allowed, "decision": outcome.decision}
    if outcome.tier is not None:
        answer["tier"] = outcome.tier
    if not outcome.allowed:
        answer["error"] = outcome.reason or outcome.decision
    return web.json_response(answer)


def setup(app: web.Application) -> None:
    r = app.router
    r.add_get("/v1/hosts/{host_id}/screen.jpg", screen)
    r.add_post("/v1/hosts/{host_id}/input", host_input)
