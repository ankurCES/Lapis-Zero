"""Control plane: SSH set-up from the phone, and chats answered by an agent tool."""

from __future__ import annotations

import asyncio
import json
import logging
import time
from pathlib import Path
from typing import Any

import asyncssh
import pytest
from conftest import wait_for
from sshhost import FakeHost, alive, file_mode
from test_api import AUTH, TOKEN, client_for

from lapis_daemon import core as core_mod
from lapis_daemon import ipc_server
from lapis_daemon.agent.harness import Answer
from lapis_daemon.config import HostsConfig, load_config, load_hosts
from lapis_daemon.kvm import ssh_driver
from lapis_daemon.kvm.base import MockHostDriver

PASSWORD = "correct horse battery staple"
WRONG = "incorrect donkey battery"  # what the vault holds after the password changed
SECRET = "LAPIS_VNC_PASSWORD_fake"  # one typed login: Screen Sharing and Remote Login
TOOL_IDS = ["claude", "opencode", "freya", "codex", "gemini", "cursor"]


async def idle(core: Any) -> None:
    await wait_for(lambda: not core.runner.is_running)
    await core._chat_q.join()


async def thread(client: Any, chat_id: str) -> list[dict[str, Any]]:
    return (await (await client.get(f"/v1/chats/{chat_id}", headers=AUTH)).json())["messages"]


def test_the_new_routes_are_there_and_need_the_bearer_token(short_tmp: Path) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        for method, path in (
            ("GET", "/v1/device/ssh-key"),
            ("GET", "/v1/hosts/macbook-pro/agents"),
            ("POST", "/v1/hosts/macbook-pro/ssh/authorize"),
        ):
            for headers in ({}, {"Authorization": "Bearer wrong"}):
                refused = await client.request(method, path, headers=headers)
                assert refused.status == 401, (method, path)
                assert "public_key" not in await refused.text()
            assert (await client.request(method, path, headers=AUTH)).status == 200
        for path in ("/v1/hosts/nope/agents", "/v1/hosts/nope/ssh/authorize"):
            method = "GET" if path.endswith("agents") else "POST"
            assert (await client.request(method, path, headers=AUTH)).status == 404
        await client.close()
        await core.stop()

    asyncio.run(scenario())


def test_the_device_key_endpoint_shows_the_public_half_only(short_tmp: Path) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        resp = await client.get("/v1/device/ssh-key", headers=AUTH)
        raw = await resp.text()
        key = await resp.json()
        assert resp.status == 200 and set(key) == {"public_key", "fingerprint"}
        kind, blob, comment = key["public_key"].split(" ")
        assert kind == "ssh-ed25519" and comment.startswith("lapis-zero@")
        assert asyncssh.import_public_key(key["public_key"]).get_fingerprint() == key["fingerprint"]
        assert key["fingerprint"].startswith("SHA256:") and len(key["fingerprint"]) == 50

        # The private half: next to the database, 0600 in a 0700 folder, and in no answer.
        private = short_tmp / "ssh" / "id_ed25519"
        assert core.paths.ssh_key == private == core.paths.db_path.parent / "ssh" / "id_ed25519"
        assert file_mode(private) == 0o600 and file_mode(private.parent) == 0o700
        pem = private.read_text()
        assert pem.startswith("-----BEGIN OPENSSH PRIVATE KEY-----")
        assert "PRIVATE" not in raw and pem.splitlines()[2] not in raw
        again = await (await client.get("/v1/device/ssh-key", headers=AUTH)).json()
        assert again == key and private.read_text() == pem  # made once
        await client.close()
        await core.stop()

    asyncio.run(scenario())


