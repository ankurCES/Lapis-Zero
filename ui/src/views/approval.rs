//! View 4: Tool approval modal (SPEC 5.3). Slides up over a Bayer-dithered
//! backdrop, shows an action badge, target host, summary and (for GUI clicks)
//! a 120x68 1-bit dithered preview with an inverted 7px crosshair.

use crate::gfx::canvas::{Canvas, ACCENT_AMBER, ACCENT_BLUE, HEIGHT, INK, PAPER, WIDTH};
use crate::gfx::font::{draw_text, Font};
use crate::gfx::spring::Spring;

use super::RULE_Y;

/// Action kinds for the modal badge (SPEC 6.1, 6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    VncClick,
    VncType,
    VncKey,
    SshExec,
    /// An action this build does not know: never mislabel it as a known one.
    Other,
}

impl ActionKind {
    /// Parses an `approval_request.action` string (SPEC 6.3).
    pub fn from_action(action: &str) -> Self {
        match action {
            "vnc_click" => ActionKind::VncClick,
            "vnc_type" => ActionKind::VncType,
            "vnc_key" => ActionKind::VncKey,
            "ssh_exec" => ActionKind::SshExec,
            _ => ActionKind::Other,
        }
    }

    /// Badge text.
    pub fn label(self) -> &'static str {
        match self {
            ActionKind::VncClick => "VNC CLICK",
            ActionKind::VncType => "VNC TYPE",
            ActionKind::VncKey => "VNC KEY",
            ActionKind::SshExec => "SSH EXEC",
            ActionKind::Other => "ACTION",
        }
    }
}

/// One pending approval request.
pub struct Approval {
    pub request_id: String,
    pub kind: ActionKind,
    pub host: String,
    pub summary: String,
    pub risk: String,
    /// Base64 string (empty if no preview, e.g. SSH exec).
    pub preview_b64: String,
    /// Spring pop-in progress (0.0 = fully shown, 1.0 = hidden).
    pub pop: Spring,
}

impl Approval {
    /// A modal at rest (used when the view is opened directly for dumps).
    pub fn empty() -> Self {
        Self {
            request_id: String::new(),
            kind: ActionKind::SshExec,
            host: "macbook-pro".to_string(),
            summary: "systemctl restart worker".to_string(),
            risk: "high".to_string(),
            preview_b64: String::new(),
            pop: Spring::new(360.0, 28.0, 0.0),
        }
    }

    /// Synthetic high-risk GUI click with a packed 1-bit preview (SPEC 6.1).
    pub fn synthetic() -> Self {
        let preview = synthetic_preview_b64();
        Self {
            request_id: String::new(),
            kind: ActionKind::VncClick,
            host: "macbook-pro".to_string(),
            summary: "Click Delete Repository at (612, 304)".to_string(),
            risk: "high".to_string(),
            preview_b64: preview.to_string(),
            pop: Spring::new(360.0, 28.0, 1.0),
        }
    }

    /// Constructs a live approval modal from a daemon `approval_request` payload.
    pub fn from_request(
        request_id: String,
        kind: ActionKind,
        host: String,
        summary: String,
        risk: String,
        preview_b64: String,
    ) -> Self {
        Self {
            request_id,
            kind,
            host,
            summary,
            risk,
            preview_b64,
            pop: Spring::new(360.0, 28.0, 1.0),
        }
    }

    /// Records a decision so the coordinator can release the modal.
    pub fn finish(&mut self, _decision: &str) {
        self.pop.set_target(1.0);
    }

    /// Snaps the pop-in spring to rest (fully shown) for deterministic frames.
    pub fn settle(&mut self) {
        self.pop.snap_to(0.0);
    }
}

