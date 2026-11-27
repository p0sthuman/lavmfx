//! Outputs (the Output menu): disabled, the composition fullscreen on a
//! display or in a window, or the **Advanced Output** setup (screens with
//! slices, see `lavmfx_core::output`).
//!
//! Every output is an egui immediate viewport (drawn in the main frame) sharing the UI's wgpu device.
//! Advanced screens render into their own texture first
//! (`lavmfx_render::slices`); simple modes show the composition texture.
//! The setup and mode are global (`advanced_output.json` beside
//! the preferences), not part of the composition.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui::{self, Color32, FontId, Rect, Stroke, ViewportBuilder, ViewportCommand, ViewportId, pos2, vec2};
use eframe::egui_wgpu::RenderState;
use lavmfx_core::output::{AdvancedOutput, Device, Screen};
use lavmfx_core::sources::RouteTarget;
use lavmfx_render::slices::{ScreenLook, ScreenOp, ScreenTarget, SliceParams, SlicePass, SliceVertex, fan_indices};
use serde::{Deserialize, Serialize};

/// Output menu choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum OutputMode {
    #[default]
    Disabled,
    /// Composition fullscreen on display N.
    Fullscreen(usize),
    /// Composition in a window (opened on display N where the system allows).
    Windowed(usize),
    /// Screens + slices.
    Advanced,
}

/// A system display.
#[derive(Debug, Clone, PartialEq)]
pub struct Monitor {
    pub name: String,
    pub size: (u32, u32),
    /// Top-left in logical points (desktop coordinates).
    pub pos: (f32, f32),
    /// Top-left in physical pixels (desktop coordinates).
    pub phys_pos: (i32, i32),
    /// Display scale (physical pixels per logical point).
    pub scale: f32,
}

/// Windows: where an output window goes. It's created as a plain window,
/// moved onto the display in physical pixels, and only then made fullscreen.
/// Creating it fullscreen on a display with another scale than the main one
/// lets Windows' DPI change shift it half off the display.
#[derive(Clone, Copy, Debug)]
struct Placement {
    monitor: usize,
    /// Display top-left, physical pixels.
    phys_pos: (i32, i32),
    scale: f32,
    fullscreen: bool,
}

/// Output windows are placed by moving them (Windows) instead of opening
/// them fullscreen on a display (Linux: Wayland can only do the latter).
const PLACE_BY_MOVING: bool = cfg!(windows);

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct SavedOutputs {
    mode: OutputMode,
    setup: Option<AdvancedOutput>,
    /// Composition size the setup's input selections refer to.
    comp: (u32, u32),
    preset: Option<String>,
}

fn file() -> PathBuf {
    crate::app::config_dir().join("advanced_output.json")
}

/// Per-frame state an output window draws (shared with its viewport).
/// Where the mouse points on the main window this frame (Output › Show
/// Mouse Pointer): set while the Composition Monitor or the Advanced Output
/// editor is hovered, consumed by [`Outputs::show`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pointer {
    /// Composition pixels (Composition Monitor, editor Input Selection).
    Comp([f32; 2]),
    /// Pixels of the screen with this id (editor Output Transformation).
    Screen(u64, [f32; 2]),
}

#[derive(Clone)]
struct WindowState {
    texture: egui::TextureId,
    /// Content size in pixels (aspect for letterboxing).
    size: (u32, u32),
    /// Stretch to the window (display outputs) instead of letterboxing.
    stretch: bool,
    /// Windowed on a chosen display: the window opens fullscreen there and
    /// leaves fullscreen after a few frames (Wayland can't place windows;
    /// fullscreen is the only way to pick the output).
    unfullscreen: bool,
    /// Windows: move onto this display (then fullscreen) in the first frames.
    place: Option<Placement>,
    title: String,
    overlay: Overlay,
}

#[derive(Clone, Default)]
struct Overlay {
    /// Show Test Card: slice outlines + names and the output's title over the
    /// card (the card itself is the composition picture).
    test_card: bool,
    /// Slice outlines in content pixels (+ names).
    slices: Vec<(Vec<[f32; 2]>, String)>,
    /// Big number ("Identify Displays").
    identify: Option<String>,
    /// Size / fps text ("Show Display Info").
    info: Option<String>,
    /// Show Mouse Pointer: crosshairs at these content pixels (the main
    /// window's pointer), and one following the pointer over this window
    /// (its own cursor hidden).
    pointer: bool,
    cursor: Vec<[f32; 2]>,
}

struct OutputWindow {
    id: ViewportId,
    state: Arc<Mutex<WindowState>>,
    closed: Arc<AtomicBool>,
    frames: Arc<AtomicU32>,
    builder: ViewportBuilder,
}

/// Render target of one advanced screen.
struct ScreenView {
    screen_id: u64,
    target: ScreenTarget,
    texture: egui::TextureId,
}

