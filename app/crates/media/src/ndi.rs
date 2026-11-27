//! NDI (network video) in and out.
//!
//! NDI is proprietary (Vizrt): the runtime library is loaded when first
//! needed (`libndi.so.6` / `Processing.NDI.Lib.x64.dll` / `libndi.dylib`,
//! from NDI Tools / NDI Runtime / the SDK), as OBS does. Without it every
//! call reports [`runtime_error`] and the app works as before.
//!
//! The declarations below follow NDI's public C API (SDK v5/v6 headers):
//! `NDIlib_find_*`, `NDIlib_recv_*_v3/_v2`, `NDIlib_send_*`.
//!
//! * [`sources`]: names seen on the network (a finder thread runs from the
//!   first call).
//! * [`receiver`]: one receiving thread per source name, shared (layers and
//!   the preview reading the same source get the same frames); RGBA frames.
//! * [`Sender`]: one sending thread; frames are dropped while it's busy.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use std::time::{Duration, Instant};

use crate::live::{FrameSink, Pixel, Shared, publish, rgba_frame, set_error, to_rgba};
pub use crate::live::{OutFrame, Receiver, RecvStatus};

// ------------------------------------------------------------------ C API

#[repr(C)]
#[derive(Clone, Copy)]
struct Source {
    p_ndi_name: *const c_char,
    p_url_address: *const c_char,
}

#[repr(C)]
struct FindCreate {
    show_local_sources: bool,
    p_groups: *const c_char,
    p_extra_ips: *const c_char,
}

#[repr(C)]
struct RecvCreateV3 {
    source_to_connect_to: Source,
    color_format: i32,
    bandwidth: i32,
    allow_video_fields: bool,
    p_ndi_recv_name: *const c_char,
}

#[repr(C)]
struct SendCreate {
    p_ndi_name: *const c_char,
    p_groups: *const c_char,
    clock_video: bool,
    clock_audio: bool,
}

#[repr(C)]
struct VideoFrameV2 {
    xres: i32,
    yres: i32,
    four_cc: u32,
    frame_rate_n: i32,
    frame_rate_d: i32,
    picture_aspect_ratio: f32,
    frame_format_type: i32,
    timecode: i64,
    p_data: *mut u8,
    line_stride_in_bytes: i32,
    p_metadata: *const c_char,
    timestamp: i64,
}

impl VideoFrameV2 {
    fn zeroed() -> Self {
        Self {
            xres: 0,
            yres: 0,
            four_cc: 0,
            frame_rate_n: 0,
            frame_rate_d: 0,
            picture_aspect_ratio: 0.0,
            frame_format_type: 0,
            timecode: 0,
            p_data: std::ptr::null_mut(),
            line_stride_in_bytes: 0,
            p_metadata: std::ptr::null(),
            timestamp: 0,
        }
    }
}

const fn fourcc(s: &[u8; 4]) -> u32 {
    s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16 | (s[3] as u32) << 24
}
const FOURCC_RGBA: u32 = fourcc(b"RGBA");
const FOURCC_RGBX: u32 = fourcc(b"RGBX");
const FOURCC_BGRA: u32 = fourcc(b"BGRA");
const FOURCC_BGRX: u32 = fourcc(b"BGRX");
const COLOR_RGBX_RGBA: i32 = 2;
const BANDWIDTH_HIGHEST: i32 = 100;
const FRAME_VIDEO: i32 = 1;
const FRAME_ERROR: i32 = 4;
const FORMAT_PROGRESSIVE: i32 = 1;
/// `NDIlib_send_timecode_synthesize`.
const TIMECODE_SYNTHESIZE: i64 = i64::MAX;

type Inst = *mut c_void;

