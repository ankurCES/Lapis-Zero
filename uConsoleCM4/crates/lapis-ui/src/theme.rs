//! The HUD's look, after the Lapis-Deck styleguide: near-black panels, two accents (phosphor
//! cyan for focus and live state, red for alerts), 2 px borders with a soft glow, monospace
//! headings and readable sans body text sized for a 5" 1280x720 panel.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};

pub const BG: Color32 = Color32::from_rgb(7, 10, 14);
pub const PANEL: Color32 = Color32::from_rgb(13, 19, 26);
pub const RAISED: Color32 = Color32::from_rgb(20, 31, 40);
pub const LINE: Color32 = Color32::from_rgb(34, 54, 66);
pub const TEXT: Color32 = Color32::from_rgb(240, 244, 247);
pub const DIM: Color32 = Color32::from_rgb(170, 177, 184);
pub const FAINT: Color32 = Color32::from_rgb(98, 110, 122);
pub const CYAN: Color32 = Color32::from_rgb(0, 255, 255);
pub const RED: Color32 = Color32::from_rgb(255, 42, 61);
/// Warnings (sparingly; the two accents carry the HUD).
pub const AMBER: Color32 = Color32::from_rgb(255, 184, 64);
/// The character's eyes (its own colour, not a UI accent).
pub const EYE: Color32 = Color32::from_rgb(255, 120, 130);

// Old names kept for the workspaces: online/OK is cyan, alerts are red.
pub const GREEN: Color32 = CYAN;
pub const CRIMSON: Color32 = RED;
pub const PAPER_BG: Color32 = Color32::from_rgb(236, 232, 222);

pub const RADIUS: u8 = 4;

pub fn apply(ctx: &egui::Context, paper: bool) {
    let mut v = if paper {
        Visuals::light()
    } else {
        Visuals::dark()
    };
    if !paper {
        v.panel_fill = PANEL;
        v.window_fill = RAISED;
        v.extreme_bg_color = BG;
        v.faint_bg_color = RAISED;
        v.override_text_color = Some(TEXT);
        v.selection.bg_fill = Color32::from_rgb(0, 90, 96);
        v.selection.stroke = Stroke::new(2.0_f32, CYAN);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, LINE);
        v.widgets.inactive.bg_fill = RAISED;
        v.widgets.inactive.weak_bg_fill = RAISED;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, LINE);
        v.widgets.hovered.bg_fill = Color32::from_rgb(18, 48, 56);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(18, 48, 56);
        v.widgets.hovered.bg_stroke = Stroke::new(2.0_f32, CYAN);
        v.widgets.active.bg_fill = Color32::from_rgb(0, 80, 86);
        v.widgets.active.bg_stroke = Stroke::new(2.0_f32, CYAN);
        v.text_cursor.stroke = Stroke::new(2.0_f32, CYAN);
        v.hyperlink_color = CYAN;
    } else {
        v.panel_fill = PAPER_BG;
        v.extreme_bg_color = Color32::from_rgb(248, 245, 238);
    }
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(RADIUS);
    }
    v.window_corner_radius = CornerRadius::same(RADIUS);
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.override_font_id = None;
        s.text_styles = [
            (TextStyle::Heading, FontId::new(24.0, FontFamily::Monospace)),
            (TextStyle::Body, FontId::new(17.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(16.0, FontFamily::Monospace)),
            (
                TextStyle::Monospace,
                FontId::new(15.0, FontFamily::Monospace),
            ),
            (
                TextStyle::Small,
                FontId::new(13.0, FontFamily::Proportional),
            ),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(12.0, 7.0);
        s.spacing.interact_size = egui::vec2(40.0, 34.0);
        s.spacing.text_edit_width = 320.0;
        s.interaction.selectable_labels = false;
    });
}

/// Monospace label font at a size.
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn sans(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn tier_color(tier: u64) -> Color32 {
    match tier {
        0 => DIM,
        1 => CYAN,
        _ => RED,
    }
}
