//! App side of audio: keeps the FFT input tap and the clip-audio output in
//! line with Preferences › Audio, decodes clip audio (once per file, on a
//! worker), turns playing clips into mixer voices and hands this frame's
//! spectra to the parameter drive.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use lavmfx_audio::{InputTap, Output, OutputConfig, TapConfig, Voice};
use lavmfx_core::fft::{self, Spectrum};
use lavmfx_media::audio::Pcm;

use crate::app::prefs;

/// Drop decoded audio not played for this long.
const KEEP: Duration = Duration::from_secs(300);

enum Decoded {
    Loading(Receiver<Option<Arc<Pcm>>>),
    /// None = the file has no (usable) audio.
    Ready(Option<Arc<Pcm>>, Instant),
}

/// A playing clip, as the app sees it this frame.
pub struct Want {
    pub key: u64,
    pub path: PathBuf,
    pub layer: usize,
    /// Seconds into the file.
    pub pos: f64,
    /// Length of the file (for wrap-around).
    pub duration: f64,
    /// Random play mode: no audio (it jumps every frame).
    pub random: bool,
    /// Gain into its layer's bus (clip volume × fade), pre-fader gain
    /// (Layer FFT), pre-composition gain (Composition FFT).
    pub gains: [f32; 3],
    /// Clip pan.
    pub pan: f32,
    /// Its layer's bus (index into the buses given to [`Audio::play`]).
    pub bus: usize,
    /// Clip audio effects.
    pub fx: Vec<lavmfx_core::audiofx::FxDesc>,
}

#[derive(Default)]
pub struct Audio {
    tap: Option<InputTap>,
    /// Config the tap was opened with (or failed with: no retry every frame).
    applied: Option<TapConfig>,
    /// Last open error, shown in Preferences.
    pub error: Option<String>,
    out: Option<Output>,
    out_applied: Option<OutputConfig>,
    pub out_error: Option<String>,
    decoded: HashMap<PathBuf, Decoded>,
    /// Last position and rate per voice key.
    prev: HashMap<u64, (f64, f64)>,
}

impl Audio {
    /// What Preferences ask for (None = neither FFT nor LTC input).
    fn wanted(p: &prefs::Audio) -> Option<TapConfig> {
        let fft = match p.fft_input {
            0 => None,
            1 => Some(None),
            n => Some(Some(n as usize - 2)),
        };
        let ltc = p.smpte_inputs.map(|i| match i {
            prefs::SmpteInput::Ltc(c) => Some(c as usize),
            _ => None,
        });
        if fft.is_none() && ltc == [None, None] {
            return None;
        }
        Some(TapConfig {
            device: p.input_device.clone(),
            fft,
            ltc,
            ltc_fps: p.smpte_fps,
            gain: p.fft_gain as f32,
            noise: p.fan_noise as f32,
            buffer: p.buffer_size,
        })
    }

    /// Latest LTC reading of SMPTE input `k` (0 / 1).
    pub fn timecode(&self, k: usize) -> Option<lavmfx_core::timecode::Reading> {
        self.tap.as_ref().and_then(|t| t.timecode.get(k)?.load())
    }

    /// Reopen the output when device / rate / buffer changed.
    fn sync_output(&mut self, p: &prefs::Audio) {
        let want = OutputConfig {
            device: p.output_device.clone(),
            sample_rate: p.sample_rate,
            buffer: p.buffer_size,
        };
        if self.out_applied.as_ref() == Some(&want) {
            return;
        }
        self.out = None;
        match Output::open(&want) {
            Ok(o) => {
                self.out = Some(o);
                self.out_error = None;
            }
            Err(e) => {
                log::warn!("audio output: {e}");
                self.out_error = Some(e);
            }
        }
        self.out_applied = Some(want);
    }

    /// Reopen the tap when the device/channel/buffer changed; gain applies live.
    pub fn sync(&mut self, p: &prefs::Audio) {
        self.sync_output(p);
        let want = Self::wanted(p);
        let same = |a: &TapConfig, b: &TapConfig| {
            TapConfig {
                gain: 0.0,
                noise: 0.0,
                ..a.clone()
            } == TapConfig {
                gain: 0.0,
                noise: 0.0,
                ..b.clone()
            }
        };
        match (&want, &self.applied) {
            (None, None) => {}
            (None, Some(_)) => {
                self.tap = None;
                self.applied = None;
                self.error = None;
            }
            (Some(w), Some(a)) if same(w, a) => {
                if let Some(t) = &self.tap {
                    t.set_gain(w.gain);
                    t.set_noise(w.noise);
                }
            }
            (Some(w), _) => {
                self.tap = None;
                match InputTap::open(w) {
                    Ok(t) => {
                        self.tap = Some(t);
                        self.error = None;
                    }
                    Err(e) => {
                        log::warn!("FFT input: {e}");
                        self.error = Some(e);
                    }
                }
                self.applied = want.clone();
            }
        }
    }

    /// Try again after an error (Preferences › Refresh).
    pub fn retry(&mut self) {
        self.applied = None;
        self.tap = None;
        self.out_applied = None;
        self.out = None;
    }

