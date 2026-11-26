//! [7] SYSTEM — the AI provider (model, effort, endpoint, write-only API key), power rails,
//! display, phone pairing (QR / Bluetooth PIN), paired phones, the audit log and power
//! actions (hold A).

use crate::app::{pane, step, Action, DeckApp, Nav};
use crate::client::Tag;
use crate::model::Model;
use crate::theme::{self, AMBER, CRIMSON, CYAN, DIM, TEXT};
use crate::widgets::{self, Field, Form};
use crate::ws::Section;
use egui::{Color32, Rect, RichText, Sense, Ui, Vec2};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const AI: usize = 0;
pub const RAILS: usize = 1;
pub const DISPLAY: usize = 2;
pub const PAIRING: usize = 3;
pub const PHONES: usize = 4;
pub const AUDIT: usize = 5;
pub const POWER: usize = 6;

pub const PROTOCOLS: [&str; 3] = ["anthropic", "openai", "openrouter"];
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// (name, protocol, base URL, key name, example model): the companion app's provider buttons.
pub const PRESETS: [(&str, &str, &str, &str, &str); 9] = [
    (
        "Anthropic",
        "anthropic",
        "",
        "ANTHROPIC_API_KEY",
        "claude-sonnet-5-5",
    ),
    (
        "OpenRouter",
        "openrouter",
        "",
        "OPENROUTER_API_KEY",
        "anthropic/claude-haiku-5.5",
    ),
    (
        "MiniMax",
        "openai",
        "https://api.minimax.io/v1",
        "MINIMAX_API_KEY",
        "MiniMax-M3.1-Flash",
    ),
    ("OpenAI", "openai", "", "OPENAI_API_KEY", "gpt-6.2-sol"),
    (
        "Gemini",
        "openai",
        "https://generativelanguage.googleapis.com/v1beta/openai",
        "GEMINI_API_KEY",
        "gemini-4-hydrogen",
    ),
    (
        "xAI",
        "openai",
        "https://api.x.ai/v1",
        "XAI_API_KEY",
        "grok-4.7",
    ),
    (
        "DeepSeek",
        "openai",
        "https://api.deepseek.com",
        "DEEPSEEK_API_KEY",
        "deepseek-flash",
    ),
    (
        "Together",
        "openai",
        "https://api.together.ai/v1",
        "TOGETHER_API_KEY",
        "moonshotai/Kimi-K3",
    ),
    (
        "Fireworks",
        "openai",
        "https://api.fireworks.ai/inference/v1",
        "FIREWORKS_API_KEY",
        "accounts/fireworks/models/kimi-k3p1-pro",
    ),
];

#[derive(Default)]
pub struct Ai {
    pub form: Form,
    /// Index into PRESETS, or PRESETS.len() for "custom".
    pub preset: usize,
    pub protocol: usize,
    pub model: String,
    pub effort: usize,
    pub base_url: String,
    pub key_env: String,
    /// Typed here, sent once to the vault, then cleared; never shown or logged.
    pub key: String,
}

#[derive(Default)]
pub struct State {
    pub confirm: Option<(String, bool)>,
    pub ai: Option<Ai>,
    pub row: usize,
    pub revoke: Option<String>,
    hold: Option<(usize, Instant)>,
}

const HOLD: Duration = Duration::from_millis(1500);

pub fn qr(ui: &mut Ui, data: &str, size: f32) {
    let Ok(code) = qrcode::QrCode::new(data.as_bytes()) else {
        ui.label("QR encode failed");
        return;
    };
    let n = code.width();
    let colors = code.to_colors();
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, Color32::WHITE);
    let quiet = 2.0;
    let cell = size / (n as f32 + 2.0 * quiet);
    for (i, c) in colors.iter().enumerate() {
        if *c == qrcode::Color::Dark {
            let (x, y) = ((i % n) as f32 + quiet, (i / n) as f32 + quiet);
            p.rect_filled(
                Rect::from_min_size(
                    rect.min + Vec2::new(x * cell, y * cell),
                    Vec2::splat(cell.ceil()),
                ),
                0.0,
                Color32::BLACK,
            );
        }
    }
}

