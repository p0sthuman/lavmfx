//! Tablet extensions to the OSC map (`/lavmfx/…`), used by the
//! Open Stage Control layout (`remote/lavmfx-control.json`):
//!
//! - **Paged grid**: `/lavmfx/grid/row/R/…` = layer `R` of the window (1 =
//!   lowest shown), `/lavmfx/grid/row/R/clip/C/…` its clip in window column
//!   `C`, `/lavmfx/grid/col/C/…` = column `C`; `/lavmfx/grid/scroll dx dy`
//!   moves the window. The app keeps the offset and feeds back a window of
//!   [`ROWS`] × [`COLS`] (more than a tablet shows; extra cells are ignored).
//!   Launching a clip from the grid also selects it.
//!   `/lavmfx/grid/row/R/select` selects that layer (fed back as `…/selected`).
//!   The window follows the app's clip grid when that scrolls, and a tablet
//!   scroll scrolls the app's grid to the same place.
//! - **Groups**: `/lavmfx/group/G/{name,selected,master,bypassed,solo,select}`
//!   = layer group `G` (up to [`GROUPS`]).
//! - **Selection**: `/lavmfx/select/layer dx`, `/lavmfx/select/clip dx`
//!   (next / previous filled clip on the selected layer), `/lavmfx/select/group dx`.
//! - **Group dashboard**: `/lavmfx/dash/group/linkN` = link N of the selected
//!   group (else the first one).
//! - **Chase**: `/lavmfx/chase/…` = one Slice Chase effect anywhere in the
//!   composition (comp, groups, layers, clips of the deck on view), picked with
//!   `/lavmfx/chase/pick dx` (name fed back as `/lavmfx/chase/name`). Keys:
//!   see [`CHASE_KEYS`]; `…/KEY/set i` picks choice `i`, `…/KEY/step dx` steps it.
//!
//! Effect params are reached through the dashboards (8 links per
//! composition / group / layer / clip), not one fader each.

use lavmfx_core::Param;
use lavmfx_core::control::{self, Value};
use lavmfx_core::drive::ensure_links;
use lavmfx_core::effects::FxTarget;
use lavmfx_io::osc::OscIn;

use super::LavmfxApp;

/// Window fed back to the tablet.
pub const ROWS: usize = 8;
pub const COLS: usize = 16;

/// Layer groups fed back (the mixer's group strips).
pub const GROUPS: usize = 4;

/// Per-group params the mixer's group strips show.
const GROUP_PARAMS: [&str; 3] = ["master", "bypassed", "solo"];

/// Chase tab keys and the chase effects' params they stand for
/// (`bypassed` = the effect's bypass).
const CHASE_KEYS: [(&str, &str); 14] = [
    ("sequence", "Sequence"),
    ("pattern", "Pattern"),
    ("advance", "Advance"),
    ("steplength", "Step Length"),
    ("steptime", "Step Time"),
    ("direction", "Direction"),
    ("trail", "Trail"),
    ("attack", "Attack"),
    ("opacity", "Opacity"),
    ("bypassed", "bypassed"),
    ("next", "Next"),
    ("previous", "Previous"),
    ("blast", "Blast"),
    ("reset", "Reset"),
];

fn param_id(p: &Param) -> Option<lavmfx_core::Id> {
    Some(match p {
        Param::Range(r) => r.id,
        Param::Choice(c) => c.id,
        Param::Bool(b) => b.id,
        Param::Int(i) => i.id,
        Param::Event(e) => e.id,
        _ => return None,
    })
}

/// Per-layer params the mixer / grid rows show.
const ROW_PARAMS: [&str; 5] = ["master", "video/opacity", "audio/volume", "bypassed", "solo"];

fn num(v: &Value) -> f64 {
    match v {
        Value::Int(i) => *i as f64,
        Value::Float(f) | Value::Step(f) => *f,
        Value::Bool(b) => *b as u8 as f64,
        _ => 0.0,
    }
}

fn step(cur: usize, d: f64, len: usize) -> usize {
    (cur as f64 + d).clamp(0.0, len.saturating_sub(1) as f64) as usize
}

