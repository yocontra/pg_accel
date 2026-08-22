//! DeviceLimits-derived costing for neutral aggregate shapes.

use crate::engine::cost::{GPU_LAUNCH_OVERHEAD, PgCost, Rows, TypedCostModel, WorkProduct};
use crate::engine::spec::{
    AggQuerySpec, AggregateKind, AggregateOutput, AggregateSource, BinaryMeasureOp, ColumnRef,
    FilterSpec, GroupKeySource, JoinMultiplicity, MaskKind, MeasureExpr, ScalarValue,
    SpatialOperand, SpatialPredicateKind, SpatialValueKind,
};

use super::{DescriptorGroupingMode, DescriptorResolution, ResidencyEstimate, ShapeInput};

/// Named cost components. No component is adjusted to undercut PostgreSQL.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeCost {
    pub fact_scan: PgCost,
    pub dimension_setup: PgCost,
    pub join_probe: PgCost,
    pub spatial_filter: PgCost,
    pub spatial_recheck_reserve: PgCost,
    pub aggregate: PgCost,
    /// Launch overhead beyond the one base dispatch charged by path costing.
    pub additional_aggregate_launches: PgCost,
    pub output_materialization: PgCost,
    pub amortized_auto_load: PgCost,
    pub total: PgCost,
}

impl ShapeCost {
    /// Replace the first-use load component and recompute the complete total.
    ///
    /// Exact residency resolution uses this after shape extraction. Keeping
    /// the component sum here prevents spatial operation and exact-recheck
    /// reserves from disappearing when only residency evidence changes.
    pub(crate) fn replace_amortized_auto_load(&mut self, load: PgCost) {
        self.amortized_auto_load = load;
        self.recompute_total();
    }

    pub(crate) fn replace_aggregate(&mut self, aggregate: PgCost) {
        self.aggregate = aggregate;
        self.recompute_total();
    }

    pub(crate) fn replace_additional_aggregate_launches(&mut self, launches: PgCost) {
        self.additional_aggregate_launches = launches;
        self.recompute_total();
    }

    fn recompute_total(&mut self) {
        self.total = PgCost::new(
            self.fact_scan.get()
                + self.dimension_setup.get()
                + self.join_probe.get()
                + self.spatial_filter.get()
                + self.spatial_recheck_reserve.get()
                + self.aggregate.get()
                + self.additional_aggregate_launches.get()
                + self.output_materialization.get()
                + self.amortized_auto_load.get(),
        );
    }
}

const SUM_OUTPUT: AggregateOutput = AggregateOutput {
    source: AggregateSource::Value,
    kind: AggregateKind::Sum,
};
const COUNT_OUTPUT: AggregateOutput = AggregateOutput {
    source: AggregateSource::Value,
    kind: AggregateKind::Count,
};

fn exact_bounded_int4_measure_filter(measure: &crate::engine::spec::MeasureSpec) -> bool {
    let MeasureExpr::Column(value) = &measure.expression else {
        return false;
    };
    let FilterSpec::Ranges { input, ranges } = &measure.filter else {
        return false;
    };
    let [range] = ranges.as_slice() else {
        return false;
    };
    let (ScalarValue::I32(lo), ScalarValue::I32(hi)) = (range.lo, range.hi) else {
        return false;
    };
    input == value
        && value.type_oid == 23
        && lo != i32::MIN
        && hi != i32::MAX
        && lo < hi
        && measure.outputs.as_slice() == [SUM_OUTPUT]
}

fn dense_descriptor_structure(spec: &AggQuerySpec, resolution: &DescriptorResolution) -> bool {
    let grouping_is_dense = match resolution {
        DescriptorResolution::Ready => spec.group_keys.is_empty(),
        DescriptorResolution::BeginTimeArtifacts {
            dictionary_keys,
            derived_keys,
            grouping_mode: DescriptorGroupingMode::DenseDictionary,
            max_group_count,
            ..
        } => {
            !spec.group_keys.is_empty()
                && dictionary_keys.len() == spec.group_keys.len()
                && derived_keys.is_empty()
                && *max_group_count > 0
        }
        DescriptorResolution::BeginTimeArtifacts { .. } => false,
    };
    grouping_is_dense
        && spec.group_keys.iter().all(|key| {
            matches!(
                key.source,
                GroupKeySource::FactColumn(_) | GroupKeySource::StarDimension { .. }
            )
        })
        && spec
            .star_dims
            .iter()
            .all(|dimension| dimension.multiplicity == JoinMultiplicity::Unique)
}

