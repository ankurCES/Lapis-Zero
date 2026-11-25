"""Control plane: auth, pairing, chats, approvals, secrets, config, WS <-> IPC sync."""

from __future__ import annotations

import asyncio
import json
import logging
import os
import stat
from pathlib import Path
from typing import Any

import pytest
from aiohttp.test_utils import TestClient, TestServer
from conftest import make_core, recv, recv_match, recv_until, send, wait_for

from lapis_daemon.api.auth import NONCE_TTL_SEC, hash_token
from lapis_daemon.api.server import ApiServer, build_app
from lapis_daemon.config import load_hosts
from lapis_daemon.frames import FrameStore
from lapis_daemon.ipc_server import IpcServer
from lapis_daemon.kvm.base import Frame, MockHostDriver

TOKEN = "test-token-0123456789"
AUTH = {"Authorization": f"Bearer {TOKEN}"}


def secret_values_absent(text: str) -> bool:
    return not any(s in text for s in ("SUPERSECRET", "correct horse battery", TOKEN))


async def client_for(d: Path, **kwargs: Any):
    core = await make_core(d, **kwargs)
    await core.auth.register_fixed("dev_test", "Pixel", TOKEN)
    client = TestClient(TestServer(build_app(core)))
    await client.start_server()
    return core, client


async def ws_until(ws: Any, msg_type: str) -> list[dict[str, Any]]:
    seen = []
    while True:
        seen.append(json.loads((await asyncio.wait_for(ws.receive(), 3)).data))
        if seen[-1]["type"] == msg_type:
            return seen


def test_every_route_requires_a_valid_bearer_token(short_tmp: Path) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        routes = {
            (r.method, r.resource.canonical)
            for r in client.app.router.routes()
            if r.method not in ("HEAD", "OPTIONS")
        }
        assert len(routes) > 35
        for method, path in sorted(routes):
            url = path.replace("{id:\\d+}", "1").replace("{", "").replace("}", "")
            if (method, path) == ("POST", "/v1/pair/exchange"):
                continue  # the one public route; covered below
            for headers in ({}, {"Authorization": "Bearer wrong"}, {"Authorization": TOKEN}):
                resp = await client.request(method, url, headers=headers)
                assert resp.status == 401, (method, path, headers)
                assert (await resp.json())["error"]["code"] == "unauthorized"

        # Tokens are stored hashed, and a revoked one stops working at once.
        rows = await core.db.run("SELECT token_hash FROM paired_devices")
        assert rows[0]["token_hash"] == hash_token(TOKEN) != TOKEN
        assert (await client.get("/v1/health", headers=AUTH)).status == 200
        assert (await client.delete("/v1/devices/dev_test", headers=AUTH)).status == 204
        assert (await client.get("/v1/health", headers=AUTH)).status == 401
        await client.close()
        await core.stop()

    asyncio.run(scenario())


def test_control_plane_binds_to_loopback_only(short_tmp: Path) -> None:
    async def scenario() -> None:
        core = await make_core(short_tmp)
        api = ApiServer(core, 0)
        await api.start()
        assert api._runner is not None
        assert {addr[0] for addr in api._runner.addresses} == {"127.0.0.1"}
        await api.stop()
        await core.stop()

    asyncio.run(scenario())


def test_qr_pairing_nonce_is_single_use_expires_and_locks_out(
    short_tmp: Path, monkeypatch: Any
) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp, dev_advertise="127.0.0.1:8421")
        body = {"device_name": "Pixel 9", "device_model": "Pixel 9 Pro"}

        resp = await client.post(
            "/v1/device/power", json={"action": "start_qr_pairing"}, headers=AUTH
        )
        assert resp.status == 202
        payload = core.pairing["payload"]
        nonce = payload.split("nonce=")[1].split("&")[0]
        assert len(nonce) == 43  # 32 random bytes, base64url

        resp = await client.post("/v1/pair/exchange", json={**body, "nonce": nonce})
        paired = await resp.json()
        assert resp.status == 200 and set(paired) == {"device_id", "api_token", "hostname"}
        assert core.pairing is None  # the QR leaves the LCD once used
        health = await client.get(
            "/v1/health", headers={"Authorization": f"Bearer {paired['api_token']}"}
        )
        assert (await health.json())["device_id"] == paired["device_id"]
        # Single use.
        assert (await client.post("/v1/pair/exchange", json={**body, "nonce": nonce})).status == 403

        # Expiry: a nonce older than 180 s is refused.
        fresh = core.auth.new_nonce()
        import time as _time

        real = _time.monotonic
        monkeypatch.setattr(_time, "monotonic", lambda: real() + NONCE_TTL_SEC + 1)
        assert (await client.post("/v1/pair/exchange", json={**body, "nonce": fresh})).status == 403
        monkeypatch.setattr(_time, "monotonic", real)

        # Brute force: after five misses in a minute even a good nonce is refused.
        good = core.auth.new_nonce()
        for _ in range(4):
            await client.post("/v1/pair/exchange", json={**body, "nonce": "guess"})
        resp = await client.post("/v1/pair/exchange", json={**body, "nonce": good})
        assert resp.status == 429 and (await resp.json())["error"]["code"] == "locked_out"
        await client.close()
        await core.stop()

    asyncio.run(scenario())


