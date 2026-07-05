use super::{PgaccelExpr, PgaccelReclassRule, bridge};

// ---------------------------------------------------------------------------
// Raster wrappers
// ---------------------------------------------------------------------------

/// GPU-accelerated map algebra.
///
/// Evaluates a bytecode expression per pixel across one or more bands.
/// `band_pixels` is a slice of per-band pixel buffers (each as raw bytes),
/// `pixel_count` is the number of pixels per band, and `pixel_type` is the
/// FFI pixel type code (`PgaccelPixelType as i32`).
///
/// Returns the output pixel buffer and updated nodata mask, or `None` if
/// the GPU is unavailable.
#[allow(clippy::too_many_arguments)]
pub fn map_algebra(
    band_pixels: &[*const std::ffi::c_void],
    pixel_count: usize,
    pixel_type: i32,
    expr: &PgaccelExpr,
    output_pixels: &mut [u8],
    nodata_mask: &mut [u8],
) -> Option<()> {
    if pixel_count == 0 {
        return Some(());
    }
    if expr.inst_count == 0
        || expr.band_count == 0
        || expr.band_count > 8
        || band_pixels.len() < expr.band_count
    {
        return None;
    }

    // SAFETY: band_pixels contains valid pointers to pixel data.
    // output_pixels and nodata_mask are pre-allocated by the caller.
    // expr is a valid PgaccelExpr with correct inst_count/band_count.
    let status = unsafe {
        bridge::pgaccel_map_algebra(
            band_pixels.as_ptr(),
            pixel_count,
            pixel_type,
            std::ptr::from_ref(expr),
            output_pixels.as_mut_ptr().cast::<std::ffi::c_void>(),
            nodata_mask.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(())
}

/// GPU-accelerated raster clip.
///
/// Clips a raster band to a polygon ring.  Pixels outside the ring are
/// marked as NODATA in the output mask.  Pixel data is copied to
/// `output_pixels`; the nodata mask is updated in place.
///
/// Returns `None` if the GPU is unavailable.
#[allow(clippy::too_many_arguments)]
pub fn raster_clip(
    rast_pixels: *const std::ffi::c_void,
    width: usize,
    height: usize,
    origin_x: f64,
    origin_y: f64,
    scale_x: f64,
    scale_y: f64,
    pixel_type: i32,
    clip_ring_xy: &[f32],
    output_pixels: &mut [u8],
    nodata_mask: &mut [u8],
) -> Option<()> {
    let vertex_count = clip_ring_xy.len() / 2;
    if width == 0 || height == 0 || vertex_count < 3 {
        return Some(());
    }

    // SAFETY: rast_pixels is a valid pointer to pixel data.
    // clip_ring_xy is a valid flat f32 slice of (x,y) pairs.
    // output_pixels and nodata_mask are pre-allocated by the caller.
    let status = unsafe {
        bridge::pgaccel_raster_clip(
            rast_pixels,
            width,
            height,
            origin_x,
            origin_y,
            scale_x,
            scale_y,
            pixel_type,
            clip_ring_xy.as_ptr(),
            vertex_count,
            output_pixels.as_mut_ptr().cast::<std::ffi::c_void>(),
            nodata_mask.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(())
}

/// GPU-accelerated raster reclassification.
///
/// Applies a set of value-range rules to reclassify pixel values.
/// Returns `None` if the GPU is unavailable.
pub fn raster_reclass(
    input_pixels: *const std::ffi::c_void,
    pixel_count: usize,
    input_type: i32,
    rules: &[PgaccelReclassRule],
    output_type: i32,
    output_pixels: &mut [u8],
) -> Option<()> {
    if pixel_count == 0 {
        return Some(());
    }

    // SAFETY: input_pixels is a valid pointer to pixel data.
    // rules is a valid slice. output_pixels is pre-allocated.
    let status = unsafe {
        bridge::pgaccel_raster_reclass(
            input_pixels,
            pixel_count,
            input_type,
            rules.as_ptr(),
            rules.len(),
            output_type,
            output_pixels.as_mut_ptr().cast::<std::ffi::c_void>(),
        )
    };
    status.is_ok().then_some(())
}

// ---------------------------------------------------------------------------
// Raster extension wrappers (Agent 3A — see raster_ops.cpp)
// ---------------------------------------------------------------------------

/// GPU-accelerated bilinear resample.
///
/// Returns `None` if the kernel fails (caller routes to PG fallback).
///
/// Wired into `dispatch::raster::dispatch_st_resample` via the multi-arg
/// dispatch carrier landed in Phase II Agent F1 (target_w / target_h are
/// captured as `qual_datums[0..2]`).
#[allow(clippy::too_many_arguments)]
pub fn raster_resample(
    src_pixels: &[f32],
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
    dst_pixels: &mut [f32],
) -> Option<()> {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Some(());
    }
    if src_pixels.len() < src_w * src_h {
        return None;
    }
    if dst_pixels.len() < dst_w * dst_h {
        return None;
    }
    // SAFETY: slice lengths checked above; both pointers refer to valid
    // contiguous fp32 buffers owned by the caller for the duration of
    // this call.
    let status = unsafe {
        bridge::pgaccel_raster_resample(
            src_pixels.as_ptr(),
            src_w,
            src_h,
            dst_w,
            dst_h,
            dst_pixels.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(())
}

/// GPU-accelerated slope (Horn's method, output in degrees).
///
/// Wired into `dispatch::raster::dispatch_st_slope` via the multi-arg
/// dispatch carrier landed in Phase II Agent F1 (cell_x / cell_y are
/// captured as `qual_datums[0..2]` f64 args).
pub fn raster_slope(
    src_pixels: &[f32],
    width: usize,
    height: usize,
    cell_size_x: f64,
    cell_size_y: f64,
    slope_out: &mut [f32],
) -> Option<()> {
    if width == 0 || height == 0 {
        return Some(());
    }
    let n = width * height;
    if src_pixels.len() < n || slope_out.len() < n {
        return None;
    }
    // SAFETY: slice lengths checked above; both buffers are caller-owned.
    let status = unsafe {
        bridge::pgaccel_raster_slope(
            src_pixels.as_ptr(),
            width,
            height,
            cell_size_x,
            cell_size_y,
            slope_out.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(())
}

/// GPU-accelerated aspect (compass direction in degrees).
///
/// Wired into `dispatch::raster::dispatch_st_aspect` via the multi-arg
/// dispatch carrier landed in Phase II Agent F1. The kernel itself ignores
/// cell sizes (aspect is angle-only) but the dispatcher still validates
/// the cell_x / cell_y args from `qual_datums[0..2]` so mis-typed call
/// sites surface as deferrals rather than silent corruption.
pub fn raster_aspect(
    src_pixels: &[f32],
    width: usize,
    height: usize,
    aspect_out: &mut [f32],
) -> Option<()> {
    if width == 0 || height == 0 {
        return Some(());
    }
    let n = width * height;
    if src_pixels.len() < n || aspect_out.len() < n {
        return None;
    }
    // SAFETY: slice lengths checked above.
    let status = unsafe {
        bridge::pgaccel_raster_aspect(src_pixels.as_ptr(), width, height, aspect_out.as_mut_ptr())
    };
    status.is_ok().then_some(())
}

/// GPU-accelerated hillshade (shaded relief, output [0, 255]).
///
/// Wired into `dispatch::raster::dispatch_st_hillshade` via the multi-arg
/// dispatch carrier landed in Phase II Agent F1
/// (cell_x / cell_y / sun_azimuth / sun_altitude are
/// `qual_datums[0..4]`; z_factor is fixed to 1.0 today).
#[allow(clippy::too_many_arguments)]
pub fn raster_hillshade(
    src_pixels: &[f32],
    width: usize,
    height: usize,
    cell_size_x: f64,
    cell_size_y: f64,
    sun_azimuth_deg: f64,
    sun_altitude_deg: f64,
    z_factor: f64,
    shade_out: &mut [f32],
) -> Option<()> {
    if width == 0 || height == 0 {
        return Some(());
    }
    let n = width * height;
    if src_pixels.len() < n || shade_out.len() < n {
        return None;
    }
    // SAFETY: slice lengths checked above.
    let status = unsafe {
        bridge::pgaccel_raster_hillshade(
            src_pixels.as_ptr(),
            width,
            height,
            cell_size_x,
            cell_size_y,
            sun_azimuth_deg,
            sun_altitude_deg,
            z_factor,
            shade_out.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(())
}

/// GPU-accelerated point-in-raster pixel-value lookup.
///
/// `point_xy` is interleaved `[x0, y0, x1, y1, ...]`. The output slice
/// must have at least `point_xy.len() / 2` elements. Out-of-bounds
/// points get NaN.
///
/// Phase 2 B3 status: consumed by
/// `engine::dispatch::raster::dispatch_st_value` now that the geometry[]
/// ArrayType walker landed.
#[allow(clippy::too_many_arguments)]
pub fn raster_value(
    rast_pixels: &[f32],
    width: usize,
    height: usize,
    origin_x: f64,
    origin_y: f64,
    scale_x: f64,
    scale_y: f64,
    point_xy: &[f64],
    output: &mut [f64],
) -> Option<()> {
    let point_count = point_xy.len() / 2;
    if width == 0 || height == 0 || point_count == 0 {
        return Some(());
    }
    if rast_pixels.len() < width * height {
        return None;
    }
    if output.len() < point_count {
        return None;
    }
    // SAFETY: slice lengths checked above.
    let status = unsafe {
        bridge::pgaccel_raster_value(
            rast_pixels.as_ptr(),
            width,
            height,
            origin_x,
            origin_y,
            scale_x,
            scale_y,
            point_xy.as_ptr(),
            point_count,
            output.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(())
}

/// GPU-accelerated 6-scalar per-row summary stats (count/sum/mean/stddev/min/max).
///
/// Pairs with [`crate::engine::registry::OutputShape::Record`]
/// `{ field_count: 6 }`. Output slice must hold `6 * row_count` `f64`s
/// laid out as `[row0_count, row0_sum, row0_mean, row0_stddev, row0_min,
/// row0_max, row1_count, ...]`. When `nodata_masks` is `Some`, mask byte
/// `1` skips that pixel.
pub fn raster_summarystats(
    rast_pixels: &[f32],
    row_count: usize,
    pixels_per_row: usize,
    nodata_masks: Option<&[u8]>,
    output: &mut [f64],
) -> Option<()> {
    if row_count == 0 || pixels_per_row == 0 {
        return Some(());
    }
    let n = row_count * pixels_per_row;
    if rast_pixels.len() < n || output.len() < row_count * 6 {
        return None;
    }
    if let Some(mask) = nodata_masks
        && mask.len() < n
    {
        return None;
    }
    let mask_ptr = nodata_masks.map_or(std::ptr::null(), <[u8]>::as_ptr);
    // SAFETY: slice lengths checked above; mask_ptr is null XOR points to
    // a buffer of at least `n` bytes (verified above).
    let status = unsafe {
        bridge::pgaccel_raster_summarystats(
            rast_pixels.as_ptr(),
            row_count,
            pixels_per_row,
            mask_ptr,
            output.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(())
}
