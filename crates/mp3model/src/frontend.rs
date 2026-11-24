//! What the hardware front end (milestone 1) produces per frame: a 70-byte
//! record of the header, every side-info field and the reservoir pointer.
//! The RTL testbench writes the same records; on the board the FPGA keeps a
//! CRC32 over them.
//!
//! Record layout (big-endian fields):
//!
//! | bytes | |
//! |---|---|
//! | 0-3 | header |
//! | 4-5 | main_data_begin |
//! | 6 | scfsi: bits 7..4 channel 0 bands 0..3, bits 3..0 channel 1 |
//! | 7-66 | granule 0 ch 0, gr 0 ch 1, gr 1 ch 0, gr 1 ch 1; 15 bytes each, zero for a missing channel |
//! | 67-68 | main-data start in a 4096-byte ring: (main-data bytes before this frame - main_data_begin) mod 4096 |
//! | 69 | 1 if the main data is all there (main_data_begin <= bytes before) |
//!
//! Per granule: part2_3_length (2), big_values (2), global_gain, scalefac_compress,
//! flags {window_switching, mixed, block_type[1:0], 0, preflag, scalefac_scale, count1table},
//! table_select 0, 1, 2, subblock_gain (2: 0000000 g0 g1 g2), region0_count, region1_count.

use crate::find_frames;
use crate::frame::SideInfo;

pub const RECORD: usize = 70;

/// The bytes the PC sends: the file without a leading ID3v2 tag.
pub fn strip_id3(data: &[u8]) -> &[u8] {
    if data.len() >= 10 && &data[..3] == b"ID3" {
        let size = data[6..10]
            .iter()
            .fold(0usize, |a, &b| (a << 7) | (b & 0x7f) as usize);
        let end = 10 + size + if data[5] & 0x10 != 0 { 10 } else { 0 };
        return &data[end.min(data.len())..];
    }
    data
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub frames: u32,
    pub crc: u32,
    /// Bytes outside any frame (sync search).
    pub skipped: u32,
    pub undecodable: u16,
    pub last_header: [u8; 4],
}

/// Records for a stream as sent (already without ID3v2).
pub fn records(data: &[u8]) -> (Vec<[u8; RECORD]>, Summary) {
    let mut out = Vec::new();
    let mut sum = Summary::default();
    let mut crc = Crc32::default();
    let mut md_total: usize = 0; // main-data bytes appended so far
    let mut in_frames = 0;
    for (pos, h) in find_frames(data) {
        let f = &data[pos..pos + h.frame_len()];
        let side_at = 4 + if h.crc { 2 } else { 0 };
        let nch = h.channels();
        let si = SideInfo::parse(&f[side_at..], nch);
        let md_len = h.frame_len() - side_at - h.side_info_len();
        let mut r = [0u8; RECORD];
        r[0..4].copy_from_slice(&f[0..4]);
        r[4..6].copy_from_slice(&(si.main_data_begin as u16).to_be_bytes());
        for ch in 0..nch {
            for band in 0..4 {
                if si.scfsi[ch][band] {
                    r[6] |= 0x80 >> (4 * ch + band);
                }
            }
        }
        for gr in 0..2 {
            for ch in 0..nch {
                let g = &si.gr[gr][ch];
                let o = 7 + 15 * (2 * gr + ch);
                r[o..o + 2].copy_from_slice(&(g.part2_3_length as u16).to_be_bytes());
                r[o + 2..o + 4].copy_from_slice(&(g.big_values as u16).to_be_bytes());
                r[o + 4] = g.global_gain as u8;
                r[o + 5] = g.scalefac_compress as u8;
                r[o + 6] = (g.window_switching as u8) << 7
                    | (g.mixed_block as u8) << 6
                    | (g.block_type as u8) << 4
                    | (g.preflag as u8) << 2
                    | (g.scalefac_scale as u8) << 1
                    | g.count1_table as u8;
                for t in 0..3 {
                    r[o + 7 + t] = g.table_select[t] as u8;
                }
                let sbg =
                    (g.subblock_gain[0] << 6 | g.subblock_gain[1] << 3 | g.subblock_gain[2]) as u16;
                r[o + 10..o + 12].copy_from_slice(&sbg.to_be_bytes());
                r[o + 12] = g.region0_count as u8;
                r[o + 13] = g.region1_count as u8;
            }
        }
        let start = (md_total as isize - si.main_data_begin as isize).rem_euclid(4096) as u16;
        r[67..69].copy_from_slice(&start.to_be_bytes());
        r[69] = (si.main_data_begin <= md_total) as u8;
        if r[69] == 0 {
            sum.undecodable += 1;
        }
        md_total += md_len;
        in_frames += h.frame_len();
        crc.update(&r);
        sum.frames += 1;
        sum.last_header.copy_from_slice(&f[0..4]);
        out.push(r);
    }
    sum.crc = crc.value();
    sum.skipped = (data.len() - in_frames) as u32;
    (out, sum)
}

/// CRC-32 (IEEE 802.3, as zlib).
#[derive(Debug, Clone)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Crc32(0xffff_ffff)
    }
}

