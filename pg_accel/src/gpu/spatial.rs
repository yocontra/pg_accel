use super::{
    GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail, PgaccelGeometry,
    PgaccelStatus, bridge, status_to_result,
};

/// Frozen ABI version for resident fp64 geometry descriptors.
#[allow(dead_code)] // reason: resident executor caller lands in the parallel integration slice
pub const PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION: u32 = 1;

/// Resident geometry row flag indicating a populated `[xmin, ymin, xmax, ymax]` bbox.
#[allow(dead_code)] // reason: row construction remains owned by the residency integration slice
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
    pub row_count: usize,
    pub coordinate_pair_count: usize,
    pub ring_count: usize,
}

impl PgaccelResidentGeometryView {
    /// Build a frozen-v1 view over already-resident geometry lanes.
    #[allow(clippy::too_many_arguments)] // reason: fields mirror the frozen C descriptor
    #[allow(dead_code)] // reason: resident executor caller lands in the parallel integration slice
    #[must_use]
    pub const fn new(
        coordinates: *const f64,
        bboxes: *const f64,
        geometry_offsets: *const u64,
        ring_offsets: *const u64,
        rows: *const PgaccelResidentGeometryRow,
        nulls: *const u8,
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
            row_count,
            coordinate_pair_count,
            ring_count,
        }
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
    #[allow(dead_code)] // reason: resident executor caller lands in the parallel integration slice
    #[must_use]
    pub const fn column(view: PgaccelResidentGeometryView, first_row: usize) -> Self {
        Self {
            view,
            first_row,
            row_stride: 1,
        }
    }

    /// Build a one-row constant operand (`first_row = row_stride = 0`).
    #[allow(dead_code)] // reason: resident executor caller lands in the parallel integration slice
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
#[allow(dead_code)] // reason: resident executor caller lands in the parallel integration slice
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
    pub distances: *mut f64,
    pub distance_uncertain: *mut u8,
    pub output_capacity: usize,
}

impl PgaccelSpatialResidentRequest {
    /// Build a boolean predicate request with an `i8` tri-state device output.
    ///
    /// `distance_threshold` must be finite and non-negative for `DWithin` and
    /// zero for every other boolean predicate. Passing `Distance` here is a
    /// native contract error; use [`Self::distance`] for that operation.
    #[allow(clippy::too_many_arguments)] // reason: fields mirror the frozen C request
    #[allow(dead_code)] // reason: resident executor caller lands in the parallel integration slice
    #[must_use]
    pub const fn boolean(
        predicate: ResidentSpatialPredicate,
        distance_threshold: f64,
        count: usize,
        max_referenced_bytes: usize,
        left: PgaccelResidentGeometryOperand,
        right: PgaccelResidentGeometryOperand,
        predicate_results: *mut i8,
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
            distances: std::ptr::null_mut(),
            distance_uncertain: std::ptr::null_mut(),
            output_capacity,
        }
    }

    /// Build a distance request with separate `f64` values and `u8` uncertainty outputs.
    #[allow(clippy::too_many_arguments)] // reason: fields mirror the frozen C request
    #[allow(dead_code)] // reason: resident executor caller lands in the parallel integration slice
    #[must_use]
    pub const fn distance(
        count: usize,
        max_referenced_bytes: usize,
        left: PgaccelResidentGeometryOperand,
        right: PgaccelResidentGeometryOperand,
        distances: *mut f64,
        distance_uncertain: *mut u8,
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
            distances,
            distance_uncertain,
            output_capacity,
        }
    }
}

const RESIDENT_SPATIAL_OPERATION: GpuOperation = GpuOperation::Kernel("spatial_eval_resident");

fn resident_spatial_result(status: PgaccelStatus, detail: i32) -> GpuResult<()> {
    if status.is_ok() {
        return if detail == ResidentSpatialDetail::None as i32 {
            Ok(())
        } else {
            Err(GpuError::with_detail(
                GpuErrorDomain::Spatial,
                RESIDENT_SPATIAL_OPERATION,
                GpuStatusDetail::InvalidDescriptor,
                "successful resident spatial evaluation returned a nonzero detail code",
            ))
        };
    }

    if status != PgaccelStatus::InvalidArgument {
        return status_to_result(status, GpuErrorDomain::Spatial, RESIDENT_SPATIAL_OPERATION);
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
        Err(_) => (
            GpuStatusDetail::InvalidDescriptor,
            "resident spatial evaluation returned an unknown detail code",
        ),
    };
    Err(GpuError::with_detail(
        GpuErrorDomain::Spatial,
        RESIDENT_SPATIAL_OPERATION,
        mapped,
        message,
    ))
}

