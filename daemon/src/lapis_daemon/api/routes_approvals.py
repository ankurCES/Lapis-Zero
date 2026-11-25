"""Approvals, screenshots, manual assist and agent control."""

from __future__ import annotations

from typing import Any

from aiohttp import web


def _core(request: web.Request) -> Any:
    from lapis_daemon.api.server import CORE

    return request.app[CORE]


def _device(request: web.Request) -> dict[str, Any]:
    from lapis_daemon.api.server import DEVICE

    return request[DEVICE]


def _by(request: web.Request) -> str:
    return f"android:{_device(request)['name']}"


async def pending(request: web.Request) -> web.Response:
    return web.json_response({"approvals": list(_core(request).runner.pending.values())})


async def decide(request: web.Request) -> web.Response:
    from lapis_daemon.api.server import body

    data = await body(request)
    decision = data.get("decision")
    if decision not in ("approve_once", "approve_session", "deny"):
        raise ValueError("decision must be approve_once, approve_session or deny")
    ok = _core(request).decide(
        request.match_info["request_id"], decision, _by(request), str(data.get("reason", ""))
    )
    if not ok:
        raise LookupError("No pending approval with that id")
    return web.json_response({"ok": True})


async def screenshot(request: web.Request) -> web.StreamResponse:
    row = await _core(request).frames.get(int(request.match_info["id"]))
    if row is None:
        raise LookupError("Screenshot not found (it may have been evicted)")
    headers = {
        "X-Lapis-Width": str(row["width"]),
        "X-Lapis-Height": str(row["height"]),
        "Cache-Control": "private, max-age=86400, immutable",
    }
    if row["click_x"] is not None:
        headers["X-Lapis-Click-X"] = str(row["click_x"])
        headers["X-Lapis-Click-Y"] = str(row["click_y"])
    return web.FileResponse(row["path"], headers=headers)


async def nudge(request: web.Request) -> web.Response:
    from lapis_daemon.api.server import body, error

    core = _core(request)
    host_id = request.match_info["host_id"]
    if host_id not in core.hosts.hosts:
        raise LookupError("Unknown host")
    data = await body(request)
    try:
        await core.runner.nudge(host_id, str(data.get("action", "")), data)
    except ValueError as exc:
        return error(409, "not_paused", str(exc))
    return web.json_response({"ok": True})


async def kill_all(request: web.Request) -> web.Response:
    await _core(request).kill_all(_by(request))
    return web.json_response({"ok": True})


async def pause(request: web.Request) -> web.Response:
    core = _core(request)
    core.runner.pause()
    core.broadcast_state()
    return web.json_response({"agent_state": core.runner.agent_state})


async def resume(request: web.Request) -> web.Response:
    core = _core(request)
    core.runner.resume()
    core.broadcast_state()
    return web.json_response({"agent_state": core.runner.agent_state})


async def active_host(request: web.Request) -> web.Response:
    from lapis_daemon.api.server import body

    core = _core(request)
    host_id = str((await body(request)).get("host_id", ""))
    if not core.select_host(host_id):
        raise LookupError("Unknown host")
    return web.json_response({"active_host": core.active_host})


async def state(request: web.Request) -> web.Response:
    return web.json_response(_core(request).state())


def setup(app: web.Application) -> None:
    r = app.router
    r.add_get("/v1/state", state)
    r.add_get("/v1/approvals", pending)
    r.add_post("/v1/approvals/{request_id}", decide)
    r.add_get("/v1/screenshots/{id:\\d+}.jpg", screenshot)
    r.add_post("/v1/hosts/{host_id}/nudge", nudge)
    r.add_post("/v1/agent/kill-all", kill_all)
    r.add_post("/v1/agent/pause", pause)
    r.add_post("/v1/agent/resume", resume)
    r.add_put("/v1/agent/active-host", active_host)
