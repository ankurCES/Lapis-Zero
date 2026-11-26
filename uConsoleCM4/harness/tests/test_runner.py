from __future__ import annotations

import asyncio
import time
from pathlib import Path
from typing import Any

from conftest import wait_for

from lapis_harness.agent import harness
from lapis_harness.agent.harness import BUILTIN, Answer
from lapis_harness.agent.runner import (
    MOCK_PREVIEW_B64,
    STUCK_HINT,
    STUCK_HINT_NO_SSH,
    AgentJob,
    Busy,
    MockPlanner,
    TaskRunner,
)
from lapis_harness.config import mock_hosts
from lapis_harness.db import LapisDb
from lapis_harness.frames import FrameStore
from lapis_harness.kvm.base import MockHostDriver
from lapis_harness.llm.base import ToolCall

DONE_KINDS = ["prompt", "ssh_exec", "vnc_click", "type_text", "ssh_exec", "vnc_click", "ssh_exec"]


def make_runner(db: LapisDb, max_steps: int = 25, **kwargs: Any):
    events: list[dict[str, Any]] = []
    approvals: list[dict[str, Any]] = []
    resolved: list[dict[str, Any]] = []
    finished: list[dict[str, Any]] = []
    runner = TaskRunner(
        db,
        mock_hosts(),
        emit_event=events.append,
        emit_approval=approvals.append,
        emit_resolved=resolved.append,
        emit_finish=finished.append,
        max_steps=max_steps,
        **kwargs,
    )
    return runner, events, approvals, resolved, finished


async def approve_all(runner: TaskRunner, approvals: list, n: int, decision="approve_once") -> None:
    for i in range(n):
        await wait_for(lambda i=i: len(approvals) > i)
        assert runner.resolve_approval(approvals[i]["request_id"], decision)


def test_scripted_task_runs_to_done_through_two_approvals(tmp_path: Path) -> None:
    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        runner, events, approvals, resolved, finished = make_runner(db)
        task_id = await runner.submit_task("macbook-pro", "Check CI", "balanced", chat_id="c_1")
        assert task_id.startswith("t_")

        # Balanced tier: only the high-risk click and the non-allowlisted ssh block.
        await wait_for(lambda: approvals)
        assert runner.is_running and runner.agent_state == "WAITING_APPROVAL"
        first = approvals[0]
        assert (first["action"], first["risk"]) == ("vnc_click", "high")
        assert first["summary"] == "Click Retry Build at (840, 412)"
        assert (first["click_x"], first["click_y"], first["chat_id"]) == (840, 412, "c_1")
        assert len(first["preview_1bit_120x68_b64"]) == len(MOCK_PREVIEW_B64)  # 1020 bytes b64
        assert runner.pending == {first["request_id"]: first}
        assert not runner.resolve_approval("req_nope", "approve_once")
        assert not runner.resolve_approval(first["request_id"], "maybe")
        await approve_all(runner, approvals, 2)

        await asyncio.wait_for(runner.wait_idle(), 5)
        assert (runner.agent_state, runner.active_task, runner.pending) == ("IDLE", None, {})
        assert approvals[1]["action"] == "ssh_exec"
        assert approvals[1]["preview_1bit_120x68_b64"] is None
        assert [r["decision"] for r in resolved] == ["approve_once", "approve_once"]
        assert (await db.get_task(task_id))["status"] == "done"
        assert finished[0]["status"] == "done" and finished[0]["chat_id"] == "c_1"
        assert "Mock run complete" in finished[0]["summary"]

        assert [e["kind"] for e in events] == DONE_KINDS
        assert all(e["type"] == "activity_event" and e["task_id"] == task_id for e in events)
        assert events[4]["detail"] == "disk usage: 42%"
        assert events[2]["detail"] == "Click header"
        steps = await db.list_recent_steps(task_id, 50)
        assert [s["kind"] for s in steps] == DONE_KINDS

        driver = runner.drivers["macbook-pro"]
        assert isinstance(driver, MockHostDriver)
        assert driver.result_lines()[0] == "ssh_exec: uname -a"
        assert not driver.is_connected

    asyncio.run(scenario())


def test_deny_stops_the_task_and_session_approval_covers_the_rest(tmp_path: Path) -> None:
    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        runner, events, approvals, resolved, finished = make_runner(db)

        denied = await runner.submit_task("macbook-pro", "first")
        await wait_for(lambda: approvals)
        runner.resolve_approval(approvals[0]["request_id"], "deny", "android:Pixel", "no")
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert (await db.get_task(denied))["status"] == "cancelled"
        assert events[-1]["status"] == "denied"
        assert resolved[0] == {
            "type": "approval_resolved",
            "request_id": approvals[0]["request_id"],
            "decision": "deny",
            "by": "android:Pixel",
        }

        # approve_session: the rest of *this* task auto-passes...
        done = await runner.submit_task("macbook-pro", "second")
        await wait_for(lambda: len(approvals) == 2)
        runner.resolve_approval(approvals[1]["request_id"], "approve_session")
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert len(approvals) == 2
        assert (await db.get_task(done))["status"] == "done"

        # ...but a new task asks again.
        await runner.submit_task("macbook-pro", "third")
        await wait_for(lambda: len(approvals) == 3)
        await runner.kill_all()

    asyncio.run(scenario())


