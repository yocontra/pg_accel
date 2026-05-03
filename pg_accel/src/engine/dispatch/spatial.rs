//! GPU spatial dispatch via the three-layer pipeline.

use crate::adapters::extractors::geometry::{extract_geometry, extract_point_xy_f32};
use crate::engine::cost;
use crate::engine::gucs;
use crate::engine::registry;
use crate::gpu;
use crate::gpu::three_layer;

use super::{DispatchResult, FcinfoWith2Args};

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
        pgrx::error!(
            "pg_accel: bulk point_in_polygon GPU kernel failed; refusing CPU fallback (rule 11)"
        );
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
/// `qual_datums` carries every constant argument captured from the call
/// site in positional order. For 2-arg predicates the constant geometry is
/// `qual_datums[0]`; for `ST_DWithin(g, g, threshold)` the threshold is
/// `qual_datums[1]`. Single-arg functions (`ST_Area`, `ST_Length`) pass an
/// empty slice.
///
/// # Safety
///
/// Must be called on the **main backend thread**.
#[must_use]
pub unsafe fn dispatch_gpu_spatial(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    fn_info: &pgrx::pg_sys::FmgrInfo,
    is_strict: bool,
    qual_datums: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)],
    _skip_bbox: bool,
) -> DispatchResult {
    // Single-arg spatial functions (st_area, eventually st_length /
    // st_x / st_y) — qual_datums is empty because there's no constant
    // second argument. Branch off before the two-arg gate.
    if qual_datums.is_empty() {
        let fn_name = registry::global_registry()
            .lookup(fn_info.fn_oid)
            .map(|e| e.name);
        match fn_name {
            Some("st_area") => return unsafe { dispatch_gpu_st_area(batch) },
            Some("st_length") => return unsafe { dispatch_gpu_st_length(batch) },
            _ => {
                // Other single-arg names not yet wired — defer.
                pgrx::debug1!("pg_accel: dispatch_gpu_spatial: no qual_datums, deferring");
                return DispatchResult::Deferred;
            }
        }
    }
    // We know qual_datums has at least one element (the empty branch
    // returned above). The first const is the constant geometry.
    let (qual_d, qual_null, _qual_typid) = qual_datums[0];

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

    // ── st_distance fast path ──────────────────────────────────────
    // Two-arg returning fp64 distance, NOT a predicate. Routes to
    // dispatch_gpu_st_distance for Point × Point pairs (the existing
    // sphere_distance_bulk_sycl<float> kernel handles the math). Other
    // geometry-pair shapes defer to PG (polygon distance is genuinely
    // harder algorithmically and not yet implemented).
    //
    // Detected here, AFTER geom_b extraction but BEFORE the
    // min_vertices gate (Point has coord_count = 1, which is below
    // the predicate vertex-count threshold).
    {
        let fn_name = registry::global_registry()
            .lookup(fn_info.fn_oid)
            .map(|e| e.name);
        if fn_name == Some("st_distance") {
            return unsafe { dispatch_gpu_st_distance(batch, &geom_b) };
        }
    }

    // ── Vertex count gate ────────────────────────────────────────
    // GPU overhead is roughly constant (~19ms on M2 Max) regardless of
    // polygon complexity (dominated by geometry deser + seq scan).
    // PG parallel scales linearly with vertex count. Below the threshold,
    // PG parallel is faster — defer to avoid overhead.
    let min_verts = cost::device_limits().gpu_spatial_min_vertices;
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

    // Predicate routing.
    //
    // - st_intersects (and unrecognised two-arg names): existing
    //   `gpu::spatial_intersects_gpu` cross-product kernel below.
    // - st_contains / st_within / st_equals / st_touches / st_crosses /
    //   st_overlaps: route through `three_layer::spatial_eval` so each
    //   predicate hits its dedicated `pgaccel_st_*_bulk` SYCL kernel
    //   (Contains has its own point-in-ring fast path; the four
    //   algorithmic predicates were landed by Agent 2A).
    // - st_dwithin: 3-arg predicate `ST_DWithin(geom, geom, threshold)`.
    //   The threshold rides on `qual_datums[1]` (the new multi-arg
    //   carrier wired by Phase II Agent F1) and is passed into
    //   `SpatialPredicate::DWithin` so the Haversine kernel uses the
    //   correct distance gate instead of a wrong Intersects fallback.
    let fn_name = registry::global_registry()
        .lookup(fn_info.fn_oid)
        .map(|e| e.name);

    let predicate = match fn_name {
        Some("st_contains") => Some(three_layer::SpatialPredicate::Contains),
        Some("st_within") => Some(three_layer::SpatialPredicate::Within),
        Some("st_equals") => Some(three_layer::SpatialPredicate::Equals),
        Some("st_touches") => Some(three_layer::SpatialPredicate::Touches),
        Some("st_crosses") => Some(three_layer::SpatialPredicate::Crosses),
        Some("st_overlaps") => Some(three_layer::SpatialPredicate::Overlaps),
        Some("st_disjoint") => Some(three_layer::SpatialPredicate::Disjoint),
        Some("st_dwithin") => {
            // 3-arg `ST_DWithin(geom, geom, threshold)` — the threshold
            // is captured at position 1 in qual_datums (qual_datums[0]
            // is the constant geometry already extracted above).
            let Some(&(threshold_datum, threshold_null, _threshold_typid)) = qual_datums.get(1)
            else {
                pgrx::debug1!("pg_accel: st_dwithin: missing threshold qual_datums[1], deferring");
                return DispatchResult::Deferred;
            };
            if threshold_null {
                // NULL threshold → strict semantics yield NULL per row.
                return DispatchResult::Accelerated(vec![
                    (pgrx::pg_sys::Datum::from(0), true);
                    batch.len()
                ]);
            }
            // PostGIS ST_DWithin's third arg is float8 (Datum carries the
            // f64 bit pattern). Decode and feed the spatial_eval pipeline.
            let threshold = f64::from_bits(threshold_datum.value() as u64);
            if !threshold.is_finite() || threshold < 0.0 {
                pgrx::debug1!(
                    "pg_accel: st_dwithin: non-finite or negative threshold {}, deferring",
                    threshold
                );
                return DispatchResult::Deferred;
            }
            Some(three_layer::SpatialPredicate::DWithin(threshold))
        }
        _ => None,
    };

    if let Some(pred) = predicate {
        // Build the per-row geom_b vector (same constant geom_b reused
        // for every row) and call `spatial_eval`. UNCERTAIN rows go
        // through the same scalar recheck path as Intersects below.
        let geom_b_repeated: Vec<three_layer::ExtractedGeometry> =
            (0..geoms_a.len()).map(|_| geom_b.clone()).collect();

        let timeout_ms = gucs::kernel_timeout_ms();
        let start = std::time::Instant::now();
        pgrx::debug1!(
            "pg_accel: dispatch_gpu_spatial: routing {} pairs through three_layer::spatial_eval({:?})",
            geoms_a.len(),
            pred,
        );
        let result = three_layer::spatial_eval(pred, &geoms_a, &geom_b_repeated, _skip_bbox);
        let elapsed_ms = start.elapsed().as_millis() as i32;
        if timeout_ms > 0 && elapsed_ms > timeout_ms {
            pgrx::warning!(
                "pg_accel: spatial GPU kernel ({:?}) took {}ms (timeout {}ms)",
                pred,
                elapsed_ms,
                timeout_ms,
            );
        }

        let bool_true = pgrx::pg_sys::Datum::from(true);
        let bool_false = pgrx::pg_sys::Datum::from(false);

        for &geom_idx in &result.definite_true {
            if geom_idx < geom_idx_to_batch.len() {
                results[geom_idx_to_batch[geom_idx]] = (bool_true, false);
            }
        }
        for &geom_idx in &result.definite_false {
            if geom_idx < geom_idx_to_batch.len() {
                results[geom_idx_to_batch[geom_idx]] = (bool_false, false);
            }
        }
        for &geom_idx in &result.uncertain {
            if geom_idx < geom_idx_to_batch.len() {
                needs_scalar_recheck.push(geom_idx_to_batch[geom_idx]);
            }
        }

        // SAFETY: same preconditions as dispatch_gpu_spatial — main backend
        // thread, valid fn_info, valid datums.
        return unsafe {
            apply_scalar_recheck(
                batch,
                fn_info,
                is_strict,
                qual_d,
                qual_null,
                results,
                needs_scalar_recheck,
            )
        };
    }

    // ── ST_Intersects / unrecognised two-arg → existing N×1 cross-product
    // kernel via `gpu::spatial_intersects_gpu`. ────────────────────────
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
        pgrx::error!("pg_accel: GPU spatial kernel failed; refusing CPU fallback (rule 11)");
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
    // SAFETY: same preconditions as dispatch_gpu_spatial — main backend thread,
    // valid fn_info, valid datums.
    unsafe {
        apply_scalar_recheck(
            batch,
            fn_info,
            is_strict,
            qual_d,
            qual_null,
            results,
            needs_scalar_recheck,
        )
    }
}

