//! Running a planned flow stage.
//!
//! Every stage (lint, simulation, synthesis, P&R, ...) is first *planned*:
//! turned into a list of [`JobSpec`]s with no side effects, so command lines
//! are easy to test and to show in the GUI. [`execute`] then runs the jobs in
//! order, streams their output, collects diagnostics and, for a simulation,
//! decides the PASS/FAIL verdict.

use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio_util::sync::CancellationToken;

use crate::diag::{self, Diagnostic};
use crate::job::{self, JobEvent, JobSpec, Outcome, RunRecord};

#[derive(Debug, thiserror::Error)]
pub enum StageError {
    #[error("cannot start {job}: {source}")]
    Spawn {
        job: String,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A planned stage: jobs to run in order, in `workdir`.
#[derive(Debug, Clone)]
pub struct Plan {
    pub stage: String,
    pub workdir: PathBuf,
    pub jobs: Vec<JobSpec>,
    /// Files the stage produces. They are deleted before the run so a
    /// failed run can't leave a stale one looking current.
    pub outputs: Vec<PathBuf>,
    /// Which job's output decides PASS/FAIL (simulation runs only).
    pub verdict_job: Option<usize>,
    /// Generated input files (scripts) written just before the jobs run.
    pub files: Vec<(PathBuf, String)>,
}

/// What a testbench said about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    /// The testbench printed neither PASS nor FAIL.
    Unknown,
}

static FAIL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(FAIL|FAILED|FATAL)\b|^%Error|^ERROR:|\$fatal").unwrap());
static PASS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(PASS|PASSED)\b").unwrap());

/// Decide PASS/FAIL from simulation output. Any failure line wins over a
/// pass line; self-checking testbenches are expected to print `PASS` or `FAIL`.
pub fn verdict<'a>(lines: impl IntoIterator<Item = &'a str>) -> Verdict {
    let mut pass = false;
    for l in lines {
        if FAIL_RE.is_match(l) {
            return Verdict::Fail;
        }
        pass |= PASS_RE.is_match(l);
    }
    if pass {
        Verdict::Pass
    } else {
        Verdict::Unknown
    }
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub stage: String,
    /// Every job that ran exited 0 (and, for a sim, the verdict isn't Fail).
    pub ok: bool,
    pub records: Vec<RunRecord>,
    pub diagnostics: Vec<Diagnostic>,
    pub verdict: Option<Verdict>,
    /// The plan's outputs that exist after the run.
    pub outputs: Vec<PathBuf>,
    /// Suggested fixes for recognised environment problems.
    pub hints: Vec<String>,
}

/// Known failure signatures and what to do about them.
const HINTS: &[(&str, &str)] = &[
    (
        "lz4.h: No such file",
        "Verilator's FST writer needs the lz4 headers: `sudo apt install liblz4-dev`, or set [sim] wave = \"vcd\"",
    ),
    ("zlib.h: No such file", "Verilator needs the zlib headers: `sudo apt install zlib1g-dev`"),
];

fn hints_for(lines: &[String]) -> Vec<String> {
    HINTS
        .iter()
        .filter(|(sig, _)| lines.iter().any(|l| l.contains(sig)))
        .map(|(_, hint)| hint.to_string())
        .collect()
}

