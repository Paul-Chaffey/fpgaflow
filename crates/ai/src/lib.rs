//! Claude integration: generate RTL, testbenches and constraints, explain
//! code and fix errors, through either the Anthropic Messages API
//! ([`api`]) or the Claude Code CLI ([`cli`]).
//!
//! Every answer is structured JSON (a list of files plus a summary), checked
//! by [`check`] (it must compile, a testbench must pass, a UCF may only use
//! pins from the board's master UCF), and handed back for review. Nothing is
//! written into the project here; the caller shows a diff and writes only
//! what the user accepts.

pub mod api;
pub mod check;
pub mod cli;
pub mod sse;

use std::path::{Component, Path, PathBuf};

use fpgaflow_core::board;
use fpgaflow_core::project::Project;
use fpgaflow_core::{AiBackend, AiConfig, UserConfig};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

/// What the user asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Task {
    /// A new synthesisable module from a description.
    NewModule { description: String },
    /// A self-checking testbench for an existing module.
    Testbench {
        module: String,
        ports: String,
        description: String,
    },
    /// Fix the listed compiler/tool errors.
    FixErrors { diagnostics: Vec<String> },
    /// A UCF for the project's board, using only its master UCF's pins.
    Constraints { ports: String, description: String },
    /// Explain a file (answer in the summary; no files).
    Explain { file: PathBuf, question: String },
}

impl Task {
    pub fn label(&self) -> &'static str {
        match self {
            Task::NewModule { .. } => "new module",
            Task::Testbench { .. } => "testbench",
            Task::FixErrors { .. } => "fix errors",
            Task::Constraints { .. } => "constraints",
            Task::Explain { .. } => "explain",
        }
    }
}

/// A file Claude proposes. `path` is relative to the project root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenFile {
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub purpose: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generated {
    pub files: Vec<GenFile>,
    pub summary: String,
}

/// Progress while a request runs.
#[derive(Debug, Clone, PartialEq)]
pub enum AiEvent {
    Status(String),
    /// Summarised reasoning, streamed (API backend).
    Thinking(String),
    /// Answer text as it streams (the JSON being written).
    Text(String),
    /// Result of checking one round's files.
    Checked {
        round: u32,
        ok: bool,
        report: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error(
        "no API key: set ANTHROPIC_API_KEY or [ai] api_key_command, or use the Claude Code backend"
    )]
    NoApiKey,
    #[error("Claude declined this request ({0})")]
    Refused(String),
    #[error("the answer was cut off at the output limit; ask for less at once")]
    Truncated,
    #[error("API error {status}: {message}")]
    Api { status: u16, message: String },
    #[error("unexpected answer format: {0}")]
    Format(String),
    #[error("generated path {0:?} is not allowed (only rtl/, tb/ and constraints/; .v .sv .svh .vh .ucf)")]
    BadPath(String),
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// JSON schema every answer must follow.
pub fn output_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "files": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Relative to the project root, e.g. rtl/uart_tx.sv"},
                        "content": {"type": "string", "description": "Complete file contents"},
                        "purpose": {"type": "string"}
                    },
                    "required": ["path", "content", "purpose"],
                    "additionalProperties": false
                }
            },
            "summary": {"type": "string", "description": "What was done and anything the engineer should check"}
        },
        "required": ["files", "summary"],
        "additionalProperties": false
    })
}

const ALLOWED_DIRS: &[&str] = &["rtl", "tb", "constraints"];
const ALLOWED_EXT: &[&str] = &["v", "sv", "svh", "vh", "ucf"];

/// Reject anything that would write outside rtl/, tb/ or constraints/, or a
/// file type the flow doesn't use.
pub fn validate_path(p: &str) -> Result<PathBuf, AiError> {
    let path = Path::new(p);
    let ok_components = path.components().all(|c| matches!(c, Component::Normal(_)));
    let first = path
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str());
    let ext_ok = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ALLOWED_EXT.contains(&e));
    if ok_components
        && first.is_some_and(|f| ALLOWED_DIRS.contains(&f))
        && ext_ok
        && path.components().count() >= 2
    {
        Ok(path.to_path_buf())
    } else {
        Err(AiError::BadPath(p.to_string()))
    }
}

/// Parse and validate an answer.
pub fn parse_answer(text: &str) -> Result<Generated, AiError> {
    let g: Generated =
        serde_json::from_str(text.trim()).map_err(|e| AiError::Format(e.to_string()))?;
    for f in &g.files {
        validate_path(&f.path)?;
    }
    Ok(g)
}

// ------------------------------------------------------------------ prompts

const SYSTEM: &str = r#"You are an experienced FPGA digital design engineer working inside fpgaflow, an open FPGA tool flow (Icarus Verilog 14 and Verilator 5 for simulation, Yosys for synthesis, then vendor or open place-and-route).

