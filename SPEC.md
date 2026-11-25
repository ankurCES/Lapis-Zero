# Lapis-Zero OS: Standalone Flashable Agent OS for CardputerZero (CM0)

## 1. Project Overview & Goals

**Lapis-Zero OS** is a dedicated, appliance-grade **Debian Trixie (`arm64`) operating system image** (`lapis-zero-arm64.img.xz`) built specifically for the **M5Stack CardputerZero (Raspberry Pi Compute Module 0)**. Rather than installing an app on top of the stock M5Stack desktop image, users flash **Lapis-Zero OS** directly onto a microSD card using **M5 Imager** (`CardputerZero/m5stack-imager`), **Raspberry Pi Imager**, `rpiboot`, or `dd`.

On boot, the device launches straight into a minimal, monochrome, spring-animated **Lapis One-inspired UI** backed by an autonomous **Option C (Cable-Free Software Agent KVM)** daemon that observes and controls remote machines over **Tailscale** via on-demand **VNC** and **SSH**.

### Non-Negotiable Engineering Constraints

1. **Purpose-Built Headless OS Image:** Built via `pi-gen` (`CardputerZero/pi-gen`, branch `cardputerzero_v1.1` base + CardputerZero hardware BSP). Completely strip `stage3`--`stage5` desktop environments, X11, Wayland, `lightdm`, `wayvnc`, `APPLaunch.service`, `ModemManager`, and `armhf` multiarch bloat.
2. **First-Class M5 Imager Compatibility:** Output standard `.img.xz` artifacts plus a valid `os-list.json` manifest matching `CardputerZero/m5stack-imager` (`devices: ["m5-cardputerzero", "pi3-64bit"]`, `architecture: "armv8"`, `init_format: "systemd"`) so users can pre-configure Wi-Fi, SSH, locale, and hostname directly inside M5 Imager before flashing.
3. **Aggressive 512 MB RAM Optimization:** Lower `gpu_mem=16` in `/boot/firmware/config.txt` (reclaiming $\sim 112\text{ MB}$ compared to the stock `128 MB` desktop split, yielding $\sim 496\text{ MB}$ Linux-visible RAM). Keep total active RSS (OS + Tailscale + `lapis-ui` + `lapis-daemon`) under $220\text{ MB}$ peak, backed by $512\text{ MB}$ LZ4 `zram` swap (zero SD-card swapfile).
4. **Strict Process Isolation:** The real-time $60\text{ FPS}$ framebuffer UI (`lapis-ui`, Rust) and the LLM/KVM agent runtime (`lapis-daemon`, Python `asyncio`) run as isolated `systemd` services communicating over a local Unix Domain Socket (`/run/lapis/lapisd.sock`). Network, VNC, or LLM stalls must never drop a UI frame.
5. **Desktop-First Verifiability:** Every UI view, spring animation, onboarding screen, and IPC flow must run in a desktop SDL2 simulator (`--sim`) and support headless PNG snapshots (`--dump-frame`) before building or flashing an SD card image.

---

## 2. Target Hardware & CardputerZero BSP Interfaces

Lapis-Zero OS inherits the verified CardputerZero CM0 hardware BSP from `CardputerZero/pi-gen` while stripping all desktop layers:

| Subsystem | Hardware / Linux Interface | OS Image Configuration |
| :--- | :--- | :--- |
| **Compute** | Raspberry Pi CM0 (BCM2837, Quad Cortex-A53 @ $1\text{ GHz}$, `aarch64`), $512\text{ MB}$ LPDDR2 | Native `arm64`-only Debian Trixie rootfs; `gpu_mem=16`; $512\text{ MB}$ LZ4 `zram`. |
| **Display** | 1.9" ST7789v3 LCD, $320 \times 170$ (`panel-mipi-dbi` over SPI), `/dev/fb0` | Preserve `cardputerzero,st7789v_lcd.bin` firmware + `initramfs-tools` hook; `fbcon=map:off` for clean boot handoff to `lapis-ui`. |
| **Keyboard** | 46-key matrix via TCA8418 (`0x34` I2C1, IRQ `GPIO27`), `evdev` | Preserve TCA8418 DT overlay & `/usr/share/keymaps/tca8418_keypad_m5stack_keymap.map`; bind `evdev` by path or device name (`tca8418`). |
| **Sensors** | BMI270 6-axis IMU (`0x68`), RX8130CE RTC (`0x32`), BQ27220 fuel gauge | Preserve I2C overlays; wake display on lift via BMI270; poll BQ27220 via `/sys/class/power_supply/*` with low-battery safe shutdown. |
| **Audio** | ES8389 codec, MEMS mic, 1W speaker (AW8737A, `GPIO24`) | ALSA-only (no PulseAudio/PipeWire overhead); $16\text{ kHz}$ mono capture for PTT + optional 8-bit procedural UI chimes. |
| **Power & Storage** | $1750\text{ mAh}$ LiPo, microSD boot (`bootfs` FAT32 + `rootfs` ext4) | `noatime`, `/tmp` on `tmpfs`, volatile `journald` ($16\text{ MB}$ cap), USB autosuspend, adaptive UI frame pacing ($60 \to 1\text{ FPS}$). |

### Memory Budget ($\sim 496\text{ MB}$ Linux-Visible RAM with `gpu_mem=16`)

