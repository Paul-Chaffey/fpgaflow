//! Flow orchestration shared by the CLI and the GUI.
//!
//! Each function runs one user-level action (lint, simulate, synthesise,
//! build, program, ...) and streams everything it does to `events`:
//! [`JobEvent::Note`] for the flow's own progress, the job runner's events
//! for tool output. Cancelling `cancel` stops the running tool.
//!
//! Errors (`Err`) mean the action could not start or was refused for safety
//! (missing tool, unconstrained pin, ...). A stage that ran and failed is a
//! normal result with `ok == false`, so callers still get its diagnostics.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fpgaflow_core::job::JobEvent;
use fpgaflow_core::project::{Project, SimTool};
use fpgaflow_core::stage::{self, Plan, Summary};
use fpgaflow_core::{tools, UserConfig};
use fpgaflow_hdl::design::{self, AnalyseRequest, Design};
use fpgaflow_pnr::ise::{self, PadRow, Resource, Timing};
use fpgaflow_pnr::{constraints, PnrError};
use fpgaflow_sim::gtkwave;
use fpgaflow_synth::report::Utilisation;
use fpgaflow_synth::Target;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

pub use fpgaflow_boards as boards;

/// Where events go and how to cancel; passed to every action.
#[derive(Clone)]
pub struct Ctx {
    pub events: UnboundedSender<JobEvent>,
    pub cancel: CancellationToken,
}

impl Ctx {
    pub fn note(&self, text: impl Into<String>) {
        let _ = self.events.send(JobEvent::Note { text: text.into() });
    }

    async fn run(&self, plan: &Plan) -> Result<Summary> {
        Ok(stage::execute(plan, &self.events, &self.cancel).await?)
    }
}

pub fn rel<'a>(project: &Project, p: &'a Path) -> &'a Path {
    p.strip_prefix(&project.root).unwrap_or(p)
}

// ------------------------------------------------------------------ lint & sim

pub async fn lint(project: &Project, cfg: &UserConfig, ctx: &Ctx) -> Result<Summary> {
    let plan = fpgaflow_sim::plan_lint(project, cfg)?;
    ctx.run(&plan).await
}

pub async fn simulate(
    project: &Project,
    cfg: &UserConfig,
    tool: SimTool,
    ctx: &Ctx,
) -> Result<Summary> {
    let plan = fpgaflow_sim::plan_sim(project, cfg, tool)?;
    ctx.note(format!("simulating with {tool:?}"));
    ctx.run(&plan).await
}

/// Waveform file the last simulation with `tool` writes.
pub fn wave_path(project: &Project, tool: SimTool) -> Result<PathBuf> {
    let top = fpgaflow_sim::tb_top(project)?;
    Ok(fpgaflow_sim::sim_workdir(project, tool)
        .join(format!("{top}.{}", project.file.sim.wave.extension())))
}

/// Open the last waveform in GTKWave; returns the layout file used.
pub fn open_waves(project: &Project, cfg: &UserConfig, tool: SimTool) -> Result<PathBuf> {
    let wave = wave_path(project, tool)?;
    anyhow::ensure!(
        wave.is_file(),
        "no waveform at {}; run a simulation first",
        wave.display()
    );
    let gtk = tools::resolve("gtkwave", cfg).context("gtkwave is not installed (see Doctor)")?;
    let top = fpgaflow_sim::tb_top(project)?;
    let layout = gtkwave::ensure_layout(project, &top, tool, &wave)?;
    gtkwave::open(&gtk, &wave, Some(&layout))?;
    Ok(layout)
}

// ------------------------------------------------------------------ analysis

