//! The cyberdeck subsystems behind the daemon: the SDR spectrum pump (live waterfall rows to
//! subscribed sockets) and the `sdr.*`, `mesh.*`, `gps.*` and `net.*` tools (SPEC 5.2).

use crate::app::App;
use crate::config::{DeckConfig, SdrConfig};
use crate::events::{Audience, Hub};
use crate::gps::Gps;
use crate::hw::run_command;
use crate::mesh::{self, Mesh};
use crate::netops;
use crate::policy::Tier;
use crate::tools::ToolSpec;
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use lapis_sdr::dsp::{self, Peak, Spectrum};
use lapis_sdr::iq::{CliKind, CliSource, IqSource, SdrError, Synthetic};
use lapis_sdr::{decoders, demod, devices, Complex32};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Sdr {
    src: Box<dyn IqSource>,
    spec: Spectrum,
    pub status: String,
    floor: f32,
    ceil: f32,
    peaks: Vec<Peak>,
    paused: bool,
}

fn pick_source(cfg: &SdrConfig, mock: bool, root: &Path) -> (Box<dyn IqSource>, String) {
    let synth = || -> Box<dyn IqSource> {
        Box::new(Synthetic::new(cfg.center_hz, cfg.sample_rate, cfg.gain_db))
    };
    if mock || cfg.source == "synthetic" {
        return (synth(), "synthetic".into());
    }
    let usb = devices::detect_usb(root);
    let kind = match cfg.source.as_str() {
        "rtl_sdr" => Some(CliKind::RtlSdr),
        "soapy" => Some(CliKind::Soapy(String::new())),
        _ => usb.first().map(|d| match d["driver"].as_str() {
            Some("rtlsdr") => CliKind::RtlSdr,
            Some(driver) => CliKind::Soapy(format!("driver={driver}")),
            None => CliKind::RtlSdr,
        }),
    };
    match kind {
        Some(k) => (
            Box::new(CliSource::new(
                k,
                cfg.center_hz,
                cfg.sample_rate,
                cfg.gain_db,
            )),
            "starting".into(),
        ),
        None => (synth(), "synthetic (no SDR on USB)".into()),
    }
}

impl Sdr {
    fn new(cfg: &SdrConfig, mock: bool) -> Self {
        let (src, status) = pick_source(cfg, mock, Path::new("/"));
        Sdr {
            src,
            spec: Spectrum::new(cfg.fft_size),
            status,
            floor: -90.0,
            ceil: -20.0,
            peaks: Vec::new(),
            paused: false,
        }
    }

    fn fall_back(&mut self, why: &str) {
        let info = self.src.info();
        self.src = Box::new(Synthetic::new(
            info.center_hz,
            info.sample_rate,
            info.gain_db,
        ));
        self.status = format!("synthetic ({why})");
    }

    /// One spectrum row for the waterfall.
    pub fn step(&mut self) -> Result<Value, String> {
        if self.paused {
            return Err("paused: a decoder has the dongle".into());
        }
        let mut buf = vec![Complex32::default(); self.spec.size()];
        match self.src.read(&mut buf) {
            Ok(()) => {}
            Err(SdrError::Missing(tool)) => {
                self.fall_back(&format!("{tool} not installed"));
                return Err(self.status.clone());
            }
            Err(e) => {
                self.status = format!("degraded: {e}");
                return Err(self.status.clone());
            }
        }
        let row = self.spec.power_db(&buf);
        let info = self.src.info();
        let floor_now = dsp::median(&row) - 6.0;
        let peak = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        self.floor = 0.9 * self.floor + 0.1 * floor_now;
        self.ceil = 0.9 * self.ceil + 0.1 * (peak + 3.0).max(self.floor + 30.0);
        self.peaks = dsp::find_peaks(
            &row,
            info.center_hz,
            info.sample_rate,
            15.0,
            (row.len() / 64).max(2),
        );
        self.peaks.truncate(8);
        if !info.synthetic {
            self.status = "streaming".into();
        } else if self.status == "starting" {
            self.status = "synthetic".into();
        }
        let bins = dsp::quantize(&dsp::decimate_max(&row, 512), self.floor, self.ceil);
        Ok(
            json!({"type": "sdr_spectrum", "center_hz": info.center_hz, "sample_rate": info.sample_rate, "gain_db": info.gain_db,
                  "synthetic": info.synthetic, "source": info.kind, "floor_db": self.floor, "ceil_db": self.ceil,
                  "bins": B64.encode(bins), "peaks": self.peaks}),
        )
    }

