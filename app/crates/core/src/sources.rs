//! Clips without a file: **sources** (generators: Gradient, Lines, Text Block…) and
//! **effect clips** (an effect dropped on an empty slot, applied to the layers below).
//!
//! Source params live in `video.sourceparams` (REST shape,
//! `data/lavmfx-source-params.json`); the clip's own effects follow as usual. The
//! renderer draws a source as a shader kind on a transparent input (see `effects.wgsl`:
//! generator kinds start at [`GENERATOR_KIND_BASE`], some reuse effect kinds).

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::effects::{SHADER_PARAMS, ShaderFx, ensure_transform, pack_params, shader_kind};
use crate::id::{Id, IdGen};
use crate::model::{Transport, TransportTimeline};
use crate::param::{Param, ParameterCollection};
use crate::{Clip, VideoEffect, VideoTrackClip};

/// What a clip without a file is. Stored beside the REST fields (not in the REST shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipSource {
    /// A generator (source name, e.g. "Gradient").
    Generator(String),
    /// Effect clip: the chain runs on everything below the layer.
    Effect,
    /// NDI source on the network (its NDI name, "MACHINE (Name)"): live frames.
    Ndi(String),
    /// Video another app shares on this machine: Spout sender (Windows), Syphon
    /// server (macOS), PipeWire node name (Linux). Imported Spout clips become this.
    Share(String),
    /// Capture device opened directly (webcam / capture card): its device id
    /// (`/dev/video0`, `video=Name`, …).
    Capture(String),
}

/// Length of a source / effect clip's timeline (5 s).
pub const VIRTUAL_SECONDS: f64 = 5.0;

/// Shader kind of the first own generator (effect kinds stay below).
pub const GENERATOR_KIND_BASE: u32 = 128;

/// One catalogue entry.
#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub name: String,
    /// Id (`idstring` in REST, `uniqueTypeId` in `.avc`).
    pub idstring: String,
    params: serde_json::Map<String, serde_json::Value>,
}

/// The video sources (capture devices left out), in browser order.
pub fn catalog() -> &'static [SourceSpec] {
    static CATALOG: OnceLock<Vec<SourceSpec>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let raw: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../data/lavmfx-source-params.json")).expect("source catalogue");
        raw.into_iter()
            .filter_map(|e| {
                Some(SourceSpec {
                    name: e.get("name")?.as_str()?.to_owned(),
                    idstring: e.get("idstring")?.as_str()?.to_owned(),
                    params: e.get("params")?.as_object()?.clone(),
                })
            })
            .collect()
    })
}

/// Catalogue entry by name or id.
pub fn spec(name: &str) -> Option<&'static SourceSpec> {
    catalog().iter().find(|s| s.name == name || s.idstring == name)
}

impl SourceSpec {
    /// Default params with fresh ids.
    pub fn instantiate(&self, ids: &mut IdGen) -> ParameterCollection {
        let mut out = ParameterCollection::new();
        for (k, v) in &self.params {
            let mut v = v.clone();
            if let Some(o) = v.as_object_mut() {
                o.insert("id".into(), ids.alloc().into());
            }
            if let Ok(mut p) = serde_json::from_value::<Param>(v) {
                if let Param::Range(r) = &mut p {
                    r.default = Some(r.value);
                }
                out.insert(k.clone(), p);
            }
        }
        if self.name == SLICE_OUTLINE {
            out.insert(
                OUTLINE_FREE_FX.into(),
                Param::Bool(crate::param::BoolParam::new(ids.alloc(), false)),
            );
        }
        if self.name == ROUTER {
            // Our own param (an input picker has no REST value).
            out.insert(
                ROUTER_INPUT.into(),
                Param::Choice(crate::defaults::choice(ids, &["Composition"], "Composition")),
            );
        }
        out
    }

    /// The renderer draws it.
    pub fn is_rendered(&self) -> bool {
        source_shader(&self.name).is_some() || routed_source(&self.name)
    }
}

