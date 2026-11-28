//! Where LAVMFX keeps its settings, compositions and log.
//!
//! - Installed (default): the system's per-user folders: `~/.config/lavmfx`
//!   and `~/.local/share/lavmfx` on Linux, `%APPDATA%\lavmfx\config` and
//!   `…\data` on Windows, `~/Library/Application Support/lavmfx` on macOS.
//! - Portable: when a folder named [`PORTABLE_FOLDER`] sits next to the
//!   program, everything goes in there instead (`config/`, `data/`,
//!   `recordings/`), so the program folder can be copied to another computer
//!   with its whole setup. For an AppImage the folder sits next to the
//!   `.AppImage` file, for a macOS app next to the `.app`.
//! - `LAVMFX_HOME=<folder>` uses that folder the same way (tests, several
//!   setups side by side). It wins over both.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The folder next to the program that turns on portable mode.
pub const PORTABLE_FOLDER: &str = "lavmfx-data";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDirs {
    config: PathBuf,
    data: PathBuf,
    /// Portable mode (folder next to the program, or `LAVMFX_HOME`): its root.
    portable: Option<PathBuf>,
}

impl AppDirs {
    /// Preferences, layouts, presets, fixtures.
    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    /// Compositions (default save folder) and, on Windows, the log.
    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    /// The portable folder, when in portable mode.
    pub fn portable_root(&self) -> Option<&Path> {
        self.portable.as_deref()
    }

    fn portable(root: PathBuf) -> Self {
        Self {
            config: root.join("config"),
            data: root.join("data"),
            portable: Some(root),
        }
    }
}

/// The folders for this run (decided once).
pub fn get() -> Option<&'static AppDirs> {
    static DIRS: OnceLock<Option<AppDirs>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let appimage = if cfg!(target_os = "linux") {
            std::env::var_os("APPIMAGE")
        } else {
            None
        };
        resolve(
            std::env::var_os("LAVMFX_HOME"),
            std::env::current_exe().ok().as_deref(),
            appimage.as_deref(),
            installed,
        )
    })
    .as_ref()
}

/// The system's per-user folders.
fn installed() -> Option<AppDirs> {
    let p = directories::ProjectDirs::from("", "", "lavmfx")?;
    Some(AppDirs {
        config: p.config_dir().to_owned(),
        data: p.data_dir().to_owned(),
        portable: None,
    })
}

fn resolve(
    home: Option<OsString>,
    program: Option<&Path>,
    appimage: Option<&OsStr>,
    installed: impl FnOnce() -> Option<AppDirs>,
) -> Option<AppDirs> {
    if let Some(home) = home.filter(|h| !h.is_empty()) {
        return Some(AppDirs::portable(PathBuf::from(home)));
    }
    if let Some(root) = portable_parent(program, appimage).map(|d| d.join(PORTABLE_FOLDER))
        && root.is_dir()
    {
        return Some(AppDirs::portable(root));
    }
    installed()
}

/// The folder the portable folder would sit in.
fn portable_parent(program: Option<&Path>, appimage: Option<&OsStr>) -> Option<PathBuf> {
    // An AppImage runs from a read-only mount: look next to the .AppImage file.
    if let Some(file) = appimage.filter(|a| !a.is_empty()) {
        return Path::new(file).parent().map(Path::to_path_buf);
    }
    let dir = program?.parent()?;
    // macOS bundle (`X.app/Contents/MacOS/lavmfx`): next to `X.app`, not inside it.
    if dir.ends_with("Contents/MacOS")
        && let Some(bundle) = dir.parent().and_then(Path::parent)
        && bundle.extension().is_some_and(|e| e == "app")
    {
        return bundle.parent().map(Path::to_path_buf);
    }
    Some(dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed() -> Option<AppDirs> {
        Some(AppDirs {
            config: "/sys-config".into(),
            data: "/sys-data".into(),
            portable: None,
        })
    }

    #[test]
    fn installed_unless_a_portable_folder_exists() {
        let tmp = std::env::temp_dir().join(format!("lavmfx-dirs-{}", std::process::id()));
        let program = tmp.join("lavmfx");
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(resolve(None, Some(&program), None, installed), installed());

        std::fs::create_dir_all(tmp.join(PORTABLE_FOLDER)).unwrap();
        let dirs = resolve(None, Some(&program), None, installed).unwrap();
        assert_eq!(dirs.portable_root(), Some(tmp.join(PORTABLE_FOLDER).as_path()));
        assert_eq!(dirs.config_dir(), tmp.join(PORTABLE_FOLDER).join("config"));
        assert_eq!(dirs.data_dir(), tmp.join(PORTABLE_FOLDER).join("data"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn lavmfx_home_wins() {
        let dirs = resolve(Some("/stick/setup".into()), None, None, installed).unwrap();
        assert_eq!(dirs.config_dir(), Path::new("/stick/setup/config"));
        // Empty = not set.
        assert_eq!(resolve(Some("".into()), None, None, installed), installed());
    }

    #[test]
    fn appimage_and_app_bundle_look_beside_the_file() {
        let image = OsStr::new("/home/u/Apps/lavmfx.AppImage");
        assert_eq!(
            portable_parent(Some(Path::new("/tmp/.mount_x/usr/bin/lavmfx")), Some(image)),
            Some(PathBuf::from("/home/u/Apps"))
        );
        assert_eq!(
            portable_parent(Some(Path::new("/Volumes/Stick/LAVMFX.app/Contents/MacOS/lavmfx")), None),
            Some(PathBuf::from("/Volumes/Stick"))
        );
        assert_eq!(
            portable_parent(Some(Path::new("/opt/lavmfx/lavmfx")), None),
            Some(PathBuf::from("/opt/lavmfx"))
        );
    }
}
