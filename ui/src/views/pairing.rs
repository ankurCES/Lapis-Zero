//! View 7: Android pairing (SPEC 6.3, ANDROID_COMPANION_SPEC 4). The daemon's
//! `pairing` message picks one of two screens: the 6-digit BLE pairing PIN, or a
//! pixel-exact QR of the `lapis://pair?...` payload beside host, PIN and countdown.
//!
//! The QR module matrix is built once, when the message arrives. Rendering only
//! paints modules and formats text into stack buffers, so it never allocates.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use qrcodegen::{QrCode, QrCodeEcc};
use serde_json::Value;

use crate::gfx::canvas::{Canvas, ACCENT_AMBER, ACCENT_BLUE, INK, PAPER, WIDTH};
use crate::gfx::font::{draw_text, Font};

use super::{fmt_into, CONTENT_H, CONTENT_Y};

/// Demo PIN for `--view pairing_pin` / `--view pairing_qr`.
pub const DEMO_PIN: &str = "482917";
/// Demo advertised BLE name.
pub const DEMO_NAME: &str = "LapisZero-4F2A";
/// Demo Tailscale host.
pub const DEMO_HOST: &str = "lapis-zero.tailnet.ts.net";
/// Demo QR payload (ANDROID_COMPANION_SPEC 4.2 shape, all-zero nonce).
pub const DEMO_PAYLOAD: &str = "lapis://pair?host=lapis-zero.tailnet.ts.net&nonce=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&pin=482917";

/// Quiet zone the layout guarantees around the symbol, in modules.
pub const QUIET_MIN: i32 = 2;
/// Quiet zone painted when the content area has room (the QR standard's 4).
const QUIET_MAX: i32 = 4;
/// Smallest module size, in pixels, worth showing on this LCD.
const MIN_SCALE: i32 = 2;
/// Module size that ECC M is traded for L to reach.
const GOOD_SCALE: i32 = 3;
/// Width of the left zone the QR is centred in.
const QR_ZONE_W: i32 = 152;
/// Left edge of the QR screen's text column.
const TEXT_X: i32 = 158;
/// Host name wrap width (6x8 cells up to the right margin) and line budget.
const HOST_COLS: usize = ((WIDTH - 4 - TEXT_X) / 6) as usize;
const HOST_LINES: usize = 3;
/// Largest countdown the `MM:SS` field shows.
const MAX_COUNTDOWN: u32 = 99 * 60 + 59;

/// Which pairing screen is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// BLE provisioning: show the PIN the phone must echo.
    Pin,
    /// Already on the tailnet: show the QR the phone scans.
    Qr,
}

/// When the pairing window closes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Expiry {
    /// The daemon sent neither `expires_at` nor `expires_in`.
    Unknown,
    /// Frozen seconds, for deterministic demo frames.
    Fixed(u32),
    /// Monotonic deadline fixed when the frame arrived.
    At(Instant),
}

/// State of the pairing view, rebuilt from each daemon `pairing` frame.
pub struct Pairing {
    pub mode: Mode,
    /// The PIN as sent; empty until the BLE handshake has produced one.
    pub pin: String,
    /// Advertised BLE name (PIN mode) or Tailscale host (QR mode).
    pub name: String,
    /// PIN-mode progress: `waiting`, `connected`, `verified`, `wifi`,
    /// `tailscale`, `done` or `locked`.
    pub status: String,
    /// Why there is no PIN (PIN mode, `status` "Bluetooth unavailable"); empty otherwise.
    pub error: String,
    /// The QR symbol; `None` in PIN mode or when the payload cannot be shown.
    pub qr: Option<Qr>,
    expiry: Expiry,
    /// Raw `expires_at`, identifying the pairing window across refreshes.
    expires_at: Option<f64>,
}