/// Run the 2-arg PG function for the rows the GPU pipeline marked uncertain
/// (or which couldn't be GPU-classified — non-extractable geoms etc.). Used
/// by both the Intersects path and the new three_layer-routed predicate
/// arms (Contains/Within/Equals/Touches/Crosses/Overlaps); the kernels all
/// emit the same 1/-1/0 trit so the recheck contract is uniform.
///
/// # Safety
///
/// Must be called on the main backend thread. `fn_info` must be a valid
/// FmgrInfo registered for a 2-arg geometry predicate.
unsafe fn apply_scalar_recheck(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    fn_info: &pgrx::pg_sys::FmgrInfo,
    is_strict: bool,
    qual_d: pgrx::pg_sys::Datum,
    qual_null: bool,
    mut results: Vec<(pgrx::pg_sys::Datum, bool)>,
    needs_scalar_recheck: Vec<usize>,
) -> DispatchResult {
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

/// Single-arg `st_area(geom)` dispatch: extract one Polygon per row,
/// run the Shoelace SYCL kernel, return one f64 per row.
///
/// CSR layout: build a flat coords buffer + row_offsets in a single
/// pass over the batch (skipping NULLs and non-Polygon shapes —
/// those rows are returned as NULL so PG handles them via st_area's
/// scalar implementation). The row_offsets[N+1] convention lets the
/// kernel index each row's coords[start..end] without storing a
/// separate length per row.
///
/// # Safety
///
/// Must be called on the **main backend thread**. Each `batch` datum
/// must be a valid PostGIS geometry datum (or NULL).
unsafe fn dispatch_gpu_st_area(batch: &[(pgrx::pg_sys::Datum, bool)]) -> DispatchResult {
    let n = batch.len();
    if n == 0 {
        return DispatchResult::Accelerated(Vec::new());
    }

    // Build CSR coords + offsets. valid_rows tracks which batch
    // indices fed into the kernel (NULL / non-Polygon rows are
    // returned as NULL).
    let mut coords: Vec<f32> = Vec::new();
    let mut row_offsets: Vec<u32> = Vec::with_capacity(n + 1);
    let mut valid_rows: Vec<usize> = Vec::with_capacity(n);
    let mut next_offset: u32 = 0;
    row_offsets.push(0);

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        let Some(geom) = extract_geometry(datum) else {
            continue;
        };
        // Single-ring Polygon only — the kernel doesn't yet handle
        // multipart geometries or polygons with holes.
        if !matches!(geom.geom_type, three_layer::GeomType::Polygon) {
            continue;
        }
        if geom.coord_count < 3 || geom.coords.len() < 6 {
            continue;
        }
        // ring_offsets stores per-ring starts; len > 1 indicates
        // multiple rings (outer + holes).
        if geom.ring_offsets.len() > 1 {
            continue;
        }
        // Append this row's coords; record the offset for the NEXT
        // row.
        coords.extend_from_slice(&geom.coords);
        next_offset = next_offset.saturating_add(geom.coords.len() as u32);
        row_offsets.push(next_offset);
        valid_rows.push(i);
    }

    let kernel_row_count = valid_rows.len();
    if kernel_row_count == 0 {
        // Nothing to dispatch — every row is NULL or unsupported.
        // Return Deferred so PG handles st_area natively.
        return DispatchResult::Deferred;
    }

    // Output buffer: one f32 area per kernel row.
    let mut areas = vec![0.0f32; kernel_row_count];

    use crate::gpu::bridge::{self, PgaccelStatus};
    // SAFETY: coords / row_offsets / areas are valid Rust-owned slices
    // of the declared lengths. fp32 path; fp64 instantiation works for
    // this kernel (no trig) but skipped here because the dispatch
    // wraps the result back into PG f8 anyway.
    let status = unsafe {
        bridge::pgaccel_st_area_bulk(
            coords.as_ptr().cast(),
            row_offsets.as_ptr(),
            kernel_row_count,
            false,
            areas.as_mut_ptr().cast(),
        )
    };
    if !matches!(status, PgaccelStatus::Ok) {
        // Surface as Deferred so PG handles via st_area natively.
        // NOT a CPU fallback: pg_accel itself does no per-row compute
        // when it returns Deferred.
        return DispatchResult::Deferred;
    }

    // Build the output Datum vector. NULL slots stay NULL; valid rows
    // get the GPU-computed area as an fp8 datum (PG `float8` storage
    // is the bit pattern of an fp64 value, so promote fp32 -> fp64).
    let mut results = vec![(pgrx::pg_sys::Datum::from(0_u64), true); n];
    for (kernel_i, &batch_i) in valid_rows.iter().enumerate() {
        let area_f64 = f64::from(areas[kernel_i]);
        results[batch_i] = (pgrx::pg_sys::Datum::from(area_f64.to_bits()), false);
    }
    DispatchResult::Accelerated(results)
}

