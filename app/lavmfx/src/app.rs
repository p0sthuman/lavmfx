//! Main window. Layout follows the usual VJ-app arrangement:
//! clip grid on top, composition/layer controls bottom-left, preview bottom-right.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use anyhow::Context as _;
use eframe::egui::{self, Color32, Panel, RichText, pos2, vec2};
use eframe::egui_wgpu::RenderState;
use lavmfx_core::transport::TransportSettings;
use lavmfx_core::{Clock, Composition, Id};
use lavmfx_render::{Compositor, FrameInput, LayerDraw, Transition};

use crate::output::{OutputMode, Outputs};
use crate::playback::{self, Opening, Outgoing, Playback, Slot, Tick};
use crate::thumbs::Thumbs;

mod audio_fx_ui;
mod chase_ui;
mod clipboard;
mod dock;
mod effects_ui;
mod live;
mod media_ui;
pub(crate) mod prefs;
mod presets;
pub(crate) mod prof;
mod remote;
mod screens_ui;
mod shortcuts;
mod smpte;
mod tablet;
pub(crate) use dock::config_dir;
mod ffgl_host;
mod help;
mod isf_host;
mod preview;
mod record_ui;
mod render_ui;
mod structure;
mod text;
mod theme;
mod toolbar;
mod widgets;

use lavmfx_core::effects::FxTarget;
use lavmfx_core::mix::shader_blend;
use shortcuts::{LearnTarget, learn_target};
use widgets::{
    LABEL_W, RowOpts, Wheel, bool_row, choice_combo, choice_combo_with, choice_row, param_row, section, toggle,
    wheel_slider,
};

const COMP_SIZE: (u32, u32) = (1920, 1080);
const CLIP_W: f32 = 112.0;
/// Slot height incl. the name strip (`NAME_H`).
const CLIP_H: f32 = 86.0;

pub struct LavmfxApp {
    comp: Composition,
    path: Option<PathBuf>,
    clock: Clock,
    gpu: RenderState,
    compositor: Compositor,
    /// ISF shader programs (shared by both compositors).
    isf: isf_host::IsfHost,
    /// FFGL plugin host (OpenGL thread; Preferences › Video folders).
    ffgl: std::sync::Arc<ffgl_host::Ffgl>,
    /// Composition as an NDI source / shared video (Output menu / Preferences › Video).
    live_out: crate::live_out::LiveOut,
    /// Recording besides the recorder (audio tap, size).
    rec: record_ui::RecordState,
    /// Clip › Render to File jobs and results.
    render_queue: render_ui::RenderQueue,
    comp_tex: egui::TextureId,
    pub(crate) outputs: Outputs,
    /// Advanced Output editor (open = Some).
    screens_editor: Option<screens_ui::Editor>,
    monitors_polled: Option<Instant>,
    gpu_summary: String,
    /// Focused slot (layer, column) in the active deck: the clip panel shows
    /// it and it anchors Shift-selection.
    selected_clip: Option<(usize, usize)>,
    /// All selected slots in the active deck (includes `selected_clip`).
    selection: BTreeSet<(usize, usize)>,
    /// Clip slot being dragged (by its thumbnail).
    clip_drag: Option<(usize, usize)>,
    status: String,
    fps: FpsCounter,
    /// Playing clip per layer (index = layer). Keeps playing across deck switches.
    playing: Vec<Option<Playback>>,
    /// Clip being opened per layer; replaces `playing[layer]` when ready.
    opening: Vec<Option<Opening>>,
    /// Running transition per layer: the clip fading out.
    outgoing: Vec<Option<Outgoing>>,
    /// Layer group shown in the Group panel.
    selected_group: Option<usize>,
    /// Slot to scroll into view on the next grid pass (layer select → its playing clip).
    reveal: Option<(usize, usize)>,
    /// Dock tab to bring to the front after this frame's panels.
    focus_tab: Option<dock::Tab>,
    /// Open rename dialog: what, and the text being edited.
    rename: Option<(structure::RenameTarget, String)>,
    /// Group band rows of the last grid pass, with the group's top layer.
    group_bands: Vec<(egui::Rect, usize)>,
    /// Visible part of the clip grid (drop targets outside it are scrolled away).
    grid_view: egui::Rect,
    /// Layer shown in the layer panel.
    selected_layer: Option<usize>,
    /// Right edge (screen x) of the clip grid's pinned header column; slots
    /// left of it are hidden under the headers.
    grid_pin: f32,
    /// Box selection being dragged on the clip grid.
    grid_marquee: Option<Marquee>,
    /// Screen position of the clip grid's content origin (moves with scrolling).
    grid_origin: egui::Pos2,
    /// Last position of clips that stopped (for play-mode-away "Continue").
    resume: HashMap<Id, f64>,
    /// Last launched column (deck, column), highlighted in the header.
    launched_column: Option<(usize, usize)>,
    thumbs: Thumbs,
    /// Audio waveforms of clip files (grid slots, clip banner).
    waves: crate::waves::Waves,
    /// Copied clips (Ctrl+C / Ctrl+X), pasted with Ctrl+V.
    clipboard: lavmfx_core::clipboard::ClipBoard,
    started: Instant,
    last_tick: Instant,
    /// Wall seconds the last tick advanced the playheads by (clip audio rate).
    last_wall_dt: f64,
    last_stats: Instant,
    /// Clip slot rectangles from the last frame, for drag-and-drop targeting.
    slot_rects: Vec<(egui::Rect, (usize, usize))>,
    /// Pointer position while files are dragged over the window, and how
    /// many files are being dragged. Only known where the backend reports
    /// motion during a drag (not winit 0.30 X11/Wayland).
    drag: Option<(egui::Pos2, usize)>,
    /// Files dropped but not yet placed: (files, when, pointer position at drop).
    pending_drop: Option<(Vec<PathBuf>, Instant, Option<egui::Pos2>)>,
    /// Pending "Open video…" dialog result.
    picker: Option<mpsc::Receiver<Vec<PathBuf>>>,
    /// GPU samples BC textures, so HAP decodes to raw blocks.
    native_bc: bool,
    /// Docked panels.
    dock: egui_dock::DockState<dock::Tab>,
    /// View-menu toggles.
    view: dock::ViewOptions,
    /// Preferences (saved in `~/.config/lavmfx/preferences.json`).
    prefs: prefs::Preferences,
    prefs_win: prefs::PrefsWindow,
    /// Help › About LAVMFX… window open.
    about_open: bool,
    /// Audio devices: FFT input tap (M5).
    audio: crate::audio::Audio,
    /// Preferences changed outside the window (toolbar, panels): save once idle.
    prefs_dirty: Option<Instant>,
    /// Appearance last applied to egui.
    theme_applied: Option<theme::Appearance>,
    /// Composition › Media Manager window (open while Some).
    media: Option<media_ui::MediaManager>,
    /// Slice Chase: running chases and the window.
    chase: chase_ui::ChaseState,
    /// Test card as last sent to the compositor (composition size), None = off.
    test_card_shown: Option<(u32, u32)>,
    /// Quit confirmation dialog open / confirmed.
    quit_dialog: bool,
    quit_ok: bool,
    /// Quit once the pending Save As… has written the file.
    quit_after_save: bool,
    /// Fingerprint of the composition as last saved / opened / created (the
    /// quit dialog only asks when it differs), or the worker computing it.
    saved_key: Option<u64>,
    saved_job: Option<mpsc::Receiver<u64>>,
    /// Layout JSON last written to `layout.json` (to save only on change).
    layout_saved: String,
    layout_checked: Instant,
    /// Preview monitor: its compositor, texture, clip and clip being opened.
    preview_comp: Compositor,
    /// Effect tried on the Preview Monitor (double-click in the Effects
    /// browser), whether it's on, and whether this frame's click kept it.
    preview_fx: Option<(effects_ui::FxDrop, lavmfx_core::VideoEffect)>,
    preview_fx_on: bool,
    preview_fx_keep: bool,
    preview_tex: egui::TextureId,
    preview: Option<Playback>,
    preview_opening: Option<Opening>,
    /// Composition-time step of the last tick (for the preview).
    last_dt: f64,
    /// Pending Composition › Open… dialog.
    comp_picker: Option<mpsc::Receiver<Option<PathBuf>>>,
    /// Pending "Save As…" file dialog.
    save_picker: Option<mpsc::Receiver<Option<PathBuf>>>,
    /// Report of the last `.avc` import (file name, report), shown until OK.
    import_report: Option<(String, lavmfx_project::avc::ImportReport)>,
    /// Layer whose name strip is being dragged (reorder).
    layer_drag: Option<usize>,
    /// Layer row rectangles from the last grid pass, top row first.
    layer_rows: Vec<(egui::Rect, usize)>,
    /// Name typed in the "Save Layout As…" dialog (`Some` = dialog open).
    layout_name: Option<String>,
    /// Composition undo / redo (M7).
    history: lavmfx_core::history::History,
    /// Last user input or control message not yet committed to `history`.
    history_poke: Option<Instant>,
    /// Snapshot whose undo key a worker thread is computing (see `track_history`).
    history_job: Option<mpsc::Receiver<(lavmfx_core::Composition, u64)>>,
    /// Launches waiting for their Beat Snap boundary (beat, launch).
    launch_queue: Vec<(f64, live::Launch)>,
    /// Deck the queue belongs to.
    launch_deck: Option<usize>,
    /// Keyboard + MIDI shortcuts (`shortcuts.json`; default keys built in).
    shortcuts: shortcuts::Shortcuts,
    /// MIDI devices and feedback.
    midi: shortcuts::MidiState,
    dmx_in: shortcuts::DmxInState,
    /// SMPTE 1 / 2 timecode inputs.
    smpte: smpte::SmpteState,
    /// Shortcuts › Edit Keyboard / MIDI (learn mode) when on.
    learn: Option<shortcuts::Learn>,
    /// Ableton Link (created the first time it is switched on).
    link: Option<lavmfx_io::link::Link>,
    /// Last tempo tap (the TAP button flashes).
    tap_flash: Option<Instant>,
    /// OSC in / out and the REST webserver.
    remote: remote::Remote,
}

impl LavmfxApp {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Vec<PathBuf>, show_output: bool) -> anyhow::Result<Self> {
        let gpu = cc.wgpu_render_state.clone().context("wgpu renderer not available")?;
        prof::start_watchdog();
        {
            let i = gpu.adapter.get_info();
            log::info!(
                "GPU: {} ({:?}, driver {} {})",
                i.name,
                i.backend,
                i.driver,
                i.driver_info
            );
        }
        let outputs = Outputs::new(&gpu, (1920, 1080));
        let mut compositor = Compositor::new(&gpu.device, COMP_SIZE.0, COMP_SIZE.1);
        let comp_tex = gpu.renderer.write().register_native_texture(
            &gpu.device,
            compositor.view(),
            eframe::wgpu::FilterMode::Linear,
        );
        let mut preview_comp = Compositor::new(&gpu.device, preview::PREVIEW_SIZE.0, preview::PREVIEW_SIZE.1);
        let preview_tex = gpu.renderer.write().register_native_texture(
            &gpu.device,
            preview_comp.view(),
            eframe::wgpu::FilterMode::Linear,
        );
        // Text never spills over neighbouring widgets: it ends in "…"
        // (paragraphs opt in to wrapping with `Label::wrap`).
        // Theme (`theme.rs`; re-applied when Preferences › Appearance changes).
        let prefs = prefs::Preferences::load();
        prefs.apply_globals();
        isf_host::reload(&prefs.folders.isf_shaders);
        prefs.ndi.apply_runtime_dir();
        let isf = isf_host::IsfHost::new();
        compositor.set_isf_library(isf.lib.clone());
        preview_comp.set_isf_library(isf.lib.clone());
        let ffgl = ffgl_host::Ffgl::start(&gpu.adapter.get_info().name);
        let scan = ffgl.reload(&prefs.folders.ffgl_plugins);
        // Debug screenshots load plugins at start: wait for the catalogue.
        if std::env::var_os("LAVMFX_SCREENSHOT").is_some() {
            let _ = scan.join();
        }
        compositor.set_external_fx(ffgl.clone());
        preview_comp.set_external_fx(ffgl.clone());
        cc.egui_ctx.set_fonts(theme::fonts());
        theme::apply(&cc.egui_ctx, &prefs.appearance);
        let gpu_summary = lavmfx_render::adapter_summary(&gpu.adapter);
        let native_bc = gpu.device.features().contains(lavmfx_render::WANTED_FEATURES);
        log::info!("GPU: {gpu_summary}");

