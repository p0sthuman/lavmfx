//! Slice Chase: sequences of advanced-output slices and the step engine
//! behind the "Slice Chase …" effects.
//!
//! A [`Sequence`] is a list of steps; each step lights a set of slices.
//! Sequences are saved in the composition ([`ChaseSetup`]), while slices live
//! in the global Advanced Output setup. So a sequence either follows a
//! [`Rule`] over whatever slices exist now (new slices join by themselves),
//! or lists its slices by [`SliceRef`]: the slice id plus the screen and
//! slice names, so the slice is found again after edits (by id, see
//! [`relink`]) and in another setup (by name, see [`find`]).
//!
//! Each frame the app turns an effect's params, its resolved steps and the
//! clock into four values per slice ([`Chaser::frame`]): level, phase within
//! the step, step number and membership. The shaders only draw from those.

use serde::{Deserialize, Serialize};

use crate::Id;
use crate::model::{Composition, VideoEffect};
use crate::output::AdvancedOutput;
use crate::param::{Param, ParameterCollection};

// ------------------------------------------------------------------ model

/// The composition's sequences.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChaseSetup {
    pub sequences: Vec<Sequence>,
}

impl ChaseSetup {
    pub fn is_empty(&self) -> bool {
        self.sequences.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Sequence> {
        self.sequences.iter().find(|s| s.name == name)
    }

    /// `base`, or `base 2`, `base 3`… whichever no sequence uses yet.
    pub fn unique_name(&self, base: &str) -> String {
        let base = base.trim();
        let base = if base.is_empty() { "Sequence" } else { base };
        if self.get(base).is_none() {
            return base.to_owned();
        }
        (2..)
            .map(|i| format!("{base} {i}"))
            .find(|n| self.get(n).is_none())
            .unwrap_or_default()
    }
}

/// Steps of slices. Effects pick a sequence by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sequence {
    pub name: String,
    /// Steps worked out from the current slices; None = `steps` as listed.
    pub rule: Option<Rule>,
    /// Hand-made steps (used when `rule` is None). An empty step is a rest.
    pub steps: Vec<Vec<SliceRef>>,
}

impl Default for Sequence {
    fn default() -> Self {
        Self {
            name: "Sequence".into(),
            rule: Some(Rule::default()),
            steps: Vec::new(),
        }
    }
}

/// A slice in a hand-made step.
///
/// The keys aren't called `id` / `name`: the undo comparison ignores every
/// `id`, and the names are only a cache for finding the slice again (see
/// [`crate::history`]).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SliceRef {
    /// `Slice::id` in the Advanced Output setup.
    pub slice: u64,
    /// Screen and slice names when last linked.
    pub screen_name: String,
    pub slice_name: String,
}

impl SliceRef {
    pub fn of(s: &SliceInfo) -> Self {
        Self {
            slice: s.id,
            screen_name: s.screen.clone(),
            slice_name: s.name.clone(),
        }
    }
}

/// How a rule sequence orders the slices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Order {
    /// As listed in the Advanced Output (screens in order, slices bottom first).
    #[default]
    Setup,
    Name,
    LeftToRight,
    RightToLeft,
    TopToBottom,
    BottomToTop,
    CentreOut,
    OutsideIn,
    Clockwise,
    Anticlockwise,
    Shuffle,
}

impl Order {
    /// Menu order and names.
    pub const ALL: [(Order, &'static str); 11] = [
        (Order::Setup, "Setup order"),
        (Order::Name, "Name"),
        (Order::LeftToRight, "Left → right"),
        (Order::RightToLeft, "Right → left"),
        (Order::TopToBottom, "Top → bottom"),
        (Order::BottomToTop, "Bottom → top"),
        (Order::CentreOut, "Centre → out"),
        (Order::OutsideIn, "Outside → in"),
        (Order::Clockwise, "Clockwise"),
        (Order::Anticlockwise, "Anticlockwise"),
        (Order::Shuffle, "Shuffle"),
    ];

    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(o, _)| *o == self).map_or("?", |(_, n)| n)
    }

    /// Orders by position, where slices at about the same spot can share a step.
    pub fn spatial(self) -> bool {
        !matches!(self, Order::Setup | Order::Name | Order::Shuffle)
    }
}

/// A sequence worked out from the slices each frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub order: Order,
    /// Slices lit together; 0 = all of them in one step.
    pub per_step: u32,
    /// Spatial orders: slices at about the same position (a column for
    /// left → right, a ring for centre → out…) share a step; `per_step` is
    /// then unused.
    pub bands: bool,
    /// Only slices of this screen ("" = every screen).
    pub screen: String,
    /// Only slices whose name contains this (any case; "" = all).
    pub filter: String,
    /// Shuffle order.
    pub seed: u32,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            order: Order::Setup,
            per_step: 1,
            bands: false,
            screen: String::new(),
            filter: String::new(),
            seed: 0,
        }
    }
}

// ------------------------------------------------------------------ slices

/// A slice as the chase sees it (one per real entry of the effect slice
/// list, same order).
#[derive(Debug, Clone, PartialEq)]
pub struct SliceInfo {
    pub id: u64,
    pub screen: String,
    pub name: String,
    /// Output shape (polygon points or the quad) on the wall: screens side
    /// by side in setup order ([`wall_screens`]), screen pixels. Rules and
    /// the window's map use this: it is what the audience sees.
    pub shape: Vec<[f32; 2]>,
    /// Centre of `shape`.
    pub center: [f32; 2],
    /// Input selection in composition pixels.
    pub input: Vec<[f32; 2]>,
}

/// Gap between screens on the wall, pixels.
const WALL_GAP: f32 = 40.0;

/// Enabled screens on the wall: name and rect (x, y, w, h), side by side.
pub fn wall_screens(setup: &AdvancedOutput) -> Vec<(String, [f32; 4])> {
    let mut x = 0.0;
    setup
        .screens
        .iter()
        .filter(|s| s.enabled)
        .map(|s| {
            let r = [x, 0.0, s.width as f32, s.height as f32];
            x += s.width as f32 + WALL_GAP;
            (s.name.clone(), r)
        })
        .collect()
}

fn centre(pts: &[[f32; 2]]) -> [f32; 2] {
    let n = pts.len().max(1) as f32;
    pts.iter().fold([0.0, 0.0], |a, p| [a[0] + p[0] / n, a[1] + p[1] / n])
}

/// The slices effects see, in their order ([`AdvancedOutput::effect_slices`]).
pub fn slice_infos(setup: &AdvancedOutput) -> Vec<SliceInfo> {
    let wall = wall_screens(setup);
    setup
        .effect_slices()
        .map(|(screen, sl)| {
            let dx = setup
                .screens
                .iter()
                .filter(|s| s.enabled)
                .position(|s| s.id == screen.id)
                .and_then(|k| wall.get(k))
                .map_or(0.0, |w| w.1[0]);
            let (out, input): (Vec<[f32; 2]>, Vec<[f32; 2]>) = match &sl.polygon {
                Some(p) if p.input.len() >= 3 && p.output.len() >= 3 => (p.output.clone(), p.input.clone()),
                _ => (sl.output.to_vec(), sl.input.to_vec()),
            };
            let shape: Vec<[f32; 2]> = out.iter().map(|p| [p[0] + dx, p[1]]).collect();
            SliceInfo {
                id: sl.id,
                screen: screen.name.clone(),
                name: sl.name.clone(),
                center: centre(&shape),
                shape,
                input,
            }
        })
        .collect()
}