def test_a_chat_is_bound_to_an_agent_tool_and_keeps_its_session_to_itself(short_tmp: Path) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        seen: list[dict[str, Any]] = []
        core.add_sink(seen.append)

        async def session(chat_id: str) -> Any:
            rows = await core.db.run("SELECT agent_session FROM chats WHERE id = ?", (chat_id,))
            return rows[0]["agent_session"]

        # What the picker offers (the mock host has three of the tools) and the probe.
        found = await (await client.get("/v1/hosts/macbook-pro/agents", headers=AUTH)).json()
        assert [a["id"] for a in found["agents"]] == ["claude", "opencode", "freya"]
        assert found["agents"][0] == {
            "id": "claude",
            "name": "Claude Code",
            "path": "/Users/mock/.local/bin/claude",
        }
        probe = await (await client.post("/v1/hosts/macbook-pro/probe", headers=AUTH)).json()
        assert probe["agents"] == found["agents"] and probe["ssh"]["ok"] is True

        # Chats are `lapis` unless they say otherwise; unknown agents name the valid ones.
        plain = await (
            await client.post("/v1/chats", json={"host_id": "macbook-pro"}, headers=AUTH)
        ).json()
        assert (plain["agent"], plain["workdir"]) == ("lapis", "")
        body = {"host_id": "macbook-pro", "agent": "claude", "workdir": " ~/code/app "}
        resp = await client.post("/v1/chats", json=body, headers=AUTH)
        chat = await resp.json()
        assert resp.status == 201 and (chat["agent"], chat["workdir"]) == ("claude", "~/code/app")
        for bad in ({"agent": "chatgpt"}, {"agent": None}, {"agent": ["claude"]}):
            resp = await client.post("/v1/chats", json={**body, **bad}, headers=AUTH)
            assert resp.status == 422
            assert (await resp.json())["error"]["message"] == (
                f"Unknown agent: use one of lapis, {', '.join(TOOL_IDS)}"
            )
        for bad in ("a\nb", "x" * 501, 7):
            resp = await client.post("/v1/chats", json={**body, "workdir": bad}, headers=AUTH)
            assert resp.status == 422 and "workdir" in (await resp.json())["error"]["message"]

        # A message in that chat goes to the tool: no approval, one tool row, its answer.
        prompt = "rm -rf / is what the build script does; fix it"
        sent = await client.post(
            f"/v1/chats/{chat['id']}/messages", json={"content": prompt}, headers=AUTH
        )
        assert sent.status == 202
        await idle(core)
        rows = await thread(client, chat["id"])
        assert [(m["role"], m["metadata"].get("kind")) for m in rows] == [
            ("user", None),
            ("tool", "agent_run"),
            ("assistant", None),
        ]
        assert rows[1]["content"] == "Claude Code" and rows[1]["metadata"]["status"] == "ok"
        assert rows[2]["metadata"] == {"status": "done"}
        assert rows[2]["content"] == (
            f"(mock) Claude Code started a session and would now do: {prompt}"
        )
        # Watchers got the progress line, with the tool at work shown as ACTING.
        progress = [m for m in seen if m.get("kind") == "agent_run" and m["status"] == "running"]
        assert [p["summary"] for p in progress] == ["(mock) Claude Code is working"]
        assert progress[0]["chat_id"] == chat["id"]
        states = [m["agent_state"] for m in seen if m["type"] == "state_sync"]
        assert "ACTING" in states and "WAITING_APPROVAL" not in states
        assert not [m for m in seen if m["type"] == "approval_request"]
        tasks = [m["active_task"] for m in seen if m["type"] == "state_sync" and m["active_task"]]
        assert tasks[0]["max_steps"] == 1

        # The tool's session id is kept for the follow-up and appears in no chat JSON.
        assert await session(chat["id"]) == "mock-session-1"
        await client.post(
            f"/v1/chats/{chat['id']}/messages", json={"content": "go on"}, headers=AUTH
        )
        await idle(core)
        assert (await thread(client, chat["id"]))[-1]["content"].startswith(
            "(mock) Claude Code continued a session"
        )
        for path in ("/v1/chats", f"/v1/chats/{chat['id']}", "/v1/state"):
            raw = await (await client.get(path, headers=AUTH)).text()
            assert "mock-session" not in raw and "agent_session" not in raw, path
        assert not [m for m in seen if "mock-session" in str(m) or "agent_session" in str(m)]

        # Another tool, another host or another folder is another conversation.
        for change in ({"agent": "freya"}, {"workdir": "/srv/app"}, {"host_id": "arch-dev"}):
            await core.db.run("UPDATE chats SET agent_session = 'kept' WHERE id = ?", (chat["id"],))
            resp = await client.patch(f"/v1/chats/{chat['id']}", json={"title": "T"}, headers=AUTH)
            assert await session(chat["id"]) == "kept"  # a rename is not a change of tool
            resp = await client.patch(f"/v1/chats/{chat['id']}", json=change, headers=AUTH)
            assert resp.status == 200 and change.items() <= (await resp.json()).items()
            assert await session(chat["id"]) is None, change
        resp = await client.patch(f"/v1/chats/{chat['id']}", json={"agent": "nope"}, headers=AUTH)
        assert resp.status == 422 and "lapis, claude" in (await resp.json())["error"]["message"]

        # A tool of the owner's own, added in the config, is as good as a built-in one:
        # under the autonomous policy. Of its permissions the device knows nothing, so on
        # this chat's host (arch-dev by now: strict) it is not started, and the chat says
        # what to do about it.
        config = (await (await client.get("/v1/config", headers=AUTH)).json())["config"]
        assert config["agents"] == {} and config["kvm"]["agent_run_timeout_sec"] == 1800
        config["agents"] = {"aider": {"name": "Aider", "command": "aider --message {prompt}"}}
        resp = await client.put("/v1/config", json={"config": config}, headers=AUTH)
        assert resp.status == 200
        assert (await resp.json())["config"]["agents"]["aider"]["tiers"] == ["autonomous"]
        resp = await client.patch(f"/v1/chats/{chat['id']}", json={"agent": "aider"}, headers=AUTH)
        assert resp.status == 200 and (await resp.json())["agent"] == "aider"
        seen.clear()
        sent = await client.post(
            f"/v1/chats/{chat['id']}/messages", json={"content": "x"}, headers=AUTH
        )
        assert sent.status == 202
        await idle(core)
        refused = (await thread(client, chat["id"]))[-1]
        assert refused["metadata"] == {"status": "failed"} and refused["content"] == (
            "Aider was not started: it cannot be held to the strict policy that applies to "
            "this chat on arch-dev. It can be run under autonomous: set that as this chat's "
            "policy to allow it, or pick a tool that keeps to strict: Claude Code, Gemini "
            "CLI, Cursor Agent."
        )
        assert not [m for m in seen if m.get("kind") == "agent_run"]  # nothing was run
        # The chat's own policy set to autonomous, as it says: now it runs.
        resp = await client.patch(
            f"/v1/chats/{chat['id']}", json={"policy_override": "autonomous"}, headers=AUTH
        )
        assert resp.status == 200
        await client.post(f"/v1/chats/{chat['id']}/messages", json={"content": "x"}, headers=AUTH)
        await idle(core)
        assert (await thread(client, chat["id"]))[-1]["content"].startswith("(mock) Aider started")
        # Or the owner vouches in the config for what the command line keeps to.
        await client.patch(f"/v1/chats/{chat['id']}", json={"policy_override": None}, headers=AUTH)
        config["agents"]["aider"]["tiers"] = ["autonomous", "strict"]
        resp = await client.put("/v1/config", json={"config": config}, headers=AUTH)
        assert (await resp.json())["config"]["agents"]["aider"]["tiers"] == ["strict", "autonomous"]
        await client.post(f"/v1/chats/{chat['id']}/messages", json={"content": "y"}, headers=AUTH)
        await idle(core)
        assert (await thread(client, chat["id"]))[-1]["metadata"] == {"status": "done"}
        for bad in ([], ["yolo"], "strict", None):
            config["agents"]["aider"]["tiers"] = bad
            resp = await client.put("/v1/config", json={"config": config}, headers=AUTH)
            assert resp.status == 422 and "agents.aider.tiers" in await resp.text(), bad
        config["agents"]["aider"]["tiers"] = ["strict", "autonomous"]
        # A command line that is not one is refused, and not repeated in the refusal.
        config["agents"]["bad"] = {
            "name": "Bad",
            "command": "tool --key sk-live-12345 --m={prompt}",
        }
        resp = await client.put("/v1/config", json={"config": config}, headers=AUTH)
        assert resp.status == 422 and "sk-live-12345" not in await resp.text()
        # Removed from the config while a chat still names it: the message says so.
        config["agents"] = {}
        assert (await client.put("/v1/config", json={"config": config}, headers=AUTH)).status == 200
        resp = await client.post(
            f"/v1/chats/{chat['id']}/messages", json={"content": "x"}, headers=AUTH
        )
        assert resp.status == 422 and "no longer configured" in await resp.text()

        await client.close()
        await core.stop()

    asyncio.run(scenario())


