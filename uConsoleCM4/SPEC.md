# Lapis-Deck OS — Technical Specification

> **Codename:** `lapis-deck`  
> **Target Hardware:** ClockworkPi uConsole (v3.14 rev 5 mainboard) + Raspberry Pi CM4 (4 GB / 8 GB LPDDR4, ARM64 Cortex-A72, VideoCore VI GPU)  
> **License:** Apache-2.0  
> **Philosophy:** Zero desktop environment. Zero X11. Boot straight into a 60 FPS GPU-accelerated Cyberdeck HUD powered by a local-first agentic harness that both **operates the cyberdeck's own radios/interfaces** and **herds your Tailscale fleet**.

---

## 1. Hardware Target & Peripheral Matrix

### 1.1 Core Platform (ClockworkPi uConsole CM4)
- **Compute:** Raspberry Pi CM4 (Quad-core ARM Cortex-A72 @ 1.5 GHz, 4 GB or 8 GB RAM, onboard 802.11ac Wi-Fi + Bluetooth 5.0 BLE)
- **Display:** 5.0" 1280×720 IPS MIPI-DSI panel (plus Micro-HDMI output)
- **Input:** 74-key backlit QWERTY keyboard (USB HID), mini optical trackball, gamepad keys
- **Power & Telemetry:** Dual 18650 cells managed by AXP228 PMIC (`/sys/class/power_supply/axp20x-battery/`), USB-C charging
- **Audio:** 3.5 mm TRRS headphone/mic jack + internal stereo speakers (with shutdown pop-fix)
- **I/O:** 1× external USB 2.0, 2× internal USB 2.0, 40-pin GPIO FPC, 52-pin Mini PCI-E expansion slot

### 1.2 Supported Expansion Modules (Auto-Detected)
1. **HackerGadgets uConsole AIO V1/V2 Extension Board:**
   - **RTL-SDR** (internal USB, bias-tee toggle, HF direct sampling 100 kHz – 28.8 MHz)
   - **LoRa SX1262** (860–960 MHz, up to 22 dBm, TCXO, SPI via `meshtasticd`)
   - **GPS/GNSS** (`/dev/ttyS0` @ 9600 baud, PPS on `GPIO6`, power rail on `GPIO27`)
   - **RTC** (`PCF85063A` via `dtoverlay=i2c-rtc,pcf85063a`)
   - **Internal USB Hub Power Rail** (`GPIO23`)
2. **External USB SDRs (via SoapySDR / `librtlsdr`):** RTL-SDR Blog v3/v4, HackRF One, Airspy, PlutoSDR.
3. **Optional ClockworkPi 4G LTE CAT4 Module:** ModemManager / QMI cellular uplink + GNSS fallback.

---

## 2. Base OS & Image Build Pipeline

- **Base Distro:** Debian 13 (`trixie`) `arm64` Lite (minimal headless rootfs; **no X11, no GNOME/KDE/XFCE, no Display Manager**).
- **Kernel & BSP:** `ak-rex` uConsole CM4 kernel (Linux 6.12 LTS / 7.x with out-of-tree ClockworkPi display, keyboard, AXP228 PMIC, and speaker pop-fix drivers).
- **Boot & Graphics Path:**
  - `systemd` boots directly into `lapis-daemon.service` and `lapis-ui.service` on `tty1` via **DRM/KMS + `libseat` / `wgpu` (Vulkan/GLES3 on VideoCore VI)**.
  - Boot-to-UI target: **< 12 seconds** from power-on.
  - `console=serial0,115200` stripped from `cmdline.txt` to keep `/dev/ttyS0` free for GPS.
  - `/boot/firmware/config.txt` pre-configured with `enable_uart=1`, `dtparam=i2c_arm=on`, `dtoverlay=i2c-rtc,pcf85063a`, `dtoverlay=pps-gpio,gpiopin=6`, and `dtoverlay=vc4-kms-v3d`.
- **Image Artifact:** Reproducible builder (`os-image/`) outputting a flashable `lapis-deck-cm4-trixie-arm64.img.xz` + SHA-256 checksum + Raspberry Pi Imager manifest.
- **Hardware Power Controls:** Daemon exposes clean GPIO toggles (`pinctrl 27` for GPS rail, `pinctrl 23` for internal USB hub, RTL-SDR bias-tee, Wi-Fi/BLE RF-kill) directly in the UI top bar and agent toolset.

