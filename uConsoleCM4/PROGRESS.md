# Lapis-Deck — Progress

Tracking: beads epic `ankur-3hf` (`BEADS_DIR=/Users/ankur/.beads`), one task per phase
(`ankur-3hf.1` … `ankur-3hf.7`). Gates are run with `scripts/verify.sh`, which writes the real
output of every AGENTS.md §3 command to `target/verify.log`; the tables below are copied from it.

## Decisions that shape every phase

- **The existing Lapis-Zero Android app must keep working** (user, 2026-11-25). Lapis-Deck therefore
  keeps the `lapis-ble-v1` BLE protocol byte for byte (same service UUIDs, same frozen
  `tests/vectors/ble_crypto_vectors.json`), the `lapis://pair?…` QR format, and the Lapis-Zero REST/WS
  API (`/v1/chats`, `/v1/approvals`, `/v1/state`, `/v1/ws`, …). `companion-android/` is that app with
  the Deck features added, same applicationId.
- **The device is a command center like the app** (user, 2026-11-25): the HUD's AGENT and FLEET
  workspaces drive the same chats, hosts, agent CLIs and approvals through the same daemon API.
- **Split of responsibilities.** `lapis-daemon` (Rust) owns `127.0.0.1:8421`, bearer auth, pairing,
  the Policy Gate and approvals, redaction of every outgoing frame, hardware, SDR, mesh, GPS and
  NetOps. `harness/` (Python) is the Lapis-Zero daemon's proven agent stack (chats, task runner, VNC/SSH
  KVM, remote CLI harness table, LLM adapters, schedules) moved behind the Rust daemon on a Unix socket;
  every step it takes is authorised by the Rust Policy Gate.

## Phase 0 — Workspace & Crypto Foundations ✅ (2026-11-25)

Files:
- `Cargo.toml` (workspace, `unwrap_used`/`expect_used` denied outside tests via `clippy.toml`),
  `.gitignore`, `LICENSE` (Apache-2.0), `git init` (nothing committed).
- `crates/lapis-crypto/`: `ble.rs` (X25519 with the all-zero check, transcript, HKDF-SHA256 key
  schedule, PIN proofs with constant-time compare, AES-256-GCM envelope, `DeviceChannel` /
  `ClientChannel` sequence counters), `framing.rs` (4-byte chunk header, reassembly with order,
  16 KiB and 5 s limits), `token.rs` (256-bit tokens, SHA-256 at rest, unbiased PINs), `redact.rs`
  (PEM keys, Bearer/Basic, provider key formats, secret-named key/values, registered known values,
  structural JSON redaction).
- `tests/vectors/ble_crypto_vectors.json`: copied unchanged from Lapis-Zero (the app's own vector
  test reads the same bytes). `scripts/gen_ble_vectors.py`: the reference generator.
- `tests/vectors/redaction_vectors.json`: new shared vectors, run by Rust and Python.
- `harness/`: uv project (Python 3.13 pinned, matching Debian trixie), ruff, pytest-asyncio, mypy strict;
  `lapis_harness/redact.py` mirrors the Rust rules.
- `scripts/verify.sh`: the gate runner.

Verification (`scripts/verify.sh`):

| Gate | Result | Output |
| --- | --- | --- |
| `cargo fmt --check` | PASS | (no diff) |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS | `Finished dev profile` |
| `cargo test --workspace` | PASS | unit 4 passed; vectors 4 passed (`chunk_framing_vector`, `crypto_vector`, `handshake_rejections`, `redaction_vectors`) |
| `cd harness && uv run ruff check .` | PASS | `All checks passed!` |
| `cd harness && uv run pytest` | PASS | `2 passed` |
| `uv run mypy` (strict, extra) | PASS | `no issues found in 2 source files` |

Hardware-only items deferred: none for this phase (pure crypto).

## Phase 1 — Daemon Control Plane & Policy Gate ✅ (2026-11-25)

`crates/lapis-daemon/` (Rust, tokio + axum):
- `api.rs`: binds **127.0.0.1:8421 only** (`config::BIND`, no flag widens it). Lapis-Zero routes the
  existing Android app uses, same JSON shapes and `{error:{code,message}}` envelope: `/v1/health`,
  `/v1/pair/exchange` (the one public route; 429 lockout, 403 `bad_nonce`), `/v1/devices[/{id}]`,
  `/v1/state`, `/v1/approvals[/{id}]`, `/v1/agent/kill-all`, `/v1/device/{telemetry,ui,screen.png,power}`,
  `/v1/ws`. Lapis-Deck additions: `/v1/deck/{status,rails/{name},tools,tools/invoke,audit}`,
  `/v1/policy/authorize` (local token only, long-polls), `/v1/internal/screen/{id}`. Everything else
  under `/v1/` is forwarded to the harness (`proxy.rs`, Unix socket, caller identity in
  `x-lapis-device-*` headers, `Authorization` stripped).
- `auth.rs`: paired phones with SHA-256 token hashes at rest (`devices.json`, 0600), a device-local
  token file for lapis-ui/harness/MCP (`local.token`, 0640), single-use 180 s QR nonces, 5/min lockout.
