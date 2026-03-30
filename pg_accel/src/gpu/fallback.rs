//! Pure-Rust fallback stubs used when the `gpu` feature is **not** enabled.
//!
//! Every function returns [`PgaccelStatus::ErrorNoDevice`] so that callers can
//! detect the absence of GPU support at runtime without conditional compilation
//! at every call-site.

use std::ffi::c_char;

// Re-export the same types as `bridge.rs` so the public API is identical
// regardless of the feature flag.

/// Status codes returned by the pgaccel library (or its fallback).
///
/// Values match the `pgaccel_status` enum in `pgaccel_ffi.h`.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelStatus {
    Ok = 0,
    ErrorInit = -1,
    ErrorUnsupported = -2,
    ErrorOom = -3,
    ErrorTimeout = -4,
    ErrorNoDevice = -5,
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
    pub device_name: [c_char; 128],
    pub backend_name: [c_char; 64],
    pub compute_units: u32,
    pub max_alloc_bytes: usize,
    pub has_fp64: bool,
    pub has_atomic64: bool,
    pub is_unified_memory: bool,
}

/// Platform-level capability summary (mirrors `pgaccel_platform_caps`).
#[repr(C)]
#[derive(Debug, Clone)]
pub struct PgaccelPlatformCaps {
    pub has_fp64: bool,
    pub has_atomic64: bool,
    pub has_ooo_queue: bool,
    pub is_unified_memory: bool,
    pub max_alloc_bytes: usize,
    pub compute_units: u32,
    pub backend_name: [c_char; 64],
}

// ---------------------------------------------------------------------------
// Expression evaluator types (mirror bridge.rs for API parity)
// ---------------------------------------------------------------------------

/// Value type tag for the expression evaluator.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelValTag {
    Null = 0,
    Bool = 1,
    Int32 = 2,
    Int64 = 3,
    Float32 = 4,
    Float64 = 5,
    Date = 6,
    Timestamp = 7,
}

/// Tagged value — 16 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelVal {
    pub tag: PgaccelValTag,
    pub data: u64,
}

impl PgaccelVal {
    #[must_use]
    pub const fn null() -> Self {
        Self {
            tag: PgaccelValTag::Null,
            data: 0,
        }
    }

    #[must_use]
    pub const fn from_i32(v: i32) -> Self {
        Self {
            tag: PgaccelValTag::Int32,
            data: v as u64,
        }
    }

    #[must_use]
    pub const fn from_i64(v: i64) -> Self {
        Self {
            tag: PgaccelValTag::Int64,
            data: v as u64,
        }
    }

    #[must_use]
    pub fn from_f64(v: f64) -> Self {
        Self {
            tag: PgaccelValTag::Float64,
            data: v.to_bits(),
        }
    }

    #[must_use]
    pub fn from_f32(v: f32) -> Self {
        Self {
            tag: PgaccelValTag::Float32,
            data: u64::from(v.to_bits()),
        }
    }

    #[must_use]
    pub const fn from_bool(v: bool) -> Self {
        Self {
            tag: PgaccelValTag::Bool,
            data: v as u64,
        }
    }
}

/// Single bytecode instruction — 8 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelExprInstruction {
    pub opcode: u16,
    pub pad: u16,
    pub arg: u32,
}

/// Expression program (bytecode + constant pool).
#[repr(C)]
pub struct PgaccelExprProgram {
    pub instructions: *const PgaccelExprInstruction,
    pub inst_count: usize,
    pub const_pool: *const PgaccelVal,
    pub const_count: usize,
    pub max_stack: usize,
    pub num_cols: usize,
}

/// Columnar batch for expression evaluation.
#[repr(C)]
pub struct PgaccelBatch {
    pub num_rows: usize,
    pub num_cols: usize,
    pub col_data: *const *const std::ffi::c_void,
    pub col_nulls: *const *const u8,
    pub col_types: *const PgaccelValTag,
}

// ---------------------------------------------------------------------------
// Hash aggregation types (mirror bridge.rs for API parity)
// ---------------------------------------------------------------------------

/// Aggregate function tag for GPU hash aggregation.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelAggFunc {
    Sum = 0,
    Min = 1,
    Max = 2,
    Count = 3,
}

