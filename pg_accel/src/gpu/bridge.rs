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

// ---------------------------------------------------------------------------
// Geometry types for the three-layer spatial pipeline.
// ---------------------------------------------------------------------------

/// Geometry type tag (mirrors `pgaccel_geom_type` in `pgaccel_ffi.h`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelGeomType {
    Point = 0,
    LineString = 1,
    Polygon = 2,
    Unknown = 99,
}

/// Geometry descriptor for the spatial dispatch pipeline
/// (mirrors `pgaccel_geometry` in `pgaccel_ffi.h`).
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
// Extern declarations — linked at load time against libpgaccel_kernels.
// ---------------------------------------------------------------------------

// SAFETY: These are the C FFI bindings to libpgaccel_kernels. The functions
// are implemented in C++ and linked at load time. Caller must ensure
// pgaccel_init() is called before other functions.
unsafe extern "C" {
    /// Initialise the GPU runtime.  Must be called once before any other
    /// `pgaccel_*` function.
    pub fn pgaccel_init() -> PgaccelStatus;

    /// Tear down the GPU runtime and release all resources.
    pub fn pgaccel_shutdown() -> PgaccelStatus;

    /// Return information about the selected compute device.
    pub fn pgaccel_get_device_info() -> PgaccelDeviceInfo;

    /// Return platform-level capability flags.
    pub fn pgaccel_get_caps() -> PgaccelPlatformCaps;

    // -- Spatial predicate kernels --

    /// Bulk point-in-ring test.
    ///
    /// Results: 1 = inside, -1 = outside, 0 = uncertain.
    pub fn pgaccel_point_in_ring_bulk(
        points_xy: *const f32,
        point_count: usize,
        ring_xy: *const f32,
        vertex_count: usize,
        use_fp64: bool,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// Bulk sphere distance computation (Haversine).
    ///
    /// Outputs distances in metres. `uncertain[i] = 1` if the result
    /// needs CPU recheck.
    pub fn pgaccel_sphere_distance_bulk(
        points_a: *const f32,
        points_b: *const f32,
        count: usize,
        use_fp64: bool,
        distances: *mut f32,
        uncertain: *mut u8,
    ) -> PgaccelStatus;

    /// Bulk segment intersection test.
    ///
    /// Results: 1 = intersects, -1 = no, 0 = uncertain.
    pub fn pgaccel_segment_intersects_bulk(
        segs_a: *const f32,
        segs_b: *const f32,
        count: usize,
        use_fp64: bool,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// Three-layer spatial intersection pipeline.
    ///
    /// Takes arrays of geometry descriptors and partitions pairs into
    /// definite-true, definite-false, and uncertain buckets.
    pub fn pgaccel_spatial_intersects(
        geoms_a: *const PgaccelGeometry,
        count_a: usize,
        geoms_b: *const PgaccelGeometry,
        count_b: usize,
        definite_true_pairs: *mut u32,
        definite_true_count: *mut usize,
        definite_false_pairs: *mut u32,
        definite_false_count: *mut usize,
        uncertain_pairs: *mut u32,
        uncertain_count: *mut usize,
    ) -> PgaccelStatus;

    /// Bulk bbox intersection test (fp32).
    pub fn pgaccel_bbox_intersects_bulk_f32(
        boxes_a: *const f32,
        count_a: usize,
        boxes_b: *const f32,
        count_b: usize,
        result: *mut u8,
        hit_count: *mut usize,
    ) -> PgaccelStatus;
}
