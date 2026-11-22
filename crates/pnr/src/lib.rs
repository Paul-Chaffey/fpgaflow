//! Implementation: place & route, timing, and bitstream generation.
//!
//! Implementation is split into two plans so a safety check can sit between
//! them: [`ise::plan_implement`] runs translate/map/par/timing, then the pad
//! report is checked for any user pin the tools placed on their own
//! (an unconstrained pin can drive a net that is wired to something else on
//! the board), and only then [`ise::plan_bitstream`] writes the `.bit`.
//!
//! Backends so far: ISE 14.7. nextpnr and VPR come next.

pub mod constraints;
pub mod ise;

use std::path::PathBuf;

use fpgaflow_core::project::{Project, TargetSection};

#[derive(Debug, thiserror::Error)]
pub enum PnrError {
    #[error("backend {0:?} is not implemented yet (ISE is; nextpnr and VPR are next)")]
    NotImplemented(String),
    #[error("[target] needs {0} for this backend (e.g. device = \"xc3s700an\", package = \"fgg484\", speed = \"4\")")]
    MissingTarget(&'static str),
    #[error(
        "ISE 14.7 not found (run `fpgaflow doctor`; expected .../ISE_DS/ISE/bin/lin64/ngdbuild)"
    )]
    NoIse,
    #[error("no constraint file: list one in [target] constraints (a bitstream with tool-chosen pins is unsafe)")]
    NoConstraints,
    #[error(
        "{} top-level port bit(s) have no LOC constraint: {}; ISE would pick those pins itself",
        .0.len(),
        .0.join(", ")
    )]
    Unconstrained(Vec<String>),
    #[error(
        "the constraints put {} signal(s) on pins the {board} reserves; they can never be \
         used: {}",
        .uses.len(),
        .uses.join("; ")
    )]
    ReservedPins { board: String, uses: Vec<String> },
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Full part name for vendor tools: `xc3s700an-4-fgg484`.
pub fn part_name(t: &TargetSection) -> Result<String, PnrError> {
    let device = t
        .device
        .as_deref()
        .ok_or(PnrError::MissingTarget("device"))?;
    let speed = t.speed.as_deref().ok_or(PnrError::MissingTarget("speed"))?;
    let package = t
        .package
        .as_deref()
        .ok_or(PnrError::MissingTarget("package"))?;
    Ok(format!("{device}-{speed}-{package}"))
}

/// The project's constraint files, as absolute paths, in order.
pub fn constraint_files(project: &Project) -> Vec<PathBuf> {
    project
        .file
        .target
        .as_ref()
        .map(|t| t.constraints.iter().map(|c| project.root.join(c)).collect())
        .unwrap_or_default()
}

/// The project's board profile, if `[target] board` names a known one.
pub fn project_board(project: &Project) -> Option<&'static fpgaflow_core::board::Board> {
    project
        .file
        .target
        .as_ref()
        .and_then(|t| t.board.as_deref())
        .and_then(fpgaflow_core::board::board)
}

/// Refuse constraints that `LOC` a signal onto a pin the board reserves
/// (e.g. an I/O pin wired straight to a supply rail).
pub fn check_reserved_pins(project: &Project) -> Result<(), PnrError> {
    let Some(b) = project_board(project) else {
        return Ok(());
    };
    let mut uses = Vec::new();
    for f in constraint_files(project) {
        let text = std::fs::read_to_string(&f).map_err(|source| PnrError::Io {
            path: f.clone(),
            source,
        })?;
        for l in constraints::ucf_pin_locs(&text) {
            if let Some(why) = b.reserved_reason(&l.pin) {
                uses.push(format!(
                    "{} on {} at {}:{} ({why})",
                    l.net,
                    l.pin,
                    f.strip_prefix(&project.root).unwrap_or(&f).display(),
                    l.line
                ));
            }
        }
    }
    if uses.is_empty() {
        Ok(())
    } else {
        Err(PnrError::ReservedPins {
            board: b.name.clone(),
            uses,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_pins_are_refused_with_file_and_line() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = fpgaflow_core::project::ProjectFile::new("t");
        f.target = Some(TargetSection {
            board: Some("rc10".into()),
            constraints: vec!["c.ucf".into()],
            ..Default::default()
        });
        let p = Project::create(dir.path(), f).unwrap();
        std::fs::write(dir.path().join("c.ucf"), "NET \"led\" LOC = \"D2\";\n").unwrap();
        assert!(check_reserved_pins(&p).is_ok());
        std::fs::write(
            dir.path().join("c.ucf"),
            "NET \"led\" LOC = \"D2\";\nNET \"x\" LOC = \"K6\";\n",
        )
        .unwrap();
        let e = check_reserved_pins(&p).unwrap_err().to_string();
        assert!(e.contains("x on K6 at c.ucf:2 (wired to +2.5 V)"), "{e}");
        assert!(!e.contains("D2"), "{e}");
    }

    #[test]
    fn part_name_needs_all_three() {
        let mut t = TargetSection {
            device: Some("xc3s700an".into()),
            speed: Some("4".into()),
            package: Some("fgg484".into()),
            ..Default::default()
        };
        assert_eq!(part_name(&t).unwrap(), "xc3s700an-4-fgg484");
        t.speed = None;
        assert!(matches!(
            part_name(&t),
            Err(PnrError::MissingTarget("speed"))
        ));
    }
}
