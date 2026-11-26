//! SDR hardware on the USB bus (sysfs: no tools needed), RTL-SDR and SoapySDR alike.

use serde_json::{json, Value};
use std::path::Path;

/// (vendor, product, kind, driver) of the receivers SPEC 1.2 lists.
pub const KNOWN: [(&str, &str, &str, &str); 6] = [
    ("0bda", "2838", "RTL-SDR (RTL2838)", "rtlsdr"),
    ("0bda", "2832", "RTL-SDR (RTL2832U)", "rtlsdr"),
    ("1d50", "6089", "HackRF One", "hackrf"),
    ("1d50", "60a1", "Airspy", "airspy"),
    ("0456", "b673", "ADALM-PlutoSDR", "plutosdr"),
    ("1d50", "cc15", "RTL-SDR Blog V4 (rad1o mode)", "rtlsdr"),
];

/// SDRs under `<root>/sys/bus/usb/devices`, with whether each can transmit.
pub fn detect_usb(root: &Path) -> Vec<Value> {
    let Ok(dir) = std::fs::read_dir(root.join("sys/bus/usb/devices")) else {
        return Vec::new();
    };
    let read = |p: &Path, f: &str| {
        std::fs::read_to_string(p.join(f))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    };
    let mut found: Vec<Value> = dir
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let (vid, pid) = (read(&p, "idVendor"), read(&p, "idProduct"));
            let (_, _, kind, driver) = KNOWN.iter().find(|k| k.0 == vid && k.1 == pid)?;
            Some(json!({
                "bus_id": e.file_name().to_string_lossy(),
                "kind": kind, "driver": driver, "vid_pid": format!("{vid}:{pid}"),
                "product": read(&p, "product"), "manufacturer": read(&p, "manufacturer"), "serial": read(&p, "serial"),
                "can_transmit": matches!(*driver, "hackrf" | "plutosdr"),
            }))
        })
        .collect();
    found.sort_by(|a, b| a["bus_id"].as_str().cmp(&b["bus_id"].as_str()));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_rtl_and_hackrf() {
        let d = std::env::temp_dir().join(format!("lapis-sdr-usb-{}", std::process::id()));
        for (bus, vid, pid, product) in [
            ("1-1.2", "0bda", "2838", "RTL2838UHIDIR"),
            ("1-1.3", "1d50", "6089", "HackRF One"),
            ("1-1.4", "046d", "c52b", "Receiver"),
        ] {
            let p = d.join("sys/bus/usb/devices").join(bus);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("idVendor"), vid).unwrap();
            std::fs::write(p.join("idProduct"), pid).unwrap();
            std::fs::write(p.join("product"), product).unwrap();
        }
        let found = detect_usb(&d);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["driver"], "rtlsdr");
        assert_eq!(found[1]["can_transmit"], true);
        assert!(detect_usb(Path::new("/nonexistent")).is_empty());
        let _ = std::fs::remove_dir_all(d);
    }
}
