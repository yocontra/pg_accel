//! GPU-resident H3 transformations used by descriptor aggregates.

use super::{
    GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail, PgaccelStatus, bridge,
    status_to_result,
};

const H3_PARENT_DETAIL_NONE: i32 = 0;
const H3_PARENT_DETAIL_CONTRACT: i32 = 1;
const H3_PARENT_DETAIL_INVALID_CELL: i32 = 2;
const H3_PARENT_DETAIL_RES_MISMATCH: i32 = 3;
const H3_PARENT_RESIDENT_OPERATION: GpuOperation =
    GpuOperation::Kernel("h3_cell_to_parent_resident");
const H3_LATLNG_DETAIL_NONE: i32 = 0;
const H3_LATLNG_DETAIL_CONTRACT: i32 = 1;
const H3_LATLNG_DETAIL_NONFINITE: i32 = 2;
const H3_LATLNG_DETAIL_STRICT_RANGE: i32 = 3;
const H3_LATLNG_RESIDENT_OPERATION: GpuOperation =
    GpuOperation::Kernel("h3_lat_lng_to_cell_resident");

fn h3_parent_resident_result(status: PgaccelStatus, detail: i32) -> GpuResult<()> {
    if status.is_ok() {
        return if detail == H3_PARENT_DETAIL_NONE {
            Ok(())
        } else {
            Err(GpuError::with_detail(
                GpuErrorDomain::H3,
                H3_PARENT_RESIDENT_OPERATION,
                GpuStatusDetail::InvalidDescriptor,
                "successful resident H3 parent transform returned an error detail",
            ))
        };
    }
    if status != PgaccelStatus::InvalidArgument {
        return status_to_result(status, GpuErrorDomain::H3, H3_PARENT_RESIDENT_OPERATION);
    }
    let mapped = match detail {
        H3_PARENT_DETAIL_INVALID_CELL => GpuStatusDetail::InvalidArgument,
        H3_PARENT_DETAIL_RES_MISMATCH => GpuStatusDetail::ShapeMismatch,
        H3_PARENT_DETAIL_NONE | H3_PARENT_DETAIL_CONTRACT => GpuStatusDetail::InvalidDescriptor,
        _ => GpuStatusDetail::InvalidDescriptor,
    };
    Err(GpuError::new(
        GpuErrorDomain::H3,
        H3_PARENT_RESIDENT_OPERATION,
        mapped,
    ))
}

/// Transform one already-resident H3 lane into an allocated device lane.
///
/// # Safety
/// `cells` and `parents` must address `count` device `u64` elements in
/// the active context. A non-null `nulls` pointer must address `count`
/// canonical bytes in that context. The input and output buffers must not
/// overlap.
pub unsafe fn h3_cell_to_parent_resident(
    cells: *const u64,
    nulls: *const u8,
    count: usize,
    parent_res: i32,
    parents: *mut u64,
) -> GpuResult<()> {
    let mut detail = H3_PARENT_DETAIL_NONE;
    // SAFETY: the caller upholds the resident pointer, size, and aliasing
    // contract; detail remains writable for this synchronous call.
    let status = unsafe {
        bridge::pgaccel_h3_cell_to_parent_resident_ex(
            cells,
            nulls,
            count,
            parent_res,
            parents,
            std::ptr::from_mut(&mut detail),
        )
    };
    h3_parent_resident_result(status, detail)
}

fn h3_latlng_resident_result(status: PgaccelStatus, detail: i32) -> GpuResult<()> {
    if status.is_ok() {
        return if detail == H3_LATLNG_DETAIL_NONE {
            Ok(())
        } else {
            Err(GpuError::with_detail(
                GpuErrorDomain::H3,
                H3_LATLNG_RESIDENT_OPERATION,
                GpuStatusDetail::InvalidDescriptor,
                "successful resident H3 lat/lng transform returned an error detail",
            ))
        };
    }
    if status != PgaccelStatus::InvalidArgument {
        return status_to_result(status, GpuErrorDomain::H3, H3_LATLNG_RESIDENT_OPERATION);
    }
    let (mapped, message) = match detail {
        H3_LATLNG_DETAIL_NONFINITE => (
            GpuStatusDetail::InvalidArgument,
            "H3 latitude or longitude is not finite",
        ),
        H3_LATLNG_DETAIL_STRICT_RANGE => (
            GpuStatusDetail::NumericOverflow,
            "H3 strict latitude or longitude is outside its accepted range",
        ),
        H3_LATLNG_DETAIL_NONE | H3_LATLNG_DETAIL_CONTRACT => (
            GpuStatusDetail::InvalidDescriptor,
            "resident H3 lat/lng transform contract violation",
        ),
        _ => (
            GpuStatusDetail::InvalidDescriptor,
            "resident H3 lat/lng transform returned an unknown detail",
        ),
    };
    Err(GpuError::with_detail(
        GpuErrorDomain::H3,
        H3_LATLNG_RESIDENT_OPERATION,
        mapped,
        message,
    ))
}

