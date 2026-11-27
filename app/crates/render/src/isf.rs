//! ISF (Interactive Shader Format, <https://isf.video>) programs: GLSL
//! fragment shaders with a JSON header, as VDMX / MadMapper
//! load them.
//!
//! The header is parsed in lavmfx-core (`isf.rs`); this module gets an
//! [`IsfDesc`] and
//! 1. translates the GLSL 1.x-style source to Vulkan GLSL 450 (inputs become
//!    globals filled from a uniform block, images separate texture + sampler,
//!    `gl_FragColor` an output, `IMG_*` macros, varyings get locations),
//! 2. parses and validates it with naga, so a broken shader reports
//!    "line N: …" instead of reaching wgpu,
//! 3. runs its passes (named / persistent / float targets, sizes from
//!    expressions like `$WIDTH/4`) into an output texture that the effect
//!    pass then mixes like any effect (dry/wet Opacity + Blend Mode).
//!
//! Conventions: ISF is y-up (`isf_FragNormCoord` (0,0) = bottom-left,
//! `gl_FragCoord.y` from the bottom); our textures are y-down, so texture
//! lookups and `gl_FragCoord` are flipped in the translation.
//! The input image is passed premultiplied (opaque content is unaffected).

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use bytemuck::{Pod, Zeroable};
use regex::Regex;

use crate::effects::{FxImage, target_texture};
use crate::{COMP_FORMAT, EFFECT_PARAMS, uniform_buffer};

/// `EffectDraw::kind` of ISF shader `i` (registry index) = BASE + i.
pub const ISF_KIND_BASE: u32 = 1000;
/// Effect-shader kind that mixes the ISF output (bound as `prev_tex`).
pub(crate) const ISF_MIX_KIND: u32 = 999;
/// Generators (MadMapper `time_base`) per shader.
pub const ISF_GENERATORS: usize = 8;
/// Format of `FLOAT` targets (32-bit float isn't filterable everywhere).
const FLOAT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// GLSL type of an input as the shader sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsfType {
    Float,
    /// `bool` and `event`.
    Bool,
    /// `long` (value, not index) and `int`.
    Int,
    Color,
    Point,
}

impl IsfType {
    fn glsl(self) -> &'static str {
        match self {
            Self::Float => "float",
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Color => "vec4",
            Self::Point => "vec2",
        }
    }
    /// Floats it takes in the param array.
    pub fn width(self) -> usize {
        match self {
            Self::Color => 4,
            Self::Point => 2,
            _ => 1,
        }
    }
}

/// A non-image input.
#[derive(Debug, Clone, PartialEq)]
pub struct IsfVar {
    pub name: String,
    pub ty: IsfType,
    /// First float in `EffectDraw::params`; None = fixed at `default` (no room).
    pub slot: Option<usize>,
    pub default: [f32; 4],
}

/// What an image input / imported image is bound to.
#[derive(Debug, Clone, PartialEq)]
pub enum IsfImage {
    /// The effect's input (`inputImage`).
    Input,
    /// Nothing to show yet (transparent).
    Empty,
    /// `audio` waveform: not available, silence.
    Audio,
    /// `audioFFT`: the External FFT bands (32 × 1, level in every channel).
    AudioFft,
    /// `IMPORTED` file, decoded by the app.
    Imported(Arc<FxImage>),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct IsfPassDesc {
    pub target: Option<String>,
    pub persistent: bool,
    pub float: bool,
    /// Size expressions (`$WIDTH/2`, `floor($HEIGHT*0.25)`, `$inputName`).
    pub width: Option<String>,
    pub height: Option<String>,
}

/// A number or a float slot of the params.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IsfArg {
    Const(f32),
    Slot(usize),
}

/// MadMapper `time_base` generator: a float that grows by speed × seconds
/// (or × beats with BPM sync).
#[derive(Debug, Clone, PartialEq)]
pub struct IsfGenerator {
    pub name: String,
    pub speed: IsfArg,
    pub bpm_sync: IsfArg,
}

/// Everything needed to build an ISF program.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IsfDesc {
    pub name: String,
    /// Fragment source, header included (it is a comment; keeps line numbers).
    pub fragment: String,
    /// Vertex source (`.vs` next to the `.fs`), if any.
    pub vertex: Option<String>,
    pub vars: Vec<IsfVar>,
    /// Image inputs and imported images, by GLSL name.
    pub images: Vec<(String, IsfImage)>,
    /// Empty = one pass to the output.
    pub passes: Vec<IsfPassDesc>,
    pub generators: Vec<IsfGenerator>,
}

// ------------------------------------------------------------------ translation

fn re(pat: &str) -> Regex {
    Regex::new(pat).expect("isf regex")
}

/// Pass-target names, in first-use order.
fn target_names(d: &IsfDesc) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in &d.passes {
        if let Some(t) = &p.target
            && !out.contains(t)
            && !d.images.iter().any(|(n, _)| n == t)
        {
            out.push(t.clone());
        }
    }
    out
}

/// GLSL float literal.
fn lit(v: f32) -> String {
    if v.is_finite() { format!("{v:.6}") } else { "0.0".into() }
}

