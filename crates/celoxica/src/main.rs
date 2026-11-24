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
use std::time::{Duration, Instant};

use fpgaflow_celoxica::{check_bitstream, Board, Error};

const USAGE: &str = "usage: fpgaflow-celoxica status | load FILE.bit | flash-list | flash-read OUT.tar | flash-write FILE.bit [INDEX] | usb-write FILE | usb-read N [OUT] | play AUDIOFILE | mp3-check FILE.mp3 | --version";

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
        ["play", file] => play(file),
        ["mp3-check", file] => mp3_check(file),
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

/// Status block of examples/rc10-pcm-player (see its pcm_player.sv).
struct PlayerStatus {
    bytes: u32,
    underruns: u16,
    missed: u16,
    dropped: u16,
    late_max: u8,
    /// Sample rate the player's DACs run at.
    rate: u32,
    fill: u8,
    idle: bool,
    paused: bool,
    empty: bool,
    rz: bool,
    volume: u8,
}

fn player_status(b: &fpgaflow_celoxica::Board) -> Result<PlayerStatus, Error> {
    let s = b.usb_read(16, Duration::from_secs(2))?;
    if &s[0..2] != b"P2" {
        return Err(Error::Other(
            "the RC10 isn't running the PCM player: load \
             examples/rc10-pcm-player/build/impl/pcm_player.bit first"
                .into(),
        ));
    }
    let u16_at = |i: usize| u16::from_le_bytes([s[i], s[i + 1]]);
    Ok(PlayerStatus {
        bytes: u32::from_le_bytes([s[2], s[3], s[4], s[5]]),
        underruns: u16_at(6),
        missed: u16_at(8),
        dropped: u16_at(10),
        late_max: s[12],
        // 2 is 44.16 kHz (one DCM); the stream is still made at 44.1 kHz.
        rate: if s[13] == 0 { 48_000 } else { 44_100 },
        fill: s[14],
        idle: s[15] & 0x80 != 0,
        paused: s[15] & 0x40 != 0,
        empty: s[15] & 0x20 != 0,
        rz: s[15] & 0x10 != 0,
        volume: s[15] & 0x0f,
    })
}