/// Loaded runtime: function pointers (the library stays loaded for the process).
struct Api {
    version: String,
    path: PathBuf,
    destroy: unsafe extern "C" fn(),
    find_create_v2: unsafe extern "C" fn(*const FindCreate) -> Inst,
    find_wait_for_sources: unsafe extern "C" fn(Inst, u32) -> bool,
    find_get_current_sources: unsafe extern "C" fn(Inst, *mut u32) -> *const Source,
    recv_create_v3: unsafe extern "C" fn(*const RecvCreateV3) -> Inst,
    recv_destroy: unsafe extern "C" fn(Inst),
    recv_capture_v2: unsafe extern "C" fn(Inst, *mut VideoFrameV2, *mut c_void, *mut c_void, u32) -> i32,
    recv_free_video_v2: unsafe extern "C" fn(Inst, *const VideoFrameV2),
    send_create: unsafe extern "C" fn(*const SendCreate) -> Inst,
    send_destroy: unsafe extern "C" fn(Inst),
    send_send_video_v2: unsafe extern "C" fn(Inst, *const VideoFrameV2),
    send_get_no_connections: unsafe extern "C" fn(Inst, u32) -> i32,
}

impl Drop for Api {
    fn drop(&mut self) {
        unsafe { (self.destroy)() };
    }
}

/// Library file names / places to try, in order.
fn candidates(dir: Option<&Path>) -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    const NAMES: &[&str] = &["Processing.NDI.Lib.x64.dll"];
    #[cfg(target_os = "macos")]
    const NAMES: &[&str] = &["libndi.dylib", "libndi_advanced.dylib"];
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    const NAMES: &[&str] = &["libndi.so.6", "libndi.so.5", "libndi.so"];
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = dir {
        dirs.push(d.to_path_buf());
        // An SDK folder: its lib/ subfolders.
        for sub in [
            "lib/x86_64-linux-gnu",
            "lib/aarch64-rpi4-linux-gnueabi",
            "lib/macOS",
            "Bin/x64",
            "lib",
        ] {
            dirs.push(d.join(sub));
        }
    }
    for var in ["NDI_RUNTIME_DIR_V6", "NDI_RUNTIME_DIR_V5"] {
        if let Some(v) = std::env::var_os(var) {
            dirs.push(PathBuf::from(v));
        }
    }
    #[cfg(target_os = "macos")]
    dirs.extend(
        [
            "/usr/local/lib",
            "/Library/NDI SDK for Apple/lib/macOS",
            "/Applications/NDI Video Monitor.app/Contents/Frameworks",
        ]
        .map(PathBuf::from),
    );
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    dirs.extend(
        [
            "/usr/lib",
            "/usr/local/lib",
            "/usr/lib/x86_64-linux-gnu",
            "/opt/ndi/lib",
        ]
        .map(PathBuf::from),
    );
    let mut out: Vec<PathBuf> = dirs
        .iter()
        .flat_map(|d| NAMES.iter().map(move |n| d.join(n)))
        .filter(|p| p.is_file())
        .collect();
    // Finally the plain names (system library path).
    out.extend(NAMES.iter().map(PathBuf::from));
    out
}

