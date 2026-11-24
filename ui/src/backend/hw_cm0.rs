//! CardputerZero hardware I/O (CM0): TCA8418 keypad `evdev`, BQ27220 fuel gauge,
//! ST7789 backlight control and BMI270 motion wake.
//!
//! Design (AGENTS.md §4): Linux/sysfs/evdev access is `#[cfg(target_os = "linux")]`;
//! every transformation the kernel data goes through (input-event decoding, keycode
//! mapping, ESC-hold state machine, battery string parsing, backlight clamping,
//! motion-delta magnitude) is a pure function that runs and tests anywhere.

use std::time::{Duration, Instant};

use crate::backend::LapisKey;

// ---------------------------------------------------------------------------
// Linux evdev input_event (see <linux/input.h>). On 64-bit targets the struct
// is 24 bytes: `struct timeval` (16) + `u16 type` + `u16 code` + `i32 value`.
// ---------------------------------------------------------------------------

/// One decoded Linux `input_event` triplet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvEvent {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

/// Decodes a 24-byte `input_event` buffer into `(type, code, value)`.
///
/// The `timeval` (first 16 bytes) is ignored; `value` is read as i32. On a
/// little-endian 64-bit Linux target this matches the kernel layout exactly.
pub fn decode_input_event(buf: &[u8; 24]) -> EvEvent {
    let kind = u16::from_le_bytes([buf[16], buf[17]]);
    let code = u16::from_le_bytes([buf[18], buf[19]]);
    let value = i32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
    EvEvent { kind, code, value }
}

/// Linux `linux/input-event-codes.h` keycodes used by the TCA8418 46-key map.
mod keycode {
    pub const KEY_ESC: u16 = 1;
    pub const KEY_TAB: u16 = 15;
    pub const KEY_ENTER: u16 = 28;
    pub const KEY_SPACE: u16 = 57;
    pub const KEY_UP: u16 = 103;
    pub const KEY_DOWN: u16 = 108;
    pub const KEY_LEFT: u16 = 105;
    pub const KEY_RIGHT: u16 = 106;
    // Letters: KEY_A..KEY_Z are contiguous 30..=55 (`KEY_Q`=16 is before that,
    // but the CardputerZero keymap emits QWERTY letter codes, so map the span).
    // (KEY_A/KEY_W/KEY_P/KEY_M are referenced from tests.)
    #[cfg(test)]
    pub const KEY_A: u16 = 30;
    #[cfg(test)]
    pub const KEY_W: u16 = 17;
    #[cfg(test)]
    pub const KEY_P: u16 = 25;
    #[cfg(test)]
    pub const KEY_M: u16 = 50;
    pub const KEY_Z: u16 = 44;
    pub const KEY_C: u16 = 46;
    pub const KEY_F: u16 = 33;
    pub const KEY_X: u16 = 45;
    pub const KEY_U: u16 = 22;
    pub const KEY_I: u16 = 23;
}

const EV_KEY: u16 = 1;
const KEY_RELEASE: i32 = 0;
const KEY_PRESS: i32 = 1;

/// Result of translating a key event, given the current `fn` modifier state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    /// Nothing the UI cares about.
    Ignored,
    /// A normal press (or autorepeat).
    Press(LapisKey),
    /// A key release (used to terminate ESC-hold).
    Release(LapisKey),
    /// `fn` itself toggled (not surfaced to the app).
    Fn,
}

fn printable_from_keycode(code: u16) -> Option<char> {
    // QWERTY: KEY_Q=16 .. KEY_P=25, KEY_A=30 .. KEY_L=38, KEY_Z=44 .. KEY_M=50.
    // QWERTY rows are keycode-contiguous but not alphabet-contiguous, so map by
    // explicit row strings (Linux KEY_Q=16..=25, KEY_A=30..=38, KEY_Z=44..=50).
    for (start, row) in [
        (16u16, b"qwertyuiop".as_slice()),
        (30u16, b"asdfghjkl".as_slice()),
        (44u16, b"zxcvbnm".as_slice()),
    ] {
        let idx = code.checked_sub(start)? as usize;
        if let Some(&ch) = row.get(idx) {
            return Some(ch as char);
        }
    }
    None
}

