//! [1] AGENT — the command center, as in the Android app: chats per host (the deck itself or
//! any Tailscale machine), each answered by the deck's agent or a CLI detected on the host;
//! the streaming conversation with its tool trace; the artifact viewer.
//!
//! Sidebar: "New chat", then the chats. Content: the host → agent picker, or the conversation.

use crate::app::{pane, step, DeckApp, Nav, AGENT};
use crate::character;
use crate::client::Tag;
use crate::model::Model;
use crate::theme::{self, AMBER, CRIMSON, CYAN, DIM, FAINT, GREEN, TEXT};
use crate::widgets;
use crate::ws::Section;
use egui::{RichText, Ui, Vec2};
use serde_json::{json, Value};

#[derive(Default)]
pub struct State {
    pub selected: Option<String>,
    pub input: String,
    /// New chat picker: host index, agent index, choosing the agent (not the host).
    pub pick: (usize, usize, bool),
}

pub fn sections(app: &DeckApp) -> Vec<Section> {
    let mut v = vec![Section::new("+", "New chat").detail("on the deck or a fleet host")];
    v.extend(app.m.chats.iter().map(|c| {
        let title = Model::str_of(c, "title");
        let agent = c["agent"].as_str().unwrap_or("lapis");
        Section::new("💬", if title.is_empty() { "untitled" } else { title })
            .detail(format!("{} · {agent}", Model::str_of(c, "host_id")))
            .badge((c["active"] == true).then(|| "LIVE".to_owned()))
    }));
    v
}

/// The sidebar row follows the chat by id (the list reorders as chats update).
pub fn sync(app: &mut DeckApp, moved: bool) {
    let s = app.section[AGENT];
    if moved {
        app.agent.selected = match s {
            0 => None,
            i => app
                .m
                .chats
                .get(i - 1)
                .and_then(|c| c["id"].as_str())
                .map(str::to_owned),
        };
        if let Some(id) = app.agent.selected.clone() {
            app.get(&format!("/v1/chats/{id}?limit=50"), Tag::Chat(id));
        }
    } else if let Some(id) = &app.agent.selected {
        if let Some(i) = app.m.chats.iter().position(|c| c["id"] == id.as_str()) {
            app.section[AGENT] = i + 1;
        }
    }
}

pub fn show(app: &mut DeckApp, ui: &mut Ui, nav: Nav, section: usize, focused: bool) {
    if section == 0 || app.agent.selected.is_none() {
        new_chat(app, ui, nav, focused);
        return;
    }
    egui::SidePanel::right("agent_artifact")
        .exact_width(320.0)
        .resizable(false)
        .frame(egui::Frame::NONE.inner_margin(egui::Margin {
            left: 8,
            ..Default::default()
        }))
        .show_inside(ui, |ui| artifact(app, ui));
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show_inside(ui, |ui| thread(app, ui, nav, focused));
}

