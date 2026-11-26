# Lapis-Zero Android Companion & Tailscale Control Plane — Implementation Specification

## 1. Overview & Product Vision

This specification extends **Lapis-Zero OS** (`SPEC.md`) with a native **Android Companion App (`android/`)** and a lightweight **Device Control & Pairing Layer** on the CardputerZero (CM0). Together they turn your Android phone into the full high-bandwidth interface for Lapis-Zero:

1. **Zero-Cable Pairing & Provisioning (BLE + QR):** Pair a freshly flashed CardputerZero over Bluetooth Low Energy (BLE) directly from Android—configure Wi-Fi, join Tailscale, seed LLM API keys, and establish a cryptographically bound pairing token without ever touching `/boot/firmware/` or opening an SSH terminal. Devices already on Tailscale can also pair instantaneously by scanning a QR code rendered on the CardputerZero's $320 \times 170$ LCD.
2. **Full Multi-Chat Agent Control Over Tailscale:** Manage multiple conversational agent chats across all configured Tailscale KVM targets (`macbook-pro`, `arch-dev`, etc.), stream live tool calls and full-color target screen snapshots, send text or phone-mic voice prompts, approve/deny high-risk actions (including from Android notifications), and trigger the global **Kill-Switch** from anywhere on your tailnet.
3. **Complete Remote Configuration & Device Cockpit:** Edit `/etc/lapis/lapis.toml`, manage `/etc/lapis/hosts.toml` with one-tap VNC/SSH connection probes, rotate write-only secrets in `/etc/lapis/secrets.env`, configure cron schedules, inspect BQ27220 battery/RAM/Tailscale telemetry, toggle CardputerZero display settings, and mirror the $320 \times 170$ screen.

### Non-Negotiable Engineering Constraints

1. **512 MB CM0 RAM Discipline:**
   * The remote HTTP + WebSocket API server runs **inside** `lapis-daemon` using lightweight `aiohttp` (no heavy framework, no second Python interpreter). Target additional overhead: $\le 12\text{ MB}$ RSS.
   * The BLE GATT provisioning service (`lapis-ble-prov.service`) runs **on-demand only** (during initial unpaired boot or an explicit 5-minute pairing window from the CardputerZero menu) and shuts down completely once paired so idle RAM stays $< 220\text{ MB}$ total.
2. **Tailscale-Only Network Exposure (Zero Public Ports):**
   * `lapis-daemon`'s API binds **exclusively** to `127.0.0.1:8421` (never `0.0.0.0`).
   * Exposed to the tailnet solely via `tailscale serve --bg --https=443 http://127.0.0.1:8421`, providing WireGuard transport encryption and automatic TLS (`https://<node>.<tailnet>.ts.net` and `wss://<node>.<tailnet>.ts.net/v1/ws`).
3. **Defense-in-Depth Authentication:**
   * Even inside the tailnet, every REST and WebSocket call requires a paired 256-bit bearer token (`Authorization: Bearer <token>`) issued only through physical proximity pairing (6-digit LCD PIN confirmation over BLE or physical QR scan from the CardputerZero LCD).
   * Write-only secret handling: API keys and VNC passwords can be pushed from Android to `/etc/lapis/secrets.env` (`0600`), and the API returns only boolean presence flags (`is_set: true`) and masked tails (`...a9f2`), never plaintext secrets.
4. **Bidirectional Real-Time Sync:**
   * The CardputerZero $320 \times 170$ LCD (`lapis-ui` over `/run/lapis/lapisd.sock`) and all connected Android clients (`wss://.../v1/ws`) observe the **exact same canonical state** from `lapis-daemon`. Starting a chat on Android animates the CardputerZero mascot; approving or killing a task on either device immediately resolves it on both.
5. **Visual Unity ("Lapis Pocket" Aesthetic):**
   * The Android app echoes Lapis-Zero's high-contrast monochrome hardware identity (`INK #0B0B0D`, `PAPER #F2F2EC`, `ACCENT_BLUE #2D6BFF`, `ACCENT_AMBER #FF6B2B`, crisp monospace tool logs, spring micro-animations) while leveraging the phone's full-color display for target VNC screenshots and comfortable configuration forms.

---

## 2. End-to-End Architecture

```text
┌──────────────────────────────────────────────────────────────────────────┐
│  Android Companion App (Kotlin 2.2+ / Jetpack Compose BOM 2026.11.01)    │
│  • Chats & Agent Stream  • Live KVM & Approvals  • Device Cockpit        │
│  • Full Config / Hosts / Secrets / Schedules     • BLE & QR Pairing      │
│  • Local Room Cache + Keystore Token Vault + Actionable Notifications    │
└───────────────────▲──────────────────────────────────▲───────────────────┘
                    │ (A) Initial Pairing Only         │ (B) Normal Operation
                    │ BLE GATT + X25519/AES-GCM        │ HTTPS + WSS over Tailscale
                    │ (Auto-stops after pairing)       │ (tailscale serve -> 127.0.0.1:8421)
┌───────────────────▼──────────────────────────────────▼───────────────────┐
│  CardputerZero (Lapis-Zero OS — Debian Trixie arm64, 512 MB RAM)         │
│  ┌──────────────────────────────┐  ┌───────────────────────────────────┐ │
│  │ lapis-ble-prov.service       │  │ tailscale serve (--https=443)     │ │
│  │ (On-demand BlueZ GATT / DBus)│  │   └─> 127.0.0.1:8421 (aiohttp)    │ │
│  └──────────────┬───────────────┘  └──────────────────┬────────────────┘ │
│                 │ writes Wi-Fi (nmcli),               │ in-process       │
│                 │ tailscale up, pairing token         │                  │
│                 ▼                                     ▼                  │
│  ┌─────────────────────────────────────────────────────────────────────┐ │
│  │ lapis-daemon.service (Python 3.12+ asyncio + SQLite WAL)            │ │
│  │ • Chat Session Manager (multi-turn chats + tasks + artifacts)       │ │
│  │ • Event Bus (fans out to /run/lapis/lapisd.sock AND /v1/ws clients) │ │
│  │ • Config & Secrets Manager + Live Host Probe (VNC/SSH test)         │ │
│  │ • Option C KVM Drivers (Tailscale VNC + SSH) + Policy Gate          │ │
│  └───────────────────────────────▲─────────────────────────────────────┘ │
│                                  │ NDJSON Unix Socket                    │
│  ┌───────────────────────────────▼─────────────────────────────────────┐ │
│  │ lapis-ui.service (Rust, 320x170 /dev/fb0)                           │ │
│  │ • Displays 6-digit Pairing PIN & QR Code view (pairing.rs)          │ │
│  │ • Stays live-synced with Android chats, approvals, & theme toggles  │ │
│  └─────────────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────────┘
```

---

## 3. Repository Additions & Module Layout

Extend the existing `lapis-zero/` monorepo cleanly:

