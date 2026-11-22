//! fpgaflow desktop GUI.

mod app;
mod device_view;
mod splash;
mod worker;

fn main() -> eframe::Result {
    // fpgaflow-gui [PROJECT_DIR] [--tab NAME]
    let mut start_dir = None;
    let mut start_tab = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--tab" {
            start_tab = args.next();
        } else {
            start_dir = Some(std::path::PathBuf::from(a));
        }
    }
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
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, start_dir, start_tab.as_deref())))),
    )
}
