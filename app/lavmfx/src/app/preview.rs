//! Preview monitor: a clip played on its own, outside the composition
//! (thumbnail click), so it can be checked before triggering it live.
//! Uses its own small compositor and texture; follows the clip's transport
//! params like a live clip.

use eframe::egui::{self, Color32, RichText, pos2, vec2};
use lavmfx_render::{FrameInput, LayerDraw};

use super::{LavmfxApp, file_name};
use crate::playback::{Opening, Playback, Slot, Tick};
use lavmfx_core::transport::TransportSettings;

/// Preview render size (16:9; the monitor panel is small).
pub(super) const PREVIEW_SIZE: (u32, u32) = (960, 540);

impl LavmfxApp {
    /// Start previewing the clip in slot (layer, column) of the active deck.
    pub(super) fn preview_clip(&mut self, li: usize, ci: usize) {
        let deck = self.comp.active_deck();
        let Some(clip) = self.comp.clip(deck, li, ci) else {
            return;
        };
        let slot = Slot {
            deck,
            layer: li,
            col: ci,
            clip: clip.id,
        };
        if self.preview.as_ref().is_some_and(|p| p.slot.clip == slot.clip) {
            return; // already previewing it
        }
        if clip.source().is_some() {
            // Source / effect clip: nothing to open.
            self.preview_opening = None;
            self.preview_comp.clear_video();
            let mut pb = Playback::without_file(slot, lavmfx_core::sources::VIRTUAL_SECONDS, 0.0);
            pb.live_src = super::clip_live_source(&self.comp, slot);
            if let Some(e) = pb.connect_live() {
                self.status = format!("Preview: {e}");
            }
            self.preview = Some(pb);
            return;
        }
        let Some(path) = clip.video_path() else { return };
        if clip.is_missing() {
            self.status = format!("Missing file: {path}");
            return;
        }
        self.preview_opening = Some(Opening::spawn(path.into(), slot, 0.0, self.native_bc));
    }

    pub(super) fn stop_preview(&mut self) {
        self.preview = None;
        self.preview_opening = None;
        self.preview_comp.clear_video();
    }

    /// Open / advance the preview clip and render the preview texture.
    pub(super) fn tick_preview(&mut self, dt: f64) {
        if let Some(r) = self.preview_opening.as_ref().and_then(Opening::poll) {
            let op = self.preview_opening.take().unwrap();
            match r {
                Ok(player) => {
                    self.preview_comp.clear_video();
                    self.preview = Some(Playback::new(player, op.slot, op.start));
                }
                Err(e) => self.status = format!("Can't preview {}: {e}", file_name(&op.path)),
            }
        }
        let bpm = self.clock.bpm;
        let Some(pb) = &mut self.preview else { return };
        pb.connect_live();
        let s = pb.slot;
        let ended = match self.comp.clip(s.deck, s.layer, s.col) {
            Some(c) if c.id == s.clip => {
                pb.settings = TransportSettings::for_clip(c, bpm);
                pb.settings.speed *= self.comp.layer_speed(s.layer);
                pb.smpte_time = super::smpte::clip_time(&self.smpte.now, &pb.settings);
                match pb.tick(dt) {
                    Tick::Frame(f) => {
                        self.preview_comp
                            .set_layer_frame(&self.gpu.device, &self.gpu.queue, 0, &f);
                        false
                    }
                    Tick::Nothing => false,
                    Tick::Ended => true,
                }
            }
            _ => true, // ejected or moved away
        };
        if ended {
            self.stop_preview();
            return;
        }
        // Preview shows the clip as it would look on its layer: clip → layer
        // chain on the clip, then the layer's group chain and the composition
        // chain (on the preview's own composition).
        use super::effects_ui::draws;
        use lavmfx_core::effects::FxTarget;
        let comp = &self.comp;
        let clip_fx = super::effects_ui::clip_draws(comp.clip(s.deck, s.layer, s.col));
        let layer_fx = draws(comp.effects(FxTarget::Layer(s.layer)));
        let group_fx = comp
            .group_of(s.layer)
            .map_or_else(Vec::new, |g| draws(comp.effects(FxTarget::Group(g))));
        let mut comp_fx = draws(comp.effects(FxTarget::Composition));
        // An effect being tried from the Effects browser goes on top.
        if self.preview_fx_on
            && let Some((_, fx)) = &self.preview_fx
        {
            comp_fx.extend(draws(Some(&vec![fx.clone()])));
        }
        let mut f = lavmfx_render::FxFrame {
            layer: vec![[clip_fx, layer_fx].concat()],
            composition: [group_fx, comp_fx].concat(),
            ..Default::default()
        };
        f.images = super::text::images(&f);
        f.fft = self.isf_fft();
        self.isf.prepare(&self.gpu.device, &self.gpu.queue, &f);
        self.preview_comp.set_effects(f);
        let input = self.clip_input(Some(s));
        if input != lavmfx_render::LayerInput::Frame {
            self.preview_comp.ensure_layer(&self.gpu.device, 0);
        }
        // Video Router / Feedback read the main composition's layers.
        if let lavmfx_render::LayerInput::Route(r) = input {
            self.preview_comp
                .feed_route(&self.gpu.device, &self.gpu.queue, r, &self.compositor);
        }
        self.preview_comp.render(
            &self.gpu.device,
            &self.gpu.queue,
            FrameInput {
                time: self.clock.seconds() as f32,
                beat: self.clock.beats() as f32,
                master: 1.0,
            },
            &[LayerDraw {
                fit: self.clip_fit(Some(s)),
                input,
                ..LayerDraw::alpha(1.0)
            }],
        );
    }

