//! [5] FLEET/KVM — the hosts the agent may drive: add one from the tailnet, probe it, start a
//! chat with the deck's agent or a CLI detected on it (Claude Code, opencode, Gemini CLI,
//! Codex…), open SSH, or watch and drive its screen (VNC) with trackball/keyboard passthrough.
//!
//! Sidebar: "Add host", then the configured hosts. Content: the add form, or the host.

use crate::app::{pane, step, DeckApp, Nav, AGENT, FLEET, PTY};
use crate::client::Tag;
use crate::model::Model;
use crate::theme::{AMBER, CRIMSON, GREEN, TEXT};
use crate::widgets::{self, Field, Form};
use crate::ws::Section;
use egui::{Sense, Ui};
use serde_json::{json, Map, Value};
use std::time::{Duration, Instant};

pub const OS: [&str; 3] = ["macos", "linux", "windows"];
pub const POLICY: [&str; 3] = ["strict", "balanced", "autonomous"];

pub struct State {
    pub live: bool,
    pub passthrough: bool,
    pub form: Form,
    pub peer: usize,
    pub id: String,
    pub hostname: String,
    pub ip: String,
    pub os: usize,
    pub user: String,
    pub ssh_port: String,
    pub vnc_port: String,
    pub policy: usize,
    /// The host row the content's action list is on.
    pub action: usize,
    /// "Remove host" was pressed once.
    pub confirm_remove: bool,
    /// After a save, show this host.
    pub go_to: Option<String>,
    last_frame: Instant,
    peers_at: Instant,
}

impl Default for State {
    fn default() -> Self {
        let then = Instant::now()
            .checked_sub(Duration::from_secs(60))
            .unwrap_or_else(Instant::now);
        State {
            live: false,
            passthrough: false,
            form: Form::default(),
            peer: 0,
            id: String::new(),
            hostname: String::new(),
            ip: String::new(),
            os: 0,
            user: String::new(),
            ssh_port: "22".into(),
            vnc_port: "5900".into(),
            policy: 1,
            action: 0,
            confirm_remove: false,
            go_to: None,
            last_frame: then,
            peers_at: then,
        }
    }
}

