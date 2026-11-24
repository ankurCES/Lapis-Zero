//! View 1: Companion / desk clock (SPEC 5.3). Left: mascot + large clock.
//! Right: active task title (2 lines), step counter, last tool summary, next cron.

use crate::gfx::canvas::{Canvas, ACCENT_BLUE, INK};
use crate::gfx::font::{draw_text, Font};
use crate::gfx::mascot::{self, MascotAnim, MascotState};
use crate::ipc::DaemonState;

use super::{Header, RULE_Y};

/// Left/right column split (SPEC 5.3): mascot zone x=0..146, task zone x=152..316.
const SPLIT: i32 = 150;

/// Synthetic task defaults shown before the first `state_sync` arrives.
const TASK_TITLE: &str = "Check CI logs and\nrestart worker";
const TASK_STEP: &str = "Step 4/25";
const TASK_LAST: &str = "vnc_click retry build";
const TASK_NEXT: &str = "08:00 Daily Digest";

#[allow(clippy::too_many_arguments)]
pub fn render(
    canvas: &mut Canvas,
    header: &Header,
    daemon: &DaemonState,
    daemon_synced: bool,
    last_tool: &str,
    anim: &MascotAnim,
    now: f32,
) {
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

    if !daemon_synced {
        for line in TASK_TITLE.lines() {
            draw_text(canvas, rx, y, line, Font::F8x14, INK);
            y += 20;
        }
        y += 4;
        draw_text(canvas, rx, y, TASK_STEP, Font::F6x8, INK);
        y += 14;

        draw_text(canvas, rx, y, "last", Font::F6x8, INK);
        draw_text(canvas, rx + 24, y, TASK_LAST, Font::F6x8, ACCENT_BLUE);
        y += 14;

        draw_text(canvas, rx, y, "next:", Font::F6x8, INK);
        draw_text(canvas, rx + 30, y, TASK_NEXT, Font::F6x8, INK);
    } else {
        let title = daemon
            .active_task
            .as_ref()
            .map(|t| t.title.as_str())
            .unwrap_or("No active task");
        let (l1, l2) = split_two_lines(title, 19);
        draw_text(canvas, rx, y, l1, Font::F8x14, INK);
        y += 20;
        if !l2.is_empty() {
            draw_text(canvas, rx, y, l2, Font::F8x14, INK);
        }
        y += 24;

        let mut step_buf = [0u8; 24];
        let step_str = if let Some(task) = daemon.active_task.as_ref() {
            let mut s1 = [0u8; 10];
            let mut s2 = [0u8; 10];
            let a = fmt_u32(&mut s1, task.step.clamp(0, 9999) as u32);
            let b = fmt_u32(&mut s2, task.max_steps.clamp(0, 9999) as u32);
            super::fmt_into(&mut step_buf, &["Step ", a, "/", b])
        } else {
            "Idle"
        };
        draw_text(canvas, rx, y, step_str, Font::F6x8, INK);
        y += 14;

        let last = if last_tool.is_empty() { "-" } else { last_tool };
        draw_text(canvas, rx, y, "last", Font::F6x8, INK);
        draw_text(canvas, rx + 24, y, last, Font::F6x8, ACCENT_BLUE);
        y += 14;

        let next = if daemon.next_cron.is_empty() {
            "none"
        } else {
            daemon.next_cron.as_str()
        };
        draw_text(canvas, rx, y, "next:", Font::F6x8, INK);
        draw_text(canvas, rx + 30, y, next, Font::F6x8, INK);
    }

    // Blink dot in the clock if idle and blink phase (matches mascot blink).
    if header.state == MascotState::Idle && anim.blink {
        canvas.fill_rect(30, 44, 3, 3, ACCENT_BLUE);
    }
    let _ = now;
}

/// Splits `text` into at most two slices of `<= max_chars` without allocating.
fn split_two_lines(text: &str, max_chars: usize) -> (&str, &str) {
    let mut split_at = text.len();
    for (idx, (byte_idx, _)) in text.char_indices().enumerate() {
        if idx == max_chars {
            split_at = byte_idx;
            break;
        }
    }
    if split_at == text.len() {
        return (text, "");
    }
    // Prefer splitting at a space if one exists in the first line.
    let head = &text[..split_at];
    let (l1, rest) = match head.rfind(' ') {
        Some(sp) if sp > 0 => (&text[..sp], text[sp + 1..].trim_start()),
        _ => (head, text[split_at..].trim_start()),
    };
    let mut l2_end = rest.len();
    for (idx, (byte_idx, _)) in rest.char_indices().enumerate() {
        if idx == max_chars {
            l2_end = byte_idx;
            break;
        }
    }
    (l1, &rest[..l2_end])
}

/// Formats a `u32` into a stack buffer with zero allocations.
fn fmt_u32(buf: &mut [u8; 10], mut v: u32) -> &str {
    if v == 0 {
        buf[0] = b'0';
        return std::str::from_utf8(&buf[..1]).unwrap_or("0");
    }
    let mut i = buf.len();
    while v > 0 && i > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    std::str::from_utf8(&buf[i..]).unwrap_or("0")
}
