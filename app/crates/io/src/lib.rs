//! Platform-facing I/O behind traits.
//!
//! Everything OS-specific lives behind a trait here so the rest of the app
//! stays portable. Each trait has a `Null*` no-op implementation; real backends
//! arrive per milestone:
//!
//! | Trait            | Linux                     | Windows | macOS   | Milestone |
//! |------------------|---------------------------|---------|---------|-----------|
//! | [`TextureShare`] | PipeWire / v4l2loopback   | Spout   | Syphon  | M8        |
//! | [`PluginHost`]   | FFGL `.so`                | `.dll`  | `.bundle` | M8      |
//! | [`ControlInput`] | OSC (rosc), MIDI (midir)  | same    | same    | M7        |
//! | [`DmxOutput`]    | Art-Net / sACN            | same    | same    | M9        |

use std::path::{Path, PathBuf};

pub mod dmx;
pub mod ffgl;
pub mod link;
pub mod midi;
pub mod osc;
pub mod web;

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("{0} is not available on this platform yet")]
    Unsupported(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, IoError>;

// ------------------------------------------------------------ texture sharing

/// Pixel layout of a shared frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Rgba8,
    Bgra8,
}

/// Publishes rendered frames to other applications (Spout/Syphon/PipeWire/v4l2loopback).
///
/// M0 takes CPU frames; zero-copy GPU handles (dmabuf / DXGI / IOSurface) will
/// be added as a second method once the render graph exists.
pub trait TextureShare: Send {
    /// Backend name shown in the UI.
    fn backend(&self) -> &'static str;
    /// Start publishing under `name`.
    fn start(&mut self, name: &str, width: u32, height: u32, format: PixelFormat) -> Result<()>;
    /// Publish one frame (`width * height * 4` bytes).
    fn publish(&mut self, pixels: &[u8]) -> Result<()>;
    fn stop(&mut self);
}

pub struct NullTextureShare;

impl TextureShare for NullTextureShare {
    fn backend(&self) -> &'static str {
        "none"
    }
    fn start(&mut self, _: &str, _: u32, _: u32, _: PixelFormat) -> Result<()> {
        Err(IoError::Unsupported("texture sharing"))
    }
    fn publish(&mut self, _: &[u8]) -> Result<()> {
        Ok(())
    }
    fn stop(&mut self) {}
}

// ---------------------------------------------------------------- plugin host

/// Metadata of a discovered effect/source plugin (FFGL first).
#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub is_source: bool,
}

/// Discovers and loads native video plugins.
pub trait PluginHost: Send {
    /// File extension of loadable plugins on this OS.
    fn library_extension(&self) -> &'static str;
    /// Scan a directory for plugins.
    fn scan(&mut self, dir: &Path) -> Result<Vec<PluginInfo>>;
}

pub struct NullPluginHost;

impl PluginHost for NullPluginHost {
    fn library_extension(&self) -> &'static str {
        platform::PLUGIN_EXTENSION
    }
    fn scan(&mut self, _: &Path) -> Result<Vec<PluginInfo>> {
        Ok(Vec::new())
    }
}

// ------------------------------------------------------------- control input

/// A normalised control message from any input (OSC, MIDI, keyboard, REST).
#[derive(Debug, Clone, PartialEq)]
pub enum ControlMessage {
    /// OSC-style address with float arguments, e.g. `/composition/layers/1/clips/2/connect`.
    Address { path: String, args: Vec<f32> },
    /// Raw MIDI bytes.
    Midi { port: String, bytes: Vec<u8> },
}

/// Source of control messages. Implementations run their own thread and
/// deliver through `poll`.
pub trait ControlInput: Send {
    fn name(&self) -> &str;
    /// Drain pending messages without blocking.
    fn poll(&mut self, out: &mut Vec<ControlMessage>);
}

pub struct NullControlInput;

impl ControlInput for NullControlInput {
    fn name(&self) -> &str {
        "none"
    }
    fn poll(&mut self, _: &mut Vec<ControlMessage>) {}
}

// ----------------------------------------------------------------- DMX output

/// Sends DMX universes (Art-Net / sACN: [`dmx::DmxSender`]).
pub trait DmxOutput: Send {
    /// Send one universe (up to 512 channels) to `to`.
    fn send_universe(&mut self, to: &dmx::DmxTarget, universe: u16, data: &[u8]) -> Result<()>;
}

pub struct NullDmxOutput;

impl DmxOutput for NullDmxOutput {
    fn send_universe(&mut self, _: &dmx::DmxTarget, _: u16, _: &[u8]) -> Result<()> {
        Ok(())
    }
}

// ------------------------------------------------------------------- platform

/// Per-OS constants and backend selection. The only place with `cfg(target_os)`.
pub mod platform {
    use super::*;

    #[cfg(target_os = "linux")]
    pub const PLUGIN_EXTENSION: &str = "so";
    #[cfg(target_os = "windows")]
    pub const PLUGIN_EXTENSION: &str = "dll";
    #[cfg(target_os = "macos")]
    pub const PLUGIN_EXTENSION: &str = "bundle";
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    pub const PLUGIN_EXTENSION: &str = "so";

    /// Name of the native texture-sharing technology on this OS.
    pub const TEXTURE_SHARE_NAME: &str = if cfg!(target_os = "windows") {
        "Spout"
    } else if cfg!(target_os = "macos") {
        "Syphon"
    } else {
        "PipeWire"
    };

    pub fn texture_share() -> Box<dyn TextureShare> {
        Box::new(NullTextureShare)
    }

    pub fn plugin_host() -> Box<dyn PluginHost> {
        Box::new(NullPluginHost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_backends() {
        let mut ts = platform::texture_share();
        assert!(ts.start("x", 1, 1, PixelFormat::Rgba8).is_err());
        let mut ph = platform::plugin_host();
        assert!(ph.scan(Path::new("/nonexistent")).unwrap().is_empty());
        let mut out = Vec::new();
        NullControlInput.poll(&mut out);
        assert!(out.is_empty());
    }
}
