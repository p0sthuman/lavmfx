//! Audio for LAVMFX: device list, FFT input tap (External FFT), analyser.
//! `mixer`: clip audio output + Composition / Layer spectra.

pub mod analyzer;
pub mod device;
pub mod fx;
pub mod mixer;

pub use device::{DeviceInfo, Devices, InputTap, SharedSpectrum, TapConfig, list};
pub use mixer::{Output, OutputConfig, Voice};
