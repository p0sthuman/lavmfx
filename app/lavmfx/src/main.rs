// Release builds on Windows are GUI apps (no console window behind them).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod audio;
mod dirs;
mod dmx;
mod live_out;
mod output;
mod playback;
mod thumbs;
mod waves;

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup};

/// Debug hook `LAVMFX_PRESENT=vsync|novsync|fifo|mailbox|immediate`: how frames
/// reach the screen (default vsync), for testing output-window problems.
///
/// Drivers offer different modes (NVIDIA on Linux has no Mailbox), and asking
/// for a missing one stops wgpu at startup. So the app starts with vsync, the
/// adapter choice notes which modes the window supports, and `apply` switches
/// on the first frame only when the asked-for mode is one of them.
pub(crate) mod present {
    use std::sync::OnceLock;

    use eframe::wgpu::{self, PresentMode};

    static SUPPORTED: OnceLock<Vec<PresentMode>> = OnceLock::new();

    pub fn parse(v: &str) -> Option<PresentMode> {
        match v.to_ascii_lowercase().as_str() {
            "novsync" => Some(PresentMode::AutoNoVsync),
            "fifo" => Some(PresentMode::Fifo),
            "mailbox" => Some(PresentMode::Mailbox),
            "immediate" => Some(PresentMode::Immediate),
            "vsync" => Some(PresentMode::AutoVsync),
            _ => None,
        }
    }

    pub fn requested() -> Option<PresentMode> {
        let v = std::env::var("LAVMFX_PRESENT").ok()?;
        let m = parse(&v);
        if m.is_none() {
            log::warn!("LAVMFX_PRESENT={v}: use vsync, novsync, fifo, mailbox or immediate");
        }
        m
    }

    /// Whether `m` can be used: the Auto modes always can (wgpu picks a mode
    /// the surface has); unknown support (no surface seen) counts as no.
    pub fn usable(m: PresentMode, supported: Option<&[PresentMode]>) -> bool {
        matches!(m, PresentMode::AutoVsync | PresentMode::AutoNoVsync) || supported.is_some_and(|s| s.contains(&m))
    }

    /// Adapter choice while LAVMFX_PRESENT is set: like wgpu's own (one that
    /// can draw to the window, by power preference), noting the window's
    /// present modes on the way.
    pub fn pick(
        adapters: &[wgpu::Adapter],
        surface: Option<&wgpu::Surface<'_>>,
        power: wgpu::PowerPreference,
    ) -> Result<wgpu::Adapter, String> {
        use wgpu::DeviceType::*;
        let rank = |t: wgpu::DeviceType| match (power, t) {
            (wgpu::PowerPreference::LowPower, IntegratedGpu) => 0,
            (wgpu::PowerPreference::LowPower, DiscreteGpu) => 1,
            (_, DiscreteGpu) => 0,
            (_, IntegratedGpu) => 1,
            (_, VirtualGpu) => 2,
            (_, Cpu) => 3,
            (_, Other) => 4,
        };
        let adapter = adapters
            .iter()
            .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
            .min_by_key(|a| rank(a.get_info().device_type))
            .cloned()
            .ok_or_else(|| "no graphics adapter can draw to the window".to_string())?;
        if let Some(s) = surface {
            let _ = SUPPORTED.set(s.get_capabilities(&adapter).present_modes);
        }
        Ok(adapter)
    }

    /// Switches to the asked-for mode once, if the window supports it.
    pub fn apply(frame: &mut eframe::Frame) {
        static DONE: OnceLock<()> = OnceLock::new();
        if DONE.set(()).is_err() {
            return;
        }
        let (Some(m), Some(mut config)) = (requested(), frame.wgpu_surface_config()) else {
            return;
        };
        let supported = SUPPORTED.get().map(Vec::as_slice);
        if usable(m, supported) {
            log::info!("present mode {m:?} (LAVMFX_PRESENT)");
            config.present_mode = m;
            frame.set_wgpu_surface_config(config);
        } else {
            log::warn!("LAVMFX_PRESENT: this driver has no {m:?} (it offers {supported:?}); staying with vsync");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn missing_modes_fall_back() {
            assert_eq!(parse("Mailbox"), Some(PresentMode::Mailbox));
            assert_eq!(parse("tearing"), None);
            let nvidia = [PresentMode::Fifo, PresentMode::Immediate];
            assert!(!usable(PresentMode::Mailbox, Some(&nvidia)));
            assert!(usable(PresentMode::Immediate, Some(&nvidia)));
            assert!(usable(PresentMode::AutoNoVsync, None));
            assert!(!usable(PresentMode::Fifo, None));
        }
    }
}

/// The app's settings / data folders: the system's per-user folders, or a
/// portable folder next to the program (see `dirs`).
pub(crate) fn app_dirs() -> Option<&'static dirs::AppDirs> {
    dirs::get()
}

