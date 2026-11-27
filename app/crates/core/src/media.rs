//! Media Manager: the files a composition's clips use (every deck), and
//! relinking them when they moved (one file, a folder search by file name, or
//! a path prefix swap).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::model::{Clip, Composition, VideoFileInfo};

/// One file and how the composition uses it.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaFile {
    pub path: String,
    /// Clips using it (all decks).
    pub clips: usize,
    /// As last checked (`Composition::check_media`).
    pub exists: bool,
    /// Size, frame rate and length the clips remember (also for missing files).
    pub width: i32,
    pub height: i32,
    pub fps: f64,
    pub duration: String,
}

impl Composition {
    /// Every clip of every deck (the active deck's and the stashed ones).
    pub fn all_clips_mut(&mut self) -> impl Iterator<Item = &mut Clip> {
        let active = self.layers.iter_mut().flat_map(|l| l.clips.iter_mut());
        let stashed = self
            .decks
            .iter_mut()
            .filter_map(|d| d.stash.as_mut())
            .flat_map(|s| s.clips.iter_mut().flatten());
        active.chain(stashed)
    }

    pub fn all_clips(&self) -> impl Iterator<Item = &Clip> {
        let active = self.layers.iter().flat_map(|l| l.clips.iter());
        let stashed = self
            .decks
            .iter()
            .filter_map(|d| d.stash.as_ref())
            .flat_map(|s| s.clips.iter().flatten());
        active.chain(stashed)
    }

    /// The files clips play, by path (sorted), with their use count.
    pub fn media_files(&self) -> Vec<MediaFile> {
        let mut by: BTreeMap<&str, MediaFile> = BTreeMap::new();
        for c in self.all_clips() {
            let Some(f) = c.video.as_ref().and_then(|v| v.fileinfo.as_ref()) else {
                continue;
            };
            by.entry(f.path.as_str())
                .or_insert_with(|| MediaFile {
                    path: f.path.clone(),
                    clips: 0,
                    exists: f.exists,
                    width: f.width,
                    height: f.height,
                    fps: if f.framerate.denom > 0 {
                        f.framerate.num as f64 / f.framerate.denom as f64
                    } else {
                        0.0
                    },
                    duration: f.duration.clone(),
                })
                .clips += 1;
        }
        by.into_values().collect()
    }

    /// Store whether each file exists (`exists(path)`); returns how many changed.
    pub fn check_media(&mut self, exists: &HashMap<String, bool>) -> usize {
        let mut n = 0;
        for f in self.all_clips_mut().filter_map(|c| c.video.as_mut()?.fileinfo.as_mut()) {
            if let Some(&e) = exists.get(&f.path)
                && f.exists != e
            {
                f.exists = e;
                n += 1;
            }
        }
        n
    }

    /// Point every clip using `old` at `new` (marked found). `info` (from a
    /// probe of `new`) replaces size / rate / length when the new file differs.
    /// Returns the number of clips changed.
    pub fn relink(&mut self, old: &str, new: &str, info: Option<&VideoFileInfo>) -> usize {
        let mut n = 0;
        for c in self.all_clips_mut() {
            let Some(f) = c.video.as_mut().and_then(|v| v.fileinfo.as_mut()) else {
                continue;
            };
            if f.path != old {
                continue;
            }
            match info {
                Some(i) => {
                    *f = VideoFileInfo {
                        path: new.to_owned(),
                        exists: true,
                        ..i.clone()
                    }
                }
                None => {
                    f.path = new.to_owned();
                    f.exists = true;
                }
            }
            if let Some(a) = c.audio.as_mut().and_then(|a| a.fileinfo.as_mut())
                && a.path == old
            {
                a.path = new.to_owned();
                a.exists = true;
            }
            n += 1;
        }
        n
    }

    /// Paths starting with `old_prefix` start with `new_prefix` instead, when
    /// the new path exists (`exists`). Returns (files relinked, files whose new
    /// path doesn't exist and were left alone).
    pub fn replace_prefix(
        &mut self,
        old_prefix: &str,
        new_prefix: &str,
        exists: impl Fn(&str) -> bool,
    ) -> (usize, usize) {
        if old_prefix.is_empty() {
            return (0, 0);
        }
        let (mut ok, mut skipped) = (0, 0);
        for f in self.media_files() {
            let Some(rest) = f.path.strip_prefix(old_prefix) else {
                continue;
            };
            let new = join_prefix(new_prefix, rest);
            if exists(&new) {
                self.relink(&f.path, &new, None);
                ok += 1;
            } else {
                skipped += 1;
            }
        }
        (ok, skipped)
    }
}

/// `prefix` + `rest` with one separator between them. A Unix `prefix`
/// turns the Windows separators of `rest` into `/` (`.avc` paths from Windows).
fn join_prefix(prefix: &str, rest: &str) -> String {
    let unix = prefix.starts_with('/');
    let rest = if unix { rest.replace('\\', "/") } else { rest.to_owned() };
    let sep = if unix {
        "/"
    } else if prefix.contains('\\') {
        "\\"
    } else {
        "/"
    };
    let p = prefix.trim_end_matches(['/', '\\']);
    let r = rest.trim_start_matches(['/', '\\']);
    if r.is_empty() {
        return prefix.to_owned();
    }
    format!("{p}{sep}{r}")
}

