#!/bin/bash -e
# stage-lapis is the sole stage after stage2; it reuses stage2's rootfs as its
# starting point.  `copy_previous` is exported by pi-gen's scripts/common.
if [ ! -d "${ROOTFS_DIR}" ]; then
    copy_previous
fi
