//! Application state and panels.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

use eframe::egui::{self, Color32, RichText};
use fpgaflow_core::board;
use fpgaflow_core::config::{EditorPreset, UserConfig};
use fpgaflow_core::diag::{Diagnostic, Severity};
use fpgaflow_core::job::{JobEvent, Stream};
use fpgaflow_core::project::{Project, ProjectFile, SimTool, TargetSection, PROJECT_FILE};
use fpgaflow_core::stage::{Summary, Verdict};
use fpgaflow_core::tools::{HostCheck, ToolStatus};
use fpgaflow_editor::Position;
use fpgaflow_flow::boards::{self, Cable, Hotplug, Identity, UsbDevice};
use fpgaflow_flow::BuildOutput;
use tokio_util::sync::CancellationToken;

use crate::device_view::{DeviceRequest, DeviceView};
use crate::pin_view::{PinRequest, PinView};
use crate::worker::{self, Action, Msg, Outcome, Slot, SynthView};

const MAX_LOG_LINES: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Log,
    Claude,
    Problems,
    Reports,
    Device,
    Pins,
    Boards,
    Doctor,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Idle,
    Running,
    Ok,
    Failed,
    /// Succeeded, but sources changed since.
    Stale,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Note,
    Command,
    Out,
    Err,
    Done,
}

struct Running {
    action: Action,
    cancel: CancellationToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum AiKind {
    #[default]
    NewModule,
    Testbench,
    Constraints,
    Explain,
}

struct AiView {
    summary: String,
    checked_ok: bool,
    report: String,
    rounds: u32,
    proposals: Vec<(fpgaflow_flow::Proposal, bool)>,
}

#[derive(Default)]
struct AiPanel {
    kind: AiKind,
    description: String,
    module: String,
    explain_file: String,
    question: String,
    running: Option<CancellationToken>,
    status: Vec<String>,
    thinking: String,
    answer_chars: usize,
    result: Option<AiView>,
    error: Option<String>,
}

#[derive(Default)]
struct NewProjectForm {
    open: bool,
    name: String,
    parent: String,
    board: Option<String>,
    error: Option<String>,
}

pub struct App {
    cfg: UserConfig,
    project: Option<Project>,
    rt: tokio::runtime::Runtime,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    repaint: egui::Context,

    tab: Tab,
    log: Vec<(LineKind, String)>,
    diagnostics: Vec<Diagnostic>,
    hints: Vec<String>,
    status: BTreeMap<Slot, Status>,
    running: Option<Running>,
    message: Option<(bool, String)>,
    sim_tool: SimTool,

    last_sim: Option<(SimTool, Option<Verdict>)>,
    last_synth: Option<SynthView>,
    last_build: Option<BuildOutput>,

    cables: Vec<Cable>,
    usb: Vec<(UsbDevice, Identity)>,
    _hotplug: Option<boards::Watcher>,
    _fs_watch: Option<notify::RecommendedWatcher>,

    doctor: Option<(Vec<ToolStatus>, Vec<HostCheck>)>,
    new_project: NewProjectForm,
    ai: AiPanel,
    device: DeviceView,
    pins: PinView,
    /// Remembered between runs (last project folder).
    state: fpgaflow_core::state::State,
    flash_read: Option<FlashReadForm>,
    confirm_flash: bool,
    splash: crate::splash::Splash,
}

/// Tools › Read flash…
struct FlashReadForm {
    board: Option<&'static board::Board>,
    folder: String,
    file: String,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        start_dir: Option<PathBuf>,
        start_tab: Option<&str>,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let repaint = cc.egui_ctx.clone();
        let cfg = UserConfig::load().unwrap_or_default();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let mut app = Self {
            cables: fpgaflow_flow::cables(&cfg),
            cfg,
            project: None,
            rt,
            tx,
            rx,
            repaint,
            tab: Tab::Log,
            log: Vec::new(),
            diagnostics: Vec::new(),
            hints: Vec::new(),
            status: BTreeMap::new(),
            running: None,
            message: None,
            sim_tool: SimTool::Icarus,
            last_sim: None,
            last_synth: None,
            last_build: None,
            usb: Vec::new(),
            _hotplug: None,
            _fs_watch: None,
            doctor: None,
            new_project: NewProjectForm::default(),
            ai: AiPanel::default(),
            device: DeviceView::default(),
            pins: PinView::default(),
            state: fpgaflow_core::state::State::load(),
            flash_read: None,
            confirm_flash: false,
            splash: crate::splash::Splash::new(&cc.egui_ctx),
        };
        match worker::start_hotplug(app.repaint.clone(), app.tx.clone()) {
            Ok((present, w)) => {
                let cables = &app.cables;
                let usb = present
                    .into_iter()
                    .map(|d| {
                        let id = boards::identify(&d, cables);
                        (d, id)
                    })
                    .collect();
                app.usb = usb;
                app._hotplug = Some(w);
            }
            Err(e) => app.message = Some((false, format!("USB hotplug unavailable: {e}"))),
        }
        worker::spawn_doctor(app.repaint.clone(), app.tx.clone(), app.cfg.clone());
        let dir = start_dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        if let Ok(p) = Project::find(&dir) {
            app.set_project(p);
        }
        app.tab = match start_tab.map(str::to_ascii_lowercase).as_deref() {
            Some("log") | None => app.tab,
            Some("claude") => Tab::Claude,
            Some("problems") => Tab::Problems,
            Some("reports") => Tab::Reports,
            Some("device") => Tab::Device,
            Some("floorplan") => {
                app.device.start_floorplan();
                Tab::Device
            }
            Some("pins") => Tab::Pins,
            Some("boards") => Tab::Boards,
            Some("doctor") => Tab::Doctor,
            Some("settings") => Tab::Settings,
            Some(other) => {
                app.message = Some((false, format!("unknown tab {other:?}")));
                app.tab
            }
        };
        if app.tab != Tab::Log {
            // Skip the splash when opening straight into a tab.
            app.splash.close();
        }
        app
    }

    // ------------------------------------------------------------ state

    fn set_project(&mut self, p: Project) {
        self.state.last_project = Some(p.root.clone());
        self.state.save();
        self.sim_tool = p.file.sim.tool;
        self._fs_watch = match worker::watch_project(&p.root, self.repaint.clone(), self.tx.clone())
        {
            Ok(w) => Some(w),
            Err(e) => {
                self.message = Some((false, format!("file watching unavailable: {e}")));
                None
            }
        };
        self.status.clear();
        self.diagnostics.clear();
        self.last_sim = None;
        self.last_synth = None;
        self.last_build = None;
        // What was already done for this project (from build/status.json):
        // passed, failed, or stale because inputs changed since.
        use fpgaflow_core::status::{self, Step, StepState};
        let done = status::current(&p);
        for (step, (state, rec)) in &done {
            let slot = match step {
                Step::Lint => Slot::Lint,
                Step::Sim => Slot::Sim,
                Step::Synth => Slot::Synth,
                Step::Build => Slot::Build,
                Step::Program => continue,
            };
            self.status.insert(
                slot,
                match state {
                    StepState::Ok => Status::Ok,
                    StepState::Failed => Status::Failed,
                    StepState::Stale => Status::Stale,
                },
            );
            let when = rec.finished_unix;
            let what = match state {
                StepState::Ok => "done",
                StepState::Failed => "failed",
                StepState::Stale => "done, sources changed since",
            };
            self.note(format!(
                "previously: {slot:?} {what}{} ({})",
                rec.detail
                    .as_deref()
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default(),
                ago(when)
            ));
        }
        if done.contains_key(&Step::Build) {
            self.last_build = fpgaflow_flow::last_build(&p);
        }
        self.device.clear();
        self.pins.clear();
        self.note(format!(
            "opened project {} ({})",
            p.file.project.name,
            p.root.display()
        ));
        self.project = Some(p);
    }

