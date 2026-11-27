//! Composition › Media Manager: every file the composition's clips use (all
//! decks), checked on a worker (exists, size, codec), with the missing ones
//! first in red and three ways to fix them: Relink… one file (then the same
//! folder is searched for the other missing files), Search
//! Folder… (by file name, sub-folders too) and Replace Path (a path prefix
//! swap, e.g. another drive letter or mount point).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui::{self, Color32, RichText, Sense, vec2};
use lavmfx_core::media::{self, MediaFile};

use super::{LavmfxApp, theme};

/// Files a folder search looks at, at most.
const MAX_INDEX: usize = 200_000;

/// What the worker found out about a file.
#[derive(Debug, Clone, Default)]
struct Detail {
    exists: bool,
    size: Option<u64>,
    codec: Option<String>,
    width: u32,
    height: u32,
    fps: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Name,
    Codec,
    Dims,
    Fps,
    Size,
    Kind,
    Clips,
    Path,
}

const COLS: [(Col, &str, f32); 8] = [
    (Col::Name, "File Name", 210.0),
    (Col::Codec, "Compression", 170.0),
    (Col::Dims, "Dimensions", 95.0),
    (Col::Fps, "Framerate", 75.0),
    (Col::Size, "Size", 75.0),
    (Col::Kind, "Type", 55.0),
    (Col::Clips, "Clips", 45.0),
    (Col::Path, "Path", 0.0),
];

enum Pick {
    /// Relink this path to a picked file.
    File(String),
    Folder,
}

/// (old path, new path) pairs a folder search found.
type Relinks = Vec<(String, String)>;

#[derive(Default)]
pub struct MediaManager {
    selected: Option<String>,
    missing_only: bool,
    search: String,
    sort: Option<(Col, bool)>,
    details: HashMap<String, Detail>,
    scan: Option<Receiver<(String, Detail)>>,
    picker: Option<(Pick, Receiver<Option<PathBuf>>)>,
    /// Folder search running: (folder, found (old, new) pairs).
    folder: Option<(PathBuf, Receiver<Relinks>)>,
    replace_from: String,
    replace_to: String,
    status: String,
}

fn kind_of(path: &str) -> &'static str {
    let p = Path::new(path);
    if lavmfx_media::is_audio_file(p) {
        "Audio"
    } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff", "webp"].contains(&e.to_ascii_lowercase().as_str())
    }) {
        "Image"
    } else {
        "Video"
    }
}

fn fmt_size(b: u64) -> String {
    let mb = b as f64 / 1_048_576.0;
    if mb >= 1024.0 {
        format!("{:.1}GB", mb / 1024.0)
    } else {
        format!("{mb:.1}MB")
    }
}

/// Parent folder of a stored path (either separator).
fn parent_of(path: &str) -> String {
    match path.rfind(['/', '\\']) {
        Some(i) => path[..i].to_owned(),
        None => String::new(),
    }
}

/// Check files on a worker; results arrive one by one.
fn scan(paths: Vec<String>) -> Receiver<(String, Detail)> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("media manager".into())
        .spawn(move || {
            for p in paths {
                let path = Path::new(&p);
                let mut d = Detail::default();
                if let Ok(m) = std::fs::metadata(path)
                    && m.is_file()
                {
                    d.exists = true;
                    d.size = Some(m.len());
                    if let Ok(i) = lavmfx_media::probe(path) {
                        d.codec = Some(i.codec.clone());
                        (d.width, d.height, d.fps) = (i.width, i.height, i.fps());
                    }
                }
                if tx.send((p, d)).is_err() {
                    return;
                }
            }
        })
        .expect("spawn media scan");
    rx
}

fn pick(folder: bool, start: Option<PathBuf>) -> Receiver<Option<PathBuf>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut d = rfd::FileDialog::new();
        if let Some(s) = start.filter(|s| s.is_dir()) {
            d = d.set_directory(s);
        }
        let r = if folder {
            d.set_title("Search missing files in folder").pick_folder()
        } else {
            d.set_title("Relink file").pick_file()
        };
        let _ = tx.send(r);
    });
    rx
}

