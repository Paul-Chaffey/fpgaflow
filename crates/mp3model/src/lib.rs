//! Golden model of the RC10's MP3 decoder (docs/mp3-decoder-plan.md).
//!
//! MPEG-1 Layer III, decoded in the same stages as the hardware will:
//! frame/side info, bit reservoir, scale factors, Huffman, requantise,
//! stereo, reorder, antialias, IMDCT + overlap + frequency inversion,
//! polyphase synthesis. Every stage's output is kept per granule
//! ([`GranuleTrace`]) so RTL can be compared stage by stage.

// Index loops mirror the hardware's address counters; keep them.
#![allow(clippy::needless_range_loop)]

pub mod bits;
pub mod fixed;
pub mod frame;
pub mod frontend;
pub mod layer3;
pub mod tables;

use bits::Bits;
use frame::{Header, SideInfo};
use layer3::{ScaleFactors, Synth};

/// Every stage's output for one granule of one channel.
#[derive(Debug, Clone)]
pub struct GranuleTrace {
    pub sf: ScaleFactors,
    /// Huffman output.
    pub is: [i32; 576],
    /// After requantisation (short blocks in coded order).
    pub xr: [f64; 576],
    /// After stereo processing.
    pub stereo: [f64; 576],
    /// After reorder and antialias: the IMDCT's input.
    pub hybrid: [f64; 576],
    /// After IMDCT, overlap-add and frequency inversion: `[18 * sb + t]`.
    pub subband: [f64; 576],
    /// Synthesis output, full scale = 1.0.
    pub pcm: [f64; 576],
}

#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub header: Header,
    pub side: SideInfo,
    /// `[granule][channel]`.
    pub granules: Vec<Vec<GranuleTrace>>,
}

impl DecodedFrame {
    /// Interleaved PCM, full scale = 1.0.
    pub fn pcm(&self) -> Vec<f64> {
        let nch = self.header.channels();
        let mut out = Vec::with_capacity(1152 * nch);
        for gr in &self.granules {
            for i in 0..576 {
                for t in gr.iter().take(nch) {
                    out.push(t.pcm[i]);
                }
            }
        }
        out
    }
}

/// Where each frame starts, skipping an ID3v2 tag and resynchronising after
/// anything that isn't a frame. A header counts only if the next frame
/// (or the end of the data) follows where its length says.
pub fn find_frames(data: &[u8]) -> Vec<(usize, Header)> {
    let mut pos = 0;
    if data.len() >= 10 && &data[..3] == b"ID3" {
        let size = data[6..10]
            .iter()
            .fold(0usize, |a, &b| (a << 7) | (b & 0x7f) as usize);
        pos = 10 + size + if data[5] & 0x10 != 0 { 10 } else { 0 };
    }
    let mut out = Vec::new();
    while pos + 4 <= data.len() {
        if let Some(h) = Header::parse(&data[pos..]) {
            let next = pos + h.frame_len();
            let follows = next + 4 > data.len()
                || Header::parse(&data[next..]).is_some_and(|n| n.rate_index == h.rate_index);
            if follows && next <= data.len() {
                out.push((pos, h));
                pos = next;
                continue;
            }
        }
        pos += 1;
    }
    out
}

/// Decoder state across frames.
pub struct Decoder {
    reservoir: Vec<u8>,
    overlap: [[[f64; 18]; 32]; 2],
    pub synth: [Synth; 2],
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder {
            reservoir: Vec::new(),
            overlap: [[[0.0; 18]; 32]; 2],
            synth: [Synth::default(), Synth::default()],
        }
    }
}

