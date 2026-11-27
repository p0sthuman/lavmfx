//! Output menu + the **Advanced Output** window:
//! presets + screens / slices tree (left), toolbar (zoom, undo, redo, pan,
//! snap, grid · Edit Points / Transform) over the Input Selection / Output
//! Transformation canvas (centre), properties of the selected screen or
//! slice (right), Cancel / Save & Close.
//!
//! Edits apply live; Cancel restores the setup the window
//! opened with; undo / redo step through the edits.

use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, Pos2, Rect, RichText, Stroke, Vec2, pos2, vec2};
use lavmfx_core::output::{
    AdvancedOutput, Device, Homography, Mask, MaskKind, MaskShape, PointMode, PolygonShape, Pt, Screen, Slice, Warp,
    bounds_of, quad,
};

use super::widgets::{Icon, icon_button, section, value_row};
use super::{LavmfxApp, theme};
use crate::output::{OutputMode, letterbox};

mod dmx_ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Input,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutMode {
    /// Corner pin + warp grid points.
    Points,
    /// Move / scale / rotate the whole slice.
    Transform,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Grab {
    Move,
    /// Corner 0..4 (TL, TR, BR, BL).
    Corner(usize),
    /// Edge midpoint 0..4 (top, right, bottom, left).
    Edge(usize),
    /// Warp grid point index.
    Point(usize),
    /// Ring around a corner (Transform): rotate about the centre.
    Rotate,
    /// Pan the view.
    Pan,
    /// Point of the edited mask / polygon.
    Poly(usize),
    /// Move the whole mask / polygon.
    PolyMove,
}

/// What the point handles edit besides the slice quad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PolyT {
    /// Output mask of the screen.
    OutMask(usize),
    /// Input mask of a slice (composition px, Input Selection tab).
    InMask(usize),
    /// Polygon slice output points.
    PolyOut(usize),
    /// Polygon slice input points.
    PolyIn(usize),
}

/// What the pen tool is drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenFor {
    OutputMask,
    Polygon,
    InputMask(usize),
}

/// Where the pointer is (for the Help panel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum HelpArea {
    #[default]
    None,
    Tree,
    Toolbar,
    Canvas,
    Properties,
}

/// Visual size of corner handles.
const HANDLE: f32 = 16.0;
/// Edge / grid point handles.
const HANDLE_SMALL: f32 = 12.0;
/// Pointer distance that still grabs a handle.
const GRAB_R: f32 = 14.0;
/// Rotation ring around corners (Transform): radius band.
const RING: (f32, f32) = (15.0, 26.0);
/// Snap distance in screen points.
const SNAP_PX: f32 = 10.0;
const GRID_STEPS: [u32; 5] = [0, 4, 8, 16, 32];

pub struct Editor {
    /// Setup when the window opened (Cancel).
    snapshot: AdvancedOutput,
    screen: usize,
    slice: Option<usize>,
    tab: Tab,
    mode: OutMode,
    /// 1 = fit.
    zoom: f32,
    /// View offset in screen points.
    pan: Vec2,
    pan_tool: bool,
    snap: bool,
    /// Grid divisions across the canvas (0 = off); snapping uses its lines.
    grid: u32,
    grab: Option<(Grab, [Pt; 4], Pos2)>,
    /// The slice when a point drag started (multi-point moves start from it).
    grab_slice: Option<Slice>,
    /// Selected control points (Edit Points: `Corner` / `Point`) of `points_of`.
    points: Vec<Grab>,
    /// (screen, slice or mask, tab) the point selection belongs to.
    points_of: Option<(usize, usize, bool)>,
    /// Selected output mask (then `slice` is None).
    mask: Option<usize>,
    /// More selected slices of `screen` besides `slice` (Ctrl / Shift+click):
    /// dragged, nudged and deleted together with it.
    also: Vec<usize>,
    /// (screen, slice, slice count) `also` was made for: anything else that
    /// changes the selection or the list (paste, duplicate, undo…) drops it.
    also_key: (usize, Option<usize>, usize),
    /// The other selected slices when a drag started.
    grab_also: Vec<(usize, Slice)>,
    /// Box selection dragged from empty canvas: start (screen), and the
    /// slices it adds to (Ctrl / Shift held at the start).
    marquee: Option<(Pos2, Vec<usize>)>,
    /// Box selection dragged from empty space in the slice list: start
    /// (screen), and the (screen, slices) it adds to (Ctrl / Shift at the start).
    list_marquee: Option<(Pos2, Option<SliceSel>)>,
    /// Pen tool in progress: target + points so far (content px).
    pen: Option<(PenFor, Vec<Pt>)>,
    /// Points of the mask / polygon when its drag started.
    grab_poly: Option<(PolyT, Vec<Pt>)>,
    /// Loaded input guide (path, texture).
    guide: Option<(String, egui::TextureHandle)>,
    help: HelpArea,
    /// Where the last drag snapped (drawn as a ring), content px.
    snap_mark: Option<Pt>,
    undo: Vec<AdvancedOutput>,
    redo: Vec<AdvancedOutput>,
    /// State before the gesture in progress (pushed when the pointer is released).
    pending: Option<AdvancedOutput>,
    /// Last copied slice / mask (also put on the system clipboard as text).
    clipboard: Option<AoClip>,
    /// Selected lumiverse (and fixture): the editor shows DMX instead of the screen.
    dmx: Option<(usize, Option<usize>)>,
    dmx_grab: Option<dmx_ui::DmxGrab>,
    /// Fixture library (loaded when first needed; None = reload).
    library: Option<Vec<lavmfx_core::dmx::Personality>>,
}

/// Selected slices of one screen: (screen, slices).
type SliceSel = (usize, Vec<usize>);

/// A slice / output mask row dragged in the tree (reorders within its list).
#[derive(Debug, Clone, Copy)]
struct TreeDrag {
    screen: usize,
    mask: bool,
    index: usize,
}

/// Move `v[from]` to index `to` (others shift); returns where it landed.
fn reorder<T>(v: &mut Vec<T>, from: usize, to: usize) -> usize {
    if from >= v.len() {
        return from;
    }
    let to = to.min(v.len() - 1);
    let item = v.remove(from);
    v.insert(to, item);
    to
}

/// Tree row: drag to reorder within its list (slices / masks of one screen),
/// with a drop line. Returns (from, to) when something was dropped on it.
fn tree_dnd(ui: &egui::Ui, r: &egui::Response, me: TreeDrag) -> Option<(usize, usize)> {
    r.dnd_set_drag_payload(me);
    let same = |p: &TreeDrag| p.screen == me.screen && p.mask == me.mask && p.index != me.index;
    if let Some(p) = r.dnd_hover_payload::<TreeDrag>().filter(|p| same(p)) {
        // Rows list the top first: moving up the stack lands above this row.
        let y = if p.index < me.index {
            r.rect.top()
        } else {
            r.rect.bottom()
        };
        let x = ui.max_rect().x_range();
        ui.painter().hline(x, y, Stroke::new(2.0, theme::pal(ui).accent));
    }
    r.dnd_release_payload::<TreeDrag>()
        .filter(|p| same(p))
        .map(|p| (p.index, me.index))
}

impl Editor {
    pub fn new(setup: &AdvancedOutput) -> Self {
        Self {
            snapshot: setup.clone(),
            screen: 0,
            slice: setup
                .screens
                .first()
                .filter(|s| !s.slices.is_empty())
                .map(|s| s.slices.len() - 1),
            tab: Tab::Output,
            mode: OutMode::Transform,
            zoom: 1.0,
            pan: Vec2::ZERO,
            pan_tool: false,
            snap: true,
            grid: 0,
            grab: None,
            grab_slice: None,
            points: Vec::new(),
            points_of: None,
            mask: None,
            also: Vec::new(),
            also_key: (0, None, 0),
            grab_also: Vec::new(),
            marquee: None,
            list_marquee: None,
            pen: None,
            grab_poly: None,
            guide: None,
            help: HelpArea::None,
            snap_mark: None,
            undo: Vec::new(),
            redo: Vec::new(),
            pending: None,
            clipboard: None,
            dmx: None,
            dmx_grab: None,
            library: None,
        }
    }
}

/// Drop the extra selection when the selection or the slice list changed
/// elsewhere (its indices would point at other slices).
fn check_also(ed: &mut Editor, n: usize) {
    if ed.also_key != (ed.screen, ed.slice, n) {
        ed.also.clear();
    }
    ed.also.retain(|&i| i < n && Some(i) != ed.slice);
    ed.also.dedup();
    ed.also_key = (ed.screen, ed.slice, n);
}

/// Ctrl / Shift+click on slice `li` (of `n`): into the selection or out of it.
fn toggle_select(ed: &mut Editor, li: usize, n: usize) {
    if ed.slice == Some(li) {
        ed.slice = (!ed.also.is_empty()).then(|| ed.also.remove(0));
    } else if let Some(k) = ed.also.iter().position(|&x| x == li) {
        ed.also.remove(k);
    } else if ed.slice.is_none() {
        ed.slice = Some(li);
    } else {
        ed.also.push(li);
    }
    ed.mask = None;
    ed.also_key = (ed.screen, ed.slice, n);
}

impl LavmfxApp {
    /// Box selection in the slice list: every slice row the box touches, in
    /// the screen of the first one (or of the selection it adds to).
    fn list_box_select(&self, ui: &egui::Ui, ed: &mut Editor, rows: &[(usize, usize, Rect)], bg: &egui::Response) {
        let Some((start, base)) = &ed.list_marquee else { return };
        let Some(p) = ui.input(|i| i.pointer.latest_pos()).filter(|_| bg.dragged()) else {
            ed.list_marquee = None;
            return;
        };
        let rect = Rect::from_two_pos(*start, p);
        let hits: Vec<(usize, usize)> = rows
            .iter()
            .filter(|(.., r)| r.intersects(rect))
            .map(|&(s, l, _)| (s, l))
            .collect();
        let screen = base.as_ref().map(|b| b.0).or(hits.first().map(|h| h.0));
        if let Some(si) = screen {
            let mut sel: Vec<usize> = base.as_ref().map(|b| b.1.clone()).unwrap_or_default();
            for &(s, l) in &hits {
                if s == si && !sel.contains(&l) {
                    sel.push(l);
                }
            }
            let n = self.outputs.setup.screens.get(si).map_or(0, |s| s.slices.len());
            // The lead stays if it's still selected, else the top-most slice.
            let lead = ed
                .slice
                .filter(|l| ed.screen == si && sel.contains(l))
                .or(sel.iter().max().copied());
            sel.retain(|&l| Some(l) != lead && l < n);
            ed.screen = si;
            ed.slice = lead;
            ed.also = sel;
            ed.mask = None;
            ed.dmx = None;
            ed.also_key = (si, lead, n);
        }
        let accent = theme::pal(ui).accent;
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, accent.gamma_multiply(0.12));
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0, accent), egui::StrokeKind::Inside);
    }
}

/// Shift+click on slice `li` (of `n`): the selected slice stays the lead and
/// every slice between it and `li` joins it.
fn range_select(ed: &mut Editor, li: usize, n: usize) {
    let Some(a) = ed.slice else {
        return toggle_select(ed, li, n);
    };
    ed.also = (a.min(li)..=a.max(li)).filter(|&i| i != a && i < n).collect();
    ed.mask = None;
    ed.also_key = (ed.screen, ed.slice, n);
}

/// `start` moved by `d` on `tab`'s side: its quad and its polygon's points.
fn shifted(start: &Slice, tab: Tab, d: Pt) -> Slice {
    let mut sl = start.clone();
    let (q, poly) = match tab {
        Tab::Input => (&mut sl.input, sl.polygon.as_mut().map(|p| &mut p.input)),
        Tab::Output => (&mut sl.output, sl.polygon.as_mut().map(|p| &mut p.output)),
    };
    quad::translate(q, d);
    if let Some(pts) = poly {
        quad::translate(pts, d);
    }
    sl
}

/// A copied slice or output mask (Ctrl+C / Ctrl+X, pasted with Ctrl+V).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
enum AoClip {
    Slice(Box<Slice>),
    Mask(Mask),
}

/// Prefix of a copied slice / mask on the system clipboard (JSON after it).
const CLIP_MARK: &str = "lavmfx-advanced-output:";
/// Pasted copies land this far (content px) right and down of the original.
const PASTE_OFFSET: f32 = 20.0;

impl AoClip {
    fn to_text(&self) -> String {
        format!("{CLIP_MARK}{}", serde_json::to_string(self).unwrap_or_default())
    }
    fn from_text(t: &str) -> Option<Self> {
        serde_json::from_str(t.trim().strip_prefix(CLIP_MARK)?).ok()
    }
}

/// Put a copy of `clip` (new `id`) on `screen`, offset by `d`; returns the new
/// slice / mask index (slices go on top).
fn paste_clip(screen: &mut Screen, clip: &AoClip, id: u64, d: Pt) -> (Option<usize>, Option<usize>) {
    match clip {
        AoClip::Slice(sl) => {
            let mut c = (**sl).clone();
            c.id = id;
            c.name = format!("{} copy", sl.name);
            quad::translate(&mut c.output, d);
            quad::translate(&mut c.input, d);
            if let Some(g) = &mut c.polygon {
                quad::translate(&mut g.output, d);
                quad::translate(&mut g.input, d);
            }
            if let Some(m) = &mut c.input_mask {
                quad::translate(&mut m.points, d);
            }
            screen.slices.push(c);
            (Some(screen.slices.len() - 1), None)
        }
        AoClip::Mask(m) => {
            let mut c = m.clone();
            c.id = id;
            c.name = format!("{} copy", m.name);
            quad::translate(&mut c.points, d);
            screen.masks.push(c);
            (None, Some(screen.masks.len() - 1))
        }
    }
}

/// An installed app's preferences folder (default folder for "Import Arena setup").
fn import_prefs_dir() -> Option<std::path::PathBuf> {
    lavmfx_project::paths::import_prefs_dirs().into_iter().next()
}

fn presets_dir() -> std::path::PathBuf {
    super::config_dir().join("output-presets")
}

fn preset_path(name: &str) -> std::path::PathBuf {
    presets_dir().join(format!("{name}.json"))
}

// ------------------------------------------------------------ snapping

/// Snap targets in content pixels: other slices' corners and sides, the
/// canvas border / centre lines, grid lines.
struct Snap {
    verts: Vec<Pt>,
    segs: Vec<(Pt, Pt)>,
    xs: Vec<f32>,
    ys: Vec<f32>,
    /// Distance in content pixels.
    thr: f32,
}

fn dist(a: Pt, b: Pt) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn closest_on_seg(p: Pt, (a, b): (Pt, Pt)) -> Pt {
    let d = [b[0] - a[0], b[1] - a[1]];
    let l2 = d[0] * d[0] + d[1] * d[1];
    if l2 < 1e-9 {
        return a;
    }
    let t = (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / l2).clamp(0.0, 1.0);
    [a[0] + d[0] * t, a[1] + d[1] * t]
}

impl Snap {
    /// Snap a dragged point: a corner first, then a side, then x / y lines.
    fn point(&self, p: Pt) -> (Pt, Option<Pt>) {
        let hit = self.point_no_lines(p);
        if hit.1.is_some() {
            return hit;
        }
        let mut out = p;
        let mut hit = false;
        if let Some(x) = nearest(&self.xs, p[0], self.thr) {
            out[0] = x;
            hit = true;
        }
        if let Some(y) = nearest(&self.ys, p[1], self.thr) {
            out[1] = y;
            hit = true;
        }
        (out, hit.then_some(out))
    }

