//! GPU spatial dispatch via the three-layer pipeline.

use crate::adapters::extractors::geometry::{
    extract_geometry, extract_point, extract_point_xy_f32,
};
use crate::engine::cost;
use crate::engine::gucs;
use crate::gpu;
use crate::gpu::three_layer;

use super::{DispatchResult, SpatialDispatchOp};

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
            "pg_accel: bulk point_in_polygon GPU kernel failed; refusing non-GPU completion"
        );
    };

    pgrx::debug1!(
        "pg_accel: bulk point_in_polygon: {} points, {}ms",
        point_idx_to_batch.len(),
        elapsed_ms,
    );

    // Apply results. Uncertain rows mean this shape is not fully GPU-covered.
    let mut unclassified_rows: Vec<usize> = Vec::new();

    for (k, &r) in pip_results.iter().enumerate() {
        let batch_idx = point_idx_to_batch[k];
        match r {
            1 => results[batch_idx] = (bool_true, false),
            -1 => results[batch_idx] = (bool_false, false),
            _ => unclassified_rows.push(batch_idx),
        }
    }

    if !unclassified_rows.is_empty() {
        pgrx::error!(
            "pg_accel: bulk point_in_polygon produced {} uncertain rows; refusing non-GPU completion",
            unclassified_rows.len(),
        );
    }

    let _ = (fn_info, is_strict, qual_d, qual_null);
    Some(DispatchResult::Accelerated(results))
}

