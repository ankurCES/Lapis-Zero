#!/usr/bin/env bash
# lapis-tailscale-up.sh — join the tailnet with a seeded auth key (if any) and
# expose the daemon's loopback control plane over Tailscale HTTPS.
#
# The control plane only ever listens on 127.0.0.1:8421; `tailscale serve`
# (never `funnel`) is the single way it becomes reachable, and only inside the
# tailnet.  Exits non-zero while offline so systemd retries with backoff.
set -euo pipefail

KEY="${LAPIS_ETC_DIR:-/etc/lapis}/tailscale.authkey"
TARGET="http://127.0.0.1:8421"
log() { echo "[lapis-tailscale-up] $*"; }

state() {
    tailscale status --json 2>/dev/null | sed -n 's/.*"BackendState": *"\([A-Za-z]*\)".*/\1/p' | head -1
}

drop_key() {
    shred -u -n 1 "${KEY:?}" 2>/dev/null || rm -f "${KEY:?}"
}

if [ -s "${KEY}" ] && [ "$(state)" != "Running" ]; then
    log "joining tailnet with the seeded auth key"
    # The CLI never prints the key; its last line is the reason on failure.
    if ! out="$(tailscale up --auth-key="file:${KEY}" --ssh --timeout=60s 2>&1)"; then
        case "${out}" in
            *"invalid key"* | *"key expired"* | *"not valid"*)
                # A rejected key never becomes valid, and re-submitting it every
                # few minutes would cancel an interactive login in progress.
                log "the seeded auth key was rejected by Tailscale; discarding it (pair over Bluetooth instead)"
                drop_key
                ;;
            *) log "tailscale up did not complete: ${out##*$'\n'}" ;;
        esac
    fi
fi

if [ "$(state)" != "Running" ]; then
    log "tailscale is not running yet (state: $(state)); will retry"
    exit 1
fi

# Joined: the one-time key has done its job.
if [ -f "${KEY}" ]; then
    drop_key
fi

# Persisted by tailscaled across reboots; re-asserting it is idempotent.
# `tailscale serve` exits 0 without configuring anything (or blocks) when HTTPS
# certificates are not enabled for the tailnet, so its exit status proves
# nothing: bound it, then check what tailscaled actually stored.
timeout 30 tailscale serve --bg --https=443 "${TARGET}" < /dev/null || true
if ! tailscale serve status --json 2>/dev/null | grep -q "${TARGET#http://}"; then
    log "tailnet joined, but HTTPS serve is not active: enable MagicDNS and HTTPS Certificates for the tailnet (admin console, DNS page); will retry"
    exit 1
fi
log "control plane served on the tailnet (https, 443 -> 127.0.0.1:8421)"
