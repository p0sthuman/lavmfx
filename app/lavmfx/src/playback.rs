//! Clip playback: one [`Playback`] per layer, opened off the UI thread.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

use lavmfx_core::Id;
use lavmfx_core::transport::{Motion, PlayMode, Playhead, TransportSettings};
use lavmfx_media::{MediaInfo, OpenOptions, Player, VideoFrame};

/// Where a playing clip lives in the composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub deck: usize,
    pub layer: usize,
    pub col: usize,
    pub clip: Id,
}

pub struct Playback {
    pub slot: Slot,
    /// The decoder; None for a source / effect clip (no file: only the playhead runs).
    pub player: Option<Player>,
    /// Length of a clip without a file.
    virtual_secs: f64,
    pub head: Playhead,
    /// Read from the clip's transport params every tick.
    pub settings: TransportSettings,
    /// Slider is being dragged: hold position.
    pub scrubbing: bool,
    /// SMPTE transport: clip time from the timecode (timecode − offset),
    /// set by the caller each tick; None = no timecode (hold).
    pub smpte_time: Option<f64>,
    /// Clip seconds played since the trigger, in any direction (autopilot:
    /// moves on after loops × duration).
    pub played: f64,
    shown: RateCounter,
    /// Live clip (NDI / shared video): what it reads, its receiver (None
    /// until connected; retried every 2 s) and the last frame number shown.
    pub live_src: Option<lavmfx_core::sources::ClipSource>,
    pub live: Option<Arc<lavmfx_media::live::Receiver>>,
    pub live_tried: Option<Instant>,
    live_seen: u64,
}

pub enum Tick {
    Frame(Arc<VideoFrame>),
    Nothing,
    /// Play-once-and-clear finished.
    Ended,
}

impl Playback {
    pub fn new(player: Player, slot: Slot, start: f64) -> Self {
        let mut head = Playhead::at(0.0);
        head.jump(start, player.duration());
        let mut pb = Self {
            slot,
            player: Some(player),
            virtual_secs: 0.0,
            head,
            settings: TransportSettings::default(),
            scrubbing: false,
            smpte_time: None,
            played: 0.0,
            shown: RateCounter::default(),
            live_src: None,
            live: None,
            live_tried: None,
            live_seen: 0,
        };
        let u = pb.head.unwrapped(pb.duration());
        if u > 0.0
            && let Some(p) = &mut pb.player
        {
            p.seek(u);
        }
        pb
    }

    /// A source / effect clip: a playhead over `secs` without a decoder.
    pub fn without_file(slot: Slot, secs: f64, start: f64) -> Self {
        let mut head = Playhead::at(0.0);
        head.jump(start, secs);
        Self {
            slot,
            player: None,
            virtual_secs: secs.max(0.01),
            head,
            settings: TransportSettings::default(),
            scrubbing: false,
            smpte_time: None,
            played: 0.0,
            shown: RateCounter::default(),
            live_src: None,
            live: None,
            live_tried: None,
            live_seen: 0,
        }
    }

    /// Media info of the file (None: a source / effect clip).
    pub fn info(&self) -> Option<&MediaInfo> {
        self.player.as_ref().map(Player::info)
    }

    pub fn duration(&self) -> f64 {
        self.player.as_ref().map_or(self.virtual_secs, Player::duration)
    }

    /// Frame duration (sources run at 60 fps).
    fn frame_secs(&self) -> f64 {
        1.0 / self.info().map_or(60.0, |i| i.fps())
    }

    /// Position within the file, 0..duration.
    pub fn offset(&self) -> f64 {
        self.head.pos
    }

