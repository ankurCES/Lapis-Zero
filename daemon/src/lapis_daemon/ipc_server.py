"""NDJSON IPC server on a Unix domain socket (SPEC 6): the on-device transport.

A thin adapter over `Core`. lapis-ui is the usual client; the on-demand BLE
provisioning process and CLI debug tools speak the same protocol.
"""

from __future__ import annotations

import asyncio
import base64
import json
import os
from pathlib import Path
from typing import Any

# A client that stops reading is dropped once this much output is queued for it.
MAX_WRITE_BUFFER = 1024 * 1024
# Frames only the remote control plane renders; keep them off the 1 FPS device UI.
# (`task_finished` is not one of them: the LCD shows "done" and "error" from it.)
SKIP = frozenset({"chat_message_created", "chat_updated", "chat_deleted", "telemetry_tick"})
# A screen snapshot line is a base64 PNG of a 320x170 frame (well under this).
LINE_LIMIT = 512 * 1024


def toast(level: str, text: str) -> dict[str, Any]:
    return {"type": "toast", "level": level, "text": text}


class IpcServer:
    def __init__(self, socket_path: str | Path, core: Any) -> None:
        self.socket_path = Path(socket_path)
        self.core = core
        self._server: asyncio.Server | None = None
        self._writers: set[asyncio.StreamWriter] = set()
        core.add_sink(self.send_to_all, SKIP)

    async def start(self) -> None:
        self.socket_path.parent.mkdir(parents=True, exist_ok=True)
        self.socket_path.unlink(missing_ok=True)  # stale socket from a previous run
        old_umask = os.umask(0o177)  # socket is created 0600
        try:
            self._server = await asyncio.start_unix_server(
                self._handle_client, path=str(self.socket_path), limit=LINE_LIMIT
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

    def send_mic(self, rms: float) -> None:
        """Broadcast a live `mic_level` frame to the UI waveform."""
        self.core.publish({"type": "mic_level", "rms": round(rms, 4)})

    async def _handle_client(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        self._writers.add(writer)
        self._resync(writer)
        try:
            while line := await reader.readline():
                if not line.strip():
                    continue
                try:
                    await self._dispatch(json.loads(line), writer)
                except Exception as exc:  # one bad message must not drop the connection
                    self._send(writer, toast("error", f"IPC error: {exc}"))
        except (ConnectionError, ValueError):  # ValueError: line over the reader limit
            pass
        finally:
            self._writers.discard(writer)
            writer.close()

    def _resync(self, writer: asyncio.StreamWriter) -> None:
        """Everything a (re)connecting UI needs to redraw: state, modal, pairing view."""
        self._send(writer, self.core.state())
        self._send(writer, {"type": "ui_settings", **self.core.ui})
        for request in self.core.runner.pending.values():
            self._send(writer, request)
        if self.core.pairing is not None:
            self._send(writer, self.core.pairing)

    async def _dispatch(self, msg: Any, writer: asyncio.StreamWriter) -> None:
        core = self.core
        match msg:
            case {"type": "request_sync"}:
                self._resync(writer)
            case {"type": "submit_task", "host": str(host), "prompt": str(prompt)} if (
                prompt.strip()
            ):
                # Returns once the task is persisted; the step loop runs as its own asyncio.Task.
                try:
                    await core.submit_prompt(prompt, host=host, source="device")
                except (ValueError, LookupError) as exc:
                    self._send(writer, toast("warn", str(exc)))
            case {
                "type": "approval_decision",
                "request_id": str(request_id),
                "decision": "approve_once" | "approve_session" | "deny" as decision,
            }:
                core.decide(request_id, decision, "device")
            case {"type": "select_host", "host": str(host)}:
                core.select_host(host)
            case {"type": "agent_control", "command": "kill_all"}:
                await core.kill_all("device")
            case {"type": "agent_control", "command": "pause"}:
                core.runner.pause()
                core.broadcast_state()
            case {"type": "agent_control", "command": "resume"}:
                core.runner.resume()
                core.broadcast_state()
            case {
                "type": "system_control",
                "command": "retry_tailscale" | "reboot" | "shutdown" as command,
            }:
                await core.system_control(command)
            case {"type": "pairing_control", "command": "start"}:
                try:
                    await core.pairing_start(str(msg.get("mode", "auto")))
                except ValueError as exc:
                    self._send(writer, toast("warn", str(exc)))
            case {"type": "pairing_control", "command": "stop"}:
                await core.pairing_stop()
            case {"type": "pairing_event"}:  # from lapis-ble-prov
                core.on_pairing_event(msg)
            case {"type": "wifi_ble"}:  # from lapis-ble-prov in the phone Wi-Fi window
                core.on_wifi_ble(msg)
            case {"type": "reload_secrets"}:  # from lapis-ble-prov after push_secrets
                core.reload_secrets()
            case {"type": "refresh_status"}:  # from lapis-ble-prov after Wi-Fi / Tailscale
                await core.status.refresh()
                core.broadcast_state()
            case {"type": "screen_snapshot", "id": str(request_id), "png_b64": str(data)}:
                core.on_screen_snapshot(request_id, base64.b64decode(data))
            case {"type": "ptt_audio", "state": "start" | "stop" as state}:
                await self._handle_ptt(state)
            case _:
                self._send(writer, toast("warn", "Unrecognized IPC message"))

    async def _handle_ptt(self, state: str) -> None:
        """Route a PTT edge to the voice controller, or fake a wave in mock mode."""
        voice = self.core.voice
        if voice is None:
            if self.core.mock:
                self.send_mic(0.42 if state == "start" else 0.0)
            elif state == "start":
                # Never invent a transcript on a real device.
                self.core.toast("warn", "Voice input is not configured")
            return
        try:
            if state == "start":
                await voice.begin_ptt()
            else:
                await voice.end_ptt()
        except Exception as exc:
            self.core.toast("error", f"voice: {exc}")
