//! `fpgaflow-celoxica`: configure a Celoxica RC board's FPGA over its USB port.
//!
//!     fpgaflow-celoxica status         # board and FPGA DONE state (read-only)
//!     fpgaflow-celoxica load FILE.bit  # clear the FPGA and load FILE (volatile)
//!
//! fpgaflow runs this as the programmer for boards whose profile says
//! `method = "celoxica"`.

use std::process::ExitCode;
use std::time::Instant;

use fpgaflow_celoxica::{check_bitstream, Board, Error};

const USAGE: &str = "usage: fpgaflow-celoxica status | load FILE.bit | --version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["--version"] => {
            println!("fpgaflow-celoxica {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        ["status"] => status(),
        ["load", file] => load(file),
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