    pub fn snapshot(&self) -> Value {
        json!({"status": self.status, "source": self.src.info(), "peaks": self.peaks, "floor_db": self.floor, "ceil_db": self.ceil, "fft_size": self.spec.size()})
    }

    fn tune(&mut self, center: u64, gain: Option<f32>) -> Result<(), String> {
        if !(100_000..=6_000_000_000).contains(&center) {
            return Err("center_hz must be between 100 kHz and 6 GHz".into());
        }
        self.src.tune(center).map_err(|e| e.to_string())?;
        if let Some(g) = gain {
            self.src.set_gain(g).map_err(|e| e.to_string())?;
        }
        self.spec.reset();
        Ok(())
    }

    fn read(&mut self, n: usize) -> Result<Vec<Complex32>, String> {
        let mut out = vec![Complex32::default(); n];
        for chunk in out.chunks_mut(65_536) {
            self.src.read(chunk).map_err(|e| e.to_string())?;
        }
        Ok(out)
    }
}

pub struct DeckState {
    pub sdr: Arc<Mutex<Sdr>>,
    pub mesh: Arc<Mesh>,
    pub gps: Arc<Gps>,
    epoch: Arc<AtomicU64>,
    started: AtomicBool,
    mock: bool,
    cfg: DeckConfig,
}

impl DeckState {
    pub fn new(cfg: &DeckConfig, mock: bool) -> Self {
        DeckState {
            sdr: Arc::new(Mutex::new(Sdr::new(&cfg.sdr, mock))),
            mesh: Mesh::new(mock),
            gps: Gps::new(mock),
            epoch: Arc::new(AtomicU64::new(0)),
            started: AtomicBool::new(false),
            mock,
            cfg: cfg.clone(),
        }
    }

    pub fn summary(&self) -> Value {
        let sdr = self.sdr.lock().map(|s| json!({"status": s.status, "center_hz": s.src.info().center_hz, "synthetic": s.src.info().synthetic})).unwrap_or(Value::Null);
        let mesh = self.mesh.state.lock().map(|m| json!({"status": m.status, "nodes": m.nodes.len(), "messages": m.messages.len()})).unwrap_or(Value::Null);
        let gps = self.gps.snapshot();
        json!({"sdr": sdr, "mesh": mesh, "gps": {"status": gps["status"], "fix": gps["fix"]["fix"], "used": gps["used"], "pps_locked": gps["pps_locked"]}})
    }

    /// Kill switch: long-running scans stop at their next step.
    pub fn stop_all(&self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
    }

    pub fn start(&self, app: Arc<App>) {
        if self.started.swap(true, Ordering::Relaxed) {
            return;
        }
        let (sdr, hub) = (self.sdr.clone(), app.hub.clone());
        std::thread::Builder::new()
            .name("sdr-pump".into())
            .spawn(move || pump(sdr, hub))
            .map_err(|e| tracing::error!(error = %e, "cannot start the SDR pump"))
            .ok();
        if !self.mock {
            let hub = app.hub.clone();
            let on_text: Arc<dyn Fn(mesh::TextMessage) + Send + Sync> =
                Arc::new(move |m| hub.send(json!({"type": "mesh_message", "message": m})));
            let region = mesh::region_code(&self.cfg.mesh_region);
            if region.is_none() && !self.cfg.mesh_region.trim().is_empty() {
                tracing::warn!(mesh_region = %self.cfg.mesh_region, "unknown LoRa region in deck.toml; leaving the radio's alone");
            }
            tokio::spawn(
                self.mesh
                    .clone()
                    .run(self.cfg.meshtasticd.clone(), region, on_text),
            );
            tokio::spawn(self.gps.clone().run(self.cfg.gpsd.clone()));
        }
    }
}

