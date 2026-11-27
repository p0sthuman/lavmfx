//! Mixing: blend modes, crossfader and per-layer gain.
//!
//! The layer's blend mode lives in its video track's `mixer` collection under
//! `"Blend Mode"` (REST shape). Compositions saved before M3 lack it; the
//! accessors add it on demand.

use crate::defaults::{DEFAULT_LAYER_BLEND, LAYER_BLEND_MODES, choice};
use crate::id::IdGen;
use crate::model::{AudioTrack, Composition, CrossFader, Layer, LayerGroup};
use crate::param::{ChoiceParam, Param};

/// Key of the blend-mode choice in a video track's mixer.
pub const BLEND_MODE_KEY: &str = "Blend Mode";

/// Blend mode new layers get (Preferences › Defaults); `None` = Add.
static DEFAULT_BLEND: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Set the blend mode new layers get (ignored unless it's a layer blend mode).
pub fn set_default_layer_blend(name: &str) {
    if LAYER_BLEND_MODES.contains(&name)
        && let Ok(mut d) = DEFAULT_BLEND.write()
    {
        *d = Some(name.to_owned());
    }
}

/// Blend mode new layers get.
pub fn default_layer_blend() -> String {
    DEFAULT_BLEND
        .read()
        .ok()
        .and_then(|d| d.clone())
        .unwrap_or_else(|| DEFAULT_LAYER_BLEND.to_owned())
}

/// A fresh layer blend-mode choice ([`default_layer_blend`]).
pub fn blend_mode_param(ids: &mut IdGen) -> ChoiceParam {
    choice(ids, LAYER_BLEND_MODES, &default_layer_blend())
}

/// Blend modes `blend.wgsl` implements, in shader index order
/// (`lavmfx_render::BLEND_MODE_COUNT` entries). Internal names; the UI shows the display names.
pub const SHADER_BLENDS: &[&str] = &[
    "Alpha",
    "Add",
    "Subtract",
    "Multiply",
    "Screen",
    "Overlay",
    "Darken",
    "Lighten",
    "Difference",
    "Exclusion",
    "Hard Light",
    "Soft Light",
    "Color Dodge",
    "Color Burn",
    "Linear Light",
    "Hue",
    "Saturation",
    "Color",
    "Luminosity",
    "Luma Key",
    "Difference I",
    "Luma Is Alpha",
    "Luma Key I",
    // 23+: spatial / transition-style (phase = layer opacity or transition progress)
    "Cut",
    "Dissolve",
    "Push Left",
    "Push Right",
    "Push Up",
    "Push Down",
    "Wipe Ellipse",
    "Zoom In",
    "Zoom Out",
    "Cube",
    "Rotate X",
    "Rotate Y",
    "to Black",
    "to White",
    "to Color",
    "B&W",
    "RGB",
    "Shift RGB",
    "LoRez",
    "Noisy",
    "Static",
    "Parts",
    "Tile",
    "PiP",
    "Side by Side",
    "Displace",
    "JitterBug",
    "Twitch",
    "TimeSwitcher",
    "Meta Mix",
    "Multi Task",
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
];

/// First spatial / transition-style mode in [`SHADER_BLENDS`].
pub const FIRST_SPATIAL_BLEND: u32 = 23;

/// Shader index for a blend/mix mode name; `None` = not implemented
/// yet (drawn as Alpha).
pub fn shader_blend(name: &str) -> Option<u32> {
    Some(match name {
        "Alpha" | "Over" => 0,
        "Dodge" => 12,
        "Burn" => 13,
        n => SHADER_BLENDS.iter().position(|b| *b == n)? as u32,
    })
}

/// Shader index of a blend-mode choice; unimplemented modes fall back to Alpha.
fn mode_index(p: &ChoiceParam) -> u32 {
    shader_blend(&p.value).unwrap_or(0)
}

impl Layer {
    /// Shader blend index (into [`SHADER_BLENDS`]).
    pub fn blend_mode(&self) -> u32 {
        match self.video.as_ref().and_then(|v| v.track.mixer.get(BLEND_MODE_KEY)) {
            Some(Param::Choice(c)) => mode_index(c),
            _ => shader_blend(DEFAULT_LAYER_BLEND).unwrap_or(0),
        }
    }

    /// The blend-mode param, created if missing. `None` for a layer without video.
    pub fn blend_mode_mut(&mut self, ids: &mut IdGen) -> Option<&mut ChoiceParam> {
        let mixer = &mut self.video.as_mut()?.track.mixer;
        if !matches!(mixer.get(BLEND_MODE_KEY), Some(Param::Choice(_))) {
            mixer.insert(BLEND_MODE_KEY.into(), Param::Choice(blend_mode_param(ids)));
        }
        match mixer.get_mut(BLEND_MODE_KEY) {
            Some(Param::Choice(c)) => Some(c),
            _ => None,
        }
    }

    /// Transition shader blend index (into [`SHADER_BLENDS`]).
    pub fn transition_mode(&self) -> u32 {
        mode_index(&self.transition.blend_mode)
    }
}

/// First option of a clip's blend mode: use the layer's.
pub const LAYER_DETERMINED_BLEND: &str = "Layer Determined";

/// Options of a clip's blend mode (REST: "Layer Determined", then the layer modes).
pub fn clip_blend_options() -> Vec<&'static str> {
    std::iter::once(LAYER_DETERMINED_BLEND)
        .chain(LAYER_BLEND_MODES.iter().copied())
        .collect()
}

