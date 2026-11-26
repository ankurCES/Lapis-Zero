"""The harness behind the real Rust `lapis-daemon` (`--mock-hw`), over their real sockets.

Skipped when the daemon has not been built (`cargo build -p lapis-daemon` in the repo root).
"""

from __future__ import annotations

import asyncio
import contextlib
import socket
import subprocess
from collections.abc import AsyncIterator
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import aiohttp
import pytest
from conftest import make_core, wait_for

from lapis_harness.api.server import ApiServer
from lapis_harness.core import Core
from lapis_harness.daemon_client import DaemonClient
from lapis_harness.deck import DaemonDeckTools
from lapis_harness.gate import ActionRequest, DaemonGate
from lapis_harness.mcp import McpServer

BINARY = Path(__file__).resolve().parents[2] / "target" / "debug" / "lapis-daemon"
pytestmark = pytest.mark.skipif(
    not BINARY.is_file(), reason="lapis-daemon is not built (cargo build -p lapis-daemon)"
)


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


@dataclass
class Stack:
    core: Core
    url: str
    token: str
    http: aiohttp.ClientSession
    client: DaemonClient

    async def call(self, method: str, path: str, body: Any = None) -> tuple[int, Any]:
        headers = {"Authorization": f"Bearer {self.token}"}
        async with self.http.request(method, self.url + path, json=body, headers=headers) as r:
            return r.status, await r.json(content_type=None)


@contextlib.asynccontextmanager
async def stack(tmp: Path) -> AsyncIterator[Stack]:
    port = free_port()
    sock = tmp / "h.sock"
    log = (tmp / "daemon.log").open("w")
    proc = subprocess.Popen(
        [str(BINARY), "--mock-hw", "--state-dir", str(tmp / "state"), "--harness-socket",
         str(sock), "--port", str(port), "--config", str(tmp / "none.toml")],
        stdout=log, stderr=subprocess.STDOUT,
    )  # fmt: skip
    token_file = tmp / "state" / "local.token"
    url = f"http://127.0.0.1:{port}"
    client = DaemonClient(url, token_file)
    core = await make_core(tmp, gate=DaemonGate(client), deck_tools=DaemonDeckTools(client))
    api = ApiServer(core, sock)
    http = aiohttp.ClientSession()
    try:
        await wait_for(token_file.exists, 20)
        await api.start()
        s = Stack(core, url, token_file.read_text().strip(), http, client)

        async def harness_up() -> bool:
            with contextlib.suppress(aiohttp.ClientError):
                return (await s.call("GET", "/v1/health"))[1].get("harness") is True
            return False

        async with asyncio.timeout(20):
            while not await harness_up():
                await asyncio.sleep(0.1)
        yield s
    finally:
        await http.close()
        await api.stop()
        await core.stop()
        await client.close()
        proc.terminate()
        proc.wait(10)
        log.close()


async def test_the_gate_round_trip_and_the_api_through_the_daemon(short_tmp: Path) -> None:
    async with stack(short_tmp) as s:
        gate = s.core.gate
        # Read-only: Tier 0 at once. The denylist: refused outright.
        ls = await gate.authorize(
            ActionRequest("ssh_exec", "arch-dev", command="ls -la", host_policy="strict")
        )
        assert (ls.allowed, ls.decision, ls.tier) == (True, "auto", 0)
        wipe = await gate.authorize(
            ActionRequest("ssh_exec", "arch-dev", command="rm -rf /", host_policy="autonomous")
        )
        assert (wipe.allowed, wipe.decision) == (False, "policy_deny")
        # Tier 1: held at the daemon until the owner answers there.
        waited: list[bool] = []
        ask = asyncio.create_task(
            gate.authorize(
                ActionRequest(
                    "ssh_exec",
                    "arch-dev",
                    command="make deploy",
                    summary="make deploy",
                    host_policy="strict",
                    task_id="t_it",
                ),
                lambda: waited.append(True),
            )  # fmt: skip
        )
        async with asyncio.timeout(10):
            while not (pending := (await s.call("GET", "/v1/approvals"))[1]["approvals"]):
                await asyncio.sleep(0.05)
        assert (pending[0]["action"], pending[0]["tier"], pending[0]["source"]) == (
            "ssh_exec",
            1,
            "harness",
        )
        await wait_for(lambda: gate.pending())  # it counts as waiting after a moment
        assert gate.pending()[0]["summary"] == "make deploy" and waited == [True]
        rid = pending[0]["request_id"]
        assert (await s.call("POST", f"/v1/approvals/{rid}", {"decision": "approve_once"}))[
            0
        ] == 200
        out = await asyncio.wait_for(ask, 10)
        assert (out.allowed, out.decision, out.by) == (True, "approve_once", "device")
        assert out.approval_token

        # The Lapis-Zero API reaches the harness through the daemon.
        status, chat = await s.call("POST", "/v1/chats", {"host_id": "macbook-pro"})
        assert status == 201 and chat["host_id"] == "macbook-pro"
        status, chats = await s.call("GET", "/v1/chats")
        assert status == 200 and [c["id"] for c in chats["chats"]] == [chat["id"]]
        status, hosts = await s.call("GET", "/v1/hosts")
        assert list(hosts["hosts"]) == ["macbook-pro", "arch-dev", "deck"]
        status, state = await s.call("GET", "/v1/state")
        assert state["hosts"][-1] == "deck" and state["device"]["kind"] == "lapis-deck"