```text
lapis-zero/
├── SPEC.md                                 # Core OS, UI, and Option C KVM spec
├── ANDROID_COMPANION_SPEC.md               # This specification
├── ui/src/views/
│   └── pairing.rs                          # View 7: BLE 6-digit PIN + Tailscale QR pairing view
├── daemon/src/lapis_daemon/
│   ├── api/
│   │   ├── server.py                       # aiohttp HTTP + WebSocket server (127.0.0.1:8421)
│   │   ├── auth.py                         # Paired device token store (SHA-256 hashed in SQLite)
│   │   ├── routes_chats.py                 # Multi-turn chat CRUD, messages, stream, cancel
│   │   ├── routes_approvals.py             # Approval queue + decision endpoints + screenshot fetch
│   │   ├── routes_config.py                # Read/write lapis.toml, hosts.toml, schedules, secrets
│   │   ├── routes_device.py                # Telemetry, display settings, screen mirror, reboot/poweroff
│   │   └── ws_hub.py                       # Authenticated WebSocket broadcast + heartbeat hub
│   ├── ble_prov/
│   │   ├── gatt_server.py                  # On-demand BlueZ D-Bus GATT server (bluez-peripheral/dbus-fast)
│   │   ├── crypto.py                       # X25519 key exchange + HKDF + AES-256-GCM envelope
│   │   └── network_ctl.py                  # nmcli Wi-Fi scan/connect + tailscale up + serve setup
│   └── chat_manager.py                     # Multi-turn chat threads backed by SQLite WAL
├── os/
│   ├── stage-lapis/03-services-firstboot/  # Add bluez, network-manager, lapis-ble-prov.service
│   └── files/systemd/
│       └── lapis-ble-prov.service          # On-demand BLE provisioning unit (socket/CLI triggered)
└── android/                                # Native Android Studio / Gradle project
    ├── settings.gradle.kts
    ├── build.gradle.kts                    # Kotlin 2.2+, Compose BOM 2026.11.01, compileSdk 37
    └── app/src/main/java/dev/lapiszero/app/
        ├── LapisApp.kt                     # App container, notification channels, background sync
        ├── theme/
        │   └── LapisTheme.kt               # Ink/Paper/LapisBlue/Amber Material 3 theme + mono type
        ├── data/
        │   ├── local/                      # Room DB (cached chats/messages/devices) + Keystore prefs
        │   ├── remote/
        │   │   ├── LapisApiClient.kt       # OkHttp/Ktor REST client with Bearer token interceptor
        │   │   ├── LapisWsClient.kt        # Reconnecting WebSocket client -> SharedFlow<LapisEvent>
        │   │   └── ProtocolModels.kt       # Kotlinx Serialization DTOs locked to daemon API
        │   └── ble/
        │       ├── BleProvisioner.kt       # Nordic BLE KTX scanner, GATT client, MTU negotiation
        │       └── BleEnvelopeCrypto.kt    # X25519 + AES-256-GCM matching daemon/ble_prov/crypto.py
        ├── domain/
        │   ├── DeviceRepository.kt         # Multi-device switcher (supports >1 CardputerZero)
        │   └── ChatRepository.kt           # Live chat state, optimistic send, approval actions
        ├── notifications/
        │   ├── ApprovalNotifier.kt         # High-priority approval alerts with Approve/Deny buttons
        │   └── ApprovalActionReceiver.kt   # BroadcastReceiver executing instant decision over Tailscale
        └── ui/
            ├── navigation/AppNav.kt        # Bottom bar: Chats | Live KVM | Device | Config
            ├── onboarding/                 # BLE scan -> PIN verify -> Wi-Fi -> Tailscale -> Done + QR path
            ├── chats/                      # Chat list + Conversational Agent Thread + Voice PTT bar
            ├── kvm/                        # Full-color screenshot viewer, click crosshair, approval sheet
            ├── device/                     # Telemetry cards, display/theme toggles, 320x170 live mirror
            └── config/                     # Hosts editor (+ probe), LLM/Secrets, Schedules, Safety Policy
```

---

## 4. Pairing & Provisioning Protocols

### 4.1 Path A: BLE Zero-Cable Pairing & Onboarding (Fresh Device)

Used when the CardputerZero has just been flashed and has no Wi-Fi or Tailscale credentials yet (or when triggered via Menu -> `Pair Android App`).

#### Advertising & GATT Service Definition
* **Advertised Local Name:** `LapisZero-<LAST4_MAC>` (e.g., `LapisZero-4F2A`)
* **Primary Service UUID:** `7c4f0001-9a3b-4c6e-8e21-51a0c0de0001`
* **Characteristics:**
  * `...0002` (`HANDSHAKE`, Read/Write): Exchanges ephemeral **X25519** public keys and triggers generation of a **6-digit pairing PIN** displayed on the CardputerZero's $320 \times 170$ LCD (`pairing.rs`).
  * `...0003` (`CMD_TX`, Write): Encrypted (`AES-256-GCM`) chunked JSON command packets from Android -> CardputerZero (framed with 2-byte sequence header for safe transfer over negotiated ATT MTU $\ge 185$).
  * `...0004` (`EVT_RX`, Notify): Encrypted (`AES-256-GCM`) chunked JSON status/response packets from CardputerZero -> Android.

#### Step-by-Step Provisioning Sequence
1. **Scan & Connect:** Android scans for Service UUID `...0001`, connects, and requests MTU $517$.
2. **Encrypted Session + Physical Verification:**
   * Both sides perform X25519 ECDH, derive a session key via HKDF-SHA256 bound to the 6-digit PIN shown on the CardputerZero LCD.
   * User enters the 6-digit PIN in the Android app (proving physical possession).
   * CardputerZero verifies PIN proof, registers the phone in SQLite `paired_devices`, and returns a freshly minted 256-bit `api_token` inside the encrypted channel.
3. **Wi-Fi Setup:**
   * Android sends `{"cmd": "wifi_scan"}` -> CardputerZero returns nearby SSIDs + signal strength via `nmcli`.
   * User selects Wi-Fi and enters password -> Android sends `{"cmd": "wifi_connect", "ssid": "...", "psk": "..."}`.
   * CardputerZero connects via NetworkManager and streams live status (`connecting` -> `connected`, `local_ip`).
4. **Tailscale Enrollment & Serve Activation:**
   * Android offers two options:
     * **Interactive Browser Login (Easiest):** Android sends `{"cmd": "tailscale_up"}`. CardputerZero returns the `https://login.tailscale.com/a/...` auth URL over BLE; Android opens it immediately in a Chrome Custom Tab where the user is already signed in!
     * **Pre-Auth Key:** Android sends `{"cmd": "tailscale_up", "authkey": "tskey-auth-..."}`.
   * Once Tailscale reports `Running`, CardputerZero automatically runs:
     ```bash
     tailscale serve --bg --https=443 http://127.0.0.1:8421
     ```
     and notifies Android with `{"evt": "ready", "tailscale_fqdn": "lapis-zero.<tailnet>.ts.net", "tailscale_ip": "100.x.y.z"}`.
5. **Optional Seed Push & Teardown:**
   * User can optionally push initial `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` and first KVM host right inside the onboarding wizard.
   * Android verifies live HTTPS connectivity to `https://lapis-zero.<tailnet>.ts.net/v1/health` with the bearer token, then sends `{"cmd": "finish_provisioning"}`.
   * CardputerZero stops `lapis-ble-prov.service` (freeing BLE radio + RAM) and transitions `lapis-ui` to the Companion view.

### 4.2 Path B: Instant QR Pairing (Already on Tailscale)

When the CardputerZero is already connected to Wi-Fi and Tailscale (e.g., via `bootfs` seed or adding a second phone):
1. On the CardputerZero, select **Menu -> Pair Android App (QR)**.
2. `lapis-daemon` mints a short-lived one-time pairing nonce ($180\text{ s}$ TTL) and sends a compact QR payload to `lapis-ui`:
   ```text
   lapis://pair?host=lapis-zero.tailnet.ts.net&nonce=<32-byte-base64url>&pin=<6-digit>
   ```
3. `lapis-ui` (`pairing.rs`) renders a crisp $125 \times 125$ pixel-exact QR code on the left side of the $320 \times 170$ LCD alongside the countdown timer and host name.
4. Android scans the QR code, calls `POST https://<host>/v1/pair/exchange` with `{nonce, device_name, device_model}` over Tailscale, receives its persistent `api_token`, and stores it in Android Keystore.

---

## 5. Daemon Control Plane API (`127.0.0.1:8421` -> Tailscale Serve)

### 5.1 Database Schema Extensions (`daemon/src/lapis_daemon/db.py`)

Extend the existing SQLite WAL database (`/var/lib/lapis/lapis.db`) with chat threads and paired devices:

