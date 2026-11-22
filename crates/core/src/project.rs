//! The project model: `fpgaflow.toml` and the source files it names.
//!
//! The front end accepts only Verilog and SystemVerilog. Any source pattern
//! that matches a file with another extension (e.g. `.vhd`) is an error, not
//! a silent skip, so a mis-typed glob is caught immediately.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// File name of the project manifest.
pub const PROJECT_FILE: &str = "fpgaflow.toml";

/// Extensions the front end accepts.
pub const HDL_EXTENSIONS: &[&str] = &["v", "sv", "vh", "svh"];

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("no fpgaflow.toml found in {0} or any parent directory")]
    NotFound(PathBuf),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("cannot serialise project: {0}")]
    Serialise(#[from] toml::ser::Error),
    #[error("bad source pattern {pattern:?}: {source}")]
    Pattern {
        pattern: String,
        #[source]
        source: glob::PatternError,
    },
    #[error("{0} is not a Verilog/SystemVerilog file (accepted: .v .sv .vh .svh)")]
    NotHdl(PathBuf),
    #[error("{0} already exists")]
    AlreadyExists(PathBuf),
}

/// Contents of `fpgaflow.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectFile {
    pub project: ProjectSection,
    #[serde(default)]
    pub sources: Sources,
    #[serde(default)]
    pub sim: SimSection,
    #[serde(default, skip_serializing_if = "SynthSection::is_default")]
    pub synth: SynthSection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetSection>,
    #[serde(default)]
    pub program: ProgramSection,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectSection {
    pub name: String,
    /// Synthesis top module. `None` means "let Yosys pick" (`hierarchy -auto-top`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Sources {
    /// Glob patterns, relative to the project root, for synthesisable RTL.
    pub rtl: Vec<String>,
    /// Glob patterns for simulation-only testbench files.
    pub tb: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub include_dirs: Vec<PathBuf>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub defines: BTreeMap<String, String>,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            rtl: vec!["rtl/**/*.v".into(), "rtl/**/*.sv".into()],
            tb: vec!["tb/**/*.v".into(), "tb/**/*.sv".into()],
            include_dirs: Vec::new(),
            defines: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SimTool {
    #[default]
    Icarus,
    Verilator,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum WaveFormat {
    #[default]
    Fst,
    Vcd,
}

impl WaveFormat {
    pub fn extension(self) -> &'static str {
        match self {
            WaveFormat::Fst => "fst",
            WaveFormat::Vcd => "vcd",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SimSection {
    pub tool: SimTool,
    /// Testbench top module. `None` means "the only top-level module in the tb files".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tb_top: Option<String>,
    pub wave: WaveFormat,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Frontend {
    /// Yosys' built-in `read_verilog -sv` (a SystemVerilog subset).
    #[default]
    Verilog,
    /// The yosys-slang plugin (`read_slang`): full SystemVerilog.
    Slang,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SynthSection {
    pub frontend: Frontend,
    /// Replace the family's synthesis command with the commands in this
    /// Yosys script (relative to the project root). Reading the sources,
    /// `hierarchy`, `stat` and writing the netlist are still done for you.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<PathBuf>,
    /// Extra Yosys commands run after synthesis, before the netlist is written.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<String>,
    /// Keep the hierarchy instead of flattening (where the family allows it).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub keep_hierarchy: bool,
}

impl SynthSection {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct TargetSection {
    /// Board profile name; fills in family/device/package when those are absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<String>,
    /// Place-and-route backend: `nextpnr`, `vpr`, `ise`, `vivado`, `quartus`, ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    pub constraints: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProgramMode {
    /// Volatile load into configuration RAM.
    #[default]
    Sram,
    /// Write the configuration flash.
    Flash,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ProgramSection {
    pub mode: ProgramMode,
    /// openFPGALoader cable name, when the board profile's default is wrong.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cable: Option<String>,
}

impl ProjectFile {
    /// A fresh manifest with default source layout.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            project: ProjectSection {
                name: name.into(),
                top: None,
            },
            sources: Sources::default(),
            sim: SimSection::default(),
            synth: SynthSection::default(),
            target: None,
            program: ProgramSection::default(),
        }
    }
}

/// A loaded project: its root directory plus the parsed manifest.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub file: ProjectFile,
}

impl Project {
    /// Find `fpgaflow.toml` in `start` or the nearest parent directory.
    pub fn find(start: &Path) -> Result<Self, ProjectError> {
        let mut dir = start.canonicalize().map_err(|source| ProjectError::Io {
            path: start.to_path_buf(),
            source,
        })?;
        loop {
            if dir.join(PROJECT_FILE).is_file() {
                return Self::load(&dir);
            }
            if !dir.pop() {
                return Err(ProjectError::NotFound(start.to_path_buf()));
            }
        }
    }

    /// Load the project whose manifest is `root/fpgaflow.toml`.
    pub fn load(root: &Path) -> Result<Self, ProjectError> {
        let path = root.join(PROJECT_FILE);
        let text = std::fs::read_to_string(&path).map_err(|source| ProjectError::Io {
            path: path.clone(),
            source,
        })?;
        let file = toml::from_str(&text).map_err(|source| ProjectError::Parse { path, source })?;
        Ok(Self {
            root: root.to_path_buf(),
            file,
        })
    }

    /// Create a new project skeleton in `root` (which may already exist but
    /// must not already contain a manifest).
    pub fn create(root: &Path, file: ProjectFile) -> Result<Self, ProjectError> {
        let manifest = root.join(PROJECT_FILE);
        if manifest.exists() {
            return Err(ProjectError::AlreadyExists(manifest));
        }
        for sub in ["rtl", "tb", "constraints"] {
            let dir = root.join(sub);
            std::fs::create_dir_all(&dir)
                .map_err(|source| ProjectError::Io { path: dir, source })?;
        }
        let gitignore = root.join(".gitignore");
        if !gitignore.exists() {
            std::fs::write(&gitignore, "/build/\n*.vcd\n*.fst\n").map_err(|source| {
                ProjectError::Io {
                    path: gitignore,
                    source,
                }
            })?;
        }
        let project = Self {
            root: root.to_path_buf(),
            file,
        };
        project.save()?;
        Ok(project)
    }

    pub fn save(&self) -> Result<(), ProjectError> {
        let path = self.manifest_path();
        let text = toml::to_string_pretty(&self.file)?;
        std::fs::write(&path, text).map_err(|source| ProjectError::Io { path, source })
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(PROJECT_FILE)
    }

    pub fn build_dir(&self) -> PathBuf {
        self.root.join("build")
    }

    /// Working directory for one flow stage, e.g. `build/sim`.
    pub fn stage_dir(&self, stage: &str) -> PathBuf {
        self.build_dir().join(stage)
    }

    pub fn rtl_files(&self) -> Result<Vec<PathBuf>, ProjectError> {
        self.expand(&self.file.sources.rtl)
    }

    pub fn tb_files(&self) -> Result<Vec<PathBuf>, ProjectError> {
        self.expand(&self.file.sources.tb)
    }

    pub fn include_dirs(&self) -> Vec<PathBuf> {
        self.file
            .sources
            .include_dirs
            .iter()
            .map(|d| self.root.join(d))
            .collect()
    }

    /// Expand glob patterns relative to the root into a sorted, de-duplicated
    /// list of files. Every match must be an HDL file.
    fn expand(&self, patterns: &[String]) -> Result<Vec<PathBuf>, ProjectError> {
        let root = glob::Pattern::escape(&self.root.to_string_lossy());
        let mut out = BTreeSet::new();
        for pattern in patterns {
            let full = format!("{root}/{pattern}");
            let paths = glob::glob(&full).map_err(|source| ProjectError::Pattern {
                pattern: pattern.clone(),
                source,
            })?;
            for entry in paths {
                let path = entry.map_err(|e| ProjectError::Io {
                    path: e.path().to_path_buf(),
                    source: e.into(),
                })?;
                if path.is_file() {
                    check_hdl(&path)?;
                    out.insert(path);
                }
            }
        }
        Ok(out.into_iter().collect())
    }
}

pub fn is_hdl(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| HDL_EXTENSIONS.contains(&e))
}

pub fn check_hdl(path: &Path) -> Result<(), ProjectError> {
    if is_hdl(path) {
        Ok(())
    } else {
        Err(ProjectError::NotHdl(path.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_manifest_gets_defaults() {
        let f: ProjectFile = toml::from_str("[project]\nname = \"x\"\n").unwrap();
        assert_eq!(f.sim.tool, SimTool::Icarus);
        assert_eq!(f.sim.wave, WaveFormat::Fst);
        assert_eq!(f.program.mode, ProgramMode::Sram);
        assert!(f.target.is_none());
        assert_eq!(f.sources, Sources::default());
    }

    #[test]
    fn full_manifest_parses() {
        let text = r#"
            [project]
            name = "blinky"
            top = "blinky"
            [sources]
            rtl = ["rtl/*.sv"]
            defines = { WIDTH = "24" }
            [sim]
            tool = "verilator"
            tb_top = "tb_blinky"
            wave = "vcd"
            [target]
            family = "spartan3a"
            device = "xc3s700an"
            backend = "ise"
            constraints = ["constraints/board.ucf"]
            [program]
            mode = "flash"
        "#;
        let f: ProjectFile = toml::from_str(text).unwrap();
        assert_eq!(f.sim.tool, SimTool::Verilator);
        assert_eq!(f.sources.defines["WIDTH"], "24");
        assert_eq!(f.target.unwrap().backend.as_deref(), Some("ise"));
        assert_eq!(f.program.mode, ProgramMode::Flash);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = toml::from_str::<ProjectFile>("[project]\nname=\"x\"\nnmae=\"y\"\n");
        assert!(err.is_err());
    }

    #[test]
    fn roundtrip_through_toml() {
        let mut f = ProjectFile::new("rt");
        f.target = Some(TargetSection {
            family: Some("ice40".into()),
            ..Default::default()
        });
        let text = toml::to_string_pretty(&f).unwrap();
        assert_eq!(toml::from_str::<ProjectFile>(&text).unwrap(), f);
    }

    #[test]
    fn hdl_extensions() {
        assert!(is_hdl(Path::new("a/b.sv")));
        assert!(is_hdl(Path::new("b.v")));
        assert!(is_hdl(Path::new("defs.svh")));
        assert!(!is_hdl(Path::new("b.vhd")));
        assert!(!is_hdl(Path::new("Makefile")));
    }
}
