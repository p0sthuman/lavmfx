//! Constructors for a fresh composition and its parts.
//!
//! Choice option lists and defaults follow the REST shape controllers expect.

use crate::id::IdGen;
use crate::model::*;
use crate::param::*;

pub const COLOR_IDS: &[&str] = &["1", "2", "3", "4", "5", "6"];
pub const CLIP_TARGETS: &[&str] = &[
    "Active Layer",
    "Own Layer",
    "Free Layer",
    "Denon Player Determined",
    "Pioneer Player Determined",
];
pub const TRIGGER_STYLES: &[&str] = &["Normal", "Piano", "Toggle"];
pub const BEAT_SNAP: &[&str] = &["None", "8 Bars", "4 Bars", "2 Bars", "1 Bar", "1/2 Bar", "1/4 Bar"];
pub const CROSSFADER_GROUPS: &[&str] = &["None", "A", "B"];
pub const MASK_MODES: &[&str] = &["Disabled", "One Below", "All Below"];
/// Transition / crossfader / effect mixer modes, names and order.
/// Which ones render is decided by [`crate::mix::shader_blend`].
pub const MIX_MODES: &[&str] = &[
    "Add",
    "Alpha",
    "B&W",
    "Burn",
    "Cube",
    "Cut",
    "Darken",
    "Difference",
    "Difference I",
    "Displace",
    "Dissolve",
    "Dodge",
    "Exclusion",
    "Hard Light",
    "JitterBug",
    "Lighten",
    "LoRez",
    "Luma Is Alpha",
    "Luma Key",
    "Luma Key I",
    "Meta Mix",
    "Multiply",
    "Multi Task",
    "Noisy",
    "Overlay",
    "Parts",
    "PiP",
    "Push Down",
    "Push Left",
    "Push Right",
    "Push Up",
    "RGB",
    "Rotate X",
    "Rotate Y",
    "Screen",
    "Shift RGB",
    "Side by Side",
    "Soft Light",
    "Static",
    "Subtract",
    "Tile",
    "TimeSwitcher",
    "to Black",
    "to Color",
    "to White",
    "Twitch",
    "Wipe Ellipse",
    "Zoom In",
    "Zoom Out",
];
/// Layer blend modes: the "50 …" variants followed by [`MIX_MODES`].
pub const LAYER_BLEND_MODES: &[&str] = &[
    "50 Add",
    "50 Burn",
    "50 Difference",
    "50 Distance",
    "50 Dodge",
    "50 Lighten",
    "50 Mask",
    "50 Multiply",
    "50 Overlay",
    "50 Subtract",
    "Add",
    "Alpha",
    "B&W",
    "Burn",
    "Cube",
    "Cut",
    "Darken",
    "Difference",
    "Difference I",
    "Displace",
    "Dissolve",
    "Dodge",
    "Exclusion",
    "Hard Light",
    "JitterBug",
    "Lighten",
    "LoRez",
    "Luma Is Alpha",
    "Luma Key",
    "Luma Key I",
    "Meta Mix",
    "Multiply",
    "Multi Task",
    "Noisy",
    "Over",
    "Overlay",
    "Parts",
    "PiP",
    "Push Down",
    "Push Left",
    "Push Right",
    "Push Up",
    "RGB",
    "Rotate X",
    "Rotate Y",
    "Screen",
    "Shift RGB",
    "Side by Side",
    "Soft Light",
    "Static",
    "Subtract",
    "Tile",
    "TimeSwitcher",
    "to Black",
    "to Color",
    "to White",
    "Twitch",
    "Wipe Ellipse",
    "Zoom In",
    "Zoom Out",
];
/// Default layer blend mode of a new composition.
pub const DEFAULT_LAYER_BLEND: &str = "Add";
pub const AUTOSIZE: &[&str] = &["Off", "Fill", "Fit", "Stretch"];
pub const CLIP_RESIZE: &[&str] = &["Fill", "Fit", "Stretch", "Original"];
pub const PLAY_DIRECTIONS: &[&str] = &["<", "||", ">"];
pub const PLAY_MODES: &[&str] = &["Loop", "Bounce", "Random", "Play Once & Clear", "Play Once & Hold"];
pub const PLAY_MODE_AWAY: &[&str] = &["Restart", "Continue", "Relative"];
pub const TRANSPORT_TYPES: &[&str] = &["Timeline", "BPM Sync", "SMPTE 1", "SMPTE 2", "Denon DJ", "Pioneer DJ"];
/// BPM Sync: loop length in beats ("Beats") or a clip tempo ("BPM").
pub const SYNC_MODES: &[&str] = &["Beats", "BPM"];
pub const BEAT_LOOPS: &[&str] = &[
    "Off",
    "4 Beats",
    "2 Beats",
    "1 Beats",
    "1/2 Beat",
    "1/4 Beat",
    "1/8 Beat",
    "1/16 Beat",
    "1/6 Beat",
    "1/12 Beat",
];
pub const CROSSFADER_BEHAVIOURS: &[&str] = &["Cut", "Cut & Return", "Jump", "Fade (Legacy)"];
pub const CROSSFADER_CURVES: &[&str] = &["Linear", "Gentle", "Fast", "Cut"];
pub const CLIP_TARGET_OPTIONS: &[&str] = &[
    "Composition Determined",
    "Active Layer",
    "Own Layer",
    "Free Layer",
    "Denon Player Determined",
    "Pioneer Player Determined",
];
pub const CLIP_TRIGGER_STYLES: &[&str] = &["Composition Determined", "Normal", "Piano", "Toggle"];
pub const CLIP_BEAT_SNAP: &[&str] = &[
    "Composition Determined",
    "None",
    "8 Bars",
    "4 Bars",
    "2 Bars",
    "1 Bar",
    "1/2 Bar",
    "1/4 Bar",
];
pub const LAYER_DETERMINED: &[&str] = &["Layer Determined", "On", "Off"];

