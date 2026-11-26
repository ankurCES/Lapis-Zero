//! The HUD without a GPU: every workspace renders from live-shaped data, and the keyboard and
//! gamepad do what the bottom bar says (approvals, palette, kill switch, workspace switching).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use base64::Engine;
use egui::{Event, Key, Modifiers, RawInput};
use lapis_ui::app::DeckApp;
use lapis_ui::client::{Client, Inbound, Tag};
use lapis_ui::input::{Pad, PadEvent};
use serde_json::{json, Value};

fn raw(events: Vec<Event>) -> RawInput {
    RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 720.0),
        )),
        events,
        ..Default::default()
    }
}

fn key(k: Key, m: Modifiers) -> Event {
    Event::Key {
        key: k,
        physical_key: Some(k),
        pressed: true,
        repeat: false,
        modifiers: m,
    }
}

/// Runs one frame; returns how many shapes it drew (tessellated too, so painting is exercised).
fn frame(ctx: &egui::Context, app: &mut DeckApp, events: Vec<Event>, pads: &[PadEvent]) -> usize {
    let mut input = raw(events);
    // egui reads held modifiers from the raw input, as the KMS backend sets them.
    input.modifiers = input
        .events
        .iter()
        .find_map(|e| {
            if let Event::Key { modifiers, .. } = e {
                Some(*modifiers)
            } else {
                None
            }
        })
        .unwrap_or_default();
    let out = ctx.run(input, |ctx| app.frame(ctx, pads));
    let n = out.shapes.len();
    assert!(!ctx.tessellate(out.shapes, out.pixels_per_point).is_empty());
    n
}

fn sent(app: &DeckApp) -> Vec<Value> {
    app.client.record.as_ref().unwrap().lock().unwrap().clone()
}

fn http(client: &Client, tag: Tag, v: Value) {
    client.inject(Inbound::Http {
        tag,
        status: 200,
        body: v.to_string().into_bytes(),
        headers: vec![],
    });
}

fn seeded() -> DeckApp {
    let client = Client::recorder();
    client.inject(Inbound::Connected(true));
    client.inject(Inbound::Frame(json!({"type": "state_sync", "agent_state": "ACTING", "battery_pct": 78, "battery_charging": false,
        "wifi_ssid": "lapis-lab", "tailscale_state": "connected", "tailscale_fqdn": "lapis-deck.tail1234.ts.net", "pending_approvals": [],
        "ui": {"theme": "dark", "brightness": 80}, "device": {"hostname": "lapis-deck", "version": "0.1.0", "mock": true},
        "deck": {"harness": "up", "battery_w": 3.2, "rails": [{"name": "gps", "on": true, "switchable": true}, {"name": "wifi", "on": true, "switchable": true}]},
        "pairing": {"mode": "qr", "payload": "lapis://pair?host=lapis-deck.tail1234.ts.net&nonce=abc&pin=123456", "pin": "123456", "host": "x", "expires_in": 180}})));
    client.inject(Inbound::Frame(
        json!({"type": "telemetry_tick", "telemetry": {"cpu": {"temp_c": 52.0, "load1": 0.4}}}),
    ));
    http(
        &client,
        Tag::Chats,
        json!({"chats": [{"id": "c1", "title": "deploy", "host_id": "macbook-pro", "agent": "claude", "updated_at": 2, "active": true},
                                               {"id": "c2", "title": "Deck operator", "host_id": "deck", "updated_at": 1}]}),
    );
    http(
        &client,
        Tag::Chat("c1".into()),
        json!({"chat": {"id": "c1"}, "messages": [{"id": 1, "role": "user", "content": "run the tests"},
        {"id": 2, "role": "tool", "content": "make test", "metadata": {"kind": "ssh_exec", "status": "ok"}}, {"id": 3, "role": "assistant", "content": "All 42 tests pass."}]}),
    );
    http(
        &client,
        Tag::Hosts,
        json!({"hosts": {"macbook-pro": {"hostname": "mbp", "tailscale_ip": "100.88.1.10", "os": "macos", "policy": "balanced", "vnc": {"port": 5900}, "ssh": {"port": 22, "user": "me"}}}}),
    );
    http(
        &client,
        Tag::Agents("macbook-pro".into()),
        json!({"agents": [{"id": "claude", "name": "Claude Code", "path": "/usr/local/bin/claude"}, {"id": "codex", "name": "Codex", "path": "/opt/bin/codex"}]}),
    );
    http(
        &client,
        Tag::Mesh,
        json!({"status": "connected", "nodes": [{"id": "!a1b2c3d4", "long_name": "Base Camp", "short_name": "BASE", "snr": 9.5, "rssi": -71, "hops": 0, "lat": 37.78, "lon": -122.41}],
        "messages": [{"id": 1, "from": "!a1b2c3d4", "text": "net check", "outgoing": false}]}),
    );
    http(
        &client,
        Tag::Gps,
        json!({"status": "connected", "fix": {"fix": "3D", "lat": 37.7749, "lon": -122.4194, "alt_m": 16.0}, "used": 7, "visible": 9, "pps_locked": true, "pps_offset_ns": 120,
        "satellites": [{"prn": 2, "el": 61, "az": 288, "snr": 41, "used": true}]}),
    );
    let bins = base64::engine::general_purpose::STANDARD
        .encode((0..512).map(|i| (i % 256) as u8).collect::<Vec<u8>>());
    for _ in 0..5 {
        client.inject(Inbound::Frame(json!({"type": "sdr_spectrum", "center_hz": 100_000_000u64, "sample_rate": 2_048_000, "gain_db": 30.0, "bins": bins,
            "floor_db": -90.0, "ceil_db": -20.0, "synthetic": true, "source": "synthetic", "peaks": [{"freq_hz": 100_100_000.0, "power_db": -22.0, "snr_db": 40.0}]})));
    }
    DeckApp::new(client)
}

