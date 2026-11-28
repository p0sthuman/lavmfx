//! Advanced output pass: draws a screen's slice meshes, sampling the
//! composition texture, into a screen-sized texture (one per output).
//!
//! Meshes come from `lavmfx_core::output::Slice::mesh` (the app converts them
//! to [`SliceVertex`]); this crate doesn't depend on the model.

use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::COMP_FORMAT;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct SliceVertex {
    /// Screen pixels.
    pub pos: [f32; 2],
    /// Composition texture 0..1.
    pub uv: [f32; 2],
    /// Slice space 0..1.
    pub local: [f32; 2],
    /// Index into the slice params.
    pub slice: u32,
}

/// Per-slice look (matches `SliceP` in `slices.wgsl`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct SliceParams {
    /// brightness, contrast, black bg (0/1), soft edge on (0/1)
    pub adjust: [f32; 4],
    /// r, g, b offsets; w = 1: alpha × the key texture (slice below "Is Key")
    pub rgb: [f32; 4],
    /// soft edge widths: left, right, top, bottom (fractions of the slice)
    pub edge: [f32; 4],
    /// luminance, power, gamma, 0
    pub curve: [f32; 4],
    /// x = alpha multiplier (1 = as is), yzw unused
    pub mult: [f32; 4],
    /// Input quad corners in composition uv: TL.xy TR.xy, BR.xy BL.xy (Sample).
    pub inq: [[f32; 4]; 2],
    /// Slice size on the screen (w, h px), number of `looks`, 0.
    pub size: [f32; 4],
    /// Slice Chase looks drawn in this slice (Output space), in order.
    pub looks: [SliceLook; MAX_LOOKS],
}

/// Slice Chase looks one slice can carry on the outputs.
pub const MAX_LOOKS: usize = 4;

/// A Slice Chase look drawn in a slice on the outputs (matches `ChaseLook`
/// in `slices.wgsl`; the pattern code is `chase.wgsl`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default, PartialEq)]
pub struct SliceLook {
    /// Look (1 Wipe, 2 Runner, 3 Tiles, 4 Sample), level, phase in the step, step.
    pub a: [f32; 4],
    /// Level with Unlit applied, dry/wet, Outside (0 Clear, 1 Black, 2 Keep), 1 = in the sequence.
    pub b: [f32; 4],
    /// The effect's first 12 shader params.
    pub p: [[f32; 4]; 3],
}

impl Default for SliceParams {
    fn default() -> Self {
        Self {
            adjust: [0.0; 4],
            rgb: [0.0; 4],
            edge: [0.0; 4],
            curve: [0.5, 2.0, 1.0, 0.0],
            mult: [1.0, 0.0, 0.0, 0.0],
            inq: [[0.0, 0.0, 1.0, 0.0], [1.0, 1.0, 0.0, 1.0]],
            size: [1.0, 1.0, 0.0, 0.0],
            looks: [SliceLook::default(); MAX_LOOKS],
        }
    }
}

/// Screen-wide look (matches `ScreenU`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
struct ScreenUniform {
    size: [f32; 4],
    adjust: [f32; 4],
    rgb: [f32; 4],
}

/// Screen-wide colour: opacity 0..1, the rest −1..1.
#[derive(Clone, Copy, Debug)]
pub struct ScreenLook {
    pub opacity: f32,
    pub brightness: f32,
    pub contrast: f32,
    pub rgb: [f32; 3],
}

impl Default for ScreenLook {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            brightness: 0.0,
            contrast: 0.0,
            rgb: [0.0; 3],
        }
    }
}

/// Stencil format for masks (bit 0 = inside the current mask).
const STENCIL: wgpu::TextureFormat = wgpu::TextureFormat::Stencil8;

