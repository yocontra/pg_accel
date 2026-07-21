use super::expr::ExprDeviceBuffer;
use super::{
    GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail, PgaccelStatus, bridge,
    status_to_result,
};

const RESIDENT_SPATIAL_OPERATION: GpuOperation = GpuOperation::Kernel("spatial_eval_resident");
const RESIDENT_SPATIAL_COMPACT_OPERATION: GpuOperation =
    GpuOperation::Kernel("spatial_recheck_compact");
const RESIDENT_SPATIAL_PATCH_OPERATION: GpuOperation =
    GpuOperation::Kernel("spatial_recheck_patch");
const RESIDENT_SPATIAL_FINISH_OPERATION: GpuOperation =
    GpuOperation::Kernel("spatial_workspace_finish");

fn resident_builder_error(status: GpuStatusDetail, detail: &'static str) -> GpuError {
    GpuError::with_detail(
        GpuErrorDomain::Spatial,
        RESIDENT_SPATIAL_OPERATION,
        status,
        detail,
    )
}

fn resident_buffer_bytes<T>(buffer: &ExprDeviceBuffer<T>) -> GpuResult<usize> {
    buffer
        .len()
        .checked_mul(std::mem::size_of::<T>())
        .ok_or_else(|| {
            resident_builder_error(
                GpuStatusDetail::CapacityOverflow,
                "resident spatial device-buffer byte length overflow",
            )
        })
}

/// Frozen ABI version for resident fp64 geometry descriptors.
pub const PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION: u32 = 2;

/// Frozen ABI version for caller-owned resident spatial control scratch.
pub const PGACCEL_SPATIAL_WORKSPACE_ABI_VERSION: u32 = 1;

/// Frozen ABI version for ordered exact-recheck helpers.
pub const PGACCEL_SPATIAL_RECHECK_ABI_VERSION: u32 = 1;

/// Exact caller-owned device control span required by every spatial launch.
pub const PGACCEL_SPATIAL_CONTROL_BYTES: usize = 384;

/// Hard per-launch row ceiling. The PostgreSQL executor owns larger chunking.
pub const PGACCEL_SPATIAL_MAX_CHUNK_ROWS: usize = 65_536;

/// Resident geometry row flag indicating a populated `[xmin, ymin, xmax, ymax]` bbox.
#[allow(dead_code)] // reason: frozen native row flag pinned by ABI tests; Rust builds rows in residency
pub const PGACCEL_RESIDENT_GEOMETRY_BBOX_VALID: u32 = 1 << 0;

/// One resident geometry row. Geometry and ring offsets count coordinate pairs.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PgaccelResidentGeometryRow {
    pub geom_type: u32,
    pub srid: i32,
    pub first_ring: u64,
    pub ring_count: u32,
    pub flags: u32,
}

/// Device-resident structure-of-arrays view for one fp64 geometry column.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelResidentGeometryView {
    pub abi_version: u32,
    pub flags: u32,
    pub coordinates: *const f64,
    pub bboxes: *const f64,
    pub geometry_offsets: *const u64,
    pub ring_offsets: *const u64,
    pub rows: *const PgaccelResidentGeometryRow,
    pub nulls: *const u8,
    pub coordinates_bytes: usize,
    pub bboxes_bytes: usize,
    pub geometry_offsets_bytes: usize,
    pub ring_offsets_bytes: usize,
    pub rows_bytes: usize,
    pub nulls_bytes: usize,
    pub row_count: usize,
    pub coordinate_pair_count: usize,
    pub ring_count: usize,
}

impl PgaccelResidentGeometryView {
    /// Build a frozen-v2 view over already-resident geometry lanes.
    #[allow(clippy::too_many_arguments)] // reason: fields mirror the frozen C descriptor
    #[must_use]
    pub const fn new(
        coordinates: *const f64,
        bboxes: *const f64,
        geometry_offsets: *const u64,
        ring_offsets: *const u64,
        rows: *const PgaccelResidentGeometryRow,
        nulls: *const u8,
        coordinates_bytes: usize,
        bboxes_bytes: usize,
        geometry_offsets_bytes: usize,
        ring_offsets_bytes: usize,
        rows_bytes: usize,
        nulls_bytes: usize,
        row_count: usize,
        coordinate_pair_count: usize,
        ring_count: usize,
    ) -> Self {
        Self {
            abi_version: PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION,
            flags: 0,
            coordinates,
            bboxes,
            geometry_offsets,
            ring_offsets,
            rows,
            nulls,
            coordinates_bytes,
            bboxes_bytes,
            geometry_offsets_bytes,
            ring_offsets_bytes,
            rows_bytes,
            nulls_bytes,
            row_count,
            coordinate_pair_count,
            ring_count,
        }
    }

