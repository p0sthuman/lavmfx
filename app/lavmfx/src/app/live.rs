//! Live control (M7): control addresses from keyboard shortcuts (later
//! OSC / MIDI / REST), quantised clip launch (Beat Snap), composition undo.

use std::time::Instant;

use eframe::egui::{self, Key};
use lavmfx_core::Id;
use lavmfx_core::control::{self, Action, Outcome, Selection, Value};

use super::LavmfxApp;

/// What a shortcut does to its address.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum KeyKind {
    /// Press = trigger (commands; booleans toggle; choices step).
    Trigger,
    /// Range param: flip between `min` and `max` (normalised 0..1).
    Toggle { min: f64, max: f64 },
    /// Add units (tempo −/+).
    Step(f64),
    /// MIDI fader / knob: the control's position (0..1) is the value.
    Absolute,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KeyShortcut {
    #[serde(with = "key_name")]
    pub key: Key,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub address: String,
    pub kind: KeyKind,
}

/// egui keys stored by name ("Q", "1", "ArrowLeft"…).
mod key_name {
    use eframe::egui::Key;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(k: &Key, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(k.name())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Key, D::Error> {
        let n = String::deserialize(d)?;
        Key::from_name(&n).ok_or_else(|| D::Error::custom(format!("unknown key {n}")))
    }
}

/// The default keyboard map.
pub fn default_keys() -> Vec<KeyShortcut> {
    let mut v = Vec::new();
    let mut add = |key, shift, ctrl, address: &str, kind| {
        v.push(KeyShortcut {
            key,
            shift,
            ctrl,
            alt: false,
            address: address.into(),
            kind,
        })
    };
    use KeyKind::*;
    let digits = [
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
    ];
    for (i, k) in digits.iter().enumerate() {
        add(
            *k,
            true,
            false,
            &format!("/composition/columns/{}/connect", i + 1),
            Trigger,
        );
    }
    for (i, k) in digits.iter().take(3).enumerate() {
        add(
            *k,
            false,
            false,
            &format!("/composition/layers/{}/select", i + 1),
            Trigger,
        );
    }
    let row = [Key::Q, Key::W, Key::E, Key::R, Key::T, Key::Y, Key::U, Key::I];
    for (i, k) in row.iter().enumerate() {
        add(
            *k,
            false,
            false,
            &format!("/composition/selectedlayer/clips/{}/connect", i + 1),
            Trigger,
        );
    }
    add(
        Key::ArrowLeft,
        false,
        false,
        "/composition/selectedlayer/connectprevclip",
        Trigger,
    );
    add(
        Key::ArrowRight,
        false,
        false,
        "/composition/selectedlayer/connectnextclip",
        Trigger,
    );
    add(Key::ArrowLeft, true, false, "/composition/connectprevcolumn", Trigger);
    add(Key::ArrowRight, true, false, "/composition/connectnextcolumn", Trigger);
    add(Key::ArrowLeft, true, true, "/composition/selectprevdeck", Trigger);
    add(Key::ArrowRight, true, true, "/composition/selectnextdeck", Trigger);
    add(
        Key::Space,
        false,
        false,
        "/composition/tempocontroller/tempotap",
        Trigger,
    );
    add(
        Key::Comma,
        false,
        false,
        "/composition/tempocontroller/tempo",
        Step(-1.0),
    );
    add(
        Key::Period,
        false,
        false,
        "/composition/tempocontroller/tempo",
        Step(1.0),
    );
    add(Key::Slash, false, false, "/composition/tempocontroller/resync", Trigger);
    add(Key::B, false, false, "/composition/selectedlayer/bypassed", Trigger);
    add(Key::S, false, false, "/composition/selectedlayer/solo", Trigger);
    add(Key::X, false, false, "/composition/selectedlayer/clear", Trigger);
    add(Key::X, true, false, "/composition/disconnectall", Trigger);
    add(Key::B, true, false, "/composition/bypassed", Trigger);
    let full = Toggle { min: 0.0, max: 1.0 };
    add(
        Key::A,
        false,
        false,
        "/composition/selectedlayer/audio/volume",
        Toggle { min: 0.063, max: 1.0 },
    );
    add(Key::M, false, false, "/composition/selectedlayer/master", full);
    add(Key::V, false, false, "/composition/selectedlayer/video/opacity", full);
    add(Key::M, true, false, "/composition/master", full);
    v
}

/// Debug (with `LAVMFX_SCREENSHOT`): `LAVMFX_SCREENSHOT_KEYS="1.0:X,2.0:shift+Num1,3:ctrl+Z,4:click 400 300"`
/// presses each key (or clicks there) once at that many seconds after start.
/// Pointer steps: `click X Y`, `down X Y`, `move X Y`, `up X Y` (a drag is
/// down, a few moves, up), each optionally after `ctrl+` / `shift+` / `alt+`.
pub fn probe_keys(started: Instant, raw: &mut egui::RawInput) {
    use std::sync::Mutex;
    static DONE: Mutex<usize> = Mutex::new(0);
    // Modifiers of the last step are let go a frame later (egui keeps the
    // last state it saw in a frame, and the step's frame must see them).
    static RELEASE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let Ok(spec) = std::env::var("LAVMFX_SCREENSHOT_KEYS") else {
        return;
    };
    if RELEASE.swap(false, std::sync::atomic::Ordering::Relaxed) {
        raw.events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    }
    let mut done = DONE.lock().unwrap_or_else(|e| e.into_inner());
    let t = started.elapsed().as_secs_f64();
    for item in spec.split(',').skip(*done) {
        let Some((at, keys)) = item.split_once(':') else { break };
        if at.trim().parse::<f64>().unwrap_or(f64::MAX) > t {
            break;
        }
        *done += 1;
        // Pointer steps, with optional modifiers in front.
        let (mods, last) = keys.trim().rsplit_once('+').unwrap_or(("", keys.trim()));
        if let Some((verb, xy)) = last.split_once(' ')
            && matches!(verb, "click" | "down" | "move" | "up")
        {
            let mut modifiers = egui::Modifiers::NONE;
            for m in mods.split('+').map(|m| m.trim().to_ascii_lowercase()) {
                match m.as_str() {
                    "shift" => modifiers.shift = true,
                    "ctrl" => {
                        modifiers.ctrl = true;
                        modifiers.command = true;
                    }
                    "alt" => modifiers.alt = true,
                    _ => {}
                }
            }
            let mut it = xy.split_whitespace().filter_map(|v| v.parse::<f32>().ok());
            if let (Some(x), Some(y)) = (it.next(), it.next()) {
                let pos = egui::pos2(x, y);
                raw.events.push(egui::Event::ModifiersChanged(modifiers));
                raw.events.push(egui::Event::PointerMoved(pos));
                let presses: &[bool] = match verb {
                    "click" => &[true, false],
                    "down" => &[true],
                    "up" => &[false],
                    _ => &[],
                };
                for &pressed in presses {
                    raw.events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers,
                    });
                }
                // The modifiers aren't held any longer than the step.
                if verb != "down" {
                    RELEASE.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            continue;
        }
        let mut modifiers = egui::Modifiers::NONE;
        let mut key = None;
        for part in keys.split('+') {
            match part.trim().to_ascii_lowercase().as_str() {
                "shift" => modifiers.shift = true,
                "ctrl" => {
                    modifiers.ctrl = true;
                    modifiers.command = true;
                }
                "alt" => modifiers.alt = true,
                _ => key = Key::from_name(part.trim()),
            }
        }
        let Some(key) = key else { continue };
        raw.events.push(egui::Event::ModifiersChanged(modifiers));
        RELEASE.store(true, std::sync::atomic::Ordering::Relaxed);
        for pressed in [true, false] {
            raw.events.push(egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: false,
                modifiers,
            });
        }
    }
}

