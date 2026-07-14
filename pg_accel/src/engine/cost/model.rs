//! Typed cost-model views over the existing flat device limits.

use std::num::NonZeroU32;

use super::device_limits::DeviceLimits;
use super::units::{Bytes, PgCost, Rows, WorkProduct};

/// Cost-model snapshot grouped by concern.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedCostModel {
    /// Hardware-derived capability gates retained in [`DeviceLimits`].
    pub hardware: HardwareCaps,
    /// Memory sizing and capacity limits.
    pub memory: MemoryModel,
    /// Per-row and per-operation cost coefficients.
    pub coefficients: CostCoefficients,
    /// Planner thresholds and admission policy.
    pub planner: PlannerPolicy,
    /// Executor chunking and scheduling limits.
    pub executor: ExecutorLimits,
    /// Temporary kernel safety gates and known-health envelopes.
    pub kernel_health: KernelHealthRegistry,
}

impl TypedCostModel {
    /// Build a typed view from the current flat limits without changing any
    /// scalar values.
    #[must_use]
    pub fn from_limits(limits: &DeviceLimits) -> Self {
        Self::from(limits)
    }
}

impl From<&DeviceLimits> for TypedCostModel {
    fn from(limits: &DeviceLimits) -> Self {
        Self {
            hardware: HardwareCaps::from(limits),
            memory: MemoryModel::from(limits),
            coefficients: CostCoefficients::from(limits),
            planner: PlannerPolicy::from(limits),
            executor: ExecutorLimits::from(limits),
            kernel_health: KernelHealthRegistry::from(limits),
        }
    }
}

impl From<DeviceLimits> for TypedCostModel {
    fn from(limits: DeviceLimits) -> Self {
        Self::from(&limits)
    }
}

/// Hardware capability flags retained by the active cost model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardwareCaps {
    /// Whether the selected device has native fp64 arithmetic.
    pub has_native_fp64: bool,
}

impl From<&DeviceLimits> for HardwareCaps {
    fn from(limits: &DeviceLimits) -> Self {
        Self {
            has_native_fp64: limits.has_native_fp64,
        }
    }
}

/// Memory sizing and state-capacity limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryModel {
    /// Maximum groups admitted for GPU hash aggregation.
    pub gpu_hash_agg_max_groups: Rows,
    /// Maximum dimension rows per dimension for PreAgg.
    pub gpu_preagg_max_dim_rows: Rows,
    /// Maximum state bytes per group for hash aggregation.
    pub hashagg_max_state_bytes_per_group: Bytes,
    /// Maximum coordinate pairs admitted for one resident spatial row.
    pub gpu_spatial_max_vertices_per_row: Rows,
}

impl From<&DeviceLimits> for MemoryModel {
    fn from(limits: &DeviceLimits) -> Self {
        Self {
            gpu_hash_agg_max_groups: Rows::new(limits.gpu_hash_agg_max_groups),
            gpu_preagg_max_dim_rows: Rows::new(limits.gpu_preagg_max_dim_rows),
            hashagg_max_state_bytes_per_group: Bytes::new(limits.hashagg_max_state_bytes_per_group),
            gpu_spatial_max_vertices_per_row: Rows::new(limits.gpu_spatial_max_vertices_per_row),
        }
    }
}

/// Per-row and per-operation cost coefficients.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct CostCoefficients {
    /// Per-row dimension materialization cost for PreAgg.
    pub preagg_dim_materialize_cost: PgCost,
    /// Per-row fact-table scan cost for PreAgg.
    pub preagg_fact_scan_cost: PgCost,
    /// Per-row hash-probe cost for PreAgg.
    pub preagg_probe_cost: PgCost,
    /// Per-row aggregate accumulation cost for PreAgg.
    pub preagg_agg_cost: PgCost,
    /// Per-row result-yield cost for PreAgg.
    pub preagg_yield_cost: PgCost,
    /// GPU reduce per-row operation cost.
    pub gpu_op_cost_reduce: PgCost,
    /// GPU hash-aggregation per-row operation cost.
    pub gpu_op_cost_hash_agg: PgCost,
    /// GPU sort per-row operation cost.
    pub gpu_op_cost_sort: PgCost,
    /// GPU window per-row operation cost.
    pub gpu_op_cost_window: PgCost,
    /// GPU filter per-row operation cost.
    pub gpu_op_cost_filter: PgCost,
    /// CPU PostGIS exact-recheck cost per uncertain row.
    pub cpu_spatial_recheck_per_row: PgCost,
    /// Resident H3 cell-to-parent device transform per-row cost.
    pub gpu_op_cost_h3_parent_resident: PgCost,
    /// GPU hash-join build-side per-row cost.
    pub gpu_hashjoin_build_per_row: PgCost,
    /// GPU hash-join probe-side per-row cost.
    pub gpu_hashjoin_probe_per_row: PgCost,
    /// Custom Scan per-output-row yield cost.
    pub custom_scan_yield_per_row: PgCost,
    /// GPU partial-aggregate per-row cost.
    pub gpu_partial_agg_per_row: PgCost,
    /// Multiplier applied to fp64 GPU op costs on devices without native fp64.
    pub soft_fp64_cost_multiplier: f64,
}

