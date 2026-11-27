//! Bring choice params of older files in line with the current option lists
//! (files saved before M3c used provisional names).

use crate::defaults::*;
use crate::mix::BLEND_MODE_KEY;
use crate::model::*;
use crate::param::{ChoiceParam, Param, ParameterCollection};

/// Old option name → current one.
fn rename(old: &str) -> &str {
    match old {
        "Backward" => "<",
        "Pause" => "||",
        "Forward" => ">",
        "Smooth" => "Gentle",
        "Sharp" => "Fast",
        "Color Dodge" => "Dodge",
        "Color Burn" => "Burn",
        "Mask Layer Below" => "One Below",
        "Mask All Below" => "All Below",
        "Composition default" => "Composition Determined",
        "Layer default" => "Layer Determined",
        "Own layer" | "Layer" => "Own Layer",
        "Selected layer" | "Selected Layer" => "Active Layer",
        "Any Layer" => "Free Layer",
        o => o,
    }
}

fn fix(p: &mut ChoiceParam, options: &[&str], fallback: &str) {
    p.realign(options, rename, fallback);
}

fn fix_mixer(mixer: &mut ParameterCollection, options: &[&str], fallback: &str) {
    if let Some(Param::Choice(c)) = mixer.get_mut(BLEND_MODE_KEY) {
        fix(c, options, fallback);
    }
}

fn fix_clip(c: &mut Clip) {
    fix(&mut c.colorid, COLOR_IDS, "1");
    fix(&mut c.target, CLIP_TARGET_OPTIONS, "Composition Determined");
    fix(&mut c.triggerstyle, CLIP_TRIGGER_STYLES, "Composition Determined");
    fix(&mut c.ignorecolumntrigger, LAYER_DETERMINED, "Layer Determined");
    fix(&mut c.faderstart, LAYER_DETERMINED, "Layer Determined");
    fix(&mut c.beatsnap, CLIP_BEAT_SNAP, "Composition Determined");
    fix(&mut c.transporttype, TRANSPORT_TYPES, "Timeline");
    let controls = match &mut c.transport {
        Some(Transport::Timeline(t)) => Some((
            &mut t.controls.playdirection,
            &mut t.controls.playmode,
            &mut t.controls.playmodeaway,
        )),
        Some(Transport::BpmSync(t)) => Some((
            &mut t.controls.playdirection,
            &mut t.controls.playmode,
            &mut t.controls.playmodeaway,
        )),
        None => None,
    };
    if let Some((dir, mode, away)) = controls {
        fix(dir, PLAY_DIRECTIONS, ">");
        fix(mode, PLAY_MODES, "Loop");
        fix(away, PLAY_MODE_AWAY, "Restart");
    }
    if let Some(v) = &mut c.video {
        fix(&mut v.resize, CLIP_RESIZE, "Fit");
    }
}

impl Composition {
    /// Realign every known choice param with its option list.
    /// Idempotent; cheap when nothing needs changing.
    pub fn align_options(&mut self) {
        fix(&mut self.cliptarget, CLIP_TARGETS, "Own Layer");
        fix(&mut self.cliptriggerstyle, TRIGGER_STYLES, "Normal");
        fix(&mut self.clipbeatsnap, BEAT_SNAP, "None");
        let cf = &mut self.crossfader;
        fix(&mut cf.behaviour, CROSSFADER_BEHAVIOURS, "Cut");
        fix(&mut cf.curve, CROSSFADER_CURVES, "Linear");
        fix_mixer(&mut cf.mixer, MIX_MODES, "Alpha");
        for d in &mut self.decks {
            fix(&mut d.colorid, COLOR_IDS, "1");
            if let Some(s) = &mut d.stash {
                s.columns.iter_mut().for_each(|c| fix(&mut c.colorid, COLOR_IDS, "1"));
                s.clips.iter_mut().flatten().for_each(fix_clip);
            }
        }
        self.columns
            .iter_mut()
            .for_each(|c| fix(&mut c.colorid, COLOR_IDS, "1"));
        for l in &mut self.layers {
            fix(&mut l.colorid, COLOR_IDS, "1");
            fix(&mut l.crossfadergroup, CROSSFADER_GROUPS, "None");
            fix(&mut l.maskmode, MASK_MODES, "Disabled");
            fix(&mut l.transition.blend_mode, MIX_MODES, "Alpha");
            if let Some(v) = &mut l.video {
                fix(&mut v.autosize, AUTOSIZE, "Off");
                fix_mixer(&mut v.track.mixer, LAYER_BLEND_MODES, DEFAULT_LAYER_BLEND);
            }
            l.clips.iter_mut().for_each(fix_clip);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_names_are_mapped() {
        let mut c = Composition::new("t", 1, 1);
        let old = |v: &str, opts: &[&str]| {
            let mut p = ChoiceParam::new(0, opts, 0);
            p.select_value(v);
            p
        };
        c.crossfader.curve = old("Smooth", &["Linear", "Smooth", "Sharp"]);
        c.layers[0].maskmode = old("Mask All Below", &["None", "Mask Layer Below", "Mask All Below"]);
        c.layers[0].transition.blend_mode = old("Color Dodge", &["Alpha", "Color Dodge"]);
        c.align_options();
        assert_eq!(
            (c.crossfader.curve.value.as_str(), c.crossfader.curve.index),
            ("Gentle", 1)
        );
        assert_eq!(c.layers[0].maskmode.value, "All Below");
        assert_eq!(c.layers[0].transition.blend_mode.value, "Dodge");
        let fresh = Composition::new("t", 1, 1);
        let mut again = fresh.clone();
        again.align_options();
        assert_eq!(again, fresh, "new compositions are already aligned");
    }
}
