//! View 1: Companion / desk clock (SPEC 5.3). Left: mascot + large clock.
//! Right: active task title (2 lines), step counter, last tool summary, next cron.

use crate::gfx::canvas::{Canvas, ACCENT_BLUE, INK};
use crate::gfx::font::{draw_text, Font};
use crate::gfx::mascot::{self, MascotAnim, MascotState};

use super::{Header, RULE_Y};

/// Left/right column split (SPEC 5.3): mascot zone x=0..146, task zone x=152..316.
const SPLIT: i32 = 150;

/// Synthetic task title shown until IPC lands (Phase 2).
const TASK_TITLE: &str = "Check CI logs and\nrestart worker";
const TASK_STEP: &str = "Step 4/25";
const TASK_LAST: &str = "vnc_click retry build";
const TASK_NEXT: &str = "08:00 Daily Digest";

pub fn render(canvas: &mut Canvas, header: &Header, anim: &MascotAnim, now: f32) {
    // Left zone.
    mascot::draw(canvas, 24, 40, header.state, anim);

    // Large desk clock below the mascot (F16x28).
    let mut bb = [0u8; 5];
    let clock = super::fmt_clock(header.minutes, &mut bb);
    let cx = 12 + (124 - Font::F16x28.text_width(clock)) / 2;
    draw_text(canvas, cx, 92, clock, Font::F16x28, INK);

    // Divider column.
    canvas.vline(
        SPLIT,
        RULE_Y + 1,
        crate::gfx::canvas::HEIGHT - RULE_Y - 1,
        INK,
    );
    canvas.fill_rect(SPLIT, RULE_Y + 1, 1, 10, INK);

    // Right zone: title, step, last tool, next cron.
    let rx = SPLIT + 9;
    let mut y = RULE_Y + 8;
    draw_text(canvas, rx, y, "TASK", Font::F6x8, ACCENT_BLUE);
    y += 12;
    for line in TASK_TITLE.lines() {
        draw_text(canvas, rx, y, line, Font::F8x14, INK);
        y += 20;
    }
    y += 4;
    draw_text(canvas, rx, y, TASK_STEP, Font::F6x8, INK);
    y += 14;

    // Last tool summary, possibly clipped to one line.
    draw_text(canvas, rx, y, "last", Font::F6x8, INK);
    draw_text(canvas, rx + 24, y, TASK_LAST, Font::F6x8, ACCENT_BLUE);
    y += 14;

    draw_text(canvas, rx, y, "next:", Font::F6x8, INK);
    draw_text(canvas, rx + 30, y, TASK_NEXT, Font::F6x8, INK);

    // Blink dot in the clock if idle and blink phase (matches mascot blink).
    if header.state == MascotState::Idle && anim.blink {
        canvas.fill_rect(30, 44, 3, 3, ACCENT_BLUE);
    }
    let _ = now;
}