fn load(dir: Option<&Path>) -> Result<Api, String> {
    let mut last = String::new();
    for path in candidates(dir) {
        let lib = match unsafe { libloading::Library::new(&path) } {
            Ok(l) => l,
            Err(e) => {
                last = e.to_string();
                continue;
            }
        };
        // The library must outlive every pointer: keep it for the process.
        let lib: &'static libloading::Library = Box::leak(Box::new(lib));
        macro_rules! sym {
            ($name:literal) => {
                *unsafe { lib.get(concat!($name, "\0").as_bytes()) }
                    .map_err(|e| format!("{}: {} missing ({e})", path.display(), $name))?
            };
        }
        let initialize: unsafe extern "C" fn() -> bool = sym!("NDIlib_initialize");
        let version: unsafe extern "C" fn() -> *const c_char = sym!("NDIlib_version");
        if !unsafe { initialize() } {
            return Err("NDI runtime refused to start (CPU not supported?)".into());
        }
        let v = unsafe { version() };
        let version = if v.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(v) }.to_string_lossy().into_owned()
        };
        return Ok(Api {
            version,
            path: path.clone(),
            destroy: sym!("NDIlib_destroy"),
            find_create_v2: sym!("NDIlib_find_create_v2"),
            find_wait_for_sources: sym!("NDIlib_find_wait_for_sources"),
            find_get_current_sources: sym!("NDIlib_find_get_current_sources"),
            recv_create_v3: sym!("NDIlib_recv_create_v3"),
            recv_destroy: sym!("NDIlib_recv_destroy"),
            recv_capture_v2: sym!("NDIlib_recv_capture_v2"),
            recv_free_video_v2: sym!("NDIlib_recv_free_video_v2"),
            send_create: sym!("NDIlib_send_create"),
            send_destroy: sym!("NDIlib_send_destroy"),
            send_send_video_v2: sym!("NDIlib_send_send_video_v2"),
            send_get_no_connections: sym!("NDIlib_send_get_no_connections"),
        });
    }
    Err(format!(
        "NDI Runtime not found (install NDI Tools / NDI Runtime from ndi.video, or set its folder in Preferences › Video){}",
        if last.is_empty() {
            String::new()
        } else {
            format!(": {last}")
        }
    ))
}

struct Runtime {
    dir: Option<PathBuf>,
    api: Option<Result<Arc<Api>, String>>,
}

static RUNTIME: Mutex<Runtime> = Mutex::new(Runtime { dir: None, api: None });

/// Where to look first for the runtime (Preferences); a change retries loading.
pub fn set_runtime_dir(dir: Option<PathBuf>) {
    let mut r = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
    if r.dir != dir {
        r.dir = dir;
        if matches!(r.api, Some(Err(_))) {
            r.api = None;
        }
    }
}

fn api() -> Result<Arc<Api>, String> {
    let mut r = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
    if r.api.is_none() {
        let res = load(r.dir.as_deref()).map(Arc::new);
        match &res {
            Ok(a) => log::info!("NDI runtime {} ({})", a.version, a.path.display()),
            Err(e) => log::info!("{e}"),
        }
        r.api = Some(res);
    }
    r.api.clone().unwrap()
}

/// "NDI 6.x (path)" or why NDI isn't available.
pub fn runtime_status() -> Result<String, String> {
    api().map(|a| format!("{} ({})", a.version, a.path.display()))
}

/// None when the runtime loaded.
pub fn runtime_error() -> Option<String> {
    api().err()
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

// ------------------------------------------------------------------ finder

struct Finder {
    sources: Mutex<Vec<String>>,
}

/// NDI sources on the network ("MACHINE (Name)"), sorted. Starts the finder on
/// first call; empty without a runtime.
pub fn sources() -> Vec<String> {
    static FINDER: Mutex<Option<Arc<Finder>>> = Mutex::new(None);
    let mut slot = FINDER.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        *slot = start_finder();
    }
    slot.as_ref()
        .map_or_else(Vec::new, |f| f.sources.lock().unwrap().clone())
}

fn start_finder() -> Option<Arc<Finder>> {
    {
        let api = api().ok()?;
        let f = Arc::new(Finder {
            sources: Mutex::new(Vec::new()),
        });
        let shared = f.clone();
        std::thread::Builder::new()
            .name("ndi-find".into())
            .spawn(move || {
                let create = FindCreate {
                    show_local_sources: true,
                    p_groups: std::ptr::null(),
                    p_extra_ips: std::ptr::null(),
                };
                let inst = unsafe { (api.find_create_v2)(&create) };
                if inst.is_null() {
                    log::warn!("NDI: finder could not start");
                    return;
                }
                loop {
                    unsafe { (api.find_wait_for_sources)(inst, 1000) };
                    let mut n = 0u32;
                    let list = unsafe { (api.find_get_current_sources)(inst, &mut n) };
                    let mut names: Vec<String> = (0..n as usize)
                        .map(|i| cstr(unsafe { (*list.add(i)).p_ndi_name }))
                        .collect();
                    names.sort();
                    *shared.sources.lock().unwrap() = names;
                }
            })
            .ok()?;
        Some(f)
    }
}

