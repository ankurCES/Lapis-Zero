#!/usr/bin/env bash
# Host preview of the whole deck on a laptop: the daemon with mock hardware, the harness in
# mock mode (demo hosts and chats), and the HUD in a 1280x720 window.
#   scripts/preview.sh [lapis-ui flags, e.g. --workspace 2]
# Gamepad stand-ins in the window: F5=A F6=B F7=X F8=Y F9=Select F10=Start.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT}"
cargo build -p lapis-daemon -p lapis-ui
./target/debug/lapis-daemon --mock-hw &
DAEMON=$!
(cd harness && exec uv run lapis-harness --mock) &
HARNESS=$!
trap 'kill "${DAEMON}" "${HARNESS}" 2>/dev/null || true' EXIT
./target/debug/lapis-ui --mock-hw "$@"