fn pump(sdr: Arc<Mutex<Sdr>>, hub: Hub) {
    loop {
        let row = sdr
            .lock()
            .map_err(|_| "poisoned".to_string())
            .and_then(|mut s| s.step());
        match row {
            Ok(v) => {
                hub.send_to(v, Audience::Spectrum);
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => std::thread::sleep(Duration::from_secs(2)),
        }
    }
}

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route(
            "/v1/deck/sdr",
            get(|State(app): State<Arc<App>>| async move {
                Json(
                    app.deck
                        .sdr
                        .lock()
                        .map(|s| s.snapshot())
                        .unwrap_or(Value::Null),
                )
            }),
        )
        .route("/v1/deck/sdr/tune", post(tune_route))
        .route(
            "/v1/deck/mesh",
            get(|State(app): State<Arc<App>>| async move { Json(app.deck.mesh.snapshot()) }),
        )
        .route(
            "/v1/deck/gps",
            get(|State(app): State<Arc<App>>| async move { Json(app.deck.gps.snapshot()) }),
        )
}

/// The owner tunes the waterfall (receive only: no gate).
async fn tune_route(State(app): State<Arc<App>>, bytes: axum::body::Bytes) -> Json<Value> {
    let b: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let Some(center) = b["center_hz"].as_u64() else {
        return Json(json!({"ok": false, "error": "center_hz is required"}));
    };
    let gain = b["gain_db"].as_f64().map(|g| g as f32);
    let res = app
        .deck
        .sdr
        .lock()
        .map_err(|_| "sdr unavailable".to_string())
        .and_then(|mut s| s.tune(center, gain));
    Json(match res {
        Ok(()) => json!({"ok": true, "center_hz": center}),
        Err(e) => json!({"ok": false, "error": e}),
    })
}

