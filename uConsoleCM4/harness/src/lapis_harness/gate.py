"""The Policy Gate as the task runner sees it: one question per step, asked before it runs.

`DaemonGate` (production, always what `main` uses) asks the Rust daemon, which classifies
the step (Tier 0/1/2, deny by default), waits for the owner on the HUD or a phone when it
has to, and answers. Anything but a clear "allowed" is a denial: an unreachable daemon, a
broken answer, a decision this module does not know (fail closed).

`LocalGate` is Lapis-Zero's in-process gate: `agent/policy.py` decides, and a step that
needs the owner waits on an approval future that `resolve` (the harness's own approvals
routes and WebSocket) completes. Tests use it, so the runner is exercised without a daemon.

The runner never executes a step whose `Outcome.allowed` is false.
"""

from __future__ import annotations

import asyncio
import contextlib
import secrets
from collections.abc import Awaitable, Callable
from dataclasses import asdict, dataclass, field
from typing import Any, Protocol

from lapis_harness.agent.policy import KINDS, Policy, is_read_only_ssh
from lapis_harness.daemon_client import DaemonClient, DaemonUnavailable

# The task runner's tools: exactly what the daemon's classifier knows (it denies the rest).
TOOLS = (
    "screenshot",
    "wait",
    "finish",
    "mouse_move",
    "mouse_click",
    "mouse_drag",
    "scroll",
    "type_text",
    "key_press",
    "ssh_exec",
    "agent_run",
)
ALLOWING = ("auto", "approve_once", "approve_session")
DECISIONS = ("approve_once", "approve_session", "deny")  # what an owner can answer
# How long a gate question may take before the step counts as waiting for the owner. A
# Tier 0 answer from the daemon takes milliseconds; an approval takes as long as it takes.
WAIT_AFTER = 0.25


def _noop(*_args: Any) -> None:
    pass


@dataclass
class ActionRequest:
    """One step an agent wants to take on a host (the daemon's `ActionRequest`)."""

    tool: str
    host: str
    command: str | None = None  # shell command or typed text: what the denylist inspects
    summary: str = ""  # what the owner reads on the approval card
    detail: str = ""
    risky: bool = False
    host_policy: str | None = None  # strict | balanced | autonomous
    chat_id: str | None = None
    task_id: str | None = None
    step: int = 0
    screenshot_id: int | None = None
    click_x: int | None = None
    click_y: int | None = None
    preview_1bit_120x68_b64: str | None = None
    source: str = "harness"
    # Not sent. Fills in the screenshot and the preview before an owner is asked: the
    # local gate calls it only then, the daemon gate before every question.
    prepare: Callable[[ActionRequest], Awaitable[None]] | None = field(
        default=None, compare=False, repr=False
    )

    def payload(self) -> dict[str, Any]:
        body = asdict(self)
        body.pop("prepare")
        return body


@dataclass
class Outcome:
    allowed: bool
    decision: str  # auto | approve_once | approve_session | deny | policy_deny | timeout
    tier: int | None
    reason: str
    by: str
    approval_token: str | None = None


def denied(reason: str, decision: str = "deny", by: str = "policy") -> Outcome:
    return Outcome(False, decision, None, reason, by)


class Gate(Protocol):
    async def authorize(
        self, req: ActionRequest, on_wait: Callable[[], None] | None = None
    ) -> Outcome:
        """Decide `req`; `on_wait` is called once if the owner has to be asked."""
        ...

    def pending(self) -> list[dict[str, Any]]:
        """The steps waiting for the owner right now, as approval_request frames."""
        ...

    def resolve(self, request_id: str, decision: str, by: str, reason: str = "") -> bool:
        """An owner's answer to a pending step; False for unknown ids and decisions."""
        ...

    def deny_all(self, by: str) -> int:
        """The kill switch: every pending step is denied; returns how many."""
        ...


async def watched[T](
    work: Awaitable[T], on_wait: Callable[[], None] | None, after: float = WAIT_AFTER
) -> T:
    """Await `work`; if it has not finished after `after` seconds, call `on_wait` once.

    A cancellation of the caller cancels `work` too.
    """
    task = asyncio.ensure_future(work)
    try:
        done, _ = await asyncio.wait({task}, timeout=after)
        if not done and on_wait is not None:
            on_wait()
        return await task
    finally:
        if not task.done():
            task.cancel()
            with contextlib.suppress(BaseException):
                await task


def outcome_from(data: Any) -> Outcome:
    """The daemon's answer as an Outcome; anything but a clear "allowed" denies."""
    if not isinstance(data, dict):
        return denied("the policy gate gave no answer")
    decision = data.get("decision")
    tier = data.get("tier")
    reason = str(data.get("reason") or "")[:500]
    by = str(data.get("by") or "policy")[:80]
    token = data.get("approval_token")
    tier = tier if isinstance(tier, int) and not isinstance(tier, bool) and 0 <= tier <= 2 else None
    if data.get("allowed") is True and decision in ALLOWING:
        return Outcome(
            True, str(decision), tier, reason, by, token if isinstance(token, str) else None
        )
    if decision not in ("deny", "policy_deny", "timeout"):
        reason = f"unrecognised decision {str(decision)[:40]!r}" + (f": {reason}" if reason else "")
        decision = "deny"
    return Outcome(False, str(decision), tier, reason or str(decision), by)


