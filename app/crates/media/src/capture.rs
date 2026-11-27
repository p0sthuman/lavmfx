//! Capture devices (webcams, capture cards) opened directly through the
//! OS camera API with FFmpeg's device inputs: V4L2 (Linux), DirectShow
//! (Windows), AVFoundation (macOS) — what OBS does. (PipeWire's
//! camera nodes tear frames on some webcams, e.g. this laptop's Chicony HD
//! Webcam, where V4L2 directly is clean.)
//!
//! * [`devices`]: video capture devices (refreshed in the background).
//! * [`receiver`]: frames of one device, shared like NDI receivers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ff::format::Pixel;
use ffmpeg_next as ff;

use crate::ffmpeg::rows;
use crate::live::{Receiver, Shared, publish, rgba_frame, set_error};

/// A device: `id` is what FFmpeg opens (`/dev/video0`, `video=Name`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureDevice {
    pub id: String,
    pub label: String,
}

/// FFmpeg's input device for this platform.
fn input_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "dshow"
    } else if cfg!(target_os = "macos") {
        "avfoundation"
    } else {
        "v4l2"
    }
}

fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = ff::init();
        ff::device::register_all();
        ff::util::log::set_level(ff::util::log::Level::Error);
    });
}

/// Linux: `/sys/class/video4linux/videoN` with `index` 0 (the picture node;
/// 1 = metadata), sorted.
#[cfg(target_os = "linux")]
fn scan() -> Vec<CaptureDevice> {
    let Ok(rd) = std::fs::read_dir("/sys/class/video4linux") else {
        return Vec::new();
    };
    let mut v: Vec<(u32, CaptureDevice)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let node = e.file_name().to_string_lossy().into_owned();
            let n: u32 = node.strip_prefix("video")?.parse().ok()?;
            let read = |f: &str| {
                std::fs::read_to_string(e.path().join(f))
                    .ok()
                    .map(|s| s.trim().to_owned())
            };
            if read("index").as_deref() != Some("0") {
                return None;
            }
            let label = read("name").unwrap_or_else(|| node.clone());
            Some((
                n,
                CaptureDevice {
                    id: format!("/dev/{node}"),
                    label,
                },
            ))
        })
        .collect();
    v.sort_by_key(|(n, _)| *n);
    v.into_iter().map(|(_, d)| d).collect()
}

/// Windows / macOS: FFmpeg's device list (untested here: no such machine).
#[cfg(not(target_os = "linux"))]
fn scan() -> Vec<CaptureDevice> {
    use std::ffi::{CStr, CString};
    init();
    let name = CString::new(input_name()).unwrap();
    let mut out = Vec::new();
    unsafe {
        let fmt = ff::ffi::av_find_input_format(name.as_ptr());
        if fmt.is_null() {
            return out;
        }
        let mut list: *mut ff::ffi::AVDeviceInfoList = std::ptr::null_mut();
        if ff::ffi::avdevice_list_input_sources(fmt, std::ptr::null(), std::ptr::null_mut(), &mut list) < 0
            || list.is_null()
        {
            return out;
        }
        for i in 0..(*list).nb_devices as usize {
            let d = *(*list).devices.add(i);
            let s = |p: *mut std::ffi::c_char| {
                if p.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(p).to_string_lossy().into_owned()
                }
            };
            let (dev, desc) = (s((*d).device_name), s((*d).device_description));
            let id = if cfg!(target_os = "windows") {
                format!("video={desc}")
            } else {
                format!("{dev}:none")
            };
            out.push(CaptureDevice {
                id,
                label: if desc.is_empty() { dev } else { desc },
            });
        }
        ff::ffi::avdevice_free_list_devices(&mut list);
    }
    out
}

/// Capture devices on this computer (rescanned every 3 s in the background).
pub fn devices() -> Vec<CaptureDevice> {
    static LIST: OnceLock<Arc<Mutex<Vec<CaptureDevice>>>> = OnceLock::new();
    let list = LIST.get_or_init(|| {
        let l = Arc::new(Mutex::new(scan()));
        let w = l.clone();
        let _ = std::thread::Builder::new().name("capture-scan".into()).spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(3));
                let v = scan();
                *w.lock().unwrap() = v;
            }
        });
        l
    });
    list.lock().unwrap().clone()
}

