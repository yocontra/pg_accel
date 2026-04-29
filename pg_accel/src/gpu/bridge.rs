//! FFI bridge to `libpgaccel_kernels` (AdaptiveCpp/SYCL GPU library).
//!
//! The declarations here mirror `pgaccel-kernels/include/pgaccel_ffi.h`
//! exactly. The kernel library is linked unconditionally; there is no
//! no-GPU build and no CPU fallback.

pub use super::types::{
    PgaccelAggCol, PgaccelAggFunc, PgaccelAggState, PgaccelBatch, PgaccelDeviceInfo, PgaccelExpr,
    PgaccelExprInst, PgaccelExprInstruction, PgaccelExprProgram, PgaccelGeomType, PgaccelGeometry,
    PgaccelHashTable, PgaccelKeyType, PgaccelOp, PgaccelPixelType, PgaccelPlatformCaps,
    PgaccelReclassRule, PgaccelReduceCol, PgaccelStatus, PgaccelVal, PgaccelValTag,
};

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
    ///
    /// Exposed as `pgaccel_get_device_info_raw` so the public wrapper
    /// below can apply process-wide overrides (e.g.
    /// `PGACCEL_FORCE_SOFT_FP64_COST`) before the struct reaches the rest
    /// of the crate.
    #[link_name = "pgaccel_get_device_info"]
    fn pgaccel_get_device_info_raw() -> PgaccelDeviceInfo;

    /// Return platform-level capability flags.
    ///
    /// Exposed as `pgaccel_get_caps_raw` so the public wrapper below can
    /// apply process-wide overrides (e.g. `PGACCEL_FORCE_SOFT_FP64_COST`)
    /// before the caps struct reaches the rest of the crate.
    #[link_name = "pgaccel_get_caps"]
    fn pgaccel_get_caps_raw() -> PgaccelPlatformCaps;

    /// Pre-fork warmup: initialize Metal/SkyLight in the postmaster before
    /// fork. Safe to call from `_PG_init()` — does not spawn threads.
    pub fn pgaccel_prefork_warmup();

    // -- GPU execution observability --

    /// Number of kernel invocations that ran on GPU since last reset.
    pub fn pgaccel_gpu_exec_count() -> u64;

    /// Reset the GPU execution counter to zero.
    pub fn pgaccel_reset_gpu_exec_count();

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

    /// Dedicated bulk point-in-polygon: flat point array vs single polygon.
    pub fn pgaccel_point_in_polygon_bulk(
        points_xy: *const f32,
        point_count: usize,
        poly_bbox: *const f32,
        poly_coords: *const f32,
        poly_coord_count: usize,
        ring_offsets: *const u32,
        ring_count: usize,
        results: *mut i8,
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

    /// Key-value sort (i32 keys).
    pub fn pgaccel_sort_kv_i32(keys: *mut i32, indices: *mut u32, count: usize) -> PgaccelStatus;

    /// Key-value sort (i64 keys).
    pub fn pgaccel_sort_kv_i64(keys: *mut i64, indices: *mut u32, count: usize) -> PgaccelStatus;

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

    // -- Fused multi-aggregate reduce kernels (Fix Agent 4, 2026-04-11) --
    //
    // Single-pass SUM+MIN+MAX+COUNT over the same input column via a
    // single kernel launch.

    pub fn pgaccel_reduce_multi_f32(
        data: *const f32,
        count: usize,
        out_sum: *mut f32,
        out_min: *mut f32,
        out_max: *mut f32,
        out_count: *mut i64,
    ) -> PgaccelStatus;

    pub fn pgaccel_reduce_multi_f64(
        data: *const f64,
        count: usize,
        out_sum: *mut f64,
        out_min: *mut f64,
        out_max: *mut f64,
        out_count: *mut i64,
    ) -> PgaccelStatus;

    pub fn pgaccel_reduce_multi_i64(
        data: *const i64,
        count: usize,
        out_sum: *mut i64,
        out_min: *mut i64,
        out_max: *mut i64,
        out_count: *mut i64,
    ) -> PgaccelStatus;

    // -- sum_sq and fused stats (count, sum, sum_sq) for partial-agg AVG/STDDEV --
    //
    // sum_sq accumulates Σ(x²) in double regardless of input element type.
    // stats fuses count, sum, sum_sq into a single kernel launch.

    pub fn pgaccel_reduce_sum_sq_f32(
        data: *const f32,
        count: usize,
        result: *mut f64,
    ) -> PgaccelStatus;

    pub fn pgaccel_reduce_sum_sq_f64(
        data: *const f64,
        count: usize,
        result: *mut f64,
    ) -> PgaccelStatus;

    pub fn pgaccel_reduce_stats_f32(
        data: *const f32,
        count: usize,
        out_count: *mut u64,
        out_sum: *mut f64,
        out_sum_sq: *mut f64,
    ) -> PgaccelStatus;

    pub fn pgaccel_reduce_stats_f64(
        data: *const f64,
        count: usize,
        out_count: *mut u64,
        out_sum: *mut f64,
        out_sum_sq: *mut f64,
    ) -> PgaccelStatus;

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

    // -- Fused filter + multi-reduce kernels --

    /// Fused filter + multi-column reduce in a single GPU pass (f32).
    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_fused_filter_multi_reduce_f32(
        filter_data: *const f32,
        n: usize,
        cmp_op: i32,
        cmp_val: f32,
        cols: *const PgaccelReduceCol,
        num_cols: usize,
        results: *mut f32,
        pass_count: *mut usize,
    ) -> PgaccelStatus;
}