fn dense_atomic_sum_count_per_row_cost(
    spec: &AggQuerySpec,
    fact_columns_have_no_null_sidecars: bool,
    model: &TypedCostModel,
) -> Option<PgCost> {
    if !matches!(
        spec.fact_filter,
        FilterSpec::None
            | FilterSpec::Mask {
                kind: MaskKind::Sql,
                ..
            }
    ) || spec.measures.len() != 2
    {
        return None;
    }
    let value = &spec.measures[0];
    let count = &spec.measures[1];
    let measure_filtered = exact_bounded_int4_measure_filter(value);
    if (value.filter != FilterSpec::None && !measure_filtered) || count.filter != FilterSpec::None {
        return None;
    }
    if measure_filtered {
        let [key] = spec.group_keys.as_slice() else {
            return None;
        };
        let GroupKeySource::FactColumn(group_column) = &key.source else {
            return None;
        };
        let MeasureExpr::Column(value_column) = &value.expression else {
            return None;
        };
        if !spec.star_dims.is_empty()
            || spec.fact_filter != FilterSpec::None
            || key.type_oid != 23
            || group_column.type_oid != 23
            || group_column == value_column
        {
            return None;
        }
    }
    if value.outputs.as_slice() != [SUM_OUTPUT]
        || count.expression != MeasureExpr::CountStar
        || count.outputs.as_slice() != [COUNT_OUTPUT]
    {
        return None;
    }
    const INT4OID: u32 = 23;
    match &value.expression {
        MeasureExpr::Column(column) if column.type_oid == INT4OID => {
            let state_updates = if measure_filtered {
                // COUNT(*) covers every WHERE-qualified row, while the
                // filtered SUM needs independent SUM and non-NULL state.
                3.0
            } else if fact_columns_have_no_null_sidecars {
                2.0
            } else {
                // A nullable SUM lane maintains a third atomic nonnull state.
                3.0
            };
            let predicate = if measure_filtered {
                model.coefficients.gpu_op_cost_filter.get() * 2.0
            } else {
                0.0
            };
            Some(PgCost::new(
                model
                    .coefficients
                    .gpu_op_cost_reduce
                    .get()
                    .mul_add(state_updates, predicate),
            ))
        }
        MeasureExpr::Binary {
            op: BinaryMeasureOp::Mul,
            lhs,
            rhs,
        } if lhs.type_oid == INT4OID
            && rhs.type_oid == INT4OID
            && fact_columns_have_no_null_sidecars =>
        {
            // The fused kernel still evaluates one integer multiplication per
            // qualifying row in addition to the SUM and COUNT state updates.
            Some(PgCost::new(
                model
                    .coefficients
                    .gpu_op_cost_reduce
                    .get()
                    .mul_add(2.0, model.coefficients.gpu_op_cost_filter.get()),
            ))
        }
        _ => None,
    }
}

pub fn dense_atomic_fact_row_floor(spec: &AggQuerySpec, model: &TypedCostModel) -> Rows {
    if !spec.star_dims.is_empty() {
        model.planner.gpu_preagg_min_fact_rows
    } else if spec.group_keys.is_empty() {
        model.planner.gpu_reduce_min_rows
    } else {
        model.planner.gpu_hash_agg_min_rows
    }
}

/// Whether this descriptor has the dense integer SUM/COUNT lifecycle that may
/// execute as one shot or as bounded accumulate calls plus finalize.
///
/// This deliberately excludes count-only and H3 descriptors: their measured
/// large-row paths use different execution strategies.
pub fn dense_atomic_sum_count_lifecycle(
    spec: &AggQuerySpec,
    resolution: &DescriptorResolution,
    model: &TypedCostModel,
) -> bool {
    dense_descriptor_structure(spec, resolution)
        && dense_atomic_sum_count_per_row_cost(spec, true, model).is_some()
}

