//! CLI front ends: run a `fpgaflow_flow` action and print its results.

use std::future::Future;
use std::path::Path;

use anyhow::{bail, Result};
use fpgaflow_core::job::JobEvent;
use fpgaflow_core::project::{Project, SimTool};
use fpgaflow_core::stage::{Summary, Verdict};
use fpgaflow_core::UserConfig;
use fpgaflow_flow::boards::{self, Hotplug, Identity, UsbDevice};
use fpgaflow_flow::{rel, BuildOptions, Ctx};
use fpgaflow_hdl::Severity;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn sim_tool(project: &Project, arg: Option<&str>) -> SimTool {
    match arg {
        Some("verilator") => SimTool::Verilator,
        Some("icarus") => SimTool::Icarus,
        _ => project.file.sim.tool,
    }
}

/// Run an async flow action, printing its events live. Ctrl-C cancels the
/// running job (jobs run in their own process group, so the terminal's SIGINT
/// doesn't reach them directly).
fn run<T, F, Fut>(f: F) -> Result<T>
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = Result<T>>,
{
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
                    JobEvent::Note { text } => eprintln!("== {text}"),
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
        let result = f(Ctx { events: tx, cancel }).await;
        let _ = printer.await;
        result
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
            d.file = rel(project, &d.file).to_path_buf();
            eprintln!("  {d}");
        }
    }
    for h in &s.hints {
        eprintln!("hint: {h}");
    }
}

pub fn lint(dir: &Path, cfg: &UserConfig) -> Result<()> {
    let project = Project::find(dir)?;
    let s = run(|ctx| {
        let project = &project;
        async move { fpgaflow_flow::lint(project, cfg, &ctx).await }
    })?;
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
    let s = run(|ctx| {
        let project = &project;
        async move { fpgaflow_flow::simulate(project, cfg, tool, &ctx).await }
    })?;
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
        eprintln!("waveform: {}", rel(&project, w).display());
        if waves {
            let layout = fpgaflow_flow::open_waves(&project, cfg, tool)?;
            eprintln!(
                "GTKWave layout: {} (save from GTKWave to keep your view)",
                rel(&project, &layout).display()
            );
        }
    }
    if !s.ok {
        bail!("simulation failed");
    }
    Ok(())
}

pub fn waves(dir: &Path, cfg: &UserConfig, tool: Option<&str>) -> Result<()> {
    let project = Project::find(dir)?;
    let tool = sim_tool(&project, tool);
    let layout = fpgaflow_flow::open_waves(&project, cfg, tool)?;
    eprintln!(
        "GTKWave layout: {} (save from GTKWave to keep your view)",
        rel(&project, &layout).display()
    );
    Ok(())
}

