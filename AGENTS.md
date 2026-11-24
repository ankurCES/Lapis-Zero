# AGENTS.md — Lapis-Zero OS Coding Agent Guide

**Source of Truth:** Always read `SPEC.md` before planning or writing code. Lapis-Zero is a **standalone flashable OS image** (`lapis-zero-arm64.img.xz`) for the **M5Stack CardputerZero (Raspberry Pi CM0)** compatible with **M5 Imager** (`CardputerZero/m5stack-imager`), **not** an app installed on top of the stock M5Stack desktop image.

## 1. Repository Pillars

* `ui/` — **Rust (`lapis-ui`)**: Direct $320 \times 170$ software renderer, damped spring physics, pixel mascot, 6 views (including First-Boot Onboarding), SDL2 desktop simulator (`--sim`), headless PNG dumper (`--dump-frame`), and CM0 `/dev/fb0` + `evdev` backends.
* `daemon/` — **Python 3.12+ (`lapis-daemon`, managed via `uv`)**: `asyncio` agent runtime, SQLite WAL store, policy gate, cron scheduler, system/Tailscale status monitor, voice PTT pipeline, and Option C KVM drivers (Tailscale VNC + SSH).
* `os/` — **Custom OS Image Pipeline (`pi-gen`)**: Builds `lapis-zero-arm64.img.xz` from `CardputerZero/pi-gen` (`cardputerzero_v1.1`, `stage0`--`stage2` BSP only) + our custom `stage-lapis`, plus `os/generate-imager-manifest.py` for M5 Imager `os-list.json` output.
* **IPC:** Local Unix Domain Socket (`/run/lapis/lapisd.sock`) carrying strict **NDJSON** (one UTF-8 JSON object per `\n`).

## 2. Non-Negotiable Engineering Guardrails

1. **OS Image Discipline (No Desktop Bloat):** Never include `stage3`--`stage5` from `CardputerZero/pi-gen`, X11, Wayland, `lightdm`, `wayvnc`, `APPLaunch.service`, PulseAudio/PipeWire, Electron, OpenCV, or PyTorch. Keep the image headless (`multi-user.target`) with `gpu_mem=16` and $512\text{ MB}$ LZ4 `zram`.
2. **Preserve CardputerZero Hardware BSP:** Never break or remove CardputerZero device-tree overlays (`st7789`, `tca8418`, `es8389`, `bmi270`, `bq27220`, `rx8130ce`), `/usr/share/keymaps/tca8418_keypad_m5stack_keymap.map`, or the `cardputerzero,st7789v_lcd.bin` initramfs hook.
3. **512 MB RAM Budget:** Target $\le 15\text{ MB}$ RSS for `lapis-ui`, $\le 130\text{ MB}$ peak RSS for `lapis-daemon`, and $< 220\text{ MB}$ combined total system RSS. Explicitly `del` raw full-resolution VNC/Pillow buffers immediately after downscaling to $\le 1280 \times 720$ JPEG.
4. **Strict Process & I/O Isolation:** `lapis-ui` must never block its render loop on socket I/O, disk I/O, or daemon stalls. Handle socket disconnects cleanly with exponential backoff reconnect ($200\text{ ms} \to 2000\text{ ms}$).
5. **Power & SD-Card Longevity:** Respect the adaptive frame governor ($60\text{ FPS}$ animating $\to 12\text{ FPS}$ active agent $\to 1\text{ FPS}$ idle). Zero busy-wait loops. Keep `noatime`, volatile `journald`, `/tmp` on `tmpfs`, and SQLite WAL mode.
6. **Secret & First-Boot Security:** Never commit API keys, VNC passwords, SSH keys, or Tailscale auth keys. Any secrets imported from `/boot/firmware/lapis-seed/` by `lapis-firstboot.sh` MUST be `chmod 600` inside `/etc/lapis/` and immediately deleted from the FAT32 boot partition.
7. **KVM Safety:** Always enforce `policy.py` gates, `dHash` stuck-loop detection, and explicit VNC modifier key-up cleanup on abort or `kill_all`.

## 3. Build, Test & Verification Commands