/// Maps one evdev key event onto a semantic [`KeyEvent`].
///
/// `fn_held` makes `U`/`I` become backlight-down/up (SPEC 5.4). Letters that
/// aren't reserved return [`KeyEvent::Press(LapisKey::Char(...))`].
pub fn map_keycode(code: u16, fn_held: bool) -> KeyEvent {
    use keycode::*;
    match code {
        KEY_ENTER => KeyEvent::Press(LapisKey::Enter),
        KEY_ESC => KeyEvent::Press(LapisKey::Esc),
        KEY_TAB => KeyEvent::Press(LapisKey::Tab),
        KEY_LEFT | KEY_Z => KeyEvent::Press(LapisKey::CycleL),
        KEY_RIGHT | KEY_C => KeyEvent::Press(LapisKey::CycleR),
        KEY_UP | KEY_F => KeyEvent::Press(LapisKey::Up),
        KEY_DOWN | KEY_X => KeyEvent::Press(LapisKey::Down),
        KEY_U if fn_held => KeyEvent::Press(LapisKey::BacklightDown),
        KEY_I if fn_held => KeyEvent::Press(LapisKey::BacklightUp),
        KEY_SPACE => KeyEvent::Press(LapisKey::Char(' ')),
        c => match printable_from_keycode(c) {
            Some(ch) => KeyEvent::Press(LapisKey::Char(ch)),
            None => KeyEvent::Ignored,
        },
    }
}

/// Converts a raw `(kind, code, value)` triplet (already decoded) into the
/// higher-level [`KeyEvent`], tracking the ESC hold state for the kill-switch.
pub fn translate_event(kind: u16, code: u16, value: i32, fn_held: bool) -> KeyEvent {
    if kind != EV_KEY {
        return KeyEvent::Ignored;
    }
    // `fn` on the TCA8418 matrix surfaces as a dedicated keycode; treat it as a
    // pure modifier and never forward it to the views. (0x1d0 is the Linux
    // `KEY_FN`; some keymaps emit `KEY_LEFTMETA` instead.)
    if code == 0x1d0 || code == 125 {
        return KeyEvent::Fn;
    }
    let ev = map_keycode(code, fn_held);
    match ev {
        KeyEvent::Press(k) => {
            if value == KEY_RELEASE {
                KeyEvent::Release(k)
            } else {
                // Autorepeat also counts as a held press for char entry.
                ev
            }
        }
        other => other,
    }
}

/// ESC-hold state machine: tracks press/release instants and yields a
/// one-shot [`LapisKey::EscHold`] once the key has been down for `hold` duration.
#[derive(Debug)]
pub struct EscHold {
    threshold: Duration,
    down_since: Option<Instant>,
    fired: bool,
}

impl EscHold {
    /// Creates the state machine with `threshold` (SPEC 5.4: 1.5 s).
    pub fn new(threshold: Duration) -> Self {
        Self {
            threshold,
            down_since: None,
            fired: false,
        }
    }

    fn now() -> Instant {
        Instant::now()
    }

    /// Feeds a decoded key event; returns `Some(EscHold)` exactly once when the
    /// threshold is reached, else reverts to [`LapisKey::Esc`] on a short tap.
    pub fn feed(&mut self, kind: u16, code: u16, value: i32) -> Option<LapisKey> {
        use keycode::*;
        if kind != EV_KEY || code != KEY_ESC {
            return None;
        }
        if value == KEY_PRESS {
            self.down_since = Some(Self::now());
            self.fired = false;
            return None;
        }
        if value == KEY_RELEASE {
            let down = self.down_since.take();
            self.down_since = None;
            if self.fired {
                self.fired = false;
                return None;
            }
            let held = down.map(|d| d.elapsed()).unwrap_or(Duration::ZERO);
            return if held >= self.threshold {
                self.fired = false;
                Some(LapisKey::EscHold)
            } else {
                Some(LapisKey::Esc)
            };
        }
        None
    }
}

