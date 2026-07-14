use super::{
    GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail, PgaccelExpr,
    PgaccelRasterReclassResidentRequest, PgaccelReclassRule,
    PgaccelResidentRasterValidationScratch, bridge, status_to_result,
};
use super::{
    PGACCEL_RASTER_VALIDATION_BYTE_BUDGET, PGACCEL_RASTER_VALIDATION_CAPACITY,
    PGACCEL_RASTER_VALIDATION_NUMERIC_OVERFLOW, PGACCEL_RASTER_VALIDATION_OFFSETS,
    PGACCEL_RASTER_VALIDATION_RULES, PGACCEL_RASTER_VALIDATION_VIEW,
};

const RASTER_DETAIL_NONE: i32 = 0;
const RASTER_DETAIL_CONTRACT: i32 = 1;
const RASTER_DETAIL_VIEW: i32 = 2;
const RASTER_DETAIL_RULES: i32 = 3;
const RASTER_DETAIL_OFFSETS: i32 = 4;
const RASTER_DETAIL_CAPACITY: i32 = 5;
const RASTER_DETAIL_BYTE_BUDGET: i32 = 6;
const RASTER_DETAIL_NUMERIC_OVERFLOW: i32 = 7;
#[allow(dead_code)] // reason: post-borrow validation mapper is consumed by executor wiring landing separately
const RASTER_VALIDATION_KNOWN_FAILURES: u32 = PGACCEL_RASTER_VALIDATION_VIEW
    | PGACCEL_RASTER_VALIDATION_RULES
    | PGACCEL_RASTER_VALIDATION_OFFSETS
    | PGACCEL_RASTER_VALIDATION_CAPACITY
    | PGACCEL_RASTER_VALIDATION_BYTE_BUDGET
    | PGACCEL_RASTER_VALIDATION_NUMERIC_OVERFLOW;

/// Exact native reason for a rejected resident Reclass launch or validation
/// scratch. This remains separate from [`GpuStatusDetail`] so callers can
/// retain the C ABI's domain-specific reason while still using `GpuResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // reason: typed resident detail surface consumed by executor wiring landing separately
pub enum RasterResidentDetail {
    Contract,
    View,
    Rules,
    Offsets,
    Capacity,
    ByteBudget,
    NumericOverflow,
    InvalidScratch,
    UnknownHostDetail(i32),
    UnknownFailureBits(u32),
}

/// Allocation-free result retained across the resident-store borrow boundary.
/// Both fields remain raw until [`raster_reclass_resident_launch_result`] runs
/// after the borrow has been released.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterResidentLaunchOutcome {
    status: i32,
    detail: i32,
}

const _: [(); 8] = [(); std::mem::size_of::<RasterResidentLaunchOutcome>()];
const _: [(); 4] = [(); std::mem::align_of::<RasterResidentLaunchOutcome>()];

fn host_raster_detail(raw: i32) -> Option<RasterResidentDetail> {
    match raw {
        RASTER_DETAIL_NONE => None,
        RASTER_DETAIL_CONTRACT => Some(RasterResidentDetail::Contract),
        RASTER_DETAIL_VIEW => Some(RasterResidentDetail::View),
        RASTER_DETAIL_RULES => Some(RasterResidentDetail::Rules),
        RASTER_DETAIL_OFFSETS => Some(RasterResidentDetail::Offsets),
        RASTER_DETAIL_CAPACITY => Some(RasterResidentDetail::Capacity),
        RASTER_DETAIL_BYTE_BUDGET => Some(RasterResidentDetail::ByteBudget),
        RASTER_DETAIL_NUMERIC_OVERFLOW => Some(RasterResidentDetail::NumericOverflow),
        unknown => Some(RasterResidentDetail::UnknownHostDetail(unknown)),
    }
}