impl LavmfxApp {
    /// An `/lavmfx/…` message from a controller.
    pub(super) fn tablet_osc(&mut self, m: &OscIn) {
        let path = m.address.strip_prefix("/lavmfx/").unwrap_or(&m.address);
        let parts: Vec<&str> = path.split('/').collect();
        // Buttons with pre-arguments: [dx, dy, press] / [dx, press].
        let press = m.args.last().is_none_or(Value::pressed);
        let arg = |i: usize| m.args.get(i).map_or(0.0, num);
        let (layers, cols) = (self.comp.layers.len(), self.comp.columns.len());
        let (loff, coff) = self.remote.grid_off;
        let idx = |s: &str| s.parse::<usize>().ok().filter(|&n| n > 0);
        match parts.as_slice() {
            ["grid", "scroll"] if press => {
                self.remote.grid_off = (step(loff, arg(1), layers), step(coff, arg(0), cols));
                self.remote.grid_pending = true;
            }
            ["select", "layer"] if press => {
                let cur = self.selected_layer.or(self.focus_layer()).unwrap_or(0);
                self.selected_layer = Some(step(cur, arg(0), layers));
            }
            ["select", "clip"] if press => self.select_next_clip(arg(0)),
            ["select", "group"] if press => {
                let n = self.comp.layergroups.len();
                if n > 0 {
                    self.selected_group = Some(step(self.selected_group.unwrap_or(0), arg(0), n));
                }
            }
            ["dash", "group", link] => {
                if let Some(g) = self.dash_group() {
                    let a = format!("/composition/layergroups/{}/dashboard/{link}", g + 1);
                    self.control(&a, m.value.clone());
                }
            }
            ["chase", "pick"] if press => {
                let n = self.chase_effects().len();
                if let Some(cur) = self.chase_index() {
                    self.remote.chase_pick = Some(step(cur, arg(0), n));
                }
            }
            ["chase", key, "set"] if press => {
                if let Some(a) = self.chase_address(key) {
                    self.control(&a, Value::Int(arg(0) as i64));
                }
            }
            ["chase", key, "step"] if press => {
                if let Some(a) = self.chase_address(key) {
                    self.control(&a, Value::Step(arg(0)));
                }
            }
            ["chase", key] => {
                if let Some(a) = self.chase_address(key) {
                    self.control(&a, m.value.clone());
                }
            }
            ["grid", "row", r, "clip", c, rest @ ..] => {
                let (Some(r), Some(c)) = (idx(r), idx(c)) else { return };
                let (li, ci) = (loff + r - 1, coff + c - 1);
                let a = format!("/composition/layers/{}/clips/{}/{}", li + 1, ci + 1, rest.join("/"));
                self.control(&a, m.value.clone());
                if rest == ["connect"] && press && li < layers && ci < cols {
                    self.select_only((li, ci));
                }
            }
            ["grid", "row", r, "select"] if press => {
                if let Some(li) = idx(r).map(|r| loff + r - 1).filter(|&li| li < layers) {
                    self.selected_layer = Some(li);
                    self.focus_tab = Some(super::dock::Tab::Layer);
                }
            }
            ["group", g, "select"] if press => {
                if let Some(gi) = idx(g).map(|g| g - 1).filter(|&gi| gi < self.comp.layergroups.len()) {
                    self.selected_group = Some(gi);
                    self.focus_tab = Some(super::dock::Tab::Group);
                }
            }
            ["group", g, rest @ ..] => {
                let Some(g) = idx(g) else { return };
                let a = format!("/composition/layergroups/{g}/{}", rest.join("/"));
                self.control(&a, m.value.clone());
            }
            ["grid", "row", r, rest @ ..] => {
                let Some(r) = idx(r) else { return };
                let a = format!("/composition/layers/{}/{}", loff + r, rest.join("/"));
                self.control(&a, m.value.clone());
            }
            ["grid", "col", c, rest @ ..] => {
                let Some(c) = idx(c) else { return };
                let a = format!("/composition/columns/{}/{}", coff + c, rest.join("/"));
                self.control(&a, m.value.clone());
            }
            _ => log::debug!("osc: unknown address {}", m.address),
        }
    }