/// Poll the player's status until `done` (or give up after `limit`).
fn wait_player(
    b: &fpgaflow_celoxica::Board,
    limit: Duration,
    done: impl Fn(&PlayerStatus) -> bool,
) -> Result<PlayerStatus, Error> {
    let start = Instant::now();
    loop {
        let s = player_status(b)?;
        if done(&s) || start.elapsed() > limit {
            return Ok(s);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Decode an audio file with ffmpeg to 16-bit stereo at the player's rate
/// and stream it to examples/rc10-pcm-player. The player paces the stream
/// with FULL.
fn play(file: &str) -> Result<(), Error> {
    use std::io::Read;
    let b = open()?;
    // Wait for any earlier stream to finish playing and end, so this one
    // starts on a sample boundary with fresh counts.
    let s = wait_player(&b, Duration::from_secs(3), |s| s.empty && s.idle)?;
    if !(s.empty && s.idle) {
        return Err(Error::Other(
            "the player is still busy with another stream".into(),
        ));
    }
    let rate = s.rate;
    let bytes_per_s = rate as u64 * 4;
    let rate_arg = rate.to_string();
    let mut ff = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i", file])
        .args([
            "-vn",
            "-f",
            "s16le",
            "-acodec",
            "pcm_s16le",
            "-ac",
            "2",
            "-ar",
            &rate_arg,
            "pipe:1",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::Other(format!("can't run ffmpeg: {e}")))?;
    let mut out = ff.stdout.take().expect("piped");
    println!(
        "playing {file} ({} Hz stereo); volume and pause on the joystick",
        rate
    );
    let mut buf = vec![0u8; 64 * 1024];
    let mut sent: u64 = 0;
    let mut shown = Instant::now();
    loop {
        // Fill the buffer (ffmpeg's pipe delivers in pieces); send whole samples.
        let mut n = 0;
        while n < buf.len() {
            match out.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) => return Err(Error::Other(format!("reading ffmpeg's output: {e}"))),
            }
        }
        let whole = n - n % 4;
        if whole == 0 {
            break;
        }
        b.usb_send(&buf[..whole])?;
        sent += whole as u64;
        if shown.elapsed() >= Duration::from_secs(1) {
            shown = Instant::now();
            let s = player_status(&b)?;
            let t = sent / bytes_per_s;
            print!(
                "\r  {:>2}:{:02}  buffer {:>3}%  volume {:>2}  DAC {}{}  underruns {}   ",
                t / 60,
                t % 60,
                s.fill as u32 * 100 / 255,
                s.volume,
                if s.rz { "RZ " } else { "NRZ" },
                if s.paused { " (paused)" } else { "" },
                s.underruns
            );
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        if n < buf.len() {
            break;
        }
    }
    let status = ff
        .wait()
        .map_err(|e| Error::Other(format!("ffmpeg: {e}")))?;
    // Let the buffer play out (85 ms when full), then report.
    let s = wait_player(&b, Duration::from_secs(2), |s| s.empty)?;
    let t = sent / bytes_per_s;
    println!(
        "\r  {}:{:02} played, {} bytes; FPGA received {} bytes, {} underrun(s) ({} samples), \
         {} dropped, at most {} bytes after FULL",
        t / 60,
        t % 60,
        sent,
        s.bytes,
        s.underruns,
        s.missed,
        s.dropped,
        s.late_max
    );
    if !status.success() {
        return Err(Error::Other(format!("ffmpeg failed ({status})")));
    }
    b.normal_mode()
}

/// Status block of examples/rc10-mp3 (milestone 1, mp3_top.sv).
#[derive(Debug, PartialEq, Eq)]
struct FrontEndStatus {
    rx_bytes: u32,
    frames: u32,
    frame_bytes: u32,
    crc: u32,
    last_header: [u8; 4],
    undecodable: u16,
    idle: bool,
    granules: u32,
    granule_crc: u32,
    spectra: u32,
    spectra_crc: u32,
    subbands: u32,
    subbands_crc: u32,
}

fn front_end_status(b: &Board) -> Result<FrontEndStatus, Error> {
    let s = b.usb_read(64, Duration::from_secs(2))?;
    if &s[0..2] != b"M4" {
        return Err(Error::Other(
            "the RC10 isn't running the MP3 front end: load \
             examples/rc10-mp3/build/impl/mp3_top.bit first"
                .into(),
        ));
    }
    let u32_at = |i: usize| u32::from_le_bytes([s[i], s[i + 1], s[i + 2], s[i + 3]]);
    Ok(FrontEndStatus {
        rx_bytes: u32_at(2),
        frames: u32_at(6),
        frame_bytes: u32_at(10),
        crc: u32_at(14),
        last_header: [s[18], s[19], s[20], s[21]],
        undecodable: u16::from_le_bytes([s[22], s[23]]),
        idle: s[25] & 0x80 != 0,
        granules: u32_at(26),
        granule_crc: u32_at(30),
        spectra: u32_at(34),
        spectra_crc: u32_at(38),
        subbands: u32_at(42),
        subbands_crc: u32_at(46),
    })
}

/// Stream an MP3 to the decoder and compare what it found with the model:
/// frame count, bytes in frames, the CRC over every frame's header and side
/// info, undecodable frames (milestone 1); granules and the CRC over their
/// scale factors and Huffman values (milestone 2).
fn mp3_check(file: &str) -> Result<(), Error> {
    use fpgaflow_mp3model::frontend::{granules, records, spectra, strip_id3, subbands};
    let data = std::fs::read(file).map_err(|e| Error::File(file.to_string(), e))?;
    let sent = strip_id3(&data);
    let (_, want) = records(sent);
    let (grans, gcrc) = granules(sent);
    let (specs, scrc) = spectra(sent);
    let (subs, bcrc) = subbands(sent);
    let b = open()?;
    // A new stream needs the front end idle (22 ms without bytes).
    let start = Instant::now();
    while !front_end_status(&b)?.idle {
        if start.elapsed() > Duration::from_secs(3) {
            return Err(Error::Other(
                "the front end is still busy with another stream".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let t = Instant::now();
    b.usb_send(sent)?;
    let secs = t.elapsed().as_secs_f64();
    // The last frame is accepted when the stream goes idle.
    let start = Instant::now();
    let got = loop {
        let s = front_end_status(&b)?;
        if (s.idle
            && s.frames == want.frames
            && s.granules == grans.len() as u32
            && s.spectra == specs.len() as u32
            && s.subbands == subs.len() as u32)
            || start.elapsed() > Duration::from_secs(5)
        {
            break s;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    b.normal_mode()?;
    let expect = FrontEndStatus {
        rx_bytes: sent.len() as u32,
        frames: want.frames,
        frame_bytes: sent.len() as u32 - want.skipped,
        crc: want.crc,
        last_header: want.last_header,
        undecodable: want.undecodable,
        idle: true,
        granules: grans.len() as u32,
        granule_crc: gcrc,
        spectra: specs.len() as u32,
        spectra_crc: scrc,
        subbands: subs.len() as u32,
        subbands_crc: bcrc,
    };
    let show = |s: &FrontEndStatus| {
        format!(
            "{} frames, {} bytes in frames, CRC {:08x}, {} undecodable, last header {:02x?}; \
             {} granules, CRC {:08x}; {} spectra, CRC {:08x}; {} subbands, CRC {:08x}",
            s.frames,
            s.frame_bytes,
            s.crc,
            s.undecodable,
            s.last_header,
            s.granules,
            s.granule_crc,
            s.spectra,
            s.spectra_crc,
            s.subbands,
            s.subbands_crc
        )
    };
    println!(
        "sent {} bytes in {secs:.2} s ({:.1} MB/s)\n  FPGA:  {}\n  model: {}",
        sent.len(),
        sent.len() as f64 / secs / 1e6,
        show(&got),
        show(&expect)
    );
    if got != expect {
        return Err(Error::Other(
            "the FPGA's front end and the model disagree".into(),
        ));
    }
    println!("  match");
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
