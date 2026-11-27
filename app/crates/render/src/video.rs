//! Video frame upload and drawing.
//!
//! Frames arrive as CPU planes ([`lavmfx_media::VideoFrame`]). Each plane is
//! written into its own texture with `write_texture` (row stride kept as-is,
//! so no repacking); colour conversion happens in `video.wgsl`. BCn frames
//! (HAP) are uploaded as compressed textures.
//!
//! [`VideoPass`] holds the shared pipeline; each clip slot has a
//! [`VideoLayer`] with its own plane textures and uniform. A frame is drawn
//! into a cleared layer texture (premultiplied, letterbox transparent); the
//! compositor blends layer textures together (`blend.wgsl`).

use bytemuck::{Pod, Zeroable};
use lavmfx_media::{FrameFormat, VideoFrame, YuvMatrix};

use crate::COMP_FORMAT;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VideoUniform {
    coeffs: [f32; 4],
    range: [f32; 4],
    scale: [f32; 4],
    mode: u32,
    opacity: f32,
    _pad: [u32; 2],
}

/// One texture per plane, recreated when size or format changes.
struct Planes {
    key: (FrameFormat, u32, u32),
    textures: Vec<wgpu::Texture>,
    bind_group: wgpu::BindGroup,
}

pub(crate) struct VideoPass {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    dummy: wgpu::TextureView,
}

/// One layer's current frame on the GPU.
pub(crate) struct VideoLayer {
    uniform: wgpu::Buffer,
    planes: Option<Planes>,
    frame: Option<(FrameFormat, u32, u32, lavmfx_media::ColorInfo)>,
}

/// Texture format and pixel size of plane `i`.
fn plane_layout(format: FrameFormat, w: u32, h: u32) -> Vec<(wgpu::TextureFormat, u32, u32)> {
    use wgpu::TextureFormat as T;
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let pad = |v: u32| v.div_ceil(4) * 4;
    match format {
        FrameFormat::Rgba8 => vec![(T::Rgba8Unorm, w, h)],
        FrameFormat::Yuv420p => vec![(T::R8Unorm, w, h), (T::R8Unorm, cw, ch), (T::R8Unorm, cw, ch)],
        FrameFormat::Nv12 => vec![(T::R8Unorm, w, h), (T::Rg8Unorm, cw, ch)],
        FrameFormat::Bc1 => vec![(T::Bc1RgbaUnorm, pad(w), pad(h))],
        FrameFormat::Bc3 | FrameFormat::Bc3YCoCg => vec![(T::Bc3RgbaUnorm, pad(w), pad(h))],
        FrameFormat::Bc7 => vec![(T::Bc7RgbaUnorm, pad(w), pad(h))],
    }
}

impl VideoPass {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("video"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/video.wgsl").into()),
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
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
            label: Some("video"),
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
                tex_entry(1),
                tex_entry(2),
                tex_entry(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COMP_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("video"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let dummy = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("video dummy"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        Self {
            pipeline,
            bgl,
            sampler,
            dummy,
        }
    }
}

impl VideoLayer {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video layer"),
            size: std::mem::size_of::<VideoUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            uniform,
            planes: None,
            frame: None,
        }
    }

    pub(crate) fn has_frame(&self) -> bool {
        self.frame.is_some()
    }

    pub(crate) fn clear(&mut self) {
        self.frame = None;
    }

    pub(crate) fn upload(&mut self, vp: &VideoPass, device: &wgpu::Device, queue: &wgpu::Queue, f: &VideoFrame) {
        let key = (f.format, f.width, f.height);
        let layout = plane_layout(f.format, f.width, f.height);
        if f.planes.len() < layout.len() {
            return;
        }
        if self.planes.as_ref().is_none_or(|p| p.key != key) {
            let textures: Vec<wgpu::Texture> = layout
                .iter()
                .map(|&(format, w, h)| {
                    device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("video plane"),
                        size: wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    })
                })
                .collect();
            let views: Vec<_> = textures.iter().map(|t| t.create_view(&Default::default())).collect();
            let view = |i: usize| views.get(i).unwrap_or(&vp.dummy);
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("video"),
                layout: &vp.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(view(0)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(view(1)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(view(2)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Sampler(&vp.sampler),
                    },
                ],
            });
            self.planes = Some(Planes {
                key,
                textures,
                bind_group,
            });
        }

        let planes = self.planes.as_ref().unwrap();
        for ((tex, plane), &(format, w, h)) in planes.textures.iter().zip(&f.planes).zip(&layout) {
            // For BC formats rows are rows of 4×4 blocks.
            let rows = if f.format.is_bc() { h / 4 } else { h };
            let row_bytes = match format.block_copy_size(None) {
                Some(b) if f.format.is_bc() => (w / 4) * b,
                Some(b) => w * b,
                None => continue,
            };
            let needed = plane.stride as usize * (rows as usize - 1) + row_bytes as usize;
            if plane.stride < row_bytes || plane.data.len() < needed {
                log::warn!("video plane too small ({} < {needed}); frame dropped", plane.data.len());
                return;
            }
            queue.write_texture(
                tex.as_image_copy(),
                &plane.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(plane.stride),
                    rows_per_image: Some(rows),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }
        self.frame = Some((f.format, f.width, f.height, f.color));
    }

    /// Draw the current frame (fit into `comp_size`, premultiplied by
    /// `opacity`). Returns false if there is none.
    pub(crate) fn draw(
        &self,
        vp: &VideoPass,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'_>,
        comp_size: (u32, u32),
        opacity: f32,
        fit_mode: crate::Fit,
    ) -> bool {
        let (Some((format, w, h, color)), Some(planes)) = (self.frame, &self.planes) else {
            return false;
        };
        let fit = fit_mode.scale((w, h), comp_size);
        let bc_scale = if format.is_bc() {
            [
                w as f32 / (w.div_ceil(4) * 4) as f32,
                h as f32 / (h.div_ceil(4) * 4) as f32,
            ]
        } else {
            [1.0, 1.0]
        };
        let coeffs = match color.matrix {
            YuvMatrix::Bt601 => [1.402, 0.344136, 0.714136, 1.772],
            YuvMatrix::Bt709 => [1.5748, 0.187324, 0.468124, 1.8556],
            YuvMatrix::Bt2020 => [1.4746, 0.164553, 0.571353, 1.8814],
            YuvMatrix::YCgCo => [0.0; 4], // own path (mode 4)
        };
        let range = if color.full_range {
            [0.0, 1.0, 1.0]
        } else {
            [16.0 / 255.0, 255.0 / 219.0, 255.0 / 224.0]
        };
        let mode = match format {
            FrameFormat::Yuv420p if color.matrix == YuvMatrix::YCgCo => 4,
            FrameFormat::Yuv420p => 1,
            FrameFormat::Nv12 => 2,
            FrameFormat::Bc3YCoCg => 3,
            _ => 0,
        };
        let u = VideoUniform {
            coeffs,
            range: [range[0], range[1], range[2], 0.0],
            scale: [fit[0], fit[1], bc_scale[0], bc_scale[1]],
            mode,
            opacity: opacity.clamp(0.0, 1.0),
            _pad: [0; 2],
        };
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&u));
        pass.set_pipeline(&vp.pipeline);
        pass.set_bind_group(0, &planes.bind_group, &[]);
        pass.draw(0..3, 0..1);
        true
    }
}
