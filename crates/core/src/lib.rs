//! Core of fpgaflow: the project model (`fpgaflow.toml`), the user
//! configuration, the external-tool registry ("Doctor") and the async job
//! runner that every flow stage uses to run tools.

pub mod config;
pub mod job;
pub mod project;
pub mod tools;

pub use config::{EditorConfig, EditorPreset, UserConfig};
pub use job::{JobEvent, JobSpec, Outcome, Stream};
pub use project::{ProgramMode, Project, ProjectError, ProjectFile, SimTool, WaveFormat};