    /// Build a view from the allocations that own every lane.
    ///
    /// Logical counts must exactly match the allocation element counts. Byte
    /// spans are derived from those allocations and cannot be supplied by the
    /// caller. `R` must be the engine row mirror with compile-time field-layout
    /// pins matching [`PgaccelResidentGeometryRow`].
    #[allow(clippy::too_many_arguments)] // reason: arguments are the six frozen ABI lanes
    pub fn from_device_buffers<R>(
        coordinates: Option<&ExprDeviceBuffer<f64>>,
        bboxes: Option<&ExprDeviceBuffer<[f64; 4]>>,
        geometry_offsets: &ExprDeviceBuffer<u64>,
        ring_offsets: Option<&ExprDeviceBuffer<u64>>,
        rows: Option<&ExprDeviceBuffer<R>>,
        nulls: Option<&ExprDeviceBuffer<u8>>,
        row_count: usize,
        coordinate_pair_count: usize,
        ring_count: usize,
    ) -> GpuResult<Self> {
        let coordinate_scalar_count = coordinate_pair_count.checked_mul(2).ok_or_else(|| {
            resident_builder_error(
                GpuStatusDetail::CapacityOverflow,
                "resident geometry coordinate scalar count overflow",
            )
        })?;
        let geometry_offset_count = row_count.checked_add(1).ok_or_else(|| {
            resident_builder_error(
                GpuStatusDetail::CapacityOverflow,
                "resident geometry offset count overflow",
            )
        })?;
        if row_count == 0
            || coordinates.map_or(0, ExprDeviceBuffer::len) != coordinate_scalar_count
            || bboxes.map_or(0, ExprDeviceBuffer::len) != row_count
            || geometry_offsets.len() != geometry_offset_count
            || ring_offsets.map_or(0, ExprDeviceBuffer::len) != ring_count
            || rows.map_or(0, ExprDeviceBuffer::len) != row_count
            || nulls.is_some_and(|buffer| buffer.len() != row_count)
        {
            return Err(resident_builder_error(
                GpuStatusDetail::ShapeMismatch,
                "resident geometry logical counts do not match device-buffer lengths",
            ));
        }
        if std::mem::size_of::<R>() != std::mem::size_of::<PgaccelResidentGeometryRow>()
            || std::mem::align_of::<R>() != std::mem::align_of::<PgaccelResidentGeometryRow>()
        {
            return Err(resident_builder_error(
                GpuStatusDetail::InvalidDescriptor,
                "resident geometry row buffer does not match the native row ABI",
            ));
        }

        let bboxes = bboxes.expect("row_count > 0 requires a bbox buffer");
        let rows = rows.expect("row_count > 0 requires a row buffer");
        Ok(Self::new(
            coordinates.map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr),
            bboxes.as_ptr().cast::<f64>(),
            geometry_offsets.as_ptr(),
            ring_offsets.map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr),
            rows.as_ptr().cast::<PgaccelResidentGeometryRow>(),
            nulls.map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr),
            coordinates.map_or(Ok(0), resident_buffer_bytes)?,
            resident_buffer_bytes(bboxes)?,
            resident_buffer_bytes(geometry_offsets)?,
            ring_offsets.map_or(Ok(0), resident_buffer_bytes)?,
            resident_buffer_bytes(rows)?,
            nulls.map_or(Ok(0), resident_buffer_bytes)?,
            row_count,
            coordinate_pair_count,
            ring_count,
        ))
    }
}

/// A row-aligned resident column or a one-row resident constant.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelResidentGeometryOperand {
    pub view: PgaccelResidentGeometryView,
    pub first_row: usize,
    pub row_stride: usize,
}

impl PgaccelResidentGeometryOperand {
    /// Build a row-aligned column operand (`row_stride = 1`).
    #[must_use]
    pub const fn column(view: PgaccelResidentGeometryView, first_row: usize) -> Self {
        Self {
            view,
            first_row,
            row_stride: 1,
        }
    }

    /// Build a one-row constant operand (`first_row = row_stride = 0`).
    #[must_use]
    pub const fn constant(view: PgaccelResidentGeometryView) -> Self {
        Self {
            view,
            first_row: 0,
            row_stride: 0,
        }
    }
}

/// Resident spatial operation tag. Discriminants mirror `pgaccel_spatial_predicate`.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentSpatialPredicate {
    Intersects = 0,
    Contains = 1,
    Within = 2,
    DWithin = 3,
    Distance = 4,
}

/// Native validation detail returned alongside a resident spatial status.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentSpatialDetail {
    None = 0,
    Contract = 1,
    Geometry = 2,
    SridMismatch = 3,
    ByteBudget = 4,
    TriState = 5,
    RecheckIndex = 6,
    RecheckPatch = 7,
}

impl TryFrom<i32> for ResidentSpatialDetail {
    type Error = i32;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Contract),
            2 => Ok(Self::Geometry),
            3 => Ok(Self::SridMismatch),
            4 => Ok(Self::ByteBudget),
            5 => Ok(Self::TriState),
            6 => Ok(Self::RecheckIndex),
            7 => Ok(Self::RecheckPatch),
            unknown => Err(unknown),
        }
    }
}

/// Frozen request descriptor for resident spatial evaluation.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelSpatialResidentRequest {
    pub abi_version: u32,
    pub flags: u32,
    pub predicate: ResidentSpatialPredicate,
    pub pad: u32,
    pub distance_threshold: f64,
    pub count: usize,
    pub max_referenced_bytes: usize,
    pub left: PgaccelResidentGeometryOperand,
    pub right: PgaccelResidentGeometryOperand,
    pub predicate_results: *mut i8,
    pub predicate_results_bytes: usize,
    pub distances: *mut f64,
    pub distances_bytes: usize,
    pub distance_uncertain: *mut u8,
    pub distance_uncertain_bytes: usize,
    pub output_capacity: usize,
}

impl PgaccelSpatialResidentRequest {
    /// Build a boolean predicate request with an `i8` tri-state device output.
    ///
    /// `distance_threshold` must be finite and non-negative for `DWithin` and
    /// zero for every other boolean predicate. Passing `Distance` here is a
    /// native contract error; use [`Self::distance`] for that operation.
    #[allow(clippy::too_many_arguments)] // reason: fields mirror the frozen C request
    #[must_use]
    pub const fn boolean(
        predicate: ResidentSpatialPredicate,
        distance_threshold: f64,
        count: usize,
        max_referenced_bytes: usize,
        left: PgaccelResidentGeometryOperand,
        right: PgaccelResidentGeometryOperand,
        predicate_results: *mut i8,
        predicate_results_bytes: usize,
        output_capacity: usize,
    ) -> Self {
        Self {
            abi_version: PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION,
            flags: 0,
            predicate,
            pad: 0,
            distance_threshold,
            count,
            max_referenced_bytes,
            left,
            right,
            predicate_results,
            predicate_results_bytes,
            distances: std::ptr::null_mut(),
            distances_bytes: 0,
            distance_uncertain: std::ptr::null_mut(),
            distance_uncertain_bytes: 0,
            output_capacity,
        }
    }

    /// Build a boolean request whose capacity and byte span come from `results`.
    pub fn boolean_device(
        predicate: ResidentSpatialPredicate,
        distance_threshold: f64,
        count: usize,
        max_referenced_bytes: usize,
        left: PgaccelResidentGeometryOperand,
        right: PgaccelResidentGeometryOperand,
        results: &ExprDeviceBuffer<i8>,
    ) -> GpuResult<Self> {
        if results.len() < count {
            return Err(resident_builder_error(
                GpuStatusDetail::ShapeMismatch,
                "resident spatial predicate output is shorter than the request count",
            ));
        }
        Ok(Self::boolean(
            predicate,
            distance_threshold,
            count,
            max_referenced_bytes,
            left,
            right,
            results.as_mut_ptr(),
            resident_buffer_bytes(results)?,
            results.len(),
        ))
    }