pub struct Outputs {
    pub mode: OutputMode,
    pub setup: AdvancedOutput,
    /// Composition size the input selections refer to.
    comp: (u32, u32),
    pub monitors: Vec<Monitor>,
    pub test_card: bool,
    pub display_info: bool,
    /// Output › Show Mouse Pointer: a thin full-size crosshair on the outputs.
    pub show_pointer: bool,
    /// This frame's pointer (see [`Pointer`]).
    pub pointer: Option<Pointer>,
    identify_until: Option<Instant>,
    pass: SlicePass,
    views: Vec<ScreenView>,
    windows: Vec<OutputWindow>,
    dirty: Option<Instant>,
    /// Render every enabled screen (the editor shows them), not only shown ones.
    pub render_all: bool,
    pub status: Option<String>,
    /// Name of the preset the setup came from / was saved as.
    pub preset: Option<String>,
    /// The layer playing a Slice Outline source, with its opacity: it isn't
    /// mixed into the composition; its picture (generator + effects, drawn at
    /// the slices' output positions) is laid over the outline screen.
    pub outline: Option<(usize, f32)>,
    /// Bypassed layers / groups: slices reading them show nothing (Video
    /// Routers still get them). Opacity 0 doesn't count.
    pub bypassed: Vec<RouteTarget>,
    /// Slice Chase in output space: level per slice id this frame (missing = 1).
    pub slice_levels: HashMap<u64, f32>,
    /// Slice Chase looks (Wipe, Runner…) in output space, per slice id this frame.
    pub slice_looks: HashMap<u64, Vec<lavmfx_render::slices::SliceLook>>,
    /// Slice Chase Delay in output space: frames back per slice id this frame.
    pub slice_delays: HashMap<u64, u32>,
    /// Recent composition frames for it and the newest one's index; kept only
    /// while a Delay needs them.
    ring: Option<(Vec<(eframe::wgpu::Texture, eframe::wgpu::TextureView)>, usize)>,
}

/// Composition frames an output Delay can reach back (as the layer's Slice Delay).
const DELAY_FRAMES: usize = lavmfx_core::effects::SLICE_DELAY_FRAMES as usize;

impl Outputs {
    pub fn new(gpu: &RenderState, comp: (u32, u32)) -> Self {
        let saved: SavedOutputs = std::fs::read_to_string(file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let mut setup = saved.setup.unwrap_or_else(|| AdvancedOutput::with_default_screen(comp));
        if saved.comp != (0, 0) {
            setup.fit_composition(saved.comp, comp);
        }
        Self {
            // Outputs start off: a window must not pop up on a projector (or
            // over the screen) just by starting the app. The last mode is still
            // saved; Output › … (or `--output`) turns them on.
            mode: OutputMode::Disabled,
            setup,
            comp,
            monitors: Vec::new(),
            test_card: false,
            show_pointer: false,
            pointer: None,
            display_info: false,
            identify_until: None,
            pass: SlicePass::new(&gpu.device),
            views: Vec::new(),
            windows: Vec::new(),
            dirty: None,
            render_all: false,
            status: None,
            preset: saved.preset,
            outline: None,
            bypassed: Vec::new(),
            slice_levels: Default::default(),
            slice_looks: Default::default(),
            slice_delays: Default::default(),
            ring: None,
        }
    }

    /// Mark the setup changed (saved a second later).
    pub fn touch(&mut self) {
        self.dirty = Some(Instant::now());
    }

    pub fn save_now(&mut self) {
        self.dirty = None;
        let s = SavedOutputs {
            mode: self.mode,
            setup: Some(self.setup.clone()),
            comp: self.comp,
            preset: self.preset.clone(),
        };
        let path = file();
        let res = path
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|_| std::fs::write(&path, serde_json::to_string_pretty(&s).unwrap_or_default()));
        if let Err(e) = res {
            self.status = Some(format!("Can't save the output setup: {e}"));
        }
    }

    pub fn set_mode(&mut self, mode: OutputMode) {
        self.mode = mode;
        self.save_now();
    }

    pub fn is_on(&self) -> bool {
        self.mode != OutputMode::Disabled
    }

    pub fn identify(&mut self) {
        self.identify_until = Some(Instant::now() + std::time::Duration::from_secs(3));
    }

    pub fn comp_size(&self) -> (u32, u32) {
        self.comp
    }

    /// Refresh the display list (call with the root winit window).
    pub fn poll_monitors(&mut self, frame: &eframe::Frame) {
        let Some(w) = frame.winit_window() else { return };
        let before = std::mem::take(&mut self.monitors);
        self.monitors = w
            .available_monitors()
            .enumerate()
            .map(|(i, m)| Monitor {
                name: m.name().unwrap_or_else(|| format!("Display {}", i + 1)),
                size: (m.size().width, m.size().height),
                pos: {
                    let p = m.position();
                    let k = m.scale_factor().max(0.1);
                    ((p.x as f64 / k) as f32, (p.y as f64 / k) as f32)
                },
                phys_pos: (m.position().x, m.position().y),
                scale: m.scale_factor().max(0.1) as f32,
            })
            .collect();
        if self.monitors != before {
            for (i, m) in self.monitors.iter().enumerate() {
                log::info!(
                    "display {}: {} {}x{} px at {},{} px, scale {}",
                    i + 1,
                    m.name,
                    m.size.0,
                    m.size.1,
                    m.phys_pos.0,
                    m.phys_pos.1,
                    m.scale
                );
            }
        }
    }

