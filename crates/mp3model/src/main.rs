//! mp3model: run the golden-model MP3 decoder.
//!
//!   mp3model decode IN.mp3 OUT.raw   16-bit little-endian PCM, interleaved
//!   mp3model compare IN.mp3          against ffmpeg's float decoder (ISO 11172-4 limits)
//!   mp3model compare-pcm IN REF.pcm  against a 16-bit reference output (ISO conformance)
//!   mp3model info IN.mp3             which coding features the stream uses

use std::process::{Command, ExitCode};

use anyhow::{bail, Context, Result};
use fpgaflow_mp3model::{decode_file, decode_file_with, find_frames, Decoder};

/// ISO/IEC 11172-4 decoder accuracy, full scale = 1.
const FULL_RMS: f64 = 1.0 / 32768.0 / 3.4641016151377544; // 2^-15 / sqrt(12)
const FULL_MAX: f64 = 1.0 / 16384.0; // 2^-14
const LIMITED_RMS: f64 = 1.0 / 2048.0 / 3.4641016151377544; // 2^-11 / sqrt(12)

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["decode", input, out] => decode(input, out),
        ["compare", input] => compare(input).map(|_| ()),
        ["info", input] => info(input),
        ["compare-pcm", input, reference] => compare_pcm(input, reference),
        _ => {
            eprintln!(
                "usage: mp3model decode IN.mp3 OUT.raw | compare IN.mp3 | \
                 compare-pcm IN.mp3 REF.pcm | info IN.mp3"
            );
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn read(path: &str) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("reading {path}"))
}

fn to_s16(x: f64) -> i16 {
    (x * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

fn decode(input: &str, out: &str) -> Result<()> {
    let (rate, nch, pcm) = decode_file(&read(input)?)?;
    let bytes: Vec<u8> = pcm.iter().flat_map(|&x| to_s16(x).to_le_bytes()).collect();
    std::fs::write(out, bytes).with_context(|| format!("writing {out}"))?;
    println!(
        "{} samples per channel, {rate} Hz, {nch} channel(s) -> {out}",
        pcm.len() / nch
    );
    Ok(())
}

/// ffmpeg's float decoder, as interleaved f32.
fn reference(input: &str, nch: usize) -> Result<Vec<f64>> {
    let out = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-c:a",
            "mp3float",
            "-i",
            input,
        ])
        .args(["-f", "f32le", "-ac", &nch.to_string(), "pipe:1"])
        .output()
        .context("running ffmpeg")?;
    if !out.status.success() {
        bail!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(out
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| f32::from_le_bytes(b) as f64)
        .collect())
}

/// Where ffmpeg's output starts in ours: it drops a Xing/Info frame and the
/// encoder delay from the LAME tag, plus the decoder's own 529 samples.
fn expected_offset(data: &[u8]) -> isize {
    let frames = find_frames(data);
    let Some(&(pos, h)) = frames.first() else {
        return 0;
    };
    let at = pos + 4 + if h.crc { 2 } else { 0 } + h.side_info_len();
    let f = &data[at..(pos + h.frame_len()).min(data.len())];
    if f.len() < 8 || !(f.starts_with(b"Xing") || f.starts_with(b"Info")) {
        return 529;
    }
    let flags = u32::from_be_bytes([f[4], f[5], f[6], f[7]]);
    let lame = 8
        + if flags & 1 != 0 { 4 } else { 0 }
        + if flags & 2 != 0 { 4 } else { 0 }
        + if flags & 4 != 0 { 100 } else { 0 }
        + if flags & 8 != 0 { 4 } else { 0 };
    let delay = f
        .get(lame + 21..lame + 23)
        .map(|d| ((d[0] as isize) << 4) | (d[1] as isize >> 4))
        .unwrap_or(0);
    1152 + delay + 529
}

