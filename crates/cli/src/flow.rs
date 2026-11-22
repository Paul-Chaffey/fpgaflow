//! CLI front ends for the flow stages.

use std::path::Path;

use anyhow::{bail, Context, Result};
use fpgaflow_core::job::JobEvent;
use fpgaflow_core::project::{Project, SimTool};
use fpgaflow_core::{tools, UserConfig};
use fpgaflow_hdl::design::{self, AnalyseRequest};
use fpgaflow_hdl::Severity;
use fpgaflow_sim::{gtkwave, Plan, Summary, Verdict};
use fpgaflow_synth::report::Utilisation;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn sim_tool(project: &Project, arg: Option<&str>) -> SimTool {
    match arg {
        Some("verilator") => SimTool::Verilator,
        Some("icarus") => SimTool::Icarus,
        _ => project.file.sim.tool,
    }
}

/// Run a plan, printing output live. Ctrl-C cancels the running job (jobs
/// run in their own process group, so the terminal's SIGINT doesn't reach them).
fn run_plan(plan: &Plan) -> Result<Summary> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("\ninterrupted, stopping job...");
                c2.cancel();
            }
        });
        let printer = tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                match ev {
                    JobEvent::Started { command, .. } => eprintln!("$ {command}"),
                    JobEvent::Line { text, .. } => println!("{text}"),
                    JobEvent::Finished {
                        job,
                        outcome,
                        duration,
                    } => {
                        eprintln!("[{job}: {outcome}, {:.2}s]", duration.as_secs_f64())
                    }
                }
            }
        });
        let summary = fpgaflow_sim::execute(plan, &tx, &cancel).await;
        drop(tx);
        let _ = printer.await;
        Ok(summary?)
    })
}

fn report(project: &Project, s: &Summary) {
    let mut diags = s.diagnostics.clone();
    diags.sort_by(|a, b| (a.severity, &a.file, a.line).cmp(&(b.severity, &b.file, b.line)));
    diags.dedup();
    let errors = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warnings = diags
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();
    if errors + warnings > 0 {
        eprintln!("\n{errors} error(s), {warnings} warning(s):");
        for d in diags.iter().filter(|d| d.severity != Severity::Note) {
            let mut d = d.clone();
            d.file = fpgaflow_sim::rel(project, &d.file).to_path_buf();
            eprintln!("  {d}");
        }
    }
    for h in &s.hints {
        eprintln!("hint: {h}");
    }
}

pub fn lint(dir: &Path, cfg: &UserConfig) -> Result<()> {
    let project = Project::find(dir)?;
    let plan = fpgaflow_sim::plan_lint(&project, cfg)?;
    let s = run_plan(&plan)?;
    report(&project, &s);
    if !s.ok {
        bail!("lint failed");
    }
    eprintln!("lint clean");
    Ok(())
}

pub fn sim(dir: &Path, cfg: &UserConfig, tool: Option<&str>, waves: bool) -> Result<()> {
    let project = Project::find(dir)?;
    let tool = sim_tool(&project, tool);
    let plan = fpgaflow_sim::plan_sim(&project, cfg, tool)?;
    let s = run_plan(&plan)?;
    report(&project, &s);
    match s.verdict {
        Some(Verdict::Pass) => eprintln!("\nSIMULATION PASSED ({tool:?})"),
        Some(Verdict::Fail) => eprintln!("\nSIMULATION FAILED ({tool:?})"),
        _ if s.ok => {
            eprintln!("\nSimulation finished; the testbench printed no PASS/FAIL line ({tool:?})")
        }
        _ => {}
    }
    if let Some(w) = s.outputs.first() {
        eprintln!("waveform: {}", fpgaflow_sim::rel(&project, w).display());
        if waves {
            open_waves(&project, cfg, tool, w)?;
        }
    }
    if !s.ok {
        bail!("simulation failed");
    }
    Ok(())
}

fn open_waves(project: &Project, cfg: &UserConfig, tool: SimTool, wave: &Path) -> Result<()> {
    let gtk = tools::resolve("gtkwave", cfg)
        .context("gtkwave is not installed (run `fpgaflow doctor`)")?;
    let top = fpgaflow_sim::tb_top(project)?;
    let layout = gtkwave::ensure_layout(project, &top, tool, wave)?;
    gtkwave::open(&gtk, wave, Some(&layout))?;
    eprintln!(
        "GTKWave layout: {} (save from GTKWave to keep your view)",
        fpgaflow_sim::rel(project, &layout).display()
    );
    Ok(())
}

