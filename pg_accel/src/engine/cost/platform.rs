//! Hardware profile for the current platform.

use crate::gpu;

/// Hardware profile for the current platform.
#[derive(Debug, Clone)]
pub struct PlatformProfile {
    /// Number of available CPU cores.
    pub cpu_cores: usize,
    /// Whether a GPU device is available.
    pub has_gpu: bool,
    /// Whether CPU and GPU share the same memory (e.g., Apple Silicon).
    pub unified_memory: bool,
    /// Rough estimate of GPU compute throughput in GFLOPS.
    pub estimated_gpu_gflops: f64,
    /// Number of GPU compute units (0 when no GPU).
    pub compute_units: u32,
    /// Maximum single allocation size on the GPU in bytes (0 when no GPU).
    pub gpu_max_alloc_bytes: usize,
    /// Whether the GPU supports native fp64 (double-precision) arithmetic.
    pub has_fp64: bool,
}

impl PlatformProfile {
    /// Detect the current platform's capabilities.
    ///
    /// Queries GPU device info directly via the Metal binary-archive runtime.
    /// The GPU runtime is initialized lazily per-backend.
    #[must_use]
    pub fn detect() -> Self {
        let cpu_cores = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1);

        gpu::ensure_init();
        let device = gpu::get_device_info();
        let has_gpu = device.compute_units > 0;
        // `is_unified_memory` from the C caps is conservatively false on
        // Metal (raw host pointers can't be passed to GPU kernels). For
        // *physical memory sharing* — the dimension that actually governs
        // whether VRAM-based chunk sizing makes sense — Metal always wins:
        // Apple Silicon puts CPU+GPU on the same DRAM die. Treat Metal
        // and CPU backends as unified-memory for chunk sizing, and keep
        // the C-side flag authoritative for USM pointer decisions.
        let backend_bytes: &[u8] = {
            // SAFETY: backend_name is a fixed 64-byte c_char array; reinterpret
            // as unsigned bytes. The terminator search is bounded by the array.
            let arr: &[u8] = unsafe {
                std::slice::from_raw_parts(
                    device.backend_name.as_ptr().cast::<u8>(),
                    device.backend_name.len(),
                )
            };
            let end = arr.iter().position(|&c| c == 0).unwrap_or(arr.len());
            &arr[..end]
        };
        let backend = std::str::from_utf8(backend_bytes).unwrap_or("");
        let unified = device.is_unified_memory || backend == "metal" || backend == "cpu";

        #[allow(clippy::cast_precision_loss)]
        let estimated_gflops = if has_gpu {
            (device.compute_units as f64) * 2.0
        } else {
            0.0
        };

        Self {
            cpu_cores,
            has_gpu,
            unified_memory: unified,
            estimated_gpu_gflops: estimated_gflops,
            compute_units: device.compute_units,
            gpu_max_alloc_bytes: device.max_alloc_bytes,
            has_fp64: device.has_fp64,
        }
    }
}