- `policy.rs`: deny-by-default classifier + Gate. Tier 0 runs; Tier 1 needs `[Y/N]` (approve for the
  task covers later Tier 1 steps); Tier 2 needs hold-to-confirm (`hold_ms` 1500), is never covered by a
  session grant nor by a host's autonomous policy; hard denylist; unknown tools denied; approvals time
  out (600 s) as denied; kill switch denies all; every decision audited (`audit.rs`, JSONL, redacted,
  rotates at 4 MiB). Approved steps get a single-use approval token (hash in the audit log).
- `events.rs`: the one fan-out; every frame passes `lapis_crypto::redact_json` before serialisation.
  Log lines go through `RedactingStderr`; panics are redacted too.
- `hw.rs`: `Hardware` trait — AXP228 battery (`axp20x-battery`, `axp22x-ac`), rails (AIO V2 GPIO 27 GPS,
  16 LoRa, 7 SDR, 23 USB hub via `pinctrl`; `rtl_biast`; `uconsole-4g-cm4`; rfkill Wi-Fi/BT), backlight
  (`backlight@0`, never written to 0), mute, timezone, power, services. `LinuxHardware` (sysfs under a
  root, commands through an injectable `Runner`) and deterministic `MockHardware`.
- `net.rs`: NetworkManager/Tailscale status and provisioning control ported from Lapis-Zero
  (`tailscale up --auth-key=file:` on tmpfs, `serve` verified by reading it back).
- `ble/`: `lapis-ble-v1` session ported from Lapis-Zero (PIN proof, 3-strike lockout + rotation,
  wifi/tailscale/secrets commands) and a BlueZ GATT peripheral (`bluer`, feature `bluez`, Linux).
  Advertises `LapisDeck-XXXX` with the same service UUID the app scans for.
- `app.rs`: merged `state_sync` (harness agent state + device fields + `deck` extras), telemetry
  ticks (5 s subscribed / 30 s all), QR/BLE pairing views, UI settings, screen mirror requests.

## Phase 2 — SDR & Cyberdeck Subsystems ✅ (2026-11-25)

- `crates/lapis-sdr/`: `iq.rs` (`IqSource` trait; `rtl_sdr` u8 and SoapySDR `rx_sdr` CF32 command-line
  sources; deterministic `Synthetic` band plan: broadcast FM, airband AM, NOAA, APRS, 433.92 MHz OOK,
  915 MHz, ADS-B pulses, HF USB tone), `dsp.rs` (Hann FFT, fft-shifted dBFS, smoothing, peaks, band
  scan with abort, waterfall quantisation + HUD colour map), `demod.rs` (WFM/NFM/AM/USB/LSB to ~48 kHz
  audio, WAV), `decoders.rs` (`rtl_433` JSON, readsb `aircraft.json`, `multimon-ng` POCSAG/APRS),
  `devices.rs` (USB SDR detection from sysfs: RTL-SDR, HackRF, Airspy, PlutoSDR).
- Daemon: `deck.rs` (spectrum pump at ~10 rows/s to `subscribe_spectrum` sockets, synthetic fallback
  when the tool is missing, degraded status when the dongle drops; decoders borrow the dongle),
  `mesh.rs` (meshtasticd TCP framing + hand-written prost messages for MyInfo/NodeInfo/MeshPacket/
  Data/User/Position/RouteDiscovery; nodes with SNR/RSSI/hops, text, traceroute, heartbeat,
  reconnect), `gps.rs` (gpsd WATCH: TPV/SKY/PPS), `netops.rs` (passive Wi-Fi via `nmcli --rescan no`,
  BLE via bluetoothctl, TCP port probe, tcpdump argv without a shell, DNS).
- 25 typed tools (`GET /v1/deck/tools`): `hw.*`, `sdr.*`, `mesh.*`, `gps.*`, `net.*`. Arguments are
  validated before the gate, so the owner is only asked about calls that can run.

Verification (2026-11-25, macOS host, real output):

| Gate | Result | Output |
| --- | --- | --- |
| `cargo fmt --check` | PASS | (no diff) |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS | `Finished dev profile` |
| `cargo test --workspace` | PASS | lapis-crypto 4+4, lapis-daemon 21 unit + 2 end-to-end (`control_plane_end_to_end`, `deck_subsystems_over_the_api`), lapis-sdr 7 |
| `scripts/linux-check.sh check` (arm64 Debian trixie container, rust 1.97, `--features lapis-daemon/bluez`) | PASS | clippy clean; lapis-daemon 21 + 2 passed |
| Python gate | see Phase 3 (harness is being ported; rerun there) | |

