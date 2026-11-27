//! SMPTE timecode in (the SMPTE 1 / SMPTE 2 inputs).
//!
//! - [`Fps`] / [`Timecode`]: frame maths, drop-frame 29.97 included.
//! - [`LtcDecoder`]: Linear Timecode from audio samples (biphase-mark code,
//!   80-bit frames ending in the sync word). [`LtcEncoder`] makes LTC for
//!   tests and the test-signal tool.
//! - [`MtcDecoder`]: MIDI Timecode (quarter frames `F1 xx`, full-frame sysex).
//! - [`SharedTimecode`]: last reading, written by the audio / MIDI thread.
//! - [`TimecodeClock`]: UI side; runs on between readings and stops when
//!   the signal does.

use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Timecode frame rate (Preferences › Audio › SMPTE Timecode Framerate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Fps {
    F24,
    #[default]
    F25,
    /// 29.97 drop frame.
    F2997Df,
    F30,
}

impl Fps {
    pub const ALL: [Fps; 4] = [Fps::F24, Fps::F25, Fps::F2997Df, Fps::F30];

    /// Frames counted per second (30 for 29.97).
    pub fn nominal(self) -> u32 {
        match self {
            Fps::F24 => 24,
            Fps::F25 => 25,
            Fps::F2997Df | Fps::F30 => 30,
        }
    }

    /// Real frames per second.
    pub fn rate(self) -> f64 {
        match self {
            Fps::F2997Df => 30_000.0 / 1001.0,
            f => f.nominal() as f64,
        }
    }

    pub fn drop_frame(self) -> bool {
        self == Fps::F2997Df
    }

    pub fn label(self) -> &'static str {
        match self {
            Fps::F24 => "24",
            Fps::F25 => "25",
            Fps::F2997Df => "29.97 drop",
            Fps::F30 => "30",
        }
    }

    /// MTC rate code (2 bits).
    pub fn from_mtc(code: u8) -> Self {
        match code & 3 {
            0 => Fps::F24,
            1 => Fps::F25,
            2 => Fps::F2997Df,
            _ => Fps::F30,
        }
    }

    pub fn mtc_code(self) -> u8 {
        match self {
            Fps::F24 => 0,
            Fps::F25 => 1,
            Fps::F2997Df => 2,
            Fps::F30 => 3,
        }
    }
}

/// HH:MM:SS:FF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timecode {
    pub h: u8,
    pub m: u8,
    pub s: u8,
    pub f: u8,
}

impl Timecode {
    pub fn new(h: u8, m: u8, s: u8, f: u8) -> Self {
        Self { h, m, s, f }
    }

    /// Fields in range for `fps` (frames 0/1 don't exist at drop-frame minute starts).
    pub fn is_valid(&self, fps: Fps) -> bool {
        let dropped = fps.drop_frame() && self.s == 0 && !self.m.is_multiple_of(10) && self.f < 2;
        self.h < 24 && self.m < 60 && self.s < 60 && (self.f as u32) < fps.nominal() && !dropped
    }

    /// Frames since 00:00:00:00.
    pub fn frame_number(&self, fps: Fps) -> i64 {
        let n = fps.nominal() as i64;
        let (h, m, s, f) = (self.h as i64, self.m as i64, self.s as i64, self.f as i64);
        let mut frames = ((h * 60 + m) * 60 + s) * n + f;
        if fps.drop_frame() {
            let minutes = h * 60 + m;
            frames -= 2 * (minutes - minutes / 10);
        }
        frames
    }