/// Beats per Beat Snap option ("None" = 0).
fn snap_beats(name: &str) -> f64 {
    match name {
        "8 Bars" => 32.0,
        "4 Bars" => 16.0,
        "2 Bars" => 8.0,
        "1 Bar" => 4.0,
        "1/2 Bar" => 2.0,
        "1/4 Bar" => 1.0,
        _ => 0.0,
    }
}

/// First beat on the `snap` grid at or after `beats` (a press just after a
/// boundary still counts as on it).
pub fn next_snap(beats: f64, snap: f64) -> f64 {
    const LATE: f64 = 0.05;
    let k = (beats / snap).floor();
    if beats - k * snap < LATE {
        beats
    } else {
        (k + 1.0) * snap
    }
}

/// A launch waiting for its Beat Snap boundary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Launch {
    Clip { layer: usize, column: usize, clip: Id },
    Column(usize),
}

impl LavmfxApp {
    pub(super) fn control_selection(&self) -> Selection {
        Selection {
            layer: self
                .selected_layer
                .or(self.focus_layer())
                .or((!self.comp.layers.is_empty()).then_some(0)),
            clip: self.selected_clip,
        }
    }

    /// Send `v` to a control address (keyboard, OSC, MIDI, REST all end here).
    pub(crate) fn control(&mut self, address: &str, v: Value) -> Outcome {
        let sel = self.control_selection();
        let out = control::apply(&mut self.comp, &sel, address, &v);
        log::debug!("control {address} {v:?} → {out:?}");
        match &out {
            Outcome::Action(a) => self.run_action(a.clone()),
            Outcome::Changed => {
                let bpm = self.comp.tempo_controller.tempo.value;
                if (bpm - self.clock.bpm).abs() > 1e-9 {
                    self.clock.set_bpm(bpm);
                }
            }
            _ => {}
        }
        self.history_poke = Some(Instant::now());
        out
    }

