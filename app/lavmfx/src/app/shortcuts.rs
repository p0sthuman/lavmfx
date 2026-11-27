//! Shortcuts (M7 slice 3, DMX in M9): keyboard, MIDI and DMX maps saved in
//! `~/.config/lavmfx/shortcuts.json`, MIDI devices, MIDI feedback, Art-Net
//! input, and the learn mode (Shortcuts › Edit Keyboard / MIDI / DMX).
//!
//! Learn mode: mappable controls register their rectangle and target each
//! frame ([`learn_target`]); the overlay marks them and shows their current
//! shortcut. A click on one is taken out of the input before the widgets see
//! it (no clip launches while mapping) and makes it the target; the next key
//! or MIDI message is mapped to it. Esc leaves, Delete removes its shortcuts.

use std::collections::HashMap;
use std::time::Instant;

use eframe::egui::{self, Key, RichText};
use lavmfx_core::Id;
use lavmfx_core::control::{self, Value};
use lavmfx_io::dmx::DmxReceiver;
use lavmfx_io::midi::{self, MidiIn, MidiInputs, MidiKind, MidiMsg, MidiOutputs};
use serde::{Deserialize, Serialize};

use super::live::{KeyKind, KeyShortcut, default_keys};
use super::{LavmfxApp, theme};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MidiShortcut {
    /// Input port name; `None` = any device.
    #[serde(default)]
    pub device: Option<String>,
    pub kind: MidiKind,
    /// 0..15.
    pub channel: u8,
    pub number: u8,
    pub address: String,
    pub action: KeyKind,
}

impl MidiShortcut {
    fn matches(&self, m: &MidiIn) -> bool {
        self.kind == m.msg.kind
            && self.channel == m.msg.channel
            && (self.kind == MidiKind::Bend || self.number == m.msg.number)
            && self.device.as_ref().is_none_or(|d| *d == m.device)
    }

    pub fn label(&self) -> String {
        let ch = self.channel + 1;
        match self.kind {
            MidiKind::Note => format!("{} ch{ch}", note_name(self.number)),
            MidiKind::Cc => format!("CC{} ch{ch}", self.number),
            MidiKind::Bend => format!("Bend ch{ch}"),
        }
    }
}

/// A DMX channel mapped to an address (DMX shortcut = input
/// lumiverse + channel; Preferences › DMX assigns Art-Net universes to input
/// lumiverses).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DmxShortcut {
    /// Input lumiverse, 1-based.
    pub lumiverse: u16,
    /// 1..=512.
    pub channel: u16,
    pub address: String,
    pub action: KeyKind,
}

impl DmxShortcut {
    pub fn label(&self) -> String {
        format!("DMX {}/{}", self.lumiverse, self.channel)
    }
}

fn note_name(n: u8) -> String {
    const N: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", N[n as usize % 12], n as i32 / 12 - 2)
}

pub fn key_label(s: &KeyShortcut) -> String {
    let mut t = String::new();
    if s.ctrl {
        t.push_str("Ctrl+");
    }
    if s.alt {
        t.push_str("Alt+");
    }
    if s.shift {
        t.push_str("Shift+");
    }
    t.push_str(s.key.symbol_or_name());
    t
}

/// Everything in `shortcuts.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Shortcuts {
    pub keys: Vec<KeyShortcut>,
    pub midi: Vec<MidiShortcut>,
    pub dmx: Vec<DmxShortcut>,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            keys: default_keys(),
            midi: Vec::new(),
            dmx: Vec::new(),
        }
    }
}

fn path() -> std::path::PathBuf {
    super::dock::config_dir().join("shortcuts.json")
}

impl Shortcuts {
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
        std::fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default())
    }
}

// --------------------------------------------------------------- learn mode

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LearnKind {
    Keyboard,
    Midi,
    Dmx,
}

/// A mappable control.
#[derive(Debug, Clone, PartialEq)]
pub enum LearnTarget {
    Address(String),
    /// A parameter, by id (address looked up when needed).
    Param(Id),
}

pub struct Learn {
    pub kind: LearnKind,
    /// Selected target: its address.
    pub target: Option<String>,
    /// Targets drawn last frame (screen rect, address).
    rects: Vec<(egui::Rect, String)>,
    /// Param id → address, refreshed while learning.
    addresses: HashMap<Id, String>,
    addresses_at: Option<Instant>,
    /// Click taken from the input this frame (hit-tested in `raw_input_hook`).
    clicked: Option<egui::Pos2>,
}