def test_kill_all_while_waiting_for_approval(tmp_path: Path) -> None:
    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        runner, _events, approvals, resolved, finished = make_runner(db)
        toasts: list[tuple[str, str]] = []
        runner.emit_toast = lambda level, text: toasts.append((level, text))

        # strict tier: the very first mutating click blocks.
        task_id = await runner.submit_task("arch-dev", "do things")
        await wait_for(lambda: approvals)
        assert approvals[0]["summary"] == "Click header at (160, 24)"
        await runner.kill_all("android:Pixel")
        assert not runner.is_running
        assert runner.agent_state == "IDLE"
        assert (await db.get_task(task_id))["status"] == "cancelled"
        assert len(toasts) == 1
        assert resolved[0]["by"] == "kill_switch" and resolved[0]["decision"] == "deny"
        assert finished[0]["status"] == "cancelled"
        assert not runner.resolve_approval(approvals[0]["request_id"], "approve_once")

    asyncio.run(scenario())


def test_step_limit_pause_resume_and_busy(tmp_path: Path) -> None:
    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        runner, events, _approvals, _resolved, finished = make_runner(db, max_steps=3)
        runner.pause()
        task_id = await runner.submit_task("macbook-pro", "limited")
        try:
            await runner.submit_task("macbook-pro", "second")
        except Busy as exc:
            assert str(exc) == "Agent busy: a task is already running"
        else:
            raise AssertionError("a second task must be refused")
        try:
            await runner.submit_task("nope", "x")
        except ValueError as exc:
            assert "Unknown host" in str(exc)
        runner.pause()
        await wait_for(lambda: runner.agent_state == "PAUSED")
        runner.resume()
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert (await db.get_task(task_id))["status"] == "stopped"
        assert "3-step limit" in finished[0]["summary"]
        assert [e["kind"] for e in events] == ["prompt", "ssh_exec", "vnc_click", "type_text"]

    asyncio.run(scenario())


def test_frames_are_stored_and_linked_to_steps(tmp_path: Path) -> None:
    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        frames = FrameStore(db, tmp_path / "frames")
        runner, events, approvals, _resolved, _finished = make_runner(db, frames=frames)
        await runner.submit_task("macbook-pro", "frames")
        await wait_for(lambda: approvals)
        # The approval points at the frame the click will land on.
        shot = await frames.get(approvals[0]["screenshot_id"])
        assert shot is not None and shot["path"].read_bytes()[:3] == b"\xff\xd8\xff"
        await approve_all(runner, approvals, 2)
        await asyncio.wait_for(runner.wait_idle(), 5)
        ids = [e["screenshot_id"] for e in events if e["kind"] in ("vnc_click", "type_text")]
        assert ids == sorted(ids) and all(ids)
        clicked = await frames.get(ids[0])
        assert (clicked["click_x"], clicked["click_y"]) == (160, 24)

    asyncio.run(scenario())


def test_hard_denylist_secret_typing_and_repeated_errors(tmp_path: Path) -> None:
    summary, _detail, inspected, risky = TaskRunner.describe(
        ToolCall("type_text", {"text": "hunter2", "intent": "Enter password", "risk": "high"})
    )
    assert summary == "type (hidden)" and inspected == "hunter2" and risky
    assert TaskRunner.describe(ToolCall("mouse_click", {"x": 1, "y": 2, "intent": "Delete repo"}))[
        3
    ]
    assert not TaskRunner.describe(ToolCall("mouse_click", {"x": 1, "y": 2, "intent": "Open tab"}))[
        3
    ]

    class Broken(MockHostDriver):
        async def run_ssh(self, command: str, timeout_s: float) -> str:
            raise ConnectionError("host unreachable")

    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        script = (ToolCall("ssh_exec", {"command": "rm -rf / --no-preserve-root"}),)
        runner, events, approvals, _resolved, finished = make_runner(
            db, planner_factory=lambda: MockPlanner(script)
        )
        await runner.submit_task("macbook-pro", "wipe", "autonomous")
        await asyncio.wait_for(runner.wait_idle(), 5)
        # Never offered for approval, never executed, even on the autonomous tier.
        assert approvals == [] and events[-1]["status"] == "denied"
        assert "hard denylist" in events[-1]["detail"]
        assert finished[0]["status"] == "cancelled"

        loop = MockPlanner((ToolCall("ssh_exec", {"command": "ls"}),), loop=True)
        runner, events, _a, _r, finished = make_runner(
            db, planner_factory=lambda: loop, driver_factory=Broken
        )
        await runner.submit_task("macbook-pro", "unreachable")
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert finished[0]["status"] == "failed"
        assert [e["status"] for e in events[1:]] == ["error"] * 3

    asyncio.run(scenario())