    /// Inverse of [`frame_number`](Self::frame_number) (wraps at 24 h).
    pub fn from_frame_number(frames: i64, fps: Fps) -> Self {
        let n = fps.nominal() as i64;
        let mut fr = frames;
        if fps.drop_frame() {
            // 17982 frames per 10 minutes, 1798 per dropped minute.
            let day = 17_982 * 6 * 24;
            fr = fr.rem_euclid(day);
            let tens = fr / 17_982;
            let rest = fr % 17_982;
            let extra = if rest < 2 { 0 } else { 2 * ((rest - 2) / 1798) };
            fr += 18 * tens + extra;
        }
        let day = n * 86_400;
        let fr = fr.rem_euclid(day);
        Self {
            h: (fr / (n * 3600)) as u8,
            m: (fr / (n * 60) % 60) as u8,
            s: (fr / n % 60) as u8,
            f: (fr % n) as u8,
        }
    }

    /// Seconds since 00:00:00:00 (real time: 29.97 frames last 1001/30000 s).
    pub fn seconds(&self, fps: Fps) -> f64 {
        self.frame_number(fps) as f64 / fps.rate()
    }

    /// The frame showing at `secs`.
    pub fn from_seconds(secs: f64, fps: Fps) -> Self {
        Self::from_frame_number((secs * fps.rate() + 1e-6).floor() as i64, fps)
    }

    /// `HH:MM:SS:FF` (`;` before the frames for drop frame).
    pub fn display(&self, fps: Fps) -> String {
        let sep = if fps.drop_frame() { ';' } else { ':' };
        format!("{:02}:{:02}:{:02}{sep}{:02}", self.h, self.m, self.s, self.f)
    }

    /// Parse `HH:MM:SS:FF` (also `;` / `.` separators, or fewer fields:
    /// `SS:FF`, `MM:SS:FF`).
    pub fn parse(text: &str) -> Option<Self> {
        let parts: Vec<u8> = text
            .trim()
            .split([':', ';', '.'])
            .map(|p| p.trim().parse::<u8>().ok())
            .collect::<Option<_>>()?;
        let mut v = [0u8; 4];
        if parts.is_empty() || parts.len() > 4 {
            return None;
        }
        v[4 - parts.len()..].copy_from_slice(&parts);
        Some(Self::new(v[0], v[1], v[2], v[3]))
    }
}

impl fmt::Display for Timecode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}:{:02}:{:02}", self.h, self.m, self.s, self.f)
    }
}

// ------------------------------------------------------------------- LTC

/// Sync word in bits 64..79 (transmitted 0011 1111 1111 1101).
const LTC_SYNC: u16 = 0xBFFC;

/// A decoded LTC frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LtcFrame {
    pub tc: Timecode,
    /// Drop-frame flag (bit 10).
    pub drop_frame: bool,
}

/// Biphase-mark LTC decoder, one audio channel.
///
/// A transition starts every bit; a "1" has a second one half-way. The bit
/// length is tracked as the signal goes (any of 24…30 fps, speed drift).
pub struct LtcDecoder {
    /// Current bit length estimate (samples).
    period: f32,
    /// Samples since the last transition.
    since: f32,
    /// Signal is on the high side (hysteresis state).
    high: bool,
    /// Running peak level (for the hysteresis threshold).
    peak: f32,
    /// First half of a "1" seen.
    half: bool,
    /// Last 80 bits, the newest at bit 79.
    reg: u128,
    /// Bits received since the last sync word (a frame needs 80).
    bits: u32,
    sample_rate: f32,
}