    /// Snap a moved quad: adjust the move so one of its corners lands on a
    /// target corner / side, else align corners with x / y lines.
    fn delta(&self, q: &[Pt], d: Pt) -> (Pt, Option<Pt>) {
        let moved: Vec<Pt> = q.iter().map(|v| [v[0] + d[0], v[1] + d[1]]).collect();
        let mut best: Option<(f32, Pt, Pt)> = None;
        for &v in &moved {
            let (s, mark) = self.point_no_lines(v);
            if let Some(m) = mark {
                let e = dist(s, v);
                if best.is_none_or(|b| e < b.0) {
                    best = Some((e, [s[0] - v[0], s[1] - v[1]], m));
                }
            }
        }
        if let Some((_, off, m)) = best {
            return ([d[0] + off[0], d[1] + off[1]], Some(m));
        }
        let mut out = d;
        let mut mark = None;
        let fx = moved
            .iter()
            .filter_map(|v| nearest(&self.xs, v[0], self.thr).map(|x| (x - v[0], *v)))
            .min_by(|a, b| a.0.abs().total_cmp(&b.0.abs()));
        let fy = moved
            .iter()
            .filter_map(|v| nearest(&self.ys, v[1], self.thr).map(|y| (y - v[1], *v)))
            .min_by(|a, b| a.0.abs().total_cmp(&b.0.abs()));
        if let Some((o, v)) = fx {
            out[0] += o;
            mark = Some([v[0] + o, v[1]]);
        }
        if let Some((o, v)) = fy {
            out[1] += o;
            mark = Some([mark.map_or(v[0], |m| m[0]), v[1] + o]);
        }
        (out, mark)
    }

    /// Corners, then sides.
    fn point_no_lines(&self, p: Pt) -> (Pt, Option<Pt>) {
        let v = self
            .verts
            .iter()
            .map(|&v| (v, dist(v, p)))
            .filter(|(_, d)| *d <= self.thr)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some(v) = v {
            return (v.0, Some(v.0));
        }
        let s = self
            .segs
            .iter()
            .map(|&s| closest_on_seg(p, s))
            .map(|q| (q, dist(q, p)))
            .filter(|(_, d)| *d <= self.thr)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        match s {
            Some(s) => (s.0, Some(s.0)),
            None => (p, None),
        }
    }
}

fn nearest(lines: &[f32], v: f32, thr: f32) -> Option<f32> {
    lines
        .iter()
        .copied()
        .filter(|l| (l - v).abs() <= thr)
        .min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs()))
}

impl LavmfxApp {
    // ------------------------------------------------------------ menu

    pub(super) fn output_menu(&mut self, ui: &mut egui::Ui) {
        let mode = self.outputs.mode;
        let mut set = None;
        if ui
            .radio(mode == OutputMode::Disabled, "Disabled      Ctrl+Shift+D")
            .clicked()
        {
            set = Some(OutputMode::Disabled);
        }
        ui.separator();
        let monitors: Vec<String> = if self.outputs.monitors.is_empty() {
            vec!["Display 1".into()]
        } else {
            self.outputs
                .monitors
                .iter()
                .enumerate()
                .map(|(i, m)| format!("Display {} ({}×{})", i + 1, m.size.0, m.size.1))
                .collect()
        };
        ui.label(RichText::new("Fullscreen").weak());
        for (i, m) in monitors.iter().enumerate() {
            let label = if i < 9 {
                format!("{m}      Ctrl+Shift+{}", i + 1)
            } else {
                m.clone()
            };
            if ui
                .radio(mode == OutputMode::Fullscreen(i), label)
                .on_hover_text(self.monitor_name(i))
                .clicked()
            {
                set = Some(OutputMode::Fullscreen(i));
            }
        }
        ui.label(RichText::new("Windowed").weak());
        for (i, m) in monitors.iter().enumerate() {
            if ui
                .radio(mode == OutputMode::Windowed(i), m)
                .on_hover_text(self.monitor_name(i))
                .clicked()
            {
                set = Some(OutputMode::Windowed(i));
            }
        }
        ui.separator();
        if ui
            .radio(mode == OutputMode::Advanced, "Advanced…      Ctrl+Shift+A")
            .clicked()
        {
            set = Some(OutputMode::Advanced);
            self.open_screens_editor();
        }
        if ui
            .button("Slice Chase…")
            .on_hover_text("Sequences of slices for the Slice Chase effects")
            .clicked()
        {
            self.open_slice_chase();
            ui.close();
        }
        ui.separator();
        use crate::live_out::Kind;
        let mut ndi = self.prefs.ndi.output;
        let r = ui
            .checkbox(&mut ndi, format!("NDI Output (\"{}\")", self.prefs.ndi.name))
            .on_hover_text(format!(
                "Send the composition as an NDI source · name in Preferences › Video\n{}",
                self.live_out.status(Kind::Ndi)
            ));
        if r.changed() {
            self.prefs.ndi.output = ndi;
            self.prefs_dirty = Some(std::time::Instant::now());
        }
        let backend = lavmfx_media::share::backend();
        let mut share = self.prefs.share.output;
        let r = ui
            .checkbox(&mut share, format!("{backend} Output (\"{}\")", self.prefs.share.name))
            .on_hover_text(format!(
                "Share the composition with other apps on this computer ({backend}) · name in Preferences › Video\n{}",
                self.live_out.status(Kind::Share)
            ));
        if r.changed() {
            self.prefs.share.output = share;
            self.prefs_dirty = Some(std::time::Instant::now());
        }
        ui.separator();
        if ui.button("Identify Displays").clicked() {
            self.outputs.identify();
        }
        ui.checkbox(&mut self.outputs.test_card, "Show Test Card      Ctrl+Shift+L");
        ui.checkbox(&mut self.outputs.display_info, "Show Display Info");
        ui.checkbox(&mut self.outputs.show_pointer, "Show Mouse Pointer")
            .on_hover_text(
                "A thin + crosshair on the outputs where the mouse points: over the Composition Monitor \
             (composition, through every slice that shows that spot) or the Advanced Output editor \
             (its screen), or directly over an output window",
            );
        ui.label(RichText::new("F11 / double-click in an output: fullscreen").weak());
        if let Some(m) = set {
            self.outputs.set_mode(m);
            ui.close();
        }
    }

    fn monitor_name(&self, i: usize) -> String {
        self.outputs.monitors.get(i).map(|m| m.name.clone()).unwrap_or_default()
    }

