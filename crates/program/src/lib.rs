//! Loading bitstreams onto boards: openFPGALoader over a JTAG cable, or
//! `fpgaflow-celoxica` over a Celoxica RC board's own USB port.
//!
//! The command line comes from the board profile (`data/boards.toml`):
//! method, cable, FX2 firmware, JTAG clock and chain position were each
//! found on real hardware, so they live in data rather than in code.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fpgaflow_core::board::{self, Board, ProgramMethod, ProgramProfile};
use fpgaflow_core::job::JobSpec;
use fpgaflow_core::project::{ProgramMode, Project};
use fpgaflow_core::stage::Plan;
use fpgaflow_core::{tools, UserConfig};

/// Upper bound for a load; a 700AN SRAM load takes a few seconds at 750 kHz.
const PROGRAM_TIMEOUT: Duration = Duration::from_secs(300);
const DETECT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum ProgramError {
    #[error("no board: set [target] board in fpgaflow.toml (see `fpgaflow boards`)")]
    NoBoard,
    #[error("unknown board {0:?} (see `fpgaflow boards`)")]
    UnknownBoard(String),
    #[error("openFPGALoader is not installed (run `fpgaflow doctor`)")]
    NoLoader,
    #[error("no bitstream at {0}; run `fpgaflow build` first")]
    NoBitstream(PathBuf),
    #[error("cable firmware {0} not found (it ships with ISE 14.7; run `fpgaflow doctor`)")]
    NoFirmware(String),
    #[error("flash programming is not supported for {0} yet; use [program] mode = \"sram\"")]
    FlashNotSupported(String),
    #[error("no JTAG cable is set up for the {0} yet (see its notes in data/boards.toml)")]
    NoProgrammer(String),
    #[error(
        "fpgaflow-celoxica is not installed (it is built with fpgaflow; run `fpgaflow doctor`)"
    )]
    NoCeloxicaTool,
}

pub fn project_board(project: &Project) -> Result<&'static Board, ProgramError> {
    let id = project
        .file
        .target
        .as_ref()
        .and_then(|t| t.board.clone())
        .ok_or(ProgramError::NoBoard)?;
    board::board(&id).ok_or(ProgramError::UnknownBoard(id))
}

/// Resolve a profile firmware reference (`ise:<file>` → ISE's `bin/lin64/<file>`).
pub fn resolve_firmware(spec: &str, cfg: &UserConfig) -> Result<PathBuf, ProgramError> {
    let path = match spec.strip_prefix("ise:") {
        Some(file) => tools::resolve("ise-ngdbuild", cfg)
            .and_then(|ngd| ngd.parent().map(|d| d.join(file)))
            .ok_or_else(|| ProgramError::NoFirmware(spec.to_string()))?,
        None => PathBuf::from(spec),
    };
    if path.is_file() {
        Ok(path)
    } else {
        Err(ProgramError::NoFirmware(path.display().to_string()))
    }
}

fn profile(b: &Board) -> Result<&ProgramProfile, ProgramError> {
    b.program
        .as_ref()
        .ok_or_else(|| ProgramError::NoProgrammer(b.name.clone()))
}

fn celoxica(cfg: &UserConfig) -> Result<PathBuf, ProgramError> {
    tools::resolve("fpgaflow-celoxica", cfg).ok_or(ProgramError::NoCeloxicaTool)
}

fn cable_args(b: &Board, cfg: &UserConfig) -> Result<Vec<String>, ProgramError> {
    let p = profile(b)?;
    let cable = p
        .cable
        .clone()
        .ok_or_else(|| ProgramError::NoProgrammer(b.name.clone()))?;
    let mut a = vec!["-c".to_string(), cable];
    if let Some(fw) = &p.firmware {
        a.push("--probe-firmware".into());
        a.push(resolve_firmware(fw, cfg)?.display().to_string());
    }
    if let Some(f) = p.freq {
        a.push("--freq".into());
        a.push(f.to_string());
    }
    if let Some(i) = p.index_chain {
        a.push("--index-chain".into());
        a.push(i.to_string());
    }
    Ok(a)
}

fn loader(cfg: &UserConfig) -> Result<PathBuf, ProgramError> {
    tools::resolve("openFPGALoader", cfg).ok_or(ProgramError::NoLoader)
}

/// The bitstream `fpgaflow build` writes for this project.
pub fn default_bitstream(project: &Project) -> PathBuf {
    let top = project
        .file
        .project
        .top
        .clone()
        .unwrap_or_else(|| project.file.project.name.clone());
    project.stage_dir("impl").join(format!("{top}.bit"))
}

