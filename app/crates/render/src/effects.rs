//! Effect pass: runs `effects.wgsl` over a texture.
//!
//! Each effect reads the target texture, writes a scratch texture and the
//! result is copied back, so chains run in place and the blend bindings stay
//! fixed. Feedback effects (Trails) also keep their output per instance.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};

use crate::isf::{ISF_KIND_BASE, ISF_MIX_KIND, IsfRunner};
use crate::{COMP_FORMAT, fullscreen_pipeline, uniform_buffer};

/// Floats per effect (= lavmfx-core `effects::SHADER_PARAMS`).
pub const EFFECT_PARAMS: usize = 32;

/// One effect to run (built from lavmfx-core `effects::ShaderFx`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectDraw {
    /// Shader kind (index into lavmfx-core `SHADER_EFFECTS`).
    pub kind: u32,
    pub params: [f32; EFFECT_PARAMS],
    /// Dry/wet 0..1.
    pub opacity: f32,
    /// Dry/wet blend (shader blend index; Alpha..Exclusion supported).
    pub blend: u32,
    /// Instance id; keys the feedback texture.
    pub id: i64,
    /// Reads its own previous output.
    pub feedback: bool,
    /// Render passes (>1: multi-pass effect; the shader gets `pass`, the last one mixes with the input).
    pub passes: u32,
    /// Frames of input history kept per effect instance (0 = none; delays read them).
    pub history: u32,
    /// Key of a CPU image the effect reads (`FxFrame::images`, e.g. rasterised text), 0 = none.
    pub image: u64,
}

/// Effect chains of one frame. Empty chains cost nothing.
/// An image an effect samples as `aux_tex`: one channel (text coverage, in
/// R) or RGBA (Mask effect files).
#[derive(Debug, Clone, PartialEq)]
pub struct FxImage {
    pub width: u32,
    pub height: u32,
    /// `width × height` bytes, or × 4 when `rgba`.
    pub data: Vec<u8>,
    pub rgba: bool,
}

/// A slice of the advanced output as effects see it (Slice Transform /
/// Delay / Outline): its input quad in composition uv (TL, TR, BR, BL).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable, Default, PartialEq)]
pub struct FxSlice {
    /// TL.xy, TR.xy
    pub p01: [f32; 4],
    /// BR.xy, BL.xy
    pub p23: [f32; 4],
    /// x = rank by name (0..n-1), y = original index, z = 1 for an input quad
    /// (0 = padding), w = 1 for an output quad (screen uv, Slice Outline only),
    /// w = 2 for one edge of an output polygon (see [`FxSlice::polygon_edges`])
    pub info: [f32; 4],
}

