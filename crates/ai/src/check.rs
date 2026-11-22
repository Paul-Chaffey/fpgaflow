//! Checking generated files before the user sees them.
//!
//! - HDL: everything must compile with Icarus (`iverilog -g2012`) alongside
//!   the project's existing sources.
//! - A generated testbench must also run, finish within a time limit and
//!   print PASS (the self-checking convention in the system prompt).
//! - A generated UCF may only use pin locations from the board's master
//!   UCF: Claude must never invent a pin.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use fpgaflow_core::job::{self, JobEvent, JobSpec, Outcome};
use fpgaflow_core::project::Project;
use fpgaflow_core::stage::{verdict, Verdict};
use fpgaflow_core::{board, diag, tools, UserConfig};
use regex::Regex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{AiError, Generated};

const COMPILE_TIMEOUT: Duration = Duration::from_secs(60);
/// A generated testbench that hasn't finished by then is reported as hung.
const SIM_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct Report {
    pub ok: bool,
    pub text: String,
}

static UCF_NET_LOC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^\s*NET\s+"?([^"\s]+)"?\s+LOC\s*=\s*"?([A-Z]+[0-9]+)"?"#).unwrap()
});

/// `(net, pin)` pairs with a LOC in UCF text (comments ignored).
pub fn ucf_locs(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .filter_map(|l| {
            UCF_NET_LOC
                .captures(l)
                .map(|c| (c[1].to_string(), c[2].to_uppercase()))
        })
        .collect()
}

/// Problems with a generated UCF against the board's master UCF.
pub fn check_ucf(generated: &str, master: &str) -> Vec<String> {
    let allowed: BTreeSet<String> = ucf_locs(master).into_iter().map(|(_, p)| p).collect();
    let mut problems = Vec::new();
    let mut by_pin: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (net, pin) in ucf_locs(generated) {
        if !allowed.contains(&pin) {
            problems.push(format!(
                "NET \"{net}\" uses pin {pin}, which is not in the board's master UCF"
            ));
        }
        by_pin.entry(pin).or_default().push(net);
    }
    for (pin, nets) in by_pin {
        if nets.len() > 1 {
            problems.push(format!(
                "pin {pin} is assigned to several nets: {}",
                nets.join(", ")
            ));
        }
    }
    problems
}

async fn run(spec: JobSpec, cancel: &CancellationToken) -> Result<(Outcome, Vec<String>), AiError> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let rec = job::run(&spec, &tx, cancel)
        .await
        .map_err(|e| AiError::Other(e.into()))?;
    drop(tx);
    let mut lines = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let JobEvent::Line { text, .. } = ev {
            lines.push(text);
        }
    }
    if rec.outcome == Outcome::Cancelled {
        return Err(AiError::Cancelled);
    }
    Ok((rec.outcome, lines))
}

fn module_of(text: &str) -> Option<String> {
    static M: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^\s*module\s+([A-Za-z_][A-Za-z0-9_$]*)").unwrap());
    M.captures(text).map(|c| c[1].to_string())
}