/// Elaborate the RTL with Yosys (hierarchy and ports).
pub fn analyse(project: &Project, cfg: &UserConfig, top: Option<String>) -> Result<Design> {
    let yosys = tools::resolve("yosys", cfg).context("yosys is not installed (see Doctor)")?;
    let req = AnalyseRequest {
        files: project.rtl_files()?,
        include_dirs: project.include_dirs(),
        defines: project.file.sources.defines.clone(),
        top: top.or_else(|| project.file.project.top.clone()),
        slang: false,
    };
    match design::analyse(&yosys, &req, &project.stage_dir("analyse")) {
        Ok(d) => Ok(d),
        Err(design::DesignError::Yosys { diagnostics, .. }) => {
            let first: Vec<String> = diagnostics.iter().take(3).map(|d| d.to_string()).collect();
            anyhow::bail!("Yosys could not elaborate the design: {}", first.join("; "))
        }
        Err(e) => Err(e.into()),
    }
}

// ------------------------------------------------------------------ synthesis

pub struct SynthOutput {
    pub summary: Summary,
    pub target: Target,
    pub netlist: PathBuf,
    pub log: PathBuf,
    pub utilisation: Option<Utilisation>,
}

pub async fn synthesize(
    project: &Project,
    cfg: &UserConfig,
    family: Option<&str>,
    backend: Option<&str>,
    ctx: &Ctx,
) -> Result<SynthOutput> {
    let target = fpgaflow_synth::resolve_target(project, family, backend)?;
    ctx.note(format!(
        "synthesis: {} ({}) for {} -> {} netlist",
        target.family.name,
        target.family.id,
        target.backend.name,
        target.backend.netlist.extension()
    ));
    if let Some(why) = &target.family.experimental {
        ctx.note(format!("note: {why}; check the result carefully"));
    }
    let sp = fpgaflow_synth::plan_synth(project, cfg, target)?;
    let summary = ctx.run(&sp.plan).await?;
    let utilisation = if summary.ok {
        Some(Utilisation::from_stat_json(&std::fs::read_to_string(
            &sp.stat,
        )?)?)
    } else {
        None
    };
    Ok(SynthOutput {
        summary,
        target,
        netlist: sp.netlist,
        log: sp.log,
        utilisation,
    })
}

// ------------------------------------------------------------------ build

#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    pub family: Option<String>,
    pub backend: Option<String>,
    /// Build even with tool-chosen pins. Unsafe on real hardware.
    pub allow_unconstrained: bool,
}

#[derive(Debug, Default)]
pub struct BuildOutput {
    /// Every stage that ran, in order.
    pub stages: Vec<Summary>,
    pub ok: bool,
    /// Why the build stopped, if it did.
    pub failure: Option<String>,
    pub part: Option<String>,
    pub utilisation: Option<Utilisation>,
    pub resources: Vec<Resource>,
    pub timing: Option<Timing>,
    pub pins: Vec<PadRow>,
    pub bitstream: Option<PathBuf>,
}

impl BuildOutput {
    fn fail(mut self, why: impl Into<String>) -> Self {
        self.ok = false;
        self.failure = Some(why.into());
        self
    }
}

/// Port bits of the design's top with no `LOC` in the project's UCFs.
pub fn unconstrained_ports(project: &Project, design: &Design) -> Result<(String, Vec<String>)> {
    let top = design.top().context("Yosys found no top module")?.clone();
    let mut located = BTreeSet::new();
    for f in fpgaflow_pnr::constraint_files(project) {
        let text =
            std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?;
        located.extend(constraints::ucf_located_nets(&text));
    }
    Ok((
        top.name.clone(),
        constraints::unconstrained(&top.ports, &located),
    ))
}

