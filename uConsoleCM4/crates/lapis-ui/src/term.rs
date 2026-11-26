//! PTY/MUX: shells (portable-pty), SSH sessions to fleet hosts and UART consoles, parsed by
//! vt100 and drawn as a monospace grid with colours, scrollback and a cursor.

use crate::theme;
use egui::{Color32, FontId, Rect, Sense, Ui, Vec2};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct Term {
    pub title: String,
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Option<Box<dyn Write + Send>>,
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    _child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    pub alive: Arc<AtomicBool>,
    size: (u16, u16),
    pub scroll: usize,
}

impl Term {
    fn reader(
        parser: Arc<Mutex<vt100::Parser>>,
        alive: Arc<AtomicBool>,
        mut r: Box<dyn Read + Send>,
    ) {
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut p) = parser.lock() {
                            p.process(&buf[..n]);
                        }
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::TimedOut
                            || e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            alive.store(false, Ordering::Relaxed);
        });
    }

    pub fn spawn(title: &str, argv: Option<Vec<String>>) -> Result<Term, String> {
        let (rows, cols) = (24u16, 80u16);
        let pty = portable_pty::native_pty_system();
        let pair = pty
            .openpty(portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
        let mut cmd = match &argv {
            Some(a) if !a.is_empty() => {
                let mut c = portable_pty::CommandBuilder::new(&a[0]);
                c.args(&a[1..]);
                c
            }
            _ => portable_pty::CommandBuilder::new_default_prog(),
        };
        cmd.env("TERM", "xterm-256color");
        if let Ok(home) = std::env::var("HOME") {
            cmd.cwd(home);
        }
        let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 2000)));
        let alive = Arc::new(AtomicBool::new(true));
        Self::reader(parser.clone(), alive.clone(), reader);
        Ok(Term {
            title: title.into(),
            parser,
            writer: Some(writer),
            master: Some(pair.master),
            _child: Some(child),
            alive,
            size: (rows, cols),
            scroll: 0,
        })
    }

    pub fn serial(device: &str, baud: u32) -> Result<Term, String> {
        let port = serialport::new(device, baud)
            .timeout(std::time::Duration::from_millis(200))
            .open()
            .map_err(|e| format!("{device}: {e}"))?;
        let reader = port.try_clone().map_err(|e| e.to_string())?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 2000)));
        let alive = Arc::new(AtomicBool::new(true));
        Self::reader(parser.clone(), alive.clone(), Box::new(reader));
        Ok(Term {
            title: format!("{device} @{baud}"),
            parser,
            writer: Some(Box::new(port)),
            master: None,
            _child: None,
            alive,
            size: (24, 80),
            scroll: 0,
        })
    }

    pub fn write(&mut self, bytes: &[u8]) {
        if let Some(w) = self.writer.as_mut() {
            if w.write_all(bytes).and_then(|_| w.flush()).is_err() {
                self.alive.store(false, Ordering::Relaxed);
            }
        }
        self.scroll = 0;
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(4), cols.max(10));
        if (rows, cols) == self.size {
            return;
        }
        self.size = (rows, cols);
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
        if let Some(m) = &self.master {
            let _ = m.resize(portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    /// Plain text of the visible screen (tests, copy).
    pub fn text(&self) -> String {
        self.parser
            .lock()
            .map(|p| p.screen().contents())
            .unwrap_or_default()
    }

    pub fn feed(&self, bytes: &[u8]) {
        if let Ok(mut p) = self.parser.lock() {
            p.process(bytes);
        }
    }

    /// Draws the grid into `rect`, resizing the terminal to fit.
    pub fn draw(&mut self, ui: &mut Ui, rect: Rect, focused: bool) {
        let font = FontId::monospace(13.0);
        let (cw, ch) = ui.fonts(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let cols = ((rect.width() - 4.0) / cw).floor().max(10.0) as u16;
        let rows = ((rect.height() - 4.0) / ch).floor().max(4.0) as u16;
        self.resize(rows, cols);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::BG);
        let Ok(mut parser) = self.parser.lock() else {
            return;
        };
        parser.set_scrollback(self.scroll);
        let screen = parser.screen();
        let origin = rect.min + Vec2::splat(2.0);
        for r in 0..rows {
            let mut job = egui::text::LayoutJob::default();
            for c in 0..cols {
                let Some(cell) = screen.cell(r, c) else {
                    continue;
                };
                let (mut fg, mut bg) = (color(cell.fgcolor(), true), color(cell.bgcolor(), false));
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let pos = origin + Vec2::new(f32::from(c) * cw, f32::from(r) * ch);
                if bg != theme::BG {
                    painter.rect_filled(Rect::from_min_size(pos, Vec2::new(cw, ch)), 0.0, bg);
                }
                let s = cell.contents().to_string();
                let text = if s.is_empty() { " ".to_owned() } else { s };
                job.append(
                    &text,
                    0.0,
                    egui::TextFormat {
                        font_id: font.clone(),
                        color: if cell.bold() {
                            fg.gamma_multiply(1.2)
                        } else {
                            fg
                        },
                        ..Default::default()
                    },
                );
            }
            let galley = ui.fonts(|f| f.layout_job(job));
            painter.galley(
                origin + Vec2::new(0.0, f32::from(r) * ch),
                galley,
                theme::TEXT,
            );
        }
        if focused && !screen.hide_cursor() && self.scroll == 0 {
            let (cr, cc) = screen.cursor_position();
            let pos = origin + Vec2::new(f32::from(cc) * cw, f32::from(cr) * ch);
            painter.rect_filled(
                Rect::from_min_size(pos, Vec2::new(cw, ch)),
                0.0,
                theme::CYAN.gamma_multiply(0.6),
            );
        }
        if !self.alive.load(Ordering::Relaxed) {
            painter.text(
                rect.right_top() + Vec2::new(-8.0, 4.0),
                egui::Align2::RIGHT_TOP,
                "[exited]",
                font,
                theme::AMBER,
            );
        }
        drop(parser);
        let _ = ui.allocate_rect(rect, Sense::click());
    }
}

const ANSI: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 49, 49),
    (13, 188, 121),
    (229, 229, 16),
    (36, 114, 200),
    (188, 63, 188),
    (17, 168, 205),
    (229, 229, 229),
    (102, 102, 102),
    (241, 76, 76),
    (35, 209, 139),
    (245, 245, 67),
    (59, 142, 234),
    (214, 112, 214),
    (41, 184, 219),
    (255, 255, 255),
];

