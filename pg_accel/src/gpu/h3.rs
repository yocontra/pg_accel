use super::{
    ExprDeviceBuffer, GpuErrorDomain, GpuOperation, GpuResult, HashAggResult, PgaccelAggState,
    bridge, status_to_result,
};

// ---------------------------------------------------------------------------
// H3 wrappers
// ---------------------------------------------------------------------------

/// GPU-accelerated bulk H3 resolution extraction.
pub fn h3_get_resolution_bulk(cells: &[u64]) -> Option<Vec<i32>> {
    let mut resolutions = vec![0i32; cells.len()];
    // SAFETY: cells and resolutions are valid slices of matching length.
    let status = unsafe {
        bridge::pgaccel_h3_get_resolution_bulk(
            cells.as_ptr(),
            cells.len(),
            resolutions.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(resolutions)
}

/// GPU-accelerated bulk H3 base-cell extraction.
pub fn h3_get_base_cell_bulk(cells: &[u64]) -> Option<Vec<i32>> {
    let mut base_cells = vec![0i32; cells.len()];
    // SAFETY: cells and base_cells are valid slices of matching length.
    let status = unsafe {
        bridge::pgaccel_h3_get_base_cell_bulk(cells.as_ptr(), cells.len(), base_cells.as_mut_ptr())
    };
    status.is_ok().then_some(base_cells)
}

/// GPU-accelerated bulk H3 cell validity check.
pub fn h3_is_valid_cell_bulk(cells: &[u64]) -> Option<Vec<u8>> {
    let mut valid = vec![0u8; cells.len()];
    // SAFETY: cells and valid are valid slices of matching length.
    let status = unsafe {
        bridge::pgaccel_h3_is_valid_cell_bulk(cells.as_ptr(), cells.len(), valid.as_mut_ptr())
    };
    status.is_ok().then_some(valid)
}

/// GPU-accelerated bulk H3 pentagon predicate.
pub fn h3_is_pentagon_bulk(cells: &[u64]) -> Option<Vec<u8>> {
    let mut is_pent = vec![0u8; cells.len()];
    // SAFETY: cells and is_pent are valid slices of matching length.
    let status = unsafe {
        bridge::pgaccel_h3_is_pentagon_bulk(cells.as_ptr(), cells.len(), is_pent.as_mut_ptr())
    };
    status.is_ok().then_some(is_pent)
}

/// GPU-accelerated bulk H3 res-class-III predicate (resolution is odd).
pub fn h3_is_res_class_iii_bulk(cells: &[u64]) -> Option<Vec<u8>> {
    let mut is_class_iii = vec![0u8; cells.len()];
    // SAFETY: cells and is_class_iii are valid slices of matching length.
    let status = unsafe {
        bridge::pgaccel_h3_is_res_class_iii_bulk(
            cells.as_ptr(),
            cells.len(),
            is_class_iii.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(is_class_iii)
}

/// GPU-accelerated bulk H3 cell-to-parent.
pub fn h3_cell_to_parent_bulk(cells: &[u64], parent_res: i32) -> Option<Vec<u64>> {
    let mut parents = vec![0u64; cells.len()];
    // SAFETY: cells and parents are valid slices of matching length.
    let status = unsafe {
        bridge::pgaccel_h3_cell_to_parent_bulk(
            cells.as_ptr(),
            cells.len(),
            parent_res,
            parents.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(parents)
}

/// Transform one already-resident H3 lane into an already-allocated device
/// output lane. No host staging or device copies occur in this wrapper.
///
/// # Safety
/// `cells` and `parents` must address `count` device `u64` elements in the
/// active pg_accel context. A non-NULL `nulls` pointer must address `count`
/// canonical bytes in the same context. The buffers must not overlap.
pub unsafe fn h3_cell_to_parent_resident(
    cells: *const u64,
    nulls: *const u8,
    count: usize,
    parent_res: i32,
    parents: *mut u64,
) -> GpuResult<()> {
    // SAFETY: caller upholds the resident pointer/count contract above.
    let status = unsafe {
        bridge::pgaccel_h3_cell_to_parent_resident(cells, nulls, count, parent_res, parents)
    };
    status_to_result(
        status,
        GpuErrorDomain::H3,
        GpuOperation::Kernel("h3_cell_to_parent_resident"),
    )
}

/// GPU-accelerated bulk H3 cell-to-center-child.
///
/// For each input cell, returns the canonical center child at the
/// requested finer `child_res`. Center child convention: each new
/// digit (positions input_res+1 .. child_res) is set to 0. Returns
/// 0 for invalid inputs (cell == 0 or child_res < input_res).
pub fn h3_cell_to_center_child_bulk(cells: &[u64], child_res: i32) -> Option<Vec<u64>> {
    let mut children = vec![0u64; cells.len()];
    // SAFETY: cells and children are valid slices of matching length.
    let status = unsafe {
        bridge::pgaccel_h3_cell_to_center_child_bulk(
            cells.as_ptr(),
            cells.len(),
            child_res,
            children.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(children)
}

/// GPU-accelerated bulk H3 grid distance.
pub fn h3_grid_distance_bulk(cells_a: &[u64], cells_b: &[u64]) -> Option<Vec<i32>> {
    let count = cells_a.len().min(cells_b.len());
    let mut distances = vec![0i32; count];
    // SAFETY: input slices and output buffer are valid and correctly sized.
    let status = unsafe {
        bridge::pgaccel_h3_grid_distance_bulk(
            cells_a.as_ptr(),
            cells_b.as_ptr(),
            count,
            distances.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(distances)
}

/// GPU-accelerated bulk H3 lat/lng to cell index conversion.
///
/// Converts arrays of latitude/longitude (in degrees) at the given H3
/// `resolution` into H3 cell indices. Returns `None` on GPU failure.
/// Invalid coordinates produce `cell_id = 0` (caller should treat as NULL).
pub fn h3_lat_lng_to_cell_bulk(lats: &[f64], lngs: &[f64], resolution: i32) -> Option<Vec<u64>> {
    let count = lats.len().min(lngs.len());
    let mut cell_ids = vec![0u64; count];
    let mut valid = vec![0u8; count];
    // SAFETY: input slices and output buffers are valid and correctly sized.
    let status = unsafe {
        bridge::pgaccel_h3_lat_lng_to_cell_bulk(
            lats.as_ptr().cast(),
            lngs.as_ptr().cast(),
            count,
            resolution,
            1, // use_fp64 = true for precision
            cell_ids.as_mut_ptr(),
            valid.as_mut_ptr(),
        )
    };
    if !status.is_ok() {
        return None;
    }
    // Zero out invalid entries so callers can treat 0 as NULL.
    for i in 0..count {
        if valid[i] == 0 {
            cell_ids[i] = 0;
        }
    }
    Some(cell_ids)
}

/// GPU-resident H3 lat/lng grouped COUNT(*).
///
/// Exact f64 and fast f32 coordinate columns live in resident relation buffers.
/// The C++ side computes H3 keys on-device and feeds
/// them directly into the device hash-count aggregate.
#[allow(dead_code)] // reason: resident H3 SQL admission is still gated above this GPU-ready wrapper.
pub fn h3_lat_lng_count_resident(
    lats_exact: &ExprDeviceBuffer<f64>,
    lngs_exact: &ExprDeviceBuffer<f64>,
    lats_f32: &ExprDeviceBuffer<f32>,
    lngs_f32: &ExprDeviceBuffer<f32>,
    resolution: i32,
) -> Option<HashAggResult> {
    let count = lats_exact
        .len()
        .min(lngs_exact.len())
        .min(lats_f32.len())
        .min(lngs_f32.len());
    if count == 0 {
        return None;
    }

    let mut state: *mut PgaccelAggState = std::ptr::null_mut();
    crate::ensure_backend_exit_callback();
    let status = unsafe {
        bridge::pgaccel_h3_lat_lng_count_resident_bulk(
            lats_exact.as_ptr(),
            lngs_exact.as_ptr(),
            lats_f32.as_ptr(),
            lngs_f32.as_ptr(),
            count,
            resolution,
            std::ptr::addr_of_mut!(state),
        )
    };
    if !status.is_ok() {
        if !state.is_null() {
            // SAFETY: state was allocated by the C++ hashagg layer.
            unsafe { bridge::pgaccel_agg_free(state) };
        }
        return None;
    }

    // SAFETY: state is either null or an owned pgaccel_agg_state allocation.
    unsafe { HashAggResult::from_raw(state) }
}

// ---------------------------------------------------------------------------
// H3 variable-output wrappers (Agent 5A kernels via two-pass size+emit)
// ---------------------------------------------------------------------------

/// Result of an H3 var-output u64 dispatch: cumulative offsets +
/// concatenated cell IDs. `offsets.len() == count + 1`; row `i`'s outputs
/// occupy `cells[offsets[i] .. offsets[i+1]]`.
pub struct H3VarOutCells {
    pub offsets: Vec<u32>,
    pub cells: Vec<u64>,
}

/// Result of an H3 var-output f64 dispatch (lat/lng coord pairs).
///
/// Consumed by the boundary / multi_polygon dispatch arms in
/// `engine/dispatch/h3.rs` (Phase 2 F3). Each pair of doubles encodes
/// `(lat, lng)` in WGS84 (EPSG:4326); the F2 GSERIALIZED encoder reorders
/// to PostGIS x=lng / y=lat semantics on encode.
pub struct H3VarOutCoords {
    pub offsets: Vec<u32>,
    pub coords: Vec<f64>,
}

fn h3_offsets_total(offsets: &[u32]) -> usize {
    offsets.last().copied().map_or(0, |v| v as usize)
}

/// `h3_grid_disk(cell, k)` — outputs all cells within `k`-ring distance of
/// each input. Two-pass: size then emit. Returns `None` on kernel failure.
pub fn h3_grid_disk_bulk(cells: &[u64], k: i32) -> Option<H3VarOutCells> {
    let count = cells.len();
    let mut offsets = vec![0u32; count + 1];
    // SAFETY: cells / offsets are caller-owned slices of the declared lengths.
    let status = unsafe {
        bridge::pgaccel_h3_grid_disk_output_size(cells.as_ptr(), count, k, offsets.as_mut_ptr())
    };
    if !status.is_ok() {
        return None;
    }
    let total = h3_offsets_total(&offsets);
    let mut out_cells = vec![0u64; total];
    if total > 0 {
        // SAFETY: offsets is the buffer the size pass populated; out_cells
        // has total elements.
        let status = unsafe {
            bridge::pgaccel_h3_grid_disk_emit(
                cells.as_ptr(),
                count,
                k,
                offsets.as_ptr(),
                out_cells.as_mut_ptr(),
            )
        };
        if !status.is_ok() {
            return None;
        }
    }
    Some(H3VarOutCells {
        offsets,
        cells: out_cells,
    })
}

/// `h3_grid_ring_unsafe(cell, k)` — outputs the k-th ring per input
/// (smaller fan-out than `grid_disk`).
pub fn h3_grid_ring_unsafe_bulk(cells: &[u64], k: i32) -> Option<H3VarOutCells> {
    let count = cells.len();
    let mut offsets = vec![0u32; count + 1];
    // SAFETY: cells / offsets are caller-owned slices of the declared lengths.
    let status = unsafe {
        bridge::pgaccel_h3_grid_ring_unsafe_output_size(
            cells.as_ptr(),
            count,
            k,
            offsets.as_mut_ptr(),
        )
    };
    if !status.is_ok() {
        return None;
    }
    let total = h3_offsets_total(&offsets);
    let mut out_cells = vec![0u64; total];
    if total > 0 {
        // SAFETY: offsets is from the size pass; out_cells has total elements.
        let status = unsafe {
            bridge::pgaccel_h3_grid_ring_unsafe_emit(
                cells.as_ptr(),
                count,
                k,
                offsets.as_ptr(),
                out_cells.as_mut_ptr(),
            )
        };
        if !status.is_ok() {
            return None;
        }
    }
    Some(H3VarOutCells {
        offsets,
        cells: out_cells,
    })
}

/// `h3_polyfill(geom, resolution)` — outputs cells whose centre lies
/// inside the polygon. `coords` is flat `[x0,y0,...]` in lon/lat degrees;
/// `ring_offsets` indexes into `coords` for `ring_count` rings.
///
/// Consumed by the `h3_polyfill` dispatch arm in `engine/dispatch/h3.rs`
/// (Phase 2 F3, replaces the deferred arm that documented the missing
/// per-row geometry extractor).
pub fn h3_polyfill_bulk(
    coords: &[f32],
    ring_offsets: &[u32],
    ring_count: usize,
    resolution: i32,
) -> Option<H3VarOutCells> {
    if ring_count == 0 {
        return Some(H3VarOutCells {
            offsets: vec![0u32; 1],
            cells: Vec::new(),
        });
    }
    let mut offsets = vec![0u32; ring_count + 1];
    // SAFETY: coords/ring_offsets/offsets are caller-owned slices.
    let status = unsafe {
        bridge::pgaccel_h3_polyfill_output_size(
            coords.as_ptr(),
            ring_offsets.as_ptr(),
            ring_count,
            resolution,
            offsets.as_mut_ptr(),
        )
    };
    if !status.is_ok() {
        return None;
    }
    let total = h3_offsets_total(&offsets);
    let mut out_cells = vec![0u64; total];
    if total > 0 {
        // SAFETY: same buffers, plus out_cells of length `total`.
        let status = unsafe {
            bridge::pgaccel_h3_polyfill_emit(
                coords.as_ptr(),
                ring_offsets.as_ptr(),
                ring_count,
                resolution,
                offsets.as_ptr(),
                out_cells.as_mut_ptr(),
            )
        };
        if !status.is_ok() {
            return None;
        }
    }
    Some(H3VarOutCells {
        offsets,
        cells: out_cells,
    })
}

/// `h3_cell_to_children(cell, child_res)` — outputs child cells at the
/// requested resolution. Output count is deterministic per input (7^Δres
/// for hexagons).
pub fn h3_cell_to_children_bulk(cells: &[u64], child_res: i32) -> Option<H3VarOutCells> {
    let count = cells.len();
    let mut offsets = vec![0u32; count + 1];
    // SAFETY: cells / offsets are caller-owned slices of the declared lengths.
    let status = unsafe {
        bridge::pgaccel_h3_cell_to_children_output_size(
            cells.as_ptr(),
            count,
            child_res,
            offsets.as_mut_ptr(),
        )
    };
    if !status.is_ok() {
        return None;
    }
    let total = h3_offsets_total(&offsets);
    let mut out_children = vec![0u64; total];
    if total > 0 {
        // SAFETY: offsets is the buffer the size pass populated.
        let status = unsafe {
            bridge::pgaccel_h3_cell_to_children_emit(
                cells.as_ptr(),
                count,
                child_res,
                offsets.as_ptr(),
                out_children.as_mut_ptr(),
            )
        };
        if !status.is_ok() {
            return None;
        }
    }
    Some(H3VarOutCells {
        offsets,
        cells: out_children,
    })
}

/// `h3_cell_to_boundary(cell)` — emits 6 (hexagon) or 5 (pentagon) lat/lng
/// vertex pairs per cell. Offsets are in DOUBLE units (2 doubles per
/// vertex pair × 6 = 12 doubles per hexagon).
///
/// Consumed by the `h3_cell_to_boundary` dispatch arm in
/// `engine/dispatch/h3.rs` (Phase 2 F3, replaces the deferred arm that
/// documented the missing PostGIS GSERIALIZED encoder).
pub fn h3_cell_to_boundary_bulk(cells: &[u64]) -> Option<H3VarOutCoords> {
    let count = cells.len();
    let mut offsets = vec![0u32; count + 1];
    // SAFETY: cells / offsets are caller-owned slices.
    let status = unsafe {
        bridge::pgaccel_h3_cell_to_boundary_output_size(cells.as_ptr(), count, offsets.as_mut_ptr())
    };
    if !status.is_ok() {
        return None;
    }
    let total = h3_offsets_total(&offsets);
    let mut out_coords = vec![0.0f64; total];
    if total > 0 {
        // SAFETY: offsets came from the size pass; out_coords has `total` doubles.
        let status = unsafe {
            bridge::pgaccel_h3_cell_to_boundary_emit(
                cells.as_ptr(),
                count,
                offsets.as_ptr(),
                out_coords.as_mut_ptr(),
            )
        };
        if !status.is_ok() {
            return None;
        }
    }
    Some(H3VarOutCoords {
        offsets,
        coords: out_coords,
    })
}

/// `h3_cells_to_multi_polygon(cells[])` — outputs the union of input cell
/// boundaries as a CSR over polygon rings. The single executor row is the
/// entire input array; the output offsets index over rings.
///
/// Returns `(ring_offsets, coords)` where `ring_offsets.len() == ring_count + 1`
/// and `coords.len() == ring_offsets[ring_count]` (in doubles).
///
/// Phase 2 B3 status: consumed by
/// `engine::dispatch::h3::dispatch_gpu_h3_cells_to_multi_polygon` once
/// the bigint[] ArrayType walker landed in `adapters::extractors::array`.
pub fn h3_cells_to_multi_polygon_bulk(cells: &[u64]) -> Option<H3VarOutCoords> {
    let count = cells.len();
    if count == 0 {
        return Some(H3VarOutCoords {
            offsets: vec![0u32; 1],
            coords: Vec::new(),
        });
    }
    // Worst case: one ring per cell. Allocate `count + 1` slots; the size
    // pass writes the realised ring count.
    let mut ring_offsets = vec![0u32; count + 1];
    let mut ring_count: u32 = 0;
    // SAFETY: cells / ring_offsets are caller-owned slices; ring_count is
    // a stack scalar the kernel writes once.
    let status = unsafe {
        bridge::pgaccel_h3_cells_to_multi_polygon_output_size(
            cells.as_ptr(),
            count,
            ring_offsets.as_mut_ptr(),
            std::ptr::addr_of_mut!(ring_count),
        )
    };
    if !status.is_ok() {
        return None;
    }
    // Truncate ring_offsets to the realised ring count + 1.
    let realised = ring_count as usize;
    if realised + 1 > ring_offsets.len() {
        return None;
    }
    ring_offsets.truncate(realised + 1);
    let total = h3_offsets_total(&ring_offsets);
    let mut out_coords = vec![0.0f64; total];
    if total > 0 {
        // SAFETY: ring_offsets / out_coords are caller-owned and correctly sized.
        let status = unsafe {
            bridge::pgaccel_h3_cells_to_multi_polygon_emit(
                cells.as_ptr(),
                count,
                ring_offsets.as_ptr(),
                ring_count,
                out_coords.as_mut_ptr(),
            )
        };
        if !status.is_ok() {
            return None;
        }
    }
    Some(H3VarOutCoords {
        offsets: ring_offsets,
        coords: out_coords,
    })
}