    pub(super) fn output_shortcuts(&mut self, ctx: &egui::Context) {
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(cs, Key::D))) {
            self.outputs.set_mode(OutputMode::Disabled);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(cs, Key::A))) {
            self.outputs.set_mode(OutputMode::Advanced);
            self.open_screens_editor();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(cs, Key::L))) {
            self.outputs.test_card = !self.outputs.test_card;
        }
        let digits = [
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
            Key::Num7,
            Key::Num8,
            Key::Num9,
        ];
        for (i, k) in digits.into_iter().enumerate() {
            if ctx.input_mut(|inp| inp.consume_shortcut(&KeyboardShortcut::new(cs, k))) {
                self.outputs.set_mode(OutputMode::Fullscreen(i));
            }
        }
    }

    pub(super) fn open_screens_editor(&mut self) {
        if self.screens_editor.is_none() {
            if self.outputs.setup.screens.is_empty() {
                self.outputs.setup = AdvancedOutput::with_default_screen(self.outputs.comp_size());
            }
            self.screens_editor = Some(Editor::new(&self.outputs.setup));
        }
    }

    // ------------------------------------------------------------ window

    pub(super) fn screens_editor_window(&mut self, ctx: &egui::Context) {
        if self.screens_editor.is_none() {
            return;
        }
        let id = egui::ViewportId::from_hash_of("lavmfx-advanced-output");
        let b = egui::ViewportBuilder::default()
            .with_title("Advanced Output")
            .with_app_id("lavmfx-advanced-output")
            .with_inner_size([1400.0, 800.0]);
        ctx.show_viewport_immediate(id, b, |ui, _| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                self.close_editor(true);
                return;
            }
            self.editor_ui(ui);
        });
    }

    fn close_editor(&mut self, keep: bool) {
        if let Some(ed) = self.screens_editor.take() {
            if !keep {
                self.outputs.setup = ed.snapshot;
            }
            self.outputs.save_now();
        }
    }

    fn undo_step(&mut self, ed: &mut Editor, redo: bool) {
        if let Some(p) = ed.pending.take() {
            ed.undo.push(p);
            ed.redo.clear();
        }
        let (from, to) = if redo {
            (&mut ed.redo, &mut ed.undo)
        } else {
            (&mut ed.undo, &mut ed.redo)
        };
        if let Some(s) = from.pop() {
            to.push(std::mem::replace(&mut self.outputs.setup, s));
            self.outputs.touch();
        }
    }

    fn editor_ui(&mut self, ui: &mut egui::Ui) {
        let Some(mut ed) = self.screens_editor.take() else {
            return;
        };
        super::widgets::guard_wheel(ui.ctx());
        let comp = self.outputs.comp_size();

        // Undo / redo first, so they don't count as edits of this frame.
        let (undo, redo) = ui.input_mut(|i| {
            let redo = i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z))
                || i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Y));
            let undo = i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Z));
            (undo, redo)
        });
        if undo || redo {
            self.undo_step(&mut ed, redo);
        }

        // Keep the selection valid after edits elsewhere.
        if self.outputs.setup.screens.is_empty() {
            self.outputs.setup.add_screen(comp, Device::Window);
        }
        ed.screen = ed.screen.min(self.outputs.setup.screens.len() - 1);
        let n = self.outputs.setup.screens[ed.screen].slices.len();
        if ed.slice.is_some_and(|s| s >= n) {
            ed.slice = n.checked_sub(1);
        }
        let nm = self.outputs.setup.screens[ed.screen].masks.len();
        if ed.mask.is_some_and(|m| m >= nm) {
            ed.mask = None;
        }
        if ed.mask.is_some() {
            ed.slice = None;
        }
        if let Some((li, fi)) = ed.dmx {
            let ls = &self.outputs.setup.lumiverses;
            ed.dmx = (li < ls.len()).then(|| (li, fi.filter(|&f| f < ls[li].fixtures.len())));
        }
        // Input guide: an image dropped on the window, then (re)load its texture.
        let dropped = ui.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .find(|p| !p.as_os_str().is_empty())
        });
        if let Some(path) = dropped {
            self.outputs.setup.screens[ed.screen].input_guide = Some(path.to_string_lossy().into());
            ed.tab = Tab::Input;
            self.outputs.touch();
        }
        self.ensure_guide(ui.ctx(), &mut ed);
        let before = self.outputs.setup.clone();
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let mut areas: Vec<(Rect, HelpArea)> = Vec::new();

        let mut close = None;
        egui::Panel::bottom("ao-buttons").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Save & Close").clicked() {
                        close = Some(true);
                    }
                    if ui
                        .button("Cancel")
                        .on_hover_text("Undo every change made in this window")
                        .clicked()
                    {
                        close = Some(false);
                    }
                });
            });
        });
        let tree = egui::Panel::left("ao-tree")
            .resizable(true)
            .default_size(260.0)
            .show(ui, |ui| {
                egui::Panel::bottom("ao-help")
                    .resizable(false)
                    .default_size(130.0)
                    .show(ui, |ui| {
                        super::widgets::section_header(ui, "ao-help-band", "Help", true, false);
                        ui.add(egui::Label::new(RichText::new(help_text(&ed)).weak()).wrap());
                    });
                self.presets_menu(ui, &mut ed);
                ui.separator();
                // Full height, so a box selection can start below the list.
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    self.tree(ui, &mut ed);
                    if !self.outputs.setup.lumiverses.is_empty() {
                        ui.separator();
                        self.dmx_tree(ui, &mut ed);
                    }
                });
            });
        areas.push((tree.response.rect, HelpArea::Tree));
        let props = egui::Panel::right("ao-props")
            .resizable(true)
            .default_size(330.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if ed.dmx.is_some() {
                        self.dmx_props(ui, &mut ed);
                    } else {
                        self.properties(ui, &mut ed);
                    }
                });
            });
        areas.push((props.response.rect, HelpArea::Properties));
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut ed.tab, Tab::Input, "Input Selection");
                let out_tab = if ed.dmx.is_some() {
                    "DMX Output"
                } else {
                    "Output Transformation"
                };
                ui.selectable_value(&mut ed.tab, Tab::Output, out_tab);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("Composition {}×{}", comp.0, comp.1)).weak());
                });
            });
            let bar = ui.scope(|ui| self.ao_toolbar(ui, &mut ed)).response.rect;
            areas.push((bar, HelpArea::Toolbar));
            areas.push((ui.available_rect_before_wrap(), HelpArea::Canvas));
            if ed.dmx.is_some() {
                self.dmx_canvas(ui, &mut ed);
            } else {
                self.canvas(ui, &mut ed);
            }
        });
        ed.help = pointer
            .and_then(|p| areas.iter().find(|(r, _)| r.contains(p)).map(|a| a.1))
            .unwrap_or_default();

        // History: one step per gesture (a drag, a typed value, a key).
        if self.outputs.setup != before && ed.pending.is_none() {
            ed.pending = Some(before);
        }
        if ed.pending.is_some() && !ui.input(|i| i.pointer.any_down()) {
            ed.undo.extend(ed.pending.take());
            ed.redo.clear();
            if ed.undo.len() > 200 {
                ed.undo.remove(0);
            }
        }

        match close {
            Some(keep) => {
                self.screens_editor = Some(ed);
                self.close_editor(keep);
            }
            None => self.screens_editor = Some(ed),
        }
    }

    /// Toolbar row above the canvas.
    fn ao_toolbar(&mut self, ui: &mut egui::Ui, ed: &mut Editor) {
        let sz = vec2(30.0, 26.0);
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("ao-zoom")
                .width(70.0)
                .selected_text(format!("{:.0}%", ed.zoom * 100.0))
                .show_ui(ui, |ui| {
                    for z in [0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 4.0] {
                        if ui.selectable_label(ed.zoom == z, format!("{:.0}%", z * 100.0)).clicked() {
                            ed.zoom = z;
                            ed.pan = Vec2::ZERO;
                        }
                    }
                })
                .response
                .on_hover_text("Zoom (100 % = fit). Ctrl+wheel zooms, middle-drag pans");
            let can_undo = !ed.undo.is_empty() || ed.pending.is_some();
            if icon_button(ui, Icon::Undo, false, sz, can_undo).on_hover_text("Undo (Ctrl+Z)").clicked() {
                self.undo_step(ed, false);
            }
            if icon_button(ui, Icon::Redo, false, sz, !ed.redo.is_empty())
                .on_hover_text("Redo (Ctrl+Shift+Z)")
                .clicked()
            {
                self.undo_step(ed, true);
            }
            if icon_button(ui, Icon::Hand, ed.pan_tool, sz, true)
                .on_hover_text("Pan: drag to move the view (middle-drag always pans)")
                .clicked()
            {
                ed.pan_tool = !ed.pan_tool;
            }
            if icon_button(ui, Icon::Magnet, ed.snap, sz, true)
                .on_hover_text("Snap: dragged corners, sides and slices stick to other slices, the canvas edges / centre and the grid (hold Ctrl to drag freely)")
                .clicked()
            {
                ed.snap = !ed.snap;
            }
            let grid = icon_button(ui, Icon::Grid, ed.grid > 0, sz, true).on_hover_text("Grid overlay (snaps when Snap is on)");
            egui::Popup::menu(&grid).show(|ui| {
                for g in GRID_STEPS {
                    let label = if g == 0 { "Off".to_string() } else { format!("{g} × {g}") };
                    if ui.selectable_label(ed.grid == g, label).clicked() {
                        ed.grid = g;
                    }
                }
            });
            // Input Selection: the switch only matters for polygon slices
            // (their input points vs. their box).
            let polygon_selected = ed
                .slice
                .and_then(|li| self.outputs.setup.screens.get(ed.screen)?.slices.get(li))
                .is_some_and(|sl| sl.polygon.is_some());
            if (ed.tab == Tab::Output || polygon_selected) && ed.dmx.is_none() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.selectable_value(&mut ed.mode, OutMode::Transform, "Transform")
                        .on_hover_text("Move, scale (corner / side squares) and rotate (rings) the whole slice");
                    ui.selectable_value(&mut ed.mode, OutMode::Points, "+ Edit Points").on_hover_text(
                        "Drag corners (corner pin) and warp grid points; on a polygon slice, its points",
                    );
                });
            }
        });
    }

    fn presets_menu(&mut self, ui: &mut egui::Ui, ed: &mut Editor) {
        let comp = self.outputs.comp_size();
        let title = self.outputs.preset.clone().unwrap_or_else(|| "Presets".into());
        let mut load: Option<AdvancedOutput> = None;
        ui.horizontal(|ui| {
            ui.menu_button(title, |ui| {
                if ui
                    .button("New")
                    .on_hover_text("Start over: one screen with one slice")
                    .clicked()
                {
                    ui.close();
                    load = Some(AdvancedOutput::with_default_screen(comp));
                    self.outputs.preset = None;
                }
                if let Some(name) = self.outputs.preset.clone()
                    && ui.button(format!("Save \"{name}\"")).clicked()
                {
                    ui.close();
                    self.write_preset(&preset_path(&name));
                }
                if ui.button("Save As…").clicked() {
                    ui.close();
                    let dir = presets_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    if let Some(p) = rfd::FileDialog::new()
                        .set_directory(&dir)
                        .add_filter("Output preset", &["json"])
                        .set_file_name(format!("{}.json", self.outputs.preset.as_deref().unwrap_or("Preset")))
                        .save_file()
                    {
                        self.write_preset(&p);
                        self.outputs.preset = p.file_stem().map(|s| s.to_string_lossy().to_string());
                        self.outputs.touch();
                    }
                }
                if ui.button("Import Arena setup (.xml)…").clicked() {
                    ui.close();
                    let mut d = rfd::FileDialog::new().add_filter("Arena advanced output", &["xml"]);
                    if let Some(dir) = import_prefs_dir() {
                        d = d.set_directory(dir);
                    }
                    if let Some(p) = d.pick_file() {
                        match lavmfx_project::screens::import_file(&p) {
                            Ok(a) => {
                                load = Some(a);
                                self.outputs.preset = None;
                                self.status = format!("Imported {}", p.display());
                            }
                            Err(e) => self.status = format!("Can't import {}: {e}", p.display()),
                        }
                    }
                }
                if let Some(name) = self.outputs.preset.clone()
                    && ui.button(format!("Remove \"{name}\"")).clicked()
                {
                    ui.close();
                    let _ = std::fs::remove_file(preset_path(&name));
                    self.outputs.preset = None;
                    self.outputs.touch();
                }
                let mut files: Vec<_> = std::fs::read_dir(presets_dir())
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "json"))
                    .collect();
                files.sort();
                if !files.is_empty() {
                    ui.separator();
                }
                for p in files {
                    let name = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
                    let on = self.outputs.preset.as_deref() == Some(name.as_str());
                    if ui.radio(on, &name).clicked() {
                        ui.close();
                        match std::fs::read_to_string(&p)
                            .map_err(|e| e.to_string())
                            .and_then(|s| serde_json::from_str::<AdvancedOutput>(&s).map_err(|e| e.to_string()))
                        {
                            Ok(a) => {
                                load = Some(a);
                                self.outputs.preset = Some(name);
                            }
                            Err(e) => self.status = format!("Can't load preset {name}: {e}"),
                        }
                    }
                }
            });
            ui.menu_button("+", |ui| {
                if ui.button("Screen").clicked() {
                    ed.screen = self.outputs.setup.add_screen(comp, Device::Window);
                    ed.slice = Some(self.outputs.setup.add_slice(ed.screen, comp));
                    self.outputs.touch();
                    ui.close();
                }
                if ui.button("Slice").clicked() {
                    ed.slice = Some(self.outputs.setup.add_slice(ed.screen, comp));
                    self.outputs.touch();
                    ui.close();
                }
                let sz = vec2(26.0, 22.0);
                ui.horizontal(|ui| {
                    ui.label("Polygon");
                    ui.add_space((90.0 - ui.min_rect().width()).max(0.0));
                    for (icon, shape, tip) in [
                        (Icon::ShapeTriangle, PolygonShape::Triangle, "Triangle slice"),
                        (Icon::ShapeHexagon, PolygonShape::Hexagon, "Hexagon slice (6 points)"),
                        (Icon::ShapeEllipse, PolygonShape::Circle, "Circle slice (32 points)"),
                    ] {
                        if icon_button(ui, icon, false, sz, true).on_hover_text(tip).clicked() {
                            ed.slice = Some(self.outputs.setup.add_polygon_shape(ed.screen, comp, shape));
                            ed.mask = None;
                            ed.tab = Tab::Output;
                            self.outputs.touch();
                            ui.close();
                        }
                    }
                    if icon_button(ui, Icon::Pen, false, sz, true)
                        .on_hover_text(
                            "Draw a polygon slice: click its points, click the first point or press Enter to finish",
                        )
                        .clicked()
                    {
                        ed.tab = Tab::Output;
                        ed.pen = Some((PenFor::Polygon, Vec::new()));
                        ui.close();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Output Mask");
                    ui.add_space((90.0 - ui.min_rect().width()).max(0.0));
                    for (icon, kind, tip) in [
                        (Icon::ShapeRect, MaskKind::Rectangle, "Rectangle"),
                        (Icon::ShapeEllipse, MaskKind::Ellipse, "Ellipse"),
                        (Icon::ShapeTriangle, MaskKind::Triangle, "Triangle"),
                        (Icon::ShapeHexagon, MaskKind::Hexagon, "Hexagon"),
                    ] {
                        if icon_button(ui, icon, false, sz, true)
                            .on_hover_text(format!("{tip} output mask"))
                            .clicked()
                        {
                            ed.mask = Some(self.outputs.setup.add_mask(ed.screen, kind));
                            ed.slice = None;
                            ed.tab = Tab::Output;
                            self.outputs.touch();
                            ui.close();
                        }
                    }
                    if icon_button(ui, Icon::Pen, false, sz, true)
                        .on_hover_text(
                            "Draw an output mask: click its points, click the first point or press Enter to finish",
                        )
                        .clicked()
                    {
                        ed.tab = Tab::Output;
                        ed.pen = Some((PenFor::OutputMask, Vec::new()));
                        ui.close();
                    }
                });
                ui.separator();
                self.dmx_add_menu(ui, ed);
            })
            .response
            .on_hover_text("Add a screen or a slice");
        });
        if let Some(a) = load {
            self.outputs.setup = a;
            ed.screen = 0;
            ed.slice = self
                .outputs
                .setup
                .screens
                .first()
                .and_then(|s| s.slices.len().checked_sub(1));
            self.outputs.touch();
        }
    }

    /// Load / drop the input guide texture to match the selected screen.
    fn ensure_guide(&mut self, ctx: &egui::Context, ed: &mut Editor) {
        let want = self.outputs.setup.screens[ed.screen].input_guide.clone();
        if ed.guide.as_ref().map(|g| &g.0) == want.as_ref() {
            return;
        }
        ed.guide = None;
        let Some(path) = want else { return };
        match lavmfx_media::thumbnail(std::path::Path::new(&path), 4096, 4096) {
            Ok(img) => {
                let ci = egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.rgba);
                let tex = ctx.load_texture("ao-input-guide", ci, egui::TextureOptions::LINEAR);
                ed.guide = Some((path, tex));
            }
            Err(e) => {
                self.status = format!("Can't load the input guide {path}: {e}");
                self.outputs.setup.screens[ed.screen].input_guide = None;
            }
        }
    }

    fn write_preset(&mut self, p: &std::path::Path) {
        let j = serde_json::to_string_pretty(&self.outputs.setup).unwrap_or_default();
        let res = p
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|_| std::fs::write(p, j));
        self.status = match res {
            Ok(()) => format!("Saved preset {}", p.display()),
            Err(e) => format!("Can't save {}: {e}", p.display()),
        };
    }

    fn tree(&mut self, ui: &mut egui::Ui, ed: &mut Editor) {
        let pal = theme::pal(ui);
        let mut action: Option<(usize, Option<usize>, &str)> = None;
        // Dropped row: (screen, mask?, from, to).
        let mut dropped: Option<(usize, bool, usize, usize)> = None;
        // Drag on empty list space (rows on top win): box selection of slices.
        let list = ui.clip_rect();
        let bg = ui.interact(list, ui.scope_id().with("tree-bg"), egui::Sense::drag());
        if bg.drag_started()
            && let Some(p) = ui.input(|i| i.pointer.press_origin())
        {
            let add = ui.input(|i| i.modifiers.command || i.modifiers.shift);
            let base = (add && ed.dmx.is_none())
                .then(|| (ed.screen, ed.slice.into_iter().chain(ed.also.iter().copied()).collect()));
            ed.list_marquee = Some((p, base));
        }
        // Slice rows (screen, slice, row rect) for the box selection.
        let mut rows: Vec<(usize, usize, Rect)> = Vec::new();
        ui.scope(|ui| {
            for (si, s) in self.outputs.setup.screens.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut s.enabled, "").on_hover_text("Screen on / off").changed() {
                        action = Some((si, None, "touch"));
                    }
                    let sel = ed.screen == si && ed.slice.is_none() && ed.mask.is_none();
                    let r = ui.selectable_label(sel, RichText::new(&s.name).strong());
                    if r.clicked() {
                        ed.screen = si;
                        ed.slice = None;
                        ed.mask = None;
                        ed.dmx = None;
                    }
                    r.context_menu(|ui| {
                        if ui.button("Delete Screen").clicked() {
                            action = Some((si, None, "delete"));
                            ui.close();
                        }
                    });
                });
                let dev = match &s.device {
                    Device::Virtual => "Virtual Output".to_string(),
                    Device::Window => "Window".to_string(),
                    Device::Display { index, .. } => format!("Display {}", index + 1),
                };
                ui.label(RichText::new(format!("    {dev} ({}×{})", s.width, s.height)).small().weak());
                // Output masks are drawn above the slices: listed first.
                for mi in (0..s.masks.len()).rev() {
                    let m = &mut s.masks[mi];
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        if ui.checkbox(&mut m.enabled, "").on_hover_text("Mask on / off").changed() {
                            action = Some((si, Some(mi), "touch"));
                        }
                        let sel = ed.screen == si && ed.mask == Some(mi);
                        let text = if sel {
                            RichText::new(&m.name).color(pal.on_accent)
                        } else {
                            RichText::new(&m.name).color(pal.pending)
                        };
                        let r = ui
                            .add(
                                egui::Button::selectable(sel, text)
                                    .fill(if sel { pal.pending } else { Color32::TRANSPARENT })
                                    .sense(egui::Sense::click_and_drag()),
                            )
                            .on_hover_text("Output mask: blacks out this part of the screen (Invert: everything else). Drag to reorder.");
                        if r.clicked() {
                            ed.screen = si;
                            ed.mask = Some(mi);
                            ed.slice = None;
                            ed.dmx = None;
                        }
                        if let Some((from, to)) = tree_dnd(ui, &r, TreeDrag { screen: si, mask: true, index: mi }) {
                            dropped = Some((si, true, from, to));
                        }
                        r.context_menu(|ui| {
                            if ui.button(if m.invert { "Don't Invert" } else { "Invert" }).clicked() {
                                action = Some((si, Some(mi), "minvert"));
                                ui.close();
                            }
                            for (label, a) in [
                                ("Bring to Front", "mfront"),
                                ("Move Up", "mup"),
                                ("Move Down", "mdown"),
                                ("Send to Back", "mback"),
                            ] {
                                if ui.button(label).clicked() {
                                    action = Some((si, Some(mi), a));
                                    ui.close();
                                }
                            }
                            if ui.button("Delete").clicked() {
                                action = Some((si, Some(mi), "mdelete"));
                                ui.close();
                            }
                        });
                    });
                }
                // Top slice first.
                let n_slices = s.slices.len();
                for li in (0..n_slices).rev() {
                    let sl = &mut s.slices[li];
                    let row = ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        if ui.checkbox(&mut sl.enabled, "").on_hover_text("Slice on / off").changed() {
                            action = Some((si, Some(li), "touch"));
                        }
                        let sel = ed.screen == si && (ed.slice == Some(li) || ed.also.contains(&li));
                        let text = if sel {
                            RichText::new(&sl.name).color(pal.on_accent)
                        } else {
                            RichText::new(&sl.name)
                        };
                        let r = ui
                            .add(
                                egui::Button::selectable(sel, text)
                                    .fill(if sel { pal.accent } else { Color32::TRANSPARENT })
                                    .sense(egui::Sense::click_and_drag()),
                            )
                            .on_hover_text("Drag to reorder: slices higher in the list are drawn on top");
                        if r.clicked() {
                            let (shift, ctrl) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
                            let same = ed.screen == si && ed.dmx.is_none();
                            if shift && same && ed.slice.is_some() {
                                // Shift+click: every slice from the selected one to this one.
                                check_also(ed, n_slices);
                                range_select(ed, li, n_slices);
                            } else if (ctrl || shift) && same {
                                // Ctrl+click: into the selection or out of it.
                                check_also(ed, n_slices);
                                toggle_select(ed, li, n_slices);
                            } else {
                                ed.screen = si;
                                ed.slice = Some(li);
                                ed.mask = None;
                                ed.dmx = None;
                            }
                        }
                        if let Some((from, to)) = tree_dnd(ui, &r, TreeDrag { screen: si, mask: false, index: li }) {
                            dropped = Some((si, false, from, to));
                        }
                        r.context_menu(|ui| {
                            for (label, a) in [
                                ("Duplicate", "dup"),
                                ("Bring to Front", "front"),
                                ("Move Up", "up"),
                                ("Move Down", "down"),
                                ("Send to Back", "back"),
                                ("Reset Warp", "reset"),
                                ("Delete", "delete"),
                            ] {
                                if ui.button(label).clicked() {
                                    action = Some((si, Some(li), a));
                                    ui.close();
                                }
                            }
                        });
                    });
                    rows.push((si, li, row.response.rect.with_max_x(list.right())));
                }
                ui.add_space(4.0);
            }
        });
        self.list_box_select(ui, ed, &rows, &bg);
        // Reorders: from the drop, or the context menu's front / up / down / back.
        let order = dropped.or_else(|| {
            let (si, Some(i), a) = action? else { return None };
            let s = self.outputs.setup.screens.get(si)?;
            let (mask, n) = if a.starts_with('m') {
                (true, s.masks.len())
            } else {
                (false, s.slices.len())
            };
            let to = match a.trim_start_matches('m') {
                "front" => n.checked_sub(1)?,
                "up" => (i + 1).min(n.checked_sub(1)?),
                "down" => i.checked_sub(1)?,
                "back" => 0,
                _ => return None,
            };
            Some((si, mask, i, to))
        });
        if let Some((si, mask, from, to)) = order {
            let s = &mut self.outputs.setup.screens[si];
            let at = if mask {
                reorder(&mut s.masks, from, to)
            } else {
                reorder(&mut s.slices, from, to)
            };
            ed.screen = si;
            (ed.slice, ed.mask) = if mask { (None, Some(at)) } else { (Some(at), None) };
            ed.points.clear();
            self.outputs.touch();
            return;
        }
        let Some((si, li, a)) = action else { return };
        let setup = &mut self.outputs.setup;
        match (li, a) {
            (_, "touch") => {}
            (None, "delete") => {
                setup.screens.remove(si);
                ed.screen = 0;
                ed.slice = None;
            }
            (Some(li), "dup") => ed.slice = Some(setup.duplicate_slice(si, li)),
            (Some(li), "reset") => {
                let w = &mut setup.screens[si].slices[li].warp;
                let mode = w.mode;
                *w = Warp::identity(w.cols, w.rows);
                w.mode = mode;
            }
            (Some(li), "delete") => {
                setup.screens[si].slices.remove(li);
                ed.slice = None;
            }
            (Some(mi), "minvert") => {
                let m = &mut setup.screens[si].masks[mi];
                m.invert = !m.invert;
            }
            (Some(mi), "mdelete") => {
                setup.screens[si].masks.remove(mi);
                ed.mask = None;
            }
            _ => {}
        }
        self.outputs.touch();
    }

    // ------------------------------------------------------------ properties

    fn properties(&mut self, ui: &mut egui::Ui, ed: &mut Editor) {
        let comp = self.outputs.comp_size();
        let monitors = self.outputs.monitors.clone();
        let routes = lavmfx_core::sources::router_options(self.comp.layers.len(), self.comp.layergroups.len());
        let screen = &mut self.outputs.setup.screens[ed.screen];
        let changed = match (ed.mask, ed.slice) {
            (Some(mi), _) => {
                let area = (screen.width as f32, screen.height as f32);
                mask_props(ui, &mut screen.masks[mi], area, "Output Mask")
            }
            (None, None) => {
                let r = screen_props(ui, screen, &monitors);
                if r.1 {
                    ed.tab = Tab::Input;
                }
                r.0
            }
            (None, Some(li)) => {
                let size = (screen.width as f32, screen.height as f32);
                slice_props(ui, &mut screen.slices[li], li, ed, comp, size, &routes)
            }
        };
        if changed {
            self.outputs.touch();
        }
    }

    // ------------------------------------------------------------ canvas

    fn canvas(&mut self, ui: &mut egui::Ui, ed: &mut Editor) {
        let pal = theme::pal(ui);
        let comp = self.outputs.comp_size();
        let screen_id = self.outputs.setup.screens[ed.screen].id;
        let (content, tex) = match ed.tab {
            Tab::Input => (comp, Some(self.comp_tex)),
            Tab::Output => {
                let s = &self.outputs.setup.screens[ed.screen];
                ((s.width, s.height), self.outputs.screen_texture(screen_id).map(|t| t.0))
            }
        };
        let (area, resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let p = ui.painter_at(area);
        p.rect_filled(area, 0.0, pal.ground);
        let view = canvas_view(ui, ed, area, &resp, content);
        let k = view.width() / content.0.max(1) as f32;
        let to = |q: Pt| pos2(view.min.x + q[0] * k, view.min.y + q[1] * k);
        let from = |s: Pos2| -> Pt { [(s.x - view.min.x) / k, (s.y - view.min.y) / k] };
        let full_uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        p.rect_filled(view, 0.0, Color32::BLACK);
        if let Some(t) = tex {
            p.image(t, view, full_uv, Color32::WHITE);
        }
        let tab = ed.tab;
        // Output › Show Mouse Pointer: where the pointer is on this canvas.
        if self.outputs.show_pointer
            && let Some(m) = ui
                .input(|i| i.pointer.latest_pos())
                .filter(|m| area.contains(*m) && view.contains(*m))
        {
            self.outputs.pointer = Some(match tab {
                Tab::Input => crate::output::Pointer::Comp(from(m)),
                Tab::Output => crate::output::Pointer::Screen(screen_id, from(m)),
            });
        }
        if tab == Tab::Input
            && let Some((_, g)) = &ed.guide
        {
            let a = (self.outputs.setup.screens[ed.screen].guide_opacity.clamp(0.0, 1.0) * 255.0) as u8;
            p.image(g.id(), view, full_uv, Color32::from_white_alpha(a));
        }
        p.rect_stroke(view, 0.0, Stroke::new(1.0, pal.accent_dim), egui::StrokeKind::Outside);
        let (cw, ch) = (content.0 as f32, content.1 as f32);
        if ed.grid > 0 {
            let st = Stroke::new(1.0, Color32::from_white_alpha(40));
            for i in 1..ed.grid {
                let t = i as f32 / ed.grid as f32;
                p.line_segment([to([cw * t, 0.0]), to([cw * t, ch])], st);
                p.line_segment([to([0.0, ch * t]), to([cw, ch * t])], st);
            }
        }

        let AdvancedOutput { screens, next_id, .. } = &mut self.outputs.setup;
        let screen = &mut screens[ed.screen];
        check_also(ed, screen.slices.len());
        let multi_key = ui.input(|i| i.modifiers.command || i.modifiers.shift);
        let mut fresh_id = || {
            let id = (*next_id).max(1);
            *next_id = id + 1;
            id
        };
        // A polygon slice's quad is the box around its points on this tab
        // (always computed, so it can't go stale).
        let quad_of = |sl: &Slice| {
            let pts = sl
                .polygon
                .as_ref()
                .map(|g| if tab == Tab::Input { &g.input } else { &g.output });
            match pts {
                Some(pts) if pts.len() >= 3 => {
                    let (lo, hi) = bounds_of(pts);
                    lavmfx_core::output::rect(lo[0], lo[1], hi[0] - lo[0], hi[1] - lo[1])
                }
                _ if tab == Tab::Input => sl.input,
                _ => sl.output,
            }
        };
        // Editable points of a slice on this tab (polygon points or quad corners).
        let slice_pts = |sl: &Slice| -> Vec<Pt> {
            match (&sl.polygon, tab) {
                (Some(g), Tab::Input) => g.input.clone(),
                (Some(g), Tab::Output) => g.output.clone(),
                (None, _) => quad_of(sl).to_vec(),
            }
        };
        let slice_outline = |sl: &Slice| -> Vec<Pt> {
            if tab == Tab::Output {
                sl.outline()
            } else {
                slice_pts(sl)
            }
        };

        // Outlines + names.
        for (li, sl) in screen.slices.iter().enumerate() {
            let sel = ed.slice == Some(li) || ed.also.contains(&li);
            let col = if sel {
                pal.accent
            } else {
                Color32::from_white_alpha(110)
            };
            let o = slice_outline(sl);
            p.add(egui::Shape::closed_line(
                o.iter().map(|&q| to(q)).collect(),
                Stroke::new(if sel { 2.0 } else { 1.0 }, col),
            ));
            let c = if tab == Tab::Output && sl.polygon.is_none() {
                sl.map_out(0.5, 0.5)
            } else {
                quad::center(&o)
            };
            let name = if sl.is_key {
                format!("{} (key)", sl.name)
            } else {
                sl.name.clone()
            };
            p.text(
                to(c),
                egui::Align2::CENTER_CENTER,
                name,
                egui::FontId::proportional(15.0),
                col,
            );
        }
        if tab == Tab::Output {
            for (mi, m) in screen.masks.iter().enumerate() {
                let sel = ed.mask == Some(mi);
                let col = if m.enabled { pal.pending } else { pal.text_weak };
                let o = m.outline();
                p.add(egui::Shape::closed_line(
                    o.iter().map(|&q| to(q)).collect(),
                    Stroke::new(if sel { 2.5 } else { 1.2 }, col),
                ));
                let name = if m.invert {
                    format!("{} (inverted)", m.name)
                } else {
                    m.name.clone()
                };
                p.text(
                    to(quad::center(&o)),
                    egui::Align2::CENTER_CENTER,
                    name,
                    egui::FontId::proportional(14.0),
                    col,
                );
            }
        } else if let Some(m) = ed.slice.and_then(|li| screen.slices[li].input_mask.as_ref()) {
            let col = if m.enabled { pal.pending } else { pal.text_weak };
            let o = m.outline();
            p.add(egui::Shape::closed_line(
                o.iter().map(|&q| to(q)).collect(),
                Stroke::new(2.0, col),
            ));
        }

        // What the point handles edit besides the quad.
        let poly_t = match (tab, ed.mask, ed.slice) {
            (Tab::Output, Some(m), _) => Some(PolyT::OutMask(m)),
            (_, None, Some(li)) if screen.slices[li].polygon.is_some() => Some(if tab == Tab::Input {
                PolyT::PolyIn(li)
            } else {
                PolyT::PolyOut(li)
            }),
            (Tab::Input, None, Some(li)) if screen.slices[li].input_mask.is_some() => Some(PolyT::InMask(li)),
            _ => None,
        };
        // Output › Transform on a polygon slice: a box around the shape (its
        // bounding quad, `sl.output`) scales / rotates / moves all its points;
        // the points themselves are edited in Edit Points.
        let poly_box = ed.mode == OutMode::Transform && matches!(poly_t, Some(PolyT::PolyOut(_) | PolyT::PolyIn(_)));
        // The polygon / mask is what arrows and Ctrl+A act on.
        let poly_edit = matches!(poly_t, Some(PolyT::OutMask(_) | PolyT::PolyOut(_) | PolyT::PolyIn(_)));
        // Its points are the handles (no slice box).
        let poly_primary = poly_edit && !poly_box;
        let points_mode = tab == Tab::Output && ed.mode == OutMode::Points && !poly_edit;
        let owner = ed
            .mask
            .map(|m| usize::MAX - m)
            .or(ed.slice)
            .map(|x| (ed.screen, x, tab == Tab::Input));
        if !(points_mode || poly_t.is_some()) || ed.points_of != owner {
            ed.points.clear();
            ed.points_of = owner;
        }
        let pickable =
            |g: &Grab| matches!(g, Grab::Poly(_)) || (points_mode && matches!(g, Grab::Corner(_) | Grab::Point(_)));

        // Handles.
        let mut handles: Vec<(Grab, Pos2)> = Vec::new();
        let mut rings: Vec<Pos2> = Vec::new();
        if let Some(li) = ed.slice.filter(|_| !poly_primary) {
            let sl = &screen.slices[li];
            let q = quad_of(sl);
            if poly_box {
                // The polygon's transform box.
                p.add(egui::Shape::closed_line(
                    q.iter().map(|&c| to(c)).collect(),
                    Stroke::new(1.0, pal.accent_dim),
                ));
            }
            if points_mode {
                let w = &sl.warp;
                for r in 0..w.rows {
                    let v = r as f32 / (w.rows - 1) as f32;
                    let line: Vec<Pos2> = (0..=32).map(|i| to(sl.map_out(i as f32 / 32.0, v))).collect();
                    p.add(egui::Shape::line(line, Stroke::new(1.0, pal.accent_dim)));
                }
                for c in 0..w.cols {
                    let u = c as f32 / (w.cols - 1) as f32;
                    let line: Vec<Pos2> = (0..=32).map(|i| to(sl.map_out(u, i as f32 / 32.0))).collect();
                    p.add(egui::Shape::line(line, Stroke::new(1.0, pal.accent_dim)));
                }
                let h = Homography::square_to_quad(&sl.output);
                for (i, &pt) in w.points.iter().enumerate() {
                    let (c, r) = (i % w.cols, i / w.cols);
                    let corner = (c == 0 || c == w.cols - 1) && (r == 0 || r == w.rows - 1);
                    if !corner {
                        handles.push((Grab::Point(i), to(h.apply(pt))));
                    }
                }
            } else {
                for i in 0..4 {
                    let (a, b) = (q[i], q[(i + 1) % 4]);
                    handles.push((Grab::Edge(i), to([(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5])));
                }
                rings = q.iter().map(|&c| to(c)).collect();
            }
            for (i, &c) in q.iter().enumerate() {
                handles.push((Grab::Corner(i), to(c)));
            }
        }
        if let Some(t) = poly_t.filter(|_| !poly_box) {
            for (i, pt) in get_poly(screen, t).into_iter().enumerate() {
                handles.push((Grab::Poly(i), to(pt)));
            }
        }
        let is_mask = matches!(poly_t, Some(PolyT::OutMask(_) | PolyT::InMask(_)));
        for r in &rings {
            p.circle_stroke(*r, (RING.0 + RING.1) * 0.5, Stroke::new(1.5, pal.accent_dim));
        }
        if ed.pen.is_none() {
            for (g, at) in &handles {
                let s = if matches!(g, Grab::Corner(_) | Grab::Poly(_)) {
                    HANDLE
                } else {
                    HANDLE_SMALL
                };
                let r = Rect::from_center_size(*at, Vec2::splat(s));
                let col = if matches!(g, Grab::Poly(_)) && is_mask {
                    pal.pending
                } else {
                    pal.accent
                };
                let picked = ed.points.contains(g);
                p.rect_filled(r, 2.0, if picked { col } else { pal.ground });
                p.rect_stroke(
                    r,
                    2.0,
                    Stroke::new(2.0, if picked { pal.text_strong } else { col }),
                    egui::StrokeKind::Middle,
                );
            }
        }

        // Snap targets (content px): other slices / masks, canvas border + centre, grid.
        // Hold Ctrl to place freely (Alt is copy / symmetric scale).
        let (alt, no_snap) = ui.input(|i| (i.modifiers.alt, i.modifiers.command));
        let pen = ed.pen.is_some();
        let snap = (ed.snap && !no_snap).then(|| {
            let mut s = Snap {
                verts: vec![[0.0, 0.0], [cw, 0.0], [cw, ch], [0.0, ch], [cw / 2.0, ch / 2.0]],
                segs: Vec::new(),
                xs: vec![0.0, cw / 2.0, cw],
                ys: vec![0.0, ch / 2.0, ch],
                thr: SNAP_PX / k,
            };
            for g in 1..ed.grid {
                s.xs.push(cw * g as f32 / ed.grid as f32);
                s.ys.push(ch * g as f32 / ed.grid as f32);
            }
            let mut add = |pts: Vec<Pt>, outline: Vec<Pt>| {
                s.xs.extend(pts.iter().map(|v| v[0]));
                s.ys.extend(pts.iter().map(|v| v[1]));
                s.verts.extend(pts);
                for i in 0..outline.len() {
                    s.segs.push((outline[i], outline[(i + 1) % outline.len()]));
                }
            };
            for (li, sl) in screen.slices.iter().enumerate() {
                // The slice being edited doesn't snap to itself (its input mask may).
                if Some(li) == ed.slice && !pen && !matches!(poly_t, Some(PolyT::InMask(_))) {
                    continue;
                }
                add(slice_pts(sl), slice_outline(sl));
            }
            if tab == Tab::Output {
                for (mi, m) in screen.masks.iter().enumerate() {
                    if Some(mi) != ed.mask || pen {
                        add(m.points.clone(), m.outline());
                    }
                }
            }
            s
        });

        let mut changed = false;

        // Pen tool: click points, finish on the first point / double-click / Enter.
        if let Some((target, pts)) = &mut ed.pen {
            if matches!(target, PenFor::InputMask(_)) != (tab == Tab::Input) {
                ed.tab = if matches!(target, PenFor::InputMask(_)) {
                    Tab::Input
                } else {
                    Tab::Output
                };
                return;
            }
            let cur = resp.hover_pos().map(|c| {
                let q = from(c);
                snap.as_ref().map_or(q, |s| s.point(q).0)
            });
            let closing = |pts: &[Pt], c: Pt| pts.len() >= 3 && to(pts[0]).distance(to(c)) <= GRAB_R;
            let mut done = false;
            if resp.clicked()
                && let Some(c) = cur
            {
                if closing(pts, c) {
                    done = true;
                } else {
                    pts.push(c);
                }
            }
            let (enter, back, esc) = ui.input(|i| {
                (
                    i.key_pressed(Key::Enter),
                    i.key_pressed(Key::Backspace),
                    i.key_pressed(Key::Escape),
                )
            });
            if resp.double_clicked() || enter {
                done = pts.len() >= 3;
            }
            if back {
                pts.pop();
            }
            // Preview.
            let col = pal.pending;
            let mut line: Vec<Pos2> = pts.iter().map(|&q| to(q)).collect();
            if let Some(c) = cur {
                line.push(to(c));
            }
            p.add(egui::Shape::line(line, Stroke::new(2.0, col)));
            for (i, &q) in pts.iter().enumerate() {
                let r = Rect::from_center_size(to(q), Vec2::splat(if i == 0 { HANDLE } else { HANDLE_SMALL }));
                p.rect_filled(
                    r,
                    2.0,
                    if i == 0 && cur.is_some_and(|c| closing(pts, c)) {
                        col
                    } else {
                        pal.ground
                    },
                );
                p.rect_stroke(r, 2.0, Stroke::new(2.0, col), egui::StrokeKind::Middle);
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            if esc {
                ed.pen = None;
            } else if done {
                let (target, mut pts) = ed.pen.take().unwrap_or((PenFor::OutputMask, Vec::new()));
                // A double-click adds the same point twice.
                pts.dedup_by(|a, b| dist(*a, *b) < 0.5);
                if pts.len() >= 3 {
                    self.finish_pen(ed, target, pts);
                }
            }
            return;
        }

        // Hover: show which handle would be grabbed.
        if let Some(h) = resp.hover_pos()
            && ed.grab.is_none()
            && let Some((_, at)) = handles.iter().find(|(_, at)| at.distance(h) <= GRAB_R)
        {
            p.circle_filled(*at, 4.0, pal.accent);
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }

        // Press: pick control points. Click = only this one, Shift+click =
        // add / remove, click on nothing = none.
        let nearest_handle = |m: Pos2| {
            handles
                .iter()
                .map(|(g, at)| (*g, at.distance(m)))
                .filter(|(_, d)| *d <= GRAB_R)
                // Corners / mask points win over edges / grid points at the same spot.
                .min_by(|a, b| {
                    let w = |g: &Grab| {
                        if matches!(g, Grab::Corner(_) | Grab::Poly(_)) {
                            0.0
                        } else {
                            3.0
                        }
                    };
                    (a.1 + w(&a.0)).total_cmp(&(b.1 + w(&b.0)))
                })
                .map(|(g, _)| g)
        };
        let (pressed, add, press_at) =
            ui.input(|i| (i.pointer.primary_pressed(), i.modifiers.shift, i.pointer.press_origin()));
        if pressed
            && !ed.pan_tool
            && let Some(m) = press_at.filter(|m| area.contains(*m))
        {
            match nearest_handle(m).filter(|g| pickable(g)) {
                Some(h) if add => {
                    if let Some(i) = ed.points.iter().position(|g| *g == h) {
                        ed.points.remove(i);
                    } else {
                        ed.points.push(h);
                    }
                }
                Some(h) if !ed.points.contains(&h) => ed.points = vec![h],
                Some(_) => {}
                None if !add => ed.points.clear(),
                None => {}
            }
        }
        if (points_mode || poly_t.is_some()) && resp.hovered() && !ui.ctx().egui_wants_keyboard_input() {
            if ui.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::A))) {
                ed.points = handles.iter().map(|(g, _)| *g).filter(|g| pickable(g)).collect();
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                ed.points.clear();
            }
        }

        // Hit tests.
        let mask_at = |screen: &Screen, q: Pt| {
            if tab != Tab::Output {
                return None;
            }
            (0..screen.masks.len())
                .rev()
                .find(|&mi| quad::contains(&screen.masks[mi].outline(), q))
        };
        let slice_at = |screen: &Screen, q: Pt| {
            (0..screen.slices.len())
                .rev()
                .find(|&li| quad::contains(&slice_outline(&screen.slices[li]), q))
        };

        // Grab.
        let middle = ui.input(|i| i.pointer.middle_down());
        if resp.drag_started()
            && let Some(m) = press_at.or(resp.interact_pointer_pos())
        {
            ed.snap_mark = None;
            let q = from(m);
            let mut grab = None;
            let mut target = poly_t;
            if ed.pan_tool || middle {
                grab = Some(Grab::Pan);
            } else if let Some(g) = nearest_handle(m) {
                // Shift+click just deselected it: no drag.
                if !(pickable(&g) && !ed.points.contains(&g)) {
                    grab = Some(g);
                }
            } else if !poly_primary && rings.iter().any(|r| (RING.0..=RING.1).contains(&r.distance(m))) {
                grab = Some(Grab::Rotate);
            } else if let Some(t) = poly_t.filter(|t| quad::contains(&poly_outline(screen, *t), q)) {
                target = Some(t);
                grab = Some(Grab::PolyMove);
            } else if let Some(mi) = mask_at(screen, q) {
                ed.mask = Some(mi);
                ed.slice = None;
                target = Some(PolyT::OutMask(mi));
                grab = Some(Grab::PolyMove);
            } else {
                let li = ed
                    .slice
                    .filter(|&li| quad::contains(&slice_outline(&screen.slices[li]), q))
                    .or(slice_at(screen, q));
                if let Some(li) = li {
                    let n = screen.slices.len();
                    if ed.also.contains(&li) {
                        // A slice of the selection: it leads, the group moves with it.
                        ed.also.retain(|&x| x != li);
                        ed.also.extend(ed.slice);
                    } else if ed.slice != Some(li) {
                        if multi_key {
                            ed.also.extend(ed.slice);
                        } else {
                            ed.also.clear();
                        }
                    }
                    ed.slice = Some(li);
                    ed.mask = None;
                    ed.also_key = (ed.screen, ed.slice, n);
                    if screen.slices[li].polygon.is_some() {
                        target = Some(if tab == Tab::Input {
                            PolyT::PolyIn(li)
                        } else {
                            PolyT::PolyOut(li)
                        });
                        grab = Some(Grab::PolyMove);
                    } else {
                        grab = Some(Grab::Move);
                    }
                }
            }
            // Alt+drag on a slice / mask body: drag a copy, the original stays.
            if alt {
                ed.also.clear();
                // Ok(slice) / Err(output mask); input masks aren't copied.
                let what = match (grab, target) {
                    (Some(Grab::PolyMove), Some(PolyT::OutMask(mi))) => Some(Err(mi)),
                    (Some(Grab::PolyMove), Some(PolyT::PolyIn(li) | PolyT::PolyOut(li))) => Some(Ok(li)),
                    (Some(Grab::Move), _) => ed.slice.map(Ok),
                    _ => None,
                };
                match what {
                    Some(Err(mi)) => {
                        let (_, nm) = paste_clip(screen, &AoClip::Mask(screen.masks[mi].clone()), fresh_id(), [0.0; 2]);
                        ed.mask = nm;
                        target = nm.map(PolyT::OutMask);
                        changed = true;
                    }
                    Some(Ok(li)) => {
                        let (ns, _) = paste_clip(
                            screen,
                            &AoClip::Slice(Box::new(screen.slices[li].clone())),
                            fresh_id(),
                            [0.0; 2],
                        );
                        // The copy goes right above the original.
                        if let Some(n) = ns {
                            let c = screen.slices.remove(n);
                            screen.slices.insert(li + 1, c);
                        }
                        ed.slice = Some(li + 1);
                        ed.mask = None;
                        target = target.map(|t| match t {
                            PolyT::PolyIn(_) => PolyT::PolyIn(li + 1),
                            PolyT::PolyOut(_) => PolyT::PolyOut(li + 1),
                            t => t,
                        });
                        changed = true;
                    }
                    None => {}
                }
            }
            // The rest of the selection follows a moved slice.
            let moves_slice = matches!(grab, Some(Grab::Move))
                || matches!(
                    (grab, target),
                    (Some(Grab::PolyMove), Some(PolyT::PolyIn(_) | PolyT::PolyOut(_)))
                );
            ed.grab_also = if moves_slice {
                ed.also.iter().map(|&i| (i, screen.slices[i].clone())).collect()
            } else {
                Vec::new()
            };
            match grab {
                Some(Grab::Pan) => ed.grab = Some((Grab::Pan, [[0.0; 2]; 4], m)),
                Some(g @ (Grab::Poly(_) | Grab::PolyMove)) => {
                    if let Some(t) = target {
                        ed.grab_poly = Some((t, get_poly(screen, t)));
                        ed.grab = Some((g, [[0.0; 2]; 4], m));
                    }
                }
                Some(g) => {
                    if let Some(li) = ed.slice {
                        ed.grab = Some((g, quad_of(&screen.slices[li]), m));
                        ed.grab_slice = Some(screen.slices[li].clone());
                    }
                }
                None => {
                    // Empty canvas: box selection of slices.
                    let base = if multi_key {
                        ed.slice.into_iter().chain(ed.also.iter().copied()).collect()
                    } else {
                        Vec::new()
                    };
                    ed.marquee = Some((m, base));
                }
            }
        }
        if resp.clicked()
            && !ed.pan_tool
            && let Some(m) = resp.interact_pointer_pos()
            && !nearest_handle(m).is_some_and(|g| pickable(&g))
        {
            let q = from(m);
            if multi_key {
                // Ctrl / Shift+click: a slice into the selection or out of it.
                if let Some(li) = slice_at(screen, q) {
                    toggle_select(ed, li, screen.slices.len());
                }
            } else if let Some(mi) = mask_at(screen, q) {
                ed.mask = Some(mi);
                ed.slice = None;
            } else {
                ed.mask = None;
                ed.slice = slice_at(screen, q);
            }
        }

        // Drag.
        if let (Some((g, start, m0)), Some(m)) = (ed.grab, resp.interact_pointer_pos())
            && resp.dragged()
        {
            let d = [(m.x - m0.x) / k, (m.y - m0.y) / k];
            let mut mark = None;
            // How far the leading slice moved (the rest of the selection follows).
            let mut moved: Option<Pt> = None;
            match g {
                Grab::Pan => ed.pan += resp.drag_delta(),
                Grab::Poly(_) | Grab::PolyMove => {
                    if let Some((t, start)) = ed.grab_poly.clone() {
                        let mut pts = start.clone();
                        if let Grab::Poly(i) = g {
                            let want = [start[i][0] + d[0], start[i][1] + d[1]];
                            let (c, mk) = match &snap {
                                Some(s) => s.point(want),
                                None => (want, None),
                            };
                            mark = mk;
                            let dd = [c[0] - start[i][0], c[1] - start[i][1]];
                            for g in &ed.points {
                                if let Grab::Poly(j) = *g
                                    && j < pts.len()
                                {
                                    pts[j] = [start[j][0] + dd[0], start[j][1] + dd[1]];
                                }
                            }
                            pts[i] = c;
                        } else {
                            let (dd, mk) = match &snap {
                                Some(s) => s.delta(&start, d),
                                None => (d, None),
                            };
                            mark = mk;
                            quad::translate(&mut pts, dd);
                            moved = Some(dd);
                        }
                        set_poly(screen, t, pts);
                        changed = true;
                    }
                }
                _ => {
                    if let Some(li) = ed.slice {
                        let sl = &mut screen.slices[li];
                        let mut q = start;
                        match g {
                            Grab::Corner(_) | Grab::Point(_) if points_mode => {
                                let start_sl = ed.grab_slice.clone().unwrap_or_else(|| sl.clone());
                                let a0 = point_pos(&start_sl, g);
                                let want = [a0[0] + d[0], a0[1] + d[1]];
                                let (c, mk) = match &snap {
                                    Some(s) => s.point(want),
                                    None => (want, None),
                                };
                                mark = mk;
                                move_points(sl, &start_sl, &ed.points, [c[0] - a0[0], c[1] - a0[1]]);
                                q = sl.output;
                            }
                            Grab::Move => {
                                let (d, mk) = match &snap {
                                    Some(s) => s.delta(&start, d),
                                    None => (d, None),
                                };
                                mark = mk;
                                quad::translate(&mut q, d);
                                moved = Some(d);
                            }
                            Grab::Corner(i) => {
                                let want = [start[i][0] + d[0], start[i][1] + d[1]];
                                let (c, mk) = match &snap {
                                    Some(s) => s.point(want),
                                    None => (want, None),
                                };
                                mark = mk;
                                let a = if alt { quad::center(&start) } else { start[(i + 2) % 4] };
                                let s0 = start[i];
                                let s = [ratio(c[0] - a[0], s0[0] - a[0]), ratio(c[1] - a[1], s0[1] - a[1])];
                                quad::scale(&mut q, a, s);
                            }
                            Grab::Edge(i) => {
                                // Opposite side's midpoint stays.
                                let mid = |e: usize| {
                                    let (a, b) = (start[e], start[(e + 1) % 4]);
                                    [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
                                };
                                let (me, op) = (mid(i), if alt { quad::center(&start) } else { mid((i + 2) % 4) });
                                let mut want = [me[0] + d[0], me[1] + d[1]];
                                if let Some(s) = &snap {
                                    // Sides snap along their axis only.
                                    if i % 2 == 0 {
                                        if let Some(y) = nearest(&s.ys, want[1], s.thr) {
                                            want[1] = y;
                                            mark = Some(want);
                                        }
                                    } else if let Some(x) = nearest(&s.xs, want[0], s.thr) {
                                        want[0] = x;
                                        mark = Some(want);
                                    }
                                }
                                let s = if i % 2 == 0 {
                                    [1.0, ratio(want[1] - op[1], me[1] - op[1])]
                                } else {
                                    [ratio(want[0] - op[0], me[0] - op[0]), 1.0]
                                };
                                quad::scale(&mut q, op, s);
                            }
                            Grab::Rotate => {
                                let ctr = to(quad::center(&start));
                                let a0 = (m0.y - ctr.y).atan2(m0.x - ctr.x);
                                let a1 = (m.y - ctr.y).atan2(m.x - ctr.x);
                                let mut deg = (a1 - a0).to_degrees();
                                let base = quad::angle(&start);
                                if snap.is_some() {
                                    // Stick to multiples of 15°.
                                    let target = ((base + deg) / 15.0).round() * 15.0;
                                    if (target - (base + deg)).abs() < 3.0 {
                                        deg = target - base;
                                    }
                                }
                                quad::rotate(&mut q, deg);
                            }
                            _ => {}
                        }
                        let box_of = ed
                            .grab_slice
                            .as_ref()
                            .and_then(|g| g.polygon.as_ref())
                            .filter(|_| poly_box);
                        if let Some(g0) = box_of {
                            // Polygon in its box: the points follow the box.
                            let (from, t) = if tab == Tab::Input {
                                (&g0.input, PolyT::PolyIn(li))
                            } else {
                                (&g0.output, PolyT::PolyOut(li))
                            };
                            let pts = from.iter().map(|&p| box_map(&start, &q, p)).collect();
                            set_poly(screen, t, pts);
                        } else if tab == Tab::Input {
                            sl.input = q;
                        } else {
                            sl.output = q;
                        }
                        changed = true;
                    }
                }
            }
            if let Some(dd) = moved {
                for (i, s0) in &ed.grab_also {
                    if let Some(sl) = screen.slices.get_mut(*i) {
                        *sl = shifted(s0, tab, dd);
                    }
                }
            }
            ed.snap_mark = mark;
        }
        // Box selection: every slice the box touches (on top of the base).
        if let (Some((m0, base)), Some(m)) = (&ed.marquee, resp.interact_pointer_pos())
            && resp.dragged()
        {
            let rect = Rect::from_two_pos(*m0, m);
            let mut sel = base.clone();
            for (li, sl) in screen.slices.iter().enumerate() {
                let pts: Vec<Pos2> = slice_outline(sl).into_iter().map(to).collect();
                if Rect::from_points(&pts).intersects(rect) && !sel.contains(&li) {
                    sel.push(li);
                }
            }
            // The lead stays if it's still selected, else the top-most slice.
            let lead = ed.slice.filter(|l| sel.contains(l)).or(sel.iter().max().copied());
            sel.retain(|&l| Some(l) != lead);
            ed.slice = lead;
            ed.also = sel;
            ed.mask = None;
            ed.also_key = (ed.screen, ed.slice, screen.slices.len());
            p.rect_filled(rect, 0.0, pal.accent.gamma_multiply(0.12));
            p.rect_stroke(rect, 0.0, Stroke::new(1.0, pal.accent), egui::StrokeKind::Inside);
        }
        if resp.drag_stopped() {
            ed.marquee = None;
            ed.grab_also.clear();
            ed.grab = None;
            ed.grab_slice = None;
            ed.grab_poly = None;
            ed.snap_mark = None;
        }
        if let Some(mk) = ed.snap_mark {
            p.circle_stroke(to(mk), 9.0, Stroke::new(2.0, pal.pending));
        }

        // Arrow keys nudge (Shift = 10 px): picked points, else the mask /
        // polygon / slice. Delete removes the selected mask or slice.
        if resp.hovered() && !ui.ctx().egui_wants_keyboard_input() {
            let step = if ui.input(|i| i.modifiers.shift) { 10.0 } else { 1.0 };
            let mut d = [0.0, 0.0];
            ui.input(|i| {
                if i.key_pressed(Key::ArrowLeft) {
                    d[0] -= step;
                }
                if i.key_pressed(Key::ArrowRight) {
                    d[0] += step;
                }
                if i.key_pressed(Key::ArrowUp) {
                    d[1] -= step;
                }
                if i.key_pressed(Key::ArrowDown) {
                    d[1] += step;
                }
            });
            let picked_poly: Vec<usize> = ed
                .points
                .iter()
                .filter_map(|g| if let Grab::Poly(i) = g { Some(*i) } else { None })
                .collect();
            if d != [0.0, 0.0] {
                if !ed.also.is_empty() && ed.points.is_empty() {
                    // Several slices: all of them.
                    for li in ed.slice.into_iter().chain(ed.also.iter().copied()) {
                        screen.slices[li] = shifted(&screen.slices[li], tab, d);
                    }
                    changed = true;
                } else if let Some(t) = poly_t.filter(|_| poly_edit || !picked_poly.is_empty()) {
                    let mut pts = get_poly(screen, t);
                    for (i, p) in pts.iter_mut().enumerate() {
                        if picked_poly.is_empty() || picked_poly.contains(&i) {
                            p[0] += d[0];
                            p[1] += d[1];
                        }
                    }
                    set_poly(screen, t, pts);
                    changed = true;
                } else if let Some(li) = ed.slice {
                    let sl = &mut screen.slices[li];
                    if points_mode && !ed.points.is_empty() {
                        let start = sl.clone();
                        move_points(sl, &start, &ed.points, d);
                    } else {
                        quad::translate(
                            if tab == Tab::Input {
                                &mut sl.input
                            } else {
                                &mut sl.output
                            },
                            d,
                        );
                    }
                    changed = true;
                }
            }
            if ed.points.is_empty() && ui.input(|i| i.key_pressed(Key::Delete)) {
                if let Some(mi) = ed.mask {
                    screen.masks.remove(mi);
                    ed.mask = None;
                    changed = true;
                } else if !ed.also.is_empty() {
                    let mut all: Vec<usize> = ed.slice.into_iter().chain(ed.also.drain(..)).collect();
                    all.sort_unstable();
                    for li in all.into_iter().rev() {
                        screen.slices.remove(li);
                    }
                    ed.slice = None;
                    changed = true;
                } else if let Some(li) = ed.slice {
                    screen.slices.remove(li);
                    ed.slice = None;
                    changed = true;
                }
            }
        }
        // Ctrl+C / Ctrl+X / Ctrl+V: the selected slice or mask (also as text on
        // the system clipboard, so it pastes into another session).
        if !ui.ctx().egui_wants_keyboard_input() && ed.pen.is_none() {
            let evs: Vec<egui::Event> = ui.input(|i| {
                i.events
                    .iter()
                    .filter(|e| matches!(e, egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_)))
                    .cloned()
                    .collect()
            });
            for e in evs {
                match e {
                    egui::Event::Copy | egui::Event::Cut => {
                        let clip = match (ed.mask, ed.slice) {
                            (Some(mi), _) if mi < screen.masks.len() => Some(AoClip::Mask(screen.masks[mi].clone())),
                            (None, Some(li)) if li < screen.slices.len() => {
                                Some(AoClip::Slice(Box::new(screen.slices[li].clone())))
                            }
                            _ => None,
                        };
                        if let Some(c) = clip {
                            ui.ctx().copy_text(c.to_text());
                            ed.clipboard = Some(c);
                            if matches!(e, egui::Event::Cut) {
                                if let Some(mi) = ed.mask.take() {
                                    screen.masks.remove(mi);
                                } else if let Some(li) = ed.slice.take() {
                                    screen.slices.remove(li);
                                }
                                ed.points.clear();
                                changed = true;
                            }
                        }
                    }
                    egui::Event::Paste(text) => {
                        // Our own text, else the last copy made here.
                        if let Some(c) = AoClip::from_text(&text).or_else(|| ed.clipboard.clone()) {
                            let (ns, nm) = paste_clip(screen, &c, fresh_id(), [PASTE_OFFSET; 2]);
                            (ed.slice, ed.mask) = (ns, nm);
                            ed.points.clear();
                            // The next paste lands a step further.
                            if let Some(c) = ns
                                .map(|i| AoClip::Slice(Box::new(screen.slices[i].clone())))
                                .or(nm.map(|i| AoClip::Mask(screen.masks[i].clone())))
                            {
                                ed.clipboard = Some(c);
                            }
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        if changed {
            self.outputs.touch();
        }
    }

    /// The pen tool finished a shape.
    fn finish_pen(&mut self, ed: &mut Editor, target: PenFor, pts: Vec<Pt>) {
        let comp = self.outputs.comp_size();
        let setup = &mut self.outputs.setup;
        match target {
            PenFor::OutputMask => {
                let id = setup.new_id();
                let s = &mut setup.screens[ed.screen];
                s.masks.push(Mask {
                    id,
                    name: format!("Mask {}", s.masks.len() + 1),
                    shape: MaskShape::Polygon,
                    points: pts,
                    ..Default::default()
                });
                ed.mask = Some(s.masks.len() - 1);
                ed.slice = None;
            }
            PenFor::Polygon => {
                ed.slice = Some(setup.add_polygon(ed.screen, comp, Some(pts)));
                ed.mask = None;
            }
            PenFor::InputMask(li) => {
                if let Some(sl) = setup.screens[ed.screen].slices.get_mut(li) {
                    sl.input_mask = Some(Mask {
                        name: "Input Mask".into(),
                        shape: MaskShape::Polygon,
                        points: pts,
                        ..Default::default()
                    });
                }
            }
        }
        self.outputs.touch();
    }
}

/// Points of a mask / polygon (content px).
/// The content's rectangle on the canvas: fitted into `area`, then the
/// editor's zoom (Alt / Ctrl+wheel about the pointer) and pan.
fn canvas_view(ui: &egui::Ui, ed: &mut Editor, area: Rect, resp: &egui::Response, content: (u32, u32)) -> Rect {
    let aspect = content.0 as f32 / content.1.max(1) as f32;
    let fit = letterbox(area.shrink(28.0), aspect);
    // Alt+wheel (or Ctrl+wheel) zooms about the pointer.
    if resp.hovered()
        && let Some(cur) = resp.hover_pos()
    {
        let factor = ui.input(|i| {
            if i.modifiers.alt {
                // Raw wheel events (some systems turn Alt+wheel sideways).
                let mut w = 0.0;
                for e in &i.raw.events {
                    if let egui::Event::MouseWheel { unit, delta, .. } = e {
                        let d = if delta.y != 0.0 { delta.y } else { delta.x };
                        w += match unit {
                            egui::MouseWheelUnit::Point => d,
                            egui::MouseWheelUnit::Line => d * 40.0,
                            egui::MouseWheelUnit::Page => d * 400.0,
                        };
                    }
                }
                (1.0 + w * 0.002).clamp(0.5, 2.0)
            } else {
                i.zoom_delta()
            }
        });
        if factor != 1.0 {
            let old = Rect::from_center_size(fit.center() + ed.pan, fit.size() * ed.zoom);
            let rel = (cur - old.min) / old.size(); // pointer in 0..1 of the canvas
            ed.zoom = (ed.zoom * factor).clamp(0.1, 8.0);
            let size = fit.size() * ed.zoom;
            let min = cur - rel * size;
            ed.pan = (min + size * 0.5) - fit.center();
        }
    }
    Rect::from_center_size(fit.center() + ed.pan, fit.size() * ed.zoom)
}

fn get_poly(screen: &Screen, t: PolyT) -> Vec<Pt> {
    match t {
        PolyT::OutMask(m) => screen.masks.get(m).map(|m| m.points.clone()),
        PolyT::InMask(li) => screen
            .slices
            .get(li)
            .and_then(|s| s.input_mask.as_ref())
            .map(|m| m.points.clone()),
        PolyT::PolyOut(li) => screen
            .slices
            .get(li)
            .and_then(|s| s.polygon.as_ref())
            .map(|g| g.output.clone()),
        PolyT::PolyIn(li) => screen
            .slices
            .get(li)
            .and_then(|s| s.polygon.as_ref())
            .map(|g| g.input.clone()),
    }
    .unwrap_or_default()
}

/// Outline for hit tests (ellipses as their curve).
fn poly_outline(screen: &Screen, t: PolyT) -> Vec<Pt> {
    match t {
        PolyT::OutMask(m) => screen.masks.get(m).map(|m| m.outline()).unwrap_or_default(),
        PolyT::InMask(li) => screen
            .slices
            .get(li)
            .and_then(|s| s.input_mask.as_ref())
            .map(|m| m.outline())
            .unwrap_or_default(),
        _ => get_poly(screen, t),
    }
}

fn set_poly(screen: &mut Screen, t: PolyT, pts: Vec<Pt>) {
    match t {
        PolyT::OutMask(m) => {
            if let Some(m) = screen.masks.get_mut(m) {
                m.points = pts;
            }
        }
        PolyT::InMask(li) => {
            if let Some(m) = screen.slices.get_mut(li).and_then(|s| s.input_mask.as_mut()) {
                m.points = pts;
            }
        }
        PolyT::PolyOut(li) | PolyT::PolyIn(li) => {
            if let Some(sl) = screen.slices.get_mut(li) {
                let (lo, hi) = bounds_of(&pts);
                let r = lavmfx_core::output::rect(lo[0], lo[1], hi[0] - lo[0], hi[1] - lo[1]);
                if let Some(g) = sl.polygon.as_mut() {
                    if matches!(t, PolyT::PolyOut(_)) {
                        g.output = pts;
                        sl.output = r;
                    } else {
                        g.input = pts;
                        sl.input = r;
                    }
                }
            }
        }
    }
}

/// Where `p` goes when quad `from` becomes `to` (affine, from the TL, TR and
/// BL corners): a polygon slice's points following its Transform box.
fn box_map(from: &[Pt; 4], to: &[Pt; 4], p: Pt) -> Pt {
    let (o, u, v) = (
        from[0],
        [from[1][0] - from[0][0], from[1][1] - from[0][1]],
        [from[3][0] - from[0][0], from[3][1] - from[0][1]],
    );
    let det = u[0] * v[1] - u[1] * v[0];
    if det.abs() < 1e-9 {
        return p;
    }
    let (dx, dy) = (p[0] - o[0], p[1] - o[1]);
    let a = (dx * v[1] - dy * v[0]) / det;
    let b = (u[0] * dy - u[1] * dx) / det;
    let (o2, u2, v2) = (
        to[0],
        [to[1][0] - to[0][0], to[1][1] - to[0][1]],
        [to[3][0] - to[0][0], to[3][1] - to[0][1]],
    );
    [o2[0] + a * u2[0] + b * v2[0], o2[1] + a * u2[1] + b * v2[1]]
}

/// Screen position of a control point (`Corner` / `Point`).
fn point_pos(sl: &Slice, g: Grab) -> Pt {
    match g {
        Grab::Corner(i) => sl.output[i],
        Grab::Point(i) => Homography::square_to_quad(&sl.output).apply(sl.warp.points[i]),
        _ => [0.0, 0.0],
    }
}

/// Move the picked control points by `d` screen pixels, from `start`:
/// corners move the corner pin, grid points keep their screen offset under
/// the new pin; unpicked grid points follow the pin as usual.
fn move_points(sl: &mut Slice, start: &Slice, picked: &[Grab], d: Pt) {
    let mut out = start.output;
    for g in picked {
        if let Grab::Corner(i) = *g {
            out[i] = [start.output[i][0] + d[0], start.output[i][1] + d[1]];
        }
    }
    sl.output = out;
    let h0 = Homography::square_to_quad(&start.output);
    let Some(inv) = Homography::square_to_quad(&out).inverse() else {
        return;
    };
    for g in picked {
        if let Grab::Point(i) = *g
            && i < sl.warp.points.len()
        {
            let p = h0.apply(start.warp.points[i]);
            sl.warp.points[i] = inv.apply([p[0] + d[0], p[1] + d[1]]);
        }
    }
}

fn ratio(now: f32, was: f32) -> f32 {
    if was.abs() < 1e-3 {
        1.0
    } else {
        (now / was).clamp(-100.0, 100.0)
    }
}

fn name_row(ui: &mut egui::Ui, name: &mut String) -> bool {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY))
            .changed()
    })
    .inner
}