async def test_a_deck_chat_and_mcp_against_the_daemons_own_tools(short_tmp: Path) -> None:
    async with stack(short_tmp) as s:
        tools = await s.core.deck_tools.list_tools()  # type: ignore[union-attr]
        assert len(tools) >= 24 and {"gps.get_fix", "sdr.scan_band"} <= {t["name"] for t in tools}
        status, chat = await s.call("POST", "/v1/chats", {"host_id": "deck"})
        assert status == 201
        status, _ = await s.call("POST", f"/v1/chats/{chat['id']}/messages", {"content": "Scan"})
        assert status == 202
        await wait_for(lambda: not s.core.runner.is_running, 20)
        await s.core._chat_q.join()
        _, thread = await s.call("GET", f"/v1/chats/{chat['id']}")
        tool_rows = [m for m in thread["messages"] if m["role"] == "tool"]
        assert [m["metadata"]["status"] for m in tool_rows] == ["ok", "ok", "ok"], tool_rows
        assert '"3D"' in tool_rows[0]["metadata"]["detail"]  # the mock GPS has a fix
        assert thread["messages"][-1]["content"] == "Mock run complete: 3 scripted steps."

        mcp = McpServer(DaemonClient(s.url, short_tmp / "state" / "local.token"))
        listed = await mcp.handle({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
        assert "gps__get_fix" in [t["name"] for t in listed["result"]["tools"]]
        call = await mcp.handle({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                                 "params": {"name": "gps__get_fix", "arguments": {}}})  # fmt: skip
        assert call["result"]["isError"] is False
        assert call["result"]["structuredContent"]["fix"]["fix"] == "3D"
        await mcp.client.close()


async def test_the_daemons_kill_switch_stops_a_task_waiting_for_its_gate(short_tmp: Path) -> None:
    async with stack(short_tmp) as s:
        status, chat = await s.call("POST", "/v1/chats", {"host_id": "arch-dev"})
        await s.call("POST", f"/v1/chats/{chat['id']}/messages", {"content": "do things"})
        # Strict host: uname runs (Tier 0), the click waits for a [Y/N] at the daemon.
        async with asyncio.timeout(15):
            while not (await s.call("GET", "/v1/approvals"))[1]["approvals"]:
                await asyncio.sleep(0.05)
        _, state = await s.call("GET", "/v1/state")
        assert state["agent_state"] == "WAITING_APPROVAL"
        assert (await s.call("POST", "/v1/agent/kill-all", {}))[0] == 200
        await wait_for(lambda: not s.core.runner.is_running, 10)
        _, state = await s.call("GET", "/v1/state")
        assert state["agent_state"] == "IDLE" and state["pending_approvals"] == []
        driver = s.core.runner.drivers["arch-dev"]
        assert [line.split(":")[0] for line in driver.result_lines()] == [  # type: ignore[attr-defined]
            "ssh_exec",
            "screenshot",
        ]
