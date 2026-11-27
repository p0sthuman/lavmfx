//! `.avc` composition import (basic): decks, layers, columns and video
//! clips with their file paths, plus the few params saved for them.
//!
//! Format notes (from version 7.x files):
//! * `<Composition numLayers numColumns currentDeckIndex>` → `<CompositionInfo
//!   name>` with `<DeckInfo name>`s, `<Params>` (Speed…), `<Layer layerIndex>`s,
//!   `<TempoController>`, `<Deck deckIndex numColumns>` holding
//!   `<Clip layerIndex columnIndex>`s.
//! * Params are sparse: only values that differ from the default are written,
//!   as `<Param*/ParamRange/ParamChoice name=… value=…>` inside `<Params>`.
//! * Blend/transition modes are `ChoosableMixer`s whose `RenderPass type`
//!   names the mode (`Add50` = "50 Add"); the UINT64 value is a plugin id.
//! * Clips: path in `PreloadData/VideoFile@value`, length in
//!   `PhaseSourceTransportTimeline@defaultMillisecondsDuration`, size in
//!   `VideoSource@width/height`. Generators/Spout/routers have no file.
//! * Effect chains: `VideoTrack › RenderPass type="RenderPassChain"` on clips,
//!   layers, groups and the composition (all have a Transform); by `index`:
//!   `TransformEffect`, then `DryWetEffect`s (Bypassed, Opacity, `Mixer` blend)
//!   wrapping the effect (`baseType="Effect"`, `uniqueTypeId` = REST `idstring`).
//!   Choices are saved as indexes (`INT32`). Animations (`PhaseSourceTimeline`,
//!   `PhaseSourceDashboardLink`, `PhaseSourceFFT`): see `import_drive`.
//! * Clip transport: `Transport › Position › DurationSource / PhaseSourceTransportTimeline`
//!   (see `import_transport`); clip blend override = the clip's `Blend` mixer;
//!   group `Speed`. Spout clips (`SpoutInfo serverName`) become shared-video clips (`ClipSource::Share`).
//!
//! Media files are never opened or copied here; paths go through
//! [`PathResolver`] and missing ones stay in the composition, flagged.

use std::collections::BTreeSet;
use std::path::Path;

use lavmfx_core::defaults::{LAYER_BLEND_MODES, MIX_MODES};
use lavmfx_core::model::{Column, DeckContent, VideoFileInfo};
use lavmfx_core::{Clip, Composition, FrameRate};
use roxmltree::{Document, Node};

use crate::paths::PathResolver;
use crate::{ProjectError, Result};

/// What happened during an import (shown to the user).
#[derive(Debug, Default, Clone)]
pub struct ImportReport {
    /// Video clips imported.
    pub clips: usize,
    /// Saved paths of clips whose file wasn't found (kept as missing clips).
    pub missing: Vec<String>,
    /// Clips without a file we can't make (Spout, unknown sources, audio only): skipped.
    pub skipped: usize,
    /// Source (generator) clips imported.
    pub sources: usize,
    /// Clips set to BPM Sync (loop length from `numManualBeats`).
    pub bpm_sync: usize,
    /// Blend / transition modes without an equivalent (set to Alpha).
    pub unknown_modes: BTreeSet<String>,
    /// Layer groups imported.
    pub groups: usize,
    /// Effects imported into clip / layer / group / composition chains.
    pub effects: usize,
    /// Params linked to a dashboard knob (P › Dashboard).
    pub dashboard_links: usize,
    /// Params animated by Timeline / BPM Sync / FFT (with range, envelope).
    pub animations: usize,
    /// Dashboard knobs with a saved value or name.
    pub dashboard_knobs: usize,
    /// Layers whose autopilot was set (imported as Off: the saved
    /// value is an internal enum we haven't mapped yet).
    pub autopilot_layers: usize,
    /// Clips with a saved autopilot Loops count.
    pub autopilot_loops: usize,
    /// Clips with saved transport settings (speed, direction, play mode, duration).
    pub transports: usize,
    /// Clips whose own blend mode overrides the layer's.
    pub clip_blends: usize,
    /// Groups with a saved Speed.
    pub group_speeds: usize,
    /// Spout clips (imported as shared-video clips) and their sender names.
    pub spout: usize,
    pub spout_senders: BTreeSet<String>,
    /// Saved effects not in our catalogue (saved name), skipped.
    pub unknown_effects: BTreeSet<String>,
    /// Drive letters found on mount points, e.g. `('D', "/mnt/local")`.
    pub drives: Vec<(char, std::path::PathBuf)>,
}

impl ImportReport {
    fn count_drives(&mut self, (links, anims): (usize, usize)) {
        self.dashboard_links += links;
        self.animations += anims;
    }

    /// One-line summary for the status bar.
    pub fn summary(&self) -> String {
        let mut s = format!("{} clips", self.clips);
        if !self.missing.is_empty() {
            s += &format!(", {} missing", self.missing.len());
        }
        if self.skipped > 0 {
            s += &format!(", {} skipped (no file)", self.skipped);
        }
        if self.effects > 0 {
            s += &format!(", {} effects", self.effects);
        }
        s
    }
}

pub struct Imported {
    pub composition: Composition,
    pub report: ImportReport,
}

pub fn import(path: &Path) -> Result<Imported> {
    let text = std::fs::read_to_string(path)?;
    let mut resolver = PathResolver::new(Some(path));
    let fallback = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    from_str(&text, &mut resolver, &fallback)
}

fn bad(msg: impl Into<String>) -> ProjectError {
    ProjectError::Avc(msg.into())
}

fn attr<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attribute(name)
}

fn num<T: std::str::FromStr>(n: Node, name: &str) -> Option<T> {
    n.attribute(name)?.trim().parse().ok()
}

/// First child element named `tag` (and `name` attribute, if given).
fn child<'a, 'i>(n: Node<'a, 'i>, tag: &str, name: Option<&str>) -> Option<Node<'a, 'i>> {
    n.children()
        .find(|c| c.has_tag_name(tag) && name.is_none_or(|w| c.attribute("name") == Some(w)))
}

/// Value of the param `name` in `n`'s direct `<Params>` children.
fn param<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.children()
        .filter(|c| c.has_tag_name("Params"))
        .flat_map(|p| p.children())
        .find(|p| p.is_element() && p.tag_name().name().starts_with("Param") && p.attribute("name") == Some(name))?
        .attribute("value")
}

/// The `<Param*>` element `name` in `n`'s direct `<Params>` children.
fn param_node<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children()
        .filter(|c| c.has_tag_name("Params"))
        .flat_map(|p| p.children())
        .find(|p| p.is_element() && p.tag_name().name().starts_with("Param") && p.attribute("name") == Some(name))
}

/// Saved value + animation of the range param `name` of `n` into `r`.
fn import_range(n: Node, name: &str, r: &mut lavmfx_core::RangeParam, report: &mut ImportReport) {
    let Some(p) = param_node(n, name) else { return };
    if let Some(v) = num::<f64>(p, "value") {
        r.set(v);
    }
    match import_drive(p, r) {
        Some(lavmfx_core::drive::Source::Dashboard) => report.dashboard_links += 1,
        Some(_) => report.animations += 1,
        None => {}
    }
}

