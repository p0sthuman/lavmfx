//! Slice Outline layers, one slice at a time: the layer's chain (the Slice
//! Outline generator + its effects) runs once per slice, in a texture the
//! size of that slice's box, so effects act on each outline on its own
//! (Radar sweeps around each slice, Stripper strips each one) instead of on
//! the whole screen. The results are laid back at the slices' places.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::effects::{EffectDraw, FxSlice, target_texture};
use crate::{COMP_FORMAT, clear_pass};

/// Kind of the Slice Outline generator.
pub const SLICE_OUTLINE_KIND: u32 = 141;
/// Its param slot "Effects Outside Lines" (after Color ×4 and Thickness).
const FREE_FX_SLOT: usize = 5;

/// The layer's effects are clipped to the lines (the generator's switch off).
pub(crate) fn clipped(chain: &[EffectDraw]) -> bool {
    chain
        .iter()
        .find(|e| e.kind == SLICE_OUTLINE_KIND)
        .is_some_and(|g| g.params[FREE_FX_SLOT] < 0.5)
}

/// The generator alone, opaque white: the lines as a mask.
pub(crate) fn mask_chain(chain: &[EffectDraw], k: usize) -> Vec<EffectDraw> {
    chain
        .iter()
        .filter(|e| e.kind == SLICE_OUTLINE_KIND)
        .take(1)
        .map(|e| {
            let mut m = *e;
            m.params[..4].copy_from_slice(&[1.0; 4]);
            m.opacity = 1.0;
            m.id = e.id ^ (((k as i64) + 1) << 40) ^ (1 << 39);
            m
        })
        .collect()
}

/// One slice's outline entries (`info.w` 1 = quad, 2 = polygon edges; same `info.y`).
type Group = Vec<FxSlice>;

/// Outline entries grouped per slice, bottom to top.
pub(crate) fn groups(all: &[FxSlice]) -> Vec<Group> {
    let mut out: Vec<Group> = Vec::new();
    let mut last = None;
    for s in all.iter().filter(|s| s.info[3] > 0.5) {
        if last != Some(s.info[1]) || s.info[3] < 1.5 {
            out.push(Vec::new());
        }
        last = (s.info[3] > 1.5).then_some(s.info[1]);
        out.last_mut().unwrap().push(*s);
    }
    out
}

fn points(s: &FxSlice) -> [[f32; 2]; 4] {
    [
        [s.p01[0], s.p01[1]],
        [s.p01[2], s.p01[3]],
        [s.p23[0], s.p23[1]],
        [s.p23[2], s.p23[3]],
    ]
}

