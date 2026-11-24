from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

from lapis_daemon.agent.runner import MOCK_PREVIEW_B64, TaskRunner
from lapis_daemon.config import mock_hosts
from lapis_daemon.db import LapisDb
from lapis_daemon.kvm.base import MockHostDriver


def make_runner(db: LapisDb, max_steps: int) -> tuple[TaskRunner, list[dict], list[dict]]:
    events: list[dict[str, Any]] = []
    approvals: list[dict[str, Any]] = []
    runner = TaskRunner(
        db,
        mock_hosts(),
        emit_event=events.append,
        emit_approval=approvals.append,
        max_steps=max_steps,
    )
    return runner, events, approvals


async def wait_for(predicate: Any) -> None:
    async with asyncio.timeout(5):
        while not predicate():
            await asyncio.sleep(0.005)


def test_task_with_one_approval_runs_to_done(tmp_path: Path) -> None:
    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        # Five balanced steps: only the high-risk "Retry Build" click needs approval.
        runner, events, approvals = make_runner(db, max_steps=5)
        task_id = await runner.submit_task("macbook-pro", "Check CI", "balanced")
        assert task_id.startswith("t_")

        await wait_for(lambda: approvals)
        assert runner.is_running
        assert runner.agent_state == "WAITING_APPROVAL"
        request = approvals[0]
        assert request["action"] == "vnc_click"
        assert request["risk"] == "high"
        assert request["preview_1bit_120x68_b64"] == MOCK_PREVIEW_B64
        assert not runner.resolve_approval("req_nope", "approve_once")
        assert runner.resolve_approval(request["request_id"], "approve_once")

        await asyncio.wait_for(runner.wait_idle(), 5)
        assert not runner.is_running
        assert runner.agent_state == "IDLE"
        assert runner.active_task is None
        assert len(approvals) == 1
        assert (await db.get_task(task_id))["status"] == "done"

        kinds = ["prompt", "ssh_exec", "vnc_click", "type_text", "ssh_exec", "vnc_click"]
        assert [e["kind"] for e in events] == kinds
        assert all(e["type"] == "activity_event" and e["task_id"] == task_id for e in events)
        assert events[4]["detail"] == "disk usage: 42%"
        steps = await db.list_recent_steps(task_id, 50)
        assert [s["kind"] for s in steps] == kinds
        assert [s["seq"] for s in steps] == [1, 2, 3, 4, 5, 6]

        driver = runner.drivers["macbook-pro"]
        assert isinstance(driver, MockHostDriver)
        assert driver.result_lines()[0] == "ssh_exec: uname -a"
        assert not driver.is_connected

    asyncio.run(scenario())


def test_deny_cancels_and_session_approval_sticks(tmp_path: Path) -> None:
    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        runner, events, approvals = make_runner(db, max_steps=12)

        denied = await runner.submit_task("macbook-pro", "first")
        await wait_for(lambda: approvals)
        runner.resolve_approval(approvals[0]["request_id"], "deny")
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert (await db.get_task(denied))["status"] == "cancelled"
        assert events[-1]["status"] == "denied"

        # approve_session: the remaining approvals for this host auto-pass.
        done = await runner.submit_task("macbook-pro", "second")
        await wait_for(lambda: len(approvals) == 2)
        runner.resolve_approval(approvals[1]["request_id"], "approve_session")
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert len(approvals) == 2
        assert (await db.get_task(done))["status"] == "done"

    asyncio.run(scenario())


def test_kill_all_while_waiting_for_approval(tmp_path: Path) -> None:
    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        runner, _events, approvals = make_runner(db, max_steps=25)
        toasts: list[tuple[str, str]] = []
        runner.emit_toast = lambda level, text: toasts.append((level, text))

        # strict tier: the very first mutating click blocks.
        task_id = await runner.submit_task("arch-dev", "do things")
        await wait_for(lambda: approvals)
        await runner.kill_all()
        assert not runner.is_running
        assert runner.agent_state == "IDLE"
        assert (await db.get_task(task_id))["status"] == "cancelled"
        assert len(toasts) == 1
        assert not runner.resolve_approval(approvals[0]["request_id"], "approve_once")

    asyncio.run(scenario())