/// The 1-bit 120x68 preview, packed MSB-first into `120*68/8 = 1020` bytes.
pub fn render_preview(canvas: &mut Canvas, x: i32, y: i32, packed: &[u8]) {
    if packed.len() < 1020 {
        return;
    }
    // Draw the preview cells with the accent crosshair inverted as an overlay.
    for py in 0..68i32 {
        for px in 0..120i32 {
            let bit = px as usize + py as usize * 120;
            let on = (packed[bit / 8] >> (7 - (bit % 8))) & 1 != 0;
            canvas.set_pixel(x + px, y + py, if on { INK } else { PAPER });
        }
    }
    // 7px inverted crosshair centered on the screen (the server draws it on the
    // actual click point; synthetic data centers it).
    let cx = x + 60;
    let cy = y + 34;
    canvas.draw_rect(cx - 3, cy - 3, 7, 7, ACCENT_AMBER);
    canvas.fill_rect(cx - 1, cy - 3, 3, 7, PAPER);
    canvas.fill_rect(cx - 3, cy - 1, 7, 3, PAPER);
    canvas.fill_rect(cx, cy, 1, 1, INK);
    canvas.draw_rect(x - 1, y - 1, 122, 70, INK);
}

pub fn render(canvas: &mut Canvas, a: &Approval) {
    // Bayer-dithered dim backdrop over the whole content area.
    canvas.fill_rect_dithered(0, RULE_Y + 1, WIDTH, HEIGHT - RULE_Y - 1, 42, INK, PAPER);

    // Panel slides up from below the canvas to its rest position. `pop` rests at
    // 0.0 = fully shown; 1.0 = fully hidden, so progress inverts the spring.
    let rest_y = RULE_Y + 4;
    let prog = (1.0 - a.pop.value()).clamp(0.0, 1.0);
    let panel_h = HEIGHT - rest_y - 5; // bottom border sits 5px above the canvas base
    let py = (HEIGHT as f32 - prog * (HEIGHT - rest_y) as f32).round() as i32;
    let px = 12;
    let pw = WIDTH - 2 * px;

    // Panel body (paper) + 1px frame.
    canvas.fill_rect(px, py, pw, panel_h, PAPER);
    canvas.draw_rect(px, py, pw, panel_h, INK);

    let tx = px + 7;

    // Action badge (inverted), top line.
    let badge = a.kind.label();
    let bw = Font::F6x8.text_width(badge) + 8;
    canvas.fill_rect(tx, py + 5, bw, 12, INK);
    canvas.fill_rect(
        tx + 2,
        py + 7,
        3,
        3,
        if a.risk == "high" {
            ACCENT_AMBER
        } else {
            ACCENT_BLUE
        },
    );
    draw_text(canvas, tx + 7, py + 7, badge, Font::F6x8, PAPER);

    // Host and risk on the same baseline as the badge's right side.
    let mut hb = [0u8; 40];
    let h = super::fmt_into(&mut hb, &["host:", &a.host, "  risk:", &a.risk]);
    draw_text(canvas, tx + bw + 8, py + 7, h, Font::F6x8, INK);

    // Summary (F6x8 so long action summaries wrap within the panel width).
    let max_lines = if a.preview_b64.is_empty() { 4 } else { 2 };
    let mut sy = py + 22;
    for line in wrap_text(&a.summary, 42).into_iter().take(max_lines) {
        draw_text(canvas, tx, sy, &line, Font::F6x8, INK);
        sy += 10;
    }

    // Preview if present (renders below the summary).
    let preview_y = sy + 2;
    if !a.preview_b64.is_empty() {
        if let Ok(bytes) = b64_decode(a.preview_b64.as_bytes()) {
            render_preview(canvas, px + (pw - 120) / 2, preview_y, &bytes);
        }
        sy = preview_y + 70;
    } else {
        let note = match a.kind {
            ActionKind::SshExec => "(no preview - SSH exec)",
            _ => "(no preview)",
        };
        draw_text(canvas, tx, preview_y, note, Font::F6x8, INK);
        sy = preview_y + 12;
    }

    // Controls pinned to the panel's bottom edge.
    let cy = py + panel_h - 11;
    draw_text(
        canvas,
        tx,
        cy,
        "ENTER approve-once  A approve-session  ESC deny",
        Font::F6x8,
        INK,
    );
    let _ = sy;
}

