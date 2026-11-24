#!/bin/bash -e
# Install tailscale from its Debian repo into the staged rootfs.  No secrets
# here; headless join is performed at first boot by lapis-firstboot.sh (using
# /boot/firmware/lapis-seed/tailscale.authkey) or interactively via View 6.

install -d -m 0755 "${ROOTFS_DIR}/usr/share/keyrings"

# Tailscale GPG key + apt source (Debian Trixie = tuxedo-ish "bookworm" suite,
# which is what Tailscale's docs pin for current Debian stable/trixie).
if ! [ -f "${ROOTFS_DIR}/usr/share/keyrings/tailscale-archive-keyring.gpg" ]; then
    curl -fsSL https://pkgs.tailscale.com/stable/debian/bookworm.noarmor.gpg \
        -o "${ROOTFS_DIR}/usr/share/keyrings/tailscale-archive-keyring.gpg"
else
    echo "tailscale keyring already present"
fi

cat > "${ROOTFS_DIR}/etc/apt/sources.list.d/tailscale.list" <<EOF
deb [signed-by=/usr/share/keyrings/tailscale-archive-keyring.gpg] https://pkgs.tailscale.com/stable/debian bookworm main
EOF

on_chroot << 'CHROOT'
set -e
apt-get update
apt-get install -y --no-install-recommends tailscale
# tailscaled is socket-activated; do NOT enable anything here.
systemctl disable tailscaled-autoconnect.service 2>/dev/null || true
CHROOT