/// Source cleanup shared by both stages; never changes line count.
fn common_fixups(src: &str) -> String {
    let s = re(r"(?m)^[ \t]*#version[^\n]*").replace_all(src, "");
    let s = re(r"(?m)^[ \t]*precision\s+\w+\s+\w+\s*;").replace_all(&s, "");
    let s = re(r"\b(highp|mediump|lowp)\s+").replace_all(&s, "");
    // ISF v1 names.
    let s = re(r"\bvv_FragNormCoord\b").replace_all(&s, "isf_FragNormCoord");
    let s = re(r"\bvv_vertShaderInit\b").replace_all(&s, "isf_vertShaderInit");
    s.into_owned()
}

fn has_main(src: &str) -> bool {
    re(r"\bvoid\s+main\s*\(").is_match(src)
}

fn rename_main(src: &str) -> String {
    re(r"\bvoid\s+main\s*\(")
        .replace_all(src, "void isf_main(")
        .into_owned()
}

/// Locations used by a varying of GLSL type `ty`.
fn location_span(ty: &str) -> u32 {
    match ty {
        "mat2" => 2,
        "mat3" => 3,
        "mat4" => 4,
        _ => 1,
    }
}

/// Columns of a matrix type (`mat3` → 3).
fn matrix_columns(ty: &str) -> Option<u32> {
    match ty {
        "mat2" => Some(2),
        "mat3" => Some(3),
        "mat4" => Some(4),
        _ => None,
    }
}

/// The fragment stage declared the column inputs of varying `name`.
fn fs_has_varying(fs: &str, name: &str) -> bool {
    fs.contains(&format!("in vec2 isf_vary_{name}_0;"))
        || fs.contains(&format!("in vec3 isf_vary_{name}_0;"))
        || fs.contains(&format!("in vec4 isf_vary_{name}_0;"))
}

fn needs_flat(ty: &str) -> bool {
    ty.starts_with("int") || ty.starts_with("ivec") || ty.starts_with("uint") || ty.starts_with("uvec")
}

/// Prelude shared by both stages: uniforms, textures, input globals, macros.
fn prelude(d: &IsfDesc, stage_vertex: bool) -> String {
    let mut s = String::from("#version 450\n");
    s += "layout(set = 0, binding = 0, std140) uniform IsfUniforms {\n";
    s += &format!("    vec4 isf_p[{}];\n", EFFECT_PARAMS / 4);
    s += &format!("    vec4 isf_g[{}];\n", ISF_GENERATORS / 4);
    s += "    vec4 DATE;\n    vec2 RENDERSIZE;\n    float TIME;\n    float TIMEDELTA;\n";
    s += "    int PASSINDEX;\n    int FRAMEINDEX;\n    float isf_beat;\n    float isf_pad;\n};\n";
    s += "layout(set = 0, binding = 1) uniform sampler isf_samp;\n";
    let names: Vec<String> = d.images.iter().map(|(n, _)| n.clone()).chain(target_names(d)).collect();
    for (i, n) in names.iter().enumerate() {
        s += &format!("layout(set = 0, binding = {}) uniform texture2D {n};\n", i + 2);
    }
    if stage_vertex {
        s += "layout(location = 0) out vec2 isf_FragNormCoord;\n";
    } else {
        s += "layout(location = 0) in vec2 isf_FragNormCoord;\n";
        s += "layout(location = 0) out vec4 isf_FragColor;\n";
        s += "vec4 isf_FragCoord;\n";
    }
    for v in &d.vars {
        s += &format!("{} {};\n", v.ty.glsl(), v.name);
    }
    for g in &d.generators {
        s += &format!("float {};\n", g.name);
    }
    s += "vec2 isf_flip(vec2 c) { return vec2(c.x, 1.0 - c.y); }\n";
    s += "#define IMG_SIZE(i) vec2(textureSize(sampler2D(i, isf_samp), 0))\n";
    if stage_vertex {
        s += "#define IMG_NORM_PIXEL(i, c) textureLod(sampler2D(i, isf_samp), isf_flip(c), 0.0)\n";
    } else {
        s += "#define IMG_NORM_PIXEL(i, c) texture(sampler2D(i, isf_samp), isf_flip(c))\n";
    }
    s += "#define IMG_PIXEL(i, c) IMG_NORM_PIXEL(i, (c) / IMG_SIZE(i))\n";
    s += "#define IMG_THIS_NORM_PIXEL(i) IMG_NORM_PIXEL(i, isf_FragNormCoord)\n";
    s += "#define IMG_THIS_PIXEL(i) IMG_NORM_PIXEL(i, isf_FragNormCoord)\n";
    for n in &names {
        s += &format!("#define _{n}_imgSize IMG_SIZE({n})\n");
        s += &format!("#define _{n}_imgRect vec4(0.0, 0.0, IMG_SIZE({n}))\n");
    }
    s
}

/// Statements filling the input globals from the uniforms.
fn assign_globals(d: &IsfDesc) -> String {
    const C: [&str; 4] = ["x", "y", "z", "w"];
    let at = |i: usize| format!("isf_p[{}].{}", i / 4, C[i % 4]);
    let mut s = String::new();
    for v in &d.vars {
        let val = match (v.slot, v.ty) {
            (Some(i), IsfType::Float) => at(i),
            (Some(i), IsfType::Bool) => format!("{} > 0.5", at(i)),
            (Some(i), IsfType::Int) => format!("int(round({}))", at(i)),
            (Some(i), IsfType::Point) => format!("vec2({}, {})", at(i), at(i + 1)),
            (Some(i), IsfType::Color) => format!("vec4({}, {}, {}, {})", at(i), at(i + 1), at(i + 2), at(i + 3)),
            (None, IsfType::Float) => lit(v.default[0]),
            (None, IsfType::Bool) => (if v.default[0] > 0.5 { "true" } else { "false" }).into(),
            (None, IsfType::Int) => format!("{}", v.default[0].round() as i32),
            (None, IsfType::Point) => format!("vec2({}, {})", lit(v.default[0]), lit(v.default[1])),
            (None, IsfType::Color) => format!(
                "vec4({}, {}, {}, {})",
                lit(v.default[0]),
                lit(v.default[1]),
                lit(v.default[2]),
                lit(v.default[3])
            ),
        };
        s += &format!("    {} = {val};\n", v.name);
    }
    for (i, g) in d.generators.iter().enumerate() {
        s += &format!("    {} = isf_g[{}].{};\n", g.name, i / 4, C[i % 4]);
    }
    s
}

