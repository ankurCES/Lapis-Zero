"""CLI entrypoint for lapis-daemon."""

from __future__ import annotations

import argparse
import asyncio
import os
import signal
import sys
from pathlib import Path

from lapis_daemon import __version__
from lapis_daemon.api.server import ApiServer
from lapis_daemon.config import (
    API_BIND_HOST,
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
from lapis_daemon.core import Core
from lapis_daemon.db import LapisDb
from lapis_daemon.ipc_server import IpcServer
from lapis_daemon.voice import VoiceController, build_voice_controller

MOCK_DEFAULT_DIR = "/tmp/lapis"  # also lapis-ui's default socket directory off-device


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
    parser.add_argument("--api-port", type=int, default=None, help="Control-plane port.")
    dev = parser.add_argument_group("development (require --mock)")
    dev.add_argument(
        "--dev-pair-token",
        default=None,
        metavar="TOKEN",
        help="Pre-register this bearer token so an emulator or script can connect.",
    )
    dev.add_argument(
        "--dev-bind",
        default=None,
        metavar="HOST",
        help=f"Bind the control plane to HOST instead of {API_BIND_HOST} (e.g. 0.0.0.0 "
        "for a phone on the same LAN). Never available outside --mock.",
    )
    dev.add_argument(
        "--dev-advertise",
        default=None,
        metavar="HOST:PORT",
        help="Address placed in pairing QR codes (default 127.0.0.1:<port>).",
    )
    return parser


def _load(path: Path, loader, mock_default, real_default, mock: bool):
    """A missing file means defaults; a broken one is reported and also means defaults.

    The daemon must come up on a card nobody has configured (and keep the LCD
    informed) rather than crash-loop under systemd.
    """
    if not path.exists():
        return mock_default() if mock else real_default()
    try:
        return loader(path)
    except (OSError, ValueError) as exc:  # TOML and pydantic errors are ValueErrors
        print(f"lapis-daemon: ignoring invalid {path}: {exc}", file=sys.stderr, flush=True)
        return mock_default() if mock else real_default()


def _voice(core: Core, server: IpcServer, config: CoreConfig, mock: bool) -> VoiceController | None:
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
            send_mic=server.send_mic,
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
        send_mic=server.send_mic,
        on_transcript=queue_transcript,
        on_error=on_error,
    )


async def serve_until_signal(
    paths: EffectivePaths,
    config: CoreConfig,
    hosts: HostsConfig,
    *,
    is_mock: bool,
    args: argparse.Namespace | None = None,
) -> None:
    """Serve the Unix socket and the control plane until SIGINT/SIGTERM."""
    # Installed before anything else: a stop request during start-up must still
    # run the shutdown path (socket removal, modifier key-ups) instead of killing us.
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)
    paths.db_path.parent.mkdir(parents=True, exist_ok=True)
    db = LapisDb(paths.db_path)
    await db.init()
    port = (args.api_port if args else None) or config.api.port
    bind = (args.dev_bind if args else None) or API_BIND_HOST
    advertise = None
    if is_mock:
        advertise = (args.dev_advertise if args else None) or f"127.0.0.1:{port}"
    core = Core(paths, config, hosts, db, mock=is_mock, dev_advertise=advertise)
    server = IpcServer(paths.socket_path, core)
    core.voice = _voice(core, server, config, is_mock)
    if core.voice is not None:
        await core.voice.start()
    await core.start()
    await core.status.refresh()
    await server.start()
    mode = "mock KVM" if is_mock else "real Option C KVM"
    print(f"lapis-daemon {__version__} listening on {paths.socket_path} ({mode})", flush=True)

    api: ApiServer | None = None
    if config.api.enabled:
        if args is not None and args.dev_pair_token:
            await core.auth.register_fixed("dev_local", "Dev token", args.dev_pair_token)
        api = ApiServer(core, port, bind)
        try:
            await api.start()
            print(f"lapis-daemon control plane on http://{bind}:{port}", flush=True)
        except OSError as exc:  # the LCD must keep working if the port is taken
            api = None
            print(f"lapis-daemon: control plane disabled: {exc}", file=sys.stderr, flush=True)

    # A device that is not provisioned opens its Bluetooth pairing window on boot:
    # no phone registered yet, or one was registered at the PIN step but the tailnet
    # login never finished (wrong Wi-Fi password, HTTPS not enabled, ...) and the
    # device was power-cycled. One window per boot; BLE stays on demand after that.
    never_joined = core.status.tailscale["state"] == "needs_login"
    if not is_mock and (await core.auth.count() == 0 or never_joined):
        await core.pairing_start("ble")

    try:
        await stop.wait()
    finally:
        if core.voice is not None:
            await core.voice.close()
        if api is not None:
            await api.stop()
        await core.stop()
        await server.stop()
        db.close()


def main(argv: list[str] | None = None) -> int:
    """Run the lapis-daemon CLI entrypoint."""
    parser = build_parser()
    args = parser.parse_args(argv)
    if not args.mock and (args.dev_pair_token or args.dev_bind or args.dev_advertise):
        parser.error("--dev-pair-token, --dev-bind and --dev-advertise require --mock")
    if args.mock and not os.environ.get("LAPIS_CONFIG_DIR") and not args.config:
        os.environ["LAPIS_CONFIG_DIR"] = MOCK_DEFAULT_DIR
    paths = effective_paths(args.config)
    config = _load(paths.config, load_config, mock_config, CoreConfig, args.mock)
    hosts = _load(paths.hosts, load_hosts, mock_hosts, HostsConfig, args.mock)
    try:
        asyncio.run(serve_until_signal(paths, config, hosts, is_mock=args.mock, args=args))
    except OSError as exc:
        print(f"lapis-daemon: {exc} (set LAPIS_CONFIG_DIR for a rootless run)", file=sys.stderr)
        return 1
    return 0
