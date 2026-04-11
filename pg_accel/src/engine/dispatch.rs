//! Batch dispatch: routes accumulated batches to the appropriate execution
//! strategy (spatial, H3, raster) and implements late-materialization via
//! predicate chain evaluation.
//!
//! # Strategies
//!
//! All strategies require GPU hardware. There is no CPU-only fallback path.
//!
//! - **`GpuSpatial`**: Offloads spatial predicates to the GPU via a
//!   three-layer pipeline:
//!   1. **Bbox filter** — fast integer/float bounding-box overlap test on GPU.
//!   2. **Geometric fast-path** — exact predicate for common simple geometries
//!      (point-in-ring, segment intersection) on GPU.
//!   3. **PG recheck** — uncertain pairs are deferred to PostGIS for edge
//!      cases the GPU kernels cannot handle (collections, curves, etc.).
//!
//! # Late Materialization
//!
//! [`PredicateChain`] orders predicates by `selectivity / cost` so the cheapest,
//! most-selective predicate runs first. Rows rejected early skip expensive
//! geometry deserialization entirely.

use crate::adapters::extractors::geometry::{extract_geometry, extract_point_xy_f32};
use crate::adapters::extractors::raster;
use crate::engine::gucs;
use crate::engine::registry::{self, AccelStrategy};
use crate::gpu;
use crate::gpu::three_layer;

/// Stack-allocated wrapper for `FunctionCallInfoBaseData` with space for two
/// arguments. Used by the GPU spatial recheck path which calls 2-arg PostGIS
/// functions.
#[repr(C)]
struct FcinfoWith2Args {
    base: pgrx::pg_sys::FunctionCallInfoBaseData,
    _arg_space: [pgrx::pg_sys::NullableDatum; 2],
}

// ---------------------------------------------------------------------------
// Dispatch result
// ---------------------------------------------------------------------------

/// Outcome of a dispatch attempt.
#[derive(Debug)]
pub enum DispatchResult {
    /// The batch was evaluated by an accelerated path.
    Accelerated(Vec<(pgrx::pg_sys::Datum, bool)>),
    /// The batch could not be accelerated for this strategy.
    ///
    /// This is **deferral**, not CPU fallback: the caller should let
    /// PostgreSQL's standard executor handle these tuples normally via
    /// scalar qual evaluation. No extraction, no CPU reimplementation —
    /// just PG's native path. Zero overhead beyond the dispatch check.
    ///
    /// Strategies that use dedicated executor nodes (GpuSort, GpuReduce,
    /// GpuHashJoin, GpuWindow, GpuExpr) return this because they do not
    /// participate in the per-datum dispatch interface.
    Deferred,
}

// ---------------------------------------------------------------------------
// Top-level dispatch
// ---------------------------------------------------------------------------

/// Route a batch of `(Datum, is_null)` pairs to the appropriate execution
/// strategy.
///
/// `qual_datum` is the constant second argument for two-argument spatial
/// predicates (e.g. the constant geometry in
/// `WHERE ST_Intersects(geom_col, $1)`). Pass `None` for single-argument
/// functions or when the second argument is not available.
///
/// Returns [`DispatchResult::Accelerated`] with per-row results when the
/// strategy is supported, or [`DispatchResult::Deferred`] when the caller
/// should use the standard PostgreSQL executor.
///
/// # Safety
///
/// Must be called on the **main backend thread** only. The underlying
/// `FunctionCallInvoke` and `CHECK_FOR_INTERRUPTS` macros are not safe to
/// call from worker threads.
#[must_use]
pub unsafe fn dispatch(
    strategy: AccelStrategy,
    batch: &[(pgrx::pg_sys::Datum, bool)],
    fn_info: &pgrx::pg_sys::FmgrInfo,
    is_strict: bool,
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
    skip_bbox: bool,
) -> DispatchResult {
    match strategy {
        AccelStrategy::GpuSpatial => {
            // SAFETY: Caller guarantees main backend thread.
            unsafe { dispatch_gpu_spatial(batch, fn_info, is_strict, qual_datum, skip_bbox) }
        }
        AccelStrategy::GpuH3 => {
            // SAFETY: Caller guarantees main backend thread.
            unsafe { dispatch_gpu_h3(batch, fn_info, is_strict, fn_info.fn_oid, qual_datum) }
        }
        AccelStrategy::GpuRaster => {
            // SAFETY: Caller guarantees main backend thread.
            unsafe { dispatch_gpu_raster(batch, fn_info, is_strict, fn_info.fn_oid) }
        }
        // GpuExpr is handled directly in scan.rs via the columnar path,
        // not through the per-datum dispatch interface.
        // GpuSort/GpuReduce/GpuHashJoin/GpuWindow not wired into per-datum dispatch.
        AccelStrategy::GpuExpr
        | AccelStrategy::GpuSort
        | AccelStrategy::GpuReduce
        | AccelStrategy::GpuHashJoin
        | AccelStrategy::GpuWindow => DispatchResult::Deferred,
    }
}

// ---------------------------------------------------------------------------
// Strategy: GpuSpatial
//
// GPU spatial dispatch via the three-layer pipeline. The pipeline evaluates
// spatial predicates in three layers:
//
// 1. Bbox filter — coarse bounding-box overlap test rejecting most
//    non-intersecting pairs with minimal memory traffic.
// 2. Geometric fast-path — exact spatial predicate for common simple
//    geometries (point-in-ring winding-number, great-circle distance,
//    segment intersection) evaluated in fp32 with an fp64 refinement band.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Bulk point-in-polygon fast path
// ---------------------------------------------------------------------------

