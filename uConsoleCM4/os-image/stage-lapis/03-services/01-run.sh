#!/bin/bash
# systemd units, default configuration, first-boot seed import, the tailscale serve bootstrap,
# and build-time checks. Declarative: files and links only, no live systemctl.
set -euo pipefail

FILES="${STAGE_DIR:?}/files"
SYSTEMD="${ROOTFS_DIR:?}/etc/systemd/system"
install -d -m 0755 "${SYSTEMD}" "${ROOTFS_DIR}/usr/lib/lapis"
install -m 0644 "${FILES}"/systemd/*.service "${FILES}"/systemd/*.timer "${SYSTEMD}/"
for d in meshtasticd gpsd; do
    install -d -m 0755 "${SYSTEMD}/${d}.service.d"
    install -m 0644 "${FILES}/systemd/${d}-lapis.conf" "${SYSTEMD}/${d}.service.d/lapis.conf"
done
for s in lapis-firstboot.sh lapis-tailscale-up.sh lapis-rails.sh lapis-bootreport.sh lapis-meshtasticd.sh; do
    install -m 0755 "${FILES}/${s}" "${ROOTFS_DIR}/usr/lib/lapis/${s}"
done

# Configuration. /etc/lapis/deck.toml is the daemon's (root); lapis.toml, hosts.toml and
# secrets.env belong to the harness, which runs as `lapis`.
install -d -m 0755 "${ROOTFS_DIR}/etc/lapis"
install -m 0644 "${FILES}/etc/deck.toml" "${ROOTFS_DIR}/etc/lapis/deck.toml"
[ -f "${ROOTFS_DIR}/etc/lapis/lapis.toml" ] || install -m 0644 "${FILES}/etc/lapis.toml" "${ROOTFS_DIR}/etc/lapis/lapis.toml"
[ -f "${ROOTFS_DIR}/etc/lapis/hosts.toml" ] || install -m 0644 "${FILES}/etc/hosts.toml" "${ROOTFS_DIR}/etc/lapis/hosts.toml"
install -m 0644 "${FILES}/etc/gpsd" "${ROOTFS_DIR}/etc/default/gpsd"
install -d -m 0755 "${ROOTFS_DIR}/etc/chrony/conf.d"
install -m 0644 "${FILES}/etc/chrony-lapis.conf" "${ROOTFS_DIR}/etc/chrony/conf.d/lapis-gps.conf"
# Boot splash: Plymouth's script plugin with the Lapis theme (art: os-image/make-splash.py).
install -d -m 0755 "${ROOTFS_DIR}/usr/share/plymouth/themes/lapis" "${ROOTFS_DIR}/etc/plymouth"
install -m 0644 "${FILES}"/plymouth/lapis/* "${ROOTFS_DIR}/usr/share/plymouth/themes/lapis/"
printf '[Daemon]\nTheme=lapis\nShowDelay=0\n' > "${ROOTFS_DIR}/etc/plymouth/plymouthd.conf"
install -d -m 0755 "${ROOTFS_DIR}/boot/firmware/lapis-seed"
install -m 0644 "${FILES}/lapis-seed-README.txt" "${ROOTFS_DIR}/boot/firmware/lapis-seed/README.txt"

on_chroot << 'CHROOT'
set -euo pipefail
id lapis >/dev/null
# No password login: the build's throwaway password is locked away.
passwd -l lapis >/dev/null
for g in video render input gpio spi i2c dialout audio bluetooth netdev; do
    getent group "${g}" >/dev/null && usermod -aG "${g}" lapis
done
chown -R lapis:lapis /etc/lapis/lapis.toml /etc/lapis/hosts.toml
# The harness (user lapis) saves its files by write-then-rename, which needs a writable
# directory. Sticky + group lapis: it can replace its own files, never root's deck.toml.
chown root:lapis /etc/lapis
chmod 1775 /etc/lapis
install -m 0600 -o lapis -g lapis /dev/null /etc/lapis/secrets.env
install -d -m 0750 -o root -g lapis /var/lib/lapis
install -d -m 0750 -o lapis -g lapis /var/lib/lapis/harness
CHROOT

# Enabled: our services, tailscaled, gpsd, meshtasticd, chrony, bluetooth, NetworkManager.
# readsb is installed but not enabled: it needs the dongle the waterfall uses.
install -d -m 0755 "${SYSTEMD}/multi-user.target.wants"
for unit in lapis-firstboot.service lapis-rails.service lapis-daemon.service lapis-harness.service \
            lapis-ui.service lapis-tailscale-up.service; do
    ln -sf "/etc/systemd/system/${unit}" "${SYSTEMD}/multi-user.target.wants/${unit}"
done
# The boot report: no console on this device, so diagnostics go to the FAT partition.
install -d -m 0755 "${SYSTEMD}/timers.target.wants"
ln -sf /etc/systemd/system/lapis-bootreport.timer "${SYSTEMD}/timers.target.wants/lapis-bootreport.timer"

on_chroot << 'CHROOT'
set -euo pipefail
{
    echo "image: lapis-deck-cm4-trixie-arm64 built $(date -u +%FT%TZ)"
    sha256sum /usr/local/bin/lapis-daemon /usr/local/bin/lapis-ui | awk '{print "sha256", substr($1, 1, 16), $2}'
} > /etc/lapis/build-info
systemctl enable tailscaled.service gpsd.service meshtasticd.service chrony.service bluetooth.service NetworkManager.service
CHROOT

# First boot: only what this build enabled may run (Debian's preset policy is "enable all").
install -d -m 0755 "${ROOTFS_DIR}/etc/systemd/system-preset"
install -m 0644 "${FILES}/etc/00-lapis.preset" "${ROOTFS_DIR}/etc/systemd/system-preset/00-lapis.preset"

on_chroot << 'CHROOT'
set -euo pipefail
problems=$(systemd-analyze verify --man=no /etc/systemd/system/lapis-*.service meshtasticd.service 2>&1 \
    | grep -E '/etc/systemd/system/lapis-|meshtasticd.service.d/lapis.conf|lapis-meshtasticd' || true)
if [ -n "${problems}" ]; then
    echo "ERROR: systemd-analyze reported problems in lapis units:" >&2
    echo "${problems}" >&2
    exit 1
fi
test "$(readlink /etc/systemd/system/default.target)" = /usr/lib/systemd/system/multi-user.target
test "$(readlink /etc/systemd/system/getty@tty1.service)" = /dev/null
test "$(readlink /etc/systemd/system/serial-getty@ttyS0.service)" = /dev/null
test "$(cat /etc/hostname)" = lapis-deck
test "$(passwd -S lapis | awk '{print $2}')" = L
test "$(stat -c '%a %U:%G' /etc/lapis)" = "1775 root:lapis"
test "$(stat -c '%U' /etc/lapis/deck.toml)" = root
# meshtasticd runs as its own user: the AIO radio config is there, and it may open
# /dev/spidev1.0 and /dev/gpiochip0 (udev gives those to groups spi and gpio).
test -f /etc/meshtasticd/config.d/lora-hackergadgets-aio.yaml
id -nG meshtasticd | grep -qw spi
id -nG meshtasticd | grep -qw gpio
# The splash: Lapis theme selected, plymouth started at boot (the package's static wants link).
test "$(plymouth-set-default-theme)" = lapis
test -e /usr/lib/systemd/system/sysinit.target.wants/plymouth-start.service
grep -qw splash /boot/firmware/cmdline.txt
# No desktop: nothing X11/Wayland/display-manager may be installed (AGENTS.md invariant 1).
if dpkg-query -W -f '${Package}\n' 'xserver-xorg*' 'lightdm*' 'gdm3' 'sddm' 'xfce4*' 'lxde*' 'gnome-shell' 'labwc' 2>/dev/null | grep -q .; then
    echo "ERROR: a desktop/X11 package is installed" >&2
    exit 1
fi
CHROOT
