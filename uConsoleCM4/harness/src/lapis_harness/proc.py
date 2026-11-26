"""Short subprocesses on the device: never left running, never logged."""

from __future__ import annotations

import asyncio
import contextlib
import os
import signal


async def run_cmd(*argv: str, timeout: float = 5.0, merge_stderr: bool = False) -> tuple[int, str]:
    """Run a command; (returncode, stdout). Missing binary or timeout -> (127|124, '').

    `merge_stderr` folds stderr into the returned text, for tools that explain a
    failure there (`tailscale up`). Nothing is ever logged here.
    """
    try:
        proc = await asyncio.create_subprocess_exec(
            *argv,
            stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.STDOUT if merge_stderr else asyncio.subprocess.DEVNULL,
            start_new_session=True,  # its own process group, so all of it can be killed
        )
    except (FileNotFoundError, PermissionError):
        return 127, ""
    try:
        out, _ = await asyncio.wait_for(proc.communicate(), timeout)
    except TimeoutError:
        await _kill_group(proc)
        return 124, ""
    except asyncio.CancelledError:
        # A cancelled caller must not leave the command running (`tailscale up`
        # would go on waiting for a login nobody is watching any more).
        await _kill_group(proc)
        raise
    return proc.returncode or 0, out.decode(errors="replace")


async def _kill_group(proc: asyncio.subprocess.Process) -> None:
    """Kill the command and anything it started; never wait on it for long.

    A wrapper script's child can outlive it holding our pipe, and `proc.wait()`
    only returns once that pipe closes, so the wait is bounded.
    """
    with contextlib.suppress(ProcessLookupError, PermissionError):
        os.killpg(proc.pid, signal.SIGKILL)
    with contextlib.suppress(TimeoutError):
        await asyncio.wait_for(proc.wait(), 2)