/// Bounding box (screen uv) of a slice's outline.
fn bounds(g: &Group) -> ([f32; 2], [f32; 2]) {
    let mut lo = [f32::MAX; 2];
    let mut hi = [f32::MIN; 2];
    for p in g.iter().flat_map(points) {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    (lo, hi)
}

/// Pixel box `(x, y, w, h)` of a slice in a `size` texture (2 px margin for
/// the anti-aliased edge), clipped to it; `None` when off-screen.
pub(crate) fn pixel_box(g: &Group, size: (u32, u32)) -> Option<(u32, u32, u32, u32)> {
    let (lo, hi) = bounds(g);
    let (w, h) = (size.0 as f32, size.1 as f32);
    let x0 = (lo[0] * w - 2.0).floor().clamp(0.0, w);
    let y0 = (lo[1] * h - 2.0).floor().clamp(0.0, h);
    let x1 = (hi[0] * w + 2.0).ceil().clamp(0.0, w);
    let y1 = (hi[1] * h + 2.0).ceil().clamp(0.0, h);
    (x1 - x0 >= 1.0 && y1 - y0 >= 1.0).then_some((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32))
}

/// Slice list for slice `k`'s own pass, in its box's uv: its outline, plus
/// the slices above it as hide-only shapes (`info.x` = 1: their inside hides
/// its stroke, as on the whole screen).
pub(crate) fn local_list(groups: &[Group], k: usize, bx: (u32, u32, u32, u32), size: (u32, u32)) -> Vec<FxSlice> {
    let (x0, y0, w, h) = (bx.0 as f32, bx.1 as f32, bx.2 as f32, bx.3 as f32);
    let m = |p: [f32; 2]| [(p[0] * size.0 as f32 - x0) / w, (p[1] * size.1 as f32 - y0) / h];
    let map = |s: &FxSlice, hide: bool| {
        let q = points(s).map(m);
        FxSlice {
            p01: [q[0][0], q[0][1], q[1][0], q[1][1]],
            p23: [q[2][0], q[2][1], q[3][0], q[3][1]],
            info: [if hide { 1.0 } else { 0.0 }, s.info[1], 0.0, s.info[3]],
        }
    };
    let own = groups[k].iter().map(|s| map(s, false));
    let above = groups[k + 1..].iter().flatten().map(|s| map(s, true));
    own.chain(above).collect()
}

/// Per-slice textures (target + scratch) and the pipeline laying them back.
pub(crate) struct OutlinePass {
    /// Per slice: target, scratch, mask.
    pool: RefCell<HashMap<usize, [wgpu::Texture; 3]>>,
    /// 1×1 opaque white: "no mask".
    white: wgpu::Texture,
    bgl: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

impl OutlinePass {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("outline blit"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var mask: texture_2d<f32>;
struct V { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return V(vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0), uv);
}
@fragment fn fs(v: V) -> @location(0) vec4<f32> {
    let d = vec2<f32>(textureDimensions(src));
    let m = vec2<f32>(textureDimensions(mask));
    let a = textureLoad(mask, vec2<i32>(clamp(floor(v.uv * m), vec2<f32>(0.0), m - 1.0)), 0).a;
    // Premultiplied: scaling all channels keeps it so.
    return textureLoad(src, vec2<i32>(clamp(floor(v.uv * d), vec2<f32>(0.0), d - 1.0)), 0) * a;
}
"#
                .into(),
            ),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("outline blit"),
            entries: &[0, 1].map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("outline blit"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("outline blit"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COMP_FORMAT,
                    // Layer pictures are premultiplied.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let white = target_texture(device, "outline no mask", 1, 1);
        Self {
            pool: Default::default(),
            white,
            bgl,
            pipeline,
        }
    }

    /// Target, scratch and mask texture of slice `k`, `w × h`.
    pub(crate) fn textures(&self, device: &wgpu::Device, k: usize, w: u32, h: u32) -> [wgpu::Texture; 3] {
        let mut pool = self.pool.borrow_mut();
        let fresh = pool
            .get(&k)
            .is_none_or(|[t, ..]| t.size().width != w || t.size().height != h);
        if fresh {
            pool.insert(
                k,
                ["outline slice", "outline scratch", "outline mask"].map(|l| target_texture(device, l, w, h)),
            );
        }
        pool[&k].clone()
    }

    /// Fill the "no mask" texture (once per frame is plenty).
    pub(crate) fn prepare(&self, enc: &mut wgpu::CommandEncoder) {
        drop(clear_pass(
            enc,
            &self.white.create_view(&Default::default()),
            wgpu::Color::WHITE,
        ));
    }

    /// Lay `src` (× `mask`'s alpha, if any) over `dst` at pixel box `bx`.
    pub(crate) fn blit(
        &self,
        device: &wgpu::Device,
        enc: &mut wgpu::CommandEncoder,
        src: &wgpu::Texture,
        mask: Option<&wgpu::Texture>,
        dst: &wgpu::TextureView,
        bx: (u32, u32, u32, u32),
    ) {
        let view = src.create_view(&Default::default());
        let mview = mask.unwrap_or(&self.white).create_view(&Default::default());
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("outline blit"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&mview),
                },
            ],
        });
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("outline blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_viewport(bx.0 as f32, bx.1 as f32, bx.2 as f32, bx.3 as f32, 0.0, 1.0);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// The chain for slice `k`'s pass: the same effects with their own ids, so
/// effects keeping state (feedback, history) keep it per slice.
pub(crate) fn chain_for(chain: &[EffectDraw], k: usize) -> Vec<EffectDraw> {
    chain
        .iter()
        .map(|e| EffectDraw {
            id: e.id ^ (((k as i64) + 1) << 40),
            ..*e
        })
        .collect()
}

pub(crate) fn clear(enc: &mut wgpu::CommandEncoder, t: &wgpu::Texture) {
    drop(clear_pass(
        enc,
        &t.create_view(&Default::default()),
        wgpu::Color::TRANSPARENT,
    ));
}
