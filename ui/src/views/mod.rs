//! The six core views. Phase 0 renders placeholders over the persistent status bar.

use crate::gfx::canvas::{Canvas, ACCENT_BLUE, INK, PAPER, WIDTH};
use crate::gfx::font::{draw_text, Font};

/// Status bar height in pixels (rows `0..17`); the 1px rule sits at [`RULE_Y`].
pub const STATUS_BAR_H: i32 = 18;
/// Row of the 1px rule separating the status bar from content.
pub const RULE_Y: i32 = 18;

/// The six core views (SPEC 5.3), in cycle order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// Ambient agent face + desk clock (default home).
    Companion,
    /// Live activity / tool-call feed.
    Ticker,
    /// Quick action & system menu.
    Menu,
    /// Tool approval modal.
    Approval,
    /// Keyboard + push-to-talk prompt overlay.
    Prompt,
    /// First-boot / network & Tailscale onboarding.
    Setup,
}

impl View {
    /// All views in cycle order.
    pub const ALL: [View; 6] = [
        View::Companion,
        View::Ticker,
        View::Menu,
        View::Approval,
        View::Prompt,
        View::Setup,
    ];

    /// Parses a CLI view name (e.g. `"companion"`).
    pub fn from_name(name: &str) -> Option<View> {
        View::ALL.into_iter().find(|v| v.name() == name)
    }

    /// Lowercase CLI name of the view.
    pub fn name(self) -> &'static str {
        match self {
            View::Companion => "companion",
            View::Ticker => "ticker",
            View::Menu => "menu",
            View::Approval => "approval",
            View::Prompt => "prompt",
            View::Setup => "setup",
        }
    }

    /// The view `delta` steps away in [`View::ALL`], wrapping around.
    pub fn cycle(self, delta: i32) -> View {
        let n = View::ALL.len() as i32;
        let i = View::ALL.iter().position(|v| *v == self).unwrap_or(0) as i32;
        View::ALL[(i + delta).rem_euclid(n) as usize]
    }
}

/// Renders the full Phase 0 placeholder frame for `view`.
pub fn render(canvas: &mut Canvas, view: View) {
    canvas.clear(PAPER);
    draw_status_bar(canvas);

    // ponytail: fixed-size stack buffer keeps the label allocation-free.
    let mut buf = [0u8; 48];
    let label = fmt_label(&mut buf, view.name());
    let x = (WIDTH - Font::F8x14.text_width(label)) / 2;
    draw_text(canvas, x, 56, label, Font::F8x14, INK);

    let sub = "Lapis-Zero OS - Z/C cycle views";
    let x = (WIDTH - Font::F6x8.text_width(sub)) / 2;
    draw_text(canvas, x, 78, sub, Font::F6x8, INK);

    canvas.fill_rect_dithered(240, 120, 60, 30, 128, INK, PAPER);
    canvas.draw_rect(239, 119, 62, 32, INK);
}

/// Draws `[● IDLE] [HOST: none] [TS] [00:00] [100%]` and the rule at [`RULE_Y`].
fn draw_status_bar(canvas: &mut Canvas) {
    let ty = (STATUS_BAR_H - 8) / 2 + 1;
    canvas.fill_rect(4, ty + 2, 3, 3, ACCENT_BLUE);
    draw_text(canvas, 10, ty, "IDLE", Font::F6x8, INK);
    draw_text(canvas, 46, ty, "HOST: none", Font::F6x8, INK);
    draw_text(canvas, 124, ty, "TS", Font::F6x8, INK);
    let right = WIDTH - 4;
    let pct = "100%";
    let pct_x = right - Font::F6x8.text_width(pct);
    draw_text(canvas, pct_x, ty, pct, Font::F6x8, INK);
    let clock = "00:00";
    let clock_x = pct_x - 10 - Font::F6x8.text_width(clock);
    draw_text(canvas, clock_x, ty, clock, Font::F6x8, INK);
    canvas.hline(0, RULE_Y, WIDTH, INK);
}

/// Writes `"[ <name>: Phase 0 placeholder ]"` into `buf` and returns it as `&str`.
fn fmt_label<'a>(buf: &'a mut [u8; 48], name: &str) -> &'a str {
    let mut len = 0;
    for part in ["[ ", name, ": Phase 0 placeholder ]"] {
        let b = part.as_bytes();
        buf[len..len + b.len()].copy_from_slice(b);
        len += b.len();
    }
    std::str::from_utf8(&buf[..len]).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for v in View::ALL {
            assert_eq!(View::from_name(v.name()), Some(v));
        }
        assert_eq!(View::from_name("nope"), None);
        assert_eq!(View::Companion.cycle(-1), View::Setup);
        assert_eq!(View::Setup.cycle(1), View::Companion);
    }

    #[test]
    fn render_draws_rule_and_label() {
        let mut c = Canvas::new();
        render(&mut c, View::Approval);
        assert_eq!(c.get_pixel(0, RULE_Y), Some(INK));
        assert_eq!(c.get_pixel(5, 9), Some(ACCENT_BLUE));
    }
}
