//! Clip thumbnails: decoded on a worker thread, cached as egui textures by file path.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;

use eframe::egui;

/// Thumbnail size in pixels (2× the clip slot, for HiDPI).
const MAX: (u32, u32) = (192, 108);

enum State {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

pub struct Thumbs {
    map: HashMap<String, State>,
    tx: mpsc::Sender<String>,
    rx: mpsc::Receiver<(String, lavmfx_media::Result<lavmfx_media::Image>)>,
}

impl Thumbs {
    pub fn new() -> Self {
        let (tx, jobs) = mpsc::channel::<String>();
        let (done, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("thumbnails".into())
            .spawn(move || {
                for path in jobs {
                    let r = lavmfx_media::thumbnail(&PathBuf::from(&path), MAX.0, MAX.1);
                    if done.send((path, r)).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn thumbnail thread");
        Self {
            map: HashMap::new(),
            tx,
            rx,
        }
    }

    /// Texture for `path`, queuing it on first request.
    pub fn get(&mut self, path: &str) -> Option<&egui::TextureHandle> {
        let state = self.map.entry(path.to_owned()).or_insert_with(|| {
            let _ = self.tx.send(path.to_owned());
            State::Loading
        });
        match state {
            State::Ready(t) => Some(t),
            State::Loading | State::Failed => None,
        }
    }

    /// Turn finished thumbnails into textures.
    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((path, r)) = self.rx.try_recv() {
            let state = match r {
                Ok(img) => {
                    let size = [img.width as usize, img.height as usize];
                    let image = egui::ColorImage::from_rgba_unmultiplied(size, &img.rgba);
                    State::Ready(ctx.load_texture(format!("thumb:{path}"), image, egui::TextureOptions::LINEAR))
                }
                Err(e) => {
                    log::warn!("thumbnail: {e}");
                    State::Failed
                }
            };
            self.map.insert(path, state);
        }
    }
}
