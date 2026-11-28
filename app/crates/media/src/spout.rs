//! **Spout** 2 senders and receivers (Windows), the `imp` of `share.rs`.
//!
//! A sender is a Direct3D 11 shared texture plus named kernel objects, as in the
//! Spout2 SDK (BSD licence):
//! * `SpoutSenderNames`: the sender list, one 256-byte NUL-terminated slot each.
//! * `ActiveSenderName`: the one receivers pick by default.
//! * `<name>`: 280 bytes: share handle, width, height, DXGI format, usage,
//!   a 256-byte description (the program), partner id.
//! * `<map>_mutex` guards a map, `<name>_SpoutAccessMutex` the texture, and
//!   `<name>_Count_Semaphore` counts frames.
//!
//! Frames go through system memory (staging texture here, upload by the app),
//! so this works whatever API and GPU the renderer uses.

use std::collections::BTreeSet;
use std::ffi::{CString, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver as ChannelReceiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, INVALID_HANDLE_VALUE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_RESOURCE_MISC_SHARED, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIResource};
use windows::Win32::System::Memory::{
    CreateFileMappingA, FILE_MAP_ALL_ACCESS, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    OpenFileMappingA, PAGE_READWRITE, UnmapViewOfFile, VirtualQuery,
};
use windows::Win32::System::Threading::{
    CreateMutexA, CreateSemaphoreA, ReleaseMutex, ReleaseSemaphore, WaitForSingleObject,
};
use windows::core::{Interface, PCSTR};

use super::ShareSource;
use crate::live::{FrameSink, OutFrame, Pixel, Receiver, Shared, publish, rgba_frame, set_error, to_rgba};

const NAME_LEN: usize = 256;
const MAX_SENDERS: usize = 64;
const INFO_LEN: usize = 280;
const NAMES_MAP: &str = "SpoutSenderNames";
const ACTIVE_MAP: &str = "ActiveSenderName";

// ------------------------------------------------------------- kernel objects

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn c_name(name: &str) -> Option<CString> {
    CString::new(name).ok()
}

fn mutex(name: &str) -> Option<Handle> {
    let n = c_name(name)?;
    // SAFETY: `n` is NUL-terminated. Opens the mutex when it exists.
    unsafe { CreateMutexA(None, false, PCSTR(n.as_ptr().cast())) }
        .ok()
        .map(Handle)
}

/// Run `f` while holding `m`; None when it stays locked for `ms`.
fn locked<T>(m: &Handle, ms: u32, f: impl FnOnce() -> T) -> Option<T> {
    // SAFETY: a mutex handle we own; released by the same thread.
    unsafe {
        let r = WaitForSingleObject(m.0, ms);
        if r != WAIT_OBJECT_0 && r != WAIT_ABANDONED {
            return None;
        }
        let out = f();
        let _ = ReleaseMutex(m.0);
        Some(out)
    }
}

/// A named shared memory block, mapped. It lives while somebody holds it open.
struct Map {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
    guard: Handle,
    _map: Handle,
}

impl Map {
    fn create(name: &str, len: usize) -> Option<Self> {
        let n = c_name(name)?;
        // SAFETY: `n` is NUL-terminated; backed by the paging file.
        let h = unsafe {
            CreateFileMappingA(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                len as u32,
                PCSTR(n.as_ptr().cast()),
            )
        }
        .ok()?;
        Self::view(name, Handle(h))
    }

    fn open(name: &str) -> Option<Self> {
        let n = c_name(name)?;
        // SAFETY: `n` is NUL-terminated.
        let h = unsafe { OpenFileMappingA(FILE_MAP_ALL_ACCESS.0, false, PCSTR(n.as_ptr().cast())) }.ok()?;
        Self::view(name, Handle(h))
    }