/// Rows that the pipeline cannot conclusively decide are rejected at runtime.
/// PostgreSQL native planning is the fallback; pg_accel plans must not perform
/// PostGIS predicate evaluation inside the accelerator node.
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
    op: SpatialDispatchOp,
    qual_datums: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)],
    _skip_bbox: bool,
) -> DispatchResult {
    // Single-arg spatial functions (st_area, eventually st_length /
    // st_x / st_y) — qual_datums is empty because there's no constant
    // second argument. Branch off before the two-arg gate.
    if qual_datums.is_empty() {
        match op {
            SpatialDispatchOp::Area => return unsafe { dispatch_gpu_st_area(batch) },
            SpatialDispatchOp::Length => return unsafe { dispatch_gpu_st_length(batch) },
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
    // dispatch_gpu_st_distance for Point × Point pairs through the f64
    // sphere-distance kernel. Other geometry-pair shapes decline this GPU
    // path.
    //
    // Detected here, AFTER geom_b extraction but BEFORE the
    // min_vertices gate (Point has coord_count = 1, which is below
    // the predicate vertex-count threshold).
    if op == SpatialDispatchOp::Distance {
        return unsafe { dispatch_gpu_st_distance(batch, &geom_b, qual_d) };
    }

    // ── Vertex count gate ────────────────────────────────────────
    // GPU overhead is roughly constant (~19ms on M2 Max) regardless of
    // polygon complexity (dominated by geometry deser + seq scan).
    // PG parallel scales linearly with vertex count. Below the threshold,
    // PG parallel is faster — defer to avoid overhead.
    let min_verts = cost::device_limits().gpu_spatial_min_vertices;
    if op != SpatialDispatchOp::DWithin && geom_b.coord_count < min_verts {
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
        // Only exact registry matches for ST_Intersects use the fast path.
        let is_intersects = matches!(op, SpatialDispatchOp::Intersects);

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
    let mut unclassified_rows: Vec<usize> = Vec::new();

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        if let Some(geom) = extract_geometry(datum) {
            geom_idx_to_batch.push(i);
            geoms_a.push(geom);
        } else {
            unclassified_rows.push(i);
        }
    }

    // If no geometries could be extracted, decline the GPU path entirely.
    if geoms_a.is_empty() {
        return DispatchResult::Deferred;
    }

    // Predicate routing.
    //
    // - st_intersects: existing `gpu::spatial_intersects_gpu` cross-product
    //   kernel below.
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
    let predicate = match op {
        SpatialDispatchOp::Contains => Some(three_layer::SpatialPredicate::Contains),
        SpatialDispatchOp::Within => Some(three_layer::SpatialPredicate::Within),
        SpatialDispatchOp::Equals => Some(three_layer::SpatialPredicate::Equals),
        SpatialDispatchOp::Touches => Some(three_layer::SpatialPredicate::Touches),
        SpatialDispatchOp::Crosses => Some(three_layer::SpatialPredicate::Crosses),
        SpatialDispatchOp::Overlaps => Some(three_layer::SpatialPredicate::Overlaps),
        SpatialDispatchOp::Disjoint => Some(three_layer::SpatialPredicate::Disjoint),
        SpatialDispatchOp::DWithin => {
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
        // for every row) and call `spatial_eval`. UNCERTAIN rows are rejected:
        // this is a GPU accelerator, not a PostGIS predicate wrapper.
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
                unclassified_rows.push(geom_idx_to_batch[geom_idx]);
            }
        }

        // SAFETY: same preconditions as dispatch_gpu_spatial — main backend
        // thread, valid fn_info, valid datums.
        return unsafe {
            reject_unclassified_rows(
                batch,
                fn_info,
                is_strict,
                qual_d,
                qual_null,
                results,
                unclassified_rows,
            )
        };
    }

    if op != SpatialDispatchOp::Intersects {
        pgrx::debug1!(
            "pg_accel: dispatch_gpu_spatial: unsupported spatial op {:?}, deferring",
            op,
        );
        return DispatchResult::Deferred;
    }

    // ── ST_Intersects → existing N×1 cross-product kernel via
    // `gpu::spatial_intersects_gpu`. ────────────────────────
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
        pgrx::error!("pg_accel: GPU spatial kernel failed; refusing non-GPU completion");
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

    // UNCERTAIN pairs are not allowed inside a pg_accel plan.
    for &(i, _j) in &uc_pairs {
        let geom_idx = i as usize;
        if geom_idx < geom_idx_to_batch.len() {
            unclassified_rows.push(geom_idx_to_batch[geom_idx]);
        }
    }

    // Runtime CPU completion is forbidden for pg_accel plans.
    // SAFETY: same preconditions as dispatch_gpu_spatial — main backend thread,
    // valid fn_info, valid datums.
    unsafe {
        reject_unclassified_rows(
            batch,
            fn_info,
            is_strict,
            qual_d,
            qual_null,
            results,
            unclassified_rows,
        )
    }
}

/// Reject rows the GPU pipeline marked uncertain or could not classify.
///
/// # Safety
///
/// Must be called on the main backend thread. `fn_info` must be a valid
/// FmgrInfo registered for a 2-arg geometry predicate.
unsafe fn reject_unclassified_rows(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    fn_info: &pgrx::pg_sys::FmgrInfo,
    is_strict: bool,
    qual_d: pgrx::pg_sys::Datum,
    qual_null: bool,
    results: Vec<(pgrx::pg_sys::Datum, bool)>,
    unclassified_rows: Vec<usize>,
) -> DispatchResult {
    let unclassified_total = unclassified_rows.len();
    if unclassified_total > 0 {
        pgrx::error!(
            "pg_accel: spatial GPU path produced {} uncertain/non-extractable rows; refusing non-GPU completion",
            unclassified_total,
        );
    }

    let _ = (batch, fn_info, is_strict, qual_d, qual_null);
    DispatchResult::Accelerated(results)
}

/// Single-arg `st_area(geom)` dispatch: extract one Polygon per row,
/// run the Shoelace SYCL kernel, return one f64 per row.
///
/// CSR layout: build a flat coords buffer + row_offsets in a single pass
/// over the batch. Any non-NULL row outside the GPU-covered shape set makes
/// the whole GPU path ineligible; pg_accel must not return partial NULLs and
/// rely on per-row PostGIS handling.
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
    let null_results = || vec![(pgrx::pg_sys::Datum::from(0_u64), true); n];

    // Build CSR coords + offsets. valid_rows tracks which batch indices fed
    // into the kernel. NULL rows keep native strict semantics; unsupported
    // non-NULL rows decline the whole GPU path.
    let mut coords: Vec<f32> = Vec::new();
    let mut row_offsets: Vec<u32> = Vec::with_capacity(n + 1);
    let mut valid_rows: Vec<usize> = Vec::with_capacity(n);
    let mut next_offset: u32 = 0;
    let mut unsupported_non_null = false;
    row_offsets.push(0);

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        let Some(geom) = extract_geometry(datum) else {
            unsupported_non_null = true;
            continue;
        };
        // Single-ring Polygon only — the kernel doesn't yet handle
        // multipart geometries or polygons with holes.
        if !matches!(geom.geom_type, three_layer::GeomType::Polygon) {
            unsupported_non_null = true;
            continue;
        }
        if geom.coord_count < 3 || geom.coords.len() < 6 {
            unsupported_non_null = true;
            continue;
        }
        // ring_offsets stores per-ring starts; len > 1 indicates
        // multiple rings (outer + holes).
        if geom.ring_offsets.len() > 1 {
            unsupported_non_null = true;
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
    if unsupported_non_null {
        return DispatchResult::Deferred;
    }
    if kernel_row_count == 0 {
        // Nothing to dispatch — every row is NULL.
        return DispatchResult::Accelerated(null_results());
    }

    // Output buffer: one f32 area per kernel row.
    let mut areas = vec![0.0f32; kernel_row_count];

    use crate::gpu::bridge::{self, PgaccelStatus};
    // SAFETY: coords / row_offsets / areas are valid Rust-owned slices
    // of the declared lengths. This dispatch still uses the f32 geometry
    // contract; the typed-geometry FP64 TODO tracks preserving PostGIS f64
    // coordinates through area/length paths.
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
        return DispatchResult::Deferred;
    }

    // Build the output Datum vector. NULL slots stay NULL; valid rows
    // get the GPU-computed area as an fp8 datum (PG `float8` storage
    // is the bit pattern of an fp64 value, so promote fp32 -> fp64).
    let mut results = null_results();
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
/// This dispatch still uses the f32 geometry contract. The C++ kernel has an
/// fp64 branch; the typed-geometry FP64 TODO tracks preserving PostGIS f64
/// coordinates through this path.
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
    let null_results = || vec![(pgrx::pg_sys::Datum::from(0_u64), true); n];

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
    let mut unsupported_non_null = false;
    open_offsets.push(0);

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        let Some(geom) = extract_geometry(datum) else {
            unsupported_non_null = true;
            continue;
        };
        match geom.geom_type {
            three_layer::GeomType::Polygon => {
                if geom.coord_count < 3 || geom.coords.len() < 6 {
                    unsupported_non_null = true;
                    continue;
                }
                if geom.ring_offsets.len() > 1 {
                    // Multi-ring (with holes) needs perimeter sum across all
                    // rings — not yet wired.
                    unsupported_non_null = true;
                    continue;
                }
                closed_coords.extend_from_slice(&geom.coords);
                closed_next = closed_next.saturating_add(geom.coords.len() as u32);
                closed_offsets.push(closed_next);
                closed_rows.push(i);
            }
            three_layer::GeomType::LineString => {
                if geom.coord_count < 2 || geom.coords.len() < 4 {
                    unsupported_non_null = true;
                    continue;
                }
                open_coords.extend_from_slice(&geom.coords);
                open_next = open_next.saturating_add(geom.coords.len() as u32);
                open_offsets.push(open_next);
                open_rows.push(i);
            }
            _ => {
                unsupported_non_null = true;
                continue;
            }
        }
    }

    if unsupported_non_null {
        return DispatchResult::Deferred;
    }
    if closed_rows.is_empty() && open_rows.is_empty() {
        // Nothing to dispatch — every row is NULL.
        return DispatchResult::Accelerated(null_results());
    }

    use crate::gpu::bridge::{self, PgaccelStatus};
    let mut results = null_results();

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
/// existing `pgaccel_sphere_distance_bulk` kernel's fp64 path, which is native
/// on fp64 GPUs and soft-fp64 on Metal. Non-Point non-NULL rows make the whole
/// GPU path ineligible.
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
    geom_b_datum: pgrx::pg_sys::Datum,
) -> DispatchResult {
    let n = batch.len();
    if n == 0 {
        return DispatchResult::Accelerated(Vec::new());
    }
    let null_results = || vec![(pgrx::pg_sys::Datum::from(0_u64), true); n];

    // ── Polygon × Polygon path ─────────────────────────────────────
    // When the constant geom_b is a single-ring Polygon, build a CSR
    // batch of per-row polygon coords and call
    // pgaccel_st_distance_polygon_polygon_bulk (Agent 2A task 3). The
    // kernel returns Euclidean min-vertex-to-edge distance + an uncertainty
    // flag for boundary touch / overlap. Uncertainty is refused under
    // GPU-only execution.
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
    let Some((bx, by)) = extract_point(geom_b_datum) else {
        return DispatchResult::Deferred;
    };

    // Per-row Point extract. Non-Point non-NULL rows make this GPU path
    // ineligible.
    let mut a_xy: Vec<f64> = Vec::with_capacity(n * 2);
    let mut b_xy: Vec<f64> = Vec::with_capacity(n * 2);
    let mut valid_rows: Vec<usize> = Vec::with_capacity(n);
    let mut unsupported_non_null = false;

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        let Some((ax, ay)) = extract_point(datum) else {
            unsupported_non_null = true;
            continue;
        };
        a_xy.push(ax);
        a_xy.push(ay);
        b_xy.push(bx);
        b_xy.push(by);
        valid_rows.push(i);
    }

    if unsupported_non_null {
        return DispatchResult::Deferred;
    }
    if valid_rows.is_empty() {
        return DispatchResult::Accelerated(null_results());
    }

    let mut distances = vec![0.0f64; valid_rows.len()];
    let mut uncertain = vec![0u8; valid_rows.len()];

    use crate::gpu::bridge::{self, PgaccelStatus};
    // SAFETY: input/output slices are Rust-owned of declared lengths.
    let status = unsafe {
        bridge::pgaccel_sphere_distance_bulk(
            a_xy.as_ptr().cast(),
            b_xy.as_ptr().cast(),
            valid_rows.len(),
            true,
            distances.as_mut_ptr().cast(),
            uncertain.as_mut_ptr(),
        )
    };
    if !matches!(status, PgaccelStatus::Ok) {
        return DispatchResult::Deferred;
    }
    if uncertain.iter().any(|&u| u != 0) {
        pgrx::error!(
            "pg_accel: st_distance GPU kernel produced uncertain rows; refusing non-GPU completion"
        );
    }

    let mut results = null_results();
    for (k, &batch_i) in valid_rows.iter().enumerate() {
        results[batch_i] = (pgrx::pg_sys::Datum::from(distances[k].to_bits()), false);
    }
    DispatchResult::Accelerated(results)
}

