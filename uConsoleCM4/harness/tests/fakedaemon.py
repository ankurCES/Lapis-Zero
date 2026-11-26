"""A stand-in for lapis-daemon's control plane: the three routes the harness calls.

`POST /v1/policy/authorize`, `GET /v1/deck/tools` and `POST /v1/deck/tools/invoke`,
on 127.0.0.1 with a bearer token, answering through handlers a test sets, and recording
every request (body and Authorization header).
"""

from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

from aiohttp import web

from lapis_harness.daemon_client import DaemonClient

TOKEN = "local-token-for-tests-0123456789abcdef"
TOOLS = [
    {
        "name": "gps.get_fix",
        "description": "Position, altitude and fix quality from gpsd.",
        "tier": 0,
        "input_schema": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "sdr.scan_band",
        "description": "Sweep a band and report the strongest peaks.",
        "tier": 0,
        "input_schema": {
            "type": "object",
            "properties": {
                "start_hz": {"type": "integer"},
                "end_hz": {"type": "integer"},
                "step_hz": {"type": "integer"},
            },
            "required": ["start_hz", "end_hz"],
        },
    },
    {
        "name": "mesh.list_nodes",
        "description": "Meshtastic nodes heard, with SNR and hops.",
        "tier": 0,
        "input_schema": {"type": "object", "properties": {}},
    },
    {
        "name": "mesh.send_message",
        "description": "Send a LoRa text message.",
        "tier": 1,
        "input_schema": {"type": "object", "properties": {"text": {"type": "string"}}},
    },
    {
        "name": "hw.gpio_rail",
        "description": "Switch a power rail.",
        "tier": 2,
        "input_schema": {"type": "object", "properties": {"name": {"type": "string"}}},
    },
]
RESULTS: dict[str, Any] = {
    "gps.get_fix": {"fix": {"fix": "3D", "lat": 37.77, "lon": -122.42}, "pps_locked": True},
    "sdr.scan_band": {"peaks": [{"freq_hz": 100_100_000, "power_db": -21.5}]},
    "mesh.list_nodes": {"nodes": [{"id": "!a1b2", "snr": 7.5}]},
    "mesh.send_message": {"sent": True},
    "hw.gpio_rail": {"rail": "gps", "on": False},
}

Handler = Callable[[dict[str, Any]], Awaitable[Any]]


async def allow_all(body: dict[str, Any]) -> dict[str, Any]:
    return {"allowed": True, "decision": "auto", "tier": 0, "reason": "test", "by": "policy",
            "approval_token": None}  # fmt: skip


async def run_tools(body: dict[str, Any]) -> dict[str, Any]:
    tool = body["tool"]
    if tool not in RESULTS:
        return {"ok": False, "decision": "policy_deny", "error": f"{tool} is not a cyberdeck tool"}
    return {"ok": True, "tier": 0, "decision": "auto", "result": RESULTS[tool]}


class FakeDaemon:
    def __init__(self, tmp: Path) -> None:
        self.token_file = tmp / "local.token"
        self.token_file.write_text(TOKEN + "\n")
        self.authorize: Handler = allow_all
        self.invoke: Handler = run_tools
        self.tools: list[dict[str, Any]] = list(TOOLS)
        self.asked: list[dict[str, Any]] = []  # authorize bodies
        self.invoked: list[dict[str, Any]] = []  # invoke bodies
        self.auth_headers: list[str] = []
        self.status = 200  # answer every route with this status (500: a broken daemon)
        self._runner: web.AppRunner | None = None
        self.url = ""

    def _check(self, request: web.Request) -> web.Response | None:
        self.auth_headers.append(request.headers.get("Authorization", ""))
        if request.headers.get("Authorization") != f"Bearer {TOKEN}":
            return web.json_response({"error": {"code": "unauthorized"}}, status=401)
        if self.status != 200:
            return web.json_response(
                {"error": {"code": "boom", "message": "x"}}, status=self.status
            )
        return None

    async def _authorize(self, request: web.Request) -> web.Response:
        if (refused := self._check(request)) is not None:
            return refused
        body = await request.json()
        self.asked.append(body)
        return web.json_response(await self.authorize(body))

    async def _tools(self, request: web.Request) -> web.Response:
        if (refused := self._check(request)) is not None:
            return refused
        return web.json_response({"tools": self.tools})

    async def _invoke(self, request: web.Request) -> web.Response:
        if (refused := self._check(request)) is not None:
            return refused
        body = await request.json()
        self.invoked.append(body)
        return web.json_response(await self.invoke(body))

    async def __aenter__(self) -> FakeDaemon:
        app = web.Application()
        app.router.add_post("/v1/policy/authorize", self._authorize)
        app.router.add_get("/v1/deck/tools", self._tools)
        app.router.add_post("/v1/deck/tools/invoke", self._invoke)
        self._runner = web.AppRunner(app, access_log=None, shutdown_timeout=0.5)
        await self._runner.setup()
        site = web.TCPSite(self._runner, "127.0.0.1", 0)
        await site.start()
        port = site._server.sockets[0].getsockname()[1]  # type: ignore[union-attr]
        self.url = f"http://127.0.0.1:{port}"
        return self

    async def __aexit__(self, *exc: object) -> None:
        if self._runner is not None:
            await self._runner.cleanup()

    def client(self) -> DaemonClient:
        return DaemonClient(self.url, self.token_file)


async def held(release: asyncio.Event, answer: dict[str, Any]) -> dict[str, Any]:
    """An answer that comes once `release` is set: an owner taking their time."""
    await release.wait()
    return answer


def closed_port_url() -> str:
    """A loopback URL nothing listens on."""
    import socket

    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return f"http://127.0.0.1:{probe.getsockname()[1]}"
