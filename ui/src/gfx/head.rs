//! The agent's head as a small lit 3D model, ray-cast per pixel.
//!
//! The model is an ellipsoid shell with a dark glass visor, two eyes on a
//! screen set a little behind the glass (so they shift against the visor rim
//! when the head turns), and two ear pods. One fixed light shades it; tones
//! become the four palette colours through the canvas' 4x4 Bayer matrix, so a
//! frame still packs into the 2-bit `screen_snapshot` PNG and still survives
//! the dark theme's ink/paper swap.
//!
//! Everything here is `+ - * /` and `sqrt` on `f32` (IEEE-exact, unlike the
//! platform `sin`/`powf`), with no tables beyond the 16-byte Bayer matrix and
//! no allocation: the same pose gives the same pixels on every target.

use super::canvas::{bayer_threshold, Canvas, ACCENT_AMBER, ACCENT_BLUE, INK, PAPER};

/// Semi-axes of the shell in head radii (x across, y down, z toward the viewer).
pub const AXES: [f32; 3] = [1.0, 0.90, 0.94];
/// The light, a unit vector from the surface toward the lamp: up, left, in front.
pub const LIGHT: [f32; 3] = [-0.55, -0.52, 0.653];

/// Visor outline on the shell's front, in the head's own unit coordinates:
/// `(|x| / VISOR_W)^4 + (|y - VISOR_Y + VISOR_BOW * x^2| / VISOR_H)^4 < 1`.
/// The bow lifts the ends above the middle, so the glass wraps round the ball
/// even seen dead-on instead of sitting on it like a flat sticker.
const VISOR_W: f32 = 0.76;
const VISOR_H: f32 = 0.47;
const VISOR_Y: f32 = 0.03;
const VISOR_BOW: f32 = 0.14;
/// The light gasket around the glass ends where the same sum reaches this.
const BEZEL: f32 = 1.42;
/// The glint on the glass: where `n . L` is above the first, and again, as a
/// short second streak on the top edge, where it is inside the second.
const GLINT: f32 = 0.80;
const GLINT_2: std::ops::Range<f32> = 0.66..0.74;
/// Radius of the eye screen behind the glass, as a fraction of the shell.
const SCREEN: f32 = 0.84;
/// Eye centre and half-size on that screen, in unit coordinates.
const EYE_X: f32 = 0.37;
const EYE_Y: f32 = 0.05;
const EYE_W: f32 = 0.155;
const EYE_H: f32 = 0.25;
/// How flat an eye shape other than the open bars gets when shut: this much
/// of its height. Below the second opening it is joined by the line that
/// shut open eyes are, so strokes too thin to draw never leave a gap.
const SHUT: f32 = 0.12;
const NEARLY_SHUT: f32 = 0.35;
/// Ear pods: spheres of this radius centred here (mirrored in x), head radii.
const POD: [f32; 3] = [1.0, 0.06, -0.04];
const POD_R: f32 = 0.26;
/// The pod's lamp looks out and forward along this unit axis (mirrored in x).
const POD_LAMP: [f32; 3] = [0.74, 0.0, 0.673];
/// The lamp's lens is the cap of the pod whose normal is within `acos(0.78)`
/// of that axis; a dark ring seats it, out to this cosine.
const LENS: f32 = 0.78;
const LENS_RIM: f32 = 0.66;

/// What the eyes are showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eyes {
    /// Two rounded bars; `Pose::open` closes them to a line. (It flattens
    /// the other shapes onto their centre line the same way, which is how one
    /// shape gives way to the next.)
    Open,
    /// Two downward arcs: shut for a while, not blinking.
    Asleep,
    /// Two upward arcs.
    Happy,
    /// Two crosses.
    Cross,
    /// A `>` chevron and a cursor: a block while `cursor` is set, an
    /// underline while it is not, so the face never goes one-eyed.
    Prompt { cursor: bool },
}

/// Colour of the eye rims and pod lamps (plain paper when `None`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accent {
    None,
    Blue,
    Amber,
}

impl Accent {
    fn color(self) -> Option<u32> {
        match self {
            Accent::None => None,
            Accent::Blue => Some(ACCENT_BLUE),
            Accent::Amber => Some(ACCENT_AMBER),
        }
    }
}

