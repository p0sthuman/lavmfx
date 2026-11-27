//! Recording to a movie file: render / record presets (DXV, H.264, ProRes,
//! MJPEG) plus HAP, encoded with FFmpeg on a worker
//! thread. Frames come in as RGBA (any rate); the file gets a constant frame
//! rate (frames placed by wall-clock time, repeated over gaps, dropped when
//! early). Optional stereo audio (interleaved f32) is written as PCM in `.mov`,
//! AAC in `.mp4`.
//!
//! Also [`save_png`] for snapshots.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ff::format::Pixel;
use ffmpeg_next as ff;

use crate::live::{FrameSink, OutFrame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// FFmpeg's DXV encoder: DXT1 only ("Normal Quality, No Alpha").
    Dxv,
    Hap,
    HapAlpha,
    HapQ,
    H264,
    ProRes,
    Mjpeg,
}

/// One preset (name and codec / format).
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub name: &'static str,
    pub codec: Codec,
    /// "Codec" / "Format" fields (shown in Preferences).
    pub codec_name: &'static str,
    pub format: &'static str,
    /// None = can be encoded; Some(why not).
    pub unavailable: Option<&'static str>,
}

const NO_DXV_HQ: &str = "DXV 3 High Quality / alpha has no open encoder: use HAP Q / HAP Alpha";

/// The DXV / common presets first, then HAP.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "DXV 3 Normal Quality No Alpha",
        codec: Codec::Dxv,
        codec_name: "DXV 3",
        format: "Normal Quality, No Alpha",
        unavailable: None,
    },
    Preset {
        name: "DXV 3 Normal Quality With Alpha",
        codec: Codec::Dxv,
        codec_name: "DXV 3",
        format: "Normal Quality, With Alpha",
        unavailable: Some(NO_DXV_HQ),
    },
    Preset {
        name: "DXV 3 High Quality No Alpha",
        codec: Codec::Dxv,
        codec_name: "DXV 3",
        format: "High Quality, No Alpha",
        unavailable: Some(NO_DXV_HQ),
    },
    Preset {
        name: "DXV 3 High Quality With Alpha",
        codec: Codec::Dxv,
        codec_name: "DXV 3",
        format: "High Quality, With Alpha",
        unavailable: Some(NO_DXV_HQ),
    },
    Preset {
        name: "H264 Low Quality",
        codec: Codec::H264,
        codec_name: "H264",
        format: "Low Quality",
        unavailable: None,
    },
    Preset {
        name: "H264 Medium Quality",
        codec: Codec::H264,
        codec_name: "H264",
        format: "Medium Quality",
        unavailable: None,
    },
    Preset {
        name: "H264 High Quality",
        codec: Codec::H264,
        codec_name: "H264",
        format: "High Quality",
        unavailable: None,
    },
    Preset {
        name: "Motion JPEG",
        codec: Codec::Mjpeg,
        codec_name: "Motion JPEG",
        format: "Normal Quality",
        unavailable: None,
    },
    Preset {
        name: "ProRes 422 Normal Quality",
        codec: Codec::ProRes,
        codec_name: "ProRes",
        format: "422 Normal Quality",
        unavailable: None,
    },
    Preset {
        name: "ProRes 422 High Quality",
        codec: Codec::ProRes,
        codec_name: "ProRes",
        format: "422 High Quality",
        unavailable: None,
    },
    Preset {
        name: "ProRes 4444",
        codec: Codec::ProRes,
        codec_name: "ProRes",
        format: "4444",
        unavailable: None,
    },
    Preset {
        name: "HAP",
        codec: Codec::Hap,
        codec_name: "HAP",
        format: "Hap",
        unavailable: None,
    },
    Preset {
        name: "HAP Alpha",
        codec: Codec::HapAlpha,
        codec_name: "HAP",
        format: "Hap Alpha",
        unavailable: None,
    },
    Preset {
        name: "HAP Q",
        codec: Codec::HapQ,
        codec_name: "HAP",
        format: "Hap Q",
        unavailable: None,
    },
];

pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

impl Preset {
    /// File extension (H.264 → mp4, everything else mov).
    pub fn extension(&self) -> &'static str {
        if self.codec == Codec::H264 { "mp4" } else { "mov" }
    }
}