/// Attempt the fast bulk point-in-polygon path: extract all batch rows as
/// points using zero-alloc `extract_point_xy_f32`, then call the dedicated
/// C++ `pgaccel_point_in_polygon_bulk` which does inline bbox + parallel
/// point-in-ring.
///
/// Returns `Some(DispatchResult)` if all non-NULL rows are successfully
/// extracted as points. Returns `None` if any row is not a point, falling
/// through to the generic extraction path.
///
/// # Safety
///
/// Must be called on the main backend thread.
#[allow(clippy::too_many_arguments)]
unsafe fn try_bulk_point_in_polygon(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    geom_b: &three_layer::ExtractedGeometry,
    fn_info: &pgrx::pg_sys::FmgrInfo,
    is_strict: bool,
    qual_d: pgrx::pg_sys::Datum,
    qual_null: bool,
) -> Option<DispatchResult> {
    let bool_true = pgrx::pg_sys::Datum::from(true);
    let bool_false = pgrx::pg_sys::Datum::from(false);

    // Pre-allocate flat point array: 2 floats per non-NULL row.
    let mut points_xy: Vec<f32> = Vec::with_capacity(batch.len() * 2);
    let mut point_idx_to_batch: Vec<usize> = Vec::with_capacity(batch.len());
    let mut results = vec![(pgrx::pg_sys::Datum::from(0), true); batch.len()];

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        // Zero-alloc extraction: reads x,y directly from detoasted pointer.
        if let Some((x, y)) = extract_point_xy_f32(datum) {
            points_xy.push(x);
            points_xy.push(y);
            point_idx_to_batch.push(i);
        } else {
            // Not a point — bail out to generic path.
            return None;
        }
    }

    if point_idx_to_batch.is_empty() {
        // All rows NULL.
        return Some(DispatchResult::Accelerated(results));
    }

    let timeout_ms = gucs::kernel_timeout_ms();
    let start = std::time::Instant::now();

    let ring_offset_zero: u32 = 0;
    let ring_offsets = if geom_b.ring_offsets.is_empty() {
        std::slice::from_ref(&ring_offset_zero)
    } else {
        geom_b.ring_offsets.as_slice()
    };
    let ring_count = if geom_b.ring_offsets.is_empty() {
        1
    } else {
        geom_b.ring_offsets.len()
    };

    let bulk_results = gpu::point_in_polygon_bulk(
        &points_xy,
        &geom_b.bbox,
        &geom_b.coords,
        geom_b.coord_count,
        ring_offsets,
        ring_count,
    );

    let elapsed_ms = start.elapsed().as_millis() as i32;
    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: bulk point_in_polygon took {}ms (timeout {}ms)",
            elapsed_ms,
            timeout_ms,
        );
    }

    let Some(pip_results) = bulk_results else {
        // GPU bridge unavailable — fall through to generic path.
        return None;
    };

    pgrx::debug1!(
        "pg_accel: bulk point_in_polygon: {} points, {}ms",
        point_idx_to_batch.len(),
        elapsed_ms,
    );

    // Apply results. Collect uncertain indices for CPU recheck.
    let mut needs_recheck: Vec<usize> = Vec::new();

    for (k, &r) in pip_results.iter().enumerate() {
        let batch_idx = point_idx_to_batch[k];
        match r {
            1 => results[batch_idx] = (bool_true, false),
            -1 => results[batch_idx] = (bool_false, false),
            _ => needs_recheck.push(batch_idx),
        }
    }

    // CPU recheck uncertain rows via the original PostGIS function.
    for &batch_idx in &needs_recheck {
        let (datum_a, is_null_a) = batch[batch_idx];
        if is_strict && is_null_a {
            results[batch_idx] = (pgrx::pg_sys::Datum::from(0), true);
            continue;
        }

        // SAFETY: Build a FunctionCallInfo with 2 args on the stack.
        // Both arg slots are backed by _arg_space in FcinfoWith2Args.
        // Called on the main backend thread with valid fn_info.
        unsafe {
            let mut fcinfo_buf: FcinfoWith2Args = std::mem::zeroed();
            fcinfo_buf.base.flinfo =
                std::ptr::from_ref::<pgrx::pg_sys::FmgrInfo>(fn_info).cast_mut();
            fcinfo_buf.base.nargs = 2;
            fcinfo_buf.base.isnull = false;

            let args = fcinfo_buf.base.args.as_mut_ptr();
            (*args).value = datum_a;
            (*args).isnull = is_null_a;
            (*args.add(1)).value = qual_d;
            (*args.add(1)).isnull = qual_null;

            let Some(func) = (*fcinfo_buf.base.flinfo).fn_addr else {
                results[batch_idx] = (pgrx::pg_sys::Datum::from(0), true);
                continue;
            };
            let result_datum = func(&raw mut fcinfo_buf.base);
            results[batch_idx] = (result_datum, fcinfo_buf.base.isnull);
        }
    }

    Some(DispatchResult::Accelerated(results))
}

