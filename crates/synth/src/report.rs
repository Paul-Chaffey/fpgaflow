//! Utilisation report from Yosys `stat -json`, with cells grouped into
//! resource categories (LUTs, flip-flops, carry, block RAM, DSP, I/O, ...)
//! across vendor primitive libraries.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Category {
    Lut,
    FlipFlop,
    Carry,
    WideMux,
    LutRam,
    BlockRam,
    Dsp,
    Io,
    Clock,
    Other,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Lut => "LUTs",
            Category::FlipFlop => "Flip-flops",
            Category::Carry => "Carry logic",
            Category::WideMux => "Wide muxes",
            Category::LutRam => "Distributed RAM",
            Category::BlockRam => "Block RAM",
            Category::Dsp => "DSP / multipliers",
            Category::Io => "I/O buffers",
            Category::Clock => "Clocking",
            Category::Other => "Other",
        }
    }
}

/// Ordered: the first match wins, so the specific patterns come first.
static RULES: LazyLock<Vec<(Category, Regex)>> = LazyLock::new(|| {
    let r = |c, p: &str| (c, Regex::new(&format!("^(?:{p})$")).unwrap());
    vec![
        r(
            Category::LutRam,
            r"RAM\d+X\d+\w*|RAM(32|64|128|256)M\w*|TRELLIS_DPR16X4|DPR16X4\w*|RAM16SDP4|MISTRAL_MLAB",
        ),
        r(
            Category::Clock,
            r"BUFG\w*|BUFH\w*|BUFR|BUFIO\w*|SB_GB\w*|DCCA|DCSC|DCM\w*|MMCM\w*|PLL\w*|EHXPLL\w*|SB_PLL40\w*|rPLL|CC_PLL|CC_BUFG|MISTRAL_CLKBUF|EFX_GBUF\w*|\w+_clkctrl",
        ),
        r(
            Category::Io,
            r"IBUF\w*|OBUF\w*|IOBUF\w*|SB_IO\w*|TRELLIS_IO|IB|OB|BB|TBUF|IOBUF|MISTRAL_IB|MISTRAL_OB|MISTRAL_IO|EFX_IO\w*|CC_[IOT]BUF|IDDR\w*|ODDR\w*|inpad|outpad|bipad|ckpad|\w+_io_[io]buf",
        ),
        r(
            Category::Dsp,
            r"DSP\w*|MULT\w*|SB_MAC16|ALU54\w*|MISTRAL_MUL\w*|CC_MULT",
        ),
        r(
            Category::BlockRam,
            r"RAMB\w*|SB_RAM\w*|DP16KD|PDPW16KD|DP8KC|PDPW8KC|BSRAM|SDPB|DPB|SPX9|SP|ROM\w*|MISTRAL_M10K|EFX_[DS]PRAM\w*|CC_BRAM\w*|EBR\w*|LRAM",
        ),
        r(
            Category::Carry,
            r"SB_CARRY|MUXCY|XORCY|CARRY\d*|CCU2\w*|ALU|MISTRAL_ALUT_ARITH|EFX_ADD|CC_ADDF",
        ),
        r(Category::WideMux, r"MUXF[5-9]|MUX2_LUT\d|PFUMX|L6MUX21"),
        r(
            Category::FlipFlop,
            r"SB_DFF\w*|FD\w*|TRELLIS_FF|DFF\w*|LD[CP]E?|MISTRAL_FF|EFX_FF|CC_DFF|CC_DLT|dff\w*|\$_DFF\w*|\$_SDFF\w*|\$dff\w*",
        ),
        r(
            Category::Lut,
            r"SB_LUT4|LUT[1-6](_2)?|LUT4|TRELLIS_COMB|LUT|MISTRAL_ALUT\d|EFX_LUT4|CC_LUT\w*|CC_L2T\d|INV|MISTRAL_NOT|\w+_lcell_comb|\$lut",
        ),
    ]
});

pub fn categorise(cell: &str) -> Category {
    RULES
        .iter()
        .find(|(_, re)| re.is_match(cell))
        .map(|(c, _)| *c)
        .unwrap_or(Category::Other)
}