* `paired_devices(id, name, model, token_hash, created_at, last_seen_at, revoked)`
* `chats(id, title, host_id, model_override, policy_override, archived, created_at, updated_at, agent, agent_session, workdir)`
  * `agent`: `"lapis"` (the device's own agent, the default) or the id of an agent tool on the host; `agent_session`: that tool's own session id, kept on the device only; `workdir`: the folder the tool runs in (`""` = home). Added by an in-place migration.
* `chat_messages(id, chat_id, role, content, task_id, metadata_json, created_at)`
  * `role`: `"user" | "assistant" | "tool" | "system"`
* `screenshots(id, task_id, step_num, jpeg_path, width, height, click_x, click_y, created_at)`
  * Ring-buffered on disk under `/var/lib/lapis/frames/` capped at **25 MB** (LRU eviction) so SD card usage stays tiny while Android can inspect recent high-res agent frames.

### 5.2 REST API Endpoints (`/v1/...`)

All endpoints except `/v1/pair/exchange` require `Authorization: Bearer <api_token>`.

| Group | Method & Path | Purpose |
| :--- | :--- | :--- |
| **Health & Auth** | `GET /v1/health` | Daemon version, hostname, uptime, auth validity. |
| | `POST /v1/pair/exchange` | Exchange QR one-time nonce for persistent device `api_token`. |
| | `GET /v1/devices` / `DELETE /v1/devices/{id}` | List or revoke paired Android devices. |
| **Agent Chats** | `GET /v1/chats` | List all chats ordered by `updated_at` (with last message preview & active status). |
| | `POST /v1/chats` | Create a new chat bound to a `host_id` (and optional `policy_override`, `agent`, `workdir`). |
| | `GET /v1/chats/{chat_id}` | Fetch chat metadata + paginated `chat_messages` + linked step logs. |
| | `PATCH /v1/chats/{chat_id}` | Rename chat, switch target `host_id`, change policy, agent tool or working folder, or archive. |
| | `DELETE /v1/chats/{chat_id}` | Delete chat and cascade messages. |
| | `POST /v1/chats/{chat_id}/messages` | Submit a user prompt (text or audio upload via multipart) to run the agent loop in this chat's conversation context. |
| | `POST /v1/chats/{chat_id}/stop` | Stop the currently running agent turn for this chat. |
| **Control & KVM** | `POST /v1/approvals/{request_id}` | Submit `{"decision": "approve_once" \| "approve_session" \| "deny", "reason": "..."}`. |
| | `GET /v1/screenshots/{id}.jpg` | Retrieve full-color downscaled ($\le 1280 \times 720$) JPEG frame + click coordinates headers. |
| | `POST /v1/hosts/{host_id}/nudge` | Optional manual assist (click/type/key) while an agent is paused on a CAPTCHA/login. |
| | `POST /v1/agent/kill-all` | Global emergency kill-switch (identical to holding `ESC` on CardputerZero). |
| **Configuration** | `GET /v1/config` / `PUT /v1/config` | Read and atomically validate+write `/etc/lapis/lapis.toml` (Pydantic validated). |
| | `GET /v1/hosts` / `PUT /v1/hosts` | Read and atomically update `/etc/lapis/hosts.toml`. |
| | `POST /v1/hosts/{host_id}/probe` | Live connectivity test: Tailscale ping + VNC handshake + SSH `echo ok` latency report, how the SSH login worked, and the agent tools found. |
| | `GET /v1/hosts/{host_id}/agents` | Agent command-line tools (Claude Code, opencode, Freya, ...) installed on the host. |
| | `POST /v1/hosts/{host_id}/ssh/authorize` | Add the device's own SSH key to the host's `~/.ssh/authorized_keys` and prove that it logs in. |
| | `GET /v1/secrets/status` / `PATCH /v1/secrets` | Inspect which keys exist (`is_set`, last 4 chars) and update `/etc/lapis/secrets.env` (`0600`) atomically. |
| | `GET /v1/schedules` / `POST` / `PATCH` / `DELETE` | Full CRUD + enable/disable + "Run Now" for background cron jobs. |
| **Device Cockpit** | `GET /v1/device/telemetry` | Battery %/voltage/charging (BQ27220), CPU temp/load, RAM/zram MB, Wi-Fi SSID/RSSI, Tailscale peers. |
| | `PATCH /v1/device/ui` | Control CardputerZero LCD remotely: `theme` (`dark`/`paper`), `brightness` ($0\text{--}100$), `mute_audio`, `wake`. |
| | `GET /v1/device/screen.png` | Grab current $320 \times 170$ CardputerZero framebuffer snapshot (via IPC request to `lapis-ui`). |
| | `GET /v1/device/ssh-key` | The device's own SSH public key (one `authorized_keys` line) and its fingerprint. |
| | `POST /v1/device/power` | `{"action": "reboot" \| "shutdown" \| "restart_daemon" \| "start_ble_pairing"}`. |

### 5.3 Real-Time WebSocket Stream (`wss://<host>/v1/ws`)

Authenticated via `Authorization: Bearer <api_token>` header during WebSocket upgrade. Uses a superset of the local NDJSON IPC contract (`SPEC.md` Section 6):

* **Server -> Android Events:**
  * `state_sync` (full device, active chat/task, hosts, pending approvals snapshot sent immediately on connect)
  * `chat_message_delta` / `chat_message_created` (live streaming assistant responses & user prompts from either phone or CardputerZero)
  * `activity_event` (live VNC/SSH tool call stream with optional `screenshot_id`)
  * `approval_request` (high-risk gate with `request_id`, `chat_id`, `host`, `action`, `summary`, `screenshot_id`, `click_x`, `click_y`)
  * `telemetry_tick` (pushed every $5\text{ s}$ while an Android client is actively viewing Device Cockpit; $30\text{ s}$ otherwise)
  * `toast` (warnings, stuck-loop alerts, VNC disconnects)
* **Android -> Server Commands:**
  * `ping` / `pong` (15-second keepalive)
  * `subscribe_telemetry` (`{"enabled": true|false}`)
  * `send_prompt`, `approval_decision`, `kill_all` (low-latency alternative to REST)

### 5.4 Wire Shapes (normative, as implemented in `daemon/src/lapis_daemon/api/`)

All bodies are JSON (`snake_case`). Timestamps are Unix seconds (integers). Errors are
`{"error": {"code": "<slug>", "message": "<text>"}}` with status `400` (bad JSON), `401`
(missing/invalid/revoked token), `403`, `404`, `409` (`busy`, `not_paused`, `ssh_unavailable`),
`422` (`invalid`), `429` (`locked_out`) or `504` (`ui_unavailable`). Clients must ignore unknown
fields. `409 busy` means exactly one thing: a task is running, or the last one is still being
stopped by the kill switch (the tool on the host is being ended, see 5.5); it is answered to a
new message and to `PUT /v1/hosts`, and retrying after `task_finished` works. Its messages are
`Agent busy: a task is already running`, `Agent busy: the last task is still being stopped` and
`Agent busy: stop the running task before editing hosts`. Any other refusal is `422 invalid`,
whatever words its message contains.

**Objects**

```text
Chat      {id, title, host_id, model_override|null, policy_override|null ("strict"|"balanced"|"autonomous"),
           archived, created_at, updated_at, last_message: Message|null,
           active: bool, step: int|null, pending_approvals: int,
           agent: "lapis" | <agent tool id>, workdir ("" = the home folder)}
Message   {id: int, chat_id, role: "user"|"assistant"|"tool"|"system", content, task_id|null,
           metadata: {...}, created_at}
           tool metadata:      {kind, status: "ok"|"denied"|"error", detail, screenshot_id|null, step, ts}
           user metadata:      {source: "device"|"voice"|"schedule"|"android:<name>"}
           assistant metadata: {status: "done"|"cancelled"|"failed"|"stopped"}
Approval  {type: "approval_request", request_id, host, action: "vnc_click"|"vnc_type"|"vnc_key"|"ssh_exec",
           summary, risk: "low"|"high", preview_1bit_120x68_b64|null (1020 bytes, MSB-first, 15 bytes/row),
           task_id, chat_id|null, step, screenshot_id|null, click_x|null, click_y|null}
           (click_x/click_y are pixels in the screenshot identified by screenshot_id)
Activity  {type: "activity_event", ts, task_id, kind, summary, detail, status, chat_id|null, step,
           screenshot_id|null}
           kind: prompt | thought | ssh_exec | vnc_click | vnc_move | vnc_drag | vnc_scroll | vnc_key |
                 type_text | screenshot | wait | nudge | agent_run
           status: ok | denied | error, and for agent_run also "running" (a progress line, see 5.5)
State     {type: "state_sync", agent_state: "IDLE"|"THINKING"|"ACTING"|"WAITING_APPROVAL"|"PAUSED",
           action: an Activity kind while a step is being executed, else "" (see 5.6),
           active_host, hosts: [id], active_task: {id, title, step, max_steps}|null, active_chat_id|null,
           next_cron, wifi_ssid, local_ip, tailscale_state: "connected"|"needs_login"|"needs_approval"|
           "starting"|"stopped"|"offline", tailscale_ip, tailscale_fqdn, tailscale_auth_url|null,
           api_key_ready, battery_pct|null, battery_charging, utc_offset_min,
           pending_approvals: [Approval], pairing: Pairing|null,
           ui: {theme: "dark"|"paper", brightness: 0..100, mute_audio, timezone (IANA name or "")},
           device: {hostname, version, mock}}
Pairing   {type: "pairing", mode: "qr", payload, host, pin, expires_in, expires_at}
        | {type: "pairing", mode: "pin", pin, device_name, status, expires_in, expires_at}
        | {type: "pairing", mode: "off"}
Host      {hostname, tailscale_ip, os: "macos"|"linux"|"windows", policy,
           vnc: {port, username, password_env ("" = none; a secret's NAME, never the password)},
           ssh: {port, user, identity_file|null, password_env ("" = keys only; a secret's NAME)}}
Schedule  {name, schedule (5-field cron), host, prompt, enabled, next_run|null, last_run_at|null,
           last_chat_id|null}
Telemetry {battery: {percent, voltage_v, charging}|null, cpu: {temp_c, load1},
           memory: {total_mb, available_mb, zram_total_mb, zram_used_mb, daemon_rss_mb, ui_rss_mb},
           wifi: {ssid, ip, rssi_dbm|null}, tailscale: {state, ip, fqdn, peers}, uptime_sec,
           services: {"lapis-ui": "active", ...}}
```

**REST**

| Method & path | Request body | Success |
| :--- | :--- | :--- |
| `GET /v1/health` | | `200 {ok, version, hostname, uptime_sec, mock, device_id}` |
| `POST /v1/pair/exchange` (no token) | `{nonce, device_name, device_model}` | `200 {device_id, api_token, hostname}` |
| `GET /v1/devices` | | `200 {devices: [{id, name, model, created_at, last_seen_at, revoked, current}]}` |
| `DELETE /v1/devices/{id}` | | `204` |
| `GET /v1/state` | | `200 State` |
| `GET /v1/chats?host_id=&archived=1` | | `200 {chats: [Chat]}` (newest first) |
| `POST /v1/chats` | `{host_id, title?, policy_override?, agent?, workdir?}` | `201 Chat`; `422` for an unknown agent |
| `GET /v1/chats/{id}?before=<message id>&limit=50` | | `200 {chat, messages: [Message] (oldest first), has_more}` |
| `PATCH /v1/chats/{id}` | `{title?, host_id?, policy_override?, archived?, agent?, workdir?}` (any other key is ignored) | `200 Chat`; `422` for an unknown agent |
| `DELETE /v1/chats/{id}` | | `204` |
| `POST /v1/chats/{id}/messages` | `{content}` or multipart with an `audio` part | `202 {message, task_id}`; `409 busy` |
| `POST /v1/chats/{id}/stop` | | `200 {stopped}` |
| `GET /v1/approvals` | | `200 {approvals: [Approval]}` (pending) |
| `POST /v1/approvals/{request_id}` | `{decision, reason?}` | `200 {ok}`; `404` if not pending |
| `GET /v1/screenshots/{id}.jpg` | | `200 image/jpeg` + `X-Lapis-Width`, `X-Lapis-Height`, `X-Lapis-Click-X/Y` |
| `POST /v1/hosts/{host_id}/nudge` | `{action: "click"|"type"|"key", x?, y?, text?, combo?}` | `200 {ok}`; `409 not_paused` |
| `POST /v1/agent/kill-all` · `/pause` · `/resume` | | `200` (`kill-all` answers when the task has ended, see 5.5) |
| `PUT /v1/agent/active-host` | `{host_id}` | `200 {active_host}` |
| `GET /v1/config` · `PUT /v1/config` | `{config: {...}}` | `200 {config}` (secret *names* only, never values); keys kept when not sent: see 5.5 |
| `GET /v1/hosts` · `PUT /v1/hosts` | `{hosts: {id: Host}}` | `200 {hosts, active_host}`; `409 busy` while a task runs; `422` for a `password_env` that is not a name |
| `POST /v1/hosts/{host_id}/probe` | | `200 {host_id, tailscale, vnc, ssh, agents}`: the first three `{ok, latency_ms, detail}`, `agents: [Agent]` |
| `GET /v1/hosts/{host_id}/agents` | | `200 {agents: [Agent]}`; `409 ssh_unavailable` |
| `POST /v1/hosts/{host_id}/ssh/authorize` | | `200 {ok, already, detail}`; `409 ssh_unavailable`; `422` for a Windows host |
| `GET /v1/secrets/status` | | `200 {secrets: [{name, is_set, masked}]}` (`masked` = `"...a9f2"` or null) |
| `PATCH /v1/secrets` | `{set: {NAME: value}, unset: [NAME]}` | `200 {secrets: [...]}` (same masked shape); `422` for a name that is not one (5.6) |
| `GET /v1/schedules` | | `200 {schedules: [Schedule]}` |
| `POST /v1/schedules` | `{name, schedule, host, prompt, enabled}` | `201 {schedules}` |
| `PATCH /v1/schedules/{name}` · `DELETE` | partial Schedule | `200 {schedules}` · `204` |
| `POST /v1/schedules/{name}/run` | | `202 {chat_id, task_id}` |
| `GET /v1/device/telemetry` | | `200 Telemetry` |
| `PATCH /v1/device/ui` | `{theme?, brightness?, mute_audio?, wake?, timezone?}` | `200 {theme, brightness, mute_audio, timezone}`; `422` for an unknown zone |
| `GET /v1/device/screen.png` | | `200 image/png` (320x170); `504 ui_unavailable` |
| `GET /v1/device/ssh-key` | | `200 {public_key, fingerprint}` |
| `POST /v1/device/power` | `{action: "reboot"|"shutdown"|"restart_daemon"|"start_ble_pairing"|"start_qr_pairing"|"stop_pairing"}` | `202 {ok, action}` |

**WebSocket `/v1/ws`** (bearer token on the upgrade request; text frames, one JSON object each)

* Server -> client: `state_sync` (State; first frame, then on every change), `activity_event`,
  `approval_request`, `approval_resolved {request_id, decision, by}` (`by`: `"device"`,
  `"android:<name>"` or `"kill_switch"`), `task_finished {task_id, chat_id, host, status, summary}`
  (`status`: `"done"|"failed"|"stopped"|"cancelled"`; see 5.6),
  `chat_message_created {chat_id, message}`, `chat_updated {chat}`, `chat_deleted {chat_id}`,
  `pairing` (Pairing), `telemetry_tick {telemetry}`, `toast {level, text}`, `pong {ts}`.
* Client -> server: `{type: "ping"}`, `{type: "subscribe_telemetry", enabled}`,
  `{type: "send_prompt", chat_id, content}`, `{type: "approval_decision", request_id, decision, reason?}`,
  `{type: "kill_all"}`.

**QR payload:** `lapis://pair?host=<fqdn>&nonce=<43-char base64url>&pin=<6 digits>`. The app calls
`https://<host>/v1/pair/exchange`. A development daemon (`--mock`) appends `&tls=0`, in which case
`host` is `HOST:PORT` and the app uses plain `http://`.

### 5.5 SSH Login and Agent Tools on a Host (normative)

Everything here needs the bearer token. No answer, error, event or log line carries a password
or the device's private key. A device from before these features answers `404` on
`GET /v1/device/ssh-key`; such a device must not be sent `ssh.password_env`, `agent` or `workdir`.

**How the device logs in.** In this order: the host's `ssh.identity_file` (if set and readable),
the device's own key, then the account's password, and the password only when `ssh.password_env`
names a stored secret (hosts may ban an address after wrong passwords). `ssh.password_env` is the
NAME of a secret set through `PATCH /v1/secrets`, never the password. On a Mac the Screen Sharing
and Remote Login accounts are the same, so the app stores the name of the VNC password secret in
both places: one typed login. `PUT /v1/hosts` refuses a `vnc.password_env` or
`ssh.password_env` that is not a secret name (`422 invalid`; the refusal names the field,
`hosts.<id>.vnc.password_env`, and does not repeat the value). A `hosts.toml` stored before that
rule still loads: such a value is dropped (it never logged anybody in) and the host is kept.

**Keys that are kept when a `PUT` does not send them.** The app writes whole objects back, and a
build from before a setting existed drops the keys it does not know. For the settings that came
later, a key that is absent from the body therefore means "as it is", not "back to the default":
`config.agents`, `config.kvm.agent_run_timeout_sec`, `tiers` of an `agents` entry whose `command`
is unchanged, and `hosts.<id>.ssh.password_env`. To clear one, send it empty (`"agents": {}`,
`"password_env": ""`).

**`GET /v1/device/ssh-key`** The device's own ed25519 key, made on first use and kept `0600` next
to the database. Only the public half exists outside the device.

```json
{"public_key": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIC9amMIXZ6wmika+BvUQkejc91cRdysT1P4L75CnQKqZ lapis-zero@lapis",
 "fingerprint": "SHA256:Tx3dxFjdBGffis65UZ27pMeeGXZUBPoe7FfE3T4JZak"}
```

**`POST /v1/hosts/{host_id}/ssh/authorize`** (no body) Logs in by any method that works, appends
the device's key to `~/.ssh/authorized_keys` unless it is there (creating `~/.ssh` `0700` and the
file `0600` when missing; no other line is changed), then proves it by logging in with the device
key alone. `already` is true when the key was on the host before the call.