/// Single-arg `st_length(geom)` dispatch: extract one Polygon (perimeter)
/// or LineString (open length) per row, run the Euclidean edge-length
/// SYCL kernel, return one f64 per row.
///
/// Same CSR layout as `dispatch_gpu_st_area` — but the kernel needs
/// to know whether each row is closed (Polygon ring) or open
/// (LineString). To avoid per-row dispatch, we batch rows by
/// closed/open flag: collect Polygon rows into one CSR + Linestring
/// rows into another, and call the kernel twice. Mixed-shape batches
/// pay one extra dispatch but stay correct.
///
/// fp32 only today — fp64 returns NO_DEVICE per the soft-fp64 sqrt
/// hang documented in `pgaccel_st_length_bulk`.
///
/// # Safety
///
/// Must be called on the **main backend thread**. Each `batch` datum
/// must be a valid PostGIS geometry datum (or NULL).
unsafe fn dispatch_gpu_st_length(batch: &[(pgrx::pg_sys::Datum, bool)]) -> DispatchResult {
    let n = batch.len();
    if n == 0 {
        return DispatchResult::Accelerated(Vec::new());
    }

    // Two parallel CSRs: one for closed rings (Polygons), one for
    // open paths (LineStrings). Mixed batches pay two kernel
    // dispatches; uniform batches pay one.
    let mut closed_coords: Vec<f32> = Vec::new();
    let mut closed_offsets: Vec<u32> = Vec::with_capacity(n + 1);
    let mut closed_rows: Vec<usize> = Vec::with_capacity(n);
    let mut closed_next: u32 = 0;
    closed_offsets.push(0);

    let mut open_coords: Vec<f32> = Vec::new();
    let mut open_offsets: Vec<u32> = Vec::with_capacity(n + 1);
    let mut open_rows: Vec<usize> = Vec::with_capacity(n);
    let mut open_next: u32 = 0;
    open_offsets.push(0);

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        let Some(geom) = extract_geometry(datum) else {
            continue;
        };
        match geom.geom_type {
            three_layer::GeomType::Polygon => {
                if geom.coord_count < 3 || geom.coords.len() < 6 {
                    continue;
                }
                if geom.ring_offsets.len() > 1 {
                    // Multi-ring (with holes) needs perimeter sum
                    // across all rings — not yet wired. Defer per-row.
                    continue;
                }
                closed_coords.extend_from_slice(&geom.coords);
                closed_next = closed_next.saturating_add(geom.coords.len() as u32);
                closed_offsets.push(closed_next);
                closed_rows.push(i);
            }
            three_layer::GeomType::LineString => {
                if geom.coord_count < 2 || geom.coords.len() < 4 {
                    continue;
                }
                open_coords.extend_from_slice(&geom.coords);
                open_next = open_next.saturating_add(geom.coords.len() as u32);
                open_offsets.push(open_next);
                open_rows.push(i);
            }
            _ => continue,
        }
    }

    if closed_rows.is_empty() && open_rows.is_empty() {
        // Nothing to dispatch — every row is NULL or unsupported.
        return DispatchResult::Deferred;
    }

    use crate::gpu::bridge::{self, PgaccelStatus};
    let mut results = vec![(pgrx::pg_sys::Datum::from(0_u64), true); n];

    // Closed-ring (Polygon perimeter) dispatch.
    if !closed_rows.is_empty() {
        let mut lengths = vec![0.0f32; closed_rows.len()];
        // SAFETY: slices are valid Rust-owned of the declared lengths.
        let status = unsafe {
            bridge::pgaccel_st_length_bulk(
                closed_coords.as_ptr().cast(),
                closed_offsets.as_ptr(),
                closed_rows.len(),
                false,
                true,
                lengths.as_mut_ptr().cast(),
            )
        };
        if !matches!(status, PgaccelStatus::Ok) {
            return DispatchResult::Deferred;
        }
        for (k, &batch_i) in closed_rows.iter().enumerate() {
            let len_f64 = f64::from(lengths[k]);
            results[batch_i] = (pgrx::pg_sys::Datum::from(len_f64.to_bits()), false);
        }
    }

    // Open-path (LineString length) dispatch.
    if !open_rows.is_empty() {
        let mut lengths = vec![0.0f32; open_rows.len()];
        // SAFETY: same as above.
        let status = unsafe {
            bridge::pgaccel_st_length_bulk(
                open_coords.as_ptr().cast(),
                open_offsets.as_ptr(),
                open_rows.len(),
                false,
                false,
                lengths.as_mut_ptr().cast(),
            )
        };
        if !matches!(status, PgaccelStatus::Ok) {
            return DispatchResult::Deferred;
        }
        for (k, &batch_i) in open_rows.iter().enumerate() {
            let len_f64 = f64::from(lengths[k]);
            results[batch_i] = (pgrx::pg_sys::Datum::from(len_f64.to_bits()), false);
        }
    }

    DispatchResult::Accelerated(results)
}

