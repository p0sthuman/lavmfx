//! FFGL (FreeFrame GL 2.x) plugin host: effects, sources (mixers listed only).
//!
//! FFGL plugins are OpenGL; our renderer is wgpu. One thread ("ffgl") owns a
//! headless OpenGL 4.1+ core context (EGL; Linux only for now), loads the
//! plugin libraries and makes every plugin call. The app scans folders with
//! [`FfglHost::scan`] and runs instances with [`FfglHost::process`]: RGBA in,
//! RGBA out (rows top first; flipped for GL, whose origin is bottom-left).
//!
//! ABI from the FFGL SDK 2.2 (`FFGL.h`, BSD licence): `plugMain(code,
//! FFMixed, instance)`; float values travel as their bits in `UIntValue`.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

// ------------------------------------------------------------------ ABI

#[repr(C)]
#[derive(Clone, Copy)]
pub union FFMixed {
    pub u: u32,
    pub p: *mut c_void,
}

type PlugMain = unsafe extern "C" fn(u32, FFMixed, *mut c_void) -> FFMixed;

const FF_GET_INFO: u32 = 0;
const FF_INITIALISE_V2: u32 = 34;
const FF_DEINITIALISE: u32 = 2;
const FF_GET_NUM_PARAMETERS: u32 = 4;
const FF_GET_PARAMETER_NAME: u32 = 5;
const FF_GET_PARAMETER_DEFAULT: u32 = 6;
const FF_SET_PARAMETER: u32 = 8;
const FF_GET_PLUGIN_CAPS: u32 = 10;
const FF_GET_PARAMETER_TYPE: u32 = 15;
const FF_PROCESS_OPENGL: u32 = 17;
const FF_INSTANTIATE_GL: u32 = 18;
const FF_DEINSTANTIATE_GL: u32 = 19;
const FF_SET_TIME: u32 = 20;
const FF_RESIZE: u32 = 23;
const FF_GET_NUM_PARAMETER_ELEMENTS: u32 = 31;
const FF_GET_PARAMETER_ELEMENT_NAME: u32 = 35;
const FF_GET_PARAMETER_ELEMENT_VALUE: u32 = 36;
const FF_SET_BEATINFO: u32 = 38;
const FF_SET_HOSTINFO: u32 = 39;
const FF_GET_RANGE: u32 = 41;
const FF_GET_PARAM_DISPLAY_NAME: u32 = 51;
const FF_SUCCESS: u32 = 0;
const FF_FAIL: u32 = 0xFFFF_FFFF;
const FF_CAP_MINIMUM_INPUT_FRAMES: u32 = 10;
const FF_CAP_MAXIMUM_INPUT_FRAMES: u32 = 11;

pub const FF_TYPE_BOOLEAN: u32 = 0;
pub const FF_TYPE_EVENT: u32 = 1;
pub const FF_TYPE_STANDARD: u32 = 10;
pub const FF_TYPE_OPTION: u32 = 11;
pub const FF_TYPE_INTEGER: u32 = 13;
pub const FF_TYPE_FILE: u32 = 14;
pub const FF_TYPE_TEXT: u32 = 100;

#[repr(C)]
struct PluginInfoStruct {
    api_major: u32,
    api_minor: u32,
    unique_id: [u8; 4],
    name: [u8; 16],
    plugin_type: u32,
}

#[repr(C)]
struct SetParameterStruct {
    number: u32,
    value: FFMixed,
}

#[repr(C)]
struct SetBeatinfoStruct {
    bpm: f32,
    bar_phase: f32,
}

#[repr(C)]
struct SetHostinfoStruct {
    name: *const c_char,
    version: *const c_char,
}

#[repr(C)]
struct GetRangeStruct {
    number: u32,
    min: f32,
    max: f32,
}

#[repr(C)]
struct GetParameterElementStruct {
    number: u32,
    element: u32,
}