fn rails(app: &DeckApp) -> Vec<Value> {
    app.m.deck_status["rails"]
        .as_array()
        .or(app.m.state["deck"]["rails"].as_array())
        .cloned()
        .unwrap_or_default()
}

pub fn sections(app: &DeckApp) -> Vec<Section> {
    let llm = &app.m.config["llm"];
    let ai_sub = if llm.is_null() {
        "provider, model, API key".to_owned()
    } else {
        format!(
            "{} · {}",
            Model::str_of(llm, "provider"),
            Model::str_of(llm, "model")
        )
    };
    let key_missing = !llm.is_null() && !key_status(app, Model::str_of(llm, "api_key_env")).0;
    let on: Vec<String> = rails(app)
        .iter()
        .filter(|r| r["on"] == true)
        .map(|r| Model::str_of(r, "name").to_owned())
        .collect();
    let pairing = app.m.state["pairing"]["mode"].as_str().unwrap_or("");
    let phones = app.m.devices["devices"]
        .as_array()
        .map(|d| d.iter().filter(|x| x["revoked"] != true).count())
        .unwrap_or(0);
    let t = &app.m.telemetry["cpu"];
    vec![
        Section::new("🔑", "AI provider")
            .detail(ai_sub)
            .badge(key_missing.then(|| "KEY".into())),
        Section::new("⚡", "Power rails").detail(if on.is_empty() {
            "none on".into()
        } else {
            format!("on: {}", on.join(" "))
        }),
        Section::new("☀", "Display").detail(format!(
            "{}% · {}",
            app.m.state["ui"]["brightness"].as_u64().unwrap_or(100),
            if app.paper { "paper" } else { "dark" }
        )),
        Section::new("🔐", "Pair a phone")
            .detail(if pairing.is_empty() {
                "QR or Bluetooth PIN".to_owned()
            } else {
                format!("{pairing} window open")
            })
            .badge((!pairing.is_empty()).then(|| "ON".into())),
        Section::new("📱", "Paired phones").detail(format!("{phones} paired")),
        Section::new("📜", "Audit log").detail("gate decisions, rails, power"),
        Section::new("🔋", "Power").detail(match (t["temp_c"].as_f64(), t["load1"].as_f64()) {
            (Some(c), Some(l)) => format!("{c:.0}°C · load {l:.2}"),
            _ => "reboot, shut down".into(),
        }),
    ]
}

/// (is set, masked tail) for a secret name.
fn key_status(app: &DeckApp, name: &str) -> (bool, String) {
    app.m
        .secrets
        .as_array()
        .and_then(|a| a.iter().find(|s| s["name"] == name))
        .map(|s| (s["is_set"] == true, Model::str_of(s, "masked").to_owned()))
        .unwrap_or((false, String::new()))
}

pub fn show(app: &mut DeckApp, ui: &mut Ui, nav: Nav, section: usize, focused: bool) {
    match section {
        AI => ai(app, ui, nav, focused),
        RAILS => rail_rows(app, ui, nav, focused),
        DISPLAY => display(app, ui, nav, focused),
        PAIRING => pairing(app, ui, nav, focused),
        PHONES => phones(app, ui, nav, focused),
        AUDIT => audit(app, ui),
        _ => power(app, ui, nav, focused),
    }
}

fn load_ai(llm: &Value) -> Ai {
    let protocol = Model::str_of(llm, "provider");
    let (base, env) = (
        Model::str_of(llm, "base_url"),
        Model::str_of(llm, "api_key_env"),
    );
    Ai {
        preset: PRESETS
            .iter()
            .position(|p| p.1 == protocol && p.2 == base && p.3 == env)
            .unwrap_or(PRESETS.len()),
        protocol: PROTOCOLS.iter().position(|p| *p == protocol).unwrap_or(0),
        model: Model::str_of(llm, "model").to_owned(),
        effort: EFFORTS
            .iter()
            .position(|e| *e == Model::str_of(llm, "effort"))
            .unwrap_or(1),
        base_url: base.to_owned(),
        key_env: env.to_owned(),
        ..Default::default()
    }
}