What the end-to-end tests cover: 401 without a token; 503 `harness_unavailable` before the harness is
up; QR pairing exactly as the app does it (payload `lapis://pair?host=…&nonce=…&pin=…&tls=0`, exchange,
single use); proxy carries the phone's identity and never its token; harness events re-broadcast
redacted; a harness `ssh_exec` approval decided by the phone over REST; a Tier 2 `hw.gpio_rail` approved
over the WebSocket; the kill switch denying a pending approval; UI settings validation; 504 screen
mirror without a HUD; immediate revocation; the live waterfall reaching a subscribed socket; scans,
WFM demodulation (1 kHz tone recovered), IQ capture, ISM/ADS-B/pager decodes, GPS fix, mesh nodes,
Wi-Fi/BLE surveys; LoRa TX waiting for `[Y/N]`; a denied probe never running.

Hardware-only items deferred to the uConsole:
- BlueZ advertising/GATT with the real phone (compiled and linted for arm64; not run).
- AXP228 sysfs values, `pinctrl` rail levels on AIO V2 (GPIO 6/7 may clash with the panel's SPI4 per
  the uConsole overlay: check PPS on GPIO6 and the SDR rail on GPIO7).
- `rtl_sdr`/`rx_sdr` streaming, `rtl_433`, `readsb`, `multimon-ng` against real dongles.
- meshtasticd with the SX1262 on the AIO board; gpsd on `/dev/ttyS0` with PPS on GPIO6.

## Phase 4 — Custom Cyberdeck UI (`lapis-ui`) ✅ (2026-11-25)

`crates/lapis-ui/` (Rust, egui 0.31):
- Backends: `kms.rs` (feature `kms`, Linux): DRM/KMS modeset on the connected connector, GBM
  surface, EGL/GLES via glutin, egui painted into a 1280x720 FBO and blitted rotated to the
  720x1280 portrait DSI panel (90° default, `--rotate`), vsynced page flips, `glReadPixels` for the
  screen mirror; evdev input threads (keyboard, trackball, gamepad buttons) **grabbed** so the VT
  never sees a key. `desktop.rs` (feature `desktop`, host preview only): eframe window; F5–F10
  stand in for A/B/X/Y/Select/Start. No X11/Wayland in the device build (`--no-default-features
  --features kms`).
- `input.rs`: uConsole gamepad mapping from the keyboard firmware (D-pad = arrow keys; X/A/B/Y =
  BTN_TRIGGER/BTN_THUMB/BTN_THUMB2/BTN_TOP; Select/Start = BTN_BASE3/BTN_BASE4), overridable
  (`--pads`) for QMK firmware; US keymap; push-to-talk on F12 / headset KEY_PLAYPAUSE.
- `client.rs` / `model.rs`: one WebSocket to the daemon (the same stream the phone gets) plus REST
  on worker threads; the HUD is a client of the same API as the Android app.
- `app.rs`: top bar (battery % and W, CPU temp, Wi-Fi, Tailscale, SDR/LoRa/GPS/USB rails, agent
  state, clock), bottom key hints, **approval prompt** (Tier 1 `[Y]/(A)` once, `[S]/(X)` for the
  task, `[N]/(B)` deny; Tier 2 hold `[Y]`/(A) 1.5 s with a progress bar; the screenshot with the
  click point), command palette (`Ctrl+Space` / Y; free text asks the deck agent), toasts, kill
  switch (palette, or Select+Start together).
- Workspaces (`Alt+1..7`, Select/Start cycle): `[1] AGENT` command center like the app (chats per
  host incl. the deck itself, new chat → host → agent CLI detected on the host, streaming thread
  with tool trace, artifact viewer with screenshots and tool output); `[2] RF/SDR` (spectrum +
  scrolling waterfall from `sdr_spectrum`, D-pad/click/drag tuning, step and gain, presets with
  demod mode, rtl_433 / multimon-ng / ADS-B / listen / band scan, decoded feed); `[3] MESH/GPS`
  (nodes with SNR/RSSI/hops and distance/bearing from the fix, channel chat with Tier 1 TX,
  traceroute, GPS fix, PPS state, satellite skyplot); `[4] NETOPS` (Tailscale peers, passive Wi-Fi,
  BLE, port probe, tcpdump capture, DNS); `[5] FLEET/KVM` (host roster with tailnet online state,
  probes, Remote Harness Selector for claude/opencode/freya/gemini/cursor-agent/codex, live VNC
  viewport with trackball/keyboard passthrough through the gate, SSH terminal); `[6] PTY/MUX`
  (portable-pty + vt100: tabs, split, colours, scrollback, UART consoles); `[7] SYSTEM` (rails, brightness,
  theme, QR / BLE pairing with the QR rendered on screen, paired phones + revoke, audit log, hold-A
  power actions).

Verification:

| Gate | Result | Output |
| --- | --- | --- |
| `cargo fmt --check` | PASS | (no diff) |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS | `Finished dev profile` |
| `cargo test --workspace` | PASS | lapis-ui 6 unit (keymap/pads, model, real shell through a PTY, VT colours, ports, great circle) + 3 headless HUD tests (`every_workspace_renders`, `approvals_by_keyboard_and_gamepad` incl. the 1.5 s Tier 2 hold, `navigation_palette_and_kill_switch`) |
| `LAPIS_LINUX_FEATURES=lapis-daemon/bluez,lapis-ui/kms scripts/linux-check.sh check` (arm64 trixie) | PASS | clippy clean with the KMS + BlueZ code; lapis-ui 8 unit (adds KMS rotation + evdev→egui) + 3 HUD; daemon 21 + 2 |
| Visual check (`lapis-ui --mock-hw --screenshot`, against the mock daemon) | done | RF, MESH/GPS and SYSTEM rendered and inspected; fixed top-bar overflow and overlapping peak labels |

