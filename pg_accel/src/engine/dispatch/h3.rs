//! GPU H3 cell dispatch.

use crate::adapters::extractors::geometry::extract_geometry;
use crate::engine::gucs;
use crate::engine::registry;
use crate::gpu;

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
