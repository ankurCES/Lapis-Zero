//! View 3: Quick action & system menu with a spring-animated inverted pill
//! (SPEC 5.3). Rows are crisp single-line entries.

use crate::gfx::canvas::{Canvas, INK, PAPER};
use crate::gfx::font::{draw_text, Font};
use crate::gfx::spring::Spring;

use super::CONTENT_Y;

/// Text row labels; row 1 is a template whose `<host>` is substituted at render.
const ROWS: [&str; 7] = [
    "New Task / Prompt",
    "Target Host: <host>",
    "Pause / Resume Agent",
    "Run Saved Workflow...",
    "Scheduled Jobs",
    "Network, Tailscale & Power",
    "Pair Android App",
];

pub struct MenuView {
    hosts: Vec<String>,
    host_idx: usize,
    /// Highlighted row (spring pill follows this).
    pub index: usize,
    pub pill: Spring,
}

impl MenuView {
    pub fn demo() -> Self {
        let mut s = Self {
            hosts: vec!["macbook-pro".to_string(), "arch-dev".to_string()],
            host_idx: 0,
            index: 0,
            pill: Spring::new(320.0, 26.0, 0.0),
        };
        s.pill.snap_to(0.0);
        s
    }

    /// Replaces the host list from a daemon `state_sync` and selects `active`.
    pub fn set_hosts(&mut self, hosts: Vec<String>, active: &str) {
        if !hosts.is_empty() {
            self.hosts = hosts;
        }
        if let Some(pos) = self.hosts.iter().position(|h| h == active) {
            self.host_idx = pos;
        } else if !active.is_empty() {
            self.hosts.push(active.to_string());
            self.host_idx = self.hosts.len() - 1;
        } else if self.host_idx >= self.hosts.len() {
            self.host_idx = 0;
        }
    }

    /// Moves the highlight by `delta` rows (wrapping).
    pub fn move_sel(&mut self, delta: i32) {
        let n = ROWS.len() as i32;
        self.index = (self.index as i32 + delta).rem_euclid(n) as usize;
        self.pill.set_target(self.index as f32);
    }

    /// Cycles the active Tailscale target (SPEC 5.4 `TAB`).
    pub fn cycle_host(&mut self) {
        if !self.hosts.is_empty() {
            self.host_idx = (self.host_idx + 1) % self.hosts.len();
        }
    }

    /// The currently active host.
    pub fn host(&self) -> &str {
        self.hosts
            .get(self.host_idx)
            .map(String::as_str)
            .unwrap_or("macbook-pro")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::canvas::HEIGHT;

    #[test]
    fn all_rows_fit_below_the_status_bar_without_overlap() {
        assert_eq!(ROWS.len(), 7);
        assert_eq!(ROWS[6], "Pair Android App");
        // Rows are taller than the 6x8 glyphs, and the last pill ends on screen.
        assert!(ROW_H > Font::F6x8.cell_size().1);
        let last_pill_bottom = LIST_Y - 1 + ROWS.len() as i32 * ROW_H;
        assert!(LIST_Y > CONTENT_Y && last_pill_bottom <= HEIGHT);

        // Rendered: the pill of the last row is ink, and nothing but paper
        // lies below it.
        let mut m = MenuView::demo();
        m.move_sel(-1);
        assert_eq!(m.index, 6);
        let mut c = Canvas::new();
        render(&mut c, &m);
        let y = LIST_Y + 6 * ROW_H;
        assert_eq!(c.get_pixel(4, y), Some(INK));
        assert_eq!(c.get_pixel(4, last_pill_bottom - 1), Some(INK));
        for yy in last_pill_bottom..HEIGHT {
            assert_eq!(c.get_pixel(4, yy), Some(PAPER));
        }
    }
}