/// What to record.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordSettings {
    pub preset: String,
    /// Frames per second of the file.
    pub fps: u32,
    /// Audio: (sample rate, 16 or 24 bit); None = video only.
    pub audio: Option<(u32, u16)>,
}

/// Recorder state for the UI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecordStatus {
    pub frames: u64,
    /// Frames dropped because the encoder was behind.
    pub dropped: u64,
    pub seconds: f64,
    pub bytes: u64,
    pub error: Option<String>,
    pub finished: bool,
}

enum Msg {
    Video(OutFrame, i64),
    Audio(Vec<f32>),
    Finish,
}

/// A file being recorded. Dropping it finishes the file too.
pub struct Recorder {
    pub path: PathBuf,
    tx: Option<SyncSender<Msg>>,
    thread: Option<std::thread::JoinHandle<()>>,
    status: Arc<Mutex<RecordStatus>>,
    start: Instant,
    fps: u32,
    last_frame: Mutex<i64>,
    spare: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Recorder {
    /// Start writing `path` (`width × height` RGBA in).
    pub fn start(path: &Path, width: u32, height: u32, s: &RecordSettings) -> Result<Self, String> {
        let p = preset(&s.preset).ok_or_else(|| format!("unknown preset {}", s.preset))?;
        if let Some(why) = p.unavailable {
            return Err(why.into());
        }
        crate::ffmpeg::init_pub();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let (tx, rx) = sync_channel::<Msg>(8);
        let status = Arc::new(Mutex::new(RecordStatus::default()));
        let spare = Arc::new(Mutex::new(Vec::new()));
        // The encoder lives on its thread (FFmpeg contexts aren't Send); the open
        // result comes back right away.
        let (st, sp) = (status.clone(), spare.clone());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let (out, fps, audio) = (path.to_path_buf(), s.fps.max(1), s.audio);
        let thread = std::thread::Builder::new()
            .name("record".into())
            .spawn(move || match Encoder::open(&out, width, height, p, fps, audio) {
                Ok(enc) => {
                    let _ = ready_tx.send(Ok(()));
                    run(enc, rx, st, sp);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        ready_rx.recv().map_err(|_| "record thread died".to_string())??;
        Ok(Self {
            path: path.to_path_buf(),
            tx: Some(tx),
            thread: Some(thread),
            status,
            start: Instant::now(),
            fps: s.fps.max(1),
            last_frame: Mutex::new(-1),
            spare,
        })
    }

    pub fn status(&self) -> RecordStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Stereo interleaved samples at the settings' rate (ignored without audio).
    pub fn push_audio(&self, samples: Vec<f32>) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(Msg::Audio(samples));
        }
    }

    /// Offline rendering: frame `idx` of the file, waiting while the encoder is busy.
    pub fn send_indexed(&self, f: OutFrame, idx: i64) -> bool {
        self.tx.as_ref().is_some_and(|tx| tx.send(Msg::Video(f, idx)).is_ok())
    }

    /// Close the file (waits for the encoder).
    pub fn finish(mut self) -> RecordStatus {
        self.close();
        self.status()
    }

    fn close(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Msg::Finish);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.close();
    }
}

impl FrameSink for Recorder {
    fn buffer(&self, len: usize) -> Vec<u8> {
        let mut b = self
            .spare
            .lock()
            .map(|mut s| s.pop().unwrap_or_default())
            .unwrap_or_default();
        b.resize(len, 0);
        b
    }

    /// Placed by wall-clock time on the file's frame grid.
    fn send(&self, f: OutFrame) -> bool {
        let idx = (self.start.elapsed().as_secs_f64() * f64::from(self.fps)).round() as i64;
        let mut last = self.last_frame.lock().unwrap();
        if idx <= *last {
            return false; // two app frames on one file frame: keep the first
        }
        let Some(tx) = &self.tx else { return false };
        match tx.try_send(Msg::Video(f, idx)) {
            Ok(()) => {
                *last = idx;
                true
            }
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                if let Ok(mut s) = self.status.lock() {
                    s.dropped += 1;
                }
                false
            }
        }
    }

    fn status(&self) -> String {
        let s = Recorder::status(self);
        match &s.error {
            Some(e) => format!("Recording failed: {e}"),
            None => format!(
                "Recording {} · {:.1} s · {} frames · {:.1} MB{}",
                self.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                s.seconds,
                s.frames,
                s.bytes as f64 / 1e6,
                if s.dropped > 0 {
                    format!(" · {} dropped", s.dropped)
                } else {
                    String::new()
                }
            ),
        }
    }
}

