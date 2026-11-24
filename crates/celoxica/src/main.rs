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

const USAGE: &str = "usage: fpgaflow-celoxica status | load FILE.bit | flash-list | flash-read OUT.tar | flash-write FILE.bit [INDEX] | usb-write FILE | usb-read N [OUT] | play AUDIOFILE | mp3-play FILE.mp3 [--monitor] | eq-play FILE [--eq G,..,G] [--eq-off] [--bass N] [--levels] | mp3-check FILE.mp3 [--monitor] | --version";

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
        ["mp3-check", file] => mp3_check(file, false),
        ["mp3-check", file, "--monitor"] => mp3_check(file, true),
        ["mp3-play", file] => mp3_play(file, false),
        ["mp3-play", file, "--monitor"] => mp3_play(file, true),
        ["eq-play", file, ref opts @ ..] => eq_play(file, opts),
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
    // The MP3 decoder (examples/rc10-mp3) takes MP3s as they are.
    if matches!(&b.usb_read(2, Duration::from_secs(2))?[..], b"M5" | b"M6") {
        drop(b);
        return play_on_decoder(file);
    }
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
    samples: u32,
    pcm_crc: u32,
}

/// The player's side of the status (not compared with the model).
struct Mp3PlayerStatus {
    underruns: u16,
    /// Bass boost level 0..3 (status byte 61 bits 5:4; 0 before the EQ).
    bass: u8,
}

fn front_end_status(b: &Board) -> Result<FrontEndStatus, Error> {
    Ok(mp3_status(b)?.0)
}

fn mp3_status(b: &Board) -> Result<(FrontEndStatus, Mp3PlayerStatus), Error> {
    let s = b.usb_read(64, Duration::from_secs(2))?;
    if s[0] != b'M' || !(b'5'..=b'6').contains(&s[1]) {
        return Err(Error::Other(
            "the RC10 isn't running the MP3 front end: load \
             examples/rc10-mp3/build/impl/mp3_top.bit first"
                .into(),
        ));
    }
    let u32_at = |i: usize| u32::from_le_bytes([s[i], s[i + 1], s[i + 2], s[i + 3]]);
    let fe = FrontEndStatus {
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
        samples: u32_at(50),
        pcm_crc: u32_at(54),
    };
    let pl = Mp3PlayerStatus {
        underruns: u16::from_le_bytes([s[58], s[59]]),
        bass: (s[61] >> 4) & 3,
    };
    Ok((fe, pl))
}

/// Wait (up to 3 s) for the decoder to finish a previous stream: a new one
/// needs the front end idle (22 ms without bytes).
fn wait_idle(b: &Board) -> Result<(), Error> {
    b.mp3_wait_idle(Duration::from_secs(3)).map(|_| ())
}

/// `eq-play FILE [--eq G,..,G] [--eq-off] [--bass N] [--levels]`: play any
/// audio file on an "M6" player with the 10 EQ gains (dB, 31 Hz..16 kHz) and
/// bass level sent as records first; `--levels` shows the band levels as a
/// text bar graph (tests stages 2-3 without the player app).
fn eq_play(file: &str, opts: &[&str]) -> Result<(), Error> {
    eq_play_rest(file, Vec::new(), false, opts)
}

