"""`lapis-harness`: the agent stack behind lapis-daemon, on a Unix socket.

    lapis-harness [--mock] [--socket PATH] [--daemon-url URL] [--token-file PATH]
                  [--state-dir DIR] [--config DIR]

Every step it takes on a fleet host is authorised by the daemon's Policy Gate
(`DaemonGate`, fail closed) and every cyberdeck tool is run by the daemon.
"""

from __future__ import annotations

import argparse
import asyncio
import signal
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Any

from lapis_harness import __version__, log
from lapis_harness.api.server import ApiServer
from lapis_harness.config import (
    DEFAULT_CONFIG_DIR,
    DEFAULT_DAEMON_URL,
    DEFAULT_SOCKET,
    DEFAULT_STATE_DIR,
    DEFAULT_TOKEN_FILE,
    CoreConfig,
    EffectivePaths,
    HostsConfig,
    effective_paths,
    load_config,
    load_hosts,
    mock_config,
    mock_dir,
    mock_hosts,
)
from lapis_harness.core import Core
from lapis_harness.daemon_client import DaemonClient, check_daemon_url
from lapis_harness.db import LapisDb
from lapis_harness.deck import DaemonDeckTools
from lapis_harness.gate import DaemonGate
from lapis_harness.voice import VoiceController, build_voice_controller


def build_parser() -> argparse.ArgumentParser:
    """Build the CLI argument parser for lapis-harness."""
    mock = mock_dir()
    parser = argparse.ArgumentParser(
        prog="lapis-harness",
        description="Lapis-Deck agent harness: chats, fleet KVM, remote agent CLIs and the "
        "cyberdeck operator, behind lapis-daemon on a Unix socket.",
    )
    parser.add_argument(
        "--mock",
        action="store_true",
        help=f"Mock fleet hosts and planner (no Tailscale, no LLM); state under {mock}/harness.",
    )
    parser.add_argument(
        "--socket",
        default=None,
        help=f"Unix socket to serve (default {DEFAULT_SOCKET}; --mock: {mock}/harness.sock).",
    )
    parser.add_argument(
        "--daemon-url",
        default=DEFAULT_DAEMON_URL,
        help=f"lapis-daemon's control plane, loopback only (default {DEFAULT_DAEMON_URL}).",
    )
    parser.add_argument(
        "--token-file",
        default=None,
        help=f"The daemon's local token (default {DEFAULT_TOKEN_FILE}; --mock: "
        f"{mock}/local.token).",
    )
    parser.add_argument(
        "--state-dir",
        default=None,
        help=f"Database, frames and fleet SSH key (default {DEFAULT_STATE_DIR}; --mock: "
        f"{mock}/harness).",
    )
    parser.add_argument(
        "--config",
        default=None,
        metavar="DIR",
        help=f"lapis.toml, hosts.toml and secrets.env (default {DEFAULT_CONFIG_DIR}; --mock: "
        "the state directory).",
    )
    return parser


def resolve(args: argparse.Namespace) -> tuple[EffectivePaths, Path, Path]:
    """(paths, socket, token file) for the parsed arguments."""
    mock = mock_dir()
    state = Path(args.state_dir or (mock / "harness" if args.mock else DEFAULT_STATE_DIR))
    config = Path(args.config or (state if args.mock else DEFAULT_CONFIG_DIR))
    socket_path = Path(args.socket or (mock / "harness.sock" if args.mock else DEFAULT_SOCKET))
    token = Path(args.token_file or (mock / "local.token" if args.mock else DEFAULT_TOKEN_FILE))
    return effective_paths(config, state), socket_path, token