fn color_rows(ui: &mut egui::Ui, c: &mut lavmfx_core::output::ColorAdjust, opacity: bool) -> bool {
    let mut ch = false;
    if opacity {
        let mut pct = c.opacity * 100.0;
        if value_row(ui, "Opacity", &mut pct, 0.0..=100.0, 1.0, 100.0, true) {
            c.opacity = pct / 100.0;
            ch = true;
        }
    }
    ch |= value_row(ui, "Brightness", &mut c.brightness, -1.0..=1.0, 0.01, 0.0, true);
    ch |= value_row(ui, "Contrast", &mut c.contrast, -1.0..=1.0, 0.01, 0.0, true);
    ch |= value_row(ui, "Red", &mut c.red, -1.0..=1.0, 0.01, 0.0, true);
    ch |= value_row(ui, "Green", &mut c.green, -1.0..=1.0, 0.01, 0.0, true);
    ch |= value_row(ui, "Blue", &mut c.blue, -1.0..=1.0, 0.01, 0.0, true);
    ch
}

/// Returns (changed, a guide was loaded → show the Input Selection tab).
fn screen_props(ui: &mut egui::Ui, s: &mut Screen, monitors: &[crate::output::Monitor]) -> (bool, bool) {
    let mut ch = false;
    ui.label(RichText::new("Screen").strong());
    ch |= name_row(ui, &mut s.name);
    let label = |d: &Device| match d {
        Device::Virtual => "Virtual Output".to_string(),
        Device::Window => "Window".to_string(),
        Device::Display { index, .. } => monitors
            .get(*index)
            .map(|m| format!("Display {} ({}×{})", index + 1, m.size.0, m.size.1))
            .unwrap_or_else(|| format!("Display {}", index + 1)),
    };
    ui.horizontal(|ui| {
        ui.label("Device");
        egui::ComboBox::from_id_salt("ao-device")
            .selected_text(label(&s.device))
            .width(200.0)
            .show_ui(ui, |ui| {
                let mut opts = vec![Device::Virtual, Device::Window];
                let n = monitors.len().max(1);
                for i in 0..n {
                    opts.push(Device::Display {
                        index: i,
                        name: monitors.get(i).map(|m| m.name.clone()).unwrap_or_default(),
                    });
                }
                for d in opts {
                    let on = match (&d, &s.device) {
                        (Device::Display { index: a, .. }, Device::Display { index: b, .. }) => a == b,
                        (a, b) => std::mem::discriminant(a) == std::mem::discriminant(b),
                    };
                    if ui.selectable_label(on, label(&d)).clicked() {
                        // A display: take its resolution (slices scale along).
                        if let Device::Display { index, .. } = &d
                            && let Some(m) = monitors.get(*index)
                        {
                            s.resize(m.size.0, m.size.1);
                        }
                        s.device = d;
                        ch = true;
                    }
                }
            });
    });
    let (mut w, mut h) = (s.width as f32, s.height as f32);
    let cw = value_row(ui, "Width", &mut w, 16.0..=16384.0, 1.0, 1920.0, false);
    let chh = value_row(ui, "Height", &mut h, 16.0..=16384.0, 1.0, 1080.0, false);
    if cw || chh {
        s.resize(w.round() as u32, h.round() as u32);
        ch = true;
    }
    ch |= color_rows(ui, &mut s.color, true);
    let mut loaded = false;
    section(ui, "Input Guide", |ui| {
        ui.horizontal(|ui| {
            if ui
                .button("Load…")
                .on_hover_text("An image to line slices up with (shown on Input Selection only)")
                .clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .add_filter("Image", &["png", "jpg", "jpeg", "bmp", "tif", "tiff", "webp"])
                    .pick_file()
            {
                s.input_guide = Some(p.to_string_lossy().into());
                ch = true;
                loaded = true;
            }
            if s.input_guide.is_some() && ui.button("Clear").clicked() {
                s.input_guide = None;
                ch = true;
            }
            let name = s
                .input_guide
                .as_deref()
                .and_then(|p| std::path::Path::new(p).file_name())
                .map(|n| n.to_string_lossy().to_string());
            ui.label(RichText::new(name.unwrap_or_else(|| "Drop an image file on this window".into())).weak());
        });
        if s.input_guide.is_some() {
            let mut pct = s.guide_opacity * 100.0;
            if value_row(ui, "Opacity", &mut pct, 0.0..=100.0, 1.0, 50.0, true) {
                s.guide_opacity = pct / 100.0;
                ch = true;
            }
        }
    });
    (ch, loaded)
}