fn spec(
    name: &'static str,
    tier: Tier,
    description: &'static str,
    schema: fn() -> Value,
) -> ToolSpec {
    ToolSpec {
        name,
        description,
        tier,
        schema,
    }
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

fn none() -> Value {
    obj(json!({}), &[])
}

pub fn specs() -> Vec<ToolSpec> {
    vec![
        spec(
            "sdr.list_devices",
            Tier::T0,
            "SDR receivers on USB (RTL-SDR, HackRF, Airspy, PlutoSDR) and the active IQ source.",
            none,
        ),
        spec(
            "sdr.tune",
            Tier::T0,
            "Retune the live waterfall (receive only).",
            || {
                obj(
                    json!({"center_hz": {"type": "integer"}, "gain_db": {"type": "number"}}),
                    &["center_hz"],
                )
            },
        ),
        spec(
            "sdr.scan_band",
            Tier::T0,
            "Sweep a frequency range and list the strongest signals.",
            || {
                obj(
                    json!({"start_hz": {"type": "integer"}, "end_hz": {"type": "integer"}, "step_hz": {"type": "integer"}}),
                    &["start_hz", "end_hz", "step_hz"],
                )
            },
        ),
        spec(
            "sdr.capture_iq",
            Tier::T0,
            "Record raw IQ (CF32) to a file on the deck (at most 2 s).",
            || {
                obj(
                    json!({"freq_hz": {"type": "integer"}, "duration_s": {"type": "number", "maximum": 2}}),
                    &["freq_hz"],
                )
            },
        ),
        spec(
            "sdr.decode_ism",
            Tier::T0,
            "Decode ISM-band sensors (rtl_433) for a while.",
            || {
                obj(
                    json!({"freq_hz": {"type": "integer"}, "duration_s": {"type": "integer", "maximum": 60}}),
                    &[],
                )
            },
        ),
        spec(
            "sdr.decode_pager",
            Tier::T0,
            "Decode POCSAG pages and APRS frames (multimon-ng).",
            || {
                obj(
                    json!({"freq_hz": {"type": "integer"}, "duration_s": {"type": "integer", "maximum": 60}}),
                    &["freq_hz"],
                )
            },
        ),
        spec(
            "sdr.adsb_snapshot",
            Tier::T0,
            "Aircraft currently decoded by readsb (ADS-B, 1090 MHz).",
            none,
        ),
        spec(
            "sdr.demod_audio",
            Tier::T0,
            "Demodulate WFM/NFM/AM/USB/LSB to a WAV file on the deck (optionally play it).",
            || {
                obj(
                    json!({"freq_hz": {"type": "integer"}, "mode": {"type": "string", "enum": ["wfm", "nfm", "am", "usb", "lsb"]}, "duration_s": {"type": "number", "maximum": 5}, "play": {"type": "boolean"}}),
                    &["freq_hz", "mode"],
                )
            },
        ),
        spec(
            "mesh.list_nodes",
            Tier::T0,
            "Meshtastic nodes with SNR, RSSI, hops and position.",
            none,
        ),
        spec(
            "mesh.read_messages",
            Tier::T0,
            "Recent Meshtastic channel messages.",
            || obj(json!({"limit": {"type": "integer", "maximum": 200}}), &[]),
        ),
        spec(
            "mesh.send_message",
            Tier::T1,
            "Transmit a text message on a Meshtastic channel (LoRa TX).",
            || {
                obj(
                    json!({"channel": {"type": "integer", "maximum": 7}, "text": {"type": "string", "maxLength": 228}, "to": {"type": "string"}}),
                    &["channel", "text"],
                )
            },
        ),
        spec(
            "mesh.traceroute",
            Tier::T1,
            "Traceroute a Meshtastic node (transmits).",
            || obj(json!({"node_id": {"type": "string"}}), &["node_id"]),
        ),
        spec(
            "gps.get_fix",
            Tier::T0,
            "GNSS fix: lat/lon, altitude, speed, PPS clock offset.",
            none,
        ),
        spec(
            "gps.get_satellites",
            Tier::T0,
            "Satellites in view and used (skyplot data).",
            none,
        ),
        spec(
            "net.wifi_scan_passive",
            Tier::T0,
            "Wi-Fi networks the radio has already heard (no probe requests).",
            none,
        ),
        spec(
            "net.ble_scan_passive",
            Tier::T0,
            "Bluetooth LE devices nearby (BlueZ discovery).",
            || {
                obj(
                    json!({"duration_s": {"type": "integer", "maximum": 15}}),
                    &[],
                )
            },
        ),
        spec(
            "net.port_probe",
            Tier::T1,
            "TCP connect probe of a host's ports (active).",
            || {
                obj(
                    json!({"target": {"type": "string"}, "ports": {"type": "array", "items": {"type": "integer"}, "maxItems": 64}}),
                    &["target", "ports"],
                )
            },
        ),
        spec(
            "net.packet_capture",
            Tier::T1,
            "Capture packet summaries with tcpdump (BPF filter).",
            || {
                obj(
                    json!({"iface": {"type": "string"}, "bpf": {"type": "string"}, "count": {"type": "integer", "maximum": 200}}),
                    &[],
                )
            },
        ),
        spec("net.dns_lookup", Tier::T0, "Resolve a host name.", || {
            obj(json!({"name": {"type": "string"}}), &["name"])
        }),
    ]
}

/// Argument checks that run before the Policy Gate: an invalid call is refused at once, never
/// put to the owner for approval.
pub fn validate(name: &str, a: &Value) -> Result<(), String> {
    match name {
        "net.port_probe" => {
            let n = a["ports"].as_array().map(Vec::len).unwrap_or(0);
            if !netops::valid_host(a["target"].as_str().unwrap_or("")) {
                return Err("target must be a host name or an IP address".into());
            }
            if n == 0
                || n > 64
                || a["ports"].as_array().is_some_and(|p| {
                    p.iter()
                        .any(|v| v.as_u64().and_then(|n| u16::try_from(n).ok()).is_none())
                })
            {
                return Err("ports must be 1 to 64 port numbers".into());
            }
        }
        "net.packet_capture" => {
            if !netops::valid_iface(a["iface"].as_str().unwrap_or("wlan0"))
                || !netops::valid_bpf(a["bpf"].as_str().unwrap_or(""))
            {
                return Err("bad interface name or BPF filter".into());
            }
        }
        "mesh.send_message" => {
            let text = a["text"].as_str().unwrap_or("");
            if text.is_empty() || text.len() > 228 || a["channel"].as_u64().unwrap_or(0) > 7 {
                return Err("text must be 1 to 228 bytes and channel 0 to 7".into());
            }
            if a["to"]
                .as_str()
                .is_some_and(|t| mesh::parse_node_id(t).is_none())
            {
                return Err("to must be a node id like !a1b2c3d4".into());
            }
        }
        "mesh.traceroute"
            if a["node_id"]
                .as_str()
                .and_then(mesh::parse_node_id)
                .is_none() =>
        {
            return Err("node_id must look like !a1b2c3d4".into());
        }
        _ => {}
    }
    Ok(())
}

fn captures(app: &App) -> Result<PathBuf, String> {
    let dir = app.paths.state_dir.join("captures");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())
}

