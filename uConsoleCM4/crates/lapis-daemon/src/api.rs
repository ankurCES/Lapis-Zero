//! The control plane on 127.0.0.1:8421: the Lapis-Zero API the Android companion speaks
//! (health, devices, pairing, state, approvals, device cockpit, WebSocket), the Lapis-Deck
//! additions under `/v1/deck`, and everything else forwarded to the harness.

use crate::app::App;
use crate::auth::{Caller, CallerKind};
use crate::hw::{PowerAction, Rail};
use crate::policy::ActionRequest;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Extension, Json, Router};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

pub const MAX_BODY: usize = 2 * 1024 * 1024;

pub struct ApiError(pub StatusCode, pub &'static str, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(json!({"error": {"code": self.1, "message": lapis_crypto::redact(&self.2)}})),
        )
            .into_response()
    }
}

type ApiResult = Result<Response, ApiError>;

fn invalid(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid", msg.into())
}

fn not_found(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not_found", msg.into())
}

fn local_only(c: &Caller) -> Result<(), ApiError> {
    if c.kind == CallerKind::Local {
        Ok(())
    } else {
        Err(ApiError(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Only the device's own services may call this".into(),
        ))
    }
}

/// A JSON object body; an empty body is `{}`.
fn body<T: DeserializeOwned>(bytes: &Bytes) -> Result<T, ApiError> {
    let raw: &[u8] = if bytes.iter().all(u8::is_ascii_whitespace) {
        b"{}"
    } else {
        bytes
    };
    let v: Value = serde_json::from_slice(raw).map_err(|_| {
        ApiError(
            StatusCode::BAD_REQUEST,
            "bad_json",
            "Request body is not valid JSON".into(),
        )
    })?;
    if !v.is_object() {
        return Err(invalid("Request body must be a JSON object"));
    }
    serde_json::from_value(v).map_err(|e| invalid(e.to_string()))
}

fn ok(v: Value) -> ApiResult {
    Ok(Json(v).into_response())
}

fn bearer(req: &Request) -> &str {
    let h = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    match h.split_once(' ') {
        Some((scheme, token)) if scheme.eq_ignore_ascii_case("bearer") => token.trim(),
        _ => "",
    }
}

async fn auth_mw(State(app): State<Arc<App>>, mut req: Request, next: Next) -> Response {
    if req.method() == axum::http::Method::POST && req.uri().path() == "/v1/pair/exchange" {
        return next.run(req).await;
    }
    match app.auth.verify(bearer(&req)) {
        Some(caller) => {
            req.extensions_mut().insert(caller);
            next.run(req).await
        }
        None => ApiError(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Missing, invalid or revoked bearer token".into(),
        )
        .into_response(),
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/pair/exchange", post(pair_exchange))
        .route("/v1/devices", get(devices))
        .route("/v1/devices/{id}", delete(revoke))
        .route("/v1/state", get(state))
        .route("/v1/approvals", get(approvals))
        .route("/v1/approvals/{id}", post(decide))
        .route("/v1/agent/kill-all", post(kill_all))
        .route("/v1/device/telemetry", get(telemetry))
        .route("/v1/device/ui", patch(patch_ui))
        .route("/v1/device/screen.png", get(screen))
        .route("/v1/device/power", post(power))
        .route("/v1/deck/status", get(deck_status))
        .route("/v1/deck/rails/{name}", post(set_rail))
        .route("/v1/deck/tools", get(list_tools))
        .route("/v1/deck/tools/invoke", post(invoke_tool))
        .route("/v1/deck/audit", get(audit))
        .route("/v1/wifi", get(wifi))
        .route("/v1/wifi/scan", get(wifi_scan))
        .route("/v1/wifi/connect", post(wifi_connect))
        .route("/v1/wifi/forget", post(wifi_forget))
        .route("/v1/wifi/ble", post(wifi_ble))
        .route("/v1/policy/authorize", post(authorize))
        .route("/v1/internal/screen/{id}", post(screen_upload))
        .route("/v1/ws", get(crate::ws::handler))
        .merge(crate::deck::routes())
        .fallback(forward)
        .layer(middleware::from_fn_with_state(app.clone(), auth_mw))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(app)
}

async fn health(State(app): State<Arc<App>>, Extension(c): Extension<Caller>) -> ApiResult {
    ok(json!({
        "ok": true, "version": crate::config::VERSION, "hostname": app.hostname,
        "uptime_sec": app.started.elapsed().as_secs(), "mock": app.mock, "device_id": c.id,
        "kind": "lapis-deck", "harness": app.harness_up(),
    }))
}

#[derive(serde::Deserialize)]
struct ExchangeBody {
    #[serde(default)]
    nonce: String,
    #[serde(default)]
    device_name: String,
    #[serde(default)]
    device_model: String,
}

async fn pair_exchange(State(app): State<Arc<App>>, bytes: Bytes) -> ApiResult {
    if app.auth.locked_out() {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "locked_out",
            "Too many failed pairing attempts; wait a minute".into(),
        ));
    }
    let b: ExchangeBody = body(&bytes)?;
    let name = if b.device_name.is_empty() {
        "Android".to_owned()
    } else {
        b.device_name
    };
    match app.pair_exchange(&b.nonce, &name, &b.device_model).await {
        Some((id, token)) => {
            ok(json!({"device_id": id, "api_token": token, "hostname": app.hostname}))
        }
        None => Err(ApiError(
            StatusCode::FORBIDDEN,
            "bad_nonce",
            "Pairing code is invalid, already used, or expired".into(),
        )),
    }
}

