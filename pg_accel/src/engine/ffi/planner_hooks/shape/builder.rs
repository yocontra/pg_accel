//! Pure relation-graph and aggregate-spec builder.

use std::collections::{BTreeMap, BTreeSet};

use crate::engine::cost::{PgCost, TypedCostModel};
use crate::engine::spec::abi::{
    PGACCEL_GROUPED_AGG_MAX_DIMS, PGACCEL_GROUPED_AGG_MAX_KEYS, PGACCEL_GROUPED_AGG_MAX_MEASURES,
};
use crate::engine::spec::{
    AggOutputProjection, AggQuerySpec, AggregateKind, AggregateSource, DimSpec, FilterSpec,
    GroupKeyEncoding, GroupKeyRef, GroupKeySource, JoinMultiplicity, MaskKind, MeasureExpr,
    MeasureSpec,
};

use super::cost::{estimate_shape_cost, spatial_point_polygon_vertices};
use super::{
    DerivedGroupKeyRequirement, DescriptorFilterBinding, DescriptorGroupingMode,
    DescriptorMeasurePlan, DescriptorResolution, DictionaryJoinRequirement,
    DictionaryKeyRequirement, InputProjection, PlannerColumn, PlannerGroupKey, ProjectionSlot,
    ProjectionSource, RelationResidency, RelationResidencyRequirement, RequiredRelation,
    ResidencyEstimate, ShapeDecline, ShapeInput, ShapeModifiers, ShapePlan,
};

const MAX_RELATIONS: usize = PGACCEL_GROUPED_AGG_MAX_DIMS + 1;

pub(super) fn reject_modifiers(modifiers: ShapeModifiers) -> Result<(), ShapeDecline> {
    if modifiers.has_window_functions {
        Err(ShapeDecline::WindowFunctions)
    } else if modifiers.has_target_srfs {
        Err(ShapeDecline::TargetSetReturningFunction)
    } else if modifiers.has_sublinks {
        Err(ShapeDecline::Sublink)
    } else if modifiers.has_recursive_query {
        Err(ShapeDecline::RecursiveQuery)
    } else if modifiers.has_modifying_cte {
        Err(ShapeDecline::ModifyingCte)
    } else if modifiers.has_row_security {
        Err(ShapeDecline::RowSecurity)
    } else if modifiers.has_distinct {
        Err(ShapeDecline::Distinct)
    } else if modifiers.has_grouping_sets {
        Err(ShapeDecline::GroupingSets)
    } else if modifiers.has_group_distinct {
        Err(ShapeDecline::GroupDistinct)
    } else if modifiers.has_having {
        Err(ShapeDecline::Having)
    } else if modifiers.has_set_operations {
        Err(ShapeDecline::SetOperations)
    } else if modifiers.has_row_marks {
        Err(ShapeDecline::RowMarks)
    } else {
        Ok(())
    }
}

fn validate_planner_column(input: &ShapeInput, column: PlannerColumn) -> Result<(), ShapeDecline> {
    let relation = relation_by_varno(input, column.varno)?;
    if relation.relation_oid != column.column.relation_oid
        || column.column.attno <= 0
        || column.column.type_oid == 0
    {
        return Err(ShapeDecline::UnsupportedColumn {
            relation_oid: column.column.relation_oid,
            attno: column.column.attno,
        });
    }
    Ok(())
}

fn validate_key_metadata(
    type_oid: u32,
    collation_oid: u32,
    collation_is_deterministic: bool,
) -> Result<(), ShapeDecline> {
    const TEXTOID: u32 = 25;
    const BPCHAROID: u32 = 1042;
    const VARCHAROID: u32 = 1043;
    if matches!(type_oid, TEXTOID | BPCHAROID | VARCHAROID)
        && (collation_oid == 0 || !collation_is_deterministic)
    {
        return Err(ShapeDecline::NondeterministicKeyCollation { collation_oid });
    }
    if !matches!(type_oid, TEXTOID | BPCHAROID | VARCHAROID) && collation_oid != 0 {
        return Err(ShapeDecline::InvalidKeyCollation {
            type_oid,
            collation_oid,
        });
    }
    Ok(())
}

fn validate_key_collation(column: PlannerColumn) -> Result<(), ShapeDecline> {
    validate_key_metadata(
        column.column.type_oid,
        column.collation_oid,
        column.collation_is_deterministic,
    )
}

fn validate_group_key_type(type_oid: u32) -> Result<(), ShapeDecline> {
    if matches!(
        type_oid,
        16 | 20 | 21 | 23 | 25 | 700 | 701 | 1042 | 1043 | 1082 | 1114 | 1184
    ) {
        Ok(())
    } else {
        Err(ShapeDecline::UnsupportedGroupKeyType { type_oid })
    }
}

fn exact_bounded_int4_filter(filter: &FilterSpec, value: &crate::engine::spec::ColumnRef) -> bool {
    let FilterSpec::Ranges { input, ranges } = filter else {
        return false;
    };
    let [range] = ranges.as_slice() else {
        return false;
    };
    let (crate::engine::spec::ScalarValue::I32(lo), crate::engine::spec::ScalarValue::I32(hi)) =
        (range.lo, range.hi)
    else {
        return false;
    };
    input == value && value.type_oid == 23 && lo != i32::MIN && hi != i32::MAX && lo < hi
}

fn validate_measure_descriptor_capability(
    aggregate: &super::AggregateExpr,
) -> Result<(), ShapeDecline> {
    const BOOLOID: u32 = 16;
    const INT2OID: u32 = 21;
    const INT4OID: u32 = 23;
    const INT8OID: u32 = 20;
    const FLOAT4OID: u32 = 700;
    const FLOAT8OID: u32 = 701;
    const DATEOID: u32 = 1082;
    const TIMESTAMPOID: u32 = 1114;
    const TIMESTAMPTZOID: u32 = 1184;
    if matches!(aggregate.filter, FilterSpec::Spatial { .. }) {
        return Err(ShapeDecline::SpatialFilterOutsideFactRelation);
    }
    if aggregate.output.source != AggregateSource::Value {
        return Err(ShapeDecline::UnsupportedAggregateModifier);
    }
    if aggregate.filter != FilterSpec::None
        && !matches!(
            &aggregate.expression,
            MeasureExpr::Column(column)
                if aggregate.output.kind == AggregateKind::Sum
                    && exact_bounded_int4_filter(&aggregate.filter, column)
        )
    {
        return Err(ShapeDecline::UnsupportedAggregateModifier);
    }
    let validate_column =
        |column: &crate::engine::spec::ColumnRef| match (column.type_oid, aggregate.output.kind) {
            (BOOLOID | INT2OID, AggregateKind::Count) => Ok(()),
            (
                BOOLOID | FLOAT4OID | DATEOID | TIMESTAMPOID | TIMESTAMPTZOID,
                AggregateKind::Count | AggregateKind::Min | AggregateKind::Max,
            ) => Err(ShapeDecline::UnsupportedAggregateInput {
                kind: aggregate.output.kind,
                type_oid: column.type_oid,
            }),
            (
                INT4OID,
                AggregateKind::Sum | AggregateKind::Count | AggregateKind::Min | AggregateKind::Max,
            )
            | (
                INT8OID | FLOAT8OID,
                AggregateKind::Count | AggregateKind::Min | AggregateKind::Max,
            ) => Ok(()),
            (INT4OID | INT8OID, AggregateKind::Avg) | (INT8OID, AggregateKind::Sum) => {
                Err(ShapeDecline::NumericAccumulatorTypeUnavailable {
                    type_oid: column.type_oid,
                })
            }
            (FLOAT8OID, AggregateKind::Sum | AggregateKind::Avg | AggregateKind::StddevSamp) => {
                Err(ShapeDecline::FloatingAccumulatorSemantics)
            }
            (INT4OID | INT8OID, AggregateKind::StddevSamp) => {
                Err(ShapeDecline::UnsupportedAggregateInput {
                    kind: aggregate.output.kind,
                    type_oid: column.type_oid,
                })
            }
            _ if !matches!(
                column.type_oid,
                BOOLOID
                    | INT2OID
                    | INT4OID
                    | INT8OID
                    | FLOAT4OID
                    | FLOAT8OID
                    | DATEOID
                    | TIMESTAMPOID
                    | TIMESTAMPTZOID
            ) =>
            {
                Err(ShapeDecline::UnsupportedMeasureType {
                    type_oid: column.type_oid,
                })
            }
            _ => Err(ShapeDecline::UnsupportedAggregateInput {
                kind: aggregate.output.kind,
                type_oid: column.type_oid,
            }),
        };
    match &aggregate.expression {
        MeasureExpr::CountStar if aggregate.output.kind == AggregateKind::Count => Ok(()),
        MeasureExpr::CountStar => Err(ShapeDecline::UnsupportedAggregateInput {
            kind: aggregate.output.kind,
            type_oid: 0,
        }),
        MeasureExpr::Column(column) => validate_column(column),
        MeasureExpr::Binary { op, lhs, rhs } => {
            if lhs.type_oid != rhs.type_oid {
                return Err(ShapeDecline::UnsupportedBinaryMeasure);
            }
            if lhs.type_oid == FLOAT8OID {
                return Err(ShapeDecline::FloatingExpressionSemantics);
            }
            if lhs.type_oid == INT8OID && *op == crate::engine::spec::BinaryMeasureOp::Mul {
                return Err(ShapeDecline::IntegerExpressionOverflowSemantics);
            }
            validate_column(lhs)
        }
        MeasureExpr::StatsPair { .. } => Err(ShapeDecline::FloatingAccumulatorSemantics),
        MeasureExpr::Bytecode { .. } => Err(ShapeDecline::UnsupportedMeasureExpression),
    }
}

