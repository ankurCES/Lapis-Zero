//! [4] NETOPS — the deck's own Wi-Fi (`ws/wifi.rs`), Tailscale peers, passive Wi-Fi and BLE
//! surveys, port probes, packet capture and DNS. Active tools go through the Policy Gate and
//! ask `[Y/N]` here on the deck.

use crate::app::{pane, DeckApp, Nav};
use crate::model::Model;
use crate::theme;
use crate::theme::{AMBER, DIM, GREEN, TEXT};
use crate::widgets::{self, Field, Form, FormOut};
use crate::ws::Section;
use egui::{RichText, Ui};
use serde_json::{json, Value};

pub struct State {
    pub target: String,
    pub ports: String,
    pub iface: String,
    pub bpf: String,
    pub count: String,
    pub dns: String,
    pub forms: [Form; 3],
    pub wifi: crate::ws::wifi::State,
}

impl Default for State {
    fn default() -> Self {
        State {
            target: String::new(),
            ports: "22,80,443,5900,8421".into(),
            iface: "wlan0".into(),
            bpf: String::new(),
            count: "50".into(),
            dns: String::new(),
            forms: [Form::default(); 3],
            wifi: Default::default(),
        }
    }
}

pub fn parse_ports(s: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<u16>(), b.trim().parse::<u16>()) {
                    out.extend((a..=b.min(a.saturating_add(63))).take(64));
                }
            }
            None => out.extend(part.parse::<u16>().ok()),
        }
    }
    out.truncate(64);
    out
}

fn result_rows(app: &DeckApp, tool: &str, key: &str) -> Vec<Value> {
    app.m
        .tool_results
        .get(tool)
        .and_then(|r| r["result"][key].as_array().cloned())
        .unwrap_or_default()
}

pub fn sections(app: &DeckApp) -> Vec<Section> {
    let n = |tool: &str, key: &str| result_rows(app, tool, key).len();
    vec![
        Section::new("📡", "Wi-Fi").detail(crate::ws::wifi::detail(app)),
        Section::new("🔗", "Tailscale peers")
            .detail(format!("{} peers", n("net.tailscale_peers", "peers"))),
        Section::new("📶", "Wi-Fi survey").detail(format!(
            "{} networks · passive",
            n("net.wifi_scan_passive", "networks")
        )),
        Section::new("📱", "BLE survey")
            .detail(format!("{} devices", n("net.ble_scan_passive", "devices"))),
        Section::new("⌖", "Port probe").detail("active · asks Y/N"),
        Section::new("⏺", "Packet capture").detail("tcpdump · asks Y/N"),
        Section::new("🔍", "DNS lookup").detail(app.net.dns.clone()),
    ]
}

/// X in the sidebar: refresh or run the selected tool.
pub fn run_section(app: &mut DeckApp, section: usize) {
    match section {
        0 => crate::ws::wifi::scan(app),
        1 => app.invoke("net.tailscale_peers", json!({})),
        2 => app.invoke("net.wifi_scan_passive", json!({})),
        3 => app.invoke("net.ble_scan_passive", json!({"duration_s": 8})),
        4 => probe(app),
        5 => capture(app),
        _ => dns(app),
    }
}

fn probe(app: &mut DeckApp) {
    if app.net.target.trim().is_empty() {
        app.m.toast("warn", "Type a target first");
        return;
    }
    let ports = parse_ports(&app.net.ports);
    let target = app.net.target.trim().to_owned();
    app.invoke("net.port_probe", json!({"target": target, "ports": ports}));
}

fn capture(app: &mut DeckApp) {
    let count = app.net.count.parse::<u64>().unwrap_or(50);
    let (iface, bpf) = (app.net.iface.clone(), app.net.bpf.clone());
    app.invoke(
        "net.packet_capture",
        json!({"iface": iface, "bpf": bpf, "count": count}),
    );
}

