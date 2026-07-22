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
}
