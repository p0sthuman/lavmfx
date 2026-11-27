//! Audio effects in the Audio sections (clip, layer, group, composition):
//! one band per effect like the video effects (✕ · B · move, Dry Wet slider
//! in the band), its params below, and "+ Audio Effect" with the six built-in ones.

use eframe::egui::{self, RichText, vec2};
use lavmfx_core::audiofx::AUDIO_EFFECTS;
use lavmfx_core::{AudioEffect, AudioTrack, IdGen, Param};

use super::widgets::{
    Icon, RowOpts, Wheel, band_right, bool_row, icon_button, param_row, section_header_dim, toggle_box, wheel_slider,
};

/// What our 0..1 → sound mapping is (hover text).
fn hint(effect: &str, param: &str) -> &'static str {
    match (effect, param) {
        (_, "Dry Wet") => "Mix of the original (0) and the effect (1)",
        ("Bitcrusher", "Gain") => "Output level",
        ("Bitcrusher", "Sample Rate") => "1 = full rate … 0 = 200 Hz (held samples)",
        ("Bitcrusher", "Bit Depth") => "1 = 24 bits … 0 = 1 bit",
        ("Bitcrusher", "Dither") => "Adds a little noise before rounding (smoother at low bit depths)",
        ("Bitcrusher", "Jitter") => "Randomises the hold length",
        ("Distortion", "Pre Gain" | "Post Gain") => "0.1 = unchanged, 1 = ×10",
        ("Distortion", "Drive") => "0 = clean … 1 = hard saturation",
        ("Distortion", "Tone") => "1 = bright … 0 = dark (low-pass after the drive)",
        ("EQ-3", "Low") => "Below 250 Hz: 0 = off, 0.5 = 0 dB, 1 = +12 dB",
        ("EQ-3", "Mid") => "250 Hz – 2.5 kHz: 0 = off, 0.5 = 0 dB, 1 = +12 dB",
        ("EQ-3", "High") => "Above 2.5 kHz: 0 = off, 0.5 = 0 dB, 1 = +12 dB",
        ("Flanger", "Depth") => "How much of the swept delay is heard (0 = none)",
        ("Flanger", "Rate") => "Sweep speed, 0 … 10 Hz",
        ("Flanger", "Feedback") => "Resonance of the comb",
        ("Flanger", "Invert") => "Flips the delayed signal (hollow sound)",
        ("Flanger", "Width") => "Sweep range in seconds (default 0.0003; try 0.002–0.01)",
        (_, "Frequency") => "Cutoff, 20 Hz … 20 kHz",
        (_, "Resonance") => "Peak at the cutoff",
        _ => "",
    }
}

/// The effects of `t` plus the add menu; true when something changed.
pub fn audio_fx_rows(
    ui: &mut egui::Ui,
    t: &mut AudioTrack,
    ids: &mut IdGen,
    salt: impl std::hash::Hash + Copy + std::fmt::Debug,
) -> bool {
    let mut changed = false;
    let mut op: Option<(usize, i32)> = None; // 0 remove, −1 up, 1 down
    let n = t.effects.len();
    for (i, e) in t.effects.iter_mut().enumerate() {
        ui.push_id(("afx", salt, e.id), |ui| {
            let sec = section_header_dim(ui, ("afx", salt, e.id), &e.name, true, false, e.bypassed.value);
            {
                let ui = &mut band_right(ui, sec.header, 190.0);
                ui.spacing_mut().item_spacing.x = 1.0;
                let cell = vec2(24.0, sec.header.height() - 2.0);
                if icon_button(ui, Icon::Close, false, cell, true)
                    .on_hover_text("Remove effect")
                    .clicked()
                {
                    op = Some((i, 0));
                }
                changed |= toggle_box(ui, &mut e.bypassed.value, "B", "Bypass", cell).changed();
                ui.add_space(3.0);
                if i + 1 < n
                    && icon_button(ui, Icon::Down, false, vec2(18.0, cell.y), true)
                        .on_hover_text("Move down")
                        .clicked()
                {
                    op = Some((i, 1));
                }
                if i > 0
                    && icon_button(ui, Icon::Up, false, vec2(18.0, cell.y), true)
                        .on_hover_text("Move up")
                        .clicked()
                {
                    op = Some((i, -1));
                }
                if let Some(Param::Range(w)) = e.params.get_mut("Dry Wet") {
                    ui.spacing_mut().slider_width = (ui.available_width() - 4.0).max(30.0);
                    let mut v = w.value;
                    let r = wheel_slider(ui, &mut v, w.min..=w.max, Wheel::Linear, false).on_hover_text("Dry Wet");
                    if r.changed() {
                        w.set(v);
                        changed = true;
                    }
                }
            }
            if !sec.open {
                return;
            }
            let name = e.name.clone();
            for (pname, p) in e.params.iter_mut() {
                match p {
                    Param::Range(r) if pname != "Dry Wet" => {
                        changed |= param_row(
                            ui,
                            pname,
                            r,
                            RowOpts {
                                animate: true,
                                hover: hint(&name, pname),
                                ..Default::default()
                            },
                        )
                        .changed;
                    }
                    Param::Bool(b) => changed |= bool_row(ui, pname, &mut b.value, hint(&name, pname)),
                    _ => {}
                }
            }
        });
    }
    if let Some((i, d)) = op {
        match d {
            0 => {
                t.effects.remove(i);
            }
            -1 => t.effects.swap(i, i - 1),
            _ => t.effects.swap(i, i + 1),
        }
        changed = true;
    }
    ui.menu_button(RichText::new("+ Audio Effect").small(), |ui| {
        for s in &AUDIO_EFFECTS {
            if ui.button(s.name).clicked() {
                if let Some(e) = AudioEffect::new(ids, s.name) {
                    t.effects.push(e);
                    changed = true;
                }
                ui.close();
            }
        }
    })
    .response
    .on_hover_text("Add an audio effect (applied in list order, before this track's volume)");
    changed
}
