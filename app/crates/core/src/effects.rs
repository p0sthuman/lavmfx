//! Video effects: the catalogue, effect instances, chains and the mapping
//! to the renderer's shader effects.
//!
//! The catalogue (`data/lavmfx-effect-params.json`) lists, in REST shape,
//! the exact parameter names, order, ranges, defaults and display hints.
//! Every effect carries a dry/wet `Opacity` param and a mixer `Blend Mode`.

use std::sync::OnceLock;

use crate::defaults::{MIX_MODES, choice};
use crate::id::{Id, IdGen};
use crate::mix::shader_blend;
use crate::model::{Composition, VideoEffect, VideoTrack};
use crate::param::{BoolParam, Param, ParameterCollection};

/// Internal name of the built-in Transform effect (first in clip and
/// layer chains; can't be removed).
pub const TRANSFORM: &str = "Transform";
/// Dry/wet amount of every effect.
pub const OPACITY_KEY: &str = "Opacity";
/// Mixer param of every effect (dry/wet blend).
pub const BLEND_KEY: &str = "Blend Mode";
/// Floats passed to an effect shader.
pub const SHADER_PARAMS: usize = 32;

/// One catalogue entry.
#[derive(Debug, Clone)]
pub struct EffectSpec {
    /// Internal name (`VideoEffect.name`, e.g. "BrightnessContrast").
    pub name: String,
    /// Name in the effect browser (e.g. "Bright.Contrast").
    pub display_name: String,
    /// Id (`idstring` in REST, `uniqueTypeId` in `.avc`).
    pub idstring: String,
    pub(crate) params: serde_json::Map<String, serde_json::Value>,
}

/// The video effects, in browser order (alphabetical by display name).
pub fn catalog() -> &'static [EffectSpec] {
    static CATALOG: OnceLock<Vec<EffectSpec>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut raw: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../data/lavmfx-effect-params.json")).expect("effect catalogue");
        // Effects with their own catalogue file: Slice Chase.
        let own: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../data/lavmfx-slice-chase.json")).expect("slice chase catalogue");
        raw.extend(own);
        let mut v: Vec<EffectSpec> = raw
            .into_iter()
            .filter_map(|e| {
                Some(EffectSpec {
                    name: e.get("name")?.as_str()?.to_owned(),
                    display_name: e.get("display_name")?.as_str()?.to_owned(),
                    idstring: e
                        .get("idstring")
                        .and_then(|i| i.as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    params: e.get("params")?.as_object()?.clone(),
                })
            })
            .collect();
        v.sort_by_key(|e| e.display_name.to_lowercase());
        v
    })
}

/// Catalogue entry by internal or display name.
pub fn spec(name: &str) -> Option<&'static EffectSpec> {
    catalog().iter().find(|e| e.name == name || e.display_name == name)
}

/// Catalogue entry by id (`.avc` `uniqueTypeId`, e.g. "A108" = Mirror).
pub fn spec_by_id(idstring: &str) -> Option<&'static EffectSpec> {
    catalog()
        .iter()
        .find(|e| !idstring.is_empty() && e.idstring == idstring)
}

impl EffectSpec {
    /// A new instance with fresh ids and default values.
    pub fn instantiate(&self, ids: &mut IdGen) -> VideoEffect {
        let mut params = ParameterCollection::new();
        for (k, v) in &self.params {
            let mut v = v.clone();
            if let Some(o) = v.as_object_mut() {
                o.insert("id".into(), ids.alloc().into());
            }
            if let Ok(mut p) = serde_json::from_value::<Param>(v) {
                if let Param::Range(r) = &mut p {
                    r.default = Some(r.value);
                }
                params.insert(k.clone(), p);
            }
        }
        if self.name == "Mask" && !params.contains_key(MASK_FILE_KEY) {
            params.insert(
                MASK_FILE_KEY.into(),
                Param::Text(crate::param::TextParam {
                    id: ids.alloc(),
                    valuetype: "ParamText".into(),
                    value: String::new(),
                    view: None,
                }),
            );
        }
        let mut mixer = ParameterCollection::new();
        mixer.insert(BLEND_KEY.into(), Param::Choice(choice(ids, MIX_MODES, "Alpha")));
        VideoEffect {
            id: ids.alloc(),
            name: self.name.clone(),
            display_name: self.display_name.clone(),
            bypassed: (self.name != TRANSFORM).then(|| BoolParam::new(ids.alloc(), false)),
            mixer,
            params,
            effect: ParameterCollection::new(),
        }
    }
}

impl VideoEffect {
    /// New effect from the catalogue (internal or display name).
    pub fn from_catalog(ids: &mut IdGen, name: &str) -> Option<Self> {
        if crate::ffgl::is_ffgl(name) {
            let (_, p) = crate::ffgl::get(name)?;
            return (p.kind == crate::ffgl::FfglKind::Effect).then(|| crate::ffgl::instantiate_effect(&p, ids));
        }
        if crate::isf::is_isf(name) {
            let (_, s) = crate::isf::get(name)?;
            return (s.kind == crate::isf::IsfKind::Effect).then(|| crate::isf::instantiate_effect(&s, ids));
        }
        spec(name).map(|s| s.instantiate(ids))
    }
    pub fn is_transform(&self) -> bool {
        self.name == TRANSFORM
    }
    pub fn is_bypassed(&self) -> bool {
        self.bypassed.as_ref().is_some_and(|b| b.value)
    }
    /// Dry/wet amount, 0..1.
    pub fn opacity(&self) -> f64 {
        match self.params.get(OPACITY_KEY) {
            Some(Param::Range(r)) => r.value,
            _ => 1.0,
        }
    }
    /// Shader blend index of the dry/wet mix (Alpha when unknown).
    pub fn blend_index(&self) -> u32 {
        match self.mixer.get(BLEND_KEY) {
            Some(Param::Choice(c)) => shader_blend(&c.value).unwrap_or(0),
            _ => 0,
        }
    }
    /// The renderer implements this effect.
    pub fn is_rendered(&self) -> bool {
        shader_kind(&self.name).is_some()
            || crate::isf::get(&self.name).is_some_and(|(_, s)| s.runs())
            || crate::ffgl::get(&self.name).is_some_and(|(_, p)| p.runs())
    }
}