fn mask_props(ui: &mut egui::Ui, m: &mut Mask, area: (f32, f32), title: &str) -> bool {
    let mut ch = false;
    ui.label(RichText::new(title).strong());
    ch |= name_row(ui, &mut m.name);
    let shape = match m.shape {
        MaskShape::Rectangle => "Rectangle",
        MaskShape::Ellipse => "Ellipse",
        MaskShape::Polygon => "Polygon",
    };
    ui.horizontal(|ui| {
        ui.label("Shape");
        ui.label(RichText::new(format!("{shape} · {} points", m.points.len())).weak());
    });
    ch |= super::widgets::bool_row(ui, "Enabled", &mut m.enabled, "Mask on / off");
    ch |= super::widgets::bool_row(ui, "Invert", &mut m.invert, "Use the outside of the shape instead");
    ch |= quad_rows(ui, &mut m.points, area);
    ch
}

fn quad_rows(ui: &mut egui::Ui, q: &mut [Pt], limit: (f32, f32)) -> bool {
    let mut ch = false;
    let c = quad::center(q);
    let sz = quad::size(q);
    let (mut x, mut y, mut w, mut h) = (c[0], c[1], sz[0], sz[1]);
    let big = limit.0.max(limit.1) * 4.0;
    let a = value_row(ui, "X", &mut x, -big..=big, 1.0, limit.0 / 2.0, false);
    let b = value_row(ui, "Y", &mut y, -big..=big, 1.0, limit.1 / 2.0, false);
    let (mut left, mut top) = (x - w / 2.0, y - h / 2.0);
    let l = value_row(ui, "Left", &mut left, -big..=big, 1.0, 0.0, false);
    let t = value_row(ui, "Top", &mut top, -big..=big, 1.0, 0.0, false);
    let cw = value_row(ui, "Width", &mut w, 1.0..=big, 1.0, limit.0, false);
    let chh = value_row(ui, "Height", &mut h, 1.0..=big, 1.0, limit.1, false);
    if l || t {
        x = left + w / 2.0;
        y = top + h / 2.0;
    }
    if a || b || l || t || cw || chh {
        quad::set_box(q, [x, y], [w, h]);
        ch = true;
    }
    let mut rot = quad::angle(q);
    let before = rot;
    if value_row(ui, "Rotation", &mut rot, -180.0..=180.0, 1.0, 0.0, false) {
        quad::rotate(q, rot - before);
        ch = true;
    }
    ch
}

