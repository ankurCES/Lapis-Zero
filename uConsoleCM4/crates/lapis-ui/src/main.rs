//! `lapis-ui [--mock-hw] [--daemon URL] [--token-file PATH] [--backend kms|desktop]
//!           [--card /dev/dri/cardN] [--rotate 0|90|180|270] [--pads a=0x121,b=0x122,...]
//!           [--trackball-gain G] [--workspace N] [--screenshot FILE.png] [--show-help]`

use lapis_ui::app::DeckApp;
use lapis_ui::client::Client;
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct Args {
    mock: bool,
    daemon: String,
    token_file: Option<PathBuf>,
    backend: Option<String>,
    card: Option<PathBuf>,
    rotate: Option<u32>,
    pads: lapis_ui::input::PadMap,
    gain: f32,
    workspace: usize,
    screenshot: Option<PathBuf>,
    show_help: bool,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        mock: false,
        daemon: "http://127.0.0.1:8421".into(),
        token_file: None,
        backend: None,
        card: None,
        rotate: None,
        pads: Default::default(),
        gain: 1.5,
        workspace: 1,
        screenshot: None,
        show_help: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--mock-hw" | "--mock" => a.mock = true,
            "--daemon" => a.daemon = val()?,
            "--token-file" => a.token_file = Some(val()?.into()),
            "--backend" => a.backend = Some(val()?),
            "--card" => a.card = Some(val()?.into()),
            "--rotate" => {
                a.rotate = Some(
                    val()?
                        .parse()
                        .map_err(|_| "--rotate takes 0, 90, 180 or 270")?,
                )
            }
            "--pads" => a.pads = lapis_ui::input::PadMap::parse(&val()?)?,
            "--trackball-gain" => {
                a.gain = val()?
                    .parse()
                    .map_err(|_| "--trackball-gain takes a number")?
            }
            "--workspace" => a.workspace = val()?.parse().map_err(|_| "--workspace takes 1..7")?,
            "--screenshot" => a.screenshot = Some(val()?.into()),
            "--show-help" => a.show_help = true,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if !a.daemon.starts_with("http://127.0.0.1:") && !a.daemon.starts_with("http://localhost:") {
        return Err("--daemon must be the loopback control plane (http://127.0.0.1:PORT)".into());
    }
    Ok(a)
}

/// The daemon writes the token at start; wait a little for it on boot.
fn token(path: &PathBuf) -> String {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(t) = std::fs::read_to_string(path) {
            if t.trim().len() >= 32 {
                return t.trim().to_owned();
            }
        }
        if Instant::now() > until {
            eprintln!(
                "lapis-ui: no token in {} (is lapis-daemon running?); continuing offline",
                path.display()
            );
            return String::new();
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn main() {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\nusage: lapis-ui [--mock-hw] [--daemon URL] [--token-file PATH] [--backend kms|desktop] [--card PATH] [--rotate DEG] [--pads MAP] [--trackball-gain G] [--workspace N] [--screenshot FILE] [--show-help]");
            std::process::exit(2);
        }
    };
    let token_path = args.token_file.clone().unwrap_or_else(|| {
        if args.mock {
            std::env::temp_dir().join("lapis-deck-mock/local.token")
        } else {
            "/var/lib/lapis/local.token".into()
        }
    });
    let tok = token(&token_path);
    let daemon = args.daemon.clone();
    let (ws, shot, show_help) = (
        args.workspace.clamp(1, 7) - 1,
        args.screenshot.clone(),
        args.show_help,
    );
    // The controls help opens once on the device, until it is closed the first time.
    let marker = (!args.mock && shot.is_none())
        .then(|| PathBuf::from("/var/lib/lapis/harness/.ui-help-seen"))
        .filter(|m| !m.exists());
    let make = move |wake| {
        let mut app = DeckApp::new(Client::start(&daemon, tok, wake));
        app.ws = ws;
        app.help = marker.is_some() || show_help;
        app.help_marker = marker;
        if let Some(p) = shot {
            // Let a few seconds of live data arrive before the capture.
            app.screenshot_after = Some((90, p));
        }
        app
    };
    let backend = args.backend.clone().unwrap_or_else(|| {
        if cfg!(all(target_os = "linux", feature = "kms")) && !args.mock {
            "kms".into()
        } else {
            "desktop".into()
        }
    });
    let result: Result<(), String> = match backend.as_str() {
        #[cfg(all(target_os = "linux", feature = "kms"))]
        "kms" => lapis_ui::kms::run(make, lapis_ui::kms::Options { card: args.card.clone(), rotate: args.rotate, pads: args.pads, trackball_gain: args.gain }),
        #[cfg(feature = "desktop")]
        "desktop" => lapis_ui::desktop::run(make, args.screenshot.is_some()),
        other => Err(format!("backend {other:?} is not built into this binary (features: desktop for host preview, kms for the device)")),
    };
    let _ = (&args.card, args.rotate, args.gain, args.pads);
    if let Err(e) = result {
        eprintln!("lapis-ui: {e}");
        if backend == "kms" {
            // The device has no other console: say it on the panel itself (fbcon on tty1).
            tell_console(&e);
            // systemd restarts the unit; do not spin.
            std::thread::sleep(Duration::from_secs(5));
        }
        std::process::exit(1);
    }
}

fn tell_console(msg: &str) {
    use std::io::Write;
    if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty1") {
        let _ = write!(tty, "\r\n\x1b[1;31mLapis-Deck HUD could not start:\x1b[0m\r\n{msg}\r\n(details: lapis-boot-report.txt on the card's bootfs partition)\r\n");
    }
}