// ---------------------------------------------------------------------------
// Capability-probe wrappers (env-var overrides)
// ---------------------------------------------------------------------------

/// Returns `true` if `PGACCEL_FORCE_SOFT_FP64_COST=1` was set in the
/// environment at process start. Test-only knob: forces
/// `has_native_fp64 = false` on the device info / caps structs so the
/// cost model exercises the soft-fp64 costing path even on hardware that
/// has native fp64 (CUDA/ROCm/L0). No effect on kernel selection — fp64
/// dispatch is unconditional now that the AdaptiveCpp soft-fp64 libkernel
/// is always available.
///
/// Read once at first call, then cached for the lifetime of the process —
/// env changes after startup are ignored on purpose.
fn force_soft_fp64_cost() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    // 0 = not queried, 1 = false, 2 = true.
    static CACHED: AtomicU8 = AtomicU8::new(0);
    match CACHED.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let forced = std::env::var("PGACCEL_FORCE_SOFT_FP64_COST")
                .ok()
                .is_some_and(|v| v == "1");
            CACHED.store(if forced { 2 } else { 1 }, Ordering::Relaxed);
            forced
        }
    }
}

/// Return information about the selected compute device, applying any
/// process-wide capability overrides (e.g. `PGACCEL_FORCE_SOFT_FP64_COST`).
///
/// # Safety
/// `pgaccel_init` must have been called. If not, the returned struct is
/// zeroed and `has_native_fp64` may still be flipped off by the force env
/// var (to exercise slow-path costing).
#[must_use]
pub unsafe fn pgaccel_get_device_info() -> PgaccelDeviceInfo {
    // SAFETY: forwards to the underlying C FFI symbol; caller's safety
    // contract already requires pgaccel_init() to have been called.
    let mut info = unsafe { pgaccel_get_device_info_raw() };
    if force_soft_fp64_cost() {
        info.has_native_fp64 = false;
    }
    info
}

