//! Mono samples → `Spectrum` (log bands, 0..1). Hann window of `N` samples,
//! a new spectrum every `HOP` samples. No allocation after `new`, so it can
//! run inside an audio callback.

use std::sync::Arc;

use lavmfx_core::fft::{self, BANDS, Spectrum};
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

pub const N: usize = 2048;
pub const HOP: usize = 512;

/// Fan noise reduction: the noise floor of a band is the lowest smoothed
/// level seen over `FLOOR_SUBS` × `FLOOR_SUB_SECS` (minimum statistics):
/// steady noise (laptop fans, hum) sits there all the time; music dips below
/// its peaks, so it isn't learned as noise.
const FLOOR_SUBS: usize = 8;
const FLOOR_SUB_SECS: f64 = 0.5;
/// The gate sits at the floor × (1 + amount × this): at 100 % 5× the floor
/// (+14 dB; the floor is a minimum, the noise's frames peak well above it).
pub const NOISE_HEADROOM: f32 = 4.0;

/// Which FFT bins (with interpolation weights) a band reads.
#[derive(Debug, Clone, Copy)]
struct BandBins {
    /// Bins fully inside the band.
    first: usize,
    last: usize,
    /// Fractional bin at the band centre (used when no bin is inside).
    centre: f64,
}

pub struct Analyzer {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    ring: Vec<f32>,
    pos: usize,
    since: usize,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    bands: [BandBins; BANDS],
    /// Multiplier applied to incoming samples (Preferences › FFT input gain).
    pub gain: f32,
    /// Fan noise reduction 0 (off) … 1 (Preferences › Audio).
    pub noise: f32,
    /// Per band, before the gain (so the reduction follows the gain at once):
    /// smoothed amplitude, this sub-window's minimum, the last sub-windows' minima.
    smooth: [f32; BANDS],
    sub_min: [f32; BANDS],
    sub_mins: [[f32; BANDS]; FLOOR_SUBS],
    sub_left: usize,
    sub_len: usize,
    sub_at: usize,
    floor: [f32; BANDS],
    spectrum: Spectrum,
    fresh: bool,
}