fn raster_detail_error(detail: RasterResidentDetail) -> GpuError {
    let (status, message) = match detail {
        RasterResidentDetail::Contract => (
            GpuStatusDetail::InvalidDescriptor,
            "resident raster request contract is invalid",
        ),
        RasterResidentDetail::View => (
            GpuStatusDetail::InvalidDescriptor,
            "resident raster device view is invalid",
        ),
        RasterResidentDetail::Rules => (
            GpuStatusDetail::InvalidDescriptor,
            "resident raster Reclass rules are invalid",
        ),
        RasterResidentDetail::Offsets => (
            GpuStatusDetail::ShapeMismatch,
            "resident raster output offsets are invalid",
        ),
        RasterResidentDetail::Capacity => (
            GpuStatusDetail::CapacityOverflow,
            "resident raster output capacity is insufficient",
        ),
        RasterResidentDetail::ByteBudget => (
            GpuStatusDetail::CapacityOverflow,
            "resident raster exact byte or launch budget is invalid",
        ),
        RasterResidentDetail::NumericOverflow => (
            GpuStatusDetail::NumericOverflow,
            "resident raster byte arithmetic overflowed",
        ),
        RasterResidentDetail::InvalidScratch => (
            GpuStatusDetail::InvalidDescriptor,
            "resident raster validation scratch is noncanonical",
        ),
        RasterResidentDetail::UnknownHostDetail(_) => (
            GpuStatusDetail::ExecutionFailed,
            "resident raster launch returned an unknown detail",
        ),
        RasterResidentDetail::UnknownFailureBits(_) => (
            GpuStatusDetail::ExecutionFailed,
            "resident raster validation returned unknown failure bits",
        ),
    };
    GpuError::with_detail(
        GpuErrorDomain::Raster,
        GpuOperation::Kernel("raster_reclass_resident"),
        status,
        message,
    )
}

/// Construct the process-local device queue before acquiring a resident-store
/// dispatch borrow. The raw launch deliberately does not call this helper: its
/// store-borrow phase must contain no lazy queue initialization.
#[allow(dead_code)] // reason: consumed by the resident raster executor checkpoint landing separately
pub fn prepare_raster_reclass_resident() -> GpuResult<()> {
    crate::ensure_backend_exit_callback();
    // SAFETY: pgaccel_init is process-idempotent and owns queue construction.
    let status = unsafe { bridge::pgaccel_init() };
    status_to_result(status, GpuErrorDomain::Raster, GpuOperation::Init)
}

/// Submit one already-built resident Reclass request and return its raw POD
/// outcome without allocating, tracing, updating counters, constructing an
/// error, or copying validation scratch. Call
/// [`prepare_raster_reclass_resident`] before acquiring the resident-store
/// dispatch borrow that protects input pointers.
///
/// # Safety
///
/// Every request pointer/span must satisfy `pgaccel_ffi.h`, all allocations
/// must remain alive through the synchronous call, and the process queue must
/// already have been prepared outside the resident-store borrow.
#[allow(dead_code)] // reason: consumed by the resident raster executor checkpoint landing separately
pub unsafe fn raster_reclass_resident_launch(
    request: &PgaccelRasterReclassResidentRequest,
) -> RasterResidentLaunchOutcome {
    let mut raw_detail = RASTER_DETAIL_NONE;
    // SAFETY: the caller upholds the resident pointer/span and queue contract.
    let status = unsafe {
        bridge::pgaccel_raster_reclass_resident_ex_raw(
            std::ptr::from_ref(request),
            std::ptr::addr_of_mut!(raw_detail),
        )
    };
    RasterResidentLaunchOutcome {
        status,
        detail: raw_detail,
    }
}