async fn devices(State(app): State<Arc<App>>, Extension(c): Extension<Caller>) -> ApiResult {
    ok(app.auth.devices(&c.id))
}

async fn revoke(
    State(app): State<Arc<App>>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult {
    if !app.auth.revoke(&id) {
        return Err(not_found("No paired device with that id"));
    }
    app.audit
        .record("revoke", json!({"device_id": id, "by": c.by()}));
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn state(State(app): State<Arc<App>>) -> ApiResult {
    ok(app.state())
}

async fn approvals(State(app): State<Arc<App>>) -> ApiResult {
    ok(json!({"approvals": app.gate.pending()}))
}

#[derive(serde::Deserialize)]
struct DecisionBody {
    #[serde(default)]
    decision: String,
    #[serde(default)]
    reason: Option<String>,
}

async fn decide(
    State(app): State<Arc<App>>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> ApiResult {
    let b: DecisionBody = body(&bytes)?;
    if !crate::policy::DECISIONS.contains(&b.decision.as_str()) {
        return Err(invalid(
            "decision must be approve_once, approve_session or deny",
        ));
    }
    if !app
        .gate
        .decide(&id, &b.decision, &c.by(), b.reason.as_deref().unwrap_or(""))
    {
        return Err(not_found("No pending approval with that id"));
    }
    ok(json!({"ok": true}))
}

async fn kill_all(State(app): State<Arc<App>>, Extension(c): Extension<Caller>) -> ApiResult {
    app.kill_all(&c.by()).await;
    ok(json!({"ok": true}))
}

async fn telemetry(State(app): State<Arc<App>>) -> ApiResult {
    app.refresh_net().await;
    let me = app.clone();
    let t = tokio::task::spawn_blocking(move || me.telemetry())
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    ok(t)
}

async fn patch_ui(State(app): State<Arc<App>>, bytes: Bytes) -> ApiResult {
    let patch: Value = body(&bytes)?;
    ok(app.set_ui(&patch).map_err(invalid)?)
}

async fn screen(State(app): State<Arc<App>>) -> ApiResult {
    match app.screen_png().await {
        Some(png) => Ok((
            [
                (header::CONTENT_TYPE, HeaderValue::from_static("image/png")),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            png,
        )
            .into_response()),
        None => Err(ApiError(
            StatusCode::GATEWAY_TIMEOUT,
            "ui_unavailable",
            "lapis-ui did not answer the screen request".into(),
        )),
    }
}

async fn screen_upload(
    State(app): State<Arc<App>>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
    png: Bytes,
) -> ApiResult {
    local_only(&c)?;
    if !png.starts_with(b"\x89PNG") {
        return Err(invalid("expected a PNG"));
    }
    app.on_screen(&id, png.to_vec());
    ok(json!({"ok": true}))
}

#[derive(serde::Deserialize)]
struct PowerBody {
    #[serde(default)]
    action: String,
}

async fn power(
    State(app): State<Arc<App>>,
    Extension(c): Extension<Caller>,
    bytes: Bytes,
) -> ApiResult {
    let b: PowerBody = body(&bytes)?;
    let action = b.action.as_str();
    let power = match action {
        "reboot" => Some(PowerAction::Reboot),
        "shutdown" => Some(PowerAction::Shutdown),
        "restart_daemon" => Some(PowerAction::RestartDaemon),
        _ => None,
    };
    if let Some(p) = power {
        app.audit
            .record("power", json!({"action": action, "by": c.by()}));
        let hw = app.hw.clone();
        // Answer first: the request must not be cut off by the reboot it asked for.
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Err(e) = tokio::task::spawn_blocking(move || hw.power(p))
                .await
                .unwrap_or(Ok(()))
            {
                tracing::error!(error = %e, "power action failed");
            }
        });
    } else {
        match action {
            "start_ble_pairing" => drop(app.pairing_start("ble").await.map_err(invalid)?),
            "start_qr_pairing" => drop(app.pairing_start("qr").await.map_err(invalid)?),
            "stop_pairing" => app.pairing_stop().await,
            _ => return Err(invalid(format!("Unknown action: {action:?}"))),
        }
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"ok": true, "action": action})),
    )
        .into_response())
}

