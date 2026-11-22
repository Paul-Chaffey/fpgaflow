use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use fpgaflow_core::project::{Project, ProjectFile};
use fpgaflow_core::tools::{self, Stage};
use fpgaflow_core::UserConfig;
use fpgaflow_editor::Position;

mod ai;
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
    /// Full implementation: synthesis, place & route, timing, bitstream.
    Build {
        #[arg(long)]
        family: Option<String>,
        #[arg(long)]
        backend: Option<String>,
        /// Build even if some top-level pins have no LOC constraint
        /// (the tools will pick those pins; unsafe to load on a real board).
        #[arg(long)]
        allow_unconstrained: bool,
    },
    /// Load the bitstream onto the project's board (volatile SRAM load).
    Program {
        /// Bitstream to load (default: the one `fpgaflow build` wrote).
        #[arg(long)]
        bit: Option<PathBuf>,
    },
    /// Read-only JTAG scan of the project's board: lists the devices in the chain.
    Detect,
    /// Where the last build put things: the device, per-module use, the worst
    /// timing paths site by site, and an instance or net on request (ISE).
    Layout {
        /// How many of the worst timing paths to show.
        #[arg(long, default_value_t = 3)]
        paths: usize,
        /// Show one placed instance (exact name).
        #[arg(long)]
        inst: Option<String>,
        /// Show one net's pins and routing tiles (exact name).
        #[arg(long)]
        net: Option<String>,
    },
    /// Pin planner: list pin assignments and problems, or change one
    /// (writes the project's UCF; ISE targets).
    Pins {
        #[command(subcommand)]
        cmd: Option<PinsCmd>,
    },
    /// List known dev boards.
    Boards,
    /// Scan USB for dev boards and programming cables.
    Usb {
        /// Show every USB device, not just boards and cables.
        #[arg(long)]
        all: bool,
    },
    /// Watch for boards and cables being plugged in or removed (Ctrl-C to stop).
    Watch,
    /// Ask Claude to write, explain or fix code. Answers are compiled/simulated
    /// before you see them and saved as a proposal; `ai apply` writes it.
    Ai {
        /// Override [ai] backend from config.toml.
        #[arg(long, global = true, value_parser = ["claude-code", "api"])]
        backend: Option<String>,
        #[command(subcommand)]
        cmd: ai::AiCmd,
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

#[derive(Subcommand)]
enum PinsCmd {
    /// Put a port bit on a pin, e.g. `pins set led[0] D2` (sets the bank's
    /// I/O standard when the board profile knows it).
    Set { port: String, pin: String },
    /// Remove a port bit's pin (comments the UCF line out).
    Clear { port: String },
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
        Cmd::Program { bit } => flow::program(&cli.dir, &cfg, bit),
        Cmd::Detect => flow::detect(&cli.dir, &cfg),
        Cmd::Pins { cmd } => match cmd {
            None => flow::pins(&cli.dir, &cfg),
            Some(PinsCmd::Set { port, pin }) => flow::pins_set(&cli.dir, &cfg, &port, Some(&pin)),
            Some(PinsCmd::Clear { port }) => flow::pins_set(&cli.dir, &cfg, &port, None),
        },
        Cmd::Layout { paths, inst, net } => {
            flow::layout(&cli.dir, &cfg, paths, inst.as_deref(), net.as_deref())
        }
        Cmd::Boards => flow::boards(),
        Cmd::Usb { all } => flow::usb(&cfg, all),
        Cmd::Watch => flow::watch(&cfg),
        Cmd::Ai { backend, cmd } => ai::run(&cli.dir, &cfg, backend.as_deref(), cmd),
        Cmd::Build {
            family,
            backend,
            allow_unconstrained,
        } => flow::build(
            &cli.dir,
            &cfg,
            family.as_deref(),
            backend.as_deref(),
            allow_unconstrained,
        ),
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
