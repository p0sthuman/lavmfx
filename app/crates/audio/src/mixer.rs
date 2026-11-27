//! Clip audio output. The UI thread describes the playing clips once per
//! frame (`Voice`: file, position, rate, gain, pan, clip effects) and the
//! mix buses (`Bus`: layers, groups, composition, each with gain, pan and
//! effects); the audio callback plays the clips from memory, following the
//! reported position (it jumps when it drifts, e.g. after a seek or loop),
//! runs every effect chain in its place (clip → layer → group →
//! composition) and analyses the mix and every layer (before effects) for
//! the Composition / Layer FFT drive sources.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use lavmfx_core::fft::{self, Spectrum};
use lavmfx_media::audio::Pcm;

use lavmfx_core::audiofx::FxDesc;

use crate::analyzer::{Analyzer, N};
use crate::device::{SharedSpectrum, find};
use crate::fx::Chain;

/// Layers with their own spectrum (Layer FFT); more layers still play.
pub const MAX_LAYERS: usize = 64;
/// Jump when the voice is this far (seconds) from where the clip is.
const RESYNC: f64 = 0.06;
/// Gain smoothing time constant (seconds): no clicks on changes.
const SMOOTH: f32 = 0.005;

/// One playing clip, as of `stamp`.
#[derive(Clone)]
pub struct Voice {
    /// Stable while the same clip plays in the same role (clip id, outgoing…).
    pub key: u64,
    pub pcm: Arc<Pcm>,
    pub layer: usize,
    /// Seconds into the file at `stamp`.
    pub pos: f64,
    /// File seconds per second (speed × direction; 0 = paused).
    pub rate: f64,
    pub stamp: Instant,
    /// Linear gain into its bus (clip volume × transition fade); without
    /// buses straight to the output (then everything multiplied in).
    pub gain: f32,
    /// Gain into the layer's spectrum: pre-fader (clip volume, transition fade),
    /// so a Layer FFT drive on the layer master can't silence its own input.
    pub layer_gain: f32,
    /// Gain into the composition spectrum: before the composition master/volume.
    pub comp_gain: f32,
    /// −1 (left) … 1 (right), balance.
    pub pan: f32,
    /// Index into the buses (its layer); out of range = the output.
    pub bus: usize,
    /// Clip audio effects (before `gain` / `pan`).
    pub fx: Vec<FxDesc>,
}

/// A mix stage: layer, group or composition. Effects, then gain and pan,
/// then into `parent` (None = the output). A parent comes before its
/// children in the list (composition first).
#[derive(Clone, Debug, PartialEq)]
pub struct Bus {
    /// Stable id (layer / group id): effect state follows it.
    pub key: u64,
    pub parent: Option<usize>,
    pub gain: f32,
    pub pan: f32,
    pub fx: Vec<FxDesc>,
}

#[derive(Default)]
struct Shared {
    voices: Vec<Voice>,
    buses: Vec<Bus>,
    generation: u64,
}

/// Balance: −1 keeps only the left, 1 only the right.
fn balance([l, r]: [f32; 2], pan: f32) -> [f32; 2] {
    let pan = pan.clamp(-1.0, 1.0);
    [l * (1.0 - pan).min(1.0), r * (1.0 + pan).min(1.0)]
}

/// What the callback keeps per bus.
struct BusState {
    b: Bus,
    chain: Chain,
    /// Smoothed gain.
    g: f32,
    acc: [f32; 2],
}

