//! The HUD's building blocks: glowing panels with chamfered headers, big navigation rows,
//! gamepad-button hint chips, buttons, toggles and status chips.

use crate::theme::{self, CYAN, DIM, FAINT, LINE, RAISED, RED, TEXT};
use egui::{
    Align2, Color32, CornerRadius, Pos2, Rect, Response, RichText, Sense, Shape, Stroke,
    StrokeKind, Ui, Vec2,
};

/// A rectangle with the top-left and bottom-right corners cut (the styleguide's title box).
pub fn chamfer(rect: Rect, cut: f32) -> Vec<Pos2> {
    let c = cut.min(rect.height() / 2.0).min(rect.width() / 2.0);
    vec![
        Pos2::new(rect.min.x + c, rect.min.y),
        Pos2::new(rect.max.x, rect.min.y),
        Pos2::new(rect.max.x, rect.max.y - c),
        Pos2::new(rect.max.x - c, rect.max.y),
        Pos2::new(rect.min.x, rect.max.y),
        Pos2::new(rect.min.x, rect.min.y + c),
    ]
}

/// A soft glow: a few widening, fading outlines.
pub fn glow(painter: &egui::Painter, rect: Rect, color: Color32, strength: f32) {
    for (i, a) in [0.20_f32, 0.10, 0.05].iter().enumerate() {
        let grow = 2.0 + i as f32 * 3.0;
        painter.rect_stroke(
            rect.expand(grow),
            CornerRadius::same(theme::RADIUS + 2),
            Stroke::new(3.0_f32, color.gamma_multiply(a * strength)),
            StrokeKind::Outside,
        );
    }
}

/// A panel with a chamfered title tab; `focused` draws the cyan 2 px border and glow.
pub fn panel<R>(ui: &mut Ui, title: &str, focused: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    let border = if focused { CYAN } else { LINE };
    let inner = egui::Frame::new()
        .fill(theme::PANEL)
        .stroke(Stroke::new(if focused { 2.0_f32 } else { 1.0_f32 }, border))
        .corner_radius(CornerRadius::same(theme::RADIUS))
        .inner_margin(egui::Margin {
            left: 10,
            right: 10,
            top: 34,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        });
    let rect = inner.response.rect;
    let painter = ui.painter();
    if focused {
        glow(painter, rect, CYAN, 1.0);
    }
    let font = theme::mono(14.0);
    let galley = ui.fonts(|f| {
        f.layout_no_wrap(
            title.to_uppercase(),
            font,
            if focused { Color32::BLACK } else { CYAN },
        )
    });
    let tab = Rect::from_min_size(
        rect.min + Vec2::new(10.0, 6.0),
        Vec2::new(galley.size().x + 24.0, 22.0),
    );
    painter.add(Shape::convex_polygon(
        chamfer(tab, 6.0),
        if focused { CYAN } else { RAISED },
        Stroke::new(1.0_f32, if focused { CYAN } else { LINE }),
    ));
    painter.galley(
        tab.min + Vec2::new(12.0, (22.0 - galley.size().y) / 2.0),
        galley,
        TEXT,
    );
    inner.inner
}

/// One big sidebar row: icon, label, optional sub-label and badge.
pub fn nav_row(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    sub: &str,
    selected: bool,
    focused: bool,
    badge: Option<&str>,
) -> Response {
    let h = if sub.is_empty() { 40.0 } else { 52.0 };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click());
    let p = ui.painter();
    if selected {
        let fill = if focused {
            CYAN.gamma_multiply(0.16)
        } else {
            RAISED
        };
        p.rect_filled(rect, CornerRadius::same(theme::RADIUS), fill);
        p.rect_filled(
            Rect::from_min_size(rect.min, Vec2::new(4.0, h)),
            CornerRadius::same(2),
            if focused { CYAN } else { DIM },
        );
        if focused {
            p.rect_stroke(
                rect,
                CornerRadius::same(theme::RADIUS),
                Stroke::new(2.0_f32, CYAN),
                StrokeKind::Inside,
            );
            glow(p, rect, CYAN, 0.6);
        }
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(theme::RADIUS), RAISED);
    }
    let color = if selected { TEXT } else { DIM };
    p.text(
        rect.min + Vec2::new(16.0, if sub.is_empty() { h / 2.0 } else { 17.0 }),
        Align2::LEFT_CENTER,
        icon,
        theme::sans(19.0),
        if selected && focused { CYAN } else { color },
    );
    p.text(
        rect.min + Vec2::new(46.0, if sub.is_empty() { h / 2.0 } else { 17.0 }),
        Align2::LEFT_CENTER,
        label,
        theme::mono(16.0),
        color,
    );
    if !sub.is_empty() {
        let fits = ((rect.width() - 56.0) / 6.6).max(8.0) as usize;
        let shown: String = if sub.chars().count() > fits {
            sub.chars()
                .take(fits - 1)
                .chain(std::iter::once('…'))
                .collect()
        } else {
            sub.to_owned()
        };
        p.text(
            rect.min + Vec2::new(46.0, 37.0),
            Align2::LEFT_CENTER,
            shown,
            theme::sans(13.0),
            FAINT,
        );
    }
    if let Some(b) = badge {
        let r = Rect::from_center_size(
            Pos2::new(rect.max.x - 22.0, rect.center().y),
            Vec2::new(26.0, 20.0),
        );
        p.rect_filled(r, CornerRadius::same(10), RED);
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            b,
            theme::mono(12.0),
            Color32::WHITE,
        );
    }
    resp
}

