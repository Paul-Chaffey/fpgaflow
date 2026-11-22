//! Dev board profiles (`data/boards.toml`, compiled in).

use std::sync::LazyLock;

use serde::Deserialize;

const DATA: &str = include_str!("../../../data/boards.toml");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramProfile {
    /// openFPGALoader cable name.
    pub cable: String,
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
    /// How openFPGALoader reaches the FPGA; `None` until a cable is known.
    #[serde(default)]
    pub program: Option<ProgramProfile>,
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
        assert_eq!(p.cable, "xilinxPlatformCableUsb_alt");
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
        assert!(b.program.is_none(), "its USB port is not a JTAG cable");
        let ucf = b.ucf_text().unwrap();
        assert!(ucf.contains(r#"NET "CLK_48MHZ" LOC = "P10""#));
        assert!(ucf.contains(r#"NET "LED<0>" LOC = "D2""#));
    }
}
