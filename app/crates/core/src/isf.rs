//! ISF shaders (<https://isf.video>): `.fs` GLSL files with a JSON header,
//! loaded from folders (Preferences › Video) plus a few built-in examples.
//!
//! A shader with an `inputImage` input is an **effect** (Effects browser,
//! ISF group); one without is a **source** (Sources tab). Transitions
//! (`startImage` / `endImage` / `progress`) are listed but not run yet.
//! Effects and sources are stored by name (`ISF/<file name>`), so a
//! composition reopens with whatever shader of that name is installed.
//!
//! Inputs become params: float → slider, bool / event → toggle, long →
//! choice (the shader gets the VALUE), int (MadMapper) → integer slider,
//! color → colour, point2D → "X" / "Y" sliders. They are packed into the
//! effect's 32 shader floats in input order; inputs past 32 floats stay at
//! their default. The renderer (lavmfx-render `isf.rs`) translates and runs
//! the GLSL; its compile result comes back through [`set_error`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde_json::Value;

use crate::effects::{BLEND_KEY, OPACITY_KEY, SHADER_PARAMS, ShaderFx, format_color, parse_color};
use crate::id::{Id, IdGen};
use crate::model::VideoEffect;
use crate::param::{
    BoolParam, ChoiceParam, ColorParam, DisplayUnits, Param, ParameterCollection, ParameterView, RangeParam,
};

/// Model name prefix: `VideoEffect.name` / generator name = `ISF/<title>`.
pub const PREFIX: &str = "ISF/";
/// Shader kind of registry entry `i` = BASE + i (= lavmfx-render `ISF_KIND_BASE`).
pub const KIND_BASE: u32 = 1000;
/// Generators (MadMapper `time_base`) per shader (= lavmfx-render `ISF_GENERATORS`).
pub const MAX_GENERATORS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsfKind {
    Effect,
    Source,
    Transition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputType {
    Float,
    Bool,
    Event,
    Long,
    Int,
    Color,
    Point,
}

impl InputType {
    /// Shader floats it takes.
    pub fn width(self) -> usize {
        match self {
            Self::Color => 4,
            Self::Point => 2,
            _ => 1,
        }
    }
}

/// A param-backed input.
#[derive(Debug, Clone, PartialEq)]
pub struct IsfInput {
    /// GLSL name.
    pub name: String,
    /// Param key (LABEL, or NAME when there is none / it clashes).
    pub key: String,
    pub ty: InputType,
    pub min: [f32; 2],
    pub max: [f32; 2],
    pub default: [f32; 4],
    /// long: option values and labels.
    pub values: Vec<f32>,
    pub labels: Vec<String>,
    /// First shader float; None = no room (fixed at the default).
    pub slot: Option<usize>,
}

/// What an image input is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageRole {
    /// `inputImage` of an effect.
    Input,
    Audio,
    AudioFft,
    /// Another image input: nothing to bind yet (transparent).
    Other,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct IsfPass {
    pub target: Option<String>,
    pub persistent: bool,
    pub float: bool,
    pub width: Option<String>,
    pub height: Option<String>,
}

/// A number or the shader float of an input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Arg {
    Const(f32),
    Slot(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct IsfGenerator {
    pub name: String,
    pub speed: Arg,
    pub bpm_sync: Arg,
}

/// One loaded shader.
#[derive(Debug, Clone, PartialEq)]
pub struct IsfShader {
    /// Display name (file name without extension).
    pub title: String,
    /// None = built in.
    pub path: Option<PathBuf>,
    pub kind: IsfKind,
    pub description: String,
    pub credit: String,
    pub categories: Vec<String>,
    pub inputs: Vec<IsfInput>,
    pub images: Vec<(String, ImageRole)>,
    pub imported: Vec<(String, PathBuf)>,
    pub passes: Vec<IsfPass>,
    pub generators: Vec<IsfGenerator>,
    pub fragment: String,
    pub vertex: Option<String>,
    /// Why it doesn't run (header, compile or "not yet" message).
    pub error: Option<String>,
}

impl IsfShader {
    /// Model name (`ISF/<title>`).
    pub fn name(&self) -> String {
        format!("{PREFIX}{}", self.title)
    }
    pub fn runs(&self) -> bool {
        self.error.is_none()
    }
}

// ------------------------------------------------------------------ parsing

/// The JSON of the first `/* … */` comment, made strict (trailing commas,
/// `//` comments removed).
pub fn header_json(src: &str) -> Option<Result<Value, String>> {
    let start = src.find("/*")?;
    let end = start + 2 + src[start + 2..].find("*/")?;
    let body = src[start + 2..end].trim();
    if !body.starts_with('{') {
        return None;
    }
    Some(serde_json::from_str(&strictify(body)).map_err(|e| format!("header: {e}")))
}

/// Drop trailing commas and `//` line comments outside strings.
fn strictify(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut in_str = false;
    while i < c.len() {
        let ch = c[i];
        if in_str {
            out.push(ch);
            if ch == '\\' && i + 1 < c.len() {
                out.push(c[i + 1]);
                i += 1;
            } else if ch == '"' {
                in_str = false;
            }
        } else if ch == '"' {
            in_str = true;
            out.push(ch);
        } else if ch == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            continue;
        } else if ch == ',' {
            let mut j = i + 1;
            while j < c.len() && c[j].is_whitespace() {
                j += 1;
            }
            if !matches!(c.get(j), Some('}') | Some(']')) {
                out.push(ch);
            }
        } else {
            out.push(ch);
        }
        i += 1;
    }
    out
}

/// Case-insensitive key lookup (`Label` / `LABEL`).
fn field<'a>(o: &'a Value, key: &str) -> Option<&'a Value> {
    o.as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

