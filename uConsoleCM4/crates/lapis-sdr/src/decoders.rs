//! Decoder adapters: command lines for `rtl_433`, `readsb` and `multimon-ng`, and parsers for
//! what they print. The daemon runs the commands; the synthetic source replays the samples
//! below through the same parsers.

use serde_json::{json, Value};

/// `rtl_433` for `duration_s` on `freq_hz`, one JSON object per decoded packet.
pub fn rtl433_argv(freq_hz: u64, duration_s: u64) -> Vec<String> {
    [
        "rtl_433",
        "-f",
        &freq_hz.to_string(),
        "-F",
        "json",
        "-M",
        "time:unix",
        "-M",
        "level",
        "-T",
        &duration_s.to_string(),
    ]
    .map(String::from)
    .to_vec()
}

/// `rtl_fm | multimon-ng` (POCSAG 512/1200/2400 and APRS AFSK1200) as one `sh -c` script.
pub fn multimon_argv(freq_hz: u64, duration_s: u64) -> Vec<String> {
    let script = format!(
        "timeout {duration_s} rtl_fm -f {freq_hz} -M fm -s 22050 - | multimon-ng -t raw -a POCSAG512 -a POCSAG1200 -a POCSAG2400 -a AFSK1200 -"
    );
    vec!["sh".into(), "-c".into(), script]
}

/// Where readsb (the `readsb.service` the image installs) writes its aircraft table.
pub const READSB_AIRCRAFT: &str = "/run/readsb/aircraft.json";

pub fn parse_rtl433(output: &str) -> Vec<Value> {
    output
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter(|v| v.get("model").is_some())
        .collect()
}

pub fn parse_readsb(text: &str) -> Value {
    let data: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let mut aircraft: Vec<Value> = data["aircraft"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|a| {
                    json!({
                        "hex": a["hex"], "flight": a["flight"].as_str().map(str::trim), "alt_baro": a["alt_baro"],
                        "gs": a["gs"], "track": a["track"], "lat": a["lat"], "lon": a["lon"], "squawk": a["squawk"],
                        "rssi": a["rssi"], "seen": a["seen"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    aircraft.sort_by(|a, b| {
        a["seen"]
            .as_f64()
            .unwrap_or(1e9)
            .total_cmp(&b["seen"].as_f64().unwrap_or(1e9))
    });
    json!({"now": data["now"], "messages": data["messages"], "aircraft": aircraft})
}

/// One `multimon-ng` line: POCSAG pages and APRS frame headers.
pub fn parse_multimon(line: &str) -> Option<Value> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("POCSAG") {
        let (rate, rest) = rest.split_once(':')?;
        let field = |name: &str| {
            rest.split_once(&format!("{name}:")).map(|(_, v)| {
                v.trim_start()
                    .split("  ")
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_owned()
            })
        };
        let message = rest
            .split_once("Alpha:")
            .or_else(|| rest.split_once("Numeric:"))
            .map(|(_, m)| m.trim().to_owned())
            .unwrap_or_default();
        return Some(
            json!({"proto": format!("POCSAG{rate}"), "address": field("Address")?, "function": field("Function"), "message": message}),
        );
    }
    if let Some(rest) = line.strip_prefix("AFSK1200:") {
        let rest = rest.trim();
        let rest = rest.strip_prefix("fm ")?;
        let (from, rest) = rest.split_once(" to ")?;
        let to = rest.split_whitespace().next()?;
        let path = rest
            .split_once(" via ")
            .map(|(_, p)| p.split_whitespace().next().unwrap_or("").to_owned());
        return Some(json!({"proto": "APRS", "from": from, "to": to, "path": path}));
    }
    None
}

pub const SAMPLE_RTL433: &str = r#"{"time":"1764100000","model":"Acurite-Tower","id":11425,"channel":"A","battery_ok":1,"temperature_C":18.4,"humidity":61,"mic":"CHECKSUM","rssi":-4.1,"snr":21.0}
{"time":"1764100003","model":"LaCrosse-TX141THBv2","id":201,"channel":0,"battery_ok":1,"temperature_C":21.7,"humidity":48,"test":"No","mic":"CRC","rssi":-11.6,"snr":14.2}
not json at all
{"time":"1764100007","model":"Schrader-EG53MA4","type":"TPMS","flags":"f7","id":"0B3CD9A1","pressure_kPa":231.0,"temperature_C":16.0,"mic":"CHECKSUM"}"#;

pub const SAMPLE_AIRCRAFT: &str = r#"{"now":1764100000.0,"messages":48211,"aircraft":[
{"hex":"a1b2c3","flight":"UAL1543 ","alt_baro":34000,"gs":451.2,"track":271.4,"lat":37.71,"lon":-122.33,"squawk":"4213","rssi":-18.2,"seen":0.4},
{"hex":"c0ffee","flight":"N172SP  ","alt_baro":4500,"gs":101.0,"track":90.0,"lat":37.52,"lon":-122.25,"squawk":"1200","rssi":-24.9,"seen":2.1},
{"hex":"abc123","alt_baro":"ground","rssi":-30.0,"seen":12.0}]}"#;

pub const SAMPLE_MULTIMON: &str = "multimon-ng 1.3.1\nPOCSAG1200: Address:  123456  Function: 3  Alpha:   FIRE STATION 4 TONE TEST\nAFSK1200: fm KJ6ABC-9 to APDR16 via WIDE1-1,WIDE2-1 UI  pid=F0\n=3745.12N/12224.55W>Lapis-Deck test\nPOCSAG512: Address:   8000  Function: 0  Numeric:  5551234\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsers() {
        let ism = parse_rtl433(SAMPLE_RTL433);
        assert_eq!(ism.len(), 3);
        assert_eq!(ism[0]["model"], "Acurite-Tower");
        let adsb = parse_readsb(SAMPLE_AIRCRAFT);
        assert_eq!(adsb["aircraft"][0]["flight"], "UAL1543");
        assert_eq!(adsb["aircraft"].as_array().unwrap().len(), 3);
        let msgs: Vec<Value> = SAMPLE_MULTIMON.lines().filter_map(parse_multimon).collect();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["message"], "FIRE STATION 4 TONE TEST");
        assert_eq!(msgs[0]["address"], "123456");
        assert_eq!(msgs[1]["from"], "KJ6ABC-9");
        assert_eq!(msgs[1]["path"], "WIDE1-1,WIDE2-1");
        assert_eq!(msgs[2]["message"], "5551234");
        assert_eq!(
            rtl433_argv(433_920_000, 30).join(" "),
            "rtl_433 -f 433920000 -F json -M time:unix -M level -T 30"
        );
        assert!(multimon_argv(152_007_500, 20)[2].starts_with("timeout 20 rtl_fm -f 152007500"));
    }
}
