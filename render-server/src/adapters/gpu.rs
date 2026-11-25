//! The headless graphics device: an adapter picked with no surface, its device, and egui-wgpu's
//! `RenderState`. With no surface the target format is `Rgba8Unorm`, so read-back bytes are RGBA in
//! gamma space, as a window would show them. Ported from SolidZ's render server `adapters/gpu.rs`
//! (its depth/stencil, MSAA and viewport-policy parts belong to a 3D viewport; this UI has none).

use std::sync::Arc;

use egui_wgpu::wgpu;

/// Which adapters may be used (`--adapter`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// The best one: hardware first, a software rasterizer when nothing else exists.
    Auto,
    /// Hardware only.
    Hardware,
    /// A software rasterizer only (WARP on Windows, lavapipe on Linux).
    Software,
}

impl Policy {
    pub fn parse(s: &str) -> Option<Policy> {
        match s {
            "auto" => Some(Policy::Auto),
            "hardware" => Some(Policy::Hardware),
            "software" => Some(Policy::Software),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Policy::Auto => "auto",
            Policy::Hardware => "hardware",
            Policy::Software => "software",
        }
    }

    fn admits(self, t: wgpu::DeviceType) -> bool {
        match self {
            Policy::Auto => true,
            Policy::Hardware => t != wgpu::DeviceType::Cpu,
            Policy::Software => t == wgpu::DeviceType::Cpu,
        }
    }
}

/// The device and what was chosen, for the log and `frames.json`.
pub struct Gpu {
    pub rs: egui_wgpu::RenderState,
    pub backend: &'static str,
    pub adapter: String,
    pub device_type: String,
    pub software: bool,
}

/// The backends tried: `WGPU_BACKEND` if set, else the platform's own (Metal on macOS, D3D12 on
/// Windows, Vulkan elsewhere).
fn backends() -> wgpu::Backends {
    if let Some(from_env) = wgpu::Backends::from_env() {
        return from_env;
    }
    if cfg!(target_os = "macos") {
        wgpu::Backends::METAL
    } else if cfg!(target_os = "windows") {
        wgpu::Backends::DX12
    } else {
        wgpu::Backends::VULKAN
    }
}

/// Open the device on the adapter `policy` admits.
pub fn open(policy: Policy) -> Result<Gpu, String> {
    // An instance with no display handle (headless).
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = backends();
    // `Ok(None)` would fall back to wgpu's own request, which ignores the policy, so a refusal is an error.
    setup.native_adapter_selector = Some(Arc::new(move |adapters, _surface| choose(adapters, policy).map(Some)));
    // Small memory blocks: one frame's targets are a few blocks, not a whole idle one.
    let base = setup.device_descriptor.clone();
    setup.device_descriptor = Arc::new(move |adapter| {
        let mut d = base(adapter);
        d.memory_hints = wgpu::MemoryHints::MemoryUsage;
        d
    });
    let config = egui_wgpu::WgpuConfiguration { wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup), ..Default::default() };
    let instance = pollster::block_on(config.wgpu_setup.new_instance());
    // The UI's own pass: one sample, no depth.
    let options = egui_wgpu::RendererOptions { msaa_samples: 1, depth_stencil_format: None, ..Default::default() };
    let rs = pollster::block_on(egui_wgpu::RenderState::create(&config, &instance, None, options)).map_err(|e| format!("graphics device: {e}"))?;
    // With no surface the format is always `Rgba8Unorm`: read-back rows are RGBA as they are.
    if rs.target_format != wgpu::TextureFormat::Rgba8Unorm {
        return Err(format!("graphics device: unexpected target format {:?}", rs.target_format));
    }
    let info = rs.adapter.get_info();
    log::info!("graphics: {:?} {} ({:?})", info.backend, info.name, info.device_type);
    Ok(Gpu {
        backend: backend_name(info.backend),
        adapter: info.name.clone(),
        device_type: format!("{:?}", info.device_type),
        software: info.device_type == wgpu::DeviceType::Cpu,
        rs,
    })
}

/// The adapter `policy` admits, best first: a discrete GPU, an integrated one, a virtual one, an
/// unknown kind, a software rasterizer.
fn choose(adapters: &[wgpu::Adapter], policy: Policy) -> Result<wgpu::Adapter, String> {
    adapters.iter().filter(|a| policy.admits(a.get_info().device_type)).max_by_key(|a| rank(a.get_info().device_type)).cloned().ok_or_else(|| {
        let found: Vec<String> =
            adapters.iter().map(|a| format!("{} ({:?}, {:?})", a.get_info().name, a.get_info().backend, a.get_info().device_type)).collect();
        format!("no graphics adapter qualifies under `{}` (found: {})", policy.name(), if found.is_empty() { "none".into() } else { found.join(", ") })
    })
}

fn rank(t: wgpu::DeviceType) -> u8 {
    match t {
        wgpu::DeviceType::DiscreteGpu => 4,
        wgpu::DeviceType::IntegratedGpu => 3,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 1,
        wgpu::DeviceType::Cpu => 0,
    }
}

fn backend_name(b: wgpu::Backend) -> &'static str {
    match b {
        wgpu::Backend::Vulkan => "vulkan",
        wgpu::Backend::Dx12 => "dx12",
        wgpu::Backend::Gl => "gl",
        wgpu::Backend::Metal => "metal",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies() {
        assert_eq!(Policy::parse("software"), Some(Policy::Software));
        assert_eq!(Policy::parse("cpu"), None);
        assert!(Policy::Software.admits(wgpu::DeviceType::Cpu) && !Policy::Software.admits(wgpu::DeviceType::DiscreteGpu));
        assert!(Policy::Hardware.admits(wgpu::DeviceType::IntegratedGpu) && !Policy::Hardware.admits(wgpu::DeviceType::Cpu));
        assert!(Policy::Auto.admits(wgpu::DeviceType::Cpu));
        assert!(rank(wgpu::DeviceType::DiscreteGpu) > rank(wgpu::DeviceType::Cpu));
    }
}
