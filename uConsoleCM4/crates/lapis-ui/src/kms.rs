//! The device backend (feature `kms`, Linux): DRM/KMS modesetting, a GBM surface, EGL + GLES
//! on VideoCore VI, egui painted into a 1280x720 FBO and blitted (rotated: the uConsole's DSI
//! panel is natively 720x1280 portrait) to the scanout buffer with vsynced page flips.
//! Input comes straight from evdev (keyboard, trackball, gamepad buttons), grabbed so the
//! virtual console never sees a keystroke. No X11, no Wayland, no window manager.

use crate::app::DeckApp;
use crate::input::{self, PadEvent, PadMap};
use drm::control::{
    connector, crtc, framebuffer, Device as ControlDevice, ModeTypeFlags, PageFlipFlags,
};
use drm::Device as BasicDevice;
use gbm::AsRaw;
use glow::HasContext;
use glutin::config::{ConfigSurfaceTypes, ConfigTemplateBuilder};
use glutin::context::{ContextApi, ContextAttributesBuilder, NotCurrentGlContext, Version};
use glutin::display::GlDisplay;
use glutin::surface::{GlSurface, SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::{GbmDisplayHandle, GbmWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::collections::HashSet;
use std::num::NonZeroU32;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const W: u32 = 1280;
pub const H: u32 = 720;
const XRGB8888: u32 = 0x3432_5258; // DRM_FORMAT_XRGB8888 ('XR24')
const ARGB8888: u32 = 0x3432_5241; // DRM_FORMAT_ARGB8888 ('AR24')

pub struct Card(std::fs::File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl BasicDevice for Card {}
impl ControlDevice for Card {}

pub struct Options {
    pub card: Option<PathBuf>,
    /// 0, 90, 180 or 270; `None` = 90 when the panel is portrait.
    pub rotate: Option<u32>,
    pub pads: PadMap,
    pub trackball_gain: f32,
}

enum Wake {
    Input(Raw),
    Repaint,
}

#[derive(Debug, Clone, Copy)]
pub enum Raw {
    Key(u16, i32),
    Rel(u16, i32),
    Abs(u16, i32),
}

struct Output {
    conn: connector::Handle,
    crtc: crtc::Handle,
    mode: drm::control::Mode,
}

/// The display to drive: a connected connector with modes and a CRTC. DSI panels may report
/// `unknown` instead of `connected`; such a connector is used when nothing reports connected.
fn find_output(card: &Card) -> Option<Output> {
    let res = card.resource_handles().ok()?;
    let mut fallback = None;
    for &c in res.connectors() {
        // Probe (not the cached state): an unprobed connector can list no modes at boot.
        let Ok(info) = card.get_connector(c, true) else {
            continue;
        };
        let state = info.state();
        if state == connector::State::Disconnected {
            continue;
        }
        let modes = info.modes();
        let Some(mode) = modes
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or_else(|| modes.first())
            .copied()
        else {
            continue;
        };
        let from_encoder = info
            .current_encoder()
            .and_then(|e| card.get_encoder(e).ok())
            .and_then(|e| e.crtc());
        let crtc = from_encoder.or_else(|| {
            info.encoders()
                .iter()
                .filter_map(|e| card.get_encoder(*e).ok())
                .flat_map(|e| res.filter_crtcs(e.possible_crtcs()))
                .next()
        });
        let Some(crtc) = crtc else { continue };
        let out = Output {
            conn: c,
            crtc,
            mode,
        };
        if state == connector::State::Connected {
            return Some(out);
        }
        fallback.get_or_insert(out);
    }
    fallback
}

fn driver_name(card: &Card) -> String {
    card.get_driver()
        .map(|d| d.name().to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Waits up to `wait` for a KMS card with a connected display. At boot the firmware's
/// `simpledrm` framebuffer exists for the first seconds and then disappears when vc4 takes
/// over (Lapis-Zero saw the same with its fbdev): never bind to it unless asked to by path.
fn open_card(path: Option<PathBuf>, wait: Duration) -> Result<(Card, Output), String> {
    let deadline = Instant::now() + wait;
    let mut last = String::new();
    loop {
        let candidates: Vec<PathBuf> = match &path {
            Some(p) => vec![p.clone()],
            None => {
                let mut v: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
                    .map(|d| d.flatten().map(|e| e.path()).collect())
                    .unwrap_or_default();
                v.retain(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("card"))
                });
                v.sort();
                v
            }
        };
        let mut seen = Vec::new();
        for p in candidates {
            let f = match std::fs::OpenOptions::new().read(true).write(true).open(&p) {
                Ok(f) => f,
                Err(e) => {
                    seen.push(format!("{}: {e}", p.display()));
                    continue;
                }
            };
            let card = Card(f);
            let driver = driver_name(&card);
            if driver == "simpledrm" && path.is_none() {
                seen.push(format!(
                    "{} simpledrm (firmware framebuffer, skipped)",
                    p.display()
                ));
                continue;
            }
            match find_output(&card) {
                Some(out) => {
                    eprintln!(
                        "lapis-ui: display {} ({driver}) {}x{}",
                        p.display(),
                        out.mode.size().0,
                        out.mode.size().1
                    );
                    return Ok((card, out));
                }
                None => seen.push(format!("{} {driver}: no connected display", p.display())),
            }
        }
        let now = if seen.is_empty() {
            "no /dev/dri/card* yet".to_owned()
        } else {
            seen.join("; ")
        };
        if now != last {
            eprintln!("lapis-ui: waiting for the display: {now}");
            last = now;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no DRM card with a connected display after {} s: {last}",
                wait.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

const VS: &str = "#version 100\nattribute vec2 pos;\nuniform vec4 m;\nuniform vec2 o;\nvarying vec2 uv;\nvoid main(){ vec2 ab = pos * 0.5 + 0.5; uv = vec2(m.x*ab.x + m.y*ab.y, m.z*ab.x + m.w*ab.y) + o; gl_Position = vec4(pos, 0.0, 1.0); }\n";
const FS: &str = "#version 100\nprecision mediump float;\nuniform sampler2D tex;\nvarying vec2 uv;\nvoid main(){ gl_FragColor = vec4(texture2D(tex, uv).rgb, 1.0); }\n";

/// (m, o) such that the screen fraction (a, b) samples the landscape texture at m*(a,b)+o.
pub fn rotation(deg: u32) -> ([f32; 4], [f32; 2]) {
    match deg % 360 {
        90 => ([0.0, -1.0, 1.0, 0.0], [1.0, 0.0]),
        180 => ([-1.0, 0.0, 0.0, -1.0], [1.0, 1.0]),
        270 => ([0.0, 1.0, -1.0, 0.0], [0.0, 1.0]),
        _ => ([1.0, 0.0, 0.0, 1.0], [0.0, 0.0]),
    }
}

struct Blit {
    program: glow::Program,
    vbo: glow::Buffer,
    fbo: glow::Framebuffer,
    tex: glow::Texture,
}

unsafe fn compile(gl: &glow::Context) -> Result<Blit, String> {
    let program = gl.create_program()?;
    for (kind, src) in [(glow::VERTEX_SHADER, VS), (glow::FRAGMENT_SHADER, FS)] {
        let sh = gl.create_shader(kind)?;
        gl.shader_source(sh, src);
        gl.compile_shader(sh);
        if !gl.get_shader_compile_status(sh) {
            return Err(gl.get_shader_info_log(sh));
        }
        gl.attach_shader(program, sh);
    }
    gl.bind_attrib_location(program, 0, "pos");
    gl.link_program(program);
    if !gl.get_program_link_status(program) {
        return Err(gl.get_program_info_log(program));
    }
    let vbo = gl.create_buffer()?;
    gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
    let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
    let bytes: Vec<u8> = quad.iter().flat_map(|f| f.to_ne_bytes()).collect();
    gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &bytes, glow::STATIC_DRAW);
    let tex = gl.create_texture()?;
    gl.bind_texture(glow::TEXTURE_2D, Some(tex));
    gl.tex_image_2d(
        glow::TEXTURE_2D,
        0,
        glow::RGBA as i32,
        W as i32,
        H as i32,
        0,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        glow::PixelUnpackData::Slice(None),
    );
    for (p, v) in [
        (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
        (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
        (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
        (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
    ] {
        gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32);
    }
    let fbo = gl.create_framebuffer()?;
    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
    gl.framebuffer_texture_2d(
        glow::FRAMEBUFFER,
        glow::COLOR_ATTACHMENT0,
        glow::TEXTURE_2D,
        Some(tex),
        0,
    );
    if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
        return Err("offscreen framebuffer incomplete".into());
    }
    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
    Ok(Blit {
        program,
        vbo,
        fbo,
        tex,
    })
}

fn spawn_input(tx: mpsc::Sender<Wake>) {
    let open: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    std::thread::spawn(move || loop {
        for (path, mut dev) in evdev::enumerate() {
            let fresh = open
                .lock()
                .map(|mut s| s.insert(path.clone()))
                .unwrap_or(false);
            if !fresh {
                continue;
            }
            let keys = dev
                .supported_keys()
                .is_some_and(|k| k.iter().next().is_some());
            let rel = dev
                .supported_relative_axes()
                .is_some_and(|r| r.iter().next().is_some());
            if !(keys || rel) {
                continue;
            }
            // Exclusive: the virtual console behind us must never act on these keys.
            let _ = dev.grab();
            eprintln!(
                "lapis-ui: input {} ({})",
                path.display(),
                dev.name().unwrap_or("?")
            );
            let (tx, open) = (tx.clone(), open.clone());
            std::thread::spawn(move || {
                // Ends when the device is unplugged.
                while let Ok(events) = dev.fetch_events() {
                    for e in events {
                        let raw = match e.event_type() {
                            evdev::EventType::KEY => Raw::Key(e.code(), e.value()),
                            evdev::EventType::RELATIVE => Raw::Rel(e.code(), e.value()),
                            evdev::EventType::ABSOLUTE => Raw::Abs(e.code(), e.value()),
                            _ => continue,
                        };
                        if tx.send(Wake::Input(raw)).is_err() {
                            return;
                        }
                    }
                }
                if let Ok(mut s) = open.lock() {
                    s.remove(&path);
                }
            });
        }
        std::thread::sleep(Duration::from_secs(3));
    });
}

/// Translates evdev into egui events and pad presses.
pub struct InputState {
    pub pointer: egui::Pos2,
    shift: bool,
    ctrl: bool,
    alt: bool,
    caps: bool,
    axes: [i8; 2],
    pub map: PadMap,
    pub gain: f32,
}

impl InputState {
    pub fn new(map: PadMap, gain: f32) -> Self {
        InputState {
            pointer: egui::pos2(W as f32 / 2.0, H as f32 / 2.0),
            shift: false,
            ctrl: false,
            alt: false,
            caps: false,
            axes: [0, 0],
            map,
            gain,
        }
    }

    fn modifiers(&self) -> egui::Modifiers {
        egui::Modifiers {
            alt: self.alt,
            ctrl: self.ctrl,
            shift: self.shift,
            mac_cmd: false,
            command: self.ctrl,
        }
    }

    fn key_event(&self, key: egui::Key, pressed: bool, repeat: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat,
            modifiers: self.modifiers(),
        }
    }

    /// Returns (egui events, pad events, push-to-talk change).
    pub fn feed(
        &mut self,
        raw: Raw,
        events: &mut Vec<egui::Event>,
        pads: &mut Vec<PadEvent>,
    ) -> Option<bool> {
        match raw {
            Raw::Key(code, value) => {
                let (pressed, repeat) = (value != 0, value == 2);
                match code {
                    input::KEY_LEFTSHIFT | input::KEY_RIGHTSHIFT => self.shift = pressed,
                    input::KEY_LEFTCTRL | input::KEY_RIGHTCTRL => self.ctrl = pressed,
                    input::KEY_LEFTALT | input::KEY_RIGHTALT => self.alt = pressed,
                    input::KEY_CAPSLOCK if value == 1 => self.caps = !self.caps,
                    _ => {}
                }
                if let Some(button) = self.map.button(code) {
                    if !repeat {
                        pads.push(PadEvent { button, pressed });
                    }
                    return None;
                }
                if input::is_ptt(code) && !repeat {
                    return Some(pressed);
                }
                let mouse = match code {
                    input::BTN_LEFT => Some(egui::PointerButton::Primary),
                    input::BTN_RIGHT => Some(egui::PointerButton::Secondary),
                    input::BTN_MIDDLE => Some(egui::PointerButton::Middle),
                    _ => None,
                };
                if let Some(button) = mouse {
                    if !repeat {
                        events.push(egui::Event::PointerButton {
                            pos: self.pointer,
                            button,
                            pressed,
                            modifiers: self.modifiers(),
                        });
                    }
                    return None;
                }
                if let Some(k) = input::key(code) {
                    events.push(self.key_event(k, pressed, repeat));
                }
                if pressed && !self.ctrl && !self.alt {
                    let letter = (16..=50).contains(&code);
                    if let Some(c) = input::key_char(code, self.shift ^ (self.caps && letter)) {
                        events.push(egui::Event::Text(c.to_string()));
                    }
                }
            }
            Raw::Rel(code, value) => match code {
                0 | 1 => {
                    let d = value as f32 * self.gain;
                    if code == 0 {
                        self.pointer.x = (self.pointer.x + d).clamp(0.0, W as f32 - 1.0);
                    } else {
                        self.pointer.y = (self.pointer.y + d).clamp(0.0, H as f32 - 1.0);
                    }
                    events.push(egui::Event::PointerMoved(self.pointer));
                }
                8 | 6 => {
                    let delta = if code == 8 {
                        egui::vec2(0.0, value as f32)
                    } else {
                        egui::vec2(value as f32, 0.0)
                    };
                    events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta,
                        modifiers: self.modifiers(),
                    });
                }
                _ => {}
            },
            // QMK gamepad mode: the D-pad as joystick X/Y (-127..127); stock 0..1023 also works.
            Raw::Abs(code @ (0 | 1), value) => {
                let dir: i8 = if (-127..=127).contains(&value) {
                    if value < -64 {
                        -1
                    } else if value > 64 {
                        1
                    } else {
                        0
                    }
                } else if value < 256 {
                    -1
                } else if value > 768 {
                    1
                } else {
                    0
                };
                let axis = usize::from(code);
                let (neg, pos) = if axis == 0 {
                    (egui::Key::ArrowLeft, egui::Key::ArrowRight)
                } else {
                    (egui::Key::ArrowUp, egui::Key::ArrowDown)
                };
                let old = self.axes[axis];
                if old != dir {
                    if old != 0 {
                        events.push(self.key_event(if old < 0 { neg } else { pos }, false, false));
                    }
                    if dir != 0 {
                        events.push(self.key_event(if dir < 0 { neg } else { pos }, true, false));
                    }
                    self.axes[axis] = dir;
                }
            }
            Raw::Abs(..) => {}
        }
        None
    }
}

pub fn run(make: impl FnOnce(crate::client::Wake) -> DeckApp, opts: Options) -> Result<(), String> {
    let (card, out) = open_card(opts.card.clone(), Duration::from_secs(60))?;
    let (mw, mh) = out.mode.size();
    let rotate = opts.rotate.unwrap_or(if mh > mw { 90 } else { 0 });
    let gbm = gbm::Device::new(card).map_err(|e| format!("gbm: {e}"))?;
    let raw_display = RawDisplayHandle::Gbm(GbmDisplayHandle::new(
        NonNull::new(gbm.as_raw() as *mut _).ok_or("null gbm device")?,
    ));
    // SAFETY: the GBM device and surface outlive the EGL display, context and surface (all
    // live in this function's scope and are dropped in reverse order).
    let display = unsafe { glutin::api::egl::display::Display::new(raw_display) }
        .map_err(|e| format!("EGL display: {e}"))?;
    let template = ConfigTemplateBuilder::new()
        .with_surface_type(ConfigSurfaceTypes::WINDOW)
        .with_api(glutin::config::Api::GLES2 | glutin::config::Api::GLES3)
        .build();
    // Pick the config first, then give the GBM surface that config's own pixel format
    // (XRGB8888 preferred, ARGB8888 accepted): a mismatch means no frame ever scans out.
    let configs: Vec<_> = unsafe { display.find_configs(template) }
        .map_err(|e| format!("EGL configs: {e}"))?
        .collect();
    let visuals: Vec<u32> = configs.iter().map(|c| c.native_visual()).collect();
    let (idx, format) = match visuals.iter().position(|v| *v == XRGB8888) {
        Some(i) => (i, gbm::Format::Xrgb8888),
        None => match visuals.iter().position(|v| *v == ARGB8888) {
            Some(i) => (i, gbm::Format::Argb8888),
            None => {
                return Err(format!(
                    "no EGL config with an XRGB8888/ARGB8888 visual ({} configs: {visuals:x?})",
                    configs.len()
                ))
            }
        },
    };
    let config = configs.into_iter().nth(idx).ok_or("EGL config vanished")?;
    eprintln!("lapis-ui: EGL ready, scanout format {format:?}, rotation {rotate}");
    let surface = gbm
        .create_surface::<()>(
            u32::from(mw),
            u32::from(mh),
            format,
            gbm::BufferObjectFlags::SCANOUT | gbm::BufferObjectFlags::RENDERING,
        )
        .map_err(|e| format!("gbm surface: {e}"))?;
    let raw_window = RawWindowHandle::Gbm(GbmWindowHandle::new(
        NonNull::new(surface.as_raw() as *mut _).ok_or("null gbm surface")?,
    ));
    let ctx_attrs = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::Gles(Some(Version::new(3, 0))))
        .build(None);
    let not_current = unsafe { display.create_context(&config, &ctx_attrs) }
        .map_err(|e| format!("GLES context: {e}"))?;
    let (nw, nh) = (
        NonZeroU32::new(u32::from(mw)).ok_or("zero width")?,
        NonZeroU32::new(u32::from(mh)).ok_or("zero height")?,
    );
    let surf_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(raw_window, nw, nh);
    let egl_surface = unsafe { display.create_window_surface(&config, &surf_attrs) }
        .map_err(|e| format!("EGL surface: {e}"))?;
    let context = not_current
        .make_current(&egl_surface)
        .map_err(|e| format!("make current: {e}"))?;
    let gl = Arc::new(unsafe {
        glow::Context::from_loader_function_cstr(|s| display.get_proc_address(s))
    });
    let blit = unsafe { compile(&gl) }?;
    let mut painter = egui_glow::Painter::new(gl.clone(), "", None, false)
        .map_err(|e| format!("egui painter: {e}"))?;

    let (tx, rx) = mpsc::channel::<Wake>();
    let wake_tx = Mutex::new(tx.clone());
    let wake: crate::client::Wake = Arc::new(move || {
        if let Ok(t) = wake_tx.lock() {
            let _ = t.send(Wake::Repaint);
        }
    });
    spawn_input(tx);
    let mut app = make(wake);
    let ctx = egui::Context::default();
    let mut input = InputState::new(opts.pads, opts.trackball_gain);
    let started = Instant::now();
    let mut current: Option<(gbm::BufferObject<()>, framebuffer::Handle)> = None;
    let mut next_frame = Instant::now();
    loop {
        let mut events = Vec::new();
        let mut pads = Vec::new();
        let timeout = next_frame.saturating_duration_since(Instant::now());
        let first = rx.recv_timeout(timeout).ok();
        for w in first.into_iter().chain(rx.try_iter()) {
            if let Wake::Input(raw) = w {
                if let Some(ptt) = input.feed(raw, &mut events, &mut pads) {
                    app.set_ptt(ptt);
                }
            }
        }
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(W as f32, H as f32),
            )),
            time: Some(started.elapsed().as_secs_f64()),
            modifiers: input.modifiers(),
            events,
            focused: true,
            ..Default::default()
        };
        let output = ctx.run(raw_input, |ctx| app.frame(ctx, &pads));
        let delay = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .unwrap_or(Duration::from_secs(1));
        next_frame = Instant::now() + delay.min(Duration::from_secs(1));
        let prims = ctx.tessellate(output.shapes, output.pixels_per_point);
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(blit.fbo));
            gl.viewport(0, 0, W as i32, H as i32);
            gl.clear_color(0.03, 0.04, 0.055, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        painter.paint_and_update_textures(
            [W, H],
            output.pixels_per_point,
            &prims,
            &output.textures_delta,
        );
        if app.wants_capture() {
            let mut px = vec![0u8; (W * H * 4) as usize];
            unsafe {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(blit.fbo));
                gl.read_pixels(
                    0,
                    0,
                    W as i32,
                    H as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut px)),
                );
            }
            let row = (W * 4) as usize;
            let flipped: Vec<u8> = px.chunks_exact(row).rev().flatten().copied().collect();
            app.deliver_capture(W, H, flipped);
        }
        let (m, o) = rotation(rotate);
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.viewport(0, 0, i32::from(mw), i32::from(mh));
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.use_program(Some(blit.program));
            gl.uniform_4_f32(
                gl.get_uniform_location(blit.program, "m").as_ref(),
                m[0],
                m[1],
                m[2],
                m[3],
            );
            gl.uniform_2_f32(
                gl.get_uniform_location(blit.program, "o").as_ref(),
                o[0],
                o[1],
            );
            gl.uniform_1_i32(gl.get_uniform_location(blit.program, "tex").as_ref(), 0);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(blit.tex));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(blit.vbo));
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
        egl_surface
            .swap_buffers(&context)
            .map_err(|e| format!("swap: {e}"))?;
        let bo =
            unsafe { surface.lock_front_buffer() }.map_err(|e| format!("front buffer: {e}"))?;
        let fb = gbm
            .add_framebuffer(&bo, 24, 32)
            .map_err(|e| format!("add framebuffer: {e}"))?;
        match &current {
            None => {
                gbm.set_crtc(out.crtc, Some(fb), (0, 0), &[out.conn], Some(out.mode))
                    .map_err(|e| format!("set crtc (is another program DRM master?): {e}"))?;
                eprintln!("lapis-ui: first frame on screen");
            }
            Some(_) => {
                gbm.page_flip(out.crtc, fb, PageFlipFlags::EVENT, None)
                    .map_err(|e| format!("page flip: {e}"))?;
                // Vsync: wait for the flip before the previous buffer goes back to GBM.
                'flip: loop {
                    for ev in gbm
                        .receive_events()
                        .map_err(|e| format!("drm events: {e}"))?
                    {
                        if let drm::control::Event::PageFlip(_) = ev {
                            break 'flip;
                        }
                    }
                }
            }
        }
        if let Some((_old_bo, old_fb)) = current.replace((bo, fb)) {
            let _ = gbm.destroy_framebuffer(old_fb);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_maps_corners() {
        // 90°: the native top-left (a=0, b=1) shows the landscape bottom-left (s=0, t=0).
        let (m, o) = rotation(90);
        let f = |a: f32, b: f32| (m[0] * a + m[1] * b + o[0], m[2] * a + m[3] * b + o[1]);
        assert_eq!(f(0.0, 1.0), (0.0, 0.0));
        assert_eq!(f(1.0, 0.0), (1.0, 1.0));
        assert_eq!(rotation(0), ([1.0, 0.0, 0.0, 1.0], [0.0, 0.0]));
    }

    #[test]
    fn evdev_to_egui() {
        let mut s = InputState::new(PadMap::default(), 2.0);
        let (mut ev, mut pads) = (Vec::new(), Vec::new());
        s.feed(Raw::Key(input::KEY_LEFTSHIFT, 1), &mut ev, &mut pads);
        s.feed(Raw::Key(30, 1), &mut ev, &mut pads);
        assert!(ev.iter().any(|e| *e == egui::Event::Text("A".into())));
        s.feed(Raw::Key(0x121, 1), &mut ev, &mut pads);
        assert_eq!(
            pads,
            vec![PadEvent {
                button: crate::input::Pad::A,
                pressed: true
            }]
        );
        s.feed(Raw::Rel(0, 10), &mut ev, &mut pads);
        assert_eq!(s.pointer.x, W as f32 / 2.0 + 20.0);
        ev.clear();
        s.feed(Raw::Abs(0, -127), &mut ev, &mut pads);
        assert!(matches!(
            ev[0],
            egui::Event::Key {
                key: egui::Key::ArrowLeft,
                pressed: true,
                ..
            }
        ));
        assert_eq!(
            s.feed(Raw::Key(input::KEY_PLAYPAUSE, 1), &mut ev, &mut pads),
            Some(true)
        );
    }
}