/// Do slices read the same part of the composition (e.g. every slice's input
/// is the full frame)? Then masking by input can't tell them apart, and Auto
/// space chases on the outputs.
pub fn inputs_overlap(slices: &[SliceInfo]) -> bool {
    let bbox = |p: &[[f32; 2]]| {
        p.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, q| {
            [b[0].min(q[0]), b[1].min(q[1]), b[2].max(q[0]), b[3].max(q[1])]
        })
    };
    let boxes: Vec<[f32; 4]> = slices.iter().map(|s| bbox(&s.input)).collect();
    let area = |b: &[f32; 4]| (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    for i in 0..boxes.len() {
        for j in i + 1..boxes.len() {
            let (a, b) = (&boxes[i], &boxes[j]);
            let inter = [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])];
            let smaller = area(a).min(area(b));
            if smaller > 0.0 && area(&inter) > 0.5 * smaller {
                return true;
            }
        }
    }
    false
}

/// The slice `r` stands for. `prefer_id`: right after the setup was edited
/// (ids are the truth there); otherwise names come first, since another
/// setup numbers its slices differently. Unchanged slices match either way.
pub fn find(r: &SliceRef, slices: &[SliceInfo], prefer_id: bool) -> Option<usize> {
    let by_id = || slices.iter().position(|s| s.id == r.slice);
    if let Some(i) = slices
        .iter()
        .position(|s| s.id == r.slice && s.name == r.slice_name && s.screen == r.screen_name)
    {
        return Some(i);
    }
    if prefer_id && let Some(i) = by_id() {
        return Some(i);
    }
    let unique = |f: &dyn Fn(&SliceInfo) -> bool| {
        let mut it = slices.iter().enumerate().filter(|(_, s)| f(s)).map(|(i, _)| i);
        let first = it.next()?;
        it.next().is_none().then_some(first)
    };
    unique(&|s| s.screen == r.screen_name && s.name == r.slice_name)
        .or_else(|| unique(&|s| s.name == r.slice_name))
        .or_else(by_id)
}

/// Point hand-made steps at the slices they now stand for (ids and names
/// updated; refs to missing slices kept, they come back with their slice).
/// True when something changed.
pub fn relink(setup: &mut ChaseSetup, slices: &[SliceInfo], prefer_id: bool) -> bool {
    let mut changed = false;
    for r in setup.sequences.iter_mut().flat_map(|s| s.steps.iter_mut().flatten()) {
        if let Some(i) = find(r, slices, prefer_id) {
            let now = SliceRef::of(&slices[i]);
            if *r != now {
                *r = now;
                changed = true;
            }
        }
    }
    changed
}

/// Fingerprint of the slices' ids and names: relink when it changes.
pub fn link_key(slices: &[SliceInfo]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for s in slices {
        (s.id, &s.screen, &s.name).hash(&mut h);
    }
    h.finish()
}

impl Sequence {
    /// Steps as indices into `slices` (missing slices left out; empty steps kept as rests).
    pub fn resolve(&self, slices: &[SliceInfo]) -> Vec<Vec<usize>> {
        match &self.rule {
            Some(r) => r.steps(slices),
            None => self
                .steps
                .iter()
                .map(|step| {
                    let mut v: Vec<usize> = step.iter().filter_map(|r| find(r, slices, false)).collect();
                    v.sort_unstable();
                    v.dedup();
                    v
                })
                .collect(),
        }
    }

    /// The same steps as hand-made ones (to edit a rule's result).
    pub fn bake(&mut self, slices: &[SliceInfo]) {
        if self.rule.is_some() {
            self.steps = self
                .resolve(slices)
                .into_iter()
                .map(|s| s.into_iter().map(|i| SliceRef::of(&slices[i])).collect())
                .collect();
            self.rule = None;
        }
    }
}

/// Indices of the clusters `vals` fall into: sorted values closer than `tol`
/// to the previous one share a cluster (a column, a row).
fn clusters(vals: &[f32], tol: f32) -> Vec<f32> {
    let mut order: Vec<usize> = (0..vals.len()).collect();
    order.sort_by(|&a, &b| vals[a].total_cmp(&vals[b]));
    let mut out = vec![0.0; vals.len()];
    let mut k = 0.0;
    for w in 0..order.len() {
        if w > 0 && vals[order[w]] - vals[order[w - 1]] > tol {
            k += 1.0;
        }
        out[order[w]] = k;
    }
    out
}

fn median(mut v: Vec<f32>) -> f32 {
    v.sort_by(f32::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(1.0)
}

impl Rule {
    /// Matching slices in order, grouped into steps.
    ///
    /// Spatial orders first find the layout's grid: slices whose centres are
    /// within half a typical slice of each other share a column (row), so a
    /// grid placed by hand, a few pixels off, still reads as a grid. Orders
    /// go by column / row and use pixels only to break ties.
    pub fn steps(&self, slices: &[SliceInfo]) -> Vec<Vec<usize>> {
        use std::f32::consts::{FRAC_PI_2, TAU};
        let filter = self.filter.trim().to_lowercase();
        let cand: Vec<usize> = (0..slices.len())
            .filter(|&i| self.screen.is_empty() || slices[i].screen == self.screen)
            .filter(|&i| filter.is_empty() || slices[i].name.to_lowercase().contains(&filter))
            .collect();
        if cand.is_empty() {
            return Vec::new();
        }
        let bands = self.bands && self.order.spatial();
        if self.per_step == 0 && !bands {
            return vec![cand];
        }
        // Grid: column / row of every candidate.
        let size = |i: usize, axis: usize| {
            let (lo, hi) = slices[i]
                .shape
                .iter()
                .fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p[axis]), hi.max(p[axis])));
            (hi - lo).max(1.0)
        };
        let tol_x = 0.5 * median(cand.iter().map(|&i| size(i, 0)).collect());
        let tol_y = 0.5 * median(cand.iter().map(|&i| size(i, 1)).collect());
        let col = clusters(&cand.iter().map(|&i| slices[i].center[0]).collect::<Vec<_>>(), tol_x);
        let row = clusters(&cand.iter().map(|&i| slices[i].center[1]).collect::<Vec<_>>(), tol_y);
        let cols = col.iter().copied().fold(0.0, f32::max);
        let rows = row.iter().copied().fold(0.0, f32::max);
        let n = cand.len() as f32;
        let mid = cand.iter().fold([0.0f32, 0.0], |a, &i| {
            [a[0] + slices[i].center[0] / n, a[1] + slices[i].center[1] / n]
        });
        // Sort key per candidate (index into `cand`): primary (steps with
        // Bands), then tie breaks.
        let key = |k: usize| -> [f32; 3] {
            let i = cand[k];
            let c = slices[i].center;
            let (gx, gy) = (col[k], row[k]);
            // Within half a slice of the middle counts as on its axis (straight
            // above the centre is 12 o'clock even if placed a few pixels off).
            let snap = |d: f32, tol: f32| if d.abs() < tol { 0.0 } else { d };
            let (dx, dy) = (snap(c[0] - mid[0], tol_x), snap(c[1] - mid[1], tol_y));
            let dist = dx.hypot(dy);
            // Grid ring around the centre cell (boxes growing outwards).
            let ring = (gx - cols / 2.0).abs().max((gy - rows / 2.0).abs());
            // The centre cell (hub) comes first in the clock orders.
            let hub = ring < 0.5;
            // Screen y points down: growing atan2 turns clockwise; 0 = 12 o'clock.
            let clock = (dy.atan2(dx) + FRAC_PI_2).rem_euclid(TAU);
            match self.order {
                Order::Setup | Order::Name => [0.0; 3],
                Order::LeftToRight => [gx, gy, c[0]],
                Order::RightToLeft => [-gx, gy, -c[0]],
                Order::TopToBottom => [gy, gx, c[1]],
                Order::BottomToTop => [-gy, gx, -c[1]],
                Order::CentreOut => [ring, dist, clock],
                Order::OutsideIn => [-ring, -dist, clock],
                Order::Clockwise => [if hub { -1.0 } else { clock }, dist, 0.0],
                Order::Anticlockwise => [if hub { -1.0 } else { (TAU - clock) % TAU }, dist, 0.0],
                Order::Shuffle => [(hash(&[u64::from(self.seed), slices[i].id]) >> 40) as f32, 0.0, 0.0],
            }
        };
        let cmp = |a: &[f32; 3], b: &[f32; 3]| {
            a[0].total_cmp(&b[0])
                .then(a[1].total_cmp(&b[1]))
                .then(a[2].total_cmp(&b[2]))
        };
        let mut order: Vec<usize> = (0..cand.len()).collect();
        match self.order {
            Order::Setup => {}
            Order::Name => order.sort_by(|&a, &b| natural_cmp(&slices[cand[a]].name, &slices[cand[b]].name)),
            _ => order.sort_by(|&a, &b| cmp(&key(a), &key(b))),
        }
        if bands {
            // A step per column / row / ring; spokes for the clock orders.
            let tol = if matches!(self.order, Order::Clockwise | Order::Anticlockwise) {
                0.03 * TAU
            } else {
                1e-3
            };
            let mut out: Vec<Vec<usize>> = Vec::new();
            let mut last = f32::NAN;
            for k in order {
                let p = key(k)[0];
                match out.last_mut() {
                    Some(step) if (p - last).abs() <= tol => step.push(cand[k]),
                    _ => out.push(vec![cand[k]]),
                }
                last = p;
            }
            return out;
        }
        order
            .iter()
            .map(|&k| cand[k])
            .collect::<Vec<_>>()
            .chunks(self.per_step.max(1) as usize)
            .map(<[usize]>::to_vec)
            .collect()
    }
}