    fn note(&mut self, text: impl Into<String>) {
        self.push_log(LineKind::Note, text.into());
    }

    fn push_log(&mut self, kind: LineKind, text: String) {
        self.log.push((kind, text));
        if self.log.len() > MAX_LOG_LINES {
            let excess = self.log.len() - MAX_LOG_LINES;
            self.log.drain(..excess);
        }
    }

    fn save_floorplan(&mut self, regions: &[fpgaflow_flow::layout_model::floorplan::Region]) {
        let (Some(project), Some(data)) = (self.project.clone(), self.device.data.clone()) else {
            return;
        };
        match fpgaflow_flow::save_floorplan(&project, &data.device, regions) {
            Ok(path) => {
                self.device.saved();
                self.note(format!(
                    "floorplan: {} region(s) written to {}; Build to apply",
                    regions.len(),
                    fpgaflow_flow::rel(&project, &path).display()
                ));
                // The file may be newly listed in fpgaflow.toml.
                match Project::load(&project.root) {
                    Ok(p) => self.project = Some(p),
                    Err(e) => self.message = Some((false, format!("fpgaflow.toml: {e}"))),
                }
                if let Some(st) = self.status.get_mut(&Slot::Build) {
                    if *st == Status::Ok {
                        *st = Status::Stale;
                    }
                }
            }
            Err(e) => self.message = Some((false, format!("floorplan: {e:#}"))),
        }
    }

    fn open_flash_read(&mut self) {
        let board = fpgaflow_flow::connected_board(self.project.as_ref());
        let ext = match board.and_then(|b| b.program.as_ref()).map(|p| p.method) {
            Some(board::ProgramMethod::Celoxica) => "tar",
            _ => "bin",
        };
        let stamp = std::process::Command::new("date")
            .arg("+%Y%m%d-%H%M")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        self.flash_read = Some(FlashReadForm {
            board,
            folder: fpgaflow_core::paths::images_dir(&self.cfg)
                .display()
                .to_string(),
            file: format!(
                "{}-flash-{stamp}.{ext}",
                board.map(|b| b.id.as_str()).unwrap_or("board")
            ),
        });
    }

