//! Control addresses, shared by keyboard shortcuts, OSC, MIDI and REST.
//!
//! Everything is addressed with OSC-style paths: `/composition/layers/2/clips/3/connect`,
//! `/composition/selectedlayer/video/opacity`,
//! `/composition/layers/1/video/effects/transform/positionx`.
//!
//! Commands (connect a clip, launch a column, tap tempo…) come back as an
//! [`Action`] for the app to run. Anything else is looked up as a parameter in
//! the composition's REST-shaped JSON: each segment matches a key with case,
//! spaces and `_` ignored, numbers are 1-based list indices, effects match by
//! name, and the `params` / `effect` / `mixer` levels may be skipped (common
//! OSC paths leave some of them out). So every param is reachable without a
//! hand-written table.

use std::cell::Cell;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value as J};

use crate::model::Composition;

/// A value sent to an address.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Button press without a value (keyboard, REST trigger).
    Trigger,
    /// OSC float: **normalised 0..1** over a range param's min..max.
    Float(f64),
    /// OSC int: a choice index, or a value in the param's own units.
    Int(i64),
    Bool(bool),
    Str(String),
    /// Add this many units (keyboard tempo −/+, choice next/previous).
    Step(f64),
}

impl Value {
    /// Button semantics: press (true) or release (false).
    pub fn pressed(&self) -> bool {
        match self {
            Value::Trigger | Value::Step(_) => true,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f >= 0.5,
            Value::Str(_) => false,
        }
    }
}

/// What `selectedlayer` / `selectedclip` stand for (0-based).
#[derive(Debug, Clone, Copy, Default)]
pub struct Selection {
    pub layer: Option<usize>,
    /// (layer, column) in the active deck.
    pub clip: Option<(usize, usize)>,
}

/// Commands the app runs (indices 0-based).
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// `on` = false is the release of a press (piano trigger style).
    ConnectClip {
        layer: usize,
        column: usize,
        on: bool,
    },
    SelectClip {
        layer: usize,
        column: usize,
    },
    /// Remove the clip's content (REST `…/clips/{i}/clear`).
    ClearClip {
        layer: usize,
        column: usize,
    },
    ConnectColumn(usize),
    ConnectPrevColumn,
    ConnectNextColumn,
    ConnectPrevClip(usize),
    ConnectNextClip(usize),
    ClearLayer(usize),
    DisconnectAll,
    SelectLayer(usize),
    SelectDeck(usize),
    SelectPrevDeck,
    SelectNextDeck,
    TempoTap,
    Resync,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Action(Action),
    /// A parameter was written.
    Changed,
    /// Known address, nothing to do (a button release).
    Ignored,
    /// No such address here.
    Unknown,
}

/// Address segment as compared: lowercase letters and digits only.
pub fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Split an address into normalised segments with the selection substituted;
/// `None` when it isn't a `/composition/…` address or names an empty selection.
fn segments(address: &str, sel: &Selection) -> Option<Vec<String>> {
    let mut it = address.split('/').filter(|s| !s.is_empty()).map(norm);
    if it.next()? != "composition" {
        return None;
    }
    let mut out = Vec::new();
    for s in it {
        match s.as_str() {
            "selectedlayer" => {
                out.push("layers".into());
                out.push((sel.layer? + 1).to_string());
            }
            "selectedclip" => {
                let (l, c) = sel.clip?;
                out.extend([
                    "layers".into(),
                    (l + 1).to_string(),
                    "clips".into(),
                    (c + 1).to_string(),
                ]);
            }
            _ => out.push(s),
        }
    }
    Some(out)
}

fn index(s: &str) -> Option<usize> {
    s.parse::<usize>().ok().filter(|&n| n > 0).map(|n| n - 1)
}

fn action(segs: &[&str]) -> Option<Action> {
    use Action::*;
    Some(match segs {
        ["connectprevcolumn"] => ConnectPrevColumn,
        ["connectnextcolumn"] => ConnectNextColumn,
        ["disconnectall"] => DisconnectAll,
        ["selectprevdeck"] => SelectPrevDeck,
        ["selectnextdeck"] => SelectNextDeck,
        ["tempocontroller", "tempotap"] => TempoTap,
        ["tempocontroller", "resync"] => Resync,
        ["columns", n, "connect"] => ConnectColumn(index(n)?),
        ["decks", n, "select"] => SelectDeck(index(n)?),
        ["layers", n, "select"] => SelectLayer(index(n)?),
        ["layers", n, "clear"] => ClearLayer(index(n)?),
        ["layers", n, "connectprevclip"] => ConnectPrevClip(index(n)?),
        ["layers", n, "connectnextclip"] => ConnectNextClip(index(n)?),
        ["layers", l, "clips", c, "connect"] => ConnectClip {
            layer: index(l)?,
            column: index(c)?,
            on: true,
        },
        ["layers", l, "clips", c, "select"] => SelectClip {
            layer: index(l)?,
            column: index(c)?,
        },
        ["layers", l, "clips", c, "clear"] => ClearClip {
            layer: index(l)?,
            column: index(c)?,
        },
        _ => return None,
    })
}