```json
{"ok": true, "already": false,
 "detail": "Added this device's key on macbook-pro (logged in by password). The key now logs in by itself."}
```

Errors: `404 not_found` (unknown host); `422 invalid` for a host whose `os` is `windows`
(Unix-like hosts only); `409 ssh_unavailable` when no login works, when the file cannot be
written, or when the host still refuses the key afterwards. The message is one the owner can act
on, for a Mac:

```json
{"error": {"code": "ssh_unavailable",
 "message": "Cannot log in to macbook-pro over SSH (PermissionDenied: Permission denied for user alice on host 100.64.10.21). On the Mac, turn on Remote Login under System Settings > General > Sharing. Then enter that computer's user name and password in its host form and authorise this device there."}}
```

**`GET /v1/hosts/{host_id}/agents`** The agent tools installed on the host, found through the
account's own login shell (they live in `~/.local/bin`, under nvm ..., which a plain SSH command
does not see). `lapis` is never listed: it is always available. `Agent = {id, name, path}`.

```json
{"agents": [{"id": "claude", "name": "Claude Code", "path": "/Users/alice/.local/bin/claude"},
            {"id": "opencode", "name": "opencode", "path": "/Users/alice/.nvm/versions/node/v22.22.1/bin/opencode"}]}
```

Errors: `404 not_found`; `409 ssh_unavailable` (same message form as above) when the device
cannot log in or the login shell does not run the command. Built-in ids: `claude` (Claude Code),
`opencode`, `freya` (binary `lumi-freya`), `codex`, `gemini`, `cursor` (binary `cursor-agent`);
more come from `[agents.<id>]` in `lapis.toml` (`config.agents` in `GET`/`PUT /v1/config`):
`{"name": "Aider", "command": "aider --yes --message {prompt}", "tiers": ["autonomous"]}`, where
`command` is a program and its arguments (no shell syntax) with `{prompt}` once as a whole word,
and `tiers` (default `["autonomous"]`; at least one of `strict`, `balanced`, `autonomous`;
returned the tightest first) lists the policy tiers under which that command line may be run:
see "Which tool runs under which policy" below.

