"""aiohttp application for the harness API, served on a Unix socket behind lapis-daemon.

The Rust daemon owns 127.0.0.1:8421: it authenticates every caller (bearer tokens,
pairing), strips `Authorization`, and forwards what it does not serve itself here with the
caller's identity in three headers. The socket's permissions (0660, the daemon's group)
are the trust boundary, so the harness reads who is asking from those headers and never
sees a token. Without them (a local tool on the socket) the caller is the device itself.

The JSON shapes are Lapis-Zero's: the Android app talks to these routes unchanged.
"""

from __future__ import annotations

import contextlib
import json
import os
from pathlib import Path
from typing import Any

from aiohttp import web

from lapis_harness.agent.runner import Busy
from lapis_harness.api import (
    routes_approvals,
    routes_chats,
    routes_config,
    routes_device,
    routes_viewport,
)
from lapis_harness.api.ws_hub import WsHub
from lapis_harness.redact import redact_json

CORE = web.AppKey("core", object)
HUB = web.AppKey("hub", WsHub)
DEVICE = web.RequestKey("lapis_device", dict)
MAX_BODY = 2 * 1024 * 1024
SOCKET_MODE = 0o660

HEADER_ID = "x-lapis-device-id"
HEADER_NAME = "x-lapis-device-name"
HEADER_KIND = "x-lapis-device-kind"
LOCAL = {"id": "device", "name": "device", "kind": "local"}


def error(status: int, code: str, message: str) -> web.Response:
    return web.json_response({"error": {"code": code, "message": message}}, status=status)


def identity(request: web.Request) -> dict[str, str]:
    """Who is asking, as the daemon said: `{id, name, kind}`; the device itself if unsaid."""
    kind = request.headers.get(HEADER_KIND, "").strip().lower()
    device_id = request.headers.get(HEADER_ID, "").strip()[:64]
    name = request.headers.get(HEADER_NAME, "").strip()[:64]
    if kind == "phone":
        return {"id": device_id or "phone", "name": name or "Android", "kind": "phone"}
    if not (device_id or name):
        return dict(LOCAL)
    return {"id": device_id or "device", "name": name or "device", "kind": "local"}


def device(request: web.Request) -> dict[str, str]:
    return request[DEVICE]


def by(request: web.Request) -> str:
    """Lapis-Zero's `by`: `android:<name>` for a phone, `device` for the device itself."""
    me = device(request)
    return f"android:{me['name']}" if me["kind"] == "phone" else "device"


def source(request: web.Request) -> str:
    """The gate's `source` for an owner's own input: the HUD, or a phone."""
    return "android" if device(request)["kind"] == "phone" else "hud"


@web.middleware
async def identity_middleware(request: web.Request, handler: Any) -> web.StreamResponse:
    request[DEVICE] = identity(request)
    return await handler(request)  # type: ignore[no-any-return]


@web.middleware
async def redact_middleware(request: web.Request, handler: Any) -> web.StreamResponse:
    """Every JSON answer is redacted on its way out, like every event."""
    resp = await handler(request)
    if isinstance(resp, web.Response) and resp.content_type == "application/json":
        raw = resp.body
        if isinstance(raw, bytes | bytearray) and raw:
            with contextlib.suppress(ValueError, TypeError, RecursionError):
                resp.body = json.dumps(redact_json(json.loads(raw))).encode()
    return resp  # type: ignore[no-any-return]


@web.middleware
async def error_middleware(request: web.Request, handler: Any) -> web.StreamResponse:
    from lapis_harness.core import HostUnreachable, NoScreen

    try:
        return await handler(request)  # type: ignore[no-any-return]
    except web.HTTPException as exc:
        if exc.status < 400:
            raise
        return error(exc.status, exc.reason.lower().replace(" ", "_"), exc.text or exc.reason)
    except json.JSONDecodeError:
        return error(400, "bad_json", "Request body is not valid JSON")
    except HostUnreachable as exc:
        return error(503, "host_unreachable", str(exc))
    except NoScreen as exc:
        return error(409, "no_screen", str(exc))
    except LookupError as exc:
        return error(404, "not_found", str(exc).strip("\"'") or "Not found")
    except Busy as exc:
        return error(409, "busy", str(exc))
    except ValueError as exc:  # includes pydantic.ValidationError
        return error(422, "invalid", str(exc)[:500])
    except PermissionError as exc:
        return error(403, "forbidden", str(exc))


async def body(request: web.Request) -> dict[str, Any]:
    """JSON object body ({} when empty)."""
    if not request.can_read_body:
        return {}
    data = await request.json()
    if not isinstance(data, dict):
        raise ValueError("Request body must be a JSON object")
    return data


def build_app(core: Any) -> web.Application:
    app = web.Application(
        middlewares=[redact_middleware, error_middleware, identity_middleware],
        client_max_size=MAX_BODY,
    )
    app[CORE] = core
    hub = app[HUB] = WsHub(core)
    app.router.add_get("/v1/ws", hub.handle)
    for module in (routes_chats, routes_approvals, routes_config, routes_device, routes_viewport):
        module.setup(app)
    app.on_shutdown.append(lambda _app: hub.close())
    return app


class ApiServer:
    """The harness API on a Unix socket, mode 0660."""

    def __init__(self, core: Any, socket_path: str | Path) -> None:
        self.core = core
        self.socket_path = Path(socket_path)
        self.app = build_app(core)
        self._runner: web.AppRunner | None = None

    async def start(self) -> None:
        self.socket_path.parent.mkdir(parents=True, exist_ok=True)
        self.socket_path.unlink(missing_ok=True)  # stale socket from a previous run
        self._runner = web.AppRunner(self.app, access_log=None)
        await self._runner.setup()
        old = os.umask(0o117)  # created 0660: never wider, not even for a moment
        try:
            await web.UnixSite(self._runner, str(self.socket_path)).start()
        finally:
            os.umask(old)
        os.chmod(self.socket_path, SOCKET_MODE)

    async def stop(self) -> None:
        if self._runner is not None:
            await self._runner.cleanup()
            self._runner = None
        self.socket_path.unlink(missing_ok=True)