def test_chat_run_syncs_phone_and_device_in_both_directions(short_tmp: Path) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        ipc = IpcServer(short_tmp / "lapisd.sock", core)
        await ipc.start()
        reader, writer = await asyncio.open_unix_connection(str(ipc.socket_path))
        await recv_until(reader, "ui_settings")

        # An upgrade without a token is refused before the socket opens.
        resp = await client.get("/v1/ws")
        assert resp.status == 401
        ws = await client.ws_connect("/v1/ws", headers=AUTH)
        first = json.loads((await ws.receive()).data)
        assert first["type"] == "state_sync" and first["device"]["mock"] is True

        resp = await client.post("/v1/chats", json={"host_id": "arch-dev"}, headers=AUTH)
        chat = await resp.json()
        assert resp.status == 201 and chat["title"] == "New chat" and chat["active"] is False
        bad = await client.post("/v1/chats", json={"host_id": "nope"}, headers=AUTH)
        assert bad.status == 422

        # A prompt from the phone animates the device...
        resp = await client.post(
            f"/v1/chats/{chat['id']}/messages", json={"content": "Check CI"}, headers=AUTH
        )
        sent = await resp.json()
        assert resp.status == 202 and sent["message"]["role"] == "user"
        assert sent["message"]["metadata"] == {"source": "android:Pixel"}
        busy = await client.post(
            f"/v1/chats/{chat['id']}/messages", json={"content": "again"}, headers=AUTH
        )
        assert busy.status == 409 and (await busy.json())["error"]["code"] == "busy"

        # ...and the approval pops on both (strict host: the first click blocks).
        on_phone = (await ws_until(ws, "approval_request"))[-1]
        on_device = (await recv_until(reader, "approval_request"))[-1]
        assert on_phone == on_device and on_phone["chat_id"] == chat["id"]
        listed = await (await client.get("/v1/approvals", headers=AUTH)).json()
        assert listed["approvals"] == [on_phone]
        listing = await (await client.get("/v1/chats", headers=AUTH)).json()
        assert [(c["active"], c["pending_approvals"]) for c in listing["chats"]] == [(True, 1)]

        # Approving on the phone clears the modal on the device and resumes the agent.
        resp = await client.post(
            f"/v1/approvals/{on_phone['request_id']}",
            json={"decision": "approve_once"},
            headers=AUTH,
        )
        assert resp.status == 200
        cleared = (await recv_until(reader, "approval_resolved"))[-1]
        assert cleared["request_id"] == on_phone["request_id"]
        assert (cleared["decision"], cleared["by"]) == ("approve_once", "android:Pixel")
        again = await client.post(
            f"/v1/approvals/{on_phone['request_id']}", json={"decision": "deny"}, headers=AUTH
        )
        assert again.status == 404

        # Approving the next one on the device clears it on the phone.
        second = (await ws_until(ws, "approval_request"))[-1]
        send(
            writer,
            type="approval_decision",
            request_id=second["request_id"],
            decision="approve_once",
        )
        resolved = (await ws_until(ws, "approval_resolved"))[-1]
        assert (resolved["request_id"], resolved["by"]) == (second["request_id"], "device")

        # The kill switch on the device stops everything on the phone at once.
        await ws_until(ws, "approval_request")
        send(writer, type="agent_control", command="kill_all")
        seen = await ws_until(ws, "task_finished")
        assert seen[-1]["status"] == "cancelled"
        assert [m for m in seen if m["type"] == "approval_resolved"][0]["by"] == "kill_switch"
        await wait_for(lambda: core.runner.agent_state == "IDLE")
        await core._chat_q.join()

        # The thread holds the whole run: prompt, tool calls, closing message.
        resp = await client.get(f"/v1/chats/{chat['id']}", headers=AUTH)
        thread = await resp.json()
        assert thread["chat"]["title"] == "Check CI" and thread["has_more"] is False
        roles = [m["role"] for m in thread["messages"]]
        assert roles[0] == "user" and roles[-1] == "assistant" and "tool" in roles
        tool = next(m for m in thread["messages"] if m["role"] == "tool")
        assert {"kind", "status", "detail", "screenshot_id", "step", "ts"} <= set(tool["metadata"])
        assert thread["messages"][-1]["metadata"] == {"status": "cancelled"}
        page = await (
            await client.get(
                f"/v1/chats/{chat['id']}?limit=2&before={thread['messages'][-1]['id']}",
                headers=AUTH,
            )
        ).json()
        assert len(page["messages"]) == 2 and page["has_more"] is True

        # WS commands: ping, a prompt, a decision, the kill switch.
        await ws.send_json({"type": "ping"})
        assert (await ws_until(ws, "pong"))[-1]["ts"] > 0
        await ws.send_json({"type": "send_prompt", "chat_id": chat["id"], "content": "Round two"})
        third = (await ws_until(ws, "approval_request"))[-1]
        await ws.send_json(
            {"type": "approval_decision", "request_id": third["request_id"], "decision": "deny"}
        )
        assert (await ws_until(ws, "task_finished"))[-1]["status"] == "cancelled"
        await ws.send_json({"type": "nonsense"})
        assert (await ws_until(ws, "toast"))[-1]["level"] == "error"

        # A prompt typed on the device lands in its own thread, visible to the phone.
        send(writer, type="submit_task", host="macbook-pro", prompt="From the keypad")
        while True:  # skip the previous run's trailing tool/assistant rows
            created = (await ws_until(ws, "chat_message_created"))[-1]
            if created["message"]["role"] == "user":
                break
        assert created["message"]["content"] == "From the keypad"
        assert created["message"]["metadata"] == {"source": "device"}
        await ws.send_json({"type": "kill_all"})
        await ws_until(ws, "task_finished")

        patched = await client.patch(
            f"/v1/chats/{chat['id']}", json={"title": "Renamed", "archived": True}, headers=AUTH
        )
        assert (await patched.json())["archived"] is True
        assert (await client.delete(f"/v1/chats/{chat['id']}", headers=AUTH)).status == 204
        assert (await client.get(f"/v1/chats/{chat['id']}", headers=AUTH)).status == 404

        await ws.close()
        writer.close()
        await client.close()
        await core.stop()
        await ipc.stop()

    asyncio.run(scenario())


