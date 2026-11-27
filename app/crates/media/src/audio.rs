//! A clip's audio track, decoded whole into memory (stereo i16 at the file's
//! own rate). Clips are short and get seeked, looped, reversed and scrubbed
//! all the time; random access into RAM makes all of that trivial for the
//! mixer, which also does the rate conversion while reading.

use std::path::Path;
use std::time::Duration;

use ffmpeg_next as ff;

use crate::{ColorInfo, FrameFormat, MediaError, MediaInfo, Plane, Result, VideoDecoder, VideoFrame};

fn init() {
    let _ = ff::init();
}

/// Decoded audio: interleaved stereo.
#[derive(Debug, Clone, Default)]
pub struct Pcm {
    pub rate: u32,
    /// L, R, L, R, …
    pub data: Vec<i16>,
}

impl Pcm {
    pub fn frames(&self) -> usize {
        self.data.len() / 2
    }

    pub fn duration(&self) -> f64 {
        self.frames() as f64 / self.rate.max(1) as f64
    }

    /// Stereo frame `i` as f32 (wraps around: clips loop).
    #[inline]
    pub fn frame(&self, i: usize) -> (f32, f32) {
        let n = self.frames();
        if n == 0 {
            return (0.0, 0.0);
        }
        let i = (i % n) * 2;
        const K: f32 = 1.0 / 32768.0;
        (self.data[i] as f32 * K, self.data[i + 1] as f32 * K)
    }

    /// Waveform overview: the loudest sample (|L| or |R|) in each of `n`
    /// equal slices, scaled so the loudest slice is 1 (silence stays 0).
    pub fn peaks(&self, n: usize) -> Vec<f32> {
        let frames = self.frames();
        if n == 0 || frames == 0 {
            return vec![0.0; n];
        }
        let mut out: Vec<f32> = (0..n)
            .map(|i| {
                let (a, b) = (
                    i * frames / n,
                    ((i + 1) * frames / n).max(i * frames / n + 1).min(frames),
                );
                self.data[a * 2..b * 2]
                    .iter()
                    .map(|s| s.unsigned_abs())
                    .max()
                    .unwrap_or(0) as f32
                    / 32768.0
            })
            .collect();
        let top = out.iter().copied().fold(0.0f32, f32::max);
        if top > 1e-4 {
            out.iter_mut().for_each(|v| *v /= top);
        }
        out
    }

    /// Linear interpolation at fractional frame `pos` (wrapped).
    #[inline]
    pub fn at(&self, pos: f64) -> (f32, f32) {
        let n = self.frames();
        if n == 0 {
            return (0.0, 0.0);
        }
        let p = pos.rem_euclid(n as f64);
        let i = p.floor() as usize;
        let t = (p - i as f64) as f32;
        let (a, b) = (self.frame(i), self.frame(i + 1));
        (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
    }
}

/// Longest audio track kept in memory (longer ones play silent).
pub const MAX_SECONDS: f64 = 30.0 * 60.0;

/// Decode the best audio stream of `path`; `Ok(None)` if it has none.
pub fn decode(path: &Path) -> Result<Option<Pcm>> {
    let err = |source: ff::Error| MediaError::Ffmpeg {
        path: path.into(),
        source,
    };
    init();
    let mut input = ff::format::input(path).map_err(err)?;
    let Some(stream) = input.streams().best(ff::media::Type::Audio) else {
        return Ok(None);
    };
    let index = stream.index();
    let ctx = ff::codec::context::Context::from_parameters(stream.parameters()).map_err(err)?;
    let mut dec = ctx.decoder().audio().map_err(err)?;
    let rate = dec.rate();
    if rate == 0 {
        return Ok(None);
    }
    let est = input.duration().max(0) as f64 / f64::from(ff::ffi::AV_TIME_BASE);
    if est > MAX_SECONDS {
        log::info!("{}: audio longer than {MAX_SECONDS} s, not loaded", path.display());
        return Ok(None);
    }
    let mut pcm = Pcm {
        rate,
        data: Vec::with_capacity((est * rate as f64 * 2.0) as usize + 4096),
    };
    let stereo = ff::ChannelLayout::STEREO;
    let packed_f32 = ff::format::Sample::F32(ff::format::sample::Type::Packed);
    let mut swr: Option<ff::software::resampling::Context> = None;
    let mut decoded = ff::frame::Audio::empty();
    type Swr = Option<ff::software::resampling::Context>;
    let push = |frame: &ff::frame::Audio, swr: &mut Swr, data: &mut Vec<i16>| -> Result<()> {
        if swr.is_none() {
            let layout = if frame.channel_layout().is_empty() {
                ff::ChannelLayout::default(frame.channels() as i32)
            } else {
                frame.channel_layout()
            };
            *swr = Some(
                ff::software::resampling::Context::get(frame.format(), layout, rate, packed_f32, stereo, rate)
                    .map_err(err)?,
            );
        }
        let mut out = ff::frame::Audio::empty();
        swr.as_mut().unwrap().run(frame, &mut out).map_err(err)?;
        let n = out.samples() * 2;
        let bytes = out.data(0);
        let floats: &[f32] = bytemuck_f32(&bytes[..(n * 4).min(bytes.len())]);
        data.extend(floats.iter().map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16));
        Ok(())
    };
    for (s, packet) in input.packets() {
        if s.index() != index {
            continue;
        }
        if dec.send_packet(&packet).is_err() {
            continue;
        }
        while dec.receive_frame(&mut decoded).is_ok() {
            push(&decoded, &mut swr, &mut pcm.data)?;
        }
        if pcm.data.len() as f64 / 2.0 / rate as f64 > MAX_SECONDS {
            break;
        }
    }
    let _ = dec.send_eof();
    while dec.receive_frame(&mut decoded).is_ok() {
        push(&decoded, &mut swr, &mut pcm.data)?;
    }
    pcm.data.shrink_to_fit();
    Ok(Some(pcm))
}