/// Transform two already-resident coordinate lanes into H3 cells.
///
/// `use_fp64=false` means both value pointers address `f32`; `true` means
/// both address `f64`. A NULL in either input produces a NULL output.
/// `strict_ranges` mirrors h3-pg's `h3.strict` range checks.
///
/// # Safety
/// Every non-null pointer must address `count` elements in the active device
/// context and typed value/output pointers are naturally aligned. NULL
/// sidecars contain canonical bytes. Writable outputs are disjoint from every
/// input and each other; sidecars are disjoint from value lanes. The two
/// read-only value lanes may alias each other, as may the two sidecars.
#[allow(dead_code)] // reason: production primitive remains planner-inaccessible until descriptor wiring lands
#[allow(clippy::too_many_arguments)] // reason: mirrors the fixed resident H3 FFI request fields
pub unsafe fn h3_lat_lng_to_cell_resident(
    latitude: *const std::ffi::c_void,
    latitude_nulls: *const u8,
    longitude: *const std::ffi::c_void,
    longitude_nulls: *const u8,
    count: usize,
    resolution: i32,
    use_fp64: bool,
    strict_ranges: bool,
    cell_ids: *mut u64,
    output_nulls: *mut u8,
) -> GpuResult<()> {
    let mut detail = H3_LATLNG_DETAIL_NONE;
    // SAFETY: the caller upholds the resident pointer, size, sidecar, and
    // aliasing contracts; detail remains writable for this synchronous call.
    let status = unsafe {
        bridge::pgaccel_h3_lat_lng_to_cell_resident_ex(
            latitude,
            latitude_nulls,
            longitude,
            longitude_nulls,
            count,
            resolution,
            i32::from(use_fp64),
            i32::from(strict_ranges),
            cell_ids,
            output_nulls,
            std::ptr::from_mut(&mut detail),
        )
    };
    h3_latlng_resident_result(status, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resident_parent_detail_mapping_is_fail_closed() {
        assert!(h3_parent_resident_result(PgaccelStatus::Ok, H3_PARENT_DETAIL_NONE).is_ok());
        assert_eq!(
            h3_parent_resident_result(
                PgaccelStatus::InvalidArgument,
                H3_PARENT_DETAIL_INVALID_CELL,
            )
            .expect_err("an invalid H3 cell detail must fail")
            .status,
            GpuStatusDetail::InvalidArgument,
        );
        assert_eq!(
            h3_parent_resident_result(
                PgaccelStatus::InvalidArgument,
                H3_PARENT_DETAIL_RES_MISMATCH,
            )
            .expect_err("an H3 resolution mismatch detail must fail")
            .status,
            GpuStatusDetail::ShapeMismatch,
        );
        assert_eq!(
            h3_parent_resident_result(PgaccelStatus::Ok, H3_PARENT_DETAIL_CONTRACT)
                .expect_err("a successful H3 status with an error detail must fail")
                .status,
            GpuStatusDetail::InvalidDescriptor,
        );
    }

    #[test]
    fn resident_latlng_detail_mapping_preserves_sql_failure_classes() {
        assert!(h3_latlng_resident_result(PgaccelStatus::Ok, H3_LATLNG_DETAIL_NONE).is_ok());
        assert_eq!(
            h3_latlng_resident_result(PgaccelStatus::InvalidArgument, H3_LATLNG_DETAIL_NONFINITE,)
                .expect_err("non-finite coordinates must fail")
                .status,
            GpuStatusDetail::InvalidArgument,
        );
        assert_eq!(
            h3_latlng_resident_result(
                PgaccelStatus::InvalidArgument,
                H3_LATLNG_DETAIL_STRICT_RANGE,
            )
            .expect_err("strict range violations must fail")
            .status,
            GpuStatusDetail::NumericOverflow,
        );
        assert_eq!(
            h3_latlng_resident_result(PgaccelStatus::Ok, H3_LATLNG_DETAIL_CONTRACT)
                .expect_err("success plus an error detail must fail")
                .status,
            GpuStatusDetail::InvalidDescriptor,
        );
        assert_eq!(
            h3_latlng_resident_result(PgaccelStatus::InvalidArgument, i32::MAX)
                .expect_err("unknown detail values must fail closed")
                .status,
            GpuStatusDetail::InvalidDescriptor,
        );
        assert_eq!(
            h3_latlng_resident_result(PgaccelStatus::ErrorOom, H3_LATLNG_DETAIL_CONTRACT)
                .expect_err("non-argument statuses retain their runtime class")
                .status,
            GpuStatusDetail::OutOfMemory,
        );
    }
}