Hardware-only items deferred: DRM master and page flips on the vc4 DSI panel, the rotation
direction, evdev codes of the real keyboard/gamepad/trackball, GLES on VideoCore VI
(see `os-image/HARDWARE_CHECKLIST.md`).

## Phase 3 — Enhanced Agentic Harness & Fleet KVM ✅ (2026-11-25)

`harness/` (Python 3.13, uv, aiohttp; built by a background agent from my brief, then
re-verified here and run end to end against the Rust daemon):
- **Port**: Lapis-Zero's hardware-tested daemon (`lapis_daemon`) → `lapis_harness`: chats and
  message history, the task runner (screenshot → plan → gate → act), VNC + SSH KVM drivers over
  Tailscale (dHash stuck detection), the Anthropic/OpenAI computer-use planners, schedules,
  secrets, voice push-to-talk, frames ring buffer. Kept the app's JSON shapes for every route it serves.
- **Removed (now in Rust)**: BLE provisioning, the CM0 IPC server, bearer auth, QR pairing,
  paired devices, `/v1/device/{telemetry,ui,screen.png,power}` — with their tests.
- **Behind the daemon**: Unix socket (0660), caller identity from `x-lapis-device-*` headers.
- **`gate.py`**: `DaemonGate` asks `POST /v1/policy/authorize` for **every** runner step and fails
  closed (no token / unreachable / non-200 / unknown decision ⇒ deny); `LocalGate` = the old
  in-process approvals, for tests. The nudge route is gated too.
- **`remote.py`**: `RemoteHarnessAdapter` protocol with adapters for `claude`, `opencode`,
  `freya`, `gemini`, `cursor-agent`, `codex` (detect over the login shell, csh-safe; session
  resume; streaming; hard timeout; `CLAUDE_CODE_OAUTH_TOKEN` from `~/.claude/lapis-token`;
  Gemini `-o json` last-object parsing).
- **`mcp.py`** (`lapis-mcp`): stdio MCP server exposing the daemon's 25 cyberdeck tools
  (`sdr.scan_band` ↔ `sdr__scan_band`), each call through `/v1/deck/tools/invoke`.
- **`deck.py`**: Mode B, the local cyberdeck operator — a chat on host `deck` gives the LLM the
  daemon's tools as function calls, executed only via the daemon.
- New routes for the HUD: `GET /v1/hosts/{id}/screen.jpg` (live VNC frame, gated as
  `screenshot`), `POST /v1/hosts/{id}/input` (click/move/scroll/type/key, each gated),
  `POST /v1/voice/ptt`.
- Every event and JSON response is redacted (`redact_json`), log lines through a redacting formatter.

Rust follow-up made here: a request that stops waiting on the gate (caller dropped) now withdraws
its approval at once (`approval_resolved: cancelled`) instead of lingering until the timeout
(`policy::tests::an_abandoned_request_is_withdrawn`).

Verification (real output):

| Gate | Result | Output |
| --- | --- | --- |
| `cd harness && uv run ruff check .` | PASS | `All checks passed!` |
| `cd harness && uv run ruff format --check .` | PASS | `69 files already formatted` |
| `cd harness && uv run pytest` | PASS | `288 passed, 1 xfailed` (257 ported + 31 new, incl. 3 integration tests against `target/debug/lapis-daemon --mock-hw`); the xfail is inherited from Lapis-Zero (`test_ssh_host`: a command whose parent ended before the stop is not found) |
| `cd harness && uv run mypy` (strict) | PASS | `Success: no issues found in 41 source files` |
| Rust gate after the gate change | PASS | fmt clean, clippy clean, `cargo test --workspace` 48 passed, 0 failed |

End-to-end run on this Mac (mock daemon + `lapis-harness --mock` + HUD): `/v1/health` reports
`harness: true`; a deck chat ran `gps.get_fix`, `sdr.scan_band`, `mesh.list_nodes` through the
gate; a fleet chat on `macbook-pro` raised a **Tier 2** approval for a risky VNC click, shown on the
HUD with the screenshot and click point (hold-to-confirm), denied from the API and audited
(`approval mouse_click deny device`); `screen.jpg` returned a 28 KB JPEG through the daemon; probe
listed Claude Code / opencode / Freya on the mock host.

Hardware/remote-only items deferred: real VNC/SSH hosts on a tailnet, each agent CLI's current
flags on real hosts, an LLM provider key for non-mock planning.

## Phase 6 — OS Image Builder & Flashing Pipeline ✅ (2026-11-25)

