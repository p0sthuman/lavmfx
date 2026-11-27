//! FFGL plugins in the model: what the app's host found (`lavmfx-io` `ffgl`),
//! as effects (`FFGL/<name>`) and sources. The host itself (OpenGL thread,
//! libraries) lives in lavmfx-io; this is the catalogue + params, like `isf.rs`.
//!
//! Values between host and plugin are normalised 0..1; `FF_GET_RANGE` gives
//! a display range, which our sliders show (packed back to 0..1). Bool /
//! event → toggle, option → choice (the element's value), text / file params
//! aren't supported yet (left at the plugin's default).

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::effects::{BLEND_KEY, OPACITY_KEY, SHADER_PARAMS, ShaderFx};
use crate::id::{Id, IdGen};
use crate::model::VideoEffect;
use crate::param::{BoolParam, ChoiceParam, DisplayUnits, Param, ParameterCollection, ParameterView, RangeParam};

/// Model name prefix (`VideoEffect.name` / generator name).
pub const PREFIX: &str = "FFGL/";
/// Render kind of plugin `i` = BASE + i (= lavmfx-render `EXT_KIND_BASE`).
pub const KIND_BASE: u32 = 2000;

/// `FF_TYPE_*` values used here.
pub const TYPE_BOOLEAN: u32 = 0;
pub const TYPE_EVENT: u32 = 1;
pub const TYPE_OPTION: u32 = 11;
pub const TYPE_INTEGER: u32 = 13;
pub const TYPE_FILE: u32 = 14;
pub const TYPE_TEXT: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfglKind {
    Effect,
    Source,
    Mixer,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FfglParam {
    pub name: String,
    pub ty: u32,
    /// Normalised default.
    pub default: f32,
    /// Display range (None = 0..1).
    pub range: Option<(f32, f32)>,
    pub options: Vec<(String, f32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FfglPlugin {
    /// Display name (unique in the registry).
    pub title: String,
    pub path: PathBuf,
    pub id: String,
    pub kind: FfglKind,
    pub params: Vec<FfglParam>,
    pub error: Option<String>,
}

impl FfglPlugin {
    pub fn name(&self) -> String {
        format!("{PREFIX}{}", self.title)
    }
    pub fn runs(&self) -> bool {
        self.error.is_none()
    }
}

static REGISTRY: RwLock<Vec<Arc<FfglPlugin>>> = RwLock::new(Vec::new());

/// Replace the catalogue (index = the host's plugin index). Titles are made
/// unique ("Name (2)").
pub fn set_plugins(mut v: Vec<FfglPlugin>) {
    for i in 0..v.len() {
        let base = v[i].title.clone();
        let mut n = 2;
        while v[..i].iter().any(|p| p.title == v[i].title) {
            v[i].title = format!("{base} ({n})");
            n += 1;
        }
    }
    if let Ok(mut r) = REGISTRY.write() {
        *r = v.into_iter().map(Arc::new).collect();
    }
}

pub fn plugins() -> Vec<Arc<FfglPlugin>> {
    REGISTRY.read().map(|r| r.clone()).unwrap_or_default()
}

pub fn is_ffgl(name: &str) -> bool {
    name.starts_with(PREFIX)
}

/// Plugin and its render kind by model name.
pub fn get(name: &str) -> Option<(u32, Arc<FfglPlugin>)> {
    let title = name.strip_prefix(PREFIX)?;
    let r = REGISTRY.read().ok()?;
    let i = r.iter().position(|p| p.title == title)?;
    Some((KIND_BASE + i as u32, r[i].clone()))
}

/// Params shown for the plugin's (float-carrying) parameters, by name.
pub fn instantiate_params(p: &FfglPlugin, ids: &mut IdGen) -> ParameterCollection {
    let mut out = ParameterCollection::new();
    for q in &p.params {
        let key = unique_key(&out, &q.name);
        let param = match q.ty {
            TYPE_TEXT | TYPE_FILE => continue,
            TYPE_BOOLEAN | TYPE_EVENT => Param::Bool(BoolParam::new(ids.alloc(), q.default > 0.5)),
            TYPE_OPTION if !q.options.is_empty() => {
                let labels: Vec<&str> = q.options.iter().map(|(l, _)| l.as_str()).collect();
                let idx = q
                    .options
                    .iter()
                    .enumerate()
                    .min_by(|a, b| (a.1.1 - q.default).abs().total_cmp(&(b.1.1 - q.default).abs()))
                    .map_or(0, |(i, _)| i);
                Param::Choice(ChoiceParam::new(ids.alloc(), &labels, idx))
            }
            _ => {
                let (min, max) = q.range.unwrap_or((0.0, 1.0));
                let v = min + q.default.clamp(0.0, 1.0) * (max - min);
                let mut r = RangeParam::new(ids.alloc(), f64::from(min), f64::from(max), f64::from(v));
                if q.ty == TYPE_INTEGER {
                    r.value = r.value.round();
                    r.default = Some(r.value);
                    r.view = Some(ParameterView {
                        step: Some(1.0),
                        ..ParameterView::units(DisplayUnits::Integer)
                    });
                }
                Param::Range(r)
            }
        };
        out.insert(key, param);
    }
    out
}

fn unique_key(map: &ParameterCollection, name: &str) -> String {
    let mut k = name.to_owned();
    let mut n = 2;
    while map.contains_key(&k) || k == OPACITY_KEY || k == BLEND_KEY {
        k = format!("{name} {n}");
        n += 1;
    }
    k
}

pub fn instantiate_effect(p: &FfglPlugin, ids: &mut IdGen) -> VideoEffect {
    let mut params = ParameterCollection::new();
    let mut op = RangeParam::new(ids.alloc(), 0.0, 1.0, 1.0);
    op.view = Some(ParameterView {
        suffix: Some(" %".into()),
        multiplier: Some(100.0),
        display_units: Some(DisplayUnits::Percent),
        control_type: Some(crate::param::ControlType::SliderColorOpacity),
        ..Default::default()
    });
    params.insert(OPACITY_KEY.into(), Param::Range(op));
    params.extend(instantiate_params(p, ids));
    let mut mixer = ParameterCollection::new();
    mixer.insert(
        BLEND_KEY.into(),
        Param::Choice(crate::defaults::choice(ids, crate::defaults::MIX_MODES, "Alpha")),
    );
    VideoEffect {
        id: ids.alloc(),
        name: p.name(),
        display_name: p.title.clone(),
        bypassed: Some(BoolParam::new(ids.alloc(), false)),
        mixer,
        params,
        effect: ParameterCollection::new(),
    }
}

/// Plugin values (normalised) of the first 32 params, in plugin order;
/// params past 32 keep their defaults.
pub fn pack(p: &FfglPlugin, params: &ParameterCollection) -> [f32; SHADER_PARAMS] {
    let mut out = [0.0f32; SHADER_PARAMS];
    let mut keys = ParameterCollection::new(); // replays the key naming of instantiate_params
    for (i, q) in p.params.iter().enumerate().take(SHADER_PARAMS) {
        out[i] = q.default;
        if q.ty == TYPE_TEXT || q.ty == TYPE_FILE {
            continue;
        }
        let key = unique_key(&keys, &q.name);
        keys.insert(key.clone(), Param::Bool(BoolParam::new(0, false)));
        out[i] = match params.get(&key) {
            Some(Param::Bool(b)) => b.value as u8 as f32,
            Some(Param::Choice(c)) => q.options.get(c.index.max(0) as usize).map_or(q.default, |o| o.1),
            Some(Param::Range(r)) => {
                let (min, max) = q.range.unwrap_or((0.0, 1.0));
                if max > min {
                    ((r.value as f32 - min) / (max - min)).clamp(0.0, 1.0)
                } else {
                    q.default
                }
            }
            _ => q.default,
        };
    }
    out
}

pub fn shader_fx(e: &VideoEffect, opacity: f32, blend: u32) -> Option<ShaderFx> {
    let (kind, p) = get(&e.name)?;
    if !p.runs() || p.kind != FfglKind::Effect {
        return None;
    }
    Some(ShaderFx {
        kind,
        params: pack(&p, &e.params),
        opacity,
        blend,
        id: e.id,
        feedback: false,
        passes: 1,
        history: 0,
    })
}

pub fn source_fx(name: &str, params: &ParameterCollection, id: Id) -> Option<ShaderFx> {
    let (kind, p) = get(name)?;
    if !p.runs() || p.kind != FfglKind::Source {
        return None;
    }
    Some(ShaderFx {
        kind,
        params: pack(&p, params),
        opacity: 1.0,
        blend: 0,
        id,
        feedback: false,
        passes: 1,
        history: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin() -> FfglPlugin {
        let p = |name: &str, ty, default, range, options: Vec<(&str, f32)>| FfglParam {
            name: name.into(),
            ty,
            default,
            range,
            options: options.into_iter().map(|(l, v)| (l.to_string(), v)).collect(),
        };
        FfglPlugin {
            title: "Glow".into(),
            path: "/x/Glow.so".into(),
            id: "GLW1".into(),
            kind: FfglKind::Effect,
            params: vec![
                p("Amount", 10, 0.5, Some((0.0, 200.0)), vec![]),
                p("Count", TYPE_INTEGER, 0.25, Some((0.0, 8.0)), vec![]),
                p("On", TYPE_BOOLEAN, 1.0, None, vec![]),
                p("Mode", TYPE_OPTION, 0.5, None, vec![("A", 0.0), ("B", 0.5), ("C", 1.0)]),
                p("Label", TYPE_TEXT, 0.0, None, vec![]),
                p("Amount", 10, 0.1, None, vec![]),
            ],
            error: None,
        }
    }

    #[test]
    fn params_show_display_ranges_and_pack_normalised() {
        let pl = plugin();
        let mut ids = IdGen::new();
        let mut fx = instantiate_effect(&pl, &mut ids);
        let keys: Vec<&str> = fx.params.keys().map(String::as_str).collect();
        assert_eq!(keys, ["Opacity", "Amount", "Count", "On", "Mode", "Amount 2"]);
        assert!(matches!(fx.params.get("Amount"), Some(Param::Range(r)) if r.value == 100.0 && r.max == 200.0));
        assert!(matches!(fx.params.get("Count"), Some(Param::Range(r)) if r.value == 2.0));
        assert!(matches!(fx.params.get("Mode"), Some(Param::Choice(c)) if c.index == 1));
        assert_eq!(&pack(&pl, &fx.params)[..6], &[0.5, 0.25, 1.0, 0.5, 0.0, 0.1]);
        if let Some(Param::Range(r)) = fx.params.get_mut("Amount") {
            r.set(50.0);
        }
        if let Some(Param::Choice(c)) = fx.params.get_mut("Mode") {
            c.select(2);
        }
        let p = pack(&pl, &fx.params);
        assert_eq!((p[0], p[3]), (0.25, 1.0));
    }

    #[test]
    fn registry_names_and_kinds() {
        let mut b = plugin();
        b.kind = FfglKind::Source;
        set_plugins(vec![plugin(), b]);
        let (k0, p0) = get("FFGL/Glow").unwrap();
        let (k1, p1) = get("FFGL/Glow (2)").unwrap();
        assert_eq!((k0, k1), (KIND_BASE, KIND_BASE + 1));
        assert_eq!((p0.kind, p1.kind), (FfglKind::Effect, FfglKind::Source));
        let mut ids = IdGen::new();
        let fx = instantiate_effect(&p0, &mut ids);
        assert!(shader_fx(&fx, 1.0, 0).is_some());
        assert!(source_fx("FFGL/Glow (2)", &instantiate_params(&p1, &mut ids), 1).is_some());
        set_plugins(Vec::new());
    }
}
