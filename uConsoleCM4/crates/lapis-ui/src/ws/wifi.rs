//! NET › Wi-Fi: the deck's own network. See it, scan, join with a password typed on the
//! keyboard, forget saved networks, or let a paired phone set it over Bluetooth. Everything
//! goes through the daemon's `/v1/wifi` API; a password lives in this pane only until sent.

use crate::app::{kv, pane, step, DeckApp, Nav};
use crate::client::Tag;
use crate::model::Model;
use crate::theme::{AMBER, DIM, GREEN, TEXT};
use crate::widgets::{self, Field, Form};
use egui::Ui;
use serde_json::{json, Value};

#[derive(Debug, Default, Clone, PartialEq)]
pub enum Mode {
    #[default]
    List,
    Join {
        ssid: String,
        secured: bool,
    },
    Forget(String),
}

#[derive(Default)]
pub struct State {
    pub sel: usize,
    pub mode: Mode,
    pub psk: String,
    pub form: Form,
    scanned: bool,
}

enum Row {
    Scan,
    Network(Value),
    Saved(Value),
    Phone,
}

fn rows(app: &DeckApp) -> Vec<Row> {
    let mut v = vec![Row::Scan];
    v.extend(app.m.wifi_networks.iter().cloned().map(Row::Network));
    v.extend(
        app.m.wifi["saved"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(Row::Saved),
    );
    v.push(Row::Phone);
    v
}

pub fn detail(app: &DeckApp) -> String {
    match app.m.wifi["ssid"].as_str().filter(|s| !s.is_empty()) {
        Some(s) => s.to_owned(),
        None => "not connected".into(),
    }
}

pub fn scan(app: &mut DeckApp) {
    if app.m.wifi_busy.is_empty() {
        app.m.wifi_busy = "Scanning…".into();
        app.get("/v1/wifi/scan", Tag::WifiScan);
    }
}

fn connect(app: &mut DeckApp, ssid: String) {
    let psk = std::mem::take(&mut app.net.wifi.psk);
    let psk = (!psk.is_empty()).then_some(psk);
    app.m.wifi_busy = format!("Joining {ssid}… (up to 45 s)");
    app.client.request(
        "POST",
        "/v1/wifi/connect",
        Some(json!({"ssid": ssid, "psk": psk})),
        Tag::WifiConnect(ssid),
    );
    back(app);
}

fn back(app: &mut DeckApp) {
    let w = &mut app.net.wifi;
    w.psk.clear();
    w.mode = Mode::List;
    w.form = Form::default();
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn show(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    if !app.net.wifi.scanned {
        app.net.wifi.scanned = true;
        scan(app);
    }
    let mode = app.net.wifi.mode.clone();
    match mode {
        Mode::List => list(app, ui, nav, focused),
        Mode::Join { ssid, secured } => join(app, ui, nav, focused, ssid, secured),
        Mode::Forget(name) => forget(app, ui, nav, focused, name),
    }
}

fn status(app: &DeckApp, ui: &mut Ui) {
    let w = &app.m.wifi;
    kv(ui, "NETWORK", detail(app));
    if let Some(ip) = w["ip"].as_str().filter(|s| !s.is_empty()) {
        kv(ui, "ADDRESS", ip);
    }
    if let Some(r) = w["rssi"].as_i64() {
        kv(ui, "SIGNAL", format!("{r} dBm"));
    }
    let until = w["ble"]["expires_at"].as_i64().unwrap_or(0);
    if w["ble"]["open"] == true && until > now() {
        kv(
            ui,
            "PHONE",
            format!(
                "paired phones can set Wi-Fi over Bluetooth ({} min left)",
                (until - now() + 59) / 60
            ),
        );
    }
    if !app.m.wifi_busy.is_empty() {
        ui.label(
            egui::RichText::new(&app.m.wifi_busy)
                .font(crate::theme::mono(15.0))
                .color(AMBER),
        );
    }
    ui.add_space(6.0);
}

fn list(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    let rows = rows(app);
    if focused {
        step(&mut app.net.wifi.sel, rows.len(), nav);
        app.hint("▲▼", "choose");
        app.hint("A", "join / forget");
        app.hint("X", "scan");
        if nav.x {
            scan(app);
        }
    }
    let sel = app.net.wifi.sel;
    let mut picked = (focused && nav.a).then_some(sel);
    pane(ui, "WI-FI NETWORK", focused, |ui| {
        status(app, ui);
        widgets::note(
            ui,
            "Saved networks rejoin by themselves. A paired phone can also change Wi-Fi from the app.",
        );
        egui::ScrollArea::vertical()
            .id_salt("wifi-list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, r) in rows.iter().enumerate() {
                    let (text, color) = match r {
                        Row::Scan => ("⟳  SCAN FOR NETWORKS".to_owned(), TEXT),
                        Row::Network(n) => {
                            let rssi = n["rssi"].as_i64().unwrap_or(-100);
                            let lock = if n["secured"] == true {
                                "secured"
                            } else {
                                "open"
                            };
                            (
                                format!("{:<32} {:>4} dBm  {lock}", Model::str_of(n, "ssid"), rssi),
                                if rssi > -60 {
                                    GREEN
                                } else if rssi > -75 {
                                    TEXT
                                } else {
                                    DIM
                                },
                            )
                        }
                        Row::Saved(s) => (
                            format!(
                                "saved: {}{}",
                                Model::str_of(s, "name"),
                                if s["active"] == true {
                                    "  (connected)"
                                } else {
                                    ""
                                }
                            ),
                            if s["active"] == true { GREEN } else { DIM },
                        ),
                        Row::Phone => (
                            "📱  LET MY PHONE SET IT (Bluetooth, 5 min)".to_owned(),
                            TEXT,
                        ),
                    };
                    if widgets::list_row(ui, &text, focused && i == sel, color).clicked() {
                        app.net.wifi.sel = i;
                        picked = Some(i);
                    }
                }
            });
    });
    let Some(row) = picked.and_then(|i| rows.into_iter().nth(i)) else {
        return;
    };
    match row {
        Row::Scan => scan(app),
        Row::Network(n) => {
            app.net.wifi.mode = Mode::Join {
                ssid: Model::str_of(&n, "ssid").to_owned(),
                secured: n["secured"] == true,
            };
            app.net.wifi.form = Form::default();
        }
        Row::Saved(s) => {
            app.net.wifi.mode = Mode::Forget(Model::str_of(&s, "name").to_owned());
            app.net.wifi.form = Form::default();
        }
        Row::Phone => app
            .client
            .request("POST", "/v1/wifi/ble", Some(json!({})), Tag::WifiBle),
    }
}

