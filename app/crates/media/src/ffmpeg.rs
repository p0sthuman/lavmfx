//! FFmpeg-backed decoder.
//!
//! Hardware decoding: the decoder context gets a hw device (CUDA/NVDEC or
//! VAAPI on Linux, D3D11VA on Windows, VideoToolbox on macOS); frames are
//! downloaded as NV12 and converted to RGB on the GPU. Zero-copy interop
//! (keeping frames on the GPU) is a later optimisation.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Once;
use std::time::Duration;

use ff::ffi;
use ff::format::Pixel;
use ffmpeg_next as ff;

use crate::{
    ColorInfo, FrameFormat, HwAccel, MediaError, MediaInfo, OpenOptions, Plane, Result, VideoDecoder, VideoFrame,
    YuvMatrix, hap,
};

/// FFmpeg set up (log level) for other modules.
pub(crate) fn init_pub() {
    init();
}

fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = ff::init();
        ff::util::log::set_level(ff::util::log::Level::Error);
    });
}

#[cfg(target_os = "linux")]
const HW_TYPES: &[ffi::AVHWDeviceType] = &[
    ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
    ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
];
#[cfg(target_os = "windows")]
const HW_TYPES: &[ffi::AVHWDeviceType] = &[
    ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
    ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
];
#[cfg(target_os = "macos")]
const HW_TYPES: &[ffi::AVHWDeviceType] = &[ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX];
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
const HW_TYPES: &[ffi::AVHWDeviceType] = &[];

enum Mode {
    /// Regular FFmpeg decode. `hw_format` is set when a hw device is attached.
    Decode {
        dec: ff::decoder::Video,
        hw_format: Option<Pixel>,
        eof_sent: bool,
    },
    /// HAP: parse packets ourselves, output BCn.
    Hap,
}

pub struct FfmpegDecoder {
    info: MediaInfo,
    input: ff::format::context::Input,
    stream: usize,
    time_base: ff::Rational,
    start_pts: i64,
    mode: Mode,
    /// Drop frames that end before this (accurate seek).
    skip_until: Option<Duration>,
    frame: ff::frame::Video,
    sw_frame: ff::frame::Video,
    scaler: Option<(Pixel, u32, u32, ff::software::scaling::Context)>,
    rgba: ff::frame::Video,
}

// The FFmpeg contexts are only touched from the thread that owns the decoder.
unsafe impl Send for FfmpegDecoder {}

fn ff_err(path: &Path) -> impl FnOnce(ff::Error) -> MediaError + '_ {
    move |source| MediaError::Ffmpeg {
        path: path.to_owned(),
        source,
    }
}