pub async fn check(
    user: &UserConfig,
    project: &Project,
    g: &Generated,
    cancel: &CancellationToken,
) -> Result<Report, AiError> {
    let mut problems: Vec<String> = Vec::new();
    let dir = project.stage_dir("ai").join("check");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| AiError::Other(e.into()))?;

    // Write the candidate files into the scratch directory.
    let mut gen_rel: BTreeSet<PathBuf> = BTreeSet::new();
    let mut gen_hdl: Vec<PathBuf> = Vec::new();
    let mut gen_tb: Vec<(PathBuf, String)> = Vec::new();
    for f in &g.files {
        let rel = crate::validate_path(&f.path)?;
        let abs = dir.join(&rel);
        if let Some(p) = abs.parent() {
            std::fs::create_dir_all(p).map_err(|e| AiError::Other(e.into()))?;
        }
        std::fs::write(&abs, &f.content).map_err(|e| AiError::Other(e.into()))?;
        gen_rel.insert(rel.clone());
        if rel.extension().is_some_and(|e| e == "ucf") {
            let master = project
                .file
                .target
                .as_ref()
                .and_then(|t| t.board.as_deref())
                .and_then(board::board)
                .and_then(|b| b.ucf_text());
            match master {
                Some(m) => problems.extend(check_ucf(&f.content, m)),
                None => problems.push(format!(
                    "{}: the project has no board, so pins can't be verified",
                    f.path
                )),
            }
        } else if rel.starts_with("tb") {
            gen_tb.push((abs.clone(), f.content.clone()));
            gen_hdl.push(abs);
        } else {
            gen_hdl.push(abs);
        }
    }

    // Existing sources the answer doesn't replace.
    let keep = |files: Vec<PathBuf>| -> Vec<PathBuf> {
        files
            .into_iter()
            .filter(|f| {
                f.strip_prefix(&project.root)
                    .map(|r| !gen_rel.contains(r))
                    .unwrap_or(true)
            })
            .collect()
    };
    let rtl = keep(project.rtl_files().unwrap_or_default());
    let include: Vec<String> = project
        .include_dirs()
        .iter()
        .map(|d| format!("-I{}", d.display()))
        .collect();
    let iverilog = tools::resolve("iverilog", user)
        .ok_or_else(|| AiError::Other(anyhow::anyhow!("iverilog not installed")))?;
    let vvp = tools::resolve("vvp", user)
        .ok_or_else(|| AiError::Other(anyhow::anyhow!("vvp not installed")))?;

    if !gen_hdl.is_empty() {
        let rtl_gen: Vec<&PathBuf> = gen_hdl
            .iter()
            .filter(|p| !gen_tb.iter().any(|(t, _)| t == *p))
            .collect();
        // 1. RTL compiles (existing RTL + generated RTL).
        let spec = JobSpec::new("ai:compile", &iverilog, &dir)
            .args(["-g2012", "-o", "rtl.vvp"])
            .args(include.clone())
            .args(rtl.iter().map(|p| p.as_os_str().to_owned()))
            .args(rtl_gen.iter().map(|p| p.as_os_str().to_owned()))
            .timeout(COMPILE_TIMEOUT);
        let (outcome, lines) = run(spec, cancel).await?;
        if !outcome.success() {
            problems.push(format!("RTL does not compile with Icarus ({outcome}):"));
            problems.extend(diag_lines(&lines, &dir, project));
        }

        // 2. Each generated testbench compiles, runs and passes.
        for (tb, text) in &gen_tb {
            let top = module_of(text).unwrap_or_else(|| "tb".into());
            let vvp_file = format!("{top}.vvp");
            let spec = JobSpec::new("ai:tb-compile", &iverilog, &dir)
                .args(["-g2012", "-o", &vvp_file, "-s", &top])
                .args(include.clone())
                .args(rtl.iter().map(|p| p.as_os_str().to_owned()))
                .args(rtl_gen.iter().map(|p| p.as_os_str().to_owned()))
                .arg(tb)
                .timeout(COMPILE_TIMEOUT);
            let (outcome, lines) = run(spec, cancel).await?;
            if !outcome.success() {
                problems.push(format!("testbench {top} does not compile ({outcome}):"));
                problems.extend(diag_lines(&lines, &dir, project));
                continue;
            }
            let wave = dir.join(format!("{top}.fst"));
            let spec = JobSpec::new("ai:tb-run", &vvp, &dir)
                .args(["-n", &vvp_file, "-fst"])
                .arg(format!("+wave={}", wave.display()))
                .timeout(SIM_TIMEOUT);
            let (outcome, lines) = run(spec, cancel).await?;
            let v = verdict(lines.iter().map(String::as_str));
            match (outcome, v) {
                (Outcome::TimedOut, _) => problems.push(format!(
                    "testbench {top} did not finish within {}s (missing $finish or watchdog?)",
                    SIM_TIMEOUT.as_secs()
                )),
                (_, Verdict::Pass) if outcome.success() => {}
                (_, Verdict::Unknown) if outcome.success() => problems.push(format!(
                    "testbench {top} ran but printed no \"PASS: {top}\" line"
                )),
                _ => {
                    problems.push(format!("testbench {top} failed ({outcome}). Last output:"));
                    let tail: Vec<&String> = lines.iter().rev().take(25).collect();
                    problems.extend(tail.into_iter().rev().cloned());
                }
            }
        }
    }

    let ok = problems.is_empty();
    let text = if ok {
        "all checks passed".to_string()
    } else {
        problems.join("\n")
    };
    Ok(Report { ok, text })
}

