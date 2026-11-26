# Lapis-Deck OS image (`os-image/`)

A flashable Debian 13 *trixie* arm64 image for the ClockworkPi uConsole with a Raspberry Pi
CM4 (and the HackerGadgets AIO board): it boots straight into the Lapis-Deck HUD on the DSI
panel. No X11, no Wayland, no display manager.

Base: [ak-rex/ClockworkPi-pi-gen](https://github.com/ak-rex/ClockworkPi-pi-gen) stages 0–2
(trixie lite, `akrex-kernel` from the ak-rex apt repo, `clockworkpi-uconsole-cm4` overlay,
audio pop fix, AXP228 charging rule), pinned to a commit (`PIGEN_REF` in `build.sh`), then
`stage-lapis/`:

| Sub-stage | What it does |
| --- | --- |
| `00-system` | appends the Lapis block to `config.txt` (`enable_uart=1`, `dtparam=i2c_arm=on`, `dtoverlay=i2c-rtc,pcf85063a`, `dtoverlay=pps-gpio,gpiopin=6`, `dtoverlay=spi1-1cs`; KMS = the base's `vc4-kms-v3d[-pi4]`), strips `console=serial0,115200` from `cmdline.txt` (GPS on `/dev/ttyS0`), quiet boot with the Plymouth splash (`splash`), multi-user target, masks getty on tty1/ttyS0, volatile journal |
| `01-packages` | gpsd + PPS + chrony, rtl-sdr, rtl-433, multimon-ng, HackRF/Airspy/SoapySDR, tcpdump/nmap, BlueZ, NetworkManager, Mesa GBM/EGL; Plymouth (boot splash); Tailscale and meshtasticd from their repos (meshtasticd configured for the AIO SX1262); `readsb` and `rx_tools` built from source |
| `02-lapis-stack` | `/usr/local/bin/lapis-daemon`, `/usr/local/bin/lapis-ui`, the harness venv in `/opt/lapis/harness` |
| `03-services` | systemd units (meshtasticd only while the LoRa rail is on), `/etc/lapis/*`, the Lapis Plymouth theme (art: `make-splash.py`), seed README, enables, `00-lapis.preset` (`disable *`), build-time checks (units parse; getty off; no desktop package installed) |

Services: `lapis-rails` (AIO V2 power rails at boot) → `lapis-daemon` (root, group `lapis`,
127.0.0.1:8421) → `lapis-harness` (user `lapis`, `/run/lapis/harness.sock`) → `lapis-ui`
(user `lapis`, DRM master on the panel, evdev). `lapis-tailscale-up` joins with a seeded key
and runs `tailscale serve --bg --https=443 http://127.0.0.1:8421` (verified by reading it
back). The account `lapis` has no password; remote shell access is Tailscale SSH.

## Build

Requirements: Docker running, `uv`, ~20 GB free disk, network.

```bash
os-image/build.sh
```

Output in `os-image/deploy/`: `lapis-deck-cm4-trixie-arm64.img.xz`, its `.sha256`, and
`os-list.json` (Raspberry Pi Imager: *Choose OS → Use custom*, or
`rpi-imager --repo file://$PWD/os-image/deploy/os-list.json`). The image grows its root
filesystem on the first boot (the base's `rpi-resize`).

## Flash

```bash
diskutil list                                   # find the card, e.g. /dev/disk6
os-image/flash.sh /dev/disk6
```

`flash.sh` refuses internal disks, makes you type the disk name, writes `/dev/rdiskN` with
`sudo dd`, then verifies the root filesystem byte for byte and the boot partition file by file
(macOS writes Spotlight files into the FAT partition, so a whole-card hash never matches).

## First boot

About 15–25 s to the HUD. An unpaired deck opens a Bluetooth pairing window by itself: the
SYSTEM workspace shows a 6-digit PIN. In the Lapis companion app choose *Pair new device*; it
sets up Wi-Fi, Tailscale and your LLM keys. Once on the tailnet, SYSTEM → *pair phone: QR*
pairs more phones instantly. Optional seed files: see `/boot/firmware/lapis-seed/README.txt`.

See `HARDWARE_CHECKLIST.md` for what to verify on a real uConsole.