fn get_str(o: &Value, key: &str) -> Option<String> {
    field(o, key).and_then(|v| v.as_str()).map(str::to_owned)
}

fn num(v: Option<&Value>) -> Option<f32> {
    match v? {
        Value::Number(n) => n.as_f64().map(|f| f as f32),
        Value::Bool(b) => Some(*b as u8 as f32),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn nums(v: Option<&Value>) -> Vec<f32> {
    match v {
        Some(Value::Array(a)) => a.iter().map(|x| num(Some(x)).unwrap_or(0.0)).collect(),
        Some(x) => num(Some(x)).into_iter().collect(),
        None => Vec::new(),
    }
}

/// Parse a shader source (`vertex` = the `.vs` next to it, if any).
pub fn parse(title: &str, path: Option<PathBuf>, fragment: &str, vertex: Option<String>) -> Result<IsfShader, String> {
    let header = match header_json(fragment) {
        Some(h) => h?,
        None => return Err("no ISF header (a /*{ … }*/ JSON comment at the top)".into()),
    };
    let mut inputs: Vec<IsfInput> = Vec::new();
    let mut images: Vec<(String, ImageRole)> = Vec::new();
    let mut slot = 0usize;
    let raw_inputs = field(&header, "INPUTS")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for inp in &raw_inputs {
        let (Some(name), Some(ty)) = (get_str(inp, "NAME"), get_str(inp, "TYPE")) else {
            continue;
        };
        let ty = match ty.to_ascii_lowercase().as_str() {
            "float" => InputType::Float,
            "bool" => InputType::Bool,
            "event" => InputType::Event,
            "long" => InputType::Long,
            "int" => InputType::Int,
            "color" => InputType::Color,
            "point2d" => InputType::Point,
            "image" => {
                images.push((
                    name.clone(),
                    if name == "inputImage" {
                        ImageRole::Input
                    } else {
                        ImageRole::Other
                    },
                ));
                continue;
            }
            "audio" => {
                images.push((name, ImageRole::Audio));
                continue;
            }
            "audiofft" => {
                images.push((name, ImageRole::AudioFft));
                continue;
            }
            other => return Err(format!("input {name}: unknown TYPE \"{other}\"")),
        };
        let label = get_str(inp, "LABEL").filter(|l| !l.trim().is_empty());
        let mut key = label.unwrap_or_else(|| name.clone());
        if key == OPACITY_KEY || key == BLEND_KEY || inputs.iter().any(|i| i.key == key) {
            key = name.clone();
        }
        let mins = nums(field(inp, "MIN"));
        let maxs = nums(field(inp, "MAX"));
        let defs = nums(field(inp, "DEFAULT"));
        let mut input = IsfInput {
            name,
            key,
            ty,
            min: [0.0; 2],
            max: [1.0; 2],
            default: [0.0; 4],
            values: Vec::new(),
            labels: Vec::new(),
            slot: None,
        };
        match ty {
            InputType::Float | InputType::Int => {
                input.min[0] = mins.first().copied().unwrap_or(0.0);
                input.max[0] = maxs
                    .first()
                    .copied()
                    .unwrap_or(if ty == InputType::Int { 10.0 } else { 1.0 });
                if input.max[0] < input.min[0] {
                    std::mem::swap(&mut input.min[0], &mut input.max[0]);
                }
                input.default[0] = defs.first().copied().unwrap_or(input.min[0]);
            }
            InputType::Bool | InputType::Event => input.default[0] = defs.first().copied().unwrap_or(0.0),
            InputType::Color => {
                input.default = [1.0, 1.0, 1.0, 1.0];
                for (i, v) in defs.iter().take(4).enumerate() {
                    input.default[i] = *v;
                }
            }
            InputType::Point => {
                for (i, d) in input.default.iter_mut().take(2).enumerate() {
                    *d = defs.get(i).copied().unwrap_or(0.0);
                }
                // No range: normalised unless the default looks like pixels.
                let pixels = input.default[..2].iter().any(|v| *v > 1.0);
                for i in 0..2 {
                    input.min[i] = mins.get(i).copied().unwrap_or(0.0);
                    input.max[i] = maxs
                        .get(i)
                        .copied()
                        .unwrap_or(if pixels { [1920.0, 1080.0][i] } else { 1.0 });
                }
            }
            InputType::Long => {
                let vals = nums(field(inp, "VALUES"));
                let labels: Vec<String> = match field(inp, "LABELS").or_else(|| field(inp, "VALUES")) {
                    Some(Value::Array(a)) => a
                        .iter()
                        .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                        .collect(),
                    _ => Vec::new(),
                };
                // MadMapper: VALUES are the labels and the value is the index.
                let string_values =
                    matches!(field(inp, "VALUES"), Some(Value::Array(a)) if a.iter().any(Value::is_string));
                input.values = if string_values || vals.is_empty() {
                    (0..labels.len().max(1)).map(|i| i as f32).collect()
                } else {
                    vals
                };
                input.labels = if labels.len() == input.values.len() {
                    labels
                } else {
                    input.values.iter().map(|v| format!("{v}")).collect()
                };
                // DEFAULT: a value, or (MadMapper) a label.
                input.default[0] = match field(inp, "DEFAULT") {
                    Some(Value::String(s)) => input
                        .labels
                        .iter()
                        .position(|l| l == s)
                        .map_or(input.values[0], |i| input.values[i]),
                    d => num(d).unwrap_or(input.values[0]),
                };
            }
        }
        if slot + ty.width() <= SHADER_PARAMS {
            input.slot = Some(slot);
            slot += ty.width();
        }
        inputs.push(input);
    }
    let names_input = images.iter().any(|(n, r)| n == "inputImage" && *r == ImageRole::Input);
    let transition = ["startImage", "endImage"]
        .iter()
        .all(|n| images.iter().any(|(m, _)| m == n))
        && inputs.iter().any(|i| i.name == "progress");
    let kind = if transition {
        IsfKind::Transition
    } else if names_input {
        IsfKind::Effect
    } else {
        IsfKind::Source
    };
    // Passes (ISF v2) and v1 persistent buffers.
    let mut passes: Vec<IsfPass> = Vec::new();
    for p in field(&header, "PASSES")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        let expr = |k: &str| match field(&p, k) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        let flag = |k: &str| match field(&p, k) {
            Some(Value::Bool(b)) => *b,
            v => num(v).is_some_and(|n| n != 0.0),
        };
        passes.push(IsfPass {
            target: get_str(&p, "TARGET").filter(|t| !t.is_empty()),
            persistent: flag("PERSISTENT"),
            float: flag("FLOAT"),
            width: expr("WIDTH"),
            height: expr("HEIGHT"),
        });
    }
    match field(&header, "PERSISTENT_BUFFERS") {
        Some(Value::Array(a)) => {
            for n in a.iter().filter_map(|v| v.as_str()) {
                for p in passes.iter_mut().filter(|p| p.target.as_deref() == Some(n)) {
                    p.persistent = true;
                }
            }
        }
        Some(Value::Object(o)) => {
            for (n, v) in o {
                for p in passes.iter_mut().filter(|p| p.target.as_deref() == Some(n.as_str())) {
                    p.persistent = true;
                    p.float |= field(v, "FLOAT").is_some_and(|f| f.as_bool() == Some(true));
                    p.width = p.width.take().or_else(|| get_str(v, "WIDTH"));
                    p.height = p.height.take().or_else(|| get_str(v, "HEIGHT"));
                }
            }
        }
        _ => {}
    }
    // IMPORTED: { "name": { "PATH": "file.png" } } or [ { "NAME", "PATH" } ].
    let mut imported = Vec::new();
    let dir = path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    match field(&header, "IMPORTED") {
        Some(Value::Object(o)) => {
            for (n, v) in o {
                if let Some(p) = get_str(v, "PATH") {
                    imported.push((n.clone(), dir.join(p)));
                }
            }
        }
        Some(Value::Array(a)) => {
            for v in a {
                if let (Some(n), Some(p)) = (get_str(v, "NAME"), get_str(v, "PATH")) {
                    imported.push((n, dir.join(p)));
                }
            }
        }
        _ => {}
    }
    // MadMapper `time_base` generators.
    let slot_of = |v: Option<&Value>, fallback: f32| -> Arg {
        match v {
            Some(Value::String(s)) => inputs
                .iter()
                .find(|i| &i.name == s)
                .and_then(|i| i.slot.map(Arg::Slot).or(Some(Arg::Const(i.default[0]))))
                .unwrap_or(Arg::Const(fallback)),
            v => Arg::Const(num(v).unwrap_or(fallback)),
        }
    };
    let mut generators = Vec::new();
    for g in field(&header, "GENERATORS")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        let Some(name) = get_str(&g, "NAME") else { continue };
        let params = field(&g, "PARAMS").cloned().unwrap_or(Value::Null);
        generators.push(IsfGenerator {
            name,
            speed: slot_of(field(&params, "speed"), 1.0),
            bpm_sync: slot_of(field(&params, "bpm_sync"), 0.0),
        });
    }
    generators.truncate(MAX_GENERATORS);
    let categories = match field(&header, "CATEGORIES") {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
        _ => Vec::new(),
    };
    // `#include "file"` (not standard ISF; MadMapper and others use it).
    let mut error = (kind == IsfKind::Transition).then(|| "ISF transitions are not supported yet".to_owned());
    let mut fragment = fragment.to_owned();
    let mut vertex = vertex;
    if fragment.contains("#include") || vertex.as_ref().is_some_and(|v| v.contains("#include")) {
        let inc = |src: &str| resolve_includes(src, &dir, 0);
        match (inc(&fragment), vertex.as_deref().map(inc).transpose()) {
            (Ok(f), Ok(v)) => {
                fragment = f;
                vertex = v;
            }
            (Err(e), _) | (_, Err(e)) => error = error.or(Some(e)),
        }
    }
    Ok(IsfShader {
        title: title.to_owned(),
        path,
        kind,
        description: get_str(&header, "DESCRIPTION").unwrap_or_default(),
        credit: get_str(&header, "CREDIT").unwrap_or_default(),
        categories,
        inputs,
        images,
        imported,
        passes,
        generators,
        fragment,
        vertex,
        error,
    })
}

