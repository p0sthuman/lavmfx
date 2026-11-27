//! The toolbar row (beat lamp, BPM, −/+, nudge, /2, ×2, TAP, RESYNC,
//! PAUSE … FFT GAIN + input meter) and the External FFT panel.

use std::time::Instant;

use eframe::egui::{self, RichText, vec2};
use lavmfx_core::fft::{self, Preset, Spectrum};

use super::{LavmfxApp, dock, prefs, theme, widgets};

/// Toolbar button height.
const H: f32 = 26.0;
/// FFT gain range (log knob).
const GAIN_MIN: f64 = 0.1;
const GAIN_MAX: f64 = 40.0;

fn gain_to_knob(g: f64) -> f64 {
    ((g.max(GAIN_MIN) / GAIN_MIN).ln() / (GAIN_MAX / GAIN_MIN).ln()).clamp(0.0, 1.0)
}
fn knob_to_gain(t: f64) -> f64 {
    GAIN_MIN * (GAIN_MAX / GAIN_MIN).powf(t)
}

/// Spectrum bars in `rect` (shared by the toolbar meter and the panel).
fn spectrum(ui: &egui::Ui, rect: egui::Rect, s: &Spectrum, highlight: Option<std::ops::Range<usize>>, thr: f64) {
    let pal = theme::cur();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, pal.field);
    let w = rect.width() / s.len() as f32;
    for (i, v) in s.iter().enumerate() {
        let h = v.clamp(0.0, 1.0) * (rect.height() - 2.0);
        let x = rect.left() + i as f32 * w;
        let col = match &highlight {
            Some(r) if !r.contains(&i) => pal.accent.gamma_multiply(0.45),
            // Below the global threshold: ignored (dim).
            _ if (*v as f64) < thr => pal.accent.gamma_multiply(0.35),
            _ => pal.accent,
        };
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x + 0.5, rect.bottom() - 1.0 - h),
                egui::pos2(x + w - 0.5, rect.bottom() - 1.0),
            ),
            0.0,
            col,
        );
    }
    if thr > 0.0 {
        // Dashed threshold line, as on the FFT controls' meter.
        let y = rect.bottom() - 1.0 - thr.clamp(0.0, 1.0) as f32 * (rect.height() - 2.0);
        let mut x = rect.left();
        while x < rect.right() {
            painter.line_segment(
                [egui::pos2(x, y), egui::pos2((x + 4.0).min(rect.right()), y)],
                egui::Stroke::new(1.0, pal.text),
            );
            x += 7.0;
        }
    }
}

fn btn(ui: &mut egui::Ui, text: &str, hover: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).strong()).min_size(vec2(36.0, H)))
        .on_hover_text(hover)
}

impl LavmfxApp {
    /// Fixed row under the menu bar (View › Toolbar).
    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let sz = vec2(H, H);
            if widgets::icon_button(ui, widgets::Icon::Undo, false, sz, self.can_undo())
                .on_hover_text("Undo (Ctrl+Z)")
                .clicked()
            {
                self.undo(false);
            }
            if widgets::icon_button(ui, widgets::Icon::Redo, false, sz, self.can_redo())
                .on_hover_text("Redo (Ctrl+Shift+Z)")
                .clicked()
            {
                self.undo(true);
            }
            ui.add_space(6.0);
            // Beat lamp: 4 squares (2×2), the current beat lit, going round
            // clockwise (1 top-left, 2 top-right, 3 bottom-right, 4 bottom-left).
            let (rect, _) = ui.allocate_exact_size(vec2(H, H), egui::Sense::hover());
            let pal = theme::cur();
            let (bar, beat) = self.clock.bar_beat();
            let pulse = if self.clock.paused() {
                0.0
            } else {
                (-(self.clock.beats().fract()) * 6.0).exp() as f32
            };
            let inner = rect.shrink(3.0);
            let half = inner.size() / 2.0;
            for i in 0..4 {
                let (col, row) = [(0, 0), (1, 0), (1, 1), (0, 1)][i];
                let min = inner.min + vec2(col as f32 * half.x, row as f32 * half.y);
                let cell = egui::Rect::from_min_size(min, half).shrink(1.0);
                let lit = !self.clock.paused() && i as i64 == beat - 1;
                let c = if lit {
                    // Full accent, a touch brighter at the beat's start.
                    pal.accent.lerp_to_gamma(pal.text_strong, 0.35 * pulse)
                } else {
                    pal.field
                };
                ui.painter().rect_filled(cell, 1.5, c);
            }
            ui.label(RichText::new(format!("{bar}.{beat}")).monospace())
                .on_hover_text("Bar . beat (4/4)");
            ui.add_space(6.0);