impl From<&DeviceLimits> for CostCoefficients {
    fn from(limits: &DeviceLimits) -> Self {
        Self {
            preagg_dim_materialize_cost: PgCost::new(limits.preagg_dim_materialize_cost),
            preagg_fact_scan_cost: PgCost::new(limits.preagg_fact_scan_cost),
            preagg_probe_cost: PgCost::new(limits.preagg_probe_cost),
            preagg_agg_cost: PgCost::new(limits.preagg_agg_cost),
            preagg_yield_cost: PgCost::new(limits.preagg_yield_cost),
            gpu_op_cost_reduce: PgCost::new(limits.gpu_op_cost_reduce),
            gpu_op_cost_hash_agg: PgCost::new(limits.gpu_op_cost_hash_agg),
            gpu_op_cost_sort: PgCost::new(limits.gpu_op_cost_sort),
            gpu_op_cost_window: PgCost::new(limits.gpu_op_cost_window),
            gpu_op_cost_filter: PgCost::new(limits.gpu_op_cost_filter),
            cpu_spatial_recheck_per_row: PgCost::new(limits.cpu_spatial_recheck_per_row),
            gpu_op_cost_h3_parent_resident: PgCost::new(limits.gpu_op_cost_h3_parent_resident),
            gpu_hashjoin_build_per_row: PgCost::new(limits.gpu_hashjoin_build_per_row),
            gpu_hashjoin_probe_per_row: PgCost::new(limits.gpu_hashjoin_probe_per_row),
            custom_scan_yield_per_row: PgCost::new(limits.custom_scan_yield_per_row),
            gpu_partial_agg_per_row: PgCost::new(limits.gpu_partial_agg_per_row),
            soft_fp64_cost_multiplier: limits.soft_fp64_cost_multiplier,
        }
    }
}

/// Planner thresholds and admission policy.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct PlannerPolicy {
    /// Expected statement-local reuse count used to amortize synchronous
    /// first-use resident loads. Zero-valued flat configuration is clamped to
    /// one at this typed boundary.
    pub auto_load_amortization_queries: NonZeroU32,
    /// Minimum rows before generic GPU dispatch is considered.
    pub gpu_min_rows: Rows,
    /// Minimum rows for GPU sort at executor level.
    pub gpu_sort_min_rows: Rows,
    /// Minimum rows for GPU sort at planner level.
    pub gpu_sort_planner_min_rows: Rows,
    /// Minimum rows for GPU window functions.
    pub gpu_window_min_rows: Rows,
    /// Minimum rows for GPU reduce / aggregate.
    pub gpu_reduce_min_rows: Rows,
    /// Minimum rows for grouped GPU hash aggregation.
    pub gpu_hash_agg_min_rows: Rows,
    /// Minimum rows for resident H3 parent grouped aggregation.
    pub gpu_h3_group_min_rows: Rows,
    /// Minimum constant polygon vertex count for GPU spatial dispatch.
    pub gpu_spatial_min_vertices: usize,
    /// Maximum estimated output fraction for heap-backed GPU spatial scans.
    pub gpu_spatial_max_output_fraction: f64,
    /// Maximum fraction reserved for exact spatial rechecks.
    pub gpu_spatial_max_recheck_fraction: f64,
    /// Minimum rows for GPU expression scan dispatch.
    pub gpu_expr_min_rows: Rows,
    /// Minimum rows for pipeline fusion.
    pub gpu_pipeline_fusion_min_rows: Rows,
    /// Minimum fact table rows for PreAgg.
    pub gpu_preagg_min_fact_rows: Rows,
    /// Maximum ratio for GpuAgg injection.
    pub gpu_agg_cost_ratio: f64,
    /// Maximum ratio for GpuWindow injection.
    pub gpu_window_cost_ratio: f64,
    /// Maximum ratio for PreAgg injection.
    pub gpu_preagg_cost_ratio: f64,
    /// Minimum rows for fp32 reduce.
    pub reduce_f32_break_even_rows: Rows,
    /// Minimum rows for fp64 reduce.
    pub reduce_f64_break_even_rows: Rows,
    /// Minimum rows for i64 reduce.
    pub reduce_i64_break_even_rows: Rows,
    /// Minimum average rows per group for GPU hash aggregation.
    pub hashagg_min_rows_per_group: Rows,
    /// Minimum rows for integer-key GPU sort.
    pub sort_break_even_rows_int: Rows,
    /// Minimum rows for floating-point-key GPU sort.
    pub sort_break_even_rows_float: Rows,
    /// Minimum vertex-by-row work product for point-in-ring dispatch.
    pub spatial_point_in_ring_break_even_verts_x_rows: WorkProduct,
    /// Minimum rows per window partition.
    pub window_min_partition_rows: Rows,
    /// Minimum instruction-by-row work product for GpuExpr dispatch.
    pub expr_min_predicate_complexity_x_rows: WorkProduct,
    /// Minimum inner build-side rows for GPU hash join.
    pub hashjoin_min_build_rows: Rows,
}