`os-image/` (see its README):
- `build.sh`: aarch64 release binaries in an arm64 trixie container (`scripts/linux-check.sh
  release`: `lapis-daemon --features bluez`, `lapis-ui --no-default-features --features kms`),
  harness wheel (`uv build`), pi-gen = **ak-rex/ClockworkPi-pi-gen** (trixie, `akrex-kernel`,
  `clockworkpi-uconsole-cm4` overlay, audio pop fix, AXP228 charge rule) pinned at
  `4c90542730df02de39ae97ca1139ec753624cd37`, stages 0–2 + `stage-lapis`, then
  `lapis-deck-cm4-trixie-arm64.img.xz` + `.sha256` + Raspberry Pi Imager `os-list.json`
  (`imager-manifest.py`, self-tested).
- `stage-lapis/`: `00-system` (config.txt Lapis block: `enable_uart=1`, `dtparam=i2c_arm=on`,
  `dtoverlay=i2c-rtc,pcf85063a`, `dtoverlay=pps-gpio,gpiopin=6`, `dtoverlay=spi1-1cs`, KMS from the
  base's `vc4-kms-v3d[-pi4]`; `console=serial0,115200` stripped; quiet boot; multi-user; getty off
  tty1/ttyS0; volatile journal), `01-packages` (gpsd/PPS/chrony, rtl-sdr, rtl-433, multimon-ng,
  HackRF/Airspy/SoapySDR, tcpdump/nmap, BlueZ, Mesa GBM/EGL; Tailscale + meshtasticd repos;
  meshtasticd config for the AIO SX1262; readsb + rx_tools from source), `02-lapis-stack`
  (binaries, harness venv, link check), `03-services` (units, configs, `00-lapis.preset` = `disable *`,
  build-time checks incl. "no X11/desktop package installed").
- Units: `lapis-rails` (AIO V2 rails 27/16/7/23 at boot, from `deck.toml`), `lapis-daemon` (root,
  group `lapis`, umask 027), `lapis-harness` (user `lapis`, `/run/lapis/harness.sock`), `lapis-ui`
  (user `lapis`, video/render/input, DRM master), `lapis-firstboot` (radios, seed import),
  `lapis-tailscale-up` (seeded key + `tailscale serve --bg --https=443 http://127.0.0.1:8421`,
  verified by reading it back), `readsb` (installed, off), drop-ins ordering gpsd/meshtasticd after
  the rails. Account `lapis` has no password; remote shell = Tailscale SSH.
- `flash.sh`: refuses non-removable disks, typed confirmation, `sudo dd` to `/dev/rdiskN`, then
  rootfs verified byte for byte and the boot partition file by file (Spotlight-safe).
- `HARDWARE_CHECKLIST.md`: what only the real uConsole can confirm.

Verification:

| Gate / check | Result | Output |
| --- | --- | --- |
| `shellcheck os-image/**/*.sh` (zsh glob, 10 scripts) | PASS | `shellcheck os-image/**/*.sh: PASS` |
| `00-system/01-run.sh` on ak-rex's real `config.txt`/`cmdline.txt` (trixie container), run twice | PASS | serial console removed, Lapis block appended once, getty masked, multi-user |
| All packages + Tailscale/meshtasticd repos, `apt-get install -s` on trixie arm64 | PASS | 381 packages resolve (tailscale 1.104.7, meshtasticd 2.7.26.61~beta, gpsd 3.25, rtl-433 25.02) |
| `systemd-analyze verify` of the Lapis units (trixie container) | PASS | no findings for our units |
| `scripts/linux-check.sh release` | PASS | `lapis-daemon` 8.7 MB, `lapis-ui` 5.1 MB, ELF aarch64, stripped; NEEDED only libdbus-1 / libgbm + libc (all installed by the image) |
| `imager-manifest.py --self-test` | PASS | `imager-manifest self-test: ok` |
| Full `os-image/build.sh` (pi-gen) | **not run** | this Mac has 20 GiB free (96 % full); a trixie pi-gen build needs ~15–20 GB of intermediate rootfs copies. Run it with more free space: `os-image/build.sh` |

