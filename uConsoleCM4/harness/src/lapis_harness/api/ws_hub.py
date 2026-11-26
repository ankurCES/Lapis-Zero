"""The harness's event stream (`/v1/ws`): a full `state_sync`, then every event.

Its one client in production is lapis-daemon, which reads this stream, lays its own
fields over each `state_sync`, routes HUD-only frames (`mic_level`) to the HUD, and fans
the rest out to every authenticated client. Commands in Lapis-Zero's vocabulary are still
understood here (a local tool on the socket, tests); the daemon handles its clients' own.
Every frame was redacted by `Core.publish`; the first snapshot is redacted here.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import time
from typing import Any

from aiohttp import WSMsgType, web

from lapis_harness.redact import redact_json

QUEUE_MAX = 512


class _Client:
    def __init__(self, ws: web.WebSocketResponse, device: dict[str, Any]) -> None:
        self.ws = ws
        self.device = device
        self.queue: asyncio.Queue[str] = asyncio.Queue(QUEUE_MAX)


class WsHub:
    def __init__(self, core: Any) -> None:
        self.core = core
        self.clients: set[_Client] = set()
        core.add_sink(self.broadcast)

    async def close(self) -> None:
        for client in list(self.clients):
            await client.ws.close()

    def broadcast(self, msg: dict[str, Any]) -> None:
        if not self.clients:
            return
        data = json.dumps(msg)
        for client in list(self.clients):
            try:
                client.queue.put_nowait(data)
            except asyncio.QueueFull:  # a stalled reader must not grow memory on the device
                self.clients.discard(client)
                asyncio.create_task(client.ws.close())

    async def handle(self, request: web.Request) -> web.StreamResponse:
        from lapis_harness.api.server import device

        ws = web.WebSocketResponse(heartbeat=20, max_msg_size=256 * 1024)
        await ws.prepare(request)
        client = _Client(ws, device(request))
        await ws.send_json(redact_json(self.core.state()))  # full snapshot first, then events
        self.clients.add(client)
        writer = asyncio.create_task(self._writer(client))
        try:
            async for raw in ws:
                if raw.type != WSMsgType.TEXT:
                    continue
                try:
                    await self._dispatch(client, json.loads(raw.data))
                except Exception as exc:  # noqa: BLE001 - one bad command keeps the socket
                    text = redact_json(str(exc)[:200])
                    await ws.send_json({"type": "toast", "level": "error", "text": text})
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
        me = client.device
        by = f"android:{me['name']}" if me["kind"] == "phone" else "device"
        match msg:
            case {"type": "ping"}:
                await client.ws.send_json({"type": "pong", "ts": int(time.time())})
            case {"type": "subscribe_telemetry"}:
                pass  # telemetry is the daemon's (it reads the hardware)
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
