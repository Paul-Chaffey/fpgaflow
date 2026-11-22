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
        }
    }

    /// Flow-bar slot this action reports into (both simulators share one).
    pub fn slot(self) -> Slot {
        match self {
            Action::Lint => Slot::Lint,
            Action::Sim(_) => Slot::Sim,
            Action::Synth => Slot::Synth,
            Action::Build => Slot::Build,
            Action::Program | Action::Detect => Slot::Program,
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

pub enum Msg {
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
        };
        drop(ctx); // closes the event channel so `forward` finishes
        let _ = forward.await;
        let _ = tx.send(Msg::Done(action, result.map_err(|e| format!("{e:#}"))));
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
