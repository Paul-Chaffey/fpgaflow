//! GTKWave launching and default save files.
//!
//! Layouts live in `waves/<tb_top>.<simulator>.gtkw` in the project, one per
//! simulator because each simulator's dump is a different file. If no layout
//! exists yet, a default one
//! listing the testbench's top-level signals is generated; once you save a
//! layout from GTKWave, it is reused and never overwritten.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use fpgaflow_core::project::{Project, SimTool};
use fpgaflow_hdl::scan::{self, Signal};

pub fn save_file(project: &Project, tb_top: &str, tool: SimTool) -> PathBuf {
    let tool = match tool {
        SimTool::Icarus => "icarus",
        SimTool::Verilator => "verilator",
    };
    project
        .root
        .join("waves")
        .join(format!("{tb_top}.{tool}.gtkw"))
}

/// Hierarchical prefix of the testbench top in each simulator's dump.
/// Both Icarus 14.0 (devel) and Verilator 5.055 (`--binary`) put the testbench at the
/// root scope (checked against real dumps), so this is just the module name.
pub fn scope_prefix(tb_top: &str, _tool: SimTool) -> String {
    tb_top.to_string()
}

/// Text of a GTKWave save file showing `signals` under `scope`.
pub fn default_layout(wave: &Path, scope: &str, signals: &[Signal]) -> String {
    let mut s = format!("[dumpfile] \"{}\"\n[timestart] 0\n", wave.display());
    for sig in signals {
        match sig.range {
            None => s.push_str(&format!("@28\n{scope}.{}\n", sig.name)),
            Some((m, l)) => s.push_str(&format!("@22\n{scope}.{}[{m}:{l}]\n", sig.name)),
        }
    }
    s
}

/// Make sure a save file exists for this testbench and simulator; returns its path.
pub fn ensure_layout(
    project: &Project,
    tb_top: &str,
    tool: SimTool,
    wave: &Path,
) -> std::io::Result<PathBuf> {
    let path = save_file(project, tb_top, tool);
    if path.exists() {
        return Ok(path);
    }
    let mut signals = Vec::new();
    for f in project.tb_files().map_err(std::io::Error::other)? {
        let src = std::fs::read_to_string(&f)?;
        if scan::declared_modules(&src).iter().any(|m| m == tb_top) {
            signals = scan::module_signals(&src, tb_top);
            break;
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        default_layout(wave, &scope_prefix(tb_top, tool), &signals),
    )?;
    Ok(path)
}

/// Open `wave` in GTKWave with `layout`, detached from fpgaflow.
pub fn open(gtkwave: &Path, wave: &Path, layout: Option<&Path>) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(gtkwave);
    cmd.arg(wave);
    if let Some(l) = layout {
        cmd.arg(l);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_text() {
        let sigs = [
            Signal {
                name: "clk".into(),
                range: None,
            },
            Signal {
                name: "data".into(),
                range: Some((7, 0)),
            },
        ];
        let t = default_layout(Path::new("/b/tb.fst"), "tb", &sigs);
        assert_eq!(
            t,
            "[dumpfile] \"/b/tb.fst\"\n[timestart] 0\n@28\ntb.clk\n@22\ntb.data[7:0]\n"
        );
    }
}
