//! `fpgaflow ai …`: ask Claude, check the answer, save it as a proposal;
//! `fpgaflow ai apply` writes it. Re-applying never re-asks (no extra cost).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use fpgaflow_core::project::Project;
use fpgaflow_core::{AiBackend, UserConfig};
use fpgaflow_flow::ai::{AiEvent, Generated, Task};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Subcommand)]
pub enum AiCmd {
    /// Write a new module from a description.
    New { description: Vec<String> },
    /// Write a self-checking testbench for a module.
    Testbench {
        module: String,
        /// What to test (default: thorough behaviour, reset, edge cases).
        #[arg(long)]
        describe: Option<String>,
    },
    /// Write a UCF for the project's board (pins only from its master UCF).
    Constraints {
        /// What each top-level port connects to, e.g. "clk is the 50 MHz oscillator, led is LED0".
        #[arg(long)]
        describe: String,
    },
    /// Explain a file.
    Explain {
        file: PathBuf,
        question: Vec<String>,
    },
    /// Lint the RTL and ask Claude to fix what it reports.
    Fix,
    /// Show the last proposal's diffs.
    Show,
    /// Write the last proposal into the project.
    Apply {
        /// Only these paths (default: all files in the proposal).
        only: Vec<String>,
    },
}

#[derive(Serialize, Deserialize)]
struct Saved {
    task: String,
    generated: Generated,
    checked_ok: bool,
    report: String,
    rounds: u32,
}

fn proposal_path(p: &Project) -> PathBuf {
    p.stage_dir("ai").join("proposal.json")
}

pub fn run(dir: &Path, cfg: &UserConfig, backend: Option<&str>, cmd: AiCmd) -> Result<()> {
    let project = Project::find(dir)?;
    let mut cfg = cfg.clone();
    match backend {
        Some("api") => cfg.ai.backend = AiBackend::Api,
        Some("claude-code") => cfg.ai.backend = AiBackend::ClaudeCode,
        _ => {}
    }
    let task = match cmd {
        AiCmd::Show => return show(&project),
        AiCmd::Apply { only } => return apply(&project, &only),
        AiCmd::New { description } => Task::NewModule {
            description: description.join(" "),
        },
        AiCmd::Testbench { module, describe } => Task::Testbench {
            ports: fpgaflow_flow::ports_text(&project, &cfg, Some(&module))?,
            module,
            description: describe.unwrap_or_default(),
        },
        AiCmd::Constraints { describe } => Task::Constraints {
            ports: fpgaflow_flow::ports_text(&project, &cfg, None)?,
            description: describe,
        },
        AiCmd::Explain { file, question } => Task::Explain {
            file: dir.join(file),
            question: if question.is_empty() {
                "What does it do, and is anything wrong?".into()
            } else {
                question.join(" ")
            },
        },
        AiCmd::Fix => {
            let diags = lint_errors(&project, &cfg)?;
            if diags.is_empty() {
                eprintln!("lint is clean: nothing to fix");
                return Ok(());
            }
            Task::FixErrors { diagnostics: diags }
        }
    };
    ask(&project, &cfg, task)
}

fn lint_errors(project: &Project, cfg: &UserConfig) -> Result<Vec<String>> {
    let rt = tokio::runtime::Runtime::new()?;
    let s = rt.block_on(async {
        let (tx, _rx) = mpsc::unbounded_channel();
        let ctx = fpgaflow_flow::Ctx {
            events: tx,
            cancel: CancellationToken::new(),
        };
        fpgaflow_flow::lint(project, cfg, &ctx).await
    })?;
    Ok(s.diagnostics
        .iter()
        .filter(|d| d.severity != fpgaflow_hdl::Severity::Note)
        .map(|d| {
            let mut d = d.clone();
            d.file = fpgaflow_flow::rel(project, &d.file).to_path_buf();
            d.to_string()
        })
        .collect())
}

fn ask(project: &Project, cfg: &UserConfig, task: Task) -> Result<()> {
    eprintln!(
        "asking {} via {} — this uses API credits / your Claude plan",
        cfg.ai.model,
        match cfg.ai.backend {
            AiBackend::Api => "the Anthropic API",
            AiBackend::ClaudeCode => "Claude Code",
        }
    );
    let rt = tokio::runtime::Runtime::new()?;
    let outcome = rt.block_on(async {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("\ninterrupted");
                c2.cancel();
            }
        });
        let printer = tokio::spawn(async move {
            let mut text_chars = 0usize;
            while let Some(ev) = rx.recv().await {
                match ev {
                    AiEvent::Status(s) => eprintln!("== {s}"),
                    AiEvent::Thinking(t) => eprint!("{t}"),
                    AiEvent::Text(t) => {
                        text_chars += t.len();
                        if text_chars % 2000 < t.len() {
                            eprint!(".");
                        }
                    }
                    AiEvent::Checked { round, ok, report } => {
                        eprintln!(
                            "\n== round {round} check: {}",
                            if ok { "passed" } else { "FAILED" }
                        );
                        if !ok {
                            eprintln!("{report}");
                        }
                    }
                }
            }
        });
        let r = fpgaflow_flow::ai_generate(project, cfg, &task, &tx, &cancel).await;
        drop(tx);
        let _ = printer.await;
        r
    })?;
    let saved = Saved {
        task: task.label().into(),
        generated: outcome.generated,
        checked_ok: outcome.checked_ok,
        report: outcome.report,
        rounds: outcome.rounds,
    };
    let path = proposal_path(project);
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, serde_json::to_string_pretty(&saved)?)?;
    print_saved(project, &saved)?;
    if !saved.generated.files.is_empty() {
        eprintln!("\nnothing written yet: review the diffs, then `fpgaflow ai apply` (or `ai apply <path>…`)");
    }
    Ok(())
}

fn load(project: &Project) -> Result<Saved> {
    let path = proposal_path(project);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("no proposal at {}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

fn print_saved(project: &Project, s: &Saved) -> Result<()> {
    println!("\n{}", s.generated.summary);
    if !s.generated.files.is_empty() {
        println!(
            "\ncheck after {} round(s): {}",
            s.rounds,
            if s.checked_ok {
                "PASSED".to_string()
            } else {
                format!("FAILED\n{}", s.report)
            }
        );
        for p in fpgaflow_flow::proposals(project, &s.generated)? {
            println!(
                "\n{} {}  ({})",
                if p.existed { "~" } else { "+" },
                p.file.path,
                p.file.purpose
            );
            print!("{}", p.diff);
        }
    }
    Ok(())
}

fn show(project: &Project) -> Result<()> {
    let s = load(project)?;
    print_saved(project, &s)
}

fn apply(project: &Project, only: &[String]) -> Result<()> {
    let s = load(project)?;
    let all = fpgaflow_flow::proposals(project, &s.generated)?;
    let chosen: Vec<&fpgaflow_flow::Proposal> = all
        .iter()
        .filter(|p| only.is_empty() || only.iter().any(|o| *o == p.file.path))
        .collect();
    if chosen.is_empty() {
        bail!(
            "nothing to apply (paths in the proposal: {})",
            all.iter()
                .map(|p| p.file.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !s.checked_ok {
        eprintln!(
            "warning: this proposal did not pass its checks:\n{}",
            s.report
        );
    }
    for w in fpgaflow_flow::apply(project, &chosen)? {
        println!("wrote {}", fpgaflow_flow::rel(project, &w).display());
    }
    Ok(())
}