/// A new chain with Transform first.
pub fn ensure_transform(chain: &mut Vec<VideoEffect>, ids: &mut IdGen) {
    if !chain.iter().any(VideoEffect::is_transform)
        && let Some(t) = VideoEffect::from_catalog(ids, TRANSFORM)
    {
        chain.insert(0, t);
    }
}

// ------------------------------------------------------------- presets

/// A factory effect preset shipped with the app
/// (`data/factory-presets.json`, effect name → preset name → values).
#[derive(Debug, Clone)]
pub struct BuiltinPreset {
    pub name: String,
    /// Param name → value (number, bool, colour string or choice option).
    pub values: serde_json::Map<String, serde_json::Value>,
    /// Mixer blend mode, if the preset sets one.
    pub blend: Option<String>,
    /// Animated params (timeline / BPM, range, direction, mode) as
    /// `drive::Drive` JSON.
    pub drives: serde_json::Map<String, serde_json::Value>,
}

/// Parse a presets file: effect name → preset name →
/// `{params, blend?, drives?}`. Presets come sorted by name.
fn parse_presets(json: &str) -> std::collections::HashMap<String, Vec<BuiltinPreset>> {
    let raw: serde_json::Map<String, serde_json::Value> = serde_json::from_str(json).expect("effect presets");
    raw.into_iter()
        .map(|(fx, presets)| {
            let mut v: Vec<BuiltinPreset> = presets
                .as_object()
                .into_iter()
                .flatten()
                .map(|(name, p)| BuiltinPreset {
                    name: name.clone(),
                    values: p.get("params").and_then(|v| v.as_object()).cloned().unwrap_or_default(),
                    blend: p.get("blend").and_then(|b| b.as_str()).map(str::to_owned),
                    drives: p.get("drives").and_then(|v| v.as_object()).cloned().unwrap_or_default(),
                })
                .collect();
            v.sort_by_key(|p| p.name.to_lowercase());
            (fx, v)
        })
        .collect()
}

/// The factory presets for an effect (internal name), sorted by name.
pub fn builtin_presets(name: &str) -> &'static [BuiltinPreset] {
    static ALL: OnceLock<std::collections::HashMap<String, Vec<BuiltinPreset>>> = OnceLock::new();
    let all = ALL.get_or_init(|| parse_presets(include_str!("../data/factory-presets.json")));
    all.get(name).map_or(&[], Vec::as_slice)
}

impl VideoEffect {
    /// Recall a factory preset: values, blend and animations.
    pub fn apply_builtin(&mut self, p: &BuiltinPreset) {
        self.apply_values(&p.values, p.blend.as_deref());
        for (k, v) in &p.drives {
            if let Some(Param::Range(r)) = self.params.get_mut(k)
                && let Ok(mut d) = serde_json::from_value::<crate::drive::Drive>(v.clone())
            {
                d.restart();
                r.drive = Some(Box::new(d));
            }
        }
    }

    /// Set params by name from plain values (preset JSON); their animations
    /// are cleared. Unknown names / mismatched types are skipped.
    pub fn apply_values(&mut self, values: &serde_json::Map<String, serde_json::Value>, blend: Option<&str>) {
        for (k, v) in values {
            match (self.params.get_mut(k), v) {
                (Some(Param::Range(r)), serde_json::Value::Number(n)) => {
                    r.drive = None;
                    r.set(n.as_f64().unwrap_or(r.value));
                }
                (Some(Param::Int(i)), serde_json::Value::Number(n)) => i.value = n.as_i64().unwrap_or(i.value),
                (Some(Param::Bool(b)), serde_json::Value::Bool(x)) => b.value = *x,
                (Some(Param::Choice(c)), serde_json::Value::String(x)) => {
                    c.select_value(x);
                }
                (Some(Param::Color(c)), serde_json::Value::String(x)) => c.value = x.clone(),
                (Some(Param::String(t)), serde_json::Value::String(x)) => t.value = x.clone(),
                (Some(Param::Text(t)), serde_json::Value::String(x)) => t.value = x.clone(),
                _ => {}
            }
        }
        if let (Some(b), Some(Param::Choice(c))) = (blend, self.mixer.get_mut(BLEND_KEY)) {
            c.select_value(b);
        }
    }

    /// The "Default" preset: every param back to the catalogue default.
    pub fn reset_defaults(&mut self) {
        let Some(s) = spec(&self.name) else { return };
        let values = s
            .params
            .iter()
            .filter_map(|(k, p)| Some((k.clone(), p.get("value")?.clone())))
            .collect();
        self.apply_values(&values, Some("Alpha"));
    }
}

// ------------------------------------------------------------------ chains

/// Owner of an effect chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FxTarget {
    Composition,
    Group(usize),
    Layer(usize),
    /// Clip of the active deck (layer, column).
    Clip(usize, usize),
}

impl Composition {
    /// Composition size in pixels (composition video track; 1920×1080 if unset).
    pub fn resolution(&self) -> (u32, u32) {
        self.video.as_ref().map_or((1920, 1080), |v| {
            (
                v.width.value.round().max(1.0) as u32,
                v.height.value.round().max(1.0) as u32,
            )
        })
    }

    pub fn set_resolution(&mut self, w: u32, h: u32) {
        let ids = &mut self.ids;
        let v = self.video.get_or_insert_with(|| VideoTrack::new(ids));
        v.width.set(w as f64);
        v.height.set(h as f64);
    }

    /// Effect chain of `t` (None when it has no video track).
    pub fn effects(&self, t: FxTarget) -> Option<&Vec<VideoEffect>> {
        match t {
            FxTarget::Composition => self.video.as_ref().map(|v| &v.effects),
            FxTarget::Group(g) => self.layergroups.get(g)?.video.as_ref().map(|v| &v.effects),
            FxTarget::Layer(l) => self.layers.get(l)?.video.as_ref().map(|v| &v.track.effects),
            FxTarget::Clip(l, c) => self
                .layers
                .get(l)?
                .clips
                .get(c)?
                .video
                .as_ref()
                .map(|v| &v.track.effects),
        }
    }

