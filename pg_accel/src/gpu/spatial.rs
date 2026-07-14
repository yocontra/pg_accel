use super::{
    GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail, PgaccelGeometry, bridge,
    status_to_result,
};

/// Typed row-wise spatial intersection dispatch.
///
/// Only a successful kernel may return the recheck-safe `0` classification.
/// Runtime, device, and output-contract failures remain hard `GpuError`s.
pub fn spatial_intersects_pairwise_result(
    geoms_a: &[PgaccelGeometry],
    geoms_b: &[PgaccelGeometry],
) -> GpuResult<Vec<i8>> {
    let count = geoms_a.len().min(geoms_b.len());
    let _span = tracing::info_span!("gpu.spatial_intersects_pairwise", count).entered();
    if count == 0 {
        return Ok(Vec::new());
    }

    let chunk_rows = crate::engine::cost::device_limits()
        .gpu_spatial_pairwise_chunk_rows
        .max(1);
    let mut results = Vec::with_capacity(count);

    for start in (0..count).step_by(chunk_rows) {
        let end = start.saturating_add(chunk_rows).min(count);
        let chunk_count = end - start;
        let mut chunk_results = vec![0i8; chunk_count];

        // SAFETY: both descriptor subslices contain `chunk_count` elements
        // and `chunk_results` has exactly `chunk_count` writable bytes.
        let status = unsafe {
            bridge::pgaccel_spatial_intersects_pairwise(
                geoms_a[start..end].as_ptr(),
                geoms_b[start..end].as_ptr(),
                chunk_count,
                chunk_results.as_mut_ptr(),
            )
        };
        status_to_result(
            status,
            GpuErrorDomain::Spatial,
            GpuOperation::Kernel("spatial_intersects_pairwise"),
        )?;
        if !chunk_results.iter().all(|result| matches!(result, -1..=1)) {
            return Err(GpuError::with_detail(
                GpuErrorDomain::Spatial,
                GpuOperation::ValidateDeviceOutput,
                GpuStatusDetail::InvalidDescriptor,
                "spatial classification must be -1, 0, or 1",
            ));
        }
        results.extend(chunk_results);
    }

    Ok(results)
}

/// Run the linear row-wise GPU spatial intersection kernel.
///
/// Pair `i` is `(geoms_a[i], geoms_b[i])`; extra rows in the longer slice are
/// ignored. Results use the recheck-safe convention 1=true, -1=false,
/// 0=uncertain. `None` means the GPU path failed and the caller must decline.
pub fn spatial_intersects_pairwise_gpu(
    geoms_a: &[PgaccelGeometry],
    geoms_b: &[PgaccelGeometry],
) -> Option<Vec<i8>> {
    spatial_intersects_pairwise_result(geoms_a, geoms_b).ok()
}

/// Compatibility bucketing wrapper over the linear pairwise kernel.
///
/// Returned pair indices are `(i, i)` because this API is row-wise. It no
/// longer allocates or evaluates a cross product.
#[allow(clippy::type_complexity)]
pub fn spatial_intersects_gpu(
    geoms_a: &[PgaccelGeometry],
    geoms_b: &[PgaccelGeometry],
) -> Option<(Vec<(u32, u32)>, Vec<(u32, u32)>, Vec<(u32, u32)>)> {
    let results = spatial_intersects_pairwise_gpu(geoms_a, geoms_b)?;
    let mut definite_true = Vec::new();
    let mut definite_false = Vec::new();
    let mut uncertain = Vec::new();
    for (index, result) in results.into_iter().enumerate() {
        let index = u32::try_from(index).ok()?;
        match result {
            1 => definite_true.push((index, index)),
            -1 => definite_false.push((index, index)),
            _ => uncertain.push((index, index)),
        }
    }

    Some((definite_true, definite_false, uncertain))
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