impl Pairing {
    /// Builds the view state from a `pairing` frame whose `mode` is `"pin"` or
    /// `"qr"`; any other mode yields `None`. `prev` is the pairing already on
    /// screen, if any.
    pub fn from_message(v: &Value, prev: Option<&Pairing>) -> Option<Pairing> {
        let text = |key: &str| v[key].as_str().unwrap_or("").to_string();
        let mode = match v["mode"].as_str()? {
            "pin" => Mode::Pin,
            "qr" => Mode::Qr,
            _ => return None,
        };
        let expires_at = v["expires_at"].as_f64();
        let expiry = match prev {
            // A status update or resync of the window already on screen keeps
            // its monotonic deadline: the wall clock may step (NTP, once Wi-Fi
            // comes up mid-pairing) without the countdown jumping.
            Some(p) if expires_at.is_some() && p.expires_at == expires_at => p.expiry,
            _ => match seconds_left(expires_at, v["expires_in"].as_f64(), wall_clock_secs()) {
                Some(s) => Expiry::At(Instant::now() + Duration::from_secs_f64(s)),
                None => Expiry::Unknown,
            },
        };
        Some(Pairing {
            mode,
            pin: text("pin"),
            name: text(match mode {
                Mode::Pin => "device_name",
                Mode::Qr => "host",
            }),
            status: text("status"),
            error: text("error"),
            qr: match mode {
                Mode::Pin => None,
                Mode::Qr => v["payload"].as_str().and_then(Qr::build),
            },
            expiry,
            expires_at,
        })
    }

    /// Deterministic PIN screen for `--view pairing_pin`.
    pub fn demo_pin() -> Pairing {
        Pairing {
            mode: Mode::Pin,
            pin: DEMO_PIN.to_string(),
            name: DEMO_NAME.to_string(),
            status: "waiting".to_string(),
            error: String::new(),
            qr: None,
            expiry: Expiry::Fixed(4 * 60 + 53),
            expires_at: None,
        }
    }

    /// Deterministic QR screen for `--view pairing_qr`.
    pub fn demo_qr() -> Pairing {
        Pairing {
            mode: Mode::Qr,
            pin: DEMO_PIN.to_string(),
            name: DEMO_HOST.to_string(),
            status: String::new(),
            error: String::new(),
            qr: Qr::build(DEMO_PAYLOAD),
            expiry: Expiry::Fixed(2 * 60 + 47),
            expires_at: None,
        }
    }

    /// Whole seconds left at `now` (rounded up), or `None` if the daemon gave no expiry.
    pub fn remaining_at(&self, now: Instant) -> Option<u32> {
        match self.expiry {
            Expiry::Unknown => None,
            Expiry::Fixed(secs) => Some(secs),
            Expiry::At(deadline) => {
                Some(deadline.saturating_duration_since(now).as_secs_f32().ceil() as u32)
            }
        }
    }
}

/// Seconds until the pairing window closes, as of a frame's arrival: `expires_at`
/// minus the wall clock, falling back to `expires_in`. With both present,
/// `expires_in` (the full window) caps the result, so a clock that stepped back
/// between the daemon's stamp and now cannot stretch the countdown.
fn seconds_left(expires_at: Option<f64>, expires_in: Option<f64>, wall_now: f64) -> Option<f64> {
    let left = match (expires_at, expires_in) {
        (Some(at), Some(window)) => (at - wall_now).min(window),
        (Some(at), None) => at - wall_now,
        (None, Some(window)) => window,
        (None, None) => return None,
    };
    Some(left.clamp(0.0, MAX_COUNTDOWN as f64))
}

/// Unix time in seconds (0 if the clock is before the epoch).
fn wall_clock_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// A QR symbol sized for the content area.
pub struct Qr {
    code: QrCode,
    /// Integer pixels per module.
    pub scale: i32,
}