def test_nudge_only_while_paused(tmp_path: Path) -> None:
    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        runner, events, approvals, _resolved, _finished = make_runner(db)
        try:
            await runner.nudge("macbook-pro", "click", {"x": 1, "y": 1})
        except ValueError as exc:
            assert "No task" in str(exc)
        await runner.submit_task("macbook-pro", "assist")
        await wait_for(lambda: approvals)  # WAITING_APPROVAL counts as an assist state
        await runner.nudge("macbook-pro", "click", {"x": 5, "y": 6})
        assert events[-1]["kind"] == "nudge" and events[-1]["summary"] == "click(5, 6)"
        await runner.kill_all()

    asyncio.run(scenario())


def test_stuck_detection_ignores_small_real_changes(tmp_path: Path) -> None:
    """Typing in a terminal leaves the 64-bit dHash unchanged; that is not a stuck loop."""
    from lapis_harness.kvm.base import Frame

    runner, *_ = make_runner(LapisDb(tmp_path / "lapis.db"))
    blank = bytes(128 * 72)

    def frame(fine: bytes | None) -> Frame:
        return Frame(b"jpeg", 1280, 720, dhash=0x1234, fine=fine)

    # Same coarse hash, but each frame shows a few more changed pixels: never stuck.
    for n in range(1, 8):
        assert not runner._screen_stuck(frame(bytes([200] * (n * 6)) + blank[n * 6 :]))
    assert not runner.stuck.should_pause

    # A screen that truly does not react: the third identical frame raises the hint.
    assert [runner._screen_stuck(frame(blank)) for _ in range(4)] == [False, False, True, False]

    # Drivers without a fine thumbnail fall back to the plain dHash rule.
    runner.stuck.reset()
    assert [runner._screen_stuck(frame(None)) for _ in range(3)] == [False, False, True]


def test_approval_shows_the_literal_text_and_keys(tmp_path: Path) -> None:
    """Whoever approves must see what will be typed, not just the model's description."""

    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        script = (
            ToolCall("type_text", {"text": "git push --force", "intent": "Publish the branch"}),
            ToolCall("key_press", {"combo": "ctrl+shift+q", "intent": "Quit the app"}),
            ToolCall("type_text", {"text": "hunter2", "intent": "Enter password", "risk": "high"}),
        )
        runner, events, approvals, _resolved, _finished = make_runner(
            db, planner_factory=lambda: MockPlanner(script)
        )
        await runner.submit_task("arch-dev", "strict host")  # strict: every mutation asks
        await approve_all(runner, approvals, 3)
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert [a["summary"] for a in approvals] == [
            "type 'git push --force' (Publish the branch)",
            "key ctrl+shift+q (Quit the app)",
            "type (hidden) (Enter password)",  # a declared secret is never echoed
        ]
        assert [a["action"] for a in approvals] == ["vnc_type", "vnc_key", "vnc_type"]
        assert "hunter2" not in str(events) and "hunter2" not in str(approvals)

    asyncio.run(scenario())


# -- a host without a working SSH login is driven through the screen only ------------


class NoShell(MockHostDriver):
    """A host the device cannot log in to: the shell must never be touched."""

    why = "PermissionDenied: Permission denied for user alice on host 100.64.10.21"

    async def ssh_problem(self) -> str | None:
        return self.why

    async def run_ssh(self, command: str, timeout_s: float) -> str:
        raise AssertionError("run_ssh must not be called on a host without SSH")

    async def run_agent(self, *args: Any, **kwargs: Any) -> Any:
        raise AssertionError("run_agent must not be called on a host without SSH")


class Watched(MockPlanner):
    """Remembers what it was told: `ssh` at start, and every result handed back."""

    def __init__(self, script: tuple[ToolCall, ...]) -> None:
        super().__init__(script)
        self.ssh_at_start: bool | None = None
        self.results: list[Any] = []

    async def start(self, task: str, **kwargs: Any) -> None:
        self.ssh_at_start = self.ssh
        await super().start(task, **kwargs)

    async def next(self, result: Any) -> ToolCall:
        self.results.append(result)
        return await super().next(result)


def test_a_host_without_ssh_is_driven_through_the_screen_alone(tmp_path: Path) -> None:
    script = (
        ToolCall("ssh_exec", {"command": "uname -a"}),  # a model may still name it
        ToolCall("mouse_click", {"x": 5, "y": 6, "intent": "Open Terminal"}),
        ToolCall("type_text", {"text": "uname -a", "intent": "Type the command"}),
    )

    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        planners: list[Watched] = []

        def planner() -> Watched:
            planners.append(Watched(script))
            return planners[-1]

        runner, events, approvals, _resolved, finished = make_runner(
            db, planner_factory=planner, driver_factory=NoShell
        )
        task_id = await runner.submit_task("macbook-pro", "which kernel?")
        await asyncio.wait_for(runner.wait_idle(), 5)

        # The planner was told before it started, so its prompt and tool list fit.
        assert planners[0].ssh_at_start is False
        # The owner sees why, once; then the refused call; then the GUI steps, which work.
        assert [(e["kind"], e["status"]) for e in events] == [
            ("prompt", "ok"),
            ("ssh_exec", "error"),
            ("ssh_exec", "ok"),
            ("vnc_click", "ok"),
            ("type_text", "ok"),
        ]
        assert events[1]["summary"] == "SSH unavailable: using the screen only"
        assert events[1]["detail"] == NoShell.why
        # What the model reads when it names the shell anyway: guidance, as an error.
        refused = planners[0].results[1]
        assert refused.is_error and refused.text == (
            f"SSH is not available on this host ({NoShell.why}). "
            "Do not call ssh_exec again: use the screen."
        )
        assert planners[0].results[2].jpeg and not planners[0].results[2].is_error
        assert approvals == [] and finished[0]["status"] == "done"
        assert (await db.get_task(task_id))["status"] == "done"
        assert [s["kind"] for s in await db.list_recent_steps(task_id, 9)][1] == "ssh_exec"

        # A host whose login works: the planner keeps its shell.
        runner, events, *_ = make_runner(db, planner_factory=planner)
        await runner.submit_task("macbook-pro", "which kernel?")
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert planners[1].ssh_at_start is True and events[1]["detail"].startswith("Linux mock")

    asyncio.run(scenario())
    assert "ssh_exec" in STUCK_HINT and "ssh_exec" not in STUCK_HINT_NO_SSH