/// What the callback keeps per voice.
struct Playing {
    v: Voice,
    /// Frames into `v.pcm` (its own rate).
    pos: f64,
    /// Fade envelope: 0 on start/jumps/pause/removal, 1 while playing.
    env: f32,
    /// Smoothed output / layer / composition gains.
    g: [f32; 3],
    gone: bool,
    /// Clip effects.
    chain: Chain,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutputConfig {
    /// Device id, None = system default output.
    pub device: Option<String>,
    pub sample_rate: u32,
    /// Frames per callback, 0 = device default.
    pub buffer: u32,
}

pub struct Output {
    _stream: cpal::Stream,
    shared: Arc<Mutex<Shared>>,
    generation: u64,
    pub composition: Arc<SharedSpectrum>,
    pub layers: Arc<Vec<SharedSpectrum>>,
    /// Output peak (f32 bits), for a meter.
    peak: Arc<AtomicU64>,
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// Final mix for recording.
    pub tap: Arc<AudioTap>,
}

impl Output {
    pub fn open(cfg: &OutputConfig) -> Result<Self, String> {
        let dev = find(cfg.device.as_deref(), false).ok_or("no audio output device")?;
        let def = dev.default_output_config().map_err(|e| e.to_string())?;
        // The preferred rate if the device has it, else its default.
        let rate_ok = dev.supported_output_configs().is_ok_and(|mut it| {
            it.any(|c| {
                c.channels() == def.channels()
                    && c.sample_format() == def.sample_format()
                    && (c.min_sample_rate()..=c.max_sample_rate()).contains(&cfg.sample_rate)
            })
        });
        let sample_rate = if rate_ok { cfg.sample_rate } else { def.sample_rate() };
        let channels = def.channels();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let composition = Arc::new(SharedSpectrum::default());
        let layers: Arc<Vec<SharedSpectrum>> = Arc::new((0..MAX_LAYERS).map(|_| SharedSpectrum::default()).collect());
        let peak = Arc::new(AtomicU64::new(0));
        let tap = Arc::new(AudioTap::default());
        let open = |buffer_size| {
            let config = cpal::StreamConfig {
                channels,
                sample_rate,
                buffer_size,
            };
            let st = State::new(
                sample_rate,
                shared.clone(),
                composition.clone(),
                layers.clone(),
                peak.clone(),
            );
            match def.sample_format() {
                cpal::SampleFormat::F32 => build::<f32>(&dev, &config, st, tap.clone()),
                cpal::SampleFormat::I16 => build::<i16>(&dev, &config, st, tap.clone()),
                cpal::SampleFormat::I32 => build::<i32>(&dev, &config, st, tap.clone()),
                cpal::SampleFormat::U16 => build::<u16>(&dev, &config, st, tap.clone()),
                f => Err(format!("unsupported sample format {f:?}")),
            }
        };
        let stream = match cfg.buffer {
            0 => open(cpal::BufferSize::Default),
            n => open(cpal::BufferSize::Fixed(n)).or_else(|e| {
                log::warn!("audio output: buffer {n} refused ({e}), using the device default");
                open(cpal::BufferSize::Default)
            }),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        let device_name = dev
            .description()
            .map_or_else(|_| dev.to_string(), |x| x.name().to_owned());
        log::info!("audio output: {device_name} ({channels} ch, {sample_rate} Hz)");
        Ok(Self {
            _stream: stream,
            shared,
            generation: 0,
            composition,
            layers,
            peak,
            device_name,
            sample_rate,
            channels,
            tap,
        })
    }

    /// Replace the playing set and the buses (call once per UI frame).
    pub fn set_mix(&mut self, voices: Vec<Voice>, buses: Vec<Bus>) {
        self.generation += 1;
        if let Ok(mut s) = self.shared.lock() {
            s.voices = voices;
            s.buses = buses;
            s.generation = self.generation;
        }
    }

    pub fn layer_spectrum(&self, layer: usize) -> Spectrum {
        self.layers.get(layer).map_or(fft::SILENT, |s| s.load())
    }

    /// Output peak since the last call (0..1+).
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed) as u32)
    }
}