impl LtcDecoder {
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1) as f32;
        Self {
            period: sample_rate / 2000.0, // 25 fps × 80 bits
            since: 0.0,
            high: false,
            peak: 0.0,
            half: false,
            reg: 0,
            bits: 0,
            sample_rate,
        }
    }

    fn reset_bits(&mut self) {
        self.half = false;
        self.bits = 0;
    }

    /// Feed one sample; a frame comes out right after its sync word's last bit.
    pub fn push(&mut self, s: f32) -> Option<LtcFrame> {
        self.since += 1.0;
        let a = s.abs();
        // Peak follower: instant attack, ~50 ms release.
        self.peak = if a > self.peak {
            a
        } else {
            self.peak * (1.0 - 20.0 / self.sample_rate)
        };
        // Silence or a lost signal: start over.
        if self.since > self.period * 4.0 {
            self.reset_bits();
            if self.since > self.sample_rate {
                self.since = self.sample_rate; // no overflow while silent
            }
        }
        let thr = (self.peak * 0.2).max(0.005);
        let flip = if self.high { s < -thr } else { s > thr };
        if !flip {
            return None;
        }
        self.high = !self.high;
        let d = std::mem::take(&mut self.since);
        self.edge(d)
    }

    fn edge(&mut self, d: f32) -> Option<LtcFrame> {
        let p = self.period;
        if d < p * 0.3 || d > p * 2.0 {
            // Noise, or the rate changed a lot: take the edge as a fresh start
            // and guess the period from it next time.
            if d > p * 2.0 && d < self.sample_rate / 1000.0 {
                self.period = d; // slower than expected
            } else if d < p * 0.3 && d > self.sample_rate / 8000.0 {
                self.period = d * 2.0;
            }
            self.reset_bits();
            return None;
        }
        if d > p * 0.75 {
            // Full bit: "0".
            if self.half {
                // A half bit without its partner: out of phase.
                self.reset_bits();
                return None;
            }
            self.period = p * 0.9 + d * 0.1;
            self.bit(false)
        } else if self.half {
            self.half = false;
            self.bit(true)
        } else {
            self.half = true;
            self.period = p * 0.9 + 2.0 * d * 0.1;
            None
        }
    }

    fn bit(&mut self, one: bool) -> Option<LtcFrame> {
        self.reg = (self.reg >> 1) | ((one as u128) << 79);
        self.bits = self.bits.saturating_add(1);
        if self.bits < 80 || ((self.reg >> 64) & 0xFFFF) as u16 != LTC_SYNC {
            return None;
        }
        self.bits = 0;
        let r = self.reg;
        let b = |i: u32, n: u32| ((r >> i) & ((1 << n) - 1)) as u8;
        let tc = Timecode {
            f: b(8, 2) * 10 + b(0, 4),
            s: b(24, 3) * 10 + b(16, 4),
            m: b(40, 3) * 10 + b(32, 4),
            h: b(56, 2) * 10 + b(48, 4),
        };
        let drop_frame = b(10, 1) == 1;
        if tc.h > 23 || tc.m > 59 || tc.s > 59 || tc.f > 29 {
            return None;
        }
        Some(LtcFrame { tc, drop_frame })
    }

    /// Bit length estimate in samples (for status displays).
    pub fn period(&self) -> f32 {
        self.period
    }
}

/// LTC generator: square-ish biphase-mark audio at `level`.
pub struct LtcEncoder {
    pub fps: Fps,
    sample_rate: f64,
    level: f32,
    /// Current output polarity.
    high: bool,
    /// Fractional sample position (keeps the bit clock exact over time).
    acc: f64,
}

impl LtcEncoder {
    pub fn new(fps: Fps, sample_rate: u32, level: f32) -> Self {
        Self {
            fps,
            sample_rate: sample_rate as f64,
            level,
            high: false,
            acc: 0.0,
        }
    }

    /// The 80 bits of `tc`, bit 0 first.
    pub fn bits(tc: Timecode, fps: Fps) -> [bool; 80] {
        let mut r: u128 = 0;
        let mut put = |i: u32, n: u32, v: u8| r |= ((v as u128) & ((1 << n) - 1)) << i;
        put(0, 4, tc.f % 10);
        put(8, 2, tc.f / 10);
        put(10, 1, fps.drop_frame() as u8);
        put(16, 4, tc.s % 10);
        put(24, 3, tc.s / 10);
        put(32, 4, tc.m % 10);
        put(40, 3, tc.m / 10);
        put(48, 4, tc.h % 10);
        put(56, 2, tc.h / 10);
        r |= (LTC_SYNC as u128) << 64;
        // Polarity correction: an even number of zeros per frame keeps the
        // waveform's phase the same at every frame start.
        let ones = (r.count_ones() as usize) % 2;
        if ones == 1 {
            let pc = if fps.nominal() == 25 { 59 } else { 27 };
            r |= 1 << pc;
        }
        std::array::from_fn(|i| (r >> i) & 1 == 1)
    }