Write Verilog-2005 / SystemVerilog-2012 that all of Icarus, Verilator and Yosys `read_verilog -sv` accept: logic, always_ff, always_comb, enums, packed structs and parameters are fine; avoid classes, interfaces/modports, assertions, covergroups and DPI in synthesisable code.

Conventions:
- Synthesisable RTL goes in rtl/<module>.sv, one module per file, file name = module name.
- Testbenches go in tb/tb_<module>.sv with module name tb_<module>.
- Start every file with `timescale 1ns/1ps.
- Testbenches are self-checking: print exactly "PASS: tb_<module>" once when every check passed, print a line starting "FAIL:" for each failed check, then call $finish. They must always terminate (bounded loops, a watchdog timeout).
- Testbenches dump waveforms like this:
    initial begin
        string wave;
        if (!$value$plusargs("wave=%s", wave)) wave = "tb_<module>.fst";
        $dumpfile(wave);
        $dumpvars(0, tb_<module>);
    end
- Avoid races in testbenches: change stimulus away from the active clock edge (e.g. #1 after @(posedge clk)).
- Physical constraints are UCF files for Xilinx ISE in constraints/. Never invent pin locations: use only pins that appear in the board's master UCF given to you, keeping its IOSTANDARD/PULLDOWN/SLEW/DRIVE settings, and rename nets to the design's top-level port names.

Answer only with the JSON object the schema describes: complete file contents (never diffs or placeholders) and a short summary that tells the engineer what to check."#;

pub fn system_prompt() -> &'static str {
    SYSTEM
}

/// Total size of source text included in a prompt.
const MAX_CONTEXT_BYTES: usize = 200_000;

fn rel(project: &Project, p: &Path) -> String {
    p.strip_prefix(&project.root)
        .unwrap_or(p)
        .display()
        .to_string()
}

/// Project description plus the source files, for the prompt.
pub fn project_context(project: &Project) -> String {
    let mut s = format!("Project: {}\n", project.file.project.name);
    if let Some(t) = &project.file.project.top {
        s.push_str(&format!("Synthesis top: {t}\n"));
    }
    if let Some(t) = &project.file.target {
        s.push_str(&format!(
            "Target: family {} device {} package {} speed {} backend {}\n",
            t.family.as_deref().unwrap_or("?"),
            t.device.as_deref().unwrap_or("?"),
            t.package.as_deref().unwrap_or("?"),
            t.speed.as_deref().unwrap_or("?"),
            t.backend.as_deref().unwrap_or("default")
        ));
        if let Some(b) = t.board.as_deref().and_then(board::board) {
            s.push_str(&format!("Board: {}\n", b.name));
        }
    }
    s.push_str(&format!("Simulator: {:?}\n", project.file.sim.tool));
    let mut used = 0usize;
    let mut files: Vec<PathBuf> = project.rtl_files().unwrap_or_default();
    files.extend(project.tb_files().unwrap_or_default());
    files.extend(
        project
            .file
            .target
            .as_ref()
            .map(|t| {
                t.constraints
                    .iter()
                    .map(|c| project.root.join(c))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        if used + text.len() > MAX_CONTEXT_BYTES {
            s.push_str(&format!(
                "\n(file {} omitted: context limit)\n",
                rel(project, &f)
            ));
            continue;
        }
        used += text.len();
        s.push_str(&format!("\n----- {} -----\n{text}", rel(project, &f)));
        if !text.ends_with('\n') {
            s.push('\n');
        }
    }
    s
}

/// The user message for a task.
pub fn user_prompt(project: &Project, task: &Task) -> String {
    let ctx = project_context(project);
    let ask = match task {
        Task::NewModule { description } => {
            format!("Write a new synthesisable module (and nothing else) for this request:\n{description}")
        }
        Task::Testbench { module, ports, description } => format!(
            "Write a self-checking testbench tb/tb_{module}.sv for module `{module}`.\nPorts (from Yosys):\n{ports}\n{}",
            if description.trim().is_empty() {
                "Check its behaviour thoroughly, including reset and edge cases.".to_string()
            } else {
                format!("What to test:\n{description}")
            }
        ),
        Task::FixErrors { diagnostics } => format!(
            "The tools report these problems. Fix them by returning corrected versions of the affected files (complete files):\n{}",
            diagnostics.join("\n")
        ),
        Task::Constraints { ports, description } => {
            let master = project
                .file
                .target
                .as_ref()
                .and_then(|t| t.board.as_deref())
                .and_then(board::board)
                .and_then(|b| b.ucf_text())
                .unwrap_or("(no board master UCF available: do not write a UCF, explain why in the summary)");
            format!(
                "Write constraints/<board>.ucf mapping the top-level ports to board pins.\nTop-level ports:\n{ports}\nWhat each port connects to:\n{description}\n\nBoard master UCF (the only pins you may use):\n{master}"
            )
        }
        Task::Explain { file, question } => format!(
            "Explain {} to an experienced hardware engineer. Return no files; put the explanation in the summary.\nQuestion: {question}",
            rel(project, file)
        ),
    };
    format!("{ctx}\n=====\n{ask}")
}

/// Ask again after a failed check, with the previous attempt and the errors.
pub fn retry_prompt(original: &str, previous: &Generated, report: &str) -> String {
    let files = serde_json::to_string_pretty(&previous.files).unwrap_or_default();
    format!(
        "{original}\n\n=====\nYour previous answer failed fpgaflow's checks.\nPrevious files:\n{files}\n\nCheck report:\n{report}\n\nReturn corrected complete files."
    )
}

// ------------------------------------------------------------------ running

/// One model call: system + user prompt → validated answer.
pub async fn ask(
    cfg: &AiConfig,
    project: &Project,
    prompt: &str,
    events: &UnboundedSender<AiEvent>,
    cancel: &CancellationToken,
) -> Result<Generated, AiError> {
    match cfg.backend {
        AiBackend::Api => api::ask(cfg, SYSTEM, prompt, events, cancel).await,
        AiBackend::ClaudeCode => cli::ask(cfg, &project.root, SYSTEM, prompt, events, cancel).await,
    }
}

/// The final result of [`generate`].
#[derive(Debug, Clone)]
pub struct Outcome {
    pub generated: Generated,
    /// Did the last round pass the checks?
    pub checked_ok: bool,
    pub report: String,
    pub rounds: u32,
}

/// Ask, check, and re-ask with the errors up to `max_fix_rounds` times.
pub async fn generate(
    user: &UserConfig,
    project: &Project,
    task: &Task,
    events: &UnboundedSender<AiEvent>,
    cancel: &CancellationToken,
) -> Result<Outcome, AiError> {
    let cfg = &user.ai;
    let first = user_prompt(project, task);
    let mut prompt = first.clone();
    let mut round = 0;
    loop {
        round += 1;
        let _ = events.send(AiEvent::Status(format!(
            "round {round}: asking {} via {}",
            cfg.model,
            match cfg.backend {
                AiBackend::Api => "the Anthropic API",
                AiBackend::ClaudeCode => "Claude Code",
            }
        )));
        let generated = ask(cfg, project, &prompt, events, cancel).await?;
        if matches!(task, Task::Explain { .. }) || generated.files.is_empty() {
            return Ok(Outcome {
                generated,
                checked_ok: true,
                report: String::new(),
                rounds: round,
            });
        }
        let _ = events.send(AiEvent::Status(format!(
            "round {round}: checking {} file(s)",
            generated.files.len()
        )));
        let report = check::check(user, project, &generated, cancel).await?;
        let _ = events.send(AiEvent::Checked {
            round,
            ok: report.ok,
            report: report.text.clone(),
        });
        if report.ok || round > cfg.max_fix_rounds {
            return Ok(Outcome {
                generated,
                checked_ok: report.ok,
                report: report.text,
                rounds: round,
            });
        }
        prompt = retry_prompt(&first, &generated, &report.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_confined() {
        for ok in [
            "rtl/uart.sv",
            "tb/tb_uart.sv",
            "constraints/s3an-starter.ucf",
            "rtl/sub/x.v",
        ] {
            assert!(validate_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "../x.sv",
            "/etc/passwd",
            "rtl/../../x.sv",
            "fpgaflow.toml",
            "rtl/x.py",
            "build/x.sv",
            "rtl.sv",
            "./rtl/x.sv",
        ] {
            assert!(validate_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn answer_parsing() {
        let g = parse_answer(r#"{"files":[{"path":"rtl/a.sv","content":"module a; endmodule","purpose":"x"}],"summary":"s"}"#).unwrap();
        assert_eq!(g.files[0].path, "rtl/a.sv");
        assert!(matches!(
            parse_answer(
                r#"{"files":[{"path":"../a.sv","content":"","purpose":""}],"summary":""}"#
            ),
            Err(AiError::BadPath(_))
        ));
        assert!(matches!(parse_answer("not json"), Err(AiError::Format(_))));
    }

    #[test]
    fn schema_is_strict() {
        let s = output_schema();
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(
            s["properties"]["files"]["items"]["additionalProperties"],
            false
        );
    }

    #[test]
    fn constraints_prompt_includes_master_ucf() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = fpgaflow_core::project::ProjectFile::new("p");
        f.target = Some(fpgaflow_core::project::TargetSection {
            board: Some("s3an-starter".into()),
            ..Default::default()
        });
        let p = Project::create(dir.path(), f).unwrap();
        let t = Task::Constraints {
            ports: "input clk".into(),
            description: "clk is the 50 MHz oscillator".into(),
        };
        let prompt = user_prompt(&p, &t);
        assert!(prompt.contains(r#"NET "CLK_50MHZ" LOC = "E12""#));
        assert!(prompt.contains("Board: Xilinx Spartan-3AN Starter Kit"));
    }
}