/// A secret NAME: letters, digits and _, not starting with a digit (the harness rule).
pub fn valid_name(s: &str) -> bool {
    let mut c = s.chars();
    c.next()
        .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

fn ai(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    if app.m.config.is_null() {
        if app.frames % 90 == 5 {
            app.get("/v1/config", Tag::Config);
            app.get("/v1/secrets/status", Tag::Secrets);
        }
        pane(ui, "AI PROVIDER", focused, |ui| {
            widgets::note(
                ui,
                "Loading lapis.toml from the agent… (is lapis-harness running?)",
            )
        });
        return;
    }
    if app.sys.ai.is_none() {
        app.sys.ai = Some(load_ai(&app.m.config["llm"]));
    }
    let Some(mut f) = app.sys.ai.take() else {
        return;
    };
    let (is_set, masked) = key_status(app, &f.key_env);
    let presets: Vec<&str> = PRESETS
        .iter()
        .map(|p| p.0)
        .chain(std::iter::once("custom"))
        .collect();
    let key_hint = if is_set {
        format!("stored ({masked}); type to replace")
    } else {
        "paste the key; it is stored write-only".to_owned()
    };
    let out = pane(ui, "AI PROVIDER", focused, |ui| {
        widgets::note(ui, "The deck's agent uses this model. The key goes to the device vault (secrets.env, 0600) and is never shown again.");
        ui.add_space(4.0);
        let mut fields = [
            Field::Choice {
                label: "PROVIDER",
                options: &presets,
                index: &mut f.preset,
            },
            Field::Choice {
                label: "PROTOCOL",
                options: &PROTOCOLS,
                index: &mut f.protocol,
            },
            Field::Text {
                label: "MODEL",
                value: &mut f.model,
                hint: "model id",
                secret: false,
            },
            Field::Choice {
                label: "EFFORT",
                options: &EFFORTS,
                index: &mut f.effort,
            },
            Field::Text {
                label: "BASE URL",
                value: &mut f.base_url,
                hint: "empty = the protocol's own endpoint",
                secret: false,
            },
            Field::Text {
                label: "KEY NAME",
                value: &mut f.key_env,
                hint: "e.g. ANTHROPIC_API_KEY",
                secret: false,
            },
            Field::Text {
                label: "API KEY",
                value: &mut f.key,
                hint: &key_hint,
                secret: true,
            },
            Field::Action {
                label: "SAVE",
                danger: false,
            },
        ];
        let out = widgets::form(ui, "ai", &mut f.form, &mut fields, nav, focused);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            widgets::chip(
                ui,
                &format!("{} {}", f.key_env, if is_set { "SET" } else { "NOT SET" }),
                if is_set { CYAN } else { AMBER },
            );
            let llm = &app.m.config["llm"];
            widgets::note(
                ui,
                &format!(
                    "saved: {} · {} · effort {}",
                    Model::str_of(llm, "provider"),
                    Model::str_of(llm, "model"),
                    Model::str_of(llm, "effort")
                ),
            );
        });
        out
    });
    app.grab_b |= out.grab_b;
    app.grab_left |= out.grab_left;
    if focused {
        app.hint("▲▼", "field");
        app.hint("◀▶", "change");
        app.hint(
            "A",
            if f.form.editing {
                "next field"
            } else {
                "edit / save"
            },
        );
    }
    if out.changed == Some(0) {
        if let Some(p) = PRESETS.get(f.preset) {
            f.protocol = PROTOCOLS.iter().position(|x| *x == p.1).unwrap_or(0);
            f.base_url = p.2.to_owned();
            f.key_env = p.3.to_owned();
            f.model = p.4.to_owned();
        }
    }
    if out.changed == Some(1) {
        f.preset = PRESETS.len();
    }
    if out.activated == Some(7) {
        save_ai(app, &mut f);
    }
    app.sys.ai = Some(f);
}

