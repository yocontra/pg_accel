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
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelStatus {
    Ok = 0,
    ErrorInit = -1,
    ErrorNoDevice = -5,
    ErrorOom = -3,
    ErrorTimeout = -4,
    ErrorUnsupported = -2,
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
// Expression evaluator types (mirrors pgaccel_expr.h).
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

/// Tagged value — 16 bytes. Matches `pgaccel_val` in `pgaccel_expr.h`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelVal {
    pub tag: PgaccelValTag,
    pub data: u64, // Union: bool/i32/i64/f32/f64 reinterpreted
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
// Hash aggregation types (mirrors pgaccel_hash_agg.h).
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
// Hash join types (mirrors pgaccel_hash_join.h).
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
// Raster types for map algebra and reclassification.
// ---------------------------------------------------------------------------

/// Pixel type tag (mirrors `pgaccel_pixel_type` in `pgaccel_ffi.h`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelPixelType {
    Int8 = 0,
    Int16 = 1,
    Int32 = 2,
    Float32 = 3,
    Float64 = 4,
}

/// Map-algebra opcode (mirrors `pgaccel_op` in `pgaccel_ffi.h`).
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
/// (mirrors `pgaccel_expr_inst` in `pgaccel_ffi.h`).
///
/// The C union stores either `band_index` (i32) or `constant` (f64).
/// We use `f64` for the union since it is the larger type (8 bytes),
/// and `band_index` can be reinterpreted from the low bits.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelExprInst {
    pub op: PgaccelOp,
    /// Union: `band_index` (i32) or `constant` (f64).  Use the larger
    /// type so the struct size matches the C layout.
    pub arg: f64,
}

/// Map-algebra expression program (mirrors `pgaccel_expr` in `pgaccel_ffi.h`).
#[repr(C)]
pub struct PgaccelExpr {
    pub instructions: *mut PgaccelExprInst,
    pub inst_count: usize,
    pub band_count: usize,
}

/// Reclassification rule (mirrors `pgaccel_reclass_rule` in `pgaccel_ffi.h`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_field_names)]
pub struct PgaccelReclassRule {
    pub min_val: f64,
    pub max_val: f64,
    pub new_val: f64,
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

    /// Bulk bbox intersection test (fp64).
    pub fn pgaccel_bbox_intersects_bulk_f64(
        boxes_a: *const f64,
        count_a: usize,
        boxes_b: *const f64,
        count_b: usize,
        result: *mut u8,
        hit_count: *mut usize,
    ) -> PgaccelStatus;

    // -- Platform capability convenience predicates --

    pub fn pgaccel_fp64_available() -> bool;
    pub fn pgaccel_unified_memory() -> bool;
    pub fn pgaccel_ooo_queue_available() -> bool;

    // -- Memory pool (USM arena allocator) --

    pub fn pgaccel_alloc(bytes: usize) -> *mut std::ffi::c_void;
    pub fn pgaccel_free(ptr: *mut std::ffi::c_void);
    pub fn pgaccel_pool_reset();
    pub fn pgaccel_pool_bytes_used() -> usize;
    pub fn pgaccel_prefetch(ptr: *mut std::ffi::c_void, bytes: usize);

    // -- Sort kernels --

    pub fn pgaccel_sort_f32(data: *mut f32, count: usize) -> PgaccelStatus;
    pub fn pgaccel_sort_f64(data: *mut f64, count: usize) -> PgaccelStatus;
    pub fn pgaccel_sort_i32(data: *mut i32, count: usize) -> PgaccelStatus;
    pub fn pgaccel_sort_i64(data: *mut i64, count: usize) -> PgaccelStatus;

    /// Key-value sort: sorts keys and permutes indices to match.
    pub fn pgaccel_sort_kv_f32(keys: *mut f32, indices: *mut u32, count: usize) -> PgaccelStatus;

    /// Key-value sort (fp64 keys).
    pub fn pgaccel_sort_kv_f64(keys: *mut f64, indices: *mut u32, count: usize) -> PgaccelStatus;

    // -- Reduce kernels --

    pub fn pgaccel_reduce_sum_f32(
        data: *const f32,
        count: usize,
        result: *mut f32,
    ) -> PgaccelStatus;
    pub fn pgaccel_reduce_min_f32(
        data: *const f32,
        count: usize,
        result: *mut f32,
    ) -> PgaccelStatus;
    pub fn pgaccel_reduce_max_f32(
        data: *const f32,
        count: usize,
        result: *mut f32,
    ) -> PgaccelStatus;