/// File name of a stored path, whatever its separators (Windows paths from
/// `.avc` files on Linux too).
pub fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Index of the files under `dir` by lower-case file name (first found wins),
/// `max_files` at most (huge folders). Hidden folders are skipped.
pub fn index_folder(dir: &Path, max_files: usize) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let mut stack = vec![dir.to_path_buf()];
    let mut seen = 0;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                if !name.starts_with('.') {
                    stack.push(e.path());
                }
            } else {
                out.entry(name.to_lowercase()).or_insert_with(|| e.path());
                seen += 1;
                if seen >= max_files {
                    return out;
                }
            }
        }
    }
    out
}

/// Missing files found in `index` by file name: (old path, new path).
pub fn find_in(missing: &[String], index: &HashMap<String, PathBuf>) -> Vec<(String, String)> {
    missing
        .iter()
        .filter_map(|m| {
            let p = index.get(&file_name(m).to_lowercase())?;
            Some((m.clone(), p.to_string_lossy().into_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FrameRate;

    fn info(path: &str, exists: bool) -> VideoFileInfo {
        VideoFileInfo {
            path: path.into(),
            exists,
            duration: "00:10".into(),
            duration_ms: 10_000.0,
            framerate: FrameRate { num: 30, denom: 1 },
            width: 1280,
            height: 720,
        }
    }

    fn comp_with(paths: &[(&str, bool)]) -> Composition {
        let mut c = Composition::new("M", 2, 4);
        for (i, (p, e)) in paths.iter().enumerate() {
            let ids = &mut c.ids;
            c.layers[i % 2].clips[i / 2].load_video(ids, "x", info(p, *e));
        }
        c
    }

    #[test]
    fn lists_relinks_and_swaps_prefixes_on_every_deck() {
        let mut c = comp_with(&[
            ("C:\\media\\AV\\Beat 001.mov", false),
            ("C:\\media\\AV\\Beat 001.mov", false),
            ("/old/clips/b.mov", false),
            ("/ok/c.mov", true),
        ]);
        // A clip on a second deck counts too.
        c.add_deck("B");
        c.select_deck(1);
        let ids = &mut c.ids;
        c.layers[0].clips[0].load_video(ids, "y", info("/old/clips/sub/d.mov", false));
        c.select_deck(0);
        let files = c.media_files();
        assert_eq!(files.len(), 4);
        let beat = files.iter().find(|f| f.path.ends_with("Beat 001.mov")).unwrap();
        assert_eq!((beat.clips, beat.exists, beat.width), (2, false, 1280));
        assert_eq!(file_name(&beat.path), "Beat 001.mov");

        assert_eq!(c.relink(&beat.path.clone(), "/new/Beat 001.mov", None), 2);
        let (ok, skipped) = c.replace_prefix("/old/clips", "/new/clips", |p| p != "/new/clips/sub/d.mov");
        assert_eq!((ok, skipped), (1, 1));
        let files = c.media_files();
        assert!(files.iter().any(|f| f.path == "/new/Beat 001.mov" && f.exists));
        assert!(files.iter().any(|f| f.path == "/new/clips/b.mov" && f.exists));
        assert!(
            files.iter().any(|f| f.path == "/old/clips/sub/d.mov" && !f.exists),
            "left alone"
        );

        let mut found = HashMap::new();
        found.insert("/ok/c.mov".to_string(), false);
        assert_eq!(c.check_media(&found), 1);
        assert!(
            c.all_clips()
                .any(|cl| cl.is_missing() && cl.video_path() == Some("/ok/c.mov"))
        );
    }

    #[test]
    fn folder_search_finds_by_name() {
        let dir = std::env::temp_dir().join(format!("lavmfx-media-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("deep/er")).unwrap();
        std::fs::write(dir.join("deep/er/Beat 001.MOV"), b"x").unwrap();
        let idx = index_folder(&dir, 10_000);
        let hits = find_in(&["C:\\media\\AV\\Beat 001.mov".into(), "/x/none.mov".into()], &idx);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].1.ends_with("Beat 001.MOV"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prefixes_join_with_one_separator() {
        assert_eq!(join_prefix("/new", "/a.mov"), "/new/a.mov");
        assert_eq!(join_prefix("/new/", "a.mov"), "/new/a.mov");
        assert_eq!(join_prefix("/new", "a.mov"), "/new/a.mov");
        // A Windows path from an .avc moved to a Linux folder.
        let mut c = comp_with(&[("C:\\media\\AV\\Beat 001.mov", false)]);
        let (ok, _) = c.replace_prefix("C:\\media", "/mnt/local/media", |p| {
            p == "/mnt/local/media/AV/Beat 001.mov"
        });
        assert_eq!(ok, 1);
        assert!(
            c.all_clips()
                .any(|cl| cl.video_path() == Some("/mnt/local/media/AV/Beat 001.mov"))
        );
    }
}
