//! Desktop SDL2 simulator: 320x170 canvas shown 3x (960x510) with nearest scaling.

use std::thread;
use std::time::{Duration, Instant};

use sdl2::event::Event;
use sdl2::keyboard::{Keycode, Mod};
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use sdl2::render::{Texture, WindowCanvas};
use sdl2::EventPump;

use super::{DisplayBackend, InputBackend, LapisKey};
use crate::gfx::canvas::{Canvas, HEIGHT, WIDTH};

/// Integer upscale factor for the simulator window.
const SCALE: u32 = 3;
/// Hold threshold for ESC global kill-switch (SPEC 5.4).
const ESC_HOLD_DURATION: Duration = Duration::from_millis(1500);

/// SDL2 window + streaming texture implementing both backend traits.
pub struct SimBackend {
    window: WindowCanvas,
    texture: Texture,
    events: EventPump,
    argb: Vec<u8>,
    quit: bool,
    esc_down: Option<Instant>,
}

impl SimBackend {
    /// Opens the 960x510 simulator window.
    pub fn new() -> Result<Self, String> {
        sdl2::hint::set("SDL_RENDER_SCALE_QUALITY", "0");
        let sdl = sdl2::init()?;
        let video = sdl.video()?;
        let win = video
            .window(
                "Lapis-Zero Simulator",
                WIDTH as u32 * SCALE,
                HEIGHT as u32 * SCALE,
            )
            .position_centered()
            .build()
            .map_err(|e| e.to_string())?;
        let window = win.into_canvas().build().map_err(|e| e.to_string())?;
        let texture = window
            .texture_creator()
            .create_texture_static(PixelFormatEnum::ARGB8888, WIDTH as u32, HEIGHT as u32)
            .map_err(|e| e.to_string())?;
        // Newer SDL runtimes (e.g. sdl2-compat 2.32) emit window-range event types
        // (0x202..=0x2FF) that sdl2 0.37 cannot decode and panics on; SDL2 itself
        // only defines 0x200/0x201 there, so ignoring the rest is lossless.
        for ty in 0x202..=0x2FF {
            // SAFETY: plain FFI call on an initialised SDL; no pointers involved.
            unsafe { sdl2::sys::SDL_EventState(ty, sdl2::sys::SDL_IGNORE as i32) };
        }
        Ok(Self {
            window,
            texture,
            events: sdl.event_pump()?,
            argb: vec![0; (WIDTH * HEIGHT * 4) as usize],
            quit: false,
            esc_down: None,
        })
    }

    /// True once the user closed the window.
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Drains and returns the next pending key, flipping the quit flag on close.
    pub fn poll(&mut self) -> Option<LapisKey> {
        <Self as InputBackend>::poll_event(self)
    }

    /// Presents a finished frame (scaled 3x with nearest neighbour).
    pub fn present(&mut self, canvas: &Canvas) {
        <Self as DisplayBackend>::push_frame(self, canvas);
    }

    /// Sleeps `iv` between frames (the adaptive governor supplies it).
    pub fn throttle(&mut self, iv: Duration) {
        thread::sleep(iv);
    }
}

impl DisplayBackend for SimBackend {
    fn push_frame(&mut self, canvas: &Canvas) {
        // ARGB8888 is stored little-endian in memory as B, G, R, A.
        for (dst, src) in self
            .argb
            .chunks_exact_mut(4)
            .zip(canvas.pixels().chunks_exact(3))
        {
            dst.copy_from_slice(&[src[2], src[1], src[0], 0xFF]);
        }
        if let Err(e) = self.texture.update(None, &self.argb, WIDTH as usize * 4) {
            eprintln!("lapis-ui: texture update failed: {e}");
            return;
        }
        let dst = Rect::new(0, 0, WIDTH as u32 * SCALE, HEIGHT as u32 * SCALE);
        if let Err(e) = self.window.copy(&self.texture, None, Some(dst)) {
            eprintln!("lapis-ui: render copy failed: {e}");
        }
        self.window.present();
    }
}

impl InputBackend for SimBackend {
    fn poll_event(&mut self) -> Option<LapisKey> {
        for event in self.events.poll_iter() {
            let mods = match &event {
                Event::KeyDown { keymod, .. } | Event::KeyUp { keymod, .. } => *keymod,
                _ => Mod::NOMOD,
            };
            match event {
                Event::Quit { .. } => self.quit = true,
                Event::KeyDown {
                    keycode: Some(Keycode::ESCAPE),
                    repeat,
                    ..
                } => {
                    if !repeat && self.esc_down.is_none() {
                        self.esc_down = Some(Instant::now());
                    }
                }
                Event::KeyUp {
                    keycode: Some(Keycode::ESCAPE),
                    ..
                } => {
                    if let Some(down_at) = self.esc_down.take() {
                        if down_at.elapsed() >= ESC_HOLD_DURATION {
                            return Some(LapisKey::EscHold);
                        }
                        return Some(LapisKey::Esc);
                    }
                }
                Event::KeyDown {
                    keycode: Some(Keycode::SPACE),
                    repeat,
                    ..
                } if is_option(mods) && !repeat => {
                    return Some(LapisKey::PushToTalk(true));
                }
                Event::KeyUp {
                    keycode: Some(Keycode::SPACE),
                    ..
                } if is_option(mods) => {
                    return Some(LapisKey::PushToTalk(false));
                }
                Event::KeyDown {
                    keycode: Some(k), ..
                } => {
                    if let Some(key) = map_key(k) {
                        return Some(key);
                    }
                }
                _ => {}
            }
        }
        None
    }
}

/// True when the `OPT` (left/right GUI) chord modifier is held — the macOS
/// stand-in for the CardputerZero `fn` key (SPEC 5.3 PTT is `fn + SPACE`).
fn is_option(mods: Mod) -> bool {
    mods.intersects(Mod::LGUIMOD | Mod::RGUIMOD)
}

/// Maps a desktop keycode to a CardputerZero semantic key (SPEC 5.4).
fn map_key(k: Keycode) -> Option<LapisKey> {
    Some(match k {
        // Z/C/F/X arrive as chars; `App::key` maps them to navigation outside Prompt.
        Keycode::LEFT => LapisKey::CycleL,
        Keycode::RIGHT => LapisKey::CycleR,
        Keycode::UP => LapisKey::Up,
        Keycode::DOWN => LapisKey::Down,
        Keycode::BACKSPACE => LapisKey::Char('\x08'),
        Keycode::RETURN => LapisKey::Enter,
        Keycode::ESCAPE => LapisKey::Esc,
        Keycode::TAB => LapisKey::Tab,
        _ => match k.into_i32() {
            c @ 0x20..=0x7E => LapisKey::Char(c as u8 as char),
            _ => return None,
        },
    })
}