    /// Mutable chain; creates the composition's/group's video track if missing.
    /// Clip and layer chains get their Transform.
    pub fn effects_mut(&mut self, t: FxTarget) -> Option<&mut Vec<VideoEffect>> {
        let ids = &mut self.ids;
        match t {
            FxTarget::Composition => Some(&mut self.video.get_or_insert_with(|| VideoTrack::new(ids)).effects),
            FxTarget::Group(g) => {
                let g = self.layergroups.get_mut(g)?;
                Some(&mut g.video.get_or_insert_with(|| VideoTrack::new(ids)).effects)
            }
            FxTarget::Layer(l) => {
                let chain = &mut self.layers.get_mut(l)?.video.as_mut()?.track.effects;
                ensure_transform(chain, ids);
                Some(chain)
            }
            FxTarget::Clip(l, c) => {
                let chain = &mut self.layers.get_mut(l)?.clips.get_mut(c)?.video.as_mut()?.track.effects;
                ensure_transform(chain, ids);
                Some(chain)
            }
        }
    }

    /// Append (or insert at `at`) a catalogue effect; returns its id.
    pub fn add_effect(&mut self, t: FxTarget, name: &str, at: Option<usize>) -> Option<Id> {
        let mut fx = VideoEffect::from_catalog(&mut self.ids, name)?;
        // Pixel params whose defaults assume 1920×1080 follow the composition.
        let (w, h) = self.resolution();
        for (k, v) in [("Right", w), ("Bottom", h)] {
            if fx.name == "Crop"
                && let Some(Param::Range(r)) = fx.params.get_mut(k)
            {
                r.set(v as f64);
                r.default = Some(r.value);
            }
        }
        let id = fx.id;
        let chain = self.effects_mut(t)?;
        let at = at.unwrap_or(chain.len()).min(chain.len());
        chain.insert(at, fx);
        Some(id)
    }

    /// Remove the effect at `i` (Transform stays).
    pub fn remove_effect(&mut self, t: FxTarget, i: usize) -> bool {
        match self.effects_mut(t) {
            Some(chain) if chain.get(i).is_some_and(|e| !e.is_transform()) => {
                chain.remove(i);
                true
            }
            _ => false,
        }
    }

    /// Drag & drop of an effect: take effect `i` of `from` and insert it in
    /// `to` before index `at` (index in `to` as it is now, before the move).
    /// `copy` keeps the original and inserts a duplicate with fresh ids.
    /// Transform never moves and stays first. Returns the new index.
    pub fn transfer_effect(&mut self, from: FxTarget, i: usize, to: FxTarget, at: usize, copy: bool) -> Option<usize> {
        let src = self.effects(from)?.get(i)?;
        if src.is_transform() {
            return None;
        }
        let same = from == to;
        if same && !copy && (at == i || at == i + 1) {
            return Some(i); // dropped onto itself
        }
        let fx = if copy {
            let src = src.clone();
            crate::defaults::with_fresh_ids(&src, &mut self.ids)
        } else {
            self.effects(to)?; // target must exist before taking the effect out
            self.effects_mut(from)?.remove(i)
        };
        let at = if same && !copy && at > i { at - 1 } else { at };
        let chain = self.effects_mut(to)?;
        let first = chain.first().is_some_and(VideoEffect::is_transform) as usize;
        let at = at.clamp(first, chain.len());
        chain.insert(at, fx);
        Some(at)
    }

    /// Move effect `from` to position `to` (in the list without it).
    pub fn move_effect(&mut self, t: FxTarget, from: usize, to: usize) -> bool {
        let Some(chain) = self.effects_mut(t) else { return false };
        if from >= chain.len() {
            return false;
        }
        let fx = chain.remove(from);
        chain.insert(to.min(chain.len()), fx);
        true
    }
}

// ------------------------------------------------------------------ shaders

