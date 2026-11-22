//! fpgaflow desktop GUI.

mod app;
mod splash;
mod worker;

fn main() -> eframe::Result {
    let start_dir = std::env::args().nth(1).map(std::path::PathBuf::from);
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("FPGA Flow")
        .with_app_id("fpgaflow")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([800.0, 500.0]);
    // X11 uses this; Wayland takes the icon from fpgaflow.desktop via the app id.
    if let Some(icon) = splash::icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "fpgaflow",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, start_dir)))),
    )
}