/// Frame-local registry in egui's memory (widgets are free functions).
#[derive(Clone, Default)]
struct LearnFrame {
    active: bool,
    targets: Vec<(egui::Rect, LearnTarget)>,
}

fn frame_id() -> egui::Id {
    egui::Id::unique("learn-frame")
}

/// Register a mappable control (no-op unless learn mode is on).
pub fn learn_target(ui: &egui::Ui, rect: egui::Rect, t: LearnTarget) {
    // Ask egui before taking its memory lock (it needs the same lock).
    let ctx = ui.ctx();
    if !ctx.data(|d| d.get_temp::<LearnFrame>(frame_id()).is_some_and(|f| f.active)) || !ui.is_rect_visible(rect) {
        return;
    }
    let r = rect.intersect(ui.clip_rect());
    ctx.data_mut(|d| d.get_temp_mut_or_default::<LearnFrame>(frame_id()).targets.push((r, t)));
}

/// Message for a learnt address: commands trigger, ranges follow a
/// fader / flip on a note, the rest trigger.
fn midi_action(is_range: bool, kind: MidiKind) -> KeyKind {
    match (is_range, kind) {
        (true, MidiKind::Cc | MidiKind::Bend) => KeyKind::Absolute,
        (true, MidiKind::Note) => KeyKind::Toggle { min: 0.0, max: 1.0 },
        (false, MidiKind::Cc | MidiKind::Bend) => KeyKind::Absolute,
        (false, MidiKind::Note) => KeyKind::Trigger,
    }
}

// -------------------------------------------------------------------- DMX

/// Art-Net input (Preferences › DMX inputs → DMX shortcuts).
#[derive(Default)]
pub struct DmxInState {
    pub rx: Option<DmxReceiver>,
    /// (adapter, node name) the receiver was opened with.
    opened: Option<(Option<std::net::Ipv4Addr>, String)>,
    pub error: Option<String>,
    retry_at: Option<Instant>,
    /// Last value per (input lumiverse, channel): commands fire on the rise.
    prev: HashMap<(u16, u16), u8>,
    /// Last change, for the learn window / Preferences.
    pub last: String,
}

// ------------------------------------------------------------------- MIDI

#[derive(Default)]
pub struct MidiState {
    inputs: Option<MidiInputs>,
    outputs: Option<MidiOutputs>,
    /// Ports seen at the last scan (inputs, outputs).
    pub ports: (Vec<String>, Vec<String>),
    scanned: Option<Instant>,
    /// Port scan running on a worker thread (listing ALSA ports takes
    /// 15–30 ms: done on the UI thread it was a hitch every 2 s).
    scanning: Option<std::sync::mpsc::Receiver<(Vec<String>, Vec<String>)>>,
    /// Config the connections were opened with.
    opened: Option<(Vec<String>, Vec<String>)>,
    /// Last feedback value per shortcut index.
    sent: HashMap<usize, u8>,
    last_feedback: Option<Instant>,
    /// Last message, for the Preferences page ("Last: CC7 ch1 = 64").
    pub last: String,
}

