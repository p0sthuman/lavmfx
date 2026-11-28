//! Composition recording (the Record button: click to start, click again
//! to stop; the recording is added to the deck as a clip) and snapshots
//! (PNG of the composition). Settings: Preferences › Recording.

use std::path::PathBuf;
use std::sync::Arc;

use lavmfx_media::record::{self, RecordSettings, Recorder};

use super::LavmfxApp;

/// Default output folder `<Videos or home>/LAVMFX/<sub>`: on the Linux drive
/// (writes to NTFS from Linux are slow), not next to the media. In portable
/// mode `<portable folder>/recordings/<sub>`, so it travels with the setup.
pub fn default_folder(sub: &str) -> PathBuf {
    if let Some(root) = crate::app_dirs().and_then(|d| d.portable_root()) {
        return root.join("recordings").join(sub);
    }
    let base = directories::UserDirs::new()
        .and_then(|u| u.video_dir().map(PathBuf::from))
        .or_else(|| directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("LAVMFX").join(sub)
}

/// A folder preference, or its default when empty.
pub fn folder_or_default(v: &str, sub: &str) -> PathBuf {
    if v.trim().is_empty() {
        default_folder(sub)
    } else {
        PathBuf::from(v.trim())
    }
}

/// Recording state the app keeps besides the recorder.
#[derive(Default)]
pub struct RecordState {
    /// Mix tap while recording with audio.
    tap: Option<Arc<lavmfx_audio::mixer::AudioTap>>,
    /// Audio wanted at this rate, the tap not attached yet (output not open).
    pending_audio: Option<u32>,
    started: Option<std::time::Instant>,
    /// Composition size it records at (a change stops the recording).
    size: (u32, u32),
    /// Debug (`LAVMFX_SCREENSHOT_RECORD=secs`): stop then; and a snapshot at the start.
    pub debug_stop: Option<std::time::Instant>,
}

impl LavmfxApp {
    pub(super) fn recording(&self) -> bool {
        self.live_out.record.is_some()
    }

    pub(super) fn toggle_recording(&mut self) {
        if self.recording() {
            self.stop_recording();
        } else {
            self.start_recording();
        }
    }

    fn start_recording(&mut self) {
        let r = &self.prefs.recording;
        let Some(preset) = record::preset(&r.preset) else {
            self.status = format!("Unknown recording preset {}", r.preset);
            return;
        };
        let dir = folder_or_default(&self.prefs.folders.recordings, "Recorded");
        let path = record::timestamped(&dir, "LAVMFX Recording", preset.extension());
        // Audio at the output's rate; if the output isn't open yet (no clip with
        // sound played so far) at the configured rate, attached when it opens.
        let tap = if r.audio { self.audio.tap() } else { None };
        let rate = tap.as_ref().map_or(self.prefs.audio.sample_rate, |(_, rate)| *rate);
        let settings = RecordSettings {
            preset: r.preset.clone(),
            fps: r.fps.max(1),
            audio: r.audio.then_some((rate, r.bit_depth)),
        };
        let size = self.compositor.size();
        match Recorder::start(&path, size.0, size.1, &settings) {
            Ok(rec) => {
                if let Some((t, _)) = &tap {
                    t.set_on(true);
                }
                let want = r.audio;
                self.rec = RecordState {
                    pending_audio: (want && tap.is_none()).then_some(rate),
                    tap: tap.map(|(t, _)| t),
                    size,
                    started: Some(std::time::Instant::now()),
                    debug_stop: self.rec.debug_stop,
                };
                self.live_out.record = Some(rec);
                self.status = format!("Recording to {}", path.display());
            }
            Err(e) => self.status = format!("Can't record: {e}"),
        }
    }

    fn stop_recording(&mut self) {
        let Some(rec) = self.live_out.record.take() else { return };
        if let Some(t) = self.rec.tap.take() {
            t.set_on(false);
        }
        let path = rec.path.clone();
        let st = rec.finish();
        if let Some(e) = st.error {
            self.status = format!("Recording failed: {e}");
            return;
        }
        // The recording becomes a clip in the deck.
        let li = self
            .selected_layer
            .unwrap_or(0)
            .min(self.comp.layers.len().saturating_sub(1));
        let ci = self
            .comp
            .layers
            .get(li)
            .and_then(|l| l.clips.iter().position(|c| c.is_empty()))
            .unwrap_or(self.comp.columns.len());
        while ci >= self.comp.columns.len() {
            self.comp.add_column();
        }
        let added = self.put_file(&path, (li, ci));
        self.status = format!(
            "Recorded {:.1} s ({} frames{}) to {}{}",
            st.seconds,
            st.frames,
            if st.dropped > 0 {
                format!(", {} dropped", st.dropped)
            } else {
                String::new()
            },
            path.display(),
            if added {
                format!(" · added to layer {} clip {}", li + 1, ci + 1)
            } else {
                String::new()
            }
        );
    }

    /// Each frame: audio into the recorder; a size change or an error stops it.
    pub(super) fn tick_recording(&mut self) {
        if self.rec.debug_stop.is_some_and(|t| std::time::Instant::now() >= t) {
            self.rec.debug_stop = None;
            self.stop_recording();
            log::info!("{}", self.status);
            if std::env::var_os("LAVMFX_SCREENSHOT_SNAPSHOT").is_some() {
                self.snapshot();
                log::info!("{}", self.status);
            }
        }
        let Some(rec) = &self.live_out.record else { return };
        // The output opened during the recording: attach, silence up to now.
        if let Some(rate) = self.rec.pending_audio
            && let Some((t, out_rate)) = self.audio.tap()
        {
            self.rec.pending_audio = None;
            if out_rate == rate {
                let secs = self.rec.started.map_or(0.0, |s| s.elapsed().as_secs_f64());
                rec.push_audio(vec![0.0; (secs * f64::from(rate)) as usize * 2]);
                t.set_on(true);
                self.rec.tap = Some(t);
            } else {
                self.status = format!("Recording audio left silent: the output runs at {out_rate} Hz, not {rate} Hz");
            }
        }
        if let Some(t) = &self.rec.tap {
            let samples = t.take();
            if !samples.is_empty() {
                rec.push_audio(samples);
            }
        }
        if self.compositor.size() != self.rec.size {
            self.stop_recording();
            self.status = format!("Recording stopped: the composition size changed. {}", self.status);
        } else if let Some(e) = rec.status().error {
            self.stop_recording();
            self.status = format!("Recording failed: {e}");
        }
    }

    /// Status bar text while recording.
    pub(super) fn recording_label(&self) -> Option<String> {
        let s = self.live_out.record.as_ref()?.status();
        let t = s.seconds as u64;
        Some(format!(
            "● REC {:02}:{:02} · {:.0} MB",
            t / 60,
            t % 60,
            s.bytes as f64 / 1e6
        ))
    }

    /// Composition › Snapshot: a PNG of the composition.
    pub(super) fn snapshot(&mut self) {
        let tex = self.compositor.texture();
        let (w, h) = (tex.width(), tex.height());
        let px = lavmfx_render::read_texture(&self.gpu.device, &self.gpu.queue, tex);
        let dir = folder_or_default(&self.prefs.folders.snapshots, "Snapshots");
        let path = record::timestamped(&dir, "LAVMFX Snapshot", "png");
        self.status = match record::save_png(&path, w, h, &px) {
            Ok(()) => format!("Snapshot saved: {}", path.display()),
            Err(e) => format!("Snapshot failed: {e}"),
        };
    }
}
