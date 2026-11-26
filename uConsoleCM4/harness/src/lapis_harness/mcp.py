"""`lapis-mcp`: the cyberdeck's tools as a Model Context Protocol server over stdio.

JSON-RPC 2.0, one message per line (MCP's stdio transport), hand-rolled: `initialize`,
`notifications/initialized`, `ping`, `tools/list`, `tools/call`. Every tool is the
daemon's: the list is `GET /v1/deck/tools` and a call is `POST /v1/deck/tools/invoke`
with `context.source = "mcp"`, which puts it to the Policy Gate and, at Tier 1/2, waits
for the owner. Nothing here runs a tool itself.

Tool names contain dots (`sdr.scan_band`); many MCP clients accept `[a-zA-Z0-9_-]` only,
so `.` is offered as `__` and mapped back (a dotted name is accepted as well). A failed
or refused call is a tool result with `isError: true`, not a protocol error.

    lapis-mcp [--daemon-url URL] [--token-file PATH] [--mock]
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

from lapis_harness import __version__
from lapis_harness.config import DEFAULT_DAEMON_URL, DEFAULT_TOKEN_FILE, mock_dir
from lapis_harness.daemon_client import DaemonClient, DaemonUnavailable, check_daemon_url
from lapis_harness.redact import redact

PROTOCOL_VERSIONS = ("2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05")
SERVER_INFO = {"name": "lapis-deck", "title": "Lapis-Deck cyberdeck tools", "version": __version__}
INSTRUCTIONS = (
    "Tools for the ClockworkPi uConsole cyberdeck's own hardware: SDR, LoRa mesh, GPS, "
    "passive network surveys, power rails and a local shell. Every call passes the device's "
    "Policy Gate; Tier 1 and 2 calls wait for the owner's approval on the device or phone."
)
SEP = "__"

PARSE_ERROR = -32700
INVALID_REQUEST = -32600
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602
INTERNAL_ERROR = -32603


def mcp_name(name: str) -> str:
    return name.replace(".", SEP)


def deck_name(name: str) -> str:
    return name if "." in name else name.replace(SEP, ".")


class RpcError(Exception):
    def __init__(self, code: int, message: str) -> None:
        super().__init__(message)
        self.code = code


class McpServer:
    """The protocol, transport aside: `handle` takes one decoded message."""

    def __init__(self, client: DaemonClient) -> None:
        self.client = client
        self.initialized = False

    async def handle(self, message: Any) -> Any:
        """The response to one message (a list for a batch), or None for a notification."""
        if isinstance(message, list):
            if not message:
                return _error(None, INVALID_REQUEST, "Empty batch")
            replies = [await self._one(m) for m in message]
            return [r for r in replies if r is not None] or None
        return await self._one(message)

    async def _one(self, message: Any) -> dict[str, Any] | None:
        if not isinstance(message, dict) or message.get("jsonrpc") != "2.0":
            return _error(None, INVALID_REQUEST, "Not a JSON-RPC 2.0 message")
        method = message.get("method")
        msg_id = message.get("id")
        if not isinstance(method, str):
            # A response to us (we send no requests) or nonsense: nothing to answer.
            return (
                None
                if "result" in message or "error" in message
                else _error(msg_id, INVALID_REQUEST, "Missing method")
            )
        notification = "id" not in message
        params = message.get("params") or {}
        handler: Callable[[dict[str, Any]], Awaitable[dict[str, Any]]] | None = {
            "initialize": self._initialize,
            "ping": self._ping,
            "tools/list": self._tools_list,
            "tools/call": self._tools_call,
        }.get(method)
        if notification:
            if method == "notifications/initialized":
                self.initialized = True
            return None  # notifications (cancelled, progress, ...) need no answer
        if handler is None:
            return _error(msg_id, METHOD_NOT_FOUND, f"Method not found: {method}")
        if not isinstance(params, dict):
            return _error(msg_id, INVALID_PARAMS, "params must be an object")
        try:
            return {"jsonrpc": "2.0", "id": msg_id, "result": await handler(params)}
        except RpcError as exc:
            return _error(msg_id, exc.code, str(exc))
        except Exception as exc:  # noqa: BLE001 - one bad call keeps the server
            return _error(msg_id, INTERNAL_ERROR, redact(f"{type(exc).__name__}: {exc}")[:300])

    async def _initialize(self, params: dict[str, Any]) -> dict[str, Any]:
        asked = params.get("protocolVersion")
        version = asked if asked in PROTOCOL_VERSIONS else PROTOCOL_VERSIONS[0]
        return {
            "protocolVersion": version,
            "capabilities": {"tools": {"listChanged": False}},
            "serverInfo": SERVER_INFO,
            "instructions": INSTRUCTIONS,
        }

    async def _ping(self, params: dict[str, Any]) -> dict[str, Any]:
        return {}

    async def _tools_list(self, params: dict[str, Any]) -> dict[str, Any]:
        try:
            tools = await self.client.deck_tools()
        except DaemonUnavailable as exc:
            raise RpcError(INTERNAL_ERROR, str(exc)) from None
        listed = []
        for tool in tools:
            tier = tool.get("tier")
            schema = tool.get("input_schema")
            note = {0: "passive, runs at once", 1: "needs a [Y/N]", 2: "needs hold-to-confirm"}
            how = note.get(tier, "gated") if isinstance(tier, int) else "gated"
            listed.append(
                {
                    "name": mcp_name(str(tool["name"])),
                    "title": str(tool["name"]),
                    "description": f"{tool.get('description') or ''} (Tier {tier}: {how})".strip(),
                    "inputSchema": schema
                    if isinstance(schema, dict)
                    else {"type": "object", "properties": {}},
                    "annotations": {
                        "readOnlyHint": tier == 0,
                        "destructiveHint": tier == 2,
                        "openWorldHint": False,
                    },
                    "_meta": {"lapis/tier": tier, "lapis/name": tool["name"]},
                }
            )
        return {"tools": listed}

    async def _tools_call(self, params: dict[str, Any]) -> dict[str, Any]:
        name = params.get("name")
        args = params.get("arguments") or {}
        if not isinstance(name, str) or not name:
            raise RpcError(INVALID_PARAMS, "tools/call needs a tool name")
        if not isinstance(args, dict):
            raise RpcError(INVALID_PARAMS, "arguments must be an object")
        reply = await self.client.invoke(deck_name(name), args, {"source": "mcp"})
        if reply.get("ok") is True:
            result = reply.get("result")
            text = json.dumps(result, indent=1, default=str)
            out: dict[str, Any] = {"content": [{"type": "text", "text": text}], "isError": False}
            if isinstance(result, dict):
                out["structuredContent"] = result
            return out
        decision = str(reply.get("decision") or "error")
        error = str(reply.get("error") or decision)
        return {
            "content": [{"type": "text", "text": f"{decision}: {error}"}],
            "isError": True,
            "_meta": {"lapis/decision": decision, "lapis/tier": reply.get("tier")},
        }


def _error(msg_id: Any, code: int, message: str) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": msg_id, "error": {"code": code, "message": message}}


async def serve(
    server: McpServer,
    read_line: Callable[[], Awaitable[bytes]],
    write: Callable[[bytes], None],
) -> None:
    """Read newline-delimited JSON-RPC until EOF; answer each line in order of arrival.

    Calls run concurrently (a Tier 2 call may wait minutes for the owner while the client
    pings), so answers to different requests may come back in another order.
    """
    pending: set[asyncio.Task[None]] = set()

    async def answer(message: Any) -> None:
        reply = await server.handle(message)
        if reply is not None:
            write(json.dumps(reply, separators=(",", ":")).encode() + b"\n")

    while line := await read_line():
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except (ValueError, RecursionError):
            write(json.dumps(_error(None, PARSE_ERROR, "Parse error")).encode() + b"\n")
            continue
        task = asyncio.create_task(answer(message))
        pending.add(task)
        task.add_done_callback(pending.discard)
    if pending:
        await asyncio.gather(*pending, return_exceptions=True)


def main(argv: list[str] | None = None) -> int:
    mock = mock_dir()
    parser = argparse.ArgumentParser(
        prog="lapis-mcp", description="MCP (stdio) server for the Lapis-Deck cyberdeck tools."
    )
    parser.add_argument("--daemon-url", default=DEFAULT_DAEMON_URL)
    parser.add_argument("--token-file", default=None)
    parser.add_argument("--mock", action="store_true", help=f"token at {mock}/local.token")
    args = parser.parse_args(argv)
    try:
        check_daemon_url(args.daemon_url)
    except ValueError as exc:
        parser.error(str(exc))
    token = Path(args.token_file or (mock / "local.token" if args.mock else DEFAULT_TOKEN_FILE))
    client = DaemonClient(args.daemon_url, token)
    stdout = sys.stdout.buffer

    def write(data: bytes) -> None:
        stdout.write(data)
        stdout.flush()

    async def run() -> None:
        try:
            await serve(McpServer(client), lambda: asyncio.to_thread(sys.stdin.buffer.readline),
                        write)  # fmt: skip
        finally:
            await client.close()

    try:
        asyncio.run(run())
    except KeyboardInterrupt:
        return 130
    return 0


if __name__ == "__main__":
    sys.exit(main())