/// Everything that eases between states: angles in radians, lengths in head radii.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    /// Turn toward the viewer's right.
    pub yaw: f32,
    /// Nod down.
    pub pitch: f32,
    /// Tilt clockwise.
    pub roll: f32,
    /// Height above the rest position.
    pub lift: f32,
    /// Where the eyes look, as an offset on the eye screen.
    pub look_x: f32,
    pub look_y: f32,
    /// Eye opening: 1 open, 0 shut.
    pub open: f32,
    /// Eye size multiplier.
    pub wide: f32,
    /// How much of the accent has faded in, 0 to 1.
    pub glow: f32,
}

impl Pose {
    /// Facing the viewer, eyes open, no accent.
    pub const REST: Pose = Pose {
        yaw: 0.0,
        pitch: 0.0,
        roll: 0.0,
        lift: 0.0,
        look_x: 0.0,
        look_y: 0.0,
        open: 1.0,
        wide: 1.0,
        glow: 0.0,
    };

    /// The pose `t` of the way from `self` to `to` (`t` may overshoot 0..1).
    pub fn mix(&self, to: &Pose, t: f32) -> Pose {
        let m = |a: f32, b: f32| a + (b - a) * t;
        Pose {
            yaw: m(self.yaw, to.yaw),
            pitch: m(self.pitch, to.pitch),
            roll: m(self.roll, to.roll),
            lift: m(self.lift, to.lift),
            look_x: m(self.look_x, to.look_x),
            look_y: m(self.look_y, to.look_y),
            open: m(self.open, to.open),
            wide: m(self.wide, to.wide),
            glow: m(self.glow, to.glow),
        }
    }
}

/// Sine from a 9th-order odd polynomial on the folded argument (error < 4e-6).
/// The standard library's `sin` may differ in the last bit between platforms.
pub fn sin(x: f32) -> f32 {
    let k = (x * std::f32::consts::FRAC_1_PI).round();
    let y = x - k * std::f32::consts::PI;
    let s = y * y;
    let p = y
        * (1.0
            + s * (-1.0 / 6.0 + s * (1.0 / 120.0 + s * (-1.0 / 5040.0 + s * (1.0 / 362_880.0)))));
    if (k as i64) & 1 == 0 {
        p
    } else {
        -p
    }
}

/// Cosine, as [`sin`] a quarter turn on.
pub fn cos(x: f32) -> f32 {
    sin(x + std::f32::consts::FRAC_PI_2)
}

/// True where an ordered dither of coverage `level` (`0.0..=1.0`) is on.
/// Anchored to canvas coordinates, so shading does not crawl when the head moves.
pub fn dither(x: i32, y: i32, level: f32) -> bool {
    // Thresholds 8, 24 .. 248: 0 draws nothing and 1 is solid.
    (level * 256.0) as i32 > i32::from(bayer_threshold(x, y) / 17) * 16 + 8
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Head-to-view rotation `Rz(roll) * Rx(pitch) * Ry(yaw)`, rows first.
fn rotation(p: &Pose) -> [[f32; 3]; 3] {
    let (sy, cy) = (sin(p.yaw), cos(p.yaw));
    let (sp, cp) = (sin(p.pitch), cos(p.pitch));
    let (sr, cr) = (sin(p.roll), cos(p.roll));
    let a0 = [cy, 0.0, sy];
    let a1 = [-sp * sy, cp, sp * cy];
    let a2 = [-cp * sy, -sp, cp * cy];
    [
        [
            cr * a0[0] - sr * a1[0],
            cr * a0[1] - sr * a1[1],
            cr * a0[2] - sr * a1[2],
        ],
        [
            sr * a0[0] + cr * a1[0],
            sr * a0[1] + cr * a1[1],
            sr * a0[2] + cr * a1[2],
        ],
        a2,
    ]
}

/// Screen position of the head-space point `h` (head radii) for a head drawn
/// at `(cx, cy)` with radius `r`, and its depth toward the viewer in radii.
pub fn project(cx: i32, cy: i32, r: f32, pose: &Pose, h: [f32; 3]) -> (f32, f32, f32) {
    let m = rotation(pose);
    (
        cx as f32 + r * dot(m[0], h),
        cy as f32 + r * (dot(m[1], h) - pose.lift),
        dot(m[2], h),
    )
}

/// Colours and face of one frame, shared by every pixel.
struct Paint<'a> {
    pose: &'a Pose,
    eyes: Eyes,
    /// One over the eye's half-width, and over an open eye's half-height in
    /// half-widths (never thinner than a line the display can show).
    eye_scale: f32,
    lid_scale: f32,
    /// One over the height the other eye shapes are flattened to (1 when open).
    squash: f32,
    /// The accent, when any of it has faded in.
    glow: Option<u32>,
    /// Shade and light: ink and paper, or the reverse under the dark theme.
    lo: u32,
    hi: u32,
}

