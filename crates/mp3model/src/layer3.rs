//! MPEG-1 Layer III decoding stages, in floating point (f64). Each function
//! is one block of the hardware decoder; `Decoder` in lib.rs chains them.

use std::f64::consts::PI;
use std::sync::OnceLock;

use crate::bits::Bits;
use crate::frame::Granule;
use crate::tables::{Code, COUNT1, LINBITS, PAIRS, PRETAB, SFB_LONG, SFB_SHORT, SLEN};

/// Scale factors of one channel in one granule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScaleFactors {
    pub long: [u8; 22],
    pub short: [[u8; 3]; 13],
}

pub fn is_short(g: &Granule) -> bool {
    g.window_switching && g.block_type == 2
}

/// Read scale factors (part 2). `prev` is the same channel's granule 0, used
/// where scfsi says granule 1 shares them.
pub fn read_scalefactors(
    b: &mut Bits,
    g: &Granule,
    scfsi: &[bool; 4],
    gr: usize,
    prev: &ScaleFactors,
) -> ScaleFactors {
    let (slen1, slen2) = SLEN[g.scalefac_compress as usize];
    let (slen1, slen2) = (slen1 as u32, slen2 as u32);
    let mut sf = ScaleFactors::default();
    if is_short(g) {
        let first_short = if g.mixed_block {
            for band in 0..8 {
                sf.long[band] = b.read(slen1) as u8;
            }
            3
        } else {
            0
        };
        for band in first_short..12 {
            let n = if band < 6 { slen1 } else { slen2 };
            for w in 0..3 {
                sf.short[band][w] = b.read(n) as u8;
            }
        }
    } else {
        const GROUPS: [(usize, usize); 4] = [(0, 6), (6, 11), (11, 16), (16, 21)];
        for (i, &(lo, hi)) in GROUPS.iter().enumerate() {
            let n = if i < 2 { slen1 } else { slen2 };
            for band in lo..hi {
                sf.long[band] = if gr == 1 && scfsi[i] {
                    prev.long[band]
                } else {
                    b.read(n) as u8
                };
            }
        }
    }
    sf
}

/// A Huffman code as a binary tree: node i = [child for 0, child for 1];
/// a child >= 0 is a node, < 0 is a leaf `-(1 + (x << 4 | y))`.
pub struct Tree(Vec<[i32; 2]>);

impl Tree {
    fn build(codes: &[Code]) -> Tree {
        let mut nodes = vec![[0i32; 2]];
        for c in codes {
            let mut n = 0usize;
            for i in (0..c.len).rev() {
                let bit = ((c.code as u32 >> i) & 1) as usize;
                if i == 0 {
                    nodes[n][bit] = -(1 + ((c.x as i32) << 4 | c.y as i32));
                } else {
                    if nodes[n][bit] == 0 {
                        nodes.push([0, 0]);
                        nodes[n][bit] = (nodes.len() - 1) as i32;
                    }
                    n = nodes[n][bit] as usize;
                }
            }
        }
        Tree(nodes)
    }

    /// Decode one code: (x, y), or the v w x y flags for count1 tables.
    fn decode(&self, b: &mut Bits) -> (u32, u32) {
        let mut n = 0usize;
        loop {
            let next = self.0[n][b.bit() as usize];
            if next < 0 {
                let v = (-next - 1) as u32;
                return (v >> 4, v & 15);
            }
            n = next as usize;
        }
    }

    pub fn nodes(&self) -> &[[i32; 2]] {
        &self.0
    }
}

pub fn pair_trees() -> &'static [Option<Tree>; 32] {
    static T: OnceLock<[Option<Tree>; 32]> = OnceLock::new();
    T.get_or_init(|| std::array::from_fn(|t| (!PAIRS[t].is_empty()).then(|| Tree::build(PAIRS[t]))))
}

pub fn count1_trees() -> &'static [Tree; 2] {
    static T: OnceLock<[Tree; 2]> = OnceLock::new();
    T.get_or_init(|| [Tree::build(&COUNT1[0]), Tree::build(&COUNT1[1])])
}