impl FfmpegDecoder {
    pub fn open(path: &Path, opts: OpenOptions) -> Result<Self> {
        init();
        let input = ff::format::input(path).map_err(ff_err(path))?;
        let stream = input
            .streams()
            .best(ff::media::Type::Video)
            .ok_or_else(|| MediaError::NoDecoder(path.into()))?;
        let index = stream.index();
        let time_base = stream.time_base();
        let start_pts = if stream.start_time() == ffi::AV_NOPTS_VALUE {
            0
        } else {
            stream.start_time()
        };
        let params = stream.parameters();
        let codec_id = params.id();
        let rate = [stream.avg_frame_rate(), stream.rate()]
            .into_iter()
            .find(|r| r.numerator() > 0 && r.denominator() > 0)
            .unwrap_or(ff::Rational::new(30, 1));
        let duration = if stream.duration() > 0 {
            Duration::from_secs_f64(stream.duration() as f64 * f64::from(time_base))
        } else if input.duration() > 0 {
            Duration::from_micros(input.duration() as u64)
        } else {
            Duration::ZERO
        };
        let has_audio = input.streams().best(ff::media::Type::Audio).is_some();

        let mut ctx = ff::codec::context::Context::from_parameters(params).map_err(ff_err(path))?;
        let codec = ff::decoder::find(codec_id).ok_or_else(|| MediaError::NoDecoder(path.into()))?;
        let (width, height) = unsafe {
            let p = ctx.as_ptr();
            ((*p).width as u32, (*p).height as u32)
        };

        let (mode, decode_path) = if codec_id == ff::codec::Id::HAP && opts.native_bc {
            (Mode::Hap, "HAP native BC".to_string())
        } else {
            let mut hw = None;
            // Only codecs GPU decoders handle reliably: NVDEC's MJPEG fails here
            // (CUDA_ERROR_NO_DEVICE) and FFmpeg logs it on every open before
            // falling back; DXV / ProRes / PNG have no GPU decoder anyway.
            use ff::codec::Id;
            let gpu_codec = matches!(
                codec_id,
                Id::H264 | Id::HEVC | Id::VP8 | Id::VP9 | Id::AV1 | Id::MPEG2VIDEO | Id::MPEG4 | Id::VC1
            );
            if opts.hwaccel == HwAccel::Auto && gpu_codec {
                hw = HW_TYPES.iter().find_map(|&t| unsafe { attach_hw(&mut ctx, &codec, t) });
            }
            if hw.is_none() {
                ctx.set_threading(ff::threading::Config {
                    kind: ff::threading::Type::Frame,
                    count: 0,
                });
            }
            let dec = ctx
                .decoder()
                .open_as(codec)
                .and_then(|o| o.video())
                .map_err(ff_err(path))?;
            let (label, hw_format) = match hw {
                Some((name, f)) => (name, Some(f)),
                None => ("software".into(), None),
            };
            (
                Mode::Decode {
                    dec,
                    hw_format,
                    eof_sent: false,
                },
                label,
            )
        };

        let info = MediaInfo {
            path: path.to_owned(),
            width,
            height,
            frame_rate: (rate.numerator(), rate.denominator()),
            duration,
            codec: codec.name().to_string(),
            has_audio,
            decode_path,
        };
        log::info!(
            "opened {} ({}x{} {} via {})",
            path.display(),
            width,
            height,
            info.codec,
            info.decode_path
        );

        Ok(Self {
            info,
            input,
            stream: index,
            time_base,
            start_pts,
            mode,
            skip_until: None,
            frame: ff::frame::Video::empty(),
            sw_frame: ff::frame::Video::empty(),
            scaler: None,
            rgba: ff::frame::Video::empty(),
        })
    }

    fn pts_to_duration(&self, pts: Option<i64>) -> Duration {
        let pts = pts.unwrap_or(self.start_pts) - self.start_pts;
        Duration::from_secs_f64((pts as f64 * f64::from(self.time_base)).max(0.0))
    }

    fn path(&self) -> PathBuf {
        self.info.path.clone()
    }

    /// Read the next packet of our stream. `None` at end of file.
    fn read_packet(&mut self) -> Result<Option<ff::Packet>> {
        let mut pkt = ff::Packet::empty();
        loop {
            match pkt.read(&mut self.input) {
                Ok(()) if pkt.stream() == self.stream => return Ok(Some(pkt)),
                Ok(()) => continue,
                Err(ff::Error::Eof) => return Ok(None),
                Err(e) => return Err(ff_err(&self.path())(e)),
            }
        }
    }

    fn next_hap(&mut self) -> Result<Option<VideoFrame>> {
        let Some(pkt) = self.read_packet()? else {
            return Ok(None);
        };
        let tex = hap::decode(pkt.data().unwrap_or_default(), self.info.width, self.info.height)?;
        let stride = (self.info.width.div_ceil(4) as usize * hap::block_bytes(tex.format)) as u32;
        Ok(Some(VideoFrame {
            pts: self.pts_to_duration(pkt.pts()),
            width: self.info.width,
            height: self.info.height,
            format: tex.format,
            color: ColorInfo::default(),
            planes: vec![Plane { data: tex.data, stride }],
        }))
    }

    /// Next decoded frame left in `self.frame`. Returns false at end of stream.
    fn decode_next(&mut self) -> Result<bool> {
        loop {
            let Mode::Decode { dec, eof_sent, .. } = &mut self.mode else {
                unreachable!()
            };
            match dec.receive_frame(&mut self.frame) {
                Ok(()) => return Ok(true),
                Err(ff::Error::Eof) => return Ok(false),
                Err(ff::Error::Other { errno }) if errno == ff::error::EAGAIN => {
                    if *eof_sent {
                        return Ok(false);
                    }
                }
                Err(e) => return Err(MediaError::Decode(e.to_string())),
            }
            match self.read_packet()? {
                Some(pkt) => {
                    let Mode::Decode { dec, .. } = &mut self.mode else {
                        unreachable!()
                    };
                    // Corrupt packets are skipped rather than ending playback.
                    if let Err(e) = dec.send_packet(&pkt) {
                        log::debug!("send_packet: {e}");
                    }
                }
                None => {
                    let Mode::Decode { dec, eof_sent, .. } = &mut self.mode else {
                        unreachable!()
                    };
                    *eof_sent = true;
                    let _ = dec.send_eof();
                }
            }
        }
    }