impl Decoder {
    /// Decode one frame (`frame` starts at its header). Returns `None` when
    /// its main data begins in frames that weren't seen (after a seek or at
    /// a damaged start); the reservoir still takes this frame's bytes.
    pub fn decode_frame(&mut self, frame: &[u8]) -> Option<DecodedFrame> {
        let h = Header::parse(frame)?;
        let nch = h.channels();
        let side_at = 4 + if h.crc { 2 } else { 0 };
        let side = SideInfo::parse(&frame[side_at..], nch);
        let main =
            &frame[(side_at + h.side_info_len()).min(frame.len())..h.frame_len().min(frame.len())];

        // Keep enough reservoir for main_data_begin (at most 511 bytes back).
        if self.reservoir.len() > 4096 {
            self.reservoir.drain(..self.reservoir.len() - 2048);
        }
        let have = self.reservoir.len();
        self.reservoir.extend_from_slice(main);
        if side.main_data_begin > have {
            return None;
        }
        let res = std::mem::take(&mut self.reservoir);
        let mut b = Bits::at(&res, (have - side.main_data_begin) * 8);

        let mut granules = Vec::with_capacity(2);
        let mut sf_prev = [ScaleFactors::default(); 2];
        for gr in 0..2 {
            let mut chans: Vec<GranuleTrace> = Vec::with_capacity(nch);
            for ch in 0..nch {
                let g = &side.gr[gr][ch];
                let start = b.pos;
                let sf = layer3::read_scalefactors(&mut b, g, &side.scfsi[ch], gr, &sf_prev[ch]);
                let mut is = [0i32; 576];
                let end = start + g.part2_3_length as usize;
                layer3::huffman(&mut b, end, g, h.rate_index, &mut is);
                let xr = layer3::requantize(g, &sf, h.rate_index, &is);
                chans.push(GranuleTrace {
                    sf,
                    is,
                    xr,
                    stereo: xr,
                    hybrid: [0.0; 576],
                    subband: [0.0; 576],
                    pcm: [0.0; 576],
                });
                if gr == 0 {
                    sf_prev[ch] = sf;
                }
            }
            if nch == 2 && (h.ms_stereo() || h.intensity_stereo()) {
                let (l, r) = chans.split_at_mut(1);
                let (l, r) = (&mut l[0], &mut r[0]);
                layer3::stereo(
                    &side.gr[gr][1],
                    &r.sf,
                    h.rate_index,
                    &r.is,
                    h.ms_stereo(),
                    h.intensity_stereo(),
                    &mut l.stereo,
                    &mut r.stereo,
                );
            }
            for (ch, t) in chans.iter_mut().enumerate() {
                let g = &side.gr[gr][ch];
                t.hybrid = t.stereo;
                layer3::reorder(g, h.rate_index, &mut t.hybrid);
                layer3::antialias(g, &mut t.hybrid);
                t.subband = layer3::imdct(g, &t.hybrid, &mut self.overlap[ch]);
                for ts in 0..18 {
                    let s: [f64; 32] = std::array::from_fn(|sb| t.subband[18 * sb + ts]);
                    let p = self.synth[ch].step(&s);
                    t.pcm[32 * ts..32 * ts + 32].copy_from_slice(&p);
                }
            }
            granules.push(chans);
        }
        self.reservoir = res;
        Some(DecodedFrame {
            header: h,
            side,
            granules,
        })
    }
}

/// Decode a whole file. Frames that can't be decoded give silence, so the
/// output keeps its timing. Returns (sample rate, channels, interleaved PCM).
pub fn decode_file(data: &[u8]) -> anyhow::Result<(u32, usize, Vec<f64>)> {
    decode_file_with(data, true)
}

/// As [`decode_file`]; with `keep_timing` false, frames that can't be
/// decoded are left out (as the ISO reference decoder does).
pub fn decode_file_with(data: &[u8], keep_timing: bool) -> anyhow::Result<(u32, usize, Vec<f64>)> {
    let frames = find_frames(data);
    let Some(&(_, first)) = frames.first() else {
        anyhow::bail!("no MPEG-1 Layer III frames found");
    };
    let mut d = Decoder::default();
    let mut pcm = Vec::new();
    for &(pos, h) in &frames {
        let bytes = &data[pos..pos + h.frame_len()];
        match d.decode_frame(bytes) {
            Some(f) => pcm.extend(f.pcm()),
            None if keep_timing => pcm.extend(std::iter::repeat_n(0.0, 1152 * h.channels())),
            None => {}
        }
    }
    Ok((first.sample_rate(), first.channels(), pcm))
}