/// Convert a retained raw launch outcome after releasing the resident-store
/// dispatch borrow. This is the first point allowed to trace, update counters,
/// or construct a typed error.
#[allow(dead_code)] // reason: consumed after the resident borrow by executor wiring landing separately
pub fn raster_reclass_resident_launch_result(
    outcome: RasterResidentLaunchOutcome,
) -> GpuResult<()> {
    let status = bridge::convert_status("pgaccel_raster_reclass_resident_ex", outcome.status);
    if let Some(detail) = host_raster_detail(outcome.detail) {
        return Err(raster_detail_error(detail));
    }
    status_to_result(
        status,
        GpuErrorDomain::Raster,
        GpuOperation::Kernel("raster_reclass_resident"),
    )
}

/// Decode a host-copied validation scratch with the native precedence order.
/// This function is pure and must only receive scratch copied after the
/// resident-store dispatch borrow has been released.
#[must_use]
#[allow(dead_code)] // reason: consumed after the resident borrow by executor wiring landing separately
pub fn raster_reclass_resident_validation_detail(
    scratch: &PgaccelResidentRasterValidationScratch,
) -> Option<RasterResidentDetail> {
    if scratch.pad != 0 {
        return Some(RasterResidentDetail::InvalidScratch);
    }
    let unknown = scratch.failures & !RASTER_VALIDATION_KNOWN_FAILURES;
    if unknown != 0 {
        return Some(RasterResidentDetail::UnknownFailureBits(unknown));
    }
    if scratch.failures & PGACCEL_RASTER_VALIDATION_NUMERIC_OVERFLOW != 0 {
        Some(RasterResidentDetail::NumericOverflow)
    } else if scratch.failures & PGACCEL_RASTER_VALIDATION_RULES != 0 {
        Some(RasterResidentDetail::Rules)
    } else if scratch.failures & PGACCEL_RASTER_VALIDATION_VIEW != 0 {
        Some(RasterResidentDetail::View)
    } else if scratch.failures & PGACCEL_RASTER_VALIDATION_OFFSETS != 0 {
        Some(RasterResidentDetail::Offsets)
    } else if scratch.failures & PGACCEL_RASTER_VALIDATION_CAPACITY != 0 {
        Some(RasterResidentDetail::Capacity)
    } else if scratch.failures & PGACCEL_RASTER_VALIDATION_BYTE_BUDGET != 0 {
        Some(RasterResidentDetail::ByteBudget)
    } else {
        None
    }
}

/// Map host-copied device validation into the typed GPU error surface.
#[allow(dead_code)] // reason: consumed after the resident borrow by executor wiring landing separately
pub fn raster_reclass_resident_validation(
    scratch: &PgaccelResidentRasterValidationScratch,
) -> GpuResult<()> {
    match raster_reclass_resident_validation_detail(scratch) {
        None => Ok(()),
        Some(detail) => Err(raster_detail_error(detail)),
    }
}

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

#[cfg(test)]
mod resident_tests {
    use super::*;
    use crate::gpu::PgaccelStatus;

    fn scratch(failures: u32) -> PgaccelResidentRasterValidationScratch {
        PgaccelResidentRasterValidationScratch {
            failures,
            first_output_offset: 17,
            last_output_offset: 29,
            ..PgaccelResidentRasterValidationScratch::default()
        }
    }

    #[test]
    fn host_launch_details_map_without_raw_enum_ffi() {
        assert_eq!(host_raster_detail(0), None);
        assert_eq!(host_raster_detail(1), Some(RasterResidentDetail::Contract));
        assert_eq!(host_raster_detail(2), Some(RasterResidentDetail::View));
        assert_eq!(host_raster_detail(3), Some(RasterResidentDetail::Rules));
        assert_eq!(host_raster_detail(4), Some(RasterResidentDetail::Offsets));
        assert_eq!(host_raster_detail(5), Some(RasterResidentDetail::Capacity));
        assert_eq!(
            host_raster_detail(6),
            Some(RasterResidentDetail::ByteBudget)
        );
        assert_eq!(
            host_raster_detail(7),
            Some(RasterResidentDetail::NumericOverflow)
        );
        assert_eq!(
            host_raster_detail(99),
            Some(RasterResidentDetail::UnknownHostDetail(99))
        );
    }