pub const ROUTER: &str = "Video Router";
/// Source-browser name of an NDI source: `NDI/<NDI name>` (see [`Clip::load_ndi`]).
pub const NDI_PREFIX: &str = "NDI/";
/// Source-browser name of a shared video source: `SHARE/<name>` or
/// `SHARE/<name>\u{1f}<label>` (see [`Clip::load_share`]).
pub const SHARE_PREFIX: &str = "SHARE/";
/// Source-browser name of a capture device: `CAPTURE/<device id>\u{1f}<label>`.
pub const CAPTURE_PREFIX: &str = "CAPTURE/";
pub const FEEDBACK: &str = "Feedback";
/// Drawn by the outputs on each slice, nothing in the composition.
pub const SLICE_OUTLINE: &str = "Slice Outline";
/// Video Router's input choice: "Composition", "Layer N" or "Group N".
pub const ROUTER_INPUT: &str = "Input";
/// Slice Outline: our own switch. Off (default, also for clips
/// saved without it) = the layer's effects show only on the lines; on = they
/// may draw anywhere in each slice's box (moving / warping the lines).
pub const OUTLINE_FREE_FX: &str = "Effects Outside Lines";

/// Sources that show another result instead of a shader (Video Router, Feedback).
pub fn routed_source(name: &str) -> bool {
    name == ROUTER || name == FEEDBACK
}

/// What a Video Router / Feedback clip (or an output slice) shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RouteTarget {
    #[default]
    Composition,
    /// Layer index (0 = bottom).
    Layer(usize),
    /// Group index.
    Group(usize),
}

impl RouteTarget {
    /// From the router's Input choice text.
    pub fn parse(s: &str) -> Self {
        let num = |p: &str| {
            s.strip_prefix(p)
                .and_then(|n| n.trim().parse::<usize>().ok())
                .filter(|n| *n >= 1)
        };
        if let Some(n) = num("Layer") {
            RouteTarget::Layer(n - 1)
        } else if let Some(n) = num("Group") {
            RouteTarget::Group(n - 1)
        } else {
            RouteTarget::Composition
        }
    }

    pub fn label(self) -> String {
        match self {
            RouteTarget::Composition => "Composition".into(),
            RouteTarget::Layer(i) => format!("Layer {}", i + 1),
            RouteTarget::Group(i) => format!("Group {}", i + 1),
        }
    }
}

/// Router input options for a composition with `layers` layers and `groups` groups.
pub fn router_options(layers: usize, groups: usize) -> Vec<String> {
    std::iter::once(RouteTarget::Composition)
        .chain((0..layers).map(RouteTarget::Layer))
        .chain((0..groups).map(RouteTarget::Group))
        .map(RouteTarget::label)
        .collect()
}

/// The route a source clip reads (None: not a routed source).
pub fn clip_route(name: &str, params: Option<&ParameterCollection>) -> Option<RouteTarget> {
    match name {
        FEEDBACK => Some(RouteTarget::Composition),
        ROUTER => Some(
            params
                .and_then(|p| p.get(ROUTER_INPUT))
                .and_then(|p| {
                    if let Param::Choice(c) = p {
                        Some(RouteTarget::parse(&c.value))
                    } else {
                        None
                    }
                })
                .unwrap_or(RouteTarget::Composition),
        ),
        _ => None,
    }
}