def _load[M](
    path: Path,
    loader: Callable[[Path], M],
    mock_default: Callable[[], M],
    real_default: Callable[[], M],
    mock: bool,
) -> M:
    """A missing file means defaults; a broken one is reported and also means defaults.

    The harness must come up on a device nobody has configured rather than crash-loop
    under systemd.
    """
    if not path.exists():
        return mock_default() if mock else real_default()
    try:
        return loader(path)
    except (OSError, ValueError) as exc:  # TOML and pydantic errors are ValueErrors
        log.log(f"ignoring invalid {path}: {exc}", error=True)
        return mock_default() if mock else real_default()


def _voice(core: Core, config: CoreConfig, mock: bool) -> VoiceController | None:
    async def queue_transcript(text: str) -> None:
        try:
            await core.submit_prompt(text, source="voice")
        except (ValueError, LookupError) as exc:
            core.toast("warn", f"voice: {exc}")

    def on_error(exc: BaseException) -> None:
        core.toast("error", f"voice: {exc}")

    v = config.voice
    if mock:  # fake capture, transcript and cues
        return build_voice_controller(
            v,
            capture_device="",
            stt_api_url="",
            audio_cues=False,
            send_mic=core.send_mic,
            on_transcript=queue_transcript,
            on_error=on_error,
        )
    if not (v.enabled and v.stt_api_url):
        return None  # a real device never falls back to a made-up transcript
    return build_voice_controller(
        v,
        sample_rate=v.sample_rate,
        channels=v.channels,
        capture_device=v.capture_device,
        stt_api_url=v.stt_api_url,
        stt_model=v.stt_model,
        stt_api_key_env=v.stt_api_key_env,
        stt_timeout_sec=v.stt_timeout_sec,
        audio_cues=v.audio_cues and not config.ui.mute_audio,
        max_capture_bytes=v.max_capture_bytes,
        send_mic=core.send_mic,
        on_transcript=queue_transcript,
        on_error=on_error,
    )


async def serve_until_signal(
    paths: EffectivePaths,
    config: CoreConfig,
    hosts: HostsConfig,
    *,
    socket_path: Path,
    client: DaemonClient,
    is_mock: bool,
    ready: Callable[[], Any] | None = None,
) -> None:
    """Serve the Unix socket until SIGINT/SIGTERM."""
    # Installed before anything else: a stop request during start-up must still run the
    # shutdown path (socket removal, modifier key-ups) instead of killing us.
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)
    paths.db_path.parent.mkdir(parents=True, exist_ok=True)
    db = LapisDb(paths.db_path)
    await db.init()
    core = Core(
        paths,
        config,
        hosts,
        db,
        mock=is_mock,
        gate=DaemonGate(client),  # always the daemon's: never the in-process gate
        deck_tools=DaemonDeckTools(client),
    )
    core.voice = _voice(core, config, is_mock)
    if core.voice is not None:
        await core.voice.start()
    await core.start()
    api = ApiServer(core, socket_path)
    await api.start()
    mode = "mock fleet" if is_mock else "real fleet KVM"
    log.log(f"{__version__} listening on {socket_path} ({mode}); gate {client.base_url}")
    if ready is not None:
        ready()
    try:
        await stop.wait()
    finally:
        if core.voice is not None:
            await core.voice.close()
        await api.stop()
        await core.stop()
        await client.close()
        db.close()


def main(argv: list[str] | None = None) -> int:
    """Run the lapis-harness CLI entrypoint."""
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        check_daemon_url(args.daemon_url)
    except ValueError as exc:
        parser.error(str(exc))
    log.install()
    paths, socket_path, token = resolve(args)
    config = _load(paths.config, load_config, mock_config, CoreConfig, args.mock)
    hosts = _load(paths.hosts, load_hosts, mock_hosts, HostsConfig, args.mock)
    client = DaemonClient(args.daemon_url, token)
    try:
        asyncio.run(
            serve_until_signal(
                paths, config, hosts, socket_path=socket_path, client=client, is_mock=args.mock
            )
        )
    except OSError as exc:
        log.log(f"{exc} (pass --state-dir, --config and --socket for a rootless run)", error=True)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