# -- a chat's message answered by an agent tool on the host ---------------------------


def forbidden_planner() -> Any:
    raise AssertionError("a chat bound to a tool must not build the LLM planner")


class Recording(MockHostDriver):
    """Answers from a script of `Answer`s and records how it was asked."""

    answers: list[Answer] = []
    asked: list[dict[str, Any]] = []
    states: list[str] = []
    runner: TaskRunner | None = None

    async def run_agent(self, tool: Any, message: str, **kwargs: Any) -> Answer:
        on_progress = kwargs.pop("on_progress")
        type(self).asked.append({"tool": tool.id, "message": message, **kwargs})
        assert self.runner is not None
        type(self).states.append(self.runner.agent_state)
        on_progress("reading files")
        on_progress("editing main.py")
        return type(self).answers.pop(0)


def tool_runner(db: LapisDb, driver: Any, answers: list[Answer]) -> tuple[Any, ...]:
    driver.answers, driver.asked, driver.states = answers, [], []
    made = make_runner(db, planner_factory=forbidden_planner, driver_factory=driver)
    driver.runner = made[0]
    return made


def test_a_message_is_handed_to_the_chats_agent_tool(tmp_path: Path) -> None:
    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        remembered: list[str] = []

        async def remember(session: str) -> None:
            remembered.append(session)

        job = AgentJob(BUILTIN["claude"], "", "~/code", 600.0, remember)
        runner, events, approvals, _resolved, finished = tool_runner(
            db, Recording, [Answer("Fixed the test.", "sess-1")]
        )
        # A strict host and a message that reads like the worst shell command: the
        # owner's own words are not put up for approval, and the denylist is for
        # commands the model writes, not for prose handed to a tool.
        prompt = "please rm -rf / the build folder; sudo make install"
        task_id = await runner.submit_task("arch-dev", prompt, chat_id="c_1", agent=job)
        assert runner.active_task["max_steps"] == 1
        await asyncio.wait_for(runner.wait_idle(), 5)

        assert Recording.asked == [
            {
                "tool": "claude",
                "message": prompt,
                "session": "",
                "tier": "strict",  # carried as the tool's own permission mode
                "workdir": "~/code",
                "timeout_s": 600.0,
            }
        ]
        assert approvals == [] and Recording.states == ["ACTING"]
        assert [(e["kind"], e["status"], e["summary"]) for e in events] == [
            ("prompt", "ok", prompt),
            ("agent_run", "running", "reading files"),
            ("agent_run", "running", "editing main.py"),
            ("agent_run", "ok", "Claude Code"),
        ]
        assert all(e["chat_id"] == "c_1" and e["task_id"] == task_id for e in events)
        assert events[-1]["detail"] == "Fixed the test."
        # Progress lines are shown, not stored: the SD card sees two rows per message.
        assert [s["kind"] for s in await db.list_recent_steps(task_id, 9)] == [
            "prompt",
            "agent_run",
        ]
        assert finished == [
            {
                "type": "task_finished",
                "task_id": task_id,
                "chat_id": "c_1",
                "host": "arch-dev",
                "status": "done",
                "summary": "Fixed the test.",
            }
        ]
        assert remembered == ["sess-1"] and runner.agent_state == "IDLE"

        # The follow-up continues that session; the same id is not stored again. The
        # step limit is for the LLM's loop: it does not cut a tool's answer off.
        job = AgentJob(BUILTIN["claude"], "sess-1", "", 600.0, remember)
        runner, _e, _a, _r, finished = tool_runner(db, Recording, [Answer("Docs.", "sess-1")])
        runner.max_steps = 1
        await runner.submit_task("macbook-pro", "now the docs", "autonomous", agent=job)
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert Recording.asked[0]["session"] == "sess-1"
        assert Recording.asked[0]["tier"] == "autonomous" and remembered == ["sess-1"]
        assert (finished[0]["status"], finished[0]["summary"]) == ("done", "Docs.")

    asyncio.run(scenario())


