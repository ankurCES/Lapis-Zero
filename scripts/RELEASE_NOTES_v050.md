# Lapis v0.5.0 — combined CardputerZero + uConsole release

This release carries both device images plus the Android companion app(s).

## What's in this release

### CardputerZero image — `lapis-zero-arm64.img.xz`
Debian Trixie arm64 for the M5Stack CardputerZero (CM0, 512 MB). Same
monochrome agent OS as before, rebuilt 2026-11-26 with this round's daemon:

- **BLE paired-phone Wi-Fi change** (no PIN): an already-paired phone proves its
  `api_token` over BLE and can `wifi_scan` / `wifi_connect` / `finish_provisioning`
  only. Wrong or revoked tokens count as failed attempts (3-strike lockout).
- **New REST endpoints**: `GET /v1/wifi`, `GET /v1/wifi/scan`, `POST /v1/wifi/connect`,
  `POST /v1/wifi/forget`, `POST /v1/wifi/ble`.
- **Passwords never on a command line**: secured networks get a root `0600`
  NetworkManager keyfile + `nmcli connection load` / `up uuid` (deleted on failure)
  instead of `nmcli … password`.
- Cross-language `paired_vector` added to `tests/vectors/ble_crypto_vectors.json`.
- `daemon` test suite: 301 passed, 1 xfailed.

**flash:**
```sh
# macOS / Linux
xzcat lapis-zero-arm64.img.xz | sudo dd of=/dev/rdiskX bs=4m
# (Linux: /dev/sdX) — rdiskX is the raw device; confirm it in `diskutil list` first
```
Windows: flash the `.img.xz` with BalenaEtcher or Rufus (extract with 7-Zip first).

### Lapis-Deck image — `lapis-deck-cm4-trixie-arm64.img.xz`
Headless Debian 13 trixie arm64 cyberdeck OS for the ClockworkPi uConsole (CM4).
DRM/KMS HUD, SDR, Meshtastic, GPS, NetOps and the same policy-gated agent harness
as Lapis-Zero; no X11.

This round adds:

- HUD **fleet workspace** (add a tailnet machine for the agent) and the **NET / Wi-Fi**
  workspace (scan, join, forget, or let the phone set it).
- Same BLE paired-phone Wi-Fi flow, shared protocol with Lapis-Zero.
- HUD screenshot walkthrough in the README.

### Android companion app(s)
Both are `dev.lapiszero.app` (same identity), rebuilt 2026-11-26 with the new
adaptive launcher icon, "Lapis Zero" launcher name and the "Pair a new Lapis Zero"
device card:

- **`app-debug.apk`** — the `android/` tree (what prior releases carried).
- **`lapis-deck-companion-debug.apk`** — the `uConsoleCM4/companion-android` tree;
  this copy has the Wi-Fi-change flow and KVM-hosts / fleet features, and is the one
  to install for the Lapis-Deck (uConsole). Same applicationId, so install one or the
  other, not both.

## Checksums (SHA-256)

| asset | size | sha256 |
|---|---|---|
| `lapis-zero-arm64.img.xz` | 639,160,516 | `d8678faaf825dda1866fe88cb83876103fa1c810bf0b02bd75ce85ad89ca7354` |
| `lapis-deck-cm4-trixie-arm64.img.xz` | 839,337,592 | `676c9e33f1a45d3bf0c1b4f4903397ee22008a4dcdb4ac51dfcf562a1e2a12ab` |
| `app-debug.apk` | 51,858,249 | `869256d3c6cb3a20e2a4f50de2afc4d6bdd9f11d1088caa2eaabf9e4ee1001df` |
| `lapis-deck-companion-debug.apk` | 52,677,912 | `b5b6a7302116a1037eaa63c1312a9a92b0b598f0584011ec0922d51c7b077c0a` |

Repository: https://github.com/ankurCES/Lapis-Zero (Apache-2.0, Rust + Python + Kotlin)