/// Choice with the option named `value` selected (first option if missing).
pub fn choice(ids: &mut IdGen, options: &[&str], value: &str) -> ChoiceParam {
    ChoiceParam::new(
        ids.alloc(),
        options,
        options.iter().position(|o| *o == value).unwrap_or(0),
    )
}

fn percent(ids: &mut IdGen, value: f64) -> RangeParam {
    RangeParam::new(ids.alloc(), 0.0, 1.0, value).with_view(ParameterView::units(DisplayUnits::Percent))
}

/// A layer / group master missing from a file: 100 %, id 0 until
/// [`Composition::ensure_master_ids`] gives it a real one.
pub fn missing_master() -> RangeParam {
    let mut ids = IdGen::new();
    let mut p = percent(&mut ids, 1.0);
    p.id = 0;
    p
}

impl Composition {
    /// Masters read without an id (see [`missing_master`]) get fresh ones.
    pub fn ensure_master_ids(&mut self) {
        let ids = &mut self.ids;
        let masters = self.layers.iter_mut().map(|l| &mut l.master);
        for m in masters.chain(self.layergroups.iter_mut().map(|g| &mut g.master)) {
            if m.id == 0 {
                m.id = ids.alloc();
            }
        }
    }

    /// Empty composition: one deck, `layers` layers × `columns` columns of empty clips.
    pub fn new(name: &str, layers: usize, columns: usize) -> Self {
        let mut ids = IdGen::new();
        let mut c = Composition {
            name: StringParam::new(ids.alloc(), name),
            selected: None,
            bypassed: BoolParam::new(ids.alloc(), false),
            master: percent(&mut ids, 1.0),
            speed: percent(&mut ids, 1.0),
            cliptarget: choice(&mut ids, CLIP_TARGETS, "Own Layer"),
            cliptriggerstyle: ChoiceParam::new(ids.alloc(), TRIGGER_STYLES, 0),
            clipbeatsnap: ChoiceParam::new(ids.alloc(), BEAT_SNAP, 0),
            dashboard: ParameterCollection::new(),
            audio: Some(AudioTrack::new(&mut ids)),
            video: Some(VideoTrack::new(&mut ids)),
            crossfader: CrossFader::new(&mut ids),
            decks: Vec::new(),
            layers: Vec::new(),
            columns: Vec::new(),
            layergroups: Vec::new(),
            tempo_controller: TempoController::new(&mut ids),
            chase: Default::default(),
            ids,
        };
        c.decks.push(Deck::new(&mut c.ids, "Deck 1"));
        c.decks[0].selected.as_mut().unwrap().value = true;
        for i in 0..columns {
            c.columns.push(Column::new(&mut c.ids, &format!("Column {}", i + 1)));
        }
        for i in 0..layers {
            let layer = Layer::new(&mut c.ids, &format!("Layer {}", i + 1), columns);
            c.layers.push(layer);
        }
        c
    }

