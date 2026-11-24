from __future__ import annotations

import asyncio
import json
import stat
from pathlib import Path
from typing import Any

from lapis_daemon.agent.runner import TaskRunner
from lapis_daemon.config import mock_hosts
from lapis_daemon.db import LapisDb
from lapis_daemon.ipc_server import IpcServer
from lapis_daemon.system_status import mock_state


async def recv(reader: asyncio.StreamReader) -> dict[str, Any]:
    line = await asyncio.wait_for(reader.readline(), 2)  # re-sync budget (SPEC 6)
    assert line, "daemon closed the connection"
    return json.loads(line)


async def recv_until(reader: asyncio.StreamReader, msg_type: str) -> list[dict[str, Any]]:
    """Read until a message of `msg_type` arrives; returns everything read, it last."""
    seen = []
    while True:
        seen.append(await recv(reader))
        if seen[-1]["type"] == msg_type:
            return seen


def send(writer: asyncio.StreamWriter, **msg: Any) -> None:
    writer.write(json.dumps(msg).encode() + b"\n")


def test_full_task_flow_over_socket(short_tmp: Path) -> None:
    async def scenario() -> None:
        sock = short_tmp / "lapisd.sock"
        sock.write_text("stale")
        db = LapisDb(short_tmp / "lapis.db")
        await db.init()
        hosts = mock_hosts()
        runner = TaskRunner(db, hosts)
        server = IpcServer(sock, db, hosts, runner, mock_state)
        await server.start()
        assert stat.S_IMODE(sock.stat().st_mode) == 0o600

        reader, writer = await asyncio.open_unix_connection(str(sock))
        assert await recv(reader) == mock_state()  # immediate state_sync on connect

        writer.write(b"not json\n")
        send(writer, type="bogus")
        assert [(await recv(reader))["level"] for _ in range(2)] == ["error", "warn"]

        send(writer, type="select_host", host="arch-dev")
        assert (await recv(reader))["active_host"] == "arch-dev"
        send(writer, type="select_host", host="macbook-pro")
        await recv(reader)

        send(writer, type="submit_task", host="macbook-pro", prompt="Check disk usage")
        seen = await recv_until(reader, "approval_request")
        events = [m for m in seen if m["type"] == "activity_event"]
        assert [e["kind"] for e in events] == [
            "prompt",
            "ssh_exec",
            "vnc_click",
            "type_text",
            "ssh_exec",
        ]
        assert set(events[0]) == {"type", "ts", "task_id", "kind", "summary", "detail", "status"}
        task_id = events[0]["task_id"]
        syncs = [m for m in seen if m["type"] == "state_sync"]
        assert syncs[-1]["agent_state"] == "WAITING_APPROVAL"
        assert syncs[-1]["active_task"] == {
            "id": task_id,
            "title": "Check disk usage",
            "step": 5,
            "max_steps": 25,
        }
        request = seen[-1]
        assert set(request) == {
            "type",
            "request_id",
            "host",
            "action",
            "summary",
            "risk",
            "preview_1bit_120x68_b64",
        }
        assert request["host"] == "macbook-pro"

        # A second task is refused while one is running.
        send(writer, type="submit_task", host="macbook-pro", prompt="another")
        assert (await recv_until(reader, "toast"))[-1]["level"] == "warn"

        send(
            writer,
            type="approval_decision",
            request_id=request["request_id"],
            decision="approve_once",
        )
        second = (await recv_until(reader, "approval_request"))[-1]
        assert second["action"] == "ssh_exec"
        assert second["preview_1bit_120x68_b64"] is None

        send(writer, type="agent_control", command="kill_all")
        assert (await recv_until(reader, "toast"))[-1]["level"] == "warn"
        final = await recv(reader)
        assert (final["agent_state"], final["active_task"]) == ("IDLE", None)
        assert (await db.get_task(task_id))["status"] == "cancelled"

        send(writer, type="system_control", command="reboot")
        assert (await recv(reader))["text"] == "(mock) reboot scheduled"
        send(writer, type="ptt_audio", state="start")
        assert await recv(reader) == {"type": "mic_level", "rms": 0.42}
        send(writer, type="request_sync")
        assert (await recv(reader))["type"] == "state_sync"

        # A reconnecting UI is re-synced immediately.
        reader2, writer2 = await asyncio.open_unix_connection(str(sock))
        assert (await recv(reader2))["type"] == "state_sync"

        await asyncio.wait_for(server.stop(), 5)
        assert not sock.exists()
        assert await reader.readline() == b""
        writer.close()
        writer2.close()

    asyncio.run(scenario())
