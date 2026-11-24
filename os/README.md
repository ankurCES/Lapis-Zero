# Lapis-Zero OS — Custom pi-gen Image Pipeline (`os/`)

Builds the standalone, flashable **Lapis-Zero OS** SD-card image for the
M5Stack CardputerZero (Raspberry Pi CM0-class, 512 MB RAM) from
`CardputerZero/pi-gen`'s `cardputerzero_v1.1` branch and our own
`stage-lapis` overlay. The result is a headless Debian Trixie arm64 image:
no desktop, no X11/Wayland, no `APPLaunch`/launcher stages — just the
CardputerZero hardware BSP plus `lapis-ui`, `lapis-daemon`, and Tailscale.

## Layout

```
os/
├── config.pigen                     # pi-gen build config (IMG_NAME, STAGE_LIST, xz)
├── build-image.sh                   # clones pi-gen + runs stages inside Docker
├── generate-imager-manifest.py      # emits M5 Imager os-list.json (SHA256 + sizes)
├── stage-lapis/                     # custom pi-gen stage (replaces desktop stages)
│   ├── prerun.sh                    # rsync-copies rootfs from stage2
│   ├── EXPORT_IMAGE                 # triggers .img.xz export at end of stage-lapis
│   ├── 00-os-tuning/                # gpu_mem=16, fbcon=map:off, zram, fstab, journald
│   ├── 01-tailscale/                # Tailscale apt repo + tailscaled (headless)
│   ├── 02-lapis-stack/              # lapis-ui binary + Python venv daemon install
│   └── 03-services-firstboot/       # systemd units, lapis-firstboot.sh, powerguard
├── files/
│   ├── systemd/                     # lapis-{ui,daemon,firstboot,powerguard}.service
│   ├── initramfs/st7789-firmware    # initramfs hook bundling the LCD firmware blob
│   ├── zram-generator.conf          # 512 MB LZ4 zram swap
│   └── lapis-firstboot.sh           # imports + shreds /boot/firmware/lapis-seed/*
└── docs/
    └── setup_target_hint.md         # binding target VNC to the Tailscale IP
```

## Prerequisites

- Docker, with the daemon running (`docker info` must succeed)
- 8+ GB free disk (the pi-gen build caches a full Debian rootfs)
- Network access to `deb.debian.org` and GitHub (README + build-image default
  `pi-gen` branch is pinned at `cardputerzero_v1.1`)
- `shellcheck` for the lint target

The build uses `build-docker.sh` with QEMU `binfmt_misc` so it also works on
non-`aarch64` hosts (macOS/amd64 Linux).

## Build the image

```bash
make -C os deploy
# or, without a pinned repository:
REPO_PATH=/path/to/CardputerZero/pi-gen PIGEN_BRANCH=cardputerzero_v1.1 make -C os deploy
# or, offline / speed: use a prebuilt binary + source dist instead of re-cross-compiling
LAPIS_UI_BIN=ui/target/aarch64-unknown-linux-gnu/release/lapis-ui \
LAPIS_DAEMON_SDIST=daemon/dist/lapis_daemon-0.1.0.tar.gz \
  make -C os deploy
```

The final artifacts land in `REPO_PATH/work/<IMG_NAME>/export-image/`:

```
lapis-zero-arm64.img        # uncompressed raw image
lapis-zero-arm64.img.xz     # compressed download (compressed ~450–650 MB target)
image_...-lapis-zero-arm64.img.xz   # pi-gen archive name
os-list.json                # M5 Imager manifest (generated into os/deploy/)
```

> `make deploy` runs the real pi-gen build. It requires a running Docker
> daemon, so it does **not** execute in a sandbox without Docker. For lint-only
> verification use `make -C os lint`.

## Flash via M5 Imager

1. Launch the imager (`CardputerZero/m5stack-imager`).
2. Either:
   - **Choose OS → Use custom** and select `lapis-zero-arm64.img.xz`, **or**
   - pass a repo list: `m5stack-imager --repo file:///abs/path/os/deploy/os-list.json`
     (or any served URL) so "Lapis-Zero OS (Trixie arm64)" appears under the
     **CardputerZero** device.
3. Use the imager's OS Customisation (`init_format: "systemd"`) to set hostname
   `lapis-zero`, Wi-Fi, SSH key/password, and timezone.

The raw `.img` can also be written with `dd`/`rpi-imager` directly, and flashing
works over USB via `rpiboot`.

## First boot (FAT32 bootfs seed)

After flashing, mount the SD card's FAT32 partition and optionally create
`/boot/firmware/lapis-seed/` with any of:

| File | Purpose |
| --- | --- |
| `lapis.toml` | LLM provider/model/schedules (written to `/etc/lapis/lapis.toml`) |
| `hosts.toml` | Tailscale target hosts (written to `/etc/lapis/hosts.toml`) |
| `secrets.env` | `ANTHROPIC_API_KEY=…`, `LAPIS_VNC_*=…` → `/etc/lapis/secrets.env` (`0600`) |
| `tailscale.authkey` | headless `tailscale up --authkey=… --ssh` join |
| `lapis_ed25519` | optional SSH private key for target hosts (`0600`) |

On first boot `lapis-firstboot.service` copies these into `/etc/lapis/`,
`chmod 600`s the secrets/key, and **shreds** the originals from the FAT32
partition so plaintext secrets never remain on `/boot/firmware/`.

If Wi-Fi/Tailscale is not configured, `lapis-ui` shows View 6 (Onboarding) with
live Wi-Fi status, local IP, and the `tailscale up` login URL/short code.

See also `docs/setup_target_hint.md` for binding a target host's VNC/SSH
**strictly** to its Tailscale IP.
