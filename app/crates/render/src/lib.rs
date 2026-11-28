//! GPU rendering.
//!
//! A [`Compositor`] renders into one offscreen texture. That texture is the
//! single source for every view (UI preview, output windows, later texture
//! sharing), so outputs never re-render.
//!
//! Render graph per frame (M3):
//! 1. the accumulator is cleared to black (with Show Test Card on, the test
//!    card replaces the result after the composition effects);
//! 2. per visible layer, bottom to top: its clip is drawn into a scratch
//!    layer texture (premultiplied; during a transition the outgoing clip
//!    into a second one), then `blend.wgsl` composites it over the
//!    accumulator into the other accumulator (ping-pong) with the layer's
//!    blend mode and opacity;
//! 3. present: accumulator × composition master → composition texture.
//!
//! Effects (M4, `effects.rs`): a layer's clip + layer chain runs on its
//! scratch texture after drawing (the outgoing clip's too), a group's chain
//! on the group buffer before it is blended, the composition chain on the
//! accumulator before present.

use bytemuck::{Pod, Zeroable};

mod effects;
pub mod isf;
mod outline;
pub mod slices;
#[cfg(test)]
mod test_gpu;
mod video;
pub use effects::{EFFECT_PARAMS, EXT_KIND_BASE, EffectDraw, ExternalFx, FxFrame, FxImage, FxSlice, read_texture};
use effects::{EffectPass, target_texture};
pub use isf::{ISF_KIND_BASE, IsfDesc, IsfLibrary};
use video::{VideoLayer, VideoPass};

/// Format of the composition texture.
pub const COMP_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Device features we want. BC compression is needed for HAP/DXV (M1);
/// without it those codecs fall back to CPU decompression.
pub const WANTED_FEATURES: wgpu::Features = wgpu::Features::TEXTURE_COMPRESSION_BC;

/// Number of blend modes `blend.wgsl` implements (= `lavmfx_core::mix::SHADER_BLENDS`).
pub const BLEND_MODE_COUNT: u32 = 64;

/// Per-frame inputs from the clock and composition.
#[derive(Debug, Clone, Copy)]
pub struct FrameInput {
    pub time: f32,
    pub beat: f32,
    /// Composition master fader, 0..1.
    pub master: f32,
}

/// A running clip transition on a layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transition {
    /// 0 = outgoing clip only, 1 = incoming clip only.
    pub progress: f32,
    /// Blend mode index used for the transition.
    pub blend: u32,
}

/// How a clip is sized into the composition (clip "Resize").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fit {
    /// Cover the composition, cropping the overflow.
    Fill,
    /// Whole clip visible, letterboxed.
    #[default]
    Fit,
    /// Both axes to the composition size.
    Stretch,
    /// 1 clip pixel = 1 composition pixel.
    Original,
}

impl Fit {
    /// Index in option order (Fill, Fit, Stretch, Original).
    pub fn from_index(i: i32) -> Self {
        match i {
            0 => Fit::Fill,
            2 => Fit::Stretch,
            3 => Fit::Original,
            _ => Fit::Fit,
        }
    }

    /// Fraction of the composition the clip covers per axis (> 1 = cropped).
    pub fn scale(self, (w, h): (u32, u32), (cw, ch): (u32, u32)) -> [f32; 2] {
        let (w, h, cw, ch) = (w.max(1) as f32, h.max(1) as f32, cw.max(1) as f32, ch.max(1) as f32);
        match self {
            Fit::Stretch => [1.0, 1.0],
            Fit::Original => [w / cw, h / ch],
            Fit::Fit | Fit::Fill => {
                let (sx, sy) = (cw / w, ch / h);
                let s = if self == Fit::Fit { sx.min(sy) } else { sx.max(sy) };
                [w * s / cw, h * s / ch]
            }
        }
    }
}

/// How to draw one layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerDraw {
    /// 0..1; 0 = skipped.
    pub opacity: f32,
    /// Blend mode index (see [`BLEND_MODE_COUNT`]).
    pub blend: u32,
    /// Mix from the outgoing clip (see [`Compositor::begin_transition`]).
    pub transition: Option<Transition>,
    /// Layer group (index into [`Compositor::set_groups`]): consecutive
    /// layers of a group are mixed together first, then blended as one.
    pub group: Option<u32>,
    /// Sizing of the current clip.
    pub fit: Fit,
    /// Sizing of the outgoing clip (during a transition).
    pub fit_prev: Fit,
    /// What the layer's chain starts from.
    pub input: LayerInput,
}

/// Start of a layer's effect chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayerInput {
    /// The clip's video frame.
    #[default]
    Frame,
    /// Transparent (generator / source clips: the chain draws the picture).
    Clear,
    /// Everything below this layer (effect clips): the group's buffer inside a group.
    Below,
    /// Another layer / group / the composition (Video Router, Feedback): its
    /// result this frame when already drawn (below), else last frame's.
    Route(Route),
}

/// What a Video Router (or a slice's Input Source) reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    /// The composition after its effects, last frame (Feedback).
    Composition,
    /// Layer `i` after its effects, before its opacity / blend.
    Layer(u32),
    /// Group `i` after its effects.
    Group(u32),
}

/// How a layer group's mixed layers are blended into the composition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroupDraw {
    pub opacity: f32,
    pub blend: u32,
}

impl Default for LayerDraw {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            blend: 0,
            transition: None,
            group: None,
            fit: Fit::Fit,
            input: LayerInput::Frame,
            fit_prev: Fit::Fit,
        }
    }
}

