//! FFI bridge to `libpgaccel_kernels` (AdaptiveCpp/SYCL GPU library).
//!
//! The declarations here mirror `pgaccel-kernels/include/pgaccel_ffi.h`
//! exactly. The kernel library is linked unconditionally; there is no
//! no-GPU build and no CPU fallback.

pub use super::spatial::{
    PgaccelSpatialRecheckCompactRequest, PgaccelSpatialRecheckPatchRequest,
    PgaccelSpatialResidentRequest, PgaccelSpatialWorkspace,
};
pub use super::types::{
    PgaccelAggState, PgaccelBatch, PgaccelDeviceInfo, PgaccelExpr, PgaccelExprInst,
    PgaccelExprInstruction, PgaccelExprProgram, PgaccelExprUsmCol, PgaccelGeomType,
    PgaccelGeometry, PgaccelHashTable, PgaccelKeyType, PgaccelOp, PgaccelPixelType,
    PgaccelPlatformCaps, PgaccelReclassRule, PgaccelStatus, PgaccelVal, PgaccelValTag,
};

// ---------------------------------------------------------------------------
// Extern declarations — linked at load time against libpgaccel_kernels.
// ---------------------------------------------------------------------------
//
// Scope (honest, verified against `pgaccel-kernels/include/*.h` 2026-07-07):
// the declarations below cover every header symbol that has a current or
// staged Rust caller. Declarations match the C signatures exactly, but this
// is NOT the complete header surface. Header symbols with no Rust caller are
// intentionally NOT declared because an extern declaration carries no
// cross-checking value on its own (the linker does not type-check, so an
// unused declaration is dead surface that can silently drift):
//
//   - pgaccel_sort_u64                        (pgaccel_ffi.h:174)
//   - pgaccel_sort_kv_i32_device              (pgaccel_ffi.h:190)
//   - pgaccel_sort_kv_i32_nonnegative_device  (pgaccel_ffi.h:191)
//   - pgaccel_sort_window_overlap_probe       (pgaccel_ffi.h:80)
//   - pgaccel_archive_stats_snapshot          (pgaccel_ffi.h:126)
//   - pgaccel_archive_jit_cache_dir           (pgaccel_ffi.h:131)
//   - pgaccel_sort_{f32,f64,i32,i64}          (host tuplesort executor retired)
//   - pgaccel_sort_kv_{f32,f64,i32,i64}       (host tuplesort executor retired)
//   - pgaccel_topk_kv_{f32,f64,i32,i64}       (host top-k sort executor retired)
//
// When a caller for one of these lands, declare it here (through
// `bridge_status_fns!` if it returns `pgaccel_status`) in the same change.
//
// Status returns: every kernel entry point that returns `pgaccel_status` in
// C is declared `-> i32` (inside `bridge_status_fns!` below) and converted
// through `PgaccelStatus::from_raw` at the wrapper layer. Declaring the
// return as the fieldless `#[repr(i32)]` enum would be undefined behaviour
// for any out-of-range value coming back from C.
//
// `#[allow(dead_code)]` on the extern items is intentional and narrow
// (per anti-cheat ban #8 — not a module-scope blanket): it covers
// only the FFI mirror, with the reason documented above.

