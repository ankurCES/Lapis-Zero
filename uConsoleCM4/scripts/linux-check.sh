#!/usr/bin/env bash
# Builds and lints the Linux-only code (BlueZ GATT, DRM/KMS) in an arm64 Debian trixie
# container — the uConsole's own userland — and optionally builds release binaries.
# Usage: scripts/linux-check.sh [check|release]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:-check}"
IMAGE="${LAPIS_BUILD_IMAGE:-lapis-deck-build:1.97-trixie}"

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    docker build --platform linux/arm64 -t "$IMAGE" - <<'DOCKERFILE'
FROM rust:1.97-trixie
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libdbus-1-dev libudev-dev libgbm-dev libegl-dev libgles-dev libdrm-dev \
      libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev \
    && rm -rf /var/lib/apt/lists/* && rustup component add clippy rustfmt
DOCKERFILE
fi

FEATURES="${LAPIS_LINUX_FEATURES:-lapis-daemon/bluez}"
case "$MODE" in
    check) CMD="cargo clippy --workspace --all-targets --features $FEATURES -- -D warnings && cargo test --workspace --features $FEATURES" ;;
    # The device binaries: the daemon with BlueZ, the HUD on DRM/KMS only (no desktop window).
    release) CMD="cargo build --release -p lapis-daemon --features bluez && cargo build --release -p lapis-ui --no-default-features --features kms" ;;
    *) echo "usage: $0 [check|release]" >&2; exit 2 ;;
esac

docker run --rm --platform linux/arm64 \
    -v "$ROOT":/src -w /src \
    -v lapis-deck-cargo:/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR=/src/target/linux-arm64 \
    "$IMAGE" bash -c "$CMD"