    /// Convert the frame in `self.frame` to a [`VideoFrame`].
    fn convert(&mut self) -> Result<VideoFrame> {
        let Mode::Decode { hw_format, .. } = &self.mode else {
            unreachable!()
        };
        let src = if Some(self.frame.format()) == *hw_format {
            let r = unsafe { ffi::av_hwframe_transfer_data(self.sw_frame.as_mut_ptr(), self.frame.as_ptr(), 0) };
            if r < 0 {
                return Err(MediaError::Decode(format!("hw frame download failed ({r})")));
            }
            unsafe {
                ffi::av_frame_copy_props(self.sw_frame.as_mut_ptr(), self.frame.as_ptr());
            }
            &self.sw_frame
        } else {
            &self.frame
        };

        let pts = self.pts_to_duration(src.timestamp());
        let (w, h) = (src.width(), src.height());
        let color = ColorInfo {
            matrix: match src.color_space() {
                ff::color::Space::BT470BG | ff::color::Space::SMPTE170M | ff::color::Space::FCC => YuvMatrix::Bt601,
                ff::color::Space::BT2020NCL | ff::color::Space::BT2020CL => YuvMatrix::Bt2020,
                ff::color::Space::BT709 => YuvMatrix::Bt709,
                ff::color::Space::YCGCO => YuvMatrix::YCgCo,
                _ if h < 720 => YuvMatrix::Bt601,
                _ => YuvMatrix::Bt709,
            },
            full_range: src.color_space() == ff::color::Space::YCGCO
                || src.color_range() == ff::color::Range::JPEG
                || matches!(src.format(), Pixel::YUVJ420P | Pixel::YUVJ422P | Pixel::YUVJ444P),
        };

        let copy = |f: &ff::frame::Video, n: usize| -> Vec<Plane> {
            (0..n)
                .map(|i| Plane {
                    data: f.data(i).to_vec(),
                    stride: f.stride(i) as u32,
                })
                .collect()
        };
        let (format, planes) = match src.format() {
            Pixel::YUV420P | Pixel::YUVJ420P => (FrameFormat::Yuv420p, copy(src, 3)),
            Pixel::NV12 => (FrameFormat::Nv12, copy(src, 2)),
            Pixel::RGBA => (FrameFormat::Rgba8, copy(src, 1)),
            other => {
                // Everything else (10-bit, 4:2:2, 4:4:4, RGB, DXV …) → RGBA on the CPU.
                let stale = !matches!(&self.scaler, Some((f, sw, sh, _)) if (*f, *sw, *sh) == (other, w, h));
                if stale {
                    let ctx = ff::software::scaling::Context::get(
                        other,
                        w,
                        h,
                        Pixel::RGBA,
                        w,
                        h,
                        ff::software::scaling::Flags::BILINEAR,
                    )
                    .map_err(|e| MediaError::Decode(format!("swscale {other:?}: {e}")))?;
                    self.scaler = Some((other, w, h, ctx));
                }
                let (.., sws) = self.scaler.as_mut().unwrap();
                sws.run(src, &mut self.rgba)
                    .map_err(|e| MediaError::Decode(e.to_string()))?;
                (FrameFormat::Rgba8, copy(&self.rgba, 1))
            }
        };
        Ok(VideoFrame {
            pts,
            width: w,
            height: h,
            format,
            color,
            planes,
        })
    }

    fn keep(&self, pts: Duration) -> bool {
        match self.skip_until {
            Some(t) => pts + self.info.frame_duration() > t,
            None => true,
        }
    }
}