/// Send `v` to `address`.
pub fn apply(comp: &mut Composition, sel: &Selection, address: &str, v: &Value) -> Outcome {
    let Some(segs) = segments(address, sel) else {
        return Outcome::Unknown;
    };
    let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
    if let Some(mut a) = action(&refs) {
        let pressed = v.pressed();
        if let Action::ConnectClip { on, .. } = &mut a {
            *on = pressed;
            return Outcome::Action(a);
        }
        return if pressed { Outcome::Action(a) } else { Outcome::Ignored };
    }
    let r = with_param(comp, &refs, true, |p| write(p, v));
    match r {
        Some(true) => Outcome::Changed,
        Some(false) => Outcome::Ignored,
        None => Outcome::Unknown,
    }
}

/// Current value of the param at `address`: ranges normalised 0..1 (as OSC
/// sends them), booleans, choice indices, strings.
pub fn read(comp: &mut Composition, sel: &Selection, address: &str) -> Option<Value> {
    let segs = segments(address, sel)?;
    let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
    with_param(comp, &refs, false, |p| (read_param(p), false))?
}

/// [`read`] for many addresses at once: each owner (composition, layer,
/// clip…) is serialised once. `None` where an address isn't a readable param.
pub fn read_many(comp: &mut Composition, sel: &Selection, addresses: &[String]) -> Vec<Option<Value>> {
    let mut cache: std::collections::HashMap<Vec<String>, Option<J>> = std::collections::HashMap::new();
    addresses
        .iter()
        .map(|a| {
            let segs = segments(a, sel)?;
            let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
            let n = owner_len(&refs);
            let key: Vec<String> = segs[..n].to_vec();
            let owner = cache
                .entry(key)
                .or_insert_with(|| owner_light_json(comp, &refs[..n]))
                .as_ref()?;
            let mut steps = Vec::new();
            if !locate(owner, &refs[n..], &mut steps) {
                return None;
            }
            let mut p = owner;
            for s in &steps {
                p = match s {
                    Step::Key(k) => p.get(k.as_str())?,
                    Step::Idx(i) => p.get(*i)?,
                };
            }
            read_param(&mut p.as_object()?.clone())
        })
        .collect()
}

/// Segments naming the owner object at the start of a param path.
fn owner_len(path: &[&str]) -> usize {
    match path {
        ["layers", _, "clips", _, ..] => 4,
        ["layers" | "groups" | "layergroups" | "columns" | "decks", _, ..] => 2,
        _ => 0,
    }
}

/// The owner's JSON without its child lists (as [`with_param`] sees it).
fn owner_light_json(comp: &mut Composition, owner: &[&str]) -> Option<J> {
    match owner {
        ["layers", l, "clips", c] => serde_json::to_value(comp.layers.get(index(l)?)?.clips.get(index(c)?)?).ok(),
        ["layers", l] => {
            let layer = comp.layers.get_mut(index(l)?)?;
            let clips = std::mem::take(&mut layer.clips);
            let v = serde_json::to_value(&*layer).ok();
            layer.clips = clips;
            v
        }
        ["groups" | "layergroups", g] => serde_json::to_value(comp.layergroups.get(index(g)?)?).ok(),
        ["columns", c] => serde_json::to_value(comp.columns.get(index(c)?)?).ok(),
        ["decks", d] => serde_json::to_value(comp.decks.get(index(d)?)?).ok(),
        [] => {
            let decks = std::mem::take(&mut comp.decks);
            let layers = std::mem::take(&mut comp.layers);
            let columns = std::mem::take(&mut comp.columns);
            let groups = std::mem::take(&mut comp.layergroups);
            let v = serde_json::to_value(&*comp).ok();
            comp.decks = decks;
            comp.layers = layers;
            comp.columns = columns;
            comp.layergroups = groups;
            v
        }
        _ => None,
    }
}

/// Whether `address` names a parameter (not a command).
pub fn is_param(comp: &mut Composition, sel: &Selection, address: &str) -> bool {
    let Some(segs) = segments(address, sel) else {
        return false;
    };
    let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
    with_param(comp, &refs, false, |_| ((), false)).is_some()
}