fn aggregate_source_type(aggregate: &super::AggregateExpr) -> Result<u32, ShapeDecline> {
    let source = match (&aggregate.expression, aggregate.output.source) {
        (MeasureExpr::CountStar, AggregateSource::Value) => 0,
        (
            MeasureExpr::Column(column) | MeasureExpr::Binary { lhs: column, .. },
            AggregateSource::Value,
        ) => column.type_oid,
        (MeasureExpr::StatsPair { value, .. }, AggregateSource::Value) => value.type_oid,
        (MeasureExpr::StatsPair { rhs, .. }, AggregateSource::Rhs) => rhs.type_oid,
        (
            MeasureExpr::Bytecode {
                result_type_oid, ..
            },
            AggregateSource::Value,
        ) => *result_type_oid,
        _ => return Err(ShapeDecline::UnsupportedAggregateModifier),
    };
    Ok(source)
}

fn expected_aggregate_result_type(source_type_oid: u32, kind: AggregateKind) -> Option<u32> {
    const BOOLOID: u32 = 16;
    const INT2OID: u32 = 21;
    const INT4OID: u32 = 23;
    const INT8OID: u32 = 20;
    const FLOAT4OID: u32 = 700;
    const FLOAT8OID: u32 = 701;
    const DATEOID: u32 = 1082;
    const TIMESTAMPOID: u32 = 1114;
    const TIMESTAMPTZOID: u32 = 1184;
    match (source_type_oid, kind) {
        (
            0 | BOOLOID | INT2OID | INT4OID | INT8OID | FLOAT4OID | FLOAT8OID | DATEOID
            | TIMESTAMPOID | TIMESTAMPTZOID,
            AggregateKind::Count,
        )
        | (INT4OID, AggregateKind::Sum) => Some(INT8OID),
        (
            source @ (INT2OID | INT4OID | INT8OID | FLOAT4OID | FLOAT8OID | DATEOID | TIMESTAMPOID
            | TIMESTAMPTZOID),
            AggregateKind::Min | AggregateKind::Max,
        ) => Some(source),
        _ => None,
    }
}

fn relation_by_varno(
    input: &ShapeInput,
    varno: pgrx::pg_sys::Index,
) -> Result<&super::RelationShape, ShapeDecline> {
    input
        .relations
        .iter()
        .find(|relation| relation.varno == varno)
        .ok_or(ShapeDecline::UnsupportedColumn {
            relation_oid: 0,
            attno: i32::try_from(varno).unwrap_or(i32::MAX),
        })
}

fn validate_relations(input: &ShapeInput) -> Result<(), ShapeDecline> {
    if input.relations.is_empty() {
        return Err(ShapeDecline::DisconnectedJoinGraph);
    }
    if input.relations.len() > MAX_RELATIONS {
        return Err(ShapeDecline::TooManyRelations {
            actual: input.relations.len(),
            maximum: MAX_RELATIONS,
        });
    }
    let mut varnos = BTreeSet::new();
    let mut oids = BTreeSet::new();
    for relation in &input.relations {
        if !varnos.insert(relation.varno) {
            return Err(ShapeDecline::DuplicatePlannerRelation {
                varno: relation.varno,
            });
        }
        if !oids.insert(relation.relation_oid) {
            return Err(ShapeDecline::SelfJoinUsesAmbiguousRelationOid {
                relation_oid: relation.relation_oid,
            });
        }
    }
    for (relation_oid, filter) in &input.relation_filters {
        if !oids.contains(relation_oid) {
            return Err(ShapeDecline::UnsupportedColumn {
                relation_oid: *relation_oid,
                attno: 0,
            });
        }
        let referenced_relation = match filter {
            FilterSpec::None => Some(*relation_oid),
            FilterSpec::Ranges { input, .. } | FilterSpec::Mask { input, .. } => {
                Some(input.relation_oid)
            }
            FilterSpec::Bytecode { inputs, .. } => inputs.first().map(|input| input.relation_oid),
            FilterSpec::Spatial { left, right, .. } => {
                let mut columns = [left, right]
                    .into_iter()
                    .filter_map(|operand| operand.column());
                let first = columns.next().map(|column| column.relation_oid);
                if first == Some(*relation_oid)
                    && columns.all(|column| column.relation_oid == *relation_oid)
                {
                    first
                } else {
                    Some(0)
                }
            }
        };
        if referenced_relation != Some(*relation_oid) {
            return Err(ShapeDecline::UnsupportedColumn {
                relation_oid: referenced_relation.unwrap_or(0),
                attno: 0,
            });
        }
    }
    Ok(())
}