/// Offset (in sample frames) of `b` relative to `a` that best lines them up:
/// a[i + off] ~ b[i], searched near `guess`.
fn align(a: &[f64], b: &[f64], nch: usize, guess: isize) -> isize {
    let frames = |v: &[f64]| v.len() / nch;
    // A stretch of b with signal in it.
    let win = 8192.min(frames(b) / 2);
    let energy = |s: usize| (s..s + win).map(|i| b[i * nch] * b[i * nch]).sum::<f64>();
    let mut start = 0;
    while start + 2 * win < frames(b) && energy(start) < 1e-6 * win as f64 {
        start += win;
    }
    let mut best = (f64::MAX, 0isize);
    for off in guess - 64..=guess + 64 {
        let mut e = 0.0;
        let mut n = 0;
        for i in start..start + win {
            let j = i as isize + off;
            if j < 0 || j as usize >= frames(a) {
                continue;
            }
            for c in 0..nch {
                let d = a[j as usize * nch + c] - b[i * nch + c];
                e += d * d;
            }
            n += 1;
        }
        if n > win / 2 && e / (n as f64) < best.0 {
            best = (e / n as f64, off);
        }
    }
    best.1
}

pub struct Comparison {
    pub rms: f64,
    pub max: f64,
}

fn compare(input: &str) -> Result<Comparison> {
    let data = read(input)?;
    let (rate, nch, ours) = decode_file(&data)?;
    let theirs = reference(input, nch)?;
    let guess = expected_offset(&data);
    let off = align(&ours, &theirs, nch, guess);
    if off != guess {
        eprintln!("  note: best alignment {off}, expected {guess}");
    }
    let mut frame_err: Vec<f64> = vec![0.0; ours.len() / nch / 1152 + 1];
    let (mut sum, mut n, mut max) = (0.0, 0usize, 0.0f64);
    for i in 0..theirs.len() / nch {
        let j = i as isize + off;
        if j < 0 || (j as usize) >= ours.len() / nch {
            continue;
        }
        for c in 0..nch {
            let d = ours[j as usize * nch + c] - theirs[i * nch + c];
            sum += d * d;
            max = max.max(d.abs());
            n += 1;
            let fe = &mut frame_err[j as usize / 1152];
            *fe = fe.max(d.abs());
        }
    }
    let rms = (sum / n.max(1) as f64).sqrt();
    let verdict = if rms < FULL_RMS && max <= FULL_MAX {
        "full accuracy"
    } else if rms < LIMITED_RMS {
        "limited accuracy"
    } else {
        "NOT compliant"
    };
    println!(
        "{input}: {rate} Hz, {nch} ch, {} samples compared (offset {off}); \
         RMS {:.2e} (full < {:.2e}), max {:.2e} (full <= {:.2e}): {verdict}",
        n / nch,
        rms,
        FULL_RMS,
        max,
        FULL_MAX
    );
    if verdict != "full accuracy" {
        // The worst frames, with what they use.
        let frames = find_frames(&data);
        let mut d = Decoder::default();
        let mut desc = Vec::new();
        for &(pos, h) in &frames {
            let s = match d.decode_frame(&data[pos..pos + h.frame_len()]) {
                None => "undecodable".to_string(),
                Some(f) => {
                    let mut s = format!(
                        "{}{}{}",
                        if h.ms_stereo() { "MS " } else { "" },
                        if h.intensity_stereo() { "IS " } else { "" },
                        if f.side.scfsi.iter().flatten().any(|&x| x) {
                            "scfsi "
                        } else {
                            ""
                        }
                    );
                    for gr in &f.side.gr {
                        for g in gr.iter().take(h.channels()) {
                            s.push_str(&format!(
                                "{}{} ",
                                g.block_type,
                                if g.mixed_block { "m" } else { "" }
                            ));
                        }
                    }
                    s
                }
            };
            desc.push(s);
        }
        let mut worst: Vec<(usize, f64)> = frame_err.iter().copied().enumerate().collect();
        worst.sort_by(|a, b| b.1.total_cmp(&a.1));
        for &(i, e) in worst.iter().take(6) {
            println!(
                "  frame {i:>5}: max error {e:.2e}  [{}]",
                desc.get(i).map(String::as_str).unwrap_or("?")
            );
        }
        let ok = frame_err.iter().filter(|&&e| e <= FULL_MAX).count();
        println!(
            "  {ok} of {} frames within {:.1e}",
            frame_err.len(),
            FULL_MAX
        );
    }
    if verdict == "NOT compliant" {
        bail!("output differs from ffmpeg's");
    }
    Ok(Comparison { rms, max })
}