fn save_ai(app: &mut DeckApp, f: &mut Ai) {
    let env = f.key_env.trim().to_owned();
    if !valid_name(&env) {
        app.m.toast(
            "error",
            "Key name: letters, digits and _ (the NAME of the key, not the key)",
        );
        return;
    }
    if f.model.trim().is_empty() {
        app.m.toast("error", "Model is empty");
        return;
    }
    let key = std::mem::take(&mut f.key);
    if !key.trim().is_empty() {
        app.client.request(
            "PATCH",
            "/v1/secrets",
            Some(json!({"set": {env.clone(): key.trim()}})),
            Tag::Secrets,
        );
    }
    let mut cfg = app.m.config.clone();
    cfg["llm"]["provider"] = json!(PROTOCOLS[f.protocol]);
    cfg["llm"]["model"] = json!(f.model.trim());
    cfg["llm"]["effort"] = json!(EFFORTS[f.effort]);
    cfg["llm"]["base_url"] = json!(f.base_url.trim());
    cfg["llm"]["api_key_env"] = json!(env);
    app.client.request(
        "PUT",
        "/v1/config",
        Some(json!({"config": cfg})),
        Tag::ConfigSaved,
    );
}

fn rail_rows(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    let rails = rails(app);
    if focused && app.sys.confirm.is_none() {
        step(&mut app.sys.row, rails.len(), nav);
        app.hint("▲▼", "rail");
        app.hint("A", "switch on / off");
    }
    let sel = app.sys.row.min(rails.len().saturating_sub(1));
    // Wi-Fi and Bluetooth cut the phone's link: they take a second A.
    if let Some((rail, on)) = app.sys.confirm.clone() {
        if nav.a {
            app.client.request(
                "POST",
                &format!("/v1/deck/rails/{rail}"),
                Some(json!({"on": on})),
                Tag::Ignore,
            );
            app.sys.confirm = None;
        } else if nav.b {
            app.sys.confirm = None;
            app.grab_b = true;
        }
        app.hint("A", "confirm");
        app.hint("B", "cancel");
    } else if let Some(r) = rails.get(sel).filter(|_| focused && nav.a) {
        let name = Model::str_of(r, "name").to_owned();
        let want = r["on"] != true;
        if r["switchable"] != true {
            app.m
                .toast("warn", format!("{name} is fixed on this hardware"));
        } else if matches!(name.as_str(), "wifi" | "bluetooth") && !want {
            app.sys.confirm = Some((name, want));
        } else {
            app.client.request(
                "POST",
                &format!("/v1/deck/rails/{name}"),
                Some(json!({"on": want})),
                Tag::Ignore,
            );
        }
    }
    pane(ui, "POWER RAILS", focused, |ui| {
        if rails.is_empty() {
            widgets::note(ui, "No rails reported yet.");
        }
        for (i, r) in rails.iter().enumerate() {
            ui.horizontal(|ui| {
                widgets::toggle(ui, r["on"].as_bool(), focused && i == sel);
                ui.add_space(8.0);
                let fixed = if r["switchable"] == true {
                    ""
                } else {
                    "  (fixed)"
                };
                ui.label(
                    RichText::new(format!("{}{fixed}", Model::str_of(r, "name")))
                        .font(theme::mono(17.0))
                        .color(if focused && i == sel { CYAN } else { TEXT }),
                );
            });
            ui.add_space(4.0);
        }
        if let Some((rail, _)) = &app.sys.confirm {
            widgets::danger_note(
                ui,
                &format!("Turning {rail} off cuts the phone's link. A confirms, B cancels."),
            );
        }
    });
}

fn display(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    let b = app.m.state["ui"]["brightness"].as_u64().unwrap_or(100);
    if focused {
        step(&mut app.sys.row, 2, nav);
        app.hint("▲▼", "row");
        app.hint("◀▶", "adjust");
        app.grab_left = app.sys.row == 0;
    }
    let row = app.sys.row.min(1);
    if focused && row == 0 && (nav.left || nav.right) {
        let nb = if nav.right {
            (b + 10).min(100)
        } else {
            b.saturating_sub(10).max(10)
        };
        app.client.request(
            "PATCH",
            "/v1/device/ui",
            Some(json!({"brightness": nb})),
            Tag::Ignore,
        );
    }
    if focused && row == 1 && nav.a {
        app.run(Action::Theme);
    }
    pane(ui, "DISPLAY", focused, |ui| {
        widgets::list_row(
            ui,
            &format!("Brightness   ◀ {b:>3}% ▶"),
            focused && row == 0,
            TEXT,
        );
        let (bar, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width().min(500.0), 14.0),
            Sense::hover(),
        );
        ui.painter().rect_filled(bar, 4, theme::RAISED);
        ui.painter().rect_filled(
            Rect::from_min_size(
                bar.min,
                Vec2::new(bar.width() * b as f32 / 100.0, bar.height()),
            ),
            4,
            CYAN,
        );
        ui.add_space(8.0);
        widgets::list_row(
            ui,
            &format!(
                "Theme        {}   (A switches)",
                if app.paper { "paper" } else { "dark" }
            ),
            focused && row == 1,
            TEXT,
        );
    });
}

