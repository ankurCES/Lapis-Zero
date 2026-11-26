#!/bin/bash
# Tailscale and meshtasticd from their apt repositories; readsb (ADS-B) and rx_tools
# (SoapySDR `rx_sdr`) built from source, as Debian packages neither. No secrets here.
set -euo pipefail

install -m 0644 files/meshtasticd-lapis.yaml "${ROOTFS_DIR}/tmp/lora-hackergadgets-aio.yaml"

on_chroot << 'CHROOT'
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
install -d -m 0755 /usr/share/keyrings

# Tailscale (the only way the control plane leaves the device: `tailscale serve`).
curl -fsSL https://pkgs.tailscale.com/stable/debian/trixie.noarmor.gpg -o /usr/share/keyrings/tailscale-archive-keyring.gpg
echo 'deb [signed-by=/usr/share/keyrings/tailscale-archive-keyring.gpg] https://pkgs.tailscale.com/stable/debian trixie main' > /etc/apt/sources.list.d/tailscale.list

# meshtasticd (Debian 13 builds on openSUSE OBS).
curl -fsSL https://download.opensuse.org/repositories/network:Meshtastic:beta/Debian_13/Release.key | gpg --dearmor > /usr/share/keyrings/meshtastic.gpg
echo 'deb [signed-by=/usr/share/keyrings/meshtastic.gpg] http://download.opensuse.org/repositories/network:/Meshtastic:/beta/Debian_13/ /' > /etc/apt/sources.list.d/meshtastic.list

apt-get update
apt-get install -y --no-install-recommends tailscale meshtasticd
install -d -m 0755 /etc/meshtasticd/config.d
install -m 0644 /tmp/lora-hackergadgets-aio.yaml /etc/meshtasticd/config.d/lora-hackergadgets-aio.yaml
rm -f /tmp/lora-hackergadgets-aio.yaml

# Build dependencies, removed again below.
BUILD="git build-essential cmake pkg-config librtlsdr-dev libsoapysdr-dev libncurses-dev zlib1g-dev libzstd-dev"
# shellcheck disable=SC2086
apt-get install -y --no-install-recommends ${BUILD}
work=$(mktemp -d)
git clone --depth 1 https://github.com/wiedehopf/readsb "${work}/readsb"
make -C "${work}/readsb" -j"$(nproc)" RTLSDR=yes
install -m 0755 "${work}/readsb/readsb" "${work}/readsb/viewadsb" /usr/local/bin/
git clone --depth 1 https://github.com/rxseger/rx_tools "${work}/rx_tools"
cmake -S "${work}/rx_tools" -B "${work}/rx_tools/build" -DCMAKE_BUILD_TYPE=Release
make -C "${work}/rx_tools/build" -j"$(nproc)"
install -m 0755 "${work}/rx_tools/build/rx_sdr" "${work}/rx_tools/build/rx_fm" "${work}/rx_tools/build/rx_power" /usr/local/bin/
rm -rf "${work}"
# shellcheck disable=SC2086
apt-get purge -y ${BUILD}
apt-get autoremove -y
apt-get clean
CHROOT
