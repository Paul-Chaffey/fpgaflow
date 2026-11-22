//! The placed and routed design, from ISE's `xdl -ncd2xdl` output.
//!
//! ```text
//! inst "u_synth.mel_e[9]" "SLICEL",placed R18C3 SLICE_X4Y92  ,
//!   cfg " ... FFX:u_synth.mel_e[8]:#FF G:...:#LUT:D=... " ;
//! net "u_btn.clk" ,
//!   outpin "clk" I ,
//!   inpin "u_seq.cnt[1]" CLK ,
//!   pip R30C34 COUT0 -> CIN2 ,
//!   ;
//! ```
//!
//! Every instance is given the top-level module it belongs to: the majority
//! hierarchical prefix (`u_synth.` -> `u_synth`) of its own name and of the
//! logic packed into it (the BEL names in `cfg`). fpgaflow's Yosys script
//! runs `autoname`, so almost every cell carries such a prefix.

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use regex::Regex;

/// Module for logic whose names carry no hierarchy (top-level signals).
pub const TOP: &str = "<top>";
/// Module for pads.
pub const IO: &str = "<io>";
/// Module for cells ISE adds itself: tie-offs and route-throughs.
pub const ISE: &str = "<ise>";

/// Name prefixes of cells ISE creates (constants, route-throughs).
const ISE_PREFIXES: &[&str] = &["GND_NET", "VCC_NET", "ProtoComp", "XST_GND", "XST_VCC"];

#[derive(Debug, Clone)]
pub struct Inst {
    pub name: String,
    /// `SLICEL`, `SLICEM`, `IOB`, `DIFFM`, `MULT18X18`, `RAMB16`, `BUFGMUX`, ...
    pub kind: String,
    pub tile: String,
    pub site: String,
    pub module: String,
}

#[derive(Debug, Clone)]
pub struct NetPin {
    pub inst: String,
    pub pin: String,
    pub output: bool,
}

#[derive(Debug, Clone)]
pub struct Net {
    pub name: String,
    pub pins: Vec<NetPin>,
    /// Tiles the net's routing passes through, with the number of PIPs used
    /// in each.
    pub pip_tiles: Vec<(String, u32)>,
}

impl Net {
    pub fn driver(&self) -> Option<&NetPin> {
        self.pins.iter().find(|p| p.output)
    }
    pub fn pip_count(&self) -> u32 {
        self.pip_tiles.iter().map(|(_, n)| n).sum()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Placed {
    pub design: String,
    /// Device and speed as XDL gives them, e.g. `xc3s1500fg320-4`.
    pub part: String,
    pub insts: Vec<Inst>,
    pub nets: Vec<Net>,
    inst_index: HashMap<String, usize>,
    net_index: HashMap<String, usize>,
}

static DESIGN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^design\s+"([^"]+)"\s+(\S+)"#).unwrap());
static INST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^inst\s+"((?:[^"\\]|\\.)*)"\s+"([^"]+)"\s*,\s*placed\s+(\S+)\s+(\S+)"#).unwrap()
});
static NET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^net\s+"((?:[^"\\]|\\.)*)""#).unwrap());
static PIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*(outpin|inpin)\s+"((?:[^"\\]|\\.)*)"\s+(\S+)"#).unwrap());
static PIP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*pip\s+(\S+)\s").unwrap());

/// The top-level module a cell or net name belongs to, if its name says.
///
/// `u_synth.mel_e[9]` -> `u_synth`; Yosys's flattened memory/arith names
/// `$flatten\u_synth.$...` -> `u_synth`; anonymous `$abc$...` -> `None`;
/// a plain top-level name such as `tune[0]` -> [`TOP`].
pub fn module_of(name: &str) -> Option<String> {
    if ISE_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return Some(ISE.to_string());
    }
    let mut n = name.trim_start_matches('\\');
    if let Some(rest) = n.strip_prefix("$flatten") {
        n = rest.trim_start_matches('\\');
    } else if n.starts_with('$') {
        return None;
    }
    match n.find('.') {
        Some(i) if i > 0 => Some(n[..i].to_string()),
        _ => Some(TOP.to_string()),
    }
}