/// Splits `text` into hard-wrapped chunks of at most `max_chars`, allocating a
/// small Vec (modal only; not on the per-frame animation path).
fn wrap_text(text: &str, max_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if rest.chars().count() <= max_chars {
            out.push(rest.to_string());
            break;
        }
        // Walk to the last char boundary within the limit.
        let mut end = 0;
        for (i, _) in rest.char_indices().take(max_chars + 1) {
            end = i;
        }
        let split = rest
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(rest.len())
            .max(end);
        out.push(rest[..split].to_string());
        rest = &rest[split..];
    }
    out
}

/// Decodes padded/unpadded base64 into a byte vector (stdlib, no deps).
pub(crate) fn b64_decode(s: &[u8]) -> Result<Vec<u8>, ()> {
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &c in s {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

/// Packs a deterministic synthetic desktop screenshot-ish pattern into 1020 b64 bytes.
fn synthetic_preview_b64() -> &'static str {
    // A diagonal-ish checkerboard with a brighter "window" region so the
    // preview and its inverted crosshair are visually verifiable.
    let mut packed = [0u8; 1020];
    for py in 0..68usize {
        for px in 0..120usize {
            let in_window = (30..90).contains(&px) && (15..54).contains(&py);
            let on = if in_window {
                (px / 2 + py / 3) % 3 == 0
            } else {
                (px + py) % 7 < 2
            };
            if on {
                let bit = px + py * 120;
                packed[bit / 8] |= 1 << (7 - (bit % 8));
            }
        }
    }
    // Encode once and leak (process-lifetime synthetic fixture).
    Box::leak(base64_encode(&packed).into_boxed_str())
}

/// Standard padded base64 (stdlib only); also encodes `screen_snapshot` PNGs.
pub(crate) fn base64_encode(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for chunk in b.chunks(3) {
        let n = chunk.len();
        let a = chunk[0] as u32;
        let b2 = chunk.get(1).copied().unwrap_or(0) as u32;
        let c = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (a << 16) | (b2 << 8) | c;
        s.push(T[(triple >> 18) as usize & 63] as char);
        s.push(T[(triple >> 12) as usize & 63] as char);
        s.push(if n > 1 {
            T[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if n > 2 {
            T[triple as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_packing_is_1020_bytes() {
        let b64 = synthetic_preview_b64();
        let bytes = b64_decode(b64.as_bytes()).unwrap();
        assert_eq!(bytes.len(), 1020);
    }

    #[test]
    fn base64_round_trips() {
        let data: Vec<u8> = (0..48).collect();
        let enc = base64_encode(&data);
        let dec = b64_decode(enc.as_bytes()).unwrap();
        assert_eq!(dec, data);
        assert!(
            enc.ends_with("==") == (48 % 3 == 1)
                || enc.ends_with('=') == (48 % 3 == 2)
                || !enc.ends_with('=')
        );
    }

    #[test]
    fn action_labels_cover_every_spec_action() {
        let label = |action| ActionKind::from_action(action).label();
        assert_eq!(label("vnc_click"), "VNC CLICK");
        assert_eq!(label("vnc_type"), "VNC TYPE");
        assert_eq!(label("vnc_key"), "VNC KEY");
        assert_eq!(label("ssh_exec"), "SSH EXEC");
        // Unknown actions get a neutral badge, never a known action's name.
        assert_eq!(label("launch_missiles"), "ACTION");
        assert_eq!(label(""), "ACTION");
    }

    #[test]
    fn render_has_crosshair_present() {
        let mut c = Canvas::new();
        let mut a = Approval::synthetic();
        a.settle();
        render(&mut c, &a);
        // The amber crosshair frame is drawn somewhere within canvas bounds.
        assert!(c.pixels().chunks_exact(3).any(|p| p == [0xFF, 0x6B, 0x2B]));
    }
}