impl LavmfxApp {
    /// Once per frame: MIDI devices, input, learn mode, feedback.
    pub(super) fn tick_midi(&mut self, ctx: &egui::Context) {
        // Hot-plug: rescan every 2 s, reopen when the wanted set changes.
        if let Some(rx) = &self.midi.scanning {
            match rx.try_recv() {
                Ok(p) => {
                    self.midi.ports = p;
                    self.midi.scanning = None;
                    self.midi.scanned = Some(Instant::now());
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.midi.scanning = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        } else if self.midi.scanned.is_none_or(|t| t.elapsed().as_secs_f32() > 2.0) {
            self.midi.scanned = Some(Instant::now());
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(midi::ports());
            });
            self.midi.scanning = Some(rx);
        }
        let want_in: Vec<String> = self
            .midi
            .ports
            .0
            .iter()
            .filter(|n| !self.prefs.midi_devices.disabled_inputs.contains(n))
            .cloned()
            .collect();
        let want_out: Vec<String> = self
            .midi
            .ports
            .1
            .iter()
            .filter(|n| self.prefs.midi_devices.outputs.contains(n))
            .cloned()
            .collect();
        let want = (want_in, want_out);
        if self.midi.opened.as_ref() != Some(&want) {
            self.midi.inputs = MidiInputs::open(&want.0, self.smpte.mtc.clone())
                .map_err(|e| log::warn!("MIDI: {e}"))
                .ok();
            self.midi.outputs = MidiOutputs::open(&want.1).map_err(|e| log::warn!("MIDI: {e}")).ok();
            self.midi.sent.clear();
            log::info!("MIDI in: {:?}, out: {:?}", want.0, want.1);
            self.midi.opened = Some(want);
        }
        let msgs = self.midi.inputs.as_ref().map(MidiInputs::poll).unwrap_or_default();
        for m in msgs {
            self.midi.last = format!(
                "{}: {} = {}",
                m.device,
                MidiShortcut {
                    device: None,
                    kind: m.msg.kind,
                    channel: m.msg.channel,
                    number: m.msg.number,
                    address: String::new(),
                    action: KeyKind::Trigger
                }
                .label(),
                (m.msg.value * 127.0).round()
            );
            if self
                .learn
                .as_ref()
                .is_some_and(|l| l.kind == LearnKind::Midi && l.target.is_some())
            {
                // Note offs and zero CCs don't learn (a button's release).
                if m.msg.value > 0.0 {
                    self.learn_midi(&m);
                }
                continue;
            }
            let hits: Vec<MidiShortcut> = self.shortcuts.midi.iter().filter(|s| s.matches(&m)).cloned().collect();
            for s in hits {
                // Notes: press on note-on; note-off releases commands (piano), not toggles.
                if s.kind == MidiKind::Note && m.msg.value == 0.0 && s.action != KeyKind::Trigger {
                    continue;
                }
                let v = if s.kind == MidiKind::Note && s.action == KeyKind::Trigger {
                    Value::Bool(m.msg.value > 0.0)
                } else {
                    self.shortcut_value(&s.address, s.action, m.msg.value)
                };
                if v == Value::Bool(false) && !s.address.ends_with("/connect") {
                    continue;
                }
                let v = if v == Value::Bool(true) { Value::Trigger } else { v };
                self.control(&s.address, v);
            }
        }
        self.midi_feedback();
        let _ = ctx;
        self.prefs_win.midi_ports = self.midi.ports.clone();
        self.prefs_win.midi_last = self.midi.last.clone();
    }

    fn learn_midi(&mut self, m: &MidiIn) {
        let Some(l) = &self.learn else { return };
        let Some(address) = l.target.clone() else { return };
        let sel = self.control_selection();
        let is_range = matches!(control::read(&mut self.comp, &sel, &address), Some(Value::Float(_)));
        let s = MidiShortcut {
            device: Some(m.device.clone()),
            kind: m.msg.kind,
            channel: m.msg.channel,
            number: m.msg.number,
            address: address.clone(),
            action: midi_action(is_range, m.msg.kind),
        };
        // One control does one thing: drop its old mapping.
        self.shortcuts
            .midi
            .retain(|x| !(x.kind == s.kind && x.channel == s.channel && x.number == s.number && x.device == s.device));
        self.status = format!("MIDI {} → {address}", s.label());
        self.shortcuts.midi.push(s);
        self.save_shortcuts();
    }