        let mut app = Self {
            comp: Composition::new("New Composition", 3, 8),
            path: None,
            clock: Clock::default(),
            gpu,
            compositor,
            isf,
            ffgl,
            live_out: crate::live_out::LiveOut::new(),
            rec: Default::default(),
            render_queue: Default::default(),
            comp_tex,
            outputs,
            screens_editor: None,
            monitors_polled: None,
            gpu_summary,
            selected_clip: None,
            selection: BTreeSet::new(),
            clip_drag: None,
            status: String::new(),
            fps: FpsCounter::default(),
            playing: Vec::new(),
            opening: Vec::new(),
            outgoing: Vec::new(),
            selected_layer: None,
            grid_pin: f32::NEG_INFINITY,
            grid_marquee: None,
            grid_origin: egui::Pos2::ZERO,
            selected_group: None,
            reveal: None,
            focus_tab: None,
            rename: None,
            group_bands: Vec::new(),
            grid_view: egui::Rect::NOTHING,
            resume: HashMap::new(),
            launched_column: None,
            thumbs: Thumbs::new(),
            waves: crate::waves::Waves::new(),
            clipboard: Default::default(),
            started: Instant::now(),
            last_tick: Instant::now(),
            last_wall_dt: 0.0,
            last_stats: Instant::now(),
            slot_rects: Vec::new(),
            picker: None,
            pending_drop: None,
            drag: None,
            native_bc,
            dock: dock::preset("Default"),
            view: dock::ViewOptions::default(),
            prefs,
            prefs_win: Default::default(),
            about_open: false,
            audio: Default::default(),
            prefs_dirty: None,
            theme_applied: None,
            test_card_shown: None,
            media: None,
            chase: Default::default(),
            quit_dialog: false,
            quit_ok: false,
            quit_after_save: false,
            saved_key: None,
            saved_job: None,
            layout_saved: String::new(),
            layout_checked: Instant::now(),
            layout_name: None,
            history: Default::default(),
            history_poke: None,
            history_job: None,
            launch_queue: Vec::new(),
            launch_deck: None,
            shortcuts: shortcuts::Shortcuts::load(),
            midi: Default::default(),
            dmx_in: Default::default(),
            smpte: Default::default(),
            learn: None,
            link: None,
            tap_flash: None,
            remote: Default::default(),
            layer_drag: None,
            comp_picker: None,
            save_picker: None,
            import_report: None,
            preview_comp,
            preview_fx: None,
            preview_fx_on: false,
            preview_fx_keep: false,
            preview_tex,
            preview: None,
            preview_opening: None,
            last_dt: 0.0,
            layer_rows: Vec::new(),
        };
        if let Some(l) = dock::load_current() {
            app.apply_layout(l);
        }
        app.layout_saved = dock::to_json(&app.dock, app.view);
        if show_output && !app.outputs.is_on() {
            app.outputs.set_mode(OutputMode::Windowed(0));
        }
        app.mark_saved();
        // Debug: `LAVMFX_SCREENSHOT_TESTCARD=1` starts with Show Test Card on.
        if std::env::var_os("LAVMFX_SCREENSHOT").is_some() && std::env::var_os("LAVMFX_SCREENSHOT_TESTCARD").is_some() {
            app.outputs.test_card = true;
        }
        match open.as_slice() {
            [] => {}
            [p] if !lavmfx_media::is_media_file(p) => app.load(p),
            files => {
                for (li, p) in files.iter().enumerate() {
                    while app.comp.layers.len() <= li {
                        app.comp.add_layer();
                    }
                    app.put_file(p, (li, 0));
                }
                // Debug: `LAVMFX_SCREENSHOT_SOURCE="Lines"` / `LAVMFX_SCREENSHOT_FXCLIP="Invert RGB"` put a
                // source clip / effect clip on new top layers (column 1).
                for (var, effect) in [("LAVMFX_SCREENSHOT_SOURCE", false), ("LAVMFX_SCREENSHOT_FXCLIP", true)] {
                    if let Ok(name) = std::env::var(var) {
                        app.comp.add_layer();
                        let li = app.comp.layers.len() - 1;
                        let ids = &mut app.comp.ids;
                        let clip = &mut app.comp.layers[li].clips[0];
                        let ok = if effect {
                            clip.load_effect_clip(ids, &name)
                        } else {
                            clip.load_source(ids, &name)
                        };
                        if !ok {
                            log::warn!("{var}: unknown {name}");
                        }
                    }
                }
                app.launch_column(0);
                app.select_only((0, 0));
                if std::env::var_os("LAVMFX_SCREENSHOT").is_some() {
                    app.preview_clip(0, 0); // so self-screenshots show the preview monitor
                    // Debug: `LAVMFX_SCREENSHOT_AUDIOFX="LowPass:Frequency=0.3,Flanger:Depth=1"` adds
                    // audio effects (with values) to Layer 1 (`LAVMFX_SCREENSHOT_AUDIOFX_COMP=1`: the composition).
                    if let Ok(list) = std::env::var("LAVMFX_SCREENSHOT_AUDIOFX") {
                        for item in list.split(',') {
                            let mut it = item.split(':');
                            let name = it.next().unwrap_or("").trim();
                            let Some(mut e) = lavmfx_core::AudioEffect::new(&mut app.comp.ids, name) else {
                                log::warn!("LAVMFX_SCREENSHOT_AUDIOFX: unknown {name}");
                                continue;
                            };
                            for kv in it.next().unwrap_or("").split(';').filter(|s| !s.is_empty()) {
                                let (k, v) = kv.split_once('=').unwrap_or((kv, "1"));
                                match e.params.get_mut(k.trim()) {
                                    Some(lavmfx_core::Param::Range(r)) => r.set(v.trim().parse().unwrap_or(r.value)),
                                    Some(lavmfx_core::Param::Bool(b)) => b.value = v.trim() == "1",
                                    _ => log::warn!("LAVMFX_SCREENSHOT_AUDIOFX: {name} has no {k}"),
                                }
                            }
                            // `LAVMFX_SCREENSHOT_AUDIOFX_COMP=1`: on the composition instead.
                            let track = if std::env::var_os("LAVMFX_SCREENSHOT_AUDIOFX_COMP").is_some() {
                                app.comp.audio.as_mut()
                            } else {
                                app.comp.layers.first_mut().and_then(|l| l.audio.as_mut())
                            };
                            if let Some(a) = track {
                                a.effects.push(e);
                            }
                        }
                    }
                    // Debug: `LAVMFX_SCREENSHOT_COMPFX="Shift RGB"` adds composition effects.
                    if let Ok(list) = std::env::var("LAVMFX_SCREENSHOT_COMPFX") {
                        for name in list.split(',') {
                            app.comp.add_effect(FxTarget::Composition, name.trim(), None);
                        }
                    }
                    // Debug: `LAVMFX_SCREENSHOT_AO=input|output|points|masks|inmask|shapes|shapes-input` opens the
                    // Advanced Output editor embedded in the main window, with a
                    // corner-pinned + warped second slice. Use a scratch
                    // XDG_CONFIG_HOME: the setup is saved.
                    // Debug: `LAVMFX_SCREENSHOT_POINTER=1` turns on Output › Show Mouse
                    // Pointer with the output windows embedded (in the screenshot);
                    // with `LAVMFX_SCREENSHOT_WHEEL=x,y` over the Composition Monitor.
                    if std::env::var_os("LAVMFX_SCREENSHOT_POINTER").is_some() {
                        cc.egui_ctx.set_embed_viewports(true);
                        app.outputs.show_pointer = true;
                    }
                    if let Some(v) = std::env::var_os("LAVMFX_SCREENSHOT_AO") {
                        cc.egui_ctx.set_embed_viewports(true);
                        app.open_screens_editor();
                        app.screens_debug(&v.to_string_lossy());
                    }
                    // Debug: `LAVMFX_SCREENSHOT_SLICES=1`: three demo slices (left third,
                    // a tilted quad, a small one) without opening the editor.
                    if std::env::var_os("LAVMFX_SCREENSHOT_SLICES").is_some() {
                        let comp = app.comp.resolution();
                        let (w, h) = (comp.0 as f32, comp.1 as f32);
                        let mut a = lavmfx_core::output::AdvancedOutput::with_default_screen(comp);
                        a.screens[0].slices[0].input = lavmfx_core::output::rect(0.0, 0.0, w / 3.0, h);
                        let i = a.add_slice(0, comp);
                        a.screens[0].slices[i].input = [
                            [w * 0.45, h * 0.1],
                            [w * 0.9, h * 0.2],
                            [w * 0.85, h * 0.6],
                            [w * 0.5, h * 0.5],
                        ];
                        let j = a.add_slice(0, comp);
                        a.screens[0].slices[j].input = lavmfx_core::output::rect(w * 0.55, h * 0.7, w * 0.3, h * 0.2);
                        // Outputs where the inputs are (screen = composition size).
                        for sl in &mut a.screens[0].slices {
                            sl.output = sl.input;
                        }
                        // `LAVMFX_SCREENSHOT_SLICES=layer`: slice 1 reads Layer 1;
                        // `LAVMFX_SCREENSHOT_BYPASS=1` bypasses Layer 1.
                        if std::env::var("LAVMFX_SCREENSHOT_SLICES").is_ok_and(|v| v == "layer") {
                            a.screens[0].slices[0].input_source = lavmfx_core::sources::RouteTarget::Layer(0);
                        }
                        if std::env::var_os("LAVMFX_SCREENSHOT_BYPASS").is_some()
                            && let Some(l) = app.comp.layers.first_mut()
                        {
                            l.bypassed.value = true;
                        }
                        app.outputs.setup = a;
                    }
                    // Debug: `LAVMFX_SCREENSHOT_SOURCES=1` brings the Sources tab to the front.
                    // `=effects` / `=layer` / `=composition`: that tab instead.
                    match std::env::var("LAVMFX_SCREENSHOT_SOURCES").as_deref() {
                        Ok("effects") => dock::focus_tab(&mut app.dock, dock::Tab::Effects),
                        Ok("layer") => dock::focus_tab(&mut app.dock, dock::Tab::Layer),
                        Ok("composition") => dock::focus_tab(&mut app.dock, dock::Tab::Composition),
                        Ok(_) => dock::focus_tab(&mut app.dock, dock::Tab::Sources),
                        Err(_) => {}
                    }
                    // Debug: `LAVMFX_SCREENSHOT_AUDITION="Kaleidoscope"` tries an effect on the preview.
                    if let Ok(name) = std::env::var("LAVMFX_SCREENSHOT_AUDITION") {
                        app.audition_effect(effects_ui::FxDrop {
                            effect: name,
                            preset: None,
                        });
                    }
                    // Debug: `LAVMFX_SCREENSHOT_RECORD=secs` records that long from the start;
                    // `LAVMFX_SCREENSHOT_SNAPSHOT=1` saves a snapshot.
                    if let Some(secs) = std::env::var("LAVMFX_SCREENSHOT_RECORD")
                        .ok()
                        .and_then(|s| s.parse::<f64>().ok())
                    {
                        app.rec.debug_stop = Some(Instant::now() + std::time::Duration::from_secs_f64(secs));
                        app.toggle_recording();
                        log::info!("{}", app.status);
                    }
                    // Debug: `LAVMFX_SCREENSHOT_PREFS=1` opens Preferences (Defaults page).
                    // `LAVMFX_SCREENSHOT_PREFS=audio` / `video` opens the Audio / Video page.
                    if let Some(v) = std::env::var_os("LAVMFX_SCREENSHOT_PREFS") {
                        app.prefs_win.open = true;
                        app.prefs_win.page = if v == "audio" {
                            prefs::Page::Audio
                        } else if v == "video" {
                            prefs::Page::Video
                        } else if v == "dmx" {
                            prefs::Page::Dmx
                        } else {
                            prefs::Page::Defaults
                        };
                    }
                    // Debug: `LAVMFX_SCREENSHOT_FFT=1` drives Layer 1 + comp Master by External FFT,
                    // `=clip` by Layer / Composition FFT (clip audio).
                    if let Some(v) = std::env::var_os("LAVMFX_SCREENSHOT_FFT") {
                        use lavmfx_core::drive::Source;
                        let (l, c) = if v == "clip" {
                            (Source::LayerFft, Source::CompositionFft)
                        } else {
                            (Source::ExternalFft, Source::ExternalFft)
                        };
                        app.comp.layers[0].master.set_source(l);
                        app.comp.master.set_source(c);
                        if let Some(d) = app.comp.master.drive.as_mut() {
                            d.fft.threshold = 0.6; // shows the threshold line
                        }
                        dock::focus_tab(&mut app.dock, dock::Tab::ExternalFft);
                    }
                    // Debug: `LAVMFX_SCREENSHOT_BPM=1` makes clip 1 a BPM Sync clip.
                    if std::env::var_os("LAVMFX_SCREENSHOT_BPM").is_some() {
                        let bpm = app.clock.bpm;
                        app.comp.layers[0].clips[0].set_transport_type(&mut app.comp.ids, 1, bpm);
                    }
                    // Debug: `LAVMFX_SCREENSHOT_SMPTE=3600` makes clip 1 an SMPTE 1 clip
                    // starting at that timecode (seconds) and opens the SMPTE panel.
                    if let Some(off) = std::env::var("LAVMFX_SCREENSHOT_SMPTE")
                        .ok()
                        .and_then(|v| v.parse::<f64>().ok())
                    {
                        let bpm = app.clock.bpm;
                        let clip = &mut app.comp.layers[0].clips[0];
                        clip.set_transport_type(&mut app.comp.ids, 2, bpm);
                        if let Some(lavmfx_core::Transport::Timeline(t)) = clip.transport.as_mut()
                            && let Some(o) = t.controls.smpteoffset.as_mut()
                        {
                            o.set(off);
                        }
                        if !dock::is_open(&app.dock, dock::Tab::Smpte) {
                            dock::toggle_tab(&mut app.dock, dock::Tab::Smpte);
                        }
                        dock::focus_tab(&mut app.dock, dock::Tab::Smpte);
                    }
                    // Debug: `LAVMFX_SCREENSHOT_FX="Kaleidoscope,Hue Rotate"` adds effects to clip 1.
                    if std::env::var_os("LAVMFX_SCREENSHOT_FX").is_some() {
                        // Layer Master: Timeline + triangle envelope (shows the editor).
                        let m = &mut app.comp.layers[0].master;
                        m.set_source(lavmfx_core::drive::Source::Timeline);
                        if let Some(md) = m.drive.as_mut() {
                            let mut e = lavmfx_core::drive::Envelope::default();
                            e.move_point(1, 1.0, 0.0);
                            e.insert(0.5, 1.0);
                            md.envelope = Some(e);
                        }
                    }
                    if let Ok(list) = std::env::var("LAVMFX_SCREENSHOT_FX") {
                        for name in list.split(',') {
                            app.comp.add_effect(FxTarget::Clip(0, 0), name.trim(), None);
                        }
                        // …and drives the clip's Transform › Scale by Timeline (shows the drive UI).
                        if let Some(t) = app.comp.effects_mut(FxTarget::Clip(0, 0)).and_then(|c| c.get_mut(0))
                            && let Some(lavmfx_core::Param::Range(p)) = t.params.get_mut("Scale")
                        {
                            p.set_source(lavmfx_core::drive::Source::Timeline);
                            if let Some(d) = p.drive.as_mut() {
                                (d.from, d.to) = (50.0, 150.0);
                                let mut e = lavmfx_core::drive::Envelope::default();
                                e.move_point(1, 1.0, 0.0);
                                e.insert(0.5, 1.0);
                                d.envelope = Some(e);
                            }
                        }
                    }
                }
                app.selected_layer = Some(0);
                // Debug: `LAVMFX_SCREENSHOT_LAYERFX="Invert RGB"` adds effects to layer 1.
                if std::env::var_os("LAVMFX_SCREENSHOT").is_some()
                    && let Ok(list) = std::env::var("LAVMFX_SCREENSHOT_LAYERFX")
                {
                    for name in list.split(',') {
                        app.comp.add_effect(FxTarget::Layer(0), name.trim(), None);
                    }
                }
                // Debug: `LAVMFX_SCREENSHOT_TOPFX="Radar"` adds effects to the top layer
                // (e.g. the `LAVMFX_SCREENSHOT_SOURCE` clip).
                if std::env::var_os("LAVMFX_SCREENSHOT").is_some()
                    && let Ok(list) = std::env::var("LAVMFX_SCREENSHOT_TOPFX")
                {
                    let top = app.comp.layers.len() - 1;
                    for name in list.split(',') {
                        app.comp.add_effect(FxTarget::Layer(top), name.trim(), None);
                    }
                }
                // Debug: `LAVMFX_SCREENSHOT_RENDER=1` renders clip 1 (after the FX hooks above).
                if std::env::var_os("LAVMFX_SCREENSHOT").is_some()
                    && std::env::var_os("LAVMFX_SCREENSHOT_RENDER").is_some()
                {
                    app.render_selected_clip();
                    log::info!("{}", app.status);
                }
                // …and the source clip is selected (its params in the Clip panel).
                if std::env::var_os("LAVMFX_SCREENSHOT_SOURCE").is_some()
                    && let Some(li) = app.comp.layers.iter().position(|l| {
                        matches!(
                            l.clips[0].source(),
                            Some(lavmfx_core::sources::ClipSource::Generator(_))
                        )
                    })
                {
                    app.select_only((li, 0));
                    app.selected_layer = Some(li);
                    dock::focus_tab(&mut app.dock, dock::Tab::Clip);
                    if std::env::var_os("LAVMFX_SCREENSHOT_PREVIEW_SOURCE").is_some() {
                        app.preview_clip(li, 0);
                    }
                    // `LAVMFX_SCREENSHOT_SOURCEFX="Hue Rotate,Blur"`: effects on the source clip.
                    if let Ok(list) = std::env::var("LAVMFX_SCREENSHOT_SOURCEFX") {
                        for name in list.split(',') {
                            app.comp.add_effect(FxTarget::Clip(li, 0), name.trim(), None);
                        }
                    }
                }
            }
        }
        // Debug: `LAVMFX_SCREENSHOT_CHASE=1` opens the Slice Chase window (embedded);
        // `=demo` also adds a rule and a hand-made sequence over the current slices.
        if std::env::var_os("LAVMFX_SCREENSHOT").is_some()
            && let Some(v) = std::env::var_os("LAVMFX_SCREENSHOT_CHASE")
        {
            cc.egui_ctx.set_embed_viewports(true);
            if v == "demo" {
                app.chase_demo();
            }
            app.open_slice_chase();
        }
        // Debug: `LAVMFX_SCREENSHOT_ABOUT=1` opens Help › About LAVMFX….
        if std::env::var_os("LAVMFX_SCREENSHOT").is_some() && std::env::var_os("LAVMFX_SCREENSHOT_ABOUT").is_some() {
            app.about_open = true;
        }
        // `LAVMFX_SCREENSHOT_CHASE_PARAMS="Sequence=Columns;Trail=2"` sets chase effect params.
        if std::env::var_os("LAVMFX_SCREENSHOT").is_some()
            && let Ok(spec) = std::env::var("LAVMFX_SCREENSHOT_CHASE_PARAMS")
        {
            app.chase_debug_params(&spec);
        }
        // Debug: `LAVMFX_SCREENSHOT_MEDIA=1` opens the Media Manager (embedded).
        if std::env::var_os("LAVMFX_SCREENSHOT").is_some() && std::env::var_os("LAVMFX_SCREENSHOT_MEDIA").is_some() {
            cc.egui_ctx.set_embed_viewports(true);
            app.open_media_manager();
        }
        Ok(app)
    }

    // ------------------------------------------------------------- file ops

    fn default_dir() -> PathBuf {
        crate::app_dirs()
            .map(|d| d.data_dir().join("compositions"))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Save to the current file (or a new one in the default folder); true on success.
    fn save(&mut self) -> bool {
        let path = self.path.clone().unwrap_or_else(|| {
            Self::default_dir().join(format!(
                "{}.{}",
                sanitize(&self.comp.name.value),
                lavmfx_project::EXTENSION
            ))
        });
        let res = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .map_err(Into::into)
            .and_then(|_| lavmfx_project::save(&self.comp, &path));
        let ok = res.is_ok();
        self.status = match res {
            Ok(()) => {
                let s = format!("Saved {}", path.display());
                self.prefs.add_recent(&path);
                self.path = Some(path);
                self.mark_saved();
                s
            }
            Err(e) => format!("Save failed: {e}"),
        };
        log::info!("{}", self.status);
        ok
    }

    /// Open a composition: our `.lavmfx`, or an `.avc` (imported; saving
    /// then writes a new `.lavmfx`, the `.avc` is never touched).
    fn load(&mut self, path: &Path) {
        let avc = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("avc"));
        let result = if avc {
            lavmfx_project::avc::import(path).map(|i| (i.composition, Some(i.report)))
        } else {
            lavmfx_project::load(path).map(|c| (c, None))
        };
        self.status = match result {
            Ok((c, report)) => {
                self.stop_all();
                self.stop_preview();
                self.resume.clear();
                self.comp = c;
                self.history_job = None;
                self.history.reset(&self.comp);
                self.launch_queue.clear();
                self.ensure_layers();
                self.clock.set_bpm(self.comp.tempo_controller.tempo.value);
                self.path = (!avc).then(|| path.to_owned());
                self.prefs.add_recent(path);
                // Files may have moved since the save: check them (before
                // the "saved" fingerprint, so this isn't an unsaved change).
                let found: HashMap<String, bool> = self
                    .comp
                    .media_files()
                    .into_iter()
                    .map(|f| {
                        let e = Path::new(&f.path).is_file();
                        (f.path, e)
                    })
                    .collect();
                self.comp.check_media(&found);
                let missing = found.values().filter(|e| !**e).count();
                // Sequences find this setup's slices (not an unsaved change either).
                self.chase_loaded();
                self.history.reset(&self.comp);
                self.mark_saved();
                if self.media.is_some() {
                    self.media = None;
                    self.open_media_manager();
                }
                self.select_none();
                self.selected_layer = None;
                self.launched_column = None;
                let opened = match report {
                    Some(r) => {
                        let s = format!("Imported {}: {}", file_name(path), r.summary());
                        self.import_report = Some((file_name(path), r));
                        s
                    }
                    None => format!("Opened {}", path.display()),
                };
                if missing > 0 {
                    format!("{opened} · {missing} missing file(s): Composition › Media Manager")
                } else {
                    opened
                }
            }
            Err(e) => format!("Open failed: {e}"),
        };
        log::info!("{}", self.status);
    }

    /// Composition › Open… (`.lavmfx` or `.avc`).
    fn open_composition_dialog(&mut self) {
        let (tx, rx) = mpsc::channel();
        let dir = Self::default_dir();
        std::thread::spawn(move || {
            let file = rfd::FileDialog::new()
                .set_title("Open composition")
                .set_directory(dir)
                .add_filter("Composition", &[lavmfx_project::EXTENSION, "avc"])
                .pick_file();
            let _ = tx.send(file);
        });
        self.comp_picker = Some(rx);
    }

    /// What the quit dialog compares: the undo key (edits only, no play
    /// positions / animated values / selection) plus the tempo, which is saved
    /// but not an undo step.
    fn saved_fingerprint(c: &Composition) -> u64 {
        lavmfx_core::history::undo_key(c) ^ c.tempo_controller.tempo.value.to_bits().rotate_left(17)
    }

    /// Remember the composition as it is now as "saved" (keyed on a worker so
    /// big compositions don't stall Ctrl+S).
    fn mark_saved(&mut self) {
        let snap = self.comp.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Self::saved_fingerprint(&snap));
        });
        self.saved_job = Some(rx);
    }

    /// Changed since the last save / open / new.
    fn is_modified(&mut self) -> bool {
        if let Some(rx) = self.saved_job.take() {
            self.saved_key = rx.recv().ok();
        }
        self.saved_key != Some(Self::saved_fingerprint(&self.comp))
    }

    /// Preferences › General › quit confirmation: closing the window asks first,
    /// but only when the composition has unsaved changes.
    fn quit_confirmation(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested())
            && self.prefs.general.quit_confirmation
            && !self.quit_ok
            && !self.quit_dialog
        {
            if self.is_modified() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.quit_dialog = true;
            } else {
                self.quit_ok = true;
            }
        }
        // Debug: `LAVMFX_SCREENSHOT_QUIT=1` logs the modified state at 3 s and opens the dialog.
        if std::env::var_os("LAVMFX_SCREENSHOT_QUIT").is_some() && std::env::var_os("LAVMFX_SCREENSHOT").is_some() {
            static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if self.started.elapsed().as_secs() >= 3 && !SHOWN.swap(true, std::sync::atomic::Ordering::Relaxed) {
                log::info!("quit probe: modified = {}", self.is_modified());
                self.quit_dialog = true;
            }
        }
        if self.quit_dialog && ctx.input(|i| i.viewport().close_requested()) && !self.quit_ok {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if !self.quit_dialog {
            return;
        }
        let pal = theme::pal_ctx(ctx);
        let name = self.comp.name.value.clone();
        let has_path = self.path.is_some();
        let r = egui::Modal::new(egui::Id::unique("quit-confirm"))
            .frame(
                egui::Frame::new()
                    .fill(pal.panel)
                    .stroke(egui::Stroke::new(1.0, pal.border))
                    .corner_radius(6)
                    .inner_margin(egui::Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.set_width(440.0);
                ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
                ui.label(
                    RichText::new("Save changes before quitting?")
                        .size(19.0)
                        .strong()
                        .color(pal.text_strong),
                );
                ui.add_space(2.0);
                ui.label(
                    RichText::new(format!("\u{201c}{name}\u{201d} has unsaved changes."))
                        .size(14.0)
                        .color(pal.text),
                );
                let note = if has_path {
                    "If you quit without saving, your changes since the last save are lost."
                } else {
                    "This composition has never been saved as a .lavmfx file."
                };
                ui.label(RichText::new(note).size(13.0).color(pal.text_weak));
                ui.add_space(14.0);
                let size = vec2(0.0, 34.0);
                let btn = |text: &str, fill: Color32, fg: Color32| {
                    egui::Button::new(RichText::new(text).size(14.0).color(fg))
                        .fill(fill)
                        .min_size(size + vec2(96.0, 0.0))
                        .corner_radius(4)
                };
                let mut action = None;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    if ui
                        .add(btn("Don\u{2019}t Save", pal.button, pal.danger))
                        .on_hover_text("Quit and lose the unsaved changes")
                        .clicked()
                    {
                        action = Some("discard");
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (label, hover) = if has_path {
                            ("Save", "Save, then quit")
                        } else {
                            ("Save As\u{2026}", "Choose a file, save, then quit")
                        };
                        if ui
                            .add(btn(label, pal.accent, pal.on_accent))
                            .on_hover_text(hover)
                            .clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            action = Some(if has_path { "save" } else { "save_as" });
                        }
                        if has_path
                            && ui
                                .add(btn("Save As\u{2026}", pal.button, pal.text))
                                .on_hover_text("Choose a file, save, then quit")
                                .clicked()
                        {
                            action = Some("save_as");
                        }
                        if ui.add(btn("Cancel", pal.button, pal.text)).clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape))
                        {
                            action = Some("cancel");
                        }
                    });
                });
                action
            });
        let quit = |app: &mut Self| {
            app.quit_ok = true;
            app.quit_dialog = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        };
        match r.inner {
            Some("save") => {
                if self.save() {
                    quit(self);
                } else {
                    self.quit_dialog = false;
                }
            }
            Some("save_as") => {
                self.quit_dialog = false;
                self.quit_after_save = true;
                self.save_as_dialog();
            }
            Some("discard") => quit(self),
            Some(_) => self.quit_dialog = false,
            None => {}
        }
        if r.should_close() {
            self.quit_dialog = false;
        }
    }

    /// Composition › Save As…: pick a file, then save there (it becomes the current file).
    fn save_as_dialog(&mut self) {
        let (tx, rx) = mpsc::channel();
        let dir = self
            .path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(Self::default_dir);
        let _ = std::fs::create_dir_all(&dir);
        let name = format!("{}.{}", sanitize(&self.comp.name.value), lavmfx_project::EXTENSION);
        std::thread::spawn(move || {
            let file = rfd::FileDialog::new()
                .set_title("Save composition as")
                .set_directory(dir)
                .set_file_name(name)
                .add_filter("Composition", &[lavmfx_project::EXTENSION])
                .save_file();
            let _ = tx.send(file);
        });
        self.save_picker = Some(rx);
    }

    fn poll_save_dialog(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.save_picker else { return };
        match rx.try_recv() {
            Ok(file) => {
                self.save_picker = None;
                if let Some(mut f) = file {
                    if f.extension().is_none_or(|e| e != lavmfx_project::EXTENSION) {
                        f.set_extension(lavmfx_project::EXTENSION);
                    }
                    self.path = Some(f);
                    if self.save() && self.quit_after_save {
                        // Quit dialog › Save As…: saved, now quit.
                        self.quit_ok = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                self.quit_after_save = false;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.save_picker = None,
        }
    }

    fn poll_composition_dialog(&mut self) {
        let Some(rx) = &self.comp_picker else { return };
        match rx.try_recv() {
            Ok(file) => {
                self.comp_picker = None;
                if let Some(f) = file {
                    self.load(&f);
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.comp_picker = None,
        }
    }

    /// After an `.avc` import: what came in, what's missing.
    fn import_report_dialog(&mut self, ctx: &egui::Context) {
        let Some((name, r)) = &self.import_report else { return };
        let resp = egui::Modal::new(egui::Id::unique("import-report")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            ui.heading(format!("Imported {name}"));
            ui.label(format!("{} video clips imported.", r.clips));
            for (d, p) in &r.drives {
                ui.label(format!("Drive {d}: found at {}", p.display()));
            }
            if r.bpm_sync > 0 {
                ui.label(format!("{} clips use BPM Sync (loop length in beats kept).", r.bpm_sync));
            }
            if r.sources > 0 {
                ui.label(format!(
                    "{} source (generator) clips imported with their numbers and colours; choices keep their defaults.",
                    r.sources
                ));
            }
            if r.transports > 0 {
                ui.label(format!(
                    "{} clips keep their transport: speed, duration, direction, play mode, \"when triggered again\".",
                    r.transports
                ));
            }
            if r.clip_blends + r.group_speeds > 0 {
                ui.label(format!(
                    "{} clips with their own blend mode; {} groups with a Speed.",
                    r.clip_blends, r.group_speeds
                ));
            }
            if r.spout > 0 {
                let senders: Vec<&str> = r.spout_senders.iter().map(String::as_str).collect();
                ui.label(format!(
                    "{} Spout clips became shared-video clips ({}: they play when an app shares that name): {}.",
                    r.spout,
                    lavmfx_media::share::backend(),
                    senders.join(", ")
                ));
            }
            if r.skipped > 0 {
                ui.label(format!(
                    "{} other clips without a file we can't make yet (unknown sources) were skipped.",
                    r.skipped
                ));
            }
            if r.effects > 0 {
                ui.label(format!(
                    "{} effects imported (clips, layers, groups, composition) with their values, bypass, opacity and blend.",
                    r.effects
                ));
            }
            if r.animations > 0 {
                ui.label(format!(
                    "{} animated params (Timeline, BPM Sync, External FFT, with range and envelope).",
                    r.animations
                ));
            }
            if r.dashboard_knobs + r.dashboard_links > 0 {
                ui.label(format!(
                    "Dashboards: {} knobs (values and names), {} params linked to knobs.",
                    r.dashboard_knobs, r.dashboard_links
                ));
            }
            if r.autopilot_layers > 0 {
                ui.label(format!(
                    "{} layers had Autopilot on in the imported file; it starts Off here (Layer panel › Autopilot). Clip Loops imported for {} clips.",
                    r.autopilot_layers, r.autopilot_loops
                ));
            }
            if !r.unknown_effects.is_empty() {
                ui.label(format!("Unknown effects (skipped): {:?}", r.unknown_effects));
            }
            if !r.unknown_modes.is_empty() {
                ui.label(format!("Unknown modes (set to Alpha): {:?}", r.unknown_modes));
            }
            if !r.missing.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "Warning: {} clips' files were not found. They stay in the grid marked \"missing\" \
                         (drop a video on one to replace it).",
                        r.missing.len()
                    ))
                    .color(theme::cur().danger),
                );
                egui::CollapsingHeader::new("Missing files").show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        let mut v: Vec<&String> = r.missing.iter().collect();
                        v.sort();
                        v.dedup();
                        for m in v {
                            ui.label(RichText::new(m).small().monospace());
                        }
                    });
                });
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new("Save (Ctrl+S) writes a new .lavmfx file; the .avc is left unchanged.")
                    .weak()
                    .small(),
            );
            if ui.button("OK").clicked() {
                ui.close();
            }
        });
        if resp.should_close() {
            self.import_report = None;
        }
    }

    fn new_composition(&mut self) {
        self.stop_all();
        self.resume.clear();
        self.comp = Composition::new("New Composition", 3, 8);
        self.chase_loaded();
        self.history_job = None;
        self.history.reset(&self.comp);
        self.launch_queue.clear();
        self.path = None;
        self.select_none();
        self.mark_saved();
        self.status = "New composition".into();
    }

    // ------------------------------------------------------------ playback

    fn ensure_layers(&mut self) {
        let n = self.comp.layers.len();
        self.playing.resize_with(n.max(self.playing.len()), || None);
        self.opening.resize_with(n.max(self.opening.len()), || None);
        self.outgoing.resize_with(n.max(self.outgoing.len()), || None);
    }

    fn remember(&mut self, pb: &Playback) {
        self.resume.insert(pb.slot.clip, pb.offset());
    }

    /// `old` (the clip leaving layer `li`, or none) fades out over the
    /// layer's transition duration. Returns false when the layer has no
    /// transition (then `old` is just remembered for "Continue").
    fn transition_out(&mut self, li: usize, old: Option<Playback>) -> bool {
        let duration = self.comp.layers.get(li).map_or(0.0, |l| l.transition.duration.value);
        if duration <= 0.0 {
            if let Some(o) = &old {
                self.remember(o);
            }
            return false;
        }
        if old.is_none() && self.outgoing[li].is_some() {
            return true; // keep the running fade
        }
        if let Some(pb) = self.outgoing[li].take().and_then(|o| o.pb) {
            self.remember(&pb);
        }
        self.compositor.begin_transition(&self.gpu.device, li);
        self.outgoing[li] = Some(Outgoing {
            pb: old,
            elapsed: 0.0,
            duration,
        });
        true
    }

    /// Stop layer `li`, fading out over its transition duration.
    fn stop_layer(&mut self, li: usize) {
        self.ensure_layers();
        self.opening[li] = None;
        let old = self.playing[li].take();
        if !self.transition_out(li, old) {
            self.compositor.clear_layer(li);
        }
    }

    /// Stop layer `li` at once, including a running transition
    /// (remembers clip positions for "Continue").
    fn stop_layer_now(&mut self, li: usize) {
        self.ensure_layers();
        self.opening[li] = None;
        for pb in [self.playing[li].take(), self.outgoing[li].take().and_then(|o| o.pb)]
            .into_iter()
            .flatten()
        {
            self.remember(&pb);
        }
        self.compositor.clear_layer(li);
    }

    fn stop_all(&mut self) {
        for li in 0..self.playing.len().max(self.opening.len()) {
            self.stop_layer_now(li);
        }
        self.compositor.clear_video();
        self.launched_column = None;
    }

    /// Trigger the clip at (layer, column) of the active deck, on its Beat
    /// Snap boundary (Composition / Clip › Beat Snap) or now.
    fn trigger(&mut self, li: usize, ci: usize) {
        let Some(clip) = self.comp.layers.get(li).and_then(|l| l.clips.get(ci)).map(|c| c.id) else {
            return;
        };
        if !self.queue_launch(live::Launch::Clip {
            layer: li,
            column: ci,
            clip,
        }) {
            self.trigger_now(li, ci);
        }
    }

    /// Trigger the clip at (layer, column) of the active deck on its layer.
    /// Re-triggering the playing clip restarts it.
    fn trigger_now(&mut self, li: usize, ci: usize) {
        let deck = self.comp.active_deck();
        let Some(clip) = self.comp.layers.get(li).and_then(|l| l.clips.get(ci)) else {
            return;
        };
        if clip.source().is_some() {
            self.trigger_without_file(li, ci);
            return;
        }
        let Some(path) = clip.video_path() else { return };
        if clip.is_missing() {
            self.status = format!("Missing file: {path}");
            return;
        }
        let slot = Slot {
            deck,
            layer: li,
            col: ci,
            clip: clip.id,
        };
        let mut settings = TransportSettings::for_clip(clip, self.clock.bpm);
        settings.speed *= self.comp.layer_speed(li);
        let path = PathBuf::from(path);
        self.ensure_layers();
        if let Some(pb) = &mut self.playing[li]
            && pb.slot == slot
        {
            pb.scrub_to(0.0);
            pb.played = 0.0;
            self.opening[li] = None;
            return;
        }
        let start = if settings.continue_away {
            self.resume.get(&slot.clip).copied().unwrap_or(0.0)
        } else {
            0.0
        };
        self.status = format!("Opening {}…", file_name(&path));
        self.opening[li] = Some(Opening::spawn(path, slot, start, self.native_bc));
    }

    /// Start a source / effect clip: no decoder to open, it plays at once.
    fn trigger_without_file(&mut self, li: usize, ci: usize) {
        let deck = self.comp.active_deck();
        let Some(clip) = self.comp.layers.get(li).and_then(|l| l.clips.get(ci)) else {
            return;
        };
        let slot = Slot {
            deck,
            layer: li,
            col: ci,
            clip: clip.id,
        };
        let mut settings = TransportSettings::for_clip(clip, self.clock.bpm);
        settings.speed *= self.comp.layer_speed(li);
        let name = clip.name.value.clone();
        self.ensure_layers();
        self.opening[li] = None;
        if let Some(pb) = &mut self.playing[li]
            && pb.slot == slot
        {
            pb.scrub_to(0.0);
            pb.played = 0.0;
            return;
        }
        let start = if settings.continue_away {
            self.resume.get(&slot.clip).copied().unwrap_or(0.0)
        } else {
            0.0
        };
        let old = self.playing[li].take();
        self.transition_out(li, old);
        let mut pb = Playback::without_file(slot, lavmfx_core::sources::VIRTUAL_SECONDS, start);
        pb.settings = settings;
        pb.live_src = clip_live_source(&self.comp, slot);
        if let Some(e) = pb.connect_live() {
            self.status = format!("{name}: {e}");
        }
        self.playing[li] = Some(pb);
        self.status = format!("Layer {}: {name}", li + 1);
    }

    /// Launch column `ci`: every layer plays its clip in that column; layers
    /// with an empty slot there stop. Layers/clips set to ignore column
    /// triggers are left alone.
    fn launch_column(&mut self, ci: usize) {
        if !self.queue_launch(live::Launch::Column(ci)) {
            self.launch_column_now(ci);
        }
    }

    fn launch_column_now(&mut self, ci: usize) {
        for li in 0..self.comp.layers.len() {
            let layer = &self.comp.layers[li];
            if layer.ignorecolumntrigger.value {
                continue;
            }
            match layer.clips.get(ci) {
                Some(c) if c.ignorecolumntrigger.value == "On" => {}
                Some(c) if !c.is_empty() => self.trigger(li, ci),
                _ => self.stop_layer(li),
            }
        }
        self.launched_column = Some((self.comp.active_deck(), ci));
    }

    fn select_deck(&mut self, i: usize) {
        if self.comp.select_deck(i) {
            self.select_none();
        }
    }

    /// Remove the media from slot (layer, column) of the active deck.
    fn eject(&mut self, li: usize, ci: usize) {
        let deck = self.comp.active_deck();
        if self
            .playing
            .get(li)
            .and_then(Option::as_ref)
            .is_some_and(|p| (p.slot.deck, p.slot.col) == (deck, ci))
        {
            self.stop_layer_now(li);
        }
        let ids = &mut self.comp.ids;
        if let Some(c) = self.comp.layers.get_mut(li).and_then(|l| l.clips.get_mut(ci)) {
            self.resume.remove(&c.id);
            *c = lavmfx_core::Clip::empty(ids);
        }
    }

    /// Set the playdirection of the clip playing on layer `li` (0 back, 1 pause, 2 forward).
    fn set_direction(&mut self, li: usize, dir: usize) {
        let Some(pb) = self.playing.get(li).and_then(Option::as_ref) else {
            return;
        };
        let s = pb.slot;
        if let Some(t) = self
            .comp
            .clip_mut(s.deck, s.layer, s.col)
            .and_then(|c| c.transport.as_mut())
        {
            transport_controls_mut(t).0.select(dir);
        }
    }

    fn step(&mut self, li: usize, n: i32) {
        self.set_direction(li, 1);
        if let Some(pb) = self.playing.get_mut(li).and_then(Option::as_mut) {
            pb.settings.direction = lavmfx_core::transport::Direction::Pause;
            pb.step(n);
        }
    }

    fn select_none(&mut self) {
        self.selected_clip = None;
        self.selection.clear();
    }

    fn select_only(&mut self, slot: (usize, usize)) {
        self.selected_clip = Some(slot);
        self.selected_layer = Some(slot.0);
        self.selection = BTreeSet::from([slot]);
    }

    /// Name-strip click: plain = select one, Ctrl = toggle, Shift = range
    /// (rectangle) from the focused slot.
    fn click_select(&mut self, slot: (usize, usize), mods: egui::Modifiers) {
        if mods.command {
            if !self.selection.remove(&slot) {
                self.selection.insert(slot);
                self.selected_clip = Some(slot);
            } else if self.selected_clip == Some(slot) {
                self.selected_clip = self.selection.iter().next().copied();
            }
        } else if let (true, Some(a)) = (mods.shift, self.selected_clip) {
            let (l0, l1) = (a.0.min(slot.0), a.0.max(slot.0));
            let (c0, c1) = (a.1.min(slot.1), a.1.max(slot.1));
            self.selection = (l0..=l1).flat_map(|l| (c0..=c1).map(move |c| (l, c))).collect();
        } else {
            self.select_only(slot);
        }
    }

    /// Slots a drag from `from` would move (the selection if `from` is in it).
    fn drag_sources(&self, from: (usize, usize)) -> Vec<(usize, usize)> {
        if self.selection.contains(&from) && self.selection.len() > 1 {
            self.selection.iter().copied().collect()
        } else {
            vec![from]
        }
    }

    /// Where `sources` land when `from` is dropped at `at`; `None` if a
    /// clip would fall outside the layers or left of column 1.
    fn drop_targets(
        &self,
        sources: &[(usize, usize)],
        from: (usize, usize),
        at: DropAt,
    ) -> Option<Vec<(usize, usize)>> {
        let n = self.comp.layers.len() as isize;
        // Between slots, the group's leftmost clip lands at the insert index.
        let (to, anchor_col) = match at {
            DropAt::On(l, c) => ((l, c), from.1),
            DropAt::Between(l, k) => ((l, k), sources.iter().map(|s| s.1).min().unwrap_or(from.1)),
        };
        let (dl, dc) = (to.0 as isize - from.0 as isize, to.1 as isize - anchor_col as isize);
        sources
            .iter()
            .map(|&(l, c)| {
                let (l, c) = (l as isize + dl, c as isize + dc);
                (l >= 0 && l < n && c >= 0).then_some((l as usize, c as usize))
            })
            .collect()
    }

    /// Drop of a clip drag. On a slot: the clips overwrite the targets.
    /// Between slots: in each target layer, slots from the insert point on
    /// shift right by the group's width first. Move leaves the sources
    /// empty; copy (Alt or Ctrl) duplicates with fresh ids.
    fn move_clips(&mut self, from: (usize, usize), at: DropAt, copy: bool) {
        let sources = self.drag_sources(from);
        if !copy {
            let noop = match at {
                DropAt::On(l, c) => (l, c) == from,
                // A single clip dropped right beside itself.
                DropAt::Between(l, k) => sources.len() == 1 && l == from.0 && (k == from.1 || k == from.1 + 1),
            };
            if noop {
                return;
            }
        }
        let Some(targets) = self.drop_targets(&sources, from, at) else {
            self.status = "Clips don't fit there".into();
            return;
        };
        let insert = match at {
            DropAt::Between(_, k) => Some(k),
            DropAt::On(..) => None,
        };
        self.comp.place_clips(&sources, &targets, insert, copy);
        // Playing/opening clips follow their clip within the layer (an
        // overwritten or cross-layer-moved playing clip stops on the next tick).
        let deck = self.comp.active_deck();
        for li in 0..self.playing.len() {
            let find = |id: Id| {
                self.comp
                    .layers
                    .get(li)
                    .and_then(|l| l.clips.iter().position(|c| c.id == id))
            };
            if let Some(pb) = self.playing[li].as_mut().filter(|p| p.slot.deck == deck)
                && let Some(c) = find(pb.slot.clip)
            {
                pb.slot.col = c;
            }
            if let Some(op) = self.opening[li].as_mut().filter(|o| o.slot.deck == deck)
                && let Some(c) = find(op.slot.clip)
            {
                op.slot.col = c;
            }
        }
        let map = |s: (usize, usize)| sources.iter().position(|x| *x == s).map(|i| targets[i]);
        self.selected_clip = self.selected_clip.and_then(map).or(targets.first().copied());
        self.selection = targets.iter().copied().collect();
        let n = targets.len();
        self.status = format!(
            "{} {n} clip{}",
            if copy { "Copied" } else { "Moved" },
            if n == 1 { "" } else { "s" }
        );
    }

    /// Where a clip drag at `p` would drop: on a slot, or between two slots
    /// (the gap and the outer ~15 % of each slot's width).
    fn drop_at(&self, p: egui::Pos2, gap: f32) -> Option<DropAt> {
        let edge = CLIP_W * 0.15;
        if p.x < self.grid_pin {
            return None;
        }
        self.slot_rects.iter().find_map(|(r, (l, c))| {
            let zone = r.expand2(vec2(gap / 2.0, 0.0));
            if !zone.contains(p) {
                return None;
            }
            Some(if p.x < r.left() + edge {
                DropAt::Between(*l, *c)
            } else if p.x > r.right() - edge {
                DropAt::Between(*l, c + 1)
            } else {
                DropAt::On(*l, *c)
            })
        })
    }

    /// Eject every selected slot.
    fn eject_selection(&mut self) {
        let sel: Vec<_> = self.selection.iter().copied().collect();
        for (li, ci) in sel {
            self.eject(li, ci);
        }
    }

    /// Layer whose playback the transport keys act on: the selected slot's.
    fn focus_layer(&self) -> Option<usize> {
        self.selected_clip.map(|(li, _)| li)
    }

    /// Put a video file into slot (layer, column) of the active deck,
    /// adding columns as needed. Returns false if it can't be opened.
    fn put_file(&mut self, path: &Path, (li, ci): (usize, usize)) -> bool {
        if !lavmfx_media::is_media_file(path) && path.extension().is_some() {
            self.status = format!("Not a video or audio file: {}", path.display());
            return false;
        }
        let info = match lavmfx_media::probe(path) {
            Ok(i) => i,
            Err(e) => {
                self.status = format!("Can't open: {e}");
                return false;
            }
        };
        while ci >= self.comp.columns.len() {
            self.comp.add_column();
        }
        let Some(layer) = self.comp.layers.get_mut(li) else {
            return false;
        };
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        layer.clips[ci].load_video(&mut self.comp.ids, &stem, playback::file_info(&info));
        // Preferences › Defaults (transport, play mode, direction).
        self.prefs
            .apply_clip_defaults(&mut layer.clips[ci], &mut self.comp.ids, self.clock.bpm);
        true
    }

    /// Load files into consecutive slots of `start`'s layer and play the first.
    fn load_files(&mut self, paths: &[PathBuf], (li, mut ci): (usize, usize)) {
        let mut first = None;
        for path in paths {
            if self.put_file(path, (li, ci)) {
                first.get_or_insert((li, ci));
                ci += 1;
            }
        }
        if let Some((li, ci)) = first {
            self.select_only((li, ci));
            self.trigger(li, ci);
        }
    }

    /// Where loaded files go: the selected slot, else the first empty slot of the bottom layer.
    fn default_target(&self) -> (usize, usize) {
        self.selected_clip.unwrap_or_else(|| {
            let ci = self
                .comp
                .layers
                .first()
                .and_then(|l| l.clips.iter().position(|c| c.is_empty()));
            (0, ci.unwrap_or(self.comp.columns.len()))
        })
    }

    fn open_dialog(&mut self) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let files = rfd::FileDialog::new()
                .set_title("Open video or audio")
                .add_filter(
                    "Video and audio",
                    &[lavmfx_media::VIDEO_EXTENSIONS, lavmfx_media::AUDIO_EXTENSIONS].concat(),
                )
                .add_filter("Video", lavmfx_media::VIDEO_EXTENSIONS)
                .add_filter("Audio", lavmfx_media::AUDIO_EXTENSIONS)
                .pick_files()
                .unwrap_or_default();
            let _ = tx.send(files);
        });
        self.picker = Some(rx);
    }

    fn poll_dialog(&mut self) {
        let Some(rx) = &self.picker else { return };
        match rx.try_recv() {
            Ok(files) => {
                self.picker = None;
                let target = self.default_target();
                self.load_files(&files, target);
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.picker = None,
        }
    }

    /// Swap in clips that finished opening.
    fn poll_openings(&mut self) {
        for li in 0..self.opening.len() {
            let Some(r) = self.opening[li].as_ref().and_then(Opening::poll) else {
                continue;
            };
            let op = self.opening[li].take().unwrap();
            match r {
                Ok(player) => {
                    let old = self.playing[li].take();
                    self.transition_out(li, old);
                    let i = player.info();
                    self.status = if lavmfx_media::is_audio_file(&op.path) {
                        format!("Layer {}: {} · audio {}", li + 1, file_name(&op.path), i.codec)
                    } else {
                        format!(
                            "Layer {}: {} · {}x{} {} · {}",
                            li + 1,
                            file_name(&op.path),
                            i.width,
                            i.height,
                            i.codec,
                            i.decode_path
                        )
                    };
                    self.playing[li] = Some(Playback::new(player, op.slot, op.start));
                }
                Err(e) => self.status = format!("Can't play {}: {e}", file_name(&op.path)),
            }
            log::info!("{}", self.status);
        }
    }

    /// Advance every playing clip and upload new frames.
    fn tick_playback(&mut self) {
        let now = Instant::now();
        let wall = now.duration_since(self.last_tick).as_secs_f64().min(0.25);
        self.last_wall_dt = wall;
        let dt = wall * self.comp.speed.value;
        self.last_dt = dt;
        self.last_tick = now;
        self.tick_transitions(dt);
        let log_stats = now.duration_since(self.last_stats).as_secs() >= 5;
        if log_stats {
            self.last_stats = now;
        }
        let bpm = self.clock.bpm;
        for li in 0..self.playing.len() {
            let Some(pb) = &mut self.playing[li] else { continue };
            pb.connect_live(); // live clips: reconnect until the source is there
            let s = pb.slot;
            // Settings follow the clip's params; a clip that was ejected stops.
            let ended = match self.comp.clip(s.deck, s.layer, s.col) {
                Some(c) if c.id == s.clip => {
                    pb.settings = TransportSettings::for_clip(c, bpm);
                    pb.settings.speed *= self.comp.layer_speed(li);
                    pb.smpte_time = smpte::clip_time(&self.smpte.now, &pb.settings);
                    match pb.tick(dt) {
                        Tick::Frame(f) => {
                            self.compositor
                                .set_layer_frame(&self.gpu.device, &self.gpu.queue, li, &f);
                            false
                        }
                        Tick::Nothing => false,
                        Tick::Ended => true,
                    }
                }
                _ => true,
            };
            if ended {
                self.resume.remove(&s.clip);
                self.playing[li] = None;
                self.compositor.clear_layer(li);
                continue;
            }
            if log_stats {
                log::debug!(
                    "layer {} · video {:.1} fps · pos {:.2}s",
                    li + 1,
                    pb.shown_fps(),
                    pb.offset()
                );
            }
            // Mirror the position into the clip's transport param (REST/OSC later).
            let offset_ms = pb.offset() * 1000.0;
            if let Some(t) = self
                .comp
                .clip_mut(s.deck, s.layer, s.col)
                .and_then(|c| c.transport.as_mut())
            {
                transport_position_mut(t).value = offset_ms;
            }
        }
    }

    /// Playing (and outgoing) clips → mixer voices. Clips whose file turns
    /// out to have audio get an audio track (volume / pan in the clip panel).
    fn tick_audio(&mut self) {
        use lavmfx_audio::mixer::Bus;
        use lavmfx_core::audiofx::chain;
        use lavmfx_core::transport::PlayMode;
        let mix = self.comp.audio_mix();
        // Buses: composition, groups, layers (parents first).
        let fx_of = |t: Option<&lavmfx_core::AudioTrack>| t.map(|t| chain(&t.effects)).unwrap_or_default();
        let mut buses = vec![Bus {
            key: u64::MAX,
            parent: None,
            gain: mix.first().map_or(1.0, |m| m.comp),
            pan: 0.0,
            fx: fx_of(self.comp.audio.as_ref()),
        }];
        for (g, gm) in self.comp.layergroups.iter().zip(self.comp.group_audio_mix()) {
            buses.push(Bus {
                key: g.id as u64,
                parent: Some(0),
                gain: gm.gain,
                pan: gm.pan,
                fx: fx_of(g.audio.as_ref()),
            });
        }
        let first_layer = buses.len();
        for (l, m) in self.comp.layers.iter().zip(&mix) {
            buses.push(Bus {
                key: l.id as u64,
                parent: Some(m.group.map_or(0, |g| g + 1)),
                gain: m.stage,
                pan: m.stage_pan,
                fx: fx_of(l.audio.as_ref()),
            });
        }
        let mut found: Vec<(Slot, PathBuf)> = Vec::new();
        let mut wants: Vec<crate::audio::Want> = Vec::new();
        for li in 0..self.playing.len() {
            let Some(m) = mix.get(li).copied() else { continue };
            let progress = self.outgoing.get(li).and_then(Option::as_ref).map(|o| o.progress());
            let parts = [
                (self.playing[li].as_ref(), progress.unwrap_or(1.0), 0u64),
                (
                    self.outgoing
                        .get(li)
                        .and_then(Option::as_ref)
                        .and_then(|o| o.pb.as_ref()),
                    1.0 - progress.unwrap_or(1.0),
                    1u64 << 62,
                ),
            ];
            for (pb, fade, tag) in parts {
                let Some(pb) = pb else { continue };
                let s = pb.slot;
                let Some(c) = self.comp.clip(s.deck, s.layer, s.col).filter(|c| c.id == s.clip) else {
                    continue;
                };
                let Some(path) = c.video_path().map(PathBuf::from) else {
                    continue;
                };
                let (cv, cp) = c.audio.as_ref().map_or((1.0, 0.0), |a| {
                    (
                        lavmfx_core::mix::db_gain(a.track.volume.value),
                        a.track.pan.value as f32,
                    )
                });
                let fx = fx_of(c.audio.as_ref().map(|a| &a.track));
                if c.audio.is_none() && tag == 0 {
                    found.push((s, path.clone()));
                }
                wants.push(crate::audio::Want {
                    key: (s.clip as u64) ^ tag,
                    path,
                    layer: li,
                    pos: pb.offset(),
                    duration: pb.duration(),
                    random: pb.settings.mode == PlayMode::Random,
                    gains: [cv * fade, cv * fade, m.layer * cv * fade],
                    pan: cp,
                    bus: first_layer + li,
                    fx,
                });
            }
        }
        self.audio.play(&wants, buses, self.last_tick, self.last_wall_dt);
        for (s, path) in found {
            if self.audio.has_audio(&path) == Some(true) {
                let ids = &mut self.comp.ids;
                let track = lavmfx_core::AudioTrack::new(ids);
                if let Some(c) = self.comp.clip_mut(s.deck, s.layer, s.col) {
                    c.audio = Some(lavmfx_core::AudioTrackClip {
                        track,
                        description: None,
                        fileinfo: None,
                    });
                }
            }
        }
    }

    /// Advance running transitions: outgoing clips keep playing (with the
    /// transport settings they had) until the transition is over.
    fn tick_transitions(&mut self, dt: f64) {
        for li in 0..self.outgoing.len() {
            let Some(o) = &mut self.outgoing[li] else { continue };
            o.elapsed += dt;
            if o.elapsed >= o.duration {
                if let Some(pb) = self.outgoing[li].take().and_then(|o| o.pb) {
                    self.remember(&pb);
                }
                self.compositor.end_transition(li);
                continue;
            }
            if let Some(pb) = &mut o.pb {
                match pb.tick(dt) {
                    Tick::Frame(f) => self
                        .compositor
                        .set_outgoing_frame(&self.gpu.device, &self.gpu.queue, li, &f),
                    Tick::Ended => o.pb = None, // its last frame stays while fading
                    Tick::Nothing => {}
                }
            }
        }
    }

    /// Video opacity of the clip playing on layer `li` (1 when none).
    fn clip_opacity(&self, li: usize) -> f32 {
        let Some(s) = self.playing.get(li).and_then(Option::as_ref).map(|p| p.slot) else {
            return 1.0;
        };
        self.comp
            .clip(s.deck, s.layer, s.col)
            .and_then(|c| c.video.as_ref())
            .map_or(1.0, |v| v.track.opacity.value as f32)
    }

    /// Bypassed layers (also via their group) and groups: slices with them as
    /// Input Source go black; Video Routers still read them.
    fn bypassed_routes(&self) -> Vec<lavmfx_core::sources::RouteTarget> {
        use lavmfx_core::sources::RouteTarget;
        let group_off = |g: usize| self.comp.layergroups.get(g).is_some_and(|g| g.bypassed.value);
        let layers = self.comp.layers.iter().enumerate().filter_map(|(li, l)| {
            (l.bypassed.value || self.comp.group_of(li).is_some_and(group_off)).then_some(RouteTarget::Layer(li))
        });
        let groups = (0..self.comp.layergroups.len())
            .filter(|&g| group_off(g))
            .map(RouteTarget::Group);
        layers.chain(groups).collect()
    }

    /// The topmost visible layer playing a Slice Outline source, with its
    /// opacity: the outputs lay its picture over the screen; it isn't mixed
    /// into the composition.
    fn slice_outline(&self) -> Option<(usize, f32)> {
        use lavmfx_core::sources::ClipSource;
        let draws = self.layer_draws();
        (0..self.playing.len()).rev().find_map(|li| {
            let s = self.playing[li].as_ref()?.slot;
            let v = self.comp.clip(s.deck, s.layer, s.col)?.video.as_ref()?;
            let Some(ClipSource::Generator(name)) = &v.source else {
                return None;
            };
            let opacity = draws.get(li).map_or(0.0, |d| d.opacity);
            (name == lavmfx_core::sources::SLICE_OUTLINE && opacity > 0.0).then_some((li, opacity))
        })
    }

    /// Direction index (0 back, 1 pause, 2 forward) of the clip playing on layer `li`.
    fn playing_direction(&self, li: usize) -> Option<i32> {
        let s = self.playing.get(li)?.as_ref()?.slot;
        let t = self.comp.clip(s.deck, s.layer, s.col)?.transport.as_ref()?;
        Some(match t {
            lavmfx_core::Transport::Timeline(t) => t.controls.playdirection.index,
            lavmfx_core::Transport::BpmSync(t) => t.controls.playdirection.index,
        })
    }

    // ------------------------------------------------------------ layouts

    /// Write the layout to `layout.json` when it changed (checked once a second).
    fn autosave_layout(&mut self) {
        if self.layout_checked.elapsed().as_secs_f32() < 1.0 {
            return;
        }
        self.layout_checked = Instant::now();
        let json = dock::to_json(&self.dock, self.view);
        if json != self.layout_saved {
            dock::save_current(&json);
            self.layout_saved = json;
        }
    }

    fn apply_layout(&mut self, l: dock::LayoutFile) {
        self.dock = l.dock;
        self.view = l.view;
        dock::add_preview(&mut self.dock);
        dock::add_group_tab(&mut self.dock);
        dock::add_effects_tab(&mut self.dock);
        dock::add_sources_tab(&mut self.dock);
        dock::add_slices_tab(&mut self.dock);
        dock::add_external_fft_tab(&mut self.dock);
    }

    /// The Effects browser tab: drag effects onto panels / clips;
    /// double-click tries one on the Preview Monitor.
    fn effects_browser(&mut self, ui: &mut egui::Ui) {
        let on = self
            .preview_fx
            .as_ref()
            .filter(|_| self.preview_fx_on)
            .map(|p| p.0.clone());
        let (picked, clicked) = effects_ui::browser(ui, on.as_ref());
        if clicked.is_some() && clicked == on {
            self.preview_fx_keep = true;
        }
        if let Some(d) = picked {
            self.audition_effect(d);
        }
    }

    /// Show `d` over the Preview Monitor (starting a preview of the selected
    /// clip when nothing is previewed).
    fn audition_effect(&mut self, d: effects_ui::FxDrop) {
        let Some(fx) = effects_ui::make_effect(&mut self.comp.ids, &d) else {
            return;
        };
        let name = fx.display_name.clone();
        self.preview_fx = Some((d, fx));
        self.preview_fx_on = true;
        self.preview_fx_keep = true;
        if self.preview.is_none() && self.preview_opening.is_none() {
            match self.selected_clip {
                Some((l, c))
                    if self
                        .comp
                        .layers
                        .get(l)
                        .and_then(|x| x.clips.get(c))
                        .is_some_and(|c| !c.is_empty()) =>
                {
                    self.preview_clip(l, c);
                }
                _ => {
                    self.status =
                        format!("{name}: preview a clip (click its thumbnail) to see it on the Preview Monitor");
                    return;
                }
            }
        }
        self.status = format!("Trying {name} on the Preview Monitor (click elsewhere to switch it off)");
    }

    /// The Sources browser tab; double-click loads the source into the selected slot (or the
    /// first empty slot of the bottom layer) and plays it.
    fn sources_browser(&mut self, ui: &mut egui::Ui) {
        if let Some(name) = effects_ui::sources_browser(ui) {
            let (li, ci) = self.default_target();
            while ci >= self.comp.columns.len() {
                self.comp.add_column();
            }
            self.ensure_layers();
            if li < self.comp.layers.len() {
                if !self.comp.layers[li].clips[ci].is_empty() {
                    self.eject(li, ci);
                }
                if self.comp.layers[li].clips[ci].load_source(&mut self.comp.ids, &name) {
                    self.select_only((li, ci));
                    self.trigger(li, ci);
                }
            }
        }
    }

    /// Effects dragged (from the browser, or moved from a chain; Alt/Ctrl = copy) and released
    /// over the grid: a filled clip slot → that clip, a layer's header → the layer, a group
    /// band → the group.
    fn effect_drop_on_grid(&mut self, ctx: &egui::Context) {
        let moving = egui::DragAndDrop::payload::<effects_ui::FxMove>(ctx);
        let source = egui::DragAndDrop::payload::<effects_ui::SourceDrop>(ctx);
        if moving.is_none() && source.is_none() && !egui::DragAndDrop::has_payload_of_type::<effects_ui::FxDrop>(ctx) {
            return;
        }
        let Some(pos) = ctx.pointer_latest_pos().filter(|p| self.grid_view.contains(*p)) else {
            return;
        };
        let filled = |li: usize, ci: usize| {
            self.comp
                .layers
                .get(li)
                .and_then(|l| l.clips.get(ci))
                .is_some_and(|c| c.video.is_some())
        };
        let slot = self
            .slot_rects
            .iter()
            .find(|(r, _)| pos.x >= self.grid_pin && r.contains(pos))
            .map(|(r, s)| (*r, *s));
        // Sources go into any slot (replacing its clip); effects into an empty slot make an
        // effect clip.
        if let Some((r, (li, ci))) = slot
            && (source.is_some() || !filled(li, ci))
        {
            self.drop_into_slot(ctx, r, (li, ci), moving.is_some());
            return;
        }
        let hit = match slot {
            Some((r, (li, ci))) => filled(li, ci).then_some((r, FxTarget::Clip(li, ci))),
            None => {
                // Layer header = the row left of its first slot.
                // Layer header = the pinned column left of the slots.
                let layer = self.layer_rows.iter().find(|(r, _)| r.contains(pos)).map(|(r, li)| {
                    let left = self.grid_view.left().max(r.left());
                    (
                        egui::Rect::from_min_max(egui::pos2(left, r.top()), egui::pos2(self.grid_pin, r.bottom())),
                        FxTarget::Layer(*li),
                    )
                });
                layer.filter(|(r, _)| r.contains(pos)).or_else(|| {
                    self.group_bands
                        .iter()
                        .find(|(r, _)| r.contains(pos))
                        .and_then(|(r, li)| self.comp.group_of(*li).map(|g| (*r, FxTarget::Group(g))))
                })
            }
        };
        let Some((rect, target)) = hit else { return };
        let copy = moving.is_some() && effects_ui::copy_held(ctx);
        let colour = if copy { theme::cur().accent } else { theme::cur().drop };
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("fx-grid-drop"),
        ))
        .rect_stroke(rect, 2.0, egui::Stroke::new(2.0, colour), egui::StrokeKind::Inside);
        if !ctx.input(|i| i.pointer.any_released()) {
            return;
        }
        let what = match target {
            FxTarget::Clip(li, ci) => format!("layer {} · clip {}", li + 1, ci + 1),
            FxTarget::Layer(li) => format!("layer {}", li + 1),
            FxTarget::Group(g) => format!("group {}", g + 1),
            FxTarget::Composition => "the composition".into(),
        };
        let end = self.comp.effects(target).map_or(0, Vec::len);
        if let Some(m) = effects_ui::take::<effects_ui::FxMove>(ctx) {
            if self.comp.transfer_effect(m.from, m.index, target, end, copy).is_some() {
                let verb = if copy { "Copied" } else { "Moved" };
                self.status = format!("{verb} {} to {what}", m.name);
            }
        } else if let Some(d) = effects_ui::take::<effects_ui::FxDrop>(ctx)
            && effects_ui::apply_drop(&mut self.comp, target, &d, None)
        {
            self.status = format!("Added {} to {what}", d.effect);
        }
    }

    /// A source, or an effect onto an empty slot: a new source / effect clip there.
    fn drop_into_slot(&mut self, ctx: &egui::Context, rect: egui::Rect, (li, ci): (usize, usize), moving: bool) {
        let copy = moving && effects_ui::copy_held(ctx);
        let colour = if copy { theme::cur().accent } else { theme::cur().drop };
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("fx-slot-new"),
        ))
        .rect_stroke(rect, 2.0, egui::Stroke::new(2.0, colour), egui::StrokeKind::Inside);
        if !ctx.input(|i| i.pointer.any_released()) {
            return;
        }
        let made = if let Some(s) = effects_ui::take::<effects_ui::SourceDrop>(ctx) {
            if !self.comp.layers[li].clips[ci].is_empty() {
                self.eject(li, ci);
            }
            let ok = match s.name.strip_prefix(render_ui::FILE_PREFIX) {
                Some(path) => self.put_file(Path::new(path), (li, ci)),
                None => self.comp.layers[li].clips[ci].load_source(&mut self.comp.ids, &s.name),
            };
            ok.then(|| format!("Source {}", s.name))
        } else if let Some(m) = effects_ui::take::<effects_ui::FxMove>(ctx) {
            let ids = &mut self.comp.ids;
            self.comp.layers[li].clips[ci].make_effect_clip(ids, &m.name);
            self.comp
                .transfer_effect(m.from, m.index, FxTarget::Clip(li, ci), usize::MAX, copy)
                .map(|_| format!("Effect clip {}", m.name))
        } else if let Some(d) = effects_ui::take::<effects_ui::FxDrop>(ctx) {
            let ids = &mut self.comp.ids;
            let ok = self.comp.layers[li].clips[ci].load_effect_clip(ids, &d.effect);
            if ok && d.preset.is_some() {
                // Recall the preset on the new clip's effect.
                let clip = &mut self.comp.layers[li].clips[ci];
                if let Some(v) = clip.video.as_mut() {
                    v.track.effects.pop();
                }
                effects_ui::apply_drop(&mut self.comp, FxTarget::Clip(li, ci), &d, None);
            }
            ok.then(|| format!("Effect clip {}", self.comp.layers[li].clips[ci].name.value))
        } else {
            None
        };
        if let Some(what) = made {
            self.status = format!("{what} in layer {} · clip {}", li + 1, ci + 1);
            self.select_only((li, ci));
        }
    }

    fn view_menu(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Panels").weak());
        for t in dock::Tab::ALL {
            let mut shown = dock::is_visible(&self.dock, t);
            let behind = !shown && dock::is_open(&self.dock, t);
            let r = ui.checkbox(&mut shown, t.title());
            let r = if behind {
                r.on_hover_text("Open behind another tab: click to bring it to the front")
            } else {
                r
            };
            if r.clicked() {
                dock::show_or_hide_tab(&mut self.dock, t);
            }
        }
        ui.separator();
        if ui.checkbox(&mut self.prefs.appearance.light, "Light theme").changed() {
            let _ = self.prefs.save();
        }
        ui.separator();
        ui.checkbox(&mut self.view.toolbar, "Toolbar (BPM, FFT gain)");
        ui.checkbox(&mut self.view.audio_controls, "Audio controls")
            .on_hover_text("Show audio controls: M / A faders in the layer headers and the Audio sections of the Composition, Group, Layer and Clip panels");
        ui.checkbox(&mut self.view.layer_transport, "Layer transport");
        ui.checkbox(&mut self.view.layer_transition, "Layer transition");
        ui.checkbox(&mut self.view.crossfader, "Crossfader")
            .on_hover_text("Show the crossfader and every A/B control (grid, Composition / Layer / Group panels)");
        ui.separator();
        ui.menu_button("Layouts", |ui| {
            for name in dock::PRESETS {
                if ui.button(*name).clicked() {
                    self.dock = dock::preset(name);
                    ui.close();
                }
            }
            let saved = dock::saved_names();
            if !saved.is_empty() {
                ui.separator();
                ui.label(RichText::new("Saved").weak());
            }
            for name in saved {
                ui.horizontal(|ui| {
                    if ui.button(&name).clicked() {
                        match dock::load_named(&name) {
                            Ok(l) => self.apply_layout(l),
                            Err(e) => self.status = format!("Layout {name}: {e}"),
                        }
                        ui.close();
                    }
                    if ui.small_button("🗑").on_hover_text("Delete this layout").clicked()
                        && let Err(e) = dock::delete_named(&name)
                    {
                        self.status = format!("Delete layout: {e}");
                    }
                });
            }
            ui.separator();
            if ui.button("Save Layout As…").clicked() {
                self.layout_name = Some(String::new());
                ui.close();
            }
        });
    }

    /// "Save Layout As…" dialog (a text field inside a menu would close it).
    fn layout_dialog(&mut self, ctx: &egui::Context) {
        let Some(name) = &mut self.layout_name else { return };
        let mut save = false;
        let r = egui::Modal::new(egui::Id::unique("save-layout")).show(ctx, |ui| {
            ui.heading("Save layout");
            let edit = ui.add(
                egui::TextEdit::singleline(name)
                    .hint_text("Layout name")
                    .desired_width(220.0),
            );
            if !edit.has_focus() && !ui.memory(|m| m.has_focus(edit.id)) && name.is_empty() {
                edit.request_focus();
            }
            let ok = !name.trim().is_empty();
            save = ok && edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                save |= ui.add_enabled(ok, egui::Button::new("Save")).clicked();
                if ui.button("Cancel").clicked() {
                    ui.close();
                }
            });
        });
        if save {
            let json = dock::to_json(&self.dock, self.view);
            self.status = match dock::save_named(name, &json) {
                Ok(n) => format!("Layout saved: {n}"),
                Err(e) => format!("Save layout: {e}"),
            };
            self.layout_name = None;
        } else if r.should_close() {
            self.layout_name = None;
        }
    }

    /// Effect chains of this frame: playing clip + layer per layer (outgoing
    /// clips too), groups, composition.
    fn fx_frame(&self) -> lavmfx_render::FxFrame {
        use effects_ui::draws;
        let comp = &self.comp;
        let (w, h) = comp.resolution();
        text::set_frame(w, h, self.clock.seconds() * comp.speed.value);
        let clip_fx = |s: Option<Slot>| effects_ui::clip_draws(s.and_then(|s| comp.clip(s.deck, s.layer, s.col)));
        let n = comp.layers.len();
        let mut f = lavmfx_render::FxFrame::default();
        for li in 0..n {
            let layer = draws(comp.effects(FxTarget::Layer(li)));
            let cur = self.playing.get(li).and_then(Option::as_ref).map(|p| p.slot);
            f.layer.push([clip_fx(cur), layer.clone()].concat());
            let out = self
                .outgoing
                .get(li)
                .and_then(Option::as_ref)
                .and_then(|o| o.pb.as_ref())
                .map(|p| p.slot);
            let mut chain = [clip_fx(out), layer].concat();
            chain.retain(|e| !e.feedback);
            f.outgoing.push(chain);
        }
        f.group = (0..comp.layergroups.len())
            .map(|g| draws(comp.effects(FxTarget::Group(g))))
            .collect();
        f.composition = draws(comp.effects(FxTarget::Composition));
        f.images = text::images(&f);
        f.slices = self.outputs.fx_slices();
        f.fft = self.isf_fft();
        f
    }

    /// External FFT as ISF `audioFFT` inputs see it (after the global threshold).
    fn isf_fft(&self) -> lavmfx_core::fft::Spectrum {
        lavmfx_core::fft::gate_spectrum(&self.audio.external(), self.prefs.audio.fft_threshold)
    }

    /// Video Router clips list the current layers / groups as inputs.
    fn refresh_router_inputs(&mut self) {
        use lavmfx_core::sources::{ROUTER, ROUTER_INPUT, router_options};
        let opts = router_options(self.comp.layers.len(), self.comp.layergroups.len());
        for l in &mut self.comp.layers {
            for c in &mut l.clips {
                let Some(v) = c.video.as_mut() else { continue };
                if !matches!(&v.source, Some(lavmfx_core::sources::ClipSource::Generator(n)) if n == ROUTER) {
                    continue;
                }
                if let Some(lavmfx_core::Param::Choice(ch)) =
                    v.sourceparams.as_mut().and_then(|p| p.get_mut(ROUTER_INPUT))
                    && ch.options != opts
                {
                    ch.options = opts.clone();
                    let i = opts.iter().position(|o| *o == ch.value).unwrap_or(0);
                    ch.index = i as i32;
                    ch.value = opts[i].clone();
                }
            }
        }
    }

    /// Follow the composition resolution: resize both compositors (the
    /// preview renders at the same size so pixel params match) and re-point
    /// their egui textures.
    fn sync_resolution(&mut self) {
        let (w, h) = self.comp.resolution();
        let (w, h) = (w.clamp(16, 8192), h.clamp(16, 8192));
        let filter = eframe::wgpu::FilterMode::Linear;
        if self.compositor.size() != (w, h) {
            self.compositor.resize(&self.gpu.device, w, h);
            self.gpu.renderer.write().update_egui_texture_from_wgpu_texture(
                &self.gpu.device,
                self.compositor.view(),
                filter,
                self.comp_tex,
            );
        }
        if self.preview_comp.size() != (w, h) {
            self.preview_comp.resize(&self.gpu.device, w, h);
            self.gpu.renderer.write().update_egui_texture_from_wgpu_texture(
                &self.gpu.device,
                self.preview_comp.view(),
                filter,
                self.preview_tex,
            );
        }
    }

    /// Sizing of the clip in `slot` (its Resize choice).
    fn clip_fit(&self, slot: Option<Slot>) -> lavmfx_render::Fit {
        slot.and_then(|s| self.comp.clip(s.deck, s.layer, s.col))
            .and_then(|c| c.video.as_ref())
            .map_or(lavmfx_render::Fit::Fit, |v| {
                lavmfx_render::Fit::from_index(v.resize.index)
            })
    }

    /// What a playing clip's layer chain starts from: its video frame, nothing (sources) or
    /// the layers below (effect clips).
    fn clip_input(&self, slot: Option<Slot>) -> lavmfx_render::LayerInput {
        use lavmfx_core::sources::ClipSource;
        let clip = slot.and_then(|s| self.comp.clip(s.deck, s.layer, s.col));
        match clip.and_then(|c| c.source()) {
            Some(ClipSource::Generator(name)) if lavmfx_core::sources::routed_source(name) => {
                use lavmfx_core::sources::RouteTarget;
                let params = clip
                    .and_then(|c| c.video.as_ref())
                    .and_then(|v| v.sourceparams.as_ref());
                let r = match lavmfx_core::sources::clip_route(name, params) {
                    Some(RouteTarget::Layer(i)) => lavmfx_render::Route::Layer(i as u32),
                    Some(RouteTarget::Group(i)) => lavmfx_render::Route::Group(i as u32),
                    _ => lavmfx_render::Route::Composition,
                };
                lavmfx_render::LayerInput::Route(r)
            }
            Some(ClipSource::Generator(_)) => lavmfx_render::LayerInput::Clear,
            Some(ClipSource::Effect) => lavmfx_render::LayerInput::Below,
            Some(ClipSource::Ndi(_) | ClipSource::Share(_) | ClipSource::Capture(_)) | None => {
                lavmfx_render::LayerInput::Frame
            }
        }
    }

    /// How the compositor draws each layer: mix settings plus running transitions.
    fn layer_draws(&self) -> Vec<LayerDraw> {
        self.comp
            .layer_mix()
            .into_iter()
            .enumerate()
            .map(|(li, m)| LayerDraw {
                group: m.group.map(|g| g as u32),
                opacity: m.opacity * self.clip_opacity(li),
                blend: self
                    .playing
                    .get(li)
                    .and_then(Option::as_ref)
                    .and_then(|p| self.comp.clip(p.slot.deck, p.slot.layer, p.slot.col))
                    .and_then(lavmfx_core::Clip::blend_override)
                    .unwrap_or(m.blend),
                transition: self.outgoing.get(li).and_then(Option::as_ref).map(|o| Transition {
                    progress: o.progress(),
                    blend: self.comp.layers[li].transition_mode(),
                }),
                fit: self.clip_fit(self.playing.get(li).and_then(Option::as_ref).map(|p| p.slot)),
                input: self.clip_input(self.playing.get(li).and_then(Option::as_ref).map(|p| p.slot)),
                fit_prev: self.clip_fit(
                    self.outgoing
                        .get(li)
                        .and_then(Option::as_ref)
                        .and_then(|o| o.pb.as_ref())
                        .map(|p| p.slot),
                ),
            })
            .collect()
    }

    /// Selected clip panel: name banner,
    /// Transport (time, timeline, direction icons, play mode, speed,
    /// duration), Video.
    /// The effect chain a dock panel shows (drop target for the whole panel).
    pub(crate) fn panel_fx_target(&self, tab: dock::Tab) -> Option<FxTarget> {
        match tab {
            dock::Tab::Clip => self.selected_clip.map(|(li, ci)| FxTarget::Clip(li, ci)),
            dock::Tab::Layer => self
                .selected_layer
                .filter(|&l| l < self.comp.layers.len())
                .map(FxTarget::Layer),
            dock::Tab::Group => self
                .selected_group
                .filter(|&g| g < self.comp.layergroups.len())
                .map(FxTarget::Group),
            dock::Tab::Composition => Some(FxTarget::Composition),
            _ => None,
        }
    }

    fn clip_panel(&mut self, ui: &mut egui::Ui) {
        let Some((li, ci)) = self.selected_clip else {
            ui.add(egui::Label::new(RichText::new("Drop video or audio files on clip slots (or Composition › Open video…). Click a clip's name strip to play it live, its thumbnail to preview it; click a column header to launch the column.").weak()).wrap());
            return;
        };
        let deck = self.comp.active_deck();
        let Some(clip) = self.comp.layers.get(li).and_then(|l| l.clips.get(ci)) else {
            return;
        };
        if clip.is_empty() {
            ui.label(RichText::new(format!("Layer {} · clip {}: empty", li + 1, ci + 1)).weak());
            return;
        }
        let clip_id = clip.id;
        let path = clip.video_path().filter(|_| !clip.is_missing()).map(str::to_owned);
        let audio_only = path
            .as_deref()
            .is_some_and(|p| lavmfx_media::is_audio_file(Path::new(p)));
        let thumb = path
            .as_deref()
            .filter(|_| !audio_only)
            .and_then(|p| self.thumbs.get(p).map(|t| t.id()));
        let wave = path.as_deref().and_then(|p| self.waves.get(p));
        let clip = &mut self.comp.layers[li].clips[ci];
        clip_banner(
            ui,
            &mut clip.name.value,
            thumb,
            wave.as_deref(),
            &format!("Layer {} · Clip {}", li + 1, ci + 1),
        );
        widgets::dashboard_section(ui, &mut clip.dashboard, &mut self.comp.ids, ("clip", clip_id), false);
        section(ui, "Clip", |ui| {
            choice_row(ui, "Beat Snap", ("beatsnap", clip_id), &mut clip.beatsnap, |_| true);
        });
        {
            let ap = clip.autopilot_mut(&mut self.comp.ids);
            let open = widgets::section_header(ui, ("clip-autopilot", clip_id), "Autopilot", false, false);
            if open.open {
                choice_row(ui, "Action", ("ap-action", clip_id), &mut ap.action, |_| true);
                param_row(
                    ui,
                    "Loops",
                    &mut ap.loops,
                    RowOpts {
                        hover: "How many times the clip plays before the layer's autopilot moves on",
                        ..Default::default()
                    },
                );
            }
        }

        // Live playback of this clip, else its preview (Preview Monitor).
        let live = self
            .playing
            .get(li)
            .and_then(Option::as_ref)
            .is_some_and(|p| p.slot.clip == clip_id && p.slot.deck == deck);
        let pb = if live {
            self.playing[li].as_mut()
        } else {
            self.preview.as_mut().filter(|p| p.slot.clip == clip_id)
        };
        let opening = self
            .opening
            .get(li)
            .and_then(Option::as_ref)
            .is_some_and(|o| o.slot.clip == clip_id);
        let mut action = None;
        let file_s = clip.file_seconds();
        let bpm = self.clock.bpm;
        if let Some(lavmfx_core::Transport::BpmSync(t)) = clip.transport.as_mut() {
            t.controls.ensure_bpm(&mut self.comp.ids);
        }
        // Transport SMPTE 1 / 2: input, its timecode now, its frame rate.
        let smpte_k = match clip.transporttype.index {
            i @ (2 | 3) => Some(i as usize - 2),
            _ => None,
        };
        let smpte_now = smpte_k.and_then(|k| self.smpte.now[k]);
        let smpte_fps = smpte_now.map_or(self.prefs.audio.smpte_fps, |n| n.fps);
        if let Some(t) = clip.transport.as_mut() {
            let file_dur = file_s;
            let (dir, mode, away, speed, sync, smpte_offset) = transport_controls_mut(t);
            let open = widgets::section_header(ui, ("transport", clip_id), "Transport", true, false);
            let _ = open.open.then(|| {
                // Time (top right), then the timeline.
                let (pos, dur) = pb.as_ref().map_or((0.0, file_dur), |p| (p.offset(), p.duration()));
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(fmt_clock(pos)).small().weak());
                        if opening {
                            ui.spinner();
                        }
                    });
                });
                if let Some(p) = timeline_bar(ui, pos, dur, pb.is_some()) {
                    action = Some(ClipAction::Scrub(p.0, p.1));
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if let Some(k) = smpte_k {
                        ui.label(RichText::new(format!("Follows SMPTE {}", k + 1)).weak())
                            .on_hover_text("Clip time = timecode − offset. Direction and speed don't apply.");
                    }
                    for (i, (icon, hover)) in [
                        (TransportIcon::Back, "Play backward"),
                        (TransportIcon::Pause, "Pause (Space)"),
                        (TransportIcon::Forward, "Play forward"),
                    ]
                    .into_iter()
                    .enumerate()
                    .filter(|_| smpte_k.is_none())
                    {
                        if transport_icon(ui, icon, dir.index == i as i32)
                            .on_hover_text(hover)
                            .clicked()
                        {
                            dir.select(i);
                            if pb.is_none() && i != 1 {
                                action = Some(ClipAction::Trigger);
                            }
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Share the space right of the icons; long names end in "…".
                        let w = ((ui.available_width() - 12.0) / 2.0 - 20.0).clamp(40.0, 130.0);
                        ui.spacing_mut().combo_width = w;
                        choice_combo(ui, "playmodeaway", "When triggered again", away);
                        choice_combo(ui, "playmode", "Play mode", mode);
                    });
                });
                ui.add_space(2.0);
                if let (Some(_), Some(off)) = (smpte_k, smpte_offset) {
                    smpte_offset_row(ui, off, smpte_fps, smpte_now);
                } else {
                    param_row(
                        ui,
                        "Speed",
                        speed,
                        RowOpts {
                            animate: true,
                            hover: "Clip speed",
                            ..Default::default()
                        },
                    );
                    match sync {
                        Length::Beats(sync) => beats_rows(ui, sync, file_s, bpm, clip_id),
                        Length::Seconds(d) => duration_row(ui, file_s, d),
                    }
                }
                ui.horizontal(|ui| {
                    ui.add_space(LABEL_W + 3.0);
                    let has_pb = pb.is_some();
                    if ui
                        .add_enabled(has_pb, egui::Button::new("|<").small())
                        .on_hover_text("Restart")
                        .clicked()
                    {
                        action = Some(ClipAction::Restart);
                    }
                    if ui
                        .add_enabled(has_pb, egui::Button::new("< 1").small())
                        .on_hover_text("Previous frame (Left)")
                        .clicked()
                    {
                        action = Some(ClipAction::Step(-1));
                    }
                    if ui
                        .add_enabled(has_pb, egui::Button::new("1 >").small())
                        .on_hover_text("Next frame (Right)")
                        .clicked()
                    {
                        action = Some(ClipAction::Step(1));
                    }
                    if ui
                        .add_enabled(has_pb, egui::Button::new("Stop").small())
                        .on_hover_text("Stop layer")
                        .clicked()
                    {
                        action = Some(ClipAction::Stop);
                    }
                    if !live && !opening && ui.button("Trigger").clicked() {
                        action = Some(ClipAction::Trigger);
                    }
                });
            });
            // Transport type at the right of the section header.
            let hr = open.header;
            let mut tt = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(egui::Rect::from_min_max(
                        pos2(ui.max_rect().right() - 110.0, hr.top()),
                        pos2(ui.max_rect().right(), hr.bottom()),
                    ))
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            tt.spacing_mut().combo_width = 100.0;
            let mut choice = clip.transporttype.clone();
            if choice_combo_with(
                &mut tt,
                ("transporttype", clip_id),
                "Transport type",
                &mut choice,
                |t| matches!(t, "Timeline" | "BPM Sync" | "SMPTE 1" | "SMPTE 2"),
            ) {
                clip.set_transport_type(&mut self.comp.ids, choice.index.max(0) as usize, bpm);
            }
        }
        if let Some(pb) = self.clip_target(li, live).filter(|p| p.slot.clip == clip_id) {
            let preview = if live { "" } else { "preview · " };
            let text = match pb.info() {
                Some(i) if lavmfx_media::is_audio_file(&i.path) => format!("{preview}audio only · {}", i.codec),
                Some(i) => format!(
                    "{preview}{}x{} · {} · {:.2} fps · {} · showing {:.0} fps",
                    i.width,
                    i.height,
                    i.codec,
                    i.fps(),
                    i.decode_path,
                    pb.shown_fps()
                ),
                None => format!("{preview}no file · rendered live · {:.0} fps", pb.shown_fps()),
            };
            ui.label(RichText::new(text).small().weak());
        }
        let clip = &mut self.comp.layers[li].clips[ci];
        if let Some(b) = clip.blend_mode_mut(&mut self.comp.ids) {
            widgets::choice_row(ui, "Blend Mode", ("clipblend", clip_id), b, |m| {
                m == lavmfx_core::mix::LAYER_DETERMINED_BLEND || lavmfx_core::mix::shader_blend(m).is_some()
            });
        }
        if let Some(v) = clip.video.as_mut() {
            video_container(ui, v, thumb);
            param_row(
                ui,
                "Opacity",
                &mut v.track.opacity,
                RowOpts {
                    animate: true,
                    ..Default::default()
                },
            );
        }
        // Source (generator) clip: its params.
        let res = self.comp.resolution();
        let live_src = match self.comp.layers[li].clips[ci].source().cloned() {
            Some(lavmfx_core::sources::ClipSource::Ndi(n)) => Some(("NDI", n, lavmfx_media::ndi::runtime_error())),
            Some(lavmfx_core::sources::ClipSource::Share(n)) => {
                Some((lavmfx_media::share::backend(), n, lavmfx_media::share::unavailable()))
            }
            Some(lavmfx_core::sources::ClipSource::Capture(d)) => Some(("Capture Device", d, None)),
            _ => None,
        };
        if let Some((kind, name, unavailable)) = live_src {
            section(ui, kind, |ui| {
                ui.label(format!("Source: {name}"));
                let clip_id = self.comp.layers[li].clips[ci].id;
                let rx = self
                    .playing
                    .iter()
                    .flatten()
                    .chain(self.preview.as_ref())
                    .filter(|pb| pb.slot.clip == clip_id)
                    .find_map(|pb| pb.live.as_ref());
                let playing = self
                    .playing
                    .iter()
                    .flatten()
                    .chain(self.preview.as_ref())
                    .any(|pb| pb.slot.clip == clip_id);
                let line = match (rx.map(|r| r.status()), unavailable) {
                    (_, Some(e)) => e,
                    (None, None) if playing => format!("Waiting for \"{name}\" (retrying)"),
                    (None, None) => "Not playing (trigger the clip to connect)".into(),
                    (Some(st), None) => match (&st.error, st.live) {
                        (Some(e), true) => format!("Receiving {}×{} · {:.0} fps · {e}", st.width, st.height, st.fps),
                        (Some(e), false) => format!("Error: {e}"),
                        (None, true) => format!("Receiving {}×{} · {:.2} fps", st.width, st.height, st.fps),
                        (None, false) if st.width > 0 => "No frames (source stopped?)".into(),
                        (None, false) => "Connecting…".into(),
                    },
                };
                ui.label(RichText::new(line).weak());
            });
        }
        let clip = &mut self.comp.layers[li].clips[ci];
        if let Some(v) = clip.video.as_mut()
            && let Some(lavmfx_core::sources::ClipSource::Generator(name)) = v.source.clone()
            && let Some(params) = v.sourceparams.as_mut()
        {
            let title = name
                .strip_prefix(lavmfx_core::isf::PREFIX)
                .or_else(|| name.strip_prefix(lavmfx_core::ffgl::PREFIX))
                .unwrap_or(&name);
            let sec = widgets::section_header(ui, ("src-section", clip_id), title, true, true);
            if sec.open {
                // Slice Outline clips made before its switch existed get it here (off).
                if name == lavmfx_core::sources::SLICE_OUTLINE
                    && !params.contains_key(lavmfx_core::sources::OUTLINE_FREE_FX)
                {
                    let id = self.comp.ids.alloc();
                    params.insert(
                        lavmfx_core::sources::OUTLINE_FREE_FX.into(),
                        lavmfx_core::Param::Bool(lavmfx_core::BoolParam::new(id, false)),
                    );
                }
                for (k, p) in params.iter_mut() {
                    effects_ui::param_widget(ui, k, p, ("src", clip_id), effects_ui::soft_range(k, res));
                }
                if let Some((_, s)) = lavmfx_core::isf::get(&name) {
                    if let Some(e) = &s.error {
                        ui.label(RichText::new(format!("ISF shader doesn't run: {e}")).weak());
                    }
                } else if let Some((_, p)) = lavmfx_core::ffgl::get(&name) {
                    if let Some(e) = &p.error {
                        ui.label(RichText::new(format!("FFGL plugin doesn't run: {e}")).weak());
                    }
                } else if lavmfx_core::ffgl::is_ffgl(&name) {
                    ui.label(RichText::new("FFGL plugin not installed (Preferences › Video).").weak());
                } else if lavmfx_core::isf::is_isf(&name) {
                    ui.label(RichText::new("ISF shader not installed (Preferences › Video).").weak());
                } else if !lavmfx_core::sources::spec(&name).is_some_and(|s| s.is_rendered()) {
                    ui.label(RichText::new("Not rendered yet (needs slices / other layers).").weak());
                }
            }
        }
        if self.comp.layers[li].clips[ci].video.is_some() {
            effects_ui::transform_block(ui, &mut self.comp, FxTarget::Clip(li, ci));
            effects_ui::effects_section(ui, &mut self.comp, FxTarget::Clip(li, ci));
        }
        if let Some(a) = self.comp.layers[li].clips[ci]
            .audio
            .as_mut()
            .filter(|_| self.view.audio_controls)
        {
            let ids = &mut self.comp.ids;
            section(ui, "Audio", |ui| {
                widgets::audio_rows(ui, &mut a.track, true);
                audio_fx_ui::audio_fx_rows(ui, &mut a.track, ids, ("clip", li, ci));
            });
        }

        match action {
            Some(ClipAction::Scrub(p, dragging)) => {
                if let Some(pb) = self.clip_target(li, live) {
                    pb.scrubbing = dragging;
                    pb.scrub_to(p);
                }
            }
            Some(ClipAction::Restart) => {
                if let Some(pb) = self.clip_target(li, live) {
                    pb.scrub_to(0.0);
                    pb.played = 0.0;
                }
            }
            Some(ClipAction::Step(n)) if live => self.step(li, n),
            Some(ClipAction::Step(n)) => {
                if let Some(t) = self.comp.layers[li].clips[ci].transport.as_mut() {
                    transport_controls_mut(t).0.select(1); // pause
                }
                if let Some(pb) = self.preview.as_mut() {
                    pb.settings.direction = lavmfx_core::transport::Direction::Pause;
                    pb.step(n);
                }
            }
            Some(ClipAction::Stop) if live => self.stop_layer(li),
            Some(ClipAction::Stop) => self.stop_preview(),
            Some(ClipAction::Trigger) => self.trigger(li, ci),
            None => {
                // Release after a timeline drag.
                if let Some(pb) = self.clip_target(li, live)
                    && pb.scrubbing
                    && !ui.input(|i| i.pointer.primary_down())
                {
                    pb.scrubbing = false;
                }
            }
        }
    }

    /// The playback the clip panel controls: the layer's live clip, or the preview.
    fn clip_target(&mut self, li: usize, live: bool) -> Option<&mut Playback> {
        if live {
            self.playing.get_mut(li)?.as_mut()
        } else {
            self.preview.as_mut()
        }
    }

    // ------------------------------------------------------------------ UI

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("LAVMFX", |ui| {
                ui.label(RichText::new("LAVMFX").strong());
                ui.label(RichText::new("Live Audio Visual Mapping FX Mixer\nsay \u{201c}love-me-FX\u{201d}").weak());
                ui.separator();
                if ui.button("About LAVMFX…").clicked() {
                    self.about_open = true;
                    ui.close();
                }
                ui.separator();
                if ui.button("Preferences…    Ctrl+,").clicked() {
                    self.prefs_win.open = true;
                    ui.close();
                }
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Composition", |ui| {
                if ui
                    .add_enabled(self.can_undo(), egui::Button::new("Undo    Ctrl+Z"))
                    .clicked()
                {
                    self.undo(false);
                }
                if ui
                    .add_enabled(self.can_redo(), egui::Button::new("Redo    Ctrl+Shift+Z"))
                    .clicked()
                {
                    self.undo(true);
                }
                ui.separator();
                let rec = if self.recording() { "Stop Recording    Ctrl+R" } else { "Start Recording    Ctrl+R" };
                if ui
                    .button(rec)
                    .on_hover_text("Record the composition to a file (Preferences › Recording); the recording is added to the deck")
                    .clicked()
                {
                    self.toggle_recording();
                    ui.close();
                }
                if ui.button("Snapshot    Ctrl+P").on_hover_text("Save a PNG of the composition").clicked() {
                    self.snapshot();
                    ui.close();
                }
                ui.separator();
                if ui.button("New").clicked() {
                    self.new_composition();
                }
                if ui
                    .button("Open…")
                    .on_hover_text("LAVMFX compositions (.lavmfx) or imported .avc compositions")
                    .clicked()
                {
                    self.open_composition_dialog();
                    ui.close();
                }
                let mut open = None;
                ui.menu_button("Open Recent", |ui| {
                    if self.prefs.recent.is_empty() {
                        ui.label(RichText::new("No recent compositions").weak());
                    }
                    for r in &self.prefs.recent {
                        let name = Path::new(r)
                            .file_name()
                            .map_or(r.clone(), |n| n.to_string_lossy().into_owned());
                        if ui.button(name).on_hover_text(r).clicked() {
                            open = Some(PathBuf::from(r));
                            ui.close();
                        }
                    }
                    if !self.prefs.recent.is_empty() {
                        ui.separator();
                        if ui.button("Clear Recent").clicked() {
                            self.prefs.recent.clear();
                            let _ = self.prefs.save();
                            ui.close();
                        }
                    }
                });
                if let Some(p) = open {
                    if p.exists() {
                        self.load(&p);
                    } else {
                        self.status = format!("Not found: {}", p.display());
                        self.prefs.recent.retain(|r| Path::new(r) != p);
                        let _ = self.prefs.save();
                    }
                }
                if ui.button("Save    Ctrl+S").clicked() {
                    self.save();
                }
                if ui.button("Save As…    Ctrl+Shift+S").clicked() {
                    self.save_as_dialog();
                    ui.close();
                }
                ui.separator();
                if ui.button("Open video…    Ctrl+O").clicked() {
                    self.open_dialog();
                    ui.close();
                }
                ui.separator();
                if ui
                    .button("Media Manager…")
                    .on_hover_text("Every file the clips use: find missing files, relink them, fix paths")
                    .clicked()
                {
                    self.open_media_manager();
                    ui.close();
                }
            });
            ui.menu_button("Layer", |ui| {
                let li = self.selected_layer.filter(|&l| l < self.comp.layers.len());
                let ctx = self.layer_menu_ctx(li);
                if let Some(a) = structure::layer_menu(ui, li, &ctx) {
                    self.apply_grid(a);
                }
            });
            ui.menu_button("Column", |ui| {
                if let Some(a) = structure::column_menu(ui, self.focus_column(), self.comp.columns.len()) {
                    self.apply_grid(a);
                }
            });
            ui.menu_button("Clip", |ui| self.clip_menu(ui));
            ui.menu_button("Output", |ui| self.output_menu(ui));
            ui.menu_button("Shortcuts", |ui| {
                if ui.button("Edit Keyboard…    Ctrl+K").clicked() {
                    self.start_learn(shortcuts::LearnKind::Keyboard);
                    ui.close();
                }
                if ui.button("Edit MIDI…    Ctrl+M").clicked() {
                    self.start_learn(shortcuts::LearnKind::Midi);
                    ui.close();
                }
                if ui.button("Edit DMX…    Ctrl+D").clicked() {
                    self.start_learn(shortcuts::LearnKind::Dmx);
                    ui.close();
                }
            });
            ui.menu_button("View", |ui| self.view_menu(ui));
            ui.menu_button("Help", |ui| self.help_menu(ui));
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(rec) = self.recording_label() {
                ui.label(RichText::new(rec).strong().color(theme::cur().danger));
                ui.separator();
            }
            ui.label(format!("{:.0} fps", self.fps.fps));
            ui.separator();
            ui.label(&self.gpu_summary);
            ui.separator();
            ui.label(format!("{:.1} BPM", self.clock.bpm));
            let n = self.playing.iter().flatten().count();
            if n > 0 {
                ui.separator();
                ui.label(format!("{n} layer{} playing", if n == 1 { "" } else { "s" }));
            }
            ui.separator();
            let file = self
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "unsaved".into());
            ui.label(file);
            if !self.status.is_empty() {
                ui.separator();
                ui.label(RichText::new(&self.status).weak());
            }
        });
    }

    fn clip_grid(&mut self, ui: &mut egui::Ui) {
        let active = self.comp.active_deck();
        // Deck tabs
        let mut switch_to = None;
        let mut remove = None;
        let mut rename_deck = None;
        let n_decks = self.comp.decks.len();
        ui.horizontal(|ui| {
            for (i, deck) in self.comp.decks.iter().enumerate() {
                let r = ui.selectable_label(i == active, &deck.name.value);
                learn_target(
                    ui,
                    r.rect,
                    LearnTarget::Address(format!("/composition/decks/{}/select", i + 1)),
                );
                if r.clicked() {
                    switch_to = Some(i);
                }
                r.context_menu(|ui| {
                    ui.label(RichText::new(&deck.name.value).weak());
                    if ui.button("Rename…").clicked() {
                        rename_deck = Some(i);
                        ui.close();
                    }
                    if ui.add_enabled(n_decks > 1, egui::Button::new("Remove")).clicked() {
                        remove = Some(i);
                        ui.close();
                    }
                });
                if n_decks > 1 {
                    let x = egui::Button::new(RichText::new("×").small()).frame(false);
                    if ui.add(x).on_hover_text("Remove deck (its clips too)").clicked() {
                        remove = Some(i);
                    }
                }
            }
            if ui.small_button("+").on_hover_text("Add deck").clicked() {
                let n = self.comp.decks.len() + 1;
                switch_to = Some(self.comp.add_deck(&format!("Deck {n}")));
            }
        });
        if let Some(i) = switch_to {
            self.select_deck(i);
        }
        if let Some(i) = remove {
            self.remove_deck(i);
        }
        if let Some(i) = rename_deck {
            self.apply_grid(structure::GridAction::Rename(structure::RenameTarget::Deck(i)));
        }
        ui.separator();

        self.ensure_layers();
        self.slot_rects.clear();
        self.layer_rows.clear();
        self.group_bands.clear();
        let mods = ui.input(|i| i.modifiers);
        let mut select = None;
        let mut trigger = None;
        let mut launch = None;
        let mut clear = None;
        let mut eject = None;
        let mut menu = None;
        let mut drag_start = None;
        let mut direction = None;
        let mut skip = None;
        let mut grid_action = None;
        let mut preview = None;
        let mut marquee_start = false;
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            self.grid_view = ui.clip_rect();
            // The header column (composition cell, group bands, layer headers)
            // stays at the left edge when the grid scrolls sideways.
            let view = self.view;
            let origin = ui.max_rect().min;
            let sx = (ui.clip_rect().left() - origin.x).max(0.0);
            let pin_w = header_extra_w(view, ui.spacing().item_spacing.x);
            self.grid_pin = origin.x + sx + pin_w;
            self.grid_origin = origin;
            // Drag on empty grid space: box selection (widgets on top win).
            let bg = ui.interact(
                ui.clip_rect(),
                ui.scope_id().with("grid-bg"),
                egui::Sense::click_and_drag(),
            );
            marquee_start |= bg.drag_started();
            // Composition row (crossfader) + column headers: click = launch column.
            ui.horizontal(|ui| {
                let w = pin_w;
                pinned_cell(ui, vec2(w, 18.0), sx, |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.label(RichText::new("Composition").small().strong());
                    if view.crossfader {
                        let cf = &mut self.comp.crossfader;
                        if ui.small_button("A").on_hover_text("Crossfader to A").clicked() {
                            cf.phase.set(-1.0);
                        }
                        ui.spacing_mut().slider_width = (ui.available_width() - 22.0).max(20.0);
                        let mut v = cf.phase.value;
                        let r = wheel_slider(ui, &mut v, -1.0..=1.0, Wheel::Linear, false)
                            .on_hover_text("Crossfader (double-click: centre)");
                        learn_target(ui, r.rect, LearnTarget::Param(cf.phase.id));
                        cf.phase.set(if r.double_clicked() { 0.0 } else { v });
                        if ui.small_button("B").on_hover_text("Crossfader to B").clicked() {
                            cf.phase.set(1.0);
                        }
                    }
                });
                for (ci, col) in self.comp.columns.iter().enumerate() {
                    let lit = self.launched_column == Some((active, ci));
                    let mut b = egui::Button::new(RichText::new(&col.name.value).small());
                    if lit {
                        b = b.fill(theme::cur().accent).selected(true);
                    }
                    let r = ui
                        .add_sized([CLIP_W, 18.0], b)
                        .on_hover_text("Launch column (right-click: column menu)");
                    learn_target(
                        ui,
                        r.rect,
                        LearnTarget::Address(format!("/composition/columns/{}/connect", ci + 1)),
                    );
                    if r.clicked() {
                        launch = Some(ci);
                    }
                    let n = self.comp.columns.len();
                    r.context_menu(|ui| {
                        if let Some(a) = structure::column_menu(ui, Some(ci), n) {
                            grid_action = Some(a);
                        }
                    });
                }
            });
            // Layers, top-most first
            let n = self.comp.layers.len();
            for li in (0..n).rev() {
                let queued: Vec<usize> = (0..self.comp.columns.len())
                    .filter(|&c| self.is_queued(li, c))
                    .collect();
                // Group band above a group's top layer; folded groups hide their layers.
                let group = self.comp.group_of(li);
                if let Some(gi) = group {
                    if li + 1 == n || self.comp.group_of(li + 1) != Some(gi) {
                        let sel = self.selected_group == Some(gi);
                        let b = group_band(ui, gi, &mut self.comp.layergroups[gi], sel, view, sx);
                        self.group_bands.push((b.rect, li));
                        if b.select {
                            // The selected layer stays (Layer and Group panels are separate).
                            self.selected_group = Some(gi);
                            self.focus_tab = Some(dock::Tab::Group);
                        }
                        if b.menu.is_some() {
                            grid_action = b.menu;
                        }
                    }
                    if self.comp.layergroups[gi].folded {
                        continue;
                    }
                }
                let row = ui.horizontal(|ui| {
                    let playing = self.playing[li].as_ref().filter(|p| p.slot.deck == active);
                    let progress = playing.map(|p| (p.slot.col, p.offset() / p.duration().max(1e-6)));
                    let opening = self.opening[li]
                        .as_ref()
                        .filter(|o| o.slot.deck == active)
                        .map(|o| o.slot.col);
                    let dir = self.playing_direction(li);
                    let menu_ctx = self.layer_menu_ctx(Some(li));
                    let any_playing = self.playing[li].is_some();
                    let layer_selected = self.selected_layer == Some(li);
                    let ids = &mut self.comp.ids;
                    let layer = &mut self.comp.layers[li];
                    let h = pinned_cell(ui, vec2(pin_w, CLIP_H), sx, |ui| {
                        layer_header(ui, li, &menu_ctx, layer, ids, any_playing, dir, layer_selected, view)
                    });
                    if h.clear {
                        clear = Some(li);
                    }
                    if h.menu.is_some() {
                        grid_action = h.menu;
                    }
                    if h.drag {
                        self.layer_drag = Some(li);
                    }
                    if h.select {
                        // The selected group stays. A playing clip on this layer (deck on
                        // view) becomes the selected clip and is scrolled into view.
                        self.selected_layer = Some(li);
                        self.focus_tab = Some(dock::Tab::Layer);
                        if let Some(c) = self.playing[li]
                            .as_ref()
                            .filter(|p| p.slot.deck == active)
                            .map(|p| p.slot.col)
                        {
                            self.selected_clip = Some((li, c));
                            self.selection = BTreeSet::from([(li, c)]);
                            self.reveal = Some((li, c));
                        }
                    }
                    if let Some(d) = h.direction {
                        direction = Some((li, d));
                    }
                    if let Some(f) = h.skip {
                        skip = Some((li, f));
                    }
                    for (ci, clip) in layer.clips.iter().enumerate() {
                        // Thumbnails only for slots on screen (big compositions) with a file.
                        let next = egui::Rect::from_min_size(ui.cursor().min, vec2(CLIP_W, CLIP_H));
                        let path = clip
                            .video_path()
                            .filter(|_| !clip.is_missing() && ui.is_rect_visible(next));
                        let audio_only = path.is_some_and(|p| lavmfx_media::is_audio_file(Path::new(p)));
                        let thumb = path
                            .filter(|_| !audio_only)
                            .and_then(|p| self.thumbs.get(p))
                            .map(|t| (t.id(), t.size_vec2()));
                        let wave = path.and_then(|p| self.waves.get(p));
                        let state = SlotState {
                            selected: self.selection.contains(&(li, ci)),
                            focused: self.selected_clip == Some((li, ci)),
                            progress: progress.filter(|(c, _)| *c == ci).map(|(_, f)| f as f32),
                            opening: opening == Some(ci) || queued.contains(&ci),
                        };
                        let (pick, name) = clip_slot(ui, (li, ci), clip, thumb, wave.as_deref(), audio_only, state);
                        learn_target(
                            ui,
                            pick.rect,
                            LearnTarget::Address(format!("/composition/layers/{}/clips/{}/connect", li + 1, ci + 1)),
                        );
                        // Thumbnail: trigger (empty slot: select). Name strip: select.
                        // Thumbnail: select (+ preview on a plain click). Name strip: trigger live.
                        if pick.clicked() {
                            select = Some((li, ci));
                            if !clip.is_empty() && !mods.command && !mods.shift {
                                preview = Some((li, ci));
                            }
                        }
                        if pick.drag_started() {
                            if clip.is_empty() {
                                marquee_start = true;
                            } else {
                                drag_start = Some((li, ci));
                            }
                        }
                        if name.as_ref().is_some_and(egui::Response::clicked) {
                            trigger = Some((li, ci));
                            if !mods.command && !mods.shift {
                                select = Some((li, ci));
                            }
                        }
                        let can_paste = !self.clipboard.is_empty();
                        if let Some(a) = clipboard::slot_menu(&pick, !clip.is_empty(), can_paste)
                            .or_else(|| name.as_ref().and_then(|n| clipboard::slot_menu(n, true, can_paste)))
                        {
                            menu = Some(((li, ci), a));
                        }
                        let whole = egui::Rect::from_min_size(pick.rect.min, vec2(CLIP_W, CLIP_H));
                        if self.reveal == Some((li, ci)) {
                            // Minimal scroll: the row is on screen already, so only x moves.
                            // Room for the pinned headers on its left, so it isn't under them.
                            let room = whole.with_min_x(whole.left() - pin_w);
                            ui.scroll_to_rect(room, None);
                            self.reveal = None;
                        }
                        self.slot_rects.push((whole, (li, ci)));
                    }
                });
                if let Some(gi) = group {
                    let r = row.response.rect.translate(vec2(sx, 0.0));
                    let stripe = egui::Rect::from_min_max(r.left_top(), pos2(r.left() + 3.0, r.bottom()));
                    ui.painter().rect_filled(stripe, 0.0, group_color(gi));
                }
                self.layer_rows.push((row.response.rect, li));
            }
        });
        if marquee_start && let Some(p) = ui.input(|i| i.pointer.press_origin()) {
            let base = if mods.command || mods.shift {
                self.selection.clone()
            } else {
                BTreeSet::new()
            };
            self.grid_marquee = Some(Marquee {
                start: p - self.grid_origin,
                base,
            });
        }
        self.marquee_feedback(ui);
        self.layer_drag_feedback(ui);
        let targets = self.drag_targets();
        for (rect, slot) in &self.slot_rects {
            if targets.contains(slot) {
                let accent = theme::cur().drop;
                ui.painter().rect_filled(*rect, 3.0, accent.gamma_multiply(0.25));
                ui.painter()
                    .rect_stroke(*rect, 3.0, egui::Stroke::new(2.0, accent), egui::StrokeKind::Inside);
            }
        }
        if let Some(slot) = select {
            self.click_select(slot, mods);
            self.selected_layer = Some(slot.0);
        }
        if let Some((li, ci)) = trigger {
            self.trigger(li, ci);
        }
        if let Some(ci) = launch {
            self.launch_column(ci);
        }
        if let Some(li) = clear {
            self.stop_layer(li);
        }
        if let Some((li, d)) = direction {
            self.set_direction(li, d);
        }
        if let Some((li, fwd)) = skip {
            let a = if fwd { "connectnextclip" } else { "connectprevclip" };
            self.control(
                &format!("/composition/layers/{}/{a}", li + 1),
                lavmfx_core::control::Value::Trigger,
            );
        }
        if let Some(a) = grid_action {
            self.apply_grid(a);
        }
        if let Some((li, ci)) = preview {
            self.preview_clip(li, ci);
        }
        if let Some((slot, a)) = menu {
            eject = self.slot_menu_action(slot, a, ui.ctx());
        }
        if let Some(slot) = eject {
            if self.selection.contains(&slot) {
                self.eject_selection();
            } else {
                self.eject(slot.0, slot.1);
            }
        }
        if let Some(slot) = drag_start {
            // Dragging an unselected clip drags just that one.
            if !self.selection.contains(&slot) {
                self.select_only(slot);
            }
            self.clip_drag = Some(slot);
        }
        self.clip_drag_feedback(ui);
    }

    /// While a box selection is dragged on the grid: the box, and every
    /// filled slot it touches selected (added to what was selected before
    /// when Ctrl / Shift was held at the start).
    fn marquee_feedback(&mut self, ui: &mut egui::Ui) {
        let Some(m) = &self.grid_marquee else { return };
        let (pos, down) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.primary_down()));
        let Some(pos) = pos.filter(|_| down) else {
            self.grid_marquee = None;
            return;
        };
        let rect = egui::Rect::from_two_pos(self.grid_origin + m.start, pos);
        let mut sel = m.base.clone();
        for (r, (l, c)) in &self.slot_rects {
            let filled = self
                .comp
                .layers
                .get(*l)
                .and_then(|x| x.clips.get(*c))
                .is_some_and(|x| !x.is_empty());
            if filled && r.right() > self.grid_pin && r.intersects(rect) {
                sel.insert((*l, *c));
            }
        }
        if !self.selected_clip.is_some_and(|s| sel.contains(&s)) {
            self.selected_clip = sel.iter().next().copied();
        }
        if let Some((l, _)) = self.selected_clip {
            self.selected_layer = Some(l);
        }
        self.selection = sel;
        let accent = theme::cur().accent;
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("grid-marquee"),
        ));
        let shown = rect.intersect(self.grid_view.with_min_x(self.grid_pin.max(self.grid_view.left())));
        painter.rect_filled(shown, 0.0, accent.gamma_multiply(0.12));
        painter.rect_stroke(shown, 0.0, egui::Stroke::new(1.0, accent), egui::StrokeKind::Inside);
        ui.ctx().request_repaint();
    }

    /// While a layer name strip is dragged: a line where the layer would
    /// land; on release, move it there.
    fn layer_drag_feedback(&mut self, ui: &mut egui::Ui) {
        let Some(from) = self.layer_drag else { return };
        let (pos, down) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.primary_down()));
        let Some(pos) = pos else { return };
        let Some(&(last, _)) = self.layer_rows.last() else {
            return;
        };
        // First row (top-down) whose middle is below the pointer: drop above it.
        let (gap, mut y) = self
            .layer_rows
            .iter()
            .find(|(r, _)| pos.y < r.center().y)
            .map_or((0, last.bottom()), |&(r, li)| (li + 1, r.top()));
        // Right above a group's top layer the pointer is over its band: upper
        // half = outside (above the group), lower half = inside.
        let band = self.group_bands.iter().find(|(_, top)| gap == top + 1).map(|(r, _)| *r);
        let above_band = band.is_some_and(|r| pos.y < r.center().y);
        if let (Some(r), true) = (band, above_band) {
            y = r.top();
        }
        let groups: Vec<Option<usize>> = (0..self.comp.layers.len()).map(|l| self.comp.group_of(l)).collect();
        let target = structure::drop_group(&groups, from, gap, above_band);
        let to = structure::drop_index(from, gap);
        let regroup = groups.get(from).copied().flatten() != target;
        if down {
            if to != from || regroup {
                let accent = target.map_or(theme::cur().drop, group_color);
                let x = (last.left(), ui.clip_rect().right().min(last.right()));
                ui.painter().hline(x.0..=x.1, y, egui::Stroke::new(3.0, accent));
            }
            let name = &self.comp.layers[from].name.value;
            let into = target.map_or(String::new(), |g| {
                format!(" into {}", self.comp.layergroups[g].name.value)
            });
            egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), egui::Id::unique("layer-drag"), pos)
                .show(|ui| ui.label(format!("Move {name}{into}")));
            ui.ctx().request_repaint();
        } else {
            self.layer_drag = None;
            if to != from || regroup {
                self.move_layer(from, to);
                let id = target.map(|g| self.comp.layergroups[g].id);
                self.comp.layers[to].group = id;
                self.comp.prune_groups();
                self.status = format!("Moved layer to position {}", to + 1);
            }
        }
    }

    /// While a clip drag is active: show where it would land (slot borders
    /// for overwrite, a line in the gap for insert) and a label at the
    /// pointer; on release, drop (Ctrl = copy).
    fn clip_drag_feedback(&mut self, ui: &mut egui::Ui) {
        let Some(from) = self.clip_drag else { return };
        let gap = ui.spacing().item_spacing.x;
        let (pos, released, copy) = ui.input(|i| {
            // Alt or Ctrl held while dragging = copy.
            (
                i.pointer.latest_pos(),
                i.pointer.any_released(),
                i.modifiers.alt || i.modifiers.command,
            )
        });
        let at = pos.and_then(|p| self.drop_at(p, gap));
        if released {
            self.clip_drag = None;
            if let Some(at) = at {
                self.move_clips(from, at, copy);
            }
            return;
        }
        let sources = self.drag_sources(from);
        let landing = at.and_then(|at| self.drop_targets(&sources, from, at));
        let accent = if copy { theme::cur().accent } else { theme::cur().drop };
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("clip-drag"),
        ));
        let rect_of = |slot: (usize, usize)| self.slot_rects.iter().find(|(_, s)| *s == slot).map(|(r, _)| *r);
        match (at, &landing) {
            (Some(DropAt::On(..)), Some(targets)) => {
                for t in targets {
                    if let Some(r) = rect_of(*t) {
                        painter.rect_filled(r, 3.0, accent.gamma_multiply(0.2));
                        painter.rect_stroke(r, 3.0, egui::Stroke::new(2.5, accent), egui::StrokeKind::Inside);
                    }
                }
            }
            (Some(DropAt::Between(_, k)), Some(targets)) => {
                let rows: BTreeSet<usize> = targets.iter().map(|t| t.0).collect();
                for l in rows {
                    // Gap left of slot k, or right of the last slot.
                    let x_y = rect_of((l, k))
                        .map(|r| (r.left() - gap / 2.0, r.y_range()))
                        .or_else(|| {
                            k.checked_sub(1)
                                .and_then(|c| rect_of((l, c)))
                                .map(|r| (r.right() + gap / 2.0, r.y_range()))
                        });
                    if let Some((x, y)) = x_y {
                        painter.vline(x, y, egui::Stroke::new(3.0, accent));
                        painter.circle_filled(pos2(x, y.min), 3.5, accent);
                        painter.circle_filled(pos2(x, y.max), 3.5, accent);
                    }
                }
            }
            _ => {}
        }
        if let Some(p) = pos {
            let n = sources.len();
            let name = self
                .comp
                .layers
                .get(from.0)
                .and_then(|l| l.clips.get(from.1))
                .map(|c| c.name.value.clone())
                .unwrap_or_default();
            let what = if n == 1 { name } else { format!("{n} clips") };
            let verb = match (copy, at) {
                (true, Some(DropAt::Between(..))) => "Insert copy of",
                (false, Some(DropAt::Between(..))) => "Insert",
                (true, _) => "Copy",
                (false, _) => "Move",
            };
            let note = if at.is_some() && landing.is_none() {
                " (doesn't fit)"
            } else {
                ""
            };
            let galley = painter.layout_no_wrap(
                format!("{verb} {what}{note}"),
                egui::FontId::proportional(13.0),
                theme::cur().overlay_text,
            );
            let at = p + vec2(14.0, 14.0);
            painter.rect_filled(
                egui::Rect::from_min_size(at, galley.size()).expand(5.0),
                4.0,
                theme::cur().overlay,
            );
            painter.galley(at, galley, theme::cur().overlay_text);
        }
        ui.ctx().request_repaint();
    }

    fn composition_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Name").weak());
            ui.text_edit_singleline(&mut self.comp.name.value);
        });
        widgets::dashboard_section(ui, &mut self.comp.dashboard, &mut self.comp.ids, "comp", true);
        section(ui, "Composition", |ui| {
            resolution_row(ui, &mut self.comp);
            param_row(
                ui,
                "Master",
                &mut self.comp.master,
                RowOpts {
                    animate: true,
                    hover: "Composition master: dims the output",
                    ..Default::default()
                },
            );
            param_row(
                ui,
                "Speed",
                &mut self.comp.speed,
                RowOpts {
                    animate: true,
                    hover: "Playback speed of every clip",
                    ..Default::default()
                },
            );
            choice_row(
                ui,
                "Clip Beat Snap",
                "clipbeatsnap",
                &mut self.comp.clipbeatsnap,
                |_| true,
            );
        });
        if let Some(a) = self.comp.audio.as_mut().filter(|_| self.view.audio_controls) {
            let ids = &mut self.comp.ids;
            section(ui, "Audio", |ui| {
                widgets::audio_rows(ui, a, false);
                audio_fx_ui::audio_fx_rows(ui, a, ids, "comp");
            });
        }
        if self.view.crossfader {
            section(ui, "Crossfader", |ui| {
                let cf = &mut self.comp.crossfader;
                param_row(
                    ui,
                    "Phase",
                    &mut cf.phase,
                    RowOpts {
                        animate: true,
                        hover: "−1 = side A, +1 = side B",
                        ..Default::default()
                    },
                );
                choice_row(ui, "Curve", "cf-curve", &mut cf.curve, |_| true);
                choice_row(ui, "Behaviour", "cf-behaviour", &mut cf.behaviour, |b| b == "Cut");
            });
        }
        effects_ui::effects_section(ui, &mut self.comp, FxTarget::Composition);
    }

    /// "Rename…" dialog for a layer, group or deck.
    fn rename_dialog(&mut self, ctx: &egui::Context) {
        let Some((target, text)) = &mut self.rename else { return };
        let what = match target {
            structure::RenameTarget::Layer(_) => "layer",
            structure::RenameTarget::Group(_) => "group",
            structure::RenameTarget::Deck(_) => "deck",
        };
        let mut ok = false;
        let r = egui::Modal::new(egui::Id::unique("rename")).show(ctx, |ui| {
            ui.heading(format!("Rename {what}"));
            let edit = ui.add(egui::TextEdit::singleline(text).desired_width(240.0));
            if ui.memory(|m| m.focused().is_none()) {
                edit.request_focus();
            }
            ok = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                ok |= ui.button("OK").clicked();
                if ui.button("Cancel").clicked() {
                    ui.close();
                }
            });
        });
        if ok {
            let (target, text) = self.rename.take().unwrap();
            let name = text.trim().to_owned();
            if !name.is_empty() {
                let slot = match target {
                    structure::RenameTarget::Layer(l) => self.comp.layers.get_mut(l).map(|x| &mut x.name.value),
                    structure::RenameTarget::Group(g) => self.comp.layergroups.get_mut(g).map(|x| &mut x.name.value),
                    structure::RenameTarget::Deck(d) => self.comp.decks.get_mut(d).map(|x| &mut x.name.value),
                };
                if let Some(v) = slot {
                    *v = name;
                }
            }
        } else if r.should_close() {
            self.rename = None;
        }
    }

    /// Group panel: the selected layer group's name, blending, transition
    /// (applies to all its layers), transport (its playing clips) and layers.
    fn group_tab(&mut self, ui: &mut egui::Ui) {
        let view_cf = self.view.crossfader;
        let view_audio = self.view.audio_controls;
        let Some(gi) = self.selected_group.filter(|&g| g < self.comp.layergroups.len()) else {
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Click a group's name in the grid (or Layer menu › Group › New Group) to see it here.",
                    )
                    .weak(),
                )
                .wrap(),
            );
            return;
        };
        let members = self.comp.group_members(gi);
        let ids = &mut self.comp.ids;
        let g = &mut self.comp.layergroups[gi];
        ui.horizontal(|ui| {
            ui.label(RichText::new("Name").weak());
            ui.text_edit_singleline(&mut g.name.value);
        });
        widgets::dashboard_section(ui, &mut g.dashboard, ids, ("group", g.id), true);
        section(ui, "Group", |ui| {
            param_row(
                ui,
                "Master",
                &mut g.master,
                RowOpts {
                    animate: true,
                    hover: "Group master",
                    ..Default::default()
                },
            );
            param_row(
                ui,
                "Speed",
                g.speed_mut(ids),
                RowOpts {
                    hover: "Playback speed of the group's clips",
                    ..Default::default()
                },
            );
            if view_cf {
                choice_row(ui, "Crossfader", ("group-cf", gi), &mut g.crossfadergroup, |_| true);
            }
            bool_row(ui, "Bypass", &mut g.bypassed.value, "Hide all layers of the group");
            bool_row(ui, "Solo", &mut g.solo.value, "Show only soloed layers/groups");
            bool_row(ui, "Folded", &mut g.folded, "Hide the group's layers in the grid");
        });
        section(ui, "Video", |ui| {
            if let Some(v) = &mut g.video {
                param_row(
                    ui,
                    "Opacity",
                    &mut v.opacity,
                    RowOpts {
                        animate: true,
                        ..Default::default()
                    },
                );
            }
            if let Some(b) = g.blend_mode_mut(ids) {
                choice_row(ui, "Blend Mode", ("group-blend", gi), b, |m| shader_blend(m).is_some());
            }
        });
        if let Some(a) = g.audio.as_mut().filter(|_| view_audio) {
            section(ui, "Audio", |ui| {
                widgets::audio_rows(ui, a, true);
                audio_fx_ui::audio_fx_rows(ui, a, ids, ("group", gi));
            });
        }
        // Transition: shown from the first layer, written to all of them.
        if let Some(&first) = members.first() {
            let mut t = self.comp.layers[first].transition.clone();
            let before = t.clone();
            section(ui, "Transition", |ui| {
                param_row(
                    ui,
                    "Duration",
                    &mut t.duration,
                    RowOpts {
                        wheel: Some(Wheel::Log { smallest: 0.05 }),
                        hover: "Transition time of every layer in the group",
                        ..Default::default()
                    },
                );
                choice_row(ui, "Mode", ("group-trans", gi), &mut t.blend_mode, |m| {
                    shader_blend(m).is_some()
                });
            });
            if t != before {
                for &l in &members {
                    let lt = &mut self.comp.layers[l].transition;
                    lt.duration.set(t.duration.value);
                    lt.blend_mode.select_value(&t.blend_mode.value);
                }
            }
        }
        // Transport: direction / stop for the group's playing clips.
        let playing: Vec<usize> = members
            .iter()
            .copied()
            .filter(|&l| self.playing.get(l).is_some_and(Option::is_some))
            .collect();
        let mut dir = None;
        let mut stop = false;
        section(ui, "Transport", |ui| {
            ui.horizontal(|ui| {
                ui.add_space(LABEL_W + 3.0);
                let live = !playing.is_empty();
                for (i, (icon, hover)) in [
                    (TransportIcon::Back, "Play all backward"),
                    (TransportIcon::Pause, "Pause all"),
                    (TransportIcon::Forward, "Play all forward"),
                ]
                .into_iter()
                .enumerate()
                {
                    let on = live && playing.iter().all(|&l| self.playing_direction(l) == Some(i as i32));
                    if transport_icon(ui, icon, on).on_hover_text(hover).clicked() && live {
                        dir = Some(i);
                    }
                }
                if ui
                    .add_enabled(live, egui::Button::new("Stop"))
                    .on_hover_text("Clear every layer of the group")
                    .clicked()
                {
                    stop = true;
                }
                ui.label(RichText::new(format!("{} playing", playing.len())).weak().small());
            });
        });
        if let Some(d) = dir {
            for &l in &playing {
                self.set_direction(l, d);
            }
        }
        if stop {
            for &l in &playing {
                self.stop_layer(l);
            }
        }
        section(ui, "Layers", |ui| {
            for &l in members.iter().rev() {
                let name = self.comp.layers[l].name.value.clone();
                if ui
                    .selectable_label(false, name)
                    .on_hover_text("Show layer settings")
                    .clicked()
                {
                    self.selected_layer = Some(l);
                    self.focus_tab = Some(dock::Tab::Layer);
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Rename…").clicked() {
                self.apply_grid(structure::GridAction::Rename(structure::RenameTarget::Group(gi)));
            }
            if ui
                .button("Ungroup")
                .on_hover_text("Dissolve the group; its layers stay")
                .clicked()
            {
                self.apply_grid(structure::GridAction::RemoveGroup(gi));
            }
        });
        effects_ui::effects_section(ui, &mut self.comp, FxTarget::Group(gi));
    }

    /// Settings of the selected layer.
    fn layer_panel(&mut self, ui: &mut egui::Ui) {
        let view_cf = self.view.crossfader;
        let view_audio = self.view.audio_controls;
        let Some(li) = self.selected_layer.filter(|&l| l < self.comp.layers.len()) else {
            ui.add(egui::Label::new(RichText::new("Click a layer name to see its settings.").weak()).wrap());
            return;
        };
        let ids = &mut self.comp.ids;
        let layer = &mut self.comp.layers[li];
        ui.horizontal(|ui| {
            ui.label(RichText::new("Name").weak());
            ui.text_edit_singleline(&mut layer.name.value);
        });
        widgets::dashboard_section(ui, &mut layer.dashboard, ids, ("layer", layer.id), true);
        section(ui, "Layer", |ui| {
            param_row(
                ui,
                "Master",
                &mut layer.master,
                RowOpts {
                    animate: true,
                    ..Default::default()
                },
            );
            if view_cf {
                choice_row(ui, "Crossfader", ("layer-cf", li), &mut layer.crossfadergroup, |_| true);
            }
            bool_row(ui, "Bypass", &mut layer.bypassed.value, "Hide this layer");
            bool_row(ui, "Solo", &mut layer.solo.value, "Show only soloed layers");
            let mut v = !layer.video_off;
            if bool_row(ui, "Video", &mut v, "Show this layer's video (V)") {
                layer.video_off = !v;
            }
            let mut a = !layer.audio_off;
            if view_audio && bool_row(ui, "Audio", &mut a, "Play this layer's audio (A)") {
                layer.audio_off = !a;
            }
            bool_row(
                ui,
                "Ignore column",
                &mut layer.ignorecolumntrigger.value,
                "Column launches skip this layer",
            );
        });
        section(ui, "Video", |ui| {
            if let Some(v) = &mut layer.video {
                param_row(
                    ui,
                    "Opacity",
                    &mut v.track.opacity,
                    RowOpts {
                        animate: true,
                        ..Default::default()
                    },
                );
            }
            if let Some(b) = layer.blend_mode_mut(ids) {
                choice_row(ui, "Blend Mode", ("layer-blend", li), b, |m| shader_blend(m).is_some());
            }
        });
        if let Some(a) = layer.audio.as_mut().filter(|_| view_audio) {
            section(ui, "Audio", |ui| {
                widgets::audio_rows(ui, a, true);
                audio_fx_ui::audio_fx_rows(ui, a, ids, ("layer", li));
            });
        }
        section(ui, "Transition", |ui| {
            param_row(
                ui,
                "Duration",
                &mut layer.transition.duration,
                RowOpts {
                    wheel: Some(Wheel::Log { smallest: 0.05 }),
                    hover: "Fade time when a new clip starts or the layer is cleared (0 = cut)",
                    ..Default::default()
                },
            );
            choice_row(ui, "Mode", ("layer-trans", li), &mut layer.transition.blend_mode, |m| {
                shader_blend(m).is_some()
            });
        });
        section(ui, "Autopilot", |ui| {
            widgets::autopilot_buttons(ui, layer, ids);
            ui.label(
                RichText::new("Moves on when a clip has played its Loops (Clip panel › Autopilot).")
                    .weak()
                    .small(),
            );
        });
        effects_ui::transform_block(ui, &mut self.comp, FxTarget::Layer(li));
        effects_ui::effects_section(ui, &mut self.comp, FxTarget::Layer(li));
    }

    fn preview(&mut self, ui: &mut egui::Ui) {
        let (cw, ch) = self.compositor.size();
        let aspect = cw as f32 / ch as f32;
        let w = ui.available_width();
        let h = (w / aspect).min(ui.available_height());
        let size = vec2(h * aspect, h);
        let (rect, r) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter().image(
            self.comp_tex,
            rect,
            egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        // Output › Show Mouse Pointer: the pointer here, in composition pixels.
        if self.outputs.show_pointer
            && let Some(m) = r.hover_pos()
        {
            let q = (m - rect.min) / rect.size();
            self.outputs.pointer = Some(crate::output::Pointer::Comp([q.x * cw as f32, q.y * ch as f32]));
        }
    }

    /// Files dropped on a clip slot load there (following files fill the
    /// slots to the right); dropped elsewhere they go to [`Self::default_target`].
    ///
    /// X11 drag-and-drop carries no position and the pointer is grabbed during
    /// the drag, so the drop is held until the pointer position updates
    /// (right after the drop) or a short timeout.
    fn handle_drops(&mut self, ctx: &egui::Context) {
        let (dropped, pos): (Vec<PathBuf>, _) = ctx.input(|i| {
            (
                i.raw.dropped_files.iter().map(|f| f.path().to_owned()).collect(),
                i.pointer.latest_pos(),
            )
        });
        if !dropped.is_empty() {
            log::info!("dropped {} file(s)", dropped.len());
            self.pending_drop = Some((dropped, Instant::now(), pos));
        }
        let Some((_, since, pos_at_drop)) = &self.pending_drop else {
            return;
        };
        let moved = pos.is_some() && pos != *pos_at_drop;
        let timed_out = since.elapsed().as_millis() > 400;
        if !moved && !timed_out {
            ctx.request_repaint();
            return;
        }
        let (files, ..) = self.pending_drop.take().unwrap();
        let slot = if moved { pos.and_then(|p| self.slot_at(p)) } else { None };
        let target = slot.unwrap_or_else(|| self.default_target());
        self.load_files(&files, target);
    }

    fn slot_at(&self, p: egui::Pos2) -> Option<(usize, usize)> {
        self.slot_rects
            .iter()
            .find(|(r, _)| p.x >= self.grid_pin && r.contains(p))
            .map(|(_, s)| *s)
    }

    /// Slots the dragged files would fill (the hovered slot and the ones to its right).
    fn drag_targets(&self) -> Vec<(usize, usize)> {
        let Some((p, n)) = self.drag else { return Vec::new() };
        let Some((li, ci)) = self.slot_at(p) else {
            return Vec::new();
        };
        (ci..ci + n.max(1)).map(|c| (li, c)).collect()
    }

    /// Label next to the pointer while files are dragged over the window.
    fn drop_hint(&self, ctx: &egui::Context) {
        let n = ctx.input(|i| i.raw.hovered_files.len());
        if n == 0 {
            return;
        }
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("drop-hint"),
        ));
        let font = egui::FontId::proportional(15.0);
        let (pos, text) = match self.drag {
            Some((p, _)) => {
                let text = match self.slot_at(p) {
                    Some((li, ci)) if n == 1 => format!("Load into layer {} · clip {}", li + 1, ci + 1),
                    Some((li, ci)) => format!("Load {n} clips into layer {} from clip {}", li + 1, ci + 1),
                    None => "Drop on a clip slot".to_string(),
                };
                (p + vec2(16.0, 16.0), text)
            }
            None => {
                // No pointer position (e.g. Wayland): centred hint.
                let r = ctx.content_rect();
                painter.rect_filled(r, 0.0, theme::cur().overlay.gamma_multiply(0.5));
                (r.center(), "Drop on a clip slot to load".to_string())
            }
        };
        let galley = painter.layout_no_wrap(text, font, theme::cur().overlay_text);
        let rect = egui::Rect::from_min_size(pos, galley.size()).expand(6.0);
        painter.rect_filled(rect, 4.0, theme::cur().overlay);
        painter.galley(pos, galley, theme::cur().overlay_text);
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let prefs = KeyboardShortcut::new(Modifiers::COMMAND, Key::Comma);
        if ctx.input_mut(|i| i.consume_shortcut(&prefs)) {
            self.prefs_win.open = !self.prefs_win.open;
        }
        let save_as = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::S);
        if ctx.input_mut(|i| i.consume_shortcut(&save_as)) {
            self.save_as_dialog();
        }
        let save = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
        let output = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::O);
        if ctx.input_mut(|i| i.consume_shortcut(&save)) {
            self.save();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&output)) {
            let m = if self.outputs.is_on() {
                OutputMode::Disabled
            } else {
                OutputMode::Windowed(0)
            };
            self.outputs.set_mode(m);
        }
        self.output_shortcuts(ctx);
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::R))) {
            self.toggle_recording();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::P))) {
            self.snapshot();
        }
        for (k, kind) in [
            (Key::K, shortcuts::LearnKind::Keyboard),
            (Key::M, shortcuts::LearnKind::Midi),
            (Key::D, shortcuts::LearnKind::Dmx),
        ] {
            if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, k))) {
                if self.learn.as_ref().is_some_and(|l| l.kind == kind) {
                    self.learn = None;
                } else {
                    self.start_learn(kind);
                }
            }
        }
        let open = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
        if ctx.input_mut(|i| i.consume_shortcut(&open)) {
            self.open_dialog();
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        self.clipboard_shortcuts(ctx);
        self.structure_shortcuts(ctx);
        if !self.selection.is_empty() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete)) {
            self.eject_selection();
        }
        let redo = ctx.input_mut(|i| {
            i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z))
                || i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Y))
        });
        if redo {
            self.undo(true);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Z))) {
            self.undo(false);
        }
        // Default map (Space = tap tempo, ←/→ = previous / next clip, Q…I, Shift+1…9…).
        self.key_shortcuts(ctx);
    }
}