impl Paint<'_> {
    fn tone(&self, px: i32, py: i32, level: f32) -> u32 {
        if dither(px, py, level) {
            self.hi
        } else {
            self.lo
        }
    }
}

/// Draws the head centred on `(cx, cy)` with radius `r` pixels.
///
/// `dark` says the finished frame will have ink and paper swapped: the shading
/// is then drawn pre-swapped so the head is lit from the same side in both themes.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    canvas: &mut Canvas,
    cx: i32,
    cy: i32,
    r: f32,
    pose: &Pose,
    eyes: Eyes,
    accent: Accent,
    dark: bool,
) {
    let (lo, hi) = if dark { (PAPER, INK) } else { (INK, PAPER) };
    let paint = Paint {
        pose,
        eyes,
        eye_scale: 1.0 / (EYE_W * pose.wide),
        lid_scale: 1.0 / ((EYE_H / EYE_W) * pose.open).max(0.24),
        squash: 1.0 / pose.open.clamp(SHUT, 1.0),
        glow: accent.color().filter(|_| pose.glow > 0.0),
        lo,
        hi,
    };
    let m = rotation(pose);
    // Rows of the head-to-view map with the shell scaled to a unit sphere: a
    // view ray `(u, v, z)` is `u*ex + v*ey + z*d` in that space, and the same
    // three rows turn a unit-sphere point back into a view-space normal.
    let scale = |row: [f32; 3]| [row[0] / AXES[0], row[1] / AXES[1], row[2] / AXES[2]];
    let (ex, ey, d) = (scale(m[0]), scale(m[1]), scale(m[2]));
    let a = dot(d, d);
    let cyf = cy as f32 - pose.lift * r;
    // Pod centres in view space, the lamp axes turned the same way, and the
    // top-left pixel of each lens' highlight: the point of the lens a little
    // way round from its axis toward the light, so it slides as the head turns.
    let spot_px = ((r / 19.0).round() as i32).max(1);
    let pods = [-1.0f32, 1.0].map(|s| {
        let turn = |h: [f32; 3]| [dot(m[0], h), dot(m[1], h), dot(m[2], h)];
        let c = turn([s * POD[0], POD[1], POD[2]]);
        let l = turn([s * POD_LAMP[0], POD_LAMP[1], POD_LAMP[2]]);
        let along = dot(l, LIGHT);
        let h = [0, 1, 2].map(|i| l[i] + 0.5 * (LIGHT[i] - along * l[i]));
        let reach = r * POD_R / dot(h, h).sqrt();
        let corner =
            |centre: f32, h: f32| (centre + reach * h - 0.5 * spot_px as f32).round() as i32;
        let spot = (h[2] > 0.0).then(|| {
            (
                corner(cx as f32 + r * c[0], h[0]),
                corner(cyf + r * c[1], h[1]),
            )
        });
        (c, l, spot)
    });
    // A contour about 1.3px wide: the normal's z within this of edge-on.
    let edge = |radius: f32| (2.6 / radius.max(2.6)).sqrt();
    let (head_edge, pod_edge) = (edge(r), edge(POD_R * r));

    let (inv_r, inv_a) = (1.0 / r, 1.0 / a);
    let half_h = r as i32 + 2;
    for py in (cyf as i32 - half_h)..=(cyf as i32 + half_h) {
        let v = (py as f32 - cyf) * inv_r;
        // The row's span: the shell lies inside the unit disc (its longest
        // semi-axis is 1), each pod inside its own.
        let half = (1.0 - v * v).max(0.0).sqrt();
        let (mut left, mut right) = (-half, half);
        for (c, _, _) in pods {
            let half = (POD_R * POD_R - (v - c[1]) * (v - c[1])).max(0.0).sqrt();
            left = left.min(c[0] - half);
            right = right.max(c[0] + half);
        }
        for px in (cx + (left * r) as i32 - 1)..=(cx + (right * r) as i32 + 1) {
            let u = (px - cx) as f32 * inv_r;
            let mut depth = f32::MIN;
            let mut color = None;

            // Shell: nearest root of |o + z*d|^2 = 1 along the view ray.
            let o = [
                u * ex[0] + v * ey[0],
                u * ex[1] + v * ey[1],
                u * ex[2] + v * ey[2],
            ];
            let (b, oo) = (dot(o, d), dot(o, o));
            let disc = b * b - a * (oo - 1.0);
            if disc > 0.0 {
                let root = disc.sqrt();
                depth = (root - b) * inv_a;
                let p = [
                    o[0] + depth * d[0],
                    o[1] + depth * d[1],
                    o[2] + depth * d[2],
                ];
                let n = [dot(ex, p), dot(ey, p), root];
                let inv = 1.0 / dot(n, n).sqrt();
                let n = [n[0] * inv, n[1] * inv, n[2] * inv];
                color = Some(if n[2] < head_edge {
                    INK
                } else {
                    // The eye screen: the same ray against a smaller sphere.
                    let screen = || {
                        let disc = b * b - a * (oo - SCREEN * SCREEN);
                        let z = (disc.max(0.0).sqrt() - b) * inv_a;
                        let e = [o[0] + z * d[0], o[1] + z * d[1], o[2] + z * d[2]];
                        let s = 1.0 / SCREEN;
                        (disc > 0.0 && e[2] > 0.0).then_some([e[0] * s, e[1] * s])
                    };
                    paint.shell(px, py, p, n, screen)
                });
            }

            for (c, lamp, spot) in pods {
                let (du, dv) = (u - c[0], v - c[1]);
                let rr = POD_R * POD_R - du * du - dv * dv;
                if rr <= 0.0 || c[2] + rr.sqrt() <= depth {
                    continue;
                }
                depth = c[2] + rr.sqrt();
                let s = 1.0 / POD_R;
                let n = [du * s, dv * s, rr.sqrt() * s];
                let on_spot = |(sx, sy): (i32, i32)| {
                    (sx..sx + spot_px).contains(&px) && (sy..sy + spot_px).contains(&py)
                };
                let facing = dot(n, lamp);
                color = Some(if n[2] < pod_edge {
                    INK
                } else if facing > LENS {
                    // The lamp's lens: the accent when lit, glass shaded by
                    // the light when not, and one small highlight either way.
                    match paint.glow {
                        _ if spot.is_some_and(on_spot) => paint.hi,
                        Some(g) if dither(px, py, pose.glow) => g,
                        _ => paint.tone(px, py, 0.25 + 0.75 * dot(n, LIGHT)),
                    }
                } else if facing > LENS_RIM {
                    // The lens' seat: a dark ring, so an unlit lens is
                    // still a lens and not a patch of the rubber.
                    paint.lo
                } else {
                    // Rubber, a tone darker than the shell; never so dark
                    // that the pod reads as a hole.
                    paint.tone(px, py, (0.30 + 0.75 * dot(n, LIGHT)).max(0.26))
                });
            }

            if let Some(color) = color {
                canvas.set_pixel(px, py, color);
            }
        }
    }
}