/// Case-insensitive, numbers by value ("Slice 2" before "Slice 10").
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn parts(s: &str) -> Vec<(bool, String)> {
        let mut v: Vec<(bool, String)> = Vec::new();
        for c in s.to_lowercase().chars() {
            let d = c.is_ascii_digit();
            match v.last_mut() {
                Some((pd, t)) if *pd == d => t.push(c),
                _ => v.push((d, c.to_string())),
            }
        }
        v
    }
    let (pa, pb) = (parts(a), parts(b));
    for (x, y) in pa.iter().zip(&pb) {
        let o = match (x.0, y.0) {
            (true, true) => {
                let (tx, ty) = (x.1.trim_start_matches('0'), y.1.trim_start_matches('0'));
                tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty))
            }
            _ => x.1.cmp(&y.1),
        };
        if o.is_ne() {
            return o;
        }
    }
    pa.len().cmp(&pb.len())
}

/// SplitMix64 over the words: stable pseudo-random numbers (same every run).
fn hash(words: &[u64]) -> u64 {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for w in words {
        x ^= w
            .wrapping_add(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(x << 6)
            .wrapping_add(x >> 2);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x = z ^ (z >> 31);
    }
    x
}

// ------------------------------------------------------------------ effects

/// Internal names of the Slice Chase effects.
pub const EFFECTS: &[&str] = &[
    "SliceChaseSequencer",
    "SliceChaseWipe",
    "SliceChaseRunner",
    "SliceChaseTiles",
    "SliceChaseSample",
    "SliceChaseDelay",
];

/// Shader kind of the first Slice Chase effect; a look's number in
/// `chase.wgsl` is its kind minus this (Wipe 1 … Delay 5).
pub const FIRST_KIND: u32 = 103;

pub fn is_chase(name: &str) -> bool {
    EFFECTS.contains(&name)
}

/// The Sequence choice's first option: every slice, one per step.
pub const ALL_SLICES: &str = "All Slices";
pub const SEQUENCE_KEY: &str = "Sequence";
/// Event params (buttons).
pub const EVENT_KEYS: [&str; 4] = ["Next", "Previous", "Blast", "Reset"];

/// Options of the effects' Sequence choice.
pub fn sequence_options(setup: &ChaseSetup) -> Vec<&str> {
    std::iter::once(ALL_SLICES)
        .chain(setup.sequences.iter().map(|s| s.name.as_str()))
        .collect()
}

/// Name of the sequence an effect plays.
pub fn effect_sequence(e: &VideoEffect) -> &str {
    match e.params.get(SEQUENCE_KEY) {
        Some(Param::Choice(c)) => c.value.as_str(),
        _ => ALL_SLICES,
    }
}

/// The steps an effect plays.
pub fn effect_steps(setup: &ChaseSetup, e: &VideoEffect, slices: &[SliceInfo]) -> Vec<Vec<usize>> {
    match setup.get(effect_sequence(e)) {
        Some(s) => s.resolve(slices),
        None => Rule::default().steps(slices),
    }
}

/// Every effect chain of the composition (composition, groups, layers, the
/// active deck's clips).
fn chains_mut(c: &mut Composition) -> Vec<&mut Vec<VideoEffect>> {
    let mut v: Vec<&mut Vec<VideoEffect>> = Vec::new();
    if let Some(t) = c.video.as_mut() {
        v.push(&mut t.effects);
    }
    for g in &mut c.layergroups {
        if let Some(t) = g.video.as_mut() {
            v.push(&mut t.effects);
        }
    }
    for l in &mut c.layers {
        if let Some(t) = l.video.as_mut() {
            v.push(&mut t.track.effects);
        }
        for k in &mut l.clips {
            if let Some(t) = k.video.as_mut() {
                v.push(&mut t.track.effects);
            }
        }
    }
    v
}

impl Composition {
    /// Every Slice Chase effect (composition, groups, layers, active deck's clips).
    pub fn chase_effects(&self) -> impl Iterator<Item = &VideoEffect> {
        let comp = self.video.as_ref().map(|t| &t.effects);
        let groups = self
            .layergroups
            .iter()
            .filter_map(|g| g.video.as_ref().map(|t| &t.effects));
        let layers = self.layers.iter().flat_map(|l| {
            l.video.as_ref().map(|t| &t.track.effects).into_iter().chain(
                l.clips
                    .iter()
                    .filter_map(|k| k.video.as_ref().map(|t| &t.track.effects)),
            )
        });
        comp.into_iter()
            .chain(groups)
            .chain(layers)
            .flatten()
            .filter(|e| is_chase(&e.name))
    }

    /// Run `f` on every Slice Chase effect (same chains as [`Self::chase_effects`]).
    pub fn for_each_chase_mut(&mut self, mut f: impl FnMut(&mut VideoEffect)) {
        for chain in chains_mut(self) {
            chain.iter_mut().filter(|e| is_chase(&e.name)).for_each(&mut f);
        }
    }

    /// Keep every chase effect's Sequence choice listing the sequences
    /// (`renamed`: old → new name, so effects follow a rename). A deleted
    /// sequence falls back to All Slices.
    pub fn sync_chase(&mut self, renamed: Option<(&str, &str)>) {
        let names: Vec<String> = sequence_options(&self.chase).into_iter().map(str::to_owned).collect();
        let opts: Vec<&str> = names.iter().map(String::as_str).collect();
        for chain in chains_mut(self) {
            for e in chain.iter_mut().filter(|e| is_chase(&e.name)) {
                if let Some(Param::Choice(c)) = e.params.get_mut(SEQUENCE_KEY) {
                    if let Some((old, new)) = renamed
                        && c.value == old
                    {
                        c.value = new.to_owned();
                        c.options.clear(); // realign even if the list looks the same
                    }
                    c.realign(&opts, |v| v, ALL_SLICES);
                }
            }
        }
    }
}

// ------------------------------------------------------------------ engine

/// What steps the chase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Advance {
    /// One step per `Step Length` beats.
    #[default]
    Bpm,
    /// One step per `Step Time` seconds.
    Time,
    /// `Position` 0..1 across the steps (drive it from the P menu).
    Position,
    /// A step each time the FFT band rises over the threshold.
    Fft,
    /// Next / Previous buttons.
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    #[default]
    Forward,
    Backward,
    PingPong,
    /// Any other step (never the same twice).
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Curve {
    #[default]
    Linear,
    Smooth,
    /// Drops fast, then lingers.
    Sharp,
    /// Full until the trail ends.
    Hold,
}

impl Curve {
    fn shape(self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Curve::Linear => x,
            Curve::Smooth => x * x * (3.0 - 2.0 * x),
            Curve::Sharp => x * x * x,
            Curve::Hold => {
                if x > 0.0 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }
}

/// Step Length options in beats.
pub const STEP_BEATS: [f64; 9] = [0.0625, 0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];

/// Where an effect chases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Space {
    /// Output when the slices' inputs overlap ([`inputs_overlap`]), else Input.
    #[default]
    Auto,
    /// Masks the layer inside each slice's input selection.
    Input,
    /// Dims each whole slice on the outputs (everything it shows).
    Output,
}

/// What slices outside the sequence do (Outside param).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Outside {
    #[default]
    Clear,
    Black,
    Keep,
}