fn choose_fact_varno(input: &ShapeInput) -> Result<pgrx::pg_sys::Index, ShapeDecline> {
    if input.relations.len() == 1 {
        if input.joins.is_empty() {
            return Ok(input.relations[0].varno);
        }
        return Err(ShapeDecline::NonStarJoinGraph);
    }
    let mut relation_pairs = BTreeSet::new();
    for join in &input.joins {
        let pair = if join.left.varno <= join.right.varno {
            (join.left.varno, join.right.varno)
        } else {
            (join.right.varno, join.left.varno)
        };
        if !relation_pairs.insert(pair) {
            return Err(ShapeDecline::CompositeJoinKeyUnsupported);
        }
    }
    if input.joins.len() != input.relations.len() - 1 {
        return Err(ShapeDecline::DisconnectedJoinGraph);
    }

    let mut degree = BTreeMap::<pgrx::pg_sys::Index, usize>::new();
    for join in &input.joins {
        validate_planner_column(input, join.left)?;
        validate_planner_column(input, join.right)?;
        validate_key_collation(join.left)?;
        validate_key_collation(join.right)?;
        if join.left.column.type_oid != join.right.column.type_oid {
            return Err(ShapeDecline::JoinKeyTypeMismatch {
                left_type_oid: join.left.column.type_oid,
                right_type_oid: join.right.column.type_oid,
            });
        }
        if join.left.collation_oid != join.right.collation_oid {
            return Err(ShapeDecline::JoinKeyCollationMismatch {
                left_collation_oid: join.left.collation_oid,
                right_collation_oid: join.right.collation_oid,
            });
        }
        if !matches!(join.left.column.type_oid, 20 | 23 | 25 | 1042 | 1043) {
            return Err(ShapeDecline::UnsupportedJoinKeyType {
                type_oid: join.left.column.type_oid,
            });
        }
        relation_by_varno(input, join.left.varno)?;
        relation_by_varno(input, join.right.varno)?;
        if join.left.varno == join.right.varno {
            return Err(ShapeDecline::NonStarJoinGraph);
        }
        *degree.entry(join.left.varno).or_default() += 1;
        *degree.entry(join.right.varno).or_default() += 1;
    }

    if input.relations.len() > 2 {
        let mut centers = input
            .relations
            .iter()
            .filter(|relation| {
                degree.get(&relation.varno).copied().unwrap_or(0) == input.relations.len() - 1
            })
            .map(|relation| relation.varno);
        let fact = centers.next().ok_or(ShapeDecline::NonStarJoinGraph)?;
        if centers.next().is_some() {
            return Err(ShapeDecline::AmbiguousFactRelation);
        }
        return Ok(fact);
    }

    let mut measure_relations = BTreeSet::new();
    for aggregate in &input.aggregates {
        match &aggregate.expression {
            MeasureExpr::CountStar => {}
            MeasureExpr::Column(column) => {
                measure_relations.insert(column.relation_oid);
            }
            MeasureExpr::Binary { lhs, rhs, .. } | MeasureExpr::StatsPair { value: lhs, rhs } => {
                measure_relations.insert(lhs.relation_oid);
                measure_relations.insert(rhs.relation_oid);
            }
            MeasureExpr::Bytecode { inputs, .. } => {
                measure_relations.extend(inputs.iter().map(|column| column.relation_oid));
            }
        }
    }
    if measure_relations.len() == 1 {
        let measure_relation = *measure_relations
            .first()
            .ok_or(ShapeDecline::AmbiguousFactRelation)?;
        return input
            .relations
            .iter()
            .find(|relation| relation.relation_oid == measure_relation)
            .map(|relation| relation.varno)
            .ok_or(ShapeDecline::AmbiguousFactRelation);
    }

    let join = input.joins[0];
    let left = relation_by_varno(input, join.left.varno)?;
    let right = relation_by_varno(input, join.right.varno)?;
    let left_unique = left.unique_attnos.contains(&join.left.column.attno);
    let right_unique = right.unique_attnos.contains(&join.right.column.attno);
    match (left_unique, right_unique) {
        (false, true) => Ok(left.varno),
        (true, false) => Ok(right.varno),
        _ if left.estimated_rows > right.estimated_rows => Ok(left.varno),
        _ if right.estimated_rows > left.estimated_rows => Ok(right.varno),
        _ => Err(ShapeDecline::AmbiguousFactRelation),
    }
}

#[derive(Debug, Clone, Copy)]
struct OrientedDimension {
    relation_oid: u32,
    fact_key: PlannerColumn,
    dim_key: PlannerColumn,
    collation_oid: u32,
    multiplicity: JoinMultiplicity,
}

fn orient_dimensions(
    input: &ShapeInput,
    fact_varno: pgrx::pg_sys::Index,
) -> Result<Vec<OrientedDimension>, ShapeDecline> {
    let mut dimensions = Vec::with_capacity(input.joins.len());
    for join in &input.joins {
        let (fact_key, dim_key) = if join.left.varno == fact_varno {
            (join.left, join.right)
        } else if join.right.varno == fact_varno {
            (join.right, join.left)
        } else {
            return Err(ShapeDecline::NonStarJoinGraph);
        };
        let dimension = relation_by_varno(input, dim_key.varno)?;
        // Collatable uniqueness depends on the index collation as well as the
        // attribute number. RelationShape deliberately carries only the
        // stricter scalar proof today, so text dimensions remain counted.
        let multiplicity = if !matches!(dim_key.column.type_oid, 25 | 1042 | 1043)
            && dimension.unique_attnos.contains(&dim_key.column.attno)
        {
            JoinMultiplicity::Unique
        } else {
            JoinMultiplicity::Counted
        };
        dimensions.push(OrientedDimension {
            relation_oid: dimension.relation_oid,
            fact_key,
            dim_key,
            collation_oid: fact_key.collation_oid,
            multiplicity,
        });
    }
    dimensions.sort_by_key(|dimension| dimension.relation_oid);
    if dimensions
        .windows(2)
        .any(|pair| pair[0].relation_oid == pair[1].relation_oid)
    {
        return Err(ShapeDecline::NonStarJoinGraph);
    }
    Ok(dimensions)
}

fn relation_filter(input: &ShapeInput, relation_oid: u32) -> Result<FilterSpec, ShapeDecline> {
    let mut filters = input
        .relation_filters
        .iter()
        .filter(|(filter_rel, _)| *filter_rel == relation_oid)
        .map(|(_, filter)| filter.clone());
    let filter = filters.next().unwrap_or(FilterSpec::None);
    if filters.next().is_some() {
        return Err(ShapeDecline::MultipleFiltersPerRelation { relation_oid });
    }
    Ok(filter)
}

fn validate_dimension_filter(filter: &FilterSpec) -> Result<(), ShapeDecline> {
    match filter {
        FilterSpec::None => Ok(()),
        FilterSpec::Ranges { input, .. }
            if matches!(
                input.type_oid,
                16 | 20 | 23 | 700 | 701 | 1082 | 1114 | 1184
            ) =>
        {
            Ok(())
        }
        FilterSpec::Mask {
            input,
            kind: MaskKind::Sql,
        } if input.type_oid == 16 => Ok(()),
        FilterSpec::Ranges { input, .. } | FilterSpec::Mask { input, .. } => {
            Err(ShapeDecline::UnsupportedFilterType {
                type_oid: input.type_oid,
            })
        }
        FilterSpec::Spatial { .. } => Err(ShapeDecline::SpatialFilterOutsideFactRelation),
        FilterSpec::Bytecode { .. } => Err(ShapeDecline::UnsupportedPredicate),
    }
}

fn collect_required_column(
    required: &mut BTreeMap<u32, BTreeSet<i32>>,
    column: crate::engine::spec::ColumnRef,
) {
    required
        .entry(column.relation_oid)
        .or_default()
        .insert(column.attno);
}

fn collect_filter_columns(required: &mut BTreeMap<u32, BTreeSet<i32>>, filter: &FilterSpec) {
    match filter {
        FilterSpec::None => {}
        FilterSpec::Ranges { input, .. } | FilterSpec::Mask { input, .. } => {
            collect_required_column(required, *input);
        }
        FilterSpec::Bytecode { inputs, .. } => {
            for input in inputs {
                collect_required_column(required, *input);
            }
        }
        FilterSpec::Spatial { left, right, .. } => {
            for column in [left, right]
                .into_iter()
                .filter_map(|operand| operand.column())
            {
                collect_required_column(required, column);
            }
        }
    }
}

fn collect_group_key_columns(required: &mut BTreeMap<u32, BTreeSet<i32>>, source: &GroupKeySource) {
    match source {
        GroupKeySource::FactColumn(column) => collect_required_column(required, *column),
        GroupKeySource::StarDimension { group_column, .. } => {
            collect_required_column(required, *group_column);
        }
        GroupKeySource::Expression { inputs, .. } => {
            for input in inputs {
                collect_required_column(required, *input);
            }
        }
        GroupKeySource::H3CellToParent { cell, .. } => {
            collect_required_column(required, *cell);
        }
        GroupKeySource::H3LatLngToCell {
            latitude,
            longitude,
            ..
        } => {
            collect_required_column(required, *latitude);
            collect_required_column(required, *longitude);
        }
    }
}