    /// Windows: how to put an output window on display `d` (None elsewhere,
    /// or when the display isn't known).
    fn placement(&self, d: usize, fullscreen: bool) -> Option<Placement> {
        if !PLACE_BY_MOVING {
            return None;
        }
        let m = self.monitors.get(d)?;
        Some(Placement {
            monitor: d,
            phys_pos: m.phys_pos,
            scale: m.scale,
            fullscreen,
        })
    }

    /// Display index of a screen device, re-found by name when the list changed.
    fn display_index(&self, index: usize, name: &str) -> usize {
        if !name.is_empty()
            && let Some(i) = self.monitors.iter().position(|m| m.name == name)
        {
            return i;
        }
        index
    }

    /// Every enabled slice of the enabled screens, as effects see them
    /// (input quads in composition uv; name rank for Slice Delay's Order).
    pub fn fx_slices(&self) -> Vec<lavmfx_render::FxSlice> {
        let (w, h) = (self.comp.0.max(1) as f32, self.comp.1.max(1) as f32);
        // Same slices, same order as Slice Chase's `chase::slice_infos`.
        let all: Vec<&lavmfx_core::output::Slice> = self.setup.effect_slices().map(|(_, sl)| sl).collect();
        let mut by_name: Vec<usize> = (0..all.len()).collect();
        by_name.sort_by(|&a, &b| all[a].name.to_lowercase().cmp(&all[b].name.to_lowercase()));
        all.iter()
            .enumerate()
            .map(|(i, sl)| {
                let q = sl.input.map(|p| [p[0] / w, p[1] / h]);
                let rank = by_name.iter().position(|&j| j == i).unwrap_or(i);
                lavmfx_render::FxSlice {
                    p01: [q[0][0], q[0][1], q[1][0], q[1][1]],
                    p23: [q[2][0], q[2][1], q[3][0], q[3][1]],
                    info: [rank as f32, i as f32, 1.0, 0.0],
                }
            })
            .chain(self.outline_quads())
            .collect()
    }

    /// Screen the Slice Outline is laid over: the first enabled one.
    fn outline_screen(&self) -> Option<usize> {
        self.setup.screens.iter().position(|s| s.enabled)
    }

    /// Output shapes (screen uv) of the outline screen's slices, for the
    /// Slice Outline generator: quads (info.w = 1), polygons as their edges
    /// (info.w = 2), bottom to top.
    fn outline_quads(&self) -> Vec<lavmfx_render::FxSlice> {
        let Some(s) = self.outline_screen().map(|i| &self.setup.screens[i]) else {
            return Vec::new();
        };
        let (w, h) = (s.width.max(1) as f32, s.height.max(1) as f32);
        let uv = |p: &lavmfx_core::output::Pt| [p[0] / w, p[1] / h];
        s.slices
            .iter()
            .filter(|sl| sl.enabled && !sl.is_key)
            .enumerate()
            .flat_map(|(i, sl)| match &sl.polygon {
                Some(g) => lavmfx_render::FxSlice::polygon_edges(&g.output.iter().map(uv).collect::<Vec<_>>(), i),
                None => {
                    let q = sl.output.map(|p| uv(&p));
                    vec![lavmfx_render::FxSlice {
                        p01: [q[0][0], q[0][1], q[1][0], q[1][1]],
                        p23: [q[2][0], q[2][1], q[3][0], q[3][1]],
                        info: [0.0, i as f32, 0.0, 1.0],
                    }]
                }
            })
            .collect()
    }

    /// Texture of an advanced screen (rendered this frame), for the editor.
    pub fn screen_texture(&self, screen_id: u64) -> Option<(egui::TextureId, (u32, u32))> {
        self.views
            .iter()
            .find(|v| v.screen_id == screen_id)
            .map(|v| (v.texture, v.target.size()))
    }

    /// Layer / group results the slices read (their Input Source): the
    /// compositor keeps copies of these.
    pub fn wanted_routes(&self) -> Vec<lavmfx_render::Route> {
        let mut v: Vec<lavmfx_render::Route> = self
            .setup
            .screens
            .iter()
            .filter(|s| s.enabled)
            .flat_map(|s| s.slices.iter().filter(|sl| sl.enabled))
            .filter(|sl| !self.bypassed.contains(&sl.input_source))
            .filter_map(|sl| route_of(sl.input_source))
            .collect();
        if let Some((li, _)) = self.outline {
            v.push(lavmfx_render::Route::Layer(li as u32));
        }
        v.sort_by_key(|r| format!("{r:?}"));
        v.dedup();
        v
    }