// ------------------------------------------------------------------ encoder

struct Encoder {
    octx: ff::format::context::Output,
    video: ff::encoder::Video,
    vtb: ff::Rational,
    sws: Option<ff::software::scaling::Context>,
    enc_format: Pixel,
    size: (u32, u32),
    audio: Option<AudioEnc>,
    next_frame: i64,
    last: Option<ff::frame::Video>,
}

struct AudioEnc {
    enc: ff::encoder::Audio,
    tb: ff::Rational,
    format: ff::format::Sample,
    frame_size: usize,
    pending: Vec<f32>,
    pts: i64,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Encoder {
    fn open(path: &Path, w: u32, h: u32, p: &Preset, fps: u32, audio: Option<(u32, u16)>) -> Result<Self, String> {
        let mut octx = ff::format::output(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let global = octx.format().flags().contains(ff::format::Flags::GLOBAL_HEADER);
        // Video encoder by preset.
        let (names, format, opts): (&[&str], Pixel, Vec<(&str, &str)>) = match (p.codec, p.format) {
            (Codec::Dxv, _) => (&["dxv"], Pixel::RGBA, vec![("format", "dxt1")]),
            (Codec::Hap, _) => (&["hap"], Pixel::RGBA, vec![("format", "hap")]),
            (Codec::HapAlpha, _) => (&["hap"], Pixel::RGBA, vec![("format", "hap_alpha")]),
            (Codec::HapQ, _) => (&["hap"], Pixel::RGBA, vec![("format", "hap_q")]),
            (Codec::Mjpeg, _) => (&["mjpeg"], Pixel::YUVJ422P, vec![]),
            (Codec::ProRes, "4444") => (&["prores_ks"], Pixel::YUVA444P10LE, vec![("profile", "4444")]),
            (Codec::ProRes, f) if f.contains("High") => (&["prores_ks"], Pixel::YUV422P10LE, vec![("profile", "hq")]),
            (Codec::ProRes, _) => (&["prores_ks"], Pixel::YUV422P10LE, vec![("profile", "standard")]),
            (Codec::H264, _) => (&["h264_nvenc", "libx264"], Pixel::YUV420P, vec![]),
        };
        let quality = match p.format {
            "Low Quality" => 0,
            "Medium Quality" => 1,
            _ => 2,
        };
        let mut last_err = String::new();
        let mut opened = None;
        for name in names {
            let Some(codec) = ff::encoder::find_by_name(name) else {
                continue;
            };
            let mut e = ff::codec::context::Context::new_with_codec(codec)
                .encoder()
                .video()
                .map_err(err)?;
            e.set_width(w);
            e.set_height(h);
            e.set_format(format);
            e.set_time_base((1, fps as i32));
            e.set_frame_rate(Some((fps as i32, 1)));
            if p.codec == Codec::H264 || p.codec == Codec::Mjpeg {
                e.set_gop(fps);
            }
            if global {
                e.set_flags(ff::codec::Flags::GLOBAL_HEADER);
            }
            let mut d = ff::Dictionary::new();
            for (k, v) in &opts {
                d.set(k, v);
            }
            match *name {
                "libx264" => {
                    d.set("preset", ["veryfast", "fast", "medium"][quality]);
                    d.set("crf", ["28", "22", "17"][quality]);
                }
                "h264_nvenc" => {
                    d.set("preset", "p4");
                    d.set("rc", "vbr");
                    d.set("cq", ["30", "23", "18"][quality]);
                }
                _ => {}
            }
            if *name == "mjpeg" {
                // Fixed quality (qscale 3), as `ffmpeg -q:v 3`.
                unsafe {
                    let c = e.as_mut_ptr();
                    (*c).global_quality = 3 * ff::ffi::FF_QP2LAMBDA;
                    (*c).flags |= ff::ffi::AV_CODEC_FLAG_QSCALE as i32;
                }
            }
            match e.open_as_with(codec, d) {
                Ok(v) => {
                    opened = Some((codec, v));
                    break;
                }
                Err(er) => last_err = format!("{name}: {er}"),
            }
        }
        let (codec, video) = opened.ok_or_else(|| format!("no encoder for {} ({last_err})", p.name))?;
        let vtb = ff::Rational(1, fps as i32);
        {
            let mut st = octx.add_stream(codec).map_err(err)?;
            st.set_parameters(&video);
            st.set_time_base(vtb);
        }
        // Audio: PCM in mov, AAC in mp4.
        let audio = match audio {
            None => None,
            Some((rate, bits)) => {
                let (name, format) = if p.extension() == "mp4" {
                    ("aac", ff::format::Sample::F32(ff::format::sample::Type::Planar))
                } else if bits >= 24 {
                    ("pcm_s24le", ff::format::Sample::I32(ff::format::sample::Type::Packed))
                } else {
                    ("pcm_s16le", ff::format::Sample::I16(ff::format::sample::Type::Packed))
                };
                let codec = ff::encoder::find_by_name(name).ok_or_else(|| format!("no {name} encoder"))?;
                let mut a = ff::codec::context::Context::new_with_codec(codec)
                    .encoder()
                    .audio()
                    .map_err(err)?;
                a.set_rate(rate as i32);
                a.set_format(format);
                a.set_channel_layout(ff::ChannelLayout::STEREO);
                a.set_time_base((1, rate as i32));
                if global {
                    a.set_flags(ff::codec::Flags::GLOBAL_HEADER);
                }
                let mut d = ff::Dictionary::new();
                if name == "aac" {
                    d.set("b", "320k");
                }
                let enc = a.open_as_with(codec, d).map_err(|e| format!("{name}: {e}"))?;
                let frame_size = match enc.frame_size() {
                    0 => 1024,
                    n => n as usize,
                };
                {
                    let mut st = octx.add_stream(codec).map_err(err)?;
                    st.set_parameters(&enc);
                    st.set_time_base((1, rate as i32));
                }
                Some(AudioEnc {
                    enc,
                    tb: ff::Rational(1, rate as i32),
                    format,
                    frame_size,
                    pending: Vec::new(),
                    pts: 0,
                })
            }
        };
        octx.write_header().map_err(|e| format!("write header: {e}"))?;
        Ok(Self {
            octx,
            video,
            vtb,
            sws: None,
            enc_format: format,
            size: (w, h),
            audio,
            next_frame: 0,
            last: None,
        })
    }

    fn drain_video(&mut self) -> Result<u64, String> {
        let mut bytes = 0;
        let mut pkt = ff::Packet::empty();
        let ost_tb = self.octx.stream(0).map(|s| s.time_base()).unwrap_or(self.vtb);
        while self.video.receive_packet(&mut pkt).is_ok() {
            bytes += pkt.size() as u64;
            pkt.set_stream(0);
            pkt.rescale_ts(self.vtb, ost_tb);
            pkt.write_interleaved(&mut self.octx).map_err(err)?;
        }
        Ok(bytes)
    }

    fn drain_audio(&mut self) -> Result<u64, String> {
        let Some(a) = &mut self.audio else { return Ok(0) };
        let mut bytes = 0;
        let mut pkt = ff::Packet::empty();
        let ost_tb = self.octx.stream(1).map(|s| s.time_base()).unwrap_or(a.tb);
        while a.enc.receive_packet(&mut pkt).is_ok() {
            bytes += pkt.size() as u64;
            pkt.set_stream(1);
            pkt.rescale_ts(a.tb, ost_tb);
            pkt.write_interleaved(&mut self.octx).map_err(err)?;
        }
        Ok(bytes)
    }

    /// Encode `f` as file frame `idx` (repeats the previous frame over gaps, up to 1 s).
    fn video(&mut self, f: &OutFrame, idx: i64, fps: i64) -> Result<(u64, u64), String> {
        let (w, h) = self.size;
        let mut rgba = ff::frame::Video::new(Pixel::RGBA, w, h);
        let stride = rgba.stride(0);
        let row = (w * 4) as usize;
        {
            let dst = rgba.data_mut(0);
            for y in 0..h.min(f.height) as usize {
                let s = y * f.stride as usize;
                if let Some(src) = f.data.get(s..s + row) {
                    dst[y * stride..y * stride + row].copy_from_slice(src);
                }
            }
        }
        let frame = if self.enc_format == Pixel::RGBA {
            rgba
        } else {
            if self.sws.is_none() {
                self.sws = Some(
                    ff::software::scaling::Context::get(
                        Pixel::RGBA,
                        w,
                        h,
                        self.enc_format,
                        w,
                        h,
                        ff::software::scaling::Flags::BILINEAR,
                    )
                    .map_err(err)?,
                );
            }
            let mut out = ff::frame::Video::new(self.enc_format, w, h);
            self.sws.as_mut().unwrap().run(&rgba, &mut out).map_err(err)?;
            out
        };
        let mut bytes = 0;
        let mut frames = 0;
        // Repeat the last frame over a gap (short stalls keep a constant rate).
        if let Some(prev) = self.last.take() {
            let gap_end = idx.min(self.next_frame + fps);
            let mut prev = prev;
            while self.next_frame < gap_end {
                prev.set_pts(Some(self.next_frame));
                self.video.send_frame(&prev).map_err(err)?;
                bytes += self.drain_video()?;
                self.next_frame += 1;
                frames += 1;
            }
        }
        let mut frame = frame;
        let pts = idx.max(self.next_frame);
        frame.set_pts(Some(pts));
        self.video.send_frame(&frame).map_err(err)?;
        bytes += self.drain_video()?;
        self.next_frame = pts + 1;
        frames += 1;
        self.last = Some(frame);
        Ok((frames, bytes))
    }

    fn audio(&mut self, samples: &[f32]) -> Result<u64, String> {
        if let Some(a) = &mut self.audio {
            a.pending.extend_from_slice(samples);
        }
        let mut bytes = 0;
        loop {
            let Some(a) = &mut self.audio else { return Ok(bytes) };
            if a.pending.len() < a.frame_size * 2 {
                return Ok(bytes);
            }
            let chunk: Vec<f32> = a.pending.drain(..a.frame_size * 2).collect();
            let mut fr = ff::frame::Audio::new(a.format, a.frame_size, ff::ChannelLayout::STEREO);
            fr.set_rate(a.tb.1 as u32);
            match a.format {
                ff::format::Sample::F32(ff::format::sample::Type::Planar) => {
                    for c in 0..2 {
                        for (i, v) in fr.plane_mut::<f32>(c).iter_mut().enumerate() {
                            *v = chunk[i * 2 + c];
                        }
                    }
                }
                ff::format::Sample::I32(_) => {
                    let d = fr.data_mut(0);
                    for (i, s) in chunk.iter().enumerate() {
                        let v = (f64::from(s.clamp(-1.0, 1.0)) * 2_147_483_647.0) as i32;
                        d[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                    }
                }
                _ => {
                    let d = fr.data_mut(0);
                    for (i, s) in chunk.iter().enumerate() {
                        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                        d[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
                    }
                }
            }
            fr.set_pts(Some(a.pts));
            a.pts += a.frame_size as i64;
            a.enc.send_frame(&fr).map_err(err)?;
            bytes += self.drain_audio()?;
        }
    }

    fn finish(&mut self) -> Result<u64, String> {
        let mut bytes = 0;
        self.video.send_eof().map_err(err)?;
        bytes += self.drain_video()?;
        if let Some(a) = &mut self.audio {
            a.enc.send_eof().map_err(err)?;
            bytes += self.drain_audio()?;
        }
        self.octx.write_trailer().map_err(err)?;
        Ok(bytes)
    }
}

fn run(mut enc: Encoder, rx: Receiver<Msg>, status: Arc<Mutex<RecordStatus>>, spare: Arc<Mutex<Vec<Vec<u8>>>>) {
    let fps = i64::from(enc.vtb.1.max(1));
    let fail = |e: String, status: &Arc<Mutex<RecordStatus>>| {
        log::warn!("record: {e}");
        if let Ok(mut s) = status.lock() {
            s.error.get_or_insert(e);
        }
    };
    while let Ok(m) = rx.recv() {
        match m {
            Msg::Video(f, idx) => {
                match enc.video(&f, idx, fps) {
                    Ok((n, b)) => {
                        if let Ok(mut s) = status.lock() {
                            s.frames += n;
                            s.bytes += b;
                            s.seconds = enc.next_frame as f64 / fps as f64;
                        }
                    }
                    Err(e) => fail(e, &status),
                }
                if let Ok(mut sp) = spare.lock()
                    && sp.len() < 3
                {
                    sp.push(f.data);
                }
            }
            Msg::Audio(a) => match enc.audio(&a) {
                Ok(b) => {
                    if let Ok(mut s) = status.lock() {
                        s.bytes += b;
                    }
                }
                Err(e) => fail(e, &status),
            },
            Msg::Finish => break,
        }
    }
    match enc.finish() {
        Ok(b) => {
            if let Ok(mut s) = status.lock() {
                s.bytes += b;
                s.finished = true;
            }
        }
        Err(e) => fail(e, &status),
    }
}

/// A PNG of `width × height` RGBA (tight rows).
pub fn save_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    crate::ffmpeg::init_pub();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let codec = ff::encoder::find(ff::codec::Id::PNG).ok_or("no PNG encoder")?;
    let mut e = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .map_err(err)?;
    e.set_width(width);
    e.set_height(height);
    e.set_format(Pixel::RGBA);
    e.set_time_base((1, 1));
    let mut enc = e.open_as(codec).map_err(err)?;
    let mut f = ff::frame::Video::new(Pixel::RGBA, width, height);
    let stride = f.stride(0);
    let row = width as usize * 4;
    for y in 0..height as usize {
        f.data_mut(0)[y * stride..y * stride + row].copy_from_slice(&rgba[y * row..y * row + row]);
    }
    f.set_pts(Some(0));
    enc.send_frame(&f).map_err(err)?;
    enc.send_eof().map_err(err)?;
    let mut out = Vec::new();
    let mut pkt = ff::Packet::empty();
    while enc.receive_packet(&mut pkt).is_ok() {
        out.extend_from_slice(pkt.data().unwrap_or_default());
    }
    std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))
}

/// `<dir>/<stem> YYYY-MM-DD HH-MM-SS.<ext>` (local time not needed: UTC is
/// fine for unique, sortable names).
pub fn timestamped(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let name = format!(
        "{stem} {y:04}-{m:02}-{d:02} {:02}-{:02}-{:02}.{ext}",
        sod / 3600,
        sod / 60 % 60,
        sod % 60
    );
    dir.join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, c: [u8; 4]) -> OutFrame {
        OutFrame {
            data: c.repeat((w * h) as usize),
            width: w,
            height: h,
            stride: w * 4,
            rate: (30, 1),
        }
    }

