//! Fixed-point back end: everything after Huffman decoding, with the
//! hardware's word lengths, to choose them (docs/mp3-decoder-plan.md,
//! "Word lengths and accuracy").
//!
//! Data words are `w`-bit two's complement with `i` integer bits (plus sign),
//! so `w - 1 - i` fraction bits. Coefficients are `c`-bit with `c - 2`
//! fraction bits (|coefficient| < 2). Multiply-accumulates keep full
//! products and round once (round half up) at the end, then saturate, as a
//! MULT18X18 chain with a wide accumulator would. |n|^(4/3) comes from a
//! table of `m`-bit normalised mantissas (n < 1024 directly; above that,
//! linear interpolation between multiples of 8), times a `m`-bit 2^(k/4).
//! The synthesis window is the standard's exact 18-bit integers.
//!
//! This model chooses word lengths; it becomes bit-exact with the RTL stage
//! by stage as each stage is designed.

use std::f64::consts::PI;

use crate::frame::Granule;
use crate::layer3::{
    imdct_window, intensity_gains, intensity_positions, is_short, ScaleFactors, AA_C,
};
use crate::tables::{PRETAB, SFB_LONG, SFB_SHORT, SYNTH_D};
use crate::DecodedFrame;

#[derive(Debug, Clone, Copy)]
pub struct Fmt {
    /// Data word width.
    pub w: u32,
    /// Integer bits (excluding sign).
    pub i: u32,
    /// Coefficient width.
    pub c: u32,
    /// |n|^(4/3) and 2^(k/4) mantissa width.
    pub m: u32,
}

impl Fmt {
    pub fn frac(&self) -> u32 {
        self.w - 1 - self.i
    }
    fn max(&self) -> i64 {
        (1i64 << (self.w - 1)) - 1
    }
    fn sat(&self, v: i128) -> i64 {
        v.clamp(-(self.max() as i128) - 1, self.max() as i128) as i64
    }
    fn coef(&self, x: f64) -> i64 {
        (x * (1u64 << (self.c - 2)) as f64).round() as i64
    }
    /// Sum of data x coefficient products, back to data format.
    fn mac(&self, acc: i128) -> i64 {
        self.sat(round_shift(acc, self.c as i64 - 2))
    }
    pub fn to_f64(&self, v: i64) -> f64 {
        v as f64 / (1u64 << self.frac()) as f64
    }
}

/// Round half up, then shift right by `s` (s may be negative: shift left).
fn round_shift(v: i128, s: i64) -> i128 {
    if s <= 0 {
        v << (-s)
    } else {
        (v + (1i128 << (s - 1))) >> s
    }
}

/// Coefficient tables in a given format.
struct Coefs {
    imdct_long: [[[i64; 18]; 36]; 4], // [block type][i][k], window folded in
    imdct_short: [[i64; 6]; 12],
    aa_cs: [i64; 8],
    aa_ca: [i64; 8],
    ms: i64,
    is_k: [(i64, i64); 7],
    synth_n: Vec<i64>, // [64][32]
}

impl Coefs {
    fn new(f: &Fmt) -> Coefs {
        let mut imdct_long = [[[0; 18]; 36]; 4];
        for bt in [0, 1, 3] {
            for i in 0..36 {
                for k in 0..18 {
                    let c = (PI / 72.0 * (2 * i + 1 + 18) as f64 * (2 * k + 1) as f64).cos();
                    imdct_long[bt][i][k] = f.coef(c * imdct_window(bt, i));
                }
            }
        }
        let mut imdct_short = [[0; 6]; 12];
        for i in 0..12 {
            for k in 0..6 {
                let c = (PI / 24.0 * (2 * i + 1 + 6) as f64 * (2 * k + 1) as f64).cos();
                imdct_short[i][k] = f.coef(c * imdct_window(2, i));
            }
        }
        let mut aa_cs = [0; 8];
        let mut aa_ca = [0; 8];
        for (i, &c) in AA_C.iter().enumerate() {
            let cs = 1.0 / (1.0 + c * c).sqrt();
            aa_cs[i] = f.coef(cs);
            aa_ca[i] = f.coef(c * cs);
        }
        let is_k = std::array::from_fn(|p| {
            let (kl, kr) = intensity_gains(p as u8);
            (f.coef(kl), f.coef(kr))
        });
        let synth_n = (0..64 * 32)
            .map(|ik| {
                let (i, k) = (ik / 32, ik % 32);
                f.coef(((16 + i) as f64 * (2 * k + 1) as f64 * PI / 64.0).cos())
            })
            .collect();
        Coefs {
            imdct_long,
            imdct_short,
            aa_cs,
            aa_ca,
            ms: f.coef(std::f64::consts::FRAC_1_SQRT_2),
            is_k,
            synth_n,
        }
    }
}