async fn deck_status(State(app): State<Arc<App>>) -> ApiResult {
    let hw = app.hw.clone();
    let (rails, battery) = tokio::task::spawn_blocking(move || (hw.rails(), hw.battery()))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    ok(
        json!({"rails": rails, "battery": battery, "net": app.net(), "harness": app.harness_up(), "deck": app.deck.summary(), "mock": app.mock}),
    )
}

#[derive(serde::Deserialize)]
struct RailBody {
    on: bool,
}

/// The owner's own toggle (HUD top bar, app): not an agent tool call, but audited.
async fn set_rail(
    State(app): State<Arc<App>>,
    Extension(c): Extension<Caller>,
    Path(name): Path<String>,
    bytes: Bytes,
) -> ApiResult {
    let rail = Rail::parse(&name).ok_or_else(|| not_found(format!("No rail {name:?}")))?;
    let b: RailBody = body(&bytes)?;
    app.set_rail(rail, b.on, &c.by())
        .await
        .map_err(|e| ApiError(StatusCode::CONFLICT, "rail_failed", e))?;
    ok(json!({"ok": true, "rail": rail.name(), "on": b.on}))
}

// ---------------------------------------------------------------------- Wi-Fi
// The owner's own network (HUD, app): like the rail toggles, audited and not gated.

async fn wifi(State(app): State<Arc<App>>) -> ApiResult {
    ok(app.wifi_view().await)
}

async fn wifi_scan(State(app): State<Arc<App>>) -> ApiResult {
    let ctl = app.net_ctl.clone();
    match tokio::task::spawn_blocking(move || ctl.wifi_scan()).await {
        Ok(Ok(networks)) => ok(json!({"networks": networks})),
        Ok(Err(e)) => Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "wifi_unavailable",
            e,
        )),
        Err(e) => Err(ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            e.to_string(),
        )),
    }
}

#[derive(serde::Deserialize)]
struct WifiConnectBody {
    #[serde(default)]
    ssid: String,
    #[serde(default)]
    psk: Option<String>,
}

/// WPA-PSK passphrase rules: 8..63 printable ASCII, or 64 hex digits.
pub fn valid_psk(psk: &str) -> bool {
    let printable = psk.bytes().all(|b| (0x20..=0x7e).contains(&b));
    (printable && (8..=63).contains(&psk.len()))
        || (psk.len() == 64 && psk.bytes().all(|b| b.is_ascii_hexdigit()))
}

async fn wifi_connect(
    State(app): State<Arc<App>>,
    Extension(c): Extension<Caller>,
    bytes: Bytes,
) -> ApiResult {
    let b: WifiConnectBody = body(&bytes)?;
    let psk = b.psk.filter(|p| !p.is_empty());
    if let Some(p) = &psk {
        lapis_crypto::register_secret(p); // before anything can echo it
    }
    if b.ssid.is_empty() || b.ssid.len() > 32 || b.ssid.chars().any(char::is_control) {
        return Err(invalid("ssid must be 1 to 32 bytes"));
    }
    if psk.as_deref().is_some_and(|p| !valid_psk(p)) {
        return Err(invalid(
            "The password must be 8 to 63 characters (or 64 hex digits)",
        ));
    }
    ok(app.wifi_connect(b.ssid, psk, &c.by()).await)
}

#[derive(serde::Deserialize)]
struct WifiForgetBody {
    #[serde(default)]
    name: String,
}

async fn wifi_forget(
    State(app): State<Arc<App>>,
    Extension(c): Extension<Caller>,
    bytes: Bytes,
) -> ApiResult {
    let b: WifiForgetBody = body(&bytes)?;
    if !app.wifi_forget(b.name.clone(), &c.by()).await {
        return Err(not_found(format!("No saved network {:?}", b.name)));
    }
    ok(app.wifi_view().await)
}

