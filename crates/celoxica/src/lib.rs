//! FPGA configuration for Celoxica RC boards over the board's own USB port.
//!
//! The RC10 has a Cypress FX2 running Celoxica firmware between USB and the
//! FPGA's SelectMAP port. Celoxica only shipped a Windows driver and host
//! library (PDK 4.1: `celoxica_rcusb.sys`, `RC.dll`). The protocol below was
//! read from those binaries and verified on an RC10 on 2026-11-22:
//!
//! | step | USB transfer |
//! |---|---|
//! | programming mode | `SET_INTERFACE` 0, alternate setting 0 |
//! | clear the FPGA (assert nPROG) | vendor OUT, `bRequest 0x41`, no data |
//! | bitstream | the whole `.bit` file on bulk OUT EP `0x02` |
//! | status | vendor IN, `bRequest 0x40`, 1 byte; bit 7 = DONE |
//!
//! The `.bit` header goes along too: the FPGA ignores everything before the
//! `AA 99 55 66` sync word. The bitstream must use `StartupClk:Cclk`, since
//! the FX2 drives CCLK.
//!
//! Only these requests are ever sent. Vendor request `0x43` (Celoxica's
//! flash, clock and RAM commands) and `0xA0` (the FX2's firmware loader) are
//! deliberately not used, so the board's flash and firmware are never touched.

use std::time::{Duration, Instant};

use rusb::{DeviceHandle, GlobalContext};

pub const VID: u16 = 0x107a;

/// Celoxica boards this crate knows how to configure.
pub const BOARDS: &[(u16, &str, &str)] = &[(0x9005, "Celoxica RC10", "3s1500")];

const REQ_STATUS: u8 = 0x40;
const REQ_NPROG: u8 = 0x41;
const EP_BITSTREAM: u8 = 0x02;
const DONE: u8 = 0x80;
/// The Windows driver splits bulk writes at 256 KiB.
const CHUNK: usize = 256 * 1024;
const CTRL_TIMEOUT: Duration = Duration::from_secs(1);
const BULK_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for DONE after the last byte (the host library polls 100 ms).
const DONE_TIMEOUT: Duration = Duration::from_secs(1);

const SYNC: [u8; 4] = [0xaa, 0x99, 0x55, 0x66];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no Celoxica board found on USB (vendor 107a)")]
    NotFound,
    #[error(
        "no permission to open the board: install data/udev/60-fpgaflow-celoxica.rules \
         (see the file for the commands), then replug its USB cable"
    )]
    Access,
    #[error("not a Spartan-3 bitstream: no sync word (AA 99 55 66)")]
    NotBitstream,
    #[error("bitstream is for {found}, but the {board} has an {expected}")]
    WrongPart {
        found: String,
        board: &'static str,
        expected: String,
    },
    #[error(
        "DONE did not go high (status 0x{0:02x}); the FPGA rejected the bitstream. \
         It must be built for this part with StartupClk:Cclk"
    )]
    NotDone(u8),
    #[error("USB: {0}")]
    Usb(#[from] rusb::Error),
    #[error("{0}: {1}")]
    File(String, std::io::Error),
}

/// Fields of a `.bit` file header that matter here.
#[derive(Debug, Default, PartialEq)]
pub struct BitHeader {
    pub design: Option<String>,
    /// Part as bitgen writes it, e.g. `3s1500fg320`.
    pub part: Option<String>,
}

