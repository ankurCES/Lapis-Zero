#!/bin/bash -e
# Install tailscale from its Debian repo into the staged rootfs.  No secrets
# here; headless join is performed at first boot by lapis-firstboot.sh (using
# /boot/firmware/lapis-seed/tailscale.authkey) or interactively via View 6.

install -d -m 0755 "${ROOTFS_DIR}/usr/share/keyrings"
install -d -m 0755 "${ROOTFS_DIR}/etc/apt/sources.list.d"

cat > "${ROOTFS_DIR}/etc/apt/sources.list.d/tailscale.list" <<EOF
deb [signed-by=/usr/share/keyrings/tailscale-archive-keyring.gpg] https://pkgs.tailscale.com/stable/debian bookworm main
EOF

# Fetch the keyring and install tailscale inside the chroot so it uses the
# container's network + pi-gen apt proxy (host-side curl would bypass both).
on_chroot << 'CHROOT'
set -e
if ! [ -f /usr/share/keyrings/tailscale-archive-keyring.gpg ]; then
    curl -fsSL https://pkgs.tailscale.com/stable/debian/bookworm.noarmor.gpg \
        -o /usr/share/keyrings/tailscale-archive-keyring.gpg
fi
apt-get update
apt-get install -y --no-install-recommends tailscale
# tailscaled is socket-activated; do NOT enable anything here.
systemctl disable tailscaled-autoconnect.service 2>/dev/null || true
CHROOT
