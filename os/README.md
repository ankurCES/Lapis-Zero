# Lapis-Zero OS — Custom pi-gen Image Pipeline (`os/`)

Builds the standalone, flashable **Lapis-Zero OS** SD-card image for the
M5Stack CardputerZero (Raspberry Pi CM0-class, 512 MB RAM) from
`CardputerZero/pi-gen`'s `cardputerzero_v1.1` branch and our own
`stage-lapis` overlay. The result is a headless Debian Trixie arm64 image:
the CardputerZero hardware BSP plus `lapis-ui`, `lapis-daemon`, Tailscale and
the on-demand BLE provisioning service. No desktop, no X11/Wayland.

## How Lapis takes over the device

`stage2` of the upstream pi-gen branch is the hardware BSP, but it also installs
M5Stack's own launcher. Several things in it would keep Lapis off the screen, and
`stage-lapis` deals with each one explicitly. Every row below was learned on the
real device (2026-11-24), not in a container:

| Upstream behaviour | What `stage-lapis` does |
| --- | --- |
| `LaunchWizard.service` (first-boot wizard) is enabled and owns the LCD and keypad | masked, its markers removed, the launcher packages purged (`00-os-tuning`) |
| `fb_load.service` shows the boot logo and **keeps the LCD's DRM device open until the stock launcher starts** (it polls for those two executables). While it runs, everything drawn on the fbdev is invisible | **masked**, in `/etc` and with `systemd.mask=fb_load.service` on the kernel command line. `Conflicts=fb_load.service` on the UI unit does *not* work: `sysinit.target` wants the splash, so any later transaction (the Bluetooth adapter appearing is enough) starts it again next to a running UI |
| With `fbcon=map:off` nothing enables the display pipeline | `lapis-ui` unblanks and re-activates the mode itself when it opens `/dev/fb_lcd`; this is what initialises the panel |
| For the first ~12 s of boot `/dev/fb0` is the firmware's 16 bpp simple-framebuffer, which the kernel then removes | `lapis-ui` accepts only a 320x170, 16 bpp framebuffer and keeps waiting until the panel's appears |

There is no boot logo: the screen stays dark for about 15 seconds, then the UI
appears. (The firmware splash needs the BSP's `start.elf`, which `gpu_mem=16`
does not load.)

`lapis-ui.service` has no device or network dependencies on purpose (a
`dev-fb0.device` dependency can never be satisfied: systemd does not create
device units for framebuffers). The UI waits for the LCD and the keypad itself.

### What else the image switches off

| What | Why |
| --- | --- |
| systemd's first-boot preset pass (`00-lapis.preset`: `disable *`) | the image ships `/etc/machine-id=uninitialized`, and Debian's default preset policy is to enable every installed unit (sshd, adbd, systemd-networkd and its 120 s wait, apt timers). On the device that pass finds `/` still read-only and does nothing; in a container it switches everything on. The preset makes both behave the same: only what the build enabled runs |
| `adbd.service` (masked) | it is a root shell on tcp/5555 if anything ever enables it |
| `cardputerzero-diskid.service` (not installed: `RANDOMIZE_DISK_ID_ON_FIRST_BOOT=0`, and masked) | it rewrites the MBR disk ID of the running card on the first boot; a power cut in that window leaves `cmdline.txt` naming a partition that does not exist |
| `fstrim.service` (masked), cloud-init (`/etc/cloud/cloud-init.disabled`) | a full-card TRIM and a Python start-up in front of `sysinit.target`, neither of which does anything useful here. `user-data` and `network-config` on the FAT partition are therefore inert |
| NetworkManager's `WirelessEnabled=false` (set to `true`) | pi-gen ships it off when no Wi-Fi country is set, and NetworkManager then soft-blocks the radio at every start |

## Layout