impl VideoDecoder for FfmpegDecoder {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn seek(&mut self, t: Duration) -> Result<()> {
        // Seek to the keyframe at or before t, then decode forward to t. The
        // demuxer may land on the frame *after* t (some .mov indexes round to
        // the nearest frame), so aim one frame early.
        let ts = t.saturating_sub(self.info.frame_duration()).as_micros() as i64;
        if let Err(e) = self.input.seek(ts, ..ts + 1) {
            log::debug!("seek {t:?}: {e}; retrying from start");
            self.input.seek(i64::MIN, ..).map_err(ff_err(&self.info.path.clone()))?;
        }
        if let Mode::Decode { dec, eof_sent, .. } = &mut self.mode {
            dec.flush();
            *eof_sent = false;
        }
        self.skip_until = (t > Duration::ZERO).then_some(t);
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Option<VideoFrame>> {
        loop {
            let frame = match self.mode {
                Mode::Hap => self.next_hap()?,
                Mode::Decode { .. } => {
                    if !self.decode_next()? {
                        None
                    } else {
                        let pts = self.pts_to_duration(self.frame.timestamp());
                        if !self.keep(pts) {
                            continue; // skip the conversion for frames we drop
                        }
                        Some(self.convert()?)
                    }
                }
            };
            match frame {
                Some(f) if !self.keep(f.pts) => continue,
                Some(f) => {
                    self.skip_until = None;
                    return Ok(Some(f));
                }
                None => {
                    self.skip_until = None;
                    return Ok(None);
                }
            }
        }
    }
}

/// Small RGBA image (rows tightly packed).
#[derive(Debug, Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Clip thumbnail: the frame at 10 % of the clip (the first frame is often
/// black), scaled on the CPU to fit `max_w × max_h`. Software decode only.
pub fn thumbnail(path: &Path, max_w: u32, max_h: u32) -> Result<Image> {
    let open = || {
        FfmpegDecoder::open(
            path,
            OpenOptions {
                hwaccel: HwAccel::None,
                native_bc: false,
            },
        )
    };
    let mut d = open()?;
    // Very short files (still images: one 0.04 s frame) start at the first frame.
    let at = d.info.duration / 10;
    let mut tries = if d.info.duration > Duration::from_millis(500) {
        d.seek(at)?;
        2
    } else {
        1
    };
    loop {
        if d.decode_next()? {
            let pts = d.pts_to_duration(d.frame.timestamp());
            if d.keep(pts) {
                break;
            }
            continue;
        }
        tries -= 1;
        if tries == 0 {
            return Err(MediaError::Decode(format!(
                "{}: no frame for thumbnail",
                path.display()
            )));
        }
        // Start over from the first frame (reopening: some demuxers can't seek back).
        d = open()?;
    }
    // swscale ignores YCgCo (it would apply BT.601); convert those ourselves.
    let ycgco = (d.frame.color_space() == ff::color::Space::YCGCO
        && matches!(d.frame.format(), Pixel::YUV420P | Pixel::YUVJ420P))
    .then(|| ycgco_to_rgba(&d.frame));
    let src = ycgco.as_ref().unwrap_or(&d.frame);
    let (w, h) = (src.width().max(1), src.height().max(1));
    let scale = (max_w as f64 / w as f64).min(max_h as f64 / h as f64).min(1.0);
    let (tw, th) = (
        ((w as f64 * scale).round() as u32).max(1),
        ((h as f64 * scale).round() as u32).max(1),
    );
    let mut sws = ff::software::scaling::Context::get(
        src.format(),
        w,
        h,
        Pixel::RGBA,
        tw,
        th,
        ff::software::scaling::Flags::AREA,
    )
    .map_err(|e| MediaError::Decode(format!("swscale {:?}: {e}", src.format())))?;
    let mut out = ff::frame::Video::empty();
    sws.run(src, &mut out).map_err(|e| MediaError::Decode(e.to_string()))?;
    let (stride, row) = (out.stride(0), tw as usize * 4);
    let rgba = rows(out.data(0), stride, row, th as usize);
    Ok(Image {
        width: tw,
        height: th,
        rgba,
    })
}

/// Try to attach a hardware device of type `ty`. Returns its name and the
/// pixel format hw frames will have.
unsafe fn attach_hw(
    ctx: &mut ff::codec::context::Context,
    codec: &ff::Codec,
    ty: ffi::AVHWDeviceType,
) -> Option<(String, Pixel)> {
    unsafe {
        let mut pix = None;
        for i in 0.. {
            let cfg = ffi::avcodec_get_hw_config(codec.as_ptr(), i);
            if cfg.is_null() {
                break;
            }
            if (*cfg).methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0 && (*cfg).device_type == ty {
                pix = Some((*cfg).pix_fmt);
                break;
            }
        }
        let pix = pix?;
        let mut dev: *mut ffi::AVBufferRef = ptr::null_mut();
        if ffi::av_hwdevice_ctx_create(&mut dev, ty, ptr::null(), ptr::null_mut(), 0) < 0 {
            return None;
        }
        let c = ctx.as_mut_ptr();
        (*c).hw_device_ctx = dev; // ownership moves to the codec context
        (*c).opaque = pix as i32 as isize as *mut c_void;
        (*c).get_format = Some(get_hw_format);
        let name = std::ffi::CStr::from_ptr(ffi::av_hwdevice_get_type_name(ty))
            .to_string_lossy()
            .into_owned();
        Some((name, Pixel::from(pix)))
    }
}

/// Pick the hw pixel format if offered, else let FFmpeg choose a software one.
unsafe extern "C" fn get_hw_format(
    ctx: *mut ffi::AVCodecContext,
    fmts: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    unsafe {
        let want = (*ctx).opaque as isize as i32;
        let mut p = fmts;
        while *p != ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            if *p as i32 == want {
                return *p;
            }
            p = p.add(1);
        }
        log::warn!("hw decode not available for this stream; using software");
        ffi::avcodec_default_get_format(ctx, fmts)
    }
}

