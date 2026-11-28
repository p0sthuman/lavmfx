//! Preferences: settings that belong to the machine, not the
//! composition (`~/.config/lavmfx/preferences.json`). Settings
//! of features that arrive later (MIDI/OSC/webserver M7, FFGL M8,
//! recording/rendering) are stored now and marked as not active yet.

use std::path::PathBuf;

use eframe::egui::{self, Rect, RichText, vec2};
use lavmfx_core::defaults::{LAYER_BLEND_MODES, PLAY_MODES, TRANSPORT_TYPES};
use serde::{Deserialize, Serialize};

use super::theme::{self, Appearance};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Clip panel follows clips triggered by MIDI/OSC (M7).
    pub follow_external_triggers: bool,
    /// Start offset for externally triggered clips (M7).
    pub clip_start_offset_ms: f64,
    pub scroll_into_view: bool,
    /// Ask before quitting.
    pub quit_confirmation: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            follow_external_triggers: true,
            clip_start_offset_ms: 0.0,
            scroll_into_view: true,
            quit_confirmation: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    /// Index into `TRANSPORT_TYPES` (Timeline / BPM Sync).
    pub video_transport: usize,
    /// Index into `PLAY_MODES`.
    pub play_mode: usize,
    /// 0 `<`, 1 `||`, 2 `>`.
    pub play_direction: usize,
    pub layer_blend: String,
    /// Tempo nudge range, fraction of the BPM (M7 nudge buttons).
    pub tempo_nudge: f64,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            video_transport: 0,
            play_mode: 0,
            play_direction: 2,
            layer_blend: lavmfx_core::defaults::DEFAULT_LAYER_BLEND.to_owned(),
            tempo_nudge: 0.1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    /// Device ids (`lavmfx_audio::DeviceInfo::id`); None = system default.
    pub output_device: Option<String>,
    pub input_device: Option<String>,
    pub sample_rate: u32,
    /// Frames per audio callback (latency).
    pub buffer_size: u32,
    /// Linear gain on the FFT input.
    pub fft_gain: f64,
    /// Global FFT threshold 0..0.95 of the meter (all FFT sources; each
    /// drive's own Thresh comes after it).
    pub fft_threshold: f64,
    /// Fan Noise Reduction 0 (off) … 1: the input's steady noise floor (laptop
    /// fans) is learned per band and gated out of the External FFT.
    pub fan_noise: f64,
    /// External Audio FFT Input: 0 = No Input, 1 = all channels, 2 + n = channel n.
    pub fft_input: u32,
    /// SMPTE Timecode Framerate (LTC; MTC brings its own).
    pub smpte_fps: lavmfx_core::timecode::Fps,
    /// SMPTE Timecode Input 1 / 2.
    pub smpte_inputs: [SmpteInput; 2],
    /// Delay compensation per input, in frames (SMPTE panel).
    pub smpte_delay: [f64; 2],
}

/// Where an SMPTE input's timecode comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SmpteInput {
    #[default]
    None,
    /// LTC on this channel (0-based) of the audio input device.
    Ltc(u32),
    /// MIDI Timecode from any enabled MIDI input.
    Mtc,
}

impl SmpteInput {
    pub fn label(self) -> String {
        match self {
            SmpteInput::None => "No Input".into(),
            SmpteInput::Ltc(c) => format!("Input channel {} (LTC)", c + 1),
            SmpteInput::Mtc => "MIDI Timecode (MTC)".into(),
        }
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            output_device: None,
            input_device: None,
            sample_rate: 44_100,
            buffer_size: 480,
            fft_gain: 1.0,
            fft_threshold: 0.0,
            fan_noise: 0.0,
            fft_input: 0,
            smpte_fps: Default::default(),
            smpte_inputs: Default::default(),
            smpte_delay: [0.0; 2],
        }
    }
}

/// Preferences › MIDI devices: inputs are on unless switched off, outputs
/// (feedback to LEDs / motor faders) only when switched on.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MidiDevices {
    pub disabled_inputs: Vec<String>,
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Osc {
    pub input: bool,
    pub in_port: u16,
    pub output: bool,
    pub out_address: String,
    pub out_port: u16,
    pub bundles: bool,
}

impl Default for Osc {
    fn default() -> Self {
        Self {
            input: false,
            in_port: 7000,
            output: false,
            out_address: "127.0.0.1".into(),
            out_port: 7001,
            bundles: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Webserver {
    pub enabled: bool,
    pub port: u16,
    pub address: String,
}

impl Default for Webserver {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 8080,
            address: "0.0.0.0".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Folders {
    pub ffgl_plugins: Vec<String>,
    /// ISF shader folders (`.fs` files, searched 3 levels deep).
    pub isf_shaders: Vec<String>,
    pub snapshots: String,
    pub recordings: String,
    pub renders: String,
}

/// NDI (Output menu › NDI Output, Preferences › Video).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ndi {
    /// Send the composition as an NDI source.
    pub output: bool,
    /// Source name (receivers see "MACHINE (name)").
    pub name: String,
    /// Folder of the NDI runtime / SDK when it isn't on the library path.
    pub runtime_dir: String,
}

impl Default for Ndi {
    fn default() -> Self {
        Self {
            output: false,
            name: "LAVMFX".into(),
            runtime_dir: String::new(),
        }
    }
}

impl Ndi {
    /// Tell the NDI loader where to look first.
    pub fn apply_runtime_dir(&self) {
        let d = self.runtime_dir.trim();
        lavmfx_media::ndi::set_runtime_dir((!d.is_empty()).then(|| PathBuf::from(d)));
    }
}

/// Clip rendering (Preferences › Clip Rendering).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rendering {
    pub preset: String,
}

impl Default for Rendering {
    fn default() -> Self {
        Self {
            preset: "DXV 3 Normal Quality No Alpha".into(),
        }
    }
}

/// Recording settings (Preferences › Recording).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Recording {
    /// Preset name (`lavmfx_media::record::PRESETS`).
    pub preset: String,
    /// Media: record the audio output too.
    pub audio: bool,
    pub fps: u32,
    /// 16 or 24 (PCM in .mov).
    pub bit_depth: u16,
}

impl Default for Recording {
    fn default() -> Self {
        Self {
            preset: "DXV 3 Normal Quality No Alpha".into(),
            audio: false,
            fps: 60,
            bit_depth: 16,
        }
    }
}

/// Preferences › DMX (node name, network adapter, inputs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dmx {
    /// Art-Net node name (ArtPollReply) and sACN source name.
    pub node_name: String,
    /// IPv4 address of the network adapter to send / listen on ("" = any).
    pub adapter: String,
    /// Art-Net inputs (DMX in → shortcuts).
    pub inputs: Vec<DmxInput>,
}