    pub fn pgaccel_reduce_sum_f64(
        data: *const f64,
        count: usize,
        result: *mut f64,
    ) -> PgaccelStatus;
    pub fn pgaccel_reduce_min_f64(
        data: *const f64,
        count: usize,
        result: *mut f64,
    ) -> PgaccelStatus;
    pub fn pgaccel_reduce_max_f64(
        data: *const f64,
        count: usize,
        result: *mut f64,
    ) -> PgaccelStatus;

    pub fn pgaccel_reduce_sum_i64(
        data: *const i64,
        count: usize,
        result: *mut i64,
    ) -> PgaccelStatus;

    /// Count nonzero bytes in mask (popcount).
    pub fn pgaccel_reduce_count(mask: *const u8, count: usize, result: *mut usize)
    -> PgaccelStatus;

    // -- H3 cell operations --

    pub fn pgaccel_h3_get_resolution_bulk(
        cells: *const u64,
        count: usize,
        resolutions: *mut i32,
    ) -> PgaccelStatus;

    pub fn pgaccel_h3_cell_to_parent_bulk(
        cells: *const u64,
        count: usize,
        parent_res: i32,
        parents: *mut u64,
    ) -> PgaccelStatus;

    pub fn pgaccel_h3_grid_distance_bulk(
        cells_a: *const u64,
        cells_b: *const u64,
        count: usize,
        distances: *mut i32,
    ) -> PgaccelStatus;

    pub fn pgaccel_h3_lat_lng_to_cell_bulk(
        lat_array: *const std::ffi::c_void,
        lng_array: *const std::ffi::c_void,
        count: usize,
        resolution: i32,
        use_fp64: i32,
        cell_ids: *mut u64,
        valid: *mut u8,
    ) -> PgaccelStatus;

    // -- Raster operations --

    pub fn pgaccel_map_algebra(
        band_pixels: *const *const std::ffi::c_void,
        pixel_count: usize,
        pixel_type: i32,
        expr: *const PgaccelExpr,
        output_pixels: *mut std::ffi::c_void,
        nodata_mask: *mut u8,
    ) -> PgaccelStatus;

    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_raster_clip(
        rast_pixels: *const std::ffi::c_void,
        width: usize,
        height: usize,
        origin_x: f64,
        origin_y: f64,
        scale_x: f64,
        scale_y: f64,
        pixel_type: i32,
        clip_ring_xy: *const f32,
        vertex_count: usize,
        output_pixels: *mut std::ffi::c_void,
        nodata_mask: *mut u8,
    ) -> PgaccelStatus;

    pub fn pgaccel_raster_reclass(
        input_pixels: *const std::ffi::c_void,
        pixel_count: usize,
        input_type: i32,
        rules: *const PgaccelReclassRule,
        rule_count: usize,
        output_type: i32,
        output_pixels: *mut std::ffi::c_void,
    ) -> PgaccelStatus;

    // -- Expression evaluator kernels --