fn soft_edge_rows(ui: &mut egui::Ui, e: &mut lavmfx_core::output::SoftEdge) -> bool {
    let mut ch = false;
    section(ui, "Soft Edge", |ui| {
        ch |= super::widgets::bool_row(
            ui,
            "Enabled",
            &mut e.enabled,
            "Fade the slice edges (projector overlap blending)",
        );
        let pct = |ui: &mut egui::Ui, label: &str, v: &mut f32| {
            let mut x = *v * 100.0;
            if value_row(ui, label, &mut x, 0.0..=50.0, 1.0, 0.0, true) {
                *v = x / 100.0;
                true
            } else {
                false
            }
        };
        ch |= pct(ui, "Left %", &mut e.left);
        ch |= pct(ui, "Right %", &mut e.right);
        ch |= pct(ui, "Top %", &mut e.top);
        ch |= pct(ui, "Bottom %", &mut e.bottom);
        ch |= value_row(ui, "Luminance", &mut e.luminance, 0.0..=1.0, 0.01, 0.5, true);
        ch |= value_row(ui, "Power", &mut e.power, 0.1..=8.0, 0.1, 2.0, true);
        ch |= value_row(ui, "Gamma", &mut e.gamma, 0.2..=4.0, 0.05, 1.0, true);
    });
    ch
}

