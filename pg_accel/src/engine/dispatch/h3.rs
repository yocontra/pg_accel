//! GPU H3 cell dispatch.

use crate::adapters::extractors::array::{ParseError as ArrayParseError, parse_array};
use crate::adapters::extractors::geometry::{encode_pg_polygon, extract_geometry};
use crate::engine::gucs;
use crate::gpu;
use crate::gpu::three_layer::GeomType;

use super::spatial::const_datum_as_i64;
#[allow(unused_imports)]
use super::{DispatchResult, H3DispatchOp};

/// Decode an integer resolution / k / step constant into an `i32`, honouring
/// the constant's declared type OID (int2/int4/int8/float). Returns `None`
/// (caller Defers) for NULL, unsupported types, or values outside `i32`.
fn const_i32_arg(qual: Option<(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)>) -> Option<i32> {
    let (datum, is_null, typid) = qual?;
    let v = const_datum_as_i64(datum, is_null, typid)?;
    i32::try_from(v).ok()
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
    op: H3DispatchOp,
    qual_datums: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)],
) -> DispatchResult {
    // First constant arg, if present (most H3 ops take a single scalar
    // const after the cell column: k for grid_disk, res for parent, etc.).
    // The type OID is retained so integer args are decoded by their declared
    // type rather than blindly truncated from the raw Datum bits.
    let qual_datum: Option<(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)> =
        qual_datums.first().copied();

    // ── Variable-output H3 ops ─────────────────────────────────────
    // Each input row expands to a CSR-laid-out list of output cells (or
    // lat/lng coord pairs). The result is wrapped in DispatchResult::
    // AcceleratedVarLen and consumed by the scan executor's per-input-row
    // tuple-emission path. Only the cell-id-output variants are wired
    // here: the boundary / multi_polygon ops emit raw lat/lng arrays
    // that need PostGIS GSERIALIZED encoding which the codebase does not
    // expose today (see adapters/h3.rs module doc + Phase B follow-up).
    match op {
        H3DispatchOp::GridDisk => {
            return unsafe { dispatch_gpu_h3_var_grid_disk(batch, qual_datum) };
        }
        H3DispatchOp::GridRingUnsafe => {
            return unsafe { dispatch_gpu_h3_var_grid_ring_unsafe(batch, qual_datum) };
        }
        H3DispatchOp::CellToChildren => {
            return unsafe { dispatch_gpu_h3_var_cell_to_children(batch, qual_datum) };
        }
        H3DispatchOp::CellToBoundary => {
            // Phase 2 F3: per-cell hexagon/pentagon boundary -> POLYGON
            // geometry. Single output Datum per input row. The boundary
            // kernel emits raw lat/lng coord pairs in DOUBLE units; we
            // encode each cell's vertex run via the F2 GSERIALIZED encoder
            // and wrap the resulting bytes in a PG varlena.
            return unsafe { dispatch_gpu_h3_cell_to_boundary(batch) };
        }
        H3DispatchOp::CellsToMultiPolygon => {
            // Phase 2 B3: per-row bigint[] cell-array -> MULTIPOLYGON
            // geometry. Walks each row's array via `parse_array`,
            // collects cells into a u64 buffer, runs the two-pass kernel
            // (`output_size` -> `emit`), and encodes the resulting ring
            // CSR as a GSERIALIZED MULTIPOLYGON via the F2 encoder.
            return unsafe { dispatch_gpu_h3_cells_to_multi_polygon(batch) };
        }
        H3DispatchOp::Polyfill => {
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
    if op == H3DispatchOp::LatLngToCell {
        let Some(res) = const_i32_arg(qual_datum) else {
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
        // Skip the 0 sentinel (NULL/invalid placeholder). This is NOT a full
        // H3 validity check — real cell validity (mode bits, reserved high
        // bit, digit range) is enforced inside the GPU kernel; here we only
        // drop obvious non-cells so they surface as per-row NULL outputs.
        if cell != 0 {
            cells.push(cell);
            valid_indices.push(i);
        }
    }

    // Only a whole batch with nothing to accelerate is an input gate. A batch
    // that is merely sparse (many NULL/zero cells) is still valid input: per
    // the dispatch contract (dispatch/mod.rs:265-270) an executor-time
    // Deferred is a contract error, so we must produce per-row NULL outputs
    // for the invalid rows rather than declining the whole batch. The result
    // vector below is pre-filled with NULLs and only valid rows are written,
    // so sparse batches are handled correctly without a hardcoded ratio gate
    // (which also violated rule #10).
    if cells.is_empty() {
        return DispatchResult::Deferred;
    }

    // Route to the correct GPU kernel based on function name.
    //
    // This match is deliberately wider than the normal planner registry in
    // `adapters/h3.rs`. Cheap scalar H3 kernels are kept here so a future
    // fused GPU pipeline can reuse them, but adapter registration remains the
    // admission gate for standalone PostgreSQL plans.
    let gpu_results: Option<GpuH3Result> = match op {
        // 1-arg: cell → i32 resolution
        H3DispatchOp::GetResolution => {
            crate::gpu::h3_get_resolution_bulk(&cells).map(GpuH3Result::I32)
        }
        // 1-arg: cell → i32 base cell index
        H3DispatchOp::GetBaseCell => {
            crate::gpu::h3_get_base_cell_bulk(&cells).map(GpuH3Result::I32)
        }
        // 1-arg: cell → bool valid; mapped via Bool variant for the
        // result-emit loop to construct a PG bool Datum.
        H3DispatchOp::IsValidCell => {
            crate::gpu::h3_is_valid_cell_bulk(&cells).map(GpuH3Result::Bool)
        }
        // 1-arg: cell → bool pentagon (Bool variant for PG bool Datum)
        H3DispatchOp::IsPentagon => crate::gpu::h3_is_pentagon_bulk(&cells).map(GpuH3Result::Bool),
        // 1-arg: cell → bool resolution-class-III (Bool variant for PG bool Datum)
        H3DispatchOp::IsResClassIii => {
            crate::gpu::h3_is_res_class_iii_bulk(&cells).map(GpuH3Result::Bool)
        }
        // 2-arg: cell + resolution constant → parent cell (u64)
        H3DispatchOp::CellToParent => const_i32_arg(qual_datum).and_then(|parent_res| {
            crate::gpu::h3_cell_to_parent_bulk(&cells, parent_res).map(GpuH3Result::U64)
        }),
        // 2-arg: cell + resolution constant → center child cell (u64)
        H3DispatchOp::CellToCenterChild => const_i32_arg(qual_datum).and_then(|child_res| {
            crate::gpu::h3_cell_to_center_child_bulk(&cells, child_res).map(GpuH3Result::U64)
        }),
        // 2-arg: cell_a + cell_b constant → distance (i32). The second cell is
        // an h3index (8-byte pass-by-value); its raw Datum bits ARE the cell,
        // so read them directly rather than through the integer decoder (which
        // only knows the builtin int/float OIDs, not the h3index type OID).
        H3DispatchOp::GridDistance => {
            let other_cell = qual_datum
                .filter(|(_, is_null, _)| !is_null)
                .map(|(d, _, _)| d.value() as u64);
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
        match op {
            H3DispatchOp::GetResolution => {
                pgrx::error!(
                    "pg_accel: h3_get_resolution GPU kernel failed; refusing CPU fallback (rule 11)"
                );
            }
            H3DispatchOp::GetBaseCell => {
                pgrx::error!(
                    "pg_accel: h3_get_base_cell GPU kernel failed; refusing CPU fallback (rule 11)"
                );
            }
            H3DispatchOp::IsValidCell => {
                pgrx::error!(
                    "pg_accel: h3_is_valid_cell GPU kernel failed; refusing CPU fallback (rule 11)"
                );
            }
            H3DispatchOp::IsPentagon => {
                pgrx::error!(
                    "pg_accel: h3_is_pentagon GPU kernel failed; refusing CPU fallback (rule 11)"
                );
            }
            H3DispatchOp::IsResClassIii => {
                pgrx::error!(
                    "pg_accel: h3_is_res_class_iii GPU kernel failed; refusing CPU fallback (rule 11)"
                );
            }
            H3DispatchOp::CellToParent => {
                if qual_datum.is_some_and(|(_, n, _)| !n) {
                    pgrx::error!(
                        "pg_accel: h3_cell_to_parent GPU kernel failed; refusing CPU fallback (rule 11)"
                    );
                }
                return DispatchResult::Deferred;
            }
            H3DispatchOp::CellToCenterChild => {
                if qual_datum.is_some_and(|(_, n, _)| !n) {
                    pgrx::error!(
                        "pg_accel: h3_cell_to_center_child GPU kernel failed; refusing CPU fallback (rule 11)"
                    );
                }
                return DispatchResult::Deferred;
            }
            H3DispatchOp::GridDistance => {
                if qual_datum.is_some_and(|(_, n, _)| !n) {
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
    qual_datum: Option<(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)>,
) -> DispatchResult {
    let Some(k) = const_i32_arg(qual_datum) else {
        return DispatchResult::Deferred;
    };

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
    qual_datum: Option<(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)>,
) -> DispatchResult {
    let Some(k) = const_i32_arg(qual_datum) else {
        return DispatchResult::Deferred;
    };

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
    qual_datum: Option<(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)>,
) -> DispatchResult {
    let Some(child_res) = const_i32_arg(qual_datum) else {
        return DispatchResult::Deferred;
    };

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
///
/// **Note:** PG's built-in `polygon` type does not carry an SRID
/// (POLYGON struct is `npts + bbox + Point[]` with no SRID slot — see
/// utils/geo_decls.h:151-157). The active encoders
/// (`encode_pg_polygon` / `encode_pg_polygon_array`) ignore the SRID.
#[allow(dead_code)]
const H3_BOUNDARY_SRID: i32 = 4326;

/// Wrap a bare varlena byte buffer in a PG varlena (4-byte header) and
/// return the resulting Datum. Allocates via `palloc` in the current
/// memory context.
///
/// # Safety
///
/// Must be called on the main backend thread. The returned Datum points
/// into the current memory context; the caller is responsible for keeping
/// the context alive long enough for downstream consumers.
unsafe fn varlena_from_bare_bytes(bare: &[u8]) -> pgrx::pg_sys::Datum {
    use pgrx::pg_sys;
    // VARHDRSZ = 4 bytes for the 4-byte varlena length header.
    let varhdrsz = std::mem::size_of::<pg_sys::int32>();
    let total = varhdrsz + bare.len();
    // SAFETY: palloc allocates `total` bytes in CurrentMemoryContext on
    // the main backend thread. Caller has guaranteed thread context.
    let ptr = unsafe { pg_sys::palloc(total) }.cast::<u8>();
    // SET_VARSIZE_4B-equivalent: write `total << 2` into the first 4
    // bytes (low 2 bits encode the varlena format; 00 = 4B aligned).
    let len_word = (total as u32) << 2;
    // SAFETY: ptr is a fresh palloc allocation of `total` bytes.
    unsafe {
        std::ptr::write_unaligned(ptr.cast::<u32>(), len_word);
        // Copy the bare bytes after the varlena header.
        std::ptr::copy_nonoverlapping(bare.as_ptr(), ptr.add(varhdrsz), bare.len());
    }
    pg_sys::Datum::from(ptr)
}

/// Build a `polygon[]` (POLYGONARRAYOID = 1027) ArrayType varlena Datum
/// from a slice of bare PG `polygon` bodies (each body matches the
/// post-VARHDRSZ payload produced by [`encode_pg_polygon`]).
///
/// Each body is first wrapped via [`varlena_from_bare_bytes`] into a
/// proper `polygon` Datum, then the slice of Datums is handed to PG's
/// `construct_array` (`src/backend/utils/adt/arrayfuncs.c`). Letting PG
/// build the array header guarantees the on-disk layout matches what
/// `heap_form_tuple` and downstream readers (npoints, array_out, etc.)
/// expect — including the inter-element MAXALIGN padding that earlier
/// hand-rolled encoders got wrong on multi-element inputs (root cause of
/// the multi-cell `h3_cells_to_multi_polygon` SIGABRT).
///
/// `polygon` type metadata: `typlen = -1` (varlena), `typbyval = false`,
/// `typalign = 'd'` (TYPALIGN_DOUBLE) per `pg_type.dat`. An empty input
/// slice produces a valid empty 1-D array via `construct_array(_, 0, ...)`.
///
/// # Safety
///
/// Must be called on the main backend thread. All allocations land in
/// `CurrentMemoryContext`.
unsafe fn build_polygon_array_datum(bodies: &[Vec<u8>]) -> pgrx::pg_sys::Datum {
    use pgrx::pg_sys;

    // Pre-build the per-element polygon varlena Datums. construct_array
    // copies element bytes into its own allocation, so it's safe for these
    // intermediate pallocs to live in the same CurrentMemoryContext.
    let mut elem_datums: Vec<pg_sys::Datum> = Vec::with_capacity(bodies.len());
    for body in bodies {
        // SAFETY: caller guarantees main backend thread.
        let d = unsafe { varlena_from_bare_bytes(body) };
        elem_datums.push(d);
    }

    // construct_array(elems, nelems, elmtype, elmlen, elmbyval, elmalign)
    // For polygon: elmlen = -1 (varlena), elmbyval = false, elmalign = 'd'.
    // Cast c_uint::MAX-safe: polygon[] holes are kernel-bounded ring counts
    // (typically < a few hundred), well under i32::MAX. nelems == 0 is
    // handled inside construct_md_array — it returns a valid empty array
    // via construct_empty_array(elmtype).
    let nelems = i32::try_from(elem_datums.len()).unwrap_or(i32::MAX);
    // SAFETY: construct_array reads `nelems` Datums from the pointer and
    // copies their varlena bytes; we have ownership of elem_datums for the
    // duration of the call. The function is callable on the main backend
    // thread and palloc's the result in CurrentMemoryContext.
    let arr_ptr = unsafe {
        pg_sys::construct_array(
            elem_datums.as_mut_ptr(),
            nelems,
            pg_sys::POLYGONOID,
            -1,
            false,
            pg_sys::TYPALIGN_DOUBLE as ::core::ffi::c_char,
        )
    };
    pg_sys::Datum::from(arr_ptr)
}

/// `h3_cell_to_boundary(cell)` -> PG built-in `polygon` per cell.
///
/// Each input row produces exactly one output Datum (a `polygon` varlena
/// holding the cell's hex/pentagon vertex ring per
/// utils/geo_decls.h:151-157). NULL / invalid input rows produce a NULL
/// Datum slot (`is_null = true`).
///
/// **Output type:** PG built-in `polygon` (OID 604), matching h3-pg's
/// declaration `h3_cell_to_boundary(cell h3index, ...) RETURNS polygon`
/// (h3--3.7.2--4.0.0.sql:47). Earlier revisions emitted PostGIS
/// GSERIALIZED bytes which crashed `npoints()` and other polygon
/// readers because the byte layout doesn't match.
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
    // a PG built-in `polygon` via `encode_pg_polygon`, then wrap as
    // varlena.
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
            // pairs in lat/lng order; PG `polygon` expects (x, y) where x
            // is longitude and y is latitude (matches PostGIS / OGC
            // convention; PG itself is type-agnostic about coordinate
            // semantics but every consumer treats the first slot as x).
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
                // PG `polygon` is logically closed by type — first vertex
                // implicitly equals last. The h3 boundary kernel emits the
                // distinct vertex sequence already (no duplicate closing
                // point per h3 docs), so we pass it directly. If a closing
                // duplicate ever appears, drop it here to keep
                // `npoints(...)` honest.
                if ring.len() >= 2 && ring[0] == ring[ring.len() - 1] {
                    ring.pop();
                }
                match encode_pg_polygon(&ring) {
                    Ok(bytes) => {
                        // SAFETY: caller is on the main backend thread (asserted at
                        // the dispatch entry point); `varlena_from_bare_bytes`
                        // calls palloc on the current memory context.
                        let datum = unsafe { varlena_from_bare_bytes(&bytes) };
                        datums.push((datum, false));
                        next_offset = next_offset.saturating_add(1);
                    }
                    Err(e) => {
                        // Encoder rejected the kernel-derived ring (e.g.
                        // empty ring after closing-vertex drop). Surface as
                        // a NULL row + a debug log; do not fabricate a
                        // "default" geometry (anti-cheat ban #4).
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
    qual_datum: Option<(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)>,
) -> DispatchResult {
    let Some(resolution) = const_i32_arg(qual_datum) else {
        return DispatchResult::Deferred;
    };

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

/// `h3_cells_to_multi_polygon(cells bigint[],
///                            OUT exterior polygon, OUT holes polygon[])`
///                            -> SETOF record.
///
/// Per h3-pg's declaration at `h3--3.7.2--4.0.0.sql:196`, this SRF emits
/// a record with two fields: an exterior `polygon` and a `polygon[]` of
/// holes. We emit `AcceleratedRecord { fields_per_row: 2, datums }` with
/// one row per non-null/non-empty input array.
///
/// **Ring assignment heuristic**: the kernel emits one ring per coverage
/// region (no hole semantics — see gpu/mod.rs:1143-1208). We pick the
/// largest-vertex-count ring as the exterior and the rest as `holes[]`.
/// Single-ring outputs produce an empty `holes` array.
///
/// Multidim arrays escalate cleanly via NULL rows per anti-cheat ban #9.
///
/// # Safety
///
/// Must be called on the main backend thread; each `batch[i].0` must be
/// either NULL or a valid `bigint[]` ArrayType varlena.
unsafe fn dispatch_gpu_h3_cells_to_multi_polygon(
    batch: &[(pgrx::pg_sys::Datum, bool)],
) -> DispatchResult {
    // Two output fields per input row (exterior, holes). `datums.len()`
    // must equal `batch.len() * 2`.
    let mut datums: Vec<(pgrx::pg_sys::Datum, bool)> = Vec::with_capacity(batch.len() * 2);

    let timeout_ms = gucs::kernel_timeout_ms();
    let start = std::time::Instant::now();

    /// Push a NULL (exterior, holes) record into `datums` for the current row.
    fn push_null_record(datums: &mut Vec<(pgrx::pg_sys::Datum, bool)>) {
        datums.push((pgrx::pg_sys::Datum::from(0u32), true));
        datums.push((pgrx::pg_sys::Datum::from(0u32), true));
    }

    for &(datum, is_null) in batch {
        if is_null {
            push_null_record(&mut datums);
            continue;
        }

        // SAFETY: main backend thread; per-row Datum is bigint[] varlena.
        let arr = match unsafe { parse_array(datum) } {
            Ok(a) => a,
            Err(ArrayParseError::Multidim(n)) => {
                pgrx::debug1!(
                    "pg_accel: h3_cells_to_multi_polygon: multidim bigint[] (ndim={n}) escalated per ban #9"
                );
                push_null_record(&mut datums);
                continue;
            }
            Err(e) => {
                pgrx::debug1!(
                    "pg_accel: h3_cells_to_multi_polygon: bigint[] parse failed ({e}); NULL row"
                );
                push_null_record(&mut datums);
                continue;
            }
        };

        // Collect non-null bigint elements as u64 cells. PG bigint is i64
        // stored in the array payload as the in-memory Datum bytes, i.e. in
        // NATIVE machine byte order — not a fixed little-endian wire format.
        // Read them with `from_ne_bytes` so this is correct on big-endian
        // hosts too (the array walker yields raw payload bytes).
        let mut cells: Vec<u64> = Vec::with_capacity(arr.nelems);
        for opt_elem in &arr {
            let Some(bytes) = opt_elem else {
                continue;
            };
            if bytes.len() < 8 {
                continue;
            }
            let cell = u64::from_ne_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]);
            if cell != 0 {
                cells.push(cell);
            }
        }

        if cells.is_empty() {
            push_null_record(&mut datums);
            continue;
        }

        // Two-pass kernel: returns CSR over rings. We treat the
        // largest-vertex-count ring as the exterior; the rest become the
        // `holes[]` array per h3-pg's record contract.
        let Some(raw) = gpu::h3_cells_to_multi_polygon_bulk(&cells) else {
            pgrx::error!(
                "pg_accel: h3_cells_to_multi_polygon GPU kernel failed; refusing CPU fallback (rule 11)"
            );
        };

        let ring_count = raw.offsets.len().saturating_sub(1);
        if ring_count == 0 {
            push_null_record(&mut datums);
            continue;
        }

        // Build per-ring (lng, lat) vertex Vecs. Kernel emits coords as
        // (lat, lng) pairs in WGS84; PG `polygon` consumers treat the
        // first coord as x — match PostGIS / OGC convention.
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
            // PG `polygon` is logically closed; drop any closing duplicate
            // so npoints() matches the distinct vertex count.
            if verts.len() >= 2 && verts[0] == verts[verts.len() - 1] {
                verts.pop();
            }
            if !verts.is_empty() {
                ring_storage.push(verts);
            }
        }

        if ring_storage.is_empty() {
            push_null_record(&mut datums);
            continue;
        }

        // Pick the largest ring (by vertex count) as exterior; the rest
        // become holes. Tie-break by original index (stable).
        let exterior_idx = ring_storage
            .iter()
            .enumerate()
            .max_by_key(|(_, r)| r.len())
            .map_or(0, |(i, _)| i);
        let exterior_ring = ring_storage[exterior_idx].clone();
        let holes: Vec<&[(f64, f64)]> = ring_storage
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != exterior_idx)
            .map(|(_, r)| r.as_slice())
            .collect();

        let exterior_bytes = match encode_pg_polygon(&exterior_ring) {
            Ok(b) => b,
            Err(e) => {
                pgrx::debug1!(
                    "pg_accel: h3_cells_to_multi_polygon: exterior encode failed ({e}); NULL row"
                );
                push_null_record(&mut datums);
                continue;
            }
        };

        // Encode each hole polygon body as bare bytes; failures emit a NULL
        // row (anti-cheat ban #4: surface error, never fabricate output).
        let mut hole_bodies: Vec<Vec<u8>> = Vec::with_capacity(holes.len());
        let mut hole_encode_failed = false;
        for hole in &holes {
            match encode_pg_polygon(hole) {
                Ok(b) => hole_bodies.push(b),
                Err(e) => {
                    pgrx::debug1!(
                        "pg_accel: h3_cells_to_multi_polygon: holes[] encode failed ({e}); NULL row"
                    );
                    hole_encode_failed = true;
                    break;
                }
            }
        }
        if hole_encode_failed {
            push_null_record(&mut datums);
            continue;
        }

        // SAFETY: main backend thread; varlena_from_bare_bytes palloc's a
        // varlena in the current memory context.
        let exterior_datum = unsafe { varlena_from_bare_bytes(&exterior_bytes) };
        // Build the polygon[] holes Datum via PG's `construct_array`
        // (utils/adt/arrayfuncs.c). PG handles all ArrayType header
        // bookkeeping (ndim, dataoffset, elemtype, dim/lbound) and inter-
        // element MAXALIGN padding; hand-rolling that layout was historically
        // a source of layout drift across PG versions.
        let holes_datum = unsafe { build_polygon_array_datum(&hole_bodies) };
        datums.push((exterior_datum, false));
        datums.push((holes_datum, false));
    }

    let elapsed_ms = start.elapsed().as_millis() as i32;
    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: h3_cells_to_multi_polygon pipeline took {}ms (timeout {}ms)",
            elapsed_ms,
            timeout_ms,
        );
    }

    DispatchResult::AcceleratedRecord {
        fields_per_row: 2,
        datums,
    }
}

// ---------------------------------------------------------------------------
// pg_test integration for the Phase 2 boundary / multi_polygon arms
// ---------------------------------------------------------------------------
//
// h3-pg declares both ops with PG built-in `polygon` types:
//   h3_cell_to_boundary(cell h3index, ...) RETURNS polygon
//     (h3--3.7.2--4.0.0.sql:47)
//   h3_cells_to_multi_polygon(h3index[], OUT exterior polygon, OUT holes
//     polygon[]) RETURNS SETOF record
//     (h3--3.7.2--4.0.0.sql:196)
//
// The dispatch encodes the per-polygon body via `encode_pg_polygon` (see
// polygon_encoder.rs) and builds the `polygon[]` array varlena via PG's
// `construct_array` from `build_polygon_array_datum` above. The earlier
// hand-rolled `encode_pg_polygon_array` route mis-laid the ArrayType
// header for multi-element inputs and crashed `heap_form_tuple`; that
// helper is retained only as a tested encoder reference. These tests
// roundtrip the bytes through PG's polygon parser via `npoints(...)`.
//
// All tests skip silently if the `h3` extension isn't installable in the
// pgrx_tests DB — same idiom as function_scan.rs:661.
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

    /// `h3_cells_to_multi_polygon(ARRAY[cell, cell]::h3index[])` returns
    /// exactly one (exterior, holes) record per call regardless of input
    /// array cardinality.
    ///
    /// **Kernel-layer fix landed (Agent K Round 3):** the delegated
    /// `pgaccel_h3_cell_to_boundary_emit` no longer JIT-fails or SIGABRT's
    /// on multi-cell input (it now runs host-side; pinned by
    /// `test_cell_to_boundary_multi_cell_emit` in
    /// `pgaccel-kernels/test/test_h3.cpp`).
    ///
    /// **Encoder-layer fix landed (Agent BUG1):** the multi-cell crash
    /// was a hand-rolled `polygon[]` varlena layout that failed downstream
    /// readers in `heap_form_tuple` -> `ExecStoreHeapTuple`. Holes are now
    /// built via PG's `construct_array` in
    /// [`super::build_polygon_array_datum`], which guarantees the on-disk
    /// ArrayType layout matches what PG expects (correct ndim/dataoffset/
    /// inter-element padding). See dispatch/h3.rs ~870 for the call site.
    #[pg_test]
    fn h3_cells_to_multi_polygon_emits_one_row() {
        if !ensure_extension("h3") {
            return;
        }
        // Trigger registry init.
        Spi::run("SELECT h3_get_resolution('8a2a1072b59ffff'::h3index)").expect("h3 ping");

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

    /// `npoints(h3_cell_to_boundary('8a..'::h3index, false))` must return
    /// the cell's distinct vertex count (6 for hexagons, 5 for pentagons).
    /// This roundtrips the encoded `polygon` bytes through PG's polygon
    /// parser — replaces the older count(*) coincidence assertion that
    /// only verified "didn't crash on read".
    #[pg_test]
    fn h3_cell_to_boundary_npoints() {
        if !ensure_extension("h3") {
            return;
        }
        Spi::run("SELECT h3_get_resolution('8a2a1072b59ffff'::h3index)").expect("h3 ping");

        // Resolution-10 hexagon: 6 distinct vertices.
        let npts = Spi::get_one::<i32>(
            "SELECT npoints(h3_cell_to_boundary('8a2a1072b59ffff'::h3index, false))",
        )
        .expect("npoints query ok")
        .expect("npoints not null");
        assert_eq!(
            npts, 6,
            "h3 hexagon cell_to_boundary should yield 6 distinct vertices, got {npts}",
        );
    }

    /// `npoints(exterior)` from the multi_polygon record must succeed and
    /// return a positive vertex count. Verifies the exterior column's
    /// `polygon` bytes roundtrip through PG's polygon parser.
    #[pg_test]
    fn h3_cells_to_multi_polygon_npoints() {
        if !ensure_extension("h3") {
            return;
        }
        Spi::run("SELECT h3_get_resolution('8a2a1072b59ffff'::h3index)").expect("h3 ping");

        // Single-cell input: union is one hexagon, so exterior has 6 verts.
        let npts = Spi::get_one::<i32>(
            "SELECT npoints(exterior) FROM h3_cells_to_multi_polygon(\
             ARRAY['8a2a1072b59ffff']::h3index[])",
        )
        .expect("npoints query ok")
        .expect("npoints not null");
        assert!(
            npts > 0,
            "exterior polygon must have a positive vertex count; got {npts}",
        );
    }
}
