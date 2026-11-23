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
use fpgaflow_core::status::{self, Step};
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

/// Remember a step's result in `build/status.json` (see [`status`]).
fn remember<T>(
    project: &Project,
    step: Step,
    r: &Result<T>,
    ok: impl Fn(&T) -> (bool, Option<String>),
) {
    let (good, detail) = match r {
        Ok(v) => ok(v),
        Err(e) => (false, Some(format!("{e:#}").chars().take(120).collect())),
    };
    status::record(project, step, good, detail);
}

pub async fn lint(project: &Project, cfg: &UserConfig, ctx: &Ctx) -> Result<Summary> {
    let plan = fpgaflow_sim::plan_lint(project, cfg)?;
    let r = ctx.run(&plan).await;
    remember(project, Step::Lint, &r, |s| (s.ok, None));
    r
}

pub async fn simulate(
    project: &Project,
    cfg: &UserConfig,
    tool: SimTool,
    ctx: &Ctx,
) -> Result<Summary> {
    let plan = fpgaflow_sim::plan_sim(project, cfg, tool)?;
    ctx.note(format!("simulating with {tool:?}"));
    let r = ctx.run(&plan).await;
    remember(project, Step::Sim, &r, |s| {
        let verdict = match s.verdict {
            Some(stage::Verdict::Pass) => "PASS",
            Some(stage::Verdict::Fail) => "FAIL",
            _ => "no verdict",
        };
        let tool = match tool {
            SimTool::Icarus => "icarus",
            SimTool::Verilator => "verilator",
        };
        (s.ok, Some(format!("{tool}: {verdict}")))
    });
    r
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
        libs: primitive_libs(project),
    };
    match design::analyse(&yosys, &req, &project.stage_dir("analyse")) {
        Ok(d) => Ok(d),
        Err(design::DesignError::Yosys { diagnostics, log }) => {
            // Errors first; Yosys's own "ERROR:" line carries no file:line.
            let mut first: Vec<String> = diagnostics
                .iter()
                .filter(|d| d.severity == fpgaflow_core::diag::Severity::Error)
                .take(3)
                .map(|d| d.to_string())
                .collect();
            if first.is_empty() {
                first.extend(
                    log.lines()
                        .filter(|l| l.starts_with("ERROR:"))
                        .take(1)
                        .map(|l| l.trim_start_matches("ERROR:").trim().to_string()),
                );
            }
            if first.is_empty() {
                first = diagnostics.iter().take(3).map(|d| d.to_string()).collect();
            }
            anyhow::bail!("Yosys could not elaborate the design: {}", first.join("; "))
        }
        Err(e) => Err(e.into()),
    }
}

/// Vendor primitive libraries for the project's family, read as black boxes
/// during analysis (synthesis reads its own).
fn primitive_libs(project: &Project) -> Vec<String> {
    project
        .file
        .target
        .as_ref()
        .and_then(|t| t.family.as_deref())
        .and_then(fpgaflow_core::family::family)
        .map(|f| f.primitive_libs().iter().map(|l| l.to_string()).collect())
        .unwrap_or_default()
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
    let r = synthesize_inner(project, cfg, family, backend, ctx).await;
    remember(project, Step::Synth, &r, |s| (s.summary.ok, None));
    r
}