    /// Decoded audio of `path`: Some(true/false) once known, None while loading
    /// (starts decoding on first ask).
    pub fn has_audio(&mut self, path: &Path) -> Option<bool> {
        self.pcm(path).map(|p| p.is_some())
    }

    fn pcm(&mut self, path: &Path) -> Option<Option<Arc<Pcm>>> {
        let e = self.decoded.entry(path.to_owned()).or_insert_with(|| {
            let (tx, rx) = channel();
            let p = path.to_owned();
            std::thread::spawn(move || {
                let pcm = match lavmfx_media::audio::decode(&p) {
                    Ok(x) => x.map(Arc::new),
                    Err(e) => {
                        log::warn!("audio of {}: {e}", p.display());
                        None
                    }
                };
                let _ = tx.send(pcm);
            });
            Decoded::Loading(rx)
        });
        if let Decoded::Loading(rx) = e {
            match rx.try_recv() {
                Ok(p) => *e = Decoded::Ready(p, Instant::now()),
                Err(std::sync::mpsc::TryRecvError::Empty) => return None,
                Err(_) => *e = Decoded::Ready(None, Instant::now()),
            }
        }
        match e {
            Decoded::Ready(p, used) => {
                *used = Instant::now();
                Some(p.clone())
            }
            Decoded::Loading(_) => None,
        }
    }

    /// Send this frame's playing clips to the mixer. `stamp` = when their
    /// positions were taken, `wall_dt` = wall time the playheads advanced by.
    pub fn play(&mut self, wants: &[Want], buses: Vec<lavmfx_audio::mixer::Bus>, stamp: Instant, wall_dt: f64) {
        let mut voices = Vec::with_capacity(wants.len());
        let mut prev = HashMap::with_capacity(wants.len());
        for w in wants {
            if w.random || w.gains.iter().all(|&g| g <= 0.0) && !self.prev.contains_key(&w.key) {
                continue;
            }
            let Some(Some(pcm)) = self.pcm(&w.path) else { continue };
            // Rate = how far the clip moved per wall second (speed, direction,
            // composition speed, BPM sync…); a jump keeps the previous rate.
            let rate = match self.prev.get(&w.key) {
                Some(&(p0, r0)) if wall_dt > 1e-6 => {
                    let dur = w.duration.max(1e-6);
                    let mut d = w.pos - p0;
                    if d.abs() > dur * 0.5 {
                        d -= d.signum() * dur; // wrapped (loop)
                    }
                    let r = d / wall_dt;
                    if r.abs() > 8.0 { r0 } else { r }
                }
                _ => 1.0,
            };
            prev.insert(w.key, (w.pos, rate));
            voices.push(Voice {
                key: w.key,
                pcm,
                layer: w.layer,
                pos: w.pos,
                rate,
                stamp,
                gain: w.gains[0],
                layer_gain: w.gains[1],
                comp_gain: w.gains[2],
                pan: w.pan,
                bus: w.bus,
                fx: w.fx.clone(),
            });
        }
        self.prev = prev;
        if let Some(o) = &mut self.out {
            o.set_mix(voices, buses);
        }
        // Forget audio nobody played for a while.
        self.decoded
            .retain(|_, d| !matches!(d, Decoded::Ready(_, used) if used.elapsed() > KEEP));
    }

    /// Composition audio spectrum.
    pub fn composition(&self) -> Spectrum {
        self.out.as_ref().map_or(fft::SILENT, |o| o.composition.load())
    }

    /// Audio spectrum per layer.
    pub fn layers(&self, n: usize) -> Vec<Spectrum> {
        (0..n)
            .map(|i| self.out.as_ref().map_or(fft::SILENT, |o| o.layer_spectrum(i)))
            .collect()
    }

    /// Output status line for Preferences.
    pub fn out_status(&self) -> String {
        match (&self.out, &self.out_error) {
            (Some(o), _) => format!(
                "Playing to: {} · {} ch · {} Hz",
                o.device_name, o.channels, o.sample_rate
            ),
            (None, Some(e)) => format!("Output failed: {e}"),
            (None, None) => "No output".into(),
        }
    }

    /// The output's mix tap and sample rate (recording), if a device is open.
    pub fn tap(&self) -> Option<(std::sync::Arc<lavmfx_audio::mixer::AudioTap>, u32)> {
        self.out.as_ref().map(|o| (o.tap.clone(), o.sample_rate))
    }

    /// External FFT spectrum (silent without input).
    pub fn external(&self) -> Spectrum {
        self.tap.as_ref().map_or(fft::SILENT, |t| t.spectrum.load())
    }

    /// Status line for Preferences.
    pub fn status(&self) -> String {
        match (&self.tap, &self.error) {
            (Some(t), _) => format!(
                "Listening: {} · {} ch · {} Hz{}",
                t.device_name,
                t.channels,
                t.sample_rate,
                if t.spectrum.count() == 0 { " · no data yet" } else { "" }
            ),
            (None, Some(e)) => format!("Input failed: {e}"),
            (None, None) => "No audio input".into(),
        }
    }
}
