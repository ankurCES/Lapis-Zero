#!/bin/bash -e
# systemd units, default config, first-boot importer, powerguard, boot report.
# All declarative: filesystem edits only, no live `systemctl` on the host.

SYSTEMD="${ROOTFS_DIR:?}/etc/systemd/system"

# -- systemd units --
install -d -m 0755 "${SYSTEMD}"
install -m 0644 "${STAGE_DIR}"/files/systemd/*.service "${SYSTEMD}/"
install -m 0644 "${STAGE_DIR}"/files/systemd/*.timer "${SYSTEMD}/"

# -- helper scripts --
install -d -m 0755 "${ROOTFS_DIR}/usr/lib/lapis"
for script in lapis-firstboot.sh lapis-powerguard.sh lapis-tailscale-up.sh \
              lapis-bootreport.sh; do
    install -m 0755 "${STAGE_DIR}/files/${script}" "${ROOTFS_DIR}/usr/lib/lapis/${script}"
done

# -- default configuration: the daemon must start on a card nobody has seeded --
install -d -m 0755 "${ROOTFS_DIR}/etc/lapis" "${ROOTFS_DIR}/var/lib/lapis"
[ -f "${ROOTFS_DIR}/etc/lapis/lapis.toml" ] || \
    install -m 0644 "${STAGE_DIR}/files/etc/lapis.toml" "${ROOTFS_DIR}/etc/lapis/lapis.toml"
[ -f "${ROOTFS_DIR}/etc/lapis/hosts.toml" ] || \
    install -m 0644 "${STAGE_DIR}/files/etc/hosts.toml" "${ROOTFS_DIR}/etc/lapis/hosts.toml"
[ -f "${ROOTFS_DIR}/etc/lapis/secrets.env" ] || \
    install -m 0600 /dev/null "${ROOTFS_DIR}/etc/lapis/secrets.env"

# Seed-directory instructions on the FAT partition (readable from any laptop).
install -d -m 0755 "${ROOTFS_DIR}/boot/firmware/lapis-seed"
install -m 0644 "${STAGE_DIR}/files/lapis-seed-README.txt" \
    "${ROOTFS_DIR}/boot/firmware/lapis-seed/README.txt"

# -- initramfs hook: bundle cardputerzero,st7789v_lcd.bin --
install -d -m 0755 "${ROOTFS_DIR}/etc/initramfs-tools/hooks"
install -m 0755 "${STAGE_DIR}"/files/st7789-firmware \
    "${ROOTFS_DIR}/etc/initramfs-tools/hooks/st7789-firmware"

# -- enable our units (multi-user headless target) --
# lapis-ble-prov.service is deliberately NOT enabled: the daemon starts it on
# demand (unpaired first boot / explicit pairing window) and stops it after.
install -d -m 0755 "${SYSTEMD}/multi-user.target.wants" "${SYSTEMD}/timers.target.wants"
for unit in lapis-firstboot.service lapis-ui.service lapis-daemon.service \
            lapis-tailscale-up.service lapis-bootreport-early.service; do
    ln -sf "/etc/systemd/system/${unit}" "${SYSTEMD}/multi-user.target.wants/${unit}"
done
for unit in lapis-powerguard.timer lapis-bootreport.timer; do
    ln -sf "/etc/systemd/system/${unit}" "${SYSTEMD}/timers.target.wants/${unit}"
done
find "${SYSTEMD}/multi-user.target.wants" -name 'lapis-powerguard.timer' -delete

# -- first-boot presets: only what this image enabled may run --
# Installed in the last sub-stage so package installs above are unaffected.
install -d -m 0755 "${ROOTFS_DIR}/etc/systemd/system-preset"
install -m 0644 "${STAGE_DIR}/files/etc/00-lapis.preset" \
    "${ROOTFS_DIR}/etc/systemd/system-preset/00-lapis.preset"

# Build-time sanity: every unit we ship must parse and name real executables,
# and the stock wizard, splash and first-boot extras must be off.
on_chroot << 'CHROOT'
set -e
problems=$(systemd-analyze verify --man=no /etc/systemd/system/lapis-*.service 2>&1 \
    | grep -E '/etc/systemd/system/lapis-' || true)
if [ -n "${problems}" ]; then
    echo "ERROR: systemd-analyze reported problems in lapis units:" >&2
    echo "${problems}" >&2
    exit 1
fi
test "$(readlink /etc/systemd/system/LaunchWizard.service)" = /dev/null
test "$(readlink /etc/systemd/system/default.target)" = /usr/lib/systemd/system/multi-user.target
# The splash must be unable to start, and nothing of ours may lean on it.
test "$(readlink /etc/systemd/system/fb_load.service)" = /dev/null
test ! -e /etc/systemd/system/sysinit.target.wants/fb_load.service
# (`! cmd` is exempt from `set -e`, hence the explicit if.)
if grep -Eq '^[A-Za-z]+=.*fb_load' /etc/systemd/system/lapis-ui.service; then
    echo "ERROR: lapis-ui.service must not depend on or conflict with fb_load.service" >&2
    exit 1
fi
test "$(readlink /etc/systemd/system/adbd.service)" = /dev/null
test "$(readlink /etc/systemd/system/cardputerzero-diskid.service)" = /dev/null
test ! -e /usr/lib/cardputerzero/randomize-disk-id
test -f /etc/systemd/system-preset/00-lapis.preset
test -f /etc/cloud/cloud-init.disabled
grep -qx 'WirelessEnabled=true' /var/lib/NetworkManager/NetworkManager.state
test "$(cat /etc/hostname)" = lapis-zero
CHROOT