/// Effects the renderer implements: internal name and the params it reads,
/// in shader slot order (a colour takes 4 slots, a bool/choice one: 0/1 or index).
/// The index is the shader kind (`lavmfx_render::effects`).
pub const SHADER_EFFECTS: &[(&str, &[&str])] = &[
    (
        "Transform",
        &[
            "Position X",
            "Position Y",
            "Scale",
            "Scale W",
            "Scale H",
            "Rotation X",
            "Rotation Y",
            "Rotation Z",
            "Anchor X",
            "Anchor Y",
        ],
    ),
    (
        "Blur",
        &["Blur", "Blur X Distance", "Blur Y Distance", "Quality", "Gaussian"],
    ),
    ("BrightnessContrast", &["Brightness", "Contrast"]),
    ("HueRotate", &["Hue Scale", "Hue Rotate", "Sat. Scale"]),
    ("Saturation", &["Saturation"]),
    ("InvertRGB", &["R", "G", "B", "A"]),
    (
        "Colorize",
        &[
            "Color",
            "Mode",
            "Weighted",
            "GreysKeep",
            "GreysTolerance",
            "Invert",
            "Contrast",
        ],
    ),
    ("Tint", &["Black To", "White To"]),
    ("Threshold", &["Threshold", "Smoothness", "Color 1", "Color 2", "Alpha"]),
    ("Posterize", &["Posterize"]),
    ("Exposure", &["Exposure"]),
    ("Vignette", &["Roundness", "Size", "Softness", "Black BG"]),
    ("Mirror", &["X", "Y", "In / Out", "Flip X", "Flip Y", "Black BG"]),
    ("MirrorQuad", &["Flip X", "Flip Y"]),
    ("Flip", &["Horizontal", "Vertical", "Black BG"]),
    (
        "Kaleidoscope",
        &["Angles", "Rotation", "Zoom", "Input Rotation", "Black BG"],
    ),
    ("PolarKaleido", &["Rings", "Parts", "Angle"]),
    ("ShiftRGB", &["Distance", "Mode", "R", "G", "B", "Black BG"]),
    (
        "WaveWarp",
        &["Mode", "Height", "Width", "Angle", "Speed", "Pin edges?", "Black BG"],
    ),
    ("Strobe", &["Frequency", "Fade", "Color"]),
    ("Trails", &["Feedback", "Mode"]),
    ("FishEye", &["FishEye", "Black BG"]),
    (
        "TileEffect",
        &["Tile X", "Tile Y", "Skew X", "Skew Y", "Rotation Angle", "Black BG"],
    ),
    ("Crop", &["Left", "Right", "Top", "Bottom", "Invert", "Black BG"]),
    ("EdgeDetection", &["Mode", "Multiplier", "Use Color", "Color", "Amount"]),
    ("Sharpen", &["Strength", "Distance"]),
    ("SolidColorEffect", &["Color", "Show Image"]),
    ("AutoMask", &["Channel", "Contrast", "Invert"]),
    ("Vibrance", &["Vibrance"]),
    (
        "Displace",
        &[
            "X factor",
            "Y factor",
            "Smooth",
            "Threshold",
            "Invert",
            "Mode",
            "Black BG",
        ],
    ),
    (
        "TwitchEffect",
        &[
            "Global Twitch",
            "Max Pos X",
            "Max Pos Y",
            "Pos Frequency",
            "Max Scale",
            "Scale Frequency",
            "Max Opacity",
            "Opacity Frequency",
            "Max Light",
            "Light Frequency",
            "Max RGB Shift",
            "RGB Frequency",
            "Max Blur",
            "Blur Frequency",
            "Black BG",
        ],
    ),
    ("DropShadow", &["Distance", "Angle", "Color", "Spread", "Strength"]),
    // ---- M5b batch 1: single-pass colour / pattern (kinds 32–54)
    ("Levels", &["Base", "Midpoint", "Ratio"]),
    (
        "Recolour",
        &["Palette", "Floor", "Ceiling", "Cycle Phase", "Cycle Speed", "Use Alpha"],
    ),
    ("ColorPass", &["Hue 1", "Hue 2", "Invert"]),
    ("AddSubtract", &["R", "G", "B"]),
    (
        "ChromaKey",
        &[
            "Show Mask",
            "Key Hue",
            "Hue Threshold",
            "Hue Softness",
            "White Threshold",
            "White Softness",
            "Black Threshold",
            "Black Softness",
            "Spill Threshold",
            "Spill Softness",
            "Edge Shrink",
            "Edge Blur",
        ],
    ),
    ("Dither", &["Mode"]),
    ("DotScreen", &["Saturation", "Angle", "Scale"]),
    ("Hatched", &["Density", "Levels", "Invert", "Color1", "Color2"]),
    ("LoRezEffect", &["Pixel Size", "Bit Reduction", "Dither"]),
    (
        "PixelBlur",
        &["Blur", "Distance X", "Distance Y", "Interval", "Gaussian"],
    ),
    ("PixelHighPass", &["R", "G", "B"]),
    ("Polkadot", &["Dot Size", "Threshold"]),
    ("Raster", &["Modulo X", "Modulo Y", "Black BG"]),
    (
        "NoisyEffect",
        &[
            "Noise",
            "Noise X",
            "Noise Y",
            "Noise S",
            "Noise C",
            "Resolution",
            "Interpolate",
        ],
    ),
    ("StaticEffect", &["Mode"]),
    ("Snow", &["Amount", "Speed", "Textured", "View Y", "View X", "Black BG"]),
    (
        "Snowy",
        &[
            "Size",
            "Speed",
            "Amount",
            "Spread",
            "Threshold",
            "Show Image",
            "Smart Edges",
        ],
    ),
    (
        "Sparkles",
        &[
            "Texture",
            "Spawn Mode",
            "Size",
            "Color",
            "Speed",
            "Amount",
            "Spread",
            "Threshold",
            "Move",
            "Show Image",
        ],
    ),
    ("Grid", &["Width", "Size X", "Size Y", "Fuzzyness", "Color"]),
    (
        "PointGrid",
        &[
            "Size",
            "Smooth Start",
            "Black BG",
            "Space",
            "Resolution",
            "Rotate X",
            "Rotate Y",
            "Rotate Z",
        ],
    ),
    ("Circles", &["Size", "Black BG"]),
    (
        "Stripper",
        &[
            "Number",
            "Mode",
            "Direction",
            "Direction Spread",
            "MovementType",
            "Speed",
            "SpeedSpread",
            "GlobalWidth",
            "MinWidth",
            "MaxWidth",
            "Feather",
            "Red",
            "Green",
            "Blue",
            "Alpha",
        ],
    ),
    ("Heat", &["Heat"]),
    // ---- M5b batch 2: warp / UV (kinds 55–81)
    ("Bendoscope", &["Divisions", "Black BG"]),
    ("Distortion", &["Distort", "Radius"]),
    (
        "Goo",
        &[
            "Resolution",
            "Speed",
            "MaxDistortionX",
            "MaxDistortionY",
            "MaxDistortionZ",
            "Black BG",
        ],
    ),
    (
        "Ripples",
        &[
            "Frequency H",
            "Amplitude H",
            "Frequency V",
            "Amplitude V",
            "Amplitude Both",
            "Phase",
            "Black BG",
        ],
    ),
    (
        "Sphere",
        &[
            "Size",
            "Resolution",
            "Extrusion",
            "Invert",
            "NoisePhase",
            "NoiseType",
            "ColorValue",
            "ColorMode",
            "ColorContrast",
            "ThresholdValue",
            "ThresholdMode",
            "Zoom",
            "Rotation X",
            "Rotation Y",
            "Rotation Z",
            "Black BG",
        ],
    ),
    (
        "StingySphere",
        &[
            "Sphere size",
            "Extrusion",
            "Noise",
            "Noise Amount",
            "Light",
            "Light Flat",
            "Min Light",
            "Saturate",
            "Saturate Amount",
            "Wired",
            "Invert",
            "Zoom",
            "Resolution",
            "Rotate X",
            "Rotate Y",
            "Rotate Z",
            "Black BG",
        ],
    ),
    ("Twisted", &["Twirl", "Radius", "Black BG"]),
    (
        "Tunnel",
        &[
            "Position",
            "Speed",
            "Twist",
            "Kaleidoscope",
            "Rotation",
            "TurnH",
            "TurnV",
            "WobbleH",
            "WobbleV",
            "Black BG",
        ],
    ),
    ("Suckr", &["Suck", "Black BG"]),
    ("Blow", &["X", "Y", "Width", "Height"]),
    ("Slide", &["X", "Y", "Black BG"]),
    (
        "Fragment",
        &[
            "Fragments",
            "Distance",
            "Rotation",
            "FragRotationX",
            "FragRotationY",
            "FragRotationZ",
            "FragScale",
            "CompositeOpacity",
            "Black BG",
        ],
    ),
    ("Triangulate", &["Scale", "Levels", "Flip Direction", "Threshold"]),
    (
        "CubeTiles",
        &[
            "Resolution",
            "Zoom",
            "Amount",
            "Rotate X",
            "Rotate Y",
            "Rotate Z",
            "Num Cubes",
            "Black BG",
        ],
    ),
    (
        "VideoWall",
        &[
            "Screens",
            "Zoom",
            "RotateX",
            "RotateY",
            "RotateZ",
            "PanX",
            "PanY",
            "SubscreenSize",
            "SubscreenMode",
            "GapSize",
            "BrightnessVary",
            "Black BG",
        ],
    ),
    (
        "InfiniteZoom",
        &["Rotation Speed", "Zoom", "Range", "Alpha", "Num Slices"],
    ),
    (
        "WarpSpeed",
        &["Vignette", "Fade", "Scale", "Rotate", "Center X", "Center Y"],
    ),
    ("Radar", &["Angle", "Trail Length", "Fade", "Show Grid", "Grid Scale"]),
    ("SearchLight", &["Size", "Blur", "Frequency", "Invert"]),
    (
        "Reveal",
        &[
            "Phase", "X", "Y", "Border", "Texture", "Stretch", "Fade", "Scaling", "Order", "Seed", "Black BG",
        ],
    ),
    (
        "Keystone",
        &[
            "Top Left X",
            "Top Left Y",
            "Top Right X",
            "Top Right Y",
            "Bot. Right X",
            "Bot. Right Y",
            "Bot. Left X",
            "Bot. Left Y",
            "Show Grid",
            "Black BG",
        ],
    ),
    (
        "KeystoneCrop",
        &[
            "Top Left X",
            "Top Left Y",
            "Top Right X",
            "Top Right Y",
            "Bot. Right X",
            "Bot. Right Y",
            "Bot. Left X",
            "Bot. Left Y",
            "Show Grid",
            "Black BG",
        ],
    ),
    (
        "KeystoneMask",
        &[
            "Top Left X",
            "Top Left Y",
            "Top Right X",
            "Top Right Y",
            "Bot. Right X",
            "Bot. Right Y",
            "Bot. Left X",
            "Bot. Left Y",
            "Show Grid",
            "Black BG",
        ],
    ),
    (
        "ShiftGlitch",
        &["Frequency", "Vertical", "Horizontal", "Size", "Black BG"],
    ),
    (
        "LinearCloner",
        &[
            "Clones",
            "Tails",
            "Center",
            "Tail Length",
            "Start Scale",
            "Start Rotation",
            "End Scale",
            "End Rotation",
            "Distribution",
            "Distortion",
            "Reverse",
            "Tail Rotation",
            "Master Rotation",
            "Delay Enabled",
            "Delay Amount",
            "Delay Order",
            "Delay Fill",
        ],
    ),
    (
        "GridCloner",
        &[
            "Columns",
            "Rows",
            "Input Rotation",
            "Input Scale",
            "Width",
            "Height",
            "Scale",
            "Rotation",
            "Vertical Shift",
            "Horiztonal Shift",
            "Random Shift",
            "Order",
            "Background",
            "Delay",
        ],
    ),
    (
        "RadialCloner",
        &[
            "Clones",
            "Start Radius",
            "Start Scale",
            "Start Rotation",
            "End Radius",
            "End Scale",
            "End Rotation",
            "Fan",
            "Fan Offset",
            "Master Rotation",
            "Delay Enabled",
            "Delay Amount",
            "Delay Order",
            "Delay Fill",
        ],
    ),
    // ---- M5b batch 3: neighbourhood / multi-pass (kinds 82–91)
    (
        "RadialBlur",
        &["Steps", "Distance", "Rotation", "High Quality", "Black BG"],
    ),
    (
        "TiltShift",
        &["Blur", "Position", "Focus", "Falloff", "Saturation", "Vignette"],
    ),
    ("Dilate", &["Spread", "Size", "Threshold", "Mode", "Shape"]),
    ("Kuwahara", &["Radius"]),
    (
        "Acuarela",
        &["Gain", "Speed", "Detail", "Strength", "Density", "Gain X", "Gain Y"],
    ),
    (
        "Iterate",
        &[
            "TransformStrength",
            "Iterations",
            "StartOpacity",
            "OpacityChange",
            "TranslateX",
            "TranslateY",
            "TranslateZ",
            "RotateX",
            "RotateY",
            "RotateZ",
            "Scale",
            "DOpacityChange",
            "DTranslateX",
            "DTranslateY",
            "DTranslateZ",
            "DRotateX",
            "DRotateY",
            "DRotateZ",
            "DScale",
            "BlendMode",
            "GRotateX",
            "GRotateY",
            "GRotateZ",
            "Black BG",
        ],
    ),
    (
        "Shaper",
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
        "ShapeMask",
        &[
            "Shape",
            "Feather",
            "Scale",
            "Rotation",
            "Position X",
            "Position Y",
            "Invert",
        ],
    ),
    (
        "MetaShape",
        &[
            "Resolution",
            "Shape 1",
            "Shape 2",
            "Color",
            "Mode",
            "Scale",
            "Invert",
            "Margin",
        ],
    ),
    (
        "Bloom",
        &["Amount", "Threshold", "Knee", "Iterations", "Size", "Gaussian"],
    ),
    // ---- M5b batch 4: temporal (kinds 92–94)
    ("Freeze", &["X", "Y", "Frozen solid", "Black BG"]),
    ("StopMotion", &["Frequency", "Frame Count", "Black BG"]),
    ("DelayRGB", &["R delay", "G delay", "B delay"]),
    // ---- M5b batch 5: special (kinds 95–98). Text layout params are read on the CPU
    // (the app's `text.rs` rasterises the text); the shader gets colours, outline, glow.
    (
        "TextBlock",
        &["Color", "Outline Color", "Outline Width", "Glow In", "Glow Out"],
    ),
    (
        "TextEffect",
        &["Color", "Outline Color", "Outline Width", "Glow In", "Glow Out"],
    ),
    (
        "ParticleSystem",
        &[
            "Particles",
            "Lifetime",
            "Spawn Mode",
            "X Pos",
            "Y Pos",
            "Z Pos",
            "X Vel Min",
            "X Vel Max",
            "Y Vel Min",
            "Y Vel Max",
            "Z Vel Min",
            "Z Vel Max",
            "Rotate Min",
            "Rotate Max",
            "Scale Min",
            "Scale Max",
            "Scale Delta",
            "Opacity Delta",
            "Inherit Vel",
            "Gravity",
            "Wind",
            "Friction",
        ],
    ),
    (
        "Terrain",
        &[
            "Tilt",
            "Direction",
            "Position Y",
            "Height",
            "Wobble",
            "Speed",
            "Jagginess",
            "Attenuation",
            "Texture Rep",
            "Additive",
        ],
    ),
    // ---- M6 part 3 (kinds 99–102): slices of the advanced output, second inputs
    ("ScreenLayerTransform", &["Black BG", "Scaling"]),
    ("SliceDelay", &["Order", "Scaling"]),
    (
        "UVMap",
        &[
            "Flip U",
            "Flip V",
            "Offset X",
            "Offset Y",
            "Filter Strength",
            "Filter Color",
            "Black Is Transparent",
        ],
    ),
    ("Mask", &["Resize", "Channel", "Invert"]),
    // ---- LAVMFX's own (kinds 103–): Slice Chase. Per-slice values (level,
    // step phase…) come from `chase` on the CPU, in the effect's chase buffer.
    ("SliceChaseSequencer", &["Scaling", "Unlit", "Outside"]),
    // The looks' params come first (`chase.wgsl` reads them as 12 slots), then Unlit, Outside.
    (
        "SliceChaseWipe",
        &[
            "Wipe Direction",
            "Wipe Style",
            "Softness",
            "Wipes per Step",
            "Alternate",
            "Unlit",
            "Outside",
        ],
    ),
    (
        "SliceChaseRunner",
        &[
            "Line Color",
            "Line Width",
            "Feather",
            "Segments",
            "Segment Length",
            "Laps per Step",
            "Runner Mode",
            "Line Content",
            "Alternate",
            "Unlit",
            "Outside",
        ],
    ),
    (
        "SliceChaseTiles",
        &[
            "Tile Color",
            "Tiles X",
            "Tiles Y",
            "Tile Shape",
            "Tile Size",
            "Softness",
            "Build",
            "Coloring",
            "Edges Only",
            "Unlit",
            "Outside",
        ],
    ),
    (
        "SliceChaseSample",
        &["Sample Point", "Sample As", "Mix", "Unlit", "Outside"],
    ),
    ("SliceChaseDelay", &["Delay", "Reverse", "Unlit", "Outside"]),
];