/// Huffman-decode part 3 of a granule into 576 integers. `end` is the bit
/// position where the granule's part 2 + 3 ends. Returns how many values
/// were decoded (the rest are zero).
pub fn huffman(b: &mut Bits, end: usize, g: &Granule, rate: usize, is: &mut [i32; 576]) -> usize {
    *is = [0; 576];
    let big = (g.big_values as usize * 2).min(576);
    let (r1, r2) = if g.window_switching {
        (36, 576)
    } else {
        let r1 = SFB_LONG[rate][(g.region0_count as usize + 1).min(22)] as usize;
        let r2 = SFB_LONG[rate][(g.region0_count as usize + g.region1_count as usize + 2).min(22)]
            as usize;
        (r1, r2)
    };
    let mut i = 0;
    while i < big {
        let t = g.table_select[if i < r1 {
            0
        } else if i < r2 {
            1
        } else {
            2
        }] as usize;
        let (mut x, mut y) = match &pair_trees()[t] {
            Some(tree) => tree.decode(b),
            None => (0, 0),
        };
        let lin = LINBITS[t] as u32;
        let mut signed = |v: &mut u32| -> i32 {
            if lin > 0 && *v == 15 {
                *v += b.read(lin);
            }
            if *v != 0 && b.bit() == 1 {
                -(*v as i32)
            } else {
                *v as i32
            }
        };
        is[i] = signed(&mut x);
        is[i + 1] = signed(&mut y);
        i += 2;
    }
    let tree = &count1_trees()[g.count1_table as usize];
    while i + 4 <= 576 && b.pos < end {
        let (flags, _) = tree.decode(b); // v w x y in bits 3..0
        let mut q = [0i32; 4];
        for (k, item) in q.iter_mut().enumerate() {
            if flags & (8 >> k) != 0 {
                *item = if b.bit() == 1 { -1 } else { 1 };
            }
        }
        if b.pos > end {
            break; // the last quadruple overran the granule: dropped
        }
        is[i..i + 4].copy_from_slice(&q);
        i += 4;
    }
    b.pos = end;
    i
}

/// `|x|^(4/3)` with x's sign.
fn pow43(x: i32) -> f64 {
    let a = (x.unsigned_abs() as f64).powf(4.0 / 3.0);
    if x < 0 {
        -a
    } else {
        a
    }
}

/// Requantise: integers to spectral values. Short-block data stays in the
/// coded order (per band: window 0, 1, 2).
pub fn requantize(g: &Granule, sf: &ScaleFactors, rate: usize, is: &[i32; 576]) -> [f64; 576] {
    let mut xr = [0.0; 576];
    let gain = |extra: f64| 2f64.powf((g.global_gain as f64 - 210.0 - extra) / 4.0);
    let mult = if g.scalefac_scale { 1.0 } else { 0.5 };
    let long = |band: usize, i: usize, xr: &mut [f64; 576]| {
        let p = if g.preflag { PRETAB[band] } else { 0 } as f64;
        xr[i] = pow43(is[i]) * gain(0.0) * 2f64.powf(-mult * (sf.long[band] as f64 + p));
    };
    if !is_short(g) {
        for band in 0..22 {
            for i in SFB_LONG[rate][band] as usize..SFB_LONG[rate][band + 1] as usize {
                long(band, i, &mut xr);
            }
        }
        return xr;
    }
    let first_short = if g.mixed_block {
        for band in 0..8 {
            for i in SFB_LONG[rate][band] as usize..SFB_LONG[rate][band + 1] as usize {
                long(band, i, &mut xr);
            }
        }
        3
    } else {
        0
    };
    for band in first_short..13 {
        let (lo, hi) = (
            SFB_SHORT[rate][band] as usize,
            SFB_SHORT[rate][band + 1] as usize,
        );
        let width = hi - lo;
        for w in 0..3 {
            let k =
                gain(8.0 * g.subblock_gain[w] as f64) * 2f64.powf(-mult * sf.short[band][w] as f64);
            for j in 0..width {
                let i = 3 * lo + w * width + j;
                xr[i] = pow43(is[i]) * k;
            }
        }
    }
    xr
}

/// Which samples are intensity-coded, and with which position (0..6; 7 is
/// "illegal" and means the band is coded normally, so it never appears).
/// Intensity starts above the right channel's last non-zero band (per window
/// for short blocks).
pub fn intensity_positions(
    g: &Granule,
    sf_r: &ScaleFactors,
    rate: usize,
    is_r: &[i32; 576],
) -> [Option<u8>; 576] {
    let mut pos = [None; 576];
    let mark = |lo: usize, hi: usize, p: u8, pos: &mut [Option<u8>; 576]| {
        if p != 7 {
            for x in &mut pos[lo..hi] {
                *x = Some(p);
            }
        }
    };
    if !is_short(g) {
        let start = match is_r.iter().rposition(|&v| v != 0) {
            None => 0,
            Some(i) => band_of(&SFB_LONG[rate], i) + 1,
        };
        for band in start..22 {
            mark(
                SFB_LONG[rate][band] as usize,
                SFB_LONG[rate][band + 1] as usize,
                sf_r.long[band.min(20)],
                &mut pos,
            );
        }
        return pos;
    }
    let first_short = if g.mixed_block { 3 } else { 0 };
    let mut short_part_zero = true;
    for w in 0..3 {
        // Last non-zero band in this window (coded order: band, window).
        let mut start = first_short;
        for band in (first_short..13).rev() {
            let (lo, hi) = (
                SFB_SHORT[rate][band] as usize,
                SFB_SHORT[rate][band + 1] as usize,
            );
            let base = 3 * lo + w * (hi - lo);
            if is_r[base..base + hi - lo].iter().any(|&v| v != 0) {
                start = band + 1;
                break;
            }
        }
        if start > first_short {
            short_part_zero = false;
        }
        for band in start..13 {
            let (lo, hi) = (
                SFB_SHORT[rate][band] as usize,
                SFB_SHORT[rate][band + 1] as usize,
            );
            let base = 3 * lo + w * (hi - lo);
            mark(base, base + hi - lo, sf_r.short[band.min(11)][w], &mut pos);
        }
    }
    if g.mixed_block && short_part_zero {
        // Nothing in any short window: intensity may start in the long part.
        let start = match is_r[..36].iter().rposition(|&v| v != 0) {
            None => 0,
            Some(i) => band_of(&SFB_LONG[rate], i) + 1,
        };
        for band in start..8 {
            mark(
                SFB_LONG[rate][band] as usize,
                SFB_LONG[rate][band + 1] as usize,
                sf_r.long[band],
                &mut pos,
            );
        }
    }
    pos
}