/// One draw step of a screen (indices into the frame's index buffer).
#[derive(Clone, Debug, PartialEq)]
pub enum ScreenOp {
    /// A slice; `mask` = triangle-fan indices of its input mask outline
    /// (even-odd fill), `invert` = keep the outside instead.
    Slice {
        indices: Range<u32>,
        mask: Option<Range<u32>>,
        invert: bool,
        /// Index into the `inputs` textures (0 = the composition).
        tex: u32,
    },
    /// Render a slice into the key texture (luma × alpha); the next slice
    /// whose params set `keyed` uses it as alpha ("Is Key").
    Key { indices: Range<u32>, tex: u32 },
    /// Output mask: black inside the fan (outside when `invert`).
    Black { fan: Range<u32>, invert: bool },
}

/// Fan triangles (0, i, i+1) over `n` outline vertices starting at `base`.
pub fn fan_indices(base: u32, n: u32) -> Vec<u32> {
    (1..n.saturating_sub(1))
        .flat_map(|i| [base, base + i, base + i + 1])
        .collect()
}

fn target_tex(
    device: &wgpu::Device,
    label: &str,
    (w, h): (u32, u32),
    format: wgpu::TextureFormat,
    extra: wgpu::TextureUsages,
) -> wgpu::Texture {
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
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | extra,
        view_formats: &[],
    })
}

/// One screen's render target (+ mask stencil and key texture).
pub struct ScreenTarget {
    size: (u32, u32),
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    stencil: wgpu::TextureView,
    key: wgpu::TextureView,
}

impl ScreenTarget {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let size = (width.clamp(1, 16384), height.clamp(1, 16384));
        let u = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST;
        let texture = target_tex(device, "screen", size, COMP_FORMAT, u);
        let view = texture.create_view(&Default::default());
        let stencil = target_tex(device, "screen stencil", size, STENCIL, wgpu::TextureUsages::empty())
            .create_view(&Default::default());
        let key = target_tex(
            device,
            "screen key",
            size,
            COMP_FORMAT,
            wgpu::TextureUsages::TEXTURE_BINDING,
        )
        .create_view(&Default::default());
        Self {
            size,
            texture,
            view,
            stencil,
            key,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }
}

pub struct SlicePass {
    slice: wgpu::RenderPipeline,
    stencil: wgpu::RenderPipeline,
    black: wgpu::RenderPipeline,
    key: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Bound as the key texture while rendering into the real one.
    dummy_key: wgpu::TextureView,
}

fn stencil_state(compare: wgpu::CompareFunction, pass: wgpu::StencilOperation, write: u32) -> wgpu::DepthStencilState {
    let face = wgpu::StencilFaceState {
        compare,
        fail_op: wgpu::StencilOperation::Keep,
        depth_fail_op: wgpu::StencilOperation::Keep,
        pass_op: pass,
    };
    wgpu::DepthStencilState {
        format: STENCIL,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Always),
        stencil: wgpu::StencilState {
            front: face,
            back: face,
            read_mask: 1,
            write_mask: write,
        },
        bias: Default::default(),
    }
}

