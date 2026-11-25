#!/usr/bin/env bash
# lapis-bootreport.sh — write a short, secret-free boot diagnosis to the FAT
# partition.  The appliance has no console: if the screen is not what you
# expect, pull the SD card and read lapis-boot-report.txt on any laptop.
#
# The journal is volatile, so this file is the only evidence that survives a
# power cycle.  It must answer, from one report: did the UI get the LCD (and
# which framebuffer did it open), is the panel's pipeline enabled, are the
# radios usable, why did a unit fail, and what is listening.
# Nothing here prints a PIN, a password, a key, a token, a login link or an
# SSID.
set -u

OUT="${LAPIS_BOOT_REPORT:-/boot/firmware/lapis-boot-report.txt}"
TMP="$(mktemp)"
T="timeout 5" # nothing here may hang the report on a daemon that is down

# The first report of a boot keeps the previous boot's report next to it.
FIRST="${LAPIS_BOOT_REPORT_FLAG:-/run/lapis-bootreport.first}"
if [ ! -e "${FIRST}" ]; then
    : > "${FIRST}" 2> /dev/null || true
    if [ -f "${OUT}" ]; then cp "${OUT}" "${OUT%.txt}.prev.txt" 2> /dev/null || true; fi
fi

unit_log() { # unit [last-lines]: state, restart facts, the first lines and the last ones
    echo "== $1: $(systemctl is-active "$1.service" 2>&1) ($(systemctl show -p NRestarts,ExecMainStatus,Result,MemoryPeak "$1.service" 2>&1 | paste -sd' '))"
    journalctl -b -u "$1.service" --no-pager -o short-monotonic 2>&1 |
        awk -v n="${2:-20}" '{ l[NR] = $0 } END { for (i = 1; i <= NR; i++) if (i <= 6 || i > NR - n) print "   " l[i]; else if (i == 7) print "   ..." }'
}

# What the daemon would tell the UI, reduced to facts that are safe on an SD
# card: the frame itself contains the pairing PIN and is never printed.
daemon_state() {
    ${T} /opt/lapis/venv/bin/python -c '
import json, socket, sqlite3
try:
    s = socket.socket(socket.AF_UNIX); s.settimeout(3); s.connect("/run/lapis/lapisd.sock")
    st = json.loads(s.makefile().readline())
    p = st.get("pairing") or {}
    print("agent=%s tailscale=%s wifi=%s api_key=%s pairing=%s/%s pin_shown=%s" % (
        st.get("agent_state"), st.get("tailscale_state"), "yes" if st.get("wifi_ssid") else "no",
        st.get("api_key_ready"), p.get("mode", "off"), p.get("status", "-"), "yes" if p.get("pin") else "no"))
    if p.get("error"):
        print("pairing error:", str(p["error"])[:120])
except Exception as exc:
    print("daemon socket:", type(exc).__name__, exc)
try:
    db = sqlite3.connect("file:/var/lib/lapis/lapis.db?mode=ro", uri=True)
    print("paired phones:", db.execute("select count(*) from paired_devices where revoked=0").fetchone()[0])
except Exception as exc:
    print("paired phones: ?", type(exc).__name__)
' 2>&1
}

