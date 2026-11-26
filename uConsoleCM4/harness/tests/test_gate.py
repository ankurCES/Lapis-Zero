"""The Policy Gate as the runner sees it: the daemon's (fail closed) and the in-process one."""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

import pytest
from conftest import wait_for
from fakedaemon import TOKEN, FakeDaemon, closed_port_url, held

from lapis_harness.agent.runner import MockPlanner, TaskRunner
from lapis_harness.config import mock_hosts
from lapis_harness.daemon_client import DaemonClient, check_daemon_url
from lapis_harness.db import LapisDb
from lapis_harness.gate import (
    TOOLS,
    ActionRequest,
    DaemonGate,
    LocalGate,
    outcome_from,
)
from lapis_harness.kvm.base import MockHostDriver
from lapis_harness.llm.base import ToolCall

ALLOW = {"allowed": True, "decision": "approve_once", "tier": 1, "reason": "", "by": "android:P"}


def test_only_a_clear_allowed_allows() -> None:
    ok = outcome_from(ALLOW)
    assert (ok.allowed, ok.decision, ok.tier, ok.by) == (True, "approve_once", 1, "android:P")
    for answer in (
        None,
        [],
        {},
        {**ALLOW, "allowed": "true"},  # not the JSON value true
        {**ALLOW, "allowed": 1},
        {**ALLOW, "decision": "deny"},  # allowed and denied at once: denied
        {**ALLOW, "decision": "maybe"},
        {**ALLOW, "decision": None},
        {"decision": "auto", "tier": 0},  # no "allowed" at all
    ):
        out = outcome_from(answer)
        assert not out.allowed, answer
    weird = outcome_from({**ALLOW, "allowed": False, "decision": "later"})
    assert (weird.decision, weird.reason) == ("deny", "unrecognised decision 'later'")
    assert outcome_from({**ALLOW, "tier": 7}).tier is None
    assert outcome_from({"allowed": False, "decision": "timeout", "by": "timeout"}).decision == (
        "timeout"
    )


def test_the_daemon_url_must_be_loopback_http() -> None:
    assert check_daemon_url("http://127.0.0.1:8421/") == "http://127.0.0.1:8421"
    assert check_daemon_url("http://localhost:9") == "http://localhost:9"
    for bad in ("https://127.0.0.1:8421", "http://192.168.1.2:8421", "http://x.ts.net", "ftp://a",
                "http://127.0.0.1:8421/v1"):  # fmt: skip
        with pytest.raises(ValueError):
            check_daemon_url(bad)


async def test_the_daemon_gate_asks_with_the_token_and_the_whole_request(tmp_path: Path) -> None:
    async with FakeDaemon(tmp_path) as daemon:
        gate = DaemonGate(daemon.client())
        req = ActionRequest(
            "ssh_exec", "mbp", command="make test", summary="make test", host_policy="strict",
            chat_id="c_1", task_id="t_1", step=3,
        )  # fmt: skip
        out = await gate.authorize(req)
        assert out.allowed and out.decision == "auto"
        assert daemon.auth_headers == [f"Bearer {TOKEN}"]
        assert daemon.asked == [
            {
                "tool": "ssh_exec", "host": "mbp", "command": "make test", "summary": "make test",
                "detail": "", "risky": False, "host_policy": "strict", "chat_id": "c_1",
                "task_id": "t_1", "step": 3, "screenshot_id": None, "click_x": None,
                "click_y": None, "preview_1bit_120x68_b64": None, "source": "harness",
            }
        ]  # fmt: skip
        # A tool the daemon's classifier does not know is not even asked about.
        out = await gate.authorize(ActionRequest("format_disk", "mbp"))
        assert (out.allowed, out.decision) == (False, "policy_deny") and len(daemon.asked) == 1
        await gate.client.close()
    assert set(TOOLS) == {
        "screenshot", "wait", "finish", "mouse_move", "mouse_click", "mouse_drag", "scroll",
        "type_text", "key_press", "ssh_exec", "agent_run",
    }  # fmt: skip