    fn view(name: &str, map: Handle) -> Option<Self> {
        let guard = mutex(&format!("{name}_mutex"))?;
        // SAFETY: maps the whole block; the size is asked from the system.
        unsafe {
            let view = MapViewOfFile(map.0, FILE_MAP_ALL_ACCESS, 0, 0, 0);
            if view.Value.is_null() {
                return None;
            }
            let mut info = MEMORY_BASIC_INFORMATION::default();
            let n = VirtualQuery(Some(view.Value), &mut info, size_of::<MEMORY_BASIC_INFORMATION>());
            let len = if n == 0 { 0 } else { info.RegionSize };
            Some(Self {
                view,
                len,
                guard,
                _map: map,
            })
        }
    }

    fn read(&self, len: usize) -> Option<Vec<u8>> {
        let len = len.min(self.len);
        locked(&self.guard, 67, || {
            let mut out = vec![0u8; len];
            // SAFETY: `len` bytes are inside the view.
            unsafe { std::ptr::copy_nonoverlapping(self.view.Value as *const u8, out.as_mut_ptr(), len) };
            out
        })
    }

    /// Read, change and write back under one lock.
    fn update<T>(&self, len: usize, f: impl FnOnce(&mut [u8]) -> T) -> Option<T> {
        let len = len.min(self.len);
        locked(&self.guard, 67, || {
            // SAFETY: `len` bytes inside the view, exclusive while locked.
            f(unsafe { std::slice::from_raw_parts_mut(self.view.Value as *mut u8, len) })
        })
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        // SAFETY: the view we mapped.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
        }
    }
}

// ------------------------------------------------------------------ the lists

/// The sender list: 256-byte slots up to the first empty one.
fn parse_names(buf: &[u8]) -> BTreeSet<String> {
    buf.as_chunks::<NAME_LEN>()
        .0
        .iter()
        .map(|slot| {
            let end = slot.iter().position(|b| *b == 0).unwrap_or(NAME_LEN - 1);
            String::from_utf8_lossy(&slot[..end]).into_owned()
        })
        .take_while(|n| !n.is_empty())
        .collect()
}

fn write_names(buf: &mut [u8], names: &BTreeSet<String>) {
    buf.fill(0);
    for (slot, name) in buf.as_chunks_mut::<NAME_LEN>().0.iter_mut().zip(names) {
        let n = name.len().min(NAME_LEN - 1);
        slot[..n].copy_from_slice(&name.as_bytes()[..n]);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Info {
    handle: u32,
    width: u32,
    height: u32,
    format: u32,
}

impl Info {
    fn parse(b: &[u8]) -> Option<Self> {
        let at = |i: usize| Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?));
        let info = Self {
            handle: at(0)?,
            width: at(4)?,
            height: at(8)?,
            format: at(12)?,
        };
        let size = 1..=16384;
        (info.handle != 0 && size.contains(&info.width) && size.contains(&info.height)).then_some(info)
    }
}

/// Info of a running sender (None: no such sender). The map is closed again:
/// holding it open would keep a sender that quit looking alive.
fn info(name: &str) -> Option<Info> {
    Info::parse(&Map::open(name)?.read(INFO_LEN)?)
}

fn registered() -> BTreeSet<String> {
    Map::open(NAMES_MAP)
        .and_then(|m| m.read(MAX_SENDERS * NAME_LEN))
        .map(|b| parse_names(&b))
        .unwrap_or_default()
}

/// Names of our own outputs, left out of [`sources`].
static OURS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

// --------------------------------------------------------------------- D3D11

struct Gpu {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
}

fn adapters() -> Result<Vec<IDXGIAdapter1>, String> {
    // SAFETY: plain DXGI enumeration.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("DXGI: {e}"))?;
        let mut all: Vec<(usize, IDXGIAdapter1)> = (0..)
            .map_while(|i| factory.EnumAdapters1(i).ok())
            .map(|a| (a.GetDesc1().map_or(0, |d| d.DedicatedVideoMemory), a))
            .collect();
        // The big GPU first: where the renderer and other video apps run.
        all.sort_by_key(|(memory, _)| std::cmp::Reverse(*memory));
        Ok(all.into_iter().map(|(_, a)| a).collect())
    }
}

