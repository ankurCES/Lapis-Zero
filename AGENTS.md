# AGENTS.md — Lapis-Zero OS & Android Companion Coding Agent Guide

**Sources of Truth:** Always read both `SPEC.md` (core OS image, Rust UI, Option C KVM daemon, M5 Imager pipeline) and `ANDROID_COMPANION_SPEC.md` (Tailscale Serve control plane, BLE/QR pairing, native Android app) before planning or writing code.

Lapis-Zero is a **standalone flashable OS image** (`lapis-zero-arm64.img.xz`) for the **M5Stack CardputerZero (Raspberry Pi CM0)** compatible with **M5 Imager** (`CardputerZero/m5stack-imager`), paired with a native **Android Companion App** (`android/`) that provides full multi-chat agent control, live KVM observation, and remote device configuration over Tailscale.

---

## 1. Repository Pillars

* `ui/` — **Rust (`lapis-ui`)**: Direct $320 \times 170$ software renderer, damped spring physics, pixel mascot, 7 views (`companion`, `ticker`, `menu`, `approval`, `prompt`, `setup`, `pairing` with its `pairing_pin` / `pairing_qr` modes), SDL2 desktop simulator (`--sim`), headless PNG dumper (`--dump-frame`), and CM0 `/dev/fb0` + `evdev` backends.
* `daemon/` — **Python 3.12+ (`lapis-daemon`, managed via `uv`)**: `asyncio` agent runtime, multi-turn chat manager, SQLite WAL store, policy gate, cron scheduler, voice PTT pipeline, Option C KVM drivers (Tailscale VNC + SSH), in-process `aiohttp` REST/WebSocket control plane (`127.0.0.1:8421`), and on-demand BlueZ D-Bus BLE GATT provisioning (`ble_prov/`, run as the separate `lapis-ble-prov` service via `bluez-peripheral`).
* `os/` — **Custom OS Image Pipeline (`pi-gen`)**: Builds `lapis-zero-arm64.img.xz` from `CardputerZero/pi-gen` (`cardputerzero_v1.1`, `stage0`--`stage2` BSP only) + custom `stage-lapis` (including `bluez`, `network-manager`, `tailscaled`, and on-demand `lapis-ble-prov.service`), plus `os/generate-imager-manifest.py` for M5 Imager `os-list.json` output.
* `android/` — **Native Android App (Kotlin 2.2+ / Jetpack Compose BOM `2026.11.01`)**: Material 3 Ink/Paper theme, Room local cache, Keystore token vault, OkHttp REST + reconnecting WebSocket client, Nordic BLE KTX provisioning, CameraX QR pairing, live KVM viewer, remote configuration cockpit, and actionable approval notifications.
* **Dual Transport Architecture:**
  * **On-Device IPC:** `/run/lapis/lapisd.sock` (Unix Domain Socket, strict NDJSON).
  * **Remote Control Plane:** `127.0.0.1:8421` exposed exclusively via `tailscale serve --bg --https=443 http://127.0.0.1:8421` (`https://<node>.<tailnet>.ts.net/v1/*` and `wss://<node>.<tailnet>.ts.net/v1/ws`).

---

## 2. Non-Negotiable Engineering Guardrails

