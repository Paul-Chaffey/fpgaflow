//! Core of fpgaflow: the project model (`fpgaflow.toml`), the user
//! configuration, the external-tool registry ("Doctor") and the async job
//! runner that every flow stage uses to run tools.

pub mod board;
pub mod config;
pub mod diag;
pub mod family;
pub mod job;
pub mod paths;
pub mod project;
pub mod stage;
pub mod state;
pub mod status;
pub mod tools;

pub use config::{AiBackend, AiConfig, EditorConfig, EditorPreset, UserConfig};
pub use job::{JobEvent, JobSpec, Outcome, Stream};
pub use project::{ProgramMode, Project, ProjectError, ProjectFile, SimTool, WaveFormat};