    /// Mapped controls get their value back (LEDs, motor faders): params as
    /// CC value / note velocity, clip `connect` as 127 while playing.
    fn midi_feedback(&mut self) {
        if self.midi.outputs.is_none() || self.midi.last_feedback.is_some_and(|t| t.elapsed().as_millis() < 33) {
            return;
        }
        self.midi.last_feedback = Some(Instant::now());
        let sel = self.control_selection();
        let deck = self.comp.active_deck();
        let mut out = Vec::new();
        for (i, s) in self.shortcuts.midi.iter().enumerate() {
            let v = if let Some(rest) = s.address.strip_suffix("/connect") {
                // /composition/layers/L/clips/C
                let parts: Vec<&str> = rest.split('/').collect();
                match parts.as_slice() {
                    ["", "composition", "layers", l, "clips", c] => {
                        let (l, c) = (l.parse::<usize>().unwrap_or(0), c.parse::<usize>().unwrap_or(0));
                        let on = self
                            .playing
                            .get(l.wrapping_sub(1))
                            .and_then(Option::as_ref)
                            .is_some_and(|p| p.slot.deck == deck && p.slot.col + 1 == c);
                        if on { 1.0 } else { 0.0 }
                    }
                    _ => continue,
                }
            } else {
                match control::read(&mut self.comp, &sel, &s.address) {
                    Some(Value::Float(f)) => f,
                    Some(Value::Bool(b)) => b as u8 as f64,
                    _ => continue,
                }
            };
            let b = (v.clamp(0.0, 1.0) * 127.0).round() as u8;
            if self.midi.sent.get(&i) != Some(&b) {
                out.push((
                    i,
                    s.device.clone(),
                    MidiMsg {
                        kind: s.kind,
                        channel: s.channel,
                        number: s.number,
                        value: v,
                    },
                    b,
                ));
            }
        }
        let Some(o) = self.midi.outputs.as_mut() else { return };
        let names = o.names();
        for (i, dev, msg, b) in out {
            // Same-named output if open (most controllers), else all.
            let to = dev.filter(|d| names.contains(d));
            o.send(to.as_deref(), &msg);
            self.midi.sent.insert(i, b);
        }
    }

    /// Once per frame: the Art-Net receiver (while an input is on or DMX
    /// learn is open), DMX shortcuts, learning.
    pub(super) fn tick_dmx_in(&mut self) {
        let learning = self.learn.as_ref().is_some_and(|l| l.kind == LearnKind::Dmx);
        let want = (learning || self.prefs.dmx.inputs.iter().any(|i| i.enabled))
            .then(|| (self.prefs.dmx.adapter_ip(), self.prefs.dmx.node_name.clone()));
        let st = &mut self.dmx_in;
        let retry = st.error.is_some() && st.retry_at.is_some_and(|t| Instant::now() >= t);
        if want != st.opened || retry {
            st.rx = None;
            st.error = None;
            st.retry_at = None;
            if let Some((bind, name)) = &want {
                match DmxReceiver::start(*bind, lavmfx_io::dmx::ARTNET_PORT, name) {
                    Ok(r) => {
                        log::info!("Art-Net input on port {}", r.port);
                        st.rx = Some(r);
                    }
                    Err(e) => {
                        st.error = Some(format!("Art-Net input: {e} (port 6454 in use by another app?)"));
                        st.retry_at = Some(Instant::now() + std::time::Duration::from_secs(5));
                    }
                }
            }
            st.opened = want;
        }
        let Some(rx) = &st.rx else { return };
        let changes = rx.drain_changes();
        if changes.is_empty() {
            return;
        }
        let inputs = self.prefs.dmx.inputs.clone();
        for (u, ch, v) in changes {
            for input in inputs.iter().filter(|i| i.enabled && i.universe == u) {
                let c = i32::from(ch) + input.channel_offset;
                if !(1..=512).contains(&c) {
                    continue;
                }
                let (lum, c) = (input.lumiverse.max(1), c as u16);
                let prev = self.dmx_in.prev.insert((lum, c), v).unwrap_or(0);
                self.dmx_in.last = format!("lumiverse {lum}, channel {c} = {v}");
                if self
                    .learn
                    .as_ref()
                    .is_some_and(|l| l.kind == LearnKind::Dmx && l.target.is_some())
                {
                    if v > 0 {
                        self.learn_dmx(lum, c);
                    }
                    continue;
                }
                let hits: Vec<DmxShortcut> = self
                    .shortcuts
                    .dmx
                    .iter()
                    .filter(|s| s.lumiverse == lum && s.channel == c)
                    .cloned()
                    .collect();
                for s in hits {
                    let rising = prev < 128 && v >= 128;
                    let value = match s.action {
                        KeyKind::Absolute => Value::Float(f64::from(v) / 255.0),
                        _ if !rising => continue,
                        a => match self.shortcut_value(&s.address, a, 1.0) {
                            Value::Bool(true) => Value::Trigger,
                            x => x,
                        },
                    };
                    self.control(&s.address, value);
                }
            }
        }
    }

