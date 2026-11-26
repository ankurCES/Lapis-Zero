#!/bin/bash
# The Lapis-Deck stack: lapis-daemon and lapis-ui (aarch64 release binaries) and the
# harness (a Python wheel in a venv). build.sh fills files/ from the current source, so an
# image never ships a stale binary.
set -euo pipefail

for f in files/lapis-daemon files/lapis-ui; do
    test -x "${f}" || { echo "ERROR: ${f} missing: run os-image/build.sh, not pi-gen directly" >&2; exit 1; }
done
install -m 0755 files/lapis-daemon files/lapis-ui "${ROOTFS_DIR:?}/usr/local/bin/"
install -d -m 0755 "${ROOTFS_DIR}/opt/lapis/pkgs"
find "${ROOTFS_DIR}/opt/lapis/pkgs" -name 'lapis_harness-*.whl' -delete
install -m 0644 files/lapis_harness-*.whl "${ROOTFS_DIR}/opt/lapis/pkgs/"

on_chroot << 'CHROOT'
set -euo pipefail
python3 -m venv --clear --system-site-packages /opt/lapis/harness
WHEEL=$(find /opt/lapis/pkgs -name 'lapis_harness-*.whl' | head -1)
/opt/lapis/harness/bin/pip install --no-cache-dir --only-binary=:all: "${WHEEL}[kvm]"
# Fail the image build, not the first boot, when something cannot load.
/opt/lapis/harness/bin/python -c 'import lapis_harness.main'
for bin in /usr/local/bin/lapis-daemon /usr/local/bin/lapis-ui; do
    if ldd "${bin}" | grep -q 'not found'; then
        ldd "${bin}" >&2
        echo "ERROR: ${bin} has missing libraries" >&2
        exit 1
    fi
done
CHROOT