    /// The Preview Monitor panel.
    pub(super) fn preview_monitor(&mut self, ui: &mut egui::Ui) {
        let Some(pb) = &self.preview else {
            let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, super::theme::cur().video_empty);
            let text = if self.preview_opening.is_some() {
                "Opening…"
            } else {
                "Click a clip's thumbnail to preview it here"
            };
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(13.0),
                super::theme::cur().text_weak,
            );
            return;
        };
        let name = self
            .comp
            .clip(pb.slot.deck, pb.slot.layer, pb.slot.col)
            .map(|c| c.name.value.clone())
            .unwrap_or_default();
        let label = format!("{name} · Layer {} · Clip {}", pb.slot.layer + 1, pb.slot.col + 1);
        let (w, h) = self.preview_comp.size();
        let aspect = w as f32 / h as f32;
        let avail = ui.available_size();
        let ih = (avail.x / aspect).min(avail.y);
        let (rect, pic) = ui.allocate_exact_size(vec2(ih * aspect, ih), egui::Sense::click());
        // Click the picture = select the previewed clip (Clip panel); the
        // group / layer panels keep what they show.
        let slot = pb.slot;
        let in_deck = slot.deck == self.comp.active_deck();
        let pic = pic.on_hover_text(if in_deck {
            "Click: show this clip in the Clip panel"
        } else {
            "This clip is on another deck"
        });
        if pic.clicked() && in_deck {
            self.selected_clip = Some((slot.layer, slot.col));
            self.selection = std::collections::BTreeSet::from([(slot.layer, slot.col)]);
            self.focus_tab = Some(super::dock::Tab::Clip);
        }
        ui.painter().image(
            self.preview_tex,
            rect,
            egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        // Name + close over the picture's top edge.
        let bar = egui::Rect::from_min_size(rect.min, vec2(rect.width(), 20.0));
        ui.painter().rect_filled(bar, 0.0, super::theme::cur().overlay);
        let mut stop = false;
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar.shrink2(vec2(4.0, 1.0)))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        if child.small_button("×").on_hover_text("Stop preview").clicked() {
            stop = true;
        }
        child.label(RichText::new(label).small().color(super::theme::cur().overlay_text));
        if stop {
            self.stop_preview();
        }
        // Floating FX toggle (bottom-left) for the effect tried from the browser.
        if let Some((_, fx)) = &self.preview_fx {
            let pal = super::theme::cur();
            let on = self.preview_fx_on;
            let text = format!("FX  {}", fx.display_name);
            let font = egui::FontId::proportional(11.0);
            let w = ui.fonts_mut(|f| f.layout_no_wrap(text.clone(), font.clone(), Color32::WHITE).size().x) + 16.0;
            let b = egui::Rect::from_min_size(
                pos2(rect.left() + 6.0, rect.bottom() - 26.0),
                vec2(w.min(rect.width() - 12.0), 20.0),
            );
            let r = ui
                .interact(b, ui.scope_id().with("preview-fx-toggle"), egui::Sense::click())
                .on_hover_text(if on {
                    "Effect tried from the Effects browser: click to switch it off"
                } else {
                    "Click to show the effect again (double-click another effect in the browser to try that one)"
                });
            let fill = if on { pal.accent } else { pal.overlay };
            ui.painter().rect_filled(b, 4.0, fill);
            if r.hovered() {
                ui.painter()
                    .rect_stroke(b, 4.0, (1.0, pal.text_strong), egui::StrokeKind::Inside);
            }
            ui.painter().with_clip_rect(b).text(
                b.left_center() + vec2(8.0, 0.0),
                egui::Align2::LEFT_CENTER,
                text,
                font,
                if on { pal.on_accent } else { pal.overlay_text },
            );
            if r.clicked() {
                self.preview_fx_on = !on;
            }
            if r.clicked() || r.double_clicked() {
                self.preview_fx_keep = true;
            }
        }
    }
}
