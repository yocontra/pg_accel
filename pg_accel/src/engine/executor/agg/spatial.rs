//! Store-neutral preparation and accounting for resident spatial aggregates.

#![allow(dead_code)] // reason: descriptor dispatch consumes this bounded checkpoint next

use std::any::Any;

use pgrx::pg_sys;

use super::artifact::{DescriptorAggArtifact, PreparedAggArtifact, prepare_spatial_base_artifact};
use crate::engine::ffi::syscache::{PostgisCatalogIdentity, resolve_postgis_catalog};
use crate::engine::residency::{
    DerivedArtifact, ResidentByteAccounting, ResidentColumnRef, ResidentColumnView,
    ResidentDispatchBundle, ResidentGeometryColumn, ResidentGeometryExactSnapshot,
    ResidentInputBundle, ResidentLoadError, StagedTransformPreflight, StagedTransformWorkspace,
    materialize_resident_geometry_constant, resident_geometry_exact_snapshot_words,
    snapshot_resident_geometry_exact,
};
use crate::engine::spec::{
    AggQuerySpec, ColumnRef, FilterSpec, GroupKeySource, JoinMultiplicity, ScalarValue,
    SpatialOperand, SpatialPredicateKind, SpatialValueKind, SpatialValueMetadata,
};
use crate::gpu::{
    ExprDeviceBuffer, PGACCEL_SPATIAL_CONTROL_BYTES, PGACCEL_SPATIAL_MAX_CHUNK_ROWS,
    PGACCEL_SPATIAL_RECHECK_ABI_VERSION, PgaccelResidentGeometryOperand,
    PgaccelResidentGeometryView, PgaccelSpatialRecheckCompactRequest,
    PgaccelSpatialRecheckPatchRequest, PgaccelSpatialResidentRequest, PgaccelSpatialWorkspace,
    ResidentSpatialPredicate, SpatialResidentLaunchOutcome, prepare_spatial_resident,
    spatial_eval_resident_launch, spatial_eval_resident_launch_result,
    spatial_recheck_compact_launch, spatial_recheck_compact_launch_result,
    spatial_recheck_patch_launch, spatial_recheck_patch_launch_result, spatial_workspace_finish,
};