1. **OS Image Discipline (No Desktop Bloat):** Never include `stage3`--`stage5` from `CardputerZero/pi-gen`, X11, Wayland, `lightdm`, `wayvnc`, `APPLaunch.service`, PulseAudio/PipeWire, Electron, OpenCV, or PyTorch. Keep the image headless (`multi-user.target`) with `gpu_mem=16` and $512\text{ MB}$ LZ4 `zram`.
2. **Preserve CardputerZero Hardware BSP:** Never break or remove CardputerZero device-tree overlays (`st7789`, `tca8418`, `es8389`, `bmi270`, `bq27220`, `rx8130ce`), `/usr/share/keymaps/tca8418_keypad_m5stack_keymap.map`, or the `cardputerzero,st7789v_lcd.bin` initramfs hook.
3. **512 MB CM0 RAM & Disk Budget:**
   * Target $\le 15\text{ MB}$ RSS for `lapis-ui`, $\le 90\text{ MB}$ steady / $\le 130\text{ MB}$ peak RSS for `lapis-daemon` (including $\le 12\text{ MB}$ overhead for in-process `aiohttp` + WS hub — never spawn a second Python interpreter or web server), and $< 220\text{ MB}$ combined total system RSS.
   * Explicitly `del` raw full-resolution VNC/Pillow buffers immediately after downscaling to $\le 1280 \times 720$ JPEG.
   * Enforce the **$25\text{ MB}$ LRU disk cap** on `/var/lib/lapis/frames/` for Android screenshot retrieval.
   * Keep `lapis-ble-prov.service` **disabled at idle** and active only during unpaired first boot or an explicit 5-minute pairing window; tear it down immediately once provisioning succeeds.
4. **Tailscale Serve & Network Security Rules:**
   * `lapis-daemon`'s HTTP/WebSocket server **MUST bind strictly to `127.0.0.1:8421`** — NEVER `0.0.0.0` or a LAN interface (the sole exception is an explicit `--dev-bind` flag used strictly in local `--mock` development, never in production or `os/` units).
   * Expose the API remotely **only** via `tailscale serve --bg --https=443 http://127.0.0.1:8421`. **NEVER** run `tailscale funnel` or open public firewall ports.
   * Every REST endpoint and WebSocket upgrade (except `POST /v1/pair/exchange`) **MUST** require `Authorization: Bearer <api_token>` validated against SHA-256 token hashes in SQLite `paired_devices`. Reject missing, invalid, or revoked tokens with `401 Unauthorized`.
   * QR pairing nonces MUST be cryptographically random ($32\text{ bytes}$), single-use, and expire after $180\text{ s}$.
   * **Write-Only Secrets:** `/etc/lapis/secrets.env` MUST stay `chmod 600`. `GET /v1/secrets/status` may only return boolean presence (`is_set`) and masked trailing characters (e.g., `...a9f2`) — NEVER return plaintext API keys or VNC passwords.
   * Any secrets seeded via `/boot/firmware/lapis-seed/` MUST be moved to `/etc/lapis/` (`chmod 600`) and immediately deleted from the FAT32 boot partition by `lapis-firstboot.sh`.
   * On Android, store `api_token` exclusively in Android Keystore-backed encrypted storage — never in plaintext `SharedPreferences`, Room, or logcat.
5. **BLE Provisioning Crypto & Physical Verification:**
   * BLE GATT command/event payloads MUST be encrypted with **X25519 ECDH + HKDF-SHA256 (bound to the 6-digit PIN shown on the CardputerZero LCD) + AES-256-GCM** with chunk sequence framing. Never transmit Wi-Fi PSKs, Tailscale auth keys, API keys, or bearer tokens in plaintext over BLE.
6. **Strict Process Isolation & Bidirectional Sync:**
   * `lapis-ui` must never block its render loop on socket I/O, disk I/O, or daemon stalls; reconnect with exponential backoff ($200\text{ ms} \to 2000\text{ ms}$).
   * `lapis-daemon` is the single canonical state owner. Its event bus MUST fan out state transitions, chat updates, tool calls, approvals, and kill-switch events to both `/run/lapis/lapisd.sock` and all authenticated `/v1/ws` Android clients simultaneously.
7. **Power, SD-Card Longevity & KVM Safety:**
   * Respect the adaptive frame governor ($60\text{ FPS}$ animating $\to 12\text{ FPS}$ active agent $\to 1\text{ FPS}$ idle) with zero busy-wait loops. Keep `noatime`, volatile `journald`, `/tmp` on `tmpfs`, and SQLite WAL mode.
   * Always enforce `policy.py` gates, `dHash` stuck-loop detection, explicit VNC modifier key-up cleanup on abort or `kill_all`, and restrict manual `/v1/hosts/{id}/nudge` actions to paused/assist states.