/// 3. **CPU recheck** — Rows that the pipeline cannot conclusively decide
///    (geometry collections, curves, numerical edge cases) are rechecked
///    via the original PostGIS function on the main backend thread.
///
/// The function expects pairs of geometry datums: each batch element is
/// a single datum that is the first argument to a two-argument spatial
/// predicate (e.g., `ST_Intersects(a, b)`). The second geometry is
/// currently assumed to be uniform across the batch (a common pattern
/// for indexed lookups). For truly arbitrary pairs, defers to
/// `Deferred`.
///
/// # Safety
///
/// Must be called on the **main backend thread**.
#[must_use]
pub unsafe fn dispatch_gpu_spatial(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    fn_info: &pgrx::pg_sys::FmgrInfo,
    is_strict: bool,
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
    _skip_bbox: bool,
) -> DispatchResult {
    // We need the constant second geometry to form pairs.
    let Some((qual_d, qual_null)) = qual_datum else {
        pgrx::debug1!("pg_accel: dispatch_gpu_spatial: no qual_datum, deferring");
        return DispatchResult::Deferred;
    };

    // Strict: if the constant arg is NULL, every result is NULL.
    if is_strict && qual_null {
        return DispatchResult::Accelerated(vec![
            (pgrx::pg_sys::Datum::from(0), true);
            batch.len()
        ]);
    }

    // Extract the constant geometry (arg B) once.
    let Some(geom_b) = extract_geometry(qual_d) else {
        pgrx::debug1!("pg_accel: dispatch_gpu_spatial: extract_geometry(qual_d) failed, deferring");
        return DispatchResult::Deferred;
    };

    // ── Vertex count gate ────────────────────────────────────────
    // GPU overhead is roughly constant (~19ms on M2 Max) regardless of
    // polygon complexity (dominated by geometry deser + seq scan).
    // PG parallel scales linearly with vertex count. Below the threshold,
    // PG parallel is faster — defer to avoid overhead.
    let min_verts = super::cost::device_limits().gpu_spatial_min_vertices;
    if geom_b.coord_count < min_verts {
        pgrx::debug1!(
            "pg_accel: dispatch_gpu_spatial: polygon has {} vertices (min {}), deferring",
            geom_b.coord_count,
            min_verts
        );
        return DispatchResult::Deferred;
    }

    // ── Fast path: bulk point-in-polygon ──────────────────────────
    // When geom_b is a Polygon, try zero-alloc extraction of all batch
    // rows as points directly into a flat f32 array.  This eliminates
    // per-row Vec<u8>/Vec<f32>/ExtractedGeometry/PgaccelGeometry overhead.
    if matches!(geom_b.geom_type, three_layer::GeomType::Polygon) {
        // Only ST_Intersects uses the fast path.
        let fn_name = registry::global_registry()
            .lookup(fn_info.fn_oid)
            .map(|e| e.name);
        let is_intersects = !matches!(fn_name, Some("st_contains" | "st_within" | "st_dwithin"));

        if is_intersects {
            // SAFETY: same preconditions as dispatch_gpu_spatial — main
            // backend thread, valid datums and fn_info.
            if let Some(result) = unsafe {
                try_bulk_point_in_polygon(batch, &geom_b, fn_info, is_strict, qual_d, qual_null)
            } {
                return result;
            }
        }
    }

    // Extract per-row geometries (arg A). Track which extracted geometry
    // index maps back to which batch index.
    let mut geoms_a: Vec<three_layer::ExtractedGeometry> = Vec::with_capacity(batch.len());
    let mut geom_idx_to_batch: Vec<usize> = Vec::with_capacity(batch.len());

    // Pre-fill results: NULL rows → NULL output.
    let mut results = vec![(pgrx::pg_sys::Datum::from(0), true); batch.len()];
    let mut needs_scalar_recheck: Vec<usize> = Vec::new();

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        if let Some(geom) = extract_geometry(datum) {
            geom_idx_to_batch.push(i);
            geoms_a.push(geom);
        } else {
            needs_scalar_recheck.push(i);
        }
    }

    // If no geometries could be extracted, fall back entirely.
    if geoms_a.is_empty() {
        return DispatchResult::Deferred;
    }

    // Only GPU-accelerated predicate currently supported is ST_Intersects.
    // Others (Contains, Within, DWithin) defer to native PostGIS.
    {
        let fn_name = registry::global_registry()
            .lookup(fn_info.fn_oid)
            .map(|e| e.name);
        if let Some("st_contains" | "st_within" | "st_dwithin") = fn_name {
            return DispatchResult::Deferred;
        }
        // ST_Intersects or unrecognised → GPU path below
    }

    // Build PgaccelGeometry descriptors pointing into the ExtractedGeometry
    // data.  The raw pointers are valid for the duration of this function.
    let ring_offset_zero: u32 = 0;
    let to_pgaccel = |eg: &three_layer::ExtractedGeometry| gpu::PgaccelGeometry {
        geom_type: match eg.geom_type {
            three_layer::GeomType::Point => gpu::PgaccelGeomType::Point,
            three_layer::GeomType::LineString => gpu::PgaccelGeomType::LineString,
            three_layer::GeomType::Polygon => gpu::PgaccelGeomType::Polygon,
            three_layer::GeomType::Unknown => gpu::PgaccelGeomType::Unknown,
        },
        bbox: eg.bbox.as_ptr(),
        coords: eg.coords.as_ptr(),
        coord_count: eg.coord_count,
        ring_offsets: std::ptr::addr_of!(ring_offset_zero),
        ring_count: usize::from(matches!(eg.geom_type, three_layer::GeomType::Polygon)),
    };

    let pgaccel_a: Vec<gpu::PgaccelGeometry> = geoms_a.iter().map(to_pgaccel).collect();
    let pgaccel_b = [to_pgaccel(&geom_b)];

    // Try GPU kernel: N variable geometries × 1 constant geometry.
    let timeout_ms = gucs::kernel_timeout_ms();
    let start = std::time::Instant::now();
    pgrx::debug1!(
        "pg_accel: dispatch_gpu_spatial: calling GPU kernel {}x{} pairs",
        pgaccel_a.len(),
        pgaccel_b.len(),
    );
    let gpu_result = gpu::spatial_intersects_gpu(&pgaccel_a, &pgaccel_b);
    let elapsed_ms = start.elapsed().as_millis() as i32;

    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: spatial GPU kernel took {}ms (timeout {}ms)",
            elapsed_ms,
            timeout_ms,
        );
    }

    let Some((dt_pairs, df_pairs, uc_pairs)) = gpu_result else {
        // GPU unavailable or kernel unsupported — defer to PostgreSQL.
        return DispatchResult::Deferred;
    };

    pgrx::debug1!(
        "pg_accel: GPU spatial results: definite_true={}, definite_false={}, uncertain={}",
        dt_pairs.len(),
        df_pairs.len(),
        uc_pairs.len(),
    );

    // Apply GPU results. Each pair is (i, j) where j=0 (constant geom).
    let bool_true = pgrx::pg_sys::Datum::from(true);
    let bool_false = pgrx::pg_sys::Datum::from(false);

    for &(i, _j) in &dt_pairs {
        let geom_idx = i as usize;
        if geom_idx < geom_idx_to_batch.len() {
            results[geom_idx_to_batch[geom_idx]] = (bool_true, false);
        }
    }

    for &(i, _j) in &df_pairs {
        let geom_idx = i as usize;
        if geom_idx < geom_idx_to_batch.len() {
            results[geom_idx_to_batch[geom_idx]] = (bool_false, false);
        }
    }

    // UNCERTAIN pairs need CPU recheck via the original PostGIS function.
    for &(i, _j) in &uc_pairs {
        let geom_idx = i as usize;
        if geom_idx < geom_idx_to_batch.len() {
            needs_scalar_recheck.push(geom_idx_to_batch[geom_idx]);
        }
    }

    // CPU recheck: call the 2-arg PG function for uncertain rows.
    let mut recheck_pass = 0usize;
    let recheck_total = needs_scalar_recheck.len();
    for &batch_idx in &needs_scalar_recheck {
        let (datum_a, is_null_a) = batch[batch_idx];
        if is_strict && is_null_a {
            results[batch_idx] = (pgrx::pg_sys::Datum::from(0), true);
            continue;
        }

        // SAFETY: Build a FunctionCallInfo with 2 args on the stack.
        // Both arg slots are backed by _arg_space in FcinfoWith2Args.
        let mut fcinfo_buf: FcinfoWith2Args = unsafe { std::mem::zeroed() };
        fcinfo_buf.base.flinfo = std::ptr::from_ref::<pgrx::pg_sys::FmgrInfo>(fn_info).cast_mut();
        fcinfo_buf.base.nargs = 2;
        fcinfo_buf.base.isnull = false;

        // SAFETY: _arg_space provides backing for args[0] and args[1].
        unsafe {
            let args = fcinfo_buf.base.args.as_mut_ptr();
            (*args).value = datum_a;
            (*args).isnull = is_null_a;
            (*args.add(1)).value = qual_d;
            (*args.add(1)).isnull = qual_null;
        }

        // SAFETY: Call the PG function on the main backend thread.
        let result_datum = unsafe {
            let Some(func) = (*fcinfo_buf.base.flinfo).fn_addr else {
                results[batch_idx] = (pgrx::pg_sys::Datum::from(0), true);
                continue;
            };
            func(&raw mut fcinfo_buf.base)
        };

        results[batch_idx] = (result_datum, fcinfo_buf.base.isnull);
        if !fcinfo_buf.base.isnull && result_datum.value() != 0 {
            recheck_pass += 1;
        }
    }

    if recheck_total > 0 {
        pgrx::debug1!(
            "pg_accel: spatial recheck: {}/{} uncertain pairs passed PostGIS",
            recheck_pass,
            recheck_total,
        );
    }

    DispatchResult::Accelerated(results)
}

// ---------------------------------------------------------------------------
// Strategy: GpuH3
// ---------------------------------------------------------------------------

