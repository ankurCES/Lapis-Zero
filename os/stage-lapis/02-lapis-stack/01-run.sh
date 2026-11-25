#!/bin/bash -e
# Install lapis-ui (prebuilt aarch64 binary) and lapis-daemon (Python venv)
# into /opt/lapis.  build-image.sh populates this sub-stage's files/ with:
#   lapis-ui                 aarch64 release binary
#   lapis_daemon-*.whl       pure-python wheel (uv build output)
#
# The wheel's own dependencies (aiohttp, asyncvnc, asyncssh,
# bluez-peripheral, ...) are resolved from PyPI inside the chroot, binary wheels only,
# so nothing is compiled under emulation.  pydantic / Pillow / cryptography come
# from Debian (python3-*) through --system-site-packages.

install -d -m 0755 "${ROOTFS_DIR:?}/opt/lapis/bin"
install -d -m 0755 "${ROOTFS_DIR}/opt/lapis/pkgs"

# pi-gen runs NN-run.sh with CWD set to the sub-stage (build.sh `pushd`).
install -m 0755 "files/lapis-ui" "${ROOTFS_DIR}/opt/lapis/bin/lapis-ui"
find "${ROOTFS_DIR}/opt/lapis/pkgs" -name 'lapis_daemon-*.whl' -delete
install -m 0644 files/lapis_daemon-*.whl "${ROOTFS_DIR}/opt/lapis/pkgs/"

on_chroot << 'CHROOT'
set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends \
    python3-venv python3-pip python3-pydantic python3-pil python3-cryptography \
    bluez network-manager rfkill alsa-utils
python3 -m venv --clear --system-site-packages /opt/lapis/venv
WHEEL=$(find /opt/lapis/pkgs -name 'lapis_daemon-*.whl' | head -1)
/opt/lapis/venv/bin/pip install --no-cache-dir --only-binary=:all: "${WHEEL}[device]"
# Fail the image build, not the first boot, if anything is missing.
/opt/lapis/venv/bin/python -m lapis_daemon.selfcheck
/opt/lapis/bin/lapis-ui --dump-frame /run/lapis-ui-selfcheck.png --view companion
apt-get clean
CHROOT
