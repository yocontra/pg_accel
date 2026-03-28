//! Pure-Rust fallback stubs used when the `gpu` feature is **not** enabled.
//!
//! Every function returns [`PgaccelStatus::ErrorNoDevice`] so that callers can
//! detect the absence of GPU support at runtime without conditional compilation
//! at every call-site.

use std::ffi::c_char;

// Re-export the same types as `bridge.rs` so the public API is identical
// regardless of the feature flag.

/// Status codes returned by the pgaccel library (or its fallback).
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
// Stub implementations — no C dependency, no unsafe.
// ---------------------------------------------------------------------------

/// Stub: always returns [`PgaccelStatus::ErrorNoDevice`].
#[must_use]
pub fn pgaccel_init() -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

/// Stub: always returns [`PgaccelStatus::ErrorNoDevice`].
#[must_use]
pub fn pgaccel_shutdown() -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

/// Stub: returns a zeroed [`PgaccelDeviceInfo`] (no device available).
#[must_use]
pub fn pgaccel_get_device_info() -> PgaccelDeviceInfo {
    PgaccelDeviceInfo {
        device_name: [0; 256],
        backend_name: [0; 64],
        has_fp64: 0,
        has_atomic64: 0,
        has_ooo_queue: 0,
        is_unified_memory: 0,
        max_alloc_bytes: 0,
        compute_units: 0,
    }
}

/// Stub: returns a zeroed [`PgaccelPlatformCaps`] (no device available).
#[must_use]
pub const fn pgaccel_get_caps() -> PgaccelPlatformCaps {
    PgaccelPlatformCaps {
        has_fp64: 0,
        has_atomic64: 0,
        has_ooo_queue: 0,
        is_unified_memory: 0,
        max_alloc_bytes: 0,
        compute_units: 0,
        backend_name: [0; 64],
    }
}