impl Crc32 {
    pub fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u32;
            for _ in 0..8 {
                self.0 = if self.0 & 1 != 0 {
                    (self.0 >> 1) ^ 0xedb8_8320
                } else {
                    self.0 >> 1
                };
            }
        }
    }
    pub fn value(&self) -> u32 {
        !self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_check_value() {
        let mut c = Crc32::default();
        c.update(b"123456789");
        assert_eq!(c.value(), 0xcbf4_3926);
    }

    #[test]
    fn strips_id3v2() {
        let mut d = b"ID3\x04\x00\x00\x00\x00\x00\x02xx".to_vec();
        d.extend_from_slice(&[0xff, 0xfb]);
        assert_eq!(strip_id3(&d), &[0xff, 0xfb]);
        assert_eq!(strip_id3(&[1, 2, 3]), &[1, 2, 3]);
    }
}

/// One granule of one channel as the hardware's granule decoder (milestone 2)
/// produces it: 22 long and 13 x 3 short scale factors, then the 576
/// Huffman-decoded values.
#[derive(Debug, Clone)]
pub struct GranuleRecord {
    pub frame: usize,
    pub gr: usize,
    pub ch: usize,
    pub sf: [u8; 61],
    pub is: [i32; 576],
}

impl GranuleRecord {
    /// The bytes the CRC covers: the 61 scale factors, then each value as a
    /// 16-bit little-endian integer.
    pub fn bytes(&self) -> Vec<u8> {
        let mut b = self.sf.to_vec();
        for v in self.is {
            b.extend_from_slice(&(v as i16).to_le_bytes());
        }
        b
    }
}

/// Granule records for every decodable frame of a stream (as sent), with
/// their count and CRC-32 over [`GranuleRecord::bytes`].
pub fn granules(data: &[u8]) -> (Vec<GranuleRecord>, u32) {
    let mut out = Vec::new();
    let mut crc = Crc32::default();
    let mut d = crate::Decoder::default();
    for (i, (pos, h)) in find_frames(data).into_iter().enumerate() {
        let Some(f) = d.decode_frame(&data[pos..pos + h.frame_len()]) else {
            continue;
        };
        for (gr, chans) in f.granules.iter().enumerate() {
            for (ch, t) in chans.iter().enumerate() {
                let mut sf = [0u8; 61];
                sf[..22].copy_from_slice(&t.sf.long);
                for band in 0..13 {
                    sf[22 + 3 * band..25 + 3 * band].copy_from_slice(&t.sf.short[band]);
                }
                let r = GranuleRecord {
                    frame: i,
                    gr,
                    ch,
                    sf,
                    is: t.is,
                };
                crc.update(&r.bytes());
                out.push(r);
            }
        }
    }
    (out, crc.value())
}

/// The decoder's word lengths (docs/mp3-decoder-plan.md): 26-bit data with 3
/// integer bits, 18-bit coefficients (17 fraction bits), 18-bit mantissas.
pub const HW: crate::fixed::Fmt = crate::fixed::Fmt {
    w: 26,
    i: 3,
    c: 19,
    m: 18,
};

/// The IMDCT's input for every granule and channel of every decodable
/// frame, from the fixed-point back end at the hardware's word lengths, with
/// the CRC-32 over each value as 32-bit little-endian.
pub fn spectra(data: &[u8]) -> (Vec<[i64; 576]>, u32) {
    let mut d = crate::Decoder::default();
    let mut fx = crate::fixed::FixedBackend::new(HW);
    fx.hybrid = Some(Vec::new());
    for (pos, h) in find_frames(data) {
        if let Some(f) = d.decode_frame(&data[pos..pos + h.frame_len()]) {
            fx.decode(&f);
        }
    }
    let out = fx.hybrid.take().unwrap_or_default();
    let mut crc = Crc32::default();
    for g in &out {
        for v in g {
            crc.update(&(*v as i32).to_le_bytes());
        }
    }
    (out, crc.value())
}

/// The IMDCT's output for every granule and channel (milestone 4), with the
/// CRC-32 over each value as 32-bit little-endian.
pub fn subbands(data: &[u8]) -> (Vec<[i64; 576]>, u32) {
    let mut d = crate::Decoder::default();
    let mut fx = crate::fixed::FixedBackend::new(HW);
    fx.subband = Some(Vec::new());
    for (pos, h) in find_frames(data) {
        if let Some(f) = d.decode_frame(&data[pos..pos + h.frame_len()]) {
            fx.decode(&f);
        }
    }
    let out = fx.subband.take().unwrap_or_default();
    let mut crc = Crc32::default();
    for g in &out {
        for v in g {
            crc.update(&(*v as i32).to_le_bytes());
        }
    }
    (out, crc.value())
}

/// The 16-bit PCM of every decodable frame (milestone 5), interleaved per
/// frame as the frame's channels come (one value per sample for mono), with
/// the CRC-32 over each value as 16-bit little-endian.
pub fn pcm(data: &[u8]) -> (Vec<i16>, u32) {
    let mut d = crate::Decoder::default();
    let mut fx = crate::fixed::FixedBackend::new(HW);
    let mut out = Vec::new();
    for (pos, h) in find_frames(data) {
        if let Some(f) = d.decode_frame(&data[pos..pos + h.frame_len()]) {
            out.extend(fx.decode(&f).1);
        }
    }
    let mut crc = Crc32::default();
    for v in &out {
        crc.update(&v.to_le_bytes());
    }
    (out, crc.value())
}
