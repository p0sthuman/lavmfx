//! The one GPU device all render tests share. A device per test, created in
//! parallel, hung the test run on Windows (two GPUs, several backends).

use std::sync::OnceLock;

/// `None` (and the test skips) when there is no GPU adapter.
pub(crate) fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    static GPU: OnceLock<Option<(wgpu::Device, wgpu::Queue)>> = OnceLock::new();
    GPU.get_or_init(|| {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no GPU adapter; skipping");
            return None;
        };
        let info = adapter.get_info();
        eprintln!("test GPU: {} ({:?}, {:?})", info.name, info.backend, info.device_type);
        let desc = wgpu::DeviceDescriptor {
            required_features: adapter.features() & crate::WANTED_FEATURES,
            ..Default::default()
        };
        Some(
            pollster::block_on(adapter.request_device(&desc)).unwrap_or_else(|e| panic!("test GPU {}: {e}", info.name)),
        )
    })
    .clone()
}
