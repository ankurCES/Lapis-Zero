from __future__ import annotations

import asyncio
import base64
import os
import stat
from pathlib import Path

from conftest import make_core, recv, recv_until, send, wait_for

from lapis_daemon.ipc_server import IpcServer


def test_full_task_flow_over_socket(short_tmp: Path) -> None:
    async def scenario() -> None:
        sock = short_tmp / "lapisd.sock"
        sock.write_text("stale")
        core = await make_core(short_tmp)
        server = IpcServer(sock, core)
        await server.start()
        assert stat.S_IMODE(sock.stat().st_mode) == 0o600

        reader, writer = await asyncio.open_unix_connection(str(sock))
        first = await recv(reader)  # immediate state_sync on connect
        assert first["type"] == "state_sync"
        assert (first["agent_state"], first["active_host"]) == ("IDLE", "macbook-pro")
        assert first["action"] == ""  # no step is being executed
        assert first["hosts"] == ["macbook-pro", "arch-dev"]
        assert (first["wifi_ssid"], first["tailscale_state"]) == ("HomeWiFi", "connected")
        assert (first["battery_pct"], first["api_key_ready"]) == (84, True)
        assert first["pending_approvals"] == [] and first["pairing"] is None
        assert (await recv(reader))["type"] == "ui_settings"

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
        assert request["host"] == "macbook-pro" and request["action"] == "vnc_click"
        # Chat traffic is for the phone; the 1 FPS device UI never sees it.
        assert not [m for m in seen if m["type"].startswith("chat_")]

        # A second task is refused while one is running.
        send(writer, type="submit_task", host="macbook-pro", prompt="another")
        assert (await recv_until(reader, "toast"))[-1]["level"] == "warn"

        # A UI that reconnects mid-approval gets the modal again.
        reader2, writer2 = await asyncio.open_unix_connection(str(sock))
        resync = await recv_until(reader2, "approval_request")
        assert resync[0]["type"] == "state_sync"
        assert resync[0]["pending_approvals"] == [request]
        assert resync[-1] == request

        send(
            writer,
            type="approval_decision",
            request_id=request["request_id"],
            decision="approve_once",
        )
        resolved = (await recv_until(reader2, "approval_resolved"))[-1]
        assert (resolved["decision"], resolved["by"]) == ("approve_once", "device")
        second = (await recv_until(reader, "approval_request"))[-1]
        assert second["action"] == "ssh_exec"
        assert second["preview_1bit_120x68_b64"] is None

        send(writer, type="agent_control", command="kill_all")
        ended = await recv_until(reader, "toast")
        assert ended[-1]["level"] == "warn"
        # How the task ended is said on this socket: the LCD shows "done" and "error"
        # from it. It follows the state frame in which the task is gone.
        device_chat = await core.chats.device_chat("macbook-pro")
        assert [m["type"] for m in ended[-3:]] == ["state_sync", "task_finished", "toast"]
        assert (ended[-3]["agent_state"], ended[-3]["active_task"]) == ("IDLE", None)
        assert ended[-2] == {
            "type": "task_finished",
            "task_id": task_id,
            "chat_id": device_chat["id"],
            "host": "macbook-pro",
            "status": "cancelled",
            "summary": "Stopped by the kill switch.",
        }
        final = (await recv_until(reader, "state_sync"))[-1]
        assert (final["agent_state"], final["active_task"]) == ("IDLE", None)
        assert (await core.db.get_task(task_id))["status"] == "cancelled"

        send(writer, type="system_control", command="reboot")
        assert (await recv_until(reader, "toast"))[-1]["text"] == "(mock) reboot scheduled"
        send(writer, type="ptt_audio", state="start")
        assert (await recv_until(reader, "mic_level"))[-1] == {"type": "mic_level", "rms": 0.42}
        send(writer, type="request_sync")
        assert (await recv_until(reader, "state_sync"))[-1]["type"] == "state_sync"

        await core.stop()
        await asyncio.wait_for(server.stop(), 5)
        assert not sock.exists()
        writer.close()
        writer2.close()

    asyncio.run(scenario())