// ------------------------------------------------------------------ REST

/// An object addressed by a REST path (`/composition`, `…/layers/2`,
/// `…/layers/2/clips/3`, `…/columns/1`, `…/decks/1`, `…/layergroups/1`;
/// `selectedlayer` / `selectedclip` already substituted, see [`rest_path`]).
fn owner_segments(address: &str, sel: &Selection) -> Option<Vec<String>> {
    segments(address, sel)
}

/// JSON of the object at a REST path, as GET returns it.
pub fn owner_json(comp: &Composition, sel: &Selection, address: &str) -> Option<J> {
    let segs = owner_segments(address, sel)?;
    let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
    let mut v = match refs.as_slice() {
        [] => serde_json::to_value(comp).ok()?,
        ["layers", l] => serde_json::to_value(comp.layers.get(index(l)?)?).ok()?,
        ["layers", l, "clips", c] => serde_json::to_value(comp.layers.get(index(l)?)?.clips.get(index(c)?)?).ok()?,
        ["columns", c] => serde_json::to_value(comp.columns.get(index(c)?)?).ok()?,
        ["decks", d] => serde_json::to_value(comp.decks.get(index(d)?)?).ok()?,
        ["layergroups" | "groups", g] => serde_json::to_value(comp.layergroups.get(index(g)?)?).ok()?,
        _ => return None,
    };
    if let Some(m) = v.as_object_mut() {
        m.remove("ids");
        m.remove("chase");
    }
    Some(v)
}

/// REST PUT: write the param values found in `patch` (partial JSON,
/// e.g. `{"video":{"opacity":{"value":0.5}}}`) into the object at `address`.
/// Only values change (`value`, or `index` of a choice); lists match by
/// position. Returns how many params changed, `None` for a bad path.
pub fn patch(comp: &mut Composition, sel: &Selection, address: &str, patch: &J) -> Option<usize> {
    let segs = owner_segments(address, sel)?;
    let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
    let mut n = 0;
    let f = |v: &mut J| {
        merge(v, patch, &mut n);
        n > 0
    };
    match refs.as_slice() {
        [] => owner_round_trip(comp, f),
        ["layers", l] => owner_round_trip(comp.layers.get_mut(index(l)?)?, f),
        ["layers", l, "clips", c] => owner_round_trip(comp.layers.get_mut(index(l)?)?.clips.get_mut(index(c)?)?, f),
        ["columns", c] => owner_round_trip(comp.columns.get_mut(index(c)?)?, f),
        ["decks", d] => owner_round_trip(comp.decks.get_mut(index(d)?)?, f),
        ["layergroups" | "groups", g] => owner_round_trip(comp.layergroups.get_mut(index(g)?)?, f),
        _ => return None,
    }?;
    Some(n)
}

fn owner_round_trip<T: Serialize + DeserializeOwned>(t: &mut T, f: impl FnOnce(&mut J) -> bool) -> Option<()> {
    ROUND_TRIP.with(|r| r.set(true));
    let json = serde_json::to_value(&*t);
    ROUND_TRIP.with(|r| r.set(false));
    let mut json = json.ok()?;
    if f(&mut json) {
        *t = serde_json::from_value(json).ok()?;
    }
    Some(())
}

fn merge(target: &mut J, patch: &J, n: &mut usize) {
    match (target, patch) {
        (J::Object(t), J::Object(p)) => {
            if is_param_json(&J::Object(t.clone())) {
                let v = match (p.get("value"), p.get("index")) {
                    (_, Some(i)) if i.is_i64() => Value::Int(i.as_i64().unwrap_or(0)),
                    (Some(J::Bool(b)), _) => Value::Bool(*b),
                    (Some(J::Number(x)), _) => Value::Str(x.to_string()),
                    (Some(J::String(s)), _) => Value::Str(s.clone()),
                    _ => return,
                };
                if write(t, &v).0 {
                    *n += 1;
                }
                return;
            }
            for (k, pv) in p {
                if let Some(tv) = t.get_mut(k) {
                    merge(tv, pv, n);
                }
            }
        }
        (J::Array(t), J::Array(p)) => {
            for (tv, pv) in t.iter_mut().zip(p) {
                merge(tv, pv, n);
            }
        }
        _ => {}
    }
}