#[repr(C)]
struct ViewportStruct {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
struct TextureStruct {
    width: u32,
    height: u32,
    hw_width: u32,
    hw_height: u32,
    handle: u32,
}

#[repr(C)]
struct ProcessOpenGLStruct {
    num_inputs: u32,
    inputs: *mut *mut TextureStruct,
    host_fbo: u32,
}

// ------------------------------------------------------------------ info

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfglKind {
    Effect,
    Source,
    Mixer,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FfglParamInfo {
    pub name: String,
    /// `FF_TYPE_*`.
    pub ty: u32,
    /// Plugin value (normalised 0..1 for standard / colour params).
    pub default: f32,
    /// Display range (`FF_GET_RANGE`); None = 0..1.
    pub range: Option<(f32, f32)>,
    /// Option params: (label, value).
    pub options: Vec<(String, f32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FfglInfo {
    pub path: PathBuf,
    /// 4-character plugin id.
    pub id: String,
    pub name: String,
    pub kind: FfglKind,
    pub params: Vec<FfglParamInfo>,
    /// Why it can't be used.
    pub error: Option<String>,
}

/// One instance call: params as plugin values, RGBA input (None for sources).
pub struct ProcessRequest {
    /// Registry index of the plugin ([`FfglHost::scan`] order).
    pub plugin: usize,
    /// Effect / clip id (keys the instance).
    pub instance: i64,
    pub values: Vec<f32>,
    pub input: Option<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    pub time: f64,
    pub bpm: f32,
    pub bar_phase: f32,
}

enum Cmd {
    Scan(Vec<PathBuf>, Sender<Vec<FfglInfo>>),
    Process(Box<ProcessRequest>, Sender<Option<Vec<u8>>>),
    Retain(Vec<i64>),
}

/// Handle to the FFGL thread (cheap to clone).
#[derive(Clone)]
pub struct FfglHost {
    tx: Sender<Cmd>,
}

impl std::fmt::Debug for FfglHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FfglHost")
    }
}

impl FfglHost {
    /// Start the thread (its GL context is made on first use, on the GPU
    /// whose name contains `gpu_hint` when there are several).
    pub fn start(gpu_hint: Option<String>) -> Self {
        let (tx, rx) = channel();
        let _ = std::thread::Builder::new()
            .name("ffgl".into())
            .spawn(move || thread_main(rx, gpu_hint));
        Self { tx }
    }

    /// Load every plugin in `dirs` (replaces the previous set; instances end).
    pub fn scan(&self, dirs: &[PathBuf]) -> Vec<FfglInfo> {
        let (tx, rx) = channel();
        if self.tx.send(Cmd::Scan(dirs.to_vec(), tx)).is_err() {
            return Vec::new();
        }
        rx.recv_timeout(Duration::from_secs(30)).unwrap_or_default()
    }

    /// Run one instance (blocking); RGBA rows top first. None = failed.
    pub fn process(&self, req: ProcessRequest) -> Option<Vec<u8>> {
        let (tx, rx) = channel();
        self.tx.send(Cmd::Process(Box::new(req), tx)).ok()?;
        rx.recv_timeout(Duration::from_secs(2)).ok().flatten()
    }

    /// End instances whose id isn't in `alive`.
    pub fn retain(&self, alive: Vec<i64>) {
        let _ = self.tx.send(Cmd::Retain(alive));
    }
}

/// File extension of plugins on this OS.
pub fn plugin_extension() -> &'static str {
    if cfg!(target_os = "windows") {
        "dll"
    } else if cfg!(target_os = "macos") {
        "bundle"
    } else {
        "so"
    }
}

// ------------------------------------------------------------------ thread

struct Plugin {
    _lib: libloading::Library,
    main: PlugMain,
    info: FfglInfo,
}

struct Instance {
    plugin: usize,
    ptr: *mut c_void,
    size: (u32, u32),
    values: Vec<f32>,
    gl: Option<gl::Targets>,
}

fn call(main: PlugMain, code: u32, input: FFMixed, inst: *mut c_void) -> FFMixed {
    unsafe { main(code, input, inst) }
}

fn uint(v: u32) -> FFMixed {
    FFMixed { u: v }
}

fn ptr<T>(p: *mut T) -> FFMixed {
    FFMixed { p: p.cast() }
}

fn cstr_at(p: *mut c_void) -> String {
    if p.is_null() || p as usize == FF_FAIL as usize {
        return String::new();
    }
    unsafe { CStr::from_ptr(p as *const c_char) }
        .to_string_lossy()
        .into_owned()
}

fn fixed_str(b: &[u8]) -> String {
    let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_owned()
}

/// Plugin library files under `dir` (2 levels; macOS bundles are folders).
fn plugin_files(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut v: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    v.sort();
    for p in v {
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == plugin_extension() {
            out.push(p);
        } else if p.is_dir() && depth > 0 {
            plugin_files(&p, depth - 1, out);
        }
    }
}

/// The loadable file of a plugin path (`X.bundle/Contents/MacOS/X` on macOS).
fn library_path(p: &Path) -> PathBuf {
    if p.extension().is_some_and(|e| e == "bundle") {
        let stem = p.file_stem().unwrap_or_default();
        return p.join("Contents").join("MacOS").join(stem);
    }
    p.to_path_buf()
}

fn load(path: &Path) -> Result<Plugin, String> {
    let lib = unsafe { libloading::Library::new(library_path(path)) }.map_err(|e| format!("load: {e}"))?;
    let main: PlugMain =
        *unsafe { lib.get::<PlugMain>(b"plugMain\0") }.map_err(|_| "no plugMain (not an FFGL plugin)")?;
    let info = call(main, FF_GET_INFO, uint(0), std::ptr::null_mut());
    let pi = unsafe { info.p } as *const PluginInfoStruct;
    if pi.is_null() {
        return Err("FF_GET_INFO failed".into());
    }
    let pi = unsafe { &*pi };
    if pi.api_major < 2 {
        return Err(format!("FreeFrame {}.{} (only FFGL 2.x)", pi.api_major, pi.api_minor));
    }
    let name = fixed_str(&pi.name);
    let kind = match pi.plugin_type {
        0 => FfglKind::Effect,
        1 => FfglKind::Source,
        _ => FfglKind::Mixer,
    };
    let host_name = c"LAVMFX";
    let host_version = c"0.1";
    let mut host = SetHostinfoStruct {
        name: host_name.as_ptr(),
        version: host_version.as_ptr(),
    };
    call(main, FF_SET_HOSTINFO, ptr(&mut host), std::ptr::null_mut());
    if unsafe { call(main, FF_INITIALISE_V2, uint(0), std::ptr::null_mut()).u } != FF_SUCCESS {
        return Err("FF_INITIALISE_V2 failed".into());
    }
    let n = unsafe { call(main, FF_GET_NUM_PARAMETERS, uint(0), std::ptr::null_mut()).u };
    let mut params = Vec::new();
    for i in 0..n.min(256) {
        let null = std::ptr::null_mut();
        let mut pname = cstr_at(unsafe { call(main, FF_GET_PARAM_DISPLAY_NAME, uint(i), null).p });
        if pname.is_empty() {
            pname = cstr_at(unsafe { call(main, FF_GET_PARAMETER_NAME, uint(i), null).p });
        }
        let ty = unsafe { call(main, FF_GET_PARAMETER_TYPE, uint(i), null).u };
        let default = f32::from_bits(unsafe { call(main, FF_GET_PARAMETER_DEFAULT, uint(i), null).u });
        let mut r = GetRangeStruct {
            number: i,
            min: 0.0,
            max: 1.0,
        };
        let range = (unsafe { call(main, FF_GET_RANGE, ptr(&mut r), null).u } == FF_SUCCESS && r.max > r.min)
            .then_some((r.min, r.max));
        let mut options = Vec::new();
        if ty == FF_TYPE_OPTION {
            let k = unsafe { call(main, FF_GET_NUM_PARAMETER_ELEMENTS, uint(i), null).u };
            for e in 0..k.min(256) {
                let mut q = GetParameterElementStruct { number: i, element: e };
                let label = cstr_at(unsafe { call(main, FF_GET_PARAMETER_ELEMENT_NAME, ptr(&mut q), null).p });
                let mut q = GetParameterElementStruct { number: i, element: e };
                let value = f32::from_bits(unsafe { call(main, FF_GET_PARAMETER_ELEMENT_VALUE, ptr(&mut q), null).u });
                options.push((label, value));
            }
        }
        params.push(FfglParamInfo {
            name: if pname.is_empty() {
                format!("Param {}", i + 1)
            } else {
                pname
            },
            ty,
            default: if default.is_finite() { default } else { 0.0 },
            range,
            options,
        });
    }
    let min_inputs = unsafe {
        call(
            main,
            FF_GET_PLUGIN_CAPS,
            uint(FF_CAP_MINIMUM_INPUT_FRAMES),
            std::ptr::null_mut(),
        )
        .u
    };
    let max_inputs = unsafe {
        call(
            main,
            FF_GET_PLUGIN_CAPS,
            uint(FF_CAP_MAXIMUM_INPUT_FRAMES),
            std::ptr::null_mut(),
        )
        .u
    };
    log::debug!(
        "FFGL {name}: {kind:?}, inputs {min_inputs}..{max_inputs}, {} params",
        params.len()
    );
    Ok(Plugin {
        _lib: lib,
        main,
        info: FfglInfo {
            path: path.to_path_buf(),
            id: fixed_str(&pi.unique_id),
            name,
            kind,
            params,
            error: (kind == FfglKind::Mixer).then(|| "FFGL mixers aren't supported yet".to_owned()),
        },
    })
}

fn thread_main(rx: Receiver<Cmd>, gpu_hint: Option<String>) {
    // Aligned with the last scan's infos (None = failed to load).
    let mut plugins: Vec<Option<Plugin>> = Vec::new();
    let mut instances: HashMap<i64, Instance> = HashMap::new();
    let mut ctx: Option<Result<gl::Context, String>> = None;
    let deinstantiate = |plugins: &[Option<Plugin>], inst: Instance, ctx: &Option<Result<gl::Context, String>>| {
        if let Some(Some(p)) = plugins.get(inst.plugin) {
            call(p.main, FF_DEINSTANTIATE_GL, uint(0), inst.ptr);
        }
        if let (Some(Ok(c)), Some(t)) = (ctx, inst.gl) {
            c.free(t);
        }
    };
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Scan(dirs, reply) => {
                // A GL context first: plugins may touch GL while initialising.
                if ctx.is_none() {
                    ctx = Some(gl::Context::new(gpu_hint.as_deref()));
                }
                for (_, i) in instances.drain() {
                    deinstantiate(&plugins, i, &ctx);
                }
                for p in plugins.drain(..).flatten() {
                    call(p.main, FF_DEINITIALISE, uint(0), std::ptr::null_mut());
                }
                let mut infos = Vec::new();
                let gl_err = match &ctx {
                    Some(Err(e)) => Some(e.clone()),
                    _ => None,
                };
                for d in &dirs {
                    let mut files = Vec::new();
                    plugin_files(d, 2, &mut files);
                    for f in files {
                        match load(&f) {
                            Ok(mut p) => {
                                if p.info.error.is_none() {
                                    p.info.error = gl_err.clone();
                                }
                                infos.push(p.info.clone());
                                plugins.push(Some(p));
                            }
                            Err(e) => {
                                let name = f
                                    .file_stem()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                log::warn!("FFGL {}: {e}", f.display());
                                infos.push(FfglInfo {
                                    path: f,
                                    id: String::new(),
                                    name,
                                    kind: FfglKind::Effect,
                                    params: Vec::new(),
                                    error: Some(e),
                                });
                                plugins.push(None);
                            }
                        }
                    }
                }
                let _ = reply.send(infos);
            }
            Cmd::Process(req, reply) => {
                let out = (|| {
                    let Some(Ok(c)) = &ctx else { return None };
                    let p = plugins.get(req.plugin)?.as_ref()?;
                    let inst = match instances.get_mut(&req.instance) {
                        Some(i) if i.plugin == req.plugin => i,
                        _ => {
                            if let Some(old) = instances.remove(&req.instance) {
                                deinstantiate(&plugins, old, &ctx);
                            }
                            let mut vp = ViewportStruct {
                                x: 0,
                                y: 0,
                                width: req.width,
                                height: req.height,
                            };
                            let ip = unsafe { call(p.main, FF_INSTANTIATE_GL, ptr(&mut vp), std::ptr::null_mut()).p };
                            if ip.is_null() || ip as usize == FF_FAIL as usize {
                                return None;
                            }
                            instances.insert(
                                req.instance,
                                Instance {
                                    plugin: req.plugin,
                                    ptr: ip,
                                    size: (req.width, req.height),
                                    values: Vec::new(),
                                    gl: None,
                                },
                            );
                            instances.get_mut(&req.instance).unwrap()
                        }
                    };
                    if inst.size != (req.width, req.height) {
                        let mut vp = ViewportStruct {
                            x: 0,
                            y: 0,
                            width: req.width,
                            height: req.height,
                        };
                        call(p.main, FF_RESIZE, ptr(&mut vp), inst.ptr);
                        inst.size = (req.width, req.height);
                    }
                    // Changed params only (text / file params aren't floats: skipped).
                    for (i, v) in req.values.iter().enumerate() {
                        let ty = p.info.params.get(i).map_or(FF_TYPE_STANDARD, |q| q.ty);
                        if ty == FF_TYPE_TEXT || ty == FF_TYPE_FILE || inst.values.get(i) == Some(v) {
                            continue;
                        }
                        let mut s = SetParameterStruct {
                            number: i as u32,
                            value: uint(v.to_bits()),
                        };
                        call(p.main, FF_SET_PARAMETER, ptr(&mut s), inst.ptr);
                    }
                    inst.values.clone_from(&req.values);
                    let mut t = req.time;
                    call(p.main, FF_SET_TIME, ptr(&mut t), inst.ptr);
                    let mut b = SetBeatinfoStruct {
                        bpm: req.bpm,
                        bar_phase: req.bar_phase,
                    };
                    call(p.main, FF_SET_BEATINFO, ptr(&mut b), inst.ptr);
                    let targets = inst.gl.get_or_insert_with(|| c.targets());
                    c.run(targets, req.width, req.height, req.input.as_deref(), |fbo, input| {
                        let mut tex = input.map(|h| TextureStruct {
                            width: req.width,
                            height: req.height,
                            hw_width: req.width,
                            hw_height: req.height,
                            handle: h,
                        });
                        let mut list: [*mut TextureStruct; 1] = [tex.as_mut().map_or(std::ptr::null_mut(), |t| t)];
                        let mut po = ProcessOpenGLStruct {
                            num_inputs: u32::from(tex.is_some()),
                            inputs: list.as_mut_ptr(),
                            host_fbo: fbo,
                        };
                        unsafe { call(p.main, FF_PROCESS_OPENGL, ptr(&mut po), inst.ptr).u == FF_SUCCESS }
                    })
                })();
                let _ = reply.send(out);
            }
            Cmd::Retain(alive) => {
                let dead: Vec<i64> = instances.keys().filter(|k| !alive.contains(k)).copied().collect();
                for k in dead {
                    if let Some(i) = instances.remove(&k) {
                        deinstantiate(&plugins, i, &ctx);
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ GL

#[cfg(target_os = "linux")]
mod gl {
    use glow::HasContext;

    /// Rows bottom-up (GL) ⇄ top-down, whole rows at a time.
    fn flip_rows(px: &[u8], row: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(px.len());
        for r in px.chunks_exact(row).rev() {
            out.extend_from_slice(r);
        }
        out
    }

    /// Headless OpenGL 4.1+ core context (EGL).
    pub struct Context {
        gl: glow::Context,
        _egl: khronos_egl::DynamicInstance<khronos_egl::EGL1_5>,
    }

    /// Per instance: input texture, output texture + framebuffer.
    pub struct Targets {
        input: glow::Texture,
        output: glow::Texture,
        fbo: glow::Framebuffer,
        size: (u32, u32),
    }

    type Egl = khronos_egl::DynamicInstance<khronos_egl::EGL1_5>;

    /// An OpenGL 4.1 core context made current on `display` (surfaceless, else
    /// a 1×1 pbuffer); returns the GL renderer string.
    fn try_display(lib: &Egl, display: khronos_egl::Display) -> Result<(glow::Context, String), String> {
        use khronos_egl as egl;
        lib.initialize(display).map_err(|e| format!("EGL initialise: {e}"))?;
        lib.bind_api(egl::OPENGL_API)
            .map_err(|e| format!("EGL OpenGL API: {e}"))?;
        // Headless: pbuffer-capable, else any surface type (EGL's default is window).
        let config = [egl::PBUFFER_BIT, 0]
            .iter()
            .find_map(|&surface| {
                let attrs = [
                    egl::SURFACE_TYPE,
                    surface,
                    egl::RENDERABLE_TYPE,
                    egl::OPENGL_BIT,
                    egl::NONE,
                ];
                lib.choose_first_config(display, &attrs).ok().flatten()
            })
            .ok_or("no EGL config for OpenGL")?;
        let ctx_attrs = [
            egl::CONTEXT_MAJOR_VERSION,
            4,
            egl::CONTEXT_MINOR_VERSION,
            1,
            egl::CONTEXT_OPENGL_PROFILE_MASK,
            egl::CONTEXT_OPENGL_CORE_PROFILE_BIT,
            egl::NONE,
        ];
        let ctx = lib
            .create_context(display, config, None, &ctx_attrs)
            .map_err(|e| format!("OpenGL 4.1 core context: {e}"))?;
        if lib.make_current(display, None, None, Some(ctx)).is_err() {
            let surface = lib
                .create_pbuffer_surface(display, config, &[egl::WIDTH, 1, egl::HEIGHT, 1, egl::NONE])
                .map_err(|e| format!("pbuffer: {e}"))?;
            lib.make_current(display, Some(surface), Some(surface), Some(ctx))
                .map_err(|e| format!("make current: {e}"))?;
        }
        let gl = unsafe {
            glow::Context::from_loader_function(|s| lib.get_proc_address(s).map_or(std::ptr::null(), |f| f as *const _))
        };
        let renderer = unsafe { gl.get_parameter_string(glow::RENDERER) };
        Ok((gl, renderer))
    }

    impl Context {
        /// `gpu_hint`: part of the GPU name the app renders on (e.g. "NVIDIA"):
        /// on hybrid laptops EGL's default display is often the other GPU.
        pub fn new(gpu_hint: Option<&str>) -> Result<Self, String> {
            use khronos_egl as egl;
            let lib = unsafe { Egl::load_required() }.map_err(|e| format!("EGL 1.5 not available: {e}"))?;
            // EGL devices (EGL_EXT_device_enumeration + platform_device).
            const PLATFORM_DEVICE: egl::Enum = 0x313F;
            type QueryDevices = unsafe extern "C" fn(i32, *mut *mut std::ffi::c_void, *mut i32) -> u32;
            let mut found: Option<(glow::Context, String)> = None;
            if let (Some(hint), Some(q)) = (gpu_hint, lib.get_proc_address("eglQueryDevicesEXT")) {
                let query: QueryDevices = unsafe { std::mem::transmute(q) };
                let mut devs = [std::ptr::null_mut(); 16];
                let mut n = 0;
                let ok = unsafe { query(16, devs.as_mut_ptr(), &mut n) };
                log::debug!("FFGL: eglQueryDevicesEXT ok {ok} devices {n}");
                if ok != 0 {
                    for d in devs.iter().take(n.max(0) as usize) {
                        let Ok(display) =
                            (unsafe { lib.get_platform_display(PLATFORM_DEVICE, *d, &[egl::ATTRIB_NONE]) })
                        else {
                            continue;
                        };
                        let tried = try_display(&lib, display);
                        log::debug!("FFGL: EGL device: {:?}", tried.as_ref().map(|t| &t.1));
                        match tried {
                            Ok((gl, r)) if r.to_lowercase().contains(&hint.to_lowercase()) => {
                                found = Some((gl, r));
                                break;
                            }
                            Ok(_) | Err(_) => {
                                let _ = lib.make_current(display, None, None, None);
                                let _ = lib.terminate(display);
                            }
                        }
                    }
                }
            }
            let (gl, renderer) = match found {
                Some(f) => f,
                None => {
                    let display = unsafe { lib.get_display(egl::DEFAULT_DISPLAY) }.ok_or("no EGL display")?;
                    try_display(&lib, display)?
                }
            };
            log::info!("FFGL: OpenGL {} on {renderer}", unsafe {
                gl.get_parameter_string(glow::VERSION)
            });
            Ok(Self { gl, _egl: lib })
        }

        pub fn targets(&self) -> Targets {
            let gl = &self.gl;
            unsafe {
                Targets {
                    input: gl.create_texture().unwrap(),
                    output: gl.create_texture().unwrap(),
                    fbo: gl.create_framebuffer().unwrap(),
                    size: (0, 0),
                }
            }
        }

        pub fn free(&self, t: Targets) {
            unsafe {
                self.gl.delete_texture(t.input);
                self.gl.delete_texture(t.output);
                self.gl.delete_framebuffer(t.fbo);
            }
        }

        fn alloc(&self, tex: glow::Texture, w: u32, h: u32) {
            let gl = &self.gl;
            unsafe {
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    w as i32,
                    h as i32,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                for (k, v) in [
                    (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                    (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                    (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                    (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
                ] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
                }
            }
        }

        /// Upload `input` (flipped for GL), call `process(fbo, input handle)`
        /// with the output bound, read it back (flipped back).
        pub fn run(
            &self,
            t: &mut Targets,
            w: u32,
            h: u32,
            input: Option<&[u8]>,
            process: impl FnOnce(u32, Option<u32>) -> bool,
        ) -> Option<Vec<u8>> {
            let gl = &self.gl;
            let row = w as usize * 4;
            unsafe {
                if t.size != (w, h) {
                    self.alloc(t.input, w, h);
                    self.alloc(t.output, w, h);
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(t.fbo));
                    gl.framebuffer_texture_2d(
                        glow::FRAMEBUFFER,
                        glow::COLOR_ATTACHMENT0,
                        glow::TEXTURE_2D,
                        Some(t.output),
                        0,
                    );
                    t.size = (w, h);
                }
                let has_input = match input {
                    Some(px) if px.len() >= row * h as usize => {
                        let flipped = flip_rows(&px[..row * h as usize], row);
                        gl.bind_texture(glow::TEXTURE_2D, Some(t.input));
                        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                        gl.tex_sub_image_2d(
                            glow::TEXTURE_2D,
                            0,
                            0,
                            0,
                            w as i32,
                            h as i32,
                            glow::RGBA,
                            glow::UNSIGNED_BYTE,
                            glow::PixelUnpackData::Slice(Some(&flipped)),
                        );
                        true
                    }
                    _ => false,
                };
                gl.bind_texture(glow::TEXTURE_2D, None);
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(t.fbo));
                gl.viewport(0, 0, w as i32, h as i32);
                gl.clear_color(0.0, 0.0, 0.0, 0.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
                let fbo = t.fbo.0.get();
                let input_handle = has_input.then(|| t.input.0.get());
                if !process(fbo, input_handle) {
                    return None;
                }
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(t.fbo));
                let mut out = vec![0u8; row * h as usize];
                gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                gl.read_pixels(
                    0,
                    0,
                    w as i32,
                    h as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut out)),
                );
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                Some(flip_rows(&out, row))
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod gl {
    pub struct Context;
    pub struct Targets;
    impl Context {
        pub fn new(_: Option<&str>) -> Result<Self, String> {
            Err("FFGL hosting isn't built for this OS yet (needs a Windows / macOS session)".into())
        }
        pub fn targets(&self) -> Targets {
            Targets
        }
        pub fn free(&self, _: Targets) {}
        pub fn run(
            &self,
            _: &mut Targets,
            _: u32,
            _: u32,
            _: Option<&[u8]>,
            _: impl FnOnce(u32, Option<u32>) -> bool,
        ) -> Option<Vec<u8>> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_sizes() {
        assert_eq!(std::mem::size_of::<FFMixed>(), std::mem::size_of::<*mut c_void>());
        assert_eq!(std::mem::size_of::<PluginInfoStruct>(), 4 + 4 + 4 + 16 + 4);
        assert_eq!(std::mem::size_of::<TextureStruct>(), 20);
        assert_eq!(std::mem::size_of::<GetRangeStruct>(), 12);
    }

    /// Needs FFGL plugins built for this OS: `LAVMFX_FFGL_TEST=<folder>` (skips otherwise).
    #[test]
    fn loads_and_runs_sample_plugins() {
        let Some(dir) = std::env::var_os("LAVMFX_FFGL_TEST") else {
            eprintln!("skipping: set LAVMFX_FFGL_TEST to a folder of FFGL plugins");
            return;
        };
        let host = FfglHost::start(None);
        let infos = host.scan(&[PathBuf::from(dir)]);
        for i in &infos {
            eprintln!(
                "{} [{}] {:?} {:?} params {:?}",
                i.name,
                i.id,
                i.kind,
                i.error,
                i.params
                    .iter()
                    .map(|p| (&p.name, p.ty, p.default, p.range))
                    .collect::<Vec<_>>()
            );
        }
        assert!(!infos.is_empty());
        let (w, h) = (64u32, 32u32);
        for (idx, i) in infos.iter().enumerate().filter(|(_, i)| i.error.is_none()) {
            let input = (i.kind == FfglKind::Effect).then(|| [255u8, 0, 0, 255].repeat((w * h) as usize));
            let out = host.process(ProcessRequest {
                plugin: idx,
                instance: idx as i64 + 1,
                values: i.params.iter().map(|p| p.default).collect(),
                input,
                width: w,
                height: h,
                time: 0.5,
                bpm: 120.0,
                bar_phase: 0.0,
            });
            let out = out.unwrap_or_else(|| panic!("{}: process failed", i.name));
            assert_eq!(out.len(), (w * h * 4) as usize);
            eprintln!(
                "{}: centre {:?}",
                i.name,
                &out[((h / 2 * w + w / 2) * 4) as usize..][..4]
            );
        }
    }
}
