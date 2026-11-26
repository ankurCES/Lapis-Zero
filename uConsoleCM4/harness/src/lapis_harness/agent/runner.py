"""Task step loop: planner -> Policy Gate (owner approval) -> driver -> record.

One loop serves both modes. In `--mock` the planner replays a short script and
the driver draws a fake desktop; on the device the planner is the configured
LLM and the driver is VNC + SSH over Tailscale. Everything in between (the gate,
stuck detection, the kill switch, persistence, the events the HUD and the phone
render) is identical, so what the simulator shows is what the hardware does.

Every step on a fleet host is put to the gate first (`gate.py`: the Rust daemon's
Policy Gate in production) and runs only if the gate allowed it. A chat bound to the
`deck` host runs the cyberdeck tools instead, each one through the daemon's
`/v1/deck/tools/invoke`, which gates it there (`deck.py`).
"""

from __future__ import annotations

import asyncio
import base64
import contextlib
import json
import time
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field, replace
from typing import Any

from lapis_harness import deck
from lapis_harness.agent import harness
from lapis_harness.agent.dhash import StuckDetector
from lapis_harness.agent.harness import Harness
from lapis_harness.agent.policy import KINDS, Policy, PolicyTier
from lapis_harness.config import DECK_HOST, HostsConfig
from lapis_harness.db import LapisDb
from lapis_harness.gate import DECISIONS, ActionRequest, Gate, LocalGate, Outcome, watched
from lapis_harness.kvm.base import Frame, HostDriver, MockHostDriver, ssh_setup_hint
from lapis_harness.llm.base import LLMError, Planner, ToolCall, ToolResult

__all__ = ["DECISIONS", "KINDS"]

# Stable 120x68 1-bit preview fixture (15 bytes/row, 1020 bytes): 50% checkerboard.
MOCK_PREVIEW_B64 = base64.b64encode(
    bytes(0xAA if y % 2 else 0x55 for y in range(68) for _ in range(15))
).decode()

# The mock "agent": one pass through this script, then finish.
MOCK_SCRIPT: tuple[ToolCall, ...] = (
    ToolCall("ssh_exec", {"command": "uname -a"}),
    ToolCall("mouse_click", {"x": 160, "y": 24, "intent": "Click header"}),
    ToolCall("type_text", {"text": "make check", "intent": "Type make check"}),
    ToolCall("ssh_exec", {"command": "df -h"}),
    ToolCall(
        "mouse_click",
        {"x": 840, "y": 412, "intent": "Click Retry Build", "risk": "high"},
    ),
    ToolCall("ssh_exec", {"command": "systemctl status worker"}),
)

GUI_MUTATING = {"mouse_click", "mouse_drag", "scroll", "type_text", "key_press"}
RISKY_WORDS = (
    "delete", "remove", "erase", "format", "send", "submit", "publish", "post ", "pay",
    "purchase", "buy", "password", "passwd", "secret", "token", "sudo", "install",
    "uninstall", "merge", "push", "deploy", "sign out", "log out", "shut down", "restart",
)  # fmt: skip
STUCK_HINT = (
    "The screen has not changed after the last few actions, so they are probably "
    "having no effect. Take a different approach: another control, a keyboard "
    "shortcut, or ssh_exec."
)
STUCK_HINT_NO_SSH = STUCK_HINT.replace(
    "another control, a keyboard shortcut, or ssh_exec", "another control or a keyboard shortcut"
)
NO_SSH = "SSH is not available on this host ({reason}). Do not call ssh_exec again: use the screen."
MAX_CONSECUTIVE_ERRORS = 3
DECK_RESULT_MAX = 8000  # characters of a deck tool's JSON result handed back to the model
# What the nudge and HUD input actions are, in the runner's tool names.
INPUT_TOOLS = {
    "click": "mouse_click",
    "move": "mouse_move",
    "scroll": "scroll",
    "type": "type_text",
    "key": "key_press",
}
FINE_CHANGED_PIXELS = 3  # thumbnail pixels that must move for "the screen changed"


class Busy(ValueError):
    """Not now: a task is running or being stopped (the control plane answers `409 busy`)."""


class _Failed(Exception):
    """The task cannot go on; the message is what the owner is told."""


@dataclass
class _Held:
    """What a task opened, for its teardown."""

    driver: HostDriver | None = None
    planner: Planner | None = None


def _noop(*_args: Any) -> None:
    pass


def _int(args: dict[str, Any], key: str, default: int = 0) -> int:
    try:
        return int(args.get(key, default))
    except (TypeError, ValueError):
        return default


@dataclass
class AgentJob:
    """A chat message that an agent tool on the host answers, instead of the device's LLM."""

    tool: Harness
    session: str = ""  # the tool's own session of this chat, to continue
    workdir: str = ""
    timeout_s: float = 1800.0
    # Called with the tool's session id once it is known ("" = it is gone: forget it).
    remember: Callable[[str], Awaitable[None]] | None = None
    # Every tool a chat could use instead, to name in a refusal (see `harness.refusal`).
    tools: dict[str, Harness] = field(default_factory=dict)
    # Filled in by the runner: the owner's message and the task's policy tier.
    message: str = ""
    tier: str = ""