/// BEL names packed into an instance, from its `cfg` string: fields like
/// `FFX:u_synth.mel_e[8]:#FF` (attribute, name, value). Colons inside names
/// are escaped as `\:`.
fn cfg_names(cfg: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tok in cfg.split_whitespace() {
        let mut fields = Vec::new();
        let mut cur = String::new();
        let mut chars = tok.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                // `\\` is a backslash and `\:` a colon inside a name.
                '\\' => match chars.next() {
                    Some(n @ ('\\' | ':')) => cur.push(n),
                    Some(n) => {
                        cur.push('\\');
                        cur.push(n);
                    }
                    None => cur.push('\\'),
                },
                ':' => fields.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            }
        }
        fields.push(cur);
        if fields.len() >= 3 && !fields[1].is_empty() && !fields[1].starts_with('#') {
            out.push(fields[1].clone());
        }
    }
    out
}

fn vote(name: &str, kind: &str, cfg: &str) -> String {
    if kind == "IOB" || kind.starts_with("DIFF") {
        return IO.to_string();
    }
    let mut votes: BTreeMap<String, u32> = BTreeMap::new();
    for n in std::iter::once(name.to_string()).chain(cfg_names(cfg)) {
        if let Some(m) = module_of(&n) {
            // A hierarchical name outweighs a bare top-level one.
            *votes.entry(m.clone()).or_default() += if m == TOP { 1 } else { 2 };
        }
    }
    votes
        .into_iter()
        .max_by_key(|(_, v)| *v)
        .map(|(m, _)| m)
        .unwrap_or_else(|| TOP.to_string())
}

fn unescape(s: &str) -> String {
    s.replace("\\\\", "\\").replace("\\\"", "\"")
}

