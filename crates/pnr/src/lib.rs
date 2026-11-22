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

#[cfg(test)]
mod tests {
    use super::*;

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