fn eq_play_rest(
    file: &str,
    mut start: Vec<fpgaflow_celoxica::mp3::Command>,
    mut show_levels: bool,
    rest: &[&str],
) -> Result<(), Error> {
    use fpgaflow_celoxica::mp3::{stream_song, Command, Source};
    let mut it = rest.iter();
    while let Some(o) = it.next() {
        match *o {
            "--levels" => show_levels = true,
            "--eq-off" => start.push(Command::Eq(None)),
            "--bass" => start.push(Command::Bass(
                it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            )),
            "--eq" => {
                let g: Vec<f64> = it
                    .next()
                    .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
                    .unwrap_or_default();
                if g.len() != 10 {
                    return Err(Error::Other(
                        "--eq takes 10 gains in dB, comma-separated".into(),
                    ));
                }
                start.push(Command::Eq(Some(std::array::from_fn(|i| g[i]))));
            }
            _ => return Err(Error::Other(format!("unknown option {o}"))),
        }
    }
    let b = open()?;
    let s = b.mp3_wait_idle(Duration::from_secs(3))?;
    if !s.framed() {
        return Err(Error::Other(
            "the running player is older than \"M6\": no EQ".into(),
        ));
    }
    let mut src = Source::open(std::path::Path::new(file))?;
    println!(
        "playing {file}{}",
        if src.converted {
            " (converted by ffmpeg)"
        } else {
            ""
        }
    );
    let (_tx, rx) = std::sync::mpsc::channel();
    let t = Instant::now();
    if show_levels {
        // A text bar graph of the band levels, 5 times a second.
        use std::sync::atomic::{AtomicBool, Ordering};
        let done = AtomicBool::new(false);
        std::thread::scope(|sc| -> Result<(), Error> {
            sc.spawn(|| {
                const BARS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
                while !done.load(Ordering::Relaxed) {
                    if let Ok(s) = b.mp3_status() {
                        let line: String = (0..10)
                            .map(|i| {
                                // left then right, per band
                                let bar = |c: usize| {
                                    let v =
                                        ((s.level_db(c, i) + 60.0) / 60.0 * 8.0).clamp(0.0, 7.0);
                                    BARS[v as usize]
                                };
                                format!("{}{} ", bar(0), bar(1))
                            })
                            .collect();
                        print!("\r  31 Hz {line}16 kHz   ");
                        let _ = std::io::Write::flush(&mut std::io::stdout());
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            });
            let r = stream_song(&b, &mut src, &start, &rx);
            done.store(true, Ordering::Relaxed);
            println!();
            r.map(|_| ())
        })?;
    } else {
        stream_song(&b, &mut src, &start, &rx)?;
    }
    let s = b.mp3_status()?;
    println!(
        "sent in {:.1} s; {} frames, {} samples, {} underruns; bass b{}, EQ {} ({} sets, {} settings taken)",
        t.elapsed().as_secs_f64(),
        s.frames,
        s.samples,
        s.underruns,
        s.bass,
        if s.eq_on { "on" } else { "off" },
        s.eq_sets,
        s.settings
    );
    b.normal_mode()
}

/// `play` with the MP3 decoder loaded: an MPEG-1 Layer III file goes as it
/// is; anything else is converted to one by ffmpeg (LAME, 192 kbit/s).
fn play_on_decoder(file: &str) -> Result<(), Error> {
    use fpgaflow_mp3model::{find_frames, frontend::strip_id3};
    let data = std::fs::read(file).map_err(|e| Error::File(file.to_string(), e))?;
    let body = strip_id3(&data);
    let in_frames: usize = find_frames(body).iter().map(|(_, h)| h.frame_len()).sum();
    if in_frames * 10 >= body.len() * 9 {
        return mp3_play_bytes(file, body, false);
    }
    println!("{file} isn't MPEG-1 Layer III: converting it with ffmpeg");
    let out = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i", file])
        .args([
            "-vn",
            "-map_metadata",
            "-1",
            "-id3v2_version",
            "0",
            "-write_xing",
            "0",
        ])
        .args([
            "-ac",
            "2",
            "-ar",
            "44100",
            "-codec:a",
            "libmp3lame",
            "-b:a",
            "192k",
        ])
        .args(["-f", "mp3", "-"])
        .output()
        .map_err(|e| Error::Other(format!("can't run ffmpeg: {e}")))?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(Error::Other(format!(
            "ffmpeg couldn't convert {file}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    mp3_play_bytes(file, strip_id3(&out.stdout), false)
}

/// Play an MP3 on the RC10's decoder (examples/rc10-mp3).
fn mp3_play(file: &str, monitor: bool) -> Result<(), Error> {
    use fpgaflow_mp3model::frontend::strip_id3;
    let data = std::fs::read(file).map_err(|e| Error::File(file.to_string(), e))?;
    mp3_play_bytes(file, strip_id3(&data), monitor)
}

/// Stream MP3 bytes to the decoder. The FPGA paces the stream: this returns
/// when the last sample is in the DAC FIFO.
/// The MP3 as the running player wants it: in records ("M6"), else raw.
fn as_sent(b: &Board, mp3: &[u8]) -> Result<Vec<u8>, Error> {
    Ok(if b.mp3_status()?.framed() {
        fpgaflow_celoxica::mp3::frame_mp3(mp3)
    } else {
        mp3.to_vec()
    })
}

fn mp3_play_bytes(file: &str, sent: &[u8], monitor: bool) -> Result<(), Error> {
    let b = open()?;
    wait_idle(&b)?;
    println!("playing {file} ({} bytes)", sent.len());
    let t = Instant::now();
    send(&b, &as_sent(&b, sent)?, monitor)?;
    // The decoder drains its buffers (well under a second).
    let mut last = mp3_status(&b)?.0.samples;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let n = mp3_status(&b)?.0.samples;
        if n == last {
            break;
        }
        last = n;
    }
    let (s, p) = mp3_status(&b)?;
    b.normal_mode()?;
    println!(
        "{} frames, {} samples in {:.1} s; {} undecodable, {} underruns, bass b{}, header {:02x?}",
        s.frames,
        s.samples,
        t.elapsed().as_secs_f64(),
        s.undecodable,
        p.underruns,
        p.bass,
        s.last_header
    );
    Ok(())
}

/// Send a stream; with `monitor`, a second thread reads the status 30 times
/// a second meanwhile (can the FX2 serve reads while a write is held back
/// by FULL?) and the read latencies and sample-count steps are reported.
fn send(b: &Board, data: &[u8], monitor: bool) -> Result<(), Error> {
    if !monitor {
        return b.usb_send(data);
    }
    use std::sync::atomic::{AtomicBool, Ordering};
    let done = AtomicBool::new(false);
    let (sent, stats) = std::thread::scope(|s| {
        let reader = s.spawn(|| {
            let period = Duration::from_millis(33);
            let mut lat: Vec<f64> = Vec::new();
            let mut steps: Vec<u32> = Vec::new();
            let mut fails = 0u32;
            let mut last: Option<u32> = None;
            let mut next = Instant::now();
            let mut shown = Instant::now();
            let began = Instant::now();
            while !done.load(Ordering::Relaxed) {
                let t = Instant::now();
                match mp3_status(b) {
                    Ok((st, _)) => {
                        lat.push(t.elapsed().as_secs_f64() * 1e3);
                        if let Some(l) = last {
                            steps.push(st.samples.wrapping_sub(l));
                        }
                        last = Some(st.samples);
                        if shown.elapsed() >= Duration::from_secs(1) {
                            shown = Instant::now();
                            print!(
                                "\r  {} samples, {} reads, {} failed   ",
                                st.samples,
                                lat.len(),
                                fails
                            );
                            let _ = std::io::Write::flush(&mut std::io::stdout());
                        }
                    }
                    Err(e) => {
                        fails += 1;
                        if fails <= 3 {
                            eprintln!("\n  read failed: {e}");
                        }
                    }
                }
                next += period;
                if let Some(w) = next.checked_duration_since(Instant::now()) {
                    std::thread::sleep(w); // (the read cadence, not a wait for an event)
                }
            }
            (lat, steps, fails, began.elapsed().as_secs_f64())
        });
        let r = b.usb_send(data);
        done.store(true, Ordering::Relaxed);
        (r, reader.join().expect("reader thread"))
    });
    let (mut lat, steps, fails, secs) = stats;
    lat.sort_by(|a, b| a.total_cmp(b));
    if !lat.is_empty() {
        let mean = lat.iter().sum::<f64>() / lat.len() as f64;
        let nz: Vec<u32> = steps.iter().copied().filter(|&d| d > 0).collect();
        println!(
            "\n  monitor: {} reads ({:.1}/s), {} failed; latency min {:.2} / mean {:.2} / p99 {:.2} / max {:.2} ms; \
             sample steps min {} max {} ({} of {} reads saw no new samples)",
            lat.len(),
            lat.len() as f64 / secs,
            fails,
            lat[0],
            mean,
            lat[(lat.len() * 99 / 100).min(lat.len() - 1)],
            lat[lat.len() - 1],
            nz.iter().min().unwrap_or(&0),
            nz.iter().max().unwrap_or(&0),
            steps.len() - nz.len(),
            steps.len()
        );
    }
    sent
}

/// Stream an MP3 to the decoder and compare what it found with the model:
/// frame count, bytes in frames, the CRC over every frame's header and side
/// info, undecodable frames (milestone 1); granules and the CRC over their
/// scale factors and Huffman values (milestone 2).
fn mp3_check(file: &str, monitor: bool) -> Result<(), Error> {
    use fpgaflow_mp3model::frontend::{granules, pcm, records, spectra, strip_id3, subbands};
    let data = std::fs::read(file).map_err(|e| Error::File(file.to_string(), e))?;
    let sent = strip_id3(&data);
    let (_, want) = records(sent);
    let (grans, gcrc) = granules(sent);
    let (specs, scrc) = spectra(sent);
    let (subs, bcrc) = subbands(sent);
    let (pcm16, pcrc) = pcm(sent);
    let b = open()?;
    wait_idle(&b)?;
    let stream = as_sent(&b, sent)?;
    let t = Instant::now();
    send(&b, &stream, monitor)?;
    let secs = t.elapsed().as_secs_f64();
    // The last frame is accepted when the stream goes idle.
    let start = Instant::now();
    let got = loop {
        let s = front_end_status(&b)?;
        if (s.idle
            && s.frames == want.frames
            && s.granules == grans.len() as u32
            && s.spectra == specs.len() as u32
            && s.subbands == subs.len() as u32
            && s.samples == pcm16.len() as u32)
            || start.elapsed() > Duration::from_secs(5)
        {
            break s;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    b.normal_mode()?;
    let expect = FrontEndStatus {
        rx_bytes: stream.len() as u32,
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
        samples: pcm16.len() as u32,
        pcm_crc: pcrc,
    };
    let show = |s: &FrontEndStatus| {
        format!(
            "{} frames, {} bytes in frames, CRC {:08x}, {} undecodable, last header {:02x?}; \
             {} granules, CRC {:08x}; {} spectra, CRC {:08x}; {} subbands, CRC {:08x}; {} samples, CRC {:08x}",
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
            s.subbands_crc,
            s.samples,
            s.pcm_crc
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
