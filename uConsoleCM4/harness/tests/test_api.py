"""Harness API: identity headers, the Unix socket, chats, approvals, secrets, config, WS."""

from __future__ import annotations

import asyncio
import json
import logging
import os
import stat
from pathlib import Path
from typing import Any

import aiohttp
import pytest
from aiohttp.test_utils import TestClient, TestServer
from conftest import make_core, wait_for

from lapis_harness.api.server import ApiServer, build_app
from lapis_harness.config import load_hosts
from lapis_harness.frames import FrameStore
from lapis_harness.kvm.base import Frame, MockHostDriver

# What lapis-daemon sends for a paired phone; the HUD and the daemon itself send
# kind=local, and a local tool on the socket sends nothing at all.
PHONE = {
    "x-lapis-device-id": "dev_test",
    "x-lapis-device-name": "Pixel",
    "x-lapis-device-kind": "phone",
}
HUD = {
    "x-lapis-device-id": "local",
    "x-lapis-device-name": "device",
    "x-lapis-device-kind": "local",
}
REMOVED = (
    ("POST", "/v1/pair/exchange"),
    ("GET", "/v1/devices"),
    ("DELETE", "/v1/devices/dev_test"),
    ("GET", "/v1/device/telemetry"),
    ("PATCH", "/v1/device/ui"),
    ("GET", "/v1/device/screen.png"),
    ("POST", "/v1/device/power"),
)


def secret_values_absent(text: str) -> bool:
    return not any(s in text for s in ("SUPERSECRET", "correct horse battery"))


async def client_for(d: Path, **kwargs: Any):
    core = await make_core(d, **kwargs)
    client = TestClient(TestServer(build_app(core)))
    await client.start_server()
    return core, client


async def ws_until(ws: Any, msg_type: str) -> list[dict[str, Any]]:
    seen = []
    while True:
        seen.append(json.loads((await asyncio.wait_for(ws.receive(), 3)).data))
        if seen[-1]["type"] == msg_type:
            return seen


async def ws_match(ws: Any, msg_type: str, predicate: Any) -> dict[str, Any]:
    """Next `msg_type` frame satisfying `predicate` (earlier, stale ones are skipped)."""
    while True:
        msg = (await ws_until(ws, msg_type))[-1]
        if predicate(msg):
            return msg


def test_no_route_asks_for_a_token_and_the_caller_is_who_the_daemon_says(
    short_tmp: Path,
) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        routes = {
            (r.method, r.resource.canonical)
            for r in client.app.router.routes()
            if r.method not in ("HEAD", "OPTIONS")
        }
        assert len(routes) > 30
        for method, path in sorted(routes):
            url = path.replace("{id:\\d+}", "1").replace("{", "").replace("}", "")
            if path == "/v1/ws":
                continue
            resp = await client.request(method, url, headers={"Authorization": "Bearer x"})
            assert resp.status != 401, (method, path)

        # Who asks: the daemon's headers, never a token; nothing said is the device itself.
        health = await (await client.get("/v1/health", headers=PHONE)).json()
        assert health["device_id"] == "dev_test" and health["kind"] == "lapis-harness"
        assert (await (await client.get("/v1/health")).json())["device_id"] == "device"
        # What moved to the daemon is gone from here.
        for method, path in REMOVED:
            assert (await client.request(method, path, headers=HUD)).status in (404, 405), path
        await client.close()
        await core.stop()

    asyncio.run(scenario())


def test_the_api_is_served_on_a_unix_socket_only_its_group_can_open(short_tmp: Path) -> None:
    async def scenario() -> None:
        core = await make_core(short_tmp)
        sock = short_tmp / "run" / "harness.sock"
        sock.parent.mkdir()
        sock.write_text("stale")  # a previous run's leftover is replaced
        api = ApiServer(core, sock)
        await api.start()
        assert stat.S_ISSOCK(sock.stat().st_mode)
        assert stat.S_IMODE(sock.stat().st_mode) == 0o660
        async with aiohttp.ClientSession(connector=aiohttp.UnixConnector(path=str(sock))) as http:
            async with http.get("http://lapis-harness/v1/chats", headers=PHONE) as resp:
                assert resp.status == 200 and (await resp.json())["chats"] == []
            async with http.ws_connect("http://lapis-harness/v1/ws", headers=HUD) as ws:
                first = await ws.receive_json()
                assert first["type"] == "state_sync" and "deck" in first["hosts"]
        await api.stop()
        assert not sock.exists()
        await core.stop()

    asyncio.run(scenario())


