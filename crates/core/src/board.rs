//! Dev board profiles (`data/boards.toml`, compiled in).

use std::sync::LazyLock;

use serde::Deserialize;

const DATA: &str = include_str!("../../../data/boards.toml");

/// Which tool loads the bitstream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProgramMethod {
    /// openFPGALoader over a JTAG cable.
    #[default]
    Openfpgaloader,
    /// `fpgaflow-celoxica` over a Celoxica RC board's own USB port.
    Celoxica,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramProfile {
    #[serde(default)]
    pub method: ProgramMethod,
    /// openFPGALoader cable name (required for `openfpgaloader`).
    #[serde(default)]
    pub cable: Option<String>,
    /// FX2 firmware; `ise:<file>` means a file in ISE's `bin/lin64`.
    #[serde(default)]
    pub firmware: Option<String>,
    /// JTAG clock in Hz.
    #[serde(default)]
    pub freq: Option<u32>,
    /// The FPGA's position in the JTAG chain.
    #[serde(default)]
    pub index_chain: Option<u32>,
}

/// Package pins a design must never use, e.g. I/O pins the board wires
/// straight to a supply rail (driving one low would short the rail).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReservedPins {
    pub pins: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Board {
    pub id: String,
    pub name: String,
    pub family: String,
    pub device: String,
    pub package: String,
    pub speed: String,
    pub backend: String,
    /// Master constraint file, relative to `data/`.
    pub ucf: String,
    /// `VID:PID` pairs (lower-case hex) that mean the board is connected:
    /// its programmer, or the board's own USB port.
    pub usb: Vec<String>,
    /// How the bitstream gets to the FPGA; `None` until a way is known.
    #[serde(default)]
    pub program: Option<ProgramProfile>,
    /// Extra bitgen `-g` options (`Name:Value`); a `StartupClk` here
    /// replaces the one fpgaflow picks from the program mode.
    #[serde(default)]
    pub bitgen: Vec<String>,
    /// Pins `fpgaflow build` refuses to let a design use.
    #[serde(default)]
    pub reserved: Vec<ReservedPins>,
    /// I/O bank supply (VCCO) in volts, by bank number, where known.
    #[serde(default)]
    pub banks: std::collections::BTreeMap<String, f32>,
}

/// Master constraint files shipped in `data/`, compiled in so a new
/// project can get a copy without knowing where fpgaflow is installed.
const UCFS: &[(&str, &str)] = &[
    (
        "boards/s3an-starter.ucf",
        include_str!("../../../data/boards/s3an-starter.ucf"),
    ),
    (
        "boards/rc10.ucf",
        include_str!("../../../data/boards/rc10.ucf"),
    ),
];

impl Board {
    /// Text of this board's master constraint file.
    pub fn ucf_text(&self) -> Option<&'static str> {
        UCFS.iter().find(|(p, _)| *p == self.ucf).map(|(_, t)| *t)
    }

    /// File name for the board's UCF inside a project (`constraints/<id>.ucf`).
    pub fn ucf_file_name(&self) -> String {
        format!("{}.ucf", self.id)
    }

    /// VCCO of an I/O bank in volts, if the profile says.
    pub fn bank_voltage(&self, bank: u32) -> Option<f32> {
        self.banks.get(&bank.to_string()).copied()
    }

    /// The LVCMOS standard matching a bank's VCCO, if known.
    pub fn bank_standard(&self, bank: u32) -> Option<&'static str> {
        match self.bank_voltage(bank)? {
            v if (v - 3.3).abs() < 0.05 => Some("LVCMOS33"),
            v if (v - 2.5).abs() < 0.05 => Some("LVCMOS25"),
            v if (v - 1.8).abs() < 0.05 => Some("LVCMOS18"),
            v if (v - 1.5).abs() < 0.05 => Some("LVCMOS15"),
            v if (v - 1.2).abs() < 0.05 => Some("LVCMOS12"),
            _ => None,
        }
    }

    /// Why `pin` must not be used on this board, if it is reserved.
    pub fn reserved_reason(&self, pin: &str) -> Option<&str> {
        self.reserved
            .iter()
            .find(|r| r.pins.iter().any(|p| p.eq_ignore_ascii_case(pin)))
            .map(|r| r.reason.as_str())
    }

    /// Every reserved pin.
    pub fn reserved_pins(&self) -> impl Iterator<Item = &str> {
        self.reserved
            .iter()
            .flat_map(|r| r.pins.iter().map(String::as_str))
    }

    /// Does this board's programmer use USB `vid:pid`?
    pub fn matches_usb(&self, vid: u16, pid: u16) -> bool {
        let id = format!("{vid:04x}:{pid:04x}");
        self.usb.iter().any(|u| u.eq_ignore_ascii_case(&id))
    }
}

