"""`lapis-mcp`: the MCP stdio server, end to end against a fake daemon."""

from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path
from typing import Any

from fakedaemon import FakeDaemon, closed_port_url, held

from lapis_harness.daemon_client import DaemonClient
from lapis_harness.mcp import McpServer, deck_name, mcp_name, serve


def rpc(msg_id: int | None, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
    msg: dict[str, Any] = {"jsonrpc": "2.0", "method": method}
    if msg_id is not None:
        msg["id"] = msg_id
    if params is not None:
        msg["params"] = params
    return msg


async def test_the_protocol_over_stdio_in_a_process_of_its_own(tmp_path: Path) -> None:
    async with FakeDaemon(tmp_path) as daemon:
        proc = await asyncio.create_subprocess_exec(
            sys.executable, "-m", "lapis_harness.mcp", "--daemon-url", daemon.url,
            "--token-file", str(daemon.token_file),
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )  # fmt: skip
        assert proc.stdin is not None and proc.stdout is not None

        async def ask(msg: dict[str, Any] | str) -> dict[str, Any]:
            line = msg if isinstance(msg, str) else json.dumps(msg)
            proc.stdin.write(line.encode() + b"\n")  # type: ignore[union-attr]
            await proc.stdin.drain()  # type: ignore[union-attr]
            return json.loads(await asyncio.wait_for(proc.stdout.readline(), 10))  # type: ignore[union-attr]

        hello = {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t"}}
        init = await ask(rpc(1, "initialize", hello))
        result = init["result"]
        assert init["id"] == 1 and result["protocolVersion"] == "2025-06-18"
        assert result["capabilities"] == {"tools": {"listChanged": False}}
        assert result["serverInfo"]["name"] == "lapis-deck"
        # A notification has no answer: the next line is the ping's.
        proc.stdin.write(json.dumps(rpc(None, "notifications/initialized")).encode() + b"\n")
        assert await ask(rpc(2, "ping")) == {"jsonrpc": "2.0", "id": 2, "result": {}}

        listed = (await ask(rpc(3, "tools/list")))["result"]["tools"]
        names = [t["name"] for t in listed]
        assert names == ["gps__get_fix", "sdr__scan_band", "mesh__list_nodes",
                         "mesh__send_message", "hw__gpio_rail"]  # fmt: skip
        scan = listed[1]
        assert scan["title"] == "sdr.scan_band" and scan["inputSchema"]["required"] == [
            "start_hz",
            "end_hz",
        ]
        assert "Tier 0" in scan["description"] and scan["annotations"]["readOnlyHint"] is True
        assert listed[4]["annotations"]["destructiveHint"] is True

        args = {"start_hz": 99_000_000, "end_hz": 101_000_000}
        call = await ask(rpc(4, "tools/call", {"name": "sdr__scan_band", "arguments": args}))
        out = call["result"]
        assert out["isError"] is False and out["structuredContent"]["peaks"][0]["freq_hz"] == (
            100_100_000
        )
        assert json.loads(out["content"][0]["text"]) == out["structuredContent"]
        assert daemon.invoked[-1] == {"tool": "sdr.scan_band", "args": args,
                                      "context": {"source": "mcp"}}  # fmt: skip

        # Refused at the gate: a tool result marked as an error, not a protocol error.
        async def refuse(body: dict[str, Any]) -> dict[str, Any]:
            return {"ok": False, "tier": 2, "decision": "deny", "error": "the owner said no"}

        daemon.invoke = refuse
        denied = await ask(rpc(5, "tools/call", {"name": "hw.gpio_rail", "arguments": {}}))
        assert denied["result"]["isError"] is True
        assert denied["result"]["content"][0]["text"] == "deny: the owner said no"
        assert daemon.invoked[-1]["tool"] == "hw.gpio_rail"  # a dotted name works too

        assert (await ask(rpc(6, "resources/list")))["error"]["code"] == -32601
        assert (await ask("{not json"))["error"]["code"] == -32700
        bad = await ask(rpc(7, "tools/call", {"arguments": {}}))
        assert bad["error"]["code"] == -32602

        proc.stdin.close()
        assert await asyncio.wait_for(proc.wait(), 10) == 0
        assert b"Traceback" not in await proc.stderr.read()  # type: ignore[union-attr]


async def test_calls_run_side_by_side_and_a_daemon_that_is_gone_is_an_error(
    tmp_path: Path,
) -> None:
    async with FakeDaemon(tmp_path) as daemon:
        release = asyncio.Event()
        daemon.invoke = lambda body: held(
            release, {"ok": True, "tier": 1, "decision": "approve_once", "result": {"sent": 1}}
        )
        server = McpServer(daemon.client())
        lines: asyncio.Queue[bytes] = asyncio.Queue()
        out: list[dict[str, Any]] = []
        for msg in (
            rpc(1, "tools/call", {"name": "mesh__send_message", "arguments": {"text": "hi"}}),
            rpc(2, "ping"),
        ):
            lines.put_nowait(json.dumps(msg).encode() + b"\n")

        async def read_line() -> bytes:
            return await lines.get()

        task = asyncio.create_task(serve(server, read_line, lambda b: out.append(json.loads(b))))
        # The ping is answered while the Tier 1 call waits for the owner.
        while not out:
            await asyncio.sleep(0.01)
        assert out == [{"jsonrpc": "2.0", "id": 2, "result": {}}]
        release.set()
        lines.put_nowait(b"")  # EOF: the server finishes what it has, then returns
        await asyncio.wait_for(task, 5)
        assert out[1]["id"] == 1 and out[1]["result"]["structuredContent"] == {"sent": 1}
        await server.client.close()

    gone = McpServer(DaemonClient(closed_port_url(), tmp_path / "local.token"))
    call = await gone.handle(rpc(1, "tools/call", {"name": "gps__get_fix"}))
    assert (
        call["result"]["isError"] is True and "unreachable" in call["result"]["content"][0]["text"]
    )
    listing = await gone.handle(rpc(2, "tools/list"))
    assert listing["error"]["code"] == -32603
    assert await gone.handle([rpc(3, "ping"), rpc(None, "notifications/cancelled")]) == [
        {"jsonrpc": "2.0", "id": 3, "result": {}}
    ]
    await gone.client.close()
    assert (mcp_name("sdr.scan_band"), deck_name("sdr__scan_band")) == (
        "sdr__scan_band",
        "sdr.scan_band",
    )
