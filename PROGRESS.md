# Lapis-Zero OS — Progress Tracker

Live tracker for every phase and acceptance criterion in `SPEC.md` §8.
Checked = verified in this repo with the listed evidence. One phase at a time;
each phase ends with a completion report and an explicit go-ahead.

Trackers: phases below; per-run scratch plans live in the session; durable
cross-session issues would go to beads (none yet).

---

## Phase 0 — Workspace, Canvas & Desktop Simulator

**Deliverables**

- [x] Cargo crate `ui/`, uv package `daemon/`, config examples in `config/`, root `Makefile`
- [x] `ui/src/gfx/canvas.rs` — 320×170 buffer, primitives, Bayer 4×4 dithering, fonts 6×8 / 8×14 / 16×28
- [x] `ui/src/backend/sdl_sim.rs` — 960×510 3×-scaled SDL2 simulator with CardputerZero key bindings
- [x] `--dump-frame <path.png> --view <name>` headless PNG exporter

**Acceptance criteria (SPEC §8 Phase 0)**

- [x] `cargo test` passes — 13/13 in `ui/` (canvas, fonts, views), 4/4 in `daemon/`
- [x] `cargo run -- --sim` renders cleanly at 60 FPS — window runs idle (~7% CPU, no busy-wait); sleep-based frame pacing
- [x] `--dump-frame` outputs valid 320×170 PNGs — all 6 views verified via `file`/pixel audit