#[test]
fn every_workspace_renders() {
    let ctx = egui::Context::default();
    let mut app = seeded();
    frame(&ctx, &mut app, vec![], &[]);
    assert_eq!(
        app.ws, 6,
        "a fresh pairing window brings up SYSTEM with the QR"
    );
    for ws in 0..7 {
        app.ws = ws;
        let mut shapes = 0;
        for _ in 0..3 {
            shapes = frame(&ctx, &mut app, vec![], &[]);
        }
        assert!(shapes > 20, "workspace {ws} drew only {shapes} primitives");
    }
    assert!(app.m.spectrum.rows.len() >= 5);
    // The RF workspace subscribed to the live waterfall.
    assert!(sent(&app)
        .iter()
        .any(|s| s["ws"]["type"] == "subscribe_spectrum"));
}

#[test]
fn approvals_by_keyboard_and_gamepad() {
    let ctx = egui::Context::default();
    let mut app = seeded();
    app.client.inject(Inbound::Frame(json!({"type": "approval_request", "request_id": "req_1", "tier": 1, "action": "ssh_exec", "host": "mbp", "summary": "make deploy", "hold_ms": 0})));
    frame(&ctx, &mut app, vec![], &[]);
    frame(&ctx, &mut app, vec![key(Key::Y, Modifiers::NONE)], &[]);
    assert!(sent(&app)
        .iter()
        .any(|s| s["ws"]["type"] == "approval_decision"
            && s["ws"]["request_id"] == "req_1"
            && s["ws"]["decision"] == "approve_once"));

    // Tier 2: a tap is not enough; holding A for 1.5 s is.
    app.client.inject(Inbound::Frame(json!({"type": "approval_request", "request_id": "req_2", "tier": 2, "action": "hw.gpio_rail", "host": "deck", "summary": "gps rail OFF", "hold_ms": 1500})));
    frame(&ctx, &mut app, vec![], &[]);
    frame(
        &ctx,
        &mut app,
        vec![],
        &[PadEvent {
            button: Pad::A,
            pressed: true,
        }],
    );
    frame(&ctx, &mut app, vec![], &[]);
    assert!(
        !sent(&app).iter().any(|s| s["ws"]["request_id"] == "req_2"),
        "no decision before the hold"
    );
    std::thread::sleep(std::time::Duration::from_millis(1600));
    frame(&ctx, &mut app, vec![], &[]);
    assert!(sent(&app)
        .iter()
        .any(|s| s["ws"]["request_id"] == "req_2" && s["ws"]["decision"] == "approve_once"));

    // B denies.
    app.client.inject(Inbound::Frame(json!({"type": "approval_request", "request_id": "req_3", "tier": 1, "action": "net.port_probe", "host": "deck", "summary": "probe"})));
    frame(
        &ctx,
        &mut app,
        vec![],
        &[PadEvent {
            button: Pad::A,
            pressed: false,
        }],
    );
    frame(
        &ctx,
        &mut app,
        vec![],
        &[PadEvent {
            button: Pad::B,
            pressed: true,
        }],
    );
    assert!(sent(&app)
        .iter()
        .any(|s| s["ws"]["request_id"] == "req_3" && s["ws"]["decision"] == "deny"));
}

