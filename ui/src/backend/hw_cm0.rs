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

/// Linux `linux/input-event-codes.h` keycodes emitted by the BSP's
/// `tca8418_keypad_m5stack` driver. The driver resolves the `sym`/`fn` layers
/// itself (see `linux,keymap1`/`keymap2` in `cardputerzero-v5-overlay.dts`), so
/// userspace never sees a `KEY_FN`: `fn+U`/`fn+I` arrive as
/// `KEY_BRIGHTNESSDOWN`/`UP`, `fn+F/X/Z/C` as arrow keys.
mod keycode {
    pub const KEY_ESC: u16 = 1;
    pub const KEY_BACKSPACE: u16 = 14;
    pub const KEY_TAB: u16 = 15;
    pub const KEY_ENTER: u16 = 28;
    #[cfg(target_os = "linux")]
    pub const KEY_LEFTSHIFT: u16 = 42;
    #[cfg(target_os = "linux")]
    pub const KEY_RIGHTSHIFT: u16 = 54;
    pub const KEY_LEFTALT: u16 = 56;
    pub const KEY_SPACE: u16 = 57;
    pub const KEY_UP: u16 = 103;
    pub const KEY_LEFT: u16 = 105;
    pub const KEY_RIGHT: u16 = 106;
    pub const KEY_DOWN: u16 = 108;
    pub const KEY_DELETE: u16 = 111;
    pub const KEY_BRIGHTNESSDOWN: u16 = 224;
    pub const KEY_BRIGHTNESSUP: u16 = 225;
    #[cfg(test)]
    pub const KEY_A: u16 = 30;
    #[cfg(test)]
    pub const KEY_W: u16 = 17;
    #[cfg(test)]
    pub const KEY_Z: u16 = 44;
}

const EV_KEY: u16 = 1;
const KEY_RELEASE: i32 = 0;
const KEY_PRESS: i32 = 1;

/// Result of translating a key event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    /// Nothing the UI cares about.
    Ignored,
    /// A press (or an autorepeat of a repeatable key).
    Press(LapisKey),
    /// A key release.
    Release(LapisKey),
}

/// `sym`-layer keycodes -> the symbol printed on the keycap, ported from the
/// BSP console keymap `/usr/share/keymaps/tca8418_keypad_m5stack_keymap.map`.
const SYM_LAYER: [(u16, char); 32] = [
    (26, '!'),
    (27, '@'),
    (39, '#'),
    (40, '$'),
    (41, '%'),
    (43, '^'),
    (51, '&'),
    (52, '*'),
    (53, '('),
    (94, ')'),
    (55, '~'),
    (69, '`'),
    (70, '_'),
    (71, '-'),
    (72, '+'),
    (73, '='),
    (74, '['),
    (75, ']'),
    (76, '{'),
    (77, '}'),
    (79, ';'),
    (80, ':'),
    (81, '\''),
    (82, '"'),
    (83, '<'),
    (85, '>'),
    (86, '\\'),
    (89, '|'),
    (90, ','),
    (91, '.'),
    (92, '/'),
    (93, '?'),
];

fn printable_from_keycode(code: u16, shift: bool) -> Option<char> {
    // Base layer: KEY_1=2..KEY_0=11, KEY_Q=16..=25, KEY_A=30..=38, KEY_Z=44..=50.
    for (start, row) in [
        (2u16, b"1234567890".as_slice()),
        (16u16, b"qwertyuiop".as_slice()),
        (30u16, b"asdfghjkl".as_slice()),
        (44u16, b"zxcvbnm".as_slice()),
    ] {
        if let Some(&ch) = code
            .checked_sub(start)
            .and_then(|idx| row.get(idx as usize))
        {
            let ch = ch as char;
            return Some(if shift { ch.to_ascii_uppercase() } else { ch });
        }
    }
    SYM_LAYER
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, ch)| *ch)
}