/// An effect's chase settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    pub space: Space,
    /// Level of dark slices (0..1).
    pub unlit: f64,
    pub outside: Outside,
    /// Random: light `count` random slices of the sequence each step
    /// instead of its steps.
    pub random: bool,
    pub advance: Advance,
    pub beats: f64,
    pub seconds: f64,
    pub position: f64,
    pub fft_band: usize,
    pub fft_threshold: f64,
    pub direction: Direction,
    pub count: usize,
    /// Fade in, in steps (0..1).
    pub attack: f64,
    /// Fade out after the step, in steps.
    pub trail: f64,
    pub curve: Curve,
    pub invert: bool,
    /// Manual: dark again after this many seconds without a press (0 = never).
    pub auto_reset: f64,
    pub seed: u64,
    /// Delay: count the steps from the end.
    pub reverse: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            space: Space::Auto,
            unlit: 0.0,
            outside: Outside::Clear,
            random: false,
            advance: Advance::Bpm,
            beats: 1.0,
            seconds: 0.5,
            position: 0.0,
            fft_band: 0,
            fft_threshold: 0.5,
            direction: Direction::Forward,
            count: 1,
            attack: 0.0,
            trail: 0.0,
            curve: Curve::Linear,
            invert: false,
            auto_reset: 0.0,
            seed: 0,
            reverse: false,
        }
    }
}

fn range(p: &ParameterCollection, k: &str, d: f64) -> f64 {
    match p.get(k) {
        Some(Param::Range(r)) => r.value,
        _ => d,
    }
}

fn index(p: &ParameterCollection, k: &str) -> usize {
    match p.get(k) {
        Some(Param::Choice(c)) => c.index.max(0) as usize,
        _ => 0,
    }
}

impl Params {
    pub fn of(e: &VideoEffect) -> Self {
        let p = &e.params;
        Self {
            space: [Space::Auto, Space::Input, Space::Output][index(p, "Space").min(2)],
            unlit: range(p, "Unlit", 0.0).clamp(0.0, 1.0),
            outside: [Outside::Clear, Outside::Black, Outside::Keep][index(p, "Outside").min(2)],
            random: index(p, "Pattern") == 1,
            advance: [
                Advance::Bpm,
                Advance::Time,
                Advance::Position,
                Advance::Fft,
                Advance::Manual,
            ][index(p, "Advance").min(4)],
            beats: STEP_BEATS[index(p, "Step Length").min(STEP_BEATS.len() - 1)],
            seconds: range(p, "Step Time", 0.5).max(0.02),
            position: range(p, "Position", 0.0),
            fft_band: index(p, "FFT Band").min(2),
            fft_threshold: range(p, "FFT Threshold", 0.5),
            direction: [
                Direction::Forward,
                Direction::Backward,
                Direction::PingPong,
                Direction::Random,
            ][index(p, "Direction").min(3)],
            count: range(p, "Random Count", 1.0).round().max(1.0) as usize,
            attack: range(p, "Attack", 0.0).max(0.0),
            trail: range(p, "Trail", 0.0).max(0.0),
            curve: [Curve::Linear, Curve::Smooth, Curve::Sharp, Curve::Hold][index(p, "Curve").min(3)],
            invert: matches!(p.get("Invert"), Some(Param::Bool(b)) if b.value),
            auto_reset: range(p, "Auto Reset", 0.0).max(0.0),
            seed: range(p, "Seed", 0.0).round().max(0.0) as u64,
            reverse: matches!(p.get("Reverse"), Some(Param::Bool(b)) if b.value),
        }
    }
}

impl Params {
    /// Chase on the outputs (whole slices) rather than in the layer?
    pub fn on_outputs(&self, slices: &[SliceInfo]) -> bool {
        match self.space {
            Space::Auto => inputs_overlap(slices),
            Space::Input => false,
            Space::Output => true,
        }
    }

    /// Output space: the multiplier of each slice (Unlit, Outside and the
    /// effect's dry/wet `opacity` applied) from its [`SliceValues`].
    pub fn output_level(&self, v: &SliceValues, opacity: f64) -> f32 {
        let wet = if v[3] < 0.5 {
            match self.outside {
                Outside::Keep => 1.0,
                Outside::Clear | Outside::Black => 0.0,
            }
        } else {
            self.unlit + (1.0 - self.unlit) * f64::from(v[0]).clamp(0.0, 1.0)
        };
        let o = opacity.clamp(0.0, 1.0);
        (1.0 - o + o * wet) as f32
    }
}

/// Button presses for one effect this frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Presses {
    pub next: u32,
    pub previous: u32,
    pub blast: bool,
    pub reset: bool,
}

impl Presses {
    /// The presses of `e`'s buttons among `fired` ([`crate::trigger::take`]).
    pub fn of(e: &VideoEffect, fired: &[Id]) -> Self {
        let count = |k: &str| match e.params.get(k) {
            Some(Param::Event(ev)) => fired.iter().filter(|&&f| f == ev.id).count() as u32,
            _ => 0,
        };
        Self {
            next: count("Next"),
            previous: count("Previous"),
            blast: count("Blast") > 0,
            reset: count("Reset") > 0,
        }
    }
}

/// Clock and audio this frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Tick {
    pub seconds: f64,
    pub beats: f64,
    /// Low / mid / high FFT levels, 0..1.
    pub fft: [f64; 3],
}

/// Low / mid / high levels of a spectrum (band means).
pub fn fft_levels(s: &crate::fft::Spectrum) -> [f64; 3] {
    let mean = |r: std::ops::Range<usize>| {
        let n = r.len().max(1) as f64;
        r.map(|i| f64::from(s[i])).sum::<f64>() / n
    };
    let n = s.len();
    [mean(0..n / 4), mean(n / 4..(n * 5) / 8), mean((n * 5) / 8..n)]
}

/// Per-slice output: level 0..1, phase in its step 0..1 (1 once released),
/// step number it was last lit in, 1 = the slice is in the sequence.
pub type SliceValues = [f32; 4];

/// One effect's running chase (not saved).
#[derive(Debug, Clone, Default)]
pub struct Chaser {
    /// Step count of the lit step (None = dark).
    counter: Option<i64>,
    /// Sequence step shown, and the slices lit.
    step: usize,
    lit: Vec<usize>,
    /// Per slice, in fade-clock units: when lit / released (NaN = never).
    on_at: Vec<f64>,
    off_at: Vec<f64>,
    step_of: Vec<f32>,
    blast_at: f64,
    last_clock: f64,
    fft_count: i64,
    fft_high: bool,
    manual: Option<i64>,
    last_press: f64,
}

