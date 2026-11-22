//! Background work for the GUI: flow actions on a tokio runtime, the USB
//! hotplug bridge, the project file watcher and the Doctor scan. Everything
//! reports back as a [`Msg`] on one channel and asks egui to repaint.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use eframe::egui;
use fpgaflow_core::job::JobEvent;
use fpgaflow_core::project::{Project, SimTool};
use fpgaflow_core::stage::Summary;
use fpgaflow_core::tools::{self, HostCheck, ToolStatus};
use fpgaflow_core::UserConfig;
use fpgaflow_flow::boards::{self, Hotplug};
use fpgaflow_flow::{BuildOptions, BuildOutput, Ctx};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Lint,
    Sim(SimTool),
    Synth,
    Build,
    Program,
    Detect,
    /// Write the bitstream to the board's flash.
    ProgramFlash,
    /// Read the connected board's flash to a file (see `spawn_flash_read`).
    ReadFlash,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Lint => "Lint",
            Action::Sim(SimTool::Icarus) => "Simulate (Icarus)",
            Action::Sim(SimTool::Verilator) => "Simulate (Verilator)",
            Action::Synth => "Synthesise",
            Action::Build => "Build",
            Action::Program => "Program",
            Action::Detect => "Detect",
            Action::ProgramFlash => "Program flash",
            Action::ReadFlash => "Read flash",
        }
    }

    /// Flow-bar slot this action reports into (both simulators share one).
    pub fn slot(self) -> Slot {
        match self {
            Action::Lint => Slot::Lint,
            Action::Sim(_) => Slot::Sim,
            Action::Synth => Slot::Synth,
            Action::Build => Slot::Build,
            Action::Program | Action::Detect | Action::ProgramFlash | Action::ReadFlash => {
                Slot::Program
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Slot {
    Lint,
    Sim,
    Synth,
    Build,
    Program,
}

pub struct SynthView {
    pub summary: Summary,
    pub target: String,
    pub netlist: PathBuf,
    pub utilisation: Option<fpgaflow_synth::report::Utilisation>,
}

pub enum Outcome {
    Summary(Summary),
    Synth(SynthView),
    Build(BuildOutput),
}

/// What the Claude tab asked for, before ports are looked up.
#[derive(Debug, Clone)]
pub enum AiRequest {
    NewModule { description: String },
    Testbench { module: String, description: String },
    Constraints { description: String },
    FixErrors { diagnostics: Vec<String> },
    Explain { file: PathBuf, question: String },
}

pub struct AiResult {
    pub outcome: fpgaflow_flow::ai::Outcome,
    pub proposals: Vec<fpgaflow_flow::Proposal>,
}

pub enum Msg {
    Layout(Result<std::sync::Arc<fpgaflow_flow::LayoutData>, String>),
    Pins(Result<std::sync::Arc<fpgaflow_flow::PinData>, String>),
    Ai(fpgaflow_flow::ai::AiEvent),
    AiDone(Result<AiResult, String>),
    Event(JobEvent),
    Done(Action, Result<Outcome, String>),
    Hotplug(Hotplug),
    Doctor(Vec<ToolStatus>, Vec<HostCheck>),
    FsChanged(Vec<PathBuf>),
}

/// Run `action` in the background; its events and result arrive as [`Msg`]s.
pub fn spawn_action(
    rt: &tokio::runtime::Runtime,
    repaint: egui::Context,
    tx: Sender<Msg>,
    project: Project,
    cfg: UserConfig,
    action: Action,
    cancel: CancellationToken,
) {
    rt.spawn(async move {
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel();
        let (fwd_tx, fwd_repaint) = (tx.clone(), repaint.clone());
        let forward = tokio::spawn(async move {
            while let Some(ev) = erx.recv().await {
                if fwd_tx.send(Msg::Event(ev)).is_err() {
                    break;
                }
                fwd_repaint.request_repaint();
            }
        });
        let ctx = Ctx {
            events: etx,
            cancel,
        };
        let p = &project;
        let c = &cfg;
        let result: anyhow::Result<Outcome> = match action {
            Action::Lint => fpgaflow_flow::lint(p, c, &ctx).await.map(Outcome::Summary),
            Action::Sim(tool) => fpgaflow_flow::simulate(p, c, tool, &ctx)
                .await
                .map(Outcome::Summary),
            Action::Synth => fpgaflow_flow::synthesize(p, c, None, None, &ctx)
                .await
                .map(|s| {
                    Outcome::Synth(SynthView {
                        target: format!("{} via {}", s.target.family.name, s.target.backend.name),
                        summary: s.summary,
                        netlist: s.netlist,
                        utilisation: s.utilisation,
                    })
                }),
            Action::Build => fpgaflow_flow::build(p, c, &BuildOptions::default(), &ctx)
                .await
                .map(Outcome::Build),
            Action::Program => fpgaflow_flow::program(p, c, None, &ctx)
                .await
                .map(Outcome::Summary),
            Action::Detect => fpgaflow_flow::detect(p, c, &ctx)
                .await
                .map(Outcome::Summary),
            Action::ProgramFlash => fpgaflow_flow::program_flash(p, c, &ctx)
                .await
                .map(Outcome::Summary),
            Action::ReadFlash => Err(anyhow::anyhow!("use spawn_flash_read")),
        };
        drop(ctx); // closes the event channel so `forward` finishes
        let _ = forward.await;
        let _ = tx.send(Msg::Done(action, result.map_err(|e| format!("{e:#}"))));
        repaint.request_repaint();
    });
}

/// Load the device, package and last placement for the Device and Pins tabs.
/// ISE runs (once per part, and after each build) report into the log.
pub fn spawn_layout(
    rt: &tokio::runtime::Runtime,
    repaint: egui::Context,
    tx: Sender<Msg>,
    project: Project,
    cfg: UserConfig,
) {
    rt.spawn(async move {
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel();
        let (fwd_tx, fwd_repaint) = (tx.clone(), repaint.clone());
        let forward = tokio::spawn(async move {
            while let Some(ev) = erx.recv().await {
                if fwd_tx.send(Msg::Event(ev)).is_err() {
                    break;
                }
                fwd_repaint.request_repaint();
            }
        });
        let ctx = Ctx {
            events: etx,
            cancel: CancellationToken::new(),
        };
        let r = fpgaflow_flow::layout(&project, &cfg, &ctx).await;
        drop(ctx);
        let _ = forward.await;
        let _ = tx.send(Msg::Layout(
            r.map(std::sync::Arc::new).map_err(|e| format!("{e:#}")),
        ));
        repaint.request_repaint();
    });
}

/// Load the package, ports and constraint files for the Pins tab.
pub fn spawn_pins(
    rt: &tokio::runtime::Runtime,
    repaint: egui::Context,
    tx: Sender<Msg>,
    project: Project,
    cfg: UserConfig,
) {
    rt.spawn(async move {
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel();
        let (fwd_tx, fwd_repaint) = (tx.clone(), repaint.clone());
        let forward = tokio::spawn(async move {
            while let Some(ev) = erx.recv().await {
                if fwd_tx.send(Msg::Event(ev)).is_err() {
                    break;
                }
                fwd_repaint.request_repaint();
            }
        });
        let ctx = Ctx {
            events: etx,
            cancel: CancellationToken::new(),
        };
        let r = fpgaflow_flow::pin_data(&project, &cfg, &ctx).await;
        drop(ctx);
        let _ = forward.await;
        let _ = tx.send(Msg::Pins(
            r.map(std::sync::Arc::new).map_err(|e| format!("{e:#}")),
        ));
        repaint.request_repaint();
    });
}

/// Read a board's flash into `out`; reports as `Msg::Done(Action::ReadFlash, ..)`.
/// Needs no project: it reads whichever known board is connected.
pub fn spawn_flash_read(
    rt: &tokio::runtime::Runtime,
    repaint: egui::Context,
    tx: Sender<Msg>,
    board: &'static fpgaflow_core::board::Board,
    out: PathBuf,
    cfg: UserConfig,
    cancel: CancellationToken,
) {
    rt.spawn(async move {
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel();
        let (fwd_tx, fwd_repaint) = (tx.clone(), repaint.clone());
        let forward = tokio::spawn(async move {
            while let Some(ev) = erx.recv().await {
                if fwd_tx.send(Msg::Event(ev)).is_err() {
                    break;
                }
                fwd_repaint.request_repaint();
            }
        });
        let ctx = Ctx {
            events: etx,
            cancel,
        };
        let r = fpgaflow_flow::read_flash(&cfg, board, &out, &ctx).await;
        drop(ctx);
        let _ = forward.await;
        let _ = tx.send(Msg::Done(
            Action::ReadFlash,
            r.map(Outcome::Summary).map_err(|e| format!("{e:#}")),
        ));
        repaint.request_repaint();
    });
}

/// Scan tools and host setup off the UI thread (version probes take a moment).
pub fn spawn_doctor(repaint: egui::Context, tx: Sender<Msg>, cfg: UserConfig) {
    std::thread::spawn(move || {
        let t = tools::discover(&cfg);
        let h = tools::host_checks();
        let _ = tx.send(Msg::Doctor(t, h));
        repaint.request_repaint();
    });
}

/// Start the USB hotplug watcher; returns the devices present now.
pub fn start_hotplug(
    repaint: egui::Context,
    tx: Sender<Msg>,
) -> std::io::Result<(Vec<boards::UsbDevice>, boards::Watcher)> {
    let present = boards::scan()?;
    let (htx, hrx) = std::sync::mpsc::channel();
    let watcher = boards::watch(&present, htx)?;
    std::thread::Builder::new()
        .name("hotplug-bridge".into())
        .spawn(move || {
            for ev in hrx {
                if tx.send(Msg::Hotplug(ev)).is_err() {
                    break;
                }
                repaint.request_repaint();
            }
        })?;
    Ok((present, watcher))
}

/// Watch the project directory (except `build/`) for edits made in the
/// external editor.
pub fn watch_project(
    root: &Path,
    repaint: egui::Context,
    tx: Sender<Msg>,
) -> notify::Result<notify::RecommendedWatcher> {
    use notify::Watcher;
    let build = root.join("build");
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if !(ev.kind.is_modify() || ev.kind.is_create() || ev.kind.is_remove()) {
            return;
        }
        let paths: Vec<PathBuf> = ev
            .paths
            .into_iter()
            .filter(|p| !p.starts_with(&build))
            .collect();
        if !paths.is_empty() && tx.send(Msg::FsChanged(paths)).is_ok() {
            repaint.request_repaint();
        }
    })?;
    w.watch(root, notify::RecursiveMode::Recursive)?;
    Ok(w)
}

/// Ask Claude in the background; progress arrives as `Msg::Ai`, the
/// checked result as `Msg::AiDone`.
pub fn spawn_ai(
    rt: &tokio::runtime::Runtime,
    repaint: egui::Context,
    tx: Sender<Msg>,
    project: Project,
    cfg: UserConfig,
    req: AiRequest,
    cancel: CancellationToken,
) {
    use fpgaflow_flow::ai::Task;
    rt.spawn(async move {
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel();
        let (fwd_tx, fwd_repaint) = (tx.clone(), repaint.clone());
        let forward = tokio::spawn(async move {
            while let Some(ev) = erx.recv().await {
                if fwd_tx.send(Msg::Ai(ev)).is_err() {
                    break;
                }
                fwd_repaint.request_repaint();
            }
        });
        // `async move` owns the event sender, so it closes (and `forward`
        // finishes) as soon as the request is done.
        let result: anyhow::Result<AiResult> = async move {
            // Port lists come from Yosys (a blocking subprocess).
            let (p2, c2) = (project.clone(), cfg.clone());
            let task = tokio::task::spawn_blocking(move || -> anyhow::Result<Task> {
                Ok(match req {
                    AiRequest::NewModule { description } => Task::NewModule { description },
                    AiRequest::Testbench {
                        module,
                        description,
                    } => Task::Testbench {
                        ports: fpgaflow_flow::ports_text(&p2, &c2, Some(&module))?,
                        module,
                        description,
                    },
                    AiRequest::Constraints { description } => Task::Constraints {
                        ports: fpgaflow_flow::ports_text(&p2, &c2, None)?,
                        description,
                    },
                    AiRequest::FixErrors { diagnostics } => Task::FixErrors { diagnostics },
                    AiRequest::Explain { file, question } => Task::Explain { file, question },
                })
            })
            .await??;
            let outcome = fpgaflow_flow::ai_generate(&project, &cfg, &task, &etx, &cancel).await?;
            let proposals = fpgaflow_flow::proposals(&project, &outcome.generated)?;
            Ok(AiResult { outcome, proposals })
        }
        .await;
        let _ = forward.await;
        let _ = tx.send(Msg::AiDone(result.map_err(|e| format!("{e:#}"))));
        repaint.request_repaint();
    });
}