/// Input Mask section: shape buttons, then the mask's rows.
fn input_mask_rows(ui: &mut egui::Ui, sl: &mut Slice, li: usize, ed: &mut Editor) -> bool {
    let mut ch = false;
    section(ui, "Input Mask", |ui| {
        let sz = vec2(30.0, 24.0);
        ui.horizontal(|ui| {
            let (lo, hi) = bounds_of(&sl.input);
            for (icon, kind, tip) in [
                (Icon::ShapeRect, MaskKind::Rectangle, "Rectangle"),
                (Icon::ShapeEllipse, MaskKind::Ellipse, "Ellipse"),
                (Icon::ShapeTriangle, MaskKind::Triangle, "Triangle"),
                (Icon::ShapeHexagon, MaskKind::Hexagon, "Hexagon"),
            ] {
                if icon_button(ui, icon, false, sz, true)
                    .on_hover_text(format!("{tip} input mask (replaces the current one)"))
                    .clicked()
                {
                    let mut m = Mask::new(0, "Input Mask", kind, (hi[0] - lo[0], hi[1] - lo[1]));
                    quad::translate(&mut m.points, lo);
                    sl.input_mask = Some(m);
                    ch = true;
                }
            }
            if icon_button(ui, Icon::Pen, ed.pen.is_some(), sz, true)
                .on_hover_text("Draw the input mask: click its points, click the first point or press Enter to finish")
                .clicked()
            {
                ed.pen = Some((PenFor::InputMask(li), Vec::new()));
            }
        });
        if let Some(m) = &mut sl.input_mask {
            ch |= super::widgets::bool_row(ui, "Enabled", &mut m.enabled, "Input mask on / off");
            ch |= super::widgets::bool_row(ui, "Invert", &mut m.invert, "Show everything except the shape");
            if ui.button("Remove Mask").clicked() {
                sl.input_mask = None;
                ch = true;
            }
        } else {
            ui.label(RichText::new("No mask: the whole input selection shows.").weak());
        }
    });
    ch
}

/// Help panel text for where the pointer is (Help box).
fn help_text(ed: &Editor) -> String {
    if ed.pen.is_some() {
        return "Pen: click to add points (snaps when the magnet is on). Click the first point, double-click or press Enter to finish; Backspace removes the last point; Esc cancels.".into();
    }
    if ed.dmx.is_some() && ed.help != HelpArea::None && ed.help != HelpArea::Toolbar {
        return dmx_ui::help(ed);
    }
    match ed.help {
        HelpArea::Tree => "Screens with their output masks and slices (top first). Checkbox = on/off; right-click for Duplicate, Move Up/Down, Invert, Delete. + adds screens, slices, polygons and output masks; Presets saves and loads whole setups.".into(),
        HelpArea::Toolbar => "Zoom (Alt+wheel on the canvas), Undo / Redo (Ctrl+Z / Ctrl+Shift+Z), Pan (middle-drag always pans), Snap (magnet; hold Ctrl to drag freely), Grid. Output Transformation: Edit Points = corner pin + warp grid, Transform = move / scale / rotate.".into(),
        HelpArea::Properties => "Settings of the selected screen, slice or mask. Double-click a name to reset its value; Shift on − / + = fine steps. Screens: pick a display as Device to output there; Input Guide = a reference image for lining up slices.".into(),
        HelpArea::Canvas => match (ed.tab, ed.mode) {
            (Tab::Input, _) => "Input Selection: the part of the composition each slice shows. Drag inside to move (Alt+drag: a copy), squares to scale (Alt: about the centre). Ctrl / Shift+click adds slices to the selection (also in the list): dragging one of them, the arrows and Delete act on all. A polygon slice (triangle, hexagon, circle, pen) transforms in its box, its points are in + Edit Points; the orange shape is the slice's input mask (drag it or its points). Arrows nudge (Shift = 10 px). Ctrl+C / X / V copy, cut, paste the selected slice or mask.".into(),
            (Tab::Output, OutMode::Points) => "Edit Points: drag corners (corner pin) and warp grid points. Click picks a point, Shift+click picks several, Ctrl+A all, Esc none; dragging moves all picked points. Arrows nudge.".into(),
            (Tab::Output, OutMode::Transform) => "Transform: drag inside to move (Alt+drag: a copy), squares to scale (Alt: about the centre), the rings to rotate. Ctrl / Shift+click adds slices to the selection (also in the list): dragging one of them, the arrows and Delete act on all; hold Ctrl to skip snapping. Polygon slices (triangle, hexagon, circle, pen) transform in their box; their points are in Edit Points. Orange shapes are output masks: click to select, drag to move, drag points to reshape. Ctrl+C / X / V copy, cut, paste the selected slice or mask.".into(),
        },
        HelpArea::None => "Move your mouse over the interface element that you would like more info about.".into(),
    }
}

/// Rows per tab / mode:
/// Input = position + Soft Edge; Output Transform = position, flip, key, BG,
/// colour, Soft Edge; Output Edit Points = flip, key, BG, colour, Soft Edge,
/// Warping.
fn slice_props(
    ui: &mut egui::Ui,
    sl: &mut Slice,
    li: usize,
    ed: &mut Editor,
    comp: (u32, u32),
    screen: (f32, f32),
    routes: &[String],
) -> bool {
    let mut ch = false;
    ui.label(RichText::new("Slice").strong());
    ch |= name_row(ui, &mut sl.name);
    ui.horizontal(|ui| {
        use lavmfx_core::sources::RouteTarget;
        ui.label("Input Source");
        egui::ComboBox::from_id_salt(("ao-input-source", sl.id))
            .selected_text(sl.input_source.label())
            .width(160.0)
            .show_ui(ui, |ui| {
                for o in routes {
                    let t = RouteTarget::parse(o);
                    if ui.selectable_label(sl.input_source == t, o).clicked() {
                        sl.input_source = t;
                        ch = true;
                    }
                }
            })
            .response
            .on_hover_text("What this slice shows: the whole composition, or one layer / group (after its effects)");
    });
    if let Some(poly) = &mut sl.polygon {
        ui.label(
            RichText::new(format!(
                "Polygon slice · {} points. Drag its points on {}; Shift+click picks several.",
                poly.input.len(),
                if ed.tab == Tab::Input {
                    "the composition"
                } else {
                    "the screen"
                }
            ))
            .weak(),
        );
        if ed.tab == Tab::Input {
            ch |= quad_rows(ui, &mut poly.input, (comp.0 as f32, comp.1 as f32));
            ch |= input_mask_rows(ui, sl, li, ed);
            return ch;
        }
        ch |= quad_rows(ui, &mut poly.output, screen);
        ch |= super::widgets::bool_row(
            ui,
            "Is Key",
            &mut sl.is_key,
            "Use this slice's brightness as the alpha of the slice above (not shown itself)",
        );
        ch |= super::widgets::bool_row(
            ui,
            "Black BG",
            &mut sl.black_bg,
            "Transparent parts of the composition show black instead of the slices below",
        );
        ch |= color_rows(ui, &mut sl.color, false);
        return ch;
    }
    if ed.tab == Tab::Input {
        ch |= quad_rows(ui, &mut sl.input, (comp.0 as f32, comp.1 as f32));
        if ui.button("Full composition").clicked() {
            sl.input = lavmfx_core::output::rect(0.0, 0.0, comp.0 as f32, comp.1 as f32);
            ch = true;
        }
        ch |= soft_edge_rows(ui, &mut sl.soft_edge);
        ch |= input_mask_rows(ui, sl, li, ed);
        return ch;
    }
    if ed.mode == OutMode::Transform {
        ch |= quad_rows(ui, &mut sl.output, screen);
    }
    if ui
        .button("Fill screen")
        .on_hover_text("Output = whole screen, corner pin reset")
        .clicked()
    {
        sl.output = lavmfx_core::output::rect(0.0, 0.0, screen.0, screen.1);
        ch = true;
    }
    ui.horizontal(|ui| {
        ui.label("Flip");
        ch |= ui
            .toggle_value(&mut sl.flip.h, "H")
            .on_hover_text("Flip horizontally")
            .changed();
        ch |= ui
            .toggle_value(&mut sl.flip.v, "V")
            .on_hover_text("Flip vertically")
            .changed();
    });
    ch |= super::widgets::bool_row(
        ui,
        "Is Key",
        &mut sl.is_key,
        "Use this slice's brightness as the alpha of the slice above (not shown itself)",
    );
    ch |= super::widgets::bool_row(
        ui,
        "Black BG",
        &mut sl.black_bg,
        "Transparent parts of the composition show black instead of the slices below",
    );
    ch |= color_rows(ui, &mut sl.color, false);
    ch |= soft_edge_rows(ui, &mut sl.soft_edge);
    if ed.mode == OutMode::Points {
        section(ui, "Warping", |ui| {
            let w = &mut sl.warp;
            ui.horizontal(|ui| {
                ui.label("Point Mode");
                ch |= ui.selectable_value(&mut w.mode, PointMode::Linear, "Linear").changed();
                ch |= ui.selectable_value(&mut w.mode, PointMode::Bezier, "Bezier").changed();
            });
            let (sx, sy) = w.subdivisions();
            let (mut fx, mut fy) = (sx as f32, sy as f32);
            let a = value_row(ui, "Subdiv. X", &mut fx, 0.0..=30.0, 1.0, 0.0, false);
            let b = value_row(ui, "Subdiv. Y", &mut fy, 0.0..=30.0, 1.0, 0.0, false);
            if a || b {
                w.set_subdivisions(fx.round() as usize, fy.round() as usize);
                ch = true;
            }
            if ui.button("Reset Warp").clicked() {
                let mode = w.mode;
                *w = Warp::identity(w.cols, w.rows);
                w.mode = mode;
                ch = true;
            }
        });
    }
    ch
}

