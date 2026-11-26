//! The deck's animated character: a small hooded robot (grey knit hood and hoodie, a glossy
//! black visor, two glowing eyes) that shows what the agent is doing, the way the Lapis head
//! does on the CardputerZero and in the companion app. Drawn with egui shapes, no assets.

use crate::theme;
use egui::{Align2, Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use std::f32::consts::TAU;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    Idle,
    Thinking,
    Acting,
    Waiting,
    Listening,
    Happy,
    Offline,
}

impl Mood {
    pub fn label(self) -> &'static str {
        match self {
            Mood::Idle => "READY",
            Mood::Thinking => "THINKING",
            Mood::Acting => "WORKING",
            Mood::Waiting => "NEEDS YOU",
            Mood::Listening => "LISTENING",
            Mood::Happy => "DONE",
            Mood::Offline => "OFFLINE",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Mood::Idle => "Y opens commands",
            Mood::Thinking => "planning the next step",
            Mood::Acting => "running a step",
            Mood::Waiting => "an approval is waiting",
            Mood::Listening => "speak now",
            Mood::Happy => "task finished",
            Mood::Offline => "daemon not reachable",
        }
    }

    pub fn eye(self) -> Color32 {
        match self {
            Mood::Acting => theme::CYAN,
            Mood::Waiting => theme::RED,
            Mood::Offline => Color32::from_rgb(70, 74, 80),
            _ => theme::EYE,
        }
    }
}

const HOOD: Color32 = Color32::from_rgb(124, 127, 133);
const HOOD_DARK: Color32 = Color32::from_rgb(98, 101, 106);
const HOODIE: Color32 = Color32::from_rgb(110, 113, 118);
const OPENING: Color32 = Color32::from_rgb(40, 42, 46);
const VISOR: Color32 = Color32::from_rgb(11, 12, 15);
const STRING: Color32 = Color32::from_rgb(233, 195, 180);

fn ellipse(c: Pos2, rx: f32, ry: f32, n: usize) -> Vec<Pos2> {
    (0..n)
        .map(|i| {
            let a = i as f32 / n as f32 * TAU;
            Pos2::new(c.x + rx * a.cos(), c.y + ry * a.sin())
        })
        .collect()
}

/// Smooth pseudo-random in 0..1 from a time and a seed (deterministic, no RNG state).
fn wobble(t: f32, seed: f32) -> f32 {
    ((t * 0.37 + seed).sin() * 43758.547).fract().abs()
}