impl eframe::App for LavmfxApp {
    /// Runs every frame, also while the main window is hidden. eframe runs no
    /// ui pass for a hidden window (minimized, covered, its display switched
    /// off by energy saving), and everything (playback, the compositor, the
    /// output windows) happens in that pass. A VJ app has to keep feeding its
    /// outputs then, so ask for the full frame anyway.
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        crate::present::apply(frame);
        if ctx.input(|i| i.viewport().visible()) == Some(false) {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
            ctx.request_repaint();
        }
    }

    /// Debug: `LAVMFX_SCREENSHOT_WHEEL="x,y"` keeps the pointer at x,y and scrolls down
    /// (with `LAVMFX_SCREENSHOT`), to check panel scrolling in self-screenshots;
    /// `LAVMFX_SCREENSHOT_WHEEL_RIGHT` scrolls right instead.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if std::env::var_os("LAVMFX_SCREENSHOT").is_some() {
            live::probe_keys(self.started, raw);
        }
        self.learn_raw_input(raw);
        if std::env::var_os("LAVMFX_SCREENSHOT").is_none() {
            return;
        }
        let Some((x, y)) = std::env::var("LAVMFX_SCREENSHOT_WHEEL").ok().and_then(|v| {
            v.split_once(',')
                .and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)))
        }) else {
            return;
        };
        let pos = egui::pos2(x, y);
        raw.events.push(egui::Event::PointerMoved(pos));
        raw.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: if std::env::var_os("LAVMFX_SCREENSHOT_WHEEL_RIGHT").is_some() {
                egui::vec2(-1.0, 0.0)
            } else {
                egui::vec2(0.0, -1.0)
            },
            modifiers: Default::default(),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
        });
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Ok(l) = std::env::var("LAVMFX_SCREENSHOT_LAYER")
            && let Ok(l) = l.parse::<usize>()
            && self.selected_layer.is_none()
        {
            self.selected_layer = Some(l);
        }
        prof::frame();
        widgets::guard_wheel(&ctx);
        if self.monitors_polled.is_none_or(|t| t.elapsed().as_secs_f32() > 2.0) {
            self.monitors_polled = Some(Instant::now());
            self.outputs.poll_monitors(frame);
        }
        self.fps.tick();
        let (hovering, dropping) = ctx.input(|i| (i.raw.hovered_files.len(), !i.raw.dropped_files.is_empty()));
        self.drag = if hovering > 0 || dropping {
            let n = hovering.max(ctx.input(|i| i.raw.dropped_files.len()));
            ctx.input(|i| i.pointer.latest_pos())
                .filter(|_| ctx.input(|i| i.pointer.is_moving()))
                .map(|p| (p, n))
        } else {
            None
        };
        if self.theme_applied.as_ref() != Some(&self.prefs.appearance) {
            theme::apply(&ctx, &self.prefs.appearance);
            self.theme_applied = Some(self.prefs.appearance.clone());
        }
        self.audio.sync(&self.prefs.audio);
        if self.prefs_win.open {
            self.prefs_win.audio.status = self.audio.status();
            self.prefs_win.audio.out_status = self.audio.out_status();
            self.prefs_win.audio.spectrum = self.audio.external();
        }
        if prefs::ffgl_rescan_wanted(false) {
            let _ = self.ffgl.reload(&self.prefs.folders.ffgl_plugins);
        }
        if prefs::show(&ctx, &mut self.prefs_win, &mut self.prefs) {
            self.prefs.apply_globals();
            if let Err(e) = self.prefs.save() {
                self.status = format!("Can't save preferences: {e}");
            }
        }
        self.about_window(&ctx);
        prof::mark("prefs/audio sync");
        self.quit_confirmation(&ctx);
        self.learn_begin(&ctx);
        self.shortcuts(&ctx);
        self.handle_drops(&ctx);
        self.poll_dialog();
        self.poll_composition_dialog();
        self.poll_save_dialog(&ctx);
        prof::mark("input/dialogs");
        self.thumbs.poll(&ctx);
        if self.waves.poll() {
            ctx.request_repaint();
        }
        prof::mark("thumbs");
        self.ensure_layers();
        // There is always an empty column on the far right.
        self.comp.ensure_spare_column();
        prof::mark("layers/spare column");
        self.tick_remote();
        prof::mark("remote (OSC/REST)");
        self.tick_midi(&ctx);
        self.tick_dmx_in();
        prof::mark("midi");
        self.tick_link();
        prof::mark("link");
        self.tick_smpte();
        prof::mark("midi/link/smpte");
        self.tick_launches();
        self.poll_openings();
        prof::mark("launches/openings");
        self.sync_resolution();
        // Parameter drive (P menu) before playback, so animated speeds apply this frame.
        let transitions = (0..self.comp.layers.len())
            .map(|li| {
                self.outgoing
                    .get(li)
                    .and_then(Option::as_ref)
                    .map_or(0.0, |o| o.progress() as f64)
            })
            .collect();
        if std::mem::take(&mut self.prefs_win.audio.retry) {
            self.audio.retry();
        }
        // Global FFT threshold first; each drive's own Thresh comes after.
        let thr = self.prefs.audio.fft_threshold;
        let gate = |s: &lavmfx_core::fft::Spectrum| lavmfx_core::fft::gate_spectrum(s, thr);
        self.comp.drive(&lavmfx_core::drive::DriveFrame {
            dt: self.last_dt,
            beats: self.clock.beats(),
            transitions,
            external: gate(&self.audio.external()),
            composition: gate(&self.audio.composition()),
            layers: self.audio.layers(self.comp.layers.len()).iter().map(gate).collect(),
        });
        prof::mark("drive");
        self.tick_playback();
        self.tick_autopilot();
        prof::mark("playback");
        self.tick_audio();
        self.tick_preview(self.last_dt);
        prof::mark("audio/preview");

        // Render the composition once per UI frame; every view samples this texture.
        self.compositor.set_groups(
            self.comp
                .group_mix()
                .into_iter()
                .map(|g| lavmfx_render::GroupDraw {
                    opacity: g.opacity,
                    blend: g.blend,
                })
                .collect(),
        );
        self.refresh_router_inputs();
        self.outputs.bypassed = self.bypassed_routes();
        self.outputs.outline = self.slice_outline();
        let mut extra = self.outputs.wanted_routes();
        // A routed clip on the Preview Monitor reads the main composition's copy.
        if let Some(pb) = &self.preview
            && let lavmfx_render::LayerInput::Route(r) = self.clip_input(Some(pb.slot))
        {
            extra.push(r);
        }
        self.compositor.set_extra_routes(extra);
        prof::mark("routes");
        let mut fx = self.fx_frame();
        self.apply_chase(&mut fx);
        self.isf.prepare(&self.gpu.device, &self.gpu.queue, &fx);
        self.ffgl.set_bpm(self.clock.bpm);
        self.compositor.set_effects(fx);
        prof::mark("fx_frame");
        let mut layer_draws = self.layer_draws();
        prof::mark("layer_draws");
        // The Slice Outline layer goes to the output only (rendered as a route).
        if let Some((li, _)) = self.outputs.outline
            && let Some(d) = layer_draws.get_mut(li)
        {
            d.opacity = 0.0;
        }
        for (li, d) in layer_draws.iter().enumerate() {
            if d.input != lavmfx_render::LayerInput::Frame {
                self.compositor.ensure_layer(&self.gpu.device, li);
            }
        }
        // Output › Show Test Card: the card replaces the composition picture.
        let card = self.outputs.test_card.then(|| self.compositor.size());
        if card != self.test_card_shown {
            self.test_card_shown = card;
            let sub = card.map(|(w, h)| test_card_subtitle(w, h)).unwrap_or_default();
            self.compositor
                .set_test_card(&self.gpu.queue, card.is_some(), "Test Card", &sub);
        }
        self.compositor.render(
            &self.gpu.device,
            &self.gpu.queue,
            FrameInput {
                time: (self.clock.seconds() * self.comp.speed.value) as f32,
                beat: self.clock.beats() as f32,
                master: self.comp.master.value as f32,
            },
            &layer_draws,
        );

        prof::mark("render");
        Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        if self.view.toolbar {
            Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        }
        if self.prefs_dirty.is_some_and(|t| t.elapsed().as_secs_f32() > 1.0) {
            self.prefs_dirty = None;
            if let Err(e) = self.prefs.save() {
                self.status = format!("Can't save preferences: {e}");
            }
        }
        Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        self.grid_view = egui::Rect::NOTHING; // set again if the grid is drawn
        self.preview_fx_keep = false;
        prof::mark("menu/toolbar/status");
        egui::CentralPanel::no_frame().show(ui, |ui| dock::show(self, ui));
        prof::mark("dock panels");
        // A click anywhere but the auditioned browser item / the FX toggle
        // switches the Preview Monitor effect off.
        if self.preview_fx_on && !self.preview_fx_keep && ctx.input(|i| i.pointer.primary_clicked()) {
            self.preview_fx_on = false;
        }
        self.effect_drop_on_grid(&ctx);
        self.layout_dialog(&ctx);
        self.rename_dialog(&ctx);
        self.import_report_dialog(&ctx);
        self.autosave_layout();

        self.drop_hint(&ctx);
        debug_screenshot(&ctx, self.started);
        self.outputs.render_all = self.screens_editor.is_some();
        prof::mark("dialogs");
        self.outputs.render(&self.gpu, &self.compositor);
        self.live_out.configure(
            self.prefs.ndi.output.then_some(self.prefs.ndi.name.as_str()),
            self.prefs.share.output.then_some(self.prefs.share.name.as_str()),
            self.comp.resolution(),
        );
        self.tick_recording();
        self.tick_render();
        self.tick_dmx();
        self.live_out
            .tick(&self.gpu.device, &self.gpu.queue, self.compositor.texture());
        if self.prefs_win.open {
            self.prefs_win.ndi_status = self.live_out.status(crate::live_out::Kind::Ndi);
            self.prefs_win.share_status = self.live_out.status(crate::live_out::Kind::Share);
        }
        prof::mark("outputs render");
        if let Some(st) = self.outputs.status.take() {
            self.status = st;
        }
        self.screens_editor_window(&ctx);
        self.media_manager_window(&ctx);
        self.slice_chase_window(&ctx);
        self.outputs.show(&ctx, self.comp_tex, self.fps.fps);
        self.learn_end(&ctx);
        prof::mark("outputs/editor");
        self.track_history(&ctx);
        prof::mark("history");
        // What follows is eframe drawing and presenting the windows; a stall
        // after this mark is in the GPU driver's present.
        prof::mark("ui end (present next)");
        ctx.request_repaint();
    }
}