impl LavmfxApp {
    pub(super) fn open_media_manager(&mut self) {
        if self.media.is_none() {
            let mut m = MediaManager {
                sort: Some((Col::Name, true)),
                ..Default::default()
            };
            m.scan = Some(scan(self.comp.media_files().into_iter().map(|f| f.path).collect()));
            self.media = Some(m);
        }
    }

    pub(super) fn media_manager_window(&mut self, ctx: &egui::Context) {
        if self.media.is_none() {
            return;
        }
        let id = egui::ViewportId::from_hash_of("lavmfx-media-manager");
        let b = egui::ViewportBuilder::default()
            .with_title("Media Manager")
            .with_app_id("lavmfx-media-manager")
            .with_inner_size([1250.0, 760.0]);
        ctx.show_viewport_immediate(id, b, |ui, _| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                self.media = None;
                return;
            }
            let Some(mut m) = self.media.take() else { return };
            let keep = self.media_ui(ui, &mut m);
            if keep {
                self.media = Some(m);
            }
        });
    }

    /// Run the manager's background results into the composition.
    fn media_poll(&mut self, m: &mut MediaManager) {
        if let Some(rx) = &m.scan {
            let mut found = HashMap::new();
            loop {
                match rx.try_recv() {
                    Ok((p, d)) => {
                        found.insert(p.clone(), d.exists);
                        m.details.insert(p, d);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        m.scan = None;
                        break;
                    }
                }
            }
            if !found.is_empty() {
                self.comp.check_media(&found);
            }
        }
        if let Some((what, rx)) = &m.picker
            && let Ok(r) = rx.try_recv()
        {
            let what = match what {
                Pick::File(p) => Pick::File(p.clone()),
                Pick::Folder => Pick::Folder,
            };
            m.picker = None;
            match (what, r) {
                (Pick::File(old), Some(new)) => self.media_relink_one(m, &old, &new),
                (Pick::Folder, Some(dir)) => self.media_search(m, dir),
                _ => {}
            }
        }
        if let Some((dir, rx)) = &m.folder
            && let Ok(pairs) = rx.try_recv()
        {
            let dir = dir.clone();
            m.folder = None;
            for (old, new) in &pairs {
                self.comp.relink(old, new, None);
            }
            m.status = format!(
                "{}: found {} missing file{}",
                dir.display(),
                pairs.len(),
                if pairs.len() == 1 { "" } else { "s" }
            );
            self.media_rescan(m, pairs.into_iter().map(|p| p.1).collect());
        }
    }

    fn media_rescan(&mut self, m: &mut MediaManager, paths: Vec<String>) {
        if !paths.is_empty() {
            m.scan = Some(scan(paths));
        }
    }

    fn missing_paths(&self) -> Vec<String> {
        self.comp
            .media_files()
            .into_iter()
            .filter(|f| !f.exists)
            .map(|f| f.path)
            .collect()
    }

    /// Relink `old` to the picked `new` file, then look for the other missing
    /// files in the same folder.
    fn media_relink_one(&mut self, m: &mut MediaManager, old: &str, new: &Path) {
        let info = lavmfx_media::probe(new).ok().map(|i| crate::playback::file_info(&i));
        let new_s = new.to_string_lossy().into_owned();
        let n = self.comp.relink(old, &new_s, info.as_ref());
        m.selected = Some(new_s.clone());
        m.status = format!(
            "Relinked {} ({n} clip{})",
            media::file_name(&new_s),
            if n == 1 { "" } else { "s" }
        );
        self.media_rescan(m, vec![new_s]);
        if let Some(dir) = new.parent().map(Path::to_path_buf)
            && !self.missing_paths().is_empty()
        {
            self.media_search(m, dir);
        }
    }

    fn media_search(&mut self, m: &mut MediaManager, dir: PathBuf) {
        let missing = self.missing_paths();
        if missing.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let d = dir.clone();
        std::thread::spawn(move || {
            let idx = media::index_folder(&d, MAX_INDEX);
            let _ = tx.send(media::find_in(&missing, &idx));
        });
        m.status = format!("Searching {}…", dir.display());
        m.folder = Some((dir, rx));
    }

    /// The window's contents; false = closed.
    fn media_ui(&mut self, ui: &mut egui::Ui, m: &mut MediaManager) -> bool {
        self.media_poll(m);
        if m.scan.is_some() || m.folder.is_some() || m.picker.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        let pal = theme::pal(ui);
        let files = self.comp.media_files();
        let missing = files.iter().filter(|f| !f.exists).count();
        let mut keep = true;

        egui::Panel::bottom("media-bottom")
            .frame(egui::Frame::new().fill(pal.panel).inner_margin(egui::Margin::symmetric(12, 10)))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                ui.horizontal(|ui| {
                    ui.label("Replace path");
                    ui.add(egui::TextEdit::singleline(&mut m.replace_from).desired_width(320.0).hint_text("old folder, e.g. C:\\media"));
                    ui.label("with");
                    ui.add(egui::TextEdit::singleline(&mut m.replace_to).desired_width(320.0).hint_text("new folder, e.g. /mnt/local/media"));
                    let can = !m.replace_from.is_empty() && !m.replace_to.is_empty();
                    if ui
                        .add_enabled(can, egui::Button::new("Apply"))
                        .on_hover_text("Every file whose path starts with the old folder points into the new one, when the file is there")
                        .clicked()
                    {
                        let (ok, skipped) = self.comp.replace_prefix(&m.replace_from, &m.replace_to, |p| Path::new(p).is_file());
                        m.status = format!("Replace path: {ok} file(s) relinked, {skipped} not found in the new folder");
                        let paths = self.comp.media_files().into_iter().map(|f| f.path).collect();
                        self.media_rescan(m, paths);
                    }
                });
                ui.horizontal(|ui| {
                    let sel_missing = m.selected.as_ref().filter(|s| files.iter().any(|f| &f.path == *s));
                    if ui
                        .add_enabled(sel_missing.is_some() && m.picker.is_none(), egui::Button::new("Relink…"))
                        .on_hover_text("Pick the file for the selected entry (all its clips); the same folder is then searched for the other missing files")
                        .clicked()
                        && let Some(s) = sel_missing
                    {
                        let start = Path::new(&parent_of(s)).to_path_buf();
                        m.picker = Some((Pick::File(s.clone()), pick(false, Some(start))));
                    }
                    if ui
                        .add_enabled(missing > 0 && m.picker.is_none() && m.folder.is_none(), egui::Button::new("Search Folder…"))
                        .on_hover_text("Find the missing files by name in a folder and its sub-folders")
                        .clicked()
                    {
                        m.picker = Some((Pick::Folder, pick(true, None)));
                    }
                    if ui.button("Refresh").on_hover_text("Check every file again").clicked() {
                        m.details.clear();
                        let paths = files.iter().map(|f| f.path.clone()).collect();
                        self.media_rescan(m, paths);
                    }
                    let busy = if m.folder.is_some() {
                        " · searching…"
                    } else if m.scan.is_some() {
                        " · checking files…"
                    } else {
                        ""
                    };
                    ui.label(RichText::new(format!("{}{busy}", m.status)).weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new("Close").min_size(vec2(90.0, 0.0))).clicked() {
                            keep = false;
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal.ground).inner_margin(egui::Margin::same(12)))
            .show(ui, |ui| {
                self.media_header(ui, m, &files);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let text = if missing == 0 {
                        RichText::new(format!("{} files · none missing", files.len())).color(pal.accent)
                    } else {
                        RichText::new(format!("{} files · {missing} missing", files.len()))
                            .color(pal.danger)
                            .strong()
                    };
                    ui.label(text);
                    ui.add_space(12.0);
                    ui.checkbox(&mut m.missing_only, "Missing only");
                    ui.add_space(12.0);
                    super::widgets::search_field(ui, &mut m.search, "Filter by name or path");
                });
                ui.add_space(6.0);
                self.media_table(ui, m, files);
            });
        keep
    }

    fn media_header(&mut self, ui: &mut egui::Ui, m: &MediaManager, files: &[MediaFile]) {
        let pal = theme::pal(ui);
        let sel = m
            .selected
            .as_ref()
            .and_then(|s| files.iter().find(|f| &f.path == s))
            .or_else(|| files.iter().find(|f| !f.exists))
            .or(files.first());
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(160.0, 90.0), Sense::hover());
            ui.painter().rect_filled(rect, 3.0, pal.video_empty);
            let Some(f) = sel else {
                ui.label(RichText::new("No clips use files yet.").weak());
                return;
            };
            let d = m.details.get(&f.path);
            let audio = lavmfx_media::is_audio_file(Path::new(&f.path));
            if f.exists && !audio {
                if let Some(t) = self.thumbs.get(&f.path) {
                    ui.painter().image(
                        t.id(),
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
            } else if f.exists
                && let Some(w) = self.waves.get(&f.path)
            {
                crate::waves::paint(ui.painter(), rect.shrink(6.0), &w, pal.audio);
            } else if !f.exists {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "MISSING",
                    egui::FontId::proportional(14.0),
                    pal.danger,
                );
            }
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(RichText::new(media::file_name(&f.path)).strong().size(15.0));
                let codec = d.and_then(|d| d.codec.clone()).unwrap_or_else(|| "–".into());
                ui.label(RichText::new(codec).color(pal.text_weak));
                if !audio {
                    ui.label(RichText::new(format!("{} x {}", f.width, f.height)).color(pal.text_weak));
                }
                ui.label(RichText::new(&f.path).color(pal.text_weak));
                if !f.exists {
                    ui.label(
                        RichText::new("File not found: Relink… it, or Search Folder… / Replace path below.")
                            .color(pal.danger),
                    );
                }
            });
        });
    }

    fn media_table(&mut self, ui: &mut egui::Ui, m: &mut MediaManager, files: Vec<MediaFile>) {
        let pal = theme::pal(ui);
        let q = m.search.to_lowercase();
        let mut rows: Vec<MediaFile> = files
            .into_iter()
            .filter(|f| !m.missing_only || !f.exists)
            .filter(|f| q.is_empty() || f.path.to_lowercase().contains(&q))
            .collect();
        let (col, asc) = m.sort.unwrap_or((Col::Name, true));
        let det = &m.details;
        rows.sort_by(|a, b| {
            let (da, db) = (det.get(&a.path), det.get(&b.path));
            let o = match col {
                Col::Name => media::file_name(&a.path)
                    .to_lowercase()
                    .cmp(&media::file_name(&b.path).to_lowercase()),
                Col::Codec => da.and_then(|d| d.codec.clone()).cmp(&db.and_then(|d| d.codec.clone())),
                Col::Dims => (a.width * a.height).cmp(&(b.width * b.height)),
                Col::Fps => a.fps.total_cmp(&b.fps),
                Col::Size => da.and_then(|d| d.size).cmp(&db.and_then(|d| d.size)),
                Col::Kind => kind_of(&a.path).cmp(kind_of(&b.path)),
                Col::Clips => a.clips.cmp(&b.clips),
                Col::Path => a.path.cmp(&b.path),
            };
            // Missing files first, then the chosen order.
            a.exists.cmp(&b.exists).then(if asc { o } else { o.reverse() })
        });

        let row_h = 26.0;
        let width = ui.available_width();
        let path_w = (width - 18.0 - COLS.iter().map(|c| c.2).sum::<f32>()).max(160.0);
        let widths: Vec<f32> = COLS
            .iter()
            .map(|c| if c.0 == Col::Path { path_w } else { c.2 })
            .collect();
        // Header.
        let (hr, _) = ui.allocate_exact_size(vec2(width, row_h), Sense::hover());
        ui.painter().rect_filled(hr, 0.0, pal.band);
        let mut x = hr.left() + 18.0;
        for (i, (c, label, _)) in COLS.iter().enumerate() {
            let r = egui::Rect::from_min_size(egui::pos2(x, hr.top()), vec2(widths[i], row_h));
            let resp = ui.interact(r, ui.scope_id().with(("media-col", i)), Sense::click());
            let tr = ui.painter().text(
                r.left_center() + vec2(6.0, 0.0),
                egui::Align2::LEFT_CENTER,
                *label,
                egui::FontId::proportional(13.0),
                if resp.hovered() { pal.text_strong } else { pal.text },
            );
            if let Some((s, up)) = m.sort
                && s == *c
            {
                // Sort direction: a small triangle after the label.
                let ctr = egui::pos2(tr.right() + 9.0, r.center().y);
                let (a, b) = if up { (-3.0, 3.0) } else { (3.0, -3.0) };
                ui.painter().add(egui::Shape::convex_polygon(
                    vec![ctr + vec2(0.0, a), ctr + vec2(4.0, b), ctr + vec2(-4.0, b)],
                    pal.accent,
                    egui::Stroke::NONE,
                ));
            }
            if resp.clicked() {
                m.sort = Some((*c, !matches!(m.sort, Some((s, true)) if s == *c)));
            }
            ui.painter()
                .vline(r.left(), r.y_range(), egui::Stroke::new(1.0, pal.border));
            x += widths[i];
        }

        egui::ScrollArea::both()
            .auto_shrink(false)
            .show_rows(ui, row_h, rows.len(), |ui, range| {
                for f in &rows[range] {
                    let (r, resp) = ui.allocate_exact_size(vec2(width, row_h), Sense::click());
                    let sel = m.selected.as_deref() == Some(f.path.as_str());
                    let bg = if sel {
                        pal.accent_dim
                    } else if resp.hovered() {
                        pal.button
                    } else {
                        pal.field
                    };
                    let p = ui.painter_at(r);
                    p.rect_filled(r.shrink2(vec2(0.0, 1.0)), 0.0, bg);
                    // Status dot.
                    p.circle_filled(
                        r.left_center() + vec2(9.0, 0.0),
                        4.0,
                        if f.exists { pal.accent } else { pal.danger },
                    );
                    let d = m.details.get(&f.path);
                    let (w, h, fps) = match d.filter(|d| d.width > 0) {
                        Some(d) => (d.width as i32, d.height as i32, d.fps),
                        None => (f.width, f.height, f.fps),
                    };
                    let audio = lavmfx_media::is_audio_file(Path::new(&f.path));
                    let cells = [
                        media::file_name(&f.path).to_owned(),
                        d.and_then(|d| d.codec.clone())
                            .unwrap_or_else(|| if f.exists { "…".into() } else { "–".into() }),
                        if audio || w <= 0 {
                            "–".into()
                        } else {
                            format!("{w} x {h}")
                        },
                        if audio || fps <= 0.0 {
                            "–".into()
                        } else {
                            format!("{fps:.2}")
                        },
                        d.and_then(|d| d.size).map_or_else(|| "–".into(), fmt_size),
                        kind_of(&f.path).into(),
                        f.clips.to_string(),
                        f.path.clone(),
                    ];
                    let color = if f.exists { pal.text } else { pal.danger };
                    let mut x = r.left() + 18.0;
                    for (i, text) in cells.iter().enumerate() {
                        let cell = egui::Rect::from_min_size(egui::pos2(x, r.top()), vec2(widths[i] - 8.0, row_h));
                        let g = p.layout_no_wrap(text.clone(), egui::FontId::proportional(13.0), color);
                        p.with_clip_rect(cell.intersect(r)).galley(
                            egui::pos2(x + 6.0, r.center().y - g.size().y / 2.0),
                            g,
                            color,
                        );
                        x += widths[i];
                    }
                    let resp = resp.on_hover_text(&f.path);
                    if resp.clicked() {
                        m.selected = Some(f.path.clone());
                        if m.replace_from.is_empty() || !f.exists {
                            m.replace_from = parent_of(&f.path);
                        }
                    }
                    if resp.double_clicked() && !f.exists && m.picker.is_none() {
                        let start = Path::new(&parent_of(&f.path)).to_path_buf();
                        m.picker = Some((Pick::File(f.path.clone()), pick(false, Some(start))));
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(parent_of("C:\\media\\AV\\a.mov"), "C:\\media\\AV");
        assert_eq!(parent_of("/mnt/a/b.mov"), "/mnt/a");
        assert_eq!(kind_of("/x/a.WAV"), "Audio");
        assert_eq!(kind_of("/x/a.png"), "Image");
        assert_eq!(kind_of("/x/a.mov"), "Video");
        assert_eq!(fmt_size(31_400_000), "29.9MB");
        assert_eq!(fmt_size(3 * 1_073_741_824), "3.0GB");
    }
}
