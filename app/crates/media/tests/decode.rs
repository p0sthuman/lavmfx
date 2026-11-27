//! Decodes small files generated with the `ffmpeg` CLI. Skips when the CLI
//! or an encoder is missing.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use lavmfx_media::{FfmpegDecoder, FrameFormat, HwAccel, OpenOptions, VideoDecoder, VideoFrame};

/// Encode 2 s of the ffmpeg test source (64 frames at 32 fps). `None` if unavailable.
fn make(name: &str, args: &[&str]) -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    if path.exists() {
        return Some(path);
    }
    let tag = format!("{:?}", std::thread::current().id()).replace(|c: char| !c.is_ascii_alphanumeric(), "");
    let tmp = path.with_extension(format!("{tag}.{}", path.extension()?.to_str()?));
    let ok = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=32:duration=2",
        ])
        .args(args)
        .arg(&tmp)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("ffmpeg could not make {name}; skipping");
        return None;
    }
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

const SW: OpenOptions = OpenOptions {
    hwaccel: HwAccel::None,
    native_bc: false,
};
const NATIVE: OpenOptions = OpenOptions {
    hwaccel: HwAccel::None,
    native_bc: true,
};

fn all_frames(dec: &mut dyn VideoDecoder) -> Vec<VideoFrame> {
    std::iter::from_fn(|| dec.next_frame().unwrap()).collect()
}