    /// Render the advanced screens from the composition texture (and the
    /// slices' routed inputs). Call after the composition rendered, every frame.
    pub fn render(&mut self, gpu: &RenderState, compositor: &lavmfx_render::Compositor) {
        let comp_view = compositor.view();
        let comp = compositor.size();
        if comp != self.comp {
            self.setup.fit_composition(self.comp, comp);
            self.comp = comp;
            self.touch();
        }
        if self.dirty.is_some_and(|t| t.elapsed().as_secs_f32() > 1.0) {
            self.save_now();
        }
        self.update_ring(gpu, compositor);
        let advanced = self.mode == OutputMode::Advanced;
        let wanted: Vec<usize> = self
            .setup
            .screens
            .iter()
            .enumerate()
            .filter(|(_, s)| s.enabled && (self.render_all || (advanced && s.device != Device::Virtual)))
            .map(|(i, _)| i)
            .collect();

        // Drop views of screens no longer rendered.
        let keep: Vec<u64> = wanted.iter().map(|&i| self.setup.screens[i].id).collect();
        let mut renderer = gpu.renderer.write();
        self.views.retain(|v| {
            let k = keep.contains(&v.screen_id);
            if !k {
                renderer.free_texture(&v.texture);
            }
            k
        });
        for &i in &wanted {
            let s = &self.setup.screens[i];
            let pos = self.views.iter().position(|v| v.screen_id == s.id);
            let pos = match pos {
                Some(p) if self.views[p].target.size() == (s.width, s.height) => p,
                Some(p) => {
                    let t = ScreenTarget::new(&gpu.device, s.width, s.height);
                    renderer.update_egui_texture_from_wgpu_texture(
                        &gpu.device,
                        t.view(),
                        eframe::wgpu::FilterMode::Linear,
                        self.views[p].texture,
                    );
                    self.views[p].target = t;
                    p
                }
                None => {
                    let target = ScreenTarget::new(&gpu.device, s.width, s.height);
                    let texture =
                        renderer.register_native_texture(&gpu.device, target.view(), eframe::wgpu::FilterMode::Linear);
                    self.views.push(ScreenView {
                        screen_id: s.id,
                        target,
                        texture,
                    });
                    self.views.len() - 1
                }
            };
            // Input textures: 0 = composition, then each routed source used.
            let sources: Vec<RouteTarget> = {
                let mut v: Vec<RouteTarget> = s.slices.iter().map(|sl| sl.input_source).collect();
                v.retain(|r| *r != RouteTarget::Composition && !self.bypassed.contains(r));
                v.sort_by_key(|r| format!("{r:?}"));
                v.dedup();
                v
            };
            let views: Vec<Option<eframe::wgpu::TextureView>> = sources
                .iter()
                .map(|r| route_of(*r).and_then(|r| compositor.route_view(r)))
                .collect();
            let mut inputs: Vec<&eframe::wgpu::TextureView> = vec![comp_view];
            let mut tex_of = vec![(RouteTarget::Composition, 0u32)];
            for (r, view) in sources.iter().zip(&views) {
                if let Some(view) = view {
                    tex_of.push((*r, inputs.len() as u32));
                    inputs.push(view);
                }
            }
            // Delayed slices (Slice Chase Delay on the outputs) read an older frame.
            let mut delayed: HashMap<u64, u32> = HashMap::new();
            if let Some((ring, head)) = &self.ring {
                let mut layer_input: HashMap<usize, u32> = HashMap::new();
                for sl in &s.slices {
                    let Some(&d) = self.slice_delays.get(&sl.id) else {
                        continue;
                    };
                    if sl.input_source != RouteTarget::Composition || d == 0 {
                        continue;
                    }
                    let n = ring.len();
                    let layer = (head + n - (d as usize).min(n - 1)) % n;
                    let at = *layer_input.entry(layer).or_insert_with(|| {
                        inputs.push(&ring[layer].1);
                        inputs.len() as u32 - 1
                    });
                    delayed.insert(sl.id, at);
                }
            }
            // Slice Outline layer over the outline screen.
            let outline_view = self
                .outline
                .filter(|_| self.outline_screen() == Some(i))
                .and_then(|(li, o)| Some((compositor.route_view(lavmfx_render::Route::Layer(li as u32))?, o)));
            let overlay = outline_view.as_ref().map(|(view, o)| {
                inputs.push(view);
                (inputs.len() as u32 - 1, *o)
            });
            let (v, idx, params, ops) = screen_mesh(
                s,
                comp,
                &tex_of,
                overlay,
                &self.slice_levels,
                &self.slice_looks,
                &delayed,
            );
            let look = ScreenLook {
                opacity: s.color.opacity,
                brightness: s.color.brightness,
                contrast: s.color.contrast,
                rgb: [s.color.red, s.color.green, s.color.blue],
            };
            self.pass.render(
                &gpu.device,
                &gpu.queue,
                &inputs,
                &self.views[pos].target,
                look,
                &v,
                &idx,
                &params,
                &ops,
            );
        }
    }

