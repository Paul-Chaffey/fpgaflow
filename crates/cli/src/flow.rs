//! CLI front ends for the flow stages.

use std::path::Path;

use anyhow::{bail, Context, Result};
use fpgaflow_core::job::JobEvent;
use fpgaflow_core::project::{Project, SimTool};
use fpgaflow_core::{tools, UserConfig};
use fpgaflow_hdl::design::{self, AnalyseRequest};
use fpgaflow_hdl::Severity;
use fpgaflow_sim::{gtkwave, Plan, Summary, Verdict};
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
    if let Some(w) = &s.wave {
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