/// Stereo and antialias coefficients in format `f`, exactly as the back end
/// uses them: (antialias cs, antialias ca, mid/side 1/sqrt(2), intensity
/// (k_left, k_right) for positions 0..6).
#[allow(clippy::type_complexity)]
pub fn coef_tables(f: &Fmt) -> ([i64; 8], [i64; 8], i64, [(i64, i64); 7]) {
    let k = Coefs::new(f);
    (k.aa_cs, k.aa_ca, k.ms, k.is_k)
}

/// The synthesis matrix N[i][k] (64 x 32, row-major) in format `f`, as the
/// back end uses it.
pub fn synth_matrix(f: &Fmt) -> Vec<i64> {
    Coefs::new(f).synth_n
}

/// IMDCT coefficients (cosine x window) in format `f`, as the back end uses
/// them: long blocks [block type][i][k] (types 0, 1, 3) and the short block's
/// [i][k].
#[allow(clippy::type_complexity)]
pub fn imdct_coefs(f: &Fmt) -> ([[[i64; 18]; 36]; 4], [[i64; 6]; 12]) {
    let k = Coefs::new(f);
    (k.imdct_long, k.imdct_short)
}

/// |n|^(4/3) as (mantissa with `m` bits, value in [2^(m-1), 2^m), exponent):
/// value = mant * 2^(exp - (m - 1)).
pub fn pow43(n: u32, m: u32) -> (i64, i32) {
    if n < 1024 {
        return pow43_entry(n, m);
    }
    // n = 8k + f: interpolate between 16 k^(4/3) and 16 (k+1)^(4/3), i.e. the
    // table entries for k and k + 1 with 4 added to their exponents, in
    // integers: align the larger entry to the smaller's exponent (they
    // differ by at most 1), add (difference * f + 4) >> 3, renormalise.
    let (k, f) = (n >> 3, (n & 7) as i64);
    let (m0, e0) = pow43_entry(k, m);
    let (m1, e1) = pow43_entry(k + 1, m);
    let m1 = m1 << (e1 - e0);
    let mut v = m0 + (((m1 - m0) * f + 4) >> 3);
    let mut e = e0 + 4;
    if v >= 1 << m {
        v = (v + 1) >> 1;
        e += 1;
    }
    (v, e)
}

/// Table entry for n = 0..1026: n^(4/3) as an `m`-bit normalised mantissa
/// in [2^(m-1), 2^m) and exponent: n^(4/3) ~ mant * 2^(e - (m - 1)).
pub fn pow43_entry(n: u32, m: u32) -> (i64, i32) {
    if n == 0 {
        return (0, 0);
    }
    let v = (n as f64).powf(4.0 / 3.0);
    let mut e = v.log2().floor() as i32;
    let mut mant = (v / 2f64.powi(e) * (1u64 << (m - 1)) as f64).round() as i64;
    if mant >= 1 << m {
        mant >>= 1;
        e += 1;
    }
    (mant, e)
}