async def test_the_daemon_gate_fails_closed(tmp_path: Path) -> None:
    req = ActionRequest("screenshot", "mbp")
    # Nobody listening.
    gate = DaemonGate(DaemonClient(closed_port_url(), tmp_path / "local.token"))
    (tmp_path / "local.token").write_text(TOKEN)
    out = await gate.authorize(req)
    assert (out.allowed, out.decision, out.by) == (False, "deny", "policy")
    assert out.reason.startswith("policy gate unavailable")
    await gate.client.close()
    async with FakeDaemon(tmp_path) as daemon:
        # No token file: nothing is sent.
        gate = DaemonGate(DaemonClient(daemon.url, tmp_path / "missing.token"))
        assert not (await gate.authorize(req)).allowed and daemon.asked == []
        await gate.client.close()
        # A broken daemon, a broken answer, a slow "yes" that comes after a kill.
        gate = DaemonGate(daemon.client())
        daemon.status = 500
        assert not (await gate.authorize(req)).allowed
        daemon.status = 200

        async def garbage(body: dict[str, Any]) -> Any:
            return ["not", "an", "answer"]

        daemon.authorize = garbage
        assert not (await gate.authorize(req)).allowed

        async def crash(body: dict[str, Any]) -> Any:
            raise RuntimeError("handler crashed")

        daemon.authorize = crash  # aiohttp answers 500 with a text page
        assert not (await gate.authorize(req)).allowed
        await gate.client.close()


async def test_while_the_owner_decides_the_step_counts_as_waiting(tmp_path: Path) -> None:
    async with FakeDaemon(tmp_path) as daemon:
        release = asyncio.Event()
        daemon.authorize = lambda body: held(release, ALLOW)
        gate = DaemonGate(daemon.client(), wait_after=0.05)
        waited: list[bool] = []
        req = ActionRequest("mouse_click", "mbp", summary="Click OK", chat_id="c_9", task_id="t_9")
        ask = asyncio.create_task(gate.authorize(req, lambda: waited.append(True)))
        await wait_for(lambda: waited)
        assert [(p["chat_id"], p["summary"], p["held_by"]) for p in gate.pending()] == [
            ("c_9", "Click OK", "lapis-daemon")
        ]
        # Answers are given at the daemon, never here.
        assert not gate.resolve("", "approve_once", "device") and gate.deny_all("device") == 0
        release.set()
        assert (await ask).allowed and gate.pending() == [] and waited == [True]
        # A quick answer never shows as waiting.
        assert (await gate.authorize(req, lambda: waited.append(False))).allowed
        assert waited == [True]
        # Cancelled while it waits (the kill switch): the question goes with it.
        release.clear()
        ask = asyncio.create_task(gate.authorize(req))
        await wait_for(lambda: gate.pending())
        ask.cancel()
        with pytest.raises(asyncio.CancelledError):
            await ask
        assert gate.pending() == []
        release.set()
        await gate.client.close()