    /// Append one frame of audio to `out`.
    pub fn frame(&mut self, tc: Timecode, out: &mut Vec<f32>) {
        let half = self.sample_rate / (self.fps.rate() * 160.0);
        for one in Self::bits(tc, self.fps) {
            for h in 0..2 {
                if h == 0 || one {
                    self.high = !self.high;
                }
                self.acc += half;
                let n = self.acc.floor();
                self.acc -= n;
                let v = if self.high { self.level } else { -self.level };
                out.extend(std::iter::repeat_n(v, n as usize));
            }
        }
    }

    /// `frames` consecutive frames starting at `start`, plus the edge that
    /// closes the last bit (a decoder sees a frame when the next one starts).
    pub fn run(&mut self, start: Timecode, frames: i64) -> Vec<f32> {
        let mut out = Vec::new();
        let n0 = start.frame_number(self.fps);
        for i in 0..frames {
            self.frame(Timecode::from_frame_number(n0 + i, self.fps), &mut out);
        }
        let v = if self.high { -self.level } else { self.level };
        out.extend(std::iter::repeat_n(v, 4));
        out
    }
}

// ------------------------------------------------------------------- MTC

/// MIDI Timecode: 8 quarter-frame messages carry one timecode over two frames.
#[derive(Debug, Clone, Default)]
pub struct MtcDecoder {
    nibbles: [u8; 8],
    /// Pieces seen since piece 0, in order.
    next: u8,
}

/// A decoded MTC position: seconds at the moment the message arrived.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MtcPosition {
    pub secs: f64,
    pub fps: Fps,
    /// Full-frame message (a locate; the sender may be stopped).
    pub full: bool,
}

impl MtcDecoder {
    /// Any MIDI message; returns a position after the 8th quarter frame of a
    /// sequence or a full-frame sysex.
    pub fn message(&mut self, b: &[u8]) -> Option<MtcPosition> {
        match b {
            [0xF1, d, ..] => self.quarter(*d),
            [0xF0, 0x7F, _, 0x01, 0x01, hr, mn, sc, fr, ..] => {
                self.next = 0;
                let fps = Fps::from_mtc(hr >> 5);
                let tc = Timecode::new(hr & 0x1F, *mn, *sc, *fr);
                tc.is_valid(fps).then(|| MtcPosition {
                    secs: tc.seconds(fps),
                    fps,
                    full: true,
                })
            }
            _ => None,
        }
    }

    fn quarter(&mut self, d: u8) -> Option<MtcPosition> {
        let piece = (d >> 4) & 7;
        if piece != self.next {
            // Out of order (or running backward): wait for the next piece 0.
            self.next = 0;
            if piece != 0 {
                return None;
            }
        }
        self.nibbles[piece as usize] = d & 0x0F;
        self.next = (piece + 1) % 8;
        if piece != 7 {
            return None;
        }
        let n = &self.nibbles;
        let fps = Fps::from_mtc(n[7] >> 1);
        let tc = Timecode::new(
            n[6] | (n[7] & 1) << 4,
            n[4] | (n[5] & 3) << 4,
            n[2] | (n[3] & 3) << 4,
            n[0] | (n[1] & 1) << 4,
        );
        // The sequence started with piece 0 at frame `tc`; piece 7 arrives
        // 7 quarter frames later.
        tc.is_valid(fps).then(|| MtcPosition {
            secs: tc.seconds(fps) + 1.75 / fps.rate(),
            fps,
            full: false,
        })
    }
}