/// Fixed-point back end with its own overlap and synthesis state.
pub struct FixedBackend {
    pub f: Fmt,
    k: Coefs,
    overlap: [[[i64; 18]; 32]; 2],
    v: [[i64; 1024]; 2],
    /// Samples clipped at any stage (overflow).
    pub clipped: u64,
    /// Largest |fixed - float| after requantise, stereo, antialias, IMDCT, synthesis V.
    pub stage_err: [f64; 5],
    /// When set, the IMDCT's input (after requantise, stereo, reorder and
    /// antialias) of each granule and channel, in decoding order.
    pub hybrid: Option<Vec<[i64; 576]>>,
    /// When set, the IMDCT's output (overlap-added, frequency-inverted;
    /// `[18 * sb + t]`) of each granule and channel, in decoding order.
    pub subband: Option<Vec<[i64; 576]>>,
}

impl FixedBackend {
    pub fn new(f: Fmt) -> Self {
        FixedBackend {
            k: Coefs::new(&f),
            f,
            overlap: [[[0; 18]; 32]; 2],
            v: [[0; 1024]; 2],
            clipped: 0,
            stage_err: [0.0; 5],
            hybrid: None,
            subband: None,
        }
    }

    fn note(&mut self, stage: usize, x: &[i64; 576], float: &[f64; 576]) {
        for (a, b) in x.iter().zip(float) {
            let e = (self.f.to_f64(*a) - b).abs();
            if e > self.stage_err[stage] {
                self.stage_err[stage] = e;
            }
        }
    }

    fn sat_count(&mut self, v: i128) -> i64 {
        let s = self.f.sat(v);
        if s as i128 != v {
            self.clipped += 1;
        }
        s
    }

    /// Requantise one granule (integer Huffman values in, data words out).
    fn requantize(
        &mut self,
        g: &Granule,
        sf: &ScaleFactors,
        rate: usize,
        is: &[i32; 576],
    ) -> [i64; 576] {
        let m = self.f.m;
        let frac4: [i64; 4] = std::array::from_fn(|k| {
            (2f64.powf(k as f64 / 4.0) * (1u64 << (m - 1)) as f64).round() as i64
        });
        let mut out = [0i64; 576];
        let mut one = |i: usize, q: i32, out: &mut [i64; 576]| {
            let n = is[i].unsigned_abs();
            if n == 0 {
                return;
            }
            let (mant, e) = pow43(n, m);
            let prod = (mant * frac4[q.rem_euclid(4) as usize]) as i128;
            // value = prod * 2^(e - 2(m-1) + floor(q/4)); data = value * 2^frac
            let shift = -(e - 2 * (m as i32 - 1) + q.div_euclid(4) + self.f.frac() as i32);
            let mag = if shift < -40 {
                i128::MAX / 4
            } else {
                round_shift(prod, shift as i64)
            };
            let v = self.sat_count(mag);
            out[i] = if is[i] < 0 { -v } else { v };
        };
        let base = g.global_gain as i32 - 210;
        let scale = 2 * (1 + g.scalefac_scale as i32);
        let long_q = |band: usize| {
            base - scale * (sf.long[band] as i32 + if g.preflag { PRETAB[band] as i32 } else { 0 })
        };
        let long_bands = if !is_short(g) {
            22
        } else if g.mixed_block {
            8
        } else {
            0
        };
        for band in 0..long_bands {
            for i in SFB_LONG[rate][band] as usize..SFB_LONG[rate][band + 1] as usize {
                one(i, long_q(band), &mut out);
            }
        }
        if is_short(g) {
            let first = if g.mixed_block { 3 } else { 0 };
            for band in first..13 {
                let (lo, hi) = (
                    SFB_SHORT[rate][band] as usize,
                    SFB_SHORT[rate][band + 1] as usize,
                );
                let width = hi - lo;
                for w in 0..3 {
                    let q = base - 8 * g.subblock_gain[w] as i32 - scale * sf.short[band][w] as i32;
                    for j in 0..width {
                        one(3 * lo + w * width + j, q, &mut out);
                    }
                }
            }
        }
        out
    }