            let t = &mut self.comp.tempo_controller.tempo;
            let mut bpm = None;
            ui.label(RichText::new("BPM").strong());
            let mut v = t.value;
            if ui
                .add_sized(
                    [64.0, H],
                    egui::DragValue::new(&mut v)
                        .range(20.0..=500.0)
                        .speed(0.1)
                        .max_decimals(2),
                )
                .on_hover_text("Composition tempo (drag, or click to type)")
                .changed()
            {
                bpm = Some(v);
            }
            if btn(ui, "−", "Tempo −1 BPM").clicked() {
                bpm = Some(t.value.round() - 1.0);
            }
            if btn(ui, "+", "Tempo +1 BPM").clicked() {
                bpm = Some(t.value.round() + 1.0);
            }
            // Nudge: slower/faster while held (Preferences › Defaults › Tempo nudge range).
            let n = self.prefs.defaults.tempo_nudge;
            let back = btn(ui, "<", "Nudge back: slower while held");
            let fwd = btn(ui, ">", "Nudge forward: faster while held");
            let nudge = if back.is_pointer_button_down_on() {
                -n
            } else if fwd.is_pointer_button_down_on() {
                n
            } else {
                0.0
            };
            self.clock.set_nudge(nudge);
            if btn(ui, "/2", "Half tempo").clicked() {
                bpm = Some(t.value / 2.0);
            }
            if btn(ui, "×2", "Double tempo").clicked() {
                bpm = Some(t.value * 2.0);
            }
            // TAP flashes on every tap (click or Space).
            let flash = self
                .tap_flash
                .map_or(0.0, |t| (1.0 - t.elapsed().as_secs_f32() / 0.18).max(0.0));
            let fill = ui
                .visuals()
                .widgets
                .inactive
                .weak_bg_fill
                .lerp_to_gamma(pal.accent, flash);
            let text = ui.visuals().text_color().lerp_to_gamma(pal.on_accent, flash);
            let tap = ui
                .add(
                    egui::Button::new(RichText::new("TAP").strong().color(text))
                        .fill(fill)
                        .min_size(vec2(36.0, H)),
                )
                .on_hover_text("Tap in time (2+ taps set the BPM; each tap lands on a beat). Key: Space");
            super::shortcuts::learn_target(
                ui,
                tap.rect,
                super::shortcuts::LearnTarget::Address("/composition/tempocontroller/tempotap".into()),
            );
            if tap.clicked() {
                bpm = self.clock.tap();
                self.tap_flash = Some(Instant::now());
            }
            if let Some(b) = bpm {
                t.set(b);
                self.clock.set_bpm(t.value);
            }
            let rs = btn(ui, "RESYNC", "Restart the beat at bar 1");
            super::shortcuts::learn_target(
                ui,
                rs.rect,
                super::shortcuts::LearnTarget::Address("/composition/tempocontroller/resync".into()),
            );
            if rs.clicked() {
                self.resync();
            }
            let mut paused = self.clock.paused();
            widgets::toggle_box(
                ui,
                &mut paused,
                "PAUSE",
                "Stop the beat clock (BPM-synced clips and animations hold)",
                vec2(56.0, H),
            );
            if paused != self.clock.paused() {
                self.clock.set_paused(paused);
            }
            // Ableton Link: on/off + peers.
            let mut link = self.prefs.ableton_link;
            let peers = self.link_peers();
            let label = match peers {
                Some(n) => format!("LINK {n}"),
                None => "LINK".into(),
            };
            let hover = match peers {
                Some(1) => "Ableton Link on: 1 other app in the session".to_owned(),
                Some(n) => format!("Ableton Link on: {n} other apps in the session"),
                None => "Ableton Link: share tempo and beat with other apps on the network".to_owned(),
            };
            widgets::toggle_box(ui, &mut link, &label, &hover, vec2(60.0, H));
            if link != self.prefs.ableton_link {
                self.prefs.ableton_link = link;
                self.prefs_dirty = Some(Instant::now());
            }
            // Record: click to start, again to stop → clip in the deck.
            let mut rec = self.recording();
            let label = self.recording_label().map_or("REC".to_owned(), |l| l.replace("● ", ""));
            let hover = format!(
                "Record the composition ({}). Click again to stop: the recording is added to the deck. Ctrl+R",
                self.prefs.recording.preset
            );
            widgets::toggle_box(
                ui,
                &mut rec,
                &label,
                &hover,
                vec2(if self.recording() { 150.0 } else { 48.0 }, H),
            );
            if rec != self.recording() {
                self.toggle_recording();
            }

