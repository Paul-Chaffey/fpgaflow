//! Registry of the external tools fpgaflow drives, and the "Doctor" that
//! finds them and reports their versions.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Serialize;
use wait_timeout::ChildExt;

use crate::config::UserConfig;

/// Upper bound for a `--version` probe. Real tools answer in milliseconds;
/// this only stops a misbehaving binary from hanging the Doctor.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Stage {
    Simulation,
    Waves,
    Synthesis,
    PlaceRoute,
    Bitstream,
    Program,
    Assistant,
}

impl Stage {
    /// Display order.
    pub const ALL: [Stage; 7] = [
        Stage::Simulation,
        Stage::Waves,
        Stage::Synthesis,
        Stage::PlaceRoute,
        Stage::Bitstream,
        Stage::Program,
        Stage::Assistant,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Stage::Simulation => "Simulation",
            Stage::Waves => "Waveforms",
            Stage::Synthesis => "Synthesis",
            Stage::PlaceRoute => "Place & route",
            Stage::Bitstream => "Bitstream",
            Stage::Program => "Programming",
            Stage::Assistant => "Assistant",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ToolSpec {
    /// Stable id used in config `tool_paths` and by the flow crates.
    pub id: &'static str,
    /// Executable name searched for.
    pub exe: &'static str,
    /// Arguments that print a version and exit. Empty: don't probe
    /// (some tools have no such flag or would wait on stdin).
    pub version_args: &'static [&'static str],
    pub stage: Stage,
    pub purpose: &'static str,
}

const fn t(
    id: &'static str,
    exe: &'static str,
    version_args: &'static [&'static str],
    stage: Stage,
    purpose: &'static str,
) -> ToolSpec {
    ToolSpec {
        id,
        exe,
        version_args,
        stage,
        purpose,
    }
}

/// Every tool the flow knows about. Only those needed for the selected
/// target and simulator are required at run time.
pub const TOOLS: &[ToolSpec] = &[
    t(
        "iverilog",
        "iverilog",
        &["-V"],
        Stage::Simulation,
        "Icarus Verilog compiler",
    ),
    t(
        "vvp",
        "vvp",
        &["-V"],
        Stage::Simulation,
        "Icarus Verilog runtime",
    ),
    t(
        "verilator",
        "verilator",
        &["--version"],
        Stage::Simulation,
        "Verilator simulator / linter",
    ),
    t(
        "gtkwave",
        "gtkwave",
        &["--version"],
        Stage::Waves,
        "Waveform viewer",
    ),
    t("yosys", "yosys", &["-V"], Stage::Synthesis, "Synthesis"),
    t(
        "vpr",
        "vpr",
        &["--version"],
        Stage::PlaceRoute,
        "VTR place & route",
    ),
    t(
        "nextpnr-ice40",
        "nextpnr-ice40",
        &["--version"],
        Stage::PlaceRoute,
        "P&R for Lattice iCE40",
    ),
    t(
        "nextpnr-ecp5",
        "nextpnr-ecp5",
        &["--version"],
        Stage::PlaceRoute,
        "P&R for Lattice ECP5",
    ),
    t(
        "nextpnr-himbaechel",
        "nextpnr-himbaechel",
        &["--version"],
        Stage::PlaceRoute,
        "P&R for Gowin and others",
    ),
    t(
        "nextpnr-xilinx",
        "nextpnr-xilinx",
        &["--version"],
        Stage::PlaceRoute,
        "P&R for Xilinx 7-series (openXC7)",
    ),
    t(
        "icepack",
        "icepack",
        &[],
        Stage::Bitstream,
        "iCE40 bitstream packer",
    ),
    t(
        "ecppack",
        "ecppack",
        &[],
        Stage::Bitstream,
        "ECP5 bitstream packer",
    ),
    t(
        "gowin_pack",
        "gowin_pack",
        &[],
        Stage::Bitstream,
        "Gowin bitstream packer",
    ),
    t(
        "ise-ngdbuild",
        "ngdbuild",
        &[],
        Stage::PlaceRoute,
        "Xilinx ISE 14.7: netlist translate",
    ),
    t(
        "ise-map",
        "map",
        &[],
        Stage::PlaceRoute,
        "Xilinx ISE 14.7: map",
    ),
    t(
        "ise-par",
        "par",
        &[],
        Stage::PlaceRoute,
        "Xilinx ISE 14.7: place & route",
    ),
    t(
        "ise-bitgen",
        "bitgen",
        &[],
        Stage::Bitstream,
        "Xilinx ISE 14.7: bitstream",
    ),
    t(
        "ise-impact",
        "impact",
        &[],
        Stage::Program,
        "Xilinx ISE 14.7: iMPACT programmer",
    ),
    t(
        "openFPGALoader",
        "openFPGALoader",
        &["--Version"],
        Stage::Program,
        "Universal FPGA programmer",
    ),
    t(
        "claude",
        "claude",
        &["--version"],
        Stage::Assistant,
        "Claude Code CLI",
    ),
];

