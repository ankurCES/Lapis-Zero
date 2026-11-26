//! [2] RF/SDR — FFT spectrum + scrolling waterfall, tuning by D-pad or a click on the
//! waterfall, demodulator presets and one-key decoder overlays.

use crate::app::{fmt_hz, painter_text, pane, step, DeckApp, Nav, RF};
use crate::model::{Model, WF_H, WF_W};
use crate::theme::{AMBER, CYAN, DIM, GREEN, LINE, TEXT};
use crate::ws::Section;
use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, TextureOptions, Ui, Vec2};
use serde_json::json;
use std::time::{Duration, Instant};

pub const STEPS: [u64; 6] = [1_000, 10_000, 100_000, 1_000_000, 10_000_000, 100_000_000];
pub const MODES: [&str; 5] = ["wfm", "nfm", "am", "usb", "lsb"];
pub const PRESETS: [(&str, u64, &str); 9] = [
    ("Broadcast FM 100.1", 100_100_000, "wfm"),
    ("Airband guard 121.5", 121_500_000, "am"),
    ("APRS 144.390", 144_390_000, "nfm"),
    ("NOAA WX 162.400", 162_400_000, "nfm"),
    ("ISM 433.92 (rtl_433)", 433_920_000, "nfm"),
    ("LoRa / ISM 915", 915_000_000, "nfm"),
    ("ADS-B 1090", 1_090_000_000, "am"),
    ("40 m USB 7.100", 7_100_000, "usb"),
    ("Pager 152.0075", 152_007_500, "nfm"),
];

pub struct State {
    pub step: usize,
    pub mode: usize,
    pub preset: usize,
    target: Option<(u64, Instant)>,
    texture: Option<TextureHandle>,
    pixels: Vec<Color32>,
}

impl Default for State {
    fn default() -> Self {
        State {
            step: 3,
            mode: 0,
            preset: 0,
            target: None,
            texture: None,
            pixels: vec![Color32::BLACK; WF_W * WF_H],
        }
    }
}

/// The waterfall's colour map (same stops as lapis_sdr::dsp::colormap).
pub fn colormap(level: u8) -> Color32 {
    const STOPS: [(f32, [f32; 3]); 5] = [
        (0.0, [8.0, 10.0, 14.0]),
        (0.35, [10.0, 40.0, 96.0]),
        (0.62, [0.0, 229.0, 255.0]),
        (0.82, [255.0, 176.0, 0.0]),
        (1.0, [255.0, 40.0, 70.0]),
    ];
    let x = f32::from(level) / 255.0;
    for w in STOPS.windows(2) {
        let ((x0, c0), (x1, c1)) = (w[0], w[1]);
        if x <= x1 {
            let t = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0);
            let c = [0, 1, 2].map(|i| (c0[i] + (c1[i] - c0[i]) * t) as u8);
            return Color32::from_rgb(c[0], c[1], c[2]);
        }
    }
    Color32::from_rgb(255, 40, 70)
}

fn tune(app: &mut DeckApp, hz: u64) {
    let hz = hz.clamp(100_000, 6_000_000_000);
    app.rf.target = Some((hz, Instant::now()));
    app.m.spectrum.center_hz = hz;
}

pub fn sections(app: &DeckApp) -> Vec<Section> {
    let s = &app.m.spectrum;
    vec![
        Section::new("📈", "Waterfall").detail(format!(
            "{} · step {}",
            fmt_hz(s.center_hz as f64),
            fmt_hz(STEPS[app.rf.step] as f64)
        )),
        Section::new("★", "Presets").detail(format!("{} bands", PRESETS.len())),
        Section::new("🔍", "Decoders").detail(format!(
            "mode {} · results",
            MODES[app.rf.mode].to_uppercase()
        )),
    ]
}