fn collect_measure_columns(required: &mut BTreeMap<u32, BTreeSet<i32>>, expression: &MeasureExpr) {
    match expression {
        MeasureExpr::CountStar => {}
        MeasureExpr::Column(column) => collect_required_column(required, *column),
        MeasureExpr::Binary { lhs, rhs, .. } | MeasureExpr::StatsPair { value: lhs, rhs } => {
            collect_required_column(required, *lhs);
            collect_required_column(required, *rhs);
        }
        MeasureExpr::Bytecode { inputs, .. } => {
            for input in inputs {
                collect_required_column(required, *input);
            }
        }
    }
}

fn fact_filter_binding(
    measures: &mut Vec<MeasureSpec>,
    fact_filter: &FilterSpec,
) -> Result<DescriptorMeasurePlan, ShapeDecline> {
    let projected_measure_count =
        u32::try_from(measures.len()).map_err(|_| ShapeDecline::TooManyMeasures {
            actual: measures.len(),
            maximum: PGACCEL_GROUPED_AGG_MAX_MEASURES,
        })?;
    let input = match fact_filter {
        FilterSpec::None => {
            return Ok(DescriptorMeasurePlan {
                projected_measure_count,
                descriptor_measure_count: projected_measure_count,
                fact_filter: None,
                derived_fact_mask: None,
                derived_spatial_mask: false,
            });
        }
        FilterSpec::Mask {
            input,
            kind: MaskKind::Sql,
        } if input.type_oid == 16 => {
            return Ok(DescriptorMeasurePlan {
                projected_measure_count,
                descriptor_measure_count: projected_measure_count,
                fact_filter: None,
                derived_fact_mask: Some(*input),
                derived_spatial_mask: false,
            });
        }
        FilterSpec::Ranges { input, .. }
            if matches!(input.type_oid, 20 | 23 | 700 | 701 | 1082 | 1114 | 1184) =>
        {
            input
        }
        FilterSpec::Ranges { input, .. } | FilterSpec::Mask { input, .. } => {
            return Err(ShapeDecline::UnsupportedFilterType {
                type_oid: input.type_oid,
            });
        }
        FilterSpec::Spatial { .. } => {
            return Ok(DescriptorMeasurePlan {
                projected_measure_count,
                descriptor_measure_count: projected_measure_count,
                fact_filter: None,
                derived_fact_mask: None,
                derived_spatial_mask: true,
            });
        }
        FilterSpec::Bytecode { .. } => {
            return Err(ShapeDecline::UnsupportedPredicate);
        }
    };
    let existing = measures.iter().enumerate().find_map(|(index, measure)| {
        let source = match &measure.expression {
            MeasureExpr::Column(column) if column == input => Some(AggregateSource::Value),
            MeasureExpr::Binary { lhs, .. } if lhs == input => Some(AggregateSource::Value),
            MeasureExpr::StatsPair { value, .. } if value == input => Some(AggregateSource::Value),
            MeasureExpr::StatsPair { rhs, .. } if rhs == input => Some(AggregateSource::Rhs),
            _ => None,
        }?;
        Some((index, source))
    });
    let (measure_index, source, hidden) = if let Some((index, source)) = existing {
        (index, source, false)
    } else {
        if measures.len() == PGACCEL_GROUPED_AGG_MAX_MEASURES {
            return Err(ShapeDecline::TooManyMeasures {
                actual: measures.len() + 1,
                maximum: PGACCEL_GROUPED_AGG_MAX_MEASURES,
            });
        }
        measures.push(MeasureSpec {
            expression: MeasureExpr::Column(*input),
            outputs: vec![crate::engine::spec::AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Count,
            }],
            filter: FilterSpec::None,
        });
        (measures.len() - 1, AggregateSource::Value, true)
    };
    Ok(DescriptorMeasurePlan {
        projected_measure_count,
        descriptor_measure_count: u32::try_from(measures.len()).map_err(|_| {
            ShapeDecline::TooManyMeasures {
                actual: measures.len(),
                maximum: PGACCEL_GROUPED_AGG_MAX_MEASURES,
            }
        })?,
        fact_filter: Some(DescriptorFilterBinding {
            measure_index: u32::try_from(measure_index)
                .map_err(|_| ShapeDecline::InvalidSpec("measure index overflow".into()))?,
            source,
            hidden,
        }),
        derived_fact_mask: None,
        derived_spatial_mask: false,
    })
}

fn estimate_relation_bytes(input: &ShapeInput, relation_oid: u32, attnos: &[i32]) -> Option<u64> {
    let relation = input
        .relations
        .iter()
        .find(|relation| relation.relation_oid == relation_oid)?;
    let row_width = attnos.iter().try_fold(0_u64, |width, attno| {
        relation
            .column_widths
            .get(attno)
            .map(|column_width| width.saturating_add(u64::from(*column_width)))
    })?;
    Some(row_width.saturating_mul(relation.estimated_rows))
}

fn build_residency(
    input: &ShapeInput,
    required_relations: &[RequiredRelation],
    model: &TypedCostModel,
) -> Result<ResidencyEstimate, ShapeDecline> {
    let mut total_required_bytes = Some(0_u64);
    let mut missing_bytes = Some(0_u64);
    let mut missing_rows = 0_u64;
    let mut relations = Vec::with_capacity(required_relations.len());
    for required in required_relations {
        let relation = input
            .relations
            .iter()
            .find(|relation| relation.relation_oid == required.relation_oid)
            .ok_or(ShapeDecline::UnsupportedColumn {
                relation_oid: required.relation_oid,
                attno: 0,
            })?;
        let estimated_bytes =
            estimate_relation_bytes(input, required.relation_oid, &required.attnos);
        total_required_bytes = match (total_required_bytes, estimated_bytes) {
            (Some(total), Some(bytes)) => Some(total.saturating_add(bytes)),
            _ => None,
        };
        if relation.residency != RelationResidency::Resident {
            missing_rows = missing_rows.saturating_add(relation.estimated_rows);
            missing_bytes = match (missing_bytes, estimated_bytes) {
                (Some(total), Some(bytes)) => Some(total.saturating_add(bytes)),
                _ => None,
            };
        }
        relations.push(RelationResidencyRequirement {
            relation_oid: required.relation_oid,
            attnos: required.attnos.clone(),
            state: relation.residency,
            estimated_rows: relation.estimated_rows,
            estimated_bytes,
        });
    }
    // ShapeInput does not yet carry the exact selected/pinned/existing type
    // union. Preserve the prior variable-width row charge here; exact
    // residency resolution replaces it before production cost admission.
    let byte_cost = missing_bytes.map_or(0.0, |bytes| {
        bytes as f64 * model.coefficients.resident_load_per_byte_cost.get()
    });
    let row_cost = missing_rows as f64 * model.coefficients.preagg_dim_materialize_cost.get();
    let load_cost = (row_cost + byte_cost) / f64::from(input.expected_reuses.get());
    Ok(ResidencyEstimate {
        relations,
        total_required_bytes,
        missing_bytes,
        missing_rows,
        expected_reuses: input.expected_reuses,
        amortized_load_cost: PgCost::new(load_cost),
    })
}

