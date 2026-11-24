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

/// The stage-1 test sequence, through `eq` (EQ off: the bass boost alone):
/// every rate and level in turn, the state carried across changes as on the
/// board; a sweep on the left, noise on the right, bursts of a full-scale
/// 40 Hz square (saturation). Lines `S rate level in_l in_r out_l out_r`.
fn bass_sequence(eq: &mut Equalizer, per_setting: usize, out: &mut String) {
    let mut seed: u32 = 1;
    let mut n = 0usize;
    eq.on = false;
    out.push_str("C 0");
    for v in eq.set.iter().flatten() {
        out.push_str(&format!(" {v}"));
    }
    out.push('\n');
    for (rate, fs) in RATES.iter().enumerate() {
        for level in [0, 1, 2, 3, 2, 0, 3] {
            for i in 0..per_setting {
                let t = n as f64 / fs;
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
                out.push_str(&format!("S {rate} {level} {l} {r} {ol} {or}\n"));
                if let Some(snap) = eq.snapshot_line() {
                    out.push_str(&snap);
                }
                n += 1;
            }
        }
    }
}

// ---------------------------------------------------------------- stage 2

/// Graphic equaliser band centres (octaves).
pub const BANDS: [f64; 10] = [
    31.25, 62.5, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];
/// One-octave bandwidth.
pub const BAND_Q: f64 = std::f64::consts::SQRT_2;
/// Band gain limit, dB.
pub const BAND_DB: f64 = 12.0;
/// Bytes of a coefficient set on the wire: 10 bands x 5 terms x 4 bytes LE.
pub const SET_BYTES: usize = 10 * 5 * 4;

/// Normalised peaking filter `[b0, b1, b2, a1, a2]` (RBJ cookbook).
pub fn peaking(fs: f64, f0: f64, q: f64, gain_db: f64) -> [f64; 5] {
    let a = 10f64.powf(gain_db / 40.0);
    let w = 2.0 * PI * f0 / fs;
    let (c, al) = (w.cos(), w.sin() / (2.0 * q));
    let a0 = 1.0 + al / a;
    [
        (1.0 + al * a) / a0,
        -2.0 * c / a0,
        (1.0 - al * a) / a0,
        -2.0 * c / a0,
        (1.0 - al / a) / a0,
    ]
}

const IDENTITY: [f64; 5] = [1.0, 0.0, 0.0, 0.0, 0.0];

/// The 10 bands' filters for `gains` (dB, clamped to +-[`BAND_DB`]) at rate
/// index `rate`, with automatic headroom: if the combined response rises
/// above 0 dB anywhere (20 Hz to Nyquist), band 0's b's are scaled down by
/// that peak. A band at 0 dB, or above 0.45 fs, is the identity.
pub fn eq_design(rate: usize, gains: &[f64; 10]) -> [[f64; 5]; 10] {
    let fs = RATES[rate];
    let mut c = [IDENTITY; 10];
    for (i, g) in gains.iter().enumerate() {
        let g = g.clamp(-BAND_DB, BAND_DB);
        if g != 0.0 && BANDS[i] < 0.45 * fs {
            c[i] = peaking(fs, BANDS[i], BAND_Q, g);
        }
    }
    let mut peak = f64::MIN;
    let mut f = 20.0;
    while f < fs / 2.0 {
        let db: f64 = c.iter().map(|b| response_db(b, f, fs)).sum();
        peak = peak.max(db);
        f *= 1.01;
    }
    if peak > 0.0 {
        let h = 10f64.powf(-peak / 20.0);
        for v in &mut c[0][..3] {
            *v *= h;
        }
    }
    c
}

/// Quantised coefficient set for the hardware (MAC form, Q2.23).
pub fn eq_coefs(rate: usize, gains: &[f64; 10]) -> [[i64; 5]; 10] {
    eq_design(rate, gains).map(|c| quantize(&c))
}

/// A coefficient set as the USB record carries it (record type 1).
pub fn set_bytes(set: &[[i64; 5]; 10]) -> Vec<u8> {
    set.iter()
        .flatten()
        .flat_map(|v| (*v as i32).to_le_bytes())
        .collect()
}

/// The full tone control as the RC10 runs it: per channel the bass shelf
/// (section 0, from the built-in table), then, while the EQ is on, the 10
/// bands (sections 1-10) with host-loaded coefficients. Off, it is exactly
/// [`BassBoost`].
#[derive(Debug, Clone)]
pub struct Equalizer {
    pub ch: [[Biquad; 11]; 2],
    pub on: bool,
    pub set: [[i64; 5]; 10],
    /// Stage 3: the analyser's band-pass sections and peak envelopes.
    pub an: [Biquad; 10],
    pub env: [i64; 10],
    /// The last snapshot of the envelopes (16-bit, sample units) and how
    /// many snapshots there have been (wrapping), as the status carries them.
    pub levels: [u16; 10],
    pub snaps: u8,
    samples: u64,
}