/// Inline `#include "file"` lines (searched next to the shader, then up to
/// two folders up). Each include becomes one line per included line, so
/// error line numbers after it shift.
fn resolve_includes(src: &str, dir: &Path, depth: u32) -> Result<String, String> {
    let mut out = String::with_capacity(src.len());
    for line in src.split_inclusive('\n') {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix("#include") else {
            out += line;
            continue;
        };
        let name = rest.trim().trim_matches(|c| c == '"' || c == '<' || c == '>').trim();
        if depth > 4 {
            return Err(format!("#include \"{name}\": nested too deeply"));
        }
        let found = dir.ancestors().take(3).map(|d| d.join(name)).find(|p| p.is_file());
        let Some(path) = found else {
            return Err(format!(
                "#include \"{name}\" not found next to the shader (MadMapper's own shader library isn't available outside MadMapper)"
            ));
        };
        let text = std::fs::read_to_string(&path).map_err(|e| format!("#include \"{name}\": {e}"))?;
        out += &resolve_includes(&text, path.parent().unwrap_or(dir), depth + 1)?;
        out.push('\n');
    }
    Ok(out)
}

// ------------------------------------------------------------------ registry

/// Built-in examples (`data/isf/`).
const BUILTIN: &[(&str, &str)] = &[
    ("Audio Bars", include_str!("../data/isf/Audio Bars.fs")),
    ("Feedback Zoom", include_str!("../data/isf/Feedback Zoom.fs")),
    ("Plasma Flow", include_str!("../data/isf/Plasma Flow.fs")),
    ("Radial Burst", include_str!("../data/isf/Radial Burst.fs")),
    ("RGB Split", include_str!("../data/isf/RGB Split.fs")),
    ("Scanlines", include_str!("../data/isf/Scanlines.fs")),
    ("Soft Glow", include_str!("../data/isf/Soft Glow.fs")),
];

