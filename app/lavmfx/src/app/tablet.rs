//! Tablet extensions to the OSC map (`/lavmfx/…`), used by the
//! Open Stage Control layout (`remote/lavmfx-control.json`):
//!
//! - **Paged grid**: `/lavmfx/grid/row/R/…` = layer `R` of the window (1 =
//!   lowest shown), `/lavmfx/grid/row/R/clip/C/…` its clip in window column
//!   `C`, `/lavmfx/grid/col/C/…` = column `C`; `/lavmfx/grid/scroll dx dy`
//!   moves the window. The app keeps the offset and feeds back a window of
//!   [`ROWS`] × [`COLS`] (more than a tablet shows; extra cells are ignored).
//!   Launching a clip from the grid also selects it.
//! - **Selection**: `/lavmfx/select/layer dx`, `/lavmfx/select/clip dx`
//!   (next / previous filled clip on the selected layer).
//! - **Effects**: `/lavmfx/fx/{clip,layer,comp}/{ranges,toggles}` = JSON lists
//!   `[{"l": label, "a": address}]` of the selected clip's / layer's /
//!   composition's effect params; the tablet builds faders / toggles from them.

use std::collections::HashMap;

use lavmfx_core::control::{self, Value};
use lavmfx_core::drive::ensure_links;
use lavmfx_core::effects::FxTarget;
use lavmfx_core::{Id, Param};
use lavmfx_io::osc::OscIn;

use super::LavmfxApp;

/// Window fed back to the tablet.
pub const ROWS: usize = 8;
pub const COLS: usize = 16;

/// (label, address) pairs of one effect list.
pub type FxList = Vec<(String, String)>;

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
            }
            ["select", "layer"] if press => {
                let cur = self.selected_layer.or(self.focus_layer()).unwrap_or(0);
                self.selected_layer = Some(step(cur, arg(0), layers));
            }
            ["select", "clip"] if press => self.select_next_clip(arg(0)),
            ["grid", "row", r, "clip", c, rest @ ..] => {
                let (Some(r), Some(c)) = (idx(r), idx(c)) else { return };
                let (li, ci) = (loff + r - 1, coff + c - 1);
                let a = format!("/composition/layers/{}/clips/{}/{}", li + 1, ci + 1, rest.join("/"));
                self.control(&a, m.value.clone());
                if rest == ["connect"] && press && li < layers && ci < cols {
                    self.select_only((li, ci));
                }
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
    pub(super) fn tablet_feedback(
        &mut self,
        vals: &mut Vec<(String, Value)>,
        params: &mut Vec<(String, String)>,
        full: bool,
    ) {
        let (loff, coff) = self.remote.grid_off;
        let (layers, cols) = (self.comp.layers.len(), self.comp.columns.len());
        let deck = self.comp.active_deck();
        for r in 1..=ROWS {
            let li = loff + r - 1;
            let row = format!("/lavmfx/grid/row/{r}");
            let Some(l) = self.comp.layers.get(li) else {
                vals.push((format!("{row}/name"), Value::Str(String::new())));
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
        // Effect lists: rebuilt once a second; their params are fed back every pass.
        if full || self.remote.fx_lists.is_empty() {
            // Chains get their Transform like a panel would show them.
            if let Some((l, c)) = sel.clip {
                let _ = self.comp.effects_mut(FxTarget::Clip(l, c));
            }
            if let Some(l) = sel.layer {
                let _ = self.comp.effects_mut(FxTarget::Layer(l));
            }
            let mut lists = Vec::new();
            let clip = sel.clip.map(|(l, c)| FxTarget::Clip(l, c));
            let layer = sel.layer.map(FxTarget::Layer);
            for (scope, t) in [("clip", clip), ("layer", layer), ("comp", Some(FxTarget::Composition))] {
                let (ranges, toggles) = t
                    .map(|t| self.fx_lists(t, &control::chain_addresses(&self.comp, t)))
                    .unwrap_or_default();
                lists.push((scope, ranges, toggles));
            }
            self.remote.fx_lists = lists;
        }
        for (scope, ranges, toggles) in &self.remote.fx_lists {
            let json = |v: &Vec<(String, String)>| {
                serde_json::to_string(
                    &v.iter()
                        .map(|(l, a)| serde_json::json!({"l": l, "a": a}))
                        .collect::<Vec<_>>(),
                )
                .unwrap_or_default()
            };
            vals.push((format!("/lavmfx/fx/{scope}/ranges"), Value::Str(json(ranges))));
            vals.push((format!("/lavmfx/fx/{scope}/toggles"), Value::Str(json(toggles))));
            for (_, a) in ranges.iter().chain(toggles) {
                params.push((a.clone(), a.clone()));
            }
        }
    }

    /// (label, address) of an effect chain's sliders (range / choice /
    /// number params, effect opacity) and toggles (bool params, bypass).
    fn fx_lists(&self, t: FxTarget, map: &HashMap<Id, String>) -> (FxList, FxList) {
        let (mut ranges, mut toggles) = (Vec::new(), Vec::new());
        let Some(chain) = self.comp.effects(t) else {
            return (ranges, toggles);
        };
        for e in chain {
            let name = &e.display_name;
            if let Some(b) = &e.bypassed
                && let Some(a) = map.get(&b.id)
            {
                toggles.push((format!("{name} · Bypass"), a.clone()));
            }
            for (k, p) in e
                .params
                .iter()
                .chain(e.mixer.iter().filter(|(k, _)| k.as_str() == "Opacity"))
            {
                let (id, slider) = match p {
                    Param::Range(r) => (r.id, true),
                    Param::Choice(c) => (c.id, true),
                    Param::Int(i) => (i.id, true),
                    Param::Bool(b) => (b.id, false),
                    _ => continue,
                };
                let Some(a) = map.get(&id) else { continue };
                let item = (format!("{name} · {k}"), a.clone());
                if slider { ranges.push(item) } else { toggles.push(item) }
            }
        }
        (ranges, toggles)
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