impl crate::model::Clip {
    /// The clip's own blend mode, overriding its layer's while it plays. `None` = Layer Determined.
    pub fn blend_override(&self) -> Option<u32> {
        match self.video.as_ref()?.track.mixer.get(BLEND_MODE_KEY) {
            Some(Param::Choice(c)) if c.value != LAYER_DETERMINED_BLEND => Some(mode_index(c)),
            _ => None,
        }
    }

    /// The clip's blend-mode param, created (Layer Determined) if missing.
    /// `None` for a clip without video.
    pub fn blend_mode_mut(&mut self, ids: &mut IdGen) -> Option<&mut ChoiceParam> {
        let mixer = &mut self.video.as_mut()?.track.mixer;
        if !matches!(mixer.get(BLEND_MODE_KEY), Some(Param::Choice(_))) {
            let opts = clip_blend_options();
            mixer.insert(
                BLEND_MODE_KEY.into(),
                Param::Choice(choice(ids, &opts, LAYER_DETERMINED_BLEND)),
            );
        }
        match mixer.get_mut(BLEND_MODE_KEY) {
            Some(Param::Choice(c)) => Some(c),
            _ => None,
        }
    }
}

impl LayerGroup {
    /// Shader blend index of the group (its video mixer; default Alpha).
    pub fn blend_mode(&self) -> u32 {
        match self.video.as_ref().and_then(|v| v.mixer.get(BLEND_MODE_KEY)) {
            Some(Param::Choice(c)) => mode_index(c),
            _ => 0,
        }
    }

    /// The group's blend-mode param, created (Alpha) if missing.
    pub fn blend_mode_mut(&mut self, ids: &mut IdGen) -> Option<&mut ChoiceParam> {
        let mixer = &mut self.video.as_mut()?.mixer;
        if !matches!(mixer.get(BLEND_MODE_KEY), Some(Param::Choice(_))) {
            mixer.insert(
                BLEND_MODE_KEY.into(),
                Param::Choice(choice(ids, LAYER_BLEND_MODES, "Alpha")),
            );
        }
        match mixer.get_mut(BLEND_MODE_KEY) {
            Some(Param::Choice(c)) => Some(c),
            _ => None,
        }
    }
}

/// Crossfader side of a layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    None,
    A,
    B,
}

impl Side {
    pub fn of(layer: &Layer) -> Self {
        Self::from_choice(&layer.crossfadergroup)
    }

    pub fn from_choice(c: &ChoiceParam) -> Self {
        match c.index {
            1 => Side::A,
            2 => Side::B,
            _ => Side::None,
        }
    }
}