/// Parse the `.bit` header (`a` design, `b` part, …, `e` length + data).
/// Returns an empty header for a raw `.bin`.
pub fn parse_bit_header(data: &[u8]) -> BitHeader {
    let mut h = BitHeader::default();
    let be16 = |i: usize| {
        data.get(i..i + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
    };
    // 2-byte length + 9 bytes, then 2-byte length + 1 byte ("a" follows).
    let Some(n) = be16(0) else { return h };
    let mut i = 2 + n;
    let Some(n) = be16(i) else { return h };
    i += 2 + n.saturating_sub(1);
    while let Some(&key) = data.get(i) {
        if key == b'e' {
            break;
        }
        let Some(len) = be16(i + 1) else { break };
        let Some(v) = data.get(i + 3..i + 3 + len) else {
            break;
        };
        let s = String::from_utf8_lossy(v)
            .trim_end_matches('\0')
            .to_string();
        match key {
            b'a' => h.design = Some(s),
            b'b' => h.part = Some(s),
            b'c' | b'd' => {}
            _ => break,
        }
        i += 3 + len;
    }
    h
}

/// Check a bitstream before sending it: it must contain the sync word and,
/// when the header names a part, match the board's device.
pub fn check_bitstream(
    data: &[u8],
    board: &'static str,
    part: &'static str,
) -> Result<BitHeader, Error> {
    // Part first: other families have other sync sequences (Spartan-3A
    // uses AA 99 then a 16-bit command), so "no sync" would mislead.
    let h = parse_bit_header(data);
    if let Some(p) = &h.part {
        if !p.to_ascii_lowercase().starts_with(part) {
            return Err(Error::WrongPart {
                found: p.clone(),
                board,
                expected: format!("XC{}", part.to_ascii_uppercase()),
            });
        }
    }
    if !data.windows(4).any(|w| w == SYNC) {
        return Err(Error::NotBitstream);
    }
    Ok(h)
}

pub struct Board {
    handle: DeviceHandle<GlobalContext>,
    pub name: &'static str,
    /// Part prefix as bitgen writes it, e.g. `3s1500`.
    pub part: &'static str,
    pub bus: u8,
    pub address: u8,
}

impl Board {
    /// Open the first Celoxica board found.
    pub fn open() -> Result<Self, Error> {
        for dev in rusb::devices()?.iter() {
            let Ok(d) = dev.device_descriptor() else {
                continue;
            };
            if d.vendor_id() != VID {
                continue;
            }
            let Some(&(_, name, part)) = BOARDS.iter().find(|(pid, ..)| *pid == d.product_id())
            else {
                continue;
            };
            let handle = dev.open().map_err(|e| match e {
                rusb::Error::Access => Error::Access,
                e => e.into(),
            })?;
            let _ = handle.set_auto_detach_kernel_driver(true);
            handle.claim_interface(0)?;
            return Ok(Self {
                handle,
                name,
                part,
                bus: dev.bus_number(),
                address: dev.address(),
            });
        }
        Err(Error::NotFound)
    }

    /// The FX2's status byte; bit 7 is the FPGA's DONE pin.
    pub fn status(&self) -> Result<u8, Error> {
        let mut b = [0u8; 1];
        self.handle
            .read_control(0xc0, REQ_STATUS, 0, 0, &mut b, CTRL_TIMEOUT)?;
        Ok(b[0])
    }

    pub fn done(&self) -> Result<bool, Error> {
        Ok(self.status()? & DONE != 0)
    }

    /// Clear the FPGA and load `data` (a `.bit` or `.bin`). `progress` gets
    /// (bytes sent, total) after each chunk.
    pub fn configure(
        &self,
        data: &[u8],
        mut progress: impl FnMut(usize, usize),
    ) -> Result<(), Error> {
        check_bitstream(data, self.name, self.part)?;
        self.handle.set_alternate_setting(0, 0)?;
        self.handle
            .write_control(0x40, REQ_NPROG, 0, 0, &[], CTRL_TIMEOUT)?;
        let mut sent = 0;
        while sent < data.len() {
            let end = (sent + CHUNK).min(data.len());
            sent += self
                .handle
                .write_bulk(EP_BITSTREAM, &data[sent..end], BULK_TIMEOUT)?;
            progress(sent, data.len());
        }
        // DONE follows the last bytes within a few CCLK cycles; poll briefly.
        let t = Instant::now();
        loop {
            let s = self.status()?;
            if s & DONE != 0 {
                return Ok(());
            }
            if t.elapsed() >= DONE_TIMEOUT {
                return Err(Error::NotDone(s));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Board {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The start of the blinky.bit bitgen wrote for the RC10 (header + sync).
    fn bit(part: &str) -> Vec<u8> {
        let mut v = vec![
            0x00, 0x09, 0x0f, 0xf0, 0x0f, 0xf0, 0x0f, 0xf0, 0x0f, 0xf0, 0x00, 0x00, 0x01,
        ];
        let field = |v: &mut Vec<u8>, k: u8, s: &str| {
            v.push(k);
            v.extend_from_slice(&((s.len() + 1) as u16).to_be_bytes());
            v.extend_from_slice(s.as_bytes());
            v.push(0);
        };
        field(&mut v, b'a', "blinky.ncd;UserID=0xFFFFFFFF");
        field(&mut v, b'b', part);
        field(&mut v, b'c', "2026/11/22");
        field(&mut v, b'd', "18:57:00");
        v.push(b'e');
        v.extend_from_slice(&8u32.to_be_bytes());
        v.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xaa, 0x99, 0x55, 0x66]);
        v
    }

    #[test]
    fn header_fields() {
        let h = parse_bit_header(&bit("3s1500fg320"));
        assert_eq!(h.part.as_deref(), Some("3s1500fg320"));
        assert_eq!(h.design.as_deref(), Some("blinky.ncd;UserID=0xFFFFFFFF"));
    }

    #[test]
    fn accepts_the_rc10_parts() {
        // Celoxica's own builds say 3s1500l; ours say 3s1500. Same die and IDCODE.
        assert!(check_bitstream(&bit("3s1500fg320"), "Celoxica RC10", "3s1500").is_ok());
        assert!(check_bitstream(&bit("3s1500lfg320"), "Celoxica RC10", "3s1500").is_ok());
    }

    #[test]
    fn rejects_wrong_part_and_non_bitstreams() {
        let mut a3 = bit("3s700anfgg484");
        a3.truncate(a3.len() - 4); // Spartan-3A: AA 99 then 30 A1, no AA995566
        a3.extend_from_slice(&[0xaa, 0x99, 0x30, 0xa1]);
        let e = check_bitstream(&a3, "Celoxica RC10", "3s1500").unwrap_err();
        assert!(matches!(e, Error::WrongPart { .. }), "{e}");
        assert!(e.to_string().contains("XC3S1500"), "{e}");
        assert!(matches!(
            check_bitstream(b"not a bitstream", "Celoxica RC10", "3s1500"),
            Err(Error::NotBitstream)
        ));
    }

    #[test]
    fn raw_bin_has_no_header_but_is_accepted() {
        let raw = [0xff, 0xff, 0xff, 0xff, 0xaa, 0x99, 0x55, 0x66];
        assert_eq!(parse_bit_header(&raw), BitHeader::default());
        assert!(check_bitstream(&raw, "Celoxica RC10", "3s1500").is_ok());
    }
}
