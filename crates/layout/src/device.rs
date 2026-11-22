//! The device grid, from an ISE XDLRC report (`xdl -report <device><package>`).
//!
//! An XDLRC lists every tile (`(tile ROW COL NAME TYPE n`) with the sites
//! inside it (`(primitive_site NAME TYPE bonded|unbonded|internal n)`).
//! Bonded IOB sites are named after their package ball (`C3`), so the device
//! and package views share coordinates. Row 0 is the top of the die.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

/// What a tile is, for drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TileClass {
    Clb,
    Bram,
    Iob,
    Dcm,
    Clock,
    Other,
}

#[derive(Debug, Clone)]
pub struct Site {
    pub name: String,
    /// Primitive type: `SLICEL`, `SLICEM`, `IOB`, `RAMB16`, `MULT18X18`, ...
    pub kind: String,
    pub tile: usize,
    /// A package pin is attached (IOBs only).
    pub bonded: bool,
}

#[derive(Debug, Clone)]
pub struct Tile {
    pub row: u32,
    pub col: u32,
    pub name: String,
    pub kind: String,
    pub class: TileClass,
    /// Indices into [`Device::sites`].
    pub sites: Vec<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct Device {
    /// As the report names it, e.g. `xc3s1500fg320-5`.
    pub part: String,
    pub family: String,
    pub rows: u32,
    pub cols: u32,
    pub tiles: Vec<Tile>,
    pub sites: Vec<Site>,
    site_index: HashMap<String, usize>,
    tile_index: HashMap<String, usize>,
}

/// Site types that are bookkeeping, not resources a design uses.
const SKIP_SITES: &[&str] = &["VCC", "RESERVED_LL", "RESERVED_ANDOR", "GLOBALSIG"];

static HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\(xdl_resource_report\s+\S+\s+(\S+)\s+(\S+)").unwrap());
static TILES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\(tiles\s+(\d+)\s+(\d+)").unwrap());
static TILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\(tile\s+(\d+)\s+(\d+)\s+(\S+)\s+(\S+)").unwrap());
static SITE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*\(primitive_site\s+(\S+)\s+(\S+)\s+(bonded|unbonded|internal)").unwrap()
});
static XY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"_X(\d+)Y(\d+)$").unwrap());

fn classify(site_kinds: &[&str]) -> TileClass {
    let has = |p: &str| site_kinds.iter().any(|k| k.starts_with(p));
    if has("SLICE") {
        TileClass::Clb
    } else if has("RAMB") || has("MULT") || has("DSP") {
        TileClass::Bram
    } else if has("IOB") || has("DIFF") {
        TileClass::Iob
    } else if has("DCM") || has("PLL") {
        TileClass::Dcm
    } else if has("BUFG") {
        TileClass::Clock
    } else {
        TileClass::Other
    }
}

impl Device {
    pub fn parse(text: &str) -> Device {
        let mut d = Device::default();
        let mut current: Option<usize> = None;
        let mut kinds: Vec<String> = Vec::new();
        let finish = |d: &mut Device, current: Option<usize>, kinds: &mut Vec<String>| {
            if let Some(t) = current {
                let k: Vec<&str> = kinds.iter().map(String::as_str).collect();
                d.tiles[t].class = classify(&k);
            }
            kinds.clear();
        };
        for line in text.lines() {
            if line.starts_with("(primitive_defs") {
                break;
            }
            if let Some(c) = HEADER.captures(line) {
                d.part = c[1].to_string();
                d.family = c[2].to_string();
            } else if let Some(c) = TILES.captures(line) {
                d.rows = c[1].parse().unwrap_or(0);
                d.cols = c[2].parse().unwrap_or(0);
            } else if let Some(c) = TILE.captures(line) {
                finish(&mut d, current, &mut kinds);
                let idx = d.tiles.len();
                d.tile_index.insert(c[3].to_string(), idx);
                d.tiles.push(Tile {
                    row: c[1].parse().unwrap_or(0),
                    col: c[2].parse().unwrap_or(0),
                    name: c[3].to_string(),
                    kind: c[4].to_string(),
                    class: TileClass::Other,
                    sites: Vec::new(),
                });
                current = Some(idx);
            } else if let (Some(c), Some(t)) = (SITE.captures(line), current) {
                let kind = &c[2];
                kinds.push(kind.to_string());
                if SKIP_SITES.contains(&kind) {
                    continue;
                }
                let idx = d.sites.len();
                d.site_index.insert(c[1].to_string(), idx);
                d.sites.push(Site {
                    name: c[1].to_string(),
                    kind: kind.to_string(),
                    tile: t,
                    bonded: &c[3] == "bonded",
                });
                d.tiles[t].sites.push(idx);
            }
        }
        finish(&mut d, current, &mut kinds);
        d
    }

