"""aiohttp application for the control plane.

Binds to loopback only. In production the single path in is
`tailscale serve --bg --https=443 http://127.0.0.1:8421`; a different bind
address exists only behind `--mock --dev-bind` for emulator testing.
"""

from __future__ import annotations

import json
from typing import Any

from aiohttp import web

from lapis_daemon.agent.runner import Busy
from lapis_daemon.api import routes_approvals, routes_chats, routes_config, routes_device
from lapis_daemon.api.ws_hub import WsHub
from lapis_daemon.config import API_BIND_HOST

CORE = web.AppKey("core", object)
HUB = web.AppKey("hub", WsHub)
DEVICE = web.RequestKey("lapis_device", dict)
# The one unauthenticated route: it is how a token is obtained in the first place.
PUBLIC = {("POST", "/v1/pair/exchange")}
MAX_BODY = 2 * 1024 * 1024


def error(status: int, code: str, message: str) -> web.Response:
    return web.json_response({"error": {"code": code, "message": message}}, status=status)


def bearer(request: web.Request) -> str:
    scheme, _, token = request.headers.get("Authorization", "").partition(" ")
    return token.strip() if scheme.lower() == "bearer" else ""


@web.middleware
async def auth_middleware(request: web.Request, handler: Any) -> web.StreamResponse:
    if (request.method, request.path) not in PUBLIC:
        device = await request.app[CORE].auth.verify(bearer(request))  # type: ignore[attr-defined]
        if device is None:
            return error(401, "unauthorized", "Missing, invalid or revoked bearer token")
        request[DEVICE] = device
    return await handler(request)


@web.middleware
async def error_middleware(request: web.Request, handler: Any) -> web.StreamResponse:
    try:
        return await handler(request)
    except web.HTTPException as exc:
        if exc.status < 400:
            raise
        return error(exc.status, exc.reason.lower().replace(" ", "_"), exc.text or exc.reason)
    except json.JSONDecodeError:
        return error(400, "bad_json", "Request body is not valid JSON")
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
    app = web.Application(middlewares=[error_middleware, auth_middleware], client_max_size=MAX_BODY)
    app[CORE] = core
    hub = app[HUB] = WsHub(core)
    app.router.add_get("/v1/ws", hub.handle)
    for module in (routes_chats, routes_approvals, routes_config, routes_device):
        module.setup(app)
    app.on_shutdown.append(lambda _app: hub.close())
    return app


class ApiServer:
    def __init__(self, core: Any, port: int, host: str = API_BIND_HOST) -> None:
        self.core = core
        self.host = host
        self.port = port
        self.app = build_app(core)
        self._runner: web.AppRunner | None = None

    async def start(self) -> None:
        self._runner = web.AppRunner(self.app, access_log=None)
        await self._runner.setup()
        await web.TCPSite(self._runner, self.host, self.port).start()
        self.app[HUB].start()

    async def stop(self) -> None:
        if self._runner is not None:
            await self._runner.cleanup()
            self._runner = None