/// Mask effect: the image file (our own param; a file chooser has no
/// REST value).
pub const MASK_FILE_KEY: &str = "File";

/// Frames Slice Delay keeps (30 ≈ 250 MB at 1080p).
pub const SLICE_DELAY_FRAMES: u32 = 30;

/// Shader kind of an effect (by internal name).
pub fn shader_kind(name: &str) -> Option<u32> {
    SHADER_EFFECTS.iter().position(|(n, _)| *n == name).map(|i| i as u32)
}

/// Shader kinds that read their own previous output (need a feedback texture).
pub const FEEDBACK_EFFECTS: &[&str] = &["Trails", "Acuarela", "Freeze", "StopMotion"];

/// What the renderer needs for one effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShaderFx {
    pub kind: u32,
    pub params: [f32; SHADER_PARAMS],
    /// Dry/wet, 0..1.
    pub opacity: f32,
    /// Dry/wet blend (shader blend index).
    pub blend: u32,
    /// Effect instance id (keys feedback state).
    pub id: Id,
    pub feedback: bool,
    /// Render passes (multi-pass effects: blurs).
    pub passes: u32,
    /// Frames of input history the renderer keeps (delays).
    pub history: u32,
}

/// `#rrggbbaa` / `#rrggbb` / `#rgb(a)` → linear-ish 0..1 floats (sRGB values as stored).
pub fn parse_color(s: &str) -> [f32; 4] {
    let h = s.trim_start_matches('#');
    let digits: Vec<u8> = h.chars().filter_map(|c| c.to_digit(16).map(|d| d as u8)).collect();
    let byte = |i: usize| (digits[i] * 16 + digits[i + 1]) as f32 / 255.0;
    let nib = |i: usize| (digits[i] * 17) as f32 / 255.0;
    match digits.len() {
        8 => [byte(0), byte(2), byte(4), byte(6)],
        6 => [byte(0), byte(2), byte(4), 1.0],
        4 => [nib(0), nib(1), nib(2), nib(3)],
        3 => [nib(0), nib(1), nib(2), 1.0],
        _ => [1.0; 4],
    }
}