pub fn waves(dir: &Path, cfg: &UserConfig, tool: Option<&str>) -> Result<()> {
    let project = Project::find(dir)?;
    let tool = sim_tool(&project, tool);
    let top = fpgaflow_sim::tb_top(&project)?;
    let wave = fpgaflow_sim::sim_workdir(&project, tool)
        .join(format!("{top}.{}", project.file.sim.wave.extension()));
    if !wave.is_file() {
        bail!(
            "no waveform at {}; run `fpgaflow sim` first",
            wave.display()
        );
    }
    open_waves(&project, cfg, tool, &wave)
}

pub fn hier(dir: &Path, cfg: &UserConfig, top: Option<String>) -> Result<()> {
    let project = Project::find(dir)?;
    let yosys =
        tools::resolve("yosys", cfg).context("yosys is not installed (run `fpgaflow doctor`)")?;
    let req = AnalyseRequest {
        files: project.rtl_files()?,
        include_dirs: project.include_dirs(),
        defines: project.file.sources.defines.clone(),
        top: top.or_else(|| project.file.project.top.clone()),
        slang: false,
    };
    let d = match design::analyse(&yosys, &req, &project.stage_dir("analyse")) {
        Ok(d) => d,
        Err(design::DesignError::Yosys { diagnostics, log }) => {
            eprintln!("{log}");
            for d in diagnostics {
                eprintln!("  {d}");
            }
            bail!("yosys could not elaborate the design");
        }
        Err(e) => return Err(e.into()),
    };
    for m in &d.modules {
        println!("{}{}", m.name, if m.is_top { "  (top)" } else { "" });
        for p in &m.ports {
            let w = if p.width > 1 {
                format!("[{}:0]", p.width - 1)
            } else {
                String::new()
            };
            println!(
                "    {:<7} {:<8} {}",
                format!("{:?}", p.direction).to_lowercase(),
                w,
                p.name
            );
        }
        if !m.children.is_empty() {
            println!("    instantiates: {}", m.children.join(", "));
        }
    }
    Ok(())
}

pub fn synth(
    dir: &Path,
    cfg: &UserConfig,
    family: Option<&str>,
    backend: Option<&str>,
) -> Result<()> {
    let project = Project::find(dir)?;
    let target = fpgaflow_synth::resolve_target(&project, family, backend)?;
    eprintln!(
        "Target: {} ({}), backend {} -> {} netlist",
        target.family.name,
        target.family.id,
        target.backend.name,
        target.backend.netlist.extension()
    );
    if let Some(why) = &target.family.experimental {
        eprintln!("note: {why}; check the result carefully.");
    }
    let sp = fpgaflow_synth::plan_synth(&project, cfg, target)?;
    let s = run_plan(&sp.plan)?;
    report(&project, &s);
    if !s.ok {
        eprintln!(
            "full log: {}",
            fpgaflow_sim::rel(&project, &sp.log).display()
        );
        bail!("synthesis failed");
    }
    let u = Utilisation::from_stat_json(&std::fs::read_to_string(&sp.stat)?)?;
    println!("\nUtilisation ({} cells)", u.cells);
    for (cat, n) in &u.by_category {
        println!("  {:<18} {n:>8}", cat.label());
    }
    println!("\nBy primitive");
    for (cell, n) in &u.by_type {
        println!("  {cell:<18} {n:>8}");
    }
    println!(
        "\nnetlist: {}",
        fpgaflow_sim::rel(&project, &sp.netlist).display()
    );
    println!(
        "log:     {}",
        fpgaflow_sim::rel(&project, &sp.log).display()
    );
    Ok(())
}

pub fn families() -> Result<()> {
    use fpgaflow_core::family;
    println!(
        "{:<14} {:<26} {:<11} NAME",
        "FAMILY", "BACKENDS", "CONSTRAINTS"
    );
    for f in family::families() {
        let flag = if f.experimental.is_some() {
            "  [experimental]"
        } else {
            ""
        };
        println!(
            "{:<14} {:<26} {:<11} {}{flag}",
            f.id,
            f.backends.join(", "),
            f.constraints,
            f.name
        );
    }
    println!("\nBackends:");
    for b in family::backends() {
        let kind = if b.open_source {
            "open source"
        } else {
            "vendor tool"
        };
        println!(
            "  {:<10} {:<18} {} netlist, {kind}",
            b.id,
            b.name,
            b.netlist.extension()
        );
    }
    Ok(())
}