impl Default for Dmx {
    fn default() -> Self {
        let host = std::fs::read_to_string("/etc/hostname")
            .ok()
            .map(|h| h.trim().to_owned())
            .filter(|h| !h.is_empty())
            .or_else(|| std::env::var("COMPUTERNAME").ok())
            .unwrap_or_else(|| "node".into());
        Self {
            node_name: format!("LAVMFX {host}"),
            adapter: String::new(),
            inputs: Vec::new(),
        }
    }
}

impl Dmx {
    pub fn adapter_ip(&self) -> Option<std::net::Ipv4Addr> {
        self.adapter.trim().parse().ok()
    }
}

/// One Art-Net input: a network universe heard as an internal input
/// lumiverse, channels shifted by an offset (DMX input settings).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DmxInput {
    pub enabled: bool,
    /// Art-Net port address (net / subnet / universe).
    pub universe: u16,
    /// Added to every received channel number.
    pub channel_offset: i32,
    /// Input lumiverse (1-based) DMX shortcuts listen to.
    pub lumiverse: u16,
}

impl Default for DmxInput {
    fn default() -> Self {
        Self {
            enabled: true,
            universe: 0,
            channel_offset: 0,
            lumiverse: 1,
        }
    }
}

/// Composition shared with other apps (Output menu, Preferences › Video).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShareOut {
    pub output: bool,
    /// Node / sender name.
    pub name: String,
}

impl Default for ShareOut {
    fn default() -> Self {
        Self {
            output: false,
            name: "LAVMFX".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub general: General,
    pub defaults: Defaults,
    pub appearance: Appearance,
    pub audio: Audio,
    /// MIDI middle C naming: 0 legacy, 1 C3, 2 C4.
    pub midi_middle_c: usize,
    pub midi_devices: MidiDevices,
    /// Toolbar › LINK: Ableton Link on.
    pub ableton_link: bool,
    pub osc: Osc,
    pub webserver: Webserver,
    pub folders: Folders,
    pub ndi: Ndi,
    /// Shared video (PipeWire / Spout / Syphon) output.
    pub share: ShareOut,
    /// Preferences › DMX.
    pub dmx: Dmx,
    /// Preferences › Recording.
    pub recording: Recording,
    /// Preferences › Clip Rendering.
    pub rendering: Rendering,
    /// Recently opened / saved compositions, newest first (Composition › Open Recent).
    pub recent: Vec<String>,
}

fn path() -> PathBuf {
    super::dock::config_dir().join("preferences.json")
}

impl Preferences {
    pub fn load() -> Self {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let p = path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
        std::fs::rename(tmp, p)
    }

    /// Remember `path` at the top of Open Recent (max 10) and save.
    pub fn add_recent(&mut self, path: &std::path::Path) {
        let p = path.display().to_string();
        self.recent.retain(|r| *r != p);
        self.recent.insert(0, p);
        self.recent.truncate(10);
        let _ = self.save();
    }

    /// Push settings that live elsewhere (core defaults).
    pub fn apply_globals(&self) {
        lavmfx_core::mix::set_default_layer_blend(&self.defaults.layer_blend);
    }

    /// New video clips: default transport, play mode and direction.
    pub fn apply_clip_defaults(&self, clip: &mut lavmfx_core::Clip, ids: &mut lavmfx_core::IdGen, bpm: f64) {
        let d = &self.defaults;
        if d.video_transport == 1 {
            clip.set_transport_type(ids, 1, bpm);
        }
        let (mode, dir) = match clip.transport.as_mut() {
            Some(lavmfx_core::Transport::Timeline(t)) => (&mut t.controls.playmode, &mut t.controls.playdirection),
            Some(lavmfx_core::Transport::BpmSync(t)) => (&mut t.controls.playmode, &mut t.controls.playdirection),
            None => return,
        };
        mode.select(d.play_mode);
        dir.select(d.play_direction);
    }
}

/// Tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    General,
    Appearance,
    Audio,
    Video,
    Midi,
    Osc,
    Dmx,
    Recording,
    ClipRendering,
    Defaults,
    Webserver,
}

impl Page {
    const ALL: [(Page, &'static str, &'static str); 11] = [
        (Page::General, "General", "General Preferences"),
        (Page::Appearance, "Appearance", "Appearance"),
        (Page::Audio, "Audio", "Audio Preferences"),
        (Page::Video, "Video", "Video Preferences"),
        (Page::Midi, "MIDI", "MIDI Preferences"),
        (Page::Osc, "OSC", "OSC Preferences"),
        (Page::Dmx, "DMX", "DMX Preferences"),
        (Page::Recording, "Recording", "Recording Preferences"),
        (Page::ClipRendering, "Clip Rendering", "Clip Rendering Preferences"),
        (Page::Defaults, "Defaults", "Defaults"),
        (Page::Webserver, "Webserver", "Webserver & REST API"),
    ];
}

/// The window's state.
#[derive(Default)]
pub struct PrefsWindow {
    pub open: bool,
    pub page: Page,
    pub audio: AudioPage,
    /// OSC / webserver status lines (filled by the app each frame).
    pub osc_status: String,
    pub web_status: String,
    /// NDI / shared-video output state lines.
    pub ndi_status: String,
    pub share_status: String,
    /// MIDI ports (inputs, outputs) and the last message, from the app.
    pub midi_ports: (Vec<String>, Vec<String>),
    pub midi_last: String,
    pub dmx: DmxPage,
}

/// Live DMX info for the DMX page (filled by the app while it shows).
#[derive(Default)]
pub struct DmxPage {
    /// Network adapters (read when the page first shows, and on Refresh).
    pub adapters: Option<Vec<(String, std::net::Ipv4Addr)>>,
    /// Input state line ("Error: …" in red).
    pub status: String,
    /// Output state line.
    pub out_status: String,
    /// Universes heard: (port address, seconds since the last packet, channels).
    pub heard: Vec<(u16, f32, Box<[u8; 512]>)>,
    /// Monitor: shown universe.
    pub monitor: Option<u16>,
}

/// Live audio info for the Audio page (filled by the app each frame).
#[derive(Default)]
pub struct AudioPage {
    /// Device list (read when the page first shows, and on Refresh).
    pub devices: Option<lavmfx_audio::Devices>,
    /// Input status line.
    pub status: String,
    /// Output status line.
    pub out_status: String,
    /// FFT input spectrum.
    pub spectrum: lavmfx_core::fft::Spectrum,
    /// Set by Refresh: the app retries opening the input.
    pub retry: bool,
}

fn dmx_page(ui: &mut egui::Ui, d: &mut Dmx, page: &mut DmxPage) {
    let pal = theme::pal(ui);
    heading(ui, "Art-Net Node Name");
    ui.add(egui::TextEdit::singleline(&mut d.node_name).desired_width(260.0));
    note(
        ui,
        "The name this app shows as on the Art-Net network (ArtPoll replies) and as the sACN source name.",
    );
    heading(ui, "Network Adapter");
    let adapters = page.adapters.get_or_insert_with(lavmfx_io::dmx::adapters).clone();
    ui.horizontal(|ui| {
        let shown = if d.adapter.trim().is_empty() {
            "Any (all adapters)".to_string()
        } else {
            adapters
                .iter()
                .find(|(_, ip)| ip.to_string() == d.adapter.trim())
                .map_or_else(|| format!("{} (not found)", d.adapter), |(n, ip)| format!("{n} · {ip}"))
        };
        egui::ComboBox::from_id_salt("dmx-adapter")
            .selected_text(shown)
            .width(260.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(d.adapter.trim().is_empty(), "Any (all adapters)")
                    .clicked()
                {
                    d.adapter.clear();
                }
                for (n, ip) in &adapters {
                    let s = ip.to_string();
                    if ui
                        .selectable_label(d.adapter.trim() == s, format!("{n} · {ip}"))
                        .clicked()
                    {
                        d.adapter = s;
                    }
                }
            });
        if ui.button("Refresh").clicked() {
            page.adapters = None;
        }
    });
    note(
        ui,
        "Send and listen on this network card only (e.g. the wired one, not Wi-Fi). Lumiverses (outputs) are set up in Output › Advanced Output: + › DMX Lumiverse / DMX Fixture.",
    );
    status_line(ui, &page.out_status);