    fn run_action(&mut self, a: Action) {
        let layers = self.comp.layers.len();
        let cols = self.comp.columns.len();
        match a {
            Action::ConnectClip { layer, column, on } => {
                if on && layer < layers {
                    self.trigger(layer, column);
                }
            }
            Action::ClearClip { layer, column } => {
                if layer < layers && column < cols {
                    self.eject(layer, column);
                }
            }
            Action::SelectClip { layer, column } => {
                if layer < layers && column < cols {
                    self.select_only((layer, column));
                }
            }
            Action::ConnectColumn(c) => {
                if c < cols {
                    self.launch_column(c);
                }
            }
            Action::ConnectPrevColumn | Action::ConnectNextColumn => {
                let deck = self.comp.active_deck();
                let cur = self.launched_column.filter(|(d, _)| *d == deck).map(|(_, c)| c);
                // The spare column on the right is never launched.
                let n = cols.saturating_sub(1).max(1);
                let next = match (cur, a == Action::ConnectNextColumn) {
                    (None, _) => 0,
                    (Some(c), true) => (c + 1) % n,
                    (Some(c), false) => (c + n - 1) % n,
                };
                self.launch_column(next);
            }
            Action::ConnectPrevClip(l) | Action::ConnectNextClip(l) => {
                let fwd = matches!(a, Action::ConnectNextClip(_));
                let Some(layer) = self.comp.layers.get(l) else { return };
                let filled: Vec<usize> = (0..layer.clips.len()).filter(|&c| !layer.clips[c].is_empty()).collect();
                if filled.is_empty() {
                    return;
                }
                let deck = self.comp.active_deck();
                let cur = self
                    .playing
                    .get(l)
                    .and_then(Option::as_ref)
                    .filter(|p| p.slot.deck == deck)
                    .map(|p| p.slot.col);
                let i = match cur.and_then(|c| filled.iter().position(|&f| f == c)) {
                    None => 0,
                    Some(i) if fwd => (i + 1) % filled.len(),
                    Some(i) => (i + filled.len() - 1) % filled.len(),
                };
                self.trigger(l, filled[i]);
            }
            Action::ClearLayer(l) => {
                if l < layers {
                    self.launch_queue
                        .retain(|(_, q)| !matches!(q, Launch::Clip { layer, .. } if *layer == l));
                    self.stop_layer(l);
                }
            }
            Action::DisconnectAll => {
                self.launch_queue.clear();
                for l in 0..layers {
                    self.stop_layer(l);
                }
                self.launched_column = None;
            }
            Action::SelectLayer(l) => {
                if l < layers {
                    self.selected_layer = Some(l);
                }
            }
            Action::SelectDeck(d) => self.select_deck(d),
            Action::SelectPrevDeck | Action::SelectNextDeck => {
                let n = self.comp.decks.len().max(1);
                let cur = self.comp.active_deck();
                let d = if a == Action::SelectNextDeck {
                    (cur + 1) % n
                } else {
                    (cur + n - 1) % n
                };
                self.select_deck(d);
            }
            Action::TempoTap => {
                self.tap_flash = Some(Instant::now());
                if let Some(b) = self.clock.tap() {
                    self.comp.tempo_controller.tempo.set(b);
                    self.clock.set_bpm(self.comp.tempo_controller.tempo.value);
                }
            }
            Action::Resync => self.resync(),
        }
    }