/// A button's face: a round chip for A/B/X/Y, a key cap for the rest.
pub fn key_cap(ui: &mut Ui, glyph: &str) {
    let round = matches!(glyph, "A" | "B" | "X" | "Y" | "?");
    let font = theme::mono(13.0);
    let g = ui.fonts(|f| f.layout_no_wrap(glyph.to_owned(), font.clone(), Color32::BLACK));
    let w = if round { 24.0 } else { g.size().x + 14.0 };
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 24.0), Sense::hover());
    let p = ui.painter();
    let color = match glyph {
        "A" => CYAN,
        "B" => RED,
        _ => DIM,
    };
    if round {
        p.circle_filled(rect.center(), 12.0, color);
    } else {
        p.rect_filled(rect, CornerRadius::same(5), color);
    }
    p.galley(rect.center() - g.size() / 2.0, g, Color32::BLACK);
}

/// What a control does now: `glyph` is A, B, X, Y, ▲▼, ◀▶, SEL, START or a key name.
pub fn hint(ui: &mut Ui, glyph: &str, label: &str) {
    key_cap(ui, glyph);
    ui.label(RichText::new(label).font(theme::sans(15.0)).color(TEXT));
    ui.add_space(10.0);
}

/// A top-bar workspace tab: icon, name, its Alt+number, and a red badge for waiting items.
pub fn tab(ui: &mut Ui, icon: &str, name: &str, number: usize, on: bool, badge: usize) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(88.0, 42.0), Sense::click());
    let p = ui.painter();
    if on {
        p.add(Shape::convex_polygon(
            chamfer(rect, 8.0),
            CYAN,
            Stroke::NONE,
        ));
        glow(p, rect, CYAN, 0.8);
    } else {
        let stroke = if resp.hovered() { CYAN } else { LINE };
        p.add(Shape::convex_polygon(
            chamfer(rect, 8.0),
            RAISED,
            Stroke::new(1.0_f32, stroke),
        ));
    }
    let fg = if on { Color32::BLACK } else { DIM };
    p.text(
        rect.min + Vec2::new(10.0, 21.0),
        Align2::LEFT_CENTER,
        icon,
        theme::sans(17.0),
        fg,
    );
    p.text(
        rect.min + Vec2::new(32.0, 21.0),
        Align2::LEFT_CENTER,
        name,
        theme::mono(14.0),
        if on { Color32::BLACK } else { TEXT },
    );
    p.text(
        rect.max - Vec2::new(6.0, 5.0),
        Align2::RIGHT_BOTTOM,
        number.to_string(),
        theme::mono(10.0),
        if on {
            Color32::from_black_alpha(160)
        } else {
            FAINT
        },
    );
    if badge > 0 {
        let c = Pos2::new(rect.max.x - 6.0, rect.min.y + 4.0);
        p.circle_filled(c, 9.0, RED);
        p.text(
            c,
            Align2::CENTER_CENTER,
            badge.min(9).to_string(),
            theme::mono(11.0),
            Color32::WHITE,
        );
    }
    resp
}

