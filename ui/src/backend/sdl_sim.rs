//! Desktop SDL2 simulator: 320x170 canvas shown 3x (960x510) with nearest scaling.

use std::thread;
use std::time::{Duration, Instant};

use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use sdl2::render::{Texture, WindowCanvas};
use sdl2::EventPump;

use super::{DisplayBackend, InputBackend, LapisKey};
use crate::gfx::canvas::{Canvas, HEIGHT, WIDTH};

/// Integer upscale factor for the simulator window.
const SCALE: u32 = 3;
/// 60 FPS frame budget.
const FRAME: Duration = Duration::from_micros(16_667);

/// SDL2 window + streaming texture implementing both backend traits.
pub struct SimBackend {
    window: WindowCanvas,
    texture: Texture,
    events: EventPump,
    argb: Vec<u8>,
    quit: bool,
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
        // ponytail: `unsafe_textures` drops the creator lifetime; the single texture
        // lives for the whole process, so it is never explicitly destroyed.
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
        })
    }

    /// Runs at 60 FPS until the window closes: each tick drains one key, calls
    /// `draw(canvas, key)`, then presents. Sleeps to the next tick (no busy wait).
    pub fn run_forever(&mut self, draw: &mut dyn FnMut(&mut Canvas, Option<LapisKey>)) {
        let mut canvas = Canvas::new();
        let mut next = Instant::now();
        while !self.quit {
            let key = self.poll_event();
            draw(&mut canvas, key);
            self.push_frame(&canvas);
            next += FRAME;
            let now = Instant::now();
            if next > now {
                thread::sleep(next - now);
            } else {
                next = now;
            }
        }
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
            match event {
                Event::Quit { .. } => self.quit = true,
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

/// Maps a desktop keycode to a CardputerZero semantic key (SPEC 5.4).
fn map_key(k: Keycode) -> Option<LapisKey> {
    Some(match k {
        Keycode::Z | Keycode::LEFT => LapisKey::CycleL,
        Keycode::C | Keycode::RIGHT => LapisKey::CycleR,
        Keycode::F | Keycode::UP => LapisKey::Up,
        Keycode::X | Keycode::DOWN => LapisKey::Down,
        Keycode::RETURN => LapisKey::Enter,
        Keycode::ESCAPE => LapisKey::Esc,
        Keycode::TAB => LapisKey::Tab,
        // ponytail: unshifted keycodes only; Prompt view (Phase 1) switches to TextInput.
        _ => match k.into_i32() {
            c @ 0x20..=0x7E => LapisKey::Char(c as u8 as char),
            _ => return None,
        },
    })
}
