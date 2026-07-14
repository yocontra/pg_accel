//! DeviceLimits-derived costing for neutral aggregate shapes.

use crate::engine::cost::{GPU_LAUNCH_OVERHEAD, PgCost, Rows, TypedCostModel};
use crate::engine::spec::{AggQuerySpec, ColumnRef, GroupKeySource, MeasureExpr};

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
    H3RowsBelowDeviceMinimum { estimated: Rows, required: Rows },
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

fn h3_parent_resolution(spec: &AggQuerySpec) -> Option<i32> {
    spec.group_keys.iter().find_map(|key| match key.source {
        GroupKeySource::H3CellToParent { resolution, .. } => Some(resolution),
        _ => None,
    })
}

fn h3_group_bound(fact_rows: u64, resolution: i32) -> Option<u64> {
    let resolution = u32::try_from(resolution)
        .ok()
        .filter(|value| *value <= 15)?;
    // H3 has `2 + 120 * 7^r` cells; reserve one additional group for NULL.
    let possible = 7_u64
        .checked_pow(resolution)?
        .checked_mul(120)?
        .checked_add(3)?;
    Some(fact_rows.min(possible))
}

fn h3_transform_chunks(fact_rows: u64, max_chunk_rows: Rows) -> u64 {
    if fact_rows == 0 {
        return 0;
    }
    let chunk_rows = u64::try_from(max_chunk_rows.get())
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or(1);
    fact_rows
        .checked_sub(1)
        .and_then(|rows| rows.checked_div(chunk_rows))
        .and_then(|chunks| chunks.checked_add(1))
        .unwrap_or(u64::MAX)
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
    let h3_resolution = h3_parent_resolution(spec);
    let h3_group_rows_bound =
        h3_resolution.and_then(|resolution| h3_group_bound(fact_rows, resolution));
    let aggregate =
        mul_cost(fact_rows, aggregate_coefficient) * spec.measures.len() as f64 * fp64_multiplier;
    let aggregate = aggregate
        + h3_resolution.map_or(0.0, |_| {
            let transform = mul_cost(fact_rows, model.coefficients.gpu_op_cost_h3_parent_resident);
            let launches = h3_transform_chunks(fact_rows, model.executor.gpu_h3_max_chunk_rows)
                as f64
                * GPU_LAUNCH_OVERHEAD;
            transform + launches
        });
    let estimated_output_rows = h3_group_rows_bound.map_or(input.estimated_output_rows, |bound| {
        input.estimated_output_rows.min(bound)
    });
    let output_materialization =
        mul_cost(estimated_output_rows, model.coefficients.preagg_yield_cost);
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

    let required_fact_rows = if h3_resolution.is_some() {
        model.planner.gpu_h3_group_min_rows
    } else if !spec.star_dims.is_empty() {
        model.planner.gpu_preagg_min_fact_rows
    } else if spec.group_keys.is_empty() {
        model.planner.gpu_reduce_min_rows
    } else {
        model.planner.gpu_hash_agg_min_rows
    };
    let estimated_fact_rows = Rows::new(rows(fact_rows));
    let gate = if estimated_fact_rows < required_fact_rows {
        if h3_resolution.is_some() {
            ShapeCostGate::H3RowsBelowDeviceMinimum {
                estimated: estimated_fact_rows,
                required: required_fact_rows,
            }
        } else {
            ShapeCostGate::FactRowsBelowDeviceMinimum {
                estimated: estimated_fact_rows,
                required: required_fact_rows,
            }
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
        let gate_group_rows = h3_group_rows_bound.unwrap_or(input.estimated_output_rows);
        let estimated_groups = Rows::new(rows(gate_group_rows));
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