impl From<&DeviceLimits> for PlannerPolicy {
    fn from(limits: &DeviceLimits) -> Self {
        Self {
            auto_load_amortization_queries: NonZeroU32::new(limits.auto_load_amortization_queries)
                .unwrap_or(NonZeroU32::MIN),
            gpu_min_rows: Rows::new(limits.gpu_min_rows),
            gpu_sort_min_rows: Rows::new(limits.gpu_sort_min_rows),
            gpu_sort_planner_min_rows: Rows::new(limits.gpu_sort_planner_min_rows),
            gpu_window_min_rows: Rows::new(limits.gpu_window_min_rows),
            gpu_reduce_min_rows: Rows::new(limits.gpu_reduce_min_rows),
            gpu_hash_agg_min_rows: Rows::new(limits.gpu_hash_agg_min_rows),
            gpu_h3_group_min_rows: Rows::new(limits.gpu_h3_group_min_rows),
            gpu_spatial_min_vertices: limits.gpu_spatial_min_vertices,
            gpu_spatial_max_output_fraction: limits.gpu_spatial_max_output_fraction,
            gpu_spatial_max_recheck_fraction: limits.gpu_spatial_max_recheck_fraction,
            gpu_expr_min_rows: Rows::new(limits.gpu_expr_min_rows),
            gpu_pipeline_fusion_min_rows: Rows::new(limits.gpu_pipeline_fusion_min_rows),
            gpu_preagg_min_fact_rows: Rows::new(limits.gpu_preagg_min_fact_rows),
            gpu_agg_cost_ratio: limits.gpu_agg_cost_ratio,
            gpu_window_cost_ratio: limits.gpu_window_cost_ratio,
            gpu_preagg_cost_ratio: limits.gpu_preagg_cost_ratio,
            reduce_f32_break_even_rows: Rows::new(limits.reduce_f32_break_even_rows),
            reduce_f64_break_even_rows: Rows::new(limits.reduce_f64_break_even_rows),
            reduce_i64_break_even_rows: Rows::new(limits.reduce_i64_break_even_rows),
            hashagg_min_rows_per_group: Rows::new(limits.hashagg_min_rows_per_group),
            sort_break_even_rows_int: Rows::new(limits.sort_break_even_rows_int),
            sort_break_even_rows_float: Rows::new(limits.sort_break_even_rows_float),
            spatial_point_in_ring_break_even_verts_x_rows: WorkProduct::new(
                limits.spatial_point_in_ring_break_even_verts_x_rows,
            ),
            window_min_partition_rows: Rows::new(limits.window_min_partition_rows),
            expr_min_predicate_complexity_x_rows: WorkProduct::new(
                limits.expr_min_predicate_complexity_x_rows,
            ),
            hashjoin_min_build_rows: Rows::new(limits.hashjoin_min_build_rows),
        }
    }
}