def test_the_handheld_and_schedules_keep_the_devices_own_agent(short_tmp: Path) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        # Somebody binds the handheld's own thread and a schedule's thread to a tool.
        device_chat = await core.chats.device_chat("macbook-pro")
        scheduled = await core.chats.create("macbook-pro", "Scheduled: Daily Digest")
        for chat in (device_chat, scheduled):
            resp = await client.patch(
                f"/v1/chats/{chat['id']}", json={"agent": "claude"}, headers=AUTH
            )
            assert resp.status == 200

        # A prompt typed on the handheld, and a schedule: still the device's own agent.
        await core.submit_prompt("status?", host="macbook-pro", source="device")
        assert core.runner._job is None and core.runner.active_task["max_steps"] == 25
        await core.kill_all()
        chat_id, _task = await core.run_schedule("Daily Digest")
        assert chat_id == scheduled["id"] and core.runner._job is None
        await core.kill_all()
        # The same thread, written to from the phone, does go to the tool.
        await client.post(
            f"/v1/chats/{device_chat['id']}/messages", json={"content": "x"}, headers=AUTH
        )
        assert core.runner._job is not None and core.runner._job.tool.id == "claude"
        await idle(core)
        await client.close()
        await core.stop()

    asyncio.run(scenario())


class Held(MockHostDriver):
    """A tool that answers when it is let go, and takes a moment to stop when it is not."""

    release: asyncio.Event
    asked: list[tuple[str, str]] = []  # (tool id, the session it was asked to continue)
    stopped: list[str] = []

    async def run_agent(self, tool: Any, message: str, *, session: str = "", **kw: Any) -> Answer:
        type(self).asked.append((tool.id, session))
        try:
            await type(self).release.wait()
        except asyncio.CancelledError:
            await asyncio.shield(asyncio.sleep(0.3))  # stopping it on the host
            type(self).stopped.append(message)
            raise
        return Answer("ok", f"{tool.id}-session-1")


