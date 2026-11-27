//! DSP for the six audio effects. Every effect takes stereo samples and
//! 0..1 values ([`FxDesc::values`], spec order); the mapping to physical
//! units is ours:
//!
//! | Effect | Mapping |
//! |---|---|
//! | all | Dry Wet: linear mix of the input and the effect |
//! | Bitcrusher | Gain linear; Sample Rate 200 Hz…full rate (exponential); Bit Depth 1…24 bits; Dither = ±½ LSB triangular noise; Jitter = random hold lengths |
//! | Distortion | Pre / Post Gain ×10 (default 0.1 = unity); Drive = tanh(k·x)/tanh(k), k 1…41 (0 = clean); Tone = low-pass 200 Hz…20 kHz (1 = open) |
//! | EQ-3 | bands split at 250 Hz / 2.5 kHz (low + mid + high = input); 0 = band off, 0.5 = 0 dB, 1 = +12 dB |
//! | Flanger | Depth = amount of the swept delay; Rate 0…10 Hz; Feedback ×0.95; Invert flips the delayed signal; Width = sweep in seconds (≤ 50 ms) over a 1 ms base; right channel 90° behind |
//! | HighPass / LowPass | 12 dB/oct biquad; Frequency 20 Hz…20 kHz (exponential); Resonance Q 0.707…10 |
//!
//! Coefficients are computed when values change (once per UI frame at
//! most), never per sample.

use lavmfx_core::audiofx::{AudioFxKind, FxDesc, MAX_PARAMS};

/// Transposed direct form II biquad, two channels.
#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [[f32; 2]; 2],
}

