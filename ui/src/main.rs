//! `lapis-ui`: Lapis-Zero OS 320x170 UI. CLI: `--sim`, `--demo`, `--dump-frame`, `--socket`.

use std::fs::File;
use std::io::BufWriter;
use std::process::ExitCode;

use lapis_ui::gfx::canvas::{Canvas, HEIGHT, WIDTH};
use lapis_ui::ipc::{selected_socket_path, IpcClient};
use lapis_ui::views::{App, View, DUMP_CLOCK};

const HELP: &str = "\
lapis-ui - Lapis-Zero OS display UI

USAGE:
    lapis-ui [--sim] [--demo] [--view <name>] [--socket <path>]
    lapis-ui --dump-frame <path.png> [--view <name>] [--socket <path>]

OPTIONS:
    --sim                 Open the 960x510 SDL2 desktop simulator
    --demo                Demo data mode (state cycling + synthetic approval modal)
    --fb                  Run on CM0 /dev/fb0 + evdev (Linux only)
    --socket <path>       Daemon IPC socket (default /run/lapis/lapisd.sock on Linux,
                          /tmp/lapis/lapisd.sock elsewhere)
    --dump-frame <path>   Render one 320x170 frame to an RGB PNG and exit
    --view <name>         companion|ticker|menu|approval|prompt|setup (default companion)
    --help                Show this help
";

/// Parsed command line.
#[derive(Default)]
struct Args {
    sim: bool,
    demo: bool,
    fb: bool,
    dump: Option<String>,
    view: Option<String>,
    socket: Option<String>,
}

fn usage_err(msg: &str) -> ExitCode {
    eprintln!("lapis-ui: {msg}");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--sim" => args.sim = true,
            "--demo" => args.demo = true,
            "--fb" => args.fb = true,
            "--dump-frame" | "--view" | "--socket" => {
                let Some(v) = it.next() else {
                    return usage_err(&format!("{a} needs a value"));
                };
                match a.as_str() {
                    "--view" => args.view = Some(v),
                    "--socket" => args.socket = Some(v),
                    _ => args.dump = Some(v),
                }
            }
            "--help" | "-h" => {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            other => return usage_err(&format!("unknown argument {other:?} (see --help)")),
        }
    }

    let view = match args.view.as_deref() {
        None => View::Companion,
        Some(name) => match View::from_name(name) {
            Some(v) => v,
            None => {
                let names: Vec<_> = View::ALL.iter().map(|v| v.name()).collect();
                return usage_err(&format!(
                    "unknown view {name:?}; expected one of: {}",
                    names.join(", ")
                ));
            }
        },
    };

    if let Some(path) = args.dump {
        let mut app = App::new();
        app.set_view(view);
        if view == View::Approval {
            app.show_synthetic_approval();
        }
        // Only an explicit --socket makes the dump depend on a live daemon.
        if let Some(sock) = args.socket.as_deref() {
            sync_once(&mut app, sock);
            app.set_view(view);
        }
        // Settled, deterministic snapshot: pinned desk clock, no slide/motion,
        // and any modal snap-rested so borders/preview land predictably.
        app.settle_all();
        app.tick(1.0 / 60.0, DUMP_CLOCK as f32 * 60.0);
        let mut canvas = Canvas::new();
        app.render(&mut canvas);
        return match write_png(&path, &canvas) {
            Ok(()) => {
                println!("wrote {path} ({WIDTH}x{HEIGHT}, view {})", view.name());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("lapis-ui: failed to write {path}: {e}");
                ExitCode::FAILURE
            }
        };
    }

    if args.fb {
        return run_fb(
            view,
            args.demo,
            &selected_socket_path(args.socket.as_deref()),
        );
    }

    if args.sim {
        return run_sim(
            view,
            args.demo,
            &selected_socket_path(args.socket.as_deref()),
        );
    }

    print!("{HELP}");
    ExitCode::from(2)
}

/// Best-effort single sync for `--dump-frame --socket`: connect, then give the
/// daemon up to 250 ms to answer `request_sync`. No daemon, no change.
fn sync_once(app: &mut App, socket: &str) {
    let mut ipc = IpcClient::new(socket);
    if let Err(e) = ipc.connect_ready(0.0) {
        eprintln!("lapis-ui: {e}");
    }
    let mut got = false;
    for _ in 0..25 {
        if got || !ipc.is_connected() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        ipc.poll(0.0, |v| {
            got = true;
            app.on_daemon_message(v);
        });
    }
}

/// Writes `canvas` as an 8-bit RGB PNG.
fn write_png(path: &str, canvas: &Canvas) -> Result<(), Box<dyn std::error::Error>> {
    let mut enc = png::Encoder::new(
        BufWriter::new(File::create(path)?),
        WIDTH as u32,
        HEIGHT as u32,
    );
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header()?;
    w.write_image_data(canvas.pixels())?;
    w.finish()?;
    Ok(())
}