def test_while_the_kill_switch_is_at_work_nothing_new_is_accepted(short_tmp: Path) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        Held.release, Held.asked, Held.stopped = asyncio.Event(), [], []
        core.runner.driver_factory = Held
        frames: list[dict[str, Any]] = []
        core.add_sink(frames.append)
        chats = []
        for _ in range(2):
            body = {"host_id": "macbook-pro", "agent": "claude"}
            chats.append(await (await client.post("/v1/chats", json=body, headers=AUTH)).json())
        a, b = (c["id"] for c in chats)
        said = await client.post(f"/v1/chats/{a}/messages", json={"content": "A"}, headers=AUTH)
        assert said.status == 202
        await wait_for(lambda: Held.asked == [("claude", "")])

        # The kill switch; and while task A is still being stopped a message for another
        # chat (a second phone, a schedule, an impatient thumb) and an edit of the hosts.
        kill = asyncio.create_task(client.post("/v1/agent/kill-all", headers=AUTH))
        await asyncio.sleep(0.1)
        assert Held.stopped == [] and not kill.done()
        resp = await client.post(f"/v1/chats/{b}/messages", json={"content": "B"}, headers=AUTH)
        assert resp.status == 409 and (await resp.json())["error"] == {
            "code": "busy",
            "message": "Agent busy: the last task is still being stopped",
        }
        hosts = (await (await client.get("/v1/hosts", headers=AUTH)).json())["hosts"]
        resp = await client.put("/v1/hosts", json={"hosts": hosts}, headers=AUTH)
        assert resp.status == 409 and (await resp.json())["error"]["code"] == "busy"
        assert (await kill).status == 200
        # When the kill switch has answered, the tool has stopped, and only then is
        # "all agent activity stopped" said: of task A, the only one there ever was.
        assert Held.stopped == ["A"] and Held.asked == [("claude", "")]
        assert core.state()["agent_state"] == "IDLE" and not core.runner.is_running
        done = [(f["chat_id"], f["status"]) for f in frames if f["type"] == "task_finished"]
        assert done == [(a, "cancelled")]
        assert [f["text"] for f in frames if f["type"] == "toast"] == [
            "Kill switch: all agent activity stopped"
        ]
        resp = await client.post(f"/v1/chats/{b}/messages", json={"content": "B"}, headers=AUTH)
        assert resp.status == 202  # ...and now it is taken
        Held.release.set()
        await idle(core)

        # `busy` is that state, not a word: a host that has "busy" in its name is just a
        # host that is not known, and an invalid value stays invalid.
        resp = await client.post("/v1/chats", json={"host_id": "busy-mac"}, headers=AUTH)
        assert resp.status == 422 and (await resp.json())["error"] == {
            "code": "invalid",
            "message": "Unknown host: busy-mac",
        }
        resp = await client.patch(f"/v1/chats/{a}", json={"host_id": "busy-mac"}, headers=AUTH)
        assert resp.status == 422 and (await resp.json())["error"]["code"] == "invalid"
        await client.close()
        await core.stop()

    asyncio.run(scenario())


def test_a_session_that_ends_after_the_chat_was_rebound_is_not_kept(short_tmp: Path) -> None:
    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        Held.release, Held.asked, Held.stopped = asyncio.Event(), [], []
        core.runner.driver_factory = Held
        body = {"host_id": "macbook-pro", "agent": "claude"}
        chat = (await (await client.post("/v1/chats", json=body, headers=AUTH)).json())["id"]

        async def stored() -> Any:
            rows = await core.db.run("SELECT agent, agent_session FROM chats WHERE id = ?", (chat,))
            return rows[0]["agent"], rows[0]["agent_session"]

        async def say(text: str) -> None:
            resp = await client.post(
                f"/v1/chats/{chat}/messages", json={"content": text}, headers=AUTH
            )
            assert resp.status == 202

        await say("one")
        await wait_for(lambda: Held.asked == [("claude", "")])
        # While Claude Code is at work the owner hands the chat to Gemini CLI. Claude
        # Code then finishes and names its session: Gemini CLI must not be asked to
        # continue it (it would fail on every message until the chat is changed again).
        resp = await client.patch(f"/v1/chats/{chat}", json={"agent": "gemini"}, headers=AUTH)
        assert resp.status == 200 and await stored() == ("gemini", None)
        Held.release.set()
        await idle(core)
        assert await stored() == ("gemini", None)
        await say("two")
        await idle(core)
        assert Held.asked[-1] == ("gemini", "") and await stored() == ("gemini", "gemini-session-1")
        # Unchanged binding: the session is kept and continued, as before.
        await say("three")
        await idle(core)
        assert Held.asked[-1] == ("gemini", "gemini-session-1")

        # A body key that is no setting of a chat is ignored, not an error of the server.
        for odd in ("chat_id", "self", "agent_session", "id"):
            resp = await client.patch(f"/v1/chats/{chat}", json={odd: "x"}, headers=AUTH)
            assert resp.status == 200 and (await resp.json())["id"] == chat, odd
        assert await stored() == ("gemini", "gemini-session-1")
        await client.close()
        await core.stop()

    asyncio.run(scenario())


