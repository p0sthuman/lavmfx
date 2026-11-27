//! Help menu and the About window: links to the project's pages (read me,
//! what's new in this version, releases, problem reports), system info for
//! bug reports, the settings folder, and version / licence details.

use eframe::egui::{self, RichText, vec2};

use super::{LavmfxApp, theme};

/// The project's home on the web (from the workspace's `repository`).
pub const REPO: &str = env!("CARGO_PKG_REPOSITORY");
/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// "<git commit> <UTC build time>" (see `build.rs`).
pub const BUILD: &str = env!("LAVMFX_BUILD");

/// The pages the Help menu opens.
pub fn readme_url() -> String {
    format!("{REPO}#readme")
}
/// Release notes of this version (the CHANGELOG at its tag).
pub fn changelog_url() -> String {
    format!("{REPO}/blob/v{VERSION}/CHANGELOG.md")
}
pub fn releases_url() -> String {
    format!("{REPO}/releases")
}
pub fn issues_url() -> String {
    format!("{REPO}/issues/new")
}

/// Open a web page or a folder with the system's default app.
pub fn open_external(target: impl AsRef<std::ffi::OsStr>) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(cmd).arg(target.as_ref()).spawn() {
        log::warn!("can't run {cmd}: {e}");
    }
}

/// Version, system and GPU, for a problem report.
pub fn system_info(gpu: &str) -> String {
    format!(
        "LAVMFX {VERSION} (build {BUILD})\nSystem: {} {}\nGPU: {gpu}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// What this build can play and share on this system.
pub fn platform_info() -> String {
    use lavmfx_io::platform;
    format!(
        "Texture sharing: {}\nPlugins: FFGL .{}\nVideo: {}\nAudio: {}",
        platform::TEXTURE_SHARE_NAME,
        platform::PLUGIN_EXTENSION,
        lavmfx_media::VIDEO_EXTENSIONS.join(", "),
        lavmfx_media::AUDIO_EXTENSIONS.join(", "),
    )
}

impl LavmfxApp {
    pub(super) fn help_menu(&mut self, ui: &mut egui::Ui) {
        let link = |ui: &mut egui::Ui, label: &str, hover: &str, url: String| {
            if ui.button(label).on_hover_text(format!("{hover}\n{url}")).clicked() {
                open_external(&url);
                ui.close();
            }
        };
        link(
            ui,
            "Read Me (online)",
            "What LAVMFX does and how to run it",
            readme_url(),
        );
        link(ui, "What's New", "Release notes of this version", changelog_url());
        link(ui, "Releases", "Downloads of every version", releases_url());
        link(
            ui,
            "Report a Problem…",
            "Open a new issue (use Copy System Info first)",
            issues_url(),
        );
        ui.separator();
        if ui
            .button("Copy System Info")
            .on_hover_text("Version, system and GPU, to paste into a problem report")
            .clicked()
        {
            ui.ctx().copy_text(system_info(&self.gpu_summary));
            self.status = "System info copied".into();
            ui.close();
        }
        if let Some(dir) = crate::app_dirs().map(|d| d.config_dir().to_path_buf())
            && ui
                .button("Show Settings Folder")
                .on_hover_text(format!(
                    "Preferences, layouts and presets{}\n{}",
                    if cfg!(windows) {
                        "; lavmfx.log is in the data folder next to it"
                    } else {
                        ""
                    },
                    dir.display()
                ))
                .clicked()
        {
            let _ = std::fs::create_dir_all(&dir);
            open_external(&dir);
            ui.close();
        }
        ui.separator();
        if ui.button("About LAVMFX…").clicked() {
            self.about_open = true;
            ui.close();
        }
    }

    pub(super) fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = self.about_open;
        egui::Window::new("About LAVMFX")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .fixed_size(vec2(440.0, 0.0))
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                let pal = theme::pal(ui);
                ui.vertical_centered(|ui| {
                    ui.add_space(4.0);
                    ui.label(RichText::new("LAVMFX").size(24.0).strong().color(pal.accent));
                    ui.label(
                        RichText::new("Live Audio Visual Mapping FX Mixer \u{b7} say \u{201c}love-me-FX\u{201d}")
                            .weak(),
                    );
                    ui.add_space(6.0);
                    ui.label(format!("Version {VERSION}"));
                    ui.label(RichText::new(format!("build {BUILD}")).weak().small());
                });
                ui.add_space(8.0);
                ui.separator();
                ui.add(
                    egui::Label::new(
                        "Copyright (C) 2026 p0sthuman and contributors. LAVMFX is free software under the \
                         GNU General Public License, version 3 or later, and comes with no warranty.",
                    )
                    .wrap(),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "The packages bundle FFmpeg (GPL build) and other open-source libraries; their \
                         licences are in the licenses folder next to the app.",
                        )
                        .weak(),
                    )
                    .wrap(),
                );
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.link("Source code").on_hover_text(REPO).clicked() {
                        open_external(REPO);
                    }
                    ui.label(RichText::new("\u{b7}").weak());
                    if ui.link("What's New").on_hover_text(changelog_url()).clicked() {
                        open_external(changelog_url());
                    }
                    ui.label(RichText::new("\u{b7}").weak());
                    if ui.link("Report a Problem").on_hover_text(issues_url()).clicked() {
                        open_external(issues_url());
                    }
                });
                ui.add_space(4.0);
                ui.separator();
                ui.add(egui::Label::new(RichText::new(platform_info()).weak().small()).wrap());
                ui.add(egui::Label::new(RichText::new(format!("GPU: {}", self.gpu_summary)).weak().small()).wrap());
                ui.add_space(4.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_sized(vec2(110.0, 22.0), egui::Button::new("Copy System Info"))
                        .clicked()
                    {
                        ui.ctx().copy_text(system_info(&self.gpu_summary));
                        self.status = "System info copied".into();
                    }
                });
            });
        self.about_open = open && self.about_open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_point_at_the_project() {
        assert!(REPO.starts_with("https://github.com/"), "{REPO}");
        assert!(changelog_url().ends_with(&format!("/blob/v{VERSION}/CHANGELOG.md")));
        assert!(issues_url().ends_with("/issues/new") && releases_url().ends_with("/releases"));
        let info = system_info("Test GPU");
        assert!(info.contains(VERSION) && info.contains("GPU: Test GPU"));
        assert!(!platform_info().contains("(M"), "no milestone labels in the UI");
    }
}