impl Default for Equalizer {
    fn default() -> Self {
        Equalizer {
            ch: [[Biquad::default(); 11]; 2],
            on: false,
            set: [[1 << COEF_FRAC, 0, 0, 0, 0]; 10],
            an: [Biquad::default(); 10],
            env: [0; 10],
            levels: [0; 10],
            snaps: 0,
            samples: 0,
        }
    }
}

/// Peak envelope release: env -= env >> RELEASE each sample (tau = 2^13
/// samples, about 190 ms at 44.1 kHz).
pub const RELEASE: u32 = 13;
/// Samples between level snapshots.
pub const SNAP: u64 = 256;

/// Normalised band-pass `[b0, b1, b2, a1, a2]`, 0 dB at `f0` (RBJ cookbook).
pub fn bandpass(fs: f64, f0: f64, q: f64) -> [f64; 5] {
    let w = 2.0 * PI * f0 / fs;
    let al = w.sin() / (2.0 * q);
    let a0 = 1.0 + al;
    [al / a0, 0.0, -al / a0, -2.0 * w.cos() / a0, (1.0 - al) / a0]
}

/// The analyser's Q: a little narrower than an octave, so a tone reads 10 dB
/// lower in the neighbouring bands (at Q = sqrt 2 only 7.4 dB) and still
/// within 4.8 dB at a band edge.
pub const ANALYSER_Q: f64 = 2.0;

/// The analyser's coefficients for a band (MAC form, Q2.23); all zero for a
/// band above 0.45 fs.
pub fn bpf_coefs(rate: usize, band: usize) -> [i64; 5] {
    if BANDS[band] < 0.45 * RATES[rate] {
        quantize(&bandpass(RATES[rate], BANDS[band], ANALYSER_Q))
    } else {
        [0; 5]
    }
}

impl Equalizer {
    pub fn step(&mut self, rate: usize, level: usize, l: i16, r: i16) -> (i16, i16) {
        let bass = bass_coefs(rate, level);
        let (on, set) = (self.on, self.set);
        let one = |st: &mut [Biquad; 11], s: i16| {
            let mut x = (s as i64) << DATA_FRAC;
            x = st[0].step(&bass, x);
            if on {
                for (bq, c) in st[1..].iter_mut().zip(set.iter()) {
                    x = bq.step(c, x);
                }
            }
            sat((x + (1 << (DATA_FRAC - 1))) >> DATA_FRAC, 16) as i16
        };
        let [a, b] = &mut self.ch;
        let (ol, or) = (one(a, l), one(b, r));
        // The analyser: each band of the mono sum, then its peak envelope.
        let m = ((ol as i64 + or as i64) >> 1) << DATA_FRAC;
        for (i, bq) in self.an.iter_mut().enumerate() {
            let y = bq.step(&bpf_coefs(rate, i), m).abs();
            self.env[i] = y.max(self.env[i] - (self.env[i] >> RELEASE));
        }
        self.samples += 1;
        if self.samples.is_multiple_of(SNAP) {
            self.levels = self.env.map(|e| (e >> DATA_FRAC).min(0xffff) as u16);
            self.snaps = self.snaps.wrapping_add(1);
        }
        (ol, or)
    }

    /// Vector line for a snapshot just taken, if one was.
    fn snapshot_line(&self) -> Option<String> {
        (self.samples.is_multiple_of(SNAP)).then(|| {
            let mut s = format!("L {}", self.snaps);
            for v in self.levels {
                s.push_str(&format!(" {v}"));
            }
            s.push('\n');
            s
        })
    }
}

