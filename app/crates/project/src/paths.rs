//! Resolve media paths saved in `.avc` files on Windows / Wine (`D:\VJ Content\x.mov`)
//! to files on this machine. Cheap by design: a few `stat`s and one-level
//! directory listings per path, never a recursive scan, never a copy.
//!
//! Order (first existing file wins):
//! 1. Linux absolute paths as saved.
//! 2. Wine drive letters: every prefix's `dosdevices/<x>:` symlink
//!    (`$WINEPREFIX`, `~/.wine`, Bottles bottles).
//! 3. Guessed drive roots: the mount points (`/mnt/*`, `/media/*`,
//!    `/run/media/$USER/*`, …) that contain the path's first folder
//!    (`D:\VJ Content\…` → `/mnt/local` when `/mnt/local/VJ Content` exists).
//!    Cached per (drive, first folder).
//! 4. Next to the `.avc` file (and its parent folder), by relative path or name.
//!
//! Every step matches names case-insensitively (Windows ignores case).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Where a saved path points now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The file found, or (when `!exists`) the saved path unchanged.
    pub path: PathBuf,
    pub exists: bool,
}

pub struct PathResolver {
    /// Folder of the `.avc`, for relative paths and "next to the file".
    base: Option<PathBuf>,
    /// Wine drive letter (lower case) → folders it maps to (several prefixes).
    drives: HashMap<char, Vec<PathBuf>>,
    /// Mount points that may hold an old Windows drive.
    roots: Vec<PathBuf>,
    /// (drive, first folder lower-cased) → guessed drive root.
    guesses: HashMap<(char, String), Option<PathBuf>>,
}

/// A Windows path split up: drive letter (if any) and its components.
fn split_windows(saved: &str) -> (Option<char>, Vec<&str>) {
    let b = saved.as_bytes();
    let (drive, rest) = if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        (Some((b[0] as char).to_ascii_lowercase()), &saved[2..])
    } else {
        (None, saved)
    };
    let parts = rest.split(['\\', '/']).filter(|p| !p.is_empty() && *p != ".").collect();
    (drive, parts)
}

/// `root/parts…` matching each name case-insensitively when the exact name
/// is missing. `None` if some component doesn't exist.
fn join_ci(root: &Path, parts: &[&str]) -> Option<PathBuf> {
    let mut cur = root.to_path_buf();
    for part in parts {
        let exact = cur.join(part);
        if exact.exists() {
            cur = exact;
            continue;
        }
        let want = part.to_lowercase();
        let found = std::fs::read_dir(&cur)
            .ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().to_lowercase() == want)?;
        cur = found.path();
    }
    cur.exists().then_some(cur)
}

/// Like [`join_ci`], but only a file counts (media paths never name folders).
fn file_ci(root: &Path, parts: &[&str]) -> Option<PathBuf> {
    if parts.is_empty() {
        return None;
    }
    join_ci(root, parts).filter(|p| p.is_file())
}

/// Known Wine prefixes: `$WINEPREFIX`, `~/.wine`, and every Bottles bottle.
fn wine_prefixes() -> Vec<PathBuf> {
    let mut prefixes = Vec::new();
    if let Some(p) = std::env::var_os("WINEPREFIX") {
        prefixes.push(PathBuf::from(p));
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        prefixes.push(home.join(".wine"));
        for bottles in [
            home.join(".var/app/com.usebottles.bottles/data/bottles/bottles"),
            home.join(".local/share/bottles/bottles"),
        ] {
            prefixes.extend(subdirs(&bottles));
        }
    }
    prefixes
}

/// Preferences folders of an installed `.avc` app that exist: under Windows'
/// `Documents`, and for every user of every known Wine prefix.
pub fn import_prefs_dirs() -> Vec<PathBuf> {
    const PREFS: [&str; 3] = ["Documents", "Resolume Arena", "Preferences"];
    let mut homes: Vec<PathBuf> = std::env::var_os("USERPROFILE").map(PathBuf::from).into_iter().collect();
    for prefix in wine_prefixes() {
        homes.extend(subdirs(&prefix.join("drive_c/users")));
    }
    homes
        .iter()
        .filter_map(|h| join_ci(h, &PREFS))
        .filter(|p| p.is_dir())
        .collect()
}

/// Wine drive mappings of all known prefixes.
pub fn wine_drives() -> HashMap<char, Vec<PathBuf>> {
    let prefixes = wine_prefixes();
    let mut drives: HashMap<char, Vec<PathBuf>> = HashMap::new();
    for prefix in prefixes {
        let dos = prefix.join("dosdevices");
        for e in std::fs::read_dir(&dos).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            // "d:" is a folder mapping; "d::" is a raw device (skip).
            let b = name.as_bytes();
            if b.len() != 2 || b[1] != b':' || !b[0].is_ascii_alphabetic() {
                continue;
            }
            let Ok(target) = std::fs::canonicalize(e.path()) else {
                continue;
            };
            if target.is_dir() {
                let v = drives.entry(b[0] as char).or_default();
                if !v.contains(&target) {
                    v.push(target);
                }
            }
        }
    }
    drives
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

/// Mount points an old Windows drive may live under now.
pub fn mount_roots() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let user = std::env::var("USER").unwrap_or_default();
    for dir in [
        "/mnt".to_owned(),
        "/media".to_owned(),
        format!("/media/{user}"),
        format!("/run/media/{user}"),
    ] {
        v.extend(subdirs(Path::new(&dir)));
    }
    if let Some(home) = std::env::var_os("HOME") {
        v.push(PathBuf::from(home));
    }
    v
}