async def test_the_local_gate_is_lapis_zeros() -> None:
    frames: list[dict[str, Any]] = []
    gate = LocalGate(frames.append, frames.append)
    auto = await gate.authorize(ActionRequest("ssh_exec", "h", command="ls", host_policy="strict"))
    assert (auto.allowed, auto.decision, auto.tier) == (True, "auto", 0)
    deny = await gate.authorize(ActionRequest("ssh_exec", "h", command="mkfs /dev/sda"))
    assert (deny.allowed, deny.decision) == (False, "policy_deny")
    assert (await gate.authorize(ActionRequest("rm_everything", "h"))).decision == "policy_deny"
    assert (await gate.authorize(ActionRequest("agent_run", "h", host_policy="strict"))).allowed

    click = ActionRequest("mouse_click", "h", summary="Click OK at (1, 2)", host_policy="strict",
                          task_id="t_1", click_x=1, click_y=2)  # fmt: skip
    prepared: list[bool] = []

    async def prepare(r: ActionRequest) -> None:
        prepared.append(True)
        r.screenshot_id = 42

    click.prepare = prepare
    ask = asyncio.create_task(gate.authorize(click))
    await wait_for(lambda: frames)
    assert prepared == [True] and frames[0]["screenshot_id"] == 42
    assert (frames[0]["action"], frames[0]["risk"]) == ("vnc_click", "high")
    assert gate.resolve(frames[0]["request_id"], "approve_session", "android:Pixel")
    out = await ask
    assert (out.allowed, out.decision, out.by) == (True, "approve_session", "android:Pixel")
    assert out.approval_token
    # The session approval covers the rest of that task, not another one.
    assert (await gate.authorize(click)).decision == "approve_session"
    other = ActionRequest("key_press", "h", host_policy="strict", task_id="t_2")
    ask = asyncio.create_task(gate.authorize(other))
    await wait_for(lambda: gate.pending())
    assert gate.deny_all("device") == 1
    assert not (await ask).allowed and gate.pending() == []


async def test_the_runner_never_runs_a_step_the_daemon_did_not_allow(tmp_path: Path) -> None:
    """The mock script against a daemon that refuses everything but read-only steps."""

    class Watched(MockHostDriver):
        made: list[MockHostDriver] = []

        def __init__(self, name: str) -> None:
            super().__init__(name)
            Watched.made.append(self)

    async def only_reads(body: dict[str, Any]) -> dict[str, Any]:
        if body["tool"] == "ssh_exec" and body["command"] == "uname -a":
            return {"allowed": True, "decision": "auto", "tier": 0, "reason": "", "by": "policy"}
        return {"allowed": False, "decision": "deny", "tier": 1, "reason": "no", "by": "device"}

    db = LapisDb(tmp_path / "lapis.db")
    await db.init()
    async with FakeDaemon(tmp_path) as daemon:
        daemon.authorize = only_reads
        events: list[dict[str, Any]] = []
        finished: list[dict[str, Any]] = []
        gate = DaemonGate(daemon.client())
        runner = TaskRunner(
            db, mock_hosts(), emit_event=events.append, emit_finish=finished.append,
            driver_factory=Watched, gate=gate,
        )  # fmt: skip
        await runner.submit_task("macbook-pro", "Check CI", chat_id="c_1")
        await asyncio.wait_for(runner.wait_idle(), 5)
        # uname ran; the click was asked about (with its preview), refused, never made.
        assert [b["tool"] for b in daemon.asked] == ["ssh_exec", "screenshot", "mouse_click"]
        click = daemon.asked[2]
        assert (click["click_x"], click["click_y"], click["host_policy"]) == (160, 24, "balanced")
        assert click["preview_1bit_120x68_b64"] and click["screenshot_id"] is None
        assert click["task_id"].startswith("t_") and click["chat_id"] == "c_1"
        assert [a["kind"] for a in Watched.made[0].log] == ["ssh_exec"]
        assert [(e["kind"], e["status"]) for e in events[1:]] == [
            ("ssh_exec", "ok"),
            ("vnc_click", "denied"),
        ]
        assert finished[0]["status"] == "cancelled"

        # Nobody to ask: not one step runs on the host.
        Watched.made.clear()
        await gate.client.close()
        runner.gate = gate = DaemonGate(DaemonClient(closed_port_url(), daemon.token_file))
        runner.planner_factory = lambda: MockPlanner((ToolCall("ssh_exec", {"command": "ls"}),))
        await runner.submit_task("macbook-pro", "ls")
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert Watched.made[0].log == []
        assert events[-1]["status"] == "denied" and "unavailable" in events[-1]["detail"]
        await gate.client.close()
