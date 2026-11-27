//! The six built-in audio effects (Bitcrusher, Distortion, EQ-3,
//! Flanger, HighPass, LowPass): catalogue + REST-shaped instances.
//!
//! Parameter names, order, ranges (all 0..1) and defaults match the REST
//! shape controllers expect. The DSP (`lavmfx-audio::fx`) maps the 0..1 values to physical units.

use crate::param::{BoolParam, Param, ParameterCollection, RangeParam};
use crate::{AudioEffect, IdGen};

/// Which effect (DSP kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioFxKind {
    Bitcrusher,
    Distortion,
    Eq3,
    Flanger,
    HighPass,
    LowPass,
}

/// A parameter: name + default (range 0..1, or a toggle).
#[derive(Debug, Clone, Copy)]
pub enum PDef {
    Range(&'static str, f64),
    Bool(&'static str, bool),
}

impl PDef {
    pub fn name(self) -> &'static str {
        match self {
            PDef::Range(n, _) | PDef::Bool(n, _) => n,
        }
    }
}

pub struct AudioFxSpec {
    pub name: &'static str,
    pub kind: AudioFxKind,
    /// In REST order; "Dry Wet" first on every effect.
    pub params: &'static [PDef],
}

/// Most params any effect has (DSP value arrays are this long).
pub const MAX_PARAMS: usize = 6;

pub const AUDIO_EFFECTS: [AudioFxSpec; 6] = [
    AudioFxSpec {
        name: "Bitcrusher",
        kind: AudioFxKind::Bitcrusher,
        params: &[
            PDef::Range("Dry Wet", 1.0),
            PDef::Range("Gain", 1.0),
            PDef::Range("Sample Rate", 1.0),
            PDef::Range("Bit Depth", 1.0),
            PDef::Bool("Dither", false),
            PDef::Bool("Jitter", false),
        ],
    },
    AudioFxSpec {
        name: "Distortion",
        kind: AudioFxKind::Distortion,
        params: &[
            PDef::Range("Dry Wet", 1.0),
            PDef::Range("Pre Gain", 0.1),
            PDef::Range("Drive", 0.0),
            PDef::Range("Tone", 1.0),
            PDef::Range("Post Gain", 0.1),
        ],
    },
    AudioFxSpec {
        name: "EQ-3",
        kind: AudioFxKind::Eq3,
        params: &[
            PDef::Range("Dry Wet", 1.0),
            PDef::Range("Low", 0.5),
            PDef::Range("Mid", 0.5),
            PDef::Range("High", 0.5),
        ],
    },
    AudioFxSpec {
        name: "Flanger",
        kind: AudioFxKind::Flanger,
        params: &[
            PDef::Range("Dry Wet", 1.0),
            PDef::Range("Depth", 0.0),
            PDef::Range("Rate", 0.05),
            PDef::Range("Feedback", 0.7),
            PDef::Bool("Invert", false),
            PDef::Range("Width", 0.0003),
        ],
    },
    AudioFxSpec {
        name: "HighPass",
        kind: AudioFxKind::HighPass,
        params: &[
            PDef::Range("Dry Wet", 1.0),
            PDef::Range("Frequency", 0.0),
            PDef::Range("Resonance", 0.0),
        ],
    },
    AudioFxSpec {
        name: "LowPass",
        kind: AudioFxKind::LowPass,
        params: &[
            PDef::Range("Dry Wet", 1.0),
            PDef::Range("Frequency", 1.0),
            PDef::Range("Resonance", 0.0),
        ],
    },
];

pub fn spec(name: &str) -> Option<&'static AudioFxSpec> {
    AUDIO_EFFECTS.iter().find(|s| s.name.eq_ignore_ascii_case(name))
}