impl CrossFader {
    /// Gains of side A and B. Phase −1 = all A, +1 = all B.
    ///
    /// Curves: Linear = straight crossfade (½ each in the middle), Gentle =
    /// equal power, Fast = both full in the middle (each side fades over
    /// its own half), Cut = switch at the middle. (Behaviour only changes
    /// what the A/B buttons do.)
    pub fn gains(&self) -> (f32, f32) {
        let mut x = ((self.phase.value as f32 + 1.0) * 0.5).clamp(0.0, 1.0);
        if self.curve.index == 3 {
            x = if x < 0.5 { 0.0 } else { 1.0 };
        }
        match self.curve.index {
            1 => {
                let a = x * std::f32::consts::FRAC_PI_2;
                (a.cos(), a.sin())
            }
            2 => ((2.0 * (1.0 - x)).min(1.0), (2.0 * x).min(1.0)),
            _ => (1.0 - x, x),
        }
    }
}

/// How the compositor should draw one layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerMix {
    /// Layer master × video opacity × crossfader gain; 0 = hidden.
    pub opacity: f32,
    pub blend: u32,
    /// Index into `layergroups` (mixed with its group first).
    pub group: Option<usize>,
}

/// How a layer group is blended (after mixing its layers).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroupMix {
    /// Group master × video opacity × crossfader gain.
    pub opacity: f32,
    pub blend: u32,
}

impl Composition {
    /// Mix settings per layer (index = layer). Bypassed layers (or layers of
    /// a bypassed group) and, when any layer or group is soloed, the layers
    /// not soloed themselves nor in a soloed group get opacity 0. Grouped
    /// layers are mixed with their group first; see [`Self::group_mix`].
    pub fn layer_mix(&self) -> Vec<LayerMix> {
        let group = |l: &Layer| l.group.and_then(|id| self.layergroups.iter().find(|g| g.id == id));
        let any_solo = self
            .layers
            .iter()
            .any(|l| l.solo.value || group(l).is_some_and(|g| g.solo.value));
        let (ga, gb) = self.crossfader.gains();
        self.layers
            .iter()
            .map(|l| {
                let g = group(l);
                let soloed = l.solo.value || g.is_some_and(|g| g.solo.value);
                let hidden =
                    l.video_off || l.bypassed.value || g.is_some_and(|g| g.bypassed.value) || (any_solo && !soloed);

                let gain = match Side::of(l) {
                    Side::None => 1.0,
                    Side::A => ga,
                    Side::B => gb,
                };
                let video = l.video.as_ref().map_or(1.0, |v| v.track.opacity.value) as f32;
                LayerMix {
                    opacity: if hidden {
                        0.0
                    } else {
                        l.master.value as f32 * video * gain
                    },
                    blend: l.blend_mode(),
                    group: l.group.and_then(|id| self.layergroups.iter().position(|g| g.id == id)),
                }
            })
            .collect()
    }
}

impl Composition {
    /// Blend settings per layer group (index = `layergroups`).
    pub fn group_mix(&self) -> Vec<GroupMix> {
        let (ga, gb) = self.crossfader.gains();
        self.layergroups
            .iter()
            .map(|g| {
                let gain = match Side::from_choice(&g.crossfadergroup) {
                    Side::None => 1.0,
                    Side::A => ga,
                    Side::B => gb,
                };
                let video = g.video.as_ref().map_or(1.0, |v| v.opacity.value) as f32;
                GroupMix {
                    opacity: g.master.value as f32 * video * gain,
                    blend: g.blend_mode(),
                }
            })
            .collect()
    }
}

/// Volume param (dB, −192…12) → linear gain; the bottom of the range is silence.
pub fn db_gain(db: f64) -> f32 {
    if db <= -191.0 {
        0.0
    } else {
        10f64.powf(db / 20.0) as f32
    }
}

fn track_gain(t: Option<&AudioTrack>) -> (f32, f32) {
    t.map_or((1.0, 0.0), |t| (db_gain(t.volume.value), t.pan.value as f32))
}

/// How a layer's audio is mixed (clip volume/pan come on top).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioMix {
    /// Linear, to the output: `layer × comp`.
    pub gain: f32,
    /// Layer level: layer master × crossfader × group master × layer and
    /// group volumes; 0 when muted/bypassed.
    pub layer: f32,
    /// Composition volume × composition master.
    pub comp: f32,
    /// Balance −1…1 (layer + group pan).
    pub pan: f32,
    /// The layer's own stage for the mix buses: layer master × crossfader
    /// side × layer volume (0 when muted), and the layer's pan.
    pub stage: f32,
    pub stage_pan: f32,
    /// Index of its group in `layergroups`.
    pub group: Option<usize>,
}