    fn learn_dmx(&mut self, lumiverse: u16, channel: u16) {
        let Some(address) = self.learn.as_ref().and_then(|l| l.target.clone()) else {
            return;
        };
        let sel = self.control_selection();
        let is_range = matches!(control::read(&mut self.comp, &sel, &address), Some(Value::Float(_)));
        let s = DmxShortcut {
            lumiverse,
            channel,
            address: address.clone(),
            // Faders follow the channel; commands fire when it crosses 50 %.
            action: if is_range { KeyKind::Absolute } else { KeyKind::Trigger },
        };
        if self.shortcuts.dmx.contains(&s) {
            return;
        }
        self.shortcuts
            .dmx
            .retain(|x| !(x.lumiverse == lumiverse && x.channel == channel));
        self.status = format!("{} → {address}", s.label());
        self.shortcuts.dmx.push(s);
        self.save_shortcuts();
    }

    pub(super) fn save_shortcuts(&mut self) {
        if let Err(e) = self.shortcuts.save() {
            self.status = format!("Can't save shortcuts: {e}");
        }
    }

    // ------------------------------------------------------------ learning

    pub(super) fn start_learn(&mut self, kind: LearnKind) {
        self.learn = Some(Learn {
            kind,
            target: None,
            rects: Vec::new(),
            addresses: HashMap::new(),
            addresses_at: None,
            clicked: None,
        });
    }