/// REST path (after `/api/v1`) → control address: `layers/selected` →
/// `selectedlayer`, `clips/selected` → `selectedclip`, `…/by-id/{id}` → the
/// object's index in the active deck.
pub fn rest_path(comp: &Composition, path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.first() != Some(&"composition") {
        return None;
    }
    let mut out = vec!["composition".to_owned()];
    let mut i = 1;
    while i < parts.len() {
        match (parts[i], parts.get(i + 1).copied(), parts.get(i + 2).copied()) {
            ("layers", Some("selected"), _) => {
                out.push("selectedlayer".into());
                i += 2;
            }
            ("clips", Some("selected"), _) => {
                out.push("selectedclip".into());
                i += 2;
            }
            (kind @ ("layers" | "clips" | "columns" | "decks" | "layergroups"), Some("by-id"), Some(id)) => {
                let id: crate::Id = id.parse().ok()?;
                match kind {
                    "layers" => out.extend([
                        "layers".into(),
                        (comp.layers.iter().position(|l| l.id == id)? + 1).to_string(),
                    ]),
                    "clips" => {
                        let (l, c) = comp
                            .layers
                            .iter()
                            .enumerate()
                            .find_map(|(l, x)| x.clips.iter().position(|c| c.id == id).map(|c| (l, c)))?;
                        out.extend([
                            "layers".into(),
                            (l + 1).to_string(),
                            "clips".into(),
                            (c + 1).to_string(),
                        ]);
                    }
                    "columns" => out.extend([
                        "columns".into(),
                        (comp.columns.iter().position(|c| c.id == id)? + 1).to_string(),
                    ]),
                    "decks" => out.extend([
                        "decks".into(),
                        (comp.decks.iter().position(|d| d.id == id)? + 1).to_string(),
                    ]),
                    _ => out.extend([
                        "layergroups".into(),
                        (comp.layergroups.iter().position(|g| g.id == id)? + 1).to_string(),
                    ]),
                }
                i += 3;
            }
            (p, _, _) => {
                out.push(p.to_owned());
                i += 1;
            }
        }
    }
    Some(format!("/{}", out.join("/")))
}

// ------------------------------------------------------- id → address

/// Control address of every parameter in the composition (active deck),
/// by param id: `/composition/layers/2/video/effects/blur/params/blurriness`.
/// Effects go by name (`blur`), or by position when the name repeats.
pub fn param_addresses(comp: &Composition) -> std::collections::HashMap<crate::Id, String> {
    let mut out = std::collections::HashMap::new();
    let mut c = comp.clone();
    c.decks.clear();
    if let Ok(v) = serde_json::to_value(&c) {
        walk_ids(&v, "/composition".to_owned(), &mut out);
    }
    out
}

/// [`param_addresses`] of one effect chain only: cheap enough for every
/// second (the full map serialises the whole composition, ~100 ms for a
/// 10 000-slot `.avc`).
pub fn chain_addresses(
    comp: &Composition,
    t: crate::effects::FxTarget,
) -> std::collections::HashMap<crate::Id, String> {
    use crate::effects::FxTarget;
    let mut out = std::collections::HashMap::new();
    let prefix = match t {
        FxTarget::Composition => "/composition/video/effects".to_owned(),
        FxTarget::Group(g) => format!("/composition/layergroups/{}/video/effects", g + 1),
        FxTarget::Layer(l) => format!("/composition/layers/{}/video/effects", l + 1),
        FxTarget::Clip(l, c) => format!("/composition/layers/{}/clips/{}/video/effects", l + 1, c + 1),
    };
    if let Some(chain) = comp.effects(t)
        && let Ok(v) = serde_json::to_value(chain)
    {
        walk_ids(&v, prefix, &mut out);
    }
    out
}

fn walk_ids(v: &J, path: String, out: &mut std::collections::HashMap<crate::Id, String>) {
    match v {
        J::Object(m) => {
            if is_param_json(v) {
                if let Some(id) = m.get("id").and_then(J::as_u64) {
                    out.insert(id as crate::Id, path);
                }
                return;
            }
            for (k, x) in m {
                if k == "drive" || k == "view" {
                    continue;
                }
                walk_ids(x, format!("{path}/{}", norm(k)), out);
            }
        }
        J::Array(a) => {
            let names: Vec<Option<String>> = a.iter().map(|e| e.get("name").and_then(J::as_str).map(norm)).collect();
            for (i, e) in a.iter().enumerate() {
                let seg = match &names[i] {
                    Some(n) if names.iter().filter(|x| x.as_ref() == Some(n)).count() == 1 && index(n).is_none() => {
                        n.clone()
                    }
                    _ => (i + 1).to_string(),
                };
                walk_ids(e, format!("{path}/{seg}"), out);
            }
        }
        _ => {}
    }
}

// ----------------------------------------------------------- JSON round trip

thread_local! {
    static ROUND_TRIP: Cell<bool> = const { Cell::new(false) };
}

