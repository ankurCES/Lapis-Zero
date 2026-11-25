//! The head is drawn every frame on a 512 MB device: its draw path must not
//! touch the heap, and nor may the frame around it. A counting allocator
//! checks that for every state, at rest and while easing into the next one:
//! first the head alone, then whole companion frames through `App`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use lapis_ui::gfx::canvas::Canvas;
use lapis_ui::gfx::mascot::{Mascot, MascotState};
use lapis_ui::views::companion::HEAD;
use lapis_ui::views::App;

thread_local! {
    /// Allocations made by this thread since counting was switched on (`None`: off).
    static COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}

/// The system allocator, counting this thread's allocations while armed.
struct Counting;

// SAFETY: every request is passed straight to `System`; the only addition is
// a thread-local counter, which itself never allocates (it is const-initialised).
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // `try_with`: a thread can still allocate while its locals are torn down.
        let _ = COUNT.try_with(|c| c.set(c.get().map(|n| n + 1)));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Counts this thread's allocations while `f` runs.
fn allocations(f: impl FnOnce()) -> usize {
    COUNT.with(|c| c.set(Some(0)));
    f();
    COUNT.with(|c| c.replace(None)).unwrap_or(0)
}

#[test]
fn drawing_the_head_never_allocates() {
    // The counter does count: one boxed value is one allocation.
    let mut boxed = None;
    assert_eq!(
        allocations(|| boxed = Some(std::hint::black_box(Box::new(7u8)))),
        1
    );
    drop(boxed);

    let (cx, cy, r) = HEAD;
    let mut canvas = Canvas::new();
    let mut mascot = Mascot::new();
    let states = MascotState::ALL;
    for (i, state) in states.into_iter().enumerate() {
        mascot.snap(state);
        mascot.rms = 0.8;
        let count = allocations(|| {
            for frame in 0..60 {
                let now = f64::from(frame) / 12.0;
                // Halfway through, start easing into the next state.
                if frame == 30 {
                    mascot.set_state(states[(i + 1) % states.len()], now);
                }
                mascot.tick(1.0 / 60.0);
                mascot.draw(&mut canvas, cx, cy, r, now, frame % 2 == 1);
            }
        });
        assert_eq!(
            count, 0,
            "{state:?}: {count} heap allocations in the draw path"
        );
    }
}

#[test]
fn a_companion_frame_never_allocates() {
    // What the live loops do per frame, on the view with the head: tick,
    // render, ask how long to wait. Daemon frames are parsed outside this
    // (they arrive a few times a second at most, not sixty).
    let mut app = App::new();
    app.on_daemon_message(serde_json::json!({
        "type": "state_sync", "agent_state": "THINKING", "tailscale_state": "connected",
        "active_host": "macbook-pro", "next_cron": "08:00 Daily Digest",
        "active_task": {"id": "t_1", "title": "Check CI logs and restart the worker",
            "step": 4, "max_steps": 25},
    }));
    app.on_daemon_message(serde_json::json!({
        "type": "activity_event", "ts": 0, "task_id": "t_1", "kind": "vnc_click",
        "summary": "click(840, 412) left", "status": "ok",
    }));
    let mut canvas = Canvas::new();
    // The first frame is allowed its one-off setup.
    app.tick(1.0 / 60.0, 0.0);
    app.render(&mut canvas);

    let states = MascotState::ALL;
    for theme in ["paper", "dark"] {
        app.on_daemon_message(serde_json::json!({"type": "ui_settings", "theme": theme}));
        for (i, state) in states.into_iter().enumerate() {
            app.pin_state(state, true);
            let count = allocations(|| {
                for frame in 0..24 {
                    // Halfway through, ease into the next state at 60 FPS.
                    if frame == 12 {
                        app.pin_state(states[(i + 1) % states.len()], false);
                    }
                    app.tick(1.0 / 60.0, 1.0 + f64::from(frame) / 60.0);
                    app.render(&mut canvas);
                    std::hint::black_box(app.frame_interval());
                }
            });
            assert_eq!(
                count, 0,
                "{state:?} ({theme}): {count} heap allocations in a companion frame"
            );
        }
    }
}

#[test]
fn a_screensaver_frame_never_allocates() {
    // Ten seconds of the screensaver at its own rate, blinks included, in
    // both themes: it runs for a minute at a time on an idle device.
    let mut app = App::new();
    let mut canvas = Canvas::new();
    for theme in ["paper", "dark"] {
        app.on_daemon_message(serde_json::json!({"type": "ui_settings", "theme": theme}));
        app.start_screensaver();
        app.tick(1.0 / 12.0, 0.0);
        app.render(&mut canvas);
        assert!(app.screensaver_on());
        let count = allocations(|| {
            for frame in 1..120 {
                app.tick(1.0 / 12.0, f64::from(frame) / 12.0);
                app.render(&mut canvas);
                std::hint::black_box(app.frame_interval());
            }
        });
        assert!(app.screensaver_on());
        assert_eq!(
            count, 0,
            "{theme}: {count} heap allocations in a screensaver frame"
        );
    }
}