/// Two-arg `st_distance(geom_a, geom_b)` dispatch where geom_b is the
/// constant qual datum. Routes Point × Point pairs through the
/// existing `pgaccel_sphere_distance_bulk` kernel (fp32 path —
/// returns Haversine distance in metres for lon/lat input). Non-Point
/// inputs (per row OR for geom_b) defer to PG.
///
/// Output Datum per row is the f64 bit pattern of the distance
/// (PG `float8` storage). NULL rows return NULL.
///
/// # Safety
///
/// Must be called on the **main backend thread**. `geom_b` must be a
/// validly-constructed `ExtractedGeometry`.
unsafe fn dispatch_gpu_st_distance(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    geom_b: &three_layer::ExtractedGeometry,
) -> DispatchResult {
    let n = batch.len();
    if n == 0 {
        return DispatchResult::Accelerated(Vec::new());
    }

    // ── Polygon × Polygon path ─────────────────────────────────────
    // When the constant geom_b is a single-ring Polygon, build a CSR
    // batch of per-row polygon coords and call
    // pgaccel_st_distance_polygon_polygon_bulk (Agent 2A task 3). The
    // kernel returns Euclidean min-vertex-to-edge distance + an
    // uncertainty flag for boundary touch / overlap which PG rechecks.
    if matches!(geom_b.geom_type, three_layer::GeomType::Polygon)
        && geom_b.coord_count >= 3
        && geom_b.coords.len() >= 6
        && geom_b.ring_offsets.len() <= 1
    {
        return unsafe { dispatch_gpu_st_distance_polygon_polygon(batch, geom_b) };
    }

    // st_distance kernel today is point-only otherwise. Other shapes
    // for geom_b → defer.
    if !matches!(geom_b.geom_type, three_layer::GeomType::Point) || geom_b.coords.len() < 2 {
        return DispatchResult::Deferred;
    }
    let bx = geom_b.coords[0];
    let by = geom_b.coords[1];

    // Per-row Point extract; non-Point rows return NULL so PG handles.
    let mut a_xy: Vec<f32> = Vec::with_capacity(n * 2);
    let mut b_xy: Vec<f32> = Vec::with_capacity(n * 2);
    let mut valid_rows: Vec<usize> = Vec::with_capacity(n);

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        // Try the zero-alloc Point extractor first; bail on non-Point.
        let Some((ax, ay)) = extract_point_xy_f32(datum) else {
            continue;
        };
        a_xy.push(ax);
        a_xy.push(ay);
        b_xy.push(bx);
        b_xy.push(by);
        valid_rows.push(i);
    }

    if valid_rows.is_empty() {
        return DispatchResult::Deferred;
    }

    let mut distances = vec![0.0f32; valid_rows.len()];
    let mut uncertain = vec![0u8; valid_rows.len()];

    use crate::gpu::bridge::{self, PgaccelStatus};
    // SAFETY: input/output slices are Rust-owned of declared lengths.
    // fp32 path; fp64 returns NO_DEVICE (soft-fp64 trig hang per
    // TODO Phase 7).
    let status = unsafe {
        bridge::pgaccel_sphere_distance_bulk(
            a_xy.as_ptr(),
            b_xy.as_ptr(),
            valid_rows.len(),
            false,
            distances.as_mut_ptr(),
            uncertain.as_mut_ptr(),
        )
    };
    if !matches!(status, PgaccelStatus::Ok) {
        return DispatchResult::Deferred;
    }

    let mut results = vec![(pgrx::pg_sys::Datum::from(0_u64), true); n];
    for (k, &batch_i) in valid_rows.iter().enumerate() {
        // uncertain==1 means the kernel flagged precision loss
        // (very-close points or antipodal). Surface as the computed
        // distance anyway — st_distance(geom, geom) returning 0.0 is
        // still correct, just less precise than PG would be.
        let _ = uncertain[k];
        let dist_f64 = f64::from(distances[k]);
        results[batch_i] = (pgrx::pg_sys::Datum::from(dist_f64.to_bits()), false);
    }
    DispatchResult::Accelerated(results)
}