/// `skip_serializing_if` for runtime state (param drive phases) that isn't
/// saved but must survive a [`with_param`] round trip.
pub(crate) fn skip_runtime<T>(_: &T) -> bool {
    !ROUND_TRIP.with(Cell::get)
}

/// Serialise `t`, let `f` find the param at `path` and change it, and write
/// the JSON back into `t` when `f` says it changed something.
fn round_trip<T: Serialize + DeserializeOwned, R>(
    t: &mut T,
    path: &[&str],
    write_back: bool,
    f: impl FnOnce(&mut Map<String, J>) -> (R, bool),
) -> Option<R> {
    ROUND_TRIP.with(|r| r.set(true));
    let json = serde_json::to_value(&*t);
    ROUND_TRIP.with(|r| r.set(false));
    let mut json = json.ok()?;
    let mut steps = Vec::new();
    if !locate(&json, path, &mut steps) {
        return None;
    }
    let mut p = &mut json;
    for s in &steps {
        p = match s {
            Step::Key(k) => p.get_mut(k.as_str())?,
            Step::Idx(i) => p.get_mut(*i)?,
        };
    }
    let (r, changed) = f(p.as_object_mut()?);
    if changed && write_back {
        match serde_json::from_value(json) {
            Ok(back) => *t = back,
            Err(_) => return None,
        }
    }
    Some(r)
}

/// Find the object that owns `path` (composition, layer, clip, group, column,
/// deck) and round-trip only it, with its child lists moved out meanwhile.
fn with_param<R>(
    comp: &mut Composition,
    path: &[&str],
    write_back: bool,
    f: impl FnOnce(&mut Map<String, J>) -> (R, bool),
) -> Option<R> {
    match path {
        ["layers", l, "clips", c, rest @ ..] => {
            let clip = comp.layers.get_mut(index(l)?)?.clips.get_mut(index(c)?)?;
            round_trip(clip, rest, write_back, f)
        }
        ["layers", l, rest @ ..] => {
            let layer = comp.layers.get_mut(index(l)?)?;
            let clips = std::mem::take(&mut layer.clips);
            let r = round_trip(layer, rest, write_back, f);
            layer.clips = clips;
            r
        }
        ["groups" | "layergroups", g, rest @ ..] => {
            round_trip(comp.layergroups.get_mut(index(g)?)?, rest, write_back, f)
        }
        ["columns", c, rest @ ..] => round_trip(comp.columns.get_mut(index(c)?)?, rest, write_back, f),
        ["decks", d, rest @ ..] => round_trip(comp.decks.get_mut(index(d)?)?, rest, write_back, f),
        _ => {
            let decks = std::mem::take(&mut comp.decks);
            let layers = std::mem::take(&mut comp.layers);
            let columns = std::mem::take(&mut comp.columns);
            let groups = std::mem::take(&mut comp.layergroups);
            let r = round_trip(comp, path, write_back, f);
            comp.decks = decks;
            comp.layers = layers;
            comp.columns = columns;
            comp.layergroups = groups;
            r
        }
    }
}

enum Step {
    Key(String),
    Idx(usize),
}

/// Levels OSC paths may leave out.
const SKIPPABLE: &[&str] = &["params", "effect", "mixer", "sourceparams", "track", "controls"];

fn is_param_json(v: &J) -> bool {
    v.get("valuetype").is_some_and(J::is_string)
}

fn locate(v: &J, segs: &[&str], out: &mut Vec<Step>) -> bool {
    let Some((seg, rest)) = segs.split_first() else {
        return is_param_json(v);
    };
    match v {
        J::Object(m) => {
            if let Some(k) = m.keys().find(|k| norm(k) == *seg) {
                out.push(Step::Key(k.clone()));
                if locate(&m[k], rest, out) {
                    return true;
                }
                out.pop();
            }
            for k in SKIPPABLE {
                if let Some(child) = m.get(*k).filter(|c| c.is_object()) {
                    out.push(Step::Key((*k).to_owned()));
                    if locate(child, segs, out) {
                        return true;
                    }
                    out.pop();
                }
            }
            false
        }
        J::Array(a) => {
            let i = index(seg).or_else(|| {
                a.iter().position(|e| {
                    ["name", "display_name"]
                        .iter()
                        .any(|k| e.get(*k).and_then(J::as_str).is_some_and(|n| norm(n) == *seg))
                })
            });
            let Some(i) = i.filter(|&i| i < a.len()) else {
                return false;
            };
            out.push(Step::Idx(i));
            if locate(&a[i], rest, out) {
                return true;
            }
            out.pop();
            false
        }
        _ => false,
    }
}