    heading(ui, "DMX Input (Art-Net)");
    status_line(ui, &page.status);
    let mut remove = None;
    for (i, input) in d.inputs.iter_mut().enumerate() {
        ui.push_id(("dmx-in", i), |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut input.enabled, "")
                    .on_hover_text("Listen to this universe");
                let (n, sn, u) = lavmfx_core::dmx::artnet_split(input.universe);
                let (mut n, mut sn, mut u) = (n, sn, u);
                ui.label("Net");
                let a = ui.add(egui::DragValue::new(&mut n).range(0..=127)).changed();
                ui.label("Subnet");
                let b = ui
                    .add(egui::DragValue::new(&mut sn).range(0..=15))
                    .on_hover_text("Subnet 0–15: with the Universe it forms the Art-Net universe this input listens to")
                    .changed();
                ui.label("Universe");
                let c = ui.add(egui::DragValue::new(&mut u).range(0..=15)).changed();
                if a || b || c {
                    input.universe = lavmfx_core::dmx::artnet_join(n, sn, u);
                }
                if super::widgets::icon_button(ui, super::widgets::Icon::Close, false, vec2(18.0, 18.0), true)
                    .on_hover_text("Delete this input")
                    .clicked()
                {
                    remove = Some(i);
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                ui.label("Lumiverse");
                ui.add(egui::DragValue::new(&mut input.lumiverse).range(1..=64))
                    .on_hover_text("The input lumiverse DMX shortcuts of this input belong to");
                ui.label("Channel Offset");
                ui.add(egui::DragValue::new(&mut input.channel_offset).range(-511..=511))
                    .on_hover_text("Shifts all received channel numbers up or down");
            });
        });
    }
    if let Some(i) = remove {
        d.inputs.remove(i);
    }
    if ui.button("+ Add Art-Net Input").clicked() {
        let next = d.inputs.iter().map(|i| i.universe + 1).max().unwrap_or(0);
        let lumi = d.inputs.iter().map(|i| i.lumiverse + 1).max().unwrap_or(1);
        d.inputs.push(DmxInput {
            universe: next,
            lumiverse: lumi,
            ..Default::default()
        });
    }
    note(
        ui,
        "Received channels drive DMX shortcuts: Shortcuts › Edit DMX (Ctrl+D), click a control, then move the channel on your desk. Faders follow the channel; buttons fire when it crosses 50 %.",
    );

    heading(ui, "Art-Net Monitor");
    if page.heard.is_empty() {
        note(ui, "Nothing received yet (the monitor listens while an input is on).");
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for (u, ago, _) in &page.heard {
            let (n, sn, x) = lavmfx_core::dmx::artnet_split(*u);
            let live = *ago < 2.0;
            let text = RichText::new(format!("{n}.{sn}.{x}")).color(if live { pal.accent } else { pal.text_weak });
            if ui
                .selectable_label(page.monitor == Some(*u), text)
                .on_hover_text(format!("Last packet {ago:.1} s ago"))
                .clicked()
            {
                page.monitor = Some(*u);
            }
        }
    });
    let shown = page.monitor.or(page.heard.first().map(|h| h.0));
    if let Some((_, _, data)) = page.heard.iter().find(|h| Some(h.0) == shown) {
        let cell = ((ui.available_width() - 4.0) / 32.0).clamp(8.0, 17.0);
        let (r, resp) = ui.allocate_exact_size(vec2(cell * 32.0, cell * 16.0), egui::Sense::hover());
        let p = ui.painter_at(r);
        for (i, &v) in data.iter().enumerate() {
            let c = Rect::from_min_size(
                r.min + vec2((i % 32) as f32 * cell, (i / 32) as f32 * cell),
                vec2(cell, cell),
            )
            .shrink(0.5);
            p.rect_filled(c, 1.0, pal.field);
            if v > 0 {
                p.rect_filled(c, 1.0, pal.accent.gamma_multiply(v as f32 / 255.0));
            }
        }
        if let Some(m) = resp.hover_pos() {
            let i = (((m.y - r.min.y) / cell) as usize * 32 + ((m.x - r.min.x) / cell) as usize).min(511);
            resp.on_hover_text_at_pointer(format!("Channel {}: {}", i + 1, data[i]));
        }
    }
}