impl Paint<'_> {
    /// Colour of the shell at unit-sphere point `p` with view normal `n`.
    /// `screen` finds the eye-screen point behind it, for glass pixels.
    fn shell(
        &self,
        px: i32,
        py: i32,
        p: [f32; 3],
        n: [f32; 3],
        screen: impl FnOnce() -> Option<[f32; 2]>,
    ) -> u32 {
        let lit = dot(n, LIGHT);
        if p[2] > 0.0 {
            let vx = p[0] * (1.0 / VISOR_W);
            let vy = (p[1] - VISOR_Y + VISOR_BOW * p[0] * p[0]) * (1.0 / VISOR_H);
            let (x4, y4) = (vx * vx * vx * vx, vy * vy * vy * vy);
            let f = x4 + y4;
            if f < 1.0 {
                if let Some(color) = screen().and_then(|e| self.eye(px, py, e)) {
                    return color;
                }
                // Dark glass. The light catches a band just inside the rim,
                // on whatever stretch of it faces the lamp: the glint runs
                // along the rim as the head turns. A short second streak
                // follows it along the top edge after a gap.
                let glint = (0.42..0.66).contains(&f)
                    && (lit > GLINT || (GLINT_2.contains(&lit) && vy < 0.0 && y4 > x4));
                return if glint { self.hi } else { self.lo };
            }
            if f < BEZEL {
                return self.hi;
            }
        }
        // Plastic: white through the lit half, falling to a fifth in shade,
        // with a little light bounced back onto the far rim.
        let bounce = (-lit - 0.55).max(0.0) * 0.5;
        self.tone(px, py, (0.56 + 1.25 * lit).clamp(0.18, 1.0) + bounce)
    }

    /// Colour of the eye-screen point `e` (unit coordinates), if an eye or its
    /// halo covers it.
    fn eye(&self, px: i32, py: i32, e: [f32; 2]) -> Option<u32> {
        let pose = self.pose;
        let side = if e[0] < pose.look_x { -1.0 } else { 1.0 };
        // Offsets from this eye's centre, in eye half-widths.
        let x = (e[0] - side * EYE_X - pose.look_x) * self.eye_scale;
        let y = (e[1] - EYE_Y - pose.look_y) * self.eye_scale;
        if x.abs() > 2.0 || y.abs() > 2.6 {
            return None;
        }
        // 0 at the middle of the shape, 1 on its edge, more outside. The
        // open bars shut by their lid; the other shapes flatten onto the
        // eye's centre line (`ys` is `y` with the shape stretched back out).
        let yl = y * self.lid_scale;
        let lid = (x * x) * (x * x) + (yl * yl) * (yl * yl);
        let ys = y * self.squash;
        let m = match self.eyes {
            Eyes::Open => lid,
            Eyes::Asleep => {
                // The bottom of a circle centred above the eye: a 2px arc
                // 1.8 half-widths across that sags half of one.
                let dist = (x * x + (ys + 0.62) * (ys + 0.62)).sqrt();
                let band = (dist - 1.0) / 0.20;
                band * band + if x.abs() > 0.9 || ys < -0.3 { 9.0 } else { 0.0 }
            }
            Eyes::Happy => {
                // The top of a circle centred below the eye: an upturned arc.
                let dist = (x * x + (ys - 0.8) * (ys - 0.8)).sqrt();
                let band = (dist - 1.25).abs() / 0.40;
                band * band + if ys > 0.45 { 9.0 } else { 0.0 }
            }
            Eyes::Cross => {
                let bar = (x - ys).abs().min((x + ys).abs()) / 0.36;
                bar * bar
                    + if x.abs().max(ys.abs()) > 1.15 {
                        9.0
                    } else {
                        0.0
                    }
            }
            Eyes::Prompt { cursor } => {
                if side < 0.0 {
                    // `>`: two strokes meeting at the right.
                    let bar = (x - 0.75 + ys.abs()).abs() / 0.40;
                    bar * bar + if ys.abs() > 1.25 { 9.0 } else { 0.0 }
                } else if x.abs() < 0.95 && (if cursor { 0.55 } else { 1.02 }..1.25).contains(&ys) {
                    0.0
                } else {
                    9.0
                }
            }
        };
        // Nearly shut, every shape is the line a shut eye is: one shape gives
        // way to the next behind what looks like a blink.
        let m = if pose.open < NEARLY_SHUT {
            m.min(lid)
        } else {
            m
        };
        match self.glow {
            _ if m < 0.42 => Some(self.hi),
            // The rim takes the accent as it fades in.
            Some(g) if m < 1.0 && dither(px, py, pose.glow) => Some(g),
            _ if m < 1.0 => Some(self.hi),
            // ... and spills a halo of it onto the glass.
            Some(g) if m < 2.6 && dither(px, py, 0.5 * pose.glow) => Some(g),
            // Without an accent the eyes still glow a little through the
            // glass, unless they are shut for the night.
            _ if m < 2.6 && self.eyes != Eyes::Asleep && dither(px, py, 0.25) => Some(self.hi),
            _ => None,
        }
    }
}