#[cfg(feature = "sim")]
fn run_sim(view: View, demo: bool, socket: &str) -> ExitCode {
    use lapis_ui::backend::sdl_sim::SimBackend;
    use std::time::Instant;

    let mut sim = match SimBackend::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lapis-ui: simulator init failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut app = App::new();
    app.set_view(view);
    // Always created; inert (demo state stays) until a daemon is listening.
    let mut ipc = IpcClient::new(socket);
    let start = Instant::now();
    let mut prev_now = 0.0f32;
    let mut canvas = Canvas::new();

    loop {
        if sim.should_quit() {
            break;
        }
        let now = start.elapsed().as_secs_f32();
        let dt = (now - prev_now).clamp(0.0, 0.1);
        prev_now = now;
        // ponytail: the socket is polled once per frame, so an idle UI (1 FPS)
        // reacts to the daemon within 1s; poll(2) on the fd if that is too slow.
        if let Err(e) = ipc.connect_ready(now) {
            eprintln!("lapis-ui: {e}");
        }
        ipc.poll(now, |v| app.on_daemon_message(v));
        // Drain at most one key per frame; the rest queue in SDL.
        if let Some(msg) = sim.poll().and_then(|k| app.key(k)) {
            if let Err(e) = ipc.send(&msg) {
                eprintln!("lapis-ui: {e}");
            }
        }
        if demo {
            app.demo_tick(now);
        }
        app.tick(dt, now);
        app.render(&mut canvas);
        sim.present(&canvas);
        let iv = app.frame_interval();
        sim.throttle(iv);
    }
    ExitCode::SUCCESS
}

#[cfg(not(feature = "sim"))]
fn run_sim(_view: View, _demo: bool, _socket: &str) -> ExitCode {
    usage_err("built without the `sim` feature; rebuild with default features")
}

/// CM0 headless frontend: `/dev/fb0` display + TCA8418 evdev input. Linux only;
/// every other target reports a clear error instead of compiling device code in.
#[cfg(target_os = "linux")]
fn run_fb(view: View, demo: bool, socket: &str) -> ExitCode {
    use lapis_ui::backend::fbdev::FbBackend;
    use lapis_ui::backend::hw_cm0::linux_io::{Backlight, EvdevInput};
    use lapis_ui::backend::{DisplayBackend, InputBackend, LapisKey};
    use std::time::Instant;

    let mut fb = match FbBackend::open("/dev/fb0") {
        Ok(f) => f,
        Err(e) => {
            eprintln!("lapis-ui: /dev/fb0 open failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut input = match EvdevInput::open_keypad() {
        Ok(ev) => ev,
        Err(e) => {
            eprintln!("lapis-ui: no tca8418 evdev device: {e}");
            return ExitCode::FAILURE;
        }
    };
    let backlight = Backlight::open().ok();

    let mut app = App::new();
    app.set_view(view);
    // Always created; inert until a daemon is listening.
    let mut ipc = IpcClient::new(socket);
    let start = Instant::now();
    let mut prev_now = 0.0f32;
    let mut canvas = Canvas::new();

    loop {
        let now = start.elapsed().as_secs_f32();
        let dt = (now - prev_now).clamp(0.0, 0.1);
        prev_now = now;

        if let Err(e) = ipc.connect_ready(now) {
            eprintln!("lapis-ui: {e}");
        }
        ipc.poll(now, |v| app.on_daemon_message(v));

        // Drain at most one key per frame; the rest queue in the kernel.
        match input.poll_event() {
            Some(LapisKey::BacklightDown) => {
                if let Err(e) = backlight
                    .as_ref()
                    .map(|b| b.step(-1, 16))
                    .unwrap_or_else(|| Err(std::io::Error::other("no backlight")))
                {
                    eprintln!("lapis-ui: backlight down failed: {e}");
                }
            }
            Some(LapisKey::BacklightUp) => {
                if let Err(e) = backlight
                    .as_ref()
                    .map(|b| b.step(1, 16))
                    .unwrap_or_else(|| Err(std::io::Error::other("no backlight")))
                {
                    eprintln!("lapis-ui: backlight up failed: {e}");
                }
            }
            Some(k) => {
                if let Some(msg) = app.key(k) {
                    if let Err(e) = ipc.send(&msg) {
                        eprintln!("lapis-ui: {e}");
                    }
                }
            }
            None => {}
        }

        if demo {
            app.demo_tick(now);
        }
        app.tick(dt, now);
        app.render(&mut canvas);
        fb.push_frame(&canvas);
        let iv = app.frame_interval();
        std::thread::sleep(iv);
    }
}

#[cfg(not(target_os = "linux"))]
fn run_fb(_view: View, _demo: bool, _socket: &str) -> ExitCode {
    usage_err("--fb requires Linux (CardputerZero CM0)")
}