/// Execute a resident fp64 spatial request synchronously.
///
/// Algorithmic uncertainty remains in the caller-provided device output.
/// Runtime, allocation, descriptor, SRID, and byte-budget failures are hard
/// [`GpuError`] values and never synthesize output classifications.
///
/// # Safety
/// Every non-null lane and output pointer in `request` must reference the
/// stated number of elements in device or shared USM belonging to the active
/// pg_accel context. Input and output spans must obey the native non-overlap
/// contract and remain alive until this synchronous call returns.
#[allow(dead_code)] // reason: resident executor caller lands in the parallel integration slice
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
    assert!(std::mem::size_of::<PgaccelResidentGeometryView>() == 80);
    assert!(std::mem::size_of::<PgaccelResidentGeometryOperand>() == 96);
    assert!(std::mem::size_of::<PgaccelSpatialResidentRequest>() == 264);
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

        assert_eq!(size_of::<PgaccelResidentGeometryView>(), 80);
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
        assert_eq!(offset_of!(PgaccelResidentGeometryView, row_count), 56);
        assert_eq!(
            offset_of!(PgaccelResidentGeometryView, coordinate_pair_count),
            64
        );
        assert_eq!(offset_of!(PgaccelResidentGeometryView, ring_count), 72);

        assert_eq!(size_of::<PgaccelResidentGeometryOperand>(), 96);
        assert_eq!(align_of::<PgaccelResidentGeometryOperand>(), 8);
        assert_eq!(offset_of!(PgaccelResidentGeometryOperand, view), 0);
        assert_eq!(offset_of!(PgaccelResidentGeometryOperand, first_row), 80);
        assert_eq!(offset_of!(PgaccelResidentGeometryOperand, row_stride), 88);

        assert_eq!(size_of::<PgaccelSpatialResidentRequest>(), 264);
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
        assert_eq!(offset_of!(PgaccelSpatialResidentRequest, right), 136);
        assert_eq!(
            offset_of!(PgaccelSpatialResidentRequest, predicate_results),
            232
        );
        assert_eq!(offset_of!(PgaccelSpatialResidentRequest, distances), 240);
        assert_eq!(
            offset_of!(PgaccelSpatialResidentRequest, distance_uncertain),
            248
        );
        assert_eq!(
            offset_of!(PgaccelSpatialResidentRequest, output_capacity),
            256
        );
    }

    #[test]
    fn resident_spatial_discriminants_match_c_header() {
        assert_eq!(PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION, 1);
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
        assert_eq!(ResidentSpatialDetail::try_from(5), Err(5));
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
        );
        assert_eq!(boolean.abi_version, PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION);
        assert_eq!(boolean.flags, 0);
        assert_eq!(boolean.pad, 0);
        assert_eq!(boolean.predicate, ResidentSpatialPredicate::DWithin);
        assert_eq!(boolean.distance_threshold, 2.5);
        assert_eq!(boolean.predicate_results, predicate_output);
        assert!(boolean.distances.is_null());
        assert!(boolean.distance_uncertain.is_null());

        let distances = std::ptr::NonNull::<f64>::dangling().as_ptr();
        let uncertainty = std::ptr::NonNull::<u8>::dangling().as_ptr();
        let distance = PgaccelSpatialResidentRequest::distance(
            3,
            4_096,
            column,
            constant,
            distances,
            uncertainty,
            3,
        );
        assert_eq!(distance.predicate, ResidentSpatialPredicate::Distance);
        assert_eq!(distance.distance_threshold, 0.0);
        assert!(distance.predicate_results.is_null());
        assert_eq!(distance.distances, distances);
        assert_eq!(distance.distance_uncertain, uncertainty);
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
}

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
