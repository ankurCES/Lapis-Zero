#!/bin/bash -e
# Lapis-Zero OS hardening: hand the device over from the stock CardputerZero
# launcher to Lapis, then kernel/cmdline, swap, storage and journald tuning.
# Runs with ROOTFS_DIR pointing at the stage rootfs (not chrooted), per pi-gen
# convention for config.txt / cmdline.txt / fstab / etc edits.

SYSTEMD="${ROOTFS_DIR:?}/etc/systemd/system"
install -d -m 0755 "${SYSTEMD}"

# -- Take the device away from the stock launcher -----------------------------
# stage2 (the BSP) ships M5Stack's APPLaunch + its first-boot LaunchWizard.
# Left enabled, the wizard owns the LCD and the keypad on every boot and the
# Lapis UI never appears.  Mask the units (authoritative even if the package
# purge below is skipped) and drop the launcher's one-shot markers.
#
# Masks, not removed wants-links: anything sysinit.target wants is re-pulled by
# later transactions, and where / is writable when PID 1 starts (containers; on
# the device it is still read-only then) systemd's first-boot preset pass
# re-creates deleted links.
#   fb_load.service             the BSP splash.  It takes DRM master on the LCD
#                               and only lets go when the *stock* launcher runs,
#                               so while it is up everything lapis-ui draws on
#                               the fbdev is invisible.  Conflicts= cannot hold
#                               it down (first hardware boot, 2026-11-24).
#   adbd.service                root shell on tcp/5555 if it were ever enabled.
#   cardputerzero-diskid.service  rewrites the MBR disk ID of the mounted card
#                               on the first boot: it pulls /boot/firmware out
#                               from under running units, and a power cut in its
#                               window leaves cmdline.txt naming a PARTUUID that
#                               does not exist (unbootable card).
#   fstrim.service              full-card TRIM burst during the first boot.
for unit in LaunchWizard.service applaunch-updater.service \
            applaunch-apps-updater.service applaunch-apt-update.service \
            cardputer-adb-hotplug.service userconfig.service \
            fb_load.service adbd.service cardputerzero-diskid.service \
            fstrim.service; do
    rm -f "${SYSTEMD:?}"/*.wants/"${unit}"
    ln -sfn /dev/null "${SYSTEMD}/${unit}"
done
rm -rf "${ROOTFS_DIR:?}/var/lib/LaunchWizard"

# Headless appliance: never wait for a display manager or a network.
ln -sfn /usr/lib/systemd/system/multi-user.target "${SYSTEMD}/default.target"
rm -f "${SYSTEMD:?}/network-online.target.wants/NetworkManager-wait-online.service"
ln -sfn /dev/null "${SYSTEMD}/NetworkManager-wait-online.service"
for unit in apt-daily.timer apt-daily-upgrade.timer man-db.timer e2scrub_all.timer; do
    rm -f "${SYSTEMD:?}/timers.target.wants/${unit}"
done
rm -f "${SYSTEMD:?}/multi-user.target.wants/rpi-eeprom-update.service" \
      "${SYSTEMD:?}/multi-user.target.wants/e2scrub_reap.service"

# cloud-init has nothing to apply here (the seed files are all comments, and
# M5 Imager customisation goes through firstrun.sh, not cloud-init), yet its
# Python start-up sits in front of sysinit.target on every boot.
install -d -m 0755 "${ROOTFS_DIR}/etc/cloud"
: > "${ROOTFS_DIR}/etc/cloud/cloud-init.disabled"

# -- Wi-Fi radio on --
# stage2 ships NetworkManager.state with WirelessEnabled=false (pi-gen does that
# when no Wi-Fi country is configured).  NetworkManager forces the kernel rfkill
# state to follow that flag at every start, which soft-blocks wlan0 and undoes
# any `rfkill unblock wifi`: BLE provisioning could never join a network.
install -d -m 0700 "${ROOTFS_DIR}/var/lib/NetworkManager"
printf '[main]\nWirelessEnabled=true\n' \
    > "${ROOTFS_DIR}/var/lib/NetworkManager/NetworkManager.state"

# The BSP makes every radio start soft-blocked (rfkill default_state=0) and
# ships saved "unblocked" states for other boards' device names.  These are the
# names the CM0 uses (read from a card after its first boot), so systemd-rfkill
# unblocks both radios the moment they register, on the first boot too.
install -d -m 0755 "${ROOTFS_DIR}/var/lib/systemd/rfkill"
for name in 'platform-soc-amba-3f201000.serial:bluetooth' 'platform-3f300000.mmcnr:wlan'; do
    echo 0 > "${ROOTFS_DIR}/var/lib/systemd/rfkill/${name}"
done


# Remove the launcher, its apps and payload that only it needs (CJK fonts,
# USB-dongle Wi-Fi firmware, the Pi 4/5 EEPROM updater).  The BSP drivers,
# overlays and udev rules are plain files from stage2, not part of these
# packages, and stay; the kernel and its headers are held by the BSP
# and are deliberately left alone.  Purge failures are non-fatal: the masks
# above are what guarantee the boot path.
on_chroot << 'CHROOT'
export DEBIAN_FRONTEND=noninteractive
purge() {
    pkgs=$(dpkg-query -W -f '${Package}\n' "$@" 2>/dev/null | sort -u | tr '\n' ' ')
    # shellcheck disable=SC2086  # word-splitting the package list is intended
    [ -z "${pkgs}" ] || apt-get purge -y ${pkgs} || echo "WARNING: purge failed for: ${pkgs}" >&2
}
purge applaunch 'm5cardputerzero-*' camera factorytest
purge fonts-noto-cjk fonts-noto-cjk-extra
purge firmware-atheros firmware-mediatek firmware-realtek rpi-eeprom
apt-get clean
CHROOT

# -- /boot/firmware/config.txt: gpu_mem=16, keep all CardputerZero overlays --
CFG="${ROOTFS_DIR}/boot/firmware/config.txt"
if [ -f "${CFG}" ]; then
    # Reserve only 16 MiB for the GPU (the firmware then loads the cut-down
    # start_cd.elf / fixup_cd.dat pair, both shipped in /boot/firmware); keep
    # every BSP overlay (st7789, tca8418, es8389, bmi270, bq27220, rx8130ce).
    # Recovery: if a board ever fails to boot with this, set gpu_mem=64 in
    # config.txt on the FAT partition to return to the BSP's start.elf.
    if grep -q '^gpu_mem=' "${CFG}"; then
        sed -i 's/^gpu_mem=.*/gpu_mem=16/' "${CFG}"
    else
        printf 'gpu_mem=16\n' >> "${CFG}"
    fi