/// Audio-thread state.
struct State {
    rate: u32,
    shared: Arc<Mutex<Shared>>,
    generation: u64,
    playing: Vec<Playing>,
    comp: Analyzer,
    layers: Vec<Analyzer>,
    /// Frames since the layer last had a voice (stop analysing once silent).
    quiet: Vec<usize>,
    layer_sum: Vec<f32>,
    out_comp: Arc<SharedSpectrum>,
    out_layers: Arc<Vec<SharedSpectrum>>,
    peak: Arc<AtomicU64>,
    smooth: f32,
    buses: Vec<BusState>,
}

impl State {
    fn new(
        rate: u32,
        shared: Arc<Mutex<Shared>>,
        out_comp: Arc<SharedSpectrum>,
        out_layers: Arc<Vec<SharedSpectrum>>,
        peak: Arc<AtomicU64>,
    ) -> Self {
        Self {
            rate,
            shared,
            generation: 0,
            playing: Vec::with_capacity(64),
            comp: Analyzer::new(rate),
            layers: (0..MAX_LAYERS).map(|_| Analyzer::new(rate)).collect(),
            quiet: vec![usize::MAX / 2; MAX_LAYERS],
            layer_sum: vec![0.0; MAX_LAYERS],
            out_comp,
            out_layers,
            peak,
            smooth: 1.0 - (-1.0 / (SMOOTH * rate as f32)).exp(),
            buses: Vec::new(),
        }
    }

    /// Take the UI's latest voices (never blocks).
    fn sync(&mut self) {
        let Ok(s) = self.shared.try_lock() else { return };
        if s.generation == self.generation {
            return;
        }
        self.generation = s.generation;
        let now = Instant::now();
        for p in &mut self.playing {
            p.gone = true;
        }
        for v in &s.voices {
            let expected = v.pos + v.rate * now.saturating_duration_since(v.stamp).as_secs_f64();
            let rate = v.pcm.rate as f64;
            match self.playing.iter_mut().find(|p| p.v.key == v.key) {
                Some(p) if Arc::ptr_eq(&p.v.pcm, &v.pcm) => {
                    let dur = v.pcm.duration().max(1e-6);
                    let own = p.pos / rate;
                    let d = (expected - own).rem_euclid(dur);
                    let drift = d.min(dur - d);
                    if drift > RESYNC {
                        p.pos = expected * rate;
                        p.env = 0.0;
                    }
                    p.v = v.clone();
                    p.gone = false;
                    p.chain.update(&v.fx);
                }
                _ => {
                    self.playing.retain(|p| p.v.key != v.key);
                    let mut chain = Chain::new(self.rate);
                    chain.update(&v.fx);
                    self.playing.push(Playing {
                        v: v.clone(),
                        pos: expected * rate,
                        env: 0.0,
                        g: [v.gain, v.layer_gain, v.comp_gain],
                        gone: false,
                        chain,
                    });
                }
            }
        }
        // Buses: keep each one's effect state and gain by key.
        let mut old = std::mem::take(&mut self.buses);
        for (i, b) in s.buses.iter().enumerate() {
            let mut b = b.clone();
            if b.parent.is_some_and(|p| p >= i) {
                b.parent = None; // parents must come first
            }
            let st = match old.iter().position(|o| o.b.key == b.key) {
                Some(j) => {
                    let mut o = old.swap_remove(j);
                    o.chain.update(&b.fx);
                    o.b = b;
                    o
                }
                None => {
                    let mut chain = Chain::new(self.rate);
                    chain.update(&b.fx);
                    BusState {
                        g: b.gain,
                        b,
                        chain,
                        acc: [0.0; 2],
                    }
                }
            };
            self.buses.push(st);
        }
    }