impl Analyzer {
    pub fn new(sample_rate: u32) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(N);
        let window = (0..N)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N as f32).cos())
            .collect();
        let input = fft.make_input_vec();
        let output = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        let hz_per_bin = sample_rate.max(1) as f64 / N as f64;
        let bands = std::array::from_fn(|i| {
            let (lo, hi) = fft::band_edges(i);
            let first = (lo / hz_per_bin).ceil() as usize;
            let last = ((hi / hz_per_bin).ceil() as usize).saturating_sub(1).min(N / 2);
            BandBins {
                first,
                last,
                centre: (lo * hi).sqrt() / hz_per_bin,
            }
        });
        Self {
            fft,
            window,
            ring: vec![0.0; N],
            pos: 0,
            since: 0,
            input,
            output,
            scratch,
            bands,
            gain: 1.0,
            noise: 0.0,
            smooth: [0.0; BANDS],
            sub_min: [f32::MAX; BANDS],
            sub_mins: [[f32::MAX; BANDS]; FLOOR_SUBS],
            sub_len: ((FLOOR_SUB_SECS * sample_rate.max(1) as f64 / HOP as f64).round() as usize).max(1),
            sub_left: 0,
            sub_at: 0,
            floor: [0.0; BANDS],
            spectrum: fft::SILENT,
            fresh: false,
        }
    }

    /// Feed mono samples; analyses every `HOP` samples.
    pub fn push(&mut self, samples: &[f32]) {
        for &s in samples {
            self.ring[self.pos] = s * self.gain;
            self.pos = (self.pos + 1) % N;
            self.since += 1;
            if self.since >= HOP {
                self.since = 0;
                self.analyse();
            }
        }
    }

    /// Feed interleaved frames, reading `channel` (None = average of all).
    pub fn push_interleaved(&mut self, data: &[f32], channels: usize, channel: Option<usize>) {
        let ch = channels.max(1);
        for frame in data.chunks_exact(ch) {
            let s = match channel {
                Some(c) => frame.get(c).copied().unwrap_or(0.0),
                None => frame.iter().sum::<f32>() / ch as f32,
            };
            self.push(std::slice::from_ref(&s));
        }
    }

    fn analyse(&mut self) {
        // Oldest sample first.
        for i in 0..N {
            self.input[i] = self.ring[(self.pos + i) % N] * self.window[i];
        }
        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch)
            .is_err()
        {
            return;
        }
        // Amplitude of a full-scale sine = 1: |X| · 2 / Σwindow (= N/2 for Hann).
        let k = 4.0 / N as f32;
        let mag = |b: usize| self.output.get(b).map_or(0.0, |c| c.norm() * k);
        let mut amps = [0.0f32; BANDS];
        for (i, b) in self.bands.iter().enumerate() {
            let inside = (b.first..=b.last).map(mag).fold(0.0f32, f32::max);
            let a = if b.first > b.last {
                // Narrow low band: interpolate between the two nearest bins.
                let j = b.centre.floor() as usize;
                let t = (b.centre - j as f64) as f32;
                mag(j) * (1.0 - t) + mag(j + 1) * t
            } else {
                inside
            };
            amps[i] = a;
        }
        for (i, a) in amps.into_iter().enumerate() {
            self.spectrum[i] = self.denoise(i, a);
        }
        self.next_sub_window();
        self.fresh = true;
    }

    /// Track band `i`'s noise floor (always, so it's ready when switched on)
    /// and return its meter level for amplitude `a` (after the gain): with the
    /// reduction on, levels up to the gate (a little above the floor) read 0
    /// and the rest is stretched back to the full meter. On the dB meter both
    /// move together with the gain, so the result doesn't depend on it.
    fn denoise(&mut self, i: usize, a: f32) -> f32 {
        let g = self.gain.max(1e-6);
        let pre = a / g;
        self.smooth[i] += 0.2 * (pre - self.smooth[i]);
        self.sub_min[i] = self.sub_min[i].min(self.smooth[i]);
        let f = self.sub_mins.iter().fold(self.sub_min[i], |m, w| m.min(w[i]));
        self.floor[i] = if f == f32::MAX { 0.0 } else { f };
        let level = fft::level_of_amplitude(a as f64);
        if self.noise <= 0.0 || self.floor[i] <= 0.0 {
            return level;
        }
        let gate = fft::level_of_amplitude((self.floor[i] * g * (1.0 + self.noise * NOISE_HEADROOM)) as f64);
        if gate >= 1.0 {
            return 0.0;
        }
        ((level - gate) / (1.0 - gate)).max(0.0)
    }

    fn next_sub_window(&mut self) {
        if self.sub_left == 0 {
            self.sub_left = self.sub_len;
            self.sub_at = (self.sub_at + 1) % FLOOR_SUBS;
            self.sub_mins[self.sub_at] = self.sub_min;
            self.sub_min = [f32::MAX; BANDS];
        }
        self.sub_left -= 1;
    }

    /// The learned noise floor per band, as meter levels at the current gain.
    pub fn noise_floor(&self) -> Spectrum {
        std::array::from_fn(|i| fft::level_of_amplitude((self.floor[i] * self.gain) as f64))
    }

    pub fn spectrum(&self) -> &Spectrum {
        &self.spectrum
    }

    /// True once after each new analysis.
    pub fn take_fresh(&mut self) -> bool {
        std::mem::take(&mut self.fresh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f64, amp: f32, sr: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (std::f64::consts::TAU * f * i as f64 / sr as f64).sin() as f32)
            .collect()
    }

    fn band_of(f: f64) -> usize {
        ((fft::pos_of(f) * BANDS as f64) as usize).min(BANDS - 1)
    }

    #[test]
    fn sine_lands_in_its_band() {
        for (f, sr) in [(60.0, 44_100), (1000.0, 48_000), (8000.0, 44_100)] {
            let mut a = Analyzer::new(sr);
            a.push(&sine(f, 1.0, sr, N * 2));
            let s = a.spectrum();
            let b = band_of(f);
            assert!(s[b] > 0.9, "{f} Hz: own band {b} = {}", s[b]);
            let far = if b > BANDS / 2 { 2 } else { BANDS - 2 };
            assert!(s[far] < 0.3, "{f} Hz: far band {far} = {}", s[far]);
        }
    }

    #[test]
    fn level_follows_amplitude_and_gain() {
        let sr = 48_000;
        let mut a = Analyzer::new(sr);
        // −20 dB → 2/3 of the 60 dB scale.
        a.push(&sine(440.0, 0.1, sr, N * 2));
        let b = band_of(440.0);
        assert!((a.spectrum()[b] - 0.667).abs() < 0.05, "{}", a.spectrum()[b]);
        a.gain = 10.0;
        a.push(&sine(440.0, 0.1, sr, N * 2));
        assert!(a.spectrum()[b] > 0.95);
        a.push(&vec![0.0; N * 2]);
        assert!(a.spectrum().iter().all(|&v| v == 0.0), "silence");
    }

    /// Fan-like noise: white noise through a one-pole low-pass (seeded).
    fn fan(n: usize, amp: f32, seed: &mut u32, lp: &mut f32) -> Vec<f32> {
        (0..n)
            .map(|_| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let w = (*seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
                *lp += 0.08 * (w - *lp);
                *lp * amp
            })
            .collect()
    }

    /// Mean meter level per band over `samples`, analysed hop by hop.
    fn mean_levels(a: &mut Analyzer, samples: &[f32]) -> Spectrum {
        let mut sum = [0.0f32; BANDS];
        let mut n = 0;
        for c in samples.chunks(HOP) {
            a.push(c);
            if a.take_fresh() {
                sum.iter_mut().zip(a.spectrum()).for_each(|(s, v)| *s += v);
                n += 1;
            }
        }
        sum.map(|s| s / n.max(1) as f32)
    }

    #[test]
    fn fan_noise_is_removed_tones_stay() {
        let sr = 48_000;
        let (mut seed, mut lp) = (7u32, 0.0f32);
        let mut a = Analyzer::new(sr);
        a.gain = 2.0;
        // Learn: 5 s of fan noise only.
        a.push(&fan(sr as usize * 5, 0.15, &mut seed, &mut lp));
        let before = mean_levels(&mut a, &fan(sr as usize, 0.15, &mut seed, &mut lp));
        let loud = before.iter().filter(|&&v| v > 0.2).count();
        assert!(loud >= 8, "the noise shows on the meter: {before:?}");
        a.noise = 0.5;
        let after = mean_levels(&mut a, &fan(sr as usize, 0.15, &mut seed, &mut lp));
        let max = after.iter().copied().fold(0.0f32, f32::max);
        assert!(max < 0.08, "fan noise gone at 50 %: {after:?}");
        // A tone over the noise stays.
        let b = band_of(1000.0);
        let mix: Vec<f32> = fan(sr as usize, 0.15, &mut seed, &mut lp)
            .iter()
            .zip(sine(1000.0, 0.1, sr, sr as usize))
            .map(|(n, t)| n + t)
            .collect();
        let tone = mean_levels(&mut a, &mix);
        assert!(tone[b] > 0.45, "tone band {b}: {}", tone[b]);
        // More gain: the reduction scales with it (no relearning).
        a.gain = 8.0;
        let up = mean_levels(&mut a, &fan(sr as usize / 2, 0.15, &mut seed, &mut lp));
        assert!(up.iter().all(|&v| v < 0.1), "still clean at 8× gain: {up:?}");
        // Off: untouched.
        a.noise = 0.0;
        a.gain = 2.0;
        let off = mean_levels(&mut a, &fan(sr as usize, 0.15, &mut seed, &mut lp));
        assert!(off.iter().filter(|&&v| v > 0.2).count() >= 8);
    }

    #[test]
    fn interleaved_channel_pick() {
        let sr = 44_100;
        let left = sine(100.0, 1.0, sr, N * 2);
        let data: Vec<f32> = left.iter().flat_map(|&l| [l, 0.0]).collect();
        let mut a = Analyzer::new(sr);
        a.push_interleaved(&data, 2, Some(1));
        assert!(a.spectrum().iter().all(|&v| v == 0.0), "right is silent");
        a.push_interleaved(&data, 2, Some(0));
        assert!(a.spectrum()[band_of(100.0)] > 0.9);
    }
}