async fn wifi_ble(State(app): State<Arc<App>>) -> ApiResult {
    if app.auth.paired_count() == 0 {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "not_paired",
            "No phone is paired with this device yet".into(),
        ));
    }
    app.phone_window_start()
        .await
        .map_err(|e| ApiError(StatusCode::SERVICE_UNAVAILABLE, "ble_unavailable", e))?;
    ok(app.wifi_view().await)
}

async fn list_tools() -> ApiResult {
    let tools: Vec<Value> = crate::tools::specs()
        .into_iter()
        .map(|s| json!({"name": s.name, "description": s.description, "tier": s.tier, "input_schema": (s.schema)()}))
        .collect();
    ok(json!({"tools": tools}))
}

#[derive(serde::Deserialize)]
struct InvokeBody {
    tool: String,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    context: InvokeContext,
}

#[derive(serde::Deserialize, Default)]
struct InvokeContext {
    chat_id: Option<String>,
    task_id: Option<String>,
    source: Option<String>,
}

/// Every local tool call, whoever makes it, goes through the Policy Gate here.
async fn invoke_tool(
    State(app): State<Arc<App>>,
    Extension(c): Extension<Caller>,
    bytes: Bytes,
) -> ApiResult {
    let b: InvokeBody = body(&bytes)?;
    let args = if b.args.is_null() { json!({}) } else { b.args };
    let Some(tier) = crate::tools::tier_of(&b.tool) else {
        return ok(
            json!({"ok": false, "decision": "policy_deny", "error": format!("{} is not a cyberdeck tool", b.tool)}),
        );
    };
    if let Err(e) = crate::tools::validate(&b.tool, &args) {
        return ok(json!({"ok": false, "tier": tier, "decision": "invalid", "error": e}));
    }
    let req = ActionRequest {
        tool: b.tool.clone(),
        host: "deck".into(),
        command: crate::tools::inspected(&b.tool, &args),
        summary: crate::tools::summary(&b.tool, &args),
        chat_id: b.context.chat_id,
        task_id: b.context.task_id,
        source: b.context.source.unwrap_or_else(|| {
            if c.kind == CallerKind::Phone {
                "android".into()
            } else {
                "local".into()
            }
        }),
        ..Default::default()
    };
    let outcome = app.gate.authorize(req, Some(tier)).await;
    if !outcome.allowed {
        return ok(
            json!({"ok": false, "tier": outcome.tier, "decision": outcome.decision, "error": outcome.reason, "by": outcome.by}),
        );
    }
    match crate::tools::invoke(&app, &b.tool, &args).await {
        Ok(mut result) => {
            lapis_crypto::redact_json(&mut result);
            ok(
                json!({"ok": true, "tier": outcome.tier, "decision": outcome.decision, "result": result}),
            )
        }
        Err(e) => ok(
            json!({"ok": false, "tier": outcome.tier, "decision": outcome.decision, "error": lapis_crypto::redact(&e)}),
        ),
    }
}

async fn audit(State(app): State<Arc<App>>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let limit = q
        .get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(100)
        .min(1000);
    ok(json!({"entries": app.audit.tail(limit)}))
}

/// The harness asks before every step it takes on a fleet host (long-polls on approvals).
async fn authorize(
    State(app): State<Arc<App>>,
    Extension(c): Extension<Caller>,
    bytes: Bytes,
) -> ApiResult {
    local_only(&c)?;
    let mut req: ActionRequest = body(&bytes)?;
    if req.tool.starts_with("hw.")
        || req.tool.starts_with("sdr.")
        || req.tool.starts_with("mesh.")
        || req.tool.starts_with("gps.")
        || req.tool.starts_with("net.")
    {
        // Deck tools are run by the daemon itself, through /v1/deck/tools/invoke.
        return Err(invalid(
            "cyberdeck tools are invoked through /v1/deck/tools/invoke",
        ));
    }
    if req.source.is_empty() {
        req.source = "harness".into();
    }
    ok(serde_json::to_value(app.gate.authorize(req, None).await).unwrap_or(Value::Null))
}

async fn forward(
    State(app): State<Arc<App>>,
    Extension(c): Extension<Caller>,
    req: Request<Body>,
) -> Response {
    if !req.uri().path().starts_with("/v1/") {
        return not_found("Not found").into_response();
    }
    // The app's send_prompt and screenshots work the same through the daemon.
    app.harness.forward(req, &c).await
}
