//! `lapis-ui`: Lapis-Zero OS 320x170 UI. Phase 0 CLI: `--sim` and `--dump-frame`.

use std::fs::File;
use std::io::BufWriter;
use std::process::ExitCode;

use lapis_ui::gfx::canvas::{Canvas, HEIGHT, WIDTH};
use lapis_ui::views::{self, View};

const HELP: &str = "\
lapis-ui - Lapis-Zero OS display UI

USAGE:
    lapis-ui [--sim] [--demo] [--view <name>]
    lapis-ui --dump-frame <path.png> [--view <name>]

OPTIONS:
    --sim                 Open the 960x510 SDL2 desktop simulator
    --demo                Demo data mode (label only in Phase 0)
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
        let mut canvas = Canvas::new();
        views::render(&mut canvas, view);
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
        if args.demo {
            println!("lapis-ui: --demo (Phase 0: label only)");
        }
        return run_sim(view);
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
fn run_sim(mut view: View) -> ExitCode {
    use lapis_ui::backend::{sdl_sim::SimBackend, LapisKey};
    let mut sim = match SimBackend::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lapis-ui: simulator init failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    sim.run_forever(&mut |canvas, key| {
        match key {
            Some(LapisKey::CycleL) => view = view.cycle(-1),
            Some(LapisKey::CycleR) => view = view.cycle(1),
            _ => {}
        }
        views::render(canvas, view);
    });
    ExitCode::SUCCESS
}

#[cfg(not(feature = "sim"))]
fn run_sim(_view: View) -> ExitCode {
    usage_err("built without the `sim` feature; rebuild with default features")
}
