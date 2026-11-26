#!/bin/sh
# meshtasticd with its node ID taken from the CM4 serial number. Without --hwid (or a
# MACAddress in config.yaml) meshtasticd 2.7 reads the Bluetooth hci0 address, which is
# all zeros until the adapter is up early in boot: it then exits with "Blank MAC Address not
# allowed!" and systemd gives up after 5 restarts. With --hwid the node ID is the serial's
# last 8 hex digits (serial ...abcdef12 -> !abcdef12), stable across boots and reflashes.
s=$(tr -d '\0' < /sys/firmware/devicetree/base/serial-number 2>/dev/null | tail -c 8)
case "${s}" in
    *[!0-9a-fA-F]* | "") exec /usr/bin/meshtasticd ;;
esac
exec /usr/bin/meshtasticd --hwid="$(printf '%u' "0x${s}")"
