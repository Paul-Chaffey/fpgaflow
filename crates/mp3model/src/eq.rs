//! Tone control after the decoder: the RC10 MP3 player's bass boost
//! (`examples/rc10-mp3/rtl/eq.sv`), bit-exact.
//!
//! A low-shelf biquad (RBJ cookbook, S = 1, corner [`BASS_HZ`]) per channel,
//! with automatic headroom: the b coefficients are scaled down by the boost,
//! so the peak gain is 0 dB and the bass is "boosted" by turning the rest
//! down. Level 0 is the identity.
//!
//! Fixed point (chosen by measurement, see the RC10 EQ plan):
//! - data 26-bit: the 16-bit sample << 8, plus 2 bits of headroom;
//! - coefficients 25-bit Q2.23, in MAC form `[b0, b1, b2, -a1, -a2]`;
//! - acc = e + Σ c·d over (x, x1, x2, y1, y2); y = (acc + 2^22) >> 23,
//!   saturated to 26 bits; e = acc − (y << 23) before saturation
//!   (first-order error feedback: the rounding noise stays near 0.005 LSB);
//! - output: (y + 128) >> 8, saturated to 16 bits.

use std::f64::consts::PI;

/// Sample rates for rate index 0, 1, 2 (as the decoder's header index).
pub const RATES: [f64; 3] = [44100.0, 48000.0, 32000.0];
/// Boost for levels 0..3.
pub const BASS_DB: [f64; 4] = [0.0, 4.0, 8.0, 12.0];
/// Shelf corner frequency.
pub const BASS_HZ: f64 = 100.0;
/// Fraction bits of the coefficients (Q2.23, 25-bit).
pub const COEF_FRAC: u32 = 23;
/// Bits below the 16-bit sample in the data path.
pub const DATA_FRAC: u32 = 8;
/// Width of the data path (signed).
pub const DATA_BITS: u32 = 26;
/// Width of the coefficients (signed).
pub const COEF_BITS: u32 = 25;

/// Normalised low-shelf `[b0, b1, b2, a1, a2]` (a0 = 1) with the b's scaled
/// by 10^(−gain/20), so the response is 0 dB in the bass and −gain above.
pub fn low_shelf(fs: f64, f0: f64, gain_db: f64) -> [f64; 5] {
    let a = 10f64.powf(gain_db / 40.0);
    let w = 2.0 * PI * f0 / fs;
    let (c, al) = (w.cos(), w.sin() / 2.0 * 2f64.sqrt());
    let sa = 2.0 * a.sqrt() * al;
    let b0 = a * ((a + 1.0) - (a - 1.0) * c + sa);
    let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * c);
    let b2 = a * ((a + 1.0) - (a - 1.0) * c - sa);
    let a0 = (a + 1.0) + (a - 1.0) * c + sa;
    let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * c);
    let a2 = (a + 1.0) + (a - 1.0) * c - sa;
    let h = 10f64.powf(-gain_db / 20.0);
    [h * b0 / a0, h * b1 / a0, h * b2 / a0, a1 / a0, a2 / a0]
}

/// Response in dB of `[b0, b1, b2, a1, a2]` at `f` Hz.
pub fn response_db(c: &[f64; 5], f: f64, fs: f64) -> f64 {
    let w = 2.0 * PI * f / fs;
    let (z1, z2) = ((w.cos(), -w.sin()), ((2.0 * w).cos(), -(2.0 * w).sin()));
    let num = (c[0] + c[1] * z1.0 + c[2] * z2.0, c[1] * z1.1 + c[2] * z2.1);
    let den = (1.0 + c[3] * z1.0 + c[4] * z2.0, c[3] * z1.1 + c[4] * z2.1);
    10.0 * ((num.0 * num.0 + num.1 * num.1) / (den.0 * den.0 + den.1 * den.1)).log10()
}

/// Quantise to the hardware's MAC form `[b0, b1, b2, -a1, -a2]`, Q2.23.
pub fn quantize(c: &[f64; 5]) -> [i64; 5] {
    let q = |v: f64| (v * (1u64 << COEF_FRAC) as f64).round() as i64;
    [q(c[0]), q(c[1]), q(c[2]), q(-c[3]), q(-c[4])]
}

/// The MAC-form coefficients back to `[b0, b1, b2, a1, a2]` as reals.
pub fn dequantize(q: &[i64; 5]) -> [f64; 5] {
    let f = |v: i64| v as f64 / (1u64 << COEF_FRAC) as f64;
    [f(q[0]), f(q[1]), f(q[2]), -f(q[3]), -f(q[4])]
}

/// The hardware's coefficients for a rate index and bass level.
pub fn bass_coefs(rate: usize, level: usize) -> [i64; 5] {
    if level == 0 {
        [1 << COEF_FRAC, 0, 0, 0, 0]
    } else {
        quantize(&low_shelf(RATES[rate], BASS_HZ, BASS_DB[level]))
    }
}

fn sat(v: i64, bits: u32) -> i64 {
    v.clamp(-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1)
}