fn color(c: vt100::Color, fg: bool) -> Color32 {
    match c {
        vt100::Color::Default => {
            if fg {
                theme::TEXT
            } else {
                theme::BG
            }
        }
        vt100::Color::Idx(i) if i < 16 => {
            let (r, g, b) = ANSI[usize::from(i)];
            Color32::from_rgb(r, g, b)
        }
        vt100::Color::Idx(i) if i >= 232 => {
            let v = 8 + (i - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
        vt100::Color::Idx(i) => {
            let i = i - 16;
            let s = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(s(i / 36), s((i / 6) % 6), s(i % 6))
        }
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// Bytes a key press sends to a terminal.
pub fn key_bytes(key: egui::Key, m: egui::Modifiers) -> Option<Vec<u8>> {
    use egui::Key as K;
    if m.ctrl && !m.alt {
        let name = key.name();
        if name.len() == 1 {
            let c = name.as_bytes()[0].to_ascii_lowercase();
            if c.is_ascii_lowercase() {
                return Some(vec![c - b'a' + 1]);
            }
        }
        if key == K::OpenBracket {
            return Some(vec![0x1b]);
        }
    }
    Some(match key {
        K::Enter => b"\r".to_vec(),
        K::Backspace => vec![0x7f],
        K::Tab => b"\t".to_vec(),
        K::Escape => vec![0x1b],
        K::ArrowUp => b"\x1b[A".to_vec(),
        K::ArrowDown => b"\x1b[B".to_vec(),
        K::ArrowRight => b"\x1b[C".to_vec(),
        K::ArrowLeft => b"\x1b[D".to_vec(),
        K::Home => b"\x1b[H".to_vec(),
        K::End => b"\x1b[F".to_vec(),
        K::PageUp if !m.shift => b"\x1b[5~".to_vec(),
        K::PageDown if !m.shift => b"\x1b[6~".to_vec(),
        K::Delete => b"\x1b[3~".to_vec(),
        K::Insert => b"\x1b[2~".to_vec(),
        K::F1 => b"\x1bOP".to_vec(),
        K::F2 => b"\x1bOQ".to_vec(),
        K::F3 => b"\x1bOR".to_vec(),
        K::F4 => b"\x1bOS".to_vec(),
        _ => return None,
    })
}

#[derive(Default)]
pub struct Terms {
    pub list: Vec<Term>,
    pub active: usize,
    pub split: bool,
    pub focused: bool,
    pub error: Option<String>,
}

impl Terms {
    fn add(&mut self, t: Result<Term, String>) {
        match t {
            Ok(t) => {
                self.list.push(t);
                self.active = self.list.len() - 1;
                self.focused = true;
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    pub fn open_shell(&mut self) {
        let n = self.list.len() + 1;
        self.add(Term::spawn(&format!("shell {n}"), None));
    }

    pub fn open_command(&mut self, title: &str, argv: Vec<String>) {
        self.add(Term::spawn(title, Some(argv)));
    }

    pub fn open_serial(&mut self, device: &str, baud: u32) {
        self.add(Term::serial(device, baud));
    }

    pub fn has_focus(&self) -> bool {
        self.focused && !self.list.is_empty()
    }

    pub fn close_active(&mut self) {
        if self.active < self.list.len() {
            self.list.remove(self.active);
            self.active = self.active.saturating_sub(1);
        }
        self.focused = !self.list.is_empty();
    }

    /// Sends this frame's keyboard input to the focused terminal; returns true when consumed.
    pub fn input(&mut self, ctx: &egui::Context) {
        if !self.has_focus() {
            return;
        }
        let events = ctx.input(|i| i.events.clone());
        let Some(t) = self.list.get_mut(self.active) else {
            return;
        };
        for e in events {
            match e {
                egui::Event::Text(s) => t.write(s.as_bytes()),
                egui::Event::Paste(s) => t.write(s.as_bytes()),
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if modifiers.alt
                        && matches!(
                            key,
                            egui::Key::Num1
                                | egui::Key::Num2
                                | egui::Key::Num3
                                | egui::Key::Num4
                                | egui::Key::Num5
                                | egui::Key::Num6
                                | egui::Key::Num7
                        )
                    {
                        continue; // workspace switching stays global
                    }
                    if modifiers.ctrl && key == egui::Key::Space {
                        continue; // the palette
                    }
                    if modifiers.shift && key == egui::Key::PageUp {
                        t.scroll += 10;
                    } else if modifiers.shift && key == egui::Key::PageDown {
                        t.scroll = t.scroll.saturating_sub(10);
                    } else if let Some(b) = key_bytes(key, modifiers) {
                        t.write(&b);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_parsing() {
        assert_eq!(
            key_bytes(egui::Key::C, egui::Modifiers::CTRL),
            Some(vec![3])
        );
        assert_eq!(
            key_bytes(egui::Key::ArrowUp, egui::Modifiers::NONE),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_bytes(egui::Key::Enter, egui::Modifiers::NONE),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            color(vt100::Color::Idx(1), true),
            Color32::from_rgb(205, 49, 49)
        );
        assert_eq!(
            color(vt100::Color::Idx(16), true),
            Color32::from_rgb(0, 0, 0)
        );
    }

    #[test]
    fn a_real_shell_echoes() {
        let mut t = Term::spawn(
            "t",
            Some(vec![
                "sh".into(),
                "-c".into(),
                "printf 'lapis-deck-ok\\n'; sleep 1".into(),
            ]),
        )
        .unwrap();
        t.resize(10, 40);
        for _ in 0..100 {
            if t.text().contains("lapis-deck-ok") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(t.text().contains("lapis-deck-ok"), "{}", t.text());
        t.feed(b"\x1b[31mred\x1b[0m");
        assert!(t.text().contains("red"));
    }
}