// ---------------------------------------------------------------- widgets

const LAYER_HEADER_W: f32 = 176.0;
/// Width of the transition slider column right of the layer header.
const TRANSITION_W: f32 = 48.0;
/// Fader column right of the layer header: M / A / V faders (View › Audio
/// controls), else one wide master fader.
const AV_W: f32 = 3.0 * FADER_W + 2.0 * 2.0;
const FADER_W: f32 = 18.0;

/// Box selection on the clip grid.
struct Marquee {
    /// Where the drag started, relative to the grid's content origin (so
    /// the box keeps its corner when the grid scrolls).
    start: egui::Vec2,
    /// Selection to add the boxed slots to (Ctrl / Shift held at the start).
    base: BTreeSet<(usize, usize)>,
}

/// Lays out `add` in a `size` cell at the row's left that stays in view when
/// the grid scrolls sideways by `sx`. The rest of the row is clipped to the
/// cell's right, so slots slide under it instead of over it.
fn pinned_cell<R>(ui: &mut egui::Ui, size: egui::Vec2, sx: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let (_, rect) = ui.allocate_space(size);
    let rect = rect.translate(vec2(sx, 0.0));
    let mut cell = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    cell.set_width(size.x);
    let r = add(&mut cell);
    let clip = ui.clip_rect();
    ui.set_clip_rect(clip.with_min_x(clip.left().max(rect.right())));
    r
}

