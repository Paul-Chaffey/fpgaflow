//! Floorplan regions: confine a top-level module to a rectangle of the die.
//!
//! Regions live in `constraints/floorplan.ucf`, which fpgaflow writes in
//! full (it is generated; hand edits elsewhere are safer). Each region is
//! an ISE area group over the module's cells, which fpgaflow's synthesis
//! names `module.*`:
//!
//! ```text
//! INST "u_synth.*" AREA_GROUP = "AG_u_synth";
//! AREA_GROUP "AG_u_synth" RANGE = SLICE_X0Y0:SLICE_X23Y31, MULT18X18_X0Y0:MULT18X18_X0Y3;
//! ```
//!
//! The rectangle is held in slice coordinates; the multipliers and block
//! RAMs inside it are added as further ranges so hard blocks follow the
//! module too.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::device::{site_xy, Device};
use crate::placed::Placed;

/// Project-relative path of the floorplan file.
pub const FILE: &str = "constraints/floorplan.ucf";

const HEADER: &str = "# Floorplan regions, written by fpgaflow's floorplanner (Device tab or\n\
# `fpgaflow floorplan`). Edits here are overwritten; change regions there.\n";

/// A module confined to slices (x0, y0)..=(x1, y1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub module: String,
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Region {
    /// Normalised so x0 <= x1 and y0 <= y1.
    pub fn new(module: &str, a: (u32, u32), b: (u32, u32)) -> Region {
        Region {
            module: module.to_string(),
            x0: a.0.min(b.0),
            y0: a.1.min(b.1),
            x1: a.0.max(b.0),
            y1: a.1.max(b.1),
        }
    }
    pub fn group(&self) -> String {
        format!("AG_{}", self.module)
    }
    pub fn slice_range(&self) -> String {
        format!(
            "SLICE_X{}Y{}:SLICE_X{}Y{}",
            self.x0, self.y0, self.x1, self.y1
        )
    }
    pub fn contains(&self, x: u32, y: u32) -> bool {
        (self.x0..=self.x1).contains(&x) && (self.y0..=self.y1).contains(&y)
    }
    pub fn slices(&self) -> u32 {
        (self.x1 - self.x0 + 1) * (self.y1 - self.y0 + 1)
    }

    /// Ranges of hard blocks (`MULT18X18`, `RAMB16`) whose tiles lie inside
    /// the rectangle's tiles.
    pub fn hard_ranges(&self, device: &Device) -> Vec<String> {
        let tile_of = |x: u32, y: u32| {
            device
                .site(&format!("SLICE_X{x}Y{y}"))
                .map(|s| device.site_tile(s))
                .map(|t| (t.row, t.col))
        };
        let (Some(a), Some(b)) = (tile_of(self.x0, self.y0), tile_of(self.x1, self.y1)) else {
            return Vec::new();
        };
        let (r0, r1) = (a.0.min(b.0), a.0.max(b.0));
        let (c0, c1) = (a.1.min(b.1), a.1.max(b.1));
        let mut by_kind: BTreeMap<&str, (u32, u32, u32, u32)> = BTreeMap::new();
        for s in &device.sites {
            let kind = if s.kind.starts_with("MULT") {
                "MULT18X18"
            } else if s.kind.starts_with("RAMB") {
                "RAMB16"
            } else {
                continue;
            };
            let t = device.site_tile(s);
            // Hard-block tiles sit in their own columns between CLB
            // columns, so they belong to the rectangle when strictly inside
            // its column span.
            if !(r0..=r1).contains(&t.row) || !(c0 < t.col && t.col < c1) {
                continue;
            }
            let Some((x, y)) = site_xy(&s.name) else {
                continue;
            };
            let e = by_kind.entry(kind).or_insert((x, y, x, y));
            *e = (e.0.min(x), e.1.min(y), e.2.max(x), e.3.max(y));
        }
        by_kind
            .into_iter()
            .map(|(k, (x0, y0, x1, y1))| format!("{k}_X{x0}Y{y0}:{k}_X{x1}Y{y1}"))
            .collect()
    }
}

static INST_AG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^\s*INST\s+"?([^"\s]+)\.\*"?\s+AREA_GROUP\s*=\s*"?([^";\s]+)"?"#).unwrap()
});
static AG_RANGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^\s*AREA_GROUP\s+"?([^"\s]+)"?\s+RANGE\s*=\s*([^;]+)"#).unwrap()
});
static SLICE_RANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"SLICE_X(\d+)Y(\d+)\s*:\s*SLICE_X(\d+)Y(\d+)").unwrap());