**`POST /v1/hosts/{host_id}/probe`** gains two things: `ssh.detail` says how the login worked,
and `agents` lists the tools as above (`[]` when SSH is down).

```json
{"host_id": "macbook-pro",
 "tailscale": {"ok": true, "latency_ms": 12, "detail": "pong from macbook-pro (100.64.10.21) via DERP(sfo) in 12ms"},
 "vnc": {"ok": true, "latency_ms": 310, "detail": "1280x720"},
 "ssh": {"ok": true, "latency_ms": 140, "detail": "ok (device key)"},
 "agents": [{"id": "claude", "name": "Claude Code", "path": "/Users/alice/.local/bin/claude"}]}
```

`ssh.detail` ends in `(identity file)`, `(device key)`, `(password)` or `(no credentials asked)`;
when `ssh.ok` is false it is the reason, e.g. `PermissionDenied: Permission denied for user ...`.

**Chats bound to a tool.** `POST /v1/chats` and `PATCH /v1/chats/{id}` take `agent` (`"lapis"` or
an agent id; anything else is `422 invalid` with the valid ids in the message) and `workdir`
(a folder on the host, at most 500 printable characters; `~/...` is the home folder; `""` = home).
Every Chat object carries both. The tool's own session id never leaves the device; changing
`agent`, `workdir` or `host_id` starts a new session.

```json
POST /v1/chats   {"host_id": "macbook-pro", "agent": "claude", "workdir": "~/code/app"}
201              {"id": "c_5f2a9c1e", "title": "New chat", "host_id": "macbook-pro", "model_override": null,
                  "policy_override": null, "archived": false, "created_at": 1795438264, "updated_at": 1795438264,
                  "agent": "claude", "workdir": "~/code/app", "last_message": null,
                  "active": false, "step": null, "pending_approvals": 0}
422              {"error": {"code": "invalid",
                  "message": "Unknown agent: use one of lapis, claude, opencode, freya, codex, gemini, cursor"}}
```

**How a message runs in such a chat** (`POST /v1/chats/{id}/messages` and the WS `send_prompt` are
unchanged: `202 {message, task_id}`, `409 busy`):

* The message is handed to the tool over SSH, unchanged, in `workdir`. No LLM API key is needed and
  nothing is put up for approval. Instead the tool's own permission settings have to keep it
  within the chat's policy tier (`policy_override`, else the host's), and **a tool that has no
  setting for that tier is not started at all**: see "Which tool runs under which policy".
* `state_sync` shows `agent_state: "ACTING"` and `active_task: {step: 1, max_steps: 1}` while the
  tool works. `activity_event`s of kind `agent_run` with `status: "running"` carry a line of the
  tool's output in `summary`, at most two a second; they are not stored as messages.
  `{"type": "activity_event", "ts": 1795438270, "task_id": "t_123456789", "kind": "agent_run",
  "summary": "bash npm test", "detail": "", "status": "running", "chat_id": "c_5f2a9c1e", "step": 1,
  "screenshot_id": null}`