/// Aggregate column descriptor for GPU hash aggregation.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelAggCol {
    pub func: PgaccelAggFunc,
    pub col_idx: usize,
}

/// Opaque handle to GPU hash aggregation state.
pub enum PgaccelAggState {}

// ---------------------------------------------------------------------------
// Hash join types (mirror bridge.rs for API parity)
// ---------------------------------------------------------------------------

/// Key type for hash join operations.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelKeyType {
    Int32 = 0,
    Int64 = 1,
    Float64 = 2,
}

/// Opaque handle to a GPU-side hash table.
pub enum PgaccelHashTable {}

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
// Raster types (mirror bridge.rs for API parity)
// ---------------------------------------------------------------------------

/// Pixel type tag (fallback mirror of bridge::PgaccelPixelType).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelPixelType {
    Int8 = 0,
    Int16 = 1,
    Int32 = 2,
    Float32 = 3,
    Float64 = 4,
}

/// Map-algebra opcode (fallback mirror of bridge::PgaccelOp).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelOp {
    LoadBand = 0,
    LoadConst = 1,
    Add = 2,
    Sub = 3,
    Mul = 4,
    Div = 5,
    Sqrt = 6,
    Abs = 7,
    Log = 8,
    Pow = 9,
    Gt = 10,
    Lt = 11,
    Eq = 12,
    Select = 13,
}

/// Single instruction in a map-algebra expression
/// (fallback mirror of bridge::PgaccelExprInst).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelExprInst {
    pub op: PgaccelOp,
    pub arg: f64,
}

/// Map-algebra expression program (fallback mirror of bridge::PgaccelExpr).
#[repr(C)]
pub struct PgaccelExpr {
    pub instructions: *mut PgaccelExprInst,
    pub inst_count: usize,
    pub band_count: usize,
}

/// Reclassification rule (fallback mirror of bridge::PgaccelReclassRule).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelReclassRule {
    pub min_val: f64,
    pub max_val: f64,
    pub new_val: f64,
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
        device_name: [0; 128],
        backend_name: [0; 64],
        compute_units: 0,
        max_alloc_bytes: 0,
        has_fp64: false,
        has_atomic64: false,
        is_unified_memory: false,
    }
}