#[test]
fn h264_decode_and_seek() {
    let Some(p) = make("h264.mp4", &["-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "16"]) else {
        return;
    };
    let mut d = FfmpegDecoder::open(&p, SW).unwrap();
    let info = d.info().clone();
    assert_eq!((info.width, info.height), (320, 240));
    assert_eq!(info.frame_rate, (32, 1));
    assert!((info.duration.as_secs_f64() - 2.0).abs() < 0.05, "{:?}", info.duration);

    let frames = all_frames(&mut d);
    assert_eq!(frames.len(), 64);
    let f = &frames[0];
    assert_eq!(f.format, FrameFormat::Yuv420p);
    assert_eq!(f.planes.len(), 3);
    assert!(f.planes[0].stride >= 320 && f.planes[1].stride >= 160);
    assert!(f.planes[0].data.len() >= f.planes[0].stride as usize * 240);
    assert!(frames.windows(2).all(|w| w[0].pts < w[1].pts));

    // Accurate seek: lands on the frame showing at t, mid-GOP.
    for t in [1.40625, 0.5, 0.0, 1.97] {
        d.seek(Duration::from_secs_f64(t)).unwrap();
        let f = d.next_frame().unwrap().unwrap();
        let want = (t * 32.0).floor() / 32.0;
        assert!((f.pts.as_secs_f64() - want).abs() < 1e-3, "seek {t}: got {:?}", f.pts);
    }
    // Seek after EOF works too.
    while d.next_frame().unwrap().is_some() {}
    d.seek(Duration::ZERO).unwrap();
    assert_eq!(d.next_frame().unwrap().unwrap().pts, Duration::ZERO);
}

/// Decode a BC1 colour block (also the colour half of BC3) to RGB.
fn bc1_block(b: &[u8], four_color: bool) -> [[u8; 3]; 16] {
    let c = |v: u16| {
        [
            ((v >> 11) & 31) as f32 * 255.0 / 31.0,
            ((v >> 5) & 63) as f32 * 255.0 / 63.0,
            (v & 31) as f32 * 255.0 / 31.0,
        ]
    };
    let (v0, v1) = (u16::from_le_bytes([b[0], b[1]]), u16::from_le_bytes([b[2], b[3]]));
    let (c0, c1) = (c(v0), c(v1));
    let mix = |a: f32, x: f32, y: f32| x * a + y * (1.0 - a);
    let pal: [[f32; 3]; 4] = if four_color || v0 > v1 {
        [
            c0,
            c1,
            [0, 1, 2].map(|i| mix(2.0 / 3.0, c0[i], c1[i])),
            [0, 1, 2].map(|i| mix(1.0 / 3.0, c0[i], c1[i])),
        ]
    } else {
        [c0, c1, [0, 1, 2].map(|i| mix(0.5, c0[i], c1[i])), [0.0; 3]]
    };
    let idx = u32::from_le_bytes(b[4..8].try_into().unwrap());
    std::array::from_fn(|i| pal[((idx >> (2 * i)) & 3) as usize].map(|v| v.round() as u8))
}

/// Mean absolute RGB error between our BC blocks and FFmpeg's RGBA decode.
fn bc_vs_rgba(bc: &VideoFrame, rgba: &VideoFrame) -> f64 {
    let (w, h) = (bc.width as usize, bc.height as usize);
    let (bw, block) = (w.div_ceil(4), if bc.format == FrameFormat::Bc1 { 8 } else { 16 });
    let rp = &rgba.planes[0];
    let mut err = 0.0;
    for y in 0..h {
        for x in 0..w {
            let b = &bc.planes[0].data[((y / 4) * bw + x / 4) * block..][..block];
            let colour = if block == 8 { b } else { &b[8..] };
            let px = bc1_block(colour, block == 16)[(y % 4) * 4 + x % 4];
            let o = y * rp.stride as usize + x * 4;
            err += (0..3)
                .map(|i| (px[i] as f64 - rp.data[o + i] as f64).abs())
                .sum::<f64>();
        }
    }
    err / (w * h * 3) as f64
}

#[test]
fn hap_native_matches_ffmpeg() {
    let cases = [
        ("hap.mov", "hap", "snappy", "1", FrameFormat::Bc1),
        ("hap_none.mov", "hap", "none", "1", FrameFormat::Bc1),
        ("hap_chunks.mov", "hap", "snappy", "4", FrameFormat::Bc1),
        ("hap_alpha.mov", "hap_alpha", "snappy", "1", FrameFormat::Bc3),
        ("hap_q.mov", "hap_q", "snappy", "2", FrameFormat::Bc3YCoCg),
    ];
    for (name, fmt, comp, chunks, want) in cases {
        let args = ["-c:v", "hap", "-format", fmt, "-compressor", comp, "-chunks", chunks];
        let Some(p) = make(name, &args) else { return };
        let mut native = FfmpegDecoder::open(&p, NATIVE).unwrap();
        assert_eq!(native.info().decode_path, "HAP native BC");
        let frames = all_frames(&mut native);
        assert_eq!(frames.len(), 64, "{name}");
        let f = &frames[10];
        assert_eq!(f.format, want, "{name}");
        assert_eq!(
            f.planes[0].data.len(),
            80 * 60 * if want == FrameFormat::Bc1 { 8 } else { 16 }
        );

        if want != FrameFormat::Bc3YCoCg {
            let mut sw = FfmpegDecoder::open(&p, SW).unwrap();
            let reference = all_frames(&mut sw).swap_remove(10);
            assert_eq!(reference.format, FrameFormat::Rgba8);
            let e = bc_vs_rgba(f, &reference);
            assert!(e < 1.0, "{name}: mean error {e}");
        }

        native.seek(Duration::from_secs_f64(1.0)).unwrap();
        assert_eq!(native.next_frame().unwrap().unwrap().pts, Duration::from_secs(1));
    }
}

#[test]
fn dxv_decodes_via_ffmpeg() {
    let Some(p) = make("dxv.mov", &["-c:v", "dxv", "-pix_fmt", "rgba"]) else {
        return;
    };
    let mut d = FfmpegDecoder::open(&p, OpenOptions::default()).unwrap();
    assert_eq!(d.info().codec, "dxv");
    let f = d.next_frame().unwrap().unwrap();
    assert_eq!((f.width, f.height, f.format), (320, 240, FrameFormat::Rgba8));
}

/// Hardware path, if this machine has one (CUDA/VAAPI). Logs and passes otherwise.
#[test]
fn h264_hwaccel_if_available() {
    let Some(p) = make("h264.mp4", &["-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "16"]) else {
        return;
    };
    let opts = OpenOptions {
        hwaccel: HwAccel::Auto,
        native_bc: true,
    };
    let mut d = FfmpegDecoder::open(&p, opts).unwrap();
    eprintln!("decode path: {}", d.info().decode_path);
    let frames = all_frames(&mut d);
    assert_eq!(frames.len(), 64);
    if d.info().decode_path != "software" {
        assert_eq!(frames[0].format, FrameFormat::Nv12);
    }
}

#[test]
fn thumbnail_fits_box() {
    let Some(p) = make("h264.mp4", &["-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "16"]) else {
        return;
    };
    let img = lavmfx_media::thumbnail(&p, 96, 54).unwrap();
    // 4:3 into 96×54 → height-bound.
    assert_eq!((img.width, img.height), (72, 54));
    assert_eq!(img.rgba.len(), 72 * 54 * 4);
    // testsrc2 is colourful, not a flat frame.
    let first = &img.rgba[..3];
    assert!(
        img.rgba
            .chunks(4)
            .any(|p| (0..3).any(|i| (p[i] as i32 - first[i] as i32).abs() > 40))
    );
    assert!(img.rgba.chunks(4).all(|p| p[3] == 255));
}

/// Play `path` backward in real time for `secs` (seek on every new frame, as
/// the app does) and return (frames shown, seeks issued, mean seek→frame ms).
fn backward_run(path: &std::path::Path, opts: OpenOptions, secs: f64) -> (usize, usize) {
    use lavmfx_media::Player;
    use std::time::Instant;
    let mut p = Player::open(path, opts).unwrap();
    let dur = p.duration();
    let fd = 1.0 / p.info().fps();
    let start = dur - 0.001;
    let t0 = Instant::now();
    let (mut shown, mut seeks, mut last_f) = (0, 0, i64::MIN);
    loop {
        let el = t0.elapsed().as_secs_f64();
        if el > secs {
            break;
        }
        let pos = (start - el).rem_euclid(dur);
        let f = (pos / fd).floor() as i64;
        if f != last_f {
            last_f = f;
            p.seek(pos);
            seeks += 1;
        }
        if p.update(pos).is_some() {
            shown += 1;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    (shown, seeks)
}

/// Diagnostic: `LAVMFX_BACKWARD_FILES=a.mov:b.mp4 cargo test -p lavmfx-media --release backward_files -- --ignored --nocapture`
#[test]
#[ignore]
fn backward_files() {
    let files = std::env::var("LAVMFX_BACKWARD_FILES").unwrap_or_default();
    for f in files.split(':').filter(|f| !f.is_empty()) {
        let (shown, seeks) = backward_run(std::path::Path::new(f), SW, 3.0);
        eprintln!("{f}: {shown} frames shown for {seeks} seeks in 3 s");
    }
}

/// Diagnostic: time seek + first frame at a few positions (same env var).
#[test]
#[ignore]
fn seek_timing_files() {
    let files = std::env::var("LAVMFX_BACKWARD_FILES").unwrap_or_default();
    for f in files.split(':').filter(|f| !f.is_empty()) {
        let mut d = FfmpegDecoder::open(std::path::Path::new(f), SW).unwrap();
        let dur = d.info().duration.as_secs_f64();
        for k in [0.9, 0.5, 0.49, 0.48] {
            let t = Duration::from_secs_f64(dur * k);
            let t0 = std::time::Instant::now();
            d.seek(t).unwrap();
            let fr = d.next_frame().unwrap().unwrap();
            eprintln!(
                "{}: seek {:.3}s -> pts {:.3}s in {:.1} ms",
                std::path::Path::new(f).file_name().unwrap().to_string_lossy(),
                t.as_secs_f64(),
                fr.pts.as_secs_f64(),
                t0.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}

/// Playing backward shows (nearly) every frame of an intra-only clip.
#[test]
fn backward_playback_shows_frames() {
    let Some(p) = make("intra.mov", &["-c:v", "mjpeg", "-q:v", "5"]) else {
        return;
    };
    let (shown, seeks) = backward_run(&p, SW, 1.5);
    assert!(seeks > 30, "{seeks}");
    assert!(shown * 10 >= seeks * 8, "{shown} frames shown for {seeks} seeks");
}

#[test]
fn audio_track_decodes_to_stereo() {
    let Some(p) = make(
        "audio51.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=441:sample_rate=44100:duration=2",
            "-ac",
            "6",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
            "-shortest",
        ],
    ) else {
        return;
    };
    let pcm = lavmfx_media::audio::decode(&p).unwrap().expect("has audio");
    assert_eq!(pcm.rate, 44_100);
    assert!((pcm.duration() - 2.0).abs() < 0.1, "{}", pcm.duration());
    // 441 Hz = one period every 100 frames: count rising zero crossings in the middle second.
    let mid: Vec<f32> = (pcm.rate as usize / 2..pcm.rate as usize * 3 / 2)
        .map(|i| pcm.frame(i).0)
        .collect();
    let rises = mid.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    assert!((440..=442).contains(&rises), "{rises} periods");
    let peak = mid.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.05, "audible after downmix: {peak}");
    // Video-only file: no audio.
    let v = make("h264.mp4", &["-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "16"]).unwrap();
    assert!(lavmfx_media::audio::decode(&v).unwrap().is_none());
}

/// A single still image (JPEG: 0.04 s long) still gives a thumbnail (ISF
/// imported images, Mask effect files).
#[test]
fn thumbnail_of_a_still_jpeg() {
    let Some(p) = make("still.jpg", &["-frames:v", "1", "-update", "1"]) else {
        return;
    };
    let img = lavmfx_media::thumbnail(&p, 4096, 4096).unwrap();
    assert_eq!((img.width, img.height), (320, 240));
}

/// An audio file (MP3 with embedded cover art, like many music files) opens as
/// an audio-only clip: its length, one transparent frame per pass, and a
/// waveform that follows the sound (silent first half, tone second half).
#[test]
fn audio_file_opens_as_audio_only_clip() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let path = dir.join("audio-only.mp3");
    let cover = dir.join("audio-only-cover.png");
    if !path.exists() {
        let ok = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:duration=1",
            ])
            .args(["-frames:v", "1"])
            .arg(&cover)
            .status()
            .is_ok_and(|s| s.success())
            && Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
                .arg("aevalsrc=if(gte(t\\,1)\\,0.5*sin(2*PI*220*t)\\,0):s=44100:d=2")
                .arg("-i")
                .arg(&cover)
                .args(["-map", "0:a", "-map", "1:v", "-c:a", "libmp3lame", "-c:v", "png"])
                .args(["-disposition:v", "attached_pic"])
                .arg(&path)
                .status()
                .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("ffmpeg could not make audio-only.mp3; skipping");
            return;
        }
    }
    assert!(lavmfx_media::is_audio_file(&path) && lavmfx_media::is_media_file(&path));
    let info = lavmfx_media::probe(&path).unwrap();
    assert!((info.duration.as_secs_f64() - 2.0).abs() < 0.1, "{:?}", info.duration);
    assert!(info.has_audio);
    let mut d = lavmfx_media::open(&path, SW).unwrap();
    let frames = all_frames(d.as_mut());
    assert_eq!(frames.len(), 1, "one frame per pass, not the cover art");
    assert!(frames[0].planes[0].data.iter().all(|&b| b == 0), "transparent");
    d.seek(Duration::from_secs(1)).unwrap();
    assert!(d.next_frame().unwrap().is_some(), "seeking starts a new pass");
    let pcm = lavmfx_media::audio::decode(&path).unwrap().unwrap();
    let peaks = pcm.peaks(8);
    assert!(peaks[..3].iter().all(|&p| p < 0.05), "{peaks:?}");
    assert!(peaks[5..].iter().all(|&p| p > 0.9), "{peaks:?}");
}