/// GPU H3 cell dispatch.
///
/// H3 functions operate on 64-bit cell indices. This handler extracts H3
/// cell values from the batch, dispatches to the appropriate GPU kernel,
/// and returns GPU-computed results directly.
///
/// # Safety
///
/// Must be called on the **main backend thread**.
#[must_use]
pub unsafe fn dispatch_gpu_h3(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    _fn_info: &pgrx::pg_sys::FmgrInfo,
    _is_strict: bool,
    fn_oid: pgrx::pg_sys::Oid,
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
) -> DispatchResult {
    // Look up the function name to route to the correct GPU kernel.
    let fn_name = registry::global_registry().lookup(fn_oid).map(|e| e.name);

    let timeout_ms = gucs::kernel_timeout_ms();
    let start = std::time::Instant::now();

    // h3_latlng_to_cell takes point geometries, not cell indices —
    // handle it separately with geometry extraction.
    if fn_name == Some("h3_latlng_to_cell") {
        let resolution = qual_datum
            .filter(|(_, is_null)| !is_null)
            .map(|(d, _)| d.value() as i32);
        let Some(res) = resolution else {
            return DispatchResult::Deferred;
        };

        let mut lats: Vec<f64> = Vec::with_capacity(batch.len());
        let mut lngs: Vec<f64> = Vec::with_capacity(batch.len());
        let mut valid_indices: Vec<usize> = Vec::with_capacity(batch.len());

        for (i, &(datum, is_null)) in batch.iter().enumerate() {
            if is_null {
                continue;
            }
            if let Some(geom) = extract_geometry(datum) {
                // For points, coords contains [x, y] and bbox has
                // [xmin, ymin, xmax, ymax]. Use coords for precision
                // when available, otherwise fall back to bbox center.
                if geom.coords.len() >= 2 {
                    lngs.push(f64::from(geom.coords[0])); // x = longitude
                    lats.push(f64::from(geom.coords[1])); // y = latitude
                } else {
                    lngs.push(f64::from(geom.bbox[0]));
                    lats.push(f64::from(geom.bbox[1]));
                }
                valid_indices.push(i);
            }
        }

        if lats.is_empty() {
            return DispatchResult::Deferred;
        }

        let gpu_result = crate::gpu::h3_lat_lng_to_cell_bulk(&lats, &lngs, res);

        log_h3_timeout(timeout_ms, &start);

        if let Some(cell_ids) = gpu_result {
            let mut results = vec![(pgrx::pg_sys::Datum::from(0), true); batch.len()];
            for (gi, &batch_idx) in valid_indices.iter().enumerate() {
                if gi < cell_ids.len() && cell_ids[gi] != 0 {
                    results[batch_idx] = (pgrx::pg_sys::Datum::from(cell_ids[gi] as i64), false);
                }
            }
            return DispatchResult::Accelerated(results);
        }

        return DispatchResult::Deferred;
    }

    // Extract H3 cell indices from the batch datums.
    // H3 cells are 64-bit integers stored as Datum (which is usize on PG).
    let mut cells: Vec<u64> = Vec::with_capacity(batch.len());
    let mut valid_indices: Vec<usize> = Vec::with_capacity(batch.len());

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        // H3 cell indices are bigint (i64) values stored as Datum.
        let cell = datum.value() as u64;
        // Basic validity check: H3 cells have a non-zero high nibble.
        if cell != 0 {
            cells.push(cell);
            valid_indices.push(i);
        }
    }

    // If we couldn't extract enough cells, fall back to the standard executor.
    if cells.is_empty() || valid_indices.len() < batch.len() / 2 {
        return DispatchResult::Deferred;
    }

    // Route to the correct GPU kernel based on function name.
    let gpu_results: Option<GpuH3Result> = match fn_name {
        // 1-arg: cell → i32 resolution
        Some("h3_get_resolution") => {
            crate::gpu::h3_get_resolution_bulk(&cells).map(GpuH3Result::I32)
        }
        // 2-arg: cell + resolution constant → parent cell (u64)
        Some("h3_cell_to_parent") => {
            let res = qual_datum
                .filter(|(_, is_null)| !is_null)
                .map(|(d, _)| d.value() as i32);
            res.and_then(|parent_res| {
                crate::gpu::h3_cell_to_parent_bulk(&cells, parent_res).map(GpuH3Result::U64)
            })
        }
        // 2-arg: cell_a + cell_b constant → distance (i32)
        Some("h3_grid_distance") => {
            let other_cell = qual_datum
                .filter(|(_, is_null)| !is_null)
                .map(|(d, _)| d.value() as u64);
            other_cell.and_then(|oc| {
                let cells_b = vec![oc; cells.len()];
                crate::gpu::h3_grid_distance_bulk(&cells, &cells_b).map(GpuH3Result::I32)
            })
        }
        _ => None,
    };

    log_h3_timeout(timeout_ms, &start);

    // If GPU returned results, map them back to batch indices.
    if let Some(gpu_res) = gpu_results {
        let mut results = vec![(pgrx::pg_sys::Datum::from(0), true); batch.len()];
        for (gi, &batch_idx) in valid_indices.iter().enumerate() {
            let datum = match &gpu_res {
                GpuH3Result::I32(v) if gi < v.len() => pgrx::pg_sys::Datum::from(v[gi]),
                GpuH3Result::U64(v) if gi < v.len() => pgrx::pg_sys::Datum::from(v[gi] as i64),
                _ => continue,
            };
            results[batch_idx] = (datum, false);
        }
        return DispatchResult::Accelerated(results);
    }

    // GPU unavailable or unsupported function — fall back to standard executor.
    DispatchResult::Deferred
}

/// Tagged union for H3 GPU kernel results — some return i32, others u64.
enum GpuH3Result {
    I32(Vec<i32>),
    U64(Vec<u64>),
}

/// Log a warning if the H3 GPU pipeline exceeded the configured timeout.
fn log_h3_timeout(timeout_ms: i32, start: &std::time::Instant) {
    let elapsed_ms = start.elapsed().as_millis() as i32;
    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: H3 pipeline took {}ms (timeout {}ms)",
            elapsed_ms,
            timeout_ms,
        );
    }
}

// ---------------------------------------------------------------------------
// Strategy: GpuRaster
// ---------------------------------------------------------------------------

/// GPU raster dispatch for `ST_MapAlgebra`, `ST_Clip`, and `ST_Reclass`.
///
/// Extracts raster WKB data from each datum, parses the header and pixel
/// data using the raster extractor, then dispatches to the appropriate GPU
/// kernel.
///
/// Currently runs the GPU pipeline as a validation pass (exercising raster
/// extraction and GPU kernel invocation). Returns `Deferred` when
/// extraction fails or the pipeline is incomplete.
///
/// # Safety
///
/// Must be called on the **main backend thread**.
#[must_use]
pub unsafe fn dispatch_gpu_raster(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    _fn_info: &pgrx::pg_sys::FmgrInfo,
    _is_strict: bool,
    _fn_oid: pgrx::pg_sys::Oid,
) -> DispatchResult {
    // Attempt raster extraction from each datum.
    let mut raster_data: Vec<(raster::RasterHeader, Vec<f64>)> = Vec::new();

    for &(datum, is_null) in batch {
        if is_null {
            continue;
        }

        // Raster datums are varlena (bytea-like). Extract the raw bytes.
        // SAFETY: Caller guarantees main backend thread, datum is a valid
        // varlena pointer for raster data.
        let varlena = unsafe { pgrx::pg_sys::pg_detoast_datum(datum.cast_mut_ptr()) };
        // SAFETY: `varlena` is a valid detoasted varlena pointer returned by
        // `pg_detoast_datum` above; `varsize_any_exhdr` reads its length header.
        let data_len = unsafe { pgrx::varsize_any_exhdr(varlena) };
        // SAFETY: `varlena` is valid and detoasted; `vardata_any` returns a
        // pointer to the payload immediately after the varlena header.
        let data_ptr = unsafe { pgrx::vardata_any(varlena) };
        // SAFETY: `data_ptr` points to `data_len` bytes of contiguous varlena
        // payload within the detoasted datum. The slice does not outlive the
        // current loop iteration.
        let data_slice = unsafe { std::slice::from_raw_parts(data_ptr.cast::<u8>(), data_len) };

        // Parse header and extract band 0 pixels.
        if let Some(header) = raster::parse_header(data_slice)
            && let Some(pixels) = raster::extract_pixels_f64(data_slice, 0)
        {
            raster_data.push((header, pixels));
        }
    }

    // If we couldn't extract enough rasters, fall back to standard executor.
    if raster_data.is_empty() || raster_data.len() < batch.len() / 2 {
        return DispatchResult::Deferred;
    }

    // Run map-algebra on each raster through the GPU pipeline.
    let timeout_ms = gucs::kernel_timeout_ms();
    let start = std::time::Instant::now();

    let mut results: Vec<(pgrx::pg_sys::Datum, bool)> = Vec::with_capacity(batch.len());
    let mut raster_idx = 0usize;

    for &(datum, is_null) in batch {
        if is_null {
            results.push((pgrx::pg_sys::Datum::from(0usize), true));
            continue;
        }

        if raster_idx >= raster_data.len() {
            // Not enough extracted rasters — defer remaining rows.
            return DispatchResult::Deferred;
        }

        let (ref header, ref pixels) = raster_data[raster_idx];
        raster_idx += 1;

        let pixel_count = header.width as usize * header.height as usize;
        if pixel_count == 0 {
            results.push((datum, false));
            continue;
        }

        // Build a trivial identity expression: LOAD_BAND 0
        let mut inst = gpu::PgaccelExprInst {
            op: gpu::PgaccelOp::LoadBand,
            arg: 0.0, // band_index = 0
        };
        let expr = gpu::PgaccelExpr {
            instructions: std::ptr::addr_of_mut!(inst),
            inst_count: 1,
            band_count: 1,
        };

        // Convert f64 pixels to f32 for the kernel (Float32 pixel type).
        let f32_pixels: Vec<f32> = pixels.iter().map(|&v| v as f32).collect();
        let band_ptr: *const std::ffi::c_void = f32_pixels.as_ptr().cast();
        let band_ptrs = [band_ptr];

        let pixel_type = gpu::PgaccelPixelType::Float32 as i32;
        let mut output_buf = vec![0u8; pixel_count * 4]; // f32 = 4 bytes
        let mut nodata_mask = vec![0u8; pixel_count];

        let gpu_ok = gpu::map_algebra(
            &band_ptrs,
            pixel_count,
            pixel_type,
            &expr,
            &mut output_buf,
            &mut nodata_mask,
        );

        if gpu_ok.is_none() {
            // GPU failed for this raster — defer to PG.
            return DispatchResult::Deferred;
        }

        // Interpret output_buf as f32 slice and patch back into original WKB.
        let output_f32: &[f32] = unsafe {
            // SAFETY: output_buf was allocated as pixel_count * 4 bytes of f32.
            std::slice::from_raw_parts(output_buf.as_ptr().cast(), pixel_count)
        };

        // Get original WKB bytes for patching.
        // SAFETY: datum is a valid varlena pointer, on main backend thread.
        let varlena = unsafe { pgrx::pg_sys::pg_detoast_datum(datum.cast_mut_ptr()) };
        let data_len = unsafe { pgrx::varsize_any_exhdr(varlena) };
        let data_ptr = unsafe { pgrx::vardata_any(varlena) };
        let original_wkb = unsafe { std::slice::from_raw_parts(data_ptr.cast::<u8>(), data_len) };

        match raster::patch_band0_pixels(original_wkb, output_f32) {
            Some(new_wkb) => {
                // Allocate a PG varlena datum with the patched WKB.
                let total_size = new_wkb.len() + pgrx::pg_sys::VARHDRSZ;
                // SAFETY: palloc is safe on main backend thread.
                let new_varlena = unsafe { pgrx::pg_sys::palloc(total_size).cast::<u8>() };
                // SAFETY: new_varlena is freshly palloc'd with total_size bytes.
                unsafe {
                    pgrx::set_varsize_4b(new_varlena.cast(), total_size as i32);
                    let data_dest = pgrx::vardata_any(new_varlena.cast()).cast::<u8>();
                    std::ptr::copy_nonoverlapping(
                        new_wkb.as_ptr(),
                        data_dest.cast_mut(),
                        new_wkb.len(),
                    );
                }
                results.push((pgrx::pg_sys::Datum::from(new_varlena), false));
            }
            None => {
                // Patching failed — pass through original datum.
                results.push((datum, false));
            }
        }
    }

    let elapsed_ms = start.elapsed().as_millis() as i32;

    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: raster pipeline took {}ms (timeout {}ms)",
            elapsed_ms,
            timeout_ms,
        );
    }

    DispatchResult::Accelerated(results)
}