/// Width of the layer header plus its optional columns.
fn header_extra_w(view: dock::ViewOptions, gap: f32) -> f32 {
    LAYER_HEADER_W + AV_W + gap + if view.layer_transition { TRANSITION_W + gap } else { 0.0 }
}

/// Colour marking a layer group (band and member stripe).
fn group_color(gi: usize) -> Color32 {
    const C: [Color32; 4] = [
        Color32::from_rgb(200, 140, 60),
        Color32::from_rgb(110, 150, 230),
        Color32::from_rgb(170, 110, 210),
        Color32::from_rgb(90, 180, 120),
    ];
    C[gi % C.len()]
}

struct BandActions {
    select: bool,
    menu: Option<structure::GridAction>,
    /// The band's row (drop target while dragging layers).
    rect: egui::Rect,
}

/// Group row above its layers: fold arrow, name (click = settings,
/// right-click = Ungroup), B, S, master.
fn group_band(
    ui: &mut egui::Ui,
    gi: usize,
    g: &mut lavmfx_core::LayerGroup,
    selected: bool,
    view: dock::ViewOptions,
    sx: f32,
) -> BandActions {
    let mut out = BandActions {
        select: false,
        menu: None,
        rect: egui::Rect::NOTHING,
    };
    let gap = ui.spacing().item_spacing.x;
    let w = header_extra_w(view, gap);
    out.rect = ui
        .horizontal(|ui| {
            pinned_cell(ui, vec2(w, 20.0), sx, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.spacing_mut().button_padding = vec2(3.0, 0.0);
                let color = group_color(gi);
                // Fold arrow, drawn (▸ / ▾ aren't in the default font at small sizes).
                let (r, resp) = ui.allocate_exact_size(vec2(16.0, 18.0), egui::Sense::click());
                let c = r.center();
                let pts = if g.folded {
                    vec![
                        pos2(c.x - 3.0, c.y - 5.0),
                        pos2(c.x + 4.0, c.y),
                        pos2(c.x - 3.0, c.y + 5.0),
                    ]
                } else {
                    vec![
                        pos2(c.x - 5.0, c.y - 3.0),
                        pos2(c.x + 5.0, c.y - 3.0),
                        pos2(c.x, c.y + 4.0),
                    ]
                };
                ui.painter()
                    .add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
                if resp
                    .on_hover_text(if g.folded { "Unfold group" } else { "Fold group" })
                    .clicked()
                {
                    g.folded = !g.folded;
                }
                let (fill, text) = if selected {
                    (color, theme::cur().on_accent)
                } else {
                    (color.gamma_multiply(0.35), ui.visuals().text_color())
                };
                let name = egui::Button::new(RichText::new(&g.name.value).strong().color(text))
                    .fill(fill)
                    .truncate();
                let r = ui
                    .add_sized([86.0, 18.0], name)
                    .on_hover_text("Click: group settings · right-click: group menu");
                out.select = r.clicked() || r.secondary_clicked();
                r.context_menu(|ui| {
                    if ui.button("Rename…").clicked() {
                        out.menu = Some(structure::GridAction::Rename(structure::RenameTarget::Group(gi)));
                        ui.close();
                    }
                    if ui.button("Ungroup").clicked() {
                        out.menu = Some(structure::GridAction::RemoveGroup(gi));
                        ui.close();
                    }
                });
                toggle(ui, &mut g.bypassed.value, "B", "Bypass group");
                toggle(ui, &mut g.solo.value, "S", "Solo group");
                ui.spacing_mut().slider_width = ui.available_width() - 4.0;
                let mut v = g.master.value;
                let r = wheel_slider(ui, &mut v, 0.0..=1.0, Wheel::Linear, false)
                    .on_hover_text("Group master (double-click: 100 %)");
                g.master.set(if r.double_clicked() { 1.0 } else { v });
            });
        })
        .response
        .rect
        .translate(vec2(sx, 0.0));
    out
}