/// Grey explanation under a setting.
fn note(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).small().weak()).wrap());
    ui.add_space(6.0);
}

/// Service status from the app; lines starting with "Error: " show in red.
fn status_line(ui: &mut egui::Ui, text: &str) {
    if text.is_empty() {
        return;
    }
    let p = theme::pal(ui);
    for part in text.split(", ") {
        let (t, c) = match part.strip_prefix("Error: ") {
            Some(e) => (e, p.danger),
            None => (part, p.accent),
        };
        ui.add(egui::Label::new(RichText::new(t).color(c)).wrap());
    }
    ui.add_space(6.0);
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).strong());
}

fn color_override(ui: &mut egui::Ui, label: &str, v: &mut Option<[u8; 3]>, base: egui::Color32) {
    ui.horizontal(|ui| {
        ui.add_sized([110.0, 20.0], egui::Label::new(label));
        let mut c = v.map_or(base, |c| egui::Color32::from_rgb(c[0], c[1], c[2]));
        if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque).changed() {
            *v = Some([c.r(), c.g(), c.b()]);
        }
        if v.is_some() && ui.small_button("Default").clicked() {
            *v = None;
        }
    });
}

/// Draw the window; returns true when something changed (caller saves and re-applies).
pub fn show(ctx: &egui::Context, win: &mut PrefsWindow, prefs: &mut Preferences) -> bool {
    if !win.open {
        return false;
    }
    let before = prefs.clone();
    let mut open = win.open;
    egui::Window::new("Preferences")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(vec2(780.0, 620.0))
        .show(ctx, |ui| {
            let pal = theme::pal(ui);
            ui.horizontal_top(|ui| {
                // Navigation column.
                ui.vertical(|ui| {
                    ui.set_width(150.0);
                    ui.add_space(52.0);
                    for (page, name, _) in Page::ALL {
                        let on = win.page == page;
                        let (rect, r) = ui.allocate_exact_size(vec2(150.0, 26.0), egui::Sense::click());
                        let fill = if on {
                            pal.accent_dim
                        } else if r.hovered() {
                            pal.button
                        } else {
                            pal.band
                        };
                        ui.painter().rect_filled(rect, 0.0, fill);
                        ui.painter().text(
                            rect.right_center() - vec2(12.0, 0.0),
                            egui::Align2::RIGHT_CENTER,
                            name,
                            egui::FontId::proportional(13.5),
                            if on { pal.text_strong } else { pal.text },
                        );
                        if r.clicked() {
                            win.page = page;
                        }
                    }
                });
                // Page.
                ui.vertical(|ui| {
                    let title = Page::ALL.iter().find(|(p, ..)| *p == win.page).map_or("", |(.., t)| t);
                    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 0.0, pal.accent_dim);
                    ui.painter().text(
                        rect.left_center() + vec2(18.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        title,
                        egui::FontId::proportional(20.0),
                        pal.text_strong,
                    );
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                        ui.set_max_width(560.0);
                        page(
                            ui,
                            win.page,
                            &mut win.audio,
                            prefs,
                            (&win.osc_status, &win.web_status, (&win.ndi_status, &win.share_status)),
                            (&win.midi_ports, &win.midi_last),
                            &mut win.dmx,
                        );
                    });
                });
            });
        });
    win.open = open;
    *prefs != before
}