---

## 3. Build, Test & Verification Commands

### Rust UI (`ui/`)

```bash
# Unit tests (canvas, Bayer dither, fonts, springs, QR/PIN layout, IPC parsing)
cargo test --manifest-path ui/Cargo.toml

# Lint & format check
cargo fmt --manifest-path ui/Cargo.toml --check
cargo clippy --manifest-path ui/Cargo.toml --all-targets -- -D warnings

# Interactive desktop simulator (3x scale, 960x510)
cargo run --manifest-path ui/Cargo.toml -- --sim --demo

# Headless 320x170 PNG snapshots for visual inspection (all views)
cargo run --manifest-path ui/Cargo.toml -- --dump-frame /tmp/companion.png --view companion
cargo run --manifest-path ui/Cargo.toml -- --dump-frame /tmp/pairing_pin.png --view pairing_pin
cargo run --manifest-path ui/Cargo.toml -- --dump-frame /tmp/pairing_qr.png --view pairing_qr
```

### Python Daemon (`daemon/`)

```bash
# Sync dependencies & run full test suite (IPC, KVM, REST/WS API, auth, BLE crypto)
cd daemon && uv sync && uv run pytest

# Lint & format
cd daemon && uv run ruff check . && uv run ruff format --check .

# Run daemon in local mock KVM mode with dev pairing token for Android Emulator (10.0.2.2:8421)
cd daemon && uv run lapis-daemon --mock --dev-pair-token dev-local-token   # or: make mock
```

### Android Companion App (`android/`)

```bash
# JVM unit tests (DTO serialization, repositories, ViewModels, BLE crypto vectors)
cd android && ./gradlew testDebugUnitTest

# Android Lint static analysis
cd android && ./gradlew lintDebug

# Build debug APK
cd android && ./gradlew assembleDebug

# Instrumented tests (when Android Emulator or physical device is connected)
cd android && ./gradlew connectedDebugAndroidTest
```

### Cross-Language BLE Crypto Verification (Python <-> Kotlin)

Whenever touching `daemon/src/lapis_daemon/ble_prov/crypto.py` or `android/.../data/ble/BleEnvelopeCrypto.kt`, maintain a shared deterministic vector suite at `tests/vectors/ble_crypto_vectors.json` covering:
1. Known X25519 keypairs + shared secret derivation.
2. HKDF-SHA256 session key derivation bound to a 6-digit PIN + salt/info label.
3. AES-256-GCM encryption/decryption with fixed IV, AAD, ciphertext, and auth tag.
4. Multi-chunk GATT packet splitting and reassembly across MTU boundaries (e.g., MTU $185$).

Verify both implementations against the exact same JSON vectors:

```bash
# Python side
cd daemon && uv run pytest tests/test_ble_crypto_vectors.py

# Android / Kotlin side
cd android && ./gradlew testDebugUnitTest --tests "*.BleEnvelopeCryptoVectorTest"
```

### OS Image & M5 Imager Manifest (`os/`)

```bash
# Lint all shell scripts in the pi-gen stage and first-boot hooks
make -C os lint          # shellcheck over every os/ script + manifest generator self-test

# Build flashable lapis-zero-arm64.img.xz (Linux/Docker host with binfmt_misc)
make build-os
```

---

## 4. Phased Execution Workflow

