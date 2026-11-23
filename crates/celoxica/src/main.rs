//! `fpgaflow-celoxica`: configure a Celoxica RC board's FPGA over its USB port.
//!
//!     fpgaflow-celoxica status            # board and FPGA DONE state (read-only)
//!     fpgaflow-celoxica load FILE.bit     # clear the FPGA and load FILE (volatile)
//!     fpgaflow-celoxica flash-list        # files in the board's flash (read-only)
//!     fpgaflow-celoxica flash-read OUT.tar  # back up every flash file (read-only)
//!     fpgaflow-celoxica flash-write FILE.bit  # replace flash file 1, the boot image
//!
//! fpgaflow runs this as the programmer for boards whose profile says
//! `method = "celoxica"`.

use std::process::ExitCode;
use std::time::Instant;

use fpgaflow_celoxica::{check_bitstream, Board, Error};

const USAGE: &str = "usage: fpgaflow-celoxica status | load FILE.bit | flash-list | flash-read OUT.tar | flash-write FILE.bit [INDEX] | usb-write FILE | usb-read N [OUT] | --version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["--version"] => {
            println!("fpgaflow-celoxica {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        ["status"] => status(),
        ["load", file] => load(file),
        ["flash-list"] => flash_list(),
        ["usb-write", file] => usb_write(file),
        ["usb-read", n] | ["usb-read", n, _] => match n.parse() {
            Ok(n) => usb_read(n, args.get(2).map(String::as_str)),
            Err(_) => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        },
        ["flash-read", out] => flash_read(out),
        ["flash-write", file] => flash_write(file, fpgaflow_celoxica::BOOT_FILE),
        ["flash-write", file, index] => match index.parse() {
            Ok(i) => flash_write(file, i),
            Err(_) => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        },
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn open() -> Result<Board, Error> {
    let b = Board::open()?;
    println!("{} on USB bus {} device {}", b.name, b.bus, b.address);
    Ok(b)
}

fn status() -> Result<(), Error> {
    let b = open()?;
    let s = b.status()?;
    println!(
        "status 0x{s:02x}: FPGA {}",
        if s & 0x80 != 0 {
            "configured (DONE high)"
        } else {
            "not configured (DONE low)"
        }
    );
    Ok(())
}

fn load(file: &str) -> Result<(), Error> {
    let data = std::fs::read(file).map_err(|e| Error::File(file.to_string(), e))?;
    let b = open()?;
    let h = check_bitstream(&data, b.name, b.part)?;
    println!(
        "loading {file} ({} bytes{}) into the FPGA (SRAM, volatile)",
        data.len(),
        h.part.map(|p| format!(", part {p}")).unwrap_or_default()
    );
    let t = Instant::now();
    let mut last = 0;
    b.configure(&data, |sent, total| {
        let pct = sent * 100 / total;
        if pct >= last + 25 || sent == total {
            println!("  {sent}/{total} bytes ({pct}%)");
            last = pct;
        }
    })?;
    println!(
        "DONE high: FPGA configured in {:.2} s (status 0x{:02x})",
        t.elapsed().as_secs_f64(),
        b.status()?
    );
    Ok(())
}

/// One line describing a flash file: a bitstream's design and part, or size.
fn describe(data: &[u8]) -> String {
    let h = fpgaflow_celoxica::parse_bit_header(data);
    match (&h.design, &h.part) {
        (Some(d), Some(p)) => format!("bitstream {d}, part {p}"),
        _ => "data".to_string(),
    }
}

/// Send a file's bytes to the running FPGA design (alternate setting 2).
fn usb_write(file: &str) -> Result<(), Error> {
    let data = std::fs::read(file).map_err(|e| Error::File(file.to_string(), e))?;
    let b = open()?;
    let sent = b.usb_write(&data, std::time::Duration::from_secs(2))?;
    println!("sent {sent} of {} bytes to the FPGA", data.len());
    b.normal_mode()
}

/// Read `n` bytes from the running FPGA design: hex dump, or into `out`.
fn usb_read(n: usize, out: Option<&str>) -> Result<(), Error> {
    let b = open()?;
    let data = b.usb_read(n, std::time::Duration::from_secs(2))?;
    b.normal_mode()?;
    match out {
        Some(o) => {
            std::fs::write(o, &data).map_err(|e| Error::File(o.to_string(), e))?;
            println!("read {} bytes into {o}", data.len());
        }
        None => {
            for (i, row) in data.chunks(16).enumerate() {
                let hex: Vec<String> = row.iter().map(|b| format!("{b:02x}")).collect();
                println!("{:06x}: {}", i * 16, hex.join(" "));
            }
        }
    }
    Ok(())
}

fn flash_list() -> Result<(), Error> {
    let b = open()?;
    let mut n = 0;
    for i in fpgaflow_celoxica::FLASH_FILES {
        let len = b.flash_length(i)?;
        if len == 0 {
            continue;
        }
        n += 1;
        // The header is in the first bytes; read a little to describe it.
        let head = b.flash_read_head(i, 256.min(len))?;
        println!("  file {i:>3}  {len:>9} bytes  {}", describe(&head));
    }
    println!("{n} file(s) in flash");
    b.normal_mode()
}

fn flash_read(out: &str) -> Result<(), Error> {
    let b = open()?;
    let f = std::fs::File::create(out).map_err(|e| Error::File(out.to_string(), e))?;
    let mut tar = tar::Builder::new(f);
    let mut index = String::from("# Celoxica RC10 flash backup: file, bytes, contents\n");
    let mut total = 0usize;
    for i in fpgaflow_celoxica::FLASH_FILES {
        if b.flash_length(i)? == 0 {
            continue;
        }
        let data = b.flash_read(i, |_, _| {})?;
        let what = describe(&data);
        let ext = if what.starts_with("bitstream") {
            "bit"
        } else {
            "bin"
        };
        let name = format!("file-{i:03}.{ext}");
        println!("  {name}  {} bytes  {what}", data.len());
        index.push_str(&format!("{name} {} {what}\n", data.len()));
        append(&mut tar, &name, &data).map_err(|e| Error::File(out.to_string(), e))?;
        total += data.len();
    }
    append(&mut tar, "index.txt", index.as_bytes()).map_err(|e| Error::File(out.to_string(), e))?;
    tar.finish().map_err(|e| Error::File(out.to_string(), e))?;
    println!("wrote {out} ({total} bytes of flash)");
    b.normal_mode()
}

fn append<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    name: &str,
    data: &[u8],
) -> std::io::Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    );
    h.set_cksum();
    tar.append_data(&mut h, name, data)
}

fn flash_write(file: &str, index: u8) -> Result<(), Error> {
    let data = std::fs::read(file).map_err(|e| Error::File(file.to_string(), e))?;
    let b = open()?;
    println!(
        "writing {file} ({} bytes) to flash file {index}",
        data.len()
    );
    let t = Instant::now();
    let mut last = 0;
    b.flash_write(index, &data, |sent, total| {
        let pct = sent * 100 / total.max(1);
        if pct >= last + 25 || sent == total {
            println!("  {sent}/{total} bytes ({pct}%)");
            last = pct;
        }
    })?;
    let len = b.flash_length(index)?;
    if len as usize != data.len() {
        b.normal_mode()?;
        return Err(Error::FlashLength {
            index,
            expected: data.len(),
            found: len as usize,
        });
    }
    let back = b.flash_read(index, |_, _| {})?;
    b.normal_mode()?;
    if back != data {
        return Err(Error::FlashVerify(index));
    }
    println!(
        "flash file {index} written and verified in {:.1} s",
        t.elapsed().as_secs_f64()
    );
    if index == fpgaflow_celoxica::BOOT_FILE {
        println!("press the reset button (or power-cycle) to boot it");
    }
    Ok(())
}