impl LavmfxApp {
    /// Slices tab: screens with their output masks and slices (top
    /// first), a search field, on/off boxes; double-click opens the
    /// Advanced Output editor on that item.
    pub(super) fn slices_tab(&mut self, ui: &mut egui::Ui) {
        let id = egui::Id::unique("slices-tab-search");
        let mut q: String = ui.data(|d| d.get_temp(id)).unwrap_or_default();
        ui.add(
            egui::TextEdit::singleline(&mut q)
                .hint_text("Search")
                .desired_width(f32::INFINITY),
        );
        ui.data_mut(|d| d.insert_temp(id, q.clone()));
        let q = q.to_lowercase();
        let pal = theme::pal(ui);
        let mut open: Option<(usize, Option<usize>, Option<usize>)> = None;
        let mut changed = false;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (si, s) in self.outputs.setup.screens.iter_mut().enumerate() {
                let r = egui::CollapsingHeader::new(&s.name)
                    .id_salt(("slices-tab", s.id))
                    .default_open(true)
                    .show(ui, |ui| {
                        for mi in (0..s.masks.len()).rev() {
                            let m = &mut s.masks[mi];
                            if !q.is_empty() && !m.name.to_lowercase().contains(&q) {
                                continue;
                            }
                            ui.horizontal(|ui| {
                                changed |= ui.checkbox(&mut m.enabled, "").changed();
                                let r = ui.add(
                                    egui::Label::new(RichText::new(&m.name).color(pal.pending))
                                        .sense(egui::Sense::click()),
                                );
                                if r.on_hover_text("Output mask · double-click: edit").double_clicked() {
                                    open = Some((si, None, Some(mi)));
                                }
                            });
                        }
                        for li in (0..s.slices.len()).rev() {
                            let sl = &mut s.slices[li];
                            if !q.is_empty() && !sl.name.to_lowercase().contains(&q) {
                                continue;
                            }
                            ui.horizontal(|ui| {
                                changed |= ui.checkbox(&mut sl.enabled, "").changed();
                                let r = ui.add(egui::Label::new(&sl.name).sense(egui::Sense::click()));
                                if r.on_hover_text("Double-click: edit in Advanced Output")
                                    .double_clicked()
                                {
                                    open = Some((si, Some(li), None));
                                }
                            });
                        }
                    });
                if r.header_response.double_clicked() {
                    open = Some((si, None, None));
                }
            }
            if self.outputs.setup.screens.is_empty() {
                ui.label(RichText::new("No screens: Output › Advanced… sets them up.").weak());
            }
        });
        if changed {
            self.outputs.touch();
        }
        if let Some((si, li, mi)) = open {
            self.open_screens_editor();
            if let Some(ed) = &mut self.screens_editor {
                ed.screen = si;
                ed.slice = li;
                ed.mask = mi;
                if mi.is_some() {
                    ed.tab = Tab::Output;
                }
            }
        }
    }
}

impl LavmfxApp {
    /// `LAVMFX_SCREENSHOT_AO`: a demo setup for screenshots.
    pub(super) fn screens_debug(&mut self, tab: &str) {
        let comp = self.outputs.comp_size();
        let setup = &mut self.outputs.setup;
        let s = 0;
        if setup.screens[s].slices.len() < 2 {
            let li = setup.add_slice(s, comp);
            let (w, h) = (setup.screens[s].width as f32, setup.screens[s].height as f32);
            let sl = &mut setup.screens[s].slices[li];
            sl.input = lavmfx_core::output::rect(comp.0 as f32 * 0.5, 0.0, comp.0 as f32 * 0.5, comp.1 as f32 * 0.5);
            sl.output = [
                [w * 0.55, h * 0.1],
                [w * 0.95, h * 0.2],
                [w * 0.9, h * 0.8],
                [w * 0.6, h * 0.7],
            ];
            sl.warp.set_subdivisions(1, 1);
            sl.warp.mode = PointMode::Bezier;
            sl.warp.points[4] = [0.6, 0.4];
            sl.soft_edge.enabled = true;
            sl.soft_edge.left = 0.2;
            setup.screens[s].slices[0].output = lavmfx_core::output::rect(0.0, 0.0, w * 0.5, h);
        }
        if tab.starts_with("masks") || tab == "inmask" {
            // Output mask, input mask on slice 2, a polygon slice.
            let m = setup.add_mask(0, MaskKind::Ellipse);
            let (w, h) = (setup.screens[0].width as f32, setup.screens[0].height as f32);
            setup.screens[0].masks[m].points =
                lavmfx_core::output::rect(w * 0.05, h * 0.55, w * 0.25, h * 0.35).to_vec();
            let mut im = Mask::new(
                0,
                "Input Mask",
                MaskKind::Hexagon,
                (comp.0 as f32 * 0.5, comp.1 as f32 * 0.5),
            );
            quad::translate(&mut im.points, [comp.0 as f32 * 0.5, 0.0]);
            setup.screens[0].slices[1].input_mask = Some(im);
            let poly = vec![[w * 0.3, h * 0.05], [w * 0.5, h * 0.4], [w * 0.1, h * 0.4]];
            setup.add_polygon(0, comp, Some(poly));
        }
        if tab.starts_with("shapes") {
            // Ready-made polygon slices: hexagon, circle, triangle (selected), moved apart.
            let w = setup.screens[0].width as f32;
            for (shape, dx) in [
                (PolygonShape::Hexagon, -0.22),
                (PolygonShape::Circle, 0.22),
                (PolygonShape::Triangle, 0.0),
            ] {
                let i = setup.add_polygon_shape(0, comp, shape);
                if let Some(p) = setup.screens[0].slices[i].polygon.as_mut() {
                    quad::translate(&mut p.output, [w * dx, 0.0]);
                }
            }
        }
        if tab.starts_with("dmx") {
            // A lumiverse: a 170-pixel strip across the top, an 8×8 snake
            // matrix in the middle, an RGBW par overlapping the matrix.
            let lib = lavmfx_core::dmx::Personality::builtin();
            let by = |n: &str| lib.iter().find(|p| p.name.starts_with(n)).cloned().unwrap_or_default();
            let li = setup.add_lumiverse();
            let (w, h) = (comp.0 as f32, comp.1 as f32);
            let a = setup.add_fixture(li, comp, by("LED 170"));
            let b = setup.add_fixture(li, comp, by("LED Matrix"));
            let c = setup.add_fixture(li, comp, by("RGBW"));
            let l = &mut setup.lumiverses[li];
            l.fixtures[a].input = lavmfx_core::output::rect(0.0, h * 0.05, w, h * 0.04);
            l.fixtures[b].input = lavmfx_core::output::rect(w * 0.4, h * 0.35, h * 0.3, h * 0.3);
            quad::rotate(&mut l.fixtures[b].input, 15.0);
            l.fixtures[c].input = lavmfx_core::output::rect(w * 0.8, h * 0.7, w * 0.05, h * 0.08);
            l.fixtures[c].start_channel = l.fixtures[b].start_channel + 10;
            if let Some(ed) = &mut self.screens_editor {
                ed.dmx = Some((li, Some(b)));
                ed.tab = if tab == "dmx-output" { Tab::Output } else { Tab::Input };
            }
            return;
        }
        if let Some(ed) = &mut self.screens_editor {
            ed.screen = 0;
            ed.slice = Some(1);
            if tab == "masks" {
                ed.slice = None;
                ed.mask = Some(0);
            }
            if tab.starts_with("shapes") {
                ed.slice = Some(self.outputs.setup.screens[0].slices.len() - 1);
            }
            ed.tab = if tab == "input" || tab == "inmask" || tab == "shapes-input" {
                Tab::Input
            } else {
                Tab::Output
            };
            ed.mode = if tab == "points" {
                OutMode::Points
            } else {
                OutMode::Transform
            };
            ed.grid = if tab == "output" { 8 } else { 0 };
            if tab == "points" {
                ed.points = vec![Grab::Corner(1), Grab::Point(4)];
                ed.points_of = Some((0, 1, false));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A polygon's points follow its Transform box: move, scale about a
    /// corner, rotate (the circle stays round).
    #[test]
    fn polygon_points_follow_their_box() {
        let s0 = lavmfx_core::output::rect(100.0, 100.0, 200.0, 200.0);
        let pts = PolygonShape::Circle.points((400.0, 400.0));
        let near = |a: Pt, b: Pt| (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3;
        let mut q = s0;
        quad::translate(&mut q, [10.0, -5.0]);
        assert!(near(box_map(&s0, &q, pts[0]), [pts[0][0] + 10.0, pts[0][1] - 5.0]));
        let mut q = s0;
        quad::scale(&mut q, s0[0], [2.0, 2.0]);
        assert!(
            near(box_map(&s0, &q, [300.0, 300.0]), [500.0, 500.0]),
            "BR corner scales"
        );
        assert!(near(box_map(&s0, &q, s0[0]), s0[0]), "anchor stays");
        let mut q = s0;
        quad::rotate(&mut q, 90.0);
        let c = [200.0, 200.0];
        for p in &pts {
            let r = box_map(&s0, &q, *p);
            let d0 = ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt();
            let d1 = ((r[0] - c[0]).powi(2) + (r[1] - c[1]).powi(2)).sqrt();
            assert!((d0 - d1).abs() < 1e-2, "rotation keeps the radius");
        }
    }

    /// Reordering moves one item; the others keep their order.
    #[test]
    fn reorder_moves_one_item() {
        let mut v = vec!['a', 'b', 'c', 'd'];
        assert_eq!(reorder(&mut v, 0, 3), 3); // a to the top
        assert_eq!(v, ['b', 'c', 'd', 'a']);
        assert_eq!(reorder(&mut v, 3, 0), 0); // and back to the bottom
        assert_eq!(v, ['a', 'b', 'c', 'd']);
        assert_eq!(reorder(&mut v, 2, 1), 1);
        assert_eq!(v, ['a', 'c', 'b', 'd']);
        assert_eq!(reorder(&mut v, 1, 9), 3, "clamped to the end");
        assert_eq!(v, ['a', 'b', 'd', 'c']);
    }

    /// Copy → text → paste: a new id, " copy" name, moved by the offset, on top.
    #[test]
    fn copy_paste_slice_and_mask() {
        let mut a = AdvancedOutput::with_default_screen((1920, 1080));
        let m = a.add_mask(0, MaskKind::Rectangle);
        let s = &mut a.screens[0];
        let clip = AoClip::from_text(&AoClip::Slice(Box::new(s.slices[0].clone())).to_text()).expect("round trip");
        assert_eq!(clip, AoClip::Slice(Box::new(s.slices[0].clone())));
        assert!(AoClip::from_text("hello").is_none());
        let (ns, nm) = paste_clip(s, &clip, 99, [20.0, 20.0]);
        assert_eq!((ns, nm), (Some(1), None));
        let (o, c) = (&s.slices[0], &s.slices[1]);
        assert_eq!(c.id, 99);
        assert_eq!(c.name, format!("{} copy", o.name));
        assert_eq!(c.output[0], [o.output[0][0] + 20.0, o.output[0][1] + 20.0]);
        assert_eq!(c.input[2], [o.input[2][0] + 20.0, o.input[2][1] + 20.0]);
        let mclip = AoClip::Mask(s.masks[m].clone());
        let (ns, nm) = paste_clip(s, &mclip, 100, [0.0, 0.0]);
        assert_eq!((ns, nm), (None, Some(1)));
        assert_eq!(s.masks[1].points, s.masks[0].points);
        assert_eq!(s.masks[1].id, 100);
    }

    fn snap() -> Snap {
        Snap {
            verts: vec![[100.0, 100.0]],
            segs: vec![([0.0, 200.0], [400.0, 200.0])],
            xs: vec![0.0, 300.0],
            ys: vec![0.0],
            thr: 10.0,
        }
    }

    #[test]
    fn snaps_points_to_corners_sides_and_lines() {
        let s = snap();
        assert_eq!(s.point([104.0, 95.0]).0, [100.0, 100.0], "corner");
        assert_eq!(s.point([50.0, 206.0]).0, [50.0, 200.0], "side");
        assert_eq!(s.point([294.0, 150.0]).0, [300.0, 150.0], "x line");
        let free = [150.0, 150.0];
        assert_eq!(s.point(free), (free, None));
    }

    #[test]
    fn picked_points_move_together() {
        let mut sl = Slice::new(1, "S", (100, 100), (100, 100));
        sl.warp.set_subdivisions(1, 1); // 3×3, centre = index 4
        let start = sl.clone();
        let picked = [Grab::Corner(0), Grab::Point(4)];
        move_points(&mut sl, &start, &picked, [10.0, 5.0]);
        assert_eq!(sl.output[0], [10.0, 5.0]);
        assert_eq!(sl.output[2], [100.0, 100.0], "unpicked corner stays");
        let c = point_pos(&sl, Grab::Point(4));
        assert!((c[0] - 60.0).abs() < 1e-3 && (c[1] - 55.0).abs() < 1e-3, "{c:?}");
    }

    #[test]
    fn snaps_moved_quads() {
        let s = snap();
        let q = lavmfx_core::output::rect(0.0, 0.0, 50.0, 50.0);
        // Moving by (47, 53): the BR corner (97, 103) lands on (100, 100).
        let (d, mark) = s.delta(&q, [47.0, 53.0]);
        assert_eq!(d, [50.0, 50.0]);
        assert_eq!(mark, Some([100.0, 100.0]));
        // Far from everything: x aligns with the 300 line (right side 296 → 300).
        let (d, _) = s.delta(&q, [246.0, 400.0]);
        assert_eq!(d, [250.0, 400.0]);
    }

    /// Ctrl / Shift+click builds a selection; changes made elsewhere (another
    /// slice picked, the list changed) drop it.
    #[test]
    fn multi_selection() {
        let setup = AdvancedOutput::with_default_screen((1920, 1080));
        let mut ed = Editor::new(&setup);
        ed.slice = Some(0);
        check_also(&mut ed, 5);
        toggle_select(&mut ed, 2, 5);
        toggle_select(&mut ed, 3, 5);
        assert_eq!((ed.slice, ed.also.clone()), (Some(0), vec![2, 3]));
        check_also(&mut ed, 5);
        assert_eq!(ed.also, [2, 3], "kept while nothing else changed");
        // Clicking a selected one again takes it out; the leading one too.
        toggle_select(&mut ed, 2, 5);
        toggle_select(&mut ed, 0, 5);
        assert_eq!((ed.slice, ed.also.clone()), (Some(3), vec![]));
        toggle_select(&mut ed, 1, 5);
        // Another slice picked the usual way, or the list changed: back to one.
        ed.slice = Some(4);
        check_also(&mut ed, 5);
        assert!(ed.also.is_empty());
        toggle_select(&mut ed, 1, 5);
        check_also(&mut ed, 4);
        assert!(ed.also.is_empty(), "a slice was added or removed");
    }

    /// Moving a slice of the selection moves its quad and its polygon points
    /// on the edited side only.
    #[test]
    fn selection_moves_quads_and_polygons() {
        let mut sl = Slice {
            polygon: Some(lavmfx_core::output::Polygon {
                input: vec![[0.0, 0.0], [10.0, 0.0], [5.0, 8.0]],
                output: vec![[100.0, 100.0], [110.0, 100.0], [105.0, 108.0]],
            }),
            ..Default::default()
        };
        sl.output = lavmfx_core::output::rect(100.0, 100.0, 10.0, 8.0);
        let moved = shifted(&sl, Tab::Output, [5.0, -2.0]);
        assert_eq!(moved.output[0], [105.0, 98.0]);
        assert_eq!(moved.polygon.as_ref().unwrap().output[2], [110.0, 106.0]);
        assert_eq!(moved.input, sl.input, "input untouched");
        assert_eq!(
            moved.polygon.as_ref().unwrap().input,
            sl.polygon.as_ref().unwrap().input
        );
    }
}
