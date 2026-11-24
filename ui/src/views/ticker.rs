//! View 2: Live activity ticker with a spring-scrolled log (SPEC 5.3).

use crate::gfx::canvas::{Canvas, ACCENT_AMBER, ACCENT_BLUE, HEIGHT, INK, PAPER};
use crate::gfx::font::{draw_text, Font};
use crate::gfx::spring::Spring;

use super::{CONTENT_H, CONTENT_Y, RULE_Y};

/// Maximum number of retained ticker entries (bounded for the 512 MB RAM budget).
const MAX_ENTRIES: usize = 40;
/// Number of visible rows below the status bar (`(170 - 21) / 14`).
const VISIBLE_ROWS: usize = 10;

/// One ticker log line (seeded with demo entries and appended by `activity_event`).
struct Entry {
    ts: String,
    kind: String,
    text: String,
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
                ts: "14:31:04".to_string(),
                kind: "VNC".to_string(),
                text: "click(840, 412) left".to_string(),
            },
            Entry {
                ts: "14:31:06".to_string(),
                kind: "SSH".to_string(),
                text: "git status -> 0".to_string(),
            },
            Entry {
                ts: "14:31:07".to_string(),
                kind: "VNC".to_string(),
                text: "type_text(\"make build\")".to_string(),
            },
            Entry {
                ts: "14:31:09".to_string(),
                kind: "SSH".to_string(),
                text: "systemctl status worker".to_string(),
            },
            Entry {
                ts: "14:31:11".to_string(),
                kind: "LLM".to_string(),
                text: "hovered Retry Build btn".to_string(),
            },
            Entry {
                ts: "14:31:13".to_string(),
                kind: "VNC".to_string(),
                text: "scroll(0, -240)".to_string(),
            },
            Entry {
                ts: "14:31:15".to_string(),
                kind: "SSH".to_string(),
                text: "journalctl -n 20".to_string(),
            },
            Entry {
                ts: "14:31:17".to_string(),
                kind: "VNC".to_string(),
                text: "click(614, 388) right".to_string(),
            },
        ];
        Self {
            entries,
            sprint: Spring::new(240.0, 24.0, 0.0),
        }
    }

    /// Appends one live activity entry, capping history to [`MAX_ENTRIES`].
    pub fn push(&mut self, ts: String, kind: String, text: String) {
        self.entries.push(Entry { ts, kind, text });
        if self.entries.len() > MAX_ENTRIES {
            let excess = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(0..excess);
        }
        self.sprint.set_position(1.0);
        self.sprint.set_target(0.0);
    }

    /// Number of currently retained ticker rows.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if no entries are present.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the most recent `(ts, kind, text)` tuple, if any.
    pub fn last_entry(&self) -> Option<(&str, &str, &str)> {
        self.entries
            .last()
            .map(|e| (e.ts.as_str(), e.kind.as_str(), e.text.as_str()))
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
    let start = tv.entries.len().saturating_sub(VISIBLE_ROWS);
    for (i, e) in tv.entries[start..].iter().enumerate() {
        if y > HEIGHT {
            break;
        }
        // Timestamp
        draw_text(canvas, 4, y, &e.ts, Font::F6x8, INK);
        // Kind badge
        let kc = match e.kind.as_str() {
            "VNC" => ACCENT_BLUE,
            "SSH" => INK,
            "LLM" => ACCENT_AMBER,
            _ => INK,
        };
        draw_text(canvas, 62, y, &e.kind, Font::F6x8, kc);
        // Body text, clipped to the right edge by canvas clipping.
        draw_text(canvas, 98, y, &e.text, Font::F6x8, INK);
        y += 14;
        let _ = i;
    }
    // Baseline rule at the bottom of the feed area.
    canvas.hline(0, HEIGHT - 1, 320, PAPER);
    let _ = (CONTENT_H, RULE_Y);
}