/// Synthesis → place & route → timing → bitstream, with the pin safety
/// checks before synthesis and before bitgen.
pub async fn build(
    project: &Project,
    cfg: &UserConfig,
    opts: &BuildOptions,
    ctx: &Ctx,
) -> Result<BuildOutput> {
    let target =
        fpgaflow_synth::resolve_target(project, opts.family.as_deref(), opts.backend.as_deref())?;
    if target.backend.id != "ise" {
        return Err(PnrError::NotImplemented(target.backend.id.clone()).into());
    }

    // 1. No signal on a pin the board reserves (e.g. wired to a supply), and
    //    every top-level port bit needs a LOC, before anything is built.
    fpgaflow_pnr::check_reserved_pins(project)?;
    let design = analyse(project, cfg, None)?;
    let (top, missing) = unconstrained_ports(project, &design)?;
    if !missing.is_empty() {
        let err = PnrError::Unconstrained(missing);
        if !opts.allow_unconstrained {
            return Err(err.into());
        }
        ctx.note(format!("warning: {err}"));
    }

    let mut out = BuildOutput::default();

    // 2. Synthesis.
    let s = synthesize(
        project,
        cfg,
        opts.family.as_deref(),
        opts.backend.as_deref(),
        ctx,
    )
    .await?;
    out.stages.push(s.summary.clone());
    out.utilisation = s.utilisation.clone();
    if !s.summary.ok {
        return Ok(out.fail(format!(
            "synthesis failed (log: {})",
            rel(project, &s.log).display()
        )));
    }

    // 3. Translate, map, place & route, timing.
    let ip = ise::plan_implement(project, cfg, &s.netlist, &top)?;
    out.part = Some(ip.part.clone());
    ctx.note(format!("implementation: {top} on {}", ip.part));
    let mut summary = ctx.run(&ip.plan).await?;
    ip.remap(&mut summary.diagnostics);
    let impl_ok = summary.ok;
    out.stages.push(summary);
    if !impl_ok {
        return Ok(out.fail("implementation failed"));
    }
    out.resources = ise::parse_map_report(&std::fs::read_to_string(&ip.files.mrp)?);
    out.timing = Some(ise::parse_timing_report(&std::fs::read_to_string(
        &ip.files.twr,
    )?));
    out.pins = ise::parse_pad_report(&std::fs::read_to_string(&ip.files.pad)?);

    // 4. No bitstream with tool-chosen pins.
    let auto: Vec<String> = ise::unlocated(&out.pins)
        .iter()
        .map(|r| format!("{}@{}", r.signal, r.pin))
        .collect();
    if !auto.is_empty() && !opts.allow_unconstrained {
        return Ok(out.fail(format!(
            "ISE placed these pins itself (UNLOCATED in the pad report): {}; no bitstream written",
            auto.join(", ")
        )));
    }

    // 5. Bitstream.
    let mode = project.file.program.mode;
    ctx.note(format!("bitstream ({mode:?} load)"));
    let board_bitgen = project
        .file
        .target
        .as_ref()
        .and_then(|t| t.board.as_deref())
        .and_then(fpgaflow_core::board::board)
        .map(|b| b.bitgen.clone())
        .unwrap_or_default();
    let summary = ctx
        .run(&ise::plan_bitstream(&ip, mode, &board_bitgen))
        .await?;
    let bit_ok = summary.ok;
    out.stages.push(summary);
    if !bit_ok {
        return Ok(out.fail("bitgen failed"));
    }
    out.bitstream = Some(ip.files.bit.clone());
    if out
        .timing
        .as_ref()
        .and_then(|t| t.errors)
        .is_some_and(|e| e > 0)
    {
        return Ok(out.fail("timing not met"));
    }
    out.ok = true;
    Ok(out)
}

// ------------------------------------------------------------------ program

pub async fn program(
    project: &Project,
    cfg: &UserConfig,
    bit: Option<&Path>,
    ctx: &Ctx,
) -> Result<Summary> {
    let board = fpgaflow_program::project_board(project)?;
    let bit = bit
        .map(Path::to_path_buf)
        .unwrap_or_else(|| fpgaflow_program::default_bitstream(project));
    ctx.note(format!(
        "program: {} -> {} (SRAM, volatile)",
        rel(project, &bit).display(),
        board.name
    ));
    let plan = fpgaflow_program::plan_program(project, cfg, &bit)?;
    ctx.run(&plan).await
}