def test_a_failed_tool_run_fails_the_task_with_what_the_tool_said(tmp_path: Path) -> None:
    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        remembered: list[str] = []

        async def remember(session: str) -> None:
            remembered.append(session)

        said = "Claude Code failed (exit status 1). Its last output:\nNot logged in"
        runner, events, _a, _r, finished = tool_runner(db, Recording, [Answer(said, "", True)])
        job = AgentJob(BUILTIN["claude"], "", "", 600.0, remember)
        task_id = await runner.submit_task("macbook-pro", "go", agent=job)
        await asyncio.wait_for(runner.wait_idle(), 5)
        # One run, reported as it ended: no second try, no "repeated errors".
        assert len(Recording.asked) == 1 and remembered == []
        assert (finished[0]["status"], finished[0]["summary"]) == ("failed", said)
        assert (events[-1]["kind"], events[-1]["status"]) == ("agent_run", "error")
        assert (await db.get_task(task_id))["status"] == "failed"

        # A run that cannot even start (the connection drops, the message does not fit on
        # a command line) is a sentence too, never an exception's name.
        class Broken(MockHostDriver):
            async def run_agent(self, *args: Any, **kwargs: Any) -> Answer:
                raise ValueError("the message is too long to hand over on a command line")

        runner, events, _a, _r, finished = make_runner(
            db, planner_factory=forbidden_planner, driver_factory=Broken
        )
        await runner.submit_task("macbook-pro", "go", agent=job)
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert (finished[0]["status"], finished[0]["summary"]) == (
            "failed",
            "Claude Code could not be run: the message is too long to hand over on a command line",
        )

        # The tool has dropped the session this chat was continuing: the message starts
        # a new one, and the new id replaces the old.
        lost = Answer("No conversation found with session ID: old", "", True, lost_session=True)
        runner, _e, _a, _r, finished = tool_runner(db, Recording, [lost, Answer("Hello.", "new")])
        job = AgentJob(BUILTIN["claude"], "old", "", 600.0, remember)
        await runner.submit_task("macbook-pro", "hi again", agent=job)
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert [a["session"] for a in Recording.asked] == ["old", ""]
        assert (finished[0]["status"], finished[0]["summary"]) == ("done", "Hello.")
        assert remembered == ["new"]
        # ...and if the fresh start fails too, the stale id is at least forgotten.
        runner, _e, _a, _r, finished = tool_runner(db, Recording, [lost, Answer("nope", "", True)])
        await runner.submit_task("macbook-pro", "hi again", agent=job)
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert finished[0]["status"] == "failed" and remembered == ["new", ""]

    asyncio.run(scenario())


def test_a_tool_chat_needs_ssh_and_says_so_at_once(tmp_path: Path) -> None:
    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        runner, events, _a, _r, finished = make_runner(
            db, planner_factory=forbidden_planner, driver_factory=NoShell
        )
        job = AgentJob(BUILTIN["opencode"])
        await runner.submit_task("macbook-pro", "go", "autonomous", agent=job)
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert [e["kind"] for e in events] == ["prompt"]  # nothing was attempted
        assert finished[0]["status"] == "failed" and finished[0]["summary"] == (
            "opencode runs on macbook-pro over SSH. Cannot log in to macbook-pro over SSH "
            f"({NoShell.why}). On the Mac, turn on Remote Login under System Settings > "
            "General > Sharing. Then enter that computer's user name and password in its "
            "host form and authorise this device there."
        )
        assert "Traceback" not in finished[0]["summary"]

    asyncio.run(scenario())


def test_only_a_chat_bound_to_a_tool_can_run_one(tmp_path: Path) -> None:
    """A model that names `agent_run` itself gets nowhere: no tool, no approval bypass."""

    class Never(MockHostDriver):
        async def run_agent(self, *args: Any, **kwargs: Any) -> Any:
            raise AssertionError("the model must not be able to start an agent tool")

    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        script = (ToolCall("agent_run", {"agent": "claude", "prompt": "rm -rf ~", "name": "x"}),)
        watched = Watched(script)
        runner, events, approvals, _r, finished = make_runner(
            db, planner_factory=lambda: watched, driver_factory=Never
        )
        await runner.submit_task("arch-dev", "be helpful")  # strict tier
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert watched.results[1].is_error and watched.results[1].text == (
            "Unknown tool 'agent_run'."
        )
        # Not even put to the gate, where it would pass as the owner's own message.
        assert approvals == [] and events[-1]["status"] == "error"
        assert finished[0]["status"] == "done"

    asyncio.run(scenario())


def test_the_kill_switch_stops_a_tool_run(tmp_path: Path) -> None:
    class Endless(MockHostDriver):
        stopped = False

        async def run_agent(self, *args: Any, **kwargs: Any) -> Any:
            try:
                kwargs["on_progress"]("thinking")
                await asyncio.sleep(3600)
            finally:
                type(self).stopped = True

    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        runner, events, _a, _r, finished = make_runner(
            db, planner_factory=forbidden_planner, driver_factory=Endless
        )
        task_id = await runner.submit_task("macbook-pro", "go", agent=AgentJob(BUILTIN["freya"]))
        await wait_for(lambda: events[-1]["status"] == "running")
        assert runner.agent_state == "ACTING" and runner.is_running
        await runner.kill_all("android:Pixel")
        assert Endless.stopped and runner.agent_state == "IDLE"
        assert finished[0]["status"] == "cancelled"
        assert (await db.get_task(task_id))["status"] == "cancelled"
        assert not runner.drivers["macbook-pro"].is_connected

    asyncio.run(scenario())