impl Qr {
    /// Encodes `payload` at the largest integer module size that fits the
    /// content area with a [`QUIET_MIN`] quiet zone. ECC M is preferred; L is
    /// used when it alone reaches [`GOOD_SCALE`] (or [`MIN_SCALE`]). `None` if
    /// the payload is empty or too long for [`MIN_SCALE`].
    pub fn build(payload: &str) -> Option<Qr> {
        if payload.is_empty() {
            return None;
        }
        let mut fallback = None;
        for ecc in [QrCodeEcc::Medium, QrCodeEcc::Low] {
            // `encode_text` picks the smallest version and then raises the ECC
            // level as far as that version allows, so M may come back as Q or H.
            let Ok(code) = QrCode::encode_text(payload, ecc) else {
                continue;
            };
            let scale = CONTENT_H / (code.size() + 2 * QUIET_MIN);
            if scale >= GOOD_SCALE {
                return Some(Qr { code, scale });
            }
            if scale >= MIN_SCALE && fallback.is_none() {
                fallback = Some(Qr { code, scale });
            }
        }
        fallback
    }

    /// Modules per side (`17 + 4 * version`).
    pub fn modules(&self) -> i32 {
        self.code.size()
    }

    /// QR version, `1..=40`.
    pub fn version(&self) -> u8 {
        self.code.version().value()
    }

    /// Error-correction level actually encoded.
    pub fn ecc(&self) -> QrCodeEcc {
        self.code.error_correction_level()
    }

    /// Painted quiet zone in pixels: up to [`QUIET_MAX`] modules, never under [`QUIET_MIN`].
    pub fn quiet_px(&self) -> i32 {
        let room = (CONTENT_H - self.modules() * self.scale) / 2;
        (room - 1).clamp(QUIET_MIN * self.scale, QUIET_MAX * self.scale)
    }

    /// `(x, y, w, h)` of the paper box holding the symbol and its quiet zone.
    /// The dark theme leaves this rectangle alone so the code stays dark-on-light.
    pub fn box_rect(&self) -> (i32, i32, i32, i32) {
        let side = self.modules() * self.scale + 2 * self.quiet_px();
        (
            (QR_ZONE_W - side) / 2,
            CONTENT_Y + (CONTENT_H - side) / 2,
            side,
            side,
        )
    }

    /// Top-left pixel of module `(0, 0)`.
    pub fn origin(&self) -> (i32, i32) {
        let (x, y, _, _) = self.box_rect();
        (x + self.quiet_px(), y + self.quiet_px())
    }

    /// Paints the paper box and the dark modules (horizontal runs merged).
    fn draw(&self, canvas: &mut Canvas) {
        let (bx, by, bw, bh) = self.box_rect();
        canvas.fill_rect(bx, by, bw, bh, PAPER);
        let (ox, oy) = self.origin();
        let (n, s) = (self.modules(), self.scale);
        for my in 0..n {
            let mut mx = 0;
            while mx < n {
                if !self.code.get_module(mx, my) {
                    mx += 1;
                    continue;
                }
                let start = mx;
                while mx < n && self.code.get_module(mx, my) {
                    mx += 1;
                }
                canvas.fill_rect(ox + start * s, oy + my * s, (mx - start) * s, s, INK);
            }
        }
    }
}

/// Renders the pairing view below the status bar.
pub fn render(canvas: &mut Canvas, p: &Pairing) {
    let left = p.remaining_at(Instant::now());
    match p.mode {
        Mode::Pin => render_pin(canvas, p, left),
        Mode::Qr => render_qr(canvas, p, left),
    }
}

