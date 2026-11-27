//! Audio devices (cpal) and the FFT input tap (Preferences › Audio ›
//! External Audio FFT Input). The tap analyses inside the audio callback and
//! publishes the spectrum through atomics, so the UI thread never blocks it.

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use lavmfx_core::fft::{BANDS, Spectrum};
use lavmfx_core::timecode::{Fps, LtcDecoder, Reading, SharedTimecode};

use crate::analyzer::Analyzer;

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    /// Stable id (stored in the preferences).
    pub id: String,
    pub name: String,
    pub channels: u16,
    pub sample_rate: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Devices {
    pub host: String,
    pub inputs: Vec<DeviceInfo>,
    pub outputs: Vec<DeviceInfo>,
    pub default_input: Option<String>,
    pub default_output: Option<String>,
}

fn info(d: &cpal::Device, cfg: Option<cpal::SupportedStreamConfig>) -> Option<DeviceInfo> {
    let cfg = cfg?;
    Some(DeviceInfo {
        id: d.id().ok()?.to_string(),
        name: d.description().map_or_else(|_| d.to_string(), |x| x.name().to_owned()),
        channels: cfg.channels(),
        sample_rate: cfg.sample_rate(),
    })
}

/// Enumerate the default host's devices (can take a moment: call on demand).
pub fn list() -> Devices {
    let host = cpal::default_host();
    let id_of = |d: Option<cpal::Device>| d.and_then(|d| d.id().ok()).map(|i| i.to_string());
    let inputs = host
        .input_devices()
        .map(|it| it.filter_map(|d| info(&d, d.default_input_config().ok())).collect())
        .unwrap_or_default();
    let outputs = host
        .output_devices()
        .map(|it| it.filter_map(|d| info(&d, d.default_output_config().ok())).collect())
        .unwrap_or_default();
    Devices {
        host: host.id().name().to_owned(),
        inputs,
        outputs,
        default_input: id_of(host.default_input_device()),
        default_output: id_of(host.default_output_device()),
    }
}

/// `id` (from the preferences) or the system default.
pub(crate) fn find(id: Option<&str>, input: bool) -> Option<cpal::Device> {
    let host = cpal::default_host();
    let by_id = id
        .and_then(|s| cpal::DeviceId::from_str(s).ok())
        .and_then(|i| host.device_by_id(&i));
    by_id.or_else(|| {
        if input {
            host.default_input_device()
        } else {
            host.default_output_device()
        }
    })
}

/// Latest spectrum, written by the audio thread.
#[derive(Default)]
pub struct SharedSpectrum {
    bands: [AtomicU32; BANDS],
    /// Number of analyses so far (0 = nothing arrived yet).
    count: AtomicU64,
}

impl SharedSpectrum {
    pub fn store(&self, s: &Spectrum) {
        for (a, v) in self.bands.iter().zip(s) {
            a.store(v.to_bits(), Ordering::Relaxed);
        }
        self.count.fetch_add(1, Ordering::Release);
    }
    pub fn load(&self) -> Spectrum {
        std::array::from_fn(|i| f32::from_bits(self.bands[i].load(Ordering::Relaxed)))
    }
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }
}

/// What the tap listens to.
#[derive(Debug, Clone, PartialEq)]
pub struct TapConfig {
    /// Device id, None = system default input.
    pub device: Option<String>,
    /// FFT: None = off; Some(channel) (0-based), Some(None) = all channels mixed.
    pub fft: Option<Option<usize>>,
    /// LTC timecode channels of SMPTE 1 / SMPTE 2 (0-based).
    pub ltc: [Option<usize>; 2],
    /// Timecode frame rate (LTC carries only the drop-frame flag).
    pub ltc_fps: Fps,
    /// Linear gain before analysis.
    pub gain: f32,
    /// Fan noise reduction 0 (off) … 1 (see `Analyzer::noise`).
    pub noise: f32,
    /// Frames per callback (0 = device default). Small = low latency.
    pub buffer: u32,
}

/// Open audio input (FFT and / or LTC): keeps the stream alive while held.
pub struct InputTap {
    _stream: cpal::Stream,
    pub spectrum: Arc<SharedSpectrum>,
    /// Decoded LTC of SMPTE 1 / SMPTE 2.
    pub timecode: [Arc<SharedTimecode>; 2],
    gain: Arc<AtomicU32>,
    noise: Arc<AtomicU32>,
    pub device_name: String,
    pub channels: u16,
    pub sample_rate: u32,
}

