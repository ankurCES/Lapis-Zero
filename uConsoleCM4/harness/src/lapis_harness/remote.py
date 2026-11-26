"""Remote harness adapters (SPEC 5.1): an agent CLI on a fleet host, behind one interface.

One adapter per CLI: `claude`, `opencode`, `freya`, `gemini`, `cursor-agent`, `codex`.
Each is a thin layer over its entry in the tested `agent/harness.py` table (command line,
permission flags per policy tier, session resume, output parsing) and the SSH driver's
`login_shell` (the account's own login shell, csh-safe, `TERM=dumb`, `NO_COLOR=1`, stdin
from /dev/null, the run stopped on the host at its time limit or a cancellation).

    adapter = ADAPTERS["claude"]
    path = await adapter.detect(shell)              # where it is installed, or None
    async for item in adapter.run(shell, "fix the test", tier="balanced", session=s):
        if isinstance(item, str): ...               # a progress line
        else: answer = item                         # the final Answer, always last
"""

from __future__ import annotations

import asyncio
import contextlib
from collections.abc import AsyncIterator, Callable
from dataclasses import dataclass
from typing import Protocol

from lapis_harness.agent import harness
from lapis_harness.agent.harness import Answer, Harness
from lapis_harness.kvm.ssh_driver import ShellRun

DETECT_TIMEOUT = 20.0  # a login shell with slow start-up files, then one `command -v`
# Past the run's own time limit, how long the shell gets to stop it on the host and say so
# before the adapter gives up waiting (a link gone silent; see ssh_driver._stop).
STOP_BACKSTOP = 30.0


class RemoteShell(Protocol):
    """What an adapter needs of a host: the SSH driver's long-run entry point."""

    async def login_shell(
        self,
        script: str,
        timeout_s: float,
        on_progress: Callable[[str], None] | None = None,
    ) -> ShellRun: ...


class RemoteHarnessAdapter(Protocol):
    """A uniform async interface to one agent CLI on a host."""

    @property
    def name(self) -> str:
        """The CLI's name: claude, opencode, freya, gemini, cursor-agent, codex."""
        ...

    @property
    def tool(self) -> Harness:
        """Its entry in the harness table (the id chats are bound to)."""
        ...

    async def detect(self, shell: RemoteShell) -> str | None:
        """Where the CLI is installed on the host, or None."""
        ...

    def command(self, prompt: str, *, tier: str, session: str = "", workdir: str = "") -> str:
        """The POSIX sh script of one headless run (ValueError for a tier it cannot keep)."""
        ...

    def run(
        self,
        shell: RemoteShell,
        prompt: str,
        *,
        tier: str,
        session: str = "",
        workdir: str = "",
        timeout_s: float = 1800.0,
        host: str = "the host",
        retry_lost: bool = True,
    ) -> AsyncIterator[str | Answer]:
        """Progress lines as they come, then the final Answer (always exactly one)."""
        ...


@dataclass(frozen=True)
class HarnessAdapter:
    """The adapter for one harness-table entry; every CLI uses this one class."""

    name: str
    tool: Harness

    async def detect(self, shell: RemoteShell) -> str | None:
        run = await shell.login_shell(harness.detect_command({self.tool.id: self.tool}),
                                      DETECT_TIMEOUT)  # fmt: skip
        if not run.started:
            raise OSError("the login shell of the account there did not run the command")
        found = harness.detected({self.tool.id: self.tool}, run.output)
        return found[0]["path"] if found else None

    def command(self, prompt: str, *, tier: str, session: str = "", workdir: str = "") -> str:
        return harness.command(self.tool, prompt, tier=tier, session=session, workdir=workdir)

    async def run(
        self,
        shell: RemoteShell,
        prompt: str,
        *,
        tier: str,
        session: str = "",
        workdir: str = "",
        timeout_s: float = 1800.0,
        host: str = "the host",
        retry_lost: bool = True,
    ) -> AsyncIterator[str | Answer]:
        answer: Answer | None = None
        async for item in self._once(shell, prompt, tier, session, workdir, timeout_s, host):
            if isinstance(item, Answer):
                answer = item
            else:
                yield item
        assert answer is not None
        if retry_lost and session and answer.lost_session:
            # The CLI dropped the session this chat was continuing (its own clean-up,
            # another folder): the message starts a new one.
            async for item in self._once(shell, prompt, tier, "", workdir, timeout_s, host):
                yield item
            return
        yield answer

    async def _once(
        self,
        shell: RemoteShell,
        prompt: str,
        tier: str,
        session: str,
        workdir: str,
        timeout_s: float,
        host: str,
    ) -> AsyncIterator[str | Answer]:
        script = self.command(prompt, tier=tier, session=session, workdir=workdir)
        lines: asyncio.Queue[str] = asyncio.Queue()

        def progress(line: str) -> None:
            if shown := self.tool.show(line):
                lines.put_nowait(shown)

        run = asyncio.ensure_future(shell.login_shell(script, timeout_s, progress))
        loop = asyncio.get_running_loop()
        deadline = loop.time() + timeout_s + STOP_BACKSTOP
        # No `yield` inside a timeout scope: the consumer runs between two of them.
        try:
            while not run.done():
                left = deadline - loop.time()
                if left <= 0:
                    # The shell did not come back even after its own limit and the stop:
                    # nothing about the host can be claimed.
                    yield Answer(
                        f"{self.tool.name} did not finish within {timeout_s:g} seconds and "
                        f"{host} did not confirm that it was stopped: it may still be running "
                        "there.",
                        is_error=True,
                    )
                    return
                getter = asyncio.ensure_future(lines.get())
                await asyncio.wait({run, getter}, timeout=left, return_when=asyncio.FIRST_COMPLETED)
                if getter.done():
                    yield getter.result()
                else:
                    getter.cancel()
            while not lines.empty():
                yield lines.get_nowait()
            result = run.result()
        finally:
            if not run.done():
                run.cancel()
                with contextlib.suppress(BaseException):
                    await run
        yield harness.answer(self.tool, result, host, timeout_s)


# The six CLIs by name. `cursor-agent` is the table's `cursor` (the id chats store).
ADAPTERS: dict[str, HarnessAdapter] = {
    name: HarnessAdapter(name, harness.BUILTIN[tool_id])
    for name, tool_id in (
        ("claude", "claude"),
        ("opencode", "opencode"),
        ("freya", "freya"),
        ("gemini", "gemini"),
        ("cursor-agent", "cursor"),
        ("codex", "codex"),
    )
}


def adapter_for(tool: Harness) -> HarnessAdapter:
    """The adapter of a harness-table entry (a configured `[agents.<id>]` tool included)."""
    for adapter in ADAPTERS.values():
        if adapter.tool == tool:
            return adapter
    return HarnessAdapter(tool.id, tool)


async def run_to_answer(
    adapter: RemoteHarnessAdapter,
    shell: RemoteShell,
    prompt: str,
    *,
    tier: str,
    session: str = "",
    workdir: str = "",
    timeout_s: float = 1800.0,
    host: str = "the host",
    on_progress: Callable[[str], None] | None = None,
    retry_lost: bool = True,
) -> Answer:
    """Drive `adapter.run` to its end: progress lines to `on_progress`, the Answer returned."""
    answer: Answer | None = None
    async for item in adapter.run(
        shell, prompt, tier=tier, session=session, workdir=workdir, timeout_s=timeout_s,
        host=host, retry_lost=retry_lost,
    ):  # fmt: skip
        if isinstance(item, Answer):
            answer = item
        elif on_progress is not None:
            on_progress(item)
    assert answer is not None
    return answer
