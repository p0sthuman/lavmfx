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
        // CI on Windows has only WARP, which loses the device on the effects shader.
        if info.device_type == wgpu::DeviceType::Cpu && std::env::var_os("LAVMFX_TEST_SKIP_SOFTWARE_GPU").is_some() {
            eprintln!(
                "skipping: software GPU only ({}) and LAVMFX_TEST_SKIP_SOFTWARE_GPU set",
                info.name
            );
            return None;
        }
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