fn page(
    ui: &mut egui::Ui,
    page: Page,
    audio: &mut AudioPage,
    p: &mut Preferences,
    (osc_status, web_status, (ndi_status, share_status)): (&str, &str, (&str, &str)),
    (midi_ports, midi_last): (&(Vec<String>, Vec<String>), &str),
    dmx: &mut DmxPage,
) {
    match page {
        Page::General => {
            let g = &mut p.general;
            heading(ui, "Clip Panel");
            ui.checkbox(
                &mut g.follow_external_triggers,
                "Update clip panel on external triggers",
            );
            note(
                ui,
                "When a clip is triggered by MIDI or OSC, the clip panel shows that clip (M7).",
            );
            heading(ui, "Clip Start Offset");
            ui.add(egui::Slider::new(&mut g.clip_start_offset_ms, 0.0..=500.0).suffix(" ms"));
            note(
                ui,
                "Externally triggered clips start this far into their timeline to make up for delay (M7).",
            );
            heading(ui, "Scrolling");
            ui.checkbox(&mut g.scroll_into_view, "Scroll layers and clips into view");
            note(ui, "When a clip or layer is selected it is scrolled into view.");
            heading(ui, "Quit Confirmation");
            ui.checkbox(&mut g.quit_confirmation, "Ask before quitting");
            note(
                ui,
                "A dialog appears when you close the app. Disable it for a silent shutdown.",
            );
        }
        Page::Appearance => {
            let a = &mut p.appearance;
            heading(ui, "Theme");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut a.light, false, "Dark");
                ui.selectable_value(&mut a.light, true, "Light");
            });
            note(ui, "View › Light theme switches too.");
            heading(ui, "Colours");
            let base = a.palette();
            color_override(ui, "Accent", &mut a.accent, base.accent);
            color_override(ui, "Background", &mut a.ground, base.ground);
            color_override(ui, "Panels", &mut a.panel, base.panel);
            color_override(ui, "Section bands", &mut a.band, base.band);
            color_override(ui, "Text", &mut a.text, base.text);
            if ui.button("All colours to default").clicked() {
                (a.accent, a.ground, a.panel, a.band, a.text) = (None, None, None, None, None);
            }
            note(
                ui,
                "Accent = selection, playing clips, animated params, toggles that are on.",
            );
            heading(ui, "UI Scale");
            ui.add(
                egui::Slider::new(&mut a.scale, 0.75..=1.5)
                    .step_by(0.05)
                    .custom_formatter(|v, _| format!("{:.0} %", v * 100.0))
                    .custom_parser(parse_percent),
            );
        }
        Page::Audio => audio_page(ui, audio, &mut p.audio),
        Page::Video => {
            heading(ui, "ISF Shader Directories");
            note(
                ui,
                "ISF shaders (.fs, with a JSON header) from these folders: shaders with an inputImage are \
                 effects (Effects › ISF), the others sources (Sources › ISF). MadMapper materials work too.",
            );
            let before = p.folders.isf_shaders.clone();
            folder_list(ui, &mut p.folders.isf_shaders);
            let reload = ui
                .small_button("Reload shaders")
                .on_hover_text("Read the folders again (after editing a shader)")
                .clicked();
            if reload || before != p.folders.isf_shaders {
                super::isf_host::reload(&p.folders.isf_shaders);
            }
            isf_status(ui);
            ui.add_space(12.0);
            heading(ui, "NDI");
            ndi_section(ui, &mut p.ndi, ndi_status);
            ui.add_space(12.0);
            let backend = lavmfx_media::share::backend();
            heading(ui, &format!("{backend} (video shared with apps on this computer)"));
            if let Some(e) = lavmfx_media::share::unavailable() {
                note(ui, &e);
            }
            ui.horizontal(|ui| {
                ui.checkbox(&mut p.share.output, format!("Share the composition via {backend}"));
                ui.add(egui::TextEdit::singleline(&mut p.share.name).desired_width(160.0));
            });
            note(ui, share_status);
            note(
                ui,
                &format!("Video other apps and cameras share shows in Sources › {backend}."),
            );
            ui.add_space(12.0);
            heading(ui, "FreeFrame (FFGL) Plugin Directories");
            note(
                ui,
                &format!(
                    "FFGL 2.x plugins (.{}) from these folders: effects show in Effects › FFGL, sources in Sources › FFGL. \
                     Linux and Windows; mixers not yet.",
                    lavmfx_io::ffgl::plugin_extension()
                ),
            );
            let before = p.folders.ffgl_plugins.clone();
            folder_list(ui, &mut p.folders.ffgl_plugins);
            let rescan = ui.small_button("Rescan plugins").clicked();
            if rescan || before != p.folders.ffgl_plugins {
                ffgl_rescan_wanted(true);
            }
            ffgl_status(ui);
        }
        Page::Midi => {
            heading(ui, "MIDI Devices");
            let d = &mut p.midi_devices;
            if midi_ports.0.is_empty() && midi_ports.1.is_empty() {
                note(ui, "No MIDI devices found (plugged-in devices show up within 2 s).");
            }
            let names: std::collections::BTreeSet<&String> = midi_ports.0.iter().chain(&midi_ports.1).collect();
            egui::Grid::new("midi-devs").striped(true).show(ui, |ui| {
                ui.label(RichText::new("Device").strong());
                ui.label(RichText::new("Input").strong());
                ui.label(RichText::new("Output").strong());
                ui.end_row();
                for n in names {
                    ui.label(n);
                    if midi_ports.0.contains(n) {
                        let mut on = !d.disabled_inputs.contains(n);
                        if ui
                            .checkbox(&mut on, "")
                            .on_hover_text("Shortcuts listen to this device")
                            .changed()
                        {
                            d.disabled_inputs.retain(|x| x != n);
                            if !on {
                                d.disabled_inputs.push(n.clone());
                            }
                        }
                    } else {
                        ui.label("");
                    }
                    if midi_ports.1.contains(n) {
                        let mut on = d.outputs.contains(n);
                        if ui
                            .checkbox(&mut on, "")
                            .on_hover_text("Send feedback (LEDs, motor faders) to this device")
                            .changed()
                        {
                            d.outputs.retain(|x| x != n);
                            if on {
                                d.outputs.push(n.clone());
                            }
                        }
                    } else {
                        ui.label("");
                    }
                    ui.end_row();
                }
            });
            if !midi_last.is_empty() {
                note(ui, &format!("Last message: {midi_last}"));
            }
            note(
                ui,
                "Map controls with Shortcuts › Edit MIDI (Ctrl+M): click a control, then move / press the MIDI control.",
            );
            heading(ui, "MIDI Middle C");
            for (i, t) in ["Legacy", "Middle C = C3", "Middle C = C4"].into_iter().enumerate() {
                ui.radio_value(&mut p.midi_middle_c, i, t);
            }
            note(ui, "C3 matches most applications (Ableton Live), C4 matches Traktor.");
        }
        Page::Osc => {
            status_line(ui, osc_status);
            let o = &mut p.osc;
            ui.checkbox(&mut o.input, "OSC Input");
            ui.horizontal(|ui| {
                ui.label("Incoming Port");
                ui.add(egui::DragValue::new(&mut o.in_port));
            });
            ui.add_space(6.0);
            ui.checkbox(&mut o.output, "OSC Output");
            ui.horizontal(|ui| {
                ui.label("Outgoing Address");
                ui.text_edit_singleline(&mut o.out_address);
            });
            ui.horizontal(|ui| {
                ui.label("Outgoing Port");
                ui.add(egui::DragValue::new(&mut o.out_port));
            });
            ui.checkbox(&mut o.bundles, "Use bundles");
            note(
                ui,
                "OSC addresses follow the composition (/composition/layers/1/clips/2/connect, /composition/layers/1/video/opacity…). \
                 Floats are 0..1 over a parameter's range; send \"?\" as the argument to ask for a value. \
                 Output sends changes of what controllers use, plus clip states and names.",
            );
        }
        Page::Dmx => dmx_page(ui, &mut p.dmx, dmx),
        Page::Recording => {
            heading(ui, "Snapshot Output Directory");
            folder_field(ui, &mut p.folders.snapshots);
            default_note(ui, &p.folders.snapshots, "Snapshots");
            heading(ui, "Recording Output Directory");
            folder_field(ui, &mut p.folders.recordings);
            default_note(ui, &p.folders.recordings, "Recorded");
            recording_page(ui, &mut p.recording);
        }
        Page::ClipRendering => {
            heading(ui, "File Output Directory");
            folder_field(ui, &mut p.folders.renders);
            default_note(ui, &p.folders.renders, "Renders");
            heading(ui, "Presets");
            preset_combo(ui, "render-preset", &mut p.rendering.preset);
            note(
                ui,
                "Clip › Render to File renders the selected clip with its effects and its layer's effects, at the composition size (video only for now). Results appear in the Render Queue.",
            );
        }
        Page::Defaults => {
            let d = &mut p.defaults;
            heading(ui, "Default Video Clip Transport");
            egui::ComboBox::from_id_salt("pref-transport")
                .selected_text(TRANSPORT_TYPES[d.video_transport.min(1)])
                .show_ui(ui, |ui| {
                    for (i, t) in TRANSPORT_TYPES.iter().enumerate().take(2) {
                        ui.selectable_value(&mut d.video_transport, i, *t);
                    }
                });
            heading(ui, "Default Video Clip Play Mode");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("pref-playmode")
                    .selected_text(PLAY_MODES.get(d.play_mode).copied().unwrap_or("Loop"))
                    .show_ui(ui, |ui| {
                        for (i, t) in PLAY_MODES.iter().enumerate() {
                            ui.selectable_value(&mut d.play_mode, i, *t);
                        }
                    });
                for (i, t) in ["<", "||", ">"].into_iter().enumerate() {
                    ui.selectable_value(&mut d.play_direction, i, t);
                }
            });
            heading(ui, "Default Layer Blend Mode");
            egui::ComboBox::from_id_salt("pref-blend")
                .selected_text(d.layer_blend.clone())
                .height(360.0)
                .show_ui(ui, |ui| {
                    for m in LAYER_BLEND_MODES {
                        ui.selectable_value(&mut d.layer_blend, (*m).to_owned(), *m);
                    }
                });
            heading(ui, "Tempo Nudge Range");
            ui.add(
                egui::Slider::new(&mut d.tempo_nudge, 0.01..=0.5)
                    .custom_formatter(|v, _| format!("{:.0} %", v * 100.0))
                    .custom_parser(parse_percent),
            );
            note(
                ui,
                "Clip defaults apply when new clips are added; they don't change clips that are already there.",
            );
        }
        Page::Webserver => {
            status_line(ui, web_status);
            let w = &mut p.webserver;
            ui.checkbox(&mut w.enabled, "Enable Webserver & REST API");
            ui.horizontal(|ui| {
                ui.label("Listen Port");
                ui.add(egui::DragValue::new(&mut w.port));
            });
            ui.horizontal(|ui| {
                ui.label("Listen Address");
                ui.text_edit_singleline(&mut w.address);
            });
            note(
                ui,
                "Serves the composition as a REST API (JSON), the shape existing VJ controllers use.",
            );
        }
    }
}