/// Translated stages plus the prelude line counts (for error line numbers).
#[derive(Debug, Clone)]
pub struct Translated {
    pub fragment: String,
    pub vertex: String,
    pub fragment_prelude: usize,
    pub vertex_prelude: usize,
}

/// ISF source → GLSL 450 for naga.
pub fn translate(d: &IsfDesc) -> Translated {
    let images: Vec<String> = d.images.iter().map(|(n, _)| n.clone()).chain(target_names(d)).collect();
    // Direct texture calls on known images → the flipping macros.
    let tex_calls = |src: String| -> String {
        let mut s = src;
        for n in &images {
            let n = regex::escape(n);
            s = re(&format!(r"\btexture2DRect\s*\(\s*{n}\s*,"))
                .replace_all(&s, format!("IMG_PIXEL({n},"))
                .into_owned();
            s = re(&format!(r"\b(?:texture2D|texture)\s*\(\s*{n}\s*,"))
                .replace_all(&s, format!("IMG_NORM_PIXEL({n},"))
                .into_owned();
            s = re(&format!(r"\btextureSize\s*\(\s*{n}\s*,"))
                .replace_all(&s, format!("textureSize(sampler2D({n}, isf_samp),"))
                .into_owned();
        }
        s
    };

    // ---- vertex
    let mut vs_body = d.vertex.as_deref().map(common_fixups).unwrap_or_default();
    let material_vs = vs_body.contains("materialVsFunc") && !has_main(&vs_body);
    let material_fs = d.fragment.contains("materialColorForPixel") && !has_main(&d.fragment);
    if d.vertex.is_none() {
        vs_body = "void main() { isf_vertShaderInit(); }\n".into();
    } else if material_vs {
        vs_body += "\nvoid main() { isf_vertShaderInit(); materialVsFunc(isf_FragNormCoord); }\n";
    } else if !has_main(&vs_body) {
        vs_body += "\nvoid main() { isf_vertShaderInit(); }\n";
    }
    // Vertex attributes (MadMapper: gxl3d_Position / gxl3d_TexCoord0) → globals.
    let mut attrs: Vec<(String, String)> = Vec::new();
    let vs_body = re(r"(?m)^[ \t]*(?:attribute|in)[ \t]+(\w+)[ \t]+(\w+)[ \t]*;")
        .replace_all(&vs_body, |c: &regex::Captures| {
            attrs.push((c[1].to_owned(), c[2].to_owned()));
            format!("{} {};", &c[1], &c[2])
        })
        .into_owned();
    // Varyings: locations from 1 (0 = isf_FragNormCoord), in declaration order.
    let mut varyings: Vec<(String, String, u32)> = Vec::new(); // (type, name, location)
    let mut next_loc = 1u32;
    let vs_body = re(r"(?m)^[ \t]*(?:varying|out)[ \t]+(?:flat[ \t]+)?(\w+)[ \t]+(\w+)[ \t]*;")
        .replace_all(&vs_body, |c: &regex::Captures| {
            let (ty, name) = (c[1].to_owned(), c[2].to_owned());
            let loc = next_loc;
            next_loc += location_span(&ty);
            let flat = if needs_flat(&ty) { "flat " } else { "" };
            varyings.push((ty.clone(), name.clone(), loc));
            match matrix_columns(&ty) {
                // Matrices can't cross stages: a global, sent as column vectors.
                Some(n) => {
                    let cols: String = (0..n)
                        .map(|i| format!(" layout(location = {}) out vec{n} isf_vary_{name}_{i};", loc + i))
                        .collect();
                    format!("{ty} {name};{cols}")
                }
                None => format!("layout(location = {loc}) {flat}out {ty} {name};"),
            }
        })
        .into_owned();
    let vs_body = rename_main(&tex_calls(vs_body));
    let mut vs = prelude(d, true);
    vs += "void isf_vertShaderInit() {\n";
    vs += "    vec2 p = vec2(float((gl_VertexIndex << 1) & 2), float(gl_VertexIndex & 2));\n";
    vs += "    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);\n";
    vs += "    isf_FragNormCoord = p;\n}\n";
    let vertex_prelude = vs.lines().count();
    vs += &vs_body;
    vs += "\nvoid main() {\n";
    vs += "    vec2 isf_v = vec2(float((gl_VertexIndex << 1) & 2), float(gl_VertexIndex & 2));\n";
    for (ty, name) in &attrs {
        let val = if name.contains("Position") {
            "vec4(isf_v * 2.0 - 1.0, 0.0, 1.0)".to_owned()
        } else if name.contains("TexCoord") {
            "vec4(isf_v, 0.0, 1.0)".to_owned()
        } else {
            format!("{ty}(0.0)")
        };
        let val = match ty.as_str() {
            "vec2" => format!("({val}).xy"),
            "vec3" => format!("({val}).xyz"),
            _ => val,
        };
        vs += &format!("    {name} = {val};\n");
    }
    vs += &assign_globals(d);
    vs += "    isf_main();\n";
    for (ty, name, _) in &varyings {
        if let Some(n) = matrix_columns(ty) {
            for i in 0..n {
                vs += &format!("    isf_vary_{name}_{i} = {name}[{i}];\n");
            }
        }
    }
    vs += "}\n";

    // ---- fragment
    let mut fs_body = common_fixups(&d.fragment);
    if material_fs {
        fs_body += "\nvoid main() { gl_FragColor = materialColorForPixel(isf_FragNormCoord); }\n";
    }
    let fs_body = re(r"\bgl_FragColor\b").replace_all(&fs_body, "isf_FragColor");
    let fs_body = re(r"\bgl_FragCoord\b").replace_all(&fs_body, "isf_FragCoord");
    let fs_body = re(r"(?m)^[ \t]*(?:varying|in)[ \t]+(?:flat[ \t]+)?(\w+)[ \t]+(\w+)[ \t]*;")
        .replace_all(&fs_body, |c: &regex::Captures| {
            let (ty, name) = (&c[1], &c[2]);
            match varyings.iter().find(|v| v.1 == name) {
                Some((_, _, loc)) => match matrix_columns(ty) {
                    Some(n) => {
                        let cols: String = (0..n)
                            .map(|i| format!(" layout(location = {}) in vec{n} isf_vary_{name}_{i};", loc + i))
                            .collect();
                        format!("{ty} {name};{cols}")
                    }
                    None => {
                        let flat = if needs_flat(ty) { "flat " } else { "" };
                        format!("layout(location = {loc}) {flat}in {ty} {name};")
                    }
                },
                // No vertex shader writes it: a plain global (zero).
                None => format!("{ty} {name};"),
            }
        })
        .into_owned();
    let fs_body = rename_main(&tex_calls(fs_body));
    let mut fs = prelude(d, false);
    let fragment_prelude = fs.lines().count();
    fs += &fs_body;
    fs += "\nvoid main() {\n";
    fs += "    isf_FragCoord = vec4(gl_FragCoord.x, RENDERSIZE.y - gl_FragCoord.y, gl_FragCoord.z, gl_FragCoord.w);\n";
    fs += &assign_globals(d);
    for (ty, name, _) in &varyings {
        if let Some(n) = matrix_columns(ty)
            && fs_has_varying(&fs, name)
        {
            let cols: Vec<String> = (0..n).map(|i| format!("isf_vary_{name}_{i}")).collect();
            fs += &format!("    {name} = {ty}({});\n", cols.join(", "));
        }
    }
    fs += "    isf_FragColor = vec4(0.0);\n";
    fs += "    isf_main();\n}\n";
    Translated {
        fragment: fs,
        vertex: vs,
        fragment_prelude,
        vertex_prelude,
    }
}