fn pairing(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    let acts = [
        ("Pair with QR code (over Tailscale)", Action::PairQr),
        ("Pair with Bluetooth PIN", Action::PairBle),
        ("Stop pairing", Action::StopPairing),
    ];
    if focused {
        step(&mut app.sys.row, acts.len(), nav);
        app.hint("▲▼", "choose");
        app.hint("A", "start");
    }
    let row = app.sys.row.min(acts.len() - 1);
    let mut run = (focused && nav.a).then(|| acts[row].1.clone());
    if nav.x {
        run = Some(Action::PairQr);
    }
    let p = app.m.state["pairing"].clone();
    egui::SidePanel::left("pair_actions").exact_width(380.0).resizable(false).frame(egui::Frame::NONE.inner_margin(egui::Margin { right: 8, ..Default::default() })).show_inside(ui, |ui| {
        pane(ui, "PAIR A PHONE", focused, |ui| {
            widgets::note(ui, "In the Lapis app: Pair new device. QR works over Tailscale; Bluetooth needs no network.");
            ui.add_space(4.0);
            for (i, (label, a)) in acts.iter().enumerate() {
                if widgets::list_row(ui, label, focused && i == row, TEXT).clicked() {
                    run = Some(a.clone());
                }
            }
        });
    });
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show_inside(ui, |ui| {
            pane(ui, "PAIRING WINDOW", false, |ui| match p["mode"].as_str() {
                Some("qr") => {
                    ui.horizontal(|ui| {
                        qr(ui, Model::str_of(&p, "payload"), 260.0);
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new("Scan with the Lapis app")
                                    .font(theme::sans(18.0))
                                    .color(CYAN),
                            );
                            ui.label(
                                RichText::new(format!("PIN {}", Model::str_of(&p, "pin")))
                                    .font(theme::mono(34.0))
                                    .color(TEXT),
                            );
                            widgets::field(ui, "host", Model::str_of(&p, "host"));
                            widgets::field(ui, "expires", format!("{} s", p["expires_in"]));
                        });
                    });
                }
                Some("pin") => {
                    ui.label(
                        RichText::new("Bluetooth: choose this deck in the app, then type")
                            .font(theme::sans(18.0))
                            .color(CYAN),
                    );
                    ui.label(
                        RichText::new(Model::str_of(&p, "pin"))
                            .font(theme::mono(64.0))
                            .color(TEXT),
                    );
                    widgets::field(ui, "device", Model::str_of(&p, "device_name"));
                    widgets::field(ui, "status", Model::str_of(&p, "status"));
                }
                _ => widgets::note(ui, "Not pairing. Choose QR or Bluetooth on the left."),
            });
        });
    if let Some(a) = run {
        app.run(a);
    }
}