impl Chaser {
    /// Values per slice for this frame. `steps` index `0..n` slices.
    pub fn frame(&mut self, p: &Params, steps: &[Vec<usize>], n: usize, t: &Tick, press: Presses) -> Vec<SliceValues> {
        if self.on_at.len() != n {
            *self = Self {
                on_at: vec![f64::NAN; n],
                off_at: vec![f64::NAN; n],
                step_of: vec![0.0; n],
                blast_at: f64::NAN,
                manual: self.manual,
                ..Default::default()
            };
        }
        let mut member = vec![false; n];
        for &i in steps.iter().flatten().filter(|&&i| i < n) {
            member[i] = true;
        }
        // Fade clock: steps for the clocked modes, Step Time units otherwise.
        let clock = match p.advance {
            Advance::Bpm => t.beats / p.beats.max(1e-3),
            _ => t.seconds / p.seconds.max(1e-3),
        };
        if clock + 1e-9 < self.last_clock || press.reset {
            // The clock jumped back (restart, resync) or Reset: start over.
            self.on_at.fill(f64::NAN);
            self.off_at.fill(f64::NAN);
            self.blast_at = f64::NAN;
            self.lit.clear();
            self.counter = None;
            if press.reset {
                self.manual = None;
            }
        }
        self.last_clock = clock;
        let nsteps = if p.random {
            usize::from(member.contains(&true))
        } else {
            steps.len()
        };
        // Which step count should be lit.
        let want: Option<i64> = if nsteps == 0 {
            None
        } else {
            match p.advance {
                Advance::Bpm | Advance::Time => Some(clock.floor() as i64),
                Advance::Position => {
                    let k = if p.random { 64 } else { nsteps };
                    Some(((p.position.clamp(0.0, 1.0) * k as f64).floor() as i64).min(k as i64 - 1))
                }
                Advance::Fft => {
                    let level = t.fft[p.fft_band.min(2)];
                    if !self.fft_high && level >= p.fft_threshold {
                        self.fft_high = true;
                        self.fft_count += 1;
                    } else if self.fft_high && level < p.fft_threshold * 0.8 {
                        self.fft_high = false;
                    }
                    Some(self.fft_count)
                }
                Advance::Manual => {
                    let d = i64::from(press.next) - i64::from(press.previous);
                    if press.next + press.previous > 0 {
                        self.last_press = t.seconds;
                        self.manual = Some(match self.manual {
                            Some(m) => m + d,
                            None if d >= 0 => d.max(1) - 1,
                            None => d,
                        });
                    } else if p.auto_reset > 0.0 && t.seconds - self.last_press > p.auto_reset {
                        self.manual = None;
                    }
                    self.manual
                }
            }
        };
        let clocked = matches!(p.advance, Advance::Bpm | Advance::Time);
        if want != self.counter {
            let at = match want {
                Some(c) if clocked => c as f64,
                _ => clock,
            };
            let new: Vec<usize> = match want {
                None => Vec::new(),
                Some(c) if p.random => {
                    let mut pool: Vec<usize> = (0..n).filter(|&i| member[i]).collect();
                    pool.sort_by_key(|&i| (self.lit.contains(&i), hash(&[p.seed, c as u64, i as u64])));
                    pool.truncate(p.count.max(1));
                    pool
                }
                Some(c) => {
                    self.step = pick(p.direction, c, nsteps, self.step, p.seed);
                    steps[self.step].iter().copied().filter(|&i| i < n).collect()
                }
            };
            for &i in &self.lit {
                if !new.contains(&i) {
                    self.off_at[i] = at;
                }
            }
            for &i in &new {
                self.on_at[i] = at;
                self.off_at[i] = f64::NAN;
                self.step_of[i] = if p.random {
                    want.unwrap_or(0) as f32
                } else {
                    self.step as f32
                };
            }
            self.lit = new;
            self.counter = want;
        }
        if press.blast {
            self.blast_at = clock;
        }
        let mut lit = vec![false; n];
        for &i in &self.lit {
            lit[i] = true;
        }
        // Step number per slice: its own step in the sequence (stable, so odd /
        // even alternation and Delay's offsets don't jump); Random: the step it
        // was last lit in.
        let mut first = vec![0.0f32; n];
        if !p.random {
            for (k, step) in steps.iter().enumerate().rev() {
                for &i in step.iter().filter(|&&i| i < n) {
                    let k = if p.reverse { steps.len() - 1 - k } else { k };
                    first[i] = k as f32;
                }
            }
        }
        (0..n)
            .map(|i| {
                let (mut level, phase) = if lit[i] {
                    let age = clock - self.on_at[i];
                    let l = if p.attack > 0.0 {
                        p.curve.shape(age / p.attack)
                    } else {
                        1.0
                    };
                    (l, age.clamp(0.0, 1.0))
                } else if self.off_at[i].is_finite() && p.trail > 0.0 {
                    (p.curve.shape(1.0 - (clock - self.off_at[i]) / p.trail), 1.0)
                } else {
                    (0.0, 1.0)
                };
                if member[i] && self.blast_at.is_finite() {
                    let b = p.curve.shape(1.0 - (clock - self.blast_at) / p.trail.max(1.0));
                    level = level.max(b);
                }
                if p.invert && member[i] {
                    level = 1.0 - level;
                }
                [
                    level as f32,
                    phase as f32,
                    if p.random { self.step_of[i] } else { first[i] },
                    f32::from(u8::from(member[i])),
                ]
            })
            .collect()
    }
}