    /// Add a column (and an empty clip slot in every layer).
    pub fn add_column(&mut self) {
        let n = self.columns.len() + 1;
        self.columns.push(Column::new(&mut self.ids, &format!("Column {n}")));
        for l in &mut self.layers {
            l.clips.push(Clip::empty(&mut self.ids));
        }
    }

    /// Keep a spare empty column at the far right of the active deck: adds
    /// one when the last column holds a clip. Returns true if it added one.
    pub fn ensure_spare_column(&mut self) -> bool {
        let last_empty =
            !self.columns.is_empty() && self.layers.iter().all(|l| l.clips.last().is_none_or(Clip::is_empty));
        if last_empty {
            return false;
        }
        self.add_column();
        true
    }

    /// Add a layer on top with one empty slot per column.
    pub fn add_layer(&mut self) {
        let n = self.layers.len() + 1;
        let layer = Layer::new(&mut self.ids, &format!("Layer {n}"), self.columns.len());
        self.layers.push(layer);
    }
}

impl LayerGroup {
    pub fn new(ids: &mut IdGen, name: &str) -> Self {
        Self {
            id: ids.alloc(),
            name: StringParam::new(ids.alloc(), name),
            colorid: ChoiceParam::new(ids.alloc(), COLOR_IDS, 0),
            selected: None,
            bypassed: BoolParam::new(ids.alloc(), false),
            solo: BoolParam::new(ids.alloc(), false),
            crossfadergroup: ChoiceParam::new(ids.alloc(), CROSSFADER_GROUPS, 0),
            master: percent(ids, 1.0),
            speed: None,
            ignorecolumntrigger: BoolParam::new(ids.alloc(), false),
            dashboard: ParameterCollection::new(),
            audio: Some(AudioTrack::new(ids)),
            video: Some(VideoTrack::new(ids)),
            layers: Vec::new(),
            folded: false,
        }
    }
}

impl LayerGroup {
    /// The group's Speed param (0..10, 1 = normal), created if missing.
    pub fn speed_mut(&mut self, ids: &mut IdGen) -> &mut RangeParam {
        self.speed.get_or_insert_with(|| {
            RangeParam::new(ids.alloc(), 0.0, 10.0, 1.0).with_view(ParameterView::units(DisplayUnits::Percent))
        })
    }
}

impl Composition {
    /// Speed factor of layer `li`'s clips from its group (1 outside a group).
    pub fn layer_speed(&self, li: usize) -> f64 {
        self.layers
            .get(li)
            .and_then(|l| l.group)
            .and_then(|id| self.layergroups.iter().find(|g| g.id == id))
            .and_then(|g| g.speed.as_ref())
            .map_or(1.0, |s| s.value.max(0.0))
    }
}

impl Deck {
    pub fn new(ids: &mut IdGen, name: &str) -> Self {
        Self {
            id: ids.alloc(),
            name: StringParam::new(ids.alloc(), name),
            colorid: ChoiceParam::new(ids.alloc(), COLOR_IDS, 0),
            selected: Some(BoolParam::new(ids.alloc(), false)),
            scrollx: IntParam::new(ids.alloc(), 0),
            stash: None,
        }
    }
}

impl Column {
    pub fn new(ids: &mut IdGen, name: &str) -> Self {
        Self {
            id: ids.alloc(),
            name: StringParam::new(ids.alloc(), name),
            colorid: ChoiceParam::new(ids.alloc(), COLOR_IDS, 0),
            connected: None,
        }
    }
}

