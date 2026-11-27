//! Audio spectrum as the drive sees it: `BANDS` log-spaced bands from
//! `F_MIN` to `F_MAX`, each 0..1 (−60 dB … 0 dB). The analyser lives in
//! `lavmfx-audio`; this is the shared shape plus band math for FFT drives.

use serde::{Deserialize, Serialize};

pub const BANDS: usize = 32;
pub const F_MIN: f64 = 20.0;
pub const F_MAX: f64 = 20_000.0;
/// Level floor in dB (maps to 0).
pub const DB_FLOOR: f64 = -60.0;

pub type Spectrum = [f32; BANDS];

/// The global FFT threshold (Preferences › Audio, the toolbar / External FFT
/// Thresh knob): every band below `t` (0..1 of the meter) is silence, the
/// rest stretched back to 0..1. Applied to all FFT sources before each
/// drive's own threshold.
pub fn gate_spectrum(s: &Spectrum, t: f64) -> Spectrum {
    let t = t.clamp(0.0, 0.99) as f32;
    if t <= 0.0 {
        return *s;
    }
    s.map(|v| ((v - t) / (1.0 - t)).max(0.0))
}
pub const SILENT: Spectrum = [0.0; BANDS];

/// Frequency at position `x` (0..1) on the log axis.
pub fn freq_at(x: f64) -> f64 {
    F_MIN * (F_MAX / F_MIN).powf(x.clamp(0.0, 1.0))
}

/// Position (0..1) of frequency `f` on the log axis.
pub fn pos_of(f: f64) -> f64 {
    ((f.max(1e-9) / F_MIN).ln() / (F_MAX / F_MIN).ln()).clamp(0.0, 1.0)
}

/// Lower and upper frequency of band `i`.
pub fn band_edges(i: usize) -> (f64, f64) {
    let n = BANDS as f64;
    (freq_at(i as f64 / n), freq_at((i + 1) as f64 / n))
}

/// Linear amplitude (1 = full scale) → 0..1.
pub fn level_of_amplitude(a: f64) -> f32 {
    if a <= 0.0 {
        return 0.0;
    }
    ((20.0 * a.log10() - DB_FLOOR) / -DB_FLOOR).clamp(0.0, 1.0) as f32
}

/// The L / M / H buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Low,
    Mid,
    High,
}

impl Preset {
    pub const ALL: [(Preset, &'static str, &'static str); 3] = [
        (Preset::Low, "L", "Low (20–240 Hz)"),
        (Preset::Mid, "M", "Mid (240 Hz–3.8 kHz)"),
        (Preset::High, "H", "High (3.8–20 kHz)"),
    ];
    pub fn range(self) -> (f64, f64) {
        match self {
            Preset::Low => (0.0, 0.36),
            Preset::Mid => (0.36, 0.76),
            Preset::High => (0.76, 1.0),
        }
    }
}

fn default_hi() -> f64 {
    Preset::Low.range().1
}
fn default_gain() -> f64 {
    1.0
}
fn default_fall() -> f64 {
    0.3
}

/// Settings of an FFT drive: band range on the log axis, gain, fall.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FftBand {
    /// Band start, 0..1 on the log frequency axis.
    #[serde(default)]
    pub lo: f64,
    #[serde(default = "default_hi")]
    pub hi: f64,
    /// Multiplier on the band level (0..4).
    #[serde(default = "default_gain")]
    pub gain: f64,
    /// How slowly the output falls after a peak: 0 = at once, 1 = 12 s over
    /// the full range (quadratic, so the low half of the knob stays snappy).
    #[serde(default = "default_fall")]
    pub fall: f64,
    /// Levels below this (0..1 of the meter) count as silence (ambient noise);
    /// what's above is stretched back to 0..1.
    #[serde(default)]
    pub threshold: f64,
}

impl Default for FftBand {
    fn default() -> Self {
        Self {
            lo: 0.0,
            hi: default_hi(),
            gain: default_gain(),
            fall: default_fall(),
            threshold: 0.0,
        }
    }
}

pub const GAIN_MAX: f64 = 4.0;
/// Fall time over the full range at Fall = 1.
pub const FALL_MAX_S: f64 = 12.0;

