#!/usr/bin/env bash
# lapis-powerguard.sh — clean shutdown when the BQ27220 reports <=3% and the
# battery is not charging.  Pure sysfs read; no external tools required.
set -euo pipefail

# BQ27220 exposes charge state via power_supply class.  We look for a
# `battery` (or `bq27220`) device and read charge_now / charge_full / status.
find_battery() {
    for d in /sys/class/power_supply/*/; do
        t="$(cat "${d}type" 2>/dev/null || true)"
        if [ "$t" = "Battery" ]; then
            echo "${d%/}"
            return 0
        fi
    done
    return 1
}

BAT="${LAPIS_BATTERY_SYSFS:-}"
if [ -z "${BAT}" ] || [ ! -d "${BAT}" ]; then
    BAT="$(find_battery)" || { exit 0; }
fi

status="$(cat "${BAT}/status" 2>/dev/null || echo Unknown)"
if [ "${status}" = "Charging" ] || [ "${status}" = "Full" ]; then
    exit 0
fi

now="$(cat "${BAT}/charge_now" 2>/dev/null || true)"
full="$(cat "${BAT}/charge_full" 2>/dev/null || true)"
if [ -z "${now}" ] || [ -z "${full}" ] || [ "${full}" -le 0 ] 2>/dev/null; then
    exit 0
fi

pct=$(( now * 100 / full ))
if [ "${pct}" -le 3 ]; then
    logger -t lapis-powerguard "battery ${pct}% and not charging; powering off"
    sync
    systemctl poweroff
fi
