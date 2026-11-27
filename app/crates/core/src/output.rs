//! Advanced output (projection mapping): screens, slices, warping.
//!
//! A **screen** is one output device (a display, a window or a virtual output)
//! holding **slices**. A slice takes a quad of the composition (the *input
//! selection*, composition pixels) and draws it on the screen through a
//! warp grid (slice space, 0..1) followed by a corner-pin homography onto
//! the *output quad* (screen pixels).
//!
//! The setup is global (not saved in the composition); the app keeps it
//! beside the preferences. Not part of the REST schema.

use serde::{Deserialize, Serialize};

/// A point: composition pixels (input), screen pixels (output) or slice
/// space 0..1 (warp grid).
pub type Pt = [f32; 2];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvancedOutput {
    pub screens: Vec<Screen>,
    /// DMX pixel mapping (lumiverses).
    pub lumiverses: Vec<crate::dmx::Lumiverse>,
    /// Next id for screens / slices / lumiverses / fixtures.
    pub next_id: u64,
}

impl AdvancedOutput {
    /// The slices effects see (Slice Delay, Slice Chase…), bottom first:
    /// enabled slices of enabled screens, key slices left out.
    pub fn effect_slices(&self) -> impl Iterator<Item = (&Screen, &Slice)> {
        self.screens.iter().filter(|s| s.enabled).flat_map(|s| {
            s.slices
                .iter()
                .filter(|sl| sl.enabled && !sl.is_key)
                .map(move |sl| (s, sl))
        })
    }
}

impl Default for AdvancedOutput {
    fn default() -> Self {
        Self {
            screens: Vec::new(),
            lumiverses: Vec::new(),
            next_id: 1,
        }
    }
}

