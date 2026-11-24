#!/usr/bin/env bash
#
# Build the Lapis-Zero OS image from CardputerZero/pi-gen (branch
# cardputerzero_v1.1) running only stage0/stage1/stage2 + our custom
# stage-lapis.  No desktop stages (stage3..stage5), no X11/Wayland/APPLaunch.
#
# The heavy lifting is delegated to pi-gen's build-docker.sh, which handles
# QEMU binfmt_misc so the build works on amd64/macOS hosts.
#
# Environment overrides:
#   REPO_URL           pi-gen git clone URL (default: CardputerZero/pi-gen)
#   PIGEN_BRANCH       branch/tag to build (default: cardputerzero_v1.1)
#   REPO_PATH          if set, use this existing checkout instead of cloning
#   PIGEN_DIR          working clone destination (default: build/pi-gen)
#                      NOTE: pi-gen refuses paths containing spaces
#   DEPLOY_DIR         where the final image + manifest are copied (default:
#                      os/deploy)
#   LAPIS_UI_BIN       prebuilt aarch64 lapis-ui binary; if unset, cross-compile
#                      via `cargo zigbuild` (requires cargo-zigbuild available)
#   LAPIS_DAEMON_WHEEL prebuilt lapis-daemon wheel; if unset, build via
#                      `uv build` (requires uv)
#   DOCKER             docker executable (default: docker)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

REPO_URL="${REPO_URL:-https://github.com/CardputerZero/pi-gen.git}"
PIGEN_BRANCH="${PIGEN_BRANCH:-cardputerzero_v1.1}"
REPO_PATH="${REPO_PATH:-}"
PIGEN_DIR="${PIGEN_DIR:-${SCRIPT_DIR}/build/pi-gen}"
DEPLOY_DIR="${DEPLOY_DIR:-${SCRIPT_DIR}/deploy}"
DOCKER="${DOCKER:-docker}"

# --- helper: fail with a clear message ---
fail() { echo "ERROR: $*" >&2; exit 1; }

# --- obtain or update the pi-gen checkout ---
if [ -n "${REPO_PATH}" ] && [ -d "${REPO_PATH}" ]; then
    echo "Using existing pi-gen checkout: ${REPO_PATH}"
    PIGEN_DIR="${REPO_PATH}"
else
    if [ -n "${REPO_PATH}" ]; then
        echo "WARNING: REPO_PATH=${REPO_PATH} does not exist; falling back to a fresh clone" >&2
    fi
    case "${PIGEN_DIR}" in
        *" "*) fail "PIGEN_DIR must not contain spaces (pi-gen debootstrap limitation): ${PIGEN_DIR}" ;;
    esac
    if [ ! -x "${PIGEN_DIR}/build-docker.sh" ]; then
        rm -rf "${PIGEN_DIR}"
        mkdir -p "$(dirname "${PIGEN_DIR}")"
        echo "Cloning pi-gen (${PIGEN_BRANCH}) into ${PIGEN_DIR} ..."
        git clone --depth 1 --branch "${PIGEN_BRANCH}" "${REPO_URL}" "${PIGEN_DIR}"
    fi
    ( cd "${PIGEN_DIR}" && git checkout -q "${PIGEN_BRANCH}" )
fi

# --- stage prereqs: cross-compile lapis-ui, build daemon sdist, assemble stage-lapis ---
STAGE_LAPIS="${SCRIPT_DIR}/stage-lapis"

# 1. aarch64 lapis-ui binary (offline-friendly: reused if already present)
mkdir -p "${STAGE_LAPIS}/02-lapis-stack/files"
if [ -n "${LAPIS_UI_BIN:-}" ]; then
    install -m 0755 "${LAPIS_UI_BIN}" "${STAGE_LAPIS}/02-lapis-stack/files/lapis-ui"
    echo "Using prebuilt lapis-ui: ${LAPIS_UI_BIN}"
elif [ -f "${STAGE_LAPIS}/02-lapis-stack/files/lapis-ui" ]; then
    echo "Reusing existing stage-lapis lapis-ui binary"
else
    command -v cargo-zigbuild >/dev/null || fail "cargo-zigbuild not found (or set LAPIS_UI_BIN). Install: cargo install cargo-zigbuild --locked"
    ( cd "${SCRIPT_DIR}/../ui" && cargo-zigbuild build --release --no-default-features --target aarch64-unknown-linux-gnu )
    install -m 0755 "${SCRIPT_DIR}/../ui/target/aarch64-unknown-linux-gnu/release/lapis-ui" \
        "${STAGE_LAPIS}/02-lapis-stack/files/lapis-ui"