* **Headless Kernel + Minimal Systemd + `tailscaled`:** $\le 75\text{ MB}$ RSS
* **`lapis-ui` (Rust static/native binary):** $\le 15\text{ MB}$ RSS
* **`lapis-daemon` (Python `asyncio` + `Pillow` + `asyncvnc` + `asyncssh`):** $\le 90\text{ MB}$ steady / $\le 130\text{ MB}$ peak during screenshot downscale
* **Free Headroom + Page Cache:** $\sim 275\text{ MB}$ (+ $512\text{ MB}$ LZ4 `zram`)

---

## 3. System Architecture & Repository Layout

```text
┌─────────────────────────────────────────────────────────────────────────┐
│  Lapis-Zero OS Image (Debian Trixie arm64 — pi-gen + CardputerZero BSP) │
│  Boot: multi-user.target | gpu_mem=16 | zram (512MB LZ4) | tailscaled   │
│  First-Boot: M5 Imager systemd customization + /boot/firmware/lapis-seed│
├─────────────────────────────────────────────────────────────────────────┤
│  lapis-ui.service (Rust)                                                │
│  • 320x170 software renderer (Playdate / Sharp Memory LCD aesthetic)    │
│  • Damped spring physics engine + procedural pixel mascot               │
│  • Backends: SDL2 Desktop Simulator (3x) | --dump-frame | /dev/fb0      │
│  • Hardware I/O: TCA8418 evdev, BMI270 wake, BQ27220 battery, backlight │
└────────────────────────────────────▲────────────────────────────────────┘
                                     │ Unix Domain Socket (NDJSON)
                                     │ /run/lapis/lapisd.sock
┌────────────────────────────────────▼────────────────────────────────────┐
│  lapis-daemon.service (Python 3.12+ asyncio)                            │
│  • Agent Core: Computer Use loop (Anthropic / OpenAI / OpenRouter)      │
│  • Policy Gate: Risk classifier + dHash stuck detector + kill-switch    │
│  • Scheduler: Asyncio cron runner for unattended background jobs        │
│  • State Store: SQLite WAL (/var/lib/lapis/lapis.db)                    │
│  • Option C KVM Drivers (over Tailscale 100.x.y.z):                     │
│      1. VncDriver (asyncvnc): capture, downscale, coord map, HID inject │
│      2. SshDriver (asyncssh): fast zero-token CLI execution             │
│  • Voice Pipeline: ALSA PTT capture -> Whisper/STT API -> Task queue    │
└─────────────────────────────────────────────────────────────────────────┘
```

### Monorepo Directory Structure

```text
lapis-zero/
├── SPEC.md                             # This implementation blueprint
├── AGENTS.md                           # Coding-agent rules & verification commands
├── Makefile                            # sim, test, dump-frames, cross-ui, build-os, manifest
├── config/
│   ├── hosts.example.toml              # Tailscale target machines (VNC + SSH + policy)
│   ├── lapis.example.toml              # LLM provider, timeouts, theme, cron schedules
│   └── secrets.example.env             # Template for FAT32 bootfs seed or /etc/lapis/secrets.env
├── ui/                                 # Rust crate: lapis-ui
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs                     # CLI: --sim, --fb, --demo, --dump-frame, --view, --socket
│       ├── backend/
│       │   ├── mod.rs                  # DisplayBackend & InputBackend traits
│       │   ├── sdl_sim.rs              # Desktop SDL2 simulator (3x pixel scale + key map)
│       │   ├── fbdev.rs                # /dev/fb0 mmap RGB565 writer + stride-safe flush
│       │   └── hw_cm0.rs               # TCA8418 evdev, BQ27220 battery, BMI270 IMU, backlight
│       ├── gfx/
│       │   ├── canvas.rs               # 320x170 framebuffer, primitives, Bayer 4x4 dithering
│       │   ├── font.rs                 # Crisp bitmap pixel fonts (6x8, 8x14, 16x28 clock)
│       │   ├── spring.rs               # Semi-implicit Euler damped spring physics
│       │   └── mascot.rs               # Procedural/sprite pixel creature (5 states)
│       ├── views/
│       │   ├── mod.rs                  # View stack + horizontal spring slide transitions
│       │   ├── companion.rs            # View 1: Ambient Agent Face + Desk Clock
│       │   ├── ticker.rs               # View 2: Live Activity / Tool-Call Feed
│       │   ├── menu.rs                 # View 3: Spring-Animated Action & Host Switcher
│       │   ├── approval.rs             # View 4: Tool Approval Modal + 1-bit click preview
│       │   ├── prompt.rs               # View 5: Keyboard + Push-to-Talk Prompt Overlay
│       │   └── setup.rs                # View 6: First-Boot / Network & Tailscale Onboarding
│       └── ipc.rs                      # Non-blocking Unix socket NDJSON client + auto-reconnect
├── daemon/                             # Python package managed with uv
│   ├── pyproject.toml
│   └── src/lapis_daemon/
│       ├── main.py                     # Entrypoint: socket server + scheduler + agent worker
│       ├── config.py                   # Pydantic config loader (/etc/lapis/lapis.toml, hosts.toml)
│       ├── db.py                       # SQLite WAL schema & async queries
│       ├── ipc_server.py               # Multi-client Unix socket server (UI + CLI debug)
│       ├── system_status.py            # Wi-Fi, Tailscale status/auth URL, battery shutdown guard
│       ├── llm/
│       │   ├── base.py                 # Provider-agnostic Computer Use interface
│       │   ├── anthropic_cua.py        # Claude Computer Use adapter
│       │   └── openai_cua.py           # OpenAI CUA / OpenRouter adapter
│       ├── kvm/
│       │   ├── base.py                 # HostDriver protocol + MockHostDriver
│       │   ├── vnc_driver.py           # Tailscale VNC capture, downscale, coord map, HID inject
│       │   └── ssh_driver.py           # Tailscale SSH command execution
│       ├── agent/
│       │   ├── runner.py               # Step loop, dHash stuck detection, kill-switch, approval
│       │   ├── policy.py               # Risk classifier (strict / balanced / autonomous)
│       │   └── scheduler.py            # Asyncio cron runner for unattended background tasks
│       └── voice.py                    # ALSA push-to-talk capture + RMS stream + STT API
└── os/                                 # Custom Flashable OS Image Pipeline (pi-gen)
    ├── README.md                       # Build & flash guide (M5 Imager, rpiboot, dd)
    ├── config.pigen                    # pi-gen config (IMG_NAME=lapis-zero, STAGE_LIST, xz)
    ├── build-image.sh                  # Docker-backed pi-gen runner cloning CardputerZero/pi-gen
    ├── generate-imager-manifest.py     # Emits M5 Imager compatible os-list.json with SHA256s
    ├── stage-lapis/                    # Custom pi-gen stage replacing desktop stages 3..5
    │   ├── prerun.sh                   # Copy rootfs from stage2
    │   ├── EXPORT_IMAGE                # Trigger .img.xz generation at end of stage-lapis
    │   ├── 00-os-tuning/               # gpu_mem=16, fbcon=map:off, zram, fstab noatime, journald
    │   ├── 01-tailscale/               # Install tailscale apt repo & tailscaled
    │   ├── 02-lapis-stack/             # Install prebuilt aarch64 lapis-ui + Python venv daemon
    │   └── 03-services-firstboot/      # systemd units, lapis-firstboot.sh, power/battery hooks
    ├── files/
    │   ├── systemd/
    │   │   ├── lapis-firstboot.service
    │   │   ├── lapis-daemon.service
    │   │   ├── lapis-ui.service
    │   │   └── lapis-powerguard.service
    │   ├── initramfs/
    │   │   └── st7789-firmware         # Ensures cardputerzero,st7789v_lcd.bin stays in initramfs
    │   ├── zram-generator.conf         # 512 MB LZ4 zram swap
    │   └── lapis-firstboot.sh          # Ingests /boot/firmware/lapis-seed/* securely on boot
    └── docs/
        └── setup_target_hint.md        # Binding host VNC strictly to Tailscale IP (macOS/Linux)
```