/// MTC bytes for testing / sending: the 8 quarter frames of `tc`.
pub fn mtc_quarter_frames(tc: Timecode, fps: Fps) -> [[u8; 2]; 8] {
    let v = [
        tc.f & 0x0F,
        tc.f >> 4 & 1,
        tc.s & 0x0F,
        tc.s >> 4 & 3,
        tc.m & 0x0F,
        tc.m >> 4 & 3,
        tc.h & 0x0F,
        (tc.h >> 4 & 1) | fps.mtc_code() << 1,
    ];
    std::array::from_fn(|i| [0xF1, (i as u8) << 4 | v[i]])
}

// --------------------------------------------------------- shared reading

/// One timecode observation: `secs` was current at `at`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    pub secs: f64,
    pub fps: Fps,
    pub at: Instant,
    /// A locate (MTC full frame): position only, not proof of running.
    pub locate: bool,
}

/// Latest reading, written by an audio / MIDI thread, read by the UI.
#[derive(Default)]
pub struct SharedTimecode {
    last: Mutex<Option<Reading>>,
    count: AtomicU64,
}

impl SharedTimecode {
    /// Never blocks (audio callbacks): a busy slot drops this reading.
    pub fn store(&self, r: Reading) {
        if let Ok(mut l) = self.last.try_lock() {
            *l = Some(r);
            self.count.fetch_add(1, Ordering::Release);
        }
    }

    pub fn load(&self) -> Option<Reading> {
        self.last.lock().ok().and_then(|l| *l)
    }

    /// Readings so far.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }
}

/// What an input shows now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimecodeNow {
    /// Seconds since 00:00:00:00, delay compensation included.
    pub secs: f64,
    pub fps: Fps,
    /// Readings keep arriving and moving forward.
    pub running: bool,
}

impl TimecodeNow {
    pub fn timecode(&self) -> Timecode {
        Timecode::from_seconds(self.secs.max(0.0), self.fps)
    }
}

/// UI-side clock of one input: between readings it runs on in real time; it
/// stops (holds the last position) when readings stop or stand still.
#[derive(Debug, Clone, Default)]
pub struct TimecodeClock {
    last: Option<Reading>,
    prev: Option<Reading>,
}

/// Readings further apart than this mean the timecode stopped.
const STALE: Duration = Duration::from_millis(250);