    /// Select the next (`d` > 0) / previous filled clip on the selected layer.
    fn select_next_clip(&mut self, d: f64) {
        let Some(li) = self.selected_layer.or(self.focus_layer()) else {
            return;
        };
        let Some(layer) = self.comp.layers.get(li) else { return };
        let filled: Vec<usize> = (0..layer.clips.len()).filter(|&c| !layer.clips[c].is_empty()).collect();
        if filled.is_empty() {
            return;
        }
        let cur = self.selected_clip.filter(|(l, _)| *l == li).map(|(_, c)| c);
        let i = match cur.and_then(|c| filled.iter().position(|&f| f == c)) {
            None => 0,
            Some(i) if d > 0.0 => (i + 1) % filled.len(),
            Some(i) => (i + filled.len() - 1) % filled.len(),
        };
        self.select_only((li, filled[i]));
    }

    /// Feedback for the tablet: (address, value) pairs, plus param addresses
    /// to read (`params`). `full` = once a second (effect lists are rebuilt then).
    pub(super) fn tablet_feedback(&mut self, vals: &mut Vec<(String, Value)>, params: &mut Vec<(String, String)>) {
        let (loff, coff) = self.remote.grid_off;
        let (layers, cols) = (self.comp.layers.len(), self.comp.columns.len());
        let deck = self.comp.active_deck();
        for r in 1..=ROWS {
            let li = loff + r - 1;
            let row = format!("/lavmfx/grid/row/{r}");
            let Some(l) = self.comp.layers.get(li) else {
                vals.push((format!("{row}/name"), Value::Str(String::new())));
                vals.push((format!("{row}/selected"), Value::Int(0)));
                for p in ROW_PARAMS {
                    vals.push((format!("{row}/{p}"), Value::Float(0.0)));
                }
                for c in 1..=COLS {
                    vals.push((format!("{row}/clip/{c}/name"), Value::Str(String::new())));
                    vals.push((format!("{row}/clip/{c}/connected"), Value::Int(0)));
                }
                continue;
            };
            vals.push((format!("{row}/name"), Value::Str(l.name.value.clone())));
            let selected = self.selected_layer == Some(li);
            vals.push((format!("{row}/selected"), Value::Int(selected as i64)));
            for p in ROW_PARAMS {
                params.push((format!("{row}/{p}"), format!("/composition/layers/{}/{p}", li + 1)));
            }
            let playing = self
                .playing
                .get(li)
                .and_then(Option::as_ref)
                .filter(|p| p.slot.deck == deck)
                .map(|p| p.slot.col);
            for c in 1..=COLS {
                let ci = coff + c - 1;
                let (name, state) = match l.clips.get(ci) {
                    Some(k) if !k.is_empty() => (k.name.value.clone(), if playing == Some(ci) { 3 } else { 1 }),
                    _ => (String::new(), 0),
                };
                vals.push((format!("{row}/clip/{c}/name"), Value::Str(name)));
                vals.push((format!("{row}/clip/{c}/connected"), Value::Int(state)));
            }
        }
        for c in 1..=COLS {
            let name = self
                .comp
                .columns
                .get(coff + c - 1)
                .map(|k| k.name.value.clone())
                .unwrap_or_default();
            vals.push((format!("/lavmfx/grid/col/{c}/name"), Value::Str(name)));
        }
        // Layer groups (the mixer's group strips; no name = no strip).
        for g in 1..=GROUPS {
            let a = format!("/lavmfx/group/{g}");
            match self.comp.layergroups.get(g - 1) {
                Some(x) => {
                    vals.push((format!("{a}/name"), Value::Str(x.name.value.clone())));
                    for p in GROUP_PARAMS {
                        params.push((format!("{a}/{p}"), format!("/composition/layergroups/{g}/{p}")));
                    }
                }
                None => {
                    vals.push((format!("{a}/name"), Value::Str(String::new())));
                    for p in GROUP_PARAMS {
                        vals.push((format!("{a}/{p}"), Value::Float(0.0)));
                    }
                }
            }
            let selected = self.selected_group == Some(g - 1) && g <= self.comp.layergroups.len();
            vals.push((format!("{a}/selected"), Value::Int(selected as i64)));
        }
        vals.push((
            "/lavmfx/grid/pos".into(),
            Value::Str(format!("L{}+ · C{}+ of {layers}×{cols}", loff + 1, coff + 1)),
        ));
        // Selection + dashboards (links created like a panel would).
        let sel = self.control_selection();
        let ids = &mut self.comp.ids;
        ensure_links(&mut self.comp.dashboard, ids);
        if let Some(l) = sel.layer.and_then(|l| self.comp.layers.get_mut(l)) {
            ensure_links(&mut l.dashboard, ids);
        }
        if let Some(k) = sel
            .clip
            .and_then(|(l, c)| self.comp.layers.get_mut(l)?.clips.get_mut(c))
        {
            ensure_links(&mut k.dashboard, ids);
        }
        let layer_name = sel
            .layer
            .and_then(|l| self.comp.layers.get(l))
            .map(|l| l.name.value.clone());
        let clip_name = sel
            .clip
            .and_then(|(l, c)| self.comp.layers.get(l)?.clips.get(c))
            .filter(|k| !k.is_empty())
            .map(|k| k.name.value.clone());
        vals.push((
            "/lavmfx/selected/layer".into(),
            Value::Str(layer_name.unwrap_or_else(|| "—".into())),
        ));
        vals.push((
            "/lavmfx/selected/clip".into(),
            Value::Str(clip_name.unwrap_or_else(|| "—".into())),
        ));
        for owner in ["", "/selectedlayer", "/selectedclip"] {
            for i in 1..=8 {
                let a = format!("/composition{owner}/dashboard/link{i}");
                params.push((a.clone(), a));
            }
        }
        // Group dashboard.
        let group = self.dash_group();
        if let Some(g) = group.and_then(|g| self.comp.layergroups.get_mut(g)) {
            ensure_links(&mut g.dashboard, &mut self.comp.ids);
        }
        let group_name = group
            .and_then(|g| self.comp.layergroups.get(g))
            .map(|g| g.name.value.clone());
        vals.push((
            "/lavmfx/selected/group".into(),
            Value::Str(group_name.unwrap_or_else(|| "—".into())),
        ));
        for i in 1..=8 {
            let send = format!("/lavmfx/dash/group/link{i}");
            match group {
                Some(g) => params.push((send, format!("/composition/layergroups/{}/dashboard/link{i}", g + 1))),
                None => vals.push((send, Value::Float(0.0))),
            }
        }
        self.chase_feedback(vals, params);
    }