fn param_f64(n: Node, name: &str) -> Option<f64> {
    param(n, name)?.trim().parse().ok()
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Mode name for a mixer `RenderPass type` (`Add50` → "50 Add",
/// `LumaKey` → "Luma Key"), from `options`.
pub fn mode_name(kind: &str, options: &[&'static str]) -> Option<&'static str> {
    let k = norm(kind);
    let k = match k.strip_suffix("50") {
        Some(base) => format!("50{base}"),
        None => k,
    };
    options.iter().copied().find(|o| norm(o) == k)
}

/// Mode name of the `ChoosableMixer name=…` under `track`.
fn mixer_mode<'a>(track: Node<'a, '_>, mixer: &str) -> Option<&'a str> {
    let m = child(track, "ChoosableMixer", Some(mixer))?;
    child(m, "RenderPass", None)?.attribute("type")
}

pub fn from_str(text: &str, resolver: &mut PathResolver, fallback_name: &str) -> Result<Imported> {
    let doc = Document::parse(text).map_err(|e| bad(e.to_string()))?;
    let root = doc.root_element();
    if !root.has_tag_name("Composition") {
        return Err(bad("not an .avc composition (no <Composition>)"));
    }
    let mut report = ImportReport::default();
    let info = child(root, "CompositionInfo", None);
    let name = info
        .and_then(|i| attr(i, "name"))
        .or_else(|| param(root, "Name"))
        .filter(|n| !n.is_empty())
        .unwrap_or(fallback_name);

    let layer_nodes: Vec<Node> = root.children().filter(|c| c.has_tag_name("Layer")).collect();
    let n_layers = num::<usize>(root, "numLayers")
        .unwrap_or(0)
        .max(
            layer_nodes
                .iter()
                .filter_map(|l| num::<usize>(*l, "layerIndex"))
                .map(|i| i + 1)
                .max()
                .unwrap_or(0),
        )
        .max(1);
    let mut comp = Composition::new(name, n_layers, 0);
    if let Some(i) = info
        && let (Some(w), Some(h)) = (num::<u32>(i, "width"), num::<u32>(i, "height"))
    {
        comp.set_resolution(w, h);
    }

    // Composition params.
    if let Some(v) = param_f64(root, "Speed") {
        comp.speed.set(v);
    }
    import_range(root, "Master", &mut comp.master, &mut report);
    if let Some(t) = child(root, "TempoController", None).and_then(|t| param_f64(t, "Tempo")) {
        comp.tempo_controller.tempo.set(t);
    }
    if let Some(m) = child(root, "CrossFader", None)
        .and_then(|c| child(c, "VideoTrack", None))
        .and_then(|t| mixer_mode(t, "Mixer"))
    {
        set_mode(&mut comp.crossfader.mixer, m, MIX_MODES, &mut report);
    }
    if let Some(track) = child(root, "VideoTrack", None) {
        let ids = &mut comp.ids;
        let v = comp.video.get_or_insert_with(|| lavmfx_core::VideoTrack::new(ids));
        import_chain(track, &mut v.effects, ids, &mut report);
    }
    import_dashboard(root, &mut comp.dashboard, &mut comp.ids, &mut report);

    // Layers.
    for ln in &layer_nodes {
        let Some(li) = num::<usize>(*ln, "layerIndex").filter(|&i| i < n_layers) else {
            continue;
        };
        let ids = &mut comp.ids;
        let layer = &mut comp.layers[li];
        if let Some(n) = param(*ln, "Name").filter(|n| !n.is_empty()) {
            layer.name.value = n.to_owned();
        }
        import_dashboard(*ln, &mut layer.dashboard, ids, &mut report);
        import_range(*ln, "Master", &mut layer.master, &mut report);
        // Layer autopilot: `ParamChoice name="AutoPilot"` with an internal enum
        // (default 4 = Off; 22 of the user's layers say 3): not mapped, kept Off.
        if param(*ln, "AutoPilot").is_some() {
            report.autopilot_layers += 1;
        }
        if let Some(d) = child(*ln, "ClipTransition", None).and_then(|t| param_f64(t, "Duration")) {
            layer.transition.duration.set(d);
        }
        if let Some(track) = child(*ln, "VideoTrack", None) {
            if let Some(vt) = layer.video.as_mut() {
                import_chain(track, &mut vt.track.effects, ids, &mut report);
            }
            if let Some(vt) = layer.video.as_mut() {
                import_range(track, "Opacity", &mut vt.track.opacity, &mut report);
            }
            if let (Some(i), Some(vt)) = (
                param(track, "Auto Size").and_then(|v| v.parse().ok()),
                layer.video.as_mut(),
            ) {
                vt.autosize.select(i);
            }
            if let Some(kind) = mixer_mode(track, "Blend") {
                match mode_name(kind, LAYER_BLEND_MODES) {
                    Some(m) => {
                        if let Some(b) = layer.blend_mode_mut(ids) {
                            b.select_value(m);
                        }
                    }
                    None => {
                        report.unknown_modes.insert(kind.to_owned());
                    }
                }
            }
            if let Some(kind) = mixer_mode(track, "Transition") {
                match mode_name(kind, MIX_MODES) {
                    Some(m) => {
                        layer.transition.blend_mode.select_value(m);
                    }
                    None => {
                        report.unknown_modes.insert(kind.to_owned());
                    }
                }
            }
        }
    }

    // Layer groups: <Group>s in order; layers point at them by `layerGroup` index.
    let group_ids: Vec<lavmfx_core::Id> = root
        .children()
        .filter(|c| c.has_tag_name("Group"))
        .enumerate()
        .map(|(i, gn)| {
            let name = param(gn, "Name")
                .filter(|n| !n.is_empty())
                .map_or_else(|| format!("Group {}", i + 1), str::to_owned);
            let mut g = lavmfx_core::LayerGroup::new(&mut comp.ids, &name);
            import_dashboard(gn, &mut g.dashboard, &mut comp.ids, &mut report);
            import_range(gn, "Master", &mut g.master, &mut report);
            if param_node(gn, "Speed").is_some() {
                import_range(gn, "Speed", g.speed_mut(&mut comp.ids), &mut report);
                report.group_speeds += 1;
            }
            if let Some(track) = child(gn, "VideoTrack", None) {
                let ids = &mut comp.ids;
                let v = g.video.get_or_insert_with(|| lavmfx_core::VideoTrack::new(ids));
                import_range(track, "Opacity", &mut v.opacity, &mut report);
                import_chain(track, &mut v.effects, ids, &mut report);
            }
            let id = g.id;
            comp.layergroups.push(g);
            id
        })
        .collect();
    for ln in &layer_nodes {
        let (Some(li), Some(gi)) = (num::<usize>(*ln, "layerIndex"), num::<usize>(*ln, "layerGroup")) else {
            continue;
        };
        if let (Some(l), Some(&id)) = (comp.layers.get_mut(li), group_ids.get(gi)) {
            l.group = Some(id);
        }
    }
    comp.prune_groups();
    report.groups = comp.layergroups.len();

    // Decks: names from CompositionInfo, content from <Deck>.
    let deck_infos: Vec<Node> = info
        .map(|i| i.children().filter(|c| c.has_tag_name("DeckInfo")).collect())
        .unwrap_or_default();
    let deck_nodes: Vec<Node> = root.children().filter(|c| c.has_tag_name("Deck")).collect();
    let n_decks = deck_infos.len().max(deck_nodes.len()).max(1);
    while comp.decks.len() < n_decks {
        let n = comp.decks.len() + 1;
        comp.add_deck(&format!("Deck {n}"));
    }
    for (i, di) in deck_infos.iter().enumerate() {
        if let Some(n) = attr(*di, "name").filter(|n| !n.is_empty()) {
            comp.decks[i].name.value = n.to_owned();
        }
    }
    for (pos, dn) in deck_nodes.iter().enumerate() {
        let di = num::<usize>(*dn, "deckIndex").unwrap_or(pos);
        if di >= comp.decks.len() {
            continue;
        }
        if let Some(n) = param(*dn, "Name").filter(|n| !n.is_empty()) {
            comp.decks[di].name.value = n.to_owned();
        }
        let clips: Vec<Node> = dn.children().filter(|c| c.has_tag_name("Clip")).collect();
        let n_cols = num::<usize>(*dn, "numColumns").unwrap_or(0).max(
            clips
                .iter()
                .filter_map(|c| num::<usize>(*c, "columnIndex"))
                .map(|c| c + 1)
                .max()
                .unwrap_or(0),
        );
        if n_cols == 0 {
            continue; // never filled: opens with default columns
        }
        let ids = &mut comp.ids;
        let mut content = DeckContent {
            columns: (0..n_cols)
                .map(|c| Column::new(ids, &format!("Column {}", c + 1)))
                .collect(),
            clips: (0..n_layers)
                .map(|_| (0..n_cols).map(|_| Clip::empty(ids)).collect())
                .collect(),
        };
        for cn in clips {
            let (Some(l), Some(c)) = (num::<usize>(cn, "layerIndex"), num::<usize>(cn, "columnIndex")) else {
                continue;
            };
            if l >= n_layers || c >= n_cols {
                continue;
            }
            if let Some(mut clip) = import_clip(cn, &mut comp.ids, resolver, &mut report) {
                import_dashboard(cn, &mut clip.dashboard, &mut comp.ids, &mut report);
                // Autopilot Loops (`AutoPilotLoopCount`, "Loops"). The clip's action is an
                // internal enum (base64): left at Layer Determined.
                if let Some(n) = param_f64(cn, "AutoPilotLoopCount") {
                    clip.autopilot_mut(&mut comp.ids).loops.set(n);
                    report.autopilot_loops += 1;
                }
                if let (Some(track), Some(v)) = (child(cn, "VideoTrack", None), clip.video.as_mut()) {
                    import_chain(track, &mut v.track.effects, &mut comp.ids, &mut report);
                }
                // Clip Blend Mode: a `Blend` mixer with a mode = overrides the layer's.
                if let Some(kind) = child(cn, "VideoTrack", None).and_then(|t| mixer_mode(t, "Blend")) {
                    match mode_name(kind, LAYER_BLEND_MODES) {
                        Some(m) => {
                            if let Some(b) = clip.blend_mode_mut(&mut comp.ids) {
                                b.select_value(m);
                                report.clip_blends += 1;
                            }
                        }
                        None => {
                            report.unknown_modes.insert(kind.to_owned());
                        }
                    }
                }
                // Beat Snap: an index into the option list (like TransportType), 0 =
                // Composition Determined.
                if let Some(i) = param(cn, "Beat Snap").and_then(|v| v.parse::<usize>().ok()) {
                    clip.beatsnap.select(i);
                }
                content.clips[l][c] = clip;
            }
        }
        comp.decks[di].stash = Some(content);
    }
    // Deck 0 was active with no columns: swap its content in, then go to the saved deck.
    let first = comp.decks[0].stash.take().unwrap_or_default();
    comp.columns = first.columns;
    for (l, clips) in comp.layers.iter_mut().zip(first.clips) {
        l.clips = clips;
    }
    if comp.columns.is_empty() {
        for _ in 0..lavmfx_core::deck::DEFAULT_COLUMNS {
            comp.add_column();
        }
    }
    let current = num::<usize>(root, "currentDeckIndex")
        .unwrap_or(0)
        .min(comp.decks.len() - 1);
    comp.select_deck(current);
    comp.align_options();
    report.drives = resolver.guessed_drives();
    Ok(Imported {
        composition: comp,
        report,
    })
}

fn set_mode(
    mixer: &mut lavmfx_core::ParameterCollection,
    kind: &str,
    options: &[&'static str],
    report: &mut ImportReport,
) {
    match mode_name(kind, options) {
        Some(m) => {
            if let Some(lavmfx_core::Param::Choice(c)) = mixer.get_mut(lavmfx_core::mix::BLEND_MODE_KEY) {
                c.select_value(m);
            }
        }
        None => {
            report.unknown_modes.insert(kind.to_owned());
        }
    }
}

/// One `<Clip>`: a video clip, or `None` (counted as skipped) when it has no file.
fn import_clip(
    cn: Node,
    ids: &mut lavmfx_core::IdGen,
    resolver: &mut PathResolver,
    report: &mut ImportReport,
) -> Option<Clip> {
    let saved = cn
        .descendants()
        .find(|e| e.has_tag_name("VideoFile"))
        .and_then(|e| e.attribute("value"))
        .or_else(|| {
            cn.descendants()
                .find(|e| e.has_tag_name("VideoFormatReaderSource"))
                .and_then(|e| e.attribute("fileName"))
        })
        .filter(|p| !p.is_empty());
    let Some(saved) = saved else {
        if let Some(mut clip) = import_source_clip(cn, ids) {
            import_transport(cn, &mut clip, ids, report);
            report.sources += 1;
            return Some(clip);
        }
        if let Some(sp) = cn.descendants().find(|e| e.has_tag_name("SpoutInfo")) {
            report.spout += 1;
            if let Some(n) = sp.attribute("serverName").filter(|n| !n.is_empty()) {
                report.spout_senders.insert(n.to_owned());
                // A shared-video clip (Spout / Syphon / PipeWire by that name).
                let mut clip = Clip::empty(ids);
                let label = param(cn, "Name").filter(|l| !l.is_empty()).unwrap_or(n).to_owned();
                clip.load_share(ids, n, &label);
                import_transport(cn, &mut clip, ids, report);
                return Some(clip);
            }
        }
        // The file has a <Clip> for every slot; only ones with a source count.
        if cn
            .descendants()
            .any(|e| e.has_tag_name("VideoSource") || e.has_tag_name("AudioFile"))
        {
            report.skipped += 1;
        }
        return None;
    };
    let r = resolver.resolve(saved);
    if !r.exists {
        report.missing.push(saved.to_owned());
    }
    let duration_ms = cn
        .descendants()
        .find(|e| e.has_tag_name("PhaseSourceTransportTimeline"))
        .and_then(|e| num::<f64>(e, "defaultMillisecondsDuration"))
        .unwrap_or(0.0);
    let src = cn.descendants().find(|e| e.has_tag_name("VideoSource"));
    let (w, h) = src.map_or((0, 0), |s| {
        (num(s, "width").unwrap_or(0), num(s, "height").unwrap_or(0))
    });
    let name = param(cn, "Name")
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            Path::new(&saved.replace('\\', "/"))
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    let info = VideoFileInfo {
        path: r.path.to_string_lossy().into_owned(),
        exists: r.exists,
        duration: format!("{:.3}s", duration_ms / 1000.0),
        duration_ms,
        framerate: FrameRate { num: 0, denom: 1 }, // unknown until played
        width: w,
        height: h,
    };
    let mut clip = Clip::empty(ids);
    clip.load_video(ids, &name, info);
    import_transport(cn, &mut clip, ids, report);
    report.clips += 1;
    Some(clip)
}

/// Clip transport (`Transport › Position`): `DurationSource` (`defaultDuration`
/// "3.5s" / "8b", `userDuration` = the length the user set: Timeline seconds at
/// speed 1, or BPM Sync beats), and in `PhaseSourceTransportTimeline › Params`:
/// Speed (+ animation), `PlayDirection` 0 < / 1 || / 2 >, `PlayMode` (our order),
/// `PlayModeAway` 0 Restart / 1 Continue / 2 Relative. `TransportType` 1 = BPM
/// Sync; its beats without `userDuration` = `Beats_double numManualBeats`.
/// "Max Distance" / "Interval" (random mode) have no counterpart.
fn import_transport(cn: Node, clip: &mut Clip, ids: &mut lavmfx_core::IdGen, report: &mut ImportReport) {
    use lavmfx_core::Transport;
    let pos = child(cn, "Transport", None).and_then(|t| param_node(t, "Position"));
    let ds = pos.and_then(|p| child(p, "DurationSource", None));
    let ptt = pos.and_then(|p| child(p, "PhaseSourceTransportTimeline", None));
    let user = ds.and_then(|d| num::<f64>(d, "userDuration")).filter(|u| *u > 0.0);
    let in_beats = ds
        .and_then(|d| d.attribute("defaultDuration"))
        .is_some_and(|d| d.ends_with('b'));
    let mut changed = false;
    if param(cn, "TransportType") == Some("1") {
        let beats = user.filter(|_| in_beats).or_else(|| {
            ptt.and_then(|t| child(t, "Beats_double", None))
                .and_then(|b| num::<f64>(b, "numManualBeats"))
                .filter(|b| *b > 0.0)
        });
        if clip.set_transport_type(ids, 1, 120.0)
            && let (Some(b), Some(Transport::BpmSync(t))) = (beats, clip.transport.as_mut())
        {
            t.controls.duration.set(b);
        }
        report.bpm_sync += 1;
        changed = true;
    } else if let (Some(u), Some(Transport::Timeline(t))) = (user.filter(|_| !in_beats), clip.transport.as_mut()) {
        t.controls.duration.set(u);
        changed = true;
    }
    let Some(ptt) = ptt else { return };
    let Some(t) = clip.transport.as_mut() else { return };
    let (dir, mode, away, speed) = match t {
        Transport::Timeline(t) => {
            let c = &mut t.controls;
            (&mut c.playdirection, &mut c.playmode, &mut c.playmodeaway, &mut c.speed)
        }
        Transport::BpmSync(t) => {
            let c = &mut t.controls;
            (&mut c.playdirection, &mut c.playmode, &mut c.playmodeaway, &mut c.speed)
        }
    };
    for (name, choice) in [("PlayDirection", dir), ("PlayMode", mode), ("PlayModeAway", away)] {
        if let Some(i) = param(ptt, name).and_then(|v| v.parse::<usize>().ok()) {
            changed |= choice.select(i);
        }
    }
    if param_node(ptt, "Speed").is_some() {
        import_range(ptt, "Speed", speed, report);
        changed = true;
    }
    if changed {
        report.transports += 1;
    }
}

/// A generator clip (`GeneratorVideoSource` + `RenderPass baseType="Generator"`, its
/// `uniqueTypeId` = the source id): the source with its saved numbers and colours
/// (choices are stored as hashes: they keep their defaults).
fn import_source_clip(cn: Node, ids: &mut lavmfx_core::IdGen) -> Option<Clip> {
    // Video Router / Feedback (their input choice isn't in the XML: the
    // composition).
    let routed = cn.descendants().find_map(|e| {
        let t = e.attribute("type").filter(|_| e.has_tag_name("VideoSource"))?;
        if t.contains("Router") {
            Some(lavmfx_core::sources::ROUTER)
        } else if t.contains("Feedback") {
            Some(lavmfx_core::sources::FEEDBACK)
        } else {
            None
        }
    });
    if let Some(name) = routed {
        let mut clip = Clip::empty(ids);
        if !clip.load_source(ids, name) {
            return None;
        }
        if let Some(n) = param(cn, "Name").filter(|n| !n.is_empty()) {
            clip.name.value = n.to_owned();
        }
        return Some(clip);
    }
    let src = cn
        .descendants()
        .find(|e| e.has_tag_name("VideoSource") && e.attribute("type") == Some("GeneratorVideoSource"))?;
    let pass = src
        .descendants()
        .find(|e| e.has_tag_name("RenderPass") && e.attribute("baseType") == Some("Generator"))?;
    let spec = pass
        .attribute("uniqueTypeId")
        .and_then(lavmfx_core::sources::spec)
        .or_else(|| pass.attribute("type").and_then(lavmfx_core::sources::spec))?;
    let mut clip = Clip::empty(ids);
    if !clip.load_source(ids, &spec.name) {
        return None;
    }
    if let Some(name) = param(cn, "Name").filter(|n| !n.is_empty()) {
        clip.name.value = name.to_owned();
    }
    let params = clip.video.as_mut().and_then(|v| v.sourceparams.as_mut())?;
    if let Some(ps) = child(pass, "Params", None) {
        apply_params(ps, params);
    }
    Some(clip)
}

/// Copy saved `<Params>` values onto params of the same name. Numbers,
/// colours (decimal ABGR), text and bools always; choices only when saved as
/// an index (`T="INT32"`, effects) — sources save a hash (`UINT64`): kept.
/// Returns (dashboard links, other animations) imported.
fn apply_params(ps: Node, params: &mut lavmfx_core::ParameterCollection) -> (usize, usize) {
    use lavmfx_core::Param;
    let (mut links, mut anims) = (0, 0);
    for p in ps.children().filter(|c| c.is_element()) {
        let Some(name) = p.attribute("name") else { continue };
        match (params.get_mut(name), p.tag_name().name()) {
            (Some(Param::Range(r)), "ParamRange") => {
                if let Some(v) = num::<f64>(p, "value") {
                    r.set(v);
                }
                match import_drive(p, r) {
                    Some(lavmfx_core::drive::Source::Dashboard) => links += 1,
                    Some(_) => anims += 1,
                    None => {}
                }
            }
            (Some(Param::Color(c)), "ParamColor") => {
                // A decimal u32, bytes A B G R from the top (Example.avc: 4288688383 with
                // its channel params Green 0.188, Blue 0.627 = #ff30a0).
                if let Some(abgr) = num::<u32>(p, "value") {
                    let [a, b, g, r] = abgr.to_be_bytes();
                    c.value = format!("#{r:02x}{g:02x}{b:02x}{a:02x}");
                }
            }
            (Some(Param::Choice(c)), "ParamChoice") if p.attribute("T") == Some("INT32") => {
                if let Some(i) = num::<usize>(p, "value") {
                    c.select(i);
                }
            }
            (Some(Param::Text(t)), _) => {
                if let Some(v) = p.attribute("value") {
                    t.value = v.to_owned();
                }
            }
            (Some(Param::String(t)), _) => {
                if let Some(v) = p.attribute("value") {
                    t.value = v.to_owned();
                }
            }
            (Some(Param::Bool(b)), _) => {
                if let Some(v) = p.attribute("value") {
                    b.value = v == "1" || v == "true";
                }
            }
            _ => {}
        }
    }
    (links, anims)
}

/// An animation under a `ParamRange` → our [`Drive`]; returns its source.
/// * `PhaseSourceDashboardLink linkId="/link7"`: follows knob 7 of the owner's dashboard.
/// * `PhaseSourceTimeline`: one pass = `DurationSource` duration (`defaultDuration`
///   "5s" / "4b", `userDuration` overrides the number; missing = 5 s) ÷ (`scale` ×
///   the timeline's `Speed`) (5 s ÷ 0.5476 = 9.13 s). Beats → BPM Sync. `PlayDirection` 0 back /
///   1 pause / 2 forward, `PlayMode` 0 loop / 1 bounce / 2 random / 3+ once.
/// * `PhaseSourceFFT linkId="/audioengine/externalfft"`: External FFT; band =
///   `FrequencyRange` startStop (0..1), `Gain` dB → ×, `Fallback` ms → fall.
///
/// Plus `ValueRange startStop` (param units) = from / to and a
/// `BehaviourDouble › ModifierEnvelope` = envelope.
fn import_drive(p: Node, r: &mut lavmfx_core::RangeParam) -> Option<lavmfx_core::drive::Source> {
    use lavmfx_core::drive::{Direction, Drive, LINKS, PlayMode, Source};
    let mut d = if let Some(l) = child(p, "PhaseSourceDashboardLink", None) {
        let n = l
            .attribute("linkId")
            .and_then(|id| id.trim_start_matches('/').strip_prefix("link"))
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| (1..=LINKS).contains(n))?;
        let mut d = Drive::new(r, Source::Dashboard);
        d.link = n - 1;
        d
    } else if let Some(t) = child(p, "PhaseSourceTimeline", None) {
        let ds = child(p, "DurationSource", None);
        let def = ds.and_then(|d| d.attribute("defaultDuration")).unwrap_or("5s");
        let beats = def.ends_with('b') || t.attribute("defaultBeatsDuration").is_some();
        let mut base: f64 = def
            .trim_end_matches(['b', 's'])
            .parse()
            .unwrap_or(if beats { 4.0 } else { 5.0 });
        if let Some(u) = ds
            .and_then(|d| num::<f64>(d, "userDuration"))
            .or_else(|| num(t, "defaultBeatsDuration"))
        {
            base = u;
        }
        let scale = ds.and_then(|d| num::<f64>(d, "scale")).unwrap_or(1.0);
        let speed = param_f64(t, "Speed").unwrap_or(1.0);
        let len = base / (scale * speed).max(1e-6);
        let mut d = Drive::new(r, if beats { Source::BpmSync } else { Source::Timeline });
        if beats {
            d.beats = len.max(1.0 / 64.0);
        } else {
            d.seconds = len.max(0.01);
        }
        d.direction = match param(t, "PlayDirection") {
            Some("0") => Direction::Backward,
            Some("1") => Direction::Pause,
            _ => Direction::Forward,
        };
        d.mode = match param(t, "PlayMode") {
            Some("1") => PlayMode::Bounce,
            Some("2") => PlayMode::Random,
            Some("3" | "4") => PlayMode::Once,
            _ => PlayMode::Loop,
        };
        d
    } else {
        let f = child(p, "PhaseSourceFFT", None)?;
        let src = match f.attribute("linkId") {
            Some(l) if l.contains("layer") => Source::LayerFft,
            Some(l) if l.contains("composition") => Source::CompositionFft,
            _ => Source::ExternalFft,
        };
        let mut d = Drive::new(r, src);
        let ps = child(f, "Params", None);
        if let Some(vr) = ps
            .and_then(|ps| ps.children().find(|c| c.attribute("name") == Some("FrequencyRange")))
            .and_then(|fr| child(fr, "ValueRange", None))
            && let (Some(a), Some(b)) = (num::<f64>(vr, "min"), num::<f64>(vr, "max"))
        {
            d.fft.lo = a.clamp(0.0, 1.0);
            d.fft.hi = b.clamp(d.fft.lo, 1.0);
        }
        if let Some(db) = param_f64(f, "Gain") {
            d.fft.gain = 10f64.powf(db / 20.0).clamp(0.0, 4.0);
        }
        if let Some(ms) = param_f64(f, "Fallback") {
            // Our fall: 1 = 12 s, quadratic (see `FftBand::fall`).
            d.fft.fall = (ms / 12_000.0).max(0.0).sqrt().min(1.0);
        }
        d
    };
    if let Some(vr) = p
        .children()
        .find(|c| c.has_tag_name("ValueRange") && c.attribute("name") == Some("startStop"))
        && let (Some(a), Some(b)) = (num::<f64>(vr, "min"), num::<f64>(vr, "max"))
    {
        d.from = a.clamp(r.min, r.max);
        d.to = b.clamp(r.min, r.max);
    }
    if let Some(env) =
        child(p, "BehaviourDouble", None).and_then(|b| b.descendants().find(|c| c.has_tag_name("points")))
    {
        d.envelope = envelope(env);
    }
    d.restart();
    if let Some(ph) = p
        .children()
        .find(|c| c.tag_name().name().starts_with("PhaseSource"))
        .and_then(|c| num::<f64>(c, "phase"))
    {
        d.phase = ph.clamp(0.0, 1.0);
    }
    let source = d.source;
    r.drive = Some(Box::new(d));
    Some(source)
}

/// Envelope `<points><point x y curve/>`: a point's `curve` shapes the
/// segment arriving at it (ours: the segment leaving a point). Codes:
/// 1 linear ("Saw"), 33 step ("Digital"), 4 eased
/// ("EasyEase"); 14 ("Jaws") → ease in, 3 → ease out, others smooth (guesses).
fn envelope(points: Node) -> Option<lavmfx_core::drive::Envelope> {
    use lavmfx_core::drive::{Curve, EnvPoint, Envelope};
    let raw: Vec<(f64, f64, u32)> = points
        .children()
        .filter(|c| c.has_tag_name("point"))
        .filter_map(|c| Some((num(c, "x")?, num(c, "y")?, num(c, "curve").unwrap_or(1))))
        .collect();
    if raw.len() < 2 {
        return None;
    }
    let curve = |code: u32| match code {
        1 => Curve::Linear,
        33 => Curve::Hold,
        14 => Curve::EaseIn,
        3 => Curve::EaseOut,
        _ => Curve::Smooth,
    };
    let mut pts: Vec<EnvPoint> = raw
        .iter()
        .enumerate()
        .map(|(i, &(x, y, _))| EnvPoint {
            x: x.clamp(0.0, 1.0),
            y: y.clamp(0.0, 1.0),
            curve: raw.get(i + 1).map_or(Curve::Linear, |n| curve(n.2)),
        })
        .collect();
    pts.sort_by(|a, b| a.x.total_cmp(&b.x));
    pts[0].x = 0.0;
    if let Some(l) = pts.last_mut() {
        l.x = 1.0;
    }
    Some(Envelope { points: pts })
}

/// Dashboard knobs saved on an owner (`Params/ParamRange name="Link n"
/// altName="<knob name>" value=…`): values (0..1) and names.
fn import_dashboard(
    owner: Node,
    dash: &mut lavmfx_core::ParameterCollection,
    ids: &mut lavmfx_core::IdGen,
    report: &mut ImportReport,
) {
    use lavmfx_core::Param;
    use lavmfx_core::drive::{LINKS, ensure_links};
    let knobs: Vec<Node> = owner
        .children()
        .filter(|c| c.has_tag_name("Params"))
        .flat_map(|ps| ps.children())
        .filter(|p| {
            p.has_tag_name("ParamRange")
                && p.attribute("name")
                    .and_then(|n| n.strip_prefix("Link "))
                    .and_then(|n| n.parse::<usize>().ok())
                    .is_some_and(|n| (1..=LINKS).contains(&n))
        })
        .collect();
    if knobs.is_empty() {
        return;
    }
    ensure_links(dash, ids);
    for k in knobs {
        let Some(Param::Range(r)) = k.attribute("name").and_then(|n| dash.get_mut(n)) else {
            continue;
        };
        if let Some(v) = num::<f64>(k, "value") {
            r.set(v);
        }
        if let Some(n) = k.attribute("altName").filter(|n| !n.is_empty()) {
            r.view.get_or_insert_with(Default::default).alternative_name = Some(n.to_owned());
        }
        report.dashboard_knobs += 1;
    }
}

/// The effect chain of a `<VideoTrack>` (its `RenderPass type="RenderPassChain"`),
/// appended to `chain` in saved order. Each entry is a `DryWetEffect` (Bypassed,
/// Opacity, `ChoosableMixer name="Mixer"` = blend) wrapping the effect
/// (`baseType="Effect"`, `uniqueTypeId` = effect id). A `TransformEffect` entry
/// with saved values goes into the chain's Transform (created first if missing:
/// groups and the composition only get one when it was changed).
fn import_chain(
    track: Node,
    chain: &mut Vec<lavmfx_core::VideoEffect>,
    ids: &mut lavmfx_core::IdGen,
    report: &mut ImportReport,
) {
    use lavmfx_core::{Param, effects};
    let Some(pc) = track
        .children()
        .find(|c| c.has_tag_name("RenderPass") && c.attribute("type") == Some("RenderPassChain"))
    else {
        return;
    };
    let mut entries: Vec<Node> = pc.children().filter(|c| c.has_tag_name("RenderPass")).collect();
    entries.sort_by_key(|e| num::<i64>(*e, "index").unwrap_or(i64::MAX));
    for e in entries {
        if e.attribute("type") == Some("TransformEffect") {
            if let Some(ps) = child(e, "Params", None) {
                effects::ensure_transform(chain, ids);
                if let Some(t) = chain.iter_mut().find(|f| f.is_transform()) {
                    report.count_drives(apply_params(ps, &mut t.params));
                }
            }
            continue;
        }
        // The effect itself: inside a DryWetEffect wrapper, or the entry.
        let (wrap, fx) = if e.attribute("baseType") == Some("DryWetEffect") {
            let inner = e
                .children()
                .find(|c| c.has_tag_name("RenderPass") && c.attribute("baseType") == Some("Effect"));
            (Some(e), inner.unwrap_or(e))
        } else if e.attribute("baseType") == Some("Effect") {
            (None, e)
        } else {
            continue;
        };
        let spec = fx
            .attribute("uniqueTypeId")
            .and_then(effects::spec_by_id)
            .or_else(|| fx.attribute("name").and_then(effects::spec));
        let Some(spec) = spec else {
            report
                .unknown_effects
                .insert(fx.attribute("name").unwrap_or("?").to_owned());
            continue;
        };
        let mut v = spec.instantiate(ids);
        if let Some(ps) = child(fx, "Params", None) {
            report.count_drives(apply_params(ps, &mut v.params));
        }
        // The wrapper's Opacity can be linked too.
        if let Some(w) = wrap
            && let Some(ps) = child(w, "Params", None)
            && let Some(op) = ps.children().find(|c| c.attribute("name") == Some("Opacity"))
            && let Some(Param::Range(r)) = v.params.get_mut(effects::OPACITY_KEY)
            && let Some(src) = import_drive(op, r)
        {
            report.count_drives(if src == lavmfx_core::drive::Source::Dashboard {
                (1, 0)
            } else {
                (0, 1)
            });
        }
        if let Some(w) = wrap {
            if let (Some(b), Some(x)) = (v.bypassed.as_mut(), param(w, "Bypassed")) {
                b.value = x == "1";
            }
            if let (Some(Param::Range(r)), Some(o)) = (v.params.get_mut(effects::OPACITY_KEY), param_f64(w, "Opacity"))
            {
                r.set(o);
            }
            if let Some(kind) = mixer_mode(w, "Mixer") {
                match mode_name(kind, MIX_MODES) {
                    Some(m) => {
                        if let Some(Param::Choice(c)) = v.mixer.get_mut(effects::BLEND_KEY) {
                            c.select_value(m);
                        }
                    }
                    None => {
                        report.unknown_modes.insert(kind.to_owned());
                    }
                }
            }
        }
        chain.push(v);
        report.effects += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const AVC: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Composition name="Composition" numDecks="2" currentDeckIndex="1" numLayers="2" numColumns="3">
  <CompositionInfo name="My Show" width="1920" height="1080">
    <DeckInfo name="Main" id="1" closed="0"/>
    <DeckInfo name="Extra" id="2" closed="0"/>
  </CompositionInfo>
  <Params name="Params"><ParamRange name="Speed" T="DOUBLE" default="1" value="0.5"/></Params>
  <Group name="Group" uniqueId="9"><Params name="Params"><Param name="Name" T="STRING" value="Lines"/>
    <ParamRange name="Speed" T="DOUBLE" default="1" value="0.25"><PhaseSourceStatic name="PhaseSourceStatic" phase="0.1"/></ParamRange></Params></Group>
  <Layer layerIndex="0"/>
  <Layer layerIndex="1" layerGroup="0">
    <ClipTransition name="ClipTransition"><Params name="Params"><ParamRange name="Duration" value="2.5"/></Params></ClipTransition>
    <VideoTrack name="VideoTrack">
      <Params name="Params"><ParamRange name="Opacity" value="0.25"><DurationSource name="DurationSource"/>
        <PhaseSourceFFT name="PhaseSourceFFT" phase="0" linkId="/audioengine/externalfft"><Params name="Params">
          <ParamRange name="FrequencyRange" T="DOUBLE" default="0.5" value="0.3"><ValueRange name="startStop" min="0.1" max="0.3"/></ParamRange>
          <ParamRange name="Gain" T="DOUBLE" default="0" value="-6.0206"/>
          <ParamRange name="Fallback" T="DOUBLE" default="1000" value="3000"/></Params></PhaseSourceFFT></ParamRange></Params>
      <ChoosableMixer name="Blend"><Params name="Params"><ParamChoice name="Blend Mode" T="UINT64" value="67464319"/></Params><RenderPass name="Add50" type="Add50"/></ChoosableMixer>
      <ChoosableMixer name="Transition"><RenderPass name="Wipe" type="WipeEllipse"/></ChoosableMixer>
    </VideoTrack>
  </Layer>
  <TempoController name="TempoController"><Params name="Params"><ParamRange name="Tempo" value="128"/></Params></TempoController>
  <Deck name="Deck" deckIndex="0" numColumns="3" numLayers="2">
    <Clip name="Clip" layerIndex="0" columnIndex="2">
      <PreloadData><VideoFile value="D:\VJ\one.mov"/></PreloadData>
      <Params name="Params"><Param name="Name" T="STRING" value="One"/><ParamChoice name="TransportType" value="1"/>
        <ParamRange name="AutoPilotLoopCount" altName="Loops" T="DOUBLE" default="1" value="2"/></Params>
      <Transport name="Transport"><Params name="Params"><ParamRange name="Position" value="0">
        <DurationSource name="DurationSource" defaultDuration="8b" userDuration="4"/>
        <PhaseSourceTransportTimeline name="P" defaultMillisecondsDuration="4000" defaultBeatsDuration="8">
          <Beats_double name="Beats_double" numManualBeats="8"/></PhaseSourceTransportTimeline></ParamRange></Params></Transport>
      <VideoTrack name="VideoTrack"><PrimarySource><VideoSource name="VideoSource" width="1280" height="720" type="VideoFormatReaderSource"/></PrimarySource></VideoTrack>
    </Clip>
    <Clip name="Clip" layerIndex="0" columnIndex="0">
      <VideoTrack name="VideoTrack"><PrimarySource><VideoSource type="SpoutVideoSource"><SpoutInfo serverName="TDAutoKage"/></VideoSource></PrimarySource></VideoTrack>
    </Clip>
    <Clip name="Clip" layerIndex="1" columnIndex="0">
      <VideoTrack name="VideoTrack"><PrimarySource><VideoSource type="GeneratorVideoSource"><RenderPass name="Metaballs" type="Metaballs" uniqueTypeId="844752396506092017" baseType="Generator"><Params name="Params"><ParamRange name="Distance" T="DOUBLE" default="0.5" value="0.25"/><ParamColor name="Color" T="COLOR" value="4288688383"/></Params></RenderPass></VideoSource></PrimarySource></VideoTrack>
    </Clip>
  </Deck>
  <Deck name="Deck" deckIndex="1" numColumns="2" numLayers="2">
    <Clip name="Clip" layerIndex="1" columnIndex="1"><PreloadData><VideoFile value="D:\VJ\gone.mov"/></PreloadData>
      <Params name="Params"><ParamChoice name="TransportType" default="0" value="0"/><ParamChoice name="Beat Snap" default="0" value="2"/></Params>
      <Transport name="Transport"><Params name="Params"><ParamRange name="Position" value="0">
        <DurationSource name="DurationSource" defaultDuration="3.5s" userDuration="1.75"/>
        <PhaseSourceTransportTimeline name="PhaseSourceTransportTimeline" defaultMillisecondsDuration="3500"><Params name="Params">
          <ParamRange name="Speed" T="DOUBLE" default="1" value="0.5"/>
          <ParamChoice name="PlayDirection" T="INT32" default="2" value="0"/>
          <ParamChoice name="PlayMode" T="INT32" default="0" value="1"/>
          <ParamChoice name="PlayModeAway" T="INT32" default="0" value="1"/></Params></PhaseSourceTransportTimeline></ParamRange></Params></Transport>
      <VideoTrack name="VideoTrack"><ChoosableMixer name="Blend"><RenderPass name="Mask50" type="Mask50"/></ChoosableMixer></VideoTrack></Clip>
  </Deck>
</Composition>"#;

    #[test]
    fn imports_structure_params_and_paths() {
        let t = std::env::temp_dir().join(format!("lavmfx-avc-{}", std::process::id()));
        std::fs::create_dir_all(t.join("disk/VJ")).unwrap();
        std::fs::write(t.join("disk/VJ/one.mov"), b"x").unwrap();
        let mut r = PathResolver::with(None, HashMap::new(), vec![t.join("disk")]);
        let Imported {
            composition: mut c,
            report,
        } = from_str(AVC, &mut r, "fallback").unwrap();

        assert_eq!(c.name.value, "My Show");
        assert_eq!(c.layergroups.len(), 1);
        assert_eq!(c.layergroups[0].name.value, "Lines");
        assert_eq!(c.group_members(0), [1]);
        assert_eq!(c.speed.value, 0.5);
        assert_eq!(c.tempo_controller.tempo.value, 128.0);
        assert_eq!(
            c.decks.iter().map(|d| d.name.value.as_str()).collect::<Vec<_>>(),
            ["Main", "Extra"]
        );
        assert_eq!(c.active_deck(), 1);
        assert_eq!(c.columns.len(), 2);
        // Layer 2 settings.
        let l = &c.layers[1];
        assert_eq!(l.transition.duration.value, 2.5);
        assert_eq!(l.video.as_ref().unwrap().track.opacity.value, 0.25);
        // FFT on the layer opacity: External FFT, band 0.1..0.3, −6 dB = ×0.5, 3 s fall = 0.5.
        let d = l.video.as_ref().unwrap().track.opacity.drive.as_ref().unwrap();
        assert_eq!(d.source, lavmfx_core::drive::Source::ExternalFft);
        assert_eq!((d.fft.lo, d.fft.hi), (0.1, 0.3));
        assert!(
            (d.fft.gain - 0.5).abs() < 1e-3 && (d.fft.fall - 0.5).abs() < 1e-9,
            "{:?}",
            d.fft
        );
        assert_eq!(l.transition.blend_mode.value, "Wipe Ellipse");
        let ids = &mut c.ids;
        assert_eq!(c.layers[1].blend_mode_mut(ids).unwrap().value, "50 Add");
        // Missing clip in the active deck keeps its saved path.
        let gone = &c.layers[1].clips[1];
        let fi = gone.video.as_ref().unwrap().fileinfo.as_ref().unwrap();
        assert!(!fi.exists);
        assert_eq!(fi.path, r"D:\VJ\gone.mov");
        // Its transport: Duration 1.75 s (of 3.5) at speed 0.5 = ×1, backward, bounce,
        // Continue; own blend mode 50 Mask; Beat Snap index 2.
        let Some(lavmfx_core::Transport::Timeline(gt)) = &gone.transport else {
            panic!()
        };
        let tc = &gt.controls;
        assert_eq!((tc.duration.value, tc.speed.value), (1.75, 0.5));
        assert_eq!(
            (tc.playdirection.value.as_str(), tc.playmode.value.as_str()),
            ("<", "Bounce")
        );
        assert_eq!(tc.playmodeaway.value, "Continue");
        assert!((lavmfx_core::transport::TransportSettings::for_clip(gone, 120.0).speed - 1.0).abs() < 1e-9);
        assert_eq!(gone.blend_override(), lavmfx_core::mix::shader_blend("50 Mask"));
        assert!(matches!(gone.video.as_ref().unwrap().track.mixer.get("Blend Mode"),
            Some(lavmfx_core::Param::Choice(b)) if b.value == "50 Mask"));
        assert_eq!(gone.beatsnap.value, lavmfx_core::defaults::CLIP_BEAT_SNAP[2]);
        // Group speed 0.25 for its layer (1); layer 0 has none.
        assert_eq!((c.layer_speed(1), c.layer_speed(0)), (0.25, 1.0));
        assert_eq!(report.group_speeds, 1);
        // Deck "Main": found clip with name, length and size.
        c.select_deck(0);
        let one = &c.layers[0].clips[2];
        assert_eq!(one.name.value, "One");
        // BPM Sync: the user's 4 beats win over the file's 8.
        assert!(matches!(&one.transport, Some(lavmfx_core::Transport::BpmSync(t)) if t.controls.duration.value == 4.0));
        assert_eq!(
            c.layers[0].clips[0].source(),
            Some(&lavmfx_core::sources::ClipSource::Share("TDAutoKage".into())),
            "Spout clip = shared-video clip"
        );
        assert_eq!(
            (report.spout, report.spout_senders.iter().next().map(String::as_str)),
            (1, Some("TDAutoKage"))
        );
        assert_eq!((report.transports, report.clip_blends), (2, 1));
        assert_eq!(one.autopilot_loops(), 2.0);
        assert_eq!(report.autopilot_loops, 1);
        let fi = one.video.as_ref().unwrap().fileinfo.as_ref().unwrap();
        assert!(fi.exists);
        assert_eq!(fi.path, t.join("disk/VJ/one.mov").to_string_lossy());
        assert_eq!((fi.duration_ms, fi.width, fi.height), (4000.0, 1280, 720));
        // Generator clip: Metaballs (by uniqueTypeId) with its saved Distance and Color.
        let generator = &c.layers[1].clips[0];
        assert_eq!(
            generator.source(),
            Some(&lavmfx_core::sources::ClipSource::Generator("Metaballs".into()))
        );
        let sp = generator.video.as_ref().unwrap().sourceparams.as_ref().unwrap();
        assert!(matches!(sp.get("Distance"), Some(lavmfx_core::Param::Range(r)) if r.value == 0.25));
        // 4288688383 = 0xFFA030FF (ABGR) → #ff30a0ff
        assert!(matches!(sp.get("Color"), Some(lavmfx_core::Param::Color(col)) if col.value == "#ff30a0ff"));

        assert_eq!(
            (
                report.clips,
                report.missing.len(),
                report.skipped,
                report.bpm_sync,
                report.sources
            ),
            (2, 1, 0, 1, 1) // the Spout clip is imported now (shared video), not skipped
        );
        assert!(report.unknown_modes.is_empty(), "{:?}", report.unknown_modes);
        assert_eq!(report.drives, vec![('D', t.join("disk"))]);
        let _ = std::fs::remove_dir_all(&t);
    }

    /// Effect chains as version 7.6 saves them (RenderPassChain › DryWetEffect › effect).
    const AVC_FX: &str = r#"<Composition name="Composition" numLayers="1" numColumns="1">
  <VideoTrack name="VideoTrack"><RenderPass name="RenderPassChain" type="RenderPassChain" baseType="RenderPassChain">
    <RenderPass storage="0" name="Transform" type="TransformEffect" uniqueTypeId="17122039101699797593" baseType="Effect" index="0">
      <Params name="Params"><ParamRange name="Rotation Z" T="DOUBLE" default="0" value="90"/></Params></RenderPass>
    <RenderPass storage="0" name="InvertRGB" type="DryWetEffect" uniqueTypeId="A105" baseType="DryWetEffect" index="1">
      <RenderPass name="InvertRGB" type="InvertRGB" uniqueTypeId="A105" baseType="Effect" dwType="Effect"/>
      <ChoosableMixer name="Mixer"><RenderPass name="Alpha" type="Alpha" baseType="Mixer"/></ChoosableMixer>
    </RenderPass>
  </RenderPass></VideoTrack>
  <Layer layerIndex="0"><Params name="Params"><ParamRange name="Link 2" altName="Soft" T="DOUBLE" default="0" value="0.5"/></Params>
  <VideoTrack name="VideoTrack"><RenderPass name="RenderPassChain" type="RenderPassChain">
    <RenderPass storage="0" name="Blur" type="DryWetEffect" uniqueTypeId="A140" baseType="DryWetEffect" index="1">
      <RenderPass name="Blur" type="Blur" uniqueTypeId="A140" baseType="Effect"><Params name="Params">
        <ParamRange name="Blur" T="DOUBLE" default="1" value="0.25"><DurationSource name="DurationSource"/>
          <PhaseSourceDashboardLink name="PhaseSourceDashboardLink" phase="0" linkId="/link2" linkName="Soft"/>
          <ValueRange name="startStop" min="0.2" max="0.8"/></ParamRange></Params></RenderPass>
    </RenderPass>
    <RenderPass storage="0" name="Bogus" type="DryWetEffect" uniqueTypeId="ZZZZ" baseType="DryWetEffect" index="2">
      <RenderPass name="Bogus" type="Bogus" uniqueTypeId="ZZZZ" baseType="Effect"/>
    </RenderPass>
  </RenderPass></VideoTrack></Layer>
  <Deck name="Deck" deckIndex="0" numColumns="1" numLayers="1">
    <Clip name="Clip" layerIndex="0" columnIndex="0">
      <PreloadData><VideoFile value="D:\VJ\one.mov"/></PreloadData>
      <VideoTrack name="VideoTrack">
        <PrimarySource><VideoSource name="VideoSource" type="VideoFormatReaderSource"/></PrimarySource>
        <RenderPass name="RenderPassChain" type="RenderPassChain" baseType="RenderPassChain">
          <RenderPass storage="0" name="ShiftRGB" type="DryWetEffect" uniqueTypeId="A153" baseType="DryWetEffect" index="2">
            <Params name="Params"><Param name="Bypassed" T="BOOL" default="0" value="1"/>
              <ParamRange name="Opacity" T="DOUBLE" default="1" value="0.5"/></Params>
            <RenderPass name="ShiftRGB" type="ShiftRGB" uniqueTypeId="A153" baseType="Effect"><Params name="Params">
              <ParamChoice name="Mode" T="INT32" default="0" value="2"/><Param name="Black BG" T="BOOL" value="1"/></Params></RenderPass>
            <ChoosableMixer name="Mixer"><RenderPass name="Add" type="Add" baseType="Mixer"/></ChoosableMixer>
          </RenderPass>
          <RenderPass storage="0" name="Transform" type="TransformEffect" uniqueTypeId="17122039101699797593" baseType="Effect" index="0">
            <Params name="Params"><ParamRange name="Scale" T="DOUBLE" default="100" value="50"/><ParamRange name="Position Y" T="DOUBLE" default="0" value="274"/></Params>
          </RenderPass>
          <RenderPass storage="0" name="Mirror" type="DryWetEffect" uniqueTypeId="A108" baseType="DryWetEffect" index="1">
            <RenderPass name="Mirror" type="Mirror" uniqueTypeId="A108" baseType="Effect"><Params name="Params">
              <ParamRange name="X" T="DOUBLE" default="0" value="0.3">
                <DurationSource name="DurationSource" defaultDuration="4b" userDuration="16" scale="2"/>
                <PhaseSourceTimeline name="PhaseSourceTimeline" phase="0.25" defaultBeatsDuration="16"><Params name="Params">
                  <ParamChoice name="PlayDirection" T="INT32" default="2" value="0"/>
                  <ParamChoice name="PlayMode" T="INT32" default="0" value="1"/></Params></PhaseSourceTimeline>
                <BehaviourDouble name="BehaviourDouble"><Modifier><ModifierEnvelope name="ModifierEnvelope"><points>
                  <point x="0" y="0" curve="33"/><point x="0.5" y="1" curve="33"/><point x="1" y="0" curve="1"/></points></ModifierEnvelope></Modifier></BehaviourDouble>
                <ValueRange name="startStop" min="0.2" max="0.6"/>
              </ParamRange>
              <ParamRange name="Y" T="DOUBLE" default="0" value="0">
                <DurationSource name="DurationSource" scale="0.5"/>
                <PhaseSourceTimeline name="PhaseSourceTimeline" phase="0"><Params name="Params">
                  <ParamRange name="Speed" T="DOUBLE" default="1" value="0.5"/></Params></PhaseSourceTimeline>
              </ParamRange></Params></RenderPass>
          </RenderPass>
        </RenderPass>
      </VideoTrack>
    </Clip>
  </Deck>
</Composition>"#;

    #[test]
    fn imports_effect_chains() {
        use lavmfx_core::Param;
        let mut r = PathResolver::new(None);
        let Imported { composition: c, report } = from_str(AVC_FX, &mut r, "fx").unwrap();
        let names = |chain: &[lavmfx_core::VideoEffect]| chain.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        let range = |e: &lavmfx_core::VideoEffect, k: &str| match e.params.get(k) {
            Some(Param::Range(r)) => r.value,
            _ => f64::NAN,
        };
        // Composition: its changed Transform (dome.avc rotates a group 90°), Invert RGB by id.
        let comp = &c.video.as_ref().unwrap().effects;
        assert_eq!(names(comp), ["Transform", "InvertRGB"]);
        assert_eq!(range(&comp[0], "Rotation Z"), 90.0);
        // Layer: Blur with its saved amount; the unknown effect is reported, not added.
        let layer = &c.layers[0].video.as_ref().unwrap().track.effects;
        assert_eq!(names(layer), ["Blur"]);
        assert_eq!(range(&layer[0], "Blur"), 0.25);
        assert!(report.unknown_effects.contains("Bogus"));
        // Dashboard: knob 2 named "Soft" at 0.5; Blur follows it between 0.2 and 0.8.
        let dash = &c.layers[0].dashboard;
        assert_eq!(lavmfx_core::drive::link_label(dash, 1), "Soft");
        assert_eq!(lavmfx_core::drive::link_label(dash, 0), "Link 1");
        assert!(matches!(dash.get("Link 2"), Some(Param::Range(r)) if r.value == 0.5));
        let Some(Param::Range(blur)) = layer[0].params.get("Blur") else {
            panic!()
        };
        let d = blur.drive.as_ref().unwrap();
        assert_eq!(
            (d.source, d.link, d.from, d.to),
            (lavmfx_core::drive::Source::Dashboard, 1, 0.2, 0.8)
        );
        assert_eq!((report.dashboard_knobs, report.dashboard_links), (1, 1));
        // Clip: saved order by `index` (Transform, Mirror, Shift RGB), Transform values,
        // wrapper Bypass / Opacity / blend, choice by index, bool.
        let clip = &c.layers[0].clips[0].video.as_ref().unwrap().track.effects;
        assert_eq!(names(clip), ["Transform", "Mirror", "ShiftRGB"]);
        assert_eq!((range(&clip[0], "Scale"), range(&clip[0], "Position Y")), (50.0, 274.0));
        let shift = &clip[2];
        assert!(shift.is_bypassed());
        assert_eq!(shift.opacity(), 0.5);
        assert!(matches!(shift.mixer.get(lavmfx_core::effects::BLEND_KEY), Some(Param::Choice(m)) if m.value == "Add"));
        assert!(matches!(shift.params.get("Mode"), Some(Param::Choice(m)) if m.value == "Rotate X"));
        assert!(matches!(shift.params.get("Black BG"), Some(Param::Bool(b)) if b.value));
        assert!(!clip[1].is_bypassed());
        assert_eq!(report.effects, 4);
        // Timeline in beats → BPM Sync 16 / scale 2 = 8 beats, backward, bounce,
        // range 0.2..0.6, envelope: step up at 0.5 (curve on the arriving point).
        use lavmfx_core::drive::{Curve, Direction, PlayMode, Source};
        let Some(Param::Range(x)) = clip[1].params.get("X") else {
            panic!()
        };
        let d = x.drive.as_ref().unwrap();
        assert_eq!(
            (d.source, d.beats, d.direction, d.mode),
            (Source::BpmSync, 8.0, Direction::Backward, PlayMode::Bounce)
        );
        assert_eq!((d.from, d.to, d.phase), (0.2, 0.6, 0.25));
        let e = d.envelope.as_ref().unwrap();
        assert_eq!(
            e.points.iter().map(|p| p.curve).collect::<Vec<_>>(),
            [Curve::Hold, Curve::Linear, Curve::Linear]
        );
        // Seconds: 5 s ÷ (scale 0.5 × Speed 0.5) = 20 s, whole range.
        let Some(Param::Range(y)) = clip[1].params.get("Y") else {
            panic!()
        };
        let d = y.drive.as_ref().unwrap();
        assert_eq!((d.source, d.seconds, d.from, d.to), (Source::Timeline, 20.0, 0.0, 1.0));
    }

    #[test]
    fn mixer_type_names() {
        assert_eq!(mode_name("Add50", LAYER_BLEND_MODES), Some("50 Add"));
        assert_eq!(mode_name("LumaKey", LAYER_BLEND_MODES), Some("Luma Key"));
        assert_eq!(mode_name("BW", LAYER_BLEND_MODES), Some("B&W"));
        assert_eq!(mode_name("Alpha", MIX_MODES), Some("Alpha"));
        assert_eq!(mode_name("Nonsense", MIX_MODES), None);
    }

    #[test]
    fn rejects_other_xml() {
        let mut r = PathResolver::with(None, HashMap::new(), Vec::new());
        assert!(from_str("<Other/>", &mut r, "x").is_err());
    }

    /// Diagnostic: `LAVMFX_AVC=show.avc cargo test -p lavmfx-project --release real_file -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_file() {
        let Ok(p) = std::env::var("LAVMFX_AVC") else { return };
        let t = std::time::Instant::now();
        let Imported { composition: c, report } = import(Path::new(&p)).unwrap();
        eprintln!("{:.0} ms: {}", t.elapsed().as_secs_f64() * 1000.0, report.summary());
        eprintln!(
            "bpm sync {} · unknown modes {:?} · drives {:?}",
            report.bpm_sync, report.unknown_modes, report.drives
        );
        eprintln!(
            "effects {} · unknown effects {:?}",
            report.effects, report.unknown_effects
        );
        eprintln!(
            "dashboard knobs {} · links {} · animations {}",
            report.dashboard_knobs, report.dashboard_links, report.animations
        );
        eprintln!(
            "autopilot layers {} · clip loops {}",
            report.autopilot_layers, report.autopilot_loops
        );
        eprintln!(
            "transports {} · clip blends {} · group speeds {} · spout {} {:?}",
            report.transports, report.clip_blends, report.group_speeds, report.spout, report.spout_senders
        );
        let t = std::time::Instant::now();
        let cl = c.clone();
        eprintln!("clone {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
        let t = std::time::Instant::now();
        let _ = lavmfx_core::history::undo_key(&cl);
        eprintln!("undo key {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
        // BPM Sync loop lengths (active deck).
        let mut beats = std::collections::BTreeMap::new();
        for cl in c.layers.iter().flat_map(|l| &l.clips) {
            if let Some(lavmfx_core::Transport::BpmSync(t)) = &cl.transport {
                *beats.entry(t.controls.duration.value as i64).or_insert(0) += 1;
            }
        }
        eprintln!("bpm sync beats (active deck): {beats:?}");
        for d in &c.decks {
            let cols = d.stash.as_ref().map_or(0, |s| s.columns.len());
            eprintln!("deck {:?}: {} columns", d.name.value, cols);
        }
        let fxs = |ch: &[lavmfx_core::VideoEffect]| {
            ch.iter()
                .map(|e| {
                    let vals: Vec<String> = e
                        .params
                        .iter()
                        .filter_map(|(k, p)| match p {
                            lavmfx_core::Param::Range(r) if r.default.is_some_and(|d| d != r.value) => {
                                Some(format!("{k}={:.2}", r.value))
                            }
                            _ => None,
                        })
                        .collect();
                    format!(
                        "{}{}[{}]",
                        e.name,
                        if e.is_bypassed() { "(B)" } else { "" },
                        vals.join(" ")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        eprintln!(
            "comp fx: {}",
            c.video.as_ref().map(|v| fxs(&v.effects)).unwrap_or_default()
        );
        for (i, g) in c.layergroups.iter().enumerate() {
            eprintln!(
                "group {} {:?} master {:.2} opacity {:.2} fx: {}",
                i + 1,
                g.name.value,
                g.master.value,
                g.video.as_ref().map_or(1.0, |v| v.opacity.value),
                g.video.as_ref().map(|v| fxs(&v.effects)).unwrap_or_default()
            );
        }
        for (i, l) in c.layers.iter().enumerate() {
            eprintln!(
                "  layer {} group {:?} master {:.2} bypass {} blend {:?} fx: {}",
                i + 1,
                l.group,
                l.master.value,
                l.bypassed.value,
                l.blend_mode(),
                fxs(&l.video.as_ref().unwrap().track.effects)
            );
        }
        for (i, l) in c.layers.iter().enumerate() {
            let filled = l.clips.iter().filter(|c| !c.is_empty()).count();
            eprintln!(
                "layer {} {:?}: {} clips, opacity {:.2}, transition {:.2}s",
                i + 1,
                l.name.value,
                filled,
                l.video.as_ref().unwrap().track.opacity.value,
                l.transition.duration.value
            );
        }
    }
}