---

## 4. Custom OS Image & Flashing Specification (`os/`)

### 4.1 `pi-gen` Image Pipeline Strategy

Lapis-Zero OS builds directly on top of **`CardputerZero/pi-gen` (branch `cardputerzero_v1.1`)** so all proprietary/custom CardputerZero hardware drivers, overlays, and firmware blobs work out of the box without reverse-engineering kernel patches:

1. **Base Stages (`stage0`, `stage1`, `stage2`):** Execute `CardputerZero/pi-gen` through `stage2` (headless Debian Trixie `arm64` + Raspberry Pi CM0 kernel + CardputerZero hardware BSP). Do **not** run `stage3`, `stage4`, or `stage5` (desktop/launcher).
2. **Custom Stage (`stage-lapis`):** Mount `os/stage-lapis` into `pi-gen` via `STAGE_LIST="stage0 stage1 stage2 stage-lapis"` with `EXPORT_IMAGE` placed inside `stage-lapis`.
3. **Pre-Built Artifacts Injection:** Before invoking `os/build-image.sh`, the build cross-compiles `lapis-ui` for `aarch64-unknown-linux-gnu` (`--release`) and packages `daemon/` as a wheel, copying both into `os/stage-lapis/02-lapis-stack/files/` so image builds stay fast, reproducible, and offline-friendly inside QEMU chroot.
4. **Compact Image Sizing:** Leave `IMAGE_TOTAL_SIZE` unset (auto-sized tight ext4 rootfs, expanding on first boot via standard `init_resize.sh` / `systemd` first-boot resize) and set `RANDOMIZE_DISK_ID_ON_FIRST_BOOT=1`. Compress final output with `xz -T0` to produce `lapis-zero-arm64.img.xz` (target compressed download size: $\sim 450\text{--}650\text{ MB}$).

### 4.2 Boot & OS Hardening (`00-os-tuning/`)

* **`/boot/firmware/config.txt` adjustments:**
  * Set `gpu_mem=16` (override stock `128`).
  * Keep all CardputerZero device-tree overlays (`st7789`, `tca8418`, `es8389`, `bmi270`, `bq27220`, `rx8130ce`) completely intact.
  * Comment out stock `splash_screen=` or replace with a clean monochrome Lapis-Zero splash binary.
* **`/boot/firmware/cmdline.txt` adjustments:**
  * Ensure `fbcon=map:off quiet loglevel=3 systemd.show_status=false` so kernel text never overwrites the SPI display before `lapis-ui` claims `/dev/fb0`.
* **Systemd & Storage Optimizations:**
  * `systemctl set-default multi-user.target`
  * Disable unnecessary services if present in `stage2`: `dphys-swapfile`, `ModemManager`, `avahi-daemon` (optional), `triggerhappy`, `apt-daily.timer`, `apt-daily-upgrade.timer`, `NetworkManager-wait-online.service`.
  * Configure `systemd-zram-generator` (`/etc/systemd/zram-generator.conf`): `zram-size = min(ram, 512)`, `compression-algorithm = lz4`.
  * Set `noatime` on `/` in `/etc/fstab`, mount `/tmp` as `tmpfs` (`size=64M`), and configure `/etc/systemd/journald.conf.d/volatile.conf` with `Storage=volatile` and `RuntimeMaxUse=16M`.
  * Install `/etc/initramfs-tools/hooks/st7789-firmware` so `cardputerzero,st7789v_lcd.bin` is always bundled in initramfs.
  * Configure udev USB autosuspend rules and a lightweight `lapis-powerguard` check that triggers clean `systemctl poweroff` if BQ27220 reports $\le 3\%$ battery and not charging.