/// Shared receiver of device `id`.
pub fn receiver(id: &str) -> Result<Arc<Receiver>, String> {
    crate::live::pooled(&format!("capture:{id}"), || {
        let shared: Shared = Default::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (sh, st, dev) = (shared.clone(), stop.clone(), id.to_owned());
        let thread = std::thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                // Reopen after errors (device busy, unplugged) until stopped.
                while !st.load(Ordering::Relaxed) {
                    if let Err(e) = run(&dev, &sh, &st) {
                        set_error(&sh, e);
                        let until = Instant::now() + Duration::from_secs(2);
                        while Instant::now() < until && !st.load(Ordering::Relaxed) {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let thread = Mutex::new(Some(thread));
        Ok(Arc::new(Receiver::new(id, shared, move || {
            stop.store(true, Ordering::Relaxed);
            if let Some(t) = thread.lock().unwrap().take() {
                let _ = t.join();
            }
        })))
    })
}

/// Open the device in its own default mode (like OBS's "Leave Unchanged";
/// forcing e.g. MJPG 1280×720 made this laptop's webcam drop off USB).
fn open(dev: &str) -> Result<ff::format::context::Input, String> {
    init();
    let name = std::ffi::CString::new(input_name()).unwrap();
    let fmt = unsafe { ff::ffi::av_find_input_format(name.as_ptr()) };
    if fmt.is_null() {
        return Err(format!("FFmpeg has no {} input", input_name()));
    }
    let format = ff::Format::Input(unsafe { ff::format::format::Input::wrap(fmt as *mut _) });
    let mut opts = ff::Dictionary::new();
    if input_name() == "avfoundation" {
        opts.set("framerate", "30"); // AVFoundation needs one
    }
    match ff::format::open_with(dev, &format, opts) {
        Ok(ff::format::context::Context::Input(i)) => Ok(i),
        Ok(_) => Err(format!("can't open {dev}: not an input")),
        Err(e) => Err(format!("can't open {dev}: {e}")),
    }
}

fn run(dev: &str, sh: &Shared, stop: &AtomicBool) -> Result<(), String> {
    let mut input = open(dev)?;
    let stream = input.streams().best(ff::media::Type::Video).ok_or("no video stream")?;
    let index = stream.index();
    let rate = stream.avg_frame_rate();
    let fps = if rate.denominator() > 0 {
        f64::from(rate.numerator()) / f64::from(rate.denominator())
    } else {
        0.0
    };
    let ctx = ff::codec::context::Context::from_parameters(stream.parameters()).map_err(|e| e.to_string())?;
    let mut dec = ctx.decoder().video().map_err(|e| e.to_string())?;
    let mut frame = ff::frame::Video::empty();
    let mut sws: Option<(ff::software::scaling::Context, Pixel, u32, u32)> = None;
    for (s, packet) in input.packets() {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if s.index() != index {
            continue;
        }
        if dec.send_packet(&packet).is_err() {
            continue; // a damaged frame: skip it
        }
        while dec.receive_frame(&mut frame).is_ok() {
            let (w, h, fmt) = (frame.width(), frame.height(), frame.format());
            if sws.as_ref().is_none_or(|(_, f, sw, sh)| (*f, *sw, *sh) != (fmt, w, h)) {
                let c = ff::software::scaling::Context::get(
                    fmt,
                    w,
                    h,
                    Pixel::RGBA,
                    w,
                    h,
                    ff::software::scaling::Flags::BILINEAR,
                )
                .map_err(|e| e.to_string())?;
                sws = Some((c, fmt, w, h));
            }
            let mut out = ff::frame::Video::empty();
            if sws.as_mut().unwrap().0.run(&frame, &mut out).is_err() {
                continue;
            }
            let (stride, row) = (out.stride(0), w as usize * 4);
            let rgba = rows(out.data(0), stride, row, h as usize);
            publish(sh, rgba_frame(w, h, rgba), fps);
        }
    }
    Err("the device stopped sending".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn devices_list_without_metadata_nodes() {
        let d = super::devices();
        eprintln!("{d:?}");
        assert!(d.iter().all(|d| !d.label.is_empty() && !d.id.is_empty()));
    }
}
