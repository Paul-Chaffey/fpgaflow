//! Checking that every top-level port has a pin before anything is built.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use fpgaflow_hdl::design::Port;
use regex::Regex;

static UCF_LOC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)^\s*NET\s+"?([^"\s]+)"?\s+LOC\s*="#).unwrap());

/// Net names that get a `LOC` in UCF text. Comments are ignored.
pub fn ucf_located_nets(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .filter_map(|l| UCF_LOC.captures(l).map(|c| c[1].to_string()))
        .collect()
}

static UCF_PIN_LOC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^\s*(?:NET|INST)\s+"?([^"\s]+)"?\s+LOC\s*=\s*"?([^";|]+)"?"#).unwrap()
});
static PACKAGE_PIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[A-Z]{1,2}[0-9]{1,2}$").unwrap());

/// A package pin given to a net or instance by a `LOC` constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinLoc {
    pub net: String,
    pub pin: String,
    /// 1-based line in the UCF.
    pub line: u32,
}

/// Every package-pin `LOC` in UCF text (`NET "x" LOC = "A8"`, also
/// `INST` and comma-separated lists). Slice sites such as `SLICE_X0Y0`
/// are not package pins and are skipped.
pub fn ucf_pin_locs(text: &str) -> Vec<PinLoc> {
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        let l = l.split('#').next().unwrap_or("");
        if let Some(c) = UCF_PIN_LOC.captures(l) {
            for pin in c[2]
                .split(',')
                .map(str::trim)
                .filter(|p| PACKAGE_PIN.is_match(p))
            {
                out.push(PinLoc {
                    net: c[1].to_string(),
                    pin: pin.to_ascii_uppercase(),
                    line: i as u32 + 1,
                });
            }
        }
    }
    out
}

/// Normalise bus-bit spellings (`led<3>`, `led[3]`, `led(3)`) to `led<3>`.
fn norm(name: &str) -> String {
    name.replace(['[', '('], "<").replace([']', ')'], ">")
}

fn matches(pattern: &str, name: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == name;
    }
    let re = format!("^{}$", regex::escape(pattern).replace(r"\*", ".*"));
    Regex::new(&re).is_ok_and(|r| r.is_match(name))
}

/// Port bits (e.g. `led<0>`) with no matching `LOC`. Wildcards like
/// `led<*>` are honoured; names compare case-sensitively, as ISE does.
pub fn unconstrained(ports: &[Port], located: &BTreeSet<String>) -> Vec<String> {
    let located: Vec<String> = located.iter().map(|n| norm(n)).collect();
    let mut missing = Vec::new();
    for p in ports {
        let bits: Vec<String> = if p.width == 1 {
            vec![p.name.clone()]
        } else {
            (0..p.width).map(|i| format!("{}<{i}>", p.name)).collect()
        };
        for b in bits {
            if !located.iter().any(|l| matches(l, &b)) {
                missing.push(b);
            }
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use fpgaflow_hdl::design::Direction;

    fn port(name: &str, width: usize) -> Port {
        Port {
            name: name.into(),
            direction: Direction::Input,
            width,
        }
    }

    #[test]
    fn parses_ucf_locs() {
        let t = r#"
            NET "clk" LOC = "E12" | IOSTANDARD = LVCMOS33 ;
            NET "clk" PERIOD = 20.0ns HIGH 40%;
            # NET "old" LOC = "A1";
            net rst loc = T15;
            NET "LED<7>" LOC = "W21" | IOSTANDARD = LVCMOS33 ;
        "#;
        let n = ucf_located_nets(t);
        assert_eq!(n.into_iter().collect::<Vec<_>>(), ["LED<7>", "clk", "rst"]);
    }

    #[test]
    fn pin_locs_with_lines() {
        let t = "NET \"clk\" LOC = \"P10\" | IOSTANDARD = LVCMOS33 ;\n\
                 # NET \"x\" LOC = \"A8\";\n\
                 net led loc = a8;\n\
                 INST \"u\" LOC = SLICE_X0Y0;\n\
                 NET \"d\" LOC = \"E4, F5\";\n";
        let l = ucf_pin_locs(t);
        let got: Vec<_> = l
            .iter()
            .map(|p| (p.net.as_str(), p.pin.as_str(), p.line))
            .collect();
        assert_eq!(
            got,
            [
                ("clk", "P10", 1),
                ("led", "A8", 3),
                ("d", "E4", 5),
                ("d", "F5", 5)
            ]
        );
    }

    #[test]
    fn finds_missing_bits() {
        let located =
            ucf_located_nets("NET \"clk\" LOC=E12;\nNET \"d[0]\" LOC=A1;\nNET \"q<*>\" LOC=B2;");
        let ports = [port("clk", 1), port("rst", 1), port("d", 2), port("q", 4)];
        assert_eq!(unconstrained(&ports, &located), ["rst", "d<1>"]);
    }

    #[test]
    fn blinky_ucf_covers_blinky() {
        let ucf = include_str!("../../../examples/blinky/constraints/s3an-starter.ucf");
        let ports = [port("clk", 1), port("rst", 1), port("led", 1)];
        assert!(unconstrained(&ports, &ucf_located_nets(ucf)).is_empty());
    }
}
