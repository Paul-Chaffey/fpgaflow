//! The player window: song list, transport, bass boost, graphic EQ, and the
//! board's status. One thread streams the song (`mp3::stream_song`, which
//! takes the settings between pieces); another reads the status.

use eframe::egui::{self, Color32, RichText};
use fpgaflow_celoxica::mp3::{self, Command, Ended, Mp3Status, Source};
use fpgaflow_celoxica::Board;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const BAND_NAMES: [&str; 10] = [
    "31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k",
];
const EXTS: [&str; 10] = [
    "mp3", "wav", "aif", "aiff", "flac", "ogg", "m4a", "opus", "wma", "aac",
];
const PRESETS: [(&str, [f64; 10]); 8] = [
    ("Flat", [0.0; 10]),
    ("Bass", [6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("Treble", [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 4.0, 5.0, 6.0]),
    (
        "Rock",
        [5.0, 4.0, 2.0, -1.0, -2.0, -1.0, 2.0, 3.0, 4.0, 4.0],
    ),
    ("Pop", [-1.0, 1.0, 3.0, 4.0, 3.0, 0.0, -1.0, -1.0, 1.0, 2.0]),
    ("Jazz", [3.0, 2.0, 1.0, 2.0, -1.0, -1.0, 0.0, 1.0, 2.0, 3.0]),
    (
        "Classical",
        [4.0, 3.0, 2.0, 1.0, -1.0, -1.0, 0.0, 2.0, 3.0, 4.0],
    ),
    (
        "Vocal",
        [-2.0, -2.0, -1.0, 1.0, 3.0, 4.0, 3.0, 1.0, 0.0, -1.0],
    ),
];
const BASS_DB: [u32; 4] = [0, 4, 8, 12];

/// Remembered between runs (~/.config/fpgaflow/player.json).
#[derive(Serialize, Deserialize, Default)]
struct Settings {
    folder: Option<PathBuf>,
    gains: [f64; 10],
    eq_on: bool,
}

fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("fpgaflow").join("player.json"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Stopped,
    Playing,
    Paused,
}

enum Msg {
    Started {
        song: usize,
        channels: usize,
        duration: Option<f64>,
        converted: bool,
        /// Where in the song this stream starts (seconds).
        start: f64,
    },
    Ended {
        song: usize,
        result: Result<Ended, String>,
    },
    Loaded(Result<(), String>),
}

/// The status thread's latest reading.
#[derive(Default)]
struct Shared {
    status: Option<Result<Mp3Status, String>>,
}

pub struct App {
    settings: Settings,
    songs: Vec<PathBuf>,
    filter: String,
    selected: Option<usize>,
    current: Option<usize>,
    state: State,
    channels: usize,
    duration: Option<f64>,
    converted: bool,
    /// Where the current stream starts in the song, and (until the board's
    /// sample count has restarted for it) the count it had before.
    start_offset: f64,
    await_reset: Option<u32>,
    /// The position slider while it is being dragged.
    seek_drag: Option<f64>,
    board: Option<Arc<Board>>,
    board_err: Option<String>,
    shared: Arc<Mutex<Shared>>,
    song_tx: Option<mpsc::Sender<Command>>,
    song_thread: Option<std::thread::JoinHandle<()>>,
    msg_tx: mpsc::Sender<Msg>,
    msg_rx: mpsc::Receiver<Msg>,
    eq_dirty: bool,
    eq_sent: Instant,
    /// Level bars' peak markers: dB and when each was set.
    peaks: [(f64, Instant); 10],
    peaks_at: Instant,
    message: Option<(bool, String)>,
    ctx: egui::Context,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings: Settings = settings_path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|d| serde_json::from_slice(&d).ok())
            .unwrap_or_else(|| Settings {
                folder: std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Music")),
                ..Default::default()
            });
        let (msg_tx, msg_rx) = mpsc::channel();
        let mut app = App {
            settings,
            songs: Vec::new(),
            filter: String::new(),
            selected: None,
            current: None,
            state: State::Stopped,
            channels: 2,
            duration: None,
            converted: false,
            start_offset: 0.0,
            await_reset: None,
            seek_drag: None,
            board: None,
            board_err: None,
            shared: Arc::new(Mutex::new(Shared::default())),
            song_tx: None,
            song_thread: None,
            msg_tx,
            msg_rx,
            eq_dirty: false,
            eq_sent: Instant::now(),
            peaks: [(-60.0, Instant::now()); 10],
            peaks_at: Instant::now(),
            message: None,
            ctx: cc.egui_ctx.clone(),
        };
        app.scan();
        app.connect();
        app
    }

    fn save_settings(&self) {
        if let Some(p) = settings_path() {
            let _ = std::fs::create_dir_all(p.parent().unwrap_or(Path::new(".")));
            if let Ok(d) = serde_json::to_vec_pretty(&self.settings) {
                let _ = std::fs::write(p, d);
            }
        }
    }

    /// Every audio file under the music folder.
    fn scan(&mut self) {
        self.songs.clear();
        let Some(root) = self.settings.folder.clone() else {
            return;
        };
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| EXTS.contains(&x.to_ascii_lowercase().as_str()))
                {
                    self.songs.push(p);
                }
            }
        }
        self.songs.sort();
        self.selected = None;
        self.current = None;
    }

    fn song_name(&self, i: usize) -> String {
        let p = &self.songs[i];
        let rel = self
            .settings
            .folder
            .as_ref()
            .and_then(|f| p.strip_prefix(f).ok())
            .unwrap_or(p);
        rel.with_extension("").display().to_string()
    }

    /// Open the RC10 and start the status thread.
    fn connect(&mut self) {
        match Board::open() {
            Ok(b) => {
                let b = Arc::new(b);
                self.board = Some(b.clone());
                self.board_err = None;
                let shared = self.shared.clone();
                let ctx = self.ctx.clone();
                std::thread::spawn(move || loop {
                    let s = b.mp3_status().map_err(|e| e.to_string());
                    shared.lock().unwrap_or_else(|e| e.into_inner()).status = Some(s);
                    ctx.request_repaint();
                    if Arc::strong_count(&b) == 1 {
                        return; // the app let go of the board
                    }
                    // 30 a second, for the level bars (reads are served
                    // while the stream is held back: measured in stage 3.0).
                    std::thread::sleep(Duration::from_millis(33));
                });
            }
            Err(e) => {
                self.board = None;
                self.board_err = Some(e.to_string());
            }
        }
    }

    fn status(&self) -> Option<Mp3Status> {
        match &self.shared.lock().unwrap_or_else(|e| e.into_inner()).status {
            Some(Ok(s)) => Some(s.clone()),
            _ => None,
        }
    }

    fn eq_command(&self) -> Command {
        Command::Eq(self.settings.eq_on.then_some(self.settings.gains))
    }

    /// Send a setting: through the song's stream while one plays, else as a
    /// stream of its own (the board is idle then).
    fn send(&mut self, c: Command) {
        if let Some(tx) = &self.song_tx {
            if self.state != State::Stopped {
                let _ = tx.send(c);
                return;
            }
        }
        let Some(b) = self.board.clone() else { return };
        let rate = self.status().map_or(0, |s| s.rate as usize);
        let rec = match &c {
            Command::Bass(v) => mp3::record(mp3::T_BASS, &[*v]),
            Command::Eq(g) => mp3::eq_record(rate, g.as_ref()),
            _ => return,
        };
        std::thread::spawn(move || {
            let _ = b.usb_send(&rec);
        });
    }

    fn play(&mut self, i: usize) {
        self.play_at(i, 0.0);
    }

    /// Jump to `t` seconds into the current song (or start it there).
    fn seek(&mut self, t: f64) {
        match (self.state, &self.song_tx) {
            (State::Playing | State::Paused, Some(tx)) => {
                let _ = tx.send(Command::Seek(t));
                self.state = State::Playing;
                self.start_offset = t;
                self.await_reset = self.status().map(|s| s.samples).filter(|&n| n > 0);
            }
            _ => {
                if let Some(i) = self.current.or(self.selected) {
                    self.play_at(i, t);
                }
            }
        }
    }

    fn play_at(&mut self, i: usize, at: f64) {
        let Some(b) = self.board.clone() else {
            self.message = Some((false, "no RC10 connected".into()));
            return;
        };
        if !self.status().is_some_and(|s| s.framed()) {
            self.message = Some((
                false,
                "the RC10 isn't running the \"M6\" MP3 player (bass and EQ over USB): load it first".into(),
            ));
            return;
        }
        self.stop();
        let (tx, rx) = mpsc::channel();
        self.song_tx = Some(tx);
        self.current = Some(i);
        self.selected = Some(i);
        self.state = State::Playing;
        self.duration = None;
        self.start_offset = at;
        self.await_reset = self.status().map(|s| s.samples).filter(|&n| n > 0);
        let path = self.songs[i].clone();
        let start = vec![self.eq_command()];
        let msg = self.msg_tx.clone();
        let ctx = self.ctx.clone();
        let previous = self.song_thread.take();
        self.song_thread = Some(std::thread::spawn(move || {
            if let Some(h) = previous {
                let _ = h.join();
            }
            let r = (|| -> Result<Ended, fpgaflow_celoxica::Error> {
                // A new song waits for the last one to play out; a seek only
                // for the stream to end (the new stream drops what the
                // decoder still held of the old position).
                b.mp3_wait_idle(Duration::from_secs(5))?;
                if at == 0.0 {
                    b.mp3_wait_drained(Duration::from_secs(5))?;
                }
                let mut at = at;
                loop {
                    let mut src = Source::open_at(&path, at)?;
                    let _ = msg.send(Msg::Started {
                        song: i,
                        channels: src.channels,
                        duration: src.duration,
                        converted: src.converted,
                        start: src.start,
                    });
                    ctx.request_repaint();
                    match mp3::stream_song(&b, &mut src, &start, &rx)? {
                        Ended::Seek(t) => {
                            drop(src);
                            at = t;
                            b.mp3_wait_idle(Duration::from_secs(5))?;
                        }
                        e => return Ok(e),
                    }
                }
            })();
            let _ = msg.send(Msg::Ended {
                song: i,
                result: r.map_err(|e| e.to_string()),
            });
            ctx.request_repaint();
        }));
    }

    fn stop(&mut self) {
        if let Some(tx) = self.song_tx.take() {
            let _ = tx.send(Command::Stop);
        }
        self.state = State::Stopped;
    }

    fn pause(&mut self, p: bool) {
        if let Some(tx) = &self.song_tx {
            let _ = tx.send(Command::Pause(p));
            self.state = if p { State::Paused } else { State::Playing };
        }
    }

    fn step(&mut self, d: isize) {
        if self.songs.is_empty() {
            return;
        }
        let n = self.songs.len() as isize;
        let i = self.current.or(self.selected).map_or(0, |c| c as isize + d);
        if (0..n).contains(&i) {
            self.play(i as usize);
        }
    }

    fn load_player(&mut self) {
        let Some(b) = self.board.clone() else { return };
        let start = std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Projects/FPGA/examples/rc10-mp3/build/impl"))
            .unwrap_or_default();
        let Some(file) = rfd::FileDialog::new()
            .set_title("MP3 player bitstream (examples/rc10-mp3)")
            .set_directory(start)
            .add_filter("Bitstream", &["bit"])
            .pick_file()
        else {
            return;
        };
        self.stop();
        let msg = self.msg_tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let r = std::fs::read(&file)
                .map_err(|e| e.to_string())
                .and_then(|d| b.configure(&d, |_, _| {}).map_err(|e| e.to_string()));
            let _ = msg.send(Msg::Loaded(r));
            ctx.request_repaint();
        });
    }

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::Started {
                song,
                channels,
                duration,
                converted,
                start,
            } => {
                if self.current == Some(song) {
                    self.channels = channels.max(1);
                    self.duration = duration;
                    self.converted = converted;
                    self.start_offset = start;
                }
            }
            Msg::Ended { song, result } => {
                if self.current != Some(song) {
                    return; // an older song, replaced
                }
                match result {
                    Ok(Ended::Finished) if self.state == State::Playing => {
                        if song + 1 < self.songs.len() {
                            self.play(song + 1);
                        } else {
                            self.state = State::Stopped;
                            self.song_tx = None;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        self.message = Some((false, e));
                        self.state = State::Stopped;
                        self.song_tx = None;
                    }
                }
            }
            Msg::Loaded(r) => {
                self.message = Some(match r {
                    Ok(()) => (true, "player loaded into the FPGA".into()),
                    Err(e) => (false, format!("loading failed: {e}")),
                });
            }
        }
    }

    // ------------------------------------------------------------------ UI

    fn transport(&mut self, ui: &mut egui::Ui) {
        let st = self.status();
        ui.horizontal(|ui| {
            let big = |t: &str| {
                egui::Button::new(RichText::new(t).size(20.0)).min_size(egui::vec2(44.0, 36.0))
            };
            if ui.add(big("⏮")).on_hover_text("Previous").clicked() {
                self.step(-1);
            }
            let (icon, tip) = match self.state {
                State::Playing => ("⏸", "Pause"),
                _ => ("▶", "Play"),
            };
            if ui.add(big(icon)).on_hover_text(tip).clicked() {
                match self.state {
                    State::Playing => self.pause(true),
                    State::Paused => self.pause(false),
                    State::Stopped => {
                        if let Some(i) = self
                            .selected
                            .or(self.current)
                            .or((!self.songs.is_empty()).then_some(0))
                        {
                            self.play(i);
                        }
                    }
                }
            }
            if ui.add(big("⏹")).on_hover_text("Stop").clicked() {
                self.stop();
            }
            if ui.add(big("⏭")).on_hover_text("Next").clicked() {
                self.step(1);
            }
            ui.add_space(12.0);
            ui.vertical(|ui| {
                let title = self
                    .current
                    .map(|i| self.song_name(i))
                    .unwrap_or_else(|| "—".into());
                ui.label(RichText::new(title).size(18.0).strong());
                // The board's sample count restarts with each stream; until it
                // has, the new stream hasn't begun playing.
                if let (Some(seen), Some(s)) = (self.await_reset, &st) {
                    if s.samples < seen {
                        self.await_reset = None;
                    }
                }
                let elapsed = match (&st, self.state) {
                    (Some(s), State::Playing | State::Paused) => {
                        self.start_offset
                            + if self.await_reset.is_some() {
                                0.0
                            } else {
                                s.samples as f64 / (self.channels as f64 * s.rate_hz() as f64)
                            }
                    }
                    _ => 0.0,
                };
                let t = |x: f64| format!("{}:{:02}", (x as u64) / 60, (x as u64) % 60);
                let mut line = match self.duration {
                    Some(d) => format!("{} / {}", t(elapsed), t(d)),
                    None => t(elapsed),
                };
                if self.state == State::Paused {
                    line.push_str("   (paused)");
                }
                if self.converted && self.state != State::Stopped {
                    line.push_str("   converted by ffmpeg");
                }
                ui.label(line);
                if let Some(d) = self.duration {
                    // The position: drag (or click) to jump there.
                    let mut v = self.seek_drag.unwrap_or(elapsed.min(d));
                    ui.spacing_mut().slider_width = 420.0;
                    let r = ui
                        .add(egui::Slider::new(&mut v, 0.0..=d).show_value(false))
                        .on_hover_text("Drag to move through the song");
                    if r.dragged() {
                        self.seek_drag = Some(v);
                    } else {
                        if r.drag_stopped() || r.changed() {
                            self.seek(v);
                        }
                        self.seek_drag = None;
                    }
                }
            });
        });
    }

    fn song_list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .button("📁 Folder…")
                .on_hover_text("Choose the music folder")
                .clicked()
            {
                if let Some(f) = rfd::FileDialog::new()
                    .set_directory(self.settings.folder.clone().unwrap_or_default())
                    .pick_folder()
                {
                    self.settings.folder = Some(f);
                    self.save_settings();
                    self.scan();
                }
            }
            if ui
                .button("⟳")
                .on_hover_text("Scan the folder again")
                .clicked()
            {
                self.scan();
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("filter")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.small(
            self.settings
                .folder
                .as_ref()
                .map(|f| format!("{} — {} songs", f.display(), self.songs.len()))
                .unwrap_or_else(|| "no folder".into()),
        );
        ui.separator();
        let filter = self.filter.to_lowercase();
        let mut play = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for i in 0..self.songs.len() {
                    let name = self.song_name(i);
                    if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
                        continue;
                    }
                    let playing = self.current == Some(i) && self.state != State::Stopped;
                    let text = if playing {
                        RichText::new(format!("▶ {name}"))
                            .strong()
                            .color(Color32::from_rgb(90, 180, 255))
                    } else {
                        RichText::new(name)
                    };
                    let r = ui.selectable_label(self.selected == Some(i), text);
                    if r.clicked() {
                        self.selected = Some(i);
                    }
                    if r.double_clicked() {
                        play = Some(i);
                    }
                }
            });
        if let Some(i) = play {
            self.play(i);
        }
    }

    fn tone(&mut self, ui: &mut egui::Ui) {
        let st = self.status();
        // Bass boost: follows the board (the joystick changes it too).
        let bass = st.as_ref().map_or(0, |s| s.bass);
        ui.horizontal(|ui| {
            let label = format!("Bass boost  b{bass}  (+{} dB)", BASS_DB[bass as usize]);
            let on = bass > 0;
            let btn = egui::Button::new(RichText::new(label).size(16.0))
                .selected(on)
                .min_size(egui::vec2(220.0, 34.0));
            if ui
                .add_enabled(self.board.is_some(), btn)
                .on_hover_text("Off, +4, +8, +12 dB around 100 Hz (as the joystick's right button)")
                .clicked()
            {
                self.send(Command::Bass((bass + 1) % 4));
            }
            ui.add_space(24.0);
            if ui
                .checkbox(
                    &mut self.settings.eq_on,
                    RichText::new("Graphic EQ").size(16.0),
                )
                .changed()
            {
                self.eq_dirty = true;
            }
            egui::ComboBox::from_id_salt("preset")
                .selected_text("Presets")
                .show_ui(ui, |ui| {
                    for (name, g) in PRESETS {
                        if ui.selectable_label(false, name).clicked() {
                            self.settings.gains = g;
                            self.settings.eq_on = true;
                            self.eq_dirty = true;
                        }
                    }
                });
            if ui.button("Flat").clicked() {
                self.settings.gains = [0.0; 10];
                self.eq_dirty = true;
            }
        });
        ui.add_space(10.0);
        self.levels(ui, st.as_ref());
        ui.add_enabled_ui(self.settings.eq_on, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.add_space(2.0);
                    for db in ["+12", "+6", "0", "−6", "−12"] {
                        ui.label(RichText::new(db).small());
                        ui.add_space(30.0);
                    }
                });
                for (i, name) in BAND_NAMES.iter().enumerate() {
                    ui.vertical(|ui| {
                        let r = ui.add(
                            egui::Slider::new(&mut self.settings.gains[i], -12.0..=12.0)
                                .vertical()
                                .step_by(0.5)
                                .show_value(false),
                        );
                        if r.changed() {
                            self.eq_dirty = true;
                        }
                        if r.double_clicked() {
                            self.settings.gains[i] = 0.0;
                            self.eq_dirty = true;
                        }
                        ui.label(RichText::new(*name).strong());
                        ui.label(RichText::new(format!("{:+.1}", self.settings.gains[i])).small());
                    });
                    ui.add_space(6.0);
                }
            });
        });
        ui.small("Double-click a slider for 0 dB. Boosts never clip: the rest is turned down instead (raise the volume on the board).");
    }

    /// Live band levels (stage 3): bars in dBFS, -60..0, each with a peak
    /// marker that holds for a second and then falls 30 dB/s.
    fn levels(&mut self, ui: &mut egui::Ui, st: Option<&Mp3Status>) {
        const FLOOR: f64 = -60.0;
        let live = st.filter(|s| !s.idle || s.samples > 0);
        let now = Instant::now();
        let dt = now.duration_since(self.peaks_at).as_secs_f64();
        self.peaks_at = now;
        let (w, h) = (ui.available_width().min(640.0), 170.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
        let p = ui.painter_at(rect);
        let vis = ui.visuals();
        p.rect_filled(rect, 4.0, vis.extreme_bg_color);
        let plot = egui::Rect::from_min_max(
            rect.min + egui::vec2(34.0, 8.0),
            rect.max - egui::vec2(8.0, 22.0),
        );
        let y_of = |db: f64| {
            plot.bottom() - ((db - FLOOR) / -FLOOR).clamp(0.0, 1.0) as f32 * plot.height()
        };
        for db in [0.0, -12.0, -24.0, -36.0, -48.0, -60.0] {
            let y = y_of(db);
            p.line_segment(
                [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
                egui::Stroke::new(1.0, vis.faint_bg_color),
            );
            p.text(
                egui::pos2(rect.left() + 4.0, y),
                egui::Align2::LEFT_CENTER,
                format!("{db:.0}"),
                egui::FontId::proportional(10.0),
                vis.weak_text_color(),
            );
        }
        let slot = plot.width() / 10.0;
        for b in 0..10 {
            let x0 = plot.left() + slot * b as f32 + slot * 0.18;
            let x1 = plot.left() + slot * (b + 1) as f32 - slot * 0.18;
            let db = live.map_or(FLOOR, |s| s.level_db(b).max(FLOOR));
            let (pk, t) = &mut self.peaks[b];
            if db >= *pk {
                *pk = db;
                *t = now;
            } else if now.duration_since(*t) > Duration::from_secs(1) {
                *pk = (*pk - 30.0 * dt).max(db);
            }
            let top = y_of(db);
            // Green up to -12, amber to -3, red above.
            let c = if db > -3.0 {
                Color32::from_rgb(230, 80, 60)
            } else if db > -12.0 {
                Color32::from_rgb(230, 180, 60)
            } else {
                Color32::from_rgb(70, 190, 110)
            };
            if top < plot.bottom() {
                p.rect_filled(
                    egui::Rect::from_min_max(egui::pos2(x0, top), egui::pos2(x1, plot.bottom())),
                    2.0,
                    c,
                );
            }
            let py = y_of(*pk);
            if *pk > FLOOR {
                p.line_segment(
                    [egui::pos2(x0, py), egui::pos2(x1, py)],
                    egui::Stroke::new(2.0, vis.strong_text_color()),
                );
            }
            p.text(
                egui::pos2((x0 + x1) / 2.0, rect.bottom() - 11.0),
                egui::Align2::CENTER_CENTER,
                BAND_NAMES[b],
                egui::FontId::proportional(11.0),
                vis.text_color(),
            );
        }
        if live.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
        ui.add_space(10.0);
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match (&self.board, &self.board_err) {
                (None, e) => {
                    ui.colored_label(Color32::from_rgb(230, 120, 90), format!(
                        "RC10 not connected{}",
                        e.as_ref().map(|e| format!(": {e}")).unwrap_or_default()
                    ));
                    if ui.button("Connect").clicked() {
                        self.connect();
                    }
                }
                (Some(_), _) => {
                    let s = self.shared.lock().unwrap_or_else(|e| e.into_inner()).status.clone();
                    match s {
                        Some(Ok(s)) => {
                            ui.label(format!(
                                "RC10: MP3 player M{} · volume {} · {} · {:.1} kHz · bass b{} · EQ {} · underruns {}{}",
                                s.version,
                                s.volume,
                                if s.rz { "RZ" } else { "NRZ" },
                                s.rate_hz() as f64 / 1000.0,
                                s.bass,
                                if s.eq_on { "on" } else { "off" },
                                s.underruns,
                                if s.paused { " · paused" } else { "" }
                            ));
                            if !s.framed() && ui.button("Load player…").clicked() {
                                self.load_player();
                            }
                        }
                        Some(Err(e)) => {
                            ui.colored_label(Color32::from_rgb(230, 170, 60), e);
                            if ui.button("Load player…").clicked() {
                                self.load_player();
                            }
                        }
                        None => {
                            ui.label("RC10: reading…");
                        }
                    }
                }
            }
            if let Some((ok, m)) = &self.message {
                ui.separator();
                let c = if *ok { Color32::from_rgb(120, 200, 120) } else { Color32::from_rgb(230, 120, 90) };
                ui.colored_label(c, m);
            }
        });
    }
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(m) = self.msg_rx.try_recv() {
            self.handle(m);
        }
        // EQ changes: at most 10 a second, and once more when the slider stops.
        if self.eq_dirty && self.eq_sent.elapsed() >= Duration::from_millis(100) {
            self.eq_dirty = false;
            self.eq_sent = Instant::now();
            let c = self.eq_command();
            self.send(c);
            self.save_settings();
        } else if self.eq_dirty {
            self.ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("transport").show(ui, |ui| {
            ui.add_space(6.0);
            self.transport(ui);
            ui.add_space(6.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("songs")
            .resizable(true)
            .default_size(380.0)
            .show(ui, |ui| self.song_list(ui));
        egui::CentralPanel::default().show(ui, |ui| self.tone(ui));
    }

    fn on_exit(&mut self) {
        self.stop();
        self.save_settings();
    }
}