pub fn hier(dir: &Path, cfg: &UserConfig, top: Option<String>) -> Result<()> {
    let project = Project::find(dir)?;
    let d = fpgaflow_flow::analyse(&project, cfg, top)?;
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

fn print_utilisation(u: &fpgaflow_synth::report::Utilisation) {
    println!("\nUtilisation ({} cells)", u.cells);
    for (cat, n) in &u.by_category {
        println!("  {:<18} {n:>8}", cat.label());
    }
}

pub fn synth(
    dir: &Path,
    cfg: &UserConfig,
    family: Option<&str>,
    backend: Option<&str>,
) -> Result<()> {
    let project = Project::find(dir)?;
    let out = run(|ctx| {
        let project = &project;
        async move { fpgaflow_flow::synthesize(project, cfg, family, backend, &ctx).await }
    })?;
    report(&project, &out.summary);
    if !out.summary.ok {
        eprintln!("full log: {}", rel(&project, &out.log).display());
        bail!("synthesis failed");
    }
    if let Some(u) = &out.utilisation {
        print_utilisation(u);
        println!("\nBy primitive");
        for (cell, n) in &u.by_type {
            println!("  {cell:<18} {n:>8}");
        }
    }
    println!("\nnetlist: {}", rel(&project, &out.netlist).display());
    println!("log:     {}", rel(&project, &out.log).display());
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
    let project = Project::find(dir)?;
    let opts = BuildOptions {
        family: family.map(String::from),
        backend: backend.map(String::from),
        allow_unconstrained,
    };
    let out = run(|ctx| {
        let (project, opts) = (&project, &opts);
        async move { fpgaflow_flow::build(project, cfg, opts, &ctx).await }
    })?;
    for s in &out.stages {
        report(&project, s);
    }
    if let Some(why) = &out.failure {
        if out.bitstream.is_none() {
            bail!("{why}");
        }
    }
    println!("\nResources ({})", out.part.as_deref().unwrap_or("?"));
    for r in &out.resources {
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
    println!("\nTiming");
    match out.timing.as_ref().map(|t| (t.errors, t.fmax_mhz, t)) {
        Some((Some(0), Some(f), t)) => {
            println!(
                "  all constraints met; Fmax {f:.1} MHz (min period {:.3} ns)",
                t.min_period_ns.unwrap_or(0.0)
            )
        }
        Some((Some(e), _, t)) if e > 0 => {
            println!("  {e} TIMING ERROR(S), score {}", t.score.unwrap_or(0))
        }
        _ => println!("  no timing summary found"),
    }
    println!("\nPins");
    for r in &out.pins {
        println!(
            "  {:<10} {:<6} {:<8} {:<11} {}",
            r.signal, r.pin, r.direction, r.io_standard, r.constraint
        );
    }
    if let Some(b) = &out.bitstream {
        println!("\nbitstream: {}", rel(&project, b).display());
    }
    if let Some(why) = out.failure {
        bail!("{why}");
    }
    Ok(())
}

pub fn program(dir: &Path, cfg: &UserConfig, bit: Option<std::path::PathBuf>) -> Result<()> {
    let project = Project::find(dir)?;
    let bit = bit.map(|b| dir.join(b));
    let s = run(|ctx| {
        let (project, bit) = (&project, bit.as_deref());
        async move { fpgaflow_flow::program(project, cfg, bit, &ctx).await }
    })?;
    report(&project, &s);
    if !s.ok {
        bail!("programming failed");
    }
    eprintln!("loaded");
    Ok(())
}

pub fn detect(dir: &Path, cfg: &UserConfig) -> Result<()> {
    let project = Project::find(dir)?;
    let s = run(|ctx| {
        let project = &project;
        async move { fpgaflow_flow::detect(project, cfg, &ctx).await }
    })?;
    if !s.ok {
        bail!("JTAG scan failed");
    }
    Ok(())
}

pub fn boards() -> Result<()> {
    for b in fpgaflow_core::board::boards() {
        println!("{:<14} {}", b.id, b.name);
        println!(
            "               {}-{}-{} via {}, cable {}",
            b.device, b.speed, b.package, b.backend, b.program.cable
        );
    }
    Ok(())
}

pub fn describe(d: &UsbDevice, id: &Identity) -> String {
    let what = match id {
        Identity::Board { name, .. } => format!("BOARD  {name}"),
        Identity::Cable { names } => format!("CABLE  {}", names.join(" / ")),
        Identity::Unknown => {
            let text = [d.manufacturer.as_deref(), d.product.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            if text.is_empty() {
                "(unknown)".to_string()
            } else {
                text
            }
        }
    };
    format!("{}  {what}  [bus {} dev {}]", d.id(), d.bus, d.dev)
}

pub fn usb(cfg: &UserConfig, all: bool) -> Result<()> {
    let cables = fpgaflow_flow::cables(cfg);
    let mut shown = 0;
    for d in boards::scan()? {
        let id = boards::identify(&d, &cables);
        if all || id != Identity::Unknown {
            println!("{}", describe(&d, &id));
            shown += 1;
        }
    }
    if shown == 0 {
        println!(
            "no boards or programming cables found ({} cable types known)",
            cables.len()
        );
    }
    Ok(())
}

pub fn watch(cfg: &UserConfig) -> Result<()> {
    let cables = fpgaflow_flow::cables(cfg);
    let present = boards::scan()?;
    for d in &present {
        let id = boards::identify(d, &cables);
        if id != Identity::Unknown {
            println!("  {}", describe(d, &id));
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let _watcher = boards::watch(&present, tx)?;
    eprintln!("watching USB (Ctrl-C to stop)");
    for ev in rx {
        let (sign, d) = match &ev {
            Hotplug::Added(d) => ('+', d),
            Hotplug::Removed(d) => ('-', d),
        };
        let id = boards::identify(d, &cables);
        if id != Identity::Unknown {
            println!("{sign} {}", describe(d, &id));
        }
    }
    Ok(())
}
