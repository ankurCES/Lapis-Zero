# AGENTS.md — Operational Rules for Coding Agents

This file governs all autonomous and assisted engineering work on **Lapis-Deck**. Read `SPEC.md` completely before writing code.

---

## 1. Core Architectural Invariants (Never Violate)

1. **No X11 / No Desktop Environment:** Never add Xorg, GTK/Qt desktop sessions, GNOME, XFCE, LXDE, or display managers to the OS image or UI dependencies. `lapis-ui` must run natively on DRM/KMS (with a desktop-window fallback feature flag strictly for host development).
2. **Loopback-Only Control Plane:** `lapis-daemon` must **only** bind `127.0.0.1:8421`. Remote access is exclusively via `tailscale serve` HTTPS/WSS with Bearer Token authentication.
3. **Deny-by-Default Policy Gate:** No agent tool—local or remote—may bypass `lapis-daemon`'s Policy Gate. Tier 1 and Tier 2 actions must require explicit approval tokens; never auto-approve active RF transmission, active network probing, or destructive commands.
4. **Zero Secret Leakage:** API keys, Tailscale auth keys, Wi-Fi PSKs, bearer tokens, and SSH keys must pass through `lapis-crypto::redact` before entering any log, UI event, WebSocket frame, or crash report.
5. **Cross-Language Crypto Parity:** Any change to BLE/QR provisioning crypto must update `tests/vectors/ble_crypto_vectors.json` first and pass both Rust (`cargo test -p lapis-crypto`) and Kotlin (`./gradlew test`) vector tests.

---

## 2. Language & Codebase Conventions

- **Rust (`crates/`):**
  - Edition 2021+, `tokio` async runtime, `axum` for local HTTP/WS, `tracing` for structured logs, `thiserror`/`anyhow` for errors.
  - Zero `unwrap()` / `expect()` in production daemon or UI paths; handle hardware disconnects (unplugged SDR, missing GPS, absent AIO board) gracefully with degraded status indicators.
  - All hardware interfaces (AXP228 PMIC, GPIO `pinctrl`, SoapySDR, BLE GATT, `meshtasticd`, `gpsd`) must sit behind Rust traits with **deterministic mock implementations** so the entire stack compiles and tests cleanly on x86_64/macOS dev hosts without a physical uConsole.
- **Python (`harness/`):**
  - Python 3.12+, managed via `uv`, strict typing (`mypy` / `pyright`), `ruff` for lint/format, `pytest` + `pytest-asyncio` for tests.
  - Remote harness adapters (`claude`, `opencode`, `freya`, `gemini`, `cursor-agent`, `codex`) must implement a unified `RemoteHarnessAdapter` protocol with clean timeouts and streaming output.
- **Kotlin (`companion-android/`):**
  - Kotlin + Jetpack Compose + Coroutines/Flow + OkHttp (WSS).
  - Must pass unit tests including `ble_crypto_vectors.json` verification and assemble a clean debug APK (`./gradlew assembleDebug`).
- **OS Image (`os-image/`):**
  - Shell scripts must use `set -euo pipefail` and pass `shellcheck`.
  - Never hardcode personal credentials, Wi-Fi SSIDs, or Tailscale keys in image scripts.

---

## 3. Execution Discipline & Verification Gates

Work strictly **phase by phase** according to Section 8 of `SPEC.md`. Do not skip ahead or leave placeholder `TODO`/`unimplemented!()` stubs in completed phases.

Before marking any phase complete, you **must** run and pass all applicable checks:
- **Rust:** `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
- **Python:** `cd harness && uv run ruff check . && uv run pytest`
- **Android (Phase 5+):** `cd companion-android && ./gradlew test assembleDebug`
- **Shell/OS (Phase 6):** `shellcheck os-image/**/*.sh`

If a tool or SDK (e.g., Android SDK or SoapySDR system libs) is unavailable in the current environment, document the exact missing dependency, keep mock/unit tests passing, and do not fake a passing build.

---

## 4. State & Handoff Tracking

Maintain a `PROGRESS.md` file at the repo root updated at the end of every phase with:
- Completed phase number and summary of files created/modified
- Exact verification commands run and their pass/fail status
- Known hardware-only verification items deferred to physical uConsole CM4 flashing