### 4.3 Flashing & Zero-Friction First Boot (`M5 Imager` + `bootfs` Seed)

Users have **two seamless ways** to flash and provision Lapis-Zero OS:

1. **M5 Imager (`CardputerZero/m5stack-imager`):**
   * Flash locally via **Choose OS -> Use custom** (`lapis-zero-arm64.img.xz`), **or** launch `m5stack-imager --repo <url-or-local-path>/os-list.json` so **Lapis-Zero OS** appears natively under the **CardputerZero** device picker.
   * Use M5 Imager's built-in **OS Customisation** (`init_format: "systemd"`) to configure hostname (`lapis-zero`), Wi-Fi credentials, SSH key/password, and timezone before writing to microSD (or via `rpiboot`).
2. **FAT32 `bootfs` Seed Directory (`/boot/firmware/lapis-seed/`):**
   * After flashing, optionally drop any of the following files into `/boot/firmware/lapis-seed/` on the SD card's FAT32 partition from any laptop:
     * `lapis.toml` (LLM provider/model/schedules)
     * `hosts.toml` (Tailscale targets)
     * `secrets.env` (`ANTHROPIC_API_KEY=...`, `LAPIS_VNC_*=...`)
     * `tailscale.authkey` (reusable or ephemeral pre-auth key for headless tailnet join)
     * `lapis_ed25519` (optional SSH private key for target hosts)
   * On first boot, `lapis-firstboot.service` (ordered `Before=lapis-daemon.service`, `After=local-fs.target network-online.target`):
     1. Moves config files to `/etc/lapis/` (`chmod 600` on `secrets.env` and SSH keys).
     2. If `tailscale.authkey` exists, executes `tailscale up --authkey=... --ssh` and securely shreds/deletes `tailscale.authkey` and `secrets.env` from the FAT32 boot partition so plaintext secrets never remain on `/boot/firmware/`.
     3. Writes `/var/lib/lapis/.firstboot_done` and exits cleanly.
   * If Wi-Fi or Tailscale is **not** yet configured at boot, `lapis-ui` displays **View 6 (Onboarding / Setup)** showing live Wi-Fi status, local IP, and the `tailscale up` login URL / short code so the user can authenticate cleanly without ever needing an external monitor.

### 4.4 M5 Imager Manifest Generator (`os/generate-imager-manifest.py`)

Every image build generates an `os-list.json` conforming strictly to `CardputerZero/m5stack-imager`'s schema (`m5stack/os-list-template.json`):

```json
{
  "imager": {
    "latest_version": "1.0.0",
    "url": "https://github.com/CardputerZero/m5stack-imager/releases",
    "devices": [
      {
        "name": "CardputerZero",
        "description": "M5Stack pocket Raspberry Pi CM0 computer.",
        "tags": ["m5-cardputerzero", "pi3-64bit"],
        "default": true,
        "matching_type": "exclusive",
        "architecture": "armv8",
        "capabilities": ["i2c", "spi", "serial", "usb_otg"]
      }
    ]
  },
  "os_list": [
    {
      "name": "Lapis-Zero OS (Trixie arm64)",
      "description": "Dedicated monochrome agent OS with Option C Tailscale KVM for CardputerZero.",
      "url": "<release_url>/lapis-zero-arm64.img.xz",
      "extract_size": 0,
      "extract_sha256": "<computed_uncompressed_sha256>",
      "image_download_size": 0,
      "image_download_sha256": "<computed_xz_sha256>",
      "release_date": "YYYY-MM-DD",
      "init_format": "systemd",
      "devices": ["m5-cardputerzero", "pi3-64bit"],
      "architecture": "armv8",
      "capabilities": ["i2c", "spi", "serial", "usb_otg"]
    }
  ]
}
```

---

## 5. UI & Animation Specification (`lapis-ui`, Rust)

### 5.1 Visual Aesthetic (Sharp Memory LCD Look on $320 \times 170$ ST7789v3)

* **Color Palette (High-Contrast 1-Bit Feel + 2 Semantic Accents):**
  * `INK` = `#0B0B0D` (deep matte black)
  * `PAPER` = `#F2F2EC` (warm e-paper off-white; support Dark/Paper inversion toggle)
  * `MID` = simulated via **4x4 Bayer ordered dithering** between `INK` and `PAPER` (modal dim backdrops, progress fills, 1-bit click previews)
  * `ACCENT_BLUE` = `#2D6BFF` (active agent execution / tool call pip)
  * `ACCENT_AMBER` = `#FF6B2B` (approval required / warning / kill-switch / offline)
* **Typography:** Crisp, non-antialiased bitmap fonts only:
  * `6x8` compact mono (status bar, timestamps, ticker metadata)
  * `8x14` clean UI font (menu items, task titles, modal text)
  * `16x28` geometric display font (ambient desk clock)

### 5.2 Spring Physics & Adaptive Frame Pacing

All UI transitions (view slides, menu highlight pill, modal pop-ins, scroll inertia, mascot bounce) use a damped spring integrated via semi-implicit Euler:

$$a = k \cdot (x_{\text{target}} - x) - d \cdot v$$