impl LayerDraw {
    pub fn alpha(opacity: f32) -> Self {
        Self {
            opacity,
            ..Default::default()
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameUniform {
    resolution: [f32; 2],
    time: f32,
    beat: f32,
    master: f32,
    _pad: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MixUniform {
    opacity: f32,
    mode: u32,
    trans_mode: u32,
    progress: f32,
    master: f32,
    /// 1 = blending into a (possibly transparent, premultiplied) group buffer.
    alpha_dst: f32,
    /// Composition time (time-based mixers: TimeSwitcher, Static…).
    time: f32,
    _pad: f32,
}

/// One composition layer: its clip and, during a transition, the outgoing clip.
struct LayerSlot {
    cur: VideoLayer,
    prev: VideoLayer,
    mix: wgpu::Buffer,
}

/// Size-dependent render targets.
struct Targets {
    /// The composition texture every view samples.
    output: wgpu::Texture,
    output_view: wgpu::TextureView,
    /// Ping-pong accumulators.
    acc: [wgpu::TextureView; 2],
    /// Ping-pong accumulators of the layer group being mixed (premultiplied).
    group: [wgpu::TextureView; 2],
    /// Scratch layer textures: [outgoing, current].
    layer: [wgpu::TextureView; 2],
    acc_tex: [wgpu::Texture; 2],
    group_tex: [wgpu::Texture; 2],
    layer_tex: [wgpu::Texture; 2],
    /// Effect pass output (copied back).
    scratch: wgpu::Texture,
}

pub struct Compositor {
    size: (u32, u32),
    targets: Targets,
    /// The test card (Output › Show Test Card) and its text.
    pattern: wgpu::RenderPipeline,
    card: wgpu::Buffer,
    test_card: bool,
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    blend_bgl: wgpu::BindGroupLayout,
    blend_layer: wgpu::RenderPipeline,
    blend_present: wgpu::RenderPipeline,
    present_mix: wgpu::Buffer,
    video: VideoPass,
    /// Indexed by composition layer (0 = bottom).
    layers: Vec<LayerSlot>,
    /// Layer groups of the next frame.
    groups: Vec<GroupDraw>,
    fx: EffectPass,
    /// Effect chains of the next frame.
    effects: FxFrame,
    /// Copies of routed results (Video Router / Feedback / slice inputs).
    routes: std::cell::RefCell<std::collections::HashMap<Route, wgpu::Texture>>,
    /// Routes wanted besides the layers' own (slice Input Sources).
    extra_routes: Vec<Route>,
    /// Slice Outline layers rendered one slice at a time.
    outline: outline::OutlinePass,
}

/// Glyphs of the test card's 5×7 font, in shader order.
const CARD_GLYPHS: &str = " 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ%:.-x/";

/// The test card's two text lines as glyph indices (`Card` in test_pattern.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CardText {
    lens: [u32; 4],
    chars: [u32; 64],
}

impl CardText {
    fn new(title: &str, sub: &str) -> Self {
        let mut t = Self {
            lens: [0; 4],
            chars: [0; 64],
        };
        for (line, text) in [title, sub].into_iter().enumerate() {
            let mut n = 0;
            for ch in text.chars().take(32) {
                let ch = if ch == 'x' || ch == '×' {
                    'x'
                } else {
                    ch.to_ascii_uppercase()
                };
                t.chars[line * 32 + n] = CARD_GLYPHS.find(ch).unwrap_or(0) as u32;
                n += 1;
            }
            t.lens[line] = n as u32;
        }
        t
    }
}

fn uniform_buffer(device: &wgpu::Device, label: &str, size: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    fs: &str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fs),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: COMP_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Render pass over `view` that clears it to `clear`.
pub(crate) fn clear_pass<'e>(
    enc: &'e mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    clear: wgpu::Color,
) -> wgpu::RenderPass<'e> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("composite"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(clear),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

impl Compositor {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let uniform_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };

        // Test pattern.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("test_pattern"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/test_pattern.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame"),
            entries: &[uniform_entry(0), uniform_entry(1)],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("test_pattern"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pattern = fullscreen_pipeline(device, "test_pattern", &layout, &shader, "fs_main");
        let uniform = uniform_buffer(device, "frame", std::mem::size_of::<FrameUniform>());
        let card = uniform_buffer(device, "test card text", std::mem::size_of::<CardText>());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: card.as_entire_binding(),
                },
            ],
        });

        // Blend / present.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blend"),
            source: wgpu::ShaderSource::Wgsl(blend_wgsl().into()),
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let blend_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blend"),
            entries: &[uniform_entry(0), tex_entry(1), tex_entry(2), tex_entry(3)],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blend"),
            bind_group_layouts: &[Some(&blend_bgl)],
            immediate_size: 0,
        });
        let blend_layer = fullscreen_pipeline(device, "blend layer", &layout, &shader, "fs_layer");
        let blend_present = fullscreen_pipeline(device, "present", &layout, &shader, "fs_present");
        let present_mix = uniform_buffer(device, "present", std::mem::size_of::<MixUniform>());

        Self {
            size: (width.max(1), height.max(1)),
            targets: Targets::new(device, width.max(1), height.max(1)),
            pattern,
            card,
            test_card: false,
            uniform,
            bind_group,
            blend_bgl,
            blend_layer,
            blend_present,
            present_mix,
            video: VideoPass::new(device),
            layers: Vec::new(),
            groups: Vec::new(),
            fx: EffectPass::new(device),
            effects: FxFrame::default(),
            routes: Default::default(),
            extra_routes: Vec::new(),
            outline: outline::OutlinePass::new(device),
        }
    }

    /// A Slice Outline layer: its chain once per slice outline, each in a
    /// texture the size of the slice's box (effects act per outline), laid
    /// back into `target`. False when there are no outlines (normal path).
    #[allow(clippy::too_many_arguments)]
    fn outline_per_slice(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        chain: &[EffectDraw],
        target: &wgpu::Texture,
        target_view: &wgpu::TextureView,
        time: f32,
        beat: f32,
    ) -> bool {
        let groups = outline::groups(&self.effects.slices);
        if groups.is_empty() {
            return false;
        }
        drop(clear_pass(enc, target_view, wgpu::Color::TRANSPARENT));
        self.outline.prepare(enc);
        let size = (target.size().width, target.size().height);
        let clip = outline::clipped(chain);
        for k in 0..groups.len() {
            let Some(bx) = outline::pixel_box(&groups[k], size) else {
                continue;
            };
            let [tex, scratch, mask] = self.outline.textures(device, k, bx.2, bx.3);
            self.fx.set_slices(device, &outline::local_list(&groups, k, bx, size));
            let images = &self.effects.images;
            if clip {
                // The lines alone (opaque): effects show only on them.
                outline::clear(enc, &mask);
                let m = outline::mask_chain(chain, k);
                self.fx
                    .apply(device, queue, enc, &m, images, &mask, &scratch, None, time, beat);
            }
            outline::clear(enc, &tex);
            let chain_k = outline::chain_for(chain, k);
            self.fx
                .apply(device, queue, enc, &chain_k, images, &tex, &scratch, None, time, beat);
            self.outline
                .blit(device, enc, &tex, clip.then_some(&mask), target_view, bx);
        }
        self.fx.set_slices(device, &self.effects.slices);
        true
    }

    /// Output › Show Test Card: draw the test card instead of the composition,
    /// with `title` and `sub` as its two text lines (up to 32 characters:
    /// A–Z, 0–9, space, `% : . - x /`; others show as spaces).
    pub fn set_test_card(&mut self, queue: &wgpu::Queue, on: bool, title: &str, sub: &str) {
        self.test_card = on;
        if on {
            queue.write_buffer(&self.card, 0, bytemuck::bytes_of(&CardText::new(title, sub)));
        }
    }

    /// Composition resolution in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Recreate the targets. Callers holding the old view (e.g. an egui
    /// texture registration) must re-register `view()`.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let (w, h) = (width.max(1), height.max(1));
        if (w, h) != self.size {
            self.targets = Targets::new(device, w, h);
            self.size = (w, h);
        }
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.targets.output
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.targets.output_view
    }

    fn slot(&mut self, device: &wgpu::Device, layer: usize) -> &mut LayerSlot {
        slot_mut(&mut self.layers, device, layer)
    }

    /// Upload a decoded frame for `layer` (0 = bottom); it is shown from the
    /// next [`render`](Self::render) on.
    /// Make sure `layer` has its GPU slot (layers without frames: source / effect clips).
    pub fn ensure_layer(&mut self, device: &wgpu::Device, layer: usize) {
        slot_mut(&mut self.layers, device, layer);
    }

    pub fn set_layer_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layer: usize,
        frame: &lavmfx_media::VideoFrame,
    ) {
        slot_mut(&mut self.layers, device, layer)
            .cur
            .upload(&self.video, device, queue, frame);
    }

    /// Upload a frame of the outgoing clip of `layer` (during a transition).
    pub fn set_outgoing_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layer: usize,
        frame: &lavmfx_media::VideoFrame,
    ) {
        slot_mut(&mut self.layers, device, layer)
            .prev
            .upload(&self.video, device, queue, frame);
    }

    /// The layer's current clip becomes the outgoing one (its last frame
    /// stays); the current slot is empty until the next frame arrives.
    pub fn begin_transition(&mut self, device: &wgpu::Device, layer: usize) {
        let slot = self.slot(device, layer);
        std::mem::swap(&mut slot.cur, &mut slot.prev);
        slot.cur.clear();
    }

    /// Drop the outgoing clip of `layer`.
    pub fn end_transition(&mut self, layer: usize) {
        if let Some(l) = self.layers.get_mut(layer) {
            l.prev.clear();
        }
    }

    /// Stop showing video on `layer` (current and outgoing).
    pub fn clear_layer(&mut self, layer: usize) {
        if let Some(l) = self.layers.get_mut(layer) {
            l.cur.clear();
            l.prev.clear();
        }
    }

    /// A layer was removed from the composition: later layers move down.
    pub fn remove_layer(&mut self, layer: usize) {
        if layer < self.layers.len() {
            self.layers.remove(layer);
        }
    }

    /// A layer moved from index `from` to `to` (its textures go with it).
    pub fn move_layer(&mut self, device: &wgpu::Device, from: usize, to: usize) {
        slot_mut(&mut self.layers, device, from.max(to));
        let l = self.layers.remove(from);
        self.layers.insert(to, l);
    }

    /// A layer was inserted at `layer`: it starts empty, later layers move up.
    pub fn insert_layer(&mut self, device: &wgpu::Device, layer: usize) {
        if layer < self.layers.len() {
            self.layers.insert(
                layer,
                LayerSlot {
                    cur: VideoLayer::new(device),
                    prev: VideoLayer::new(device),
                    mix: uniform_buffer(device, "layer mix", std::mem::size_of::<MixUniform>()),
                },
            );
        }
    }

    /// Stop showing video on every layer.
    pub fn clear_video(&mut self) {
        for l in &mut self.layers {
            l.cur.clear();
            l.prev.clear();
        }
    }

    pub fn has_video(&self) -> bool {
        self.layers.iter().any(|l| l.cur.has_frame() || l.prev.has_frame())
    }

    /// Layer groups for the next [`Self::render`] (index = `LayerDraw::group`).
    pub fn set_groups(&mut self, groups: Vec<GroupDraw>) {
        self.groups = groups;
    }

    fn blend_group(&self, device: &wgpu::Device, mix: &wgpu::Buffer, dst: &wgpu::TextureView) -> wgpu::BindGroup {
        self.blend_group_with(device, mix, dst, &self.targets.layer[1])
    }

    /// A kept route (`set_extra_routes`) read back as RGBA rows (premultiplied),
    /// waiting for the GPU: (width, height, pixels). Offline clip rendering.
    pub fn read_route(&self, device: &wgpu::Device, queue: &wgpu::Queue, r: Route) -> Option<(u32, u32, Vec<u8>)> {
        let routes = self.routes.borrow();
        let t = routes.get(&r)?;
        Some((t.width(), t.height(), effects::read_texture(device, queue, t)))
    }

    /// CPU-side effects (FFGL host) this compositor runs.
    pub fn set_external_fx(&mut self, ext: std::sync::Arc<dyn ExternalFx>) {
        *self.fx.ext.borrow_mut() = Some(ext);
    }

    /// ISF programs this compositor runs (shared by all compositors of a device).
    pub fn set_isf_library(&mut self, lib: std::sync::Arc<IsfLibrary>) {
        *self.fx.isf.library.borrow_mut() = Some(lib);
    }

    /// Effect chains for the next [`Self::render`].
    pub fn set_effects(&mut self, fx: FxFrame) {
        self.effects = fx;
    }

    /// Bind group blending `cur` (premultiplied) over `dst`.
    fn blend_group_with(
        &self,
        device: &wgpu::Device,
        mix: &wgpu::Buffer,
        dst: &wgpu::TextureView,
        cur: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        let t = &self.targets;
        let tex = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blend"),
            layout: &self.blend_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: mix.as_entire_binding(),
                },
                tex(1, dst),
                tex(2, &t.layer[0]),
                tex(3, cur),
            ],
        })
    }

    /// Routes to keep copies of besides the layers' (e.g. advanced output
    /// slices reading a layer).
    pub fn set_extra_routes(&mut self, r: Vec<Route>) {
        self.extra_routes = r;
    }

    /// The last copy of a routed result (after a `render` that wanted it).
    pub fn route_view(&self, r: Route) -> Option<wgpu::TextureView> {
        self.routes.borrow().get(&r).map(|t| t.create_view(&Default::default()))
    }

    /// Use `from`'s copy of route `r` as this compositor's (the Preview Monitor
    /// shows a Video Router / Feedback clip with the main composition's
    /// layers). Call before `render`; sizes must match, else it is skipped.
    pub fn feed_route(&self, device: &wgpu::Device, queue: &wgpu::Queue, r: Route, from: &Compositor) {
        let Some(src) = from.routes.borrow().get(&r).cloned() else {
            return;
        };
        if (src.size().width, src.size().height) != self.size {
            return;
        }
        let mut routes = self.routes.borrow_mut();
        let dst = routes
            .entry(r)
            .or_insert_with(|| effects::target_texture(device, "route", self.size.0, self.size.1));
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("feed route"),
        });
        enc.copy_texture_to_texture(src.as_image_copy(), dst.as_image_copy(), src.size());
        queue.submit([enc.finish()]);
    }

    /// Render one frame into the composition texture and submit it.
    /// `layers[i]` says how to draw layer `i` (missing = [`LayerDraw::default`]).
    pub fn render(&self, device: &wgpu::Device, queue: &wgpu::Queue, input: FrameInput, layers: &[LayerDraw]) {
        // Route copies: keep (or create) the wanted ones, drop the rest.
        let wanted: std::collections::HashSet<Route> = layers
            .iter()
            .filter_map(|d| {
                if let LayerInput::Route(r) = d.input {
                    Some(r)
                } else {
                    None
                }
            })
            .chain(self.extra_routes.iter().copied())
            .collect();
        {
            let mut routes = self.routes.borrow_mut();
            routes.retain(|r, t| wanted.contains(r) && t.size().width == self.size.0 && t.size().height == self.size.1);
            for r in &wanted {
                routes
                    .entry(*r)
                    .or_insert_with(|| effects::target_texture(device, "route", self.size.0, self.size.1));
            }
        }
        let routes = self.routes.borrow();
        let keep = |enc: &mut wgpu::CommandEncoder, r: Route, from: Option<&wgpu::Texture>| {
            if let Some(dst) = routes.get(&r) {
                match from {
                    Some(t) => enc.copy_texture_to_texture(t.as_image_copy(), dst.as_image_copy(), t.size()),
                    None => drop(clear_pass(
                        enc,
                        &dst.create_view(&Default::default()),
                        wgpu::Color::TRANSPARENT,
                    )),
                }
            }
        };
        let u = FrameUniform {
            resolution: [self.size.0 as f32, self.size.1 as f32],
            time: input.time,
            beat: input.beat,
            master: 1.0, // applied in present
            _pad: [0.0; 3],
        };
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&u));

        self.fx.set_slices(device, &self.effects.slices);
        self.fx.set_chase(device, &self.effects.chase);
        self.fx.isf.set_fft(device, queue, &self.effects.fft);
        let t = &self.targets;
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("composite"),
        });
        let transparent = wgpu::Color::TRANSPARENT;
        drop(clear_pass(&mut enc, &t.acc[0], wgpu::Color::BLACK));
        let mut src = 0;
        // Layer group being mixed: (group index, group buffer index, layers drawn).
        let mut open: Option<(u32, usize, usize)> = None;
        let n = self.layers.len().max(layers.len());
        for i in 0..=n {
            let d = layers.get(i).copied().unwrap_or_default();
            let g = if i < n { d.group } else { None };
            // Leaving a group's run: blend its buffer into the composition.
            if let Some((gi, gsrc, drawn)) = open
                && Some(gi) != g
            {
                open = None;
                let gd = self
                    .groups
                    .get(gi as usize)
                    .copied()
                    .unwrap_or(GroupDraw { opacity: 1.0, blend: 0 });
                if !(drawn > 0 && gd.opacity > 0.0) && wanted.contains(&Route::Group(gi)) {
                    keep(&mut enc, Route::Group(gi), None);
                }
                if drawn > 0 && gd.opacity > 0.0 {
                    if let Some(chain) = self.effects.group.get(gi as usize) {
                        let (time, beat) = (input.time, input.beat);
                        self.fx.apply(
                            device,
                            queue,
                            &mut enc,
                            chain,
                            &self.effects.images,
                            &t.group_tex[gsrc],
                            &t.scratch,
                            Some(&t.acc_tex[src]),
                            time,
                            beat,
                        );
                    }
                    if wanted.contains(&Route::Group(gi)) {
                        keep(&mut enc, Route::Group(gi), Some(&t.group_tex[gsrc]));
                    }
                    let m = MixUniform {
                        opacity: gd.opacity.clamp(0.0, 1.0),
                        mode: gd.blend.min(BLEND_MODE_COUNT - 1),
                        trans_mode: 0,
                        progress: -1.0,
                        master: 1.0,
                        alpha_dst: 0.0,
                        time: input.time,
                        _pad: 0.0,
                    };
                    let buf = uniform_buffer(device, "group mix", std::mem::size_of::<MixUniform>());
                    queue.write_buffer(&buf, 0, bytemuck::bytes_of(&m));
                    let bg = self.blend_group_with(device, &buf, &t.acc[src], &t.group[gsrc]);
                    let mut pass = clear_pass(&mut enc, &t.acc[1 - src], wgpu::Color::BLACK);
                    pass.set_pipeline(&self.blend_layer);
                    pass.set_bind_group(0, &bg, &[]);
                    pass.draw(0..3, 0..1);
                    src = 1 - src;
                }
            }
            if i == n {
                break;
            }
            // Entering a group's run: start from a transparent buffer.
            if let Some(gi) = g
                && open.is_none()
            {
                drop(clear_pass(&mut enc, &t.group[0], transparent));
                open = Some((gi, 0, 0));
            }
            let Some(slot) = self.layers.get(i) else { continue };
            let transition = d
                .transition
                .filter(|_| d.input == LayerInput::Frame && slot.prev.has_frame());
            let has_input = d.input != LayerInput::Frame || slot.cur.has_frame();
            // A hidden layer (opacity 0, bypassed) is still rendered when a router or a
            // slice reads it; it just isn't mixed in.
            let route_only = d.opacity <= 0.0 && has_input && wanted.contains(&Route::Layer(i as u32));
            if (d.opacity <= 0.0 && !route_only) || !(has_input || transition.is_some()) {
                if wanted.contains(&Route::Layer(i as u32)) {
                    keep(&mut enc, Route::Layer(i as u32), None);
                }
                continue;
            }
            if let LayerInput::Route(r) = d.input {
                match routes.get(&r) {
                    Some(from) => {
                        enc.copy_texture_to_texture(from.as_image_copy(), t.layer_tex[1].as_image_copy(), from.size())
                    }
                    None => drop(clear_pass(&mut enc, &t.layer[1], transparent)),
                }
            } else if d.input == LayerInput::Clear {
                drop(clear_pass(&mut enc, &t.layer[1], transparent));
            } else if d.input == LayerInput::Below {
                // What has been mixed so far (inside a group: the group's buffer).
                let below = match open {
                    Some((_, gsrc, _)) => &t.group_tex[gsrc],
                    None => &t.acc_tex[src],
                };
                enc.copy_texture_to_texture(below.as_image_copy(), t.layer_tex[1].as_image_copy(), below.size());
            } else {
                let mut pass = clear_pass(&mut enc, &t.layer[1], transparent);
                slot.cur.draw(&self.video, queue, &mut pass, self.size, 1.0, d.fit);
            }
            let per_slice = self.effects.layer.get(i).is_some_and(|c| {
                c.iter().any(|e| e.kind == outline::SLICE_OUTLINE_KIND)
                    && self.outline_per_slice(
                        device,
                        queue,
                        &mut enc,
                        c,
                        &t.layer_tex[1],
                        &t.layer[1],
                        input.time,
                        input.beat,
                    )
            });
            if let Some(chain) = self.effects.layer.get(i).filter(|_| !per_slice) {
                self.fx.apply(
                    device,
                    queue,
                    &mut enc,
                    chain,
                    &self.effects.images,
                    &t.layer_tex[1],
                    &t.scratch,
                    Some(match open {
                        Some((_, gsrc, _)) => &t.group_tex[gsrc],
                        None => &t.acc_tex[src],
                    }),
                    input.time,
                    input.beat,
                );
            }
            if wanted.contains(&Route::Layer(i as u32)) {
                keep(&mut enc, Route::Layer(i as u32), Some(&t.layer_tex[1]));
            }
            if route_only {
                continue;
            }
            if transition.is_some() {
                {
                    let mut pass = clear_pass(&mut enc, &t.layer[0], transparent);
                    slot.prev
                        .draw(&self.video, queue, &mut pass, self.size, 1.0, d.fit_prev);
                }
                if let Some(chain) = self.effects.outgoing.get(i) {
                    self.fx.apply(
                        device,
                        queue,
                        &mut enc,
                        chain,
                        &self.effects.images,
                        &t.layer_tex[0],
                        &t.scratch,
                        Some(match open {
                            Some((_, gsrc, _)) => &t.group_tex[gsrc],
                            None => &t.acc_tex[src],
                        }),
                        input.time,
                        input.beat,
                    );
                }
            }
            let m = MixUniform {
                opacity: d.opacity.clamp(0.0, 1.0),
                // An effect clip's result replaces what is below, whatever the layer's
                // blend mode; its opacity fades the effect in.
                mode: if d.input == LayerInput::Below {
                    0
                } else {
                    d.blend.min(BLEND_MODE_COUNT - 1)
                },
                trans_mode: transition.map_or(0, |t| t.blend.min(BLEND_MODE_COUNT - 1)),
                progress: transition.map_or(-1.0, |t| t.progress.clamp(0.0, 1.0)),
                master: 1.0,
                alpha_dst: if open.is_some() { 1.0 } else { 0.0 },
                time: input.time,
                _pad: 0.0,
            };
            queue.write_buffer(&slot.mix, 0, bytemuck::bytes_of(&m));
            if let Some((gi, gsrc, drawn)) = open {
                let bg = self.blend_group(device, &slot.mix, &t.group[gsrc]);
                let mut pass = clear_pass(&mut enc, &t.group[1 - gsrc], transparent);
                pass.set_pipeline(&self.blend_layer);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..3, 0..1);
                open = Some((gi, 1 - gsrc, drawn + 1));
            } else {
                let group = self.blend_group(device, &slot.mix, &t.acc[src]);
                let mut pass = clear_pass(&mut enc, &t.acc[1 - src], wgpu::Color::BLACK);
                pass.set_pipeline(&self.blend_layer);
                pass.set_bind_group(0, &group, &[]);
                pass.draw(0..3, 0..1);
                src = 1 - src;
            }
        }
        let fxs = &self.effects;
        self.fx.apply(
            device,
            queue,
            &mut enc,
            &fxs.composition,
            &fxs.images,
            &t.acc_tex[src],
            &t.scratch,
            None,
            input.time,
            input.beat,
        );
        if self.test_card {
            // Show Test Card: the card replaces the picture (outputs, slices, monitor).
            let mut pass = clear_pass(&mut enc, &t.acc[src], wgpu::Color::BLACK);
            pass.set_pipeline(&self.pattern);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        if wanted.contains(&Route::Composition) {
            keep(&mut enc, Route::Composition, Some(&t.acc_tex[src]));
        }
        let alive: std::collections::HashSet<i64> = fxs
            .layer
            .iter()
            .chain(&fxs.outgoing)
            .chain(&fxs.group)
            .flatten()
            .chain(&fxs.composition)
            .map(|e| e.id)
            .collect();
        self.fx.retain(&|id| alive.contains(&id));
        self.fx.retain_images(&|k| fxs.images.contains_key(&k));
        let m = MixUniform {
            opacity: 1.0,
            mode: 0,
            trans_mode: 0,
            progress: -1.0,
            master: input.master.clamp(0.0, 1.0),
            alpha_dst: 0.0,
            time: input.time,
            _pad: 0.0,
        };
        queue.write_buffer(&self.present_mix, 0, bytemuck::bytes_of(&m));
        let group = self.blend_group(device, &self.present_mix, &t.acc[src]);
        {
            let mut pass = clear_pass(&mut enc, &t.output_view, wgpu::Color::BLACK);
            pass.set_pipeline(&self.blend_present);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([enc.finish()]);
    }
}