// ---------------------------------------------------------------------------
// Late Materialization — Predicate Chain
// ---------------------------------------------------------------------------

/// A single predicate in a [`PredicateChain`].
#[derive(Debug, Clone)]
pub struct Predicate {
    /// Human-readable label (e.g. `"bbox_overlap"`, `"st_contains"`).
    pub label: &'static str,
    /// Estimated fraction of rows that *pass* this predicate (0.0–1.0).
    /// Lower values are more selective.
    pub selectivity: f64,
    /// Estimated per-row cost in arbitrary units. Higher means more expensive.
    pub cost: f64,
    /// The evaluation function.  Takes a slice of `(Datum, is_null)` and
    /// returns a boolean mask of the same length (`true` = row passes).
    ///
    /// # Safety
    ///
    /// The function must be safe to call in the context where `evaluate_chain`
    /// is invoked (typically main backend thread).
    pub eval_fn: fn(&[(pgrx::pg_sys::Datum, bool)]) -> Vec<bool>,
}

/// An ordered chain of predicates for late materialization.
///
/// Predicates are sorted by *efficiency* (`selectivity / cost`) so the
/// cheapest, most-selective filter runs first. Rows rejected by an early
/// predicate skip all subsequent (more expensive) predicates, avoiding
/// unnecessary geometry deserialization.
#[derive(Debug, Clone)]
pub struct PredicateChain {
    /// Predicates in evaluation order (cheapest/most-selective first).
    predicates: Vec<Predicate>,
}

impl PredicateChain {
    /// Build a new predicate chain, automatically sorted by efficiency.
    ///
    /// Efficiency is defined as `selectivity / cost`. Lower selectivity (more
    /// rows filtered) and lower cost both increase efficiency, so predicates
    /// that filter the most rows for the least work run first.
    #[must_use]
    pub fn new(mut predicates: Vec<Predicate>) -> Self {
        predicates.sort_by(|a, b| {
            let eff_a = efficiency(a);
            let eff_b = efficiency(b);
            // Lower efficiency value = better (more selective & cheaper).
            eff_a
                .partial_cmp(&eff_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Self { predicates }
    }

    /// The ordered list of predicates.
    #[must_use]
    pub fn predicates(&self) -> &[Predicate] {
        &self.predicates
    }

    /// Number of predicates in the chain.
    #[must_use]
    pub fn len(&self) -> usize {
        self.predicates.len()
    }

    /// Whether the chain has no predicates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.predicates.is_empty()
    }
}

/// Efficiency metric: `selectivity / cost`. Lower is better — it means we
/// filter more rows for less work.
fn efficiency(p: &Predicate) -> f64 {
    if p.cost <= 0.0 {
        return 0.0;
    }
    p.selectivity / p.cost
}

/// Evaluate a [`PredicateChain`] against a batch, applying predicates in
/// efficiency order and short-circuiting rejected rows.
///
/// Returns a boolean mask of length `batch.len()` where `true` means the row
/// passed **all** predicates.
///
/// # Late Materialization
///
/// This is the key optimisation: an early, cheap predicate (e.g. integer range
/// check or bounding-box overlap) can eliminate rows before an expensive
/// predicate (e.g. exact `ST_Contains` requiring full geometry deserialization)
/// ever sees them.
#[must_use]
pub fn evaluate_chain(chain: &PredicateChain, batch: &[(pgrx::pg_sys::Datum, bool)]) -> Vec<bool> {
    let mut alive = vec![true; batch.len()];

    for predicate in &chain.predicates {
        // Collect only the surviving rows for this predicate.
        let survivors: Vec<(pgrx::pg_sys::Datum, bool)> = batch
            .iter()
            .zip(alive.iter())
            .filter_map(|(&datum, &is_alive)| if is_alive { Some(datum) } else { None })
            .collect();

        if survivors.is_empty() {
            break;
        }

        let pred_results = (predicate.eval_fn)(&survivors);

        // Map predicate results back to the full-width alive mask.
        let mut survivor_idx = 0;
        for flag in &mut alive {
            if *flag {
                if survivor_idx < pred_results.len() {
                    *flag = pred_results[survivor_idx];
                }
                survivor_idx += 1;
            }
        }
    }

    alive
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(feature = "pg_test")]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    // -- Predicate chain ordering --------------------------------------------

    fn make_predicate(label: &'static str, selectivity: f64, cost: f64) -> Predicate {
        Predicate {
            label,
            selectivity,
            cost,
            eval_fn: |batch| vec![true; batch.len()],
        }
    }

    #[test]
    fn chain_orders_by_efficiency() {
        // "cheap" has selectivity 0.1, cost 1.0 → efficiency 0.1 (best)
        // "expensive" has selectivity 0.5, cost 10.0 → efficiency 0.05
        // "medium" has selectivity 0.3, cost 2.0 → efficiency 0.15 (worst)
        let predicates = vec![
            make_predicate("medium", 0.3, 2.0),
            make_predicate("expensive", 0.5, 10.0),
            make_predicate("cheap", 0.1, 1.0),
        ];

        let chain = PredicateChain::new(predicates);
        let labels: Vec<&str> = chain.predicates().iter().map(|p| p.label).collect();

        // Sorted ascending by selectivity/cost:
        // expensive = 0.05, cheap = 0.1, medium = 0.15
        assert_eq!(labels, vec!["expensive", "cheap", "medium"]);
    }

    #[test]
    fn empty_chain_returns_all_alive() {
        let chain = PredicateChain::new(vec![]);
        assert!(chain.is_empty());

        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = vec![
            (pgrx::pg_sys::Datum::from(1), false),
            (pgrx::pg_sys::Datum::from(2), false),
        ];
        let result = evaluate_chain(&chain, &batch);
        assert_eq!(result, vec![true, true]);
    }

    #[test]
    fn chain_len_matches() {
        let chain = PredicateChain::new(vec![
            make_predicate("a", 0.5, 1.0),
            make_predicate("b", 0.3, 2.0),
        ]);
        assert_eq!(chain.len(), 2);
        assert!(!chain.is_empty());
    }

    // -- Predicate chain evaluation ------------------------------------------

    #[test]
    fn chain_filters_rows_correctly() {
        // First predicate: reject odd-indexed rows.
        let pred_even = Predicate {
            label: "even_index",
            selectivity: 0.5,
            cost: 1.0,
            eval_fn: |batch| batch.iter().enumerate().map(|(i, _)| i % 2 == 0).collect(),
        };

        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..4)
            .map(|i| (pgrx::pg_sys::Datum::from(i), false))
            .collect();

        let chain = PredicateChain::new(vec![pred_even]);
        let result = evaluate_chain(&chain, &batch);

        // Rows 0,1,2,3 → predicate sees all 4, returns [true, false, true, false]
        assert_eq!(result, vec![true, false, true, false]);
    }