def test_an_app_from_before_the_new_settings_does_not_wipe_them(short_tmp: Path) -> None:
    """The app writes whole objects back. A build that does not know `agents`,
    `kvm.agent_run_timeout_sec`, an agent's `tiers` or `ssh.password_env` reads the
    object, drops those keys, changes something else and saves."""

    async def scenario() -> None:
        core, client = await client_for(short_tmp)

        async def get(path: str, key: str) -> Any:
            return (await (await client.get(path, headers=AUTH)).json())[key]

        config = await get("/v1/config", "config")
        config["agents"] = {
            "aider": {"name": "Aider", "command": "aider --yes {prompt}", "tiers": ["balanced"]}
        }
        config["kvm"]["agent_run_timeout_sec"] = 7200
        assert (await client.put("/v1/config", json={"config": config}, headers=AUTH)).status == 200
        hosts = await get("/v1/hosts", "hosts")
        hosts["macbook-pro"]["ssh"]["password_env"] = "LAPIS_VNC_PASSWORD_macbook_pro"
        assert (await client.put("/v1/hosts", json={"hosts": hosts}, headers=AUTH)).status == 200

        old = await get("/v1/config", "config")
        del old["agents"], old["kvm"]["agent_run_timeout_sec"]
        old["ui"]["theme"] = "dark"
        assert (await client.put("/v1/config", json={"config": old}, headers=AUTH)).status == 200
        old_hosts = await get("/v1/hosts", "hosts")
        for host in old_hosts.values():
            del host["ssh"]["password_env"]
        old_hosts["macbook-pro"]["policy"] = "strict"
        resp = await client.put("/v1/hosts", json={"hosts": old_hosts}, headers=AUTH)
        assert resp.status == 200

        now, now_hosts = await get("/v1/config", "config"), await get("/v1/hosts", "hosts")
        assert now["ui"]["theme"] == "dark" and now_hosts["macbook-pro"]["policy"] == "strict"
        assert now["agents"] == config["agents"] and now["kvm"]["agent_run_timeout_sec"] == 7200
        assert now_hosts["macbook-pro"]["ssh"]["password_env"] == "LAPIS_VNC_PASSWORD_macbook_pro"
        assert now_hosts["arch-dev"]["ssh"]["password_env"] == ""
        # ...also on disk, where the next start reads them.
        assert load_config(core.paths.config).agents["aider"].tiers == ["balanced"]
        assert load_hosts(core.paths.hosts).hosts["macbook-pro"].ssh.password_env

        # An app that knows `agents` but not `tiers`: kept for a command line that is the
        # one the owner vouched for; a changed line is a new one, vouched for by nobody.
        del now["agents"]["aider"]["tiers"]
        resp = await client.put("/v1/config", json={"config": now}, headers=AUTH)
        assert (await resp.json())["config"]["agents"]["aider"]["tiers"] == ["balanced"]
        now["agents"]["aider"] = {"name": "Aider", "command": "aider --yes --force {prompt}"}
        resp = await client.put("/v1/config", json={"config": now}, headers=AUTH)
        assert (await resp.json())["config"]["agents"]["aider"]["tiers"] == ["autonomous"]
        # Sent empty, a setting is cleared: that is how the current app removes one.
        now["agents"] = {}
        now_hosts["macbook-pro"]["ssh"]["password_env"] = ""
        await client.put("/v1/config", json={"config": now}, headers=AUTH)
        await client.put("/v1/hosts", json={"hosts": now_hosts}, headers=AUTH)
        assert await get("/v1/config", "config") == {**now, "agents": {}}
        assert (await get("/v1/hosts", "hosts"))["macbook-pro"]["ssh"]["password_env"] == ""
        # A body that is not an object at all is refused as before, not kept.
        for bad in (None, [], "x"):
            resp = await client.put("/v1/config", json={"config": bad}, headers=AUTH)
            assert resp.status == 422
            resp = await client.put("/v1/hosts", json={"hosts": bad}, headers=AUTH)
            assert resp.status == 422
        await client.close()
        await core.stop()

    asyncio.run(scenario())