/// Typed text of a field shown as "NN %": "120", "120 %", "120,5" → 1.2 / 1.205.
fn parse_percent(t: &str) -> Option<f64> {
    let n: String = t
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-'))
        .collect();
    n.replace(',', ".").parse::<f64>().ok().map(|v| v / 100.0)
}

/// "Empty = <default folder>" under a folder field.
fn default_note(ui: &mut egui::Ui, v: &str, sub: &str) {
    if v.trim().is_empty() {
        note(
            ui,
            &format!("Empty = {}", super::record_ui::default_folder(sub).display()),
        );
    }
}

/// Recording / render preset picker (unavailable ones disabled with the reason).
fn preset_combo(ui: &mut egui::Ui, id: &str, preset: &mut String) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(preset.as_str())
        .width(280.0)
        .show_ui(ui, |ui| {
            for p in lavmfx_media::record::PRESETS {
                let item = ui.add_enabled(
                    p.unavailable.is_none(),
                    egui::Button::selectable(*preset == p.name, p.name),
                );
                let item = match p.unavailable {
                    Some(why) => item.on_disabled_hover_text(why),
                    None => item,
                };
                if item.clicked() {
                    *preset = p.name.into();
                }
            }
        });
}

/// Media, preset (codec / format), frame rate, audio bit depth.
fn recording_page(ui: &mut egui::Ui, r: &mut Recording) {
    heading(ui, "Media");
    egui::ComboBox::from_id_salt("rec-media")
        .selected_text(if r.audio { "Audio and Video" } else { "Video Only" })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut r.audio, false, "Video Only");
            ui.selectable_value(&mut r.audio, true, "Audio and Video");
        });
    heading(ui, "Presets");
    preset_combo(ui, "rec-preset", &mut r.preset);
    if let Some(p) = lavmfx_media::record::preset(&r.preset) {
        note(
            ui,
            &format!(
                "Video: {} · {} · .{}{}",
                p.codec_name,
                p.format,
                p.extension(),
                if p.codec_name == "H264" {
                    " (NVIDIA encoder when available, else x264)"
                } else {
                    ""
                }
            ),
        );
    }
    heading(ui, "Frame Rate");
    egui::ComboBox::from_id_salt("rec-fps")
        .selected_text(format!("{} fps", r.fps))
        .show_ui(ui, |ui| {
            for f in [24, 25, 30, 50, 60] {
                ui.selectable_value(&mut r.fps, f, format!("{f} fps"));
            }
        });
    heading(ui, "Audio Bit Depth");
    egui::ComboBox::from_id_salt("rec-bits")
        .selected_text(r.bit_depth.to_string())
        .show_ui(ui, |ui| {
            for b in [16u16, 24] {
                ui.selectable_value(&mut r.bit_depth, b, b.to_string());
            }
        });
    note(
        ui,
        "Audio = the app's audio output mix (sample rate of the output device). H264 files get AAC audio.",
    );
}

fn folder_field(ui: &mut egui::Ui, v: &mut String) {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(v).desired_width(360.0));
        if ui.button("Browse…").clicked()
            && let Some(d) = rfd::FileDialog::new().pick_folder()
        {
            *v = d.display().to_string();
        }
    });
}