/// PIN screen: title, advertised name, large PIN, progress, countdown, hint.
fn render_pin(canvas: &mut Canvas, p: &Pairing, left: Option<u32>) {
    let (x, right) = (8, WIDTH - 8);
    draw_text(canvas, x, 24, "PAIR ANDROID APP", Font::F8x14, INK);
    draw_expiry(canvas, right, 27, left, true);

    // Advertised BLE name the phone will list.
    draw_text(canvas, x, 47, "Bluetooth", Font::F6x8, INK);
    let mut nb = [0u8; 26];
    let name = fmt_into(&mut nb, &[if p.name.is_empty() { "-" } else { &p.name }]);
    draw_text(canvas, x + 62, 44, name, Font::F8x14, ACCENT_BLUE);

    // The PIN, framed and centred.
    let mut pb = [0u8; 16];
    let pin = fmt_pin(&mut pb, &p.pin);
    let pw = Font::F16x28.text_width(pin);
    let px = (WIDTH - pw) / 2;
    canvas.draw_rect(px - 16, 63, pw + 32, 44, INK);
    draw_text(canvas, px, 71, pin, Font::F16x28, INK);

    // Progress, centred under the frame with a state pip.
    let (status, color) = status_text(&p.status);
    let mut sb = [0u8; 30];
    let status = fmt_into(&mut sb, &[status]);
    let sx = (WIDTH - Font::F8x14.text_width(status) + 10) / 2;
    canvas.fill_rect(sx - 10, 120, 5, 5, color);
    draw_text(canvas, sx, 115, status, Font::F8x14, color);

    // No PIN and a reason: say why (two lines of 50 characters) instead of
    // telling the user to type a PIN that is not there.
    if p.pin.is_empty() && !p.error.is_empty() {
        let (first, second) = split_at_chars(&p.error, 50);
        draw_text(canvas, x, 134, first, Font::F6x8, ACCENT_AMBER);
        let (second, _) = split_at_chars(second, 50);
        draw_text(canvas, x, 144, second, Font::F6x8, ACCENT_AMBER);
    }
    let prompt = if p.pin.is_empty() {
        "No PIN yet"
    } else {
        "Type this PIN on your phone"
    };
    draw_text(canvas, x, 154, prompt, Font::F6x8, INK);
    let hint = "ESC to cancel";
    let hx = right - Font::F6x8.text_width(hint);
    draw_text(canvas, hx, 154, hint, Font::F6x8, INK);
}

/// QR screen: the symbol on the left; host, PIN, countdown and hint on the right.
fn render_qr(canvas: &mut Canvas, p: &Pairing, left: Option<u32>) {
    match p.qr.as_ref() {
        Some(qr) => qr.draw(canvas),
        None => {
            let msg = "QR unavailable";
            let mx = (QR_ZONE_W - Font::F8x14.text_width(msg)) / 2;
            draw_text(canvas, mx, 87, msg, Font::F8x14, ACCENT_AMBER);
        }
    }

    let x = TEXT_X;
    draw_text(canvas, x, 24, "SCAN TO PAIR", Font::F8x14, INK);

    // Host name, wrapped at separators.
    draw_text(canvas, x, 45, "Host", Font::F6x8, INK);
    let mut rest = p.name.as_str();
    let mut y = 56;
    for _ in 0..HOST_LINES {
        if rest.is_empty() {
            break;
        }
        let (line, tail) = wrap_line(rest, HOST_COLS);
        draw_text(canvas, x, y, line, Font::F6x8, ACCENT_BLUE);
        rest = tail;
        y += 10;
    }

    draw_text(canvas, x, 92, "PIN", Font::F6x8, INK);
    let mut pb = [0u8; 16];
    draw_text(canvas, x, 103, fmt_pin(&mut pb, &p.pin), Font::F16x28, INK);

    draw_expiry(canvas, x, 139, left, false);
    draw_text(canvas, x, 156, "ESC to cancel", Font::F6x8, INK);
}

/// Draws `expires in M:SS` (or `expired`) at `(x, y)`; `x` is the right edge
/// when `align_right`. Nothing is drawn when the daemon sent no expiry.
fn draw_expiry(canvas: &mut Canvas, x: i32, y: i32, left: Option<u32>, align_right: bool) {
    let Some(left) = left else {
        return;
    };
    let mut cb = [0u8; 5];
    let mut tb = [0u8; 16];
    let (text, color) = if left == 0 {
        ("expired", ACCENT_AMBER)
    } else {
        let t = fmt_into(&mut tb, &["expires in ", fmt_countdown(&mut cb, left)]);
        (t, INK)
    };
    let tx = if align_right {
        x - Font::F6x8.text_width(text)
    } else {
        x
    };
    draw_text(canvas, tx, y, text, Font::F6x8, color);
}