* **One Phase at a Time:** Follow `SPEC.md` Section 8 (**Phases 0--6**) and `ANDROID_COMPANION_SPEC.md` Section 7 (**Phases A0--A5**) strictly one phase at a time. Never jump ahead or speculatively implement later phases unless explicitly instructed. Keep `PROGRESS.md` updated at the end of every phase.
* **Desktop- & Emulator-First Portability:** Keep CM0-specific code (`fbdev.rs`, `hw_cm0.rs`, ALSA/I2C/sysfs, BlueZ D-Bus, `nmcli`) behind clean traits/protocols and platform gates so `ui/`, `daemon/`, and `android/` build and test cleanly on macOS/Linux desktops and Android Emulators.
* **Visual Verification Gate (UI Changes):** After any layout, font, mascot, QR, or view change in `ui/`, generate `--dump-frame` PNGs at $320 \times 170$ for every affected view (including `setup`, `approval`, `pairing_pin`, and `pairing_qr`) and inspect them to confirm zero text clipping, overlap, misaligned crosshairs, or un-scannable QR quiet zones.
* **Contract Lockstep:**
  * Local NDJSON IPC changes MUST update `ui/src/ipc.rs`, `daemon/src/lapis_daemon/ipc_server.py`, and `SPEC.md` Section 6 together with round-trip tests.
  * Remote REST/WS changes MUST update `daemon/src/lapis_daemon/api/`, `android/.../data/remote/ProtocolModels.kt`, and `ANDROID_COMPANION_SPEC.md` Section 5 together with serialization tests on both sides.
  * BLE protocol/crypto changes MUST update `daemon/src/lapis_daemon/ble_prov/`, `android/.../data/ble/`, and `tests/vectors/ble_crypto_vectors.json` together.
* **Declarative OS Packaging:** Never rely on manual SSH tweaks on a live device. Every OS package, `systemd` unit (`lapis-daemon`, `lapis-ui`, `lapis-firstboot`, `lapis-powerguard`, `lapis-ble-prov`), udev rule, `tailscale serve` bootstrap, and `config.txt` adjustment MUST live declaratively inside `os/stage-lapis/`.

---

## 5. Code Conventions

* **Rust (`ui/`):** Keep per-frame rendering deterministic and allocation-free; use integer pixel coordinates for crisp bitmap typography and QR modules (minimum 2px quiet zone); keep `DisplayBackend` and `InputBackend` traits minimal so `--sim`, `--dump-frame`, and `/dev/fb0` share 100% of view and spring logic.
* **Python (`daemon/`):** Full type annotations; Pydantic v2 for config, IPC, and REST/WS validation; non-blocking `asyncio` for all I/O (offload Pillow resizing/dithering and crypto/hashing via `asyncio.to_thread` when heavy); SQLite opened with `WAL` + `NORMAL` + `busy_timeout=5000`; keep `HostDriver` and BLE/network controllers abstract for mock testing.
* **Kotlin / Android (`android/`):** Idiomatic Kotlin Coroutines + `StateFlow`/`SharedFlow`; unidirectional data flow in Compose screens; `kotlinx.serialization` with explicit `@SerialName` matching daemon snake_case JSON; honor the monochrome Ink/Paper + LapisBlue/Amber design system (`LapisTheme.kt`); handle Android 12+ BLE (`BLUETOOTH_SCAN`, `BLUETOOTH_CONNECT`) and notification (`POST_NOTIFICATIONS`) runtime permissions cleanly.
* **Shell / `pi-gen` (`os/`):** Use `set -euo pipefail`, pass `shellcheck`, keep stage scripts idempotent, and ensure `os/generate-imager-manifest.py` emits exact `extract_size`, `extract_sha256`, `image_download_size`, `image_download_sha256`, and `init_format: "systemd"` matching `CardputerZero/m5stack-imager`.

---

## 6. Definition of Done (Per Task / Phase)

Before concluding a phase or handing control back:

1. All relevant checks pass (`cargo test`, `cargo clippy`, `uv run pytest`, `ruff`, `./gradlew testDebugUnitTest lintDebug assembleDebug`, `shellcheck`, and cross-language BLE crypto vector tests when applicable).
2. Security invariants are verified: API bound to `127.0.0.1:8421`, unauthorized requests return `401`, secrets never leak in API responses or logs, and BLE provisioning stops cleanly when finished.
3. Modified `lapis-ui` views have been rendered via `--dump-frame` and visually verified at $320 \times 170$.
4. Every acceptance criterion for the active phase in `SPEC.md` Section 8 or `ANDROID_COMPANION_SPEC.md` Section 7 is verified, checked off in `PROGRESS.md`, and concisely reported.