/// Size of the (transparent) picture an audio file shows as.
const AUDIO_ONLY_SIZE: u32 = 16;

/// An audio file played as a clip: a "video" decoder whose only frame (one
/// per pass) is fully transparent, so the layer shows nothing while the
/// clip's transport, looping, play modes and audio work as for any clip.
pub struct AudioOnlyDecoder {
    info: MediaInfo,
    sent: bool,
}

impl AudioOnlyDecoder {
    pub fn open(path: &Path) -> Result<Self> {
        let err = |source: ff::Error| MediaError::Ffmpeg {
            path: path.into(),
            source,
        };
        init();
        let input = ff::format::input(path).map_err(err)?;
        let stream = input
            .streams()
            .best(ff::media::Type::Audio)
            .ok_or_else(|| MediaError::NoDecoder(path.into()))?;
        let duration = if stream.duration() > 0 {
            Duration::from_secs_f64(stream.duration() as f64 * f64::from(stream.time_base()))
        } else if input.duration() > 0 {
            Duration::from_micros(input.duration() as u64)
        } else {
            Duration::ZERO
        };
        let codec = ff::decoder::find(stream.parameters().id()).map_or_else(|| "audio".into(), |c| c.name().to_owned());
        Ok(Self {
            info: MediaInfo {
                path: path.into(),
                width: AUDIO_ONLY_SIZE,
                height: AUDIO_ONLY_SIZE,
                frame_rate: (30, 1),
                duration,
                codec,
                has_audio: true,
                decode_path: "audio only".into(),
            },
            sent: false,
        })
    }
}

impl VideoDecoder for AudioOnlyDecoder {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn seek(&mut self, _t: Duration) -> Result<()> {
        self.sent = false;
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Option<VideoFrame>> {
        if std::mem::replace(&mut self.sent, true) {
            return Ok(None);
        }
        let n = AUDIO_ONLY_SIZE;
        Ok(Some(VideoFrame {
            pts: Duration::ZERO,
            width: n,
            height: n,
            format: FrameFormat::Rgba8,
            color: ColorInfo::default(),
            planes: vec![Plane {
                data: vec![0; (n * n * 4) as usize],
                stride: n * 4,
            }],
        }))
    }
}

/// View bytes as f32 (ffmpeg's buffers are aligned).
fn bytemuck_f32(b: &[u8]) -> &[f32] {
    let n = b.len() / 4;
    if n == 0 || !(b.as_ptr() as usize).is_multiple_of(std::mem::align_of::<f32>()) {
        return &[];
    }
    // SAFETY: aligned (checked), length in whole f32s, any bit pattern is a valid f32.
    unsafe { std::slice::from_raw_parts(b.as_ptr().cast::<f32>(), n) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_are_normalised_maxima() {
        let p = Pcm {
            rate: 4,
            data: vec![0, 0, 8192, -16384, 0, 0, 0, 1000],
        };
        let k = p.peaks(2);
        assert_eq!(k.len(), 2);
        assert!(
            (k[0] - 1.0).abs() < 1e-6 && (k[1] - 1000.0 / 16384.0).abs() < 1e-4,
            "{k:?}"
        );
        assert_eq!(Pcm::default().peaks(3), [0.0; 3]);
    }

    #[test]
    fn interpolates_and_wraps() {
        let p = Pcm {
            rate: 4,
            data: vec![0, 0, 16384, -16384, 32767, 0, 0, 0],
        };
        assert_eq!(p.frames(), 4);
        assert_eq!(p.duration(), 1.0);
        let (l, r) = p.at(1.5);
        assert!((l - 0.75).abs() < 1e-3 && (r + 0.25).abs() < 1e-3, "{l} {r}");
        assert_eq!(p.at(4.0), p.at(0.0), "wraps");
        assert_eq!(p.at(-3.0), p.at(1.0), "wraps backwards");
    }
}
