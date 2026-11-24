//! The RC10 MP3 player (`examples/rc10-mp3`) from the host side: its status
//! block, the record framing of the stream (bitstreams reporting "M6"), song
//! sources, and streaming a song with settings slipped in between pieces.
//!
//! Records (rtl/stream_demux.sv): `[type, len_hi, len_lo, payload]`.

use crate::{Board, Error};
use fpgaflow_mp3model::eq;
use std::io::Read;
use std::time::{Duration, Instant};

/// Record types.
pub const T_MP3: u8 = 0;
pub const T_EQ: u8 = 1;
pub const T_BASS: u8 = 2;
pub const T_PAUSE: u8 = 3;
pub const T_EQ_OFF: u8 = 4;
/// MP3 bytes per record: small, so a setting waits behind at most this much
/// of the song (about 0.1 s at 128 kbit/s).
pub const CHUNK: usize = 2048;

/// One record.
pub fn record(typ: u8, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= 0xffff);
    let mut r = Vec::with_capacity(3 + payload.len());
    r.push(typ);
    r.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    r.extend_from_slice(payload);
    r
}

/// A whole MP3 as records of [`CHUNK`] bytes.
pub fn frame_mp3(data: &[u8]) -> Vec<u8> {
    data.chunks(CHUNK).flat_map(|c| record(T_MP3, c)).collect()
}

/// The EQ record for `gains` (dB per band) at rate index `rate`, or the
/// "EQ off" record.
pub fn eq_record(rate: usize, gains: Option<&[f64; 10]>) -> Vec<u8> {
    match gains {
        Some(g) => record(T_EQ, &eq::set_bytes(&eq::eq_coefs(rate, g))),
        None => record(T_EQ_OFF, &[]),
    }
}

/// The player's status block ("M5": 64 bytes; "M6": 128).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mp3Status {
    /// Status version: 5 (raw stream) or 6 (records).
    pub version: u8,
    pub rx_bytes: u32,
    pub frames: u32,
    pub frame_bytes: u32,
    pub crc: u32,
    pub last_header: [u8; 4],
    pub undecodable: u16,
    pub fill: u8,
    pub idle: bool,
    pub full: bool,
    pub granules: u32,
    pub granule_crc: u32,
    pub spectra: u32,
    pub spectra_crc: u32,
    pub subbands: u32,
    pub subbands_crc: u32,
    pub samples: u32,
    pub pcm_crc: u32,
    pub underruns: u16,
    pub pcm_fill: u8,
    pub paused: bool,
    pub rz: bool,
    pub bass: u8,
    pub volume: u8,
    /// Rate index the DACs play at: 0 = 44.1, 1 = 48, 2 = 32 kHz.
    pub rate: u8,
    // "M6" only:
    pub eq_sets: u8,
    pub settings: u8,
    pub host_paused: bool,
    pub eq_on: bool,
    pub eq_bank: bool,
    /// Band levels 31 Hz..16 kHz: peak envelopes in sample units (32768 =
    /// full scale), and the snapshot counter (wrapping). Zero before stage 3.
    pub levels: [u16; 10],
    pub level_snaps: u8,
}

impl Mp3Status {
    /// Whether the stream must be sent as records.
    pub fn framed(&self) -> bool {
        self.version >= 6
    }
    /// A band's level in dBFS (peak), floored at -90.
    pub fn level_db(&self, band: usize) -> f64 {
        20.0 * (self.levels[band].max(1) as f64 / 32768.0)
            .log10()
            .max(-4.5)
    }
    /// The DACs' sample rate in Hz.
    pub fn rate_hz(&self) -> u32 {
        match self.rate {
            1 => 48_000,
            2 => 32_000,
            _ => 44_100,
        }
    }
}

