#!/bin/bash
# usage: real_bluez2.sh <label> <delay> <secs> ; env PROV=shipped|patched
label="$1"; delay="${2:-3}"; secs="${3:-6}"
mkdir -p /run/dbus /run/lapis /tmp/build /var/lib/bluetooth
gcc -shared -fPIC -O1 -o /tmp/build/fakebt.so /scratch/fakebt.c -ldl || exit 9
dbus-daemon --system --fork --nopidfile
PY=/opt/lapis/venv/bin/python
$PY /scratch/fake_daemon.py &
python3 /scratch/mgmt_emu.py & sleep 0.5
echo "===== CASE $label: real bluetoothd $(/usr/libexec/bluetooth/bluetoothd --version), prov=${PROV:-shipped}"
LD_PRELOAD=/tmp/build/fakebt.so /usr/libexec/bluetooth/bluetoothd -n ${BTD_ARGS:--d} > /tmp/bluetoothd.log 2>&1 &
btd=$!
sleep "$delay"
[ -n "$PRE_CMD" ] && bash -c "$PRE_CMD"
echo "===== starting lapis-ble-prov"
if [ "${PROV:-shipped}" = patched ]; then cmd="$PY ${PROV_SCRIPT:-/scratch/prov_patched.py}"; else cmd=/opt/lapis/venv/bin/lapis-ble-prov; fi
( sleep "${PROBE_AT:-3}"; [ -n "$PROBE_CMD" ] && bash -c "$PROBE_CMD" ) &
timeout -s TERM "$secs" $cmd; rc=$?
echo "===== lapis-ble-prov exit=$rc"
[ -n "$POST_CMD" ] && bash -c "$POST_CMD"
sleep 0.5
echo "===== bluetoothd alive: $(kill -0 $btd 2>/dev/null && echo yes || echo no)"
echo "===== bluetoothd log (filtered)"
grep -nE "${LOG_FILTER:-rror|ailed|advert|Advert|GATT app|nclude|RegisterApp|set mode|rfkill|RFKILL|Adapter .* registered|power_state|Blocked|handle 0x|Adding Flags|ServiceUUID}" /tmp/bluetoothd.log | cut -c1-230 | tail -${LOG_LINES:-60}
