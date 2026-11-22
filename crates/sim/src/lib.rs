//! Simulation and lint stages: planning only. Running a plan is
//! [`fpgaflow_core::stage::execute`], shared with every other stage.

pub mod gtkwave;

use std::path::{Path, PathBuf};

use fpgaflow_core::job::JobSpec;
use fpgaflow_core::project::{Project, ProjectError, SimTool};
pub use fpgaflow_core::stage::{execute, verdict, Plan, Summary, Verdict};
use fpgaflow_core::tools;
use fpgaflow_core::UserConfig;
use fpgaflow_hdl::scan;

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
    #[error(transparent)]
    Io(#[from] std::io::Error),
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
        outputs: vec![wave],
        verdict_job,
        files: vec![],
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
        outputs: vec![],
        verdict_job: None,
        files: vec![],
    })
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
}