    /// Fill one interleaved buffer.
    fn render(&mut self, out: &mut [f32], channels: usize) {
        self.sync();
        let ch = channels.max(1);
        let out_rate = self.rate as f64;
        let mut peak = 0.0f32;
        let k = self.smooth;
        for frame in out.chunks_exact_mut(ch) {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            self.layer_sum.iter_mut().for_each(|s| *s = 0.0);
            self.buses.iter_mut().for_each(|b| b.acc = [0.0; 2]);
            let mut comp = 0.0f32;
            for p in &mut self.playing {
                let target = if p.gone || p.v.rate == 0.0 { 0.0 } else { 1.0 };
                p.env += (target - p.env) * k;
                if p.env < 1e-5 && target == 0.0 {
                    continue;
                }
                for (g, t) in p.g.iter_mut().zip([p.v.gain, p.v.layer_gain, p.v.comp_gain]) {
                    *g += (t - *g) * k;
                }
                let (a, b) = p.v.pcm.at(p.pos);
                let mono = (a + b) * 0.5 * p.env;
                let mut x = [a * p.env, b * p.env];
                if !p.chain.is_empty() {
                    x = p.chain.run(x);
                }
                let [xl, xr] = balance(x, p.v.pan);
                let go = p.g[0];
                match self.buses.get_mut(p.v.bus) {
                    Some(bus) => {
                        bus.acc[0] += xl * go;
                        bus.acc[1] += xr * go;
                    }
                    None => {
                        l += xl * go;
                        r += xr * go;
                    }
                }
                if let Some(s) = self.layer_sum.get_mut(p.v.layer) {
                    *s += mono * p.g[1];
                }
                comp += mono * p.g[2];
                p.pos += p.v.rate * p.v.pcm.rate as f64 / out_rate;
            }
            // Buses, children first: effects, gain, pan, up to the parent.
            for i in (0..self.buses.len()).rev() {
                let bus = &mut self.buses[i];
                bus.g += (bus.b.gain - bus.g) * k;
                let mut x = bus.acc;
                if !bus.chain.is_empty() {
                    x = bus.chain.run(x);
                }
                let [bl, br] = balance(x, bus.b.pan);
                let (bl, br) = (bl * bus.g, br * bus.g);
                match bus.b.parent {
                    Some(p) => {
                        let up = &mut self.buses[p].acc;
                        up[0] += bl;
                        up[1] += br;
                    }
                    None => {
                        l += bl;
                        r += br;
                    }
                }
            }
            match ch {
                1 => frame[0] = (l + r) * 0.5,
                _ => {
                    frame[0] = l;
                    frame[1] = r;
                    frame[2..].iter_mut().for_each(|s| *s = 0.0);
                }
            }
            peak = peak.max(l.abs()).max(r.abs());
            self.comp.push(&[comp]);
            for (i, s) in self.layer_sum.iter().enumerate() {
                if *s != 0.0 {
                    self.quiet[i] = 0;
                } else if self.quiet[i] > N * 2 {
                    continue; // long silent: nothing to analyse
                } else {
                    self.quiet[i] += 1;
                }
                self.layers[i].push(std::slice::from_ref(s));
            }
        }
        // Drop voices that have faded out.
        self.playing.retain(|p| !(p.gone && p.env < 1e-5));
        if self.comp.take_fresh() {
            self.out_comp.store(self.comp.spectrum());
        }
        for (a, o) in self.layers.iter_mut().zip(self.out_layers.iter()) {
            if a.take_fresh() {
                o.store(a.spectrum());
            }
        }
        let prev = f32::from_bits(self.peak.load(Ordering::Relaxed) as u32);
        if peak > prev {
            self.peak.store(peak.to_bits() as u64, Ordering::Relaxed);
        }
    }
}

/// A copy of the final mix (stereo interleaved f32) while `on`, for
/// recording. The audio thread never waits: a busy lock drops that block.
#[derive(Default)]
pub struct AudioTap {
    on: std::sync::atomic::AtomicBool,
    buf: Mutex<Vec<f32>>,
}