/// Runs a decoder that needs the dongle: the waterfall lets go of it meanwhile.
async fn with_dongle(
    app: &Arc<App>,
    argv: Vec<String>,
    timeout: Duration,
) -> Result<String, String> {
    let sdr = app.deck.sdr.clone();
    blocking(move || {
        if let Ok(mut s) = sdr.lock() {
            s.paused = true;
            s.src.release();
        }
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let (rc, out) = run_command(&refs, timeout, None);
        if let Ok(mut s) = sdr.lock() {
            s.paused = false;
        }
        match rc {
            127 => Err(format!("{} is not installed", argv[0])),
            0 | 124 => Ok(out),
            _ if !out.trim().is_empty() => Ok(out),
            _ => Err(format!("{} exited {rc}", argv[0])),
        }
    })
    .await?
}

fn synthetic(app: &App) -> bool {
    app.deck
        .sdr
        .lock()
        .map(|s| s.src.info().synthetic)
        .unwrap_or(true)
}

pub async fn invoke(app: &Arc<App>, name: &str, a: &Value) -> Result<Value, String> {
    let d = &app.deck;
    let u = |k: &str| a[k].as_u64();
    match name {
        "sdr.list_devices" => {
            let usb = blocking(|| devices::detect_usb(Path::new("/"))).await?;
            let s = d.sdr.lock().map_err(|_| "sdr unavailable")?;
            Ok(json!({"usb": usb, "source": s.src.info(), "status": s.status}))
        }
        "sdr.tune" => {
            let center = u("center_hz").ok_or("center_hz is required")?;
            d.sdr
                .lock()
                .map_err(|_| "sdr unavailable")?
                .tune(center, a["gain_db"].as_f64().map(|g| g as f32))?;
            Ok(json!({"center_hz": center}))
        }
        "sdr.scan_band" => {
            let (start, end, step) = (
                u("start_hz").ok_or("start_hz")?,
                u("end_hz").ok_or("end_hz")?,
                u("step_hz").ok_or("step_hz")?,
            );
            let (sdr, epoch) = (d.sdr.clone(), d.epoch.clone());
            let at = epoch.load(Ordering::Relaxed);
            blocking(move || {
                let mut s = sdr.lock().map_err(|_| "sdr unavailable")?;
                let home = s.src.info().center_hz;
                let size = s.spec.size();
                let res = dsp::scan_band(s.src.as_mut(), start, end, step, size, &|| epoch.load(Ordering::Relaxed) != at);
                let _ = s.tune(home, None);
                res.map(|peaks| json!({"start_hz": start, "end_hz": end, "step_hz": step, "peaks": peaks})).map_err(|e| e.to_string())
            })
            .await?
        }
        "sdr.capture_iq" => {
            let freq = u("freq_hz").ok_or("freq_hz is required")?;
            let secs = a["duration_s"].as_f64().unwrap_or(0.5).clamp(0.01, 2.0);
            let path = captures(app)?.join(format!("iq-{freq}-{}.cf32", crate::auth::now()));
            let sdr = d.sdr.clone();
            blocking(move || {
                let mut s = sdr.lock().map_err(|_| "sdr unavailable")?;
                let home = s.src.info().center_hz;
                s.tune(freq, None)?;
                let rate = s.src.info().sample_rate;
                let n = ((f64::from(rate) * secs) as usize).min(4_000_000);
                let iq = s.read(n);
                let _ = s.tune(home, None);
                let iq = iq?;
                let mut bytes = Vec::with_capacity(n * 8);
                for c in &iq {
                    bytes.extend_from_slice(&c.re.to_le_bytes());
                    bytes.extend_from_slice(&c.im.to_le_bytes());
                }
                std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
                let p = iq.iter().map(|c| c.norm_sqr()).sum::<f32>() / iq.len().max(1) as f32;
                Ok(json!({"path": path, "samples": iq.len(), "bytes": bytes.len(), "sample_rate": rate, "center_hz": freq, "format": "cf32le", "mean_power_dbfs": 10.0 * (p + 1e-12).log10()}))
            })
            .await?
        }
        "sdr.decode_ism" => {
            let freq = u("freq_hz").unwrap_or(433_920_000);
            let secs = u("duration_s").unwrap_or(20).clamp(1, 60);
            let (packets, synth) = if synthetic(app) {
                (decoders::parse_rtl433(decoders::SAMPLE_RTL433), true)
            } else {
                (
                    decoders::parse_rtl433(
                        &with_dongle(
                            app,
                            decoders::rtl433_argv(freq, secs),
                            Duration::from_secs(secs + 10),
                        )
                        .await?,
                    ),
                    false,
                )
            };
            for p in &packets {
                app.hub.send(json!({"type": "sdr_decode", "decoder": "rtl_433", "freq_hz": freq, "packet": p, "synthetic": synth}));
            }
            Ok(json!({"freq_hz": freq, "packets": packets, "synthetic": synth}))
        }
        "sdr.decode_pager" => {
            let freq = u("freq_hz").ok_or("freq_hz is required")?;
            let secs = u("duration_s").unwrap_or(20).clamp(1, 60);
            let (out, synth) = if synthetic(app) {
                (decoders::SAMPLE_MULTIMON.to_owned(), true)
            } else {
                (
                    with_dongle(
                        app,
                        decoders::multimon_argv(freq, secs),
                        Duration::from_secs(secs + 10),
                    )
                    .await?,
                    false,
                )
            };
            let msgs: Vec<Value> = out.lines().filter_map(decoders::parse_multimon).collect();
            for m in &msgs {
                app.hub.send(json!({"type": "sdr_decode", "decoder": "multimon-ng", "freq_hz": freq, "packet": m, "synthetic": synth}));
            }
            Ok(json!({"freq_hz": freq, "messages": msgs, "synthetic": synth}))
        }
        "sdr.adsb_snapshot" => match std::fs::read_to_string(decoders::READSB_AIRCRAFT) {
            Ok(text) => Ok(decoders::parse_readsb(&text)),
            Err(_) if synthetic(app) => Ok(
                json!({"synthetic": true, "table": decoders::parse_readsb(decoders::SAMPLE_AIRCRAFT)}),
            ),
            Err(_) => {
                Err("readsb is not running (start readsb.service; it needs the dongle)".into())
            }
        },
        "sdr.demod_audio" => {
            let freq = u("freq_hz").ok_or("freq_hz is required")?;
            let mode = a["mode"]
                .as_str()
                .and_then(demod::Mode::parse)
                .ok_or("mode must be wfm, nfm, am, usb or lsb")?;
            let secs = a["duration_s"].as_f64().unwrap_or(3.0).clamp(0.2, 5.0);
            let play = a["play"].as_bool().unwrap_or(false);
            let path = captures(app)?.join(format!("audio-{freq}-{}.wav", crate::auth::now()));
            let sdr = d.sdr.clone();
            blocking(move || {
                let mut s = sdr.lock().map_err(|_| "sdr unavailable")?;
                let home = s.src.info().center_hz;
                // Tune beside the signal so the receiver's DC spike stays out of the channel.
                const OFFSET: u64 = 250_000;
                s.tune(freq + OFFSET, None)?;
                let rate = s.src.info().sample_rate;
                let total = (f64::from(rate) * secs) as usize;
                let (mut pcm, mut audio_rate) = (Vec::new(), 48_000);
                let mut left = total;
                let mut err = None;
                while left > 0 {
                    let n = left.min(262_144);
                    match s.read(n) {
                        Ok(iq) => {
                            let (chunk, r) = demod::demodulate(&iq, rate, -(OFFSET as f64), mode);
                            pcm.extend(chunk);
                            audio_rate = r;
                        }
                        Err(e) => {
                            err = Some(e);
                            break;
                        }
                    }
                    left -= n;
                }
                let _ = s.tune(home, None);
                drop(s);
                if let Some(e) = err {
                    return Err(e);
                }
                std::fs::write(&path, demod::wav(&pcm, audio_rate)).map_err(|e| e.to_string())?;
                if play {
                    let p = path.display().to_string();
                    std::thread::spawn(move || run_command(&["aplay", "-q", &p], Duration::from_secs(30), None));
                }
                Ok(json!({"path": path, "mode": mode, "freq_hz": freq, "audio_rate": audio_rate, "seconds": pcm.len() as f64 / f64::from(audio_rate.max(1)),
                          "rms": demod::rms(&pcm), "dominant_hz": demod::dominant_freq(&pcm, audio_rate), "playing": play}))
            })
            .await?
        }
        "mesh.list_nodes" => {
            let s = d.mesh.snapshot();
            Ok(json!({"status": s["status"], "my_node": s["my_node"], "nodes": s["nodes"]}))
        }
        "mesh.read_messages" => {
            let limit = u("limit").unwrap_or(50).min(200) as usize;
            let s = d.mesh.snapshot();
            let all = s["messages"].as_array().cloned().unwrap_or_default();
            Ok(json!({"status": s["status"], "messages": all[all.len().saturating_sub(limit)..]}))
        }
        "mesh.send_message" => {
            let to = match a["to"].as_str() {
                Some(t) => {
                    Some(mesh::parse_node_id(t).ok_or("to must be a node id like !a1b2c3d4")?)
                }
                None => None,
            };
            let r = d.mesh.send_text(
                u("channel").unwrap_or(0) as u32,
                to,
                a["text"].as_str().unwrap_or(""),
            )?;
            app.hub.send(json!({"type": "mesh_message", "message": d.mesh.snapshot()["messages"].as_array().and_then(|m| m.last().cloned())}));
            Ok(r)
        }
        "mesh.traceroute" => {
            let node = a["node_id"]
                .as_str()
                .and_then(mesh::parse_node_id)
                .ok_or("node_id must look like !a1b2c3d4")?;
            d.mesh.traceroute(node, Duration::from_secs(45)).await
        }
        "gps.get_fix" => {
            let s = d.gps.snapshot();
            Ok(
                json!({"status": s["status"], "fix": s["fix"], "hdop": s["hdop"], "pps_offset_ns": s["pps_offset_ns"], "pps_locked": s["pps_locked"], "device": s["device"]}),
            )
        }
        "gps.get_satellites" => {
            let s = d.gps.snapshot();
            Ok(json!({"visible": s["visible"], "used": s["used"], "satellites": s["satellites"]}))
        }
        "net.wifi_scan_passive" => {
            if app.mock {
                return Ok(
                    json!({"networks": netops::parse_wifi_survey(netops::MOCK_WIFI), "mock": true}),
                );
            }
            let (rc, out) =
                blocking(|| run_command(&netops::WIFI_ARGV, Duration::from_secs(10), None)).await?;
            if rc == 127 {
                return Err("nmcli (NetworkManager) is not installed".into());
            }
            Ok(json!({"networks": netops::parse_wifi_survey(&out)}))
        }
        "net.ble_scan_passive" => {
            if app.mock {
                return Ok(
                    json!({"devices": netops::parse_bluetoothctl(netops::MOCK_BLE), "mock": true}),
                );
            }
            let secs = u("duration_s").unwrap_or(8).clamp(2, 15);
            let (rc, out) = blocking(move || {
                run_command(
                    &["bluetoothctl", "--timeout", &secs.to_string(), "scan", "on"],
                    Duration::from_secs(secs + 5),
                    None,
                )
            })
            .await?;
            if rc == 127 {
                return Err("bluetoothctl (BlueZ) is not installed".into());
            }
            Ok(json!({"devices": netops::parse_bluetoothctl(&out)}))
        }
        "net.port_probe" => {
            let ports: Vec<u16> = a["ports"]
                .as_array()
                .map(|p| {
                    p.iter()
                        .filter_map(|v| v.as_u64().and_then(|n| u16::try_from(n).ok()))
                        .collect()
                })
                .unwrap_or_default();
            netops::port_probe(a["target"].as_str().unwrap_or(""), &ports).await
        }
        "net.packet_capture" => {
            let iface = a["iface"].as_str().unwrap_or("wlan0").to_owned();
            let bpf = a["bpf"].as_str().unwrap_or("").to_owned();
            let count = u("count").unwrap_or(50).clamp(1, 200) as u32;
            if !netops::valid_iface(&iface) || !netops::valid_bpf(&bpf) {
                return Err("bad interface name or BPF filter".into());
            }
            let out = if app.mock {
                netops::MOCK_CAPTURE.to_owned()
            } else {
                let argv = netops::tcpdump_argv(&iface, &bpf, count);
                let (rc, out) = blocking(move || {
                    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
                    run_command(&refs, Duration::from_secs(30), None)
                })
                .await?;
                if rc == 127 {
                    return Err("tcpdump is not installed".into());
                }
                out
            };
            let lines: Vec<String> = out
                .lines()
                .filter(|l| l.starts_with(char::is_numeric))
                .take(count as usize)
                .map(lapis_crypto::redact)
                .collect();
            Ok(
                json!({"iface": iface, "bpf": bpf, "packets": lines.len(), "lines": lines, "mock": app.mock}),
            )
        }
        "net.dns_lookup" => netops::dns_lookup(a["name"].as_str().unwrap_or("")).await,
        _ => Err(format!("unknown tool {name:?}")),
    }
}

