//! FFI bridge to `libpgaccel_kernels` (C++/SYCL GPU library).
//!
//! Only compiled when the `gpu` feature is enabled.  The declarations here
//! mirror `pgaccel-kernels/include/pgaccel_ffi.h` exactly.

use std::ffi::c_char;

// ---------------------------------------------------------------------------
// Status code returned by every kernel-library call.
// ---------------------------------------------------------------------------

/// Status codes returned by the pgaccel C library.
///
/// Values **must** stay in sync with the `pgaccel_status` enum in
/// `pgaccel_ffi.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelStatus {
    Ok = 0,
    ErrorInit = 1,
    ErrorNoDevice = 2,
    ErrorOom = 3,
    ErrorTimeout = 4,
    ErrorUnsupported = 5,
}

impl PgaccelStatus {
    /// Returns `true` when the status indicates success.
    #[must_use]
    pub const fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

// ---------------------------------------------------------------------------
// Structs returned by device-query functions.
// ---------------------------------------------------------------------------

/// Per-device information (mirrors `pgaccel_device_info`).
#[repr(C)]
#[derive(Debug, Clone)]
pub struct PgaccelDeviceInfo {
    pub device_name: [c_char; 256],
    pub backend_name: [c_char; 64],
    pub has_fp64: i32,
    pub has_atomic64: i32,
    pub has_ooo_queue: i32,
    pub is_unified_memory: i32,
    pub max_alloc_bytes: usize,
    pub compute_units: u32,
}

/// Platform-level capability summary (mirrors `pgaccel_platform_caps`).
#[repr(C)]
#[derive(Debug, Clone)]
pub struct PgaccelPlatformCaps {
    pub has_fp64: i32,
    pub has_atomic64: i32,
    pub has_ooo_queue: i32,
    pub is_unified_memory: i32,
    pub max_alloc_bytes: usize,
    pub compute_units: u32,
    pub backend_name: [c_char; 64],
}

// ---------------------------------------------------------------------------
// Extern declarations — linked at load time against libpgaccel_kernels.
// ---------------------------------------------------------------------------

extern "C" {
    /// Initialise the GPU runtime.  Must be called once before any other
    /// `pgaccel_*` function.
    pub fn pgaccel_init() -> PgaccelStatus;

    /// Tear down the GPU runtime and release all resources.
    pub fn pgaccel_shutdown() -> PgaccelStatus;

    /// Return information about the selected compute device.
    pub fn pgaccel_get_device_info() -> PgaccelDeviceInfo;

    /// Return platform-level capability flags.
    pub fn pgaccel_get_caps() -> PgaccelPlatformCaps;
}