    #[test]
    fn raw_launch_outcome_is_fixed_pod_and_maps_only_on_request() {
        assert_eq!(std::mem::size_of::<RasterResidentLaunchOutcome>(), 8);
        assert_eq!(std::mem::align_of::<RasterResidentLaunchOutcome>(), 4);
        assert_eq!(std::mem::offset_of!(RasterResidentLaunchOutcome, status), 0);
        assert_eq!(std::mem::offset_of!(RasterResidentLaunchOutcome, detail), 4);

        let outcome = RasterResidentLaunchOutcome {
            status: PgaccelStatus::Ok as i32,
            detail: RASTER_DETAIL_NONE,
        };
        assert!(raster_reclass_resident_launch_result(outcome).is_ok());
    }

    #[test]
    fn device_validation_bits_map_to_exact_typed_details() {
        let cases = [
            (PGACCEL_RASTER_VALIDATION_VIEW, RasterResidentDetail::View),
            (PGACCEL_RASTER_VALIDATION_RULES, RasterResidentDetail::Rules),
            (
                PGACCEL_RASTER_VALIDATION_OFFSETS,
                RasterResidentDetail::Offsets,
            ),
            (
                PGACCEL_RASTER_VALIDATION_CAPACITY,
                RasterResidentDetail::Capacity,
            ),
            (
                PGACCEL_RASTER_VALIDATION_BYTE_BUDGET,
                RasterResidentDetail::ByteBudget,
            ),
            (
                PGACCEL_RASTER_VALIDATION_NUMERIC_OVERFLOW,
                RasterResidentDetail::NumericOverflow,
            ),
        ];
        for (failure, expected) in cases {
            assert_eq!(
                raster_reclass_resident_validation_detail(&scratch(failure)),
                Some(expected)
            );
        }
        assert_eq!(raster_reclass_resident_validation_detail(&scratch(0)), None);
    }

    #[test]
    fn validation_precedence_and_corruption_are_frozen() {
        let every_known = RASTER_VALIDATION_KNOWN_FAILURES;
        assert_eq!(
            raster_reclass_resident_validation_detail(&scratch(every_known)),
            Some(RasterResidentDetail::NumericOverflow)
        );

        let unknown_bit = 1 << 31;
        assert_eq!(
            raster_reclass_resident_validation_detail(&scratch(
                PGACCEL_RASTER_VALIDATION_VIEW | unknown_bit
            )),
            Some(RasterResidentDetail::UnknownFailureBits(unknown_bit))
        );

        let mut invalid_pad = scratch(0);
        invalid_pad.pad = 1;
        assert_eq!(
            raster_reclass_resident_validation_detail(&invalid_pad),
            Some(RasterResidentDetail::InvalidScratch)
        );
    }

    #[test]
    fn validation_maps_to_normalized_gpu_status_without_using_offsets_as_failure() {
        assert!(raster_reclass_resident_validation(&scratch(0)).is_ok());

        let numeric = raster_reclass_resident_validation(&scratch(
            PGACCEL_RASTER_VALIDATION_NUMERIC_OVERFLOW,
        ))
        .expect_err("numeric validation must fail");
        assert_eq!(numeric.domain, GpuErrorDomain::Raster);
        assert_eq!(numeric.status, GpuStatusDetail::NumericOverflow);

        let capacity =
            raster_reclass_resident_validation(&scratch(PGACCEL_RASTER_VALIDATION_CAPACITY))
                .expect_err("capacity validation must fail");
        assert_eq!(capacity.status, GpuStatusDetail::CapacityOverflow);

        let offsets =
            raster_reclass_resident_validation(&scratch(PGACCEL_RASTER_VALIDATION_OFFSETS))
                .expect_err("offset validation must fail");
        assert_eq!(offsets.status, GpuStatusDetail::ShapeMismatch);
    }
}
