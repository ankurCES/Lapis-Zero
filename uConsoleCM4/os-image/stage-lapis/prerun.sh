#!/bin/bash
# stage-lapis starts from stage2's rootfs (`copy_previous` comes from pi-gen's scripts/common).
set -euo pipefail
if [ ! -d "${ROOTFS_DIR}" ]; then
    copy_previous
fi