/// Stub: returns a zeroed [`PgaccelPlatformCaps`] (no device available).
#[must_use]
pub const fn pgaccel_get_caps() -> PgaccelPlatformCaps {
    PgaccelPlatformCaps {
        has_fp64: false,
        has_atomic64: false,
        has_ooo_queue: false,
        is_unified_memory: false,
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

/// Stub: returns `ErrorNoDevice`.
#[must_use]
pub fn pgaccel_bbox_intersects_bulk_f64(
    _boxes_a: *const f64,
    _count_a: usize,
    _boxes_b: *const f64,
    _count_b: usize,
    _result: *mut u8,
    _hit_count: *mut usize,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

// ---------------------------------------------------------------------------
// Platform capability convenience stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_fp64_available() -> bool {
    false
}

#[must_use]
pub fn pgaccel_unified_memory() -> bool {
    false
}

#[must_use]
pub fn pgaccel_ooo_queue_available() -> bool {
    false
}

// ---------------------------------------------------------------------------
// Memory pool stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_alloc(_bytes: usize) -> *mut std::ffi::c_void {
    std::ptr::null_mut()
}

pub fn pgaccel_free(_ptr: *mut std::ffi::c_void) {}

pub fn pgaccel_pool_reset() {}

#[must_use]
pub fn pgaccel_pool_bytes_used() -> usize {
    0
}

pub fn pgaccel_prefetch(_ptr: *mut std::ffi::c_void, _bytes: usize) {}

// ---------------------------------------------------------------------------
// Sort kernel stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_sort_f32(_data: *mut f32, _count: usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_sort_f64(_data: *mut f64, _count: usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_sort_i32(_data: *mut i32, _count: usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_sort_i64(_data: *mut i64, _count: usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_sort_kv_f32(_keys: *mut f32, _indices: *mut u32, _count: usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_sort_kv_f64(_keys: *mut f64, _indices: *mut u32, _count: usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

// ---------------------------------------------------------------------------
// Reduce kernel stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_reduce_sum_f32(
    _data: *const f32,
    _count: usize,
    _result: *mut f32,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_reduce_min_f32(
    _data: *const f32,
    _count: usize,
    _result: *mut f32,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_reduce_max_f32(
    _data: *const f32,
    _count: usize,
    _result: *mut f32,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_reduce_sum_f64(
    _data: *const f64,
    _count: usize,
    _result: *mut f64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_reduce_min_f64(
    _data: *const f64,
    _count: usize,
    _result: *mut f64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_reduce_max_f64(
    _data: *const f64,
    _count: usize,
    _result: *mut f64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_reduce_sum_i64(
    _data: *const i64,
    _count: usize,
    _result: *mut i64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_reduce_count(_mask: *const u8, _count: usize, _result: *mut usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

// ---------------------------------------------------------------------------
// H3 cell operation stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_h3_get_resolution_bulk(
    _cells: *const u64,
    _count: usize,
    _resolutions: *mut i32,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_h3_cell_to_parent_bulk(
    _cells: *const u64,
    _count: usize,
    _parent_res: i32,
    _parents: *mut u64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_h3_grid_distance_bulk(
    _cells_a: *const u64,
    _cells_b: *const u64,
    _count: usize,
    _distances: *mut i32,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_h3_lat_lng_to_cell_bulk(
    _lat_array: *const std::ffi::c_void,
    _lng_array: *const std::ffi::c_void,
    _count: usize,
    _resolution: i32,
    _use_fp64: i32,
    _cell_ids: *mut u64,
    _valid: *mut u8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

// ---------------------------------------------------------------------------
// Raster operation stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_map_algebra(
    _band_pixels: *const *const std::ffi::c_void,
    _pixel_count: usize,
    _pixel_type: i32,
    _expr: *const PgaccelExpr,
    _output_pixels: *mut std::ffi::c_void,
    _nodata_mask: *mut u8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn pgaccel_raster_clip(
    _rast_pixels: *const std::ffi::c_void,
    _width: usize,
    _height: usize,
    _origin_x: f64,
    _origin_y: f64,
    _scale_x: f64,
    _scale_y: f64,
    _pixel_type: i32,
    _clip_ring_xy: *const f32,
    _vertex_count: usize,
    _output_pixels: *mut std::ffi::c_void,
    _nodata_mask: *mut u8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_raster_reclass(
    _input_pixels: *const std::ffi::c_void,
    _pixel_count: usize,
    _input_type: i32,
    _rules: *const PgaccelReclassRule,
    _rule_count: usize,
    _output_type: i32,
    _output_pixels: *mut std::ffi::c_void,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

// ---------------------------------------------------------------------------
// Hash join stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_hash_join_build(
    _keys: *const std::ffi::c_void,
    _null_mask: *const u8,
    _indices: *const u32,
    _count: usize,
    _key_type: PgaccelKeyType,
) -> *mut PgaccelHashTable {
    std::ptr::null_mut()
}

pub fn pgaccel_hash_join_free(_ht: *mut PgaccelHashTable) {}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn pgaccel_hash_join_probe(
    _ht: *const PgaccelHashTable,
    _outer_keys: *const std::ffi::c_void,
    _outer_null_mask: *const u8,
    _outer_count: usize,
    _match_pairs: *mut u32,
    _max_matches: usize,
    _match_count: *mut usize,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

// ---------------------------------------------------------------------------
// Hash aggregation stubs
// ---------------------------------------------------------------------------

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn pgaccel_hash_agg_execute(
    _group_keys: *const std::ffi::c_void,
    _group_null_mask: *const u8,
    _row_count: usize,
    _key_type: i32,
    _value_cols: *const *const std::ffi::c_void,
    _value_nulls: *const *const u8,
    _value_types: *const i32,
    _agg_cols: *const PgaccelAggCol,
    _num_aggs: usize,
) -> *mut PgaccelAggState {
    std::ptr::null_mut()
}

#[must_use]
pub fn pgaccel_agg_group_count(_state: *const PgaccelAggState) -> usize {
    0
}

#[must_use]
pub fn pgaccel_agg_get_group_keys(_state: *const PgaccelAggState) -> *const std::ffi::c_void {
    std::ptr::null()
}

#[must_use]
pub fn pgaccel_agg_get_results(_state: *const PgaccelAggState, _agg_idx: usize) -> *const f64 {
    std::ptr::null()
}

#[must_use]
pub fn pgaccel_agg_get_counts(_state: *const PgaccelAggState) -> *const i64 {
    std::ptr::null()
}

pub fn pgaccel_agg_free(_state: *mut PgaccelAggState) {}

// ---------------------------------------------------------------------------
// Expression evaluator stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_expr_eval_predicate(
    _program: *const PgaccelExprProgram,
    _batch: *const PgaccelBatch,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_expr_eval_project(
    _program: *const PgaccelExprProgram,
    _batch: *const PgaccelBatch,
    _output: *mut PgaccelVal,
    _uncertain: *mut u8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_expr_template_cmp_const(
    _batch: *const PgaccelBatch,
    _col_idx: u32,
    _cmp_opcode: u16,
    _const_val: f64,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_expr_template_between(
    _batch: *const PgaccelBatch,
    _col_idx: u32,
    _lo: f64,
    _hi: f64,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_expr_template_in_list(
    _batch: *const PgaccelBatch,
    _col_idx: u32,
    _values: *const f64,
    _value_count: usize,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_expr_template_is_null(
    _batch: *const PgaccelBatch,
    _col_idx: u32,
    _check_not_null: bool,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn pgaccel_expr_template_two_pred_and(
    _batch: *const PgaccelBatch,
    _col1_idx: u32,
    _cmp1_opcode: u16,
    _const1_val: f64,
    _col2_idx: u32,
    _cmp2_opcode: u16,
    _const2_val: f64,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

// ---------------------------------------------------------------------------
// Window function stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_window_row_number(
    _partition_starts: *const u8,
    _count: usize,
    _results: *mut i64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_window_rank(
    _partition_starts: *const u8,
    _sort_keys: *const f64,
    _count: usize,
    _results: *mut i64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_window_dense_rank(
    _partition_starts: *const u8,
    _sort_keys: *const f64,
    _count: usize,
    _results: *mut i64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_window_sum(
    _partition_starts: *const u8,
    _values: *const f64,
    _null_mask: *const u8,
    _count: usize,
    _results: *mut f64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_window_count(
    _partition_starts: *const u8,
    _null_mask: *const u8,
    _count: usize,
    _results: *mut i64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn pgaccel_window_lag(
    _partition_starts: *const u8,
    _values: *const f64,
    _null_mask: *const u8,
    _count: usize,
    _offset: i32,
    _default_val: f64,
    _results: *mut f64,
    _result_nulls: *mut u8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn pgaccel_window_lead(
    _partition_starts: *const u8,
    _values: *const f64,
    _null_mask: *const u8,
    _count: usize,
    _offset: i32,
    _default_val: f64,
    _results: *mut f64,
    _result_nulls: *mut u8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    // -----------------------------------------------------------------------
    // Stub return values
    // -----------------------------------------------------------------------

    #[test]
    fn init_returns_error_no_device() {
        assert_eq!(pgaccel_init(), PgaccelStatus::ErrorNoDevice);
    }

    #[test]
    fn shutdown_returns_error_no_device() {
        assert_eq!(pgaccel_shutdown(), PgaccelStatus::ErrorNoDevice);
    }

    #[test]
    fn get_device_info_returns_zeroed() {
        let info = pgaccel_get_device_info();
        assert!(!info.has_fp64);
        assert!(!info.has_atomic64);
        assert!(!info.is_unified_memory);
        assert_eq!(info.max_alloc_bytes, 0);
        assert_eq!(info.compute_units, 0);
        assert!(info.device_name.iter().all(|&c| c == 0));
        assert!(info.backend_name.iter().all(|&c| c == 0));
    }

    #[test]
    fn get_caps_returns_zeroed() {
        let caps = pgaccel_get_caps();
        assert!(!caps.has_fp64);
        assert!(!caps.has_atomic64);
        assert!(!caps.has_ooo_queue);
        assert!(!caps.is_unified_memory);
        assert_eq!(caps.max_alloc_bytes, 0);
        assert_eq!(caps.compute_units, 0);
        assert!(caps.backend_name.iter().all(|&c| c == 0));
    }

    #[test]
    fn point_in_ring_bulk_returns_error_no_device() {
        let status =
            pgaccel_point_in_ring_bulk(ptr::null(), 0, ptr::null(), 0, false, ptr::null_mut());
        assert_eq!(status, PgaccelStatus::ErrorNoDevice);
    }

    #[test]
    fn sphere_distance_bulk_returns_error_no_device() {
        let status = pgaccel_sphere_distance_bulk(
            ptr::null(),
            ptr::null(),
            0,
            false,
            ptr::null_mut(),
            ptr::null_mut(),
        );
        assert_eq!(status, PgaccelStatus::ErrorNoDevice);
    }

    #[test]
    fn segment_intersects_bulk_returns_error_no_device() {
        let status =
            pgaccel_segment_intersects_bulk(ptr::null(), ptr::null(), 0, false, ptr::null_mut());
        assert_eq!(status, PgaccelStatus::ErrorNoDevice);
    }

    #[test]
    fn spatial_intersects_returns_error_no_device() {
        let status = pgaccel_spatial_intersects(
            ptr::null(),
            0,
            ptr::null(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
        );
        assert_eq!(status, PgaccelStatus::ErrorNoDevice);
    }

    #[test]
    fn bbox_intersects_bulk_f32_returns_error_no_device() {
        let status = pgaccel_bbox_intersects_bulk_f32(
            ptr::null(),
            0,
            ptr::null(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        );
        assert_eq!(status, PgaccelStatus::ErrorNoDevice);
    }

    // -----------------------------------------------------------------------
    // PgaccelStatus::is_ok
    // -----------------------------------------------------------------------

    #[test]
    fn status_is_ok_returns_true_only_for_ok() {
        assert!(PgaccelStatus::Ok.is_ok());
        assert!(!PgaccelStatus::ErrorInit.is_ok());
        assert!(!PgaccelStatus::ErrorNoDevice.is_ok());
        assert!(!PgaccelStatus::ErrorOom.is_ok());
        assert!(!PgaccelStatus::ErrorTimeout.is_ok());
        assert!(!PgaccelStatus::ErrorUnsupported.is_ok());
    }

    // -----------------------------------------------------------------------
    // PgaccelGeomType discriminants
    // -----------------------------------------------------------------------

    #[test]
    fn geom_type_discriminants() {
        assert_eq!(PgaccelGeomType::Point as u32, 0);
        assert_eq!(PgaccelGeomType::LineString as u32, 1);
        assert_eq!(PgaccelGeomType::Polygon as u32, 2);
        assert_eq!(PgaccelGeomType::Unknown as u32, 99);
    }

    #[test]
    fn geom_type_equality() {
        assert_eq!(PgaccelGeomType::Point, PgaccelGeomType::Point);
        assert_ne!(PgaccelGeomType::Point, PgaccelGeomType::Polygon);
    }

    // -----------------------------------------------------------------------
    // Derive coverage (Debug, Clone)
    // -----------------------------------------------------------------------

    #[test]
    fn device_info_debug_and_clone() {
        let info = pgaccel_get_device_info();
        let cloned = info.clone();
        assert_eq!(cloned.compute_units, 0);
        // Debug derive produces non-empty output
        let dbg = format!("{info:?}");
        assert!(dbg.contains("PgaccelDeviceInfo"));
    }

    #[test]
    fn platform_caps_debug_and_clone() {
        let caps = pgaccel_get_caps();
        let cloned = caps.clone();
        assert_eq!(cloned.compute_units, 0);
        let dbg = format!("{caps:?}");
        assert!(dbg.contains("PgaccelPlatformCaps"));
    }

    #[test]
    fn status_debug_and_clone() {
        let s = PgaccelStatus::ErrorOom;
        let cloned = s;
        assert_eq!(cloned, PgaccelStatus::ErrorOom);
        let dbg = format!("{s:?}");
        assert!(dbg.contains("ErrorOom"));
    }

    #[test]
    fn geom_type_debug_and_clone() {
        let g = PgaccelGeomType::Polygon;
        let cloned = g;
        assert_eq!(cloned, PgaccelGeomType::Polygon);
        let dbg = format!("{g:?}");
        assert!(dbg.contains("Polygon"));
    }
}