// ------------------------------------------------------------------ receiver

/// NDI pixel order → ours.
fn pixel_of(four_cc: u32) -> Option<Pixel> {
    Some(match four_cc {
        FOURCC_RGBA => Pixel::Rgba,
        FOURCC_RGBX => Pixel::Rgbx,
        FOURCC_BGRA => Pixel::Bgra,
        FOURCC_BGRX => Pixel::Bgrx,
        _ => return None,
    })
}

struct SendPtr(Inst);
unsafe impl Send for SendPtr {}

/// Shared receiver for `name` (a new thread unless one is running).
pub fn receiver(name: &str) -> Result<Arc<Receiver>, String> {
    crate::live::pooled(&format!("ndi:{name}"), || {
        let api = api()?;
        let shared: Shared = Default::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (sh, st, source) = (shared.clone(), stop.clone(), name.to_owned());
        let thread = std::thread::Builder::new()
            .name("ndi-recv".into())
            .spawn(move || recv_loop(&api, &source, &sh, &st))
            .map_err(|e| e.to_string())?;
        let thread = Mutex::new(Some(thread));
        Ok(Arc::new(Receiver::new(name, shared, move || {
            stop.store(true, Ordering::Relaxed);
            if let Some(t) = thread.lock().unwrap().take() {
                let _ = t.join();
            }
        })))
    })
}