---

## 3. System Architecture

```text
┌──────────────────────────────────────────────────────────────────────┐
│                  lapis-ui (Rust · DRM/KMS · 1280×720)                │
│  [Command/Agent] [RF/SDR] [Mesh/GPS] [NetOps] [KVM/Fleet] [PTY] [Sys]│
└───────────────────────────────▲──────────────────────────────────────┘
                                │ Unix Socket / Local WS (127.0.0.1:8421)
┌───────────────────────────────▼──────────────────────────────────────┐
│                   lapis-daemon (Rust · tokio)                        │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐ ┌─────────────┐  │
│  │ Control API  │ │ Policy Gate  │ │ BLE + QR     │ │ Hardware &  │  │
│  │ (HTTPS/WSS   │ │ (Deny-by-    │ │ Provisioning │ │ Power Mgr   │  │
│  │ via tailscale│ │  Default)    │ │ (X25519/GCM) │ │ (AXP228/GPIO│  │
│  │  serve)      │ │              │ │              │ │  /RF-Kill)  │  │
│  └──────┬───────┘ └──────┬───────┘ └──────────────┘ └─────────────┘  │
└─────────┼────────────────┼───────────────────────────────────────────┘
          │                │
          ▼                ▼
┌───────────────────┐  ┌───────────────────────────────────────────────┐
│ Android Companion │  │       lapis-harness (Python / Rust MCP)       │
│ (Kotlin · BLE/QR  │  │  • Fleet Shepherd: VNC/SSH KVM over Tailscale │
│  + Tailscale WSS) │  │  • Remote Harness Bridge: Claude Code,        │
│                   │  │    opencode, Freya, Gemini CLI, Cursor, Codex │
│                   │  │  • Local Cyberdeck Tools: SDR/RF, Meshtastic, │
│                   │  │    GPS, NetOps, Serial/UART, Local PTY        │
└───────────────────┘  └───────────────────────────────────────────────┘
```

---

## 4. Custom Lightweight Cyberdeck UI (`lapis-ui`)

- **Stack:** Rust + `wgpu` / `egui` (or `slint` on `linuxkms`) rendering directly to DRM/KMS at native **1280×720 @ 60 FPS** with zero window manager overhead.
- **Visual Language:** High-contrast tactical utilitarian HUD (dark obsidian background, crisp monospace typography, phosphor-cyan / amber / crimson status accents, zero decorative bloat).
- **Navigation:** 100% keyboard-first (`Alt+1..7` workspace switching, `Ctrl+Space` universal command/agent palette, trackball for waterfall tuning, VNC pointer, and pane focus).
- **Persistent HUD Top Bar:** Battery % & draw (W) from AXP228, CPU temp/load, Wi-Fi/LTE/Tailscale status, active RF rails (SDR / LoRa / GPS lock), active agent status.

### 4.1 Workspaces (`Alt+1` through `Alt+7`)
1. **`[1] AGENT` (Command Center):** Split view—streaming agent conversation & tool-call trace on left, live artifact/output viewer (JSON, decoded packets, screenshots, diffs) on right. Supports keyboard input + push-to-talk voice (`Fn+Space` or headset button).
2. **`[2] RF / SDR`:** Hardware-accelerated real-time **FFT spectrum + scrolling waterfall** (SoapySDR / `librtlsdr`), frequency/gain/sample-rate controls, demodulator presets (WFM, NFM, AM, USB/LSB), and one-key decoder overlays (`rtl_433` ISM sensors, `readsb` ADS-B aircraft table, `multimon-ng` POCSAG/APRS).
3. **`[3] MESH / GPS`:** Native `meshtasticd` client (channel chat, node list with SNR/RSSI/hops, traceroute) + `gpsd` telemetry (lat/lon, altitude, satellite skyplot, PPS precision clock status).
4. **`[4] NETOPS`:** Passive Wi-Fi/BLE monitor & survey table, active Tailscale peer status, quick `nmap`/`tshark` live capture pane, DNS/HTTP diagnostics.
5. **`[5] FLEET / KVM`:** Tailscale host roster, live VNC viewport (trackball + keyboard passthrough), remote SSH terminal, and **Remote Harness Selector** (dispatch tasks directly to `claude`, `opencode`, `freya`, `gemini`, `cursor-agent`, or `codex` on any reachable tailnet host).
6. **`[6] PTY / MUX`:** Native GPU-accelerated multi-pane terminal emulator (embedded `portable-pty` + `alacritty_terminal` / `vt100`) with split panes, scrollback, and UART serial console mode (`/dev/ttyUSB*`, `/dev/ttyACM*`).
7. **`[7] SYSTEM`:** Power rail toggles (GPS, USB Hub, Bias-Tee, 4G, Wi-Fi/BLE), keyboard backlight & screen brightness, Tailscale/Companion pairing QR screen, audit log viewer, and safe shutdown/reboot.