impl Layer {
    pub fn new(ids: &mut IdGen, name: &str, columns: usize) -> Self {
        Self {
            id: ids.alloc(),
            name: StringParam::new(ids.alloc(), name),
            colorid: ChoiceParam::new(ids.alloc(), COLOR_IDS, 0),
            selected: None,
            bypassed: BoolParam::new(ids.alloc(), false),
            solo: BoolParam::new(ids.alloc(), false),
            crossfadergroup: ChoiceParam::new(ids.alloc(), CROSSFADER_GROUPS, 0),
            master: percent(ids, 1.0),
            maskmode: ChoiceParam::new(ids.alloc(), MASK_MODES, 0),
            ignorecolumntrigger: BoolParam::new(ids.alloc(), false),
            faderstart: BoolParam::new(ids.alloc(), false),
            dashboard: ParameterCollection::new(),
            audio: Some(AudioTrack::new(ids)),
            video: Some(VideoTrackLayer {
                track: {
                    let mut t = VideoTrack::new(ids);
                    let blend = crate::mix::blend_mode_param(ids);
                    t.mixer.insert(crate::mix::BLEND_MODE_KEY.into(), Param::Choice(blend));
                    t
                },
                autosize: choice(ids, AUTOSIZE, "Off"),
            }),
            transition: LayerTransition {
                duration: RangeParam::new(ids.alloc(), 0.0, 10.0, 0.0)
                    .with_view(ParameterView::units(DisplayUnits::Seconds)),
                blend_mode: choice(ids, MIX_MODES, "Alpha"),
            },
            clips: (0..columns).map(|_| Clip::empty(ids)).collect(),
            autopilot: Some(crate::model::AutoPilot::new(ids)),
            group: None,
            video_off: false,
            audio_off: false,
        }
    }
}

impl Clip {
    pub fn empty(ids: &mut IdGen) -> Self {
        Self {
            id: ids.alloc(),
            name: StringParam::new(ids.alloc(), ""),
            colorid: ChoiceParam::new(ids.alloc(), COLOR_IDS, 0),
            selected: None,
            connected: None,
            target: ChoiceParam::new(ids.alloc(), CLIP_TARGET_OPTIONS, 0),
            triggerstyle: ChoiceParam::new(ids.alloc(), CLIP_TRIGGER_STYLES, 0),
            ignorecolumntrigger: ChoiceParam::new(ids.alloc(), LAYER_DETERMINED, 0),
            faderstart: ChoiceParam::new(ids.alloc(), LAYER_DETERMINED, 0),
            beatsnap: ChoiceParam::new(ids.alloc(), CLIP_BEAT_SNAP, 0),
            transporttype: ChoiceParam::new(ids.alloc(), TRANSPORT_TYPES, 0),
            transport: None,
            dashboard: ParameterCollection::new(),
            audio: None,
            video: None,
            thumbnail: None,
            autopilot: None,
        }
    }
}

impl TransportTimeline {
    pub fn new(ids: &mut IdGen, duration_s: f64) -> Self {
        Self {
            position: RangeParam::new(ids.alloc(), 0.0, duration_s * 1000.0, 0.0)
                .with_view(ParameterView::units(DisplayUnits::Milliseconds)),
            controls: TimelineControls {
                playdirection: ChoiceParam::new(ids.alloc(), PLAY_DIRECTIONS, 2),
                playmode: ChoiceParam::new(ids.alloc(), PLAY_MODES, 0),
                playmodeaway: choice(ids, PLAY_MODE_AWAY, "Restart"),
                duration: RangeParam::new(ids.alloc(), 0.0, f64::MAX, duration_s)
                    .with_view(ParameterView::units(DisplayUnits::Seconds)),
                speed: RangeParam::new(ids.alloc(), 0.0, 10.0, 1.0),
                smpteoffset: None,
            },
        }
    }
}

impl BpmSyncControls {
    /// Sync mode "BPM" needs the clip's own tempo (added on demand, 120).
    pub fn ensure_bpm(&mut self, ids: &mut IdGen) {
        if self.syncmode.value == "BPM" && self.bpm.is_none() {
            self.bpm = Some(RangeParam::new(ids.alloc(), 20.0, 500.0, 120.0));
        }
    }
}

