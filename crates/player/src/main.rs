//! RC10 Player: plays music through the Celoxica RC10's hardware MP3 decoder
//! (examples/rc10-mp3), with a song list, bass boost and a 10-band graphic
//! equaliser whose settings travel over USB between pieces of the song.

mod app;

fn main() -> eframe::Result {
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_title("RC10 Player")
        .with_app_id("fpgaflow-player")
        .with_inner_size([1040.0, 700.0])
        .with_min_inner_size([780.0, 520.0]);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "fpgaflow-player",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