/// What was clicked in a layer header.
#[derive(Default)]
struct HeaderActions {
    clear: bool,
    select: bool,
    /// New play direction for the layer's playing clip (0 back, 1 pause, 2 forward).
    direction: Option<usize>,
    /// Previous (false) / next (true) clip button.
    skip: Option<bool>,
    /// Picked from the name strip's right-click menu.
    menu: Option<structure::GridAction>,
    /// The name strip started being dragged (reorder layers).
    drag: bool,
}

/// Layer controls left of its clips (layer header):
/// row 1 `X B S A/B blend`, row 2 master + mini transport, row 3 the name
/// strip (click = select, right-click = layer menu); then (View › Layer
/// transition) a vertical transition-time slider whose label opens the
/// transition mode.
#[allow(clippy::too_many_arguments)]
fn layer_header(
    ui: &mut egui::Ui,
    li: usize,
    menu_ctx: &structure::LayerMenuCtx,
    layer: &mut lavmfx_core::Layer,
    ids: &mut lavmfx_core::IdGen,
    playing: bool,
    direction: Option<i32>,
    selected: bool,
    view: dock::ViewOptions,
) -> HeaderActions {
    let mut out = HeaderActions::default();
    ui.allocate_ui_with_layout(
        vec2(LAYER_HEADER_W, CLIP_H),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_width(LAYER_HEADER_W);
            ui.spacing_mut().item_spacing = vec2(2.0, 1.0);
            ui.spacing_mut().interact_size.y = 16.0;
            ui.spacing_mut().button_padding = vec2(3.0, 0.0);
            // Block: big ✕ | B S A/B squares over the blend menu.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                let x = widgets::icon_button(ui, widgets::Icon::Close, false, vec2(30.0, 44.0), playing)
                    .on_hover_text("Clear layer (stop its clip)");
                learn_target(
                    ui,
                    x.rect,
                    LearnTarget::Address(format!("/composition/layers/{}/clear", li + 1)),
                );
                out.clear = x.clicked();
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                    ui.horizontal(|ui| {
                        let sq = vec2(34.0, 22.0);
                        let b = widgets::toggle_box(ui, &mut layer.bypassed.value, "B", "Bypass layer", sq);
                        learn_target(ui, b.rect, LearnTarget::Param(layer.bypassed.id));
                        let s = widgets::toggle_box(ui, &mut layer.solo.value, "S", "Solo layer", sq);
                        learn_target(ui, s.rect, LearnTarget::Param(layer.solo.id));
                        if !view.crossfader {
                            // Hidden (View › Crossfader): keep its space so B / S don't grow.
                            ui.allocate_space(vec2(ui.available_width(), 22.0));
                            return;
                        }
                        let g = &mut layer.crossfadergroup;
                        let mut ab = g.index > 0;
                        let label = ["A/B", "A", "B"].get(g.index as usize).copied().unwrap_or("A/B");
                        let before = ab;
                        widgets::toggle_box(
                            ui,
                            &mut ab,
                            label,
                            "Crossfader side (click: none → A → B)",
                            vec2(ui.available_width(), 22.0),
                        );
                        if ab != before {
                            // Any click cycles none → A → B.
                            g.select((g.index as usize + 1) % 3);
                        }
                    });
                    ui.spacing_mut().interact_size.y = 20.0;
                    ui.spacing_mut().combo_width = ui.available_width();
                    if let Some(b) = layer.blend_mode_mut(ids) {
                        choice_combo_with(ui, ("hdr-blend", li), "Layer blend mode", b, |m| {
                            shader_blend(m).is_some()
                        });
                    }
                });
            });
            ui.horizontal(|ui| {
                // Master lives in the fader column (`layer_faders`).
                ui.set_min_height(20.0);
                if view.layer_transport {
                    for (i, (icon, hover)) in [
                        (widgets::Icon::Back, "Play backward"),
                        (widgets::Icon::Pause, "Pause"),
                        (widgets::Icon::Forward, "Play forward"),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let on = direction == Some(i as i32);
                        let r = widgets::icon_button(ui, icon, on, vec2(18.0, 16.0), direction.is_some());
                        if r.on_hover_text(hover).clicked() {
                            out.direction = Some(i);
                        }
                    }
                    ui.add_space(4.0);
                    for (fwd, icon, hover) in [
                        (false, widgets::Icon::PrevClip, "Previous clip (←)"),
                        (true, widgets::Icon::NextClip, "Next clip (→)"),
                    ] {
                        if widgets::icon_button(ui, icon, false, vec2(18.0, 16.0), true)
                            .on_hover_text(hover)
                            .clicked()
                        {
                            out.skip = Some(fwd);
                        }
                    }
                }
            });
            // Name strip: the whole width is the click target.
            let (fill, text) = if selected {
                (theme::cur().accent, theme::cur().on_accent)
            } else {
                (ui.visuals().widgets.inactive.weak_bg_fill, ui.visuals().text_color())
            };
            let name = egui::Button::new(RichText::new(&layer.name.value).strong().color(text))
                .fill(fill)
                .truncate();
            let r = ui
                .add_sized([LAYER_HEADER_W, NAME_H], name)
                .on_hover_text("Click: layer settings · drag: reorder · right-click: layer menu");
            let r = r.interact(egui::Sense::drag());
            learn_target(
                ui,
                r.rect,
                LearnTarget::Address(format!("/composition/layers/{}/select", li + 1)),
            );
            out.select = r.clicked() || r.secondary_clicked();
            out.drag = r.drag_started();
            r.context_menu(|ui| {
                out.menu = structure::layer_menu(ui, Some(li), menu_ctx);
            });
        },
    );
    layer_faders(ui, layer, view.audio_controls);
    if !view.layer_transition {
        return out;
    }
    // Transition time: vertical slider (wheel-friendly); its label opens the mode list.
    ui.allocate_ui_with_layout(
        vec2(TRANSITION_W, CLIP_H),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            // Full width even when the label is short, so slot columns line
            // up with the column headers (which reserve TRANSITION_W).
            ui.set_width(TRANSITION_W);
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.spacing_mut().button_padding = vec2(1.0, 0.0);
            ui.spacing_mut().slider_width = CLIP_H - 6.0;
            let d = &mut layer.transition.duration;
            let mut secs = d.value;
            if wheel_slider(ui, &mut secs, d.min..=d.max, Wheel::Log { smallest: 0.05 }, true)
                .on_hover_text("Transition time (0 = cut). Wheel: adjust, Shift: fine")
                .changed()
            {
                d.set(secs);
            }
            let t = if d.value <= 0.0 {
                "cut".to_owned()
            } else {
                format!("{:.1}s", d.value)
            };
            let mode = &mut layer.transition.blend_mode;
            let hover = format!("Transition mode: {}", mode.value);
            ui.menu_button(RichText::new(t).small().weak(), |ui| {
                egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                    for i in 0..mode.options.len() {
                        let name = mode.options[i].clone();
                        let ok = shader_blend(&name).is_some();
                        let text = if ok {
                            RichText::new(&name)
                        } else {
                            RichText::new(&name).weak()
                        };
                        if ui.selectable_label(mode.index == i as i32, text).clicked() {
                            mode.select(i);
                            ui.close();
                        }
                    }
                });
            })
            .response
            .on_hover_text(hover);
        },
    );
    out
}

