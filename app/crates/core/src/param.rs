//! Parameter types.
//!
//! Field names and JSON shape follow the public REST API schema
//! (`rest/docs/schemas/*Parameter.yaml`), so a composition serialised with
//! `serde_json` can be served to existing REST/OSC controllers unchanged.

use serde::{Deserialize, Serialize};

use crate::id::Id;

/// How a parameter wants to be displayed. Read-only metadata.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ParameterView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiplier: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_units: Option<DisplayUnits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_type: Option<ControlType>,
    /// Display name (REST `alternative_name`; a dashboard knob's name,
    /// `altName` in `.avc`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternative_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayUnits {
    Real,
    Integer,
    Percent,
    Degrees,
    Decibels,
    FramesPerSecond,
    Milliseconds,
    Seconds,
    Beats,
    Fractions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlType {
    BasedOnParam,
    ChoiceButtons,
    ChoiceCombobox,
    Spinner,
    DurationSpinner,
    Slider,
    SliderColorRed,
    SliderColorGreen,
    SliderColorBlue,
    SliderColorHue,
    SliderColorSaturation,
    SliderColorBrightness,
    SliderColorAlpha,
    SliderColorOpacity,
    /// Spelling follows the upstream schema.
    #[serde(rename = "color_pallette")]
    ColorPalette,
    ColorPicker,
    Rotary,
    Text,
    TextMultiline,
}

// serde's `default = "..."` needs a function path.
macro_rules! valuetype_fn {
    ($fn_name:ident, $v:literal) => {
        fn $fn_name() -> String {
            $v.to_owned()
        }
    };
}
valuetype_fn!(vt_string, "ParamString");
valuetype_fn!(vt_text, "ParamText");
valuetype_fn!(vt_bool, "ParamBoolean");
valuetype_fn!(vt_int, "ParamNumber");
valuetype_fn!(vt_range, "ParamRange");
valuetype_fn!(vt_choice, "ParamChoice");
valuetype_fn!(vt_color, "ParamColor");
valuetype_fn!(vt_event, "ParamEvent");

/// Single-line string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StringParam {
    pub id: Id,
    #[serde(default = "vt_string")]
    pub valuetype: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ParameterView>,
}

/// Multi-line string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextParam {
    pub id: Id,
    #[serde(default = "vt_text")]
    pub valuetype: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ParameterView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoolParam {
    pub id: Id,
    #[serde(default = "vt_bool")]
    pub valuetype: String,
    pub value: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ParameterView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntParam {
    pub id: Id,
    #[serde(default = "vt_int")]
    pub valuetype: String,
    pub value: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ParameterView>,
}

/// Floating-point value with a min/max range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RangeParam {
    pub id: Id,
    #[serde(default = "vt_range")]
    pub valuetype: String,
    pub min: f64,
    pub max: f64,
    pub value: f64,
    /// Value a double-click resets to (not in the REST schema; absent in
    /// files saved before M3c).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ParameterView>,
    /// Parameter drive (P menu; not in the REST schema).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drive: Option<Box<crate::drive::Drive>>,
}

/// One of a fixed list of string options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoiceParam {
    pub id: Id,
    #[serde(default = "vt_choice")]
    pub valuetype: String,
    pub value: String,
    pub index: i32,
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ParameterView>,
}

/// Colour as `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorParam {
    pub id: Id,
    #[serde(default = "vt_color")]
    pub valuetype: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub palette: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ParameterView>,
}

/// Value-less trigger (tap, resync, crossfader side A/B, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventParam {
    pub id: Id,
    #[serde(default = "vt_event")]
    pub valuetype: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ParameterView>,
}

/// Any parameter, as stored in a [`ParameterCollection`].
///
/// Untagged like the REST schema's `oneOf`; variant order matters for
/// deserialisation (most specific shapes first).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Param {
    Range(RangeParam),
    Choice(ChoiceParam),
    Color(ColorParam),
    Bool(BoolParam),
    Int(IntParam),
    String(StringParam),
    Text(TextParam),
    /// Must stay last: it matches any object with an id.
    Event(EventParam),
}

/// By `valuetype` (several kinds share a JSON shape: a `ParamText` {value: "…"} would
/// otherwise load as a colour); unknown kinds try the shapes in order (most specific first).
impl<'de> Deserialize<'de> for Param {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        use serde_json::from_value as from;
        let v = serde_json::Value::deserialize(d)?;
        let typed = match v.get("valuetype").and_then(|t| t.as_str()) {
            Some("ParamRange") => Some(from(v.clone()).map(Param::Range)),
            Some("ParamChoice") => Some(from(v.clone()).map(Param::Choice)),
            Some("ParamColor") => Some(from(v.clone()).map(Param::Color)),
            Some("ParamBoolean") => Some(from(v.clone()).map(Param::Bool)),
            Some("ParamNumber") => Some(from(v.clone()).map(Param::Int)),
            Some("ParamString") => Some(from(v.clone()).map(Param::String)),
            Some("ParamText") => Some(from(v.clone()).map(Param::Text)),
            Some("ParamEvent") => Some(from(v.clone()).map(Param::Event)),
            _ => None,
        };
        if let Some(Ok(p)) = typed {
            return Ok(p);
        }
        from(v.clone())
            .map(Param::Range)
            .or_else(|_| from(v.clone()).map(Param::Choice))
            .or_else(|_| from(v.clone()).map(Param::Color))
            .or_else(|_| from(v.clone()).map(Param::Bool))
            .or_else(|_| from(v.clone()).map(Param::Int))
            .or_else(|_| from(v.clone()).map(Param::String))
            .or_else(|_| from(v.clone()).map(Param::Text))
            .or_else(|_| from(v).map(Param::Event))
            .map_err(|_| D::Error::custom("data did not match any parameter shape"))
    }
}