/// Run a plan's jobs in order, stopping at the first failure. Output is
/// forwarded to `events` as it arrives. Run records go to `workdir/run.json`.
pub async fn execute(
    plan: &Plan,
    events: &UnboundedSender<JobEvent>,
    cancel: &CancellationToken,
) -> Result<Summary, StageError> {
    std::fs::create_dir_all(&plan.workdir)?;
    for (path, text) in &plan.files {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, text)?;
    }
    // Stale artefacts from an earlier run must not look like this run's output.
    for out in &plan.outputs {
        match std::fs::remove_file(out) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    let mut records = Vec::new();
    let mut diagnostics = Vec::new();
    let mut verdict_lines: Vec<String> = Vec::new();
    let mut hints = Vec::new();
    let mut ok = true;

    for (i, spec) in plan.jobs.iter().enumerate() {
        let (itx, mut irx) = mpsc::unbounded_channel();
        let runner = async move {
            let r = job::run(spec, &itx, cancel).await;
            drop(itx); // lets the forwarding loop below finish
            r
        };
        let forward = async {
            let mut lines = Vec::new();
            while let Some(ev) = irx.recv().await {
                if let JobEvent::Line { text, .. } = &ev {
                    lines.push(text.clone());
                }
                let _ = events.send(ev);
            }
            lines
        };
        let (result, lines) = tokio::join!(runner, forward);
        let rec = result.map_err(|source| StageError::Spawn {
            job: spec.name.clone(),
            source,
        })?;
        diagnostics.extend(
            diag::parse_lines(&lines)
                .into_iter()
                .map(|d| d.resolve(&spec.cwd)),
        );
        hints.extend(hints_for(&lines));
        if plan.verdict_job == Some(i) {
            verdict_lines = lines;
        }
        let success = rec.outcome.success();
        records.push(rec);
        if !success {
            ok = false;
            break;
        }
    }

    let verdict = plan.verdict_job.map(|i| {
        if records.len() <= i {
            Verdict::Fail // never got to run the simulation
        } else {
            verdict(verdict_lines.iter().map(String::as_str))
        }
    });
    if verdict == Some(Verdict::Fail) {
        ok = false;
    }
    let text = serde_json::to_string_pretty(&records).map_err(std::io::Error::other)?;
    std::fs::write(plan.workdir.join("run.json"), text)?;

    let outputs = plan
        .outputs
        .iter()
        .filter(|p| p.is_file())
        .cloned()
        .collect();
    Ok(Summary {
        stage: plan.stage.clone(),
        ok,
        records,
        diagnostics,
        verdict,
        outputs,
        hints,
    })
}

/// True if any job in the summary was cancelled.
pub fn was_cancelled(s: &Summary) -> bool {
    s.records.iter().any(|r| r.outcome == Outcome::Cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts() {
        assert_eq!(verdict(["x", "PASS: tb"]), Verdict::Pass);
        assert_eq!(verdict(["PASS: a", "FAIL: b"]), Verdict::Fail);
        assert_eq!(verdict(["%Error: tb.sv:3: assertion"]), Verdict::Fail);
        assert_eq!(verdict(["done"]), Verdict::Unknown);
        assert_eq!(verdict(["PASSIVE mode"]), Verdict::Unknown);
    }

    #[tokio::test]
    async fn execute_stops_at_first_failure_and_collects_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let sh = |name: &str, script: &str| {
            JobSpec::new(name, "/bin/sh", dir.path()).args(["-c", script])
        };
        let plan = Plan {
            stage: "t".into(),
            workdir: dir.path().join("w"),
            jobs: vec![
                sh("a", "echo 'rtl/x.sv:3: error: oops'; exit 1"),
                sh("b", "echo never"),
            ],
            outputs: vec![],
            verdict_job: Some(1),
            files: vec![],
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        let s = execute(&plan, &tx, &CancellationToken::new())
            .await
            .unwrap();
        assert!(!s.ok);
        assert_eq!(s.records.len(), 1);
        assert_eq!(s.verdict, Some(Verdict::Fail));
        assert_eq!(s.diagnostics.len(), 1);
        assert_eq!(s.diagnostics[0].file, dir.path().join("rtl/x.sv"));
        assert!(dir.path().join("w/run.json").is_file());
    }

    #[tokio::test]
    async fn execute_pass() {
        let dir = tempfile::tempdir().unwrap();
        let plan = Plan {
            stage: "t".into(),
            workdir: dir.path().to_path_buf(),
            jobs: vec![JobSpec::new("run", "/bin/sh", dir.path()).args(["-c", "echo PASS"])],
            outputs: vec![],
            verdict_job: Some(0),
            files: vec![],
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        let s = execute(&plan, &tx, &CancellationToken::new())
            .await
            .unwrap();
        assert!(s.ok);
        assert_eq!(s.verdict, Some(Verdict::Pass));
    }
}