Hardware-only items: all of `os-image/HARDWARE_CHECKLIST.md` (boot time < 12 s, panel rotation,
rails, PPS/GPIO6 vs the overlay's SPI4, radios, first-boot BLE pairing, tailscale serve).

## Phase 5 — Android Companion App ✅ (2026-11-25)

`companion-android/` (built by a background agent from my brief, re-verified here with a fresh,
uncached Gradle run): **the existing Lapis-Zero app**, same applicationId/namespace
`dev.lapiszero.app`, same versionCode, same debug signing certificate — `adb install -r` upgrades the
installed app in place and keeps its pairings; with a Lapis-Zero CardputerZero it behaves exactly
as before (a smoke test asserts no `/v1/deck/*` call is made). It learns the device kind from the BLE
name (`LapisZero-` / `LapisDeck-`), `/v1/health` and every `state_sync`, stored with the pairing.
- **Deck tab** (only for a Lapis-Deck, with an unseen-alerts badge): rails (Wi-Fi/BT off asks
  first) and AXP228 draw; GPS fix; mesh nodes and channel chat (sending = `mesh.send_message`
  through `tools/invoke`, answered by the phone's own approval sheet); SDR status, live spectrum,
  tune presets, quick decoders; quick NetOps; live mesh/SDR alert feed; audit log.
- **Approvals**: tier badge, reason, source, expiry; **Tier 2 = 1.5 s hold** (no tap, no "approve for
  task"); a Tier 2 notification only opens the app and the receiver refuses a quick approve for it.
- BLE crypto files untouched; `BleEnvelopeCryptoVectorTest` reads `uConsoleCM4/tests/vectors/`.

Verification (fresh run, `./gradlew test assembleDebug --rerun-tasks`, JDK 21):

| Gate | Result | Output |
| --- | --- | --- |
| `cd companion-android && ./gradlew test assembleDebug` | PASS | `BUILD SUCCESSFUL in 58s`, 51 tasks executed; debug unit tests **309 run, 299 passed, 0 failed, 10 skipped** (opt-in integration tests) |
| BLE vector parity (`BleEnvelopeCryptoVectorTest`) | PASS | 19 tests, 0 failures, against the same `tests/vectors/ble_crypto_vectors.json` as Rust |
| Against the real Rust daemon (`DeckDaemonIntegrationTest`, opt-in, agent run on port 18441) | PASS | 2/2: WS frames, quick actions, Tier 1 approve, Tier 2 deny, rails, tune |
| APK | built | `companion-android/app/build/outputs/apk/debug/app-debug.apk` (52.4 MB) |

Deferred: install on the Pixel (67240DLKX0092Z was not connected over adb) and pair with a real
deck; background notifications for mesh messages.

## Final verification — all AGENTS.md §3 gates (`scripts/verify.sh`, 2026-11-25)

```
PASS   cargo fmt --check
PASS   cargo clippy --workspace --all-targets -- -D warnings
PASS   cargo test --workspace          (48 passed, 0 failed)
PASS   uv run ruff check .             (All checks passed!)
PASS   uv run pytest                   (288 passed, 1 xfailed)
PASS   ./gradlew test assembleDebug    (309 run, 299 passed, 0 failed, 10 skipped)
PASS   shellcheck os-image/**/*.sh scripts/*.sh
```

Also passing: `uv run mypy` strict (0 issues, 41 files); the arm64 Debian trixie container with the
Linux-only features (`LAPIS_LINUX_FEATURES=lapis-daemon/bluez,lapis-ui/kms scripts/linux-check.sh check`).
Open follow-ups (beads): `ankur-3hf.8` full pi-gen image build (needs ~20 GB free), `ankur-3hf.9`
hardware checklist on a real uConsole, `ankur-3hf.10` Pixel install + pairing check.

## Phase 6 addendum — image built (2026-11-25)

After `cargo clean` freed 16.4 GiB (34 GiB free), `os-image/build.sh` ran to completion in Docker
(native arm64; a watchdog would have stopped pi-gen below 5 GiB free — it never fired; 30 GiB free
afterwards). Two problems found and fixed on the way:
1. pi-gen refuses `DISABLE_FIRST_BOOT_USER_RENAME=1` without `FIRST_USER_PASS`. `build.sh` now
   appends a random throwaway (`openssl rand -hex 24`, never stored or printed) to the copied config,
   and `stage-lapis` locks the account (`passwd -l lapis`, checked at build time).
2. The image's own build-time import check caught that Debian's `python3-aiohttp` 3.11 (pulled in by
   `--system-site-packages`) lacks `web.RequestKey` (aiohttp ≥ 3.14). `harness/pyproject.toml` now
   declares `aiohttp>=3.14` and the package was dropped from the image list (this image still carries
   the unused Debian copy from the first pass; the harness venv has its own aiohttp ≥ 3.14).

Artifacts (`os-image/deploy/`, gitignored):

| File | Value |
| --- | --- |
| `lapis-deck-cm4-trixie-arm64.img.xz` | 835,416,564 bytes, `shasum -a 256 -c` OK, `xz -t` OK |
| raw image | 4,194,304,000 bytes (512 MiB bootfs + 3,480 MiB rootfs; grows on first boot) |
| download SHA-256 | `fb1852810cec94ec9982e64d3a9b649b9f0ed04fb6b077001071b3d3b3502a59` |
| raw SHA-256 | `503264254381f866b5d268ca28e6737a42319025aab27711b1336802fe78b2f6` |
| `os-list.json` | Raspberry Pi Imager manifest, device `pi4-64bit` |

Inspected inside the image (debugfs/mtools in a container, no root on the Mac): `cmdline.txt` has no
serial console and keeps `fbcon=rotate:1`; `config.txt` ends with the Lapis block; `kernel8.img`,
`initramfs8` and `overlays/clockworkpi-uconsole-cm4.dtbo` present; `akrex-kernel 7.3.1-v8-16k+`,
tailscale 1.104.7, meshtasticd 2.7.26.61~beta, gpsd 3.25, rtl-433 25.02; `/usr/local/bin/{lapis-daemon,
lapis-ui,readsb,rx_sdr}`, `/opt/lapis/harness/bin/{lapis-harness,lapis-mcp}`; enabled: lapis-rails,
lapis-firstboot, lapis-daemon, lapis-harness, lapis-ui, lapis-tailscale-up, tailscaled, gpsd,
meshtasticd, chrony; getty@tty1 masked; `lapis` locked; hostname `lapis-deck`; 0 X11/desktop packages.

Gates re-run after these changes (`scripts/verify.sh`): all 7 PASS (pytest `288 passed, 1 xfailed`).
Not done: flashing a card and booting it (needs the SD card and your sudo password) — `ankur-3hf.9`.

## First boot on the uConsole — fixes (2026-11-25)

**Blank screen after the boot log.** The HUD exited before the panel driver was ready and systemd's
start limit then gave up. `lapis-ui` now waits up to 60 s for a DRM card that is not `simpledrm`,
picks a connected connector (or one in Unknown state that has modes), prefers XRGB8888 and falls back
to ARGB8888, and writes any failure to `/dev/tty1` in red. `lapis-ui.service`:
`StartLimitIntervalSec=0`, `RestartSec=3`. New `lapis-bootreport.timer` (45 s after boot, then every
3 min) writes `lapis-boot-report.txt` to the card's bootfs (units, DRM state, journals with Tailscale
keys and login URLs removed), so a failed boot can be read on any computer.

**Could not save the AI provider or add hosts** (HTTP 403, PermissionError). The harness runs as
`lapis` and saves `lapis.toml`/`hosts.toml` by write-then-rename, which needs a writable directory;
`/etc/lapis` was `root:root 0755`. Now `root:lapis 1775` (sticky: the harness can replace its own
files, never root's `deck.toml`), checked at image build time. Reproduced in a container before and
after: PermissionError → saved; replacing `deck.toml` as `lapis` is still refused.

**HUD v2: navigation and look** (after the styleguide images the user sent):
- Layout: top bar with seven big workspace tabs (icon, name, Alt+number, approval badge) between
  `SEL ◀` and `▶ START` key caps; a left sidebar listing the workspace's sections; the content;
  a guide bar that always shows what each button does right now (SIDEBAR/CONTENT, A/B/X/Y, ▲▼, ◀▶).
- One focus rule everywhere: SELECT/START (or Alt+1..7, or a tap) change workspace; ▲▼ choose a
  section; A or ▶ enter it; B or ◀ come back (unless a field or a ◀ value ▶ choice is using them);
  Y commands; SELECT+START kill switch. A breadcrumb says where you are.
- Help overlay (F1, `?`, or "Controls & navigation help" in commands) with a map of the three zones
  and a gamepad/keyboard table. It opens by itself on the device's first boot, once (marker
  `/var/lib/lapis/harness/.ui-help-seen`).
- Styleguide palette (#AAB1B8 / #F0F4F7, cyan #00FFFF focus, red alerts), chamfered title tabs,
  2 px borders with a soft glow, bigger text (body 17, rows 40–52 px).
- Sections: AGENT (New chat, chats), RF (Waterfall, Presets, Decoders), MESH (Nodes, Messages, GPS),
  NET (peers, Wi-Fi, BLE, probe, capture, DNS), FLEET (Add host, hosts), PTY (terminals, New shell,
  Serial console with port/baud picker), SYSTEM (AI provider, rails, display, pairing, phones,
  audit, power). Every action is reachable with the D-pad; FLEET's SSH/live screen/passthrough/
  probe/remove are rows, not mouse-only buttons.
- **SYSTEM › AI provider**: provider presets (the companion app's list), protocol, model, effort,
  base URL, key name, and a write-only API key field (PATCH `/v1/secrets`, then PUT `/v1/config`
  with the rest of lapis.toml sent back unchanged). Shows SET/NOT SET and the masked tail only.
- **FLEET › Add host**: ◀▶ picks a tailnet peer not configured yet and fills id, hostname, IP and OS;
  SSH user/port, VNC port, policy; PUT `/v1/hosts` with the other hosts kept and the built-in deck
  left out. "Remove host" asks for a second A.
- **Character**: the uConsole gets its own animated hooded robot (grey knit hoodie, black visor, two
  glowing eyes, peach drawstrings and shoes), drawn with egui shapes. It blinks, glances, bobs and
  changes with the agent: READY, THINKING (dots), WORKING (cyan eyes), NEEDS YOU (red, "!"),
  LISTENING (sound bars), DONE (happy eyes, 4 s), OFFLINE (dim, z z z). Shown in the sidebar, on the
  AGENT home and in the approval prompt. The Lapis character on the CardputerZero and in the app is
  unchanged.
- Fixed on the way: in a form, the A that started editing a field also ended it in the same frame;
  pad events are now ignored on egui's second layout pass (a new Grid reruns the frame).

Checks: `cargo test -p lapis-ui` 14 passed (new: AI provider save sends PUT config + PATCH secrets
and clears the key; add host from the tailnet sends PUT hosts keeping the others and never `deck`;
A/B sidebar↔content; palette and F1 open the help). The arm64 trixie container check: 53 passed,
clippy `-D warnings` clean. Against the real harness in mock mode through the daemon with the HUD's
token: PUT `/v1/config` (→ openrouter/high), PUT `/v1/hosts` (+build-box), PATCH `/v1/secrets` all
accepted. Screenshots of every workspace and the help taken with `lapis-ui --mock-hw --screenshot`
(new flag `--show-help`).

Image rebuilt with all of the above (`os-image/build.sh`, 2026-11-25 21:47; the build-time check of
`/etc/lapis` = `1775 root:lapis` passed): `lapis-deck-cm4-trixie-arm64.img.xz` 835,324,716 bytes,
SHA-256 `5e173f017e18336875fbf372d5431fe742bd82b3bc02775230bb590ecc7a46e3`, `shasum -c` and `xz -t`
OK; the staged `lapis-ui` is byte-identical to the fresh aarch64 KMS build. Gates before the build:
`scripts/verify.sh` all 7 PASS. The first boot's `lapis-boot-report.txt` (read before reflashing):
HUD on screen 15 s after boot, uConsole keyboard/gamepad found, BLE pairing → Wi-Fi → Tailscale done;
`meshtasticd.service` failed (LoRa config, follow-up). Reflashing wipes that pairing: pair again.

## meshtasticd on the AIO board + boot splash (2026-11-25)

- **meshtasticd** (`loaded failed failed` on the first boot; that report had no journal, so the
  cause is read from meshtasticd 2.7.26's source, not seen). It exits on two paths this image hits:
  no `MACAddress`, so it reads the Bluetooth hci0 address, which is zero until the adapter is up
  (wait-online is masked, so it starts very early): "Blank MAC Address not allowed!"; and "No
  sx1262 radio" when the LoRa rail is off. systemd gives up after 5 restarts (15 s). Fix: the
  drop-in starts it through `lapis-meshtasticd.sh` with `--hwid` from the CM4 serial (node ID = the
  serial's last 8 hex digits), and only when `/run/lapis-rails/lora` exists (`lapis-rails` writes it
  once GPIO16 is high, or for a V1 board; `board = "none"` skips the unit). The SX1262 config
  (`config.d/lora-hackergadgets-aio.yaml`) keeps the vendor guide's pins (SPI1 CE0, IRQ 26, Busy 24,
  Reset 25, DIO2 RF switch, DIO3 TCXO), now cited. Build checks: the drop-in passes
  `systemd-analyze verify`, the config is present, user `meshtasticd` is in `spi` and `gpio`. The
  boot report has a `meshtasticd` section (state, restarts, rail flag, `/dev/spidev*`, 60 journal
  lines). Bead `ankur-3hf.12`.
- **Boot splash**: Plymouth (`script` plugin) with a Lapis theme: the faceted lapis gem in the HUD's
  palette with gold flecks, "LAPIS DECK" in Hack, a breathing glow and a running light. It is turned
  onto the portrait panel the way the HUD is (knob `TURN` in `lapis.script`). `lapis-ui` quits it
  with `--retain-splash` before taking DRM master, and plymouth-quit waits for the HUD, so the
  splash stays up until the HUD's first frame. Art is generated by `os-image/make-splash.py`
  (committed PNGs). Not yet seen on hardware: rotation and the hand-off are checklist items.
- **LoRa region from deck.toml**: `mesh_region = "US"` (Meshtastic RegionCode names; default empty
  = leave the radio alone). After each config sync the daemon reads the radio's LoRaConfig and, if
  the region differs, sends its own node an admin `set_config` with the received LoRaConfig plus
  the new region appended (last value wins, so nothing else changes). meshtasticd accepts local
  admin (`from` 0) and restarts to apply it; the daemon reconnects. The MESH title shows the region
  and says `region UNSET, no TX` with a hint when none is set (a new radio does not transmit).
  `deck.toml` can be seeded at first boot (`lapis-seed/deck.toml`), since on a running deck it is
  root's and `lapis` has no sudo; `lapis-rails` now runs after that import. Tests: region patch
  keeps other LoRa fields, the fake-meshtasticd session gets the admin packet after sync, the
  shipped deck.toml parses. Not yet tried against a real meshtasticd.
- **Image rebuilt** with the three changes above (`os-image/build.sh`, 2026-11-25 22:35, exit 0):
  `lapis-deck-cm4-trixie-arm64.img.xz` 842,502,588 bytes, SHA-256
  `b14f9429102a978c87cf4221fd46e1b20ecbd79c7d0d16c4ed61dfd507580f2c`; `shasum -c` and `xz -t` OK.
  Packages: plymouth 24.004.60-5, meshtasticd 2.7.26.61~obs54e0d8d~beta. stage-lapis's build-time
  checks passed (meshtasticd drop-in verifies, radio config present, meshtasticd in spi/gpio, Lapis
  Plymouth theme selected, `splash` on the cmdline). The staged lapis-daemon / lapis-ui are fresh
  aarch64 builds carrying the region code. Gates before: `scripts/verify.sh` 7/7 PASS.