def test_a_tool_is_not_run_under_a_policy_it_cannot_be_held_to(tmp_path: Path) -> None:
    """A tool's chat is not put up for approval step by step, so the tool's own
    permission settings have to stand in for the tier. Without any for it: not run."""

    class Never(MockHostDriver):
        made = 0

        def __init__(self, name: str) -> None:
            type(self).made += 1
            super().__init__(name)

        async def run_agent(self, *args: Any, **kwargs: Any) -> Any:
            raise AssertionError("the tool was run under a policy it cannot be held to")

    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        tools = harness.all_tools({})
        aider = harness.custom("aider", "Aider", "aider --yes --message {prompt}")
        vouched = harness.custom("safe", "Safe", "safe {prompt}", ["strict", "autonomous"])
        # (host, the chat's own policy or None for the host's, tool) -> the tier it is under
        refused = [
            ("macbook-pro", None, BUILTIN["opencode"], "balanced"),  # the host's policy
            ("arch-dev", None, BUILTIN["opencode"], "strict"),
            ("arch-dev", None, BUILTIN["freya"], "strict"),
            ("macbook-pro", "strict", BUILTIN["codex"], "strict"),  # the chat's override
            ("macbook-pro", None, BUILTIN["cursor"], "balanced"),
            ("macbook-pro", None, aider, "balanced"),
            ("macbook-pro", None, vouched, "balanced"),
        ]
        for host, override, tool, tier in refused:
            runner, events, approvals, _r, finished = make_runner(
                db, planner_factory=forbidden_planner, driver_factory=Never
            )
            job = AgentJob(tool, tools=tools)
            task_id = await runner.submit_task(host, "rm -rf build", override, agent=job)
            await asyncio.wait_for(runner.wait_idle(), 5)
            assert [e["kind"] for e in events] == ["prompt"] and approvals == []
            assert finished[0]["status"] == "failed", tool.id
            assert finished[0]["summary"] == harness.refusal(tool, tier, host, tools)
            assert finished[0]["summary"].startswith(
                f"{tool.name} was not started: it cannot be held to the {tier} policy"
            )
            assert (await db.get_task(task_id))["status"] == "failed"
        assert Never.made == 0  # not even a connection to the host

        # Under a tier the tool does have a setting for, it runs.
        allowed = [
            ("macbook-pro", "autonomous", BUILTIN["opencode"]),
            ("macbook-pro", None, BUILTIN["freya"]),
            ("arch-dev", None, BUILTIN["cursor"]),
            ("arch-dev", None, vouched),
        ]
        for host, override, tool in allowed:
            runner, _e, _a, _r, finished = tool_runner(db, Recording, [Answer("ok")])
            await runner.submit_task(host, "go", override, agent=AgentJob(tool, tools=tools))
            await asyncio.wait_for(runner.wait_idle(), 5)
            assert finished[0]["status"] == "done" and len(Recording.asked) == 1, tool.id
            assert Recording.asked[0]["tier"] in tool.tiers

    asyncio.run(scenario())


class SlowToStop(MockHostDriver):
    """A tool that runs until it is stopped, and whose stopping takes a moment: on a real
    host that is TERM, a grace period, KILL, and closing the connection."""

    log: list[str] = []
    stop_s = 0.2
    left = harness.LEFT_NOTHING  # what the host confirmed of the stop

    async def run_agent(self, tool: Any, message: str, **kwargs: Any) -> Answer:
        type(self).log.append(f"start {message}")
        try:
            await asyncio.sleep(3600)
        finally:
            stopping = asyncio.ensure_future(asyncio.sleep(self.stop_s))
            while not stopping.done():  # (as the SSH driver: not cut short by a cancel)
                try:
                    await asyncio.shield(stopping)
                except asyncio.CancelledError:
                    type(self).log.append("cancelled again")
            self.left_running = type(self).left
            type(self).log.append(f"stopped {message}")
        return Answer("never")

    async def disconnect(self) -> None:
        await asyncio.sleep(0.05)  # closing the session is not instant either
        await super().disconnect()
        type(self).log.append("disconnected")