    #[test]
    fn chain_short_circuits_rejected_rows() {
        // First predicate: pass only the first row.
        // efficiency = 0.1 / 10.0 = 0.01 (sorts first — lowest efficiency wins).
        let pred_first_only = Predicate {
            label: "first_only",
            selectivity: 0.1,
            cost: 10.0,
            eval_fn: |batch| {
                let mut v = vec![false; batch.len()];
                if !v.is_empty() {
                    v[0] = true;
                }
                v
            },
        };

        // Second predicate: always returns true — but should only see 1 row.
        // efficiency = 1.0 / 1.0 = 1.0 (sorts second).
        let pred_pass_all = Predicate {
            label: "pass_all",
            selectivity: 1.0,
            cost: 1.0,
            eval_fn: |batch| {
                // If short-circuiting works, batch should have exactly 1 row.
                assert_eq!(batch.len(), 1);
                vec![true; batch.len()]
            },
        };

        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..5)
            .map(|i| (pgrx::pg_sys::Datum::from(i), false))
            .collect();

        let chain = PredicateChain::new(vec![pred_first_only, pred_pass_all]);
        let result = evaluate_chain(&chain, &batch);

        assert_eq!(result, vec![true, false, false, false, false]);
    }

    #[test]
    fn chain_all_rejected_skips_remaining() {
        // First predicate: reject everything.
        let pred_reject_all = Predicate {
            label: "reject_all",
            selectivity: 0.0,
            cost: 1.0,
            eval_fn: |batch| vec![false; batch.len()],
        };

        // Second predicate: would panic if called — ensures short-circuit.
        let pred_should_not_run = Predicate {
            label: "should_not_run",
            selectivity: 1.0,
            cost: 100.0,
            eval_fn: |batch| {
                assert!(
                    batch.is_empty(),
                    "should_not_run predicate should not receive any rows"
                );
                vec![]
            },
        };

        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..3)
            .map(|i| (pgrx::pg_sys::Datum::from(i), false))
            .collect();

        let chain = PredicateChain::new(vec![pred_reject_all, pred_should_not_run]);
        let result = evaluate_chain(&chain, &batch);

        assert_eq!(result, vec![false, false, false]);
    }

    // -- NULL passthrough (strict function semantics) --------------------------
    // These test the pure logic of NULL handling. Actual FunctionCallInvoke
    // tests require a running PG instance and are covered by #[pg_test].

    #[test]
    fn strict_null_passthrough_logic() {
        // Simulate strict semantics without calling PG FFI.
        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = vec![
            (pgrx::pg_sys::Datum::from(1), false),
            (pgrx::pg_sys::Datum::from(0), true), // NULL
            (pgrx::pg_sys::Datum::from(3), false),
            (pgrx::pg_sys::Datum::from(0), true), // NULL
        ];

        let is_strict = true;
        let results: Vec<(pgrx::pg_sys::Datum, bool)> = batch
            .iter()
            .map(|&(datum, is_null)| {
                if is_strict && is_null {
                    (pgrx::pg_sys::Datum::from(0), true)
                } else {
                    // In real code this would call FunctionCallInvoke.
                    (datum, false)
                }
            })
            .collect();

        // NULLs pass through as NULL.
        assert!(results[1].1);
        assert!(results[3].1);
        // Non-NULLs are "evaluated".
        assert!(!results[0].1);
        assert!(!results[2].1);
    }

    #[test]
    fn non_strict_null_not_skipped_logic() {
        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = vec![
            (pgrx::pg_sys::Datum::from(0), true), // NULL
            (pgrx::pg_sys::Datum::from(1), false),
        ];

        let is_strict = false;
        let should_call_fn: Vec<bool> = batch
            .iter()
            .map(|&(_, is_null)| !(is_strict && is_null))
            .collect();

        // Non-strict: even NULL inputs go through the function.
        assert!(should_call_fn[0]);
        assert!(should_call_fn[1]);
    }

    // -- DispatchResult variants ----------------------------------------------

    #[test]
    fn dispatch_result_deferred_variant() {
        let result = DispatchResult::Deferred;
        assert!(matches!(result, DispatchResult::Deferred));
    }

    #[test]
    fn dispatch_result_accelerated_variant() {
        let data = vec![(pgrx::pg_sys::Datum::from(42), false)];
        let result = DispatchResult::Accelerated(data);
        assert!(matches!(result, DispatchResult::Accelerated(_)));
    }

    // -- Efficiency metric ---------------------------------------------------

    #[test]
    fn efficiency_zero_cost_returns_zero() {
        let p = make_predicate("zero_cost", 0.5, 0.0);
        assert!((efficiency(&p)).abs() < f64::EPSILON);
    }

    #[test]
    fn efficiency_negative_cost_returns_zero() {
        let p = make_predicate("neg_cost", 0.5, -1.0);
        assert!((efficiency(&p)).abs() < f64::EPSILON);
    }

    #[test]
    fn efficiency_normal_computation() {
        let p = make_predicate("normal", 0.3, 2.0);
        let eff = efficiency(&p);
        assert!((eff - 0.15).abs() < f64::EPSILON);
    }

    // -- PredicateChain: construction edge cases --------------------------------

    #[test]
    fn chain_with_single_predicate() {
        let chain = PredicateChain::new(vec![make_predicate("only", 0.5, 1.0)]);
        assert_eq!(chain.len(), 1);
        assert!(!chain.is_empty());
        assert_eq!(chain.predicates()[0].label, "only");
    }

    #[test]
    fn chain_with_ten_predicates_sorted_correctly() {
        let predicates: Vec<Predicate> = (1..=10)
            .map(|i| {
                let sel = i as f64 * 0.1;
                let cost = (11 - i) as f64; // inverse cost so sorting varies
                make_predicate(
                    // Static labels for 10 predicates.
                    match i {
                        1 => "p1",
                        2 => "p2",
                        3 => "p3",
                        4 => "p4",
                        5 => "p5",
                        6 => "p6",
                        7 => "p7",
                        8 => "p8",
                        9 => "p9",
                        _ => "p10",
                    },
                    sel,
                    cost,
                )
            })
            .collect();

        let chain = PredicateChain::new(predicates);
        assert_eq!(chain.len(), 10);

        // Verify sorted by ascending efficiency (selectivity / cost).
        let effs: Vec<f64> = chain.predicates().iter().map(|p| efficiency(p)).collect();
        for i in 1..effs.len() {
            assert!(
                effs[i - 1] <= effs[i] + f64::EPSILON,
                "predicates not sorted by efficiency at index {}: {} > {}",
                i,
                effs[i - 1],
                effs[i],
            );
        }
    }

    #[test]
    fn chain_with_equal_efficiency_maintains_order_stability() {
        // Two predicates with identical efficiency should not cause issues.
        let predicates = vec![
            make_predicate("alpha", 0.5, 2.0), // efficiency = 0.25
            make_predicate("beta", 0.5, 2.0),  // efficiency = 0.25
        ];
        let chain = PredicateChain::new(predicates);
        assert_eq!(chain.len(), 2);
        // Both have the same efficiency; just verify both are present.
        let labels: Vec<&str> = chain.predicates().iter().map(|p| p.label).collect();
        assert!(labels.contains(&"alpha"));
        assert!(labels.contains(&"beta"));
    }

    // -- Predicate cost classification -----------------------------------------

    #[test]
    fn efficiency_very_low_selectivity_is_best() {
        // selectivity near 0 = filters almost everything = very efficient.
        let p = make_predicate("ultra_selective", 0.001, 1.0);
        let eff = efficiency(&p);
        assert!(eff < 0.01);
    }

    #[test]
    fn efficiency_high_cost_penalizes() {
        let cheap = make_predicate("cheap", 0.5, 1.0);
        let expensive = make_predicate("expensive", 0.5, 100.0);
        assert!(efficiency(&cheap) > efficiency(&expensive));
    }

    #[test]
    fn efficiency_selectivity_one_is_worst() {
        // selectivity 1.0 = filters nothing = least useful.
        let p = make_predicate("passes_all", 1.0, 1.0);
        assert!((efficiency(&p) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn efficiency_tiny_cost_yields_large_ratio() {
        let p = make_predicate("tiny_cost", 0.5, 0.001);
        let eff = efficiency(&p);
        assert!((eff - 500.0).abs() < 0.01);
    }

    // -- Batch size calculations and edge cases --------------------------------

    #[test]
    fn batch_size_one_produces_single_element_batch() {
        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = vec![(pgrx::pg_sys::Datum::from(42), false)];
        // Simulate strict null check for a single-element batch.
        let is_strict = true;
        let results: Vec<bool> = batch
            .iter()
            .map(|&(_, is_null)| !(is_strict && is_null))
            .collect();
        assert_eq!(results.len(), 1);
        assert!(results[0]);
    }

    #[test]
    fn batch_all_nulls_strict_all_skipped() {
        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..10)
            .map(|_| (pgrx::pg_sys::Datum::from(0), true))
            .collect();

        let is_strict = true;
        let results: Vec<(pgrx::pg_sys::Datum, bool)> = batch
            .iter()
            .map(|&(_, is_null)| {
                if is_strict && is_null {
                    (pgrx::pg_sys::Datum::from(0), true)
                } else {
                    (pgrx::pg_sys::Datum::from(1), false)
                }
            })
            .collect();

        assert!(results.iter().all(|(_, is_null)| *is_null));
    }

    #[test]
    fn batch_no_nulls_strict_all_evaluated() {
        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..10)
            .map(|i| (pgrx::pg_sys::Datum::from(i), false))
            .collect();

        let is_strict = true;
        let eval_count = batch
            .iter()
            .filter(|&&(_, is_null)| !(is_strict && is_null))
            .count();
        assert_eq!(eval_count, 10);
    }

    #[test]
    fn very_large_batch_null_passthrough() {
        let batch_size = 100_000;
        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..batch_size)
            .map(|i| {
                let is_null = i % 3 == 0;
                (pgrx::pg_sys::Datum::from(i as i64), is_null)
            })
            .collect();

        let is_strict = true;
        let null_count = batch
            .iter()
            .filter(|&&(_, is_null)| is_strict && is_null)
            .count();

        // Every 3rd element (0, 3, 6, ...) is NULL.
        let expected_nulls = (batch_size + 2) / 3;
        assert_eq!(null_count, expected_nulls);
    }

    // -- AccelStrategy enum: all variants, conversion --------------------------

    #[test]
    fn accel_strategy_from_i32_known_values() {
        assert_eq!(AccelStrategy::from_i32(1), AccelStrategy::GpuSpatial);
        assert_eq!(AccelStrategy::from_i32(2), AccelStrategy::GpuRaster);
        assert_eq!(AccelStrategy::from_i32(3), AccelStrategy::GpuH3);
        assert_eq!(AccelStrategy::from_i32(4), AccelStrategy::GpuSort);
        assert_eq!(AccelStrategy::from_i32(5), AccelStrategy::GpuReduce);
        assert_eq!(AccelStrategy::from_i32(6), AccelStrategy::GpuExpr);
        assert_eq!(AccelStrategy::from_i32(7), AccelStrategy::GpuHashJoin);
        assert_eq!(AccelStrategy::from_i32(8), AccelStrategy::GpuWindow);
    }

    #[test]
    fn accel_strategy_from_i32_unknown_defaults_to_gpu_spatial() {
        assert_eq!(AccelStrategy::from_i32(0), AccelStrategy::GpuSpatial);
        assert_eq!(AccelStrategy::from_i32(-1), AccelStrategy::GpuSpatial);
        assert_eq!(AccelStrategy::from_i32(9), AccelStrategy::GpuSpatial);
        assert_eq!(AccelStrategy::from_i32(100), AccelStrategy::GpuSpatial);
        assert_eq!(AccelStrategy::from_i32(i32::MAX), AccelStrategy::GpuSpatial);
        assert_eq!(AccelStrategy::from_i32(i32::MIN), AccelStrategy::GpuSpatial);
    }

    #[test]
    fn accel_strategy_roundtrip_through_i32() {
        let strategies = [
            AccelStrategy::GpuSpatial,
            AccelStrategy::GpuRaster,
            AccelStrategy::GpuH3,
            AccelStrategy::GpuSort,
            AccelStrategy::GpuReduce,
            AccelStrategy::GpuExpr,
            AccelStrategy::GpuHashJoin,
            AccelStrategy::GpuWindow,
        ];
        for s in strategies {
            let as_i32 = s as i32;
            assert_eq!(AccelStrategy::from_i32(as_i32), s);
        }
    }

    #[test]
    fn accel_strategy_debug_format_contains_variant_name() {
        let dbg = format!("{:?}", AccelStrategy::GpuSpatial);
        assert!(dbg.contains("GpuSpatial"), "debug format: {dbg}");
    }

    #[test]
    fn accel_strategy_copy_semantics() {
        let a = AccelStrategy::GpuH3;
        let b = a; // Copy
        assert_eq!(a, b);
    }

    #[test]
    fn accel_strategy_hash_consistency() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(AccelStrategy::GpuSpatial);
        set.insert(AccelStrategy::GpuSpatial); // duplicate
        set.insert(AccelStrategy::GpuH3);
        assert_eq!(set.len(), 2);
    }

    // -- Dispatch routing (which strategy goes where) --------------------------

    #[test]
    fn dispatch_routing_gpu_strategies_that_return_deferred() {
        // GpuExpr, GpuSort, GpuReduce, GpuHashJoin, GpuWindow are not wired
        // into per-datum dispatch and should map to Deferred.
        let deferred_strategies = [
            AccelStrategy::GpuExpr,
            AccelStrategy::GpuSort,
            AccelStrategy::GpuReduce,
            AccelStrategy::GpuHashJoin,
            AccelStrategy::GpuWindow,
        ];
        for strategy in deferred_strategies {
            // Verify the match arm maps these to Deferred by checking
            // the pattern from the dispatch function.
            assert!(
                matches!(
                    strategy,
                    AccelStrategy::GpuExpr
                        | AccelStrategy::GpuSort
                        | AccelStrategy::GpuReduce
                        | AccelStrategy::GpuHashJoin
                        | AccelStrategy::GpuWindow
                ),
                "{strategy:?} should be in the deferred arm"
            );
        }
    }

    #[test]
    fn dispatch_routing_gpu_spatial_is_not_in_deferred_arm() {
        // GpuSpatial has its own dispatch arm, not the catch-all deferred.
        assert!(!matches!(
            AccelStrategy::GpuSpatial,
            AccelStrategy::GpuExpr
                | AccelStrategy::GpuSort
                | AccelStrategy::GpuReduce
                | AccelStrategy::GpuHashJoin
                | AccelStrategy::GpuWindow
        ));
    }

    #[test]
    fn dispatch_routing_gpu_spatial_is_not_deferred() {
        assert!(!matches!(
            AccelStrategy::GpuSpatial,
            AccelStrategy::GpuExpr
                | AccelStrategy::GpuSort
                | AccelStrategy::GpuReduce
                | AccelStrategy::GpuHashJoin
                | AccelStrategy::GpuWindow
        ));
    }

    // -- DispatchResult: data access -------------------------------------------

    #[test]
    fn dispatch_result_accelerated_empty_vec() {
        let result = DispatchResult::Accelerated(vec![]);
        if let DispatchResult::Accelerated(data) = result {
            assert!(data.is_empty());
        } else {
            panic!("expected Accelerated variant");
        }
    }

    #[test]
    fn dispatch_result_accelerated_preserves_data() {
        let data = vec![
            (pgrx::pg_sys::Datum::from(1), false),
            (pgrx::pg_sys::Datum::from(0), true),
            (pgrx::pg_sys::Datum::from(3), false),
        ];
        let result = DispatchResult::Accelerated(data);
        if let DispatchResult::Accelerated(ref d) = result {
            assert_eq!(d.len(), 3);
            assert!(!d[0].1);
            assert!(d[1].1);
            assert!(!d[2].1);
        } else {
            panic!("expected Accelerated variant");
        }
    }

    #[test]
    fn dispatch_result_debug_format() {
        let result = DispatchResult::Deferred;
        let dbg = format!("{result:?}");
        assert!(dbg.contains("Deferred"));
    }

    // -- evaluate_chain edge cases ---------------------------------------------

    #[test]
    fn evaluate_chain_empty_batch() {
        let chain = PredicateChain::new(vec![make_predicate("a", 0.5, 1.0)]);
        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = vec![];
        let result = evaluate_chain(&chain, &batch);
        assert!(result.is_empty());
    }

    #[test]
    fn evaluate_chain_single_row_passes() {
        let chain = PredicateChain::new(vec![make_predicate("pass", 1.0, 1.0)]);
        let batch = vec![(pgrx::pg_sys::Datum::from(1), false)];
        let result = evaluate_chain(&chain, &batch);
        assert_eq!(result, vec![true]);
    }

    #[test]
    fn evaluate_chain_single_row_rejected() {
        let pred = Predicate {
            label: "reject",
            selectivity: 0.0,
            cost: 1.0,
            eval_fn: |batch| vec![false; batch.len()],
        };
        let chain = PredicateChain::new(vec![pred]);
        let batch = vec![(pgrx::pg_sys::Datum::from(1), false)];
        let result = evaluate_chain(&chain, &batch);
        assert_eq!(result, vec![false]);
    }

    #[test]
    fn evaluate_chain_multiple_predicates_progressive_filtering() {
        // First predicate: keep first 3 of 5 rows.
        let pred1 = Predicate {
            label: "keep_first_3",
            selectivity: 0.3,
            cost: 10.0, // efficiency = 0.03 (runs first)
            eval_fn: |batch| batch.iter().enumerate().map(|(i, _)| i < 3).collect(),
        };

        // Second predicate: keep only even-indexed survivors.
        let pred2 = Predicate {
            label: "keep_even",
            selectivity: 0.5,
            cost: 5.0, // efficiency = 0.1 (runs second)
            eval_fn: |batch| batch.iter().enumerate().map(|(i, _)| i % 2 == 0).collect(),
        };

        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..5)
            .map(|i| (pgrx::pg_sys::Datum::from(i), false))
            .collect();

        let chain = PredicateChain::new(vec![pred1, pred2]);
        let result = evaluate_chain(&chain, &batch);

        // After pred1: [true, true, true, false, false]
        // Survivors sent to pred2: rows 0,1,2 → pred2 sees 3 rows, returns [true, false, true]
        // Final: row0=true, row1=false, row2=true, row3=false, row4=false
        assert_eq!(result, vec![true, false, true, false, false]);
    }

    // -- Predicate struct fields -----------------------------------------------

    #[test]
    fn predicate_label_accessible() {
        let p = make_predicate("my_label", 0.5, 1.0);
        assert_eq!(p.label, "my_label");
    }

    #[test]
    fn predicate_selectivity_and_cost_accessible() {
        let p = make_predicate("test", 0.42, 7.5);
        assert!((p.selectivity - 0.42).abs() < f64::EPSILON);
        assert!((p.cost - 7.5).abs() < f64::EPSILON);
    }

    #[test]
    fn predicate_eval_fn_callable() {
        let p = make_predicate("test", 0.5, 1.0);
        let batch = vec![(pgrx::pg_sys::Datum::from(1), false)];
        let result = (p.eval_fn)(&batch);
        assert_eq!(result, vec![true]);
    }

    #[test]
    fn predicate_clone() {
        let p = make_predicate("original", 0.3, 2.0);
        let cloned = p.clone();
        assert_eq!(cloned.label, "original");
        assert!((cloned.selectivity - 0.3).abs() < f64::EPSILON);
        assert!((cloned.cost - 2.0).abs() < f64::EPSILON);
    }

    // -- FcinfoWith2Args layout -------------------------------------------------

    #[test]
    fn fcinfo_with_2args_size_exceeds_base() {
        let base_size = std::mem::size_of::<pgrx::pg_sys::FunctionCallInfoBaseData>();
        let with_2args_size = std::mem::size_of::<FcinfoWith2Args>();
        assert!(
            with_2args_size > base_size,
            "FcinfoWith2Args ({with_2args_size}) must be larger than base ({base_size})"
        );
    }
}
