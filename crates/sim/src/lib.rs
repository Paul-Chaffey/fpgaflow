//! Simulation and lint stages.
//!
//! A stage is first *planned*: turned into a list of [`JobSpec`]s with no
//! side effects, which makes command lines easy to test and to show in the
//! GUI. It is then *executed* with [`execute`], which runs the jobs in order,
//! streams their output, collects diagnostics and decides the verdict.

pub mod gtkwave;

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use fpgaflow_core::job::{self, JobEvent, JobSpec, Outcome, RunRecord};
use fpgaflow_core::project::{Project, ProjectError, SimTool};
use fpgaflow_core::tools;
use fpgaflow_core::UserConfig;
use fpgaflow_hdl::diag::{self, Diagnostic};
use fpgaflow_hdl::scan;
use regex::Regex;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum SimError {
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error("{0} is not installed (run `fpgaflow doctor`)")]
    ToolMissing(&'static str),
    #[error("no testbench files found (check [sources] tb in fpgaflow.toml)")]
    NoTestbench,
    #[error("no RTL files found (check [sources] rtl in fpgaflow.toml)")]
    NoRtl,
    #[error("testbench top is ambiguous; set [sim] tb_top to one of: {}", .0.join(", "))]
    AmbiguousTbTop(Vec<String>),
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
    /// Waveform file the last job will write, if any.
    pub wave: Option<PathBuf>,
    /// Which job's output decides PASS/FAIL (simulation runs only).
    pub verdict_job: Option<usize>,
}

fn require(id: &'static str, cfg: &UserConfig) -> Result<PathBuf, SimError> {
    tools::resolve(id, cfg).ok_or(SimError::ToolMissing(id))
}

/// The testbench top module: `[sim] tb_top`, else the only module declared
/// in the testbench files.
pub fn tb_top(project: &Project) -> Result<String, SimError> {
    if let Some(t) = &project.file.sim.tb_top {
        return Ok(t.clone());
    }
    let files = project.tb_files()?;
    if files.is_empty() {
        return Err(SimError::NoTestbench);
    }
    let mut mods = Vec::new();
    for f in files {
        mods.extend(scan::declared_modules(&std::fs::read_to_string(&f)?));
    }
    match mods.len() {
        0 => Err(SimError::NoTestbench),
        1 => Ok(mods.remove(0)),
        _ => Err(SimError::AmbiguousTbTop(mods)),
    }
}

fn define_args(project: &Project) -> Vec<String> {
    let mut v: Vec<String> = project
        .file
        .sources
        .defines
        .iter()
        .map(|(k, val)| format!("-D{k}={val}"))
        .collect();
    v.extend(
        project
            .include_dirs()
            .iter()
            .map(|d| format!("-I{}", d.display())),
    );
    v
}

pub fn sim_workdir(project: &Project, tool: SimTool) -> PathBuf {
    project.stage_dir("sim").join(match tool {
        SimTool::Icarus => "icarus",
        SimTool::Verilator => "verilator",
    })
}

/// Plan a simulation run with `tool` (normally `project.file.sim.tool`).
pub fn plan_sim(project: &Project, cfg: &UserConfig, tool: SimTool) -> Result<Plan, SimError> {
    let top = tb_top(project)?;
    let mut files = project.rtl_files()?;
    let tb = project.tb_files()?;
    if tb.is_empty() {
        return Err(SimError::NoTestbench);
    }
    files.extend(tb);
    let workdir = sim_workdir(project, tool);
    let fmt = project.file.sim.wave;
    let wave = workdir.join(format!("{top}.{}", fmt.extension()));
    let wave_arg = format!("+wave={}", wave.display());

    let jobs = match tool {
        SimTool::Icarus => {
            let iverilog = require("iverilog", cfg)?;
            let vvp = require("vvp", cfg)?;
            let build = JobSpec::new("sim:compile", iverilog, &workdir)
                .args(["-g2012", "-Wall", "-o", "tb.vvp", "-s", &top])
                .args(define_args(project))
                .args(files.iter().map(|f| f.as_os_str().to_owned()));
            // `-n`: make $stop behave as $finish so a run never waits on stdin.
            // Format flags (-fst) are vvp "extended arguments" and go after the file.
            let mut run = JobSpec::new("sim:run", vvp, &workdir).args(["-n", "tb.vvp"]);
            if fmt == fpgaflow_core::WaveFormat::Fst {
                run = run.arg("-fst");
            }
            vec![build, run.arg(&wave_arg)]
        }
        SimTool::Verilator => {
            let verilator = require("verilator", cfg)?;
            let trace = match fmt {
                fpgaflow_core::WaveFormat::Fst => "--trace-fst",
                fpgaflow_core::WaveFormat::Vcd => "--trace",
            };
            let build = JobSpec::new("sim:compile", verilator, &workdir)
                .args(["--binary", "--timing", trace, "-j", "0", "-Wno-fatal"])
                .args(["--top-module", &top, "-Mdir", "obj_dir", "-o", "Vtb"])
                .args(define_args(project))
                .args(files.iter().map(|f| f.as_os_str().to_owned()));
            let run = JobSpec::new("sim:run", workdir.join("obj_dir/Vtb"), &workdir).arg(&wave_arg);
            vec![build, run]
        }
    };
    let verdict_job = Some(jobs.len() - 1);
    Ok(Plan {
        stage: "sim".into(),
        workdir,
        jobs,
        wave: Some(wave),
        verdict_job,
    })
}

