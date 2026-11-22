//! Package pins, from ISE's `partgen -v <device><package>` (`<part>.pkg`).
//!
//! Lines look like
//! `pin  PAD47  B5  0  0  IO_L25N_0  X37Y127 ...` (pad, ball, VREF bank,
//! VCCO bank, function) and `pkgpin  CCLK  T15  -1  -1  CCLK ...` for
//! dedicated, power and ground balls.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq)]
pub struct PackagePin {
    /// Ball name, e.g. `B5`.
    pub ball: String,
    /// The pad (`PAD47`) or dedicated name (`CCLK`, `VCCO_4`, `GND`).
    pub pad: String,
    /// I/O bank for user I/O and VCCO pins.
    pub bank: Option<u32>,
    /// Pin function, e.g. `IO_L32P_4/GCLK0`, `CCLK`, `VCCO_4`.
    pub function: String,
}

impl PackagePin {
    /// A pin a design can use for I/O.
    pub fn is_user_io(&self) -> bool {
        self.function.starts_with("IO")
    }
    /// A global clock input (GCLKn).
    pub fn is_gclk(&self) -> bool {
        self.function.contains("GCLK")
    }
    pub fn is_power(&self) -> bool {
        self.function.starts_with("VCC") || self.function == "GND"
    }
}

#[derive(Debug, Clone, Default)]
pub struct Package {
    pub name: String,
    pub pins: Vec<PackagePin>,
    /// Ball-grid row labels in order (A, B, ... with I, O, Q, S skipped by
    /// Xilinx; AA, AB, ... on larger packages).
    pub rows: Vec<String>,
    pub cols: u32,
}

fn split_ball(ball: &str) -> Option<(&str, u32)> {
    let i = ball.find(|c: char| c.is_ascii_digit())?;
    let (r, c) = ball.split_at(i);
    (!r.is_empty()).then_some(())?;
    Some((r, c.parse().ok()?))
}

impl Package {
    pub fn parse(text: &str) -> Package {
        let mut p = Package::default();
        for line in text.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.first() == Some(&"package") && f.len() > 1 {
                p.name = f[1].to_string();
                continue;
            }
            if f.len() < 6 || (f[0] != "pin" && f[0] != "pkgpin") {
                continue;
            }
            let bank = f[4]
                .parse::<i64>()
                .ok()
                .filter(|b| *b >= 0)
                .map(|b| b as u32);
            p.pins.push(PackagePin {
                pad: f[1].to_string(),
                ball: f[2].to_string(),
                bank,
                function: f[5].to_string(),
            });
        }
        let mut rows: BTreeSet<(usize, String)> = BTreeSet::new();
        for pin in &p.pins {
            if let Some((r, c)) = split_ball(&pin.ball) {
                rows.insert((r.len(), r.to_string()));
                p.cols = p.cols.max(c);
            }
        }
        p.rows = rows.into_iter().map(|(_, r)| r).collect();
        p
    }

    pub fn pin(&self, ball: &str) -> Option<&PackagePin> {
        self.pins.iter().find(|p| p.ball.eq_ignore_ascii_case(ball))
    }

    /// Grid position of a ball: (row index from the top, column index from
    /// the left), both 0-based, as seen from the top of the package.
    pub fn position(&self, ball: &str) -> Option<(usize, usize)> {
        let (r, c) = split_ball(ball)?;
        let row = self.rows.iter().position(|x| x == r)?;
        Some((row, (c as usize).checked_sub(1)?))
    }

    /// Banks that have user I/O, in order.
    pub fn banks(&self) -> Vec<u32> {
        let b: BTreeSet<u32> = self
            .pins
            .iter()
            .filter(|p| p.is_user_io())
            .filter_map(|p| p.bank)
            .collect();
        b.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PKG: &str = include_str!("../tests/fixtures/xc3s1500fg320-excerpt.pkg");

    #[test]
    fn parses_user_io_and_dedicated_pins() {
        let p = Package::parse(PKG);
        assert_eq!(p.name, "xc3s1500fg320");
        let b5 = p.pin("B5").unwrap();
        assert_eq!(
            (b5.pad.as_str(), b5.bank, b5.function.as_str()),
            ("PAD47", Some(0), "IO_L25N_0")
        );
        assert!(b5.is_user_io());
        let clk = p.pin("P10").unwrap();
        assert!(clk.is_gclk() && clk.bank == Some(4));
        let cclk = p.pin("T15").unwrap();
        assert!(!cclk.is_user_io() && cclk.bank.is_none());
        let vcco = p.pin("M10").unwrap();
        assert!(vcco.is_power() && vcco.bank == Some(4));
        assert!(p.pin("F18").unwrap().is_power());
        assert_eq!(p.banks(), [0, 4, 7]);
    }

    #[test]
    fn ball_grid() {
        let p = Package::parse(PKG);
        // Rows present in the excerpt: A B C D E F M P T U.
        assert_eq!(p.rows.first().map(String::as_str), Some("A"));
        assert_eq!(p.position("A2"), Some((0, 1)));
        assert_eq!(p.position("B5"), Some((1, 4)));
        assert!(p.position("Z99").is_none());
        assert_eq!(split_ball("AA12"), Some(("AA", 12)));
    }
}