def test_chat_run_syncs_phone_and_device_in_both_directions(short_tmp: Path) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        # The HUD's view, as the daemon relays it: the harness's own WebSocket.
        dev = await client.ws_connect("/v1/ws", headers=HUD)
        assert json.loads((await dev.receive()).data)["type"] == "state_sync"
        ws = await client.ws_connect("/v1/ws", headers=PHONE)
        first = json.loads((await ws.receive()).data)
        assert first["type"] == "state_sync" and first["device"]["mock"] is True

        resp = await client.post("/v1/chats", json={"host_id": "arch-dev"}, headers=PHONE)
        chat = await resp.json()
        assert resp.status == 201 and chat["title"] == "New chat" and chat["active"] is False
        bad = await client.post("/v1/chats", json={"host_id": "nope"}, headers=PHONE)
        assert bad.status == 422

        # A prompt from the phone animates the device...
        resp = await client.post(
            f"/v1/chats/{chat['id']}/messages", json={"content": "Check CI"}, headers=PHONE
        )
        sent = await resp.json()
        assert resp.status == 202 and sent["message"]["role"] == "user"
        assert sent["message"]["metadata"] == {"source": "android:Pixel"}
        busy = await client.post(
            f"/v1/chats/{chat['id']}/messages", json={"content": "again"}, headers=PHONE
        )
        assert busy.status == 409 and (await busy.json())["error"]["code"] == "busy"

        # ...and the approval pops on both (strict host: the first click blocks).
        on_phone = (await ws_until(ws, "approval_request"))[-1]
        on_device = (await ws_until(dev, "approval_request"))[-1]
        assert on_phone == on_device and on_phone["chat_id"] == chat["id"]
        listed = await (await client.get("/v1/approvals", headers=PHONE)).json()
        assert listed["approvals"] == [on_phone]
        listing = await (await client.get("/v1/chats", headers=PHONE)).json()
        assert [(c["active"], c["pending_approvals"]) for c in listing["chats"]] == [(True, 1)]

        # Approving on the phone clears the modal on the device and resumes the agent.
        resp = await client.post(
            f"/v1/approvals/{on_phone['request_id']}",
            json={"decision": "approve_once"},
            headers=PHONE,
        )
        assert resp.status == 200
        cleared = (await ws_until(dev, "approval_resolved"))[-1]
        assert cleared["request_id"] == on_phone["request_id"]
        assert (cleared["decision"], cleared["by"]) == ("approve_once", "android:Pixel")
        again = await client.post(
            f"/v1/approvals/{on_phone['request_id']}", json={"decision": "deny"}, headers=PHONE
        )
        assert again.status == 404

        # Approving the next one on the device clears it on the phone.
        second = (await ws_until(ws, "approval_request"))[-1]
        await dev.send_json(
            {
                "type": "approval_decision",
                "request_id": second["request_id"],
                "decision": "approve_once",
            }  # fmt: skip
        )
        resolved = (await ws_until(ws, "approval_resolved"))[-1]
        assert (resolved["request_id"], resolved["by"]) == (second["request_id"], "device")

        # The kill switch on the device stops everything on the phone at once.
        await ws_until(ws, "approval_request")
        await dev.send_json({"type": "kill_all"})
        seen = await ws_until(ws, "task_finished")
        assert seen[-1]["status"] == "cancelled"
        assert [m for m in seen if m["type"] == "approval_resolved"][0]["by"] == "kill_switch"
        await wait_for(lambda: core.runner.agent_state == "IDLE")
        await core._chat_q.join()

        # The thread holds the whole run: prompt, tool calls, closing message.
        resp = await client.get(f"/v1/chats/{chat['id']}", headers=PHONE)
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
                headers=PHONE,
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

        # A prompt spoken on the device lands in its own thread, visible to the phone.
        await core.submit_prompt("From the keypad", host="macbook-pro", source="device")
        while True:  # skip the previous run's trailing tool/assistant rows
            created = (await ws_until(ws, "chat_message_created"))[-1]
            if created["message"]["role"] == "user":
                break
        assert created["message"]["content"] == "From the keypad"
        assert created["message"]["metadata"] == {"source": "device"}
        await ws.send_json({"type": "kill_all"})
        await ws_until(ws, "task_finished")

        patched = await client.patch(
            f"/v1/chats/{chat['id']}", json={"title": "Renamed", "archived": True}, headers=PHONE
        )
        assert (await patched.json())["archived"] is True
        assert (await client.delete(f"/v1/chats/{chat['id']}", headers=PHONE)).status == 204
        assert (await client.get(f"/v1/chats/{chat['id']}", headers=PHONE)).status == 404

        await ws.close()
        await dev.close()
        await client.close()
        await core.stop()

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
        dev = await client.ws_connect("/v1/ws", headers=HUD)
        ws = await client.ws_connect("/v1/ws", headers=PHONE)
        over_rest = await (await client.get("/v1/state", headers=PHONE)).json()
        firsts = [json.loads((await w.receive()).data) for w in (dev, ws)]
        for idle in (*firsts, over_rest):
            assert (idle["type"], idle["agent_state"], idle["action"]) == ("state_sync", "IDLE", "")

        # A whole run with nothing to approve, watched from the handheld and the phone.
        body = {"host_id": "macbook-pro", "policy_override": "autonomous"}
        chat = await (await client.post("/v1/chats", json=body, headers=PHONE)).json()
        messages = f"/v1/chats/{chat['id']}/messages"
        sent = await (
            await client.post(messages, json={"content": "Check CI"}, headers=PHONE)
        ).json()
        on_phone = await ws_until(ws, "task_finished")
        on_device = await ws_until(dev, "task_finished")
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

        # In the middle of a step: GET /v1/state names it, and so does the first frame a
        # screen gets when it (re)connects.
        entered, release = asyncio.Event(), asyncio.Event()

        class Slow(MockHostDriver):
            async def run_ssh(self, command: str, timeout_s: float) -> str:
                entered.set()
                await release.wait()
                return "ok"

        core.runner.driver_factory = Slow
        await client.post(messages, json={"content": "Once more"}, headers=PHONE)
        await asyncio.wait_for(entered.wait(), 5)
        now = await (await client.get("/v1/state", headers=PHONE)).json()
        assert (now["agent_state"], now["action"]) == ("ACTING", "ssh_exec")
        late = await client.ws_connect("/v1/ws", headers=HUD)
        late_ws = await client.ws_connect("/v1/ws", headers=PHONE)
        for first in [json.loads((await w.receive()).data) for w in (late, late_ws)]:
            assert (first["agent_state"], first["action"]) == ("ACTING", "ssh_exec")
        # The kill switch on the handheld: it is told the task was cancelled, and the
        # last state names nothing.
        await dev.send_json({"type": "kill_all"})
        killed = (await ws_until(dev, "task_finished"))[-1]
        assert (killed["status"], killed["summary"]) == ("cancelled", "Stopped by the kill switch.")
        assert (await ws_until(ws, "task_finished"))[-1] == killed
        final = (await ws_until(dev, "state_sync"))[-1]
        assert (final["agent_state"], final["action"], final["active_task"]) == ("IDLE", "", None)

        for w in (ws, late_ws, dev, late):
            await w.close()
        await client.close()
        await core.stop()

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
            headers=PHONE,
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
            "/v1/device/ssh-key", "/v1/schedules", "/v1/chats", "/v1/approvals",
        ):  # fmt: skip
            text = await (await client.get(path, headers=PHONE)).text()
            assert secret not in text and vnc not in text and "SUPERSECRET" not in text, path

        bad = await client.patch("/v1/secrets", json={"set": {"BAD NAME": "x"}}, headers=PHONE)
        assert bad.status == 422
        multi = await client.patch(
            "/v1/secrets", json={"set": {"A_KEY": "line1\nline2"}}, headers=PHONE
        )
        assert multi.status == 422

        resp = await client.patch(
            "/v1/secrets", json={"unset": ["ANTHROPIC_API_KEY"]}, headers=PHONE
        )
        assert resp.status == 200 and "ANTHROPIC_API_KEY" not in os.environ
        os.environ.pop("LAPIS_VNC_PASSWORD_macbook_pro", None)
        await client.close()
        await core.stop()

    asyncio.run(scenario())
    # Nothing the harness logged (at any level) carries a secret.
    assert secret_values_absent(caplog.text)