/// Intensity stereo gains (k_left, k_right) for position 0..6.
pub fn intensity_gains(p: u8) -> (f64, f64) {
    if p == 6 {
        (1.0, 0.0)
    } else {
        let t = (p as f64 * PI / 12.0).tan();
        (t / (1.0 + t), 1.0 / (1.0 + t))
    }
}

/// Joint stereo: intensity (above the right channel's last non-zero band)
/// and mid/side (everywhere else).
#[allow(clippy::too_many_arguments)]
pub fn stereo(
    g: &Granule, // right channel's granule (both share block type in joint stereo)
    sf_r: &ScaleFactors,
    rate: usize,
    is_r: &[i32; 576],
    ms: bool,
    intensity: bool,
    l: &mut [f64; 576],
    r: &mut [f64; 576],
) {
    let pos = if intensity {
        intensity_positions(g, sf_r, rate, is_r)
    } else {
        [None; 576]
    };
    let s = std::f64::consts::FRAC_1_SQRT_2;
    for i in 0..576 {
        if let Some(p) = pos[i] {
            let (kl, kr) = intensity_gains(p);
            let v = l[i];
            l[i] = v * kl;
            r[i] = v * kr;
        } else if ms {
            let (m, d) = (l[i], r[i]);
            l[i] = (m + d) * s;
            r[i] = (m - d) * s;
        }
    }
}

fn band_of(edges: &[u16], i: usize) -> usize {
    edges
        .windows(2)
        .position(|e| (e[0] as usize..e[1] as usize).contains(&i))
        .unwrap_or(edges.len() - 2)
}

/// Short blocks: from coded order (band, window, frequency) to (band,
/// frequency, window), which is what the short IMDCT reads.
pub fn reorder(g: &Granule, rate: usize, xr: &mut [f64; 576]) {
    if !is_short(g) {
        return;
    }
    let src = *xr;
    let first_short = if g.mixed_block { 3 } else { 0 };
    for band in first_short..13 {
        let (lo, hi) = (
            SFB_SHORT[rate][band] as usize,
            SFB_SHORT[rate][band + 1] as usize,
        );
        let width = hi - lo;
        for w in 0..3 {
            for k in 0..width {
                xr[3 * lo + 3 * k + w] = src[3 * lo + w * width + k];
            }
        }
    }
}

pub const AA_C: [f64; 8] = [
    -0.6, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037,
];

/// Alias reduction butterflies between subbands (long blocks: all 31
/// boundaries; mixed: the first; short: none).
pub fn antialias(g: &Granule, xr: &mut [f64; 576]) {
    let boundaries = if !is_short(g) {
        32
    } else if g.mixed_block {
        2
    } else {
        return;
    };
    for sb in 1..boundaries {
        for (i, &c) in AA_C.iter().enumerate() {
            let cs = 1.0 / (1.0 + c * c).sqrt();
            let ca = c * cs;
            let (a, b) = (xr[18 * sb - 1 - i], xr[18 * sb + i]);
            xr[18 * sb - 1 - i] = a * cs - b * ca;
            xr[18 * sb + i] = b * cs + a * ca;
        }
    }
}

/// IMDCT windows for block types 0..3 (type 2 = the 12-point short window,
/// in the first 12 entries).
pub fn imdct_window(bt: usize, i: usize) -> f64 {
    let long = |i: usize| (PI / 36.0 * (i as f64 + 0.5)).sin();
    let short = |i: usize| (PI / 12.0 * (i as f64 + 0.5)).sin();
    match bt {
        0 => long(i),
        1 => match i {
            0..=17 => long(i),
            18..=23 => 1.0,
            24..=29 => short(i - 18),
            _ => 0.0,
        },
        3 => match i {
            0..=5 => 0.0,
            6..=11 => short(i - 6),
            12..=17 => 1.0,
            _ => long(i),
        },
        _ => short(i),
    }
}