    /// Every available preset writes a file FFmpeg can open with the right size / codec.
    #[test]
    fn every_preset_records() {
        let dir = std::env::temp_dir().join(format!("lavmfx-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for p in PRESETS.iter().filter(|p| p.unavailable.is_none()) {
            let path = dir.join(format!("{}.{}", p.name, p.extension()));
            let s = RecordSettings {
                preset: p.name.into(),
                fps: 30,
                audio: Some((48_000, 16)),
            };
            let (w, h) = (128, 64);
            let mut enc = Encoder::open(&path, w, h, p, 30, s.audio).unwrap_or_else(|e| panic!("{}: {e}", p.name));
            for i in 0..15 {
                enc.video(&frame(w, h, [200, 50, (i * 10) as u8, 255]), i, 30).unwrap();
                enc.audio(&vec![0.1; 3200]).unwrap();
            }
            enc.finish().unwrap();
            drop(enc);
            let ictx = ff::format::input(&path).unwrap_or_else(|e| panic!("{}: {e}", p.name));
            let v = ictx.streams().best(ff::media::Type::Video).unwrap();
            let params = ff::codec::context::Context::from_parameters(v.parameters()).unwrap();
            let dec = params.decoder().video().unwrap();
            assert_eq!((dec.width(), dec.height()), (w, h), "{}", p.name);
            assert!(
                ictx.streams().best(ff::media::Type::Audio).is_some(),
                "{}: audio",
                p.name
            );
            eprintln!(
                "{}: {} bytes, codec {:?}",
                p.name,
                std::fs::metadata(&path).unwrap().len(),
                dec.id()
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn png_snapshot() {
        let p = std::env::temp_dir().join(format!("lavmfx-snap-{}.png", std::process::id()));
        save_png(&p, 4, 2, &[255, 0, 0, 255].repeat(8)).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(&bytes[1..4], b"PNG");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn timestamped_names() {
        let p = timestamped(Path::new("/x"), "LAVMFX Recording", "mov");
        let n = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(n.starts_with("LAVMFX Recording 20") && n.ends_with(".mov"), "{n}");
    }
}