impl TransportBpmSync {
    /// BPM Sync transport (REST shape):
    /// the file plays as one loop of `beats` beats.
    pub fn new(ids: &mut IdGen, file_s: f64, beats: f64) -> Self {
        Self {
            position: RangeParam::new(ids.alloc(), 0.0, file_s * 1000.0, 0.0)
                .with_view(ParameterView::units(DisplayUnits::Milliseconds)),
            controls: BpmSyncControls {
                playdirection: ChoiceParam::new(ids.alloc(), PLAY_DIRECTIONS, 2),
                playmode: ChoiceParam::new(ids.alloc(), PLAY_MODES, 0),
                playmodeaway: choice(ids, PLAY_MODE_AWAY, "Restart"),
                duration: RangeParam::new(ids.alloc(), 0.0078125, 1_048_576.0, beats)
                    .with_view(ParameterView::units(DisplayUnits::Beats)),
                speed: RangeParam::new(ids.alloc(), 0.0, 16.0, 1.0),
                bpm: None,
                syncmode: choice(ids, SYNC_MODES, "Beats"),
                beatloop: choice(ids, BEAT_LOOPS, "Off"),
            },
        }
    }
}

impl VideoTrack {
    pub fn new(ids: &mut IdGen) -> Self {
        Self {
            width: RangeParam::new(ids.alloc(), 1.0, 16384.0, 1920.0),
            height: RangeParam::new(ids.alloc(), 1.0, 16384.0, 1080.0),
            opacity: percent(ids, 1.0),
            mixer: ParameterCollection::new(),
            effects: Vec::new(),
        }
    }
}

impl AudioTrack {
    pub fn new(ids: &mut IdGen) -> Self {
        Self {
            volume: RangeParam::new(ids.alloc(), -192.0, 12.0, 0.0)
                .with_view(ParameterView::units(DisplayUnits::Decibels)),
            pan: RangeParam::new(ids.alloc(), -1.0, 1.0, 0.0),
            effects: Vec::new(),
        }
    }
}

impl CrossFader {
    pub fn new(ids: &mut IdGen) -> Self {
        Self {
            id: ids.alloc(),
            phase: RangeParam::new(ids.alloc(), -1.0, 1.0, 0.0),
            behaviour: ChoiceParam::new(ids.alloc(), CROSSFADER_BEHAVIOURS, 0),
            curve: ChoiceParam::new(ids.alloc(), CROSSFADER_CURVES, 0),
            sidea: EventParam::new(ids.alloc()),
            sideb: EventParam::new(ids.alloc()),
            mixer: ParameterCollection::new(),
        }
    }
}

impl TempoController {
    pub fn new(ids: &mut IdGen) -> Self {
        Self {
            tempo: RangeParam::new(ids.alloc(), 20.0, 500.0, 120.0),
            tempo_pull: EventParam::new(ids.alloc()),
            tempo_push: EventParam::new(ids.alloc()),
            tempo_tap: EventParam::new(ids.alloc()),
            resync: EventParam::new(ids.alloc()),
        }
    }
}

impl VideoTrackClip {
    pub fn new(ids: &mut IdGen, fileinfo: VideoFileInfo) -> Self {
        let mut track = VideoTrack::new(ids);
        track.width.set(fileinfo.width as f64);
        track.height.set(fileinfo.height as f64);
        Self {
            track,
            description: None,
            fileinfo: Some(fileinfo),
            resize: choice(ids, CLIP_RESIZE, "Fit"),
            r: BoolParam::new(ids.alloc(), true),
            g: BoolParam::new(ids.alloc(), true),
            b: BoolParam::new(ids.alloc(), true),
            a: BoolParam::new(ids.alloc(), true),
            sourceparams: None,
            source: None,
        }
    }
}

impl Clip {
    /// Put a video file into this slot (keeps the slot's id). Transport is a
    /// looping timeline of the file's length.
    pub fn load_video(&mut self, ids: &mut IdGen, name: &str, fileinfo: VideoFileInfo) {
        let secs = fileinfo.duration_ms / 1000.0;
        self.name.value = name.to_string();
        self.transporttype.select(0);
        self.transport = Some(Transport::Timeline(TransportTimeline::new(ids, secs)));
        self.video = Some(VideoTrackClip::new(ids, fileinfo));
    }