impl AudioEffect {
    /// A new effect with its defaults; None for an unknown name.
    pub fn new(ids: &mut IdGen, name: &str) -> Option<Self> {
        let s = spec(name)?;
        let mut params = ParameterCollection::new();
        for p in s.params {
            let v = match *p {
                PDef::Range(n, d) => (n, Param::Range(RangeParam::new(ids.alloc(), 0.0, 1.0, d))),
                PDef::Bool(n, d) => (n, Param::Bool(BoolParam::new(ids.alloc(), d))),
            };
            params.insert(v.0.to_owned(), v.1);
        }
        Some(Self {
            id: ids.alloc(),
            name: s.name.to_owned(),
            bypassed: BoolParam::new(ids.alloc(), false),
            params,
        })
    }
}

/// What the mixer needs of one effect: stable key, kind, values in the
/// spec's order (toggles 0 / 1; missing params = defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct FxDesc {
    pub key: u64,
    pub kind: AudioFxKind,
    pub bypassed: bool,
    pub values: [f32; MAX_PARAMS],
}

impl FxDesc {
    pub fn of(e: &AudioEffect) -> Option<Self> {
        let s = spec(&e.name)?;
        let mut values = [0.0f32; MAX_PARAMS];
        for (v, d) in values.iter_mut().zip(s.params) {
            *v = match (e.params.get(d.name()), *d) {
                (Some(Param::Range(r)), _) => r.value.clamp(0.0, 1.0) as f32,
                (Some(Param::Bool(b)), _) => b.value as u8 as f32,
                (_, PDef::Range(_, x)) => x as f32,
                (_, PDef::Bool(_, x)) => x as u8 as f32,
            };
        }
        Some(Self {
            key: e.id as u64,
            kind: s.kind,
            bypassed: e.bypassed.value,
            values,
        })
    }
}

/// The chain as the mixer sees it (unknown effects left out).
pub fn chain(fx: &[AudioEffect]) -> Vec<FxDesc> {
    fx.iter().filter_map(FxDesc::of).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_match_the_catalogue() {
        let mut ids = IdGen::default();
        let e = AudioEffect::new(&mut ids, "flanger").unwrap();
        assert_eq!(e.name, "Flanger");
        let names: Vec<&str> = e.params.keys().map(String::as_str).collect();
        assert_eq!(names, ["Dry Wet", "Depth", "Rate", "Feedback", "Invert", "Width"]);
        let d = FxDesc::of(&e).unwrap();
        assert_eq!(d.kind, AudioFxKind::Flanger);
        assert!((d.values[3] - 0.7).abs() < 1e-6 && d.values[4] == 0.0);
        assert!(AudioEffect::new(&mut ids, "Reverb").is_none());
        // REST JSON shape round trip.
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(j["params"]["Invert"]["valuetype"], "ParamBoolean");
        let back: AudioEffect = serde_json::from_value(j).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn reference_capture_agrees() {
        // Captured REST data, kept outside the repo: skip without it.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../reference/lavmfx-audio-effects.json"
        );
        let Ok(text) = std::fs::read_to_string(path) else {
            eprintln!("no {path}: skipped");
            return;
        };
        let j: serde_json::Value = serde_json::from_str(&text).unwrap();
        let fx = j["effects"].as_array().unwrap();
        assert_eq!(fx.len(), AUDIO_EFFECTS.len());
        for (e, s) in fx.iter().zip(AUDIO_EFFECTS.iter()) {
            assert_eq!(e["name"], s.name);
            let ps = e["params"].as_array().unwrap();
            assert_eq!(ps.len(), s.params.len(), "{}", s.name);
            for (p, d) in ps.iter().zip(s.params) {
                assert_eq!(p["name"], d.name());
                match *d {
                    PDef::Range(_, v) => assert!(
                        (p["value"].as_f64().unwrap() - v).abs() < 1e-6,
                        "{} {}",
                        s.name,
                        d.name()
                    ),
                    PDef::Bool(_, v) => assert_eq!(p["value"].as_bool(), Some(v)),
                }
            }
        }
    }
}