/// Convert catalog-normalized query facts into the unified planner bundle.
pub fn build_shape(input: ShapeInput, model: &TypedCostModel) -> Result<ShapePlan, ShapeDecline> {
    reject_modifiers(input.modifiers)?;
    validate_relations(&input)?;
    if input.aggregates.is_empty() {
        return Err(ShapeDecline::NoAggregate);
    }
    if input.projections.is_empty() {
        return Err(ShapeDecline::UnsupportedProjection);
    }
    if input.group_keys.len() > PGACCEL_GROUPED_AGG_MAX_KEYS {
        return Err(ShapeDecline::TooManyGroupKeys {
            actual: input.group_keys.len(),
            maximum: PGACCEL_GROUPED_AGG_MAX_KEYS,
        });
    }
    for group in &input.group_keys {
        match group {
            PlannerGroupKey::Column(column) => {
                validate_planner_column(&input, *column)?;
                validate_group_key_type(column.column.type_oid)?;
                validate_key_collation(*column)?;
            }
            PlannerGroupKey::Expression {
                source,
                type_oid,
                collation_oid,
                collation_is_deterministic,
            } => {
                if *type_oid == 0
                    || !matches!(
                        source,
                        GroupKeySource::Expression { .. }
                            | GroupKeySource::H3CellToParent { .. }
                            | GroupKeySource::H3LatLngToCell { .. }
                    )
                {
                    return Err(ShapeDecline::UnsupportedGroupExpression);
                }
                validate_key_metadata(*type_oid, *collation_oid, *collation_is_deterministic)?;
            }
        }
        if !input.projections.iter().any(
            |projection| matches!(projection, InputProjection::Group { key, .. } if key == group),
        ) {
            return match group {
                PlannerGroupKey::Column(column) => Err(ShapeDecline::UnprojectedGroupKey {
                    relation_oid: column.column.relation_oid,
                    attno: column.column.attno,
                }),
                PlannerGroupKey::Expression { .. } => Err(ShapeDecline::UnsupportedProjection),
            };
        }
    }

    let fact_varno = choose_fact_varno(&input)?;
    let fact = relation_by_varno(&input, fact_varno)?;
    let dimensions = orient_dimensions(&input, fact_varno)?;
    if dimensions.len() > PGACCEL_GROUPED_AGG_MAX_DIMS {
        return Err(ShapeDecline::TooManyDimensions {
            actual: dimensions.len(),
            maximum: PGACCEL_GROUPED_AGG_MAX_DIMS,
        });
    }

    let mut dim_index_by_oid = BTreeMap::new();
    let mut star_dims = Vec::with_capacity(dimensions.len());
    for (index, dimension) in dimensions.iter().enumerate() {
        dim_index_by_oid.insert(dimension.relation_oid, index);
        let filter = relation_filter(&input, dimension.relation_oid)?;
        validate_dimension_filter(&filter)?;
        star_dims.push(DimSpec {
            relation_oid: dimension.relation_oid,
            fact_key: dimension.fact_key.column,
            dim_key: dimension.dim_key.column,
            collation_oid: dimension.collation_oid,
            multiplicity: dimension.multiplicity,
            filter,
        });
    }

    let mut group_keys = Vec::with_capacity(input.group_keys.len());
    for group in &input.group_keys {
        let (source, type_oid, collation_oid) = match group {
            PlannerGroupKey::Column(group) => {
                let source = if group.varno == fact_varno {
                    GroupKeySource::FactColumn(group.column)
                } else {
                    let dimension_index = dim_index_by_oid
                        .get(&group.column.relation_oid)
                        .copied()
                        .ok_or(ShapeDecline::UnsupportedGroupExpression)?;
                    let dimension = &dimensions[dimension_index];
                    if dimension.multiplicity != JoinMultiplicity::Unique {
                        return Err(ShapeDecline::GroupedByNonUniqueDimension {
                            relation_oid: group.column.relation_oid,
                            attno: group.column.attno,
                        });
                    }
                    GroupKeySource::StarDimension {
                        dim_index: u32::try_from(dimension_index).map_err(|_| {
                            ShapeDecline::TooManyDimensions {
                                actual: dimensions.len(),
                                maximum: PGACCEL_GROUPED_AGG_MAX_DIMS,
                            }
                        })?,
                        group_column: group.column,
                    }
                };
                (source, group.column.type_oid, group.collation_oid)
            }
            PlannerGroupKey::Expression {
                source,
                type_oid,
                collation_oid,
                ..
            } => (source.clone(), *type_oid, *collation_oid),
        };
        group_keys.push(GroupKeyRef {
            source,
            type_oid,
            collation_oid,
            encoding: GroupKeyEncoding::Hash,
        });
    }

    let mut measures = Vec::<MeasureSpec>::new();
    let mut aggregate_locations = Vec::with_capacity(input.aggregates.len());
    for aggregate in &input.aggregates {
        validate_measure_descriptor_capability(aggregate)?;
        let measure_index = if let Some(index) = measures.iter().position(|measure| {
            measure.expression == aggregate.expression && measure.filter == aggregate.filter
        }) {
            if !measures[index].outputs.contains(&aggregate.output) {
                measures[index].outputs.push(aggregate.output);
            }
            index
        } else {
            if measures.len() == PGACCEL_GROUPED_AGG_MAX_MEASURES {
                return Err(ShapeDecline::TooManyMeasures {
                    actual: measures.len() + 1,
                    maximum: PGACCEL_GROUPED_AGG_MAX_MEASURES,
                });
            }
            measures.push(MeasureSpec {
                expression: aggregate.expression.clone(),
                outputs: vec![aggregate.output],
                filter: aggregate.filter.clone(),
            });
            measures.len() - 1
        };
        aggregate_locations.push(measure_index);
    }

    let mut projections = Vec::with_capacity(input.projections.len());
    for projection in &input.projections {
        let (output, source) = match projection {
            InputProjection::Group { key, output } => {
                let (type_oid, collation_oid) = match key {
                    PlannerGroupKey::Column(column) => {
                        (column.column.type_oid, column.collation_oid)
                    }
                    PlannerGroupKey::Expression {
                        type_oid,
                        collation_oid,
                        ..
                    } => (*type_oid, *collation_oid),
                };
                if output.source_type_oid != type_oid {
                    return Err(ShapeDecline::ProjectionSourceTypeMismatch {
                        expected_type_oid: type_oid,
                        actual_type_oid: output.source_type_oid,
                    });
                }
                if output.result_type_oid != type_oid
                    || output.result_type_oid == 0
                    || output.result_typmod < -1
                    || output.result_collation_oid != collation_oid
                {
                    return Err(ShapeDecline::UnsupportedProjection);
                }
                let key_index = input
                    .group_keys
                    .iter()
                    .position(|group| group == key)
                    .ok_or(ShapeDecline::UnsupportedProjection)?;
                (
                    *output,
                    ProjectionSource::GroupKey {
                        key_index: u32::try_from(key_index)
                            .map_err(|_| ShapeDecline::UnsupportedProjection)?,
                    },
                )
            }
            InputProjection::Aggregate {
                aggregate_index,
                output,
            } => {
                let aggregate_index_usize = usize::try_from(*aggregate_index).map_err(|_| {
                    ShapeDecline::InvalidProjectionReference {
                        aggregate_index: *aggregate_index,
                    }
                })?;
                let aggregate = input.aggregates.get(aggregate_index_usize).ok_or(
                    ShapeDecline::InvalidProjectionReference {
                        aggregate_index: *aggregate_index,
                    },
                )?;
                let source_type_oid = aggregate_source_type(aggregate)?;
                if output.source_type_oid != source_type_oid {
                    return Err(ShapeDecline::ProjectionSourceTypeMismatch {
                        expected_type_oid: source_type_oid,
                        actual_type_oid: output.source_type_oid,
                    });
                }
                let expected_result_type =
                    expected_aggregate_result_type(source_type_oid, aggregate.output.kind).ok_or(
                        ShapeDecline::UnsupportedAggregateInput {
                            kind: aggregate.output.kind,
                            type_oid: source_type_oid,
                        },
                    )?;
                if output.result_type_oid != expected_result_type {
                    return Err(ShapeDecline::AggregateResultTypeMismatch {
                        source_type_oid,
                        kind: aggregate.output.kind,
                        result_type_oid: output.result_type_oid,
                    });
                }
                if output.result_type_oid == 0
                    || output.result_typmod < -1
                    || output.result_collation_oid != 0
                    || (aggregate.output.kind == AggregateKind::Count) == output.nullable
                {
                    return Err(ShapeDecline::UnsupportedProjection);
                }
                let measure_index = aggregate_locations[aggregate_index_usize];
                (
                    *output,
                    ProjectionSource::Aggregate {
                        measure_index: u32::try_from(measure_index)
                            .map_err(|_| ShapeDecline::UnsupportedProjection)?,
                        source: aggregate.output.source,
                        kind: aggregate.output.kind,
                    },
                )
            }
        };
        projections.push(ProjectionSlot {
            source,
            source_type_oid: output.source_type_oid,
            result_type_oid: output.result_type_oid,
            result_typmod: output.result_typmod,
            result_collation_oid: output.result_collation_oid,
            nullable: output.nullable,
        });
    }

    let fact_filter = relation_filter(&input, fact.relation_oid)?;
    if matches!(fact_filter, FilterSpec::Spatial { .. })
        && spatial_point_polygon_vertices(&fact_filter).is_none()
    {
        return Err(ShapeDecline::SpatialWorkShapeUnproved);
    }
    let descriptor_measures = fact_filter_binding(&mut measures, &fact_filter)?;
    let spec = AggQuerySpec {
        fact_rel: fact.relation_oid,
        group_keys,
        measures,
        fact_filter,
        star_dims,
        having: None,
    };
    spec.validate()
        .map_err(|error| ShapeDecline::InvalidSpec(error.to_string()))?;

    let mut required = BTreeMap::<u32, BTreeSet<i32>>::new();
    required.entry(spec.fact_rel).or_default();
    for dimension in &spec.star_dims {
        required.entry(dimension.relation_oid).or_default();
    }
    for key in &spec.group_keys {
        collect_group_key_columns(&mut required, &key.source);
    }
    for measure in &spec.measures {
        collect_measure_columns(&mut required, &measure.expression);
        collect_filter_columns(&mut required, &measure.filter);
    }
    collect_filter_columns(&mut required, &spec.fact_filter);
    for dimension in &spec.star_dims {
        collect_required_column(&mut required, dimension.fact_key);
        collect_required_column(&mut required, dimension.dim_key);
        collect_filter_columns(&mut required, &dimension.filter);
    }
    let required_relations = required
        .into_iter()
        .map(|(relation_oid, attnos)| RequiredRelation {
            relation_oid,
            attnos: attnos.into_iter().collect(),
        })
        .collect::<Vec<_>>();
    let residency = build_residency(&input, &required_relations, model)?;

    let mut digest_words = spec
        .encode_i32()
        .map_err(|error| ShapeDecline::Codec(error.to_string()))?;
    let projection = AggOutputProjection { slots: projections };
    digest_words.extend(
        projection
            .encode_i32(&spec)
            .map_err(|error| ShapeDecline::Codec(error.to_string()))?,
    );

    let (cost, cost_gate, estimated_output_rows) =
        estimate_shape_cost(&input, &spec, &residency, model);
    let dictionary_joins = spec
        .star_dims
        .iter()
        .enumerate()
        .filter(|(_, dimension)| matches!(dimension.fact_key.type_oid, 25 | 1042 | 1043))
        .map(|(index, dimension)| {
            Ok(DictionaryJoinRequirement {
                dim_index: u32::try_from(index)
                    .map_err(|_| ShapeDecline::InvalidGroupKeyResolution)?,
                fact_key: dimension.fact_key,
                dim_key: dimension.dim_key,
                collation_oid: dimension.collation_oid,
            })
        })
        .collect::<Result<Vec<_>, ShapeDecline>>()?;
    let derived_keys = spec
        .group_keys
        .iter()
        .enumerate()
        .filter(|(_, key)| {
            matches!(
                &key.source,
                GroupKeySource::Expression { .. }
                    | GroupKeySource::H3CellToParent { .. }
                    | GroupKeySource::H3LatLngToCell { .. }
            )
        })
        .map(|(index, key)| {
            Ok(DerivedGroupKeyRequirement {
                key_index: u32::try_from(index)
                    .map_err(|_| ShapeDecline::InvalidGroupKeyResolution)?,
                source: key.source.clone(),
                result_type_oid: key.type_oid,
            })
        })
        .collect::<Result<Vec<_>, ShapeDecline>>()?;
    let descriptor_resolution = if spec.group_keys.is_empty() && dictionary_joins.is_empty() {
        DescriptorResolution::Ready
    } else {
        let grouping_mode = if derived_keys.is_empty() {
            DescriptorGroupingMode::DenseDictionary
        } else {
            DescriptorGroupingMode::Hash
        };
        let dictionary_keys = if grouping_mode == DescriptorGroupingMode::DenseDictionary {
            spec.group_keys
                .iter()
                .enumerate()
                .map(|(index, key)| {
                    Ok(DictionaryKeyRequirement {
                        key_index: u32::try_from(index)
                            .map_err(|_| ShapeDecline::InvalidGroupKeyResolution)?,
                        source: key.source.clone(),
                        collation_oid: key.collation_oid,
                    })
                })
                .collect::<Result<Vec<_>, ShapeDecline>>()?
        } else {
            Vec::new()
        };
        DescriptorResolution::BeginTimeArtifacts {
            dictionary_keys,
            derived_keys,
            joins: dictionary_joins,
            grouping_mode,
            max_group_count: model.memory.gpu_hash_agg_max_groups.get(),
        }
    };
    Ok(ShapePlan {
        spec,
        projections: projection.slots,
        required_relations,
        estimated_output_rows,
        digest_words,
        descriptor_resolution,
        descriptor_measures,
        residency,
        cost,
        cost_gate,
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::super::{AggregateExpr, EquiJoin, OutputMetadata, RelationShape};
    use super::*;
    use crate::engine::cost::DeviceLimits;
    use crate::engine::spec::{
        AggregateOutput, BinaryMeasureOp, ColumnRef, ScalarRange, ScalarValue, SpatialOperand,
        SpatialPredicateKind, SpatialValueKind, SpatialValueMetadata,
    };

    fn column(relation_oid: u32, attno: i32, type_oid: u32) -> ColumnRef {
        ColumnRef {
            relation_oid,
            attno,
            type_oid,
        }
    }

    fn planner_column(
        varno: pgrx::pg_sys::Index,
        relation_oid: u32,
        attno: i32,
        type_oid: u32,
    ) -> PlannerColumn {
        PlannerColumn {
            varno,
            column: column(relation_oid, attno, type_oid),
            type_modifier: -1,
            collation_oid: 0,
            collation_is_deterministic: true,
        }
    }

    fn relation(varno: pgrx::pg_sys::Index, relation_oid: u32) -> RelationShape {
        RelationShape {
            varno,
            relation_oid,
            estimated_rows: 100,
            unique_attnos: BTreeSet::new(),
            column_widths: BTreeMap::from([(1, 4), (2, 8), (3, 16)]),
            residency: RelationResidency::Unknown,
        }
    }

    fn aggregate(expression: MeasureExpr, kind: AggregateKind) -> AggregateExpr {
        AggregateExpr {
            expression,
            output: AggregateOutput {
                source: AggregateSource::Value,
                kind,
            },
            filter: FilterSpec::None,
        }
    }

    fn input() -> ShapeInput {
        ShapeInput {
            relations: vec![relation(1, 100)],
            joins: Vec::new(),
            group_keys: Vec::new(),
            aggregates: vec![aggregate(MeasureExpr::CountStar, AggregateKind::Count)],
            projections: vec![InputProjection::Aggregate {
                aggregate_index: 0,
                output: OutputMetadata {
                    source_type_oid: 0,
                    result_type_oid: 20,
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: false,
                },
            }],
            relation_filters: Vec::new(),
            estimated_output_rows: 1,
            expected_reuses: NonZeroU32::MIN,
            modifiers: ShapeModifiers::default(),
        }
    }

    #[test]
    fn modifier_and_key_metadata_rejections_are_complete_and_ordered() {
        type ModifierCase = (fn(&mut ShapeModifiers), ShapeDecline);
        let cases: &[ModifierCase] = &[
            (
                |m| m.has_window_functions = true,
                ShapeDecline::WindowFunctions,
            ),
            (
                |m| m.has_target_srfs = true,
                ShapeDecline::TargetSetReturningFunction,
            ),
            (|m| m.has_sublinks = true, ShapeDecline::Sublink),
            (
                |m| m.has_recursive_query = true,
                ShapeDecline::RecursiveQuery,
            ),
            (|m| m.has_modifying_cte = true, ShapeDecline::ModifyingCte),
            (|m| m.has_row_security = true, ShapeDecline::RowSecurity),
            (|m| m.has_distinct = true, ShapeDecline::Distinct),
            (|m| m.has_grouping_sets = true, ShapeDecline::GroupingSets),
            (|m| m.has_group_distinct = true, ShapeDecline::GroupDistinct),
            (|m| m.has_having = true, ShapeDecline::Having),
            (|m| m.has_set_operations = true, ShapeDecline::SetOperations),
            (|m| m.has_row_marks = true, ShapeDecline::RowMarks),
        ];
        assert_eq!(reject_modifiers(ShapeModifiers::default()), Ok(()));
        for (mutate, expected) in cases {
            let mut modifiers = ShapeModifiers::default();
            mutate(&mut modifiers);
            assert_eq!(reject_modifiers(modifiers), Err(expected.clone()));
        }

        for type_oid in [16, 20, 21, 23, 25, 700, 701, 1042, 1043, 1082, 1114, 1184] {
            assert_eq!(validate_group_key_type(type_oid), Ok(()));
        }
        assert_eq!(
            validate_group_key_type(17),
            Err(ShapeDecline::UnsupportedGroupKeyType { type_oid: 17 })
        );
        assert_eq!(validate_key_metadata(25, 100, true), Ok(()));
        assert_eq!(
            validate_key_metadata(25, 0, true),
            Err(ShapeDecline::NondeterministicKeyCollation { collation_oid: 0 })
        );
        assert_eq!(
            validate_key_metadata(1043, 100, false),
            Err(ShapeDecline::NondeterministicKeyCollation { collation_oid: 100 })
        );
        assert_eq!(
            validate_key_metadata(23, 100, true),
            Err(ShapeDecline::InvalidKeyCollation {
                type_oid: 23,
                collation_oid: 100,
            })
        );
    }

    #[test]
    fn descriptor_measure_capability_covers_every_expression_family() {
        let int4 = column(100, 1, 23);
        let int8 = column(100, 2, 20);
        let float8 = column(100, 3, 701);
        let text = column(100, 4, 25);
        let boolean = column(100, 5, 16);
        let int2 = column(100, 6, 21);

        assert_eq!(
            validate_measure_descriptor_capability(&aggregate(
                MeasureExpr::CountStar,
                AggregateKind::Count
            )),
            Ok(())
        );
        assert!(matches!(
            validate_measure_descriptor_capability(&aggregate(
                MeasureExpr::CountStar,
                AggregateKind::Sum
            )),
            Err(ShapeDecline::UnsupportedAggregateInput { type_oid: 0, .. })
        ));
        for kind in [
            AggregateKind::Sum,
            AggregateKind::Count,
            AggregateKind::Min,
            AggregateKind::Max,
        ] {
            assert_eq!(
                validate_measure_descriptor_capability(&aggregate(MeasureExpr::Column(int4), kind)),
                Ok(())
            );
        }
        assert_eq!(
            validate_measure_descriptor_capability(&aggregate(
                MeasureExpr::Column(boolean),
                AggregateKind::Count
            )),
            Ok(())
        );
        assert_eq!(
            validate_measure_descriptor_capability(&aggregate(
                MeasureExpr::Column(int2),
                AggregateKind::Count
            )),
            Ok(())
        );
        assert!(matches!(
            validate_measure_descriptor_capability(&aggregate(
                MeasureExpr::Column(int8),
                AggregateKind::Sum
            )),
            Err(ShapeDecline::NumericAccumulatorTypeUnavailable { type_oid: 20 })
        ));
        assert!(matches!(
            validate_measure_descriptor_capability(&aggregate(
                MeasureExpr::Column(float8),
                AggregateKind::Avg
            )),
            Err(ShapeDecline::FloatingAccumulatorSemantics)
        ));
        assert!(matches!(
            validate_measure_descriptor_capability(&aggregate(
                MeasureExpr::Column(int4),
                AggregateKind::StddevSamp
            )),
            Err(ShapeDecline::UnsupportedAggregateInput { .. })
        ));
        assert_eq!(
            validate_measure_descriptor_capability(&aggregate(
                MeasureExpr::Column(text),
                AggregateKind::Count
            )),
            Err(ShapeDecline::UnsupportedMeasureType { type_oid: 25 })
        );

        assert_eq!(
            validate_measure_descriptor_capability(&aggregate(
                MeasureExpr::Binary {
                    op: BinaryMeasureOp::Sub,
                    lhs: int4,
                    rhs: int8,
                },
                AggregateKind::Sum,
            )),
            Err(ShapeDecline::UnsupportedBinaryMeasure)
        );
        assert_eq!(
            validate_measure_descriptor_capability(&aggregate(
                MeasureExpr::Binary {
                    op: BinaryMeasureOp::Sub,
                    lhs: float8,
                    rhs: float8,
                },
                AggregateKind::Sum,
            )),
            Err(ShapeDecline::FloatingExpressionSemantics)
        );
        assert_eq!(
            validate_measure_descriptor_capability(&aggregate(
                MeasureExpr::Binary {
                    op: BinaryMeasureOp::Mul,
                    lhs: int8,
                    rhs: int8,
                },
                AggregateKind::Count,
            )),
            Err(ShapeDecline::IntegerExpressionOverflowSemantics)
        );
        assert_eq!(
            validate_measure_descriptor_capability(&aggregate(
                MeasureExpr::StatsPair {
                    value: float8,
                    rhs: float8,
                },
                AggregateKind::Count,
            )),
            Err(ShapeDecline::FloatingAccumulatorSemantics)
        );
        assert_eq!(
            validate_measure_descriptor_capability(&aggregate(
                MeasureExpr::Bytecode {
                    inputs: vec![int4],
                    program: vec![1],
                    result_type_oid: 23,
                },
                AggregateKind::Count,
            )),
            Err(ShapeDecline::UnsupportedMeasureExpression)
        );

        let mut filtered = aggregate(MeasureExpr::Column(int4), AggregateKind::Count);
        filtered.filter = FilterSpec::Ranges {
            input: int4,
            ranges: Vec::new(),
        };
        assert_eq!(
            validate_measure_descriptor_capability(&filtered),
            Err(ShapeDecline::UnsupportedAggregateModifier)
        );
        filtered = aggregate(MeasureExpr::Column(int4), AggregateKind::Sum);
        filtered.filter = FilterSpec::Ranges {
            input: int4,
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(200),
                hi: ScalarValue::I32(800),
            }],
        };
        assert_eq!(validate_measure_descriptor_capability(&filtered), Ok(()));
        filtered.output.kind = AggregateKind::Count;
        assert_eq!(
            validate_measure_descriptor_capability(&filtered),
            Err(ShapeDecline::UnsupportedAggregateModifier)
        );
        filtered.output.kind = AggregateKind::Sum;
        let FilterSpec::Ranges { ranges, .. } = &mut filtered.filter else {
            unreachable!("bounded filter fixture")
        };
        ranges[0].lo = ScalarValue::I32(i32::MIN);
        assert_eq!(
            validate_measure_descriptor_capability(&filtered),
            Err(ShapeDecline::UnsupportedAggregateModifier)
        );
        filtered.output.source = AggregateSource::Rhs;
        filtered.filter = FilterSpec::None;
        assert_eq!(
            validate_measure_descriptor_capability(&filtered),
            Err(ShapeDecline::UnsupportedAggregateModifier)
        );
    }

    #[test]
    fn relation_graph_validation_rejects_ambiguous_and_disconnected_inputs() {
        let mut candidate = input();
        candidate.relations.clear();
        assert_eq!(
            validate_relations(&candidate),
            Err(ShapeDecline::DisconnectedJoinGraph)
        );

        candidate = input();
        candidate.relations = (0..=MAX_RELATIONS)
            .map(|index| relation(index as pgrx::pg_sys::Index + 1, index as u32 + 100))
            .collect();
        assert!(matches!(
            validate_relations(&candidate),
            Err(ShapeDecline::TooManyRelations { .. })
        ));

        candidate = input();
        candidate.relations.push(relation(1, 200));
        assert_eq!(
            validate_relations(&candidate),
            Err(ShapeDecline::DuplicatePlannerRelation { varno: 1 })
        );
        candidate = input();
        candidate.relations.push(relation(2, 100));
        assert_eq!(
            validate_relations(&candidate),
            Err(ShapeDecline::SelfJoinUsesAmbiguousRelationOid { relation_oid: 100 })
        );

        candidate = input();
        candidate.relation_filters.push((999, FilterSpec::None));
        assert_eq!(
            validate_relations(&candidate),
            Err(ShapeDecline::UnsupportedColumn {
                relation_oid: 999,
                attno: 0,
            })
        );
        candidate = input();
        candidate.relation_filters.push((
            100,
            FilterSpec::Bytecode {
                inputs: Vec::new(),
                program: vec![1],
            },
        ));
        assert_eq!(
            validate_relations(&candidate),
            Err(ShapeDecline::UnsupportedColumn {
                relation_oid: 0,
                attno: 0,
            })
        );

        assert_eq!(choose_fact_varno(&input()), Ok(1));
        candidate = input();
        let same = planner_column(1, 100, 1, 23);
        candidate.joins.push(EquiJoin {
            left: same,
            right: same,
        });
        assert_eq!(
            choose_fact_varno(&candidate),
            Err(ShapeDecline::NonStarJoinGraph)
        );

        candidate = input();
        candidate.relations.push(relation(2, 200));
        candidate.joins.push(EquiJoin {
            left: planner_column(1, 100, 1, 23),
            right: planner_column(2, 200, 1, 20),
        });
        assert!(matches!(
            choose_fact_varno(&candidate),
            Err(ShapeDecline::JoinKeyTypeMismatch { .. })
        ));
    }

    #[test]
    fn filter_collection_binding_and_residency_estimates_are_exact() {
        let bool_col = column(100, 1, 16);
        let int_col = column(100, 2, 23);
        let rhs_col = column(100, 3, 23);
        let other_col = column(200, 1, 23);
        let range = FilterSpec::Ranges {
            input: int_col,
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(1),
                hi: ScalarValue::I32(9),
            }],
        };
        assert_eq!(validate_dimension_filter(&range), Ok(()));
        assert_eq!(
            validate_dimension_filter(&FilterSpec::Mask {
                input: bool_col,
                kind: MaskKind::Sql,
            }),
            Ok(())
        );
        assert_eq!(
            validate_dimension_filter(&FilterSpec::Mask {
                input: int_col,
                kind: MaskKind::Recheck,
            }),
            Err(ShapeDecline::UnsupportedFilterType { type_oid: 23 })
        );
        assert_eq!(
            validate_dimension_filter(&FilterSpec::Bytecode {
                inputs: vec![int_col],
                program: vec![1],
            }),
            Err(ShapeDecline::UnsupportedPredicate)
        );

        let metadata = SpatialValueMetadata {
            kind: SpatialValueKind::Geometry,
            typmod: -1,
            srid: Some(4_326),
        };
        let spatial = FilterSpec::Spatial {
            predicate: SpatialPredicateKind::Intersects,
            left: SpatialOperand::Column {
                column: int_col,
                metadata,
            },
            right: SpatialOperand::Constant {
                metadata,
                bytes: vec![1, 2].into_boxed_slice(),
            },
            distance: None,
        };
        assert_eq!(
            validate_dimension_filter(&spatial),
            Err(ShapeDecline::SpatialFilterOutsideFactRelation)
        );

        let mut required = BTreeMap::new();
        collect_filter_columns(&mut required, &FilterSpec::None);
        collect_filter_columns(
            &mut required,
            &FilterSpec::Mask {
                input: bool_col,
                kind: MaskKind::Sql,
            },
        );
        collect_filter_columns(&mut required, &range);
        collect_filter_columns(
            &mut required,
            &FilterSpec::Bytecode {
                inputs: vec![rhs_col, other_col],
                program: vec![1],
            },
        );
        collect_filter_columns(&mut required, &spatial);
        collect_group_key_columns(
            &mut required,
            &GroupKeySource::H3LatLngToCell {
                latitude: int_col,
                longitude: rhs_col,
                resolution: 7,
            },
        );
        collect_measure_columns(
            &mut required,
            &MeasureExpr::StatsPair {
                value: int_col,
                rhs: rhs_col,
            },
        );
        assert_eq!(required.get(&100), Some(&BTreeSet::from([1, 2, 3])));
        assert_eq!(required.get(&200), Some(&BTreeSet::from([1])));

        let mut measures = vec![MeasureSpec {
            expression: MeasureExpr::StatsPair {
                value: int_col,
                rhs: rhs_col,
            },
            outputs: vec![AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Count,
            }],
            filter: FilterSpec::None,
        }];
        let binding = fact_filter_binding(
            &mut measures,
            &FilterSpec::Ranges {
                input: rhs_col,
                ranges: Vec::new(),
            },
        )
        .expect("existing RHS lane binds");
        assert_eq!(binding.projected_measure_count, 1);
        assert_eq!(binding.descriptor_measure_count, 1);
        assert_eq!(
            binding.fact_filter,
            Some(DescriptorFilterBinding {
                measure_index: 0,
                source: AggregateSource::Rhs,
                hidden: false,
            })
        );

        let hidden_col = column(100, 4, 23);
        let hidden = fact_filter_binding(
            &mut measures,
            &FilterSpec::Ranges {
                input: hidden_col,
                ranges: Vec::new(),
            },
        )
        .expect("missing filter lane appends a hidden measure");
        assert_eq!(hidden.projected_measure_count, 1);
        assert_eq!(hidden.descriptor_measure_count, 2);
        assert!(hidden.fact_filter.expect("binding").hidden);

        let mut shape_input = input();
        assert_eq!(
            estimate_relation_bytes(&shape_input, 100, &[1, 2]),
            Some(1_200)
        );
        assert_eq!(estimate_relation_bytes(&shape_input, 100, &[99]), None);
        assert_eq!(estimate_relation_bytes(&shape_input, 999, &[1]), None);
        shape_input.relations[0].residency = RelationResidency::Resident;
        let model = TypedCostModel::from_limits(&DeviceLimits::cpu_only());
        let residency = build_residency(
            &shape_input,
            &[RequiredRelation {
                relation_oid: 100,
                attnos: vec![1, 2],
            }],
            &model,
        )
        .expect("known relation residency builds");
        assert_eq!(residency.total_required_bytes, Some(1_200));
        assert_eq!(residency.missing_bytes, Some(0));
        assert_eq!(residency.missing_rows, 0);
    }
}
