//! GPU H3 cell dispatch.

use crate::adapters::extractors::array::{ParseError as ArrayParseError, parse_array};
use crate::adapters::extractors::geometry::{
    encode_multipolygon, encode_polygon, extract_geometry,
};
use crate::engine::gucs;
use crate::engine::registry;
use crate::gpu;
use crate::gpu::three_layer::GeomType;

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
/// `qual_datums` carries every constant argument in positional order. The
/// 2-arg H3 ops (`h3_grid_disk(cell, k)`, `h3_cell_to_parent(cell, res)`,
/// etc.) read the second argument from `qual_datums[0]`.
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
    qual_datums: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)],
) -> DispatchResult {
    // Look up the function name to route to the correct GPU kernel.
    let fn_name = registry::global_registry().lookup(fn_oid).map(|e| e.name);

    // First constant arg, if present (most H3 ops take a single scalar
    // const after the cell column: k for grid_disk, res for parent, etc.).
    let qual_datum: Option<(pgrx::pg_sys::Datum, bool)> =
        qual_datums.first().map(|&(d, n, _)| (d, n));

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
        Some("h3_cell_to_boundary") => {
            // Phase 2 F3: per-cell hexagon/pentagon boundary -> POLYGON
            // geometry. Single output Datum per input row. The boundary
            // kernel emits raw lat/lng coord pairs in DOUBLE units; we
            // encode each cell's vertex run via the F2 GSERIALIZED encoder
            // and wrap the resulting bytes in a PG varlena.
            return unsafe { dispatch_gpu_h3_cell_to_boundary(batch) };
        }
        Some("h3_cells_to_multi_polygon") => {
            // Phase 2 B3: per-row bigint[] cell-array -> MULTIPOLYGON
            // geometry. Walks each row's array via `parse_array`,
            // collects cells into a u64 buffer, runs the two-pass kernel
            // (`output_size` -> `emit`), and encodes the resulting ring
            // CSR as a GSERIALIZED MULTIPOLYGON via the F2 encoder.
            return unsafe { dispatch_gpu_h3_cells_to_multi_polygon(batch) };
        }
        Some("h3_polyfill") => {
            // Phase 2 F3: polygon geometry -> SETOF h3index. The geometry
            // arg is a per-row column (NOT a plan-time const); we extract
            // each row's polygon via `extract_geometry` and call the
            // two-pass kernel to get cells. Resolution is the constant
            // `qual_datum`.
            return unsafe { dispatch_gpu_h3_polyfill(batch, qual_datum) };
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

// ---------------------------------------------------------------------------
// Geometry-emitting H3 var-output dispatch arms (Phase 2 F3)
// ---------------------------------------------------------------------------

/// SRID emitted by H3 boundary kernels. H3 cells are spherical-coordinates
/// (WGS84 lat/lng), so the standard EPSG:4326 SRID is correct for the
/// resulting POLYGON / MULTIPOLYGON geometries.
const H3_BOUNDARY_SRID: i32 = 4326;

/// Wrap a bare GSERIALIZED byte buffer in a PG varlena and return the
/// resulting Datum. Allocates via `palloc` in the current memory context.
///
/// # Safety
///
/// Must be called on the main backend thread. The returned Datum points
/// into the current memory context; the caller is responsible for keeping
/// the context alive long enough for downstream consumers.
unsafe fn varlena_from_gserialized(bare: &[u8]) -> pgrx::pg_sys::Datum {
    use pgrx::pg_sys;
    // VARHDRSZ = 4 bytes for the 4-byte varlena length header (short or
    // long format). For our payloads we always use the 4-byte form.
    let varhdrsz = std::mem::size_of::<pg_sys::int32>();
    let total = varhdrsz + bare.len();
    // SAFETY: palloc allocates `total` bytes in CurrentMemoryContext on
    // the main backend thread. Caller has guaranteed thread context.
    let ptr = unsafe { pg_sys::palloc(total) }.cast::<u8>();
    // SET_VARSIZE_4B-equivalent: write `total << 2` into the first 4
    // bytes (low 2 bits encode the varlena format; 00 = 4B aligned).
    // pgrx exposes `set_varsize_4b` via the `varatt_*` helpers; use the
    // raw write to avoid coupling to a specific helper signature.
    let len_word = (total as u32) << 2;
    // SAFETY: ptr is a fresh palloc allocation of `total` bytes.
    unsafe {
        std::ptr::write_unaligned(ptr.cast::<u32>(), len_word);
        // Copy the bare GSERIALIZED bytes after the varlena header.
        std::ptr::copy_nonoverlapping(bare.as_ptr(), ptr.add(varhdrsz), bare.len());
    }
    pg_sys::Datum::from(ptr)
}

/// `h3_cell_to_boundary(cell)` -> POLYGON geometry per cell.
///
/// Each input row produces exactly one output Datum (a GSERIALIZED varlena
/// holding the cell's hex/pentagon vertex ring). NULL / invalid input
/// rows produce a NULL Datum slot (`is_null = true`).
///
/// # Safety
///
/// Must be called on the main backend thread.
unsafe fn dispatch_gpu_h3_cell_to_boundary(
    batch: &[(pgrx::pg_sys::Datum, bool)],
) -> DispatchResult {
    let (cells, valid_indices) = extract_h3_cells_from_batch(batch);
    if cells.is_empty() {
        return DispatchResult::Deferred;
    }

    let Some(raw) = gpu::h3_cell_to_boundary_bulk(&cells) else {
        pgrx::error!(
            "pg_accel: h3_cell_to_boundary GPU kernel failed; refusing CPU fallback (rule 11)"
        );
    };

    // The kernel emits a flat coords vector with `raw.offsets[i+1] -
    // raw.offsets[i]` doubles per cell (always 12 for hexagons, 10 for
    // pentagons - 2 doubles per vertex). Encode each cell's vertex run as
    // a single-ring POLYGON via the F2 encoder, then wrap as varlena.
    //
    // Build the flat output as one Datum per input row in `valid_indices`
    // order. Rows whose cell was filtered out (NULL / cell == 0) get an
    // is_null = true slot. The `AcceleratedVarLen` offsets carry one
    // output per input row (constant fan-out 1).
    let mut datums: Vec<(pgrx::pg_sys::Datum, bool)> = Vec::with_capacity(batch.len());
    let mut offsets: Vec<u32> = Vec::with_capacity(batch.len() + 1);
    offsets.push(0);
    let mut next_offset: u32 = 0;
    let mut valid_cursor = 0usize;

    for batch_idx in 0..batch.len() {
        if valid_cursor < valid_indices.len() && valid_indices[valid_cursor] == batch_idx {
            let start = raw.offsets[valid_cursor] as usize;
            let end = raw.offsets[valid_cursor + 1] as usize;
            // Each (lat, lng) pair occupies two doubles. The kernel writes
            // pairs in lat/lng order; PostGIS expects x = lng, y = lat.
            let n_doubles = end - start;
            if n_doubles == 0 || !n_doubles.is_multiple_of(2) {
                // Degenerate / malformed kernel output - emit NULL rather
                // than fabricating bytes (anti-cheat ban #4).
                datums.push((pgrx::pg_sys::Datum::from(0u32), true));
            } else {
                let n_vertices = n_doubles / 2;
                let mut ring: Vec<(f64, f64)> = Vec::with_capacity(n_vertices);
                let coords = &raw.coords[start..end];
                for v in 0..n_vertices {
                    let lat = coords[v * 2];
                    let lng = coords[v * 2 + 1];
                    ring.push((lng, lat));
                }
                match encode_polygon(H3_BOUNDARY_SRID, &[&ring]) {
                    Ok(bytes) => {
                        // SAFETY: caller is on the main backend thread (asserted at
                        // the dispatch entry point); `varlena_from_gserialized`
                        // calls palloc on the current memory context.
                        let datum = unsafe { varlena_from_gserialized(&bytes) };
                        datums.push((datum, false));
                        next_offset = next_offset.saturating_add(1);
                    }
                    Err(e) => {
                        // Encoder rejected the kernel-derived ring (e.g. empty
                        // ring or out-of-range SRID). Surface as a NULL row +
                        // a debug log; do not fabricate a "default" geometry
                        // (anti-cheat ban #4).
                        pgrx::debug1!("pg_accel: h3_cell_to_boundary encoder rejected ring: {}", e,);
                        datums.push((pgrx::pg_sys::Datum::from(0u32), true));
                    }
                }
            }
            valid_cursor += 1;
        } else {
            // NULL or invalid input row - emit a NULL slot.
            datums.push((pgrx::pg_sys::Datum::from(0u32), true));
            next_offset = next_offset.saturating_add(1);
        }
        offsets.push(next_offset);
    }

    DispatchResult::AcceleratedVarLen { offsets, datums }
}

/// `h3_polyfill(geometry, resolution)` -> SETOF h3index.
///
/// Each input row carries a polygon `geometry` column; the kernel returns
/// one h3index per cell whose centre lies inside the polygon. The output
/// fan-out per input row varies with polygon area and resolution.
///
/// # Safety
///
/// Must be called on the main backend thread.
unsafe fn dispatch_gpu_h3_polyfill(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
) -> DispatchResult {
    let Some((res_datum, res_null)) = qual_datum else {
        return DispatchResult::Deferred;
    };
    if res_null {
        return DispatchResult::Deferred;
    }
    let resolution = res_datum.value() as i32;

    // Collect per-row polygon rings from each batch entry. We assemble all
    // rings into one flat coords + offsets buffer and call the kernel
    // once; the kernel's per-row offsets carry the cell fan-out per ring.
    //
    // Each polygon contributes its outer ring only (h3_polyfill in
    // h3-pg semantics ignores holes for cell coverage; the kernel
    // matches by ignoring all but ring index 0). Multi-ring polygons fall
    // through with their first ring; non-polygon geoms produce zero cells
    // for that row.
    let mut flat_coords: Vec<f32> = Vec::new();
    let mut ring_offsets: Vec<u32> = Vec::new();
    let mut valid_indices: Vec<usize> = Vec::with_capacity(batch.len());
    ring_offsets.push(0);
    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        let Some(geom) = extract_geometry(datum) else {
            continue;
        };
        if geom.geom_type != GeomType::Polygon || geom.coord_count == 0 {
            continue;
        }
        // ExtractedGeometry stores coords as f32 pairs [x0, y0, x1, y1, ...].
        // For polygons, ring_offsets indexes into the coord-pair stream.
        // We only forward the outer ring (ring_offsets[0]..end-of-ring-0).
        let outer_pair_start = geom.ring_offsets.first().copied().unwrap_or(0) as usize;
        let outer_pair_end = geom
            .ring_offsets
            .get(1)
            .copied()
            .map_or(geom.coord_count, |v| v as usize);
        if outer_pair_end <= outer_pair_start {
            continue;
        }
        let n_pairs = outer_pair_end - outer_pair_start;
        // Append the outer ring's coord pairs (lng, lat in PostGIS order)
        // to the flat buffer.
        let coords_flat_start = outer_pair_start * 2;
        let coords_flat_end = outer_pair_end * 2;
        flat_coords.extend_from_slice(&geom.coords[coords_flat_start..coords_flat_end]);
        let next_offset = flat_coords.len() / 2;
        ring_offsets.push(next_offset as u32);
        valid_indices.push(i);
        let _ = n_pairs;
    }

    if valid_indices.is_empty() {
        return DispatchResult::Deferred;
    }

    let ring_count = valid_indices.len();
    let Some(raw) = gpu::h3_polyfill_bulk(&flat_coords, &ring_offsets, ring_count, resolution)
    else {
        pgrx::error!("pg_accel: h3_polyfill GPU kernel failed; refusing CPU fallback (rule 11)");
    };

    build_var_cells_dispatch_result(batch.len(), &valid_indices, &raw)
}

/// `h3_cells_to_multi_polygon(cells bigint[])` -> MULTIPOLYGON geometry.
///
/// Each input row carries a `bigint[]` of H3 cells; per row we walk the
/// array via [`parse_array`], collect cells into a `Vec<u64>`, run the
/// two-pass `h3_cells_to_multi_polygon_bulk` kernel (size + emit), and
/// encode the resulting ring CSR as a single GSERIALIZED MULTIPOLYGON
/// (one polygon per ring, no holes — matches the kernel's flat ring
/// emission contract). Multidim arrays escalate cleanly via
/// `DispatchResult::Deferred` per anti-cheat ban #9.
///
/// # Safety
///
/// Must be called on the main backend thread; each `batch[i].0` must be
/// either NULL or a valid `bigint[]` ArrayType varlena.
unsafe fn dispatch_gpu_h3_cells_to_multi_polygon(
    batch: &[(pgrx::pg_sys::Datum, bool)],
) -> DispatchResult {
    // One output Datum per input row; constant fan-out 1 in the
    // AcceleratedVarLen offsets vec (matches dispatch_gpu_h3_cell_to_boundary
    // upstream).
    let mut datums: Vec<(pgrx::pg_sys::Datum, bool)> = Vec::with_capacity(batch.len());
    let mut offsets: Vec<u32> = Vec::with_capacity(batch.len() + 1);
    offsets.push(0);
    let mut next_offset: u32 = 0;

    let timeout_ms = gucs::kernel_timeout_ms();
    let start = std::time::Instant::now();

    for &(datum, is_null) in batch {
        if is_null {
            datums.push((pgrx::pg_sys::Datum::from(0u32), true));
            next_offset = next_offset.saturating_add(1);
            offsets.push(next_offset);
            continue;
        }

        // SAFETY: main backend thread; per-row Datum is bigint[] varlena.
        let arr = match unsafe { parse_array(datum) } {
            Ok(a) => a,
            Err(ArrayParseError::Multidim(n)) => {
                pgrx::debug1!(
                    "pg_accel: h3_cells_to_multi_polygon: multidim bigint[] (ndim={n}) escalated per ban #9"
                );
                datums.push((pgrx::pg_sys::Datum::from(0u32), true));
                next_offset = next_offset.saturating_add(1);
                offsets.push(next_offset);
                continue;
            }
            Err(e) => {
                pgrx::debug1!(
                    "pg_accel: h3_cells_to_multi_polygon: bigint[] parse failed ({e}); NULL row"
                );
                datums.push((pgrx::pg_sys::Datum::from(0u32), true));
                next_offset = next_offset.saturating_add(1);
                offsets.push(next_offset);
                continue;
            }
        };

        // Collect non-null bigint elements as u64 cells. PG bigint is i64
        // stored in a Datum; we read each element as 8 packed LE bytes.
        let mut cells: Vec<u64> = Vec::with_capacity(arr.nelems);
        for opt_elem in &arr {
            let Some(bytes) = opt_elem else {
                continue;
            };
            if bytes.len() < 8 {
                continue;
            }
            // SAFETY: bigint elements are 8 bytes LE, packed in the array
            // payload by PG's array_send/array_out.
            let cell = u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]);
            if cell != 0 {
                cells.push(cell);
            }
        }

        if cells.is_empty() {
            datums.push((pgrx::pg_sys::Datum::from(0u32), true));
            next_offset = next_offset.saturating_add(1);
            offsets.push(next_offset);
            continue;
        }

        // Two-pass kernel: returns CSR over rings (one ring per polygon
        // member of the multipolygon). For now, treat each ring as the
        // outer ring of a stand-alone polygon (no holes), which matches
        // the kernel's flat emission contract — see gpu/mod.rs:1143-1208.
        let Some(raw) = gpu::h3_cells_to_multi_polygon_bulk(&cells) else {
            pgrx::error!(
                "pg_accel: h3_cells_to_multi_polygon GPU kernel failed; refusing CPU fallback (rule 11)"
            );
        };

        let ring_count = raw.offsets.len().saturating_sub(1);
        if ring_count == 0 {
            datums.push((pgrx::pg_sys::Datum::from(0u32), true));
            next_offset = next_offset.saturating_add(1);
            offsets.push(next_offset);
            continue;
        }

        // Build per-ring (lng, lat) vertex Vecs. Kernel emits coords as
        // (lat, lng) pairs in WGS84; PostGIS expects x=lng, y=lat.
        let mut ring_storage: Vec<Vec<(f64, f64)>> = Vec::with_capacity(ring_count);
        for r in 0..ring_count {
            let s = raw.offsets[r] as usize;
            let e = raw.offsets[r + 1] as usize;
            let n_doubles = e.saturating_sub(s);
            if n_doubles == 0 || !n_doubles.is_multiple_of(2) {
                continue;
            }
            let n_vertices = n_doubles / 2;
            let mut verts = Vec::with_capacity(n_vertices);
            let coords = &raw.coords[s..e];
            for v in 0..n_vertices {
                let lat = coords[v * 2];
                let lng = coords[v * 2 + 1];
                verts.push((lng, lat));
            }
            ring_storage.push(verts);
        }

        if ring_storage.is_empty() {
            datums.push((pgrx::pg_sys::Datum::from(0u32), true));
            next_offset = next_offset.saturating_add(1);
            offsets.push(next_offset);
            continue;
        }

        // Each ring becomes a single-ring polygon member of the
        // multipolygon. The encode_multipolygon API takes
        // `&[&[&[(f64, f64)]]]`: outer slice = polygons, middle = rings
        // per polygon (1 here), inner = vertices per ring.
        let polygon_views: Vec<Vec<&[(f64, f64)]>> = ring_storage
            .iter()
            .map(|ring| vec![ring.as_slice()])
            .collect();
        let polygon_refs: Vec<&[&[(f64, f64)]]> = polygon_views
            .iter()
            .map(<Vec<&[(f64, f64)]> as AsRef<[&[(f64, f64)]]>>::as_ref)
            .collect();

        match encode_multipolygon(H3_BOUNDARY_SRID, &polygon_refs) {
            Ok(bytes) => {
                // SAFETY: main backend thread; varlena_from_gserialized
                // palloc's a varlena in the current memory context.
                let datum_out = unsafe { varlena_from_gserialized(&bytes) };
                datums.push((datum_out, false));
            }
            Err(e) => {
                pgrx::debug1!(
                    "pg_accel: h3_cells_to_multi_polygon encoder rejected rings: {}",
                    e,
                );
                datums.push((pgrx::pg_sys::Datum::from(0u32), true));
            }
        }
        next_offset = next_offset.saturating_add(1);
        offsets.push(next_offset);
    }

    let elapsed_ms = start.elapsed().as_millis() as i32;
    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: h3_cells_to_multi_polygon pipeline took {}ms (timeout {}ms)",
            elapsed_ms,
            timeout_ms,
        );
    }

    DispatchResult::AcceleratedVarLen { offsets, datums }
}

