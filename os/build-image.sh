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

# 1. aarch64 lapis-ui binary.  Always rebuilt from the current source: a stale
#    binary silently shipping in a "fresh" image is how a fixed bug stays broken.
mkdir -p "${STAGE_LAPIS}/02-lapis-stack/files"
if [ -n "${LAPIS_UI_BIN:-}" ]; then
    install -m 0755 "${LAPIS_UI_BIN}" "${STAGE_LAPIS}/02-lapis-stack/files/lapis-ui"
    echo "Using prebuilt lapis-ui: ${LAPIS_UI_BIN}"
else
    command -v cargo-zigbuild >/dev/null || fail "cargo-zigbuild not found (or set LAPIS_UI_BIN). Install: cargo install cargo-zigbuild --locked"
    ( cd "${SCRIPT_DIR}/../ui" && cargo-zigbuild build --release --no-default-features --target aarch64-unknown-linux-gnu )
    install -m 0755 "${SCRIPT_DIR}/../ui/target/aarch64-unknown-linux-gnu/release/lapis-ui" \
        "${STAGE_LAPIS}/02-lapis-stack/files/lapis-ui"
fi

# 2. lapis-daemon wheel, likewise always rebuilt (its dependencies are resolved
#    from PyPI inside the image chroot; see 02-lapis-stack/01-run.sh).
find "${STAGE_LAPIS}/02-lapis-stack/files" -name 'lapis_daemon-*.whl' -delete
if [ -n "${LAPIS_DAEMON_WHEEL:-}" ]; then
    install -m 0644 "${LAPIS_DAEMON_WHEEL}" "${STAGE_LAPIS}/02-lapis-stack/files/$(basename "${LAPIS_DAEMON_WHEEL}")"
    echo "Using prebuilt daemon wheel: ${LAPIS_DAEMON_WHEEL}"
else
    command -v uv >/dev/null || fail "uv not found (or set LAPIS_DAEMON_WHEEL)"
    WHEEL_OUT="$(mktemp -d)"
    ( cd "${SCRIPT_DIR}/../daemon" && uv build --wheel --out-dir "${WHEEL_OUT}" )
    install -m 0644 "${WHEEL_OUT}"/lapis_daemon-*.whl "${STAGE_LAPIS}/02-lapis-stack/files/"
fi

# 3. Install our custom stage into the pi-gen clone (STAGE_LIST references
#    "stage-lapis", which must be a sibling of stage0..stage2 inside the clone).
rm -rf "${PIGEN_DIR}/stage-lapis"
cp -a "${STAGE_LAPIS}" "${PIGEN_DIR}/stage-lapis"
# pi-gen only runs prerun.sh / NN-run.sh / files scripts that are executable;
# enforce +x in the clone so a checkout that lost its exec bits does not cause
# stage-lapis to be silently skipped (which yields an empty export rootfs).
find "${PIGEN_DIR}/stage-lapis" -type f \( -name '*.sh' -o -name 'prerun.sh' -o -name '00-run.sh' \) -exec chmod +x {} +
find "${PIGEN_DIR}/stage-lapis" -type f -path '*/files/*' -exec chmod +x {} +
echo "Installed stage-lapis into ${PIGEN_DIR}/stage-lapis"
# stage2 would otherwise also export a "-lite" image (5 extra minutes, 1 GB)
# that nobody flashes; pi-gen skips a stage's image export when this exists.
touch "${PIGEN_DIR}/stage2/SKIP_IMAGES"

# 3b. The upstream Dockerfile omits python3, but stage2/05-cardputerzero's
#     02-run.sh / 03-run.sh invoke HOST-side `python3` to parse the APPLaunch
#     GitHub release JSON.  Add python3 to the build container so those stages
#     succeed.  (Idempotent sed; safe to re-run.)
if ! grep -q 'python3' "${PIGEN_DIR}/Dockerfile"; then
    sed -i.bak 's/ qemu-user-static debootstrap/ qemu-user-static debootstrap python3/' "${PIGEN_DIR}/Dockerfile"
    rm -f "${PIGEN_DIR}/Dockerfile.bak"
    echo "Added python3 to pi-gen build Dockerfile"
fi

# --- make sure docker is available ---
if ! "${DOCKER}" ps >/dev/null 2>&1; then
    fail "cannot reach the Docker daemon (is Docker Desktop running?). The pi-gen build requires a running daemon."
fi

# --- run the pi-gen build ---
# NOTE: build-docker.sh rewrites -c paths with `sed -E 's/-c\s?…/-c \/config/'`,
# and macOS's BSD sed does not understand \s, so the host path leaks into the
# container and the build fails.  Copy our config into the clone as the
# canonical `config` file and invoke build-docker.sh with no -c argument, which
# works identically on macOS and Linux.
install -m 0644 "${SCRIPT_DIR}/config.pigen" "${PIGEN_DIR}/config"
mkdir -p "${DEPLOY_DIR}"
# Never let an archive from an earlier build be mistaken for this one.
if [ -d "${PIGEN_DIR}/deploy" ]; then
    find "${PIGEN_DIR}/deploy" -maxdepth 1 -name '*.img.xz' -delete
fi
echo "Building Lapis-Zero OS with pi-gen (stages: stage0 stage1 stage2 stage-lapis) ..."
(
    cd "${PIGEN_DIR}"
    set +e
    ./build-docker.sh
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

# Locate the freshly built image (stale archives were removed before the build).
IMG_XZ=$(find "${PIGEN_DIR}/deploy" -maxdepth 1 -name '*lapis-zero*.img.xz' ! -name '*-lite*' | head -1)
[ -n "${IMG_XZ}" ] || fail "no .img.xz produced in ${PIGEN_DIR}/deploy"

cp -f "${IMG_XZ}" "${DEPLOY_DIR}/lapis-zero-arm64.img.xz"

# pi-gen only emits the .img.xz (the raw .img is deleted after compression).
# The M5 Imager manifest needs the uncompressed size + SHA256 (extract_size /
# extract_sha256), so decompress a local copy for hashing and dd-based flashing.
echo "Decompressing raw image for manifest extract_* fields (dd + hashing)..."
xz -dkf -T0 "${DEPLOY_DIR}/lapis-zero-arm64.img.xz"

# Generate the M5 Imager manifest from the two artifacts. Without LAPIS_RELEASE_URL the
# download URL points at the local file, so the manifest works for a local flash as built.
LAPIS_IMG_XZ="${DEPLOY_DIR}/lapis-zero-arm64.img.xz" \
LAPIS_IMG_RAW="${DEPLOY_DIR}/lapis-zero-arm64.img" \
    python3 "${SCRIPT_DIR}/generate-imager-manifest.py" \
    --release-url "${LAPIS_RELEASE_URL:-file://${DEPLOY_DIR}/lapis-zero-arm64.img.xz}" \
    -o "${DEPLOY_DIR}/os-list.json"

echo "Done."
echo "  image:     ${DEPLOY_DIR}/lapis-zero-arm64.img.xz"
echo "  raw image: ${DEPLOY_DIR}/lapis-zero-arm64.img"   "(if produced)"
echo "  manifest:  ${DEPLOY_DIR}/os-list.json"
