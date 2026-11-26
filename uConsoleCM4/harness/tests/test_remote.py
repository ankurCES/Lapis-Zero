"""Remote harness adapters: one interface for the six agent CLIs, over a fake SSH shell."""

from __future__ import annotations

import asyncio
import json
from collections.abc import Awaitable, Callable
from typing import Any

import pytest

from lapis_harness import remote
from lapis_harness.agent import harness
from lapis_harness.agent.harness import Answer
from lapis_harness.kvm.base import CompositeHostDriver
from lapis_harness.kvm.ssh_driver import ShellRun, login_shell_command
from lapis_harness.remote import ADAPTERS, adapter_for, run_to_answer

Step = Callable[[Callable[[str], None] | None], Awaitable[ShellRun]]


def done(output: str, status: int = 0) -> Step:
    async def step(_progress: Callable[[str], None] | None) -> ShellRun:
        return ShellRun(output, status, started=True)

    return step


class FakeShell:
    """The SSH driver's `login_shell`, answering from a script of steps."""

    connected = True

    def __init__(self, *steps: Step) -> None:
        self.steps = list(steps)
        self.scripts: list[tuple[str, float]] = []
        self.cancelled = False

    async def login_shell(
        self, script: str, timeout_s: float, on_progress: Callable[[str], None] | None = None
    ) -> ShellRun:
        self.scripts.append((script, timeout_s))
        try:
            return await self.steps.pop(0)(on_progress)
        except asyncio.CancelledError:
            self.cancelled = True
            raise


async def collect(gen: Any) -> list[str | Answer]:
    return [item async for item in gen]


def run_line(script: str) -> str:
    """The tool's own command line inside the adapter's script."""
    run = script.split("; then ", 1)[1].rsplit("; else", 1)[0]
    return run.rsplit("; fi; ", 1)[-1]


def test_one_adapter_per_cli_each_on_its_table_entry() -> None:
    assert list(ADAPTERS) == ["claude", "opencode", "freya", "gemini", "cursor-agent", "codex"]
    assert {a.name: a.tool.binary for a in ADAPTERS.values()} == {
        "claude": "claude", "opencode": "opencode", "freya": "lumi-freya", "gemini": "gemini",
        "cursor-agent": "cursor-agent", "codex": "codex",
    }  # fmt: skip
    assert ADAPTERS["cursor-agent"].tool.id == "cursor"  # the id chats are bound to
    assert adapter_for(harness.BUILTIN["gemini"]) is ADAPTERS["gemini"]
    custom = harness.custom("aider", "Aider", "aider --yes --message {prompt}")
    assert adapter_for(custom).name == "aider"


def test_each_cli_gets_its_command_line_tier_flags_and_resume() -> None:
    def line(name: str, **kw: str) -> str:
        return run_line(ADAPTERS[name].command("do it", **kw))

    assert line("claude", tier="strict", session="s-1") == (
        'printf %s "$p" | "$t" -p --output-format json --permission-mode plan --resume s-1'
    )
    assert line("opencode", tier="autonomous", session="ses_9") == (
        'printf %s "$p" | "$t" run --format json --dangerously-skip-permissions -s ses_9'
    )
    assert line("freya", tier="autonomous") == '"$t" agent --yolo "$p"'
    assert line("gemini", tier="balanced") == '"$t" -o json --approval-mode auto_edit -p "$p"'
    assert line("cursor-agent", tier="strict") == (
        '"$t" -p --output-format text --trust --mode plan "$p"'
    )
    assert line("codex", tier="autonomous") == '"$t" exec "$p"'
    # Claude Code's long-lived token, from the account's own file, never sent from here.
    claude = ADAPTERS["claude"].command("x", tier="strict")
    assert 'CLAUDE_CODE_OAUTH_TOKEN=$(cat "$HOME/".claude/lapis-token)' in claude
    # No command line at all for a tier the CLI cannot be held to.
    for name, tier in (("opencode", "strict"), ("codex", "balanced"), ("freya", "strict")):
        with pytest.raises(ValueError, match="cannot be held to"):
            ADAPTERS[name].command("x", tier=tier)
    # What the SSH driver wraps it in: the login shell, csh-safe, a dumb terminal, no
    # colours, nothing to read on stdin.
    wrapped = login_shell_command(claude, "n0nce")
    assert wrapped.startswith("exec env TERM=dumb NO_COLOR=1 /bin/sh -c ")
    assert '*csh) exec "$SHELL" -c "$1";;' in wrapped and wrapped.endswith("</dev/null")


