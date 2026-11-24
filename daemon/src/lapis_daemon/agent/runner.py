"""Bounded task step loop driving a HostDriver through policy-gated actions."""

from __future__ import annotations

import asyncio
import base64
import time
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from lapis_daemon.agent.dhash import StuckDetector
from lapis_daemon.agent.policy import Policy, PolicyTier, is_read_only_ssh
from lapis_daemon.config import HostsConfig
from lapis_daemon.db import LapisDb
from lapis_daemon.kvm.base import HostDriver, MockHostDriver

DECISIONS = ("approve_once", "approve_session", "deny")

# Stable 120x68 1-bit preview fixture (15 bytes/row, 1020 bytes): 50% checkerboard.
MOCK_PREVIEW_B64 = base64.b64encode(
    bytes(0xAA if y % 2 else 0x55 for y in range(68) for _ in range(15))
).decode()


@dataclass(frozen=True)
class ScriptStep:
    action: str  # ssh_exec | vnc_click | type_text
    arg: str  # ssh command, click target label, or text to type
    detail: str
    x: int = 0
    y: int = 0
    high_risk: bool = False


# Phase 2 stand-in for the LLM: the mock "agent" rotates through this script.
MOCK_SCRIPT = (
    ScriptStep("ssh_exec", "uname -a", ""),
    ScriptStep("vnc_click", "header", "Clicked header", 160, 24),
    ScriptStep("type_text", "make check", "Typed into terminal"),
    ScriptStep("ssh_exec", "df -h", ""),
    ScriptStep("vnc_click", "Retry Build", "Clicked Retry Build button", 840, 412, high_risk=True),
    ScriptStep("ssh_exec", "systemctl status worker", ""),
)