    fn flash_windows(&mut self, ctx: &egui::Context) {
        if let Some(mut f) = self.flash_read.take() {
            let mut open = true;
            let mut go = false;
            egui::Window::new("Read flash")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    match f.board {
                        Some(b) => {
                            ui.label(format!(
                                "{}: {}",
                                b.name,
                                fpgaflow_flow::flash_name(b).unwrap_or("no readable flash")
                            ));
                        }
                        None => {
                            ui.colored_label(
                                Color32::from_rgb(220, 80, 70),
                                "No known board is connected.",
                            );
                        }
                    }
                    ui.small("Reading never changes the flash.");
                    egui::Grid::new("flash_read").num_columns(2).show(ui, |ui| {
                        ui.label("Folder");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut f.folder).desired_width(320.0));
                            if ui.button("Browse…").clicked() {
                                if let Some(d) = rfd::FileDialog::new()
                                    .set_directory(&f.folder)
                                    .pick_folder()
                                {
                                    f.folder = d.display().to_string();
                                }
                            }
                        });
                        ui.end_row();
                        ui.label("File name");
                        ui.add(egui::TextEdit::singleline(&mut f.file).desired_width(320.0));
                        ui.end_row();
                    });
                    let out = PathBuf::from(&f.folder).join(&f.file);
                    if out.exists() {
                        ui.colored_label(
                            Color32::from_rgb(200, 170, 60),
                            "That file exists and will be replaced.",
                        );
                    }
                    let ready = f.board.and_then(fpgaflow_flow::flash_name).is_some()
                        && !f.file.trim().is_empty()
                        && self.running.is_none();
                    if ui
                        .add_enabled(ready, egui::Button::new("Read flash"))
                        .clicked()
                    {
                        go = true;
                    }
                });
            if go {
                let out = PathBuf::from(&f.folder).join(f.file.trim());
                if let Some(b) = f.board {
                    self.start_flash_read(b, out);
                }
            } else if open {
                self.flash_read = Some(f);
            }
        }
        if self.confirm_flash {
            let board = self.board_connected();
            let what = board.and_then(fpgaflow_flow::flash_name).unwrap_or("flash");
            let replaces = match board.and_then(|b| b.program.as_ref()).map(|p| p.method) {
                Some(board::ProgramMethod::Celoxica) => {
                    "This replaces flash file 1, which the RC10 boots from: Celoxica's demo menu, if it is still there."
                }
                _ => "This replaces whatever the Platform Flash holds now. The FPGA boots from it when the configuration mode jumpers select Master Serial.",
            };
            let mut open = true;
            let mut go = false;
            egui::Window::new("Program flash")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Write this project's bitstream to the {what} on the {}?",
                        board.map(|b| b.name.as_str()).unwrap_or("board")
                    ));
                    ui.label(replaces);
                    ui.small("Use Tools › Read flash… first to keep a copy of what is there.");
                    ui.horizontal(|ui| {
                        if ui.button("Write flash").clicked() {
                            go = true;
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_flash = false;
                        }
                    });
                });
            if !open {
                self.confirm_flash = false;
            }
            if go {
                self.confirm_flash = false;
                self.start(Action::ProgramFlash);
            }
        }
    }

    fn start_flash_read(&mut self, b: &'static board::Board, out: PathBuf) {
        if self.running.is_some() {
            return;
        }
        let cancel = CancellationToken::new();
        self.status.insert(Slot::Program, Status::Running);
        self.note(format!(
            "---- Read flash: {} -> {} ----",
            b.name,
            out.display()
        ));
        self.tab = Tab::Log;
        worker::spawn_flash_read(
            &self.rt,
            self.repaint.clone(),
            self.tx.clone(),
            b,
            out,
            self.cfg.clone(),
            cancel.clone(),
        );
        self.running = Some(Running {
            action: Action::ReadFlash,
            cancel,
        });
    }

    fn load_pins(&mut self) {
        let Some(project) = self.project.clone() else {
            return;
        };
        if self.pins.loading {
            return;
        }
        self.pins.loading = true;
        self.pins.error = None;
        worker::spawn_pins(
            &self.rt,
            self.repaint.clone(),
            self.tx.clone(),
            project,
            self.cfg.clone(),
        );
    }

    fn load_layout(&mut self) {
        let Some(project) = self.project.clone() else {
            return;
        };
        if self.device.loading {
            return;
        }
        self.device.loading = true;
        self.device.error = None;
        worker::spawn_layout(
            &self.rt,
            self.repaint.clone(),
            self.tx.clone(),
            project,
            self.cfg.clone(),
        );
    }

    fn start(&mut self, action: Action) {
        let Some(project) = self.project.clone() else {
            return;
        };
        if self.running.is_some() {
            return;
        }
        let cancel = CancellationToken::new();
        self.diagnostics.clear();
        self.hints.clear();
        self.status.insert(action.slot(), Status::Running);
        self.note(format!("---- {} ----", action.label()));
        self.tab = Tab::Log;
        worker::spawn_action(
            &self.rt,
            self.repaint.clone(),
            self.tx.clone(),
            project,
            self.cfg.clone(),
            action,
            cancel.clone(),
        );
        self.running = Some(Running { action, cancel });
    }

    fn absorb(&mut self, s: &Summary) {
        let mut d = s.diagnostics.clone();
        d.retain(|d| d.severity != Severity::Note);
        self.diagnostics.extend(d);
        self.diagnostics
            .sort_by(|a, b| (a.severity, &a.file, a.line).cmp(&(b.severity, &b.file, b.line)));
        self.diagnostics.dedup();
        self.hints.extend(s.hints.iter().cloned());
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Event(ev) => match ev {
                JobEvent::Note { text } => self.push_log(LineKind::Note, text),
                JobEvent::Started { command, .. } => {
                    self.push_log(LineKind::Command, format!("$ {command}"))
                }
                JobEvent::Line { stream, text, .. } => self.push_log(
                    if stream == Stream::Stderr {
                        LineKind::Err
                    } else {
                        LineKind::Out
                    },
                    text,
                ),
                JobEvent::Finished {
                    job,
                    outcome,
                    duration,
                } => self.push_log(
                    LineKind::Done,
                    format!("[{job}: {outcome}, {:.2}s]", duration.as_secs_f64()),
                ),
            },
            Msg::Done(action, result) => self.finish(action, result),
            Msg::Pins(r) => {
                if let Err(e) = &r {
                    self.push_log(LineKind::Err, format!("pins: {e}"));
                }
                self.pins.set_data(r);
            }
            Msg::Layout(r) => {
                if let Err(e) = &r {
                    self.push_log(LineKind::Err, format!("layout: {e}"));
                }
                self.device.set_data(r);
            }
            Msg::Hotplug(Hotplug::Added(d)) => {
                let id = boards::identify(&d, &self.cables);
                if id != Identity::Unknown {
                    self.note(format!("USB connected: {} {}", d.id(), id_label(&id)));
                }
                self.usb.retain(|(x, _)| x.syspath != d.syspath);
                self.usb.push((d, id));
            }
            Msg::Hotplug(Hotplug::Removed(d)) => {
                let id = boards::identify(&d, &self.cables);
                if id != Identity::Unknown {
                    self.note(format!("USB disconnected: {} {}", d.id(), id_label(&id)));
                }
                self.usb.retain(|(x, _)| x.syspath != d.syspath);
            }
            Msg::Doctor(t, h) => self.doctor = Some((t, h)),
            Msg::Ai(ev) => {
                use fpgaflow_flow::ai::AiEvent;
                match ev {
                    AiEvent::Status(t) => self.ai.status.push(t),
                    AiEvent::Thinking(t) => self.ai.thinking.push_str(&t),
                    AiEvent::Text(t) => self.ai.answer_chars += t.len(),
                    AiEvent::Checked { round, ok, report } => self.ai.status.push(if ok {
                        format!("round {round} check: passed")
                    } else {
                        format!("round {round} check: failed, sending the errors back\n{report}")
                    }),
                }
            }
            Msg::AiDone(r) => {
                self.ai.running = None;
                match r {
                    Ok(res) => {
                        self.ai.result = Some(AiView {
                            summary: res.outcome.generated.summary,
                            checked_ok: res.outcome.checked_ok,
                            report: res.outcome.report,
                            rounds: res.outcome.rounds,
                            proposals: res.proposals.into_iter().map(|p| (p, true)).collect(),
                        });
                        self.note("Claude: answer ready for review (Claude tab)");
                    }
                    Err(e) => {
                        self.ai.error = Some(e.clone());
                        self.message = Some((false, format!("Claude: {e}")));
                    }
                }
            }
            Msg::FsChanged(paths) => self.on_fs_change(paths),
        }
    }

    fn finish(&mut self, action: Action, result: Result<Outcome, String>) {
        self.running = None;
        let ok = match result {
            Err(e) => {
                self.push_log(LineKind::Err, format!("{}: {e}", action.label()));
                self.message = Some((false, format!("{}: {e}", action.label())));
                false
            }
            Ok(Outcome::Summary(s)) => {
                self.absorb(&s);
                if let Action::Sim(tool) = action {
                    self.last_sim = Some((tool, s.verdict));
                }
                s.ok
            }
            Ok(Outcome::Synth(v)) => {
                self.absorb(&v.summary);
                let ok = v.summary.ok;
                self.last_synth = Some(v);
                ok
            }
            Ok(Outcome::Build(b)) => {
                for s in &b.stages {
                    self.absorb(s);
                }
                if let Some(why) = &b.failure {
                    self.push_log(LineKind::Err, format!("build: {why}"));
                }
                let ok = b.ok;
                if b.bitstream.is_some() || ok {
                    self.device.stale = true;
                }
                self.last_build = Some(b);
                ok
            }
        };
        self.status
            .insert(action.slot(), if ok { Status::Ok } else { Status::Failed });
        let verdict = match (action, &self.last_sim) {
            (Action::Sim(_), Some((_, Some(Verdict::Pass)))) => " (PASS)",
            (Action::Sim(_), Some((_, Some(Verdict::Fail)))) => " (FAIL)",
            _ => "",
        };
        let text = format!(
            "{}: {}{verdict}",
            action.label(),
            if ok { "ok" } else { "failed" }
        );
        self.note(text.clone());
        if self.message.as_ref().is_none_or(|(good, _)| *good || ok) {
            self.message = Some((ok, text));
        }
        if !ok && !self.diagnostics.is_empty() {
            self.tab = Tab::Problems;
        } else if ok && matches!(action, Action::Synth | Action::Build) {
            self.tab = Tab::Reports;
        }
    }

    fn on_fs_change(&mut self, paths: Vec<PathBuf>) {
        // A UCF edited in the editor (or by the pin planner) updates the Pins tab.
        if paths.iter().any(|p| self.pins.watches(p)) {
            self.pins.refresh();
        }
        let Some(p) = &self.project else { return };
        let manifest = p.manifest_path();
        if paths.contains(&manifest) {
            match Project::load(&p.root) {
                Ok(np) => {
                    self.project = Some(np);
                    self.note("fpgaflow.toml changed: project reloaded");
                }
                Err(e) => self.message = Some((false, format!("fpgaflow.toml: {e}"))),
            }
        }
        // Waves and editor backups don't invalidate anything.
        let relevant = paths.iter().any(|x| {
            let s = x.to_string_lossy();
            !(s.ends_with('~') || s.contains("/.") || s.contains("/waves/"))
        });
        if relevant {
            for st in self.status.values_mut() {
                if *st == Status::Ok {
                    *st = Status::Stale;
                }
            }
        }
    }

    fn open_in_editor(&mut self, path: &Path, pos: Option<Position>) {
        if let Err(e) = fpgaflow_editor::open(&self.cfg.editor, path, pos) {
            self.message = Some((false, format!("editor: {e}")));
        }
    }

    fn project_board(&self) -> Option<&'static board::Board> {
        self.project
            .as_ref()?
            .file
            .target
            .as_ref()?
            .board
            .as_deref()
            .and_then(board::board)
    }

    fn board_connected(&self) -> Option<&'static board::Board> {
        let b = self.project_board()?;
        self.usb
            .iter()
            .any(|(d, _)| b.matches_usb(d.vid, d.pid))
            .then_some(b)
    }

    // ------------------------------------------------------------ panels

    fn menu(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("Project", |ui| {
                if ui.button("New project…").clicked() {
                    self.new_project = NewProjectForm {
                        open: true,
                        parent: self.state.open_dir().display().to_string(),
                        ..Default::default()
                    };
                    ui.close();
                }
                if ui.button("Open project…").clicked() {
                    ui.close();
                    if let Some(dir) = rfd::FileDialog::new()
                        .set_title("Open fpgaflow project")
                        .set_directory(self.state.open_dir())
                        .pick_folder()
                    {
                        match Project::find(&dir) {
                            Ok(p) => self.set_project(p),
                            Err(e) => self.message = Some((false, e.to_string())),
                        }
                    }
                }
                ui.separator();
                let manifest = self.project.as_ref().map(|p| p.manifest_path());
                if ui
                    .add_enabled(
                        manifest.is_some(),
                        egui::Button::new(format!("Edit {PROJECT_FILE}")),
                    )
                    .clicked()
                {
                    ui.close();
                    if let Some(m) = manifest {
                        self.open_in_editor(&m, None);
                    }
                }
                if ui.button("Edit user settings (config.toml)").clicked() {
                    ui.close();
                    match UserConfig::ensure_exists() {
                        Ok(path) => self.open_in_editor(&path, None),
                        Err(e) => self.message = Some((false, e.to_string())),
                    }
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Tools", |ui| {
                if ui
                    .button("Device view")
                    .on_hover_text("Where the last build placed things, routing, critical paths")
                    .clicked()
                {
                    ui.close();
                    self.tab = Tab::Device;
                }
                if ui
                    .button("Pin planner")
                    .on_hover_text("Assign ports to package pins")
                    .clicked()
                {
                    ui.close();
                    self.tab = Tab::Pins;
                }
                if ui
                    .button("Floorplanner")
                    .on_hover_text("Draw regions that confine modules (Device tab, edit mode)")
                    .clicked()
                {
                    ui.close();
                    self.tab = Tab::Device;
                    self.device.start_floorplan();
                }
                ui.separator();
                if ui
                    .button("Read flash…")
                    .on_hover_text("Save the connected board's flash contents to a file")
                    .clicked()
                {
                    ui.close();
                    self.open_flash_read();
                }
                let flash_ok = self
                    .board_connected()
                    .and_then(fpgaflow_flow::flash_name)
                    .is_some();
                if ui
                    .add_enabled(
                        flash_ok && self.running.is_none(),
                        egui::Button::new("Program flash…"),
                    )
                    .on_hover_text(
                        "Write this project's bitstream to its board's flash, so it boots from it",
                    )
                    .on_disabled_hover_text(
                        "Needs a project whose board is connected and has flash fpgaflow can write",
                    )
                    .clicked()
                {
                    ui.close();
                    self.confirm_flash = true;
                }
                ui.separator();
                if ui.button("Re-scan tools (Doctor)").clicked() {
                    ui.close();
                    self.doctor = None;
                    worker::spawn_doctor(self.repaint.clone(), self.tx.clone(), self.cfg.clone());
                    self.tab = Tab::Doctor;
                }
                if ui.button("Clear log").clicked() {
                    ui.close();
                    self.log.clear();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("About FPGA Flow").clicked() {
                    ui.close();
                    self.splash.show_about();
                }
            });
        });
    }

    fn flow_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let has_project = self.project.is_some();
            let idle = self.running.is_none();
            let can = has_project && idle;
            let mut clicked = None;

            let mut slot_button =
                |ui: &mut egui::Ui, slot: Slot, action: Action, status: Option<&Status>| {
                    let (mark, color) = status_mark(status.copied().unwrap_or(Status::Idle));
                    let text = RichText::new(format!("{mark} {}", action.label())).color(color);
                    if ui.add_enabled(can, egui::Button::new(text)).clicked() {
                        clicked = Some(action);
                    }
                    let _ = slot;
                };
            slot_button(ui, Slot::Lint, Action::Lint, self.status.get(&Slot::Lint));
            ui.separator();
            egui::ComboBox::from_id_salt("sim_tool")
                .width(90.0)
                .selected_text(match self.sim_tool {
                    SimTool::Icarus => "Icarus",
                    SimTool::Verilator => "Verilator",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.sim_tool, SimTool::Icarus, "Icarus");
                    ui.selectable_value(&mut self.sim_tool, SimTool::Verilator, "Verilator");
                });
            slot_button(
                ui,
                Slot::Sim,
                Action::Sim(self.sim_tool),
                self.status.get(&Slot::Sim),
            );
            if ui
                .add_enabled(has_project, egui::Button::new("〰 Waves"))
                .clicked()
            {
                if let Some(p) = &self.project {
                    match fpgaflow_flow::open_waves(p, &self.cfg, self.sim_tool) {
                        Ok(_) => {}
                        Err(e) => self.message = Some((false, format!("{e:#}"))),
                    }
                }
            }
            ui.separator();
            slot_button(
                ui,
                Slot::Synth,
                Action::Synth,
                self.status.get(&Slot::Synth),
            );
            slot_button(
                ui,
                Slot::Build,
                Action::Build,
                self.status.get(&Slot::Build),
            );
            ui.separator();
            let connected = self.board_connected().is_some();
            // Say exactly why Program is off: no board in the project, or
            // its board isn't plugged in (and which known board is).
            let why_not = match self.project_board() {
                None => "This project has no board: set [target] board in fpgaflow.toml".to_string(),
                Some(b) => {
                    let other = board::boards()
                        .iter()
                        .find(|o| o.id != b.id && self.usb.iter().any(|(d, _)| o.matches_usb(d.vid, d.pid)));
                    match other {
                        Some(o) => format!(
                            "This project is for the {}, which is not connected. The {} is connected; open a project for that board to program it.",
                            b.name, o.name
                        ),
                        None => format!("The project's board, the {}, is not connected.", b.name),
                    }
                }
            };
            let prog_status = self.status.get(&Slot::Program).copied();
            let (mark, color) = status_mark(prog_status.unwrap_or(Status::Idle));
            if ui
                .add_enabled(
                    can && connected,
                    egui::Button::new(RichText::new(format!("{mark} Program")).color(color)),
                )
                .on_disabled_hover_text(if connected { "Busy, or no project open".to_string() } else { why_not.clone() })
                .clicked()
            {
                clicked = Some(Action::Program);
            }
            if ui
                .add_enabled(can && connected, egui::Button::new("Detect"))
                .on_disabled_hover_text(if connected { "Busy, or no project open".to_string() } else { why_not })
                .clicked()
            {
                clicked = Some(Action::Detect);
            }
            ui.separator();
            if let Some(r) = &self.running {
                ui.spinner();
                ui.label(format!("{}…", r.action.label()));
                if ui.button("⏹ Cancel").clicked() {
                    r.cancel.cancel();
                }
            }
            if let Some(a) = clicked {
                self.start(a);
            }
        });
    }

    fn project_tree(&mut self, ui: &mut egui::Ui) {
        let Some(p) = self.project.clone() else {
            ui.label("No project open.");
            if ui.button("Open project…").clicked() {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Open fpgaflow project")
                    .set_directory(self.state.open_dir())
                    .pick_folder()
                {
                    match Project::find(&dir) {
                        Ok(p) => self.set_project(p),
                        Err(e) => self.message = Some((false, e.to_string())),
                    }
                }
            }
            if ui.button("New project…").clicked() {
                self.new_project = NewProjectForm {
                    open: true,
                    parent: self.state.open_dir().display().to_string(),
                    ..Default::default()
                };
            }
            return;
        };
        ui.heading(&p.file.project.name);
        ui.small(p.root.display().to_string());
        if let Some(t) = &p.file.target {
            ui.small(format!(
                "{} {}-{}-{} via {}",
                t.family.as_deref().unwrap_or("?"),
                t.device.as_deref().unwrap_or("?"),
                t.speed.as_deref().unwrap_or("?"),
                t.package.as_deref().unwrap_or("?"),
                t.backend.as_deref().unwrap_or("default"),
            ));
        }
        ui.separator();
        let mut open = None;
        if ui.link(PROJECT_FILE).clicked() {
            open = Some(p.manifest_path());
        }
        let groups: [(&str, Vec<PathBuf>); 3] = [
            ("RTL", p.rtl_files().unwrap_or_default()),
            ("Testbench", p.tb_files().unwrap_or_default()),
            ("Constraints", fpgaflow_pnr_constraints(&p)),
        ];
        for (title, files) in groups {
            egui::CollapsingHeader::new(format!("{title} ({})", files.len()))
                .default_open(true)
                .show(ui, |ui| {
                    for f in files {
                        let name = f.strip_prefix(&p.root).unwrap_or(&f).display().to_string();
                        if ui.link(name).on_hover_text("Open in editor").clicked() {
                            open = Some(f.clone());
                        }
                    }
                });
        }
        if let Some(f) = open {
            self.open_in_editor(&f, None);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match self.board_connected() {
                Some(b) => ui.colored_label(
                    Color32::from_rgb(60, 170, 90),
                    format!("● {} connected", b.name),
                ),
                None => match self.project_board() {
                    Some(b) => {
                        ui.colored_label(Color32::GRAY, format!("○ {} not connected", b.name))
                    }
                    None => ui.colored_label(Color32::GRAY, "○ no board set for this project"),
                },
            };
            ui.separator();
            if let Some((good, text)) = &self.message {
                let c = if *good {
                    Color32::from_rgb(60, 170, 90)
                } else {
                    Color32::from_rgb(220, 80, 70)
                };
                ui.colored_label(c, text);
            }
        });
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let errors = self
                .diagnostics
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .count();
            let warns = self
                .diagnostics
                .iter()
                .filter(|d| d.severity == Severity::Warning)
                .count();
            for (tab, label) in [
                (Tab::Log, "Log".to_string()),
                (
                    Tab::Claude,
                    if self.ai.running.is_some() {
                        "Claude …".to_string()
                    } else {
                        "Claude".to_string()
                    },
                ),
                (Tab::Problems, format!("Problems ({errors}E/{warns}W)")),
                (Tab::Reports, "Reports".to_string()),
                (Tab::Device, "Device".to_string()),
                (Tab::Pins, "Pins".to_string()),
                (
                    Tab::Boards,
                    format!(
                        "Boards ({})",
                        self.usb
                            .iter()
                            .filter(|(_, i)| *i != Identity::Unknown)
                            .count()
                    ),
                ),
                (Tab::Doctor, "Doctor".to_string()),
                (Tab::Settings, "Settings".to_string()),
            ] {
                ui.selectable_value(&mut self.tab, tab, label);
            }
        });
        ui.separator();
        match self.tab {
            Tab::Log => self.log_view(ui),
            Tab::Claude => self.ai_view(ui),
            Tab::Problems => self.problems_view(ui),
            Tab::Reports => self.reports_view(ui),
            Tab::Device | Tab::Pins if self.project.is_none() => {
                ui.label("No project open: use Project › Open project… (or New project…).");
                ui.small(
                    "The Device and Pins tabs show the open project's chip, placement and pins.",
                );
            }
            Tab::Device => {
                if self.device.stale && !self.device.loading && self.running.is_none() {
                    self.load_layout();
                }
                match self.device.ui(ui) {
                    Some(DeviceRequest::Load) => self.load_layout(),
                    Some(DeviceRequest::SaveFloorplan(regions)) => self.save_floorplan(&regions),
                    None => {}
                }
            }
            Tab::Pins => match self.pins.ui(ui) {
                Some(PinRequest::Load) => self.load_pins(),
                Some(PinRequest::Open(file, line)) => {
                    self.open_in_editor(&file, Some(Position { line, col: None }))
                }
                None => {}
            },
            Tab::Boards => self.boards_view(ui),
            Tab::Doctor => self.doctor_view(ui),
            Tab::Settings => self.settings_view(ui),
        }
    }

    fn log_view(&mut self, ui: &mut egui::Ui) {
        let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
        egui::ScrollArea::both()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show_rows(ui, row_h, self.log.len(), |ui, range| {
                for (kind, text) in &self.log[range] {
                    let color = match kind {
                        LineKind::Note => Color32::from_rgb(110, 160, 230),
                        LineKind::Command => Color32::from_rgb(160, 160, 160),
                        LineKind::Out => ui.visuals().text_color(),
                        LineKind::Err => Color32::from_rgb(230, 150, 90),
                        LineKind::Done => Color32::from_rgb(150, 150, 150),
                    };
                    ui.label(RichText::new(text).monospace().color(color));
                }
            });
    }

    fn problems_view(&mut self, ui: &mut egui::Ui) {
        if self.diagnostics.is_empty() && self.hints.is_empty() {
            ui.label("No problems from the last run.");
            return;
        }
        for h in self.hints.clone() {
            ui.colored_label(Color32::from_rgb(110, 160, 230), format!("hint: {h}"));
        }
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        let fixable = self
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error);
        if ui
            .add_enabled(
                fixable && self.ai.running.is_none(),
                egui::Button::new("✨ Ask Claude to fix these errors"),
            )
            .clicked()
        {
            let diagnostics = self
                .diagnostics
                .iter()
                .filter(|d| d.severity != Severity::Note)
                .map(|d| {
                    let mut d = d.clone();
                    d.file = d.file.strip_prefix(&root).unwrap_or(&d.file).to_path_buf();
                    d.to_string()
                })
                .collect();
            self.start_ai(worker::AiRequest::FixErrors { diagnostics });
        }
        let mut open = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for d in &self.diagnostics {
                    ui.horizontal(|ui| {
                        let (tag, c) = match d.severity {
                            Severity::Error => ("error", Color32::from_rgb(220, 80, 70)),
                            Severity::Warning => ("warning", Color32::from_rgb(220, 170, 60)),
                            Severity::Note => ("note", Color32::GRAY),
                        };
                        ui.colored_label(c, tag);
                        let file = d.file.strip_prefix(&root).unwrap_or(&d.file);
                        let loc = match d.col {
                            Some(c) => format!("{}:{}:{c}", file.display(), d.line),
                            None => format!("{}:{}", file.display(), d.line),
                        };
                        if ui
                            .link(RichText::new(loc).monospace())
                            .on_hover_text("Open in editor at this line")
                            .clicked()
                        {
                            open = Some((
                                d.file.clone(),
                                Position {
                                    line: d.line,
                                    col: d.col,
                                },
                            ));
                        }
                        if let Some(code) = &d.code {
                            ui.small(code);
                        }
                        ui.label(&d.message);
                    });
                }
            });
        if let Some((f, pos)) = open {
            self.open_in_editor(&f, Some(pos));
        }
    }

    fn reports_view(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                if let Some((tool, verdict)) = &self.last_sim {
                    ui.heading("Simulation");
                    let (t, c) = match verdict {
                        Some(Verdict::Pass) => ("PASS", Color32::from_rgb(60, 170, 90)),
                        Some(Verdict::Fail) => ("FAIL", Color32::from_rgb(220, 80, 70)),
                        _ => ("no PASS/FAIL line printed", Color32::GRAY),
                    };
                    ui.horizontal(|ui| {
                        ui.label(format!("{tool:?}:"));
                        ui.colored_label(c, t);
                    });
                    ui.separator();
                }
                let util = self
                    .last_build
                    .as_ref()
                    .and_then(|b| b.utilisation.as_ref())
                    .or(self
                        .last_synth
                        .as_ref()
                        .and_then(|s| s.utilisation.as_ref()));
                if let Some(u) = util {
                    ui.heading("Synthesis");
                    if let Some(s) = &self.last_synth {
                        ui.small(&s.target);
                        ui.small(format!("netlist: {}", s.netlist.display()));
                    }
                    egui::Grid::new("util")
                        .striped(true)
                        .num_columns(2)
                        .show(ui, |ui| {
                            for (cat, n) in &u.by_category {
                                ui.label(cat.label());
                                ui.monospace(n.to_string());
                                ui.end_row();
                            }
                            ui.label(RichText::new("Total cells").strong());
                            ui.monospace(u.cells.to_string());
                            ui.end_row();
                        });
                    ui.separator();
                }
                let Some(b) = &self.last_build else {
                    if util.is_none() && self.last_sim.is_none() {
                        ui.label("Run Simulate, Synthesise or Build to see reports.");
                    }
                    return;
                };
                ui.heading(format!(
                    "Implementation ({})",
                    b.part.as_deref().unwrap_or("?")
                ));
                if let Some(why) = &b.failure {
                    ui.colored_label(Color32::from_rgb(220, 80, 70), why);
                }
                if !b.resources.is_empty() {
                    egui::Grid::new("res")
                        .striped(true)
                        .num_columns(3)
                        .show(ui, |ui| {
                            for r in &b.resources {
                                let pct = if r.available > 0 {
                                    100.0 * r.used as f64 / r.available as f64
                                } else {
                                    0.0
                                };
                                ui.label(&r.name);
                                ui.monospace(format!("{} / {}", r.used, r.available));
                                ui.monospace(format!("{pct:.1}%"));
                                ui.end_row();
                            }
                        });
                }
                if let Some(t) = &b.timing {
                    ui.add_space(6.0);
                    match (t.errors, t.fmax_mhz) {
                        (Some(0), Some(f)) => ui.colored_label(
                            Color32::from_rgb(60, 170, 90),
                            format!(
                                "Timing met — Fmax {f:.1} MHz (min period {:.3} ns)",
                                t.min_period_ns.unwrap_or(0.0)
                            ),
                        ),
                        (Some(e), _) if e > 0 => ui.colored_label(
                            Color32::from_rgb(220, 80, 70),
                            format!("{e} timing error(s), score {}", t.score.unwrap_or(0)),
                        ),
                        _ => ui.label("No timing summary"),
                    };
                }
                if !b.pins.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new("Pins").strong());
                    egui::Grid::new("pins")
                        .striped(true)
                        .num_columns(5)
                        .show(ui, |ui| {
                            for h in ["Signal", "Pin", "Dir", "I/O standard", "Constraint"] {
                                ui.label(RichText::new(h).strong());
                            }
                            ui.end_row();
                            for r in &b.pins {
                                ui.monospace(&r.signal);
                                ui.monospace(&r.pin);
                                ui.label(&r.direction);
                                ui.label(&r.io_standard);
                                let c = if r.constraint == "LOCATED" {
                                    Color32::from_rgb(60, 170, 90)
                                } else {
                                    Color32::from_rgb(220, 80, 70)
                                };
                                ui.colored_label(c, &r.constraint);
                                ui.end_row();
                            }
                        });
                }
                if let Some(bit) = &b.bitstream {
                    ui.add_space(6.0);
                    ui.label(format!("Bitstream: {}", bit.display()));
                }
            });
    }

    fn boards_view(&mut self, ui: &mut egui::Ui) {
        ui.label("Boards and programming cables on USB (updates live):");
        let mut any = false;
        egui::Grid::new("usb")
            .striped(true)
            .num_columns(3)
            .show(ui, |ui| {
                for (d, id) in &self.usb {
                    if *id == Identity::Unknown {
                        continue;
                    }
                    any = true;
                    ui.monospace(d.id());
                    ui.label(id_label(id));
                    ui.small(format!("bus {} dev {}", d.bus, d.dev));
                    ui.end_row();
                }
            });
        if !any {
            ui.colored_label(Color32::GRAY, "none detected");
        }
        ui.separator();
        ui.label(format!("Known board profiles ({}):", board::boards().len()));
        for b in board::boards() {
            ui.small(format!(
                "{} — {} ({}-{}-{})",
                b.id, b.name, b.device, b.speed, b.package
            ));
        }
    }

    fn doctor_view(&mut self, ui: &mut egui::Ui) {
        let Some((tools, host)) = &self.doctor else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Scanning tools…");
            });
            return;
        };
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                egui::Grid::new("doctor")
                    .striped(true)
                    .num_columns(4)
                    .show(ui, |ui| {
                        for t in tools {
                            let (m, c) = if t.found() {
                                ("✔", Color32::from_rgb(60, 170, 90))
                            } else {
                                ("—", Color32::GRAY)
                            };
                            ui.colored_label(c, m);
                            ui.monospace(t.spec.id);
                            ui.label(t.spec.stage.label());
                            ui.small(match (&t.path, &t.version) {
                                (Some(p), Some(v)) => format!("{v}  [{}]", p.display()),
                                (Some(p), None) => p.display().to_string(),
                                (None, _) => "not found".into(),
                            });
                            ui.end_row();
                        }
                    });
                ui.separator();
                for h in host {
                    let (m, c) = if h.ok {
                        ("✔", Color32::from_rgb(60, 170, 90))
                    } else {
                        ("!", Color32::from_rgb(220, 80, 70))
                    };
                    ui.horizontal(|ui| {
                        ui.colored_label(c, m);
                        ui.label(h.name);
                        ui.small(&h.detail);
                    });
                    if let Some(fix) = &h.fix {
                        ui.monospace(format!("    fix: {fix}"));
                    }
                }
            });
    }

    fn settings_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("Editor");
        ui.label(
            "All project files (HDL, fpgaflow.toml, constraints, settings) open in this editor.",
        );
        egui::ComboBox::from_id_salt("editor")
            .selected_text(self.cfg.editor.preset.label())
            .show_ui(ui, |ui| {
                for p in EditorPreset::ALL {
                    ui.selectable_value(&mut self.cfg.editor.preset, p, p.label());
                }
            });
        if self.cfg.editor.preset == EditorPreset::Custom {
            let mut with = self.cfg.editor.command.clone().unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label("Command ({file} {line} {col}):");
                ui.text_edit_singleline(&mut with);
            });
            self.cfg.editor.command = (!with.is_empty()).then_some(with);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Save settings").clicked() {
                let path = UserConfig::default_path();
                self.message = Some(match self.cfg.save_to(&path) {
                    Ok(()) => (true, format!("saved {}", path.display())),
                    Err(e) => (false, e.to_string()),
                });
            }
            if ui.button("Open config.toml in editor").clicked() {
                match UserConfig::ensure_exists() {
                    Ok(path) => self.open_in_editor(&path, None),
                    Err(e) => self.message = Some((false, e.to_string())),
                }
            }
        });
        ui.add_space(12.0);
        ui.heading("Claude");
        use fpgaflow_core::AiBackend;
        egui::Grid::new("ai_settings")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Backend");
                egui::ComboBox::from_id_salt("ai_backend")
                    .selected_text(match self.cfg.ai.backend {
                        AiBackend::ClaudeCode => "Claude Code CLI (your login)",
                        AiBackend::Api => "Anthropic API (API key)",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.cfg.ai.backend,
                            AiBackend::ClaudeCode,
                            "Claude Code CLI (your login)",
                        );
                        ui.selectable_value(
                            &mut self.cfg.ai.backend,
                            AiBackend::Api,
                            "Anthropic API (API key)",
                        );
                    });
                ui.end_row();
                ui.label("Model");
                ui.text_edit_singleline(&mut self.cfg.ai.model);
                ui.end_row();
                ui.label("Effort");
                egui::ComboBox::from_id_salt("ai_effort")
                    .selected_text(self.cfg.ai.effort.clone())
                    .show_ui(ui, |ui| {
                        for e in ["low", "medium", "high", "xhigh", "max"] {
                            ui.selectable_value(&mut self.cfg.ai.effort, e.to_string(), e);
                        }
                    });
                ui.end_row();
                ui.label("Fix-up rounds");
                ui.add(egui::DragValue::new(&mut self.cfg.ai.max_fix_rounds).range(0..=6));
                ui.end_row();
            });
        if self.cfg.ai.backend == AiBackend::Api {
            ui.small("API key: ANTHROPIC_API_KEY, or [ai] api_key_command in config.toml (e.g. a KWallet lookup).");
        }
        ui.small("Press \"Save settings\" above to keep these.");
        ui.add_space(8.0);
        ui.small("Tool search paths and per-tool overrides are in config.toml.");
    }

    fn start_ai(&mut self, req: worker::AiRequest) {
        let Some(project) = self.project.clone() else {
            return;
        };
        if self.ai.running.is_some() {
            return;
        }
        let cancel = CancellationToken::new();
        self.ai.status.clear();
        self.ai.thinking.clear();
        self.ai.answer_chars = 0;
        self.ai.result = None;
        self.ai.error = None;
        self.tab = Tab::Claude;
        worker::spawn_ai(
            &self.rt,
            self.repaint.clone(),
            self.tx.clone(),
            project,
            self.cfg.clone(),
            req,
            cancel.clone(),
        );
        self.ai.running = Some(cancel);
    }

    fn rtl_modules(&self) -> Vec<String> {
        let Some(p) = &self.project else {
            return Vec::new();
        };
        p.rtl_files()
            .unwrap_or_default()
            .iter()
            .filter_map(|f| std::fs::read_to_string(f).ok())
            .flat_map(|t| fpgaflow_hdl::scan::declared_modules(&t))
            .collect()
    }

    fn ai_view(&mut self, ui: &mut egui::Ui) {
        use fpgaflow_core::AiBackend;
        if self.project.is_none() {
            ui.label("Open a project first.");
            return;
        }
        let (route, cost) = match self.cfg.ai.backend {
            AiBackend::ClaudeCode => ("Claude Code", "your Claude plan"),
            AiBackend::Api => ("the Anthropic API", "API credits"),
        };
        ui.label(format!(
            "{} via {route}: each request uses {cost}",
            self.cfg.ai.model
        ));
        let busy = self.ai.running.is_some();
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.ai.kind, AiKind::NewModule, "New module");
                ui.selectable_value(&mut self.ai.kind, AiKind::Testbench, "Testbench");
                ui.selectable_value(&mut self.ai.kind, AiKind::Constraints, "Constraints");
                ui.selectable_value(&mut self.ai.kind, AiKind::Explain, "Explain");
            });
            match self.ai.kind {
                AiKind::NewModule => {
                    ui.label("Describe the module (behaviour, ports, parameters, clocking):");
                }
                AiKind::Testbench => {
                    let mods = self.rtl_modules();
                    if self.ai.module.is_empty() {
                        if let Some(m) = mods.first() {
                            self.ai.module = m.clone();
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.label("Module");
                        egui::ComboBox::from_id_salt("ai_module")
                            .selected_text(self.ai.module.clone())
                            .show_ui(ui, |ui| {
                                for m in mods {
                                    ui.selectable_value(&mut self.ai.module, m.clone(), m);
                                }
                            });
                    });
                    ui.label("What to test (optional):");
                }
                AiKind::Constraints => {
                    ui.label("What each top-level port connects to on the board (pins come only from the board's master UCF):");
                }
                AiKind::Explain => {
                    ui.horizontal(|ui| {
                        ui.label("File (relative to the project)");
                        ui.text_edit_singleline(&mut self.ai.explain_file);
                    });
                    ui.label("Question:");
                }
            }
            let text = if self.ai.kind == AiKind::Explain { &mut self.ai.question } else { &mut self.ai.description };
            ui.add(egui::TextEdit::multiline(text).desired_rows(4).desired_width(f32::INFINITY));
        });
        ui.horizontal(|ui| {
            let ready = match self.ai.kind {
                AiKind::NewModule | AiKind::Constraints => !self.ai.description.trim().is_empty(),
                AiKind::Testbench => !self.ai.module.is_empty(),
                AiKind::Explain => !self.ai.explain_file.trim().is_empty(),
            };
            if ui
                .add_enabled(!busy && ready, egui::Button::new("✨ Ask Claude"))
                .clicked()
            {
                let req = match self.ai.kind {
                    AiKind::NewModule => worker::AiRequest::NewModule {
                        description: self.ai.description.clone(),
                    },
                    AiKind::Testbench => worker::AiRequest::Testbench {
                        module: self.ai.module.clone(),
                        description: self.ai.description.clone(),
                    },
                    AiKind::Constraints => worker::AiRequest::Constraints {
                        description: self.ai.description.clone(),
                    },
                    AiKind::Explain => worker::AiRequest::Explain {
                        file: self
                            .project
                            .as_ref()
                            .map(|p| p.root.join(self.ai.explain_file.trim()))
                            .unwrap_or_default(),
                        question: if self.ai.question.trim().is_empty() {
                            "What does it do, and is anything wrong?".into()
                        } else {
                            self.ai.question.clone()
                        },
                    },
                };
                self.start_ai(req);
            }
            if let Some(c) = &self.ai.running {
                ui.spinner();
                ui.label(format!(
                    "working… ({} chars of answer so far)",
                    self.ai.answer_chars
                ));
                if ui.button("⏹ Cancel").clicked() {
                    c.cancel();
                }
            }
        });
        ui.separator();
        let mut apply_now = false;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for s in &self.ai.status {
                ui.colored_label(Color32::from_rgb(110, 160, 230), s);
            }
            if !self.ai.thinking.is_empty() {
                egui::CollapsingHeader::new("Reasoning (summary)").default_open(busy).show(ui, |ui| {
                    ui.label(RichText::new(&self.ai.thinking).italics());
                });
            }
            if let Some(e) = &self.ai.error {
                ui.colored_label(Color32::from_rgb(220, 80, 70), e);
            }
            if let Some(r) = &mut self.ai.result {
                ui.add_space(6.0);
                ui.label(&r.summary);
                if !r.proposals.is_empty() {
                    ui.add_space(6.0);
                    if r.checked_ok {
                        ui.colored_label(
                            Color32::from_rgb(60, 170, 90),
                            format!("✔ checks passed (round {}): compiles, testbenches pass, pins from the master UCF", r.rounds),
                        );
                    } else {
                        ui.colored_label(
                            Color32::from_rgb(220, 80, 70),
                            format!("✖ still failing checks after {} round(s): review carefully", r.rounds),
                        );
                        ui.monospace(&r.report);
                    }
                    for (p, keep) in &mut r.proposals {
                        ui.horizontal(|ui| {
                            ui.checkbox(keep, "");
                            ui.label(RichText::new(&p.file.path).monospace().strong());
                            ui.small(if p.existed { "changed" } else { "new" });
                            ui.small(&p.file.purpose);
                        });
                        egui::CollapsingHeader::new(format!("diff {}", p.file.path)).id_salt(&p.file.path).show(ui, |ui| {
                            for line in p.diff.lines() {
                                let c = if line.starts_with('+') && !line.starts_with("+++") {
                                    Color32::from_rgb(90, 180, 100)
                                } else if line.starts_with('-') && !line.starts_with("---") {
                                    Color32::from_rgb(220, 100, 90)
                                } else if line.starts_with("@@") {
                                    Color32::from_rgb(110, 160, 230)
                                } else {
                                    ui.visuals().text_color()
                                };
                                ui.label(RichText::new(line).monospace().color(c));
                            }
                        });
                    }
                    apply_now = ui.button("Apply selected files to the project").clicked();
                }
            }
        });
        if apply_now {
            self.apply_ai();
        }
    }

    fn apply_ai(&mut self) {
        let (Some(project), Some(r)) = (self.project.clone(), &self.ai.result) else {
            return;
        };
        let chosen: Vec<&fpgaflow_flow::Proposal> = r
            .proposals
            .iter()
            .filter(|(_, k)| *k)
            .map(|(p, _)| p)
            .collect();
        match fpgaflow_flow::apply(&project, &chosen) {
            Ok(written) => {
                for w in &written {
                    let rel = w
                        .strip_prefix(&project.root)
                        .unwrap_or(w)
                        .display()
                        .to_string();
                    self.note(format!("Claude: wrote {rel}"));
                }
                self.message = Some((true, format!("wrote {} file(s)", written.len())));
                if let Ok(p) = Project::load(&project.root) {
                    self.project = Some(p);
                }
                for w in written {
                    self.open_in_editor(&w, None);
                }
                self.ai.result = None;
            }
            Err(e) => self.message = Some((false, format!("apply: {e:#}"))),
        }
    }

    fn new_project_window(&mut self, ctx: &egui::Context) {
        if !self.new_project.open {
            return;
        }
        let mut open = true;
        let mut create = false;
        egui::Window::new("New project")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let f = &mut self.new_project;
                egui::Grid::new("np").num_columns(2).show(ui, |ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut f.name);
                    ui.end_row();
                    ui.label("Location");
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut f.parent);
                        if ui.button("Browse…").clicked() {
                            if let Some(d) = rfd::FileDialog::new()
                                .set_directory(&f.parent)
                                .pick_folder()
                            {
                                f.parent = d.display().to_string();
                            }
                        }
                    });
                    ui.end_row();
                    ui.label("Board");
                    egui::ComboBox::from_id_salt("np_board")
                        .selected_text(
                            f.board
                                .as_deref()
                                .and_then(board::board)
                                .map_or("(none)", |b| b.name.as_str()),
                        )
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut f.board, None, "(none)");
                            for b in board::boards() {
                                ui.selectable_value(&mut f.board, Some(b.id.clone()), &b.name);
                            }
                        });
                    ui.end_row();
                });
                if let Some(e) = &f.error {
                    ui.colored_label(Color32::from_rgb(220, 80, 70), e);
                }
                create = ui
                    .add_enabled(!f.name.trim().is_empty(), egui::Button::new("Create"))
                    .clicked();
            });
        if create {
            match create_project(&self.new_project) {
                Ok(p) => {
                    self.new_project.open = false;
                    self.set_project(p);
                }
                Err(e) => self.new_project.error = Some(format!("{e:#}")),
            }
        }
        if !open {
            self.new_project.open = false;
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("menu").show(ui, |ui| self.menu(ui));
        egui::Panel::top("flow").show(ui, |ui| {
            ui.add_space(3.0);
            self.flow_bar(ui);
            ui.add_space(3.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("project")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.project_tree(ui));
            });
        egui::CentralPanel::default().show(ui, |ui| self.tabs(ui));
        self.new_project_window(&ctx);
        self.flash_windows(&ctx);
        self.splash.ui(&ctx);
    }
}