* It ends with one stored tool message (`metadata.kind: "agent_run"`, `status: "ok"|"error"`,
  `content`: the tool's name) and the assistant message: the tool's answer with
  `metadata.status: "done"`, or with `"failed"` one of these sentences (`task_finished` carries the
  same status and text):

  ```text
  Claude Code is not installed on macbook-pro: no `claude` on the PATH of the login shell there.
  opencode failed (exit status 1). Its last output:\n<up to 1500 characters>
  Claude Code reported an error. Its last output:\n<what it said>
  Freya was stopped after 30 minutes without finishing. Its last output:\n<...>
  The connection to macbook-pro closed before Claude Code finished. Its last output:\n<...>
  Claude Code could not be started on macbook-pro: the login shell of the account there did not run the command.
  Claude Code could not be run: the message is too long to hand over on a command line
  Claude Code runs on macbook-pro over SSH. Cannot log in to macbook-pro over SSH (<reason>). On the Mac, ...
  opencode was not started: it cannot be held to the balanced policy that applies to this chat on macbook-pro. It can be run under autonomous: set that as this chat's policy to allow it, or pick a tool that keeps to balanced: Claude Code, Freya, Gemini CLI.
  Claude Code printed more than the 64 KB this device keeps of one run, so its answer could not be read and whether it succeeded is not known. Its last output:\n<...>
  Freya did not finish within 30 minutes, and macbook-pro did not confirm that it was stopped: it may still be running there. Its last output:\n<...>
  Freya was stopped after 30 minutes without finishing, but the programs it had started on macbook-pro could not be checked: some may still be running there. Its last output:\n<...>
  The connection to macbook-pro closed before Claude Code finished: it may still be running there. Its last output:\n<...>
  ```

  "Stopped" is said only of what the host confirmed. The last three are what is said instead
  when it did not: the host could not be reached, or what the tool had started could not be
  looked for there. A connection that goes silent (the host asleep, off the network) is noticed
  within a minute, not at the end of the run's time limit.

  An answer is at most 16000 characters (the end of it, if the tool printed more); a tool that
  prints nothing gives `"<name> finished without printing an answer."` with status `done`.
* `POST /v1/agent/kill-all` and `POST /v1/chats/{id}/stop` end the tool on the host and finish the
  task as `cancelled`. The tool and every process that descends from it (also the commands it
  runs in sessions of their own) are asked to end, and killed after a second; the same goes for
  a shell command of the device's own agent (`ssh_exec`). The call answers when that is done:
  normally well under a second, about one for a tool that ignores being asked, two when the host
  does not answer. Until then the agent counts as busy: a new message gets `409 busy`, and a
  second `kill-all` waits for the first. What the task's closing message, `task_finished.summary`
  and the `warn` toast then say depends on what the host confirmed:

  | The host | Summary (assistant message, `task_finished.summary`) | Toast |
  | :--- | :--- | :--- |
  | confirmed that nothing is left | `Stopped by the kill switch.` | `Kill switch: all agent activity stopped` |
  | ended the tool, but could not list what it had started (no `ps`, another kind of system) | `Stopped by the kill switch, but the programs that Claude Code had started on macbook-pro could not be checked: some may still be running there.` | `Kill switch: programs started by Claude Code may still be running on macbook-pro` |
  | confirmed nothing (no answer, or an SSH server that passes no signals on) | `Stopped on this device by the kill switch, but macbook-pro did not confirm that Claude Code was stopped: it may still be running there.` | `Kill switch: Claude Code may still be running on macbook-pro` |

  For a chat of the device's own agent the name is `the last command`. A limit that the device
  cannot see past: a program whose parent process had already ended when the kill came (a job the
  tool left in the background, a daemon) no longer descends from the tool and is not found.
* The longest one run may take is `config.kvm.agent_run_timeout_sec` (default 1800, 10 to 14400).
* Prompts typed or spoken on the handheld and schedules always use `lapis`, whatever their chat says.
* A chat whose agent was an `[agents.<id>]` entry that has since been removed from the config
  refuses new messages: `422 invalid`, `"This chat's agent 'aider' is no longer configured: pick
  another one for the chat"`. `PATCH` the chat to another agent.

**Which tool runs under which policy.** A tool's chat is not gated step by step, so the tier is
only as good as the tool's own permission settings. The device runs a tool under a tier only
where the tool's help or source says what holds it there:

| Tool | `strict` | `balanced` | `autonomous` |
| :--- | :--- | :--- | :--- |
| `claude` | `--permission-mode plan` | `--permission-mode acceptEdits` | `--permission-mode bypassPermissions` |
| `gemini` | `--approval-mode plan` | `--approval-mode auto_edit` | `--approval-mode yolo` |
| `cursor` | `--mode plan` | not run | `--force` |
| `freya` | not run | no flag (runs what is not destructive) | `--yolo` |
| `opencode` | not run | not run | `--dangerously-skip-permissions` |
| `codex` | not run | not run | no flag |
| `[agents.<id>]` | only if listed in its `tiers` | only if listed in its `tiers` | by default |

"Not run": the message is accepted (`202`), nothing is started on the host, and the task ends
at once as `failed` with the "was not started" sentence above, which names the tiers the tool
can run under and the tools that keep to the chat's tier. The owner's way out is the chat's
`policy_override` (or the host's `policy`), or another tool. opencode 1.4 allows edits and shell
commands outright and has no flag for less (its `OPENCODE_PERMISSION` variable is ignored in
silence by a version that does not know it, and overridden by an agent-level permission in the
host's own `opencode.json`). An `[agents.<id>]` tool is known to the device by its command line
only: listing a tier in `tiers` is the owner vouching that the line as written keeps the tool
within it.

**How long these calls take.** `GET /v1/hosts/{id}/agents`, `POST /v1/hosts/{id}/ssh/authorize` and
`POST /v1/hosts/{id}/probe` log in to the host before they answer: normally one to three seconds,
up to about 45 seconds when the host drops packets or its login shell is slow. Give them a read
timeout of 60 seconds.

### 5.6 What the Agent Is Doing Right Now, How a Task Ended, Secret Names (normative)

**`action` in every `state_sync`.** A top-level string in every State: on the WebSocket, in
`GET /v1/state`, and in the same frame on the handheld's own socket. It is the Activity `kind` of
the step that is being executed right now, and `""` when no step is: while thinking, waiting for
an approval, paused, or idle. An `activity_event` reports a step when it has *returned*; `action`
is how a screen shows a fetch or a command while it happens.

```json
{"type": "state_sync", "agent_state": "ACTING", "action": "ssh_exec", "active_host": "macbook-pro",
 "active_task": {"id": "t_123456789", "title": "Check CI", "step": 3, "max_steps": 25}, "...": "..."}
```

* Values: `screenshot`, `wait`, `ssh_exec`, `agent_run`, `vnc_click`, `vnc_move`, `vnc_drag`,
  `vnc_scroll`, `type_text`, `vnc_key`: the same words as `activity_event.kind` (not the badge names
  of `approval_request.action`, which is another field of another frame). Treat a word you do
  not know as plain work. `prompt`, `thought` and `nudge` are never an `action`.
* It is non-empty only together with `agent_state: "ACTING"`. A state frame is sent when a step
  starts (`ACTING` with its `action`) and when it has returned (`ACTING` with `""`, just before the
  step's `activity_event`); the next frame is usually `THINKING`. For a chat bound to an agent tool
  it is `agent_run` for as long as the tool works, which can be minutes.
* Pausing in the middle of a step shows `PAUSED` with `""` at once, although that step still runs
  to its end; resuming before it has ended shows `ACTING` with the `action` again.
* A (re)connecting client gets the current `action` in its first frame.
* A device from before this field sends State without `action`: treat a missing field as "not
  reported" and fall back to `activity_event` (which is unchanged), not as `""`.
* The mock daemon (`--mock`) holds each of its scripted steps for 0.8 s, so the field can be
  watched without a host.

**`task_finished` on every transport.** `{type, task_id, chat_id|null, host, status, summary}` is
sent once for every task that ends, to WebSocket clients and (new) on the handheld's own socket,
directly after the `state_sync` that shows `agent_state: "IDLE"` and `active_task: null`.

```json
{"type": "task_finished", "task_id": "t_123456789", "chat_id": "c_5f2a9c1e", "host": "macbook-pro",
 "status": "failed", "summary": "Stopped after repeated errors. Error: ConnectionError: host unreachable"}
```

`status`: `done`; `failed` (it could not go on: `summary` says why, as a sentence); `stopped` (the
step limit); `cancelled` (the kill switch, a stopped chat, or a denied action that ended the
task). `summary` is the text of the chat's closing assistant message (a tool's answer can be up to
16000 characters). The kill switch also sends a `toast` of level `warn` after it; both its
summary and its toast say whether the host confirmed the stop (the three forms are in 5.5), so
show them as sent rather than a text of the app's own. A device from before this change sends
`task_finished` to WebSocket clients only.

**Secret names.** `llm.api_key_env` (`PUT /v1/config`), `vnc.password_env` and `ssh.password_env`
(`PUT /v1/hosts`) and the keys of `set` in `PATCH /v1/secrets` are NAMES: letters, digits and `_`, not starting with a
digit, at most 64 characters, and not one of the device's own variables (`PATH`, `HOME`, `USER`,
`SHELL`, `LANG`, `TZ`, `LAPIS_CONFIG_DIR`, or anything starting with `LD_`, `PYTHON`, `DYLD_`). A
name that looks like a key is refused, by the rule the app applies while typing
(`ConfigRules.looksLikeSecretValue`; the daemon's tests read the app's own lists of keys and
names and hold the daemon to them). With `switches(s)` the number of neighbouring characters in
`s` of which exactly one is a digit, a text (trimmed) looks like a key if

* it starts, in any case, with `sk-`, `gsk_`, `AIza`, `eyJ`, `tskey-` or `tskey_auth`; or
* its letters and digits alone (everything else removed) are 20 or more with 6 or more switches; or
* split at `_`, `-`, `.` and space, one piece is 12 or more characters with 3 or more switches.

So `MINIMAX_API_KEY`, `minimaxapikey2025`, `SK_OPENROUTER` and `OPENAI_API_KEY_2025_11_24` are
names; `gsk_aB3dE5gH...`, `9f8e1c0a7b6d5e4f3a2b1c0d9e8f7a6b` and `ABCD-EF12-GH34-IJ56-KL78` are not.
In `PATCH /v1/secrets` the rule applies to new names only: a name the device already stores stays
writable. Names starting with `LAPIS_` (derived by the app from a host id) are exempt from it
there and in `vnc.password_env` and `ssh.password_env`. Every refusal is `422 invalid`, and one about a text that is not
a name or looks like a key does not repeat that text:

```json
{"error": {"code": "invalid", "message": "a secret name looks like a key or password: give the secret a name such as MINIMAX_API_KEY and send the key as its value"}}
{"error": {"code": "invalid", "message": "Invalid secret name: a name is letters, digits and _ and does not start with a digit"}}
{"error": {"code": "invalid", "message": "Invalid secret: its value must be a non-empty string"}}
{"error": {"code": "invalid", "message": "secret name PATH is reserved (it would replace a variable of the device's own process): use another name, or unset it if it is already stored"}}
```

For `PUT /v1/config` and `PUT /v1/hosts` the message is the validation error of the field
(`llm.api_key_env`, `hosts.<id>.vnc.password_env` or `hosts.<id>.ssh.password_env`), which names
the field and the rule. The rule cannot tell a password that happens to be made of letters and
digits only from a name: the app must never offer a typed password to these fields.

### 5.7 Changing Wi-Fi Only (normative; same JSON on Lapis-Deck)

A paired phone changes the device's Wi-Fi without re-pairing or touching Tailscale: over this API
when the device is reachable, over BLE paired-phone mode (`BLE_PROVISIONING_PROTOCOL.md` 4.2)
when it is not.

| Method | Path | Body | Answer |
| --- | --- | --- | --- |
| GET | `/v1/wifi` | | `{"ssid", "ip", "rssi": int\|null, "saved": [{"name", "active"}], "ble": {"open", "expires_at"}}` |
| GET | `/v1/wifi/scan` | | `{"networks": [{"ssid", "rssi", "secured"}]}` (<= 20); 503 `wifi_unavailable` |
| POST | `/v1/wifi/connect` | `{"ssid", "psk": str\|null}` | `{"state": "connected"\|"failed", "local_ip"}`; 422 `invalid` (ssid 1..32 bytes, psk 8..63 printable or 64 hex) |
| POST | `/v1/wifi/forget` | `{"name"}` | the `GET /v1/wifi` body; 404 `not_found` |
| POST | `/v1/wifi/ble` | | the `GET /v1/wifi` body with `ble.open` true (the phone Wi-Fi window); 409 `not_paired`, 503 `ble_unavailable` |

`connect` can take ~45 s and moves the device off the network the phone may be using: a timeout
or a dropped connection means "switching networks", not failure. The password never appears in
an answer, a log line, the journal or a command line (NetworkManager keyfile, root 0600).

---

## 6. Android Companion App Specification (`android/`)

### 6.1 Tech Stack (2026 Modern Native Android)

* **Language & Build:** Kotlin $2.2+$, Gradle Kotlin DSL, `minSdk = 29` (Android 10+), `targetSdk = 36+`, `compileSdk = 37` (or latest installed SDK matching Compose BOM).
* **UI Toolkit:** Jetpack Compose (`androidx.compose:compose-bom:2026.11.01`, Material 3 `1.5.0`), `navigation-compose`, crisp monospace font family (`JetBrains Mono`) paired with geometric sans (`Space Grotesk` / `Inter`).
* **Networking & Serialization:** OkHttp (HTTPS + WebSocket), `kotlinx-serialization-json`, Kotlin Coroutines + `StateFlow`/`SharedFlow`.
* **Local Storage & Security:** Room (offline chat history & telemetry cache), Android Keystore-backed encrypted preferences (`EncryptedSharedPreferences` / Tink AEAD + DataStore) for `api_token` storage.
* **BLE & QR:** Nordic `no.nordicsemi.android:ble-ktx:2.11.0` (coroutines BLE wrapper with automatic MTU/packet splitting), AndroidX CameraX + ZXing / ML Kit Barcode for QR pairing.
* **Voice Input:** Android `SpeechRecognizer` (on-device fast STT) OR raw $16\text{ kHz}$ audio upload to `lapis-daemon`'s configured Whisper endpoint.

### 6.2 Core Screens & User Flows

#### 1. Onboarding & Device Pairing (`ui/onboarding/`)
* **Zero-State Welcome:** Detect whether Tailscale VPN is active on the phone (friendly status chip + deep-link to open Tailscale app if disconnected).
* **Two Entry Cards:**
  * **Pair New CardputerZero (BLE):** Scans nearby `LapisZero-*` devices -> 6-digit PIN entry -> Wi-Fi picker -> Tailscale login (Custom Tab or Auth Key) -> optional API key entry -> auto-verifies HTTPS over Tailscale.
  * **Pair Existing Device (QR Code):** Scans QR from CardputerZero LCD -> instant token exchange.
* **Multi-Device Switcher:** Top app bar pill lets you switch between multiple paired CardputerZero units (or a local desktop `--mock` daemon for development).

#### 2. Chats Tab (`ui/chats/` — Primary Workspace)
* **Chat List:** Filterable by target KVM host (`All`, `macbook-pro`, `arch-dev`), showing live pulsing `ACCENT_BLUE` dot on actively running agent threads, step counts, and unread approval badges in `ACCENT_AMBER`.
* **Conversational Agent Thread:**
  * Full multi-turn chat interface with the agent. Every user prompt starts or continues an autonomous Computer Use run on the chat's bound host.
  * Inline **Tool-Call Cards** (collapsible): shows `[VNC] click(840, 412)` or `[SSH] git status -> 0`, execution latency, expandable stdout/stderr, and tappable full-color target screenshot thumbnails.
  * Inline **Sticky Approval Card** whenever `policy.py` pauses execution: shows cropped target screenshot with red/amber crosshair + **Approve Once**, **Approve Session**, and **Deny (with note)** buttons.
  * Bottom composer bar: Host badge switcher, Policy badge (`Strict` / `Balanced` / `Auto`), text input, **Hold-to-Talk Mic** button, and—while running—an unmistakable red/amber **STOP / KILL** control.

#### 3. Live KVM Tab (`ui/kvm/`)
* Real-time view of the active agent's target screen:
  * Pinch-to-zoom full-color latest frame (`GET /v1/screenshots/{id}.jpg`) with overlaid history of recent click points (`#1`, `#2`, `#3`).
  * Live **Step Ticker** drawer beneath the viewport.
  * **Manual Assist Mode ("Nudge"):** When an agent pauses for a password, 2FA, or CAPTCHA, tap **"Take Control Temporarily"** to tap coordinates or type text directly via `/v1/hosts/{id}/nudge`, then tap **"Hand Back to Agent"**.

#### 4. Device Cockpit Tab (`ui/device/`)
* **CardputerZero Live Header:** Optional live $320 \times 170$ pixel-sharp mirror of the CardputerZero's actual ST7789v3 display (tap to refresh or toggle $2\text{ FPS}$ preview) + quick status pills (`ONLINE`, `TAILSCALE 100.x.y.z`, `UPTIME`).
* **Hardware Telemetry Grid:**
  * BQ27220 Battery % + Voltage + Charging state
  * RAM & `zram` gauge (flags warning if active RSS $> 220\text{ MB}$)
  * CPU Temperature & Load + Wi-Fi Signal (dBm)
* **Hardware Controls:**
  * LCD Theme toggle (`Dark Ink` vs `Paper White`)
  * Backlight brightness slider + Screen Wake button
  * Audio Chimes mute toggle
  * Power controls (with confirmation dialog): `Restart Agent`, `Pair Another Phone (BLE)`, `Reboot`, `Shutdown`.

#### 5. Config & Fleet Tab (`ui/config/`)
* **Tailscale KVM Hosts (`hosts.toml`):** Add/edit/delete target machines (`display_name`, `tailscale_host`, `os`, `vnc_port`, `ssh_user`, `llm_max_width/height`, `policy`) with a **Test Connection** button that runs `/v1/hosts/{id}/probe` and displays green/red checks for Tailscale Reachability, VNC Auth, and SSH Access.
* **Agent & LLM Settings (`lapis.toml`):** Provider selector (Anthropic / OpenAI / OpenRouter), model ID, `max_steps`, `stuck_threshold`, idle timeouts.
* **Secrets Vault (`secrets.env`):** Write-only inputs for `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `LAPIS_VNC_*` passwords, and STT API key; shows masked status only.
* **Scheduled Jobs (`schedules`):** Visual cron builder / list with enable toggle, last run summary link (opens resulting chat!), and **Run Now** button.
* **Paired Phones Manager:** View and revoke any paired Android device tokens.

### 6.3 Actionable Approval Notifications

When the app is backgrounded while a long agent task runs:
* A lightweight foreground service (active **only** while an agent task is running or when user enables "Always-On Approval Listener") maintains the Tailscale WebSocket.
* Incoming `approval_request` events post a high-priority Android notification showing the host, action summary, and two direct action buttons (**Approve Once** / **Deny**), plus tapping the notification deep-links directly into the chat's Approval Card.

---

## 7. Phased Implementation Roadmap & Acceptance Criteria

Execute strictly one phase at a time. Each phase must be tested and verified before advancing.

### Phase A0: Daemon Chat Threads, Auth & Control Plane API
* **Deliverables:**
  1. Extend `daemon/src/lapis_daemon/db.py` with `paired_devices`, `chats`, `chat_messages`, and `screenshots` (25 MB LRU disk cap).
  2. Implement `daemon/src/lapis_daemon/chat_manager.py` so multi-turn conversations maintain message history across agent runs and mirror cleanly to the existing `lapis-ui` Unix socket.
  3. Implement `daemon/src/lapis_daemon/api/` (`aiohttp` bound strictly to `127.0.0.1:8421`): bearer token auth, QR nonce exchange, Chat CRUD + message submission, Approval endpoints, Screenshot serving, Config/Hosts/Secrets/Schedules endpoints, Live Host Probe, Telemetry, and authenticated `/v1/ws` broadcast hub.
  4. Add `--dev-pair-token <token>` flag for `lapis-daemon --mock` so Android emulator/desktop testing works without real hardware.
* **Acceptance Criteria:**
  * `uv run pytest` passes full API, WebSocket, auth rejection (`401`), secret masking (never returns plaintext), and 25 MB screenshot ring-buffer eviction tests.
  * Daemon binds strictly to `127.0.0.1:8421`; steady RSS overhead of `aiohttp` + WS hub is $\le 15\text{ MB}$.

### Phase A1: CardputerZero BLE Provisioning Service & QR Pairing View
* **Deliverables:**
  1. Implement `daemon/src/lapis_daemon/ble_prov/` (`crypto.py` X25519 + HKDF + AES-256-GCM, `gatt_server.py` BlueZ D-Bus GATT peripheral, `network_ctl.py` `nmcli` + `tailscale up` + `tailscale serve --bg --https=443 http://127.0.0.1:8421`).
  2. Implement `ui/src/views/pairing.rs` in Rust (`lapis-ui`): renders both the **6-digit BLE Pairing PIN** screen and the crisp pixel-exact **$125 \times 125$ QR Code** pairing screen + `screen.png` framebuffer snapshot IPC command.
  3. Update `os/stage-lapis/` to install `bluez`, `network-manager`, `lapis-ble-prov.service` (disabled by default, activated on-demand), and verify `tailscale serve` persistence across reboots.
* **Acceptance Criteria:**
  * Round-trip unit tests pass for X25519/AES-GCM chunked GATT encryption and QR payload parsing.
  * `--dump-frame /tmp/pairing_pin.png --view pairing_pin` and `--dump-frame /tmp/pairing_qr.png --view pairing_qr` produce scannable, unclipped $320 \times 170$ PNGs.
  * `shellcheck` passes on all updated `os/` scripts.

### Phase A2: Android Project Scaffold, Theme & Tailscale API/WS Client
* **Deliverables:**
  1. Initialize Gradle Kotlin project in `android/` (Kotlin 2.2+, Compose BOM `2026.11.01`, Material 3, Room, OkHttp, Kotlinx Serialization, Keystore token vault).
  2. Implement `LapisTheme.kt` (high-contrast Ink/Paper/LapisBlue/Amber aesthetic matching `lapis-ui`).
  3. Implement `LapisApiClient.kt`, `LapisWsClient.kt` (auto-reconnect + heartbeat), `ProtocolModels.kt`, and `DeviceRepository.kt` with a manual "Dev Connect (`10.0.2.2:8421`)" option for Android Emulator testing against `lapis-daemon --mock`.
* **Acceptance Criteria:**
  * `./gradlew testDebugUnitTest assembleDebug` passes cleanly.
  * Android Emulator connects to local `lapis-daemon --mock`, receives `state_sync` over WebSocket, and survives daemon restarts with automatic reconnect.

### Phase A3: Full Conversational Agent Chats, Voice Input & Live KVM Approvals
* **Deliverables:**
  1. Implement `ui/chats/` (Chat list, host filter chips, multi-turn conversation thread, collapsible tool-call cards, inline screenshot previews, voice PTT input, and global Kill-Switch button).
  2. Implement `ui/kvm/` (pinch-to-zoom full-color screenshot viewer with click-point overlays, sticky Approval Sheet, and Manual Assist "Nudge" dialog).
  3. Implement `notifications/ApprovalNotifier.kt` and `ApprovalActionReceiver.kt` for background approval alerts with inline Approve/Deny actions.
* **Acceptance Criteria:**
  * End-to-end test with `lapis-daemon --mock` + `lapis-ui --sim` + Android Emulator: sending a chat message on Android streams live steps to **both** Android and `lapis-ui`, pops an approval modal on both, and approving on Android immediately clears the modal on `lapis-ui` and resumes the agent.
  * Background notification Approve/Deny action resolves a pending approval cleanly.

### Phase A4: Device Cockpit, Screen Mirror & Complete Remote Configuration UI
* **Deliverables:**
  1. Implement `ui/device/` (BQ27220 battery/RAM/zram/CPU/Tailscale telemetry cards, LCD theme/brightness/mute/wake controls, live $320 \times 170$ framebuffer mirror viewer, and reboot/shutdown actions).
  2. Implement `ui/config/` (Tailscale KVM Host editor with live "Test VNC & SSH" probe feedback, LLM/Agent settings, write-only Secrets Vault, Cron Schedules manager with "Run Now", and Paired Devices revocation list).
* **Acceptance Criteria:**
  * Toggling `Paper White` theme or brightness in Android updates `lapis-ui` in real time.
  * Adding a host, probing it, updating a write-only secret, and triggering a scheduled job all persist to `/etc/lapis/` and SQLite without exposing plaintext secrets over the API.

### Phase A5: Android BLE & QR Zero-Cable Onboarding Flow
* **Deliverables:**
  1. Implement `data/ble/BleProvisioner.kt` (Nordic BLE KTX + Android 12+ permission flow + MTU negotiation + chunk reassembly) and `BleEnvelopeCrypto.kt` (matching `daemon/ble_prov/crypto.py` byte-for-byte).
  2. Implement `ui/onboarding/` wizard (BLE scan -> 6-digit PIN entry -> Wi-Fi list & connect -> Tailscale Custom Tab login or Auth Key -> optional API key push -> Tailscale HTTPS verification -> BLE teardown) plus CameraX QR Scanner path.
* **Acceptance Criteria:**
  * Cross-language crypto test vectors verify Kotlin `BleEnvelopeCrypto.kt` <-> Python `ble_prov/crypto.py` interoperability.
  * QR pairing flow completes end-to-end against `lapis-daemon --mock` and stores the bearer token securely in Android Keystore.
  * On real CardputerZero hardware, a freshly flashed SD card pairs over BLE, joins Wi-Fi + Tailscale, turns off BLE provisioning, and lands directly in the Android Chats workspace without ever connecting a cable or editing files on the SD card.