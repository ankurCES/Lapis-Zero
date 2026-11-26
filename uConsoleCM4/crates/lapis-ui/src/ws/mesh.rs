//! [3] MESH/GPS — Meshtastic nodes (SNR/RSSI/hops, distance), channel chat, traceroute;
//! gpsd fix, PPS clock and a satellite skyplot.

use crate::app::{kv, painter_text, pane, step, DeckApp, Nav, MESH};
use crate::model::Model;
use crate::theme;
use crate::theme::{AMBER, CYAN, DIM, GREEN, LINE, TEXT};
use crate::widgets;
use crate::ws::Section;
use egui::{Pos2, RichText, Sense, Stroke, Ui, Vec2};
use serde_json::{json, Value};

#[derive(Default)]
pub struct State {
    pub input: String,
    pub channel: u32,
}

/// Great-circle distance (km) and initial bearing (deg).
pub fn distance(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let (la1, lo1, la2, lo2) = (
        a.0.to_radians(),
        a.1.to_radians(),
        b.0.to_radians(),
        b.1.to_radians(),
    );
    let (dla, dlo) = (la2 - la1, lo2 - lo1);
    let h = (dla / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlo / 2.0).sin().powi(2);
    let km = 2.0 * 6371.0 * h.sqrt().asin();
    let y = dlo.sin() * la2.cos();
    let x = la1.cos() * la2.sin() - la1.sin() * la2.cos() * dlo.cos();
    (km, (y.atan2(x).to_degrees() + 360.0) % 360.0)
}

pub fn sections(app: &DeckApp) -> Vec<Section> {
    let nodes = app.m.mesh["nodes"].as_array().map(Vec::len).unwrap_or(0);
    let msgs = app.m.mesh["messages"].as_array().map(Vec::len).unwrap_or(0);
    let f = &app.m.gps["fix"];
    vec![
        Section::new("🖧", "Nodes").detail(format!(
            "{nodes} heard · {}",
            app.m.mesh["status"].as_str().unwrap_or("no data")
        )),
        Section::new("✉", "Messages").detail(format!("channel {} · {msgs}", app.mesh.channel)),
        Section::new("🗺", "GPS").detail(format!(
            "fix {} · {} sats",
            f["fix"].as_str().unwrap_or("none"),
            app.m.gps["used"].as_u64().unwrap_or(0)
        )),
    ]
}

pub fn show(app: &mut DeckApp, ui: &mut Ui, nav: Nav, section: usize, focused: bool) {
    match section {
        0 => nodes(app, ui, nav, focused),
        1 => messages(app, ui, nav, focused),
        _ => gps(app, ui),
    }
}