/// Regions in a floorplan file.
pub fn parse(text: &str) -> Vec<Region> {
    let mut module_of_group: BTreeMap<String, String> = BTreeMap::new();
    let mut ranges: Vec<(String, (u32, u32), (u32, u32))> = Vec::new();
    for line in text.lines().map(|l| l.split('#').next().unwrap_or("")) {
        if let Some(c) = INST_AG.captures(line) {
            module_of_group.insert(c[2].to_string(), c[1].to_string());
        } else if let Some(c) = AG_RANGE.captures(line) {
            if let Some(r) = SLICE_RANGE.captures(&c[2]) {
                let n = |i: usize| r[i].parse::<u32>().unwrap_or(0);
                ranges.push((c[1].to_string(), (n(1), n(2)), (n(3), n(4))));
            }
        }
    }
    ranges
        .into_iter()
        .filter_map(|(g, a, b)| module_of_group.get(&g).map(|m| Region::new(m, a, b)))
        .collect()
}

/// The whole floorplan file for `regions`.
pub fn render(regions: &[Region], device: &Device) -> String {
    let mut s = String::from(HEADER);
    for r in regions {
        let mut ranges = vec![r.slice_range()];
        ranges.extend(r.hard_ranges(device));
        s.push_str(&format!(
            "\nINST \"{}.*\" AREA_GROUP = \"{}\";\nAREA_GROUP \"{}\" RANGE = {};\n",
            r.module,
            r.group(),
            r.group(),
            ranges.join(", ")
        ));
    }
    s
}

/// How well a module kept to its region after place and route.
#[derive(Debug, Clone, PartialEq)]
pub struct Containment {
    pub module: String,
    pub inside: usize,
    /// Slices of the module placed outside: (instance, site).
    pub outside: Vec<(String, String)>,
}

/// Slices of each region's module inside and outside the region.
pub fn containment(regions: &[Region], placed: &Placed) -> Vec<Containment> {
    regions
        .iter()
        .map(|r| {
            let mut c = Containment {
                module: r.module.clone(),
                inside: 0,
                outside: Vec::new(),
            };
            for i in placed
                .insts
                .iter()
                .filter(|i| i.module == r.module && i.kind.starts_with("SLICE"))
            {
                match site_xy(&i.site) {
                    Some((x, y)) if r.contains(x, y) => c.inside += 1,
                    _ => c.outside.push((i.name.clone(), i.site.clone())),
                }
            }
            c
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const XDLRC: &str = include_str!("../tests/fixtures/xc3s1500fg320-excerpt.xdlrc");
    const XDL: &str = include_str!("../tests/fixtures/music-excerpt.xdl");

    #[test]
    fn round_trip() {
        let d = Device::parse(XDLRC);
        let regions = vec![
            Region::new("u_synth", (23, 31), (0, 0)),
            Region::new("u_seg", (40, 80), (60, 100)),
        ];
        assert_eq!(regions[0].slice_range(), "SLICE_X0Y0:SLICE_X23Y31");
        let text = render(&regions, &d);
        assert!(
            text.contains("INST \"u_synth.*\" AREA_GROUP = \"AG_u_synth\";"),
            "{text}"
        );
        assert!(text.contains("AREA_GROUP \"AG_u_synth\" RANGE = SLICE_X0Y0:SLICE_X23Y31"));
        assert_eq!(parse(&text), regions);
        // Hand-written forms parse too.
        let hand = "inst u_meter.* area_group = AG_m ;  # meter\nAREA_GROUP AG_m RANGE = SLICE_X2Y2:SLICE_X5Y9, MULT18X18_X0Y0:MULT18X18_X0Y1;\n";
        assert_eq!(parse(hand), [Region::new("u_meter", (2, 2), (5, 9))]);
    }

    #[test]
    fn hard_blocks_inside_the_rectangle() {
        // The excerpt has a BRAM/MULT tile at row 5, col 5 (MULT18X18_X0Y15)
        // between CLB columns; SLICE_X4Y120 is at row 5, col 6.
        let d = Device::parse(XDLRC);
        let none = Region::new("m", (4, 120), (5, 121));
        assert!(none.hard_ranges(&d).is_empty());
    }

    #[test]
    fn containment_counts_slices() {
        let p = Placed::parse(XDL);
        // u_synth.mel_e[9] at SLICE_X4Y92, the anonymous u_synth slice at SLICE_X11Y62.
        let r = Region::new("u_synth", (0, 0), (10, 100));
        let c = &containment(&[r], &p)[0];
        assert_eq!(c.inside, 1);
        assert_eq!(
            c.outside,
            [(
                "$abc$57268$aiger$o2869".to_string(),
                "SLICE_X11Y62".to_string()
            )]
        );
    }
}
