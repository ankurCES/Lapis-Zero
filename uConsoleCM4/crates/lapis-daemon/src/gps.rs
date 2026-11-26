//! gpsd client (JSON over TCP 2947): fix (TPV), satellites (SKY) and the PPS clock offset.

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

pub const WATCH: &str = "?WATCH={\"enable\":true,\"json\":true,\"pps\":true};\n";

#[derive(Default)]
pub struct GpsState {
    pub status: String,
    pub fix: Value,
    pub satellites: Vec<Value>,
    pub hdop: Option<f64>,
    pub pps_offset_ns: Option<i64>,
    pub device: String,
}

impl GpsState {
    pub fn apply(&mut self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match v["class"].as_str() {
            Some("TPV") => {
                let mode = v["mode"].as_u64().unwrap_or(0);
                self.fix = json!({
                    "mode": mode, "fix": match mode { 3 => "3D", 2 => "2D", _ => "none" },
                    "lat": v["lat"], "lon": v["lon"], "alt_m": if v["altHAE"].is_null() { v["alt"].clone() } else { v["altHAE"].clone() },
                    "speed_mps": v["speed"], "track_deg": v["track"], "time": v["time"], "eph_m": v["eph"],
                });
                if let Some(d) = v["device"].as_str() {
                    self.device = d.to_owned();
                }
            }
            Some("SKY") => {
                if let Some(sats) = v["satellites"].as_array() {
                    self.satellites = sats
                        .iter()
                        .map(|s| json!({"prn": s["PRN"], "el": s["el"], "az": s["az"], "snr": s["ss"], "used": s["used"].as_bool().unwrap_or(false), "gnss": s["gnssid"]}))
                        .collect();
                }
                self.hdop = v["hdop"].as_f64().or(self.hdop);
            }
            Some("PPS") | Some("TOFF") => {
                let ns = |s: &str, n: &str| {
                    v[s].as_i64().unwrap_or(0) * 1_000_000_000 + v[n].as_i64().unwrap_or(0)
                };
                self.pps_offset_ns =
                    Some(ns("clock_sec", "clock_nsec") - ns("real_sec", "real_nsec"));
            }
            _ => {}
        }
    }

    pub fn snapshot(&self) -> Value {
        let used = self.satellites.iter().filter(|s| s["used"] == true).count();
        json!({"status": self.status, "device": self.device, "fix": self.fix, "satellites": self.satellites,
               "visible": self.satellites.len(), "used": used, "hdop": self.hdop, "pps_offset_ns": self.pps_offset_ns,
               "pps_locked": self.pps_offset_ns.is_some_and(|o| o.abs() < 1_000_000)})
    }
}

pub struct Gps {
    pub state: Arc<Mutex<GpsState>>,
}

pub const MOCK_LINES: [&str; 3] = [
    r#"{"class":"TPV","device":"/dev/ttyS0","mode":3,"time":"2026-11-25T18:00:00.000Z","lat":37.774900,"lon":-122.419400,"altHAE":16.2,"speed":0.31,"track":12.5,"eph":3.1}"#,
    r#"{"class":"SKY","device":"/dev/ttyS0","hdop":0.92,"satellites":[{"PRN":2,"el":61,"az":288,"ss":41,"used":true,"gnssid":0},{"PRN":5,"el":12,"az":40,"ss":22,"used":false,"gnssid":0},{"PRN":12,"el":45,"az":130,"ss":38,"used":true,"gnssid":0},{"PRN":15,"el":33,"az":200,"ss":35,"used":true,"gnssid":0},{"PRN":24,"el":72,"az":12,"ss":44,"used":true,"gnssid":0},{"PRN":25,"el":20,"az":315,"ss":28,"used":true,"gnssid":0},{"PRN":29,"el":8,"az":95,"ss":19,"used":false,"gnssid":0},{"PRN":67,"el":38,"az":250,"ss":33,"used":true,"gnssid":6},{"PRN":81,"el":54,"az":170,"ss":40,"used":true,"gnssid":6}]}"#,
    r#"{"class":"PPS","device":"/dev/pps0","real_sec":1764093600,"real_nsec":0,"clock_sec":1764093600,"clock_nsec":120}"#,
];

impl Gps {
    pub fn new(mock: bool) -> Arc<Self> {
        let mut st = GpsState {
            status: "offline: not connected yet".into(),
            ..Default::default()
        };
        if mock {
            st.status = "connected (mock)".into();
            for l in MOCK_LINES {
                st.apply(l);
            }
        }
        Arc::new(Gps {
            state: Arc::new(Mutex::new(st)),
        })
    }

    pub fn snapshot(&self) -> Value {
        self.state
            .lock()
            .map(|s| s.snapshot())
            .unwrap_or(Value::Null)
    }

    pub async fn session<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        stream: S,
    ) -> std::io::Result<()> {
        let (r, mut w) = tokio::io::split(stream);
        w.write_all(WATCH.as_bytes()).await?;
        if let Ok(mut s) = self.state.lock() {
            s.status = "connected".into();
        }
        let mut lines = BufReader::new(r).lines();
        while let Some(line) = lines.next_line().await? {
            if let Ok(mut s) = self.state.lock() {
                s.apply(&line);
            }
        }
        Ok(())
    }

    pub async fn run(self: Arc<Self>, addr: String) {
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
                    let res = self.session(stream).await;
                    if let Ok(mut s) = self.state.lock() {
                        s.status = format!(
                            "offline: {}",
                            res.err()
                                .map(|e| e.to_string())
                                .unwrap_or_else(|| "gpsd closed".into())
                        );
                    }
                }
                _ => {
                    if let Ok(mut s) = self.state.lock() {
                        s.status =
                            "offline: gpsd not reachable (GPS rail off or no AIO board?)".into();
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
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn gpsd_session() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; WATCH.len()];
            s.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, WATCH.as_bytes());
            s.write_all(b"{\"class\":\"VERSION\",\"release\":\"3.25\"}\n")
                .await
                .unwrap();
            for line in MOCK_LINES {
                s.write_all(format!("{line}\n").as_bytes()).await.unwrap();
            }
        });
        let gps = Gps::new(false);
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        gps.session(stream).await.unwrap();
        let snap = gps.snapshot();
        assert_eq!(
            (
                snap["fix"]["fix"].as_str(),
                snap["visible"].as_u64(),
                snap["used"].as_u64()
            ),
            (Some("3D"), Some(9), Some(7))
        );
        assert_eq!(snap["fix"]["alt_m"], 16.2);
        assert_eq!(
            (snap["pps_offset_ns"].as_i64(), snap["pps_locked"].as_bool()),
            (Some(120), Some(true))
        );
        assert_eq!(Gps::new(true).snapshot()["fix"]["lat"], 37.7749);
    }
}