pub fn build(
    dir: &Path,
    cfg: &UserConfig,
    family: Option<&str>,
    backend: Option<&str>,
    allow_unconstrained: bool,
) -> Result<()> {
    use fpgaflow_pnr::{constraints, ise};

    let project = Project::find(dir)?;
    let target = fpgaflow_synth::resolve_target(&project, family, backend)?;
    if target.backend.id != "ise" {
        return Err(fpgaflow_pnr::PnrError::NotImplemented(target.backend.id.clone()).into());
    }

    // 1. Pin check before spending any time: every top-level port bit needs a LOC.
    let yosys =
        tools::resolve("yosys", cfg).context("yosys is not installed (run `fpgaflow doctor`)")?;
    let req = AnalyseRequest {
        files: project.rtl_files()?,
        include_dirs: project.include_dirs(),
        defines: project.file.sources.defines.clone(),
        top: project.file.project.top.clone(),
        slang: false,
    };
    let design = design::analyse(&yosys, &req, &project.stage_dir("analyse")).map_err(|e| {
        anyhow::anyhow!("elaboration failed: {e} (run `fpgaflow hier` for details)")
    })?;
    let top = design.top().context("Yosys found no top module")?.clone();
    let mut located = std::collections::BTreeSet::new();
    for f in fpgaflow_pnr::constraint_files(&project) {
        let text =
            std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?;
        located.extend(constraints::ucf_located_nets(&text));
    }
    let missing = constraints::unconstrained(&top.ports, &located);
    if !missing.is_empty() {
        let err = fpgaflow_pnr::PnrError::Unconstrained(missing);
        if !allow_unconstrained {
            return Err(err.into());
        }
        eprintln!("warning: {err}");
    }

    // 2. Synthesis.
    eprintln!(
        "== synthesis: {} -> {}",
        target.family.name, target.backend.name
    );
    let sp = fpgaflow_synth::plan_synth(&project, cfg, target)?;
    let s = run_plan(&sp.plan)?;
    report(&project, &s);
    if !s.ok {
        bail!(
            "synthesis failed (log: {})",
            fpgaflow_sim::rel(&project, &sp.log).display()
        );
    }

    // 3. Translate, map, place & route, timing.
    let ip = ise::plan_implement(&project, cfg, &sp.netlist, &top.name)?;
    eprintln!("== implementation: {} on {}", top.name, ip.part);
    let mut s = run_plan(&ip.plan)?;
    ip.remap(&mut s.diagnostics);
    report(&project, &s);
    if !s.ok {
        bail!("implementation failed");
    }

    // 4. Pad report check: refuse to make a bitstream with tool-chosen pins.
    let pads = ise::parse_pad_report(&std::fs::read_to_string(&ip.files.pad)?);
    let auto: Vec<String> = ise::unlocated(&pads)
        .iter()
        .map(|r| format!("{}@{}", r.signal, r.pin))
        .collect();
    if !auto.is_empty() && !allow_unconstrained {
        bail!(
            "ISE placed these pins itself (UNLOCATED in the pad report): {}; no bitstream written",
            auto.join(", ")
        );
    }

    // 5. Bitstream.
    let mode = project.file.program.mode;
    let bp = ise::plan_bitstream(&ip, mode);
    eprintln!("== bitstream ({mode:?} load)");
    let s = run_plan(&bp)?;
    report(&project, &s);
    if !s.ok {
        bail!("bitgen failed");
    }

    // Summary.
    println!("\nResources ({})", ip.part);
    for r in ise::parse_map_report(&std::fs::read_to_string(&ip.files.mrp)?) {
        let pct = if r.available > 0 {
            100.0 * r.used as f64 / r.available as f64
        } else {
            0.0
        };
        println!(
            "  {:<28} {:>7} / {:<7} {pct:>5.1}%",
            r.name, r.used, r.available
        );
    }
    let t = ise::parse_timing_report(&std::fs::read_to_string(&ip.files.twr)?);
    println!("\nTiming");
    match (t.errors, t.fmax_mhz) {
        (Some(0), Some(f)) => println!(
            "  all constraints met; Fmax {f:.1} MHz (min period {:.3} ns)",
            t.min_period_ns.unwrap_or(0.0)
        ),
        (Some(e), _) if e > 0 => println!("  {e} TIMING ERROR(S), score {}", t.score.unwrap_or(0)),
        _ => println!(
            "  no timing summary found (see {})",
            fpgaflow_sim::rel(&project, &ip.files.twr).display()
        ),
    }
    println!("\nPins");
    for r in &pads {
        println!(
            "  {:<10} {:<6} {:<8} {:<11} {}",
            r.signal, r.pin, r.direction, r.io_standard, r.constraint
        );
    }
    println!(
        "\nbitstream: {}",
        fpgaflow_sim::rel(&project, &ip.files.bit).display()
    );
    if t.errors.is_some_and(|e| e > 0) {
        bail!("timing not met");
    }
    Ok(())
}
