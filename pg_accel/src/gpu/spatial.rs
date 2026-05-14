use super::{PgaccelGeometry, bridge};

/// Run the GPU three-layer spatial intersection pipeline.
///
/// Returns `(definite_true, definite_false, uncertain)` as vectors of
/// `(idx_a, idx_b)` pair indices, or `None` if no GPU device is available
/// or the kernel failed. Callers handle `None` by deferring to the stock
/// PostgreSQL executor (the planner would not have injected this path if
/// the GPU were unavailable — a `None` here indicates a kernel failure).
///
/// The C++ kernel evaluates all `count_a × count_b` pairs and partitions
/// them into the three buckets.  Each output pair is two consecutive `u32`
/// values `(i, j)` written by the C side.
#[allow(clippy::similar_names, clippy::type_complexity)]
pub fn spatial_intersects_gpu(
    geoms_a: &[PgaccelGeometry],
    geoms_b: &[PgaccelGeometry],
) -> Option<(Vec<(u32, u32)>, Vec<(u32, u32)>, Vec<(u32, u32)>)> {
    let count_a = geoms_a.len();
    let count_b = geoms_b.len();
    let _span = tracing::info_span!("gpu.spatial_intersects", count_a, count_b,).entered();
    if count_a == 0 || count_b == 0 {
        return Some((Vec::new(), Vec::new(), Vec::new()));
    }

    // The C++ kernel writes (i, j) pair indices — 2 u32 values per pair.
    // Worst case: all pairs land in one bucket = count_a * count_b pairs.
    let total_pairs = count_a.checked_mul(count_b)?;
    // Cap pair buffer at 128 MB (3 buffers × buf_len × 4 bytes each).
    // Beyond this, defer to PG recheck — the transfer overhead dominates.
    const MAX_PAIRS: usize = 8_000_000; // 3 × 16M × 4 = ~192 MB total
    if total_pairs > MAX_PAIRS {
        return None;
    }

    let buf_len = total_pairs * 2;
    let mut dt_buf = vec![0u32; buf_len];
    let mut df_buf = vec![0u32; buf_len];
    let mut uc_buf = vec![0u32; buf_len];
    let mut dt_count: usize = 0;
    let mut df_count: usize = 0;
    let mut uc_count: usize = 0;

    // SAFETY: geoms arrays are valid slices.  Output buffers are
    // pre-allocated to `total_pairs * 2` u32 elements each.  The C
    // function writes at most `total_pairs` pairs (2 u32 each) into
    // each buffer and sets the count to the number of pairs written.
    let status = unsafe {
        bridge::pgaccel_spatial_intersects(
            geoms_a.as_ptr(),
            count_a,
            geoms_b.as_ptr(),
            count_b,
            dt_buf.as_mut_ptr(),
            std::ptr::addr_of_mut!(dt_count),
            df_buf.as_mut_ptr(),
            std::ptr::addr_of_mut!(df_count),
            uc_buf.as_mut_ptr(),
            std::ptr::addr_of_mut!(uc_count),
        )
    };
    if !status.is_ok() {
        pgrx::debug1!(
            "pg_accel: spatial_intersects_gpu bridge returned {:?} for {}x{} pairs",
            status,
            count_a,
            count_b,
        );
        return None;
    }

    // Each count is the number of PAIRS; each pair is 2 consecutive u32s.
    let parse_pairs = |buf: &[u32], pair_count: usize| -> Vec<(u32, u32)> {
        buf[..pair_count * 2]
            .chunks_exact(2)
            .map(|c| (c[0], c[1]))
            .collect()
    };

    Some((
        parse_pairs(&dt_buf, dt_count),
        parse_pairs(&df_buf, df_count),
        parse_pairs(&uc_buf, uc_count),
    ))
}

// ---------------------------------------------------------------------------
// Bulk point-in-polygon (fast path)
// ---------------------------------------------------------------------------

/// Dedicated bulk point-in-polygon test. Takes a flat array of point (x,y)
/// pairs and a single polygon. Returns per-point results: 1=inside,
/// -1=outside, 0=uncertain/boundary.
///
/// Returns `None` if GPU bridge is unavailable.
#[allow(clippy::too_many_arguments)]
pub fn point_in_polygon_bulk(
    points_xy: &[f32],
    poly_bbox: &[f32; 4],
    poly_coords: &[f32],
    poly_coord_count: usize,
    ring_offsets: &[u32],
    ring_count: usize,
) -> Option<Vec<i8>> {
    let point_count = points_xy.len() / 2;
    let _span =
        tracing::info_span!("gpu.point_in_polygon_bulk", point_count, ring_count,).entered();
    if point_count == 0 {
        return Some(Vec::new());
    }

    let mut results = vec![0i8; point_count];
    // SAFETY: all slices are valid; results is pre-allocated to point_count.
    let status = unsafe {
        bridge::pgaccel_point_in_polygon_bulk(
            points_xy.as_ptr(),
            point_count,
            poly_bbox.as_ptr(),
            poly_coords.as_ptr(),
            poly_coord_count,
            ring_offsets.as_ptr(),
            ring_count,
            results.as_mut_ptr(),
        )
    };
    if status.is_ok() {
        return Some(results);
    }

    None
}