---

## 5. Enhanced Agentic Harness (`lapis-harness`)

### 5.1 Mode A — Fleet Shepherd (Lapis-Zero Parity+)
- Discovers peers on the Tailscale tailnet (`tailscale status --json`).
- Operates remote machines over **VNC** (capture frame, vision-guided click/type/scroll) and **SSH** (zero-agent target requirement—uses only what is already installed).
- **Remote Harness Discovery & Delegation:** Probes target host for installed CLI harnesses (`claude`, `opencode`, `freya`, `gemini`, `cursor-agent`, `codex`), routes prompts with session continuity, streams responses back to `lapis-ui` and the Android Companion.

### 5.2 Mode B — Local Cyberdeck Operator (Tool-Calling MCP Runtime)
The agent has structured, typed access to the uConsole's own hardware through local MCP tool servers:
- `sdr.*`: `list_devices`, `scan_band(start_hz, end_hz, step_hz)`, `capture_iq(freq_hz, duration_s)`, `decode_ism(freq_hz, duration_s)`, `adsb_snapshot()`, `demod_audio(freq_hz, mode)`.
- `mesh.*`: `list_nodes`, `read_messages`, `send_message(channel, text)`, `traceroute(node_id)`.
- `gps.*`: `get_fix`, `get_satellites`.
- `net.*`: `tailscale_peers`, `wifi_scan_passive`, `ble_scan_passive`, `port_probe(target, ports)`, `packet_capture(iface, bpf, count)`.
- `hw.*`: `battery_status`, `gpio_rail(name, state)`, `serial_probe(device, baud)`, `pty_exec(cmd)`.

### 5.3 Model Routing & Offline Fallback
- **Cloud / Tailnet LLM Providers:** Configurable OpenAI-compatible, Anthropic, Gemini, or self-hosted tailnet Ollama/vLLM endpoints (secrets stored encrypted, never logged).
- **Local Fallback:** Optional quantized small model (`llama.cpp` server, e.g., 1.5B–3B Q4_K_M) for offline command synthesis when off-grid.

### 5.4 Deny-by-Default Security & RF Safety Policy
Every tool call passes through `lapis-daemon`'s **Policy Gate** before execution:
- **Tier 0 (Auto-Allow, Read-Only / Passive):** SDR receive/decode, GPS read, passive Wi-Fi/BLE scan, battery/system status, Tailscale status, local file read.
- **Tier 1 (Confirm on Device — Single Keystroke `[Y/N]`):** Active network scans (`nmap`), SSH non-destructive commands on fleet hosts, VNC pointer/keystroke batches, sending LoRa/Meshtastic text messages.
- **Tier 2 (Explicit Hold-to-Confirm + Audit Log):** Destructive shell commands, RF transmit (HackRF TX, if attached), GPIO rail changes, credential/key rotation, Tailscale ACL/exit-node changes.
- **Secret Redaction:** All API keys, bearer tokens, Tailscale auth keys, and SSH private keys are scrubbed from logs, UI transcripts, and WebSocket broadcasts before serialization.

---

## 6. Tailscale Control Plane & Mobile Companion (Lapis-Zero Protocol Parity)

### 6.1 Daemon Control Plane (`lapis-daemon`)
- Listens strictly on `127.0.0.1:8421` (never binds `0.0.0.0`).
- Exposed to the user's tailnet exclusively via `tailscale serve --bg --https=443 http://127.0.0.1:8421` (HTTPS + WSS).
- Authenticated via per-device **Bearer Token** (256-bit CSPRNG, hashed with SHA-256 at rest).
- REST + WebSocket API for: device status, chat/agent stream, SDR waterfall/decoder summaries, Meshtastic relay, KVM screenshots, and remote power/rail control.