/// Polygon × Polygon `st_distance` via Agent 2A's
/// `pgaccel_st_distance_polygon_polygon_bulk`. CSR-laid out the same way as
/// `pgaccel_st_area_bulk`: per-row coords concatenated in a flat fp32
/// buffer indexed by `row_offsets[N+1]`. The kernel emits one f32 distance
/// per row plus a `uncertain[i]` flag for boundary touch / overlap; GPU-only
/// dispatch rejects uncertain rows rather than invoking PostGIS recheck.
///
/// Rows with non-Polygon shape, multi-ring polygon, or insufficient
/// vertices make this GPU path ineligible.
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
    let null_results = || vec![(pgrx::pg_sys::Datum::from(0_u64), true); n];

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
    let mut unsupported_non_null = false;
    a_offsets.push(0);
    b_offsets.push(0);

    let b_floats: u32 = u32::try_from(geom_b.coords.len()).unwrap_or(u32::MAX);

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        let Some(geom_a) = extract_geometry(datum) else {
            unsupported_non_null = true;
            continue;
        };
        if !matches!(geom_a.geom_type, three_layer::GeomType::Polygon) {
            unsupported_non_null = true;
            continue;
        }
        if geom_a.coord_count < 3 || geom_a.coords.len() < 6 {
            unsupported_non_null = true;
            continue;
        }
        if geom_a.ring_offsets.len() > 1 {
            // Multi-ring (with holes) not yet handled by the kernel.
            unsupported_non_null = true;
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

    if unsupported_non_null {
        return DispatchResult::Deferred;
    }
    if valid_rows.is_empty() {
        return DispatchResult::Accelerated(null_results());
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
    if uncertain.iter().any(|&u| u != 0) {
        pgrx::error!(
            "pg_accel: polygon st_distance GPU kernel produced uncertain rows; refusing non-GPU completion"
        );
    }

    let mut results = null_results();
    for (k, &batch_i) in valid_rows.iter().enumerate() {
        let dist_f64 = f64::from(distances[k]);
        results[batch_i] = (pgrx::pg_sys::Datum::from(dist_f64.to_bits()), false);
    }
    DispatchResult::Accelerated(results)
}
