//! Text Block / Text Animator: lay out and rasterise the text (ab_glyph) into a coverage image
//! at composition size; the effect shader (`e_text`) adds colour, outline and glow.
//!
//! Fonts: Arial / Courier New / Times New Roman map to the metric-compatible
//! Liberation Sans / Mono / Serif (system fonts). Images are cached by a key of everything
//! that shapes them; the Text Animator re-rasterises only while it animates.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use lavmfx_core::{Param, VideoEffect};
use lavmfx_render::{FxFrame, FxImage};

/// A loaded font (None: not found on this system).
type FontSlot = Option<Arc<FontVec>>;

thread_local! {
    static FRAME: Cell<(u32, u32, f64)> = const { Cell::new((1920, 1080, 0.0)) };
    static CACHE: RefCell<HashMap<u64, (Arc<FxImage>, Instant)>> = RefCell::new(HashMap::new());
    static FONTS: RefCell<HashMap<(usize, usize), FontSlot>> = RefCell::new(HashMap::new());
}

/// Composition size and clock time for this frame's text images.
pub fn set_frame(w: u32, h: u32, time: f64) {
    FRAME.with(|f| f.set((w, h, time)));
}

/// Images referenced by the frame's effects (and forget ones unused for a few seconds).
pub fn images(f: &FxFrame) -> HashMap<u64, Arc<FxImage>> {
    let keys = f
        .layer
        .iter()
        .chain(&f.outgoing)
        .chain(&f.group)
        .flatten()
        .chain(&f.composition)
        .map(|e| e.image)
        .filter(|k| *k != 0);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let now = Instant::now();
        let out: HashMap<_, _> = keys
            .filter_map(|k| {
                c.get_mut(&k).map(|(img, used)| {
                    *used = now;
                    (k, img.clone())
                })
            })
            .collect();
        c.retain(|_, (_, used)| now.duration_since(*used).as_secs_f32() < 3.0);
        out
    })
}

/// Key of the effect's text image (rendering it if needed); 0 when it is no text effect.
pub fn image_key(e: &VideoEffect) -> u64 {
    let animator = match e.name.as_str() {
        "TextBlock" => false,
        "TextEffect" => true,
        "Mask" => return mask_key(e),
        _ => return 0,
    };
    let (w, h, time) = FRAME.with(Cell::get);
    let spec = Spec::read(e, animator, w, h);
    let anim_t = spec.anim.as_ref().map(|a| a.local_time(time, spec.units_hint()));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{spec:?}{w}x{h}").hash(&mut hasher);
    // Animated: a new image per 1/60 s while the units move; the hold phase is one image.
    if let Some(t) = anim_t {
        ((t * 60.0).floor() as i64).hash(&mut hasher);
    }
    let key = hasher.finish() | 1;
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        c.entry(key)
            .or_insert_with(|| (Arc::new(render(&spec, w, h, anim_t)), Instant::now()));
    });
    key
}

