//! Composition undo / redo by snapshots.
//!
//! Every edit path (panels, keyboard, OSC, MIDI, REST) changes the
//! composition directly, so instead of wrapping each one in a command the app
//! calls [`History::commit`] once an interaction has settled: if the
//! composition differs from the last committed state, that state becomes an
//! undo step. The comparison ignores what changes without an edit: animated
//! values, play positions, the active deck, tempo, ids, and the effect /
//! dashboard defaults panels create on first display.

use serde_json::Value as J;

use crate::drive::ensure_links;
use crate::effects::FxTarget;
use crate::model::Composition;

/// Steps kept.
pub const LIMIT: usize = 100;

#[derive(Default)]
pub struct History {
    undo: Vec<Composition>,
    redo: Vec<Composition>,
    /// Last committed state and its comparison key.
    base: Option<(Composition, u64)>,
}

impl History {
    /// Forget all steps; `c` is the new starting point (load / new).
    pub fn reset(&mut self, c: &Composition) {
        self.undo.clear();
        self.redo.clear();
        self.base = Some((c.clone(), undo_key(c)));
    }

    /// Record a step if `c` differs from the last committed state.
    pub fn commit(&mut self, c: &Composition) -> bool {
        self.commit_keyed(c.clone(), undo_key(c))
    }

    /// [`commit`](Self::commit) with the snapshot and its [`undo_key`] made
    /// elsewhere (the key takes ~0.2 s for a 10 000-slot composition: the app
    /// computes it on a worker thread).
    pub fn commit_keyed(&mut self, c: Composition, k: u64) -> bool {
        if self.base.as_ref().is_some_and(|(_, b)| *b == k) {
            return false;
        }
        let first = self.base.is_none();
        if let Some((b, _)) = self.base.replace((c, k)) {
            self.undo.push(b);
            if self.undo.len() > LIMIT {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        !first
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The state before the last step (call [`commit`](Self::commit) first).
    pub fn undo(&mut self, cur: &Composition) -> Option<Composition> {
        let prev = self.undo.pop()?;
        self.redo.push(cur.clone());
        self.base = Some((prev.clone(), undo_key(&prev)));
        Some(prev)
    }

    pub fn redo(&mut self, cur: &Composition) -> Option<Composition> {
        let next = self.redo.pop()?;
        self.undo.push(cur.clone());
        self.base = Some((next.clone(), undo_key(&next)));
        Some(next)
    }
}

/// What [`History`] compares: a hash of [`undo_json`] (a hash, not the JSON:
/// freeing a 10 000-slot composition's JSON on the UI thread took ~40 ms).
pub fn undo_key(c: &Composition) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    undo_json(c).to_string().hash(&mut h);
    h.finish()
}

/// The composition with lazily created defaults filled in on every deck, as
/// JSON without volatile fields.
pub fn undo_json(c: &Composition) -> J {
    let mut c = c.clone();
    for d in 0..c.decks.len() {
        c.select_deck(d);
        fill_defaults(&mut c);
    }
    c.select_deck(0);
    let mut v = serde_json::to_value(&c).unwrap_or_default();
    strip(&mut v);
    v
}

fn fill_defaults(c: &mut Composition) {
    let _ = c.effects_mut(FxTarget::Composition);
    for g in 0..c.layergroups.len() {
        let _ = c.effects_mut(FxTarget::Group(g));
    }
    for l in 0..c.layers.len() {
        let _ = c.effects_mut(FxTarget::Layer(l));
        for k in 0..c.layers[l].clips.len() {
            let _ = c.effects_mut(FxTarget::Clip(l, k));
        }
    }
    let ids = &mut c.ids;
    ensure_links(&mut c.dashboard, ids);
    for g in &mut c.layergroups {
        ensure_links(&mut g.dashboard, ids);
    }
    for l in &mut c.layers {
        ensure_links(&mut l.dashboard, ids);
        l.autopilot_mut(ids);
        for k in &mut l.clips {
            ensure_links(&mut k.dashboard, ids);
            if !k.is_empty() {
                k.autopilot_mut(ids); // created when the Clip panel shows it
            }
        }
    }
}

fn strip(v: &mut J) {
    match v {
        J::Object(m) => {
            // `screen_name` / `slice_name`: Slice Chase's cache for finding
            // slices again (relinking it isn't an edit). `collapsed`: an
            // effect folded in its panel (a view change, not an edit).
            for k in [
                "id",
                "ids",
                "selected",
                "tempo_controller",
                "screen_name",
                "slice_name",
                "collapsed",
            ] {
                m.remove(k);
            }
            let driven = m
                .get("drive")
                .and_then(|d| d.get("source"))
                .is_some_and(|s| s.as_str() != Some("Basic"));
            if driven {
                m.remove("value");
            }
            if let Some(t) = m.get_mut("transport").and_then(J::as_object_mut) {
                t.remove("position");
            }
            m.values_mut().for_each(strip);
        }
        J::Array(a) => a.iter_mut().for_each(strip),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::Source;

    #[test]
    fn steps_only_for_edits() {
        let mut c = Composition::new("T", 2, 3);
        let mut h = History::default();
        h.reset(&c);
        // Lazy defaults (panel shown), tempo, deck switch, animated value: no step.
        let _ = c.effects_mut(FxTarget::Layer(0));
        ensure_links(&mut c.layers[1].dashboard, &mut c.ids);
        c.tempo_controller.tempo.value = 130.0;
        c.layers[0].master.set_source(Source::Timeline);
        assert!(h.commit(&c), "starting an animation is an edit");
        c.layers[0].master.value = 0.3;
        assert!(!h.commit(&c), "the animated value isn't");
        c.add_deck("B");
        assert!(h.commit(&c));
        c.select_deck(1);
        assert!(!h.commit(&c), "switching decks isn't an edit");
        // Edit, undo, redo.
        c.layers[1].bypassed.value = true;
        assert!(h.commit(&c));
        let before = h.undo(&c).unwrap();
        assert!(!before.layers[1].bypassed.value);
        let after = h.redo(&before).unwrap();
        assert!(after.layers[1].bypassed.value);
        assert!(h.can_undo() && !h.can_redo());
    }

    #[test]
    fn effect_fold_is_saved_but_not_an_edit() {
        let mut c = Composition::new("T", 1, 1);
        c.add_effect(FxTarget::Layer(0), "Blur", None).unwrap();
        let mut h = History::default();
        h.reset(&c);
        let fx = |c: &mut Composition| c.effects_mut(FxTarget::Layer(0)).unwrap().last_mut().unwrap().collapsed;
        assert!(!fx(&mut c));
        c.effects_mut(FxTarget::Layer(0)).unwrap().last_mut().unwrap().collapsed = true;
        assert!(!h.commit(&c), "folding an effect isn't an undo step");
        // Saved and loaded with the composition.
        let mut back: Composition = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert!(fx(&mut back));
        // Open effects don't write the field (older files and REST stay as they were).
        let open = serde_json::to_string(&Composition::new("T", 1, 1)).unwrap();
        assert!(!open.contains("collapsed"));
    }
}