async def test_detection_over_the_login_shell() -> None:
    shell = FakeShell(done("claude /home/u/.local/bin/claude\n"), done(""))
    assert await ADAPTERS["claude"].detect(shell) == "/home/u/.local/bin/claude"
    assert await ADAPTERS["codex"].detect(shell) is None
    assert "w claude .local/bin/claude .claude/local/claude" in shell.scripts[0][0]
    assert shell.scripts[0][1] == remote.DETECT_TIMEOUT

    async def silent(_p: Any) -> ShellRun:
        return ShellRun("", None, started=False)

    with pytest.raises(OSError, match="login shell"):
        await ADAPTERS["claude"].detect(FakeShell(silent))


async def test_progress_streams_while_the_cli_runs_then_the_answer() -> None:
    events = [
        {"type": "tool_use", "sessionID": "ses_1", "part": {"tool": "bash",
                                                             "state": {"title": "make test"}}},
        {"type": "step_finish", "sessionID": "ses_1", "part": {}},
        {"type": "text", "sessionID": "ses_1", "part": {"text": "All green.\nDetails."}},
    ]  # fmt: skip
    lines = [json.dumps(e) for e in events]
    seen_first = asyncio.Event()

    async def run(progress: Callable[[str], None] | None) -> ShellRun:
        assert progress is not None
        progress(lines[0])
        await seen_first.wait()  # still running: the consumer already has the first line
        for line in lines[1:]:
            progress(line)
        return ShellRun("\n".join(lines) + "\n", 0, started=True)

    shell = FakeShell(run)
    items: list[str | Answer] = []
    async for item in ADAPTERS["opencode"].run(shell, "test it", tier="autonomous"):
        items.append(item)
        seen_first.set()
    # opencode's events become one line each (a step boundary says nothing).
    assert items[:2] == ["bash make test", "All green."]
    answer = items[-1]
    assert isinstance(answer, Answer) and len(items) == 3
    assert (answer.text, answer.session, answer.is_error) == (
        "All green.\nDetails.",
        "ses_1",
        False,
    )


async def test_a_run_past_its_time_limit_ends_with_an_error_answer(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # The shell stopped it at the limit and said so.
    async def stopped(_p: Any) -> ShellRun:
        return ShellRun("half done", None, started=True, timed_out=True)

    answer = await run_to_answer(
        ADAPTERS["freya"], FakeShell(stopped), "x", tier="balanced", timeout_s=90, host="mbp"
    )
    assert answer.is_error and answer.text.startswith(
        "Freya was stopped after 90 seconds without finishing."
    )
    # A shell that never comes back at all: the adapter's own backstop ends it.
    monkeypatch.setattr(remote, "STOP_BACKSTOP", 0.05)

    async def hangs(_p: Any) -> ShellRun:
        await asyncio.Event().wait()
        raise AssertionError("unreachable")

    shell = FakeShell(hangs)
    answer = await asyncio.wait_for(
        run_to_answer(ADAPTERS["codex"], shell, "x", tier="autonomous", timeout_s=0.05, host="mbp"),
        5,
    )
    assert answer.is_error and "mbp did not confirm that it was stopped" in answer.text
    assert shell.cancelled  # what was waiting on the host is stopped, not left behind


async def test_a_lost_session_starts_a_new_one() -> None:
    lost = done("No conversation found with session ID: old-1\n", 1)
    fresh = done(json.dumps({"type": "result", "result": "Fresh start", "session_id": "new-2"}))
    shell = FakeShell(lost, fresh)
    items = await collect(ADAPTERS["claude"].run(shell, "go on", tier="balanced", session="old-1"))
    assert items == [Answer("Fresh start", "new-2")]
    assert "--resume old-1" in shell.scripts[0][0] and "--resume" not in shell.scripts[1][0]
    # Left to the caller (the task runner does it itself): the lost answer as it came.
    shell = FakeShell(done("No conversation found with session ID: old-1\n", 1))
    answer = await run_to_answer(
        ADAPTERS["claude"], shell, "go on", tier="balanced", session="old-1", retry_lost=False
    )
    assert answer.lost_session and answer.is_error and len(shell.scripts) == 1


async def test_gemini_answers_after_its_warnings_and_the_driver_uses_the_adapters() -> None:
    said = 'Basic terminal detected\n{\n  "response": "Hi there",\n  "session_id": "g-1"\n}\n'
    shell = FakeShell(done(said))
    driver = CompositeHostDriver("mbp", None, vnc_factory=None, ssh_factory=lambda: shell)
    progress: list[str] = []
    answer = await driver.run_agent(
        harness.BUILTIN["gemini"], "hello", tier="strict", on_progress=progress.append
    )
    assert (answer.text, answer.session, answer.is_error) == ("Hi there", "g-1", False)
    assert "--approval-mode plan" in shell.scripts[0][0] and progress == []