impl Board {
    /// Read the MP3 player's status. Fails if another design is running.
    pub fn mp3_status(&self) -> Result<Mp3Status, Error> {
        let s = self.usb_read(128, Duration::from_secs(2))?;
        if s[0] != b'M' || !(b'5'..=b'6').contains(&s[1]) {
            return Err(Error::Other(
                "the RC10 isn't running the MP3 player: load \
                 examples/rc10-mp3/build/impl/mp3_top.bit first"
                    .into(),
            ));
        }
        let u32_at = |i: usize| u32::from_le_bytes([s[i], s[i + 1], s[i + 2], s[i + 3]]);
        let version = s[1] - b'0';
        let m6 = version >= 6;
        Ok(Mp3Status {
            version,
            rx_bytes: u32_at(2),
            frames: u32_at(6),
            frame_bytes: u32_at(10),
            crc: u32_at(14),
            last_header: [s[18], s[19], s[20], s[21]],
            undecodable: u16::from_le_bytes([s[22], s[23]]),
            fill: s[24],
            idle: s[25] & 0x80 != 0,
            full: s[25] & 0x40 != 0,
            granules: u32_at(26),
            granule_crc: u32_at(30),
            spectra: u32_at(34),
            spectra_crc: u32_at(38),
            subbands: u32_at(42),
            subbands_crc: u32_at(46),
            samples: u32_at(50),
            pcm_crc: u32_at(54),
            underruns: u16::from_le_bytes([s[58], s[59]]),
            pcm_fill: s[60],
            paused: s[61] & 0x80 != 0,
            rz: s[61] & 0x40 != 0,
            bass: (s[61] >> 4) & 3,
            volume: s[61] & 0x0f,
            rate: s[62],
            eq_sets: if m6 { s[64] } else { 0 },
            settings: if m6 { s[65] } else { 0 },
            host_paused: m6 && s[66] & 4 != 0,
            eq_on: m6 && s[66] & 2 != 0,
            eq_bank: m6 && s[66] & 1 != 0,
            levels: std::array::from_fn(|b| {
                if m6 {
                    u16::from_le_bytes([s[68 + 2 * b], s[69 + 2 * b]])
                } else {
                    0
                }
            }),
            level_snaps: if m6 { s[88] } else { 0 },
        })
    }

    /// Wait (up to `limit`) until the player has finished its last stream
    /// and can take a new one. If the host had paused it, it is resumed
    /// first (otherwise the stream would never end).
    pub fn mp3_wait_idle(&self, limit: Duration) -> Result<Mp3Status, Error> {
        let start = Instant::now();
        let mut s = self.mp3_status()?;
        if s.host_paused {
            self.usb_send(&record(T_PAUSE, &[0]))?;
        }
        while !s.idle {
            if start.elapsed() > limit {
                return Err(Error::Other(
                    "the MP3 player is still busy with another stream".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
            s = self.mp3_status()?;
        }
        Ok(s)
    }
}

impl Board {
    /// Wait (up to `limit`) until the player has played everything it was
    /// sent: idle, and the PCM sample count steady for 100 ms. ("Idle" alone
    /// only means no bytes for 22 ms; a new stream then would cut the end
    /// off the last one.)
    pub fn mp3_wait_drained(&self, limit: Duration) -> Result<Mp3Status, Error> {
        let start = Instant::now();
        let mut last = self.mp3_status()?;
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let s = self.mp3_status()?;
            if (s.idle && s.samples == last.samples && !s.paused) || start.elapsed() > limit {
                return Ok(s);
            }
            last = s;
        }
    }
}

/// Whether `data` (after any ID3v2 tag) is MPEG-1 Layer III, which the
/// player decodes as it is; and its rate index if so.
pub fn mp3_rate(data: &[u8]) -> Option<usize> {
    let body = fpgaflow_mp3model::frontend::strip_id3(data);
    let frames = fpgaflow_mp3model::find_frames(body);
    let in_frames: usize = frames.iter().map(|(_, h)| h.frame_len()).sum();
    (in_frames * 10 >= body.len() * 9 && in_frames > 0).then(|| frames[0].1.rate_index)
}

/// A song as MP3 bytes: the file itself when the player can decode it, else
/// ffmpeg converting it on the fly (LAME, 192 kbit/s, 44.1 kHz stereo), so
/// AIFF, WAV, FLAC or MPEG-2 files start playing at once.
pub struct Source {
    pub reader: Box<dyn Read + Send>,
    /// Rate index of the MP3 (for EQ coefficients).
    pub rate: usize,
    /// Channels (for turning sample counts into time).
    pub channels: usize,
    /// Length in seconds, when known (MP3 files; not while converting).
    pub duration: Option<f64>,
    /// Whether ffmpeg is converting it.
    pub converted: bool,
    child: Option<std::process::Child>,
}

impl Source {
    pub fn open(path: &std::path::Path) -> Result<Source, Error> {
        let data =
            std::fs::read(path).map_err(|e| Error::Other(format!("{}: {e}", path.display())))?;
        if let Some(rate) = mp3_rate(&data) {
            let body = fpgaflow_mp3model::frontend::strip_id3(&data).to_vec();
            let frames = fpgaflow_mp3model::find_frames(&body);
            let channels = frames.first().map_or(2, |(_, h)| h.channels());
            let hz = [44100.0, 48000.0, 32000.0][rate.min(2)];
            return Ok(Source {
                reader: Box::new(std::io::Cursor::new(body)),
                rate,
                channels,
                duration: Some(frames.len() as f64 * 1152.0 / hz),
                converted: false,
                child: None,
            });
        }
        let mut child = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
            .arg(path)
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
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| Error::Other(format!("can't run ffmpeg: {e}")))?;
        let out = child.stdout.take().expect("piped");
        Ok(Source {
            reader: Box::new(out),
            rate: 0,
            channels: 2,
            duration: None,
            converted: true,
            child: Some(child),
        })
    }

    /// The next piece of the song (up to [`CHUNK`] bytes; empty at the end).
    pub fn next_chunk(&mut self) -> Result<Vec<u8>, Error> {
        let mut buf = vec![0u8; CHUNK];
        let mut n = 0;
        while n < CHUNK {
            match self.reader.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) => return Err(Error::Other(format!("reading the song: {e}"))),
            }
        }
        buf.truncate(n);
        Ok(buf)
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Some(c) = &mut self.child {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// What the player app (or `play`) asks for while a song streams.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Bass(u8),
    /// EQ gains (dB per band), or `None` for off.
    Eq(Option<[f64; 10]>),
    Pause(bool),
    Stop,
}

/// How a song ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Finished,
    Stopped,
}