/// Short human text and colour for a PIN-mode `status` (SPEC 6.3). An unknown
/// status is shown as sent rather than hidden.
fn status_text(status: &str) -> (&str, u32) {
    match status {
        "" | "waiting" => ("Waiting for phone", INK),
        "connected" => ("Phone connected", ACCENT_BLUE),
        "verified" => ("PIN verified", ACCENT_BLUE),
        "wifi" => ("Setting up Wi-Fi", ACCENT_BLUE),
        "tailscale" => ("Joining Tailscale", ACCENT_BLUE),
        "done" => ("Paired", ACCENT_BLUE),
        "locked" => ("Locked: too many tries", ACCENT_AMBER),
        "Bluetooth unavailable" => ("Bluetooth unavailable", ACCENT_AMBER),
        other => (other, INK),
    }
}

/// Splits `s` after at most `n` characters, on a character boundary.
fn split_at_chars(s: &str, n: usize) -> (&str, &str) {
    match s.char_indices().nth(n) {
        Some((i, _)) => s.split_at(i),
        None => (s, ""),
    }
}

/// Formats the PIN for display: six characters are grouped `482 917`, an empty
/// PIN (no BLE handshake yet) shows a placeholder, anything else is shown as
/// sent, cut to eight characters.
fn fmt_pin<'a>(buf: &'a mut [u8; 16], pin: &'a str) -> &'a str {
    if pin.is_empty() {
        "--- ---"
    } else if pin.len() == 6 && pin.is_ascii() {
        fmt_into(buf, &[&pin[..3], " ", &pin[3..]])
    } else {
        fmt_into(&mut buf[..8], &[pin])
    }
}

/// Formats seconds as `M:SS` / `MM:SS` (capped at `99:59`) into a stack buffer.
fn fmt_countdown(buf: &mut [u8; 5], secs: u32) -> &str {
    let secs = secs.min(MAX_COUNTDOWN);
    let (m, s) = (secs / 60, secs % 60);
    let mut len = 0;
    if m >= 10 {
        buf[len] = b'0' + (m / 10) as u8;
        len += 1;
    }
    for byte in [
        b'0' + (m % 10) as u8,
        b':',
        b'0' + (s / 10) as u8,
        b'0' + (s % 10) as u8,
    ] {
        buf[len] = byte;
        len += 1;
    }
    std::str::from_utf8(&buf[..len]).unwrap()
}