```
os/
├── config.pigen                     # pi-gen config (IMG_NAME, STAGE_LIST, xz, fixed 5 GiB size)
├── build-image.sh                   # cross-builds lapis-ui, builds the wheel, runs pi-gen in Docker
├── generate-imager-manifest.py      # emits M5 Imager os-list.json (SHA256 + sizes)
├── tests/ble-real-bluez/            # pairing start-up against the image's real bluetoothd (see its README)
├── stage-lapis/                     # custom pi-gen stage (runs after the BSP's stage2)
│   ├── prerun.sh                    # copies the rootfs from stage2
│   ├── EXPORT_IMAGE                 # exports the .img.xz at the end of stage-lapis
│   ├── 00-os-tuning/                # launcher handover, gpu_mem=16, cmdline, zram, fstab, journald
│   ├── 01-tailscale/                # Tailscale apt repo + tailscaled
│   ├── 02-lapis-stack/              # lapis-ui binary + Python venv (deps from PyPI, binary wheels)
│   ├── 03-services-firstboot/       # units, default /etc/lapis, seed README, build-time unit checks
│   └── files/
│       ├── systemd/                 # lapis-{ui,daemon,firstboot,tailscale-up,ble-prov,bootreport,powerguard}
│       ├── etc/                     # default lapis.toml / hosts.toml shipped in the image
│       ├── lapis-firstboot.sh       # radio unblock + seed import (never touches the network)
│       ├── lapis-tailscale-up.sh    # auth-key join + `tailscale serve` for 127.0.0.1:8421
│       ├── lapis-bootreport.sh      # writes /boot/firmware/lapis-boot-report.txt every boot
│       ├── lapis-powerguard.sh      # shutdown at <= 3 % AND < 3.40 V, not charging
│       ├── swap-lapis.conf          # rpi-swap: zram only, 512 MiB
│       └── st7789-firmware          # initramfs hook for the LCD firmware blob
└── docs/
    └── setup_target_hint.md         # binding target VNC to the Tailscale IP
```

## Prerequisites

- Docker, with the daemon running (`docker info` must succeed)
- ~12 GB free disk (pi-gen work volume + the 5 GiB raw image)
- Network access (Debian, Raspberry Pi and Tailscale apt repos, PyPI, GitHub)
- `cargo-zigbuild` + `zig` (cross-compiles `lapis-ui`), `uv` (builds the wheel)
- `shellcheck` for the lint target

## Build the image

```bash
make -C os deploy
```

`build-image.sh` always rebuilds `lapis-ui` and the daemon wheel from the
current source before running pi-gen, so an image never ships a stale binary.
Overrides: `LAPIS_UI_BIN=...`, `LAPIS_DAEMON_WHEEL=...`, `REPO_PATH=...` (an
existing pi-gen checkout), `PIGEN_BRANCH=...`.

Artifacts land in `os/deploy/`:

```
lapis-zero-arm64.img.xz     # what you flash / host for M5 Imager
lapis-zero-arm64.img        # the same image, uncompressed (5 GiB, for dd and hashing)
os-list.json                # M5 Imager manifest with exact sizes and SHA-256s
```

The manifest's download `url` points at the local `.img.xz` (a `file://` URL), so it works for
a local flash as built. For a published release, build with
`LAPIS_RELEASE_URL=https://…/lapis-zero-arm64.img.xz make -C os deploy`.

The image is a **fixed 5 GiB** (`IMAGE_TOTAL_SIZE` in `config.pigen`): the
CardputerZero pi-gen branch has no first-boot root resize, so an auto-sized
image would run with a nearly full root filesystem forever. Any card of 8 GB or
more works.

`make -C os lint` runs `shellcheck` over every script and the manifest
generator's self-test; it needs no Docker.

## Flash

**M5 Imager** (`CardputerZero/m5stack-imager`): *Choose OS → Use custom* and pick
`lapis-zero-arm64.img.xz`, or start it with
`m5stack-imager --repo file:///abs/path/os/deploy/os-list.json` so "Lapis-Zero OS"
appears under the CardputerZero device. Its OS customisation (hostname, Wi-Fi,
SSH user) is optional.

**Command line** (macOS; double-check the disk number, this erases it):