/// Source → (shader: an effect's internal name or an own generator, params in slot order).
/// Own generators are numbered from [`GENERATOR_KIND_BASE`] in [`GENERATORS`] order.
const SOURCE_SHADERS: &[(&str, &str, &[&str])] = &[
    (
        "Abstract Field",
        "Gen.AbstractField",
        &["Octaves", "Amplitude", "Density"],
    ),
    ("Blank", "Gen.Blank", &["Black BG"]),
    ("Checkered", "Gen.Checkered", &["Num X", "Num Y", "Color 1", "Color 2"]),
    (
        "Gradient",
        "Gen.Gradient",
        &["Method", "Color 1", "Color 2", "Mode", "Dither", "Type"],
    ),
    (
        "Line Scape",
        "Gen.LineScape",
        &[
            "Tilt",
            "Direction",
            "Position Y",
            "Speed",
            "Height",
            "Jagginess",
            "Wobble",
            "Attenuation",
            "Line Width",
            "Fuzzyness",
            "Color",
            "Show Knots",
        ],
    ),
    (
        "Lines",
        "Gen.Lines",
        &["Fuzzyness", "Amount", "Width", "Rotation", "Position", "Color"],
    ),
    (
        "Metaballs",
        "Gen.Metaballs",
        &[
            "Grid",
            "Blend",
            "Size",
            "Distance",
            "Radius",
            "Fuzzyness",
            "Speed",
            "Position X",
            "Position Y",
            "Color",
            "BG Color",
        ],
    ),
    (
        "Rings",
        "Gen.Rings",
        &[
            "Num Rings",
            "Size",
            "Spacing",
            "Line Width",
            "Style",
            "Gap",
            "Rotation",
            "Rotation Step",
            "Rotation Alternate",
            "Color 1",
            "Color 2",
        ],
    ),
    (
        "Shaper",
        "Gen.Shaper",
        &[
            "Scale",
            "Noise Type",
            "Noise Amount",
            "Noise Speed",
            "Color",
            "Outline",
            "Outline Color",
        ],
    ),
    (
        "Sine Wave",
        "Gen.SineWave",
        &[
            "Waves",
            "Mode",
            "Line Width",
            "Fuzzyness",
            "Direction",
            "Mirror",
            "Frequency",
            "Main Speed",
            "Modulator",
            "Modulation Depth",
            "Modulator Speed",
            "Amplitude",
            "Position",
            "Color",
            "BG Color",
        ],
    ),
    ("Solid Color", "SolidColorEffect", &["Color"]),
    (
        "Spiral",
        "Gen.Spiral",
        &[
            "Direction",
            "Control Mode",
            "Spread Even",
            "Detail",
            "Speed",
            "Zoom",
            "Leafs",
            "Distortion",
            "Fuzzyness",
            "Ramification",
            "Button",
            "Button Size",
            "Button Fuzzyness",
            "Color",
            "BG Color",
        ],
    ),
    ("Static", "StaticEffect", &["Mode"]),
    (
        "Stroboscope",
        "Gen.Stroboscope",
        &["Frequency", "Fade", "Color 1", "Color 2"],
    ),
    (
        "Text Animator",
        "TextEffect",
        &["Color", "Outline Color", "Outline Width", "Glow In", "Glow Out"],
    ),
    (
        "Text Block",
        "TextBlock",
        &["Color", "Outline Color", "Outline Width", "Glow In", "Glow Out"],
    ),
    (
        "Tunnelines",
        "Gen.Tunnelines",
        &[
            "Color 1",
            "Color 2",
            "Line Width",
            "Zoom",
            "Vertical",
            "Frequency",
            "Horizontal",
        ],
    ),
    (
        "Slice Outline",
        "Gen.SliceOutline",
        &["Color", "Thickness", OUTLINE_FREE_FX],
    ),
];

/// Own generator shaders, in kind order (kind = base + index).
pub const GENERATORS: &[&str] = &[
    "Gen.AbstractField",
    "Gen.Blank",
    "Gen.Checkered",
    "Gen.Gradient",
    "Gen.LineScape",
    "Gen.Lines",
    "Gen.Metaballs",
    "Gen.Rings",
    "Gen.Shaper",
    "Gen.SineWave",
    "Gen.Spiral",
    "Gen.Stroboscope",
    "Gen.Tunnelines",
    "Gen.SliceOutline",
];

/// (shader kind, param names) of a source.
fn source_shader(name: &str) -> Option<(u32, &'static [&'static str])> {
    let (_, shader, params) = SOURCE_SHADERS.iter().find(|(n, ..)| *n == name)?;
    let kind = match GENERATORS.iter().position(|g| g == shader) {
        Some(i) => GENERATOR_KIND_BASE + i as u32,
        None => shader_kind(shader)?,
    };
    Some((kind, params))
}

/// Effect name a source reuses for its CPU side (text rasterising), if any.
pub fn source_effect_name(name: &str) -> Option<&'static str> {
    match name {
        "Text Block" => Some("TextBlock"),
        "Text Animator" => Some("TextEffect"),
        _ => None,
    }
}

/// The generator as the renderer draws it (first in the clip's chain, on a clear input).
pub fn source_fx(name: &str, params: &ParameterCollection, id: Id) -> Option<ShaderFx> {
    if crate::isf::is_isf(name) {
        return crate::isf::source_fx(name, params, id);
    }
    if crate::ffgl::is_ffgl(name) {
        return crate::ffgl::source_fx(name, params, id);
    }
    let (kind, names) = source_shader(name)?;
    let fallback = spec(name).map(|s| &s.params);
    let p: [f32; SHADER_PARAMS] = pack_params(params, names, fallback);
    Some(ShaderFx {
        kind,
        params: p,
        opacity: 1.0,
        blend: 0,
        id,
        feedback: false,
        passes: 1,
        history: 0,
    })
}