/// Splits one display line of at most `max_chars` off `text`. When the text has
/// to wrap it breaks after the last `.` or `-` in the line's second half, else
/// at the limit. Returns `(line, rest)`.
fn wrap_line(text: &str, max_chars: usize) -> (&str, &str) {
    let Some((limit, _)) = text.char_indices().nth(max_chars) else {
        return (text, "");
    };
    let cut = match text[..limit].rfind(['.', '-']) {
        Some(i) if i >= limit / 2 => i + 1,
        _ => limit,
    };
    text.split_at(cut)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::canvas::HEIGHT;
    use serde_json::json;

    /// True when the 7x7 finder pattern sits with its top-left module at `(mx, my)`.
    fn finder_at(c: &Canvas, qr: &Qr, mx: i32, my: i32) -> bool {
        let (ox, oy) = qr.origin();
        let s = qr.scale;
        (0..7 * s).all(|py| {
            (0..7 * s).all(|px| {
                // Dark frame, light ring, dark 3x3 core: ring index 2 is the light one.
                let ring = (px / s - 3).abs().max((py / s - 3).abs());
                let want = if ring == 2 { PAPER } else { INK };
                c.get_pixel(ox + mx * s + px, oy + my * s + py) == Some(want)
            })
        })
    }

    #[test]
    fn qr_geometry_fits_and_shows_three_finders() {
        // The demo payload, the longest the daemon builds (a long tailnet host
        // plus the dev `&tls=0` suffix), and one that only fits at 2 px.
        let long = format!(
            "{}&tls=0",
            DEMO_PAYLOAD.replace("lapis-zero", "lapis-zero-workbench-4f2a")
        );
        let huge = format!("lapis://pair?host={}&nonce=x", "h".repeat(200));
        assert_eq!((DEMO_PAYLOAD.len(), long.len()), (104, 125));
        for (payload, min_scale) in [(DEMO_PAYLOAD, 3), (long.as_str(), 3), (huge.as_str(), 2)] {
            let qr = Qr::build(payload).expect("payload fits");
            let (n, s) = (qr.modules(), qr.scale);
            assert_eq!(n, 17 + 4 * qr.version() as i32);
            assert!(
                s >= min_scale,
                "{} px modules for {} bytes",
                s,
                payload.len()
            );
            // Largest integer scale that keeps the minimum quiet zone on screen.
            assert!(s * (n + 2 * QUIET_MIN) <= CONTENT_H);
            assert!((s + 1) * (n + 2 * QUIET_MIN) > CONTENT_H);

            // The paper box sits below the status bar, left of the text column,
            // with at least the minimum quiet zone on every side of the symbol.
            let (bx, by, bw, bh) = qr.box_rect();
            let (ox, oy) = qr.origin();
            assert!(bx >= 0 && by >= CONTENT_Y && by + bh <= HEIGHT);
            assert!(bx + bw <= QR_ZONE_W && QR_ZONE_W < TEXT_X);
            assert_eq!(
                (bw, bh),
                (n * s + 2 * qr.quiet_px(), n * s + 2 * qr.quiet_px())
            );
            assert_eq!((ox - bx, oy - by), (qr.quiet_px(), qr.quiet_px()));
            assert!(qr.quiet_px() >= QUIET_MIN * s);

            let mut c = Canvas::new();
            c.clear(INK); // the symbol must bring its own paper
            qr.draw(&mut c);
            for (mx, my) in [(0, 0), (n - 7, 0), (0, n - 7)] {
                assert!(finder_at(&c, &qr, mx, my), "finder missing at {mx},{my}");
            }
            assert!(
                !finder_at(&c, &qr, n - 7, n - 7),
                "fourth corner has no finder"
            );
            // Quiet zone: every box pixel outside the symbol is paper.
            for y in by..by + bh {
                for x in bx..bx + bw {
                    let inside = (ox..ox + n * s).contains(&x) && (oy..oy + n * s).contains(&y);
                    if !inside {
                        assert_eq!(c.get_pixel(x, y), Some(PAPER), "quiet zone at {x},{y}");
                    }
                }
            }
            // Every module is drawn as an exact `s` x `s` block of one colour.
            for my in 0..n {
                for mx in 0..n {
                    let want = if qr.code.get_module(mx, my) {
                        INK
                    } else {
                        PAPER
                    };
                    for (dx, dy) in [(0, 0), (s - 1, 0), (0, s - 1), (s - 1, s - 1)] {
                        assert_eq!(c.get_pixel(ox + mx * s + dx, oy + my * s + dy), Some(want));
                    }
                }
            }
        }
    }

    #[test]
    fn qr_prefers_ecc_m_and_falls_back_to_l_for_module_size() {
        // 104 bytes: version 6 holds it at M (106 bytes) with 3 px modules.
        let demo = Qr::build(DEMO_PAYLOAD).unwrap();
        assert_eq!((demo.version(), demo.modules(), demo.scale), (6, 41, 3));
        assert!(demo.ecc() == QrCodeEcc::Medium);
        assert_eq!(demo.box_rect(), (2, 21, 147, 147));
        assert_eq!(demo.origin(), (14, 33));
        // 125 bytes needs version 8 at M (2 px) but fits version 6 at L (3 px).
        let l = Qr::build(&"a".repeat(125)).unwrap();
        assert_eq!((l.version(), l.scale), (6, 3));
        assert!(l.ecc() == QrCodeEcc::Low);
        // 250 bytes cannot reach 3 px at either level: stay at M, 2 px.
        let m = Qr::build(&"a".repeat(250)).unwrap();
        assert_eq!(m.scale, 2);
        assert!(m.ecc() != QrCodeEcc::Low);
        // Too long for 2 px modules even at L, and nothing to encode.
        assert!(Qr::build(&"a".repeat(600)).is_none());
        assert!(Qr::build("").is_none());
    }

    #[test]
    fn pairing_frames_parse_both_modes() {
        let pin = Pairing::from_message(
            &json!({"type": "pairing", "mode": "pin", "pin": "482917",
                "device_name": "LapisZero-4F2A", "status": "waiting", "expires_in": 300}),
            None,
        )
        .unwrap();
        assert_eq!(pin.mode, Mode::Pin);
        assert_eq!(
            (pin.pin.as_str(), pin.name.as_str(), pin.status.as_str()),
            ("482917", "LapisZero-4F2A", "waiting")
        );
        assert!(pin.qr.is_none());
        let left = pin.remaining_at(Instant::now()).unwrap();
        assert!((299..=300).contains(&left), "{left}");

        let at = wall_clock_secs().floor() + 120.0;
        let frame = json!({"type": "pairing", "mode": "qr", "payload": DEMO_PAYLOAD,
            "host": "lapis-zero.tailnet.ts.net", "pin": "482917",
            "expires_in": 180, "expires_at": at});
        let qr = Pairing::from_message(&frame, None).unwrap();
        assert_eq!(qr.mode, Mode::Qr);
        assert_eq!(qr.name, "lapis-zero.tailnet.ts.net");
        assert_eq!(qr.qr.as_ref().map(|q| q.modules()), Some(41));
        // `expires_at` minus the wall clock wins over the stale `expires_in`.
        let left = qr.remaining_at(Instant::now()).unwrap();
        assert!((118..=120).contains(&left), "{left}");
        // A refresh of the same window keeps the deadline already running.
        let again = Pairing::from_message(&frame, Some(&qr)).unwrap();
        assert_eq!(again.expiry, qr.expiry);

        // No expiry fields: no countdown. Unknown modes are not a view.
        let bare = Pairing::from_message(&json!({"mode": "pin"}), None).unwrap();
        assert_eq!(bare.remaining_at(Instant::now()), None);
        assert!(bare.pin.is_empty());
        assert!(Pairing::from_message(&json!({"mode": "starting"}), None).is_none());
        assert!(Pairing::from_message(&json!({"mode": "off"}), None).is_none());
        assert!(Pairing::from_message(&json!({}), None).is_none());
    }

    #[test]
    fn countdown_prefers_expires_at_then_expires_in() {
        let now = 1_795_438_264.0;
        assert_eq!(seconds_left(Some(now + 90.0), Some(180.0), now), Some(90.0));
        assert_eq!(seconds_left(Some(now + 90.0), None, now), Some(90.0));
        assert_eq!(seconds_left(None, Some(300.0), now), Some(300.0));
        assert_eq!(seconds_left(None, None, now), None);
        // Already over, and a clock that stepped back past the window's start.
        assert_eq!(seconds_left(Some(now - 5.0), Some(180.0), now), Some(0.0));
        assert_eq!(
            seconds_left(Some(now + 4000.0), Some(180.0), now),
            Some(180.0)
        );
        assert_eq!(seconds_left(Some(now + 1e12), None, now), Some(5999.0));

        let mut b = [0u8; 5];
        assert_eq!(fmt_countdown(&mut b, 293), "4:53");
        assert_eq!(fmt_countdown(&mut b, 9), "0:09");
        assert_eq!(fmt_countdown(&mut b, 600), "10:00");
        assert_eq!(fmt_countdown(&mut b, u32::MAX), "99:59");

        let mut demo = Pairing::demo_pin();
        assert_eq!(demo.remaining_at(Instant::now()), Some(293));
        let t0 = Instant::now();
        demo.expiry = Expiry::At(t0 + Duration::from_millis(1500));
        assert_eq!(demo.remaining_at(t0), Some(2));
        assert_eq!(demo.remaining_at(t0 + Duration::from_secs(1)), Some(1));
        assert_eq!(demo.remaining_at(t0 + Duration::from_secs(9)), Some(0));
    }

    #[test]
    fn pin_status_and_host_text_helpers() {
        let mut b = [0u8; 16];
        assert_eq!(split_at_chars("abcdef", 4), ("abcd", "ef"));
        assert_eq!(split_at_chars("abc", 4), ("abc", ""));
        assert_eq!(split_at_chars("né-ö", 2), ("né", "-ö"));
        assert_eq!(fmt_pin(&mut b, "482917"), "482 917");
        assert_eq!(fmt_pin(&mut b, ""), "--- ---");
        assert_eq!(fmt_pin(&mut b, "1234"), "1234");
        assert_eq!(fmt_pin(&mut b, "1234567890"), "12345678");

        for status in [
            "waiting",
            "connected",
            "verified",
            "wifi",
            "tailscale",
            "done",
            "locked",
        ] {
            let (text, _) = status_text(status);
            assert!(text != status && text.len() <= 22, "{status} -> {text}");
        }
        assert_eq!(status_text("locked").1, ACCENT_AMBER);
        assert_eq!(status_text("rebooting").0, "rebooting");

        assert_eq!(
            wrap_line("lapis-zero.tailnet.ts.net", 26),
            ("lapis-zero.tailnet.ts.net", "")
        );
        assert_eq!(
            wrap_line("cardputer-zero-4f2a.tail1a2b3c.ts.net", 26),
            ("cardputer-zero-4f2a.", "tail1a2b3c.ts.net")
        );
        assert_eq!(wrap_line("abcdefghij", 4), ("abcd", "efghij"));
        assert_eq!(
            wrap_line("a-bcdefghijklmnop", 10),
            ("a-bcdefghi", "jklmnop")
        );
        assert_eq!(wrap_line("héllo-wörld", 7), ("héllo-", "wörld"));
        assert_eq!(wrap_line("", 4), ("", ""));
    }

    #[test]
    fn both_screens_render_inside_the_content_area() {
        for p in [Pairing::demo_pin(), Pairing::demo_qr()] {
            let mut c = Canvas::new();
            render(&mut c, &p);
            // Nothing is drawn into the status bar rows or its rule.
            for y in 0..CONTENT_Y {
                for x in 0..WIDTH {
                    assert_eq!(c.get_pixel(x, y), Some(PAPER));
                }
            }
            let inked = c
                .pixels()
                .chunks_exact(3)
                .filter(|px| px != &[0xF2, 0xF2, 0xEC]);
            assert!(inked.count() > 1500);
        }
        // Hostile strings (long, non-ASCII, missing payload) must not panic.
        let odd = Pairing::from_message(
            &json!({"mode": "qr", "host": "é".repeat(200), "pin": "ünïcödé-pin", "expires_in": 0}),
            None,
        )
        .unwrap();
        assert!(odd.qr.is_none());
        render(&mut Canvas::new(), &odd);
        let odd = Pairing::from_message(
            &json!({"mode": "pin", "device_name": "ñ".repeat(90), "status": "ß".repeat(90)}),
            None,
        )
        .unwrap();
        render(&mut Canvas::new(), &odd);
    }

    #[test]
    fn bluetooth_unavailable_shows_its_reason_instead_of_a_pin_prompt() {
        let frame = json!({"mode": "pin", "pin": "", "device_name": "",
            "status": "Bluetooth unavailable",
            "error": "no usable Bluetooth adapter: bluetoothd is not running or not answering"});
        let p = Pairing::from_message(&frame, None).unwrap();
        assert_eq!(p.error.len(), 71);
        assert_eq!(status_text(&p.status).1, ACCENT_AMBER);
        let (mut with, mut without) = (Canvas::new(), Canvas::new());
        render(&mut with, &p);
        let mut silent = Pairing::from_message(&frame, None).unwrap();
        silent.error.clear();
        render(&mut without, &silent);
        // The reason occupies the two lines above the hint row and nothing else.
        let differs = |y0: i32, y1: i32| {
            (y0..y1).any(|y| (0..WIDTH).any(|x| with.get_pixel(x, y) != without.get_pixel(x, y)))
        };
        assert!(differs(134, 152), "the reason is drawn");
        assert!(!differs(0, 134) && !differs(152, 170));
        // A hostile reason (long, non-ASCII) must not panic or overflow its lines.
        let mut odd = Pairing::from_message(&frame, None).unwrap();
        odd.error = "ß".repeat(400);
        render(&mut Canvas::new(), &odd);
    }
}
