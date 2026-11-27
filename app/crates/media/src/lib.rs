//! Media decoding.
//!
//! [`open`] returns a [`VideoDecoder`] backed by FFmpeg (with hardware
//! decoding where available). HAP is special-cased: packets are unwrapped by
//! [`hap`] and the BCn blocks go straight to the GPU. [`Player`] runs a
//! decoder on its own thread and hands out frames for a playback position.

use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod audio;
pub mod capture;
mod ffmpeg;
pub mod hap;
pub mod live;
pub mod ndi;
mod player;
pub mod record;
pub mod share;

pub use ffmpeg::{FfmpegDecoder, Image, JpegDecoder, thumbnail};
pub use player::Player;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("no decoder available for {0}")]
    NoDecoder(PathBuf),
    #[error("{path}: {source}")]
    Ffmpeg {
        path: PathBuf,
        source: ::ffmpeg_next::Error,
    },
    #[error("decode: {0}")]
    Decode(String),
    #[error(transparent)]
    Hap(#[from] hap::HapError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(
        "{0}: the disk can't read this file (input/output error at {1} MB): the drive or its file system is damaged; the file itself may be fine (check it on Windows / run chkdsk)"
    )]
    Unreadable(PathBuf, u64),
}

/// After FFmpeg failed: sample-read the file (16 spots + its end, 64 KB each)
/// to tell a damaged disk (EIO, e.g. a corrupt NTFS volume) from a bad file.
fn unreadable(path: &Path) -> Option<MediaError> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut buf = vec![0u8; 64 * 1024];
    let spots = (0..16)
        .map(|i| len / 16 * i)
        .chain([len.saturating_sub(buf.len() as u64)]);
    for at in spots {
        let r = f.seek(SeekFrom::Start(at)).and_then(|_| f.read(&mut buf));
        if let Err(e) = r
            && e.raw_os_error() == Some(5)
        {
            return Some(MediaError::Unreadable(path.to_owned(), at / 1_000_000));
        }
    }
    None
}

/// FFmpeg's open error, or [`MediaError::Unreadable`] when the disk is the cause.
fn open_error(path: &Path, e: MediaError) -> MediaError {
    match e {
        MediaError::Ffmpeg { .. } => unreadable(path).unwrap_or(e),
        e => e,
    }
}

pub type Result<T> = std::result::Result<T, MediaError>;

/// Static information about an opened file.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaInfo {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Frames per second as num/den.
    pub frame_rate: (i32, i32),
    pub duration: Duration,
    pub codec: String,
    pub has_audio: bool,
    /// How frames are decoded, e.g. "cuda", "vaapi", "software", "HAP native BC".
    pub decode_path: String,
}

impl MediaInfo {
    pub fn fps(&self) -> f64 {
        let (n, d) = self.frame_rate;
        if n > 0 && d > 0 { n as f64 / d as f64 } else { 30.0 }
    }

    pub fn frame_duration(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.fps())
    }
}

/// How a frame's pixels are laid out, which decides the GPU upload path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameFormat {
    Rgba8,
    /// Planar YUV 4:2:0 (Y, U, V planes), converted on the GPU.
    Yuv420p,
    /// Semi-planar YUV 4:2:0 (Y plane, interleaved UV plane): hw decoder output.
    Nv12,
    /// GPU-compressed (HAP / DXV): uploaded directly as BC1/BC3/BC7.
    Bc1,
    Bc3,
    Bc7,
    /// HAP Q: scaled YCoCg stored in BC3, converted on the GPU.
    Bc3YCoCg,
}

impl FrameFormat {
    pub fn is_bc(self) -> bool {
        matches!(self, Self::Bc1 | Self::Bc3 | Self::Bc7 | Self::Bc3YCoCg)
    }
}

/// YCbCr → RGB matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum YuvMatrix {
    Bt601,
    #[default]
    Bt709,
    Bt2020,
    /// YCgCo (DXV3 high quality): U plane = Cg, V plane = Co, full range.
    YCgCo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorInfo {
    pub matrix: YuvMatrix,
    /// true = 0..255 ("JPEG"), false = 16..235 ("MPEG").
    pub full_range: bool,
}

/// One plane of pixel data, rows `stride` bytes apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plane {
    pub data: Vec<u8>,
    pub stride: u32,
}

/// One decoded video frame in CPU memory.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub pts: Duration,
    pub width: u32,
    pub height: u32,
    pub format: FrameFormat,
    pub color: ColorInfo,
    pub planes: Vec<Plane>,
}

pub trait VideoDecoder: Send {
    fn info(&self) -> &MediaInfo;
    /// Seek so that the next frame is the one showing at `t`.
    fn seek(&mut self, t: Duration) -> Result<()>;
    /// Next frame in presentation order; `None` at end of stream.
    fn next_frame(&mut self) -> Result<Option<VideoFrame>>;
}

/// Hardware decoding preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HwAccel {
    /// Try the platform's accelerators in order, fall back to software.
    #[default]
    Auto,
    None,
}

impl HwAccel {
    /// `LAVMFX_HWACCEL=none` disables hardware decoding.
    pub fn from_env() -> Self {
        match std::env::var("LAVMFX_HWACCEL").as_deref() {
            Ok("none" | "off" | "0") => Self::None,
            _ => Self::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenOptions {
    pub hwaccel: HwAccel,
    /// The GPU can sample BCn textures: decode HAP to raw blocks.
    pub native_bc: bool,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            hwaccel: HwAccel::from_env(),
            native_bc: true,
        }
    }
}

/// Open a decoder for `path` (an audio file opens as an
/// [`audio::AudioOnlyDecoder`]: transparent picture, the audio plays).
pub fn open(path: &Path, opts: OpenOptions) -> Result<Box<dyn VideoDecoder>> {
    if is_audio_file(path) {
        return Ok(Box::new(
            audio::AudioOnlyDecoder::open(path).map_err(|e| open_error(path, e))?,
        ));
    }
    Ok(Box::new(
        FfmpegDecoder::open(path, opts).map_err(|e| open_error(path, e))?,
    ))
}

/// File extensions the clip browser offers.
pub const VIDEO_EXTENSIONS: &[&str] = &["mov", "mp4", "avi", "mkv", "webm", "m4v", "dxv"];

/// Audio files that load as audio-only clips.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "aif", "aiff", "flac", "ogg", "oga", "opus", "m4a", "aac", "wma",
];

fn has_extension(path: &Path, list: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| list.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

pub fn is_video_file(path: &Path) -> bool {
    has_extension(path, VIDEO_EXTENSIONS)
}

pub fn is_audio_file(path: &Path) -> bool {
    has_extension(path, AUDIO_EXTENSIONS)
}

/// A file that can go on a clip slot (video or audio).
pub fn is_media_file(path: &Path) -> bool {
    is_video_file(path) || is_audio_file(path)
}

/// Read a file's [`MediaInfo`] without setting up hardware decoding.
pub fn probe(path: &Path) -> Result<MediaInfo> {
    if is_audio_file(path) {
        let d = audio::AudioOnlyDecoder::open(path).map_err(|e| open_error(path, e))?;
        return Ok(d.info().clone());
    }
    let d = FfmpegDecoder::open(
        path,
        OpenOptions {
            hwaccel: HwAccel::None,
            native_bc: true,
        },
    )
    .map_err(|e| open_error(path, e))?;
    Ok(d.info().clone())
}