    /// Evaluate a predicate expression on a columnar batch.
    ///
    /// Results: +1 = TRUE, -1 = FALSE, 0 = UNCERTAIN (CPU recheck).
    pub fn pgaccel_expr_eval_predicate(
        program: *const PgaccelExprProgram,
        batch: *const PgaccelBatch,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// Evaluate a projection expression on a columnar batch.
    pub fn pgaccel_expr_eval_project(
        program: *const PgaccelExprProgram,
        batch: *const PgaccelBatch,
        output: *mut PgaccelVal,
        uncertain: *mut u8,
    ) -> PgaccelStatus;

    // -- Expression template kernels --

    /// Template: col <cmp> const.
    pub fn pgaccel_expr_template_cmp_const(
        batch: *const PgaccelBatch,
        col_idx: u32,
        cmp_opcode: u16,
        const_val: f64,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// Template: col BETWEEN lo AND hi.
    pub fn pgaccel_expr_template_between(
        batch: *const PgaccelBatch,
        col_idx: u32,
        lo: f64,
        hi: f64,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// Template: col IN (values...).
    pub fn pgaccel_expr_template_in_list(
        batch: *const PgaccelBatch,
        col_idx: u32,
        values: *const f64,
        value_count: usize,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// Template: col IS NULL / IS NOT NULL.
    pub fn pgaccel_expr_template_is_null(
        batch: *const PgaccelBatch,
        col_idx: u32,
        check_not_null: bool,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// Template: col1 <cmp1> const1 AND col2 <cmp2> const2.
    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_expr_template_two_pred_and(
        batch: *const PgaccelBatch,
        col1_idx: u32,
        cmp1_opcode: u16,
        const1_val: f64,
        col2_idx: u32,
        cmp2_opcode: u16,
        const2_val: f64,
        results: *mut i8,
    ) -> PgaccelStatus;

    // -- Hash join kernels --

    /// Build a hash table from inner relation keys.
    pub fn pgaccel_hash_join_build(
        keys: *const std::ffi::c_void,
        null_mask: *const u8,
        indices: *const u32,
        count: usize,
        key_type: PgaccelKeyType,
    ) -> *mut PgaccelHashTable;

    /// Free a hash table.
    pub fn pgaccel_hash_join_free(ht: *mut PgaccelHashTable);

    /// Probe the hash table with outer keys.
    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_hash_join_probe(
        ht: *const PgaccelHashTable,
        outer_keys: *const std::ffi::c_void,
        outer_null_mask: *const u8,
        outer_count: usize,
        match_pairs: *mut u32,
        max_matches: usize,
        match_count: *mut usize,
    ) -> PgaccelStatus;

    // -- Hash aggregation kernels --

    /// Perform grouped aggregation on columnar data.
    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_hash_agg_execute(
        group_keys: *const std::ffi::c_void,
        group_null_mask: *const u8,
        row_count: usize,
        key_type: i32,
        value_cols: *const *const std::ffi::c_void,
        value_nulls: *const *const u8,
        value_types: *const i32,
        agg_cols: *const PgaccelAggCol,
        num_aggs: usize,
    ) -> *mut PgaccelAggState;

    /// Get the number of groups in the aggregation result.
    pub fn pgaccel_agg_group_count(state: *const PgaccelAggState) -> usize;

    /// Get the group keys as a contiguous buffer.
    pub fn pgaccel_agg_get_group_keys(state: *const PgaccelAggState) -> *const std::ffi::c_void;

    /// Get aggregate results for one aggregate column.
    pub fn pgaccel_agg_get_results(state: *const PgaccelAggState, agg_idx: usize) -> *const f64;

    /// Get per-group row counts.
    pub fn pgaccel_agg_get_counts(state: *const PgaccelAggState) -> *const i64;

    /// Free aggregation state.
    pub fn pgaccel_agg_free(state: *mut PgaccelAggState);

    // -- Window function kernels --

    /// Compute ROW_NUMBER within each partition.
    pub fn pgaccel_window_row_number(
        partition_starts: *const u8,
        count: usize,
        results: *mut i64,
    ) -> PgaccelStatus;

    /// Compute RANK within each partition (requires sorted input).
    pub fn pgaccel_window_rank(
        partition_starts: *const u8,
        sort_keys: *const f64,
        count: usize,
        results: *mut i64,
    ) -> PgaccelStatus;

    /// Compute DENSE_RANK within each partition.
    pub fn pgaccel_window_dense_rank(
        partition_starts: *const u8,
        sort_keys: *const f64,
        count: usize,
        results: *mut i64,
    ) -> PgaccelStatus;

    /// Compute running SUM within each partition (Kahan compensated).
    pub fn pgaccel_window_sum(
        partition_starts: *const u8,
        values: *const f64,
        null_mask: *const u8,
        count: usize,
        results: *mut f64,
    ) -> PgaccelStatus;

    /// Compute running COUNT within each partition.
    pub fn pgaccel_window_count(
        partition_starts: *const u8,
        null_mask: *const u8,
        count: usize,
        results: *mut i64,
    ) -> PgaccelStatus;

    /// Compute LAG(value, offset, default) within each partition.
    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_window_lag(
        partition_starts: *const u8,
        values: *const f64,
        null_mask: *const u8,
        count: usize,
        offset: i32,
        default_val: f64,
        results: *mut f64,
        result_nulls: *mut u8,
    ) -> PgaccelStatus;

    /// Compute LEAD(value, offset, default) within each partition.
    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_window_lead(
        partition_starts: *const u8,
        values: *const f64,
        null_mask: *const u8,
        count: usize,
        offset: i32,
        default_val: f64,
        results: *mut f64,
        result_nulls: *mut u8,
    ) -> PgaccelStatus;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem;

    // -----------------------------------------------------------------------
    // PgaccelStatus discriminants and is_ok
    // -----------------------------------------------------------------------

    #[test]
    fn status_discriminant_values() {
        assert_eq!(PgaccelStatus::Ok as i32, 0);
        assert_eq!(PgaccelStatus::ErrorInit as i32, -1);
        assert_eq!(PgaccelStatus::ErrorNoDevice as i32, -5);
        assert_eq!(PgaccelStatus::ErrorOom as i32, -3);
        assert_eq!(PgaccelStatus::ErrorTimeout as i32, -4);
        assert_eq!(PgaccelStatus::ErrorUnsupported as i32, -2);
    }

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
    fn geom_type_discriminant_values() {
        assert_eq!(PgaccelGeomType::Point as u32, 0);
        assert_eq!(PgaccelGeomType::LineString as u32, 1);
        assert_eq!(PgaccelGeomType::Polygon as u32, 2);
        assert_eq!(PgaccelGeomType::Unknown as u32, 99);
    }

    // -----------------------------------------------------------------------
    // Struct size/alignment sanity (repr(C) stability)
    // -----------------------------------------------------------------------

    #[test]
    fn device_info_is_repr_c_and_nonzero_size() {
        let size = mem::size_of::<PgaccelDeviceInfo>();
        let align = mem::align_of::<PgaccelDeviceInfo>();
        // Must be large enough to hold 128 + 64 c_chars + several fields
        assert!(size >= 128 + 64, "PgaccelDeviceInfo too small: {size}");
        assert!(
            align >= mem::align_of::<usize>(),
            "alignment too small: {align}"
        );
    }

    #[test]
    fn platform_caps_is_repr_c_and_nonzero_size() {
        let size = mem::size_of::<PgaccelPlatformCaps>();
        let align = mem::align_of::<PgaccelPlatformCaps>();
        // Must be large enough to hold 64 c_chars + several fields
        assert!(size >= 64, "PgaccelPlatformCaps too small: {size}");
        assert!(
            align >= mem::align_of::<usize>(),
            "alignment too small: {align}"
        );
    }

    #[test]
    fn geometry_struct_is_repr_c_and_nonzero_size() {
        let size = mem::size_of::<PgaccelGeometry>();
        let align = mem::align_of::<PgaccelGeometry>();
        // Must hold at least the enum tag + 3 pointers + 2 usizes
        assert!(size > 0, "PgaccelGeometry has zero size");
        assert!(
            align >= mem::align_of::<*const f32>(),
            "alignment too small: {align}"
        );
    }

    // -----------------------------------------------------------------------
    // Clone / Debug / PartialEq derive coverage
    // -----------------------------------------------------------------------

    #[test]
    fn status_clone_debug_partial_eq() {
        let a = PgaccelStatus::ErrorTimeout;
        let b = a;
        assert_eq!(a, b);
        assert_ne!(a, PgaccelStatus::Ok);
        let dbg = format!("{a:?}");
        assert!(dbg.contains("ErrorTimeout"));
    }

    #[test]
    fn geom_type_clone_debug_partial_eq() {
        let a = PgaccelGeomType::LineString;
        let b = a;
        assert_eq!(a, b);
        assert_ne!(a, PgaccelGeomType::Unknown);
        let dbg = format!("{a:?}");
        assert!(dbg.contains("LineString"));
    }

    #[test]
    fn device_info_debug_clone() {
        let info = PgaccelDeviceInfo {
            device_name: [0; 128],
            backend_name: [0; 64],
            compute_units: 8,
            max_alloc_bytes: 1024,
            has_fp64: true,
            has_atomic64: false,
            is_unified_memory: false,
        };
        let cloned = info.clone();
        assert!(cloned.has_fp64);
        assert_eq!(cloned.max_alloc_bytes, 1024);
        assert_eq!(cloned.compute_units, 8);
        let dbg = format!("{info:?}");
        assert!(dbg.contains("PgaccelDeviceInfo"));
    }

    #[test]
    fn platform_caps_debug_clone() {
        let caps = PgaccelPlatformCaps {
            has_fp64: false,
            has_atomic64: true,
            has_ooo_queue: false,
            is_unified_memory: true,
            max_alloc_bytes: 2048,
            compute_units: 16,
            backend_name: [0; 64],
        };
        let cloned = caps.clone();
        assert!(cloned.has_atomic64);
        assert_eq!(cloned.compute_units, 16);
        let dbg = format!("{caps:?}");
        assert!(dbg.contains("PgaccelPlatformCaps"));
    }
}
