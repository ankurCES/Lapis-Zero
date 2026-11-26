//! The seven workspaces. Each one lists its sections for the sidebar and draws the selected
//! section in the content area.

pub mod agent;
pub mod fleet;
pub mod mesh;
pub mod net;
pub mod pty;
pub mod rf;
pub mod sys;
pub mod wifi;

use crate::app::{DeckApp, Nav, AGENT, FLEET, MESH, NET, PTY, RF};

/// One sidebar row.
pub struct Section {
    pub icon: &'static str,
    pub label: String,
    pub sub: String,
    pub badge: Option<String>,
}

impl Section {
    pub fn new(icon: &'static str, label: impl Into<String>) -> Self {
        Section {
            icon,
            label: label.into(),
            sub: String::new(),
            badge: None,
        }
    }
    pub fn detail(mut self, s: impl Into<String>) -> Self {
        self.sub = s.into();
        self
    }
    pub fn badge(mut self, b: Option<String>) -> Self {
        self.badge = b;
        self
    }
}

pub fn sections(app: &DeckApp, ws: usize) -> Vec<Section> {
    match ws {
        AGENT => agent::sections(app),
        RF => rf::sections(app),
        MESH => mesh::sections(app),
        NET => net::sections(app),
        FLEET => fleet::sections(app),
        PTY => pty::sections(app),
        _ => sys::sections(app),
    }
}

/// Keeps the sidebar on the item a workspace tracks by id (chats reorder, terminals open).
pub fn sync(app: &mut DeckApp, ws: usize, moved: bool) {
    match ws {
        AGENT => agent::sync(app, moved),
        FLEET => fleet::sync(app),
        PTY => pty::sync(app, moved),
        _ => {}
    }
}

/// What the sidebar does with X: the selected item's quick action.
pub fn sidebar_x(app: &mut DeckApp, ws: usize, section: usize) {
    match ws {
        FLEET => fleet::probe_section(app, section),
        PTY => pty::close_section(app, section),
        NET => net::run_section(app, section),
        _ => {}
    }
}

pub fn show(
    app: &mut DeckApp,
    ui: &mut egui::Ui,
    ws: usize,
    nav: Nav,
    section: usize,
    focused: bool,
) {
    match ws {
        AGENT => agent::show(app, ui, nav, section, focused),
        RF => rf::show(app, ui, nav, section, focused),
        MESH => mesh::show(app, ui, nav, section, focused),
        NET => net::show(app, ui, nav, section, focused),
        FLEET => fleet::show(app, ui, nav, section, focused),
        PTY => pty::show(app, ui, nav, section, focused),
        _ => sys::show(app, ui, nav, section, focused),
    }
}