/// Return platform-level capability flags, applying any process-wide
/// capability overrides (e.g. `PGACCEL_FORCE_SOFT_FP64_COST`).
///
/// `has_atomic64` comes straight from the AdaptiveCpp device probe
/// (`sycl::aspect::atomic64`) — it's set by `populate_caps()` in
/// `pgaccel-kernels/src/device_manager.cpp` and read through FFI here.
///
/// # Safety
/// `pgaccel_init` must have been called. If not, the returned struct is
/// zeroed and `has_native_fp64` may still be flipped off by the force env
/// var (to exercise slow-path costing).
#[must_use]
pub unsafe fn pgaccel_get_caps() -> PgaccelPlatformCaps {
    // SAFETY: forwards to the underlying C FFI symbol; caller's safety
    // contract already requires pgaccel_init() to have been called.
    let mut caps = unsafe { pgaccel_get_caps_raw() };
    if force_soft_fp64_cost() {
        caps.has_native_fp64 = false;
    }
    caps
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::mem;

    use super::*;

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

    #[test]
    fn expr_instruction_size_is_8_bytes() {
        // PgaccelExprInstruction: u16 opcode + u16 pad + u32 arg = 8 bytes
        assert_eq!(mem::size_of::<PgaccelExprInstruction>(), 8);
    }

    #[test]
    fn expr_instruction_alignment() {
        assert!(mem::align_of::<PgaccelExprInstruction>() >= mem::align_of::<u32>());
    }

    #[test]
    fn pgaccel_val_size_is_16_bytes() {
        // tag (i32 = 4 bytes) + padding (4 bytes) + data (u64 = 8 bytes) = 16
        assert_eq!(mem::size_of::<PgaccelVal>(), 16);
    }

    #[test]
    fn pgaccel_val_alignment() {
        assert!(mem::align_of::<PgaccelVal>() >= mem::align_of::<u64>());
    }

    #[test]
    fn agg_col_size_and_alignment() {
        let size = mem::size_of::<PgaccelAggCol>();
        let align = mem::align_of::<PgaccelAggCol>();
        // func (repr(C) enum) + col_idx (usize)
        assert!(
            size >= mem::size_of::<usize>(),
            "PgaccelAggCol too small: {size}"
        );
        assert!(
            align >= mem::align_of::<usize>(),
            "alignment too small: {align}"
        );
    }

    #[test]
    fn reclass_rule_size_is_three_f64s() {
        // min_val + max_val + new_val = 3 * 8 = 24 bytes
        assert_eq!(mem::size_of::<PgaccelReclassRule>(), 24);
    }

    #[test]
    fn reclass_rule_alignment() {
        assert_eq!(
            mem::align_of::<PgaccelReclassRule>(),
            mem::align_of::<f64>()
        );
    }

    #[test]
    fn expr_inst_size_and_alignment() {
        let size = mem::size_of::<PgaccelExprInst>();
        let align = mem::align_of::<PgaccelExprInst>();
        // op (repr(C) enum, at least 4 bytes) + arg (f64, 8 bytes) + padding
        assert!(size >= 12, "PgaccelExprInst too small: {size}");
        assert!(
            align >= mem::align_of::<f64>(),
            "alignment too small: {align}"
        );
    }

    #[test]
    fn batch_struct_nonzero_size() {
        let size = mem::size_of::<PgaccelBatch>();
        // 2 usizes + 3 pointers
        assert!(
            size >= 5 * mem::size_of::<usize>(),
            "PgaccelBatch too small: {size}"
        );
    }

    #[test]
    fn expr_program_struct_nonzero_size() {
        let size = mem::size_of::<PgaccelExprProgram>();
        // 2 pointers + 4 usizes
        assert!(
            size >= 6 * mem::size_of::<usize>(),
            "PgaccelExprProgram too small: {size}"
        );
    }

    #[test]
    fn expr_struct_nonzero_size() {
        let size = mem::size_of::<PgaccelExpr>();
        // 1 pointer + 2 usizes
        assert!(
            size >= 3 * mem::size_of::<usize>(),
            "PgaccelExpr too small: {size}"
        );
    }

    // -----------------------------------------------------------------------
    // Enum variant values match expected C constants
    // -----------------------------------------------------------------------

    #[test]
    fn val_tag_discriminant_values_match_c() {
        assert_eq!(PgaccelValTag::Null as i32, 0);
        assert_eq!(PgaccelValTag::Bool as i32, 1);
        assert_eq!(PgaccelValTag::Int32 as i32, 2);
        assert_eq!(PgaccelValTag::Int64 as i32, 3);
        assert_eq!(PgaccelValTag::Float32 as i32, 4);
        assert_eq!(PgaccelValTag::Float64 as i32, 5);
        assert_eq!(PgaccelValTag::Date as i32, 6);
        assert_eq!(PgaccelValTag::Timestamp as i32, 7);
    }

    #[test]
    fn agg_func_discriminant_values_match_c() {
        assert_eq!(PgaccelAggFunc::Sum as i32, 0);
        assert_eq!(PgaccelAggFunc::Min as i32, 1);
        assert_eq!(PgaccelAggFunc::Max as i32, 2);
        assert_eq!(PgaccelAggFunc::Count as i32, 3);
    }

    #[test]
    fn key_type_discriminant_values_match_c() {
        assert_eq!(PgaccelKeyType::Int32 as i32, 0);
        assert_eq!(PgaccelKeyType::Int64 as i32, 1);
        assert_eq!(PgaccelKeyType::Float64 as i32, 2);
    }

    #[test]
    fn pixel_type_discriminant_values_match_c() {
        assert_eq!(PgaccelPixelType::Int8 as i32, 0);
        assert_eq!(PgaccelPixelType::Int16 as i32, 1);
        assert_eq!(PgaccelPixelType::Int32 as i32, 2);
        assert_eq!(PgaccelPixelType::Float32 as i32, 3);
        assert_eq!(PgaccelPixelType::Float64 as i32, 4);
    }

    #[test]
    fn op_discriminant_values_match_c() {
        assert_eq!(PgaccelOp::LoadBand as i32, 0);
        assert_eq!(PgaccelOp::LoadConst as i32, 1);
        assert_eq!(PgaccelOp::Add as i32, 2);
        assert_eq!(PgaccelOp::Sub as i32, 3);
        assert_eq!(PgaccelOp::Mul as i32, 4);
        assert_eq!(PgaccelOp::Div as i32, 5);
        assert_eq!(PgaccelOp::Sqrt as i32, 6);
        assert_eq!(PgaccelOp::Abs as i32, 7);
        assert_eq!(PgaccelOp::Log as i32, 8);
        assert_eq!(PgaccelOp::Pow as i32, 9);
        assert_eq!(PgaccelOp::Gt as i32, 10);
        assert_eq!(PgaccelOp::Lt as i32, 11);
        assert_eq!(PgaccelOp::Eq as i32, 12);
        assert_eq!(PgaccelOp::Select as i32, 13);
    }

    // -----------------------------------------------------------------------
    // PgaccelVal type conversion helpers
    // -----------------------------------------------------------------------

    #[test]
    fn val_null_constructor() {
        let v = PgaccelVal::null();
        assert_eq!(v.tag, PgaccelValTag::Null);
        assert_eq!(v.data, 0);
    }

    #[test]
    fn val_from_i32_roundtrips() {
        let v = PgaccelVal::from_i32(-42);
        assert_eq!(v.tag, PgaccelValTag::Int32);
        assert_eq!(v.data as i32, -42);
    }

    #[test]
    fn val_from_i64_roundtrips() {
        let v = PgaccelVal::from_i64(i64::MAX);
        assert_eq!(v.tag, PgaccelValTag::Int64);
        assert_eq!(v.data as i64, i64::MAX);
    }

    #[test]
    fn val_from_f64_roundtrips() {
        let v = PgaccelVal::from_f64(std::f64::consts::PI);
        assert_eq!(v.tag, PgaccelValTag::Float64);
        assert_eq!(f64::from_bits(v.data), std::f64::consts::PI);
    }

    #[test]
    fn val_from_f32_roundtrips() {
        let v = PgaccelVal::from_f32(1.5f32);
        assert_eq!(v.tag, PgaccelValTag::Float32);
        assert_eq!(f32::from_bits(v.data as u32), 1.5f32);
    }

    #[test]
    fn val_from_bool_true_and_false() {
        let t = PgaccelVal::from_bool(true);
        assert_eq!(t.tag, PgaccelValTag::Bool);
        assert_eq!(t.data, 1);

        let f = PgaccelVal::from_bool(false);
        assert_eq!(f.tag, PgaccelValTag::Bool);
        assert_eq!(f.data, 0);
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
            has_native_fp64: true,
            has_atomic64: false,
            is_unified_memory: false,
        };
        let cloned = info.clone();
        assert!(cloned.has_native_fp64);
        assert_eq!(cloned.max_alloc_bytes, 1024);
        assert_eq!(cloned.compute_units, 8);
        let dbg = format!("{info:?}");
        assert!(dbg.contains("PgaccelDeviceInfo"));
    }

    #[test]
    fn platform_caps_debug_clone() {
        let caps = PgaccelPlatformCaps {
            has_native_fp64: false,
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

    #[test]
    fn agg_col_debug_clone() {
        let col = PgaccelAggCol {
            func: PgaccelAggFunc::Count,
            col_idx: 7,
        };
        let cloned = col;
        assert_eq!(cloned.func, PgaccelAggFunc::Count);
        assert_eq!(cloned.col_idx, 7);
        let dbg = format!("{col:?}");
        assert!(dbg.contains("PgaccelAggCol"));
    }

    #[test]
    fn reclass_rule_debug_clone() {
        let rule = PgaccelReclassRule {
            min_val: -10.0,
            max_val: 10.0,
            new_val: 0.0,
        };
        let cloned = rule;
        assert!((cloned.min_val - (-10.0)).abs() < f64::EPSILON);
        let dbg = format!("{rule:?}");
        assert!(dbg.contains("PgaccelReclassRule"));
    }

    #[test]
    fn pixel_type_clone_debug_partial_eq() {
        let a = PgaccelPixelType::Float32;
        let b = a;
        assert_eq!(a, b);
        assert_ne!(a, PgaccelPixelType::Int8);
        let dbg = format!("{a:?}");
        assert!(dbg.contains("Float32"));
    }

    #[test]
    fn op_clone_debug_partial_eq() {
        let a = PgaccelOp::Sqrt;
        let b = a;
        assert_eq!(a, b);
        assert_ne!(a, PgaccelOp::Add);
        let dbg = format!("{a:?}");
        assert!(dbg.contains("Sqrt"));
    }

    #[test]
    fn key_type_clone_debug_partial_eq() {
        let a = PgaccelKeyType::Float64;
        let b = a;
        assert_eq!(a, b);
        assert_ne!(a, PgaccelKeyType::Int32);
        let dbg = format!("{a:?}");
        assert!(dbg.contains("Float64"));
    }
}