def _noop(*_args: Any) -> None:
    pass


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
        emit_toast: Callable[[str, str], None] = _noop,
        max_steps: int = 25,
        step_delay: float = 0.0,
    ) -> None:
        self.db = db
        self.hosts = hosts
        self.emit_state = emit_state
        self.emit_event = emit_event
        self.emit_approval = emit_approval
        self.emit_toast = emit_toast
        self.driver_factory = driver_factory
        self.max_steps = max_steps
        self.step_delay = step_delay
        self.agent_state = "IDLE"
        self.active_task: dict[str, Any] | None = None
        self.drivers: dict[str, HostDriver] = {}
        self._task: asyncio.Task[None] | None = None
        self._pending: dict[str, asyncio.Future[str]] = {}
        self._session_hosts: set[str] = set()
        self._resume = asyncio.Event()
        self._resume.set()
        self._seq = 0
        self._req_n = 0
        self._current: ScriptStep | None = None
        self.stuck = StuckDetector()

    @property
    def is_running(self) -> bool:
        return self._task is not None and not self._task.done()

    async def wait_idle(self) -> None:
        if self._task is not None:
            await asyncio.gather(self._task, return_exceptions=True)

    async def submit_task(
        self, host: str, prompt: str, policy_tier: PolicyTier | str | None = None
    ) -> str:
        """Persist a task and start its step loop in the background; returns the task id."""
        if host not in self.hosts.hosts:
            raise ValueError(f"Unknown host: {host}")
        if self.is_running:
            raise ValueError("Agent busy: a task is already running")
        policy = Policy(policy_tier or self.hosts.hosts[host].policy)
        task_id = f"t_{time.time_ns() % 100000}"
        self.active_task = {"id": task_id, "title": prompt, "step": 0, "max_steps": self.max_steps}
        self._seq = 0
        self.stuck.reset()
        await self.db.insert_task(task_id, host, prompt, policy.tier.value, time.time())
        await self._record("prompt", prompt, f"Task queued for {host}", "ok")
        self._set_state("THINKING")
        self._task = asyncio.create_task(self._run(host, policy))
        return task_id

    def resolve_approval(self, request_id: str, decision: str, session: bool = False) -> bool:
        """Unblock a pending approval; returns False for unknown ids or decisions."""
        fut = self._pending.get(request_id)
        if fut is None or fut.done() or decision not in DECISIONS:
            return False
        if session and decision == "approve_once":
            decision = "approve_session"
        fut.set_result(decision)
        return True

    def pause(self) -> None:
        self._resume.clear()
        if self.is_running:
            self._set_state("PAUSED")

    def resume(self) -> None:
        self._resume.set()
        if self.is_running:
            self._set_state("THINKING")

    async def kill_all(self) -> None:
        """Kill switch: cancel the running task and drop every pending approval."""
        task, self._task = self._task, None
        if task is not None and not task.done():
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
        # MockHostDriver holds no modifier keys; Phase 3 VncDriver releases them on disconnect.
        self._session_hosts.clear()
        self._resume.set()
        if self.active_task is not None:
            await self._finish("cancelled")
        self.emit_toast("warn", "Kill switch: all agent activity stopped")
        self.emit_state()

    def _driver(self, host: str) -> HostDriver:
        if host not in self.drivers:
            driver = self.driver_factory(host)
            if isinstance(driver, MockHostDriver):
                driver.emit = self._on_action
            self.drivers[host] = driver
        return self.drivers[host]

    def _set_state(self, state: str) -> None:
        self.agent_state = state
        self.emit_state()

    async def _record(self, kind: str, summary: str, detail: str, status: str) -> None:
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
            }
        )

    async def _on_action(self, kind: str, summary: str, detail: str) -> None:
        hint = self._current.detail if self._current else ""
        await self._record(kind, summary, detail or hint, "ok")

    async def _approve(self, host: str, step: ScriptStep) -> bool:
        self._req_n += 1
        request_id = f"req_{self._req_n}"
        fut: asyncio.Future[str] = asyncio.get_running_loop().create_future()
        self._pending[request_id] = fut
        click = step.action == "vnc_click"
        self._set_state("WAITING_APPROVAL")
        self.emit_approval(
            {
                "type": "approval_request",
                "request_id": request_id,
                "host": host,
                "action": step.action,
                "summary": f"Click {step.arg} at ({step.x}, {step.y})" if click else step.arg,
                "risk": "low" if is_read_only_ssh(step.arg) else "high",
                "preview_1bit_120x68_b64": MOCK_PREVIEW_B64 if click else None,
            }
        )
        try:
            decision = await fut
        finally:
            self._pending.pop(request_id, None)
        if decision == "approve_session":
            self._session_hosts.add(host)
        return decision != "deny"

    async def _execute(self, driver: HostDriver, step: ScriptStep) -> None:
        if step.action == "ssh_exec":
            await driver.run_ssh(step.arg, 30)
        elif step.action == "vnc_click":
            await driver.click(step.x, step.y, "left", 1)
        else:
            await driver.type_text(step.arg)

    async def _run(self, host: str, policy: Policy) -> None:
        assert self.active_task is not None
        driver = self._driver(host)
        status = "done"
        try:
            await driver.connect()
            for n in range(1, self.max_steps + 1):
                if not self._resume.is_set():
                    self._set_state("PAUSED")
                    await self._resume.wait()
                self.active_task["step"] = n
                self._set_state("THINKING")
                step = self._current = MOCK_SCRIPT[(n - 1) % len(MOCK_SCRIPT)]
                command = None if step.action == "vnc_click" else step.arg
                decision = policy.evaluate(step.action, command, high_risk=step.high_risk)
                if decision.denied:
                    await self._record(step.action, step.arg, decision.reason, "denied")
                    continue
                if (
                    decision.requires_approval
                    and host not in self._session_hosts
                    and not await self._approve(host, step)
                ):
                    await self._record(step.action, step.arg, "Denied by user", "denied")
                    status = "cancelled"
                    break
                self._set_state("ACTING")
                await self._execute(driver, step)
                # dHash stuck-loop detection after each mutating action (SPEC 7.3).
                # MockHostDriver has no real frames: only feed real drivers' hashes.
                frame_hash = None
                if hasattr(driver, "vnc"):
                    frame_hash = getattr(driver.vnc, "last_frame_hash", None)
                if frame_hash is not None:
                    hint = self.stuck.update(frame_hash)
                    if hint:
                        self.emit_toast("warn", hint)
                        self._set_state("THINKING")
                if self.stuck.should_pause:
                    self.emit_toast("warn", "Stuck loop persists; paused for manual review")
                    self._set_state("PAUSED")
                    await self._resume.wait()
                await asyncio.sleep(self.step_delay)
        except Exception as exc:  # a broken step must not leave the agent stuck mid-task
            status = "failed"
            self.emit_toast("error", f"Task failed: {exc}")
        finally:
            await driver.disconnect()
        await self._finish(status)

    async def _finish(self, status: str) -> None:
        assert self.active_task is not None
        task_id = self.active_task["id"]
        self.active_task = None
        self._current = None
        for fut in self._pending.values():
            fut.cancel()
        self._pending.clear()
        await self.db.update_task_status(task_id, status, time.time())
        self._set_state("IDLE")
