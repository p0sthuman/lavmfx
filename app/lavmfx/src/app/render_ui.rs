//! Clip rendering (Clip › Render to file + Render Queue): the clip
//! with its effects, frame by frame, offline, into a movie file
//! (Preferences › Clip Rendering preset). A separate render-only compositor
//! runs the clip's chain; the layer (with alpha) is read back after its
//! effects and encoded with exact frame numbers. Work is spread over UI
//! frames (a few ms each) so the app stays live.
//!
//! The Render Queue panel lists running jobs (progress, cancel) and rendered
//! clips (drag onto a slot / double-click: load into the selected slot, show
//! the folder, remove from the queue — the file stays).

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use lavmfx_media::record::{self, Codec, RecordSettings, Recorder};
use lavmfx_media::{OpenOptions, VideoDecoder, VideoFrame};
use lavmfx_render::{Compositor, EffectDraw, FrameInput, FxFrame, LayerDraw, LayerInput, Route};

use super::LavmfxApp;
use crate::playback::Slot;

/// Source name prefix of a rendered file dragged from the queue (`SourceDrop`).
pub const FILE_PREFIX: &str = "FILE/";

/// Frames of a source / effect-only clip (no file): 5 s at 60 fps.
const VIRTUAL_FPS: u32 = 60;

enum Feed {
    /// Decoder thread: frames in order, then the channel closes.
    File(Frames),
    /// A generator: `total` frames on a clear layer.
    Virtual(u64),
}

struct Job {
    name: String,
    out: PathBuf,
    feed: Feed,
    total: Option<u64>,
    fps: u32,
    idx: i64,
    comp: Compositor,
    draws: Vec<EffectDraw>,
    fit: lavmfx_render::Fit,
    input: LayerInput,
    rec: Option<Recorder>,
    alpha: bool,
    started: Instant,
    /// All frames sent (decoder closed / virtual length reached).
    ended: bool,
    error: Option<String>,
    cancelled: bool,
}

/// A finished render in the queue.
pub struct Rendered {
    pub name: String,
    pub path: PathBuf,
    pub frames: u64,
    pub seconds: f64,
}

#[derive(Default)]
pub struct RenderQueue {
    jobs: Vec<Job>,
    pub done: Vec<Rendered>,
}

/// Frames in order from the decoder worker.
type Frames = Receiver<Result<VideoFrame, String>>;

