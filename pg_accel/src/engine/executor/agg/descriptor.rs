//! Runtime capability checks and frozen-ABI binding for neutral aggregates.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;

use pgrx::pg_sys;

use super::artifact::{
    ArtifactKeyInput, DescriptorAggArtifact, artifact_column_refs, prepare_agg_artifact,
};
use super::output::DescriptorAggOutput;
use crate::engine::residency::{
    ArtifactEnsureOutcome, DerivedArtifactIdentity, ResidentColumnRef, ResidentColumnView,
    ResidentLoadError, ResolvedDerivedInputs, SelectedRelation, ensure_derived_artifact,
    ensure_selected_relations, with_derived_artifact_inputs,
};
use crate::engine::spec::abi;
use crate::engine::spec::{
    AggOutputProjection, AggQuerySpec, AggregateKind, AggregateSource, BinaryMeasureOp, ColumnRef,
    FilterSpec, GroupKeyEncoding, GroupKeySource, JoinMultiplicity, MaskKind, MeasureExpr,
    ScalarValue,
};
use crate::gpu::{
    GroupedAggOutcome, GroupedAggOutputStorage, PgaccelExprUsmCol, PgaccelVal, PgaccelValTag,
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
                        BOOLOID | INT4OID | INT8OID | FLOAT4OID | FLOAT8OID
                    )
                } else {
                    matches!(input.type_oid, INT4OID | INT8OID | FLOAT8OID)
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

fn validate_runtime_capability(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<(), String> {
    spec.validate()
        .map_err(|error| format!("invalid aggregate query spec: {error}"))?;
    projection
        .validate(spec)
        .map_err(|error| format!("invalid aggregate output projection: {error}"))?;
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

fn validate_catalog_collations(spec: &AggQuerySpec) -> Result<(), String> {
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
            insert_column(relations, *left)?;
            insert_column(relations, *right)
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
            GroupKeySource::H3Cell { input, .. } => insert_column(&mut relations, *input)?,
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
pub(super) struct DescriptorAggPlan {
    spec: AggQuerySpec,
    projection: AggOutputProjection,
    identity: DerivedArtifactIdentity,
    selected: Vec<SelectedRelation>,
    dependencies: Vec<pg_sys::Oid>,
    artifact_columns: Vec<ResidentColumnRef>,
    dispatch_columns: Vec<ResidentColumnRef>,
    max_groups: usize,
}

impl DescriptorAggPlan {
    pub(super) fn new(spec: AggQuerySpec, projection: AggOutputProjection) -> Result<Self, String> {
        validate_runtime_capability(&spec, &projection)?;
        validate_catalog_collations(&spec)?;
        let mut identity_words = spec
            .encode_i32()
            .map_err(|error| format!("could not encode aggregate query spec: {error}"))?;
        identity_words.extend(
            projection
                .encode_i32(&spec)
                .map_err(|error| format!("could not encode aggregate projection: {error}"))?,
        );
        let selected = selected_relations(&spec)?;
        let dependencies = dependency_relids(&spec);
        let artifact_columns = artifact_column_refs(&spec)?;
        let dispatch_columns = dispatch_column_refs(&spec)?;
        Ok(Self {
            spec,
            projection,
            identity: DerivedArtifactIdentity::from_canonical_words(identity_words),
            selected,
            dependencies,
            artifact_columns,
            dispatch_columns,
            max_groups: crate::engine::cost::device_limits().gpu_hash_agg_max_groups,
        })
    }

    pub(super) fn spec(&self) -> &AggQuerySpec {
        &self.spec
    }

    pub(super) fn projection(&self) -> &AggOutputProjection {
        &self.projection
    }

    pub(super) fn ensure_artifact(&self) -> Result<ArtifactEnsureOutcome, String> {
        ensure_selected_relations(&self.selected).map_err(|error| error.to_string())?;
        ensure_derived_artifact(
            pg_sys::Oid::from(self.spec.fact_rel),
            &self.identity,
            &self.dependencies,
            &self.artifact_columns,
            |bundle| {
                prepare_agg_artifact(&self.spec, &self.artifact_columns, bundle, self.max_groups)
            },
            DescriptorAggArtifact::build,
        )
        .map_err(|error| error.to_string())
    }

    fn execute_once(&self) -> Result<Result<DescriptorAggDispatch, String>, ResidentLoadError> {
        with_derived_artifact_inputs::<DescriptorAggArtifact, _>(
            pg_sys::Oid::from(self.spec.fact_rel),
            &self.identity,
            &self.dispatch_columns,
            |inputs| build_and_execute(inputs, &self.dispatch_columns, &self.projection),
        )
    }

    pub(super) fn execute(&self) -> Result<DescriptorAggDispatch, String> {
        self.ensure_artifact()?;
        match self.execute_once() {
            Ok(result) => result,
            Err(
                ResidentLoadError::ArtifactDependencyChanged { .. }
                | ResidentLoadError::ArtifactNotFound { .. },
            ) => {
                self.ensure_artifact()?;
                self.execute_once().map_err(|error| error.to_string())?
            }
            Err(error) => Err(error.to_string()),
        }
    }
}

pub(super) struct DescriptorAggDispatch {
    pub output: DescriptorAggOutput,
    pub fact_rows: usize,
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
            type_oid => return Err(format!("type OID {type_oid} cannot be a measure")),
        },
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
                    if column.type_oid == FLOAT8OID {
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

fn build_and_execute(
    inputs: ResolvedDerivedInputs<'_, DescriptorAggArtifact>,
    requests: &[ResidentColumnRef],
    projection: &AggOutputProjection,
) -> Result<DescriptorAggDispatch, String> {
    let desc = build_descriptor(inputs.artifact, requests, &inputs.columns)?;
    // SAFETY: every active pointer is owned by the artifact/raw resident views
    // held by this residency callback and the kernel call below is synchronous.
    let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }
        .map_err(|error| format!("generic aggregate descriptor rejected: {error}"))?;
    let mut storage = GroupedAggOutputStorage::new(&plan)
        .map_err(|error| format!("generic aggregate output allocation failed: {error}"))?;
    let outcome = execute_grouped_agg_one_shot(&plan, &mut storage)
        .map_err(|error| format!("generic aggregate kernel failed: {error}"))?;
    let output = DescriptorAggOutput::new(
        storage,
        outcome,
        inputs.artifact.domains.clone(),
        inputs.artifact.resolved_spec.clone(),
        projection.clone(),
    )?;
    Ok(DescriptorAggDispatch {
        output,
        fact_rows: inputs.artifact.fact_rows,
    })
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
}