    /// Keep the last `DELAY_FRAMES` composition frames while an output-space
    /// Slice Chase Delay runs (dropped as soon as none does: ~250 MB at 1080p).
    fn update_ring(&mut self, gpu: &RenderState, compositor: &lavmfx_render::Compositor) {
        if self.slice_delays.is_empty() {
            self.ring = None;
            return;
        }
        let src = compositor.texture();
        let size = src.size();
        let fresh = self.ring.as_ref().is_none_or(|(r, _)| r[0].0.size() != size);
        if fresh {
            let ring = (0..DELAY_FRAMES)
                .map(|_| {
                    let t = gpu.device.create_texture(&eframe::wgpu::TextureDescriptor {
                        label: Some("output delay"),
                        size,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: eframe::wgpu::TextureDimension::D2,
                        format: lavmfx_render::COMP_FORMAT,
                        usage: eframe::wgpu::TextureUsages::TEXTURE_BINDING | eframe::wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                    let v = t.create_view(&Default::default());
                    (t, v)
                })
                .collect();
            self.ring = Some((ring, 0));
        }
        let Some((ring, head)) = &mut self.ring else { return };
        *head = (*head + 1) % ring.len();
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        for (k, (t, _)) in ring.iter().enumerate() {
            // Until there is a history, every frame back is this one.
            if k == *head || fresh {
                enc.copy_texture_to_texture(src.as_image_copy(), t.as_image_copy(), size);
            }
        }
        gpu.queue.submit([enc.finish()]);
    }

    /// Open / update / close the output windows. Call every frame.
    pub fn show(&mut self, ctx: &egui::Context, comp_tex: egui::TextureId, fps: f32) {
        if self.identify_until.is_some_and(|t| Instant::now() > t) {
            self.identify_until = None;
        }
        let identify = self.identify_until.is_some();
        let pointer = self.pointer.take().filter(|_| self.show_pointer);
        let mut wanted: Vec<(ViewportId, WindowState, ViewportBuilder)> = Vec::new();
        match self.mode {
            OutputMode::Disabled => {}
            OutputMode::Fullscreen(d) | OutputMode::Windowed(d) => {
                let full = matches!(self.mode, OutputMode::Fullscreen(_));
                let id = ViewportId::from_hash_of(("lavmfx-output", full, d));
                let title = format!("LAVMFX – Output (Display {})", d + 1);
                let aspect = self.comp.0 as f32 / self.comp.1.max(1) as f32;
                let mut b = ViewportBuilder::default()
                    .with_title(title.clone())
                    .with_app_id("lavmfx-output")
                    .with_inner_size([960.0, 960.0 / aspect]);
                let place = self.placement(d, full);
                if place.is_some() {
                    if full {
                        b = b.with_decorations(false);
                    }
                } else if full {
                    b = b.with_monitor(d).with_decorations(false);
                } else if let Some(m) = self.monitors.get(d).filter(|_| self.monitors.len() > 1) {
                    // X11 honours the position; Wayland gets the fullscreen detour.
                    b = b.with_position([m.pos.0 + 40.0, m.pos.1 + 40.0]).with_monitor(d);
                }
                let st = WindowState {
                    texture: comp_tex,
                    size: self.comp,
                    stretch: false,
                    unfullscreen: !full && place.is_none() && self.monitors.len() > 1,
                    place,
                    title,
                    overlay: Overlay {
                        test_card: self.test_card,
                        slices: Vec::new(),
                        identify: identify.then(|| format!("{}", d + 1)),
                        info: self.display_info.then(|| info_text(self.comp, fps)),
                        pointer: self.show_pointer,
                        cursor: match pointer {
                            Some(Pointer::Comp(p)) => vec![p],
                            _ => Vec::new(),
                        },
                    },
                };
                wanted.push((id, st, b));
            }
            OutputMode::Advanced => {
                for (i, s) in self.setup.screens.iter().enumerate() {
                    if !s.enabled || s.device == Device::Virtual {
                        continue;
                    }
                    let Some((texture, size)) = self.screen_texture(s.id) else {
                        continue;
                    };
                    let display = match &s.device {
                        Device::Display { index, name } => Some(self.display_index(*index, name)),
                        _ => None,
                    };
                    let id = ViewportId::from_hash_of(("lavmfx-screen", s.id, display));
                    let title = format!("LAVMFX – {}", s.name);
                    let aspect = size.0 as f32 / size.1.max(1) as f32;
                    let mut b = ViewportBuilder::default()
                        .with_title(title.clone())
                        .with_app_id("lavmfx-output")
                        .with_inner_size([960.0, 960.0 / aspect]);
                    let place = display.and_then(|d| self.placement(d, true));
                    if let Some(d) = display {
                        b = b.with_decorations(false);
                        if place.is_none() {
                            b = b.with_monitor(d);
                        }
                    }
                    let st = WindowState {
                        texture,
                        size,
                        stretch: display.is_some(),
                        unfullscreen: false,
                        place,
                        title,
                        overlay: Overlay {
                            test_card: self.test_card,
                            slices: if self.test_card {
                                s.slices
                                    .iter()
                                    .filter(|sl| sl.enabled)
                                    .map(|sl| (sl.outline(), sl.name.clone()))
                                    .collect()
                            } else {
                                Vec::new()
                            },
                            identify: identify.then(|| format!("{}  {}", i + 1, s.name)),
                            info: self.display_info.then(|| info_text(size, fps)),
                            pointer: self.show_pointer,
                            cursor: match pointer {
                                Some(Pointer::Screen(id, p)) if id == s.id => vec![p],
                                Some(Pointer::Comp(p)) => s.comp_to_screen(p),
                                _ => Vec::new(),
                            },
                        },
                    };
                    wanted.push((id, st, b));
                }
            }
        }

        // Close windows that are no longer wanted; update the rest.
        self.windows.retain(|w| wanted.iter().any(|(id, ..)| *id == w.id));
        for (id, st, b) in wanted {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                *w.state.lock().unwrap() = st;
                w.builder = b;
            } else {
                self.windows.push(OutputWindow {
                    id,
                    state: Arc::new(Mutex::new(st)),
                    closed: Arc::new(AtomicBool::new(false)),
                    frames: Arc::new(AtomicU32::new(0)),
                    builder: b,
                });
            }
        }
        let mut closed_any = false;
        for w in &self.windows {
            if w.closed.swap(false, Ordering::Relaxed) {
                closed_any = true;
                continue;
            }
            let state = w.state.clone();
            let closed = w.closed.clone();
            let frames = w.frames.clone();
            // Immediate viewport: drawn as part of every main-window frame.
            // A deferred one keeps its own repaint schedule, and on Windows
            // that schedule stalled: the output only updated when the main
            // window moved, and everything stopped when it lost focus.
            ctx.show_viewport_immediate(w.id, w.builder.clone(), |ui, _class| {
                crate::app::prof::mark("output window ui");
                let st = state.lock().unwrap().clone();
                window_ui(ui, &st, &closed, &frames);
                crate::app::prof::mark("output window ui end");
            });
        }
        if closed_any {
            // Closing an output window = Output › Disabled.
            self.windows.clear();
            self.set_mode(OutputMode::Disabled);
        }
    }
}

/// Windows placement, over the first frames: move the window onto the display
/// (physical pixels, so the display's scale doesn't matter), let the DPI
/// change settle, then make it fullscreen there or give it its window size.
fn place_window(ctx: &egui::Context, st: &WindowState, pl: Placement, frame: u32) {
    // OuterPosition is in this window's points: physical / its current scale.
    let ppp = ctx.pixels_per_point().max(0.1);
    match frame {
        1 => {
            let inset = if pl.fullscreen { 0.0 } else { 40.0 * pl.scale };
            let x = pl.phys_pos.0 as f32 + inset;
            let y = pl.phys_pos.1 as f32 + inset;
            log::info!("output window: move to display {} at {x}, {y} px", pl.monitor + 1);
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos2(x / ppp, y / ppp)));
        }
        4 if pl.fullscreen => {
            log::info!("output window: fullscreen on display {}", pl.monitor + 1);
            ctx.send_viewport_cmd(ViewportCommand::SetMonitor(pl.monitor));
        }
        4 => {
            let aspect = st.size.0 as f32 / st.size.1.max(1) as f32;
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(960.0, 960.0 / aspect)));
        }
        _ => {}
    }
}

