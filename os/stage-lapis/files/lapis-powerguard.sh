#!/usr/bin/env bash
# lapis-powerguard.sh — clean shutdown when the battery is genuinely empty.
#
# Powers off only when ALL of these hold on two consecutive runs (30 s apart):
#   * the gauge says the cell is being drained (Discharging / Not charging),
#   * it reports <= 3 %,
#   * the cell voltage agrees (< 3.40 V) and is physically plausible (>= 2.50 V).
# A gauge that is absent, resetting, unreadable, or reporting 0 V / "Unknown"
# is never treated as an empty battery: a board that is running cannot be fed
# by a 0 V cell, so such a reading means "no measurement", not "empty".
# Pure sysfs reads; no external tools.
set -euo pipefail

CRITICAL_PCT=3
CRITICAL_UV=3400000
PLAUSIBLE_MIN_UV=2500000
STRIKES_NEEDED=2
STRIKES="${LAPIS_POWERGUARD_STATE:-/run/lapis-powerguard.strikes}"

find_battery() {
    for d in /sys/class/power_supply/*/; do
        if [ "$(cat "${d}type" 2>/dev/null || true)" = "Battery" ]; then
            echo "${d%/}"
            return 0
        fi
    done
    return 1
}

ok() { # not critical (or not knowable): forget earlier strikes
    rm -f "${STRIKES:?}"
    exit 0
}

BAT="${LAPIS_BATTERY_SYSFS:-}"
if [ -z "${BAT}" ] || [ ! -d "${BAT}" ]; then
    BAT="$(find_battery)" || ok
fi

status="$(cat "${BAT}/status" 2>/dev/null || echo Unknown)"
case "${status}" in
    Discharging | "Not charging") ;; # the only states in which "empty" is believable
    *) ok ;;                         # Charging, Full, Unknown, unreadable
esac

pct="$(cat "${BAT}/capacity" 2>/dev/null || true)"
uv="$(cat "${BAT}/voltage_now" 2>/dev/null || true)"
case "${pct}" in '' | *[!0-9]*) ok ;; esac # a reading is missing or not a number: never guess
case "${uv}" in '' | *[!0-9]*) ok ;; esac

if [ "${pct}" -le "${CRITICAL_PCT}" ] && [ "${uv}" -lt "${CRITICAL_UV}" ] &&
    [ "${uv}" -ge "${PLAUSIBLE_MIN_UV}" ]; then
    seen="$(cat "${STRIKES}" 2>/dev/null || true)"
    case "${seen}" in '' | *[!0-9]*) seen=0 ;; esac
    seen=$((seen + 1))
    echo "${seen}" > "${STRIKES}"
    if [ "${seen}" -ge "${STRIKES_NEEDED}" ]; then
        logger -t lapis-powerguard "battery ${pct}% at ${uv} uV (${status}); powering off"
        sync
        ${LAPIS_POWEROFF:-systemctl poweroff}
    fi
else
    ok
fi