#[test]
fn navigation_palette_and_kill_switch() {
    let ctx = egui::Context::default();
    let mut app = seeded();
    frame(&ctx, &mut app, vec![], &[]);
    frame(&ctx, &mut app, vec![key(Key::Num3, Modifiers::ALT)], &[]);
    assert_eq!(app.ws, 2);
    frame(
        &ctx,
        &mut app,
        vec![],
        &[PadEvent {
            button: Pad::Start,
            pressed: true,
        }],
    );
    frame(
        &ctx,
        &mut app,
        vec![],
        &[PadEvent {
            button: Pad::Start,
            pressed: false,
        }],
    );
    assert_eq!(app.ws, 3);
    frame(
        &ctx,
        &mut app,
        vec![],
        &[PadEvent {
            button: Pad::Select,
            pressed: true,
        }],
    );
    frame(
        &ctx,
        &mut app,
        vec![],
        &[PadEvent {
            button: Pad::Select,
            pressed: false,
        }],
    );
    assert_eq!(app.ws, 2);
    // Gamepad Y opens the palette; typing filters; Enter runs.
    frame(
        &ctx,
        &mut app,
        vec![],
        &[PadEvent {
            button: Pad::Y,
            pressed: true,
        }],
    );
    assert!(app.palette.is_some());
    frame(
        &ctx,
        &mut app,
        vec![Event::Text("kill".into())],
        &[PadEvent {
            button: Pad::Y,
            pressed: false,
        }],
    );
    frame(&ctx, &mut app, vec![key(Key::Enter, Modifiers::NONE)], &[]);
    assert!(
        sent(&app).iter().any(|s| s["ws"]["type"] == "kill_all"),
        "{:?}",
        sent(&app)
    );
    // Select + Start together is the kill switch from anywhere.
    let before = sent(&app)
        .iter()
        .filter(|s| s["ws"]["type"] == "kill_all")
        .count();
    frame(
        &ctx,
        &mut app,
        vec![],
        &[PadEvent {
            button: Pad::Select,
            pressed: true,
        }],
    );
    frame(
        &ctx,
        &mut app,
        vec![],
        &[PadEvent {
            button: Pad::Start,
            pressed: true,
        }],
    );
    assert_eq!(
        sent(&app)
            .iter()
            .filter(|s| s["ws"]["type"] == "kill_all")
            .count(),
        before + 1
    );
    // AGENT: the sidebar starts on "New chat"; D-pad down selects (and fetches) the chats.
    let mut app = seeded();
    frame(&ctx, &mut app, vec![], &[]);
    app.ws = 0;
    frame(&ctx, &mut app, vec![], &[]);
    for _ in 0..2 {
        frame(
            &ctx,
            &mut app,
            vec![key(Key::ArrowDown, Modifiers::NONE)],
            &[],
        );
    }
    assert_eq!(app.agent.selected.as_deref(), Some("c2"));
    assert!(sent(&app)
        .iter()
        .any(|s| s["path"] == "/v1/chats/c2?limit=50"));
    // A enters the content; B comes back to the sidebar.
    frame(&ctx, &mut app, vec![], &[pad(Pad::A, true)]);
    assert!(app.content[0]);
    frame(&ctx, &mut app, vec![], &[pad(Pad::A, false)]);
    frame(&ctx, &mut app, vec![], &[pad(Pad::B, true)]);
    assert!(!app.content[0]);
    // X in FLEET's sidebar probes the selected host (row 0 is "Add host").
    app.ws = 4;
    frame(&ctx, &mut app, vec![], &[]);
    frame(
        &ctx,
        &mut app,
        vec![key(Key::ArrowDown, Modifiers::NONE)],
        &[],
    );
    frame(&ctx, &mut app, vec![], &[pad(Pad::X, true)]);
    assert!(sent(&app)
        .iter()
        .any(|s| s["path"] == "/v1/hosts/macbook-pro/probe"));
    // The palette opens the help with A.
    frame(&ctx, &mut app, vec![], &[pad(Pad::Y, true)]);
    typed(&ctx, &mut app, "controls");
    frame(&ctx, &mut app, vec![], &[pad(Pad::A, true)]);
    assert!(app.help);
    frame(&ctx, &mut app, vec![], &[pad(Pad::B, true)]);
    assert!(!app.help);
    // F1 opens the controls help; B closes it.
    frame(&ctx, &mut app, vec![key(Key::F1, Modifiers::NONE)], &[]);
    assert!(app.help);
    frame(&ctx, &mut app, vec![], &[pad(Pad::B, true)]);
    assert!(!app.help);
}