/// Named parameters (dashboard, effect params, mixer, source params).
/// Ordered so UI and serialisation are stable.
pub type ParameterCollection = indexmap::IndexMap<String, Param>;

// ---- convenience constructors ------------------------------------------------

impl RangeParam {
    pub fn new(id: Id, min: f64, max: f64, value: f64) -> Self {
        Self {
            id,
            valuetype: vt_range(),
            min,
            max,
            value: value.clamp(min, max),
            default: Some(value.clamp(min, max)),
            view: None,
            drive: None,
        }
    }
    /// Set the value, clamped to `[min, max]`.
    pub fn set(&mut self, v: f64) {
        self.value = v.clamp(self.min, self.max);
    }
    /// Value mapped to 0..1.
    pub fn normalized(&self) -> f64 {
        if self.max > self.min {
            (self.value - self.min) / (self.max - self.min)
        } else {
            0.0
        }
    }
    pub fn with_view(mut self, view: ParameterView) -> Self {
        self.view = Some(view);
        self
    }
}

impl ChoiceParam {
    pub fn new(id: Id, options: &[&str], index: usize) -> Self {
        let options: Vec<String> = options.iter().map(|s| (*s).to_owned()).collect();
        let index = index.min(options.len().saturating_sub(1));
        Self {
            id,
            valuetype: vt_choice(),
            value: options.get(index).cloned().unwrap_or_default(),
            index: index as i32,
            options,
            view: None,
        }
    }
    /// Select by index; out-of-range indices are ignored.
    pub fn select(&mut self, index: usize) -> bool {
        match self.options.get(index) {
            Some(v) => {
                self.value = v.clone();
                self.index = index as i32;
                true
            }
            None => false,
        }
    }
    /// Select the option named `value`; unknown names are ignored.
    pub fn select_value(&mut self, value: &str) -> bool {
        match self.options.iter().position(|o| o == value) {
            Some(i) => self.select(i),
            None => false,
        }
    }
    /// Replace the option list, keeping the current value when it still
    /// exists (after `rename`), else selecting `fallback` (or the first option).
    pub fn realign(&mut self, options: &[&str], rename: impl Fn(&str) -> &str, fallback: &str) {
        if self.options.iter().map(String::as_str).eq(options.iter().copied()) {
            return;
        }
        let v = rename(&self.value).to_owned();
        let i = options
            .iter()
            .position(|o| *o == v)
            .or_else(|| options.iter().position(|o| *o == fallback))
            .unwrap_or(0);
        self.options = options.iter().map(|s| (*s).to_owned()).collect();
        self.select(i);
    }
}

impl BoolParam {
    pub fn new(id: Id, value: bool) -> Self {
        Self {
            id,
            valuetype: vt_bool(),
            value,
            view: None,
        }
    }
}

impl IntParam {
    pub fn new(id: Id, value: i64) -> Self {
        Self {
            id,
            valuetype: vt_int(),
            value,
            view: None,
        }
    }
}

impl StringParam {
    pub fn new(id: Id, value: impl Into<String>) -> Self {
        Self {
            id,
            valuetype: vt_string(),
            value: value.into(),
            view: None,
        }
    }
}

impl TextParam {
    pub fn new(id: Id, value: impl Into<String>) -> Self {
        Self {
            id,
            valuetype: vt_text(),
            value: value.into(),
            view: None,
        }
    }
}

impl ColorParam {
    pub fn new(id: Id, value: impl Into<String>) -> Self {
        Self {
            id,
            valuetype: vt_color(),
            value: value.into(),
            palette: Vec::new(),
            view: None,
        }
    }
}

impl EventParam {
    pub fn new(id: Id) -> Self {
        Self {
            id,
            valuetype: vt_event(),
            view: None,
        }
    }
}

impl ParameterView {
    pub fn units(units: DisplayUnits) -> Self {
        Self {
            display_units: Some(units),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod valuetype_tests {
    use super::*;

    #[test]
    fn same_shaped_kinds_load_by_valuetype() {
        let text: Param = serde_json::from_str(r#"{"id":1,"valuetype":"ParamText","value":"LAVMFX"}"#).unwrap();
        assert!(matches!(text, Param::Text(ref t) if t.value == "LAVMFX"), "{text:?}");
        let col: Param = serde_json::from_str(r##"{"id":2,"valuetype":"ParamColor","value":"#ff0000ff"}"##).unwrap();
        assert!(matches!(col, Param::Color(_)));
        let s: Param = serde_json::from_str(r#"{"id":3,"valuetype":"ParamString","value":"a"}"#).unwrap();
        assert!(matches!(s, Param::String(_)));
        // Unknown kinds still load by shape.
        let st: Param = serde_json::from_str(r#"{"id":4,"valuetype":"ParamState","value":"x"}"#).unwrap();
        let _ = st;
        // Round trip keeps the kind.
        let back: Param = serde_json::from_str(&serde_json::to_string(&text).unwrap()).unwrap();
        assert_eq!(back, text);
    }
}