fn gpu_on(adapter: &IDXGIAdapter1) -> Result<Gpu, String> {
    let (mut device, mut context) = (None, None);
    // SAFETY: out pointers to locals.
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|e| format!("Direct3D 11: {e}"))?;
    Ok(Gpu {
        device: device.ok_or("no Direct3D 11 device")?,
        context: context.ok_or("no Direct3D 11 context")?,
    })
}

impl Gpu {
    fn best() -> Result<Self, String> {
        let mut last = "no graphics adapter".to_string();
        for a in adapters()? {
            match gpu_on(&a) {
                Ok(g) => return Ok(g),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn texture(&self, w: u32, h: u32, format: u32, staging: bool) -> Result<ID3D11Texture2D, String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT(format as i32),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: if staging {
                D3D11_USAGE_STAGING
            } else {
                D3D11_USAGE_DEFAULT
            },
            BindFlags: if staging {
                0
            } else {
                (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32
            },
            CPUAccessFlags: if staging { D3D11_CPU_ACCESS_READ.0 as u32 } else { 0 },
            MiscFlags: if staging {
                0
            } else {
                D3D11_RESOURCE_MISC_SHARED.0 as u32
            },
        };
        let mut t = None;
        // SAFETY: a valid description, no initial data.
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut t)) }.map_err(|e| format!("texture: {e}"))?;
        t.ok_or_else(|| "no texture".into())
    }
}

// -------------------------------------------------------------------- pixels

/// The DXGI (or Direct3D 9) format a staging copy uses for a sender's format.
fn copy_format(format: u32) -> u32 {
    match format {
        // 0 and D3DFMT_A8R8G8B8 / X8R8G8B8 (old senders) are BGRA / BGRX.
        0 | 21 => 87,
        22 => 88,
        f => f,
    }
}

fn half_to_u8(h: u16) -> u8 {
    let (sign, exp, frac) = (h >> 15, (h >> 10) & 31, f32::from(h & 1023));
    let v = match exp {
        0 => frac * 2f32.powi(-24),
        31 => 1.0,
        e => (1.0 + frac / 1024.0) * 2f32.powi(i32::from(e) - 15),
    };
    if sign == 1 {
        0
    } else {
        (v.min(1.0) * 255.0).round() as u8
    }
}

/// Mapped rows of `format` → tight RGBA. None = a format we don't read.
fn rgba(src: &[u8], w: usize, h: usize, stride: usize, format: u32) -> Option<Vec<u8>> {
    let px = match format {
        28 | 29 => Some(Pixel::Rgba),
        87 | 91 => Some(Pixel::Bgra),
        88 | 93 => Some(Pixel::Bgrx),
        _ => None,
    };
    if let Some(px) = px {
        return to_rgba(src, w, h, stride, px);
    }
    // Deeper formats: R16G16B16A16 float (10) / unorm (11), R10G10B10A2 (24).
    let size = match format {
        10 | 11 => 8,
        24 => 4,
        _ => return None,
    };
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for p in src.get(y * stride..y * stride + w * size)?.chunks_exact(size) {
            if format == 24 {
                let v = u32::from_le_bytes(p.try_into().ok()?);
                let ten = |s: u32| ((v >> s) & 1023) >> 2;
                out.extend_from_slice(&[ten(0) as u8, ten(10) as u8, ten(20) as u8, ((v >> 30) * 85) as u8]);
                continue;
            }
            for c in p.as_chunks::<2>().0 {
                let v = u16::from_le_bytes(*c);
                out.push(if format == 10 { half_to_u8(v) } else { (v >> 8) as u8 });
            }
        }
    }
    Some(out)
}

/// RGBA rows of `stride` bytes → tight BGRA in `out`.
fn to_bgra(src: &[u8], w: usize, h: usize, stride: usize, out: &mut Vec<u8>) -> bool {
    if w == 0 || h == 0 || stride < w * 4 || src.len() < stride * (h - 1) + w * 4 {
        return false;
    }
    out.resize(w * h * 4, 0);
    for (y, row) in out.chunks_exact_mut(w * 4).enumerate() {
        let source = src[y * stride..].as_chunks::<4>().0;
        for (d, s) in row.as_chunks_mut::<4>().0.iter_mut().zip(source) {
            *d = [s[2], s[1], s[0], s[3]];
        }
    }
    true
}

