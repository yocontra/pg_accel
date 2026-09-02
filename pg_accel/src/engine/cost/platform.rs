//! Hardware profile for the current platform.

use crate::gpu;

/// Hardware profile for the current platform.
#[derive(Debug, Clone, PartialEq)]
pub struct PlatformProfile {
    /// Number of available CPU cores.
    pub cpu_cores: usize,
    /// Whether a GPU device is available.
    pub has_gpu: bool,
    /// Rough estimate of GPU compute throughput in GFLOPS.
    pub estimated_gpu_gflops: f64,
    /// Number of GPU compute units (0 when no GPU).
    pub compute_units: u32,
    /// Runtime-selected GPU device name (empty when no GPU).
    pub device_name: String,
    /// Runtime-selected GPU backend name (empty when no GPU).
    pub backend_name: String,
    /// Maximum single allocation size on the GPU in bytes (0 when no GPU).
    pub gpu_max_alloc_bytes: usize,
    /// Whether the GPU supports native fp64 (double-precision) arithmetic.
    pub has_native_fp64: bool,
}

impl PlatformProfile {
    /// Detect the current platform's capabilities.
    ///
    /// Queries GPU device info directly via the Metal binary-archive runtime.
    /// The GPU runtime is initialized lazily per-backend.
    #[must_use]
    pub fn detect() -> Self {
        let cpu_cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);

        gpu::ensure_init();
        let device = gpu::get_device_info();
        let has_gpu = device.compute_units > 0;
        let device_name = cchar_buf_to_string(&device.device_name);
        let backend_name = cchar_buf_to_string(&device.backend_name);
        #[allow(clippy::cast_precision_loss)]
        let estimated_gflops = if has_gpu {
            (device.compute_units as f64) * 2.0
        } else {
            0.0
        };

        Self {
            cpu_cores,
            has_gpu,
            estimated_gpu_gflops: estimated_gflops,
            compute_units: device.compute_units,
            device_name,
            backend_name,
            gpu_max_alloc_bytes: device.max_alloc_bytes,
            has_native_fp64: device.has_native_fp64,
        }
    }
}

fn cchar_buf_to_string(buf: &[std::ffi::c_char]) -> String {
    buf.iter()
        .take_while(|&&value| value != 0)
        .map(|&value| value as u8 as char)
        .collect()
}