/// One channel's biquad state, as in the RTL.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Biquad {
    pub x1: i64,
    pub x2: i64,
    pub y1: i64,
    pub y2: i64,
    pub e: i64,
}

impl Biquad {
    /// One sample of data-path input (26-bit) with MAC-form coefficients.
    pub fn step(&mut self, c: &[i64; 5], x: i64) -> i64 {
        let acc =
            self.e + c[0] * x + c[1] * self.x1 + c[2] * self.x2 + c[3] * self.y1 + c[4] * self.y2;
        let full = (acc + (1 << (COEF_FRAC - 1))) >> COEF_FRAC;
        self.e = acc - (full << COEF_FRAC);
        let y = sat(full, DATA_BITS);
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// The stereo bass boost: 16-bit samples in and out.
#[derive(Debug, Default, Clone)]
pub struct BassBoost {
    pub ch: [Biquad; 2],
}

impl BassBoost {
    /// One stereo sample at rate index `rate` and bass level `level`.
    pub fn step(&mut self, rate: usize, level: usize, l: i16, r: i16) -> (i16, i16) {
        let c = bass_coefs(rate, level);
        let one = |bq: &mut Biquad, s: i16| {
            let y = bq.step(&c, (s as i64) << DATA_FRAC);
            sat((y + (1 << (DATA_FRAC - 1))) >> DATA_FRAC, 16) as i16
        };
        (one(&mut self.ch[0], l), one(&mut self.ch[1], r))
    }
}

/// Test vectors for the RTL: lines `rate level in_l in_r out_l out_r`.
/// Every rate and level in turn (with the state carried across changes, as
/// on the board): a sweep on the left, noise on the right, bursts of a
/// full-scale 40 Hz square (to exercise saturation).
pub fn vectors(per_setting: usize) -> String {
    let mut eq = BassBoost::default();
    let mut out = String::new();
    let mut seed: u32 = 1;
    let mut n = 0usize;
    for rate in 0..3 {
        let fs = RATES[rate];
        for level in [0, 1, 2, 3, 2, 0, 3] {
            for i in 0..per_setting {
                let t = n as f64 / fs;
                // Exponential sweep 20 Hz -> 2 kHz over each setting, -6 dBFS.
                let p = i as f64 / per_setting as f64;
                let f = 20.0 * 100f64.powf(p);
                let l = if (i / 700) % 4 == 3 {
                    if ((t * 40.0) as i64) % 2 == 0 {
                        32767
                    } else {
                        -32768
                    }
                } else {
                    (16384.0 * (2.0 * PI * f * t).sin()).round() as i32
                };
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let r = ((seed >> 16) as i32 & 0x3fff) - 0x2000;
                let (l, r) = (l as i16, r as i16);
                let (ol, or) = eq.step(rate, level, l, r);
                out.push_str(&format!("{rate} {level} {l} {r} {ol} {or}\n"));
                n += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantised_response_matches_the_design() {
        for rate in 0..3 {
            for level in 1..4 {
                let c = low_shelf(RATES[rate], BASS_HZ, BASS_DB[level]);
                let q = dequantize(&bass_coefs(rate, level));
                for f in [20.0, 30.0, 60.0, 100.0, 200.0, 1000.0, 10000.0] {
                    let (a, b) = (
                        response_db(&c, f, RATES[rate]),
                        response_db(&q, f, RATES[rate]),
                    );
                    assert!(
                        (a - b).abs() < 0.05,
                        "rate {rate} level {level} {f} Hz: {a} vs {b}"
                    );
                }
                // Bass at 0 dB, treble down by the boost (automatic headroom).
                assert!(response_db(&c, 20.0, RATES[rate]).abs() < 0.3);
                let hi = response_db(&c, 5000.0, RATES[rate]);
                assert!((hi + BASS_DB[level]).abs() < 0.1, "{hi}");
                // Coefficients fit 25 bits.
                for v in bass_coefs(rate, level) {
                    assert!(v.abs() < 1 << (COEF_BITS - 1));
                }
            }
        }
    }

    #[test]
    fn level_zero_is_the_identity() {
        let mut eq = BassBoost::default();
        let mut s: u32 = 7;
        for _ in 0..5000 {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let (l, r) = ((s >> 16) as i16, (s >> 3) as i16);
            assert_eq!(eq.step(0, 0, l, r), (l, r));
        }
    }

    #[test]
    fn a_1khz_tone_is_cut_by_the_boost() {
        // Level 3 (+12 dB) at 44.1 kHz: 1 kHz comes out 12 dB down (x0.25).
        let mut eq = BassBoost::default();
        let mut peak = 0i32;
        for n in 0..44100 {
            let x = (16000.0 * (2.0 * PI * 1000.0 * n as f64 / 44100.0).sin()).round() as i16;
            let (l, _) = eq.step(0, 3, x, 0);
            if n > 22050 {
                peak = peak.max((l as i32).abs());
            }
        }
        let db = 20.0 * (peak as f64 / 16000.0).log10();
        assert!((db + 12.0).abs() < 0.2, "{db}");
    }
}