/// Maps one evdev keycode onto a semantic key. Letters stay [`LapisKey::Char`];
/// the shared `App::key` turns `z/c/f/x` into navigation outside the Prompt view.
pub fn map_keycode(code: u16, shift: bool) -> KeyEvent {
    use keycode::*;
    match code {
        KEY_ENTER => KeyEvent::Press(LapisKey::Enter),
        KEY_ESC => KeyEvent::Press(LapisKey::Esc),
        KEY_TAB => KeyEvent::Press(LapisKey::Tab),
        KEY_LEFT => KeyEvent::Press(LapisKey::CycleL),
        KEY_RIGHT => KeyEvent::Press(LapisKey::CycleR),
        KEY_UP => KeyEvent::Press(LapisKey::Up),
        KEY_DOWN => KeyEvent::Press(LapisKey::Down),
        KEY_BRIGHTNESSDOWN => KeyEvent::Press(LapisKey::BacklightDown),
        KEY_BRIGHTNESSUP => KeyEvent::Press(LapisKey::BacklightUp),
        KEY_BACKSPACE | KEY_DELETE => KeyEvent::Press(LapisKey::Char('\x08')),
        KEY_SPACE => KeyEvent::Press(LapisKey::Char(' ')),
        c => match printable_from_keycode(c, shift) {
            Some(ch) => KeyEvent::Press(LapisKey::Char(ch)),
            None => KeyEvent::Ignored,
        },
    }
}

