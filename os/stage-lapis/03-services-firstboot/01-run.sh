#!/bin/bash -e
# systemd units, first-boot secret importer, powerguard, and service enablement.
# All declarative: filesystem edits only, no live `systemctl` on the host.

# -- systemd units --
install -d -m 0755 "${ROOTFS_DIR}/etc/systemd/system"
install -m 0644 "${STAGE_DIR}"/files/systemd/*.service \
    "${ROOTFS_DIR}/etc/systemd/system/"
install -m 0644 "${STAGE_DIR}"/files/systemd/*.timer \
    "${ROOTFS_DIR}/etc/systemd/system/" 2>/dev/null || true

# -- first-boot helper & powerguard script --
install -d -m 0755 "${ROOTFS_DIR}/usr/lib/lapis"
install -m 0755 "${STAGE_DIR}"/files/lapis-firstboot.sh \
    "${ROOTFS_DIR}/usr/lib/lapis/lapis-firstboot.sh"
install -m 0755 "${STAGE_DIR}"/files/lapis-powerguard.sh \
    "${ROOTFS_DIR}/usr/lib/lapis/lapis-powerguard.sh"

# -- initramfs hook: bundle cardputerzero,st7789v_lcd.bin --
install -d -m 0755 "${ROOTFS_DIR}/etc/initramfs-tools/hooks"
install -m 0755 "${STAGE_DIR}"/files/st7789-firmware \
    "${ROOTFS_DIR}/etc/initramfs-tools/hooks/st7789-firmware"

# -- enable our units (multi-user headless target) --
install -d -m 0755 "${ROOTFS_DIR}/etc/systemd/system/multi-user.target.wants"
ln -sf /etc/systemd/system/lapis-firstboot.service \
    "${ROOTFS_DIR}/etc/systemd/system/multi-user.target.wants/lapis-firstboot.service"
ln -sf /etc/systemd/system/lapis-ui.service \
    "${ROOTFS_DIR}/etc/systemd/system/multi-user.target.wants/lapis-ui.service"
ln -sf /etc/systemd/system/lapis-daemon.service \
    "${ROOTFS_DIR}/etc/systemd/system/multi-user.target.wants/lapis-daemon.service"
ln -sf /etc/systemd/system/lapis-powerguard.timer \
    "${ROOTFS_DIR}/etc/systemd/system/multi-user.target.wants/lapis-powerguard.timer"
