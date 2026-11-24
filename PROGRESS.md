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

- [ ] `gfx/spring.rs` + `gfx/mascot.rs` (5 animation states + desk clock)
- [ ] All 6 views (`companion`, `ticker`, `menu`, `approval`, `prompt`, `setup`) with spring view slides, spring menu pill, 1-bit 120×68 preview in `approval.rs`
- [ ] `--demo` mode cycling states, synthetic approval modal, first-boot onboarding view

**Acceptance criteria**

- [ ] Smooth 60 FPS transitions across all 6 views
- [ ] `--dump-frame` PNGs for all 6 views verify zero text overlap or clipping at 320×170
- [ ] Idle governor drops to 1 FPS

**Report:** _pending_

---

## Phase 2 — IPC Protocol & Daemon Core (Mock Driver)

**Deliverables**

- [ ] `db.py` (SQLite WAL), `config.py`, `system_status.py`, `ipc_server.py`
- [ ] `ui/src/ipc.rs` with non-blocking reads + automatic reconnect
- [ ] `agent/runner.py`, `agent/policy.py`, `MockHostDriver`

**Acceptance criteria**

- [ ] `lapis-daemon --mock` + `lapis-ui --sim` executes a full task Prompt → SQLite → Ticker → Approval Modal → `ESC` kill-switch
- [ ] Daemon restart auto-reconnects and re-syncs within 2 s

**Report:** _pending_

---

## Phase 3 — Option C KVM Drivers (Tailscale VNC + SSH) & LLM Adapter

**Deliverables**

- [ ] `kvm/vnc_driver.py` (asyncvnc, Pillow downscale, coordinate transform, 1-bit 120×68 crosshair thumbnail, modifier key-up cleanup)
- [ ] `kvm/ssh_driver.py` (asyncssh, timeout + 4 KB truncation)
- [ ] `llm/anthropic_cua.py` behind `llm/base.py`, dHash stuck detection, `agent/scheduler.py`

**Acceptance criteria**

- [ ] Unit tests pass: coordinate math, 1020-byte 1-bit thumbnail packing, dHash stuck detection, policy tiers
- [ ] Live Tailscale host test completes a multi-step GUI + SSH task with daemon peak RSS < 130 MB

**Report:** _pending_

---

## Phase 4 — CardputerZero CM0 Hardware Backends & Cross-Compilation

**Deliverables**

- [ ] `ui/src/backend/fbdev.rs` (`/dev/fb0` mmap RGB565 + stride-safe flush)
- [ ] `ui/src/backend/hw_cm0.rs` (TCA8418 evdev, BQ27220 battery, backlight `fn+U/I` + idle blanking, BMI270 motion wake)
- [ ] `aarch64-unknown-linux-gnu` cross-compile (`zigbuild`/`cross`) targets in `Makefile`

**Acceptance criteria**

- [ ] `make cross-ui` builds a stripped aarch64 release binary cleanly
- [ ] Linux hardware backend unit tests and mock-sysfs tests pass

**Report:** _pending_

---

## Phase 5 — Custom Flashable OS Image Pipeline (`os/` + M5 Imager Integration)

**Deliverables**

- [ ] `os/config.pigen` + `os/build-image.sh` (clones `CardputerZero/pi-gen` `cardputerzero_v1.1`, runs `STAGE_LIST="stage0 stage1 stage2 stage-lapis"` in Docker)
- [ ] `os/stage-lapis/` (`00-os-tuning`, `01-tailscale`, `02-lapis-stack`, `03-services-firstboot`) incl. `gpu_mem=16`, `fbcon=map:off`, st7789 initramfs hook, `zram-generator.conf`, systemd units, `lapis-firstboot.sh` (bootfs seed importer + secret shredder), `lapis-powerguard`
- [ ] `os/generate-imager-manifest.py` emitting M5 Imager `os-list.json` with exact SHA256s and byte sizes
- [ ] `os/README.md` + `os/docs/setup_target_hint.md` (M5 Imager `Use custom` / `--repo`, `rpiboot`, bootfs seeding)

**Acceptance criteria**

- [ ] `shellcheck` passes on all scripts in `os/`
- [ ] `os/generate-imager-manifest.py` passes schema validation against `CardputerZero/m5stack-imager` `os-list-template.json`
- [ ] `make build-os` produces bootable `lapis-zero-arm64.img.xz` + `os-list.json`
- [ ] Flash via M5 Imager boots CardputerZero into Lapis-Zero: `multi-user.target`, zero desktop/X11/APPLaunch, ~496 MB Linux RAM visible, total active RSS < 220 MB, `lapis-firstboot.service` imports and shreds bootfs secrets

**Report:** _pending_

---

## Phase 6 — Push-to-Talk Voice Pipeline & Audio Polish

**Deliverables**

- [ ] `daemon/src/lapis_daemon/voice.py` (ALSA 16 kHz mono PTT capture, live `mic_level` RMS → UI waveform, STT transcription, auto task enqueue)
- [ ] Toggleable 8-bit procedural audio cues via ES8389 ALSA output

**Acceptance criteria**

- [ ] Holding PTT animates the Companion waveform reactively
- [ ] Releasing transcribes speech and dispatches the task to the active Tailscale host without breaking the RAM budget

**Report:** _pending_
