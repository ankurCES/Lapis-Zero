//! `lapis-ui`: Lapis-Zero OS 320x170 UI. Phase 1 CLI: `--sim`, `--demo`, `--dump-frame`.

use std::fs::File;
use std::io::BufWriter;
use std::process::ExitCode;

use lapis_ui::gfx::canvas::{Canvas, HEIGHT, WIDTH};
use lapis_ui::views::{App, View, DUMP_CLOCK};

const HELP: &str = "\
lapis-ui - Lapis-Zero OS display UI

USAGE:
    lapis-ui [--sim] [--demo] [--view <name>]
    lapis-ui --dump-frame <path.png> [--view <name>]

OPTIONS:
    --sim                 Open the 960x510 SDL2 desktop simulator
    --demo                Demo data mode (state cycling + synthetic approval modal)
    --fb                  /dev/fb0 backend (Phase 4)
    --socket <path>       Daemon IPC socket (Phase 2)
    --dump-frame <path>   Render one 320x170 frame to an RGB PNG and exit
    --view <name>         companion|ticker|menu|approval|prompt|setup (default companion)
    --help                Show this help
";

/// Parsed command line.
#[derive(Default)]
struct Args {
    sim: bool,
    demo: bool,
    dump: Option<String>,
    view: Option<String>,
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
            "--fb" => return usage_err("fbdev lands in Phase 4"),
            "--socket" => return usage_err("ipc socket lands in Phase 2"),
            "--dump-frame" | "--view" => {
                let Some(v) = it.next() else {
                    return usage_err(&format!("{a} needs a value"));
                };
                if a == "--view" {
                    args.view = Some(v);
                } else {
                    args.dump = Some(v);
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

    if args.sim {
        return run_sim(view, args.demo);
    }

    print!("{HELP}");
    ExitCode::from(2)
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
fn run_sim(view: View, demo: bool) -> ExitCode {
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
        // Drain at most one key per frame; the rest queue in SDL.
        if let Some(k) = sim.poll() {
            app.key(k);
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
fn run_sim(_view: View, _demo: bool) -> ExitCode {
    usage_err("built without the `sim` feature; rebuild with default features")
}