/// Floats → `#rrggbbaa`.
pub fn format_color(c: [f32; 4]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]), b(c[3]))
}

/// Shader slots of `names` from `params` (a colour takes 4 slots, a bool/choice one).
/// A missing param keeps the layout using its catalogue entry (`fallback`).
pub(crate) fn pack_params(
    params: &ParameterCollection,
    names: &[&str],
    fallback: Option<&serde_json::Map<String, serde_json::Value>>,
) -> [f32; SHADER_PARAMS] {
    let mut out = [0.0f32; SHADER_PARAMS];
    let mut slot = 0;
    for name in names {
        let vals: Vec<f32> = match params.get(*name) {
            Some(Param::Range(r)) => vec![r.value as f32],
            Some(Param::Bool(b)) => vec![b.value as u8 as f32],
            Some(Param::Choice(c)) => vec![c.index as f32],
            Some(Param::Int(i)) => vec![i.value as f32],
            Some(Param::Color(c)) => parse_color(&c.value).to_vec(),
            _ => {
                let w = fallback
                    .and_then(|s| s.get(*name))
                    .and_then(|p| p.get("valuetype"))
                    .and_then(|v| v.as_str())
                    .map_or(1, |t| if t == "ParamColor" { 4 } else { 1 });
                vec![0.0; w]
            }
        };
        for v in vals {
            if slot < SHADER_PARAMS {
                out[slot] = v;
            }
            slot += 1;
        }
    }
    out
}

/// The effect as the renderer draws it; None when bypassed, fully dry or not
/// implemented.
pub fn shader_fx(e: &VideoEffect) -> Option<ShaderFx> {
    if e.is_bypassed() {
        return None;
    }
    let opacity = e.opacity() as f32;
    if opacity <= 0.0 {
        return None;
    }
    if crate::isf::is_isf(&e.name) {
        return crate::isf::shader_fx(e, opacity.min(1.0), e.blend_index());
    }
    if crate::ffgl::is_ffgl(&e.name) {
        return crate::ffgl::shader_fx(e, opacity.min(1.0), e.blend_index());
    }
    let kind = shader_kind(&e.name)?;
    let params = pack_params(
        &e.params,
        SHADER_EFFECTS[kind as usize].1,
        spec(&e.name).map(|s| &s.params),
    );
    Some(ShaderFx {
        kind,
        params,
        opacity: opacity.min(1.0),
        blend: e.blend_index(),
        id: e.id,
        feedback: FEEDBACK_EFFECTS.contains(&e.name.as_str()),
        passes: shader_passes(&e.name, &params),
        history: shader_history(&e.name, &params),
    })
}