/// Against a reference decoder's 16-bit output (the ISO 11172-4 streams come
/// with one): our output rounded to 16 bits, differences in LSBs.
fn compare_pcm(input: &str, reference: &str) -> Result<()> {
    let (_, nch, ours) = decode_file_with(&read(input)?, false)?;
    let refs: Vec<i16> = read(reference)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| i16::from_le_bytes(b))
        .collect();
    let n = ours.len().min(refs.len());
    let mut hist = [0usize; 3]; // |diff| = 0, 1, > 1
    let (mut sum, mut max, mut first_bad) = (0.0, 0i32, None);
    for i in 0..n {
        let d = to_s16(ours[i]) as i32 - refs[i] as i32;
        hist[(d.unsigned_abs() as usize).min(2)] += 1;
        sum += (d * d) as f64;
        if d.abs() > max {
            max = d.abs();
        }
        if d.abs() > 1 && first_bad.is_none() {
            first_bad = Some(i / nch);
        }
    }
    println!(
        "{input}: {nch} ch, {} vs {} reference samples; exact {}, \u{b1}1 LSB {}, worse {}; RMS {:.3} LSB, max {} LSB{}",
        ours.len(),
        refs.len(),
        hist[0],
        hist[1],
        hist[2],
        (sum / n.max(1) as f64).sqrt(),
        max,
        first_bad.map(|f| format!(" (first at sample {f}, frame {})", f / 1152)).unwrap_or_default()
    );
    // The reference decoder doesn't emit the last frame.
    if hist[2] > 0 || ours.len() < refs.len() || ours.len() > refs.len() + 1152 * nch {
        bail!("differs from the reference");
    }
    Ok(())
}

fn info(input: &str) -> Result<()> {
    let data = read(input)?;
    let frames = find_frames(&data);
    let mut d = Decoder::default();
    let (mut ms, mut is, mut short, mut mixed, mut start, mut stop, mut skipped, mut scfsi) =
        (0, 0, 0, 0, 0, 0, 0, 0);
    let mut rates = std::collections::BTreeSet::new();
    for &(pos, h) in &frames {
        rates.insert(h.bitrate_kbps);
        ms += h.ms_stereo() as usize;
        is += h.intensity_stereo() as usize;
        let Some(f) = d.decode_frame(&data[pos..pos + h.frame_len()]) else {
            skipped += 1;
            continue;
        };
        scfsi += f.side.scfsi.iter().flatten().any(|&s| s) as usize;
        for gr in &f.side.gr {
            for g in gr.iter().take(h.channels()) {
                match (g.window_switching, g.block_type, g.mixed_block) {
                    (true, 2, true) => mixed += 1,
                    (true, 2, false) => short += 1,
                    (true, 1, _) => start += 1,
                    (true, 3, _) => stop += 1,
                    _ => {}
                }
            }
        }
    }
    let Some(&(_, h)) = frames.first() else {
        bail!("no MPEG-1 Layer III frames found");
    };
    println!(
        "{input}: {} frames, {} Hz, {:?}, bitrates {:?} kbit/s\n  frames with mid/side {ms}, intensity {is}, scfsi {scfsi}; \
         granules with short blocks {short}, mixed {mixed}, start {start}, stop {stop}; undecodable {skipped}",
        frames.len(),
        h.sample_rate(),
        h.mode,
        rates
    );
    Ok(())
}
