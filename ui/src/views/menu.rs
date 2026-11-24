//! View 3: Quick action & system menu with a spring-animated inverted pill
//! (SPEC 5.3). Rows are crisp single-line entries.

use crate::gfx::canvas::{Canvas, INK, PAPER};
use crate::gfx::font::{draw_text, Font};
use crate::gfx::spring::Spring;

use super::CONTENT_Y;

/// Text row labels; row 1 is a template whose `<host>` is substituted at render.
const ROWS: [&str; 6] = [
    "New Task / Prompt",
    "Target Host: <host>",
    "Pause / Resume Agent",
    "Run Saved Workflow...",
    "Scheduled Jobs",
    "Network, Tailscale & Power",
];

pub struct MenuView {
    hosts: [&'static str; 2],
    host_idx: usize,
    /// Highlighted row (spring pill follows this).
    pub index: usize,
    pub pill: Spring,
}

impl MenuView {
    pub fn demo() -> Self {
        let mut s = Self {
            hosts: ["macbook-pro", "arch-dev"],
            host_idx: 0,
            index: 0,
            pill: Spring::new(320.0, 26.0, 0.0),
        };
        s.pill.snap_to(0.0);
        s
    }

    /// Moves the highlight by `delta` rows (wrapping).
    pub fn move_sel(&mut self, delta: i32) {
        let n = ROWS.len() as i32;
        self.index = (self.index as i32 + delta).rem_euclid(n) as usize;
        self.pill.set_target(self.index as f32);
    }

    /// Cycles the active Tailscale target (SPEC 5.4 `TAB`).
    pub fn cycle_host(&mut self) {
        self.host_idx = (self.host_idx + 1) % self.hosts.len();
    }

    /// The currently active host.
    pub fn host(&self) -> &'static str {
        self.hosts[self.host_idx]
    }
}

/// Top of the menu list.
const LIST_Y: i32 = CONTENT_Y + 2;
/// Row height in pixels.
const ROW_H: i32 = 18;
/// Left inset for row labels.
const PAD_X: i32 = 8;

pub fn render(canvas: &mut Canvas, m: &MenuView) {
    for (i, _label) in ROWS.iter().enumerate() {
        let y = LIST_Y + i as i32 * ROW_H;
        let selected = i == m.index;
        if selected {
            let pill_y = LIST_Y + (m.index as i32) * ROW_H - 1;
            canvas.fill_rect(2, pill_y, 316, ROW_H, INK);
        }
        let color = if selected { PAPER } else { INK };
        match i {
            1 => {
                let mut b = [0u8; 32];
                let s = super::fmt_into(&mut b, &["Target Host: ", m.host()]);
                draw_text(canvas, PAD_X, y, s, Font::F6x8, color);
            }
            _ => draw_text(canvas, PAD_X, y, ROWS[i], Font::F6x8, color),
        }
    }
}
