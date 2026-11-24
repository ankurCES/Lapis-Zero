"""CLI entrypoint for lapis-daemon."""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import signal
import sys
from collections.abc import Callable
from pathlib import Path

from lapis_daemon import __version__
from lapis_daemon.agent.runner import TaskRunner
from lapis_daemon.agent.scheduler import CronScheduler, Schedule
from lapis_daemon.config import (
    DEFAULT_CONFIG,
    CoreConfig,
    EffectivePaths,
    HostsConfig,
    effective_paths,
    load_config,
    load_hosts,
    mock_config,
    mock_hosts,
)
from lapis_daemon.db import LapisDb
from lapis_daemon.ipc_server import IpcServer
from lapis_daemon.kvm.base import HostDriver, MockHostDriver, build_host_driver
from lapis_daemon.system_status import mock_state

_suppress = contextlib.suppress

# Pause between mock steps so the UI ticker is watchable.
MOCK_STEP_DELAY_SEC = 0.8


def build_parser() -> argparse.ArgumentParser:
    """Build the CLI argument parser for lapis-daemon."""
    parser = argparse.ArgumentParser(
        prog="lapis-daemon",
        description="Lapis-Zero OS autonomous Option C KVM agent daemon.",
    )
    parser.add_argument(
        "--mock",
        action="store_true",
        help="Run in local mock KVM mode without connecting to Tailscale hosts.",
    )
    parser.add_argument(
        "--config",
        default=None,
        help="Path to the daemon configuration file "
        f"(default: $LAPIS_CONFIG_DIR/lapis.toml, else {DEFAULT_CONFIG}).",
    )
    return parser


def _load_or_mock[T](
    path: Path, loader: Callable[[Path], T], fallback: Callable[[], T], mock: bool
) -> T:
    """Prefer the real file; in mock mode fall back to built-in defaults."""
    try:
        return loader(path)
    except (OSError, ValueError) as exc:  # TOML and pydantic errors are ValueErrors
        if not mock:
            raise
        if path.exists():
            print(f"lapis-daemon: ignoring invalid {path}: {exc}", file=sys.stderr)
        return fallback()


async def serve_until_signal(
    paths: EffectivePaths, config: CoreConfig, hosts: HostsConfig, *, is_mock: bool
) -> None:
    """Serve IPC until SIGINT/SIGTERM, then stop cleanly."""
    paths.db_path.parent.mkdir(parents=True, exist_ok=True)
    db = LapisDb(paths.db_path)
    await db.init()
    if is_mock:
        driver_factory = MockHostDriver
    else:

        def driver_factory(name: str) -> HostDriver:
            return build_host_driver(name, hosts.hosts[name], config)

    runner = TaskRunner(
        db,
        hosts,
        driver_factory=driver_factory,
        max_steps=config.llm.max_steps,
        step_delay=MOCK_STEP_DELAY_SEC if is_mock else 0.0,
    )
    server = IpcServer(paths.socket_path, db, hosts, runner, mock_state)

    # Unattended cron schedules (SPEC 7.3); real tasks land only when the daemon is not mock.
    async def _run_scheduled(name: str, host: str, prompt: str) -> None:
        if is_mock:
            return
        with _suppress(ValueError):
            await runner.submit_task(host, prompt)

    scheduler = CronScheduler(_run_scheduled)
    scheduler.set_schedules(
        [
            Schedule(s.name, s.schedule, s.host, s.prompt)
            for s in config.cron.schedules
            if s.enabled and s.host in hosts.hosts
        ]
    )
    scheduler.start()

    await server.start()
    mode = "mock KVM" if is_mock else "real Option C KVM"
    print(f"lapis-daemon {__version__} listening on {paths.socket_path} ({mode})", flush=True)

    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)
    try:
        await stop.wait()
    finally:
        scheduler.stop()
        if runner.is_running:
            await runner.kill_all()
        await server.stop()
        db.close()


def main(argv: list[str] | None = None) -> int:
    """Run the lapis-daemon CLI entrypoint."""
    args = build_parser().parse_args(argv)
    paths = effective_paths(args.config)
    try:
        config = _load_or_mock(paths.config, load_config, mock_config, args.mock)
        hosts = _load_or_mock(paths.hosts, load_hosts, mock_hosts, args.mock)
    except (OSError, ValueError) as exc:
        print(f"lapis-daemon: cannot load configuration: {exc}", file=sys.stderr)
        return 2
    try:
        asyncio.run(serve_until_signal(paths, config, hosts, is_mock=args.mock))
    except OSError as exc:
        print(f"lapis-daemon: {exc} (set LAPIS_CONFIG_DIR for a rootless run)", file=sys.stderr)
        return 1
    return 0