pub async fn detect(project: &Project, cfg: &UserConfig, ctx: &Ctx) -> Result<Summary> {
    let board = fpgaflow_program::project_board(project)?;
    let plan = fpgaflow_program::plan_detect(board, cfg, &project.stage_dir("program"))?;
    ctx.run(&plan).await
}

/// openFPGALoader's cable table (empty if it isn't installed).
pub fn cables(cfg: &UserConfig) -> Vec<boards::Cable> {
    tools::resolve("openFPGALoader", cfg)
        .map(|o| boards::load_cable_list(&o))
        .unwrap_or_default()
}

// ------------------------------------------------------------------ layout

pub use fpgaflow_layout as layout_model;
use fpgaflow_layout::{Device, Package, Placed};

/// Everything the layout tools draw, for one project.
pub struct LayoutData {
    /// Full part, e.g. `xc3s1500-4-fg320`.
    pub part: String,
    pub device: Device,
    pub package: Package,
    /// The last build's placement and routing, if there has been a build.
    pub placed: Option<Placed>,
    /// Detailed timing paths from the last build, worst first.
    pub paths: Vec<ise::TimingPath>,
    /// The `.xdl` the placement was read from.
    pub xdl: Option<PathBuf>,
}

/// Where device and package data are cached, one set per part.
pub fn device_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".cache"))
        .join("fpgaflow")
        .join("devices")
}