impl FxSlice {
    /// A polygon slice's outline for the Slice Outline generator (screen uv):
    /// one entry per edge, `p01` = the edge, `p23` = the bounding box, `y` =
    /// `index` (consecutive entries with the same index form one polygon).
    pub fn polygon_edges(points: &[[f32; 2]], index: usize) -> Vec<FxSlice> {
        if points.len() < 3 {
            return Vec::new();
        }
        let lo = points
            .iter()
            .fold([f32::MAX; 2], |m, p| [m[0].min(p[0]), m[1].min(p[1])]);
        let hi = points
            .iter()
            .fold([f32::MIN; 2], |m, p| [m[0].max(p[0]), m[1].max(p[1])]);
        (0..points.len())
            .map(|k| {
                let (a, b) = (points[k], points[(k + 1) % points.len()]);
                FxSlice {
                    p01: [a[0], a[1], b[0], b[1]],
                    p23: [lo[0], lo[1], hi[0], hi[1]],
                    info: [0.0, index as f32, 0.0, 2.0],
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct FxFrame {
    /// Clip chain + layer chain of each layer's current clip.
    pub layer: Vec<Vec<EffectDraw>>,
    /// Same for the outgoing clip during a transition (feedback effects skipped).
    pub outgoing: Vec<Vec<EffectDraw>>,
    /// Per layer group (index = `LayerDraw::group`).
    pub group: Vec<Vec<EffectDraw>>,
    /// Composition chain, before the master fader.
    pub composition: Vec<EffectDraw>,
    /// Images referenced by `EffectDraw::image` keys.
    pub images: HashMap<u64, Arc<FxImage>>,
    /// Advanced output slices (input quads), bottom first.
    pub slices: Vec<FxSlice>,
    /// External FFT bands, 0..1 (ISF `audioFFT` inputs).
    pub fft: [f32; 32],
    /// Slice Chase values by effect id: one per `slices` entry (level, step
    /// phase, step number, in the sequence), bound as `chase_buf`.
    pub chase: HashMap<i64, Vec<[f32; 4]>>,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FxUniform {
    p: [f32; EFFECT_PARAMS],
    resolution: [f32; 2],
    time: f32,
    beat: f32,
    opacity: f32,
    blend: u32,
    kind: u32,
    pass: u32,
    passes: u32,
    /// Time of this effect's previous frame (-1 = first frame).
    prev_time: f32,
    /// History ring: layer of the newest frame, and depth (0 = none).
    head: u32,
    depth: u32,
}

pub(crate) struct EffectPass {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Transparent 1×1 for effects without feedback.
    empty: wgpu::TextureView,
    feedback: RefCell<HashMap<i64, (wgpu::Texture, wgpu::TextureView)>>,
    /// Copy of a multi-pass effect's input (bound as `prev_tex`).
    orig: RefCell<Option<(wgpu::Texture, wgpu::TextureView)>>,
    /// Input history rings, by effect id.
    history: RefCell<HashMap<i64, History>>,
    empty_array: wgpu::TextureView,
    /// Time each effect was last drawn (prev_time).
    times: RefCell<HashMap<i64, f32>>,
    /// Uploaded `FxImage`s by key.
    images: RefCell<HashMap<u64, (wgpu::Texture, wgpu::TextureView)>>,
    /// This frame's slices (storage buffer, at least one entry).
    slices: RefCell<(wgpu::Buffer, Vec<FxSlice>)>,
    /// Slice Chase values per effect id (re-uploaded when they change), and
    /// a one-entry zero buffer for every other effect.
    chase: RefCell<HashMap<i64, ChaseBuf>>,
    no_chase: wgpu::Buffer,
    /// ISF shaders (kinds ≥ `ISF_KIND_BASE`).
    pub(crate) isf: IsfRunner,
    /// CPU-side effects (FFGL plugins; kinds ≥ `EXT_KIND_BASE`) and their
    /// output textures by effect id.
    pub(crate) ext: RefCell<Option<Arc<dyn ExternalFx>>>,
    ext_out: RefCell<HashMap<i64, (wgpu::Texture, wgpu::TextureView)>>,
}

/// First `EffectDraw::kind` of external (CPU-side) effects: FFGL plugins.
pub const EXT_KIND_BASE: u32 = 2000;

/// Effects run outside wgpu (FFGL plugins on their OpenGL thread). Frames go
/// through the CPU: RGBA rows top first, `width × height`.
pub trait ExternalFx: Send + Sync {
    /// Sources ignore their input (no readback needed).
    fn is_source(&self, kind: u32) -> bool;
    #[allow(clippy::too_many_arguments)]
    fn process(
        &self,
        kind: u32,
        id: i64,
        params: &[f32; EFFECT_PARAMS],
        input: Option<&[u8]>,
        width: u32,
        height: u32,
        time: f32,
        beat: f32,
    ) -> Option<Vec<u8>>;
}

/// A chase effect's uploaded values and what they were.
type ChaseBuf = (wgpu::Buffer, Vec<[f32; 4]>);

fn chase_buffer(device: &wgpu::Device, v: &[[f32; 4]]) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    let pad = [[0.0f32; 4]];
    let data = if v.is_empty() { &pad[..] } else { v };
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("fx chase"),
        contents: bytemuck::cast_slice(data),
        usage: wgpu::BufferUsages::STORAGE,
    })
}

fn slices_buffer(device: &wgpu::Device, s: &[FxSlice]) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    let pad = [FxSlice::default()];
    let data = if s.is_empty() { &pad[..] } else { s };
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("fx slices"),
        contents: bytemuck::cast_slice(data),
        usage: wgpu::BufferUsages::STORAGE,
    })
}