$$v_{t+\Delta t} = v_t + a \cdot \Delta t, \quad x_{t+\Delta t} = x_t + v_{t+\Delta t} \cdot \Delta t$$

* **Menu Pill Spring:** $k = 320.0$, $d = 26.0$.
* **View Slide Spring:** $k = 240.0$, $d = 24.0$.
* **Modal Pop-In Spring:** $k = 360.0$, $d = 28.0$.
* **Adaptive Refresh Governor:**
  * Active spring motion ($|v| > 0.05$ or $|x_{\text{target}} - x| > 0.25$) or input within last $2.0\text{ s}$: **$60\text{ FPS}$** ($16.6\text{ ms}$ tick).
  * Agent active (`THINKING` / `ACTING` / `LISTENING`) with settled springs: **$12\text{ FPS}$**.
  * Ambient `IDLE` / Desk Clock: **$1\text{ FPS}$** (clock update + subtle blink every $5\text{--}8\text{ s}$).
  * Backlight auto-dim at $30\text{ s}$ idle; off at $90\text{ s}$ idle; instant wake on keypress, incoming `approval_request`, or BMI270 motion delta.

### 5.3 Screen Layout & Six Core Views ($320 \times 170$)

**Persistent Top Status Bar ($y = 0..18$, separated by a 1px solid rule):**
`[● STATE] [HOST: macbook-pro] [TS] [14:32] [84%]`

1. **View 1: Companion / Desk Clock (`companion.rs`, Default Home)**
   * **Left Zone ($x = 0..146$):** Procedural pixel companion creature + large clock below (`14:32`). Distinct states: `IDLE` (breathing + blink), `LISTENING` (live RMS waveform), `THINKING` (orbiting dots + scanning eyes), `ACTING` (typing pose + pulsing `ACCENT_BLUE`), `WAITING_APPROVAL` (alert pip + gentle jitter in `ACCENT_AMBER`).
   * **Right Zone ($x = 152..316$):** Active task title (max 2 wrapped lines), step counter (`Step 4/25`), last tool summary, and next scheduled cron run.
2. **View 2: Live Activity Ticker (`ticker.rs`)**
   * Smooth spring-scrolled log of timestamped agent tool calls (`14:31:04 [VNC] click(840, 412)`, `14:31:06 [SSH] git status -> 0`).
   * Pressing `ENTER` opens an inline detail overlay with full command/result output.
3. **View 3: Quick Action & System Menu (`menu.rs`)**
   * Spring-animated inverted selection pill behind crisp single-line rows:
     * `New Task / Prompt`
     * `Target Host: <cycle configured Tailscale hosts>`
     * `Pause / Resume Agent`
     * `Run Saved Workflow...`
     * `Scheduled Jobs`
     * `Network, Tailscale & Power` (opens View 6 / reboot / shutdown)
4. **View 4: Tool Approval Modal (`approval.rs`)**
   * Slides up over a Bayer-dithered dim backdrop on `approval_request`.
   * Displays action badge (`VNC CLICK`, `VNC TYPE`, `SSH EXEC`), target host, summary, and—for GUI clicks—a **$120 \times 68$ 1-bit dithered crop** with an inverted 7px crosshair.
   * Controls: `ENTER` (Approve once), `A` (Approve session), `ESC` (Deny).
5. **View 5: Prompt & Voice Overlay (`prompt.rs`)**
   * Opened via `/` or `SPACE` from Companion view, or while holding Push-to-Talk (`fn + SPACE` or `OPT`).
   * Shows active host, multi-line text buffer with blinking block cursor, and `ENTER` to dispatch `submit_task`.
6. **View 6: First-Boot / Network & System Onboarding (`setup.rs`)**
   * Automatically surfaced on first boot if Wi-Fi is disconnected or Tailscale is unauthenticated (and accessible anytime via Menu).
   * Displays Wi-Fi SSID/IP, Tailscale connection state (plus `tailscale up` login URL if awaiting browser auth), API key status (`OK` / `MISSING`), battery voltage/%, and quick actions (`Retry Tailscale`, `Reboot`, `Shutdown`).

### 5.4 CardputerZero 46-Key Mapping

* `Z` / `C` (or `Left` / `Right`): Cycle views (`Companion <-> Ticker <-> Menu`) when not typing in Prompt view.
* `F` / `X` (or `Up` / `Down`): Move menu highlight / scroll ticker.
* `ENTER`: Select / confirm / expand / approve.
* `ESC` (tap): Back / cancel / deny modal.
* `ESC` (hold $1.5\text{ s}$): **Global Agent Kill-Switch** (`agent_control: kill_all`).
* `TAB`: Quick-cycle active Tailscale target host.
* `fn + U` / `fn + I`: Decrease / increase LCD backlight brightness.

---

## 6. IPC Protocol Contract (`/run/lapis/lapisd.sock`)

Local Unix Domain Socket (`SOCK_STREAM`) carrying **NDJSON** (one UTF-8 JSON object per `\n`). Both sides tolerate restarts cleanly; `lapis-ui` reconnects with exponential backoff ($200\text{ ms} \to 2000\text{ ms}$).

### 6.1 Daemon -> UI Messages