impl PathResolver {
    /// Resolver for an `.avc` at `avc` using this machine's Wine prefixes and mounts.
    pub fn new(avc: Option<&Path>) -> Self {
        Self::with(avc.and_then(Path::parent), wine_drives(), mount_roots())
    }

    pub fn with(base: Option<&Path>, drives: HashMap<char, Vec<PathBuf>>, roots: Vec<PathBuf>) -> Self {
        Self {
            base: base.map(Path::to_path_buf),
            drives,
            roots,
            guesses: HashMap::new(),
        }
    }

    /// Drive roots guessed so far (drive letter → folder), for the import report.
    pub fn guessed_drives(&self) -> Vec<(char, PathBuf)> {
        let mut v: Vec<(char, PathBuf)> = self
            .guesses
            .iter()
            .filter_map(|((d, _), r)| Some((d.to_ascii_uppercase(), r.clone()?)))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    pub fn resolve(&mut self, saved: &str) -> Resolved {
        let found = |p: PathBuf| Resolved { path: p, exists: true };
        if saved.starts_with('/') {
            let parts: Vec<&str> = saved.split('/').filter(|p| !p.is_empty()).collect();
            if let Some(p) = file_ci(Path::new("/"), &parts) {
                return found(p);
            }
        }
        let (drive, parts) = split_windows(saved);
        if let Some(d) = drive
            && !parts.is_empty()
        {
            for root in self.drives.get(&d).cloned().unwrap_or_default() {
                if let Some(p) = file_ci(&root, &parts) {
                    return found(p);
                }
            }
            if let Some(root) = self.guess_root(d, parts[0])
                && let Some(p) = file_ci(&root, &parts)
            {
                return found(p);
            }
        }
        if let Some(base) = self.base.clone() {
            let name = parts.last().copied();
            let mut tries: Vec<(PathBuf, Vec<&str>)> = Vec::new();
            if drive.is_none() && !saved.starts_with('/') {
                tries.push((base.clone(), parts.clone()));
            }
            if let Some(n) = name {
                tries.push((base.clone(), vec![n]));
                if let Some(parent) = base.parent() {
                    tries.push((parent.to_path_buf(), vec![n]));
                }
            }
            for (root, rel) in tries {
                if let Some(p) = file_ci(&root, &rel) {
                    return found(p);
                }
            }
        }
        Resolved {
            path: PathBuf::from(saved),
            exists: false,
        }
    }

    /// Mount root holding `first` (the path's first folder) for drive `d`.
    fn guess_root(&mut self, d: char, first: &str) -> Option<PathBuf> {
        let key = (d, first.to_lowercase());
        if let Some(g) = self.guesses.get(&key) {
            return g.clone();
        }
        let g = self.roots.iter().find(|r| join_ci(r, &[first]).is_some()).cloned();
        self.guesses.insert(key, g.clone());
        g
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temp tree: wine C: → <t>/prefix/drive_c, old D: drive mounted at <t>/mnt/local.
    fn tree() -> (PathBuf, PathResolver) {
        let t = std::env::temp_dir().join(format!("lavmfx-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let mk = |p: &str| {
            let f = t.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, b"x").unwrap();
        };
        mk("prefix/drive_c/users/me/Videos/a.mov");
        mk("mnt/local/VJ Content/DXV/Loop One.mov");
        mk("mnt/other/stuff/b.mov");
        mk("show/next.mov");
        mk("show/sub/rel.mov");
        let drives = HashMap::from([('c', vec![t.join("prefix/drive_c")])]);
        let roots = vec![t.join("mnt/other"), t.join("mnt/local")];
        let r = PathResolver::with(Some(&t.join("show")), drives, roots);
        (t, r)
    }

    #[test]
    fn resolves_wine_guessed_relative_and_missing() {
        let (t, mut r) = tree();
        // Wine drive mapping.
        let a = r.resolve(r"C:\users\me\Videos\a.mov");
        assert_eq!(
            a,
            Resolved {
                path: t.join("prefix/drive_c/users/me/Videos/a.mov"),
                exists: true
            }
        );
        // Unmapped drive found on a mount point, case-insensitively.
        let b = r.resolve(r"D:\vj content\dxv\LOOP ONE.mov");
        assert!(b.exists, "{b:?}");
        // Canonical forms: where names ignore case (Windows), the path keeps the
        // saved spelling and still names the same file.
        assert_eq!(
            b.path.canonicalize().unwrap(),
            t.join("mnt/local/VJ Content/DXV/Loop One.mov").canonicalize().unwrap()
        );
        assert_eq!(r.guessed_drives(), vec![('D', t.join("mnt/local"))]);
        // Relative to the .avc, and same name next to it.
        assert!(r.resolve(r"sub\rel.mov").exists);
        assert_eq!(r.resolve(r"E:\gone\next.mov").path, t.join("show/next.mov"));
        // Missing: original path kept.
        let m = r.resolve(r"D:\VJ Content\DXV\nope.mov");
        assert_eq!(
            m,
            Resolved {
                path: PathBuf::from(r"D:\VJ Content\DXV\nope.mov"),
                exists: false
            }
        );
        // Device entries / raw text don't panic.
        assert!(!r.resolve("").exists);
        assert!(!r.resolve("D:").exists);
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn splits_windows_paths() {
        assert_eq!(split_windows(r"D:\a\b.mov"), (Some('d'), vec!["a", "b.mov"]));
        assert_eq!(split_windows(r"rel\x.mov"), (None, vec!["rel", "x.mov"]));
        assert_eq!(
            split_windows("z:/home/u/x.mov"),
            (Some('z'), vec!["home", "u", "x.mov"])
        );
    }
}