/// Render passes of an effect (see `effects.wgsl`: blurs are separable).
pub fn shader_passes(name: &str, params: &[f32; SHADER_PARAMS]) -> u32 {
    match name {
        "Blur" => 2,
        // extract, Iterations × (horizontal + vertical blur), composite
        "Bloom" => 2 * (params[3].round().clamp(1.0, 5.0) as u32) + 2,
        "TiltShift" => 3,
        _ => 1,
    }
}

/// Frames of input history an effect reads (delayed copies), 0 = none.
pub const HISTORY_FRAMES: u32 = 16;
pub fn shader_history(name: &str, params: &[f32; SHADER_PARAMS]) -> u32 {
    let on = match name {
        "WarpSpeed" | "DelayRGB" => true,
        "LinearCloner" => params[13] > 0.5, // Delay Enabled
        "RadialCloner" => params[10] > 0.5,
        "GridCloner" => params[16] > 0.5, // Delay
        "SliceDelay" | "SliceChaseDelay" => return SLICE_DELAY_FRAMES,
        _ => false,
    };
    if on { HISTORY_FRAMES } else { 0 }
}

/// Renderer view of a chain.
pub fn shader_chain(chain: &[VideoEffect]) -> Vec<ShaderFx> {
    chain.iter().filter_map(shader_fx).filter(|f| !is_identity(f)).collect()
}

/// Effects at their neutral settings are skipped (Transform at defaults).
fn is_identity(f: &ShaderFx) -> bool {
    f.kind == 0 && f.params[..10] == [0.0, 0.0, 100.0, 100.0, 100.0, 0.0, 0.0, 0.0, 0.0, 0.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_has_all_effects() {
        assert_eq!(catalog().len(), 109);
        assert_eq!(SHADER_EFFECTS.len(), 109, "render test loops over every kind");
        // Effect kinds stay below the generators'.
        assert!((SHADER_EFFECTS.len() as u32) <= crate::sources::GENERATOR_KIND_BASE);
        assert_eq!(shader_kind("SliceChaseSequencer"), Some(103), "effects.wgsl case 103");
        assert_eq!(shader_kind("SliceChaseDelay"), Some(108), "effects.wgsl case 108");
        for (name, params) in SHADER_EFFECTS {
            let s = spec(name).unwrap_or_else(|| panic!("{name} not in catalogue"));
            for p in *params {
                assert!(s.params.contains_key(*p), "{name}: no param {p}");
            }
        }
    }

    #[test]
    fn instantiate_keeps_order_ids_and_defaults() {
        let mut ids = IdGen::new();
        let fx = VideoEffect::from_catalog(&mut ids, "Bright.Contrast").unwrap();
        assert_eq!(fx.name, "BrightnessContrast");
        let keys: Vec<_> = fx.params.keys().map(String::as_str).collect();
        assert_eq!(keys, ["Opacity", "Brightness", "Contrast"]);
        assert!(fx.bypassed.is_some());
        let Some(Param::Range(b)) = fx.params.get("Brightness") else {
            panic!()
        };
        assert_eq!((b.min, b.max, b.default), (-1.0, 1.0, Some(0.0)));
        let json = serde_json::to_string(&fx).unwrap();
        assert_eq!(serde_json::from_str::<VideoEffect>(&json).unwrap(), fx);
        // Every param of every effect parses.
        for s in catalog() {
            let fx = s.instantiate(&mut ids);
            let extra = usize::from(s.name == "Mask"); // + File
            assert_eq!(fx.params.len(), s.params.len() + extra, "{}", s.name);
        }
    }

    #[test]
    fn chains_and_shader_params() {
        let mut c = Composition::new("t", 2, 2);
        let id = c.add_effect(FxTarget::Layer(0), "Tint", None).unwrap();
        let chain = c.effects(FxTarget::Layer(0)).unwrap();
        assert_eq!(chain.len(), 2);
        assert!(chain[0].is_transform());
        assert_eq!(chain[1].id, id);
        assert!(!c.remove_effect(FxTarget::Layer(0), 0), "Transform stays");
        let f = shader_chain(c.effects(FxTarget::Layer(0)).unwrap());
        assert_eq!(f.len(), 1, "default Transform is skipped");
        assert_eq!(f[0].kind, shader_kind("Tint").unwrap());
        assert_eq!(&f[0].params[..8], &[1.0, 0.0, 0.0, 1.0, 0.8, 1.0, 0.0, 1.0]);
        c.add_effect(FxTarget::Composition, "Blur", None).unwrap();
        assert_eq!(c.effects(FxTarget::Composition).unwrap().len(), 1);
        assert!(c.move_effect(FxTarget::Layer(0), 1, 0));
        assert_eq!(c.effects(FxTarget::Layer(0)).unwrap()[0].id, id);
    }

    /// Test presets (our own values) in the factory-presets file format.
    pub(crate) const TEST_PRESETS: &str = r#"{"ShiftRGB": {
        "Wide": {"params": {"Distance": 0.25, "Mode": "Shift X", "R": -0.3, "B": 0.3}, "blend": "Add"},
        "Up & Down": {"params": {"Distance": 0.0, "Mode": "Shift Y"}, "blend": "Add",
            "drives": {"Distance": {"from": 0.0, "to": 0.4, "base": 0.2, "direction": "Forward",
                "mode": "Bounce", "link": 0, "seconds": 2.0, "beats": 4.0, "source": "Timeline"}}}
    }}"#;

    #[test]
    fn builtin_presets_apply() {
        let mut ids = IdGen::new();
        let all = parse_presets(TEST_PRESETS);
        let ps = &all["ShiftRGB"];
        assert_eq!(ps.len(), 2);
        let p = ps.iter().find(|p| p.name == "Wide").unwrap();
        let mut fx = VideoEffect::from_catalog(&mut ids, "Shift RGB").unwrap();
        fx.apply_values(&p.values, p.blend.as_deref());
        let changed = p.values.iter().any(|(k, v)| match (fx.params.get(k), v) {
            (Some(Param::Range(r)), serde_json::Value::Number(n)) => (r.value - n.as_f64().unwrap()).abs() < 1e-9,
            _ => false,
        });
        assert!(changed);
        fx.reset_defaults();
        let Some(Param::Range(d)) = fx.params.get("Distance") else {
            panic!()
        };
        assert_eq!(d.value, 1.0);
        // Animated preset: Up & Down bounces Distance.
        let up = ps.iter().find(|p| p.name == "Up & Down").unwrap();
        fx.apply_builtin(up);
        let Some(Param::Range(d)) = fx.params.get("Distance") else {
            panic!()
        };
        assert!(d.is_driven(), "preset animation restored");
        assert_eq!(d.drive.as_ref().unwrap().mode, crate::drive::PlayMode::Bounce);
        fx.reset_defaults();
        let Some(Param::Range(d)) = fx.params.get("Distance") else {
            panic!()
        };
        assert!(!d.is_driven(), "Default clears animations");
        // Every shipped factory preset belongs to a catalogue effect.
        let shipped = parse_presets(include_str!("../data/factory-presets.json"));
        for fx in shipped.keys() {
            assert!(
                catalog().iter().any(|s| &s.name == fx),
                "factory preset for unknown effect {fx}"
            );
        }
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#ff000080"), [1.0, 0.0, 0.0, 128.0 / 255.0]);
        assert_eq!(format_color([1.0, 0.0, 0.0, 1.0]), "#ff0000ff");
    }
}