/// The mock tailnet the host preview shows.
pub fn mock_peers() -> Vec<Value> {
    vec![
        json!({"hostname": "macbook-pro", "dns_name": "macbook-pro.tail1234.ts.net", "ip": "100.88.1.10", "os": "macOS", "online": true, "exit_node": false, "last_seen": ""}),
        json!({"hostname": "arch-dev", "dns_name": "arch-dev.tail1234.ts.net", "ip": "100.88.1.20", "os": "linux", "online": true, "exit_node": false, "last_seen": ""}),
        json!({"hostname": "pixel-9", "dns_name": "pixel-9.tail1234.ts.net", "ip": "100.88.1.30", "os": "android", "online": true, "exit_node": false, "last_seen": ""}),
        json!({"hostname": "nas", "dns_name": "nas.tail1234.ts.net", "ip": "100.88.1.40", "os": "linux", "online": false, "exit_node": false, "last_seen": "2026-11-24T21:02:11Z"}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pump_rows_and_fallback() {
        let mut s = Sdr::new(&SdrConfig::default(), true);
        let row = s.step().unwrap();
        assert_eq!(row["type"], "sdr_spectrum");
        assert_eq!(
            B64.decode(row["bins"].as_str().unwrap()).unwrap().len(),
            512
        );
        assert!(s.tune(50, None).is_err());
        // A configured receiver whose tool is missing falls back to synthetic, degraded.
        let cfg = SdrConfig {
            source: "soapy".into(),
            ..Default::default()
        };
        let mut real = Sdr {
            src: Box::new(CliSource::new(
                CliKind::Soapy("driver=nonexistent-lapis".into()),
                1,
                1,
                0.0,
            )),
            ..Sdr::new(&cfg, true)
        };
        let _ = std::process::Command::new("true").status();
        let err = real.step();
        if which("rx_sdr") {
            assert!(err.is_err());
        } else {
            assert!(err.unwrap_err().contains("not installed"));
            assert!(real.step().is_ok(), "synthetic after the fallback");
        }
    }

    fn which(tool: &str) -> bool {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(tool).exists()))
    }

    #[test]
    fn every_spec_has_a_tier_and_schema() {
        let all = crate::tools::specs();
        assert!(all.len() >= 24);
        for s in &all {
            assert_eq!((s.schema)()["type"], "object", "{}", s.name);
        }
        assert_eq!(crate::tools::tier_of("mesh.send_message"), Some(Tier::T1));
        assert_eq!(crate::tools::tier_of("net.port_probe"), Some(Tier::T1));
        assert_eq!(crate::tools::tier_of("hw.gpio_rail"), Some(Tier::T2));
        assert_eq!(crate::tools::tier_of("sdr.scan_band"), Some(Tier::T0));
    }
}