/// Fader column: with View › Audio controls three faders
/// **M** (master), **A** (volume, pink) and **V** (opacity, green), the letter
/// on each knob; a click on the A / V knob switches the layer's audio / video
/// off (grey knob) and on. Without audio controls: one **V** (video opacity)
/// fader as wide as the column (it used to be the master: imported layers
/// with opacity 0 couldn't be brought up from the header).
fn layer_faders(ui: &mut egui::Ui, layer: &mut lavmfx_core::Layer, audio_controls: bool) {
    let pal = theme::cur();
    ui.allocate_ui_with_layout(
        vec2(AV_W, CLIP_H),
        egui::Layout::left_to_right(egui::Align::Min),
        |ui| {
            ui.set_width(AV_W);
            ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
            let h = CLIP_H - 2.0;
            let fader_w = FADER_W;
            let master = |ui: &mut egui::Ui, layer: &mut lavmfx_core::Layer, w: f32, label: &str, color| {
                let mut v = layer.master.value;
                let id = layer.master.id;
                let r = widgets::cap_fader(ui, &mut v, 0.0..=1.0, vec2(w, h), label, color).on_hover_text(format!(
                    "Layer master {:.0} % (double-click: 100 %)",
                    layer.master.value * 100.0
                ));
                layer.master.set(if r.double_clicked() { 1.0 } else { v });
                learn_target(ui, r.rect, LearnTarget::Param(id));
            };
            let opacity = |ui: &mut egui::Ui, layer: &mut lavmfx_core::Layer, w: f32| {
                let v_col = if layer.video_off { pal.button } else { pal.accent };
                let Some(o) = layer.video.as_mut().map(|v| &mut v.track.opacity) else {
                    ui.add_space(w);
                    return;
                };
                let mut v = o.value;
                let r = widgets::cap_fader(ui, &mut v, o.min..=o.max, vec2(w, h), "V", v_col).on_hover_text(format!(
                    "Layer opacity {:.0} % · click: video on/off · double-click: 100 %",
                    o.value * 100.0
                ));
                o.set(if r.double_clicked() { o.max } else { v });
                learn_target(ui, r.rect, LearnTarget::Param(o.id));
                if r.clicked() {
                    layer.video_off = !layer.video_off;
                }
            };
            if !audio_controls {
                opacity(ui, layer, AV_W);
                return;
            }
            master(ui, layer, fader_w, "M", pal.button);
            // A: volume (dB); knob position = √(amplitude / max), 0 dB a bit below the middle.
            let a_col = if layer.audio_off { pal.button } else { pal.audio };
            if let Some(a) = layer.audio.as_mut() {
                let vol = &mut a.volume;
                let amp_max = 10f64.powf(vol.max / 20.0);
                let mut t = if vol.value <= vol.min {
                    0.0
                } else {
                    (10f64.powf(vol.value / 20.0) / amp_max).sqrt()
                };
                let r = widgets::cap_fader(ui, &mut t, 0.0..=1.0, vec2(fader_w, h), "A", a_col).on_hover_text(format!(
                    "Layer volume {:.1} dB · click: audio on/off · double-click: 0 dB",
                    vol.value
                ));
                if r.double_clicked() {
                    vol.set(0.0);
                } else if r.changed() {
                    let db = if t <= 1e-4 {
                        vol.min
                    } else {
                        20.0 * (t * t * amp_max).log10()
                    };
                    vol.set(db.clamp(vol.min, vol.max));
                }
                if r.clicked() {
                    layer.audio_off = !layer.audio_off;
                }
                learn_target(ui, r.rect, LearnTarget::Param(vol.id));
            } else {
                ui.add_space(fader_w);
            }
            opacity(ui, layer, fader_w);
        },
    );
}

/// Something clicked in the clip panel, applied after drawing it.
enum ClipAction {
    /// Seek to seconds; `true` while still dragging.
    Scrub(f64, bool),
    Restart,
    Step(i32),
    Stop,
    Trigger,
}

/// Clock: `SS.cc`, or `M:SS.cc` from a minute on.
fn fmt_clock(s: f64) -> String {
    let s = s.max(0.0);
    if s < 60.0 {
        format!("{s:05.2}")
    } else {
        format!("{}:{:05.2}", (s / 60.0).floor() as u64, s % 60.0)
    }
}

