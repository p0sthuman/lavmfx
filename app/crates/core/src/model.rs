//! Composition model.
//!
//! Struct and field names mirror the REST API schema
//! (`rest/docs/schemas/{Composition,Deck,Layer,Clip,...}.yaml`) so that
//! `serde_json` output matches what REST clients expect. Read-only, computed
//! fields from the schema (`selected`, `connected`, `thumbnail`, `fileinfo`)
//! are runtime state and are skipped when absent.

use serde::{Deserialize, Serialize};

use crate::id::{Id, IdGen};
use crate::param::*;

// ---------------------------------------------------------------- composition

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Composition {
    pub name: StringParam,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<BoolParam>,
    pub bypassed: BoolParam,
    pub master: RangeParam,
    pub speed: RangeParam,
    pub cliptarget: ChoiceParam,
    pub cliptriggerstyle: ChoiceParam,
    pub clipbeatsnap: ChoiceParam,
    #[serde(default)]
    pub dashboard: ParameterCollection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioTrack>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoTrack>,
    pub crossfader: CrossFader,
    pub decks: Vec<Deck>,
    pub layers: Vec<Layer>,
    pub columns: Vec<Column>,
    #[serde(default)]
    pub layergroups: Vec<LayerGroup>,
    pub tempo_controller: TempoController,

    /// Slice Chase sequences (not part of the REST schema).
    #[serde(default, skip_serializing_if = "crate::chase::ChaseSetup::is_empty")]
    pub chase: crate::chase::ChaseSetup,

    /// Id source for new objects. Not part of the REST schema; the REST
    /// layer (M7) serialises a view without it.
    #[serde(default)]
    pub ids: IdGen,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deck {
    pub id: Id,
    pub name: StringParam,
    pub colorid: ChoiceParam,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<BoolParam>,
    pub scrollx: IntParam,
    /// This deck's columns and clips while another deck is active (the
    /// active deck's live in `Composition::{columns, layers[].clips}`).
    /// Not part of the REST schema; `None` = never filled (empty deck).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stash: Option<DeckContent>,
}

/// Columns and per-layer clips of an inactive deck.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeckContent {
    pub columns: Vec<Column>,
    /// Indexed by layer, then column.
    pub clips: Vec<Vec<Clip>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub id: Id,
    pub name: StringParam,
    pub colorid: ChoiceParam,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected: Option<ChoiceParam>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub id: Id,
    pub name: StringParam,
    pub colorid: ChoiceParam,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<BoolParam>,
    pub bypassed: BoolParam,
    pub solo: BoolParam,
    pub crossfadergroup: ChoiceParam,
    /// Layer master (video and audio). Files saved 2026-11-25 while it was
    /// briefly removed lack it: 100 %, id assigned by [`Composition::ensure_master_ids`].
    #[serde(default = "crate::defaults::missing_master")]
    pub master: RangeParam,
    pub maskmode: ChoiceParam,
    pub ignorecolumntrigger: BoolParam,
    pub faderstart: BoolParam,
    #[serde(default)]
    pub dashboard: ParameterCollection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioTrack>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoTrackLayer>,
    pub transition: LayerTransition,
    pub clips: Vec<Clip>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autopilot: Option<AutoPilot>,
    /// Id of the [`LayerGroup`] this layer belongs to. Not in the REST
    /// schema (there groups nest their layers); layers stay in one flat list
    /// here and point at their group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<Id>,
    /// The V toggle off: the layer's video isn't drawn (audio plays). Not REST.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub video_off: bool,
    /// The A toggle off: the layer's audio is muted. Not REST.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub audio_off: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerGroup {
    pub id: Id,
    pub name: StringParam,
    pub colorid: ChoiceParam,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<BoolParam>,
    pub bypassed: BoolParam,
    pub solo: BoolParam,
    pub crossfadergroup: ChoiceParam,
    /// Group master (video and audio). Files saved 2026-11-25 while it was
    /// briefly removed lack it: 100 %, id assigned by [`Composition::ensure_master_ids`].
    #[serde(default = "crate::defaults::missing_master")]
    pub master: RangeParam,
    /// Playback speed of the group's clips (group Speed, saved in `.avc`;
    /// not in our REST capture). Created on demand; `None` = 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<RangeParam>,
    pub ignorecolumntrigger: BoolParam,
    #[serde(default)]
    pub dashboard: ParameterCollection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioTrack>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoTrack>,
    /// REST shape only; membership is [`Layer::group`]. Kept empty.
    #[serde(default)]
    pub layers: Vec<Layer>,
    /// Folded in the grid (its layers' rows hidden). Not in the REST schema.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub folded: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerTransition {
    pub duration: RangeParam,
    pub blend_mode: ChoiceParam,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutoPilot {
    pub target: ChoiceParam,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrossFader {
    pub id: Id,
    pub phase: RangeParam,
    pub behaviour: ChoiceParam,
    pub curve: ChoiceParam,
    pub sidea: EventParam,
    pub sideb: EventParam,
    #[serde(default)]
    pub mixer: ParameterCollection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TempoController {
    pub tempo: RangeParam,
    pub tempo_pull: EventParam,
    pub tempo_push: EventParam,
    pub tempo_tap: EventParam,
    pub resync: EventParam,
}

// ----------------------------------------------------------------------- clip

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: Id,
    pub name: StringParam,
    pub colorid: ChoiceParam,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<BoolParam>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected: Option<ChoiceParam>,
    pub target: ChoiceParam,
    pub triggerstyle: ChoiceParam,
    pub ignorecolumntrigger: ChoiceParam,
    pub faderstart: ChoiceParam,
    pub beatsnap: ChoiceParam,
    pub transporttype: ChoiceParam,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    #[serde(default)]
    pub dashboard: ParameterCollection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioTrackClip>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoTrackClip>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<Thumbnail>,
    /// Autopilot action / loops (not in the REST schema).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autopilot: Option<crate::autopilot::ClipAutoPilot>,
}

impl Clip {
    /// A slot with no media loaded.
    pub fn is_empty(&self) -> bool {
        self.video.is_none() && self.audio.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thumbnail {
    pub size: i64,
    pub last_update: String,
    pub is_default: bool,
}

/// Clip transport. `BpmSync` is listed first because it is a superset of
/// `Timeline` (untagged deserialisation picks the first match).
/// Not boxed: clips are few and boxing complicates serde/pattern code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum Transport {
    BpmSync(TransportBpmSync),
    Timeline(TransportTimeline),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransportTimeline {
    pub position: RangeParam,
    pub controls: TimelineControls,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineControls {
    pub playdirection: ChoiceParam,
    pub playmode: ChoiceParam,
    pub playmodeaway: ChoiceParam,
    pub duration: RangeParam,
    pub speed: RangeParam,
    /// Not in the REST schema: timecode in seconds at
    /// which the clip starts, used with transport type "SMPTE 1" / "SMPTE 2".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smpteoffset: Option<RangeParam>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransportBpmSync {
    pub position: RangeParam,
    pub controls: BpmSyncControls,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BpmSyncControls {
    pub playdirection: ChoiceParam,
    pub playmode: ChoiceParam,
    pub playmodeaway: ChoiceParam,
    pub duration: RangeParam,
    pub speed: RangeParam,
    /// Clip tempo; only with sync mode "BPM".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<RangeParam>,
    pub syncmode: ChoiceParam,
    pub beatloop: ChoiceParam,
}

// --------------------------------------------------------------------- tracks

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoTrack {
    pub width: RangeParam,
    pub height: RangeParam,
    pub opacity: RangeParam,
    #[serde(default)]
    pub mixer: ParameterCollection,
    #[serde(default)]
    pub effects: Vec<VideoEffect>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoTrackLayer {
    #[serde(flatten)]
    pub track: VideoTrack,
    pub autosize: ChoiceParam,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoTrackClip {
    #[serde(flatten)]
    pub track: VideoTrack,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fileinfo: Option<VideoFileInfo>,
    pub resize: ChoiceParam,
    pub r: BoolParam,
    pub g: BoolParam,
    pub b: BoolParam,
    pub a: BoolParam,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sourceparams: Option<ParameterCollection>,
    /// Not a file: a source (generator) or an effect clip. Not in the REST shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<crate::sources::ClipSource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioTrack {
    pub volume: RangeParam,
    pub pan: RangeParam,
    #[serde(default)]
    pub effects: Vec<AudioEffect>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioTrackClip {
    #[serde(flatten)]
    pub track: AudioTrack,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fileinfo: Option<AudioFileInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoFileInfo {
    pub path: String,
    pub exists: bool,
    pub duration: String,
    pub duration_ms: f64,
    pub framerate: FrameRate,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioFileInfo {
    pub path: String,
    pub exists: bool,
    pub duration: String,
    pub duration_ms: f64,
    pub sample_rate: f64,
    pub num_channels: i32,
    pub bpm: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRate {
    pub num: i32,
    pub denom: i32,
}

// -------------------------------------------------------------------- effects

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoEffect {
    pub id: Id,
    pub name: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bypassed: Option<BoolParam>,
    #[serde(default)]
    pub mixer: ParameterCollection,
    #[serde(default)]
    pub params: ParameterCollection,
    #[serde(default)]
    pub effect: ParameterCollection,
    /// Folded in the effects panel (saved with the project; not an edit for undo).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub collapsed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioEffect {
    pub id: Id,
    pub name: String,
    pub bypassed: BoolParam,
    #[serde(default)]
    pub params: ParameterCollection,
}

/// Entry in the effect/source catalogue (`GET /effects`, `GET /sources`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub idstring: String,
    pub name: String,
    #[serde(default)]
    pub presets: Vec<Preset>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub id: Id,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProductInfo {
    pub name: String,
    pub major: i64,
    pub minor: i64,
    pub micro: i64,
    pub revision: i64,
}