impl TimecodeClock {
    /// Take a new reading (no-op if it's the one already held).
    pub fn update(&mut self, r: Option<Reading>) {
        let Some(r) = r else { return };
        if self.last.is_some_and(|l| l.at == r.at && l.secs == r.secs) {
            return;
        }
        self.prev = self.last.replace(r);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Position at `now`, `delay_frames` added (delay compensation).
    pub fn now(&self, now: Instant, delay_frames: f64) -> Option<TimecodeNow> {
        let last = self.last?;
        let age = now.saturating_duration_since(last.at);
        let moving = self.prev.is_some_and(|p| {
            let dt = last.at.saturating_duration_since(p.at).as_secs_f64();
            let ds = last.secs - p.secs;
            // Forward at roughly real speed (×0.5…×2), readings close together.
            !last.locate && dt > 0.0 && dt < STALE.as_secs_f64() && ds > dt * 0.5 && ds < dt * 2.0 + 0.1
        });
        let running = moving && age < STALE;
        let run_on = if running { age.as_secs_f64() } else { 0.0 };
        Some(TimecodeNow {
            secs: last.secs + run_on + delay_frames / last.fps.rate(),
            fps: last.fps,
            running,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_numbers_round_trip() {
        for fps in Fps::ALL {
            for n in [0i64, 1, 29, 1799, 1800, 1801, 17_982, 17_983, 107_892, 2_589_407] {
                let tc = Timecode::from_frame_number(n, fps);
                assert!(tc.is_valid(fps), "{fps:?} {n} → {tc}");
                let day = Timecode::new(23, 59, 59, fps.nominal() as u8 - 1).frame_number(fps) + 1;
                assert_eq!(tc.frame_number(fps), n % day, "{fps:?} {tc}");
            }
        }
        // Drop frame skips ;00 and ;01 at minutes not divisible by 10.
        let df = Fps::F2997Df;
        assert_eq!(Timecode::from_frame_number(1800, df), Timecode::new(0, 1, 0, 2));
        assert_eq!(Timecode::from_frame_number(17_982, df), Timecode::new(0, 10, 0, 0));
        // One hour of 29.97 DF is 3600 real seconds (within a frame).
        let hour = Timecode::new(1, 0, 0, 0).seconds(df);
        assert!((hour - 3600.0).abs() < 0.004, "{hour}");
        assert_eq!(Timecode::new(1, 2, 3, 4).seconds(Fps::F25), 3723.16);
        assert_eq!(Timecode::from_seconds(3723.16, Fps::F25), Timecode::new(1, 2, 3, 4));
    }

    #[test]
    fn parse_and_display() {
        assert_eq!(Timecode::parse("01:02:03:04"), Some(Timecode::new(1, 2, 3, 4)));
        assert_eq!(Timecode::parse("3:04"), Some(Timecode::new(0, 0, 3, 4)));
        assert_eq!(Timecode::parse("00;01;00;02"), Some(Timecode::new(0, 1, 0, 2)));
        assert_eq!(Timecode::parse("x"), None);
        assert_eq!(Timecode::new(0, 1, 0, 2).display(Fps::F2997Df), "00:01:00;02");
    }

    fn decode_all(dec: &mut LtcDecoder, samples: &[f32]) -> Vec<(usize, LtcFrame)> {
        samples
            .iter()
            .enumerate()
            .filter_map(|(i, &s)| dec.push(s).map(|f| (i, f)))
            .collect()
    }

    #[test]
    fn ltc_round_trips_every_rate() {
        for fps in Fps::ALL {
            for sr in [44_100, 48_000, 96_000] {
                let start = Timecode::new(10, 59, 58, 3);
                let mut enc = LtcEncoder::new(fps, sr, 0.5);
                let audio = enc.run(start, 60);
                let mut dec = LtcDecoder::new(sr);
                let got = decode_all(&mut dec, &audio);
                // The first frame or two lock the decoder.
                assert!(got.len() >= 57, "{fps:?} @ {sr}: {} frames", got.len());
                let n0 = start.frame_number(fps);
                let (_, last) = got.last().unwrap();
                assert_eq!(last.tc, Timecode::from_frame_number(n0 + 59, fps), "{fps:?} @ {sr}");
                assert_eq!(last.drop_frame, fps.drop_frame());
                // Consecutive frames.
                for w in got.windows(2) {
                    assert_eq!(w[1].1.tc.frame_number(fps), w[0].1.tc.frame_number(fps) + 1);
                }
                // Each frame is reported where it ends.
                let per = sr as f64 / fps.rate();
                let (i, _) = got.last().unwrap();
                assert!(
                    (*i as f64 - (60.0 * per - 1.0)).abs() < per * 0.02,
                    "{i} vs {}",
                    60.0 * per
                );
            }
        }
    }

    #[test]
    fn ltc_survives_noise_level_and_inversion() {
        let mut enc = LtcEncoder::new(Fps::F25, 48_000, 0.05);
        let audio = enc.run(Timecode::new(0, 0, 1, 0), 30);
        let mut rng = 12345u32;
        let noisy: Vec<f32> = audio
            .iter()
            .map(|s| {
                rng = rng.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let n = ((rng >> 16) as f32 / 65_536.0 - 0.5) * 0.01;
                -(s + n) // inverted polarity, quiet, a little noise
            })
            .collect();
        let got = decode_all(&mut LtcDecoder::new(48_000), &noisy);
        assert!(got.len() >= 27, "{}", got.len());
        assert_eq!(got.last().unwrap().1.tc, Timecode::new(0, 0, 2, 4));
    }

    #[test]
    fn ltc_relocks_after_a_gap() {
        let mut enc = LtcEncoder::new(Fps::F30, 48_000, 0.5);
        let mut audio = enc.run(Timecode::new(0, 0, 0, 0), 10);
        audio.extend(std::iter::repeat_n(0.0, 24_000));
        audio.extend(enc.run(Timecode::new(1, 0, 0, 0), 10));
        let got = decode_all(&mut LtcDecoder::new(48_000), &audio);
        assert_eq!(got.last().unwrap().1.tc, Timecode::new(1, 0, 0, 9));
        assert!(got.iter().filter(|(_, f)| f.tc.h == 1).count() >= 8);
    }

    #[test]
    fn mtc_quarter_frames_and_full_frame() {
        let mut d = MtcDecoder::default();
        let fps = Fps::F25;
        let tc = Timecode::new(1, 2, 3, 4);
        let mut out = None;
        // Start mid-sequence: the decoder waits for piece 0.
        for q in &mtc_quarter_frames(Timecode::new(1, 2, 3, 2), fps)[5..] {
            assert_eq!(d.message(q), None);
        }
        for q in mtc_quarter_frames(tc, fps) {
            out = d.message(&q).or(out);
        }
        let p = out.unwrap();
        assert!(!p.full);
        assert_eq!(p.fps, fps);
        assert!((p.secs - (tc.seconds(fps) + 1.75 / 25.0)).abs() < 1e-9);
        let full = [0xF0, 0x7F, 0x7F, 0x01, 0x01, 0x60 | 10, 20, 30, 15, 0xF7];
        let p = d.message(&full).unwrap();
        assert!(p.full);
        assert_eq!(p.fps, Fps::F30);
        assert_eq!(Timecode::from_seconds(p.secs, p.fps), Timecode::new(10, 20, 30, 15));
        // 29.97 DF hour bit and rate in piece 7.
        let tc = Timecode::new(23, 59, 59, 29);
        let mut out = None;
        for q in mtc_quarter_frames(tc, Fps::F2997Df) {
            out = d.message(&q).or(out);
        }
        assert_eq!(out.unwrap().fps, Fps::F2997Df);
        assert!((out.unwrap().secs - tc.seconds(Fps::F2997Df) - 1.75 / Fps::F2997Df.rate()).abs() < 1e-9);
    }

    #[test]
    fn clock_runs_between_readings_and_stops() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let r = |secs: f64, ms: u64| Reading {
            secs,
            fps: Fps::F25,
            at: at(ms),
            locate: false,
        };
        let mut c = TimecodeClock::default();
        assert!(c.now(at(0), 0.0).is_none());
        c.update(Some(r(10.0, 0)));
        let n = c.now(at(20), 0.0).unwrap();
        assert!(!n.running, "one reading isn't running yet");
        assert_eq!(n.secs, 10.0);
        c.update(Some(r(10.04, 40)));
        let n = c.now(at(60), 0.0).unwrap();
        assert!(n.running);
        assert!((n.secs - 10.06).abs() < 1e-9);
        // Delay compensation: 2 frames at 25 fps.
        assert!((c.now(at(60), 2.0).unwrap().secs - 10.14).abs() < 1e-9);
        // Signal gone: holds where the last reading was.
        let n = c.now(at(600), 0.0).unwrap();
        assert!(!n.running);
        assert_eq!(n.secs, 10.04);
        // Same reading again changes nothing; a locate holds.
        c.update(Some(r(10.04, 40)));
        assert!(c.now(at(60), 0.0).unwrap().running);
        c.update(Some(Reading {
            locate: true,
            ..r(50.0, 700)
        }));
        let n = c.now(at(720), 0.0).unwrap();
        assert!(!n.running);
        assert_eq!(n.timecode(), Timecode::new(0, 0, 50, 0));
    }
}