    pub fn site(&self, name: &str) -> Option<&Site> {
        self.site_index.get(name).map(|&i| &self.sites[i])
    }

    pub fn tile(&self, name: &str) -> Option<&Tile> {
        self.tile_index.get(name).map(|&i| &self.tiles[i])
    }

    pub fn site_tile(&self, site: &Site) -> &Tile {
        &self.tiles[site.tile]
    }

    /// How many sites of a primitive type (prefix match: `SLICE` counts both
    /// `SLICEL` and `SLICEM`).
    pub fn count(&self, kind_prefix: &str) -> usize {
        self.sites
            .iter()
            .filter(|s| s.kind.starts_with(kind_prefix))
            .count()
    }

    /// I/O sites with a package pin (IOB, DIFFM and DIFFS sites on Spartan-3).
    pub fn bonded_pads(&self) -> usize {
        self.sites.iter().filter(|s| s.bonded).count()
    }

    /// The slice grid's extent: (max X + 1, max Y + 1).
    pub fn slice_extent(&self) -> (u32, u32) {
        self.sites
            .iter()
            .filter(|s| s.kind.starts_with("SLICE"))
            .filter_map(|s| site_xy(&s.name))
            .fold((0, 0), |(mx, my), (x, y)| (mx.max(x + 1), my.max(y + 1)))
    }
}

/// The `_X<x>Y<y>` coordinates in a site name (`SLICE_X4Y120` -> (4, 120)).
pub fn site_xy(name: &str) -> Option<(u32, u32)> {
    let c = XY.captures(name)?;
    Some((c[1].parse().ok()?, c[2].parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XDLRC: &str = include_str!("../tests/fixtures/xc3s1500fg320-excerpt.xdlrc");

    #[test]
    fn parses_header_tiles_and_sites() {
        let d = Device::parse(XDLRC);
        assert_eq!(d.part, "xc3s1500fg320-5");
        assert_eq!(d.family, "spartan3");
        assert_eq!((d.rows, d.cols), (79, 63));
        assert_eq!(d.tiles.len(), 3);

        let clb = d.tile("R4C3").unwrap();
        assert_eq!((clb.row, clb.col, clb.class), (5, 6, TileClass::Clb));
        assert_eq!(clb.sites.len(), 4, "VCC and RLL sites are skipped");
        let s = d.site("SLICE_X5Y121").unwrap();
        assert_eq!(s.kind, "SLICEL");
        assert_eq!(d.site_tile(s).name, "R4C3");

        assert_eq!(d.tile("BMR4C1").unwrap().class, TileClass::Bram);
        assert!(d.site("MULT18X18_X0Y15").is_some());

        let io = d.tile("LIOIR1").unwrap();
        assert_eq!(io.class, TileClass::Iob);
        let pad = d.site("C3").unwrap();
        assert!(pad.bonded, "bonded IOB sites are named after the ball");
        assert_eq!(d.count("SLICE"), 4);
        assert_eq!(d.bonded_pads(), 2, "NOPAD180 is unbonded");
    }

    #[test]
    fn site_coordinates() {
        assert_eq!(site_xy("SLICE_X4Y120"), Some((4, 120)));
        assert_eq!(site_xy("MULT18X18_X0Y15"), Some((0, 15)));
        assert_eq!(site_xy("C3"), None);
        let d = Device::parse(XDLRC);
        assert_eq!(d.slice_extent(), (6, 122));
    }
}