fn new_chat(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    let mut hosts = app.m.host_ids();
    hosts.sort_by_key(|h| h != "deck"); // the deck first
    let (mut h, mut a, mut picking) = app.agent.pick;
    h = h.min(hosts.len().saturating_sub(1));
    let host = hosts.get(h).cloned().unwrap_or_else(|| "deck".into());
    if !app.m.agents.contains_key(&host) && host != "deck" && app.frames % 60 == 1 {
        app.get(
            &format!("/v1/hosts/{host}/agents"),
            Tag::Agents(host.clone()),
        );
    }
    let mut agents = vec![
        json!({"id": "lapis", "name": "Lapis agent", "path": "this deck's own agent (SSH / VNC tools)"}),
    ];
    agents.extend(app.m.agents.get(&host).cloned().unwrap_or_default());
    a = a.min(agents.len() - 1);
    let mut create = None;
    if focused {
        if picking {
            step(&mut a, agents.len(), nav);
            if nav.b || nav.left {
                picking = false;
                app.grab_b = true;
                app.grab_left = true;
            }
            if nav.a || nav.right {
                create = agents.get(a).map(|v| Model::str_of(v, "id").to_owned());
            }
            app.hint("▲▼", "agent");
            app.hint("A", "start chat");
            app.hint("B", "back to hosts");
        } else {
            step(&mut h, hosts.len(), nav);
            if nav.a || nav.right {
                picking = true;
                a = 0;
            }
            app.hint("▲▼", "host");
            app.hint("A", "choose agent");
        }
    }
    ui.horizontal_top(|ui| {
        let (face, _) = ui.allocate_exact_size(Vec2::new(180.0, 180.0), egui::Sense::hover());
        character::draw(ui.painter(), face, app.mood(), app.clock());
        ui.vertical(|ui| {
            ui.add_space(20.0);
            ui.label(RichText::new("COMMAND CENTER").font(theme::mono(26.0)).color(CYAN));
            ui.label(RichText::new("Start a chat on the deck itself or on any Tailscale machine. It is answered by this deck's agent, or by a CLI found on that host (Claude Code, Codex, Gemini…).").font(theme::sans(16.0)).color(DIM));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                widgets::hint(ui, "A", "pick");
                widgets::hint(ui, "Y", "or type a question in commands");
            });
        });
    });
    ui.add_space(6.0);
    ui.columns(2, |cols| {
        pane(&mut cols[0], "1 · HOST", focused && !picking, |ui| {
            for (i, id) in hosts.iter().enumerate() {
                let sub = if id == "deck" {
                    "this uConsole".to_owned()
                } else {
                    Model::str_of(&app.m.hosts["hosts"][id], "tailscale_ip").to_owned()
                };
                widgets::nav_row(
                    ui,
                    if id == "deck" { "🖳" } else { "🖥" },
                    id,
                    &sub,
                    i == h,
                    focused && !picking,
                    None,
                );
            }
            if hosts.len() == 1 {
                widgets::note(
                    ui,
                    "Only the deck so far. Add machines in FLEET › Add host.",
                );
            }
        });
        pane(
            &mut cols[1],
            &format!("2 · AGENT ON {host}"),
            focused && picking,
            |ui| {
                for (j, ag) in agents.iter().enumerate() {
                    let r = widgets::nav_row(
                        ui,
                        "🛠",
                        Model::str_of(ag, "name"),
                        Model::str_of(ag, "path"),
                        picking && j == a,
                        focused && picking,
                        None,
                    );
                    if r.clicked() {
                        create = Some(Model::str_of(ag, "id").to_owned());
                    }
                }
                if host != "deck" && !app.m.agents.contains_key(&host) {
                    widgets::note(ui, "looking for CLIs on the host over SSH…");
                }
            },
        );
    });
    if let Some(agent) = create {
        let mut body = json!({"host_id": host});
        if agent != "lapis" {
            body["agent"] = json!(agent);
        }
        app.client
            .request("POST", "/v1/chats", Some(body), Tag::NewChat);
        picking = false;
    }
    app.agent.pick = (h, a, picking);
}

fn role_color(role: &str, status: &str) -> egui::Color32 {
    match (role, status) {
        ("user", _) => CYAN,
        ("tool", "denied") => CRIMSON,
        ("tool", "error") => AMBER,
        ("tool", _) => DIM,
        ("system", _) => AMBER,
        _ => TEXT,
    }
}