### 6.2 BLE + QR First-Boot Provisioning
Identical cryptographic handshake to Lapis-Zero for zero-friction headless setup:
1. Device generates ephemeral **X25519** keypair + 6-digit visual **PIN** displayed as a QR code on the 720p screen (`[7] SYSTEM` or first-boot wizard).
2. Android Companion scans QR, connects over **BLE GATT**, exchanges public keys, derives session key via **HKDF-SHA256**, and encrypts payloads with **AES-256-GCM** (authenticated with the 6-digit PIN).
3. Companion pushes Wi-Fi credentials, Tailscale auth key (or login trigger), and initial LLM provider config.
4. Device joins Wi-Fi + Tailscale, configures `tailscale serve`, returns its `https://<hostname>.<tailnet>.ts.net` URL + scoped Bearer Token, and tears down BLE provisioning.
5. **Deterministic Test Vectors:** Shared `tests/vectors/ble_crypto_vectors.json` validated by both Rust (`lapis-daemon`) and Kotlin (`companion-android`) test suites.

### 6.3 Android Companion App (`companion-android`)
- Kotlin + Jetpack Compose.
- Features: BLE/QR onboarding wizard, Tailscale WSS dashboard, remote agent chat (including voice input routed to uConsole), live SDR/Mesh alert feed, and remote Tailscale fleet quick-actions.

---

## 7. Repository Layout

```text
lapis-deck/
├── SPEC.md
├── AGENTS.md
├── Cargo.toml                    # Rust workspace (lapis-daemon, lapis-ui, lapis-sdr, lapis-crypto)
├── crates/
│   ├── lapis-crypto/             # X25519, HKDF-SHA256, AES-256-GCM, secret redaction
│   ├── lapis-daemon/             # Control plane (127.0.0.1:8421), Policy Gate, BLE GATT, HW/GPIO
│   ├── lapis-sdr/                # SoapySDR/RTL-SDR DSP, FFT waterfall, decoder adapters
│   └── lapis-ui/                 # DRM/KMS 1280×720 Cyberdeck HUD (7 workspaces)
├── harness/                      # Python/uv agentic harness + MCP tool servers + Fleet KVM/SSH
│   ├── pyproject.toml
│   ├── src/lapis_harness/
│   └── tests/
├── companion-android/            # Kotlin Jetpack Compose Android companion app
├── os-image/                     # Debian 13 Trixie arm64 + ak-rex kernel image builder & systemd units
└── tests/
    └── vectors/
        └── ble_crypto_vectors.json
```

---

## 8. Phased Execution Roadmap

- **Phase 0 — Workspace & Crypto Foundations:** Initialize repo, Rust workspace, Python `uv` project, `ble_crypto_vectors.json`, and `lapis-crypto` (passing deterministic vector tests).
- **Phase 1 — Daemon Control Plane & Policy Gate:** Implement `lapis-daemon` (`127.0.0.1:8421` REST/WSS, bearer auth, secret redaction, Policy Gate Tier 0/1/2, mockable HW/AXP228/GPIO/BLE GATT layers).
- **Phase 2 — SDR & Cyberdeck Subsystems:** Implement `lapis-sdr` (SoapySDR/RTL-SDR + synthetic IQ mock source for CI, FFT engine, `rtl_433` / `readsb` / `multimon-ng` wrappers) and `meshtasticd` / `gpsd` / NetOps adapters.
- **Phase 3 — Enhanced Agentic Harness & Fleet KVM:** Build `lapis-harness` (MCP local cyberdeck tools, Tailscale VNC/SSH KVM, remote harness discovery for `claude`/`opencode`/`freya`/`gemini`/`cursor-agent`/`codex`, policy-gated execution loop).
- **Phase 4 — Custom Cyberdeck UI (`lapis-ui`):** Build the 1280×720 DRM/KMS (with desktop-window preview mode for host dev) 7-workspace HUD with live FFT waterfall, embedded PTY, VNC viewer, and keyboard-first navigation.
- **Phase 5 — Android Companion App:** Build `companion-android` (BLE/QR provisioning passing `ble_crypto_vectors.json`, Tailscale WSS client, chat/SDR/Mesh views, debug APK build).
- **Phase 6 — OS Image Builder & Flashing Pipeline:** Create `os-image/` scripts, `systemd` units, `config.txt`/`cmdline.txt` templates, `tailscale serve` bootstrap, and hardware verification checklist for uConsole CM4.