#[derive(Deserialize)]
struct Db {
    board: Vec<Board>,
}

static DB: LazyLock<Db> =
    LazyLock::new(|| toml::from_str(DATA).expect("data/boards.toml is invalid"));

pub fn boards() -> &'static [Board] {
    &DB.board
}

pub fn board(id: &str) -> Option<&'static Board> {
    DB.board.iter().find(|b| b.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boards_reference_known_families_and_backends() {
        for b in boards() {
            let f = crate::family::family(&b.family)
                .unwrap_or_else(|| panic!("{}: unknown family", b.id));
            assert!(
                f.backends.contains(&b.backend),
                "{}: backend not valid for family",
                b.id
            );
            if let Some(p) = &b.program {
                assert!(
                    p.method != ProgramMethod::Openfpgaloader || p.cable.is_some(),
                    "{}: openFPGALoader needs a cable",
                    b.id
                );
            }
            let ucf = b.ucf_text().unwrap_or_default();
            for pin in b.reserved_pins() {
                assert!(
                    !ucf.contains(&format!("LOC = \"{pin}\"")),
                    "{}: master UCF uses reserved pin {pin}",
                    b.id
                );
            }
            assert!(
                b.ucf_text().is_some(),
                "{}: UCF {} not compiled in",
                b.id,
                b.ucf
            );
        }
    }

    #[test]
    fn s3an_starter() {
        let b = board("s3an-starter").unwrap();
        let p = b.program.as_ref().unwrap();
        assert_eq!(p.method, ProgramMethod::Openfpgaloader);
        assert_eq!(p.cable.as_deref(), Some("xilinxPlatformCableUsb_alt"));
        assert_eq!(p.freq, Some(750_000));
        assert!(b.matches_usb(0x03fd, 0x000d));
        assert!(b.matches_usb(0x03fd, 0x0008));
        assert!(!b.matches_usb(0x0403, 0x6010));
        assert!(b
            .ucf_text()
            .unwrap()
            .contains(r#"NET "CLK_50MHZ" LOC = "E12""#));
    }

    #[test]
    fn rc10() {
        let b = board("rc10").unwrap();
        assert_eq!(
            (b.device.as_str(), b.package.as_str()),
            ("xc3s1500", "fg320")
        );
        assert!(b.matches_usb(0x107a, 0x9005));
        assert_eq!(b.program.as_ref().unwrap().method, ProgramMethod::Celoxica);
        assert!(b.bitgen.iter().any(|g| g == "StartupClk:Cclk"));
        let ucf = b.ucf_text().unwrap();
        assert!(ucf.contains(r#"NET "CLK_48MHZ" LOC = "P10""#));
        assert_eq!(b.reserved_pins().count(), 14);
        assert_eq!(b.bank_standard(0), Some("LVCMOS12"));
        assert_eq!(b.bank_standard(7), Some("LVCMOS25"));
        assert_eq!(b.bank_standard(4), Some("LVCMOS33"));
        assert!(b.reserved_reason("a8").is_some());
        assert!(b.reserved_reason("D2").is_none());
        assert!(ucf.contains(r#"NET "LED<0>" LOC = "D2""#));
    }
}
