//! [6] PTY/MUX — terminal tabs and a split view; shells, SSH sessions and UART consoles.
//!
//! Sidebar: the open terminals, "New shell", "Serial console". Content: the terminal (A gives
//! it the keyboard, pad B takes it back), or the serial port picker.

use crate::app::{pane, step, Action, DeckApp, Nav, PTY};
use crate::theme::{AMBER, LINE, TEXT};
use crate::widgets;
use crate::ws::Section;
use egui::{RichText, Ui};
use std::sync::atomic::Ordering;

pub const BAUDS: [u32; 6] = [115_200, 9_600, 19_200, 38_400, 57_600, 230_400];

#[derive(Default)]
pub struct State {
    seen: usize,
    ports: Vec<String>,
    port: usize,
    baud: usize,
}

pub fn sections(app: &DeckApp) -> Vec<Section> {
    let mut v: Vec<Section> = app
        .terms
        .list
        .iter()
        .map(|t| {
            let alive = t.alive.load(Ordering::Relaxed);
            Section::new(if alive { "▣" } else { "⚠" }, t.title.clone()).detail(if alive {
                "running"
            } else {
                "exited"
            })
        })
        .collect();
    v.push(Section::new("+", "New shell").detail("bash as lapis"));
    v.push(Section::new("🔌", "Serial console").detail("USB / UART"));
    v
}

/// A new terminal (from here, FLEET's SSH or the palette) is selected when it opens.
pub fn sync(app: &mut DeckApp, moved: bool) {
    let n = app.terms.list.len();
    if n != app.pty.seen {
        app.pty.seen = n;
        if n > 0 {
            app.section[PTY] = app.terms.active.min(n - 1);
        }
    } else if moved && app.section[PTY] < n {
        app.terms.active = app.section[PTY];
    }
}

pub fn close_section(app: &mut DeckApp, section: usize) {
    if section < app.terms.list.len() {
        app.terms.active = section;
        app.terms.close_active();
    }
}

fn serial_ports() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir("/dev")
        .map(|d| {
            d.filter_map(Result::ok)
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| {
                    ["ttyUSB", "ttyACM", "ttyAMA", "serial"]
                        .iter()
                        .any(|p| n.starts_with(p))
                })
                .map(|n| format!("/dev/{n}"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn show(app: &mut DeckApp, ui: &mut Ui, nav: Nav, section: usize, focused: bool) {
    let n = app.terms.list.len();
    if section == n {
        if focused && (nav.a || app.entered) {
            app.run(Action::NewShell);
        }
        pane(ui, "NEW SHELL", focused, |ui| {
            widgets::note(ui, "A opens a shell on the deck. FLEET opens SSH to a host; the serial console is the next row.")
        });
        return;
    }
    if section > n {
        serial(app, ui, nav, focused);
        return;
    }
    app.terms.active = section;
    app.terms.focused = focused;
    if focused {
        app.hint("⌨", "keys go to the terminal");
        app.hint("B", "pad B: back to the list");
    }
    if let Some(e) = &app.terms.error {
        ui.label(RichText::new(e).color(AMBER));
    }
    app.terms.input(ui.ctx());
    let rect = ui.available_rect_before_wrap();
    let active = app.terms.active.min(n - 1);
    if app.terms.split && n > 1 {
        let other = if active == 0 { 1 } else { 0 };
        let (l, r) = rect.split_left_right_at_fraction(0.5);
        app.terms.list[active].draw(ui, l.shrink(2.0), focused);
        app.terms.list[other].draw(ui, r.shrink(2.0), false);
        ui.painter()
            .vline(l.max.x, rect.y_range(), egui::Stroke::new(1.0_f32, LINE));
    } else {
        app.terms.list[active].draw(ui, rect.shrink(2.0), focused);
    }
}

fn serial(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    if app.pty.ports.is_empty() || app.frames.is_multiple_of(60) {
        app.pty.ports = serial_ports();
    }
    let ports = app.pty.ports.clone();
    let rows = ports.len() + 1;
    if focused {
        step(&mut app.pty.port, rows, nav);
        app.grab_left = true;
        if nav.left || nav.right {
            app.pty.baud =
                (app.pty.baud + if nav.right { 1 } else { BAUDS.len() - 1 }) % BAUDS.len();
        }
        app.hint("▲▼", "port");
        app.hint("◀▶", "baud");
        app.hint("A", "open");
    }
    let sel = app.pty.port.min(rows - 1);
    let baud = BAUDS[app.pty.baud];
    let mut open = None;
    pane(
        ui,
        &format!("SERIAL CONSOLE · ◀ {baud} baud ▶"),
        focused,
        |ui| {
            for (i, p) in ports.iter().enumerate() {
                if widgets::list_row(ui, p, focused && i == sel, TEXT).clicked()
                    || (focused && nav.a && i == sel)
                {
                    open = Some(p.clone());
                }
            }
            let other = "/dev/ttyUSB0 (when it appears)";
            if widgets::list_row(ui, other, focused && sel == ports.len(), TEXT).clicked()
                || (focused && nav.a && sel == ports.len())
            {
                open = Some("/dev/ttyUSB0".into());
            }
            if ports.is_empty() {
                widgets::note(
                    ui,
                    "No serial ports right now: plug a USB-serial adapter in.",
                );
            }
        },
    );
    if let Some(dev) = open {
        app.run(Action::Serial(dev, baud));
    }
}