#[derive(Default)]
struct Registry {
    shaders: Vec<Arc<IsfShader>>,
    /// Bumped on every change (the app recompiles then).
    generation: u64,
}

static REGISTRY: RwLock<Registry> = RwLock::new(Registry {
    shaders: Vec::new(),
    generation: 0,
});

/// What a folder scan found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadReport {
    pub effects: usize,
    pub sources: usize,
    pub transitions: usize,
    /// (file, reason) of `.fs` files that aren't usable ISF.
    pub skipped: Vec<(PathBuf, String)>,
}

fn builtins() -> Vec<IsfShader> {
    BUILTIN
        .iter()
        .map(|(t, src)| parse(t, None, src, None).unwrap_or_else(|e| panic!("built-in ISF {t}: {e}")))
        .collect()
}

/// `.fs` / `.isf` files under `dir` (3 levels deep, sorted).
fn shader_files(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            if depth > 0 {
                shader_files(&p, depth - 1, out);
            }
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("fs") || e.eq_ignore_ascii_case("isf"))
        {
            out.push(p);
        }
    }
}

/// Built-ins + every ISF shader in `folders` (replaces the previous set).
pub fn load_folders(folders: &[PathBuf]) -> LoadReport {
    let mut shaders = builtins();
    let mut report = LoadReport::default();
    for folder in folders {
        let mut files = Vec::new();
        shader_files(folder, 3, &mut files);
        for f in files {
            let Ok(src) = std::fs::read_to_string(&f) else {
                report.skipped.push((f, "unreadable".into()));
                continue;
            };
            let vertex = std::fs::read_to_string(f.with_extension("vs")).ok();
            let mut title = f.file_stem().and_then(|s| s.to_str()).unwrap_or("Shader").to_owned();
            let base = title.clone();
            let mut n = 2;
            while shaders.iter().any(|s| s.title == title) {
                title = format!("{base} ({n})");
                n += 1;
            }
            match parse(&title, Some(f.clone()), &src, vertex) {
                Ok(s) => shaders.push(s),
                Err(e) => report.skipped.push((f, e)),
            }
        }
    }
    for s in &shaders {
        match s.kind {
            IsfKind::Effect => report.effects += 1,
            IsfKind::Source => report.sources += 1,
            IsfKind::Transition => report.transitions += 1,
        }
    }
    if let Ok(mut r) = REGISTRY.write() {
        r.shaders = shaders.into_iter().map(Arc::new).collect();
        r.generation += 1;
    }
    report
}

