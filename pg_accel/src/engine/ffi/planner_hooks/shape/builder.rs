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

use super::cost::estimate_shape_cost;
use super::{
    DescriptorFilterBinding, DescriptorMeasurePlan, DescriptorResolution,
    DictionaryJoinRequirement, DictionaryKeyRequirement, InputProjection, PlannerColumn,
    ProjectionSlot, ProjectionSource, RelationResidency, RelationResidencyRequirement,
    RequiredRelation, ResidencyEstimate, ShapeDecline, ShapeInput, ShapeModifiers, ShapePlan,
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

fn validate_key_collation(column: PlannerColumn) -> Result<(), ShapeDecline> {
    const TEXTOID: u32 = 25;
    const BPCHAROID: u32 = 1042;
    const VARCHAROID: u32 = 1043;
    if matches!(column.column.type_oid, TEXTOID | BPCHAROID | VARCHAROID)
        && (column.collation_oid == 0 || !column.collation_is_deterministic)
    {
        return Err(ShapeDecline::NondeterministicKeyCollation {
            collation_oid: column.collation_oid,
        });
    }
    if !matches!(column.column.type_oid, TEXTOID | BPCHAROID | VARCHAROID)
        && column.collation_oid != 0
    {
        return Err(ShapeDecline::InvalidKeyCollation {
            type_oid: column.column.type_oid,
            collation_oid: column.collation_oid,
        });
    }
    Ok(())
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

fn validate_measure_descriptor_capability(
    aggregate: &super::AggregateExpr,
) -> Result<(), ShapeDecline> {
    const INT4OID: u32 = 23;
    const INT8OID: u32 = 20;
    const FLOAT8OID: u32 = 701;
    if aggregate.filter != FilterSpec::None || aggregate.output.source != AggregateSource::Value {
        return Err(ShapeDecline::UnsupportedAggregateModifier);
    }
    let validate_column =
        |column: &crate::engine::spec::ColumnRef| match (column.type_oid, aggregate.output.kind) {
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
            _ if !matches!(column.type_oid, INT4OID | INT8OID | FLOAT8OID) => {
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
    const INT4OID: u32 = 23;
    const INT8OID: u32 = 20;
    const FLOAT8OID: u32 = 701;
    match (source_type_oid, kind) {
        (0 | INT4OID | INT8OID | FLOAT8OID, AggregateKind::Count)
        | (INT4OID, AggregateKind::Sum) => Some(INT8OID),
        (INT4OID, AggregateKind::Min | AggregateKind::Max) => Some(INT4OID),
        (INT8OID, AggregateKind::Min | AggregateKind::Max) => Some(INT8OID),
        (FLOAT8OID, AggregateKind::Min | AggregateKind::Max) => Some(FLOAT8OID),
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
            FilterSpec::Spatial { left, .. } => Some(left.relation_oid),
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
        if !matches!(join.left.column.type_oid, 23 | 25 | 1042 | 1043) {
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
        FilterSpec::Bytecode { .. } | FilterSpec::Spatial { .. } => {
            Err(ShapeDecline::UnsupportedPredicate)
        }
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
            collect_required_column(required, *left);
            collect_required_column(required, *right);
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
        FilterSpec::Bytecode { .. } | FilterSpec::Spatial { .. } => {
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
    let load_cost = missing_rows as f64 * model.coefficients.preagg_dim_materialize_cost.get()
        / f64::from(input.expected_reuses.get());
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
    if input.group_columns.len() > PGACCEL_GROUPED_AGG_MAX_KEYS {
        return Err(ShapeDecline::TooManyGroupKeys {
            actual: input.group_columns.len(),
            maximum: PGACCEL_GROUPED_AGG_MAX_KEYS,
        });
    }
    for group in &input.group_columns {
        validate_planner_column(&input, *group)?;
        validate_group_key_type(group.column.type_oid)?;
        validate_key_collation(*group)?;
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

    let mut group_keys = Vec::with_capacity(input.group_columns.len());
    for group in &input.group_columns {
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
        group_keys.push(GroupKeyRef {
            source,
            type_oid: group.column.type_oid,
            collation_oid: group.collation_oid,
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
        let (output, source) = match *projection {
            InputProjection::Group { column, output } => {
                if output.source_type_oid != column.column.type_oid {
                    return Err(ShapeDecline::ProjectionSourceTypeMismatch {
                        expected_type_oid: column.column.type_oid,
                        actual_type_oid: output.source_type_oid,
                    });
                }
                if output.result_type_oid != column.column.type_oid
                    || output.result_type_oid == 0
                    || output.result_typmod < -1
                    || output.result_collation_oid != column.collation_oid
                {
                    return Err(ShapeDecline::UnsupportedProjection);
                }
                let key_index = input
                    .group_columns
                    .iter()
                    .position(|group| *group == column)
                    .ok_or(ShapeDecline::UnsupportedProjection)?;
                (
                    output,
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
                let aggregate_index_usize = usize::try_from(aggregate_index)
                    .map_err(|_| ShapeDecline::InvalidProjectionReference { aggregate_index })?;
                let aggregate = input
                    .aggregates
                    .get(aggregate_index_usize)
                    .ok_or(ShapeDecline::InvalidProjectionReference { aggregate_index })?;
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
                    output,
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
    for key in &input.group_columns {
        collect_required_column(&mut required, key.column);
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

    let (cost, cost_gate) = estimate_shape_cost(&input, &spec, &residency, model);
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
    let descriptor_resolution = if spec.group_keys.is_empty() && dictionary_joins.is_empty() {
        DescriptorResolution::Ready
    } else {
        DescriptorResolution::BeginTimeDictionary {
            keys: spec
                .group_keys
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
                .collect::<Result<Vec<_>, ShapeDecline>>()?,
            joins: dictionary_joins,
            max_group_count: model.memory.gpu_hash_agg_max_groups.get(),
        }
    };
    Ok(ShapePlan {
        spec,
        projections: projection.slots,
        required_relations,
        digest_words,
        descriptor_resolution,
        descriptor_measures,
        residency,
        cost,
        cost_gate,
    })
}