#[cfg(test)]
mod transfer {
    use super::*;

    fn names(c: &Composition, t: FxTarget) -> Vec<String> {
        c.effects(t).unwrap().iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn reorder_move_copy() {
        let mut c = Composition::new("t", 2, 2);
        let (a, b) = (FxTarget::Layer(0), FxTarget::Composition);
        for n in ["Blur", "Invert RGB", "Mirror"] {
            c.add_effect(a, n, None).unwrap();
        }
        let base = names(&c, a); // Transform, Blur, InvertRGB, Mirror
        // Reorder: Blur (1) to the end.
        assert_eq!(c.transfer_effect(a, 1, a, 4, false), Some(3));
        assert_eq!(
            names(&c, a),
            [base[0].clone(), base[2].clone(), base[3].clone(), base[1].clone()]
        );
        // Onto itself: nothing.
        assert_eq!(c.transfer_effect(a, 1, a, 2, false), Some(1));
        // Transform stays first and doesn't move.
        assert_eq!(c.transfer_effect(a, 3, a, 0, false), Some(1));
        assert_eq!(names(&c, a)[0], base[0]);
        assert_eq!(c.transfer_effect(a, 0, b, 0, false), None);
        // Move to the composition.
        let id = c.effects(a).unwrap()[1].id;
        assert_eq!(c.transfer_effect(a, 1, b, 0, false), Some(0));
        assert_eq!(c.effects(a).unwrap().len(), 3);
        assert_eq!(c.effects(b).unwrap()[0].id, id);
        // Copy back: fresh ids, original stays.
        assert_eq!(c.transfer_effect(b, 0, a, 9, true), Some(3));
        let copy = &c.effects(a).unwrap()[3];
        assert_ne!(copy.id, id);
        assert_eq!(c.effects(b).unwrap().len(), 1);
        let ids = |fx: &VideoEffect| -> Vec<i64> {
            fn walk(v: &serde_json::Value, out: &mut Vec<i64>) {
                match v {
                    serde_json::Value::Object(m) => m.iter().for_each(|(k, v)| match v.as_i64() {
                        Some(n) if k == "id" => out.push(n),
                        _ => walk(v, out),
                    }),
                    serde_json::Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
                    _ => {}
                }
            }
            let mut out = Vec::new();
            walk(&serde_json::to_value(fx).unwrap(), &mut out);
            out
        };
        let (old, new) = (ids(&c.effects(b).unwrap()[0]), ids(copy));
        assert!(new.len() > 3 && new.iter().all(|n| !old.contains(n)));
    }
}

#[cfg(test)]
mod preset_recall {
    use super::*;

    /// Recalling a preset (shipped factory presets plus the test set) never
    /// hides a rendered effect, and presets that animate keep moving after recall.
    #[test]
    fn builtin_presets_render_and_animate() {
        let mut ids = IdGen::new();
        let frame = crate::drive::Inputs {
            dt: 0.1,
            ..Default::default()
        };
        let test = parse_presets(super::tests::TEST_PRESETS);
        let mut animated = 0;
        for s in catalog() {
            let extra = test.get(&s.name).map_or(&[][..], Vec::as_slice);
            for p in builtin_presets(&s.name).iter().chain(extra) {
                let mut fx = VideoEffect::from_catalog(&mut ids, &s.display_name).expect("catalogue effect");
                let before = shader_fx(&fx).is_some();
                fx.apply_builtin(p);
                assert_eq!(
                    before,
                    shader_fx(&fx).is_some(),
                    "{} / {} hides the effect",
                    s.name,
                    p.name
                );
                // Pause-direction drives are static.
                if !p
                    .drives
                    .values()
                    .any(|d| d.get("direction").and_then(|v| v.as_str()) != Some("Pause"))
                {
                    continue;
                }
                animated += 1;
                let snap = |fx: &VideoEffect| -> Vec<f64> {
                    p.drives
                        .keys()
                        .filter_map(|k| match fx.params.get(k) {
                            Some(Param::Range(r)) => Some(r.value),
                            _ => None,
                        })
                        .collect()
                };
                // 0.1 s per frame at 120 BPM.
                let step = |fx: &mut VideoEffect, n: f64| {
                    let i = crate::drive::Inputs {
                        beats: n * 0.2,
                        ..frame
                    };
                    for q in fx.params.values_mut() {
                        if let Param::Range(r) = q {
                            r.drive_step(&i);
                        }
                    }
                };
                step(&mut fx, 1.0);
                let a = snap(&fx);
                step(&mut fx, 2.0);
                assert_ne!(a, snap(&fx), "{} / {} does not animate after recall", s.name, p.name);
            }
        }
        assert!(animated >= 1, "{animated} animated presets");
    }
}