fi

# 2. lapis-daemon wheel (installed offline into a --system-site-packages venv)
if [ -n "${LAPIS_DAEMON_WHEEL:-}" ]; then
    install -m 0644 "${LAPIS_DAEMON_WHEEL}" "${STAGE_LAPIS}/02-lapis-stack/files/$(basename "${LAPIS_DAEMON_WHEEL}")"
    echo "Using prebuilt daemon wheel: ${LAPIS_DAEMON_WHEEL}"
elif ls "${STAGE_LAPIS}/02-lapis-stack/files"/lapis_daemon-*.whl >/dev/null 2>&1; then
    echo "Reusing existing stage-lapis daemon wheel"
else
    command -v uv >/dev/null || fail "uv not found (or set LAPIS_DAEMON_WHEEL)"
    ( cd "${SCRIPT_DIR}/../daemon" && uv build )
    # Filenames are version-stamped and path has no spaces; sorted by mtime.
    # shellcheck disable=SC2012
    WHEEL=$(ls -t "${SCRIPT_DIR}/../daemon/dist"/lapis_daemon-*.whl | head -1)
    install -m 0644 "${WHEEL}" "${STAGE_LAPIS}/02-lapis-stack/files/$(basename "${WHEEL}")"
fi

# 3. Pre-seed the lapis-ui/dbus-service names the stage scripts reference.
true

# --- make sure docker is available ---
if ! "${DOCKER}" ps >/dev/null 2>&1; then
    fail "cannot reach the Docker daemon (is Docker Desktop running?). The pi-gen build requires a running daemon."
fi

# --- run the pi-gen build ---
mkdir -p "${DEPLOY_DIR}"
echo "Building Lapis-Zero OS with pi-gen (stages: stage0 stage1 stage2 stage-lapis) ..."
(
    cd "${PIGEN_DIR}"
    set +e
    ./build-docker.sh -c "${SCRIPT_DIR}/config.pigen"
    rc=$?
    set -e
    echo "build-docker.sh exited ${rc}"
    exit "${rc}"
)

# --- collect outputs ---
# pi-gen deploy dir is at <pigen>/deploy by default (DEPLOY_COMPRESSION=xz).
if [ ! -d "${PIGEN_DIR}/deploy" ]; then
    fail "pi-gen deploy dir not found after build: ${PIGEN_DIR}/deploy"
fi

# Locate the freshly built .img and its .img.xz.
# shellcheck disable=SC2012
IMG_XZ=$(ls -t "${PIGEN_DIR}"/deploy/*-lapis-zero*.img.xz 2>/dev/null | head -1 || true)
if [ -z "${IMG_XZ}" ]; then
    # Older pi-gen names the archive image_<date>-lapis-zero.img.xz; accept any .img.xz.
    # shellcheck disable=SC2012
    IMG_XZ=$(ls -t "${PIGEN_DIR}"/deploy/*.img.xz 2>/dev/null | head -1 || true)
fi
[ -n "${IMG_XZ}" ] || fail "no .img.xz produced in ${PIGEN_DIR}/deploy"

cp -f "${IMG_XZ}" "${DEPLOY_DIR}/lapis-zero-arm64.img.xz"

# Copy the raw .img too (if present) for dd-based flashing.
# shellcheck disable=SC2012
RAW_IMG=$(ls -t "${PIGEN_DIR}"/deploy/*-lapis-zero*.img 2>/dev/null | head -1 || true)
if [ -n "${RAW_IMG}" ] && [ ! -L "${RAW_IMG}" ]; then
    cp -f "${RAW_IMG}" "${DEPLOY_DIR}/lapis-zero-arm64.img"
fi

# Generate the M5 Imager manifest from the two artifacts.
LAPIS_IMG_XZ="${DEPLOY_DIR}/lapis-zero-arm64.img.xz" \
LAPIS_IMG_RAW="${DEPLOY_DIR}/lapis-zero-arm64.img" \
    python3 "${SCRIPT_DIR}/generate-imager-manifest.py" \
    -o "${DEPLOY_DIR}/os-list.json"

echo "Done."
echo "  image:     ${DEPLOY_DIR}/lapis-zero-arm64.img.xz"
echo "  raw image: ${DEPLOY_DIR}/lapis-zero-arm64.img"   "(if produced)"
echo "  manifest:  ${DEPLOY_DIR}/os-list.json"