/// Executor chunking and scheduling limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExecutorLimits {
    /// Maximum elements per GPU reduce dispatch chunk.
    pub gpu_reduce_max_chunk: Rows,
    /// Maximum elements for GPU sort dispatch.
    pub gpu_sort_max_elements: Rows,
    /// Maximum resident H3 rows per device-to-device transform launch.
    pub gpu_h3_max_chunk_rows: Rows,
    /// Maximum resident spatial pairs per device launch.
    pub gpu_spatial_pairwise_chunk_rows: Rows,
    /// Lower bound for `optimal_batch_size`.
    pub optimal_batch_min: Rows,
    /// Upper bound for `optimal_batch_size`.
    pub optimal_batch_max: Rows,
    /// Row interval between interrupt checks in fused scan+agg.
    pub fused_interrupt_interval: Rows,
}

impl From<&DeviceLimits> for ExecutorLimits {
    fn from(limits: &DeviceLimits) -> Self {
        Self {
            gpu_reduce_max_chunk: Rows::new(limits.gpu_reduce_max_chunk),
            gpu_sort_max_elements: Rows::new(limits.gpu_sort_max_elements),
            gpu_h3_max_chunk_rows: Rows::new(limits.gpu_h3_max_chunk_rows),
            gpu_spatial_pairwise_chunk_rows: Rows::new(limits.gpu_spatial_pairwise_chunk_rows),
            optimal_batch_min: Rows::new(limits.optimal_batch_min),
            optimal_batch_max: Rows::new(limits.optimal_batch_max),
            fused_interrupt_interval: Rows::new(limits.fused_interrupt_interval),
        }
    }
}

/// Kernel safety gates and known-health envelopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct KernelHealthRegistry {
    /// First grouped-hashagg input size considered unsafe.
    pub gpu_hash_agg_unsafe_input_rows: Rows,
    /// Maximum output rows for GPU hash join injection.
    pub gpu_join_max_output_rows: Rows,
    /// Lower bound of the spatial polygon unsafe row band.
    pub gpu_spatial_unsafe_band_min_rows: Rows,
    /// Upper bound of the spatial polygon unsafe row band.
    pub gpu_spatial_unsafe_band_max_rows: Rows,
    /// Minimum polygon vertex count for the spatial unsafe row band.
    pub gpu_spatial_unsafe_band_min_vertices: usize,
    /// Maximum inner-side rows for GPU hash join build phase.
    pub gpu_hash_join_build_max_rows: Rows,
    /// Maximum vertex-by-row point-in-ring work product.
    pub spatial_point_in_ring_max_verts_x_rows: WorkProduct,
}

