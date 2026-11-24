//! View 2: Live activity ticker with a spring-scrolled log (SPEC 5.3).

use crate::gfx::canvas::{Canvas, ACCENT_AMBER, ACCENT_BLUE, HEIGHT, INK, PAPER};
use crate::gfx::font::{draw_text, Font};
use crate::gfx::spring::Spring;

use super::{CONTENT_H, CONTENT_Y, RULE_Y};

/// One ticker log line (synthetic until IPC lands).
struct Entry {
    ts: &'static str,
    kind: &'static str,
    text: &'static str,
}

pub struct TickerView {
    entries: Vec<Entry>,
    sprint: Spring,
}

impl TickerView {
    /// Synthetic Phase 1 feed.
    pub fn demo() -> Self {
        let entries = vec![
            Entry {
                ts: "14:31:04",
                kind: "VNC",
                text: "click(840, 412) left",
            },
            Entry {
                ts: "14:31:06",
                kind: "SSH",
                text: "git status -> 0",
            },
            Entry {
                ts: "14:31:07",
                kind: "VNC",
                text: "type_text(\"make build\")",
            },
            Entry {
                ts: "14:31:09",
                kind: "SSH",
                text: "systemctl status worker",
            },
            Entry {
                ts: "14:31:11",
                kind: "LLM",
                text: "hovered Retry Build btn",
            },
            Entry {
                ts: "14:31:13",
                kind: "VNC",
                text: "scroll(0, -240)",
            },
            Entry {
                ts: "14:31:15",
                kind: "SSH",
                text: "journalctl -n 20",
            },
            Entry {
                ts: "14:31:17",
                kind: "VNC",
                text: "click(614, 388) right",
            },
        ];
        Self {
            entries,
            sprint: Spring::new(240.0, 24.0, 0.0),
        }
    }

    pub fn tick(&mut self, dt: f32) {
        self.sprint.tick(dt);
    }

    pub fn is_active(&self) -> bool {
        self.sprint.is_active()
    }

    /// Snaps the scroll spring to rest for deterministic snapshots.
    pub fn settle(&mut self) {
        self.sprint.snap_to(0.0);
    }
}

pub fn render(canvas: &mut Canvas, tv: &TickerView) {
    // The newest entry feeds in with a small spring offset; mostly static here.
    let off = (tv.sprint.value() * 6.0).round() as i32;
    let mut y = CONTENT_Y + 2 - off;
    for (i, e) in tv.entries.iter().enumerate() {
        if y > HEIGHT {
            break;
        }
        // Timestamp
        draw_text(canvas, 4, y, e.ts, Font::F6x8, INK);
        // Kind badge
        let kc = match e.kind {
            "VNC" => ACCENT_BLUE,
            "SSH" => INK,
            "LLM" => ACCENT_AMBER,
            _ => INK,
        };
        draw_text(canvas, 62, y, e.kind, Font::F6x8, kc);
        // Body text, clipped to the right edge by canvas clipping.
        draw_text(canvas, 98, y, e.text, Font::F6x8, INK);
        y += 14;
        let _ = i;
    }
    // Baseline rule at the bottom of the feed area.
    canvas.hline(0, HEIGHT - 1, 320, PAPER);
    let _ = (CONTENT_H, RULE_Y);
}