```json
{"type":"state_sync","agent_state":"ACTING","active_host":"macbook-pro","hosts":["macbook-pro","arch-dev"],"active_task":{"id":"t_01","title":"Check CI logs and restart worker","step":4,"max_steps":25},"next_cron":"08:00 Daily Digest","wifi_ssid":"HomeWiFi","local_ip":"192.168.1.42","tailscale_state":"connected","tailscale_ip":"100.91.12.4","tailscale_auth_url":null,"api_key_ready":true}
{"type":"activity_event","ts":1795438264,"task_id":"t_01","kind":"vnc_click","summary":"click(840, 412) left","detail":"Clicked Retry Build button","status":"ok"}
{"type":"approval_request","request_id":"req_99","host":"macbook-pro","action":"ssh_exec","summary":"systemctl restart worker","risk":"high","preview_1bit_120x68_b64":null}
{"type":"approval_request","request_id":"req_100","host":"macbook-pro","action":"vnc_click","summary":"Click Delete Repository at (612, 304)","risk":"high","preview_1bit_120x68_b64":"<base64 of 1020 packed 1-bit bytes>"}
{"type":"mic_level","rms":0.42}
{"type":"toast","level":"warn","text":"Target VNC unreachable (macbook-pro)"}
```

### 6.2 UI -> Daemon Messages

```json
{"type":"request_sync"}
{"type":"submit_task","host":"macbook-pro","prompt":"Open terminal and check disk usage"}
{"type":"approval_decision","request_id":"req_99","decision":"approve_once"}
{"type":"select_host","host":"arch-dev"}
{"type":"agent_control","command":"kill_all"}
{"type":"agent_control","command":"pause"}
{"type":"agent_control","command":"resume"}
{"type":"system_control","command":"retry_tailscale"}
{"type":"system_control","command":"reboot"}
{"type":"system_control","command":"shutdown"}
{"type":"ptt_audio","state":"start"}
{"type":"ptt_audio","state":"stop"}
```

Allowed `decision` values: `"approve_once"`, `"approve_session"`, `"deny"`.

### 6.3 Control-Plane Additions (Android companion, pairing, mirroring)

`state_sync` is a superset of the frame above (see `ANDROID_COMPANION_SPEC.md` 5.4 `State`): it also
carries `action`, `tailscale_fqdn`, `battery_pct` (int or null), `battery_charging`,
`utc_offset_min` (the device's local-time offset from UTC, which drives the desk clock and ticker
timestamps), `pending_approvals`, `pairing`, `ui` and `device`. Clients ignore fields they do not
know.

Daemon -> UI:

```json
{"type":"state_sync","agent_state":"ACTING","action":"ssh_exec","active_host":"macbook-pro","active_task":{"id":"t_01","title":"Check CI logs","step":4,"max_steps":25}}
{"type":"task_finished","task_id":"t_01","chat_id":"c_5f2a9c1e","host":"macbook-pro","status":"done","summary":"CI is green; the worker was restarted."}
{"type":"approval_resolved","request_id":"req_99","decision":"approve_once","by":"android:Pixel"}
{"type":"pairing","mode":"pin","pin":"482917","device_name":"LapisZero-4F2A","status":"waiting","expires_in":300,"expires_at":1795438564}
{"type":"pairing","mode":"qr","payload":"lapis://pair?host=lapis-zero.tailnet.ts.net&nonce=...&pin=482917","host":"lapis-zero.tailnet.ts.net","pin":"482917","expires_in":180,"expires_at":1795438444}
{"type":"pairing","mode":"off"}
{"type":"ui_settings","theme":"dark","brightness":80,"mute_audio":false,"wake":true}
{"type":"screen_request","id":"a1b2c3d4"}
```

* `action` (a string in every `state_sync`) is what the agent is doing on the host right now: the
  `kind` of the step that is being executed (`screenshot`, `wait`, `ssh_exec`, `agent_run`,
  `vnc_click`, `vnc_move`, `vnc_drag`, `vnc_scroll`, `type_text`, `vnc_key`), and `""` when no
  step is. It is non-empty only together with `agent_state: "ACTING"`. A frame is sent when a
  step starts and when it has returned (`ACTING` with `""`, just before that step's
  `activity_event`), so a screen can show a fetch or a command while it happens;
  `activity_event` reports a step only afterwards. A daemon from before this field sends no
  `action`: treat a missing field as "not reported", not as `""`.
* `task_finished` is sent once for every task that ends, directly after the `state_sync` that
  shows `agent_state: "IDLE"` and `active_task: null`. `status` is `done`, `failed` (`summary`
  says why), `stopped` (the step limit) or `cancelled` (the kill switch, a stopped chat, a denied
  action that ended the task); `chat_id` is null for a task without a chat. `summary` is the
  closing sentence, up to 16000 characters for the answer of an agent tool. After the kill
  switch a `toast` of level `warn` follows; like the summary it says what the host confirmed:
  `Kill switch: all agent activity stopped`, or that something `may still be running on <host>`
  (`ANDROID_COMPANION_SPEC.md` 5.5). A daemon from before this change sends `task_finished` to
  WebSocket clients only, and a daemon that sends `action` also sends `task_finished` here.
* `approval_resolved` dismisses the matching modal when the decision was made elsewhere (the phone
  or the kill switch). `approval_request` also carries `task_id`, `chat_id`, `step`,
  `screenshot_id`, `click_x`, `click_y`; `action` may be `vnc_click`, `vnc_type`, `vnc_key` or `ssh_exec`.
* `pairing` shows View 7 (`pairing_pin` / `pairing_qr`); `mode: "off"` returns to the previous view.
  `status` for `pin` mode: `waiting`, `connected`, `verified`, `wifi`, `tailscale`, `done`, `locked`.
* `ui_settings`: `theme` `"dark"` swaps INK and PAPER; `brightness` is a percentage of the panel's
  maximum; `wake: true` resets the idle timer.

UI -> Daemon:

```json
{"type":"pairing_control","command":"start"}
{"type":"pairing_control","command":"stop"}
{"type":"screen_snapshot","id":"a1b2c3d4","png_b64":"<base64 PNG of the current 320x170 frame>"}
```

Other local clients (the on-demand `lapis-ble-prov` service) use the same socket:
`{"type":"pairing_event","mode":"pin"|"off", ...}` (rebroadcast as `pairing`),
`{"type":"reload_secrets"}` and `{"type":"refresh_status"}`.

---

## 7. Software Agent KVM & Daemon Specification (`lapis-daemon`, Python)

### 7.1 Configuration Paths (OS Image Standard)

* `/etc/lapis/lapis.toml` — core daemon, LLM, voice, and cron schedule settings
* `/etc/lapis/hosts.toml` — Tailscale target hosts (`VNC` + `SSH` + safety policy)
* `/etc/lapis/secrets.env` — `0600` environment file loaded by `lapis-daemon.service` (`EnvironmentFile=-/etc/lapis/secrets.env`)
* `/var/lib/lapis/lapis.db` — SQLite WAL persistence database
* `/run/lapis/lapisd.sock` — runtime IPC socket (`RuntimeDirectory=lapis`)

### 7.2 Option C KVM Drivers (`kvm/`)

#### `VncDriver` (`kvm/vnc_driver.py`)
1. **On-Demand Connection:** Connect via `asyncvnc` over Tailscale only during active tasks; disconnect after `vnc_idle_disconnect_sec` ($60\text{ s}$) idle.
2. **Low-Memory Screenshot Pipeline:** Capture raw frame, immediately downscale via `Pillow` (`Image.Resampling.LANCZOS`) preserving aspect ratio within `llm_max_width` ($1280$) $\times$ `llm_max_height` ($720$), encode JPEG (quality $75$, $\sim 60\text{--}110\text{ KB}$), and immediately `del` raw image buffers.
3. **Deterministic Coordinate Mapping:**
   $$x_{\text{vnc}} = \text{round}\left(x_{\text{llm}} \cdot \frac{W_{\text{native}}}{W_{\text{scaled}}}\right), \quad y_{\text{vnc}} = \text{round}\left(y_{\text{llm}} \cdot \frac{H_{\text{native}}}{H_{\text{scaled}}}\right)$$
4. **1-Bit UI Approval Preview:** Crop $240 \times 136$ centered on $(x_{\text{vnc}}, y_{\text{vnc}})$, resize to $120 \times 68$, draw 7px inverted crosshair, dither to 1-bit MSB-first packed bytes ($120 \times 68 / 8 = 1020\text{ bytes}$), base64 encode.
5. **HID Safety:** Track held modifier keys; on abort or `kill_all`, transmit explicit key-up events for all held modifiers before closing.

#### `SshDriver` (`kvm/ssh_driver.py`)
* Non-interactive command execution via `asyncssh` with strict timeout ($30\text{ s}$) and combined `stdout`+`stderr` truncation to $4096\text{ bytes}$. The output is read as it arrives and only that head is ever held in memory. At the timeout, and on `kill_all`, the remote command is stopped before the call returns (`TERM`, a second of grace, `KILL`); if the host does not confirm that, the result and the kill switch say that it may still be running.

### 7.3 Agent Tools, Safety Policy & Persistence

* **Unified Tool Surface:** `screenshot()`, `mouse_click(x, y, button, clicks)`, `mouse_move(x, y)`, `mouse_drag(x1, y1, x2, y2)`, `scroll(x, y, dx, dy)`, `type_text(text)`, `key_press(combo)`, `ssh_exec(command, timeout_s)`, `wait(seconds)`, `finish(summary)`.
* **Policy Tiers (`agent/policy.py`):**
  * **Hard Denylist (Always Blocked):** `rm -rf /`, `mkfs`, `dd if=`, fork bombs, host `shutdown`/`reboot`.
  * `strict`: All mutating actions require UI approval.
  * `balanced`: `screenshot`, `wait`, `scroll`, and allowlisted read-only SSH commands (`ls`, `pwd`, `git status`, `git log`, `df`, `ps`, `uname`) auto-run; destructive shell commands (`rm`, `sudo`, `kill`, `chmod`, `curl | sh`), secret entry, and high-risk UI clicks require UI approval.
  * `autonomous`: Auto-runs everything not on the hard denylist.
* **Stuck-Loop Detector:** 64-bit grayscale `dHash` on each captured frame. If `stuck_threshold` ($3$) consecutive mutating actions yield Hamming distance $\le 2$, inject a corrective system hint; pause and warn UI if still stuck after $2$ retries.
* **SQLite WAL (`db.py`):** `PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;` with tables `tasks`, `steps`, and `schedules`.

---

## 8. Phased Implementation Roadmap & Acceptance Criteria

Execute strictly one phase at a time. Pass all acceptance criteria before advancing.

### Phase 0: Workspace, Canvas & Desktop Simulator
* **Deliverables:**
  1. Initialize Cargo crate in `ui/`, `uv` Python package in `daemon/`, config examples in `config/`, and root `Makefile`.
  2. Implement `ui/src/gfx/canvas.rs` ($320 \times 170$ buffer, primitives, Bayer 4x4 dithering, embedded `6x8`, `8x14`, `16x28` bitmap fonts).
  3. Implement `ui/src/backend/sdl_sim.rs` ($960 \times 510$ 3x integer-scaled desktop simulator with CardputerZero key bindings).
  4. Implement `--dump-frame <path.png> --view <name>` headless PNG exporter.