const GEOMETRY_ROW_BYTES: u64 = 24;
const GEOMETRY_BBOX_BYTES: u64 = 4 * 8;
const GEOMETRY_OFFSET_BYTES_PER_ROW: u64 = 2 * 8;
const POINT_COORDINATE_BYTES: u64 = 2 * 8;
const NULL_SIDECAR_BYTES_PER_ROW: u64 = 1;
const CONSTANT_OFFSET_WORDS: u64 = 2;
const CONSTANT_PREFIX_WORDS: u64 = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct SpatialDimensionShape {
    pub row_count: usize,
    pub group_key_count: usize,
    pub counted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SpatialBaseShape {
    pub fact_rows: usize,
    pub fact_group_key_count: usize,
    pub dimension_count: usize,
    pub dimensions: [SpatialDimensionShape; crate::engine::spec::abi::PGACCEL_GROUPED_AGG_MAX_DIMS],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SpatialConstantShape {
    pub exact_bytes: usize,
    pub coordinate_pairs: usize,
    pub ring_count: usize,
}

impl SpatialConstantShape {
    fn referenced_bytes(self) -> Option<u64> {
        checked_sum([
            checked_mul(self.coordinate_pairs, 2 * std::mem::size_of::<f64>())?,
            checked_mul(self.ring_count, std::mem::size_of::<u64>())?,
            GEOMETRY_ROW_BYTES,
            GEOMETRY_BBOX_BYTES,
            GEOMETRY_OFFSET_BYTES_PER_ROW,
        ])
    }

    fn device_bytes(self) -> Option<u64> {
        self.referenced_bytes()
    }

    fn host_bytes(self) -> Option<u64> {
        checked_sum([
            u64::try_from(self.exact_bytes).ok()?,
            CONSTANT_OFFSET_WORDS.checked_mul(8)?,
            CONSTANT_PREFIX_WORDS.checked_mul(8)?,
        ])
    }
}

/// Host storage allocated only after the persistent and transient charges are
/// both held. `exact_snapshot_words` is filled from resident GSERIALIZED bytes
/// during the staged snapshot callback; preflight never copies source values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SpatialSnapshotLayout {
    pub exact_snapshot_words: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SpatialPreflight {
    pub fact_rows: usize,
    pub chunk_count: usize,
    pub chunk_limit: usize,
    pub max_referenced_bytes: usize,
    pub published_accounting: ResidentByteAccounting,
    pub transient_accounting: ResidentByteAccounting,
    pub snapshot: SpatialSnapshotLayout,
    pub constant: SpatialConstantShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SpatialPreflightError {
    ZeroChunkLimit,
    ChunkLimitExceedsNativeMaximum,
    AccountingOverflow,
}

fn checked_mul(elements: usize, width: usize) -> Option<u64> {
    elements
        .checked_mul(width)
        .and_then(|bytes| u64::try_from(bytes).ok())
}

fn checked_sum<const N: usize>(terms: [u64; N]) -> Option<u64> {
    terms
        .into_iter()
        .try_fold(0_u64, |total, term| total.checked_add(term))
}

fn geometry_typmod_shape(metadata: SpatialValueMetadata) -> Option<(u32, i32)> {
    if metadata.kind != SpatialValueKind::Geometry
        || metadata.typmod < 0
        || metadata.typmod & 3 != 0
    {
        return None;
    }
    let geometry_type = u32::try_from((metadata.typmod & 0xfc) >> 2).ok()?;
    let srid = ((metadata.typmod & 0x0fff_ff00) - (metadata.typmod & 0x1000_0000)) >> 8;
    Some((geometry_type, srid))
}

fn padded_base_device_bytes(shape: &SpatialBaseShape) -> Option<u64> {
    let mut bytes = checked_mul(shape.fact_rows, shape.fact_group_key_count.checked_mul(4)?)?;
    for dimension in shape.dimensions.get(..shape.dimension_count)? {
        bytes = bytes.checked_add(checked_mul(
            dimension.row_count,
            dimension.group_key_count.checked_mul(4)?,
        )?)?;
        bytes = bytes.checked_add(checked_mul(shape.fact_rows, 4)?)?;
        bytes = bytes.checked_add(checked_mul(dimension.row_count, 1)?)?;
        if dimension.counted {
            bytes = bytes.checked_add(checked_mul(dimension.row_count, 8)?)?;
        }
    }
    Some(bytes)
}

fn chunk_device_bytes(rows: usize) -> Option<u64> {
    checked_sum([
        u64::try_from(PGACCEL_SPATIAL_CONTROL_BYTES).ok()?,
        u64::try_from(std::mem::size_of::<u32>()).ok()?,
        checked_mul(rows, std::mem::size_of::<i8>())?,
        checked_mul(rows, std::mem::size_of::<i8>())?,
        checked_mul(rows, std::mem::size_of::<u64>())?,
        u64::try_from(std::mem::size_of::<u64>()).ok()?,
    ])
}

pub(super) fn spatial_preflight(
    base: &SpatialBaseShape,
    constant: SpatialConstantShape,
    snapshot: SpatialSnapshotLayout,
    chunk_limit: usize,
) -> Result<SpatialPreflight, SpatialPreflightError> {
    if chunk_limit == 0 {
        return Err(SpatialPreflightError::ZeroChunkLimit);
    }
    if chunk_limit > PGACCEL_SPATIAL_MAX_CHUNK_ROWS {
        return Err(SpatialPreflightError::ChunkLimitExceedsNativeMaximum);
    }
    let chunk_count = base.fact_rows.div_ceil(chunk_limit);

    let base_device_bytes =
        padded_base_device_bytes(base).ok_or(SpatialPreflightError::AccountingOverflow)?;
    let final_mask_bytes = checked_mul(base.fact_rows, std::mem::size_of::<i8>())
        .ok_or(SpatialPreflightError::AccountingOverflow)?;
    let published_device_bytes = base_device_bytes
        .checked_add(final_mask_bytes)
        .ok_or(SpatialPreflightError::AccountingOverflow)?;

    let fixed_chunk_scratch =
        chunk_device_bytes(0).and_then(|bytes| bytes.checked_mul(u64::try_from(chunk_count).ok()?));
    let row_chunk_scratch = checked_mul(base.fact_rows, 10);
    let chunk_scratch = fixed_chunk_scratch.and_then(|fixed| fixed.checked_add(row_chunk_scratch?));
    let transient_device_bytes = base_device_bytes
        .checked_add(
            constant
                .device_bytes()
                .ok_or(SpatialPreflightError::AccountingOverflow)?,
        )
        .and_then(|bytes| bytes.checked_add(chunk_scratch?))
        .ok_or(SpatialPreflightError::AccountingOverflow)?;
    let transient_host_bytes = checked_sum([
        checked_mul(snapshot.exact_snapshot_words, std::mem::size_of::<u64>())
            .ok_or(SpatialPreflightError::AccountingOverflow)?,
        constant
            .host_bytes()
            .ok_or(SpatialPreflightError::AccountingOverflow)?,
        final_mask_bytes,
        checked_mul(base.fact_rows, std::mem::size_of::<u64>())
            .ok_or(SpatialPreflightError::AccountingOverflow)?,
        checked_mul(base.fact_rows, std::mem::size_of::<i8>())
            .ok_or(SpatialPreflightError::AccountingOverflow)?,
    ])
    .ok_or(SpatialPreflightError::AccountingOverflow)?;

    let largest_chunk = base.fact_rows.min(chunk_limit);
    let point_row_bytes = checked_sum([
        POINT_COORDINATE_BYTES,
        GEOMETRY_ROW_BYTES,
        GEOMETRY_BBOX_BYTES,
        GEOMETRY_OFFSET_BYTES_PER_ROW,
        NULL_SIDECAR_BYTES_PER_ROW,
    ])
    .ok_or(SpatialPreflightError::AccountingOverflow)?;
    let column_half = u64::try_from(largest_chunk)
        .ok()
        .and_then(|rows| rows.checked_mul(point_row_bytes))
        .ok_or(SpatialPreflightError::AccountingOverflow)?;
    let max_referenced_bytes = constant
        .referenced_bytes()
        .and_then(|bytes| bytes.max(column_half).checked_mul(2))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or(SpatialPreflightError::AccountingOverflow)?;

    Ok(SpatialPreflight {
        fact_rows: base.fact_rows,
        chunk_count,
        chunk_limit,
        max_referenced_bytes,
        published_accounting: ResidentByteAccounting {
            device_bytes: published_device_bytes,
            retained_host_exact_bytes: 0,
        },
        transient_accounting: ResidentByteAccounting {
            device_bytes: transient_device_bytes,
            retained_host_exact_bytes: transient_host_bytes,
        },
        snapshot,
        constant,
    })
}

#[derive(Debug, Clone)]
pub(super) struct SpatialTransformPlan {
    column: ColumnRef,
    column_request: ResidentColumnRef,
    column_is_left: bool,
    predicate: ResidentSpatialPredicate,
    distance_threshold: f64,
    exact_fn_oid: pg_sys::Oid,
    geometry_type_oid: pg_sys::Oid,
    catalog_fingerprint: Box<[i32]>,
    constant: SpatialConstantShape,
    max_groups: usize,
}

impl SpatialTransformPlan {
    pub fn new(
        spec: &AggQuerySpec,
        catalog: &PostgisCatalogIdentity,
        max_groups: usize,
    ) -> Result<Self, String> {
        let FilterSpec::Spatial {
            predicate,
            left,
            right,
            distance,
        } = &spec.fact_filter
        else {
            return Err("spatial transform plan requires a spatial fact filter".to_owned());
        };
        let (column, column_metadata, constant_metadata, constant_bytes, column_is_left) =
            match (left, right) {
                (
                    SpatialOperand::Column { column, metadata },
                    SpatialOperand::Constant {
                        metadata: constant_metadata,
                        bytes,
                    },
                ) => (*column, *metadata, *constant_metadata, bytes.as_ref(), true),
                (
                    SpatialOperand::Constant {
                        metadata: constant_metadata,
                        bytes,
                    },
                    SpatialOperand::Column { column, metadata },
                ) => (
                    *column,
                    *metadata,
                    *constant_metadata,
                    bytes.as_ref(),
                    false,
                ),
                _ => {
                    return Err(
                        "spatial aggregate requires exactly one column and one constant".to_owned(),
                    );
                }
            };
        if column.relation_oid != spec.fact_rel
            || column.type_oid != u32::from(catalog.geometry_type_oid)
        {
            return Err(
                "spatial aggregate column does not match the proved PostGIS type".to_owned(),
            );
        }
        let limits = crate::engine::cost::device_limits();
        let parsed = crate::engine::residency::validate_resident_geometry_value(
            constant_bytes,
            limits.resident_domain_max_exact_value_bytes,
        )?;
        let point_type = crate::engine::residency::RESIDENT_GEOMETRY_POINT;
        let polygon_type = crate::engine::residency::RESIDENT_GEOMETRY_POLYGON;
        if geometry_typmod_shape(column_metadata) != Some((point_type, parsed.srid))
            || column_metadata.srid != Some(parsed.srid)
            || parsed.srid == 0
            || constant_metadata.kind != SpatialValueKind::Geometry
            || constant_metadata.srid != Some(parsed.srid)
            || (constant_metadata.typmod != -1
                && geometry_typmod_shape(constant_metadata) != Some((polygon_type, parsed.srid)))
        {
            return Err(
                "spatial aggregate requires a same-SRID geometry POINT column and POLYGON constant"
                    .to_owned(),
            );
        }
        if parsed.geom_type != polygon_type
            || parsed.coordinate_pairs == 0
            || parsed.coordinate_pairs > limits.gpu_spatial_max_vertices_per_row
        {
            return Err("spatial aggregate constant is not a covered polygon".to_owned());
        }
        let covered_orientation = match predicate {
            SpatialPredicateKind::Intersects | SpatialPredicateKind::DWithin => true,
            SpatialPredicateKind::Contains => !column_is_left,
            SpatialPredicateKind::Within => column_is_left,
            SpatialPredicateKind::Disjoint
            | SpatialPredicateKind::Equals
            | SpatialPredicateKind::Touches
            | SpatialPredicateKind::Crosses
            | SpatialPredicateKind::Overlaps => false,
        };
        if !covered_orientation {
            return Err(
                "spatial aggregate predicate orientation is outside the proved lane".to_owned(),
            );
        }
        let predicate = match predicate {
            SpatialPredicateKind::Intersects => ResidentSpatialPredicate::Intersects,
            SpatialPredicateKind::Contains => ResidentSpatialPredicate::Contains,
            SpatialPredicateKind::Within => ResidentSpatialPredicate::Within,
            SpatialPredicateKind::DWithin => ResidentSpatialPredicate::DWithin,
            _ => return Err("spatial aggregate predicate has no resident kernel".to_owned()),
        };
        let distance_threshold = match distance {
            Some(ScalarValue::F64(value)) if predicate == ResidentSpatialPredicate::DWithin => {
                *value
            }
            None if predicate != ResidentSpatialPredicate::DWithin => 0.0,
            _ => return Err("spatial aggregate distance is not canonical float8".to_owned()),
        };
        let exact_fn_oid = match predicate {
            ResidentSpatialPredicate::Intersects => catalog.intersects_fn_oid,
            ResidentSpatialPredicate::Contains => catalog.contains_fn_oid,
            ResidentSpatialPredicate::Within => catalog.within_fn_oid,
            ResidentSpatialPredicate::DWithin => catalog.dwithin_fn_oid,
            ResidentSpatialPredicate::Distance => unreachable!("boolean plan excludes distance"),
        };
        let attno = i16::try_from(column.attno)
            .map_err(|_| "spatial aggregate attribute number exceeds int16".to_owned())?;
        Ok(Self {
            column,
            column_request: ResidentColumnRef {
                relid: pg_sys::Oid::from(column.relation_oid),
                attno,
            },
            column_is_left,
            predicate,
            distance_threshold,
            exact_fn_oid,
            geometry_type_oid: catalog.geometry_type_oid,
            catalog_fingerprint: catalog.fingerprint_words.clone().into_boxed_slice(),
            constant: SpatialConstantShape {
                exact_bytes: constant_bytes.len(),
                coordinate_pairs: parsed.coordinate_pairs,
                ring_count: parsed.ring_count,
            },
            max_groups,
        })
    }

    pub const fn column_request(&self) -> ResidentColumnRef {
        self.column_request
    }

    fn evidence_rows(
        inputs: &ResidentInputBundle<'_>,
        relation_oid: u32,
    ) -> Result<usize, ResidentLoadError> {
        let row_count = inputs
            .evidence
            .iter()
            .find(|evidence| u32::from(evidence.relid) == relation_oid)
            .ok_or_else(|| {
                ResidentLoadError::Loader(format!(
                    "spatial preflight is missing relation evidence for OID {relation_oid}"
                ))
            })?
            .row_count;
        usize::try_from(row_count)
            .map_err(|_| ResidentLoadError::Loader("relation row count exceeds usize".to_owned()))
    }

    fn base_shape(
        &self,
        spec: &AggQuerySpec,
        inputs: &ResidentInputBundle<'_>,
    ) -> Result<SpatialBaseShape, ResidentLoadError> {
        let mut dimensions = [SpatialDimensionShape::default();
            crate::engine::spec::abi::PGACCEL_GROUPED_AGG_MAX_DIMS];
        for (index, dimension) in spec.star_dims.iter().enumerate() {
            let slot = dimensions.get_mut(index).ok_or_else(|| {
                ResidentLoadError::Loader("spatial dimension count exceeds ABI maximum".to_owned())
            })?;
            slot.row_count = Self::evidence_rows(inputs, dimension.relation_oid)?;
            slot.counted = dimension.multiplicity == JoinMultiplicity::Counted;
            slot.group_key_count = spec
                .group_keys
                .iter()
                .filter(|key| {
                    matches!(
                        key.source,
                        GroupKeySource::StarDimension { dim_index, .. }
                            if usize::try_from(dim_index).ok() == Some(index)
                    )
                })
                .count();
        }
        Ok(SpatialBaseShape {
            fact_rows: Self::evidence_rows(inputs, spec.fact_rel)?,
            fact_group_key_count: spec
                .group_keys
                .iter()
                .filter(|key| matches!(key.source, GroupKeySource::FactColumn(_)))
                .count(),
            dimension_count: spec.star_dims.len(),
            dimensions,
        })
    }

    pub fn preflight(
        &self,
        spec: &AggQuerySpec,
        requests: &[ResidentColumnRef],
        inputs: ResidentInputBundle<'_>,
    ) -> Result<StagedTransformPreflight<SpatialPreflight>, ResidentLoadError> {
        let base = self.base_shape(spec, &inputs)?;
        let exact_snapshot_words =
            resident_geometry_exact_snapshot_words(requests, &inputs, self.column_request)?;
        let preflight = spatial_preflight(
            &base,
            self.constant,
            SpatialSnapshotLayout {
                exact_snapshot_words,
            },
            PGACCEL_SPATIAL_MAX_CHUNK_ROWS,
        )
        .map_err(|error| {
            ResidentLoadError::Loader(format!("spatial preflight failed: {error:?}"))
        })?;
        Ok(StagedTransformPreflight {
            prepared: preflight,
            published_accounting: preflight.published_accounting,
            transient_accounting: preflight.transient_accounting,
        })
    }

    pub fn snapshot_prepare(
        &self,
        spec: &AggQuerySpec,
        requests: &[ResidentColumnRef],
        preflight: SpatialPreflight,
        inputs: ResidentInputBundle<'_>,
    ) -> Result<SpatialPreparedSnapshot, ResidentLoadError> {
        let exact_storage = try_zeroed_box::<u64>(
            preflight.snapshot.exact_snapshot_words,
            "spatial exact snapshot allocation failed",
        )?;
        let exact = snapshot_resident_geometry_exact(
            requests,
            &inputs,
            self.column_request,
            exact_storage,
        )?;
        let mut base_spec = spec.clone();
        base_spec.fact_filter = FilterSpec::None;
        let prepared = prepare_spatial_base_artifact(&base_spec, requests, inputs, self.max_groups)
            .map_err(ResidentLoadError::Loader)?;
        validate_prepared_base_device_bytes(&preflight, prepared.device_bytes)?;
        Ok(SpatialPreparedSnapshot {
            preflight,
            prepared_base: prepared.prepared,
            exact,
        })
    }

    fn verify_catalog(&self) -> Result<(), ResidentLoadError> {
        // SAFETY: staged finalize runs synchronously on PostgreSQL's main backend thread.
        let current = unsafe { resolve_postgis_catalog() }.map_err(ResidentLoadError::Loader)?;
        if current.geometry_type_oid != self.geometry_type_oid
            || current.fingerprint_words.as_slice() != self.catalog_fingerprint.as_ref()
        {
            return Err(ResidentLoadError::Loader(
                "PostGIS catalog identity changed during spatial artifact construction".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(super) struct SpatialPreparedSnapshot {
    preflight: SpatialPreflight,
    prepared_base: PreparedAggArtifact,
    exact: ResidentGeometryExactSnapshot,
}

fn expected_base_device_bytes(preflight: &SpatialPreflight) -> Result<u64, ResidentLoadError> {
    let final_mask_bytes = u64::try_from(preflight.fact_rows)
        .map_err(|_| ResidentLoadError::ArtifactAccountingOverflow)?;
    preflight
        .published_accounting
        .device_bytes
        .checked_sub(final_mask_bytes)
        .ok_or(ResidentLoadError::ArtifactAccountingOverflow)
}

fn validate_prepared_base_device_bytes(
    preflight: &SpatialPreflight,
    actual: u64,
) -> Result<(), ResidentLoadError> {
    if actual != expected_base_device_bytes(preflight)? {
        return Err(ResidentLoadError::Loader(
            "padded spatial base artifact does not match preflight accounting".to_owned(),
        ));
    }
    Ok(())
}

fn try_zeroed_box<T: Clone + Default>(
    len: usize,
    detail: &'static str,
) -> Result<Box<[T]>, ResidentLoadError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|_| ResidentLoadError::Loader(detail.to_owned()))?;
    values.resize(len, T::default());
    Ok(values.into_boxed_slice())
}

/// Publishable composite: the ordinary group/dimension artifact and the final
/// exact SQL mask are retained under one derived-artifact identity.
pub(super) struct SpatialAggArtifact {
    pub base: DescriptorAggArtifact,
    pub final_mask: Option<ExprDeviceBuffer<i8>>,
    device_bytes: u64,
}

impl SpatialAggArtifact {
    pub fn new(
        base: DescriptorAggArtifact,
        final_mask: Option<ExprDeviceBuffer<i8>>,
    ) -> Result<Self, &'static str> {
        if final_mask.as_ref().map_or(0, ExprDeviceBuffer::len) != base.fact_rows {
            return Err("spatial SQL mask length does not match fact rows");
        }
        let mask_bytes = checked_mul(base.fact_rows, std::mem::size_of::<i8>())
            .ok_or("spatial SQL mask byte count overflow")?;
        let device_bytes = base
            .device_bytes()
            .checked_add(mask_bytes)
            .ok_or("spatial composite byte count overflow")?;
        Ok(Self {
            base,
            final_mask,
            device_bytes,
        })
    }
}

impl DerivedArtifact for SpatialAggArtifact {
    fn device_bytes(&self) -> u64 {
        self.device_bytes
    }

    fn retained_host_exact_bytes(&self) -> u64 {
        0
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct SpatialChunkWorkspace {
    first_row: usize,
    row_count: usize,
    control: ExprDeviceBuffer<u8>,
    failure_flags: ExprDeviceBuffer<u32>,
    tri_state: ExprDeviceBuffer<i8>,
    final_mask: ExprDeviceBuffer<i8>,
    uncertain_indices: ExprDeviceBuffer<u64>,
    uncertain_count: ExprDeviceBuffer<u64>,
    native_workspace: PgaccelSpatialWorkspace,
    compact_request: PgaccelSpatialRecheckCompactRequest,
    eval_outcome: Option<SpatialResidentLaunchOutcome>,
    compact_outcome: Option<SpatialResidentLaunchOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpatialBorrowFailure {
    MissingGeometryColumn,
    GeometryShapeChanged,
    RequestConstruction,
}

/// Complete nonpublishable W. All device scratch and every D2H destination
/// are allocated after both ledger charges and before the raw store borrow.
pub(super) struct SpatialWorkspace {
    preflight: SpatialPreflight,
    base: DescriptorAggArtifact,
    exact: ResidentGeometryExactSnapshot,
    constant: ResidentGeometryColumn,
    constant_view: PgaccelResidentGeometryView,
    chunks: Box<[SpatialChunkWorkspace]>,
    host_final_mask: Box<[i8]>,
    host_uncertain_indices: Box<[u64]>,
    host_exact_results: Box<[i8]>,
    borrow_failure: Option<SpatialBorrowFailure>,
}

fn allocation_error(detail: &'static str) -> ResidentLoadError {
    ResidentLoadError::Loader(detail.to_owned())
}

fn device_buffer<T>(
    len: usize,
    detail: &'static str,
) -> Result<ExprDeviceBuffer<T>, ResidentLoadError> {
    ExprDeviceBuffer::new(len).ok_or_else(|| allocation_error(detail))
}

fn buffer_bytes<T>(buffer: &ExprDeviceBuffer<T>) -> Option<u64> {
    checked_mul(buffer.len(), std::mem::size_of::<T>())
}

fn geometry_view(
    data: crate::engine::residency::ResidentGeometryColumnView<'_>,
) -> Result<PgaccelResidentGeometryView, SpatialBorrowFailure> {
    PgaccelResidentGeometryView::from_device_buffers(
        data.coordinates,
        data.bboxes,
        data.geometry_offsets,
        data.ring_offsets,
        data.rows,
        data.nulls,
        data.row_count,
        data.coordinate_pair_count,
        data.ring_count,
    )
    .map_err(|_| SpatialBorrowFailure::GeometryShapeChanged)
}

fn spatial_constant_bytes(spec: &AggQuerySpec) -> Option<&[u8]> {
    let FilterSpec::Spatial { left, right, .. } = &spec.fact_filter else {
        return None;
    };
    [left, right].into_iter().find_map(|operand| match operand {
        SpatialOperand::Constant { bytes, .. } => Some(bytes.as_ref()),
        SpatialOperand::Column { .. } => None,
    })
}

impl SpatialWorkspace {
    pub fn build(
        plan: &SpatialTransformPlan,
        spec: &AggQuerySpec,
        snapshot: SpatialPreparedSnapshot,
    ) -> Result<Self, ResidentLoadError> {
        let SpatialPreparedSnapshot {
            preflight,
            prepared_base,
            exact,
        } = snapshot;
        prepare_spatial_resident().map_err(ResidentLoadError::Gpu)?;
        let mut base =
            DescriptorAggArtifact::build(prepared_base).map_err(ResidentLoadError::Loader)?;
        base.resolved_spec = spec.clone();
        validate_prepared_base_device_bytes(&preflight, base.device_bytes())?;
        let constant_bytes = spatial_constant_bytes(spec).ok_or_else(|| {
            ResidentLoadError::Loader(
                "spatial constant disappeared before workspace build".to_owned(),
            )
        })?;
        let limits = crate::engine::cost::device_limits();
        let constant = materialize_resident_geometry_constant(
            constant_bytes,
            limits.resident_domain_max_exact_value_bytes,
            limits.gpu_spatial_max_vertices_per_row,
        )
        .map_err(ResidentLoadError::Loader)?;
        let constant_view = geometry_view(constant.view()).map_err(|_| {
            ResidentLoadError::Loader(
                "materialized spatial constant has an invalid device shape".to_owned(),
            )
        })?;
        if constant_view.row_count != 1
            || constant_view.coordinate_pair_count != plan.constant.coordinate_pairs
            || constant_view.ring_count != plan.constant.ring_count
        {
            return Err(ResidentLoadError::Loader(
                "materialized spatial constant changed preflight shape".to_owned(),
            ));
        }

        let mut chunks = Vec::new();
        chunks
            .try_reserve_exact(preflight.chunk_count)
            .map_err(|_| allocation_error("spatial chunk layout allocation failed"))?;
        for first_row in (0..preflight.fact_rows).step_by(preflight.chunk_limit) {
            let row_count = (preflight.fact_rows - first_row).min(preflight.chunk_limit);
            let control = device_buffer::<u8>(
                PGACCEL_SPATIAL_CONTROL_BYTES,
                "spatial control allocation failed",
            )?;
            let failure_flags = device_buffer::<u32>(1, "spatial failure-word allocation failed")?;
            let tri_state = device_buffer::<i8>(row_count, "spatial tri-state allocation failed")?;
            let final_mask =
                device_buffer::<i8>(row_count, "spatial chunk-mask allocation failed")?;
            let uncertain_indices =
                device_buffer::<u64>(row_count, "spatial uncertainty-index allocation failed")?;
            let uncertain_count =
                device_buffer::<u64>(1, "spatial uncertainty-count allocation failed")?;
            let native_workspace =
                PgaccelSpatialWorkspace::from_device_buffers(&control, &failure_flags)
                    .map_err(ResidentLoadError::Gpu)?;
            let compact_request = PgaccelSpatialRecheckCompactRequest::from_device_buffers(
                &tri_state,
                &final_mask,
                &uncertain_indices,
                &uncertain_count,
                row_count,
            )
            .map_err(ResidentLoadError::Gpu)?;
            chunks.push(SpatialChunkWorkspace {
                first_row,
                row_count,
                control,
                failure_flags,
                tri_state,
                final_mask,
                uncertain_indices,
                uncertain_count,
                native_workspace,
                compact_request,
                eval_outcome: None,
                compact_outcome: None,
            });
        }
        let fact_rows = preflight.fact_rows;
        let workspace = Self {
            preflight,
            base,
            exact,
            constant,
            constant_view,
            chunks: chunks.into_boxed_slice(),
            host_final_mask: try_zeroed_box(
                fact_rows,
                "spatial final-mask readback allocation failed",
            )?,
            host_uncertain_indices: try_zeroed_box(
                fact_rows,
                "spatial uncertainty-index readback allocation failed",
            )?,
            host_exact_results: try_zeroed_box(
                fact_rows,
                "spatial exact-result staging allocation failed",
            )?,
            borrow_failure: None,
        };
        let actual = workspace
            .accounting()
            .ok_or(ResidentLoadError::ArtifactAccountingOverflow)?;
        if actual != workspace.preflight.transient_accounting {
            return Err(ResidentLoadError::TransformWorkspaceAccountingMismatch {
                declared: workspace.preflight.transient_accounting,
                actual,
            });
        }
        Ok(workspace)
    }

    fn accounting(&self) -> Option<ResidentByteAccounting> {
        let constant = self.constant.accounting();
        let chunk_device_bytes = self.chunks.iter().try_fold(0_u64, |total, chunk| {
            let bytes = checked_sum([
                buffer_bytes(&chunk.control)?,
                buffer_bytes(&chunk.failure_flags)?,
                buffer_bytes(&chunk.tri_state)?,
                buffer_bytes(&chunk.final_mask)?,
                buffer_bytes(&chunk.uncertain_indices)?,
                buffer_bytes(&chunk.uncertain_count)?,
            ])?;
            total.checked_add(bytes)
        })?;
        let host_bytes = checked_sum([
            self.exact.retained_host_bytes()?,
            constant.retained_host_exact_bytes,
            checked_mul(self.host_final_mask.len(), std::mem::size_of::<i8>())?,
            checked_mul(
                self.host_uncertain_indices.len(),
                std::mem::size_of::<u64>(),
            )?,
            checked_mul(self.host_exact_results.len(), std::mem::size_of::<i8>())?,
        ])?;
        Some(ResidentByteAccounting {
            device_bytes: self
                .base
                .device_bytes()
                .checked_add(constant.device_bytes)?
                .checked_add(chunk_device_bytes)?,
            retained_host_exact_bytes: host_bytes,
        })
    }

    /// Raw launch only. All errors and native outcomes are retained as POD for
    /// mapping after the resident-store borrow is released.
    pub fn launch(&mut self, plan: &SpatialTransformPlan, inputs: ResidentDispatchBundle<'_>) {
        let view = match inputs.find_column(plan.column_request) {
            Ok(ResidentColumnView::Geometry { type_oid, data })
                if type_oid == plan.geometry_type_oid
                    && data.row_count == self.preflight.fact_rows =>
            {
                geometry_view(data)
            }
            Ok(_) => Err(SpatialBorrowFailure::GeometryShapeChanged),
            Err(_) => Err(SpatialBorrowFailure::MissingGeometryColumn),
        };
        let column_view = match view {
            Ok(view) => view,
            Err(failure) => {
                self.borrow_failure = Some(failure);
                return;
            }
        };
        for chunk in &mut self.chunks {
            let column_operand =
                PgaccelResidentGeometryOperand::column(column_view, chunk.first_row);
            let constant_operand = PgaccelResidentGeometryOperand::constant(self.constant_view);
            let (left, right) = if plan.column_is_left {
                (column_operand, constant_operand)
            } else {
                (constant_operand, column_operand)
            };
            let Some(max_referenced_bytes) = checked_sum([
                POINT_COORDINATE_BYTES,
                GEOMETRY_ROW_BYTES,
                GEOMETRY_BBOX_BYTES,
                GEOMETRY_OFFSET_BYTES_PER_ROW,
                NULL_SIDECAR_BYTES_PER_ROW,
            ])
            .and_then(|point_bytes| {
                u64::try_from(chunk.row_count)
                    .ok()?
                    .checked_mul(point_bytes)
            })
            .and_then(|column_bytes| {
                plan.constant
                    .referenced_bytes()?
                    .max(column_bytes)
                    .checked_mul(2)
            })
            .and_then(|bytes| usize::try_from(bytes).ok()) else {
                self.borrow_failure = Some(SpatialBorrowFailure::RequestConstruction);
                return;
            };
            let Ok(request) = PgaccelSpatialResidentRequest::boolean_device(
                plan.predicate,
                plan.distance_threshold,
                chunk.row_count,
                max_referenced_bytes,
                left,
                right,
                &chunk.tri_state,
            ) else {
                self.borrow_failure = Some(SpatialBorrowFailure::RequestConstruction);
                return;
            };
            // SAFETY: every pointer is owned by this workspace or the active
            // resident input borrow; the queue was prepared by `build`.
            chunk.eval_outcome =
                Some(unsafe { spatial_eval_resident_launch(&request, &chunk.native_workspace) });
            // SAFETY: compaction is the ordered second half of the same chain.
            chunk.compact_outcome = Some(unsafe {
                spatial_recheck_compact_launch(&chunk.compact_request, &chunk.native_workspace)
            });
        }
    }

    fn caught_error_message(caught: &pg_sys::panic::CaughtError) -> String {
        use pg_sys::panic::CaughtError;

        match caught {
            CaughtError::PostgresError(error) | CaughtError::ErrorReport(error) => {
                error.message().to_owned()
            }
            CaughtError::RustPanic { ereport, .. } => ereport.message().to_owned(),
        }
    }

    fn exact_result(
        plan: &SpatialTransformPlan,
        spec: &AggQuerySpec,
        row: &[u8],
    ) -> Result<i8, ResidentLoadError> {
        let constant = spatial_constant_bytes(spec).ok_or_else(|| {
            ResidentLoadError::Loader(
                "spatial constant disappeared before exact recheck".to_owned(),
            )
        })?;
        let row_datum = pg_sys::Datum::from(row.as_ptr());
        let constant_datum = pg_sys::Datum::from(constant.as_ptr());
        let (left, right) = if plan.column_is_left {
            (row_datum, constant_datum)
        } else {
            (constant_datum, row_datum)
        };
        let result = pg_sys::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| {
            let result = if plan.predicate == ResidentSpatialPredicate::DWithin {
                // SAFETY: the catalog identity proves the exact strict three-argument
                // PostGIS OID. Both geometry Datums reference complete retained
                // GSERIALIZED values, the float8 threshold is passed by value, and this
                // callback runs synchronously on the main backend thread.
                unsafe {
                    pg_sys::OidFunctionCall3Coll(
                        plan.exact_fn_oid,
                        pg_sys::InvalidOid,
                        left,
                        right,
                        pg_sys::Datum::from(plan.distance_threshold.to_bits() as usize),
                    )
                }
            } else {
                // SAFETY: the catalog identity proves the exact strict two-argument
                // PostGIS OID. Both geometry Datums reference complete retained
                // GSERIALIZED values and this callback runs synchronously on the main
                // backend thread.
                unsafe {
                    pg_sys::OidFunctionCall2Coll(plan.exact_fn_oid, pg_sys::InvalidOid, left, right)
                }
            };
            Ok::<_, ResidentLoadError>(result)
        }))
        .catch_others(|caught| {
            Err(ResidentLoadError::Loader(format!(
                "PostGIS exact spatial recheck raised an error: {}",
                Self::caught_error_message(&caught)
            )))
        })
        .execute()?;
        Ok(if result.value() == 0 { -1 } else { 1 })
    }

    pub fn finalize(
        mut self,
        plan: &SpatialTransformPlan,
        spec: &AggQuerySpec,
    ) -> Result<SpatialAggArtifact, ResidentLoadError> {
        plan.verify_catalog()?;
        if let Some(failure) = self.borrow_failure {
            return Err(ResidentLoadError::Loader(format!(
                "spatial resident borrow failed: {failure:?}"
            )));
        }
        for chunk in &mut self.chunks {
            let eval = chunk.eval_outcome.ok_or_else(|| {
                ResidentLoadError::Loader("spatial chunk was not evaluated".to_owned())
            })?;
            let compact = chunk.compact_outcome.ok_or_else(|| {
                ResidentLoadError::Loader("spatial chunk was not compacted".to_owned())
            })?;
            spatial_eval_resident_launch_result(eval).map_err(ResidentLoadError::Gpu)?;
            spatial_recheck_compact_launch_result(compact).map_err(ResidentLoadError::Gpu)?;
            // SAFETY: this workspace owns the live chain buffers.
            unsafe { spatial_workspace_finish(&chunk.native_workspace) }
                .map_err(ResidentLoadError::Gpu)?;

            let mut uncertain_count = 0_u64;
            chunk
                .uncertain_count
                .copy_to_slice(std::slice::from_mut(&mut uncertain_count))
                .map_err(ResidentLoadError::Gpu)?;
            let uncertain_count = usize::try_from(uncertain_count)
                .ok()
                .filter(|count| *count <= chunk.row_count)
                .ok_or_else(|| {
                    ResidentLoadError::Loader(
                        "spatial uncertainty count exceeds chunk capacity".to_owned(),
                    )
                })?;
            let end = chunk.first_row + chunk.row_count;
            let host_indices = &mut self.host_uncertain_indices[chunk.first_row..end];
            chunk
                .uncertain_indices
                .copy_to_slice(host_indices)
                .map_err(ResidentLoadError::Gpu)?;
            let mut previous = None;
            for index in host_indices.iter().copied().take(uncertain_count) {
                let index = usize::try_from(index)
                    .ok()
                    .filter(|index| *index < chunk.row_count)
                    .ok_or_else(|| {
                        ResidentLoadError::Loader(
                            "spatial uncertainty index is out of range".to_owned(),
                        )
                    })?;
                if previous.is_some_and(|previous| index <= previous) {
                    return Err(ResidentLoadError::Loader(
                        "spatial uncertainty indices are not strictly ordered".to_owned(),
                    ));
                }
                previous = Some(index);
            }

            if uncertain_count != 0 {
                let exact_results = &mut self.host_exact_results[chunk.first_row..end];
                exact_results.fill(-1);
                for (patch, local_row) in host_indices
                    .iter()
                    .copied()
                    .take(uncertain_count)
                    .enumerate()
                {
                    let local_row = usize::try_from(local_row).map_err(|_| {
                        ResidentLoadError::Loader("spatial recheck row exceeds usize".to_owned())
                    })?;
                    let global_row = chunk
                        .first_row
                        .checked_add(local_row)
                        .ok_or(ResidentLoadError::ArtifactAccountingOverflow)?;
                    let exact = self.exact.exact_value(global_row).ok_or_else(|| {
                        ResidentLoadError::Loader(
                            "spatial uncertain row has no exact GSERIALIZED snapshot".to_owned(),
                        )
                    })?;
                    exact_results[patch] = Self::exact_result(plan, spec, exact)?;
                }
                chunk
                    .tri_state
                    .write_from_slice(exact_results)
                    .map_err(ResidentLoadError::Gpu)?;
                let patch_request = PgaccelSpatialRecheckPatchRequest {
                    abi_version: PGACCEL_SPATIAL_RECHECK_ABI_VERSION,
                    flags: 0,
                    indices: chunk.uncertain_indices.as_ptr(),
                    indices_bytes: uncertain_count
                        .checked_mul(std::mem::size_of::<u64>())
                        .ok_or(ResidentLoadError::ArtifactAccountingOverflow)?,
                    results: chunk.tri_state.as_ptr(),
                    results_bytes: uncertain_count,
                    final_mask: chunk.final_mask.as_mut_ptr(),
                    final_mask_bytes: chunk.row_count,
                    row_count: chunk.row_count,
                    patch_count: uncertain_count,
                };
                // SAFETY: patch spans are exact prefixes of the live buffers.
                let patch = unsafe {
                    spatial_recheck_patch_launch(&patch_request, &chunk.native_workspace)
                };
                spatial_recheck_patch_launch_result(patch).map_err(ResidentLoadError::Gpu)?;
                // SAFETY: patch created a new chain in this live workspace.
                unsafe { spatial_workspace_finish(&chunk.native_workspace) }
                    .map_err(ResidentLoadError::Gpu)?;
            }
            chunk
                .final_mask
                .copy_to_slice(&mut self.host_final_mask[chunk.first_row..end])
                .map_err(ResidentLoadError::Gpu)?;
            pgrx::check_for_interrupts!();
        }

        let final_mask = if self.preflight.fact_rows == 0 {
            None
        } else {
            Some(
                ExprDeviceBuffer::copy_from_slice(&self.host_final_mask)
                    .ok_or_else(|| allocation_error("spatial final-mask upload failed"))?,
            )
        };
        let artifact = SpatialAggArtifact::new(self.base, final_mask)
            .map_err(|detail| ResidentLoadError::Loader(detail.to_owned()))?;
        plan.verify_catalog()?;
        Ok(artifact)
    }
}

impl StagedTransformWorkspace for SpatialWorkspace {
    fn device_bytes(&self) -> u64 {
        self.accounting()
            .map_or(u64::MAX, |value| value.device_bytes)
    }

    fn host_bytes(&self) -> u64 {
        self.accounting()
            .map_or(u64::MAX, |value| value.retained_host_exact_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_GEOMETRY_OID: u32 = 60_001;
    const TEST_SRID: i32 = 4_326;

    fn geometry_typmod(geometry_type: u32, srid: i32) -> i32 {
        (srid << 8) | i32::try_from(geometry_type).expect("geometry tag fits i32") << 2
    }

    fn polygon_bytes(srid: i32) -> Box<[u8]> {
        let srid = u32::try_from(srid).expect("test SRID is nonnegative");
        let mut bytes = vec![
            0,
            0,
            0,
            0,
            ((srid >> 16) & 0xff) as u8,
            ((srid >> 8) & 0xff) as u8,
            (srid & 0xff) as u8,
            0,
        ];
        bytes.extend_from_slice(&crate::engine::residency::RESIDENT_GEOMETRY_POLYGON.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&4_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        for (x, y) in [(0.0_f64, 0.0_f64), (4.0, 0.0), (0.0, 4.0), (0.0, 0.0)] {
            bytes.extend_from_slice(&x.to_le_bytes());
            bytes.extend_from_slice(&y.to_le_bytes());
        }
        bytes.into_boxed_slice()
    }

    fn spatial_spec(
        predicate: SpatialPredicateKind,
        column_is_left: bool,
        column_geometry_type: u32,
        srid: i32,
        constant_typmod: i32,
    ) -> AggQuerySpec {
        let column = SpatialOperand::Column {
            column: ColumnRef {
                relation_oid: 10,
                attno: 1,
                type_oid: TEST_GEOMETRY_OID,
            },
            metadata: SpatialValueMetadata {
                kind: SpatialValueKind::Geometry,
                typmod: geometry_typmod(column_geometry_type, srid),
                srid: Some(srid),
            },
        };
        let constant = SpatialOperand::Constant {
            metadata: SpatialValueMetadata {
                kind: SpatialValueKind::Geometry,
                typmod: constant_typmod,
                srid: Some(srid),
            },
            bytes: polygon_bytes(srid),
        };
        let (left, right) = if column_is_left {
            (column, constant)
        } else {
            (constant, column)
        };
        AggQuerySpec {
            fact_rel: 10,
            group_keys: Vec::new(),
            measures: vec![crate::engine::spec::MeasureSpec {
                expression: crate::engine::spec::MeasureExpr::CountStar,
                outputs: vec![crate::engine::spec::AggregateOutput {
                    source: crate::engine::spec::AggregateSource::Value,
                    kind: crate::engine::spec::AggregateKind::Count,
                }],
                filter: FilterSpec::None,
            }],
            fact_filter: FilterSpec::Spatial {
                predicate,
                left,
                right,
                distance: (predicate == SpatialPredicateKind::DWithin)
                    .then_some(ScalarValue::F64(1.0)),
            },
            star_dims: Vec::new(),
            having: None,
        }
    }

    fn decoded(spec: &AggQuerySpec) -> AggQuerySpec {
        let words = spec.encode_i32().expect("test spec encodes");
        AggQuerySpec::decode_i32(&words).expect("test spec decodes")
    }

    fn catalog() -> PostgisCatalogIdentity {
        PostgisCatalogIdentity {
            extension_oid: pg_sys::Oid::from(1_u32),
            schema_oid: pg_sys::Oid::from(2_u32),
            geometry_type_oid: pg_sys::Oid::from(TEST_GEOMETRY_OID),
            intersects_fn_oid: pg_sys::Oid::from(10_u32),
            contains_fn_oid: pg_sys::Oid::from(11_u32),
            within_fn_oid: pg_sys::Oid::from(12_u32),
            dwithin_fn_oid: pg_sys::Oid::from(13_u32),
            distance_fn_oid: pg_sys::Oid::from(14_u32),
            is_valid_fn_oid: pg_sys::Oid::from(15_u32),
            fingerprint_words: vec![1, 2, 3],
        }
    }

    fn shape(fact_rows: usize) -> SpatialBaseShape {
        SpatialBaseShape {
            fact_rows,
            fact_group_key_count: 1,
            dimension_count: 1,
            dimensions: [
                SpatialDimensionShape {
                    row_count: 7,
                    group_key_count: 1,
                    counted: true,
                },
                SpatialDimensionShape {
                    row_count: 0,
                    group_key_count: 0,
                    counted: false,
                },
                SpatialDimensionShape {
                    row_count: 0,
                    group_key_count: 0,
                    counted: false,
                },
                SpatialDimensionShape {
                    row_count: 0,
                    group_key_count: 0,
                    counted: false,
                },
            ],
        }
    }

    fn constant() -> SpatialConstantShape {
        SpatialConstantShape {
            exact_bytes: 96,
            coordinate_pairs: 5,
            ring_count: 1,
        }
    }

    #[test]
    fn preflight_chunks_at_the_native_boundary_and_reserves_all_uncertain_rows() {
        let preflight = spatial_preflight(
            &shape(PGACCEL_SPATIAL_MAX_CHUNK_ROWS + 3),
            constant(),
            SpatialSnapshotLayout {
                exact_snapshot_words: 40,
            },
            PGACCEL_SPATIAL_MAX_CHUNK_ROWS,
        )
        .expect("bounded preflight");
        assert_eq!(preflight.chunk_count, 2);
        assert_eq!(preflight.chunk_limit, 65_536);
        let full_uncertainty_host = (PGACCEL_SPATIAL_MAX_CHUNK_ROWS as u64 + 3) * 9;
        assert!(preflight.transient_accounting.retained_host_exact_bytes >= full_uncertainty_host);
    }

    #[test]
    fn half_budget_is_twice_the_larger_referenced_operand() {
        let preflight = spatial_preflight(
            &shape(10),
            constant(),
            SpatialSnapshotLayout {
                exact_snapshot_words: 1,
            },
            10,
        )
        .expect("bounded preflight");
        assert_eq!(preflight.max_referenced_bytes, 2 * 10 * 89);

        let large_constant = SpatialConstantShape {
            coordinate_pairs: 1_000,
            ..constant()
        };
        let preflight = spatial_preflight(
            &shape(1),
            large_constant,
            SpatialSnapshotLayout {
                exact_snapshot_words: 1,
            },
            1,
        )
        .expect("bounded preflight");
        assert_eq!(
            preflight.max_referenced_bytes as u64,
            2 * large_constant.referenced_bytes().expect("constant bytes")
        );
    }

    #[test]
    fn accounting_matches_padded_base_mask_constant_and_chunk_scratch() {
        let base = shape(4);
        let snapshot = SpatialSnapshotLayout {
            exact_snapshot_words: 10,
        };
        let preflight = spatial_preflight(&base, constant(), snapshot, 3).expect("preflight");
        // Base: fact key 16 + dimension key 28 + fact join 16 + match 7 + multiplicity 56.
        assert_eq!(preflight.published_accounting.device_bytes, 123 + 4);
        // W owns the padded base 123, constant 160, and chunks (384 + 4 + 8) + 10*n.
        assert_eq!(
            preflight.transient_accounting.device_bytes,
            123 + 160 + 426 + 406
        );
        // Snapshot 80 + constant host 128 + full mask/index/result 40.
        assert_eq!(
            preflight.transient_accounting.retained_host_exact_bytes,
            248
        );
    }

    #[test]
    fn zero_or_oversized_chunk_limits_and_arithmetic_overflow_fail_closed() {
        assert_eq!(
            spatial_preflight(
                &shape(1),
                constant(),
                SpatialSnapshotLayout {
                    exact_snapshot_words: 1,
                },
                0,
            ),
            Err(SpatialPreflightError::ZeroChunkLimit)
        );
        assert_eq!(
            spatial_preflight(
                &shape(1),
                constant(),
                SpatialSnapshotLayout {
                    exact_snapshot_words: 1,
                },
                PGACCEL_SPATIAL_MAX_CHUNK_ROWS + 1,
            ),
            Err(SpatialPreflightError::ChunkLimitExceedsNativeMaximum)
        );
        let overflow = SpatialBaseShape {
            fact_rows: usize::MAX,
            fact_group_key_count: usize::MAX,
            dimension_count: 0,
            dimensions: [SpatialDimensionShape {
                row_count: 0,
                group_key_count: 0,
                counted: false,
            }; crate::engine::spec::abi::PGACCEL_GROUPED_AGG_MAX_DIMS],
        };
        assert_eq!(
            spatial_preflight(
                &overflow,
                constant(),
                SpatialSnapshotLayout {
                    exact_snapshot_words: 0,
                },
                PGACCEL_SPATIAL_MAX_CHUNK_ROWS,
            ),
            Err(SpatialPreflightError::AccountingOverflow)
        );
    }

    #[test]
    fn preflight_performs_no_allocation_or_snapshot_copy() {
        let base = shape(10);
        let constant = constant();
        let snapshot = SpatialSnapshotLayout {
            exact_snapshot_words: 10,
        };
        crate::engine::residency::begin_test_allocation_count();
        let result = spatial_preflight(&base, constant, snapshot, 4);
        let allocation_count = crate::engine::residency::finish_test_allocation_count();
        assert!(result.is_ok());
        assert_eq!(allocation_count, 0);
    }

    #[test]
    fn prepared_base_accounting_mismatch_is_a_hard_error() {
        let preflight = spatial_preflight(
            &shape(4),
            constant(),
            SpatialSnapshotLayout {
                exact_snapshot_words: 0,
            },
            4,
        )
        .expect("preflight");
        let expected = expected_base_device_bytes(&preflight).expect("base accounting");
        assert!(validate_prepared_base_device_bytes(&preflight, expected).is_ok());
        let error = validate_prepared_base_device_bytes(&preflight, expected + 1)
            .expect_err("mismatch must fail closed");
        assert!(matches!(error, ResidentLoadError::Loader(_)));
    }

    #[test]
    fn decoded_spatial_plan_rejects_nonpoint_zero_srid_and_hostile_orientation() {
        let point = crate::engine::residency::RESIDENT_GEOMETRY_POINT;
        let polygon = crate::engine::residency::RESIDENT_GEOMETRY_POLYGON;
        let valid = decoded(&spatial_spec(
            SpatialPredicateKind::Intersects,
            true,
            point,
            TEST_SRID,
            -1,
        ));
        assert!(SpatialTransformPlan::new(&valid, &catalog(), 1_024).is_ok());

        let nonpoint = decoded(&spatial_spec(
            SpatialPredicateKind::Intersects,
            true,
            polygon,
            TEST_SRID,
            -1,
        ));
        assert!(SpatialTransformPlan::new(&nonpoint, &catalog(), 1_024).is_err());

        let zero_srid = decoded(&spatial_spec(
            SpatialPredicateKind::Intersects,
            true,
            point,
            0,
            -1,
        ));
        assert!(SpatialTransformPlan::new(&zero_srid, &catalog(), 1_024).is_err());

        let hostile_orientation = decoded(&spatial_spec(
            SpatialPredicateKind::Contains,
            true,
            point,
            TEST_SRID,
            -1,
        ));
        assert!(SpatialTransformPlan::new(&hostile_orientation, &catalog(), 1_024).is_err());

        let contradictory_constant_typmod = decoded(&spatial_spec(
            SpatialPredicateKind::Intersects,
            true,
            point,
            TEST_SRID,
            geometry_typmod(point, TEST_SRID),
        ));
        assert!(
            SpatialTransformPlan::new(&contradictory_constant_typmod, &catalog(), 1_024).is_err()
        );
    }
}
