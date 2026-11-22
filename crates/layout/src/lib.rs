//! Layout models for fpgaflow's device view, pin planner and floorplanner.
//!
//! Everything here is parsed from text that ISE writes:
//! - [`device`]: the tile grid (`xdl -report`),
//! - [`package`]: package balls and banks (`partgen -v`),
//! - [`placed`]: the placed and routed design (`xdl -ncd2xdl`).
//!
//! Critical paths come from the timing report, parsed in
//! [`fpgaflow_pnr::ise::parse_timing_paths`].

pub mod device;
pub mod floorplan;
pub mod package;
pub mod pins;
pub mod placed;

pub use device::{site_xy, Device, Site, Tile, TileClass};
pub use package::{Package, PackagePin};
pub use placed::{module_of, Inst, ModuleUse, Net, Placed};
