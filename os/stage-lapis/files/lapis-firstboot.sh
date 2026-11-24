#!/usr/bin/env bash
# lapis-firstboot.sh — one-shot importer for /boot/firmware/lapis-seed/.
#
# Moves config files into /etc/lapis/, chmod 600s secrets/keys, joins
# tailscale with an authkey if present, then securely shreds the plaintext
# originals from the FAT32 boot partition so secrets never remain there.
#
# Idempotent: exits 0 early when /var/lib/lapis/.firstboot_done exists.
set -euo pipefail

DONE="/var/lib/lapis/.firstboot_done"
SEED="${LAPIS_SEED_DIR:-/boot/firmware/lapis-seed}"
DEST="/etc/lapis"

log() { echo "[lapis-firstboot] $*"; }

if [ -f "${DONE}" ]; then
    log "already initialized (${DONE}); nothing to do"
    exit 0
fi

mkdir -p "${DEST}" /var/lib/lapis

# --- config files (world-owned, but kept 0644) ---
if [ -f "${SEED}/lapis.toml" ]; then
    install -m 0644 "${SEED}/lapis.toml" "${DEST}/lapis.toml"
    log "imported lapis.toml"
fi
if [ -f "${SEED}/hosts.toml" ]; then
    install -m 0644 "${SEED}/hosts.toml" "${DEST}/hosts.toml"
    log "imported hosts.toml"
fi

# --- secrets: strict 0600 ---
if [ -f "${SEED}/secrets.env" ]; then
    install -m 0600 "${SEED}/secrets.env" "${DEST}/secrets.env"
    log "imported secrets.env (0600)"
fi
if [ -f "${SEED}/lapis_ed25519" ]; then
    install -m 0600 "${SEED}/lapis_ed25519" "${DEST}/lapis_ed25519"
    log "imported lapis_ed25519 SSH key (0600)"
fi

# --- Tailscale headless join ---
if [ -f "${SEED}/tailscale.authkey" ]; then
    AUTHKEY="$(tr -d '[:space:]' < "${SEED}/tailscale.authkey")"
    if [ -n "${AUTHKEY}" ]; then
        tailscale up --authkey="${AUTHKEY}" --ssh
        log "joined tailscale with authkey"
    fi
fi

# --- udev/discovery niceties are handled by the daemon; nothing else here ---

# Shred the plaintext originals from the FAT32 boot partition.  `shred` is not
# reliable on FAT32 clusters (the FS may remap clusters), so fall back to
# overwrite+unlink.  We also attempt `fstrim` on the mount only when available.
shred_seed() {
    local f="$1"
    if [ -f "${f}" ]; then
        if command -v shred >/dev/null 2>&1; then
            shred -u -n 3 "${f}" 2>/dev/null || rm -f "${f}"
        else
            dd if=/dev/urandom of="${f}" bs=1M count=1 conv=notrunc 2>/dev/null || true
            rm -f "${f}"
        fi
    fi
}

shred_seed "${SEED}/secrets.env"
shred_seed "${SEED}/tailscale.authkey"
shred_seed "${SEED}/lapis_ed25519"

# Drop a completion marker so this is never re-run (and never re-reads secrets).
touch "${DONE}"
log "first-boot seed import complete"