/// Soft dithered contact shadow on the ground under the head: an ellipse of
/// half-size `(rx, ry)` pixels, `strength` coverage at its centre. Drawn in
/// canvas ink, so the dark theme turns it into a pool of light.
pub fn shadow(canvas: &mut Canvas, cx: i32, cy: i32, rx: f32, ry: f32, strength: f32) {
    if rx < 1.0 || ry < 1.0 {
        return;
    }
    for py in (cy - ry as i32)..=(cy + ry as i32) {
        let dy = (py - cy) as f32 / ry;
        for px in (cx - rx as i32)..=(cx + rx as i32) {
            let dx = (px - cx) as f32 / rx;
            let fall = 1.0 - dx * dx - dy * dy;
            if fall > 0.0 && dither(px, py, strength * fall) {
                canvas.set_pixel(px, py, INK);
            }
        }
    }
}

/// A small lit ball of `color` (hands, the orbiting moon), contoured in the
/// shade colour so it stays crisp over the shell in either theme.
pub fn ball(canvas: &mut Canvas, cx: f32, cy: f32, r: f32, color: u32, dark: bool) {
    if r < 0.5 {
        return;
    }
    let (lo, hi) = if dark { (PAPER, INK) } else { (INK, PAPER) };
    let edge = 1.0 - 1.3 / r.max(1.3);
    let span = r as i32 + 1;
    let (ix, iy) = (cx.round() as i32, cy.round() as i32);
    for py in (iy - span)..=(iy + span) {
        let dy = (py as f32 - cy) / r;
        for px in (ix - span)..=(ix + span) {
            let dx = (px as f32 - cx) / r;
            let d2 = dx * dx + dy * dy;
            if d2 >= 1.0 {
                continue;
            }
            let lit = dx * LIGHT[0] + dy * LIGHT[1] + (1.0 - d2).sqrt() * LIGHT[2];
            let c = if r >= 3.0 && d2 > edge * edge {
                lo
            } else if dither(px, py, (lit - 0.62) * 2.2) {
                hi
            } else if dither(px, py, (0.12 - lit) * 1.6) {
                lo
            } else {
                color
            };
            canvas.set_pixel(px, py, c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_tracks_the_library_and_is_periodic() {
        for i in -2000..2000 {
            let x = i as f32 * 0.01;
            assert!((sin(x) - x.sin()).abs() < 2e-5, "sin({x})");
            assert!((cos(x) - x.cos()).abs() < 2e-5, "cos({x})");
        }
        assert_eq!(sin(0.0), 0.0);
    }

    #[test]
    fn dither_extremes_are_solid() {
        for y in 0..4 {
            for x in 0..4 {
                assert!(!dither(x, y, 0.0));
                assert!(dither(x, y, 1.0));
            }
        }
        let half = (0..4)
            .flat_map(|y| (0..4).map(move |x| dither(x, y, 0.5)))
            .filter(|on| *on)
            .count();
        assert_eq!(half, 8);
    }

    #[test]
    fn rotation_turns_the_face_the_documented_way() {
        let face = |pose: &Pose| {
            let (x, y, z) = project(0, 0, 1.0, pose, [0.0, 0.0, 1.0]);
            [x, y, z]
        };
        let yaw = face(&Pose {
            yaw: 0.5,
            ..Pose::REST
        });
        assert!(yaw[0] > 0.4 && yaw[1].abs() < 1e-6, "yaw turns right");
        let pitch = face(&Pose {
            pitch: 0.5,
            ..Pose::REST
        });
        assert!(pitch[1] > 0.4 && pitch[0].abs() < 1e-6, "pitch nods down");
        // Roll leaves the face where it is and swings the crown clockwise.
        let roll = Pose {
            roll: 0.5,
            ..Pose::REST
        };
        assert!(face(&roll)[2] > 0.999);
        assert!(project(0, 0, 1.0, &roll, [0.0, -1.0, 0.0]).0 > 0.4);
    }
}