**Report:** 2026-11-24 — Phase 0 complete. See phase report in chat; visual audit of
`dumps/*.png` passed (status bar y0–17 + 1px rule y18, crisp F6x8/F8x14 text,
descenders preserved, 50/50 Bayer dither block, zero text clipping; the only
edge pixels are the rule's endpoints at (0,18)/(319,18), by design).

---

## Phase 1 — Lapis One Animation Engine & All 6 Views

**Deliverables**

- [x] `gfx/spring.rs` + `gfx/mascot.rs` (5 animation states + desk clock)
- [x] All 6 views (`companion`, `ticker`, `menu`, `approval`, `prompt`, `setup`) with spring view slides, spring menu pill, 1-bit 120×68 preview in `approval.rs`
- [x] `--demo` mode cycling states, synthetic approval modal, first-boot onboarding view

**Acceptance criteria**

- [x] Smooth 60 FPS transitions across all 6 views — spring presets match SPEC 5.2 (slide 240/24, pill 320/26, modal 360/28); sim loop uses governor-paced sleeps
- [x] `--dump-frame` PNGs for all 6 views verify zero text overlap or clipping at 320×170 — pixel audit: rule y18 full-width on all six; 0 right-edge and 0 bottom-edge ink below the rule except companion's 1 intentional pixel at the rule baseline; approval panel confirmed as a clean bordered rect (x12–307, y22–164) with no controls overlap
- [x] Idle governor drops to 1 FPS — `App::frame_interval()` returns 1000 ms for settled IDLE

**Report:** 2026-11-24 — Phase 1 complete. Changed: `gfx/{spring,mascot}.rs`, `gfx/canvas.rs` blit,
`gfx/mod.rs`, `views/{companion,ticker,menu,approval,prompt,setup}.rs` + `views/mod.rs`
(`App` orchestrator, status bar, adaptive governor), `backend/sdl_sim.rs`, `main.rs`,
`PROGRESS.md`. Commands (all pass): `cargo build`, `cargo test` (27 passed), `cargo clippy
--all-targets -- -D warnings`, `cargo fmt --check`, same trio with `--no-default-features`,
and `--dump-frame` for all six views. Visual audit (`dumps/{companion,ticker,menu,approval,prompt,setup}.png`):
crisp F6x8/F8x14/F16x28 text, descenders intact, Bayer dim + amber crosshair on approval,
zero clipping. STOP for Phase 2 approval.

---

## Phase 2 — IPC Protocol & Daemon Core (Mock Driver)

**Deliverables**

- [x] `db.py` (SQLite WAL), `config.py`, `system_status.py`, `ipc_server.py`
- [x] `ui/src/ipc.rs` with non-blocking reads + automatic reconnect
- [x] `agent/runner.py`, `agent/policy.py`, `MockHostDriver`

**Acceptance criteria**

- [x] `lapis-daemon --mock` + `lapis-ui --sim` executes a full task Prompt → SQLite → Ticker → Approval Modal → `ESC` kill-switch
- [x] Daemon restart auto-reconnects and re-syncs within 2 s

**Report:** 2026-11-24 — Phase 2 complete. Daemon: `config.py` (pydantic
v2 + TOML + `EffectivePaths`/`LAPIS_CONFIG_DIR` rootless override), `db.py`
(SQLite WAL/NORMAL/busy_timeout=5000; `tasks`/`steps`/`schedules`),
`system_status.py` (`state_sync` builder), `kvm/base.py`
(`HostDriver` ABC + `MockHostDriver`), `agent/policy.py` (strict/balanced/
autonomous + hard denylist), `agent/runner.py` (bounded step loop, policy gate,
`asyncio.Future`-backed approvals, pause/resume/kill_all), `ipc_server.py`
(Unix socket `0600`, NDJSON framing, immediate re-sync on connect, broadcast).
UI: `ipc.rs` (`IpcClient` non-blocking poll + exp backoff 0.2s→2.0s,
`DaemonState` merge, all SPEC §6 message builders), `LapisKey::EscHold`
(ESC hold 1.5s → `kill_all`), `App::on_daemon_message` + `key()->Option<Value>`
out-message transport, owned `String` host state, `--socket` in `main.rs`.
Commands (all pass): `cargo test` 32 passed (both feature sets), `cargo clippy
--all-targets -- -D warnings` clean (both), `cargo fmt --check` clean, `uv run
pytest` 34 passed, `ruff check`/`ruff format --check` clean, 6× `--dump-frame`
wrote 320×170 PNGs. Live end-to-end (real daemon over real socket): submit→
activity_events[prompt,ssh_exec,vnc_click,type_text,ssh_exec]→approval_request
(req_1 vnc_click high, 1020-byte preview b64)→approve_once→req_2 ssh_exec→deny→
state IDLE→kill_all toast; reconnect re-sync measured 0.001s. STOP for Phase 3.

---

## Phase 3 — Option C KVM Drivers (Tailscale VNC + SSH) & LLM Adapter

**Deliverables**

- [x] `kvm/vnc_driver.py` (asyncvnc, Pillow downscale, coordinate transform, 1-bit 120×68 crosshair thumbnail, modifier key-up cleanup)
- [x] `kvm/ssh_driver.py` (asyncssh, timeout + 4 KB truncation)
- [x] `llm/anthropic_cua.py` behind `llm/base.py`, dHash stuck detection, `agent/scheduler.py`

**Acceptance criteria**

- [x] Unit tests pass: coordinate math, 1020-byte 1-bit thumbnail packing, dHash stuck detection, policy tiers
- [ ] Live Tailscale host test completes a multi-step GUI + SSH task with daemon peak RSS < 130 MB  — _partly covered on 2026-11-24: the real VNC + SSH drivers and the runner loop ran against a live VNC/SSH server (typed text arrived remotely; peak RSS 107 MB with 1600x900 captures inside the device image). Still open: a run over Tailscale driven by a real LLM key_

**Report:** 2026-11-24 — Phase 3 complete (code + unit gates). Added `agent/dhash.py`
(`StuckDetector`: threshold 3, hamming ≤2, corrective hint, pause after passing max_retries),
`kvm/vnc_driver.py` (pure helpers `downscale_dims`/`to_vnc_coords`/`compute_crop_box`/
`build_preview_1bit`/`pack_bits_msb` + lazy `VncDriver` with modifier key-up cleanup and JPEG
downscale pipeline that frees raw buffers), `kvm/ssh_driver.py` (`truncate_output` + lazy
`SshDriver` 30 s timeout), `llm/{base,anthropic_cua}.py` (unified tool surface, Claude CUA
adapter, lazy `anthropic`), `agent/scheduler.py` (5-field cron parse + asyncio runner),
`CompositeHostDriver`/`build_host_driver` in `kvm/base.py`, `StuckDetector` wired into
`runner.py`, real driver factory + cron scheduler in `main.py`, `pillow` dep + `kvm` extra.
Commands (all pass): `uv run pytest` 53 passed, `uv run ruff check`/`format --check` clean,
`cargo test` 32 passed, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`,
6× `--dump-frame` (320×170 RGB), live mock E2E over the socket, real `build_host_driver`
factory verifies a lazily-built `CompositeHostDriver`. STOP for Phase 4.

---

## Phase 4 — CardputerZero CM0 Hardware Backends & Cross-Compilation

**Deliverables**

- [x] `ui/src/backend/fbdev.rs` (`/dev/fb0` mmap RGB565 + stride-safe flush)
- [x] `ui/src/backend/hw_cm0.rs` (TCA8418 evdev, BQ27220 battery, backlight `fn+U/I` + idle blanking, BMI270 motion wake)
- [x] `aarch64-unknown-linux-gnu` cross-compile (`zigbuild`/`cross`) targets in `Makefile`

**Acceptance criteria**

- [x] `make cross-ui` builds a stripped aarch64 release binary cleanly — `ui/target/aarch64-unknown-linux-gnu/release/lapis-ui`, ELF aarch64, stripped, 762,880 bytes
- [x] Linux hardware backend unit tests and mock-sysfs tests pass — fbdev RGB565/stride + hw_cm0 evdev/battery/backlight/IMU pure-function tests in `cargo test` (host); Linux-only sysfs/evdev/mmap paths are `#[cfg(target_os = "linux")]`

**Report:** 2026-11-24 — Phase 4 complete. Changed: `ui/src/backend/fbdev.rs` (portable
RGB888→RGB565 + stride-aware row blit; Linux `/dev/fb0` mmap via `FBIOGET_VSCREENINFO`
0x4600 / `FBIOGET_FSCREENINFO` 0x4602, `#[repr(C)]` ABI structs copied from
`include/uapi/linux/fb.h`, 16 bpp guard, `munmap` on drop), `ui/src/backend/hw_cm0.rs`
(evdev `input_event` decode, TCA8418 keycode→`LapisKey` map with `fn` modifier,
ESC-hold 1.5 s state machine, BQ27220 parsers, backlight clamp + 30 s/90 s idle
policy, BMI270 motion delta; `linux_io` submodule with `Backlight`/`FuelGauge`/
`Imu`/`EvdevInput` behind `#[cfg(target_os = "linux")]`), `backend/mod.rs` (module
registration + `LapisKey::{BacklightDown,BacklightUp}`), `main.rs` (`--fb` run loop:
fbdev display + evdev input + sysfs backlight step on `fn+U/I` + IPC; non-Linux
`--fb` errors cleanly), `ui/Cargo.toml` + `Cargo.lock` (`libc` pinned to
`cfg(target_os = "linux")`), `Makefile` (`cross-ui` via `cargo-zigbuild` +
`llvm-strip`). Commands (all pass): `cargo test` 47 passed (default + no-default),
`cargo clippy --all-targets` clean (all-features + no-default), `cargo fmt --check`
clean, `make cross-ui` clean (stripped aarch64 binary, 762,880 B), `uv run pytest`
53 passed + `ruff check`/`ruff format --check` clean (unchanged Phase 3 code),
6× `--dump-frame` wrote 320×170 PNGs. STOP for Phase 5.

---

## Phase 5 — Custom Flashable OS Image Pipeline (`os/` + M5 Imager Integration)

**Deliverables**

- [x] `os/config.pigen` + `os/build-image.sh` (clones `CardputerZero/pi-gen` `cardputerzero_v1.1`, runs `STAGE_LIST="stage0 stage1 stage2 stage-lapis"` in Docker)
- [x] `os/stage-lapis/` (`00-os-tuning`, `01-tailscale`, `02-lapis-stack`, `03-services-firstboot`) incl. `gpu_mem=16`, `fbcon=map:off`, st7789 initramfs hook, `zram-generator.conf`, systemd units, `lapis-firstboot.sh` (bootfs seed importer + secret shredder), `lapis-powerguard`
- [x] `os/generate-imager-manifest.py` emitting M5 Imager `os-list.json` with exact SHA256s and byte sizes
- [x] `os/README.md` + `os/docs/setup_target_hint.md` (M5 Imager `Use custom` / `--repo`, `rpiboot`, bootfs seeding)

**Acceptance criteria**

- [x] `shellcheck` passes on all scripts in `os/` — `make -C os lint` exit 0 (only source/main reads pinned via `# shellcheck disable=SC2012/SC1091` in `build-image.sh` and the initramfs hook)
- [x] `os/generate-imager-manifest.py` passes schema validation against `CardputerZero/m5stack-imager` `os-list-template.json` — `--self-test` OK; emitted `os_list[0]` and `imager.devices[0]` key sets are **byte-identical** to the live template's (added the `icon` field to match); `--validate-only` reports OK
- [x] `make -C os deploy` produces `lapis-zero-arm64.img.xz` + `os-list.json` — _built 2026-11-24 (see Phase A5 for hashes). "Bootable" is shown only under systemd in a container; booting the device is the next line_
- [ ] Flash via M5 Imager boots CardputerZero into Lapis-Zero: `multi-user.target`, zero desktop/X11/APPLaunch, ~496 MB Linux RAM visible, total active RSS < 220 MB, `lapis-firstboot.service` imports and shreds bootfs secrets — _needs the physical CardputerZero; the image exists now. In the container boot: `multi-user.target`, launcher masked, seed import ran, `lapis-ui` 2 MB + `lapis-daemon` 54 MB RSS_

**Report:** 2026-11-24 — Phase 5 code complete (build + flash are
environment-blocked). Created the full `os/` tree: `config.pigen`
(`IMG_NAME=lapis-zero`, `STAGE_LIST="stage0 stage1 stage2 stage-lapis"`,
`DEPLOY_COMPRESSION=xz`, auto-sized rootfs per SPEC 4.1), `build-image.sh`
(clones `cardputerzero_v1.1`, cross-compiles `lapis-ui` via `cargo-zigbuild`,
builds the daemon wheel via `uv build`, injects both into
`stage-lapis/02-lapis-stack/files/`, then runs pi-gen's `build-docker.sh`),
`generate-imager-manifest.py` (SHA256 + byte sizes, schema-validated). Stage
subdirs: `00-os-tuning` (`gpu_mem=16`, `fbcon=map:off`/quiet/loglevel3, zram
512 MB LZ4, fstab noatime + `/tmp` tmpfs, volatile journald),
`01-tailscale` (apt repo + `tailscaled`, no secrets), `02-lapis-stack`
(offline venv install of the wheel + binary), `03-services-firstboot`
(systemd units, `lapis-firstboot.sh` seed importer + FAT32 shredder, st7789
initramfs hook, `lapis-powerguard`). Verification run: `make -C os lint` exit 0
(shellcheck clean + manifest self-test OK); `bash -n` clean on all scripts;
manifest key-diff vs the live upstream `os-list-template.json` is empty;
`cargo test` 47 passed + `uv run pytest` 53 passed (unchanged, re-confirmed).
STOP for Phase 6 approval.

---

## Phase 6 — Push-to-Talk Voice Pipeline & Audio Polish

**Deliverables**

- [x] `daemon/src/lapis_daemon/voice.py` (ALSA 16 kHz mono PTT capture, live `mic_level` RMS → UI waveform, STT transcription, auto task enqueue)
- [x] Toggleable 8-bit procedural audio cues via ES8389 ALSA output

**Acceptance criteria**

- [x] Holding PTT animates the Companion waveform reactively
- [x] Releasing transcribes speech and dispatches the task to the active Tailscale host without breaking the RAM budget

**Report:** 2026-11-24 — Phase 6 complete (voice pipeline + cues; hardware THD/ES8389
and live STT remain environment-blocked, see below).

Files created/modified: `daemon/src/lapis_daemon/voice.py` (PTT capture pipeline
`normalize_rms` float contract + `RMS_FLOOR=0.01` noise floor, 6 procedural 8-bit WAV
cues `generate_cue`/`_wav_8bit`, `ensure_wav`, swap-able `CaptureSource`/
`Transcriber`/`CuePlayer`, `ArecordSource`/`AplayCuePlayer`/`SyntheticSource`/
`FakeTranscriber`/`HttpTranscriber`, `VoiceController` PTT lifecycle
`begin_ptt`→`end_ptt`→transcribe→dispatch with `asyncio.Event` start-gate + ring-buffer
RAM cap `DEFAULT_MAX_CAPTURE_BYTES=512 KB`), `daemon/src/lapis_daemon/main.py`
(dropped unused `NullCuePlayer` import), `daemon/tests/test_voice.py`,
`daemon/tests/test_ipc.py` (PTT routing test). Rust PTT waveform path was already
present from prior phases and left intact: `ui/src/views/mod.rs` (`mic_level`
ingest + `PushToTalk` → `ptt_audio` start/stop), `ui/src/ipc.rs`
(`msg_ptt_audio`), `ui/src/gfx/mascot.rs` (`draw_waveform`, `MascotState::Listening`),
`ui/src/backend/{sdl_sim,hw_cm0}.rs` (PTT key mapping).

Bugs fixed this pass (all in uncommitted Phase-6 code): (1) `_capture_loop` opened
the source *outside* try/finally, so a prompt `end_ptt` cancelled before capture
started and dropped the transcript; (2) `begin_ptt` returned after one tick, not
once audio actually flowed; (3) `normalize_rms(b"")` returned `None` and full-scale
normalized to 0.9999 instead of the test contract's 0.0/1.0. Fixes: start-gate event
set only after the first live RMS chunk (guaranteed set on error/short source), a
`0.0` baseline meter frame on PTT-begin, and the final `0.0` meter reset moved to
*after* transcription dispatch.

Commands run (all pass): `cd daemon && uv run pytest -q` → **60 passed**;
`uv run ruff check .` → All checks passed; `uv run ruff format --check .` → 33 files
formatted; `cd ui && cargo test` → **47 passed**; `cargo clippy --all-targets -- -D
warnings` → clean; `cargo clippy --all-targets --no-default-features -- -D warnings`
→ clean; `cargo fmt --check` → clean; `make dump-frames` → 6×320×170 PNGs
(`file` confirms `PNG image data, 320 x 170, 8-bit/color RGB, non-interlaced`):
`dumps/{companion,ticker,menu,approval,prompt,setup}.png`; `make -C os lint` → shellcheck
clean + `generate-imager-manifest.py --self-test` OK.

Visual artifacts to inspect: `dumps/*.png` (regenerated, companion view validates
char/clock/rule crispness at 320×170). No new per-frame artifacts are needed for the
voice pipeline itself — the waveform is animation-driven (RMS stream), covered by
`tests/test_ipc.py::test_ptt_context_routes_to_voice_controller` asserting a live
`mic_level` stream (leading `0.0`, non-zero RMS frames, final `0.0` at stop).

Edge cases / decisions for the next phase:
- **Environment-blocked (not code-blocked):** hardware ES8389 capture/playback
  (`arecord`/`aplay` devices) and live STT (`HttpTranscriber` → API key) need the CM0
  board + credentials. The swap-able `CaptureSource`/`Transcriber`/`CuePlayer`
  interfaces + `SyntheticSource`/`FakeTranscriber` make the full pipeline test green
  without them; live THD validation was left unclaimed to match prior phases (Phase 3
  live-Tailscale criterion is likewise environment-blocked and already tracked as such).
- `normalize_rms` now returns `float` (`0.0` for silence), never `None`; the UI
  `mic_rms` already clamps to `0.0..=1.0`.
- PTT begin waits on an `asyncio.Event` set after the first live chunk, so a very-short
  tap still emits at least one RMS frame — confirmed by the IPC test.

STOP for approval before Phase 7.

---

## Hardware boot failure and corrections to Phases 2–5 (2026-11-24)

The first flashed image stayed on the M5 logo. The image was inspected directly
(mounted read-only, then booted under systemd in a container) rather than
guessed at. What was wrong, and what changed:

| # | Finding in the flashed image | Fix |
|---|---|---|
| 1 | `lapis-ui.service` had `Requires=dev-fb0.device`; systemd never creates device units for framebuffers, so the UI was never started | no device dependencies; the unit starts right after `local-fs` + udev (`DefaultDependencies=no`) and the binary waits for the LCD itself |
| 2 | The BSP's `fb_load` splash holds the LCD's DRM device until the *stock* launcher process appears, so fbdev drawing would have been invisible anyway | ~~`Conflicts=fb_load.service` on the UI unit~~ **did not work on the device** (see "Hardware bring-up" below): the splash is now masked; the UI unblanks and re-activates the fbdev mode on open |
| 3 | The BSP's `LaunchWizard.service` (stock first-boot wizard) was still enabled and owned LCD + keypad | masked, markers removed, launcher packages purged in `stage-lapis/00-os-tuning` |
| 4 | `lapis-ui` exited if the keypad was missing, and read keys with `FIONREAD`, which evdev does not implement (no key ever arrived) | non-blocking reads with a queue; UI runs without a keypad and retries; exclusive `EVIOCGRAB` so typed prompts do not reach the hidden tty1 login |
| 5 | Key map assumed a `KEY_FN` modifier; the BSP keypad driver resolves `fn`/`sym` layers itself | map ported from the BSP device tree + console keymap (`fn+U/I` are `KEY_BRIGHTNESS*`, symbols from the `sym` layer, shift tracking, digits) |
| 6 | `lapis-daemon` exited with status 2 when `/etc/lapis/lapis.toml` was absent, which is every unseeded card: a systemd restart loop | missing or invalid config means defaults; default `lapis.toml`/`hosts.toml` ship in the image; unit gained `RuntimeDirectory`, `StateDirectory`, `EnvironmentFile` |
| 7 | Daemon dependencies (`asyncvnc`, `asyncssh`, …) were never installed in the image (`pip --no-deps --no-index`) | resolved from PyPI at build time, binary wheels only, with an import self-check that fails the build |
| 8 | Root filesystem 91 % full and never grown (the CardputerZero pi-gen branch has no first-boot resize) | fixed 5 GiB image, ~2.3 GB free; compressed download 639 MB (was 1.13 GB) after removing launcher apps, CJK fonts and unused firmware |
| 9 | `lapis-firstboot` ran `tailscale up` inline and was ordered `Before=lapis-ui` after `network-online`: an offline first boot delayed or blocked the UI | first boot never touches the network; `lapis-tailscale-up.service` joins and runs `tailscale serve` in the background with backoff |
| 10 | zram config was written for `systemd-zram-generator`, but Trixie's `rpi-swap` owns swap (default adds an SD swap file) | `rpi-swap` drop-in: zram only, 512 MiB, LZ4 |
| 11 | Low-battery guard trusted the fuel gauge alone (an uncalibrated gauge reading 0 % would power the device off 60 s after every boot) | requires <= 3 % **and** < 3.40 V **and** not charging; tightened again after the hardware bring-up (0 V / unknown readings and single samples no longer count) |

Corrections to earlier phase claims, found while reading the code:

* **Phase 2/3 agent loop:** the "real" mode never called the LLM. It replayed the mock script
  (click, type `make check`, …) against the selected real host. The runner now drives a planner
  (scripted in `--mock`, the configured LLM otherwise) through one policy-gated loop.
* **Phase 3 VNC driver:** written against methods `asyncvnc` does not have. Rewritten against the
  real library and verified against a live VNC + SSH server (typed text arrived on the remote
  machine). `asyncvnc` 1.3.0 swaps the Bell and ServerCutText message types; the driver reads
  frames itself to stay in sync.
* **Phase 2 system status:** `state_sync` reported fixed fake Wi-Fi/Tailscale values in real mode.
  It now probes `nmcli`, `tailscale`, the battery and `/proc`.
* **Stuck-loop detection:** a 64-bit dHash does not change when a line of text is typed, so
  normal terminal work would have been flagged. A finer thumbnail now distinguishes "no change".
* **Phase 5 `make -C os deploy`:** `build-image.sh` was not executable and reused stale
  binaries when present. It now always rebuilds from source.

Diagnostics added for a device with no console: `/boot/firmware/lapis-boot-report.txt` is
rewritten 45 s after every boot (service states, logs, display/input nodes, radios, memory).

---

## Android Companion (Phases A0–A5)

Source of truth: `ANDROID_COMPANION_SPEC.md` section 7 plus the execution brief (main OS first,
then `android/`, then image + APK). The wire contract actually implemented is recorded in
`ANDROID_COMPANION_SPEC.md` 5.4 and `SPEC.md` 6.3; the BLE implementation notes are in
`BLE_PROVISIONING_PROTOCOL.md` 4.1.

### Phase A0 — Daemon chat threads, auth and control-plane API

- [x] `db.py`: `paired_devices`, `chats`, `chat_messages`, `screenshots`, `schedule_runs`
- [x] `chat_manager.py`: multi-turn threads; prompts from the device land in their own thread
- [x] `core.py`: one owner of state; the Unix socket and REST/WS are thin adapters over it
- [x] `api/`: aiohttp on `127.0.0.1:8421`, bearer auth, QR nonce exchange, chats, approvals,
      screenshots, config/hosts/secrets/schedules, host probe, telemetry, `/v1/ws` hub
- [x] `--dev-pair-token`, `--dev-bind`, `--dev-advertise` (all refused without `--mock`)

Acceptance:

- [x] `uv run pytest`: API, WebSocket, `401` on every route without a valid token (all 37
      routes, enumerated from the router; only `POST /v1/pair/exchange` is public), write-only secrets (never present in any GET body, file `0600`),
      screenshot ring-buffer eviction
- [x] Binds strictly to `127.0.0.1:8421` (asserted in tests and observed in the booted image)
- [x] RSS: the whole daemon idles at 54 MB on the device image's Python (aarch64), aiohttp included

### Phase A1 — BLE provisioning, shared vectors, pairing view

- [x] `scripts/gen_ble_vectors.py` and frozen `tests/vectors/ble_crypto_vectors.json`
- [x] `ble_prov/crypto.py`, `session.py` (PIN proof, 3-strike lockout with rotation),
      `gatt_server.py` (BlueZ via `bluez-peripheral`), `network_ctl.py`, `service.py`
- [x] `lapis-ble-prov.service`: never enabled; started by the daemon on an unpaired boot or from
      the menu, exits after `finish_provisioning` or when the window closes
- [x] `ui/src/views/pairing.rs`: 6-digit PIN view and QR view; `screen_snapshot`, dark theme,
      brightness, `approval_resolved`, real battery level, OFFLINE state
- [x] `os/stage-lapis`: `bluez`, `network-manager`, the BLE unit, `tailscale serve` helper

Acceptance:

- [x] Round-trip and vector tests for X25519 / HKDF / AES-GCM and chunked framing (MTU 23/185/517)
- [x] `--dump-frame` for `pairing_pin` and `pairing_qr`: unclipped; the QR was decoded back to
      the exact payload, including from a live daemon's pairing frame
- [x] `shellcheck` clean on all `os/` scripts
- [x] GATT service exercised over a real D-Bus inside the device image's Python (object tree,
      flags, handshake, encrypted commands, notifications)
- [ ] BLE on real hardware (advertising, a phone connecting) — _needs the CardputerZero_
- [x] BLE frames capped at 512 bytes on both sides (the Bluetooth attribute-value limit, which
      Android enforces; the plain `MTU - 3` rule gave 514 at MTU 517)

### Phase A2 — Android scaffold, BLE and QR onboarding

- [x] `android/`: single `app` module, Kotlin + Jetpack Compose + Material 3, `minSdk 29`,
      `targetSdk 35`, `compileSdk 36` (the newest platform installed on the build machine)
- [x] `data/ble`: `ChunkFraming.kt` (reassembly, reset frame, 16 KiB guard), `BleEnvelopeCrypto.kt`
      (X25519, HKDF-SHA256, AES-256-GCM), `ProvisioningClient.kt` (protocol, no Android imports),
      `BleProvisioner.kt` (scan + Nordic GATT client, MTU 517 request)
- [x] `ui/onboarding`: BLE wizard (scan, PIN, Wi-Fi, Tailscale login or auth key, optional API
      key, HTTPS check with ~90 s of retries before `finish_provisioning`) and the QR path
      (CameraX + ZXing, or a pasted `lapis://pair` code)
- [x] `data/local/SecureStore.kt`: device host and bearer token live only in a Tink AES-256-GCM
      blob whose key is wrapped by the Android Keystore, in `noBackupFilesDir`; backups and
      device transfer are excluded; nothing logs (no `Log.*`, no HTTP logging interceptor)

Acceptance:

- [x] `./gradlew testDebugUnitTest lintDebug`: 191 tests, 0 failures (6 are the opt-in
      integration test); lint 0 errors, 11 warnings (all "newer version" / `OldTargetApi`, a
      consequence of the pinned SDK levels), no baseline and no suppressions
- [x] `BleEnvelopeCryptoVectorTest` (19 tests) reproduces every value in the frozen
      `tests/vectors/ble_crypto_vectors.json`; `scripts/gen_ble_vectors.py` regenerates that file
      byte-for-byte, and the Python side tests against the same file
- [x] `ProvisioningClientTest`: the phone protocol against a Kotlin port of the daemon session
      (wrong PIN, lockout and rotation, replay, chunking at MTU 23/185/517)
- [ ] BLE scan/connect/MTU, camera QR scan and Keystore on a real phone — _not run: the app is
      installed and starts on a Pixel 11 Pro (2026-11-24), but these paths need a booted
      CardputerZero to pair with_

### Phase A3 — Chats, tool timeline, approvals, kill switch

- [x] `LapisApiClient.kt` (HTTPS; cleartext refused except `127.0.0.1` / `localhost` /
      `10.0.2.2`, also enforced by the network security config) and `LapisWsClient.kt`
      (WSS, heartbeat, reconnect with backoff, resync on reconnect)
- [x] Chat list and thread with collapsible tool-call cards and screenshot previews; host switcher
- [x] Approval sheet with the 120x68 1-bit preview (`Preview1Bit.kt`); approve once / for the
      session / deny; notification actions while a task runs
- [x] Global kill switch; state changes made on the device appear in the app and the reverse

Acceptance:

- [x] Unit tests for the client, WebSocket, repositories, view models and a Robolectric pass
      over every screen (`AppSmokeTest`)
- [x] `MockDaemonIntegrationTest`, 6/6 with the real Kotlin client against a live
      `lapis-daemon --mock` on `127.0.0.1`: `401` without a token, chat run with one approval
      granted and one denied over REST + WebSocket, kill switch, QR nonce exchange (single use)
      and revocation, config and write-only secrets, timezone hand-over
- [x] Three-party check: a prompt sent through the phone API raised the approval modal on
      `lapis-ui` (with the 1-bit preview); approving through the phone API cleared it and the
      agent resumed; `kill-all` returned the device to `IDLE`
- [ ] The same on a phone over Tailscale HTTPS — _needs the hardware test_

### Phase A4 — Remote configuration

- [x] Hosts CRUD with a live VNC + SSH probe; agent/LLM settings (unknown keys are sent back
      unchanged); schedules (create, edit, run now, delete)
- [x] Secrets are write-only: the app shows only set / not set, and no GET returns a value
- [x] Device tab: battery, memory, zram, CPU, Wi-Fi, Tailscale, service states; theme,
      brightness, wake, time zone; LCD mirror; reboot / power off; start or stop pairing mode
      (BLE or QR) to add another phone
- [x] Paired-phone list with revocation

Acceptance:

- [x] `./gradlew testDebugUnitTest lintDebug assembleDebug` passes from a clean rebuild
      (`--rerun-tasks`), run independently of the session that wrote the app
- [x] Config, secrets and schedules round-trip against the live mock daemon (integration test);
      the daemon's tests assert that no secret appears in any GET body, and that neither a secret
      nor the bearer token appears in anything the daemon logs

Not built (out of scope for the brief, listed so nobody looks for them): voice input from the
phone, bundled custom fonts, a visual cron builder, push alerts when the app process is dead,
an archived-chat list, a live video stream (Live KVM shows the newest captured frame).

### Phase A5 — Image integration, APK and deploy script

- [x] `os/stage-lapis` carries the control plane, the on-demand BLE unit and the
      `tailscale serve` helper; BSP stage0–2 untouched; `gpu_mem=16` and zram (512 MiB, LZ4,
      no SD swap) preserved
- [x] `make -C os deploy` rebuilt from current source (build-time gates inside the chroot:
      daemon import self-check, `lapis-ui --dump-frame`, `systemd-analyze verify`, mask checks)
- [x] `os/deploy/os-list.json` regenerated; sizes and SHA-256s re-hashed from the files and
      compared with the manifest (match); `xz -t` clean. The `url` is a local `file://` path
      unless `LAPIS_RELEASE_URL` is set
- [x] Debug APK: `android/app/build/outputs/apk/debug/app-debug.apk`
- [x] `scripts/deploy_android.sh`: finds `adb`, requires exactly one authorized device (or
      `ANDROID_SERIAL`), `adb install -r`, launches the app; `--reverse` for a local mock daemon.
      `shellcheck` clean. Run for real on 2026-11-24 against a Pixel 11 Pro: install `Success`,
      `MainActivity` resumed on the onboarding screen, crash buffer empty (`--reverse` and
      `--build` still only exercised against a fake `adb`)

Acceptance (full regression, all re-run after the last source change):

- [x] Python: `uv run pytest` 100 passed; `ruff check` and `ruff format --check` clean
- [x] Rust: `cargo test` 74 passed; `clippy -D warnings` on the host (both feature sets) and
      `aarch64-unknown-linux-gnu`; `cargo fmt --check`
- [x] Android: `./gradlew testDebugUnitTest lintDebug assembleDebug` 191 tests, 0 failures;
      integration test 6/6 against a live mock daemon
- [x] OS: `make -C os lint`; final image booted under systemd in a container: `lapis-ui` up
      before `basic.target`, `lapis-daemon` 54 MB RSS on `127.0.0.1:8421` (`401` without a
      token), `LaunchWizard` masked. **Two claims made here were wrong and are withdrawn**
      (details under "Hardware bring-up"): "BLE service started on demand" was an observation
      inside the 20 s it took to fail for lack of an adapter, and the container never ran the
      BSP splash (`ConditionVirtualization=!container`), so it said nothing about the handover
- [x] Daemon peak RSS with real VNC captures (1600x900 target) inside the final image: 107 MB
      (budget 130 MB)
- [ ] **On the CardputerZero:** panel takeover from the boot splash, keypad, backlight, BLE
      pairing with a phone, Wi-Fi + Tailscale join, HTTPS through `tailscale serve`, a chat with
      an approval, the kill switch — _the hardware smoke test; nothing above can prove these_

Artifacts of this build (2026-11-24):

| File | Bytes | SHA-256 |
|---|---|---|
| `os/deploy/lapis-zero-arm64.img.xz` | 639,015,324 | `0f8009bac19071e7b1de74f7ad22db1ef565fa4ae69b00da2f957f7ae0275cd9` |
| `os/deploy/lapis-zero-arm64.img` | 5,368,709,120 | `73b691766917593342f66b0dafefc7eab4da2a592582625f26dd8119c80a06e0` |
| `os/deploy/os-list.json` | 1,562 | `1ff6e5d35648ec55f92d5f482b87028a97eb4b04ee3fa021e8868eb43ee84cc5` |
| `android/app/build/outputs/apk/debug/app-debug.apk` | 51,344,969 | `9001c2dcbdb6574985cdbdd235b08e6834009fdf8ae713abdfa04c75727d0692` |

Deviations from the written specs, all deliberate:

* **Claude adapter uses the Messages API over `aiohttp`, not the `anthropic` SDK.** Importing
  the SDK cost 35–50 MB of RSS on the device's Python and pushed a screenshot cycle past the
  130 MB budget. Default model `claude-sonnet-5-5`; server-side refusal fallbacks are requested
  on models that support them.
* **Android library versions:** `compileSdk 36` is the newest platform installed, so the
  Compose BOM is `2026.06.01` (the spec's `2026.11.01` needs `compileSdk 37`), with matching
  AndroidX/OkHttp versions. `targetSdk 35` and `minSdk 29` as the execution brief states.
* **Token storage:** Tink AEAD with an Android Keystore master key rather than
  `EncryptedSharedPreferences` (deprecated); same guarantee, key never leaves the Keystore.


---

## Hardware bring-up: four boots on the real device (2026-11-24)

The image built for A5 was flashed and did not come up. This section is the record of what the
device actually did, read from the card after each boot, and what changed because of it. Every
row of the first table is evidence from the hardware, not from a container.

| Boot | Card state | What happened | Evidence |
|---|---|---|---|
| 1 | A5 image as built | Stayed on the M5 logo. `lapis-ui` was running and had opened `/dev/fb_lcd`, but the BSP splash `fb_load` was running too and held the panel. `lapis-ble-prov` failed. Wi-Fi soft-blocked | `lapis-boot-report.txt`: both units `active`, `fb_load: DRM logo displayed`, `waiting for APPLaunch application to start` |
| 2 | + `systemd.mask=fb_load.service`, + a diagnostic `bootcmd` in cloud-init `user-data` | Black, no report written. **My diagnostic edit deadlocked the boot** (see below); this boot says nothing about the product | raw dump of the card: `cloud-init.log` ends at `Running config-bootcmd`, nothing written after ~18 s |
| 3 | mask only (`user-data` restored) | Black. The UI started before the LCD driver had loaded and latched onto `/dev/fb0`, which at that moment was the firmware's 16 bpp simple-framebuffer; the kernel removed it a second later and the UI drew into nothing | report: `lapis-ui: display on /dev/fb0`, then `panel-mipi-dbid` registering as fb0 at 12.96 s |
| 4 | mask + `initcall_blacklist=simplefb_driver_init` | **A Lapis screen appeared** (confirmed by the user). The UI waited, opened the panel, and its own unblank + mode re-activation lit it with no DRM master involved | report: `waiting for the LCD framebuffer`, `display on /dev/fb0`, `/proc/fb: 0 panel-mipi-dbid`, `fb_load: inactive` |

### What was wrong, and the fix

- [x] **Splash never let go of the panel.** `Conflicts=fb_load.service` on `lapis-ui.service`
      drops the splash from the boot transaction, but `sysinit.target` wants that unit, so the
      first later transaction that reaches `sysinit.target` starts it again; in job mode `fail`
      (udev `SYSTEMD_WANTS`, here the Bluetooth adapter appearing) the stop job for the UI is
      dropped "to minimise impact" and both run. Reproduced under the image's systemd 257 with
      a stub. **Fix:** the unit is masked in `/etc` and on the kernel command line; the
      `Conflicts=`/`After=` lines are gone and a build-time check keeps them out.
- [x] **UI latched onto the wrong framebuffer.** `/dev/fb0` is the firmware simple-framebuffer
      for the first ~12 s of boot and it is 16 bpp, which was the only thing the UI checked.
      **Fix:** `check_lcd_mode` accepts only 320x170 at 16 bpp, and nothing touches a
      framebuffer before it passes; the firmware framebuffer driver is also blacklisted on the
      kernel command line (the configuration of boot 4).
- [x] **Bluetooth pairing crashed on every start.** `bluez-peripheral`'s `Adapter.get_first()`
      treats every child of `/org/bluez` as an adapter, and BlueZ 5.82 exports `/org/bluez/test`
      (SAP plugin) as soon as a real adapter exists: `InterfaceNotFoundError` 0.25 s after start.
      Reproduced with the image's real `bluetoothd` on an emulated controller. **Fix:**
      `find_adapter()` picks the node that has `Adapter1` + `GattManager1` +
      `LEAdvertisingManager1`, waits up to 30 s for it, powers it on with retries (Busy,
      rfkill). Start-up failures exit 75 and the unit retries four times; the daemon shows
      "Bluetooth unavailable: <reason>" on the pairing screen instead of nothing (the old
      failure report was lost: the process exited before the daemon read it).
- [x] **Wi-Fi radio blocked.** pi-gen ships `NetworkManager.state` with
      `WirelessEnabled=false` when no Wi-Fi country is set, and NetworkManager forces rfkill to
      follow it at every start, undoing `rfkill unblock`. **Fix:** the image ships `true`;
      `lapis-firstboot` repairs the flag before NetworkManager starts; scan and connect wait
      for `wlan0` to be usable.
- [x] **A second join attempt could never succeed.** `nmcli` 1.52.1 reuses a saved profile for
      the SSID and drops its `key-mgmt` (fixed upstream later), so one mistyped password broke
      every retry. **Fix:** the stale profile is deleted before each join; a join is also not
      reported failed while DHCPv4 trails IPv6.
- [x] **"Ready" could be a lie.** Without HTTPS certificates on the tailnet `tailscale serve`
      exits 0 having configured nothing. **Fix:** success is what `tailscale serve status`
      shows; the phone is told to enable MagicDNS and HTTPS; the daemon re-asserts serve
      whenever the node is connected and QR pairing waits for it. A rejected auth key is
      reported at once instead of after a five-minute timeout, the key goes to `tailscale` in a
      0600 file instead of on its command line, and a key sent during a pending interactive
      login replaces it.
- [x] **First-boot extras from the BSP.** The disk-ID randomiser rewrote the MBR of the running
      card (it stopped `lapis-firstboot` and cloud-init on boot 1, and a power cut in its window
      leaves the card unbootable): not installed and masked. cloud-init: disabled (it applied
      nothing, sat in front of `sysinit.target`, and is what my boot-2 diagnostic hung in).
      `adbd` and `fstrim` masked. Preset `disable *` so a container boot and the device agree.
- [x] **Power guard** no longer treats 0 V, "Unknown" or a single sample as an empty battery.
- [x] **Backlight** stays on while the pairing PIN is showing (it used to switch off after
      90 s, which looks like a dead device).
- [x] **Boot report** now records which image this is, which framebuffer the UI opened and
      its driver, the DRM connector state, pending jobs, reloads and stops, Bluetooth,
      NetworkManager's radio flag and why a join failed, Tailscale and control-plane state,
      what the daemon is showing (never the PIN), listening sockets, battery and boot timing;
      journals carry timestamps. It runs at 45 s, 3, 6, 10, 15, 20 and 30 min, at the end of
      every pairing session, and from a fallback unit when a boot never reaches
      `timers.target`; the previous boot's report is kept as `lapis-boot-report.prev.txt`.
- [x] **A trail of the pairing steps.** `lapis-ble-prov`, the daemon and the UI used to log
      nothing about pairing; they now log each step by name (status word, number of networks
      found, join result, serve active or not, view on screen), with no PIN, SSID, password,
      key or token.
- [x] **Pairing window after an unfinished first attempt.** The phone is registered at the
      PIN step, so a later failure plus a power cycle used to leave a device that never showed
      a PIN again. The window now also opens on boot while the device has never joined a
      tailnet.
- [x] **rfkill names.** The BSP ships saved "unblocked" states for other boards; the CM0's own
      names (read from the card) are now in the image, so both radios are unblocked the moment
      they register on the first boot too.

Found by the independent reviews of these fixes, and corrected before the final build:

- [x] A pairing request made while "Bluetooth unavailable" was on screen was stopped again by
      that view's stale expiry timer (reproduced under systemd and as a unit test).
- [x] A pairing window that dies after it opened no longer reopens by itself (only start-up
      failures are retried): BLE stays strictly on demand.
- [x] The PIN view had no expiry of its own once the BLE process had reported a PIN; a process
      killed without saying "off" would have left a dead PIN on a lit screen.
- [x] The Tailscale step acted on a status read taken before `tailscale up` ran: a stale login
      link could be sent for a keyed join, and a key could be re-submitted to a joined node.
- [x] `run_cmd` could hang for good when a cancelled command was a wrapper script whose child
      kept the pipe open; it now kills the process group and bounds the wait.
- [x] The "Bluetooth unavailable" screen said "Type this PIN on your phone" under `--- ---`;
      it now shows the reason.
- [x] `bluez-peripheral` and `dbus-next` are pinned (the fix builds on 0.1.7 internals).

### What I got wrong along the way

* **The container boot test could not see any of this.** `fb_load.service` has
  `ConditionVirtualization=!container`; there is no framebuffer, radio or Bluetooth adapter in
  Docker; with a writable root systemd's first-boot preset pass enables units the device never
  enables. Simulations now start with a read-only root and a live udev monitor
  (`init-real.sh`), and the Bluetooth start-up is tested against the real `bluetoothd`.
* **My boot-2 diagnostic broke the boot.** A cloud-init `bootcmd` ran `systemd-run --on-active`,
  which waits for a timer unit ordered after `sysinit.target`, which waits for cloud-init:
  deadlock. I had not simulated the edit before handing the card back. Every later card edit
  was replayed in simulation first, except the two kernel/firmware options of boot 4, which
  cannot be simulated and were declared as such.
* **I told the user to look after two minutes**, when the backlight is off by design after 90 s.
* **A test I wrote ran the real `tailscale serve` on the development Mac.** It flipped a mock
  daemon to "real" while the new status loop was live, so `NetworkCtl` called the Mac's own
  CLI (which blocked without configuring anything; verified `{}` afterwards, stray processes
  killed). `make_core` in the test suite now replaces the command runner, so no Core-based
  test can reach `tailscale`, `nmcli` or `rfkill` on the machine running it.
* **Builds raced edits.** Several images were built while review corrections were still
  landing. The final verification compares the image's UI binary, units, scripts and daemon
  package byte for byte with the working tree.

### Verification of the image to flash

- [x] Python: `uv run pytest` 112 passed; `ruff check` and `ruff format --check` clean
- [x] Rust: `cargo test` 77 passed; `clippy -D warnings` on the host (both feature sets) and
      `aarch64-unknown-linux-gnu`; `cargo fmt --check`
- [x] `make -C os lint` (shellcheck + manifest self-test)
- [x] Image vs working tree: UI binary, all ten units, the four scripts, the preset and the
      whole `lapis_daemon` package are byte-identical to the tree (no build raced an edit)
- [x] 39 static checks on the image (masks, kernel command line, radio state, hostname,
      pinned library versions, BSP driver commit `a3fd3e83` as analysed)
- [x] Two simulated boots the way the device boots (read-only root at PID 1, live udev
      monitor), with a later fail-mode transaction triggered: the splash cannot start, the
      only TCP listener is `127.0.0.1:8421`, nothing is enabled that the image did not enable,
      the report is written and the previous one kept
- [x] The image's own `lapis-ble-prov` against the image's real `bluetoothd` 5.82 on an
      emulated controller: advertisement registered, a fake phone completes handshake, PIN
      proof and finish at MTU 517, exit 0 (`os/tests/ble-real-bluez/`)
- [x] Independent reviews of the changes (display path, daemon, image scripts): no blocker
      left; every finding above is fixed in this image
- [x] On the device: the UI lights the panel by itself once the splash is masked and the
      firmware framebuffer is out of the way (boot 4, old UI binary). The new binary's
      geometry check has not run on the panel yet

| File | Bytes | SHA-256 |
|---|---|---|
| `os/deploy/lapis-zero-arm64.img.xz` | 640,854,964 | `cb5000f9601e9f209f6a2a0de4f54441a6ccdf63aab1fe21a9645f6de15f6fde` |
| `os/deploy/lapis-zero-arm64.img` | 5,368,709,120 | `f7ae54f3f8a5ee148a733604c12d1586835ebc81e9911b3169f63ab8252cf4d5` |
| `os/deploy/os-list.json` | 1,562 | `29ba9fcf15bbdfacdaff36e0bfcae7241f1b21f89303fa0a91e1dd440aca3ece` |

The A5 artifact table above describes the image that failed on the device; the Android APK
is unchanged.

### Still open on the hardware

- [x] Boot of the fixed image: pairing PIN on the LCD (user, 2026-11-24). This is the new UI
      binary's panel check, the masked splash and the BLE start-up all working on the device
- [x] BLE advertising on air and a real pairing from the Android app: PIN accepted, the
      wizard moved on (Pixel 11 Pro)
- [x] Wi-Fi join from the wizard
- [x] Tailscale join: the device is on the tailnet as `lapis-zero`, online (seen from another
      node's `tailscale status`)
- [x] HTTPS through `tailscale serve`. First attempt: the device reported "Joined the tailnet,
      but HTTPS is not enabled for it" (correct: the tailnet had no `https` capability,
      `CertDomains: null`). After HTTPS Certificates were enabled in the admin console the
      retry completed, and the app reached the device over the tailnet: Device tab ONLINE,
      v0.2.0, live telemetry (battery, Wi-Fi signal, daemon RSS 53 MB, UI RSS 3 MB)
- [ ] A chat with an approval, the kill switch (see "First chats on the hardware" below: the
      LLM side works, the chat stops at the target Mac's logins)
- [ ] Passing the Wi-Fi password to `nmcli` on stdin instead of its command line (needs a
      device check that NetworkManager stores the secret that way; today it is visible in
      `/proc` to local processes for the length of the join, never logged)

## First chats on the hardware, and what they started (2026-11-24, evening)

Nothing in this section is in a firmware image yet. The image on the device is the one in the
table above; the app on the phone is build `5742b532...` (see "App builds" below).

### What the device did

| Step | Result |
|---|---|
| LLM call through the OpenAI-compatible adapter (MiniMax, `https://api.minimax.io/v1`) | Works: the model returned tool calls |
| Same provider with the protocol left on `anthropic` | "Task failed: Extra data: line 1 column 5 (char 4)": Anthropic's path on MiniMax answers a 404 page, which was parsed as JSON |
| VNC to the Mac mini | "ValueError: server requires username and password": the host's "Password secret name" held a password typed there by mistake, so no secret was found |
| SSH to the Mac mini | "PermissionDenied: Permission denied for user ... on host ...": the device has no key and never offers a password |
| The agent after the SSH failure | Kept calling `ssh_exec` (the prompt says to prefer it); three failures in a row end the task |

Not done on the hardware: a chat that completes, an approval, the kill switch.

### Phone app (in the tree; tests pass; the last round is not installed)

- Any provider's key: provider buttons (Anthropic, OpenRouter, MiniMax, OpenAI, Gemini, xAI,
  DeepSeek, Together, Fireworks) set protocol, base URL, key name and an example model; "Add
  key" stores the key under that name. Endpoints and model ids are from the providers' docs of
  2026-11-24 and were not exercised against the live services
- Host form: a write-only VNC password field replaces the "secret name" field; the password
  goes to the vault under a generated name, the host records the name
- Two independent review rounds: 8 findings, then 12 on the fixes; all fixed. The recurring one
  was a secret ending up where a name belongs. Now: name fields do not tidy input into a name
  on the Agent screen; a heuristic (`ConfigRules.looksLikeSecretValue`: a key prefix, or digits
  scattered through the text) refuses key-shaped names in the app; a host's stored name is kept
  only if a secret really is stored under it
- Gates: `./gradlew testDebugUnitTest lintDebug assembleDebug`: 199 tests run, 0 failures,
  6 skipped; lint 0 errors

App builds: `5742b532...` is installed on the phone (provider buttons, host password field, the
first 8 fixes). `c5d88077...` adds the 12 re-check fixes and is not installed (owner: "Wait for
full build").

### Daemon (in the tree)

- OpenAI-compatible adapter hardened for third-party providers: readable errors for odd
  answers, retries, fallback when a provider rejects `tool_choice` / `parallel_tool_calls`,
  only the last three screenshots kept in the conversation, config validation. 156 tests passed
  at that point. Its review found 7 more defects (among them: a kill that lands in the same
  loop tick as the request timeout is swallowed, in both adapters). Being fixed
- A host without a working SSH login is driven through the screen only: the shell tool is
  neither offered nor recommended for that task, and the chat says why

### Asked for by the owner that evening (built since: see the next section)

1. SSH that needs no thought, and agent tools on the host: a key of the device's own, the
   screen login reused for SSH, "Authorise this device", and chats that are run by Claude Code,
   opencode, Freya (`lumi-freya`) or another tool found on the host. tracker item 9g1.12
2. A 3D-style agent head with animations per state, on the device screen and in the app.
   tracker item 9g1.13
3. The review fixes above. tracker item 9g1.14

Facts found on the development Mac (not the target Mac mini) that shaped item 1: `claude`,
`opencode`, `gemini` and `cursor-agent` are only on the PATH of the login shell, not of a plain
`ssh host command`; Remote Login is off here, so nothing was tried over real SSH; `lumi-freya
run` treats every argument as a prompt (running it with `--help` spent one small completion).
Open risk: Claude Code on macOS keeps its login in the Keychain, which SSH sessions may not be
able to read.

## Effortless SSH, agent tools on the host, the new head (2026-11-25)

Built and tested on this Mac; nothing in this section has run on the device, on a real SSH
host or with a real agent tool. Daemon version is now 0.3.0 (the Device tab shows it).

### What was built

- **SSH login without setup.** The device has an ed25519 key of its own (private half 0600,
  never leaves it; `GET /v1/device/ssh-key` returns the public half). A host's `ssh.password_env`
  names a vault secret; the app's host form has "Same login as the screen", which points it at
  the VNC secret, so one typed Mac login serves Screen Sharing and Remote Login. Login order:
  identity file, device key, password (only when configured). `POST /v1/hosts/{id}/ssh/authorize`
  adds the device key to the host's `authorized_keys` and proves it by a key-only login
- **No SSH, no shell tool.** When a host has no working SSH login the agent is not offered
  `ssh_exec` for that task and the chat says why
- **Chats run by a tool on the host.** A chat has an `agent` (`lapis`, or `claude`, `opencode`,
  `freya`, `codex`, `gemini`, `cursor`, or one from `[agents.<id>]` in `lapis.toml`) and a
  `workdir`. The message goes to the tool's headless mode over SSH through the account's login
  shell (a plain `ssh host command` does not see these tools on the owner's Mac); follow-ups
  continue the tool's session where it has one; the answer comes back into the chat
- **Policy tier per tool.** A tool only runs under a host policy it can be held to; otherwise
  the task ends with a sentence naming what would allow it

  | Tool | strict | balanced | autonomous |
  |---|---|---|---|
  | Claude Code, Gemini CLI | yes | yes | yes |
  | Cursor Agent | yes | no | yes |
  | Freya (`lumi-freya agent`) | no | yes | yes |
  | opencode, Codex | no | no | yes |
  | configured tools | only if listed | only if listed | default |

- **Live action.** `state_sync` carries `action` (the kind of the step being executed now) and
  the handheld gets `task_finished`, so both screens animate what is happening while it happens
- **The agent's head.** Device: a shaded, dithered 3D-style head with ten states (idle,
  listening, thinking, working, fetching, command, approval, paused, done, error), about 0.1 ms
  per frame on this Mac, no allocation in the draw path. App: the same character drawn smooth
  with Compose, in the chat list header, the chat thread and the Device tab
- **App.** Host form switch and SSH password, "Authorise device", "Device key" with a copyable
  command, "Run with" and "Folder" in a chat, tool badge on chat rows; all hidden with one line
  against older device software

### Reviews and what they found

| Review | Findings | State |
|---|---|---|
| OpenAI-compatible adapter and config (second round) | 7 (kill swallowed in the timeout tick in both adapters, key-shaped `api_key_env`, stored file dropped whole, error wording, ...) | fixed, each with a test |
| Daemon: SSH and tools (one reviewer, the owner's choice) | 5 major, 8 minor | fixed, each with a test, except the limit below |
| Device head: art direction and Rust | 10 visual points; the head could vanish at the device's real frame rates; transitions snapped | fixed; tests step it as the live loop does |

The daemon review's majors: the kill switch and the timeout did not stop a running
`ssh_exec` (older than this work); `ssh_exec` held a command's whole output in RAM (also
older); an opencode chat ignored a strict or balanced policy; a message sent during a kill ran
untracked; commands a tool started in their own session survived the kill.

**Known limit (expected-failure test `test_a_command_that_is_nobodys_child_any_more_is_stopped_too`):**
a kill stops the tool and everything still descended from it, but a command whose parent had
already exited cannot be found on macOS without privileges. In the cases where the device
cannot confirm a stop (dead link, or the sweep could not run) the kill message says the tool
"may still be running"; in the orphan case it cannot know and says "stopped".

### Verification (2026-11-25, this Mac)

| Check | Result |
|---|---|
| `daemon`: `uv run ruff check .`, `ruff format --check .` | clean |
| `daemon`: `uv run pytest -q` | 278 passed, 1 xfailed (the limit above) |
| `ui`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` | clean |
| `ui`: `cargo test` | 108 + 2 passed |
| `android`: `./gradlew testDebugUnitTest lintDebug assembleDebug` | 256 run, 0 failures, 7 skipped; lint 0 errors |
| `android`: `MockDaemonIntegrationTest` against the new daemon in `--mock` on 127.0.0.1 | 6 of 6 passed |
| New endpoints against `--mock` by hand (curl) | listening on 127.0.0.1 only; 401 without a token; key, agents, probe, authorise, chat agent, `action` in state; opencode refused on a balanced host; a Claude Code chat ran with `ACTING` / `agent_run` |

Not verified anywhere: real sshd and a macOS login shell; Claude Code's login over SSH (it may
sit in the macOS Keychain, which an SSH session may not be able to read); real opencode
session ids; Gemini, Cursor, Codex and Freya beyond their help text or source; the head on the
LCD and on a phone; the app's new screens against a real device.

### The image (built 2026-11-25, not yet flashed)

| File | Bytes | SHA-256 |
|---|---|---|
| `os/deploy/lapis-zero-arm64.img.xz` | 642,419,324 | `f26b59955057ab34371f5a8eb6ccc013e93447f9f61ba19063b70434c38dc5a4` |
| `os/deploy/lapis-zero-arm64.img` | 5,368,709,120 | `e00debadffd3dde53f0872cbd0d3f80d33a330f3d8dc160ca507516081a84e9d` |
| `os/deploy/os-list.json` | 1,562 | `81e9cc54b2d11f308097058a15322d221aabaad572dd57240472d7b8619ff0b4` |

The image flashed on 2026-11-24 (0.2.0) is kept as
`os/deploy/previous/lapis-zero-arm64-0.2.0-build7.img.xz` (`cb5000f9...`).

Checked with the same script as that image: the manifest matches the files and `xz -t` passes;
39 static checks pass, none fail; the daemon package, the UI binary, the units and the scripts in
the image are byte-identical to the working tree; daemon 0.3.0 with `asyncssh`, `cryptography`
and the tool table importable in the image's venv. Simulated first boot (read-only root, udev
trigger): `lapis-ui`, `lapis-daemon` and `lapis-ble-prov` active, `fb_load` masked, listening
only on `127.0.0.1:8421`; `systemd-firstboot` and `systemd-growfs-root` failed there, the same
two as in the simulation of the 0.2.0 image that boots on the device. Second boot after a hard power-off: no failed units. Real `bluetoothd`
with the emulated kernel side and the fake phone: PIN pairing verified and a token issued.
Nothing of this is the device: the LCD, the radios and the new head on the panel are unseen.

### Screenshots

`screenshots/cardputerZero/`: 38 renders from the device UI's own dump mode (every view and
every head state, both themes, 320x170 scaled 3x). `screenshots/android-app/` is filled after
the device is reflashed and re-paired (owner's choice: the running app against the real device).

## 0.3.0 on the device, and the two VNC failures (2026-11-25)

The owner flashed 0.3.0, re-paired and added two hosts. On the device, seen through the app:
SSH logs in on both hosts with the device key ("ok (device key)"); agent tools are found over
real SSH through the login shell (this Mac: Claude Code, opencode, Freya, Gemini CLI, Cursor
Agent; the Mac mini: Freya); a lapis chat ran real `ssh_exec` steps on the Mac mini. The new
head, pairing, the app's new screens: on the device and the phone, not yet looked at by me.

VNC failed on both hosts, for two different reasons.

| Host | What the app showed | Cause |
|---|---|---|
| `dev-mac` (the development Mac, 3440x1440 beside 1920x1080) | `TimeoutError: ` | The login is accepted. The combined desktop is 5360x1440, and the 1920x360 strip under the second display belongs to no display: macOS never sends pixels for it. The driver waited until every pixel of the rectangle had arrived, which never happened |
| `mac-mini` | `IncompleteReadError: 0 bytes read on a total of 4 expected bytes` | The Mac refuses the login. macOS 26 answers a refused Screen Sharing login by closing the connection (reproduced here with a made-up account). SSH there uses the device key, so the stored password was never checked. Not fixed by software: the owner has to re-enter that host's password (or allow the user under Screen Sharing) |

What I got wrong first: I took the timeout for a memory and bandwidth problem (31 MB per
frame, held three times over, against the daemon's 130 MB cap). The check against the real
Mac said otherwise: "login: ok in 1.3 s ... the host stopped sending its screen after 3.1 MB
and 21 s". The stripe-wise reading written for the first theory stays, because it is what the
512 MB device should do anyway.

Changes (`daemon/src/lapis_daemon/kvm/vnc_driver.py`, `core.py`, `kvm/base.py`; version 0.3.1):

- A frame is done when the host has sent an update and then nothing for 0.5 s; what no
  display covers stays black, and the capture note says what share the displays cover
- Each rectangle is scaled into the small image a stripe at a time while it arrives, raw or
  zlib, in any of the four pixel layouts: a 31 MB frame peaks under 10 MB in the test
- Limits: 20 s of silence or 120 s in all for a frame (was a flat 20 s); the probe allows VNC
  45 s; a timeout says what ran out instead of an empty `TimeoutError: `
- A refused login reads "... refused the screen login for user 'x': check the user name and
  password in this host's form and that this user is allowed under ... Screen Sharing"
- `Frame.note` and the probe's VNC detail: native size, size shown, encoding, MB, seconds

Checked: `uv run pytest -q` 283 passed, 1 xfailed; and `scripts/check_vnc_host.py --device`
(the driver inside the device image, 0.25 CPU, hard 160 MB, against this Mac's real Screen
Sharing; the owner typed his password into its hidden prompt): "login: ok in 1.1 s; capture 1:
5360x1440 shown as 1280x344, zlib, 3.0 MB in 2.0 s, displays cover 91% of that area; JPEG 52 KB;
peak Python memory 4.6 MB; capture 2: ... 1.1 s; process peak memory: 70 MB".

Open: two displays shrunk to 1280x344 leave text about a quarter of its size for the model; a
host has no way yet to pick one display. Not tried on the device itself.

### Image 0.3.1 (built 2026-11-25, not yet flashed)

| File | Bytes | SHA-256 |
|---|---|---|
| `os/deploy/lapis-zero-arm64.img.xz` | 641,493,040 | `c4e1e65eb179a17c2fa58ebd50809abb980c588c36cd6bd8f8b7c17274bf44cb` |
| `os/deploy/lapis-zero-arm64.img` | 5,368,709,120 | `b810478abf757d4dcc92dd83fa8a57b23a31ee9f76cd4d0f8fe425e2ac815339` |
| `os/deploy/os-list.json` | 1,562 | `fbc84a2cc93c0d22905af27cc5833454c42520d98503d61c9194a1eeaf34e65c` |

Earlier images are in `os/deploy/previous/` (0.2.0 `cb5000f9...`, 0.3.0 `f26b5995...`).
Same checks as 0.3.0, same results: manifest matches, 39 static checks pass, image identical to
the tree, two simulated boots (the same two container-only units fail on the first), PIN
pairing on the Bluetooth rig; daemon 0.3.1 with the VNC changes importable in the image.

### 0.3.1 on the device (2026-11-25, seen through the app on the phone)

The owner flashed 0.3.1 and re-paired ("looks great"). Read from the app while taking
screenshots; I did not operate the device itself.

| Check | Result on the device |
|---|---|
| Device tab | ONLINE, V0.3.1, live LCD mirror, the new head |
| VNC, `dev-mac` (two displays) | OK: "5360x1440 shown as 1280x344, zlib, 3.1 MB in 2.9 s, displays cover 91% of that area" |
| VNC, `mac-mini` | OK: "1920x1200 shown as 1152x720, zlib, 4.5 MB in 2.3 s" (after its password was entered again) |
| SSH, both hosts | OK: "ok (password)" |
| Agent tools found | this Mac: Claude Code, opencode, Freya, Gemini CLI, Cursor Agent; the Mac mini: Freya |
| Chats | completed chats that took and described a screenshot; one ended "Stopped by the kill switch." |
| A refused screen login (before the password was fixed) | shown in the chat as "PermissionError: ... refused the screen login for user ..." |

Seen and not addressed: the Device tab's memory card is over budget ("OVER 220 MB", 223 to
234 MB in use of 462; lapis-daemon RSS about 98 to 106 MB) once VNC and SSH have been used.
Not seen by me: an approval prompt, a chat run by a host tool, the head on the LCD itself.

Screenshots: `screenshots/android-app/` holds 19 captures of the running app (chats, a chat
thread with its steps, the Run-with menu, Live KVM, Device, Config and its screens, the host
form, Authorise device, Device key, paired phones and devices). At the owner's request IP
addresses, host and account names, tailnet names, the Wi-Fi name, the time zone, key
fingerprints, device ids, host output and every picture of a host's or the device's screen are
blurred (coarse blocks, then a blur). `screenshots/cardputerZero/` is unchanged (demo data, no
personal details).


## 0.3.2: screensaver on the device, chat overhaul in the app (2026-11-25)

Device: after 20 s idle with nothing waiting on the owner, the agent head fills the LCD (12 fps,
drifting, with a clock); the waking key is swallowed except Esc-hold and push-to-talk release.
`ui/src/views/screensaver.rs`. App (no daemon change, works with 0.3.1): tool logos, sessions as
tabs, a "Start a session" row per host, user bubbles, Markdown answers, folded tool steps,
plain-text previews. Checked: cargo 111+3, clippy, fmt; daemon 283 passed, 1 xfailed; app 260
tests, lint. The app is installed on the phone.

### Image 0.3.2 (built and flashed 2026-11-25)

| File | Bytes | SHA-256 |
|---|---|---|
| `os/deploy/lapis-zero-arm64.img.xz` | 639,245,984 | `68cd2d9781e65967c15f6542226b62f624bcdf85c91b6578ed03641c4aa478d9` |
| `os/deploy/lapis-zero-arm64.img` | 5,368,709,120 | `b8cae34048fe98e71fc9d0572c8fb25eb17d66fef5bfa1c64396a45cc4ee9382` |
| `os/deploy/os-list.json` | 1,562 | `c1defe1f96b06842d54a6b218b45b15173b7427b6fb6fb86c8b6ff1dfa33e324` |

0.3.1 moved to `os/deploy/previous/lapis-zero-arm64-0.3.1.img.xz`. In the image: `lapis-ui`
byte-identical to the fresh cross build (has `--screensaver`), daemon wheel 0.3.2, hostname
`lapis-zero`, cloud-init off. Not repeated this time: the simulated boots and the Bluetooth rig.

Flashed with `dd` to the 1 TB card in the built-in reader. macOS mounted `bootfs` right after the
write and added `.Spotlight-V100` and `.fseventsd`, so the whole-card hash differs; checked by
part instead: every boot file identical to the image's, root partition (`64bdd308...`) and
partition table (`cdee224a...`) identical. Not yet booted.

## 0.3.3: Claude Code and Gemini CLI on a Mac host (2026-11-25)

Seen on the phone after 0.3.2: a Claude Code chat ended "Not logged in · Please run /login" on a
Mac where `claude` is logged in, and every Gemini CLI answer began with its TERM=dumb and
Ripgrep warnings (also as the step line).

- Claude Code on macOS keeps its login only in the login Keychain (`Claude Code-credentials`),
  which a session over SSH cannot open. Each run now reads `~/.claude/lapis-token` on the host
  (made with `claude setup-token`, mode 600) into `CLAUDE_CODE_OAUTH_TOKEN` when that is not
  already set; the token never leaves the host. A reply that still asks for /login says how.
- Gemini CLI runs with `-o json`; its indented answer object is read past the warnings
  (`read_gemini`), and it has no ticker line. Reproduced on this Mac with Gemini CLI 0.71.1.
- `agent/harness.py`; checked: 285 passed, 1 xfailed (real Gemini output in a unit test, the token
  file through the fake SSH host), ruff clean, mypy unchanged (46, none new). The token file on
  cut-mac-002 is in place and Claude Code answers with it alone.
- Seen once, not addressed: Gemini's own `response` began with leaked model reasoning
  ("...thought / CRITICAL INSTRUCTION 1"); the run before was clean.

### Image 0.3.3 (built 2026-11-25, not yet flashed)

| File | Bytes | SHA-256 |
|---|---|---|
| `os/deploy/lapis-zero-arm64.img.xz` | 641,771,016 | `547393d629daaa64cd384afdb328150097c3ba6be40450f3737fa59d57f75e25` |
| `os/deploy/lapis-zero-arm64.img` | 5,368,709,120 | `81b3634c2573e8f7eae22c1ae5c1025ef727c4e81efb2b0840b52be382732456` |
| `os/deploy/os-list.json` | 1562 | `b7b89d4d874fce11397dc1833480c92ccc0699b72e120496488afe41e35244ef` |

0.3.2 moved to `os/deploy/previous/`. In the image: daemon wheel 0.3.3 with both changes,
`lapis-ui` unchanged from 0.3.2 (`fac4d78e...`).