fn slot_mut<'a>(layers: &'a mut Vec<LayerSlot>, device: &wgpu::Device, layer: usize) -> &'a mut LayerSlot {
    while layers.len() <= layer {
        layers.push(LayerSlot {
            cur: VideoLayer::new(device),
            prev: VideoLayer::new(device),
            mix: uniform_buffer(device, "layer mix", std::mem::size_of::<MixUniform>()),
        });
    }
    &mut layers[layer]
}

impl Targets {
    fn new(device: &wgpu::Device, w: u32, h: u32) -> Self {
        let make = |label: &str| target_texture(device, label, w, h);
        let view = |t: &wgpu::Texture| t.create_view(&Default::default());
        let output = make("composition");
        let output_view = view(&output);
        let acc_tex = [make("accumulator"), make("accumulator")];
        let group_tex = [make("group"), make("group")];
        let layer_tex = [make("layer"), make("layer")];
        Self {
            output,
            output_view,
            acc: [view(&acc_tex[0]), view(&acc_tex[1])],
            layer: [view(&layer_tex[0]), view(&layer_tex[1])],
            group: [view(&group_tex[0]), view(&group_tex[1])],
            acc_tex,
            group_tex,
            layer_tex,
            scratch: make("fx scratch"),
        }
    }
}

/// Human-readable GPU summary for the status bar.
pub fn adapter_summary(adapter: &wgpu::Adapter) -> String {
    let info = adapter.get_info();
    let bc = if adapter.features().contains(WANTED_FEATURES) {
        "BC: yes"
    } else {
        "BC: no (HAP/DXV via CPU)"
    };
    format!("{} · {:?} · {}", info.name, info.backend, bc)
}