// ------------------------------------------------------------ helpers

/// "5 min ago" style age of a Unix time.
fn ago(unix: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(unix);
    let s = now.saturating_sub(unix);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

fn status_mark(s: Status) -> (&'static str, Color32) {
    match s {
        Status::Idle => ("○", Color32::GRAY),
        Status::Running => ("◌", Color32::from_rgb(110, 160, 230)),
        Status::Ok => ("✔", Color32::from_rgb(60, 170, 90)),
        Status::Failed => ("✖", Color32::from_rgb(220, 80, 70)),
        Status::Stale => ("✔", Color32::from_rgb(200, 170, 60)),
    }
}

fn id_label(id: &Identity) -> String {
    match id {
        Identity::Board { name, .. } => name.clone(),
        Identity::Cable { names } => format!("cable: {}", names.join(" / ")),
        Identity::Unknown => "unknown".into(),
    }
}

fn fpgaflow_pnr_constraints(p: &Project) -> Vec<PathBuf> {
    p.file
        .target
        .as_ref()
        .map(|t| t.constraints.iter().map(|c| p.root.join(c)).collect())
        .unwrap_or_default()
}

/// Create the project directory, manifest and (with a board) its UCF.
fn create_project(f: &NewProjectForm) -> anyhow::Result<Project> {
    let name = f.name.trim();
    anyhow::ensure!(
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
        "use letters, digits, '_' or '-' in the project name"
    );
    let root = Path::new(f.parent.trim()).join(name);
    std::fs::create_dir_all(&root)?;
    let mut file = ProjectFile::new(name);
    if let Some(b) = f.board.as_deref().and_then(board::board) {
        let ucf_rel = PathBuf::from("constraints").join(b.ucf_file_name());
        file.target = Some(TargetSection {
            board: Some(b.id.clone()),
            family: Some(b.family.clone()),
            device: Some(b.device.clone()),
            package: Some(b.package.clone()),
            speed: Some(b.speed.clone()),
            backend: Some(b.backend.clone()),
            constraints: vec![ucf_rel.clone()],
        });
        let project = Project::create(&root, file)?;
        if let Some(text) = b.ucf_text() {
            std::fs::write(root.join(&ucf_rel), text)?;
        }
        return Ok(project);
    }
    Ok(Project::create(&root, file)?)
}
