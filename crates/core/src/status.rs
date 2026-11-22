//! What the flow has already done for a project, kept across restarts.
//!
//! Each step records its result in `build/status.json` when it finishes,
//! whether it ran from the GUI or the CLI. On opening a project the GUI reads
//! it back; a step whose inputs (RTL, testbench, constraints, fpgaflow.toml)
//! changed since it ran counts as stale.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::project::Project;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    Lint,
    Sim,
    Synth,
    Build,
    Program,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    pub ok: bool,
    /// Seconds since the Unix epoch when the step finished.
    pub finished_unix: u64,
    /// A short note, e.g. `verilator: PASS` or `Fmax 54.3 MHz`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// How a recorded step stands now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Ok,
    Failed,
    /// It passed, but its inputs have changed since.
    Stale,
}

fn path(project: &Project) -> PathBuf {
    project.build_dir().join("status.json")
}

/// Recorded steps. Projects built before fpgaflow kept `status.json` get
/// what can be inferred from their build directory instead.
pub fn load(project: &Project) -> BTreeMap<Step, StepRecord> {
    match std::fs::read_to_string(path(project)) {
        Ok(t) => serde_json::from_str(&t).unwrap_or_default(),
        Err(_) => infer(project),
    }
}

/// `build/<stage>/run.json`: did every job exit 0, and when was it written?
fn run_ok(dir: &Path) -> Option<(bool, u64)> {
    let p = dir.join("run.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
    let ok = v
        .as_array()?
        .iter()
        .all(|r| r["outcome"]["kind"] == "exited" && r["outcome"]["code"].as_i64() == Some(0));
    Some((ok, mtime_unix(&p)?))
}

/// Steps inferred from a build directory with no `status.json`. Simulation
/// is left out: whether the testbench printed PASS was not kept.
fn infer(project: &Project) -> BTreeMap<Step, StepRecord> {
    let mut out = BTreeMap::new();
    let rec = |(ok, t): (bool, u64)| StepRecord {
        ok,
        finished_unix: t,
        detail: None,
    };
    for (step, stage) in [(Step::Lint, "lint"), (Step::Synth, "synth")] {
        if let Some(r) = run_ok(&project.stage_dir(stage)) {
            out.insert(step, rec(r));
        }
    }
    let top = project
        .file
        .project
        .top
        .clone()
        .unwrap_or_else(|| project.file.project.name.clone());
    let impl_dir = project.stage_dir("impl");
    let bit = impl_dir.join(format!("{top}.bit"));
    if let (Some((ran_ok, _)), Some(t)) = (run_ok(&impl_dir), mtime_unix(&bit)) {
        let met = std::fs::read_to_string(impl_dir.join(format!("{top}.twr")))
            .is_ok_and(|s| s.contains("All constraints were met"));
        out.insert(
            Step::Build,
            StepRecord {
                ok: ran_ok && met,
                finished_unix: t,
                detail: (!met).then(|| "timing not met".to_string()),
            },
        );
    }
    out
}

/// Record a finished step (best effort: a failure to write is ignored).
pub fn record(project: &Project, step: Step, ok: bool, detail: Option<String>) {
    let mut all = load(project);
    let finished_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    all.insert(
        step,
        StepRecord {
            ok,
            finished_unix,
            detail,
        },
    );
    let p = path(project);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&all) {
        let _ = std::fs::write(p, text);
    }
}

/// The files a step depends on.
pub fn inputs(project: &Project, step: Step) -> Vec<PathBuf> {
    let mut v = vec![project.manifest_path()];
    v.extend(project.rtl_files().unwrap_or_default());
    match step {
        Step::Lint | Step::Synth => {}
        Step::Sim => v.extend(project.tb_files().unwrap_or_default()),
        Step::Build | Step::Program => {
            if let Some(t) = &project.file.target {
                v.extend(t.constraints.iter().map(|c| project.root.join(c)));
            }
        }
    }
    v
}

fn mtime_unix(p: &Path) -> Option<u64> {
    let m = std::fs::metadata(p).and_then(|m| m.modified()).ok()?;
    m.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// Where each recorded step stands now. Programming is not restored: the
/// board may have been reset or power-cycled since.
pub fn current(project: &Project) -> BTreeMap<Step, (StepState, StepRecord)> {
    let mut out = BTreeMap::new();
    for (step, rec) in load(project) {
        if step == Step::Program {
            continue;
        }
        let state = if !rec.ok {
            StepState::Failed
        } else if inputs(project, step)
            .iter()
            .filter_map(|p| mtime_unix(p))
            .any(|m| m > rec.finished_unix)
        {
            StepState::Stale
        } else {
            StepState::Ok
        };
        out.insert(step, (state, rec));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectFile;

    #[test]
    fn records_survive_and_go_stale_when_sources_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = ProjectFile::new("t");
        f.sources.rtl = vec!["rtl/*.sv".into()];
        let p = Project::create(dir.path(), f).unwrap();
        std::fs::create_dir_all(dir.path().join("rtl")).unwrap();
        let src = dir.path().join("rtl/top.sv");
        std::fs::write(&src, "module top; endmodule\n").unwrap();
        // Sources written "before" the run.
        let old = SystemTime::now() - std::time::Duration::from_secs(10);
        for f in [&src, &p.manifest_path()] {
            std::fs::File::options()
                .write(true)
                .open(f)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }

        record(&p, Step::Synth, true, Some("ok".into()));
        record(&p, Step::Sim, false, Some("icarus: FAIL".into()));
        record(&p, Step::Program, true, None);
        let now = current(&p);
        assert_eq!(now[&Step::Synth].0, StepState::Ok);
        assert_eq!(now[&Step::Sim].0, StepState::Failed);
        assert!(
            !now.contains_key(&Step::Program),
            "programming is not restored"
        );
        assert_eq!(now[&Step::Synth].1.detail.as_deref(), Some("ok"));

        // Without status.json, lint/synth/build are inferred from build/.
        std::fs::remove_file(path(&p)).unwrap();
        let synth = p.stage_dir("synth");
        std::fs::create_dir_all(&synth).unwrap();
        std::fs::write(
            synth.join("run.json"),
            r#"[{"job":"yosys","outcome":{"kind":"exited","code":0},"duration_ms":5}]"#,
        )
        .unwrap();
        let inferred = load(&p);
        assert!(inferred[&Step::Synth].ok);
        assert!(!inferred.contains_key(&Step::Sim));
        record(&p, Step::Synth, true, Some("ok".into()));

        // Editing a source afterwards makes the step stale.
        let later = SystemTime::now() + std::time::Duration::from_secs(10);
        std::fs::File::options()
            .write(true)
            .open(&src)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(current(&p)[&Step::Synth].0, StepState::Stale);
    }
}