/// Plan a lint of the RTL: Verilator `--lint-only -Wall` if available,
/// otherwise an Icarus elaboration with `-Wall`.
pub fn plan_lint(project: &Project, cfg: &UserConfig) -> Result<Plan, SimError> {
    let files = project.rtl_files()?;
    if files.is_empty() {
        return Err(SimError::NoRtl);
    }
    let workdir = project.stage_dir("lint");
    let top = project.file.project.top.clone();
    let job = if let Some(verilator) = tools::resolve("verilator", cfg) {
        let mut j =
            JobSpec::new("lint", verilator, &workdir).args(["--lint-only", "-Wall", "--timing"]);
        if let Some(t) = &top {
            j = j.args(["--top-module", t]);
        }
        j.args(define_args(project))
            .args(files.iter().map(|f| f.as_os_str().to_owned()))
    } else {
        let iverilog = require("iverilog", cfg)?;
        let mut j =
            JobSpec::new("lint", iverilog, &workdir).args(["-g2012", "-Wall", "-o", "/dev/null"]);
        if let Some(t) = &top {
            j = j.args(["-s", t]);
        }
        j.args(define_args(project))
            .args(files.iter().map(|f| f.as_os_str().to_owned()))
    };
    Ok(Plan {
        stage: "lint".into(),
        workdir,
        jobs: vec![job],
        wave: None,
        verdict_job: None,
    })
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
    pub wave: Option<PathBuf>,
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
) -> Result<Summary, SimError> {
    std::fs::create_dir_all(&plan.workdir)?;
    // A stale waveform from an earlier run must not look like this run's output.
    if let Some(w) = &plan.wave {
        match std::fs::remove_file(w) {
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
        let rec = result.map_err(|source| SimError::Spawn {
            job: spec.name.clone(),
            source,
        })?;
        diagnostics.extend(
            lines
                .iter()
                .filter_map(|l| diag::parse_line(l))
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

    let wave = plan.wave.clone().filter(|w| w.is_file());
    Ok(Summary {
        stage: plan.stage.clone(),
        ok,
        records,
        diagnostics,
        verdict,
        wave,
        hints,
    })
}

/// True if any job in the summary was cancelled.
pub fn was_cancelled(s: &Summary) -> bool {
    s.records.iter().any(|r| r.outcome == Outcome::Cancelled)
}

/// Absolute path of a file relative to the project root, for display.
pub fn rel<'a>(project: &Project, p: &'a Path) -> &'a Path {
    p.strip_prefix(&project.root).unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fpgaflow_core::project::ProjectFile;

    fn project(tb_top: Option<&str>) -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let mut f = ProjectFile::new("t");
        f.sim.tb_top = tb_top.map(String::from);
        f.sources.defines.insert("W".into(), "4".into());
        let p = Project::create(dir.path(), f).unwrap();
        std::fs::write(dir.path().join("rtl/a.sv"), "module a; endmodule\n").unwrap();
        std::fs::write(dir.path().join("tb/tb.sv"), "module tb; a u(); endmodule\n").unwrap();
        (dir, p)
    }

    fn fake_cfg() -> UserConfig {
        // Point every simulator tool at /bin/true so planning doesn't need real tools.
        let mut cfg = UserConfig {
            search_paths: vec![],
            ..Default::default()
        };
        for id in ["iverilog", "vvp", "verilator"] {
            cfg.tool_paths.insert(id.into(), "/bin/true".into());
        }
        cfg
    }

    fn argv(j: &JobSpec) -> Vec<String> {
        j.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn tb_top_is_inferred_when_unique() {
        let (_d, p) = project(None);
        assert_eq!(tb_top(&p).unwrap(), "tb");
        std::fs::write(p.root.join("tb/tb2.sv"), "module tb2; endmodule\n").unwrap();
        assert!(matches!(tb_top(&p), Err(SimError::AmbiguousTbTop(m)) if m == ["tb", "tb2"]));
    }

    #[test]
    fn icarus_plan() {
        let (_d, p) = project(Some("tb"));
        let plan = plan_sim(&p, &fake_cfg(), SimTool::Icarus).unwrap();
        assert_eq!(plan.jobs.len(), 2);
        let c = argv(&plan.jobs[0]);
        assert_eq!(&c[..6], ["-g2012", "-Wall", "-o", "tb.vvp", "-s", "tb"]);
        assert!(c.contains(&"-DW=4".to_string()));
        assert!(c.last().unwrap().ends_with("tb/tb.sv"));
        let r = argv(&plan.jobs[1]);
        assert_eq!(&r[..3], ["-n", "tb.vvp", "-fst"]);
        assert!(r[3].starts_with("+wave=") && r[3].ends_with("sim/icarus/tb.fst"));
        assert_eq!(plan.verdict_job, Some(1));
    }

    #[test]
    fn verilator_plan() {
        let (_d, p) = project(Some("tb"));
        let plan = plan_sim(&p, &fake_cfg(), SimTool::Verilator).unwrap();
        let c = argv(&plan.jobs[0]);
        assert!(c.starts_with(&["--binary".into(), "--timing".into(), "--trace-fst".into()]));
        assert!(plan.jobs[1].program.ends_with("sim/verilator/obj_dir/Vtb"));
    }

    #[test]
    fn missing_tool_is_reported() {
        let (_d, p) = project(Some("tb"));
        let cfg = UserConfig {
            search_paths: vec![],
            tool_paths: [("iverilog".to_string(), "/nonexistent".into())].into(),
            ..Default::default()
        };
        assert!(matches!(
            plan_sim(&p, &cfg, SimTool::Icarus),
            Err(SimError::ToolMissing("iverilog"))
        ));
    }

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
            wave: None,
            verdict_job: Some(1),
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
            wave: None,
            verdict_job: Some(0),
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        let s = execute(&plan, &tx, &CancellationToken::new())
            .await
            .unwrap();
        assert!(s.ok);
        assert_eq!(s.verdict, Some(Verdict::Pass));
    }
}