/// Parse + validate one stage; errors name the user's line.
fn parse_stage(src: &str, stage: naga::ShaderStage, prelude: usize, label: &str) -> Result<naga::Module, String> {
    let line_of = |span: naga::Span| -> String {
        if !span.is_defined() {
            return String::new();
        }
        let l = span.location(src).line_number as usize;
        if l > prelude {
            format!("line {}: ", l - prelude)
        } else {
            String::new()
        }
    };
    let mut fe = naga::front::glsl::Frontend::default();
    let module = fe.parse(&naga::front::glsl::Options::from(stage), src).map_err(|e| {
        let msgs: Vec<String> = e
            .errors
            .iter()
            .take(3)
            .map(|er| format!("{label}{}{}", line_of(er.meta), er.kind))
            .collect();
        msgs.join("\n")
    })?;
    let mut v = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty());
    v.validate(&module).map_err(|e| {
        let at = e.spans().next().map(|(s, _)| line_of(*s)).unwrap_or_default();
        let mut msg = format!("{label}{at}{}", e.as_inner());
        let mut src_err: Option<&dyn std::error::Error> = std::error::Error::source(e.as_inner());
        while let Some(s) = src_err {
            msg += &format!(": {s}");
            src_err = s.source();
        }
        msg
    })?;
    Ok(module)
}

/// Translate, parse and validate without a GPU (tests, preflight).
pub fn check(d: &IsfDesc) -> Result<(naga::Module, naga::Module), String> {
    let t = translate(d);
    let vs = parse_stage(
        &t.vertex,
        naga::ShaderStage::Vertex,
        t.vertex_prelude,
        if d.vertex.is_some() { "vertex shader " } else { "" },
    )?;
    let fs = parse_stage(&t.fragment, naga::ShaderStage::Fragment, t.fragment_prelude, "")?;
    Ok((vs, fs))
}

// ------------------------------------------------------------------ size expressions