    /// Group the tablet's Group dashboard shows: the selected one, else the first.
    fn dash_group(&self) -> Option<usize> {
        let n = self.comp.layergroups.len();
        (n > 0).then(|| self.selected_group.unwrap_or(0).min(n - 1))
    }

    /// Slice Chase effects the Chase tab can pick: (chain, index, label).
    fn chase_effects(&self) -> Vec<(FxTarget, usize, String)> {
        // Clips: those of the deck on view (the layers hold them).
        let mut targets = vec![(FxTarget::Composition, "Composition".to_owned())];
        for (g, x) in self.comp.layergroups.iter().enumerate() {
            targets.push((FxTarget::Group(g), x.name.value.clone()));
        }
        for (l, x) in self.comp.layers.iter().enumerate() {
            targets.push((FxTarget::Layer(l), x.name.value.clone()));
        }
        for (l, x) in self.comp.layers.iter().enumerate() {
            for (c, k) in x.clips.iter().enumerate().filter(|(_, k)| !k.is_empty()) {
                targets.push((FxTarget::Clip(l, c), format!("{} › {}", x.name.value, k.name.value)));
            }
        }
        let mut out = Vec::new();
        for (t, owner) in targets {
            if let Some(chain) = self.comp.effects(t) {
                for (i, e) in chain.iter().enumerate() {
                    if lavmfx_core::chase::is_chase(&e.name) {
                        out.push((t, i, format!("{} · {owner}", e.display_name)));
                    }
                }
            }
        }
        out
    }

