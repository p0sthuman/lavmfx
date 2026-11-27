//! Clip transport: where the playhead goes as time passes.
//!
//! Pure maths, no decoding: [`Playhead::advance`] moves the position by the
//! elapsed clip time according to the clip's play mode and direction, and
//! tells the caller whether the decoder can keep reading forward or has to
//! seek.

use crate::defaults::{PLAY_DIRECTIONS, PLAY_MODES};
use crate::model::Transport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayMode {
    Loop,
    Bounce,
    Random,
    OnceClear,
    OnceHold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Backward,
    Pause,
    Forward,
}

impl PlayMode {
    /// From the option text of a `playmode` choice (see [`PLAY_MODES`]).
    pub fn from_option(s: &str) -> Self {
        match PLAY_MODES.iter().position(|m| *m == s) {
            Some(1) => Self::Bounce,
            Some(2) => Self::Random,
            Some(3) => Self::OnceClear,
            Some(4) => Self::OnceHold,
            _ => Self::Loop,
        }
    }
}

impl Direction {
    /// From the option text of a `playdirection` choice (see [`PLAY_DIRECTIONS`]).
    pub fn from_option(s: &str) -> Self {
        match PLAY_DIRECTIONS.iter().position(|m| *m == s) {
            Some(0) => Self::Backward,
            Some(1) => Self::Pause,
            _ => Self::Forward,
        }
    }
}

/// Transport settings of one clip, read from its params.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportSettings {
    pub mode: PlayMode,
    pub direction: Direction,
    /// Clip speed multiplier (1 = normal).
    pub speed: f64,
    /// Return to the clip continues where it was (`false` = restart).
    pub continue_away: bool,
    /// Transport "SMPTE 1" / "SMPTE 2": the position follows timecode.
    pub smpte: Option<SmpteFollow>,
}

/// A clip following an SMPTE input: clip time = timecode − offset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SmpteFollow {
    /// 0 = SMPTE 1, 1 = SMPTE 2.
    pub input: usize,
    /// Timecode (seconds) at which the clip is at its start.
    pub offset: f64,
}

impl Default for TransportSettings {
    fn default() -> Self {
        Self {
            mode: PlayMode::Loop,
            direction: Direction::Forward,
            speed: 1.0,
            continue_away: true,
            smpte: None,
        }
    }
}

impl TransportSettings {
    pub fn of(t: Option<&Transport>) -> Self {
        let Some(t) = t else { return Self::default() };
        let (dir, mode, away, speed) = match t {
            Transport::Timeline(t) => {
                let c = &t.controls;
                (&c.playdirection, &c.playmode, &c.playmodeaway, c.speed.value)
            }
            Transport::BpmSync(t) => {
                let c = &t.controls;
                (&c.playdirection, &c.playmode, &c.playmodeaway, c.speed.value)
            }
        };
        Self {
            mode: PlayMode::from_option(&mode.value),
            direction: Direction::from_option(&dir.value),
            speed,
            continue_away: away.value != "Restart",
            // Used only when the transport type is SMPTE 1 / 2 (see `for_clip`);
            // a type set over OSC / REST may come without the offset param.
            smpte: match t {
                Transport::Timeline(t) => Some(SmpteFollow {
                    input: 0,
                    offset: t.controls.smpteoffset.as_ref().map_or(0.0, |o| o.value),
                }),
                Transport::BpmSync(_) => None,
            },
        }
    }
}