fn dns(app: &mut DeckApp) {
    if !app.net.dns.trim().is_empty() {
        let name = app.net.dns.trim().to_owned();
        app.invoke("net.dns_lookup", json!({"name": name}));
    }
}

fn row(ui: &mut Ui, text: String, color: egui::Color32) {
    ui.label(RichText::new(text).font(theme::mono(15.0)).color(color));
}

pub fn show(app: &mut DeckApp, ui: &mut Ui, nav: Nav, section: usize, focused: bool) {
    if section == 0 {
        return crate::ws::wifi::show(app, ui, nav, focused);
    }
    let section = section - 1;
    let run = focused && nav.x;
    if focused && section < 3 {
        app.hint("X", "refresh");
    }
    match section {
        0 => {
            if run
                || (!app.m.tool_results.contains_key("net.tailscale_peers") && app.frames % 90 == 2)
            {
                app.invoke("net.tailscale_peers", json!({}));
            }
            pane(ui, "TAILSCALE PEERS", focused, |ui| {
                row(
                    ui,
                    format!("{:<24} {:<16} {:<9} {}", "HOST", "IP", "OS", "STATE"),
                    DIM,
                );
                for p in result_rows(app, "net.tailscale_peers", "peers") {
                    let online = p["online"] == true;
                    row(
                        ui,
                        format!(
                            "{:<24} {:<16} {:<9} {}",
                            Model::str_of(&p, "hostname"),
                            Model::str_of(&p, "ip"),
                            Model::str_of(&p, "os"),
                            if online { "online" } else { "offline" }
                        ),
                        if online { GREEN } else { DIM },
                    );
                }
                ui.add_space(6.0);
                widgets::note(
                    ui,
                    "To let the agent drive one of these, add it in FLEET › Add host.",
                );
            });
        }
        1 => {
            if run {
                app.invoke("net.wifi_scan_passive", json!({}));
            }
            pane(ui, "WI-FI SURVEY (passive, cached scan)", focused, |ui| {
                row(
                    ui,
                    format!(
                        "{:<24} {:<18} {:>4} {:>7} {:>5}  {}",
                        "SSID", "BSSID", "CH", "BAND", "dBm", "SECURITY"
                    ),
                    DIM,
                );
                egui::ScrollArea::vertical()
                    .id_salt("wifi")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for n in result_rows(app, "net.wifi_scan_passive", "networks") {
                            let rssi = n["rssi_dbm"].as_i64().unwrap_or(-100);
                            row(
                                ui,
                                format!(
                                    "{:<24} {:<18} {:>4} {:>7} {:>5}  {}",
                                    Model::str_of(&n, "ssid"),
                                    Model::str_of(&n, "bssid"),
                                    n["channel"],
                                    Model::str_of(&n, "band"),
                                    rssi,
                                    Model::str_of(&n, "security")
                                ),
                                if rssi > -60 {
                                    GREEN
                                } else if rssi > -75 {
                                    TEXT
                                } else {
                                    DIM
                                },
                            );
                        }
                    });
            });
        }
        2 => {
            if run {
                app.invoke("net.ble_scan_passive", json!({"duration_s": 8}));
            }
            pane(ui, "BLE SURVEY · X scans 8 s", focused, |ui| {
                for d in result_rows(app, "net.ble_scan_passive", "devices") {
                    row(
                        ui,
                        format!(
                            "{:<18} {:>5} dBm  {}",
                            Model::str_of(&d, "address"),
                            d["rssi_dbm"],
                            d["name"].as_str().unwrap_or("(no name)")
                        ),
                        TEXT,
                    );
                }
            });
        }
        3 => {
            let out = pane(
                ui,
                "PORT PROBE (active: the deck asks Y/N)",
                focused,
                |ui| {
                    let n = &mut app.net;
                    let mut fields = [
                        Field::Text {
                            label: "TARGET",
                            value: &mut n.target,
                            hint: "host or IP",
                            secret: false,
                        },
                        Field::Text {
                            label: "PORTS",
                            value: &mut n.ports,
                            hint: "22,80,8000-8010",
                            secret: false,
                        },
                        Field::Action {
                            label: "RUN PROBE",
                            danger: false,
                        },
                    ];
                    let out =
                        widgets::form(ui, "probe", &mut n.forms[0], &mut fields, nav, focused);
                    if let Some(r) = app.m.tool_results.get("net.port_probe") {
                        row(
                            ui,
                            format!(
                                "{} ({})",
                                Model::str_of(&r["result"], "target"),
                                Model::str_of(&r["result"], "ip")
                            ),
                            DIM,
                        );
                        for p in r["result"]["ports"].as_array().cloned().unwrap_or_default() {
                            let st = Model::str_of(&p, "state");
                            row(
                                ui,
                                format!(
                                    "{:>5}/tcp {:<9} {:<10} {} ms",
                                    p["port"],
                                    st,
                                    Model::str_of(&p, "service"),
                                    p["ms"]
                                ),
                                match st {
                                    "open" => GREEN,
                                    "closed" => TEXT,
                                    _ => AMBER,
                                },
                            );
                        }
                    }
                    out
                },
            );
            form_done(app, out, focused, run, probe);
        }
        4 => {
            let out = pane(
                ui,
                "PACKET CAPTURE (tcpdump; the deck asks Y/N)",
                focused,
                |ui| {
                    let n = &mut app.net;
                    let mut fields = [
                        Field::Text {
                            label: "INTERFACE",
                            value: &mut n.iface,
                            hint: "wlan0",
                            secret: false,
                        },
                        Field::Text {
                            label: "BPF FILTER",
                            value: &mut n.bpf,
                            hint: "e.g. port 53",
                            secret: false,
                        },
                        Field::Text {
                            label: "PACKETS",
                            value: &mut n.count,
                            hint: "50",
                            secret: false,
                        },
                        Field::Action {
                            label: "CAPTURE",
                            danger: false,
                        },
                    ];
                    let out =
                        widgets::form(ui, "capture", &mut n.forms[1], &mut fields, nav, focused);
                    egui::ScrollArea::vertical()
                        .id_salt("cap")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for l in result_rows(app, "net.packet_capture", "lines") {
                                row(ui, l.as_str().unwrap_or("").to_owned(), TEXT);
                            }
                        });
                    out
                },
            );
            form_done(app, out, focused, run, capture);
        }
        _ => {
            let out = pane(ui, "DNS LOOKUP", focused, |ui| {
                let n = &mut app.net;
                let mut fields = [
                    Field::Text {
                        label: "NAME",
                        value: &mut n.dns,
                        hint: "host name",
                        secret: false,
                    },
                    Field::Action {
                        label: "LOOK UP",
                        danger: false,
                    },
                ];
                let out = widgets::form(ui, "dns", &mut n.forms[2], &mut fields, nav, focused);
                if let Some(r) = app.m.tool_results.get("net.dns_lookup") {
                    row(
                        ui,
                        serde_json::to_string_pretty(&r["result"]).unwrap_or_default(),
                        TEXT,
                    );
                }
                out
            });
            form_done(app, out, focused, run, dns);
        }
    }
}

fn form_done(app: &mut DeckApp, out: FormOut, focused: bool, run: bool, go: fn(&mut DeckApp)) {
    app.grab_b |= out.grab_b;
    app.grab_left |= out.grab_left;
    if focused {
        app.hint("▲▼", "field");
        app.hint("A", "edit / run");
        app.hint("X", "run");
    }
    if out.activated.is_some() || run {
        go(app);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ports() {
        assert_eq!(
            super::parse_ports("22, 80,8000-8002,x"),
            vec![22, 80, 8000, 8001, 8002]
        );
        assert_eq!(super::parse_ports("1-1000").len(), 64);
    }
}