def test_every_screen_is_told_what_the_agent_is_doing_and_how_the_task_ended(
    short_tmp: Path,
) -> None:
    """`action` in every state frame; `task_finished` on the handheld's socket as well."""
    steps = ["ssh_exec", "vnc_click", "type_text", "ssh_exec", "vnc_click", "ssh_exec"]

    def doing(frames: list[dict[str, Any]]) -> list[tuple[str, str]]:
        return [(m["agent_state"], m["action"]) for m in frames if m["type"] == "state_sync"]

    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        ipc = IpcServer(short_tmp / "lapisd.sock", core)
        await ipc.start()
        reader, writer = await asyncio.open_unix_connection(str(ipc.socket_path))
        ws = await client.ws_connect("/v1/ws", headers=AUTH)
        over_rest = await (await client.get("/v1/state", headers=AUTH)).json()
        for idle in (await recv(reader), json.loads((await ws.receive()).data), over_rest):
            assert (idle["type"], idle["agent_state"], idle["action"]) == ("state_sync", "IDLE", "")

        # A whole run with nothing to approve, watched from the handheld and the phone.
        body = {"host_id": "macbook-pro", "policy_override": "autonomous"}
        chat = await (await client.post("/v1/chats", json=body, headers=AUTH)).json()
        messages = f"/v1/chats/{chat['id']}/messages"
        sent = await (
            await client.post(messages, json={"content": "Check CI"}, headers=AUTH)
        ).json()
        on_phone = await ws_until(ws, "task_finished")
        on_device = await recv_until(reader, "task_finished")
        # Both saw the same states. Each step was named while it ran, in its turn, and a
        # step is only ever named while the agent is ACTING.
        assert doing(on_device) == doing(on_phone)
        assert [action for _state, action in doing(on_phone) if action] == steps
        assert all(state == "ACTING" for state, action in doing(on_phone) if action)
        for frames in (on_device, on_phone):  # named first, reported when it has returned
            order = [
                m["action"] if m["type"] == "state_sync" else m["kind"]
                for m in frames
                if (m["type"] == "state_sync" and m["action"])
                or (m["type"] == "activity_event" and m["kind"] != "prompt")
            ]
            assert order == [kind for step in steps for kind in (step, step)]
        # How it ended, on the handheld's socket as on the phone's: right after the
        # state frame that shows the agent idle, and nothing in it but these six fields.
        assert (
            on_device[-1]
            == on_phone[-1]
            == {
                "type": "task_finished",
                "task_id": sent["task_id"],
                "chat_id": chat["id"],
                "host": "macbook-pro",
                "status": "done",
                "summary": "Mock run complete: 6 scripted steps.",
            }
        )
        for frames in (on_device, on_phone):
            assert (frames[-2]["type"], frames[-2]["agent_state"]) == ("state_sync", "IDLE")
            assert frames[-2]["active_task"] is None and frames[-2]["action"] == ""
        assert not [m for m in on_device if m["type"].startswith("chat_")]  # still phone-only

        # In the middle of a step: GET /v1/state names it, and so does the first frame a
        # screen gets when it (re)connects.
        entered, release = asyncio.Event(), asyncio.Event()

        class Slow(MockHostDriver):
            async def run_ssh(self, command: str, timeout_s: float) -> str:
                entered.set()
                await release.wait()
                return "ok"

        core.runner.driver_factory = Slow
        await client.post(messages, json={"content": "Once more"}, headers=AUTH)
        await asyncio.wait_for(entered.wait(), 5)
        now = await (await client.get("/v1/state", headers=AUTH)).json()
        assert (now["agent_state"], now["action"]) == ("ACTING", "ssh_exec")
        late, late_writer = await asyncio.open_unix_connection(str(ipc.socket_path))
        late_ws = await client.ws_connect("/v1/ws", headers=AUTH)
        for first in (await recv(late), json.loads((await late_ws.receive()).data)):
            assert (first["agent_state"], first["action"]) == ("ACTING", "ssh_exec")
        # The kill switch on the handheld: it is told the task was cancelled, and the
        # last state names nothing.
        send(writer, type="agent_control", command="kill_all")
        killed = (await recv_until(reader, "task_finished"))[-1]
        assert (killed["status"], killed["summary"]) == ("cancelled", "Stopped by the kill switch.")
        assert (await ws_until(ws, "task_finished"))[-1] == killed
        final = (await recv_until(reader, "state_sync"))[-1]
        assert (final["agent_state"], final["action"], final["active_task"]) == ("IDLE", "", None)

        await ws.close()
        await late_ws.close()
        writer.close()
        late_writer.close()
        await client.close()
        await core.stop()
        await ipc.stop()

    asyncio.run(scenario())


