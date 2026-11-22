use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use fpgaflow_core::project::{Project, ProjectFile};
use fpgaflow_core::tools::{self, Stage};
use fpgaflow_core::UserConfig;
use fpgaflow_editor::Position;

mod flow;

#[derive(Parser)]
#[command(
    name = "fpgaflow",
    version,
    about = "Verilog/SystemVerilog FPGA flow: simulate, synthesise, route, program"
)]
struct Cli {
    /// Run as if started in this directory.
    #[arg(short = 'C', long, global = true, default_value = ".")]
    dir: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check which external tools are installed and whether USB access is set up.
    Doctor {
        /// Print machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Create a new project (fpgaflow.toml, rtl/, tb/, constraints/).
    Init {
        path: PathBuf,
        /// Project name (default: the directory name).
        #[arg(long)]
        name: Option<String>,
        /// Synthesis top module.
        #[arg(long)]
        top: Option<String>,
    },
    /// Show the project and the source files it resolves to.
    Info,
    /// Open a file in the configured editor (default: the project's fpgaflow.toml).
    Edit {
        file: Option<PathBuf>,
        #[arg(long)]
        line: Option<u32>,
        #[arg(long)]
        col: Option<u32>,
    },
    /// Open the user configuration (~/.config/fpgaflow/config.toml) in the configured editor.
    Config,
    /// Lint the RTL (Verilator --lint-only, or Icarus if Verilator is absent).
    Lint,
    /// Compile and run the testbench.
    Sim {
        /// Override [sim] tool from fpgaflow.toml.
        #[arg(long, value_parser = ["icarus", "verilator"])]
        tool: Option<String>,
        /// Open the waveform in GTKWave afterwards.
        #[arg(long)]
        waves: bool,
    },
    /// Open the last simulation's waveform in GTKWave.
    Waves {
        #[arg(long, value_parser = ["icarus", "verilator"])]
        tool: Option<String>,
    },
    /// Synthesise the RTL with Yosys for the target family.
    Synth {
        /// Override [target] family (see `fpgaflow families`).
        #[arg(long)]
        family: Option<String>,
        /// Override [target] backend; decides the netlist format.
        #[arg(long)]
        backend: Option<String>,
    },
    /// List supported FPGA families and their P&R backends.
    Families,
    /// Elaborate the RTL with Yosys and print the module hierarchy and ports.
    Hier {
        /// Top module (default: [project] top, else Yosys picks).
        #[arg(long)]
        top: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = UserConfig::load()?;
    match cli.cmd {
        Cmd::Doctor { json } => doctor(&cfg, json),
        Cmd::Init { path, name, top } => init(&cli.dir.join(path), name, top),
        Cmd::Info => info(&cli.dir),
        Cmd::Edit { file, line, col } => {
            let file = match file {
                Some(f) => cli.dir.join(f),
                None => Project::find(&cli.dir)?.manifest_path(),
            };
            let pos = line.map(|line| Position { line, col });
            fpgaflow_editor::open(&cfg.editor, &file, pos)?;
            Ok(())
        }
        Cmd::Config => {
            let path = UserConfig::ensure_exists()?;
            fpgaflow_editor::open(&cfg.editor, &path, None)?;
            println!("{}", path.display());
            Ok(())
        }
        Cmd::Lint => flow::lint(&cli.dir, &cfg),
        Cmd::Sim { tool, waves } => flow::sim(&cli.dir, &cfg, tool.as_deref(), waves),
        Cmd::Waves { tool } => flow::waves(&cli.dir, &cfg, tool.as_deref()),
        Cmd::Hier { top } => flow::hier(&cli.dir, &cfg, top),
        Cmd::Synth { family, backend } => {
            flow::synth(&cli.dir, &cfg, family.as_deref(), backend.as_deref())
        }
        Cmd::Families => flow::families(),
    }
}

fn doctor(cfg: &UserConfig, json: bool) -> Result<()> {
    let tools = tools::discover(cfg);
    let host = tools::host_checks();
    if json {
        let v = serde_json::json!({ "tools": tools, "host": host });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    for stage in Stage::ALL {
        println!("\n{}", stage.label());
        for t in tools.iter().filter(|t| t.spec.stage == stage) {
            let mark = if t.found() { "ok " } else { "-- " };
            let where_ = match (&t.path, &t.version) {
                (Some(p), Some(v)) => format!("{v}  [{}]", p.display()),
                (Some(p), None) => p.display().to_string(),
                (None, _) => "not found".into(),
            };
            println!("  {mark} {:<20} {where_}", t.spec.id);
        }
    }
    println!("\nHost");
    for h in &host {
        println!(
            "  {} {:<26} {}",
            if h.ok { "ok " } else { "!! " },
            h.name,
            h.detail
        );
        if let Some(fix) = &h.fix {
            println!("      fix: {fix}");
        }
    }
    Ok(())
}

fn init(path: &Path, name: Option<String>, top: Option<String>) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))?;
    let name = match name {
        Some(n) => n,
        None => path
            .canonicalize()?
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .context("cannot derive a project name from the path; pass --name")?,
    };
    let mut file = ProjectFile::new(name);
    file.project.top = top;
    let project = Project::create(path, file)?;
    println!("Created {}", project.manifest_path().display());
    Ok(())
}

fn info(dir: &Path) -> Result<()> {
    let p = Project::find(dir)?;
    println!("Project  {}  ({})", p.file.project.name, p.root.display());
    println!(
        "Top      {}",
        p.file.project.top.as_deref().unwrap_or("(auto)")
    );
    println!(
        "Sim      {:?}, waves {:?}",
        p.file.sim.tool, p.file.sim.wave
    );
    match &p.file.target {
        Some(t) => println!(
            "Target   board={} family={} device={} backend={}",
            t.board.as_deref().unwrap_or("-"),
            t.family.as_deref().unwrap_or("-"),
            t.device.as_deref().unwrap_or("-"),
            t.backend.as_deref().unwrap_or("-"),
        ),
        None => println!("Target   (none)"),
    }
    let show = |label: &str, files: Vec<PathBuf>| {
        println!("{label} ({}):", files.len());
        for f in files {
            println!("  {}", f.strip_prefix(&p.root).unwrap_or(&f).display());
        }
    };
    show("RTL", p.rtl_files()?);
    show("Testbench", p.tb_files()?);
    Ok(())
}