struct History {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    head: u32,
    last: f32,
}

fn history_texture(device: &wgpu::Device, w: u32, h: u32, depth: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("fx history"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: depth,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COMP_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn array_view(t: &wgpu::Texture) -> wgpu::TextureView {
    t.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    })
}

pub(crate) fn target_texture(device: &wgpu::Device, label: &str, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COMP_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// `tex` (RGBA8) as tight rows, waiting for the GPU (snapshots, tests).
pub fn read_texture(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture) -> Vec<u8> {
    let mut enc = device.create_command_encoder(&Default::default());
    let out = readback(device, queue, &mut enc, tex);
    queue.submit([enc.finish()]);
    out
}

/// `tex` as tight RGBA rows, now: what `enc` recorded so far is submitted
/// (a fresh encoder takes its place) and the copy waited for.
fn readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    enc: &mut wgpu::CommandEncoder,
    tex: &wgpu::Texture,
) -> Vec<u8> {
    let (w, h) = (tex.width(), tex.height());
    let row = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ext readback"),
        size: u64::from(row) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: None,
            },
        },
        tex.size(),
    );
    let done = std::mem::replace(enc, device.create_command_encoder(&Default::default()));
    queue.submit([done.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    let Ok(view) = buf.slice(..).get_mapped_range() else {
        return vec![0; (w * h * 4) as usize];
    };
    let out = {
        let mut v = Vec::new();
        for r in view.chunks(row as usize).take(h as usize) {
            v.extend_from_slice(&r[..(w * 4) as usize]);
        }
        v
    };
    drop(view);
    buf.unmap();
    out
}

fn copy(enc: &mut wgpu::CommandEncoder, from: &wgpu::Texture, to: &wgpu::Texture) {
    enc.copy_texture_to_texture(from.as_image_copy(), to.as_image_copy(), from.size());
}

impl EffectPass {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("effects"),
            // + the Slice Chase looks (shared with slices.wgsl).
            source: wgpu::ShaderSource::Wgsl(
                concat!(include_str!("shaders/effects.wgsl"), include_str!("shaders/chase.wgsl")).into(),
            ),
        });
        let tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effects"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                tex(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                tex(5),
                // What is mixed below the layer (UV Map's second input).
                tex(6),
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Slice Chase values of this effect (one per slice entry).
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effects"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = fullscreen_pipeline(device, "effects", &layout, &shader, "fs_main");
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("effects"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let empty = target_texture(device, "fx empty", 1, 1).create_view(&Default::default());
        Self {
            pipeline,
            bgl,
            sampler,
            empty,
            feedback: RefCell::new(HashMap::new()),
            orig: RefCell::new(None),
            history: RefCell::new(HashMap::new()),
            empty_array: array_view(&history_texture(device, 1, 1, 1)),
            times: RefCell::new(HashMap::new()),
            images: RefCell::new(HashMap::new()),
            slices: RefCell::new((slices_buffer(device, &[]), Vec::new())),
            chase: RefCell::new(HashMap::new()),
            no_chase: chase_buffer(device, &[]),
            isf: IsfRunner::new(device),
            ext: RefCell::new(None),
            ext_out: RefCell::new(HashMap::new()),
        }
    }

    /// The slice list effects see this frame (re-uploaded when it changed).
    pub(crate) fn set_slices(&self, device: &wgpu::Device, s: &[FxSlice]) {
        let mut cur = self.slices.borrow_mut();
        if cur.1 != s {
            *cur = (slices_buffer(device, s), s.to_vec());
        }
    }

    /// This frame's Slice Chase values (buffers kept while unchanged;
    /// effects no longer in the map are dropped).
    pub(crate) fn set_chase(&self, device: &wgpu::Device, values: &HashMap<i64, Vec<[f32; 4]>>) {
        let mut cur = self.chase.borrow_mut();
        cur.retain(|id, _| values.contains_key(id));
        for (id, v) in values {
            if cur.get(id).is_none_or(|(_, old)| old != v) {
                cur.insert(*id, (chase_buffer(device, v), v.clone()));
            }
        }
    }

    /// Run `chain` over `target` in place (`scratch` = same size).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        chain: &[EffectDraw],
        images: &HashMap<u64, Arc<FxImage>>,
        target: &wgpu::Texture,
        scratch: &wgpu::Texture,
        below: Option<&wgpu::Texture>,
        time: f32,
        beat: f32,
    ) {
        if chain.is_empty() {
            return;
        }
        let below_view = below.map(|t| t.create_view(&Default::default()));
        let slices = self.slices.borrow();
        let chase = self.chase.borrow();
        let size = target.size();
        let src_view = target.create_view(&Default::default());
        let dst_view = scratch.create_view(&Default::default());
        let mut fb = self.feedback.borrow_mut();
        let mut orig = self.orig.borrow_mut();
        let mut hist = self.history.borrow_mut();
        let mut times = self.times.borrow_mut();
        let mut uploaded = self.images.borrow_mut();
        for e in chain {
            let prev_time = times.insert(e.id, time).unwrap_or(-1.0);
            // History ring: push this frame's input once per frame time.
            let (head, depth) = if e.history > 0 {
                let fresh = hist.get(&e.id).is_none_or(|h| {
                    h.tex.size()
                        != wgpu::Extent3d {
                            depth_or_array_layers: e.history,
                            ..size
                        }
                });
                if fresh {
                    let tex = history_texture(device, size.width, size.height, e.history);
                    let view = array_view(&tex);
                    hist.insert(
                        e.id,
                        History {
                            tex,
                            view,
                            head: 0,
                            last: f32::NAN,
                        },
                    );
                }
                let h = hist.get_mut(&e.id).unwrap();
                if h.last != time {
                    h.last = time;
                    h.head = (h.head + 1) % e.history;
                    enc.copy_texture_to_texture(
                        target.as_image_copy(),
                        wgpu::TexelCopyTextureInfo {
                            texture: &h.tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d { x: 0, y: 0, z: h.head },
                            aspect: wgpu::TextureAspect::All,
                        },
                        size,
                    );
                }
                (h.head, e.history)
            } else {
                (0, 0)
            };
            if e.feedback {
                let stale = fb.get(&e.id).is_some_and(|(t, _)| t.size() != size);
                if stale || !fb.contains_key(&e.id) {
                    let t = target_texture(device, "fx feedback", size.width, size.height);
                    let v = t.create_view(&Default::default());
                    fb.insert(e.id, (t, v));
                }
            }
            if e.image != 0
                && !uploaded.contains_key(&e.image)
                && let Some(img) = images.get(&e.image)
            {
                let ext = wgpu::Extent3d {
                    width: img.width.max(1),
                    height: img.height.max(1),
                    depth_or_array_layers: 1,
                };
                let tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("fx image"),
                    size: ext,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: if img.rgba {
                        wgpu::TextureFormat::Rgba8Unorm
                    } else {
                        wgpu::TextureFormat::R8Unorm
                    },
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let bpp = if img.rgba { 4 } else { 1 };
                if img.data.len() as u64 >= u64::from(img.width) * u64::from(img.height) * bpp && img.width > 0 {
                    queue.write_texture(
                        tex.as_image_copy(),
                        &img.data,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(img.width * bpp as u32),
                            rows_per_image: None,
                        },
                        ext,
                    );
                }
                let view = tex.create_view(&Default::default());
                uploaded.insert(e.image, (tex, view));
            }
            let mut kind = e.kind;
            // External (FFGL): through the CPU into this effect's own texture, then
            // mixed like ISF output. The frame is read back synchronously (v1).
            let mut ext_view = None;
            if kind >= EXT_KIND_BASE {
                let Some(ext) = self.ext.borrow().clone() else { continue };
                let t0 = std::time::Instant::now();
                let input = (!ext.is_source(kind)).then(|| readback(device, queue, enc, target));
                let t1 = std::time::Instant::now();
                let out = ext.process(
                    kind,
                    e.id,
                    &e.params,
                    input.as_deref(),
                    size.width,
                    size.height,
                    time,
                    beat,
                );
                log::debug!("ext fx: readback {:?} process {:?}", t1 - t0, t1.elapsed());
                let Some(out) = out else {
                    continue;
                };
                if out.len() < (size.width * size.height * 4) as usize {
                    continue;
                }
                let mut outs = self.ext_out.borrow_mut();
                if outs.get(&e.id).is_none_or(|(t, _)| t.size() != size) {
                    let t = target_texture(device, "ext out", size.width, size.height);
                    let v = t.create_view(&Default::default());
                    outs.insert(e.id, (t, v));
                }
                let (t, v) = &outs[&e.id];
                queue.write_texture(
                    t.as_image_copy(),
                    &out,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(size.width * 4),
                        rows_per_image: None,
                    },
                    size,
                );
                ext_view = Some(v.clone());
                kind = ISF_MIX_KIND;
            }
            // ISF: run its program into `isf.out`, then mix that like an effect.
            if (ISF_KIND_BASE..EXT_KIND_BASE).contains(&kind) {
                if !self.isf.run(
                    device,
                    queue,
                    enc,
                    kind,
                    e.id,
                    &e.params,
                    &src_view,
                    size,
                    &self.empty,
                    time,
                    beat,
                ) {
                    continue;
                }
                kind = ISF_MIX_KIND;
            }
            let isf_out = self.isf.out.borrow();
            let passes = e.passes.max(1);
            if passes > 1 {
                if orig.as_ref().is_none_or(|(t, _)| t.size() != size) {
                    let t = target_texture(device, "fx orig", size.width, size.height);
                    let v = t.create_view(&Default::default());
                    *orig = Some((t, v));
                }
                copy(enc, target, &orig.as_ref().unwrap().0);
            }
            for pass in 0..passes {
                let u = FxUniform {
                    p: e.params,
                    resolution: [size.width as f32, size.height as f32],
                    time,
                    beat,
                    opacity: e.opacity,
                    blend: e.blend,
                    kind,
                    pass,
                    passes,
                    prev_time,
                    head,
                    depth,
                };
                let buf = uniform_buffer(device, "fx", std::mem::size_of::<FxUniform>());
                queue.write_buffer(&buf, 0, bytemuck::bytes_of(&u));
                let prev = if let Some(v) = &ext_view {
                    v
                } else if kind == ISF_MIX_KIND {
                    &isf_out.as_ref().unwrap().1
                } else if e.feedback {
                    &fb[&e.id].1
                } else if passes > 1 {
                    &orig.as_ref().unwrap().1
                } else {
                    &self.empty
                };
                let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fx"),
                    layout: &self.bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: buf.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&src_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(prev),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: wgpu::BindingResource::TextureView(
                                uploaded.get(&e.image).map_or(&self.empty, |(_, v)| v),
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 6,
                            resource: wgpu::BindingResource::TextureView(below_view.as_ref().unwrap_or(&self.empty)),
                        },
                        wgpu::BindGroupEntry {
                            binding: 7,
                            resource: slices.0.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 8,
                            resource: chase.get(&e.id).map_or(&self.no_chase, |(b, _)| b).as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(
                                hist.get(&e.id)
                                    .filter(|_| depth > 0)
                                    .map_or(&self.empty_array, |h| &h.view),
                            ),
                        },
                    ],
                });
                {
                    let mut pass = crate::clear_pass(enc, &dst_view, wgpu::Color::TRANSPARENT);
                    pass.set_pipeline(&self.pipeline);
                    pass.set_bind_group(0, &bg, &[]);
                    pass.draw(0..3, 0..1);
                }
                copy(enc, scratch, target);
            }
            if e.feedback {
                copy(enc, scratch, &fb[&e.id].0);
            }
        }
    }

    /// Drop uploaded images no longer in the frame.
    pub(crate) fn retain_images(&self, keep: &dyn Fn(u64) -> bool) {
        self.images.borrow_mut().retain(|k, _| keep(*k));
    }

    pub(crate) fn retain(&self, alive: &dyn Fn(i64) -> bool) {
        self.feedback.borrow_mut().retain(|id, _| alive(*id));
        self.history.borrow_mut().retain(|id, _| alive(*id));
        self.times.borrow_mut().retain(|id, _| alive(*id));
        self.isf.retain(alive);
        self.ext_out.borrow_mut().retain(|id, _| alive(*id));
    }
}