    /// Decode a frame's back end. Returns interleaved PCM (full scale = 1.0,
    /// at the data format's precision) and the 16-bit output.
    pub fn decode(&mut self, fr: &DecodedFrame) -> (Vec<f64>, Vec<i16>) {
        let h = &fr.header;
        let nch = h.channels();
        let rate = h.rate_index;
        let mut pcm = vec![0.0; 1152 * nch];
        let mut pcm16 = vec![0i16; 1152 * nch];
        for (gi, gr) in fr.granules.iter().enumerate() {
            let mut xr: Vec<[i64; 576]> = (0..nch)
                .map(|ch| self.requantize(&fr.side.gr[gi][ch], &gr[ch].sf, rate, &gr[ch].is))
                .collect();
            for ch in 0..nch {
                self.note(0, &xr[ch], &gr[ch].xr);
            }
            if nch == 2 && (h.ms_stereo() || h.intensity_stereo()) {
                self.stereo(fr, gi, &mut xr);
            }
            for ch in 0..nch {
                self.note(1, &xr[ch], &gr[ch].stereo);
            }
            for ch in 0..nch {
                let g = fr.side.gr[gi][ch];
                let mut x = xr[ch];
                reorder(&g, rate, &mut x);
                self.antialias(&g, &mut x);
                self.note(2, &x, &gr[ch].hybrid);
                if let Some(h) = &mut self.hybrid {
                    h.push(x);
                }
                let sub = self.imdct(&g, &x, ch);
                if let Some(s) = &mut self.subband {
                    s.push(sub);
                }
                self.note(3, &sub, &gr[ch].subband);
                for t in 0..18 {
                    let s: [i64; 32] = std::array::from_fn(|sb| sub[18 * sb + t]);
                    for (j, (p, p16)) in self.synth(ch, &s).into_iter().enumerate() {
                        let at = (gi * 576 + 32 * t + j) * nch + ch;
                        pcm[at] = p;
                        pcm16[at] = p16;
                    }
                }
            }
        }
        (pcm, pcm16)
    }

    fn stereo(&mut self, fr: &DecodedFrame, gi: usize, xr: &mut [[i64; 576]]) {
        let h = &fr.header;
        let r = &fr.granules[gi][1];
        let pos = if h.intensity_stereo() {
            intensity_positions(&fr.side.gr[gi][1], &r.sf, h.rate_index, &r.is)
        } else {
            [None; 576]
        };
        for i in 0..576 {
            let (a, b) = (xr[0][i] as i128, xr[1][i] as i128);
            if let Some(p) = pos[i] {
                let (kl, kr) = self.k.is_k[p as usize];
                xr[0][i] = self.f.mac(a * kl as i128);
                xr[1][i] = self.f.mac(a * kr as i128);
            } else if h.ms_stereo() {
                let ms = self.k.ms as i128;
                xr[0][i] = self.f.mac((a + b) * ms);
                xr[1][i] = self.f.mac((a - b) * ms);
            }
        }
    }

    fn antialias(&mut self, g: &Granule, x: &mut [i64; 576]) {
        let boundaries = if !is_short(g) {
            32
        } else if g.mixed_block {
            2
        } else {
            return;
        };
        for sb in 1..boundaries {
            for i in 0..8 {
                let (a, b) = (x[18 * sb - 1 - i] as i128, x[18 * sb + i] as i128);
                let (cs, ca) = (self.k.aa_cs[i] as i128, self.k.aa_ca[i] as i128);
                x[18 * sb - 1 - i] = self.f.mac(a * cs - b * ca);
                x[18 * sb + i] = self.f.mac(b * cs + a * ca);
            }
        }
    }