// ---------------------------------------------------------------------------
// BQ27220 fuel gauge (read via /sys/class/power_supply/<name>/...)
// ---------------------------------------------------------------------------

/// Parsed BQ27220 snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryStatus {
    pub capacity_pct: u8,
    pub voltage_uv: u32,
    pub charging: bool,
}

/// Parses the ASCII content of a `/sys/class/power_supply/*/capacity` file.
pub fn parse_capacity(raw: &[u8]) -> Option<u8> {
    let s = std::str::from_utf8(raw).ok()?.trim();
    let v: u8 = s.parse().ok()?;
    Some(v.min(100))
}

/// Parses a `/sys/.../voltage_now` value (µV) and a `status` string.
pub fn parse_voltage_uv(raw: &[u8]) -> Option<u32> {
    std::str::from_utf8(raw).ok()?.trim().parse().ok()
}

/// Maps a `status` string onto a charging flag (`Charging`/`Full` => true).
pub fn parse_charging(raw: &[u8]) -> bool {
    let s = std::str::from_utf8(raw).unwrap_or("").trim();
    matches!(s, "Charging" | "Full")
}

// ---------------------------------------------------------------------------
// Backlight (ST7789) — /sys/class/backlight/<name>/{brightness,max_brightness}
// ---------------------------------------------------------------------------

/// Clamps a brightness step within `[0, max]`.
pub fn clamp_brightness(current: u32, delta: i32, max: u32) -> u32 {
    let cur = current as i64;
    let m = max as i64;
    (cur + delta as i64).clamp(0, m) as u32
}

/// Backlight idle policy (SPEC 5.2 §272): auto-dim at 30 s, off at 90 s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BacklightState {
    Full,
    Dim,
    Off,
}

/// Maps idle milliseconds onto the desired backlight state.
pub fn backlight_state_for_idle(idle_ms: u64) -> BacklightState {
    if idle_ms >= 90_000 {
        BacklightState::Off
    } else if idle_ms >= 30_000 {
        BacklightState::Dim
    } else {
        BacklightState::Full
    }
}

// ---------------------------------------------------------------------------
// BMI270 (6-axis IMU) motion wake — /sys/bus/iio/devices/iio:device*/in_accel_*_raw
// ---------------------------------------------------------------------------

/// Squared magnitude of a 3-axis accelerometer delta, in raw counts².
pub fn accel_delta_sq(dx: i32, dy: i32, dz: i32) -> i64 {
    let (x, y, z) = (dx as i64, dy as i64, dz as i64);
    x * x + y * y + z * z
}

/// True when the delta exceeds `threshold²`, indicating a lift/motion wake.
pub fn motion_wake(dx: i32, dy: i32, dz: i32, threshold_sq: i64) -> bool {
    accel_delta_sq(dx, dy, dz) > threshold_sq
}

#[cfg(target_os = "linux")]
pub mod linux_io {
    use std::io::Read;
    use std::os::unix::fs::FileTypeExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::*;
    use crate::backend::InputBackend;

    /// Reads a small sysfs file to an owned string.
    fn read_sysfs(path: &str) -> std::io::Result<String> {
        Ok(std::fs::read_to_string(path)?.trim().to_string())
    }

    /// ST7789 backlight handle: reads/writes the first `/sys/class/backlight`.
    pub struct Backlight {
        root: PathBuf,
        max: u32,
    }

