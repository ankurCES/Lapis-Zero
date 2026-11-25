//! `lapis-ui`: Lapis-Zero OS 320x170 UI. CLI: `--sim`, `--demo`, `--dump-frame`, `--socket`.

use std::fs::File;
use std::io::BufWriter;
use std::process::ExitCode;

use lapis_ui::gfx::canvas::{Canvas, HEIGHT, WIDTH};
use lapis_ui::gfx::mascot::MascotState;
use lapis_ui::ipc::{selected_socket_path, IpcClient};
use lapis_ui::views::{App, DUMP_CLOCK, VIEW_NAMES};

const HELP: &str = "\
lapis-ui - Lapis-Zero OS display UI

USAGE:
    lapis-ui [--sim] [--demo] [--view <name>] [--state <name>] [--socket <path>]
    lapis-ui --dump-frame <path.png> [--view <name>] [--socket <path>]
             [--state <name>] [--from <name>] [--time <seconds>]
             [--theme <name>] [--rms <level>]
    lapis-ui --bench-head

OPTIONS:
    --sim                 Open the 960x510 SDL2 desktop simulator
    --demo                Demo data mode (state cycling + synthetic approval modal)
    --fb                  Run on the CM0 LCD (/dev/fb_lcd) + keypad evdev (Linux only)
    --socket <path>       Daemon IPC socket (default /run/lapis/lapisd.sock on Linux,
                          /tmp/lapis/lapisd.sock elsewhere)
    --dump-frame <path>   Render one 320x170 frame to an RGB PNG and exit
    --view <name>         companion|ticker|menu|approval|prompt|setup|pairing_pin|pairing_qr
                          (default companion; the pairing views show demo data)
    --state <name>        Pin the agent's head to one state:
                          idle|listening|thinking|working|fetching|command|
                          approval|paused|done|error
    --time <seconds>      Dump only: the animation clock of the frame (with
                          --state, seconds into that state)
    --from <name>         Dump only: ease from this state into --state, and
                          take the frame --time seconds into the change
    --theme <name>        Dump only: paper (default) or dark
    --rms <level>         Dump only: microphone level 0..1 for the listening head
    --bench-head          Time the head's draw call, state by state, and exit
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
    state: Option<MascotState>,
    from: Option<MascotState>,
    time: Option<f64>,
    dark: bool,
    rms: Option<f32>,
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
            "--bench-head" => return bench_head(),
            "--dump-frame" | "--view" | "--socket" | "--state" | "--from" | "--time"
            | "--theme" | "--rms" => {
                let Some(v) = it.next() else {
                    return usage_err(&format!("{a} needs a value"));
                };
                let seconds = || v.parse::<f64>().ok().filter(|t| t.is_finite() && *t >= 0.0);
                match a.as_str() {
                    "--view" => args.view = Some(v),
                    "--socket" => args.socket = Some(v),
                    "--state" | "--from" => match MascotState::from_name(&v) {
                        Some(state) if a == "--state" => args.state = Some(state),
                        Some(state) => args.from = Some(state),
                        None => return usage_err(&format!("unknown state {v:?} (see --help)")),
                    },
                    "--time" | "--rms" => match seconds() {
                        Some(t) if a == "--time" => args.time = Some(t),
                        Some(t) => args.rms = Some(t.min(1.0) as f32),
                        None => return usage_err(&format!("{a} needs a number >= 0, got {v:?}")),
                    },
                    "--theme" => match v.as_str() {
                        "paper" | "dark" => args.dark = v == "dark",
                        _ => return usage_err(&format!("unknown theme {v:?}: paper or dark")),
                    },
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

    let view = args.view.as_deref().unwrap_or("companion");
    if !VIEW_NAMES.contains(&view) {
        return usage_err(&format!(
            "unknown view {view:?}; expected one of: {}",
            VIEW_NAMES.join(", ")
        ));
    }

    if let Some(path) = args.dump {
        let mut app = App::new();
        app.open_named(view);
        if view == "approval" {
            app.show_synthetic_approval();
        }
        // Only an explicit --socket makes the dump depend on a live daemon.
        if let Some(sock) = args.socket.as_deref() {
            sync_once(&mut app, sock);
            // An explicit --view wins; without one the snapshot shows whatever
            // the daemon put on screen (an approval modal, the pairing view).
            if args.view.is_some() {
                app.open_named(view);
            }
        }
        if let Some(state) = args.from.or(args.state) {
            app.pin_state(state, true);
        }
        if args.dark {
            app.on_daemon_message(serde_json::json!({"type": "ui_settings", "theme": "dark"}));
        }
        if let Some(rms) = args.rms {
            app.on_daemon_message(serde_json::json!({"type": "mic_level", "rms": rms}));
        }
        // Settled, deterministic snapshot: pinned desk clock, no slide/motion,
        // and any modal snap-rested so borders/preview land predictably.
        app.settle_all();
        let time = args.time.unwrap_or(f64::from(DUMP_CLOCK) * 60.0);
        match (args.from, args.state) {
            // The frame `time` seconds into the eased change between two
            // states, stepped at 60 FPS as the live loop would: the change
            // is noticed on the frame at time zero and eases from the next.
            (Some(_), Some(to)) => {
                app.tick(1.0 / 60.0, 0.0);
                app.pin_state(to, false);
                for i in 0..=(time * 60.0).round() as u32 {
                    app.tick(1.0 / 60.0, f64::from(i) / 60.0);
                }
            }
            _ => app.tick(1.0 / 60.0, time),
        }
        let mut canvas = Canvas::new();
        app.render(&mut canvas);
        return match write_png(&path, &canvas) {
            Ok(()) => {
                println!("wrote {path} ({WIDTH}x{HEIGHT}, view {view})");
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
            args.state,
            &selected_socket_path(args.socket.as_deref()),
        );
    }

    if args.sim {
        return run_sim(
            view,
            args.demo,
            args.state,
            &selected_socket_path(args.socket.as_deref()),
        );
    }

    print!("{HELP}");
    ExitCode::from(2)
}

/// `--bench-head`: what one frame of the head costs, state by state, next to
/// a whole companion frame. Run it on the CM0 to see the real budget.
fn bench_head() -> ExitCode {
    use lapis_ui::gfx::mascot::Mascot;
    use lapis_ui::views::companion::HEAD;
    use std::time::Instant;

    const FRAMES: usize = 600;
    // Median and worst of the samples, in microseconds; the median shrugs off
    // the frames a busy machine preempts.
    let report = |name: &str, nanos: &mut [u64; FRAMES]| {
        nanos.sort_unstable();
        let us = |n: u64| n as f64 / 1000.0;
        println!(
            "  {name:<16} median {:>8.1}   worst {:>8.1}",
            us(nanos[FRAMES / 2]),
            us(nanos[FRAMES - 1])
        );
    };
    let (cx, cy, r) = HEAD;
    let mut canvas = Canvas::new();
    let mut nanos = [0u64; FRAMES];
    println!("microseconds per frame over {FRAMES} frames, head radius {r} px");
    for state in MascotState::ALL {
        let mut mascot = Mascot::new();
        mascot.snap(state);
        mascot.rms = 1.0;
        for (i, slot) in nanos.iter_mut().enumerate() {
            // 12 FPS steps, alternating themes.
            let start = Instant::now();
            mascot.draw(&mut canvas, cx, cy, r, i as f64 / 12.0, i % 2 == 1);
            *slot = start.elapsed().as_nanos() as u64;
            std::hint::black_box(canvas.pixels());
        }
        report(state.name(), &mut nanos);
    }
    let mut app = App::new();
    app.pin_state(MascotState::Thinking, true);
    for (i, slot) in nanos.iter_mut().enumerate() {
        let start = Instant::now();
        app.tick(1.0 / 12.0, i as f64 / 12.0);
        app.render(&mut canvas);
        *slot = start.elapsed().as_nanos() as u64;
    }
    report("companion frame", &mut nanos);
    ExitCode::SUCCESS
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

/// Sends the replies `App::on_daemon_message` queued (`screen_snapshot`).
#[cfg(any(feature = "sim", target_os = "linux"))]
fn send_replies(app: &mut App, ipc: &mut IpcClient) {
    for msg in app.take_outbox() {
        if let Err(e) = ipc.send(&msg) {
            eprintln!("lapis-ui: {e}");
        }
    }
}

#[cfg(feature = "sim")]
fn run_sim(view: &str, demo: bool, state: Option<MascotState>, socket: &str) -> ExitCode {
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
    app.open_named(view);
    if let Some(state) = state {
        app.pin_state(state, true);
    }
    // Always created; inert (demo state stays) until a daemon is listening.
    let mut ipc = IpcClient::new(socket);
    let start = Instant::now();
    let mut prev_now = 0.0f64;
    let mut canvas = Canvas::new();

    loop {
        if sim.should_quit() {
            break;
        }
        // Seconds as f64: an f32 would step in whole frames after a few days.
        let now = start.elapsed().as_secs_f64();
        let dt = (now - prev_now).clamp(0.0, 0.1) as f32;
        prev_now = now;
        // ponytail: the socket is polled once per frame, so an idle UI (1 FPS)
        // reacts to the daemon within 1s; poll(2) on the fd if that is too slow.
        // (The reconnect backoff only compares seconds: f32 is enough for it.)
        if let Err(e) = ipc.connect_ready(now as f32) {
            eprintln!("lapis-ui: {e}");
        }
        ipc.poll(now as f32, |v| app.on_daemon_message(v));
        send_replies(&mut app, &mut ipc);
        // Drain at most one key per frame; the rest queue in SDL.
        if let Some(msg) = sim.poll().and_then(|k| app.key(k)) {
            if let Err(e) = ipc.send(&msg) {
                eprintln!("lapis-ui: {e}");
            }
        }
        if demo {
            app.demo_tick(now);
        }
        if let Ok(since) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            app.set_wall_clock(since.as_secs() as i64);
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
fn run_sim(_view: &str, _demo: bool, _state: Option<MascotState>, _socket: &str) -> ExitCode {
    usage_err("built without the `sim` feature; rebuild with default features")
}

/// CM0 headless frontend: LCD fbdev + TCA8418 evdev input. Linux only; every
/// other target reports a clear error instead of compiling device code in.
///
/// Nothing here may exit on a missing device: on the appliance there is no
/// other screen to explain why, so the loop waits for the LCD and runs without
/// a keypad (retrying) rather than leaving the boot splash up forever.
#[cfg(target_os = "linux")]
fn run_fb(view: &str, demo: bool, state: Option<MascotState>, socket: &str) -> ExitCode {
    use lapis_ui::backend::fbdev::FbBackend;
    use lapis_ui::backend::hw_cm0::linux_io::{wait_frame, Backlight, EvdevInput};
    use lapis_ui::backend::hw_cm0::{backlight_state_for_idle, brightness_level, BacklightState};
    use lapis_ui::backend::{DisplayBackend, InputBackend, LapisKey};
    use std::time::{Duration, Instant};

    let mut tries = 0u32;
    let mut fb = loop {
        match FbBackend::open_lcd() {
            Ok((fb, path)) => {
                eprintln!("lapis-ui: display on {path} ({})", fb.id());
                break fb;
            }
            Err(e) => {
                if tries.is_multiple_of(20) {
                    eprintln!("lapis-ui: waiting for the LCD framebuffer: {e}");
                }
                tries += 1;
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    };

    let mut input: Option<EvdevInput> = None;
    let mut next_input_try = 0.0f64;
    let mut input_warned = false;

    // `level` is the user's brightness; the idle policy scales it (30 s dim, 90 s off).
    // Found lazily, like the keypad: this process can start before the driver
    // has registered the backlight, and a screen left at brightness 0 is dark.
    let mut backlight: Option<Backlight> = None;
    let mut bl_max = 0u32;
    let mut bl_step = 1u32;
    let mut level = 0u32;
    let mut bl_applied: Option<(BacklightState, u32)> = None;

    let mut app = App::new();
    app.open_named(view);
    if let Some(state) = state {
        app.pin_state(state, true);
    }
    // Always created; inert until a daemon is listening.
    let mut ipc = IpcClient::new(socket);
    let start = Instant::now();
    let mut prev_now = 0.0f64;
    let mut canvas = Canvas::new();
    // What was on screen and whether the daemon was reachable, for the journal:
    // the boot report on the SD card is the only way to see this device's past.
    let mut logged_view = "";
    let mut logged_link = false;

    loop {
        // Seconds as f64: this loop runs for weeks, and an f32 would step in
        // whole frames after a few days.
        let now = start.elapsed().as_secs_f64();
        let dt = (now - prev_now).clamp(0.0, 0.1) as f32;
        prev_now = now;

        // (The reconnect backoff only compares seconds: f32 is enough for it.)
        if let Err(e) = ipc.connect_ready(now as f32) {
            eprintln!("lapis-ui: {e}");
        }
        ipc.poll(now as f32, |v| app.on_daemon_message(v));
        if ipc.is_connected() != logged_link {
            logged_link = !logged_link;
            let state = if logged_link { "connected" } else { "lost" };
            eprintln!("lapis-ui: daemon {state}");
        }
        // No demo data on the device: an unreachable daemon shows as OFFLINE.
        app.set_offline(!demo && !ipc.is_connected());
        send_replies(&mut app, &mut ipc);
        // `ui_settings.brightness` (percent of the panel maximum) replaces the
        // user's level; the idle policy below then scales it as usual.
        if let Some(pct) = app.take_brightness() {
            level = brightness_level(pct, bl_max);
        }

        // Missing devices are looked for again every two seconds.
        let retry_devices = now >= next_input_try;
        if retry_devices {
            next_input_try = now + 2.0;
        }
        if backlight.is_none() && retry_devices {
            if let Ok(b) = Backlight::open() {
                bl_max = b.max();
                bl_step = (bl_max / 8).max(1);
                level = match b.brightness() {
                    0 => bl_max,
                    v => v,
                };
                bl_applied = None;
                backlight = Some(b);
            }
        }
        if input.as_ref().is_none_or(|i| i.is_dead()) && retry_devices {
            input = match EvdevInput::open_keypad() {
                Ok(ev) => {
                    eprintln!("lapis-ui: keypad ready");
                    input_warned = false;
                    Some(ev)
                }
                Err(e) => {
                    if !input_warned {
                        eprintln!("lapis-ui: keypad unavailable ({e}); retrying");
                        input_warned = true;
                    }
                    None
                }
            };
        }

        // Drain at most one key per frame; the rest stay queued.
        if let Some(k) = input.as_mut().and_then(|i| i.poll_event()) {
            let asleep = matches!(bl_applied, Some((BacklightState::Off, _)));
            match k {
                // A key on a dark screen only wakes it: never act on (or
                // approve) something the user could not see.
                _ if asleep => app.wake(),
                LapisKey::BacklightDown => {
                    level = level.saturating_sub(bl_step).max(bl_step);
                    app.wake();
                }
                LapisKey::BacklightUp => {
                    level = (level + bl_step).min(bl_max);
                    app.wake();
                }
                k => {
                    if let Some(msg) = app.key(k) {
                        if let Err(e) = ipc.send(&msg) {
                            eprintln!("lapis-ui: {e}");
                        }
                    }
                }
            }
        }

        if let Some(b) = backlight.as_ref() {
            let state = if app.keeps_screen_on() {
                BacklightState::Full
            } else {
                backlight_state_for_idle(app.idle_ms())
            };
            app.set_backlight_off(state == BacklightState::Off);
            if bl_applied != Some((state, level)) {
                let value = match state {
                    BacklightState::Full => level,
                    BacklightState::Dim => (level / 4).max(1),
                    BacklightState::Off => 0,
                };
                if let Err(e) = b.set_brightness(value) {
                    eprintln!("lapis-ui: backlight write failed: {e}");
                }
                bl_applied = Some((state, level));
            }
        }

        if demo {
            app.demo_tick(now);
        }
        if let Ok(since) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            app.set_wall_clock(since.as_secs() as i64);
        }
        app.tick(dt, now);
        if app.view_name() != logged_view {
            logged_view = app.view_name();
            eprintln!("lapis-ui: showing {logged_view}");
        }
        app.render(&mut canvas);
        fb.push_frame(&canvas);
        // Wait out the frame interval, waking early for a key or a daemon frame.
        let mut fds = [0; 2];
        let mut n = 0;
        if let Some(fd) = input.as_ref().filter(|i| !i.is_dead()).map(|i| i.raw_fd()) {
            fds[n] = fd;
            n += 1;
        }
        if let Some(fd) = ipc.raw_fd() {
            fds[n] = fd;
            n += 1;
        }
        wait_frame(&fds[..n], app.frame_interval());
    }
}

#[cfg(not(target_os = "linux"))]
fn run_fb(_view: &str, _demo: bool, _state: Option<MascotState>, _socket: &str) -> ExitCode {
    usage_err("--fb requires Linux (CardputerZero CM0)")
}
