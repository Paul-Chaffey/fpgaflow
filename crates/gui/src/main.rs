//! fpgaflow desktop GUI.

mod app;
mod worker;

fn main() -> eframe::Result {
    let start_dir = std::env::args().nth(1).map(std::path::PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("fpgaflow")
            .with_app_id("fpgaflow")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "fpgaflow",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, start_dir)))),
    )
}