impl SlicePass {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("slices"),
            // + the Slice Chase looks (shared with effects.wgsl).
            source: wgpu::ShaderSource::Wgsl(
                concat!(include_str!("shaders/slices.wgsl"), include_str!("shaders/chase.wgsl")).into(),
            ),
        });
        let vf = wgpu::ShaderStages::VERTEX_FRAGMENT;
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
            label: Some("slices"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: vf,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                tex(4),
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("slices"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Uint32];
        let alpha_over = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let pipeline = |label: &str,
                        fs: &str,
                        blend: Option<wgpu::BlendState>,
                        writes: wgpu::ColorWrites,
                        ds: Option<wgpu::DepthStencilState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<SliceVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attrs,
                    })],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: ds,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COMP_FORMAT,
                        blend,
                        write_mask: writes,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        use wgpu::{ColorWrites as W, CompareFunction as C, StencilOperation as S};
        // Slices pass where stencil == reference (0 = no mask / outside, 1 = inside).
        let slice = pipeline(
            "slices",
            "fs_main",
            Some(alpha_over),
            W::ALL,
            Some(stencil_state(C::Equal, S::Keep, 0)),
        );
        let stencil = pipeline(
            "slice mask",
            "fs_black",
            None,
            W::empty(),
            Some(stencil_state(C::Always, S::Invert, 1)),
        );
        let black = pipeline(
            "output mask",
            "fs_black",
            None,
            W::ALL,
            Some(stencil_state(C::Equal, S::Keep, 0)),
        );
        let key = pipeline("slice key", "fs_key", None, W::ALL, None);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("slices"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let dummy_key = target_tex(
            device,
            "no key",
            (1, 1),
            COMP_FORMAT,
            wgpu::TextureUsages::TEXTURE_BINDING,
        )
        .create_view(&Default::default());
        Self {
            slice,
            stencil,
            black,
            key,
            bgl,
            sampler,
            dummy_key,
        }
    }

    /// Clear `target` to opaque black and run `ops` over the meshes.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        inputs: &[&wgpu::TextureView],
        target: &ScreenTarget,
        look: ScreenLook,
        vertices: &[SliceVertex],
        indices: &[u32],
        slices: &[SliceParams],
        ops: &[ScreenOp],
    ) {
        let (w, h) = target.size;
        // A screen-covering quad for inverted output masks.
        let mut verts = vertices.to_vec();
        let mut idx = indices.to_vec();
        let base = verts.len() as u32;
        for p in [[0.0, 0.0], [w as f32, 0.0], [w as f32, h as f32], [0.0, h as f32]] {
            verts.push(SliceVertex {
                pos: p,
                ..Default::default()
            });
        }
        let cover = idx.len() as u32..idx.len() as u32 + 6;
        idx.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        let params = if slices.is_empty() {
            vec![SliceParams::default()]
        } else {
            slices.to_vec()
        };

        let u = ScreenUniform {
            size: [w as f32, h as f32, look.opacity.clamp(0.0, 1.0), 0.0],
            adjust: [look.brightness, look.contrast, 0.0, 0.0],
            rgb: [look.rgb[0], look.rgb[1], look.rgb[2], 0.0],
        };
        let init = |label, contents: &[u8], usage| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage,
            })
        };
        let ubuf = init("screen", bytemuck::bytes_of(&u), wgpu::BufferUsages::UNIFORM);
        let sbuf = init(
            "slice params",
            bytemuck::cast_slice(&params),
            wgpu::BufferUsages::STORAGE,
        );
        let vbuf = init("slice mesh", bytemuck::cast_slice(&verts), wgpu::BufferUsages::VERTEX);
        let ibuf = init("slice index", bytemuck::cast_slice(&idx), wgpu::BufferUsages::INDEX);
        let bind = |comp: &wgpu::TextureView, key: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("slices"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: ubuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: sbuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(comp),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(key),
                    },
                ],
            })
        };
        // Per input texture: (main pass, key pass) bind groups.
        let groups: Vec<_> = inputs
            .iter()
            .map(|t| (bind(t, &target.key), bind(t, &self.dummy_key)))
            .collect();
        let group = |i: &u32| &groups[(*i as usize).min(groups.len().saturating_sub(1))];
        if groups.is_empty() {
            return;
        }

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("slices") });
        let mut first = true;
        let mut i = 0;
        loop {
            // Main pass: every op up to the next key.
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("screen"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: if first {
                                wgpu::LoadOp::Clear(wgpu::Color::BLACK)
                            } else {
                                wgpu::LoadOp::Load
                            },
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &target.stencil,
                        depth_ops: None,
                        stencil_ops: Some(wgpu::Operations {
                            load: if first {
                                wgpu::LoadOp::Clear(0)
                            } else {
                                wgpu::LoadOp::Load
                            },
                            store: wgpu::StoreOp::Store,
                        }),
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                first = false;
                pass.set_bind_group(0, &groups[0].0, &[]);
                pass.set_vertex_buffer(0, vbuf.slice(..));
                pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
                while i < ops.len() && !matches!(ops[i], ScreenOp::Key { .. }) {
                    match &ops[i] {
                        ScreenOp::Slice {
                            indices,
                            mask,
                            invert,
                            tex,
                        } => {
                            pass.set_bind_group(0, &group(tex).0, &[]);
                            if let Some(m) = mask {
                                pass.set_pipeline(&self.stencil);
                                pass.draw_indexed(m.clone(), 0, 0..1);
                            }
                            pass.set_pipeline(&self.slice);
                            pass.set_stencil_reference(u32::from(mask.is_some() && !invert));
                            pass.draw_indexed(indices.clone(), 0, 0..1);
                            if let Some(m) = mask {
                                // Invert again: stencil back to 0.
                                pass.set_pipeline(&self.stencil);
                                pass.draw_indexed(m.clone(), 0, 0..1);
                            }
                        }
                        ScreenOp::Black { fan, invert } => {
                            pass.set_pipeline(&self.stencil);
                            pass.draw_indexed(fan.clone(), 0, 0..1);
                            pass.set_pipeline(&self.black);
                            pass.set_stencil_reference(u32::from(!invert));
                            pass.draw_indexed(cover.clone(), 0, 0..1);
                            pass.set_pipeline(&self.stencil);
                            pass.draw_indexed(fan.clone(), 0, 0..1);
                        }
                        ScreenOp::Key { .. } => {}
                    }
                    i += 1;
                }
            }
            let Some(ScreenOp::Key { indices, tex }) = ops.get(i) else {
                break;
            };
            {
                let mut pass = crate::clear_pass(&mut enc, &target.key, wgpu::Color::TRANSPARENT);
                pass.set_pipeline(&self.key);
                pass.set_bind_group(0, &group(tex).1, &[]);
                pass.set_vertex_buffer(0, vbuf.slice(..));
                pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(indices.clone(), 0, 0..1);
            }
            i += 1;
        }
        queue.submit([enc.finish()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_gpu::gpu;

    /// 64×32 source: left half red, right half green.
    fn source(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
        let t = ScreenTarget::new(device, 64, 32);
        let mut px = Vec::with_capacity(64 * 32 * 4);
        for _y in 0..32 {
            for x in 0..64 {
                px.extend_from_slice(if x < 32 { &[255, 0, 0, 255] } else { &[0, 255, 0, 255] });
            }
        }
        queue.write_texture(
            t.texture().as_image_copy(),
            &px,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(64 * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: 64,
                height: 32,
                depth_or_array_layers: 1,
            },
        );
        t.view().clone()
    }

    fn read(device: &wgpu::Device, queue: &wgpu::Queue, t: &ScreenTarget) -> Vec<u8> {
        let (w, h) = t.size();
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (w * h * 4) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            t.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .unwrap();
        buf.slice(..).get_mapped_range().unwrap().to_vec()
    }

    /// Quad mesh (2 triangles) from screen corners + uv corners.
    fn quad(pos: [[f32; 2]; 4], uv: [[f32; 2]; 4]) -> (Vec<SliceVertex>, Vec<u32>) {
        let local = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let v = (0..4)
            .map(|i| SliceVertex {
                pos: pos[i],
                uv: uv[i],
                local: local[i],
                slice: 0,
            })
            .collect();
        (v, vec![0, 1, 2, 0, 2, 3])
    }

    #[test]
    fn slice_maps_input_to_output() {
        let Some((device, queue)) = gpu() else { return };
        let src = source(&device, &queue);
        let pass = SlicePass::new(&device);
        let target = ScreenTarget::new(&device, 64, 64);
        // Right (green) half of the source onto the left half of the screen.
        let (v, i) = quad(
            [[0.0, 0.0], [32.0, 0.0], [32.0, 64.0], [0.0, 64.0]],
            [[0.5, 0.0], [1.0, 0.0], [1.0, 1.0], [0.5, 1.0]],
        );
        pass.render(
            &device,
            &queue,
            &[&src],
            &target,
            ScreenLook::default(),
            &v,
            &i,
            &[SliceParams::default()],
            &[ScreenOp::Slice {
                indices: 0..6,
                mask: None,
                invert: false,
                tex: 0,
            }],
        );
        let px = read(&device, &queue, &target);
        let at = |x: usize, y: usize| &px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
        assert_eq!(at(10, 30), &[0, 255, 0, 255], "slice shows the green half");
        assert_eq!(at(50, 30), &[0, 0, 0, 255], "outside the slice is black");

        // Soft edge on the left 50 % of the slice: dark at its left border.
        let p = SliceParams {
            adjust: [0.0, 0.0, 0.0, 1.0],
            edge: [0.5, 0.0, 0.0, 0.0],
            ..Default::default()
        };
        pass.render(
            &device,
            &queue,
            &[&src],
            &target,
            ScreenLook::default(),
            &v,
            &i,
            &[p],
            &[ScreenOp::Slice {
                indices: 0..6,
                mask: None,
                invert: false,
                tex: 0,
            }],
        );
        let px = read(&device, &queue, &target);
        let g = |x: usize| px[(30 * 64 + x) * 4 + 1];
        assert!(g(0) < 10, "edge starts dark: {}", g(0));
        assert!((100..160).contains(&g(8)), "half-way = luminance 0.5: {}", g(8));
        assert!(g(20) > 250);

        // Screen opacity dims everything.
        let look = ScreenLook {
            opacity: 0.5,
            ..Default::default()
        };
        pass.render(
            &device,
            &queue,
            &[&src],
            &target,
            look,
            &v,
            &i,
            &[SliceParams::default()],
            &[ScreenOp::Slice {
                indices: 0..6,
                mask: None,
                invert: false,
                tex: 0,
            }],
        );
        let px = read(&device, &queue, &target);
        assert!((120..135).contains(&px[(30 * 64 + 10) * 4 + 1]));
    }

    /// Slice Chase looks on the outputs: a half-way wipe, a white outline,
    /// the sampled colour, and Outside for a slice not in the sequence.
    #[test]
    fn chase_looks_on_the_outputs() {
        let Some((device, queue)) = gpu() else { return };
        let src = source(&device, &queue); // red | green
        let pass = SlicePass::new(&device);
        let target = ScreenTarget::new(&device, 64, 64);
        let (v, i) = quad(
            [[0.0, 0.0], [64.0, 0.0], [64.0, 64.0], [0.0, 64.0]],
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        );
        let draw = |look: SliceLook| {
            let mut p = SliceParams {
                size: [64.0, 64.0, 1.0, 0.0],
                ..Default::default()
            };
            p.looks[0] = look;
            pass.render(
                &device,
                &queue,
                &[&src],
                &target,
                ScreenLook::default(),
                &v,
                &i,
                &[p],
                &[ScreenOp::Slice {
                    indices: 0..6,
                    mask: None,
                    invert: false,
                    tex: 0,
                }],
            );
            read(&device, &queue, &target)
        };
        let at = |px: &[u8], x: usize, y: usize| -> [u8; 4] { px[(y * 64 + x) * 4..][..4].try_into().unwrap() };
        // Wipe top → bottom, Reveal, hard edge, half-way: top shows, bottom black.
        let px = draw(SliceLook {
            a: [1.0, 1.0, 0.5, 0.0],
            b: [1.0, 1.0, 0.0, 1.0],
            p: [[2.0, 0.0, 0.001, 1.0], [0.0; 4], [0.0; 4]],
        });
        assert_eq!(at(&px, 10, 10), [255, 0, 0, 255], "revealed");
        assert_eq!(at(&px, 10, 54)[..3], [0, 0, 0], "not yet");
        // Runner, white Outline: the border white, the middle black.
        let px = draw(SliceLook {
            a: [2.0, 1.0, 0.5, 0.0],
            b: [1.0, 1.0, 0.0, 1.0],
            p: [[1.0; 4], [0.1, 0.0, 1.0, 0.25], [1.0, 1.0, 0.0, 0.0]],
        });
        assert_eq!(at(&px, 1, 32), [255, 255, 255, 255], "outline");
        assert_eq!(at(&px, 32, 32)[..3], [0, 0, 0], "inside");
        // Sample (average): half red, half green → the mix everywhere.
        let px = draw(SliceLook {
            a: [4.0, 1.0, 0.5, 0.0],
            b: [1.0, 1.0, 0.0, 1.0],
            p: [[1.0, 0.0, 1.0, 0.0], [0.0; 4], [0.0; 4]],
        });
        let c = at(&px, 10, 10);
        assert!(c[0].abs_diff(128) < 12 && c[1].abs_diff(128) < 12, "average {c:?}");
        // Not in the sequence: Clear hides it, Keep leaves it.
        let mut out = SliceLook {
            a: [1.0, 0.0, 0.0, 0.0],
            b: [0.0, 1.0, 0.0, 0.0],
            p: [[0.0; 4]; 3],
        };
        assert_eq!(draw(out)[(10 * 64 + 10) * 4..][..3], [0, 0, 0]);
        out.b[2] = 2.0;
        assert_eq!(draw(out)[(10 * 64 + 10) * 4..][..3], [255, 0, 0]);
    }

    #[test]
    fn masks_and_key() {
        let Some((device, queue)) = gpu() else { return };
        let src = source(&device, &queue); // red | green
        let pass = SlicePass::new(&device);
        let target = ScreenTarget::new(&device, 64, 64);
        // Slice over the whole screen; mask fan = the left half (verts 4..8).
        let (mut v, mut i) = quad(
            [[0.0, 0.0], [64.0, 0.0], [64.0, 64.0], [0.0, 64.0]],
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        );
        for p in [[0.0, 0.0], [32.0, 0.0], [32.0, 64.0], [0.0, 64.0]] {
            v.push(SliceVertex {
                pos: p,
                ..Default::default()
            });
        }
        let fan = i.len() as u32..i.len() as u32 + 6;
        i.extend(fan_indices(4, 4));
        let px_at = |ops: &[ScreenOp], params: &[SliceParams]| {
            pass.render(
                &device,
                &queue,
                &[&src],
                &target,
                ScreenLook::default(),
                &v,
                &i,
                params,
                ops,
            );
            let px = read(&device, &queue, &target);
            move |x: usize| px[(30 * 64 + x) * 4..(30 * 64 + x) * 4 + 3].to_vec()
        };
        let p = [SliceParams::default()];
        let at = px_at(
            &[ScreenOp::Slice {
                indices: 0..6,
                mask: Some(fan.clone()),
                invert: false,
                tex: 0,
            }],
            &p,
        );
        assert_eq!(at(10), vec![255, 0, 0], "inside the mask");
        assert_eq!(at(50), vec![0, 0, 0], "outside the mask");
        let at = px_at(
            &[ScreenOp::Slice {
                indices: 0..6,
                mask: Some(fan.clone()),
                invert: true,
                tex: 0,
            }],
            &p,
        );
        assert_eq!((at(10), at(50)), (vec![0, 0, 0], vec![0, 255, 0]), "inverted");
        // Output mask: black over the left half; inverted: over the right half.
        let slice = ScreenOp::Slice {
            indices: 0..6,
            mask: None,
            invert: false,
            tex: 0,
        };
        let at = px_at(
            &[
                slice.clone(),
                ScreenOp::Black {
                    fan: fan.clone(),
                    invert: false,
                },
            ],
            &p,
        );
        assert_eq!((at(10), at(50)), (vec![0, 0, 0], vec![0, 255, 0]));
        let at = px_at(&[slice, ScreenOp::Black { fan, invert: true }], &p);
        assert_eq!((at(10), at(50)), (vec![255, 0, 0], vec![0, 0, 0]));
        // Is Key: the key slice's luma becomes the next slice's alpha.
        let keyed = SliceParams {
            rgb: [0.0, 0.0, 0.0, 1.0],
            ..Default::default()
        };
        let at = px_at(
            &[
                ScreenOp::Key { indices: 0..6, tex: 0 },
                ScreenOp::Slice {
                    indices: 0..6,
                    mask: None,
                    invert: false,
                    tex: 0,
                },
            ],
            &[keyed],
        );
        let (l, r) = (at(10), at(50));
        assert!((45..65).contains(&l[0]), "red × its luma 0.21: {l:?}");
        assert!((170..195).contains(&r[1]), "green × its luma 0.72: {r:?}");
    }
}