fn num(p: &Map<String, J>, k: &str) -> Option<f64> {
    p.get(k).and_then(J::as_f64)
}

/// Write `v` into the param object; true when its value changed.
fn write(p: &mut Map<String, J>, v: &Value) -> (bool, bool) {
    let vt = p.get("valuetype").and_then(J::as_str).unwrap_or_default().to_owned();
    let before = p.get("value").cloned();
    match vt.as_str() {
        "ParamRange" | "ParamNumber" => {
            let cur = num(p, "value").unwrap_or(0.0);
            let (min, max) = (num(p, "min"), num(p, "max"));
            let (lo, hi) = (min.unwrap_or(f64::MIN), max.unwrap_or(f64::MAX));
            let x = match v {
                Value::Float(f) => match (min, max) {
                    (Some(a), Some(b)) => a + f.clamp(0.0, 1.0) * (b - a),
                    _ => *f,
                },
                Value::Int(i) => *i as f64,
                Value::Bool(b) => {
                    if *b {
                        hi
                    } else {
                        lo
                    }
                }
                Value::Step(d) => cur + d,
                Value::Trigger => {
                    if cur > (lo + hi) * 0.5 {
                        lo
                    } else {
                        hi
                    }
                }
                Value::Str(s) => match s.trim().parse() {
                    Ok(x) => x,
                    Err(_) => return (false, false),
                },
            };
            let x = x.clamp(lo, hi);
            let x = if vt == "ParamNumber" { x.round() } else { x };
            p.insert("value".into(), J::from(x));
        }
        "ParamBoolean" => {
            let cur = p.get("value").and_then(J::as_bool).unwrap_or(false);
            let b = match v {
                Value::Trigger | Value::Step(_) => !cur,
                Value::Str(s) => matches!(s.trim(), "1" | "true" | "on"),
                v => v.pressed(),
            };
            p.insert("value".into(), J::Bool(b));
        }
        "ParamChoice" => {
            let opts: Vec<String> = p
                .get("options")
                .and_then(J::as_array)
                .map(|a| a.iter().filter_map(|o| o.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            if opts.is_empty() {
                return (false, false);
            }
            let n = opts.len() as i64;
            let cur = p.get("index").and_then(J::as_i64).unwrap_or(0);
            let i = match v {
                Value::Int(i) => *i,
                Value::Float(f) => (f.clamp(0.0, 1.0) * (n - 1) as f64).round() as i64,
                Value::Trigger => (cur + 1) % n,
                Value::Step(d) => (cur + *d as i64).rem_euclid(n),
                Value::Bool(b) => *b as i64,
                Value::Str(s) => match opts.iter().position(|o| o.eq_ignore_ascii_case(s.trim())) {
                    Some(i) => i as i64,
                    None => return (false, false),
                },
            }
            .clamp(0, n - 1);
            p.insert("index".into(), J::from(i));
            p.insert("value".into(), J::from(opts[i as usize].clone()));
        }
        "ParamString" | "ParamText" | "ParamColor" => {
            let Value::Str(s) = v else { return (false, false) };
            p.insert("value".into(), J::from(s.clone()));
        }
        "ParamEvent" => {
            // A button: pressed fires it (nothing in the composition changes).
            if v.pressed()
                && let Some(id) = p.get("id").and_then(J::as_i64)
            {
                crate::trigger::fire(id);
            }
            return (false, false);
        }
        _ => return (false, false),
    }
    let changed = p.get("value") != before.as_ref();
    (changed, changed)
}

fn read_param(p: &mut Map<String, J>) -> Option<Value> {
    let vt = p.get("valuetype").and_then(J::as_str)?;
    Some(match vt {
        "ParamRange" => {
            let (v, a, b) = (num(p, "value")?, num(p, "min")?, num(p, "max")?);
            Value::Float(if b > a { (v - a) / (b - a) } else { 0.0 })
        }
        "ParamNumber" => Value::Int(num(p, "value")? as i64),
        "ParamBoolean" => Value::Bool(p.get("value")?.as_bool()?),
        "ParamChoice" => Value::Int(p.get("index")?.as_i64()?),
        "ParamString" | "ParamText" | "ParamColor" => Value::Str(p.get("value")?.as_str()?.to_owned()),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::Source;

    fn sel() -> Selection {
        Selection {
            layer: Some(1),
            clip: Some((0, 2)),
        }
    }

    #[test]
    fn chain_addresses_match_the_full_map() {
        use crate::effects::FxTarget;
        let mut c = Composition::new("T", 3, 4);
        let g = c.new_group(&[2], "G");
        let ids = &mut c.ids;
        assert!(c.layers[0].clips[1].load_source(ids, "Lines"));
        let targets = [
            FxTarget::Composition,
            FxTarget::Group(g),
            FxTarget::Layer(1),
            FxTarget::Clip(0, 1),
        ];
        for t in targets {
            c.add_effect(t, "Blur", None).unwrap();
            c.add_effect(t, "Blur", None).unwrap(); // repeated name → by position
            c.add_effect(t, "Invert RGB", None).unwrap();
        }
        let full = param_addresses(&c);
        for t in targets {
            let part = chain_addresses(&c, t);
            assert!(part.len() > 6, "{t:?}");
            for (id, a) in &part {
                assert_eq!(full.get(id), Some(a), "{t:?}");
            }
        }
    }

    #[test]
    fn commands() {
        let mut c = Composition::new("T", 3, 4);
        let s = sel();
        let a = |c: &mut Composition, p: &str, v: Value| apply(c, &s, p, &v);
        assert_eq!(
            a(&mut c, "/composition/columns/2/connect", Value::Int(1)),
            Outcome::Action(Action::ConnectColumn(1))
        );
        assert_eq!(
            a(&mut c, "/composition/columns/2/connect", Value::Int(0)),
            Outcome::Ignored
        );
        assert_eq!(
            a(&mut c, "/composition/selectedlayer/clips/3/connect", Value::Trigger),
            Outcome::Action(Action::ConnectClip {
                layer: 1,
                column: 2,
                on: true
            })
        );
        assert_eq!(
            a(&mut c, "/composition/tempocontroller/tempotap", Value::Trigger),
            Outcome::Action(Action::TempoTap)
        );
        assert_eq!(a(&mut c, "/composition/nonsense", Value::Trigger), Outcome::Unknown);
        assert_eq!(a(&mut c, "/other/thing", Value::Trigger), Outcome::Unknown);
    }

    #[test]
    fn params_by_path() {
        let mut c = Composition::new("T", 3, 4);
        let s = sel();
        let mut a = |p: &str, v: Value| apply(&mut c, &s, p, &v);
        assert_eq!(a("/composition/master", Value::Float(0.25)), Outcome::Changed);
        assert_eq!(a("/composition/layers/1/bypassed", Value::Trigger), Outcome::Changed);
        assert_eq!(a("/composition/selectedlayer/solo", Value::Int(1)), Outcome::Changed);
        assert_eq!(
            a("/composition/layers/3/video/opacity", Value::Float(0.5)),
            Outcome::Changed
        );
        assert_eq!(
            a("/composition/tempocontroller/tempo", Value::Step(1.0)),
            Outcome::Changed
        );
        assert_eq!(
            a("/composition/layers/1/video/blendmode", Value::Str("Multiply".into())),
            Outcome::Changed
        );
        assert_eq!(c.master.value, 0.25);
        assert!(c.layers[0].bypassed.value);
        assert!(c.layers[1].solo.value);
        assert_eq!(c.layers[2].video.as_ref().unwrap().track.opacity.value, 0.5);
        assert_eq!(c.tempo_controller.tempo.value, 121.0);
        assert_eq!(read(&mut c, &s, "/composition/master"), Some(Value::Float(0.25)));
        assert_eq!(
            read(&mut c, &s, "/composition/layers/1/bypassed"),
            Some(Value::Bool(true))
        );
    }

    #[test]
    fn effect_params_skip_levels() {
        let mut c = Composition::new("T", 1, 3);
        let s = sel();
        let info = crate::VideoFileInfo {
            path: "x.mov".into(),
            exists: true,
            duration: String::new(),
            duration_ms: 1000.0,
            framerate: crate::FrameRate { num: 30, denom: 1 },
            width: 2,
            height: 2,
        };
        let ids = &mut c.ids;
        c.layers[0].clips[2].load_video(ids, "x", info);
        let fx = &mut c.layers[0].clips[2].video.as_mut().unwrap().track.effects;
        crate::effects::ensure_transform(fx, &mut c.ids);
        let addr = "/composition/selectedclip/video/effects/transform/positionx";
        assert!(is_param(&mut c, &s, addr), "clip Transform's Position X");
        assert_eq!(apply(&mut c, &s, addr, &Value::Float(1.0)), Outcome::Changed);
        assert_eq!(read(&mut c, &s, addr), Some(Value::Float(1.0)));
    }

    #[test]
    fn rest_get_put_paths() {
        let mut c = Composition::new("T", 2, 3);
        let s = sel();
        let id = c.layers[1].clips[2].id;
        assert_eq!(
            rest_path(&c, &format!("/composition/clips/by-id/{id}/connect")).as_deref(),
            Some("/composition/layers/2/clips/3/connect")
        );
        assert_eq!(
            rest_path(&c, "/composition/layers/selected").as_deref(),
            Some("/composition/selectedlayer")
        );
        let j = owner_json(&c, &s, "/composition/layers/2").unwrap();
        assert!(j.get("master").is_some());
        assert!(owner_json(&c, &s, "/composition").unwrap().get("ids").is_none());
        let p = serde_json::json!({"master": {"value": 0.25}, "video": {"opacity": {"value": 0.5}}, "bypassed": {"value": true}});
        assert_eq!(patch(&mut c, &s, "/composition/layers/1", &p), Some(3));
        assert_eq!(c.layers[0].master.value, 0.25);
        assert!(c.layers[0].bypassed.value);
        let p = serde_json::json!({"tempo_controller": {"tempo": {"value": 128}}});
        assert_eq!(patch(&mut c, &s, "/composition", &p), Some(1));
        assert_eq!(c.tempo_controller.tempo.value, 128.0);
    }

    #[test]
    fn addresses_of_ids_resolve_back() {
        let mut c = Composition::new("T", 2, 2);
        crate::effects::ensure_transform(&mut c.layers[1].video.as_mut().unwrap().track.effects, &mut c.ids);
        let map = param_addresses(&c);
        let s = Selection::default();
        let opacity = c.layers[1].video.as_ref().unwrap().track.opacity.id;
        assert_eq!(map[&opacity], "/composition/layers/2/video/opacity");
        let fx = &c.layers[1].video.as_ref().unwrap().track.effects[0];
        let (_, px) = fx.params.iter().find(|(k, _)| k.as_str() == "Position X").unwrap();
        let position_x_id = match px {
            crate::Param::Range(r) => r.id,
            _ => panic!("range"),
        };
        let addr = map[&position_x_id].clone();
        assert!(addr.contains("/effects/transform/"), "{addr}");
        assert!(is_param(&mut c, &s, &addr), "{addr} resolves");
        for a in map.values() {
            assert!(is_param(&mut c, &s, a), "{a} resolves");
        }
    }

    #[test]
    fn chase_params_by_chain_address() {
        use crate::effects::FxTarget;
        let mut c = Composition::new("T", 2, 2);
        let t = FxTarget::Layer(1);
        c.add_effect(t, "Slice Chase Sequencer", None).unwrap();
        let s = Selection::default();
        let map = chain_addresses(&c, t);
        let id = |c: &Composition, k: &str| match c.effects(t).unwrap().last().unwrap().params.get(k) {
            Some(crate::Param::Choice(x)) => (x.id, x.index as f64),
            Some(crate::Param::Range(x)) => (x.id, x.value),
            other => panic!("{k}: {other:?}"),
        };
        for (k, v, want) in [
            ("Advance", Value::Int(1), 1.0),
            ("Step Length", Value::Int(2), 2.0),
            ("Direction", Value::Int(2), 2.0),
            ("Trail", Value::Float(0.5), 8.0),
            ("Attack", Value::Float(1.0), 1.0),
        ] {
            let addr = map[&id(&c, k).0].clone();
            assert!(!matches!(apply(&mut c, &s, &addr, &v), Outcome::Unknown), "{k}: {addr}");
            assert_eq!(id(&c, k).1, want, "{k}: {addr}");
        }
    }

    #[test]
    fn read_many_matches_read() {
        let mut c = Composition::new("T", 3, 3);
        let s = sel();
        c.layers[1].master.value = 0.3;
        let addrs: Vec<String> = [
            "/composition/master",
            "/composition/layers/2/master",
            "/composition/selectedlayer/video/opacity",
            "/composition/layers/9/master",
            "/composition/tempocontroller/tempo",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let many = read_many(&mut c, &s, &addrs);
        for (a, v) in addrs.iter().zip(&many) {
            assert_eq!(*v, read(&mut c, &s, a), "{a}");
        }
        assert_eq!(many[1], Some(Value::Float(0.3)));
        assert_eq!(many[3], None);
    }

    #[test]
    fn round_trip_keeps_drive_phase() {
        let mut c = Composition::new("T", 1, 1);
        c.layers[0].master.set_source(Source::Timeline);
        c.layers[0].master.drive.as_mut().unwrap().phase = 0.4;
        let s = sel();
        apply(&mut c, &s, "/composition/layers/1/bypassed", &Value::Trigger);
        assert_eq!(c.layers[0].master.drive.as_ref().unwrap().phase, 0.4);
        let saved = serde_json::to_string(&c).unwrap();
        assert!(!saved.contains("rt_phase"), "runtime state isn't saved");
    }
}
