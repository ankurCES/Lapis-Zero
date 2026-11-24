//! View 6: First-boot / network & system onboarding (SPEC 5.3). Wi-Fi SSID/IP,
//! Tailscale state, API key status, battery, and quick actions.

use crate::gfx::canvas::{Canvas, ACCENT_AMBER, ACCENT_BLUE, INK, PAPER};
use crate::gfx::font::{draw_text, Font};

use super::{CONTENT_Y, RULE_Y};

pub struct SetupView {
    pub wifi_ssid: String,
    pub local_ip: String,
    pub tailscale: String,
    pub tailscale_auth: Option<String>,
    pub api_key_ok: bool,
    pub battery_v: String,
    pub battery_pct: u8,
    /// Highlighted quick action (index into [`ACTIONS`]).
    pub action: usize,
}

/// Quick actions: button label and its `system_control` command.
pub const ACTIONS: [(&str, &str); 3] = [
    ("Retry Tailscale", "retry_tailscale"),
    ("Reboot", "reboot"),
    ("Shutdown", "shutdown"),
];

impl SetupView {
    pub fn demo() -> Self {
        Self {
            wifi_ssid: "HomeWiFi".to_string(),
            local_ip: "192.168.1.42".to_string(),
            tailscale: "connected".to_string(),
            tailscale_auth: None,
            api_key_ok: true,
            battery_v: "3.9V".to_string(),
            battery_pct: 84,
            action: 0,
        }
    }
}

pub fn render(canvas: &mut Canvas, s: &SetupView) {
    let mut y = CONTENT_Y + 4;
    let x = 8;

    draw_text(canvas, x, y, "NETWORK & SYSTEM", Font::F8x14, INK);
    y += 18;

    // Wi-Fi.
    let mut wb = [0u8; 48];
    let w = super::fmt_into(&mut wb, &["Wi-Fi: ", &s.wifi_ssid, "  ", &s.local_ip]);
    draw_text(canvas, x, y, w, Font::F6x8, INK);
    y += 14;

    // Tailscale.
    let mut tb = [0u8; 64];
    let t = super::fmt_into(&mut tb, &["Tailscale: ", &s.tailscale]);
    draw_text(
        canvas,
        x,
        y,
        t,
        Font::F6x8,
        if s.tailscale == "connected" {
            ACCENT_BLUE
        } else {
            ACCENT_AMBER
        },
    );
    y += 14;

    if let Some(url) = s.tailscale_auth.as_deref() {
        let mut ub = [0u8; 64];
        let u = super::fmt_into(&mut ub, &["login: ", url]);
        draw_text(canvas, x, y, u, Font::F6x8, ACCENT_AMBER);
        y += 14;
    }

    // API key.
    let mut ab = [0u8; 48];
    let a = super::fmt_into(
        &mut ab,
        &["API key: ", if s.api_key_ok { "OK" } else { "MISSING" }],
    );
    draw_text(
        canvas,
        x,
        y,
        a,
        Font::F6x8,
        if s.api_key_ok { INK } else { ACCENT_AMBER },
    );
    y += 14;

    // Battery.
    let mut bb = [0u8; 48];
    let mut pb = [0u8; 4];
    let pct = super::fmt_pct(&mut pb, s.battery_pct);
    let b = super::fmt_into(&mut bb, &["Battery: ", &s.battery_v, "  ", pct]);
    draw_text(canvas, x, y, b, Font::F6x8, INK);
    y += 22;

    // Quick actions as inverted buttons.
    let mut bx = x;
    for (i, (label, _)) in ACTIONS.iter().enumerate() {
        let w = Font::F6x8.text_width(label) + 10;
        let selected = i == s.action;
        if selected {
            canvas.fill_rect(bx, y, w, 16, INK);
        } else {
            canvas.draw_rect(bx, y, w, 16, INK);
        }
        let color = if selected { PAPER } else { INK };
        draw_text(canvas, bx + 5, y + 4, label, Font::F6x8, color);
        bx += w + 8;
    }
    let _ = RULE_Y;
}
