#!/usr/bin/env bash
# Writes /boot/firmware/lapis-boot-report.txt (the previous boot's report is kept as .prev):
# what a device with no console needs to be diagnosed from a laptop. No secrets: no tokens,
# keys, PINs, login links or network names (Lapis logs are already redacted; the rest is filtered).
set -euo pipefail

OUT="${LAPIS_REPORT:-/boot/firmware/lapis-boot-report.txt}"
BOOT_ID=$(cat /proc/sys/kernel/random/boot_id)
TMP="${OUT}.tmp"

scrub() {
    sed -E 's/tskey-[A-Za-z0-9_-]+/tskey-[REDACTED]/g; s#https://login\.tailscale\.com/[^ ]+#https://login.tailscale.com/[REDACTED]#g'
}
section() { printf '\n===== %s =====\n' "$1"; }

if [ -f "${OUT}" ] && ! grep -q "boot_id ${BOOT_ID}" "${OUT}"; then
    mv -f "${OUT}" "${OUT%.txt}.prev.txt" 2>/dev/null || true
fi

{
    echo "Lapis-Deck boot report  $(date -u +%FT%TZ)  boot_id ${BOOT_ID}"
    echo "uptime: $(cut -d' ' -f1 /proc/uptime)s  kernel: $(uname -r)"
    cat /etc/lapis/build-info 2>/dev/null || true
    section "cmdline"; cat /proc/cmdline
    section "boot time"; systemd-analyze 2>&1 || true
    section "failed units"; systemctl --failed --no-legend --no-pager 2>&1 || true
    section "lapis units"
    for u in lapis-rails lapis-firstboot lapis-daemon lapis-harness lapis-ui lapis-tailscale-up; do
        printf '%-22s %s (restarts %s)\n' "$u" "$(systemctl is-active "$u" 2>/dev/null)" "$(systemctl show -p NRestarts --value "$u" 2>/dev/null)"
    done
    section "display: DRM cards and connectors"
    ls -l /dev/dri 2>&1 || true
    for c in /sys/class/drm/card[0-9]; do
        [ -e "$c" ] || continue
        echo "$(basename "$c") driver=$(basename "$(readlink -f "$c/device/driver" 2>/dev/null)")"
    done
    for c in /sys/class/drm/card*-*; do
        [ -e "$c/status" ] || continue
        echo "  $(basename "$c") status=$(cat "$c/status") enabled=$(cat "$c/enabled" 2>/dev/null) modes=$(tr '\n' ' ' < "$c/modes" 2>/dev/null)"
    done
    for b in /sys/class/backlight/*; do
        [ -e "$b/brightness" ] || continue
        echo "backlight $(basename "$b"): $(cat "$b/brightness")/$(cat "$b/max_brightness") bl_power=$(cat "$b/bl_power" 2>/dev/null)"
    done
    section "lapis-ui log"; journalctl -b -u lapis-ui --no-pager -o short-monotonic -n 120 2>&1 | scrub
    section "lapis-daemon log"; journalctl -b -u lapis-daemon --no-pager -o short-monotonic -n 60 2>&1 | scrub
    section "lapis-harness log"; journalctl -b -u lapis-harness --no-pager -o short-monotonic -n 40 2>&1 | scrub
    section "meshtasticd"
    echo "state: $(systemctl is-active meshtasticd 2>/dev/null) (restarts $(systemctl show -p NRestarts --value meshtasticd 2>/dev/null))  lora rail flag: $([ -e /run/lapis-rails/lora ] && echo present || echo absent)"
    ls -l /dev/spidev* 2>&1 || true
    journalctl -b -u meshtasticd --no-pager -o short-monotonic -n 60 2>&1 | scrub
    section "kernel: display, GPU, panel, input, power"
    dmesg 2>/dev/null | grep -iE 'drm|vc4|v3d|dsi|panel|backlight|simple-?framebuffer|fbcon|input:|axp|ocp|error|fail' | tail -n 120 || true
    section "input devices"; grep -E '^N: Name=' /proc/bus/input/devices 2>/dev/null || true
    section "storage"; lsblk -o NAME,SIZE,FSTYPE,MOUNTPOINT 2>&1 || true; df -h / /boot/firmware 2>&1 || true
    section "radios"; rfkill 2>&1 || true
    section "tailscale"; tailscale status --json 2>/dev/null | grep -m1 '"BackendState"' || echo "not running"
    section "listening sockets"; ss -ltnp 2>/dev/null | awk 'NR==1 || /127\.0\.0\.1|0\.0\.0\.0|\[::\]/' || true
    section "memory"; free -m 2>&1 || true
} > "${TMP}" 2>&1
mv -f "${TMP}" "${OUT}"
sync