/// Return operation-count aggregate cost when exact resident evidence proves
/// the dense integer SUM/COUNT lifecycle. Launch overhead is accounted as a
/// separate named cost component by the caller.
pub fn dense_atomic_sum_count_cost(
    spec: &AggQuerySpec,
    resolution: &DescriptorResolution,
    estimated_fact_rows: u64,
    resident_fact_rows: Option<u64>,
    fact_columns_have_no_null_sidecars: Option<bool>,
    model: &TypedCostModel,
) -> Option<PgCost> {
    let resident_fact_rows_u64 = resident_fact_rows?;
    if estimated_fact_rows < resident_fact_rows_u64 {
        return None;
    }
    let resident_fact_rows = usize::try_from(resident_fact_rows_u64).ok()?;
    let exact_rows = Rows::new(resident_fact_rows);
    if exact_rows < dense_atomic_fact_row_floor(spec, model)
        || !dense_atomic_sum_count_lifecycle(spec, resolution, model)
    {
        return None;
    }
    let per_row =
        dense_atomic_sum_count_per_row_cost(spec, fact_columns_have_no_null_sidecars?, model)?;
    Some(PgCost::new(resident_fact_rows_u64 as f64 * per_row.get()))
}

/// Device-derived capability gate, separate from cost comparison with a
/// native PostgreSQL path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeCostGate {
    Eligible,
    FactRowsBelowDeviceMinimum {
        estimated: Rows,
        required: Rows,
    },
    DenseOneShotRowsExceedDeviceMaximum {
        fact_rows: Rows,
        maximum: Rows,
    },
    H3RowsBelowDeviceMinimum {
        estimated: Rows,
        required: Rows,
    },
    SpatialRowsBelowDeviceMinimum {
        estimated: Rows,
        required: Rows,
    },
    SpatialVerticesBelowDeviceMinimum {
        estimated: Rows,
        required: Rows,
    },
    SpatialVerticesExceedDeviceMaximum {
        estimated: Rows,
        maximum: Rows,
    },
    SpatialWorkBelowDeviceMinimum {
        estimated: WorkProduct,
        required: WorkProduct,
    },
    SpatialWorkExceedsDeviceMaximum {
        estimated: WorkProduct,
        maximum: WorkProduct,
    },
    DimensionRowsExceedDeviceMaximum {
        estimated: Rows,
        maximum: Rows,
    },
    GroupsExceedDeviceMaximum {
        estimated: Rows,
        maximum: Rows,
    },
}