{
    echo "Lapis-Zero boot report — $(date -u +%Y-%m-%dT%H:%M:%SZ) (uptime $(cut -d' ' -f1 /proc/uptime)s)"
    echo "kernel: $(uname -r)   model: $(tr -d '\0' < /proc/device-tree/model 2>/dev/null)"
    echo "cmdline: $(cat /proc/cmdline 2>&1)"
    echo "image: $(stat -c %y /etc/rpi-issue 2> /dev/null | cut -c1-19)   lapis-ui $(sha256sum /opt/lapis/bin/lapis-ui 2> /dev/null | cut -c1-12)   boot $(cut -c1-8 /proc/sys/kernel/random/boot_id 2> /dev/null)   state: $(systemctl is-system-running 2>&1)"
    echo "daemon: $(daemon_state | paste -sd'|')"
    echo
    echo "== failed units"
    systemctl --failed --no-legend --no-pager 2>&1 | head -20
    echo "== pending jobs"
    systemctl list-jobs --no-legend --no-pager 2>&1 | head -12
    echo "== reloads, stops and failed dependencies during this boot"
    journalctl -b --no-pager -o short-monotonic 2>&1 |
        grep -E 'Reload requested|Stopped |Unmount|Dependency failed|Failed to populate' | tail -15
    echo
    for unit in lapis-ui lapis-daemon lapis-ble-prov lapis-firstboot lapis-tailscale-up lapis-powerguard; do
        unit_log "${unit}"
    done
    unit_log bluetooth 12
    echo "== fb_load (must be masked): $(systemctl show -p LoadState,ActiveState --value fb_load.service 2>&1 | paste -sd' ')"
    echo
    echo "== boot timing"
    ${T} systemd-analyze time 2>&1 | head -3
    ${T} systemd-analyze blame --no-pager 2>&1 | head -10
    echo
    echo "== display / input devices"
    ls -l /dev/fb* /dev/dri/ /dev/input/by-path/ 2>&1
    cat /proc/fb 2>&1
    grep -H . /sys/class/graphics/fb*/name /sys/class/graphics/fb*/virtual_size \
        /sys/class/graphics/fb*/bits_per_pixel 2>&1
    # "enabled" on the SPI connector is what proves the panel was initialised.
    grep -H . /sys/class/drm/card*-*/status /sys/class/drm/card*-*/enabled \
        /sys/class/drm/card*-*/dpms 2>&1
    for d in /sys/kernel/debug/dri/[0-9]*; do
        [ -d "${d}" ] || continue
        echo "${d} clients:"
        cat "${d}/clients" 2>&1 | head -8
        grep -E 'crtc\[|active=|plane\[|fb=' "${d}/state" 2>&1 | head -10
    done
    grep -H . /sys/class/backlight/*/brightness /sys/class/backlight/*/max_brightness \
        /sys/class/backlight/*/bl_power 2>&1
    echo
    echo "== radios / network"
    rfkill list 2>&1 | head -20
    ${T} nmcli -t -f DEVICE,TYPE,STATE device 2>&1 | head -8
    echo "wifi radio (NetworkManager): $(${T} nmcli radio wifi 2>&1)"
    echo "wlan0: $(${T} nmcli -g GENERAL.STATE,GENERAL.REASON device show wlan0 2>&1 | paste -sd' ')   saved profiles: $(find /etc/NetworkManager/system-connections -type f 2> /dev/null | wc -l)"
    echo "nm state file: $(tr '\n' ' ' < /var/lib/NetworkManager/NetworkManager.state 2>&1)"
    journalctl -b -u NetworkManager --no-pager -o short-monotonic 2>&1 |
        grep -i -E 'rfkill|device \(wlan0\): state change' | tail -10 | sed 's/^/   /'
    # Why a join failed, without the network's name.
    journalctl -b -u wpa_supplicant --no-pager -o short-monotonic 2>&1 |
        grep -E 'CTRL-EVENT-(SSID-TEMP-DISABLED|ASSOC-REJECT|DISCONNECTED)|WRONG_KEY|4-Way Handshake failed' |
        sed -E 's/ssid=".*" auth_failures=/ssid=<redacted> auth_failures=/; s/ssid="[^"]*"/ssid=<redacted>/g' | tail -6 | sed 's/^/   /'
    iw reg get 2>&1 | head -4
    ${T} bluetoothctl show 2>&1 | grep -E 'Controller|Powered|Discoverable|Pairable|Alias|ActiveInstances|Roles' | head -9
    ${T} busctl tree org.bluez 2>&1 | head -6
    echo "tailscale: $(${T} tailscale status --json 2> /dev/null | grep -m1 BackendState | tr -d ' ",')   serve entries for the control plane: $(${T} tailscale serve status --json 2> /dev/null | grep -c '127.0.0.1:8421')"
    echo "control plane: http $(${T} curl -s -m 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:8421/v1/health 2>&1) without a token (401 = up)"
    echo "listening tcp: $(ss -Hltn 2>&1 | awk '{print $4}' | sort -u | tr '\n' ' ')"
    echo "enabled (ssh adbd systemd-networkd): $(systemctl is-enabled ssh.service adbd.service systemd-networkd.service 2>&1 | paste -sd' ')   cloud-init: $(if [ -e /run/cloud-init/disabled ]; then echo disabled; else echo NOT-DISABLED; fi)/$(systemctl is-active cloud-init-main.service 2>&1)"
    echo
    echo "== time / power / storage"
    timedatectl show -p NTPSynchronized -p TimeUSec -p RTCTimeUSec 2>&1 | paste -sd' '
    for p in /sys/class/power_supply/*/; do
        [ -d "${p}" ] || continue
        printf '%s' "${p}: "
        for f in type status capacity voltage_now current_now; do
            printf '%s=%s ' "${f}" "$(cat "${p}${f}" 2>/dev/null || echo '?')"
        done
        echo
    done
    findmnt -no SOURCE,OPTIONS /boot/firmware 2>&1
    df -h / 2>&1 | tail -1
    echo "throttled: $(${T} vcgencmd get_throttled 2>&1)   $(${T} vcgencmd measure_temp 2>&1)"
    echo
    echo "== memory"
    free -m 2>&1
    swapon --show 2>&1
    echo
    echo "== kernel log (display, keypad, radios, firmware)"
    dmesg 2>&1 | grep -i -E 'simple-framebuffer|st7789|mipi|panel|tca8418|backlight|drm|vc4|fb0|firmware|brcmf|hci0|rfkill|Out of memory|oom|blacklist|powerfail|Emergency Remount|EXT4-fs (error|warning)|FAT-fs|mmc0.*(error|timeout)|I/O error|Under-voltage|bq27' |
        awk '{ l[NR] = $0 } END { for (i = 1; i <= NR; i++) if (i <= 30 || i > NR - 60) print l[i] }'
} > "${TMP}" 2>&1

# One small write per run; never fail the boot over it.
cp "${TMP}" "${OUT}" 2>/dev/null && sync
rm -f "${TMP:?}"
exit 0