/// NDI runtime, output name and state.
fn ndi_section(ui: &mut egui::Ui, n: &mut Ndi, output_status: &str) {
    match lavmfx_media::ndi::runtime_status() {
        Ok(v) => note(ui, &format!("Runtime: NDI {v}")),
        Err(e) => note(ui, &e),
    }
    ui.horizontal(|ui| {
        ui.label("Runtime folder");
        let shown = if n.runtime_dir.is_empty() {
            "(system library path)".to_owned()
        } else {
            n.runtime_dir.clone()
        };
        ui.label(RichText::new(shown).weak());
        if ui.small_button("Choose…").clicked()
            && let Some(d) = rfd::FileDialog::new().pick_folder()
        {
            n.runtime_dir = d.display().to_string();
            n.apply_runtime_dir();
        }
        if !n.runtime_dir.is_empty() && ui.small_button("−").clicked() {
            n.runtime_dir.clear();
            n.apply_runtime_dir();
        }
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut n.output, "Send the composition as NDI source");
        ui.add(egui::TextEdit::singleline(&mut n.name).desired_width(160.0));
    });
    note(ui, output_status);
    note(ui, "NDI sources on the network show in Sources › NDI.");
}

/// Asks the app to rescan FFGL folders (the host lives in the app).
pub fn ffgl_rescan_wanted(set: bool) -> bool {
    static WANT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if set {
        WANT.store(true, std::sync::atomic::Ordering::Relaxed);
        true
    } else {
        WANT.swap(false, std::sync::atomic::Ordering::Relaxed)
    }
}

/// FFGL scan result: usable plugins and files that aren't (with why).
fn ffgl_status(ui: &mut egui::Ui) {
    let Some((ok, problems)) = super::ffgl_host::last_report() else {
        note(ui, "Scanning…");
        return;
    };
    note(ui, &format!("{ok} plugins ready · {} not usable", problems.len()));
    if problems.is_empty() {
        return;
    }
    egui::CollapsingHeader::new("FFGL problems")
        .id_salt("ffgl-problems")
        .show(ui, |ui| {
            for (f, why) in problems {
                ui.label(RichText::new(f.display().to_string()).strong());
                ui.label(RichText::new(why).weak());
            }
        });
}

/// Loaded ISF shaders and the ones that don't run (with why).
fn isf_status(ui: &mut egui::Ui) {
    use lavmfx_core::isf::IsfKind;
    let all = lavmfx_core::isf::shaders();
    let count = |k: IsfKind| all.iter().filter(|s| s.kind == k && s.runs()).count();
    let broken: Vec<_> = all.iter().filter(|s| !s.runs()).collect();
    let skipped = super::isf_host::last_report().map_or_else(Vec::new, |r| r.skipped);
    note(
        ui,
        &format!(
            "{} effects, {} sources ready · {} don't run · {} files aren't ISF",
            count(IsfKind::Effect),
            count(IsfKind::Source),
            broken.len(),
            skipped.len()
        ),
    );
    if broken.is_empty() && skipped.is_empty() {
        return;
    }
    egui::CollapsingHeader::new("Problems")
        .id_salt("isf-problems")
        .show(ui, |ui| {
            for s in broken {
                let where_ = s.path.as_ref().map_or("built in".into(), |p| p.display().to_string());
                ui.label(RichText::new(&s.title).strong()).on_hover_text(where_);
                ui.label(RichText::new(s.error.as_deref().unwrap_or_default()).weak());
            }
            for (f, why) in skipped {
                ui.label(RichText::new(f.display().to_string()).strong());
                ui.label(RichText::new(why).weak());
            }
        });
}

fn folder_list(ui: &mut egui::Ui, dirs: &mut Vec<String>) {
    let mut remove = None;
    for (i, d) in dirs.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(d);
            if ui.small_button("−").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        dirs.remove(i);
    }
    if ui.small_button("+ Add folder").clicked()
        && let Some(d) = rfd::FileDialog::new().pick_folder()
    {
        dirs.push(d.display().to_string());
    }
}

pub fn device_combo(
    ui: &mut egui::Ui,
    salt: &str,
    list: &[lavmfx_audio::DeviceInfo],
    default: Option<&str>,
    v: &mut Option<String>,
) {
    let name_of = |id: &str| list.iter().find(|d| d.id == id).map(|d| d.name.clone());
    let default_name = default.and_then(name_of).unwrap_or_else(|| "none".into());
    let shown = match v {
        None => format!("System default ({default_name})"),
        Some(id) => name_of(id).unwrap_or_else(|| format!("{id} (not connected)")),
    };
    egui::ComboBox::from_id_salt(salt)
        .width(ui.available_width().min(420.0))
        .selected_text(shown)
        .show_ui(ui, |ui| {
            ui.selectable_value(v, None, format!("System default ({default_name})"));
            for d in list {
                ui.selectable_value(v, Some(d.id.clone()), &d.name)
                    .on_hover_text(format!("{} ch · {} Hz", d.channels, d.sample_rate));
            }
        });
}