fn info_text(size: (u32, u32), fps: f32) -> String {
    format!("{} × {}  ·  {:.0} fps", size.0, size.1, fps)
}

/// Meshes, params and draw steps of a screen: enabled slices bottom first
/// (input masks as stencil fans; an "Is Key" slice keys the next one), then
/// the output masks.
fn route_of(t: RouteTarget) -> Option<lavmfx_render::Route> {
    match t {
        RouteTarget::Composition => None,
        RouteTarget::Layer(i) => Some(lavmfx_render::Route::Layer(i as u32)),
        RouteTarget::Group(i) => Some(lavmfx_render::Route::Group(i as u32)),
    }
}

fn screen_mesh(
    s: &Screen,
    comp: (u32, u32),
    tex_of: &[(RouteTarget, u32)],
    overlay: Option<(u32, f32)>,
    levels: &HashMap<u64, f32>,
    looks: &HashMap<u64, Vec<lavmfx_render::slices::SliceLook>>,
    delayed: &HashMap<u64, u32>,
) -> (Vec<SliceVertex>, Vec<u32>, Vec<SliceParams>, Vec<ScreenOp>) {
    let (mut v, mut idx, mut params, mut ops) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let fan = |v: &mut Vec<SliceVertex>, idx: &mut Vec<u32>, pts: &[[f32; 2]]| {
        let base = v.len() as u32;
        v.extend(pts.iter().map(|p| SliceVertex {
            pos: *p,
            ..Default::default()
        }));
        let start = idx.len() as u32;
        idx.extend(fan_indices(base, pts.len() as u32));
        start..idx.len() as u32
    };
    let mut keyed = false;
    for sl in s.slices.iter().filter(|sl| sl.enabled) {
        // A routed input not available yet (first frame): skip the slice.
        let Some(tex) = tex_of.iter().find(|(r, _)| *r == sl.input_source).map(|t| t.1) else {
            continue;
        };
        let tex = delayed.get(&sl.id).copied().unwrap_or(tex);
        let k = params.len() as u32;
        let m = sl.mesh(comp, 8);
        let base = v.len() as u32;
        v.extend(m.vertices.iter().map(|mv| SliceVertex {
            pos: mv.pos,
            uv: mv.uv,
            local: mv.local,
            slice: k,
        }));
        let start = idx.len() as u32;
        idx.extend(m.indices.iter().map(|i| i + base));
        let indices = start..idx.len() as u32;
        let e = &sl.soft_edge;
        let soft = e.enabled && sl.polygon.is_none();
        params.push(SliceParams {
            adjust: [
                sl.color.brightness,
                sl.color.contrast,
                if sl.black_bg { 1.0 } else { 0.0 },
                if soft { 1.0 } else { 0.0 },
            ],
            rgb: [
                sl.color.red,
                sl.color.green,
                sl.color.blue,
                if keyed && !sl.is_key { 1.0 } else { 0.0 },
            ],
            edge: [e.left, e.right, e.top, e.bottom],
            curve: [e.luminance, e.power, e.gamma, 0.0],
            // Slice Chase (output space) dims the whole slice …
            mult: [levels.get(&sl.id).copied().unwrap_or(1.0), 0.0, 0.0, 0.0],
            // … and draws its looks in it.
            inq: {
                let (cw, ch) = (comp.0.max(1) as f32, comp.1.max(1) as f32);
                let q = sl.input.map(|p| [p[0] / cw, p[1] / ch]);
                [
                    [q[0][0], q[0][1], q[1][0], q[1][1]],
                    [q[2][0], q[2][1], q[3][0], q[3][1]],
                ]
            },
            size: {
                let o = sl.outline();
                let (lo, hi) = o.iter().fold(([f32::MAX; 2], [f32::MIN; 2]), |(lo, hi), p| {
                    ([lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])])
                });
                let n = looks
                    .get(&sl.id)
                    .map_or(0, |v| v.len().min(lavmfx_render::slices::MAX_LOOKS));
                [(hi[0] - lo[0]).max(1.0), (hi[1] - lo[1]).max(1.0), n as f32, 0.0]
            },
            looks: {
                let mut a = [lavmfx_render::slices::SliceLook::default(); lavmfx_render::slices::MAX_LOOKS];
                for (k, l) in looks.get(&sl.id).into_iter().flatten().take(a.len()).enumerate() {
                    a[k] = *l;
                }
                a
            },
        });
        if sl.is_key {
            ops.push(ScreenOp::Key { indices, tex });
            keyed = true;
            continue;
        }
        keyed = false;
        let mask = sl.input_mask_on_screen().map(|pts| fan(&mut v, &mut idx, &pts));
        let invert = sl.input_mask.as_ref().is_some_and(|m| m.invert);
        ops.push(ScreenOp::Slice {
            indices,
            mask,
            invert,
            tex,
        });
    }
    // Slice Outline: its layer's picture over the whole screen, under the masks.
    if let Some((tex, opacity)) = overlay.filter(|o| o.1 > 0.0) {
        let k = params.len() as u32;
        params.push(SliceParams {
            mult: [opacity, 0.0, 0.0, 0.0],
            ..Default::default()
        });
        let (w, h) = (s.width as f32, s.height as f32);
        let base = v.len() as u32;
        for (p, uv) in [
            ([0.0, 0.0], [0.0, 0.0]),
            ([w, 0.0], [1.0, 0.0]),
            ([w, h], [1.0, 1.0]),
            ([0.0, h], [0.0, 1.0]),
        ] {
            v.push(SliceVertex {
                pos: p,
                uv,
                local: uv,
                slice: k,
            });
        }
        let start = idx.len() as u32;
        idx.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        ops.push(ScreenOp::Slice {
            indices: start..idx.len() as u32,
            mask: None,
            invert: false,
            tex,
        });
    }
    for m in s.masks.iter().filter(|m| m.enabled) {
        let o = m.outline();
        if o.len() >= 3 {
            let f = fan(&mut v, &mut idx, &o);
            ops.push(ScreenOp::Black {
                fan: f,
                invert: m.invert,
            });
        }
    }
    (v, idx, params, ops)
}

