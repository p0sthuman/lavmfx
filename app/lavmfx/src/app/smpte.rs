//! SMPTE timecode in (M7 slice 4): the SMPTE 1 / SMPTE 2 inputs, fed by
//! LTC on an audio input channel or by MIDI Timecode; the SMPTE panel
//! (View › SMPTE) shows them with their delay compensation. Clips with
//! transport "SMPTE n" follow `SmpteState::now[n]` (see `tick_playback`).

use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, RichText};
use lavmfx_core::timecode::{SharedTimecode, TimecodeClock, TimecodeNow};
use lavmfx_core::transport::TransportSettings;

use super::LavmfxApp;
use super::prefs::{self, SmpteInput};
use super::theme;

#[derive(Default)]
pub struct SmpteState {
    /// MIDI Timecode from any MIDI input (written by the MIDI threads).
    pub mtc: Arc<SharedTimecode>,
    clocks: [TimecodeClock; 2],
    /// Sources the clocks were fed from (a change starts them over).
    sources: [SmpteInput; 2],
    /// This frame's timecode per input (None = nothing received yet).
    pub now: [Option<TimecodeNow>; 2],
}

/// Clip time of an SMPTE clip: its input's timecode − the clip's offset.
pub fn clip_time(now: &[Option<TimecodeNow>; 2], s: &TransportSettings) -> Option<f64> {
    let f = s.smpte?;
    now.get(f.input)?.map(|n| n.secs - f.offset)
}

impl LavmfxApp {
    /// Once per frame, before playback: read both inputs.
    pub(super) fn tick_smpte(&mut self) {
        let at = Instant::now();
        let a = &self.prefs.audio;
        for k in 0..2 {
            let src = a.smpte_inputs[k];
            let s = &mut self.smpte;
            if s.sources[k] != src {
                s.sources[k] = src;
                s.clocks[k].clear();
            }
            let reading = match src {
                SmpteInput::None => None,
                SmpteInput::Ltc(_) => self.audio.timecode(k),
                SmpteInput::Mtc => s.mtc.load(),
            };
            s.clocks[k].update(reading);
            s.now[k] = s.clocks[k].now(at, a.smpte_delay[k]);
        }
    }

    /// SMPTE panel: per input its timecode, source and delay compensation.
    pub(super) fn smpte_panel(&mut self, ui: &mut egui::Ui) {
        if self.prefs_win.audio.devices.is_none() {
            self.prefs_win.audio.devices = Some(lavmfx_audio::list());
        }
        let devs = self.prefs_win.audio.devices.clone().unwrap_or_default();
        let before = self.prefs.audio.clone();
        let pal = theme::cur();
        for k in 0..2 {
            let now = self.smpte.now[k];
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("SMPTE {}", k + 1)).strong());
                let (text, color) = match now {
                    Some(n) => (
                        n.timecode().display(n.fps),
                        if n.running { pal.accent } else { pal.text },
                    ),
                    None => ("--:--:--:--".to_owned(), pal.text_weak),
                };
                ui.label(RichText::new(text).monospace().size(20.0).color(color))
                    .on_hover_text("Current timecode of the incoming SMPTE signal");
                let state = match (self.prefs.audio.smpte_inputs[k], now) {
                    (SmpteInput::None, _) => "no input",
                    (_, None) => "waiting…",
                    (_, Some(n)) if n.running => n.fps.label(),
                    (_, Some(_)) => "stopped",
                };
                ui.label(RichText::new(state).small().weak());
            });
            ui.horizontal(|ui| {
                prefs::smpte_input_combo(ui, k, &devs, &mut self.prefs.audio);
                ui.label(RichText::new("Delay").weak()).on_hover_text(
                    "Delay compensation in frames: added to the incoming timecode to make up for delays in the signal",
                );
                ui.add(
                    egui::DragValue::new(&mut self.prefs.audio.smpte_delay[k])
                        .range(-100.0..=100.0)
                        .speed(0.1)
                        .fixed_decimals(1)
                        .suffix(" fr"),
                );
            });
            ui.add_space(6.0);
        }
        if self.prefs.audio != before {
            self.prefs_dirty = Some(Instant::now());
        }
        ui.label(
            RichText::new(format!(
                "LTC framerate {} (Preferences › Audio). Clips follow with transport SMPTE 1 / SMPTE 2.",
                self.prefs.audio.smpte_fps.label()
            ))
            .small()
            .weak(),
        );
    }
}