/// `$WIDTH/2`, `floor($HEIGHT*0.25)+1`, `max(1.0,$blurAmount)` … (None = unparsable).
pub fn eval_size(expr: &str, var: &dyn Fn(&str) -> Option<f32>) -> Option<f32> {
    struct P<'a> {
        s: &'a [u8],
        i: usize,
        var: &'a dyn Fn(&str) -> Option<f32>,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
        }
        fn eat(&mut self, c: u8) -> bool {
            self.ws();
            if self.s.get(self.i) == Some(&c) {
                self.i += 1;
                true
            } else {
                false
            }
        }
        fn ident(&mut self) -> String {
            let st = self.i;
            while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
                self.i += 1;
            }
            String::from_utf8_lossy(&self.s[st..self.i]).into_owned()
        }
        fn expr(&mut self) -> Option<f32> {
            let mut v = self.term()?;
            loop {
                if self.eat(b'+') {
                    v += self.term()?;
                } else if self.eat(b'-') {
                    v -= self.term()?;
                } else {
                    return Some(v);
                }
            }
        }
        fn term(&mut self) -> Option<f32> {
            let mut v = self.unary()?;
            loop {
                if self.eat(b'*') {
                    v *= self.unary()?;
                } else if self.eat(b'/') {
                    let d = self.unary()?;
                    v = if d == 0.0 { 0.0 } else { v / d };
                } else {
                    return Some(v);
                }
            }
        }
        fn unary(&mut self) -> Option<f32> {
            if self.eat(b'-') {
                return Some(-self.unary()?);
            }
            self.atom()
        }
        fn atom(&mut self) -> Option<f32> {
            self.ws();
            if self.eat(b'(') {
                let v = self.expr()?;
                return self.eat(b')').then_some(v);
            }
            if self.eat(b'$') {
                let name = self.ident();
                return (self.var)(&name);
            }
            let c = *self.s.get(self.i)?;
            if c.is_ascii_digit() || c == b'.' {
                let st = self.i;
                while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.') {
                    self.i += 1;
                }
                return std::str::from_utf8(&self.s[st..self.i]).ok()?.parse().ok();
            }
            if c.is_ascii_alphabetic() {
                let f = self.ident();
                if !self.eat(b'(') {
                    return None;
                }
                let mut args = vec![self.expr()?];
                while self.eat(b',') {
                    args.push(self.expr()?);
                }
                if !self.eat(b')') {
                    return None;
                }
                let a = args[0];
                let b = args.get(1).copied().unwrap_or(0.0);
                return Some(match f.as_str() {
                    "floor" => a.floor(),
                    "ceil" => a.ceil(),
                    "round" => a.round(),
                    "abs" => a.abs(),
                    "sqrt" => a.max(0.0).sqrt(),
                    "max" => a.max(b),
                    "min" => a.min(b),
                    "pow" => a.powf(b),
                    _ => return None,
                });
            }
            None
        }
    }
    let mut p = P {
        s: expr.as_bytes(),
        i: 0,
        var,
    };
    let v = p.expr()?;
    p.ws();
    (p.i == p.s.len()).then_some(v)
}

// ------------------------------------------------------------------ GPU programs

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct IsfUniform {
    p: [f32; EFFECT_PARAMS],
    g: [f32; ISF_GENERATORS],
    date: [f32; 4],
    rendersize: [f32; 2],
    time: f32,
    timedelta: f32,
    passindex: i32,
    frameindex: i32,
    beat: f32,
    _pad: f32,
}

/// Slot `i` of the texture bindings (binding 2 + i).
#[derive(Debug, Clone)]
enum Bound {
    Image(IsfImage),
    /// Index into the instance's targets.
    Target(usize),
}

pub(crate) struct IsfProgram {
    desc: IsfDesc,
    bgl: wgpu::BindGroupLayout,
    /// 8-bit and float target pipelines.
    pipe8: wgpu::RenderPipeline,
    pipe_f: wgpu::RenderPipeline,
    bound: Vec<Bound>,
    targets: Vec<String>,
    /// Uploaded imported images, by binding slot.
    imported: HashMap<usize, wgpu::TextureView>,
}

/// Compiled ISF programs, shared by every compositor of a device.
#[derive(Default)]
pub struct IsfLibrary {
    programs: RwLock<HashMap<u32, Arc<IsfProgram>>>,
}

// wgpu objects are Send + Sync on native; the library is shared across compositors.
impl std::fmt::Debug for IsfLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.programs.read().map_or(0, |p| p.len());
        write!(f, "IsfLibrary({n} programs)")
    }
}

impl IsfLibrary {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Build the program for `kind`; Err = message for the user (nothing installed).
    pub fn compile(&self, device: &wgpu::Device, queue: &wgpu::Queue, kind: u32, d: &IsfDesc) -> Result<(), String> {
        if let Some(p) = self.programs.read().ok().and_then(|p| p.get(&kind).cloned())
            && p.desc == *d
        {
            return Ok(());
        }
        let (vs, fs) = check(d)?;
        let label = format!("isf {}", d.name);
        let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&label),
            source: wgpu::ShaderSource::Naga(Cow::Owned(vs)),
        });
        let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&label),
            source: wgpu::ShaderSource::Naga(Cow::Owned(fs)),
        });
        let targets = target_names(d);
        let bound: Vec<Bound> = d
            .images
            .iter()
            .map(|(_, i)| Bound::Image(i.clone()))
            .chain((0..targets.len()).map(Bound::Target))
            .collect();
        let mut entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ];
        for i in 0..bound.len() {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: 2 + i as u32,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
        }
        // Pipeline creation can still fail in the backend: catch it.
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&label),
            entries: &entries,
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&label),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipe = |format| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &vs,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &fs,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipe8 = pipe(COMP_FORMAT);
        let pipe_f = pipe(FLOAT_FORMAT);
        if let Some(e) = pollster_block(scope.pop()) {
            return Err(format!("GPU: {e}"));
        }
        let mut imported = HashMap::new();
        for (i, b) in bound.iter().enumerate() {
            if let Bound::Image(IsfImage::Imported(img)) = b {
                imported.insert(i, upload_rgba(device, queue, img));
            }
        }
        let prog = IsfProgram {
            desc: d.clone(),
            bgl,
            pipe8,
            pipe_f,
            bound,
            targets,
            imported,
        };
        if let Ok(mut p) = self.programs.write() {
            p.insert(kind, Arc::new(prog));
        }
        Ok(())
    }

    pub fn remove(&self, kind: u32) {
        if let Ok(mut p) = self.programs.write() {
            p.remove(&kind);
        }
    }

    pub fn contains(&self, kind: u32) -> bool {
        self.programs.read().is_ok_and(|p| p.contains_key(&kind))
    }

    pub(crate) fn get(&self, kind: u32) -> Option<Arc<IsfProgram>> {
        self.programs.read().ok()?.get(&kind).cloned()
    }
}