    /// Index of the picked chase effect: the tablet's pick (clamped), else
    /// the first one that isn't bypassed, else the first.
    fn chase_index(&self) -> Option<usize> {
        let list = self.chase_effects();
        if list.is_empty() {
            return None;
        }
        Some(match self.remote.chase_pick {
            Some(i) => i.min(list.len() - 1),
            None => list
                .iter()
                .position(|(t, i, _)| {
                    self.comp
                        .effects(*t)
                        .and_then(|c| c.get(*i))
                        .is_some_and(|e| !e.is_bypassed())
                })
                .unwrap_or(0),
        })
    }

    /// The picked chase effect.
    fn picked_chase(&self) -> Option<(FxTarget, usize, String)> {
        let i = self.chase_index()?;
        Some(self.chase_effects().swap_remove(i))
    }

    /// OSC address of chase key `key` on the picked effect.
    fn chase_address(&self, key: &str) -> Option<String> {
        let (_, param) = CHASE_KEYS.iter().find(|(k, _)| *k == key)?;
        let (t, i, _) = self.picked_chase()?;
        let e = self.comp.effects(t)?.get(i)?;
        let id = if *param == "bypassed" {
            e.bypassed.as_ref()?.id
        } else {
            param_id(e.params.get(*param).or_else(|| e.mixer.get(*param))?)?
        };
        control::chain_addresses(&self.comp, t).remove(&id)
    }

    /// Chase tab feedback: the picked effect's name, its params (values read
    /// from the model), the sequence name and readable step time / trail.
    fn chase_feedback(&self, vals: &mut Vec<(String, Value)>, params: &mut Vec<(String, String)>) {
        let picked = self.picked_chase();
        let effect = picked
            .as_ref()
            .and_then(|(t, i, _)| Some((*t, self.comp.effects(*t)?.get(*i)?)));
        let name = match (&picked, effect) {
            (Some(p), Some((_, e))) if e.is_bypassed() => format!("{} (bypassed)", p.2),
            (Some(p), _) => p.2.clone(),
            _ => "No Slice Chase effect: add one in the app".to_owned(),
        };
        vals.push(("/lavmfx/chase/name".into(), Value::Str(name)));
        let Some((t, e)) = effect else {
            for (k, _) in CHASE_KEYS {
                vals.push((format!("/lavmfx/chase/{k}"), Value::Int(-1)));
            }
            for k in ["sequence/name", "steptime/text", "trail/text", "attack/text"] {
                vals.push((format!("/lavmfx/chase/{k}"), Value::Str("—".into())));
            }
            return;
        };
        let map = control::chain_addresses(&self.comp, t);
        for (k, p) in CHASE_KEYS {
            let id = if p == "bypassed" {
                e.bypassed.as_ref().map(|b| b.id)
            } else {
                e.params.get(p).or_else(|| e.mixer.get(p)).and_then(param_id)
            };
            if let Some(a) = id.and_then(|id| map.get(&id)) {
                params.push((format!("/lavmfx/chase/{k}"), a.clone()));
            }
        }
        let text = |k: &str| match e.params.get(k) {
            Some(Param::Choice(c)) => c.value.clone(),
            Some(Param::Range(r)) => format!("{:.2}", r.value),
            _ => "—".into(),
        };
        vals.push(("/lavmfx/chase/sequence/name".into(), Value::Str(text("Sequence"))));
        vals.push((
            "/lavmfx/chase/steptime/text".into(),
            Value::Str(format!("{} s", text("Step Time"))),
        ));
        vals.push((
            "/lavmfx/chase/trail/text".into(),
            Value::Str(format!("{} steps", text("Trail"))),
        ));
        vals.push((
            "/lavmfx/chase/attack/text".into(),
            Value::Str(format!("{} step", text("Attack"))),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::step;

    #[test]
    fn window_steps_stay_inside() {
        assert_eq!(step(0, 8.0, 20), 8);
        assert_eq!(step(16, 8.0, 20), 19);
        assert_eq!(step(4, -8.0, 20), 0);
        assert_eq!(step(0, 1.0, 0), 0);
    }
}