    fn imdct(&mut self, g: &Granule, x: &[i64; 576], ch: usize) -> [i64; 576] {
        let mut out = [0i64; 576];
        for sb in 0..32 {
            let bt = if g.window_switching && g.mixed_block && sb < 2 {
                0
            } else {
                g.block_type as usize
            };
            let xs = &x[18 * sb..18 * sb + 18];
            let mut z = [0i64; 36];
            if bt == 2 {
                for w in 0..3 {
                    for i in 0..12 {
                        let acc: i128 = (0..6)
                            .map(|k| xs[3 * k + w] as i128 * self.k.imdct_short[i][k] as i128)
                            .sum();
                        z[6 + 6 * w + i] = self
                            .f
                            .sat(z[6 + 6 * w + i] as i128 + self.f.mac(acc) as i128);
                    }
                }
            } else {
                for i in 0..36 {
                    let acc: i128 = (0..18)
                        .map(|k| xs[k] as i128 * self.k.imdct_long[bt][i][k] as i128)
                        .sum();
                    z[i] = self.f.mac(acc);
                }
            }
            for t in 0..18 {
                let mut v = self.sat_count(z[t] as i128 + self.overlap[ch][sb][t] as i128);
                if sb % 2 == 1 && t % 2 == 1 {
                    v = -v;
                }
                out[18 * sb + t] = v;
                self.overlap[ch][sb][t] = z[t + 18];
            }
        }
        out
    }

    /// 32 subband samples in, 32 PCM samples out: (data precision, 16-bit).
    fn synth(&mut self, ch: usize, s: &[i64; 32]) -> Vec<(f64, i16)> {
        let v = &mut self.v[ch];
        v.copy_within(0..960, 64);
        for i in 0..64 {
            let acc: i128 = (0..32)
                .map(|k| self.k.synth_n[i * 32 + k] as i128 * s[k] as i128)
                .sum();
            v[i] = self.f.sat(round_shift(acc, self.f.c as i64 - 2));
        }
        let f = self.f;
        (0..32)
            .map(|j| {
                let mut acc: i128 = 0;
                for i in 0..8 {
                    acc += v[128 * i + j] as i128 * SYNTH_D[64 * i + j] as i128;
                    acc += v[128 * i + 96 + j] as i128 * SYNTH_D[64 * i + 32 + j] as i128;
                }
                // D has 16 fraction bits.
                let full = round_shift(acc, 16);
                let p16 = round_shift(acc, 16 + f.frac() as i64 - 15).clamp(-32768, 32767) as i16;
                (full as f64 / (1u64 << f.frac()) as f64, p16)
            })
            .collect()
    }
}

fn reorder(g: &Granule, rate: usize, x: &mut [i64; 576]) {
    if !is_short(g) {
        return;
    }
    let src = *x;
    let first = if g.mixed_block { 3 } else { 0 };
    for band in first..13 {
        let (lo, hi) = (
            SFB_SHORT[rate][band] as usize,
            SFB_SHORT[rate][band + 1] as usize,
        );
        let width = hi - lo;
        for w in 0..3 {
            for k in 0..width {
                x[3 * lo + 3 * k + w] = src[3 * lo + w * width + k];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_and_shifts() {
        assert_eq!(round_shift(5, 1), 3); // 2.5 -> 3 (half up)
        assert_eq!(round_shift(-5, 1), -2); // -2.5 -> -2 (half up)
        assert_eq!(round_shift(3, -2), 12);
    }

    /// The table (and its interpolation above 1023) is within 1.5 units of
    /// the 18-bit mantissa's last place for every Huffman value.
    #[test]
    fn pow43_table_accuracy() {
        for n in 1..=8206u32 {
            let (mant, e) = pow43(n, 18);
            assert!(
                (1 << 17..1 << 18).contains(&mant),
                "n {n}: mantissa {mant} not normalised"
            );
            let got = mant as f64 * 2f64.powi(e - 17);
            let want = (n as f64).powf(4.0 / 3.0);
            let ulp = 2f64.powi(e - 17);
            assert!((got - want).abs() <= 1.5 * ulp, "n {n}: {got} vs {want}");
        }
    }
}
