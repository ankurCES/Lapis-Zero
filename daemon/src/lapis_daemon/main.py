"""CLI entrypoint for lapis-daemon."""

from __future__ import annotations

import argparse

from lapis_daemon import __version__


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
        default="/etc/lapis/lapis.toml",
        help="Path to the daemon configuration file (default: %(default)s).",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    """Run the lapis-daemon CLI entrypoint."""
    parser = build_parser()
    args = parser.parse_args(argv)

    if args.mock:
        print(f"lapis-daemon {__version__} (mock mode) — daemon core lands in Phase 2")
        return 0

    print("config loading lands in Phase 2")
    return 0