def test_secrets_are_write_only_and_never_echoed(
    short_tmp: Path, caplog: pytest.LogCaptureFixture
) -> None:
    caplog.set_level(logging.DEBUG)

    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        secret = "sk-ant-api03-SUPERSECRETVALUE-a9f2"
        vnc = "correct horse battery"
        resp = await client.patch(
            "/v1/secrets",
            json={"set": {"ANTHROPIC_API_KEY": secret, "LAPIS_VNC_PASSWORD_macbook_pro": vnc}},
            headers=AUTH,
        )
        body = await resp.text()
        assert resp.status == 200 and secret not in body and vnc not in body
        by_name = {s["name"]: s for s in json.loads(body)["secrets"]}
        assert by_name["ANTHROPIC_API_KEY"] == {
            "name": "ANTHROPIC_API_KEY",
            "is_set": True,
            "masked": "...a9f2",
        }
        assert by_name["LAPIS_VNC_PASSWORD_arch_dev"]["is_set"] is False

        # 0600 on disk, loaded into the daemon, and absent from every read endpoint.
        assert stat.S_IMODE(core.paths.secrets.stat().st_mode) == 0o600
        assert os.environ["ANTHROPIC_API_KEY"] == secret
        for path in (
            "/v1/secrets/status", "/v1/config", "/v1/hosts", "/v1/state", "/v1/health",
            "/v1/devices", "/v1/device/telemetry", "/v1/schedules", "/v1/chats", "/v1/approvals",
        ):  # fmt: skip
            text = await (await client.get(path, headers=AUTH)).text()
            assert secret not in text and vnc not in text and "SUPERSECRET" not in text, path

        bad = await client.patch("/v1/secrets", json={"set": {"BAD NAME": "x"}}, headers=AUTH)
        assert bad.status == 422
        multi = await client.patch(
            "/v1/secrets", json={"set": {"A_KEY": "line1\nline2"}}, headers=AUTH
        )
        assert multi.status == 422

        resp = await client.patch(
            "/v1/secrets", json={"unset": ["ANTHROPIC_API_KEY"]}, headers=AUTH
        )
        assert resp.status == 200 and "ANTHROPIC_API_KEY" not in os.environ
        os.environ.pop("LAPIS_VNC_PASSWORD_macbook_pro", None)
        await client.close()
        await core.stop()

    asyncio.run(scenario())
    # Nothing the daemon logged (at any level) carries a secret or the bearer token.
    assert secret_values_absent(caplog.text)


