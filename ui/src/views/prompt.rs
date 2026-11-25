//! View 5: Prompt & voice overlay (SPEC 5.3). Multi-line text buffer with a
//! blinking block cursor; ENTER dispatches `submit_task` (IPC in Phase 2).

use crate::gfx::canvas::{Canvas, ACCENT_BLUE, INK, PAPER};
use crate::gfx::font::{draw_text, Font};

use super::{CONTENT_Y, RULE_Y};

pub struct PromptView {
    pub host: String,
    /// Accumulated ASCII prompt buffer.
    pub buffer: Vec<u8>,
    /// Set true by ENTER while waiting for daemon `state_sync`.
    pub dispatch: bool,
    /// Blink phase for the cursor.
    pub cursor: bool,
}

impl PromptView {
    pub fn new() -> Self {
        Self {
            host: "macbook-pro".to_string(),
            buffer: Vec::new(),
            dispatch: false,
            cursor: true,
        }
    }

    pub fn push(&mut self, c: char) {
        if c == '\x08' || c == '\x7f' {
            self.buffer.pop();
            return;
        }
        if c.is_ascii() && !c.is_ascii_control() && self.buffer.len() < 60 {
            self.buffer.push(c as u8);
        }
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.dispatch = false;
    }
}

impl Default for PromptView {
    fn default() -> Self {
        Self::new()
    }
}

pub fn render(canvas: &mut Canvas, p: &PromptView, now: f64) {
    // Panel: full-width prompt card below the status bar.
    canvas.fill_rect(10, CONTENT_Y, 300, 96, PAPER);
    canvas.draw_rect(10, CONTENT_Y, 300, 96, INK);

    let tx = 18;
    let ty = CONTENT_Y + 8;
    draw_text(canvas, tx, ty, "NEW TASK", Font::F8x14, INK);

    // Active host.
    let mut hb = [0u8; 32];
    let h = super::fmt_into(&mut hb, &["host: ", &p.host]);
    draw_text(canvas, tx, ty + 20, h, Font::F6x8, ACCENT_BLUE);

    // Text area (2 lines, wrapped trivially).
    let text = std::str::from_utf8(&p.buffer).unwrap_or("");
    let line1 = &text[..text.len().min(30)];
    let line2 = if text.len() > 30 { &text[30..] } else { "" };
    draw_text(canvas, tx, ty + 36, line1, Font::F6x8, INK);
    if !line2.is_empty() {
        draw_text(
            canvas,
            tx,
            ty + 48,
            &line2[..line2.len().min(30)],
            Font::F6x8,
            INK,
        );
    }

    // Blinking block cursor (SPEC: block cursor, blink at ~4 Hz).
    if (now * 4.0).fract() < 0.5 || p.cursor {
        let (line_idx, col) = if text.len() > 30 {
            (1, text.len() - 30)
        } else {
            (0, text.len())
        };
        let cx = tx + col as i32 * 6;
        let cy = ty + 36 + line_idx * 12;
        canvas.fill_rect(cx, cy, 6, 8, INK);
    }

    // Footer note.
    let fy = CONTENT_Y + 108;
    if p.dispatch {
        draw_text(
            canvas,
            tx,
            fy,
            "dispatching to host...",
            Font::F6x8,
            ACCENT_BLUE,
        );
    } else {
        draw_text(
            canvas,
            tx,
            fy,
            "ENTER dispatch  ESC cancel",
            Font::F6x8,
            INK,
        );
    }
    let _ = RULE_Y;
}