/// Log to stderr; Windows release builds have no console, so they write
/// `lavmfx.log` in the data folder (`%APPDATA%\lavmfx\data`) instead.
fn init_logging() {
    let mut b = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,wgpu_core=warn,wgpu_hal=warn"),
    );
    if cfg!(all(windows, not(debug_assertions)))
        && let Some(dir) = app_dirs().map(|d| d.data_dir().to_path_buf())
        && std::fs::create_dir_all(&dir).is_ok()
        && let Ok(f) = std::fs::File::create(dir.join("lavmfx.log"))
    {
        b.target(env_logger::Target::Pipe(Box::new(f)));
    }
    b.init();
    // A crash (panic) goes to the log too: Windows release builds have no
    // console, so the message would be lost otherwise.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        log::error!("crash in thread '{}': {info}", thread.name().unwrap_or("?"));
        default_hook(info);
    }));
    log::info!(
        "LAVMFX {} ({} {}, build {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        env!("LAVMFX_BUILD")
    );
    match app_dirs() {
        Some(d) if d.portable_root().is_some() => log::info!("portable: settings in {}", d.config_dir().display()),
        Some(d) => log::info!("settings in {}", d.config_dir().display()),
        None => log::warn!("no settings folder found; using the current folder"),
    }
}

fn main() -> eframe::Result {
    init_logging();

    prefer_x11_for_drag_and_drop();

    // Usage: lavmfx [--output] [composition.lavmfx | video files…]
    // Several videos go one per layer into column 1, which is launched.
    let mut open_paths = Vec::new();
    let mut show_output = false;
    for arg in std::env::args_os().skip(1) {
        if arg == "--output" {
            show_output = true;
        } else {
            open_paths.push(PathBuf::from(arg));
        }
    }

    // Request BC texture compression when the GPU has it (HAP/DXV in M1).
    let mut wgpu_options = WgpuConfiguration::default();
    if let WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        let base = setup.device_descriptor.clone();
        setup.device_descriptor = Arc::new(move |adapter| {
            let mut desc = base(adapter);
            desc.label = Some("lavmfx device");
            desc.required_features |= adapter.features() & lavmfx_render::WANTED_FEATURES;
            desc.required_limits.max_texture_dimension_2d = adapter.limits().max_texture_dimension_2d.min(16384);
            desc
        });
    }

    // LAVMFX_PRESENT: start with vsync (every driver has it); `present::apply`
    // switches once the window's supported modes are known.
    if let (Some(m), WgpuSetup::CreateNew(setup)) = (present::requested(), &mut wgpu_options.wgpu_setup) {
        log::info!("present mode {m:?} asked for (LAVMFX_PRESENT)");
        let power = setup.power_preference;
        setup.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
            present::pick(adapters, surface, power)
        }));
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("LAVMFX")
            .with_app_id("lavmfx")
            .with_icon(eframe::egui::IconData {
                // 128×128 RGBA, rendered from assets/lavmfx.svg (packaging/make-icons.sh).
                rgba: include_bytes!("../assets/icon-128.rgba").to_vec(),
                width: 128,
                height: 128,
            })
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([800.0, 500.0]),
        wgpu_options,
        ..Default::default()
    };

    eframe::run_native(
        "LAVMFX",
        options,
        Box::new(move |cc| Ok(Box::new(app::LavmfxApp::new(cc, open_paths, show_output)?))),
    )
}

/// winit 0.30 only delivers file drag-and-drop on X11, so on a Wayland
/// session we run through XWayland unless `LAVMFX_WAYLAND=1` is set.
fn prefer_x11_for_drag_and_drop() {
    #[cfg(target_os = "linux")]
    {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let x11 = std::env::var_os("DISPLAY").is_some();
        let keep = std::env::var("LAVMFX_WAYLAND").is_ok_and(|v| v != "0");
        if wayland && x11 && !keep {
            // SAFETY: called first thing in main, before any other thread exists.
            unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
            log::info!("using X11 (XWayland) for file drag-and-drop; LAVMFX_WAYLAND=1 for native Wayland");
        }
    }
}
