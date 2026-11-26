//! End to end over real sockets: the Lapis-Zero API the Android app speaks, QR pairing,
//! the Policy Gate with phone approvals, deck tools, and the harness behind its Unix socket.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::extract::ws::{Message as AxMsg, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use lapis_daemon::app::{App, Parts};
use lapis_daemon::config::{DeckConfig, Paths};
use lapis_daemon::hw::MockHardware;
use lapis_daemon::net::MockNet;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut tx, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(conn);
    let mut req = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let req = req
        .body(Full::new(Bytes::from(
            body.map(|b| b.to_string()).unwrap_or_default(),
        )))
        .unwrap();
    let resp = tx.send_request(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn fake_harness() -> Router {
    Router::new()
        .route(
            "/v1/chats",
            get(|h: HeaderMap| async move {
                Json(json!({"chats": [{"id": "c1", "title": "deploy", "host_id": "mbp"}],
                            "seen_name": h.get("x-lapis-device-name").and_then(|v| v.to_str().ok()),
                            "seen_kind": h.get("x-lapis-device-kind").and_then(|v| v.to_str().ok()),
                            "saw_authorization": h.contains_key("authorization")}))
            }),
        )
        .route("/v1/secrets", patch(|| async { Json(json!({"secrets": []})) }))
        .route("/v1/agent/kill-all", post(|| async { Json(json!({"ok": true})) }))
        .route(
            "/v1/ws",
            get(|ws: WebSocketUpgrade| async move {
                ws.on_upgrade(|mut socket| async move {
                    let state = json!({"type": "state_sync", "agent_state": "THINKING", "hosts": ["mbp"], "active_host": "mbp", "api_key_ready": true});
                    let _ = socket.send(AxMsg::Text(state.to_string().into())).await;
                    let event = json!({"type": "chat_message_created", "chat_id": "c1",
                                       "message": {"id": 1, "role": "assistant", "content": "use sk-ant-api03-abcdefghijklmnopqrstu"}});
                    let _ = socket.send(AxMsg::Text(event.to_string().into())).await;
                    while let Some(Ok(_)) = socket.recv().await {}
                })
            }),
        )
}

async fn next_of(
    ws: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    kind: &str,
) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("frame in time")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(t.as_str()).unwrap();
            if v["type"] == kind {
                return v;
            }
        }
    }
}

