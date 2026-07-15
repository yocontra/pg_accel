use super::{
    GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail,
    PgaccelRasterReclassResidentRequest, PgaccelResidentRasterValidationScratch, bridge,
    status_to_result,
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

/// Test-only hard failure retained after a real resident raster dispatch.
#[cfg(feature = "pg_test")]
pub(crate) const fn injected_raster_resident_failure() -> RasterResidentLaunchOutcome {
    RasterResidentLaunchOutcome {
        status: super::PgaccelStatus::Error as i32,
        detail: RASTER_DETAIL_NONE,
    }
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
pub fn raster_reclass_resident_validation(
    scratch: &PgaccelResidentRasterValidationScratch,
) -> GpuResult<()> {
    match raster_reclass_resident_validation_detail(scratch) {
        None => Ok(()),
        Some(detail) => Err(raster_detail_error(detail)),
    }
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