/// Test vectors for the RTL (`tb_eq`). Lines are either
/// `S rate level in_l in_r out_l out_r` (a sample),
/// `L snaps l0 .. l9` (the analyser's snapshot just taken, after that sample) or
/// `C on c0 .. c49` (from the next sample: EQ on/off and, if on, a new set):
/// first the bass boost alone at every rate and level, with saturating
/// bursts, then EQ curves changing while a 20 Hz-20 kHz sweep and noise play.
pub fn eq_vectors(per_setting: usize) -> String {
    let mut eq = Equalizer::default();
    let mut out = String::new();
    bass_sequence(&mut eq, per_setting, &mut out);
    let mut seed: u32 = 3;
    let mut rnd = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        seed
    };
    let curves: [[f64; 10]; 4] = [
        [12.0, 9.0, 6.0, 3.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [-12.0, -6.0, 0.0, 6.0, 12.0, 12.0, 6.0, 0.0, -6.0, -12.0],
        [0.0, 0.0, 0.0, -3.0, -6.0, -6.0, -3.0, 3.0, 9.0, 12.0],
        [12.0; 10],
    ];
    let mut n = 0usize;
    for rate in 0..3 {
        let fs = RATES[rate];
        for (step, curve) in curves.iter().enumerate() {
            let on = step != 2 || rate != 1; // one stretch with the EQ off
            eq.on = on;
            if on {
                eq.set = eq_coefs(rate, curve);
            }
            out.push_str(&format!("C {}", on as u8));
            for v in eq.set.iter().flatten() {
                out.push_str(&format!(" {v}"));
            }
            out.push('\n');
            let level = step % 4;
            for i in 0..per_setting {
                let t = n as f64 / fs;
                let p = i as f64 / per_setting as f64;
                let f = 20.0 * 1000f64.powf(p); // 20 Hz .. 20 kHz
                let l = (16384.0 * (2.0 * PI * f * t).sin()).round() as i32;
                let r = ((rnd() >> 16) as i32 & 0x7fff) - 0x4000;
                let (l, r) = (l as i16, r as i16);
                let (ol, or) = eq.step(rate, level, l, r);
                out.push_str(&format!("S {rate} {level} {l} {r} {ol} {or}\n"));
                if let Some(snap) = eq.snapshot_line() {
                    out.push_str(&snap);
                }
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

    #[test]
    fn eq_bands_match_the_design_and_never_clip() {
        let curves = [
            [12.0; 10],
            [-12.0; 10],
            [
                12.0, -12.0, 12.0, -12.0, 12.0, -12.0, 12.0, -12.0, 12.0, -12.0,
            ],
        ];
        for rate in 0..3 {
            let fs = RATES[rate];
            for g in curves {
                let d = eq_design(rate, &g);
                let q = eq_coefs(rate, &g).map(|c| dequantize(&c));
                let mut f = 20.0;
                while f < fs / 2.0 {
                    let a: f64 = d.iter().map(|c| response_db(c, f, fs)).sum();
                    let b: f64 = q.iter().map(|c| response_db(c, f, fs)).sum();
                    assert!((a - b).abs() < 0.15, "rate {rate} {f} Hz: {a} vs {b}");
                    assert!(a < 0.01, "rate {rate} {f} Hz: {a} dB (headroom)");
                    f *= 1.1;
                }
            }
        }
        // One band up 12 dB: its centre is the 0 dB peak, 1 kHz is 12 dB lower.
        let mut g = [0.0; 10];
        g[0] = 12.0;
        let d = eq_design(0, &g);
        let at = |f: f64| d.iter().map(|c| response_db(c, f, 44100.0)).sum::<f64>();
        assert!(
            at(31.25).abs() < 0.05 && (at(1000.0) + 12.0).abs() < 0.3,
            "{} {}",
            at(31.25),
            at(1000.0)
        );
    }

    #[test]
    fn eq_off_is_the_bass_boost() {
        let (mut a, mut b) = (Equalizer::default(), BassBoost::default());
        for n in 0..3000 {
            let x = ((n * 7919) % 65536) as i16;
            assert_eq!(a.step(1, 2, x, -x), b.step(1, 2, x, -x));
        }
        assert_eq!(set_bytes(&eq_coefs(0, &[0.0; 10])).len(), SET_BYTES);
    }

    #[test]
    fn the_analyser_sees_a_tone_in_its_band() {
        // A 1 kHz tone at -6 dBFS lights band 5 (1 kHz) near -6 dB and the
        // others well below.
        let mut eq = Equalizer::default();
        for n in 0..44100 {
            let x = (16384.0 * (2.0 * PI * 1000.0 * n as f64 / 44100.0).sin()).round() as i16;
            eq.step(0, 0, x, x);
        }
        let db = |v: u16| 20.0 * (v.max(1) as f64 / 32768.0).log10();
        assert!((db(eq.levels[5]) + 6.0).abs() < 0.5, "{}", db(eq.levels[5]));
        for (i, v) in eq.levels.iter().enumerate() {
            if i != 5 {
                assert!(db(*v) < db(eq.levels[5]) - 9.0, "band {i}: {}", db(*v));
            }
        }
        // The 16 kHz band is silent at 32 kHz (above Nyquist).
        assert_eq!(bpf_coefs(2, 9), [0; 5]);
    }
}