/// Decode `path` in order on a worker (bounded: waits for the renderer):
/// (frames, fps, frame count estimate).
fn decode_thread(path: PathBuf) -> Result<(Frames, f64, Option<u64>), String> {
    let mut dec = lavmfx_media::FfmpegDecoder::open(
        &path,
        OpenOptions {
            native_bc: false,
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let info = dec.info().clone();
    let fps = info.fps();
    let total = (info.duration.as_secs_f64() * fps).round() as u64;
    let (tx, rx) = sync_channel(4);
    std::thread::Builder::new()
        .name("render-decode".into())
        .spawn(move || {
            loop {
                match dec.next_frame() {
                    Ok(Some(f)) => {
                        if tx.send(Ok(f)).is_err() {
                            return; // cancelled
                        }
                    }
                    Ok(None) => return,
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok((rx, fps, (total > 0).then_some(total)))
}

/// Premultiplied RGBA → straight (alpha presets) in place.
fn unpremultiply(px: &mut [u8]) {
    for p in px.as_chunks_mut::<4>().0 {
        let a = p[3];
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((u32::from(*c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8;
            }
        }
    }
}

impl LavmfxApp {
    /// Clip › Render to File: queue the selected clip.
    pub(super) fn render_selected_clip(&mut self) {
        let Some((li, ci)) = self.selected_clip else {
            self.status = "Select a clip to render".into();
            return;
        };
        let deck = self.comp.active_deck();
        let Some(clip) = self.comp.clip(deck, li, ci) else {
            return;
        };
        if clip.is_empty() {
            self.status = "Select a clip to render".into();
            return;
        }
        let slot = Slot {
            deck,
            layer: li,
            col: ci,
            clip: clip.id,
        };
        let name = clip.name.value.clone();
        let preset_name = self.prefs.rendering.preset.clone();
        let Some(preset) = record::preset(&preset_name) else {
            self.status = format!("Unknown render preset {preset_name}");
            return;
        };
        let input = match self.clip_input(Some(slot)) {
            LayerInput::Route(_) | LayerInput::Below => {
                self.status = "Render to File: routed / effect clips need other layers (not yet)".into();
                return;
            }
            i => i,
        };
        let (feed, fps, total) = match clip.video_path() {
            Some(p) if input == LayerInput::Frame && clip.source().is_none() => match decode_thread(PathBuf::from(p)) {
                Ok((rx, fps, total)) => (Feed::File(rx), fps.round().max(1.0) as u32, total),
                Err(e) => {
                    self.status = format!("Can't render {name}: {e}");
                    return;
                }
            },
            _ if input == LayerInput::Clear => {
                let n = (lavmfx_core::sources::VIRTUAL_SECONDS * f64::from(VIRTUAL_FPS)) as u64;
                (Feed::Virtual(n), VIRTUAL_FPS, Some(n))
            }
            _ => {
                self.status = format!("Can't render {name}: live inputs (NDI / cameras) aren't rendered");
                return;
            }
        };
        let (w, h) = self.compositor.size();
        let mut comp = Compositor::new(&self.gpu.device, w, h);
        comp.set_isf_library(self.isf.lib.clone());
        comp.set_external_fx(self.ffgl.clone());
        comp.ensure_layer(&self.gpu.device, 0);
        comp.set_extra_routes(vec![Route::Layer(0)]);
        let dir = super::record_ui::folder_or_default(&self.prefs.folders.renders, "Renders");
        let out = record::timestamped(&dir, &name, preset.extension());
        let settings = RecordSettings {
            preset: preset_name,
            fps,
            audio: None,
        };
        let rec = match Recorder::start(&out, w, h, &settings) {
            Ok(r) => r,
            Err(e) => {
                self.status = format!("Can't render {name}: {e}");
                return;
            }
        };
        // What plays in the layer: the clip's chain, then the layer's.
        let mut draws = super::effects_ui::clip_draws(Some(clip));
        draws.extend(super::effects_ui::draws(
            self.comp.effects(lavmfx_core::effects::FxTarget::Layer(li)),
        ));
        let fit = self.clip_fit(Some(slot));
        self.render_queue.jobs.push(Job {
            name: name.clone(),
            out,
            feed,
            total,
            fps,
            idx: 0,
            comp,
            draws,
            fit,
            input,
            rec: Some(rec),
            alpha: matches!(preset.codec, Codec::HapAlpha) || preset.format == "4444",
            started: Instant::now(),
            ended: false,
            error: None,
            cancelled: false,
        });
        super::dock::add_render_tab(&mut self.dock);
        self.focus_tab = Some(super::dock::Tab::Render);
        self.status = format!("Rendering {name}…");
    }

    /// Each UI frame: render queued frames for ~6 ms; finish jobs that are done.
    pub(super) fn tick_render(&mut self) {
        let budget = Instant::now() + Duration::from_millis(6);
        let (device, queue) = (&self.gpu.device, &self.gpu.queue);
        for job in &mut self.render_queue.jobs {
            while Instant::now() < budget && job.error.is_none() && !job.cancelled {
                let frame = match &job.feed {
                    Feed::File(rx) => match rx.try_recv() {
                        Ok(Ok(f)) => Some(f),
                        Ok(Err(e)) => {
                            job.error = Some(e);
                            break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break, // decoder behind
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            job.total = Some(job.idx as u64);
                            job.ended = true;
                            break;
                        }
                    },
                    Feed::Virtual(n) => {
                        if job.idx as u64 >= *n {
                            job.ended = true;
                            break;
                        }
                        None
                    }
                };
                if let Some(f) = &frame {
                    job.comp.set_layer_frame(device, queue, 0, f);
                }
                job.comp.set_effects(FxFrame {
                    layer: vec![job.draws.clone()],
                    ..Default::default()
                });
                job.comp.render(
                    device,
                    queue,
                    FrameInput {
                        time: job.idx as f32 / job.fps as f32,
                        beat: 0.0,
                        master: 1.0,
                    },
                    &[LayerDraw {
                        fit: job.fit,
                        input: job.input,
                        ..LayerDraw::alpha(1.0)
                    }],
                );
                let Some((w, h, mut px)) = job.comp.read_route(device, queue, Route::Layer(0)) else {
                    job.error = Some("no layer output".into());
                    break;
                };
                if job.alpha {
                    unpremultiply(&mut px);
                }
                if let Some(rec) = &job.rec {
                    rec.send_indexed(
                        lavmfx_media::live::OutFrame {
                            data: px,
                            width: w,
                            height: h,
                            stride: w * 4,
                            rate: (job.fps as i32, 1),
                        },
                        job.idx,
                    );
                }
                job.idx += 1;
            }
        }
        // Finished / failed / cancelled jobs.
        let mut i = 0;
        while i < self.render_queue.jobs.len() {
            let j = &self.render_queue.jobs[i];
            if !(j.ended || j.error.is_some() || j.cancelled) {
                i += 1;
                continue;
            }
            let mut j = self.render_queue.jobs.remove(i);
            let st = j.rec.take().map(Recorder::finish);
            if j.cancelled {
                let _ = std::fs::remove_file(&j.out);
                self.status = format!("Render of {} cancelled", j.name);
            } else if let Some(e) = j.error.or_else(|| st.as_ref().and_then(|s| s.error.clone())) {
                self.status = format!("Render of {} failed: {e}", j.name);
                log::warn!("{}", self.status);
            } else {
                let frames = j.idx as u64;
                self.status = format!(
                    "Rendered {} ({} frames in {:.1} s) to {}",
                    j.name,
                    frames,
                    j.started.elapsed().as_secs_f64(),
                    j.out.display()
                );
                log::info!("{}", self.status);
                self.render_queue.done.insert(
                    0,
                    Rendered {
                        name: j.name,
                        path: j.out,
                        frames,
                        seconds: frames as f64 / f64::from(j.fps),
                    },
                );
            }
        }
    }

    /// The Render Queue panel.
    pub(super) fn render_queue_panel(&mut self, ui: &mut egui::Ui) {
        let pal = super::theme::pal(ui);
        ui.label(
            RichText::new(format!(
                "Preset: {} (Preferences › Clip Rendering)",
                self.prefs.rendering.preset
            ))
            .small()
            .color(pal.text_weak),
        );
        let mut cancel = None;
        for (i, j) in self.render_queue.jobs.iter().enumerate() {
            ui.horizontal(|ui| {
                let frac = j.total.map_or(0.0, |t| j.idx as f32 / t.max(1) as f32);
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width((ui.available_width() - 70.0).max(60.0))
                        .text(format!(
                            "{} · {}/{}",
                            j.name,
                            j.idx,
                            j.total.map_or("?".into(), |t| t.to_string())
                        )),
                );
                if ui.small_button("Cancel").clicked() {
                    cancel = Some(i);
                }
            });
        }
        if let Some(i) = cancel {
            self.render_queue.jobs[i].cancelled = true;
        }
        if self.render_queue.jobs.is_empty() && self.render_queue.done.is_empty() {
            ui.label(RichText::new("Clip › Render to File renders the selected clip with its effects.").weak());
        }
        let mut remove = None;
        let mut load = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, r) in self.render_queue.done.iter().enumerate() {
                ui.horizontal(|ui| {
                    let label = ui
                        .add(
                            egui::Label::new(RichText::new(&r.name).color(pal.text_strong))
                                .selectable(false)
                                .sense(egui::Sense::click_and_drag()),
                        )
                        .on_hover_text(format!(
                            "{}\n{} frames · {:.1} s\nDrag onto a clip slot · double-click: load into the selected slot",
                            r.path.display(),
                            r.frames,
                            r.seconds
                        ));
                    label.dnd_set_drag_payload(super::effects_ui::SourceDrop {
                        name: format!("{FILE_PREFIX}{}", r.path.display()),
                    });
                    if label.double_clicked() {
                        load = Some(r.path.clone());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Remove").on_hover_text("Remove from the queue (the file stays)").clicked() {
                            remove = Some(i);
                        }
                        if ui.small_button("Folder").on_hover_text("Show in the file manager").clicked()
                            && let Some(dir) = r.path.parent()
                        {
                            let _ = std::process::Command::new(if cfg!(target_os = "macos") {
                                "open"
                            } else if cfg!(target_os = "windows") {
                                "explorer"
                            } else {
                                "xdg-open"
                            })
                            .arg(dir)
                            .spawn();
                        }
                    });
                });
            }
        });
        if let Some(i) = remove {
            self.render_queue.done.remove(i);
        }
        if let Some(p) = load {
            let (li, ci) = self.default_target();
            while ci >= self.comp.columns.len() {
                self.comp.add_column();
            }
            self.ensure_layers();
            if self.put_file(&p, (li, ci)) {
                self.select_only((li, ci));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unpremultiply_restores_colour() {
        let mut px = [64u8, 32, 0, 128, 10, 10, 10, 255, 0, 0, 0, 0];
        super::unpremultiply(&mut px);
        assert_eq!(&px[..4], &[128, 64, 0, 128]);
        assert_eq!(&px[4..], &[10, 10, 10, 255, 0, 0, 0, 0]);
    }
}