def test_config_hosts_schedules_and_device_endpoints(short_tmp: Path) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        ipc = IpcServer(short_tmp / "lapisd.sock", core)
        await ipc.start()
        reader, writer = await asyncio.open_unix_connection(str(ipc.socket_path))
        await recv_until(reader, "ui_settings")

        # lapis.toml: validated, then written atomically.
        config = (await (await client.get("/v1/config", headers=AUTH)).json())["config"]
        config["llm"]["max_steps"] = 12
        resp = await client.put("/v1/config", json={"config": config}, headers=AUTH)
        assert resp.status == 200 and core.runner.max_steps == 12
        assert "max_steps = 12" in core.paths.config.read_text()
        config["llm"]["max_steps"] = 0
        assert (await client.put("/v1/config", json={"config": config}, headers=AUTH)).status == 422

        # hosts.toml: add a host, see it on the device, probe it.
        hosts = (await (await client.get("/v1/hosts", headers=AUTH)).json())["hosts"]
        hosts["studio"] = {"hostname": "studio", "tailscale_ip": "100.64.9.9", "os": "linux"}
        resp = await client.put("/v1/hosts", json={"hosts": hosts}, headers=AUTH)
        assert resp.status == 200 and "studio" in load_hosts(core.paths.hosts).hosts
        await recv_match(reader, "state_sync", lambda m: "studio" in m["hosts"])
        hosts["studio"]["policy"] = "yolo"
        assert (await client.put("/v1/hosts", json={"hosts": hosts}, headers=AUTH)).status == 422
        probe = await (await client.post("/v1/hosts/studio/probe", headers=AUTH)).json()
        assert all(probe[k]["ok"] for k in ("tailscale", "vnc", "ssh"))
        assert (await client.post("/v1/hosts/nope/probe", headers=AUTH)).status == 404
        resp = await client.put("/v1/agent/active-host", json={"host_id": "studio"}, headers=AUTH)
        assert (await resp.json()) == {"active_host": "studio"}

        # Schedules: CRUD, validation, run now.
        job = {
            "name": "Nightly",
            "schedule": "30 2 * * *",
            "host": "studio",
            "prompt": "Check",
            "enabled": True,
        }
        resp = await client.post("/v1/schedules", json=job, headers=AUTH)
        listed = {s["name"]: s for s in (await resp.json())["schedules"]}
        assert resp.status == 201 and listed["Nightly"]["next_run"] > 0
        assert (await client.post("/v1/schedules", json=job, headers=AUTH)).status == 422
        bad = await client.post(
            "/v1/schedules", json={**job, "name": "Bad", "schedule": "99 * * * *"}, headers=AUTH
        )
        assert bad.status == 422
        resp = await client.patch("/v1/schedules/Nightly", json={"enabled": False}, headers=AUTH)
        assert {s["name"]: s for s in (await resp.json())["schedules"]}["Nightly"][
            "next_run"
        ] is None
        ran = await client.post("/v1/schedules/Nightly/run", headers=AUTH)
        started = await ran.json()
        assert ran.status == 202 and started["task_id"].startswith("t_")
        await client.post("/v1/agent/kill-all", headers=AUTH)
        after = {
            s["name"]: s
            for s in (await (await client.get("/v1/schedules", headers=AUTH)).json())["schedules"]
        }
        assert after["Nightly"]["last_chat_id"] == started["chat_id"]
        assert (await client.delete("/v1/schedules/Nightly", headers=AUTH)).status == 204
        assert (await client.delete("/v1/schedules/Nightly", headers=AUTH)).status == 404

        # Device cockpit: telemetry, LCD settings reach lapis-ui, screen mirror.
        telemetry = await (await client.get("/v1/device/telemetry", headers=AUTH)).json()
        assert {"battery", "cpu", "memory", "wifi", "tailscale", "uptime_sec", "services"} == set(
            telemetry
        )
        resp = await client.patch(
            "/v1/device/ui", json={"theme": "dark", "brightness": 40, "wake": True}, headers=AUTH
        )
        ui = await resp.json()
        assert (ui["theme"], ui["brightness"], ui["mute_audio"]) == ("dark", 40, False)
        applied = await recv_match(reader, "ui_settings", lambda m: m.get("wake"))
        assert (applied["theme"], applied["brightness"]) == ("dark", 40)
        assert (
            await client.patch("/v1/device/ui", json={"theme": "neon"}, headers=AUTH)
        ).status == 422

        # Timezone: the phone sets it, the LCD clock gets the offset with every state_sync.
        resp = await client.patch(
            "/v1/device/ui", json={"timezone": "America/Los_Angeles"}, headers=AUTH
        )
        assert (await resp.json())["timezone"] == "America/Los_Angeles"
        state = await (await client.get("/v1/state", headers=AUTH)).json()
        assert state["utc_offset_min"] in (-480, -420) and state["ui"]["timezone"]
        assert "America/Los_Angeles" in core.paths.config.read_text()
        for bad in ("Mars/Olympus_Mons", "../../etc/passwd", ""):
            resp = await client.patch("/v1/device/ui", json={"timezone": bad}, headers=AUTH)
            assert resp.status == 422, bad

        grab = asyncio.create_task(client.get("/v1/device/screen.png", headers=AUTH))
        ask = (await recv_until(reader, "screen_request"))[-1]
        send(writer, type="screen_snapshot", id=ask["id"], png_b64="iVBORw0KGgo=")
        shot = await grab
        assert shot.status == 200 and shot.content_type == "image/png"
        assert await shot.read() == b"\x89PNG\r\n\x1a\n"

        assert (
            await client.post("/v1/hosts/studio/nudge", json={"action": "click"}, headers=AUTH)
        ).status == 409
        assert (
            await client.post("/v1/device/power", json={"action": "format"}, headers=AUTH)
        ).status == 422
        assert (
            await client.post("/v1/device/power", json={"action": "reboot"}, headers=AUTH)
        ).status == 202
        writer.close()
        await client.close()
        await core.stop()
        await ipc.stop()

    asyncio.run(scenario())