// ----------------------------------------------------------------------- API

pub fn unavailable() -> Option<String> {
    adapters().err()
}

pub fn sources() -> Vec<ShareSource> {
    let ours = OURS.lock().unwrap().clone();
    registered()
        .into_iter()
        // Entries of senders that crashed have no info block any more.
        .filter(|n| !ours.contains(n) && info(n).is_some())
        .map(|n| ShareSource {
            name: n.clone(),
            label: n,
        })
        .collect()
}

pub fn receiver(name: &str) -> Result<Arc<Receiver>, String> {
    if info(name).is_none() {
        return Err(format!("no Spout sender \"{name}\" (is the app sending?)"));
    }
    let shared: Shared = Default::default();
    let stop = Arc::new(AtomicBool::new(false));
    let (source, s, quit) = (name.to_owned(), shared.clone(), stop.clone());
    let thread = std::thread::Builder::new()
        .name("spout-in".into())
        .spawn(move || receive(&source, &s, &quit))
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(Receiver::new(name, shared, move || {
        stop.store(true, Ordering::Relaxed);
        let _ = thread.join();
    })))
}

/// A sender's texture opened on the adapter that has it, plus our staging copy.
struct Opened {
    info: Info,
    gpu: Gpu,
    texture: ID3D11Texture2D,
    staging: ID3D11Texture2D,
}

fn open(i: Info) -> Result<Opened, String> {
    let mut last = "no graphics adapter".to_string();
    // A shared texture opens only on the GPU that made it: try each.
    for adapter in adapters()? {
        let gpu = match gpu_on(&adapter) {
            Ok(g) => g,
            Err(e) => {
                last = e;
                continue;
            }
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        let handle = HANDLE(i.handle as usize as *mut c_void);
        // SAFETY: the handle comes from the sender's info block; a stale one fails.
        match unsafe { gpu.device.OpenSharedResource(handle, &mut texture) } {
            Ok(()) => {
                let Some(texture) = texture else { continue };
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                // SAFETY: out pointer to a local.
                unsafe { texture.GetDesc(&mut desc) };
                // The texture itself says what it is: info blocks can lag.
                let info = Info {
                    width: desc.Width,
                    height: desc.Height,
                    format: desc.Format.0 as u32,
                    ..i
                };
                let staging = gpu.texture(info.width, info.height, copy_format(info.format), true)?;
                return Ok(Opened {
                    info,
                    gpu,
                    texture,
                    staging,
                });
            }
            Err(e) => last = format!("open the shared texture: {e}"),
        }
    }
    Err(last)
}

impl Opened {
    /// Copy the current picture out as RGBA.
    fn read(&self, access: &Handle) -> Result<Option<Vec<u8>>, String> {
        let ctx = &self.gpu.context;
        // SAFETY: both textures belong to this device and have equal size / format.
        let copied = locked(access, 67, || unsafe {
            ctx.CopyResource(&self.staging, &self.texture);
            ctx.Flush();
        });
        if copied.is_none() {
            return Ok(None); // the sender is writing
        }
        let (w, h) = (self.info.width as usize, self.info.height as usize);
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: a staging texture; unmapped below.
        unsafe { ctx.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut m)) }.map_err(|e| format!("read: {e}"))?;
        let stride = m.RowPitch as usize;
        // SAFETY: the mapping holds `h` rows of `stride` bytes.
        let src = unsafe { std::slice::from_raw_parts(m.pData as *const u8, stride * h) };
        let out = rgba(src, w, h, stride, copy_format(self.info.format));
        // SAFETY: mapped above.
        unsafe { ctx.Unmap(&self.staging, 0) };
        out.map(Some)
            .ok_or_else(|| format!("unsupported Spout texture format {}", self.info.format))
    }
}

