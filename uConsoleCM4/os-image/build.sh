#!/usr/bin/env bash
# Builds the flashable Lapis-Deck image for the ClockworkPi uConsole CM4:
#   os-image/deploy/lapis-deck-cm4-trixie-arm64.img.xz (+ .sha256, os-list.json)
#
# 1. aarch64 release binaries (lapis-daemon with BlueZ, lapis-ui on DRM/KMS), built in an arm64
#    Debian trixie container (scripts/linux-check.sh release);
# 2. the harness wheel (uv build);
# 3. pi-gen: ak-rex/ClockworkPi-pi-gen (trixie, akrex-kernel, uConsole overlays) stages 0-2 +
#    stage-lapis, in Docker;
# 4. SHA-256 and a Raspberry Pi Imager manifest.
#
# Environment: PIGEN_REPO, PIGEN_REF (a commit, for a reproducible base), PIGEN_DIR (no
# spaces), DEPLOY_DIR, LAPIS_RELEASE_URL (manifest download URL), SKIP_BINARIES=1 (reuse).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${HERE}/.." && pwd)"
PIGEN_REPO="${PIGEN_REPO:-https://github.com/ak-rex/ClockworkPi-pi-gen.git}"
PIGEN_REF="${PIGEN_REF:-4c90542730df02de39ae97ca1139ec753624cd37}"
PIGEN_DIR="${PIGEN_DIR:-${HOME}/.cache/lapis-deck/pi-gen}"
DEPLOY_DIR="${DEPLOY_DIR:-${HERE}/deploy}"
NAME="lapis-deck-cm4-trixie-arm64"
STAGE_FILES="${HERE}/stage-lapis/02-lapis-stack/files"

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

case "${PIGEN_DIR}" in
    *" "*) fail "PIGEN_DIR must not contain spaces (debootstrap): ${PIGEN_DIR}" ;;
esac
docker info >/dev/null 2>&1 || fail "Docker is not running (pi-gen and the arm64 build need it)"
command -v uv >/dev/null || fail "uv not found (it builds the harness wheel)"

# 1. Device binaries, always from the current source.
mkdir -p "${STAGE_FILES}"
if [ "${SKIP_BINARIES:-0}" != "1" ]; then
    "${ROOT}/scripts/linux-check.sh" release
fi
for bin in lapis-daemon lapis-ui; do
    src="${ROOT}/target/linux-arm64/release/${bin}"
    [ -x "${src}" ] || fail "${src} missing (run scripts/linux-check.sh release)"
    file "${src}" | grep -q 'ARM aarch64' || fail "${src} is not an aarch64 binary"
    install -m 0755 "${src}" "${STAGE_FILES}/${bin}"
done

# 2. Harness wheel.
find "${STAGE_FILES}" -name 'lapis_harness-*.whl' -delete
(cd "${ROOT}/harness" && uv build --wheel --out-dir "${STAGE_FILES}")
ls "${STAGE_FILES}"/lapis_harness-*.whl >/dev/null

# 3. pi-gen at a fixed commit, our stage copied in.
if [ ! -d "${PIGEN_DIR}/.git" ]; then
    mkdir -p "$(dirname "${PIGEN_DIR}")"
    git clone "${PIGEN_REPO}" "${PIGEN_DIR}"
fi
git -C "${PIGEN_DIR}" fetch --quiet origin
git -C "${PIGEN_DIR}" checkout --quiet --force "${PIGEN_REF}"
git -C "${PIGEN_DIR}" clean -fdq -e work -e deploy
rm -rf "${PIGEN_DIR}/stage-lapis"
cp -a "${HERE}/stage-lapis" "${PIGEN_DIR}/stage-lapis"
find "${PIGEN_DIR}/stage-lapis" -name '*.sh' -exec chmod +x {} +
# No separate "-lite" export from stage2: stage-lapis exports the one image.
touch "${PIGEN_DIR}/stage2/SKIP_IMAGES"
install -m 0600 "${HERE}/config.pigen" "${PIGEN_DIR}/config"
# pi-gen insists on a first-user password when the first-boot rename is disabled. A random
# throwaway per build (never stored in the repo, never printed); stage-lapis then locks the
# account, so no password login exists on the image.
printf 'FIRST_USER_PASS=%s\n' "$(openssl rand -hex 24)" >> "${PIGEN_DIR}/config"
find "${PIGEN_DIR}/deploy" -maxdepth 1 -name '*.img*' -delete 2>/dev/null || true
echo "Building ${NAME} with pi-gen ${PIGEN_REF} ..."
(cd "${PIGEN_DIR}" && CONTINUE=1 PRESERVE_CONTAINER=0 ./build-docker.sh)

# 4. Artifacts.
img=$(find "${PIGEN_DIR}/deploy" -maxdepth 1 -name "*${NAME}*.img.xz" | head -1)
[ -n "${img}" ] || fail "pi-gen produced no ${NAME} image in ${PIGEN_DIR}/deploy"
mkdir -p "${DEPLOY_DIR}"
cp -f "${img}" "${DEPLOY_DIR}/${NAME}.img.xz"
(cd "${DEPLOY_DIR}" && shasum -a 256 "${NAME}.img.xz" > "${NAME}.img.xz.sha256")
python3 "${HERE}/imager-manifest.py" \
    --image "${DEPLOY_DIR}/${NAME}.img.xz" \
    --url "${LAPIS_RELEASE_URL:-file://${DEPLOY_DIR}/${NAME}.img.xz}" \
    -o "${DEPLOY_DIR}/os-list.json"
echo "Done:"
echo "  ${DEPLOY_DIR}/${NAME}.img.xz"
echo "  ${DEPLOY_DIR}/${NAME}.img.xz.sha256"
echo "  ${DEPLOY_DIR}/os-list.json"