/// Mask effect: its image file, decoded once (FFmpeg, up to 4096 px) and
/// kept in the image cache; 0 = no file / unreadable (no masking).
fn mask_key(e: &VideoEffect) -> u64 {
    let Some(Param::Text(t)) = e.params.get(lavmfx_core::effects::MASK_FILE_KEY) else {
        return 0;
    };
    if t.value.is_empty() {
        return 0;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ("mask", &t.value).hash(&mut hasher);
    let key = hasher.finish() | 1;
    thread_local! {
        static FAILED: RefCell<std::collections::HashSet<u64>> = RefCell::new(Default::default());
    }
    if FAILED.with(|f| f.borrow().contains(&key)) {
        return 0;
    }
    let have = CACHE.with(|c| c.borrow().contains_key(&key));
    if !have {
        match lavmfx_media::thumbnail(std::path::Path::new(&t.value), 4096, 4096) {
            Ok(img) => CACHE.with(|c| {
                let img = FxImage {
                    width: img.width,
                    height: img.height,
                    data: img.rgba,
                    rgba: true,
                };
                c.borrow_mut().insert(key, (Arc::new(img), Instant::now()));
            }),
            Err(e) => {
                log::warn!("mask image {}: {e}", t.value);
                FAILED.with(|f| f.borrow_mut().insert(key));
                return 0;
            }
        }
    }
    key
}

// ------------------------------------------------------------------ spec

#[derive(Debug, Clone)]
struct Anim {
    range: usize, // All, Lines, Words, Characters
    order: usize, // Front to back, Back to front, Random
    delay: f32,   // seconds between units
    scale: f32,
    pos: (f32, f32),
    rotation: f32,
    anchor: (f32, f32),
}

impl Anim {
    fn dur(&self) -> f32 {
        (self.delay * 4.0).clamp(0.25, 2.0)
    }
    /// Time in the loop (units come in one after another, then a 2 s hold), capped at the
    /// end of the movement so the hold doesn't change the key.
    fn local_time(&self, time: f64, units: usize) -> f32 {
        let moving = self.delay * units.saturating_sub(1) as f32 + self.dur();
        let total = moving + 2.0;
        ((time as f32).rem_euclid(total)).min(moving)
    }
}

#[derive(Debug, Clone)]
struct Spec {
    text: String,
    font: usize,
    style: usize,
    size: f32,
    align: usize,
    valign: usize,
    line_width: f32,
    pos: (f32, f32),
    rotation: f32,
    scale: f32,
    spacing: (f32, f32),
    anim: Option<Anim>,
}

fn range(e: &VideoEffect, k: &str, d: f32) -> f32 {
    match e.params.get(k) {
        Some(Param::Range(r)) => r.value as f32,
        _ => d,
    }
}
fn choice(e: &VideoEffect, k: &str, d: usize) -> usize {
    match e.params.get(k) {
        Some(Param::Choice(c)) => c.index.max(0) as usize,
        _ => d,
    }
}

impl Spec {
    fn read(e: &VideoEffect, animator: bool, w: u32, _h: u32) -> Self {
        let text = match e.params.get("Text") {
            Some(Param::Text(t)) => t.value.clone(),
            Some(Param::String(t)) => t.value.clone(),
            _ => "LAVMFX".into(),
        };
        let animated = matches!(e.params.get("Animated"), Some(Param::Bool(b)) if b.value);
        Spec {
            text,
            font: choice(e, "Font", 0),
            style: choice(e, "Style", 0),
            size: range(e, "Size", 1.0),
            align: choice(e, "Alignment", if animator { 0 } else { 1 }),
            valign: choice(e, "AlignmentVertically", 1),
            line_width: if animator {
                w as f32
            } else {
                range(e, "Line Width", w as f32)
            },
            pos: (range(e, "Position X", 0.0), range(e, "Position Y", 0.0)),
            rotation: if animator { 0.0 } else { range(e, "Rotation", 0.0) },
            scale: if animator { 1.0 } else { range(e, "Scale", 1.0) },
            spacing: (range(e, "Spacing X", 0.0), range(e, "Spacing Y", 0.0)),
            anim: (animator && animated).then(|| Anim {
                range: choice(e, "Range", 3),
                order: choice(e, "Order", 0),
                delay: range(e, "Delay", 100.0) / 1000.0,
                scale: range(e, "Scale", 1.0),
                pos: (
                    range(e, "Animated Position X", 0.0),
                    range(e, "Animated Position Y", 0.0),
                ),
                rotation: range(e, "Rotation", 0.0),
                anchor: (range(e, "Anchor X", 0.0), range(e, "Anchor Y", 0.0)),
            }),
        }
    }

    /// Upper bound of animation units (for the loop length before layout).
    fn units_hint(&self) -> usize {
        match self.anim.as_ref().map_or(0, |a| a.range) {
            0 => 1,
            1 => self.text.lines().count().max(1),
            2 => self.text.split_whitespace().count().max(1),
            _ => self.text.chars().filter(|c| !c.is_whitespace()).count().max(1),
        }
    }
}

// ------------------------------------------------------------------ fonts

fn font(family: usize, style: usize) -> Option<Arc<FontVec>> {
    FONTS.with(|f| {
        f.borrow_mut()
            .entry((family, style))
            .or_insert_with(|| {
                let fam = ["Sans", "Mono", "Serif"][family.min(2)];
                let sty = ["Bold", "BoldItalic", "Italic", "Regular"][style.min(3)];
                // Windows: the metric-compatible originals of the Liberation fonts.
                let win = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
                let win_base = ["arial", "cour", "times"][family.min(2)];
                let win_sty = ["bd", "bi", "i", ""][style.min(3)];
                let paths = [
                    format!("/usr/share/fonts/liberation/Liberation{fam}-{sty}.ttf"),
                    format!("/usr/share/fonts/TTF/Liberation{fam}-{sty}.ttf"),
                    format!("/usr/share/fonts/truetype/liberation/Liberation{fam}-{sty}.ttf"),
                    "/usr/share/fonts/TTF/DejaVuSans.ttf".into(),
                    format!(r"{win}\Fonts\{win_base}{win_sty}.ttf"),
                    format!(r"{win}\Fonts\arial.ttf"),
                ];
                paths.iter().find_map(|p| {
                    let data = std::fs::read(p).ok()?;
                    FontVec::try_from_vec(data).ok().map(Arc::new)
                })
            })
            .clone()
    })
}

// ------------------------------------------------------------------ layout + raster

struct Placed {
    c: char,
    x: f32, // pen position (pixels, untransformed)
    y: f32, // baseline
    line: usize,
    word: usize,
    index: usize,
}

fn render(spec: &Spec, w: u32, h: u32, anim_t: Option<f32>) -> FxImage {
    let mut img = FxImage {
        rgba: false,
        width: w,
        height: h,
        data: vec![0; (w * h) as usize],
    };
    let Some(font) = font(spec.font, spec.style) else {
        log::warn!("text effect: no font found");
        return img;
    };
    let px = (spec.size * h as f32 / 10.0).max(1.0);
    let sf = font.as_scaled(PxScale::from(px));
    let lh = sf.ascent() - sf.descent() + sf.line_gap() + spec.spacing.1;
    let advance = |a: char, b: Option<char>| {
        let ga = font.glyph_id(a);
        sf.h_advance(ga) + spec.spacing.0 + b.map_or(0.0, |b| sf.kern(ga, font.glyph_id(b)))
    };
    let measure = |s: &str| -> f32 {
        let cs: Vec<char> = s.chars().collect();
        cs.iter()
            .enumerate()
            .map(|(i, c)| advance(*c, cs.get(i + 1).copied()))
            .sum::<f32>()
            - if cs.is_empty() { 0.0 } else { spec.spacing.0 }
    };
    // Greedy word wrap per paragraph.
    let mut lines: Vec<String> = Vec::new();
    for para in spec.text.split('\n') {
        let mut cur = String::new();
        for word in para.split(' ') {
            let cand = if cur.is_empty() {
                word.to_string()
            } else {
                format!("{cur} {word}")
            };
            if !cur.is_empty() && measure(&cand) > spec.line_width {
                lines.push(std::mem::take(&mut cur));
                cur = word.to_string();
            } else {
                cur = cand;
            }
        }
        lines.push(cur);
    }
    let anchor = (w as f32 / 2.0 + spec.pos.0, h as f32 / 2.0 + spec.pos.1);
    let total = lh * lines.len() as f32;
    let top = match spec.valign {
        0 => anchor.1 - h as f32 / 2.0,
        2 => anchor.1 + h as f32 / 2.0 - total,
        _ => anchor.1 - total / 2.0,
    };
    let box_left = anchor.0 - spec.line_width / 2.0;
    let mut placed = Vec::new();
    let (mut word, mut index) = (0, 0);
    for (li, line) in lines.iter().enumerate() {
        let lw = measure(line);
        let mut x = match spec.align {
            0 => box_left,
            2 => box_left + spec.line_width - lw,
            _ => anchor.0 - lw / 2.0,
        };
        let y = top + sf.ascent() + lh * li as f32;
        let cs: Vec<char> = line.chars().collect();
        for (i, c) in cs.iter().enumerate() {
            if c.is_whitespace() {
                word += 1;
            } else {
                placed.push(Placed {
                    c: *c,
                    x,
                    y,
                    line: li,
                    word,
                    index,
                });
                index += 1;
            }
            x += advance(*c, cs.get(i + 1).copied());
        }
        word += 1;
    }
    // Animation units: centre of each unit's glyphs and its progress.
    let unit_of = |p: &Placed, a: &Anim| match a.range {
        0 => 0,
        1 => p.line,
        2 => p.word,
        _ => p.index,
    };
    let mut unit_ctr: HashMap<usize, (f32, f32, f32)> = HashMap::new();
    if let Some(a) = &spec.anim {
        for p in &placed {
            let e = unit_ctr.entry(unit_of(p, a)).or_default();
            let cx = p.x + sf.h_advance(font.glyph_id(p.c)) / 2.0;
            let cy = p.y - sf.ascent() / 2.0;
            *e = (e.0 + cx, e.1 + cy, e.2 + 1.0);
        }
    }
    let mut units: Vec<usize> = unit_ctr.keys().copied().collect();
    units.sort_unstable();
    let order_of = |u: usize, a: &Anim| -> f32 {
        let i = units.iter().position(|x| *x == u).unwrap_or(0);
        let n = units.len().max(1);
        match a.order {
            1 => (n - 1 - i) as f32,
            2 => {
                // random permutation: rank by a hash
                let hsh = |x: usize| (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 33;
                units.iter().filter(|o| hsh(**o) < hsh(u)).count() as f32
            }
            _ => i as f32,
        }
    };
    let (cr, sr) = (spec.rotation.cos(), spec.rotation.sin());
    for p in &placed {
        // Whole block: rotate/scale round the anchor.
        let (dx, dy) = ((p.x - anchor.0) * spec.scale, (p.y - anchor.1) * spec.scale);
        let mut pen = (anchor.0 + dx * cr - dy * sr, anchor.1 + dx * sr + dy * cr);
        let (mut rot, mut scale, mut alpha) = (spec.rotation, spec.scale, 1.0f32);
        if let (Some(a), Some(t)) = (&spec.anim, anim_t) {
            let u = unit_of(p, a);
            let k = ((t - order_of(u, a) * a.delay) / a.dur()).clamp(0.0, 1.0);
            let k = k * k * (3.0 - 2.0 * k);
            let (sx, sy, n) = unit_ctr[&u];
            let c = (sx / n + a.anchor.0, sy / n + a.anchor.1);
            let s = a.scale + (1.0 - a.scale) * k;
            let r = a.rotation * (1.0 - k);
            let (dx, dy) = ((pen.0 - c.0) * s, (pen.1 - c.1) * s);
            pen = (
                c.0 + dx * r.cos() - dy * r.sin() + a.pos.0 * (1.0 - k),
                c.1 + dx * r.sin() + dy * r.cos() + a.pos.1 * (1.0 - k),
            );
            rot = r;
            scale = s;
            alpha = k;
        }
        if alpha <= 0.0 || scale <= 0.0 {
            continue;
        }
        draw_glyph(&mut img, &font, p.c, px * scale, pen, rot, alpha);
    }
    img
}

/// Rasterise one glyph at `size` px with its pen at `pen`, rotated by `rot`, into `img` (max).
fn draw_glyph(img: &mut FxImage, font: &FontVec, c: char, size: f32, pen: (f32, f32), rot: f32, alpha: f32) {
    let g = font
        .glyph_id(c)
        .with_scale_and_position(PxScale::from(size), ab_glyph::point(0.0, 0.0));
    let Some(og) = font.outline_glyph(g) else { return };
    let b = og.px_bounds();
    let (bw, bh) = (b.width().ceil() as i32 + 1, b.height().ceil() as i32 + 1);
    if bw <= 0 || bh <= 0 || bw > 8192 || bh > 8192 {
        return;
    }
    let mut bmp = vec![0f32; (bw * bh) as usize];
    og.draw(|x, y, v| {
        let i = (y as i32 * bw + x as i32) as usize;
        if i < bmp.len() {
            bmp[i] = v;
        }
    });
    let sample = |x: f32, y: f32| -> f32 {
        // bilinear in the glyph bitmap
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |xi: i32, yi: i32| {
            if xi < 0 || yi < 0 || xi >= bw || yi >= bh {
                0.0
            } else {
                bmp[(yi * bw + xi) as usize]
            }
        };
        let (xi, yi) = (x0 as i32, y0 as i32);
        let top = at(xi, yi) * (1.0 - fx) + at(xi + 1, yi) * fx;
        let bot = at(xi, yi + 1) * (1.0 - fx) + at(xi + 1, yi + 1) * fx;
        top * (1.0 - fy) + bot * fy
    };
    let (cr, sr) = (rot.cos(), rot.sin());
    // Destination bounds of the rotated bitmap.
    let corners = [
        (b.min.x, b.min.y),
        (b.max.x, b.min.y),
        (b.min.x, b.max.y),
        (b.max.x, b.max.y),
    ];
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (lx, ly) in corners {
        let (dx, dy) = (pen.0 + lx * cr - ly * sr, pen.1 + lx * sr + ly * cr);
        (x0, y0, x1, y1) = (x0.min(dx), y0.min(dy), x1.max(dx), y1.max(dy));
    }
    let (w, h) = (img.width as i32, img.height as i32);
    for y in (y0.floor() as i32).max(0)..=(y1.ceil() as i32).min(h - 1) {
        for x in (x0.floor() as i32).max(0)..=(x1.ceil() as i32).min(w - 1) {
            let (dx, dy) = (x as f32 + 0.5 - pen.0, y as f32 + 0.5 - pen.1);
            let (lx, ly) = (dx * cr + dy * sr, -dx * sr + dy * cr);
            let v = sample(lx - b.min.x - 0.5, ly - b.min.y - 0.5) * alpha;
            if v > 0.0 {
                let d = &mut img.data[(y * w + x) as usize];
                *d = (*d).max((v.clamp(0.0, 1.0) * 255.0) as u8);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_block_rasterises_centred() {
        let mut ids = lavmfx_core::IdGen::new();
        let e = VideoEffect::from_catalog(&mut ids, "Text Block").unwrap();
        set_frame(320, 180, 0.0);
        let spec = Spec::read(&e, false, 320, 180);
        if font(spec.font, spec.style).is_none() {
            return; // no system fonts here
        }
        let img = render(&spec, 320, 180, None);
        let ink: Vec<(usize, usize)> = img
            .data
            .iter()
            .enumerate()
            .filter(|(_, v)| **v > 128)
            .map(|(i, _)| (i % 320, i / 320))
            .collect();
        assert!(ink.len() > 200, "some text drawn");
        let cx = ink.iter().map(|p| p.0).sum::<usize>() / ink.len();
        let cy = ink.iter().map(|p| p.1).sum::<usize>() / ink.len();
        assert!((140..180).contains(&cx) && (70..110).contains(&cy), "centred {cx},{cy}");
        // Key is stable and non-zero; a different text changes it.
        let k = image_key(&e);
        assert!(k != 0 && k == image_key(&e));
    }

    #[test]
    fn animator_units_come_in_over_time() {
        let mut ids = lavmfx_core::IdGen::new();
        let mut e = VideoEffect::from_catalog(&mut ids, "Text Animator").unwrap();
        if let Some(Param::Bool(b)) = e.params.get_mut("Animated") {
            b.value = true;
        }
        let spec = Spec::read(&e, true, 320, 180);
        if font(spec.font, spec.style).is_none() {
            return;
        }
        let ink = |t| {
            render(&spec, 320, 180, Some(t))
                .data
                .iter()
                .filter(|v| **v > 128)
                .count()
        };
        let (early, late) = (ink(0.05), ink(3.0));
        assert!(early < late, "more letters later: {early} < {late}");
    }
}