pub fn show(app: &mut DeckApp, ui: &mut Ui, nav: Nav, section: usize, focused: bool) {
    let center = app.m.spectrum.center_hz;
    if focused && section == 0 && !nav.typing {
        let st = STEPS[app.rf.step];
        if nav.left {
            tune(app, center.saturating_sub(st));
        }
        if nav.right {
            tune(app, center + st);
        }
        if nav.up {
            app.rf.step = (app.rf.step + 1).min(STEPS.len() - 1);
        }
        if nav.down {
            app.rf.step = app.rf.step.saturating_sub(1);
        }
        app.grab_left = true;
        app.hint("◀▶", "tune");
        app.hint("▲▼", "step size");
        app.hint("[ ]", "gain");
    }
    let (gain_up, gain_down) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::CloseBracket),
            i.key_pressed(egui::Key::OpenBracket),
        )
    });
    if focused && !nav.typing && (gain_up || gain_down) {
        let g = (app.m.spectrum.gain_db + if gain_up { 5.0 } else { -5.0 }).clamp(0.0, 50.0);
        app.client.request(
            "POST",
            "/v1/deck/sdr/tune",
            Some(json!({"center_hz": app.m.spectrum.center_hz.max(100_000), "gain_db": g})),
            crate::client::Tag::Ignore,
        );
    }
    // Debounced retune: holding the D-pad sends one request, not twenty.
    if let Some((hz, at)) = app.rf.target {
        if at.elapsed() > Duration::from_millis(150) {
            app.client.request(
                "POST",
                "/v1/deck/sdr/tune",
                Some(json!({"center_hz": hz})),
                crate::client::Tag::Ignore,
            );
            app.rf.target = None;
        }
    }
    if section > 0 {
        egui::SidePanel::left("rf_side")
            .exact_width(400.0)
            .resizable(false)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                right: 8,
                ..Default::default()
            }))
            .show_inside(ui, |ui| {
                if section == 1 {
                    presets(app, ui, nav, focused)
                } else {
                    decoders(app, ui, nav, focused)
                }
            });
    }
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show_inside(ui, |ui| waterfall(app, ui, focused && section == 0));
}

fn waterfall(app: &mut DeckApp, ui: &mut Ui, focused: bool) {
    let s = &app.m.spectrum;
    let chips = [
        (fmt_hz(s.center_hz as f64), CYAN),
        (format!("span {}", fmt_hz(f64::from(s.sample_rate))), DIM),
        (format!("step {}", fmt_hz(STEPS[app.rf.step] as f64)), DIM),
        (format!("gain {:.0} dB", s.gain_db), DIM),
        (
            if s.source.is_empty() {
                "no data yet".to_owned()
            } else {
                s.source.clone()
            },
            DIM,
        ),
    ];
    let synthetic = s.synthetic;
    let (floor, ceil, center, rate_u) = (s.floor_db, s.ceil_db, s.center_hz, s.sample_rate);
    let peaks = s.peaks.clone();
    let row0 = s.rows.front().cloned();
    pane(ui, "SPECTRUM", focused, |ui| {
        ui.horizontal(|ui| {
            for (t, c) in &chips {
                crate::widgets::chip(ui, t, *c);
            }
            if synthetic {
                crate::widgets::chip(ui, "SYNTHETIC IQ", AMBER);
            }
        });
        let avail = ui.available_size();
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(avail.x, avail.y - 4.0), Sense::click_and_drag());
        let spec_h = (rect.height() * 0.32).max(80.0);
        let spec = Rect::from_min_size(rect.min, Vec2::new(rect.width(), spec_h));
        let wf = Rect::from_min_max(Pos2::new(rect.min.x, spec.max.y + 4.0), rect.max);
        let painter = ui.painter_at(rect);
        painter.rect_filled(spec, 0.0, crate::theme::BG);
        for i in 1..4 {
            let y = spec.min.y + spec.height() * i as f32 / 4.0;
            painter.line_segment(
                [Pos2::new(spec.min.x, y), Pos2::new(spec.max.x, y)],
                Stroke::new(1.0_f32, LINE),
            );
        }
        if let Some(row) = &row0 {
            let pts: Vec<Pos2> = row
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    Pos2::new(
                        spec.min.x + spec.width() * i as f32 / WF_W as f32,
                        spec.max.y - spec.height() * f32::from(*v) / 255.0,
                    )
                })
                .collect();
            painter.add(egui::Shape::line(pts, Stroke::new(1.2_f32, CYAN)));
        }
        painter_text(
            &painter,
            spec.min + Vec2::new(4.0, 2.0),
            &format!("{ceil:.0} dBFS"),
            DIM,
            12.0,
        );
        painter_text(
            &painter,
            Pos2::new(spec.min.x + 4.0, spec.max.y - 14.0),
            &format!("{floor:.0} dBFS"),
            DIM,
            12.0,
        );
        // Peaks.
        // Strongest first; a label only where it does not overlap one already drawn.
        let mut labelled: Vec<f32> = Vec::new();
        for p in peaks.iter().take(8) {
            let f = p["freq_hz"].as_f64().unwrap_or(0.0);
            let x = spec.min.x
                + spec.width()
                    * ((f - (center as f64 - f64::from(rate_u) / 2.0)) / f64::from(rate_u.max(1)))
                        as f32;
            if labelled.iter().any(|l| (l - x).abs() < 110.0) {
                continue;
            }
            labelled.push(x);
            painter.line_segment(
                [Pos2::new(x, spec.min.y + 14.0), Pos2::new(x, spec.max.y)],
                Stroke::new(1.0_f32, AMBER.gamma_multiply(0.5)),
            );
            painter_text(
                &painter,
                Pos2::new(x + 2.0, spec.min.y + 14.0),
                &fmt_hz(f),
                AMBER,
                12.0,
            );
        }
        // Waterfall texture, newest row on top.
        if app.m.spectrum.dirty || app.rf.texture.is_none() {
            let st = &mut app.rf;
            st.pixels.fill(Color32::from_rgb(8, 10, 14));
            for (r, row) in app.m.spectrum.rows.iter().enumerate().take(WF_H) {
                for (c, v) in row.iter().enumerate().take(WF_W) {
                    st.pixels[r * WF_W + c] = colormap(*v);
                }
            }
            let img = egui::ColorImage {
                size: [WF_W, WF_H],
                pixels: st.pixels.clone(),
            };
            match &mut st.texture {
                Some(t) => t.set(img, TextureOptions::LINEAR),
                None => {
                    st.texture = Some(ui.ctx().load_texture(
                        "waterfall",
                        img,
                        TextureOptions::LINEAR,
                    ))
                }
            }
            app.m.spectrum.dirty = false;
        }
        if let Some(t) = &app.rf.texture {
            painter.image(
                t.id(),
                wf,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        // Center marker and frequency axis.
        let cx = rect.center().x;
        painter.line_segment(
            [Pos2::new(cx, rect.min.y), Pos2::new(cx, rect.max.y)],
            Stroke::new(1.0_f32, CYAN.gamma_multiply(0.4)),
        );
        for i in 0..=4 {
            let f =
                center as f64 - f64::from(rate_u) / 2.0 + f64::from(rate_u) * f64::from(i) / 4.0;
            let x = rect.min.x + rect.width() * i as f32 / 4.0;
            painter_text(
                &painter,
                Pos2::new(
                    (x - 40.0).clamp(rect.min.x, rect.max.x - 90.0),
                    wf.min.y + 2.0,
                ),
                &fmt_hz(f),
                TEXT,
                12.0,
            );
        }
        // Click (or trackball drag) on the waterfall tunes there.
        let rate = f64::from(rate_u);
        if let Some(pos) = resp.interact_pointer_pos() {
            if resp.clicked() {
                let frac = f64::from((pos.x - rect.min.x) / rect.width());
                let hz = center as f64 - rate / 2.0 + rate * frac;
                tune(app, hz.max(0.0) as u64);
            }
        }
        if resp.dragged() {
            let dx = f64::from(resp.drag_delta().x);
            let hz = (center as f64 - dx * rate / f64::from(rect.width()).max(1.0)).max(0.0);
            tune(app, hz as u64);
        }
    });
}