/// Inverse MDCT, windowing and overlap-add for one channel; `overlap` holds
/// the second halves from the previous granule. Output: 18 time samples per
/// subband (`out[18 * sb + t]`), frequency inversion applied.
pub fn imdct(g: &Granule, xr: &[f64; 576], overlap: &mut [[f64; 18]; 32]) -> [f64; 576] {
    let mut out = [0.0; 576];
    for sb in 0..32 {
        let bt = if g.window_switching && g.mixed_block && sb < 2 {
            0
        } else {
            g.block_type as usize
        };
        let x = &xr[18 * sb..18 * sb + 18];
        let mut z = [0.0; 36];
        if bt == 2 {
            for w in 0..3 {
                for i in 0..12 {
                    let mut s = 0.0;
                    for k in 0..6 {
                        s += x[3 * k + w]
                            * (PI / 24.0 * (2 * i + 1 + 6) as f64 * (2 * k + 1) as f64).cos();
                    }
                    z[6 + 6 * w + i] += s * imdct_window(2, i);
                }
            }
        } else {
            for (i, zi) in z.iter_mut().enumerate() {
                let mut s = 0.0;
                for (k, xk) in x.iter().enumerate() {
                    s += xk * (PI / 72.0 * (2 * i + 1 + 18) as f64 * (2 * k + 1) as f64).cos();
                }
                *zi = s * imdct_window(bt, i);
            }
        }
        for t in 0..18 {
            out[18 * sb + t] = z[t] + overlap[sb][t];
            overlap[sb][t] = z[t + 18];
        }
        if sb % 2 == 1 {
            for t in (1..18).step_by(2) {
                out[18 * sb + t] = -out[18 * sb + t];
            }
        }
    }
    out
}

/// Polyphase synthesis filterbank state for one channel.
pub struct Synth {
    v: [f64; 1024],
    /// Largest |V| computed so far (for choosing fixed-point formats).
    pub v_peak: f64,
}

impl Default for Synth {
    fn default() -> Self {
        Synth {
            v: [0.0; 1024],
            v_peak: 0.0,
        }
    }
}

impl Synth {
    /// 32 subband samples in, 32 PCM samples out (ISO 11172-3 figure A.2).
    pub fn step(&mut self, s: &[f64; 32]) -> [f64; 32] {
        static N: OnceLock<Vec<f64>> = OnceLock::new();
        let n = N.get_or_init(|| {
            (0..64 * 32)
                .map(|ik| {
                    let (i, k) = (ik / 32, ik % 32);
                    ((16 + i) as f64 * (2 * k + 1) as f64 * PI / 64.0).cos()
                })
                .collect()
        });
        self.v.copy_within(0..960, 64);
        for i in 0..64 {
            self.v[i] = (0..32).map(|k| n[i * 32 + k] * s[k]).sum();
        }
        self.v_peak = self.v[..64].iter().fold(self.v_peak, |m, v| m.max(v.abs()));
        let mut out = [0.0; 32];
        for (j, o) in out.iter_mut().enumerate() {
            let mut acc = 0.0;
            for i in 0..8 {
                acc += self.v[128 * i + j] * d(64 * i + j);
                acc += self.v[128 * i + 96 + j] * d(64 * i + 32 + j);
            }
            *o = acc;
        }
        out
    }
}

/// Synthesis window coefficient D[i].
pub fn d(i: usize) -> f64 {
    crate::tables::SYNTH_D[i] as f64 / 65536.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every code of every table decodes back to its own value.
    #[test]
    fn every_huffman_code_round_trips() {
        let mut n = 0;
        let check = |tree: &Tree, c: &Code| {
            let bits: Vec<u8> = {
                let v = (c.code as u32) << (32 - c.len as u32);
                v.to_be_bytes().to_vec()
            };
            let mut b = Bits::new(&bits);
            let got = tree.decode(&mut b);
            assert_eq!(
                got,
                (c.x as u32, c.y as u32),
                "code {:0w$b}",
                c.code,
                w = c.len as usize
            );
            assert_eq!(b.pos, c.len as usize);
        };
        for t in 0..32 {
            if let Some(tree) = &pair_trees()[t] {
                for c in PAIRS[t] {
                    check(tree, c);
                    n += 1;
                }
            }
        }
        for k in 0..2 {
            for c in &COUNT1[k] {
                check(&count1_trees()[k], c);
            }
        }
        assert_eq!(
            n,
            4 + 9 + 9 + 16 + 16 + 3 * 36 + 3 * 64 + 2 * 256 + 16 * 256
        );
    }
}