/// Stream a song to an "M6" player: MP3 records of [`CHUNK`] bytes, with
/// the commands from `cmds` sent between them. Pausing sends the pause
/// record and then nothing but settings until resumed (the FPGA lets go of
/// FULL meanwhile). `start` holds settings to send before the song (EQ,
/// bass). Returns when the whole song has been sent (it then plays out the
/// FPGA's buffers, well under a second) or on `Stop`.
pub fn stream_song(
    b: &Board,
    src: &mut Source,
    start: &[Command],
    cmds: &std::sync::mpsc::Receiver<Command>,
) -> Result<Ended, Error> {
    let rate = src.rate;
    let send_cmd = |c: &Command| -> Result<(), Error> {
        match c {
            Command::Bass(v) => b.usb_send(&record(T_BASS, &[(*v).min(3)])),
            Command::Eq(g) => b.usb_send(&eq_record(rate, g.as_ref())),
            Command::Pause(p) => b.usb_send(&record(T_PAUSE, &[*p as u8])),
            Command::Stop => Ok(()),
        }
    };
    for c in start {
        send_cmd(c)?;
    }
    let mut paused = false;
    loop {
        // Settings first: while paused, wait for them (no song data).
        loop {
            let c = if paused {
                match cmds.recv() {
                    Ok(c) => c,
                    Err(_) => return Ok(Ended::Stopped),
                }
            } else {
                match cmds.try_recv() {
                    Ok(c) => c,
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return Ok(Ended::Stopped),
                }
            };
            match c {
                Command::Stop => {
                    if paused {
                        b.usb_send(&record(T_PAUSE, &[0]))?;
                    }
                    return Ok(Ended::Stopped);
                }
                Command::Pause(p) => {
                    if p != paused {
                        send_cmd(&c)?;
                        paused = p;
                    }
                }
                other => send_cmd(&other)?,
            }
        }
        let piece = src.next_chunk()?;
        if piece.is_empty() {
            return Ok(Ended::Finished);
        }
        b.usb_send(&record(T_MP3, &piece))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_framing() {
        assert_eq!(record(T_BASS, &[2]), [2, 0, 1, 2]);
        assert_eq!(record(T_EQ_OFF, &[]), [4, 0, 0]);
        let data: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        let f = frame_mp3(&data);
        assert_eq!(f.len(), data.len() + 3 * 3);
        assert_eq!(&f[..3], &[0, 8, 0]); // 2048
        assert_eq!(&f[2051..2054], &[0, 8, 0]);
        assert_eq!(&f[4102..4105], &[0, 3, 136]); // 904
        let e = eq_record(0, Some(&[3.0; 10]));
        assert_eq!(e.len(), 3 + eq::SET_BYTES);
        assert_eq!(&e[..3], &[1, 0, 200]);
    }
}
