//! Autopilot: a layer plays its clips on its own. When the playing
//! clip has run its **Loops** (clip setting, default 1 pass), the layer moves
//! to the next / previous / a random filled clip. A clip's **Action** can
//! override the layer's direction (or jump to the first / last clip, or stay).

use crate::defaults::choice;
use crate::id::IdGen;
use crate::model::{AutoPilot, Clip, Layer};
use crate::param::{ChoiceParam, RangeParam};
use serde::{Deserialize, Serialize};

/// Layer autopilot options, in REST order.
pub const LAYER_MODES: &[&str] = &["Play Previous Clip", "Off", "Play Next Clip", "Play Random Clip"];
/// Clip autopilot actions ("Layer Determined" = follow the layer).
pub const CLIP_ACTIONS: &[&str] = &[
    "Layer Determined",
    "Play Next Clip",
    "Play Previous Clip",
    "Play Random Clip",
    "Play First Clip",
    "Play Last Clip",
    "Off",
];

/// Where autopilot goes after a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Next,
    Previous,
    Random,
    First,
    Last,
}

/// A clip's autopilot settings (not in the REST schema; `.avc`:
/// `AutoPilotLoopCount` "Loops", `AutoPilot` action).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipAutoPilot {
    pub action: ChoiceParam,
    /// Passes through the clip before autopilot moves on.
    pub loops: RangeParam,
}

impl ClipAutoPilot {
    pub fn new(ids: &mut IdGen) -> Self {
        Self {
            action: choice(ids, CLIP_ACTIONS, "Layer Determined"),
            loops: RangeParam::new(ids.alloc(), 0.1, 64.0, 1.0),
        }
    }
}

impl AutoPilot {
    pub fn new(ids: &mut IdGen) -> Self {
        Self {
            target: choice(ids, LAYER_MODES, "Off"),
        }
    }
}

fn step_of(name: &str) -> Option<Step> {
    Some(match name {
        "Play Next Clip" => Step::Next,
        "Play Previous Clip" => Step::Previous,
        "Play Random Clip" => Step::Random,
        "Play First Clip" => Step::First,
        "Play Last Clip" => Step::Last,
        _ => return None,
    })
}

impl Layer {
    /// The autopilot param, created (Off) if missing.
    pub fn autopilot_mut(&mut self, ids: &mut IdGen) -> &mut AutoPilot {
        self.autopilot.get_or_insert_with(|| AutoPilot::new(ids))
    }

    /// The layer's autopilot direction; `None` = off.
    pub fn autopilot_step(&self) -> Option<Step> {
        step_of(&self.autopilot.as_ref()?.target.value)
    }

    /// Set the layer's autopilot (`None` = off).
    pub fn set_autopilot(&mut self, ids: &mut IdGen, step: Option<Step>) {
        let v = match step {
            None => "Off",
            Some(Step::Next) => "Play Next Clip",
            Some(Step::Previous) => "Play Previous Clip",
            Some(_) => "Play Random Clip",
        };
        self.autopilot_mut(ids).target.select_value(v);
    }
}

impl Clip {
    /// The clip's autopilot settings, created if missing.
    pub fn autopilot_mut(&mut self, ids: &mut IdGen) -> &mut ClipAutoPilot {
        self.autopilot.get_or_insert_with(|| ClipAutoPilot::new(ids))
    }

    /// Passes before autopilot moves on (1 when unset).
    pub fn autopilot_loops(&self) -> f64 {
        self.autopilot.as_ref().map_or(1.0, |a| a.loops.value.max(0.01))
    }
}

/// What autopilot does after `clip` on `layer`: `None` = nothing (layer
/// autopilot off, or the clip's action is Off).
pub fn after(layer: &Layer, clip: &Clip) -> Option<Step> {
    let layer_step = layer.autopilot_step()?;
    match clip.autopilot.as_ref().map(|a| a.action.value.as_str()) {
        None | Some("Layer Determined") => Some(layer_step),
        Some(a) => step_of(a),
    }
}

/// The column to play after `cur` among `filled` columns (sorted);
/// `rnd` = any random number (Random never repeats the current clip when
/// there is another one).
pub fn pick(filled: &[usize], cur: usize, step: Step, rnd: u64) -> Option<usize> {
    if filled.is_empty() {
        return None;
    }
    let pos = filled.iter().position(|&c| c == cur);
    Some(match step {
        Step::First => filled[0],
        Step::Last => *filled.last()?,
        Step::Next => match pos {
            Some(i) => filled[(i + 1) % filled.len()],
            None => *filled.iter().find(|&&c| c > cur).unwrap_or(&filled[0]),
        },
        Step::Previous => match pos {
            Some(i) => filled[(i + filled.len() - 1) % filled.len()],
            None => *filled.iter().rev().find(|&&c| c < cur).unwrap_or(filled.last()?),
        },
        Step::Random => {
            let others: Vec<usize> = filled.iter().copied().filter(|&c| c != cur).collect();
            if others.is_empty() {
                filled[0]
            } else {
                others[(rnd % others.len() as u64) as usize]
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Composition;

    #[test]
    fn picks_wrap_and_skip_empty() {
        let f = [1, 4, 7];
        assert_eq!(pick(&f, 4, Step::Next, 0), Some(7));
        assert_eq!(pick(&f, 7, Step::Next, 0), Some(1));
        assert_eq!(pick(&f, 1, Step::Previous, 0), Some(7));
        assert_eq!(pick(&f, 5, Step::Next, 0), Some(7)); // current slot emptied
        assert_eq!(pick(&f, 5, Step::Previous, 0), Some(4));
        assert_eq!(
            (pick(&f, 4, Step::First, 0), pick(&f, 4, Step::Last, 0)),
            (Some(1), Some(7))
        );
        for r in 0..10 {
            assert_ne!(pick(&f, 4, Step::Random, r), Some(4));
        }
        assert_eq!(pick(&[3], 3, Step::Random, 5), Some(3));
        assert_eq!(pick(&[], 3, Step::Next, 0), None);
    }

    #[test]
    fn clip_action_overrides_layer() {
        let mut c = Composition::new("T", 1, 2);
        let ids = &mut c.ids;
        let l = &mut c.layers[0];
        assert_eq!(after(l, &l.clips[0]), None); // layer off
        l.set_autopilot(ids, Some(Step::Next));
        assert_eq!(l.autopilot_step(), Some(Step::Next));
        let mut clip = l.clips[0].clone();
        assert_eq!(after(l, &clip), Some(Step::Next));
        clip.autopilot_mut(ids).action.select_value("Play First Clip");
        assert_eq!(after(l, &clip), Some(Step::First));
        clip.autopilot_mut(ids).action.select_value("Off");
        assert_eq!(after(l, &clip), None);
        assert_eq!(clip.autopilot_loops(), 1.0);
    }
}