### Rust UI (`ui/`)
```bash
# Unit tests (canvas, Bayer dither, fonts, springs, IPC parsing)
cargo test --manifest-path ui/Cargo.toml

# Lint & format check
cargo fmt --manifest-path ui/Cargo.toml --check
cargo clippy --manifest-path ui/Cargo.toml --all-targets -- -D warnings

# Interactive desktop simulator (3x scale, 960x510)
cargo run --manifest-path ui/Cargo.toml -- --sim --demo

# Headless 320x170 PNG snapshot for visual inspection
cargo run --manifest-path ui/Cargo.toml -- --dump-frame /tmp/companion.png --view companion
```

### Python Daemon (`daemon/`)
```bash
# Sync dependencies & run tests
cd daemon && uv sync && uv run pytest

# Lint & format
cd daemon && uv run ruff check . && uv run ruff format --check .

# Run daemon in local mock KVM mode
cd daemon && uv run lapis-daemon --mock
```

### OS Image & M5 Imager Manifest (`os/`)
```bash
# Lint all shell scripts in the pi-gen stage and first-boot hooks
shellcheck os/build-image.sh os/files/lapis-firstboot.sh os/stage-lapis/**/0*-run*.sh

# Validate M5 Imager manifest generator
python3 os/generate-imager-manifest.py --self-test

# Build flashable lapis-zero-arm64.img.xz (Linux/Docker host with binfmt_misc)
make build-os
```

## 4. Phased Execution Workflow

* **One Phase at a Time:** Work strictly within the active phase in `SPEC.md` Section 8. Do not jump ahead or speculatively implement later phases unless explicitly asked.
* **Desktop-First Portability:** Keep CM0-specific code (`fbdev.rs`, `hw_cm0.rs`, ALSA/I2C/sysfs) behind traits and `#[cfg(target_os = "linux")]` / feature gates so `ui/` and `daemon/` build and test cleanly on macOS and Linux desktops.
* **Visual Verification Gate (UI Changes):** After any layout, font, mascot, or view change, generate `--dump-frame` PNGs at $320 \times 170$ for every affected view (including `setup` and `approval`) and inspect them to confirm zero text clipping, overlap, or misaligned crosshairs.
* **IPC Contract Lockstep:** Any change to NDJSON message types or fields must update `ui/src/ipc.rs`, `daemon/src/lapis_daemon/ipc_server.py`, and `SPEC.md` Section 6 together, with round-trip serialization tests on both sides.
* **Declarative OS Packaging:** Never assume manual SSH tweaks on a live device. Every OS setting, package, `systemd` unit, udev rule, and `config.txt` adjustment must live declaratively inside `os/stage-lapis/`.

## 5. Code Conventions

* **Rust (`ui/`):** Keep per-frame rendering deterministic and allocation-free; use integer pixel coordinates for crisp bitmap typography; keep `DisplayBackend` and `InputBackend` traits minimal so desktop simulation, headless `--dump-frame`, and CM0 `/dev/fb0` share 100% of view and spring logic.
* **Python (`daemon/`):** Full type annotations; Pydantic v2 for config and IPC validation; non-blocking `asyncio` for all I/O (offload CPU-bound Pillow downscaling/dithering via `asyncio.to_thread`); SQLite opened with `WAL` + `NORMAL` + `busy_timeout=5000`; keep `HostDriver` abstract so `MockHostDriver` covers all tests.
* **Shell / `pi-gen` (`os/`):** Use `set -euo pipefail`, pass `shellcheck`, keep stage scripts idempotent, and ensure `os/generate-imager-manifest.py` emits exact `extract_size`, `extract_sha256`, `image_download_size`, `image_download_sha256`, and `init_format: "systemd"` matching `CardputerZero/m5stack-imager`.

## 6. Definition of Done (Per Task / Phase)

Before concluding a phase or handing control back:
1. All relevant `cargo test`, `cargo clippy`, `pytest`, `ruff`, and `shellcheck` checks pass.
2. Modified UI views have been rendered via `--dump-frame` and visually verified at $320 \times 170$.
3. Every acceptance criterion for the active phase in `SPEC.md` Section 8 is verified and concisely reported.