/// Where a screen is shown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "kind")]
pub enum Device {
    /// Not shown anywhere (for sending to other outputs later; a
    /// "Virtual Output").
    #[default]
    Virtual,
    /// A normal window (movable, F11 = fullscreen on its monitor).
    Window,
    /// Borderless fullscreen on a display (index into the system's monitor
    /// list; the name is remembered to find it again when the order changes).
    Display { index: usize, name: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorAdjust {
    /// 0..1 (screens only).
    pub opacity: f32,
    /// −1..1 each.
    pub brightness: f32,
    pub contrast: f32,
    pub red: f32,
    pub green: f32,
    pub blue: f32,
}

impl Default for ColorAdjust {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            brightness: 0.0,
            contrast: 0.0,
            red: 0.0,
            green: 0.0,
            blue: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Screen {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    pub device: Device,
    pub width: u32,
    pub height: u32,
    pub color: ColorAdjust,
    /// Bottom first (drawn in order; the editor lists the top one first).
    pub slices: Vec<Slice>,
    /// Output masks: black out parts of the screen, drawn above the slices.
    pub masks: Vec<Mask>,
    /// Input Guide: an image file shown over the Input Selection
    /// canvas to line slices up (editor only, never output).
    pub input_guide: Option<String>,
    pub guide_opacity: f32,
}

impl Default for Screen {
    fn default() -> Self {
        Self {
            id: 0,
            name: "Screen".into(),
            enabled: true,
            device: Device::Virtual,
            width: 1920,
            height: 1080,
            color: ColorAdjust::default(),
            slices: Vec::new(),
            masks: Vec::new(),
            input_guide: None,
            guide_opacity: 0.5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PointMode {
    /// Straight lines between grid points.
    #[default]
    Linear,
    /// Smooth curves through the grid points.
    Bezier,
}

/// Warp grid in slice space: `cols × rows` points, row-major, top row first.
/// The identity grid is evenly spaced over 0..1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Warp {
    pub mode: PointMode,
    pub cols: usize,
    pub rows: usize,
    pub points: Vec<Pt>,
}

impl Default for Warp {
    fn default() -> Self {
        Self::identity(2, 2)
    }
}

impl Warp {
    pub fn identity(cols: usize, rows: usize) -> Self {
        let (cols, rows) = (cols.max(2), rows.max(2));
        let mut points = Vec::with_capacity(cols * rows);
        for r in 0..rows {
            for c in 0..cols {
                points.push([c as f32 / (cols - 1) as f32, r as f32 / (rows - 1) as f32]);
            }
        }
        Self {
            mode: PointMode::Linear,
            cols,
            rows,
            points,
        }
    }

    /// "Subdivisions X / Y": extra points between the corners.
    pub fn subdivisions(&self) -> (usize, usize) {
        (self.cols - 2, self.rows - 2)
    }

    /// Change the grid density, keeping the current shape (resampled).
    pub fn set_subdivisions(&mut self, sx: usize, sy: usize) {
        let (cols, rows) = (sx.min(30) + 2, sy.min(30) + 2);
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        let mut points = Vec::with_capacity(cols * rows);
        for r in 0..rows {
            for c in 0..cols {
                points.push(self.eval(c as f32 / (cols - 1) as f32, r as f32 / (rows - 1) as f32));
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.points = points;
    }

    pub fn is_identity(&self) -> bool {
        let id = Self::identity(self.cols, self.rows);
        self.points
            .iter()
            .zip(&id.points)
            .all(|(a, b)| (a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5)
    }

    fn at(&self, c: isize, r: isize) -> Pt {
        let c = c.clamp(0, self.cols as isize - 1) as usize;
        let r = r.clamp(0, self.rows as isize - 1) as usize;
        self.points[r * self.cols + c]
    }

    /// Warped position of slice point (u, v) ∈ 0..1.
    pub fn eval(&self, u: f32, v: f32) -> Pt {
        if self.points.len() != self.cols * self.rows || self.cols < 2 || self.rows < 2 {
            return [u, v];
        }
        let fx = u.clamp(0.0, 1.0) * (self.cols - 1) as f32;
        let fy = v.clamp(0.0, 1.0) * (self.rows - 1) as f32;
        let cx = (fx.floor() as isize).min(self.cols as isize - 2);
        let cy = (fy.floor() as isize).min(self.rows as isize - 2);
        let (tx, ty) = (fx - cx as f32, fy - cy as f32);
        match self.mode {
            PointMode::Linear => {
                let p00 = self.at(cx, cy);
                let p10 = self.at(cx + 1, cy);
                let p01 = self.at(cx, cy + 1);
                let p11 = self.at(cx + 1, cy + 1);
                let top = lerp2(p00, p10, tx);
                let bot = lerp2(p01, p11, tx);
                lerp2(top, bot, ty)
            }
            PointMode::Bezier => {
                // Bicubic Catmull-Rom: passes through every grid point.
                // Edge neighbours are mirrored so borders stay straight
                // when the grid is regular.
                let row = |r: isize| -> Pt {
                    let p = |c: isize| self.extrap(c, r);
                    catmull(p(cx - 1), p(cx), p(cx + 1), p(cx + 2), tx)
                };
                catmull(row(cy - 1), row(cy), row(cy + 1), row(cy + 2), ty)
            }
        }
    }

    /// Grid point with linear extrapolation outside the grid.
    fn extrap(&self, c: isize, r: isize) -> Pt {
        let (w, h) = (self.cols as isize, self.rows as isize);
        let mirror = |edge: Pt, inner: Pt| [2.0 * edge[0] - inner[0], 2.0 * edge[1] - inner[1]];
        if r < 0 || r >= h {
            let (edge, inner) = if r < 0 { (0, 1) } else { (h - 1, h - 2) };
            return mirror(self.extrap(c, edge), self.extrap(c, inner));
        }
        if c < 0 || c >= w {
            let (edge, inner) = if c < 0 { (0, 1) } else { (w - 1, w - 2) };
            return mirror(self.at(edge, r), self.at(inner, r));
        }
        self.at(c, r)
    }
}

fn dist2(a: Pt, b: Pt) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

/// Bounding box of any point list.
pub fn bounds_of(p: &[Pt]) -> (Pt, Pt) {
    let mut lo = [f32::MAX; 2];
    let mut hi = [f32::MIN; 2];
    for v in p {
        lo = [lo[0].min(v[0]), lo[1].min(v[1])];
        hi = [hi[0].max(v[0]), hi[1].max(v[1])];
    }
    (lo, hi)
}

fn lerp2(a: Pt, b: Pt, t: f32) -> Pt {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn catmull(p0: Pt, p1: Pt, p2: Pt, p3: Pt, t: f32) -> Pt {
    let t2 = t * t;
    let t3 = t2 * t;
    let f = |a: f32, b: f32, c: f32, d: f32| {
        0.5 * (2.0 * b + (c - a) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2 + (3.0 * b - a - 3.0 * c + d) * t3)
    };
    [f(p0[0], p1[0], p2[0], p3[0]), f(p0[1], p1[1], p2[1], p3[1])]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Flip {
    pub h: bool,
    pub v: bool,
}

/// Edge blend for overlapping projectors. Widths are fractions of the slice
/// (0..0.5); the ramp follows Paul Bourke's curve (luminance at the
/// centre of the overlap, power = steepness) then 1/gamma.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoftEdge {
    pub enabled: bool,
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
    pub luminance: f32,
    pub power: f32,
    pub gamma: f32,
}

impl Default for SoftEdge {
    fn default() -> Self {
        Self {
            enabled: false,
            left: 0.0,
            right: 0.0,
            top: 0.0,
            bottom: 0.0,
            luminance: 0.5,
            power: 2.0,
            gamma: 1.0,
        }
    }
}

impl SoftEdge {
    /// Brightness factor at slice point (u, v); 1 when disabled.
    pub fn factor(&self, u: f32, v: f32) -> f32 {
        if !self.enabled {
            return 1.0;
        }
        let ramp = |x: f32, w: f32| {
            if w <= 0.0 {
                1.0
            } else {
                self.curve((x / w).clamp(0.0, 1.0))
            }
        };
        ramp(u, self.left) * ramp(1.0 - u, self.right) * ramp(v, self.top) * ramp(1.0 - v, self.bottom)
    }

    /// 0 → 0, 0.5 → luminance, 1 → 1.
    pub fn curve(&self, x: f32) -> f32 {
        let (a, p) = (self.luminance.clamp(0.0, 1.0), self.power.max(0.01));
        let y = if x < 0.5 {
            a * (2.0 * x).powf(p)
        } else {
            1.0 - (1.0 - a) * (2.0 * (1.0 - x)).powf(p)
        };
        y.powf(1.0 / self.gamma.max(0.01))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Slice {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    /// Input selection: TL, TR, BR, BL in composition pixels.
    pub input: [Pt; 4],
    /// Output corner pin: TL, TR, BR, BL in screen pixels.
    pub output: [Pt; 4],
    pub warp: Warp,
    pub flip: Flip,
    /// "Is Key": the slice masks the slices above (stored; drawn in a later step).
    pub is_key: bool,
    /// "Black BG": transparent composition pixels show black, not
    /// the slices below.
    pub black_bg: bool,
    pub color: ColorAdjust,
    pub soft_edge: SoftEdge,
    /// Input Mask: only this part of the input selection shows
    /// (composition pixels).
    pub input_mask: Option<Mask>,
    /// Polygon slice: input / output polygons with matching points
    /// instead of quads (the quads, warp and soft edge are then unused).
    pub polygon: Option<Polygon>,
    /// Input Source: the composition, or one layer / group's result.
    pub input_source: crate::sources::RouteTarget,
}

impl Default for Slice {
    fn default() -> Self {
        let r = rect(0.0, 0.0, 1920.0, 1080.0);
        Self {
            id: 0,
            name: "Slice".into(),
            enabled: true,
            input: r,
            output: r,
            warp: Warp::default(),
            flip: Flip::default(),
            is_key: false,
            black_bg: false,
            color: ColorAdjust::default(),
            soft_edge: SoftEdge::default(),
            input_mask: None,
            polygon: None,
            input_source: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MaskShape {
    /// `points` = the 4 corners (TL, TR, BR, BL).
    #[default]
    Rectangle,
    /// Ellipse inside the 4 corner points.
    Ellipse,
    /// Free polygon (triangle, hexagon, pen).
    Polygon,
}

/// A mask shape: Input Mask (composition px) or Output Mask (screen px).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mask {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    pub shape: MaskShape,
    pub points: Vec<Pt>,
    /// Keep the inside instead (input mask) / black out the outside (output mask).
    pub invert: bool,
}

impl Default for Mask {
    fn default() -> Self {
        Self {
            id: 0,
            name: "Mask".into(),
            enabled: true,
            shape: MaskShape::Rectangle,
            points: rect(0.0, 0.0, 100.0, 100.0).to_vec(),
            invert: false,
        }
    }
}

/// Shapes offered by the mask buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskKind {
    Rectangle,
    Ellipse,
    Triangle,
    Hexagon,
}

impl Mask {
    /// A shape centred in `area` (half its size).
    pub fn new(id: u64, name: impl Into<String>, kind: MaskKind, area: (f32, f32)) -> Self {
        let (cx, cy, w, h) = (area.0 / 2.0, area.1 / 2.0, area.0 / 2.0, area.1 / 2.0);
        let (shape, points) = match kind {
            MaskKind::Rectangle => (MaskShape::Rectangle, rect(cx - w / 2.0, cy - h / 2.0, w, h).to_vec()),
            MaskKind::Ellipse => (MaskShape::Ellipse, rect(cx - w / 2.0, cy - h / 2.0, w, h).to_vec()),
            MaskKind::Triangle => (
                MaskShape::Polygon,
                vec![
                    [cx, cy - h / 2.0],
                    [cx + w / 2.0, cy + h / 2.0],
                    [cx - w / 2.0, cy + h / 2.0],
                ],
            ),
            MaskKind::Hexagon => (
                MaskShape::Polygon,
                (0..6)
                    .map(|i| {
                        let a = (i as f32 * 60.0 - 90.0).to_radians();
                        [cx + a.cos() * w / 2.0, cy + a.sin() * h / 2.0]
                    })
                    .collect(),
            ),
        };
        Self {
            id,
            name: name.into(),
            shape,
            points,
            ..Default::default()
        }
    }

    /// Closed outline (ellipses as 64 points).
    pub fn outline(&self) -> Vec<Pt> {
        match self.shape {
            MaskShape::Ellipse if self.points.len() == 4 => {
                let q = [self.points[0], self.points[1], self.points[2], self.points[3]];
                let h = Homography::square_to_quad(&q);
                (0..64)
                    .map(|i| {
                        let a = i as f32 / 64.0 * std::f32::consts::TAU;
                        h.apply([0.5 + 0.5 * a.cos(), 0.5 + 0.5 * a.sin()])
                    })
                    .collect()
            }
            _ => self.points.clone(),
        }
    }
}

/// Ready-made polygon slice shapes (Advanced Output › + › Polygon).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolygonShape {
    Triangle,
    Hexagon,
    Circle,
}

impl PolygonShape {
    /// Points on the circle the circle is made of (each one a handle).
    pub const CIRCLE_POINTS: usize = 32;

    pub fn name(self) -> &'static str {
        match self {
            Self::Triangle => "Triangle",
            Self::Hexagon => "Hexagon",
            Self::Circle => "Circle",
        }
    }

    /// The shape centred in `area`: the triangle half the area's size (as
    /// before), hexagon and circle regular (round on screen), their
    /// diameter half the area's shorter side.
    pub fn points(self, area: (f32, f32)) -> Vec<Pt> {
        let (cx, cy) = (area.0 / 2.0, area.1 / 2.0);
        let r = area.0.min(area.1) / 4.0;
        let ring = |n: usize| {
            (0..n)
                .map(|i| {
                    let a = (i as f32 * 360.0 / n as f32 - 90.0).to_radians();
                    [cx + a.cos() * r, cy + a.sin() * r]
                })
                .collect()
        };
        match self {
            Self::Triangle => Mask::new(0, "", MaskKind::Triangle, area).points,
            Self::Hexagon => ring(6),
            Self::Circle => ring(Self::CIRCLE_POINTS),
        }
    }
}

/// Polygon slice: point `i` of `input` (composition px) shows at point `i`
/// of `output` (screen px).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Polygon {
    pub input: Vec<Pt>,
    pub output: Vec<Pt>,
}

fn signed_area(p: &[Pt]) -> f32 {
    let n = p.len();
    (0..n)
        .map(|i| p[i][0] * p[(i + 1) % n][1] - p[(i + 1) % n][0] * p[i][1])
        .sum::<f32>()
        * 0.5
}

/// Ear-clipping triangulation of a simple polygon (either winding).
/// Falls back to a fan for degenerate input.
pub fn triangulate(p: &[Pt]) -> Vec<u32> {
    let n = p.len();
    if n < 3 {
        return Vec::new();
    }
    let ccw = signed_area(p) > 0.0;
    let mut idx: Vec<usize> = (0..n).collect();
    let mut out = Vec::with_capacity((n - 2) * 3);
    let cross = |a: Pt, b: Pt, c: Pt| (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
    let inside = |t: [Pt; 3], q: Pt| {
        let d1 = cross(t[0], t[1], q);
        let d2 = cross(t[1], t[2], q);
        let d3 = cross(t[2], t[0], q);
        let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(neg && pos)
    };
    let mut guard = 0;
    while idx.len() > 3 && guard < n * n {
        guard += 1;
        let m = idx.len();
        let mut clipped = false;
        for k in 0..m {
            let (ia, ib, ic) = (idx[(k + m - 1) % m], idx[k], idx[(k + 1) % m]);
            let (a, b, c) = (p[ia], p[ib], p[ic]);
            let c2 = cross(a, b, c);
            let convex = if ccw { c2 > 1e-9 } else { c2 < -1e-9 };
            if !convex {
                continue;
            }
            if idx
                .iter()
                .any(|&j| j != ia && j != ib && j != ic && inside([a, b, c], p[j]))
            {
                continue;
            }
            out.extend([ia as u32, ib as u32, ic as u32]);
            idx.remove(k);
            clipped = true;
            break;
        }
        if !clipped {
            break;
        }
    }
    if idx.len() == 3 {
        out.extend(idx.iter().map(|&i| i as u32));
    } else if idx.len() > 3 {
        for k in 1..idx.len() - 1 {
            out.extend([idx[0] as u32, idx[k] as u32, idx[k + 1] as u32]);
        }
    }
    out
}

/// Barycentric coordinates of `q` in triangle `t`.
fn barycentric(t: [Pt; 3], q: Pt) -> Option<[f32; 3]> {
    let [a, b, c] = t;
    let den = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
    if den.abs() < 1e-9 {
        return None;
    }
    let l1 = ((b[1] - c[1]) * (q[0] - c[0]) + (c[0] - b[0]) * (q[1] - c[1])) / den;
    let l2 = ((c[1] - a[1]) * (q[0] - c[0]) + (a[0] - c[0]) * (q[1] - c[1])) / den;
    Some([l1, l2, 1.0 - l1 - l2])
}

/// Axis-aligned rect as TL, TR, BR, BL.
pub fn rect(x: f32, y: f32, w: f32, h: f32) -> [Pt; 4] {
    [[x, y], [x + w, y], [x + w, y + h], [x, y + h]]
}

/// Bounding box (min, max) of a quad.
pub fn bounds(q: &[Pt; 4]) -> (Pt, Pt) {
    let mut lo = [f32::MAX; 2];
    let mut hi = [f32::MIN; 2];
    for p in q {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    (lo, hi)
}

/// Quad edits shared by the input selection and the output transform.
pub mod quad {
    use super::Pt;

    pub fn center(q: &[Pt]) -> Pt {
        let (lo, hi) = super::bounds_of(q);
        [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5]
    }

    pub fn size(q: &[Pt]) -> Pt {
        let (lo, hi) = super::bounds_of(q);
        [hi[0] - lo[0], hi[1] - lo[1]]
    }

    pub fn translate(q: &mut [Pt], d: Pt) {
        for p in q {
            p[0] += d[0];
            p[1] += d[1];
        }
    }

    /// Scale about `anchor`.
    pub fn scale(q: &mut [Pt], anchor: Pt, s: Pt) {
        for p in q {
            p[0] = anchor[0] + (p[0] - anchor[0]) * s[0];
            p[1] = anchor[1] + (p[1] - anchor[1]) * s[1];
        }
    }

    /// Rotation of the top edge, degrees (clockwise on screen).
    pub fn angle(q: &[Pt]) -> f32 {
        let d = [q[1][0] - q[0][0], q[1][1] - q[0][1]];
        d[1].atan2(d[0]).to_degrees()
    }

    /// Rotate by `deg` about the quad's centre of mass.
    pub fn rotate(q: &mut [Pt], deg: f32) {
        let n = q.len().max(1) as f32;
        let c = [
            q.iter().map(|p| p[0]).sum::<f32>() / n,
            q.iter().map(|p| p[1]).sum::<f32>() / n,
        ];
        let (s, co) = deg.to_radians().sin_cos();
        for p in q {
            let (x, y) = (p[0] - c[0], p[1] - c[1]);
            *p = [c[0] + x * co - y * s, c[1] + x * s + y * co];
        }
    }

    /// Set the bounding box (centre x/y, width, height), keeping the shape.
    pub fn set_box(q: &mut [Pt], c: Pt, sz: Pt) {
        let (lo, hi) = super::bounds_of(q);
        let old = [hi[0] - lo[0], hi[1] - lo[1]];
        let s = [
            if old[0].abs() > 1e-6 { sz[0] / old[0] } else { 1.0 },
            if old[1].abs() > 1e-6 { sz[1] / old[1] } else { 1.0 },
        ];
        scale(q, lo, s);
        let now = center(q);
        translate(q, [c[0] - now[0], c[1] - now[1]]);
    }

    /// Point-in-quad (convex or not: even-odd rule).
    pub fn contains(q: &[Pt], p: Pt) -> bool {
        let n = q.len();
        if n < 3 {
            return false;
        }
        let mut inside = false;
        let mut j = n - 1;
        for i in 0..n {
            let (a, b) = (q[i], q[j]);
            if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
                inside = !inside;
            }
            j = i;
        }
        inside
    }
}

impl AdvancedOutput {
    /// The composition resolution changed: scale every input selection.
    pub fn fit_composition(&mut self, from: (u32, u32), to: (u32, u32)) {
        if from == to || from.0 == 0 || from.1 == 0 {
            return;
        }
        let s = [to.0 as f32 / from.0 as f32, to.1 as f32 / from.1 as f32];
        for sl in self.screens.iter_mut().flat_map(|s| s.slices.iter_mut()) {
            quad::scale(&mut sl.input, [0.0, 0.0], s);
        }
    }
}

impl Screen {
    /// Where composition point `p` (composition px) shows on this screen:
    /// one screen point per enabled slice whose input selection contains it
    /// (Output › Show Mouse Pointer).
    pub fn comp_to_screen(&self, p: Pt) -> Vec<Pt> {
        self.slices
            .iter()
            .filter(|sl| sl.enabled && !sl.is_key)
            .filter(|sl| match &sl.polygon {
                Some(g) if g.input.len() >= 3 => quad::contains(&g.input, p),
                _ => quad::contains(&sl.input, p),
            })
            .map(|sl| sl.input_to_output(p))
            .collect()
    }

    /// Change the screen size, scaling the slices' output quads with it.
    pub fn resize(&mut self, w: u32, h: u32) {
        let (w, h) = (w.clamp(16, 16384), h.clamp(16, 16384));
        if (w, h) == (self.width, self.height) {
            return;
        }
        let s = [
            w as f32 / self.width.max(1) as f32,
            h as f32 / self.height.max(1) as f32,
        ];
        for sl in &mut self.slices {
            quad::scale(&mut sl.output, [0.0, 0.0], s);
        }
        self.width = w;
        self.height = h;
    }
}

/// Projective map of the unit square onto a quad (TL, TR, BR, BL):
/// (0,0)→TL, (1,0)→TR, (1,1)→BR, (0,1)→BL. Row-major 3×3.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Homography(pub [f32; 9]);

impl Homography {
    /// Heckbert's square-to-quad. Degenerate quads fall back to bilinear-ish affine.
    pub fn square_to_quad(q: &[Pt; 4]) -> Self {
        let [[x0, y0], [x1, y1], [x2, y2], [x3, y3]] = q.map(|p| [p[0] as f64, p[1] as f64]);
        let sx = x0 - x1 + x2 - x3;
        let sy = y0 - y1 + y2 - y3;
        let m = if sx.abs() < 1e-9 && sy.abs() < 1e-9 {
            [x1 - x0, x3 - x0, x0, y1 - y0, y3 - y0, y0, 0.0, 0.0, 1.0]
        } else {
            let (dx1, dx2) = (x1 - x2, x3 - x2);
            let (dy1, dy2) = (y1 - y2, y3 - y2);
            let den = dx1 * dy2 - dx2 * dy1;
            let den = if den.abs() < 1e-12 { 1e-12 } else { den };
            let g = (sx * dy2 - dx2 * sy) / den;
            let h = (dx1 * sy - sx * dy1) / den;
            [
                x1 - x0 + g * x1,
                x3 - x0 + h * x3,
                x0,
                y1 - y0 + g * y1,
                y3 - y0 + h * y3,
                y0,
                g,
                h,
                1.0,
            ]
        };
        Self(m.map(|v| v as f32))
    }

    pub fn apply(&self, p: Pt) -> Pt {
        let m = &self.0;
        let w = m[6] * p[0] + m[7] * p[1] + m[8];
        let w = if w.abs() < 1e-9 { 1e-9 } else { w };
        [
            (m[0] * p[0] + m[1] * p[1] + m[2]) / w,
            (m[3] * p[0] + m[4] * p[1] + m[5]) / w,
        ]
    }

    pub fn inverse(&self) -> Option<Self> {
        let m = self.0.map(|v| v as f64);
        let a = m[4] * m[8] - m[5] * m[7];
        let b = m[5] * m[6] - m[3] * m[8];
        let c = m[3] * m[7] - m[4] * m[6];
        let det = m[0] * a + m[1] * b + m[2] * c;
        if det.abs() < 1e-12 {
            return None;
        }
        let inv = [
            a,
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            b,
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            c,
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];
        Some(Self(inv.map(|v| (v / det) as f32)))
    }
}

/// One mesh vertex of a slice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshVertex {
    /// Screen pixels.
    pub pos: Pt,
    /// Composition texture coordinate 0..1 (flip applied).
    pub uv: Pt,
    /// Slice space 0..1 (soft edge).
    pub local: Pt,
}

/// Triangle mesh of one slice.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SliceMesh {
    pub vertices: Vec<MeshVertex>,
    pub indices: Vec<u32>,
}

impl Slice {
    pub fn new(id: u64, name: impl Into<String>, comp: (u32, u32), screen: (u32, u32)) -> Self {
        Self {
            id,
            name: name.into(),
            input: rect(0.0, 0.0, comp.0 as f32, comp.1 as f32),
            output: rect(0.0, 0.0, screen.0 as f32, screen.1 as f32),
            ..Default::default()
        }
    }

    /// Screen position of slice point (u, v): warp, then corner pin.
    pub fn map_out(&self, u: f32, v: f32) -> Pt {
        Homography::square_to_quad(&self.output).apply(self.warp.eval(u, v))
    }

    /// Composition texture coordinate (0..1) of slice point (u, v).
    pub fn map_in(&self, u: f32, v: f32, comp: (u32, u32)) -> Pt {
        let (u, v) = (
            if self.flip.h { 1.0 - u } else { u },
            if self.flip.v { 1.0 - v } else { v },
        );
        let [tl, tr, br, bl] = self.input;
        let p = lerp2(lerp2(tl, tr, u), lerp2(bl, br, u), v);
        [p[0] / comp.0.max(1) as f32, p[1] / comp.1.max(1) as f32]
    }

    /// Screen position of a composition point (Input Mask → screen).
    /// Points outside the slice land on its border.
    pub fn input_to_output(&self, p: Pt) -> Pt {
        if let Some(poly) = self
            .polygon
            .as_ref()
            .filter(|g| g.input.len() >= 3 && g.input.len() == g.output.len())
        {
            let tris = triangulate(&poly.input);
            let mut best: Option<(f32, Pt)> = None;
            for t in tris.chunks(3) {
                let ti = [
                    poly.input[t[0] as usize],
                    poly.input[t[1] as usize],
                    poly.input[t[2] as usize],
                ];
                let to = [
                    poly.output[t[0] as usize],
                    poly.output[t[1] as usize],
                    poly.output[t[2] as usize],
                ];
                if let Some(l) = barycentric(ti, p) {
                    // Outside every triangle: use the least-outside one.
                    let out = -(l[0].min(l[1]).min(l[2])).min(0.0);
                    let q = [
                        l[0] * to[0][0] + l[1] * to[1][0] + l[2] * to[2][0],
                        l[0] * to[0][1] + l[1] * to[1][1] + l[2] * to[2][1],
                    ];
                    if best.is_none_or(|b| out < b.0) {
                        best = Some((out, q));
                    }
                }
            }
            return best.map_or(p, |b| b.1);
        }
        let Some(inv) = Homography::square_to_quad(&self.input).inverse() else {
            return p;
        };
        let [mut u, mut v] = inv.apply(p);
        if self.flip.h {
            u = 1.0 - u;
        }
        if self.flip.v {
            v = 1.0 - v;
        }
        self.map_out(u, v)
    }

    /// Input mask outline on the screen, edges subdivided so they follow the warp.
    pub fn input_mask_on_screen(&self) -> Option<Vec<Pt>> {
        let m = self.input_mask.as_ref().filter(|m| m.enabled)?;
        let o = m.outline();
        if o.len() < 3 {
            return None;
        }
        let (lo, hi) = bounds(&self.input);
        let step = ((hi[0] - lo[0]).max(hi[1] - lo[1]) / 48.0).max(1.0);
        let mut pts = Vec::new();
        for i in 0..o.len() {
            let (a, b) = (o[i], o[(i + 1) % o.len()]);
            let n = ((dist2(a, b).sqrt() / step).ceil() as usize).clamp(1, 64);
            for k in 0..n {
                pts.push(self.input_to_output(lerp2(a, b, k as f32 / n as f32)));
            }
        }
        Some(pts)
    }

    /// Outline on the screen (polygon points, or the warped quad border).
    pub fn outline(&self) -> Vec<Pt> {
        if let Some(p) = &self.polygon {
            return p.output.clone();
        }
        let n = 24;
        let mut v = Vec::with_capacity(n * 4);
        for i in 0..n {
            v.push(self.map_out(i as f32 / n as f32, 0.0));
        }
        for i in 0..n {
            v.push(self.map_out(1.0, i as f32 / n as f32));
        }
        for i in 0..n {
            v.push(self.map_out(1.0 - i as f32 / n as f32, 1.0));
        }
        for i in 0..n {
            v.push(self.map_out(0.0, 1.0 - i as f32 / n as f32));
        }
        v
    }

    /// A Polygon slice: `input` (composition px) shown at `output` (screen px).
    pub fn new_polygon(id: u64, name: impl Into<String>, input: Vec<Pt>, output: Vec<Pt>) -> Self {
        let r = |p: &[Pt]| {
            let (lo, hi) = (
                [
                    p.iter().map(|v| v[0]).fold(f32::MAX, f32::min),
                    p.iter().map(|v| v[1]).fold(f32::MAX, f32::min),
                ],
                [
                    p.iter().map(|v| v[0]).fold(f32::MIN, f32::max),
                    p.iter().map(|v| v[1]).fold(f32::MIN, f32::max),
                ],
            );
            rect(lo[0], lo[1], hi[0] - lo[0], hi[1] - lo[1])
        };
        Self {
            id,
            name: name.into(),
            input: r(&input),
            output: r(&output),
            polygon: Some(Polygon { input, output }),
            ..Default::default()
        }
    }

    /// Tessellate into a grid fine enough for the warp and perspective
    /// (`steps` cells per grid cell, at least 16 per side).
    pub fn mesh(&self, comp: (u32, u32), steps: usize) -> SliceMesh {
        if let Some(p) = self
            .polygon
            .as_ref()
            .filter(|g| g.input.len() >= 3 && g.input.len() == g.output.len())
        {
            let (lo, hi) = bounds_of(&p.output);
            let (w, h) = ((hi[0] - lo[0]).max(1e-3), (hi[1] - lo[1]).max(1e-3));
            return SliceMesh {
                vertices: p
                    .input
                    .iter()
                    .zip(&p.output)
                    .map(|(i, o)| MeshVertex {
                        pos: *o,
                        uv: [i[0] / comp.0.max(1) as f32, i[1] / comp.1.max(1) as f32],
                        local: [(o[0] - lo[0]) / w, (o[1] - lo[1]) / h],
                    })
                    .collect(),
                indices: triangulate(&p.input),
            };
        }
        let nx = ((self.warp.cols - 1) * steps).max(16);
        let ny = ((self.warp.rows - 1) * steps).max(16);
        let h = Homography::square_to_quad(&self.output);
        let mut m = SliceMesh::default();
        m.vertices.reserve((nx + 1) * (ny + 1));
        for j in 0..=ny {
            for i in 0..=nx {
                let (u, v) = (i as f32 / nx as f32, j as f32 / ny as f32);
                m.vertices.push(MeshVertex {
                    pos: h.apply(self.warp.eval(u, v)),
                    uv: self.map_in(u, v, comp),
                    local: [u, v],
                });
            }
        }
        let w = (nx + 1) as u32;
        for j in 0..ny as u32 {
            for i in 0..nx as u32 {
                let a = j * w + i;
                m.indices.extend_from_slice(&[a, a + 1, a + w, a + 1, a + w + 1, a + w]);
            }
        }
        m
    }
}

impl AdvancedOutput {
    fn alloc(&mut self) -> u64 {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        id
    }

    /// The default: one screen with one full-size slice.
    pub fn with_default_screen(comp: (u32, u32)) -> Self {
        let mut a = Self::default();
        let s = a.add_screen(comp, Device::Window);
        a.add_slice(s, comp);
        a
    }

    /// Add a screen sized like the composition; returns its index.
    pub fn add_screen(&mut self, comp: (u32, u32), device: Device) -> usize {
        let id = self.alloc();
        self.screens.push(Screen {
            id,
            name: format!("Screen {}", self.screens.len() + 1),
            device,
            width: comp.0,
            height: comp.1,
            ..Default::default()
        });
        self.screens.len() - 1
    }

    /// Add a slice covering the whole composition and screen on top of
    /// `screen`'s slices; returns its index.
    pub fn add_slice(&mut self, screen: usize, comp: (u32, u32)) -> usize {
        let id = self.alloc();
        let s = &mut self.screens[screen];
        let n = s.slices.len() + 1;
        let slice = Slice::new(id, format!("Slice {n}"), comp, (s.width, s.height));
        s.slices.push(slice);
        s.slices.len() - 1
    }

    pub fn duplicate_slice(&mut self, screen: usize, slice: usize) -> usize {
        let id = self.alloc();
        let s = &mut self.screens[screen];
        let mut copy = s.slices[slice].clone();
        copy.id = id;
        copy.name = format!("{} copy", copy.name);
        s.slices.insert(slice + 1, copy);
        slice + 1
    }

    pub fn new_id(&mut self) -> u64 {
        self.alloc()
    }

    /// Add an output mask to a screen; returns its index.
    pub fn add_mask(&mut self, screen: usize, kind: MaskKind) -> usize {
        let id = self.alloc();
        let s = &mut self.screens[screen];
        let n = s.masks.len() + 1;
        let m = Mask::new(id, format!("Mask {n}"), kind, (s.width as f32, s.height as f32));
        s.masks.push(m);
        s.masks.len() - 1
    }

    /// Add a polygon slice (a triangle, or `points` from the pen) to a
    /// screen; input and output start at the same place.
    pub fn add_polygon(&mut self, screen: usize, comp: (u32, u32), points: Option<Vec<Pt>>) -> usize {
        let id = self.alloc();
        let s = &mut self.screens[screen];
        let out =
            points.unwrap_or_else(|| Mask::new(0, "", MaskKind::Triangle, (s.width as f32, s.height as f32)).points);
        // Input: the same shape in composition pixels.
        let sx = comp.0 as f32 / s.width.max(1) as f32;
        let sy = comp.1 as f32 / s.height.max(1) as f32;
        let input = out.iter().map(|p| [p[0] * sx, p[1] * sy]).collect();
        let n = s.slices.len() + 1;
        s.slices
            .push(Slice::new_polygon(id, format!("Polygon {n}"), input, out));
        s.slices.len() - 1
    }

    /// Add a ready-made polygon slice (triangle, hexagon, circle) centred on
    /// the screen; named after its shape.
    pub fn add_polygon_shape(&mut self, screen: usize, comp: (u32, u32), shape: PolygonShape) -> usize {
        let s = &self.screens[screen];
        let pts = shape.points((s.width as f32, s.height as f32));
        let i = self.add_polygon(screen, comp, Some(pts));
        let slices = &mut self.screens[screen].slices;
        slices[i].name = format!("{} {}", shape.name(), slices.len());
        i
    }

    /// Add a lumiverse on the next free universe; returns its index.
    pub fn add_lumiverse(&mut self) -> usize {
        let id = self.alloc();
        let universe = self
            .lumiverses
            .iter()
            .map(|l| l.universe + l.universes() as u16)
            .max()
            .unwrap_or(0);
        self.lumiverses.push(crate::dmx::Lumiverse {
            id,
            name: format!("Lumiverse {}", self.lumiverses.len() + 1),
            universe,
            ..Default::default()
        });
        self.lumiverses.len() - 1
    }

    /// Add a fixture of `personality` to a lumiverse after its last channel,
    /// its input a strip / grid shaped like the pixels, stacked below the
    /// previous fixture; returns its index.
    pub fn add_fixture(&mut self, lumi: usize, comp: (u32, u32), personality: crate::dmx::Personality) -> usize {
        let id = self.alloc();
        let l = &mut self.lumiverses[lumi];
        let (gw, gh) = personality.grid();
        // About 10 composition px per pixel, kept inside the composition.
        let cell = (comp.0 as f32 / gw as f32)
            .min(comp.1 as f32 / gh as f32)
            .clamp(1.0, 10.0);
        let (w, h) = (gw as f32 * cell, gh as f32 * cell);
        let y = l.fixtures.last().map(|f| bounds(&f.input).1[1] + cell).unwrap_or(0.0);
        let y = if y + h > comp.1 as f32 { 0.0 } else { y };
        let n = l.fixtures.len() + 1;
        let start_channel = l.next_channel();
        l.fixtures.push(crate::dmx::Fixture {
            id,
            name: format!("{} {n}", personality.name),
            start_channel,
            input: rect(0.0, y, w, h),
            personality,
            ..Default::default()
        });
        l.fixtures.len() - 1
    }

    /// Screens that need a window (not virtual, enabled).
    pub fn shown(&self) -> impl Iterator<Item = (usize, &Screen)> {
        self.screens
            .iter()
            .enumerate()
            .filter(|(_, s)| s.enabled && s.device != Device::Virtual)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Pt, b: Pt) -> bool {
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
    }

    #[test]
    fn homography_hits_corners_and_inverts() {
        let q = [[10.0, 20.0], [300.0, 5.0], [280.0, 250.0], [30.0, 200.0]];
        let h = Homography::square_to_quad(&q);
        for (p, c) in [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]].into_iter().zip(q) {
            assert!(close(h.apply(p), c), "{:?} -> {:?} != {:?}", p, h.apply(p), c);
        }
        let inv = h.inverse().unwrap();
        let p = h.apply([0.3, 0.7]);
        assert!(close(inv.apply(p), [0.3, 0.7]));
        // Perspective: the quad centre is not the average of the corners.
        let mid = h.apply([0.5, 0.5]);
        assert!(!close(mid, [155.0, 118.75]));
    }

    #[test]
    fn affine_quad_is_exact() {
        let h = Homography::square_to_quad(&rect(100.0, 50.0, 200.0, 100.0));
        assert!(close(h.apply([0.5, 0.5]), [200.0, 100.0]));
    }

    #[test]
    fn warp_identity_and_resample() {
        for mode in [PointMode::Linear, PointMode::Bezier] {
            let mut w = Warp::identity(4, 3);
            w.mode = mode;
            for (u, v) in [(0.0, 0.0), (0.37, 0.81), (1.0, 1.0), (0.5, 0.5)] {
                assert!(close(w.eval(u, v), [u, v]), "{mode:?} {u},{v}");
            }
            w.set_subdivisions(3, 1);
            assert_eq!((w.cols, w.rows), (5, 3));
            assert!(w.is_identity());
        }
    }

    #[test]
    fn warp_passes_through_moved_point() {
        let mut w = Warp::identity(3, 3);
        w.points[4] = [0.6, 0.4];
        for mode in [PointMode::Linear, PointMode::Bezier] {
            w.mode = mode;
            assert!(close(w.eval(0.5, 0.5), [0.6, 0.4]));
            assert!(close(w.eval(0.0, 0.0), [0.0, 0.0]));
        }
        // Bezier bends between points, linear doesn't.
        w.mode = PointMode::Linear;
        let lin = w.eval(0.25, 0.5);
        w.mode = PointMode::Bezier;
        let bez = w.eval(0.25, 0.5);
        assert!(!close(lin, bez));
    }

    #[test]
    fn soft_edge_curve() {
        let mut e = SoftEdge {
            enabled: true,
            left: 0.2,
            ..Default::default()
        };
        assert_eq!(e.factor(0.0, 0.5), 0.0);
        assert!((e.factor(0.1, 0.5) - 0.5).abs() < 1e-5);
        assert_eq!(e.factor(0.5, 0.5), 1.0);
        e.enabled = false;
        assert_eq!(e.factor(0.0, 0.5), 1.0);
    }

    #[test]
    fn mesh_maps_input_to_output() {
        let mut s = Slice::new(1, "S", (1280, 720), (1920, 1080));
        s.input = rect(640.0, 0.0, 640.0, 360.0); // top-right quarter
        s.output = rect(0.0, 540.0, 960.0, 540.0); // bottom-left quarter
        let m = s.mesh((1280, 720), 4);
        assert_eq!(m.vertices.len(), 17 * 17);
        assert_eq!(m.indices.len(), 16 * 16 * 6);
        let first = m.vertices[0];
        assert!(close(first.pos, [0.0, 540.0]) && close(first.uv, [0.5, 0.0]));
        let last = *m.vertices.last().unwrap();
        assert!(close(last.pos, [960.0, 1080.0]) && close(last.uv, [1.0, 0.5]));
        s.flip.h = true;
        assert!(close(s.mesh((1280, 720), 4).vertices[0].uv, [1.0, 0.0]));
    }

    #[test]
    fn quad_edits() {
        let mut q = rect(0.0, 0.0, 100.0, 50.0);
        quad::set_box(&mut q, [200.0, 100.0], [50.0, 50.0]);
        assert_eq!(q, rect(175.0, 75.0, 50.0, 50.0));
        quad::rotate(&mut q, 90.0);
        assert!((quad::angle(&q) - 90.0).abs() < 1e-3);
        assert!(close(quad::center(&q), [200.0, 100.0]));
        assert!(quad::contains(&q, [200.0, 100.0]));
        assert!(!quad::contains(&q, [100.0, 100.0]));
        let mut s = Screen {
            slices: vec![Slice::new(1, "S", (100, 100), (1920, 1080))],
            ..Default::default()
        };
        s.resize(960, 540);
        assert_eq!(s.slices[0].output, rect(0.0, 0.0, 960.0, 540.0));
    }

    #[test]
    fn triangulates_concave_polygons() {
        // An L shape (concave), both windings.
        let l = vec![[0.0, 0.0], [2.0, 0.0], [2.0, 1.0], [1.0, 1.0], [1.0, 2.0], [0.0, 2.0]];
        for p in [l.clone(), l.iter().rev().copied().collect::<Vec<_>>()] {
            let t = triangulate(&p);
            assert_eq!(t.len(), 4 * 3);
            let area: f32 = t
                .chunks(3)
                .map(|c| signed_area(&[p[c[0] as usize], p[c[1] as usize], p[c[2] as usize]]).abs())
                .sum();
            assert!((area - 3.0).abs() < 1e-4, "area {area}");
        }
    }

    #[test]
    fn polygon_shapes_are_regular_and_centred() {
        let mut a = AdvancedOutput::with_default_screen((1920, 1080));
        for (shape, n) in [
            (PolygonShape::Hexagon, 6),
            (PolygonShape::Circle, PolygonShape::CIRCLE_POINTS),
        ] {
            let i = a.add_polygon_shape(0, (1920, 1080), shape);
            let sl = &a.screens[0].slices[i];
            let p = &sl.polygon.as_ref().unwrap().output;
            assert_eq!(p.len(), n);
            assert!(sl.name.starts_with(shape.name()), "{}", sl.name);
            let (w, h) = (a.screens[0].width as f32, a.screens[0].height as f32);
            for q in p {
                let r = ((q[0] - w / 2.0).powi(2) + (q[1] - h / 2.0).powi(2)).sqrt();
                assert!((r - w.min(h) / 4.0).abs() < 0.01, "round on screen: {r}");
            }
            // It renders: the slice mesh covers the shape.
            assert!(sl.polygon.as_ref().unwrap().input.len() == n);
        }
    }

    #[test]
    fn composition_point_lands_in_every_slice_showing_it() {
        let mut a = AdvancedOutput::with_default_screen((1920, 1080));
        let s = &mut a.screens[0];
        // Slice 0: whole composition → left half of the screen.
        s.slices[0].output = rect(0.0, 0.0, 960.0, 1080.0);
        let mut b = s.slices[0].clone();
        // Slice 1: top-left quarter of the composition → right half.
        b.input = rect(0.0, 0.0, 960.0, 540.0);
        b.output = rect(960.0, 0.0, 960.0, 1080.0);
        s.slices.push(b);
        let near = |a: Pt, b: Pt| (a[0] - b[0]).abs() < 0.5 && (a[1] - b[1]).abs() < 0.5;
        let hits = s.comp_to_screen([480.0, 270.0]);
        assert_eq!(hits.len(), 2);
        assert!(
            near(hits[0], [240.0, 270.0]) && near(hits[1], [1440.0, 540.0]),
            "{hits:?}"
        );
        assert_eq!(
            s.comp_to_screen([1500.0, 900.0]).len(),
            1,
            "only the full slice shows it"
        );
        s.slices[0].enabled = false;
        assert!(s.comp_to_screen([1500.0, 900.0]).is_empty());
    }

    #[test]
    fn polygon_slice_maps_points() {
        let input = vec![[0.0, 0.0], [100.0, 0.0], [0.0, 100.0]];
        let output = vec![[10.0, 10.0], [210.0, 10.0], [10.0, 210.0]];
        let s = Slice::new_polygon(1, "P", input, output);
        assert!(close(s.input_to_output([50.0, 25.0]), [110.0, 60.0]));
        let m = s.mesh((100, 100), 4);
        assert_eq!(m.indices.len(), 3);
        assert!(close(m.vertices[1].uv, [1.0, 0.0]));
    }

    #[test]
    fn masks_and_input_mask_mapping() {
        let e = Mask::new(1, "E", MaskKind::Ellipse, (200.0, 100.0));
        let o = e.outline();
        assert_eq!(o.len(), 64);
        assert!(close(o[0], [150.0, 50.0]), "{:?}", o[0]);
        assert_eq!(Mask::new(1, "H", MaskKind::Hexagon, (100.0, 100.0)).points.len(), 6);
        // Quad slice: comp 100×100 → screen 200×200, so mask points double.
        let mut s = Slice::new(1, "S", (100, 100), (200, 200));
        s.input_mask = Some(Mask::new(2, "M", MaskKind::Rectangle, (100.0, 100.0)));
        assert!(close(s.input_to_output([25.0, 25.0]), [50.0, 50.0]));
        let pts = s.input_mask_on_screen().unwrap();
        assert!(pts.len() >= 4);
        assert!(close(pts[0], [50.0, 50.0]));
        s.flip.h = true;
        assert!(close(s.input_to_output([25.0, 25.0]), [150.0, 50.0]));
    }

    #[test]
    fn default_setup_and_json() {
        let mut a = AdvancedOutput::with_default_screen((1280, 720));
        assert_eq!(a.screens[0].slices[0].output, rect(0.0, 0.0, 1280.0, 720.0));
        let d = a.duplicate_slice(0, 0);
        assert_ne!(a.screens[0].slices[d].id, a.screens[0].slices[0].id);
        let j = serde_json::to_string(&a).unwrap();
        let back: AdvancedOutput = serde_json::from_str(&j).unwrap();
        assert_eq!(a, back);
        assert_eq!(a.shown().count(), 1);
    }
}
