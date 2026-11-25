#!/usr/bin/env bash
# lapis-firstboot.sh — runs early on every boot.
#
# Every boot: unblock the radios (the BSP boots with rfkill soft-blocking Wi-Fi
# and Bluetooth until its own launcher enables them).
# First boot only: import /boot/firmware/lapis-seed/ into /etc/lapis/ (0600 for
# secrets and keys) and remove the plaintext originals from the FAT partition.
#
# This script never talks to the network: the Tailscale join is done by
# lapis-tailscale-up.service, which can wait for Wi-Fi without delaying the UI.
set -euo pipefail

DONE="${LAPIS_STATE_DIR:-/var/lib/lapis}/.firstboot_done"
SEED="${LAPIS_SEED_DIR:-/boot/firmware/lapis-seed}"
DEST="${LAPIS_ETC_DIR:-/etc/lapis}"

log() { echo "[lapis-firstboot] $*"; }

if command -v rfkill >/dev/null 2>&1; then
    rfkill unblock wifi || true
    rfkill unblock bluetooth || true
fi

# NetworkManager forces the kernel rfkill state to follow its own saved
# WirelessEnabled flag every time it starts, so `rfkill unblock wifi` alone is
# undone a moment later.  Keep that flag, and systemd-rfkill's saved per-device
# state, on "enabled"; this unit is ordered before NetworkManager.service.
# (Writes only when something is wrong: no SD wear on a normal boot.)
NM_STATE="${LAPIS_NM_STATE:-/var/lib/NetworkManager/NetworkManager.state}"
if grep -q '^WirelessEnabled=false' "${NM_STATE}" 2>/dev/null; then
    sed -i 's/^WirelessEnabled=.*/WirelessEnabled=true/' "${NM_STATE}" || true
fi
for saved in /var/lib/systemd/rfkill/*:wlan; do
    if [ -e "${saved}" ] && [ "$(cat "${saved}" 2>/dev/null)" != 0 ]; then
        echo 0 > "${saved}" || true
    fi
done

if [ -f "${DONE}" ]; then
    log "already initialized (${DONE}); nothing to do"
    exit 0
fi

mkdir -p "${DEST}" "$(dirname "${DONE}")"

# --- config files (no secrets, 0644) ---
for name in lapis.toml hosts.toml; do
    if [ -f "${SEED}/${name}" ]; then
        install -m 0644 "${SEED}/${name}" "${DEST}/${name}"
        log "imported ${name}"
    fi
done

# --- secrets: strict 0600, then removed from the FAT partition ---
# `shred` cannot guarantee anything on FAT (clusters may be remapped), so it is
# overwrite-then-unlink on a best-effort basis.
import_secret() {
    local name="$1"
    [ -f "${SEED}/${name}" ] || return 0
    install -m 0600 "${SEED}/${name}" "${DEST}/${name}"
    shred -u -n 1 "${SEED:?}/${name:?}" 2>/dev/null || rm -f "${SEED:?}/${name:?}"
    log "imported ${name} (0600) and removed it from the boot partition"
}
import_secret secrets.env
import_secret lapis_ed25519
import_secret tailscale.authkey   # consumed later by lapis-tailscale-up.service
[ -f "${DEST}/secrets.env" ] || install -m 0600 /dev/null "${DEST}/secrets.env"
sync

touch "${DONE}"
log "first-boot seed import complete"