fn thread(app: &mut DeckApp, ui: &mut Ui, nav: Nav, focused: bool) {
    let Some(id) = app.agent.selected.clone() else {
        return;
    };
    let chat = app
        .m
        .chats
        .iter()
        .find(|c| c["id"] == id.as_str())
        .cloned()
        .unwrap_or(Value::Null);
    let msgs = app.m.messages.get(&id).cloned().unwrap_or_default();
    let title = format!(
        "{} · {}",
        Model::str_of(&chat, "host_id"),
        chat["agent"].as_str().unwrap_or("lapis")
    );
    if focused {
        app.hint("⌨", "type, Enter sends");
        app.hint("B", "back to chats");
    }
    pane(ui, &title, focused, |ui| {
        let h = ui.available_height() - 46.0;
        egui::ScrollArea::vertical()
            .id_salt("thread")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .max_height(h)
            .show(ui, |ui| {
                for m in &msgs {
                    let role = Model::str_of(m, "role");
                    let status = m["metadata"]["status"].as_str().unwrap_or("");
                    let content = Model::str_of(m, "content");
                    if role == "tool" {
                        let kind = m["metadata"]["kind"].as_str().unwrap_or("tool");
                        ui.label(
                            RichText::new(format!(
                                "  ▸ {kind:<10} {} {}",
                                content.lines().next().unwrap_or(""),
                                if status.is_empty() {
                                    String::new()
                                } else {
                                    format!("[{status}]")
                                }
                            ))
                            .font(theme::mono(14.0))
                            .color(role_color(role, status)),
                        );
                    } else {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(match role {
                                "user" => "YOU",
                                "system" => "SYSTEM",
                                _ => "AGENT",
                            })
                            .font(theme::mono(13.0))
                            .color(role_color(role, status)),
                        );
                        ui.label(
                            RichText::new(content)
                                .font(theme::sans(17.0))
                                .color(if role == "user" { CYAN } else { TEXT }),
                        );
                    }
                }
                if chat["active"] == true {
                    ui.label(
                        RichText::new(format!(
                            "… {}",
                            app.m.state["agent_state"].as_str().unwrap_or("working")
                        ))
                        .font(theme::mono(15.0))
                        .color(GREEN),
                    );
                }
                if msgs.is_empty() {
                    widgets::note(ui, "No messages yet: say what you want done.");
                }
            });
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut app.agent.input)
                    .hint_text("message (Enter sends)")
                    .font(theme::mono(16.0))
                    .desired_width(ui.available_width() - 170.0),
            );
            if focused && (app.entered || (nav.a && !nav.typing)) {
                r.request_focus();
            }
            if r.has_focus() && nav.b {
                r.surrender_focus();
            }
            let send = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (send || ui.button("SEND").clicked()) && !app.agent.input.trim().is_empty() {
                let text = std::mem::take(&mut app.agent.input);
                app.send_prompt(&id, text.trim());
                r.request_focus();
            } else if send {
                // Enter on an empty line keeps the cursor here.
                r.request_focus();
            }
            if ui.button(RichText::new("STOP").color(AMBER)).clicked() {
                app.client.request(
                    "POST",
                    &format!("/v1/chats/{id}/stop"),
                    Some(json!({})),
                    Tag::Ignore,
                );
            }
        });
    });
}

fn artifact(app: &mut DeckApp, ui: &mut Ui) {
    let shot = app
        .agent
        .selected
        .as_ref()
        .and_then(|id| app.m.messages.get(id))
        .and_then(|msgs| {
            msgs.iter()
                .rev()
                .find_map(|m| m["metadata"]["screenshot_id"].as_i64())
        });
    pane(ui, "ARTIFACTS", false, |ui| {
        if let Some(sid) = shot {
            if let Some(img) = app.m.screenshots.remove(&sid) {
                let tex = app.texture(ui.ctx(), &format!("shot{sid}"), &img);
                app.m.screenshots.insert(sid, img);
                let size = tex.size_vec2();
                let scale = (ui.available_width() / size.x).min(1.0);
                ui.add(egui::Image::new(&tex).fit_to_exact_size(size * scale));
            } else if app.frames % 20 == 1 {
                app.get(&format!("/v1/screenshots/{sid}.jpg"), Tag::Screenshot(sid));
            }
            ui.separator();
        }
        egui::ScrollArea::vertical()
            .id_salt("artifact")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (name, v) in app.m.tool_results.iter().rev().take(3) {
                    ui.label(RichText::new(name).font(theme::mono(13.0)).color(CYAN));
                    let text = serde_json::to_string_pretty(&v["result"]).unwrap_or_default();
                    ui.label(
                        RichText::new(text.chars().take(700).collect::<String>())
                            .font(theme::mono(12.0))
                            .color(TEXT),
                    );
                }
                ui.label(
                    RichText::new("ACTIVITY")
                        .font(theme::mono(13.0))
                        .color(FAINT),
                );
                for a in app.m.activity.iter().take(20) {
                    ui.label(
                        RichText::new(format!(
                            "{} {} {}",
                            Model::str_of(a, "kind"),
                            Model::str_of(a, "summary"),
                            Model::str_of(a, "status")
                        ))
                        .font(theme::sans(13.0))
                        .color(DIM),
                    );
                }
            });
    });
}