#[derive(Debug, Clone, Serialize)]
pub struct Utilisation {
    pub cells: u64,
    pub by_type: BTreeMap<String, u64>,
    pub by_category: BTreeMap<Category, u64>,
}

#[derive(Deserialize)]
struct StatJson {
    design: StatDesign,
}
#[derive(Deserialize)]
struct StatDesign {
    num_cells: u64,
    #[serde(default)]
    num_cells_by_type: BTreeMap<String, u64>,
}

impl Utilisation {
    pub fn from_stat_json(text: &str) -> Result<Self, serde_json::Error> {
        let s: StatJson = serde_json::from_str(text)?;
        let mut by_category = BTreeMap::new();
        for (cell, n) in &s.design.num_cells_by_type {
            // Constant tie-offs are not resources.
            if matches!(
                cell.as_str(),
                "VCC" | "GND" | "VHI" | "VLO" | "logic_0" | "logic_1" | "$scopeinfo"
            ) {
                continue;
            }
            *by_category.entry(categorise(cell)).or_default() += n;
        }
        Ok(Self {
            cells: s.design.num_cells,
            by_type: s.design.num_cells_by_type,
            by_category,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `stat -json` output from Yosys 0.71 for blinky on xc3sa.
    const XC3SA: &str = r#"{
       "creator": "Yosys 0.71+77",
       "invocation": "stat -json ",
       "modules": { "\\blinky": { "num_cells": 79 } },
       "design": {
          "num_wires": 57, "num_cells": 79, "num_submodules": 0,
          "num_cells_by_type": { "BUFG": 1, "FDRE": 25, "IBUF": 2, "INV": 1, "MUXCY": 24, "OBUF": 1, "XORCY": 25 }
       }
    }"#;

    #[test]
    fn xc3sa_blinky() {
        let u = Utilisation::from_stat_json(XC3SA).unwrap();
        assert_eq!(u.cells, 79);
        let c = |k| u.by_category.get(&k).copied().unwrap_or(0);
        assert_eq!(c(Category::FlipFlop), 25);
        assert_eq!(c(Category::Carry), 49);
        assert_eq!(c(Category::Io), 3);
        assert_eq!(c(Category::Clock), 1);
        assert_eq!(c(Category::Lut), 1);
    }

    #[test]
    fn vendor_primitives() {
        for (cell, cat) in [
            ("SB_LUT4", Category::Lut),
            ("SB_DFFR", Category::FlipFlop),
            ("SB_CARRY", Category::Carry),
            ("SB_RAM40_4K", Category::BlockRam),
            ("SB_IO", Category::Io),
            ("SB_GB", Category::Clock),
            ("LUT6", Category::Lut),
            ("RAM32M", Category::LutRam),
            ("RAMB36E1", Category::BlockRam),
            ("DSP48E1", Category::Dsp),
            ("CARRY4", Category::Carry),
            ("MUXF7", Category::WideMux),
            ("TRELLIS_FF", Category::FlipFlop),
            ("TRELLIS_COMB", Category::Lut),
            ("CCU2C", Category::Carry),
            ("DP16KD", Category::BlockRam),
            ("MULT18X18D", Category::Dsp),
            ("MULT18X18SIO", Category::Dsp),
            ("RAMB16BWER", Category::BlockRam),
            ("DFFE", Category::FlipFlop),
            ("MUX2_LUT5", Category::WideMux),
            ("ALU", Category::Carry),
            ("dffepc", Category::FlipFlop),
            ("ckpad", Category::Io),
            ("FD1P3IX", Category::FlipFlop),
            ("CCU2", Category::Carry),
            ("MISTRAL_CLKBUF", Category::Clock),
            ("MISTRAL_NOT", Category::Lut),
            ("fiftyfivenm_lcell_comb", Category::Lut),
            ("cycloneive_io_ibuf", Category::Io),
            ("cycloneive_clkctrl", Category::Clock),
            ("dffeas", Category::FlipFlop),
            ("EFX_GBUFCE", Category::Clock),
            ("weird_thing", Category::Other),
        ] {
            assert_eq!(categorise(cell), cat, "{cell}");
        }
    }
}