class HarnessPlanner(Planner):
    """The whole plan of such a message: run the tool, then finish with what it said."""

    def __init__(self, tool: Harness) -> None:
        self.tool = tool

    async def start(
        self, task: str, *, host: str, host_os: str, history: list[dict[str, str]]
    ) -> None:
        """Nothing to prepare: the tool keeps its own conversation."""

    async def next(self, result: ToolResult | None) -> ToolCall:
        if result is None:
            return ToolCall("agent_run", {"agent": self.tool.id, "name": self.tool.name})
        return ToolCall("finish", {"summary": result.text})


class MockPlanner(Planner):
    """Stand-in for the LLM: replays `MOCK_SCRIPT` once, stops if the owner says no."""

    def __init__(self, script: tuple[ToolCall, ...] = MOCK_SCRIPT, loop: bool = False) -> None:
        self.script = script
        self.loop = loop
        self.i = 0

    async def start(
        self, task: str, *, host: str, host_os: str, history: list[dict[str, str]]
    ) -> None:
        self.i = 0

    async def next(self, result: ToolResult | None) -> ToolCall:
        if result is not None and result.denied:
            return ToolCall("finish", {"summary": "Stopped: the last action was denied."})
        if self.i >= len(self.script) and not self.loop:
            n = len(self.script)
            return ToolCall("finish", {"summary": f"Mock run complete: {n} scripted steps."})
        call = self.script[self.i % len(self.script)]
        self.i += 1
        return call


