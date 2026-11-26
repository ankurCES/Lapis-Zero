//! Host preview (feature `desktop`): the HUD in a 1280x720 window via eframe. For laptops only;
//! the uConsole image is built with `--no-default-features --features kms`.

use crate::app::{Capture, DeckApp};
use crate::input::{desktop_pad, PadEvent};
use std::sync::Arc;

struct Window {
    app: DeckApp,
    shot_pending: bool,
    exit_after_shot: bool,
}

impl eframe::App for Window {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // F5..F10 stand in for the uConsole's A, B, X, Y, Select and Start.
        let pads = ctx.input_mut(|i| {
            let mut pads = Vec::new();
            i.events.retain(|e| match e {
                egui::Event::Key {
                    key,
                    pressed,
                    repeat,
                    ..
                } => match desktop_pad(*key) {
                    Some(button) => {
                        if !repeat {
                            pads.push(PadEvent {
                                button,
                                pressed: *pressed,
                            });
                        }
                        false
                    }
                    None => true,
                },
                _ => true,
            });
            pads
        });
        self.app.frame(ctx, &pads);
        if self.app.wants_capture() && !self.shot_pending {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            self.shot_pending = true;
        }
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| {
                if let egui::Event::Screenshot { image, .. } = e {
                    Some(image.clone())
                } else {
                    None
                }
            })
        });
        if let Some(img) = shot {
            self.shot_pending = false;
            let to_file = matches!(self.app.capture, Some(Capture::File(_)));
            let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            self.app
                .deliver_capture(img.size[0] as u32, img.size[1] as u32, rgba);
            if to_file && self.exit_after_shot {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

pub fn run(
    make: impl FnOnce(crate::client::Wake) -> DeckApp + 'static,
    exit_after_shot: bool,
) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 720.0])
            .with_resizable(false)
            .with_title("Lapis-Deck (host preview)"),
        ..Default::default()
    };
    eframe::run_native(
        "lapis-deck",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_pixels_per_point(1.0);
            let ctx = cc.egui_ctx.clone();
            let app = make(Arc::new(move || ctx.request_repaint()));
            Ok(Box::new(Window {
                app,
                shot_pending: false,
                exit_after_shot,
            }))
        }),
    )
    .map_err(|e| e.to_string())
}