    /// Length of the clip's file in seconds (0 if unknown).
    pub fn file_seconds(&self) -> f64 {
        if self.source().is_some() {
            return crate::sources::VIRTUAL_SECONDS;
        }
        self.video
            .as_ref()
            .and_then(|v| v.fileinfo.as_ref())
            .map_or(0.0, |f| f.duration_ms / 1000.0)
    }

    /// Switch transport type (index into [`TRANSPORT_TYPES`]: Timeline,
    /// BPM Sync, SMPTE 1, SMPTE 2; Denon / Pioneer aren't supported).
    /// Direction, play modes and "away" carry over; speed resets to 1. A new
    /// BPM Sync loop gets the power-of-two beat count closest to the file's
    /// length at `bpm`. SMPTE uses the Timeline transport plus an offset.
    pub fn set_transport_type(&mut self, ids: &mut IdGen, index: usize, bpm: f64) -> bool {
        if index > 3 || self.transport.is_none() {
            return false;
        }
        let file_s = self.file_seconds();
        let want_bpm = index == 1;
        let is_bpm = matches!(self.transport, Some(Transport::BpmSync(_)));
        if want_bpm != is_bpm {
            let (dir, mode, away) = match &self.transport {
                Some(Transport::Timeline(t)) => (
                    &t.controls.playdirection,
                    &t.controls.playmode,
                    &t.controls.playmodeaway,
                ),
                Some(Transport::BpmSync(t)) => (
                    &t.controls.playdirection,
                    &t.controls.playmode,
                    &t.controls.playmodeaway,
                ),
                None => return false,
            };
            let (dir, mode, away) = (dir.index, mode.index, away.value.clone());
            let mut t = if want_bpm {
                let beats = file_s * bpm / 60.0;
                let beats = if beats > 0.0 {
                    2f64.powf(beats.log2().round()).max(1.0)
                } else {
                    16.0
                };
                Transport::BpmSync(TransportBpmSync::new(ids, file_s, beats))
            } else {
                Transport::Timeline(TransportTimeline::new(ids, file_s))
            };
            let (d, m, a) = match &mut t {
                Transport::Timeline(t) => (
                    &mut t.controls.playdirection,
                    &mut t.controls.playmode,
                    &mut t.controls.playmodeaway,
                ),
                Transport::BpmSync(t) => (
                    &mut t.controls.playdirection,
                    &mut t.controls.playmode,
                    &mut t.controls.playmodeaway,
                ),
            };
            d.select(dir as usize);
            m.select(mode as usize);
            a.select_value(&away);
            self.transport = Some(t);
        } else if self.transporttype.index as usize == index {
            return false;
        }
        if index >= 2
            && let Some(Transport::Timeline(t)) = &mut self.transport
            && t.controls.smpteoffset.is_none()
        {
            t.controls.smpteoffset = Some(RangeParam::new(ids.alloc(), 0.0, 86_400.0, 0.0));
        }
        self.transporttype.select(index);
        true
    }

    /// Path of the clip's video file, if any.
    pub fn video_path(&self) -> Option<&str> {
        self.video.as_ref()?.fileinfo.as_ref().map(|f| f.path.as_str())
    }

    /// The clip's file wasn't found (e.g. imported from another machine).
    pub fn is_missing(&self) -> bool {
        self.video
            .as_ref()
            .and_then(|v| v.fileinfo.as_ref())
            .is_some_and(|f| !f.exists)
    }
}

impl Clip {
    /// Copy of this clip with fresh ids for it and all its parameters.
    pub fn duplicate(&self, ids: &mut IdGen) -> Clip {
        with_fresh_ids(self, ids)
    }
}

/// Copy of any model value with a fresh id for every `"id"` in it.
pub(crate) fn with_fresh_ids<T: serde::Serialize + serde::de::DeserializeOwned>(x: &T, ids: &mut IdGen) -> T {
    fn renumber(v: &mut serde_json::Value, ids: &mut IdGen) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, v) in m.iter_mut() {
                    if k == "id" && v.is_i64() {
                        *v = ids.alloc().into();
                    } else {
                        renumber(v, ids);
                    }
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(|v| renumber(v, ids)),
            _ => {}
        }
    }
    let mut v = serde_json::to_value(x).expect("model serialises");
    renumber(&mut v, ids);
    serde_json::from_value(v).expect("model round-trips")
}