fn nodes(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    let nodes: Vec<Value> = app.m.mesh["nodes"].as_array().cloned().unwrap_or_default();
    let me = app.m.gps["fix"]["lat"]
        .as_f64()
        .zip(app.m.gps["fix"]["lon"].as_f64());
    if focused {
        step(&mut app.sel[MESH][0], nodes.len(), nav);
        app.hint("▲▼", "node");
        app.hint("X", "traceroute");
        if nav.x {
            if let Some(n) = nodes.get(app.sel[MESH][0]) {
                let id = Model::str_of(n, "id").to_owned();
                app.invoke("mesh.traceroute", json!({"node_id": id}));
            }
        }
    }
    let sel = app.sel[MESH][0];
    let status = app.m.mesh["status"]
        .as_str()
        .unwrap_or("no data yet")
        .to_owned();
    let region = app.m.mesh["region"].as_str().unwrap_or("").to_owned();
    let region_tag = match region.as_str() {
        "" => String::new(),
        "UNSET" => " · region UNSET, no TX".into(),
        r => format!(" · {r}"),
    };
    pane(
        ui,
        &format!("MESH NODES · meshtasticd {status}{region_tag}"),
        focused,
        |ui| {
            ui.label(
                RichText::new(format!(
                    " {:<11} {:<16} {:>6} {:>5} {:>4} {:>12}",
                    "ID", "NAME", "SNR", "RSSI", "HOP", "DIST"
                ))
                .font(theme::mono(14.0))
                .color(DIM),
            );
            egui::ScrollArea::vertical()
                .id_salt("nodes")
                .max_height(ui.available_height() - 40.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (i, n) in nodes.iter().enumerate() {
                        let dist = me
                            .zip(n["lat"].as_f64().zip(n["lon"].as_f64()))
                            .map(|(a, b)| distance(a, b));
                        let line = format!(
                            "{:<11} {:<16} {:>6} {:>5} {:>4} {:>12}",
                            Model::str_of(n, "id"),
                            Model::str_of(n, "long_name")
                                .chars()
                                .take(16)
                                .collect::<String>(),
                            n["snr"]
                                .as_f64()
                                .map(|s| format!("{s:.1}"))
                                .unwrap_or("-".into()),
                            n["rssi"]
                                .as_i64()
                                .map(|s| s.to_string())
                                .unwrap_or("-".into()),
                            n["hops"]
                                .as_u64()
                                .map(|s| s.to_string())
                                .unwrap_or("-".into()),
                            dist.map(|(km, b)| format!("{km:.1}km {b:.0}°"))
                                .unwrap_or("-".into()),
                        );
                        widgets::list_row(ui, &line, focused && i == sel, TEXT);
                    }
                    if region == "UNSET" {
                        widgets::note(
                            ui,
                            "LoRa region not set: the radio will not transmit. Set mesh_region in /etc/lapis/deck.toml.",
                        );
                    } else if nodes.is_empty() {
                        widgets::note(
                            ui,
                            "No nodes heard yet (is the LoRa rail on and meshtasticd running?).",
                        );
                    }
                });
            if let Some(tr) = app
                .m
                .tool_results
                .get("mesh.traceroute")
                .filter(|t| t["ok"] == true)
            {
                let route: Vec<&str> = tr["result"]["route"]
                    .as_array()
                    .map(|r| r.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                ui.label(
                    RichText::new(format!(
                        "traceroute → {}: deck → {} → dest",
                        Model::str_of(&tr["result"], "to"),
                        if route.is_empty() {
                            "direct".to_owned()
                        } else {
                            route.join(" → ")
                        }
                    ))
                    .font(theme::mono(14.0))
                    .color(AMBER),
                );
            }
        },
    );
}

fn messages(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    let nodes: Vec<Value> = app.m.mesh["nodes"].as_array().cloned().unwrap_or_default();
    if focused {
        app.hint("⌨", "type, Enter transmits (asks Y/N)");
        app.hint("X", "next channel");
        if nav.x {
            app.mesh.channel = (app.mesh.channel + 1) % 8;
        }
    }
    pane(
        ui,
        &format!("CHANNEL {}", app.mesh.channel),
        focused,
        |ui| {
            let msgs: Vec<Value> = app.m.mesh["messages"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            egui::ScrollArea::vertical()
                .id_salt("meshmsgs")
                .stick_to_bottom(true)
                .max_height(ui.available_height() - 44.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for m in &msgs {
                        let out = m["outgoing"] == true;
                        let who = nodes
                            .iter()
                            .find(|n| n["id"] == m["from"])
                            .map(|n| Model::str_of(n, "short_name").to_owned())
                            .unwrap_or_else(|| Model::str_of(m, "from").to_owned());
                        ui.label(
                            RichText::new(format!(
                                "{} {:<6} {}",
                                if out { "▲" } else { "▼" },
                                who,
                                Model::str_of(m, "text")
                            ))
                            .font(theme::mono(16.0))
                            .color(if out { CYAN } else { TEXT }),
                        );
                    }
                });
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut app.mesh.input)
                        .char_limit(228)
                        .hint_text("message to the mesh")
                        .font(theme::mono(16.0))
                        .desired_width(ui.available_width() - 130.0),
                );
                if focused && (app.entered || (nav.a && !nav.typing)) {
                    r.request_focus();
                }
                if r.has_focus() && nav.b {
                    r.surrender_focus();
                }
                let send = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (send || ui.button("TX").clicked()) && !app.mesh.input.trim().is_empty() {
                    let text = std::mem::take(&mut app.mesh.input);
                    let ch = app.mesh.channel;
                    app.invoke(
                        "mesh.send_message",
                        json!({"channel": ch, "text": text.trim()}),
                    );
                }
                if ui.button(format!("ch {}", app.mesh.channel)).clicked() {
                    app.mesh.channel = (app.mesh.channel + 1) % 8;
                }
            });
        },
    );
}

