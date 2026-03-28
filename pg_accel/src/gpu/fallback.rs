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
// Geometry types (mirror bridge.rs for API parity)
// ---------------------------------------------------------------------------

/// Geometry type tag (fallback mirror of bridge::PgaccelGeomType).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelGeomType {
    Point = 0,
    LineString = 1,
    Polygon = 2,
    Unknown = 99,
}

/// Geometry descriptor (fallback mirror of bridge::PgaccelGeometry).
#[repr(C)]
#[derive(Debug, Clone)]
pub struct PgaccelGeometry {
    pub geom_type: PgaccelGeomType,
    pub bbox: *const f32,
    pub coords: *const f32,
    pub coord_count: usize,
    pub ring_offsets: *const u32,
    pub ring_count: usize,
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

// ---------------------------------------------------------------------------
// Spatial kernel stubs
// ---------------------------------------------------------------------------

/// Stub: returns `ErrorNoDevice`.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn pgaccel_spatial_intersects(
    _geoms_a: *const PgaccelGeometry,
    _count_a: usize,
    _geoms_b: *const PgaccelGeometry,
    _count_b: usize,
    _definite_true_pairs: *mut u32,
    _definite_true_count: *mut usize,
    _definite_false_pairs: *mut u32,
    _definite_false_count: *mut usize,
    _uncertain_pairs: *mut u32,
    _uncertain_count: *mut usize,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

/// Stub: returns `ErrorNoDevice`.
#[must_use]
pub fn pgaccel_bbox_intersects_bulk_f32(
    _boxes_a: *const f32,
    _count_a: usize,
    _boxes_b: *const f32,
    _count_b: usize,
    _result: *mut u8,
    _hit_count: *mut usize,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

/// Stub: returns `ErrorNoDevice`.
#[must_use]
pub fn pgaccel_point_in_ring_bulk(
    _points_xy: *const f32,
    _point_count: usize,
    _ring_xy: *const f32,
    _vertex_count: usize,
    _use_fp64: bool,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

/// Stub: returns `ErrorNoDevice`.
#[must_use]
pub fn pgaccel_sphere_distance_bulk(
    _points_a: *const f32,
    _points_b: *const f32,
    _count: usize,
    _use_fp64: bool,
    _distances: *mut f32,
    _uncertain: *mut u8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

/// Stub: returns `ErrorNoDevice`.
#[must_use]
pub fn pgaccel_segment_intersects_bulk(
    _segs_a: *const f32,
    _segs_b: *const f32,
    _count: usize,
    _use_fp64: bool,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}
