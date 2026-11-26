#!/bin/bash
# Boot straight into the HUD: firmware config, kernel command line, a headless multi-user
# target with nothing extra enabled, the serial console off /dev/ttyS0 (the GPS owns it).
# Runs on the host with ROOTFS_DIR set (pi-gen convention), not chrooted.
set -euo pipefail

SYSTEMD="${ROOTFS_DIR:?}/etc/systemd/system"
install -d -m 0755 "${SYSTEMD}"

# -- /boot/firmware/config.txt (SPEC 2): keep ak-rex's uConsole sections, append ours --
CFG="${ROOTFS_DIR}/boot/firmware/config.txt"
test -f "${CFG}" || { echo "ERROR: ${CFG} missing (stage1 of the base pi-gen did not run?)" >&2; exit 1; }
if ! grep -q '^# --- Lapis-Deck' "${CFG}"; then
    cat >> "${CFG}" <<'CONFIG'

# --- Lapis-Deck (SPEC 2) ---
[all]
# UART for the AIO board's GPS on /dev/ttyS0 (the serial console is removed from cmdline.txt).
enable_uart=1
# RTC (PCF85063A) on I2C, PPS from the GPS on GPIO6, SX1262 LoRa on SPI1 (CS GPIO18).
dtparam=i2c_arm=on
dtoverlay=i2c-rtc,pcf85063a
dtoverlay=pps-gpio,gpiopin=6
dtoverlay=spi1-1cs
CONFIG
fi
# The KMS driver (vc4-kms-v3d; on CM4 the [pi4] section loads its -pi4 form). Fail if absent.
grep -Eq '^dtoverlay=vc4-kms-v3d' "${CFG}" || { echo "ERROR: no vc4-kms-v3d overlay in config.txt" >&2; exit 1; }
grep -q '^dtoverlay=clockworkpi-uconsole-cm4' "${CFG}" || { echo "ERROR: no uConsole CM4 overlay in config.txt" >&2; exit 1; }

# -- /boot/firmware/cmdline.txt: ttyS0 free for the GPS, a quiet, fast boot --
CMDLINE="${ROOTFS_DIR}/boot/firmware/cmdline.txt"
sed -i -E 's/console=serial0,[0-9]+ ?//; s/console=ttyAMA0,[0-9]+ ?//' "${CMDLINE}"
for token in quiet splash loglevel=3 logo.nologo vt.global_cursor_default=0 consoleblank=0 systemd.show_status=false; do
    grep -q "${token}" "${CMDLINE}" || sed -i "1s/\$/ ${token}/" "${CMDLINE}"
done
if grep -q 'console=serial0' "${CMDLINE}"; then
    echo "ERROR: console=serial0 is still in cmdline.txt" >&2
    exit 1
fi

# -- headless appliance --
ln -sfn /usr/lib/systemd/system/multi-user.target "${SYSTEMD}/default.target"
# tty1 belongs to lapis-ui (DRM/KMS); ttyS0 belongs to the GPS.
for unit in getty@tty1.service serial-getty@ttyS0.service serial-getty@serial0.service \
            NetworkManager-wait-online.service systemd-networkd-wait-online.service \
            apt-daily.timer apt-daily-upgrade.timer man-db.timer fstrim.timer \
            userconfig.service lightdm.service; do
    rm -f "${SYSTEMD:?}"/*.wants/"${unit}"
    ln -sfn /dev/null "${SYSTEMD}/${unit}"
done
install -d -m 0755 "${ROOTFS_DIR}/etc/cloud"
: > "${ROOTFS_DIR}/etc/cloud/cloud-init.disabled"

# Radios on: NetworkManager otherwise soft-blocks Wi-Fi when no country was set (seen on
# Lapis-Zero hardware), and BLE provisioning could never join a network.
install -d -m 0700 "${ROOTFS_DIR}/var/lib/NetworkManager"
printf '[main]\nWirelessEnabled=true\n' > "${ROOTFS_DIR}/var/lib/NetworkManager/NetworkManager.state"

# Volatile journal: spare the SD card.
install -d -m 0755 "${ROOTFS_DIR}/etc/systemd/journald.conf.d"
printf '[Journal]\nStorage=volatile\nRuntimeMaxUse=32M\n' > "${ROOTFS_DIR}/etc/systemd/journald.conf.d/lapis.conf"
if ! grep -q '[[:space:]]/tmp[[:space:]]' "${ROOTFS_DIR}/etc/fstab"; then
    echo 'tmpfs   /tmp   tmpfs   defaults,size=256M,noatime,nosuid,nodev   0 0' >> "${ROOTFS_DIR}/etc/fstab"
fi