class TaskRunner:
    """Runs one task at a time; emit_* callbacks are sync and must not block."""

    def __init__(
        self,
        db: LapisDb,
        hosts: HostsConfig,
        emit_state: Callable[[], None] = _noop,
        emit_event: Callable[[dict[str, Any]], None] = _noop,
        emit_approval: Callable[[dict[str, Any]], None] = _noop,
        driver_factory: Callable[[str], HostDriver] = MockHostDriver,
        *,
        planner_factory: Callable[[], Planner] = MockPlanner,
        deck_planner_factory: Callable[[], Planner] = lambda: MockPlanner(deck.DECK_SCRIPT),
        gate: Gate | None = None,
        deck_tools: deck.DeckTools | None = None,
        emit_toast: Callable[[str, str], None] = _noop,
        emit_resolved: Callable[[dict[str, Any]], None] = _noop,
        emit_finish: Callable[[dict[str, Any]], Any] = _noop,
        frames: Any = None,
        max_steps: int = 25,
        step_delay: float = 0.0,
        settle_delay: float = 0.0,
        stuck_threshold: int = 3,
        stuck_hamming_max: int = 2,
    ) -> None:
        self.db = db
        self.hosts = hosts
        self.emit_state = emit_state
        self.emit_event = emit_event
        self.emit_approval = emit_approval
        self.emit_toast = emit_toast
        self.emit_resolved = emit_resolved
        self.emit_finish = emit_finish
        self.driver_factory = driver_factory
        self.planner_factory = planner_factory
        self.deck_planner_factory = deck_planner_factory
        # Without one given: Lapis-Zero's in-process gate, whose approval requests go out
        # through emit_approval (tests). `main` always passes the daemon's gate.
        self.gate: Gate = gate if gate is not None else LocalGate(emit_approval, emit_resolved)
        self.deck_tools = deck_tools
        self.frames = frames
        self.max_steps = max_steps
        self.step_delay = step_delay  # how long a mock step takes (see `_act`)
        self.settle_delay = settle_delay  # pause before the post-action screenshot
        self.agent_state = "IDLE"
        self.active_task: dict[str, Any] | None = None
        self.drivers: dict[str, HostDriver] = {}
        self._task: asyncio.Task[None] | None = None
        self._killing: asyncio.Future[None] | None = None  # the kill switch at work
        self._resume = asyncio.Event()
        self._resume.set()
        self._seq = 0
        self._deck_wait: dict[str, Any] | None = None  # a deck tool the daemon holds for the owner
        self._last_shot: int | None = None
        self._last_fine: bytes | None = None
        self._no_ssh: str | None = None  # why the running task has no shell, if it has none
        self._job: AgentJob | None = None  # set while an agent tool answers the running task
        self._acting = ""  # the activity kind of the step inside `_execute` right now
        self.stuck = StuckDetector(stuck_threshold, stuck_hamming_max)

    # -- public control surface ------------------------------------------------

    @property
    def pending(self) -> dict[str, dict[str, Any]]:
        """request_id -> approval_request frame: the steps waiting for the owner now."""
        frames = [*self.gate.pending(), *([self._deck_wait] if self._deck_wait else [])]
        return {frame["request_id"]: frame for frame in frames}

    @property
    def is_running(self) -> bool:
        """A task is running, or one is still being stopped: nothing new is taken."""
        return self._killing is not None or (self._task is not None and not self._task.done())

    def refuse_if_busy(self) -> None:
        """Busy, saying which of the two it is, unless a new task can be taken."""
        if self._killing is not None:
            raise Busy("Agent busy: the last task is still being stopped")
        if self.is_running:
            raise Busy("Agent busy: a task is already running")

    @property
    def action(self) -> str:
        """What the agent is doing on the host right now, for the screens: an activity
        kind (`screenshot`, `vnc_click`, `ssh_exec`, `agent_run`, ...), or "".

        Named only while the state is ACTING: an activity event reports a step when it
        has returned, which is too late to show a fetch or a command while it happens.
        """
        return self._acting if self.agent_state == "ACTING" else ""

    async def wait_idle(self) -> None:
        if self._task is not None:
            await asyncio.gather(self._task, return_exceptions=True)

    async def submit_task(
        self,
        host: str,
        prompt: str,
        policy_tier: PolicyTier | str | None = None,
        *,
        chat_id: str | None = None,
        history: list[dict[str, str]] | None = None,
        agent: AgentJob | None = None,
    ) -> str:
        """Persist a task and start its step loop in the background; returns the task id.

        With `agent`, the message is handed to that tool on the host; otherwise the
        configured LLM plans the steps.
        """
        on_deck = host == DECK_HOST
        if host not in self.hosts.hosts and not on_deck:
            raise ValueError(f"Unknown host: {host}")
        if on_deck and agent is not None:
            raise ValueError("Agent tools run on fleet hosts: the deck chat uses its own agent")
        self.refuse_if_busy()
        policy = Policy(policy_tier or ("strict" if on_deck else self.hosts.hosts[host].policy))
        task_id = f"t_{time.time_ns() % 10**9:09d}"
        self.active_task = {
            "id": task_id,
            "title": prompt,
            "step": 0,
            "max_steps": 1 if agent else self.max_steps,  # a tool's run is one step
            "host": host,
            "chat_id": chat_id,
        }
        self._job = replace(agent, message=prompt, tier=policy.tier.value) if agent else None
        self._seq = 0
        self._last_shot = None
        self._last_fine = None
        self._resume.set()
        self.stuck.reset()
        await self.db.insert_task(task_id, host, prompt, policy.tier.value, time.time(), chat_id)
        await self._record("prompt", prompt, f"Task queued for {host}", "ok")
        self._set_state("THINKING")
        self._task = asyncio.create_task(self._run(host, prompt, policy, history or []))
        return task_id

    def resolve_approval(
        self, request_id: str, decision: str, by: str = "device", reason: str = ""
    ) -> bool:
        """Unblock a pending approval (in-process gate); False for unknown ids or decisions."""
        return self.gate.resolve(request_id, decision, by, reason)

    def pause(self) -> None:
        self._resume.clear()
        if self.is_running and self.agent_state != "WAITING_APPROVAL":
            self._set_state("PAUSED")

    def resume(self) -> None:
        self._resume.set()
        if self.is_running and self.agent_state == "PAUSED":
            # Paused in the middle of a step (a tool's run takes minutes): it is still at it.
            self._set_state("ACTING" if self._acting else "THINKING")

    async def kill_all(self, by: str = "device") -> None:
        """Kill switch: cancel the running task and drop every pending approval.

        Returns when the task has ended, the stopping of what it ran on the host
        included. Until then the agent counts as running (`is_running`), and a second
        press waits for the first one: it must not cut the stopping short.
        """
        if self._killing is None:
            self._killing = asyncio.ensure_future(self._kill(by))
        # Shielded: the kill goes on even if whoever asked for it goes away.
        await asyncio.shield(self._killing)

    async def _kill(self, by: str) -> None:
        try:
            task, self._task = self._task, None
            if task is not None and not task.done():
                # Cancelled once, then waited for. A second cancellation would end the
                # wait for the remote command to stop, and with it the stopping (hence
                # the shield, should this very wait be cancelled).
                task.cancel()
                await asyncio.gather(asyncio.shield(task), return_exceptions=True)
            self._resume.set()
            summary, toast = self._kill_report()
            if self.active_task is not None:
                await self._finish("cancelled", summary, by=by)
            self.emit_toast("warn", toast)
            self.emit_state()
        finally:
            self._killing = None

    def _kill_report(self) -> tuple[str, str]:
        """(the cancelled task's summary, the toast): only what the host confirmed is claimed."""
        task = self.active_task
        driver = self.drivers.get(task["host"]) if task else None
        left = driver.left_running if driver is not None else harness.LEFT_NOTHING
        if task is None or not left:
            return "Stopped by the kill switch.", "Kill switch: all agent activity stopped"
        host = task["host"]
        what = self._job.tool.name if self._job else "the last command"
        if left == harness.LEFT_STARTED:
            return (
                f"Stopped by the kill switch, but the programs that {what} had started on "
                f"{host} could not be checked: some may still be running there.",
                f"Kill switch: programs started by {what} may still be running on {host}",
            )
        return (
            f"Stopped on this device by the kill switch, but {host} did not confirm that "
            f"{what} was stopped: it may still be running there.",
            f"Kill switch: {what} may still be running on {host}",
        )

    async def nudge(
        self, host: str, action: str, args: dict[str, Any], source: str = "android"
    ) -> Outcome:
        """Manual assist from the phone while the agent is paused or waiting.

        Put to the gate like any step; returns its outcome (the input ran only if allowed).
        """
        task = self.active_task
        if task is None or task["host"] != host:
            raise ValueError("No task is running on this host")
        if self.agent_state not in ("PAUSED", "WAITING_APPROVAL"):
            raise ValueError("Manual assist is only available while the agent is paused")
        if action not in ("click", "type", "key"):
            raise ValueError(f"Unknown nudge action: {action}")
        driver = self.drivers[host]
        outcome = await self.manual_input(
            driver, host, action, args, source=source, chat_id=task.get("chat_id"),
            task_id=task["id"],
        )  # fmt: skip
        if outcome.allowed:
            shot = await self._capture(driver, None)
            summary = input_summary(action, args)
            await self._record("nudge", summary, "Manual assist from the phone", "ok", shot)
        return outcome

    async def manual_input(
        self,
        driver: HostDriver,
        host: str,
        action: str,
        args: dict[str, Any],
        *,
        source: str,
        chat_id: str | None = None,
        task_id: str | None = None,
        lock: asyncio.Lock | None = None,
        timeout_s: float | None = None,
    ) -> Outcome:
        """One input from the owner's own hands (phone nudge, HUD viewport), gated first.

        `lock` is held while the input runs (not while the owner decides); `timeout_s`
        bounds the input itself.
        """
        tool = INPUT_TOOLS.get(action)
        if tool is None:
            raise ValueError(f"Unknown input action: {action!r}")
        text = str(args.get("text", "")) if action == "type" else None
        cfg = self.hosts.hosts.get(host)
        req = ActionRequest(
            tool=tool,
            host=host,
            command=text,
            summary=input_summary(action, args),
            host_policy=cfg.policy if cfg is not None else "strict",
            chat_id=chat_id,
            task_id=task_id,
            click_x=_int(args, "x") if action == "click" else None,
            click_y=_int(args, "y") if action == "click" else None,
            source=source,
        )
        outcome = await self.gate.authorize(req)
        if not outcome.allowed:
            return outcome
        x, y = _int(args, "x"), _int(args, "y")

        async def run() -> None:
            if action == "click":
                await driver.click(x, y, str(args.get("button", "left")), 1)
            elif action == "move":
                await driver.move(x, y)
            elif action == "scroll":
                await driver.scroll(x, y, _int(args, "dx"), _int(args, "dy"))
            elif action == "type":
                await driver.type_text(text or "")
            else:
                await driver.key_press(str(args.get("combo", "")))

        async with lock if lock is not None else contextlib.nullcontext():
            await asyncio.wait_for(run(), timeout_s)
        return outcome

    # -- internals -------------------------------------------------------------

    def _set_state(self, state: str) -> None:
        self.agent_state = state
        self.emit_state()

    async def _record(
        self, kind: str, summary: str, detail: str, status: str, screenshot_id: int | None = None
    ) -> None:
        assert self.active_task is not None
        task_id = self.active_task["id"]
        self._seq += 1
        now = time.time()
        await self.db.insert_step(task_id, self._seq, kind, summary, detail, status, now)
        self.emit_event(
            {
                "type": "activity_event",
                "ts": int(now),
                "task_id": task_id,
                "kind": kind,
                "summary": summary,
                "detail": detail,
                "status": status,
                "chat_id": self.active_task.get("chat_id"),
                "step": self.active_task["step"],
                "screenshot_id": screenshot_id,
            }
        )

    async def _capture(self, driver: HostDriver, click: tuple[int, int] | None) -> int | None:
        """Take a screenshot, store it in the ring buffer, return its id."""
        frame = await driver.screenshot()
        return await self._store(frame, click)

    async def _store(self, frame: Frame, click: tuple[int, int] | None) -> int | None:
        if self.frames is None or self.active_task is None:
            return None
        self._last_shot = await self.frames.save(
            self.active_task["id"], self.active_task["step"], frame, click
        )
        return self._last_shot

    @staticmethod
    def describe(call: ToolCall) -> tuple[str, str, str | None, bool]:
        """(ticker summary, detail, text the policy inspects, self-declared/heuristic risk)."""
        a = call.args
        intent = str(a.get("intent", "")).strip()
        declared = str(a.get("risk", "low")).lower() == "high"
        risky = declared or any(w in intent.lower() for w in RISKY_WORDS)
        match call.tool:
            case "ssh_exec":
                return str(a.get("command", "")), "", str(a.get("command", "")), False
            case "mouse_click":
                button = a.get("button", "left")
                return f"click({_int(a, 'x')}, {_int(a, 'y')}) {button}", intent, None, risky
            case "mouse_drag":
                summary = (
                    f"drag({_int(a, 'x1')}, {_int(a, 'y1')} -> {_int(a, 'x2')}, {_int(a, 'y2')})"
                )
                return summary, intent, None, risky
            case "mouse_move":
                return f"move({_int(a, 'x')}, {_int(a, 'y')})", "", None, False
            case "scroll":
                return f"scroll({_int(a, 'dx')}, {_int(a, 'dy')})", "", None, False
            case "type_text":
                text = str(a.get("text", ""))
                # Never log something flagged as a secret.
                shown = "(hidden)" if declared else repr(text[:60])
                return f"type {shown}", intent, text, risky
            case "key_press":
                return f"key {a.get('combo', '')}", intent, None, risky
            case "wait":
                return f"wait {a.get('seconds', 1)}s", "", None, False
            case "agent_run":
                return str(a.get("name") or "agent tool"), "", None, False
            case _:
                return call.tool, intent, None, False

    async def _authorize(
        self,
        driver: HostDriver,
        host: str,
        policy: Policy,
        call: ToolCall,
        summary: str,
        detail: str,
        inspected: str | None,
        risky: bool,
    ) -> Outcome:
        """Put one step to the gate; the state shows WAITING_APPROVAL while the owner decides."""
        assert self.active_task is not None
        click: tuple[int, int] | None = None
        if call.tool in ("mouse_click", "mouse_drag"):
            key = ("x", "y") if call.tool == "mouse_click" else ("x1", "y1")
            click = (_int(call.args, key[0]), _int(call.args, key[1]))
        if call.tool == "mouse_click" and click is not None:
            shown = f"{detail or 'Click'} at ({click[0]}, {click[1]})"
        elif call.tool == "ssh_exec" or not detail:
            shown = summary
        else:
            # The literal keys/text first (what will actually happen), then the
            # model's stated intent. `summary` already hides declared secrets.
            shown = f"{summary} ({detail})"
        req = ActionRequest(
            tool=call.tool,
            host=host,
            command=inspected,
            summary=shown,
            detail=detail,
            risky=risky,
            host_policy=policy.tier.value,
            chat_id=self.active_task.get("chat_id"),
            task_id=self.active_task["id"],
            step=self.active_task["step"],
            screenshot_id=self._last_shot,
            click_x=click[0] if click else None,
            click_y=click[1] if click else None,
        )
        if click is not None:

            async def prepare(r: ActionRequest) -> None:
                """The frame the click lands on, and its 1-bit crop for the approval card."""
                if self._last_shot is None:
                    shot = ActionRequest("screenshot", host, task_id=r.task_id, chat_id=r.chat_id)
                    if (await self.gate.authorize(shot)).allowed:
                        await self._capture(driver, None)
                r.screenshot_id = self._last_shot
                r.preview_1bit_120x68_b64 = driver.preview(*click) or (
                    MOCK_PREVIEW_B64 if driver.mock else None
                )

            req.prepare = prepare
        return await self.gate.authorize(req, lambda: self._set_state("WAITING_APPROVAL"))

    async def _execute(self, driver: HostDriver, call: ToolCall) -> ToolResult:
        a = call.args
        match call.tool:
            case "screenshot":
                frame = await driver.screenshot()
                await self._store(frame, None)
                return ToolResult("", frame.jpeg, frame.width, frame.height)
            case "ssh_exec":
                if self._no_ssh:  # not offered for this task; a model may still name it
                    return ToolResult(NO_SSH.format(reason=self._no_ssh), is_error=True)
                timeout = max(1, min(_int(a, "timeout_s", 30), 120))
                return ToolResult(await driver.run_ssh(str(a.get("command", "")), timeout))
            case "agent_run":
                # Only a chat bound to a tool runs one, with the owner's own message:
                # nothing a model puts in the call's arguments is used.
                if self._job is None:
                    return ToolResult("Unknown tool 'agent_run'.", is_error=True)
                return await self._agent_run(driver, self._job)
            case "wait":
                try:
                    seconds = float(a.get("seconds", 1))
                except (TypeError, ValueError):
                    seconds = 1.0
                await asyncio.sleep(max(1.0, min(seconds, 15.0)) if not driver.mock else 0)
                return ToolResult("waited")
            case "mouse_move":
                await driver.move(_int(a, "x"), _int(a, "y"))
                return ToolResult("moved")
            case "mouse_click":
                clicks = max(1, min(_int(a, "clicks", 1), 3))
                await driver.click(_int(a, "x"), _int(a, "y"), str(a.get("button", "left")), clicks)
            case "mouse_drag":
                await driver.drag(_int(a, "x1"), _int(a, "y1"), _int(a, "x2"), _int(a, "y2"))
            case "scroll":
                await driver.scroll(_int(a, "x"), _int(a, "y"), _int(a, "dx"), _int(a, "dy"))
            case "type_text":
                await driver.type_text(str(a.get("text", "")))
            case "key_press":
                await driver.key_press(str(a.get("combo", "")))
            case _:
                return ToolResult(f"Unknown tool {call.tool!r}.", is_error=True)
        # Every mutating GUI action reports what the screen looks like afterwards:
        # the model's next observation, the phone's live view, and the stuck check.
        await asyncio.sleep(self.settle_delay)
        frame = await driver.screenshot()
        click = (_int(a, "x"), _int(a, "y")) if call.tool == "mouse_click" else None
        await self._store(frame, click)
        result = ToolResult("done", frame.jpeg, frame.width, frame.height)
        if frame.dhash is not None and not driver.mock and self._screen_stuck(frame):
            result.hint = STUCK_HINT_NO_SSH if self._no_ssh else STUCK_HINT
            self.emit_toast("warn", "Screen unchanged after several actions; changing approach")
        return result

    async def _agent_run(self, driver: HostDriver, job: AgentJob) -> ToolResult:
        """Hand the task's message to the chat's tool on the host; its answer is the result."""

        async def run(session: str) -> Any:
            return await driver.run_agent(
                job.tool,
                job.message,
                session=session,
                tier=job.tier,
                workdir=job.workdir,
                timeout_s=job.timeout_s,
                on_progress=self._progress,
            )

        try:
            answer = await run(job.session)
            lost = bool(job.session) and answer.lost_session
            if lost:
                # The tool has dropped the session this chat was continuing (its own
                # clean-up, another folder): the message starts a new one.
                answer = await run("")
        except Exception as exc:  # noqa: BLE001 - the owner gets a sentence, not a traceback
            why = str(exc) or type(exc).__name__
            return ToolResult(f"{job.tool.name} could not be run: {why}", is_error=True)
        new = answer.session and answer.session != job.session
        if job.remember is not None and (lost or new):
            await job.remember(answer.session)
        return ToolResult(answer.text, is_error=answer.is_error)

    def _progress(self, line: str) -> None:
        """A line of the tool's output for the ticker and the phone: shown, not stored."""
        task = self.active_task
        if task is not None:
            self.emit_event(
                {
                    "type": "activity_event",
                    "ts": int(time.time()),
                    "task_id": task["id"],
                    "kind": "agent_run",
                    "summary": line,
                    "detail": "",
                    "status": "running",
                    "chat_id": task.get("chat_id"),
                    "step": task["step"],
                    "screenshot_id": None,
                }
            )

    def _screen_stuck(self, frame: Frame) -> bool:
        """SPEC 7.3 stuck check (dHash, Hamming <= 2), refined so typing does not trip it.

        A 64-bit dHash cannot see a line of text appear in a terminal, which would
        flag ordinary shell work as a stuck loop. When the frame carries a finer
        thumbnail and that shows a real change, the streak is restarted instead.
        """
        if frame.dhash is None:
            return False
        previous, self._last_fine = self._last_fine, frame.fine
        if previous and frame.fine and len(previous) == len(frame.fine):
            moved = sum(1 for a, b in zip(previous, frame.fine, strict=True) if abs(a - b) > 12)
            if moved >= FINE_CHANGED_PIXELS:
                # This frame starts a new streak, exactly as `update()` would count it.
                self.stuck.consecutive = 1
                self.stuck._last_hash = frame.dhash
                return False
        return self.stuck.update(frame.dhash) is not None

    async def _step(
        self, driver: HostDriver, host: str, policy: Policy, call: ToolCall
    ) -> ToolResult:
        kind = KINDS.get(call.tool, (call.tool, call.tool))[0]
        summary, detail, inspected, risky = self.describe(call)
        if call.tool == "agent_run" and self._job is None:
            # Only a chat bound to a tool runs one, with the owner's own message: a model
            # that names it is not put to the gate (where it would pass as that message).
            result = ToolResult("Unknown tool 'agent_run'.", is_error=True)
            await self._record(kind, summary, result.text, "error")
            return result
        # `agent_run` is the owner's own message, handed over as it is: prose is not a
        # command for the denylist (`inspected` is None), and what the tool then does is
        # kept within the tier by its own permission settings; a tool that has none for
        # this tier never gets here (see `_run`).
        outcome = await self._authorize(
            driver, host, policy, call, summary, detail, inspected, risky
        )
        if not outcome.allowed:
            return await self._refused(kind, summary, outcome)
        try:
            result = await self._act(kind, driver, call)
        except (LLMError, asyncio.CancelledError):
            raise
        except Exception as exc:  # noqa: BLE001 - reported to the model, which can adapt
            await self._record(kind, summary, f"{type(exc).__name__}: {exc}", "error")
            return ToolResult(f"Error: {type(exc).__name__}: {exc}", is_error=True)
        shot = self._last_shot if result.jpeg else None
        status = "error" if result.is_error and call.tool == "agent_run" else "ok"
        await self._record(kind, summary, detail or result.text[:500], status, shot)
        return result

    async def _refused(self, kind: str, summary: str, outcome: Outcome) -> ToolResult:
        """Record a step the gate did not allow; what the model is told about it."""
        if outcome.decision == "policy_deny":
            await self._record(kind, summary, outcome.reason, "denied")
            return ToolResult(f"Blocked by policy ({outcome.reason}).", denied=True)
        if outcome.decision == "timeout":
            await self._record(kind, summary, "No answer from the owner in time", "denied")
            return ToolResult("The owner did not answer in time; it was not run.", denied=True)
        if outcome.by == "policy":  # the gate itself could not be asked (fail closed)
            await self._record(kind, summary, outcome.reason, "denied")
            return ToolResult(f"Not run: {outcome.reason}.", denied=True)
        await self._record(kind, summary, "Denied by user", "denied")
        why = (
            f" Their note: {outcome.reason}"
            if outcome.reason and outcome.by != "kill_switch"
            else ""
        )
        return ToolResult(f"The owner denied this action.{why}", denied=True)

    async def _deck_step(self, call: ToolCall) -> ToolResult:
        """One cyberdeck tool, run by the daemon (`/v1/deck/tools/invoke`) and only there.

        The daemon's Policy Gate decides it, waiting for the owner at Tier 1/2; nothing
        here executes a deck tool itself.
        """
        assert self.active_task is not None
        summary = deck.summary(call)
        if self.deck_tools is None:
            await self._record("deck_tool", summary, "The cyberdeck tools are unavailable", "error")
            return ToolResult("The cyberdeck tools are unavailable.", is_error=True)
        context = {
            "chat_id": self.active_task.get("chat_id"),
            "task_id": self.active_task["id"],
            "source": "harness",
        }
        task = self.active_task

        def waiting() -> None:
            self._deck_wait = {
                "type": "approval_request", "request_id": "", "host": DECK_HOST,
                "action": call.tool, "summary": summary, "task_id": task["id"],
                "chat_id": task.get("chat_id"), "step": task["step"], "source": "harness",
                "held_by": "lapis-daemon",
            }  # fmt: skip
            self._set_state("WAITING_APPROVAL")

        self._acting = "deck_tool"
        self._set_state("ACTING")
        try:
            reply = await watched(self.deck_tools.invoke(call.tool, call.args, context), waiting)
            await asyncio.sleep(self.step_delay)
        finally:
            self._deck_wait = None
            shown, self._acting = self.action, ""
            if shown:
                self.emit_state()
        decision = str(reply.get("decision") or "")
        if reply.get("ok") is not True:
            error = str(reply.get("error") or decision or "failed")[:500]
            if decision in ("deny", "policy_deny", "timeout"):
                await self._record("deck_tool", summary, f"{decision}: {error}", "denied")
                return ToolResult(f"Not run ({decision}): {error}", denied=True)
            await self._record("deck_tool", summary, error, "error")
            return ToolResult(f"Error: {error}", is_error=True)
        text = json.dumps(reply.get("result"), separators=(",", ":"), default=str)
        await self._record("deck_tool", summary, text[:500], "ok")
        return ToolResult(text[:DECK_RESULT_MAX])

    async def _run_deck(
        self, prompt: str, history: list[dict[str, str]], held: _Held
    ) -> tuple[str, str]:
        """The deck chat's loop; returns (status, summary)."""
        assert self.active_task is not None
        planner = held.planner = self.deck_planner_factory()
        if self.deck_tools is None:
            raise _Failed("The cyberdeck tools are unavailable: lapis-daemon is not set up.")
        try:
            tools = await self.deck_tools.list_tools()
        except Exception as exc:  # noqa: BLE001 - the owner gets a sentence
            raise _Failed(f"Could not list the cyberdeck tools: {exc}") from exc
        planner.use_tools(deck.llm_specs(tools), deck.system_prompt())
        await planner.start(prompt, host=DECK_HOST, host_os="linux", history=history)
        result: ToolResult | None = None
        errors = 0
        for n in range(1, self.max_steps + 1):
            await self._gate()
            self.active_task["step"] = n
            self._set_state("THINKING")
            call = await planner.next(result)
            call = ToolCall(deck.tool_name(call.tool), call.args, call.thought, call.call_id)
            if call.thought and call.tool != "finish":
                await self._record("thought", call.thought[:200], "", "ok")
            if call.tool == "finish":
                summary = str(call.args.get("summary", "")).strip() or "Done."
                return ("cancelled" if result is not None and result.denied else "done"), summary
            result = await self._deck_step(call)
            errors = errors + 1 if result.is_error else 0
            if errors >= MAX_CONSECUTIVE_ERRORS:
                return "failed", f"Stopped after repeated errors. {result.text}"
        return "stopped", f"Stopped at the {self.max_steps}-step limit without finishing."

    async def _act(self, kind: str, driver: HostDriver, call: ToolCall) -> ToolResult:
        """Execute one step; `action` names it for exactly as long as it runs."""
        self._acting = kind
        self._set_state("ACTING")
        try:
            result = await self._execute(driver, call)
            # A mock step returns at once: its pause is the time the step "takes", so
            # the ticker and the screens get to show it. Nothing on a real host.
            await asyncio.sleep(self.step_delay)
            return result
        finally:
            shown, self._acting = self.action, ""
            if shown:  # the screens were told it runs: tell them it is over
                self.emit_state()

    async def _gate(self) -> None:
        if not self._resume.is_set():
            self._set_state("PAUSED")
            await self._resume.wait()

    async def _run(
        self, host: str, prompt: str, policy: Policy, history: list[dict[str, str]]
    ) -> None:
        assert self.active_task is not None
        status, summary = "done", ""
        held = _Held()
        self.drivers.pop(host, None)  # the last task's driver: its stops are not this one's
        try:
            if host == DECK_HOST:
                status, summary = await self._run_deck(prompt, history, held)
            else:
                status, summary = await self._run_fleet(host, prompt, policy, history, held)
        except asyncio.CancelledError:
            raise
        except _Failed as exc:
            status, summary = "failed", str(exc)
        except Exception as exc:  # noqa: BLE001 - a broken step must not wedge the agent
            status, summary = "failed", f"Task failed: {exc}"
            self.emit_toast("error", summary[:120])
        finally:
            # Shielded: a kill must still release modifiers and close the session.
            with contextlib.suppress(Exception):
                await asyncio.shield(self._teardown(held.driver, held.planner))
        await self._finish(status, summary)

    async def _run_fleet(
        self,
        host: str,
        prompt: str,
        policy: Policy,
        history: list[dict[str, str]],
        held: _Held,
    ) -> tuple[str, str]:
        """A fleet host's loop; returns (status, summary)."""
        assert self.active_task is not None
        job = self._job
        if job and job.tier not in job.tool.tiers:
            # Nothing is asked of the owner step by step in such a chat, so a tool
            # whose own permissions cannot stand in for the tier is not run at all.
            raise _Failed(harness.refusal(job.tool, job.tier, host, job.tools))
        driver = held.driver = self.drivers[host] = self.driver_factory(host)
        # A tool on the host needs no LLM, so no API key is asked for either.
        planner = held.planner = HarnessPlanner(job.tool) if job else self.planner_factory()
        await driver.connect()
        # A host without a working SSH login is driven through the screen alone. Offered
        # a shell that always fails, a model keeps calling it until the error limit
        # ends the task (seen on the first hardware chat: no SSH key on the device).
        self._no_ssh = await driver.ssh_problem()
        host_os = getattr(self.hosts.hosts[host], "os", "macos")
        if self._no_ssh and job:
            # A tool on the host is reached over SSH and nothing else.
            raise _Failed(
                f"{job.tool.name} runs on {host} over SSH. "
                + ssh_setup_hint(host, host_os, self._no_ssh)
            )
        if self._no_ssh:
            planner.ssh = False
            await self._record(
                "ssh_exec", "SSH unavailable: using the screen only", self._no_ssh, "error"
            )
        await planner.start(prompt, host=host, host_os=host_os, history=history)
        result: ToolResult | None = None
        errors = 0
        # A tool's run is two turns here (run it, finish) whatever `llm.max_steps` says.
        for n in range(1, (2 if job else self.max_steps) + 1):
            await self._gate()
            self.active_task["step"] = min(n, self.active_task["max_steps"])
            self._set_state("THINKING")
            call = await planner.next(result)
            if call.thought and call.tool != "finish":
                await self._record("thought", call.thought[:200], "", "ok")
            if call.tool == "finish":
                summary = str(call.args.get("summary", "")).strip() or "Done."
                return ("cancelled" if result is not None and result.denied else "done"), summary
            result = await self._step(driver, host, policy, call)
            if job and result.is_error:  # the tool's run is the task: no second try
                raise _Failed(result.text)
            errors = errors + 1 if result.is_error else 0
            if errors >= MAX_CONSECUTIVE_ERRORS:
                return "failed", f"Stopped after repeated errors. {result.text}"
            if self.stuck.should_pause:
                self.emit_toast("warn", "Stuck loop persists; paused for manual review")
                self.stuck.reset()
                self._resume.clear()
                await self._gate()
        return "stopped", f"Stopped at the {self.max_steps}-step limit without finishing."

    @staticmethod
    async def _teardown(driver: HostDriver | None, planner: Planner | None) -> None:
        if planner is not None:
            with contextlib.suppress(Exception):
                await planner.close()
        if driver is not None:
            await driver.disconnect()

    async def _finish(self, status: str, summary: str = "", by: str = "") -> None:
        task = self.active_task
        if task is None:
            return
        self.active_task = None
        self.gate.deny_all(by or "kill_switch")
        await self.db.update_task_status(task["id"], status, time.time(), summary)
        self._set_state("IDLE")
        outcome = self.emit_finish(
            {
                "type": "task_finished",
                "task_id": task["id"],
                "chat_id": task.get("chat_id"),
                "host": task["host"],
                "status": status,
                "summary": summary,
            }
        )
        if asyncio.iscoroutine(outcome):
            await outcome


def input_summary(action: str, args: dict[str, Any]) -> str:
    """The ticker line of a manual input (typed text is never echoed)."""
    x, y = _int(args, "x"), _int(args, "y")
    return {
        "click": f"click({x}, {y})",
        "move": f"move({x}, {y})",
        "scroll": f"scroll({_int(args, 'dx')}, {_int(args, 'dy')})",
        "type": "type (manual)",
        "key": f"key {args.get('combo', '')}",
    }.get(action, action)