/// Frames the sender has counted (0: it doesn't count).
fn frame_count(semaphore: &Handle) -> i32 {
    let mut count = 0;
    // SAFETY: take one and give it back, which tells us the value before.
    unsafe {
        if WaitForSingleObject(semaphore.0, 0) == WAIT_OBJECT_0 {
            let _ = ReleaseSemaphore(semaphore.0, 1, Some(&mut count));
        }
    }
    count
}

/// The frame-count semaphore of a sender, made if it isn't there yet.
fn count_semaphore(name: &str) -> Option<Handle> {
    let n = c_name(&format!("{name}_Count_Semaphore"))?;
    // SAFETY: `n` is NUL-terminated.
    unsafe { CreateSemaphoreA(None, 1, i32::MAX, PCSTR(n.as_ptr().cast())) }
        .ok()
        .map(Handle)
}

fn receive(name: &str, shared: &Shared, quit: &AtomicBool) {
    let (Some(access), Some(counter)) = (mutex(&format!("{name}_SpoutAccessMutex")), count_semaphore(name)) else {
        set_error(shared, "can't open the sender's locks");
        return;
    };
    let mut opened: Option<Opened> = None;
    let mut wanted: Option<Info> = None;
    let (mut seen, mut last) = (0, Instant::now());
    while !quit.load(Ordering::Relaxed) {
        let Some(now) = info(name) else {
            // Gone: wait for it to come back.
            opened = None;
            wanted = None;
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        if wanted != Some(now) || opened.is_none() {
            match open(now) {
                Ok(o) => opened = Some(o),
                Err(e) => {
                    set_error(shared, e);
                    opened = None;
                    std::thread::sleep(Duration::from_millis(250));
                    continue;
                }
            }
            wanted = Some(now);
        }
        let count = frame_count(&counter);
        // Counting senders: only new frames. Others: at most 60 a second.
        let new = if count > 1 {
            count != seen
        } else {
            last.elapsed() >= Duration::from_millis(16)
        };
        if new && let Some(o) = &opened {
            match o.read(&access) {
                Ok(Some(data)) => {
                    publish(shared, rgba_frame(o.info.width, o.info.height, data), 0.0);
                    (seen, last) = (count, Instant::now());
                }
                Ok(None) => {}
                Err(e) => {
                    set_error(shared, e);
                    opened = None;
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

// -------------------------------------------------------------------- output

/// Our registration: the maps stay open while we send.
struct Registration {
    name: String,
    names: Map,
    _info: Map,
    _active: Option<Map>,
}

impl Registration {
    /// Register `wanted` (or `wanted_1`, … when taken) for a shared texture.
    fn new(wanted: &str, handle: u32, width: u32, height: u32) -> Result<Self, String> {
        let names = Map::create(NAMES_MAP, MAX_SENDERS * NAME_LEN).ok_or("can't open the Spout sender list")?;
        let wanted: String = wanted.chars().filter(|c| *c != '\0').take(200).collect();
        let name = names
            .update(MAX_SENDERS * NAME_LEN, |buf| {
                let mut set = parse_names(buf);
                set.retain(|n| info(n).is_some());
                let name = (0..)
                    .map(|i| {
                        if i == 0 {
                            wanted.clone()
                        } else {
                            format!("{wanted}_{i}")
                        }
                    })
                    .find(|n| !set.contains(n))?;
                if set.len() >= buf.len() / NAME_LEN {
                    return None;
                }
                set.insert(name.clone());
                write_names(buf, &set);
                Some(name)
            })
            .ok_or("the Spout sender list is locked")?
            .ok_or("too many Spout senders")?;
        let block = Map::create(&name, INFO_LEN).ok_or("can't create the Spout sender")?;
        let program = std::env::current_exe().unwrap_or_default();
        block
            .update(INFO_LEN, |b| {
                b.fill(0);
                for (i, v) in [handle, width, height, DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32]
                    .into_iter()
                    .enumerate()
                {
                    b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                let p = program.to_string_lossy();
                let n = p.len().min(255);
                b[20..20 + n].copy_from_slice(&p.as_bytes()[..n]);
            })
            .ok_or("the Spout sender is locked")?;
        let active = Map::create(ACTIVE_MAP, NAME_LEN);
        if let Some(a) = &active {
            a.update(NAME_LEN, |b| {
                b.fill(0);
                let n = name.len().min(NAME_LEN - 1);
                b[..n].copy_from_slice(&name.as_bytes()[..n]);
            });
        }
        OURS.lock().unwrap().insert(name.clone());
        Ok(Self {
            name,
            names,
            _info: block,
            _active: active,
        })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let name = &self.name;
        self.names.update(MAX_SENDERS * NAME_LEN, |buf| {
            let mut set = parse_names(buf);
            set.remove(name);
            write_names(buf, &set);
        });
        OURS.lock().unwrap().remove(name);
    }
}

#[derive(Default)]
struct OutShared {
    spare: Mutex<Vec<Vec<u8>>>,
    sent: AtomicU64,
}

struct Output {
    name: String,
    size: (u32, u32),
    tx: Option<SyncSender<OutFrame>>,
    shared: Arc<OutShared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

pub fn output(name: &str, width: u32, height: u32) -> Result<Box<dyn FrameSink>, String> {
    if width == 0 || height == 0 {
        return Err("empty output size".into());
    }
    let (tx, rx) = sync_channel::<OutFrame>(1);
    let (ready_tx, ready_rx) = sync_channel(1);
    let shared: Arc<OutShared> = Default::default();
    let (wanted, s) = (name.to_owned(), shared.clone());
    let thread = std::thread::Builder::new()
        .name("spout-out".into())
        .spawn(move || send(&wanted, width, height, &rx, &s, &ready_tx))
        .map_err(|e| e.to_string())?;
    let name = ready_rx.recv().map_err(|_| "the Spout thread stopped".to_string())??;
    log::info!("Spout output \"{name}\" {width}×{height}");
    Ok(Box::new(Output {
        name,
        size: (width, height),
        tx: Some(tx),
        shared,
        thread: Some(thread),
    }))
}

/// The output thread: makes the shared texture, registers, writes frames.
fn send(
    wanted: &str,
    width: u32,
    height: u32,
    frames: &ChannelReceiver<OutFrame>,
    shared: &OutShared,
    ready: &SyncSender<Result<String, String>>,
) {
    let made = (|| {
        let gpu = Gpu::best()?;
        let texture = gpu.texture(width, height, DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32, false)?;
        let resource: IDXGIResource = texture.cast().map_err(|e| e.to_string())?;
        // SAFETY: the texture was made with D3D11_RESOURCE_MISC_SHARED.
        let handle = unsafe { resource.GetSharedHandle() }.map_err(|e| format!("share handle: {e}"))?;
        let registration = Registration::new(wanted, handle.0 as usize as u32, width, height)?;
        let access = mutex(&format!("{}_SpoutAccessMutex", registration.name)).ok_or("can't make the access lock")?;
        let counter = count_semaphore(&registration.name).ok_or("can't make the frame counter")?;
        Ok::<_, String>((gpu, texture, registration, access, counter))
    })();
    let (gpu, texture, registration, access, counter) = match made {
        Ok(m) => m,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(registration.name.clone()));
    let mut bgra = Vec::new();
    // Ends when the Output drops its sender; `registration` then unregisters.
    while let Ok(f) = frames.recv() {
        let fits = (f.width, f.height) == (width, height)
            && to_bgra(&f.data, width as usize, height as usize, f.stride as usize, &mut bgra);
        if fits {
            let written = locked(&access, 67, || {
                // SAFETY: `bgra` holds `height` rows of `width * 4` bytes.
                unsafe {
                    gpu.context
                        .UpdateSubresource(&texture, 0, None, bgra.as_ptr().cast(), width * 4, 0);
                    gpu.context.Flush();
                    // Count the frame: take one, give back two.
                    if WaitForSingleObject(counter.0, 0) == WAIT_OBJECT_0 {
                        let _ = ReleaseSemaphore(counter.0, 2, None);
                    }
                }
            });
            if written.is_some() {
                shared.sent.fetch_add(1, Ordering::Relaxed);
            }
        }
        let mut spare = shared.spare.lock().unwrap();
        if spare.len() < 2 {
            spare.push(f.data);
        }
    }
}

impl FrameSink for Output {
    fn buffer(&self, len: usize) -> Vec<u8> {
        let mut b = self.shared.spare.lock().unwrap().pop().unwrap_or_default();
        b.resize(len, 0);
        b
    }

    fn send(&self, f: OutFrame) -> bool {
        self.tx.as_ref().is_some_and(|tx| tx.try_send(f).is_ok())
    }

    fn status(&self) -> String {
        format!(
            "Sending \"{}\" {}×{}, {} frames",
            self.name,
            self.size.0,
            self.size.1,
            self.shared.sent.load(Ordering::Relaxed)
        )
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_list_round_trips() {
        let set = BTreeSet::from(["b".to_string(), "a sender".to_string()]);
        let mut buf = vec![7u8; 4 * NAME_LEN];
        write_names(&mut buf, &set);
        assert_eq!(&buf[..9], b"a sender\0");
        assert_eq!(parse_names(&buf), set);
        assert!(parse_names(&vec![0u8; NAME_LEN]).is_empty());
    }

    #[test]
    fn sender_formats_become_rgba() {
        assert_eq!(rgba(&[1, 2, 3, 4], 1, 1, 4, copy_format(0)).unwrap(), [3, 2, 1, 4]);
        assert_eq!(rgba(&[1, 2, 3, 4], 1, 1, 4, 28).unwrap(), [1, 2, 3, 4]);
        // Half floats 1.0, 0.5, 0.0, 2.0 (clamped).
        let half: Vec<u8> = [0x3C00u16, 0x3800, 0, 0x4000]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(rgba(&half, 1, 1, 8, 10).unwrap(), [255, 128, 0, 255]);
        let ten = (1023u32 | (512 << 10) | (3 << 30)).to_le_bytes();
        assert_eq!(rgba(&ten, 1, 1, 4, 24).unwrap(), [255, 128, 0, 255]);
        assert!(rgba(&[0; 4], 1, 1, 4, 2).is_none());
    }

    /// Our output, received back through Spout (skips without Direct3D 11).
    #[test]
    fn spout_loopback() {
        if let Err(e) = Gpu::best() {
            eprintln!("skipping: {e}");
            return;
        }
        let (w, h) = (64u32, 32u32);
        let wanted = format!("LAVMFX test {}", std::process::id());
        let out = output(&wanted, w, h).unwrap();
        let second = output(&wanted, w, h).unwrap();
        assert!(second.status().contains(&format!("{wanted}_1")), "{}", second.status());
        drop(second);
        assert!(registered().contains(&wanted));
        assert!(sources().iter().all(|s| s.name != wanted), "our own output is hidden");
        let rx = receiver(&wanted).unwrap();
        let (t0, mut seen) = (Instant::now(), 0);
        let got = loop {
            let mut data = out.buffer((w * h * 4) as usize);
            for p in data.as_chunks_mut::<4>().0 {
                *p = [200, 100, 50, 255];
            }
            out.send(OutFrame {
                data,
                width: w,
                height: h,
                stride: w * 4,
                rate: (60, 1),
            });
            std::thread::sleep(Duration::from_millis(16));
            if let Some(f) = rx.latest(&mut seen)
                && f.planes[0].data[0] != 0
            {
                break f;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "no frame: {:?} / {}",
                rx.status(),
                out.status()
            );
        };
        assert_eq!((got.width, got.height), (w, h));
        assert_eq!(&got.planes[0].data[..4], &[200, 100, 50, 255]);
        drop(rx);
        drop(out);
        assert!(!registered().contains(&wanted));
        assert!(receiver(&wanted).is_err());
    }
}
