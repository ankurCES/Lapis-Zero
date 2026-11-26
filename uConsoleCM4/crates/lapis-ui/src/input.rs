//! Input that is not plain egui: the uConsole's gamepad buttons and the evdev keymap.
//!
//! On the stock uConsole keyboard firmware the D-pad sends arrow keys, and X/A/B/Y arrive as
//! joystick buttons BTN_TRIGGER/BTN_THUMB/BTN_THUMB2/BTN_TOP (0x120..0x123), Select/Start as
//! BTN_BASE3/BTN_BASE4 (0x128/0x129). The community QMK firmware numbers them differently,
//! hence [`PadMap`] in `/etc/lapis/ui.toml`-style overrides on the command line.

use egui::Key;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pad {
    A,
    B,
    X,
    Y,
    Select,
    Start,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadEvent {
    pub button: Pad,
    pub pressed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadMap {
    pub a: u16,
    pub b: u16,
    pub x: u16,
    pub y: u16,
    pub select: u16,
    pub start: u16,
}

impl Default for PadMap {
    fn default() -> Self {
        PadMap {
            x: 0x120,
            a: 0x121,
            b: 0x122,
            y: 0x123,
            select: 0x128,
            start: 0x129,
        }
    }
}

impl PadMap {
    pub fn button(&self, code: u16) -> Option<Pad> {
        [
            (self.a, Pad::A),
            (self.b, Pad::B),
            (self.x, Pad::X),
            (self.y, Pad::Y),
            (self.select, Pad::Select),
            (self.start, Pad::Start),
        ]
        .into_iter()
        .find(|(c, _)| *c == code)
        .map(|(_, p)| p)
    }

    /// `a=0x121,b=0x122,...` (decimal or 0x hex).
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut m = PadMap::default();
        for part in spec.split(',').filter(|p| !p.trim().is_empty()) {
            let (k, v) = part
                .split_once('=')
                .ok_or_else(|| format!("bad pad mapping {part:?}"))?;
            let v = v.trim();
            let code = match v.strip_prefix("0x") {
                Some(h) => u16::from_str_radix(h, 16),
                None => v.parse(),
            }
            .map_err(|_| format!("bad code {v:?}"))?;
            match k.trim() {
                "a" => m.a = code,
                "b" => m.b = code,
                "x" => m.x = code,
                "y" => m.y = code,
                "select" => m.select = code,
                "start" => m.start = code,
                other => return Err(format!("unknown pad button {other:?}")),
            }
        }
        Ok(m)
    }
}

/// Host preview: F5..F10 stand in for A, B, X, Y, Select, Start.
pub fn desktop_pad(key: Key) -> Option<Pad> {
    match key {
        Key::F5 => Some(Pad::A),
        Key::F6 => Some(Pad::B),
        Key::F7 => Some(Pad::X),
        Key::F8 => Some(Pad::Y),
        Key::F9 => Some(Pad::Select),
        Key::F10 => Some(Pad::Start),
        _ => None,
    }
}

// Linux input-event-codes.h
pub const KEY_ESC: u16 = 1;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_TAB: u16 = 15;
pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_SPACE: u16 = 57;
pub const KEY_CAPSLOCK: u16 = 58;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_PLAYPAUSE: u16 = 164;
pub const KEY_F12: u16 = 88;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;

/// Push-to-talk: F12 or a headset button (KEY_PLAYPAUSE).
pub fn is_ptt(code: u16) -> bool {
    code == KEY_F12 || code == KEY_PLAYPAUSE
}

const ROW1: &str = "1234567890-=";
const ROW1_S: &str = "!@#$%^&*()_+";
const ROW2: &str = "qwertyuiop[]";
const ROW2_S: &str = "QWERTYUIOP{}";
const ROW3: &str = "asdfghjkl;'`";
const ROW3_S: &str = "ASDFGHJKL:\"~";
const ROW4: &str = "\\zxcvbnm,./";
const ROW4_S: &str = "|ZXCVBNM<>?";

/// The character a US-layout key types (codes 2..=13, 16..=27, 30..=41, 43..=53).
pub fn key_char(code: u16, shift: bool) -> Option<char> {
    let pick = |plain: &str, shifted: &str, i: u16| {
        if shift {
            shifted.chars().nth(i as usize)
        } else {
            plain.chars().nth(i as usize)
        }
    };
    match code {
        2..=13 => pick(ROW1, ROW1_S, code - 2),
        16..=27 => pick(ROW2, ROW2_S, code - 16),
        30..=41 => pick(ROW3, ROW3_S, code - 30),
        43..=53 => pick(ROW4, ROW4_S, code - 43),
        KEY_SPACE => Some(' '),
        _ => None,
    }
}

/// The egui key for an evdev key code.
pub fn key(code: u16) -> Option<Key> {
    let letters = [
        (16, Key::Q),
        (17, Key::W),
        (18, Key::E),
        (19, Key::R),
        (20, Key::T),
        (21, Key::Y),
        (22, Key::U),
        (23, Key::I),
        (24, Key::O),
        (25, Key::P),
        (30, Key::A),
        (31, Key::S),
        (32, Key::D),
        (33, Key::F),
        (34, Key::G),
        (35, Key::H),
        (36, Key::J),
        (37, Key::K),
        (38, Key::L),
        (44, Key::Z),
        (45, Key::X),
        (46, Key::C),
        (47, Key::V),
        (48, Key::B),
        (49, Key::N),
        (50, Key::M),
        (2, Key::Num1),
        (3, Key::Num2),
        (4, Key::Num3),
        (5, Key::Num4),
        (6, Key::Num5),
        (7, Key::Num6),
        (8, Key::Num7),
        (9, Key::Num8),
        (10, Key::Num9),
        (11, Key::Num0),
    ];
    if let Some((_, k)) = letters.iter().find(|(c, _)| *c == code) {
        return Some(*k);
    }
    Some(match code {
        KEY_ESC => Key::Escape,
        KEY_BACKSPACE => Key::Backspace,
        KEY_TAB => Key::Tab,
        KEY_ENTER | 96 => Key::Enter,
        KEY_SPACE => Key::Space,
        12 => Key::Minus,
        13 => Key::Equals,
        26 => Key::OpenBracket,
        27 => Key::CloseBracket,
        39 => Key::Semicolon,
        51 => Key::Comma,
        52 => Key::Period,
        53 => Key::Slash,
        103 => Key::ArrowUp,
        108 => Key::ArrowDown,
        105 => Key::ArrowLeft,
        106 => Key::ArrowRight,
        102 => Key::Home,
        107 => Key::End,
        104 => Key::PageUp,
        109 => Key::PageDown,
        110 => Key::Insert,
        111 => Key::Delete,
        59 => Key::F1,
        60 => Key::F2,
        61 => Key::F3,
        62 => Key::F4,
        63 => Key::F5,
        64 => Key::F6,
        65 => Key::F7,
        66 => Key::F8,
        67 => Key::F9,
        68 => Key::F10,
        87 => Key::F11,
        KEY_F12 => Key::F12,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keymap_and_pads() {
        assert_eq!(key(30), Some(Key::A));
        assert_eq!(key(103), Some(Key::ArrowUp));
        assert_eq!(key_char(30, false), Some('a'));
        assert_eq!(key_char(30, true), Some('A'));
        assert_eq!(key_char(2, true), Some('!'));
        assert_eq!(key_char(53, false), Some('/'));
        assert_eq!(key_char(KEY_SPACE, false), Some(' '));
        assert_eq!(key_char(KEY_ENTER, false), None);
        let m = PadMap::default();
        assert_eq!(m.button(0x121), Some(Pad::A));
        assert_eq!(m.button(0x120), Some(Pad::X));
        assert_eq!(m.button(0x129), Some(Pad::Start));
        let q = PadMap::parse("a=0x120,b=0x121,x=290,y=0x123").unwrap();
        assert_eq!((q.a, q.b, q.x), (0x120, 0x121, 290));
        assert!(PadMap::parse("z=1").is_err());
        assert_eq!(desktop_pad(Key::F5), Some(Pad::A));
        assert!(is_ptt(KEY_PLAYPAUSE));
    }
}