    /// Build a distance request with separate `f64` values and `u8` uncertainty outputs.
    #[allow(clippy::too_many_arguments)] // reason: fields mirror the frozen C request
    #[allow(dead_code)] // reason: frozen distance-request constructor pinned by ABI tests
    #[must_use]
    pub const fn distance(
        count: usize,
        max_referenced_bytes: usize,
        left: PgaccelResidentGeometryOperand,
        right: PgaccelResidentGeometryOperand,
        distances: *mut f64,
        distances_bytes: usize,
        distance_uncertain: *mut u8,
        distance_uncertain_bytes: usize,
        output_capacity: usize,
    ) -> Self {
        Self {
            abi_version: PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION,
            flags: 0,
            predicate: ResidentSpatialPredicate::Distance,
            pad: 0,
            distance_threshold: 0.0,
            count,
            max_referenced_bytes,
            left,
            right,
            predicate_results: std::ptr::null_mut(),
            predicate_results_bytes: 0,
            distances,
            distances_bytes,
            distance_uncertain,
            distance_uncertain_bytes,
            output_capacity,
        }
    }

    /// Build a distance request from equally sized value and uncertainty buffers.
    #[allow(dead_code)] // reason: distance output is not planner-exposed; retained as a typed ABI builder
    pub fn distance_device(
        count: usize,
        max_referenced_bytes: usize,
        left: PgaccelResidentGeometryOperand,
        right: PgaccelResidentGeometryOperand,
        distances: &ExprDeviceBuffer<f64>,
        distance_uncertain: &ExprDeviceBuffer<u8>,
    ) -> GpuResult<Self> {
        if distances.len() < count || distance_uncertain.len() != distances.len() {
            return Err(resident_builder_error(
                GpuStatusDetail::ShapeMismatch,
                "resident spatial distance outputs have inconsistent capacities",
            ));
        }
        Ok(Self::distance(
            count,
            max_referenced_bytes,
            left,
            right,
            distances.as_mut_ptr(),
            resident_buffer_bytes(distances)?,
            distance_uncertain.as_mut_ptr(),
            resident_buffer_bytes(distance_uncertain)?,
            distances.len(),
        ))
    }
}

/// Caller-owned exact device scratch shared by one resident launch chain.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelSpatialWorkspace {
    pub abi_version: u32,
    pub flags: u32,
    pub control: *mut u8,
    pub control_bytes: usize,
    pub failure_flags: *mut u32,
    pub failure_flags_bytes: usize,
}

impl PgaccelSpatialWorkspace {
    #[must_use]
    pub const fn new(
        control: *mut u8,
        control_bytes: usize,
        failure_flags: *mut u32,
        failure_flags_bytes: usize,
    ) -> Self {
        Self {
            abi_version: PGACCEL_SPATIAL_WORKSPACE_ABI_VERSION,
            flags: 0,
            control,
            control_bytes,
            failure_flags,
            failure_flags_bytes,
        }
    }

    /// Bind the exact native workspace spans to preallocated device buffers.
    pub fn from_device_buffers(
        control: &ExprDeviceBuffer<u8>,
        failure_flags: &ExprDeviceBuffer<u32>,
    ) -> GpuResult<Self> {
        if control.len() != PGACCEL_SPATIAL_CONTROL_BYTES || failure_flags.len() != 1 {
            return Err(resident_builder_error(
                GpuStatusDetail::ShapeMismatch,
                "resident spatial workspace buffers have noncanonical lengths",
            ));
        }
        Ok(Self::new(
            control.as_mut_ptr(),
            resident_buffer_bytes(control)?,
            failure_flags.as_mut_ptr(),
            resident_buffer_bytes(failure_flags)?,
        ))
    }
}

/// Ordered tri-state compaction into a SQL filter mask and row-index list.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelSpatialRecheckCompactRequest {
    pub abi_version: u32,
    pub flags: u32,
    pub tri_state: *const i8,
    pub tri_state_bytes: usize,
    pub final_mask: *mut i8,
    pub final_mask_bytes: usize,
    pub uncertain_indices: *mut u64,
    pub uncertain_indices_bytes: usize,
    pub uncertain_count: *mut u64,
    pub uncertain_count_bytes: usize,
    pub row_count: usize,
    pub uncertain_capacity: usize,
}

impl PgaccelSpatialRecheckCompactRequest {
    /// Bind exact full-capacity compaction buffers. Zero-row chunks are skipped
    /// by the executor and are intentionally rejected here.
    pub fn from_device_buffers(
        tri_state: &ExprDeviceBuffer<i8>,
        final_mask: &ExprDeviceBuffer<i8>,
        uncertain_indices: &ExprDeviceBuffer<u64>,
        uncertain_count: &ExprDeviceBuffer<u64>,
        row_count: usize,
    ) -> GpuResult<Self> {
        if row_count == 0
            || row_count > PGACCEL_SPATIAL_MAX_CHUNK_ROWS
            || tri_state.len() != row_count
            || final_mask.len() != row_count
            || uncertain_indices.len() != row_count
            || uncertain_count.len() != 1
        {
            return Err(resident_builder_error(
                GpuStatusDetail::ShapeMismatch,
                "resident spatial compaction buffers do not match the exact chunk shape",
            ));
        }
        Ok(Self {
            abi_version: PGACCEL_SPATIAL_RECHECK_ABI_VERSION,
            flags: 0,
            tri_state: tri_state.as_ptr(),
            tri_state_bytes: resident_buffer_bytes(tri_state)?,
            final_mask: final_mask.as_mut_ptr(),
            final_mask_bytes: resident_buffer_bytes(final_mask)?,
            uncertain_indices: uncertain_indices.as_mut_ptr(),
            uncertain_indices_bytes: resident_buffer_bytes(uncertain_indices)?,
            uncertain_count: uncertain_count.as_mut_ptr(),
            uncertain_count_bytes: resident_buffer_bytes(uncertain_count)?,
            row_count,
            uncertain_capacity: row_count,
        })
    }
}

/// Ordered exact-result patch into a previously compacted SQL filter mask.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelSpatialRecheckPatchRequest {
    pub abi_version: u32,
    pub flags: u32,
    pub indices: *const u64,
    pub indices_bytes: usize,
    pub results: *const i8,
    pub results_bytes: usize,
    pub final_mask: *mut i8,
    pub final_mask_bytes: usize,
    pub row_count: usize,
    pub patch_count: usize,
}