#[tokio::test]
async fn control_plane_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("harness.sock");
    let listener = lapis_daemon::bind(0).await.unwrap();
    let addr = listener.local_addr().unwrap();
    assert!(addr.ip().is_loopback(), "never binds beyond loopback");
    let app = App::new(Parts {
        cfg: DeckConfig::default(),
        paths: Paths {
            state_dir: dir.path().join("state"),
            harness_socket: socket.clone(),
        },
        mock: true,
        hw: Arc::new(MockHardware::default()),
        net_ctl: Arc::new(MockNet::default()),
        dev_advertise: Some(format!("127.0.0.1:{}", addr.port())),
    })
    .unwrap();
    tokio::spawn(lapis_daemon::serve(app.clone(), listener));
    let local = std::fs::read_to_string(dir.path().join("state/local.token")).unwrap();

    // Auth: everything but the exchange needs a bearer token.
    let (st, body) = http(addr, "GET", "/v1/health", None, None).await;
    assert_eq!(
        (st, body["error"]["code"].as_str()),
        (401, Some("unauthorized"))
    );
    let (st, body) = http(addr, "GET", "/v1/health", Some(&local), None).await;
    assert_eq!(
        (st, body["kind"].as_str(), body["mock"].as_bool()),
        (200, Some("lapis-deck"), Some(true))
    );

    // The harness is not up yet: forwarded routes say so in the API's error envelope.
    let (st, body) = http(addr, "GET", "/v1/chats", Some(&local), None).await;
    assert_eq!(
        (st, body["error"]["code"].as_str()),
        (503, Some("harness_unavailable"))
    );

    // QR pairing exactly as the app does it.
    app.refresh_net().await;
    let (st, _) = http(
        addr,
        "POST",
        "/v1/device/power",
        Some(&local),
        Some(json!({"action": "start_qr_pairing"})),
    )
    .await;
    assert_eq!(st, 202);
    let (_, state) = http(addr, "GET", "/v1/state", Some(&local), None).await;
    let payload = state["pairing"]["payload"].as_str().unwrap().to_owned();
    assert!(
        payload.starts_with(&format!(
            "lapis://pair?host=127.0.0.1:{}&nonce=",
            addr.port()
        )) && payload.ends_with("&tls=0")
    );
    let nonce = payload
        .split("nonce=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_owned();
    assert_eq!(nonce.len(), 43);
    let ex = json!({"nonce": nonce, "device_name": "Pixel", "device_model": "Pixel 9 Pro"});
    let (st, paired) = http(addr, "POST", "/v1/pair/exchange", None, Some(ex.clone())).await;
    assert_eq!(st, 200);
    let phone = paired["api_token"].as_str().unwrap().to_owned();
    assert!(paired["device_id"].as_str().unwrap().starts_with("dev_"));
    let (st, body) = http(addr, "POST", "/v1/pair/exchange", None, Some(ex)).await;
    assert_eq!(
        (st, body["error"]["code"].as_str()),
        (403, Some("bad_nonce"))
    );
    let (_, devices) = http(addr, "GET", "/v1/devices", Some(&phone), None).await;
    assert_eq!(devices["devices"][0]["current"], true);
    let (_, state) = http(addr, "GET", "/v1/state", Some(&phone), None).await;
    assert!(state["pairing"].is_null(), "the QR closes once used");
    assert_eq!(state["battery_pct"], 78);
    assert_eq!(state["device"]["kind"], "lapis-deck");

    // Bring the harness up; the phone's WebSocket sees its events, merged and redacted.
    let ul = tokio::net::UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move { axum::serve(ul, fake_harness()).await });
    let mut req = format!("ws://{addr}/v1/ws").into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {phone}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let first = next_of(&mut ws, "state_sync").await;
    assert!(first["device"]["hostname"].as_str().is_some());
    let msg = next_of(&mut ws, "chat_message_created").await;
    let content = msg["message"]["content"].as_str().unwrap();
    assert!(
        !content.contains("sk-ant") && content.contains("[REDACTED]"),
        "{content}"
    );
    for _ in 0..50 {
        if app.harness_up() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (_, state) = http(addr, "GET", "/v1/state", Some(&phone), None).await;
    assert_eq!(
        (state["agent_state"].as_str(), state["hosts"][0].as_str()),
        (Some("THINKING"), Some("mbp"))
    );
    assert_eq!(state["tailscale_fqdn"], "lapis-deck.tail1234.ts.net");

    // Forwarded with the caller's identity, never with its token.
    let (st, chats) = http(addr, "GET", "/v1/chats", Some(&phone), None).await;
    assert_eq!(
        (st, chats["seen_name"].as_str(), chats["seen_kind"].as_str()),
        (200, Some("Pixel"), Some("phone"))
    );
    assert_eq!(chats["saw_authorization"], false);

    // A fleet step from the harness waits for the phone's approval.
    let tok = local.clone();
    let ask = tokio::spawn(async move {
        http(addr, "POST", "/v1/policy/authorize", Some(&tok),
             Some(json!({"tool": "ssh_exec", "host": "mbp", "command": "make deploy", "host_policy": "strict", "task_id": "t_9", "summary": "make deploy"}))).await
    });
    let req_frame = next_of(&mut ws, "approval_request").await;
    assert_eq!(
        (req_frame["tier"].as_u64(), req_frame["action"].as_str()),
        (Some(1), Some("ssh_exec"))
    );
    let (_, list) = http(addr, "GET", "/v1/approvals", Some(&phone), None).await;
    let id = list["approvals"][0]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (st, _) = http(
        addr,
        "POST",
        &format!("/v1/approvals/{id}"),
        Some(&phone),
        Some(json!({"decision": "approve_once"})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, outcome) = ask.await.unwrap();
    assert_eq!(
        (st, outcome["allowed"].as_bool(), outcome["by"].as_str()),
        (200, Some(true), Some("android:Pixel"))
    );
    assert!(outcome["approval_token"].is_string());
    let resolved = next_of(&mut ws, "approval_resolved").await;
    assert_eq!(resolved["decision"], "approve_once");
    // Only the device's own services may ask the gate.
    let (st, _) = http(
        addr,
        "POST",
        "/v1/policy/authorize",
        Some(&phone),
        Some(json!({"tool": "ssh_exec"})),
    )
    .await;
    assert_eq!(st, 403);

    // Deck tools: Tier 0 runs at once, Tier 2 waits for an approval over the WebSocket.
    let (_, r) = http(
        addr,
        "POST",
        "/v1/deck/tools/invoke",
        Some(&local),
        Some(json!({"tool": "hw.battery_status"})),
    )
    .await;
    assert_eq!(
        (
            r["ok"].as_bool(),
            r["result"]["battery"]["percent"].as_u64()
        ),
        (Some(true), Some(78))
    );
    let (_, r) = http(
        addr,
        "POST",
        "/v1/deck/tools/invoke",
        Some(&local),
        Some(json!({"tool": "hw.pty_exec", "args": {"cmd": "uname"}})),
    )
    .await;
    assert_eq!(
        (r["ok"].as_bool(), r["tier"].as_u64()),
        (Some(true), Some(0)),
        "{r}"
    );
    let (_, r) = http(
        addr,
        "POST",
        "/v1/deck/tools/invoke",
        Some(&local),
        Some(json!({"tool": "fs.wipe"})),
    )
    .await;
    assert_eq!(r["decision"], "policy_deny");
    let tok = local.clone();
    let rail = tokio::spawn(async move {
        http(
            addr,
            "POST",
            "/v1/deck/tools/invoke",
            Some(&tok),
            Some(json!({"tool": "hw.gpio_rail", "args": {"name": "gps", "state": false}})),
        )
        .await
    });
    let req_frame = next_of(&mut ws, "approval_request").await;
    assert_eq!(
        (req_frame["tier"].as_u64(), req_frame["hold_ms"].as_u64()),
        (Some(2), Some(1500))
    );
    let decision = json!({"type": "approval_decision", "request_id": req_frame["request_id"], "decision": "approve_once"});
    ws.send(Message::Text(decision.to_string().into()))
        .await
        .unwrap();
    let (_, r) = rail.await.unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let (_, status) = http(addr, "GET", "/v1/deck/status", Some(&phone), None).await;
    let gps = status["rails"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "gps")
        .unwrap()
        .clone();
    assert_eq!(gps["on"], false);

    // The kill switch denies what is pending.
    let tok = local.clone();
    let pending = tokio::spawn(async move {
        http(addr, "POST", "/v1/policy/authorize", Some(&tok), Some(json!({"tool": "ssh_exec", "host": "mbp", "command": "rm -r dist", "task_id": "t_10"}))).await
    });
    next_of(&mut ws, "approval_request").await;
    ws.send(Message::Text(
        json!({"type": "kill_all"}).to_string().into(),
    ))
    .await
    .unwrap();
    let (_, outcome) = pending.await.unwrap();
    assert_eq!(
        (outcome["allowed"].as_bool(), outcome["by"].as_str()),
        (Some(false), Some("android:Pixel"))
    );

    // UI settings and the audit trail.
    let (st, ui) = http(
        addr,
        "PATCH",
        "/v1/device/ui",
        Some(&phone),
        Some(json!({"brightness": 40, "theme": "paper"})),
    )
    .await;
    assert_eq!(
        (st, ui["brightness"].as_u64(), ui["theme"].as_str()),
        (200, Some(40), Some("paper"))
    );
    let (st, _) = http(
        addr,
        "PATCH",
        "/v1/device/ui",
        Some(&phone),
        Some(json!({"theme": "neon"})),
    )
    .await;
    assert_eq!(st, 422);
    let (_, audit) = http(addr, "GET", "/v1/deck/audit?limit=50", Some(&local), None).await;
    let events: Vec<&str> = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["event"].as_str())
        .collect();
    assert!(
        events.contains(&"approval")
            && events.contains(&"kill_all")
            && events.contains(&"rail")
            && events.contains(&"paired"),
        "{events:?}"
    );
    // The screen mirror answers 504 when no lapis-ui is connected.
    let (st, body) = http(addr, "GET", "/v1/device/screen.png", Some(&phone), None).await;
    assert_eq!(
        (st, body["error"]["code"].as_str()),
        (504, Some("ui_unavailable"))
    );
    // Revocation is immediate.
    let id = paired["device_id"].as_str().unwrap();
    let (st, _) = http(
        addr,
        "DELETE",
        &format!("/v1/devices/{id}"),
        Some(&local),
        None,
    )
    .await;
    assert_eq!(st, 204);
    let (st, _) = http(addr, "GET", "/v1/state", Some(&phone), None).await;
    assert_eq!(st, 401);
}