fn newer(a: &Path, b: &Path) -> bool {
    let m = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (m(a), m(b)) {
        (Some(a), Some(b)) => a > b,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Load (generating with ISE where needed) the device, package and the last
/// build's placement for the project's target. ISE designs only for now.
pub async fn layout(project: &Project, cfg: &UserConfig, ctx: &Ctx) -> Result<LayoutData> {
    let t = project
        .file
        .target
        .as_ref()
        .context("fpgaflow.toml has no [target]")?;
    let backend = t.backend.as_deref().unwrap_or("");
    if backend != "ise" {
        anyhow::bail!(
            "the layout tools support the ISE backend only so far (target backend: {backend:?})"
        );
    }
    let part = fpgaflow_pnr::part_name(t)?;
    let devpkg = format!(
        "{}{}",
        t.device.as_deref().unwrap_or_default(),
        t.package.as_deref().unwrap_or_default()
    );
    let dir = device_cache_dir();
    std::fs::create_dir_all(&dir)?;

    let xdlrc = dir.join(format!("{devpkg}.xdlrc"));
    if !xdlrc.is_file() {
        ctx.note(format!(
            "reading the {devpkg} device from ISE (once per part)"
        ));
        let s = ctx
            .run(&ise::plan_device_report(cfg, &devpkg, &xdlrc)?)
            .await?;
        if !s.ok || !xdlrc.is_file() {
            anyhow::bail!("xdl -report {devpkg} failed");
        }
    }
    let pkg = dir.join(format!("{devpkg}.pkg"));
    if !pkg.is_file() {
        // partgen also writes partlist files; keep them out of the cache.
        let tmp = dir.join(format!("partgen-{devpkg}"));
        std::fs::create_dir_all(&tmp)?;
        let s = ctx.run(&ise::plan_package(cfg, &devpkg, &tmp)?).await?;
        let made = tmp.join(format!("{devpkg}.pkg"));
        if !s.ok || !made.is_file() {
            anyhow::bail!("partgen -v {devpkg} failed");
        }
        std::fs::rename(&made, &pkg)?;
        let _ = std::fs::remove_dir_all(&tmp);
    }
    let device = Device::parse(&std::fs::read_to_string(&xdlrc)?);
    let package = Package::parse(&std::fs::read_to_string(&pkg)?);

    let bit = fpgaflow_program::default_bitstream(project);
    let ncd = bit.with_extension("ncd");
    let twr = bit.with_extension("twr");
    let (mut placed, mut xdl_used) = (None, None);
    if ncd.is_file() {
        let xdl = bit.with_extension("xdl");
        if !xdl.is_file() || newer(&ncd, &xdl) {
            let s = ctx.run(&ise::plan_xdl(cfg, &ncd, &xdl)?).await?;
            if !s.ok {
                anyhow::bail!("xdl -ncd2xdl failed");
            }
        }
        placed = Some(Placed::parse(&std::fs::read_to_string(&xdl)?));
        xdl_used = Some(xdl);
    }
    let paths = std::fs::read_to_string(&twr)
        .map(|t| ise::parse_timing_paths(&t))
        .unwrap_or_default();
    Ok(LayoutData {
        part,
        device,
        package,
        placed,
        paths,
        xdl: xdl_used,
    })
}

// ------------------------------------------------------------------ Claude

pub use fpgaflow_ai as ai;
use fpgaflow_ai::{AiEvent, GenFile, Generated};

/// One proposed file, ready for review.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub file: GenFile,
    /// Absolute destination in the project.
    pub path: PathBuf,
    pub existed: bool,
    /// Unified diff against the current file (or the whole file if new).
    pub diff: String,
}

pub fn proposals(project: &Project, g: &Generated) -> Result<Vec<Proposal>> {
    g.files
        .iter()
        .map(|f| {
            let rel = ai::validate_path(&f.path)?;
            let path = project.root.join(&rel);
            let old = std::fs::read_to_string(&path).ok();
            let existed = old.is_some();
            let old = old.unwrap_or_default();
            let td = similar::TextDiff::from_lines(old.as_str(), f.content.as_str());
            let mut ud = td.unified_diff();
            let a = if existed {
                format!("a/{}", f.path)
            } else {
                "/dev/null".to_string()
            };
            ud.context_radius(3).header(&a, &format!("b/{}", f.path));
            Ok(Proposal {
                file: f.clone(),
                path,
                existed,
                diff: ud.to_string(),
            })
        })
        .collect()
}

/// Write accepted proposals. A new UCF is also added to `[target] constraints`.
pub fn apply(project: &Project, accepted: &[&Proposal]) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let mut manifest = project.clone();
    let mut manifest_changed = false;
    for p in accepted {
        if let Some(dir) = p.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&p.path, &p.file.content)
            .with_context(|| format!("writing {}", p.path.display()))?;
        written.push(p.path.clone());
        if p.path.extension().is_some_and(|e| e == "ucf") {
            let rel = PathBuf::from(&p.file.path);
            if let Some(t) = manifest.file.target.as_mut() {
                if !t.constraints.contains(&rel) {
                    t.constraints.push(rel);
                    manifest_changed = true;
                }
            }
        }
    }
    if manifest_changed {
        manifest.save()?;
    }
    Ok(written)
}

/// Port list of `module` (or the top), as text for a prompt.
pub fn ports_text(project: &Project, cfg: &UserConfig, module: Option<&str>) -> Result<String> {
    let d = analyse(project, cfg, module.map(String::from))?;
    let m = match module {
        Some(name) => d
            .module(name)
            .with_context(|| format!("module {name} not found"))?,
        None => d.top().context("no top module")?,
    };
    Ok(m.ports
        .iter()
        .map(|p| {
            let dir = format!("{:?}", p.direction).to_lowercase();
            if p.width > 1 {
                format!("{dir} [{}:0] {}", p.width - 1, p.name)
            } else {
                format!("{dir} {}", p.name)
            }
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Ask Claude, check the answer, retry with errors; returns the outcome
/// (the caller reviews and [`apply`]s it).
pub async fn ai_generate(
    project: &Project,
    cfg: &UserConfig,
    task: &ai::Task,
    events: &UnboundedSender<AiEvent>,
    cancel: &CancellationToken,
) -> Result<ai::Outcome> {
    Ok(ai::generate(cfg, project, task, events, cancel).await?)
}