    /// `raw_input_hook`: in learn mode a click on a target is removed from the
    /// input (the control doesn't act) and selects it.
    pub(super) fn learn_raw_input(&mut self, raw: &mut egui::RawInput) {
        let Some(l) = &mut self.learn else { return };
        let mut hit = None;
        raw.events.retain(|e| match e {
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                ..
            } if l.rects.iter().any(|(r, _)| r.contains(*pos)) => {
                if *pressed {
                    hit = Some(*pos);
                }
                false
            }
            _ => true,
        });
        if hit.is_some() {
            l.clicked = hit;
        }
    }

    /// Start of the frame: tell widgets to register; keys while learning.
    pub(super) fn learn_begin(&mut self, ctx: &egui::Context) {
        let active = self.learn.is_some();
        ctx.data_mut(|d| {
            *d.get_temp_mut_or_default::<LearnFrame>(frame_id()) = LearnFrame {
                active,
                targets: Vec::new(),
            }
        });
        let Some(l) = &mut self.learn else { return };
        if let Some(p) = l.clicked.take() {
            // Smallest rect under the click (a param row inside a section…).
            l.target = l
                .rects
                .iter()
                .filter(|(r, _)| r.contains(p))
                .min_by(|a, b| a.0.area().total_cmp(&b.0.area()))
                .map(|(_, a)| a.clone());
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let mut leave = false;
        let mut remove = false;
        let mut key = None;
        ctx.input_mut(|i| {
            i.events.retain(|e| match e {
                egui::Event::Key {
                    key: k,
                    physical_key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                } => {
                    match k {
                        Key::Escape => leave = true,
                        Key::Delete | Key::Backspace => remove = true,
                        _ => key = Some((physical_key.unwrap_or(*k), *modifiers)),
                    }
                    false
                }
                _ => true,
            })
        });
        if leave {
            self.learn = None;
            return;
        }
        let Some(target) = l.target.clone() else { return };
        let kind = l.kind;
        if remove {
            match kind {
                LearnKind::Keyboard => self.shortcuts.keys.retain(|s| s.address != target),
                LearnKind::Midi => self.shortcuts.midi.retain(|s| s.address != target),
                LearnKind::Dmx => self.shortcuts.dmx.retain(|s| s.address != target),
            }
            self.save_shortcuts();
            return;
        }
        if let (LearnKind::Keyboard, Some((k, m))) = (kind, key) {
            let sel = self.control_selection();
            let is_range = matches!(control::read(&mut self.comp, &sel, &target), Some(Value::Float(_)));
            let s = KeyShortcut {
                key: k,
                shift: m.shift,
                ctrl: m.command,
                alt: m.alt,
                address: target.clone(),
                kind: if is_range {
                    KeyKind::Toggle { min: 0.0, max: 1.0 }
                } else {
                    KeyKind::Trigger
                },
            };
            self.shortcuts
                .keys
                .retain(|x| !(x.key == s.key && x.shift == s.shift && x.ctrl == s.ctrl && x.alt == s.alt));
            self.status = format!("Key {} → {target}", key_label(&s));
            self.shortcuts.keys.push(s);
            self.save_shortcuts();
        }
    }

    /// End of the frame: collect targets, draw the overlay and the panel.
    pub(super) fn learn_end(&mut self, ctx: &egui::Context) {
        let Some(l) = &mut self.learn else { return };
        let targets = ctx.data(|d| d.get_temp::<LearnFrame>(frame_id()).unwrap_or_default().targets);
        if l.addresses_at.is_none_or(|t| t.elapsed().as_secs_f32() > 1.0) {
            l.addresses = control::param_addresses(&self.comp);
            l.addresses_at = Some(Instant::now());
        }
        l.rects = targets
            .into_iter()
            .filter_map(|(r, t)| match t {
                LearnTarget::Address(a) => Some((r, a)),
                LearnTarget::Param(id) => l.addresses.get(&id).map(|a| (r, a.clone())),
            })
            .collect();
        // Labels of existing shortcuts per address.
        let mut labels: HashMap<&str, Vec<String>> = HashMap::new();
        match l.kind {
            LearnKind::Keyboard => {
                for s in &self.shortcuts.keys {
                    labels.entry(s.address.as_str()).or_default().push(key_label(s));
                }
            }
            LearnKind::Midi => {
                for s in &self.shortcuts.midi {
                    labels.entry(s.address.as_str()).or_default().push(s.label());
                }
            }
            LearnKind::Dmx => {
                for s in &self.shortcuts.dmx {
                    labels.entry(s.address.as_str()).or_default().push(s.label());
                }
            }
        }
        let pal = theme::cur();
        let p = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("learn-overlay"),
        ));
        for (r, a) in &l.rects {
            let sel = l.target.as_deref() == Some(a.as_str());
            let mapped = labels.get(a.as_str());
            let fill = if sel {
                pal.pending.gamma_multiply(0.45)
            } else if mapped.is_some() {
                pal.accent.gamma_multiply(0.35)
            } else {
                pal.accent.gamma_multiply(0.12)
            };
            p.rect_filled(*r, 2.0, fill);
            if sel {
                p.rect_stroke(*r, 2.0, egui::Stroke::new(2.0, pal.pending), egui::StrokeKind::Inside);
            }
            if let Some(m) = mapped {
                let font = egui::FontId::proportional(10.0);
                let text = m.join(" · ");
                let g = p.layout_no_wrap(text, font, pal.text_strong);
                let at = r.left_top() + egui::vec2(2.0, 1.0);
                p.rect_filled(
                    egui::Rect::from_min_size(at, g.size() + egui::vec2(4.0, 0.0)),
                    2.0,
                    pal.overlay,
                );
                p.galley(at + egui::vec2(2.0, 0.0), g, pal.text_strong);
            }
        }
        self.learn_window(ctx);
    }

    fn learn_window(&mut self, ctx: &egui::Context) {
        let Some(l) = &self.learn else { return };
        let kind = l.kind;
        let target = l.target.clone();
        let (title, what) = match kind {
            LearnKind::Keyboard => ("Edit Keyboard Shortcuts", "press a key"),
            LearnKind::Midi => ("Edit MIDI Shortcuts", "move / press a MIDI control"),
            LearnKind::Dmx => ("Edit DMX Shortcuts", "change a DMX channel on your desk"),
        };
        let mut open = true;
        let mut remove: Option<usize> = None;
        let mut clear = false;
        let mut done = false;
        egui::Window::new(title)
            .open(&mut open)
            .resizable(false)
            .default_pos(ctx.content_rect().right_top() + egui::vec2(-340.0, 60.0))
            .show(ctx, |ui| {
                ui.set_width(300.0);
                let pal = theme::pal(ui);
                match &target {
                    None => {
                        ui.label("Click a highlighted control.");
                    }
                    Some(a) => {
                        ui.label(RichText::new(a).monospace().color(pal.pending));
                        ui.label(format!("Now {what} to map it. Delete removes its shortcuts."));
                    }
                }
                if kind == LearnKind::Midi && !self.midi.last.is_empty() {
                    ui.label(RichText::new(format!("Last: {}", self.midi.last)).small().weak());
                }
                if kind == LearnKind::Dmx {
                    if let Some(e) = &self.dmx_in.error {
                        ui.label(RichText::new(e).small().color(pal.danger));
                    } else if self.prefs.dmx.inputs.iter().all(|i| !i.enabled) {
                        ui.label(
                            RichText::new("No DMX input: add one in Preferences › DMX.")
                                .small()
                                .color(pal.pending),
                        );
                    } else if !self.dmx_in.last.is_empty() {
                        ui.label(RichText::new(format!("Last: {}", self.dmx_in.last)).small().weak());
                    }
                }
                ui.separator();
                if let Some(a) = &target {
                    let rows: Vec<(usize, String)> = match kind {
                        LearnKind::Keyboard => self
                            .shortcuts
                            .keys
                            .iter()
                            .enumerate()
                            .filter(|(_, s)| &s.address == a)
                            .map(|(i, s)| (i, key_label(s)))
                            .collect(),
                        LearnKind::Midi => self
                            .shortcuts
                            .midi
                            .iter()
                            .enumerate()
                            .filter(|(_, s)| &s.address == a)
                            .map(|(i, s)| {
                                let dev = s.device.clone().unwrap_or_else(|| "any device".into());
                                (i, format!("{} · {dev}", s.label()))
                            })
                            .collect(),
                        LearnKind::Dmx => self
                            .shortcuts
                            .dmx
                            .iter()
                            .enumerate()
                            .filter(|(_, s)| &s.address == a)
                            .map(|(i, s)| (i, format!("Lumiverse {} · channel {}", s.lumiverse, s.channel)))
                            .collect(),
                    };
                    if rows.is_empty() {
                        ui.label(RichText::new("No shortcut yet.").weak());
                    }
                    for (i, t) in rows {
                        ui.horizontal(|ui| {
                            if super::widgets::icon_button(
                                ui,
                                super::widgets::Icon::Close,
                                false,
                                egui::vec2(18.0, 18.0),
                                true,
                            )
                            .on_hover_text("Remove")
                            .clicked()
                            {
                                remove = Some(i);
                            }
                            ui.label(t);
                        });
                    }
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    let n = match kind {
                        LearnKind::Keyboard => self.shortcuts.keys.len(),
                        LearnKind::Midi => self.shortcuts.midi.len(),
                        LearnKind::Dmx => self.shortcuts.dmx.len(),
                    };
                    ui.label(RichText::new(format!("{n} shortcuts")).weak());
                    if ui.button("Done (Esc)").clicked() {
                        done = true;
                    }
                    if ui
                        .button(if kind == LearnKind::Keyboard {
                            "Reset to defaults"
                        } else {
                            "Clear all"
                        })
                        .clicked()
                    {
                        clear = true;
                    }
                });
            });
        if let Some(i) = remove {
            match kind {
                LearnKind::Keyboard => {
                    self.shortcuts.keys.remove(i);
                }
                LearnKind::Midi => {
                    self.shortcuts.midi.remove(i);
                }
                LearnKind::Dmx => {
                    self.shortcuts.dmx.remove(i);
                }
            }
            self.save_shortcuts();
        }
        if clear {
            match kind {
                LearnKind::Keyboard => self.shortcuts.keys = default_keys(),
                LearnKind::Midi => self.shortcuts.midi.clear(),
                LearnKind::Dmx => self.shortcuts.dmx.clear(),
            }
            self.save_shortcuts();
        }
        if done || !open {
            self.learn = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_matching() {
        let s = MidiShortcut {
            device: Some("APC".into()),
            kind: MidiKind::Note,
            channel: 0,
            number: 60,
            address: "/composition/columns/1/connect".into(),
            action: KeyKind::Trigger,
        };
        assert_eq!(s.label(), "C3 ch1");
        let m = |dev: &str, n| MidiIn {
            device: dev.into(),
            msg: MidiMsg {
                kind: MidiKind::Note,
                channel: 0,
                number: n,
                value: 1.0,
            },
        };
        assert!(s.matches(&m("APC", 60)));
        assert!(!s.matches(&m("Other", 60)));
        assert!(!s.matches(&m("APC", 61)));
    }

    #[test]
    fn shortcuts_round_trip() {
        let mut s = Shortcuts::default();
        s.midi.push(MidiShortcut {
            device: None,
            kind: MidiKind::Cc,
            channel: 2,
            number: 7,
            address: "/composition/master".into(),
            action: KeyKind::Absolute,
        });
        let j = serde_json::to_string(&s).unwrap();
        let back: Shortcuts = serde_json::from_str(&j).unwrap();
        assert_eq!(back.keys.len(), s.keys.len());
        assert_eq!(back.keys[0].key, s.keys[0].key);
        assert_eq!(back.midi, s.midi);
    }
}