/// A group's stage for the mix buses: group volume × master × crossfader
/// side, and its pan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroupAudioMix {
    pub gain: f32,
    pub pan: f32,
}

impl Composition {
    /// Audio gain and pan per layer. Masters and the crossfader move audio
    /// with video; video opacity and the V toggle don't.
    pub fn audio_mix(&self) -> Vec<AudioMix> {
        let group = |l: &Layer| l.group.and_then(|id| self.layergroups.iter().find(|g| g.id == id));
        let any_solo = self
            .layers
            .iter()
            .any(|l| l.solo.value || group(l).is_some_and(|g| g.solo.value));
        let (ga, gb) = self.crossfader.gains();
        let side = |s: Side| match s {
            Side::None => 1.0,
            Side::A => ga,
            Side::B => gb,
        };
        let (comp_vol, _) = track_gain(self.audio.as_ref());
        let comp = comp_vol * self.master.value as f32;
        self.layers
            .iter()
            .map(|l| {
                let g = group(l);
                let soloed = l.solo.value || g.is_some_and(|g| g.solo.value);
                let muted =
                    l.audio_off || l.bypassed.value || g.is_some_and(|g| g.bypassed.value) || (any_solo && !soloed);
                let (lv, lp) = track_gain(l.audio.as_ref());
                let (gv, gp) = g.map_or((1.0, 0.0), |g| {
                    let (v, p) = track_gain(g.audio.as_ref());
                    (
                        v * g.master.value as f32 * side(Side::from_choice(&g.crossfadergroup)),
                        p,
                    )
                });
                let stage = if muted {
                    0.0
                } else {
                    l.master.value as f32 * side(Side::of(l)) * lv
                };
                let layer = stage * gv;
                AudioMix {
                    gain: layer * comp,
                    layer,
                    comp,
                    pan: (lp + gp).clamp(-1.0, 1.0),
                    stage,
                    stage_pan: lp,
                    group: l.group.and_then(|id| self.layergroups.iter().position(|g| g.id == id)),
                }
            })
            .collect()
    }