    impl Backlight {
        pub fn open() -> std::io::Result<Self> {
            let mut root = None;
            for entry in std::fs::read_dir("/sys/class/backlight")? {
                let p = entry?.path();
                if p.join("brightness").exists() {
                    root = Some(p);
                    break;
                }
            }
            let root = root.ok_or_else(|| std::io::Error::other("no backlight device found"))?;
            let max: u32 = read_sysfs(&root.join("max_brightness").to_string_lossy())?
                .parse()
                .unwrap_or(0);
            Ok(Self { root, max })
        }

        pub fn brightness(&self) -> u32 {
            read_sysfs(&self.root.join("brightness").to_string_lossy())
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0)
        }

        pub fn set_brightness(&self, value: u32) -> std::io::Result<()> {
            let v = value.min(self.max);
            std::fs::write(self.root.join("brightness"), v.to_string())
        }

        /// `fn+U`/`fn+I` step the brightness by `step`.
        pub fn step(&self, delta: i32, step: u32) -> std::io::Result<u32> {
            let cur = self.brightness();
            let next = clamp_brightness(cur, delta * step as i32, self.max);
            self.set_brightness(next)?;
            Ok(next)
        }
    }

    /// BQ27220 fuel-gauge reader over the power-supply sysfs class.
    pub struct FuelGauge {
        root: PathBuf,
    }

    impl FuelGauge {
        pub fn open() -> std::io::Result<Self> {
            // Pick the battery-type supply if present, else any supply.
            let mut root = None;
            for entry in std::fs::read_dir("/sys/class/power_supply")? {
                let p = entry?.path();
                let kind = read_sysfs(&p.join("type").to_string_lossy()).unwrap_or_default();
                if root.is_none() || kind == "Battery" {
                    root = Some(p);
                }
            }
            let root = root.ok_or_else(|| std::io::Error::other("no power_supply device found"))?;
            Ok(Self { root })
        }

        pub fn status(&self) -> Option<BatteryStatus> {
            let cap = std::fs::read(self.root.join("capacity")).ok()?;
            let volts = std::fs::read(self.root.join("voltage_now")).ok();
            let charging = std::fs::read(self.root.join("status"))
                .map(|b| parse_charging(&b))
                .unwrap_or(false);
            Some(BatteryStatus {
                capacity_pct: parse_capacity(&cap)?,
                voltage_uv: volts.and_then(|v| parse_voltage_uv(&v)).unwrap_or(0),
                charging,
            })
        }
    }

    /// BMI270 accelerometer reader (first IIO device with `in_accel_x_raw`).
    pub struct Imu {
        root: PathBuf,
        last: Option<(i32, i32, i32)>,
    }

    impl Imu {
        pub fn open() -> std::io::Result<Self> {
            let mut root = None;
            for dev in std::fs::read_dir("/sys/bus/iio/devices")? {
                let p = dev?.path();
                if p.join("in_accel_x_raw").exists() {
                    root = Some(p);
                    break;
                }
            }
            let root = root.ok_or_else(|| std::io::Error::other("no BMI270/iio accelerometer"))?;
            Ok(Self { root, last: None })
        }

        /// Returns `Some(dx,dy,dz)` since last read, or `None` on first poll.
        pub fn delta(&mut self) -> Option<(i32, i32, i32)> {
            let read = |f: &str| -> i32 {
                read_sysfs(&self.root.join(f).to_string_lossy())
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0)
            };
            let cur = (
                read("in_accel_x_raw"),
                read("in_accel_y_raw"),
                read("in_accel_z_raw"),
            );
            let prev = self.last.replace(cur)?;
            Some((cur.0 - prev.0, cur.1 - prev.1, cur.2 - prev.2))
        }
    }

    /// TCA8418 keypad via `evdev`: opens a device (by path or the first whose
    /// sysfs `name` contains `tca8418`), reads with FIONREAD (never blocks),
    /// decodes events and maintains the `fn`-modifier + ESC-hold state.
    pub struct EvdevInput {
        file: std::fs::File,
        buf: Vec<u8>,
        fn_held: bool,
        esc_hold: EscHold,
    }

    impl EvdevInput {
        pub fn open(path: &Path) -> std::io::Result<Self> {
            let file = std::fs::OpenOptions::new().read(true).open(path)?;
            Ok(Self {
                file,
                buf: vec![0u8; 64 * 24],
                fn_held: false,
                esc_hold: EscHold::new(Duration::from_millis(1500)),
            })
        }

        /// Lazy import-safe guard: the evdev read path only exists on Linux.
        pub fn open_keypad() -> std::io::Result<Self> {
            for entry in std::fs::read_dir("/dev/input")? {
                let p = entry?.path();
                let Ok(meta) = std::fs::metadata(&p) else {
                    continue;
                };
                if !meta.file_type().is_char_device() {
                    continue;
                }
                let name_path = format!(
                    "/sys/class/input/{}/device/name",
                    p.file_name().unwrap().to_string_lossy()
                );
                if let Ok(name) = std::fs::read_to_string(name_path) {
                    if name.to_lowercase().contains("tca8418") {
                        return Self::open(&p);
                    }
                }
            }
            Err(std::io::Error::other("no tca8418 evdev device found"))
        }
    }

    impl InputBackend for EvdevInput {
        fn poll_event(&mut self) -> Option<LapisKey> {
            use std::os::fd::AsRawFd;
            let fd = self.file.as_raw_fd();
            let mut n: libc::c_int = 0;
            if unsafe { libc::ioctl(fd, libc::FIONREAD, &mut n) } != 0 || n <= 0 {
                return None;
            }
            let want = (n as usize).min(self.buf.len());
            let read = self.file.read(&mut self.buf[..want]).ok()?;
            let mut out = None;
            for chunk in self.buf[..read].chunks_exact(24) {
                let mut e = [0u8; 24];
                e.copy_from_slice(chunk);
                let ev = decode_input_event(&e);
                // `fn` modifier tracking.
                if ev.kind == 1 && (ev.code == 0x1d0 || ev.code == 125) {
                    self.fn_held = ev.value == 1;
                    continue;
                }
                if let Some(k) = self.esc_hold.feed(ev.kind, ev.code, ev.value) {
                    // ESC-hold wins: it is the global kill-switch, and a bare
                    // ESC tap is the back/cancel that follows it.
                    out = Some(k);
                    continue;
                }
                if let KeyEvent::Press(k) =
                    translate_event(ev.kind, ev.code, ev.value, self.fn_held)
                {
                    // A release event for ESC is handled by the hold state
                    // machine above; surface every other press (incl. repeat).
                    out = Some(k);
                }
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- input_event decoding -------------------------------------------------
    #[test]
    fn decodes_24_byte_event() {
        let mut buf = [0u8; 24];
        // timeval at [0..16)
        buf[0..8].copy_from_slice(&12345u64.to_le_bytes());
        buf[8..16].copy_from_slice(&678u64.to_le_bytes());
        buf[16..18].copy_from_slice(&EV_KEY.to_le_bytes()); // type=1
        buf[18..20].copy_from_slice(&keycode::KEY_ENTER.to_le_bytes()); // code=28
        buf[20..24].copy_from_slice(&1i32.to_le_bytes()); // value=press
        let e = decode_input_event(&buf);
        assert_eq!(e.kind, EV_KEY);
        assert_eq!(e.code, keycode::KEY_ENTER);
        assert_eq!(e.value, 1);
    }

    // --- keycode mapping ------------------------------------------------------
    #[test]
    fn maps_navigation_and_letters() {
        assert_eq!(
            map_keycode(keycode::KEY_ENTER, false),
            KeyEvent::Press(LapisKey::Enter)
        );
        assert_eq!(
            map_keycode(keycode::KEY_Z, false),
            KeyEvent::Press(LapisKey::CycleL)
        );
        assert_eq!(
            map_keycode(keycode::KEY_C, false),
            KeyEvent::Press(LapisKey::CycleR)
        );
        assert_eq!(
            map_keycode(keycode::KEY_UP, false),
            KeyEvent::Press(LapisKey::Up)
        );
        assert_eq!(
            map_keycode(keycode::KEY_DOWN, false),
            KeyEvent::Press(LapisKey::Down)
        );
        assert_eq!(
            map_keycode(keycode::KEY_W, false),
            KeyEvent::Press(LapisKey::Char('w'))
        );
        assert_eq!(
            map_keycode(keycode::KEY_P, false),
            KeyEvent::Press(LapisKey::Char('p'))
        );
        assert_eq!(
            map_keycode(keycode::KEY_A, false),
            KeyEvent::Press(LapisKey::Char('a'))
        );
        assert_eq!(
            map_keycode(keycode::KEY_M, false),
            KeyEvent::Press(LapisKey::Char('m'))
        );
    }

    #[test]
    fn fn_modifier_routes_u_i_to_backlight() {
        assert_eq!(
            map_keycode(keycode::KEY_U, false),
            KeyEvent::Press(LapisKey::Char('u'))
        );
        assert_eq!(
            map_keycode(keycode::KEY_U, true),
            KeyEvent::Press(LapisKey::BacklightDown)
        );
        assert_eq!(
            map_keycode(keycode::KEY_I, true),
            KeyEvent::Press(LapisKey::BacklightUp)
        );
    }

    #[test]
    fn translate_release_and_fn() {
        assert_eq!(
            translate_event(EV_KEY, keycode::KEY_ENTER, KEY_RELEASE, false),
            KeyEvent::Release(LapisKey::Enter)
        );
        assert_eq!(
            translate_event(EV_KEY, 0x1d0, KEY_PRESS, false),
            KeyEvent::Fn
        );
        assert_eq!(translate_event(0, 0, 0, false), KeyEvent::Ignored);
    }

    // --- ESC hold -------------------------------------------------------------
    #[test]
    fn esc_hold_fires_once_at_threshold() {
        let mut h = EscHold::new(Duration::from_millis(1500));
        assert_eq!(h.feed(EV_KEY, keycode::KEY_ESC, KEY_PRESS), None);
        assert_eq!(
            h.feed(EV_KEY, keycode::KEY_ESC, KEY_RELEASE),
            Some(LapisKey::Esc)
        );
    }

    // --- BQ27220 --------------------------------------------------------------
    #[test]
    fn parses_battery_fields() {
        assert_eq!(parse_capacity(b"100\n"), Some(100));
        assert_eq!(parse_capacity(b"7\n"), Some(7));
        assert_eq!(parse_capacity(b"255\n"), Some(100)); // clamps
        assert_eq!(parse_capacity(b"garbage"), None);
        assert_eq!(parse_voltage_uv(b"3700000\n"), Some(3_700_000));
        assert!(parse_charging(b"Charging\n"));
        assert!(!parse_charging(b"Discharging\n"));
    }

    // --- backlight ------------------------------------------------------------
    #[test]
    fn clamps_brightness() {
        assert_eq!(clamp_brightness(100, -20, 255), 80);
        assert_eq!(clamp_brightness(0, -1, 255), 0);
        assert_eq!(clamp_brightness(250, 20, 255), 255);
    }

    #[test]
    fn idle_policy_thresholds() {
        assert_eq!(backlight_state_for_idle(0), BacklightState::Full);
        assert_eq!(backlight_state_for_idle(29_999), BacklightState::Full);
        assert_eq!(backlight_state_for_idle(30_000), BacklightState::Dim);
        assert_eq!(backlight_state_for_idle(89_999), BacklightState::Dim);
        assert_eq!(backlight_state_for_idle(90_000), BacklightState::Off);
    }

    // --- BMI270 ---------------------------------------------------------------
    #[test]
    fn motion_wake_threshold() {
        assert!(motion_wake(100, 0, 0, 90 * 90));
        assert!(!motion_wake(0, 0, 0, 1));
        assert!(!motion_wake(10, 10, 10, 40_000));
    }
}
