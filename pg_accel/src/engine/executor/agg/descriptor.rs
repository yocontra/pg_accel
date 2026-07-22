//! Runtime capability checks and frozen-ABI binding for neutral aggregates.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::fmt;
use std::time::{Duration, Instant};

#[cfg(feature = "pg_test")]
use std::sync::atomic::{AtomicUsize, Ordering};

use pgrx::pg_sys;

use super::artifact::{
    ArtifactKeyInput, DescriptorAggArtifact, H3ParentArtifact, artifact_column_refs,
    prepare_agg_artifact,
};
use super::output::{DescriptorAggOutput, validate_h3_compact_key_buffers};
use super::spatial::{SpatialAggArtifact, SpatialTransformPlan, SpatialWorkspace};
use crate::engine::cost::{ExecutorLimits, GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS, Rows};
use crate::engine::executor::bounded::{
    BoundedDispatchError, bounded_dispatch_call_count, cleanup_before_rethrow,
    dispatch_warning_threshold_exceeded, run_bounded_dispatch,
};
use crate::engine::ffi::syscache::{PostgisCatalogIdentity, resolve_postgis_catalog};
use crate::engine::gucs;
use crate::engine::residency::{
    ArtifactEnsureOutcome, DerivedArtifactIdentity, ResidentByteAccounting, ResidentColumnRef,
    ResidentColumnView, ResidentLoadError, ResidentRelationEvidence, ResolvedDerivedInputs,
    SelectedRelation, ensure_derived_artifact, ensure_device_derived_artifact,
    ensure_selected_relations, ensure_staged_device_transform_artifact,
    with_derived_artifact_inputs,
};
use crate::engine::spec::abi;
use crate::engine::spec::{
    AggOutputProjection, AggQuerySpec, AggregateKind, AggregateSource, BinaryMeasureOp, ColumnRef,
    FilterSpec, GroupKeyEncoding, GroupKeySource, JoinMultiplicity, MaskKind, MeasureExpr,
    ScalarValue, SpatialOperand,
};
use crate::gpu::{
    GpuError, GpuErrorDomain, GpuOperation, GpuStatusDetail, GroupedAggOutcome,
    GroupedAggOutputStorage, GroupedAggSession, PgaccelExprUsmCol, PgaccelVal, PgaccelValTag,
    ResolvedGroupedAggPlan, execute_grouped_agg_one_shot,
};

const BOOLOID: u32 = 16;
const INT2OID: u32 = 21;
const INT4OID: u32 = 23;
const INT8OID: u32 = 20;
const TEXTOID: u32 = 25;
const FLOAT4OID: u32 = 700;
const FLOAT8OID: u32 = 701;
const BPCHAROID: u32 = 1042;
const VARCHAROID: u32 = 1043;
const DATEOID: u32 = 1082;
const TIMESTAMPOID: u32 = 1114;
const TIMESTAMPTZOID: u32 = 1184;

// Atomic SUM one-shot execution only needs the row cap. Ordered integer
// statistics still use the bounded native partial layout mirrored here.
const DENSE_INTEGER_CHUNK_ROWS: usize = 1_024;
const DENSE_INTEGER_PARTIAL_BYTES: usize = 56;
const DENSE_INTEGER_MAX_PARTIAL_BYTES: usize = 32 * 1_024 * 1_024;

fn is_text_family(type_oid: u32) -> bool {
    matches!(type_oid, TEXTOID | VARCHAROID | BPCHAROID)
}

fn is_supported_group_type(type_oid: u32) -> bool {
    matches!(
        type_oid,
        BOOLOID
            | INT2OID
            | INT4OID
            | INT8OID
            | FLOAT4OID
            | FLOAT8OID
            | DATEOID
            | TIMESTAMPOID
            | TIMESTAMPTZOID
            | TEXTOID
            | VARCHAROID
            | BPCHAROID
    )
}

fn validate_filter(
    filter: &FilterSpec,
    relation_oid: u32,
    dimension: bool,
    allow_internal_spatial: bool,
) -> Result<(), String> {
    match filter {
        FilterSpec::None => Ok(()),
        FilterSpec::Ranges { input, .. }
            if input.relation_oid == relation_oid
                && if dimension {
                    matches!(
                        input.type_oid,
                        BOOLOID
                            | INT4OID
                            | INT8OID
                            | FLOAT4OID
                            | FLOAT8OID
                            | DATEOID
                            | TIMESTAMPOID
                            | TIMESTAMPTZOID
                    )
                } else {
                    matches!(
                        input.type_oid,
                        INT4OID
                            | INT8OID
                            | FLOAT4OID
                            | FLOAT8OID
                            | DATEOID
                            | TIMESTAMPOID
                            | TIMESTAMPTZOID
                    )
                } =>
        {
            Ok(())
        }
        FilterSpec::Mask {
            input,
            kind: MaskKind::Sql,
        } if input.relation_oid == relation_oid && input.type_oid == BOOLOID => Ok(()),
        FilterSpec::Spatial { .. } if !dimension && allow_internal_spatial => Ok(()),
        _ if dimension => Err("dimension filter is outside the Phase 5D descriptor subset".into()),
        _ => Err("fact filter is outside the Phase 5D descriptor subset".into()),
    }
}

fn validate_measure_outputs(
    expression: &MeasureExpr,
    outputs: &[crate::engine::spec::AggregateOutput],
) -> Result<(), String> {
    if outputs
        .iter()
        .any(|output| output.source != AggregateSource::Value)
    {
        return Err("Phase 5D does not expose RHS aggregate lanes".into());
    }
    let count_min_max = || {
        outputs.iter().all(|output| {
            matches!(
                output.kind,
                AggregateKind::Count | AggregateKind::Min | AggregateKind::Max
            )
        })
    };
    let basic = |kind: AggregateKind| {
        matches!(
            kind,
            AggregateKind::Sum | AggregateKind::Count | AggregateKind::Min | AggregateKind::Max
        )
    };
    match expression {
        MeasureExpr::CountStar if outputs.len() == 1 && outputs[0].kind == AggregateKind::Count => {
            Ok(())
        }
        MeasureExpr::Column(column) if column.type_oid == INT4OID => {
            if outputs.iter().all(|output| basic(output.kind)) {
                Ok(())
            } else {
                Err("INT4 descriptor measure requests an unsupported aggregate lane".into())
            }
        }
        MeasureExpr::Column(column)
            if matches!(column.type_oid, INT8OID | FLOAT8OID) && count_min_max() =>
        {
            Ok(())
        }
        MeasureExpr::Column(column)
            if matches!(
                column.type_oid,
                BOOLOID | FLOAT4OID | DATEOID | TIMESTAMPOID | TIMESTAMPTZOID
            ) && outputs
                .iter()
                .all(|output| output.kind == AggregateKind::Count) =>
        {
            Ok(())
        }
        MeasureExpr::Binary { op, lhs, rhs }
            if lhs.type_oid == INT4OID
                && rhs.type_oid == INT4OID
                && matches!(op, BinaryMeasureOp::Mul | BinaryMeasureOp::Sub)
                && outputs.iter().all(|output| basic(output.kind)) =>
        {
            Ok(())
        }
        MeasureExpr::Binary {
            op: BinaryMeasureOp::Sub,
            lhs,
            rhs,
        } if lhs.type_oid == INT8OID && rhs.type_oid == INT8OID && count_min_max() => Ok(()),
        MeasureExpr::Binary {
            op: BinaryMeasureOp::Mul,
            lhs,
            rhs,
        } if lhs.type_oid == INT8OID && rhs.type_oid == INT8OID => {
            Err("INT8 multiplication is not implemented by the generic kernel".into())
        }
        _ => Err("measure is outside the Phase 5D descriptor subset".into()),
    }
}

fn h3_parent_group(spec: &AggQuerySpec) -> Option<(ColumnRef, i32, u32)> {
    let [key] = spec.group_keys.as_slice() else {
        return None;
    };
    let GroupKeySource::H3CellToParent { cell, resolution } = key.source else {
        return None;
    };
    Some((cell, resolution, key.type_oid))
}

