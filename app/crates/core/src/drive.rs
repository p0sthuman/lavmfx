//! Parameter drive: the P menu. A range param can follow a phase source
//! (Timeline, BPM Sync, a Dashboard link, the layer's transition, the
//! crossfader) between two handles: `value = from + phase × (to − from)`.
//!
//! Stored on the param as the non-REST `drive` field. Runtime state (phase,
//! bounce direction) isn't saved. FFT sources follow an audio spectrum
//! (External = audio input, Composition = the mix, Layer = the layer's audio).

use serde::{Deserialize, Serialize};

use crate::fft::{self, FftBand, Spectrum};
use crate::id::IdGen;
use crate::model::{AudioTrack, Composition, Transport, VideoEffect};
use crate::param::{Param, ParameterCollection, RangeParam};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Source {
    /// Not animated (the slider sets the value).
    #[default]
    Basic,
    Timeline,
    BpmSync,
    Dashboard,
    TransitionPhase,
    CrossFaderPhase,
    ExternalFft,
    CompositionFft,
    LayerFft,
}

impl Source {
    /// Menu order and names.
    pub const ALL: [(Source, &'static str); 9] = [
        (Source::Basic, "Basic"),
        (Source::Timeline, "Timeline"),
        (Source::BpmSync, "BPM Sync"),
        (Source::Dashboard, "Dashboard"),
        (Source::TransitionPhase, "Transition Phase"),
        (Source::CrossFaderPhase, "CrossFader Phase"),
        (Source::ExternalFft, "External FFT"),
        (Source::CompositionFft, "Composition FFT"),
        (Source::LayerFft, "Layer FFT"),
    ];
    pub fn is_fft(self) -> bool {
        matches!(self, Source::ExternalFft | Source::CompositionFft | Source::LayerFft)
    }
    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(s, _)| *s == self).map_or("?", |(_, n)| n)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Direction {
    Backward,
    Pause,
    #[default]
    Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PlayMode {
    #[default]
    Loop,
    Bounce,
    Once,
    Random,
}

impl PlayMode {
    pub const ALL: [(PlayMode, &'static str); 4] = [
        (PlayMode::Loop, "Loop"),
        (PlayMode::Bounce, "Bounce"),
        (PlayMode::Once, "Once"),
        (PlayMode::Random, "Random"),
    ];
}

/// Number of dashboard link knobs per owner.
pub const LINKS: usize = 8;

fn default_seconds() -> f64 {
    4.0
}
fn default_beats() -> f64 {
    4.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Drive {
    pub source: Source,
    /// Value at phase 0 (param units).
    pub from: f64,
    /// Value at phase 1 (param units).
    pub to: f64,
    /// Value before the drive started; restored by Basic.
    pub base: f64,
    /// Timeline duration of one pass.
    #[serde(default = "default_seconds")]
    pub seconds: f64,
    /// BPM Sync length of one pass.
    #[serde(default = "default_beats")]
    pub beats: f64,
    #[serde(default)]
    pub direction: Direction,
    #[serde(default)]
    pub mode: PlayMode,
    /// Dashboard link 0..LINKS.
    #[serde(default)]
    pub link: usize,
    /// Optional curve from phase to output (P › Envelope).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope: Option<Envelope>,
    /// FFT sources: band range, gain, fall.
    #[serde(default)]
    pub fft: FftBand,
    #[serde(default, rename = "rt_phase", skip_serializing_if = "crate::control::skip_runtime")]
    pub phase: f64,
    /// FFT sources: the spectrum read last frame (for the drive's display).
    #[serde(skip)]
    pub seen: Spectrum,
    #[serde(default, rename = "rt_back", skip_serializing_if = "crate::control::skip_runtime")]
    back: bool,
    #[serde(default, rename = "rt_acc", skip_serializing_if = "crate::control::skip_runtime")]
    acc: f64,
    #[serde(
        default,
        rename = "rt_last_beats",
        skip_serializing_if = "crate::control::skip_runtime"
    )]
    last_beats: Option<f64>,
    #[serde(default, rename = "rt_seed", skip_serializing_if = "crate::control::skip_runtime")]
    seed: u64,
}

/// What a param's drive can read this frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct Inputs {
    /// Seconds since the last frame.
    pub dt: f64,
    /// Composition beats (clock).
    pub beats: f64,
    /// Dashboard links of the param's owner, 0..1.
    pub links: [f64; LINKS],
    /// Transition progress of the param's layer (0 when idle).
    pub transition: f64,
    /// Crossfader phase mapped to 0 (A) … 1 (B).
    pub crossfader: f64,
    /// Audio input spectrum (External FFT).
    pub external: Spectrum,
    /// Composition audio spectrum.
    pub composition: Spectrum,
    /// Spectrum of the param's layer (the composition's above layer level).
    pub layer: Spectrum,
}

impl Drive {
    /// Start driving `p` from `source`. Default range: the whole param range,
    /// or ±10 % of the value's neighbourhood for huge ranges (pixel spinners).
    pub fn new(p: &RangeParam, source: Source) -> Self {
        let (from, to) = if p.max - p.min > 10_000.0 {
            (p.value, (p.value + 100.0).min(p.max))
        } else {
            (p.min, p.max)
        };
        Self {
            source,
            from,
            to,
            base: p.value,
            seconds: default_seconds(),
            beats: default_beats(),
            direction: Direction::Forward,
            mode: PlayMode::Loop,
            link: 0,
            envelope: None,
            fft: FftBand::default(),
            phase: 0.0,
            seen: fft::SILENT,
            back: false,
            acc: 0.0,
            last_beats: None,
            seed: (p.id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
        }
    }

    /// Back to the start (Once replays).
    pub fn restart(&mut self) {
        self.phase = if self.direction == Direction::Backward {
            1.0
        } else {
            0.0
        };
        self.back = false;
        self.acc = 0.0;
        self.last_beats = None;
    }

    fn random(&mut self) -> f64 {
        // xorshift64*
        let mut x = self.seed.max(1);
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.seed = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Advance by `d` passes (signed by direction) in the current play mode.
    fn advance(&mut self, d: f64) {
        let sign = match self.direction {
            Direction::Backward => -1.0,
            Direction::Pause => 0.0,
            Direction::Forward => 1.0,
        };
        match self.mode {
            PlayMode::Loop => self.phase = (self.phase + sign * d).rem_euclid(1.0),
            PlayMode::Once => self.phase = (self.phase + sign * d).clamp(0.0, 1.0),
            PlayMode::Bounce => {
                let mut p = self.phase + if self.back { -sign * d } else { sign * d };
                // Reflect (also for big steps).
                for _ in 0..4 {
                    if p > 1.0 {
                        p = 2.0 - p;
                        self.back = !self.back;
                    } else if p < 0.0 {
                        p = -p;
                        self.back = !self.back;
                    }
                }
                self.phase = p.clamp(0.0, 1.0);
            }
            // A new random value every frame (shakes and jitters); Pause holds.
            PlayMode::Random => {
                if sign != 0.0 && d > 0.0 {
                    self.phase = self.random();
                }
            }
        }
    }

    /// This frame's phase (0..1), or None when not animated.
    pub fn step(&mut self, i: &Inputs) -> Option<f64> {
        match self.source {
            Source::Basic => return None,
            Source::Timeline => self.advance(i.dt / self.seconds.max(1e-3)),
            Source::BpmSync => {
                let len = self.beats.max(1e-3);
                match self.mode {
                    // Loop and Bounce lock to the beat grid.
                    PlayMode::Loop | PlayMode::Bounce if self.direction != Direction::Pause => {
                        let t = i.beats / len;
                        let p = if self.mode == PlayMode::Loop {
                            t.rem_euclid(1.0)
                        } else {
                            1.0 - ((t * 0.5).rem_euclid(1.0) * 2.0 - 1.0).abs()
                        };
                        self.phase = if self.direction == Direction::Backward {
                            1.0 - p
                        } else {
                            p
                        };
                    }
                    _ => {
                        let db = self.last_beats.map_or(0.0, |b| (i.beats - b).max(0.0));
                        self.advance(db / len);
                    }
                }
                self.last_beats = Some(i.beats);
            }
            Source::Dashboard => self.phase = i.links[self.link.min(LINKS - 1)],
            Source::TransitionPhase => self.phase = i.transition,
            Source::CrossFaderPhase => self.phase = i.crossfader,
            Source::ExternalFft | Source::CompositionFft | Source::LayerFft => {
                self.seen = match self.source {
                    Source::ExternalFft => i.external,
                    Source::CompositionFft => i.composition,
                    _ => i.layer,
                };
                let target = self.fft.level(&self.seen);
                self.phase = self.fft.follow(self.phase, target, i.dt);
            }
        }
        Some(self.phase.clamp(0.0, 1.0))
    }

    /// Output 0..1 for the current phase (through the envelope, if on).
    pub fn output(&self) -> f64 {
        let p = self.phase.clamp(0.0, 1.0);
        self.envelope.as_ref().map_or(p, |e| e.eval(p))
    }

    pub fn value(&self) -> f64 {
        self.from + self.output() * (self.to - self.from)
    }
}

impl RangeParam {
    /// Animated (a drive with a source other than Basic).
    pub fn is_driven(&self) -> bool {
        self.drive.as_ref().is_some_and(|d| d.source != Source::Basic)
    }

    /// Choose the drive source (P menu). Basic restores the value from before.
    pub fn set_source(&mut self, source: Source) {
        match (&mut self.drive, source) {
            (None, Source::Basic) => {}
            (None, s) => self.drive = Some(Box::new(Drive::new(self, s))),
            (Some(d), Source::Basic) => {
                let base = d.base;
                d.source = Source::Basic;
                self.set(base);
            }
            (Some(d), s) => {
                if d.source == Source::Basic {
                    d.base = self.value;
                }
                d.source = s;
                d.restart();
            }
        }
    }

    /// Apply the drive for this frame.
    pub fn drive_step(&mut self, i: &Inputs) {
        if let Some(d) = &mut self.drive
            && d.step(i).is_some()
        {
            let v = d.value();
            self.set(v);
        }
    }
}

// --------------------------------------------------------------- envelope

/// Shape of the segment that starts at a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Curve {
    #[default]
    Linear,
    Smooth,
    EaseIn,
    EaseOut,
    /// Keep this point's value until the next point.
    Hold,
}

impl Curve {
    pub const ALL: [(Curve, &'static str); 5] = [
        (Curve::Linear, "Linear"),
        (Curve::Smooth, "Smooth"),
        (Curve::EaseIn, "Ease In"),
        (Curve::EaseOut, "Ease Out"),
        (Curve::Hold, "Hold"),
    ];
    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(c, _)| *c == self).map_or("?", |(_, n)| n)
    }
    fn shape(self, t: f64) -> f64 {
        match self {
            Curve::Linear => t,
            Curve::Smooth => t * t * (3.0 - 2.0 * t),
            Curve::EaseIn => t * t,
            Curve::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Curve::Hold => 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EnvPoint {
    /// Phase 0..1.
    pub x: f64,
    /// Output 0..1 (0 = `from`, 1 = `to`).
    pub y: f64,
    /// Shape towards the next point.
    #[serde(default)]
    pub curve: Curve,
}

/// Phase → output curve. Points are sorted by `x`; the first is at 0 and the
/// last at 1 (they can't be removed or moved sideways).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub points: Vec<EnvPoint>,
}

impl Default for Envelope {
    /// Identity (0,0) → (1,1).
    fn default() -> Self {
        let p = |x, y| EnvPoint {
            x,
            y,
            curve: Curve::Linear,
        };
        Self {
            points: vec![p(0.0, 0.0), p(1.0, 1.0)],
        }
    }
}

impl Envelope {
    pub fn eval(&self, x: f64) -> f64 {
        let pts = &self.points;
        let Some(first) = pts.first() else { return x };
        if x <= first.x {
            return first.y;
        }
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            if x <= b.x {
                let span = b.x - a.x;
                let t = if span > 1e-12 { (x - a.x) / span } else { 1.0 };
                return a.y + (b.y - a.y) * a.curve.shape(t.clamp(0.0, 1.0));
            }
        }
        pts.last().map_or(x, |p| p.y)
    }

    /// Insert a point (keeps order); returns its index.
    pub fn insert(&mut self, x: f64, y: f64) -> usize {
        let x = x.clamp(0.0, 1.0);
        let i = self.points.iter().position(|p| p.x > x).unwrap_or(self.points.len());
        let curve = i.checked_sub(1).map_or(Curve::Linear, |j| self.points[j].curve);
        self.points.insert(
            i,
            EnvPoint {
                x,
                y: y.clamp(0.0, 1.0),
                curve,
            },
        );
        i
    }

    /// Remove an inner point (the ends stay).
    pub fn remove(&mut self, i: usize) -> bool {
        if i == 0 || i + 1 >= self.points.len() {
            return false;
        }
        self.points.remove(i);
        true
    }

    /// Move point `i`; ends keep their x, inner points stay between neighbours.
    pub fn move_point(&mut self, i: usize, x: f64, y: f64) {
        let n = self.points.len();
        if i >= n {
            return;
        }
        let x = if i == 0 {
            0.0
        } else if i + 1 == n {
            1.0
        } else {
            x.clamp(self.points[i - 1].x, self.points[i + 1].x)
        };
        self.points[i].x = x;
        self.points[i].y = y.clamp(0.0, 1.0);
    }
}

// ------------------------------------------------------------- dashboards

/// Key of dashboard link `i` (0-based) — "Link 1" … "Link 8".
pub fn link_key(i: usize) -> String {
    format!("Link {}", i + 1)
}

/// A dashboard knob's label: its name (`altName` in `.avc`), else "Link n".
pub fn link_label(dash: &ParameterCollection, i: usize) -> String {
    match dash.get(&link_key(i)) {
        Some(Param::Range(r)) => r
            .view
            .as_ref()
            .and_then(|v| v.alternative_name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| link_key(i)),
        _ => link_key(i),
    }
}

/// Make sure the 8 link knobs exist (0..1, at 0).
pub fn ensure_links(dash: &mut ParameterCollection, ids: &mut IdGen) {
    for i in 0..LINKS {
        dash.entry(link_key(i))
            .or_insert_with(|| Param::Range(RangeParam::new(ids.alloc(), 0.0, 1.0, 0.0)));
    }
}

fn links(dash: &ParameterCollection) -> [f64; LINKS] {
    let mut l = [0.0; LINKS];
    for (i, v) in l.iter_mut().enumerate() {
        if let Some(Param::Range(r)) = dash.get(&link_key(i)) {
            *v = r.normalized();
        }
    }
    l
}

// ------------------------------------------------------------- evaluation

/// Per-frame inputs from the app.
#[derive(Debug, Clone)]
pub struct DriveFrame {
    pub dt: f64,
    pub beats: f64,
    /// Transition progress per layer (0 when idle).
    pub transitions: Vec<f64>,
    /// Audio input spectrum.
    pub external: Spectrum,
    /// Composition audio spectrum.
    pub composition: Spectrum,
    /// Audio spectrum per layer (missing = silent).
    pub layers: Vec<Spectrum>,
}

impl Default for DriveFrame {
    fn default() -> Self {
        Self {
            dt: 0.0,
            beats: 0.0,
            transitions: Vec::new(),
            external: fft::SILENT,
            composition: fft::SILENT,
            layers: Vec::new(),
        }
    }
}

fn drive_coll(c: &mut ParameterCollection, i: &Inputs) {
    for p in c.values_mut() {
        if let Param::Range(r) = p {
            r.drive_step(i);
        }
    }
}

fn drive_audio(a: Option<&mut AudioTrack>, i: &Inputs) {
    if let Some(a) = a {
        a.volume.drive_step(i);
        a.pan.drive_step(i);
        for e in &mut a.effects {
            drive_coll(&mut e.params, i);
        }
    }
}

fn drive_fx(fx: &mut [VideoEffect], i: &Inputs) {
    for e in fx {
        drive_coll(&mut e.params, i);
    }
}

impl Composition {
    /// Advance every driven param one frame.
    pub fn drive(&mut self, f: &DriveFrame) {
        let base = Inputs {
            dt: f.dt,
            beats: f.beats,
            crossfader: (self.crossfader.phase.value + 1.0) * 0.5,
            external: f.external,
            composition: f.composition,
            layer: f.composition,
            ..Default::default()
        };
        // Dashboards drive params too (they can be driven themselves: first).
        let i = Inputs {
            links: links(&self.dashboard),
            ..base
        };
        drive_coll(&mut self.dashboard, &i);
        let i = Inputs {
            links: links(&self.dashboard),
            ..base
        };
        self.master.drive_step(&i);
        self.speed.drive_step(&i);
        self.crossfader.phase.drive_step(&i);
        drive_audio(self.audio.as_mut(), &i);
        if let Some(v) = &mut self.video {
            v.opacity.drive_step(&i);
            drive_fx(&mut v.effects, &i);
        }
        for g in &mut self.layergroups {
            let i = Inputs {
                links: links(&g.dashboard),
                ..base
            };
            drive_coll(&mut g.dashboard, &i);
            g.master.drive_step(&i);
            drive_audio(g.audio.as_mut(), &i);
            if let Some(v) = &mut g.video {
                v.opacity.drive_step(&i);
                drive_fx(&mut v.effects, &i);
            }
        }
        for (li, l) in self.layers.iter_mut().enumerate() {
            let transition = f.transitions.get(li).copied().unwrap_or(0.0);
            let layer = f.layers.get(li).copied().unwrap_or(fft::SILENT);
            let i = Inputs {
                links: links(&l.dashboard),
                transition,
                layer,
                ..base
            };
            drive_coll(&mut l.dashboard, &i);
            l.master.drive_step(&i);
            l.transition.duration.drive_step(&i);
            drive_audio(l.audio.as_mut(), &i);
            if let Some(v) = &mut l.video {
                v.track.opacity.drive_step(&i);
                drive_fx(&mut v.track.effects, &i);
            }
            for c in &mut l.clips {
                let i = Inputs {
                    links: links(&c.dashboard),
                    transition,
                    layer,
                    ..base
                };
                drive_coll(&mut c.dashboard, &i);
                drive_audio(c.audio.as_mut().map(|a| &mut a.track), &i);
                if let Some(v) = &mut c.video {
                    v.track.opacity.drive_step(&i);
                    // Generator settings (Sources) can be driven like any param.
                    if let Some(sp) = &mut v.sourceparams {
                        drive_coll(sp, &i);
                    }
                    drive_fx(&mut v.track.effects, &i);
                }
                match &mut c.transport {
                    Some(Transport::Timeline(t)) => t.controls.speed.drive_step(&i),
                    Some(Transport::BpmSync(t)) => t.controls.speed.drive_step(&i),
                    None => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opacity() -> RangeParam {
        RangeParam::new(7, 0.0, 1.0, 0.5)
    }

    fn run(p: &mut RangeParam, dt: f64, n: usize, beats0: f64) -> Vec<f64> {
        (0..n)
            .map(|k| {
                p.drive_step(&Inputs {
                    dt,
                    beats: beats0 + (k + 1) as f64 * dt * 2.0, // 120 BPM
                    ..Default::default()
                });
                p.value
            })
            .collect()
    }

    #[test]
    fn envelope_shapes_the_output() {
        let mut e = Envelope::default();
        assert_eq!(e.eval(0.25), 0.25, "identity");
        // Triangle: up to 1 at the middle, back to 0.
        e.move_point(1, 1.0, 0.0);
        let i = e.insert(0.5, 1.0);
        assert_eq!(i, 1);
        assert_eq!(e.eval(0.25), 0.5);
        assert_eq!(e.eval(0.5), 1.0);
        assert_eq!(e.eval(0.75), 0.5);
        // Hold keeps the value until the next point.
        e.points[0].curve = Curve::Hold;
        assert_eq!(e.eval(0.4), 0.0);
        // Ends stay put.
        assert!(!e.remove(0));
        e.move_point(0, 0.3, 0.2);
        assert_eq!((e.points[0].x, e.points[0].y), (0.0, 0.2));
        // Inner points stay between neighbours.
        e.move_point(1, 2.0, 0.5);
        assert_eq!(e.points[1].x, 1.0);
        assert!(e.remove(1));

        // Through a drive: value = from + envelope(phase) × (to − from).
        let mut p = opacity();
        p.set_source(Source::Dashboard);
        let d = p.drive.as_mut().unwrap();
        d.envelope = Some(Envelope {
            points: vec![
                EnvPoint {
                    x: 0.0,
                    y: 1.0,
                    curve: Curve::Linear,
                },
                EnvPoint {
                    x: 1.0,
                    y: 0.0,
                    curve: Curve::Linear,
                },
            ],
        });
        let mut links = [0.0; LINKS];
        links[0] = 0.25;
        p.drive_step(&Inputs {
            links,
            ..Default::default()
        });
        assert!((p.value - 0.75).abs() < 1e-9, "inverted: {}", p.value);
    }

    #[test]
    fn timeline_modes() {
        let mut p = opacity();
        p.set_source(Source::Timeline);
        let d = p.drive.as_mut().unwrap();
        d.seconds = 1.0;
        let v = run(&mut p, 0.25, 5, 0.0);
        assert_eq!(v, [0.25, 0.5, 0.75, 0.0, 0.25], "loop");

        let mut p = opacity();
        p.set_source(Source::Timeline);
        let d = p.drive.as_mut().unwrap();
        d.seconds = 1.0;
        d.mode = PlayMode::Bounce;
        let v = run(&mut p, 0.25, 6, 0.0);
        assert_eq!(v, [0.25, 0.5, 0.75, 1.0, 0.75, 0.5], "bounce");

        let mut p = opacity();
        p.set_source(Source::Timeline);
        let d = p.drive.as_mut().unwrap();
        d.seconds = 1.0;
        d.mode = PlayMode::Once;
        d.direction = Direction::Backward;
        d.restart();
        let v = run(&mut p, 0.5, 3, 0.0);
        assert_eq!(v, [0.5, 0.0, 0.0], "once backward");

        // Basic restores the value from before.
        p.set_source(Source::Basic);
        assert_eq!(p.value, 0.5);
        assert!(!p.is_driven());
    }

    #[test]
    fn bpm_sync_locks_to_beats_and_range_handles() {
        let mut p = opacity();
        p.set_source(Source::BpmSync);
        let d = p.drive.as_mut().unwrap();
        d.beats = 4.0;
        d.from = 0.2;
        d.to = 0.6;
        p.drive_step(&Inputs {
            beats: 9.0, // bar 3, beat 2 → phase 0.25
            ..Default::default()
        });
        assert!((p.value - 0.3).abs() < 1e-9, "{}", p.value);
    }

    #[test]
    fn dashboard_and_serde() {
        let mut c = Composition::new("t", 1, 1);
        ensure_links(&mut c.layers[0].dashboard, &mut c.ids);
        if let Some(Param::Range(l)) = c.layers[0].dashboard.get_mut("Link 3") {
            l.set(0.75);
        }
        c.layers[0].master.set_source(Source::Dashboard);
        c.layers[0].master.drive.as_mut().unwrap().link = 2;
        c.drive(&DriveFrame::default());
        assert_eq!(c.layers[0].master.value, 0.75);
        let json = serde_json::to_string(&c).unwrap();
        let back: Composition = serde_json::from_str(&json).unwrap();
        let d = back.layers[0].master.drive.as_ref().unwrap();
        assert_eq!((d.source, d.link), (Source::Dashboard, 2));
    }

    #[test]
    fn source_params_are_driven() {
        let mut c = Composition::new("T", 1, 1);
        let ids = &mut c.ids;
        assert!(c.layers[0].clips[0].load_source(ids, "Lines"));
        let sp = c.layers[0].clips[0]
            .video
            .as_mut()
            .unwrap()
            .sourceparams
            .as_mut()
            .unwrap();
        let key = sp
            .iter()
            .find_map(|(k, p)| matches!(p, Param::Range(_)).then(|| k.clone()))
            .expect("a range param");
        let Some(Param::Range(r)) = sp.get_mut(&key) else {
            unreachable!()
        };
        r.drive = Some(Box::new(Drive::new(r, Source::ExternalFft)));
        let mut f = DriveFrame {
            dt: 1.0 / 60.0,
            ..Default::default()
        };
        f.external = [1.0; fft::BANDS];
        c.drive(&f);
        let sp = c.layers[0].clips[0]
            .video
            .as_ref()
            .unwrap()
            .sourceparams
            .as_ref()
            .unwrap();
        let Some(Param::Range(r)) = sp.get(&key) else {
            unreachable!()
        };
        let d = r.drive.as_ref().unwrap();
        assert_eq!(d.seen, [1.0; fft::BANDS], "the drive saw the external spectrum");
        assert!(d.output() > 0.9, "and follows it: {}", d.output());
    }

    #[test]
    fn fft_sources_read_their_spectrum() {
        let mut c = Composition::new("t", 2, 1);
        c.layers[1].master.set_source(Source::LayerFft);
        c.master.set_source(Source::ExternalFft);
        c.master.drive.as_mut().unwrap().fft.set_preset(fft::Preset::High);
        let mut loud_low = fft::SILENT;
        loud_low[1] = 0.8;
        let mut loud_high = fft::SILENT;
        loud_high[31] = 0.6;
        let mut f = DriveFrame {
            dt: 0.1,
            external: loud_high,
            layers: vec![fft::SILENT, loud_low],
            ..Default::default()
        };
        c.drive(&f);
        assert!((c.master.value - 0.6).abs() < 1e-6, "{}", c.master.value);
        assert!((c.layers[1].master.value - 0.8).abs() < 1e-6);
        // Silence: falls at the fall rate (0.75 → 0.1875 s for the full range).
        f.layers[1] = fft::SILENT;
        c.drive(&f);
        let v = c.layers[1].master.value;
        assert!(v < 0.8 && v > 0.2, "falling, not dropped: {v}");
        // Serde keeps the band.
        let json = serde_json::to_string(&c).unwrap();
        let back: Composition = serde_json::from_str(&json).unwrap();
        assert_eq!(back.master.drive.unwrap().fft.preset(), Some(fft::Preset::High));
    }

    #[test]
    fn effect_params_follow_transition() {
        let mut c = Composition::new("t", 1, 1);
        c.add_effect(crate::effects::FxTarget::Layer(0), "Blur", None);
        let fx = &mut c.layers[0].video.as_mut().unwrap().track.effects[1];
        let Some(Param::Range(b)) = fx.params.get_mut("Blur") else {
            panic!()
        };
        b.set_source(Source::TransitionPhase);
        c.drive(&DriveFrame {
            transitions: vec![0.4],
            ..Default::default()
        });
        let fx = &c.layers[0].video.as_ref().unwrap().track.effects[1];
        let Some(Param::Range(b)) = fx.params.get("Blur") else {
            panic!()
        };
        assert!((b.value - 0.4).abs() < 1e-9);
    }
}
