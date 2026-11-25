"""Authenticated WebSocket fan-out (`/v1/ws`) with heartbeats and telemetry ticks."""

from __future__ import annotations

import asyncio
import contextlib
import json
import time
from typing import Any

from aiohttp import WSMsgType, web

# Frames that only make sense on the device's own socket.
SKIP = frozenset({"mic_level", "screen_request", "ui_settings"})
QUEUE_MAX = 256
TELEMETRY_FAST_SEC = 5
TELEMETRY_SLOW_SEC = 30


def _device(request: web.Request) -> dict[str, Any]:
    from lapis_daemon.api.server import DEVICE

    return request[DEVICE]


class _Client:
    def __init__(self, ws: web.WebSocketResponse, device: dict[str, Any]) -> None:
        self.ws = ws
        self.device = device
        self.queue: asyncio.Queue[str] = asyncio.Queue(QUEUE_MAX)
        self.telemetry = False


class WsHub:
    def __init__(self, core: Any) -> None:
        self.core = core
        self.clients: set[_Client] = set()
        self._ticker: asyncio.Task[None] | None = None
        core.add_sink(self.broadcast, SKIP)

    def start(self) -> None:
        if self._ticker is None:
            self._ticker = asyncio.create_task(self._telemetry_loop())

    async def close(self) -> None:
        if self._ticker is not None:
            self._ticker.cancel()
            self._ticker = None
        for client in list(self.clients):
            await client.ws.close()

    def broadcast(self, msg: dict[str, Any]) -> None:
        if not self.clients:
            return
        data = json.dumps(msg)
        for client in list(self.clients):
            try:
                client.queue.put_nowait(data)
            except asyncio.QueueFull:  # a stalled phone must not grow memory on the device
                self.clients.discard(client)
                asyncio.create_task(client.ws.close())

    async def _telemetry_loop(self) -> None:
        elapsed = 0
        while True:
            await asyncio.sleep(TELEMETRY_FAST_SEC)
            elapsed += TELEMETRY_FAST_SEC
            fast = [c for c in self.clients if c.telemetry]
            slow_due = elapsed % TELEMETRY_SLOW_SEC == 0
            targets = list(self.clients) if slow_due else fast
            if not targets:
                continue
            with contextlib.suppress(Exception):
                await self.core.status.refresh()
                tick = json.dumps(
                    {"type": "telemetry_tick", "telemetry": await self.core.status.telemetry()}
                )
                for client in targets:
                    with contextlib.suppress(asyncio.QueueFull):
                        client.queue.put_nowait(tick)

    async def handle(self, request: web.Request) -> web.StreamResponse:
        # The auth middleware has already rejected the upgrade without a valid token.
        ws = web.WebSocketResponse(heartbeat=20, max_msg_size=256 * 1024)
        await ws.prepare(request)
        client = _Client(ws, _device(request))
        await ws.send_json(self.core.state())  # full snapshot first, then live events
        self.clients.add(client)
        writer = asyncio.create_task(self._writer(client))
        try:
            async for raw in ws:
                if raw.type != WSMsgType.TEXT:
                    continue
                try:
                    await self._dispatch(client, json.loads(raw.data))
                except Exception as exc:  # noqa: BLE001 - one bad command keeps the socket
                    await ws.send_json({"type": "toast", "level": "error", "text": str(exc)[:200]})
        finally:
            self.clients.discard(client)
            writer.cancel()
        return ws

    @staticmethod
    async def _writer(client: _Client) -> None:
        with contextlib.suppress(ConnectionError, asyncio.CancelledError):
            while True:
                await client.ws.send_str(await client.queue.get())

    async def _dispatch(self, client: _Client, msg: Any) -> None:
        by = f"android:{client.device['name']}"
        match msg:
            case {"type": "ping"}:
                await client.ws.send_json({"type": "pong", "ts": int(time.time())})
            case {"type": "subscribe_telemetry", "enabled": bool(enabled)}:
                client.telemetry = enabled
            case {"type": "send_prompt", "chat_id": str(chat_id), "content": str(content)}:
                await self.core.submit_prompt(content, chat_id=chat_id, source=by)
            case {
                "type": "approval_decision",
                "request_id": str(request_id),
                "decision": "approve_once" | "approve_session" | "deny" as decision,
            }:
                self.core.decide(request_id, decision, by, str(msg.get("reason", "")))
            case {"type": "kill_all"}:
                await self.core.kill_all(by)
            case _:
                raise ValueError("Unrecognized command")