/// Declares raw `-> i32` externs plus converting public wrappers for every
/// kernel entry point whose C return type is `pgaccel_status`.
///
/// The raw extern lives in the private `raw` module and returns `i32`
/// exactly as the C ABI does. The generated public wrapper keeps the
/// original symbol name and argument list, forwards to the raw extern, and
/// converts through [`convert_status`] — the single point where unknown
/// status values are rejected (logged + counted, never assumed OK) and
/// every non-OK status is logged at error level with a per-domain failure
/// counter bump.
macro_rules! bridge_status_fns {
    ($(
        $(#[$meta:meta])*
        pub fn $name:ident( $($arg:ident : $ty:ty),* $(,)? ) -> PgaccelStatus;
    )*) => {
        /// Raw `-> i32` extern declarations for status-returning kernel
        /// entry points. Never call these directly — go through the
        /// like-named converting wrappers in `bridge`.
        mod raw {
            #[allow(unused_imports)] // reason: which mirror types appear in signatures varies
            use super::*;

            #[allow(dead_code)] // reason: ABI mirror of pgaccel headers; symbol set is load-bearing
            #[allow(clippy::too_many_arguments)] // reason: C kernel ABI dictates the arity
            unsafe extern "C" {
                $( pub fn $name( $($arg : $ty),* ) -> i32; )*
            }
        }
        $(
            $(#[$meta])*
            ///
            /// # Safety
            ///
            /// Direct C kernel entry point: the caller must uphold the
            /// pointer/count contract documented for this symbol in the
            /// pgaccel headers (buffers valid for the stated counts,
            /// `pgaccel_init()` called first).
            #[allow(dead_code)] // reason: ABI mirror of pgaccel headers; symbol set is load-bearing
            #[allow(clippy::too_many_arguments)] // reason: C kernel ABI dictates the arity
            #[must_use]
            #[inline]
            pub unsafe fn $name( $($arg : $ty),* ) -> PgaccelStatus {
                // SAFETY: forwards verbatim to the like-named C symbol; the
                // caller upholds that symbol's documented contract.
                let raw_status = unsafe { raw::$name( $($arg),* ) };
                convert_status(stringify!($name), raw_status)
            }
        )*
    };
}

/// Convert a raw C status `i32` into [`PgaccelStatus`] at the single
/// bridge conversion point.
///
/// - Unknown raw values (ABI drift / corruption) are logged at error level
///   with the raw value, counted via `counters::record_unknown_status`, and
///   mapped to [`PgaccelStatus::Error`] so no caller can confuse ABI drift
///   with a supported capability decline.
/// - Every non-OK status is logged at error level with the failing symbol
///   name and counted per kernel domain via
///   `counters::record_kernel_failure`. Typed domain dispatchers preserve the
///   status as a hard error; legacy compatibility wrappers may still expose
///   `None` where their public API predates [`crate::gpu::GpuResult`].
pub fn convert_status(func: &'static str, raw: i32) -> PgaccelStatus {
    match PgaccelStatus::from_raw(raw) {
        Ok(PgaccelStatus::Ok) => PgaccelStatus::Ok,
        Ok(status) => {
            let domain = super::counters::GpuFailureDomain::classify(func);
            super::counters::record_kernel_failure(domain);
            tracing::error!(
                target: "pg_accel::gpu",
                func,
                status = ?status,
                raw,
                domain = domain.as_str(),
                "GPU kernel dispatch failed"
            );
            status
        }
        Err(unknown) => {
            let domain = super::counters::GpuFailureDomain::classify(func);
            super::counters::record_kernel_failure(domain);
            super::counters::record_unknown_status();
            tracing::error!(
                target: "pg_accel::gpu",
                func,
                raw = unknown,
                domain = domain.as_str(),
                "GPU kernel returned an UNKNOWN status value (ABI drift or \
                 memory corruption on the C side); treating as generic Error"
            );
            PgaccelStatus::Error
        }
    }
}

bridge_status_fns! {
    /// Initialise the GPU runtime.  Must be called once before any other
    /// `pgaccel_*` function.
    pub fn pgaccel_init() -> PgaccelStatus;

    /// Tear down the GPU runtime and release all resources.
    pub fn pgaccel_shutdown() -> PgaccelStatus;

    // -- Spatial predicate kernels --

    /// Bulk point-in-ring test.
    ///
    /// Results: 1 = inside, -1 = outside, 0 = uncertain.
    ///
    /// `points_xy` / `ring_xy` mirror the C ABI (`pgaccel_ffi.h:338-343`):
    /// untyped `const void*` buffers whose element type is selected by
    /// `use_fp64` (`false` = f32, `true` = f64). Prefer the typed
    /// [`pgaccel_point_in_ring_bulk_f32`] / [`pgaccel_point_in_ring_bulk_f64`]
    /// wrappers, which make the buffer-type/flag pairing impossible to get
    /// wrong.
    pub fn pgaccel_point_in_ring_bulk(
        points_xy: *const std::ffi::c_void,
        point_count: usize,
        ring_xy: *const std::ffi::c_void,
        vertex_count: usize,
        use_fp64: bool,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// Bulk sphere distance computation (Haversine).
    ///
    /// Outputs distances in metres. `uncertain[i] = 1` means the GPU result
    /// was not classified as exact; pg_accel callers must reject it.
    ///
    /// `points_a` / `points_b` / `distances` mirror the C ABI
    /// (`pgaccel_ffi.h:345-350`): untyped `void*` buffers whose element type
    /// is selected by `use_fp64`. Prefer the typed
    /// [`pgaccel_sphere_distance_bulk_f32`] / [`pgaccel_sphere_distance_bulk_f64`]
    /// wrappers.
    pub fn pgaccel_sphere_distance_bulk(
        points_a: *const std::ffi::c_void,
        points_b: *const std::ffi::c_void,
        count: usize,
        use_fp64: bool,
        distances: *mut std::ffi::c_void,
        uncertain: *mut u8,
    ) -> PgaccelStatus;

    /// Bulk segment intersection test.
    ///
    /// Results: 1 = intersects, -1 = no, 0 = uncertain.
    ///
    /// `segs_a` / `segs_b` mirror the C ABI (`pgaccel_ffi.h:352-357`):
    /// untyped `const void*` buffers whose element type is selected by
    /// `use_fp64`. Prefer the typed
    /// [`pgaccel_segment_intersects_bulk_f32`] /
    /// [`pgaccel_segment_intersects_bulk_f64`] wrappers.
    pub fn pgaccel_segment_intersects_bulk(
        segs_a: *const std::ffi::c_void,
        segs_b: *const std::ffi::c_void,
        count: usize,
        use_fp64: bool,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// Bulk Shoelace area for single-ring polygons.
    ///
    /// CSR-style input: `coords` is a flat `[x, y, x, y, ...]` buffer
    /// with each row's vertices concatenated; `row_offsets` is a
    /// `[row_count + 1]` array of `coords` indices marking each row's
    /// starting xy pair. Output: one f64 area per row in `areas`.
    pub fn pgaccel_st_area_bulk(
        coords: *const std::ffi::c_void,
        row_offsets: *const u32,
        row_count: usize,
        use_fp64: bool,
        areas: *mut std::ffi::c_void,
    ) -> PgaccelStatus;

    /// Bulk Euclidean edge-length sum.
    ///
    /// `closed_ring` flags whether the wrap-around edge is included
    /// (Polygon perimeter) or not (open LineString length).
    pub fn pgaccel_st_length_bulk(
        coords: *const std::ffi::c_void,
        row_offsets: *const u32,
        row_count: usize,
        use_fp64: bool,
        closed_ring: bool,
        lengths: *mut std::ffi::c_void,
    ) -> PgaccelStatus;

    /// Linear row-wise spatial intersection classification.
    ///
    /// Pair `i` is `(geoms_a[i], geoms_b[i])`. Each result is 1 for definite
    /// true, -1 for definite false, or 0 for uncertain.
    pub fn pgaccel_spatial_intersects_pairwise(
        geoms_a: *const PgaccelGeometry,
        geoms_b: *const PgaccelGeometry,
        count: usize,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// Evaluate one resident fp64 spatial predicate or distance operation.
    ///
    /// The request owns no memory. Every lane and output pointer in the
    /// descriptor must reference device or shared USM in the active context.
    pub fn pgaccel_spatial_eval_resident_ex(
        request: *const PgaccelSpatialResidentRequest,
        detail: *mut i32,
    ) -> PgaccelStatus;

    /// Deprecated cross-product ABI. Non-empty inputs are unsupported.
    #[allow(dead_code)] // reason: ABI symbol retained for compatibility audits
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

    /// `ST_Equals` per-row predicate. Pair `i` writes
    /// `results[i] = 1 / -1 / 0` for DEFINITE TRUE / DEFINITE FALSE /
    /// UNCERTAIN. UNCERTAIN routes to PG. Agent 2A task 4.
    pub fn pgaccel_st_equals_bulk(
        geoms_a: *const PgaccelGeometry,
        geoms_b: *const PgaccelGeometry,
        count: usize,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// `ST_Touches` per-row predicate. Same shape / output convention
    /// as `pgaccel_st_equals_bulk`.
    pub fn pgaccel_st_touches_bulk(
        geoms_a: *const PgaccelGeometry,
        geoms_b: *const PgaccelGeometry,
        count: usize,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// `ST_Crosses` per-row predicate. Same shape / output convention
    /// as `pgaccel_st_equals_bulk`.
    pub fn pgaccel_st_crosses_bulk(
        geoms_a: *const PgaccelGeometry,
        geoms_b: *const PgaccelGeometry,
        count: usize,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// `ST_Overlaps` per-row predicate. Same shape / output convention
    /// as `pgaccel_st_equals_bulk`.
    pub fn pgaccel_st_overlaps_bulk(
        geoms_a: *const PgaccelGeometry,
        geoms_b: *const PgaccelGeometry,
        count: usize,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// Polygon-to-polygon Euclidean distance via vertex-to-edge
    /// boundary walks. CSR layout matches `pgaccel_st_area_bulk` and
    /// `pgaccel_st_length_bulk`. `uncertain[i] = 1` when the
    /// boundaries touch / overlap so PG re-checks for the interior
    /// containment case. Agent 2A task 3.
    pub fn pgaccel_st_distance_polygon_polygon_bulk(
        coords_a: *const f32,
        row_offsets_a: *const u32,
        coords_b: *const f32,
        row_offsets_b: *const u32,
        row_count: usize,
        distances: *mut f32,
        uncertain: *mut u8,
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

    // -- H3 cell operations --

    pub fn pgaccel_h3_get_resolution_bulk(
        cells: *const u64,
        count: usize,
        resolutions: *mut i32,
    ) -> PgaccelStatus;

    pub fn pgaccel_h3_get_base_cell_bulk(
        cells: *const u64,
        count: usize,
        base_cells: *mut i32,
    ) -> PgaccelStatus;

    pub fn pgaccel_h3_is_valid_cell_bulk(
        cells: *const u64,
        count: usize,
        valid: *mut u8,
    ) -> PgaccelStatus;

    pub fn pgaccel_h3_is_pentagon_bulk(
        cells: *const u64,
        count: usize,
        is_pent: *mut u8,
    ) -> PgaccelStatus;

    pub fn pgaccel_h3_is_res_class_iii_bulk(
        cells: *const u64,
        count: usize,
        is_class_iii: *mut u8,
    ) -> PgaccelStatus;

    pub fn pgaccel_h3_cell_to_parent_bulk(
        cells: *const u64,
        count: usize,
        parent_res: i32,
        parents: *mut u64,
    ) -> PgaccelStatus;

    /// Transform a resident H3 lane directly into a resident parent lane.
    pub fn pgaccel_h3_cell_to_parent_resident(
        cells: *const u64,
        nulls: *const u8,
        count: usize,
        parent_res: i32,
        parents: *mut u64,
    ) -> PgaccelStatus;

    /// Transform a resident H3 lane and classify invalid input precisely.
    pub fn pgaccel_h3_cell_to_parent_resident_ex(
        cells: *const u64,
        nulls: *const u8,
        count: usize,
        parent_res: i32,
        parents: *mut u64,
        detail: *mut i32,
    ) -> PgaccelStatus;

    pub fn pgaccel_h3_cell_to_parent_count_bulk(
        cells: *const u64,
        count: usize,
        parent_res: i32,
        out_state: *mut *mut PgaccelAggState,
    ) -> PgaccelStatus;

    pub fn pgaccel_h3_cell_to_center_child_bulk(
        cells: *const u64,
        count: usize,
        child_res: i32,
        children: *mut u64,
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

    pub fn pgaccel_h3_lat_lng_count_bulk(
        lat_array: *const f64,
        lng_array: *const f64,
        count: usize,
        resolution: i32,
        out_state: *mut *mut PgaccelAggState,
    ) -> PgaccelStatus;

    pub fn pgaccel_h3_lat_lng_count_bulk_f32_exact(
        lat_f32_array: *const f32,
        lng_f32_array: *const f32,
        lat_exact_array: *const f64,
        lng_exact_array: *const f64,
        count: usize,
        resolution: i32,
        out_state: *mut *mut PgaccelAggState,
    ) -> PgaccelStatus;

    pub fn pgaccel_h3_lat_lng_count_resident_bulk(
        lat_exact_array: *const f64,
        lng_exact_array: *const f64,
        lat_f32_array: *const f32,
        lng_f32_array: *const f32,
        count: usize,
        resolution: i32,
        out_state: *mut *mut PgaccelAggState,
    ) -> PgaccelStatus;

    // -- H3 variable-output kernels (Agent 5A; two-pass size+emit per pgaccel_ffi.h:405) --

    /// Size pass for `h3_grid_disk`: writes cumulative cell-count offsets
    /// `[0..=count]` so the executor can size the emit buffer.
    pub fn pgaccel_h3_grid_disk_output_size(
        cells: *const u64,
        count: usize,
        k: i32,
        out_offsets: *mut u32,
    ) -> PgaccelStatus;

    /// Emit pass for `h3_grid_disk`: writes neighbour cells into
    /// `out_cells[out_offsets[count]]` per the size pass.
    pub fn pgaccel_h3_grid_disk_emit(
        cells: *const u64,
        count: usize,
        k: i32,
        offsets: *const u32,
        out_cells: *mut u64,
    ) -> PgaccelStatus;

    /// Size pass for `h3_grid_ring_unsafe`: outputs only the k-th ring per input.
    pub fn pgaccel_h3_grid_ring_unsafe_output_size(
        cells: *const u64,
        count: usize,
        k: i32,
        out_offsets: *mut u32,
    ) -> PgaccelStatus;

    /// Emit pass for `h3_grid_ring_unsafe`.
    pub fn pgaccel_h3_grid_ring_unsafe_emit(
        cells: *const u64,
        count: usize,
        k: i32,
        offsets: *const u32,
        out_cells: *mut u64,
    ) -> PgaccelStatus;

    /// Size pass for `h3_polyfill`: writes cumulative cell-count offsets
    /// for each polygon (one ring per polygon for the first cut).
    pub fn pgaccel_h3_polyfill_output_size(
        coords: *const f32,
        ring_offsets: *const u32,
        ring_count: usize,
        resolution: i32,
        out_offsets: *mut u32,
    ) -> PgaccelStatus;

    /// Emit pass for `h3_polyfill`.
    pub fn pgaccel_h3_polyfill_emit(
        coords: *const f32,
        ring_offsets: *const u32,
        ring_count: usize,
        resolution: i32,
        offsets: *const u32,
        out_cells: *mut u64,
    ) -> PgaccelStatus;

    /// Size pass for `h3_cell_to_children`: writes cumulative child-count
    /// offsets per input cell at `child_res`.
    pub fn pgaccel_h3_cell_to_children_output_size(
        cells: *const u64,
        count: usize,
        child_res: i32,
        out_offsets: *mut u32,
    ) -> PgaccelStatus;

    /// Emit pass for `h3_cell_to_children`.
    pub fn pgaccel_h3_cell_to_children_emit(
        cells: *const u64,
        count: usize,
        child_res: i32,
        offsets: *const u32,
        out_children: *mut u64,
    ) -> PgaccelStatus;

    /// Size pass for `h3_cell_to_boundary`: writes cumulative DOUBLE-count
    /// offsets (12 per hexagon, 10 per pentagon).
    pub fn pgaccel_h3_cell_to_boundary_output_size(
        cells: *const u64,
        count: usize,
        out_offsets: *mut u32,
    ) -> PgaccelStatus;

    /// Emit pass for `h3_cell_to_boundary`. Output is interleaved lat/lng
    /// pairs per the kernel's documented unit (see h3_ops.cpp).
    pub fn pgaccel_h3_cell_to_boundary_emit(
        cells: *const u64,
        count: usize,
        offsets: *const u32,
        out_coords: *mut f64,
    ) -> PgaccelStatus;

    /// Size pass for `h3_cells_to_multi_polygon`. `out_ring_offsets` holds
    /// `ring_count + 1` cumulative double-count offsets; the kernel writes
    /// the realised ring count into `*out_ring_count` so the caller can
    /// correctly size both the offsets buffer and the coord emit buffer.
    pub fn pgaccel_h3_cells_to_multi_polygon_output_size(
        cells: *const u64,
        count: usize,
        out_ring_offsets: *mut u32,
        out_ring_count: *mut u32,
    ) -> PgaccelStatus;

    /// Emit pass for `h3_cells_to_multi_polygon`. `ring_count` must equal
    /// the value the size pass wrote to `out_ring_count`.
    pub fn pgaccel_h3_cells_to_multi_polygon_emit(
        cells: *const u64,
        count: usize,
        ring_offsets: *const u32,
        ring_count: u32,
        out_coords: *mut f64,
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

    // -- Raster extension kernels (Agent 3A) --

    /// Bilinear-interpolate `src` (`src_w` × `src_h`, fp32) to
    /// `dst` (`dst_w` × `dst_h`, fp32). Edge-clamped neighbours.
    pub fn pgaccel_raster_resample(
        src_pixels: *const f32,
        src_w: usize,
        src_h: usize,
        dst_w: usize,
        dst_h: usize,
        dst_pixels: *mut f32,
    ) -> PgaccelStatus;

    /// Per-pixel slope angle (degrees) via Horn's 3×3 gradient.
    pub fn pgaccel_raster_slope(
        src_pixels: *const f32,
        width: usize,
        height: usize,
        cell_size_x: f64,
        cell_size_y: f64,
        slope_out: *mut f32,
    ) -> PgaccelStatus;

    /// Per-pixel aspect (compass direction of steepest descent, degrees).
    pub fn pgaccel_raster_aspect(
        src_pixels: *const f32,
        width: usize,
        height: usize,
        aspect_out: *mut f32,
    ) -> PgaccelStatus;

    /// Per-pixel hillshade (shaded relief value [0, 255]).
    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_raster_hillshade(
        src_pixels: *const f32,
        width: usize,
        height: usize,
        cell_size_x: f64,
        cell_size_y: f64,
        sun_azimuth_deg: f64,
        sun_altitude_deg: f64,
        z_factor: f64,
        shade_out: *mut f32,
    ) -> PgaccelStatus;

    /// Per-point pixel-value lookup (`(x, y)` world coords → `f64`).
    /// Out-of-bounds points get NaN.
    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_raster_value(
        rast_pixels: *const f32,
        width: usize,
        height: usize,
        origin_x: f64,
        origin_y: f64,
        scale_x: f64,
        scale_y: f64,
        point_xy: *const f64,
        point_count: usize,
        output: *mut f64,
    ) -> PgaccelStatus;

    /// Per-row 6-scalar summary stats (`count`, `sum`, `mean`, `stddev`,
    /// `min`, `max`). Output buffer = `6 * sizeof(f64) * row_count`.
    pub fn pgaccel_raster_summarystats(
        rast_pixels: *const f32,
        row_count: usize,
        pixels_per_row: usize,
        nodata_masks: *const u8,
        output: *mut f64,
    ) -> PgaccelStatus;

    // -- Expression evaluator kernels --

    /// Evaluate a predicate expression on a columnar batch.
    ///
    /// Results: +1 = TRUE, -1 = FALSE, 0 = UNCERTAIN.
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

    /// Allocate device memory for resident cached columns and scratch.
    pub fn pgaccel_expr_device_alloc(
        bytes: usize,
        out: *mut *mut std::ffi::c_void,
    ) -> PgaccelStatus;

    /// Allocate device memory and copy host bytes into it once.
    pub fn pgaccel_expr_device_alloc_copy(
        src: *const std::ffi::c_void,
        bytes: usize,
        out: *mut *mut std::ffi::c_void,
    ) -> PgaccelStatus;

    /// Copy device-resident bytes into caller-owned host memory.
    pub fn pgaccel_expr_device_copy_to_host(
        dst: *mut std::ffi::c_void,
        src: *const std::ffi::c_void,
        bytes: usize,
    ) -> PgaccelStatus;

    // -- Descriptor-driven grouped aggregation --

    /// Validate a grouped-aggregate descriptor and return its exact workspace
    /// requirement. The output struct must carry the frozen ABI version/size.
    pub fn pgaccel_grouped_agg_workspace_requirements(
        desc: *const crate::engine::spec::abi::PgaccelGroupedAggDesc,
        out: *mut crate::engine::spec::abi::PgaccelGroupedAggWorkspaceReq,
    ) -> PgaccelStatus;

    /// Allocate an aligned grouped-aggregate workspace in shared/device USM.
    pub fn pgaccel_grouped_agg_workspace_alloc(
        bytes: usize,
        alignment: usize,
        space: i32,
        out: *mut *mut std::ffi::c_void,
    ) -> PgaccelStatus;

    /// Execute one grouped-aggregate lifecycle transition.
    pub fn pgaccel_grouped_agg_execute(
        desc: *const crate::engine::spec::abi::PgaccelGroupedAggDesc,
        out: *mut crate::engine::spec::abi::PgaccelGroupedAggOut,
    ) -> PgaccelStatus;

    /// Execute one transition and refine `PGACCEL_ERROR` with a grouped-agg
    /// device detail code. The legacy entry point remains exported for ABI
    /// compatibility.
    pub fn pgaccel_grouped_agg_execute_ex(
        desc: *const crate::engine::spec::abi::PgaccelGroupedAggDesc,
        out: *mut crate::engine::spec::abi::PgaccelGroupedAggOut,
        detail: *mut i32,
    ) -> PgaccelStatus;

    /// Template: col <cmp> const.
    pub fn pgaccel_expr_template_cmp_const(
        batch: *const PgaccelBatch,
        col_idx: u32,
        cmp_opcode: u16,
        const_val: f64,
        results: *mut i8,
    ) -> PgaccelStatus;

    /// Template: col <cmp> const, fused with COUNT(*) over TRUE rows.
    pub fn pgaccel_expr_template_cmp_const_count(
        batch: *const PgaccelBatch,
        col_idx: u32,
        cmp_opcode: u16,
        const_val: f64,
        true_count: *mut usize,
        uncertain_count: *mut usize,
    ) -> PgaccelStatus;

    /// Template: already-staged shared-USM column <cmp> const, fused with
    /// COUNT(*) over TRUE rows.
    pub fn pgaccel_expr_template_cmp_const_count_usm(
        col: PgaccelExprUsmCol,
        row_count: usize,
        cmp_opcode: u16,
        const_val: f64,
        true_count: *mut usize,
        uncertain_count: *mut usize,
    ) -> PgaccelStatus;

    pub fn pgaccel_expr_template_cmp_const_mask_usm(
        col: PgaccelExprUsmCol,
        row_count: usize,
        cmp_opcode: u16,
        const_val: f64,
        selection: *mut u8,
        true_count: *mut usize,
        uncertain_count: *mut usize,
    ) -> PgaccelStatus;

    pub fn pgaccel_expr_template_cmp_const_reduce_f32_usm(
        pred_col: PgaccelExprUsmCol,
        cmp_opcode: u16,
        const_val: f64,
        value_col: PgaccelExprUsmCol,
        row_count: usize,
        out_sum: *mut f32,
        out_min: *mut f32,
        out_max: *mut f32,
        out_value_count: *mut i64,
        true_count: *mut usize,
        uncertain_count: *mut usize,
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

    /// Template: col1 <cmp1> const1 AND col2 <cmp2> const2, fused with
    /// COUNT(*) over TRUE rows.
    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_expr_template_two_pred_and_count(
        batch: *const PgaccelBatch,
        col1_idx: u32,
        cmp1_opcode: u16,
        const1_val: f64,
        col2_idx: u32,
        cmp2_opcode: u16,
        const2_val: f64,
        true_count: *mut usize,
        uncertain_count: *mut usize,
    ) -> PgaccelStatus;

    /// Template: already-staged shared-USM two-predicate AND, fused with
    /// COUNT(*) over TRUE rows.
    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_expr_template_two_pred_and_count_usm(
        col1: PgaccelExprUsmCol,
        cmp1_opcode: u16,
        const1_val: f64,
        col2: PgaccelExprUsmCol,
        cmp2_opcode: u16,
        const2_val: f64,
        row_count: usize,
        true_count: *mut usize,
        uncertain_count: *mut usize,
    ) -> PgaccelStatus;

    pub fn pgaccel_expr_template_two_pred_and_mask_usm(
        col1: PgaccelExprUsmCol,
        cmp1_opcode: u16,
        const1_val: f64,
        col2: PgaccelExprUsmCol,
        cmp2_opcode: u16,
        const2_val: f64,
        row_count: usize,
        selection: *mut u8,
        true_count: *mut usize,
        uncertain_count: *mut usize,
    ) -> PgaccelStatus;

    pub fn pgaccel_expr_template_two_pred_and_reduce_f32_usm(
        col1: PgaccelExprUsmCol,
        cmp1_opcode: u16,
        const1_val: f64,
        col2: PgaccelExprUsmCol,
        cmp2_opcode: u16,
        const2_val: f64,
        value_col: PgaccelExprUsmCol,
        row_count: usize,
        out_sum: *mut f32,
        out_min: *mut f32,
        out_max: *mut f32,
        out_value_count: *mut i64,
        true_count: *mut usize,
        uncertain_count: *mut usize,
    ) -> PgaccelStatus;


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

    /// Count matching pairs without materializing pair output.
    pub fn pgaccel_hash_join_count(
        ht: *const PgaccelHashTable,
        outer_keys: *const std::ffi::c_void,
        outer_null_mask: *const u8,
        outer_count: usize,
        match_count: *mut usize,
    ) -> PgaccelStatus;

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

    // -- NestedLoop scalar inequality kernel --
    //
    // Mirrors `pgaccel-kernels/include/pgaccel_nested_loop_ineq.h`.
    // The kernel evaluates one scalar btree inequality (or a BETWEEN-shape
    // double inequality) per (outer_i, inner_j) pair and emits matching
    // index pairs into `pairs_out`. Callers MUST strip NULL rows from the
    // key arrays before invocation (PG INNER-join semantics exclude NULLs).
    // Overflow is signalled when `*pair_count_out > max_pairs` — the caller
    // must reject the result and let PG plan natively.

    /// Single-predicate i64 inequality NLJ. `op` selects `<`, `<=`, `>=`, `>`.
    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_nlj_ineq_i64(
        outer_keys: *const i64,
        n_outer: usize,
        inner_keys: *const i64,
        n_inner: usize,
        op: i32,
        pairs_out: *mut u32,
        max_pairs: usize,
        pair_count_out: *mut usize,
    ) -> PgaccelStatus;

    /// Single-predicate f64 inequality NLJ.
    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_nlj_ineq_f64(
        outer_keys: *const f64,
        n_outer: usize,
        inner_keys: *const f64,
        n_inner: usize,
        op: i32,
        pairs_out: *mut u32,
        max_pairs: usize,
        pair_count_out: *mut usize,
    ) -> PgaccelStatus;

    /// BETWEEN-shape i64 NLJ. Predicate: `inner_lo[j] <= outer[i] <= inner_hi[j]`.
    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_nlj_between_i64(
        outer_keys: *const i64,
        n_outer: usize,
        inner_lo: *const i64,
        inner_hi: *const i64,
        n_inner: usize,
        pairs_out: *mut u32,
        max_pairs: usize,
        pair_count_out: *mut usize,
    ) -> PgaccelStatus;

    /// BETWEEN-shape f64 NLJ.
    #[allow(clippy::too_many_arguments)]
    pub fn pgaccel_nlj_between_f64(
        outer_keys: *const f64,
        n_outer: usize,
        inner_lo: *const f64,
        inner_hi: *const f64,
        n_inner: usize,
        pairs_out: *mut u32,
        max_pairs: usize,
        pair_count_out: *mut usize,
    ) -> PgaccelStatus;
}

/// Raw resident spatial entry points kept outside [`bridge_status_fns!`].
/// Launch callers retain these statuses while a resident-store borrow is
/// active, so conversion/tracing must wait until the borrow is released.
mod resident_spatial_raw {
    use super::{
        PgaccelSpatialRecheckCompactRequest, PgaccelSpatialRecheckPatchRequest,
        PgaccelSpatialResidentRequest, PgaccelSpatialWorkspace,
    };

    unsafe extern "C" {
        #[allow(dead_code)]
        // reason: raw borrow-safe caller lands in the spatial executor checkpoint
        pub fn pgaccel_spatial_eval_resident_launch(
            request: *const PgaccelSpatialResidentRequest,
            workspace: *const PgaccelSpatialWorkspace,
            detail: *mut i32,
        ) -> i32;

        #[allow(dead_code)]
        // reason: raw borrow-safe caller lands in the spatial executor checkpoint
        pub fn pgaccel_spatial_recheck_compact_launch(
            request: *const PgaccelSpatialRecheckCompactRequest,
            workspace: *const PgaccelSpatialWorkspace,
            detail: *mut i32,
        ) -> i32;

        #[allow(dead_code)]
        // reason: raw borrow-safe caller lands in the spatial executor checkpoint
        pub fn pgaccel_spatial_recheck_patch_launch(
            request: *const PgaccelSpatialRecheckPatchRequest,
            workspace: *const PgaccelSpatialWorkspace,
            detail: *mut i32,
        ) -> i32;

        #[allow(dead_code)] // reason: post-borrow caller lands in the spatial executor checkpoint
        pub fn pgaccel_spatial_workspace_finish(
            workspace: *const PgaccelSpatialWorkspace,
            detail: *mut i32,
        ) -> i32;
    }
}

/// Submit resident spatial evaluation without converting its raw status.
///
/// # Safety
///
/// The caller must uphold the native request/workspace span contract and
/// prepare the process queue before entering the resident-store borrow.
#[must_use]
#[inline]
#[allow(dead_code)] // reason: consumed by the spatial executor checkpoint landing separately
pub(super) unsafe fn pgaccel_spatial_eval_resident_launch_raw(
    request: *const PgaccelSpatialResidentRequest,
    workspace: *const PgaccelSpatialWorkspace,
    detail: *mut i32,
) -> i32 {
    // SAFETY: forwards verbatim under the caller's native pointer contract.
    unsafe {
        resident_spatial_raw::pgaccel_spatial_eval_resident_launch(request, workspace, detail)
    }
}

/// Submit ordered uncertainty compaction without converting its raw status.
///
/// # Safety
///
/// The caller must uphold the native request/workspace span and launch-chain
/// contract.
#[must_use]
#[inline]
#[allow(dead_code)] // reason: consumed by the spatial executor checkpoint landing separately
pub(super) unsafe fn pgaccel_spatial_recheck_compact_launch_raw(
    request: *const PgaccelSpatialRecheckCompactRequest,
    workspace: *const PgaccelSpatialWorkspace,
    detail: *mut i32,
) -> i32 {
    // SAFETY: forwards verbatim under the caller's native pointer contract.
    unsafe {
        resident_spatial_raw::pgaccel_spatial_recheck_compact_launch(request, workspace, detail)
    }
}

/// Submit ordered exact-result patching without converting its raw status.
///
/// # Safety
///
/// The caller must uphold the native request/workspace span contract.
#[must_use]
#[inline]
#[allow(dead_code)] // reason: consumed by the spatial executor checkpoint landing separately
pub(super) unsafe fn pgaccel_spatial_recheck_patch_launch_raw(
    request: *const PgaccelSpatialRecheckPatchRequest,
    workspace: *const PgaccelSpatialWorkspace,
    detail: *mut i32,
) -> i32 {
    // SAFETY: forwards verbatim under the caller's native pointer contract.
    unsafe {
        resident_spatial_raw::pgaccel_spatial_recheck_patch_launch(request, workspace, detail)
    }
}

/// Copy the sticky resident spatial status to host without converting it.
///
/// # Safety
///
/// The workspace must remain alive and satisfy the native exact-span
/// contract. Call this only after releasing resident input borrows.
#[must_use]
#[inline]
#[allow(dead_code)] // reason: consumed by the spatial executor checkpoint landing separately
pub(super) unsafe fn pgaccel_spatial_workspace_finish_raw(
    workspace: *const PgaccelSpatialWorkspace,
    detail: *mut i32,
) -> i32 {
    // SAFETY: forwards verbatim under the caller's native pointer contract.
    unsafe { resident_spatial_raw::pgaccel_spatial_workspace_finish(workspace, detail) }
}

// Non-status externs: entry points returning pointers, scalars, structs, or
// nothing. These carry no `pgaccel_status` and need no conversion layer.
//
// SAFETY: These are the C FFI bindings to libpgaccel_kernels. The functions
// are implemented in C++ and linked at load time. Caller must ensure
// pgaccel_init() is called before other functions.
#[allow(dead_code)] // reason: ABI mirror of pgaccel headers; symbol set is load-bearing
unsafe extern "C" {

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

    // NOTE: the USM arena externs (pgaccel_alloc/free/pool_reset/
    // pool_bytes_used/prefetch) were removed in Phase 3 — the arena had zero
    // callers on either side of the FFI. The C symbol pgaccel_pool_reset
    // still exists (device_manager.cpp calls it at shutdown) but no Rust
    // code invokes it.

    /// Free a pointer returned by `pgaccel_expr_device_alloc_copy`.
    pub fn pgaccel_expr_device_free(ptr: *mut std::ffi::c_void);

    /// Free a pointer returned by `pgaccel_grouped_agg_workspace_alloc`.
    pub fn pgaccel_grouped_agg_workspace_free(ptr: *mut std::ffi::c_void);

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

    // -- Hash aggregation kernels --

    /// Deprecated host-key grouped COUNT(*) ABI. Always returns null before
    /// GPU execution; resident callers use the device-buffer entry points.
    #[allow(dead_code)] // reason: compatibility ABI hard-declines host-key grouping
    pub fn pgaccel_hash_count_i64_execute(
        group_keys: *const i64,
        group_null_mask: *const u8,
        row_count: usize,
    ) -> *mut PgaccelAggState;

    /// Perform grouped COUNT(*) over a resident int64 key buffer using the
    /// bounded device hash-count path.
    pub fn pgaccel_hash_count_i64_device_hash_execute_bounded(
        group_keys: *mut i64,
        row_count: usize,
        max_distinct_hint: usize,
    ) -> *mut PgaccelAggState;

    /// Get the number of groups in the aggregation result.
    pub fn pgaccel_agg_group_count(state: *const PgaccelAggState) -> usize;

    /// Get the group keys as a contiguous buffer.
    pub fn pgaccel_agg_get_group_keys(state: *const PgaccelAggState) -> *const std::ffi::c_void;

    /// Get per-group row counts.
    pub fn pgaccel_agg_get_counts(state: *const PgaccelAggState) -> *const i64;

    /// Free aggregation state.
    pub fn pgaccel_agg_free(state: *mut PgaccelAggState);
}

// ---------------------------------------------------------------------------
// Typed spatial wrappers over the `void* + use_fp64` C ABI
// ---------------------------------------------------------------------------
//
// The three spatial predicate kernels take untyped `void*` buffers whose
// element type is selected by a `use_fp64` flag (`pgaccel_ffi.h:338-357`).
// The raw wrappers above mirror that ABI exactly; the typed pairs below fix
// the buffer type and the flag together so the type system prevents the
// f64-through-f32-buffer mismatch. New call sites should use these.

/// `pgaccel_point_in_ring_bulk` with fp32 buffers (`use_fp64 = false`).
///
/// # Safety
/// `points_xy` must hold `point_count * 2` f32 values, `ring_xy` must hold
/// `vertex_count * 2` f32 values, and `results` must have room for
/// `point_count` bytes. `pgaccel_init()` must have been called.
#[must_use]
pub unsafe fn pgaccel_point_in_ring_bulk_f32(
    points_xy: *const f32,
    point_count: usize,
    ring_xy: *const f32,
    vertex_count: usize,
    results: *mut i8,
) -> PgaccelStatus {
    // SAFETY: forwards to the void* ABI with the flag matching the buffer
    // element type (f32 / false); caller upholds the pointer contract.
    unsafe {
        pgaccel_point_in_ring_bulk(
            points_xy.cast(),
            point_count,
            ring_xy.cast(),
            vertex_count,
            false,
            results,
        )
    }
}

/// `pgaccel_point_in_ring_bulk` with fp64 buffers (`use_fp64 = true`).
///
/// # Safety
/// Same contract as [`pgaccel_point_in_ring_bulk_f32`] with f64 elements.
#[allow(dead_code)]
// reason: typed pair of the f32 wrapper; the f64 three-layer contract lands with the shared typed-geometry work
#[must_use]
pub unsafe fn pgaccel_point_in_ring_bulk_f64(
    points_xy: *const f64,
    point_count: usize,
    ring_xy: *const f64,
    vertex_count: usize,
    results: *mut i8,
) -> PgaccelStatus {
    // SAFETY: forwards to the void* ABI with the flag matching the buffer
    // element type (f64 / true); caller upholds the pointer contract.
    unsafe {
        pgaccel_point_in_ring_bulk(
            points_xy.cast(),
            point_count,
            ring_xy.cast(),
            vertex_count,
            true,
            results,
        )
    }
}

/// `pgaccel_sphere_distance_bulk` with fp32 buffers (`use_fp64 = false`).
///
/// # Safety
/// `points_a` / `points_b` must each hold `count * 2` f32 lon/lat values;
/// `distances` must have room for `count` f32 values; `uncertain` must have
/// room for `count` bytes. `pgaccel_init()` must have been called.
#[must_use]
pub unsafe fn pgaccel_sphere_distance_bulk_f32(
    points_a: *const f32,
    points_b: *const f32,
    count: usize,
    distances: *mut f32,
    uncertain: *mut u8,
) -> PgaccelStatus {
    // SAFETY: forwards to the void* ABI with the flag matching the buffer
    // element type (f32 / false); caller upholds the pointer contract.
    unsafe {
        pgaccel_sphere_distance_bulk(
            points_a.cast(),
            points_b.cast(),
            count,
            false,
            distances.cast(),
            uncertain,
        )
    }
}

/// `pgaccel_sphere_distance_bulk` with fp64 buffers (`use_fp64 = true`).
///
/// # Safety
/// Same contract as [`pgaccel_sphere_distance_bulk_f32`] with f64 elements.
#[allow(dead_code)]
// reason: typed pair of the f32 wrapper; engine/dispatch/spatial.rs (agent 2B's file) migrates to it next phase
#[must_use]
pub unsafe fn pgaccel_sphere_distance_bulk_f64(
    points_a: *const f64,
    points_b: *const f64,
    count: usize,
    distances: *mut f64,
    uncertain: *mut u8,
) -> PgaccelStatus {
    // SAFETY: forwards to the void* ABI with the flag matching the buffer
    // element type (f64 / true); caller upholds the pointer contract.
    unsafe {
        pgaccel_sphere_distance_bulk(
            points_a.cast(),
            points_b.cast(),
            count,
            true,
            distances.cast(),
            uncertain,
        )
    }
}

/// `pgaccel_segment_intersects_bulk` with fp32 buffers (`use_fp64 = false`).
///
/// # Safety
/// `segs_a` / `segs_b` must each hold `count * 4` f32 values; `results`
/// must have room for `count` bytes. `pgaccel_init()` must have been called.
#[allow(dead_code)] // reason: typed pair mirror; segment predicate dispatch is staged, no Rust caller yet
#[must_use]
pub unsafe fn pgaccel_segment_intersects_bulk_f32(
    segs_a: *const f32,
    segs_b: *const f32,
    count: usize,
    results: *mut i8,
) -> PgaccelStatus {
    // SAFETY: forwards to the void* ABI with the flag matching the buffer
    // element type (f32 / false); caller upholds the pointer contract.
    unsafe { pgaccel_segment_intersects_bulk(segs_a.cast(), segs_b.cast(), count, false, results) }
}

/// `pgaccel_segment_intersects_bulk` with fp64 buffers (`use_fp64 = true`).
///
/// # Safety
/// Same contract as [`pgaccel_segment_intersects_bulk_f32`] with f64 elements.
#[allow(dead_code)] // reason: typed pair mirror; segment predicate dispatch is staged, no Rust caller yet
#[must_use]
pub unsafe fn pgaccel_segment_intersects_bulk_f64(
    segs_a: *const f64,
    segs_b: *const f64,
    count: usize,
    results: *mut i8,
) -> PgaccelStatus {
    // SAFETY: forwards to the void* ABI with the flag matching the buffer
    // element type (f64 / true); caller upholds the pointer contract.
    unsafe { pgaccel_segment_intersects_bulk(segs_a.cast(), segs_b.cast(), count, true, results) }
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

// Pure layout / discriminant / conversion tests: no PostgreSQL dependency,
// so they run under plain `cargo test -p pg_accel --lib` (previously gated
// behind `pg_test`, which silently skipped them in the default test run).
// The load-bearing size pins are additionally enforced at compile time by
// the const assertions in `types.rs`.
#[cfg(test)]
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
        assert_eq!(PgaccelStatus::Error as i32, -1);
        assert_eq!(PgaccelStatus::ErrorNoDevice as i32, -5);
        assert_eq!(PgaccelStatus::ErrorOom as i32, -3);
        assert_eq!(PgaccelStatus::ErrorTimeout as i32, -4);
        assert_eq!(PgaccelStatus::ErrorUnsupported as i32, -2);
        assert_eq!(PgaccelStatus::InvalidArgument as i32, -6);
    }

    #[test]
    fn status_is_ok_returns_true_only_for_ok() {
        assert!(PgaccelStatus::Ok.is_ok());
        assert!(!PgaccelStatus::Error.is_ok());
        assert!(!PgaccelStatus::ErrorNoDevice.is_ok());
        assert!(!PgaccelStatus::ErrorOom.is_ok());
        assert!(!PgaccelStatus::ErrorTimeout.is_ok());
        assert!(!PgaccelStatus::ErrorUnsupported.is_ok());
        assert!(!PgaccelStatus::InvalidArgument.is_ok());
    }

    #[test]
    fn raw_invalid_argument_status_is_typed() {
        assert_eq!(
            PgaccelStatus::from_raw(-6),
            Ok(PgaccelStatus::InvalidArgument)
        );
        assert_eq!(PgaccelStatus::from_raw(-7), Err(-7));
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
    fn expr_usm_col_struct_matches_c_shape() {
        let size = mem::size_of::<PgaccelExprUsmCol>();
        let align = mem::align_of::<PgaccelExprUsmCol>();
        // values pointer + nulls pointer + repr(i32) tag plus padding.
        assert_eq!(size, 3 * mem::size_of::<usize>());
        assert!(
            align >= mem::align_of::<*const std::ffi::c_void>(),
            "alignment too small: {align}"
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
    fn key_type_discriminant_values_match_c() {
        assert_eq!(PgaccelKeyType::Int32 as i32, 0);
        assert_eq!(PgaccelKeyType::Int64 as i32, 1);
        assert_eq!(PgaccelKeyType::Float64 as i32, 2);
        // Slot 3 reserved for CompositeInt4x2 (planner-only, never
        // sent to kernel — see PgaccelKeyType doc comment).
        assert_eq!(PgaccelKeyType::Uuid as i32, 4);
        assert_eq!(PgaccelKeyType::Inet as i32, 5);
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