/// Converts a decoded `(kind, code, value)` triplet into a [`KeyEvent`].
/// `value`: 0 release, 1 press, 2 autorepeat.
pub fn translate_event(kind: u16, code: u16, value: i32, shift: bool) -> KeyEvent {
    if kind != EV_KEY {
        return KeyEvent::Ignored;
    }
    // Push-to-Talk is the `opt`/alt key held down (SPEC 5.3): surface the two
    // edges only, never the autorepeat in between.
    if code == keycode::KEY_LEFTALT {
        return match value {
            KEY_PRESS => KeyEvent::Press(LapisKey::PushToTalk(true)),
            KEY_RELEASE => KeyEvent::Press(LapisKey::PushToTalk(false)),
            _ => KeyEvent::Ignored,
        };
    }
    match map_keycode(code, shift) {
        KeyEvent::Press(k) if value == KEY_RELEASE => KeyEvent::Release(k),
        // Autorepeat must never re-fire a confirm/cancel (a held ENTER would
        // approve the *next* request too); only navigation and text repeat.
        KeyEvent::Press(LapisKey::Enter | LapisKey::Esc | LapisKey::Tab) if value != KEY_PRESS => {
            KeyEvent::Ignored
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

    /// Called every frame: fires [`LapisKey::EscHold`] once, while ESC is still
    /// down, as soon as the threshold elapses (the kill-switch must not wait
    /// for the release).
    pub fn poll(&mut self) -> Option<LapisKey> {
        let down = self.down_since?;
        if self.fired || down.elapsed() < self.threshold {
            return None;
        }
        self.fired = true;
        Some(LapisKey::EscHold)
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

/// Maps a `ui_settings.brightness` percentage (SPEC 6.3) onto a sysfs level:
/// that share of `max`, rounded, but never 0 — only the idle policy may turn an
/// awake screen dark.
pub fn brightness_level(pct: u8, max: u32) -> u32 {
    let level = (max as u64 * pct.min(100) as u64 + 50) / 100;
    (level as u32).max(1).min(max)
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

        pub fn max(&self) -> u32 {
            self.max
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

    /// Sleeps until the next frame is due, or earlier if a key or a daemon frame
    /// arrives. At the idle rate (1 FPS) a plain sleep would make the first key
    /// press, an approval or a screen request wait up to a second; `poll(2)` keeps
    /// the CPU just as idle but wakes immediately.
    pub fn wait_frame(fds: &[std::os::fd::RawFd], timeout: Duration) {
        let mut set: Vec<libc::pollfd> = fds
            .iter()
            .map(|&fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let ms = timeout.as_millis().min(1000) as libc::c_int;
        // EINTR or an error simply ends the wait early; the loop redraws either way.
        unsafe { libc::poll(set.as_mut_ptr(), set.len() as libc::nfds_t, ms) };
    }

    /// `EVIOCGRAB` = `_IOW('E', 0x90, int)` from `<linux/input.h>`.
    const EVIOCGRAB: libc::c_ulong = 0x4004_4590;

    /// The BSP's stable keypad node (TCA8418 on i2c1).
    pub const KEYPAD_BY_PATH: &str = "/dev/input/by-path/platform-3f804000.i2c-event";

    /// TCA8418 keypad via `evdev`: non-blocking reads (evdev has no FIONREAD),
    /// every decoded key queued so fast typing never drops a character.
    pub struct EvdevInput {
        file: std::fs::File,
        queue: std::collections::VecDeque<LapisKey>,
        shift: bool,
        esc_hold: EscHold,
        dead: bool,
    }

    impl EvdevInput {
        pub fn open(path: &Path) -> std::io::Result<Self> {
            use std::os::unix::fs::OpenOptionsExt;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)?;
            // Exclusive grab: without it every key also reaches the kernel VT,
            // i.e. the (invisible) login prompt on tty1 would receive whatever
            // is typed into a task prompt. Best effort; EBUSY just means shared.
            {
                use std::os::fd::AsRawFd;
                unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGRAB, 1 as libc::c_int) };
            }
            Ok(Self {
                file,
                queue: std::collections::VecDeque::new(),
                shift: false,
                esc_hold: EscHold::new(Duration::from_millis(1500)),
                dead: false,
            })
        }

        /// Opens the keypad: the BSP by-path node first, else the first evdev
        /// device whose sysfs `name` contains `tca8418`.
        pub fn open_keypad() -> std::io::Result<Self> {
            if let Ok(ev) = Self::open(Path::new(KEYPAD_BY_PATH)) {
                return Ok(ev);
            }
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

        /// The evdev file descriptor (for `poll(2)` in the fb loop).
        pub fn raw_fd(&self) -> std::os::fd::RawFd {
            use std::os::fd::AsRawFd;
            self.file.as_raw_fd()
        }

        /// True once the device node went away (unbind/replug): reopen it.
        pub fn is_dead(&self) -> bool {
            self.dead
        }

        fn pump(&mut self) {
            let mut buf = [0u8; 16 * 24];
            loop {
                let read = match self.file.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        self.dead = true;
                        return;
                    }
                };
                for chunk in buf[..read].chunks_exact(24) {
                    let mut e = [0u8; 24];
                    e.copy_from_slice(chunk);
                    let ev = decode_input_event(&e);
                    if ev.kind != EV_KEY {
                        continue;
                    }
                    if ev.code == keycode::KEY_LEFTSHIFT || ev.code == keycode::KEY_RIGHTSHIFT {
                        self.shift = ev.value != KEY_RELEASE;
                        continue;
                    }
                    if ev.code == keycode::KEY_ESC {
                        // Tap -> Esc on release; hold -> EscHold via `poll()`.
                        if let Some(k) = self.esc_hold.feed(ev.kind, ev.code, ev.value) {
                            self.queue.push_back(k);
                        }
                        continue;
                    }
                    if let KeyEvent::Press(k) =
                        translate_event(ev.kind, ev.code, ev.value, self.shift)
                    {
                        self.queue.push_back(k);
                    }
                }
            }
        }
    }

    impl InputBackend for EvdevInput {
        fn poll_event(&mut self) -> Option<LapisKey> {
            if self.queue.is_empty() {
                self.pump();
                if let Some(k) = self.esc_hold.poll() {
                    self.queue.push_back(k);
                }
            }
            self.queue.pop_front()
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
        use LapisKey as K;
        assert_eq!(
            map_keycode(keycode::KEY_ENTER, false),
            KeyEvent::Press(K::Enter)
        );
        assert_eq!(
            map_keycode(keycode::KEY_LEFT, false),
            KeyEvent::Press(K::CycleL)
        );
        assert_eq!(
            map_keycode(keycode::KEY_RIGHT, false),
            KeyEvent::Press(K::CycleR)
        );
        assert_eq!(map_keycode(keycode::KEY_UP, false), KeyEvent::Press(K::Up));
        assert_eq!(
            map_keycode(keycode::KEY_DOWN, false),
            KeyEvent::Press(K::Down)
        );
        // Letters (incl. z/c/f/x) stay chars; `App::key` decides navigation.
        assert_eq!(
            map_keycode(keycode::KEY_Z, false),
            KeyEvent::Press(K::Char('z'))
        );
        assert_eq!(
            map_keycode(keycode::KEY_W, false),
            KeyEvent::Press(K::Char('w'))
        );
        assert_eq!(
            map_keycode(keycode::KEY_A, true),
            KeyEvent::Press(K::Char('A'))
        );
        assert_eq!(map_keycode(2, false), KeyEvent::Press(K::Char('1')));
        assert_eq!(map_keycode(11, false), KeyEvent::Press(K::Char('0')));
        assert_eq!(
            map_keycode(keycode::KEY_BACKSPACE, false),
            KeyEvent::Press(K::Char('\x08'))
        );
        assert_eq!(map_keycode(0x2ff, false), KeyEvent::Ignored);
    }

    #[test]
    fn sym_and_fn_layers_follow_the_bsp_keymap() {
        use LapisKey as K;
        // sym layer (keymap1) codes, per tca8418_keypad_m5stack_keymap.map.
        assert_eq!(map_keycode(26, false), KeyEvent::Press(K::Char('!')));
        assert_eq!(map_keycode(92, false), KeyEvent::Press(K::Char('/')));
        assert_eq!(map_keycode(91, false), KeyEvent::Press(K::Char('.')));
        assert_eq!(map_keycode(71, false), KeyEvent::Press(K::Char('-')));
        // fn layer (keymap2): fn+U / fn+I are real brightness keycodes.
        assert_eq!(
            map_keycode(keycode::KEY_BRIGHTNESSDOWN, false),
            KeyEvent::Press(K::BacklightDown)
        );
        assert_eq!(
            map_keycode(keycode::KEY_BRIGHTNESSUP, false),
            KeyEvent::Press(K::BacklightUp)
        );
    }

    #[test]
    fn translate_release_repeat_and_ptt() {
        use LapisKey as K;
        assert_eq!(
            translate_event(EV_KEY, keycode::KEY_ENTER, KEY_RELEASE, false),
            KeyEvent::Release(K::Enter)
        );
        // A held ENTER must not auto-approve the next request.
        assert_eq!(
            translate_event(EV_KEY, keycode::KEY_ENTER, 2, false),
            KeyEvent::Ignored
        );
        assert_eq!(
            translate_event(EV_KEY, keycode::KEY_DOWN, 2, false),
            KeyEvent::Press(K::Down)
        );
        assert_eq!(
            translate_event(EV_KEY, keycode::KEY_LEFTALT, KEY_PRESS, false),
            KeyEvent::Press(K::PushToTalk(true))
        );
        assert_eq!(
            translate_event(EV_KEY, keycode::KEY_LEFTALT, 2, false),
            KeyEvent::Ignored
        );
        assert_eq!(
            translate_event(EV_KEY, keycode::KEY_LEFTALT, KEY_RELEASE, false),
            KeyEvent::Press(K::PushToTalk(false))
        );
        assert_eq!(translate_event(0, 0, 0, false), KeyEvent::Ignored);
    }

    // --- ESC hold -------------------------------------------------------------
    #[test]
    fn esc_tap_is_esc_and_hold_fires_once_while_down() {
        let mut h = EscHold::new(Duration::from_millis(1500));
        assert_eq!(h.feed(EV_KEY, keycode::KEY_ESC, KEY_PRESS), None);
        assert_eq!(h.poll(), None);
        assert_eq!(
            h.feed(EV_KEY, keycode::KEY_ESC, KEY_RELEASE),
            Some(LapisKey::Esc)
        );

        let mut h = EscHold::new(Duration::ZERO);
        assert_eq!(h.feed(EV_KEY, keycode::KEY_ESC, KEY_PRESS), None);
        assert_eq!(h.poll(), Some(LapisKey::EscHold));
        assert_eq!(h.poll(), None);
        // The release after a fired hold is swallowed (no stray Esc).
        assert_eq!(h.feed(EV_KEY, keycode::KEY_ESC, KEY_RELEASE), None);
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
    fn brightness_percent_maps_to_a_nonzero_level() {
        assert_eq!(brightness_level(100, 255), 255);
        assert_eq!(brightness_level(80, 255), 204);
        assert_eq!(brightness_level(50, 7), 4);
        // Never dark while awake, never past the panel maximum.
        assert_eq!(brightness_level(0, 255), 1);
        assert_eq!(brightness_level(1, 31), 1);
        assert_eq!(brightness_level(250, 255), 255);
        // No backlight device (max 0): nothing to drive.
        assert_eq!(brightness_level(80, 0), 0);
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