impl TransportSettings {
    /// Settings of `clip` at composition tempo `bpm`: BPM Sync clips get the
    /// speed that makes the file last their loop length (sync mode "Beats":
    /// `file ÷ (beats × 60 / bpm)`; "BPM": `bpm ÷ clip bpm`), times their speed.
    pub fn for_clip(clip: &crate::model::Clip, bpm: f64) -> Self {
        let mut s = Self::of(clip.transport.as_ref());
        // The offset param stays when switching back to Timeline: the type decides.
        s.smpte = match clip.transporttype.index {
            i @ (2 | 3) => s.smpte.map(|f| SmpteFollow {
                input: i as usize - 2,
                ..f
            }),
            _ => None,
        };
        match &clip.transport {
            Some(Transport::BpmSync(t)) => s.speed *= bpm_rate(&t.controls, clip.file_seconds(), bpm),
            Some(Transport::Timeline(t)) => s.speed *= timeline_rate(&t.controls, clip.file_seconds()),
            None => {}
        }
        s
    }
}

/// Playback-rate factor of a Timeline clip: its Duration (seconds at speed 1,
/// ÷2 / ×2) against the file's length. 1 when either is unknown; a new
/// clip's Duration is its file length.
pub fn timeline_rate(c: &crate::model::TimelineControls, file_s: f64) -> f64 {
    let d = c.duration.value;
    if file_s > 0.0 && d > 0.0 && (file_s - d).abs() > 1e-9 {
        let r = file_s / d;
        if r.is_finite() { r } else { 1.0 }
    } else {
        1.0
    }
}

/// Playback-rate factor of a BPM Sync clip (1 when unknown).
pub fn bpm_rate(c: &crate::model::BpmSyncControls, file_s: f64, bpm: f64) -> f64 {
    let rate = if c.syncmode.value == "BPM" {
        bpm / c.bpm.as_ref().map_or(120.0, |b| b.value).max(1e-3)
    } else {
        let loop_s = c.duration.value * 60.0 / bpm.max(1e-3);
        if file_s > 0.0 && loop_s > 0.0 {
            file_s / loop_s
        } else {
            1.0
        }
    };
    if rate.is_finite() { rate } else { 1.0 }
}

/// What the decoder should do after [`Playhead::advance`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// Moved forward continuously (possibly into the next loop lap): keep
    /// decoding ahead, no seek.
    Forward,
    /// Jumped or moving backward onto a new frame: seek to [`Playhead::unwrapped`].
    Seek,
    /// Still on the same frame (paused, holding, or backward within a frame).
    Hold,
    /// Play-once-and-clear reached the end: stop the clip.
    Ended,
}

/// Playhead of a playing clip.
#[derive(Debug, Clone, PartialEq)]
pub struct Playhead {
    /// Position within the file, `0..=duration` seconds.
    pub pos: f64,
    /// Loop count for forward playback; the decoder position is
    /// `lap × duration + pos` so wrapping needs no seek.
    pub lap: i64,
    /// Bounce mode is on its way back.
    pub reversed: bool,
    last_seek_frame: i64,
    random_acc: f64,
    rng: u64,
}

impl Default for Playhead {
    fn default() -> Self {
        Self::at(0.0)
    }
}

impl Playhead {
    pub fn at(pos: f64) -> Self {
        Self {
            pos: pos.max(0.0),
            lap: 0,
            reversed: false,
            last_seek_frame: i64::MIN,
            random_acc: 0.0,
            rng: 0x9E37_79B9_7F4A_7C15,
        }
    }

    /// Decoder position: `lap × duration + pos`.
    pub fn unwrapped(&self, dur: f64) -> f64 {
        self.lap as f64 * dur + self.pos
    }

    /// Jump to `pos` (clamped to the file); the caller seeks the decoder.
    pub fn jump(&mut self, pos: f64, dur: f64) {
        self.pos = pos.clamp(0.0, dur.max(0.0));
        self.last_seek_frame = i64::MIN;
    }