def test_the_kill_switch_returns_when_everything_has_stopped_and_not_before(
    tmp_path: Path,
) -> None:
    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        SlowToStop.log, SlowToStop.left = [], harness.LEFT_NOTHING
        runner, events, _a, _r, finished = make_runner(
            db, planner_factory=forbidden_planner, driver_factory=SlowToStop
        )
        toasts: list[str] = []
        runner.emit_toast = lambda level, text: toasts.append(text)
        job = AgentJob(BUILTIN["claude"])
        await runner.submit_task("macbook-pro", "A", agent=job)
        await wait_for(lambda: SlowToStop.log == ["start A"])

        # The owner hits the kill switch. While the tool is still being stopped: a second
        # press, from another screen, and a new message.
        start = time.monotonic()
        first = asyncio.create_task(runner.kill_all("device"))
        await asyncio.sleep(0.05)
        assert runner.is_running and not first.done()  # being stopped is not stopped
        second = asyncio.create_task(runner.kill_all("android:Pixel"))
        await asyncio.sleep(0.05)
        try:
            await runner.submit_task("macbook-pro", "B", agent=job)
        except Busy as exc:
            assert str(exc) == "Agent busy: the last task is still being stopped"
        else:
            raise AssertionError("a task was accepted while the last one was being stopped")
        assert not first.done() and not second.done() and runner.agent_state != "IDLE"
        await asyncio.gather(first, second)
        # Both presses return when the tool has stopped and the session is closed, the
        # second one having waited for the first instead of cutting its stop short.
        assert time.monotonic() - start >= SlowToStop.stop_s
        assert SlowToStop.log == ["start A", "stopped A", "disconnected"]
        assert not runner.is_running and runner.agent_state == "IDLE"
        assert [(f["status"], f["summary"]) for f in finished] == [
            ("cancelled", "Stopped by the kill switch.")
        ]
        assert toasts == ["Kill switch: all agent activity stopped"]  # once, and true

        # Afterwards the agent takes work again; whoever asked for the kill and went away
        # (a phone that dropped its request) does not take the kill with it.
        await runner.submit_task("macbook-pro", "C", agent=job)
        await wait_for(lambda: SlowToStop.log[-1] == "start C")
        gone = asyncio.create_task(runner.kill_all("android:Pixel"))
        await asyncio.sleep(0.05)
        gone.cancel()
        await asyncio.gather(gone, return_exceptions=True)
        assert runner.is_running
        await wait_for(lambda: not runner.is_running)
        assert SlowToStop.log[-2:] == ["stopped C", "disconnected"]
        assert "cancelled again" not in SlowToStop.log and runner.agent_state == "IDLE"

        # What the kill switch says is what the host confirmed, and no more.
        said = {
            harness.LEFT_STARTED: (
                "Stopped by the kill switch, but the programs that Claude Code had started "
                "on macbook-pro could not be checked: some may still be running there.",
                "Kill switch: programs started by Claude Code may still be running on macbook-pro",
            ),
            harness.LEFT_UNKNOWN: (
                "Stopped on this device by the kill switch, but macbook-pro did not confirm "
                "that Claude Code was stopped: it may still be running there.",
                "Kill switch: Claude Code may still be running on macbook-pro",
            ),
        }
        for left, (summary, toast) in said.items():
            SlowToStop.log, SlowToStop.left = [], left
            finished.clear()
            toasts.clear()
            await runner.submit_task("macbook-pro", "D", agent=job)
            await wait_for(lambda: SlowToStop.log == ["start D"])
            await runner.kill_all()
            assert [(f["status"], f["summary"]) for f in finished] == [("cancelled", summary)]
            assert toasts == [toast]
        # Nothing running at all: the plain words, as before.
        toasts.clear()
        await runner.kill_all()
        assert toasts == ["Kill switch: all agent activity stopped"]

    asyncio.run(scenario())


def test_a_task_cancelled_twice_still_closes_its_session(tmp_path: Path) -> None:
    """The teardown (modifier keys released, sessions closed) survives a second
    cancellation of the task: whatever sends one, the host is not left half held."""

    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        SlowToStop.log, SlowToStop.left = [], harness.LEFT_NOTHING
        runner, _e, _a, _r, _f = make_runner(
            db, planner_factory=forbidden_planner, driver_factory=SlowToStop
        )
        await runner.submit_task("macbook-pro", "A", agent=AgentJob(BUILTIN["claude"]))
        await wait_for(lambda: SlowToStop.log == ["start A"])
        task = runner._task
        task.cancel()
        await wait_for(lambda: SlowToStop.log[-1] == "stopped A")  # now in the teardown
        task.cancel()
        await asyncio.gather(task, return_exceptions=True)
        await wait_for(lambda: SlowToStop.log[-1] == "disconnected", 2)
        assert not runner.drivers["macbook-pro"].is_connected
        await runner.kill_all()

    asyncio.run(scenario())


# -- `action`: what the agent is doing on the host right now, for the screens ---------