impl VideoTrackClip {
    /// A video track without a file (source or effect clip).
    pub fn without_file(ids: &mut IdGen, source: ClipSource, sourceparams: Option<ParameterCollection>) -> Self {
        let mut t = Self {
            track: crate::VideoTrack::new(ids),
            description: None,
            fileinfo: None,
            resize: crate::defaults::choice(ids, crate::defaults::CLIP_RESIZE, "Fit"),
            r: crate::param::BoolParam::new(ids.alloc(), true),
            g: crate::param::BoolParam::new(ids.alloc(), true),
            b: crate::param::BoolParam::new(ids.alloc(), true),
            a: crate::param::BoolParam::new(ids.alloc(), true),
            sourceparams,
            source: Some(source),
        };
        ensure_transform(&mut t.track.effects, ids);
        t
    }
}

impl Clip {
    fn load_virtual(&mut self, ids: &mut IdGen, name: &str, video: VideoTrackClip) {
        self.name.value = name.to_owned();
        self.transporttype.select(0);
        self.transport = Some(Transport::Timeline(TransportTimeline::new(ids, VIRTUAL_SECONDS)));
        self.video = Some(video);
        self.audio = None;
    }

    /// Put a source (generator) into this slot; false if the name is unknown.
    pub fn load_source(&mut self, ids: &mut IdGen, name: &str) -> bool {
        if let Some(ndi) = name.strip_prefix(NDI_PREFIX) {
            self.load_ndi(ids, ndi);
            return true;
        }
        if let Some(c) = name.strip_prefix(CAPTURE_PREFIX) {
            let (dev, label) = c.split_once('\u{1f}').unwrap_or((c, c));
            let v = VideoTrackClip::without_file(ids, ClipSource::Capture(dev.to_owned()), None);
            self.load_virtual(ids, label, v);
            return true;
        }
        if let Some(sh) = name.strip_prefix(SHARE_PREFIX) {
            // `<source>\u{1f}<label>` or just `<source>`.
            let (src, label) = sh.split_once('\u{1f}').unwrap_or((sh, sh));
            self.load_share(ids, src, label);
            return true;
        }
        if let Some((_, p)) = crate::ffgl::get(name).filter(|(_, p)| p.kind == crate::ffgl::FfglKind::Source) {
            let params = crate::ffgl::instantiate_params(&p, ids);
            let v = VideoTrackClip::without_file(ids, ClipSource::Generator(p.name()), Some(params));
            self.load_virtual(ids, &p.title, v);
            return true;
        }
        if let Some((_, s)) = crate::isf::get(name).filter(|(_, s)| s.kind == crate::isf::IsfKind::Source) {
            let params = crate::isf::instantiate_params(&s, ids);
            let v = VideoTrackClip::without_file(ids, ClipSource::Generator(s.name()), Some(params));
            self.load_virtual(ids, &s.title, v);
            return true;
        }
        let Some(s) = spec(name) else { return false };
        let params = s.instantiate(ids);
        let v = VideoTrackClip::without_file(ids, ClipSource::Generator(s.name.clone()), Some(params));
        self.load_virtual(ids, &s.name, v);
        true
    }

    /// Put an NDI source (by NDI name) into this slot.
    pub fn load_ndi(&mut self, ids: &mut IdGen, ndi_name: &str) {
        let v = VideoTrackClip::without_file(ids, ClipSource::Ndi(ndi_name.to_owned()), None);
        self.load_virtual(ids, ndi_name, v);
    }

    /// Put a shared video source (Spout / Syphon / PipeWire name) into this slot,
    /// named `label`.
    pub fn load_share(&mut self, ids: &mut IdGen, source: &str, label: &str) {
        let v = VideoTrackClip::without_file(ids, ClipSource::Share(source.to_owned()), None);
        self.load_virtual(ids, label, v);
    }

    /// Make this slot an effect clip with `effect` (internal or display name).
    pub fn load_effect_clip(&mut self, ids: &mut IdGen, effect: &str) -> bool {
        let Some(fx) = VideoEffect::from_catalog(ids, effect) else {
            return false;
        };
        let name = fx.display_name.clone();
        self.make_effect_clip(ids, &name);
        if let Some(v) = self.video.as_mut() {
            v.track.effects.push(fx);
        }
        true
    }