    /// Advance by `dt` seconds of clip time (already scaled by composition
    /// and clip speed). `frame` is the file's frame duration.
    pub fn advance(&mut self, dt: f64, dur: f64, s: &TransportSettings, frame: f64) -> Motion {
        if dur <= 0.0 {
            return Motion::Hold;
        }
        let frame = frame.max(1e-4);
        if s.mode != PlayMode::Bounce {
            self.reversed = false; // don't resume a stale bounce-back later
        }
        let sign = match s.direction {
            Direction::Pause => return Motion::Hold,
            Direction::Forward => 1.0,
            Direction::Backward => -1.0,
        };
        let d = dt * s.speed.max(0.0);
        if d <= 0.0 {
            return Motion::Hold;
        }
        // Last frame start: once-modes hold here.
        let last = (dur - frame * 0.5).max(0.0);
        match s.mode {
            PlayMode::Loop if sign > 0.0 => {
                self.pos += d;
                while self.pos >= dur {
                    self.pos -= dur;
                    self.lap += 1;
                }
                Motion::Forward
            }
            PlayMode::Loop => {
                self.pos -= d;
                while self.pos < 0.0 {
                    self.pos += dur;
                }
                self.seek_if_new_frame(frame)
            }
            PlayMode::Bounce => {
                let mut fwd = (sign > 0.0) != self.reversed;
                let mut p = self.pos + if fwd { d } else { -d };
                let mut turned = false;
                // Reflect at both ends (loop covers d > duration).
                while !(0.0..=last).contains(&p) {
                    turned = true;
                    if p > last {
                        p = 2.0 * last - p;
                    } else {
                        p = -p;
                    }
                    fwd = !fwd;
                    self.reversed = !self.reversed;
                }
                self.pos = p;
                if fwd && !turned {
                    Motion::Forward
                } else {
                    self.seek_if_new_frame(frame)
                }
            }
            PlayMode::Random => {
                self.random_acc += d;
                if self.random_acc < frame {
                    return Motion::Hold;
                }
                self.random_acc %= frame;
                self.pos = self.next_random() * last;
                self.seek_if_new_frame(frame)
            }
            PlayMode::OnceClear | PlayMode::OnceHold => {
                let p = self.pos + sign * d;
                let done = if sign > 0.0 { p > last } else { p < 0.0 };
                if done {
                    if s.mode == PlayMode::OnceClear {
                        return Motion::Ended;
                    }
                    self.pos = if sign > 0.0 { last } else { 0.0 };
                    return if sign > 0.0 {
                        Motion::Hold
                    } else {
                        self.seek_if_new_frame(frame)
                    };
                }
                self.pos = p;
                if sign > 0.0 {
                    Motion::Forward
                } else {
                    self.seek_if_new_frame(frame)
                }
            }
        }
    }

    /// SMPTE transport: put the playhead at clip time `t` (timecode − offset;
    /// before 0 the first frame shows). Loop / Random wrap, Bounce goes back
    /// and forth, the once-modes stop or hold at the end. Small steps forward
    /// keep the decoder reading ahead; tiny steps back (timecode jitter) hold.
    pub fn follow(&mut self, t: f64, dur: f64, mode: PlayMode, frame: f64) -> Motion {
        if dur <= 0.0 {
            return Motion::Hold;
        }
        let frame = frame.max(1e-4);
        let last = (dur - frame * 0.5).max(0.0);
        let t = t.max(0.0);
        let (lap, pos) = match mode {
            PlayMode::Loop | PlayMode::Random => {
                let lap = (t / dur).floor();
                (lap as i64, (t - lap * dur).clamp(0.0, dur))
            }
            PlayMode::Bounce => {
                let span = last.max(frame);
                let k = t.rem_euclid(2.0 * span);
                (self.lap, if k <= span { k } else { 2.0 * span - k })
            }
            PlayMode::OnceClear if t > last => return Motion::Ended,
            PlayMode::OnceClear | PlayMode::OnceHold => (self.lap, t.min(last)),
        };
        self.reversed = false;
        let d = (lap - self.lap) as f64 * dur + pos - self.pos;
        if (0.0..1.0).contains(&d) {
            self.lap = lap;
            self.pos = pos;
            return if d > 0.0 { Motion::Forward } else { Motion::Hold };
        }
        if d < 0.0 && d > -1.5 * frame {
            return Motion::Hold;
        }
        self.lap = lap;
        self.pos = pos;
        self.seek_if_new_frame(frame)
    }