/// Tool output lines as diagnostics with project-relative paths.
fn diag_lines(lines: &[String], cwd: &Path, project: &Project) -> Vec<String> {
    let d = diag::parse_lines(lines);
    if d.is_empty() {
        return lines.iter().rev().take(20).rev().cloned().collect();
    }
    d.into_iter()
        .map(|d| d.resolve(cwd))
        .map(|mut d| {
            if let Ok(r) = d.file.strip_prefix(cwd) {
                d.file = r.to_path_buf();
            } else if let Ok(r) = d.file.strip_prefix(&project.root) {
                d.file = r.to_path_buf();
            }
            d.to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: &str = include_str!("../../../data/boards/s3an-starter.ucf");

    #[test]
    fn ucf_pins_must_come_from_the_master() {
        let good =
            "NET \"clk\" LOC = \"E12\" | IOSTANDARD = LVCMOS33 ;\nNET \"led\" LOC = \"R20\" ;";
        assert!(check_ucf(good, MASTER).is_empty());
        let invented = "NET \"led\" LOC = \"A1\" ;";
        assert_eq!(check_ucf(invented, MASTER).len(), 1);
        let dup = "NET \"a\" LOC = \"R20\" ;\nNET \"b\" LOC = \"R20\" ;";
        assert!(check_ucf(dup, MASTER)[0].contains("several nets"));
        assert!(
            check_ucf("# NET \"x\" LOC = \"A1\";", MASTER).is_empty(),
            "comments ignored"
        );
    }

    fn user() -> UserConfig {
        UserConfig::default()
    }

    fn project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let p = Project::create(dir.path(), fpgaflow_core::project::ProjectFile::new("t")).unwrap();
        (dir, p)
    }

    fn gfile(path: &str, content: &str) -> crate::GenFile {
        crate::GenFile {
            path: path.into(),
            content: content.into(),
            purpose: String::new(),
        }
    }

    /// Runs the real Icarus if it is installed (skipped otherwise).
    #[tokio::test]
    async fn real_icarus_check_of_good_and_bad_code() {
        if tools::resolve("iverilog", &user()).is_none() {
            return;
        }
        let (_d, p) = project();
        let c = CancellationToken::new();
        let good = Generated {
            files: vec![
                gfile("rtl/inv.sv", "`timescale 1ns/1ps\nmodule inv(input logic a, output logic y); assign y = ~a; endmodule\n"),
                gfile(
                    "tb/tb_inv.sv",
                    "`timescale 1ns/1ps\nmodule tb_inv; logic a, y; inv dut(.a(a), .y(y));\ninitial begin a = 0; #1; if (y !== 1) $display(\"FAIL: y\"); else $display(\"PASS: tb_inv\"); $finish; end\nendmodule\n",
                ),
            ],
            summary: String::new(),
        };
        let r = check(&user(), &p, &good, &c).await.unwrap();
        assert!(r.ok, "{}", r.text);

        let bad = Generated {
            files: vec![gfile(
                "rtl/bad.sv",
                "module bad(input a output y); endmodule\n",
            )],
            summary: String::new(),
        };
        let r = check(&user(), &p, &bad, &c).await.unwrap();
        assert!(!r.ok);
        assert!(r.text.contains("does not compile"), "{}", r.text);
    }
}