fn pad(button: Pad, pressed: bool) -> PadEvent {
    PadEvent { button, pressed }
}

/// Types into whichever field has the keyboard.
fn typed(ctx: &egui::Context, app: &mut DeckApp, text: &str) {
    frame(ctx, app, vec![Event::Text(text.into())], &[]);
}

#[test]
fn ai_provider_saves_config_and_key() {
    let ctx = egui::Context::default();
    let mut app = seeded();
    http(
        &app.client,
        Tag::Config,
        json!({"config": {"llm": {"provider": "anthropic", "model": "claude-sonnet-5-5", "api_key_env": "ANTHROPIC_API_KEY", "effort": "medium", "base_url": "", "max_steps": 40}, "agents": {}}}),
    );
    http(
        &app.client,
        Tag::Secrets,
        json!({"secrets": [{"name": "ANTHROPIC_API_KEY", "is_set": false, "masked": null}]}),
    );
    frame(&ctx, &mut app, vec![], &[]);
    app.ws = 6;
    app.section[6] = 0;
    frame(&ctx, &mut app, vec![], &[]);
    // Enter the form, choose the OpenRouter preset (▶ on PROVIDER).
    frame(&ctx, &mut app, vec![], &[pad(Pad::A, true)]);
    frame(
        &ctx,
        &mut app,
        vec![key(Key::ArrowRight, Modifiers::NONE)],
        &[pad(Pad::A, false)],
    );
    frame(&ctx, &mut app, vec![], &[]);
    // Down to API KEY (row 6), A to edit, type, Enter.
    for _ in 0..6 {
        frame(
            &ctx,
            &mut app,
            vec![key(Key::ArrowDown, Modifiers::NONE)],
            &[],
        );
    }
    frame(&ctx, &mut app, vec![], &[pad(Pad::A, true)]);
    frame(&ctx, &mut app, vec![], &[pad(Pad::A, false)]);
    typed(&ctx, &mut app, "sk-or-test-1234");
    frame(&ctx, &mut app, vec![key(Key::Enter, Modifiers::NONE)], &[]);
    frame(&ctx, &mut app, vec![], &[]);
    // Now on SAVE.
    frame(&ctx, &mut app, vec![], &[pad(Pad::A, true)]);
    let log = sent(&app);
    let put = log
        .iter()
        .find(|s| s["method"] == "PUT" && s["path"] == "/v1/config")
        .expect("PUT /v1/config");
    assert_eq!(put["body"]["config"]["llm"]["provider"], "openrouter");
    assert_eq!(
        put["body"]["config"]["llm"]["api_key_env"],
        "OPENROUTER_API_KEY"
    );
    assert_eq!(
        put["body"]["config"]["llm"]["max_steps"], 40,
        "other settings are sent back as they were"
    );
    let patch = log
        .iter()
        .find(|s| s["method"] == "PATCH" && s["path"] == "/v1/secrets")
        .expect("PATCH /v1/secrets");
    assert_eq!(
        patch["body"]["set"]["OPENROUTER_API_KEY"],
        "sk-or-test-1234"
    );
    assert!(
        app.sys.ai.as_ref().unwrap().key.is_empty(),
        "the key is not kept in the HUD"
    );
}