class DaemonGate:
    """Asks `lapis-daemon`'s Policy Gate (`POST /v1/policy/authorize`) for every step."""

    def __init__(self, client: DaemonClient, wait_after: float = WAIT_AFTER) -> None:
        self.client = client
        self.wait_after = wait_after
        self._waiting: dict[int, dict[str, Any]] = {}
        self._n = 0

    async def authorize(
        self, req: ActionRequest, on_wait: Callable[[], None] | None = None
    ) -> Outcome:
        if req.tool not in TOOLS:
            return denied(f"{req.tool} is not in the policy table", "policy_deny")
        if req.prepare is not None:
            with contextlib.suppress(Exception):  # a missing preview never skips the question
                await req.prepare(req)
        self._n += 1
        key = self._n

        def waiting() -> None:
            self._waiting[key] = _view(req)
            if on_wait is not None:
                on_wait()

        try:
            data = await watched(self.client.authorize(req.payload()), waiting, self.wait_after)
        except DaemonUnavailable as exc:
            return denied(f"policy gate unavailable: {exc}")
        except Exception as exc:  # noqa: BLE001 - whatever broke, the step does not run
            return denied(f"policy gate error: {type(exc).__name__}")
        finally:
            self._waiting.pop(key, None)
        return outcome_from(data)

    def pending(self) -> list[dict[str, Any]]:
        return list(self._waiting.values())

    def resolve(self, request_id: str, decision: str, by: str, reason: str = "") -> bool:
        return False  # approvals are answered at the daemon (HUD, phone)

    def deny_all(self, by: str) -> int:
        return 0  # the daemon's kill switch denies its own pending approvals


def _view(req: ActionRequest) -> dict[str, Any]:
    """What the harness knows of a step the daemon holds for the owner."""
    return {
        "type": "approval_request",
        "request_id": "",
        "host": req.host,
        "action": req.tool,
        "summary": req.summary or req.tool,
        "task_id": req.task_id or "",
        "chat_id": req.chat_id,
        "step": req.step,
        "source": req.source,
        "held_by": "lapis-daemon",
    }


class LocalGate:
    """Lapis-Zero's gate, in process: policy tiers, approval futures, session approval."""

    def __init__(
        self,
        emit: Callable[[dict[str, Any]], None] = _noop,
        emit_resolved: Callable[[dict[str, Any]], None] = _noop,
    ) -> None:
        self.emit = emit
        self.emit_resolved = emit_resolved
        # request_id -> the approval_request frame, for (re)connecting clients.
        self._pending: dict[str, dict[str, Any]] = {}
        self._futures: dict[str, asyncio.Future[tuple[str, str, str]]] = {}
        self._grants: set[str] = set()  # tasks the owner approved "for the session"
        self._n = 0

    async def authorize(
        self, req: ActionRequest, on_wait: Callable[[], None] | None = None
    ) -> Outcome:
        if req.tool not in TOOLS:
            return denied(f"{req.tool} is not in the policy table", "policy_deny")
        if req.tool == "agent_run":
            # The owner's own message, handed over as it is: what the tool then does is
            # kept within the tier by its own permission settings (agent/harness.py).
            return Outcome(True, "auto", 0, "the owner's own message", "policy")
        try:
            policy = Policy(req.host_policy or "balanced")
        except ValueError:
            return denied(f"unknown host policy {req.host_policy!r}", "policy_deny")
        verdict = policy.evaluate(req.tool, req.command, high_risk=req.risky)
        if verdict.denied:
            return denied(verdict.reason, "policy_deny")
        if not verdict.requires_approval:
            return Outcome(True, "auto", 0, verdict.reason, "policy")
        scope = req.task_id or req.chat_id
        if scope and scope in self._grants:
            return Outcome(True, "approve_session", 1, "approved for this task", "owner")
        if req.prepare is not None:
            with contextlib.suppress(Exception):  # no frame is not a reason to skip approval
                await req.prepare(req)
        self._n += 1
        request_id = f"req_{self._n}"
        frame = {
            "type": "approval_request",
            "request_id": request_id,
            "host": req.host,
            "action": KINDS.get(req.tool, (req.tool, req.tool))[1],
            "summary": req.summary or req.tool,
            "risk": "low"
            if req.tool == "ssh_exec" and is_read_only_ssh(req.command or "")
            else "high",
            "preview_1bit_120x68_b64": req.preview_1bit_120x68_b64,
            "task_id": req.task_id,
            "chat_id": req.chat_id,
            "step": req.step,
            "screenshot_id": req.screenshot_id,
            "click_x": req.click_x,
            "click_y": req.click_y,
        }
        fut: asyncio.Future[tuple[str, str, str]] = asyncio.get_running_loop().create_future()
        self._futures[request_id] = fut
        self._pending[request_id] = frame
        if on_wait is not None:
            on_wait()
        self.emit(frame)
        decision, by, note = "deny", "kill_switch", ""
        try:
            decision, by, note = await fut
        finally:
            self._futures.pop(request_id, None)
            self._pending.pop(request_id, None)
            self.emit_resolved(
                {
                    "type": "approval_resolved",
                    "request_id": request_id,
                    "decision": decision,
                    "by": by,
                }  # fmt: skip
            )
        if decision == "approve_session" and scope:
            self._grants.add(scope)
        allowed = decision in ("approve_once", "approve_session")
        token = secrets.token_urlsafe(32) if allowed else None
        return Outcome(allowed, decision, 1, note or verdict.reason, by, token)

    def pending(self) -> list[dict[str, Any]]:
        return list(self._pending.values())

    def resolve(self, request_id: str, decision: str, by: str, reason: str = "") -> bool:
        fut = self._futures.get(request_id)
        if fut is None or fut.done() or decision not in DECISIONS:
            return False
        fut.set_result((decision, by, reason))
        return True

    def deny_all(self, by: str) -> int:
        self._grants.clear()
        n = 0
        for fut in list(self._futures.values()):
            if not fut.done():
                fut.set_result(("deny", by or "kill_switch", ""))
                n += 1
        return n
