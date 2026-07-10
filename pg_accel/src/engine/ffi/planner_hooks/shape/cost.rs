//! DeviceLimits-derived costing for neutral aggregate shapes.

use crate::engine::cost::{PgCost, Rows, TypedCostModel};
use crate::engine::spec::{AggQuerySpec, ColumnRef, MeasureExpr};

use super::{ResidencyEstimate, ShapeInput};

/// Named cost components. No component is adjusted to undercut PostgreSQL.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeCost {
    pub fact_scan: PgCost,
    pub dimension_setup: PgCost,
    pub join_probe: PgCost,
    pub aggregate: PgCost,
    pub output_materialization: PgCost,
    pub amortized_auto_load: PgCost,
    pub total: PgCost,
}

/// Device-derived capability gate, separate from cost comparison with a
/// native PostgreSQL path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeCostGate {
    Eligible,
    FactRowsBelowDeviceMinimum { estimated: Rows, required: Rows },
    DimensionRowsExceedDeviceMaximum { estimated: Rows, maximum: Rows },
    GroupsExceedDeviceMaximum { estimated: Rows, maximum: Rows },
}

fn rows(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn mul_cost(count: u64, coefficient: PgCost) -> f64 {
    count as f64 * coefficient.get()
}

fn expression_uses_fp64(expression: &MeasureExpr) -> bool {
    const FLOAT8OID: u32 = 701;
    let is_fp64 = |column: &ColumnRef| column.type_oid == FLOAT8OID;
    match expression {
        MeasureExpr::CountStar => false,
        MeasureExpr::Column(column) => is_fp64(column),
        MeasureExpr::Binary { lhs, rhs, .. } | MeasureExpr::StatsPair { value: lhs, rhs } => {
            is_fp64(lhs) || is_fp64(rhs)
        }
        MeasureExpr::Bytecode {
            inputs,
            result_type_oid,
            ..
        } => *result_type_oid == FLOAT8OID || inputs.iter().any(is_fp64),
    }
}

/// Cost one normalized shape exclusively from typed device coefficients.
#[must_use]
pub fn estimate_shape_cost(
    input: &ShapeInput,
    spec: &AggQuerySpec,
    residency: &ResidencyEstimate,
    model: &TypedCostModel,
) -> (ShapeCost, ShapeCostGate) {
    let fact_rows = input
        .relations
        .iter()
        .find(|relation| relation.relation_oid == spec.fact_rel)
        .map_or(0, |relation| relation.estimated_rows);
    let dimension_rows = spec
        .star_dims
        .iter()
        .filter_map(|dimension| {
            input
                .relations
                .iter()
                .find(|relation| relation.relation_oid == dimension.relation_oid)
        })
        .map(|relation| relation.estimated_rows)
        .fold(0_u64, u64::saturating_add);

    let fp64_multiplier = if !model.hardware.has_native_fp64
        && spec
            .measures
            .iter()
            .any(|measure| expression_uses_fp64(&measure.expression))
    {
        model.coefficients.soft_fp64_cost_multiplier
    } else {
        1.0
    };

    let fact_scan = mul_cost(fact_rows, model.coefficients.preagg_fact_scan_cost);
    let dimension_setup = mul_cost(
        dimension_rows,
        model.coefficients.preagg_dim_materialize_cost,
    );
    let join_probe =
        mul_cost(fact_rows, model.coefficients.preagg_probe_cost) * spec.star_dims.len() as f64;
    let aggregate_coefficient = if spec.group_keys.is_empty() {
        model.coefficients.gpu_op_cost_reduce
    } else {
        model.coefficients.gpu_op_cost_hash_agg
    };
    let aggregate =
        mul_cost(fact_rows, aggregate_coefficient) * spec.measures.len() as f64 * fp64_multiplier;
    let output_materialization = mul_cost(
        input.estimated_output_rows,
        model.coefficients.preagg_yield_cost,
    );
    let amortized_auto_load = residency.amortized_load_cost.get();
    let total = fact_scan
        + dimension_setup
        + join_probe
        + aggregate
        + output_materialization
        + amortized_auto_load;

    let cost = ShapeCost {
        fact_scan: PgCost::new(fact_scan),
        dimension_setup: PgCost::new(dimension_setup),
        join_probe: PgCost::new(join_probe),
        aggregate: PgCost::new(aggregate),
        output_materialization: PgCost::new(output_materialization),
        amortized_auto_load: PgCost::new(amortized_auto_load),
        total: PgCost::new(total),
    };

    let required_fact_rows = if !spec.star_dims.is_empty() {
        model.planner.gpu_preagg_min_fact_rows
    } else if spec.group_keys.is_empty() {
        model.planner.gpu_reduce_min_rows
    } else {
        model.planner.gpu_hash_agg_min_rows
    };
    let estimated_fact_rows = Rows::new(rows(fact_rows));
    let gate = if estimated_fact_rows < required_fact_rows {
        ShapeCostGate::FactRowsBelowDeviceMinimum {
            estimated: estimated_fact_rows,
            required: required_fact_rows,
        }
    } else if let Some(too_large) = spec.star_dims.iter().find_map(|dimension| {
        let relation = input
            .relations
            .iter()
            .find(|relation| relation.relation_oid == dimension.relation_oid)?;
        let estimated = Rows::new(rows(relation.estimated_rows));
        (estimated > model.memory.gpu_preagg_max_dim_rows).then_some(estimated)
    }) {
        ShapeCostGate::DimensionRowsExceedDeviceMaximum {
            estimated: too_large,
            maximum: model.memory.gpu_preagg_max_dim_rows,
        }
    } else {
        let estimated_groups = Rows::new(rows(input.estimated_output_rows));
        if !spec.group_keys.is_empty() && estimated_groups > model.memory.gpu_hash_agg_max_groups {
            ShapeCostGate::GroupsExceedDeviceMaximum {
                estimated: estimated_groups,
                maximum: model.memory.gpu_hash_agg_max_groups,
            }
        } else {
            ShapeCostGate::Eligible
        }
    };

    (cost, gate)
}