/// Largest rect of the given aspect ratio centred in `outer`.
pub fn letterbox(outer: Rect, aspect: f32) -> Rect {
    let size = outer.size();
    let fit = if size.x / size.y > aspect {
        vec2(size.y * aspect, size.y)
    } else {
        vec2(size.x, size.x / aspect)
    };
    Rect::from_center_size(outer.center(), fit)
}

fn window_ui(ui: &mut egui::Ui, st: &WindowState, closed: &AtomicBool, frames: &AtomicU32) {
    let ctx = ui.ctx().clone();
    if ctx.input(|i| i.viewport().close_requested()) {
        closed.store(true, Ordering::Relaxed);
    }
    let frame = frames.fetch_add(1, Ordering::Relaxed);
    if let Some(pl) = st.place {
        place_window(&ctx, st, pl, frame);
    }
    if frame == 4 && st.unfullscreen {
        let aspect = st.size.0 as f32 / st.size.1.max(1) as f32;
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(960.0, 960.0 / aspect)));
    }
    // F11 / double-click toggles fullscreen, Esc leaves it.
    let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
    let toggle = ctx.input(|i| i.key_pressed(egui::Key::F11));
    let leave = fullscreen && ctx.input(|i| i.key_pressed(egui::Key::Escape));

    let resp = egui::CentralPanel::no_frame()
        .show(ui, |ui| {
            let full = ui.max_rect();
            let p = ui.painter();
            p.rect_filled(full, 0.0, Color32::BLACK);
            let aspect = st.size.0 as f32 / st.size.1.max(1) as f32;
            let rect = if st.stretch { full } else { letterbox(full, aspect) };
            p.image(
                st.texture,
                rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            draw_overlay(p, rect, st);
            let r = ui.interact(full, ui.scope_id().with("output-area"), egui::Sense::click());
            // Show Mouse Pointer with the pointer over this window: its own
            // cursor hidden, the crosshair follows it.
            if st.overlay.pointer
                && let Some(m) = r.hover_pos()
            {
                ui.ctx().set_cursor_icon(egui::CursorIcon::None);
                crosshair(p, rect, m);
            }
            r
        })
        .inner;
    if toggle || leave || resp.double_clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fullscreen && !leave));
    }
    ctx.request_repaint();
}

