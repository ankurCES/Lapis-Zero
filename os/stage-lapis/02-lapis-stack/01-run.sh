#!/bin/bash -e
# Install lapis-ui (prebuilt aarch64 binary) and lapis-daemon (Python 3.12
# venv) into /opt/lapis.  Assumes build-image.sh populated ${STAGE_DIR}/files
# with:
#   lapis-ui                 aarch64 release binary
#   lapis_daemon-*.whl       pure-python wheel (uv build output)
#
# Dependencies (pydantic, pillow) are installed from Debian packages into the
# system site-packages, and the venv is created with --system-site-packages so
# the wheel can be installed with --no-deps / --no-index (offline, no build
# backend needed for a wheel).

install -d -m 0755 "${ROOTFS_DIR}/opt/lapis/bin"
install -d -m 0755 "${ROOTFS_DIR}/opt/lapis/pkgs"

install -m 0755 "${STAGE_DIR}/files/lapis-ui" "${ROOTFS_DIR}/opt/lapis/bin/lapis-ui"
install -m 0644 "${STAGE_DIR}/files"/lapis_daemon-*.whl "${ROOTFS_DIR}/opt/lapis/pkgs/"

on_chroot << 'CHROOT'
set -e
apt-get update
apt-get install -y --no-install-recommends \
    python3-venv python3-pip python3-pydantic python3-pil
python3 -m venv --system-site-packages /opt/lapis/venv
/opt/lapis/venv/bin/pip install --no-deps --no-index /opt/lapis/pkgs/lapis_daemon-*.whl
CHROOT