impl Biquad {
    /// RBJ cookbook low / high pass.
    fn set(&mut self, high: bool, f: f32, q: f32, rate: f32) {
        let f = f.clamp(10.0, rate * 0.45);
        let w = std::f32::consts::TAU * f / rate;
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q.max(0.1));
        let a0 = 1.0 + alpha;
        let (b0, b1) = if high {
            ((1.0 + c) / 2.0, -(1.0 + c))
        } else {
            ((1.0 - c) / 2.0, 1.0 - c)
        };
        self.b = [b0 / a0, b1 / a0, b0 / a0];
        self.a = [-2.0 * c / a0, (1.0 - alpha) / a0];
    }

    fn run(&mut self, x: f32, ch: usize) -> f32 {
        let z = &mut self.z[ch];
        let y = self.b[0] * x + z[0];
        z[0] = self.b[1] * x - self.a[0] * y + z[1];
        z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// One-pole low-pass, two channels.
#[derive(Clone, Copy, Default)]
struct OnePole {
    k: f32,
    z: [f32; 2],
}

impl OnePole {
    fn set(&mut self, f: f32, rate: f32) {
        self.k = 1.0 - (-std::f32::consts::TAU * f.min(rate * 0.45) / rate).exp();
    }
    fn run(&mut self, x: f32, ch: usize) -> f32 {
        self.z[ch] += (x - self.z[ch]) * self.k;
        self.z[ch]
    }
}

fn exp_map(v: f32, lo: f32, hi: f32) -> f32 {
    lo * (hi / lo).powf(v.clamp(0.0, 1.0))
}

/// Small fast PRNG (audio-thread safe).
#[derive(Clone, Copy)]
struct Rng(u32);

impl Rng {
    /// −1..1.
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

enum Dsp {
    Bitcrusher {
        hold: [f32; 2],
        phase: f32,
        step: f32,
        levels: f32,
        rng: Rng,
    },
    Distortion {
        pre: f32,
        k: f32,
        norm: f32,
        tone: OnePole,
        open: bool,
        post: f32,
    },
    Eq3 {
        /// Linkwitz-Riley (2 × Butterworth) crossovers at 250 Hz and 2.5 kHz:
        /// [lp250 ×2, hp250 ×2, lp2500 ×2, hp2500 ×2, low-band all-pass (lp2500 ×2, hp2500 ×2)].
        f: Box<[Biquad; 12]>,
        g: [f32; 3],
    },
    Flanger {
        buf: Vec<[f32; 2]>,
        w: usize,
        phase: f32,
        inc: f32,
        base: f32,
        sweep: f32,
        fb: f32,
        depth: f32,
        sign: f32,
    },
    Filter(Biquad),
}

struct Unit {
    key: u64,
    kind: AudioFxKind,
    bypassed: bool,
    v: [f32; MAX_PARAMS],
    dsp: Dsp,
    rate: f32,
}

impl Unit {
    fn new(d: &FxDesc, rate: f32) -> Self {
        let dsp = match d.kind {
            AudioFxKind::Bitcrusher => Dsp::Bitcrusher {
                hold: [0.0; 2],
                phase: 1.0,
                step: 1.0,
                levels: 1.0,
                rng: Rng(0x9e37_79b9 ^ d.key as u32 | 1),
            },
            AudioFxKind::Distortion => Dsp::Distortion {
                pre: 1.0,
                k: 1.0,
                norm: 1.0,
                tone: OnePole::default(),
                open: true,
                post: 1.0,
            },
            AudioFxKind::Eq3 => Dsp::Eq3 {
                f: Box::new([Biquad::default(); 12]),
                g: [1.0; 3],
            },
            AudioFxKind::Flanger => Dsp::Flanger {
                buf: vec![[0.0; 2]; (rate * 0.06) as usize + 4],
                w: 0,
                phase: 0.0,
                inc: 0.0,
                base: 0.0,
                sweep: 0.0,
                fb: 0.0,
                depth: 0.0,
                sign: 1.0,
            },
            AudioFxKind::HighPass | AudioFxKind::LowPass => Dsp::Filter(Biquad::default()),
        };
        let mut u = Self {
            key: d.key,
            kind: d.kind,
            bypassed: d.bypassed,
            v: [f32::NAN; MAX_PARAMS],
            dsp,
            rate,
        };
        u.set(d, rate);
        u
    }

    /// New values → coefficients (only when something changed).
    fn set(&mut self, d: &FxDesc, rate: f32) {
        self.bypassed = d.bypassed;
        if self.v == d.values {
            return;
        }
        self.v = d.values;
        let v = d.values;
        match &mut self.dsp {
            Dsp::Bitcrusher { step, levels, .. } => {
                *step = exp_map(v[2], 200.0, rate) / rate;
                let bits = 1.0 + v[3].clamp(0.0, 1.0) * 23.0;
                *levels = 2f32.powf(bits - 1.0);
            }
            Dsp::Distortion {
                pre,
                k,
                norm,
                tone,
                open,
                post,
            } => {
                *pre = v[1] * 10.0;
                *k = 1.0 + v[2] * 40.0;
                *norm = 1.0 / k.tanh();
                *open = v[3] >= 0.999;
                tone.set(exp_map(v[3], 200.0, 20_000.0), rate);
                *post = v[4] * 10.0;
            }
            Dsp::Eq3 { f, g } => {
                let q = std::f32::consts::FRAC_1_SQRT_2;
                for (i, b) in f.iter_mut().enumerate() {
                    let (high, freq) = match i {
                        0 | 1 => (false, 250.0),
                        2 | 3 => (true, 250.0),
                        4 | 5 | 8 | 9 => (false, 2500.0),
                        _ => (true, 2500.0),
                    };
                    b.set(high, freq, q, rate);
                }
                for (g, x) in g.iter_mut().zip(&v[1..4]) {
                    *g = eq_gain(*x);
                }
            }
            Dsp::Flanger {
                inc,
                base,
                sweep,
                fb,
                depth,
                sign,
                ..
            } => {
                *depth = v[1];
                *inc = std::f32::consts::TAU * v[2] * 10.0 / rate;
                *fb = v[3] * 0.95;
                *sign = if v[4] > 0.5 { -1.0 } else { 1.0 };
                *base = 0.001 * rate;
                *sweep = v[5].clamp(0.0, 0.05) * rate;
            }
            Dsp::Filter(b) => {
                let high = self.kind == AudioFxKind::HighPass;
                b.set(high, exp_map(v[1], 20.0, 20_000.0), 0.707 * 14f32.powf(v[2]), rate);
            }
        }
    }

    fn run(&mut self, x: [f32; 2]) -> [f32; 2] {
        let mut y = x;
        match &mut self.dsp {
            Dsp::Bitcrusher {
                hold,
                phase,
                step,
                levels,
                rng,
            } => {
                let dither = self.v[4] > 0.5;
                let jitter = self.v[5] > 0.5;
                *phase += *step;
                if *phase >= 1.0 {
                    *phase -= 1.0;
                    if jitter {
                        *phase = (*phase + rng.next() * 0.5 * (1.0 - *step).max(0.0)).clamp(0.0, 0.999);
                    }
                    for (h, s) in hold.iter_mut().zip(x) {
                        let n = if dither { (rng.next() + rng.next()) * 0.25 } else { 0.0 };
                        *h = ((s * *levels + n).round() / *levels).clamp(-1.0, 1.0);
                    }
                }
                y = hold.map(|h| h * self.v[1]);
            }
            Dsp::Distortion {
                pre,
                k,
                norm,
                tone,
                open,
                post,
            } => {
                for (ch, s) in y.iter_mut().enumerate() {
                    let mut t = *s * *pre;
                    if *k > 1.0001 {
                        t = (t * *k).tanh() * *norm;
                    }
                    if !*open {
                        t = tone.run(t, ch);
                    }
                    *s = t * *post;
                }
            }
            Dsp::Eq3 { f, g } => {
                for (ch, s) in y.iter_mut().enumerate() {
                    let x = *s;
                    let mut two = |i: usize, v: f32| {
                        let t = f[i].run(v, ch);
                        f[i + 1].run(t, ch)
                    };
                    let low = two(0, x);
                    let rest = two(2, x);
                    let mid = two(4, rest);
                    let high = two(6, rest);
                    // Same phase as mid + high (LR4 low + high = all-pass).
                    let low = two(8, low) + two(10, low);
                    *s = low * g[0] + mid * g[1] + high * g[2];
                }
            }
            Dsp::Flanger {
                buf,
                w,
                phase,
                inc,
                base,
                sweep,
                fb,
                depth,
                sign,
            } => {
                let n = buf.len();
                *phase = (*phase + *inc) % std::f32::consts::TAU;
                let mut d = [0.0f32; 2];
                for (ch, o) in d.iter_mut().enumerate() {
                    let lfo = 0.5 + 0.5 * (*phase - ch as f32 * std::f32::consts::FRAC_PI_2).sin();
                    let delay = (*base + *sweep * lfo).clamp(1.0, n as f32 - 2.0);
                    let r = *w as f32 - delay + n as f32;
                    let i = r.floor() as usize;
                    let f = r - r.floor();
                    let a = buf[i % n][ch];
                    let b = buf[(i + 1) % n][ch];
                    *o = (a + (b - a) * f) * *sign;
                }
                buf[*w] = [x[0] + d[0] * *fb, x[1] + d[1] * *fb];
                *w = (*w + 1) % n;
                y = [x[0] + d[0] * *depth, x[1] + d[1] * *depth];
            }
            Dsp::Filter(b) => {
                for (ch, s) in y.iter_mut().enumerate() {
                    *s = b.run(*s, ch);
                }
            }
        }
        let wet = self.v[0].clamp(0.0, 1.0);
        let out = [x[0] * (1.0 - wet) + y[0] * wet, x[1] * (1.0 - wet) + y[1] * wet];
        // Unstable settings never blow up the output.
        if out[0].is_finite() && out[1].is_finite() {
            out
        } else {
            self.reset();
            x
        }
    }

    fn reset(&mut self) {
        let d = FxDesc {
            key: self.key,
            kind: self.kind,
            bypassed: self.bypassed,
            values: self.v,
        };
        *self = Unit::new(&d, self.rate);
    }
}

/// 0 = off, 0.5 = 0 dB, 1 = +12 dB.
fn eq_gain(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.5 {
        (v * 2.0).powi(2)
    } else {
        10f32.powf((v - 0.5) * 2.0 * 12.0 / 20.0)
    }
}

/// An effect chain with its state, kept across updates by effect key.
#[derive(Default)]
pub struct Chain {
    units: Vec<Unit>,
    rate: f32,
}

impl Chain {
    pub fn new(rate: u32) -> Self {
        Self {
            units: Vec::new(),
            rate: rate as f32,
        }
    }

    /// Follow the UI's chain: same keys keep their state (delay lines, filter
    /// memory), new ones start fresh, removed ones go.
    pub fn update(&mut self, desc: &[FxDesc]) {
        let same = self.units.len() == desc.len()
            && self
                .units
                .iter()
                .zip(desc)
                .all(|(u, d)| u.key == d.key && u.kind == d.kind);
        if same {
            for (u, d) in self.units.iter_mut().zip(desc) {
                u.set(d, self.rate);
            }
            return;
        }
        let mut old = std::mem::take(&mut self.units);
        for d in desc {
            let u = match old.iter().position(|u| u.key == d.key && u.kind == d.kind) {
                Some(i) => {
                    let mut u = old.swap_remove(i);
                    u.set(d, self.rate);
                    u
                }
                None => Unit::new(d, self.rate),
            };
            self.units.push(u);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.units.iter().all(|u| u.bypassed)
    }

    pub fn run(&mut self, mut x: [f32; 2]) -> [f32; 2] {
        for u in self.units.iter_mut().filter(|u| !u.bypassed) {
            x = u.run(x);
        }
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lavmfx_core::audiofx::{AUDIO_EFFECTS, PDef};

    const RATE: u32 = 48_000;

    fn desc(kind: AudioFxKind, set: &[(usize, f32)]) -> FxDesc {
        let spec = AUDIO_EFFECTS.iter().find(|s| s.kind == kind).unwrap();
        let mut values = [0.0; MAX_PARAMS];
        for (v, p) in values.iter_mut().zip(spec.params) {
            *v = match *p {
                PDef::Range(_, d) => d as f32,
                PDef::Bool(_, d) => d as u8 as f32,
            };
        }
        for &(i, x) in set {
            values[i] = x;
        }
        FxDesc {
            key: 1,
            kind,
            bypassed: false,
            values,
        }
    }

    fn sine(f: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (std::f32::consts::TAU * f * i as f32 / RATE as f32).sin() * 0.5)
            .collect()
    }

    /// RMS of the last half after running `x` through the chain.
    fn rms(c: &mut Chain, x: &[f32]) -> f32 {
        let out: Vec<f32> = x.iter().map(|&s| c.run([s, s])[0]).collect();
        let tail = &out[out.len() / 2..];
        (tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt()
    }

    fn chain(d: FxDesc) -> Chain {
        let mut c = Chain::new(RATE);
        c.update(&[d]);
        c
    }

    #[test]
    fn defaults_are_mostly_transparent() {
        let x = sine(440.0, 9600);
        let dry = rms(&mut Chain::new(RATE), &x);
        for kind in [
            AudioFxKind::Distortion,
            AudioFxKind::Eq3,
            AudioFxKind::HighPass,
            AudioFxKind::LowPass,
            AudioFxKind::Flanger,
            AudioFxKind::Bitcrusher,
        ] {
            let r = rms(&mut chain(desc(kind, &[])), &x);
            assert!((r - dry).abs() / dry < 0.03, "{kind:?}: {r} vs {dry}");
        }
    }

    #[test]
    fn filters_cut_what_they_should() {
        let low = sine(100.0, 19200);
        let high = sine(8000.0, 19200);
        let dry = rms(&mut Chain::new(RATE), &low);
        let lp = || chain(desc(AudioFxKind::LowPass, &[(1, 0.3)])); // ~160 Hz
        assert!(rms(&mut lp(), &low) > dry * 0.7);
        assert!(rms(&mut lp(), &high) < dry * 0.01);
        let hp = || chain(desc(AudioFxKind::HighPass, &[(1, 0.6)])); // ~1.3 kHz
        assert!(rms(&mut hp(), &low) < dry * 0.02);
        assert!(rms(&mut hp(), &high) > dry * 0.9);
        // Dry Wet 0 = dry.
        assert!((rms(&mut chain(desc(AudioFxKind::LowPass, &[(0, 0.0), (1, 0.3)])), &high) - dry).abs() < dry * 0.01);
    }

    #[test]
    fn eq_kills_and_boosts_bands() {
        let low = sine(60.0, 19200);
        let dry = rms(&mut Chain::new(RATE), &low);
        assert!(rms(&mut chain(desc(AudioFxKind::Eq3, &[(1, 0.0)])), &low) < dry * 0.1);
        let boost = rms(&mut chain(desc(AudioFxKind::Eq3, &[(1, 1.0)])), &low);
        assert!(boost > dry * 3.0, "{boost} {dry}");
    }

    #[test]
    fn distortion_bitcrusher_flanger_change_the_sound() {
        let x = sine(440.0, 9600);
        // Drive squares the sine: more RMS at the same peak.
        let mut d = chain(desc(AudioFxKind::Distortion, &[(2, 1.0)]));
        let out: Vec<f32> = x.iter().map(|&s| d.run([s, s])[0]).collect();
        let peak = out.iter().fold(0f32, |m, s| m.max(s.abs()));
        let r = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        assert!(r / peak > 0.85, "crest {}", peak / r);
        // 2-bit crush: only a few distinct levels.
        let mut b = chain(desc(AudioFxKind::Bitcrusher, &[(3, 1.0 / 23.0)]));
        let mut levels: Vec<i32> = x.iter().map(|&s| (b.run([s, s])[0] * 1000.0) as i32).collect();
        levels.sort();
        levels.dedup();
        assert!(levels.len() <= 4, "{levels:?}");
        // Sample-rate reduction holds samples.
        let mut b = chain(desc(AudioFxKind::Bitcrusher, &[(2, 0.0)]));
        let out: Vec<f32> = x.iter().map(|&s| b.run([s, s])[0]).collect();
        let repeats = out.windows(2).filter(|w| w[0] == w[1]).count();
        assert!(repeats > out.len() * 9 / 10);
        // Flanger with depth: comb filtering changes the level of a tone; stays bounded.
        let mut f = chain(desc(AudioFxKind::Flanger, &[(1, 1.0), (5, 0.005), (3, 1.0)]));
        let out: Vec<f32> = sine(3000.0, 48000).iter().map(|&s| f.run([s, s])[0]).collect();
        assert!(out.iter().all(|s| s.abs() < 20.0));
        assert!(out.iter().any(|s| s.abs() > 0.6));
    }

    #[test]
    fn chain_keeps_state_by_key_and_skips_bypassed() {
        let mut c = Chain::new(RATE);
        let mut d = desc(AudioFxKind::LowPass, &[(1, 0.0)]);
        c.update(std::slice::from_ref(&d));
        assert!(!c.is_empty());
        d.bypassed = true;
        c.update(std::slice::from_ref(&d));
        assert!(c.is_empty());
        assert_eq!(c.run([0.5, -0.5]), [0.5, -0.5]);
        c.update(&[]);
        assert!(c.units.is_empty());
    }
}
