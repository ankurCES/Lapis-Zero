#!/usr/bin/env bash
# Power the HackerGadgets AIO V2 rails at boot (all are off at power-on): GPS 27, LoRa 16,
# SDR 7, internal USB hub 23 (active high, per aiov2_ctl). The board revision and pins come
# from /etc/lapis/deck.toml ([rails] board = "v1" skips this: V1 rails are always on).
# ${STATE}/lora exists while the LoRa radio has power: meshtasticd starts only then (its drop-in).
set -euo pipefail

CONF="${LAPIS_DECK_TOML:-/etc/lapis/deck.toml}"
STATE="${LAPIS_RAILS_STATE:-/run/lapis-rails}"
mkdir -p "${STATE}"
rm -f "${STATE}/lora"
board="v2"
if [ -f "${CONF}" ]; then
    b=$(sed -n 's/^[[:space:]]*board[[:space:]]*=[[:space:]]*"\([a-z0-9]*\)".*/\1/p' "${CONF}" | head -1)
    board="${b:-v2}"
fi
[ "${board}" = "v1" ] && : > "${STATE}/lora"
if [ "${board}" != "v2" ]; then
    echo "lapis-rails: board ${board}: nothing to switch"
    exit 0
fi
if ! command -v pinctrl >/dev/null; then
    echo "lapis-rails: pinctrl not installed; rails left as they are" >&2
    exit 0
fi
pin() {
    local name="$1" default="$2" v
    v=$(sed -n "s/^[[:space:]]*${name}_pin[[:space:]]*=[[:space:]]*\([0-9]*\).*/\1/p" "${CONF}" 2>/dev/null | head -1)
    echo "${v:-${default}}"
}
for rail in gps:27 lora:16 sdr:7 usb_hub:23; do
    name="${rail%%:*}"
    p=$(pin "${name}" "${rail##*:}")
    pinctrl set "${p}" op dh
    echo "lapis-rails: ${name} on (GPIO${p})"
done
: > "${STATE}/lora"
