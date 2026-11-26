# Lapis-Deck — hardware verification checklist (uConsole CM4 + AIO board)

Everything below was built and tested in containers and on a laptop with mock hardware; these
are the items only a real uConsole can confirm. Read the journal with
`tailscale ssh lapis@lapis-deck journalctl -b -u 'lapis-*'`.

## Boot and display
- [ ] Boots to the HUD in < 12 s from power-on (SPEC 2); measure with `systemd-analyze`.
- [ ] The boot splash (Plymouth, Lapis theme) appears once the panel driver is up, upright, gem
      breathing, dots running, and stays until the HUD's first frame (no black gap, no console
      text). Sideways or upside down: change `TURN` in
      `/usr/share/plymouth/themes/lapis/lapis.script` the same way as `--rotate` for the HUD.
- [ ] `lapis-ui` logs `display /dev/dri/cardN 720x1280` and becomes DRM master (no `set crtc` error).
- [ ] The HUD is upright. If it is upside down, set `--rotate 270` in `lapis-ui.service`
      (default: 90° when the panel reports portrait; labwc on this panel uses transform 270, the same turn).
- [ ] Brightness from SYSTEM changes `/sys/class/backlight/backlight@0` (max 9); never goes black.
- [ ] No console text or cursor over the HUD; tty1 has no getty.

## Input
- [ ] Keyboard types into the PTY workspace; `Alt+1..7` switch workspaces; `Ctrl+Space` opens the palette.
- [ ] D-pad (arrow keys on stock firmware) moves selections; A/B/X/Y arrive as BTN_THUMB/BTN_THUMB2/
      BTN_TRIGGER/BTN_TOP and Select/Start as BTN_BASE3/BTN_BASE4 (`evtest`). On QMK firmware, pass
      `--pads a=…,b=…` to `lapis-ui` with the codes `evtest` shows.
- [ ] Select+Start together fires the kill switch.
- [ ] Trackball moves the pointer; `--trackball-gain` tunes it. Clicks tune the waterfall.
- [ ] Holding A for 1.5 s approves a Tier 2 request; a tap does not.

## Power and rails (AIO V2)
- [ ] Battery % / draw (W) in the top bar match `/sys/class/power_supply/axp20x-battery/*`.
- [ ] `pinctrl get 27 16 7 23` reads `hi` after boot (lapis-rails); toggling in SYSTEM changes them.
- [ ] GPIO6 (PPS) and GPIO7 (SDR rail) still work alongside the uConsole overlay (it maps SPI4 to
      GPIO6/7 — a possible clash flagged by research; if so, move PPS or disable that SPI).
- [ ] `rtl_biast -b 1/0` toggles the bias tee; Wi-Fi/Bluetooth rail toggles hit rfkill.
- [ ] 4G module: `uconsole-4g-cm4 enable` (GPIO24/GPIO15) — note it may share pins with the AIO
      board's GPS RX and LoRa Busy; do not fit both.

## Radios
- [ ] RF workspace streams from the internal RTL-SDR (`source: rtl_sdr`, not SYNTHETIC);
      unplugging it shows `degraded`, replugging recovers.
- [ ] `sdr.decode_ism` decodes a 433 MHz sensor; `sdr.demod_audio play=true` is audible.
- [ ] `readsb.service` (when started) writes `/run/readsb/aircraft.json`; ADS-B table fills.
- [ ] meshtasticd starts with the SX1262: `journalctl -u meshtasticd` shows `sx1262 init success`
      and `MAC ADDRESS: 80:00:…` (node ID = last 8 hex digits of the CM4 serial); the boot
      report's `meshtasticd` section has the same lines. MESH lists nodes; sending asks `[Y/N]`
      and transmits. With `[rails] board = "none"` the unit is skipped (condition), not failed.
- [ ] LoRa region: with `mesh_region = "US"` (or yours) in `/etc/lapis/deck.toml` (or seeded as
      `lapis-seed/deck.toml`), the daemon logs `setting the LoRa region from deck.toml` once,
      meshtasticd restarts, and the MESH title shows the region (not `region UNSET, no TX`).
      A Meshtastic node on the same band, region, preset and channel (e.g. a Heltec V3 on
      defaults) shows up in MESH nodes and receives a message from the deck.
- [ ] `ss -ltnp | grep 4403`: note whether meshtasticd's API listens on 0.0.0.0 (LAN-reachable).
- [ ] gpsd sees `/dev/ttyS0` at 9600 baud; GPS shows a 3D fix; `ppstest /dev/pps0` pulses;
      `chronyc sources` selects PPS.
- [ ] RTC: `hwclock -r` reads the PCF85063A.

## Pairing and control plane
- [ ] First boot: SYSTEM shows the BLE PIN; the companion app finds `LapisDeck-XXXX`, pairs,
      joins Wi-Fi and Tailscale, and `tailscale serve status` shows `127.0.0.1:8421`.
- [ ] The app reaches `https://lapis-deck.<tailnet>.ts.net`; chats, approvals, KVM work as with Lapis-Zero.
- [ ] `ss -ltnp` shows lapis-daemon on 127.0.0.1:8421 only, and nothing of ours on 0.0.0.0.
- [ ] Approval prompts appear on the HUD and the phone at the same time; either can decide.
- [ ] Audio: speaker/headphone switching and no shutdown pop (clockworkpi-audio from the base image).