    /// Advance by `dt` seconds of composition time.
    pub fn tick(&mut self, dt: f64) -> Tick {
        let dur = self.duration();
        // Random: wait for the last jump's frame before the next jump, or a
        // slow (long-GOP) decoder never finishes one.
        let waiting = self.settings.mode == PlayMode::Random && self.player.as_ref().is_some_and(Player::seek_pending);
        if !self.scrubbing && !waiting {
            if self.settings.smpte.is_none() && self.settings.direction != lavmfx_core::transport::Direction::Pause {
                self.played += dt * self.settings.speed.max(0.0);
            }
            let frame = self.frame_secs();
            let motion = match (self.settings.smpte, self.smpte_time) {
                (None, _) => self.head.advance(dt, dur, &self.settings, frame),
                (Some(_), Some(t)) => self.head.follow(t, dur, self.settings.mode, frame),
                (Some(_), None) => Motion::Hold,
            };
            let at = self.head.unwrapped(dur);
            match (motion, &mut self.player) {
                (Motion::Ended, _) => return Tick::Ended,
                // Random jumps: drop frames of earlier jumps. Backward/bounce
                // steps: an older nearby frame beats showing nothing.
                (Motion::Seek, Some(p)) if self.settings.mode == PlayMode::Random => p.seek(at),
                (Motion::Seek, Some(p)) => p.seek_soft(at),
                _ => {}
            }
        }
        if self.live_src.is_some() {
            // Live: the newest frame, whatever the playhead does.
            let Some(r) = &self.live else { return Tick::Nothing };
            return match r.latest(&mut self.live_seen) {
                Some(f) => {
                    self.shown.tick();
                    Tick::Frame(f)
                }
                None => Tick::Nothing,
            };
        }
        let Some(player) = &mut self.player else {
            self.shown.tick();
            return Tick::Nothing;
        };
        match player.update(self.head.unwrapped(dur)) {
            Some(f) => {
                self.shown.tick();
                Tick::Frame(Arc::new(f))
            }
            None => Tick::Nothing,
        }
    }

    /// Jump to `offset` seconds within the file.
    pub fn scrub_to(&mut self, offset: f64) {
        let dur = self.duration();
        self.head.jump(offset, dur);
        let at = self.head.unwrapped(dur);
        if let Some(p) = &mut self.player {
            p.seek(at);
        }
    }

    /// Step `n` frames (negative = back). The caller pauses the clip.
    pub fn step(&mut self, n: i32) {
        let fd = self.frame_secs();
        // Snap to the frame grid so repeated steps don't drift.
        let f = ((self.head.pos / fd).round() + n as f64).max(0.0);
        let last = ((self.duration() / fd).ceil() - 1.0).max(0.0);
        self.scrub_to(f.min(last) * fd);
    }

    /// Connect a live clip's receiver (now, then every 2 s until it works).
    /// Some(error) the first time a try fails.
    pub fn connect_live(&mut self) -> Option<String> {
        let src = self.live_src.as_ref()?;
        if self.live.is_some() || self.live_tried.is_some_and(|t| t.elapsed().as_secs_f32() < 2.0) {
            return None;
        }
        let first = self.live_tried.is_none();
        self.live_tried = Some(Instant::now());
        let r = match src {
            lavmfx_core::sources::ClipSource::Ndi(n) => lavmfx_media::ndi::receiver(n),
            lavmfx_core::sources::ClipSource::Share(n) => lavmfx_media::share::receiver(n),
            lavmfx_core::sources::ClipSource::Capture(d) => lavmfx_media::capture::receiver(d),
            _ => return None,
        };
        match r {
            Ok(r) => {
                self.live = Some(r);
                None
            }
            Err(e) => first.then_some(e),
        }
    }

    /// Frames per second actually presented.
    pub fn shown_fps(&self) -> f32 {
        self.shown.rate
    }
}

/// A clip being opened on a worker thread (decoder + hw device setup can
/// take a while; the layer keeps playing its old clip meanwhile).
pub struct Opening {
    pub slot: Slot,
    pub start: f64,
    pub path: PathBuf,
    rx: mpsc::Receiver<lavmfx_media::Result<Player>>,
}

impl Opening {
    pub fn spawn(path: PathBuf, slot: Slot, start: f64, native_bc: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Player::open(
                &p,
                OpenOptions {
                    native_bc,
                    ..Default::default()
                },
            ));
        });
        Self { slot, start, path, rx }
    }

    /// `Some` once the open finished (or the worker died).
    pub fn poll(&self) -> Option<lavmfx_media::Result<Player>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(lavmfx_media::MediaError::Decode("open worker exited".into())))
            }
        }
    }
}

#[derive(Default)]
struct RateCounter {
    since: Option<Instant>,
    n: u32,
    rate: f32,
}