#[test]
fn add_host_from_the_tailnet() {
    let ctx = egui::Context::default();
    let mut app = seeded();
    app.client.inject(Inbound::Http {
        tag: Tag::Tool("net.tailscale_peers".into(), 1),
        status: 200,
        body: json!({"ok": true, "result": {"peers": [{"hostname": "mbp", "ip": "100.88.1.10", "os": "macOS", "online": true}, {"hostname": "Build-Box", "ip": "100.88.1.20", "os": "linux", "online": true}]}}).to_string().into_bytes(),
        headers: vec![],
    });
    frame(&ctx, &mut app, vec![], &[]);
    app.ws = 4;
    frame(&ctx, &mut app, vec![], &[]);
    frame(&ctx, &mut app, vec![], &[pad(Pad::A, true)]);
    // ▶ picks the first peer not configured yet (Build-Box) and fills the form.
    frame(
        &ctx,
        &mut app,
        vec![key(Key::ArrowRight, Modifiers::NONE)],
        &[pad(Pad::A, false)],
    );
    frame(&ctx, &mut app, vec![], &[]);
    assert_eq!(app.fleet.id, "build-box");
    assert_eq!(app.fleet.ip, "100.88.1.20");
    assert_eq!(app.fleet.os, 1);
    // SSH USER is row 5.
    for _ in 0..5 {
        frame(
            &ctx,
            &mut app,
            vec![key(Key::ArrowDown, Modifiers::NONE)],
            &[],
        );
    }
    frame(&ctx, &mut app, vec![], &[pad(Pad::A, true)]);
    frame(&ctx, &mut app, vec![], &[pad(Pad::A, false)]);
    typed(&ctx, &mut app, "dev");
    frame(&ctx, &mut app, vec![key(Key::Escape, Modifiers::NONE)], &[]);
    assert!(app.content[4], "Esc leaves the field, not the form");
    for _ in 0..4 {
        frame(
            &ctx,
            &mut app,
            vec![key(Key::ArrowDown, Modifiers::NONE)],
            &[],
        );
    }
    frame(&ctx, &mut app, vec![], &[pad(Pad::A, true)]);
    let put = sent(&app)
        .into_iter()
        .find(|s| s["method"] == "PUT" && s["path"] == "/v1/hosts")
        .expect("PUT /v1/hosts");
    let hosts = &put["body"]["hosts"];
    assert_eq!(hosts["build-box"]["tailscale_ip"], "100.88.1.20");
    assert_eq!(hosts["build-box"]["ssh"]["user"], "dev");
    assert_eq!(
        hosts["macbook-pro"]["hostname"], "mbp",
        "the hosts already there are kept"
    );
    assert!(
        hosts.get("deck").is_none(),
        "the deck is built in, never stored"
    );
}