async fn synthesize_inner(
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

/// The last build's reports, read back from disk (for the Reports tab after
/// a restart). `None` if no build has been recorded.
pub fn last_build(project: &Project) -> Option<BuildOutput> {
    let rec = status::load(project).remove(&Step::Build)?;
    let bit = fpgaflow_program::default_bitstream(project);
    let read = |ext: &str| std::fs::read_to_string(bit.with_extension(ext)).ok();
    let mrp = std::fs::read_to_string(
        bit.with_file_name(format!("{}_map.mrp", bit.file_stem()?.to_string_lossy())),
    )
    .ok();
    let utilisation = std::fs::read_to_string(project.stage_dir("synth").join("stat.json"))
        .ok()
        .and_then(|t| Utilisation::from_stat_json(&t).ok());
    Some(BuildOutput {
        stages: Vec::new(),
        ok: rec.ok,
        failure: (!rec.ok).then(|| rec.detail.clone().unwrap_or_else(|| "failed".into())),
        part: project
            .file
            .target
            .as_ref()
            .and_then(|t| fpgaflow_pnr::part_name(t).ok()),
        utilisation,
        resources: mrp.map(|t| ise::parse_map_report(&t)).unwrap_or_default(),
        timing: read("twr").map(|t| ise::parse_timing_report(&t)),
        pins: read("pad")
            .map(|t| ise::parse_pad_report(&t))
            .unwrap_or_default(),
        bitstream: bit.is_file().then_some(bit),
    })
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
    let r = build_inner(project, cfg, opts, ctx).await;
    remember(project, Step::Build, &r, |b| {
        let detail = match (&b.failure, b.timing.as_ref().and_then(|t| t.fmax_mhz)) {
            (Some(why), _) => Some(why.clone()),
            (None, Some(f)) => Some(format!("Fmax {f:.1} MHz")),
            _ => None,
        };
        (b.ok, detail)
    });
    r
}

async fn build_inner(
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
    let r = ctx.run(&plan).await;
    remember(project, Step::Program, &r, |s| (s.ok, None));
    r
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

// ------------------------------------------------------------------ flash

pub use fpgaflow_program::flash_name;

/// The known board that is plugged in now (the project's own board first).
pub fn connected_board(project: Option<&Project>) -> Option<&'static fpgaflow_core::board::Board> {
    let present = boards::scan().unwrap_or_default();
    let plugged =
        |b: &fpgaflow_core::board::Board| present.iter().any(|d| b.matches_usb(d.vid, d.pid));
    project
        .and_then(fpgaflow_pnr::project_board)
        .filter(|b| plugged(b))
        .or_else(|| fpgaflow_core::board::boards().iter().find(|b| plugged(b)))
}

/// Read the whole flash of `board` into `out` (read-only).
pub async fn read_flash(
    cfg: &UserConfig,
    board: &fpgaflow_core::board::Board,
    out: &Path,
    ctx: &Ctx,
) -> Result<Summary> {
    let flash = fpgaflow_program::flash_name(board)
        .with_context(|| format!("fpgaflow cannot read the flash on the {}", board.name))?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Run records go to a scratch directory, not next to the image.
    let workdir = device_cache_dir().with_file_name("flash");
    std::fs::create_dir_all(&workdir)?;
    ctx.note(format!(
        "reading the {} on the {} into {}",
        flash,
        board.name,
        out.display()
    ));
    let s = ctx
        .run(&fpgaflow_program::plan_flash_read(
            board, cfg, out, &workdir,
        )?)
        .await?;
    if s.ok && !out.is_file() {
        anyhow::bail!(
            "the flash read finished but {} was not written",
            out.display()
        );
    }
    Ok(s)
}

/// Write the project's bitstream to its board's flash so it boots from it.
/// For boards whose SRAM bitstreams start up on the JTAG clock, a CCLK
/// copy (`<top>_flash.bit`) is made from the routed design first.
pub async fn program_flash(project: &Project, cfg: &UserConfig, ctx: &Ctx) -> Result<Summary> {
    let board = fpgaflow_program::project_board(project)?;
    let flash = fpgaflow_program::flash_name(board)
        .with_context(|| format!("fpgaflow cannot write the flash on the {}", board.name))?;
    let bit = fpgaflow_program::default_bitstream(project);
    anyhow::ensure!(
        bit.is_file(),
        "no bitstream at {}; run Build first",
        rel(project, &bit).display()
    );
    let cclk = board.bitgen.iter().any(|g| g == "StartupClk:Cclk");
    let image = if cclk {
        bit
    } else {
        let ncd = bit.with_extension("ncd");
        let stem = bit
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let out = bit.with_file_name(format!("{stem}_flash.bit"));
        ctx.note("making a CCLK start-up copy of the bitstream for booting from flash");
        let s = ctx
            .run(&ise::plan_flash_bitstream(cfg, &ncd, &out, &board.bitgen)?)
            .await?;
        anyhow::ensure!(s.ok && out.is_file(), "bitgen for the flash image failed");
        out
    };
    ctx.note(format!(
        "writing {} to the {} on the {}",
        rel(project, &image).display(),
        flash,
        board.name
    ));
    let workdir = project.stage_dir("program");
    let r = ctx
        .run(&fpgaflow_program::plan_flash_write(
            board, cfg, &image, &workdir,
        )?)
        .await;
    remember(project, Step::Program, &r, |s| (s.ok, Some("flash".into())));
    r
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
    /// Floorplan regions from `constraints/floorplan.ucf`.
    pub regions: Vec<fpgaflow_layout::floorplan::Region>,
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

/// The target's full part (`xc3s1500-4-fg320`) and device + package
/// (`xc3s1500fg320`); ISE targets only for now.
fn ise_part(project: &Project) -> Result<(String, String)> {
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
    Ok((part, devpkg))
}

/// The package file for `devpkg`, generated with partgen once per part.
async fn package_file(cfg: &UserConfig, devpkg: &str, ctx: &Ctx) -> Result<PathBuf> {
    let dir = device_cache_dir();
    std::fs::create_dir_all(&dir)?;
    let pkg = dir.join(format!("{devpkg}.pkg"));
    if !pkg.is_file() {
        // partgen also writes partlist files; keep them out of the cache.
        let tmp = dir.join(format!("partgen-{devpkg}"));
        std::fs::create_dir_all(&tmp)?;
        let s = ctx.run(&ise::plan_package(cfg, devpkg, &tmp)?).await?;
        let made = tmp.join(format!("{devpkg}.pkg"));
        if !s.ok || !made.is_file() {
            anyhow::bail!("partgen -v {devpkg} failed");
        }
        std::fs::rename(&made, &pkg)?;
        let _ = std::fs::remove_dir_all(&tmp);
    }
    Ok(pkg)
}

/// Load (generating with ISE where needed) the device, package and the last
/// build's placement for the project's target. ISE designs only for now.
pub async fn layout(project: &Project, cfg: &UserConfig, ctx: &Ctx) -> Result<LayoutData> {
    let (part, devpkg) = ise_part(project)?;
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
    let pkg = package_file(cfg, &devpkg, ctx).await?;
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
    let regions = std::fs::read_to_string(project.root.join(fpgaflow_layout::floorplan::FILE))
        .map(|t| fpgaflow_layout::floorplan::parse(&t))
        .unwrap_or_default();
    Ok(LayoutData {
        part,
        device,
        package,
        placed,
        paths,
        xdl: xdl_used,
        regions,
    })
}

/// Write the floorplan file for `regions` and make sure the project lists
/// it in `[target] constraints`. Takes effect at the next build.
pub fn save_floorplan(
    project: &Project,
    device: &Device,
    regions: &[fpgaflow_layout::floorplan::Region],
) -> Result<PathBuf> {
    use fpgaflow_layout::floorplan;
    let path = project.root.join(floorplan::FILE);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, floorplan::render(regions, device))?;
    add_constraint_file(project, floorplan::FILE)?;
    Ok(path)
}

/// What the pin planner works with.
pub struct PinData {
    pub part: String,
    pub package: Package,
    /// Top-level port bits, named as a UCF names them (`led<0>`).
    pub ports: Vec<fpgaflow_layout::pins::PortBit>,
    pub board: Option<&'static fpgaflow_core::board::Board>,
    /// Board net names by ball (`D2` -> `LED<0>`) from its master UCF.
    pub labels: std::collections::BTreeMap<String, String>,
    /// The project's constraint files.
    pub files: Vec<PathBuf>,
    /// Where new assignments go: the first constraint file.
    pub new_file: PathBuf,
}

impl PinData {
    pub fn assignments(&self) -> Vec<fpgaflow_layout::pins::Assignment> {
        use fpgaflow_layout::pins::{read_assignments, read_files};
        read_assignments(&read_files(&self.files))
    }
    pub fn issues(
        &self,
        assigned: &[fpgaflow_layout::pins::Assignment],
    ) -> Vec<fpgaflow_layout::pins::Issue> {
        fpgaflow_layout::pins::check(&self.ports, assigned, &self.package, self.board)
    }
}

/// Load the package, the design's ports and the constraint files for the
/// pin planner. A project with no constraint file gets `constraints/pins.ucf`
/// listed in `[target]` (the file is created on the first assignment).
pub async fn pin_data(project: &Project, cfg: &UserConfig, ctx: &Ctx) -> Result<PinData> {
    let (part, devpkg) = ise_part(project)?;
    let pkg = package_file(cfg, &devpkg, ctx).await?;
    let package = Package::parse(&std::fs::read_to_string(pkg)?);
    let design = analyse(project, cfg, None)?;
    let top = design.top().context("Yosys found no top module")?;
    let ports = fpgaflow_layout::pins::port_bits(top.ports.iter().map(|p| {
        let dir = match p.direction {
            design::Direction::Input => "input",
            design::Direction::Output => "output",
            design::Direction::Inout => "inout",
        };
        (p.name.as_str(), p.width, dir)
    }));
    let board = fpgaflow_pnr::project_board(project);
    let labels = board
        .map(fpgaflow_layout::pins::board_labels)
        .unwrap_or_default();
    let mut files = fpgaflow_pnr::constraint_files(project);
    if files.is_empty() {
        add_constraint_file(project, "constraints/pins.ucf")?;
        files.push(project.root.join("constraints/pins.ucf"));
    }
    Ok(PinData {
        part,
        package,
        ports,
        board,
        labels,
        new_file: files[0].clone(),
        files,
    })
}

/// List `rel` in `[target] constraints` of fpgaflow.toml, editing the text in
/// place so comments and layout survive. No-op if it is already listed.
pub fn add_constraint_file(project: &Project, rel: &str) -> Result<()> {
    let listed = project
        .file
        .target
        .as_ref()
        .is_some_and(|t| t.constraints.iter().any(|c| c.as_os_str() == rel));
    if listed {
        return Ok(());
    }
    let path = project.manifest_path();
    let text = std::fs::read_to_string(&path)?;
    std::fs::write(&path, add_constraint_text(&text, rel))?;
    Ok(())
}

fn add_constraint_text(text: &str, rel: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let item = format!("\"{rel}\"");
    match lines.iter().position(|l| l.trim() == "[target]") {
        None => {
            lines.push(String::new());
            lines.push("[target]".into());
            lines.push(format!("constraints = [{item}]"));
        }
        Some(t) => {
            let end = lines[t + 1..]
                .iter()
                .position(|l| l.trim_start().starts_with('['))
                .map(|i| t + 1 + i)
                .unwrap_or(lines.len());
            match (t + 1..end).find(|&i| lines[i].trim_start().starts_with("constraints")) {
                None => lines.insert(t + 1, format!("constraints = [{item}]")),
                Some(k) => {
                    // The closing bracket: on this line, or on a later one.
                    let close = (k..end).find(|&i| lines[i].contains(']')).unwrap_or(k);
                    let l = lines[close].clone();
                    let at = l.rfind(']').unwrap_or(l.len());
                    let before = l[..at].trim_end();
                    if before.is_empty() {
                        // `]` on its own line: add a line in the list's style.
                        let indent: String = lines[close - 1]
                            .chars()
                            .take_while(|c| c.is_whitespace())
                            .collect();
                        lines.insert(close, format!("{indent}{item},"));
                    } else {
                        let sep = if before.ends_with('[') { "" } else { ", " };
                        lines[close] = format!("{before}{sep}{item}{}", &l[at..]);
                    }
                }
            }
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod constraint_list_tests {
    use super::add_constraint_text;

    #[test]
    fn adds_to_existing_list_or_creates_one() {
        let one = "[target]\nboard = \"rc10\"\nconstraints = [\"constraints/a.ucf\"]  # pins\n\n[program]\nmode = \"sram\"\n";
        assert_eq!(
            add_constraint_text(one, "constraints/floorplan.ucf"),
            "[target]\nboard = \"rc10\"\nconstraints = [\"constraints/a.ucf\", \"constraints/floorplan.ucf\"]  # pins\n\n[program]\nmode = \"sram\"\n"
        );
        let multi = "[target]\nconstraints = [\n    \"a.ucf\",\n]\n";
        assert_eq!(
            add_constraint_text(multi, "b.ucf"),
            "[target]\nconstraints = [\n    \"a.ucf\",\n    \"b.ucf\",\n]\n"
        );
        assert_eq!(
            add_constraint_text("[target]\nboard = \"x\"\n", "p.ucf"),
            "[target]\nconstraints = [\"p.ucf\"]\nboard = \"x\"\n"
        );
        assert_eq!(
            add_constraint_text("[project]\nname = \"x\"\n", "p.ucf"),
            "[project]\nname = \"x\"\n\n[target]\nconstraints = [\"p.ucf\"]\n"
        );
    }
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
