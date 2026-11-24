"""NDJSON IPC server on a Unix domain socket (SPEC 6)."""

from __future__ import annotations

import asyncio
import json
import os
from collections.abc import Callable
from pathlib import Path
from typing import Any

from lapis_daemon.agent.runner import TaskRunner
from lapis_daemon.config import HostsConfig
from lapis_daemon.db import LapisDb

# A client that stops reading is dropped once this much output is queued for it.
MAX_WRITE_BUFFER = 1024 * 1024


def toast(level: str, text: str) -> dict[str, Any]:
    return {"type": "toast", "level": level, "text": text}


class IpcServer:
    def __init__(
        self,
        socket_path: str | Path,
        db: LapisDb,
        hosts: HostsConfig,
        runner: TaskRunner,
        state_provider: Callable[[], dict[str, Any]],
    ) -> None:
        self.socket_path = Path(socket_path)
        self.db = db
        self.hosts = hosts
        self.runner = runner
        self.state_provider = state_provider
        preferred = state_provider().get("active_host", "")
        self.active_host = preferred if preferred in hosts.hosts else next(iter(hosts.hosts), "")
        self._server: asyncio.Server | None = None
        self._writers: set[asyncio.StreamWriter] = set()
        runner.emit_state = self.broadcast_state
        runner.emit_event = self.send_to_all
        runner.emit_approval = self.send_to_all
        runner.emit_toast = lambda level, text: self.send_to_all(toast(level, text))

    def state(self) -> dict[str, Any]:
        """System status from the provider overlaid with live daemon state."""
        return {
            **self.state_provider(),
            "type": "state_sync",
            "agent_state": self.runner.agent_state,
            "active_host": self.active_host,
            "hosts": list(self.hosts.hosts),
            "active_task": self.runner.active_task,
        }

    async def start(self) -> None:
        self.socket_path.parent.mkdir(parents=True, exist_ok=True)
        self.socket_path.unlink(missing_ok=True)  # stale socket from a previous run
        old_umask = os.umask(0o177)  # socket is created 0600
        try:
            self._server = await asyncio.start_unix_server(
                self._handle_client, path=str(self.socket_path)
            )
        finally:
            os.umask(old_umask)

    async def stop(self) -> None:
        if self._server is None:
            return
        self._server.close()
        for writer in list(self._writers):
            writer.close()
        await self._server.wait_closed()
        self._server = None
        self.socket_path.unlink(missing_ok=True)

    def _send(self, writer: asyncio.StreamWriter, msg: dict[str, Any]) -> None:
        # ponytail: unawaited write + buffer cap instead of per-client queues; one UI client
        if writer.is_closing() or writer.transport.get_write_buffer_size() > MAX_WRITE_BUFFER:
            self._writers.discard(writer)
            writer.close()
            return
        writer.write(json.dumps(msg).encode() + b"\n")

    def send_to_all(self, msg: dict[str, Any]) -> None:
        for writer in list(self._writers):
            self._send(writer, msg)

    def broadcast_state(self) -> None:
        self.send_to_all(self.state())

    async def _handle_client(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        self._writers.add(writer)
        self._send(writer, self.state())  # immediate re-sync for (re)connecting UIs
        try:
            while line := await reader.readline():
                if not line.strip():
                    continue
                try:
                    await self._dispatch(json.loads(line), writer)
                except Exception as exc:  # one bad message must not drop the connection
                    self._send(writer, toast("error", f"IPC error: {exc}"))
        except (ConnectionError, ValueError):  # ValueError: line over the 64 KiB reader limit
            pass
        finally:
            self._writers.discard(writer)
            writer.close()

    async def _dispatch(self, msg: Any, writer: asyncio.StreamWriter) -> None:
        match msg:
            case {"type": "request_sync"}:
                self._send(writer, self.state())
            case {"type": "submit_task", "host": str(host), "prompt": str(prompt)} if (
                prompt.strip()
            ):
                # Returns once the task is persisted; the step loop runs as its own asyncio.Task.
                try:
                    await self.runner.submit_task(host, prompt)
                except ValueError as exc:
                    self._send(writer, toast("warn", str(exc)))
            case {
                "type": "approval_decision",
                "request_id": str(request_id),
                "decision": "approve_once" | "approve_session" | "deny" as decision,
            }:
                self.runner.resolve_approval(request_id, decision)
            case {"type": "select_host", "host": str(host)} if host in self.hosts.hosts:
                self.active_host = host
                self.broadcast_state()
            case {"type": "agent_control", "command": "kill_all"}:
                await self.runner.kill_all()
            case {"type": "agent_control", "command": "pause"}:
                self.runner.pause()
                self.broadcast_state()
            case {"type": "agent_control", "command": "resume"}:
                self.runner.resume()
                self.broadcast_state()
            case {
                "type": "system_control",
                "command": "retry_tailscale" | "reboot" | "shutdown" as command,
            }:
                self.send_to_all(toast("info", f"(mock) {command} scheduled"))
            case {"type": "ptt_audio", "state": "start" | "stop" as state}:
                self.send_to_all({"type": "mic_level", "rms": 0.42 if state == "start" else 0.0})
            case _:
                self._send(writer, toast("warn", "Unrecognized IPC message"))
