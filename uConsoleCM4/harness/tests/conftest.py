"""Shared fixtures."""

from __future__ import annotations

import asyncio
import json
import shutil
import socket
import tempfile
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

from lapis_harness.config import EffectivePaths, effective_paths, mock_config, mock_hosts
from lapis_harness.core import Core
from lapis_harness.db import LapisDb


@pytest.fixture
def short_tmp() -> Iterator[Path]:
    """Temp dir with a short path: Unix socket paths cap near 104 bytes on macOS."""
    path = Path(tempfile.mkdtemp(prefix="lapis-", dir="/tmp"))
    yield path
    shutil.rmtree(path, ignore_errors=True)


def make_paths(d: Path) -> EffectivePaths:
    return effective_paths(d, d)


async def make_core(d: Path, **kwargs: Any) -> Core:
    """A started mock Core rooted in `d` (no step delay unless asked).

    Without a `gate`, the in-process LocalGate: approvals are answered through the
    harness's own approvals routes and WebSocket, as on Lapis-Zero.
    """
    db = LapisDb(d / "lapis.db")
    await db.init()
    kwargs.setdefault("step_delay", 0.0)
    core = Core(make_paths(d), mock_config(), mock_hosts(), db, mock=True, **kwargs)
    await core.start()
    return core


def free_port() -> int:
    """A port on 127.0.0.1 that nothing listens on right now, picked by the system.

    For a daemon started as a process of its own, which has to be told its port. Never
    a fixed number: the suite runs side by side with itself.
    """
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


async def wait_for(predicate: Any, timeout: float = 5.0) -> None:
    async with asyncio.timeout(timeout):
        while not predicate():
            await asyncio.sleep(0.005)


async def recv(reader: asyncio.StreamReader) -> dict[str, Any]:
    line = await asyncio.wait_for(reader.readline(), 2)  # re-sync budget (SPEC 6)
    assert line, "daemon closed the connection"
    return json.loads(line)


async def recv_until(reader: asyncio.StreamReader, msg_type: str) -> list[dict[str, Any]]:
    """Read until a message of `msg_type` arrives; returns everything read, it last."""
    seen = []
    while True:
        seen.append(await recv(reader))
        if seen[-1]["type"] == msg_type:
            return seen


def send(writer: asyncio.StreamWriter, **msg: Any) -> None:
    writer.write(json.dumps(msg).encode() + b"\n")


async def recv_match(reader: asyncio.StreamReader, msg_type: str, predicate: Any) -> dict[str, Any]:
    """Next `msg_type` frame satisfying `predicate` (earlier, stale ones are skipped)."""
    while True:
        msg = (await recv_until(reader, msg_type))[-1]
        if predicate(msg):
            return msg