fn draw_overlay(p: &egui::Painter, rect: Rect, st: &WindowState) {
    let o = &st.overlay;
    let sx = rect.width() / st.size.0.max(1) as f32;
    let sy = rect.height() / st.size.1.max(1) as f32;
    let to = |q: [f32; 2]| pos2(rect.min.x + q[0] * sx, rect.min.y + q[1] * sy);
    if o.test_card {
        // The card itself is the composition picture (the compositor draws it);
        // here: this output's slice outlines + names and its title.
        for (q, name) in &o.slices {
            let pts: Vec<_> = q.iter().map(|&v| to(v)).collect();
            p.add(egui::Shape::closed_line(
                pts,
                Stroke::new(2.0, Color32::from_rgb(255, 200, 60)),
            ));
            let (lo, hi) = lavmfx_core::output::bounds_of(q);
            p.text(
                to([(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0]),
                egui::Align2::CENTER_CENTER,
                name,
                FontId::proportional(28.0),
                Color32::from_rgb(255, 200, 60),
            );
        }
        let g = p.layout_no_wrap(st.title.clone(), FontId::proportional(20.0), Color32::WHITE);
        let at = rect.left_top() + vec2(rect.width() * 0.5 - g.size().x * 0.5, rect.height() * 0.12);
        p.rect_filled(
            Rect::from_min_size(at, g.size()).expand2(vec2(8.0, 4.0)),
            4.0,
            Color32::from_black_alpha(190),
        );
        p.galley(at, g, Color32::WHITE);
    }
    for &c in &o.cursor {
        crosshair(p, rect, to(c));
    }
    if let Some(n) = &o.identify {
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            n,
            FontId::proportional(rect.height() * 0.25),
            Color32::from_rgb(120, 240, 200),
        );
    }
    if let Some(i) = &o.info {
        p.text(
            rect.right_bottom() - vec2(12.0, 10.0),
            egui::Align2::RIGHT_BOTTOM,
            i,
            FontId::monospace(16.0),
            Color32::WHITE,
        );
    }
}

/// Show Mouse Pointer: a thin "+" through `at` across the whole picture
/// (white, with a dark edge so it shows on any content).
fn crosshair(p: &egui::Painter, rect: Rect, at: egui::Pos2) {
    if !rect.expand(0.5).contains(at) {
        return;
    }
    let (x, y) = (at.x.round() + 0.5, at.y.round() + 0.5);
    let lines = [
        [pos2(rect.min.x, y), pos2(rect.max.x, y)],
        [pos2(x, rect.min.y), pos2(x, rect.max.y)],
    ];
    let p = p.with_clip_rect(rect);
    for l in lines {
        p.line_segment(l, Stroke::new(3.0, Color32::from_black_alpha(150)));
    }
    for l in lines {
        p.line_segment(l, Stroke::new(1.0, Color32::WHITE));
    }
}