impl InputTap {
    pub fn open(cfg: &TapConfig) -> Result<Self, String> {
        let dev = find(cfg.device.as_deref(), true).ok_or("no audio input device")?;
        let sc = dev.default_input_config().map_err(|e| e.to_string())?;
        let channels = sc.channels();
        let sample_rate = sc.sample_rate();
        let spectrum = Arc::new(SharedSpectrum::default());
        let timecode = [Arc::new(SharedTimecode::default()), Arc::new(SharedTimecode::default())];
        let gain = Arc::new(AtomicU32::new(cfg.gain.to_bits()));
        let noise = Arc::new(AtomicU32::new(cfg.noise.to_bits()));
        let fft = cfg.fft.map(|c| c.filter(|&c| c < channels as usize));
        let open = |buffer_size| {
            let config = cpal::StreamConfig {
                channels,
                sample_rate,
                buffer_size,
            };
            let tap = Tap {
                fft,
                ltc: cfg.ltc,
                fps: cfg.ltc_fps,
                spectrum: spectrum.clone(),
                timecode: timecode.clone(),
                gain: gain.clone(),
                noise: noise.clone(),
            };
            match sc.sample_format() {
                cpal::SampleFormat::F32 => build::<f32>(&dev, &config, tap),
                cpal::SampleFormat::I16 => build::<i16>(&dev, &config, tap),
                cpal::SampleFormat::I32 => build::<i32>(&dev, &config, tap),
                cpal::SampleFormat::U16 => build::<u16>(&dev, &config, tap),
                cpal::SampleFormat::U8 => build::<u8>(&dev, &config, tap),
                f => Err(format!("unsupported sample format {f:?}")),
            }
        };
        // The device default can be large (PulseAudio: ~170 ms) → ask for small buffers.
        let stream = match cfg.buffer {
            0 => open(cpal::BufferSize::Default),
            n => open(cpal::BufferSize::Fixed(n)).or_else(|e| {
                log::warn!("audio input: buffer {n} refused ({e}), using the device default");
                open(cpal::BufferSize::Default)
            }),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        let device_name = dev
            .description()
            .map_or_else(|_| dev.to_string(), |x| x.name().to_owned());
        log::info!(
            "audio input: {device_name} ({channels} ch, {sample_rate} Hz, FFT {fft:?}, LTC {:?})",
            cfg.ltc
        );
        Ok(Self {
            _stream: stream,
            spectrum,
            timecode,
            gain,
            noise,
            device_name,
            channels,
            sample_rate,
        })
    }

    /// Change the gain without reopening.
    pub fn set_gain(&self, g: f32) {
        self.gain.store(g.to_bits(), Ordering::Relaxed);
    }

    /// Change the fan noise reduction without reopening (the learned floor stays).
    pub fn set_noise(&self, n: f32) {
        self.noise.store(n.to_bits(), Ordering::Relaxed);
    }
}

/// What the input callback feeds.
struct Tap {
    fft: Option<Option<usize>>,
    ltc: [Option<usize>; 2],
    fps: Fps,
    spectrum: Arc<SharedSpectrum>,
    timecode: [Arc<SharedTimecode>; 2],
    gain: Arc<AtomicU32>,
    noise: Arc<AtomicU32>,
}

fn build<T>(dev: &cpal::Device, config: &cpal::StreamConfig, tap: Tap) -> Result<cpal::Stream, String>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let mut an = Analyzer::new(config.sample_rate);
    let channels = config.channels as usize;
    let sr = config.sample_rate as f64;
    let mut ltc: [Option<LtcDecoder>; 2] = std::array::from_fn(|i| {
        tap.ltc[i]
            .filter(|&c| c < channels)
            .map(|_| LtcDecoder::new(config.sample_rate))
    });
    // Conversion buffer, grown on the first callbacks only.
    let mut buf: Vec<f32> = Vec::with_capacity(8192);
    dev.build_input_stream::<T, _, _>(
        *config,
        move |data: &[T], _| {
            // The callback runs right after the last sample arrived.
            let now = std::time::Instant::now();
            buf.clear();
            buf.extend(data.iter().map(|&s| s.to_sample::<f32>()));
            let frames = buf.len() / channels.max(1);
            for (k, dec) in ltc.iter_mut().enumerate() {
                let (Some(dec), Some(ch)) = (dec.as_mut(), tap.ltc[k]) else {
                    continue;
                };
                for (i, frame) in buf.chunks_exact(channels.max(1)).enumerate() {
                    if let Some(f) = dec.push(frame[ch]) {
                        let fps = if f.drop_frame { Fps::F2997Df } else { tap.fps };
                        let ago = (frames - i) as f64 / sr;
                        tap.timecode[k].store(Reading {
                            // The frame just ended: the next one is current.
                            secs: (f.tc.frame_number(fps) + 1) as f64 / fps.rate(),
                            fps,
                            at: now.checked_sub(std::time::Duration::from_secs_f64(ago)).unwrap_or(now),
                            locate: false,
                        });
                    }
                }
            }
            if let Some(ch) = tap.fft {
                an.gain = f32::from_bits(tap.gain.load(Ordering::Relaxed));
                an.noise = f32::from_bits(tap.noise.load(Ordering::Relaxed));
                an.push_interleaved(&buf, channels, ch);
                if an.take_fresh() {
                    tap.spectrum.store(an.spectrum());
                }
            }
        },
        |e| log::warn!("audio input: {e}"),
        None,
    )
    .map_err(|e| e.to_string())
}