fn ensure_loaded() {
    let empty = REGISTRY.read().is_ok_and(|r| r.shaders.is_empty());
    if empty {
        load_folders(&[]);
    }
}

/// Every shader, in kind order (index i ↔ kind `KIND_BASE + i`).
pub fn shaders() -> Vec<Arc<IsfShader>> {
    ensure_loaded();
    REGISTRY.read().map(|r| r.shaders.clone()).unwrap_or_default()
}

/// Changes on every (re)load or compile result.
pub fn generation() -> u64 {
    ensure_loaded();
    REGISTRY.read().map_or(0, |r| r.generation)
}

/// Shader and its render kind by model name (`ISF/<title>`) or title.
pub fn get(name: &str) -> Option<(u32, Arc<IsfShader>)> {
    let title = name.strip_prefix(PREFIX)?;
    ensure_loaded();
    let r = REGISTRY.read().ok()?;
    let i = r.shaders.iter().position(|s| s.title == title)?;
    Some((KIND_BASE + i as u32, r.shaders[i].clone()))
}

pub fn is_isf(name: &str) -> bool {
    name.starts_with(PREFIX)
}

/// Record the renderer's compile result (None = runs).
pub fn set_error(title: &str, error: Option<String>) {
    if let Ok(mut r) = REGISTRY.write()
        && let Some(s) = r.shaders.iter_mut().find(|s| s.title == title)
        && s.error != error
    {
        Arc::make_mut(s).error = error;
        r.generation += 1;
    }
}

// ------------------------------------------------------------------ params