def test_pairing_screen_snapshot_and_ble_helper_messages(short_tmp: Path) -> None:
    async def scenario() -> None:
        core = await make_core(short_tmp, dev_advertise="127.0.0.1:8421")
        server = IpcServer(short_tmp / "lapisd.sock", core)
        await server.start()
        reader, writer = await asyncio.open_unix_connection(str(server.socket_path))
        await recv_until(reader, "ui_settings")

        # Menu -> Pair Android App: tailnet is up in mock mode, so a QR is offered.
        send(writer, type="pairing_control", command="start")
        pairing = (await recv_until(reader, "pairing"))[-1]
        assert pairing["mode"] == "qr" and pairing["expires_in"] == 180
        assert pairing["payload"].startswith("lapis://pair?host=127.0.0.1:8421&nonce=")
        assert pairing["payload"].endswith(f"&pin={pairing['pin']}&tls=0")
        assert (await recv_until(reader, "state_sync"))[-1]["pairing"] == pairing

        send(writer, type="pairing_control", command="stop")
        assert (await recv_until(reader, "pairing"))[-1] == {"type": "pairing", "mode": "off"}

        # The BLE provisioning process reports its PIN over the same socket.
        send(
            writer,
            type="pairing_event",
            mode="pin",
            pin="482917",
            device_name="LapisZero-4F2A",
            status="waiting",
            expires_in=300,
            junk="dropped",
        )
        shown = (await recv_until(reader, "pairing"))[-1]
        assert shown == {
            "type": "pairing",
            "mode": "pin",
            "pin": "482917",
            "device_name": "LapisZero-4F2A",
            "status": "waiting",
            "expires_in": 300,
        }
        send(writer, type="pairing_event", mode="off")
        assert (await recv_until(reader, "pairing"))[-1]["mode"] == "off"
        # If the BLE service could not start, the LCD says why.
        send(writer, type="pairing_event", mode="off", error="No Bluetooth adapter")
        warned = (await recv_until(reader, "toast"))[-1]
        assert warned["level"] == "warn" and "No Bluetooth adapter" in warned["text"]

        # push_secrets from the BLE helper: the daemon picks the new key up.
        core.paths.secrets.write_text("LAPIS_TEST_PUSHED=abc\n")
        send(writer, type="reload_secrets")
        await wait_for(lambda: "LAPIS_TEST_PUSHED" in os.environ)
        assert os.environ.pop("LAPIS_TEST_PUSHED") == "abc"

        # Screen mirror: the daemon asks, the UI answers with a PNG.
        grab = asyncio.create_task(core.screen_png())
        ask = (await recv_until(reader, "screen_request"))[-1]
        send(
            writer,
            type="screen_snapshot",
            id=ask["id"],
            png_b64=base64.b64encode(b"\x89PNG fake").decode(),
        )
        assert await asyncio.wait_for(grab, 2) == b"\x89PNG fake"

        await core.stop()
        await server.stop()
        writer.close()

    asyncio.run(scenario())


def test_ptt_context_routes_to_voice_controller(short_tmp: Path) -> None:
    """PTT start/stop drives a real VoiceController: mic_level stream + transcript."""

    async def scenario() -> None:
        from lapis_daemon.config import VoiceConfig
        from lapis_daemon.voice import FakeTranscriber, SyntheticSource, VoiceController

        core = await make_core(short_tmp)
        server = IpcServer(short_tmp / "voice.sock", core)
        transcripts: list[str] = []
        core.voice = VoiceController(
            VoiceConfig(enabled=True, max_capture_bytes=256 * 1024),
            source_factory=lambda: SyntheticSource(chunks=8),
            transcriber_factory=lambda: FakeTranscriber("run the diagnostics"),
            send_mic=server.send_mic,
            on_transcript=transcripts.append,
        )
        await server.start()

        reader, writer = await asyncio.open_unix_connection(str(server.socket_path))
        await recv_until(reader, "ui_settings")

        send(writer, type="ptt_audio", state="start")
        # Collect mic_level frames until the stop edge's final 0.0 lands.
        rms_values: list[float] = []
        while len(rms_values) == 0 or rms_values[-1] != 0.0:
            msg = await recv(reader)
            if msg["type"] == "mic_level":
                rms_values.append(msg["rms"])

        send(writer, type="ptt_audio", state="stop")
        assert rms_values[0] == 0.0  # hold-begin initial level

        # The transcript callback fires after the stop edge finalizes.
        for _ in range(200):
            if transcripts:
                break
            await asyncio.sleep(0.001)
        assert transcripts == ["run the diagnostics"]

        await core.stop()
        await asyncio.wait_for(server.stop(), 5)
        writer.close()

    asyncio.run(scenario())
