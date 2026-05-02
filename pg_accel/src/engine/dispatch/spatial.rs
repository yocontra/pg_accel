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
    // Single-arg spatial functions (st_area, eventually st_length /
    // st_x / st_y) — qual_datum is always None because there's no
    // constant second argument. Branch off before the two-arg gate.
    if qual_datum.is_none() {
        let fn_name = registry::global_registry()
            .lookup(fn_info.fn_oid)
            .map(|e| e.name);
        if fn_name == Some("st_area") {
            return unsafe { dispatch_gpu_st_area(batch) };
        }
        // Other single-arg names not yet wired — defer.
        pgrx::debug1!("pg_accel: dispatch_gpu_spatial: no qual_datum, deferring");
        return DispatchResult::Deferred;
    }
    // We know qual_datum is Some here (the None branch returned above).
    let Some((qual_d, qual_null)) = qual_datum else {
        unreachable!("qual_datum is Some after the None branch returned above")
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