fn gps(app: &mut DeckApp, ui: &mut Ui) {
    let g = app.m.gps.clone();
    let f = &g["fix"];
    pane(
        ui,
        &format!(
            "GPS · gpsd {}",
            g["status"].as_str().unwrap_or("no data yet")
        ),
        false,
        |ui| {
            kv(
                ui,
                "fix",
                format!(
                    "{} ({} of {} sats)",
                    f["fix"].as_str().unwrap_or("none"),
                    g["used"],
                    g["visible"]
                ),
            );
            kv(
                ui,
                "lat/lon",
                format!(
                    "{:.6}, {:.6}",
                    f["lat"].as_f64().unwrap_or(0.0),
                    f["lon"].as_f64().unwrap_or(0.0)
                ),
            );
            kv(
                ui,
                "alt",
                format!(
                    "{:.1} m  hdop {}",
                    f["alt_m"].as_f64().unwrap_or(0.0),
                    g["hdop"]
                ),
            );
            kv(
                ui,
                "speed",
                format!(
                    "{:.1} m/s  track {:.0}°",
                    f["speed_mps"].as_f64().unwrap_or(0.0),
                    f["track_deg"].as_f64().unwrap_or(0.0)
                ),
            );
            kv(ui, "time", f["time"].as_str().unwrap_or("-"));
            let pps = g["pps_offset_ns"].as_i64();
            ui.label(
                RichText::new(format!(
                    "PPS {}",
                    pps.map(|o| format!(
                        "{o} ns {}",
                        if g["pps_locked"] == true {
                            "LOCKED"
                        } else {
                            ""
                        }
                    ))
                    .unwrap_or("no pulses".into())
                ))
                .color(if g["pps_locked"] == true {
                    GREEN
                } else {
                    AMBER
                }),
            );
            let size = ui
                .available_width()
                .min(ui.available_height() - 6.0)
                .max(120.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
            let p = ui.painter_at(rect);
            let c = rect.center();
            let r = size / 2.0 - 12.0;
            for k in [1.0, 2.0 / 3.0, 1.0 / 3.0] {
                p.circle_stroke(c, r * k, Stroke::new(1.0_f32, LINE));
            }
            p.line_segment(
                [Pos2::new(c.x - r, c.y), Pos2::new(c.x + r, c.y)],
                Stroke::new(1.0_f32, LINE),
            );
            p.line_segment(
                [Pos2::new(c.x, c.y - r), Pos2::new(c.x, c.y + r)],
                Stroke::new(1.0_f32, LINE),
            );
            painter_text(&p, Pos2::new(c.x - 4.0, c.y - r - 12.0), "N", DIM, 13.0);
            for s in g["satellites"].as_array().cloned().unwrap_or_default() {
                let (el, az) = (
                    s["el"].as_f64().unwrap_or(0.0),
                    s["az"].as_f64().unwrap_or(0.0),
                );
                let d = r * (1.0 - (el as f32 / 90.0));
                let a = (az as f32).to_radians();
                let pos = Pos2::new(c.x + d * a.sin(), c.y - d * a.cos());
                let used = s["used"] == true;
                p.circle_filled(pos, 5.0, if used { GREEN } else { DIM });
                painter_text(
                    &p,
                    pos + Vec2::new(6.0, -6.0),
                    &s["prn"].to_string(),
                    if used { CYAN } else { DIM },
                    12.0,
                );
            }
        },
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn great_circle() {
        let (km, b) = super::distance((37.7749, -122.4194), (37.8044, -122.4660));
        assert!((km - 5.24).abs() < 0.1, "{km}");
        assert!((290.0..310.0).contains(&b), "{b}");
    }
}