    /// Keyboard shortcuts (not while a text field has focus or keys are being learnt).
    pub(super) fn key_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.learn.is_some() {
            return;
        }
        let mut hits = Vec::new();
        ctx.input_mut(|i| {
            i.events.retain(|e| {
                let egui::Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    repeat,
                    modifiers,
                } = e
                else {
                    return true;
                };
                let k = physical_key.unwrap_or(*key);
                let found = self.shortcuts.keys.iter().position(|s| {
                    s.key == k && s.shift == modifiers.shift && s.ctrl == modifiers.command && s.alt == modifiers.alt
                });
                match found {
                    Some(n) => {
                        if !repeat {
                            hits.push(n);
                        }
                        false
                    }
                    None => true,
                }
            })
        });
        for n in hits {
            let s = self.shortcuts.keys[n].clone();
            let v = self.shortcut_value(&s.address, s.kind, 1.0);
            self.control(&s.address, v);
        }
    }

    /// Value a shortcut of `kind` sends; `pos` = MIDI position 0..1 (keys: 1).
    pub(super) fn shortcut_value(&mut self, address: &str, kind: KeyKind, pos: f64) -> Value {
        match kind {
            KeyKind::Trigger => Value::Trigger,
            KeyKind::Step(d) => Value::Step(d),
            KeyKind::Absolute => Value::Float(pos),
            KeyKind::Toggle { min, max } => {
                let sel = self.control_selection();
                match control::read(&mut self.comp, &sel, address) {
                    Some(Value::Float(f)) if f >= (min + max) * 0.5 => Value::Float(min),
                    _ => Value::Float(max),
                }
            }
        }
    }

    /// Bar 1 now (with Link: for every app in the session).
    pub(super) fn resync(&mut self) {
        match self.link.as_mut().filter(|l| l.enabled()) {
            Some(l) => l.restart_bar(),
            None => self.clock.resync(),
        }
    }

    /// Ableton Link: the session's tempo and beat drive our clock; our
    /// tempo changes (typed, tapped) go to the session.
    pub(super) fn tick_link(&mut self) {
        if !self.prefs.ableton_link {
            if let Some(l) = &mut self.link {
                l.set_enabled(false);
            }
            return;
        }
        let bpm = self.comp.tempo_controller.tempo.value;
        let l = self.link.get_or_insert_with(|| lavmfx_io::link::Link::new(bpm));
        l.set_enabled(true);
        let (tempo, beat) = l.sync(bpm);
        if !self.clock.paused() {
            self.comp.tempo_controller.tempo.set(tempo);
            self.clock.follow(self.comp.tempo_controller.tempo.value, beat);
        }
    }

    /// Peers in the Link session (toolbar).
    pub(super) fn link_peers(&self) -> Option<u64> {
        self.link.as_ref().filter(|l| l.enabled()).map(|l| l.peers())
    }

    // ------------------------------------------------------------ Beat Snap

    /// Beat Snap of slot (layer, column): the clip's, or the composition's.
    fn clip_snap(&self, li: usize, ci: usize) -> f64 {
        let comp = &self.comp.clipbeatsnap.value;
        match self.comp.layers.get(li).and_then(|l| l.clips.get(ci)) {
            Some(c) if c.beatsnap.value != "Composition Determined" => snap_beats(&c.beatsnap.value),
            _ => snap_beats(comp),
        }
    }

    /// Wait for the Beat Snap boundary, or run `l` now. True if queued.
    pub(super) fn queue_launch(&mut self, l: Launch) -> bool {
        let snap = match l {
            Launch::Clip { layer, column, .. } => self.clip_snap(layer, column),
            Launch::Column(_) => snap_beats(&self.comp.clipbeatsnap.value),
        };
        if snap <= 0.0 || self.clock.paused() {
            return false;
        }
        let now = self.clock.beats();
        let at = next_snap(now, snap);
        if at <= now {
            return false;
        }
        // A newer press replaces what waits on the same layer / column.
        self.launch_queue.retain(|(_, q)| match (q, &l) {
            (Launch::Clip { layer: a, .. }, Launch::Clip { layer: b, .. }) => a != b,
            (Launch::Column(_), Launch::Column(_)) => false,
            _ => true,
        });
        self.launch_queue.push((at, l));
        self.launch_deck = Some(self.comp.active_deck());
        true
    }

    /// Autopilot: a layer whose clip has played its Loops (× its length)
    /// moves to the next / previous / a random filled clip (clip Action can
    /// override). Only the deck on view; not while a clip is opening.
    pub(super) fn tick_autopilot(&mut self) {
        use lavmfx_core::autopilot::{after, pick};
        let deck = self.comp.active_deck();
        for li in 0..self.playing.len() {
            let Some(pb) = &self.playing[li] else { continue };
            if pb.slot.deck != deck || self.opening.get(li).is_some_and(Option::is_some) {
                continue;
            }
            let Some(layer) = self.comp.layers.get(li) else {
                continue;
            };
            let Some(clip) = layer.clips.get(pb.slot.col).filter(|c| c.id == pb.slot.clip) else {
                continue;
            };
            let Some(step) = after(layer, clip) else { continue };
            let dur = pb.duration();
            if dur <= 0.0 || pb.played < clip.autopilot_loops() * dur {
                continue;
            }
            let filled: Vec<usize> = layer
                .clips
                .iter()
                .enumerate()
                .filter(|(_, c)| !c.is_empty() && !c.is_missing())
                .map(|(i, _)| i)
                .collect();
            let rnd = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let from = pb.slot.col;
            // Count again from here: a clip that fails to open isn't retried every frame.
            if let Some(pb) = &mut self.playing[li] {
                pb.played = 0.0;
            }
            if let Some(col) = pick(&filled, from, step, rnd) {
                log::debug!("autopilot: layer {} column {} → {}", li + 1, from + 1, col + 1);
                self.trigger_now(li, col);
            }
        }
    }

    /// Run launches whose boundary has come.
    pub(super) fn tick_launches(&mut self) {
        if self.launch_queue.is_empty() {
            return;
        }
        let deck = self.comp.active_deck();
        // A deck switch drops what waits (its slots are gone).
        if self.launch_deck != Some(deck) {
            self.launch_queue.clear();
            return;
        }
        let now = self.clock.beats();
        let (due, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut self.launch_queue)
            .into_iter()
            .partition(|(at, _)| *at <= now || self.clock.paused());
        self.launch_queue = wait;
        for (_, l) in due {
            match l {
                Launch::Clip { layer, column, clip } => {
                    let same = self
                        .comp
                        .layers
                        .get(layer)
                        .and_then(|x| x.clips.get(column))
                        .map(|c| c.id);
                    if same == Some(clip) {
                        self.trigger_now(layer, column);
                    }
                }
                Launch::Column(c) => {
                    if c < self.comp.columns.len() {
                        self.launch_column_now(c);
                    }
                }
            }
        }
    }

    /// Slot (layer, column) waits for its Beat Snap boundary.
    pub(super) fn is_queued(&self, li: usize, ci: usize) -> bool {
        self.launch_queue.iter().any(|(_, l)| match l {
            Launch::Clip { layer, column, .. } => (*layer, *column) == (li, ci),
            Launch::Column(c) => *c == ci,
        })
    }

    // ----------------------------------------------------------------- undo

    /// Commit an undo step once the user's interaction has settled.
    pub(super) fn track_history(&mut self, ctx: &egui::Context) {
        let input = ctx.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::PointerButton { pressed: false, .. }
                        | egui::Event::Key { pressed: true, .. }
                        | egui::Event::Text(_)
                        | egui::Event::MouseWheel { .. }
                        | egui::Event::Paste(_)
                        | egui::Event::Cut
                )
            }) || !i.raw.dropped_files.is_empty()
        });
        // Debug: `LAVMFX_UNDO_PROBE` checks every frame (playback alone must not add steps).
        if input || (std::env::var_os("LAVMFX_UNDO_PROBE").is_some() && self.history_poke.is_none()) {
            self.history_poke = Some(Instant::now());
        }
        // A snapshot's key came back from the worker: record the step.
        if let Some(rx) = &self.history_job {
            match rx.try_recv() {
                Ok((c, k)) => {
                    self.history_job = None;
                    if self.history.commit_keyed(c, k) {
                        log::info!("undo step");
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.history_job = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let busy = ctx.input(|i| i.pointer.any_down()) || ctx.egui_wants_keyboard_input();
        if let Some(t) = self.history_poke
            && !busy
            && self.history_job.is_none()
            && t.elapsed().as_secs_f32() > 0.3
        {
            self.history_poke = None;
            // Snapshot now (a cheap clone); the comparison key is built on a worker
            // thread so big compositions don't freeze the UI after every edit.
            let snap = self.comp.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let k = lavmfx_core::history::undo_key(&snap);
                let _ = tx.send((snap, k));
            });
            self.history_job = Some(rx);
        }
    }

    /// Wait for a snapshot still being keyed (before undo / redo).
    fn finish_history_job(&mut self) {
        if let Some(rx) = self.history_job.take()
            && let Ok((c, k)) = rx.recv()
        {
            self.history.commit_keyed(c, k);
        }
    }

    pub(super) fn can_undo(&self) -> bool {
        self.history.can_undo() || self.history_poke.is_some() || self.history_job.is_some()
    }

    pub(super) fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Ctrl+Z / Ctrl+Shift+Z.
    pub(super) fn undo(&mut self, redo: bool) {
        self.finish_history_job();
        self.history.commit(&self.comp);
        self.history_poke = None;
        let got = if redo {
            self.history.redo(&self.comp)
        } else {
            self.history.undo(&self.comp)
        };
        let Some(mut c) = got else { return };
        let old_layers: Vec<Id> = self.comp.layers.iter().map(|l| l.id).collect();
        // Keep what isn't undone: the deck on view and the tempo.
        c.select_deck(self.comp.active_deck());
        c.tempo_controller = self.comp.tempo_controller.clone();
        self.comp = c;
        self.reconcile_layers(old_layers);
        self.launch_queue.clear();
        self.status = if redo { "Redo".into() } else { "Undo".into() };
        log::debug!("{}", self.status);
    }

    /// After the composition was replaced by an undo step: move per-layer
    /// state (playing clips, GPU slots) to where each layer now is, drop what
    /// belongs to layers or clips that are gone.
    fn reconcile_layers(&mut self, mut cur: Vec<Id>) {
        let n = cur.len();
        self.playing.resize_with(n, || None);
        self.opening.resize_with(n, || None);
        self.outgoing.resize_with(n, || None);
        let new: Vec<Id> = self.comp.layers.iter().map(|l| l.id).collect();
        for i in (0..cur.len()).rev() {
            if !new.contains(&cur[i]) {
                self.playing.remove(i);
                self.opening.remove(i);
                self.outgoing.remove(i);
                self.compositor.remove_layer(i);
                cur.remove(i);
            }
        }
        for (i, id) in new.iter().enumerate() {
            match cur.iter().position(|x| x == id) {
                Some(j) if j == i => {}
                Some(j) => {
                    let x = self.playing.remove(j);
                    self.playing.insert(i, x);
                    let x = self.opening.remove(j);
                    self.opening.insert(i, x);
                    let x = self.outgoing.remove(j);
                    self.outgoing.insert(i, x);
                    self.compositor.move_layer(&self.gpu.device, j, i);
                    cur.remove(j);
                    cur.insert(i, *id);
                }
                None => {
                    self.playing.insert(i, None);
                    self.opening.insert(i, None);
                    self.outgoing.insert(i, None);
                    self.compositor.insert_layer(&self.gpu.device, i);
                    cur.insert(i, *id);
                }
            }
        }
        // Clips: follow them to their column; stop those that are gone.
        for li in 0..new.len() {
            let clips: Vec<Id> = self.comp.layers[li].clips.iter().map(|c| c.id).collect();
            let find = |id: Id| clips.iter().position(|&c| c == id);
            let mut gone = false;
            if let Some(pb) = &mut self.playing[li] {
                pb.slot.layer = li;
                match find(pb.slot.clip) {
                    Some(c) => pb.slot.col = c,
                    None => gone = true,
                }
            }
            if let Some(o) = &mut self.opening[li] {
                o.slot.layer = li;
                match find(o.slot.clip) {
                    Some(c) => o.slot.col = c,
                    None => self.opening[li] = None,
                }
            }
            if let Some(pb) = self.outgoing[li].as_mut().and_then(|o| o.pb.as_mut()) {
                pb.slot.layer = li;
            }
            if gone {
                self.stop_layer_now(li);
            }
        }
        let (l, c) = (self.comp.layers.len(), self.comp.columns.len());
        self.selected_layer = self.selected_layer.filter(|&x| x < l);
        self.selected_group = self.selected_group.filter(|&g| g < self.comp.layergroups.len());
        self.selected_clip = self.selected_clip.filter(|&(x, y)| x < l && y < c);
        self.selection.retain(|&(x, y)| x < l && y < c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_grid() {
        assert_eq!(next_snap(0.0, 4.0), 0.0, "on the bar: now");
        assert_eq!(next_snap(4.02, 4.0), 4.02, "just late: now");
        assert_eq!(next_snap(4.5, 4.0), 8.0);
        assert_eq!(next_snap(7.9, 1.0), 8.0);
        assert_eq!(snap_beats("1/2 Bar"), 2.0);
        assert_eq!(snap_beats("None"), 0.0);
    }

    #[test]
    fn default_keys_are_unique() {
        let k = default_keys();
        for (i, a) in k.iter().enumerate() {
            for b in &k[i + 1..] {
                assert!(
                    !(a.key == b.key && a.shift == b.shift && a.ctrl == b.ctrl && a.alt == b.alt),
                    "{:?} twice",
                    a.key
                );
            }
        }
    }
}
