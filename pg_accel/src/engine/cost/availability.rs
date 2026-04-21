//! GPU availability and capability probes (cached via OnceLock).

use super::platform::PlatformProfile;
use crate::engine::gucs;

// ---------------------------------------------------------------------------
// GPU availability
// ---------------------------------------------------------------------------

/// Cached result of GPU hardware detection.
static GPU_AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Whether the current platform has GPU hardware.
///
/// Result is cached via [`OnceLock`] so the GPU runtime is only probed once.
/// In `#[cfg(test)]` builds, always returns `false` to avoid calling into the
/// GPU runtime (which requires a running PG backend).
#[must_use]
pub fn gpu_hardware_available() -> bool {
    *GPU_AVAILABLE.get_or_init(|| {
        #[cfg(test)]
        {
            false
        }
        #[cfg(not(test))]
        {
            PlatformProfile::detect().has_gpu
        }
    })
}

/// Whether GPU acceleration can be used: hardware is present **and** the
/// `pg_accel.gpu_enabled` GUC is on.
#[must_use]
pub fn gpu_is_usable() -> bool {
    gucs::gpu_enabled() && gpu_hardware_available()
}

/// Cached result of fp64 hardware detection.
static HAS_FP64: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Whether the GPU supports native fp64 (double-precision) arithmetic.
///
/// Result is cached via [`OnceLock`] so the GPU runtime is only probed once.
///
/// On Apple GPUs fp64 is emulated via soft-float — flipped to `true` when
/// AdaptiveCpp is built with `ACPP_METAL_EXTERNAL_FP64_DIR` pointing at the
/// external soft-fp64 implementation (see
/// `AdaptiveCpp/src/libkernel/sscp/metal/float64/README.md`). Absent that,
/// this returns `false` on Metal and planner gates route f64 paths to
/// fp32 fallbacks.
#[must_use]
pub fn platform_has_fp64() -> bool {
    *HAS_FP64.get_or_init(|| PlatformProfile::detect().has_fp64)
}