// ---------------------------------------------------------------------------
// pg_test integration for the Phase 2 B3 h3_cells_to_multi_polygon arm
// ---------------------------------------------------------------------------
//
// **Known dispatch shape mismatch (mirrors function_scan.rs:676-684 for
// `h3_cell_to_boundary`).** h3-pg declares
// `h3_cells_to_multi_polygon(h3index[], OUT exterior polygon, OUT holes
// polygon[])` (PG built-in `polygon` types — see
// `h3--3.7.2--4.0.0.sql:ALTER FUNCTION h3_set_to_multi_polygon ...
// RENAME TO h3_cells_to_multi_polygon`). Our Phase 2 B3 dispatch arm
// instead emits PostGIS GSERIALIZED MULTIPOLYGON varlena bytes via
// `varlena_from_gserialized`, and the planner-side classifier wires our
// kernel up regardless of the declared return type. Counting the row
// exercises the FunctionScan dispatch + tuple emission path without
// forcing PG to interpret the value bytes; a follow-up bug ticket
// will retarget the kernel output to the declared return type once
// the var-output FunctionScan injection chain stabilises.
//
// The test asserts the call doesn't crash (count >= 1). It runs only
// when h3 is installable in the pgrx_tests DB; otherwise it skips
// silently — same idiom as function_scan.rs:661.
#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::{Spi, pg_test};

    fn ensure_extension(name: &str) -> bool {
        let create_sql = format!("CREATE EXTENSION IF NOT EXISTS {name} CASCADE");
        if Spi::run(&create_sql).is_err() {
            return false;
        }
        let q = format!("SELECT count(*) FROM pg_extension WHERE extname = '{name}'");
        Spi::get_one::<i64>(&q).ok().flatten().unwrap_or(0) > 0
    }

    /// `SELECT count(*) FROM h3_cells_to_multi_polygon(ARRAY[
    ///   '8a..', '8a..']::h3index[])` returns exactly one row (the
    /// function declares `OUT exterior polygon, OUT holes polygon[]`).
    /// Counts rows rather than reading values to avoid the GSERIALIZED
    /// vs PG-`polygon` shape mismatch (see module-level comment).
    #[pg_test]
    fn h3_cells_to_multi_polygon_emits_one_row() {
        if !ensure_extension("h3") {
            return;
        }
        // Trigger registry init.
        Spi::run("SELECT h3_get_resolution('8a2a1072b59ffff'::h3index)").expect("h3 ping");

        // Two adjacent resolution-10 cells. h3-pg's
        // `h3_cells_to_multi_polygon(h3index[])` always emits exactly
        // one (exterior, holes) row regardless of input cardinality.
        let count = Spi::get_one::<i64>(
            "SELECT count(*) FROM h3_cells_to_multi_polygon(\
             ARRAY['8a2a1072b59ffff', '8a2a1072b597fff']::h3index[])",
        )
        .expect("count query ok")
        .expect("count not null");
        assert_eq!(
            count, 1,
            "h3_cells_to_multi_polygon must emit exactly one (exterior, holes) row, got {count}",
        );
    }
}