fn range(ids: &mut IdGen, min: f32, max: f32, value: f32, int: bool) -> Param {
    let mut r = RangeParam::new(ids.alloc(), min as f64, max as f64, value as f64);
    if int {
        r.view = Some(ParameterView {
            step: Some(1.0),
            ..ParameterView::units(DisplayUnits::Integer)
        });
    }
    Param::Range(r)
}

/// Default params of a shader's inputs.
pub fn instantiate_params(s: &IsfShader, ids: &mut IdGen) -> ParameterCollection {
    let mut out = ParameterCollection::new();
    for i in &s.inputs {
        match i.ty {
            InputType::Float => {
                out.insert(i.key.clone(), range(ids, i.min[0], i.max[0], i.default[0], false));
            }
            InputType::Int => {
                out.insert(
                    i.key.clone(),
                    range(ids, i.min[0], i.max[0], i.default[0].round(), true),
                );
            }
            InputType::Bool | InputType::Event => {
                out.insert(
                    i.key.clone(),
                    Param::Bool(BoolParam::new(ids.alloc(), i.default[0] > 0.5)),
                );
            }
            InputType::Long => {
                let labels: Vec<&str> = i.labels.iter().map(String::as_str).collect();
                let idx = i.values.iter().position(|v| *v == i.default[0]).unwrap_or(0);
                out.insert(
                    i.key.clone(),
                    Param::Choice(ChoiceParam::new(ids.alloc(), &labels, idx)),
                );
            }
            InputType::Color => {
                out.insert(
                    i.key.clone(),
                    Param::Color(ColorParam::new(ids.alloc(), format_color(i.default))),
                );
            }
            InputType::Point => {
                for (k, axis) in ["X", "Y"].iter().enumerate() {
                    out.insert(
                        format!("{} {axis}", i.key),
                        range(ids, i.min[k], i.max[k], i.default[k], false),
                    );
                }
            }
        }
    }
    out
}

/// A new effect instance (Opacity first, then the inputs; Alpha mixer).
pub fn instantiate_effect(s: &IsfShader, ids: &mut IdGen) -> VideoEffect {
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
    params.extend(instantiate_params(s, ids));
    let mut mixer = ParameterCollection::new();
    mixer.insert(
        BLEND_KEY.into(),
        Param::Choice(crate::defaults::choice(ids, crate::defaults::MIX_MODES, "Alpha")),
    );
    VideoEffect {
        id: ids.alloc(),
        name: s.name(),
        display_name: s.title.clone(),
        bypassed: Some(BoolParam::new(ids.alloc(), false)),
        mixer,
        params,
        effect: ParameterCollection::new(),
        collapsed: false,
    }
}

/// Inputs → the 32 shader floats.
pub fn pack(s: &IsfShader, params: &ParameterCollection) -> [f32; SHADER_PARAMS] {
    let mut out = [0.0f32; SHADER_PARAMS];
    for i in &s.inputs {
        let Some(at) = i.slot else { continue };
        let range_of = |k: &str, d: f32| match params.get(k) {
            Some(Param::Range(r)) => r.value as f32,
            Some(Param::Int(n)) => n.value as f32,
            _ => d,
        };
        match i.ty {
            InputType::Float => out[at] = range_of(&i.key, i.default[0]),
            InputType::Int => out[at] = range_of(&i.key, i.default[0]).round(),
            InputType::Bool | InputType::Event => {
                out[at] = match params.get(&i.key) {
                    Some(Param::Bool(b)) => b.value as u8 as f32,
                    _ => i.default[0],
                }
            }
            InputType::Long => {
                out[at] = match params.get(&i.key) {
                    Some(Param::Choice(c)) => i.values.get(c.index.max(0) as usize).copied().unwrap_or(i.default[0]),
                    _ => i.default[0],
                }
            }
            InputType::Color => {
                let c = match params.get(&i.key) {
                    Some(Param::Color(c)) => parse_color(&c.value),
                    _ => i.default,
                };
                out[at..at + 4].copy_from_slice(&c);
            }
            InputType::Point => {
                out[at] = range_of(&format!("{} X", i.key), i.default[0]);
                out[at + 1] = range_of(&format!("{} Y", i.key), i.default[1]);
            }
        }
    }
    out
}

/// Renderer view of an ISF effect (None: not ISF, unknown or broken).
pub fn shader_fx(e: &VideoEffect, opacity: f32, blend: u32) -> Option<ShaderFx> {
    let (kind, s) = get(&e.name)?;
    if !s.runs() {
        return None;
    }
    Some(ShaderFx {
        kind,
        params: pack(&s, &e.params),
        opacity,
        blend,
        id: e.id,
        feedback: false,
        passes: 1,
        history: 0,
    })
}

