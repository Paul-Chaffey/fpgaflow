//! HDL-level helpers shared by the flow stages and the GUI.
//!
//! - [`diag`]: turn tool output lines into clickable `file:line:col` diagnostics.
//! - [`scan`]: lightweight source scanning (module names, top-level signals),
//!   used where running a full parser would be overkill.
//! - [`design`]: authoritative hierarchy and port lists from Yosys JSON.

pub mod design;
pub mod diag;
pub mod scan;

pub use design::{Design, Direction, Module, Port};
pub use diag::{Diagnostic, Severity};
