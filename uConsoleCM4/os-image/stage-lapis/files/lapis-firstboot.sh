#!/usr/bin/env bash
# Every boot: unblock the radios. First boot only: import /boot/firmware/lapis-seed/ into
# /etc/lapis (secrets and keys 0600, owned by the harness account) and delete the plaintext
# copies from the FAT partition. Never touches the network.
set -euo pipefail

STATE="${LAPIS_STATE_DIR:-/var/lib/lapis}"
SEED="${LAPIS_SEED_DIR:-/boot/firmware/lapis-seed}"
ETC="${LAPIS_ETC_DIR:-/etc/lapis}"
DONE="${STATE}/.firstboot_done"
OWNER="${LAPIS_OWNER:-lapis}"

rfkill unblock wifi 2>/dev/null || true
rfkill unblock bluetooth 2>/dev/null || true

[ -f "${DONE}" ] && exit 0
install -d -m 0750 "${STATE}"

import() {
    local name="$1" mode="$2" dest="$3"
    if [ -s "${SEED}/${name}" ]; then
        install -m "${mode}" -o "${OWNER}" -g "${OWNER}" "${SEED}/${name}" "${dest}"
        echo "lapis-firstboot: imported ${name}"
        return 0
    fi
    return 1
}

import lapis.toml 0644 "${ETC}/lapis.toml" || true
import hosts.toml 0644 "${ETC}/hosts.toml" || true
if [ -s "${SEED}/deck.toml" ]; then
    install -m 0644 -o root -g root "${SEED}/deck.toml" "${ETC}/deck.toml"
    echo "lapis-firstboot: imported deck.toml"
fi
if import secrets.env 0600 "${ETC}/secrets.env"; then
    shred -u "${SEED}/secrets.env" 2>/dev/null || rm -f "${SEED}/secrets.env"
fi
if [ -s "${SEED}/tailscale.authkey" ]; then
    install -m 0600 -o root -g root "${SEED}/tailscale.authkey" "${ETC}/tailscale.authkey"
    shred -u "${SEED}/tailscale.authkey" 2>/dev/null || rm -f "${SEED}/tailscale.authkey"
    echo "lapis-firstboot: imported tailscale.authkey"
fi
if import lapis_ed25519 0600 "${STATE}/harness/lapis_ed25519"; then
    shred -u "${SEED}/lapis_ed25519" 2>/dev/null || rm -f "${SEED}/lapis_ed25519"
fi
sync
touch "${DONE}"