impl Placed {
    pub fn parse(text: &str) -> Placed {
        let mut p = Placed::default();
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            if line.trim().is_empty() || line.starts_with('#') {
                i += 1;
                continue;
            }
            // Gather one statement: up to and including the line ending in ';'.
            let start = i;
            while i < lines.len() && !lines[i].trim_end().ends_with(';') {
                i += 1;
            }
            let end = i.min(lines.len() - 1);
            i += 1;
            if let Some(c) = DESIGN.captures(line) {
                p.design = c[1].to_string();
                p.part = c[2].to_string();
            } else if let Some(c) = INST.captures(line) {
                let body = lines[start..=end].join(" ");
                let cfg = body.split_once("cfg").map(|(_, c)| c).unwrap_or("");
                let name = unescape(&c[1]);
                let kind = c[2].to_string();
                p.inst_index.insert(name.clone(), p.insts.len());
                p.insts.push(Inst {
                    module: vote(&name, &kind, cfg),
                    name,
                    kind,
                    tile: c[3].to_string(),
                    site: c[4].to_string(),
                });
            } else if let Some(c) = NET.captures(line) {
                let mut net = Net {
                    name: unescape(&c[1]),
                    pins: Vec::new(),
                    pip_tiles: Vec::new(),
                };
                let mut tiles: BTreeMap<String, u32> = BTreeMap::new();
                for l in &lines[start..=end] {
                    if let Some(c) = PIN.captures(l) {
                        net.pins.push(NetPin {
                            output: &c[1] == "outpin",
                            inst: unescape(&c[2]),
                            pin: c[3].to_string(),
                        });
                    } else if let Some(c) = PIP.captures(l) {
                        *tiles.entry(c[1].to_string()).or_default() += 1;
                    }
                }
                net.pip_tiles = tiles.into_iter().collect();
                p.net_index.insert(net.name.clone(), p.nets.len());
                p.nets.push(net);
            }
        }
        p
    }

    pub fn inst(&self, name: &str) -> Option<&Inst> {
        self.inst_index.get(name).map(|&i| &self.insts[i])
    }

    pub fn net(&self, name: &str) -> Option<&Net> {
        self.net_index.get(name).map(|&i| &self.nets[i])
    }

    /// Instances per module and kind class: module -> (slices, pads, other).
    pub fn by_module(&self) -> BTreeMap<String, ModuleUse> {
        let mut m: BTreeMap<String, ModuleUse> = BTreeMap::new();
        for i in &self.insts {
            let e = m.entry(i.module.clone()).or_default();
            if i.kind.starts_with("SLICE") {
                e.slices += 1;
            } else if i.kind == "IOB" || i.kind.starts_with("DIFF") {
                e.pads += 1;
            } else if i.kind.starts_with("MULT") {
                e.mults += 1;
            } else if i.kind.starts_with("RAMB") {
                e.brams += 1;
            } else {
                e.other += 1;
            }
        }
        m
    }

    /// Routing PIPs per tile across all nets (for a congestion heat map).
    pub fn pip_density(&self) -> HashMap<&str, u32> {
        let mut d: HashMap<&str, u32> = HashMap::new();
        for n in &self.nets {
            for (t, c) in &n.pip_tiles {
                *d.entry(t.as_str()).or_default() += c;
            }
        }
        d
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModuleUse {
    pub slices: u32,
    pub pads: u32,
    pub mults: u32,
    pub brams: u32,
    pub other: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    const XDL: &str = include_str!("../tests/fixtures/music-excerpt.xdl");

    #[test]
    fn module_names() {
        assert_eq!(module_of("u_synth.mel_e[9]").as_deref(), Some("u_synth"));
        assert_eq!(
            module_of(r"$flatten\u_synth.$/p/synth.sv:104$mul$233").as_deref(),
            Some("u_synth")
        );
        assert_eq!(module_of("$abc$57268$aiger$o2869"), None);
        assert_eq!(module_of("tune[0]").as_deref(), Some(TOP));
        assert_eq!(module_of("GND_NET_10.G").as_deref(), Some(ISE));
        assert_eq!(module_of("ProtoComp27.IMUX").as_deref(), Some(ISE));
    }

    #[test]
    fn cfg_bel_names_with_escaped_colons() {
        let cfg = r#" BXINV::BX F:$abc$1$lut:#LUT:D=~A2 CYMUXF:$flatten\\u_synth.$/p/s.sv\:135$lt$351.muxcy: FFX::#OFF"#;
        let n = cfg_names(cfg);
        assert_eq!(n[0], "$abc$1$lut");
        assert!(n[1].starts_with(r"$flatten\u_synth.$/p/s.sv:135"), "{n:?}");
        assert_eq!(n.len(), 2);
    }

    #[test]
    fn parses_insts_and_nets() {
        let p = Placed::parse(XDL);
        assert_eq!(p.design, "music_top");
        assert_eq!(p.part, "xc3s1500fg320-4");
        assert_eq!(p.insts.len(), 4);

        let s = p.inst("u_synth.mel_e[9]").unwrap();
        assert_eq!(
            (s.kind.as_str(), s.tile.as_str(), s.site.as_str()),
            ("SLICEL", "R18C3", "SLICE_X4Y92")
        );
        assert_eq!(s.module, "u_synth");

        let m = p.insts.iter().find(|i| i.kind == "MULT18X18").unwrap();
        assert_eq!(m.module, "u_synth");
        assert_eq!(p.inst("clk").unwrap().module, IO);
        // An anonymous slice is attributed through the logic packed into it.
        assert_eq!(p.inst("$abc$57268$aiger$o2869").unwrap().module, "u_synth");

        let n = p.net("$abc$57268$aiger$o100").unwrap();
        assert_eq!(n.driver().unwrap().inst, "$abc$57268$aiger$o99");
        assert_eq!(n.pins.iter().filter(|p| !p.output).count(), 11);
        assert!(n.pip_count() >= 12);
        assert!(n.pip_tiles.iter().any(|(t, _)| t == "BMR28C1"));
        assert!(p.net("u_btn.clk").is_some());

        let u = p.by_module();
        assert_eq!(u["u_synth"].slices, 2);
        assert_eq!(u["u_synth"].mults, 1);
        assert_eq!(u[IO].pads, 1);
    }
}