* **Acceptance Criteria:** `cargo test` passes; `cargo run -- --sim` renders cleanly at $60\text{ FPS}$; `--dump-frame` outputs valid $320 \times 170$ PNGs.

### Phase 1: Lapis One Animation Engine & All 6 Views
* **Deliverables:**
  1. Implement `gfx/spring.rs` and `gfx/mascot.rs` (5 animation states + desk clock).
  2. Implement all 6 views (`companion`, `ticker`, `menu`, `approval`, `prompt`, `setup`) with horizontal spring view slides, spring menu pill, and 1-bit `120x68` preview rendering in `approval.rs`.
  3. Add `--demo` mode cycling through states, a synthetic approval modal, and the first-boot onboarding view.
* **Acceptance Criteria:** Smooth $60\text{ FPS}$ transitions across all 6 views; `--dump-frame` PNGs for all 6 views verify zero text overlap or clipping at $320 \times 170$; idle governor drops to $1\text{ FPS}$.

### Phase 2: IPC Protocol & Daemon Core (Mock Driver)
* **Deliverables:**
  1. Implement `db.py` (SQLite WAL), `config.py`, `system_status.py`, and `ipc_server.py`.
  2. Implement `ui/src/ipc.rs` with non-blocking reads and automatic reconnect.
  3. Implement `agent/runner.py`, `agent/policy.py`, and `MockHostDriver`.
* **Acceptance Criteria:** Launching `lapis-daemon --mock` + `lapis-ui --sim` executes a full task from Prompt -> SQLite -> Ticker -> Approval Modal -> `ESC` kill-switch; daemon restart auto-reconnects and re-syncs within $2\text{ s}$.

### Phase 3: Option C KVM Drivers (Tailscale VNC + SSH) & LLM Adapter
* **Deliverables:**
  1. Implement `kvm/vnc_driver.py` (`asyncvnc`, Pillow downscaling, coordinate transform, 1-bit `120x68` crosshair thumbnail, modifier key-up cleanup).
  2. Implement `kvm/ssh_driver.py` (`asyncssh`, timeout + $4\text{ KB}$ truncation).
  3. Implement `llm/anthropic_cua.py` (behind `llm/base.py`), `dHash` stuck detection, and `agent/scheduler.py`.
* **Acceptance Criteria:** Unit tests pass for coordinate math, 1020-byte 1-bit thumbnail packing, `dHash` stuck detection, and policy tiers; live Tailscale host test completes a multi-step GUI + SSH task with daemon peak RSS $< 130\text{ MB}$.

### Phase 4: CardputerZero CM0 Hardware Backends & Cross-Compilation
* **Deliverables:**
  1. Implement `ui/src/backend/fbdev.rs` (`/dev/fb0` mmap RGB565 conversion + stride-safe flush).
  2. Implement `ui/src/backend/hw_cm0.rs`: TCA8418 `evdev`, BQ27220 battery reader, backlight brightness (`fn + U/I`) + idle blanking, BMI270 motion wake listener.
  3. Add `aarch64-unknown-linux-gnu` cross-compile (`zigbuild` or `cross`) targets to `Makefile`.
* **Acceptance Criteria:** `make cross-ui` builds a stripped `aarch64` release binary cleanly; Linux hardware backend unit tests and mock-sysfs tests pass.

### Phase 5: Custom Flashable OS Image Pipeline (`os/` + M5 Imager Integration)
* **Deliverables:**
  1. Author `os/config.pigen`, `os/build-image.sh` (cloning `CardputerZero/pi-gen` `cardputerzero_v1.1` and running `STAGE_LIST="stage0 stage1 stage2 stage-lapis"` inside Docker).
  2. Author `os/stage-lapis/` (`00-os-tuning`, `01-tailscale`, `02-lapis-stack`, `03-services-firstboot`) including `gpu_mem=16`, `fbcon=map:off`, `st7789-firmware` initramfs hook, `zram-generator.conf`, `systemd` units, `lapis-firstboot.sh` (FAT32 `/boot/firmware/lapis-seed/` importer + secret shredder), and `lapis-powerguard`.
  3. Author `os/generate-imager-manifest.py` outputting M5 Imager-compatible `os-list.json` with exact SHA256 hashes and byte sizes.
  4. Author `os/README.md` and `os/docs/setup_target_hint.md` covering flashing via `M5 Imager` (`Use custom` and `--repo`), `rpiboot`, and `bootfs` seeding.
* **Acceptance Criteria:**
  * `shellcheck` passes on all scripts in `os/`.
  * `os/generate-imager-manifest.py` passes schema validation against `CardputerZero/m5stack-imager`'s `os-list-template.json`.
  * `make build-os` produces a bootable `lapis-zero-arm64.img.xz` and `os-list.json`.
  * Flashing via **M5 Imager** boots CardputerZero directly into Lapis-Zero (`multi-user.target`, zero desktop/X11/APPLaunch, $\sim 496\text{ MB}$ Linux RAM visible, total active RSS $< 220\text{ MB}$, `lapis-firstboot.service` imports and shreds `bootfs` secrets).

### Phase 6: Push-to-Talk Voice Pipeline & Audio Polish
* **Deliverables:**
  1. Implement `daemon/src/lapis_daemon/voice.py` (ALSA $16\text{ kHz}$ mono capture while PTT is held, live `mic_level` RMS streaming to UI waveform, STT transcription, automatic task enqueue).
  2. Add toggleable 8-bit procedural audio cues via ES8389 ALSA output.
* **Acceptance Criteria:** Holding PTT animates the Companion waveform reactively; releasing transcribes speech and dispatches the task to the active Tailscale host without breaking the RAM budget.