    /// An effect clip named `name` with only its Transform (effects get moved in after).
    pub fn make_effect_clip(&mut self, ids: &mut IdGen, name: &str) {
        let v = VideoTrackClip::without_file(ids, ClipSource::Effect, None);
        self.load_virtual(ids, name, v);
    }

    /// Source / effect clip marker (None: a file clip or empty).
    pub fn source(&self) -> Option<&ClipSource> {
        self.video.as_ref()?.source.as_ref()
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn share_clips_take_their_label() {
        let mut ids = IdGen::new();
        let mut clip = Clip::empty(&mut ids);
        assert!(clip.load_source(&mut ids, "SHARE/v4l2_input.usb-0\u{1f}HD Webcam"));
        assert_eq!(clip.source(), Some(&ClipSource::Share("v4l2_input.usb-0".into())));
        assert_eq!(clip.name.value, "HD Webcam");
    }

    #[test]
    fn ndi_clips_load_and_save() {
        let mut ids = IdGen::new();
        let mut clip = Clip::empty(&mut ids);
        assert!(clip.load_source(&mut ids, "NDI/STUDIO-PC (TouchDesigner)"));
        assert_eq!(
            clip.source(),
            Some(&ClipSource::Ndi("STUDIO-PC (TouchDesigner)".into()))
        );
        assert_eq!(clip.name.value, "STUDIO-PC (TouchDesigner)");
        let back: Clip = serde_json::from_str(&serde_json::to_string(&clip).unwrap()).unwrap();
        assert_eq!(back.source(), clip.source());
    }

    use super::*;

    #[test]
    fn catalogue_and_shaders() {
        assert_eq!(catalog().len(), 20);
        for (name, _, params) in SOURCE_SHADERS {
            let s = spec(name).unwrap_or_else(|| panic!("{name} not in catalogue"));
            // Catalogue params, or ones we add when the clip is made (Slice Outline's switch).
            let made = s.instantiate(&mut IdGen::new());
            for p in *params {
                assert!(
                    s.params.contains_key(*p) || made.contains_key(*p),
                    "{name}: no param {p}"
                );
            }
            assert!(source_shader(name).is_some(), "{name}: no shader");
        }
        // Routed sources are drawn without a shader.
        for n in ["Feedback", "Video Router", "Slice Outline"] {
            assert!(spec(n).unwrap().is_rendered(), "{n}");
        }
        let mut ids = IdGen::new();
        let p = spec("Video Router").unwrap().instantiate(&mut ids);
        assert_eq!(clip_route(ROUTER, Some(&p)), Some(RouteTarget::Composition));
        assert_eq!(RouteTarget::parse("Layer 3"), RouteTarget::Layer(2));
        assert_eq!(RouteTarget::parse("Group 1"), RouteTarget::Group(0));
        assert_eq!(router_options(2, 1), ["Composition", "Layer 1", "Layer 2", "Group 1"]);
        assert_eq!(clip_route(FEEDBACK, None), Some(RouteTarget::Composition));
        // Metaballs' id (`.avc` uniqueTypeId) finds it.
        assert_eq!(spec("844752396506092017").unwrap().name, "Metaballs");
    }

    #[test]
    fn source_and_effect_clips() {
        let mut ids = IdGen::new();
        let mut c = Clip::empty(&mut ids);
        assert!(c.load_source(&mut ids, "Gradient"));
        assert!(!c.is_empty() && c.video_path().is_none());
        assert_eq!(c.source(), Some(&ClipSource::Generator("Gradient".into())));
        let v = c.video.as_ref().unwrap();
        assert!(v.sourceparams.as_ref().unwrap().contains_key("Color 1"));
        let f = source_fx("Gradient", v.sourceparams.as_ref().unwrap(), c.id).unwrap();
        assert_eq!(f.kind, GENERATOR_KIND_BASE + 3);
        assert_eq!(&f.params[1..5], &[1.0, 0.0, 0.0, 1.0]); // Color 1 red
        // Serialises and loads back.
        let back: Clip = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);

        let mut e = Clip::empty(&mut ids);
        assert!(e.load_effect_clip(&mut ids, "Blur"));
        assert_eq!(e.source(), Some(&ClipSource::Effect));
        assert_eq!(e.name.value, "Blur");
        let chain = &e.video.as_ref().unwrap().track.effects;
        assert!(chain[0].is_transform() && chain[1].name == "Blur");
        assert!(!e.load_effect_clip(&mut ids, "No Such Effect"));
    }
}
