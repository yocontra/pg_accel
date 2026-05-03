//! GPU H3 cell dispatch.

use crate::adapters::extractors::geometry::extract_geometry;
use crate::engine::gucs;
use crate::engine::registry;
use crate::gpu;

#[allow(unused_imports)]
use super::{DispatchResult, FcinfoWith2Args};

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

    // ── Variable-output H3 ops ─────────────────────────────────────
    // Each input row expands to a CSR-laid-out list of output cells (or
    // lat/lng coord pairs). The result is wrapped in DispatchResult::
    // AcceleratedVarLen and consumed by the scan executor's per-input-row
    // tuple-emission path. Only the cell-id-output variants are wired
    // here: the boundary / multi_polygon ops emit raw lat/lng arrays
    // that need PostGIS GSERIALIZED encoding which the codebase does not
    // expose today (see adapters/h3.rs module doc + Phase B follow-up).
    match fn_name {
        Some("h3_grid_disk") => {
            return unsafe { dispatch_gpu_h3_var_grid_disk(batch, qual_datum) };
        }
        Some("h3_grid_ring_unsafe") => {
            return unsafe { dispatch_gpu_h3_var_grid_ring_unsafe(batch, qual_datum) };
        }
        Some("h3_cell_to_children") => {
            return unsafe { dispatch_gpu_h3_var_cell_to_children(batch, qual_datum) };
        }
        Some("h3_polyfill" | "h3_cell_to_boundary" | "h3_cells_to_multi_polygon") => {
            // h3_polyfill needs polygon-vertex extraction from per-row
            // geometry datums; h3_cell_to_boundary +
            // h3_cells_to_multi_polygon emit raw lat/lng arrays needing a
            // PostGIS GSERIALIZED encoder that the codebase does not have
            // today. Defer cleanly; do NOT silently substitute another
            // kernel (anti-cheat ban #7).
            pgrx::debug1!(
                "pg_accel: dispatch_gpu_h3: {} kernel exists but PostGIS GSERIALIZED \
                 plumbing / polygon extractor for h3 not yet wired — deferring",
                fn_name.unwrap_or("?"),
            );
            return DispatchResult::Deferred;
        }
        _ => {}
    }

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

        let Some(cell_ids) = gpu_result else {
            pgrx::error!(
                "pg_accel: h3_lat_lng_to_cell GPU kernel failed; refusing CPU fallback (rule 11)"
            );
        };
        let mut results = vec![(pgrx::pg_sys::Datum::from(0), true); batch.len()];
        for (gi, &batch_idx) in valid_indices.iter().enumerate() {
            if gi < cell_ids.len() && cell_ids[gi] != 0 {
                results[batch_idx] = (pgrx::pg_sys::Datum::from(cell_ids[gi] as i64), false);
            }
        }
        return DispatchResult::Accelerated(results);
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
        // 1-arg: cell → i32 base cell index
        Some("h3_get_base_cell") => crate::gpu::h3_get_base_cell_bulk(&cells).map(GpuH3Result::I32),
        // 1-arg: cell → bool valid; mapped via Bool variant for the
        // result-emit loop to construct a PG bool Datum.
        Some("h3_is_valid_cell") => {
            crate::gpu::h3_is_valid_cell_bulk(&cells).map(GpuH3Result::Bool)
        }
        // 1-arg: cell → bool pentagon (Bool variant for PG bool Datum)
        Some("h3_is_pentagon") => crate::gpu::h3_is_pentagon_bulk(&cells).map(GpuH3Result::Bool),
        // 1-arg: cell → bool resolution-class-III (Bool variant for PG bool Datum)
        Some("h3_is_res_class_iii") => {
            crate::gpu::h3_is_res_class_iii_bulk(&cells).map(GpuH3Result::Bool)
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
        // 2-arg: cell + resolution constant → center child cell (u64)
        Some("h3_cell_to_center_child") => {
            let res = qual_datum
                .filter(|(_, is_null)| !is_null)
                .map(|(d, _)| d.value() as i32);
            res.and_then(|child_res| {
                crate::gpu::h3_cell_to_center_child_bulk(&cells, child_res).map(GpuH3Result::U64)
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

    // Unsupported function → defer (input gate, not kernel failure).
    // Supported function + missing qual arg → defer (input gate).
    // Supported function + present args → GPU kernel MUST succeed (rule 11).
    let Some(gpu_res) = gpu_results else {
        match fn_name {
            Some("h3_get_resolution") => {
                pgrx::error!(
                    "pg_accel: h3_get_resolution GPU kernel failed; refusing CPU fallback (rule 11)"
                );
            }
            Some("h3_get_base_cell") => {
                pgrx::error!(
                    "pg_accel: h3_get_base_cell GPU kernel failed; refusing CPU fallback (rule 11)"
                );
            }
            Some("h3_is_valid_cell") => {
                pgrx::error!(
                    "pg_accel: h3_is_valid_cell GPU kernel failed; refusing CPU fallback (rule 11)"
                );
            }
            Some("h3_is_pentagon") => {
                pgrx::error!(
                    "pg_accel: h3_is_pentagon GPU kernel failed; refusing CPU fallback (rule 11)"
                );
            }
            Some("h3_is_res_class_iii") => {
                pgrx::error!(
                    "pg_accel: h3_is_res_class_iii GPU kernel failed; refusing CPU fallback (rule 11)"
                );
            }
            Some("h3_cell_to_parent") => {
                if qual_datum.is_some_and(|(_, n)| !n) {
                    pgrx::error!(
                        "pg_accel: h3_cell_to_parent GPU kernel failed; refusing CPU fallback (rule 11)"
                    );
                }
                return DispatchResult::Deferred;
            }
            Some("h3_cell_to_center_child") => {
                if qual_datum.is_some_and(|(_, n)| !n) {
                    pgrx::error!(
                        "pg_accel: h3_cell_to_center_child GPU kernel failed; refusing CPU fallback (rule 11)"
                    );
                }
                return DispatchResult::Deferred;
            }
            Some("h3_grid_distance") => {
                if qual_datum.is_some_and(|(_, n)| !n) {
                    pgrx::error!(
                        "pg_accel: h3_grid_distance GPU kernel failed; refusing CPU fallback (rule 11)"
                    );
                }
                return DispatchResult::Deferred;
            }
            _ => return DispatchResult::Deferred,
        }
    };

    let mut results = vec![(pgrx::pg_sys::Datum::from(0), true); batch.len()];
    for (gi, &batch_idx) in valid_indices.iter().enumerate() {
        let datum = match &gpu_res {
            GpuH3Result::I32(v) if gi < v.len() => pgrx::pg_sys::Datum::from(v[gi]),
            GpuH3Result::U64(v) if gi < v.len() => pgrx::pg_sys::Datum::from(v[gi] as i64),
            GpuH3Result::Bool(v) if gi < v.len() => pgrx::pg_sys::Datum::from(v[gi] != 0),
            _ => continue,
        };
        results[batch_idx] = (datum, false);
    }
    DispatchResult::Accelerated(results)
}

/// Tagged union for H3 GPU kernel results — some return i32, others u64,
/// h3_is_valid_cell returns bool (carried as u8 0/1 from the kernel).
enum GpuH3Result {
    I32(Vec<i32>),
    U64(Vec<u64>),
    Bool(Vec<u8>),
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
// Variable-output H3 dispatch arms
// ---------------------------------------------------------------------------

/// Extract H3 cells from `batch`, run a 2-pass CSR var-output kernel, and
/// repackage the result as `DispatchResult::AcceleratedVarLen` with one
/// `bigint` Datum per output cell. The `offsets` carry the per-input-row
/// fan-out (`offsets.len() == batch.len() + 1` after re-indexing for NULL
/// rows).
///
/// Caller is responsible for actually computing the per-input fan-out via
/// the size-pass kernel; this helper just maps a `gpu::H3VarOutCells` result
/// back onto the input batch (preserving NULLs as empty ranges) and
/// constructs the Datum vec.
fn build_var_cells_dispatch_result(
    batch_len: usize,
    valid_indices: &[usize],
    raw: &gpu::H3VarOutCells,
) -> DispatchResult {
    // Build per-batch-row offsets. NULL / non-extractable rows get zero
    // fan-out (offsets[i] == offsets[i+1]).
    let mut offsets: Vec<u32> = Vec::with_capacity(batch_len + 1);
    let mut datums: Vec<(pgrx::pg_sys::Datum, bool)> = Vec::with_capacity(raw.cells.len());

    let mut next_offset: u32 = 0;
    offsets.push(0);
    let mut valid_cursor = 0usize;

    for batch_idx in 0..batch_len {
        if valid_cursor < valid_indices.len() && valid_indices[valid_cursor] == batch_idx {
            // This batch row produced cells. Pull its slice from raw.
            let start = raw.offsets[valid_cursor] as usize;
            let end = raw.offsets[valid_cursor + 1] as usize;
            for cell in &raw.cells[start..end] {
                // h3_index is stored as bigint in PG (i64).
                #[allow(clippy::cast_possible_wrap)]
                let cell_i64 = *cell as i64;
                datums.push((pgrx::pg_sys::Datum::from(cell_i64), false));
            }
            next_offset = next_offset.saturating_add((end - start) as u32);
            valid_cursor += 1;
        }
        offsets.push(next_offset);
    }

    DispatchResult::AcceleratedVarLen { offsets, datums }
}

/// Build the parallel `(cells, valid_indices)` from a batch by extracting
/// the i64 cell datum from each non-null entry.
fn extract_h3_cells_from_batch(batch: &[(pgrx::pg_sys::Datum, bool)]) -> (Vec<u64>, Vec<usize>) {
    let mut cells: Vec<u64> = Vec::with_capacity(batch.len());
    let mut valid_indices: Vec<usize> = Vec::with_capacity(batch.len());
    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        let cell = datum.value() as u64;
        if cell != 0 {
            cells.push(cell);
            valid_indices.push(i);
        }
    }
    (cells, valid_indices)
}

/// `h3_grid_disk(cell, k)` — emits all cells within k-ring distance.
///
/// # Safety
///
/// Must be called on the main backend thread; `qual_datum` comes from the
/// 2-arg dispatch interface and is the constant `k`.
unsafe fn dispatch_gpu_h3_var_grid_disk(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
) -> DispatchResult {
    let Some((k_datum, k_null)) = qual_datum else {
        return DispatchResult::Deferred;
    };
    if k_null {
        return DispatchResult::Deferred;
    }
    let k = k_datum.value() as i32;

    let (cells, valid_indices) = extract_h3_cells_from_batch(batch);
    if cells.is_empty() {
        return DispatchResult::Deferred;
    }

    let Some(raw) = gpu::h3_grid_disk_bulk(&cells, k) else {
        pgrx::error!("pg_accel: h3_grid_disk GPU kernel failed; refusing CPU fallback (rule 11)");
    };

    build_var_cells_dispatch_result(batch.len(), &valid_indices, &raw)
}

/// `h3_grid_ring_unsafe(cell, k)` — emits the k-th ring per input cell.
///
/// # Safety
///
/// Same as `dispatch_gpu_h3_var_grid_disk`.
unsafe fn dispatch_gpu_h3_var_grid_ring_unsafe(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
) -> DispatchResult {
    let Some((k_datum, k_null)) = qual_datum else {
        return DispatchResult::Deferred;
    };
    if k_null {
        return DispatchResult::Deferred;
    }
    let k = k_datum.value() as i32;

    let (cells, valid_indices) = extract_h3_cells_from_batch(batch);
    if cells.is_empty() {
        return DispatchResult::Deferred;
    }

    let Some(raw) = gpu::h3_grid_ring_unsafe_bulk(&cells, k) else {
        pgrx::error!(
            "pg_accel: h3_grid_ring_unsafe GPU kernel failed; refusing CPU fallback (rule 11)"
        );
    };

    build_var_cells_dispatch_result(batch.len(), &valid_indices, &raw)
}

/// `h3_cell_to_children(cell, child_res)` — emits child cells at the
/// requested resolution.
///
/// # Safety
///
/// Same as `dispatch_gpu_h3_var_grid_disk`.
unsafe fn dispatch_gpu_h3_var_cell_to_children(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
) -> DispatchResult {
    let Some((res_datum, res_null)) = qual_datum else {
        return DispatchResult::Deferred;
    };
    if res_null {
        return DispatchResult::Deferred;
    }
    let child_res = res_datum.value() as i32;

    let (cells, valid_indices) = extract_h3_cells_from_batch(batch);
    if cells.is_empty() {
        return DispatchResult::Deferred;
    }

    let Some(raw) = gpu::h3_cell_to_children_bulk(&cells, child_res) else {
        pgrx::error!(
            "pg_accel: h3_cell_to_children GPU kernel failed; refusing CPU fallback (rule 11)"
        );
    };

    build_var_cells_dispatch_result(batch.len(), &valid_indices, &raw)
}