fn phones(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    let devs: Vec<Value> = app.m.devices["devices"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if focused {
        let before = app.sys.row;
        step(&mut app.sys.row, devs.len(), nav);
        if before != app.sys.row {
            app.sys.revoke = None;
        }
        app.hint("▲▼", "phone");
        app.hint(
            "A",
            if app.sys.revoke.is_some() {
                "A again revokes"
            } else {
                "revoke"
            },
        );
    }
    let row = app.sys.row.min(devs.len().saturating_sub(1));
    if focused && nav.b && app.sys.revoke.is_some() {
        app.sys.revoke = None;
        app.grab_b = true;
    }
    if let Some(d) = devs
        .get(row)
        .filter(|d| focused && nav.a && d["revoked"] != true)
    {
        let id = Model::str_of(d, "id").to_owned();
        if app.sys.revoke.as_deref() == Some(id.as_str()) {
            app.client
                .request("DELETE", &format!("/v1/devices/{id}"), None, Tag::Ignore);
            app.sys.revoke = None;
        } else {
            app.sys.revoke = Some(id);
        }
    }
    pane(ui, "PAIRED PHONES", focused, |ui| {
        if devs.is_empty() {
            widgets::note(ui, "No phones yet: open \"Pair a phone\".");
        }
        for (i, d) in devs.iter().enumerate() {
            let revoked = d["revoked"] == true;
            let pending = app.sys.revoke.as_deref() == Some(Model::str_of(d, "id"));
            let text = format!(
                "{:<12} {:<20} {}",
                Model::str_of(d, "id"),
                Model::str_of(d, "name"),
                if revoked {
                    "revoked"
                } else if pending {
                    "A again: revoke"
                } else {
                    ""
                }
            );
            widgets::list_row(
                ui,
                &text,
                focused && i == row,
                if revoked {
                    DIM
                } else if pending {
                    CRIMSON
                } else {
                    TEXT
                },
            );
        }
    });
}

fn audit(app: &mut DeckApp, ui: &mut Ui) {
    pane(
        ui,
        "AUDIT LOG · Policy Gate decisions, rails, power",
        false,
        |ui| {
            egui::ScrollArea::vertical()
                .id_salt("audit")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for e in app.m.audit["entries"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                    {
                        let d = &e["data"];
                        let line = format!(
                            "{} {:<14} {} {} {}",
                            e["ts"],
                            Model::str_of(&e, "event"),
                            d["tool"]
                                .as_str()
                                .or(d["rail"].as_str())
                                .or(d["action"].as_str())
                                .unwrap_or(""),
                            d["decision"].as_str().unwrap_or(""),
                            d["by"].as_str().unwrap_or("")
                        );
                        ui.label(RichText::new(line).font(theme::mono(14.0)).color(
                            if e["event"] == "policy_deny" || e["event"] == "kill_all" {
                                CRIMSON
                            } else {
                                DIM
                            },
                        ));
                    }
                });
        },
    );
}

fn power(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    let acts = [
        ("reboot", "Reboot"),
        ("shutdown", "Shut down"),
        ("restart_daemon", "Restart Lapis services"),
    ];
    if focused {
        step(&mut app.sys.row, acts.len(), nav);
        app.hint("▲▼", "choose");
        app.hint("A", "hold to confirm");
    }
    let row = app.sys.row.min(acts.len() - 1);
    let held = focused && app.pad_or_enter_held(ui.ctx());
    match (&app.sys.hold, held) {
        (Some((i, t)), true) if *i == row && t.elapsed() >= HOLD => {
            app.client.request(
                "POST",
                "/v1/device/power",
                Some(json!({"action": acts[row].0})),
                Tag::Ignore,
            );
            app.sys.hold = None;
        }
        (Some((i, _)), true) if *i == row => {}
        (_, true) => app.sys.hold = Some((row, Instant::now())),
        (_, false) => app.sys.hold = None,
    }
    pane(ui, "POWER", focused, |ui| {
        for (i, (_, label)) in acts.iter().enumerate() {
            widgets::list_row(
                ui,
                &format!("{label}   (hold A)"),
                focused && i == row,
                CRIMSON,
            );
            if let Some((h, t)) = &app.sys.hold {
                if *h == i {
                    ui.add(
                        egui::ProgressBar::new(
                            (t.elapsed().as_secs_f32() / HOLD.as_secs_f32()).min(1.0),
                        )
                        .fill(CRIMSON)
                        .desired_height(8.0),
                    );
                }
            }
        }
        ui.add_space(10.0);
        let s = &app.m.state;
        widgets::field(ui, "host", s["device"]["hostname"].as_str().unwrap_or("?"));
        widgets::field(
            ui,
            "version",
            s["device"]["version"].as_str().unwrap_or("?"),
        );
        widgets::field(ui, "tailnet", s["tailscale_fqdn"].as_str().unwrap_or(""));
        widgets::field(ui, "agent", s["deck"]["harness"].as_str().unwrap_or("?"));
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn secret_names() {
        assert!(super::valid_name("ANTHROPIC_API_KEY"));
        assert!(super::valid_name("_X1"));
        assert!(!super::valid_name("1KEY"));
        assert!(!super::valid_name("sk-ant-abc"));
        assert!(!super::valid_name(""));
    }
}