/// Draws the character into `rect` at time `t` (seconds).
pub fn draw(p: &Painter, rect: Rect, mood: Mood, t: f64) {
    let t = t as f32;
    let s = (rect.width() / 220.0).min(rect.height() / 220.0);
    let origin = rect.center() - Vec2::splat(110.0 * s);
    let speed = if mood == Mood::Acting { 4.0 } else { 1.6 };
    let bob = (t * speed).sin() * 2.5 * s;
    let at = |x: f32, y: f32| origin + Vec2::new(x * s, y * s + bob);
    let fixed = |x: f32, y: f32| origin + Vec2::new(x * s, y * s);

    // Shadow (does not bob).
    p.add(Shape::convex_polygon(
        ellipse(fixed(110.0, 212.0), 62.0 * s, 6.0 * s, 24),
        Color32::from_black_alpha(90),
        Stroke::NONE,
    ));
    // Shoes (they stay on the ground), then the hoodie.
    for x in [66.0, 118.0] {
        let shoe = Rect::from_min_size(fixed(x, 198.0), Vec2::new(36.0 * s, 13.0 * s));
        p.rect_filled(shoe, CornerRadius::same((6.0 * s) as u8), STRING);
        let sole = Rect::from_min_size(fixed(x, 207.0), Vec2::new(36.0 * s, 4.0 * s));
        p.rect_filled(sole, CornerRadius::same((2.0 * s) as u8), HOOD_DARK);
    }
    let body = vec![
        at(50.0, 146.0),
        at(170.0, 146.0),
        at(178.0, 200.0),
        at(42.0, 200.0),
    ];
    p.add(Shape::convex_polygon(body, HOODIE, Stroke::NONE));
    let hem = Rect::from_min_max(at(42.0, 192.0), at(178.0, 202.0));
    p.rect_filled(hem, CornerRadius::same(3), HOOD_DARK);
    let pocket = Rect::from_min_max(at(78.0, 168.0), at(142.0, 190.0));
    p.rect_stroke(
        pocket,
        CornerRadius::same((5.0 * s) as u8),
        Stroke::new(1.5 * s, HOOD_DARK),
        egui::StrokeKind::Inside,
    );
    // Sleeves and hands.
    for (x0, x1) in [(36.0, 56.0), (164.0, 184.0)] {
        p.rect_filled(
            Rect::from_min_max(at(x0, 150.0), at(x1, 192.0)),
            CornerRadius::same((9.0 * s) as u8),
            HOODIE,
        );
        p.circle_filled(at((x0 + x1) / 2.0, 194.0), 8.0 * s, VISOR);
    }
    // Drawstrings.
    for (x0, x1, y1) in [(100.0, 97.0, 180.0), (120.0, 123.0, 178.0)] {
        p.line_segment([at(x0, 148.0), at(x1, y1)], Stroke::new(2.5 * s, STRING));
        p.circle_filled(at(x1, y1 + 2.0), 2.4 * s, STRING);
    }
    // Hood with a knit texture, then its dark opening.
    p.add(Shape::convex_polygon(
        ellipse(at(110.0, 94.0), 80.0 * s, 74.0 * s, 48),
        HOOD,
        Stroke::NONE,
    ));
    for i in 0..14 {
        let x = 40.0 + i as f32 * 10.5;
        let dy = ((x - 110.0) / 80.0).powi(2);
        let top = 30.0 + 40.0 * dy;
        let bottom = 160.0 - 30.0 * dy;
        p.line_segment(
            [at(x, top), at(x, bottom)],
            Stroke::new(1.0 * s, HOOD_DARK.gamma_multiply(0.5)),
        );
    }
    p.add(Shape::convex_polygon(
        ellipse(at(110.0, 102.0), 60.0 * s, 54.0 * s, 40),
        OPENING,
        Stroke::NONE,
    ));
    // Visor with a gloss highlight.
    let visor = Rect::from_center_size(at(110.0, 102.0), Vec2::new(104.0 * s, 78.0 * s));
    p.rect_filled(visor, CornerRadius::same((28.0 * s) as u8), VISOR);
    p.rect_filled(
        Rect::from_min_size(
            visor.min + Vec2::new(12.0 * s, 6.0 * s),
            Vec2::new(46.0 * s, 9.0 * s),
        ),
        CornerRadius::same((5.0 * s) as u8),
        Color32::from_white_alpha(14),
    );

    // Eyes: blink, glance, pulse, by mood.
    let color = mood.eye();
    let blink_phase = (t % 4.3) / 4.3;
    let mut open = if blink_phase > 0.965 { 0.12 } else { 1.0 };
    if mood == Mood::Waiting {
        open = if (t * 3.0).fract() > 0.85 { 0.2 } else { 1.0 };
    }
    if mood == Mood::Offline {
        open = 0.35;
    }
    let glance = match mood {
        Mood::Thinking => Vec2::new(4.0, -7.0),
        Mood::Idle => {
            let slot = (t / 3.0).floor();
            let w = wobble(slot, 1.7);
            Vec2::new(
                if w < 0.25 {
                    -6.0
                } else if w > 0.75 {
                    6.0
                } else {
                    0.0
                },
                0.0,
            )
        }
        _ => Vec2::ZERO,
    };
    let pulse = match mood {
        Mood::Waiting | Mood::Thinking => 0.7 + 0.3 * (t * 4.0).sin().abs(),
        Mood::Offline => 0.4,
        _ => 1.0,
    };
    for ex in [88.0, 132.0] {
        let c = at(ex, 102.0) + glance * s;
        if mood == Mood::Happy {
            let w = 9.0 * s;
            let stroke = Stroke::new(3.5 * s, color);
            p.line_segment(
                [c + Vec2::new(-w, 5.0 * s), c + Vec2::new(0.0, -5.0 * s)],
                stroke,
            );
            p.line_segment(
                [c + Vec2::new(0.0, -5.0 * s), c + Vec2::new(w, 5.0 * s)],
                stroke,
            );
            continue;
        }
        let size = Vec2::new(14.0 * s, 26.0 * s * open);
        for (grow, alpha) in [(10.0, 0.08), (6.0, 0.16), (3.0, 0.32)] {
            p.rect_filled(
                Rect::from_center_size(c, size + Vec2::splat(grow * s)),
                CornerRadius::same(((7.0 + grow) * s) as u8),
                color.gamma_multiply(alpha * pulse),
            );
        }
        p.rect_filled(
            Rect::from_center_size(c, size),
            CornerRadius::same((7.0 * s) as u8),
            color.gamma_multiply(pulse),
        );
        p.rect_filled(
            Rect::from_center_size(
                c + Vec2::new(-2.0 * s, -4.0 * s * open),
                size * Vec2::new(0.35, 0.3),
            ),
            CornerRadius::same((3.0 * s) as u8),
            Color32::from_white_alpha(70),
        );
    }

    // Extras per mood.
    match mood {
        Mood::Thinking => {
            for i in 0..3 {
                let a = ((t * 3.0 - i as f32 * 0.5).sin() * 0.5 + 0.5).clamp(0.15, 1.0);
                p.circle_filled(
                    at(150.0 + i as f32 * 14.0, 22.0),
                    4.5 * s,
                    theme::CYAN.gamma_multiply(a),
                );
            }
        }
        Mood::Listening => {
            for i in 0..4 {
                let h = (6.0 + 14.0 * ((t * 7.0 + i as f32).sin() * 0.5 + 0.5)) * s;
                let x = at(196.0 + i as f32 * 7.0, 102.0);
                p.line_segment(
                    [x - Vec2::new(0.0, h), x + Vec2::new(0.0, h)],
                    Stroke::new(3.0 * s, theme::EYE),
                );
            }
        }
        Mood::Offline => {
            for i in 0..3 {
                let k = ((t * 0.6 + i as f32 / 3.0) % 1.0) * 30.0;
                p.text(
                    at(160.0 + i as f32 * 8.0, 40.0 - k),
                    Align2::CENTER_CENTER,
                    "z",
                    theme::mono((12.0 + i as f32 * 3.0) * s),
                    theme::DIM.gamma_multiply(1.0 - k / 30.0),
                );
            }
        }
        Mood::Waiting => {
            let r = Rect::from_center_size(at(180.0, 28.0), Vec2::splat(26.0 * s));
            p.circle_filled(r.center(), 13.0 * s, theme::RED.gamma_multiply(pulse));
            p.text(
                r.center(),
                Align2::CENTER_CENTER,
                "!",
                theme::mono(18.0 * s),
                Color32::WHITE,
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mood_draws() {
        let ctx = egui::Context::default();
        for mood in [
            Mood::Idle,
            Mood::Thinking,
            Mood::Acting,
            Mood::Waiting,
            Mood::Listening,
            Mood::Happy,
            Mood::Offline,
        ] {
            let out = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    draw(
                        ui.painter(),
                        Rect::from_min_size(Pos2::ZERO, Vec2::splat(220.0)),
                        mood,
                        12.3,
                    );
                });
            });
            assert!(out.shapes.len() > 20, "{mood:?}");
            assert!(!mood.label().is_empty());
        }
        assert_eq!(Mood::Waiting.eye(), theme::RED);
    }
}