pub fn spec(id: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|t| t.id == id)
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolStatus {
    pub spec: ToolSpec,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
}

impl ToolStatus {
    pub fn found(&self) -> bool {
        self.path.is_some()
    }
}

/// Resolve a tool: explicit config path, then `search_paths`, then `$PATH`.
pub fn locate(spec: &ToolSpec, cfg: &UserConfig) -> Option<PathBuf> {
    if let Some(p) = cfg.tool_paths.get(spec.id) {
        return is_executable(p).then(|| p.clone());
    }
    cfg.search_paths
        .iter()
        .map(|dir| dir.join(spec.exe))
        .find(|p| is_executable(p))
        .or_else(|| which::which(spec.exe).ok())
}

/// Like [`locate`] but by id; the flow crates use this to get a runnable path.
pub fn resolve(id: &str, cfg: &UserConfig) -> Option<PathBuf> {
    spec(id).and_then(|s| locate(s, cfg))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Run `exe args`, wait at most `timeout`, return the first non-empty output line.
pub fn probe_version(exe: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    if child.wait_timeout(timeout).ok()?.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut err);
    }
    out.lines()
        .chain(err.lines())
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(120).collect())
}

/// Locate and version-probe every known tool, in parallel.
pub fn discover(cfg: &UserConfig) -> Vec<ToolStatus> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = TOOLS
            .iter()
            .map(|spec| {
                scope.spawn(move || {
                    let path = locate(spec, cfg);
                    let version = match (&path, spec.version_args.is_empty()) {
                        (Some(p), false) => probe_version(p, spec.version_args, PROBE_TIMEOUT),
                        _ => None,
                    };
                    ToolStatus {
                        spec: *spec,
                        path,
                        version,
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("probe thread panicked"))
            .collect()
    })
}

/// A check on the host rather than on one tool.
#[derive(Debug, Clone, Serialize)]
pub struct HostCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
    /// What the user should do if `ok` is false.
    pub fix: Option<String>,
}

/// USB-access prerequisites for programming boards without root.
pub fn host_checks() -> Vec<HostCheck> {
    let groups = Command::new("id")
        .arg("-nG")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let in_plugdev = groups.split_whitespace().any(|g| g == "plugdev");

    let rules: Vec<String> = [
        "/etc/udev/rules.d",
        "/usr/lib/udev/rules.d",
        "/lib/udev/rules.d",
    ]
    .iter()
    .filter_map(|d| std::fs::read_dir(d).ok())
    .flatten()
    .filter_map(Result::ok)
    .map(|e| e.file_name().to_string_lossy().into_owned())
    .filter(|n| n.to_lowercase().contains("openfpgaloader"))
    .collect();

    vec![
        HostCheck {
            name: "plugdev group",
            ok: in_plugdev,
            detail: if in_plugdev {
                "user is in plugdev".into()
            } else {
                "user is not in plugdev".into()
            },
            fix: (!in_plugdev)
                .then(|| "sudo usermod -aG plugdev $USER   (then log out and in)".into()),
        },
        HostCheck {
            name: "openFPGALoader udev rules",
            ok: !rules.is_empty(),
            detail: if rules.is_empty() {
                "no openFPGALoader rules installed".into()
            } else {
                rules.join(", ")
            },
            fix: rules.is_empty().then(|| {
                "sudo cp <openFPGALoader>/99-openfpgaloader.rules /etc/udev/rules.d/ && \
                 sudo udevadm control --reload-rules && sudo udevadm trigger"
                    .into()
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<_> = TOOLS.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len());
    }

    #[test]
    fn config_override_wins_and_must_be_executable() {
        let mut cfg = UserConfig {
            search_paths: vec![],
            ..Default::default()
        };
        cfg.tool_paths.insert("yosys".into(), "/bin/sh".into());
        assert_eq!(resolve("yosys", &cfg), Some(PathBuf::from("/bin/sh")));
        cfg.tool_paths
            .insert("yosys".into(), "/nonexistent/yosys".into());
        assert_eq!(resolve("yosys", &cfg), None);
    }

    #[test]
    fn probe_reads_first_line_and_respects_timeout() {
        let v = probe_version(
            Path::new("/bin/sh"),
            &["-c", "echo; echo ' tool 1.2 '; echo x"],
            PROBE_TIMEOUT,
        );
        assert_eq!(v.as_deref(), Some("tool 1.2"));
        let start = std::time::Instant::now();
        let v = probe_version(
            Path::new("/bin/sh"),
            &["-c", "sleep 30"],
            Duration::from_millis(200),
        );
        assert!(v.is_none());
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
