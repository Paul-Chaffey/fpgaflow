//! Start-up splash (also Help → About): the app icon and a short message.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

/// The application icon, 400×400 RGBA. Also installed as the desktop icon.
pub const ICON_PNG: &[u8] = include_bytes!("../../../data/icons/fpgaflow.png");

/// How long the start-up splash stays up unless dismissed sooner.
const AUTO_CLOSE: Duration = Duration::from_secs(5);

pub fn icon() -> Option<egui::IconData> {
    eframe::icon_data::from_png_bytes(ICON_PNG).ok()
}

pub struct Splash {
    texture: Option<egui::TextureHandle>,
    /// `Some` while visible; `None` deadline means it stays until closed.
    open: Option<Option<Instant>>,
}

impl Splash {
    pub fn new(ctx: &egui::Context) -> Self {
        let texture = icon().map(|i| {
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [i.width as usize, i.height as usize],
                &i.rgba,
            );
            ctx.load_texture("fpgaflow-icon", img, egui::TextureOptions::LINEAR)
        });
        Self {
            texture,
            open: Some(Some(Instant::now() + AUTO_CLOSE)),
        }
    }

    /// Show it again until the user closes it (Help → About).
    pub fn show_about(&mut self) {
        self.open = Some(None);
    }

    pub fn ui(&mut self, ctx: &egui::Context) {
        let Some(deadline) = self.open else { return };
        if let Some(d) = deadline {
            let now = Instant::now();
            if now >= d {
                self.open = None;
                return;
            }
            ctx.request_repaint_after(d - now);
        }
        let mut close = false;
        let resp = egui::Modal::new(egui::Id::unique("fpgaflow-splash")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                if let Some(t) = &self.texture {
                    ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(160.0, 160.0)));
                }
                ui.add_space(6.0);
                ui.label(RichText::new("FPGA Flow").size(26.0).strong());
                ui.add_space(4.0);
                ui.label("Open Source and freely available tools supported");
                ui.add_space(4.0);
                ui.label("Contact");
                let repo = env!("CARGO_PKG_REPOSITORY");
                ui.hyperlink_to(repo.trim_start_matches("https://"), repo);
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("version {}", env!("CARGO_PKG_VERSION")))
                        .small()
                        .color(Color32::GRAY),
                );
                ui.add_space(6.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if close || resp.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.open = None;
        }
    }
}
