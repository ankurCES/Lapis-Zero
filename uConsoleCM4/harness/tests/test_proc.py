"""`run_cmd`: a cancelled command is killed with everything it started."""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest


def test_run_cmd_kills_its_child_when_cancelled(tmp_path: Path) -> None:
    """A cancelled `tailscale up` must not keep waiting for a login nobody watches."""
    import os
    import shlex
    import sys

    from lapis_harness.proc import run_cmd

    pidfile = tmp_path / "pid"
    code = f"import os, time; open({str(pidfile)!r}, 'w').write(str(os.getpid())); time.sleep(60)"
    # Through a shell wrapper, like /usr/local/bin/tailscale on a Mac: the grandchild
    # inherits our pipe, and killing only the wrapper used to hang the caller.
    wrapper = f"{sys.executable} -c {shlex.quote(code)}; true"

    async def scenario() -> int:
        task = asyncio.create_task(run_cmd("/bin/sh", "-c", wrapper, timeout=60))
        while not (pidfile.exists() and pidfile.read_text()):
            await asyncio.sleep(0.02)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        return int(pidfile.read_text())

    pid = asyncio.run(scenario())
    with pytest.raises(ProcessLookupError):
        os.kill(pid, 0)