/// A full-width primary button; `selected` marks the one the D-pad is on.
pub fn button(ui: &mut Ui, label: &str, selected: bool, danger: bool) -> Response {
    let accent = if danger { RED } else { CYAN };
    let text = RichText::new(label)
        .font(theme::mono(16.0))
        .color(if selected { Color32::BLACK } else { accent });
    let b = egui::Button::new(text)
        .fill(if selected {
            accent
        } else {
            Color32::TRANSPARENT
        })
        .stroke(Stroke::new(2.0_f32, accent))
        .corner_radius(CornerRadius::same(theme::RADIUS))
        .min_size(Vec2::new(ui.available_width().min(360.0), 38.0));
    let r = ui.add(b);
    if selected {
        glow(ui.painter(), r.rect, accent, 0.8);
    }
    r
}

/// An ON/OFF pill.
pub fn toggle(ui: &mut Ui, on: Option<bool>, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(64.0, 30.0), Sense::click());
    let p = ui.painter();
    let c = match on {
        Some(true) => CYAN,
        Some(false) => FAINT,
        None => theme::AMBER,
    };
    p.rect_stroke(
        rect,
        CornerRadius::same(15),
        Stroke::new(2.0_f32, c),
        StrokeKind::Inside,
    );
    if selected {
        glow(p, rect, CYAN, 0.8);
    }
    let knob_x = if on == Some(true) {
        rect.max.x - 15.0
    } else {
        rect.min.x + 15.0
    };
    p.circle_filled(Pos2::new(knob_x, rect.center().y), 9.0, c);
    let label = match on {
        Some(true) => "ON",
        Some(false) => "OFF",
        None => "?",
    };
    let lx = if on == Some(true) {
        rect.min.x + 18.0
    } else {
        rect.max.x - 20.0
    };
    p.text(
        Pos2::new(lx, rect.center().y),
        Align2::CENTER_CENTER,
        label,
        theme::mono(11.0),
        c,
    );
    resp
}

/// A small status chip.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) {
    let font = theme::mono(13.0);
    let g = ui.fonts(|f| f.layout_no_wrap(text.to_owned(), font, color));
    let (rect, _) = ui.allocate_exact_size(g.size() + Vec2::new(14.0, 8.0), Sense::hover());
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(theme::RADIUS),
        Stroke::new(1.0_f32, color.gamma_multiply(0.7)),
        StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.min + Vec2::new(7.0, 4.0), g, color);
}

/// A label + value row.
pub fn field(ui: &mut Ui, label: &str, value: impl Into<String>) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [140.0, 22.0],
            egui::Label::new(RichText::new(label).font(theme::mono(14.0)).color(DIM)),
        );
        ui.label(
            RichText::new(value.into())
                .font(theme::sans(16.0))
                .color(TEXT),
        );
    });
}

/// A selectable list row for content panes (bigger than egui's).
pub fn list_row(ui: &mut Ui, text: &str, selected: bool, color: Color32) -> Response {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(
            rect,
            CornerRadius::same(theme::RADIUS),
            CYAN.gamma_multiply(0.14),
        );
        p.rect_stroke(
            rect,
            CornerRadius::same(theme::RADIUS),
            Stroke::new(2.0_f32, CYAN),
            StrokeKind::Inside,
        );
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(theme::RADIUS), RAISED);
    }
    p.text(
        rect.min + Vec2::new(10.0, 16.0),
        Align2::LEFT_CENTER,
        text,
        theme::mono(15.0),
        if selected { TEXT } else { color },
    );
    if selected {
        resp.scroll_to_me(None);
    }
    resp
}

/// A muted explanatory line.
pub fn note(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::sans(14.0)).color(FAINT));
}

pub fn danger_note(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::sans(14.0)).color(RED));
}

/// Where the D-pad is in a [`form`].
#[derive(Debug, Default, Clone, Copy)]
pub struct Form {
    pub row: usize,
    pub editing: bool,
}

pub enum Field<'a> {
    Text {
        label: &'a str,
        value: &'a mut String,
        hint: &'a str,
        secret: bool,
    },
    Choice {
        label: &'a str,
        options: &'a [&'a str],
        index: &'a mut usize,
    },
    Action {
        label: &'a str,
        danger: bool,
    },
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FormOut {
    /// An Action row was activated.
    pub activated: Option<usize>,
    /// A Choice row changed.
    pub changed: Option<usize>,
    /// The form used B (left a field) or ◀ (a choice): the HUD must not go back.
    pub grab_b: bool,
    pub grab_left: bool,
}