impl FftBand {
    pub fn set_preset(&mut self, p: Preset) {
        (self.lo, self.hi) = p.range();
    }

    pub fn preset(&self) -> Option<Preset> {
        Preset::ALL
            .iter()
            .map(|(p, _, _)| *p)
            .find(|p| p.range() == (self.lo, self.hi))
    }

    /// Bands covered by the range (at least one).
    pub fn bands(&self) -> std::ops::Range<usize> {
        let (lo, hi) = (self.lo.min(self.hi), self.lo.max(self.hi));
        let a = ((lo * BANDS as f64).floor() as usize).min(BANDS - 1);
        let b = ((hi * BANDS as f64).ceil() as usize).clamp(a + 1, BANDS);
        a..b
    }

    /// A meter value after the threshold: 0 below it, the rest stretched to 0..1.
    pub fn gate(&self, v: f64) -> f64 {
        let t = self.threshold.clamp(0.0, 0.99);
        ((v - t) / (1.0 - t)).max(0.0)
    }

    /// Loudest band in the range, through the threshold, × gain, 0..1 (before fall).
    pub fn level(&self, s: &Spectrum) -> f64 {
        let peak = s[self.bands()].iter().copied().fold(0.0f32, f32::max) as f64;
        (self.gate(peak) * self.gain).clamp(0.0, 1.0)
    }

    /// Seconds to fall over the full range (0 = instant).
    pub fn fall_time(&self) -> f64 {
        FALL_MAX_S * self.fall.clamp(0.0, 1.0).powi(2)
    }

    /// Follow `target` from `prev`: rise at once, fall at the fall rate.
    pub fn follow(&self, prev: f64, target: f64, dt: f64) -> f64 {
        if target >= prev {
            return target;
        }
        let t = self.fall_time();
        if t <= 1e-6 { target } else { (prev - dt / t).max(target) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_and_presets() {
        assert!((freq_at(0.0) - 20.0).abs() < 1e-9);
        assert!((freq_at(1.0) - 20_000.0).abs() < 1e-6);
        assert!((pos_of(freq_at(0.4)) - 0.4).abs() < 1e-12);
        let mut b = FftBand::default();
        assert_eq!(b.preset(), Some(Preset::Low));
        assert!(freq_at(b.hi) > 200.0 && freq_at(b.hi) < 260.0);
        b.set_preset(Preset::High);
        assert_eq!(b.bands().end, BANDS);
        b.lo = 0.5;
        b.hi = 0.5;
        assert_eq!(b.bands().len(), 1, "empty range still reads one band");
    }

    #[test]
    fn level_gain_and_fall() {
        let mut s = SILENT;
        s[2] = 0.5;
        s[30] = 0.9;
        let mut b = FftBand::default();
        assert_eq!(b.level(&s), 0.5, "low band peak");
        b.gain = 4.0;
        assert_eq!(b.level(&s), 1.0, "clamped");
        b.fall = 0.0;
        assert_eq!(b.follow(1.0, 0.0, 0.01), 0.0, "instant fall");
        b.fall = 0.5; // 3 s over the full range
        assert!((b.follow(1.0, 0.0, 0.3) - 0.9).abs() < 1e-9);
        b.fall = 1.0;
        assert!((b.fall_time() - FALL_MAX_S).abs() < 1e-9);
        // Threshold: noise below it reads 0, a full peak still reaches 1.
        b.gain = 1.0;
        b.threshold = 0.5;
        assert_eq!(b.level(&s), 0.0, "0.5 at threshold 0.5");
        let g = gate_spectrum(&s, 0.5);
        assert_eq!((g[0], g[BANDS - 1]), (0.0, 0.0));
        assert_eq!(gate_spectrum(&s, 0.0), s);
        s[2] = 0.75;
        assert!((b.level(&s) - 0.5).abs() < 1e-6);
        s[2] = 1.0;
        assert_eq!(b.level(&s), 1.0);
        assert_eq!(b.follow(0.2, 0.7, 0.3), 0.7, "rises at once");
        assert_eq!(level_of_amplitude(1.0), 1.0);
        assert!((level_of_amplitude(0.001) - 0.0).abs() < 1e-6, "−60 dB");
    }
}