/// `blend.wgsl` with its spatial mixer (MIXER BEGIN … END, names ending in
/// `_X`) instantiated for layers (`_L`) and transitions (`_T`).
fn blend_wgsl() -> String {
    let src = include_str!("shaders/blend.wgsl");
    let (head, rest) = src.split_once("// MIXER BEGIN").expect("MIXER BEGIN");
    let (tmpl, tail) = rest.split_once("// MIXER END").expect("MIXER END");
    format!("{head}{}{}{tail}", tmpl.replace("_X", "_L"), tmpl.replace("_X", "_T"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_gpu::gpu;

    /// Read the composition texture back as RGBA rows (width must be a multiple of 64).
    fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, comp: &Compositor) -> Vec<u8> {
        let (w, h) = comp.size();
        let bytes_per_row = w * 4;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (bytes_per_row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            comp.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
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

    /// Renders one frame headless and reads it back. Skips if no GPU adapter.
    #[test]
    fn empty_is_black_and_test_card_on_request() {
        let Some((device, queue)) = gpu() else { return };
        let (w, h) = (640u32, 360u32);
        let mut comp = Compositor::new(&device, w, h);
        let frame = |comp: &Compositor| {
            comp.render(
                &device,
                &queue,
                FrameInput {
                    time: 0.0,
                    beat: 0.0,
                    master: 1.0,
                },
                &[],
            );
            read_back(&device, &queue, comp)
        };
        // Nothing playing: black (no test pattern any more).
        let data = frame(&comp);
        assert!(
            data.chunks(4).all(|p| p[..3] == [0, 0, 0]),
            "empty composition is black"
        );
        // Show Test Card (cell = 30 px, centre 320, 180).
        comp.set_test_card(&queue, true, "Test Card", "640 x 360  16:9");
        let data = frame(&comp);
        let px = |x: u32, y: u32| -> [u8; 4] { data[((y * w + x) * 4) as usize..][..4].try_into().unwrap() };
        let c = px(323, 183);
        assert!(c[0] > 200 && c[2] < 100, "beat lamp (orange at beat 0) {c:?}");
        let b = px(395, 135);
        assert!(b[2] > 200 && b[0] < 30 && b[1] < 30, "blue bar {b:?}");
        let y = px(245, 135);
        assert!(y[0] > 200 && y[1] > 200 && y[2] < 30, "yellow bar {y:?}");
        assert!(px(0, 50)[0] > 200 && px(0, 50)[1] < 100, "red 1 px border");
        // Title "TEST CARD" (2 px dots, rows 83…96) has lit dots around the centre.
        let lit = (83..97)
            .flat_map(|y| (250..390).map(move |x| (x, y)))
            .filter(|&(x, y)| px(x, y)[..3] == [255, 255, 255])
            .count();
        assert!(lit > 100, "title text: {lit} px");
        assert!(data.chunks(4).all(|p| p[3] == 255));
        comp.set_test_card(&queue, false, "", "");
        assert!(frame(&comp).chunks(4).all(|p| p[..3] == [0, 0, 0]), "off again");
        // Look at it: `LAVMFX_CARD_PPM=card.ppm cargo test -p lavmfx-render empty_is_black`
        // writes the card at 1920 × 1080.
        if let Some(out) = std::env::var_os("LAVMFX_CARD_PPM") {
            let mut big = Compositor::new(&device, 1920, 1080);
            big.set_test_card(&queue, true, "Test Card", "1920 x 1080  16:9");
            let d = frame(&big);
            let mut ppm = b"P6\n1920 1080\n255\n".to_vec();
            ppm.extend(d.chunks(4).flat_map(|p| [p[0], p[1], p[2]]));
            std::fs::write(out, ppm).unwrap();
        }
    }

    use lavmfx_media::{ColorInfo, FrameFormat, Plane, VideoFrame, YuvMatrix};

    fn solid(format: FrameFormat, w: u32, h: u32, planes: Vec<Plane>) -> VideoFrame {
        let color = ColorInfo {
            matrix: YuvMatrix::Bt709,
            full_range: false,
        };
        VideoFrame {
            pts: std::time::Duration::ZERO,
            width: w,
            height: h,
            format,
            color,
            planes,
        }
    }

    /// Renders `frame` into a 256×128 comp and returns the centre pixel and a letterbox pixel.
    fn render_frame(frame: &VideoFrame, master: f32) -> Option<([u8; 4], [u8; 4])> {
        let (device, queue) = gpu()?;
        if frame.format.is_bc() && !device.features().contains(WANTED_FEATURES) {
            eprintln!("no BC support; skipping");
            return None;
        }
        let mut comp = Compositor::new(&device, 256, 128);
        comp.set_layer_frame(&device, &queue, 0, frame);
        assert!(comp.has_video());
        comp.render(
            &device,
            &queue,
            FrameInput {
                time: 0.0,
                beat: 0.0,
                master,
            },
            &[],
        );
        let data = read_back(&device, &queue, &comp);
        let px = |x: usize, y: usize| -> [u8; 4] { data[(y * 256 + x) * 4..][..4].try_into().unwrap() };
        Some((px(128, 64), px(4, 64)))
    }

    fn close(a: [u8; 4], b: [u8; 3]) -> bool {
        (0..3).all(|i| (a[i] as i32 - b[i] as i32).abs() <= 3)
    }

    /// BT.709 limited range: RGB(200, 120, 60) ≈ Y=130, Cb=94, Cr=166.
    const YUV: (u8, u8, u8) = (130, 94, 166);
    const RGB: [u8; 3] = [200, 120, 60];

    #[test]
    fn yuv420p_converts_and_letterboxes() {
        // 4:3 frame in a 2:1 comp → pillarboxed; odd size and padded stride.
        let (w, h) = (63u32, 47u32);
        let ch = h.div_ceil(2);
        let p = |v: u8, stride: u32, rows: u32| Plane {
            data: vec![v; (stride * rows) as usize],
            stride,
        };
        let f = solid(
            FrameFormat::Yuv420p,
            w,
            h,
            vec![p(YUV.0, 64, h), p(YUV.1, 32, ch), p(YUV.2, 32 + 8, ch)],
        );
        let Some((centre, edge)) = render_frame(&f, 1.0) else {
            return;
        };
        assert!(close(centre, RGB), "centre {centre:?}");
        assert!(close(edge, [0, 0, 0]), "pillarbox {edge:?}");
    }

    #[test]
    fn nv12_converts_with_master() {
        let (w, h) = (256u32, 128u32);
        let uv: Vec<u8> = std::iter::repeat_n([YUV.1, YUV.2], 128 * 64).flatten().collect();
        let f = solid(
            FrameFormat::Nv12,
            w,
            h,
            vec![
                Plane {
                    data: vec![YUV.0; (w * h) as usize],
                    stride: w,
                },
                Plane { data: uv, stride: 256 },
            ],
        );
        let Some((centre, _)) = render_frame(&f, 0.5) else {
            return;
        };
        assert!(
            close(centre, RGB.map(|c| (c as f32 * 0.5).round() as u8)),
            "centre {centre:?}"
        );
    }

    #[test]
    fn bc1_uploads() {
        // Solid pure-red BC1 blocks: colour0 = colour1 = 0xF800, indices 0.
        let (w, h) = (30u32, 15u32); // not block-aligned
        let blocks = (w.div_ceil(4) * h.div_ceil(4)) as usize;
        let data: Vec<u8> = std::iter::repeat_n([0x00, 0xF8, 0x00, 0xF8, 0, 0, 0, 0], blocks)
            .flatten()
            .collect();
        let f = solid(
            FrameFormat::Bc1,
            w,
            h,
            vec![Plane {
                data,
                stride: w.div_ceil(4) * 8,
            }],
        );
        let Some((centre, _)) = render_frame(&f, 1.0) else {
            return;
        };
        assert!(close(centre, [255, 0, 0]), "centre {centre:?}");
    }

    fn rgba(w: u32, h: u32, c: [u8; 4]) -> VideoFrame {
        let data = std::iter::repeat_n(c, (w * h) as usize).flatten().collect();
        solid(FrameFormat::Rgba8, w, h, vec![Plane { data, stride: w * 4 }])
    }

    /// Layer 1 (4:3, half opacity) over layer 0 (red, full frame): blended in
    /// the middle, layer 0 shows through the pillarbox; layer 2 at 0 is skipped.
    #[test]
    fn layers_stack_with_opacity() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 256, 128);
        comp.set_layer_frame(&device, &queue, 0, &rgba(32, 16, [255, 0, 0, 255]));
        comp.set_layer_frame(&device, &queue, 1, &rgba(40, 30, [0, 0, 255, 255]));
        comp.set_layer_frame(&device, &queue, 2, &rgba(32, 16, [0, 255, 0, 255]));
        comp.render(
            &device,
            &queue,
            FrameInput {
                time: 0.0,
                beat: 0.0,
                master: 1.0,
            },
            &[LayerDraw::alpha(1.0), LayerDraw::alpha(0.5), LayerDraw::alpha(0.0)],
        );
        let data = read_back(&device, &queue, &comp);
        let px = |x: usize, y: usize| -> [u8; 4] { data[(y * 256 + x) * 4..][..4].try_into().unwrap() };
        assert!(close(px(128, 64), [128, 0, 128]), "centre {:?}", px(128, 64));
        assert!(close(px(4, 64), [255, 0, 0]), "pillarbox {:?}", px(4, 64));
        assert_eq!(px(128, 64)[3], 255, "comp stays opaque");

        comp.clear_layer(0);
        comp.render(
            &device,
            &queue,
            FrameInput {
                time: 0.0,
                beat: 0.0,
                master: 1.0,
            },
            &[LayerDraw::alpha(1.0), LayerDraw::alpha(0.5), LayerDraw::alpha(0.0)],
        );
        let data = read_back(&device, &queue, &comp);
        let px = |x: usize, y: usize| -> [u8; 4] { data[(y * 256 + x) * 4..][..4].try_into().unwrap() };
        assert!(close(px(4, 64), [0, 0, 0]), "cleared layer {:?}", px(4, 64));
    }

    /// DXV3 HQ: full-range YCgCo planes. RGB(200, 120, 60) → Y=125, Cg=123, Co=198.
    #[test]
    fn ycgco_converts() {
        let (w, h) = (16u32, 16u32);
        let p = |v: u8, s: u32, r: u32| Plane {
            data: vec![v; (s * r) as usize],
            stride: s,
        };
        let mut f = solid(
            FrameFormat::Yuv420p,
            w,
            h,
            vec![p(125, 16, 16), p(123, 8, 8), p(198, 8, 8)],
        );
        f.color = ColorInfo {
            matrix: YuvMatrix::YCgCo,
            full_range: true,
        };
        let Some((centre, _)) = render_frame(&f, 1.0) else {
            return;
        };
        // 125 - (-5) + 70 = 200 · 125 + (-5) = 120 · 130 - 70 = 60
        assert!(close(centre, RGB), "centre {centre:?}");
    }

    /// Renders full-frame layers and returns the centre pixel.
    fn centre(device: &wgpu::Device, queue: &wgpu::Queue, comp: &Compositor, layers: &[LayerDraw]) -> [u8; 4] {
        let input = FrameInput {
            time: 0.0,
            beat: 0.0,
            master: 1.0,
        };
        comp.render(device, queue, input, layers);
        let data = read_back(device, queue, comp);
        data[(64 * 256 + 128) * 4..][..4].try_into().unwrap()
    }

    #[test]
    fn blend_modes() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 256, 128);
        comp.set_layer_frame(&device, &queue, 0, &rgba(32, 16, [200, 100, 50, 255]));
        comp.set_layer_frame(&device, &queue, 1, &rgba(32, 16, [100, 100, 100, 255]));
        let top = |blend, opacity| {
            [
                LayerDraw::default(),
                LayerDraw {
                    opacity,
                    blend,
                    ..Default::default()
                },
            ]
        };
        let cases: [(u32, f32, [u8; 3]); 7] = [
            (0, 1.0, [100, 100, 100]), // Alpha
            (1, 1.0, [255, 200, 150]), // Add
            (1, 0.5, [250, 150, 100]), // Add at half opacity
            (2, 1.0, [100, 0, 0]),     // Subtract
            (3, 1.0, [78, 39, 20]),    // Multiply
            (6, 1.0, [100, 100, 50]),  // Darken
            (8, 1.0, [100, 0, 50]),    // Difference
        ];
        for (mode, o, want) in cases {
            let c = centre(&device, &queue, &comp, &top(mode, o));
            assert!(close(c, want), "mode {mode} at {o}: {c:?} want {want:?}");
            assert_eq!(c[3], 255);
        }
    }

    #[test]
    fn groups_mix_first_then_blend() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 256, 128);
        comp.set_layer_frame(&device, &queue, 0, &rgba(32, 16, [200, 100, 50, 255]));
        comp.set_layer_frame(&device, &queue, 1, &rgba(32, 16, [100, 100, 100, 255]));
        comp.set_layer_frame(&device, &queue, 2, &rgba(32, 16, [40, 40, 40, 255]));
        let grouped = |l2: f32| {
            [
                LayerDraw::default(),
                LayerDraw {
                    group: Some(0),
                    ..Default::default()
                },
                LayerDraw {
                    group: Some(0),
                    opacity: l2,
                    blend: 1,
                    ..Default::default()
                },
            ]
        };
        // Group (grey + nothing) at 50 % Alpha over orange.
        comp.set_groups(vec![GroupDraw { opacity: 0.5, blend: 0 }]);
        let c = centre(&device, &queue, &comp, &grouped(0.0));
        assert!(close(c, [150, 100, 75]), "{c:?}");
        assert_eq!(c[3], 255);
        // Inside the group grey + 40 (Add) = 140; group added onto orange.
        comp.set_groups(vec![GroupDraw { opacity: 1.0, blend: 1 }]);
        let c = centre(&device, &queue, &comp, &grouped(1.0));
        assert!(close(c, [255, 240, 190]), "{c:?}");
        // Group with no visible layers changes nothing.
        let c = centre(
            &device,
            &queue,
            &comp,
            &[
                LayerDraw::default(),
                LayerDraw {
                    group: Some(0),
                    opacity: 0.0,
                    ..Default::default()
                },
                LayerDraw {
                    group: Some(0),
                    opacity: 0.0,
                    ..Default::default()
                },
            ],
        );
        assert!(close(c, [200, 100, 50]), "{c:?}");
    }

    /// Every blend mode compiles and runs; spatial (transition-style) modes show
    /// only the base at opacity 0 and only the layer at opacity 1.
    #[test]
    fn every_blend_mode_runs_and_spatial_ends_match() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 256, 128);
        comp.set_layer_frame(&device, &queue, 0, &rgba(32, 16, [200, 0, 0, 255]));
        comp.set_layer_frame(&device, &queue, 1, &rgba(32, 16, [0, 0, 200, 255]));
        let draw = |blend, opacity| {
            [
                LayerDraw::default(),
                LayerDraw {
                    opacity,
                    blend,
                    ..Default::default()
                },
            ]
        };
        for mode in 0..BLEND_MODE_COUNT {
            let mid = centre(&device, &queue, &comp, &draw(mode, 0.5));
            assert_eq!(mid[3], 255, "mode {mode}");
            if mode < 23 {
                continue;
            }
            let lo = centre(&device, &queue, &comp, &draw(mode, 0.0));
            assert!(close(lo, [200, 0, 0]), "mode {mode} at 0: {lo:?}");
            let hi = centre(&device, &queue, &comp, &draw(mode, 1.0));
            assert!(close(hi, [0, 0, 200]), "mode {mode} at 1: {hi:?}");
        }
    }

    #[test]
    fn transition_mixes_outgoing_and_incoming() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 256, 128);
        comp.set_layer_frame(&device, &queue, 0, &rgba(32, 16, [200, 0, 0, 255]));
        comp.begin_transition(&device, 0);
        let at = |progress, blend| {
            [LayerDraw {
                transition: Some(Transition { progress, blend }),
                ..Default::default()
            }]
        };
        // Incoming clip has no frame yet: fades from red towards nothing.
        assert!(close(centre(&device, &queue, &comp, &at(0.0, 0)), [200, 0, 0]));
        comp.set_layer_frame(&device, &queue, 0, &rgba(32, 16, [0, 0, 200, 255]));
        assert!(close(centre(&device, &queue, &comp, &at(0.5, 0)), [100, 0, 100]));
        assert!(close(centre(&device, &queue, &comp, &at(1.0, 0)), [0, 0, 200]));
        // Additive transition passes through red + blue halfway.
        assert!(close(centre(&device, &queue, &comp, &at(0.5, 1)), [200, 0, 200]));
        // Without a transition only the incoming clip shows.
        comp.end_transition(0);
        assert!(close(
            centre(&device, &queue, &comp, &[LayerDraw::default()]),
            [0, 0, 200]
        ));
    }

    fn fx(kind: u32, params: &[f32]) -> EffectDraw {
        let mut p = [0.0; EFFECT_PARAMS];
        p[..params.len()].copy_from_slice(params);
        EffectDraw {
            kind,
            params: p,
            opacity: 1.0,
            blend: 0,
            id: kind as i64 + 1,
            feedback: matches!(kind, 20 | 86 | 92 | 93),
            passes: match kind {
                1 => 2,
                83 => 3,
                91 => 4,
                _ => 1,
            },
            history: if matches!(kind, 71 | 94) { 16 } else { 0 },
            image: 0,
        }
    }

    /// Video Router / Feedback: a layer reads another layer (this frame) or
    /// the composition (last frame); a stopped layer routes transparent.
    #[test]
    fn routes() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 256, 128);
        comp.set_layer_frame(&device, &queue, 0, &rgba(256, 128, [0, 0, 255, 255]));
        comp.ensure_layer(&device, 1); // the app does this for non-frame inputs
        let routed = |r| LayerDraw {
            input: LayerInput::Route(r),
            ..LayerDraw::alpha(1.0)
        };
        // Layer 0 nearly invisible; layer 1 routes it (before its opacity): blue.
        let c = centre(
            &device,
            &queue,
            &comp,
            &[LayerDraw::alpha(0.01), routed(Route::Layer(0))],
        );
        assert!(close(c, [0, 0, 255]), "router shows layer 0: {c:?}");
        assert!(comp.route_view(Route::Layer(0)).is_some());
        // Feedback: frame 1 blue; frame 2 layer 0 off, the router shows last frame's blue.
        let c = centre(
            &device,
            &queue,
            &comp,
            &[LayerDraw::alpha(1.0), routed(Route::Composition)],
        );
        assert!(close(c, [0, 0, 255]), "{c:?}");
        let c = centre(
            &device,
            &queue,
            &comp,
            &[LayerDraw::alpha(0.0), routed(Route::Composition)],
        );
        assert!(close(c, [0, 0, 255]), "feedback keeps last frame: {c:?}");
        // A hidden layer (opacity 0 / bypassed) is still routed, not mixed in.
        let c = centre(
            &device,
            &queue,
            &comp,
            &[LayerDraw::alpha(0.0), routed(Route::Layer(0))],
        );
        assert!(close(c, [0, 0, 255]), "hidden layer still routes: {c:?}");
        let c = centre(&device, &queue, &comp, &[LayerDraw::alpha(0.0), LayerDraw::alpha(0.0)]);
        assert!(close(c, [0, 0, 0]), "hidden layer isn't mixed: {c:?}");
        // Router of a stopped layer: transparent (black composition).
        comp.clear_layer(0);
        let c = centre(
            &device,
            &queue,
            &comp,
            &[LayerDraw::alpha(1.0), routed(Route::Layer(0))],
        );
        let c2 = centre(
            &device,
            &queue,
            &comp,
            &[LayerDraw::alpha(1.0), routed(Route::Layer(0))],
        );
        assert!(close(c2, [0, 0, 0]), "stopped layer routes nothing: {c:?} {c2:?}");
    }

    /// Slice Chase Sequencer: each slice at its chase level (red layer over
    /// blue); slices outside the sequence follow Outside.
    #[test]
    fn slice_chase_levels() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 256, 128);
        comp.set_layer_frame(&device, &queue, 0, &rgba(256, 128, [0, 0, 255, 255]));
        comp.set_layer_frame(&device, &queue, 1, &rgba(256, 128, [255, 0, 0, 255]));
        let third = |k: f32| FxSlice {
            p01: [k / 3.0, 0.0, (k + 1.0) / 3.0, 0.0],
            p23: [(k + 1.0) / 3.0, 1.0, k / 3.0, 1.0],
            info: [k, k, 1.0, 0.0],
        };
        let px = |comp: &Compositor, x: usize| -> [u8; 4] {
            let data = read_back(&device, &queue, comp);
            data[(64 * 256 + x) * 4..][..4].try_into().unwrap()
        };
        let both = [LayerDraw::alpha(1.0), LayerDraw::alpha(1.0)];
        for (outside, right) in [(0.0, [0, 0, 255]), (2.0, [255, 0, 0])] {
            let e = fx(103, &[0.0, 0.0, outside]); // Mask, Unlit 0, Outside Clear | Keep
            comp.set_effects(FxFrame {
                layer: vec![vec![], vec![e]],
                slices: vec![third(0.0), third(1.0), third(2.0)],
                chase: std::collections::HashMap::from([(
                    e.id,
                    vec![[1.0, 0.0, 0.0, 1.0], [0.5, 0.0, 1.0, 1.0], [0.0, 0.0, 0.0, 0.0]],
                )]),
                ..Default::default()
            });
            comp.render(
                &device,
                &queue,
                FrameInput {
                    time: 0.0,
                    beat: 0.0,
                    master: 1.0,
                },
                &both,
            );
            assert!(close(px(&comp, 40), [255, 0, 0]), "lit slice: red");
            let mid = px(&comp, 128);
            assert!(close(mid, [128, 0, 127]), "half level: red half over blue, {mid:?}");
            assert!(close(px(&comp, 220), right), "outside the sequence: Clear | Keep");
        }
    }

    /// Slice Chase looks in Input space (one slice over the frame, red layer
    /// over blue): Wipe half-way, Runner outline, Tiles, Sample, Delay.
    #[test]
    fn slice_chase_looks() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 256, 128);
        comp.set_layer_frame(&device, &queue, 0, &rgba(256, 128, [0, 0, 255, 255]));
        comp.set_layer_frame(&device, &queue, 1, &rgba(256, 128, [255, 0, 0, 255]));
        let whole = FxSlice {
            p01: [0.0, 0.0, 1.0, 0.0],
            p23: [1.0, 1.0, 0.0, 1.0],
            info: [0.0, 0.0, 1.0, 0.0],
        };
        let both = [LayerDraw::alpha(1.0), LayerDraw::alpha(1.0)];
        let mut run = |kind: u32, params: &[f32], values: [f32; 4]| -> Vec<u8> {
            let mut e = fx(kind, params);
            e.history = if kind == 108 { 4 } else { 0 };
            comp.set_effects(FxFrame {
                layer: vec![vec![], vec![e]],
                slices: vec![whole],
                chase: std::collections::HashMap::from([(e.id, vec![values])]),
                ..Default::default()
            });
            comp.render(
                &device,
                &queue,
                FrameInput {
                    time: 0.0,
                    beat: 0.0,
                    master: 1.0,
                },
                &both,
            );
            read_back(&device, &queue, &comp)
        };
        let px = |d: &[u8], x: usize, y: usize| -> [u8; 4] { d[(y * 256 + x) * 4..][..4].try_into().unwrap() };
        let lit = [1.0, 0.5, 0.0, 1.0];
        // Wipe: L→R, Reveal, hard edge, half-way through the step.
        let d = run(104, &[0.0, 0.0, 0.001, 1.0, 0.0, 0.0, 0.0], lit);
        assert!(close(px(&d, 64, 64), [255, 0, 0]), "revealed part");
        assert!(close(px(&d, 192, 64), [0, 0, 255]), "not reached yet");
        // Runner: white Outline mode, 10 % wide lines.
        let runner = [1.0, 1.0, 1.0, 1.0, 0.1, 0.0, 1.0, 0.25, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        let d = run(105, &runner, lit);
        assert!(close(px(&d, 2, 64), [255, 255, 255]), "outline");
        assert!(close(px(&d, 128, 64), [0, 0, 255]), "inside: clear");
        // Tiles: 2 × 1 green squares at half size.
        let tiles = [0.0, 1.0, 0.0, 1.0, 2.0, 1.0, 0.0, 0.5, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        let d = run(106, &tiles, lit);
        assert!(close(px(&d, 64, 64), [0, 255, 0]), "a tile");
        assert!(close(px(&d, 8, 64), [0, 0, 255]), "between tiles");
        // Sample: the average colour; as luminance.
        let d = run(107, &[1.0, 0.0, 1.0, 0.0, 0.0], lit);
        assert!(close(px(&d, 128, 64), [255, 0, 0]));
        let d = run(107, &[1.0, 1.0, 1.0, 0.0, 0.0], lit);
        assert!(close(px(&d, 128, 64), [54, 54, 54]), "luminance {:?}", px(&d, 128, 64));
        // Delay: the layer (step 0: no delay); unlit slices still show (Unlit 100 %).
        let d = run(108, &[2.0, 0.0, 1.0, 0.0], [0.0, 1.0, 0.0, 1.0]);
        assert!(close(px(&d, 128, 64), [255, 0, 0]));
    }

    /// M6 part 3: Slice Transform, Mask (RGBA image), UV Map (layers below), Slice Outline.
    #[test]
    fn slice_mask_and_uv_effects() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 256, 128);
        comp.set_layer_frame(&device, &queue, 0, &rgba(256, 128, [0, 0, 255, 255]));
        comp.set_layer_frame(&device, &queue, 1, &rgba(256, 128, [255, 0, 0, 255]));
        let left_half = FxSlice {
            p01: [0.0, 0.0, 0.5, 0.0],
            p23: [0.5, 1.0, 0.0, 1.0],
            info: [0.0, 0.0, 1.0, 0.0],
        };
        let px = |comp: &Compositor, x: usize, y: usize| -> [u8; 4] {
            let data = read_back(&device, &queue, comp);
            data[(y * 256 + x) * 4..][..4].try_into().unwrap()
        };
        let run = |comp: &mut Compositor, f: FxFrame, layers: &[LayerDraw]| {
            comp.set_effects(f);
            comp.render(
                &device,
                &queue,
                FrameInput {
                    time: 0.0,
                    beat: 0.0,
                    master: 1.0,
                },
                layers,
            );
        };
        let one = [LayerDraw::alpha(1.0), LayerDraw::alpha(0.0)];
        // Slice Transform, Stretch, Black BG: red inside the left-half slice, black outside.
        run(
            &mut comp,
            FxFrame {
                layer: vec![vec![fx(99, &[1.0, 2.0])]],
                slices: vec![left_half],
                ..Default::default()
            },
            &one,
        );
        assert!(close(px(&comp, 64, 64), [0, 0, 255]), "layer 0 is blue");
        // Mask with a 2×1 image (white | black), Stretch, Luminance: left kept, right cut.
        let mut m = fx(102, &[2.0, 0.0, 0.0]);
        m.image = 7;
        let img = FxImage {
            width: 2,
            height: 1,
            data: vec![255, 255, 255, 255, 0, 0, 0, 255],
            rgba: true,
        };
        let both = [LayerDraw::alpha(1.0), LayerDraw::alpha(1.0)];
        run(
            &mut comp,
            FxFrame {
                layer: vec![vec![], vec![m]],
                images: std::collections::HashMap::from([(7, std::sync::Arc::new(img))]),
                ..Default::default()
            },
            &both,
        );
        assert!(close(px(&comp, 32, 64), [255, 0, 0]), "mask keeps the left");
        assert!(close(px(&comp, 224, 64), [0, 0, 255]), "mask cuts the right");
        // Slice Transform on layer 1: red only in the slice, blue (layer 0) elsewhere.
        run(
            &mut comp,
            FxFrame {
                layer: vec![vec![], vec![fx(99, &[0.0, 2.0])]],
                slices: vec![left_half],
                ..Default::default()
            },
            &both,
        );
        assert!(close(px(&comp, 32, 64), [255, 0, 0]), "slice shows the layer");
        assert!(close(px(&comp, 224, 64), [0, 0, 255]), "outside: transparent");
        // UV Map on layer 1 (red = u 1, v 0): reads the blue layer below.
        run(
            &mut comp,
            FxFrame {
                layer: vec![vec![], vec![fx(101, &[0.0; 10])]],
                ..Default::default()
            },
            &both,
        );
        assert!(close(px(&comp, 128, 64), [0, 0, 255]), "uv map reads below");
        // Slice Outline (composition chain): stroke at the edge of the output quad
        // (info.w = 1); input quads (info.w = 0) are ignored.
        let mut o = fx(141, &[1.0, 1.0, 1.0, 1.0, 0.2]);
        o.opacity = 1.0;
        let out_quad = FxSlice {
            info: [0.0, 0.0, 0.0, 1.0],
            ..left_half
        };
        let in_quad = FxSlice {
            p01: [0.5, 0.0, 1.0, 0.0],
            p23: [1.0, 1.0, 0.5, 1.0],
            ..left_half
        };
        run(
            &mut comp,
            FxFrame {
                composition: vec![o],
                slices: vec![in_quad, out_quad],
                ..Default::default()
            },
            &[],
        );
        assert!(px(&comp, 130, 64)[0] < 30, "no outline on input quads");
        assert!(px(&comp, 126, 64)[0] > 200, "right edge of the left half");
        // A slice above (x 0.4..0.9) hides the left half's right edge under it.
        let above = FxSlice {
            p01: [0.4, 0.0, 0.9, 0.0],
            p23: [0.9, 1.0, 0.4, 1.0],
            info: [0.0, 1.0, 0.0, 1.0],
        };
        let mut o = fx(141, &[1.0, 1.0, 1.0, 1.0, 0.2]);
        o.opacity = 1.0;
        run(
            &mut comp,
            FxFrame {
                composition: vec![o],
                slices: vec![out_quad, above],
                ..Default::default()
            },
            &[],
        );
        assert!(px(&comp, 126, 64)[0] < 30, "covered outline hidden");
        assert!(px(&comp, 104, 64)[0] > 200, "the upper slice's own edge");
        assert!(px(&comp, 1, 64)[0] > 200, "uncovered edge of the lower slice");
        assert!(px(&comp, 1, 64)[0] > 200, "outline at the slice edge");
        assert!(px(&comp, 64, 64)[0] < 30, "inside the slice: no outline");
        // A polygon slice (diamond in the middle) is outlined along its own
        // edges, not its bounding box.
        let diamond = [[0.5, 0.1], [0.7, 0.5], [0.5, 0.9], [0.3, 0.5]];
        let mut o = fx(141, &[1.0, 1.0, 1.0, 1.0, 0.1]);
        o.opacity = 1.0;
        run(
            &mut comp,
            FxFrame {
                composition: vec![o],
                slices: FxSlice::polygon_edges(&diamond, 0),
                ..Default::default()
            },
            &[],
        );
        assert!(px(&comp, 178, 64)[0] > 200, "right tip of the diamond");
        assert!(px(&comp, 128, 64)[0] < 30, "centre: no outline");
        assert!(px(&comp, 78, 14)[0] < 30, "bounding-box corner: no outline");
        assert!(px(&comp, 177, 15)[0] < 30, "bounding-box corner: no outline");
    }

    /// A Slice Outline layer's effects act on each outline on its own: Radar
    /// sweeps around each slice's centre (the same spot of two equal slices
    /// gets the same light), not around the screen's.
    #[test]
    fn slice_outline_effects_run_per_slice() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 256, 128);
        comp.set_layer_frame(&device, &queue, 0, &rgba(256, 128, [0, 0, 0, 255]));
        let half = |x0: f32, i: f32| FxSlice {
            p01: [x0, 0.0, x0 + 0.5, 0.0],
            p23: [x0 + 0.5, 1.0, x0, 1.0],
            info: [0.0, i, 0.0, 1.0],
        };
        let mut gen_ = fx(141, &[1.0, 1.0, 1.0, 1.0, 0.2]);
        gen_.opacity = 1.0;
        // Radar: beam at 0° (pointing right), short trail, no floor.
        let radar = fx(72, &[0.0, 0.25, 0.0]);
        comp.set_effects(FxFrame {
            layer: vec![vec![gen_, radar]],
            slices: vec![half(0.0, 0.0), half(0.5, 1.0)],
            ..Default::default()
        });
        comp.render(
            &device,
            &queue,
            FrameInput {
                time: 0.0,
                beat: 0.0,
                master: 1.0,
            },
            &[LayerDraw::alpha(1.0)],
        );
        let data = read_back(&device, &queue, &comp);
        let px = |x: usize, y: usize| data[(y * 256 + x) * 4];
        // Just above each right side's middle: inside the trail behind the beam.
        let (a, b) = (px(126, 56), px(254, 56));
        assert!(
            a > 150 && (a as i32 - b as i32).abs() <= 2,
            "same light on both slices: {a} {b}"
        );
        // Below the beam and on the left sides: dark (the screen centre would light x 129).
        assert!(px(126, 80) < 30 && px(254, 80) < 30, "below the beam");
        assert!(
            px(1, 64) < 30 && px(129, 64) < 30,
            "left sides dark: {} {}",
            px(1, 64),
            px(129, 64)
        );
        assert!(px(64, 64) < 30 && px(192, 64) < 30, "no stroke inside");
        // Radar's grid (axes through each slice's centre): clipped to the lines
        // by default; with "Effects Outside Lines" it shows inside the slices.
        let grid = fx(72, &[0.0, 0.25, 1.0, 1.0, 0.5]);
        let mut free = gen_;
        free.params[5] = 1.0;
        for (g, inside) in [(gen_, false), (free, true)] {
            comp.set_effects(FxFrame {
                layer: vec![vec![g, grid]],
                slices: vec![half(0.0, 0.0), half(0.5, 1.0)],
                ..Default::default()
            });
            comp.render(
                &device,
                &queue,
                FrameInput {
                    time: 0.0,
                    beat: 0.0,
                    master: 1.0,
                },
                &[LayerDraw::alpha(1.0)],
            );
            let data = read_back(&device, &queue, &comp);
            let g = |x: usize, y: usize| data[(y * 256 + x) * 4 + 1];
            let c = (g(64, 64), g(192, 64));
            if inside {
                assert!(c.0 > 30 && c.1 > 30, "grid inside each slice: {c:?}");
            } else {
                assert!(c.0 < 10 && c.1 < 10, "clipped to the lines: {c:?}");
            }
        }
    }

    #[test]
    fn effects_run_and_apply() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 256, 128);
        comp.set_layer_frame(&device, &queue, 0, &rgba(256, 128, [255, 0, 0, 255]));
        let with = |comp: &mut Compositor, layer: Vec<EffectDraw>, composition: Vec<EffectDraw>| {
            comp.set_effects(FxFrame {
                layer: vec![layer],
                composition,
                ..Default::default()
            });
            centre(&device, &queue, comp, &[LayerDraw::alpha(1.0)])
        };
        // Every kind compiles and runs (layer and composition chains).
        for k in (0..103).chain(128..142) {
            let mut f = fx(k, &[0.5; 16]);
            if k == 0 {
                f.params[2] = 100.0;
            }
            with(&mut comp, vec![f], vec![f]);
        }
        // Invert RGB: red → cyan.
        let c = with(&mut comp, vec![fx(5, &[1.0, 1.0, 1.0, 0.0])], vec![]);
        assert!(close(c, [0, 255, 255]), "invert {c:?}");
        // Half dry/wet.
        let mut half = fx(5, &[1.0, 1.0, 1.0, 0.0]);
        half.opacity = 0.5;
        let c = with(&mut comp, vec![half], vec![]);
        assert!(close(c, [128, 128, 128]), "dry/wet {c:?}");
        // Transform at 0 % scale: layer vanishes.
        let c = with(&mut comp, vec![fx(0, &[0.0, 0.0, 0.0, 100.0, 100.0])], vec![]);
        assert!(close(c, [0, 0, 0]), "transform {c:?}");
        // Composition chain: Solid Color blue over everything.
        let c = with(&mut comp, vec![], vec![fx(26, &[0.0, 0.0, 1.0, 1.0, 0.0])]);
        assert!(close(c, [0, 0, 255]), "solid {c:?}");
        // Chain order: invert then tint (black→green, white→black): cyan luma ≈ 0.7.
        let c = with(
            &mut comp,
            vec![fx(5, &[1.0, 1.0, 1.0, 0.0]), fx(2, &[-1.0, 0.0])],
            vec![],
        );
        assert!(close(c, [0, 0, 0]), "chain {c:?}");
        // M5b batch 1. Add Subtract +G: red → yellow.
        let c = with(&mut comp, vec![fx(35, &[0.0, 1.0, 0.0])], vec![]);
        assert!(close(c, [255, 255, 0]), "add subtract {c:?}");
        // Levels at 0.5 everywhere is neutral.
        let c = with(&mut comp, vec![fx(32, &[0.5, 0.5, 0.5])], vec![]);
        assert!(close(c, [255, 0, 0]), "levels {c:?}");
        // Chroma Key on red (hue 0) keys it out.
        let ck = [0.0, 0.0, 0.1, 0.1, 0.2, 0.2, 0.2, 0.1, 0.2, 0.1, 0.0, 0.0];
        let c = with(&mut comp, vec![fx(36, &ck)], vec![]);
        assert!(close(c, [0, 0, 0]), "chroma key {c:?}");
        // Heat: full red (luma 0.3) maps into the dark-purple part of the ramp.
        let c = with(&mut comp, vec![fx(54, &[0.5])], vec![]);
        assert!(c[1] < 60 && c[2] > 60, "heat {c:?}");
        // Color Pass keeping hue 0.5..1 greys out red.
        let c = with(&mut comp, vec![fx(34, &[0.5, 0.9, 0.0])], vec![]);
        assert!(close(c, [76, 76, 76]), "color pass {c:?}");
        // Multi-pass (Blur, 2 passes): a solid stays solid; dry/wet mixes with the input,
        // so Invert → half Blur → still half grey (not the intermediate pass).
        let mut b = fx(1, &[1.0, 0.05, 0.05, 0.5, 0.5]);
        b.opacity = 0.5;
        let c = with(&mut comp, vec![b], vec![]);
        assert!(close(c, [255, 0, 0]), "blur {c:?}");
        // Bloom (4 passes) on full red leaves it red and opaque.
        let c = with(&mut comp, vec![fx(91, &[1.0, 0.9, 0.1, 1.0, 0.25, 0.5])], vec![]);
        assert!(close(c, [255, 0, 0]) && c[3] == 255, "bloom {c:?}");
    }

    /// Layers without frames: a generator (Clear) and an effect clip (Below).
    #[test]
    fn source_and_effect_layers() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 64, 32);
        comp.set_layer_frame(&device, &queue, 0, &rgba(64, 32, [255, 0, 0, 255]));
        comp.ensure_layer(&device, 1);
        // Layer 1 = Solid Color source (kind 26: Color, Show Image 0) over red: green.
        let layer = |input| LayerDraw {
            input,
            ..Default::default()
        };
        comp.set_effects(FxFrame {
            layer: vec![vec![], vec![fx(26, &[0.0, 1.0, 0.0, 1.0, 0.0])]],
            ..Default::default()
        });
        let c = centre_of(
            &device,
            &queue,
            &comp,
            &[LayerDraw::default(), layer(LayerInput::Clear)],
        );
        assert!(close(c, [0, 255, 0]), "source {c:?}");
        // Without an effect a Clear layer adds nothing: red stays.
        comp.set_effects(FxFrame::default());
        let c = centre_of(
            &device,
            &queue,
            &comp,
            &[LayerDraw::default(), layer(LayerInput::Clear)],
        );
        assert!(close(c, [255, 0, 0]), "empty source {c:?}");
        // Layer 1 = effect clip with Invert RGB: inverts what is below → cyan.
        comp.set_effects(FxFrame {
            layer: vec![vec![], vec![fx(5, &[1.0, 1.0, 1.0, 0.0])]],
            ..Default::default()
        });
        let c = centre_of(
            &device,
            &queue,
            &comp,
            &[LayerDraw::default(), layer(LayerInput::Below)],
        );
        assert!(close(c, [0, 255, 255]), "effect clip {c:?}");
        // …also on an Add layer (the default blend): replaces, doesn't add.
        let add = LayerDraw {
            blend: 1,
            ..layer(LayerInput::Below)
        };
        let c = centre_of(&device, &queue, &comp, &[LayerDraw::default(), add]);
        assert!(close(c, [0, 255, 255]), "effect clip on Add {c:?}");
        // Only a source layer (no frames anywhere): no test pattern behind it.
        let mut comp = Compositor::new(&device, 64, 32);
        comp.ensure_layer(&device, 0);
        comp.set_effects(FxFrame {
            layer: vec![vec![fx(26, &[0.0, 0.0, 1.0, 1.0, 0.0])]],
            ..Default::default()
        });
        let c = centre_of(&device, &queue, &comp, &[layer(LayerInput::Clear)]);
        assert!(close(c, [0, 0, 255]), "lone source {c:?}");
    }

    fn centre_of(device: &wgpu::Device, queue: &wgpu::Queue, comp: &Compositor, layers: &[LayerDraw]) -> [u8; 4] {
        let input = FrameInput {
            time: 0.0,
            beat: 0.0,
            master: 1.0,
        };
        comp.render(device, queue, input, layers);
        let (w, h) = comp.size();
        let data = read_back(device, queue, comp);
        let i = ((h / 2 * w + w / 2) * 4) as usize;
        data[i..i + 4].try_into().unwrap()
    }

    /// Temporal effects: white at t = 0, black at t = 0.01.
    #[test]
    fn temporal_effects_hold_old_frames() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 64, 32);
        let run = |comp: &mut Compositor, e: EffectDraw| -> [u8; 4] {
            let mut out = [0; 4];
            for (t, v) in [(0.0, 255), (0.01, 0)] {
                comp.set_layer_frame(&device, &queue, 0, &rgba(64, 32, [v, v, v, 255]));
                comp.set_effects(FxFrame {
                    layer: vec![vec![e]],
                    ..Default::default()
                });
                let input = FrameInput {
                    time: t,
                    beat: 0.0,
                    master: 1.0,
                };
                comp.render(&device, &queue, input, &[LayerDraw::alpha(1.0)]);
                let data = read_back(&device, &queue, comp);
                out = data[(16 * 64 + 32) * 4..][..4].try_into().unwrap();
            }
            out
        };
        // Delay RGB: green one frame late → still white; red/blue now → black.
        let c = run(&mut comp, fx(94, &[0.0, 1.0 / 15.0, 0.0]));
        assert!(close(c, [0, 255, 0]), "delay rgb {c:?}");
        // Stop Motion at 0.5 frames/s holds the first frame.
        let c = run(&mut comp, fx(93, &[0.0, 0.0, 0.0]));
        assert!(close(c, [255, 255, 255]), "stop motion {c:?}");
        // Freeze solid holds it too; unfrozen passes the new frame.
        let c = run(&mut comp, fx(92, &[0.0, 0.0, 1.0, 0.0]));
        assert!(close(c, [255, 255, 255]), "freeze {c:?}");
        let c = run(&mut comp, fx(92, &[0.0, 0.0, 0.0, 0.0]));
        assert!(close(c, [0, 0, 0]), "no freeze {c:?}");
    }

    /// Debug contact sheet: `LAVMFX_FX_SHEET="Levels,Heat" cargo test -p lavmfx-render fx_sheet -- --ignored`
    /// renders each effect (catalogue defaults, t = 1.5 s) on a test pattern into
    /// `target/fx_sheet.ppm` (first tile = no effect).
    #[test]
    #[ignore]
    fn fx_sheet() {
        let Some((device, queue)) = gpu() else { return };
        let names = std::env::var("LAVMFX_FX_SHEET").unwrap_or_default();
        let names: Vec<&str> = names.split(',').map(str::trim).filter(|n| !n.is_empty()).collect();
        let (w, h) = (320u32, 180u32);
        // Pattern: hue across, brightness down, a dark disc and a white bar.
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
                let hue = |o: f32| (((u + o) * 6.0).rem_euclid(6.0) - 3.0).abs().clamp(1.0, 2.0) - 1.0;
                let l = 1.0 - v * 0.8;
                let mut c = [hue(0.0) * l, hue(2.0 / 3.0) * l, hue(1.0 / 3.0) * l];
                if ((u - 0.3) * 1.78).hypot(v - 0.5) < 0.2 {
                    c = [0.1, 0.1, 0.15];
                }
                if (0.62..0.7).contains(&u) {
                    c = [1.0; 3];
                }
                data.extend(c.map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8));
                data.push(255);
            }
        }
        let frame = solid(FrameFormat::Rgba8, w, h, vec![Plane { data, stride: w * 4 }]);
        let mut comp = Compositor::new(&device, w, h);
        comp.set_layer_frame(&device, &queue, 0, &frame);
        let cols = 4u32;
        let tiles = names.len() as u32 + 1;
        let rows = tiles.div_ceil(cols);
        let mut sheet = vec![40u8; (w * cols * h * rows * 3) as usize];
        let mut ids = lavmfx_core::IdGen::new();
        for t in 0..tiles {
            let mut layer_input = LayerInput::Frame;
            let chain = match t {
                0 => vec![],
                _ if names[t as usize - 1].starts_with("src:") => {
                    // `src:Gradient`: a source (generator) on a clear layer.
                    let name = &names[t as usize - 1][4..];
                    let spec = lavmfx_core::sources::spec(name).unwrap_or_else(|| panic!("{name}: no source"));
                    let params = spec.instantiate(&mut ids);
                    let f = lavmfx_core::sources::source_fx(name, &params, 1)
                        .unwrap_or_else(|| panic!("{name}: no shader"));
                    layer_input = LayerInput::Clear;
                    vec![EffectDraw {
                        kind: f.kind,
                        params: f.params,
                        opacity: 1.0,
                        blend: 0,
                        id: 1,
                        feedback: false,
                        passes: 1,
                        history: 0,
                        image: 0,
                    }]
                }
                _ => {
                    let name = names[t as usize - 1];
                    let e = lavmfx_core::model::VideoEffect::from_catalog(&mut ids, name)
                        .unwrap_or_else(|| panic!("{name}: not in catalogue"));
                    let f = lavmfx_core::effects::shader_fx(&e).unwrap_or_else(|| panic!("{name}: no shader"));
                    vec![EffectDraw {
                        kind: f.kind,
                        params: f.params,
                        opacity: f.opacity,
                        blend: f.blend,
                        id: f.id,
                        feedback: f.feedback,
                        passes: f.passes,
                        history: f.history,
                        image: 0,
                    }]
                }
            };
            comp.set_effects(FxFrame {
                layer: vec![chain],
                ..Default::default()
            });
            let input = FrameInput {
                time: std::env::var("LAVMFX_FX_TIME")
                    .ok()
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(1.5),
                beat: 3.0,
                master: 1.0,
            };
            comp.ensure_layer(&device, 0);
            let draw = LayerDraw {
                input: layer_input,
                ..LayerDraw::alpha(1.0)
            };
            comp.render(&device, &queue, input, &[draw]);
            let px = read_back(&device, &queue, &comp);
            let (ox, oy) = ((t % cols) * w, (t / cols) * h);
            for y in 0..h {
                for x in 0..w {
                    let s = ((y * w + x) * 4) as usize;
                    let d = (((oy + y) * w * cols + ox + x) * 3) as usize;
                    sheet[d..d + 3].copy_from_slice(&px[s..s + 3]);
                }
            }
        }
        let mut out = format!("P6\n{} {}\n255\n", w * cols, h * rows).into_bytes();
        out.extend(sheet);
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/fx_sheet.ppm");
        std::fs::write(path, out).unwrap();
        eprintln!("wrote {path}");
    }

    #[test]
    fn fit_modes() {
        let (v, c) = ((1280, 720), (1920, 1440));
        assert_eq!(Fit::Fit.scale(v, c), [1.0, 0.75]);
        assert_eq!(Fit::Fill.scale(v, c), [4.0 / 3.0, 1.0]);
        assert_eq!(Fit::Stretch.scale(v, c), [1.0, 1.0]);
        assert_eq!(Fit::Original.scale(v, c), [1280.0 / 1920.0, 0.5]);
    }

    /// ISF programs: effect + mix, y-up conventions, params, persistent multi-pass.
    #[test]
    fn isf_programs_run() {
        use crate::isf::{IsfDesc, IsfImage, IsfPassDesc, IsfType, IsfVar};
        let Some((device, queue)) = gpu() else { return };
        let lib = IsfLibrary::new();
        let mut comp = Compositor::new(&device, 64, 32);
        comp.set_isf_library(lib.clone());
        // Layer 0: top half red, bottom half blue.
        let mut data = Vec::new();
        for y in 0..32 {
            for _ in 0..64 {
                data.extend_from_slice(if y < 16 { &[255, 0, 0, 255] } else { &[0, 0, 255, 255] });
            }
        }
        let frame = solid(FrameFormat::Rgba8, 64, 32, vec![Plane { data, stride: 256 }]);
        comp.set_layer_frame(&device, &queue, 0, &frame);
        let effect = |fs: &str| IsfDesc {
            name: "t".into(),
            fragment: fs.into(),
            images: vec![("inputImage".into(), IsfImage::Input)],
            ..Default::default()
        };
        let draw = |kind: u32, params: &[f32]| {
            let mut d = fx(0, params);
            d.kind = kind;
            d.id = kind as i64;
            d.passes = 1;
            d
        };
        // Top / bottom pixel after `chain` on layer 0 (rows are top-first).
        let run = |comp: &mut Compositor, layers: Vec<Vec<EffectDraw>>, draws: &[LayerDraw]| {
            comp.set_effects(FxFrame {
                layer: layers,
                ..Default::default()
            });
            comp.render(
                &device,
                &queue,
                FrameInput {
                    time: 0.0,
                    beat: 0.0,
                    master: 1.0,
                },
                draws,
            );
            let d = read_back(&device, &queue, comp);
            let px = |y: usize| -> [u8; 4] { d[(y * 64 + 32) * 4..][..4].try_into().unwrap() };
            (px(4), px(28))
        };
        let k = ISF_KIND_BASE;
        // Invert (straight ISF effect) with a float input.
        let mut inv = effect(
            "void main() { vec4 c = IMG_THIS_PIXEL(inputImage); gl_FragColor = vec4(mix(c.rgb, 1.0 - c.rgb, amount), c.a); }",
        );
        inv.vars.push(IsfVar {
            name: "amount".into(),
            ty: IsfType::Float,
            slot: Some(0),
            default: [0.0; 4],
        });
        lib.compile(&device, &queue, k, &inv).unwrap();
        let (top, bottom) = run(&mut comp, vec![vec![draw(k, &[1.0])]], &[LayerDraw::alpha(1.0)]);
        assert!(
            close(top, [0, 255, 255]) && close(bottom, [255, 255, 0]),
            "invert {top:?} {bottom:?}"
        );
        // Dry/wet still applies.
        let mut half = draw(k, &[1.0]);
        half.opacity = 0.5;
        let (top, _) = run(&mut comp, vec![vec![half]], &[LayerDraw::alpha(1.0)]);
        assert!(close(top, [128, 128, 128]), "dry/wet {top:?}");
        // y-up: normalised y 0.9 is near the top of the input (red), and
        // isf_FragNormCoord / gl_FragCoord count from the bottom.
        let up = effect(concat!(
            "void main() {\n",
            "  vec4 top = IMG_NORM_PIXEL(inputImage, vec2(0.5, 0.9));\n",
            "  float a = isf_FragNormCoord.y > 0.5 ? 1.0 : 0.0;\n",
            "  float b = gl_FragCoord.y > RENDERSIZE.y * 0.5 ? 1.0 : 0.0;\n",
            "  gl_FragColor = vec4(top.r, a, b, 1.0);\n",
            "}\n"
        ));
        lib.compile(&device, &queue, k + 1, &up).unwrap();
        let (top, bottom) = run(&mut comp, vec![vec![draw(k + 1, &[])]], &[LayerDraw::alpha(1.0)]);
        assert!(
            close(top, [255, 255, 255]) && close(bottom, [255, 0, 0]),
            "y-up {top:?} {bottom:?}"
        );
        // Persistent target: +0.25 per frame, read back by the last pass.
        let mut acc = effect(concat!(
            "void main() {\n",
            "  if (PASSINDEX == 0) gl_FragColor = IMG_THIS_PIXEL(acc) + vec4(0.25, 0.0, 0.0, 0.25);\n",
            "  else gl_FragColor = vec4(IMG_THIS_PIXEL(acc).r, 0.0, 0.0, 1.0);\n",
            "}\n"
        ));
        acc.passes = vec![
            IsfPassDesc {
                target: Some("acc".into()),
                persistent: true,
                float: true,
                width: Some("$WIDTH/2".into()),
                height: Some("$HEIGHT/2".into()),
            },
            IsfPassDesc::default(),
        ];
        lib.compile(&device, &queue, k + 2, &acc).unwrap();
        let (one, _) = run(&mut comp, vec![vec![draw(k + 2, &[])]], &[LayerDraw::alpha(1.0)]);
        let (two, _) = run(&mut comp, vec![vec![draw(k + 2, &[])]], &[LayerDraw::alpha(1.0)]);
        assert!(
            close(one, [64, 0, 0]) && close(two, [128, 0, 0]),
            "persistent {one:?} {two:?}"
        );
        // Generator on a clear layer, colour input from the params.
        let mut gen_ = IsfDesc {
            name: "g".into(),
            fragment: "void main() { gl_FragColor = tint; }".into(),
            ..Default::default()
        };
        gen_.vars.push(IsfVar {
            name: "tint".into(),
            ty: IsfType::Color,
            slot: Some(0),
            default: [0.0; 4],
        });
        lib.compile(&device, &queue, k + 3, &gen_).unwrap();
        comp.ensure_layer(&device, 1);
        let layers = [
            LayerDraw::alpha(1.0),
            LayerDraw {
                input: LayerInput::Clear,
                ..LayerDraw::alpha(1.0)
            },
        ];
        let (top, _) = run(
            &mut comp,
            vec![vec![], vec![draw(k + 3, &[0.0, 1.0, 0.0, 1.0])]],
            &layers,
        );
        assert!(close(top, [0, 255, 0]), "generator {top:?}");
        // A kind without a program is skipped (input unchanged).
        let (top, _) = run(&mut comp, vec![vec![draw(k + 50, &[])]], &[LayerDraw::alpha(1.0)]);
        assert!(close(top, [255, 0, 0]), "missing program {top:?}");
        // Broken GLSL is refused with a message, nothing installed.
        let e = lib
            .compile(&device, &queue, k + 4, &effect("void main() { gl_FragColor = nope; }"))
            .unwrap_err();
        assert!(e.contains("line 1"), "{e}");
        assert!(!lib.contains(k + 4));
    }

    /// External (CPU) effects: the frame goes out and back, mixed by dry/wet;
    /// a source ignores its input; two in one chain don't share a texture.
    #[test]
    fn external_effects_run_through_the_cpu() {
        struct Fake;
        impl ExternalFx for Fake {
            fn is_source(&self, kind: u32) -> bool {
                kind == EXT_KIND_BASE + 1
            }
            fn process(
                &self,
                kind: u32,
                _: i64,
                p: &[f32; EFFECT_PARAMS],
                input: Option<&[u8]>,
                w: u32,
                h: u32,
                _: f32,
                _: f32,
            ) -> Option<Vec<u8>> {
                match input {
                    // Invert RGB; top half only when p[0] > 0.5 (checks row order).
                    Some(px) => Some(
                        px.chunks(4)
                            .enumerate()
                            .flat_map(|(i, c)| {
                                let top = (i as u32 / w) < h / 2;
                                if p[0] > 0.5 && !top {
                                    [c[0], c[1], c[2], c[3]]
                                } else {
                                    [255 - c[0], 255 - c[1], 255 - c[2], c[3]]
                                }
                            })
                            .collect(),
                    ),
                    None => (kind == EXT_KIND_BASE + 1).then(|| [0u8, 0, 255, 255].repeat((w * h) as usize)),
                }
            }
        }
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 64, 32);
        comp.set_external_fx(std::sync::Arc::new(Fake));
        comp.set_layer_frame(&device, &queue, 0, &rgba(64, 32, [255, 0, 0, 255]));
        let draw = |kind: u32, id: i64, p0: f32| {
            let mut d = fx(0, &[p0]);
            d.kind = kind;
            d.id = id;
            d.passes = 1;
            d
        };
        let run = |comp: &mut Compositor, layers: Vec<Vec<EffectDraw>>, draws: &[LayerDraw]| {
            comp.set_effects(FxFrame {
                layer: layers,
                ..Default::default()
            });
            comp.render(
                &device,
                &queue,
                FrameInput {
                    time: 0.0,
                    beat: 0.0,
                    master: 1.0,
                },
                draws,
            );
            let d = read_back(&device, &queue, comp);
            let px = |y: usize| -> [u8; 4] { d[(y * 64 + 32) * 4..][..4].try_into().unwrap() };
            (px(4), px(28))
        };
        let (top, bottom) = run(
            &mut comp,
            vec![vec![draw(EXT_KIND_BASE, 1, 0.0)]],
            &[LayerDraw::alpha(1.0)],
        );
        assert!(
            close(top, [0, 255, 255]) && close(bottom, [0, 255, 255]),
            "invert {top:?} {bottom:?}"
        );
        // Rows stay in order: only the top half inverted.
        let (top, bottom) = run(
            &mut comp,
            vec![vec![draw(EXT_KIND_BASE, 1, 1.0)]],
            &[LayerDraw::alpha(1.0)],
        );
        assert!(
            close(top, [0, 255, 255]) && close(bottom, [255, 0, 0]),
            "rows {top:?} {bottom:?}"
        );
        // Two in a chain = invert twice = original; then a GPU effect after works too.
        let mut half = draw(EXT_KIND_BASE, 3, 0.0);
        half.opacity = 0.5;
        let (top, _) = run(
            &mut comp,
            vec![vec![draw(EXT_KIND_BASE, 1, 0.0), draw(EXT_KIND_BASE, 2, 0.0)]],
            &[LayerDraw::alpha(1.0)],
        );
        assert!(close(top, [255, 0, 0]), "twice {top:?}");
        let (top, _) = run(&mut comp, vec![vec![half]], &[LayerDraw::alpha(1.0)]);
        assert!(close(top, [128, 128, 128]), "dry/wet {top:?}");
        // Source on a clear layer.
        comp.ensure_layer(&device, 1);
        let layers = [
            LayerDraw::alpha(1.0),
            LayerDraw {
                input: LayerInput::Clear,
                ..LayerDraw::alpha(1.0)
            },
        ];
        let (top, _) = run(&mut comp, vec![vec![], vec![draw(EXT_KIND_BASE + 1, 4, 0.0)]], &layers);
        assert!(close(top, [0, 0, 255]), "source {top:?}");
    }

    /// More than two layers all reach the output (user: "sometimes only 2 layers
    /// mix"): Add sums every layer; Alpha at 50 % stacks; with a group in the middle.
    #[test]
    fn many_layers_all_mix() {
        let Some((device, queue)) = gpu() else { return };
        let mut comp = Compositor::new(&device, 256, 128);
        let cols = [
            [40u8, 0, 0],
            [0, 40, 0],
            [0, 0, 40],
            [40, 40, 0],
            [0, 40, 40],
            [40, 0, 40],
        ];
        for (i, c) in cols.iter().enumerate() {
            comp.set_layer_frame(&device, &queue, i, &rgba(32, 16, [c[0], c[1], c[2], 255]));
        }
        // Every slot gets a draw (as the app does): unused ones at opacity 0.
        for n in [3usize, 4, 6] {
            let draws: Vec<LayerDraw> = (0..6)
                .map(|i| LayerDraw {
                    blend: if i == 0 { 0 } else { 1 },
                    ..LayerDraw::alpha(if i < n { 1.0 } else { 0.0 })
                })
                .collect();
            let want = (0..3)
                .map(|k| cols[..n].iter().map(|c| c[k] as u32).sum::<u32>().min(255) as u8)
                .collect::<Vec<_>>();
            let c = centre(&device, &queue, &comp, &draws);
            assert!(
                close(c, [want[0], want[1], want[2]]),
                "Add, {n} layers: {c:?} want {want:?}"
            );
        }
        // Alpha 50 % stacking: each layer halves what's below.
        comp.set_layer_frame(&device, &queue, 0, &rgba(32, 16, [0, 0, 0, 255]));
        for i in 1..6 {
            comp.set_layer_frame(&device, &queue, i, &rgba(32, 16, [255, 255, 255, 255]));
        }
        let draws: Vec<LayerDraw> = (0..6)
            .map(|i| LayerDraw::alpha(if i == 0 { 1.0 } else { 0.5 }))
            .collect();
        let c = centre(&device, &queue, &comp, &draws);
        let want = (255.0 * (1.0 - 0.5f32.powi(5))).round() as u8; // 247
        assert!(close(c, [want, want, want]), "alpha stack: {c:?} want {want}");
        // Layers 2–3 in a group, Add inside, group Add on top of layers 0–1.
        for (i, c) in cols.iter().enumerate() {
            comp.set_layer_frame(&device, &queue, i, &rgba(32, 16, [c[0], c[1], c[2], 255]));
        }
        comp.set_groups(vec![GroupDraw { opacity: 1.0, blend: 1 }]);
        let draws: Vec<LayerDraw> = (0..6)
            .map(|i| LayerDraw {
                blend: if i == 0 { 0 } else { 1 },
                group: (2..4).contains(&i).then_some(0),
                ..LayerDraw::alpha(if i < 5 { 1.0 } else { 0.0 })
            })
            .collect();
        let want = (0..3)
            .map(|k| cols[..5].iter().map(|c| c[k] as u32).sum::<u32>().min(255) as u8)
            .collect::<Vec<_>>();
        let c = centre(&device, &queue, &comp, &draws);
        assert!(
            close(c, [want[0], want[1], want[2]]),
            "with a group: {c:?} want {want:?}"
        );
    }
}