impl AudioTap {
    pub fn set_on(&self, on: bool) {
        self.on.store(on, Ordering::Relaxed);
        if let Ok(mut b) = self.buf.lock() {
            b.clear();
        }
    }
    /// Samples collected since the last call.
    pub fn take(&self) -> Vec<f32> {
        self.buf.lock().map(|mut b| std::mem::take(&mut *b)).unwrap_or_default()
    }
    fn push(&self, mix: &[f32], channels: usize) {
        if !self.on.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut b) = self.buf.try_lock() else { return };
        // At most ~10 s waiting (nobody reading): drop the oldest.
        if b.len() > 960_000 {
            b.clear();
        }
        for f in mix.chunks_exact(channels.max(1)) {
            let l = f[0];
            let r = f.get(1).copied().unwrap_or(l);
            b.push(l);
            b.push(r);
        }
    }
}

fn build<T>(
    dev: &cpal::Device,
    config: &cpal::StreamConfig,
    mut st: State,
    tap: Arc<AudioTap>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = config.channels as usize;
    let mut buf: Vec<f32> = Vec::with_capacity(16384);
    dev.build_output_stream::<T, _, _>(
        *config,
        move |data: &mut [T], _| {
            buf.resize(data.len(), 0.0);
            st.render(&mut buf, channels);
            tap.push(&buf, channels);
            for (d, s) in data.iter_mut().zip(&buf) {
                *d = T::from_sample(s.clamp(-1.0, 1.0));
            }
        },
        |e| log::warn!("audio output: {e}"),
        None,
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(rate: u32) -> (State, Arc<Mutex<Shared>>) {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let st = State::new(
            rate,
            shared.clone(),
            Arc::default(),
            Arc::new((0..MAX_LAYERS).map(|_| SharedSpectrum::default()).collect()),
            Arc::default(),
        );
        (st, shared)
    }

    /// 1 s ramp: frame i holds i (as i16) on both channels.
    fn ramp(rate: u32) -> Arc<Pcm> {
        Arc::new(Pcm {
            rate,
            data: (0..rate).flat_map(|i| [i as i16, i as i16]).collect(),
        })
    }

    fn set(shared: &Arc<Mutex<Shared>>, voices: Vec<Voice>) {
        let mut s = shared.lock().unwrap();
        s.generation += 1;
        s.voices = voices;
    }

    fn voice(pcm: &Arc<Pcm>, pos: f64, rate: f64) -> Voice {
        Voice {
            key: 1,
            pcm: pcm.clone(),
            layer: 0,
            pos,
            rate,
            stamp: Instant::now(),
            gain: 1.0,
            layer_gain: 1.0,
            comp_gain: 1.0,
            pan: 0.0,
            bus: 0,
            fx: Vec::new(),
        }
    }

    #[test]
    fn follows_position_rate_and_resyncs() {
        let (mut st, shared) = state(1000);
        let pcm = ramp(1000);
        set(&shared, vec![voice(&pcm, 0.5, 1.0)]);
        let mut out = vec![0.0; 2 * 100];
        st.render(&mut out, 2);
        // Starts at frame 500 and plays forward (gain fades in from 0).
        let p = &st.playing[0];
        assert!((p.pos - 600.0).abs() < 2.0, "{}", p.pos);
        assert!(out[198] > 0.0 && (out[198] * 32768.0 - 599.0).abs() < 2.0 / p.env.max(0.5));

        // Same position reported again (still in sync): no jump.
        set(&shared, vec![voice(&pcm, 0.6, 1.0)]);
        st.render(&mut out, 2);
        assert!((st.playing[0].pos - 700.0).abs() < 2.0);

        // Seek: jump to the reported place.
        set(&shared, vec![voice(&pcm, 0.1, -2.0)]);
        st.render(&mut out, 2);
        assert!(
            (st.playing[0].pos - (100.0 - 200.0)).abs() < 2.0,
            "backwards ×2: {}",
            st.playing[0].pos
        );
        // Reading below 0 wraps to the end of the file.
        assert!(out[198] * 32768.0 > 800.0, "{}", out[198] * 32768.0);
    }

    #[test]
    fn pan_mute_and_fade_out() {
        let (mut st, shared) = state(1000);
        let pcm = Arc::new(Pcm {
            rate: 1000,
            data: vec![16384; 2000],
        });
        let mut v = voice(&pcm, 0.0, 1.0);
        v.pan = -1.0;
        set(&shared, vec![v]);
        let mut out = vec![0.0; 2 * 200];
        st.render(&mut out, 2);
        assert!(
            out[398] > 0.45 && out[399] == 0.0,
            "hard left: {} {}",
            out[398],
            out[399]
        );
        // Removed: fades out, then the voice is dropped.
        set(&shared, vec![]);
        st.render(&mut out, 2);
        assert!(out[398].abs() < 1e-3);
        assert!(st.playing.is_empty());
    }

    #[test]
    fn layer_and_mix_spectra() {
        let (mut st, shared) = state(48_000);
        let pcm = Arc::new(Pcm {
            rate: 48_000,
            data: (0..48_000)
                .flat_map(|i| {
                    let s = ((std::f64::consts::TAU * 100.0 * i as f64 / 48_000.0).sin() * 30000.0) as i16;
                    [s, s]
                })
                .collect(),
        });
        let mut v = voice(&pcm, 0.0, 1.0);
        v.layer = 2;
        v.gain = 0.0; // faders down: silent output, spectra still read (pre-fader)
        set(&shared, vec![v]);
        let mut out = vec![0.0; 2 * 8192];
        st.render(&mut out, 2);
        let low = |s: Spectrum| s[..8].iter().copied().fold(0.0f32, f32::max);
        assert!(low(st.out_layers[2].load()) > 0.8);
        assert!(low(st.out_comp.load()) > 0.8);
        assert_eq!(low(st.out_layers[0].load()), 0.0);
        assert!(out.iter().all(|s| s.abs() < 1e-6), "output silent");
    }

    #[test]
    fn buses_multiply_gains_and_run_effects_in_order() {
        let (mut st, shared) = state(1000);
        let pcm = Arc::new(Pcm {
            rate: 1000,
            data: vec![16384; 4000],
        });
        let bus = |key, parent, gain, fx: Vec<FxDesc>| Bus {
            key,
            parent,
            gain,
            pan: 0.0,
            fx,
        };
        let mut v = voice(&pcm, 0.0, 1.0);
        v.bus = 2;
        v.gain = 0.5;
        {
            let mut s = shared.lock().unwrap();
            s.generation += 1;
            s.voices = vec![v.clone()];
            // composition 0.5 ← group 1.0 ← layer 0.8
            s.buses = vec![
                bus(10, None, 0.5, vec![]),
                bus(11, Some(0), 1.0, vec![]),
                bus(12, Some(1), 0.8, vec![]),
            ];
        }
        let mut out = vec![0.0; 2 * 400];
        st.render(&mut out, 2);
        let want = 0.5 * 0.5 * 1.0 * 0.8 * 0.5;
        assert!((out[798] - want).abs() < 2e-3, "{} vs {want}", out[798]);
        // A bitcrusher with Gain 0 on the group silences everything below it.
        let mut fx = lavmfx_core::audiofx::FxDesc::of(
            &lavmfx_core::AudioEffect::new(&mut lavmfx_core::IdGen::default(), "Bitcrusher").unwrap(),
        )
        .unwrap();
        fx.values[1] = 0.0;
        {
            let mut s = shared.lock().unwrap();
            s.generation += 1;
            s.buses[1].fx = vec![fx];
        }
        st.render(&mut out, 2);
        assert!(out[798].abs() < 1e-6, "{}", out[798]);
        // A parent listed after its child goes to the output instead.
        {
            let mut s = shared.lock().unwrap();
            s.generation += 1;
            s.buses = vec![bus(12, Some(1), 1.0, vec![]), bus(11, None, 1.0, vec![])];
            s.voices[0].bus = 0;
        }
        st.render(&mut out, 2);
        assert_eq!(st.buses[0].b.parent, None);
        assert!((out[798] - 0.25).abs() < 2e-3, "{}", out[798]);
    }
}