/// Wait for a wgpu future that resolves right away on native (error scopes).
fn pollster_block<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    fn noop(_: *const ()) {}
    fn clone(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

fn upload_rgba(device: &wgpu::Device, queue: &wgpu::Queue, img: &FxImage) -> wgpu::TextureView {
    let ext = wgpu::Extent3d {
        width: img.width.max(1),
        height: img.height.max(1),
        depth_or_array_layers: 1,
    };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("isf image"),
        size: ext,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let bpp = if img.rgba { 4 } else { 1 };
    if img.rgba && img.width > 0 && img.data.len() as u64 >= u64::from(img.width) * u64::from(img.height) * bpp {
        queue.write_texture(
            tex.as_image_copy(),
            &img.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(img.width * 4),
                rows_per_image: None,
            },
            ext,
        );
    }
    tex.create_view(&Default::default())
}

// ------------------------------------------------------------------ per-instance state + run

struct Target {
    /// [read, write]; swapped after each pass that writes it.
    tex: [wgpu::Texture; 2],
    views: [wgpu::TextureView; 2],
    float: bool,
}

#[derive(Default)]
pub(crate) struct IsfState {
    targets: Vec<Option<Target>>,
    gens: [f32; ISF_GENERATORS],
    frame: i32,
    last_time: Option<f32>,
    last_beat: f32,
}

fn make_target(device: &wgpu::Device, w: u32, h: u32, float: bool) -> Target {
    let mk = || {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("isf target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: if float { FLOAT_FORMAT } else { COMP_FORMAT },
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    };
    let tex = [mk(), mk()];
    let views = [
        tex[0].create_view(&Default::default()),
        tex[1].create_view(&Default::default()),
    ];
    Target { tex, views, float }
}

/// Shared GPU bits of the ISF runner (one per effect pass).
pub(crate) struct IsfRunner {
    sampler: wgpu::Sampler,
    pub(crate) out: RefCell<Option<(wgpu::Texture, wgpu::TextureView)>>,
    states: RefCell<HashMap<i64, IsfState>>,
    fft: RefCell<Option<(wgpu::Texture, wgpu::TextureView, [f32; 32])>>,
    pub(crate) library: RefCell<Option<Arc<IsfLibrary>>>,
}

/// Seconds since 1970 → DATE (year, month, day, seconds of the day), UTC.
fn date_now() -> [f32; 4] {
    static START: OnceLock<std::time::SystemTime> = OnceLock::new();
    let _ = START.get_or_init(std::time::SystemTime::now);
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400) as f32;
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    [y as f32, m as f32, d as f32, sod]
}

impl IsfRunner {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        Self {
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("isf"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            out: RefCell::new(None),
            states: RefCell::new(HashMap::new()),
            fft: RefCell::new(None),
            library: RefCell::new(None),
        }
    }