/// Sequence step for step count `c` of `n` (`prev` = the step shown before).
fn pick(d: Direction, c: i64, n: usize, prev: usize, seed: u64) -> usize {
    let n64 = n.max(1) as i64;
    match d {
        Direction::Forward => c.rem_euclid(n64) as usize,
        Direction::Backward => (n64 - 1 - c.rem_euclid(n64)) as usize,
        Direction::PingPong => {
            if n < 2 {
                return 0;
            }
            let period = 2 * n64 - 2;
            let k = c.rem_euclid(period);
            (if k < n64 { k } else { period - k }) as usize
        }
        Direction::Random => {
            if n < 2 {
                return 0;
            }
            let r = (hash(&[seed, c as u64]) % (n as u64 - 1)) as usize;
            (prev.min(n - 1) + 1 + r) % n
        }
    }
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{Screen, Slice};

    fn sl(id: u64, name: &str, x: f32, y: f32) -> Slice {
        let (w, h) = (100.0, 100.0);
        Slice {
            id,
            name: name.into(),
            input: [[x, y], [x + w, y], [x + w, y + h], [x, y + h]],
            output: [[x, y], [x + w, y], [x + w, y + h], [x, y + h]],
            ..Default::default()
        }
    }

    /// A 3 × 2 grid of slices on one screen ("S 1".."S 6", left to right, top row first).
    fn setup() -> AdvancedOutput {
        let mut screen = Screen {
            name: "Wall".into(),
            ..Default::default()
        };
        for r in 0..2 {
            for c in 0..3 {
                let id = (r * 3 + c + 1) as u64;
                screen
                    .slices
                    .push(sl(id, &format!("S {id}"), c as f32 * 200.0, r as f32 * 200.0));
            }
        }
        AdvancedOutput {
            screens: vec![screen],
            ..Default::default()
        }
    }

    fn rule(order: Order, per_step: u32, bands: bool) -> Rule {
        Rule {
            order,
            per_step,
            bands,
            ..Default::default()
        }
    }

    #[test]
    fn rules_order_and_group() {
        let s = slice_infos(&setup());
        assert_eq!(rule(Order::Setup, 1, false).steps(&s), [[0], [1], [2], [3], [4], [5]]);
        assert_eq!(rule(Order::Setup, 2, false).steps(&s), [[0, 1], [2, 3], [4, 5]]);
        assert_eq!(rule(Order::Setup, 0, false).steps(&s), [[0, 1, 2, 3, 4, 5]]);
        // Columns left → right, rows bottom → top.
        assert_eq!(
            rule(Order::LeftToRight, 1, true).steps(&s),
            [vec![0, 3], vec![1, 4], vec![2, 5]]
        );
        assert_eq!(
            rule(Order::BottomToTop, 1, true).steps(&s),
            [vec![3, 4, 5], vec![0, 1, 2]]
        );
        // Clockwise from 12 o'clock around the middle: top row left of centre
        // comes last, the top-middle slice first.
        let cw = rule(Order::Clockwise, 1, false).steps(&s);
        assert_eq!(cw[0], [1]);
        assert_eq!(cw.last().unwrap(), &[0]);
        // Shuffle: every slice once, same order for the same seed.
        let sh = rule(Order::Shuffle, 1, false).steps(&s);
        let mut all: Vec<usize> = sh.iter().flatten().copied().collect();
        all.sort_unstable();
        assert_eq!(all, [0, 1, 2, 3, 4, 5]);
        assert_eq!(sh, rule(Order::Shuffle, 1, false).steps(&s));
        // Filters.
        let f = Rule {
            filter: "s 1".into(),
            ..Default::default()
        };
        assert_eq!(f.steps(&s), [[0]]);
        let other = Rule {
            screen: "Other".into(),
            ..Default::default()
        };
        assert!(other.steps(&s).is_empty());
    }

    /// Every slice reads the full frame, the grid is on the outputs (a common
    /// way to map): rules follow the outputs and Auto chases on them.
    #[test]
    fn full_frame_inputs_chase_on_the_outputs() {
        let mut out = setup();
        assert!(!inputs_overlap(&slice_infos(&out)), "a grid of inputs");
        for s in &mut out.screens[0].slices {
            s.input = [[0.0, 0.0], [1920.0, 0.0], [1920.0, 1080.0], [0.0, 1080.0]];
        }
        let infos = slice_infos(&out);
        assert!(inputs_overlap(&infos));
        assert_eq!(
            rule(Order::LeftToRight, 1, true).steps(&infos),
            [vec![0, 3], vec![1, 4], vec![2, 5]]
        );
        let p = Params::default();
        assert!(p.on_outputs(&infos));
        let input = Params {
            space: Space::Input,
            ..Default::default()
        };
        assert!(!input.on_outputs(&infos));
        // Levels: lit, dark with Unlit 20 %, outside the sequence (Clear | Keep), half dry.
        let p = Params {
            unlit: 0.2,
            ..Default::default()
        };
        assert_eq!(p.output_level(&[1.0, 0.0, 0.0, 1.0], 1.0), 1.0);
        assert!((p.output_level(&[0.0, 0.0, 0.0, 1.0], 1.0) - 0.2).abs() < 1e-6);
        assert_eq!(p.output_level(&[0.0, 0.0, 0.0, 0.0], 1.0), 0.0);
        let keep = Params {
            outside: Outside::Keep,
            ..Default::default()
        };
        assert_eq!(keep.output_level(&[0.0, 0.0, 0.0, 0.0], 1.0), 1.0);
        assert!((p.output_level(&[0.0, 0.0, 0.0, 1.0], 0.5) - 0.6).abs() < 1e-6);
    }

    #[test]
    fn screens_side_by_side_on_the_wall() {
        let mut out = setup();
        let mut b = out.screens[0].clone();
        b.id = 2;
        b.name = "Floor".into();
        for s in &mut b.slices {
            s.id += 50;
        }
        out.screens[0].id = 1;
        out.screens.push(b);
        let infos = slice_infos(&out);
        let w = wall_screens(&out);
        assert_eq!(w[1].1[0], 1920.0 + WALL_GAP);
        // Left → right runs over the first screen, then the second.
        let lr = rule(Order::LeftToRight, 1, true).steps(&infos);
        assert_eq!(lr.len(), 6);
        assert!(lr[3].iter().all(|&i| infos[i].screen == "Floor"));
    }

    /// A 3 × 5 grid placed by hand (slices a few pixels off their column /
    /// row, all reading the full frame): rules still read it as a grid.
    #[test]
    fn hand_placed_grid_orders() {
        let mut screen = Screen {
            name: "Wall".into(),
            width: 1829,
            height: 1029,
            ..Default::default()
        };
        // Listed column by column, like duplicating a slice downwards.
        let jitter = [
            0.0, -3.0, 2.0, 14.0, -1.0, 17.0, 3.0, -2.0, 22.0, 0.0, 17.0, -4.0, 19.0, 1.0, 0.0,
        ];
        for c in 0..5 {
            for r in 0..3 {
                let k = c * 3 + r;
                let (x, y) = (c as f32 * 340.0 + jitter[k], r as f32 * 275.0 + jitter[14 - k]);
                let q = [[x, y], [x + 320.0, y], [x + 320.0, y + 260.0], [x, y + 260.0]];
                screen.slices.push(Slice {
                    id: k as u64 + 1,
                    name: format!("S {}", k + 1),
                    input: [[0.0, 0.0], [1920.0, 0.0], [1920.0, 1080.0], [0.0, 1080.0]],
                    output: q,
                    ..Default::default()
                });
            }
        }
        let out = AdvancedOutput {
            screens: vec![screen],
            ..Default::default()
        };
        let s = slice_infos(&out);
        // Index of the slice at column c, row r.
        let at = |c: usize, r: usize| c * 3 + r;
        let lr = rule(Order::LeftToRight, 1, false).steps(&s);
        assert_eq!(
            lr.iter().flatten().copied().collect::<Vec<_>>(),
            (0..15).collect::<Vec<_>>(),
            "columns read top to bottom"
        );
        let tb = rule(Order::TopToBottom, 1, false).steps(&s);
        let rows: Vec<usize> = (0..3).flat_map(|r| (0..5).map(move |c| at(c, r))).collect();
        assert_eq!(
            tb.iter().flatten().copied().collect::<Vec<_>>(),
            rows,
            "rows read left to right"
        );
        assert_eq!(rule(Order::LeftToRight, 1, true).steps(&s).len(), 5);
        assert_eq!(rule(Order::TopToBottom, 1, true).steps(&s).len(), 3);
        // Box rings around the centre: centre, the 3 × 3 around it, the outer columns.
        let rings = rule(Order::CentreOut, 1, true).steps(&s);
        assert_eq!(rings.len(), 3);
        assert_eq!(rings[0], [at(2, 1)]);
        assert_eq!(rings[1].len(), 8);
        assert_eq!(rings[2].len(), 6);
        // Clockwise: the centre, then straight up, then on to the right.
        let cw = rule(Order::Clockwise, 1, false).steps(&s);
        assert_eq!(cw[0], [at(2, 1)]);
        assert_eq!(cw[1], [at(2, 0)]);
        assert_eq!(cw[2], [at(3, 0)]);
        let acw = rule(Order::Anticlockwise, 1, false).steps(&s);
        assert_eq!((acw[1][0], acw[2][0]), (at(2, 0), at(1, 0)));
    }

    /// Every Slice Chase effect: in the catalogue, drawn by the renderer, its
    /// chase controls at the engine's defaults (Delay shows every slice).
    #[test]
    fn all_chase_effects() {
        let mut ids = crate::IdGen::new();
        for name in EFFECTS {
            let e = VideoEffect::from_catalog(&mut ids, name).unwrap_or_else(|| panic!("{name}"));
            let fx = crate::effects::shader_fx(&e).unwrap_or_else(|| panic!("{name} not drawn"));
            assert!(fx.kind >= FIRST_KIND && fx.kind < FIRST_KIND + EFFECTS.len() as u32);
            let p = Params::of(&e);
            let unlit = if *name == "SliceChaseDelay" { 1.0 } else { 0.0 };
            assert_eq!(
                p,
                Params {
                    unlit,
                    ..Default::default()
                },
                "{name}"
            );
            for k in EVENT_KEYS {
                assert!(matches!(e.params.get(k), Some(Param::Event(_))), "{name}: {k}");
            }
        }
    }

    /// Each slice's step number is its own step in the sequence (Reverse:
    /// counted from the end), whatever is lit.
    #[test]
    fn step_numbers_are_the_slices_own() {
        let steps = vec![vec![0], vec![1, 2], vec![3]];
        let mut c = Chaser::default();
        let v = run(&mut c, &Params::default(), &steps, 4, 0.1);
        assert_eq!(v.iter().map(|x| x[2]).collect::<Vec<_>>(), [0.0, 1.0, 1.0, 2.0]);
        let rev = Params {
            reverse: true,
            ..Default::default()
        };
        let v = run(&mut Chaser::default(), &rev, &steps, 4, 0.1);
        assert_eq!(v.iter().map(|x| x[2]).collect::<Vec<_>>(), [2.0, 1.0, 1.0, 0.0]);
    }

    /// Every panel param reaches the engine under its catalogue name.
    #[test]
    fn effect_params_reach_the_engine() {
        let mut ids = crate::IdGen::new();
        let mut e = VideoEffect::from_catalog(&mut ids, "SliceChaseSequencer").unwrap();
        assert_eq!(
            Params::of(&e),
            Params::default(),
            "catalogue defaults = engine defaults"
        );
        let set = |e: &mut VideoEffect, k: &str, v: &str| match e.params.get_mut(k) {
            Some(Param::Choice(c)) => assert!(c.select_value(v), "{k}: no option {v}"),
            Some(Param::Range(r)) => r.value = v.parse().unwrap(),
            Some(Param::Bool(b)) => b.value = v == "1",
            _ => panic!("no param {k}"),
        };
        for (k, v) in [
            ("Space", "Output"),
            ("Pattern", "Random"),
            ("Advance", "Manual"),
            ("Step Length", "1/4"),
            ("Step Time", "2"),
            ("Position", "0.5"),
            ("FFT Band", "High"),
            ("FFT Threshold", "0.3"),
            ("Direction", "Ping-Pong"),
            ("Random Count", "4"),
            ("Attack", "0.25"),
            ("Trail", "3"),
            ("Curve", "Hold"),
            ("Invert", "1"),
            ("Auto Reset", "5"),
            ("Seed", "7"),
            ("Unlit", "0.1"),
            ("Outside", "Keep"),
        ] {
            set(&mut e, k, v);
        }
        let p = Params::of(&e);
        assert_eq!(
            p,
            Params {
                space: Space::Output,
                unlit: 0.1,
                outside: Outside::Keep,
                random: true,
                advance: Advance::Manual,
                beats: 0.25,
                seconds: 2.0,
                position: 0.5,
                fft_band: 2,
                fft_threshold: 0.3,
                direction: Direction::PingPong,
                count: 4,
                attack: 0.25,
                trail: 3.0,
                curve: Curve::Hold,
                invert: true,
                auto_reset: 5.0,
                seed: 7,
                reverse: false,
            }
        );
        // The shader reads Scaling / Unlit / Outside in its slots.
        let fx = crate::effects::shader_fx(&e).unwrap();
        assert_eq!(fx.params[..3], [0.0, 0.1, 2.0]);
    }

    #[test]
    fn natural_name_order() {
        let mut v = vec!["Slice 10", "slice 2", "Slice 1", "Arch"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["Arch", "Slice 1", "slice 2", "Slice 10"]);
    }

    #[test]
    fn hand_made_steps_follow_their_slices() {
        let mut out = setup();
        let infos = slice_infos(&out);
        let mut seq = Sequence {
            name: "Hand".into(),
            rule: Some(rule(Order::RightToLeft, 1, true)),
            steps: Vec::new(),
        };
        seq.bake(&infos);
        assert!(seq.rule.is_none());
        assert_eq!(seq.resolve(&infos), [vec![2, 5], vec![1, 4], vec![0, 3]]);
        let mut setup_c = ChaseSetup { sequences: vec![seq] };

        // Renamed and moved to the top of the list in the same setup: found by id.
        let mut moved = out.screens[0].slices.remove(2);
        moved.name = "Corner".into();
        out.screens[0].slices.push(moved);
        let infos = slice_infos(&out);
        assert!(relink(&mut setup_c, &infos, true));
        assert_eq!(setup_c.sequences[0].steps[0][0].slice_name, "Corner");
        assert_eq!(setup_c.sequences[0].resolve(&infos)[0], [4, 5]);

        // Another setup: same names, other ids → found by name.
        let mut other = out.clone();
        for (k, s) in other.screens[0].slices.iter_mut().enumerate() {
            s.id = 100 + k as u64;
        }
        let infos2 = slice_infos(&other);
        assert_eq!(
            setup_c.sequences[0].resolve(&infos2),
            setup_c.sequences[0].resolve(&infos)
        );
        assert!(relink(&mut setup_c, &infos2, false));
        assert!(setup_c.sequences[0].steps[0].iter().all(|r| r.slice >= 100));

        // A deleted slice drops out of its step but its ref is kept.
        other.screens[0].slices.retain(|s| s.name != "Corner");
        let infos3 = slice_infos(&other);
        assert!(!relink(&mut setup_c, &infos3, true));
        assert_eq!(setup_c.sequences[0].steps[0].len(), 2);
        assert_eq!(setup_c.sequences[0].resolve(&infos3)[0].len(), 1);
    }

    #[test]
    fn same_names_on_two_screens() {
        let mut out = setup();
        let mut b = out.screens[0].clone();
        b.name = "Floor".into();
        for s in &mut b.slices {
            s.id += 50;
        }
        out.screens.push(b);
        let infos = slice_infos(&out);
        let r = SliceRef {
            slice: 999, // unknown id: names decide
            screen_name: "Floor".into(),
            slice_name: "S 2".into(),
        };
        assert_eq!(infos[find(&r, &infos, false).unwrap()].id, 52);
    }

    fn run(c: &mut Chaser, p: &Params, steps: &[Vec<usize>], n: usize, beats: f64) -> Vec<SliceValues> {
        c.frame(
            p,
            steps,
            n,
            &Tick {
                seconds: beats * 0.5,
                beats,
                fft: [0.0; 3],
            },
            Presses::default(),
        )
    }

    fn levels(v: &[SliceValues]) -> Vec<f32> {
        v.iter().map(|x| (x[0] * 100.0).round() / 100.0).collect()
    }

    #[test]
    fn bpm_steps_and_trails() {
        let steps = vec![vec![0], vec![1], vec![2]];
        let mut c = Chaser::default();
        let p = Params::default();
        assert_eq!(levels(&run(&mut c, &p, &steps, 4, 0.1)), [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(levels(&run(&mut c, &p, &steps, 4, 1.5)), [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(levels(&run(&mut c, &p, &steps, 4, 3.2)), [1.0, 0.0, 0.0, 0.0], "wraps");
        // Slice 3 isn't in the sequence.
        assert_eq!(run(&mut c, &p, &steps, 4, 3.3)[3][3], 0.0);

        // Two-step trail: half a step after the change the old slice is at 75 %.
        let p = Params {
            trail: 2.0,
            beats: 0.5,
            ..Default::default()
        };
        let mut c = Chaser::default();
        run(&mut c, &p, &steps, 3, 0.1);
        let v = run(&mut c, &p, &steps, 3, 0.75); // step 1 began at beat 0.5
        assert_eq!(levels(&v), [0.75, 1.0, 0.0]);
        assert!((v[1][1] - 0.5).abs() < 1e-6, "phase within the step");
        assert_eq!(v[1][2], 1.0, "step number");
    }

    #[test]
    fn directions() {
        let n = 4;
        let seq: Vec<usize> = (0..8).map(|c| pick(Direction::PingPong, c, n, 0, 0)).collect();
        assert_eq!(seq, [0, 1, 2, 3, 2, 1, 0, 1]);
        let back: Vec<usize> = (0..5).map(|c| pick(Direction::Backward, c, n, 0, 0)).collect();
        assert_eq!(back, [3, 2, 1, 0, 3]);
        let mut prev = 0;
        for c in 0..50 {
            let s = pick(Direction::Random, c, n, prev, 7);
            assert_ne!(s, prev, "random never repeats");
            assert!(s < n);
            prev = s;
        }
    }

    #[test]
    fn manual_buttons_blast_and_auto_reset() {
        let steps = vec![vec![0], vec![1], vec![2]];
        let p = Params {
            advance: Advance::Manual,
            auto_reset: 2.0,
            ..Default::default()
        };
        let mut c = Chaser::default();
        let tick = |s: f64| Tick {
            seconds: s,
            beats: s * 2.0,
            fft: [0.0; 3],
        };
        let next = Presses {
            next: 1,
            ..Default::default()
        };
        assert_eq!(
            levels(&c.frame(&p, &steps, 3, &tick(0.0), Presses::default())),
            [0.0; 3]
        );
        assert_eq!(levels(&c.frame(&p, &steps, 3, &tick(0.1), next)), [1.0, 0.0, 0.0]);
        assert_eq!(levels(&c.frame(&p, &steps, 3, &tick(0.2), next)), [0.0, 1.0, 0.0]);
        let prev = Presses {
            previous: 2,
            ..Default::default()
        };
        assert_eq!(
            levels(&c.frame(&p, &steps, 3, &tick(0.3), prev)),
            [0.0, 0.0, 1.0],
            "wraps back"
        );
        // No press for 2 s: dark again.
        assert_eq!(
            levels(&c.frame(&p, &steps, 3, &tick(2.5), Presses::default())),
            [0.0; 3]
        );
        // Blast: everything flashes and fades over one Step Time (0.5 s).
        let blast = Presses {
            blast: true,
            ..Default::default()
        };
        assert_eq!(levels(&c.frame(&p, &steps, 3, &tick(3.0), blast)), [1.0; 3]);
        assert_eq!(
            levels(&c.frame(&p, &steps, 3, &tick(3.25), Presses::default())),
            [0.5; 3]
        );
        // Reset: dark at once.
        let reset = Presses {
            reset: true,
            ..Default::default()
        };
        assert_eq!(levels(&c.frame(&p, &steps, 3, &tick(3.3), reset)), [0.0; 3]);
    }

    #[test]
    fn fft_onsets_step() {
        let steps = vec![vec![0], vec![1]];
        let p = Params {
            advance: Advance::Fft,
            ..Default::default()
        };
        let mut c = Chaser::default();
        let at = |c: &mut Chaser, s: f64, lo: f64| {
            levels(&c.frame(
                &p,
                &steps,
                2,
                &Tick {
                    seconds: s,
                    beats: 0.0,
                    fft: [lo, 0.0, 0.0],
                },
                Presses::default(),
            ))
        };
        assert_eq!(at(&mut c, 0.0, 0.0), [1.0, 0.0]);
        assert_eq!(at(&mut c, 0.1, 0.9), [0.0, 1.0], "a hit steps");
        assert_eq!(at(&mut c, 0.2, 0.7), [0.0, 1.0], "still high: no step");
        assert_eq!(at(&mut c, 0.3, 0.1), [0.0, 1.0]);
        assert_eq!(at(&mut c, 0.4, 0.6), [1.0, 0.0], "next hit");
    }

    #[test]
    fn random_pattern_and_invert() {
        let steps = vec![vec![0, 1, 2, 3, 4, 5]];
        let p = Params {
            random: true,
            count: 2,
            invert: true,
            ..Default::default()
        };
        let mut c = Chaser::default();
        let mut last: Vec<usize> = Vec::new();
        for b in 0..10 {
            let v = run(&mut c, &p, &steps, 6, b as f64 + 0.5);
            let dark: Vec<usize> = (0..6).filter(|&i| v[i][0] < 0.5).collect();
            assert_eq!(dark.len(), 2, "two lit (inverted: two dark)");
            assert!(dark.iter().all(|i| !last.contains(i)), "no slice twice in a row");
            last = dark;
        }
    }

    #[test]
    fn sync_choices_follow_sequences() {
        let mut comp = Composition::new("T", 1, 1);
        comp.add_effect(crate::effects::FxTarget::Composition, "SliceChaseSequencer", None)
            .unwrap();
        comp.chase.sequences.push(Sequence {
            name: "Ring".into(),
            ..Default::default()
        });
        comp.sync_chase(None);
        let choice = |c: &Composition| match &c.video.as_ref().unwrap().effects[0].params[SEQUENCE_KEY] {
            Param::Choice(ch) => (ch.options.clone(), ch.value.clone()),
            _ => unreachable!(),
        };
        assert_eq!(choice(&comp).0, ["All Slices", "Ring"]);
        if let Param::Choice(ch) = comp.video.as_mut().unwrap().effects[0]
            .params
            .get_mut(SEQUENCE_KEY)
            .unwrap()
        {
            ch.select(1);
        }
        comp.chase.sequences[0].name = "Circle".into();
        comp.sync_chase(Some(("Ring", "Circle")));
        assert_eq!(choice(&comp).1, "Circle", "follows the rename");
        comp.chase.sequences.clear();
        comp.sync_chase(None);
        assert_eq!(choice(&comp).1, ALL_SLICES, "deleted → All Slices");
    }

    #[test]
    fn relinking_names_is_no_undo_step() {
        let mut c = Composition::new("T", 1, 1);
        c.chase.sequences.push(Sequence {
            name: "A".into(),
            rule: None,
            steps: vec![vec![SliceRef {
                slice: 1,
                screen_name: "Wall".into(),
                slice_name: "S 1".into(),
            }]],
        });
        let k = crate::history::undo_key(&c);
        c.chase.sequences[0].steps[0][0].slice_name = "Renamed".into();
        c.chase.sequences[0].steps[0][0].screen_name = "Floor".into();
        assert_eq!(crate::history::undo_key(&c), k);
        c.chase.sequences[0].steps[0].clear();
        assert_ne!(crate::history::undo_key(&c), k, "taking a slice out is an edit");
    }

    #[test]
    fn buttons_pressed_by_address() {
        use crate::control::{Outcome, Selection, Value, apply, chain_addresses};
        let mut c = Composition::new("T", 1, 1);
        let t = crate::effects::FxTarget::Composition;
        c.add_effect(t, "SliceChaseSequencer", None).unwrap();
        let e = c.effects(t).unwrap().last().unwrap().clone();
        let Some(Param::Event(next)) = e.params.get("Next") else {
            panic!("Next is an event param")
        };
        let addr = chain_addresses(&c, t)[&next.id].clone();
        let before = c.clone();
        let out = apply(&mut c, &Selection::default(), &addr, &Value::Trigger);
        assert_ne!(out, Outcome::Unknown, "{addr}");
        assert_eq!(c, before, "a press changes nothing in the composition");
        let fired = crate::trigger::take();
        assert!(fired.contains(&next.id));
        assert_eq!(Presses::of(&e, &fired).next, 1);
    }

    #[test]
    fn saved_with_the_composition() {
        let mut comp = Composition::new("T", 1, 1);
        let j = serde_json::to_value(&comp).unwrap();
        assert!(j.get("chase").is_none(), "nothing written while empty");
        comp.chase.sequences.push(Sequence {
            name: "A".into(),
            rule: None,
            steps: vec![vec![SliceRef {
                slice: 3,
                screen_name: "Wall".into(),
                slice_name: "S 3".into(),
            }]],
        });
        let s = serde_json::to_string(&comp).unwrap();
        let back: Composition = serde_json::from_str(&s).unwrap();
        assert_eq!(back.chase, comp.chase);
    }
}