/// B outside a text field goes back to the list (not out of the workspace).
fn cancel_on_b(app: &mut DeckApp, nav: Nav, focused: bool, was_editing: bool, grabbed: bool) {
    if focused && nav.b && !was_editing && !grabbed {
        back(app);
        app.grab_b = true;
    }
}

fn join(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool, ssid: String, secured: bool) {
    let was_editing = app.net.wifi.form.editing;
    let title = format!("JOIN {ssid}");
    let out = pane(ui, &title, focused, |ui| {
        let w = &mut app.net.wifi;
        let mut fields = Vec::new();
        if secured {
            fields.push(Field::Text {
                label: "PASSWORD",
                value: &mut w.psk,
                hint: "type it on the keyboard",
                secret: true,
            });
        }
        fields.push(Field::Action {
            label: "CONNECT",
            danger: false,
        });
        fields.push(Field::Action {
            label: "CANCEL",
            danger: false,
        });
        let out = widgets::form(ui, "wifi-join", &mut w.form, &mut fields, nav, focused);
        widgets::note(
            ui,
            if secured {
                "The password goes to NetworkManager only; it is never shown or logged."
            } else {
                "Open network: no password, and traffic on it is not encrypted."
            },
        );
        out
    });
    app.grab_b |= out.grab_b;
    if focused {
        app.hint("A", "edit / run");
        app.hint("B", "back");
    }
    let connect_row = usize::from(secured);
    match out.activated {
        Some(i) if i == connect_row => {
            let n = app.net.wifi.psk.len();
            if secured && !(8..=63).contains(&n) && n != 64 {
                app.m
                    .toast("warn", "A Wi-Fi password is 8 to 63 characters");
            } else {
                connect(app, ssid);
            }
        }
        Some(_) => back(app),
        None => cancel_on_b(app, nav, focused, was_editing, out.grab_b),
    }
}

fn forget(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool, name: String) {
    let title = format!("FORGET {name}?");
    let out = pane(ui, &title, focused, |ui| {
        let mut fields = [
            Field::Action {
                label: "FORGET",
                danger: true,
            },
            Field::Action {
                label: "CANCEL",
                danger: false,
            },
        ];
        let out = widgets::form(
            ui,
            "wifi-forget",
            &mut app.net.wifi.form,
            &mut fields,
            nav,
            focused,
        );
        widgets::note(
            ui,
            "The deck stops rejoining it. Forgetting the network it is on disconnects it now.",
        );
        out
    });
    match out.activated {
        Some(0) => {
            app.client.request(
                "POST",
                "/v1/wifi/forget",
                Some(json!({"name": name})),
                Tag::WifiForget(name),
            );
            back(app);
        }
        Some(_) => back(app),
        None => cancel_on_b(app, nav, focused, false, false),
    }
}