fi
for f in start_cd.elf fixup_cd.dat; do
    [ -f "${ROOTFS_DIR}/boot/firmware/${f}" ] || {
        echo "ERROR: gpu_mem=16 needs /boot/firmware/${f}, which is missing" >&2
        exit 1
    }
done

# -- /boot/firmware/cmdline.txt: keep kernel text off the framebuffer --
CMDLINE="${ROOTFS_DIR}/boot/firmware/cmdline.txt"
if [ -f "${CMDLINE}" ]; then
    # The last two are the exact settings under which the UI first came up on
    # the hardware (fourth boot, 2026-11-24), each a second layer under a fix
    # elsewhere:
    #   systemd.mask=fb_load.service             the unit mask above, independent of /etc
    #   initcall_blacklist=simplefb_driver_init  no firmware framebuffer as /dev/fb0
    #                                            early in boot (lapis-ui also refuses
    #                                            any framebuffer that is not 320x170)
    for token in fbcon=map:off quiet loglevel=3 systemd.show_status=false \
                 systemd.mask=fb_load.service \
                 initcall_blacklist=simplefb_driver_init; do
        if ! grep -q "${token}" "${CMDLINE}"; then
            sed -i "1s/$/ ${token}/" "${CMDLINE}"
        fi
    done
fi

# -- swap: 512 MB LZ4 zram, never a file on the SD card --
# Trixie's rpi-swap owns swap setup (it drives systemd-zram-generator); its
# default "auto" mechanism adds an SD-backed swap file, so pin it to zram.
install -d -m 0755 "${ROOTFS_DIR}/etc/rpi/swap.conf.d"
install -m 0644 "${STAGE_DIR}/files/swap-lapis.conf" \
    "${ROOTFS_DIR}/etc/rpi/swap.conf.d/90-lapis.conf"
# rpi-swap sizes the device; this drop-in only adds the compressor to the same
# [zram0] section (zram-generator merges drop-ins across /etc and /run).
install -d -m 0755 "${ROOTFS_DIR}/etc/systemd/zram-generator.conf.d"
printf '[zram0]\ncompression-algorithm = lz4\n' \
    > "${ROOTFS_DIR}/etc/systemd/zram-generator.conf.d/10-lapis-lz4.conf"

# -- fstab: noatime on /, /tmp as tmpfs --
if [ -f "${ROOTFS_DIR}/etc/fstab" ]; then
    # Append noatime to the / ext4 line without duplicating the option.
    if ! grep -E '^[^#].*[[:space:]]/[[:space:]]' "${ROOTFS_DIR}/etc/fstab" | grep -q noatime; then
        sed -i -E '/^[^#].*[[:space:]]\/[[:space:]]/ s/([[:space:]]+)ext4([[:space:]]+)([^[:space:]]+)/\1ext4\2\3,noatime/' \
            "${ROOTFS_DIR}/etc/fstab"
    fi
fi
if ! grep -q '[[:space:]]/tmp[[:space:]]' "${ROOTFS_DIR}/etc/fstab"; then
    cat >> "${ROOTFS_DIR}/etc/fstab" <<'EOF'
tmpfs   /tmp   tmpfs   defaults,size=64M,noatime,nosuid,nodev   0 0
EOF
fi

# -- volatile journal (overrides the BSP's persist.conf: later name wins) --
install -d -m 0755 "${ROOTFS_DIR}/etc/systemd/journald.conf.d"
cat > "${ROOTFS_DIR}/etc/systemd/journald.conf.d/volatile.conf" <<'EOF'
[Journal]
Storage=volatile
RuntimeMaxUse=16M
EOF