/// Renderer view of an ISF source (generator).
pub fn source_fx(name: &str, params: &ParameterCollection, id: Id) -> Option<ShaderFx> {
    let (kind, s) = get(name)?;
    if !s.runs() || s.kind != IsfKind::Source {
        return None;
    }
    Some(ShaderFx {
        kind,
        params: pack(&s, params),
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

    const MM: &str = r#"/*{
    "CREDIT": "x",
    "INPUTS": [
        { "Label": "Speed", "NAME": "mat_speed", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 1.0 },
        { "LABEL": "BPM Sync", "NAME": "mat_bpmsync", "TYPE": "bool", "DEFAULT": true, "FLAGS": "button" },
        { "LABEL": "Shape", "NAME": "shape", "TYPE": "long", "VALUES": ["Out","In","Smooth"], "DEFAULT": "In" },
        { "LABEL": "Count", "NAME": "n", "TYPE": "int", "MIN": 1, "MAX": 200, "DEFAULT": 10 },
        { "Label": "Color/Back Color", "NAME": "bg", "TYPE": "color", "DEFAULT": [ 0.0, 0.5, 0.0, 1.0 ] },
	],
    "GENERATORS": [
        { "NAME": "pos", "TYPE": "time_base", "PARAMS": {"speed": "mat_speed", "bpm_sync": "mat_bpmsync"} },
    ],
}*/
vec4 materialColorForPixel(vec2 texCoord) { return bg; }
"#;

    #[test]
    fn lenient_header_and_madmapper_extensions() {
        let s = parse("Strob", None, MM, None).unwrap();
        assert_eq!(s.kind, IsfKind::Source);
        let keys: Vec<&str> = s.inputs.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, ["Speed", "BPM Sync", "Shape", "Count", "Color/Back Color"]);
        let shape = &s.inputs[2];
        assert_eq!(
            (shape.values.as_slice(), shape.default[0]),
            ([0.0, 1.0, 2.0].as_slice(), 1.0)
        );
        assert_eq!(s.inputs[4].slot, Some(4));
        assert_eq!(s.generators[0].speed, Arg::Slot(0));
        assert_eq!(s.generators[0].bpm_sync, Arg::Slot(1));
    }

    #[test]
    fn params_round_trip_through_pack() {
        let s = parse("Strob", None, MM, None).unwrap();
        let mut ids = IdGen::new();
        let mut fx = instantiate_effect(&s, &mut ids);
        assert!(fx.params.contains_key(OPACITY_KEY));
        let p = pack(&s, &fx.params);
        assert_eq!(&p[..8], &[1.0, 1.0, 1.0, 10.0, 0.0, 128.0 / 255.0, 0.0, 1.0]); // colours are stored as #rrggbbaa
        if let Some(Param::Choice(c)) = fx.params.get_mut("Shape") {
            c.select(2);
        }
        if let Some(Param::Range(r)) = fx.params.get_mut("Count") {
            r.set(3.4);
        }
        let p = pack(&s, &fx.params);
        assert_eq!((p[2], p[3]), (2.0, 3.0));
    }

    #[test]
    fn standard_isf_kinds_points_and_passes() {
        let fx = r#"/*{ "ISFVSN": "2", "INPUTS": [
            { "NAME": "inputImage", "TYPE": "image" },
            { "NAME": "center", "TYPE": "point2D", "DEFAULT": [0.5, 0.25] },
            { "NAME": "level", "TYPE": "long", "VALUES": [2, 4, 8], "LABELS": ["Low", "Mid", "High"], "DEFAULT": 4 },
            { "NAME": "spectrum", "TYPE": "audioFFT" }
        ], "PASSES": [ { "TARGET": "small", "WIDTH": "$WIDTH/4", "HEIGHT": 64, "PERSISTENT": true, "FLOAT": 1 }, {} ] }*/
        void main() { gl_FragColor = IMG_THIS_PIXEL(inputImage); }"#;
        let s = parse("Zoom", None, fx, None).unwrap();
        assert_eq!(s.kind, IsfKind::Effect);
        assert_eq!(s.images[1], ("spectrum".to_string(), ImageRole::AudioFft));
        assert_eq!(s.inputs[1].labels, ["Low", "Mid", "High"]);
        assert_eq!(s.inputs[1].default[0], 4.0);
        let p = &s.passes[0];
        assert_eq!(
            (p.width.as_deref(), p.height.as_deref(), p.persistent, p.float),
            (Some("$WIDTH/4"), Some("64"), true, true)
        );
        let mut ids = IdGen::new();
        let params = instantiate_params(&s, &mut ids);
        assert!(params.contains_key("center X") && params.contains_key("center Y"));
        let packed = pack(&s, &params);
        assert_eq!(&packed[..3], &[0.5, 0.25, 4.0]);
        let tr = r#"/*{ "INPUTS": [ {"NAME":"startImage","TYPE":"image"}, {"NAME":"endImage","TYPE":"image"}, {"NAME":"progress","TYPE":"float"} ] }*/"#;
        let t = parse("T", None, tr, None).unwrap();
        assert_eq!(t.kind, IsfKind::Transition);
        assert!(!t.runs());
        assert!(parse("x", None, "void main() {}", None).is_err());
    }

    #[test]
    fn includes_resolve_next_to_the_shader() {
        let dir = std::env::temp_dir().join(format!("lavmfx-isf-inc-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("common.glsl"), "float twice(float x) { return 2.0 * x; }\n").unwrap();
        let src =
            "/*{ \"INPUTS\": [] }*/\n#include \"common.glsl\"\nvoid main() { gl_FragColor = vec4(twice(0.25)); }\n";
        let s = parse("inc", Some(dir.join("sub/inc.fs")), src, None).unwrap();
        assert!(s.runs() && s.fragment.contains("float twice"), "{:?}", s.error);
        let missing = parse(
            "m",
            Some(dir.join("m.fs")),
            "/*{}*/\n#include \"MadNoise.glsl\"\n",
            None,
        )
        .unwrap();
        assert!(missing.error.unwrap().contains("MadNoise.glsl"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn too_many_floats_keep_defaults() {
        let inputs: Vec<String> = (0..10)
            .map(|i| format!(r#"{{ "NAME": "c{i}", "TYPE": "color", "DEFAULT": [1,0,0,1] }}"#))
            .collect();
        let src = format!(r#"/*{{ "INPUTS": [{}] }}*/"#, inputs.join(","));
        let s = parse("many", None, &src, None).unwrap();
        assert_eq!(s.inputs.iter().filter(|i| i.slot.is_some()).count(), 8);
        assert_eq!(s.inputs[9].slot, None);
    }

    #[test]
    fn effects_and_sources_survive_save_and_load() {
        let mut comp = crate::Composition::new("t", 1, 1);
        let t = crate::effects::FxTarget::Composition;
        comp.add_effect(t, "ISF/RGB Split", None).unwrap();
        assert!(comp.layers[0].clips[0].load_source(&mut comp.ids, "ISF/Plasma Flow"));
        let back: crate::Composition = serde_json::from_str(&serde_json::to_string(&comp).unwrap()).unwrap();
        let fx = &back.effects(t).unwrap()[0];
        assert!(fx.is_rendered());
        let f = crate::effects::shader_fx(fx).unwrap();
        assert!(f.kind >= KIND_BASE);
        assert_eq!(f.params[0], 0.01); // Amount default
        let v = back.layers[0].clips[0].video.as_ref().unwrap();
        let Some(crate::sources::ClipSource::Generator(name)) = &v.source else {
            panic!()
        };
        assert_eq!(name, "ISF/Plasma Flow");
        assert_eq!(back.layers[0].clips[0].name.value, "Plasma Flow");
        let src = crate::sources::source_fx(name, v.sourceparams.as_ref().unwrap(), 1).unwrap();
        assert_eq!(src.params[1], 4.0); // Scale default
        // A shader that isn't installed stays in the file but isn't drawn.
        let mut gone = fx.clone();
        gone.name = "ISF/Not Here".into();
        assert!(!gone.is_rendered() && crate::effects::shader_fx(&gone).is_none());
    }

    #[test]
    fn builtins_load_and_lookup() {
        let all = builtins();
        assert!(all.iter().any(|s| s.kind == IsfKind::Effect));
        assert!(all.iter().any(|s| s.kind == IsfKind::Source));
        let (kind, s) = get("ISF/Plasma Flow").unwrap();
        assert!(kind >= KIND_BASE);
        assert_eq!(s.name(), "ISF/Plasma Flow");
        assert!(get("Plasma Flow").is_none());
    }
}
