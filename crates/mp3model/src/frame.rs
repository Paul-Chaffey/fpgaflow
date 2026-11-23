//! MPEG-1 Layer III frame header and side information.

use crate::bits::Bits;

const BITRATES: [u32; 15] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
/// Sample rates for header codes 0, 1, 2 (MPEG-1).
pub const RATES: [u32; 3] = [44100, 48000, 32000];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Stereo,
    JointStereo,
    DualChannel,
    Mono,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub crc: bool,
    pub bitrate_kbps: u32,
    /// 0 = 44.1, 1 = 48, 2 = 32 kHz (index into the band tables).
    pub rate_index: usize,
    pub padding: bool,
    pub mode: Mode,
    pub mode_ext: u8,
}

impl Header {
    /// Parse 4 bytes. Only MPEG-1 Layer III with a fixed bitrate is accepted.
    pub fn parse(h: &[u8]) -> Option<Header> {
        if h.len() < 4 || h[0] != 0xff || h[1] & 0xe0 != 0xe0 {
            return None;
        }
        let version = (h[1] >> 3) & 3; // 3 = MPEG-1
        let layer = (h[1] >> 1) & 3; // 1 = Layer III
        let br = (h[2] >> 4) as usize;
        let sr = ((h[2] >> 2) & 3) as usize;
        if version != 3 || layer != 1 || br == 0 || br == 15 || sr == 3 {
            return None;
        }
        Some(Header {
            crc: h[1] & 1 == 0,
            bitrate_kbps: BITRATES[br],
            rate_index: sr,
            padding: h[2] & 2 != 0,
            mode: match h[3] >> 6 {
                0 => Mode::Stereo,
                1 => Mode::JointStereo,
                2 => Mode::DualChannel,
                _ => Mode::Mono,
            },
            mode_ext: (h[3] >> 4) & 3,
        })
    }

    pub fn channels(&self) -> usize {
        if self.mode == Mode::Mono {
            1
        } else {
            2
        }
    }

    pub fn sample_rate(&self) -> u32 {
        RATES[self.rate_index]
    }

    /// Frame length in bytes, header included.
    pub fn frame_len(&self) -> usize {
        (144_000 * self.bitrate_kbps / self.sample_rate()) as usize + self.padding as usize
    }

    pub fn side_info_len(&self) -> usize {
        if self.channels() == 1 {
            17
        } else {
            32
        }
    }

    pub fn ms_stereo(&self) -> bool {
        self.mode == Mode::JointStereo && self.mode_ext & 2 != 0
    }

    pub fn intensity_stereo(&self) -> bool {
        self.mode == Mode::JointStereo && self.mode_ext & 1 != 0
    }
}

/// Side information for one granule of one channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Granule {
    pub part2_3_length: u32,
    pub big_values: u32,
    pub global_gain: u32,
    pub scalefac_compress: u32,
    pub window_switching: bool,
    pub block_type: u32,
    pub mixed_block: bool,
    pub table_select: [u32; 3],
    pub subblock_gain: [u32; 3],
    pub region0_count: u32,
    pub region1_count: u32,
    pub preflag: bool,
    pub scalefac_scale: bool,
    pub count1_table: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SideInfo {
    pub main_data_begin: usize,
    pub scfsi: [[bool; 4]; 2],
    pub gr: [[Granule; 2]; 2], // [granule][channel]
}

impl SideInfo {
    pub fn parse(data: &[u8], channels: usize) -> SideInfo {
        let mut b = Bits::new(data);
        let mut si = SideInfo {
            main_data_begin: b.read(9) as usize,
            ..Default::default()
        };
        b.read(if channels == 1 { 5 } else { 3 }); // private bits
        for ch in 0..channels {
            for band in 0..4 {
                si.scfsi[ch][band] = b.bit() == 1;
            }
        }
        for gr in 0..2 {
            for ch in 0..channels {
                let g = &mut si.gr[gr][ch];
                g.part2_3_length = b.read(12);
                g.big_values = b.read(9);
                g.global_gain = b.read(8);
                g.scalefac_compress = b.read(4);
                g.window_switching = b.bit() == 1;
                if g.window_switching {
                    g.block_type = b.read(2);
                    g.mixed_block = b.bit() == 1;
                    g.table_select[0] = b.read(5);
                    g.table_select[1] = b.read(5);
                    for w in 0..3 {
                        g.subblock_gain[w] = b.read(3);
                    }
                    // Implicit region sizes (MPEG-1).
                    g.region0_count = if g.block_type == 2 && !g.mixed_block {
                        8
                    } else {
                        7
                    };
                    g.region1_count = 36;
                } else {
                    for t in 0..3 {
                        g.table_select[t] = b.read(5);
                    }
                    g.region0_count = b.read(4);
                    g.region1_count = b.read(3);
                }
                g.preflag = b.bit() == 1;
                g.scalefac_scale = b.bit() == 1;
                g.count1_table = b.bit();
            }
        }
        si
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_and_length() {
        // FF FB 90 64: MPEG-1 L3, no CRC, 128 kbit/s, 44.1 kHz, joint stereo, MS.
        let h = Header::parse(&[0xff, 0xfb, 0x90, 0x64]).unwrap();
        assert_eq!(h.bitrate_kbps, 128);
        assert_eq!(h.sample_rate(), 44100);
        assert_eq!(h.mode, Mode::JointStereo);
        assert!(h.ms_stereo() && !h.intensity_stereo());
        assert_eq!(h.frame_len(), 417);
        assert!(
            Header::parse(&[0xff, 0xf3, 0x90, 0x64]).is_none(),
            "MPEG-2 is not accepted"
        );
    }
}