impl RateCounter {
    fn tick(&mut self) {
        let now = Instant::now();
        let since = *self.since.get_or_insert(now);
        self.n += 1;
        let dt = now.duration_since(since).as_secs_f32();
        if dt >= 1.0 {
            self.rate = self.n as f32 / dt;
            self.n = 0;
            self.since = Some(now);
        }
    }
}

pub fn fmt_time(s: f64) -> String {
    let s = s.max(0.0);
    format!("{:02}:{:05.2}", (s / 60.0).floor() as u64, s % 60.0)
}

pub fn file_info(info: &MediaInfo) -> lavmfx_core::VideoFileInfo {
    lavmfx_core::VideoFileInfo {
        path: info.path.to_string_lossy().into_owned(),
        exists: true,
        duration: fmt_time(info.duration.as_secs_f64()),
        duration_ms: info.duration.as_secs_f64() * 1000.0,
        framerate: lavmfx_core::FrameRate {
            num: info.frame_rate.0,
            denom: info.frame_rate.1,
        },
        width: info.width as i32,
        height: info.height as i32,
    }
}

/// A clip leaving its layer during a transition (`None` = fading in from
/// nothing). It keeps playing until the transition ends.
pub struct Outgoing {
    pub pb: Option<Playback>,
    /// Composition seconds since the transition started.
    pub elapsed: f64,
    pub duration: f64,
}

impl Outgoing {
    pub fn progress(&self) -> f32 {
        (self.elapsed / self.duration.max(1e-6)).clamp(0.0, 1.0) as f32
    }
}

#[cfg(test)]
mod sim {
    //! Diagnostic: `LAVMFX_SIM_FILE=clip.mov cargo test -p lavmfx --release sim -- --ignored --nocapture`
    #[allow(unused_imports)]
    use super::Motion as _;
    use super::*;
    use lavmfx_core::transport::{Direction, PlayMode};