impl From<&DeviceLimits> for KernelHealthRegistry {
    fn from(limits: &DeviceLimits) -> Self {
        Self {
            gpu_hash_agg_unsafe_input_rows: Rows::new(limits.gpu_hash_agg_unsafe_input_rows),
            gpu_join_max_output_rows: Rows::new(limits.gpu_join_max_output_rows),
            gpu_spatial_unsafe_band_min_rows: Rows::new(limits.gpu_spatial_unsafe_band_min_rows),
            gpu_spatial_unsafe_band_max_rows: Rows::new(limits.gpu_spatial_unsafe_band_max_rows),
            gpu_spatial_unsafe_band_min_vertices: limits.gpu_spatial_unsafe_band_min_vertices,
            gpu_hash_join_build_max_rows: Rows::new(limits.gpu_hash_join_build_max_rows),
            spatial_point_in_ring_max_verts_x_rows: WorkProduct::new(
                limits.spatial_point_in_ring_max_verts_x_rows,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_model_preserves_cpu_only_device_limits() {
        let limits = DeviceLimits::cpu_only();
        let model = TypedCostModel::from(&limits);

        assert_eq!(model.hardware.has_native_fp64, limits.has_native_fp64);

        assert_eq!(
            model.memory.gpu_hash_agg_max_groups.get(),
            limits.gpu_hash_agg_max_groups,
        );
        assert_eq!(
            model.memory.gpu_preagg_max_dim_rows.get(),
            limits.gpu_preagg_max_dim_rows,
        );
        assert_eq!(
            model.memory.hashagg_max_state_bytes_per_group.get(),
            limits.hashagg_max_state_bytes_per_group,
        );
        assert_eq!(
            model.memory.gpu_spatial_max_vertices_per_row.get(),
            limits.gpu_spatial_max_vertices_per_row,
        );

        assert_eq!(
            model.coefficients.preagg_dim_materialize_cost.get(),
            limits.preagg_dim_materialize_cost,
        );
        assert_eq!(
            model.coefficients.preagg_fact_scan_cost.get(),
            limits.preagg_fact_scan_cost,
        );
        assert_eq!(
            model.coefficients.preagg_probe_cost.get(),
            limits.preagg_probe_cost,
        );
        assert_eq!(
            model.coefficients.preagg_agg_cost.get(),
            limits.preagg_agg_cost,
        );
        assert_eq!(
            model.coefficients.preagg_yield_cost.get(),
            limits.preagg_yield_cost,
        );
        assert_eq!(
            model.coefficients.gpu_op_cost_reduce.get(),
            limits.gpu_op_cost_reduce,
        );
        assert_eq!(
            model.coefficients.gpu_op_cost_hash_agg.get(),
            limits.gpu_op_cost_hash_agg,
        );
        assert_eq!(
            model.coefficients.gpu_op_cost_sort.get(),
            limits.gpu_op_cost_sort,
        );
        assert_eq!(
            model.coefficients.gpu_op_cost_window.get(),
            limits.gpu_op_cost_window,
        );
        assert_eq!(
            model.coefficients.gpu_op_cost_filter.get(),
            limits.gpu_op_cost_filter,
        );
        assert_eq!(
            model.coefficients.cpu_spatial_recheck_per_row.get(),
            limits.cpu_spatial_recheck_per_row,
        );
        assert_eq!(
            model.coefficients.gpu_op_cost_h3_parent_resident.get(),
            limits.gpu_op_cost_h3_parent_resident,
        );
        assert_eq!(
            model.coefficients.gpu_hashjoin_build_per_row.get(),
            limits.gpu_hashjoin_build_per_row,
        );
        assert_eq!(
            model.coefficients.gpu_hashjoin_probe_per_row.get(),
            limits.gpu_hashjoin_probe_per_row,
        );
        assert_eq!(
            model.coefficients.custom_scan_yield_per_row.get(),
            limits.custom_scan_yield_per_row,
        );
        assert_eq!(
            model.coefficients.gpu_partial_agg_per_row.get(),
            limits.gpu_partial_agg_per_row,
        );
        assert_eq!(
            model.coefficients.soft_fp64_cost_multiplier,
            limits.soft_fp64_cost_multiplier,
        );

        assert_eq!(model.planner.gpu_min_rows.get(), limits.gpu_min_rows);
        assert_eq!(
            model.planner.auto_load_amortization_queries.get(),
            limits.auto_load_amortization_queries.max(1),
        );
        assert_eq!(
            model.planner.gpu_sort_min_rows.get(),
            limits.gpu_sort_min_rows,
        );
        assert_eq!(
            model.planner.gpu_sort_planner_min_rows.get(),
            limits.gpu_sort_planner_min_rows,
        );
        assert_eq!(
            model.planner.gpu_window_min_rows.get(),
            limits.gpu_window_min_rows,
        );
        assert_eq!(
            model.planner.gpu_reduce_min_rows.get(),
            limits.gpu_reduce_min_rows,
        );
        assert_eq!(
            model.planner.gpu_hash_agg_min_rows.get(),
            limits.gpu_hash_agg_min_rows,
        );
        assert_eq!(
            model.planner.gpu_h3_group_min_rows.get(),
            limits.gpu_h3_group_min_rows,
        );
        assert_eq!(
            model.planner.gpu_spatial_min_vertices,
            limits.gpu_spatial_min_vertices,
        );
        assert_eq!(
            model.planner.gpu_spatial_max_output_fraction,
            limits.gpu_spatial_max_output_fraction,
        );
        assert_eq!(
            model.planner.gpu_spatial_max_recheck_fraction,
            limits.gpu_spatial_max_recheck_fraction,
        );
        assert_eq!(
            model.planner.gpu_expr_min_rows.get(),
            limits.gpu_expr_min_rows
        );
        assert_eq!(
            model.planner.gpu_pipeline_fusion_min_rows.get(),
            limits.gpu_pipeline_fusion_min_rows,
        );
        assert_eq!(
            model.planner.gpu_preagg_min_fact_rows.get(),
            limits.gpu_preagg_min_fact_rows,
        );
        assert_eq!(model.planner.gpu_agg_cost_ratio, limits.gpu_agg_cost_ratio);
        assert_eq!(
            model.planner.gpu_window_cost_ratio,
            limits.gpu_window_cost_ratio,
        );
        assert_eq!(
            model.planner.gpu_preagg_cost_ratio,
            limits.gpu_preagg_cost_ratio,
        );
        assert_eq!(
            model.planner.reduce_f32_break_even_rows.get(),
            limits.reduce_f32_break_even_rows,
        );
        assert_eq!(
            model.planner.reduce_f64_break_even_rows.get(),
            limits.reduce_f64_break_even_rows,
        );
        assert_eq!(
            model.planner.reduce_i64_break_even_rows.get(),
            limits.reduce_i64_break_even_rows,
        );
        assert_eq!(
            model.planner.hashagg_min_rows_per_group.get(),
            limits.hashagg_min_rows_per_group,
        );
        assert_eq!(
            model.planner.sort_break_even_rows_int.get(),
            limits.sort_break_even_rows_int,
        );
        assert_eq!(
            model.planner.sort_break_even_rows_float.get(),
            limits.sort_break_even_rows_float,
        );
        assert_eq!(
            model
                .planner
                .spatial_point_in_ring_break_even_verts_x_rows
                .get(),
            limits.spatial_point_in_ring_break_even_verts_x_rows,
        );
        assert_eq!(
            model.planner.window_min_partition_rows.get(),
            limits.window_min_partition_rows,
        );
        assert_eq!(
            model.planner.expr_min_predicate_complexity_x_rows.get(),
            limits.expr_min_predicate_complexity_x_rows,
        );
        assert_eq!(
            model.planner.hashjoin_min_build_rows.get(),
            limits.hashjoin_min_build_rows,
        );

        assert_eq!(
            model.executor.gpu_reduce_max_chunk.get(),
            limits.gpu_reduce_max_chunk,
        );
        assert_eq!(
            model.executor.gpu_sort_max_elements.get(),
            limits.gpu_sort_max_elements,
        );
        assert_eq!(
            model.executor.gpu_h3_max_chunk_rows.get(),
            limits.gpu_h3_max_chunk_rows,
        );
        assert_eq!(
            model.executor.gpu_spatial_pairwise_chunk_rows.get(),
            limits.gpu_spatial_pairwise_chunk_rows,
        );
        assert_eq!(
            model.executor.optimal_batch_min.get(),
            limits.optimal_batch_min,
        );
        assert_eq!(
            model.executor.optimal_batch_max.get(),
            limits.optimal_batch_max,
        );
        assert_eq!(
            model.executor.fused_interrupt_interval.get(),
            limits.fused_interrupt_interval,
        );

        assert_eq!(
            model.kernel_health.gpu_hash_agg_unsafe_input_rows.get(),
            limits.gpu_hash_agg_unsafe_input_rows,
        );
        assert_eq!(
            model.kernel_health.gpu_join_max_output_rows.get(),
            limits.gpu_join_max_output_rows,
        );
        assert_eq!(
            model.kernel_health.gpu_spatial_unsafe_band_min_rows.get(),
            limits.gpu_spatial_unsafe_band_min_rows,
        );
        assert_eq!(
            model.kernel_health.gpu_spatial_unsafe_band_max_rows.get(),
            limits.gpu_spatial_unsafe_band_max_rows,
        );
        assert_eq!(
            model.kernel_health.gpu_spatial_unsafe_band_min_vertices,
            limits.gpu_spatial_unsafe_band_min_vertices,
        );
        assert_eq!(
            model.kernel_health.gpu_hash_join_build_max_rows.get(),
            limits.gpu_hash_join_build_max_rows,
        );
        assert_eq!(
            model
                .kernel_health
                .spatial_point_in_ring_max_verts_x_rows
                .get(),
            limits.spatial_point_in_ring_max_verts_x_rows,
        );
    }

    #[test]
    fn owned_and_borrowed_device_limit_conversions_match() {
        let limits = DeviceLimits::cpu_only();
        assert_eq!(
            TypedCostModel::from(limits.clone()),
            TypedCostModel::from(&limits),
        );
    }

    #[test]
    fn section_conversions_are_available_independently() {
        let limits = DeviceLimits::cpu_only();

        assert_eq!(
            PlannerPolicy::from(&limits).gpu_min_rows,
            Rows::new(limits.gpu_min_rows),
        );
        assert_eq!(
            CostCoefficients::from(&limits).gpu_op_cost_reduce,
            PgCost::new(limits.gpu_op_cost_reduce),
        );
        assert_eq!(
            KernelHealthRegistry::from(&limits).gpu_hash_agg_unsafe_input_rows,
            Rows::new(limits.gpu_hash_agg_unsafe_input_rows),
        );
    }

    #[test]
    fn planner_policy_clamps_zero_auto_load_amortization_to_one() {
        let mut limits = DeviceLimits::cpu_only();
        limits.auto_load_amortization_queries = 0;

        assert_eq!(
            PlannerPolicy::from(&limits).auto_load_amortization_queries,
            NonZeroU32::MIN,
        );
    }
}