impl PgaccelSpatialRecheckPatchRequest {
    /// Bind exact patch inputs and the full chunk mask. Empty patches are
    /// skipped by the executor and are intentionally rejected here.
    #[allow(dead_code)] // reason: runtime patch counts vary, so executor builds this frozen descriptor directly
    pub fn from_device_buffers(
        indices: &ExprDeviceBuffer<u64>,
        results: &ExprDeviceBuffer<i8>,
        final_mask: &ExprDeviceBuffer<i8>,
        row_count: usize,
        patch_count: usize,
    ) -> GpuResult<Self> {
        if row_count == 0
            || row_count > PGACCEL_SPATIAL_MAX_CHUNK_ROWS
            || patch_count == 0
            || patch_count > row_count
            || indices.len() != patch_count
            || results.len() != patch_count
            || final_mask.len() != row_count
        {
            return Err(resident_builder_error(
                GpuStatusDetail::ShapeMismatch,
                "resident spatial patch buffers do not match the exact recheck shape",
            ));
        }
        Ok(Self {
            abi_version: PGACCEL_SPATIAL_RECHECK_ABI_VERSION,
            flags: 0,
            indices: indices.as_ptr(),
            indices_bytes: resident_buffer_bytes(indices)?,
            results: results.as_ptr(),
            results_bytes: resident_buffer_bytes(results)?,
            final_mask: final_mask.as_mut_ptr(),
            final_mask_bytes: resident_buffer_bytes(final_mask)?,
            row_count,
            patch_count,
        })
    }
}

/// Allocation-free raw status retained across a resident-store borrow.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpatialResidentLaunchOutcome {
    status: i32,
    detail: i32,
}

#[cfg(any(test, feature = "pg_test"))]
#[must_use]
pub const fn test_injected_spatial_kernel_failure_outcome() -> SpatialResidentLaunchOutcome {
    SpatialResidentLaunchOutcome {
        status: PgaccelStatus::Error as i32,
        detail: ResidentSpatialDetail::None as i32,
    }
}

#[inline(always)]
fn capture_spatial_raw_outcome(
    launch: impl FnOnce(*mut i32) -> i32,
) -> SpatialResidentLaunchOutcome {
    let mut detail = ResidentSpatialDetail::None as i32;
    let status = launch(std::ptr::addr_of_mut!(detail));
    SpatialResidentLaunchOutcome { status, detail }
}

fn resident_spatial_result_for(
    operation: GpuOperation,
    status: PgaccelStatus,
    detail: i32,
) -> GpuResult<()> {
    if status.is_ok() {
        return if detail == ResidentSpatialDetail::None as i32 {
            Ok(())
        } else {
            Err(GpuError::with_detail(
                GpuErrorDomain::Spatial,
                operation,
                GpuStatusDetail::InvalidDescriptor,
                "successful resident spatial evaluation returned a nonzero detail code",
            ))
        };
    }

    if status != PgaccelStatus::InvalidArgument {
        return status_to_result(status, GpuErrorDomain::Spatial, operation);
    }

    let (mapped, message) = match ResidentSpatialDetail::try_from(detail) {
        Ok(ResidentSpatialDetail::None) => (
            GpuStatusDetail::InvalidDescriptor,
            "resident spatial evaluation rejected the request without a detail code",
        ),
        Ok(ResidentSpatialDetail::Contract) => (
            GpuStatusDetail::InvalidDescriptor,
            "resident spatial request contract violation",
        ),
        Ok(ResidentSpatialDetail::Geometry) => (
            GpuStatusDetail::InvalidDescriptor,
            "resident geometry descriptor is malformed",
        ),
        Ok(ResidentSpatialDetail::SridMismatch) => (
            GpuStatusDetail::ShapeMismatch,
            "resident spatial operands use different SRIDs",
        ),
        Ok(ResidentSpatialDetail::ByteBudget) => (
            GpuStatusDetail::CapacityOverflow,
            "resident spatial request exceeds its referenced-byte budget",
        ),
        Ok(ResidentSpatialDetail::TriState) => (
            GpuStatusDetail::InvalidDescriptor,
            "resident spatial evaluation produced a noncanonical tri-state value",
        ),
        Ok(ResidentSpatialDetail::RecheckIndex) => (
            GpuStatusDetail::ShapeMismatch,
            "resident spatial recheck indices are not strictly ordered and in range",
        ),
        Ok(ResidentSpatialDetail::RecheckPatch) => (
            GpuStatusDetail::InvalidDescriptor,
            "resident spatial exact-recheck patch value is noncanonical",
        ),
        Err(_) => (
            GpuStatusDetail::InvalidDescriptor,
            "resident spatial evaluation returned an unknown detail code",
        ),
    };
    Err(GpuError::with_detail(
        GpuErrorDomain::Spatial,
        operation,
        mapped,
        message,
    ))
}

fn resident_spatial_result(status: PgaccelStatus, detail: i32) -> GpuResult<()> {
    resident_spatial_result_for(RESIDENT_SPATIAL_OPERATION, status, detail)
}

fn resident_spatial_outcome_result(
    outcome: SpatialResidentLaunchOutcome,
    symbol: &'static str,
    operation: GpuOperation,
) -> GpuResult<()> {
    let status = bridge::convert_status(symbol, outcome.status);
    resident_spatial_result_for(operation, status, outcome.detail)
}

/// Construct the process-local device queue before acquiring a resident-store
/// dispatch borrow. Raw spatial launches never initialize the queue lazily.
pub fn prepare_spatial_resident() -> GpuResult<()> {
    crate::ensure_backend_exit_callback();
    // SAFETY: pgaccel_init is process-idempotent and owns queue construction.
    let status = unsafe { bridge::pgaccel_init() };
    status_to_result(status, GpuErrorDomain::Spatial, GpuOperation::Init)
}

/// Submit one resident spatial evaluation and retain its raw POD outcome.
/// This function performs no status conversion, tracing, counter updates,
/// allocation, or device-to-host copy.
///
/// # Safety
///
/// Every request/workspace span must satisfy the native contract, remain live
/// through the synchronous launch, and belong to the already-prepared queue.
#[must_use]
#[inline]
pub unsafe fn spatial_eval_resident_launch(
    request: &PgaccelSpatialResidentRequest,
    workspace: &PgaccelSpatialWorkspace,
) -> SpatialResidentLaunchOutcome {
    capture_spatial_raw_outcome(|detail| {
        // SAFETY: the caller upholds the native pointer/span and queue contract.
        unsafe {
            bridge::pgaccel_spatial_eval_resident_launch_raw(
                std::ptr::from_ref(request),
                std::ptr::from_ref(workspace),
                detail,
            )
        }
    })
}