    /// Each group's stage (in `layergroups` order).
    pub fn group_audio_mix(&self) -> Vec<GroupAudioMix> {
        let (ga, gb) = self.crossfader.gains();
        self.layergroups
            .iter()
            .map(|g| {
                let side = match Side::from_choice(&g.crossfadergroup) {
                    Side::None => 1.0,
                    Side::A => ga,
                    Side::B => gb,
                };
                let (v, p) = track_gain(g.audio.as_ref());
                GroupAudioMix {
                    gain: v * g.master.value as f32 * side,
                    pan: p,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-5 && (a.1 - b.1).abs() < 1e-5
    }

    #[test]
    fn audio_mix_follows_masters_and_toggles() {
        let mut c = Composition::new("t", 2, 1);
        assert_eq!(
            c.audio_mix()[0],
            AudioMix {
                gain: 1.0,
                layer: 1.0,
                comp: 1.0,
                pan: 0.0,
                stage: 1.0,
                stage_pan: 0.0,
                group: None,
            }
        );
        c.layers[0].master.set(0.5);
        c.layers[0].audio.as_mut().unwrap().volume.set(-6.0);
        c.layers[0].audio.as_mut().unwrap().pan.set(-0.5);
        let m = c.audio_mix()[0];
        assert!((m.gain - 0.5 * 0.501).abs() < 1e-3 && m.pan == -0.5, "{m:?}");
        // Video opacity and V don't touch audio; A mutes it.
        c.layers[1].video.as_mut().unwrap().track.opacity.set(0.0);
        c.layers[1].video_off = true;
        assert_eq!(c.audio_mix()[1].gain, 1.0);
        assert_eq!(c.layer_mix()[1].opacity, 0.0);
        c.layers[1].audio_off = true;
        assert_eq!(c.audio_mix()[1].gain, 0.0);
        // Composition volume at the bottom = silence.
        c.audio.as_mut().unwrap().volume.set(-192.0);
        assert_eq!(c.audio_mix()[0].gain, 0.0);
    }

    #[test]
    fn crossfader_curves() {
        let mut c = Composition::new("t", 1, 1);
        let cf = &mut c.crossfader;
        cf.phase.set(-1.0);
        assert!(close(cf.gains(), (1.0, 0.0)));
        cf.phase.set(0.0);
        assert!(close(cf.gains(), (0.5, 0.5)));
        cf.curve.select(1);
        let s = std::f32::consts::FRAC_1_SQRT_2;
        assert!(close(cf.gains(), (s, s)));
        cf.curve.select(2);
        assert!(close(cf.gains(), (1.0, 1.0)));
        cf.phase.set(0.5);
        assert!(close(cf.gains(), (0.5, 1.0)));
        cf.curve.select(3);
        assert!(close(cf.gains(), (0.0, 1.0)));
        cf.phase.set(-0.1);
        assert!(close(cf.gains(), (1.0, 0.0)));
    }

    #[test]
    fn layer_mix_applies_groups_solo_and_blend() {
        let mut c = Composition::new("t", 3, 1);
        c.layers[0].crossfadergroup.select(1);
        c.layers[1].crossfadergroup.select(2);
        c.layers[2].master.set(0.5);
        c.crossfader.phase.set(1.0);
        let ids = &mut c.ids;
        assert!(c.layers[2].blend_mode_mut(ids).unwrap().select_value("Screen"));
        let m = c.layer_mix();
        assert_eq!(m.iter().map(|m| m.opacity).collect::<Vec<_>>(), [0.0, 1.0, 0.5]);
        assert_eq!(m[2].blend, 4);
        assert_eq!(m[0].blend, 1, "new layers blend with Add");
        c.layers[1].solo.value = true;
        let m = c.layer_mix();
        assert_eq!(m.iter().map(|m| m.opacity).collect::<Vec<_>>(), [0.0, 1.0, 0.0]);
    }

    #[test]
    fn missing_blend_mode_is_created_and_survives_json() {
        let mut c = Composition::new("t", 1, 1);
        c.layers[0].video.as_mut().unwrap().track.mixer.clear();
        assert_eq!(c.layers[0].blend_mode(), 1);
        let ids = &mut c.ids;
        c.layers[0].blend_mode_mut(ids).unwrap().select_value("Screen");
        let json = serde_json::to_string(&c).unwrap();
        let back: Composition = serde_json::from_str(&json).unwrap();
        assert_eq!(back.layers[0].blend_mode(), 4);
    }

    #[test]
    fn every_implemented_mode_is_a_known_shader_mode() {
        for name in LAYER_BLEND_MODES {
            if let Some(i) = shader_blend(name) {
                assert!((i as usize) < SHADER_BLENDS.len(), "{name}");
            }
        }
        assert!(crate::defaults::MIX_MODES.iter().all(|m| LAYER_BLEND_MODES.contains(m)));
    }

    #[test]
    fn groups_scale_bypass_and_solo() {
        let mut c = Composition::new("t", 3, 1);
        let g = c.new_group(&[1, 2], "G");
        c.layergroups[g].master.set(0.5);
        let op = |c: &Composition| c.layer_mix().iter().map(|m| m.opacity).collect::<Vec<_>>();
        assert_eq!(op(&c), [1.0, 1.0, 1.0]);
        assert_eq!(c.layer_mix()[2].group, Some(g));
        assert_eq!(c.group_mix()[g], GroupMix { opacity: 0.5, blend: 0 });
        c.layergroups[g].solo.value = true;
        assert_eq!(op(&c), [0.0, 1.0, 1.0]);
        c.layergroups[g].solo.value = false;
        c.layergroups[g].bypassed.value = true;
        assert_eq!(op(&c), [1.0, 0.0, 0.0]);
    }
}

#[cfg(test)]
mod blend_list_tests {
    use super::*;
    use crate::defaults::MIX_MODES;

    #[test]
    fn every_mode_is_rendered() {
        for m in LAYER_BLEND_MODES.iter().chain(MIX_MODES) {
            assert!(shader_blend(m).is_some(), "{m}");
        }
        assert_eq!(SHADER_BLENDS.len(), 64);
        assert_eq!(shader_blend("Cut"), Some(FIRST_SPATIAL_BLEND));
    }
}