```bash
diskutil list                      # find the SD card, e.g. /dev/disk6
diskutil unmountDisk /dev/disk6
xz -dc os/deploy/lapis-zero-arm64.img.xz | sudo dd of=/dev/rdisk6 bs=4m
diskutil eject /dev/disk6
```

## First boot

What you should see: a dark screen for about 15 seconds, then the Lapis UI. On a
card nobody has configured, the UI shows the **pairing screen with a 6-digit
PIN**: open the Android app, choose *Pair new CardputerZero*, and it will set up
Wi-Fi and Tailscale over Bluetooth. No files to edit. If Bluetooth cannot start,
the same screen says "Bluetooth unavailable" with the reason instead of a PIN.

The backlight stays on while a PIN is showing. Otherwise it dims after 30 s
without a key press and switches off after 90 s; any key wakes it.

### Optional: seed files on the FAT partition

Copy any of these into `lapis-seed/` on the card's `bootfs` partition before the
first boot (a `README.txt` there repeats this):

| File | Purpose |
| --- | --- |
| `lapis.toml` | LLM provider/model/schedules → `/etc/lapis/lapis.toml` |
| `hosts.toml` | Tailscale target hosts → `/etc/lapis/hosts.toml` |
| `secrets.env` | `ANTHROPIC_API_KEY=…`, `LAPIS_VNC_*=…` → `/etc/lapis/secrets.env` (`0600`) |
| `tailscale.authkey` | pre-auth key for a headless tailnet join |
| `lapis_ed25519` | SSH private key for target hosts (`0600`) |

`lapis-firstboot.service` imports them and removes the three secret files from
the FAT partition. The Tailscale join itself is done by
`lapis-tailscale-up.service`, which retries in the background until the network
is up, so a missing network never delays the UI.

## Troubleshooting a device with no console

**`lapis-boot-report.txt`** is rewritten on the FAT partition 45 seconds after
every boot, again at 3, 6, 10, 15, 20 and 30 minutes, and whenever a Bluetooth
pairing session ends. The previous boot's report is kept beside it as
`lapis-boot-report.prev.txt`. It holds: which image this is, failed units, the
state and log lines of the Lapis services and Bluetooth (each pairing step is
logged by name), boot timing, which framebuffer the UI opened and whether the
panel's pipeline is enabled, radios, Tailscale and control-plane state,
listening sockets, battery, memory, and the relevant kernel messages. If the
device does not do what you expect, leave it on for three minutes, power off,
put the card in a laptop and read that file first. It contains no PIN,
password, key, token, login link or network name.

| Symptom | Where to look |
| --- | --- |
| Screen stays dark | press a key first (the backlight sleeps after 90 s). Then the `lapis-ui` section: it logs `display on /dev/fb_lcd (panel-mipi-dbid)` once it has the panel, or `waiting for the LCD framebuffer` with the reason. the `/sys/class/drm/card1-*/enabled` line in the display section says whether the panel was initialised, and `fb_load` must read `masked inactive` |
| The M5 logo shows and stays | something un-masked `fb_load.service`: see its line in the report |
| No report at all | Linux never got 45 s in: on the FAT partition set `gpu_mem=64` in `config.txt` (this switches from the cut-down `start_cd.elf` back to the BSP's `start.elf`) and try again |
| "Bluetooth unavailable" on the pairing screen | the `lapis-ble-prov` and `bluetooth` sections |
| Phone finds no Wi-Fi networks | the radios section: `wifi radio (NetworkManager)` must be `enabled` and `phy0` not blocked |
| UI shows `OFFLINE` | `lapis-daemon` section of the report |
| Keys do nothing | the report lists `/dev/input/by-path/`; `lapis-ui` logs `keypad ready` or why not |

Do not add diagnostics through cloud-init's `user-data`: it is disabled in this
image, and on the stock BSP a `bootcmd` that waits on systemd deadlocks the boot
before `sysinit.target`.

See also `docs/setup_target_hint.md` for binding a target host's VNC/SSH
strictly to its Tailscale IP.