    fn seek_if_new_frame(&mut self, frame: f64) -> Motion {
        let f = (self.pos / frame).floor() as i64;
        if f == self.last_seek_frame {
            Motion::Hold
        } else {
            self.last_seek_frame = f;
            Motion::Seek
        }
    }

    /// xorshift64*, 0..1.
    fn next_random(&mut self) -> f64 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: f64 = 0.1; // 10 fps
    fn s(mode: PlayMode, direction: Direction) -> TransportSettings {
        TransportSettings {
            mode,
            direction,
            ..Default::default()
        }
    }

    #[test]
    fn smpte_follow_reads_ahead_wraps_and_seeks() {
        let mut p = Playhead::at(0.0);
        // Running timecode: small forward steps, no seeks, laps like Loop.
        assert_eq!(p.follow(0.04, 1.0, PlayMode::Loop, F), Motion::Forward);
        assert_eq!(p.follow(0.98, 1.0, PlayMode::Loop, F), Motion::Forward);
        assert_eq!(p.follow(1.02, 1.0, PlayMode::Loop, F), Motion::Forward);
        assert_eq!((p.lap, (p.pos * 100.0).round()), (1, 2.0));
        // Jitter backwards: hold. Same spot: hold.
        assert_eq!(p.follow(1.0, 1.0, PlayMode::Loop, F), Motion::Hold);
        assert_eq!(p.follow(1.02, 1.0, PlayMode::Loop, F), Motion::Hold);
        // Locate far away: seek there.
        assert_eq!(p.follow(7.5, 1.0, PlayMode::Loop, F), Motion::Seek);
        assert_eq!(p.lap, 7);
        assert!((p.unwrapped(1.0) - 7.5).abs() < 1e-9);
        // Before the offset: first frame.
        assert_eq!(p.follow(-3.0, 1.0, PlayMode::Loop, F), Motion::Seek);
        assert_eq!((p.lap, p.pos), (0, 0.0));
        // Once & hold: stays on the last frame; once & clear: ends.
        let mut p = Playhead::at(0.0);
        p.follow(5.0, 1.0, PlayMode::OnceHold, F);
        assert!((p.pos - 0.95).abs() < 1e-9);
        assert_eq!(p.follow(5.0, 1.0, PlayMode::OnceClear, F), Motion::Ended);
        // Bounce: 1.2 s into a 1 s clip (last frame start 0.95) is on the way back.
        let mut p = Playhead::at(0.0);
        p.follow(1.2, 1.0, PlayMode::Bounce, F);
        assert!((p.pos - 0.7).abs() < 1e-9);
    }

    #[test]
    fn smpte_type_follows_the_transport_choice() {
        let mut ids = crate::IdGen::new();
        let mut c = crate::Clip::empty(&mut ids);
        c.transport = Some(Transport::Timeline(crate::TransportTimeline::new(&mut ids, 4.0)));
        // Type set directly (OSC / REST): follows with offset 0.
        c.transporttype.select(2);
        assert_eq!(
            TransportSettings::for_clip(&c, 120.0).smpte,
            Some(SmpteFollow { input: 0, offset: 0.0 })
        );
        c.transporttype.select(0);
        assert!(c.set_transport_type(&mut ids, 3, 120.0));
        if let Some(Transport::Timeline(t)) = &mut c.transport {
            t.controls.smpteoffset.as_mut().unwrap().set(60.0);
        }
        let s = TransportSettings::for_clip(&c, 120.0);
        assert_eq!(s.smpte, Some(SmpteFollow { input: 1, offset: 60.0 }));
        assert!(c.set_transport_type(&mut ids, 2, 120.0));
        assert_eq!(TransportSettings::for_clip(&c, 120.0).smpte.map(|f| f.input), Some(0));
        // Back to Timeline: offset kept for later, not followed.
        assert!(c.set_transport_type(&mut ids, 0, 120.0));
        assert_eq!(TransportSettings::for_clip(&c, 120.0).smpte, None);
        assert!(
            matches!(&c.transport, Some(Transport::Timeline(t)) if t.controls.smpteoffset.as_ref().unwrap().value == 60.0)
        );
        // BPM Sync → SMPTE makes a Timeline transport.
        assert!(c.set_transport_type(&mut ids, 1, 120.0));
        assert!(c.set_transport_type(&mut ids, 2, 120.0));
        assert!(matches!(&c.transport, Some(Transport::Timeline(_))));
        assert_eq!(c.transporttype.value, "SMPTE 1");
        // Denon / Pioneer aren't supported.
        assert!(!c.set_transport_type(&mut ids, 4, 120.0));
    }