            // Right: FFT GAIN knob + input meter (click: External FFT panel).
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let (rect, r) = ui.allocate_exact_size(vec2(64.0, H), egui::Sense::click());
                spectrum(ui, rect, &self.audio.external(), None, self.prefs.audio.fft_threshold);
                if r.on_hover_text(format!("{} (click: External FFT panel)", self.audio.status()))
                    .clicked()
                {
                    if !dock::is_open(&self.dock, dock::Tab::ExternalFft) {
                        dock::toggle_tab(&mut self.dock, dock::Tab::ExternalFft);
                    }
                    dock::focus_tab(&mut self.dock, dock::Tab::ExternalFft);
                }
                let a = &mut self.prefs.audio;
                let mut k = gain_to_knob(a.fft_gain);
                let r = widgets::value_knob(ui, "", &mut k, (0.0, 1.0), gain_to_knob(1.0));
                let g = knob_to_gain(k);
                if (g - a.fft_gain).abs() > 1e-6 {
                    a.fft_gain = g;
                    self.prefs_dirty = Some(Instant::now());
                }
                r.on_hover_text(format!("Audio FFT input gain {:.2}×", a.fft_gain));
                ui.add(egui::Label::new(RichText::new("FFT GAIN").small().strong()).extend());
                if widgets::thresh_knob(ui, &mut a.fft_threshold, "").changed() {
                    self.prefs_dirty = Some(Instant::now());
                }
                ui.add(egui::Label::new(RichText::new("THRESH").small().strong()).extend());
                if self.audio_input_off() {
                    ui.label(RichText::new("no FFT input").small().weak());
                }
            });
        });
    }

    fn audio_input_off(&self) -> bool {
        self.prefs.audio.fft_input == 0
    }

    /// External FFT panel: input choice, gain, big spectrum, L/M/H levels.
    pub(super) fn external_fft_panel(&mut self, ui: &mut egui::Ui) {
        if self.prefs_win.audio.devices.is_none() {
            self.prefs_win.audio.devices = Some(lavmfx_audio::list());
        }
        let devs = self.prefs_win.audio.devices.clone().unwrap_or_default();
        let before = self.prefs.audio.clone();
        let a = &mut self.prefs.audio;
        egui::Grid::new("xfft-grid")
            .num_columns(2)
            .min_col_width(60.0)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Input").weak());
                ui.horizontal(|ui| {
                    prefs::device_combo(
                        ui,
                        "xfft-dev",
                        &devs.inputs,
                        devs.default_input.as_deref(),
                        &mut a.input_device,
                    );
                    if ui.small_button("⟳").on_hover_text("List devices again").clicked() {
                        self.prefs_win.audio.devices = None;
                        self.prefs_win.audio.retry = true;
                    }
                });
                ui.end_row();
                ui.label(RichText::new("Channel").weak());
                prefs::fft_input_combo(ui, "panel", &devs, a);
                ui.end_row();
                ui.label(RichText::new("Gain").weak());
                ui.add(
                    egui::Slider::new(&mut a.fft_gain, GAIN_MIN..=GAIN_MAX)
                        .logarithmic(true)
                        .custom_formatter(|v, _| format!("{v:.2}×")),
                );
                ui.end_row();
                ui.label(RichText::new("Thresh").weak());
                widgets::thresh_knob(ui, &mut a.fft_threshold, "");
                ui.end_row();
            });
        if self.prefs.audio != before {
            self.prefs_dirty = Some(Instant::now());
        }
        ui.label(RichText::new(self.audio.status()).small().weak());
        ui.add_space(4.0);

        // Big spectrum with the L/M/H zones and a frequency axis.
        let s = self.audio.external();
        let w = ui.available_width();
        let h = (ui.available_height() - 90.0).clamp(60.0, 220.0);
        let (rect, _) = ui.allocate_exact_size(vec2(w, h), egui::Sense::hover());
        spectrum(ui, rect, &s, None, self.prefs.audio.fft_threshold);
        let pal = theme::cur();
        let painter = ui.painter_at(rect.expand(1.0));
        for (p, name, _) in Preset::ALL {
            let (lo, hi) = p.range();
            let x = rect.left() + lo as f32 * rect.width();
            if lo > 0.0 {
                painter.line_segment(
                    [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                    (1.0, pal.text_weak.gamma_multiply(0.5)),
                );
            }
            let cx = rect.left() + ((lo + hi) / 2.0) as f32 * rect.width();
            painter.text(
                egui::pos2(cx, rect.top() + 3.0),
                egui::Align2::CENTER_TOP,
                name,
                egui::FontId::proportional(11.0),
                pal.text_weak,
            );
        }
        let (axis, _) = ui.allocate_exact_size(vec2(w, 14.0), egui::Sense::hover());
        for (f, label) in [
            (50.0, "50"),
            (100.0, "100"),
            (200.0, "200"),
            (500.0, "500"),
            (1000.0, "1k"),
            (2000.0, "2k"),
            (5000.0, "5k"),
            (10_000.0, "10k"),
        ] {
            let x = axis.left() + fft::pos_of(f) as f32 * axis.width();
            ui.painter().text(
                egui::pos2(x, axis.center().y),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(10.0),
                pal.text_weak,
            );
        }
        ui.add_space(4.0);
        // L / M / H level bars (what an FFT drive with that preset reads, gain 1).
        for (p, name, hover) in Preset::ALL {
            let mut b = fft::FftBand::default();
            b.set_preset(p);
            let v = b.level(&s) as f32;
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).strong()).on_hover_text(hover);
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width() - 44.0, 12.0), egui::Sense::hover());
                ui.painter().rect_filled(r, 2.0, pal.field);
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(r.min, vec2(r.width() * v, r.height())),
                    2.0,
                    pal.accent,
                );
                ui.label(RichText::new(format!("{:.0} %", v * 100.0)).small().monospace());
            });
        }
        ui.ctx().request_repaint();
    }
}
