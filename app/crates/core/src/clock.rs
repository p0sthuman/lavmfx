//! Global clock: wall time + beat position, tap tempo. Link comes in M7.

use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Clock {
    start: Instant,
    /// Beats per minute.
    pub bpm: f64,
    beat_offset: f64,
    beat_anchor: Duration,
    /// Recent tap times (tap tempo).
    taps: Vec<Duration>,
    /// Beat clock stopped (toolbar PAUSE).
    paused: bool,
    /// Temporary tempo change while a nudge button is held (−0.5…0.5).
    nudge: f64,
}

impl Default for Clock {
    fn default() -> Self {
        Self::new(120.0)
    }
}

impl Clock {
    pub fn new(bpm: f64) -> Self {
        Self {
            start: Instant::now(),
            bpm,
            beat_offset: 0.0,
            beat_anchor: Duration::ZERO,
            taps: Vec::new(),
            paused: false,
            nudge: 0.0,
        }
    }

    /// Seconds since the clock started.
    pub fn seconds(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    /// Continuous beat position.
    pub fn beats(&self) -> f64 {
        self.beats_at(self.start.elapsed())
    }

    fn beats_at(&self, t: Duration) -> f64 {
        self.beat_offset + (t.as_secs_f64() - self.beat_anchor.as_secs_f64()) * self.effective_bpm() / 60.0
    }

    /// Tempo the beat actually runs at (0 when paused, nudged while held).
    pub fn effective_bpm(&self) -> f64 {
        if self.paused {
            0.0
        } else {
            self.bpm * (1.0 + self.nudge)
        }
    }

    /// Re-anchor, then change how the beat runs (no jump in position).
    fn rebase(&mut self, f: impl FnOnce(&mut Self)) {
        let now = self.start.elapsed();
        self.beat_offset = self.beats_at(now);
        self.beat_anchor = now;
        f(self);
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Stop / restart the beat clock (BPM-synced clips and animations hold).
    pub fn set_paused(&mut self, p: bool) {
        self.rebase(|c| c.paused = p);
    }

    /// Nudge the tempo by a fraction while held (0 = release).
    pub fn set_nudge(&mut self, n: f64) {
        if n != self.nudge {
            self.rebase(|c| c.nudge = n.clamp(-0.5, 0.5));
        }
    }

    /// Change tempo without jumping the beat position.
    pub fn set_bpm(&mut self, bpm: f64) {
        self.set_bpm_at(bpm, self.start.elapsed());
    }

    fn set_bpm_at(&mut self, bpm: f64, now: Duration) {
        self.beat_offset = self.beats_at(now);
        self.beat_anchor = now;
        self.bpm = bpm.clamp(20.0, 500.0);
    }

    /// Tap tempo: returns the new BPM once there are two taps. A pause of
    /// more than 2 s starts over; the last 8 intervals are averaged. Each tap
    /// also puts the beat on the nearest whole beat.
    pub fn tap(&mut self) -> Option<f64> {
        self.tap_at(self.start.elapsed())
    }

    fn tap_at(&mut self, now: Duration) -> Option<f64> {
        if self
            .taps
            .last()
            .is_some_and(|&t| now.saturating_sub(t) > Duration::from_secs(2))
        {
            self.taps.clear();
        }
        self.taps.push(now);
        if self.taps.len() > 9 {
            self.taps.remove(0);
        }
        let n = self.taps.len();
        let bpm = (n >= 2).then(|| {
            let span = (self.taps[n - 1] - self.taps[0]).as_secs_f64();
            60.0 * (n - 1) as f64 / span.max(1e-3)
        });
        if let Some(b) = bpm {
            self.set_bpm_at(b, now);
        }
        // Tap lands on a beat.
        let beats = self.beats_at(now).round();
        self.beat_offset = beats;
        self.beat_anchor = now;
        bpm.map(|_| self.bpm)
    }

    /// Bar (1-based) and beat in the bar (1–4).
    /// Follow an external clock (Ableton Link): tempo and beat position now.
    pub fn follow(&mut self, bpm: f64, beats: f64) {
        self.bpm = bpm.clamp(20.0, 500.0);
        self.beat_offset = beats;
        self.beat_anchor = self.start.elapsed();
    }

    pub fn bar_beat(&self) -> (i64, i64) {
        let b = self.beats().floor() as i64;
        (b.div_euclid(4) + 1, b.rem_euclid(4) + 1)
    }

    /// Snap the beat position to bar 1 ("resync").
    pub fn resync(&mut self) {
        self.beat_anchor = self.start.elapsed();
        self.beat_offset = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_and_nudge_keep_the_position() {
        let mut c = Clock::new(120.0);
        c.set_paused(true);
        let b = c.beats();
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(c.beats(), b, "paused");
        assert_eq!(c.effective_bpm(), 0.0);
        c.set_paused(false);
        assert!((c.beats() - b).abs() < 0.01, "no jump on resume");
        c.set_nudge(0.1);
        assert!((c.effective_bpm() - 132.0).abs() < 1e-9);
        c.set_nudge(0.0);
        assert_eq!(c.effective_bpm(), 120.0);
    }

    #[test]
    fn beats_follow_bpm() {
        let c = Clock::new(120.0);
        assert!((c.beats_at(Duration::from_secs(3)) - 6.0).abs() < 1e-9);
    }

    #[test]
    fn tap_tempo() {
        let mut c = Clock::new(120.0);
        let ms = Duration::from_millis;
        assert_eq!(c.tap_at(ms(1000)), None);
        let bpm = c.tap_at(ms(1500)).unwrap(); // 0.5 s → 120
        assert!((bpm - 120.0).abs() < 1e-6);
        c.tap_at(ms(1950));
        let bpm = c.tap_at(ms(2400)).unwrap(); // 3 intervals over 1.4 s
        assert!((bpm - 60.0 * 3.0 / 1.4).abs() < 1e-6, "{bpm}");
        // Long pause: starts over.
        assert_eq!(c.tap_at(ms(9000)), None);
        // The tap is on a whole beat.
        assert!((c.beats_at(ms(9000)).fract()).abs() < 1e-9);
    }
}