/// A form for the D-pad: ▲▼ rows, A edits a text row or runs an action, ◀▶ change a choice,
/// Enter / A / ▲▼ leave a text field, B / Esc leave it without moving.
pub fn form(
    ui: &mut Ui,
    id: &str,
    st: &mut Form,
    fields: &mut [Field],
    nav: crate::app::Nav,
    focused: bool,
) -> FormOut {
    let mut out = FormOut::default();
    let n = fields.len();
    if n == 0 {
        return out;
    }
    st.row = st.row.min(n - 1);
    let (k_up, k_down, k_enter) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::Enter),
        )
    });
    if focused && !st.editing {
        crate::app::step(&mut st.row, n, nav);
        if let Field::Choice { index, options, .. } = &mut fields[st.row] {
            out.grab_left = true;
            if nav.left || nav.right {
                let len = options.len().max(1);
                **index = if nav.right {
                    (**index + 1) % len
                } else {
                    (**index + len - 1) % len
                };
                out.changed = Some(st.row);
            }
        }
    }
    let mut start_edit = false;
    if focused && !st.editing && nav.a {
        match &fields[st.row] {
            Field::Text { .. } => start_edit = true,
            Field::Action { .. } => out.activated = Some(st.row),
            Field::Choice { .. } => {}
        }
    }
    for (i, f) in fields.iter_mut().enumerate() {
        let on = focused && i == st.row;
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 42.0), Sense::hover());
        let p = ui.painter().clone();
        if on {
            p.rect_filled(
                rect,
                CornerRadius::same(theme::RADIUS),
                CYAN.gamma_multiply(0.10),
            );
            p.rect_stroke(
                rect,
                CornerRadius::same(theme::RADIUS),
                Stroke::new(2.0_f32, CYAN),
                StrokeKind::Inside,
            );
        }
        let label_pos = rect.min + Vec2::new(12.0, 21.0);
        let left = if matches!(f, Field::Action { .. }) {
            12.0
        } else {
            190.0
        };
        let field_rect = Rect::from_min_max(
            rect.min + Vec2::new(left, 5.0),
            rect.max - Vec2::new(8.0, 5.0),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(field_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        match f {
            Field::Text {
                label,
                value,
                hint,
                secret,
            } => {
                p.text(
                    label_pos,
                    Align2::LEFT_CENTER,
                    *label,
                    theme::mono(15.0),
                    if on { CYAN } else { DIM },
                );
                let r = child.add(
                    egui::TextEdit::singleline(*value)
                        .hint_text(RichText::new(*hint).color(FAINT))
                        .password(*secret)
                        .font(theme::mono(16.0))
                        .desired_width(field_rect.width())
                        .id_salt((id, i)),
                );
                if start_edit && on {
                    r.request_focus();
                    st.editing = true;
                }
                if r.gained_focus() || r.has_focus() {
                    st.row = i;
                    st.editing = true;
                }
                // The A that started editing is not also the one that ends it.
                if st.editing
                    && st.row == i
                    && r.has_focus()
                    && !start_edit
                    && (k_up || k_down || nav.a || nav.b)
                {
                    r.surrender_focus();
                    st.editing = false;
                    out.grab_b |= nav.b;
                    if k_up {
                        st.row = st.row.saturating_sub(1);
                    } else if k_down || nav.a {
                        st.row = (st.row + 1).min(n - 1);
                    }
                } else if st.editing && st.row == i && r.lost_focus() {
                    st.editing = false;
                    out.grab_b = true; // Esc lands here too: one press leaves the field only
                    if k_enter {
                        st.row = (st.row + 1).min(n - 1);
                    }
                }
            }
            Field::Choice {
                label,
                options,
                index,
            } => {
                p.text(
                    label_pos,
                    Align2::LEFT_CENTER,
                    *label,
                    theme::mono(15.0),
                    if on { CYAN } else { DIM },
                );
                let shown = options.get(**index).copied().unwrap_or("");
                child.label(
                    RichText::new(format!("◀  {shown}  ▶"))
                        .font(theme::mono(16.0))
                        .color(if on { TEXT } else { DIM }),
                );
            }
            Field::Action { label, danger } => {
                let accent = if *danger { RED } else { CYAN };
                let b =
                    egui::Button::new(RichText::new(*label).font(theme::mono(16.0)).color(if on {
                        Color32::BLACK
                    } else {
                        accent
                    }))
                    .fill(if on { accent } else { Color32::TRANSPARENT })
                    .stroke(Stroke::new(2.0_f32, accent))
                    .min_size(Vec2::new(220.0, 32.0));
                if child.add(b).clicked() {
                    out.activated = Some(i);
                }
            }
        }
    }
    out
}