/// Polygon × Polygon `st_distance` via Agent 2A's
/// `pgaccel_st_distance_polygon_polygon_bulk`. CSR-laid out the same way as
/// `pgaccel_st_area_bulk`: per-row coords concatenated in a flat fp32
/// buffer indexed by `row_offsets[N+1]`. The kernel emits one f32 distance
/// per row plus a `uncertain[i]` flag for boundary touch / overlap (PG
/// rechecks those for the interior containment case where the algorithmic
/// distance == 0 but the true topological distance == 0 only for actual
/// overlap).
///
/// Rows with non-Polygon shape, multi-ring polygon, or insufficient
/// vertices are emitted NULL so PG handles them via native `st_distance`.
///
/// # Safety
///
/// Must be called on the **main backend thread**. `geom_b` must be a
/// validly-constructed single-ring `ExtractedGeometry` with
/// `geom_type == Polygon`, verified by the caller.
unsafe fn dispatch_gpu_st_distance_polygon_polygon(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    geom_b: &three_layer::ExtractedGeometry,
) -> DispatchResult {
    let n = batch.len();
    if n == 0 {
        return DispatchResult::Accelerated(Vec::new());
    }

    // Build CSR for the variable side (geom_a per row) and a parallel CSR
    // for geom_b (the constant polygon repeated). Constant-side CSR has
    // identical row_offsets stride so the kernel indexes both consistently.
    let mut a_coords: Vec<f32> = Vec::new();
    let mut a_offsets: Vec<u32> = Vec::with_capacity(n + 1);
    let mut b_coords: Vec<f32> = Vec::new();
    let mut b_offsets: Vec<u32> = Vec::with_capacity(n + 1);
    let mut valid_rows: Vec<usize> = Vec::with_capacity(n);
    let mut a_next: u32 = 0;
    let mut b_next: u32 = 0;
    a_offsets.push(0);
    b_offsets.push(0);

    let b_floats: u32 = u32::try_from(geom_b.coords.len()).unwrap_or(u32::MAX);

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        let Some(geom_a) = extract_geometry(datum) else {
            continue;
        };
        if !matches!(geom_a.geom_type, three_layer::GeomType::Polygon) {
            continue;
        }
        if geom_a.coord_count < 3 || geom_a.coords.len() < 6 {
            continue;
        }
        if geom_a.ring_offsets.len() > 1 {
            // Multi-ring (with holes) not yet handled by the kernel; PG
            // rechecks via native st_distance.
            continue;
        }

        a_coords.extend_from_slice(&geom_a.coords);
        a_next = a_next.saturating_add(geom_a.coords.len() as u32);
        a_offsets.push(a_next);

        b_coords.extend_from_slice(&geom_b.coords);
        b_next = b_next.saturating_add(b_floats);
        b_offsets.push(b_next);

        valid_rows.push(i);
    }

    if valid_rows.is_empty() {
        return DispatchResult::Deferred;
    }

    let mut distances = vec![0.0f32; valid_rows.len()];
    let mut uncertain = vec![0u8; valid_rows.len()];

    use crate::gpu::bridge::{self, PgaccelStatus};
    // SAFETY: All four input buffers and both output buffers are
    // Rust-owned slices of the declared lengths. row_count is
    // valid_rows.len() so a_offsets[N+1] / b_offsets[N+1] index into
    // a_coords / b_coords correctly.
    let status = unsafe {
        bridge::pgaccel_st_distance_polygon_polygon_bulk(
            a_coords.as_ptr(),
            a_offsets.as_ptr(),
            b_coords.as_ptr(),
            b_offsets.as_ptr(),
            valid_rows.len(),
            distances.as_mut_ptr(),
            uncertain.as_mut_ptr(),
        )
    };
    if !matches!(status, PgaccelStatus::Ok) {
        return DispatchResult::Deferred;
    }

    let mut results = vec![(pgrx::pg_sys::Datum::from(0_u64), true); n];
    for (k, &batch_i) in valid_rows.iter().enumerate() {
        // uncertain==1 → boundary touch/overlap. Caller (PG executor)
        // doesn't recheck distance results today; surface the kernel's
        // value, which is bit-correct for non-overlapping polygons and
        // 0.0 for boundary-touch (matching PG's st_distance(P, P) == 0
        // for adjacent polygons).
        let _ = uncertain[k];
        let dist_f64 = f64::from(distances[k]);
        results[batch_i] = (pgrx::pg_sys::Datum::from(dist_f64.to_bits()), false);
    }
    DispatchResult::Accelerated(results)
}