    #[test]
    fn loop_forward_wraps_without_seek() {
        let mut p = Playhead::at(0.9);
        assert_eq!(
            p.advance(0.25, 1.0, &s(PlayMode::Loop, Direction::Forward), F),
            Motion::Forward
        );
        assert!((p.pos - 0.15).abs() < 1e-9);
        assert_eq!(p.lap, 1);
        assert!((p.unwrapped(1.0) - 1.15).abs() < 1e-9);
    }

    #[test]
    fn loop_backward_seeks_once_per_frame() {
        let st = s(PlayMode::Loop, Direction::Backward);
        let mut p = Playhead::at(0.05);
        assert_eq!(p.advance(0.1, 1.0, &st, F), Motion::Seek);
        assert!((p.pos - 0.95).abs() < 1e-9);
        assert_eq!(p.advance(0.01, 1.0, &st, F), Motion::Hold); // same frame 9
        assert_eq!(p.advance(0.1, 1.0, &st, F), Motion::Seek);
    }

    #[test]
    fn bounce_reflects() {
        let st = s(PlayMode::Bounce, Direction::Forward);
        let mut p = Playhead::at(0.8);
        assert_eq!(p.advance(0.05, 1.0, &st, F), Motion::Forward);
        // last = 0.95; 0.85 + 0.2 = 1.05 → 0.85 on the way back
        assert_eq!(p.advance(0.2, 1.0, &st, F), Motion::Seek);
        assert!(p.reversed);
        assert!((p.pos - 0.85).abs() < 1e-9);
        p.advance(0.9, 1.0, &st, F); // 0.85 - 0.9 = -0.05 → 0.05, forward again
        assert!(!p.reversed);
        assert!((p.pos - 0.05).abs() < 1e-9);
        assert_eq!(p.advance(0.1, 1.0, &st, F), Motion::Forward);
    }

    #[test]
    fn once_modes() {
        let mut p = Playhead::at(0.9);
        assert_eq!(
            p.advance(0.2, 1.0, &s(PlayMode::OnceClear, Direction::Forward), F),
            Motion::Ended
        );
        let mut p = Playhead::at(0.9);
        assert_eq!(
            p.advance(0.2, 1.0, &s(PlayMode::OnceHold, Direction::Forward), F),
            Motion::Hold
        );
        assert!((p.pos - 0.95).abs() < 1e-9);
        assert_eq!(
            p.advance(0.2, 1.0, &s(PlayMode::OnceHold, Direction::Forward), F),
            Motion::Hold
        );
        let mut p = Playhead::at(0.05);
        assert_eq!(
            p.advance(0.2, 1.0, &s(PlayMode::OnceClear, Direction::Backward), F),
            Motion::Ended
        );
    }

