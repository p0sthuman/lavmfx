//! Audio waveform overviews for clip slots (a clip's audio as a pink
//! waveform): decoded on a worker thread, cached by file path. Files
//! without audio (or too long to load) cache `None`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};

/// Slices per waveform (a slot is ~96 px wide; 2× for HiDPI).
const SLICES: usize = 192;

pub type Peaks = Arc<[f32]>;

enum State {
    Loading,
    Ready(Option<Peaks>),
}

pub struct Waves {
    map: HashMap<String, State>,
    tx: mpsc::Sender<String>,
    rx: mpsc::Receiver<(String, Option<Peaks>)>,
}

impl Waves {
    pub fn new() -> Self {
        let (tx, jobs) = mpsc::channel::<String>();
        let (done, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("waveforms".into())
            .spawn(move || {
                for path in jobs {
                    let peaks = match lavmfx_media::audio::decode(&PathBuf::from(&path)) {
                        Ok(pcm) => pcm.map(|p| Peaks::from(p.peaks(SLICES))),
                        Err(e) => {
                            log::warn!("waveform: {e}");
                            None
                        }
                    };
                    if done.send((path, peaks)).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn waveform thread");
        Self {
            map: HashMap::new(),
            tx,
            rx,
        }
    }

    /// Waveform of `path` (`None` while loading or without audio), queued on first request.
    pub fn get(&mut self, path: &str) -> Option<Peaks> {
        let state = self.map.entry(path.to_owned()).or_insert_with(|| {
            let _ = self.tx.send(path.to_owned());
            State::Loading
        });
        match state {
            State::Ready(p) => p.clone(),
            State::Loading => None,
        }
    }

    /// Take finished waveforms; true when any arrived (repaint).
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((path, peaks)) = self.rx.try_recv() {
            self.map.insert(path, State::Ready(peaks));
            any = true;
        }
        any
    }
}

/// Draw `peaks` as a waveform mirrored around the middle of `rect`, one bar
/// per pixel column.
pub fn paint(p: &eframe::egui::Painter, rect: eframe::egui::Rect, peaks: &[f32], color: eframe::egui::Color32) {
    use eframe::egui::{Shape, Stroke, pos2};
    if peaks.is_empty() || rect.width() < 1.0 {
        return;
    }
    let cols = rect.width().floor().max(1.0) as usize;
    let mid = rect.center().y;
    let half = rect.height() / 2.0;
    let shapes = (0..cols).map(|x| {
        let a = x * peaks.len() / cols;
        let b = ((x + 1) * peaks.len() / cols).max(a + 1).min(peaks.len());
        let v = peaks[a..b].iter().copied().fold(0.0f32, f32::max);
        let h = (v * half).max(0.5);
        let px = rect.left() + x as f32 + 0.5;
        Shape::line_segment([pos2(px, mid - h), pos2(px, mid + h)], Stroke::new(1.0, color))
    });
    p.extend(shapes);
}