fn rows(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn mul_cost(count: u64, coefficient: PgCost) -> f64 {
    count as f64 * coefficient.get()
}

fn measure_uses_fp64_arithmetic(measure: &crate::engine::spec::MeasureSpec) -> bool {
    if measure.outputs.as_slice() == [COUNT_OUTPUT]
        && matches!(measure.expression, MeasureExpr::Column(_))
    {
        return false;
    }
    const FLOAT8OID: u32 = 701;
    let is_fp64 = |column: &ColumnRef| column.type_oid == FLOAT8OID;
    match &measure.expression {
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

/// Number of synchronous native calls in the dense aggregate lifecycle.
///
/// A descriptor at or below the independently proved one-shot boundary uses
/// one RESET|ACCUMULATE|FINALIZE call. A larger descriptor uses one ACCUMULATE
/// call per bounded row chunk followed by one FINALIZE call. `None` is a
/// fail-closed signal for a zero chunk limit or arithmetic overflow.
pub fn dense_lifecycle_call_count(
    fact_rows: u64,
    one_shot_max_rows: Rows,
    max_chunk_rows: Rows,
) -> Option<u64> {
    if fact_rows <= u64::try_from(one_shot_max_rows.get()).ok()? {
        return Some(1);
    }
    let chunk_rows = u64::try_from(max_chunk_rows.get())
        .ok()
        .filter(|rows| *rows > 0)?;
    fact_rows
        .checked_sub(1)?
        .checked_div(chunk_rows)?
        .checked_add(1)?
        .checked_add(1)
}

fn has_spatial_filter(spec: &AggQuerySpec) -> bool {
    matches!(spec.fact_filter, FilterSpec::Spatial { .. })
}

fn geometry_type_from_typmod(typmod: i32) -> Option<u32> {
    (typmod >= 0 && typmod.trailing_zeros() >= 2)
        .then(|| u32::try_from((typmod & 0xfc) >> 2).ok())
        .flatten()
}

fn is_point_column(operand: &SpatialOperand) -> bool {
    matches!(
        operand,
        SpatialOperand::Column { metadata, .. }
            if metadata.kind == SpatialValueKind::Geometry
                && metadata.srid.is_some()
                && geometry_type_from_typmod(metadata.typmod)
                    == Some(crate::engine::residency::RESIDENT_GEOMETRY_POINT)
    )
}

fn constant_polygon_vertices(operand: &SpatialOperand) -> Option<usize> {
    let SpatialOperand::Constant { metadata, bytes } = operand else {
        return None;
    };
    if metadata.kind != SpatialValueKind::Geometry || metadata.srid.is_none() {
        return None;
    }
    let parsed = crate::engine::residency::validate_resident_geometry_value(
        bytes,
        crate::engine::spec::MAX_SPATIAL_CONSTANT_BYTES,
    )
    .ok()?;
    (parsed.geom_type == crate::engine::residency::RESIDENT_GEOMETRY_POLYGON
        && parsed.srid == metadata.srid?
        && parsed.coordinate_pairs > 0)
        .then_some(parsed.coordinate_pairs)
}

/// Return the exact constant polygon size for the currently proved resident
/// lane. Column-by-column spatial work stays declined until catalog statistics
/// can prove a conservative per-row vertex bound.
pub(super) fn spatial_point_polygon_vertices(filter: &FilterSpec) -> Option<usize> {
    let FilterSpec::Spatial {
        predicate,
        left,
        right,
        ..
    } = filter
    else {
        return None;
    };
    match predicate {
        SpatialPredicateKind::Intersects | SpatialPredicateKind::DWithin => {
            if is_point_column(left) {
                constant_polygon_vertices(right)
            } else if is_point_column(right) {
                constant_polygon_vertices(left)
            } else {
                None
            }
        }
        SpatialPredicateKind::Contains => {
            constant_polygon_vertices(left).filter(|_| is_point_column(right))
        }
        SpatialPredicateKind::Within => is_point_column(left)
            .then(|| constant_polygon_vertices(right))
            .flatten(),
        SpatialPredicateKind::Disjoint
        | SpatialPredicateKind::Equals
        | SpatialPredicateKind::Touches
        | SpatialPredicateKind::Crosses
        | SpatialPredicateKind::Overlaps => None,
    }
}

/// Cost one normalized shape exclusively from typed device coefficients.
#[must_use]
pub fn estimate_shape_cost(
    input: &ShapeInput,
    spec: &AggQuerySpec,
    residency: &ResidencyEstimate,
    model: &TypedCostModel,
) -> (ShapeCost, ShapeCostGate, u64) {
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
        && spec.measures.iter().any(measure_uses_fp64_arithmetic)
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
    let has_spatial = has_spatial_filter(spec);
    let spatial_vertices = spatial_point_polygon_vertices(&spec.fact_filter);
    let spatial_fp64_multiplier = if has_spatial && !model.hardware.has_native_fp64 {
        model.coefficients.soft_fp64_cost_multiplier
    } else {
        1.0
    };
    let spatial_filter = if has_spatial {
        let vertex_scale = spatial_vertices.map_or(1.0, |vertices| {
            (vertices as f64 / model.planner.gpu_spatial_min_vertices.max(1) as f64).max(1.0)
        });
        let operation = mul_cost(fact_rows, model.coefficients.gpu_op_cost_filter)
            * vertex_scale
            * spatial_fp64_multiplier;
        let launches =
            h3_transform_chunks(fact_rows, model.executor.gpu_spatial_pairwise_chunk_rows) as f64
                * GPU_LAUNCH_OVERHEAD;
        operation + launches
    } else {
        0.0
    };
    // The exact uncertainty count is unavailable before execution. Reserve
    // the configured maximum at the separate CPU PostGIS coefficient so a
    // spatial path is never priced as bbox-only or as a second cheap GPU op.
    let spatial_recheck_reserve = if has_spatial {
        mul_cost(fact_rows, model.coefficients.cpu_spatial_recheck_per_row)
            * model.planner.gpu_spatial_max_recheck_fraction
    } else {
        0.0
    };
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
    let mut cost = ShapeCost {
        fact_scan: PgCost::new(fact_scan),
        dimension_setup: PgCost::new(dimension_setup),
        join_probe: PgCost::new(join_probe),
        spatial_filter: PgCost::new(spatial_filter),
        spatial_recheck_reserve: PgCost::new(spatial_recheck_reserve),
        aggregate: PgCost::new(aggregate),
        additional_aggregate_launches: PgCost::ZERO,
        output_materialization: PgCost::new(output_materialization),
        amortized_auto_load: PgCost::new(amortized_auto_load),
        total: PgCost::ZERO,
    };
    cost.replace_amortized_auto_load(PgCost::new(amortized_auto_load));

    let required_fact_rows = if has_spatial {
        model.planner.gpu_reduce_min_rows
    } else if h3_resolution.is_some() {
        model.planner.gpu_h3_group_min_rows
    } else if !spec.star_dims.is_empty() {
        model.planner.gpu_preagg_min_fact_rows
    } else if spec.group_keys.is_empty() {
        model.planner.gpu_reduce_min_rows
    } else {
        model.planner.gpu_hash_agg_min_rows
    };
    let estimated_fact_rows = Rows::new(rows(fact_rows));
    debug_assert!(!has_spatial || spatial_vertices.is_some());
    let estimated_spatial_vertices = Rows::new(spatial_vertices.unwrap_or(0));
    let spatial_work = WorkProduct::new(
        fact_rows.saturating_mul(
            spatial_vertices
                .and_then(|vertices| u64::try_from(vertices).ok())
                .unwrap_or(0),
        ),
    );
    let gate = if estimated_fact_rows < required_fact_rows {
        if has_spatial {
            ShapeCostGate::SpatialRowsBelowDeviceMinimum {
                estimated: estimated_fact_rows,
                required: required_fact_rows,
            }
        } else if h3_resolution.is_some() {
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
    } else if has_spatial
        && estimated_spatial_vertices < Rows::new(model.planner.gpu_spatial_min_vertices)
    {
        ShapeCostGate::SpatialVerticesBelowDeviceMinimum {
            estimated: estimated_spatial_vertices,
            required: Rows::new(model.planner.gpu_spatial_min_vertices),
        }
    } else if has_spatial
        && estimated_spatial_vertices > model.memory.gpu_spatial_max_vertices_per_row
    {
        ShapeCostGate::SpatialVerticesExceedDeviceMaximum {
            estimated: estimated_spatial_vertices,
            maximum: model.memory.gpu_spatial_max_vertices_per_row,
        }
    } else if has_spatial
        && spatial_work < model.planner.spatial_point_in_ring_break_even_verts_x_rows
    {
        ShapeCostGate::SpatialWorkBelowDeviceMinimum {
            estimated: spatial_work,
            required: model.planner.spatial_point_in_ring_break_even_verts_x_rows,
        }
    } else if has_spatial
        && spatial_work > model.kernel_health.spatial_point_in_ring_max_verts_x_rows
    {
        ShapeCostGate::SpatialWorkExceedsDeviceMaximum {
            estimated: spatial_work,
            maximum: model.kernel_health.spatial_point_in_ring_max_verts_x_rows,
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

    (cost, gate, estimated_output_rows)
}
