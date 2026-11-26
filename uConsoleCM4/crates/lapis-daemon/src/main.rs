//! `lapis-daemon [--mock-hw] [--state-dir DIR] [--harness-socket PATH] [--config FILE]
//!               [--dev-pair-token TOKEN] [--dev-advertise HOST:PORT] [--port N]`

use lapis_daemon::app::{App, Parts};
use lapis_daemon::config::{DeckConfig, Paths, PORT};
use lapis_daemon::hw::{Hardware, LinuxHardware, MockHardware, SystemRunner};
use lapis_daemon::net::{LinuxNet, MockNet, NetControl};
use std::path::PathBuf;
use std::sync::Arc;

struct Args {
    mock: bool,
    state_dir: Option<PathBuf>,
    socket: Option<PathBuf>,
    config: PathBuf,
    dev_token: Option<String>,
    dev_advertise: Option<String>,
    port: u16,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        mock: false,
        state_dir: None,
        socket: None,
        config: "/etc/lapis/deck.toml".into(),
        dev_token: None,
        dev_advertise: None,
        port: PORT,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--mock-hw" | "--mock" => a.mock = true,
            "--state-dir" => a.state_dir = Some(val()?.into()),
            "--harness-socket" => a.socket = Some(val()?.into()),
            "--config" => a.config = val()?.into(),
            "--dev-pair-token" => a.dev_token = Some(val()?),
            "--dev-advertise" => a.dev_advertise = Some(val()?),
            "--port" => a.port = val()?.parse().map_err(|_| "--port needs a number")?,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if (a.dev_token.is_some() || a.dev_advertise.is_some()) && !a.mock {
        return Err(
            "--dev-pair-token and --dev-advertise are development flags: they need --mock-hw"
                .into(),
        );
    }
    Ok(a)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    lapis_daemon::init_logging();
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\nusage: lapis-daemon [--mock-hw] [--state-dir DIR] [--harness-socket PATH] [--config FILE] [--dev-pair-token T] [--dev-advertise HOST:PORT] [--port N]");
            std::process::exit(2);
        }
    };
    let cfg = DeckConfig::load(&args.config)?;
    let tmp = std::env::temp_dir().join("lapis-deck-mock");
    let paths = Paths {
        state_dir: args.state_dir.unwrap_or_else(|| {
            if args.mock {
                tmp.clone()
            } else {
                "/var/lib/lapis".into()
            }
        }),
        harness_socket: args.socket.unwrap_or_else(|| {
            if args.mock {
                tmp.join("harness.sock")
            } else {
                "/run/lapis/harness.sock".into()
            }
        }),
    };
    let runner = Arc::new(SystemRunner);
    let (hw, net): (Arc<dyn Hardware>, Arc<dyn NetControl>) = if args.mock {
        (
            Arc::new(MockHardware::default()),
            Arc::new(MockNet::default()),
        )
    } else {
        (
            Arc::new(LinuxHardware::new(
                "/",
                runner.clone(),
                cfg.rails.clone(),
                &cfg.wifi_iface,
            )),
            Arc::new(LinuxNet::new(runner, &cfg.wifi_iface)),
        )
    };
    let app = App::new(Parts {
        cfg,
        paths: paths.clone(),
        mock: args.mock,
        hw,
        net_ctl: net,
        dev_advertise: args.dev_advertise,
    })?;
    if let Some(t) = args.dev_token {
        app.auth.register_fixed("dev_local", "Dev phone", &t);
        tracing::info!("development pairing token registered (device dev_local)");
    }
    let listener = lapis_daemon::bind(args.port).await?;
    tracing::info!(addr = %listener.local_addr()?, state = %paths.state_dir.display(), mock = args.mock, "lapis-daemon listening");
    // First boot of an unpaired device: open the Bluetooth pairing window by itself.
    if !args.mock && app.auth.paired_count() == 0 {
        let a = app.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let _ = a.pairing_start("auto").await;
        });
    }
    lapis_daemon::serve(app, listener).await?;
    Ok(())
}