fn presets(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    if focused {
        step(&mut app.rf.preset, PRESETS.len(), nav);
        app.hint("▲▼", "band");
        app.hint("A", "tune there");
        if nav.a {
            let (_, hz, mode) = PRESETS[app.rf.preset];
            tune(app, hz);
            app.rf.mode = MODES.iter().position(|m| *m == mode).unwrap_or(0);
        }
    }
    pane(ui, "PRESETS", focused, |ui| {
        for (i, (name, hz, mode)) in PRESETS.iter().enumerate() {
            let here = app.m.spectrum.center_hz == *hz;
            let r = crate::widgets::list_row(
                ui,
                &format!(
                    "{}{name:<22} {}",
                    if here { "● " } else { "  " },
                    mode.to_uppercase()
                ),
                focused && i == app.rf.preset,
                if here { GREEN } else { TEXT },
            );
            if r.clicked() {
                tune(app, *hz);
            }
        }
    });
}

fn decoders(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    let focus = if focused { 2 } else { 0 };
    ui.add_space(4.0);
    let center = app.m.spectrum.center_hz;
    let mode = MODES[app.rf.mode];
    let actions: [(&str, &'static str, serde_json::Value); 5] = [
        (
            "Decode ISM (rtl_433)",
            "sdr.decode_ism",
            json!({"freq_hz": 433_920_000u64, "duration_s": 20}),
        ),
        (
            "Pager / APRS (multimon)",
            "sdr.decode_pager",
            json!({"freq_hz": center, "duration_s": 20}),
        ),
        ("ADS-B aircraft", "sdr.adsb_snapshot", json!({})),
        (
            "Listen 3 s here",
            "sdr.demod_audio",
            json!({"freq_hz": center, "mode": mode, "duration_s": 3, "play": true}),
        ),
        (
            "Scan ±5 MHz",
            "sdr.scan_band",
            json!({"start_hz": center.saturating_sub(5_000_000), "end_hz": center + 5_000_000, "step_hz": 1_000_000}),
        ),
    ];
    if focus == 2 {
        step(&mut app.sel[RF][2], actions.len(), nav);
        app.hint("▲▼", "decoder");
        app.hint("A", "run");
        app.hint("M", "demod mode");
    }
    let sel = app.sel[RF][2];
    let mut run = None;
    pane(
        ui,
        &format!("DECODERS · {}", mode.to_uppercase()),
        focus == 2,
        |ui| {
            if ui.input(|i| i.key_pressed(egui::Key::M)) && !nav.typing {
                app.rf.mode = (app.rf.mode + 1) % MODES.len();
            }
            for (i, (label, tool, args)) in actions.iter().enumerate() {
                let busy = app.m.tool_busy.get(*tool).copied().unwrap_or(false);
                let on = focus == 2 && i == sel;
                let r = crate::widgets::list_row(
                    ui,
                    &format!("{label}{}", if busy { "  …running" } else { "" }),
                    on,
                    TEXT,
                );
                if r.clicked() || (on && nav.a) {
                    run = Some((*tool, args.clone()));
                }
            }
        },
    );
    if let Some((t, a)) = run {
        app.invoke(t, a);
    }
    ui.add_space(4.0);
    pane(ui, "DECODED", false, |ui| {
        egui::ScrollArea::vertical()
            .id_salt("decoded")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(r) = app.m.tool_results.get("sdr.adsb_snapshot") {
                    let table = if r["result"]["table"].is_null() {
                        &r["result"]
                    } else {
                        &r["result"]["table"]
                    };
                    for a in table["aircraft"].as_array().cloned().unwrap_or_default() {
                        ui.label(
                            RichText::new(format!(
                                "✈ {:<8} {:>6} ft {:>4} kt {}",
                                a["flight"].as_str().unwrap_or(Model::str_of(&a, "hex")),
                                a["alt_baro"],
                                a["gs"]
                                    .as_f64()
                                    .map(|g| format!("{g:.0}"))
                                    .unwrap_or_default(),
                                a["squawk"].as_str().unwrap_or("")
                            ))
                            .color(TEXT)
                            .size(14.0),
                        );
                    }
                }
                if let Some(r) = app
                    .m
                    .tool_results
                    .get("sdr.demod_audio")
                    .filter(|r| r["ok"] == true)
                {
                    ui.label(
                        RichText::new(format!(
                            "♪ {} {:.1}s rms {:.2} tone {:.0} Hz",
                            r["result"]["mode"].as_str().unwrap_or(""),
                            r["result"]["seconds"].as_f64().unwrap_or(0.0),
                            r["result"]["rms"].as_f64().unwrap_or(0.0),
                            r["result"]["dominant_hz"].as_f64().unwrap_or(0.0)
                        ))
                        .color(GREEN)
                        .size(14.0),
                    );
                }
                if let Some(r) = app
                    .m
                    .tool_results
                    .get("sdr.scan_band")
                    .filter(|r| r["ok"] == true)
                {
                    for p in r["result"]["peaks"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .take(8)
                    {
                        ui.label(
                            RichText::new(format!(
                                "⌁ {} {:.0} dB (+{:.0})",
                                fmt_hz(p["freq_hz"].as_f64().unwrap_or(0.0)),
                                p["power_db"].as_f64().unwrap_or(0.0),
                                p["snr_db"].as_f64().unwrap_or(0.0)
                            ))
                            .color(AMBER)
                            .size(14.0),
                        );
                    }
                }
                for d in app.m.decodes.iter().take(30) {
                    let p = &d["packet"];
                    let line = match d["decoder"].as_str() {
                        Some("rtl_433") => format!(
                            "◎ {} id {} {}°C {}%",
                            Model::str_of(p, "model"),
                            p["id"],
                            p["temperature_C"],
                            p["humidity"]
                        ),
                        _ => format!(
                            "☏ {} {} {}",
                            Model::str_of(p, "proto"),
                            p["address"].as_str().or(p["from"].as_str()).unwrap_or(""),
                            p["message"].as_str().unwrap_or("")
                        ),
                    };
                    ui.label(RichText::new(line).color(TEXT).size(14.0));
                }
            });
    });
}