/// Plan loading `bit` onto the project's board.
pub fn plan_program(project: &Project, cfg: &UserConfig, bit: &Path) -> Result<Plan, ProgramError> {
    let b = project_board(project)?;
    if project.file.program.mode == ProgramMode::Flash {
        // The 3AN's in-system flash needs a different protocol than an SRAM load.
        return Err(ProgramError::FlashNotSupported(b.device.clone()));
    }
    if !bit.is_file() {
        return Err(ProgramError::NoBitstream(bit.to_path_buf()));
    }
    let workdir = project.stage_dir("program");
    let job = match profile(b)?.method {
        ProgramMethod::Openfpgaloader => JobSpec::new("program", loader(cfg)?, &workdir)
            .args(cable_args(b, cfg)?)
            .arg(bit),
        ProgramMethod::Celoxica => JobSpec::new("program", celoxica(cfg)?, &workdir)
            .arg("load")
            .arg(bit),
    }
    .timeout(PROGRAM_TIMEOUT);
    Ok(Plan {
        stage: "program".into(),
        workdir,
        jobs: vec![job],
        outputs: vec![],
        verdict_job: None,
        files: vec![],
    })
}

/// Plan a read-only JTAG scan (`--detect`) of a board.
pub fn plan_detect(b: &Board, cfg: &UserConfig, workdir: &Path) -> Result<Plan, ProgramError> {
    let job = match profile(b)?.method {
        ProgramMethod::Openfpgaloader => {
            let mut args = cable_args(b, cfg)?;
            // --detect lists the whole chain; the chain index only matters for loading.
            if let Some(i) = args.iter().position(|a| a == "--index-chain") {
                args.drain(i..i + 2);
            }
            JobSpec::new("detect", loader(cfg)?, workdir)
                .args(args)
                .arg("--detect")
        }
        ProgramMethod::Celoxica => JobSpec::new("detect", celoxica(cfg)?, workdir).arg("status"),
    }
    .timeout(DETECT_TIMEOUT);
    Ok(Plan {
        stage: "detect".into(),
        workdir: workdir.to_path_buf(),
        jobs: vec![job],
        outputs: vec![],
        verdict_job: None,
        files: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fpgaflow_core::project::{ProjectFile, TargetSection};

    fn exe(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, "").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn setup(mode: ProgramMode) -> (tempfile::TempDir, Project, UserConfig) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut f = ProjectFile::new("blinky");
        f.project.top = Some("blinky".into());
        f.target = Some(TargetSection {
            board: Some("s3an-starter".into()),
            ..Default::default()
        });
        f.program.mode = mode;
        let p = Project::create(root, f).unwrap();
        let bin = root.join("ise/bin/lin64");
        std::fs::create_dir_all(&bin).unwrap();
        exe(&bin.join("ngdbuild"));
        std::fs::write(bin.join("xusb_emb.hex"), ":00000001FF\n").unwrap();
        exe(&root.join("openFPGALoader"));
        let mut cfg = UserConfig {
            search_paths: vec![],
            ..Default::default()
        };
        cfg.tool_paths
            .insert("ise-ngdbuild".into(), bin.join("ngdbuild"));
        cfg.tool_paths
            .insert("openFPGALoader".into(), root.join("openFPGALoader"));
        (dir, p, cfg)
    }

    fn args(j: &JobSpec) -> Vec<String> {
        j.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn program_command_line_comes_from_the_board_profile() {
        let (dir, p, cfg) = setup(ProgramMode::Sram);
        let bit = default_bitstream(&p);
        assert!(matches!(
            plan_program(&p, &cfg, &bit),
            Err(ProgramError::NoBitstream(_))
        ));
        std::fs::create_dir_all(bit.parent().unwrap()).unwrap();
        std::fs::write(&bit, "x").unwrap();
        let plan = plan_program(&p, &cfg, &bit).unwrap();
        let fw = dir
            .path()
            .join("ise/bin/lin64/xusb_emb.hex")
            .display()
            .to_string();
        let bit_s = bit.display().to_string();
        assert_eq!(
            args(&plan.jobs[0]),
            [
                "-c",
                "xilinxPlatformCableUsb_alt",
                "--probe-firmware",
                fw.as_str(),
                "--freq",
                "750000",
                "--index-chain",
                "0",
                bit_s.as_str(),
            ]
        );
    }

    #[test]
    fn detect_drops_the_chain_index() {
        let (dir, p, cfg) = setup(ProgramMode::Sram);
        let b = project_board(&p).unwrap();
        let plan = plan_detect(b, &cfg, dir.path()).unwrap();
        let a = args(&plan.jobs[0]);
        assert!(!a.contains(&"--index-chain".to_string()));
        assert_eq!(a.last().unwrap(), "--detect");
    }

    #[test]
    fn flash_mode_is_refused_for_now() {
        let (_d, p, cfg) = setup(ProgramMode::Flash);
        assert!(matches!(
            plan_program(&p, &cfg, Path::new("/x.bit")),
            Err(ProgramError::FlashNotSupported(_))
        ));
    }
}