#[tokio::test]
async fn deck_subsystems_over_the_api() {
    let dir = tempfile::tempdir().unwrap();
    let listener = lapis_daemon::bind(0).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = App::new(Parts {
        cfg: DeckConfig::default(),
        paths: Paths {
            state_dir: dir.path().join("state"),
            harness_socket: dir.path().join("none.sock"),
        },
        mock: true,
        hw: Arc::new(MockHardware::default()),
        net_ctl: Arc::new(MockNet::default()),
        dev_advertise: None,
    })
    .unwrap();
    tokio::spawn(lapis_daemon::serve(app.clone(), listener));
    let t = std::fs::read_to_string(dir.path().join("state/local.token")).unwrap();
    let call = |tool: &'static str, args: Value| {
        let t = t.clone();
        async move {
            http(
                addr,
                "POST",
                "/v1/deck/tools/invoke",
                Some(&t),
                Some(json!({"tool": tool, "args": args})),
            )
            .await
            .1
        }
    };

    let (_, tools) = http(addr, "GET", "/v1/deck/tools", Some(&t), None).await;
    assert!(tools["tools"].as_array().unwrap().len() >= 24);

    // The live waterfall reaches a subscribed socket.
    let mut req = format!("ws://{addr}/v1/ws").into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {t}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws.send(Message::Text(
        json!({"type": "subscribe_spectrum", "enabled": true})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let row = next_of(&mut ws, "sdr_spectrum").await;
    assert_eq!(
        (row["synthetic"].as_bool(), row["center_hz"].as_u64()),
        (Some(true), Some(100_000_000))
    );

    let r = call(
        "sdr.scan_band",
        json!({"start_hz": 99_000_000u64, "end_hz": 101_000_000u64, "step_hz": 1_000_000u64}),
    )
    .await;
    let peaks = r["result"]["peaks"].as_array().unwrap();
    assert!(
        peaks
            .iter()
            .any(|p| (p["freq_hz"].as_f64().unwrap() - 100.1e6).abs() < 100e3),
        "{r}"
    );
    let r = call(
        "sdr.demod_audio",
        json!({"freq_hz": 100_100_000u64, "mode": "wfm", "duration_s": 0.5}),
    )
    .await;
    assert!(
        (r["result"]["dominant_hz"].as_f64().unwrap() - 1000.0).abs() < 80.0,
        "{r}"
    );
    assert!(std::path::Path::new(r["result"]["path"].as_str().unwrap()).exists());
    let r = call(
        "sdr.capture_iq",
        json!({"freq_hz": 433_920_000u64, "duration_s": 0.05}),
    )
    .await;
    assert_eq!(
        r["result"]["bytes"].as_u64(),
        Some(r["result"]["samples"].as_u64().unwrap() * 8)
    );
    let r = call("sdr.decode_ism", json!({})).await;
    assert_eq!(r["result"]["packets"][0]["model"], "Acurite-Tower");
    let r = call("sdr.adsb_snapshot", json!({})).await;
    assert_eq!(r["result"]["table"]["aircraft"][0]["flight"], "UAL1543");
    let r = call("sdr.decode_pager", json!({"freq_hz": 152_007_500u64})).await;
    assert_eq!(r["result"]["messages"][1]["proto"], "APRS");
    let r = call("gps.get_fix", json!({})).await;
    assert_eq!(
        (
            r["result"]["fix"]["fix"].as_str(),
            r["result"]["pps_locked"].as_bool()
        ),
        (Some("3D"), Some(true))
    );
    let r = call("gps.get_satellites", json!({})).await;
    assert_eq!(r["result"]["used"], 7);
    let r = call("mesh.list_nodes", json!({})).await;
    assert_eq!(r["result"]["nodes"].as_array().unwrap().len(), 4);
    let r = call("net.wifi_scan_passive", json!({})).await;
    assert_eq!(r["result"]["networks"][0]["ssid"], "lapis-lab");
    let r = call("net.ble_scan_passive", json!({})).await;
    assert_eq!(r["result"]["devices"][0]["name"], "Pixel 9 Pro");
    let r = call("net.dns_lookup", json!({"name": "localhost"})).await;
    assert_eq!(r["ok"], true);
    // Invalid arguments are refused before anyone is asked to approve them.
    let r = call("net.packet_capture", json!({"bpf": "port 53; reboot"})).await;
    assert_eq!(
        (r["ok"].as_bool(), r["decision"].as_str()),
        (Some(false), Some("invalid"))
    );

    // LoRa transmit is Tier 1: it waits for a [Y/N].
    let tok = t.clone();
    let send = tokio::spawn(async move {
        http(
            addr,
            "POST",
            "/v1/deck/tools/invoke",
            Some(&tok),
            Some(
                json!({"tool": "mesh.send_message", "args": {"channel": 0, "text": "deck online"}}),
            ),
        )
        .await
        .1
    });
    let ask = next_of(&mut ws, "approval_request").await;
    assert_eq!(
        (ask["tier"].as_u64(), ask["action"].as_str()),
        (Some(1), Some("mesh.send_message"))
    );
    let id = ask["request_id"].as_str().unwrap();
    http(
        addr,
        "POST",
        &format!("/v1/approvals/{id}"),
        Some(&t),
        Some(json!({"decision": "approve_once"})),
    )
    .await;
    let r = send.await.unwrap();
    assert_eq!(
        (r["ok"].as_bool(), r["tier"].as_u64()),
        (Some(true), Some(1)),
        "{r}"
    );
    let m = next_of(&mut ws, "mesh_message").await;
    assert_eq!(m["message"]["text"], "deck online");
    // A denied probe never runs.
    let tok = t.clone();
    let probe = tokio::spawn(async move {
        http(
            addr,
            "POST",
            "/v1/deck/tools/invoke",
            Some(&tok),
            Some(json!({"tool": "net.port_probe", "args": {"target": "127.0.0.1", "ports": [22]}})),
        )
        .await
        .1
    });
    let ask = next_of(&mut ws, "approval_request").await;
    ws.send(Message::Text(
        json!({"type": "approval_decision", "request_id": ask["request_id"], "decision": "deny"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let r = probe.await.unwrap();
    assert_eq!(
        (r["ok"].as_bool(), r["decision"].as_str()),
        (Some(false), Some("deny"))
    );

    let (_, sdr) = http(addr, "GET", "/v1/deck/sdr", Some(&t), None).await;
    assert_eq!(sdr["source"]["synthetic"], true);
    let (_, tuned) = http(
        addr,
        "POST",
        "/v1/deck/sdr/tune",
        Some(&t),
        Some(json!({"center_hz": 433_920_000u64})),
    )
    .await;
    assert_eq!(tuned["ok"], true);
    let (_, mesh) = http(addr, "GET", "/v1/deck/mesh", Some(&t), None).await;
    assert_eq!(
        mesh["messages"].as_array().unwrap().last().unwrap()["outgoing"],
        true
    );
    let (_, state) = http(addr, "GET", "/v1/state", Some(&t), None).await;
    assert_eq!(state["deck"]["summary"]["gps"]["fix"], "3D");
}