def test_the_step_being_executed_is_named_while_it_runs_and_only_then(tmp_path: Path) -> None:
    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        timeline: list[Any] = []  # state frames as (agent_state, action), events as their kind

        def watch(runner: TaskRunner) -> None:
            timeline.clear()
            runner.emit_state = lambda: timeline.append((runner.agent_state, runner.action))
            runner.emit_event = lambda event: timeline.append(event["kind"])

        runner, *_ = make_runner(db)
        watch(runner)
        assert runner.action == ""
        await runner.submit_task("macbook-pro", "Check CI", "autonomous")
        await asyncio.wait_for(runner.wait_idle(), 5)
        # Each step: named when it starts, cleared when it has returned, and only then
        # reported as an activity event. Thinking names nothing, nor does the end.
        expected: list[Any] = ["prompt", ("THINKING", "")]
        for kind in DONE_KINDS[1:]:
            expected += [("THINKING", ""), ("ACTING", kind), ("ACTING", ""), kind]
        assert timeline == [*expected, ("THINKING", ""), ("IDLE", "")]

        # Waiting for the owner is not acting: the step is named once it is allowed.
        key = (ToolCall("key_press", {"combo": "Return", "intent": "Confirm"}),)
        runner, _events, approvals, *_ = make_runner(db, planner_factory=lambda: MockPlanner(key))
        watch(runner)
        await runner.submit_task("arch-dev", "confirm")  # strict tier
        await wait_for(lambda: approvals)
        assert (runner.agent_state, runner.action) == ("WAITING_APPROVAL", "")
        runner.resolve_approval(approvals[0]["request_id"], "approve_once")
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert [x for x in timeline if isinstance(x, tuple)][2:] == [
            ("WAITING_APPROVAL", ""),
            ("ACTING", "vnc_key"),
            ("ACTING", ""),
            ("THINKING", ""),
            ("IDLE", ""),
        ]

        # A step that fails is over too, before its error is reported.
        class Broken(MockHostDriver):
            async def run_ssh(self, command: str, timeout_s: float) -> str:
                raise ConnectionError("host unreachable")

        ls = (ToolCall("ssh_exec", {"command": "ls"}),)
        runner, *_ = make_runner(db, planner_factory=lambda: MockPlanner(ls), driver_factory=Broken)
        watch(runner)
        await runner.submit_task("macbook-pro", "list")
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert timeline[2:6] == [
            ("THINKING", ""),
            ("ACTING", "ssh_exec"),
            ("ACTING", ""),
            "ssh_exec",
        ]
        assert all(action == "" for state, action in timeline[6:] if state != "ACTING")

        # A mock step returns at once. Its pause counts as the step's own time, so the
        # simulator gets to show each step while it "runs".
        at: dict[tuple[str, str], float] = {}
        runner, *_ = make_runner(db, planner_factory=lambda: MockPlanner(ls), step_delay=0.05)
        runner.emit_state = lambda: at.setdefault(
            (runner.agent_state, runner.action), time.monotonic()
        )
        await runner.submit_task("macbook-pro", "list")
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert at[("ACTING", "")] - at[("ACTING", "ssh_exec")] >= 0.05

    asyncio.run(scenario())


def test_a_long_step_stays_named_through_a_pause_and_ends_with_the_kill(tmp_path: Path) -> None:
    class SlowTool(MockHostDriver):
        entered: asyncio.Event
        release: asyncio.Event

        async def run_agent(self, *args: Any, **kwargs: Any) -> Answer:
            type(self).entered.set()
            await type(self).release.wait()
            return Answer("Done.")

    async def scenario() -> None:
        db = LapisDb(tmp_path / "lapis.db")
        await db.init()
        frames: list[tuple[str, str]] = []
        runner, events, _a, _r, finished = make_runner(
            db, planner_factory=forbidden_planner, driver_factory=SlowTool
        )
        runner.emit_state = lambda: frames.append((runner.agent_state, runner.action))

        async def start() -> None:
            SlowTool.entered, SlowTool.release = asyncio.Event(), asyncio.Event()
            await runner.submit_task("macbook-pro", "go", agent=AgentJob(BUILTIN["claude"]))
            await asyncio.wait_for(SlowTool.entered.wait(), 5)

        # While the tool works (for minutes, on a real host) the state says so; its
        # activity event only comes when it has returned.
        await start()
        assert (runner.agent_state, runner.action) == ("ACTING", "agent_run")
        assert frames[-1] == ("ACTING", "agent_run") and [e["kind"] for e in events] == ["prompt"]
        # PAUSED names nothing. Resumed while the step is still going: acting, as it is.
        runner.pause()
        assert frames[-1] == ("PAUSED", "") and runner.action == ""
        runner.resume()
        assert frames[-1] == ("ACTING", "agent_run")
        SlowTool.release.set()
        await asyncio.wait_for(runner.wait_idle(), 5)
        assert frames[-3:] == [("ACTING", ""), ("THINKING", ""), ("IDLE", "")]
        assert finished[0]["status"] == "done"

        # A step that returns while paused was already off the screens: no frame for it,
        # and the resume that follows has nothing to go back to.
        await start()
        runner.pause()
        before = len(frames)
        SlowTool.release.set()
        await wait_for(lambda: [e["kind"] for e in events].count("agent_run") == 2)
        assert set(frames[before:]) <= {("PAUSED", "")}
        runner.resume()
        assert frames[-1] == ("THINKING", "")
        await asyncio.wait_for(runner.wait_idle(), 5)

        # The kill switch in the middle of a step: nothing is named afterwards.
        await start()
        await runner.kill_all()
        assert frames[-3:] == [("ACTING", ""), ("IDLE", ""), ("IDLE", "")]
        assert (runner.agent_state, runner.action) == ("IDLE", "")
        assert all(action == "" for state, action in frames if state != "ACTING")

    asyncio.run(scenario())
