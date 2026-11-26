//! Meshtastic over `meshtasticd`'s TCP API (port 4403): the stream framing
//! (`0x94 0xC3 len_hi len_lo` + protobuf) and the subset of `meshtastic/mesh.proto` the MESH
//! workspace needs: node list with SNR/RSSI/hops, channel text, positions, traceroute, and
//! the LoRa region (`config.proto`), which the daemon sets from deck.toml's `mesh_region`.

use prost::Message as _;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

pub const BROADCAST: u32 = 0xFFFF_FFFF;
pub const PORT_TEXT: i32 = 1;
pub const PORT_POSITION: i32 = 3;
pub const PORT_NODEINFO: i32 = 4;
pub const PORT_ADMIN: i32 = 6;
pub const PORT_TRACEROUTE: i32 = 70;
const START1: u8 = 0x94;
const START2: u8 = 0xC3;
const MAX_FRAME: usize = 512;

pub mod pb {
    //! Field numbers as in meshtastic/protobufs `mesh.proto`.
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ToRadio {
        #[prost(oneof = "to_radio::Payload", tags = "1, 3, 4, 7")]
        pub payload: Option<to_radio::Payload>,
    }
    pub mod to_radio {
        #[derive(Clone, PartialEq, prost::Oneof)]
        pub enum Payload {
            #[prost(message, tag = "1")]
            Packet(super::MeshPacket),
            #[prost(uint32, tag = "3")]
            WantConfigId(u32),
            #[prost(bool, tag = "4")]
            Disconnect(bool),
            #[prost(message, tag = "7")]
            Heartbeat(super::Heartbeat),
        }
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Heartbeat {}
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct FromRadio {
        #[prost(uint32, tag = "1")]
        pub id: u32,
        #[prost(oneof = "from_radio::Payload", tags = "2, 3, 4, 5, 7")]
        pub payload: Option<from_radio::Payload>,
    }
    pub mod from_radio {
        #[derive(Clone, PartialEq, prost::Oneof)]
        pub enum Payload {
            #[prost(message, tag = "2")]
            Packet(super::MeshPacket),
            #[prost(message, tag = "3")]
            MyInfo(super::MyNodeInfo),
            #[prost(message, tag = "4")]
            NodeInfo(super::NodeInfo),
            #[prost(message, tag = "5")]
            Config(super::Config),
            #[prost(uint32, tag = "7")]
            ConfigCompleteId(u32),
        }
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct MeshPacket {
        #[prost(fixed32, tag = "1")]
        pub from: u32,
        #[prost(fixed32, tag = "2")]
        pub to: u32,
        #[prost(uint32, tag = "3")]
        pub channel: u32,
        #[prost(message, optional, tag = "4")]
        pub decoded: Option<Data>,
        #[prost(fixed32, tag = "6")]
        pub id: u32,
        #[prost(fixed32, tag = "7")]
        pub rx_time: u32,
        #[prost(float, tag = "8")]
        pub rx_snr: f32,
        #[prost(uint32, tag = "9")]
        pub hop_limit: u32,
        #[prost(bool, tag = "10")]
        pub want_ack: bool,
        #[prost(int32, tag = "12")]
        pub rx_rssi: i32,
        #[prost(uint32, tag = "15")]
        pub hop_start: u32,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Data {
        #[prost(int32, tag = "1")]
        pub portnum: i32,
        #[prost(bytes = "vec", tag = "2")]
        pub payload: Vec<u8>,
        #[prost(bool, tag = "3")]
        pub want_response: bool,
        #[prost(fixed32, tag = "4")]
        pub dest: u32,
        #[prost(fixed32, tag = "5")]
        pub source: u32,
        #[prost(fixed32, tag = "6")]
        pub request_id: u32,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct MyNodeInfo {
        #[prost(uint32, tag = "1")]
        pub my_node_num: u32,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct NodeInfo {
        #[prost(uint32, tag = "1")]
        pub num: u32,
        #[prost(message, optional, tag = "2")]
        pub user: Option<User>,
        #[prost(message, optional, tag = "3")]
        pub position: Option<Position>,
        #[prost(float, tag = "4")]
        pub snr: f32,
        #[prost(fixed32, tag = "5")]
        pub last_heard: u32,
        #[prost(uint32, tag = "7")]
        pub channel: u32,
        #[prost(uint32, optional, tag = "9")]
        pub hops_away: Option<u32>,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct User {
        #[prost(string, tag = "1")]
        pub id: String,
        #[prost(string, tag = "2")]
        pub long_name: String,
        #[prost(string, tag = "3")]
        pub short_name: String,
        #[prost(int32, tag = "5")]
        pub hw_model: i32,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Position {
        #[prost(sfixed32, optional, tag = "1")]
        pub latitude_i: Option<i32>,
        #[prost(sfixed32, optional, tag = "2")]
        pub longitude_i: Option<i32>,
        #[prost(int32, optional, tag = "3")]
        pub altitude: Option<i32>,
        #[prost(fixed32, tag = "4")]
        pub time: u32,
    }
    /// `config.proto` Config. Only the LoRa variant (tag 6) is read, kept as raw bytes so that
    /// writing it back carries every field this client does not model.
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Config {
        #[prost(bytes = "vec", optional, tag = "6")]
        pub lora: Option<Vec<u8>>,
    }
    /// `config.proto` LoRaConfig, the region field only.
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct LoRaRegion {
        #[prost(int32, tag = "7")]
        pub region: i32,
    }
    /// `admin.proto` AdminMessage, set_config only.
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct AdminMessage {
        #[prost(message, optional, tag = "34")]
        pub set_config: Option<Config>,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct RouteDiscovery {
        #[prost(fixed32, repeated, tag = "1")]
        pub route: Vec<u32>,
        #[prost(int32, repeated, tag = "2")]
        pub snr_towards: Vec<i32>,
        #[prost(fixed32, repeated, tag = "3")]
        pub route_back: Vec<u32>,
        #[prost(int32, repeated, tag = "4")]
        pub snr_back: Vec<i32>,
    }
}

pub fn node_id(num: u32) -> String {
    format!("!{num:08x}")
}

pub fn parse_node_id(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim().trim_start_matches('!'), 16).ok()
}

/// `config.proto` LoRaConfig.RegionCode names, indexed by code.
const REGIONS: [&str; 34] = [
    "UNSET", "US", "EU_433", "EU_868", "CN", "JP", "ANZ", "KR", "TW", "RU", "IN", "NZ_865", "TH",
    "LORA_24", "UA_433", "UA_868", "MY_433", "MY_919", "SG_923", "PH_433", "PH_868", "PH_915",
    "ANZ_433", "KZ_433", "KZ_863", "NP_865", "BR_902", "ITU1_2M", "ITU2_2M", "EU_866", "EU_874",
    "EU_917", "EU_N_868", "ITU3_2M",
];

/// A region name (any case) to its code; UNSET and unknown names are `None`.
pub fn region_code(name: &str) -> Option<i32> {
    let i = REGIONS
        .iter()
        .position(|r| r.eq_ignore_ascii_case(name.trim()))?;
    (i > 0).then_some(i as i32)
}

pub fn region_name(code: i32) -> String {
    usize::try_from(code)
        .ok()
        .and_then(|i| REGIONS.get(i))
        .map_or_else(|| code.to_string(), |r| (*r).to_owned())
}

pub fn frame(msg: &pb::ToRadio) -> Vec<u8> {
    let body = msg.encode_to_vec();
    let mut out = vec![START1, START2, (body.len() >> 8) as u8, body.len() as u8];
    out.extend(body);
    out
}

/// Next protobuf frame from the stream; bytes before a header (debug text) are skipped.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Vec<u8>> {
    loop {
        if r.read_u8().await? != START1 {
            continue;
        }
        if r.read_u8().await? != START2 {
            continue;
        }
        let len = usize::from(r.read_u16().await?);
        if len > MAX_FRAME {
            continue;
        }
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf).await?;
        return Ok(buf);
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Node {
    pub id: String,
    pub num: u32,
    pub long_name: String,
    pub short_name: String,
    pub snr: Option<f32>,
    pub rssi: Option<i32>,
    pub hops: Option<u32>,
    pub last_heard: u32,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub alt_m: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TextMessage {
    pub id: u32,
    pub from: String,
    pub to: String,
    pub channel: u32,
    pub text: String,
    pub rx_time: u32,
    pub snr: Option<f32>,
    pub rssi: Option<i32>,
    pub hops: Option<u32>,
    pub outgoing: bool,
}

#[derive(Default)]
pub struct MeshState {
    pub status: String,
    pub my_num: Option<u32>,
    pub nodes: BTreeMap<u32, Node>,
    pub messages: VecDeque<TextMessage>,
    /// The radio's LoRa region code, and its whole LoRaConfig as received.
    pub region: Option<i32>,
    lora: Option<Vec<u8>>,
    traceroutes: HashMap<u32, oneshot::Sender<pb::RouteDiscovery>>,
}

impl MeshState {
    fn node(&mut self, num: u32) -> &mut Node {
        self.nodes.entry(num).or_insert_with(|| Node {
            id: node_id(num),
            num,
            ..Default::default()
        })
    }

    fn push_message(&mut self, m: TextMessage) {
        self.messages.push_back(m);
        while self.messages.len() > 200 {
            self.messages.pop_front();
        }
    }

    /// Applies one FromRadio; returns a text message to announce, if it carried one.
    pub fn apply(&mut self, from_radio: pb::FromRadio) -> Option<TextMessage> {
        use pb::from_radio::Payload;
        match from_radio.payload? {
            Payload::MyInfo(i) => self.my_num = Some(i.my_node_num),
            Payload::NodeInfo(n) => {
                let entry = self.node(n.num);
                if let Some(u) = n.user {
                    entry.long_name = u.long_name;
                    entry.short_name = u.short_name;
                }
                if let Some(p) = n.position {
                    set_position(entry, &p);
                }
                if n.snr != 0.0 {
                    entry.snr = Some(n.snr);
                }
                entry.hops = n.hops_away.or(entry.hops);
                entry.last_heard = entry.last_heard.max(n.last_heard);
            }
            Payload::Config(c) => {
                if let Some(raw) = c.lora {
                    self.region = pb::LoRaRegion::decode(raw.as_slice())
                        .ok()
                        .map(|l| l.region);
                    self.lora = Some(raw);
                }
            }
            Payload::ConfigCompleteId(_) => self.status = "connected".into(),
            Payload::Packet(p) => {
                let hops = (p.hop_start >= p.hop_limit && p.hop_start > 0)
                    .then(|| p.hop_start - p.hop_limit);
                {
                    let n = self.node(p.from);
                    if p.rx_snr != 0.0 {
                        n.snr = Some(p.rx_snr);
                    }
                    if p.rx_rssi != 0 {
                        n.rssi = Some(p.rx_rssi);
                    }
                    n.hops = hops.or(n.hops);
                    n.last_heard = n.last_heard.max(p.rx_time);
                }
                let d = p.decoded?;
                match d.portnum {
                    PORT_TEXT => {
                        let m = TextMessage {
                            id: p.id,
                            from: node_id(p.from),
                            to: if p.to == BROADCAST {
                                "^all".into()
                            } else {
                                node_id(p.to)
                            },
                            channel: p.channel,
                            text: String::from_utf8_lossy(&d.payload).into_owned(),
                            rx_time: p.rx_time,
                            snr: (p.rx_snr != 0.0).then_some(p.rx_snr),
                            rssi: (p.rx_rssi != 0).then_some(p.rx_rssi),
                            hops,
                            outgoing: false,
                        };
                        self.push_message(m.clone());
                        return Some(m);
                    }
                    PORT_POSITION => {
                        if let Ok(pos) = pb::Position::decode(d.payload.as_slice()) {
                            set_position(self.node(p.from), &pos);
                        }
                    }
                    PORT_NODEINFO => {
                        if let Ok(u) = pb::User::decode(d.payload.as_slice()) {
                            let n = self.node(p.from);
                            n.long_name = u.long_name;
                            n.short_name = u.short_name;
                        }
                    }
                    PORT_TRACEROUTE if d.request_id != 0 => {
                        if let (Some(tx), Ok(route)) = (
                            self.traceroutes.remove(&d.request_id),
                            pb::RouteDiscovery::decode(d.payload.as_slice()),
                        ) {
                            let _ = tx.send(route);
                        }
                    }
                    _ => {}
                }
            }
        }
        None
    }

    /// The admin packet that sets the radio's region to `want`, when it is known to differ.
    /// Appending the region to the LoRaConfig as received changes nothing else: for a scalar
    /// field seen twice the last value wins (protobuf, and nanopb in the firmware). meshtasticd
    /// accepts admin from its local client (`from` 0) and restarts itself to apply a region.
    pub fn region_fix(&self, want: i32, id: u32) -> Option<pb::ToRadio> {
        let (me, raw) = (self.my_num?, self.lora.as_ref()?);
        if self.region == Some(want) {
            return None;
        }
        let mut lora = raw.clone();
        lora.extend(pb::LoRaRegion { region: want }.encode_to_vec());
        let admin = pb::AdminMessage {
            set_config: Some(pb::Config { lora: Some(lora) }),
        };
        Some(pb::ToRadio {
            payload: Some(pb::to_radio::Payload::Packet(pb::MeshPacket {
                to: me,
                id,
                decoded: Some(pb::Data {
                    portnum: PORT_ADMIN,
                    payload: admin.encode_to_vec(),
                    ..Default::default()
                }),
                ..Default::default()
            })),
        })
    }

    pub fn snapshot(&self) -> Value {
        json!({
            "status": self.status,
            "region": self.region.map(region_name),
            "my_node": self.my_num.map(node_id),
            "nodes": self.nodes.values().collect::<Vec<_>>(),
            "messages": self.messages.iter().collect::<Vec<_>>(),
        })
    }
}

fn set_position(n: &mut Node, p: &pb::Position) {
    if let (Some(lat), Some(lon)) = (p.latitude_i, p.longitude_i) {
        n.lat = Some(f64::from(lat) * 1e-7);
        n.lon = Some(f64::from(lon) * 1e-7);
    }
    n.alt_m = p.altitude.or(n.alt_m);
}

/// The MESH backend: live meshtasticd, or a deterministic mock mesh.
pub struct Mesh {
    pub state: Arc<Mutex<MeshState>>,
    tx: Mutex<Option<mpsc::UnboundedSender<pb::ToRadio>>>,
    mock: bool,
    next_id: std::sync::atomic::AtomicU32,
}

fn now_u32() -> u32 {
    crate::auth::now() as u32
}

impl Mesh {
    pub fn new(mock: bool) -> Arc<Self> {
        let mut st = MeshState {
            status: "offline: not connected yet".into(),
            ..Default::default()
        };
        if mock {
            st.status = "connected (mock)".into();
            st.my_num = Some(0xdeca_fbad);
            let t = now_u32();
            for (num, long, short, snr, rssi, hops, lat, lon) in [
                (
                    0xdeca_fbadu32,
                    "Lapis Deck",
                    "LDK",
                    None,
                    None,
                    Some(0),
                    37.7749,
                    -122.4194,
                ),
                (
                    0xa1b2_c3d4,
                    "Base Camp",
                    "BASE",
                    Some(9.5),
                    Some(-71),
                    Some(0),
                    37.7790,
                    -122.4170,
                ),
                (
                    0x0bad_f00d,
                    "Ridge Relay",
                    "RDG",
                    Some(4.25),
                    Some(-96),
                    Some(1),
                    37.7925,
                    -122.4380,
                ),
                (
                    0xc0ff_ee01,
                    "Rover",
                    "RVR",
                    Some(-3.5),
                    Some(-114),
                    Some(2),
                    37.8044,
                    -122.4660,
                ),
            ] {
                st.nodes.insert(
                    num,
                    Node {
                        id: node_id(num),
                        num,
                        long_name: long.into(),
                        short_name: short.into(),
                        snr,
                        rssi,
                        hops,
                        last_heard: t,
                        lat: Some(lat),
                        lon: Some(lon),
                        alt_m: Some(30),
                    },
                );
            }
            for (i, (from, text, hops)) in [
                (0xa1b2_c3d4u32, "net check 19:00, all stations", 0),
                (0x0bad_f00d, "relay up on the ridge, 3 bars", 1),
                (0xc0ff_ee01, "rover at trailhead, heading north", 2),
            ]
            .into_iter()
            .enumerate()
            {
                st.messages.push_back(TextMessage {
                    id: i as u32 + 1,
                    from: node_id(from),
                    to: "^all".into(),
                    channel: 0,
                    text: text.into(),
                    rx_time: t,
                    snr: Some(5.0),
                    rssi: Some(-90),
                    hops: Some(hops),
                    outgoing: false,
                });
            }
        }
        Arc::new(Mesh {
            state: Arc::new(Mutex::new(st)),
            tx: Mutex::new(None),
            mock,
            next_id: std::sync::atomic::AtomicU32::new(0x4c00_0000 | (now_u32() & 0xffff)),
        })
    }

    pub fn snapshot(&self) -> Value {
        self.state
            .lock()
            .map(|s| s.snapshot())
            .unwrap_or(Value::Null)
    }

    fn id(&self) -> u32 {
        self.next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn send_text(&self, channel: u32, to: Option<u32>, text: &str) -> Result<Value, String> {
        if text.is_empty() || text.len() > 228 {
            return Err("text must be 1 to 228 bytes (one LoRa packet)".into());
        }
        if channel > 7 {
            return Err("channel is 0 to 7".into());
        }
        let id = self.id();
        let to = to.unwrap_or(BROADCAST);
        if !self.mock {
            let packet = pb::MeshPacket {
                to,
                channel,
                id,
                hop_limit: 3,
                want_ack: to != BROADCAST,
                decoded: Some(pb::Data {
                    portnum: PORT_TEXT,
                    payload: text.as_bytes().to_vec(),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let tx = self
                .tx
                .lock()
                .ok()
                .and_then(|t| t.clone())
                .ok_or("meshtasticd is not connected")?;
            tx.send(pb::ToRadio {
                payload: Some(pb::to_radio::Payload::Packet(packet)),
            })
            .map_err(|_| "meshtasticd is not connected")?;
        }
        let mut st = self.state.lock().map_err(|_| "mesh state unavailable")?;
        let from = st.my_num.map(node_id).unwrap_or_default();
        st.push_message(TextMessage {
            id,
            from,
            to: if to == BROADCAST {
                "^all".into()
            } else {
                node_id(to)
            },
            channel,
            text: text.into(),
            rx_time: now_u32(),
            snr: None,
            rssi: None,
            hops: None,
            outgoing: true,
        });
        Ok(
            json!({"id": id, "channel": channel, "to": if to == BROADCAST { "^all".to_owned() } else { node_id(to) }, "queued": true}),
        )
    }

    pub async fn traceroute(&self, node: u32, timeout: Duration) -> Result<Value, String> {
        if self.mock {
            let route = if node == 0xc0ff_ee01 {
                vec![node_id(0x0bad_f00d)]
            } else {
                Vec::new()
            };
            return Ok(
                json!({"to": node_id(node), "route": route, "route_back": route, "mock": true}),
            );
        }
        let id = self.id();
        let (otx, orx) = oneshot::channel();
        let tx = self
            .tx
            .lock()
            .ok()
            .and_then(|t| t.clone())
            .ok_or("meshtasticd is not connected")?;
        if let Ok(mut st) = self.state.lock() {
            st.traceroutes.insert(id, otx);
        }
        let packet = pb::MeshPacket {
            to: node,
            id,
            hop_limit: 7,
            want_ack: true,
            decoded: Some(pb::Data {
                portnum: PORT_TRACEROUTE,
                payload: pb::RouteDiscovery::default().encode_to_vec(),
                want_response: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        tx.send(pb::ToRadio {
            payload: Some(pb::to_radio::Payload::Packet(packet)),
        })
        .map_err(|_| "meshtasticd is not connected")?;
        let route = tokio::time::timeout(timeout, orx)
            .await
            .map_err(|_| "no traceroute reply in time")?
            .map_err(|_| "traceroute cancelled")?;
        Ok(
            json!({"to": node_id(node), "route": route.route.iter().map(|n| node_id(*n)).collect::<Vec<_>>(),
                  "route_back": route.route_back.iter().map(|n| node_id(*n)).collect::<Vec<_>>(), "snr_towards_db": route.snr_towards.iter().map(|s| *s as f32 / 4.0).collect::<Vec<_>>()}),
        )
    }

    /// Talks to one meshtasticd connection until it drops.
    pub async fn session<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        stream: S,
        region: Option<i32>,
        on_text: &(dyn Fn(TextMessage) + Send + Sync),
    ) -> std::io::Result<()> {
        let (mut r, mut w) = tokio::io::split(stream);
        let (tx, mut rx) = mpsc::unbounded_channel::<pb::ToRadio>();
        if let Ok(mut t) = self.tx.lock() {
            *t = Some(tx.clone());
        }
        w.write_all(&frame(&pb::ToRadio {
            payload: Some(pb::to_radio::Payload::WantConfigId(self.id())),
        }))
        .await?;
        let mut heartbeat = tokio::time::interval(Duration::from_secs(60));
        heartbeat.tick().await;
        let result = loop {
            tokio::select! {
                f = read_frame(&mut r) => match f {
                    Ok(bytes) => {
                        if let Ok(msg) = pb::FromRadio::decode(bytes.as_slice()) {
                            let synced = matches!(msg.payload, Some(pb::from_radio::Payload::ConfigCompleteId(_)));
                            let text = self.state.lock().ok().and_then(|mut s| s.apply(msg));
                            if let Some(t) = text {
                                on_text(t);
                            }
                            if let (true, Some(want)) = (synced, region) {
                                self.set_region(want, &tx);
                            }
                        }
                    }
                    Err(e) => break Err(e),
                },
                Some(out) = rx.recv() => {
                    if let Err(e) = w.write_all(&frame(&out)).await {
                        break Err(e);
                    }
                }
                _ = heartbeat.tick() => {
                    if let Err(e) = w.write_all(&frame(&pb::ToRadio { payload: Some(pb::to_radio::Payload::Heartbeat(pb::Heartbeat {})) })).await {
                        break Err(e);
                    }
                }
            }
        };
        if let Ok(mut t) = self.tx.lock() {
            *t = None;
        }
        result
    }

    /// After the config sync: asks meshtasticd to switch to deck.toml's region if it differs.
    fn set_region(&self, want: i32, tx: &mpsc::UnboundedSender<pb::ToRadio>) {
        let Ok(mut st) = self.state.lock() else {
            return;
        };
        if let Some(fix) = st.region_fix(want, self.id()) {
            let from = st.region.map_or_else(|| "unknown".into(), region_name);
            tracing::info!(%from, to = %region_name(want), "setting the LoRa region from deck.toml");
            st.status = format!(
                "setting region {} (meshtasticd restarts)",
                region_name(want)
            );
            let _ = tx.send(fix);
        }
    }

    /// Keeps a connection to meshtasticd, reconnecting with backoff (LoRa hat absent = offline).
    /// `region`: deck.toml's `mesh_region`, applied after each config sync when it differs.
    pub async fn run(
        self: Arc<Self>,
        addr: String,
        region: Option<i32>,
        on_text: Arc<dyn Fn(TextMessage) + Send + Sync>,
    ) {
        let mut backoff = Duration::from_secs(1);
        loop {
            match tokio::time::timeout(
                Duration::from_secs(5),
                tokio::net::TcpStream::connect(&addr),
            )
            .await
            {
                Ok(Ok(stream)) => {
                    backoff = Duration::from_secs(1);
                    if let Ok(mut s) = self.state.lock() {
                        s.status = "syncing".into();
                    }
                    let res = self.session(stream, region, on_text.as_ref()).await;
                    if let Ok(mut s) = self.state.lock() {
                        s.status = format!(
                            "offline: {}",
                            res.err()
                                .map(|e| e.to_string())
                                .unwrap_or_else(|| "closed".into())
                        );
                    }
                }
                Ok(Err(e)) => {
                    if let Ok(mut s) = self.state.lock() {
                        s.status = format!("offline: meshtasticd not reachable ({e})");
                    }
                }
                Err(_) => {
                    if let Ok(mut s) = self.state.lock() {
                        s.status = "offline: meshtasticd timed out".into();
                    }
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb::from_radio::Payload as FP;

    fn from(p: FP) -> Vec<u8> {
        let body = pb::FromRadio {
            id: 1,
            payload: Some(p),
        }
        .encode_to_vec();
        let mut out = vec![START1, START2, (body.len() >> 8) as u8, body.len() as u8];
        out.extend(body);
        out
    }

    #[tokio::test]
    async fn talks_to_a_fake_meshtasticd() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let want = pb::ToRadio::decode(read_frame(&mut s).await.unwrap().as_slice()).unwrap();
            assert!(matches!(
                want.payload,
                Some(pb::to_radio::Payload::WantConfigId(_))
            ));
            s.write_all(b"debug log text before a frame\n")
                .await
                .unwrap();
            s.write_all(&from(FP::MyInfo(pb::MyNodeInfo {
                my_node_num: 0x1234_5678,
            })))
            .await
            .unwrap();
            let user = pb::User {
                id: "!a1b2c3d4".into(),
                long_name: "Base Camp".into(),
                short_name: "BASE".into(),
                hw_model: 0,
            };
            let pos = pb::Position {
                latitude_i: Some(377_749_000),
                longitude_i: Some(-1_224_194_000),
                altitude: Some(12),
                time: 0,
            };
            s.write_all(&from(FP::NodeInfo(pb::NodeInfo {
                num: 0xa1b2_c3d4,
                user: Some(user),
                position: Some(pos),
                snr: 7.5,
                last_heard: 100,
                channel: 0,
                hops_away: Some(1),
            })))
            .await
            .unwrap();
            // A new radio: LoRaConfig with no region (hop_limit 3 must survive the rewrite).
            let lora = [8u8 << 3, 3].to_vec();
            s.write_all(&from(FP::Config(pb::Config { lora: Some(lora) })))
                .await
                .unwrap();
            s.write_all(&from(FP::ConfigCompleteId(9))).await.unwrap();
            // deck.toml says US: the deck asks its own node to switch.
            let out = pb::ToRadio::decode(read_frame(&mut s).await.unwrap().as_slice()).unwrap();
            let Some(pb::to_radio::Payload::Packet(p)) = out.payload else {
                panic!("no admin packet")
            };
            let d = p.decoded.unwrap();
            assert_eq!((p.to, d.portnum), (0x1234_5678, PORT_ADMIN));
            let admin = pb::AdminMessage::decode(d.payload.as_slice()).unwrap();
            let lora = admin.set_config.unwrap().lora.unwrap();
            assert_eq!(pb::LoRaRegion::decode(lora.as_slice()).unwrap().region, 1);
            assert_eq!(&lora[..2], &[8u8 << 3, 3]);
            let text = pb::MeshPacket {
                from: 0xa1b2_c3d4,
                to: BROADCAST,
                rx_snr: 6.0,
                rx_rssi: -88,
                hop_start: 3,
                hop_limit: 2,
                id: 77,
                decoded: Some(pb::Data {
                    portnum: PORT_TEXT,
                    payload: b"hello deck".to_vec(),
                    ..Default::default()
                }),
                ..Default::default()
            };
            s.write_all(&from(FP::Packet(text))).await.unwrap();
            // The deck's own message arrives as a ToRadio packet.
            let out = pb::ToRadio::decode(read_frame(&mut s).await.unwrap().as_slice()).unwrap();
            let Some(pb::to_radio::Payload::Packet(p)) = out.payload else {
                panic!("no packet")
            };
            assert_eq!(p.decoded.unwrap().payload, b"copy that".to_vec());
            assert_eq!(p.to, BROADCAST);
        });
        let mesh = Mesh::new(false);
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        let m = mesh.clone();
        let client = tokio::spawn(async move {
            let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let cb = move |t: TextMessage| g.lock().unwrap().push(t.text);
            let _ = m.session(stream, region_code("us"), &cb).await;
        });
        for _ in 0..200 {
            if !got.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(*got.lock().unwrap(), vec!["hello deck".to_string()]);
        let snap = mesh.snapshot();
        assert_eq!(snap["my_node"], "!12345678");
        assert_eq!(snap["status"], "setting region US (meshtasticd restarts)");
        assert_eq!(snap["region"], "UNSET");
        let base = snap["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == "!a1b2c3d4")
            .unwrap()
            .clone();
        assert_eq!(
            (
                base["long_name"].as_str(),
                base["hops"].as_u64(),
                base["rssi"].as_i64()
            ),
            (Some("Base Camp"), Some(1), Some(-88))
        );
        assert!((base["lat"].as_f64().unwrap() - 37.7749).abs() < 1e-6);
        mesh.send_text(0, None, "copy that").unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        client.abort();
        assert!(mesh.send_text(9, None, "x").is_err());
        assert_eq!(parse_node_id("!a1b2c3d4"), Some(0xa1b2_c3d4));
    }

    #[test]
    fn region_names_and_fix() {
        assert_eq!(region_code(" eu_868 "), Some(3));
        assert_eq!((region_code("UNSET"), region_code("XX")), (None, None));
        assert_eq!(
            (region_name(1), region_name(99)),
            ("US".into(), "99".into())
        );
        let mut st = MeshState::default();
        let lora = |region: i32| pb::Config {
            lora: Some(pb::LoRaRegion { region }.encode_to_vec()),
        };
        st.apply(pb::FromRadio {
            id: 1,
            payload: Some(FP::Config(lora(1))),
        });
        assert!(st.region_fix(1, 7).is_none(), "no node number yet");
        st.my_num = Some(5);
        assert!(st.region_fix(1, 7).is_none(), "already US");
        assert!(st.region_fix(3, 7).is_some());
    }

    #[tokio::test]
    async fn mock_mesh() {
        let m = Mesh::new(true);
        assert_eq!(m.snapshot()["nodes"].as_array().unwrap().len(), 4);
        m.send_text(0, None, "test").unwrap();
        assert_eq!(
            m.snapshot()["messages"].as_array().unwrap().last().unwrap()["outgoing"],
            true
        );
        let tr = m
            .traceroute(0xc0ff_ee01, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(tr["route"][0], "!0badf00d");
    }
}