fn validate_h3_runtime_capability(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<(), String> {
    let Some((cell, resolution, result_type_oid)) = h3_parent_group(spec) else {
        return Err("only one H3 cell-to-parent group key is supported".to_owned());
    };
    if cell.relation_oid != spec.fact_rel
        || cell.type_oid != result_type_oid
        || !(0..=15).contains(&resolution)
        || spec.group_keys[0].encoding != GroupKeyEncoding::Hash
        || spec.group_keys[0].collation_oid != 0
    {
        return Err("H3 parent group key metadata is not canonical".to_owned());
    }
    if !spec.star_dims.is_empty()
        || !matches!(spec.fact_filter, FilterSpec::None)
        || spec.having.is_some()
    {
        return Err("H3 parent grouping does not support joins, filters, or HAVING".to_owned());
    }
    let [measure] = spec.measures.as_slice() else {
        return Err("H3 parent grouping requires exactly one COUNT(*) measure".to_owned());
    };
    if !matches!(measure.expression, MeasureExpr::CountStar)
        || !matches!(measure.filter, FilterSpec::None)
        || measure.outputs.len() != 1
        || measure.outputs[0].source != AggregateSource::Value
        || measure.outputs[0].kind != AggregateKind::Count
    {
        return Err("H3 parent grouping requires exactly one unfiltered COUNT(*)".to_owned());
    }
    if projection.slots.len() != 2
        || projection
            .slots
            .iter()
            .filter(|slot| {
                matches!(
                    slot.source,
                    crate::engine::spec::AggOutputSource::GroupKey { key_index: 0 }
                )
            })
            .count()
            != 1
        || projection
            .slots
            .iter()
            .filter(|slot| {
                matches!(
                    slot.source,
                    crate::engine::spec::AggOutputSource::Aggregate {
                        measure_index: 0,
                        source: AggregateSource::Value,
                        kind: AggregateKind::Count,
                    }
                )
            })
            .count()
            != 1
    {
        return Err(
            "H3 parent grouping projection must contain its key and COUNT(*) once".to_owned(),
        );
    }
    Ok(())
}

fn validate_runtime_capability_with_spatial(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
    allow_internal_spatial: bool,
) -> Result<(), String> {
    spec.validate()
        .map_err(|error| format!("invalid aggregate query spec: {error}"))?;
    projection
        .validate(spec)
        .map_err(|error| format!("invalid aggregate output projection: {error}"))?;
    if spec.group_keys.iter().any(|key| {
        matches!(
            key.source,
            GroupKeySource::H3CellToParent { .. } | GroupKeySource::H3LatLngToCell { .. }
        )
    }) {
        return validate_h3_runtime_capability(spec, projection);
    }
    if spec.having.is_some() {
        return Err("HAVING is not implemented by the Phase 5D descriptor executor".into());
    }
    for key in &spec.group_keys {
        if !is_supported_group_type(key.type_oid) {
            return Err(format!("group type OID {} is unsupported", key.type_oid));
        }
        if key.encoding != GroupKeyEncoding::Hash {
            return Err("generic plan group keys must use unresolved HASH encoding".into());
        }
        if is_text_family(key.type_oid) != (key.collation_oid != 0) {
            return Err("group-key type/collation metadata is not canonical".into());
        }
        if !matches!(
            key.source,
            GroupKeySource::FactColumn(_) | GroupKeySource::StarDimension { .. }
        ) {
            return Err("Phase 5D supports only column group keys".into());
        }
    }
    let counted_dimension = spec
        .star_dims
        .iter()
        .any(|dimension| dimension.multiplicity == JoinMultiplicity::Counted);
    for dimension in &spec.star_dims {
        if dimension.fact_key.type_oid != dimension.dim_key.type_oid {
            return Err("join inputs must have exactly the same type OID".into());
        }
        match dimension.fact_key.type_oid {
            INT4OID if dimension.collation_oid == 0 => {}
            type_oid if is_text_family(type_oid) && dimension.collation_oid != 0 => {}
            INT8OID => return Err("INT8 star joins are not implemented by Phase 5D".into()),
            type_oid => return Err(format!("join type OID {type_oid} is unsupported")),
        }
        validate_filter(
            &dimension.filter,
            dimension.relation_oid,
            true,
            allow_internal_spatial,
        )?;
    }
    validate_filter(
        &spec.fact_filter,
        spec.fact_rel,
        false,
        allow_internal_spatial,
    )?;
    for measure in &spec.measures {
        if !matches!(measure.filter, FilterSpec::None) {
            return Err("aggregate FILTER is not implemented by Phase 5D".into());
        }
        validate_measure_outputs(&measure.expression, &measure.outputs)?;
        if counted_dimension
            && measure
                .outputs
                .iter()
                .any(|output| output.kind == AggregateKind::Sum)
            && matches!(
                measure.expression,
                MeasureExpr::Column(ColumnRef {
                    type_oid: INT4OID,
                    ..
                }) | MeasureExpr::Binary {
                    lhs: ColumnRef {
                        type_oid: INT4OID,
                        ..
                    },
                    ..
                }
            )
        {
            return Err("integer SUM with a counted dimension is conservatively declined".into());
        }
    }
    Ok(())
}

pub(super) fn validate_runtime_capability(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<(), String> {
    validate_runtime_capability_with_spatial(spec, projection, false)
}

fn validate_internal_runtime_capability(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<(), String> {
    validate_runtime_capability_with_spatial(spec, projection, true)
}

#[cfg(any(test, feature = "pg_test"))]
pub(super) fn validate_test_forced_spatial_capability(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<(), String> {
    if !matches!(spec.fact_filter, FilterSpec::Spatial { .. }) {
        return Err("test-forced spatial capability requires a spatial fact filter".to_owned());
    }
    validate_internal_runtime_capability(spec, projection)
}

fn validate_catalog_contract(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<(), String> {
    let collations = spec
        .group_keys
        .iter()
        .map(|key| key.collation_oid)
        .chain(
            spec.star_dims
                .iter()
                .map(|dimension| dimension.collation_oid),
        )
        .filter(|oid| *oid != 0)
        .collect::<BTreeSet<_>>();
    for oid in collations {
        // SAFETY: executor Begin runs on the PostgreSQL main backend thread and
        // the strict wire has already proved a nonzero collation OID.
        if !unsafe { pg_sys::get_collation_isdeterministic(pg_sys::Oid::from(oid)) } {
            return Err(format!(
                "collation OID {oid} is missing or nondeterministic at execution time"
            ));
        }
    }
    let validate_column = |column: ColumnRef, expected_collation: u32| -> Result<i32, String> {
        let attnum = i16::try_from(column.attno)
            .map_err(|_| format!("attribute {} exceeds int16", column.attno))?;
        let mut type_oid = pg_sys::InvalidOid;
        let mut typmod = -1;
        let mut collation_oid = pg_sys::InvalidOid;
        // SAFETY: executor Begin runs on the main backend thread. The strict
        // spec proved positive relation/attribute identities before lookup.
        unsafe {
            pg_sys::get_atttypetypmodcoll(
                pg_sys::Oid::from(column.relation_oid),
                attnum,
                std::ptr::from_mut(&mut type_oid),
                std::ptr::from_mut(&mut typmod),
                std::ptr::from_mut(&mut collation_oid),
            );
        }
        if u32::from(type_oid) != column.type_oid {
            return Err(format!(
                "column ({}, {}) catalog type OID {} does not match planned OID {}",
                column.relation_oid,
                column.attno,
                u32::from(type_oid),
                column.type_oid
            ));
        }
        if u32::from(collation_oid) != expected_collation {
            return Err(format!(
                "column ({}, {}) catalog collation OID {} does not match planned OID {}",
                column.relation_oid,
                column.attno,
                u32::from(collation_oid),
                expected_collation
            ));
        }
        Ok(typmod)
    };
    let mut key_typmods = Vec::with_capacity(spec.group_keys.len());
    for key in &spec.group_keys {
        let typmod = match key.source {
            GroupKeySource::FactColumn(column)
            | GroupKeySource::StarDimension {
                group_column: column,
                ..
            } => validate_column(column, key.collation_oid)?,
            GroupKeySource::H3CellToParent { cell, .. } => {
                let typmod = validate_column(cell, 0)?;
                if typmod != -1 || cell.type_oid != key.type_oid || key.collation_oid != 0 {
                    return Err("H3 parent column/result catalog metadata is not canonical".into());
                }
                // SAFETY: executor Begin runs on PostgreSQL's main backend thread.
                let catalog = unsafe { crate::engine::ffi::syscache::resolve_h3_catalog() }?;
                if u32::from(catalog.type_oid) != key.type_oid {
                    return Err(format!(
                        "planned H3 type OID {} is not the canonical h3index OID {}",
                        key.type_oid,
                        u32::from(catalog.type_oid)
                    ));
                }
                // SAFETY: both OIDs were resolved from the canonical catalog proof.
                unsafe {
                    crate::engine::ffi::syscache::validate_h3_parent_function(
                        catalog.parent_fn_oid,
                        catalog.type_oid,
                    )
                }?;
                typmod
            }
            GroupKeySource::Expression { .. } | GroupKeySource::H3LatLngToCell { .. } => -1,
        };
        key_typmods.push(typmod);
    }
    for dimension in &spec.star_dims {
        validate_column(dimension.fact_key, dimension.collation_oid)?;
        validate_column(dimension.dim_key, dimension.collation_oid)?;
    }
    if let FilterSpec::Spatial { left, right, .. } = &spec.fact_filter {
        for operand in [left, right] {
            let SpatialOperand::Column { column, metadata } = operand else {
                continue;
            };
            let actual_typmod = validate_column(*column, 0)?;
            if actual_typmod != metadata.typmod {
                return Err(format!(
                    "spatial column ({}, {}) catalog typmod {actual_typmod} does not match planned typmod {}",
                    column.relation_oid, column.attno, metadata.typmod
                ));
            }
        }
    }
    for slot in &projection.slots {
        if let crate::engine::spec::AggOutputSource::GroupKey { key_index } = slot.source {
            let key_index = usize::try_from(key_index)
                .map_err(|_| "group projection key index exceeds usize".to_owned())?;
            let actual_typmod = key_typmods
                .get(key_index)
                .copied()
                .ok_or_else(|| "group projection references a missing key typmod".to_owned())?;
            if slot.result_typmod != actual_typmod {
                return Err(format!(
                    "group projection key {key_index} typmod {} does not match catalog typmod {actual_typmod}",
                    slot.result_typmod
                ));
            }
        }
    }
    Ok(())
}

fn insert_column(
    relations: &mut BTreeMap<u32, BTreeSet<i16>>,
    column: ColumnRef,
) -> Result<(), String> {
    let attno = i16::try_from(column.attno)
        .map_err(|_| format!("attribute {} exceeds int16", column.attno))?;
    relations
        .entry(column.relation_oid)
        .or_default()
        .insert(attno);
    Ok(())
}

fn insert_filter_columns(
    relations: &mut BTreeMap<u32, BTreeSet<i16>>,
    filter: &FilterSpec,
) -> Result<(), String> {
    match filter {
        FilterSpec::None => Ok(()),
        FilterSpec::Ranges { input, .. } | FilterSpec::Mask { input, .. } => {
            insert_column(relations, *input)
        }
        FilterSpec::Bytecode { inputs, .. } => {
            for input in inputs {
                insert_column(relations, *input)?;
            }
            Ok(())
        }
        FilterSpec::Spatial { left, right, .. } => {
            if let Some(column) = left.column() {
                insert_column(relations, column)?;
            }
            if let Some(column) = right.column() {
                insert_column(relations, column)?;
            }
            Ok(())
        }
    }
}

fn selected_relations(spec: &AggQuerySpec) -> Result<Vec<SelectedRelation>, String> {
    let mut relations = BTreeMap::<u32, BTreeSet<i16>>::new();
    relations.entry(spec.fact_rel).or_default();
    for key in &spec.group_keys {
        match &key.source {
            GroupKeySource::FactColumn(column) => insert_column(&mut relations, *column)?,
            GroupKeySource::StarDimension { group_column, .. } => {
                insert_column(&mut relations, *group_column)?;
            }
            GroupKeySource::Expression { inputs, .. } => {
                for input in inputs {
                    insert_column(&mut relations, *input)?;
                }
            }
            GroupKeySource::H3CellToParent { cell, .. } => {
                insert_column(&mut relations, *cell)?;
            }
            GroupKeySource::H3LatLngToCell {
                latitude,
                longitude,
                ..
            } => {
                insert_column(&mut relations, *latitude)?;
                insert_column(&mut relations, *longitude)?;
            }
        }
    }
    for measure in &spec.measures {
        match &measure.expression {
            MeasureExpr::CountStar => {}
            MeasureExpr::Column(column) => insert_column(&mut relations, *column)?,
            MeasureExpr::Binary { lhs, rhs, .. } | MeasureExpr::StatsPair { value: lhs, rhs } => {
                insert_column(&mut relations, *lhs)?;
                insert_column(&mut relations, *rhs)?;
            }
            MeasureExpr::Bytecode { inputs, .. } => {
                for input in inputs {
                    insert_column(&mut relations, *input)?;
                }
            }
        }
        insert_filter_columns(&mut relations, &measure.filter)?;
    }
    insert_filter_columns(&mut relations, &spec.fact_filter)?;
    for dimension in &spec.star_dims {
        relations.entry(dimension.relation_oid).or_default();
        insert_column(&mut relations, dimension.fact_key)?;
        insert_column(&mut relations, dimension.dim_key)?;
        insert_filter_columns(&mut relations, &dimension.filter)?;
    }
    Ok(relations
        .into_iter()
        .map(|(relid, columns)| SelectedRelation {
            relid: pg_sys::Oid::from(relid),
            columns: columns.into_iter().collect(),
        })
        .collect())
}

fn dispatch_column_refs(spec: &AggQuerySpec) -> Result<Vec<ResidentColumnRef>, String> {
    let mut columns = BTreeSet::new();
    let mut insert = |column: ColumnRef| -> Result<(), String> {
        columns.insert((
            column.relation_oid,
            i16::try_from(column.attno)
                .map_err(|_| format!("attribute {} exceeds int16", column.attno))?,
        ));
        Ok(())
    };
    for measure in &spec.measures {
        match &measure.expression {
            MeasureExpr::CountStar => {}
            MeasureExpr::Column(column) => insert(*column)?,
            MeasureExpr::Binary { lhs, rhs, .. } | MeasureExpr::StatsPair { value: lhs, rhs } => {
                insert(*lhs)?;
                insert(*rhs)?;
            }
            MeasureExpr::Bytecode { inputs, .. } => {
                for input in inputs {
                    insert(*input)?;
                }
            }
        }
    }
    if let Some((cell, ..)) = h3_parent_group(spec) {
        insert(cell)?;
    }
    if let FilterSpec::Ranges { input, .. } = &spec.fact_filter {
        insert(*input)?;
    }
    Ok(columns
        .into_iter()
        .map(|(relid, attno)| ResidentColumnRef {
            relid: pg_sys::Oid::from(relid),
            attno,
        })
        .collect())
}

fn dependency_relids(spec: &AggQuerySpec) -> Vec<pg_sys::Oid> {
    std::iter::once(spec.fact_rel)
        .chain(
            spec.star_dims
                .iter()
                .map(|dimension| dimension.relation_oid),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(pg_sys::Oid::from)
        .collect()
}

/// Begin-time plan for a generic childless aggregate.
#[derive(Debug, Clone)]
enum DescriptorArtifactKind {
    Dense,
    H3Parent {
        cell: ColumnRef,
        resolution: i32,
        max_chunk_rows: usize,
    },
    Spatial(SpatialTransformPlan),
}

fn descriptor_artifact_identity(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
    catalog_fingerprint: &[i32],
) -> Result<DerivedArtifactIdentity, String> {
    let mut identity_words = spec
        .encode_i32()
        .map_err(|error| format!("could not encode aggregate query spec: {error}"))?;
    identity_words.extend(
        projection
            .encode_i32(spec)
            .map_err(|error| format!("could not encode aggregate projection: {error}"))?,
    );
    identity_words.extend_from_slice(catalog_fingerprint);
    Ok(DerivedArtifactIdentity::from_canonical_words(
        identity_words,
    ))
}

fn spatial_artifact_kind(
    spec: &AggQuerySpec,
    catalog: &PostgisCatalogIdentity,
    max_groups: usize,
) -> Result<DescriptorArtifactKind, String> {
    Ok(DescriptorArtifactKind::Spatial(SpatialTransformPlan::new(
        spec, catalog, max_groups,
    )?))
}

fn verify_catalog_before<T>(
    verify_catalog: impl FnOnce() -> Result<(), ResidentLoadError>,
    operation: impl FnOnce() -> Result<T, ResidentLoadError>,
) -> Result<T, ResidentLoadError> {
    verify_catalog()?;
    operation()
}

pub(super) struct DescriptorAggPlan {
    spec: AggQuerySpec,
    projection: AggOutputProjection,
    identity: DerivedArtifactIdentity,
    selected: Vec<SelectedRelation>,
    dependencies: Vec<pg_sys::Oid>,
    artifact_columns: Vec<ResidentColumnRef>,
    dispatch_columns: Vec<ResidentColumnRef>,
    max_groups: usize,
    dispatch_chunk_rows: usize,
    grouped_agg_one_shot_max_rows: Rows,
    artifact_kind: DescriptorArtifactKind,
}

pub(super) struct DescriptorResidencyReport {
    pub(super) artifact_outcome: ArtifactEnsureOutcome,
    pub(super) relations: Vec<ResidentRelationEvidence>,
    pub(super) loaded_relations: Vec<pg_sys::Oid>,
    pub(super) artifact_bytes: u64,
    pub(super) raw_load_ms: f64,
    pub(super) preparation_time_us: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum DescriptorAggExecutionError {
    NumericOverflow,
    ExternalRoutineException(String),
    Failure(String),
}

impl fmt::Display for DescriptorAggExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NumericOverflow => formatter.write_str("numeric value out of range"),
            Self::ExternalRoutineException(message) | Self::Failure(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl From<String> for DescriptorAggExecutionError {
    fn from(message: String) -> Self {
        Self::Failure(message)
    }
}

impl From<ResidentLoadError> for DescriptorAggExecutionError {
    fn from(error: ResidentLoadError) -> Self {
        match error {
            ResidentLoadError::Gpu(error)
                if error.domain == GpuErrorDomain::H3
                    && error.operation == GpuOperation::Kernel("h3_cell_to_parent_resident")
                    && matches!(
                        error.status,
                        GpuStatusDetail::InvalidArgument | GpuStatusDetail::ShapeMismatch
                    ) =>
            {
                Self::ExternalRoutineException(error.to_string())
            }
            error => Self::Failure(error.to_string()),
        }
    }
}

fn gpu_execution_error(context: &'static str, error: GpuError) -> DescriptorAggExecutionError {
    if error.status == GpuStatusDetail::NumericOverflow {
        DescriptorAggExecutionError::NumericOverflow
    } else {
        DescriptorAggExecutionError::Failure(format!("{context}: {error}"))
    }
}

impl DescriptorResidencyReport {
    pub(super) fn merge(&mut self, latest: Self) {
        if latest.artifact_outcome != ArtifactEnsureOutcome::Hit {
            self.artifact_outcome = latest.artifact_outcome;
        }
        self.relations = latest.relations;
        for relid in latest.loaded_relations {
            if !self.loaded_relations.contains(&relid) {
                self.loaded_relations.push(relid);
            }
        }
        self.artifact_bytes = latest.artifact_bytes;
        self.raw_load_ms += latest.raw_load_ms;
        self.preparation_time_us = self
            .preparation_time_us
            .saturating_add(latest.preparation_time_us);
    }
}

fn h3_group_capacity(
    fact_rows: usize,
    resolution: i32,
    max_groups: usize,
) -> Result<usize, String> {
    let resolution = u32::try_from(resolution)
        .ok()
        .filter(|resolution| *resolution <= 15)
        .ok_or_else(|| "H3 parent resolution is outside 0..=15".to_owned())?;
    let possible_nonnull = 7_usize
        .checked_pow(resolution)
        .and_then(|cells| cells.checked_mul(120))
        .and_then(|cells| cells.checked_add(2))
        .ok_or_else(|| "H3 group bound overflows usize".to_owned())?;
    let possible_with_null = possible_nonnull
        .checked_add(1)
        .ok_or_else(|| "H3 NULL group bound overflows usize".to_owned())?;
    let kernel_max_groups = 1_usize << 30;
    Ok(fact_rows
        .max(1)
        .min(possible_with_null)
        .min(max_groups.min(kernel_max_groups))
        .max(1))
}

fn selected_fact_rows(
    selected: &[ResidentRelationEvidence],
    fact_rel: u32,
) -> Result<usize, String> {
    let rows = selected
        .iter()
        .find(|evidence| u32::from(evidence.relid) == fact_rel)
        .ok_or_else(|| format!("resident evidence is missing fact relation OID {fact_rel}"))?
        .row_count;
    usize::try_from(rows).map_err(|_| format!("fact row count {rows} exceeds usize"))
}

impl DescriptorAggPlan {
    pub(super) fn new(spec: AggQuerySpec, projection: AggOutputProjection) -> Result<Self, String> {
        validate_internal_runtime_capability(&spec, &projection)?;
        validate_catalog_contract(&spec, &projection)?;
        let limits = crate::engine::cost::device_limits();
        let executor_limits = ExecutorLimits::from(limits);
        let (artifact_kind, catalog_fingerprint) =
            if let Some((cell, resolution, result_type_oid)) = h3_parent_group(&spec) {
                // SAFETY: executor Begin runs on PostgreSQL's main backend thread.
                let catalog = unsafe { crate::engine::ffi::syscache::resolve_h3_catalog() }?;
                if u32::from(catalog.type_oid) != cell.type_oid || cell.type_oid != result_type_oid
                {
                    return Err("H3 catalog identity changed after validation".to_owned());
                }
                (
                    DescriptorArtifactKind::H3Parent {
                        cell,
                        resolution,
                        max_chunk_rows: limits.gpu_h3_max_chunk_rows,
                    },
                    catalog.fingerprint_words,
                )
            } else if matches!(spec.fact_filter, FilterSpec::Spatial { .. }) {
                // SAFETY: executor Begin runs on PostgreSQL's main backend thread.
                let catalog = unsafe { resolve_postgis_catalog() }?;
                let artifact_kind =
                    spatial_artifact_kind(&spec, &catalog, limits.gpu_hash_agg_max_groups)?;
                (artifact_kind, catalog.fingerprint_words)
            } else {
                (DescriptorArtifactKind::Dense, Vec::new())
            };
        let identity = descriptor_artifact_identity(&spec, &projection, &catalog_fingerprint)?;
        let selected = selected_relations(&spec)?;
        let dependencies = dependency_relids(&spec);
        let artifact_columns = artifact_column_refs(&spec)?;
        if let DescriptorArtifactKind::Spatial(plan) = &artifact_kind
            && !artifact_columns.contains(&plan.column_request())
        {
            return Err("spatial source column is missing from artifact requests".to_owned());
        }
        let dispatch_columns = dispatch_column_refs(&spec)?;
        let dispatch_chunk_rows = if h3_parent_group(&spec).is_some() {
            executor_limits.gpu_h3_max_chunk_rows.get()
        } else {
            executor_limits.gpu_reduce_max_chunk.get()
        };
        Ok(Self {
            spec,
            projection,
            identity,
            selected,
            dependencies,
            artifact_columns,
            dispatch_columns,
            max_groups: limits.gpu_hash_agg_max_groups,
            dispatch_chunk_rows,
            grouped_agg_one_shot_max_rows: executor_limits.gpu_grouped_agg_one_shot_max_rows,
            artifact_kind,
        })
    }

    pub(super) fn spec(&self) -> &AggQuerySpec {
        &self.spec
    }

    pub(super) fn projection(&self) -> &AggOutputProjection {
        &self.projection
    }

    pub(super) fn ensure_artifact(
        &self,
    ) -> Result<DescriptorResidencyReport, DescriptorAggExecutionError> {
        let preparation_started = Instant::now();
        let selected = match &self.artifact_kind {
            DescriptorArtifactKind::Spatial(plan) => verify_catalog_before(
                || plan.verify_catalog(),
                || ensure_selected_relations(&self.selected),
            )?,
            DescriptorArtifactKind::Dense | DescriptorArtifactKind::H3Parent { .. } => {
                ensure_selected_relations(&self.selected)?
            }
        };
        let owner_relid = pg_sys::Oid::from(self.spec.fact_rel);
        let (artifact_outcome, relations, artifact_bytes) = match &self.artifact_kind {
            DescriptorArtifactKind::Dense => {
                let outcome = ensure_derived_artifact(
                    owner_relid,
                    &self.identity,
                    &self.dependencies,
                    &self.artifact_columns,
                    |bundle| {
                        prepare_agg_artifact(
                            &self.spec,
                            &self.artifact_columns,
                            bundle,
                            self.max_groups,
                        )
                    },
                    DescriptorAggArtifact::build,
                )?;
                let (relations, bytes) = with_derived_artifact_inputs::<DescriptorAggArtifact, _>(
                    owner_relid,
                    &self.identity,
                    &[],
                    |inputs| (inputs.evidence, inputs.device_bytes),
                )?;
                (outcome, relations, bytes)
            }
            DescriptorArtifactKind::H3Parent {
                cell,
                resolution,
                max_chunk_rows,
            } => {
                let fact_rows = selected_fact_rows(&selected.evidence, self.spec.fact_rel)?;
                if fact_rows > u32::MAX as usize {
                    return Err(format!(
                        "H3 grouped aggregation row count {fact_rows} exceeds the kernel u32 domain"
                    )
                    .into());
                }
                let device_bytes = fact_rows
                    .checked_mul(std::mem::size_of::<u64>())
                    .and_then(|bytes| u64::try_from(bytes).ok())
                    .ok_or_else(|| "derived H3 parent byte count overflow".to_owned())?;
                let group_capacity = h3_group_capacity(fact_rows, *resolution, self.max_groups)?;
                let declared = ResidentByteAccounting {
                    device_bytes,
                    retained_host_exact_bytes: 0,
                };
                let outcome = ensure_device_derived_artifact(
                    owner_relid,
                    &self.identity,
                    &self.dependencies,
                    &self.artifact_columns,
                    declared,
                    |bundle| {
                        let view = find_view(&self.artifact_columns, &bundle.columns, *cell)?;
                        let (cells, nulls) = match view {
                            ResidentColumnView::Empty { type_oid }
                                if fact_rows == 0 && u32::from(*type_oid) == cell.type_oid =>
                            {
                                (std::ptr::null(), std::ptr::null())
                            }
                            ResidentColumnView::H3 {
                                type_oid,
                                values,
                                nulls,
                            } if u32::from(*type_oid) == cell.type_oid
                                && values.len() == fact_rows =>
                            {
                                (values.as_ptr(), null_ptr(*nulls))
                            }
                            _ => {
                                return Err(ResidentLoadError::Loader(
                                    "resident H3 source does not match the planned raw lane"
                                        .to_owned(),
                                ));
                            }
                        };
                        let parents = if fact_rows == 0 {
                            None
                        } else {
                            Some(
                                crate::gpu::ExprDeviceBuffer::<u64>::new(fact_rows).ok_or_else(
                                    || {
                                        "could not allocate derived H3 parent device lane"
                                            .to_owned()
                                    },
                                )?,
                            )
                        };
                        if fact_rows != 0 {
                            if *max_chunk_rows == 0 {
                                return Err(ResidentLoadError::Loader(
                                    "H3 device chunk limit is zero".to_owned(),
                                ));
                            }
                            let output = parents.as_ref().ok_or_else(|| {
                                "nonempty H3 parent artifact has no output".to_owned()
                            })?;
                            for offset in (0..fact_rows).step_by(*max_chunk_rows) {
                                let count = (fact_rows - offset).min(*max_chunk_rows);
                                // SAFETY: the matched resident/output buffers each have
                                // `fact_rows` elements and this chunk stays in bounds.
                                let chunk_cells = unsafe { cells.add(offset) };
                                let chunk_nulls = if nulls.is_null() {
                                    std::ptr::null()
                                } else {
                                    // SAFETY: the resident NULL sidecar has one byte per row.
                                    unsafe { nulls.add(offset) }
                                };
                                // SAFETY: output has `fact_rows` u64 elements.
                                let chunk_parents = unsafe { output.as_mut_ptr().add(offset) };
                                // SAFETY: all pointers/counts were bounded above and the
                                // synchronous call cannot outlive the residency borrow.
                                unsafe {
                                    crate::gpu::h3_cell_to_parent_resident(
                                        chunk_cells,
                                        chunk_nulls,
                                        count,
                                        *resolution,
                                        chunk_parents,
                                    )
                                }
                                .map_err(ResidentLoadError::Gpu)?;
                            }
                        }
                        Ok(H3ParentArtifact::new(
                            self.spec.clone(),
                            fact_rows,
                            group_capacity,
                            parents,
                        )?)
                    },
                )?;
                let (relations, bytes) = with_derived_artifact_inputs::<H3ParentArtifact, _>(
                    owner_relid,
                    &self.identity,
                    &[],
                    |inputs| (inputs.evidence, inputs.device_bytes),
                )?;
                (outcome, relations, bytes)
            }
            DescriptorArtifactKind::Spatial(plan) => {
                let outcome = verify_catalog_before(
                    || plan.verify_catalog(),
                    || {
                        ensure_staged_device_transform_artifact(
                            owner_relid,
                            &self.identity,
                            &self.dependencies,
                            &self.artifact_columns,
                            |bundle| plan.preflight(&self.spec, &self.artifact_columns, bundle),
                            |preflight, bundle| {
                                plan.snapshot_prepare(
                                    &self.spec,
                                    &self.artifact_columns,
                                    preflight,
                                    bundle,
                                )
                            },
                            |snapshot| SpatialWorkspace::build(plan, &self.spec, snapshot),
                            |workspace, bundle| {
                                workspace.launch(plan, bundle);
                                Ok(())
                            },
                            |workspace| workspace.finalize(plan, &self.spec),
                        )
                    },
                )?;
                let (relations, bytes) = with_derived_artifact_inputs::<SpatialAggArtifact, _>(
                    owner_relid,
                    &self.identity,
                    &[],
                    |inputs| (inputs.evidence, inputs.device_bytes),
                )?;
                (outcome, relations, bytes)
            }
        };
        Ok(DescriptorResidencyReport {
            artifact_outcome,
            relations,
            loaded_relations: selected.loaded_relations,
            artifact_bytes,
            raw_load_ms: selected.raw_load_ms,
            preparation_time_us: u64::try_from(preparation_started.elapsed().as_micros())
                .unwrap_or(u64::MAX),
        })
    }

    fn execute_once(
        &self,
    ) -> Result<Result<DescriptorAggDispatch, DescriptorAggExecutionError>, ResidentLoadError> {
        let owner_relid = pg_sys::Oid::from(self.spec.fact_rel);
        let result: Result<DescriptorAggDispatch, DescriptorDispatchFailure> = match &self
            .artifact_kind
        {
            DescriptorArtifactKind::Dense => execute_bounded_dense_artifact(
                owner_relid,
                &self.identity,
                &self.dispatch_columns,
                &self.projection,
                self.dispatch_chunk_rows,
                self.grouped_agg_one_shot_max_rows.get(),
            ),
            DescriptorArtifactKind::H3Parent { cell, .. } => {
                match with_derived_artifact_inputs::<H3ParentArtifact, _>(
                    owner_relid,
                    &self.identity,
                    &self.dispatch_columns,
                    |inputs| {
                        build_and_execute_h3_one_shot(
                            inputs,
                            &self.dispatch_columns,
                            *cell,
                            &self.projection,
                        )
                    },
                ) {
                    Ok(dispatch) => dispatch.map_err(DescriptorDispatchFailure::Execution),
                    Err(error) => Err(DescriptorDispatchFailure::Residency(error)),
                }
            }
            DescriptorArtifactKind::Spatial(_) => {
                match with_derived_artifact_inputs::<SpatialAggArtifact, _>(
                    owner_relid,
                    &self.identity,
                    &self.dispatch_columns,
                    |inputs| {
                        build_and_execute_spatial(inputs, &self.dispatch_columns, &self.projection)
                    },
                ) {
                    Ok(dispatch) => dispatch.map_err(DescriptorDispatchFailure::Execution),
                    Err(error) => Err(DescriptorDispatchFailure::Residency(error)),
                }
            }
        };
        match result {
            Ok(dispatch) => Ok(Ok(dispatch)),
            Err(DescriptorDispatchFailure::Residency(error)) => Err(error),
            Err(DescriptorDispatchFailure::Execution(error)) => Ok(Err(error)),
        }
    }

    pub(super) fn execute(&self) -> Result<DescriptorAggDispatch, DescriptorAggExecutionError> {
        let mut residency = self.ensure_artifact()?;
        let mut result = match self.execute_once() {
            Ok(result) => result?,
            Err(
                ResidentLoadError::ArtifactDependencyChanged { .. }
                | ResidentLoadError::ArtifactNotFound { .. },
            ) => {
                residency.merge(self.ensure_artifact()?);
                self.execute_once()
                    .map_err(DescriptorAggExecutionError::from)??
            }
            Err(error) => return Err(DescriptorAggExecutionError::from(error)),
        };
        result.residency = Some(residency);
        Ok(result)
    }
}

pub(super) struct DescriptorAggDispatch {
    pub output: DescriptorAggOutput,
    pub fact_rows: usize,
    pub batches_executed: u64,
    pub dispatch_time_us: u64,
    pub residency: Option<DescriptorResidencyReport>,
}

fn find_view<'a>(
    requests: &[ResidentColumnRef],
    views: &'a [ResidentColumnView<'a>],
    column: ColumnRef,
) -> Result<&'a ResidentColumnView<'a>, String> {
    let attno = i16::try_from(column.attno)
        .map_err(|_| format!("attribute {} exceeds int16", column.attno))?;
    let index = requests
        .iter()
        .position(|request| {
            u32::from(request.relid) == column.relation_oid && request.attno == attno
        })
        .ok_or_else(|| {
            format!(
                "descriptor did not request raw column ({}, {})",
                column.relation_oid, column.attno
            )
        })?;
    let view = views
        .get(index)
        .ok_or_else(|| "resident raw-column bundle changed shape".to_owned())?;
    if u32::from(view.type_oid()) != column.type_oid {
        return Err(format!(
            "resident raw column ({}, {}) type OID {} does not match planned OID {}",
            column.relation_oid,
            column.attno,
            u32::from(view.type_oid()),
            column.type_oid
        ));
    }
    Ok(view)
}

fn null_ptr(nulls: Option<&crate::gpu::ExprDeviceBuffer<u8>>) -> *const u8 {
    nulls.map_or(std::ptr::null(), |buffer| buffer.as_ptr())
}

fn measure_column(
    view: &ResidentColumnView<'_>,
    expected_rows: usize,
) -> Result<abi::PgaccelGroupedAggMeasureCol, String> {
    if view.len() != expected_rows {
        return Err(format!(
            "resident measure has {} rows, fact artifact has {expected_rows}",
            view.len()
        ));
    }
    let (values, nulls, physical_type, element_bytes) = match view {
        ResidentColumnView::Empty { type_oid } => match u32::from(*type_oid) {
            BOOLOID => (
                std::ptr::null(),
                std::ptr::null(),
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL,
                1,
            ),
            INT4OID => (
                std::ptr::null(),
                std::ptr::null(),
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32,
                4,
            ),
            INT8OID => (
                std::ptr::null(),
                std::ptr::null(),
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64,
                8,
            ),
            FLOAT8OID => (
                std::ptr::null(),
                std::ptr::null(),
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64,
                8,
            ),
            FLOAT4OID => (
                std::ptr::null(),
                std::ptr::null(),
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32,
                4,
            ),
            DATEOID => (
                std::ptr::null(),
                std::ptr::null(),
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE,
                4,
            ),
            TIMESTAMPOID | TIMESTAMPTZOID => (
                std::ptr::null(),
                std::ptr::null(),
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP,
                8,
            ),
            type_oid => return Err(format!("type OID {type_oid} cannot be a measure")),
        },
        ResidentColumnView::Bool {
            type_oid,
            values,
            nulls,
        } if u32::from(*type_oid) == BOOLOID => (
            values.as_ptr().cast::<c_void>(),
            null_ptr(*nulls),
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL,
            1,
        ),
        ResidentColumnView::I32 {
            type_oid,
            values,
            nulls,
        } if u32::from(*type_oid) == INT4OID => (
            values.as_ptr().cast::<c_void>(),
            null_ptr(*nulls),
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32,
            4,
        ),
        ResidentColumnView::I32 {
            type_oid,
            values,
            nulls,
        } if u32::from(*type_oid) == DATEOID => (
            values.as_ptr().cast::<c_void>(),
            null_ptr(*nulls),
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE,
            4,
        ),
        ResidentColumnView::I64 {
            type_oid,
            values,
            nulls,
        } if u32::from(*type_oid) == INT8OID => (
            values.as_ptr().cast::<c_void>(),
            null_ptr(*nulls),
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64,
            8,
        ),
        ResidentColumnView::I64 {
            type_oid,
            values,
            nulls,
        } if matches!(u32::from(*type_oid), TIMESTAMPOID | TIMESTAMPTZOID) => (
            values.as_ptr().cast::<c_void>(),
            null_ptr(*nulls),
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP,
            8,
        ),
        ResidentColumnView::F32 {
            type_oid,
            values,
            nulls,
        } if u32::from(*type_oid) == FLOAT4OID => (
            values.as_ptr().cast::<c_void>(),
            null_ptr(*nulls),
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32,
            4,
        ),
        ResidentColumnView::F64 {
            type_oid,
            values,
            nulls,
        } if u32::from(*type_oid) == FLOAT8OID => (
            values.as_ptr().cast::<c_void>(),
            null_ptr(*nulls),
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64,
            8,
        ),
        _ => {
            return Err(format!(
                "resident type OID {} has no Phase 5D measure representation",
                u32::from(view.type_oid())
            ));
        }
    };
    Ok(abi::PgaccelGroupedAggMeasureCol {
        values,
        nulls,
        physical_type,
        element_bytes,
        scale: 0,
        flags: 0,
    })
}

fn lane_mask(outputs: &[crate::engine::spec::AggregateOutput]) -> u32 {
    outputs.iter().fold(0, |mask, output| {
        mask | match output.kind {
            AggregateKind::Sum => abi::PGACCEL_GROUPED_AGG_LANE_SUM,
            AggregateKind::Count => abi::PGACCEL_GROUPED_AGG_LANE_COUNT,
            AggregateKind::Min => abi::PGACCEL_GROUPED_AGG_LANE_MIN,
            AggregateKind::Max => abi::PGACCEL_GROUPED_AGG_LANE_MAX,
            AggregateKind::Avg | AggregateKind::StddevSamp => 0,
        }
    })
}

fn disabled_filter() -> abi::PgaccelGroupedAggFilter {
    // SAFETY: all-zero is valid for every field, including PgaccelValTag::Null.
    let mut filter: abi::PgaccelGroupedAggFilter = unsafe { std::mem::zeroed() };
    filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
    filter
}

fn scalar_value(value: ScalarValue) -> PgaccelVal {
    match value {
        ScalarValue::Bool(value) => PgaccelVal::from_bool(value),
        ScalarValue::I32(value) => PgaccelVal::from_i32(value),
        ScalarValue::I64(value) => PgaccelVal::from_i64(value),
        ScalarValue::F32(value) => PgaccelVal::from_f32(value),
        ScalarValue::F64(value) => PgaccelVal::from_f64(value),
        ScalarValue::Date(value) => PgaccelVal {
            tag: PgaccelValTag::Date,
            data: value as u64,
        },
        ScalarValue::Timestamp(value) | ScalarValue::TimestampTz(value) => PgaccelVal {
            tag: PgaccelValTag::Timestamp,
            data: value as u64,
        },
    }
}

fn filter_measure_binding(spec: &AggQuerySpec, input: ColumnRef) -> Result<usize, String> {
    spec.measures
        .iter()
        .position(|measure| match measure.expression {
            MeasureExpr::Column(column) => column == input,
            MeasureExpr::Binary { lhs, .. } | MeasureExpr::StatsPair { value: lhs, .. } => {
                lhs == input
            }
            MeasureExpr::CountStar | MeasureExpr::Bytecode { .. } => false,
        })
        .ok_or_else(|| "fact range input has no descriptor measure binding".to_owned())
}

fn build_sql_mask_filter(
    fact_rows: usize,
    mask: Option<*const i8>,
) -> Result<abi::PgaccelGroupedAggFilter, String> {
    if fact_rows == 0 {
        return Ok(disabled_filter());
    }
    let mask = mask
        .filter(|mask| !mask.is_null())
        .ok_or_else(|| "nonempty SQL fact mask has no derived device buffer".to_owned())?;
    let mut filter = disabled_filter();
    filter.kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
    filter.mask = mask;
    Ok(filter)
}

fn build_filter(
    spec: &AggQuerySpec,
    fact_rows: usize,
    fact_mask: Option<*const i8>,
) -> Result<abi::PgaccelGroupedAggFilter, String> {
    match &spec.fact_filter {
        FilterSpec::None => Ok(disabled_filter()),
        FilterSpec::Mask {
            kind: MaskKind::Sql,
            ..
        }
        | FilterSpec::Spatial { .. } => build_sql_mask_filter(fact_rows, fact_mask),
        FilterSpec::Ranges { input, ranges } => {
            let mut filter = disabled_filter();
            filter.predicate_measure_slot = i32::try_from(filter_measure_binding(spec, *input)?)
                .map_err(|_| "filter measure index exceeds i32".to_owned())?;
            filter.predicate_source = abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
            filter.predicate_range_count = i32::try_from(ranges.len())
                .map_err(|_| "fact filter range count exceeds i32".to_owned())?;
            for (index, range) in ranges.iter().enumerate() {
                filter.predicate_lo[index] = scalar_value(range.lo);
                filter.predicate_hi[index] = scalar_value(range.hi);
            }
            Ok(filter)
        }
        _ => Err("fact filter escaped the Phase 5D capability backstop".into()),
    }
}

fn build_descriptor_with_fact_mask(
    artifact: &DescriptorAggArtifact,
    requests: &[ResidentColumnRef],
    views: &[ResidentColumnView<'_>],
    fact_mask: Option<*const i8>,
) -> Result<abi::PgaccelGroupedAggDesc, String> {
    let spec = &artifact.resolved_spec;
    if artifact.keys.len() != spec.group_keys.len()
        || artifact.domains.len() != spec.group_keys.len()
        || artifact.dimensions.len() != spec.star_dims.len()
    {
        return Err("derived aggregate artifact changed structural shape".into());
    }
    // SAFETY: every enum-bearing field has zero as a valid discriminant and
    // all inactive ABI slots are required to be bytewise zero.
    let mut desc: abi::PgaccelGroupedAggDesc = unsafe { std::mem::zeroed() };
    desc.abi_version = abi::PGACCEL_OLAP_ABI_VERSION;
    desc.size_bytes = std::mem::size_of::<abi::PgaccelGroupedAggDesc>() as u32;
    desc.row_count = artifact.fact_rows;
    desc.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX;
    desc.output_mode = abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
    desc.key_count = u32::try_from(spec.group_keys.len())
        .map_err(|_| "group-key count exceeds u32".to_owned())?;
    desc.group_capacity = artifact.group_capacity;
    for (index, ((key, input), domain)) in spec
        .group_keys
        .iter()
        .zip(&artifact.keys)
        .zip(artifact.domains.iter())
        .enumerate()
    {
        let (source, values, lookup_by_key) = match (&key.source, input) {
            (GroupKeySource::FactColumn(_), ArtifactKeyInput::Fact(values)) => (
                abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT,
                PgaccelExprUsmCol {
                    values: values
                        .as_ref()
                        .map_or(std::ptr::null(), |values| values.as_ptr().cast()),
                    nulls: std::ptr::null(),
                    tag: PgaccelValTag::Int32,
                },
                std::ptr::null(),
            ),
            (
                GroupKeySource::StarDimension { dim_index, .. },
                ArtifactKeyInput::Dimension {
                    dim_index: artifact_dim,
                    lookup,
                },
            ) if usize::try_from(*dim_index).ok() == Some(*artifact_dim) => (
                abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0
                    + i32::try_from(*artifact_dim)
                        .map_err(|_| "dimension index exceeds i32".to_owned())?,
                PgaccelExprUsmCol {
                    values: std::ptr::null(),
                    nulls: std::ptr::null(),
                    tag: PgaccelValTag::Null,
                },
                lookup
                    .as_ref()
                    .map_or(std::ptr::null(), |lookup| lookup.as_ptr()),
            ),
            _ => return Err("artifact group-key source does not match resolved spec".into()),
        };
        desc.keys[index] = abi::PgaccelGroupedAggKey {
            values,
            lookup_by_key,
            source,
            code_min: 0,
            cardinality: domain.cardinality()?,
            null_code: domain
                .null_code
                .unwrap_or(abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE),
            flags: 0,
            pad0: 0,
        };
    }
    desc.measure_count =
        u32::try_from(spec.measures.len()).map_err(|_| "measure count exceeds u32".to_owned())?;
    for (index, measure) in spec.measures.iter().enumerate() {
        let (op, value, rhs, accumulator_kind) = match &measure.expression {
            MeasureExpr::CountStar => (
                abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR,
                // SAFETY: inactive measure columns must be bytewise zero.
                unsafe { std::mem::zeroed() },
                // SAFETY: COUNT(*) has no right-hand input column, and the ABI
                // requires its inactive column descriptor to be all zeroes.
                unsafe { std::mem::zeroed() },
                abi::PGACCEL_GROUPED_AGG_ACCUM_I64,
            ),
            MeasureExpr::Column(column) => {
                let view = find_view(requests, views, *column)?;
                (
                    abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                    measure_column(view, artifact.fact_rows)?,
                    // SAFETY: a unary column measure has no right-hand input;
                    // the ABI requires that inactive descriptor to be zeroed.
                    unsafe { std::mem::zeroed() },
                    if matches!(column.type_oid, FLOAT4OID | FLOAT8OID) {
                        abi::PGACCEL_GROUPED_AGG_ACCUM_F64
                    } else {
                        abi::PGACCEL_GROUPED_AGG_ACCUM_I64
                    },
                )
            }
            MeasureExpr::Binary { op, lhs, rhs } => {
                let lhs_view = find_view(requests, views, *lhs)?;
                let rhs_view = find_view(requests, views, *rhs)?;
                (
                    match op {
                        BinaryMeasureOp::Mul => abi::PGACCEL_GROUPED_AGG_MEASURE_MUL,
                        BinaryMeasureOp::Sub => abi::PGACCEL_GROUPED_AGG_MEASURE_SUB,
                    },
                    measure_column(lhs_view, artifact.fact_rows)?,
                    measure_column(rhs_view, artifact.fact_rows)?,
                    abi::PGACCEL_GROUPED_AGG_ACCUM_I64,
                )
            }
            MeasureExpr::StatsPair { .. } | MeasureExpr::Bytecode { .. } => {
                return Err("unsupported measure escaped the Phase 5D capability backstop".into());
            }
        };
        desc.measures[index] = abi::PgaccelGroupedAggMeasure {
            value,
            rhs,
            op,
            agg_mask: lane_mask(&measure.outputs),
            accumulator_kind,
            state_bytes: 8,
            flags: 0,
            pad0: 0,
        };
    }
    desc.where_filter = build_filter(spec, artifact.fact_rows, fact_mask)?;
    for filter in &mut desc.measure_filters {
        *filter = disabled_filter();
    }
    desc.dim_count = u32::try_from(artifact.dimensions.len())
        .map_err(|_| "dimension count exceeds u32".to_owned())?;
    for (index, dimension) in artifact.dimensions.iter().enumerate() {
        desc.dims[index] = abi::PgaccelGroupedAggDim {
            fact_key: PgaccelExprUsmCol {
                values: dimension
                    .fact_codes
                    .as_ref()
                    .map_or(std::ptr::null(), |codes| codes.as_ptr().cast()),
                nulls: std::ptr::null(),
                tag: PgaccelValTag::Int32,
            },
            match_by_key: dimension
                .match_by_key
                .as_ref()
                .map_or(std::ptr::null(), |mask| mask.as_ptr()),
            multiplicity_by_key: dimension
                .multiplicity_by_key
                .as_ref()
                .map_or(std::ptr::null(), |multiplicity| multiplicity.as_ptr()),
            key_min: 0,
            key_count: u32::try_from(dimension.key_count)
                .map_err(|_| "dimension dictionary exceeds u32".to_owned())?,
            flags: 0,
            pad0: 0,
        };
    }
    Ok(desc)
}

fn build_descriptor(
    artifact: &DescriptorAggArtifact,
    requests: &[ResidentColumnRef],
    views: &[ResidentColumnView<'_>],
) -> Result<abi::PgaccelGroupedAggDesc, String> {
    build_descriptor_with_fact_mask(
        artifact,
        requests,
        views,
        artifact.fact_mask.as_ref().map(|mask| mask.as_ptr()),
    )
}

fn build_spatial_descriptor(
    artifact: &SpatialAggArtifact,
    requests: &[ResidentColumnRef],
    views: &[ResidentColumnView<'_>],
) -> Result<abi::PgaccelGroupedAggDesc, String> {
    let fact_mask = spatial_final_mask_binding(
        artifact.base.fact_rows,
        artifact
            .final_mask
            .as_ref()
            .map(|mask| (mask.len(), mask.as_ptr())),
    )?;
    build_descriptor_with_fact_mask(&artifact.base, requests, views, fact_mask)
}

fn spatial_final_mask_binding(
    fact_rows: usize,
    mask: Option<(usize, *const i8)>,
) -> Result<Option<*const i8>, String> {
    match (fact_rows, mask) {
        (0, None) => Ok(None),
        (0, Some(_)) => Err("empty spatial artifact unexpectedly retained a final mask".to_owned()),
        (_, None) => Err("nonempty spatial artifact has no final mask".to_owned()),
        (rows, Some((len, _))) if len != rows => Err(format!(
            "spatial final mask length {len} does not match fact rows {rows}"
        )),
        (_, Some((_, mask))) if mask.is_null() => {
            Err("nonempty spatial final mask has a null device pointer".to_owned())
        }
        (_, Some((_, mask))) => Ok(Some(mask)),
    }
}

fn build_h3_descriptor(
    artifact: &H3ParentArtifact,
    requests: &[ResidentColumnRef],
    views: &[ResidentColumnView<'_>],
    cell: ColumnRef,
) -> Result<abi::PgaccelGroupedAggDesc, String> {
    let source = find_view(requests, views, cell)?;
    let source_nulls = match source {
        ResidentColumnView::Empty { type_oid }
            if artifact.fact_rows == 0 && u32::from(*type_oid) == cell.type_oid =>
        {
            std::ptr::null()
        }
        ResidentColumnView::H3 {
            type_oid,
            values,
            nulls,
        } if u32::from(*type_oid) == cell.type_oid && values.len() == artifact.fact_rows => {
            null_ptr(*nulls)
        }
        _ => {
            return Err("H3 descriptor source is not the planned resident h3index lane".to_owned());
        }
    };
    if artifact
        .parents
        .as_ref()
        .map_or(0, crate::gpu::ExprDeviceBuffer::len)
        != artifact.fact_rows
    {
        return Err("H3 parent artifact length changed before dispatch".to_owned());
    }

    // SAFETY: all-zero is canonical for every inactive ABI slot.
    let mut desc: abi::PgaccelGroupedAggDesc = unsafe { std::mem::zeroed() };
    desc.abi_version = abi::PGACCEL_OLAP_ABI_VERSION;
    desc.size_bytes = std::mem::size_of::<abi::PgaccelGroupedAggDesc>() as u32;
    desc.row_count = artifact.fact_rows;
    desc.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
    desc.output_mode = abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
    desc.key_count = 1;
    desc.group_capacity = artifact.group_capacity;
    desc.keys[0] = abi::PgaccelGroupedAggKey {
        values: PgaccelExprUsmCol {
            values: artifact
                .parents
                .as_ref()
                .map_or(std::ptr::null(), |parents| {
                    parents.as_ptr().cast::<c_void>()
                }),
            nulls: source_nulls,
            tag: PgaccelValTag::Int64,
        },
        lookup_by_key: std::ptr::null(),
        source: abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT,
        code_min: 0,
        cardinality: 0,
        null_code: abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE,
        flags: 0,
        pad0: 0,
    };
    desc.measure_count = 1;
    desc.measures[0] = abi::PgaccelGroupedAggMeasure {
        // SAFETY: COUNT(*) has no input columns.
        value: unsafe { std::mem::zeroed() },
        // SAFETY: COUNT(*) has no right-hand input column, so its inactive ABI
        // descriptor must be represented by the all-zero value.
        rhs: unsafe { std::mem::zeroed() },
        op: abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR,
        agg_mask: abi::PGACCEL_GROUPED_AGG_LANE_COUNT,
        accumulator_kind: abi::PGACCEL_GROUPED_AGG_ACCUM_I64,
        state_bytes: 8,
        flags: 0,
        pad0: 0,
    };
    desc.where_filter = disabled_filter();
    for filter in &mut desc.measure_filters {
        *filter = disabled_filter();
    }
    Ok(desc)
}

fn build_and_execute_spatial(
    inputs: ResolvedDerivedInputs<'_, SpatialAggArtifact>,
    requests: &[ResidentColumnRef],
    projection: &AggOutputProjection,
) -> Result<DescriptorAggDispatch, DescriptorAggExecutionError> {
    let desc = build_spatial_descriptor(inputs.artifact, requests, &inputs.columns)
        .map_err(DescriptorAggExecutionError::Failure)?;
    // SAFETY: the composite artifact owns the base descriptor lanes and final
    // SQL mask for the duration of this synchronous residency callback.
    let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }
        .map_err(|error| gpu_execution_error("spatial aggregate descriptor rejected", error))?;
    let mut storage = GroupedAggOutputStorage::new(&plan).map_err(|error| {
        gpu_execution_error("spatial aggregate output allocation failed", error)
    })?;
    let dispatch_started = Instant::now();
    let outcome = execute_grouped_agg_one_shot(&plan, &mut storage)
        .map_err(|error| gpu_execution_error("spatial aggregate kernel failed", error))?;
    let dispatch_time_us =
        u64::try_from(dispatch_started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let output = DescriptorAggOutput::new(
        storage,
        outcome,
        inputs.artifact.base.domains.clone(),
        inputs.artifact.base.resolved_spec.clone(),
        projection.clone(),
    )
    .map_err(DescriptorAggExecutionError::Failure)?;
    Ok(DescriptorAggDispatch {
        output,
        fact_rows: inputs.artifact.base.fact_rows,
        batches_executed: 1,
        dispatch_time_us,
        residency: None,
    })
}

fn build_and_execute_h3_one_shot(
    inputs: ResolvedDerivedInputs<'_, H3ParentArtifact>,
    requests: &[ResidentColumnRef],
    cell: ColumnRef,
    projection: &AggOutputProjection,
) -> Result<DescriptorAggDispatch, DescriptorAggExecutionError> {
    let desc = build_h3_descriptor(inputs.artifact, requests, &inputs.columns, cell)
        .map_err(DescriptorAggExecutionError::Failure)?;
    // SAFETY: hash/H3 is deliberately one-shot because its native owner
    // indexes are relative to this exact row pointer range. Every pointer is
    // pinned by the active residency callback through the synchronous call.
    let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }
        .map_err(|error| gpu_execution_error("H3 grouped descriptor rejected", error))?;
    let mut storage = GroupedAggOutputStorage::new(&plan)
        .map_err(|error| gpu_execution_error("H3 grouped output allocation failed", error))?;
    let dispatch_started = Instant::now();
    let outcome = execute_grouped_agg_one_shot(&plan, &mut storage)
        .map_err(|error| gpu_execution_error("H3 grouped kernel failed", error))?;
    let dispatch_time_us =
        u64::try_from(dispatch_started.elapsed().as_micros()).unwrap_or(u64::MAX);
    validate_h3_compact_outcome(outcome, &storage, inputs.artifact.fact_rows)
        .map_err(DescriptorAggExecutionError::Failure)?;
    let output = DescriptorAggOutput::new(
        storage,
        outcome,
        std::rc::Rc::from(Vec::<super::artifact::GroupDomain>::new()),
        inputs.artifact.resolved_spec.clone(),
        projection.clone(),
    )
    .map_err(DescriptorAggExecutionError::Failure)?;
    Ok(DescriptorAggDispatch {
        output,
        fact_rows: inputs.artifact.fact_rows,
        batches_executed: 1,
        dispatch_time_us,
        residency: None,
    })
}

struct DescriptorExecutionMetadata {
    fact_rows: usize,
    domains: std::rc::Rc<[super::artifact::GroupDomain]>,
    resolved_spec: AggQuerySpec,
}

enum DescriptorDispatchFailure {
    Residency(ResidentLoadError),
    Execution(DescriptorAggExecutionError),
}

enum DenseBoundedFailure {
    Dispatch(DescriptorDispatchFailure),
    Interrupt(Box<pg_sys::panic::CaughtError>),
}

enum DenseExecutionSetup {
    OneShot {
        dispatch: Box<DescriptorAggDispatch>,
        elapsed: Duration,
    },
    Bounded {
        session: Box<GroupedAggSession>,
        metadata: Box<DescriptorExecutionMetadata>,
    },
}

#[cfg(feature = "pg_test")]
static TEST_DENSE_CHUNK_ROWS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "pg_test")]
static TEST_DENSE_ONE_SHOT_MAX_ROWS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "pg_test")]
static TEST_DENSE_TIMEOUT_AFTER_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "pg_test")]
static TEST_DENSE_COMPLETED_CALLS: AtomicUsize = AtomicUsize::new(0);

/// Configure the live PostgreSQL cancellation fixture. Zero chunk and
/// one-shot overrides preserve their independent production device limits; a
/// zero timeout count disables the timeout arm. This code is absent from
/// release builds.
#[cfg(feature = "pg_test")]
pub(crate) fn configure_dense_dispatch_test(
    chunk_rows: usize,
    one_shot_max_rows: usize,
    timeout_after_calls: usize,
) -> (usize, usize, usize) {
    let previous_chunk_rows = TEST_DENSE_CHUNK_ROWS.swap(chunk_rows, Ordering::SeqCst);
    let previous_one_shot_max_rows =
        TEST_DENSE_ONE_SHOT_MAX_ROWS.swap(one_shot_max_rows, Ordering::SeqCst);
    let previous_timeout_after =
        TEST_DENSE_TIMEOUT_AFTER_CALLS.swap(timeout_after_calls, Ordering::SeqCst);
    TEST_DENSE_COMPLETED_CALLS.store(0, Ordering::SeqCst);
    (
        previous_chunk_rows,
        previous_one_shot_max_rows,
        previous_timeout_after,
    )
}

#[cfg(feature = "pg_test")]
#[must_use]
pub(crate) fn dense_dispatch_test_completed_calls() -> usize {
    TEST_DENSE_COMPLETED_CALLS.load(Ordering::SeqCst)
}

fn effective_dense_chunk_rows(device_limit: usize) -> usize {
    #[cfg(feature = "pg_test")]
    {
        let test_limit = TEST_DENSE_CHUNK_ROWS.load(Ordering::SeqCst);
        if test_limit != 0 {
            return test_limit.min(device_limit);
        }
    }
    device_limit
}

fn clamp_dense_one_shot_row_cap(device_limit: usize, test_override: usize) -> usize {
    let production_limit = device_limit.min(GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS);
    if test_override == 0 {
        production_limit
    } else {
        test_override.min(production_limit)
    }
}

fn dense_one_shot_row_cap(device_limit: usize) -> usize {
    #[cfg(feature = "pg_test")]
    let test_override = TEST_DENSE_ONE_SHOT_MAX_ROWS.load(Ordering::SeqCst);
    #[cfg(not(feature = "pg_test"))]
    let test_override = 0;
    clamp_dense_one_shot_row_cap(device_limit, test_override)
}

fn canonical_null_value(value: &PgaccelVal) -> bool {
    value.tag == PgaccelValTag::Null && value.data == 0
}

fn canonical_parallel_filter_fields(filter: &abi::PgaccelGroupedAggFilter) -> bool {
    filter.predicate_source == 0
        && filter.predicate_measure_slot == 0
        && filter.predicate_range_count == 0
        && filter.value_cmp_opcode == crate::engine::expr_compiler::opcode::ALWAYS_TRUE
        && filter.pad0 == 0
        && filter.flags == 0
        && canonical_null_value(&filter.value_cmp_const)
        && filter.predicate_lo.iter().all(canonical_null_value)
        && filter.predicate_hi.iter().all(canonical_null_value)
}

fn canonical_disabled_filter(filter: &abi::PgaccelGroupedAggFilter) -> bool {
    filter.kind == abi::PGACCEL_GROUPED_AGG_FILTER_NONE
        && filter.mask.is_null()
        && canonical_parallel_filter_fields(filter)
}

fn parallel_dense_filter_kind(filter: &abi::PgaccelGroupedAggFilter) -> bool {
    canonical_disabled_filter(filter)
        || (filter.kind == abi::PGACCEL_GROUPED_AGG_FILTER_SQL
            && !filter.mask.is_null()
            && canonical_parallel_filter_fields(filter))
}

fn parallel_dense_unique_dimensions(desc: &abi::PgaccelGroupedAggDesc) -> bool {
    let Ok(dim_count) = usize::try_from(desc.dim_count) else {
        return false;
    };
    dim_count <= abi::PGACCEL_GROUPED_AGG_MAX_DIMS
        && desc.dims[..dim_count]
            .iter()
            .all(|dim| dim.multiplicity_by_key.is_null())
}

fn parallel_dense_count_shape(desc: &abi::PgaccelGroupedAggDesc) -> bool {
    if desc.grouping_mode != abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX
        || desc.output_mode != abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE
        || desc.measure_count != 1
        || !parallel_dense_filter_kind(&desc.where_filter)
        || !parallel_dense_unique_dimensions(desc)
        || !canonical_disabled_filter(&desc.measure_filters[0])
    {
        return false;
    }
    let count = &desc.measures[0];
    count.op == abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR
        && count.agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_COUNT
        && count.accumulator_kind == abi::PGACCEL_GROUPED_AGG_ACCUM_I64
        && count.state_bytes == 8
}

fn parallel_dense_integer_shape(desc: &abi::PgaccelGroupedAggDesc) -> bool {
    if desc.grouping_mode != abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX
        || desc.output_mode != abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE
        || desc.group_capacity == 0
        || desc.measure_count != 2
        || !parallel_dense_filter_kind(&desc.where_filter)
        || !parallel_dense_unique_dimensions(desc)
        || desc.measure_filters[..2]
            .iter()
            .any(|filter| !canonical_disabled_filter(filter))
    {
        return false;
    }
    let value = &desc.measures[0];
    let count = &desc.measures[1];
    let direct_sum = value.op == abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN
        && (value.agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_SUM
            || value.agg_mask
                == (abi::PGACCEL_GROUPED_AGG_LANE_SUM
                    | abi::PGACCEL_GROUPED_AGG_LANE_MIN
                    | abi::PGACCEL_GROUPED_AGG_LANE_MAX));
    let product_sum = value.op == abi::PGACCEL_GROUPED_AGG_MEASURE_MUL
        && value.agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_SUM
        && value.rhs.physical_type == abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
        && value.rhs.element_bytes == 4;
    (direct_sum || product_sum)
        && value.value.physical_type == abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
        && value.value.element_bytes == 4
        && value.accumulator_kind == abi::PGACCEL_GROUPED_AGG_ACCUM_I64
        && value.state_bytes == 8
        && count.op == abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR
        && count.agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_COUNT
        && count.accumulator_kind == abi::PGACCEL_GROUPED_AGG_ACCUM_I64
        && count.state_bytes == 8
}

fn dense_one_shot_eligible(desc: &abi::PgaccelGroupedAggDesc, row_cap: usize) -> bool {
    if row_cap == 0 || desc.row_count > row_cap {
        return false;
    }
    if parallel_dense_count_shape(desc) {
        return true;
    }
    if !parallel_dense_integer_shape(desc) {
        return false;
    }
    if desc.measures[0].agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_SUM {
        return true;
    }
    desc.group_capacity
        .checked_mul(desc.row_count.div_ceil(DENSE_INTEGER_CHUNK_ROWS))
        .and_then(|partials| partials.checked_mul(DENSE_INTEGER_PARTIAL_BYTES))
        .is_some_and(|bytes| bytes <= DENSE_INTEGER_MAX_PARTIAL_BYTES)
}

#[cfg(feature = "pg_test")]
unsafe extern "C" {
    fn enable_timeout_after(id: std::ffi::c_int, delay_ms: std::ffi::c_int);
    fn get_timeout_indicator(id: std::ffi::c_int, reset_indicator: bool) -> bool;
}

#[cfg(feature = "pg_test")]
fn note_dense_test_call_and_maybe_arm_timeout() {
    const STATEMENT_TIMEOUT_ID: std::ffi::c_int = 3;

    let completed = TEST_DENSE_COMPLETED_CALLS.fetch_add(1, Ordering::SeqCst) + 1;
    if completed != TEST_DENSE_TIMEOUT_AFTER_CALLS.load(Ordering::SeqCst) {
        return;
    }

    // SAFETY: this pg_test-only hook runs on the initialized PostgreSQL main
    // backend thread. TimeoutId::STATEMENT_TIMEOUT has stable discriminant 3.
    unsafe { enable_timeout_after(STATEMENT_TIMEOUT_ID, 1) };
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        // SAFETY: same backend-thread contract as enable_timeout_after. The
        // indicator remains set so the normal interrupt boundary consumes it.
        if unsafe { get_timeout_indicator(STATEMENT_TIMEOUT_ID, false) } {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "PostgreSQL statement timeout did not become pending"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(not(feature = "pg_test"))]
fn note_dense_test_call_and_maybe_arm_timeout() {}

fn capture_postgres_interrupt() -> Option<pg_sys::panic::CaughtError> {
    pg_sys::PgTryBuilder::new(|| {
        pgrx::check_for_interrupts!();
        None
    })
    .catch_others(Some)
    .execute()
}

fn warn_if_grouped_dispatch_slow(phase: &str, elapsed: Duration) {
    let threshold_ms = gucs::kernel_timeout_ms();
    if !dispatch_warning_threshold_exceeded(elapsed, threshold_ms) {
        return;
    }
    pgrx::warning!(
        "pg_accel: grouped aggregate {phase} dispatch returned after {}us (warning threshold {threshold_ms}ms); cancellation and statement_timeout are observed only between synchronous calls",
        elapsed.as_micros()
    );
}

fn execute_bounded_dense_artifact(
    owner_relid: pg_sys::Oid,
    identity: &DerivedArtifactIdentity,
    requests: &[ResidentColumnRef],
    projection: &AggOutputProjection,
    max_chunk_rows: usize,
    one_shot_max_rows: usize,
) -> Result<DescriptorAggDispatch, DescriptorDispatchFailure> {
    let bounded_chunk_rows = effective_dense_chunk_rows(max_chunk_rows);
    if bounded_chunk_rows == 0 {
        return Err(DescriptorDispatchFailure::Execution(
            DescriptorAggExecutionError::Failure(
                "grouped aggregate chunk limit is zero".to_owned(),
            ),
        ));
    }
    if let Some(caught) = capture_postgres_interrupt() {
        caught.rethrow();
    }

    let initialized = with_derived_artifact_inputs::<DescriptorAggArtifact, _>(
        owner_relid,
        identity,
        requests,
        |inputs| {
            let desc = build_descriptor(inputs.artifact, requests, &inputs.columns)
                .map_err(DescriptorAggExecutionError::Failure)?;
            // SAFETY: every active pointer is pinned by this exact residency
            // callback. The session copies only shape identity; it never
            // submits or dereferences those pointer values after return.
            let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }
                .map_err(|error| gpu_execution_error("grouped descriptor rejected", error))?;
            if dense_one_shot_eligible(plan.descriptor(), dense_one_shot_row_cap(one_shot_max_rows))
            {
                let mut storage = GroupedAggOutputStorage::new(&plan).map_err(|error| {
                    gpu_execution_error("grouped one-shot output allocation failed", error)
                })?;
                let dispatch_started = Instant::now();
                let outcome =
                    execute_grouped_agg_one_shot(&plan, &mut storage).map_err(|error| {
                        gpu_execution_error("grouped one-shot kernel failed", error)
                    })?;
                let elapsed = dispatch_started.elapsed();
                let output = DescriptorAggOutput::new(
                    storage,
                    outcome,
                    inputs.artifact.domains.clone(),
                    inputs.artifact.resolved_spec.clone(),
                    projection.clone(),
                )
                .map_err(DescriptorAggExecutionError::Failure)?;
                return Ok(DenseExecutionSetup::OneShot {
                    dispatch: Box::new(DescriptorAggDispatch {
                        output,
                        fact_rows: inputs.artifact.fact_rows,
                        batches_executed: 1,
                        dispatch_time_us: u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
                        residency: None,
                    }),
                    elapsed,
                });
            }
            let session = GroupedAggSession::start(&plan, bounded_chunk_rows)
                .map_err(|error| gpu_execution_error("grouped session setup failed", error))?;
            let metadata = DescriptorExecutionMetadata {
                fact_rows: inputs.artifact.fact_rows,
                domains: inputs.artifact.domains.clone(),
                resolved_spec: inputs.artifact.resolved_spec.clone(),
            };
            Ok::<_, DescriptorAggExecutionError>(DenseExecutionSetup::Bounded {
                session: Box::new(session),
                metadata: Box::new(metadata),
            })
        },
    )
    .map_err(DescriptorDispatchFailure::Residency)?
    .map_err(DescriptorDispatchFailure::Execution)?;
    let (mut session, metadata) = match initialized {
        DenseExecutionSetup::OneShot { dispatch, elapsed } => {
            note_dense_test_call_and_maybe_arm_timeout();
            warn_if_grouped_dispatch_slow("one-shot", elapsed);
            return match capture_postgres_interrupt() {
                Some(caught) => cleanup_before_rethrow(dispatch, caught, |caught| caught.rethrow()),
                None => Ok(*dispatch),
            };
        }
        DenseExecutionSetup::Bounded { session, metadata } => (session, metadata),
    };
    let DescriptorExecutionMetadata {
        fact_rows,
        domains,
        resolved_spec,
    } = *metadata;

    let dispatch_started = Instant::now();
    let dispatch = run_bounded_dispatch(
        fact_rows,
        bounded_chunk_rows,
        |range| {
            let elapsed = with_derived_artifact_inputs::<DescriptorAggArtifact, _>(
                owner_relid,
                identity,
                requests,
                |inputs| {
                    let desc = build_descriptor(inputs.artifact, requests, &inputs.columns)
                        .map_err(DescriptorAggExecutionError::Failure)?;
                    // SAFETY: the descriptor is used only during this synchronous
                    // callback while all row pointers remain pinned.
                    let plan =
                        unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.map_err(|error| {
                            gpu_execution_error("grouped descriptor changed between chunks", error)
                        })?;
                    let chunk =
                        plan.row_chunk(range.first_row, range.row_count)
                            .map_err(|error| {
                                gpu_execution_error("grouped chunk descriptor rejected", error)
                            })?;
                    let chunk_started = Instant::now();
                    session.accumulate(&chunk).map_err(|error| {
                        gpu_execution_error("grouped chunk kernel failed", error)
                    })?;
                    Ok::<_, DescriptorAggExecutionError>(chunk_started.elapsed())
                },
            )
            .map_err(DescriptorDispatchFailure::Residency)
            .and_then(|result| result.map_err(DescriptorDispatchFailure::Execution))
            .map_err(DenseBoundedFailure::Dispatch)?;
            warn_if_grouped_dispatch_slow("accumulate chunk", elapsed);
            note_dense_test_call_and_maybe_arm_timeout();
            Ok(())
        },
        || {
            // This closure runs only after the preceding residency callback
            // returned and its RefCell guard was dropped.
            capture_postgres_interrupt().map_or(Ok(()), |caught| {
                Err(DenseBoundedFailure::Interrupt(Box::new(caught)))
            })
        },
    );
    let launches = match dispatch {
        Ok(launches) => launches,
        Err(BoundedDispatchError::ZeroChunkLimit) => unreachable!("checked before setup"),
        Err(
            BoundedDispatchError::Dispatch(error) | BoundedDispatchError::InterruptBoundary(error),
        ) => match error {
            DenseBoundedFailure::Interrupt(caught) => {
                return cleanup_before_rethrow(session, caught, |caught| (*caught).rethrow());
            }
            DenseBoundedFailure::Dispatch(error) => return Err(error),
        },
    };

    let (outcome, storage, finalize_elapsed) = with_derived_artifact_inputs::<
        DescriptorAggArtifact,
        _,
    >(owner_relid, identity, requests, |inputs| {
        let desc = build_descriptor(inputs.artifact, requests, &inputs.columns)
            .map_err(DescriptorAggExecutionError::Failure)?;
        // SAFETY: finalization is synchronous and this fresh plan remains
        // pinned until the callback returns.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.map_err(|error| {
            gpu_execution_error("grouped descriptor changed before finalize", error)
        })?;
        // Output buffers carry an exact resolved-plan identity. Allocate
        // them from this final pinned plan, not the temporary setup plan
        // that was dropped before the bounded residency borrows began.
        let mut storage = GroupedAggOutputStorage::new(&plan).map_err(|error| {
            gpu_execution_error("grouped final output allocation failed", error)
        })?;
        let finalize_started = Instant::now();
        let outcome = session
            .finalize(&plan, &mut storage)
            .map_err(|error| gpu_execution_error("grouped finalization failed", error))?;
        Ok::<_, DescriptorAggExecutionError>((outcome, storage, finalize_started.elapsed()))
    })
    .map_err(DescriptorDispatchFailure::Residency)?
    .map_err(DescriptorDispatchFailure::Execution)?;
    note_dense_test_call_and_maybe_arm_timeout();
    warn_if_grouped_dispatch_slow("finalize", finalize_elapsed);
    // Finalize is another synchronous native call. Capture only after its
    // store guard has been released, then release detached resources before
    // preserving the original PostgreSQL error and SQLSTATE.
    if let Some(caught) = capture_postgres_interrupt() {
        return cleanup_before_rethrow((session, storage), caught, |caught| caught.rethrow());
    }

    let dispatch_time_us =
        u64::try_from(dispatch_started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let completed_calls =
        bounded_dispatch_call_count(fact_rows, bounded_chunk_rows).ok_or_else(|| {
            DescriptorDispatchFailure::Execution(DescriptorAggExecutionError::Failure(
                "grouped aggregate bounded dispatch call count overflowed".to_owned(),
            ))
        })?;
    debug_assert_eq!(completed_calls, launches.saturating_add(1));
    let output =
        DescriptorAggOutput::new(storage, outcome, domains, resolved_spec, projection.clone())
            .map_err(DescriptorAggExecutionError::Failure)
            .map_err(DescriptorDispatchFailure::Execution)?;
    Ok(DescriptorAggDispatch {
        output,
        fact_rows,
        batches_executed: u64::try_from(completed_calls).unwrap_or(u64::MAX),
        dispatch_time_us,
        residency: None,
    })
}

fn validate_h3_compact_outcome(
    outcome: GroupedAggOutcome,
    storage: &GroupedAggOutputStorage,
    fact_rows: usize,
) -> Result<(), String> {
    let result = match outcome {
        GroupedAggOutcome::Complete(result) => result,
        GroupedAggOutcome::NeedsRecheck(result) => {
            return Err(format!(
                "H3 grouped aggregate returned {} uncertain rows",
                result.uncertain_count
            ));
        }
    };
    if result.uncertain_count != 0 {
        return Err(format!(
            "complete H3 grouped aggregate contains {} uncertain rows",
            result.uncertain_count
        ));
    }
    let expected_selected = u64::try_from(fact_rows)
        .map_err(|_| "H3 fact row count exceeds the u64 result domain".to_owned())?;
    if result.selected_count != expected_selected {
        return Err(format!(
            "H3 grouped aggregate selected {} rows, expected {expected_selected}",
            result.selected_count
        ));
    }
    if result.emitted_group_count > result.group_capacity {
        return Err("H3 emitted group count exceeds group capacity".to_owned());
    }
    let counts = storage
        .measure_count(0)
        .ok_or_else(|| "H3 compact output is missing its COUNT(*) lane".to_owned())?;
    let key_values = storage
        .key_values(0)
        .ok_or_else(|| "H3 compact output is missing its key lane".to_owned())?;
    validate_h3_compact_key_buffers(
        key_values,
        storage.key_nulls(0),
        result.group_capacity,
        result.emitted_group_count,
    )?;
    validate_h3_count_partition(
        counts,
        result.group_capacity,
        result.emitted_group_count,
        result.selected_count,
    )
}

fn validate_h3_count_partition(
    counts: &[u64],
    group_capacity: usize,
    emitted_group_count: usize,
    selected_count: u64,
) -> Result<(), String> {
    if counts.len() != group_capacity {
        return Err(format!(
            "H3 COUNT(*) lane has length {}, expected {}",
            counts.len(),
            group_capacity
        ));
    }
    if emitted_group_count > group_capacity {
        return Err("H3 emitted group count exceeds group capacity".to_owned());
    }
    let (emitted, unused) = counts.split_at(emitted_group_count);
    if emitted.contains(&0) {
        return Err("H3 compact output contains an emitted zero-count group".to_owned());
    }
    let counted_rows = emitted.iter().try_fold(0_u64, |total, count| {
        total
            .checked_add(*count)
            .ok_or_else(|| "H3 compact output COUNT(*) sum overflows u64".to_owned())
    })?;
    if counted_rows != selected_count {
        return Err(format!(
            "H3 compact output counts sum to {counted_rows}, expected {selected_count} selected rows"
        ));
    }
    if unused.iter().any(|count| *count != 0) {
        return Err("H3 compact output contains nonzero unused COUNT(*) state".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::residency::ResidentInputBundle;
    use crate::engine::spec::{
        AggOutputSlot, AggOutputSource, AggregateOutput, DimSpec, GroupKeyRef, MeasureSpec,
        ScalarRange, SpatialPredicateKind, SpatialValueKind, SpatialValueMetadata,
    };

    const TEST_GEOMETRY_OID: u32 = 60_001;
    const TEST_SRID: i32 = 4_326;

    fn parallel_dense_integer_desc(
        row_count: usize,
        group_capacity: usize,
    ) -> abi::PgaccelGroupedAggDesc {
        // SAFETY: zero is the canonical inactive representation for every ABI
        // field; the active shape fields are populated below.
        let mut desc: abi::PgaccelGroupedAggDesc = unsafe { std::mem::zeroed() };
        desc.row_count = row_count;
        desc.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX;
        desc.output_mode = abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
        desc.group_capacity = group_capacity;
        desc.measure_count = 2;
        desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN;
        desc.measures[0].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_SUM;
        desc.measures[0].accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_I64;
        desc.measures[0].state_bytes = 8;
        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
        desc.measures[0].value.element_bytes = 4;
        desc.measures[1].op = abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
        desc.measures[1].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        desc.measures[1].accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_I64;
        desc.measures[1].state_bytes = 8;
        desc.where_filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        for filter in &mut desc.measure_filters {
            filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        }
        desc
    }

    fn column(type_oid: u32) -> ColumnRef {
        ColumnRef {
            relation_oid: 42,
            attno: 1,
            type_oid,
        }
    }

    #[test]
    fn dense_one_shot_branch_obeys_row_and_ordered_partial_budgets() {
        let release_shape = parallel_dense_integer_desc(1_000_000, 350);
        assert!(dense_one_shot_eligible(
            &release_shape,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));

        let chunks = release_shape.row_count.div_ceil(DENSE_INTEGER_CHUNK_ROWS);
        let max_groups = DENSE_INTEGER_MAX_PARTIAL_BYTES / (chunks * DENSE_INTEGER_PARTIAL_BYTES);
        let over_budget = parallel_dense_integer_desc(1_000_000, max_groups + 1);
        assert!(dense_one_shot_eligible(
            &over_budget,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));
        let mut ordered_over_budget = over_budget;
        ordered_over_budget.measures[0].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_SUM
            | abi::PGACCEL_GROUPED_AGG_LANE_MIN
            | abi::PGACCEL_GROUPED_AGG_LANE_MAX;
        assert!(!dense_one_shot_eligible(
            &ordered_over_budget,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));

        assert!(!dense_one_shot_eligible(&release_shape, 999_999));
        let oversized = parallel_dense_integer_desc(1_000_001, 350);
        assert!(!dense_one_shot_eligible(
            &oversized,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));

        let mut unsupported = release_shape;
        unsupported.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_SUB;
        assert!(!dense_one_shot_eligible(
            &unsupported,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));

        let mask = [1_i8];
        let mut noncanonical = release_shape;
        noncanonical.where_filter.kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        noncanonical.where_filter.mask = mask.as_ptr();
        assert!(dense_one_shot_eligible(
            &noncanonical,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));
        noncanonical.where_filter.predicate_range_count = 1;
        assert!(!dense_one_shot_eligible(
            &noncanonical,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));
    }

    #[test]
    fn dense_count_one_shot_honors_explicit_limit() {
        let mut desc = parallel_dense_integer_desc(10, 1);
        desc.measure_count = 1;
        desc.measures[0] = desc.measures[1];
        assert!(dense_one_shot_eligible(&desc, 10));
        assert!(!dense_one_shot_eligible(&desc, 4));
    }

    #[test]
    fn one_shot_limit_boundary_preserves_bounded_cancellation_contract() {
        const DEVICE_ONE_SHOT_LIMIT: usize = 250_000;
        const SESSION_CHUNK_ROWS: usize = 65_536;

        assert_eq!(
            clamp_dense_one_shot_row_cap(DEVICE_ONE_SHOT_LIMIT, 0),
            DEVICE_ONE_SHOT_LIMIT
        );
        assert_eq!(
            clamp_dense_one_shot_row_cap(DEVICE_ONE_SHOT_LIMIT, 1),
            1,
            "the pg_test one-shot override is independent of session chunking"
        );
        assert_eq!(
            clamp_dense_one_shot_row_cap(GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS + 1, 0),
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        );

        let at_limit = parallel_dense_integer_desc(DEVICE_ONE_SHOT_LIMIT, 350);
        assert!(dense_one_shot_eligible(&at_limit, DEVICE_ONE_SHOT_LIMIT));
        let above_limit = parallel_dense_integer_desc(DEVICE_ONE_SHOT_LIMIT + 1, 350);
        assert!(!dense_one_shot_eligible(
            &above_limit,
            DEVICE_ONE_SHOT_LIMIT
        ));
        assert_eq!(
            bounded_dispatch_call_count(above_limit.row_count, SESSION_CHUNK_ROWS),
            Some(5),
            "limit+1 must use four bounded calls plus finalize, leaving interrupt boundaries"
        );
    }

    fn projection(
        kind: AggregateKind,
        source_type_oid: u32,
        result_type_oid: u32,
    ) -> AggOutputProjection {
        AggOutputProjection {
            slots: vec![AggOutputSlot {
                source: AggOutputSource::Aggregate {
                    measure_index: 0,
                    source: AggregateSource::Value,
                    kind,
                },
                source_type_oid,
                result_type_oid,
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: kind != AggregateKind::Count,
            }],
        }
    }

    fn spec(expression: MeasureExpr, kind: AggregateKind) -> AggQuerySpec {
        AggQuerySpec {
            fact_rel: 42,
            group_keys: Vec::new(),
            measures: vec![MeasureSpec {
                expression,
                outputs: vec![AggregateOutput {
                    source: AggregateSource::Value,
                    kind,
                }],
                filter: FilterSpec::None,
            }],
            fact_filter: FilterSpec::None,
            star_dims: Vec::new(),
            having: None,
        }
    }

    fn h3_count_shape() -> (AggQuerySpec, AggOutputProjection) {
        const H3OID: u32 = 90_001;
        let mut spec = spec(MeasureExpr::CountStar, AggregateKind::Count);
        spec.group_keys.push(GroupKeyRef {
            source: GroupKeySource::H3CellToParent {
                cell: column(H3OID),
                resolution: 7,
            },
            type_oid: H3OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        });
        let projection = AggOutputProjection {
            slots: vec![
                AggOutputSlot {
                    source: AggOutputSource::GroupKey { key_index: 0 },
                    source_type_oid: H3OID,
                    result_type_oid: H3OID,
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: true,
                },
                AggOutputSlot {
                    source: AggOutputSource::Aggregate {
                        measure_index: 0,
                        source: AggregateSource::Value,
                        kind: AggregateKind::Count,
                    },
                    source_type_oid: 0,
                    result_type_oid: INT8OID,
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: false,
                },
            ],
        };
        (spec, projection)
    }

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

    fn spatial_count_shape() -> (AggQuerySpec, AggOutputProjection) {
        let mut spec = spec(MeasureExpr::CountStar, AggregateKind::Count);
        spec.fact_filter = FilterSpec::Spatial {
            predicate: SpatialPredicateKind::Intersects,
            left: SpatialOperand::Column {
                column: column(TEST_GEOMETRY_OID),
                metadata: SpatialValueMetadata {
                    kind: SpatialValueKind::Geometry,
                    typmod: geometry_typmod(
                        crate::engine::residency::RESIDENT_GEOMETRY_POINT,
                        TEST_SRID,
                    ),
                    srid: Some(TEST_SRID),
                },
            },
            right: SpatialOperand::Constant {
                metadata: SpatialValueMetadata {
                    kind: SpatialValueKind::Geometry,
                    typmod: -1,
                    srid: Some(TEST_SRID),
                },
                bytes: polygon_bytes(TEST_SRID),
            },
            distance: None,
        };
        (
            spec,
            projection(AggregateKind::Count, 0, u32::from(pg_sys::INT8OID)),
        )
    }

    fn postgis_catalog(fingerprint_words: Vec<i32>) -> PostgisCatalogIdentity {
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
            fingerprint_words,
        }
    }

    #[test]
    fn spatial_capability_is_internal_while_planner_validation_stays_dark() {
        let (spec, projection) = spatial_count_shape();
        assert!(validate_runtime_capability(&spec, &projection).is_err());
        validate_internal_runtime_capability(&spec, &projection)
            .expect("executor-internal spatial capability should bind");
    }

    #[test]
    fn spatial_kind_requests_source_and_fingerprint_changes_identity() {
        let (spec, projection) = spatial_count_shape();
        let catalog = postgis_catalog(vec![101, 202, 303]);
        let kind =
            spatial_artifact_kind(&spec, &catalog, 1_024).expect("valid spatial kind should bind");
        let DescriptorArtifactKind::Spatial(plan) = kind else {
            panic!("spatial spec selected a nonspatial artifact kind");
        };
        assert_eq!(
            plan.column_request(),
            ResidentColumnRef {
                relid: pg_sys::Oid::from(42_u32),
                attno: 1,
            }
        );
        let requests = artifact_column_refs(&spec).expect("spatial artifact requests bind");
        assert!(requests.contains(&plan.column_request()));

        let first = descriptor_artifact_identity(&spec, &projection, &catalog.fingerprint_words)
            .expect("first identity encodes");
        let second = descriptor_artifact_identity(&spec, &projection, &[101, 202, 304])
            .expect("second identity encodes");
        assert_ne!(first, second);
        assert!(
            first
                .canonical_words()
                .ends_with(&catalog.fingerprint_words)
        );
    }

    #[test]
    fn catalog_verification_precedes_guarded_operation() {
        let calls = std::cell::RefCell::new(Vec::new());
        let outcome = verify_catalog_before(
            || {
                calls.borrow_mut().push("verify");
                Ok(())
            },
            || {
                calls.borrow_mut().push("ensure");
                Ok(ArtifactEnsureOutcome::Hit)
            },
        )
        .expect("verified cache hit should pass");
        assert_eq!(outcome, ArtifactEnsureOutcome::Hit);
        assert_eq!(*calls.borrow(), ["verify", "ensure"]);

        let ensure_called = std::cell::Cell::new(false);
        let error = verify_catalog_before(
            || Err(ResidentLoadError::Loader("catalog changed".to_owned())),
            || {
                ensure_called.set(true);
                Ok(ArtifactEnsureOutcome::Hit)
            },
        )
        .expect_err("catalog mismatch must reject before cache lookup");
        assert!(!ensure_called.get());
        assert!(error.to_string().contains("catalog changed"));
    }

    #[test]
    fn spatial_mask_binding_rechecks_empty_and_nonempty_shape() {
        assert_eq!(spatial_final_mask_binding(0, None), Ok(None));

        let mask = [1_i8, -1_i8];
        let mask_ptr = mask.as_ptr();
        assert_eq!(
            spatial_final_mask_binding(mask.len(), Some((mask.len(), mask_ptr))),
            Ok(Some(mask_ptr))
        );
        assert!(spatial_final_mask_binding(2, Some((1, mask_ptr))).is_err());
        assert!(spatial_final_mask_binding(2, Some((2, std::ptr::null()))).is_err());
        assert!(spatial_final_mask_binding(2, None).is_err());
        assert!(spatial_final_mask_binding(0, Some((0, mask_ptr))).is_err());

        let empty = build_sql_mask_filter(0, None).expect("zero rows disable the filter");
        assert_eq!(empty.kind, abi::PGACCEL_GROUPED_AGG_FILTER_NONE);
        assert!(empty.mask.is_null());
        let bound = build_sql_mask_filter(mask.len(), Some(mask_ptr))
            .expect("nonempty mask should bind as SQL filter");
        assert_eq!(bound.kind, abi::PGACCEL_GROUPED_AGG_FILTER_SQL);
        assert_eq!(bound.mask, mask_ptr);
    }

    #[test]
    fn runtime_backstop_accepts_only_exact_h3_parent_count_shape() {
        let (spec, projection) = h3_count_shape();
        validate_runtime_capability(&spec, &projection).expect("exact H3 COUNT(*) shape");

        let mut wrong = spec.clone();
        wrong.measures[0].outputs[0].kind = AggregateKind::Sum;
        assert!(validate_runtime_capability(&wrong, &projection).is_err());

        let mut wrong = spec;
        let GroupKeySource::H3CellToParent { resolution, .. } = &mut wrong.group_keys[0].source
        else {
            unreachable!()
        };
        *resolution = 16;
        assert!(validate_runtime_capability(&wrong, &projection).is_err());
    }

    #[test]
    fn h3_group_capacity_obeys_universe_and_runtime_bounds() {
        assert_eq!(h3_group_capacity(0, 0, usize::MAX), Ok(1));
        assert_eq!(h3_group_capacity(1_000, 0, usize::MAX), Ok(123));
        assert_eq!(h3_group_capacity(7, 15, usize::MAX), Ok(7));
        assert_eq!(h3_group_capacity(1_000, 15, 19), Ok(19));
        assert_eq!(
            h3_group_capacity((1_usize << 30) + 1, 15, usize::MAX),
            Ok(1_usize << 30)
        );
        assert!(h3_group_capacity(1, -1, usize::MAX).is_err());
        assert!(h3_group_capacity(1, 16, usize::MAX).is_err());
    }

    #[test]
    fn h3_count_partition_rejects_invalid_compact_state() {
        validate_h3_count_partition(&[2, 3, 0], 3, 2, 5)
            .expect("valid emitted counts with a zero tail");
        assert!(validate_h3_count_partition(&[2, 0, 0], 3, 2, 2).is_err());
        assert!(validate_h3_count_partition(&[u64::MAX, 1], 2, 2, u64::MAX).is_err());
        assert!(validate_h3_count_partition(&[2, 3, 0], 3, 2, 6).is_err());
        assert!(validate_h3_count_partition(&[2, 3, 1], 3, 2, 5).is_err());
    }

    #[test]
    fn runtime_backstop_accepts_int4_binary_sum() {
        let mut rhs = column(INT4OID);
        rhs.attno = 2;
        let spec = spec(
            MeasureExpr::Binary {
                op: BinaryMeasureOp::Mul,
                lhs: column(INT4OID),
                rhs,
            },
            AggregateKind::Sum,
        );
        validate_runtime_capability(&spec, &projection(AggregateKind::Sum, INT4OID, INT8OID))
            .expect("INT4 multiplication has checked PostgreSQL overflow semantics");
    }

    #[test]
    fn runtime_backstop_declines_int8_multiplication() {
        let mut rhs = column(INT8OID);
        rhs.attno = 2;
        let spec = spec(
            MeasureExpr::Binary {
                op: BinaryMeasureOp::Mul,
                lhs: column(INT8OID),
                rhs,
            },
            AggregateKind::Count,
        );
        assert!(
            validate_runtime_capability(&spec, &projection(AggregateKind::Count, INT8OID, INT8OID))
                .is_err()
        );
    }

    #[test]
    fn runtime_backstop_requires_unresolved_dictionary_keys() {
        let mut spec = spec(MeasureExpr::CountStar, AggregateKind::Count);
        spec.group_keys.push(GroupKeyRef {
            source: GroupKeySource::FactColumn(column(INT4OID)),
            type_oid: INT4OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::DenseI32 {
                code_min: 0,
                cardinality: 1,
                null_code: None,
            },
        });
        let projection = AggOutputProjection {
            slots: vec![AggOutputSlot {
                source: AggOutputSource::GroupKey { key_index: 0 },
                source_type_oid: INT4OID,
                result_type_oid: INT4OID,
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: true,
            }],
        };
        assert!(validate_runtime_capability(&spec, &projection).is_err());
    }

    #[test]
    fn logical_temporal_scalars_map_to_distinct_abi_tags() {
        let date = scalar_value(ScalarValue::Date(-7));
        let timestamp = scalar_value(ScalarValue::Timestamp(-11));
        let timestamptz = scalar_value(ScalarValue::TimestampTz(13));
        assert_eq!(date.tag, PgaccelValTag::Date);
        assert_eq!(date.data, (-7_i32) as u64);
        assert_eq!(timestamp.tag, PgaccelValTag::Timestamp);
        assert_eq!(timestamp.data, (-11_i64) as u64);
        assert_eq!(timestamptz.tag, PgaccelValTag::Timestamp);
        assert_eq!(timestamptz.data, 13);
    }

    #[test]
    fn predicate_only_columns_use_exact_physical_types() {
        for (type_oid, expected_physical, expected_bytes) in [
            (BOOLOID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL, 1),
            (FLOAT4OID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32, 4),
            (DATEOID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE, 4),
            (TIMESTAMPOID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP, 8),
            (
                TIMESTAMPTZOID,
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP,
                8,
            ),
        ] {
            let column = measure_column(
                &ResidentColumnView::Empty {
                    type_oid: pg_sys::Oid::from(type_oid),
                },
                0,
            )
            .expect("predicate-only type binds");
            assert_eq!(column.physical_type, expected_physical);
            assert_eq!(column.element_bytes, expected_bytes);
        }
    }

    #[test]
    fn runtime_backstop_accepts_hidden_temporal_and_float4_counts() {
        let cases = [
            (FLOAT4OID, ScalarValue::F32(1.0), ScalarValue::F32(2.0)),
            (DATEOID, ScalarValue::Date(1), ScalarValue::Date(2)),
            (
                TIMESTAMPOID,
                ScalarValue::Timestamp(1),
                ScalarValue::Timestamp(2),
            ),
            (
                TIMESTAMPTZOID,
                ScalarValue::TimestampTz(1),
                ScalarValue::TimestampTz(2),
            ),
        ];
        for (type_oid, lo, hi) in cases {
            let mut spec = spec(MeasureExpr::CountStar, AggregateKind::Count);
            let input = column(type_oid);
            spec.measures.push(MeasureSpec {
                expression: MeasureExpr::Column(input),
                outputs: vec![AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                }],
                filter: FilterSpec::None,
            });
            spec.fact_filter = FilterSpec::Ranges {
                input,
                ranges: vec![crate::engine::spec::ScalarRange { lo, hi }],
            };
            validate_runtime_capability(&spec, &projection(AggregateKind::Count, 0, INT8OID))
                .unwrap_or_else(|error| panic!("type OID {type_oid} should bind: {error}"));
        }
    }

    #[test]
    fn only_typed_gpu_numeric_overflow_reaches_sql_overflow_path() {
        let numeric = gpu_execution_error(
            "kernel failed",
            GpuError::new(
                crate::gpu::GpuErrorDomain::GroupedAgg,
                crate::gpu::GpuOperation::Kernel("grouped_agg_execute"),
                GpuStatusDetail::NumericOverflow,
            ),
        );
        assert_eq!(numeric, DescriptorAggExecutionError::NumericOverflow);

        let invalid = gpu_execution_error(
            "kernel failed",
            GpuError::new(
                crate::gpu::GpuErrorDomain::GroupedAgg,
                crate::gpu::GpuOperation::Kernel("grouped_agg_execute"),
                GpuStatusDetail::ExecutionFailed,
            ),
        );
        let DescriptorAggExecutionError::Failure(message) = invalid else {
            panic!("invalid runtime metadata must stay a generic execution failure")
        };
        assert!(message.contains("kernel failed"));
        assert!(message.contains("execution_failed"));
    }

    #[test]
    fn only_algorithmic_h3_parent_errors_are_external_routine_exceptions() {
        for status in [
            GpuStatusDetail::InvalidArgument,
            GpuStatusDetail::ShapeMismatch,
        ] {
            let error = DescriptorAggExecutionError::from(ResidentLoadError::Gpu(GpuError::new(
                GpuErrorDomain::H3,
                GpuOperation::Kernel("h3_cell_to_parent_resident"),
                status,
            )));
            assert!(matches!(
                error,
                DescriptorAggExecutionError::ExternalRoutineException(_)
            ));
        }

        for error in [
            GpuError::new(
                GpuErrorDomain::H3,
                GpuOperation::Kernel("h3_cell_to_parent_resident"),
                GpuStatusDetail::NoDevice,
            ),
            GpuError::new(
                GpuErrorDomain::H3,
                GpuOperation::Kernel("another_h3_kernel"),
                GpuStatusDetail::InvalidArgument,
            ),
            GpuError::new(
                GpuErrorDomain::H3,
                GpuOperation::Kernel("h3_cell_to_parent_resident"),
                GpuStatusDetail::InvalidDescriptor,
            ),
            GpuError::new(
                GpuErrorDomain::GroupedAgg,
                GpuOperation::Kernel("h3_cell_to_parent_resident"),
                GpuStatusDetail::InvalidArgument,
            ),
        ] {
            assert!(matches!(
                DescriptorAggExecutionError::from(ResidentLoadError::Gpu(error)),
                DescriptorAggExecutionError::Failure(_)
            ));
        }
    }

    #[test]
    fn execution_errors_and_residency_reports_preserve_semantics() {
        assert_eq!(
            DescriptorAggExecutionError::NumericOverflow.to_string(),
            "numeric value out of range"
        );
        assert_eq!(
            DescriptorAggExecutionError::ExternalRoutineException("external".to_owned())
                .to_string(),
            "external"
        );
        assert_eq!(
            DescriptorAggExecutionError::from("failure".to_owned()),
            DescriptorAggExecutionError::Failure("failure".to_owned())
        );

        let mut report = DescriptorResidencyReport {
            artifact_outcome: ArtifactEnsureOutcome::Hit,
            relations: Vec::new(),
            loaded_relations: vec![pg_sys::Oid::from(10_u32)],
            artifact_bytes: 10,
            raw_load_ms: 1.5,
            preparation_time_us: 7,
        };
        report.merge(DescriptorResidencyReport {
            artifact_outcome: ArtifactEnsureOutcome::Built,
            relations: Vec::new(),
            loaded_relations: vec![pg_sys::Oid::from(10_u32), pg_sys::Oid::from(20_u32)],
            artifact_bytes: 20,
            raw_load_ms: 2.5,
            preparation_time_us: 11,
        });
        assert_eq!(report.artifact_outcome, ArtifactEnsureOutcome::Built);
        assert_eq!(
            report.loaded_relations,
            vec![pg_sys::Oid::from(10_u32), pg_sys::Oid::from(20_u32)]
        );
        assert_eq!(report.artifact_bytes, 20);
        assert_eq!(report.raw_load_ms, 4.0);
        assert_eq!(report.preparation_time_us, 18);

        report.merge(DescriptorResidencyReport {
            artifact_outcome: ArtifactEnsureOutcome::Hit,
            relations: Vec::new(),
            loaded_relations: Vec::new(),
            artifact_bytes: 21,
            raw_load_ms: 0.0,
            preparation_time_us: u64::MAX,
        });
        assert_eq!(report.artifact_outcome, ArtifactEnsureOutcome::Built);
        assert_eq!(report.artifact_bytes, 21);
        assert_eq!(report.preparation_time_us, u64::MAX);
    }

    #[test]
    fn h3_count_partition_reports_shape_and_capacity_errors() {
        assert_eq!(
            validate_h3_count_partition(&[1], 2, 1, 1),
            Err("H3 COUNT(*) lane has length 1, expected 2".to_owned())
        );
        assert_eq!(
            validate_h3_count_partition(&[1], 1, 2, 1),
            Err("H3 emitted group count exceeds group capacity".to_owned())
        );
    }

    fn relation_evidence(relid: u32) -> ResidentRelationEvidence {
        ResidentRelationEvidence {
            relid: pg_sys::Oid::from(relid),
            generation: 1,
            global_generation: 1,
            relfilenode: pg_sys::Oid::from(relid + 10_000),
            row_count: 0,
            raw_bytes: 0,
            raw_accounting: ResidentByteAccounting {
                device_bytes: 0,
                retained_host_exact_bytes: 0,
            },
            derived_bytes: 0,
            loaded_at_us: 0,
            last_used_us: 0,
            load_ms: 0.0,
        }
    }

    fn zero_artifact(spec: &AggQuerySpec, artifact_types: &[u32]) -> DescriptorAggArtifact {
        let requests = artifact_column_refs(spec).expect("artifact requests resolve");
        assert_eq!(requests.len(), artifact_types.len());
        let columns = artifact_types
            .iter()
            .map(|type_oid| ResidentColumnView::Empty {
                type_oid: pg_sys::Oid::from(*type_oid),
            })
            .collect();
        let relids = dependency_relids(spec);
        let prepared = prepare_agg_artifact(
            spec,
            &requests,
            ResidentInputBundle {
                columns,
                evidence: relids
                    .into_iter()
                    .map(|relid| relation_evidence(u32::from(relid)))
                    .collect(),
            },
            1_024,
        )
        .expect("zero-row artifact prepares");
        assert_eq!(prepared.device_bytes, 0);
        DescriptorAggArtifact::build(prepared.prepared).expect("empty lanes require no device")
    }

    fn zero_descriptor(
        spec: &AggQuerySpec,
        artifact_types: &[u32],
        dispatch_types: &[u32],
    ) -> abi::PgaccelGroupedAggDesc {
        let artifact = zero_artifact(spec, artifact_types);
        let requests = dispatch_column_refs(&artifact.resolved_spec).expect("dispatch requests");
        assert_eq!(requests.len(), dispatch_types.len());
        let views = dispatch_types
            .iter()
            .map(|type_oid| ResidentColumnView::Empty {
                type_oid: pg_sys::Oid::from(*type_oid),
            })
            .collect::<Vec<_>>();
        build_descriptor(&artifact, &requests, &views).expect("zero descriptor binds")
    }

    fn output(source: AggregateSource, kind: AggregateKind) -> AggregateOutput {
        AggregateOutput { source, kind }
    }

    #[test]
    fn primitive_capability_checks_cover_supported_and_rejected_domains() {
        for oid in [TEXTOID, VARCHAROID, BPCHAROID] {
            assert!(is_text_family(oid));
            assert!(is_supported_group_type(oid));
        }
        for oid in [
            BOOLOID,
            INT2OID,
            INT4OID,
            INT8OID,
            FLOAT4OID,
            FLOAT8OID,
            DATEOID,
            TIMESTAMPOID,
            TIMESTAMPTZOID,
        ] {
            assert!(!is_text_family(oid));
            assert!(is_supported_group_type(oid));
        }
        assert!(!is_supported_group_type(60_001));

        let range = |input| FilterSpec::Ranges {
            input,
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(1),
                hi: ScalarValue::I32(2),
            }],
        };
        assert!(validate_filter(&FilterSpec::None, 42, false, false).is_ok());
        assert!(validate_filter(&range(column(INT4OID)), 42, false, false).is_ok());
        assert!(validate_filter(&range(column(BOOLOID)), 42, true, false).is_ok());
        assert!(validate_filter(&range(column(INT2OID)), 42, true, false).is_err());
        assert!(validate_filter(&range(column(INT4OID)), 99, false, false).is_err());
        assert!(
            validate_filter(
                &FilterSpec::Mask {
                    input: column(BOOLOID),
                    kind: MaskKind::Sql
                },
                42,
                false,
                false,
            )
            .is_ok()
        );
        assert!(
            validate_filter(
                &FilterSpec::Mask {
                    input: column(INT4OID),
                    kind: MaskKind::Sql
                },
                42,
                false,
                false,
            )
            .is_err()
        );
        let spatial = spatial_count_shape().0.fact_filter;
        assert!(validate_filter(&spatial, 42, false, true).is_ok());
        assert!(validate_filter(&spatial, 42, false, false).is_err());

        let all_basic = [
            output(AggregateSource::Value, AggregateKind::Sum),
            output(AggregateSource::Value, AggregateKind::Count),
            output(AggregateSource::Value, AggregateKind::Min),
            output(AggregateSource::Value, AggregateKind::Max),
        ];
        assert!(validate_measure_outputs(&MeasureExpr::CountStar, &all_basic[1..2]).is_ok());
        assert!(validate_measure_outputs(&MeasureExpr::CountStar, &all_basic[..1]).is_err());
        assert!(
            validate_measure_outputs(&MeasureExpr::Column(column(INT4OID)), &all_basic).is_ok()
        );
        assert!(
            validate_measure_outputs(
                &MeasureExpr::Column(column(INT4OID)),
                &[output(AggregateSource::Value, AggregateKind::Avg)],
            )
            .is_err()
        );
        for oid in [INT8OID, FLOAT8OID] {
            assert!(
                validate_measure_outputs(
                    &MeasureExpr::Column(column(oid)),
                    &[all_basic[1], all_basic[2], all_basic[3]],
                )
                .is_ok()
            );
            assert!(
                validate_measure_outputs(&MeasureExpr::Column(column(oid)), &all_basic[..1],)
                    .is_err()
            );
        }
        for oid in [BOOLOID, FLOAT4OID, DATEOID, TIMESTAMPOID, TIMESTAMPTZOID] {
            assert!(
                validate_measure_outputs(&MeasureExpr::Column(column(oid)), &all_basic[1..2],)
                    .is_ok()
            );
        }
        let mut rhs = column(INT4OID);
        rhs.attno = 2;
        for op in [BinaryMeasureOp::Mul, BinaryMeasureOp::Sub] {
            assert!(
                validate_measure_outputs(
                    &MeasureExpr::Binary {
                        op,
                        lhs: column(INT4OID),
                        rhs
                    },
                    &all_basic,
                )
                .is_ok()
            );
        }
        let mut rhs64 = column(INT8OID);
        rhs64.attno = 2;
        assert!(
            validate_measure_outputs(
                &MeasureExpr::Binary {
                    op: BinaryMeasureOp::Sub,
                    lhs: column(INT8OID),
                    rhs: rhs64
                },
                &[all_basic[1], all_basic[2], all_basic[3]],
            )
            .is_ok()
        );
        assert!(
            validate_measure_outputs(
                &MeasureExpr::Binary {
                    op: BinaryMeasureOp::Mul,
                    lhs: column(INT8OID),
                    rhs: rhs64
                },
                &all_basic[1..2],
            )
            .is_err()
        );
        assert!(
            validate_measure_outputs(
                &MeasureExpr::Column(column(INT4OID)),
                &[output(AggregateSource::Rhs, AggregateKind::Count)],
            )
            .is_err()
        );
    }

    #[test]
    fn relation_and_dispatch_collection_cover_every_pure_source_shape() {
        let mut collection_spec = spec(MeasureExpr::CountStar, AggregateKind::Count);
        let mut fact_second = column(INT4OID);
        fact_second.attno = 2;
        let dim_key = ColumnRef {
            relation_oid: 43,
            attno: 1,
            type_oid: INT4OID,
        };
        collection_spec.group_keys = vec![
            GroupKeyRef {
                source: GroupKeySource::FactColumn(column(INT4OID)),
                type_oid: INT4OID,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            },
            GroupKeyRef {
                source: GroupKeySource::StarDimension {
                    dim_index: 0,
                    group_column: dim_key,
                },
                type_oid: INT4OID,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            },
            GroupKeyRef {
                source: GroupKeySource::Expression {
                    inputs: vec![fact_second],
                    program: vec![1],
                },
                type_oid: INT4OID,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            },
            GroupKeyRef {
                source: GroupKeySource::H3LatLngToCell {
                    latitude: ColumnRef {
                        relation_oid: 42,
                        attno: 3,
                        type_oid: FLOAT8OID,
                    },
                    longitude: ColumnRef {
                        relation_oid: 42,
                        attno: 4,
                        type_oid: FLOAT8OID,
                    },
                    resolution: 7,
                },
                type_oid: 90_001,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            },
        ];
        collection_spec.measures = vec![
            MeasureSpec {
                expression: MeasureExpr::StatsPair {
                    value: fact_second,
                    rhs: column(INT4OID),
                },
                outputs: vec![output(AggregateSource::Value, AggregateKind::Count)],
                filter: FilterSpec::Mask {
                    input: column(BOOLOID),
                    kind: MaskKind::Sql,
                },
            },
            MeasureSpec {
                expression: MeasureExpr::Bytecode {
                    inputs: vec![ColumnRef {
                        relation_oid: 42,
                        attno: 5,
                        type_oid: INT4OID,
                    }],
                    program: vec![1],
                    result_type_oid: INT4OID,
                },
                outputs: vec![output(AggregateSource::Value, AggregateKind::Count)],
                filter: FilterSpec::None,
            },
        ];
        collection_spec.fact_filter = FilterSpec::Bytecode {
            inputs: vec![ColumnRef {
                relation_oid: 42,
                attno: 6,
                type_oid: BOOLOID,
            }],
            program: vec![1],
        };
        collection_spec.star_dims.push(DimSpec {
            relation_oid: 43,
            fact_key: fact_second,
            dim_key,
            collation_oid: 0,
            multiplicity: JoinMultiplicity::Unique,
            filter: FilterSpec::Mask {
                input: ColumnRef {
                    relation_oid: 43,
                    attno: 2,
                    type_oid: BOOLOID,
                },
                kind: MaskKind::Sql,
            },
        });
        let selected = selected_relations(&collection_spec).expect("all column sources collect");
        assert_eq!(
            selected
                .iter()
                .map(|relation| u32::from(relation.relid))
                .collect::<Vec<_>>(),
            vec![42, 43]
        );
        assert!(selected[0].columns.len() >= 6);
        assert_eq!(
            dependency_relids(&collection_spec),
            vec![pg_sys::Oid::from(42_u32), pg_sys::Oid::from(43_u32)]
        );

        let mut dispatch_spec = spec(MeasureExpr::Column(column(INT4OID)), AggregateKind::Count);
        dispatch_spec.fact_filter = FilterSpec::Ranges {
            input: fact_second,
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(0),
                hi: ScalarValue::I32(1),
            }],
        };
        let refs = dispatch_column_refs(&dispatch_spec).expect("measure and range inputs collect");
        assert_eq!(refs.len(), 2);
        let mut invalid = column(INT4OID);
        invalid.attno = i32::MAX;
        dispatch_spec.measures[0].expression = MeasureExpr::Column(invalid);
        assert!(dispatch_column_refs(&dispatch_spec).is_err());
    }

    #[test]
    fn zero_row_descriptor_builder_covers_all_host_only_shapes() {
        let global = spec(MeasureExpr::CountStar, AggregateKind::Count);
        let desc = zero_descriptor(&global, &[], &[]);
        assert_eq!(desc.row_count, 0);
        assert_eq!(desc.measure_count, 1);
        assert_eq!(
            desc.measures[0].op,
            abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR
        );

        let mut scalar = spec(MeasureExpr::Column(column(INT4OID)), AggregateKind::Sum);
        scalar.measures[0].outputs = vec![
            output(AggregateSource::Value, AggregateKind::Sum),
            output(AggregateSource::Value, AggregateKind::Count),
            output(AggregateSource::Value, AggregateKind::Min),
            output(AggregateSource::Value, AggregateKind::Max),
        ];
        let desc = zero_descriptor(&scalar, &[], &[INT4OID]);
        assert_eq!(desc.measures[0].op, abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN);
        assert_eq!(
            desc.measures[0].agg_mask,
            abi::PGACCEL_GROUPED_AGG_LANE_SUM
                | abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                | abi::PGACCEL_GROUPED_AGG_LANE_MIN
                | abi::PGACCEL_GROUPED_AGG_LANE_MAX
        );

        let mut rhs = column(INT4OID);
        rhs.attno = 2;
        let binary = spec(
            MeasureExpr::Binary {
                op: BinaryMeasureOp::Sub,
                lhs: column(INT4OID),
                rhs,
            },
            AggregateKind::Count,
        );
        let desc = zero_descriptor(&binary, &[], &[INT4OID, INT4OID]);
        assert_eq!(desc.measures[0].op, abi::PGACCEL_GROUPED_AGG_MEASURE_SUB);

        let mut ranged = spec(MeasureExpr::Column(column(INT4OID)), AggregateKind::Count);
        ranged.fact_filter = FilterSpec::Ranges {
            input: column(INT4OID),
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(-2),
                hi: ScalarValue::I32(9),
            }],
        };
        let desc = zero_descriptor(&ranged, &[], &[INT4OID]);
        assert_eq!(desc.where_filter.predicate_range_count, 1);
        assert_eq!(desc.where_filter.predicate_measure_slot, 0);

        let mut masked = global.clone();
        masked.fact_filter = FilterSpec::Mask {
            input: column(BOOLOID),
            kind: MaskKind::Sql,
        };
        let desc = zero_descriptor(&masked, &[BOOLOID], &[]);
        assert_eq!(desc.where_filter.kind, abi::PGACCEL_GROUPED_AGG_FILTER_NONE);

        let mut grouped = global.clone();
        grouped.group_keys.push(GroupKeyRef {
            source: GroupKeySource::FactColumn(column(INT4OID)),
            type_oid: INT4OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        });
        let desc = zero_descriptor(&grouped, &[INT4OID], &[]);
        assert_eq!(desc.key_count, 1);
        assert_eq!(
            desc.keys[0].source,
            abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT
        );
        assert_eq!(desc.keys[0].cardinality, 1);

        let fact_key = column(INT4OID);
        let dim_key = ColumnRef {
            relation_oid: 43,
            attno: 1,
            type_oid: INT4OID,
        };
        let mut dimension = global;
        dimension.star_dims.push(DimSpec {
            relation_oid: 43,
            fact_key,
            dim_key,
            collation_oid: 0,
            multiplicity: JoinMultiplicity::Unique,
            filter: FilterSpec::None,
        });
        dimension.group_keys.push(GroupKeyRef {
            source: GroupKeySource::StarDimension {
                dim_index: 0,
                group_column: dim_key,
            },
            type_oid: INT4OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        });
        let desc = zero_descriptor(&dimension, &[INT4OID, INT4OID], &[]);
        assert_eq!(desc.dim_count, 1);
        assert_eq!(
            desc.keys[0].source,
            abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0
        );
        assert_eq!(desc.dims[0].key_count, 0);
    }

    #[test]
    fn empty_h3_descriptor_and_view_lookup_fail_closed_without_device_work() {
        let (spec, _) = h3_count_shape();
        let GroupKeySource::H3CellToParent { cell, .. } = spec.group_keys[0].source else {
            unreachable!()
        };
        let artifact = H3ParentArtifact::new(spec, 0, 1, None).expect("empty H3 artifact");
        let request = ResidentColumnRef {
            relid: pg_sys::Oid::from(cell.relation_oid),
            attno: 1,
        };
        let view = ResidentColumnView::Empty {
            type_oid: pg_sys::Oid::from(cell.type_oid),
        };
        let desc = build_h3_descriptor(&artifact, &[request], &[view], cell)
            .expect("empty H3 descriptor binds");
        assert_eq!(desc.grouping_mode, abi::PGACCEL_GROUPED_AGG_GROUPING_HASH);
        assert_eq!(desc.output_mode, abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        assert_eq!(desc.group_capacity, 1);
        assert!(desc.keys[0].values.values.is_null());

        assert!(find_view(&[], &[], cell).is_err());
        assert!(find_view(&[request], &[], cell).is_err());
        let wrong = ResidentColumnView::Empty {
            type_oid: pg_sys::Oid::from(INT4OID),
        };
        assert!(find_view(&[request], &[wrong], cell).is_err());
        let mut invalid = cell;
        invalid.attno = i32::MAX;
        let view = ResidentColumnView::Empty {
            type_oid: pg_sys::Oid::from(cell.type_oid),
        };
        assert!(find_view(&[request], &[view], invalid).is_err());
    }
}