/// Map an evaluation launch outcome after releasing the resident-store borrow.
pub fn spatial_eval_resident_launch_result(outcome: SpatialResidentLaunchOutcome) -> GpuResult<()> {
    resident_spatial_outcome_result(
        outcome,
        "pgaccel_spatial_eval_resident_launch",
        RESIDENT_SPATIAL_OPERATION,
    )
}

/// Submit ordered uncertainty compaction as the second half of the evaluation
/// chain. The native sticky failure word is preserved.
///
/// # Safety
///
/// The request/workspace must satisfy the native exact-span contract and use
/// the same workspace as the immediately preceding evaluation launch.
#[must_use]
#[inline]
pub unsafe fn spatial_recheck_compact_launch(
    request: &PgaccelSpatialRecheckCompactRequest,
    workspace: &PgaccelSpatialWorkspace,
) -> SpatialResidentLaunchOutcome {
    capture_spatial_raw_outcome(|detail| {
        // SAFETY: the caller upholds the native pointer/span and chain contract.
        unsafe {
            bridge::pgaccel_spatial_recheck_compact_launch_raw(
                std::ptr::from_ref(request),
                std::ptr::from_ref(workspace),
                detail,
            )
        }
    })
}

/// Map a compaction launch outcome after releasing the resident-store borrow.
pub fn spatial_recheck_compact_launch_result(
    outcome: SpatialResidentLaunchOutcome,
) -> GpuResult<()> {
    resident_spatial_outcome_result(
        outcome,
        "pgaccel_spatial_recheck_compact_launch",
        RESIDENT_SPATIAL_COMPACT_OPERATION,
    )
}

/// Submit ordered exact-result patching as a new launch chain after the first
/// workspace finish.
///
/// # Safety
///
/// The request/workspace must satisfy the native exact-span contract and every
/// allocation must remain live through the synchronous launch.
#[must_use]
#[inline]
pub unsafe fn spatial_recheck_patch_launch(
    request: &PgaccelSpatialRecheckPatchRequest,
    workspace: &PgaccelSpatialWorkspace,
) -> SpatialResidentLaunchOutcome {
    capture_spatial_raw_outcome(|detail| {
        // SAFETY: the caller upholds the native pointer/span contract.
        unsafe {
            bridge::pgaccel_spatial_recheck_patch_launch_raw(
                std::ptr::from_ref(request),
                std::ptr::from_ref(workspace),
                detail,
            )
        }
    })
}

/// Map a patch launch outcome after the native call returns.
pub fn spatial_recheck_patch_launch_result(outcome: SpatialResidentLaunchOutcome) -> GpuResult<()> {
    resident_spatial_outcome_result(
        outcome,
        "pgaccel_spatial_recheck_patch_launch",
        RESIDENT_SPATIAL_PATCH_OPERATION,
    )
}

/// Perform the sole sticky-status D2H read for the current launch chain and
/// map it after resident input borrows have been released.
///
/// # Safety
///
/// `workspace` must satisfy the native exact-span contract and remain live
/// through the synchronous finish call.
pub unsafe fn spatial_workspace_finish(workspace: &PgaccelSpatialWorkspace) -> GpuResult<()> {
    let outcome = capture_spatial_raw_outcome(|detail| {
        // SAFETY: the caller upholds the native workspace pointer/span contract.
        unsafe {
            bridge::pgaccel_spatial_workspace_finish_raw(std::ptr::from_ref(workspace), detail)
        }
    });
    resident_spatial_outcome_result(
        outcome,
        "pgaccel_spatial_workspace_finish",
        RESIDENT_SPATIAL_FINISH_OPERATION,
    )
}

/// Execute a resident fp64 spatial request synchronously.
///
/// Algorithmic uncertainty remains in the caller-provided device output.
/// Runtime, allocation, descriptor, SRID, and byte-budget failures are hard
/// [`GpuError`] values and never synthesize output classifications.
///
/// # Safety
/// Every non-null lane and output pointer in `request` must reference its
/// explicitly declared byte span in device or shared USM belonging to the
/// active pg_accel context. Input and output spans must obey the native
/// non-overlap contract and remain alive until this synchronous call returns.
#[allow(dead_code)] // reason: legacy one-shot ABI wrapper retained for direct native conformance
pub unsafe fn spatial_eval_resident(request: &PgaccelSpatialResidentRequest) -> GpuResult<()> {
    let mut detail = ResidentSpatialDetail::None as i32;
    // SAFETY: the caller upholds the complete resident pointer/count contract.
    let status = unsafe {
        bridge::pgaccel_spatial_eval_resident_ex(
            std::ptr::from_ref(request),
            std::ptr::from_mut(&mut detail),
        )
    };
    resident_spatial_result(status, detail)
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<PgaccelResidentGeometryRow>() == 24);
    assert!(std::mem::align_of::<PgaccelResidentGeometryRow>() == 8);
    assert!(std::mem::size_of::<PgaccelResidentGeometryView>() == 128);
    assert!(std::mem::size_of::<PgaccelResidentGeometryOperand>() == 144);
    assert!(std::mem::size_of::<PgaccelSpatialResidentRequest>() == 384);
    assert!(std::mem::size_of::<PgaccelSpatialWorkspace>() == 40);
    assert!(std::mem::size_of::<PgaccelSpatialRecheckCompactRequest>() == 88);
    assert!(std::mem::size_of::<PgaccelSpatialRecheckPatchRequest>() == 72);
    assert!(std::mem::size_of::<SpatialResidentLaunchOutcome>() == 8);
    assert!(std::mem::align_of::<SpatialResidentLaunchOutcome>() == 4);
};

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // reason: ABI tests stay adjacent to the frozen mirrors
mod resident_spatial_tests {
    use super::*;