    #[test]
    fn pause_and_random() {
        let mut p = Playhead::at(0.5);
        assert_eq!(
            p.advance(1.0, 1.0, &s(PlayMode::Loop, Direction::Pause), F),
            Motion::Hold
        );
        assert_eq!(p.pos, 0.5);
        let st = s(PlayMode::Random, Direction::Forward);
        assert_eq!(p.advance(0.05, 1.0, &st, F), Motion::Hold); // less than a frame
        let mut seen = std::collections::HashSet::new();
        for _ in 0..50 {
            p.advance(0.1, 1.0, &st, F);
            assert!((0.0..=0.95).contains(&p.pos));
            seen.insert((p.pos * 10.0) as i64);
        }
        assert!(seen.len() > 5, "random spreads: {seen:?}");
    }

    #[test]
    fn bpm_sync_speed_follows_tempo() {
        let mut ids = crate::IdGen::new();
        let mut c = crate::Composition::new("t", 1, 1);
        let clip = &mut c.layers[0].clips[0];
        clip.load_video(
            &mut ids,
            "c",
            crate::VideoFileInfo {
                path: "x.mov".into(),
                exists: true,
                duration: String::new(),
                duration_ms: 3000.0,
                framerate: crate::FrameRate { num: 30, denom: 1 },
                width: 4,
                height: 4,
            },
        );
        // 3 s at 120 BPM = 6 beats → nearest power of two: 8 beats (4 s).
        assert!(clip.set_transport_type(&mut ids, 1, 120.0));
        let Some(Transport::BpmSync(t)) = &clip.transport else {
            panic!()
        };
        assert_eq!(t.controls.duration.value, 8.0);
        let s = TransportSettings::for_clip(clip, 120.0);
        assert!((s.speed - 0.75).abs() < 1e-9, "{}", s.speed);
        // Double tempo: double speed.
        assert!((TransportSettings::for_clip(clip, 240.0).speed - 1.5).abs() < 1e-9);
        // Back to Timeline: normal speed, modes kept.
        assert!(clip.set_transport_type(&mut ids, 0, 120.0));
        assert_eq!(TransportSettings::for_clip(clip, 240.0).speed, 1.0);
        assert_eq!(clip.transporttype.value, "Timeline");
        // JSON round trip keeps BPM Sync (untagged enum).
        clip.set_transport_type(&mut ids, 1, 120.0);
        let json = serde_json::to_string(&*clip).unwrap();
        let back: crate::Clip = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.transport, Some(Transport::BpmSync(_))));
    }

    #[test]
    fn timeline_duration_scales_speed() {
        let mut ids = crate::IdGen::new();
        let mut clip = crate::Clip::empty(&mut ids);
        clip.load_video(
            &mut ids,
            "c",
            crate::VideoFileInfo {
                path: "x.mov".into(),
                exists: true,
                duration: String::new(),
                duration_ms: 4000.0,
                framerate: crate::FrameRate { num: 30, denom: 1 },
                width: 4,
                height: 4,
            },
        );
        // New clip: Duration = file length, normal speed.
        assert_eq!(TransportSettings::for_clip(&clip, 120.0).speed, 1.0);
        // Duration 1 s (÷2 ÷2) at speed 0.5: 4 s file in 1 s, halved = ×2.
        if let Some(Transport::Timeline(t)) = &mut clip.transport {
            t.controls.duration.set(1.0);
            t.controls.speed.set(0.5);
        }
        assert!((TransportSettings::for_clip(&clip, 120.0).speed - 2.0).abs() < 1e-9);
    }

    #[test]
    fn settings_from_params() {
        let mut ids = crate::IdGen::new();
        let mut t = crate::TransportTimeline::new(&mut ids, 2.0);
        t.controls.playmode.select(1);
        t.controls.playdirection.select(0);
        t.controls.playmodeaway.select_value("Restart");
        t.controls.speed.set(2.0);
        let st = TransportSettings::of(Some(&Transport::Timeline(t)));
        assert_eq!(
            TransportSettings { smpte: None, ..st },
            TransportSettings {
                mode: PlayMode::Bounce,
                direction: Direction::Backward,
                speed: 2.0,
                continue_away: false,
                smpte: None
            }
        );
    }
}
