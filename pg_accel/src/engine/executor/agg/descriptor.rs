//! Runtime capability checks and frozen-ABI binding for neutral aggregates.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::fmt;
use std::time::Instant;

use pgrx::pg_sys;

use super::artifact::{
    ArtifactKeyInput, DescriptorAggArtifact, H3ParentArtifact, artifact_column_refs,
    prepare_agg_artifact,
};
use super::output::{DescriptorAggOutput, validate_h3_compact_key_buffers};
use crate::engine::residency::{
    ArtifactEnsureOutcome, DerivedArtifactIdentity, ResidentByteAccounting, ResidentColumnRef,
    ResidentColumnView, ResidentLoadError, ResidentRelationEvidence, ResolvedDerivedInputs,
    SelectedRelation, ensure_derived_artifact, ensure_device_derived_artifact,
    ensure_selected_relations, with_derived_artifact_inputs,
};
use crate::engine::spec::abi;
use crate::engine::spec::{
    AggOutputProjection, AggQuerySpec, AggregateKind, AggregateSource, BinaryMeasureOp, ColumnRef,
    FilterSpec, GroupKeyEncoding, GroupKeySource, JoinMultiplicity, MaskKind, MeasureExpr,
    ScalarValue,
};
use crate::gpu::{
    GpuError, GpuErrorDomain, GpuOperation, GpuStatusDetail, GroupedAggOutcome,
    GroupedAggOutputStorage, PgaccelExprUsmCol, PgaccelVal, PgaccelValTag, ResolvedGroupedAggPlan,
    execute_grouped_agg_one_shot,
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

fn validate_filter(filter: &FilterSpec, relation_oid: u32, dimension: bool) -> Result<(), String> {
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

pub(super) fn validate_runtime_capability(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
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
        validate_filter(&dimension.filter, dimension.relation_oid, true)?;
    }
    validate_filter(&spec.fact_filter, spec.fact_rel, false)?;
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
#[derive(Debug, Clone, Copy)]
enum DescriptorArtifactKind {
    Dense,
    H3Parent {
        cell: ColumnRef,
        resolution: i32,
        max_chunk_rows: usize,
    },
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
        validate_runtime_capability(&spec, &projection)?;
        validate_catalog_contract(&spec, &projection)?;
        let mut identity_words = spec
            .encode_i32()
            .map_err(|error| format!("could not encode aggregate query spec: {error}"))?;
        identity_words.extend(
            projection
                .encode_i32(&spec)
                .map_err(|error| format!("could not encode aggregate projection: {error}"))?,
        );
        if let Some((cell, _, result_type_oid)) = h3_parent_group(&spec) {
            // SAFETY: executor Begin runs on PostgreSQL's main backend thread.
            let catalog = unsafe { crate::engine::ffi::syscache::resolve_h3_catalog() }?;
            if u32::from(catalog.type_oid) != cell.type_oid || cell.type_oid != result_type_oid {
                return Err("H3 catalog identity changed after validation".to_owned());
            }
            identity_words.extend(catalog.fingerprint_words);
        }
        let selected = selected_relations(&spec)?;
        let dependencies = dependency_relids(&spec);
        let artifact_columns = artifact_column_refs(&spec)?;
        let dispatch_columns = dispatch_column_refs(&spec)?;
        let limits = crate::engine::cost::device_limits();
        let artifact_kind = h3_parent_group(&spec).map_or(
            DescriptorArtifactKind::Dense,
            |(cell, resolution, _)| DescriptorArtifactKind::H3Parent {
                cell,
                resolution,
                max_chunk_rows: limits.gpu_h3_max_chunk_rows,
            },
        );
        Ok(Self {
            spec,
            projection,
            identity: DerivedArtifactIdentity::from_canonical_words(identity_words),
            selected,
            dependencies,
            artifact_columns,
            dispatch_columns,
            max_groups: limits.gpu_hash_agg_max_groups,
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
        let selected = ensure_selected_relations(&self.selected)?;
        let owner_relid = pg_sys::Oid::from(self.spec.fact_rel);
        let (artifact_outcome, relations, artifact_bytes) = match self.artifact_kind {
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
                let group_capacity = h3_group_capacity(fact_rows, resolution, self.max_groups)?;
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
                        let view = find_view(&self.artifact_columns, &bundle.columns, cell)?;
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
                            if max_chunk_rows == 0 {
                                return Err(ResidentLoadError::Loader(
                                    "H3 device chunk limit is zero".to_owned(),
                                ));
                            }
                            let output = parents.as_ref().ok_or_else(|| {
                                "nonempty H3 parent artifact has no output".to_owned()
                            })?;
                            for offset in (0..fact_rows).step_by(max_chunk_rows) {
                                let count = (fact_rows - offset).min(max_chunk_rows);
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
                                        resolution,
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
        match self.artifact_kind {
            DescriptorArtifactKind::Dense => {
                with_derived_artifact_inputs::<DescriptorAggArtifact, _>(
                    pg_sys::Oid::from(self.spec.fact_rel),
                    &self.identity,
                    &self.dispatch_columns,
                    |inputs| build_and_execute(inputs, &self.dispatch_columns, &self.projection),
                )
            }
            DescriptorArtifactKind::H3Parent { cell, .. } => {
                with_derived_artifact_inputs::<H3ParentArtifact, _>(
                    pg_sys::Oid::from(self.spec.fact_rel),
                    &self.identity,
                    &self.dispatch_columns,
                    |inputs| {
                        build_and_execute_h3(inputs, &self.dispatch_columns, cell, &self.projection)
                    },
                )
            }
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

fn build_filter(
    spec: &AggQuerySpec,
    artifact: &DescriptorAggArtifact,
) -> Result<abi::PgaccelGroupedAggFilter, String> {
    match &spec.fact_filter {
        FilterSpec::None => Ok(disabled_filter()),
        FilterSpec::Mask {
            kind: MaskKind::Sql,
            ..
        } if artifact.fact_rows == 0 => Ok(disabled_filter()),
        FilterSpec::Mask {
            kind: MaskKind::Sql,
            ..
        } => {
            let mut filter = disabled_filter();
            filter.kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
            filter.mask = artifact
                .fact_mask
                .as_ref()
                .map_or(std::ptr::null(), |mask| mask.as_ptr());
            if filter.mask.is_null() {
                return Err("nonempty SQL fact mask has no derived device buffer".into());
            }
            Ok(filter)
        }
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

fn build_descriptor(
    artifact: &DescriptorAggArtifact,
    requests: &[ResidentColumnRef],
    views: &[ResidentColumnView<'_>],
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
                unsafe { std::mem::zeroed() },
                abi::PGACCEL_GROUPED_AGG_ACCUM_I64,
            ),
            MeasureExpr::Column(column) => {
                let view = find_view(requests, views, *column)?;
                (
                    abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                    measure_column(view, artifact.fact_rows)?,
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
    desc.where_filter = build_filter(spec, artifact)?;
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

fn build_and_execute(
    inputs: ResolvedDerivedInputs<'_, DescriptorAggArtifact>,
    requests: &[ResidentColumnRef],
    projection: &AggOutputProjection,
) -> Result<DescriptorAggDispatch, DescriptorAggExecutionError> {
    let desc = build_descriptor(inputs.artifact, requests, &inputs.columns)
        .map_err(DescriptorAggExecutionError::Failure)?;
    // SAFETY: every active pointer is owned by the artifact/raw resident views
    // held by this residency callback and the kernel call below is synchronous.
    let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }
        .map_err(|error| gpu_execution_error("generic aggregate descriptor rejected", error))?;
    let mut storage = GroupedAggOutputStorage::new(&plan).map_err(|error| {
        gpu_execution_error("generic aggregate output allocation failed", error)
    })?;
    let dispatch_started = Instant::now();
    let outcome = execute_grouped_agg_one_shot(&plan, &mut storage)
        .map_err(|error| gpu_execution_error("generic aggregate kernel failed", error))?;
    let dispatch_time_us =
        u64::try_from(dispatch_started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let output = DescriptorAggOutput::new(
        storage,
        outcome,
        inputs.artifact.domains.clone(),
        inputs.artifact.resolved_spec.clone(),
        projection.clone(),
    )
    .map_err(DescriptorAggExecutionError::Failure)?;
    Ok(DescriptorAggDispatch {
        output,
        fact_rows: inputs.artifact.fact_rows,
        dispatch_time_us,
        residency: None,
    })
}

fn build_and_execute_h3(
    inputs: ResolvedDerivedInputs<'_, H3ParentArtifact>,
    requests: &[ResidentColumnRef],
    cell: ColumnRef,
    projection: &AggOutputProjection,
) -> Result<DescriptorAggDispatch, DescriptorAggExecutionError> {
    let desc = build_h3_descriptor(inputs.artifact, requests, &inputs.columns, cell)
        .map_err(DescriptorAggExecutionError::Failure)?;
    // SAFETY: every active pointer remains pinned by the residency callback
    // through the synchronous grouped-aggregate lifecycle below.
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
    use crate::engine::spec::{
        AggOutputSlot, AggOutputSource, AggregateOutput, GroupKeyRef, MeasureSpec,
    };

    fn column(type_oid: u32) -> ColumnRef {
        ColumnRef {
            relation_oid: 42,
            attno: 1,
            type_oid,
        }
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
}
