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

use crate::worker::{self, Action, Msg, Outcome, Slot, SynthView};

const MAX_LOG_LINES: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Log,
    Problems,
    Reports,
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
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, start_dir: Option<PathBuf>) -> Self {
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
        app
    }

    // ------------------------------------------------------------ state

    fn set_project(&mut self, p: Project) {
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
                        parent: dirs_home().display().to_string(),
                        ..Default::default()
                    };
                    ui.close();
                }
                if ui.button("Open project…").clicked() {
                    ui.close();
                    if let Some(dir) = rfd::FileDialog::new()
                        .set_title("Open fpgaflow project")
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
            let prog_status = self.status.get(&Slot::Program).copied();
            let (mark, color) = status_mark(prog_status.unwrap_or(Status::Idle));
            if ui
                .add_enabled(
                    can && connected,
                    egui::Button::new(RichText::new(format!("{mark} Program")).color(color)),
                )
                .on_disabled_hover_text("Needs a project board that is connected")
                .clicked()
            {
                clicked = Some(Action::Program);
            }
            if ui
                .add_enabled(can && connected, egui::Button::new("Detect"))
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
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    match Project::find(&dir) {
                        Ok(p) => self.set_project(p),
                        Err(e) => self.message = Some((false, e.to_string())),
                    }
                }
            }
            if ui.button("New project…").clicked() {
                self.new_project = NewProjectForm {
                    open: true,
                    parent: dirs_home().display().to_string(),
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
                (Tab::Problems, format!("Problems ({errors}E/{warns}W)")),
                (Tab::Reports, "Reports".to_string()),
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
            Tab::Problems => self.problems_view(ui),
            Tab::Reports => self.reports_view(ui),
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
        ui.add_space(8.0);
        ui.small("Tool search paths and per-tool overrides are in config.toml.");
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
    }
}

// ------------------------------------------------------------ helpers

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

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
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