/// Full-range YCgCo 4:2:0 (U = Cg, V = Co) → RGBA frame, same size.
fn ycgco_to_rgba(src: &ff::frame::Video) -> ff::frame::Video {
    let (w, h) = (src.width() as usize, src.height() as usize);
    let mut out = ff::frame::Video::new(Pixel::RGBA, w as u32, h as u32);
    let (ys, us, vs, os) = (src.stride(0), src.stride(1), src.stride(2), out.stride(0));
    let (yp, up, vp) = (src.data(0), src.data(1), src.data(2));
    let dst = out.data_mut(0);
    for row in 0..h {
        for x in 0..w {
            let y = yp[row * ys + x] as i32;
            let cg = up[(row / 2) * us + x / 2] as i32 - 128;
            let co = vp[(row / 2) * vs + x / 2] as i32 - 128;
            let t = y - cg;
            let px = [t + co, y + cg, t - co].map(|c| c.clamp(0, 255) as u8);
            dst[row * os + x * 4..][..4].copy_from_slice(&[px[0], px[1], px[2], 255]);
        }
    }
    out
}

/// Decodes single JPEG frames (MJPG webcams) to tight RGBA, software.
pub struct JpegDecoder {
    dec: ff::decoder::Video,
    sws: Option<(ff::software::scaling::Context, Pixel, u32, u32)>,
    frame: ff::frame::Video,
}

impl JpegDecoder {
    pub fn new() -> Result<Self> {
        init();
        let codec = ff::decoder::find(ff::codec::Id::MJPEG)
            .ok_or_else(|| MediaError::Decode("FFmpeg has no MJPEG decoder".into()))?;
        let dec = ff::codec::context::Context::new_with_codec(codec)
            .decoder()
            .video()
            .map_err(|e| MediaError::Decode(format!("MJPEG decoder: {e}")))?;
        Ok(Self {
            dec,
            sws: None,
            frame: ff::frame::Video::empty(),
        })
    }

    /// One JPEG → (width, height, RGBA); None for a broken frame.
    pub fn decode(&mut self, jpeg: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
        let pkt = ff::Packet::copy(jpeg);
        self.dec.send_packet(&pkt).ok()?;
        self.dec.receive_frame(&mut self.frame).ok()?;
        let (w, h, fmt) = (self.frame.width(), self.frame.height(), self.frame.format());
        if self
            .sws
            .as_ref()
            .is_none_or(|(_, f, sw, sh)| (*f, *sw, *sh) != (fmt, w, h))
        {
            let ctx = ff::software::scaling::Context::get(
                fmt,
                w,
                h,
                Pixel::RGBA,
                w,
                h,
                ff::software::scaling::Flags::BILINEAR,
            )
            .ok()?;
            self.sws = Some((ctx, fmt, w, h));
        }
        let mut out = ff::frame::Video::empty();
        self.sws.as_mut()?.0.run(&self.frame, &mut out).ok()?;
        let (stride, row) = (out.stride(0), w as usize * 4);
        let rgba = rows(out.data(0), stride, row, h as usize);
        Some((w, h, rgba))
    }
}

/// The first `n` rows of `row` bytes from rows `stride` apart, packed.
pub(crate) fn rows(data: &[u8], stride: usize, row: usize, n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(row * n);
    for r in data.chunks(stride).take(n) {
        v.extend_from_slice(&r[..row]);
    }
    v
}