def test_a_password_where_the_name_of_its_secret_belongs_is_refused_for_vnc_too(
    short_tmp: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    passwords = ("My Mac p4ssword!", "hunter2 hunter2", "sk-ant-api03-aB3dE5gH7jK9", "PATH")

    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        hosts = (await (await client.get("/v1/hosts", headers=AUTH)).json())["hosts"]
        for field in ("vnc", "ssh"):
            for password in passwords:
                sent = json.loads(json.dumps(hosts))
                sent["macbook-pro"][field]["password_env"] = password
                resp = await client.put("/v1/hosts", json={"hosts": sent}, headers=AUTH)
                said = await resp.text()
                assert resp.status == 422 and password not in said, (field, password)
                assert f"hosts.macbook-pro.{field}.password_env" in said
                assert "not the password itself" in said
                for path in ("/v1/hosts", "/v1/secrets/status"):
                    assert password not in await (await client.get(path, headers=AUTH)).text()
        # A name is taken as before, with or without the LAPIS_ prefix the app derives.
        for name in ("LAPIS_VNC_PASSWORD_macbook_pro", "MAC_LOGIN", ""):
            hosts["macbook-pro"]["vnc"]["password_env"] = name
            resp = await client.put("/v1/hosts", json={"hosts": hosts}, headers=AUTH)
            assert resp.status == 200, name
        await client.close()
        await core.stop()

    asyncio.run(scenario())

    # A hosts.toml stored before the rule may hold such a value. It never logged anybody
    # in, so that one value is dropped; the host, its other settings and the other
    # hosts are kept. Said in one line each, which names the setting and not the value.
    path = short_tmp / "stored-hosts.toml"
    path.write_text(
        '[hosts.mac]\nhostname = "mac"\ntailscale_ip = "100.64.0.9"\npolicy = "strict"\n'
        '[hosts.mac.vnc]\nport = 5901\nusername = "alice"\npassword_env = "My Mac p4ssword!"\n'
        '[hosts.mac.ssh]\nuser = "alice"\npassword_env = "hunter2 hunter2"\n'
        '[hosts.linux]\nhostname = "linux"\ntailscale_ip = "100.64.0.8"\nos = "linux"\n'
        '[hosts.linux.vnc]\npassword_env = "LAPIS_VNC_PASSWORD_linux"\n'
    )
    capsys.readouterr()
    loaded = load_hosts(path).hosts
    mac = loaded["mac"]
    assert (mac.policy, mac.vnc.port, mac.vnc.username, mac.ssh.user) == (
        "strict", 5901, "alice", "alice",
    )  # fmt: skip
    assert (mac.vnc.password_env, mac.ssh.password_env) == ("", "")
    assert loaded["linux"].vnc.password_env == "LAPIS_VNC_PASSWORD_linux"
    printed = capsys.readouterr().err
    assert "ignoring hosts.mac.vnc.password_env: " in printed
    assert "ignoring hosts.mac.ssh.password_env: " in printed
    assert "p4ssword" not in printed and "hunter2" not in printed
    # The daemon's own start-up gets the same, instead of starting with no host at all.
    from lapis_daemon import main as daemon

    assert daemon._load(path, load_hosts, HostsConfig, HostsConfig, False).hosts == loaded
    # A file that is wrong in another way is still refused whole, without the value.
    path.write_text(path.read_text().replace('policy = "strict"', 'policy = "yolo"'))
    with pytest.raises(ValueError) as err:
        load_hosts(path)
    assert "hosts.mac.policy" in str(err.value) and "p4ssword" not in str(err.value)


def test_two_authorisations_of_one_device_take_turns(
    short_tmp: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Both would look for the key on the host, both miss it, and both add it."""
    inside: list[int] = []

    class Slow:
        ssh_login = "password"

        async def ssh_problem(self) -> None:
            return None

        async def authorize(self, public_key: str) -> bool:
            inside.append(inside[-1] + 1 if inside else 1)
            await asyncio.sleep(0.05)
            inside.append(inside[-1] - 1)
            return False

        async def disconnect(self) -> None:
            pass

    async def scenario() -> None:
        core, client = await client_for(short_tmp)
        core.mock = False
        monkeypatch.setattr(core, "_driver", lambda name, ssh_methods=None: Slow())
        answers = await asyncio.gather(
            *(client.post("/v1/hosts/macbook-pro/ssh/authorize", headers=AUTH) for _ in range(3))
        )
        assert [a.status for a in answers] == [200, 200, 200]
        assert inside == [1, 0, 1, 0, 1, 0]  # never two at once
        await client.close()
        await core.stop()

    asyncio.run(scenario())


async def live(d: Path, host: FakeHost, monkeypatch: pytest.MonkeyPatch, **ssh: Any) -> Any:
    """A core whose one host is the loopback SSH server, driven by the real drivers."""
    core, client = await client_for(d)

    async def no_tailscale(*argv: str, **_kw: Any) -> tuple[int, str]:
        return 127, ""  # never this machine's `tailscale`

    def no_llm() -> Any:
        raise AssertionError("an LLM planner was built: these chats must not need one")

    monkeypatch.setattr(core_mod, "run_cmd", no_tailscale)
    core.mock = False  # real drivers; `status` and `net` stay stand-ins
    core.runner.planner_factory = no_llm
    core.apply_hosts(HostsConfig(hosts={"fake": host.config(**ssh)}))
    return core, client


def test_ssh_is_set_up_from_the_phone_and_no_answer_carries_a_secret(
    short_tmp: Path,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
) -> None:
    caplog.set_level(logging.DEBUG)
    monkeypatch.setenv(SECRET, "")  # restored afterwards; the vault sets the real value
    bodies: list[str] = []
    frames: list[dict[str, Any]] = []  # everything the screens are sent, both sockets

    async def scenario() -> None:
        async with FakeHost(tmp_path, password=PASSWORD) as host:
            core, client = await live(short_tmp, host, monkeypatch)
            core.add_sink(frames.append)
            host.tool("claude")

            async def call(method: str, path: str, **kw: Any) -> tuple[int, Any]:
                resp = await client.request(method, path, headers=AUTH, **kw)
                bodies.append(await resp.text())
                return resp.status, await resp.json()

            # No key on the host, no password given: nothing works, and each answer
            # says what to do about it. The password was never guessed at.
            for method, path in (
                ("GET", "/v1/hosts/fake/agents"),
                ("POST", "/v1/hosts/fake/ssh/authorize"),
            ):
                status, body = await call(method, path)
                assert status == 409 and body["error"]["code"] == "ssh_unavailable"
                assert body["error"]["message"].startswith(
                    "Cannot log in to fake over SSH (PermissionDenied: Permission denied"
                )
                assert (
                    "turn on Remote Login under System Settings > General > Sharing"
                    in (body["error"]["message"])
                )
            status, probe = await call("POST", "/v1/hosts/fake/probe")
            assert status == 200 and probe["ssh"]["ok"] is False and probe["agents"] == []
            assert probe["ssh"]["detail"].startswith("PermissionDenied")
            assert host.passwords == [] and not host.authorized_keys().exists()

            # The owner types the Mac's login once: the password goes into the vault,
            # its NAME into the host. A password where the name belongs is refused
            # without being repeated.
            status, _ = await call("PATCH", "/v1/secrets", json={"set": {SECRET: PASSWORD}})
            assert status == 200
            _, hosts = await call("GET", "/v1/hosts")
            assert hosts["hosts"]["fake"]["ssh"]["password_env"] == ""
            hosts["hosts"]["fake"]["ssh"]["password_env"] = PASSWORD
            status, body = await call("PUT", "/v1/hosts", json={"hosts": hosts["hosts"]})
            assert status == 422 and "not the password itself" in body["error"]["message"]
            for wrong in ("sk-ant-api03-aB3dE5gH7jK9", "PATH", "has space"):
                hosts["hosts"]["fake"]["ssh"]["password_env"] = wrong
                status, body = await call("PUT", "/v1/hosts", json={"hosts": hosts["hosts"]})
                assert status == 422 and wrong not in body["error"]["message"], wrong
            hosts["hosts"]["fake"]["ssh"]["password_env"] = SECRET
            status, saved = await call("PUT", "/v1/hosts", json={"hosts": hosts["hosts"]})
            assert status == 200 and saved["hosts"]["fake"]["ssh"]["password_env"] == SECRET
            _, secrets = await call("GET", "/v1/secrets/status")
            assert {"name": SECRET, "is_set": True, "masked": "...aple"} in secrets["secrets"]

            # Authorise: logs in with the password, adds the key, proves the key alone.
            status, done = await call("POST", "/v1/hosts/fake/ssh/authorize")
            assert (status, done["ok"], done["already"]) == (200, True, False)
            assert done["detail"] == (
                "Added this device's key on fake (logged in by password). "
                "The key now logs in by itself."
            )
            _, key = await call("GET", "/v1/device/ssh-key")
            assert host.authorized_keys().read_text() == key["public_key"] + "\n"
            assert host.passwords == [PASSWORD]
            status, again = await call("POST", "/v1/hosts/fake/ssh/authorize")
            assert (status, again["already"]) == (200, True)
            assert again["detail"] == "This device's key was already authorised on fake."
            assert host.authorized_keys().read_text() == key["public_key"] + "\n"
            assert host.passwords == [PASSWORD]  # the key got in: no password sent again

            # The probe says how the login worked and what it found.
            status, probe = await call("POST", "/v1/hosts/fake/probe")
            assert probe["ssh"]["ok"] is True and probe["ssh"]["detail"] == "ok (device key)"
            assert probe["ssh"]["latency_ms"] >= 0 and probe["host_id"] == "fake"
            assert probe["tailscale"]["ok"] is False and probe["vnc"]["ok"] is False
            claude = {"id": "claude", "name": "Claude Code", "path": str(host.home / "bin/claude")}
            assert probe["agents"] == [claude]
            assert await call("GET", "/v1/hosts/fake/agents") == (200, {"agents": [claude]})
            # A chat with that tool, for what the screens are told about its runs.
            _, chat = await call("POST", "/v1/chats", json={"host_id": "fake", "agent": "claude"})
            said = {"content": "hello"}
            assert (await call("POST", f"/v1/chats/{chat['id']}/messages", json=said))[0] == 202
            await idle(core)

            # No login at all (the password changed): said as before. A home folder
            # that cannot be written, and a host that takes the line but still refuses
            # the key: each said in words, with what to look at.
            status, _ = await call("PATCH", "/v1/secrets", json={"set": {SECRET: WRONG}})
            host.authorized_keys().unlink()
            status, body = await call("POST", "/v1/hosts/fake/ssh/authorize")
            assert status == 409 and body["error"]["message"].startswith("Cannot log in to fake")
            # ...and a message in the tool's chat fails with the same words.
            assert (await call("POST", f"/v1/chats/{chat['id']}/messages", json=said))[0] == 202
            await idle(core)
            await call("GET", f"/v1/chats/{chat['id']}")
            assert host.passwords[-1] == WRONG  # it was sent to the host, and to nobody else
            status, _ = await call("PATCH", "/v1/secrets", json={"set": {SECRET: PASSWORD}})
            host.authorized_keys().parent.chmod(0o500)
            status, body = await call("POST", "/v1/hosts/fake/ssh/authorize")
            assert status == 409 and body["error"]["message"].startswith(
                "Logged in to fake (password), but could not write ~/.ssh/authorized_keys there"
            )
            host.authorized_keys().parent.chmod(0o700)
            host.reads_key_file = False
            status, body = await call("POST", "/v1/hosts/fake/ssh/authorize")
            assert status == 409 and body["error"]["code"] == "ssh_unavailable"
            assert body["error"]["message"].startswith(
                "This device's key is in ~/.ssh/authorized_keys on fake, but the host still "
                "refuses it (PermissionDenied"
            )
            assert host.authorized_keys().read_text() == key["public_key"] + "\n"
            host.reads_key_file = True

            # Windows hosts are not authorised this way.
            hosts["hosts"]["fake"]["os"] = "windows"
            await call("PUT", "/v1/hosts", json={"hosts": hosts["hosts"]})
            status, body = await call("POST", "/v1/hosts/fake/ssh/authorize")
            assert (
                status == 422 and "only done for macOS and Linux hosts" in body["error"]["message"]
            )

            for path in (
                "/v1/config",
                "/v1/state",
                "/v1/chats",
                "/v1/hosts",
                "/v1/device/telemetry",
            ):
                await call("GET", path)
            await client.close()
            await core.stop()
            private = core.paths.ssh_key.read_text().splitlines()

        # How the two runs ended reached the screens (the handheld's socket included)...
        finished = [f for f in frames if f["type"] == "task_finished"]
        assert [(f["status"], f["summary"][:38]) for f in finished] == [
            ("done", "answer 1"),
            ("failed", "Claude Code runs on fake over SSH. Can"),
        ]
        assert not {"task_finished", "state_sync", "activity_event"} & ipc_server.SKIP
        # ...and not one answer, error, frame or log line holds a password, the token
        # or the key.
        assert len(bodies) > 20 and len(frames) > 20
        for text in (*bodies, json.dumps(frames), caplog.text):
            assert PASSWORD not in text and "PRIVATE KEY" not in text and TOKEN not in text
            assert WRONG not in text and not any(line in text for line in private[1:-1])

    asyncio.run(scenario())


def test_a_tool_chat_runs_for_real_over_ssh_and_stops_when_told(
    short_tmp: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(ssh_driver, "PROGRESS_INTERVAL", 0.05)
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)  # no LLM key anywhere

    async def scenario() -> None:
        async with FakeHost(tmp_path) as host:
            core, client = await live(short_tmp, host, monkeypatch)
            assert core.state()["api_key_ready"] in (True, False)  # (mock status: not the point)
            key = await (await client.get("/v1/device/ssh-key", headers=AUTH)).json()
            host.authorized.append(key["public_key"].split()[1])
            host.tool("claude")
            host.tool("actor", "actor")
            config = core.config.model_copy(deep=True).model_dump()
            config["agents"] = {
                # The owner vouches that this command line keeps to "balanced" too.
                "actor": {
                    "name": "Actor",
                    "command": "actor {prompt}",
                    "tiers": ["balanced", "autonomous"],
                }
            }
            assert (
                await client.put("/v1/config", json={"config": config}, headers=AUTH)
            ).status == 200
            seen: list[dict[str, Any]] = []
            core.add_sink(seen.append)

            async def chat_with(agent: str, **more: Any) -> str:
                body = {"host_id": "fake", "agent": agent, **more}
                resp = await client.post("/v1/chats", json=body, headers=AUTH)
                assert resp.status == 201
                return (await resp.json())["id"]

            async def say(chat_id: str, content: str) -> dict[str, Any]:
                resp = await client.post(
                    f"/v1/chats/{chat_id}/messages", json={"content": content}, headers=AUTH
                )
                assert resp.status == 202, await resp.text()
                await idle(core)
                return (await thread(client, chat_id))[-1]

            # Claude Code, with a message that is trouble for a shell, in a folder.
            (host.home / "proj").mkdir()
            chat = await chat_with("claude", workdir="~/proj", policy_override="autonomous")
            nasty = 'why does `make` say $(pwd)\'s "Makefile" is missing; rm -rf /tmp/x\n'
            reply = await say(chat, nasty)
            assert (reply["role"], reply["content"]) == ("assistant", "answer 1")
            assert reply["metadata"] == {"status": "done"}
            assert (host.home / "stdin").read_bytes() == nasty.strip().encode()
            assert (host.home / "cwd").read_text().strip().endswith("/home/proj")
            flags = (host.home / "args").read_bytes().replace(b"\0", b" ").decode().strip()
            assert flags == "-p --output-format json --permission-mode bypassPermissions"
            # The follow-up continues the tool's own session.
            assert (await say(chat, "and then?"))["content"] == "answer 2"
            flags = (host.home / "args").read_bytes().replace(b"\0", b" ").decode().strip()
            assert flags.endswith("--permission-mode bypassPermissions --resume sess-1")
            assert not [m for m in seen if m["type"] == "approval_request"]

            # A tool that cannot be held to the host's policy (balanced) is not started:
            # nothing reaches the host, and the chat says what would allow it.
            ran = len(host.commands)
            reply = await say(await chat_with("opencode"), "hello")
            assert reply["metadata"] == {"status": "failed"} and reply["content"] == (
                "opencode was not started: it cannot be held to the balanced policy that "
                "applies to this chat on fake. It can be run under autonomous: set that as "
                "this chat's policy to allow it, or pick a tool that keeps to balanced: "
                "Claude Code, Freya, Gemini CLI, Actor."
            )
            assert len(host.commands) == ran
            # Not installed, and a run that fails: sentences, with status `failed`.
            reply = await say(await chat_with("opencode", policy_override="autonomous"), "hello")
            assert reply["metadata"] == {"status": "failed"} and reply["content"] == (
                "opencode is not installed on fake: no `opencode` on the PATH of the login "
                "shell there."
            )
            actor = await chat_with("actor")
            reply = await say(actor, "fail")
            assert reply["metadata"] == {"status": "failed"} and reply["content"] == (
                "Actor failed (exit status 3). Its last output:\nreading the repo\nno API key"
            )
            finished = [m for m in seen if m["type"] == "task_finished"]
            assert [f["status"] for f in finished] == ["done", "done", "failed", "failed", "failed"]

            # The kill switch while a tool is at work: back at once, the tool gone.
            resp = await client.post(
                f"/v1/chats/{actor}/messages", json={"content": "polite"}, headers=AUTH
            )
            assert resp.status == 202
            await wait_for(lambda: seen[-1].get("summary") == "started")
            assert seen[-1]["kind"] == "agent_run" and seen[-1]["status"] == "running"
            assert core.state()["agent_state"] == "ACTING"
            pids = [int(p) for p in (host.home / "pids").read_text().split()]
            assert all(alive(p) for p in pids)
            start = time.monotonic()
            resp = await client.post("/v1/agent/kill-all", headers=AUTH)
            assert resp.status == 200 and time.monotonic() - start < 1.0
            # Gone by the time the kill switch has answered, not some time after it.
            assert not any(alive(p) for p in pids)
            await core._chat_q.join()
            reply = (await thread(client, actor))[-1]
            assert reply["metadata"] == {"status": "cancelled"}
            assert reply["content"] == "Stopped by the kill switch."
            assert core.state()["agent_state"] == "IDLE"
            # Progress lines were shown but are not part of the thread.
            assert "started" not in [m["content"] for m in await thread(client, actor)]

            # SSH gone (the key is no longer accepted): the chat says what to do.
            host.authorized.clear()
            reply = await say(chat, "still there?")
            assert reply["metadata"] == {"status": "failed"}
            assert reply["content"].startswith(
                "Claude Code runs on fake over SSH. Cannot log in to fake over SSH (Permission"
            )
            assert "Remote Login" in reply["content"] and "Traceback" not in reply["content"]
            await client.close()
            await core.stop()

    asyncio.run(scenario())