    pub(crate) fn set_fft(&self, device: &wgpu::Device, queue: &wgpu::Queue, bands: &[f32; 32]) {
        let mut f = self.fft.borrow_mut();
        if f.is_none() {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("isf fft"),
                size: wgpu::Extent3d {
                    width: 32,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            *f = Some((tex, view, [f32::NAN; 32]));
        }
        let (tex, _, last) = f.as_mut().unwrap();
        if last != bands {
            *last = *bands;
            let data: Vec<u8> = bands
                .iter()
                .flat_map(|v| {
                    let b = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                    [b, b, b, 255]
                })
                .collect();
            queue.write_texture(
                tex.as_image_copy(),
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(32 * 4),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: 32,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    pub(crate) fn retain(&self, alive: &dyn Fn(i64) -> bool) {
        self.states.borrow_mut().retain(|id, _| alive(*id));
    }

    /// Run the program of `kind` over `input` into `self.out` (input size).
    /// False = no such program (the effect is skipped).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        kind: u32,
        id: i64,
        params: &[f32; EFFECT_PARAMS],
        input: &wgpu::TextureView,
        size: wgpu::Extent3d,
        empty: &wgpu::TextureView,
        time: f32,
        beat: f32,
    ) -> bool {
        let Some(prog) = self.library.borrow().as_ref().and_then(|l| l.get(kind)) else {
            return false;
        };
        {
            let mut out = self.out.borrow_mut();
            if out.as_ref().is_none_or(|(t, _)| t.size() != size) {
                let t = target_texture(device, "isf out", size.width, size.height);
                let v = t.create_view(&Default::default());
                *out = Some((t, v));
            }
        }
        let mut states = self.states.borrow_mut();
        let st = states.entry(id).or_default();
        if st.targets.len() != prog.targets.len() {
            st.targets = (0..prog.targets.len()).map(|_| None).collect();
        }
        // Time + generators.
        let dt = st.last_time.map_or(0.0, |t| (time - t).max(0.0));
        let dbeat = if st.last_time.is_some() {
            (beat - st.last_beat).max(0.0)
        } else {
            0.0
        };
        st.last_time = Some(time);
        st.last_beat = beat;
        let arg = |a: IsfArg| match a {
            IsfArg::Const(v) => v,
            IsfArg::Slot(i) => params.get(i).copied().unwrap_or(0.0),
        };
        for (i, g) in prog.desc.generators.iter().take(ISF_GENERATORS).enumerate() {
            let step = if arg(g.bpm_sync) > 0.5 { dbeat } else { dt };
            st.gens[i] += arg(g.speed) * step;
        }
        let var = |name: &str| -> Option<f32> {
            match name {
                "WIDTH" => Some(size.width as f32),
                "HEIGHT" => Some(size.height as f32),
                _ => {
                    let v = prog.desc.vars.iter().find(|v| v.name == name)?;
                    Some(v.slot.map_or(v.default[0], |s| params[s]))
                }
            }
        };
        let passes: Vec<IsfPassDesc> = if prog.desc.passes.is_empty() {
            vec![IsfPassDesc::default()]
        } else {
            prog.desc.passes.clone()
        };
        let date = date_now();
        let fft = self.fft.borrow();
        let out_ref = self.out.borrow();
        let out_view = &out_ref.as_ref().unwrap().1;
        let last = passes.len() - 1;
        for (pi, pass) in passes.iter().enumerate() {
            let ti = pass
                .target
                .as_ref()
                .and_then(|t| prog.targets.iter().position(|n| n == t));
            // Size of this pass.
            let dim = |e: &Option<String>, full: u32| -> u32 {
                e.as_deref()
                    .and_then(|e| eval_size(e, &var))
                    .map_or(full, |v| v.round().clamp(1.0, 8192.0) as u32)
            };
            let (w, h) = if ti.is_some() {
                (dim(&pass.width, size.width), dim(&pass.height, size.height))
            } else {
                (size.width, size.height)
            };
            // Draw into: the last pass → output (and its target), others → their target.
            let mut dests: Vec<Option<usize>> = Vec::new();
            if pi == last {
                dests.push(None);
            }
            if let Some(ti) = ti {
                let stale = st.targets[ti]
                    .as_ref()
                    .is_none_or(|t| t.tex[0].width() != w || t.tex[0].height() != h || t.float != pass.float);
                if stale {
                    st.targets[ti] = Some(make_target(device, w, h, pass.float));
                }
                dests.push(Some(ti));
            }
            for dest in dests {
                let rsize = match dest {
                    None => [size.width as f32, size.height as f32],
                    Some(_) => [w as f32, h as f32],
                };
                let u = IsfUniform {
                    p: *params,
                    g: st.gens,
                    date,
                    rendersize: rsize,
                    time,
                    timedelta: dt,
                    passindex: pi as i32,
                    frameindex: st.frame,
                    beat,
                    _pad: 0.0,
                };
                let buf = uniform_buffer(device, "isf", std::mem::size_of::<IsfUniform>());
                queue.write_buffer(&buf, 0, bytemuck::bytes_of(&u));
                let mut entries = vec![
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ];
                for (bi, b) in prog.bound.iter().enumerate() {
                    let view = match b {
                        Bound::Image(IsfImage::Input) => input,
                        Bound::Image(IsfImage::AudioFft) => fft.as_ref().map_or(empty, |f| &f.1),
                        Bound::Image(IsfImage::Imported(_)) => prog.imported.get(&bi).unwrap_or(empty),
                        Bound::Image(_) => empty,
                        Bound::Target(t) => st.targets[*t].as_ref().map_or(empty, |t| &t.views[0]),
                    };
                    entries.push(wgpu::BindGroupEntry {
                        binding: 2 + bi as u32,
                        resource: wgpu::BindingResource::TextureView(view),
                    });
                }
                let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("isf"),
                    layout: &prog.bgl,
                    entries: &entries,
                });
                let (view, pipe) = match dest {
                    None => (out_view, &prog.pipe8),
                    Some(t) => {
                        let tg = st.targets[t].as_ref().unwrap();
                        (&tg.views[1], if tg.float { &prog.pipe_f } else { &prog.pipe8 })
                    }
                };
                let mut rp = crate::clear_pass(enc, view, wgpu::Color::TRANSPARENT);
                rp.set_pipeline(pipe);
                rp.set_bind_group(0, &bg, &[]);
                rp.draw(0..3, 0..1);
            }
            // The freshly written copy becomes the one later passes read.
            if let Some(ti) = ti
                && let Some(t) = st.targets[ti].as_mut()
            {
                t.tex.swap(0, 1);
                t.views.swap(0, 1);
            }
        }
        st.frame = st.frame.wrapping_add(1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(name: &str, ty: IsfType, slot: usize) -> IsfVar {
        IsfVar {
            name: name.into(),
            ty,
            slot: Some(slot),
            default: [0.0; 4],
        }
    }

    fn effect(fs: &str) -> IsfDesc {
        IsfDesc {
            name: "t".into(),
            fragment: fs.into(),
            images: vec![("inputImage".into(), IsfImage::Input)],
            ..Default::default()
        }
    }

    #[test]
    fn plain_isf_effect_compiles() {
        let mut d = effect(
            "/*{ \"INPUTS\": [] }*/\nvoid main() {\n  vec4 c = IMG_THIS_PIXEL(inputImage);\n  gl_FragColor = vec4(1.0 - c.rgb * amount, c.a);\n}\n",
        );
        d.vars.push(var("amount", IsfType::Float, 0));
        check(&d).unwrap();
    }

    #[test]
    fn glsl1_habits_compile() {
        let mut d = effect(concat!(
            "#version 120\nprecision mediump float;\n",
            "varying vec2 shifted;\n",
            "const float PI = 3.14159;\n",
            "void main() {\n",
            "  highp float a = 1;\n",
            "  vec2 px = gl_FragCoord.xy + vec2(mode, 0);\n",
            "  vec4 c = texture2D(inputImage, vv_FragNormCoord) + IMG_PIXEL(inputImage, px) + texture2DRect(inputImage, px);\n",
            "  if (flip) c.rgb = c.bgr;\n",
            "  gl_FragColor = c * tint * a + vec4(shifted, pos);\n",
            "}\n"
        ));
        d.vars = vec![
            var("mode", IsfType::Int, 0),
            var("flip", IsfType::Bool, 1),
            var("tint", IsfType::Color, 2),
            var("pos", IsfType::Point, 6),
        ];
        check(&d).unwrap();
    }

    #[test]
    fn errors_name_the_users_line() {
        let d = effect("/*{\n}*/\nvoid main() {\n  gl_FragColor = vec4(nope);\n}\n");
        let e = check(&d).unwrap_err();
        assert!(e.starts_with("line 4:"), "{e}");
    }

    #[test]
    fn vertex_shader_varyings_and_materials() {
        // ISF .vs with varyings.
        let mut d = effect(
            "varying vec2 left;\nvarying mat3 m;\nvoid main() { gl_FragColor = IMG_NORM_PIXEL(inputImage, (m * vec3(left, 1.0)).xy); }\n",
        );
        d.vertex = Some(
            "varying vec2 left;\nout mat3 m;\nvoid main() { isf_vertShaderInit(); left = isf_FragNormCoord - 0.1; m = mat3(1.0); }\n"
                .into(),
        );
        check(&d).unwrap();
        // MadMapper material: no main, generator, materialVsFunc, gxl3d attributes.
        let d = IsfDesc {
            name: "mat".into(),
            fragment: "in float adj;\nvec4 materialColorForPixel(vec2 texCoord) { return vec4(texCoord, adj * mat_time, 1); }\n"
                .into(),
            vertex: Some("in vec4 gxl3d_Position;\nout float adj;\nvoid materialVsFunc(vec2 uv) { adj = mod(uv.x + 1, 1); }\n".into()),
            generators: vec![IsfGenerator {
                name: "mat_time".into(),
                speed: IsfArg::Const(1.0),
                bpm_sync: IsfArg::Const(0.0),
            }],
            ..Default::default()
        };
        check(&d).unwrap();
    }

    #[test]
    fn multipass_targets_are_textures() {
        let mut d = effect(concat!(
            "void main() {\n",
            "  if (PASSINDEX == 0) gl_FragColor = IMG_THIS_PIXEL(inputImage);\n",
            "  else gl_FragColor = mix(IMG_THIS_PIXEL(inputImage), IMG_THIS_PIXEL(bufferA), 0.9) + vec4(IMG_SIZE(bufferA), 0.0, 0.0) * 0.0;\n",
            "}\n"
        ));
        d.passes = vec![
            IsfPassDesc {
                target: Some("bufferA".into()),
                persistent: true,
                float: true,
                width: Some("$WIDTH/2".into()),
                height: None,
            },
            IsfPassDesc::default(),
        ];
        check(&d).unwrap();
        assert_eq!(target_names(&d), vec!["bufferA".to_string()]);
    }

    #[test]
    fn size_expressions() {
        let v = |n: &str| match n {
            "WIDTH" => Some(1920.0),
            "HEIGHT" => Some(1080.0),
            "blur" => Some(4.0),
            _ => None,
        };
        assert_eq!(eval_size("$WIDTH/2", &v), Some(960.0));
        assert_eq!(eval_size("floor($HEIGHT * 0.25) + 1", &v), Some(271.0));
        assert_eq!(eval_size("max(1.0, $WIDTH / $blur)", &v), Some(480.0));
        assert_eq!(eval_size("-($blur - 6)", &v), Some(2.0));
        assert_eq!(eval_size("$nope", &v), None);
        assert_eq!(eval_size("$WIDTH +", &v), None);
    }

    #[test]
    fn constants_match_core() {
        assert_eq!(ISF_KIND_BASE, lavmfx_core::isf::KIND_BASE);
        assert_eq!(ISF_GENERATORS, lavmfx_core::isf::MAX_GENERATORS);
        assert_eq!(EFFECT_PARAMS, lavmfx_core::effects::SHADER_PARAMS);
    }

    #[test]
    fn date_is_plausible() {
        let d = date_now();
        assert!(d[0] >= 2024.0 && (1.0..=12.0).contains(&d[1]) && (1.0..=31.0).contains(&d[2]));
        assert!((0.0..86_400.0).contains(&d[3]));
    }
}