    fn empty_view() -> PgaccelResidentGeometryView {
        PgaccelResidentGeometryView::new(
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            0,
            1,
            0,
            0,
        )
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn resident_spatial_layout_matches_c_header() {
        use std::mem::{align_of, offset_of, size_of};

        assert_eq!(size_of::<PgaccelResidentGeometryRow>(), 24);
        assert_eq!(align_of::<PgaccelResidentGeometryRow>(), 8);
        assert_eq!(offset_of!(PgaccelResidentGeometryRow, geom_type), 0);
        assert_eq!(offset_of!(PgaccelResidentGeometryRow, srid), 4);
        assert_eq!(offset_of!(PgaccelResidentGeometryRow, first_ring), 8);
        assert_eq!(offset_of!(PgaccelResidentGeometryRow, ring_count), 16);
        assert_eq!(offset_of!(PgaccelResidentGeometryRow, flags), 20);

        assert_eq!(size_of::<PgaccelResidentGeometryView>(), 128);
        assert_eq!(align_of::<PgaccelResidentGeometryView>(), 8);
        assert_eq!(offset_of!(PgaccelResidentGeometryView, abi_version), 0);
        assert_eq!(offset_of!(PgaccelResidentGeometryView, flags), 4);
        assert_eq!(offset_of!(PgaccelResidentGeometryView, coordinates), 8);
        assert_eq!(offset_of!(PgaccelResidentGeometryView, bboxes), 16);
        assert_eq!(
            offset_of!(PgaccelResidentGeometryView, geometry_offsets),
            24
        );
        assert_eq!(offset_of!(PgaccelResidentGeometryView, ring_offsets), 32);
        assert_eq!(offset_of!(PgaccelResidentGeometryView, rows), 40);
        assert_eq!(offset_of!(PgaccelResidentGeometryView, nulls), 48);
        assert_eq!(
            offset_of!(PgaccelResidentGeometryView, coordinates_bytes),
            56
        );
        assert_eq!(offset_of!(PgaccelResidentGeometryView, bboxes_bytes), 64);
        assert_eq!(
            offset_of!(PgaccelResidentGeometryView, geometry_offsets_bytes),
            72
        );
        assert_eq!(
            offset_of!(PgaccelResidentGeometryView, ring_offsets_bytes),
            80
        );
        assert_eq!(offset_of!(PgaccelResidentGeometryView, rows_bytes), 88);
        assert_eq!(offset_of!(PgaccelResidentGeometryView, nulls_bytes), 96);
        assert_eq!(offset_of!(PgaccelResidentGeometryView, row_count), 104);
        assert_eq!(
            offset_of!(PgaccelResidentGeometryView, coordinate_pair_count),
            112
        );
        assert_eq!(offset_of!(PgaccelResidentGeometryView, ring_count), 120);

        assert_eq!(size_of::<PgaccelResidentGeometryOperand>(), 144);
        assert_eq!(align_of::<PgaccelResidentGeometryOperand>(), 8);
        assert_eq!(offset_of!(PgaccelResidentGeometryOperand, view), 0);
        assert_eq!(offset_of!(PgaccelResidentGeometryOperand, first_row), 128);
        assert_eq!(offset_of!(PgaccelResidentGeometryOperand, row_stride), 136);

        assert_eq!(size_of::<PgaccelSpatialResidentRequest>(), 384);
        assert_eq!(align_of::<PgaccelSpatialResidentRequest>(), 8);
        assert_eq!(offset_of!(PgaccelSpatialResidentRequest, abi_version), 0);
        assert_eq!(offset_of!(PgaccelSpatialResidentRequest, flags), 4);
        assert_eq!(offset_of!(PgaccelSpatialResidentRequest, predicate), 8);
        assert_eq!(offset_of!(PgaccelSpatialResidentRequest, pad), 12);
        assert_eq!(
            offset_of!(PgaccelSpatialResidentRequest, distance_threshold),
            16
        );
        assert_eq!(offset_of!(PgaccelSpatialResidentRequest, count), 24);
        assert_eq!(
            offset_of!(PgaccelSpatialResidentRequest, max_referenced_bytes),
            32
        );
        assert_eq!(offset_of!(PgaccelSpatialResidentRequest, left), 40);
        assert_eq!(offset_of!(PgaccelSpatialResidentRequest, right), 184);
        assert_eq!(
            offset_of!(PgaccelSpatialResidentRequest, predicate_results),
            328
        );
        assert_eq!(
            offset_of!(PgaccelSpatialResidentRequest, predicate_results_bytes),
            336
        );
        assert_eq!(offset_of!(PgaccelSpatialResidentRequest, distances), 344);
        assert_eq!(
            offset_of!(PgaccelSpatialResidentRequest, distances_bytes),
            352
        );
        assert_eq!(
            offset_of!(PgaccelSpatialResidentRequest, distance_uncertain),
            360
        );
        assert_eq!(
            offset_of!(PgaccelSpatialResidentRequest, distance_uncertain_bytes),
            368
        );
        assert_eq!(
            offset_of!(PgaccelSpatialResidentRequest, output_capacity),
            376
        );

        assert_eq!(size_of::<PgaccelSpatialWorkspace>(), 40);
        assert_eq!(align_of::<PgaccelSpatialWorkspace>(), 8);
        assert_eq!(offset_of!(PgaccelSpatialWorkspace, abi_version), 0);
        assert_eq!(offset_of!(PgaccelSpatialWorkspace, flags), 4);
        assert_eq!(offset_of!(PgaccelSpatialWorkspace, control), 8);
        assert_eq!(offset_of!(PgaccelSpatialWorkspace, control_bytes), 16);
        assert_eq!(offset_of!(PgaccelSpatialWorkspace, failure_flags), 24);
        assert_eq!(offset_of!(PgaccelSpatialWorkspace, failure_flags_bytes), 32);

        assert_eq!(size_of::<PgaccelSpatialRecheckCompactRequest>(), 88);
        assert_eq!(align_of::<PgaccelSpatialRecheckCompactRequest>(), 8);
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckCompactRequest, abi_version),
            0
        );
        assert_eq!(offset_of!(PgaccelSpatialRecheckCompactRequest, flags), 4);
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckCompactRequest, tri_state),
            8
        );
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckCompactRequest, tri_state_bytes),
            16
        );
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckCompactRequest, final_mask),
            24
        );
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckCompactRequest, final_mask_bytes),
            32
        );
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckCompactRequest, uncertain_indices),
            40
        );
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckCompactRequest, uncertain_indices_bytes),
            48
        );
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckCompactRequest, uncertain_count),
            56
        );
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckCompactRequest, uncertain_count_bytes),
            64
        );
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckCompactRequest, row_count),
            72
        );
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckCompactRequest, uncertain_capacity),
            80
        );

        assert_eq!(size_of::<PgaccelSpatialRecheckPatchRequest>(), 72);
        assert_eq!(align_of::<PgaccelSpatialRecheckPatchRequest>(), 8);
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckPatchRequest, abi_version),
            0
        );
        assert_eq!(offset_of!(PgaccelSpatialRecheckPatchRequest, flags), 4);
        assert_eq!(offset_of!(PgaccelSpatialRecheckPatchRequest, indices), 8);
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckPatchRequest, indices_bytes),
            16
        );
        assert_eq!(offset_of!(PgaccelSpatialRecheckPatchRequest, results), 24);
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckPatchRequest, results_bytes),
            32
        );
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckPatchRequest, final_mask),
            40
        );
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckPatchRequest, final_mask_bytes),
            48
        );
        assert_eq!(offset_of!(PgaccelSpatialRecheckPatchRequest, row_count), 56);
        assert_eq!(
            offset_of!(PgaccelSpatialRecheckPatchRequest, patch_count),
            64
        );

        assert_eq!(size_of::<SpatialResidentLaunchOutcome>(), 8);
        assert_eq!(align_of::<SpatialResidentLaunchOutcome>(), 4);
        assert_eq!(offset_of!(SpatialResidentLaunchOutcome, status), 0);
        assert_eq!(offset_of!(SpatialResidentLaunchOutcome, detail), 4);
    }

    #[test]
    fn resident_spatial_discriminants_match_c_header() {
        assert_eq!(PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION, 2);
        assert_eq!(PGACCEL_RESIDENT_GEOMETRY_BBOX_VALID, 1);
        assert_eq!(ResidentSpatialPredicate::Intersects as i32, 0);
        assert_eq!(ResidentSpatialPredicate::Contains as i32, 1);
        assert_eq!(ResidentSpatialPredicate::Within as i32, 2);
        assert_eq!(ResidentSpatialPredicate::DWithin as i32, 3);
        assert_eq!(ResidentSpatialPredicate::Distance as i32, 4);
        assert_eq!(ResidentSpatialDetail::None as i32, 0);
        assert_eq!(ResidentSpatialDetail::Contract as i32, 1);
        assert_eq!(ResidentSpatialDetail::Geometry as i32, 2);
        assert_eq!(ResidentSpatialDetail::SridMismatch as i32, 3);
        assert_eq!(ResidentSpatialDetail::ByteBudget as i32, 4);
        assert_eq!(ResidentSpatialDetail::TriState as i32, 5);
        assert_eq!(ResidentSpatialDetail::RecheckIndex as i32, 6);
        assert_eq!(ResidentSpatialDetail::RecheckPatch as i32, 7);
        assert_eq!(ResidentSpatialDetail::try_from(8), Err(8));
        assert_eq!(PGACCEL_SPATIAL_WORKSPACE_ABI_VERSION, 1);
        assert_eq!(PGACCEL_SPATIAL_RECHECK_ABI_VERSION, 1);
        assert_eq!(PGACCEL_SPATIAL_CONTROL_BYTES, 384);
        assert_eq!(PGACCEL_SPATIAL_MAX_CHUNK_ROWS, 65_536);
    }

    #[test]
    fn resident_spatial_builders_pin_control_fields_and_outputs() {
        let view = empty_view();
        assert_eq!(view.abi_version, PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION);
        assert_eq!(view.flags, 0);

        let column = PgaccelResidentGeometryOperand::column(view, 7);
        assert_eq!(column.first_row, 7);
        assert_eq!(column.row_stride, 1);
        let constant = PgaccelResidentGeometryOperand::constant(view);
        assert_eq!(constant.first_row, 0);
        assert_eq!(constant.row_stride, 0);

        let predicate_output = std::ptr::NonNull::<i8>::dangling().as_ptr();
        let boolean = PgaccelSpatialResidentRequest::boolean(
            ResidentSpatialPredicate::DWithin,
            2.5,
            3,
            4_096,
            column,
            constant,
            predicate_output,
            3,
            3,
        );
        assert_eq!(boolean.abi_version, PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION);
        assert_eq!(boolean.flags, 0);
        assert_eq!(boolean.pad, 0);
        assert_eq!(boolean.predicate, ResidentSpatialPredicate::DWithin);
        assert_eq!(boolean.distance_threshold, 2.5);
        assert_eq!(boolean.predicate_results, predicate_output);
        assert_eq!(boolean.predicate_results_bytes, 3);
        assert!(boolean.distances.is_null());
        assert_eq!(boolean.distances_bytes, 0);
        assert!(boolean.distance_uncertain.is_null());
        assert_eq!(boolean.distance_uncertain_bytes, 0);

        let distances = std::ptr::NonNull::<f64>::dangling().as_ptr();
        let uncertainty = std::ptr::NonNull::<u8>::dangling().as_ptr();
        let distance = PgaccelSpatialResidentRequest::distance(
            3,
            4_096,
            column,
            constant,
            distances,
            3 * size_of::<f64>(),
            uncertainty,
            3,
            3,
        );
        assert_eq!(distance.predicate, ResidentSpatialPredicate::Distance);
        assert_eq!(distance.distance_threshold, 0.0);
        assert!(distance.predicate_results.is_null());
        assert_eq!(distance.predicate_results_bytes, 0);
        assert_eq!(distance.distances, distances);
        assert_eq!(distance.distances_bytes, 3 * size_of::<f64>());
        assert_eq!(distance.distance_uncertain, uncertainty);
        assert_eq!(distance.distance_uncertain_bytes, 3);

        let control = std::ptr::NonNull::<u8>::dangling().as_ptr();
        let failure_flags = std::ptr::NonNull::<u32>::dangling().as_ptr();
        let workspace = PgaccelSpatialWorkspace::new(
            control,
            PGACCEL_SPATIAL_CONTROL_BYTES,
            failure_flags,
            size_of::<u32>(),
        );
        assert_eq!(workspace.abi_version, PGACCEL_SPATIAL_WORKSPACE_ABI_VERSION);
        assert_eq!(workspace.flags, 0);
        assert_eq!(workspace.control, control);
        assert_eq!(workspace.control_bytes, PGACCEL_SPATIAL_CONTROL_BYTES);
        assert_eq!(workspace.failure_flags, failure_flags);
        assert_eq!(workspace.failure_flags_bytes, size_of::<u32>());
    }

    #[test]
    fn resident_spatial_ok_requires_none_detail() {
        assert!(resident_spatial_result(PgaccelStatus::Ok, 0).is_ok());

        let Err(error) = resident_spatial_result(PgaccelStatus::Ok, 1) else {
            panic!("OK with a nonzero detail must be a hard output-contract error");
        };
        assert_eq!(error.domain, GpuErrorDomain::Spatial);
        assert_eq!(error.operation, RESIDENT_SPATIAL_OPERATION);
        assert_eq!(error.status, GpuStatusDetail::InvalidDescriptor);
        assert_eq!(
            error.detail,
            Some("successful resident spatial evaluation returned a nonzero detail code")
        );
    }

    #[test]
    fn resident_spatial_invalid_argument_details_remain_distinguishable() {
        let cases = [
            (
                ResidentSpatialDetail::None as i32,
                GpuStatusDetail::InvalidDescriptor,
                "resident spatial evaluation rejected the request without a detail code",
            ),
            (
                ResidentSpatialDetail::Contract as i32,
                GpuStatusDetail::InvalidDescriptor,
                "resident spatial request contract violation",
            ),
            (
                ResidentSpatialDetail::Geometry as i32,
                GpuStatusDetail::InvalidDescriptor,
                "resident geometry descriptor is malformed",
            ),
            (
                ResidentSpatialDetail::SridMismatch as i32,
                GpuStatusDetail::ShapeMismatch,
                "resident spatial operands use different SRIDs",
            ),
            (
                ResidentSpatialDetail::ByteBudget as i32,
                GpuStatusDetail::CapacityOverflow,
                "resident spatial request exceeds its referenced-byte budget",
            ),
            (
                ResidentSpatialDetail::TriState as i32,
                GpuStatusDetail::InvalidDescriptor,
                "resident spatial evaluation produced a noncanonical tri-state value",
            ),
            (
                ResidentSpatialDetail::RecheckIndex as i32,
                GpuStatusDetail::ShapeMismatch,
                "resident spatial recheck indices are not strictly ordered and in range",
            ),
            (
                ResidentSpatialDetail::RecheckPatch as i32,
                GpuStatusDetail::InvalidDescriptor,
                "resident spatial exact-recheck patch value is noncanonical",
            ),
            (
                99,
                GpuStatusDetail::InvalidDescriptor,
                "resident spatial evaluation returned an unknown detail code",
            ),
        ];

        for (detail, expected_status, expected_message) in cases {
            let Err(error) = resident_spatial_result(PgaccelStatus::InvalidArgument, detail) else {
                panic!("invalid argument detail {detail} must be a hard error");
            };
            assert_eq!(error.domain, GpuErrorDomain::Spatial);
            assert_eq!(error.operation, RESIDENT_SPATIAL_OPERATION);
            assert_eq!(error.status, expected_status);
            assert_eq!(error.detail, Some(expected_message));
        }
    }

    #[test]
    fn resident_spatial_runtime_statuses_are_hard_errors() {
        let cases = [
            (PgaccelStatus::Error, GpuStatusDetail::ExecutionFailed),
            (
                PgaccelStatus::ErrorUnsupported,
                GpuStatusDetail::Unsupported,
            ),
            (PgaccelStatus::ErrorOom, GpuStatusDetail::OutOfMemory),
            (PgaccelStatus::ErrorTimeout, GpuStatusDetail::Timeout),
            (PgaccelStatus::ErrorNoDevice, GpuStatusDetail::NoDevice),
        ];
        for (status, expected) in cases {
            let Err(error) = resident_spatial_result(status, 0) else {
                panic!("non-OK status {status:?} must be a hard error");
            };
            assert_eq!(error.domain, GpuErrorDomain::Spatial);
            assert_eq!(error.operation, RESIDENT_SPATIAL_OPERATION);
            assert_eq!(error.status, expected);
        }
    }

    #[test]
    fn raw_launch_outcomes_are_mapped_only_by_post_borrow_helpers() {
        let ok = SpatialResidentLaunchOutcome {
            status: PgaccelStatus::Ok as i32,
            detail: ResidentSpatialDetail::None as i32,
        };
        assert!(spatial_eval_resident_launch_result(ok).is_ok());
        assert!(spatial_recheck_compact_launch_result(ok).is_ok());
        assert!(spatial_recheck_patch_launch_result(ok).is_ok());

        let contract = SpatialResidentLaunchOutcome {
            status: PgaccelStatus::InvalidArgument as i32,
            detail: ResidentSpatialDetail::Contract as i32,
        };
        let error = spatial_recheck_compact_launch_result(contract)
            .expect_err("contract failure must stay hard");
        assert_eq!(error.domain, GpuErrorDomain::Spatial);
        assert_eq!(error.operation, RESIDENT_SPATIAL_COMPACT_OPERATION);
        assert_eq!(error.status, GpuStatusDetail::InvalidDescriptor);

        let unknown = SpatialResidentLaunchOutcome {
            status: 99,
            detail: ResidentSpatialDetail::None as i32,
        };
        let error = spatial_eval_resident_launch_result(unknown)
            .expect_err("unknown raw status must fail closed");
        assert_eq!(error.status, GpuStatusDetail::ExecutionFailed);

        let error =
            spatial_eval_resident_launch_result(test_injected_spatial_kernel_failure_outcome())
                .expect_err("test-injected kernel status must use the hard failure mapper");
        assert_eq!(error.domain, GpuErrorDomain::Spatial);
        assert_eq!(error.operation, RESIDENT_SPATIAL_OPERATION);
        assert_eq!(error.status, GpuStatusDetail::ExecutionFailed);
    }

    #[test]
    fn raw_launch_outcome_capture_allocates_nothing() {
        crate::engine::residency::begin_test_allocation_count();
        let outcome = capture_spatial_raw_outcome(|detail| {
            // SAFETY: `capture_spatial_raw_outcome` supplies a valid stack
            // pointer for the duration of this synchronous callback.
            unsafe { detail.write(ResidentSpatialDetail::RecheckIndex as i32) };
            PgaccelStatus::InvalidArgument as i32
        });
        let allocation_count = crate::engine::residency::finish_test_allocation_count();

        assert_eq!(allocation_count, 0);
        assert_eq!(outcome.status, PgaccelStatus::InvalidArgument as i32);
        assert_eq!(outcome.detail, ResidentSpatialDetail::RecheckIndex as i32);
    }
}