/// Clip name over its thumbnail (dimmed), at the top of the Clip panel.
/// The clip's video-source box: thumbnail, file and format, and the
/// Resize buttons (Fit / Fill / Stretch / Original) in their own container.
fn video_container(ui: &mut egui::Ui, v: &mut lavmfx_core::VideoTrackClip, thumb: Option<egui::TextureId>) {
    egui::Frame::new()
        .fill(theme::cur().accent_dim)
        .corner_radius(3.0)
        .inner_margin(4.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let size = vec2(64.0, 36.0);
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                ui.painter().rect_filled(rect, 2.0, theme::cur().video_empty);
                if let Some(t) = thumb {
                    ui.painter().image(
                        t,
                        rect,
                        egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                // Resize buttons, right-aligned in their own box.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::Frame::new()
                        .fill(theme::cur().panel)
                        .corner_radius(3.0)
                        .inner_margin(2.0)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = 1.0;
                            let modes = [
                                ("Orig", 3, "Original size: 1 clip pixel = 1 composition pixel"),
                                ("Stretch", 2, "Stretch to the composition (ignores aspect)"),
                                ("Fill", 0, "Fill the composition, crop the overflow"),
                                ("Fit", 1, "Fit inside the composition, letterboxed"),
                            ];
                            for (label, idx, hover) in modes {
                                let on = v.resize.index == idx;
                                if ui
                                    .add(egui::Button::new(RichText::new(label).small()).selected(on))
                                    .on_hover_text(hover)
                                    .clicked()
                                {
                                    v.resize.select(idx as usize);
                                }
                            }
                        });
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        let (name, info) = match &v.fileinfo {
                            Some(f) => {
                                let name = Path::new(&f.path)
                                    .file_name()
                                    .map_or_else(|| f.path.clone(), |n| n.to_string_lossy().into_owned());
                                let fps = if f.framerate.denom > 0 && f.framerate.num > 0 {
                                    format!(" · {:.2} fps", f.framerate.num as f64 / f.framerate.denom as f64)
                                } else {
                                    String::new()
                                };
                                let info = if lavmfx_media::is_audio_file(Path::new(&f.path)) {
                                    format!("audio · {}", f.duration)
                                } else {
                                    format!("{}×{}{fps} · {}", f.width, f.height, f.duration)
                                };
                                (name, info)
                            }
                            None => ("(no file)".to_owned(), String::new()),
                        };
                        ui.add(egui::Label::new(RichText::new(name).strong()).truncate());
                        ui.add(egui::Label::new(RichText::new(info).small().weak()).truncate());
                    });
                });
            });
        });
}

fn clip_banner(ui: &mut egui::Ui, name: &mut String, thumb: Option<egui::TextureId>, wave: Option<&[f32]>, sub: &str) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 34.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, theme::cur().video_empty);
    if let Some(t) = thumb {
        // Centre strip of the thumbnail, stretched across the banner.
        let aspect = rect.width() / rect.height() / (16.0 / 9.0);
        let h = (1.0 / aspect).min(1.0);
        let uv = egui::Rect::from_min_max(pos2(0.0, 0.5 - h / 2.0), pos2(1.0, 0.5 + h / 2.0));
        p.image(t, rect, uv, Color32::from_gray(150));
    }
    if let Some(peaks) = wave {
        // The clip's audio, pink (over the thumbnail when there is one).
        let a = if thumb.is_some() { 0.75 } else { 1.0 };
        crate::waves::paint(
            &p,
            rect.shrink2(vec2(0.0, 3.0)),
            peaks,
            theme::cur().audio.gamma_multiply(a),
        );
    }
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(6.0, 6.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.add(
        egui::TextEdit::singleline(name)
            .desired_width((w * 0.45).min(220.0))
            .background_color(theme::cur().overlay),
    );
    child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        egui::Frame::new()
            .fill(theme::cur().overlay)
            .corner_radius(2)
            .inner_margin(egui::Margin::symmetric(4, 1))
            .show(ui, |ui| {
                ui.label(RichText::new(sub).small().color(theme::cur().overlay_text))
            });
    });
}

/// Timeline: track with in/out ends and a position marker. Click or drag
/// to scrub. Returns (new position, still dragging).
fn timeline_bar(ui: &mut egui::Ui, pos: f64, dur: f64, enabled: bool) -> Option<(f64, bool)> {
    let w = ui.available_width();
    let sense = if enabled {
        egui::Sense::click_and_drag()
    } else {
        egui::Sense::hover()
    };
    let (rect, r) = ui.allocate_exact_size(vec2(w, 30.0), sense);
    let p = ui.painter_at(rect.expand(2.0));
    let accent = theme::cur().accent;
    let track = egui::Rect::from_min_max(
        pos2(rect.left() + 6.0, rect.bottom() - 9.0),
        pos2(rect.right() - 6.0, rect.bottom() - 5.0),
    );
    p.rect_filled(track, 2.0, theme::cur().button);
    // In / out ends.
    for (x, dir) in [(track.left(), 1.0f32), (track.right(), -1.0)] {
        let y = track.center().y;
        p.add(egui::Shape::convex_polygon(
            vec![pos2(x, y - 6.0), pos2(x + 6.0 * dir, y + 4.0), pos2(x, y + 4.0)],
            accent,
            egui::Stroke::NONE,
        ));
    }
    let frac = if dur > 0.0 {
        (pos / dur).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let x = track.left() + frac * track.width();
    let top = rect.top() + 4.0;
    let marker = vec![
        pos2(x - 6.0, top),
        pos2(x + 6.0, top),
        pos2(x + 6.0, top + 14.0),
        pos2(x, top + 19.0),
        pos2(x - 6.0, top + 14.0),
    ];
    let c = if enabled { accent } else { theme::cur().text_weak };
    p.add(egui::Shape::convex_polygon(marker, c, egui::Stroke::NONE));
    let hit = r.dragged() || r.clicked() || r.drag_started();
    if !hit || dur <= 0.0 {
        return None;
    }
    let px = r.interact_pointer_pos()?.x;
    let f = ((px - track.left()) / track.width()).clamp(0.0, 1.0) as f64;
    Some((f * dur, r.dragged()))
}

#[derive(Clone, Copy)]
enum TransportIcon {
    Back,
    Pause,
    Forward,
}

/// Clip panel ◀ ⏸ ▶ (big; selected = accent).
fn transport_icon(ui: &mut egui::Ui, icon: TransportIcon, on: bool) -> egui::Response {
    let icon = match icon {
        TransportIcon::Back => widgets::Icon::Back,
        TransportIcon::Pause => widgets::Icon::Pause,
        TransportIcon::Forward => widgets::Icon::Forward,
    };
    widgets::icon_button(ui, icon, on, vec2(30.0, 24.0), true)
}

/// Duration row: the clip's length at its current speed. Changing
/// it changes the speed (`duration = file length / speed`); /2 and *2 halve
/// and double it.
/// SMPTE transport: Offset = the timecode at which the clip is at its start
/// (typed or dragged as HH:MM:SS:FF; "Now" = the input's current timecode).
fn smpte_offset_row(
    ui: &mut egui::Ui,
    off: &mut lavmfx_core::RangeParam,
    fps: lavmfx_core::timecode::Fps,
    now: Option<lavmfx_core::timecode::TimecodeNow>,
) {
    use lavmfx_core::timecode::Timecode;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.allocate_ui_with_layout(
            vec2(LABEL_W, 18.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.set_width(LABEL_W);
                ui.add(egui::Label::new("Offset").selectable(false));
            },
        );
        let mut v = off.value;
        let r = ui
            .add_sized(
                [96.0, 18.0],
                egui::DragValue::new(&mut v)
                    .range(off.min..=off.max)
                    .speed(1.0 / fps.rate())
                    .custom_formatter(move |s, _| Timecode::from_seconds(s, fps).display(fps))
                    .custom_parser(move |t| {
                        Timecode::parse(t)
                            .filter(|tc| tc.is_valid(fps))
                            .map(|tc| tc.seconds(fps))
                    }),
            )
            .on_hover_text("Timecode at which the clip starts (type HH:MM:SS:FF or drag)");
        if r.changed() {
            off.set(v);
        }
        if ui
            .add_enabled(now.is_some(), egui::Button::new(RichText::new("Now").small()))
            .on_hover_text("Start the clip at the current timecode")
            .clicked()
            && let Some(n) = now
        {
            off.set((n.secs * n.fps.rate()).floor() / n.fps.rate());
        }
        if let Some(n) = now {
            let t = n.secs - off.value;
            let text = if t < 0.0 {
                format!("starts in {}", fmt_clock(-t))
            } else {
                format!("clip time {}", fmt_clock(t))
            };
            ui.label(RichText::new(text).small().weak());
        }
    });
}

fn duration_row(ui: &mut egui::Ui, file_dur: f64, duration: &mut lavmfx_core::RangeParam) {
    if file_dur <= 0.0 {
        return;
    }
    if duration.value <= 0.0 {
        duration.set(file_dur);
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.allocate_ui_with_layout(
            vec2(LABEL_W, 18.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.set_width(LABEL_W);
                ui.add(egui::Label::new("Duration").selectable(false))
                    .on_hover_text("Time one pass takes at speed 1 (double-click: the file's length)")
                    .double_clicked()
                    .then(|| duration.set(file_dur));
            },
        );
        let cur = duration.value;
        let mut d = cur;
        ui.add_sized(
            [64.0, 18.0],
            egui::DragValue::new(&mut d)
                .range(0.001..=604_800.0)
                .speed(0.01)
                .suffix(" s")
                .max_decimals(3),
        );
        let fine = ui.input(|i| i.modifiers.shift);
        let step = if fine { 0.01 } else { 0.1 };
        ui.spacing_mut().button_padding = vec2(2.0, 0.0);
        let btn = |ui: &mut egui::Ui, t: &str, hover: &str| {
            ui.add_sized([26.0, 18.0], egui::Button::new(RichText::new(t).small()))
                .on_hover_text(hover)
                .clicked()
        };
        if btn(ui, "−", "Shorter (Shift: fine)") {
            d = cur - step;
        }
        if btn(ui, "+", "Longer (Shift: fine)") {
            d = cur + step;
        }
        if btn(ui, "/2", "Half the duration (plays twice as fast)") {
            d = cur / 2.0;
        }
        if btn(ui, "*2", "Double the duration (plays half as fast)") {
            d = cur * 2.0;
        }
        if (d - cur).abs() > 1e-9 && d > 0.0 {
            duration.set(d.max(0.001));
        }
    });
}

/// Drop position of a clip drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DropAt {
    /// On slot (layer, column): overwrite.
    On(usize, usize),
    /// Between slots in a layer: insert before this column index.
    Between(usize, usize),
}

#[derive(Clone, Copy)]
struct SlotState {
    selected: bool,
    focused: bool,
    /// Playing here, at this fraction of the clip.
    progress: Option<f32>,
    opening: bool,
}

/// Height of the name strip (the trigger handle) under each thumbnail.
const NAME_H: f32 = 18.0;

/// One clip slot: the thumbnail area (trigger / drag handle) and, for
/// filled slots, the name strip below it (select handle). Returns
/// (thumbnail response, name-strip response).
#[allow(clippy::too_many_arguments)]
fn clip_slot(
    ui: &mut egui::Ui,
    (li, ci): (usize, usize),
    clip: &lavmfx_core::Clip,
    thumb: Option<(egui::TextureId, egui::Vec2)>,
    wave: Option<&[f32]>,
    audio_only: bool,
    s: SlotState,
) -> (egui::Response, Option<egui::Response>) {
    let (rect, _) = ui.allocate_exact_size(vec2(CLIP_W, CLIP_H), egui::Sense::hover());
    let pick_rect = egui::Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - NAME_H));
    let name_rect = egui::Rect::from_min_max(pos2(rect.left(), rect.bottom() - NAME_H), rect.max);
    let id = ui.scope_id().with(("slot", li, ci));
    let empty = clip.is_empty();
    let pick = ui.interact(if empty { rect } else { pick_rect }, id, egui::Sense::click_and_drag());
    let name = (!empty).then(|| ui.interact(name_rect, id.with("name"), egui::Sense::click()));
    if !ui.is_rect_visible(rect) {
        return (pick, name);
    }

    let p = ui.painter_at(rect.expand(1.0));
    let base = if empty { theme::cur().field } else { theme::cur().button };
    p.rect_filled(rect, 3.0, if pick.hovered() { base.gamma_multiply(1.4) } else { base });
    if let Some((tex, size)) = thumb {
        // Fit inside the thumbnail area, keeping the aspect ratio.
        let inner = pick_rect.shrink(2.0);
        let scale = (inner.width() / size.x).min(inner.height() / size.y);
        let img = egui::Rect::from_center_size(inner.center(), size * scale);
        p.image(
            tex,
            img,
            egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
    if audio_only {
        // Audio clip: its waveform instead of a thumbnail, pink.
        let inner = pick_rect.shrink2(vec2(4.0, 6.0));
        p.rect_filled(pick_rect.shrink(2.0), 2.0, theme::cur().video_empty);
        match wave {
            Some(peaks) => crate::waves::paint(&p, inner, peaks, theme::cur().audio),
            None => {
                p.text(
                    pick_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "AUDIO",
                    egui::FontId::proportional(13.0),
                    theme::cur().audio,
                );
            }
        }
    }
    if thumb.is_none()
        && let Some(src) = clip.source()
    {
        // Source / effect clip: a badge instead of a thumbnail.
        let label = match src {
            lavmfx_core::sources::ClipSource::Effect => "FX",
            lavmfx_core::sources::ClipSource::Generator(_) => "SOURCE",
            lavmfx_core::sources::ClipSource::Ndi(_) => "NDI",
            lavmfx_core::sources::ClipSource::Share(_) => lavmfx_media::share::backend(),
            lavmfx_core::sources::ClipSource::Capture(_) => "CAMERA",
        };
        p.text(
            pick_rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(13.0),
            theme::cur().text,
        );
    }
    if let Some(name) = &name {
        // Vivid while playing, desaturated otherwise.
        let mut fill = if s.progress.is_some() {
            theme::cur().accent
        } else if s.opening {
            theme::cur().pending
        } else {
            theme::cur().band
        };
        if name.hovered() {
            fill = fill.gamma_multiply(1.3);
        }
        p.rect_filled(
            name_rect,
            egui::CornerRadius {
                nw: 0,
                ne: 0,
                sw: 3,
                se: 3,
            },
            fill,
        );
        if let Some(peaks) = wave {
            // The clip's audio track as a pink waveform in its title strip.
            let c = if s.progress.is_some() {
                theme::cur().audio
            } else {
                theme::cur().audio.gamma_multiply(0.7)
            };
            crate::waves::paint(&p, name_rect.shrink2(vec2(1.0, 2.0)), peaks, c);
        }
        let text = if s.progress.is_some() {
            theme::cur().on_accent
        } else {
            theme::cur().text
        };
        let galley = p.layout(
            clip.name.value.clone(),
            egui::FontId::proportional(10.5),
            text,
            name_rect.width() - 8.0,
        );
        let row = galley.rows.first().map(|r| r.rect().height()).unwrap_or(10.0);
        p.with_clip_rect(name_rect.shrink2(vec2(4.0, 0.0))).galley(
            pos2(name_rect.left() + 4.0, name_rect.center().y - row / 2.0),
            galley,
            text,
        );
        if let Some(frac) = s.progress {
            let mut bar = pick_rect;
            bar.set_top(bar.bottom() - 3.0);
            bar.set_width(bar.width() * frac.clamp(0.0, 1.0));
            p.rect_filled(bar, 0.0, theme::cur().on_accent.gamma_multiply(0.6));
        }
    }
    if clip.is_missing() {
        let red = theme::cur().danger;
        p.text(
            pick_rect.center(),
            egui::Align2::CENTER_CENTER,
            "missing",
            egui::FontId::proportional(11.0),
            red,
        );
        p.rect_stroke(rect, 3.0, egui::Stroke::new(1.5, red), egui::StrokeKind::Inside);
    }
    if s.selected {
        let c = if s.focused {
            theme::cur().text_strong
        } else {
            theme::cur().accent
        };
        p.rect_stroke(rect, 3.0, egui::Stroke::new(2.0, c), egui::StrokeKind::Inside);
    }
    let pick = if empty {
        pick.on_hover_text("Empty slot: drop a video or audio file here")
    } else if clip.is_missing() {
        pick.on_hover_text(format!(
            "File not found:\n{}\n(drop a video or audio file here to replace it)",
            clip.video_path().unwrap_or_default()
        ))
    } else {
        pick.on_hover_text(
            "Click: preview (Preview Monitor) · drag onto a slot: overwrite, between slots: insert (Alt/Ctrl while dragging: copy)",
        )
    };
    let name = name.map(|n| {
        n.on_hover_text(format!(
            "{}\nClick: play live · Ctrl/Shift-click on the thumbnail: multi-select",
            clip.name.value
        ))
    });
    (pick, name)
}

/// (playdirection, playmode, playmodeaway, speed) of either transport kind.
/// BPM Sync fields of a clip transport (see [`beats_rows`]).
struct SyncFields<'a> {
    beats: &'a mut lavmfx_core::RangeParam,
    mode: &'a mut lavmfx_core::ChoiceParam,
    bpm: &'a mut Option<lavmfx_core::RangeParam>,
    beatloop: &'a mut lavmfx_core::ChoiceParam,
}

/// How a clip transport sets its loop length.
enum Length<'a> {
    /// Timeline: Duration in seconds (at speed 1).
    Seconds(&'a mut lavmfx_core::RangeParam),
    /// BPM Sync: beats or clip BPM.
    Beats(SyncFields<'a>),
}

#[allow(clippy::type_complexity)]
fn transport_controls_mut(
    t: &mut lavmfx_core::Transport,
) -> (
    &mut lavmfx_core::ChoiceParam,
    &mut lavmfx_core::ChoiceParam,
    &mut lavmfx_core::ChoiceParam,
    &mut lavmfx_core::RangeParam,
    Length<'_>,
    Option<&mut lavmfx_core::RangeParam>,
) {
    match t {
        lavmfx_core::Transport::Timeline(t) => {
            let c = &mut t.controls;
            (
                &mut c.playdirection,
                &mut c.playmode,
                &mut c.playmodeaway,
                &mut c.speed,
                Length::Seconds(&mut c.duration),
                c.smpteoffset.as_mut(),
            )
        }
        lavmfx_core::Transport::BpmSync(t) => {
            let c = &mut t.controls;
            (
                &mut c.playdirection,
                &mut c.playmode,
                &mut c.playmodeaway,
                &mut c.speed,
                Length::Beats(SyncFields {
                    beats: &mut c.duration,
                    mode: &mut c.syncmode,
                    bpm: &mut c.bpm,
                    beatloop: &mut c.beatloop,
                }),
                None,
            )
        }
    }
}

/// BPM Sync rows: Beats (− + /2 *2, resulting loop time and speed) or the
/// clip's BPM, then Sync Mode and Beat Loop (only Off works yet).
fn beats_rows(ui: &mut egui::Ui, f: SyncFields<'_>, file_s: f64, bpm: f64, clip_id: Id) {
    let by_bpm = f.mode.value == "BPM";
    if by_bpm && let Some(p) = f.bpm.as_mut() {
        param_row(
            ui,
            "Clip BPM",
            p,
            RowOpts {
                hover: "Tempo the clip was made at; it plays at composition BPM ÷ clip BPM",
                ..Default::default()
            },
        );
    } else {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.allocate_ui_with_layout(
                vec2(LABEL_W, 18.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.set_width(LABEL_W);
                    ui.add(egui::Label::new("Beats").selectable(false));
                },
            );
            let b = &mut *f.beats;
            let mut v = b.value;
            ui.add_sized(
                [64.0, 18.0],
                egui::DragValue::new(&mut v)
                    .range(b.min..=b.max)
                    .speed(0.1)
                    .max_decimals(3),
            )
            .on_hover_text("Loop length: the whole file plays in this many beats");
            ui.spacing_mut().button_padding = vec2(2.0, 0.0);
            let btn = |ui: &mut egui::Ui, t: &str, hover: &str| {
                ui.add_sized([26.0, 18.0], egui::Button::new(RichText::new(t).small()))
                    .on_hover_text(hover)
                    .clicked()
            };
            if btn(ui, "−", "One beat shorter") {
                v -= 1.0;
            }
            if btn(ui, "+", "One beat longer") {
                v += 1.0;
            }
            if btn(ui, "/2", "Half the beats (double speed)") {
                v /= 2.0;
            }
            if btn(ui, "*2", "Double the beats (half speed)") {
                v *= 2.0;
            }
            if v != b.value && v > 0.0 {
                b.set(v);
            }
        });
    }
    // What that means right now.
    let rate = if by_bpm {
        bpm / f.bpm.as_ref().map_or(120.0, |b| b.value).max(1e-3)
    } else if file_s > 0.0 {
        file_s / (f.beats.value * 60.0 / bpm.max(1e-3))
    } else {
        1.0
    };
    ui.horizontal(|ui| {
        ui.add_space(LABEL_W + 3.0);
        let loop_s = if rate > 0.0 { file_s / rate } else { 0.0 };
        ui.label(
            RichText::new(format!("loop {loop_s:.2} s · {rate:.2}× at {bpm:.1} BPM"))
                .small()
                .weak(),
        );
    });
    choice_row(ui, "Sync Mode", ("syncmode", clip_id), f.mode, |_| true);
    choice_row(ui, "Beat Loop", ("beatloop", clip_id), f.beatloop, |m| m == "Off");
}

fn transport_position_mut(t: &mut lavmfx_core::Transport) -> &mut lavmfx_core::RangeParam {
    match t {
        lavmfx_core::Transport::Timeline(t) => &mut t.position,
        lavmfx_core::Transport::BpmSync(t) => &mut t.position,
    }
}

#[derive(Default)]
struct FpsCounter {
    last: Option<Instant>,
    frames: u32,
    fps: f32,
}

impl FpsCounter {
    fn tick(&mut self) {
        let now = Instant::now();
        let last = *self.last.get_or_insert(now);
        self.frames += 1;
        let dt = now.duration_since(last).as_secs_f32();
        if dt >= 0.5 {
            self.fps = self.frames as f32 / dt;
            self.frames = 0;
            self.last = Some(now);
        }
    }
}

/// Test card's second line: "1920 x 1080  16:9" (or "2.35:1" style).
fn test_card_subtitle(w: u32, h: u32) -> String {
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    let g = gcd(w, h).max(1);
    let (a, b) = (w / g, h / g);
    let aspect = if a <= 32 && b <= 32 {
        format!("{a}:{b}")
    } else {
        format!("{:.2}:1", w as f32 / h.max(1) as f32)
    };
    format!("{w} x {h}  {aspect}")
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() { "untitled".into() } else { s }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Composition size presets (name, width, height).
const RESOLUTIONS: &[(&str, u32, u32)] = &[
    ("HD 720p", 1280, 720),
    ("Full HD 1080p", 1920, 1080),
    ("QHD 1440p", 2560, 1440),
    ("4K UHD", 3840, 2160),
    ("Portrait 1080×1920", 1080, 1920),
    ("Square 1080", 1080, 1080),
    ("XGA 4:3", 1024, 768),
];

/// `Resolution  [w] × [h]  Presets` row; changes apply on the next frame.
fn resolution_row(ui: &mut egui::Ui, comp: &mut Composition) {
    let (mut w, mut h) = comp.resolution();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.allocate_ui_with_layout(
            vec2(LABEL_W, 18.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.set_width(LABEL_W);
                ui.label("Resolution");
            },
        );
        let mut changed = ui
            .add_sized([56.0, 18.0], egui::DragValue::new(&mut w).range(16..=8192).speed(4.0))
            .on_hover_text("Composition width (px)")
            .changed();
        ui.label("×");
        changed |= ui
            .add_sized([56.0, 18.0], egui::DragValue::new(&mut h).range(16..=8192).speed(4.0))
            .on_hover_text("Composition height (px)")
            .changed();
        ui.menu_button("Presets", |ui| {
            for (name, pw, ph) in RESOLUTIONS {
                if ui.button(format!("{name}  ({pw}×{ph})")).clicked() {
                    (w, h) = (*pw, *ph);
                    changed = true;
                    ui.close();
                }
            }
        });
        if changed {
            comp.set_resolution(w, h);
        }
    });
}

/// What the clip at `slot` receives live (NDI / shared video), if anything.
fn clip_live_source(comp: &Composition, slot: Slot) -> Option<lavmfx_core::sources::ClipSource> {
    use lavmfx_core::sources::ClipSource;
    match comp.clip(slot.deck, slot.layer, slot.col)?.source()? {
        s @ (ClipSource::Ndi(_) | ClipSource::Share(_) | ClipSource::Capture(_)) => Some(s.clone()),
        _ => None,
    }
}

/// Debug aid: with `LAVMFX_SCREENSHOT=file.ppm`, save the main window 5 s
/// after start (binary PPM; no image dependency).
fn debug_screenshot(ctx: &egui::Context, started: Instant) {
    let Some(path) = std::env::var_os("LAVMFX_SCREENSHOT") else {
        return;
    };
    static REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if started.elapsed().as_secs() >= 5 && !REQUESTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
            move |img| {
                let [w, h] = img.size;
                let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
                out.extend(img.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b()]));
                match std::fs::write(&path, out) {
                    Ok(()) => log::info!("screenshot saved to {}", PathBuf::from(&path).display()),
                    Err(e) => log::warn!("screenshot: {e}"),
                }
            },
        )));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_card_subtitle_names_the_aspect() {
        assert_eq!(super::test_card_subtitle(1920, 1080), "1920 x 1080  16:9");
        assert_eq!(super::test_card_subtitle(1024, 768), "1024 x 768  4:3");
        assert_eq!(super::test_card_subtitle(2048, 871), "2048 x 871  2.35:1");
    }

    #[test]
    fn blend_mode_lists_match() {
        assert_eq!(
            lavmfx_core::mix::SHADER_BLENDS.len() as u32,
            lavmfx_render::BLEND_MODE_COUNT
        );
    }
}