    #[test]
    #[ignore]
    fn transport_modes_on_real_file() {
        let Ok(path) = std::env::var("LAVMFX_SIM_FILE") else {
            return;
        };
        let opts = lavmfx_media::OpenOptions {
            hwaccel: lavmfx_media::HwAccel::None,
            native_bc: false,
        };
        let slot = Slot {
            deck: 0,
            layer: 0,
            col: 0,
            clip: 1,
        };
        for (mode, dir) in [
            (PlayMode::Loop, Direction::Forward),
            (PlayMode::Loop, Direction::Backward),
            (PlayMode::Bounce, Direction::Forward),
            (PlayMode::Random, Direction::Forward),
            (PlayMode::OnceHold, Direction::Forward),
            (PlayMode::OnceHold, Direction::Backward),
        ] {
            let player = Player::open(std::path::Path::new(&path), opts).unwrap();
            let mut pb = Playback::new(player, slot, 0.0);
            let dur = pb.duration();
            if dir == Direction::Backward {
                pb.scrub_to(dur * 0.5);
            }
            pb.settings = TransportSettings {
                mode,
                direction: dir,
                speed: 3.0,
                continue_away: false,
                smpte: None,
            };
            let fd = 1.0 / pb.info().unwrap().fps();
            let t0 = std::time::Instant::now();
            let mut last = t0;
            let (mut frames, mut worst, mut far) = (0, 0.0f64, 0);
            let mut trace = Vec::new();
            while t0.elapsed().as_secs_f64() < 4.0 {
                let now = std::time::Instant::now();
                let dt = (now - last).as_secs_f64();
                last = now;
                if let Tick::Frame(f) = pb.tick(dt) {
                    frames += 1;
                    let err = (f.pts.as_secs_f64() - pb.offset()).abs();
                    let err = err.min(dur - err); // wrap
                    worst = worst.max(err);
                    if err > 3.0 * fd {
                        far += 1;
                    }
                    if trace.len() < 400 {
                        trace.push(format!("{:.2}/{:.2}", pb.offset(), f.pts.as_secs_f64()));
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(8));
            }
            eprintln!(
                "{mode:?} {dir:?}: {frames} frames, {far} more than 3 frames off, worst {:.2}s, end pos {:.2}/{dur:.2}",
                worst,
                pb.offset()
            );
            if far > 0 {
                eprintln!("  pos/pts: {}", trace.join(" "));
            }
        }
    }

    #[test]
    #[ignore]
    fn transport_switches_on_real_file() {
        let Ok(path) = std::env::var("LAVMFX_SIM_FILE") else {
            return;
        };
        let opts = lavmfx_media::OpenOptions {
            hwaccel: lavmfx_media::HwAccel::None,
            native_bc: false,
        };
        let slot = Slot {
            deck: 0,
            layer: 0,
            col: 0,
            clip: 1,
        };
        let player = Player::open(std::path::Path::new(&path), opts).unwrap();
        let mut pb = Playback::new(player, slot, 0.0);
        let dur = pb.duration();
        let fd = 1.0 / pb.info().unwrap().fps();
        let steps = [
            (PlayMode::Loop, Direction::Forward),
            (PlayMode::Loop, Direction::Backward),
            (PlayMode::Loop, Direction::Forward),
            (PlayMode::Bounce, Direction::Forward),
            (PlayMode::Bounce, Direction::Backward),
            (PlayMode::Loop, Direction::Pause),
            (PlayMode::Loop, Direction::Forward),
            (PlayMode::OnceHold, Direction::Forward),
            (PlayMode::OnceHold, Direction::Backward),
            (PlayMode::Loop, Direction::Forward),
        ];
        for (mode, dir) in steps {
            pb.settings = TransportSettings {
                mode,
                direction: dir,
                speed: 4.0,
                continue_away: false,
                smpte: None,
            };
            let t0 = std::time::Instant::now();
            let mut last = t0;
            let (mut frames, mut far, mut worst) = (0, 0, 0.0f64);
            let start = pb.offset();
            while t0.elapsed().as_secs_f64() < 1.5 {
                let now = std::time::Instant::now();
                let dt = (now - last).as_secs_f64();
                last = now;
                if let Tick::Frame(f) = pb.tick(dt) {
                    frames += 1;
                    let e = (f.pts.as_secs_f64() - pb.offset()).abs();
                    let e = e.min(dur - e);
                    worst = worst.max(e);
                    if e > 3.0 * fd {
                        far += 1;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(8));
            }
            eprintln!(
                "{mode:?} {dir:?}: pos {start:.2}->{:.2} lap {} rev {}: {frames} frames, {far} off, worst {worst:.2}s",
                pb.offset(),
                pb.head.lap,
                pb.head.reversed
            );
        }
    }

    #[test]
    fn clip_without_file_runs_its_playhead() {
        let slot = Slot {
            deck: 0,
            layer: 0,
            col: 0,
            clip: 1,
        };
        let mut pb = Playback::without_file(slot, 5.0, 0.0);
        pb.settings.mode = PlayMode::Loop;
        assert!(pb.info().is_none());
        assert_eq!(pb.duration(), 5.0);
        assert!(matches!(pb.tick(1.5), Tick::Nothing));
        assert!((pb.offset() - 1.5).abs() < 1e-6);
        pb.tick(4.0); // loops
        assert!((pb.offset() - 0.5).abs() < 1e-6, "{}", pb.offset());
        pb.step(3);
        assert!((pb.offset() - (0.5 + 3.0 / 60.0)).abs() < 1e-3);
    }

    #[test]
    fn smpte_clip_follows_timecode_not_wall_time() {
        use lavmfx_core::transport::SmpteFollow;
        let slot = Slot {
            deck: 0,
            layer: 0,
            col: 0,
            clip: 1,
        };
        let mut pb = Playback::without_file(slot, 5.0, 0.0);
        pb.settings.smpte = Some(SmpteFollow {
            input: 0,
            offset: 100.0,
        });
        // No timecode: holds however much time passes.
        pb.tick(1.0);
        assert_eq!(pb.offset(), 0.0);
        pb.smpte_time = Some(2.25);
        pb.tick(10.0);
        assert!((pb.offset() - 2.25).abs() < 1e-9);
        pb.smpte_time = Some(7.5); // loops at 5 s
        pb.tick(0.0);
        assert!((pb.offset() - 2.5).abs() < 1e-9, "{}", pb.offset());
    }
}