def test_config_hosts_schedules_and_device_endpoints(short_tmp: Path) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        dev = await client.ws_connect("/v1/ws", headers=HUD)

        # lapis.toml: validated, then written atomically.
        config = (await (await client.get("/v1/config", headers=PHONE)).json())["config"]
        config["llm"]["max_steps"] = 12
        resp = await client.put("/v1/config", json={"config": config}, headers=PHONE)
        assert resp.status == 200 and core.runner.max_steps == 12
        assert "max_steps = 12" in core.paths.config.read_text()
        config["llm"]["max_steps"] = 0
        assert (
            await client.put("/v1/config", json={"config": config}, headers=PHONE)
        ).status == 422

        # hosts.toml: add a host, see it on the device, probe it.
        hosts = (await (await client.get("/v1/hosts", headers=PHONE)).json())["hosts"]
        # The uConsole itself is a built-in host: listed, never stored, sent back harmlessly.
        assert list(hosts) == ["macbook-pro", "arch-dev", "deck"] and hosts["deck"]["builtin"]
        hosts["studio"] = {"hostname": "studio", "tailscale_ip": "100.64.9.9", "os": "linux"}
        resp = await client.put("/v1/hosts", json={"hosts": hosts}, headers=PHONE)
        assert resp.status == 200 and "studio" in load_hosts(core.paths.hosts).hosts
        assert "deck" not in load_hosts(core.paths.hosts).hosts
        assert list((await resp.json())["hosts"]) == ["macbook-pro", "arch-dev", "studio", "deck"]
        await ws_match(dev, "state_sync", lambda m: "studio" in m["hosts"])
        hosts["studio"]["policy"] = "yolo"
        assert (await client.put("/v1/hosts", json={"hosts": hosts}, headers=PHONE)).status == 422
        probe = await (await client.post("/v1/hosts/studio/probe", headers=PHONE)).json()
        assert all(probe[k]["ok"] for k in ("tailscale", "vnc", "ssh"))
        assert (await client.post("/v1/hosts/nope/probe", headers=PHONE)).status == 404
        resp = await client.put("/v1/agent/active-host", json={"host_id": "studio"}, headers=PHONE)
        assert (await resp.json()) == {"active_host": "studio"}

        # Schedules: CRUD, validation, run now.
        job = {
            "name": "Nightly",
            "schedule": "30 2 * * *",
            "host": "studio",
            "prompt": "Check",
            "enabled": True,
        }
        resp = await client.post("/v1/schedules", json=job, headers=PHONE)
        listed = {s["name"]: s for s in (await resp.json())["schedules"]}
        assert resp.status == 201 and listed["Nightly"]["next_run"] > 0
        assert (await client.post("/v1/schedules", json=job, headers=PHONE)).status == 422
        bad = await client.post(
            "/v1/schedules", json={**job, "name": "Bad", "schedule": "99 * * * *"}, headers=PHONE
        )
        assert bad.status == 422
        resp = await client.patch("/v1/schedules/Nightly", json={"enabled": False}, headers=PHONE)
        assert {s["name"]: s for s in (await resp.json())["schedules"]}["Nightly"][
            "next_run"
        ] is None
        ran = await client.post("/v1/schedules/Nightly/run", headers=PHONE)
        started = await ran.json()
        assert ran.status == 202 and started["task_id"].startswith("t_")
        await client.post("/v1/agent/kill-all", headers=PHONE)
        after = {
            s["name"]: s
            for s in (await (await client.get("/v1/schedules", headers=PHONE)).json())["schedules"]
        }
        assert after["Nightly"]["last_chat_id"] == started["chat_id"]
        assert (await client.delete("/v1/schedules/Nightly", headers=PHONE)).status == 204
        assert (await client.delete("/v1/schedules/Nightly", headers=PHONE)).status == 404

        # The LCD clock's offset follows the configured timezone (the daemon owns the
        # rest of the UI settings).
        config = (await (await client.get("/v1/config", headers=PHONE)).json())["config"]
        config["ui"]["timezone"] = "America/Los_Angeles"
        assert (await client.put("/v1/config", json={"config": config}, headers=PHONE)).ok
        state = await (await client.get("/v1/state", headers=PHONE)).json()
        assert state["utc_offset_min"] in (-480, -420)
        assert state["hosts"][-1] == "deck" and "wifi_ssid" not in state

        assert (
            await client.post("/v1/hosts/studio/nudge", json={"action": "click"}, headers=PHONE)
        ).status == 409
        await dev.close()
        await client.close()
        await core.stop()

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

        resp = await client.get(f"/v1/screenshots/{ids[-1]}.jpg", headers=PHONE)
        assert resp.status == 200 and resp.content_type == "image/jpeg"
        assert (resp.headers["X-Lapis-Width"], resp.headers["X-Lapis-Click-X"]) == ("1280", "5")
        assert (await resp.read())[:3] == b"\xff\xd8\xff"
        assert (await client.get(f"/v1/screenshots/{ids[0]}.jpg", headers=PHONE)).status == 404
        await client.close()
        await core.stop()

    asyncio.run(scenario())