fn hosts(app: &DeckApp) -> Vec<(String, Value)> {
    // The deck itself is not a KVM target: it has its own workspaces.
    app.m.hosts["hosts"]
        .as_object()
        .map(|h| {
            h.iter()
                .filter(|(k, _)| k.as_str() != "deck")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn peers(app: &DeckApp) -> Vec<Value> {
    app.m
        .tool_results
        .get("net.tailscale_peers")
        .and_then(|r| r["result"]["peers"].as_array().cloned())
        .unwrap_or_default()
}

/// A host id the harness accepts: letters, digits, `-` and `_`, not starting with a digit.
pub fn host_id(name: &str) -> String {
    let base = name.split('.').next().unwrap_or("").to_lowercase();
    let mut id: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    id = id.trim_matches('-').to_owned();
    if id.starts_with(|c: char| c.is_ascii_digit()) {
        id.insert_str(0, "host-");
    }
    id
}

fn os_index(os: &str) -> usize {
    let os = os.to_lowercase();
    if os.contains("mac") || os.contains("darwin") {
        0
    } else if os.contains("windows") {
        2
    } else {
        1
    }
}

fn online(peers: &[Value], h: &Value) -> Option<bool> {
    let ip = Model::str_of(h, "tailscale_ip");
    peers
        .iter()
        .find(|p| p["ip"] == ip || p["hostname"] == Model::str_of(h, "hostname"))
        .map(|p| p["online"] == true)
}

pub fn sections(app: &DeckApp) -> Vec<Section> {
    let peers = peers(app);
    let mut v = vec![Section::new("+", "Add host").detail("from the tailnet or by hand")];
    v.extend(hosts(app).iter().map(|(id, h)| {
        let state = match online(&peers, h) {
            Some(true) => "online",
            Some(false) => "offline",
            None => "?",
        };
        Section::new("🖥", id.clone())
            .detail(format!("{} · {state}", Model::str_of(h, "tailscale_ip")))
    }));
    v
}

pub fn sync(app: &mut DeckApp) {
    if let Some(id) = app.fleet.go_to.clone() {
        if let Some(i) = hosts(app).iter().position(|(k, _)| *k == id) {
            app.section[FLEET] = i + 1;
            app.fleet.go_to = None;
        }
    }
}

pub fn probe_section(app: &mut DeckApp, section: usize) {
    if let Some((id, _)) = section
        .checked_sub(1)
        .and_then(|i| hosts(app).get(i).cloned())
    {
        app.client.request(
            "POST",
            &format!("/v1/hosts/{id}/probe"),
            Some(json!({})),
            Tag::Probe(id.clone()),
        );
        app.m.toast("info", format!("probing {id}…"));
    }
}

/// Sends the whole host map (the deck is built in and never stored).
fn put_hosts(app: &mut DeckApp, map: Map<String, Value>) {
    app.client.request(
        "PUT",
        "/v1/hosts",
        Some(json!({"hosts": map})),
        Tag::HostsSaved,
    );
}

fn stored(app: &DeckApp) -> Map<String, Value> {
    hosts(app)
        .into_iter()
        .map(|(k, mut v)| {
            if let Some(o) = v.as_object_mut() {
                o.remove("builtin");
            }
            (k, v)
        })
        .collect()
}

pub fn show(app: &mut DeckApp, ui: &mut Ui, nav: Nav, section: usize, focused: bool) {
    if app.fleet.peers_at.elapsed() > Duration::from_secs(20) {
        app.fleet.peers_at = Instant::now();
        app.invoke("net.tailscale_peers", json!({}));
    }
    if section == 0 {
        add_form(app, ui, nav, focused);
        return;
    }
    let Some((id, h)) = hosts(app).get(section - 1).cloned() else {
        return;
    };
    let (key_l, key_p) = ui.input(|i| (i.key_pressed(egui::Key::L), i.key_pressed(egui::Key::P)));
    if focused && !nav.typing && !app.fleet.passthrough && key_l {
        app.fleet.live = !app.fleet.live;
    }
    if focused && !nav.typing && key_p && app.fleet.live {
        app.fleet.passthrough = !app.fleet.passthrough;
    }
    if app.fleet.passthrough && nav.b {
        app.fleet.passthrough = false;
        app.grab_b = true;
    }
    if focused && nav.x && !app.fleet.passthrough {
        probe_section(app, section);
    }
    if app.fleet.live && app.fleet.last_frame.elapsed() > Duration::from_millis(700) {
        app.fleet.last_frame = Instant::now();
        app.get(
            &format!("/v1/hosts/{id}/screen.jpg"),
            Tag::HostScreen(id.clone()),
        );
    }
    egui::SidePanel::left("fleet_detail")
        .exact_width(420.0)
        .resizable(false)
        .frame(egui::Frame::NONE.inner_margin(egui::Margin {
            right: 8,
            ..Default::default()
        }))
        .show_inside(ui, |ui| detail(app, ui, nav, focused, &id, &h));
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show_inside(ui, |ui| kvm(app, ui, &id));
}

fn add_form(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    let configured: Vec<String> = hosts(app)
        .iter()
        .map(|(_, h)| Model::str_of(h, "tailscale_ip").to_owned())
        .collect();
    let fresh: Vec<Value> = peers(app)
        .into_iter()
        .filter(|p| !configured.iter().any(|ip| p["ip"] == ip.as_str()))
        .collect();
    let labels: Vec<String> = std::iter::once("type it in".to_owned())
        .chain(fresh.iter().map(|p| {
            format!(
                "{} {} {}",
                if p["online"] == true { "●" } else { "○" },
                Model::str_of(p, "hostname"),
                Model::str_of(p, "ip")
            )
        }))
        .collect();
    let options: Vec<&str> = labels.iter().map(String::as_str).collect();
    let f = &mut app.fleet;
    f.peer = f.peer.min(options.len() - 1);
    let out = pane(ui, "ADD A HOST", focused, |ui| {
        widgets::note(ui, "Pick a machine on your tailnet (◀ ▶) and the fields fill in, or type them. SSH keys or a VNC password are set from the phone app.");
        ui.add_space(4.0);
        let mut fields = [
            Field::Choice {
                label: "TAILNET PEER",
                options: &options,
                index: &mut f.peer,
            },
            Field::Text {
                label: "HOST ID",
                value: &mut f.id,
                hint: "e.g. macbook-pro",
                secret: false,
            },
            Field::Text {
                label: "HOSTNAME",
                value: &mut f.hostname,
                hint: "as Tailscale names it",
                secret: false,
            },
            Field::Text {
                label: "TAILSCALE IP",
                value: &mut f.ip,
                hint: "100.x.y.z or MagicDNS name",
                secret: false,
            },
            Field::Choice {
                label: "OS",
                options: &OS,
                index: &mut f.os,
            },
            Field::Text {
                label: "SSH USER",
                value: &mut f.user,
                hint: "login on that machine",
                secret: false,
            },
            Field::Text {
                label: "SSH PORT",
                value: &mut f.ssh_port,
                hint: "22",
                secret: false,
            },
            Field::Text {
                label: "VNC PORT",
                value: &mut f.vnc_port,
                hint: "5900",
                secret: false,
            },
            Field::Choice {
                label: "POLICY",
                options: &POLICY,
                index: &mut f.policy,
            },
            Field::Action {
                label: "SAVE HOST",
                danger: false,
            },
        ];
        widgets::form(ui, "addhost", &mut f.form, &mut fields, nav, focused)
    });
    app.grab_b |= out.grab_b;
    app.grab_left |= out.grab_left;
    if focused {
        app.hint("▲▼", "field");
        app.hint(
            "A",
            if app.fleet.form.editing {
                "next field"
            } else {
                "edit / save"
            },
        );
        app.hint("◀▶", "change choice");
    }
    if out.changed == Some(0) && app.fleet.peer > 0 {
        if let Some(p) = fresh.get(app.fleet.peer - 1) {
            let name = Model::str_of(p, "hostname");
            app.fleet.id = host_id(name);
            app.fleet.hostname = name.to_owned();
            app.fleet.ip = Model::str_of(p, "ip").to_owned();
            app.fleet.os = os_index(Model::str_of(p, "os"));
        }
    }
    if out.activated == Some(9) {
        save(app);
    }
}

fn save(app: &mut DeckApp) {
    let f = &app.fleet;
    let id = f.id.trim().to_owned();
    if id.is_empty() || host_id(&id) != id {
        app.m.toast(
            "error",
            "Host id: letters, digits and -, not starting with a digit",
        );
        return;
    }
    if id == "deck" {
        app.m
            .toast("error", "\"deck\" is this uConsole; pick another id");
        return;
    }
    if f.ip.trim().is_empty() {
        app.m
            .toast("error", "The Tailscale IP (or MagicDNS name) is needed");
        return;
    }
    let (Ok(ssh), Ok(vnc)) = (
        f.ssh_port.trim().parse::<u16>(),
        f.vnc_port.trim().parse::<u16>(),
    ) else {
        app.m.toast("error", "Ports are numbers (SSH 22, VNC 5900)");
        return;
    };
    let hostname = if f.hostname.trim().is_empty() {
        id.clone()
    } else {
        f.hostname.trim().to_owned()
    };
    let host = json!({
        "hostname": hostname,
        "tailscale_ip": f.ip.trim(),
        "os": OS[f.os],
        "policy": POLICY[f.policy],
        "vnc": {"port": vnc},
        "ssh": {"port": ssh, "user": f.user.trim()},
    });
    let mut map = stored(app);
    map.insert(id.clone(), host);
    put_hosts(app, map);
    app.fleet = State {
        go_to: Some(id),
        peers_at: app.fleet.peers_at,
        ..State::default()
    };
    app.content[FLEET] = false;
}

fn detail(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool, id: &str, h: &Value) {
    if !app.m.agents.contains_key(id) && app.frames % 120 == 3 {
        app.get(&format!("/v1/hosts/{id}/agents"), Tag::Agents(id.into()));
    }
    let mut agents = vec![json!({"id": "lapis", "name": "Lapis agent (VNC/SSH)"})];
    agents.extend(app.m.agents.get(id).cloned().unwrap_or_default());
    let mut rows: Vec<(String, &'static str)> = agents
        .iter()
        .map(|a| (format!("Chat with {}", Model::str_of(a, "name")), "chat"))
        .collect();
    rows.push(("SSH terminal".into(), "ssh"));
    rows.push((
        format!("Live screen: {}", if app.fleet.live { "ON" } else { "OFF" }),
        "live",
    ));
    if app.fleet.live {
        rows.push((
            format!(
                "Passthrough: {}",
                if app.fleet.passthrough { "ON" } else { "OFF" }
            ),
            "pass",
        ));
    }
    rows.push(("Probe connection".into(), "probe"));
    rows.push((
        if app.fleet.confirm_remove {
            "Remove host: press A again".to_owned()
        } else {
            "Remove host".to_owned()
        },
        "remove",
    ));
    if focused && !app.fleet.passthrough {
        let before = app.fleet.action;
        step(&mut app.fleet.action, rows.len(), nav);
        if before != app.fleet.action {
            app.fleet.confirm_remove = false;
        }
        app.hint("▲▼", "action");
        app.hint("A", "run");
        app.hint("X", "probe");
    } else if app.fleet.passthrough {
        app.hint("B", "release passthrough");
    }
    let sel = app.fleet.action.min(rows.len() - 1);
    let mut run = (focused && nav.a && !app.fleet.passthrough).then_some(sel);
    pane(ui, id, focused, |ui| {
        widgets::field(ui, "hostname", Model::str_of(h, "hostname"));
        widgets::field(ui, "tailscale", Model::str_of(h, "tailscale_ip"));
        widgets::field(
            ui,
            "ssh",
            format!(
                "{}@ :{}",
                Model::str_of(&h["ssh"], "user"),
                h["ssh"]["port"]
            ),
        );
        widgets::field(
            ui,
            "vnc / policy",
            format!(":{} · {}", h["vnc"]["port"], Model::str_of(h, "policy")),
        );
        if let Some(p) = app.m.probes.get(id) {
            ui.horizontal(|ui| {
                for k in ["tailscale", "vnc", "ssh"] {
                    let ok = p[k]["ok"] == true;
                    widgets::chip(
                        ui,
                        &format!("{k} {}", if ok { "OK" } else { "ERR" }),
                        if ok { GREEN } else { CRIMSON },
                    );
                }
            });
        }
        ui.add_space(4.0);
        for (i, (label, kind)) in rows.iter().enumerate() {
            let color = if *kind == "remove" { CRIMSON } else { TEXT };
            if widgets::list_row(ui, label, focused && i == sel, color).clicked() {
                run = Some(i);
            }
        }
    });
    let Some(i) = run else {
        return;
    };
    match rows[i].1 {
        "chat" => {
            let agent = agents
                .get(i)
                .map(|a| Model::str_of(a, "id").to_owned())
                .unwrap_or_else(|| "lapis".into());
            let mut body = json!({"host_id": id, "title": format!("{agent} on {id}")});
            if agent != "lapis" {
                body["agent"] = json!(agent);
            }
            app.client
                .request("POST", "/v1/chats", Some(body), Tag::NewChat);
            app.ws = AGENT;
        }
        "ssh" => {
            let user = Model::str_of(&h["ssh"], "user");
            let host = Model::str_of(h, "tailscale_ip");
            let target = if user.is_empty() {
                host.to_owned()
            } else {
                format!("{user}@{host}")
            };
            let port = h["ssh"]["port"].as_u64().unwrap_or(22).to_string();
            app.terms.open_command(
                &format!("ssh {id}"),
                vec![
                    "ssh".into(),
                    "-o".into(),
                    "StrictHostKeyChecking=accept-new".into(),
                    "-p".into(),
                    port,
                    target,
                ],
            );
            app.ws = PTY;
            app.content[PTY] = true;
        }
        "live" => app.fleet.live = !app.fleet.live,
        "pass" => app.fleet.passthrough = !app.fleet.passthrough,
        "probe" => {
            app.client.request(
                "POST",
                &format!("/v1/hosts/{id}/probe"),
                Some(json!({})),
                Tag::Probe(id.to_owned()),
            );
        }
        _ if !app.fleet.confirm_remove => app.fleet.confirm_remove = true,
        _ => {
            let mut map = stored(app);
            map.remove(id);
            put_hosts(app, map);
            app.fleet.confirm_remove = false;
            app.content[FLEET] = false;
            app.section[FLEET] = 0;
        }
    }
}

fn kvm(app: &mut DeckApp, ui: &mut Ui, id: &str) {
    let title = if app.fleet.passthrough {
        "SCREEN · PASSTHROUGH (B releases)"
    } else {
        "SCREEN"
    };
    pane(ui, title, app.fleet.passthrough, |ui| {
        let Some(img) = app.m.host_screens.remove(id) else {
            widgets::note(
                ui,
                if app.fleet.live {
                    "waiting for the first frame…"
                } else {
                    "Live screen is off: choose \"Live screen\" on the left (or press L)."
                },
            );
            return;
        };
        let tex = app.texture(ui.ctx(), &format!("kvm-{id}"), &img);
        let remote = img.remote.unwrap_or((img.width as u32, img.height as u32));
        app.m.host_screens.insert(id.to_owned(), img);
        let size = tex.size_vec2();
        let avail = ui.available_size();
        let scale = (avail.x / size.x).min(avail.y / size.y).min(2.0);
        let resp = ui.add(
            egui::Image::new(&tex)
                .fit_to_exact_size(size * scale)
                .sense(Sense::click()),
        );
        if !app.fleet.passthrough {
            return;
        }
        ui.painter().rect_stroke(
            resp.rect,
            0.0,
            egui::Stroke::new(2.0_f32, AMBER),
            egui::StrokeKind::Outside,
        );
        let to_remote = |p: egui::Pos2| {
            let fx = (p.x - resp.rect.min.x) / resp.rect.width();
            let fy = (p.y - resp.rect.min.y) / resp.rect.height();
            ((fx * remote.0 as f32) as i64, (fy * remote.1 as f32) as i64)
        };
        if let Some(pos) = resp.interact_pointer_pos() {
            if resp.clicked() || resp.secondary_clicked() {
                let (x, y) = to_remote(pos);
                let button = if resp.secondary_clicked() {
                    "right"
                } else {
                    "left"
                };
                app.client.request(
                    "POST",
                    &format!("/v1/hosts/{id}/input"),
                    Some(json!({"action": "click", "x": x, "y": y, "button": button})),
                    Tag::Ignore,
                );
            }
        }
        let (text, keys) = ui.input(|i| {
            let text: String = i
                .events
                .iter()
                .filter_map(|e| {
                    if let egui::Event::Text(t) = e {
                        Some(t.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            let keys: Vec<&'static str> = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        pressed: true,
                        ..
                    } => Some("Return"),
                    egui::Event::Key {
                        key: egui::Key::Backspace,
                        pressed: true,
                        ..
                    } => Some("BackSpace"),
                    egui::Event::Key {
                        key: egui::Key::Tab,
                        pressed: true,
                        ..
                    } => Some("Tab"),
                    _ => None,
                })
                .collect();
            (text, keys)
        });
        if !text.is_empty() {
            app.client.request(
                "POST",
                &format!("/v1/hosts/{id}/input"),
                Some(json!({"action": "type", "text": text})),
                Tag::Ignore,
            );
        }
        for k in keys {
            app.client.request(
                "POST",
                &format!("/v1/hosts/{id}/input"),
                Some(json!({"action": "key", "combo": k})),
                Tag::Ignore,
            );
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_ids() {
        assert_eq!(super::host_id("MacBook-Pro.tail1234.ts.net"), "macbook-pro");
        assert_eq!(super::host_id("9lives box"), "host-9lives-box");
        assert_eq!(super::os_index("macOS"), 0);
        assert_eq!(super::os_index("windows"), 2);
        assert_eq!(super::os_index("linux"), 1);
    }
}