def test_screenshots_are_served_and_the_ring_buffer_evicts_oldest(short_tmp: Path) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        frame = Frame(b"\xff\xd8\xff" + bytes(1021), 1280, 720)  # 1 KiB
        core.frames = store = FrameStore(core.db, short_tmp / "frames", cap_bytes=10 * 1024)
        ids = [await store.save("t_1", n, frame, (5, 6) if n == 29 else None) for n in range(30)]

        # 30 KiB written into a 10 KiB cap: only the newest ten survive, on disk too.
        rows = await core.db.run("SELECT id, bytes FROM screenshots ORDER BY id")
        assert [r["id"] for r in rows] == ids[-10:]
        assert sum(r["bytes"] for r in rows) <= store.cap_bytes
        assert len(list((short_tmp / "frames").iterdir())) == 10

        resp = await client.get(f"/v1/screenshots/{ids[-1]}.jpg", headers=AUTH)
        assert resp.status == 200 and resp.content_type == "image/jpeg"
        assert (resp.headers["X-Lapis-Width"], resp.headers["X-Lapis-Click-X"]) == ("1280", "5")
        assert (await resp.read())[:3] == b"\xff\xd8\xff"
        assert (await client.get(f"/v1/screenshots/{ids[0]}.jpg", headers=AUTH)).status == 404
        await client.close()
        await core.stop()

    asyncio.run(scenario())