/// Audio page: devices, sample rate, buffer, FFT input + gain.
fn audio_page(ui: &mut egui::Ui, view: &mut AudioPage, a: &mut Audio) {
    if view.devices.is_none() {
        view.devices = Some(lavmfx_audio::list());
    }
    let devs = view.devices.clone().unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Audio system: {}", devs.host)).weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("Refresh")
                .on_hover_text("List devices again and reopen the input")
                .clicked()
            {
                view.devices = None;
                view.retry = true;
            }
        });
    });
    heading(ui, "Audio Output Device");
    device_combo(
        ui,
        "pref-out",
        &devs.outputs,
        devs.default_output.as_deref(),
        &mut a.output_device,
    );
    ui.label(RichText::new(&view.out_status).small());
    note(
        ui,
        "Plays clip audio. Clip audio also feeds the Composition / Layer FFT sources.",
    );
    heading(ui, "Audio Input Device");
    device_combo(
        ui,
        "pref-in",
        &devs.inputs,
        devs.default_input.as_deref(),
        &mut a.input_device,
    );
    note(
        ui,
        "\"Monitor of …\" devices listen to what an output plays (another player, the system mix).",
    );
    heading(ui, "External Audio FFT Input");
    fft_input_combo(ui, "prefs", &devs, a);
    ui.label(RichText::new(&view.status).small());
    spectrum_meter(ui, &view.spectrum);
    note(
        ui,
        "Parameters follow this input with the cogwheel › External FFT. Composition and Layer FFT follow clip audio.",
    );
    heading(ui, "Audio FFT Input Gain");
    ui.add(
        egui::Slider::new(&mut a.fft_gain, 0.0..=40.0)
            .logarithmic(true)
            .custom_formatter(|v, _| format!("{v:.2}×")),
    );
    heading(ui, "Audio FFT Threshold");
    ui.horizontal(|ui| {
        super::widgets::thresh_knob(ui, &mut a.fft_threshold, "Thresh");
        ui.label(
            RichText::new("Quieter sound (ambient noise) is ignored by every FFT-driven parameter: External, Composition and Layer FFT. Each parameter's own Thresh adds to it.")
                .small()
                .weak(),
        );
    });
    heading(ui, "Fan Noise Reduction");
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(&mut a.fan_noise, 0.0..=1.0).custom_formatter(|v, _| {
            if v <= 0.0 {
                "Off".into()
            } else {
                format!("{:.0} %", v * 100.0)
            }
        }))
        .on_hover_text("0 = off. Double-click the value to type it.");
    });
    note(
        ui,
        "Removes the steady hum of laptop fans (or any constant noise) from the External FFT input: the quietest level of each band over the last few seconds is learned as noise and gated out. Higher = stronger. It keeps learning while the input is open (so it works the moment it's on) and follows the input gain. Sounds held steady for longer than ~4 s get weaker.",
    );
    heading(ui, "SMPTE Timecode Framerate");
    egui::ComboBox::from_id_salt("pref-smpte-fps")
        .selected_text(a.smpte_fps.label())
        .show_ui(ui, |ui| {
            for f in lavmfx_core::timecode::Fps::ALL {
                ui.selectable_value(&mut a.smpte_fps, f, f.label());
            }
        });
    note(
        ui,
        "This should match the framerate of the SMPTE (LTC) signal you receive. Drop-frame LTC and MIDI Timecode bring their own rate.",
    );
    for k in 0..2 {
        heading(ui, &format!("SMPTE Timecode Input {}", k + 1));
        smpte_input_combo(ui, k, &devs, a);
    }
    note(
        ui,
        "LTC on a channel of the audio input device, or MIDI Timecode from any enabled MIDI input. Clips follow it with transport SMPTE 1 / SMPTE 2; View › SMPTE shows the timecode.",
    );
    heading(ui, "Sample Rate");
    egui::ComboBox::from_id_salt("pref-sr")
        .selected_text(a.sample_rate.to_string())
        .show_ui(ui, |ui| {
            for r in [44_100, 48_000, 96_000] {
                ui.selectable_value(&mut a.sample_rate, r, r.to_string());
            }
        });
    note(
        ui,
        "Output rate for clip audio (slice 2). The FFT input uses its device's rate.",
    );
    heading(ui, "Buffer Size");
    ui.add(
        egui::Slider::new(&mut a.buffer_size, 64..=4096)
            .logarithmic(true)
            .suffix(" frames"),
    );
    note(
        ui,
        "Frames per audio callback, input and output. Smaller = less latency, more CPU. 480 ≈ 10 ms.",
    );
}

/// Channels of the chosen (or default) input device (2 when unknown).
fn input_channels(devs: &lavmfx_audio::Devices, a: &Audio) -> u32 {
    let id = a.input_device.as_deref().or(devs.default_input.as_deref());
    devs.inputs
        .iter()
        .find(|d| Some(d.id.as_str()) == id)
        .map_or(2, |d| d.channels as u32)
}

/// SMPTE Timecode Input k: No Input / LTC on channel n / MTC.
pub fn smpte_input_combo(ui: &mut egui::Ui, k: usize, devs: &lavmfx_audio::Devices, a: &mut Audio) {
    let n = input_channels(devs, a);
    let v = &mut a.smpte_inputs[k];
    egui::ComboBox::from_id_salt(("smpte-in", k))
        .selected_text(v.label())
        .show_ui(ui, |ui| {
            ui.selectable_value(v, SmpteInput::None, SmpteInput::None.label());
            for c in 0..n {
                ui.selectable_value(v, SmpteInput::Ltc(c), SmpteInput::Ltc(c).label());
            }
            ui.selectable_value(v, SmpteInput::Mtc, SmpteInput::Mtc.label());
        });
}

/// External Audio FFT Input: No Input / All channels / channel n of the input device.
pub fn fft_input_combo(ui: &mut egui::Ui, salt: &str, devs: &lavmfx_audio::Devices, a: &mut Audio) {
    let input_ch = {
        let id = a.input_device.as_deref().or(devs.default_input.as_deref());
        devs.inputs
            .iter()
            .find(|d| Some(d.id.as_str()) == id)
            .map_or(2, |d| d.channels as u32)
    };
    let label = |n: u32| match n {
        0 => "No Input".to_owned(),
        1 => "All channels".to_owned(),
        n => format!("Input channel {}", n - 1),
    };
    egui::ComboBox::from_id_salt(("fft-in", salt))
        .selected_text(label(a.fft_input))
        .show_ui(ui, |ui| {
            for n in 0..2 + input_ch {
                ui.selectable_value(&mut a.fft_input, n, label(n));
            }
        });
}

/// Small bar display of a spectrum (Preferences › Audio).
fn spectrum_meter(ui: &mut egui::Ui, s: &lavmfx_core::fft::Spectrum) {
    let pal = theme::pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(420.0), 40.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, pal.field);
    let w = rect.width() / s.len() as f32;
    for (i, v) in s.iter().enumerate() {
        let h = v.clamp(0.0, 1.0) * (rect.height() - 2.0);
        let x = rect.left() + i as f32 * w;
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x + 1.0, rect.bottom() - 1.0 - h),
                egui::pos2(x + w - 1.0, rect.bottom() - 1.0),
            ),
            0.0,
            pal.accent,
        );
    }
    ui.ctx().request_repaint();
}

#[cfg(test)]
mod tests {
    #[test]
    fn percent_fields_parse_typed_text() {
        assert_eq!(super::parse_percent("120"), Some(1.2));
        assert_eq!(super::parse_percent(" 85 %"), Some(0.85));
        assert_eq!(super::parse_percent("12,5%"), Some(0.125));
        assert_eq!(super::parse_percent("abc"), None);
    }
}