fn recv_loop(api: &Api, source: &str, sh: &Shared, st: &AtomicBool) {
    let c_name = CString::new(source).unwrap_or_default();
    let recv_name = CString::new("LAVMFX").unwrap();
    let create = RecvCreateV3 {
        source_to_connect_to: Source {
            p_ndi_name: c_name.as_ptr(),
            p_url_address: std::ptr::null(),
        },
        color_format: COLOR_RGBX_RGBA,
        bandwidth: BANDWIDTH_HIGHEST,
        allow_video_fields: false,
        p_ndi_recv_name: recv_name.as_ptr(),
    };
    let inst = unsafe { (api.recv_create_v3)(&create) };
    if inst.is_null() {
        set_error(sh, "NDI receiver could not start");
        return;
    }
    while !st.load(Ordering::Relaxed) {
        let mut v = VideoFrameV2::zeroed();
        let t = unsafe { (api.recv_capture_v2)(inst, &mut v, std::ptr::null_mut(), std::ptr::null_mut(), 100) };
        if t == FRAME_ERROR {
            set_error(sh, "connection lost");
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        if t != FRAME_VIDEO {
            continue;
        }
        let (w, h) = (v.xres.max(0) as usize, v.yres.max(0) as usize);
        let stride = v.line_stride_in_bytes.max(0) as usize;
        let px = pixel_of(v.four_cc);
        let data = match px {
            Some(px) if !v.p_data.is_null() && w > 0 && h > 0 => {
                let src = unsafe { std::slice::from_raw_parts(v.p_data, stride * (h - 1) + w * 4) };
                to_rgba(src, w, h, stride, px)
            }
            _ => None,
        };
        let fps = if v.frame_rate_d > 0 {
            v.frame_rate_n as f64 / v.frame_rate_d as f64
        } else {
            0.0
        };
        let four_cc = v.four_cc;
        unsafe { (api.recv_free_video_v2)(inst, &v) };
        match data {
            Some(rgba) => publish(sh, rgba_frame(w as u32, h as u32, rgba), fps),
            None => set_error(
                sh,
                format!(
                    "unsupported pixel format {}",
                    String::from_utf8_lossy(&four_cc.to_le_bytes())
                ),
            ),
        }
    }
    unsafe { (api.recv_destroy)(inst) };
}

// ------------------------------------------------------------------ sender

/// An NDI source on the network (`MACHINE (name)`).
pub struct Sender {
    pub name: String,
    tx: Option<SyncSender<OutFrame>>,
    thread: Option<JoinHandle<()>>,
    connections: Arc<AtomicU32>,
    sent: Arc<AtomicU64>,
    /// Buffers coming back from the thread (reused for the next frames).
    spare: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Sender {
    pub fn new(name: &str) -> Result<Self, String> {
        let api = api()?;
        let c_name = CString::new(name).map_err(|e| e.to_string())?;
        let create = SendCreate {
            p_ndi_name: c_name.as_ptr(),
            p_groups: std::ptr::null(),
            clock_video: false,
            clock_audio: false,
        };
        let inst = SendPtr(unsafe { (api.send_create)(&create) });
        if inst.0.is_null() {
            return Err(format!("NDI could not create the source \"{name}\""));
        }
        let (tx, rx) = sync_channel::<OutFrame>(1);
        let connections = Arc::new(AtomicU32::new(0));
        let sent = Arc::new(AtomicU64::new(0));
        let spare = Arc::new(Mutex::new(Vec::new()));
        let (c, s, sp) = (connections.clone(), sent.clone(), spare.clone());
        let thread = std::thread::Builder::new()
            .name("ndi-send".into())
            .spawn(move || {
                let inst = inst;
                let mut polled = Instant::now() - Duration::from_secs(1);
                while let Ok(mut f) = rx.recv() {
                    let v = VideoFrameV2 {
                        xres: f.width as i32,
                        yres: f.height as i32,
                        four_cc: FOURCC_RGBA,
                        frame_rate_n: f.rate.0,
                        frame_rate_d: f.rate.1.max(1),
                        picture_aspect_ratio: 0.0, // = width / height
                        frame_format_type: FORMAT_PROGRESSIVE,
                        timecode: TIMECODE_SYNTHESIZE,
                        p_data: f.data.as_mut_ptr(),
                        line_stride_in_bytes: f.stride as i32,
                        p_metadata: std::ptr::null(),
                        timestamp: 0,
                    };
                    // Synchronous: NDI compresses here; the buffer is ours again after.
                    unsafe { (api.send_send_video_v2)(inst.0, &v) };
                    s.fetch_add(1, Ordering::Relaxed);
                    if polled.elapsed() > Duration::from_millis(500) {
                        polled = Instant::now();
                        let n = unsafe { (api.send_get_no_connections)(inst.0, 0) };
                        c.store(n.max(0) as u32, Ordering::Relaxed);
                    }
                    let mut spare = sp.lock().unwrap();
                    if spare.len() < 2 {
                        spare.push(std::mem::take(&mut f.data));
                    }
                }
                unsafe { (api.send_destroy)(inst.0) };
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            name: name.to_owned(),
            tx: Some(tx),
            thread: Some(thread),
            connections,
            sent,
            spare,
        })
    }

    /// A buffer of at least `len` bytes to fill (recycled when possible).
    pub fn buffer(&self, len: usize) -> Vec<u8> {
        let mut b = self.spare.lock().unwrap().pop().unwrap_or_default();
        b.resize(len, 0);
        b
    }

    /// Queue a frame; false = the previous one is still being sent (dropped).
    pub fn send(&self, f: OutFrame) -> bool {
        match self.tx.as_ref().map(|t| t.try_send(f)) {
            Some(Ok(())) => true,
            Some(Err(TrySendError::Full(f))) => {
                let mut spare = self.spare.lock().unwrap();
                if spare.len() < 2 {
                    spare.push(f.data);
                }
                false
            }
            _ => false,
        }
    }

    /// Receivers connected (updated twice a second while sending).
    pub fn connections(&self) -> u32 {
        self.connections.load(Ordering::Relaxed)
    }

    pub fn frames_sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }
}

impl FrameSink for Sender {
    fn buffer(&self, len: usize) -> Vec<u8> {
        Sender::buffer(self, len)
    }
    fn send(&self, f: OutFrame) -> bool {
        Sender::send(self, f)
    }
    fn status(&self) -> String {
        let n = self.connections();
        format!(
            "NDI \"{}\" · {n} receiver{} · {} frames",
            self.name,
            if n == 1 { "" } else { "s" },
            self.frames_sent()
        )
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.tx = None; // ends the thread's loop
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layouts_match_the_c_api() {
        // x86-64 / aarch64 sizes of the SDK's structs.
        assert_eq!(std::mem::size_of::<VideoFrameV2>(), 72);
        assert_eq!(std::mem::offset_of!(VideoFrameV2, timecode), 32);
        assert_eq!(std::mem::offset_of!(VideoFrameV2, p_data), 40);
        assert_eq!(std::mem::offset_of!(VideoFrameV2, line_stride_in_bytes), 48);
        assert_eq!(std::mem::offset_of!(VideoFrameV2, timestamp), 64);
        assert_eq!(std::mem::offset_of!(RecvCreateV3, color_format), 16);
        assert_eq!(std::mem::offset_of!(RecvCreateV3, allow_video_fields), 24);
        assert_eq!(std::mem::offset_of!(RecvCreateV3, p_ndi_recv_name), 32);
        assert_eq!(std::mem::offset_of!(SendCreate, clock_video), 16);
        assert_eq!(std::mem::offset_of!(FindCreate, p_groups), 8);
        assert_eq!(FOURCC_RGBA, 0x4142_4752);
    }

    #[test]
    fn fourccs_map_to_pixel_orders() {
        assert_eq!(pixel_of(FOURCC_BGRX), Some(Pixel::Bgrx));
        assert_eq!(pixel_of(FOURCC_RGBA), Some(Pixel::Rgba));
        assert_eq!(pixel_of(fourcc(b"UYVY")), None);
    }

    /// Needs the NDI runtime (skips without): send a gradient as "lavmfx-test",
    /// find it on the network, receive it back.
    #[test]
    fn loopback_send_find_receive() {
        if let Some(e) = runtime_error() {
            eprintln!("skipping: {e}");
            return;
        }
        let tx = Sender::new("lavmfx-test").unwrap();
        let (w, h) = (64u32, 32u32);
        let frame = |tx: &Sender| {
            let mut data = tx.buffer((w * h * 4) as usize);
            for (i, p) in data.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                p.copy_from_slice(&[(i % 64 * 4) as u8, 128, 30, 255]);
            }
            tx.send(OutFrame {
                data,
                width: w,
                height: h,
                stride: w * 4,
                rate: (30, 1),
            });
        };
        let t0 = Instant::now();
        let name = loop {
            frame(&tx);
            if let Some(n) = sources().into_iter().find(|n| n.ends_with("(lavmfx-test)")) {
                break n;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "source not found: {:?}",
                sources()
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let rx = receiver(&name).unwrap();
        let mut seen = 0;
        let got = loop {
            frame(&tx);
            if let Some(f) = rx.latest(&mut seen) {
                break f;
            }
            assert!(t0.elapsed() < Duration::from_secs(20), "no frame: {:?}", rx.status());
            std::thread::sleep(Duration::from_millis(30));
        };
        assert_eq!((got.width, got.height), (w, h));
        let px = &got.planes[0].data[(10 * 4) as usize..][..4];
        // NDI's codec is lossy: close, not exact.
        assert!(
            (px[0] as i32 - 40).abs() < 12 && (px[1] as i32 - 128).abs() < 12,
            "{px:?}"
        );
        assert!(Arc::ptr_eq(&rx, &receiver(&name).unwrap()), "receivers are shared");
        assert!(rx.status().live);
    }

    #[test]
    fn missing_runtime_is_an_error_not_a_crash() {
        match runtime_status() {
            Ok(s) => eprintln!("NDI runtime present: {s}"),
            Err(e) => {
                assert!(e.contains("NDI Runtime not found"), "{e}");
                assert!(Sender::new("x").is_err() && receiver("y").is_err());
                assert!(sources().is_empty());
            }
        }
    }
}
