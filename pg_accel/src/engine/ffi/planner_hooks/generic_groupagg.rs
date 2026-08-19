//! Generic descriptor aggregate admission and childless path construction.

use std::collections::BTreeMap;
use std::ffi::{c_int, c_void};
use std::num::NonZeroU32;
use std::rc::Rc;

use pgrx::pg_sys::{self, List, NodeTag, RelOptInfo};

use super::decline_cache::{self, DeclineCacheKey, DeclinePolicyKey, QueryFingerprint};
use super::shape::{
    DescriptorGroupingMode, DescriptorResolution, RelationResidency, RelationResidencyRequirement,
    ResidencyEstimate, ShapeCostGate, ShapeDecline, ShapePlan, dense_atomic_fact_row_floor,
    dense_atomic_sum_count_cost, dense_atomic_sum_count_lifecycle, dense_lifecycle_call_count,
};
use super::{PlannerSubstageGuard, add_gpu_path_with_resident_proof, rel_rows_estimate};
use crate::engine::cost::{self, PgCost, Rows, TypedCostModel};
use crate::engine::executor::agg::{
    estimate_descriptor_artifact_bytes_upper_bound, planned_descriptor_kernel_mode,
    validate_normal_descriptor_capability, validate_normal_spatial_candidate_capability,
};
use crate::engine::ffi::custom_scan;
use crate::engine::gucs;
use crate::engine::residency::{
    MaterializationBoundary, ResidentBudgetSnapshot, ResidentLoadEstimate, ResidentOperatorClass,
    ResidentOperatorStage, ResidentPipelineProof, ResidentPlannerDependency, SelectedRelation,
    estimate_selected_relation, resident_budget_snapshot, resident_planner_dependency,
    revalidate_loaded_estimates,
};
use crate::engine::spec::{AggOutputProjection, FilterSpec, GroupKeyEncoding, MeasureExpr};
use crate::engine::stats;
use crate::gpu::GroupedAggKernelMode;

const GENERIC_SHAPE_PATH_CONTEXT: &str = "upper_paths_generic_groupagg";
const AGG_QUERY_SPEC_SENTINEL: c_int = i32::from_be_bytes(*b"AQS3");
const AGG_OUTPUT_PROJECTION_SENTINEL: c_int = i32::from_be_bytes(*b"AOP2");

unsafe extern "C" {
    // PostgreSQL declares this in optimizer/prep.h and exports it from the
    // backend, but pgrx does not currently include it in pg_sys for PG18/19.
    fn get_agg_clause_costs(
        root: *mut pg_sys::PlannerInfo,
        aggsplit: pg_sys::AggSplit::Type,
        costs: *mut pg_sys::AggClauseCosts,
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AdmissionPolicy {
    auto_load: bool,
    budget_bytes: u64,
    budget_snapshot: ResidentBudgetSnapshot,
}

#[derive(Debug, Clone, PartialEq)]
enum AdmissionDecline {
    Shape(ShapeDecline),
    AttributeNumberOutOfRange {
        relation_oid: u32,
        attno: i32,
    },
    ResidencyEstimateFailed {
        relation_oid: u32,
        detail: String,
    },
    MissingResidencyEstimate {
        relation_oid: u32,
    },
    UnexpectedResidencyEstimate {
        relation_oid: u32,
    },
    DescriptorCapability {
        detail: String,
    },
    SerialGenericKernelMode {
        mode: &'static str,
    },
    AutoLoadDisabled {
        relation_oid: u32,
        estimated_bytes: u64,
    },
    ResidencyBytesOverflow,
    ResidencyBudgetSnapshotUnavailable,
    ResidencyEvidenceChanged,
    DerivedArtifactEstimateUnavailable,
    GroupEstimateUnavailable,
    ResidencyBudgetExceeded {
        cluster_live_bytes: u64,
        current_backend_live_bytes: u64,
        other_backend_live_bytes: u64,
        pinned_unselected_raw_bytes: u64,
        evictable_or_replaced_local_bytes: u64,
        selected_raw_bytes: u64,
        derived_artifact_bytes: u64,
        projected_final_bytes: u64,
        budget_bytes: u64,
    },
    DeviceCostGate(ShapeCostGate),
    NativePathUnavailable,
    CostNotCompetitive {
        gpu_cost: f64,
        native_cost: f64,
        required_cost: f64,
    },
    PathNotAdded,
}

impl AdmissionDecline {
    const fn code(&self) -> &'static str {
        match self {
            Self::Shape(decline) => decline.code(),
            Self::AttributeNumberOutOfRange { .. } => "generic_residency_attno_out_of_range",
            Self::ResidencyEstimateFailed { .. } => "generic_residency_estimate_failed",
            Self::MissingResidencyEstimate { .. } => "generic_residency_estimate_missing",
            Self::UnexpectedResidencyEstimate { .. } => "generic_residency_estimate_unexpected",
            Self::DescriptorCapability { .. } => "generic_descriptor_capability",
            Self::SerialGenericKernelMode { .. } => "generic_serial_kernel_mode_unqualified",
            Self::AutoLoadDisabled { .. } => "generic_auto_load_disabled",
            Self::ResidencyBytesOverflow => "generic_residency_bytes_overflow",
            Self::ResidencyBudgetSnapshotUnavailable => {
                "generic_residency_budget_snapshot_unavailable"
            }
            Self::ResidencyEvidenceChanged => "generic_residency_evidence_changed",
            Self::DerivedArtifactEstimateUnavailable => {
                "generic_derived_artifact_estimate_unavailable"
            }
            Self::GroupEstimateUnavailable => "generic_group_estimate_unavailable",
            Self::ResidencyBudgetExceeded { .. } => "generic_residency_budget_exceeded",
            Self::DeviceCostGate(ShapeCostGate::Eligible) => "generic_invalid_eligible_cost_gate",
            Self::DeviceCostGate(ShapeCostGate::FactRowsBelowDeviceMinimum { .. }) => {
                "generic_fact_rows_below_device_minimum"
            }
            Self::DeviceCostGate(ShapeCostGate::DenseOneShotRowsExceedDeviceMaximum { .. }) => {
                "generic_fact_rows_exceed_dense_one_shot_maximum"
            }
            Self::DeviceCostGate(ShapeCostGate::H3RowsBelowDeviceMinimum { .. }) => {
                "h3_rows_below_grouped_agg_min"
            }
            Self::DeviceCostGate(ShapeCostGate::SpatialRowsBelowDeviceMinimum { .. }) => {
                "postgis_rows_below_device_minimum"
            }
            Self::DeviceCostGate(ShapeCostGate::SpatialVerticesBelowDeviceMinimum { .. }) => {
                "postgis_vertices_below_device_minimum"
            }
            Self::DeviceCostGate(ShapeCostGate::SpatialVerticesExceedDeviceMaximum { .. }) => {
                "postgis_vertices_exceed_device_maximum"
            }
            Self::DeviceCostGate(ShapeCostGate::SpatialWorkBelowDeviceMinimum { .. }) => {
                "postgis_work_below_device_minimum"
            }
            Self::DeviceCostGate(ShapeCostGate::SpatialWorkExceedsDeviceMaximum { .. }) => {
                "postgis_work_exceeds_device_maximum"
            }
            Self::DeviceCostGate(ShapeCostGate::DimensionRowsExceedDeviceMaximum { .. }) => {
                "generic_dimension_rows_exceed_device_maximum"
            }
            Self::DeviceCostGate(ShapeCostGate::GroupsExceedDeviceMaximum { .. }) => {
                "generic_groups_exceed_device_maximum"
            }
            Self::NativePathUnavailable => "generic_native_path_unavailable",
            Self::CostNotCompetitive { .. } => "generic_cost_not_competitive",
            Self::PathNotAdded => "generic_path_not_added",
        }
    }
}

impl std::fmt::Display for AdmissionDecline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {self:?}", self.code())
    }
}

fn selected_relation(
    required: &super::shape::RequiredRelation,
) -> Result<SelectedRelation, AdmissionDecline> {
    let columns = required
        .attnos
        .iter()
        .map(|attno| {
            i16::try_from(*attno).map_err(|_| AdmissionDecline::AttributeNumberOutOfRange {
                relation_oid: required.relation_oid,
                attno: *attno,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SelectedRelation {
        relid: pg_sys::Oid::from(required.relation_oid),
        columns,
    })
}

#[derive(Debug, Clone, PartialEq)]
struct ExactResidencySnapshot {
    requests: Vec<SelectedRelation>,
    estimates: Vec<ResidentLoadEstimate>,
}

fn exact_residency_estimates(
    shape: &ShapePlan,
) -> Result<ExactResidencySnapshot, AdmissionDecline> {
    exact_residency_estimates_with(shape, estimate_selected_relation)
}

fn exact_residency_estimates_with<E>(
    shape: &ShapePlan,
    mut estimate: impl FnMut(&SelectedRelation) -> Result<ResidentLoadEstimate, E>,
) -> Result<ExactResidencySnapshot, AdmissionDecline>
where
    E: std::fmt::Display,
{
    super::with_planner_hooks_suspended(|| {
        let requests = shape
            .required_relations
            .iter()
            .map(selected_relation)
            .collect::<Result<Vec<_>, _>>()?;
        let estimates = requests
            .iter()
            .map(|selected| {
                estimate(selected).map_err(|error| AdmissionDecline::ResidencyEstimateFailed {
                    relation_oid: u32::from(selected.relid),
                    detail: error.to_string(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ExactResidencySnapshot {
            requests,
            estimates,
        })
    })
}

fn require_coherent_resident_evidence(
    snapshot: &ExactResidencySnapshot,
) -> Result<(), AdmissionDecline> {
    revalidate_loaded_estimates(&snapshot.requests, &snapshot.estimates)
        .then_some(())
        .ok_or(AdmissionDecline::ResidencyEvidenceChanged)
}

fn add_bytes(total: u64, bytes: u64) -> Result<u64, AdmissionDecline> {
    total
        .checked_add(bytes)
        .ok_or(AdmissionDecline::ResidencyBytesOverflow)
}

fn unamortized_resident_load_cost(
    rows: u64,
    bytes: u64,
    fixed_width: bool,
    model: &TypedCostModel,
) -> f64 {
    let row_cost = if fixed_width {
        model.coefficients.resident_load_scan_per_row_cost
    } else {
        // Variable-width decoding and dictionary/domain construction do not
        // scale with the compressed resident byte footprint. Retain the
        // established conservative materialization row charge for that class.
        model.coefficients.preagg_dim_materialize_cost
    };
    // Keep the products separate so admission retains the established
    // multiply-then-add rounding instead of using a fused operation.
    let row_charge = rows as f64 * row_cost.get();
    let byte_charge = bytes as f64 * model.coefficients.resident_load_per_byte_cost.get();
    row_charge + byte_charge
}

fn apply_exact_residency(
    shape: &mut ShapePlan,
    estimates: &[ResidentLoadEstimate],
    policy: AdmissionPolicy,
    model: &TypedCostModel,
) -> Result<(), AdmissionDecline> {
    for estimate in estimates {
        let relation_oid = u32::from(estimate.relid);
        if !shape
            .required_relations
            .iter()
            .any(|required| required.relation_oid == relation_oid)
        {
            return Err(AdmissionDecline::UnexpectedResidencyEstimate { relation_oid });
        }
    }

    let mut relations = Vec::with_capacity(shape.required_relations.len());
    let mut total_required_bytes = 0_u64;
    let mut missing_bytes = 0_u64;
    let mut missing_rows = 0_u64;
    let mut unamortized_load_cost = 0.0;
    for required in &shape.required_relations {
        let estimate = estimates
            .iter()
            .find(|estimate| u32::from(estimate.relid) == required.relation_oid)
            .ok_or(AdmissionDecline::MissingResidencyEstimate {
                relation_oid: required.relation_oid,
            })?;
        let prior = shape
            .residency
            .relations
            .iter()
            .find(|relation| relation.relation_oid == required.relation_oid)
            .ok_or(AdmissionDecline::MissingResidencyEstimate {
                relation_oid: required.relation_oid,
            })?;
        total_required_bytes = add_bytes(total_required_bytes, estimate.estimated_bytes)?;
        let state = if estimate.loaded {
            RelationResidency::Resident
        } else {
            if !policy.auto_load && !estimate.pinned {
                return Err(AdmissionDecline::AutoLoadDisabled {
                    relation_oid: required.relation_oid,
                    estimated_bytes: estimate.estimated_bytes,
                });
            }
            missing_bytes = add_bytes(missing_bytes, estimate.estimated_bytes)?;
            // Shape extraction records max(RelOptInfo.tuples, rows), so this
            // is the base relation's full-scan cardinality rather than a
            // post-restriction query-row estimate.
            missing_rows = missing_rows.saturating_add(prior.estimated_rows);
            unamortized_load_cost += unamortized_resident_load_cost(
                prior.estimated_rows,
                estimate.estimated_bytes,
                estimate.fixed_width,
                model,
            );
            RelationResidency::AutoLoad
        };
        relations.push(RelationResidencyRequirement {
            relation_oid: required.relation_oid,
            attnos: required.attnos.clone(),
            state,
            estimated_rows: prior.estimated_rows,
            estimated_bytes: Some(estimate.estimated_bytes),
        });
    }

    let relation_rows = shape
        .residency
        .relations
        .iter()
        .map(|relation| (relation.relation_oid, relation.estimated_rows))
        .collect::<BTreeMap<_, _>>();
    let derived_artifact_bytes =
        estimate_descriptor_artifact_bytes_upper_bound(&shape.spec, &relation_rows)
            .ok_or(AdmissionDecline::DerivedArtifactEstimateUnavailable)?;
    let projected_final_bytes = policy
        .budget_snapshot
        .projected_final_bytes(total_required_bytes, derived_artifact_bytes)
        .ok_or(AdmissionDecline::ResidencyBytesOverflow)?;
    if projected_final_bytes > policy.budget_bytes {
        return Err(AdmissionDecline::ResidencyBudgetExceeded {
            cluster_live_bytes: policy.budget_snapshot.cluster_live_bytes,
            current_backend_live_bytes: policy.budget_snapshot.current_backend_live_bytes,
            other_backend_live_bytes: policy.budget_snapshot.other_backend_live_bytes,
            pinned_unselected_raw_bytes: policy.budget_snapshot.pinned_unselected_raw_bytes,
            evictable_or_replaced_local_bytes: policy
                .budget_snapshot
                .evictable_or_replaced_local_bytes,
            selected_raw_bytes: total_required_bytes,
            derived_artifact_bytes,
            projected_final_bytes,
            budget_bytes: policy.budget_bytes,
        });
    }

    let expected_reuses = estimates
        .iter()
        .map(|estimate| estimate.amortization_queries.max(1))
        .min()
        .and_then(NonZeroU32::new)
        .unwrap_or(shape.residency.expected_reuses);
    let load_cost = unamortized_load_cost / f64::from(expected_reuses.get());
    let amortized_auto_load = PgCost::new(load_cost);
    shape.residency = ResidencyEstimate {
        relations,
        total_required_bytes: Some(total_required_bytes),
        missing_bytes: Some(missing_bytes),
        missing_rows,
        expected_reuses,
        amortized_load_cost: amortized_auto_load,
    };
    shape.cost.replace_amortized_auto_load(amortized_auto_load);
    let fact_estimate = estimates
        .iter()
        .find(|estimate| u32::from(estimate.relid) == shape.spec.fact_rel);
    let resident_fact_rows = fact_estimate
        .filter(|estimate| estimate.loaded)
        .and_then(|estimate| estimate.resident_rows);
    let fact_columns_have_no_null_sidecars = fact_estimate
        .filter(|estimate| estimate.loaded)
        .and_then(|estimate| estimate.selected_columns_have_no_null_sidecars);
    let estimated_fact_rows = shape
        .residency
        .relations
        .iter()
        .find(|relation| relation.relation_oid == shape.spec.fact_rel)
        .map_or(0, |relation| relation.estimated_rows);
    if let Some(aggregate) = dense_atomic_sum_count_cost(
        &shape.spec,
        &shape.descriptor_resolution,
        estimated_fact_rows,
        resident_fact_rows,
        fact_columns_have_no_null_sidecars,
        model,
    ) {
        shape.cost.replace_aggregate(aggregate);
    }
    let lifecycle_fact_rows = resident_fact_rows.unwrap_or(estimated_fact_rows);
    if let Some(calls) = planned_dense_lifecycle_calls(shape, lifecycle_fact_rows, model) {
        let additional_calls = calls.saturating_sub(1);
        shape
            .cost
            .replace_additional_aggregate_launches(PgCost::new(
                additional_calls as f64 * cost::GPU_LAUNCH_OVERHEAD,
            ));
    } else {
        shape
            .cost
            .replace_additional_aggregate_launches(PgCost::ZERO);
    }
    if dense_atomic_sum_count_lifecycle(&shape.spec, &shape.descriptor_resolution, model)
        && matches!(
            shape.cost_gate,
            ShapeCostGate::Eligible | ShapeCostGate::FactRowsBelowDeviceMinimum { .. }
        )
    {
        // Exact loaded rows replace the generic estimate-derived row gate.
        // Before first load, the planner estimate remains the best available
        // evidence. Other shape gates are deliberately never overwritten.
        let lifecycle_fact_rows =
            Rows::new(usize::try_from(lifecycle_fact_rows).unwrap_or(usize::MAX));
        let required = dense_atomic_fact_row_floor(&shape.spec, model);
        shape.cost_gate = if lifecycle_fact_rows < required {
            ShapeCostGate::FactRowsBelowDeviceMinimum {
                estimated: lifecycle_fact_rows,
                required,
            }
        } else if lifecycle_fact_rows > model.executor.gpu_grouped_agg_one_shot_max_rows {
            ShapeCostGate::DenseOneShotRowsExceedDeviceMaximum {
                fact_rows: lifecycle_fact_rows,
                maximum: model.executor.gpu_grouped_agg_one_shot_max_rows,
            }
        } else {
            ShapeCostGate::Eligible
        };
    }
    Ok(())
}

fn shape_kernel_mode(shape: &ShapePlan) -> GroupedAggKernelMode {
    let hash_grouping = match &shape.descriptor_resolution {
        DescriptorResolution::BeginTimeArtifacts { grouping_mode, .. } => {
            *grouping_mode == DescriptorGroupingMode::Hash
        }
        DescriptorResolution::Ready => shape
            .spec
            .group_keys
            .iter()
            .any(|key| key.encoding == GroupKeyEncoding::Hash),
    };
    planned_descriptor_kernel_mode(&shape.spec, hash_grouping)
}

fn planned_dense_lifecycle_calls(
    shape: &ShapePlan,
    fact_rows: u64,
    model: &TypedCostModel,
) -> Option<u64> {
    if matches!(shape.spec.fact_filter, FilterSpec::Spatial { .. })
        || shape.spec.group_keys.iter().any(|key| {
            matches!(
                key.source,
                crate::engine::spec::GroupKeySource::H3CellToParent { .. }
                    | crate::engine::spec::GroupKeySource::H3LatLngToCell { .. }
            )
        })
        || !matches!(
            shape_kernel_mode(shape),
            GroupedAggKernelMode::ParallelDenseCount | GroupedAggKernelMode::ParallelDenseInteger
        )
    {
        return None;
    }
    dense_lifecycle_call_count(
        fact_rows,
        model.executor.gpu_grouped_agg_one_shot_max_rows,
        model.executor.gpu_reduce_max_chunk,
    )
}

fn validate_shape_capability(shape: &ShapePlan) -> Result<(), AdmissionDecline> {
    let projection = AggOutputProjection {
        slots: shape.projections.clone(),
    };
    let result = if matches!(shape.spec.fact_filter, FilterSpec::Spatial { .. }) {
        validate_normal_spatial_candidate_capability(&shape.spec, &projection)
    } else {
        validate_normal_descriptor_capability(&shape.spec, &projection)
    };
    result.map_err(|detail| AdmissionDecline::DescriptorCapability { detail })?;
    let mode = shape_kernel_mode(shape);
    if mode == GroupedAggKernelMode::SerialGeneric {
        return Err(AdmissionDecline::SerialGenericKernelMode { mode: mode.label() });
    }
    Ok(())
}

const NORMAL_SPATIAL_PROMOTION_ROWS: u64 = 1_000_000;

fn normal_spatial_promotion_rows(exact_rows: Option<u64>) -> bool {
    exact_rows == Some(NORMAL_SPATIAL_PROMOTION_ROWS)
}

fn validate_normal_spatial_residency_envelope(
    shape: &ShapePlan,
    snapshot: &ExactResidencySnapshot,
) -> Result<(), AdmissionDecline> {
    if !matches!(shape.spec.fact_filter, FilterSpec::Spatial { .. }) {
        return Ok(());
    }
    let exact_rows = snapshot
        .estimates
        .iter()
        .find(|estimate| u32::from(estimate.relid) == shape.spec.fact_rel)
        .filter(|estimate| estimate.loaded)
        .and_then(|estimate| estimate.resident_rows);
    if !normal_spatial_promotion_rows(exact_rows) {
        return Err(AdmissionDecline::DescriptorCapability {
            detail: format!(
                "normal spatial admission requires exactly {NORMAL_SPATIAL_PROMOTION_ROWS} resident fact rows; observed {exact_rows:?}"
            ),
        });
    }
    Ok(())
}

fn stable_device_cost_gate_before_residency(
    shape: &ShapePlan,
    model: &TypedCostModel,
) -> Option<ShapeCostGate> {
    match shape.cost_gate {
        ShapeCostGate::Eligible => None,
        ShapeCostGate::FactRowsBelowDeviceMinimum { .. }
            if dense_atomic_sum_count_lifecycle(
                &shape.spec,
                &shape.descriptor_resolution,
                model,
            ) =>
        {
            // Exact loaded-row evidence may replace the planner estimate for
            // this lifecycle, so residency must retain the chance to refine it.
            None
        }
        gate => Some(gate),
    }
}

fn cheap_exact_dense_row_gate(
    shape: &ShapePlan,
    model: &TypedCostModel,
) -> Result<Option<(ShapeCostGate, ResidentPlannerDependency)>, AdmissionDecline> {
    if !matches!(
        shape.cost_gate,
        ShapeCostGate::FactRowsBelowDeviceMinimum { .. }
    ) || !dense_atomic_sum_count_lifecycle(&shape.spec, &shape.descriptor_resolution, model)
    {
        return Ok(None);
    }
    let fact = shape
        .required_relations
        .iter()
        .find(|required| required.relation_oid == shape.spec.fact_rel)
        .ok_or(AdmissionDecline::MissingResidencyEstimate {
            relation_oid: shape.spec.fact_rel,
        })?;
    let request = selected_relation(fact)?;
    let Some(dependency) = resident_planner_dependency(&request) else {
        return Ok(None);
    };
    let gate = cheap_exact_dense_row_gate_with(shape, model, |_| Some(dependency.row_count))?;
    Ok(gate.map(|gate| (gate, dependency)))
}

fn cheap_exact_dense_row_gate_with(
    shape: &ShapePlan,
    model: &TypedCostModel,
    exact_rows: impl FnOnce(&SelectedRelation) -> Option<u64>,
) -> Result<Option<ShapeCostGate>, AdmissionDecline> {
    if !matches!(
        shape.cost_gate,
        ShapeCostGate::FactRowsBelowDeviceMinimum { .. }
    ) || !dense_atomic_sum_count_lifecycle(&shape.spec, &shape.descriptor_resolution, model)
    {
        return Ok(None);
    }
    let fact = shape
        .required_relations
        .iter()
        .find(|required| required.relation_oid == shape.spec.fact_rel)
        .ok_or(AdmissionDecline::MissingResidencyEstimate {
            relation_oid: shape.spec.fact_rel,
        })?;
    let request = selected_relation(fact)?;
    let Some(exact_rows) = exact_rows(&request) else {
        return Ok(None);
    };
    let exact_rows = Rows::new(usize::try_from(exact_rows).unwrap_or(usize::MAX));
    let required = dense_atomic_fact_row_floor(&shape.spec, model);
    if exact_rows < required {
        return Ok(Some(ShapeCostGate::FactRowsBelowDeviceMinimum {
            estimated: exact_rows,
            required,
        }));
    }
    Ok(None)
}

#[cfg(any(test, feature = "pg_test"))]
fn test_force_spatial_groupagg(shape: &ShapePlan) -> bool {
    gucs::test_force_spatial_groupagg()
        && matches!(&shape.spec.fact_filter, FilterSpec::Spatial { .. })
}

#[cfg(any(test, feature = "pg_test"))]
fn test_forceable_spatial_cost_gate(gate: ShapeCostGate) -> bool {
    matches!(
        gate,
        ShapeCostGate::Eligible
            | ShapeCostGate::FactRowsBelowDeviceMinimum { .. }
            | ShapeCostGate::SpatialRowsBelowDeviceMinimum { .. }
            | ShapeCostGate::SpatialVerticesBelowDeviceMinimum { .. }
            | ShapeCostGate::SpatialWorkBelowDeviceMinimum { .. }
    )
}

fn gate_cost(
    shape: &ShapePlan,
    native_cost: Option<f64>,
    maximum_cost_ratio: f64,
    effective_cost: EffectivePathCost,
) -> Result<(), AdmissionDecline> {
    match shape.cost_gate {
        ShapeCostGate::Eligible => {}
        gate => return Err(AdmissionDecline::DeviceCostGate(gate)),
    }
    let native_cost = native_cost
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
        .ok_or(AdmissionDecline::NativePathUnavailable)?;
    let gpu_cost = effective_cost.total;
    let required_cost = native_cost * maximum_cost_ratio;
    if gpu_cost > required_cost {
        return Err(AdmissionDecline::CostNotCompetitive {
            gpu_cost,
            native_cost,
            required_cost,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct EffectivePathCost {
    startup: f64,
    total: f64,
}

fn effective_path_cost(shape: &ShapePlan, multiplier: f64) -> EffectivePathCost {
    let total = ((cost::GPU_LAUNCH_OVERHEAD + shape.cost.total.get()) * multiplier).max(1.0);
    let startup = (cost::GPU_LAUNCH_OVERHEAD * multiplier).max(0.0).min(total);
    EffectivePathCost { startup, total }
}

fn maximum_cost_ratio(shape: &ShapePlan, model: &TypedCostModel) -> f64 {
    if shape.spec.star_dims.is_empty() {
        model.planner.gpu_agg_cost_ratio
    } else {
        model.planner.gpu_preagg_cost_ratio
    }
}

fn encode_shape_path_private(
    spec: &crate::engine::spec::AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<Vec<c_int>, ShapeDecline> {
    let spec_words = spec
        .encode_i32()
        .map_err(|error| ShapeDecline::Codec(error.to_string()))?;
    let projection_words = projection
        .encode_i32(spec)
        .map_err(|error| ShapeDecline::Codec(error.to_string()))?;
    let mut words = Vec::with_capacity(spec_words.len() + projection_words.len() + 2);
    words.push(AGG_QUERY_SPEC_SENTINEL);
    words.extend(spec_words);
    words.push(AGG_OUTPUT_PROJECTION_SENTINEL);
    words.extend(projection_words);
    Ok(words)
}

fn shape_stages(shape: &ShapePlan) -> (Vec<ResidentOperatorStage>, bool) {
    let has_spatial = matches!(shape.spec.fact_filter, FilterSpec::Spatial { .. })
        || shape
            .spec
            .measures
            .iter()
            .any(|measure| matches!(measure.filter, FilterSpec::Spatial { .. }))
        || shape
            .spec
            .star_dims
            .iter()
            .any(|dimension| matches!(dimension.filter, FilterSpec::Spatial { .. }));
    let has_non_spatial_filter = (!matches!(shape.spec.fact_filter, FilterSpec::None)
        && !matches!(shape.spec.fact_filter, FilterSpec::Spatial { .. }))
        || shape.spec.measures.iter().any(|measure| {
            !matches!(
                measure.filter,
                FilterSpec::None | FilterSpec::Spatial { .. }
            )
        })
        || shape.spec.star_dims.iter().any(|dimension| {
            !matches!(
                dimension.filter,
                FilterSpec::None | FilterSpec::Spatial { .. }
            )
        });
    let has_filter = has_spatial || has_non_spatial_filter;
    let has_expression = has_non_spatial_filter
        || shape.spec.measures.iter().any(|measure| {
            !matches!(
                measure.expression,
                MeasureExpr::CountStar | MeasureExpr::Column(_)
            )
        });
    let mut stages = vec![ResidentOperatorStage::Scan];
    if !shape.spec.star_dims.is_empty() {
        stages.push(ResidentOperatorStage::Join);
    }
    if has_expression {
        stages.push(ResidentOperatorStage::Expression);
    }
    if has_spatial {
        stages.push(ResidentOperatorStage::Postgis);
    }
    if shape.spec.group_keys.iter().any(|key| {
        matches!(
            key.source,
            crate::engine::spec::GroupKeySource::H3CellToParent { .. }
                | crate::engine::spec::GroupKeySource::H3LatLngToCell { .. }
        )
    }) {
        stages.push(ResidentOperatorStage::H3);
    }
    stages.push(ResidentOperatorStage::GroupedAggregate);
    stages.push(ResidentOperatorStage::FinalMaterialization);
    (stages, has_filter)
}

fn shape_resident_proof(shape: &ShapePlan) -> crate::engine::residency::ResidentProofSnapshot {
    let (stages, has_filter) = shape_stages(shape);
    let resident_inputs = shape
        .required_relations
        .iter()
        .fold(0_usize, |count, relation| {
            count.saturating_add(relation.attnos.len())
        });
    let device_columns = resident_inputs.max(1);
    ResidentPipelineProof::device_resident(
        ResidentOperatorClass::ResidentGroupAgg,
        stages,
        MaterializationBoundary::FinalOutput,
        device_columns,
        has_filter,
        true,
    )
    .snapshot()
}

fn advertised_output_rows(shape: &ShapePlan) -> f64 {
    shape.estimated_output_rows as f64
}

unsafe fn inject_childless_shape_path(
    output_rel: *mut RelOptInfo,
    shape: &ShapePlan,
    effective_cost: EffectivePathCost,
) -> Result<bool, ShapeDecline> {
    if output_rel.is_null() || shape.cost_gate != ShapeCostGate::Eligible {
        return Ok(false);
    }

    let projection = AggOutputProjection {
        slots: shape.projections.clone(),
    };
    let words = encode_shape_path_private(&shape.spec, &projection)?;
    let mut digest_words = shape.spec.encode_i32().map_err(|error| {
        ShapeDecline::Codec(format!(
            "shape spec changed before path construction: {error}"
        ))
    })?;
    digest_words.extend(projection.encode_i32(&shape.spec).map_err(|error| {
        ShapeDecline::Codec(format!(
            "shape projection changed before path construction: {error}"
        ))
    })?);
    if digest_words != shape.digest_words {
        return Err(ShapeDecline::Codec(
            "shape digest words do not match the serialized AQS3/AOP2 contract".to_owned(),
        ));
    }

    // SAFETY: allocation occurs in the active planner memory context and requests
    // exactly enough zeroed storage for one CustomPath.
    let cpath = unsafe {
        pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomPath>()).cast::<pg_sys::CustomPath>()
    };
    // SAFETY: palloc0 either raises a PostgreSQL error or returns aligned storage
    // for CustomPath; output_rel is valid for this callback and all constructed
    // List nodes share its planner memory context.
    unsafe {
        (*cpath).path.type_ = NodeTag::T_CustomPath;
        (*cpath).path.pathtype = NodeTag::T_CustomScan;
        (*cpath).path.parent = output_rel;
        (*cpath).path.pathtarget = (*output_rel).reltarget;
        (*cpath).path.param_info = std::ptr::null_mut();
        (*cpath).path.parallel_aware = false;
        (*cpath).path.parallel_safe = false;
        (*cpath).path.parallel_workers = 0;
        (*cpath).path.rows = advertised_output_rows(shape);
        (*cpath).path.startup_cost = effective_cost.startup;
        (*cpath).path.total_cost = effective_cost.total;
        (*cpath).path.pathkeys = std::ptr::null_mut();
        (*cpath).flags = 0;
        (*cpath).custom_paths = std::ptr::null_mut();
        (*cpath).custom_restrictinfo = std::ptr::null_mut();
        (*cpath).methods = custom_scan::agg_path_methods();

        let mut private: *mut List = std::ptr::null_mut();
        for word in words {
            private = pg_sys::lappend(private, pg_sys::makeInteger(word).cast());
        }
        (*cpath).custom_private = private;
    }

    // SAFETY: output_rel and the newly initialized cpath are planner-owned, and
    // cpath carries a complete resident proof in its private payload.
    Ok(unsafe {
        add_gpu_path_with_resident_proof(
            stats::PlannerHookStage::UpperGroupAgg,
            GENERIC_SHAPE_PATH_CONTEXT,
            output_rel,
            cpath,
            shape_resident_proof(shape),
        )
    })
}

const MAX_SERIAL_PATH_DEPTH: usize = 128;

unsafe fn path_list_is_serial(pathlist: *mut List, depth: usize) -> bool {
    if pathlist.is_null() {
        return true;
    }
    // SAFETY: the caller supplies a live, aligned PostgreSQL List with a valid
    // header.
    let len = unsafe { pg_sys::list_length(pathlist) };
    for index in 0..len {
        // SAFETY: index is within the live caller-supplied List.
        let path = unsafe { pg_sys::list_nth(pathlist, index).cast::<pg_sys::Path>() };
        // SAFETY: the caller guarantees each non-null element is a live, aligned
        // concrete Path allocation matching its NodeTag. Null fails closed.
        if !unsafe { path_tree_is_serial(path, depth) } {
            return false;
        }
    }
    true
}

unsafe fn path_tree_is_serial(path: *mut pg_sys::Path, depth: usize) -> bool {
    if path.is_null() || depth > MAX_SERIAL_PATH_DEPTH {
        return false;
    }
    // `parallel_safe` only describes eligibility. These two fields describe
    // an execution path that actually participates in parallel execution.
    // SAFETY: path is non-null; the caller guarantees it is live, aligned, and
    // allocated as a concrete Path-derived value matching its NodeTag.
    if unsafe { (*path).parallel_aware || (*path).parallel_workers > 0 } {
        return false;
    }

    // SAFETY: every valid Path begins with its NodeTag. The caller guarantees
    // that tag matches the concrete allocation and that all child pointers and
    // Lists satisfy the same live/aligned contract. Unknown path kinds fail
    // closed because their possible children cannot be proved serial.
    let next_depth = depth + 1;
    // SAFETY: path satisfies the caller contract above and was checked non-null.
    match unsafe { (*path).type_ } {
        NodeTag::T_GatherPath | NodeTag::T_GatherMergePath => false,
        NodeTag::T_AggPath => {
            let path = path.cast::<pg_sys::AggPath>();
            // SAFETY: the caller contract and T_AggPath tag establish the
            // concrete layout; its subpath remains live for this walk.
            unsafe {
                (*path).aggsplit == pg_sys::AggSplit::AGGSPLIT_SIMPLE
                    && path_tree_is_serial((*path).subpath, next_depth)
            }
        }
        NodeTag::T_SortPath | NodeTag::T_IncrementalSortPath => {
            let path = path.cast::<pg_sys::SortPath>();
            // SAFETY: the caller contract and matched tag establish the layout;
            // both concrete types share SortPath's live subpath field.
            unsafe { path_tree_is_serial((*path).subpath, next_depth) }
        }
        NodeTag::T_ProjectionPath => {
            let path = path.cast::<pg_sys::ProjectionPath>();
            // SAFETY: the caller contract and matched tag establish the
            // ProjectionPath layout, and its subpath remains live for this walk.
            unsafe { path_tree_is_serial((*path).subpath, next_depth) }
        }
        NodeTag::T_MaterialPath => {
            let path = path.cast::<pg_sys::MaterialPath>();
            // SAFETY: the caller contract and matched tag establish the
            // MaterialPath layout, and its subpath remains live for this walk.
            unsafe { path_tree_is_serial((*path).subpath, next_depth) }
        }
        NodeTag::T_MemoizePath => {
            let path = path.cast::<pg_sys::MemoizePath>();
            // SAFETY: the caller contract and matched tag establish the
            // MemoizePath layout, and its subpath remains live for this walk.
            unsafe { path_tree_is_serial((*path).subpath, next_depth) }
        }
        NodeTag::T_NestPath | NodeTag::T_MergePath | NodeTag::T_HashPath => {
            let path = path.cast::<pg_sys::JoinPath>();
            // SAFETY: the caller contract and matched tag establish the layout;
            // each concrete type embeds JoinPath here and both children stay live.
            unsafe {
                path_tree_is_serial((*path).outerjoinpath, next_depth)
                    && path_tree_is_serial((*path).innerjoinpath, next_depth)
            }
        }
        NodeTag::T_AppendPath => {
            let path = path.cast::<pg_sys::AppendPath>();
            // SAFETY: the caller contract and matched tag establish the
            // AppendPath layout and the live subpaths List satisfies that contract.
            unsafe { path_list_is_serial((*path).subpaths, next_depth) }
        }
        NodeTag::T_MergeAppendPath => {
            let path = path.cast::<pg_sys::MergeAppendPath>();
            // SAFETY: the caller contract and matched tag establish the
            // MergeAppendPath layout, and its live subpaths List satisfies that
            // contract.
            unsafe { path_list_is_serial((*path).subpaths, next_depth) }
        }
        // Extension and FDW implementations are opaque to this proof even
        // when they expose no child paths or PostgreSQL parallel fields.
        NodeTag::T_CustomPath | NodeTag::T_ForeignPath => false,
        // These are terminal scan/result paths for aggregate inputs. They have
        // no executable Path children; the parallel fields above are decisive.
        NodeTag::T_Path
        | NodeTag::T_IndexPath
        | NodeTag::T_BitmapHeapPath
        | NodeTag::T_TidPath
        | NodeTag::T_TidRangePath
        | NodeTag::T_GroupResultPath => true,
        _ => false,
    }
}

unsafe fn cheapest_serial_agg_path_from_iter(
    paths: impl IntoIterator<Item = *mut pg_sys::Path>,
) -> *mut pg_sys::Path {
    let mut best: *mut pg_sys::Path = std::ptr::null_mut();
    for path in paths {
        // SAFETY: the caller guarantees non-null entries are live, aligned
        // concrete Path-derived values whose NodeTag matches their allocation.
        if path.is_null() || unsafe { (*path).type_ != NodeTag::T_AggPath } {
            continue;
        }
        let agg = path.cast::<pg_sys::AggPath>();
        // SAFETY: the caller contract and checked T_AggPath tag establish the
        // cast.
        if unsafe { (*agg).aggsplit != pg_sys::AggSplit::AGGSPLIT_SIMPLE }
            // SAFETY: path satisfies the caller contract; recursive traversal
            // matches each concrete layout to its NodeTag before reading children.
            || !unsafe { path_tree_is_serial(path, 0) }
        {
            continue;
        }
        // SAFETY: path is non-null and remains live under the caller contract
        // throughout selection.
        let valid_cost = unsafe {
            (*path).startup_cost.is_finite()
                && (*path).startup_cost >= 0.0
                && (*path).total_cost.is_finite()
                && (*path).total_cost >= (*path).startup_cost
        };
        if !valid_cost {
            continue;
        }
        // SAFETY: path is the current validated entry; non-null best always
        // points to an earlier validated entry with the same caller lifetime.
        if best.is_null() || unsafe { (*path).total_cost < (*best).total_cost } {
            best = path;
        }
    }
    best
}

unsafe fn cheapest_live_agg_cost_from_iter(
    paths: impl IntoIterator<Item = *mut pg_sys::Path>,
    output_rel: *mut RelOptInfo,
) -> Option<f64> {
    let mut best: Option<f64> = None;
    for path in paths {
        // SAFETY: the caller guarantees non-null entries are live, aligned
        // concrete Path-derived values whose NodeTag matches their allocation.
        if path.is_null() || unsafe { (*path).type_ != NodeTag::T_AggPath } {
            continue;
        }
        // SAFETY: short-circuiting proves both pointers non-null before reading
        // the Path parent field; output_rel and every path are planner-owned for
        // the duration of this selection.
        if output_rel.is_null() || unsafe { (*path).parent != output_rel } {
            continue;
        }
        // This fallback may be a parallel FINAL_DESERIAL aggregate. It is used
        // only when no proved serial comparator survives PostgreSQL path
        // pruning, so comparing against it is stricter than the preferred
        // serial baseline.
        // SAFETY: path is non-null and remains live under the caller contract.
        let cost = unsafe { (*path).total_cost };
        // SAFETY: the same live common Path header contains startup_cost.
        if !cost.is_finite()
            || cost < 0.0
            || !unsafe { (*path).startup_cost.is_finite() }
            || unsafe { (*path).startup_cost < 0.0 || cost < (*path).startup_cost }
        {
            continue;
        }
        if best.is_none_or(|current| cost < current) {
            best = Some(cost);
        }
    }
    best
}

unsafe fn cheapest_serial_input_path_from_iter(
    paths: impl IntoIterator<Item = *mut pg_sys::Path>,
    input_rel: *mut RelOptInfo,
) -> *mut pg_sys::Path {
    let mut best: *mut pg_sys::Path = std::ptr::null_mut();
    for path in paths {
        if path.is_null() || input_rel.is_null() {
            continue;
        }
        // SAFETY: the caller guarantees path and input_rel are live, aligned,
        // correctly typed pointers. The common Path header contains parent and
        // param_info; recursive traversal matches concrete layouts to NodeTags.
        let incompatible = unsafe {
            (*path).parent != input_rel
                || !(*path).param_info.is_null()
                || !path_tree_is_serial(path, 0)
        };
        if incompatible {
            continue;
        }
        // SAFETY: path is non-null and the caller keeps it live for the duration
        // of selection.
        let valid_cost = unsafe {
            (*path).startup_cost.is_finite()
                && (*path).startup_cost >= 0.0
                && (*path).total_cost.is_finite()
                && (*path).total_cost >= (*path).startup_cost
                && (*path).rows.is_finite()
                && (*path).rows >= 0.0
                && !(*path).pathtarget.is_null()
        };
        // SAFETY: path is the current validated entry; non-null best points to
        // an earlier validated entry that remains live under the caller contract.
        if valid_cost && (best.is_null() || unsafe { (*path).total_cost < (*best).total_cost }) {
            best = path;
        }
    }
    best
}

unsafe fn consistent_agg_num_groups_from_iter(
    paths: impl IntoIterator<Item = *mut pg_sys::Path>,
    output_rel: *mut RelOptInfo,
) -> Option<f64> {
    let mut estimate: Option<f64> = None;
    for path in paths {
        // SAFETY: the caller guarantees non-null entries are live, aligned
        // concrete Path-derived values whose NodeTag matches their allocation.
        if path.is_null() || unsafe { (*path).type_ != NodeTag::T_AggPath } {
            continue;
        }
        // SAFETY: short-circuiting proves both pointers non-null before reading
        // the Path parent field; the caller guarantees output_rel is a live,
        // aligned RelOptInfo.
        if output_rel.is_null() || unsafe { (*path).parent != output_rel } {
            return None;
        }
        // SAFETY: the caller contract and checked T_AggPath tag establish the
        // concrete layout, and the path remains live for the iterator call.
        let groups = unsafe { (*path.cast::<pg_sys::AggPath>()).numGroups };
        if !groups.is_finite() || groups <= 0.0 {
            return None;
        }
        match estimate {
            Some(previous) if previous.to_bits() != groups.to_bits() => return None,
            None => estimate = Some(groups),
            _ => {}
        }
    }
    estimate
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct AggGroupEstimate {
    num_groups: f64,
    output_rows: u64,
}

fn conservative_output_rows(num_groups: f64) -> Option<u64> {
    // Exact IEEE-754 representation of 2^64, the first value outside u64.
    const U64_EXCLUSIVE_UPPER_BOUND: f64 = f64::from_bits(0x43f0_0000_0000_0000);

    if !num_groups.is_finite() || num_groups <= 0.0 || num_groups >= U64_EXCLUSIVE_UPPER_BOUND {
        return None;
    }
    Some(num_groups.ceil() as u64)
}

unsafe fn consistent_agg_group_estimate(output_rel: *mut RelOptInfo) -> Option<AggGroupEstimate> {
    if output_rel.is_null() {
        return None;
    }
    // SAFETY: output_rel is the live upper relation supplied to this planner
    // callback, and pathlist contains planner-owned Path pointers.
    let pathlist = unsafe { (*output_rel).pathlist };
    // SAFETY: pathlist is either PostgreSQL's null empty-list representation or
    // a live planner-owned List.
    let len = unsafe { pg_sys::list_length(pathlist) };
    // SAFETY: every index is bounded by list_length; list entries and output_rel
    // remain planner-owned and live throughout the consistency scan.
    let num_groups = unsafe {
        consistent_agg_num_groups_from_iter(
            (0..len).map(|index| pg_sys::list_nth(pathlist, index).cast()),
            output_rel,
        )?
    };
    Some(AggGroupEstimate {
        num_groups,
        output_rows: conservative_output_rows(num_groups)?,
    })
}

fn minimum_valid_cost(costs: impl IntoIterator<Item = Option<f64>>) -> Option<f64> {
    costs
        .into_iter()
        .flatten()
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
        .min_by(f64::total_cmp)
}

fn grouping_capability_enabled(flags: u32, capability: u32) -> bool {
    flags & capability != 0
}

/// Return a conservative numerical lower bound for a serial sorted aggregate.
///
/// The selected input is not required to provide the grouping order, so this
/// is not an executable path and must never be added to a relation. Omitting
/// the cost of establishing that order makes the result no greater than any
/// valid sorted aggregate over the proven serial inputs. It prevents hash
/// spill I/O from inflating the native admission comparator.
unsafe fn serial_sorted_agg_cost_lower_bound(
    root: *mut pg_sys::PlannerInfo,
    serial_input: *mut pg_sys::Path,
    output_rel: *mut RelOptInfo,
    extra: *mut pg_sys::GroupPathExtraData,
    costs: *const pg_sys::AggClauseCosts,
    num_groups: f64,
) -> Option<f64> {
    if root.is_null()
        || serial_input.is_null()
        || output_rel.is_null()
        || extra.is_null()
        || costs.is_null()
    {
        return None;
    }
    // SAFETY: root was checked non-null and is the live PlannerInfo supplied by
    // PostgreSQL for this callback.
    let group_clause = unsafe { (*root).processed_groupClause };
    // SAFETY: serial_input was checked non-null and is a live planner Path.
    let input_target = unsafe { (*serial_input).pathtarget };
    // SAFETY: output_rel was checked non-null and is a live planner relation.
    let output_target = unsafe { (*output_rel).reltarget };
    if group_clause.is_null() || input_target.is_null() || output_target.is_null() {
        return None;
    }
    // SAFETY: group_clause is a non-null, planner-owned PostgreSQL List.
    let group_columns = unsafe { pg_sys::list_length(group_clause) };
    // SAFETY: output_target is non-null and planner-owned for this callback.
    let target_cost = unsafe { (*output_target).cost };
    // SAFETY: input_target is non-null and planner-owned for this callback.
    let input_width = unsafe { (*input_target).width };
    if group_columns <= 0
        || input_width < 0
        || !target_cost.startup.is_finite()
        || target_cost.startup < 0.0
        || !target_cost.per_tuple.is_finite()
        || target_cost.per_tuple < 0.0
    {
        return None;
    }

    let mut lower_bound = pg_sys::Path::default();
    // SAFETY: all planner pointers were checked non-null, group_clause and
    // targets are live, costs points to caller-owned initialized clause costs,
    // and lower_bound is valid writable storage for cost_agg's output.
    unsafe {
        pg_sys::cost_agg(
            &raw mut lower_bound,
            root,
            pg_sys::AggStrategy::AGG_SORTED,
            costs,
            group_columns,
            num_groups,
            (*extra).havingQual.cast::<List>(),
            (*serial_input).disabled_nodes,
            (*serial_input).startup_cost,
            (*serial_input).total_cost,
            (*serial_input).rows,
            f64::from(input_width),
        );
    }
    lower_bound.startup_cost += target_cost.startup;
    let per_tuple_cost = target_cost.per_tuple * lower_bound.rows;
    lower_bound.total_cost += target_cost.startup + per_tuple_cost;

    // SAFETY: serial_input was checked non-null and remains planner-owned while
    // the derived lower bound is validated.
    (lower_bound.disabled_nodes == unsafe { (*serial_input).disabled_nodes }
        && lower_bound.rows.is_finite()
        && lower_bound.rows >= 0.0
        && lower_bound.startup_cost.is_finite()
        && lower_bound.startup_cost >= 0.0
        && lower_bound.total_cost.is_finite()
        && lower_bound.total_cost >= lower_bound.startup_cost)
        .then_some(lower_bound.total_cost)
}

unsafe fn reconstructed_serial_agg_cost(
    root: *mut pg_sys::PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
    extra: *mut c_void,
    num_groups: f64,
) -> Option<f64> {
    if root.is_null() || input_rel.is_null() || output_rel.is_null() || extra.is_null() {
        return None;
    }
    // SAFETY: root was checked non-null and is the live planner root supplied
    // to this upper-path callback.
    let parse = unsafe { (*root).parse };
    let extra = extra.cast::<pg_sys::GroupPathExtraData>();
    if parse.is_null() {
        return None;
    }
    // SAFETY: parse is now non-null and owned by root; output_rel is non-null,
    // and extra is the valid GroupPathExtraData promised by the callback.
    let unsupported = unsafe {
        !(*parse).hasAggs
            || !(*parse).groupingSets.is_null()
            || (*output_rel).reltarget.is_null()
            || (*extra).havingQual != (*parse).havingQual
    };
    if unsupported {
        return None;
    }

    // SAFETY: input_rel is non-null and is the live planner input relation.
    let input_paths = unsafe { (*input_rel).pathlist };
    // SAFETY: input_paths is either PostgreSQL's null empty-list value or a
    // live planner-owned List.
    let input_len = unsafe { pg_sys::list_length(input_paths) };
    // SAFETY: each index is bounded by list_length, every entry is a planner
    // Path, and input_rel remains live during selection.
    let serial_input = unsafe {
        cheapest_serial_input_path_from_iter(
            (0..input_len).map(|index| pg_sys::list_nth(input_paths, index).cast()),
            input_rel,
        )
    };
    if serial_input.is_null() {
        return None;
    }

    // SAFETY: parse is non-null and owned by the active PlannerInfo.
    let grouped = unsafe { !(*parse).groupClause.is_null() };
    let strategy = if grouped {
        // SAFETY: root is non-null and its processed grouping list is live for
        // this planner invocation.
        if unsafe { (*root).processed_groupClause.is_null() } {
            return None;
        }
        // SAFETY: extra is the valid GroupPathExtraData pointer from the
        // callback contract.
        let hash_enabled = grouping_capability_enabled(
            unsafe { (*extra).flags } as u32,
            pg_sys::GROUPING_CAN_USE_HASH,
        );
        hash_enabled.then_some(pg_sys::AggStrategy::AGG_HASHED)
    } else {
        // SAFETY: root is non-null and owned by the active planner invocation.
        if unsafe { !(*root).processed_groupClause.is_null() } {
            return None;
        }
        Some(pg_sys::AggStrategy::AGG_PLAIN)
    };

    let mut costs = pg_sys::AggClauseCosts::default();
    // SAFETY: root is the live PlannerInfo and costs is initialized writable
    // storage retained for all subsequent path construction in this function.
    unsafe { get_agg_clause_costs(root, pg_sys::AggSplit::AGGSPLIT_SIMPLE, &raw mut costs) };
    let reconstructed = strategy.and_then(|strategy| {
        // SAFETY: root, output_rel, serial_input, parse, and extra are validated
        // live planner pointers; costs remains initialized, and all list/target
        // arguments belong to this same planner invocation.
        let candidate = unsafe {
            pg_sys::create_agg_path(
                root,
                output_rel,
                serial_input,
                (*output_rel).reltarget,
                strategy,
                pg_sys::AggSplit::AGGSPLIT_SIMPLE,
                (*root).processed_groupClause,
                (*extra).havingQual.cast::<List>(),
                &raw const costs,
                num_groups,
            )
        };
        if candidate.is_null() {
            return None;
        }
        let path = candidate.cast::<pg_sys::Path>();
        // SAFETY: candidate was checked non-null, create_agg_path returns an
        // AggPath, and serial_input/output_rel remain live while it is validated.
        let valid = unsafe {
            (*candidate).aggsplit == pg_sys::AggSplit::AGGSPLIT_SIMPLE
                && (*candidate).subpath == serial_input
                && (*path).parent == output_rel
                && (*path).disabled_nodes <= (*serial_input).disabled_nodes
                && (*path).startup_cost.is_finite()
                && (*path).startup_cost >= 0.0
                && (*path).total_cost.is_finite()
                && (*path).total_cost >= (*path).startup_cost
                && path_tree_is_serial(path, 0)
        };
        // SAFETY: path aliases the non-null candidate and remains live regardless
        // of whether the validation predicate succeeded.
        valid.then(|| unsafe { (*path).total_cost })
    });
    if !grouped {
        return reconstructed;
    }
    // SAFETY: extra is the validated GroupPathExtraData from the callback.
    let sorted_lower_bound = if grouping_capability_enabled(
        unsafe { (*extra).flags } as u32,
        pg_sys::GROUPING_CAN_USE_SORT,
    ) {
        // SAFETY: all pointers and costs were validated above; serial_input is
        // a proven live serial Path and the callee performs its own null checks.
        unsafe {
            serial_sorted_agg_cost_lower_bound(
                root,
                serial_input,
                output_rel,
                extra,
                &raw const costs,
                num_groups,
            )
        }
    } else {
        None
    };
    minimum_valid_cost([reconstructed, sorted_lower_bound])
}

unsafe fn cheapest_native_cost(
    root: *mut pg_sys::PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
    extra: *mut c_void,
    num_groups: f64,
) -> Option<f64> {
    if output_rel.is_null() {
        return None;
    }
    // SAFETY: output_rel is non-null and is the live upper planner relation.
    let pathlist = unsafe { (*output_rel).pathlist };
    // SAFETY: pathlist is either PostgreSQL's null empty-list value or a live
    // planner-owned List.
    let len = unsafe { pg_sys::list_length(pathlist) };
    // SAFETY: every index is bounded by list_length and entries remain live
    // planner Paths throughout selection.
    let existing = unsafe {
        cheapest_serial_agg_path_from_iter(
            (0..len).map(|index| pg_sys::list_nth(pathlist, index).cast()),
        )
    };
    // SAFETY: a non-null existing pointer was selected from the live pathlist
    // after its costs were validated.
    let existing_cost = (!existing.is_null()).then(|| unsafe { (*existing).total_cost });
    // SAFETY: the caller contract supplies planner-owned root/input/output/extra
    // pointers; the callee checks every nullable argument before dereferencing.
    let reconstructed =
        unsafe { reconstructed_serial_agg_cost(root, input_rel, output_rel, extra, num_groups) };
    let serial_cost = minimum_valid_cost([existing_cost, reconstructed]);
    if serial_cost.is_some() {
        return serial_cost;
    }

    // A strongly parallel profile can prune every serial join input before
    // UPPERREL_GROUP_AGG. The already-built parallel aggregate remains a
    // truthful, more competitive comparator; failing admission solely because
    // its serial counterpart was pruned makes count-only resident joins
    // unreachable. This fallback never replaces an available serial baseline.
    // SAFETY: every index is bounded by the pathlist length measured above and
    // every entry remains planner-owned for this callback.
    unsafe {
        cheapest_live_agg_cost_from_iter(
            (0..len).map(|index| pg_sys::list_nth(pathlist, index).cast()),
            output_rel,
        )
    }
}

fn record_decline(decline: &AdmissionDecline, output_rel: *mut RelOptInfo) {
    let _profile = PlannerSubstageGuard::new(stats::PlannerSubstage::RejectionRecording);
    let rows = rel_rows_estimate(output_rel).unwrap_or(0);
    stats::increment_planner_rejected(decline.code(), rows);
}

fn record_cached_decline(reason: &'static str, output_rel: *mut RelOptInfo) {
    let _profile = PlannerSubstageGuard::new(stats::PlannerSubstage::RejectionRecording);
    let rows = rel_rows_estimate(output_rel).unwrap_or(0);
    stats::increment_planner_rejected(reason, rows);
    stats::record_planner_stage_fast_decline(
        stats::PlannerHookStage::UpperGroupAgg,
        "upper_groupagg_cached_decline",
    );
}

fn relation_rows_bits(rel: *mut RelOptInfo) -> u64 {
    if rel.is_null() {
        return f64::NAN.to_bits();
    }
    // SAFETY: callers pass planner-owned RelOptInfo pointers from this hook.
    unsafe { (*rel).rows.to_bits() }
}

fn decline_cache_key(
    query_fingerprint: Rc<QueryFingerprint>,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
    policy: DeclinePolicyKey,
) -> DeclineCacheKey {
    DeclineCacheKey::new(
        query_fingerprint,
        relation_rows_bits(input_rel),
        relation_rows_bits(output_rel),
        policy,
    )
}

/// Analyze and admit one generic descriptor aggregate path.
///
/// # Safety
///
/// All pointers must be the planner-owned arguments for the active
/// `UPPERREL_GROUP_AGG` callback; `extra` must point to GroupPathExtraData.
pub(super) unsafe fn try_inject(
    root: *mut pg_sys::PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
    extra: *mut c_void,
) -> bool {
    if !gucs::gpu_enabled() {
        return false;
    }
    // SAFETY: root is the live PlannerInfo supplied to this hook.
    let query_fingerprint = unsafe { decline_cache::query_fingerprint(root) };
    let structural_cache_key = query_fingerprint.as_ref().map(|fingerprint| {
        decline_cache_key(
            Rc::clone(fingerprint),
            input_rel,
            output_rel,
            DeclinePolicyKey::structural(),
        )
    });
    if let Some(reason) = structural_cache_key
        .as_ref()
        .and_then(decline_cache::lookup)
    {
        record_cached_decline(reason, output_rel);
        return false;
    }
    // SAFETY: root is the live PlannerInfo supplied to this upper-path callback.
    if let Err(decline) = unsafe { super::shape::preflight_base_relations(root) } {
        let decline = AdmissionDecline::Shape(decline);
        if let Some(key) = structural_cache_key {
            decline_cache::insert(key, decline.code(), Vec::new());
        }
        record_decline(&decline, output_rel);
        return false;
    }
    if !cost::gpu_is_usable() {
        return false;
    }
    let model = TypedCostModel::from_limits(cost::device_limits());
    let device_cache_key = query_fingerprint.as_ref().map(|fingerprint| {
        decline_cache_key(
            Rc::clone(fingerprint),
            input_rel,
            output_rel,
            DeclinePolicyKey::device(),
        )
    });
    if let Some(reason) = device_cache_key.as_ref().and_then(decline_cache::lookup) {
        record_cached_decline(reason, output_rel);
        return false;
    }
    // Use the aggregate paths PostgreSQL just built as the single source of
    // truth for both admission and the injected path's output cardinality.
    // SAFETY: output_rel is the live planner-owned upper relation.
    let group_estimate = unsafe { consistent_agg_group_estimate(output_rel) };
    // Include PostgreSQL's exact native comparator in the late cost-decline
    // identity. This retains planner-policy and parameter sensitivity while
    // allowing a hit to skip descriptor and residency reconstruction.
    // SAFETY: all pointers are the live upper-path hook arguments.
    let native_cost = {
        let _profile = PlannerSubstageGuard::new(stats::PlannerSubstage::NativeCostReconstruction);
        group_estimate.and_then(|estimate| unsafe {
            cheapest_native_cost(root, input_rel, output_rel, extra, estimate.num_groups)
        })
    };
    let cost_cache_key = device_cache_key
        .clone()
        .map(|key| key.with_native_cost(native_cost));
    if let Some(reason) = cost_cache_key.as_ref().and_then(decline_cache::lookup) {
        record_cached_decline(reason, output_rel);
        return false;
    }
    let mut cache_dependencies = Vec::new();
    let mut cache_dependencies_valid = true;
    let result = (|| {
        // Use the aggregate paths PostgreSQL just built as the single source of
        // truth for both admission and the injected path's output cardinality.
        // Missing, invalid, or disagreeing estimates fail closed before catalog
        // shape extraction and resident-byte estimation.
        // SAFETY: try_inject's contract supplies the live planner-owned
        // output_rel; the helper checks null and bounds every pathlist access.
        let group_estimate = group_estimate.ok_or(AdmissionDecline::GroupEstimateUnavailable)?;
        // SAFETY: root and output_rel are the live planner-owned pointers supplied
        // together to this upper-path callback.
        let mut shape = unsafe {
            super::shape::extract_shape(root, output_rel, group_estimate.output_rows, &model)
        }
        .map_err(AdmissionDecline::Shape)?;

        #[cfg(any(test, feature = "pg_test"))]
        if test_force_spatial_groupagg(&shape) {
            crate::engine::executor::agg::validate_test_forced_spatial_capability(
                &shape.spec,
                &AggOutputProjection {
                    slots: shape.projections.clone(),
                },
            )
            .map_err(|detail| AdmissionDecline::DescriptorCapability { detail })?;
            if !test_forceable_spatial_cost_gate(shape.cost_gate) {
                return Err(AdmissionDecline::DeviceCostGate(shape.cost_gate));
            }
            let residency_snapshot = exact_residency_estimates(&shape)?;
            let selected_relids = residency_snapshot
                .estimates
                .iter()
                .map(|estimate| estimate.relid)
                .collect::<Vec<_>>();
            let budget_snapshot = resident_budget_snapshot(&selected_relids)
                .ok_or(AdmissionDecline::ResidencyBudgetSnapshotUnavailable)?;
            require_coherent_resident_evidence(&residency_snapshot)?;
            apply_exact_residency(
                &mut shape,
                &residency_snapshot.estimates,
                AdmissionPolicy {
                    auto_load: gucs::auto_load(),
                    budget_bytes: gucs::resident_memory_budget_bytes(),
                    budget_snapshot,
                },
                &model,
            )?;
            shape.cost_gate = ShapeCostGate::Eligible;
            // SAFETY: output_rel remains live and shape has passed capability,
            // residency, and forced-test cost validation.
            let added = unsafe {
                inject_childless_shape_path(
                    output_rel,
                    &shape,
                    EffectivePathCost {
                        startup: 0.0,
                        total: 0.0,
                    },
                )
            }
            .map_err(AdmissionDecline::Shape)?;
            return added.then_some(()).ok_or(AdmissionDecline::PathNotAdded);
        }

        validate_shape_capability(&shape)?;
        if let Some(gate) = stable_device_cost_gate_before_residency(&shape, &model) {
            return Err(AdmissionDecline::DeviceCostGate(gate));
        }
        if let Some((gate, dependency)) = cheap_exact_dense_row_gate(&shape, &model)? {
            cache_dependencies.push(dependency);
            return Err(AdmissionDecline::DeviceCostGate(gate));
        }
        let residency_snapshot = exact_residency_estimates(&shape)?;
        let selected_relids = residency_snapshot
            .estimates
            .iter()
            .map(|estimate| estimate.relid)
            .collect::<Vec<_>>();
        let budget_snapshot = resident_budget_snapshot(&selected_relids)
            .ok_or(AdmissionDecline::ResidencyBudgetSnapshotUnavailable)?;
        require_coherent_resident_evidence(&residency_snapshot)?;
        validate_normal_spatial_residency_envelope(&shape, &residency_snapshot)?;
        if let Some(dependencies) = residency_snapshot
            .requests
            .iter()
            .map(resident_planner_dependency)
            .collect::<Option<Vec<_>>>()
        {
            cache_dependencies = dependencies;
        } else {
            cache_dependencies_valid = false;
        }
        apply_exact_residency(
            &mut shape,
            &residency_snapshot.estimates,
            AdmissionPolicy {
                auto_load: gucs::auto_load(),
                budget_bytes: gucs::resident_memory_budget_bytes(),
                budget_snapshot,
            },
            &model,
        )?;
        let effective_cost = effective_path_cost(&shape, gucs::cost_multiplier());
        gate_cost(
            &shape,
            native_cost,
            maximum_cost_ratio(&shape, &model),
            effective_cost,
        )?;
        // SAFETY: output_rel remains live and shape passed capability, residency,
        // and cost admission before path construction.
        let added = unsafe { inject_childless_shape_path(output_rel, &shape, effective_cost) }
            .map_err(AdmissionDecline::Shape)?;
        if !added {
            return Err(AdmissionDecline::PathNotAdded);
        }
        Ok(())
    })();
    match result {
        Ok(()) => true,
        Err(decline) => {
            let cache_key = match &decline {
                AdmissionDecline::Shape(_)
                | AdmissionDecline::DescriptorCapability { .. }
                | AdmissionDecline::SerialGenericKernelMode { .. } => {
                    structural_cache_key.map(|key| (key, Vec::new()))
                }
                AdmissionDecline::DeviceCostGate(_) if cache_dependencies_valid => {
                    device_cache_key.map(|key| (key, cache_dependencies))
                }
                AdmissionDecline::CostNotCompetitive { .. } if cache_dependencies_valid => {
                    cost_cache_key.map(|key| (key, cache_dependencies))
                }
                _ => None,
            };
            if let Some((key, dependencies)) = cache_key {
                decline_cache::insert(key, decline.code(), dependencies);
            }
            record_decline(&decline, output_rel);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cost::{DeviceLimits, Rows, WorkProduct};
    use crate::engine::ffi::planner_hooks::shape::{
        DescriptorGroupingMode, DescriptorMeasurePlan, DescriptorResolution,
        DictionaryKeyRequirement, RequiredRelation, ShapeCost, dense_atomic_fact_row_floor,
    };
    use crate::engine::residency::ResidentDependencyStamp;
    use crate::engine::spec::{
        AggOutputSlot, AggOutputSource, AggQuerySpec, AggregateKind, AggregateOutput,
        AggregateSource, BinaryMeasureOp, ColumnRef, DimSpec, GroupKeyEncoding, GroupKeyRef,
        GroupKeySource, JoinMultiplicity, MaskKind, MeasureSpec, ScalarRange, ScalarValue,
    };

    fn test_leaf() -> pg_sys::Path {
        pg_sys::Path {
            type_: NodeTag::T_Path,
            ..Default::default()
        }
    }

    fn test_agg(
        subpath: *mut pg_sys::Path,
        cost: f64,
        split: pg_sys::AggSplit::Type,
    ) -> pg_sys::AggPath {
        let mut path = pg_sys::AggPath::default();
        path.path.type_ = NodeTag::T_AggPath;
        path.path.total_cost = cost;
        path.subpath = subpath;
        path.aggsplit = split;
        path
    }

    #[test]
    fn serial_agg_baseline_ignores_cheaper_parallel_finalize_agg() {
        let mut serial_leaf = test_leaf();
        let mut serial_agg = test_agg(
            std::ptr::from_mut(&mut serial_leaf),
            100.0,
            pg_sys::AggSplit::AGGSPLIT_SIMPLE,
        );

        let mut partial_leaf = test_leaf();
        partial_leaf.parallel_aware = true;
        let mut gather = pg_sys::GatherMergePath::default();
        gather.path.type_ = NodeTag::T_GatherMergePath;
        gather.subpath = std::ptr::from_mut(&mut partial_leaf);
        gather.num_workers = 7;
        let mut finalize_agg = test_agg(
            std::ptr::from_mut(&mut gather.path),
            10.0,
            pg_sys::AggSplit::AGGSPLIT_FINAL_DESERIAL,
        );

        // SAFETY: every pointer targets a test-owned path that remains live for
        // this call; each NodeTag and child link was initialized above.
        let selected = unsafe {
            cheapest_serial_agg_path_from_iter([
                std::ptr::from_mut(&mut finalize_agg.path),
                std::ptr::from_mut(&mut serial_agg.path),
            ])
        };
        assert_eq!(selected, std::ptr::from_mut(&mut serial_agg.path));
    }

    #[test]
    fn serial_agg_baseline_rejects_nested_sort_over_gather() {
        let mut parallel_leaf = test_leaf();
        let mut gather = pg_sys::GatherPath::default();
        gather.path.type_ = NodeTag::T_GatherPath;
        gather.subpath = std::ptr::from_mut(&mut parallel_leaf);
        gather.num_workers = 4;
        let mut sort = pg_sys::SortPath::default();
        sort.path.type_ = NodeTag::T_SortPath;
        sort.subpath = std::ptr::from_mut(&mut gather.path);
        let mut nested_parallel_agg = test_agg(
            std::ptr::from_mut(&mut sort.path),
            5.0,
            pg_sys::AggSplit::AGGSPLIT_SIMPLE,
        );

        let mut serial_leaf = test_leaf();
        let mut serial_agg = test_agg(
            std::ptr::from_mut(&mut serial_leaf),
            20.0,
            pg_sys::AggSplit::AGGSPLIT_SIMPLE,
        );
        // SAFETY: every pointer targets a test-owned path that remains live for
        // this call; each NodeTag and child link was initialized above.
        let selected = unsafe {
            cheapest_serial_agg_path_from_iter([
                std::ptr::from_mut(&mut nested_parallel_agg.path),
                std::ptr::from_mut(&mut serial_agg.path),
            ])
        };
        assert_eq!(selected, std::ptr::from_mut(&mut serial_agg.path));
    }

    #[test]
    fn serial_agg_baseline_selects_cheapest_ordinary_serial_agg() {
        let mut first_leaf = test_leaf();
        let mut first = test_agg(
            std::ptr::from_mut(&mut first_leaf),
            50.0,
            pg_sys::AggSplit::AGGSPLIT_SIMPLE,
        );
        let mut second_leaf = test_leaf();
        let mut second = test_agg(
            std::ptr::from_mut(&mut second_leaf),
            25.0,
            pg_sys::AggSplit::AGGSPLIT_SIMPLE,
        );

        // SAFETY: every pointer targets a test-owned path that remains live for
        // this call; each NodeTag and child link was initialized above.
        let selected = unsafe {
            cheapest_serial_agg_path_from_iter([
                std::ptr::from_mut(&mut first.path),
                std::ptr::from_mut(&mut second.path),
            ])
        };
        assert_eq!(selected, std::ptr::from_mut(&mut second.path));
    }

    #[test]
    fn serial_agg_baseline_ignores_invalid_costs() {
        let mut invalid_leaf = test_leaf();
        let mut invalid = test_agg(
            std::ptr::from_mut(&mut invalid_leaf),
            f64::NAN,
            pg_sys::AggSplit::AGGSPLIT_SIMPLE,
        );
        let mut valid_leaf = test_leaf();
        let mut valid = test_agg(
            std::ptr::from_mut(&mut valid_leaf),
            25.0,
            pg_sys::AggSplit::AGGSPLIT_SIMPLE,
        );

        // SAFETY: every pointer targets a test-owned path that remains live for
        // this call; each NodeTag and child link was initialized above.
        let selected = unsafe {
            cheapest_serial_agg_path_from_iter([
                std::ptr::from_mut(&mut invalid.path),
                std::ptr::from_mut(&mut valid.path),
            ])
        };
        assert_eq!(selected, std::ptr::from_mut(&mut valid.path));

        valid.path.startup_cost = 30.0;
        assert!(
            // SAFETY: both pointers target test-owned AggPaths that remain live
            // for this call and have initialized NodeTags and child links.
            unsafe {
                cheapest_serial_agg_path_from_iter([
                    std::ptr::from_mut(&mut invalid.path),
                    std::ptr::from_mut(&mut valid.path),
                ])
            }
            .is_null()
        );
    }

    #[test]
    fn serial_agg_baseline_fails_closed_without_proved_serial_agg() {
        let mut leaf = test_leaf();
        let mut gather = pg_sys::GatherPath::default();
        gather.path.type_ = NodeTag::T_GatherPath;
        gather.subpath = std::ptr::from_mut(&mut leaf);
        gather.num_workers = 2;
        let mut agg = test_agg(
            std::ptr::from_mut(&mut gather.path),
            5.0,
            pg_sys::AggSplit::AGGSPLIT_SIMPLE,
        );

        // SAFETY: agg and its linked test-owned GatherPath remain live for this
        // call, with their NodeTags initialized above.
        let selected =
            unsafe { cheapest_serial_agg_path_from_iter([std::ptr::from_mut(&mut agg.path)]) };
        assert!(selected.is_null());
    }

    #[test]
    fn live_agg_fallback_accepts_parallel_finalize_cost() {
        let mut output_rel = pg_sys::RelOptInfo::default();
        let mut leaf = test_leaf();
        leaf.parallel_aware = true;
        let mut gather = pg_sys::GatherPath::default();
        gather.path.type_ = NodeTag::T_GatherPath;
        gather.path.parallel_workers = 8;
        gather.subpath = std::ptr::from_mut(&mut leaf);
        let mut finalize = test_agg(
            std::ptr::from_mut(&mut gather.path),
            80.0,
            pg_sys::AggSplit::AGGSPLIT_FINAL_DESERIAL,
        );
        finalize.path.parent = std::ptr::from_mut(&mut output_rel);
        finalize.path.startup_cost = 75.0;

        // SAFETY: every pointer targets a test-owned path that remains live for
        // this call; NodeTags, parent, costs, and child links are initialized.
        let cost = unsafe {
            cheapest_live_agg_cost_from_iter(
                [std::ptr::from_mut(&mut finalize.path)],
                std::ptr::from_mut(&mut output_rel),
            )
        };
        assert_eq!(cost, Some(80.0));
    }

    #[test]
    fn live_agg_fallback_rejects_nonaggregate_foreign_and_invalid_paths() {
        let mut output_rel = pg_sys::RelOptInfo::default();
        let mut other_rel = pg_sys::RelOptInfo::default();
        let mut leaf = test_leaf();
        leaf.parent = std::ptr::from_mut(&mut output_rel);

        let mut foreign = test_agg(
            std::ptr::from_mut(&mut leaf),
            10.0,
            pg_sys::AggSplit::AGGSPLIT_SIMPLE,
        );
        foreign.path.parent = std::ptr::from_mut(&mut other_rel);
        let mut invalid = test_agg(
            std::ptr::from_mut(&mut leaf),
            f64::NAN,
            pg_sys::AggSplit::AGGSPLIT_SIMPLE,
        );
        invalid.path.parent = std::ptr::from_mut(&mut output_rel);

        // SAFETY: every pointer targets a test-owned path that remains live for
        // this call and each NodeTag matches its concrete allocation.
        let cost = unsafe {
            cheapest_live_agg_cost_from_iter(
                [
                    std::ptr::from_mut(&mut leaf),
                    std::ptr::from_mut(&mut foreign.path),
                    std::ptr::from_mut(&mut invalid.path),
                ],
                std::ptr::from_mut(&mut output_rel),
            )
        };
        assert_eq!(cost, None);
    }

    #[test]
    fn serial_agg_baseline_rejects_opaque_custom_and_foreign_inputs() {
        let mut custom = pg_sys::CustomPath::default();
        custom.path.type_ = NodeTag::T_CustomPath;
        let mut custom_agg = test_agg(
            std::ptr::from_mut(&mut custom.path),
            10.0,
            pg_sys::AggSplit::AGGSPLIT_SIMPLE,
        );
        let mut foreign = pg_sys::ForeignPath::default();
        foreign.path.type_ = NodeTag::T_ForeignPath;
        let mut foreign_agg = test_agg(
            std::ptr::from_mut(&mut foreign.path),
            20.0,
            pg_sys::AggSplit::AGGSPLIT_SIMPLE,
        );

        // SAFETY: all pointers target test-owned paths that remain live for this
        // call, with matching NodeTags initialized above.
        let selected = unsafe {
            cheapest_serial_agg_path_from_iter([
                std::ptr::from_mut(&mut custom_agg.path),
                std::ptr::from_mut(&mut foreign_agg.path),
            ])
        };
        assert!(selected.is_null());
    }

    #[test]
    fn serial_agg_baseline_rejects_gather_nested_in_join_branch() {
        let mut outer = test_leaf();
        let mut inner = test_leaf();
        let mut gather = pg_sys::GatherPath::default();
        gather.path.type_ = NodeTag::T_GatherPath;
        gather.subpath = std::ptr::from_mut(&mut inner);
        gather.num_workers = 3;
        let mut join = pg_sys::HashPath::default();
        join.jpath.path.type_ = NodeTag::T_HashPath;
        join.jpath.outerjoinpath = std::ptr::from_mut(&mut outer);
        join.jpath.innerjoinpath = std::ptr::from_mut(&mut gather.path);
        let mut agg = test_agg(
            std::ptr::from_mut(&mut join.jpath.path),
            15.0,
            pg_sys::AggSplit::AGGSPLIT_SIMPLE,
        );

        // SAFETY: agg and its linked test-owned join tree remain live for this
        // call, with every concrete path tagged above.
        let selected =
            unsafe { cheapest_serial_agg_path_from_iter([std::ptr::from_mut(&mut agg.path)]) };
        assert!(selected.is_null());
    }

    #[test]
    fn reconstructed_baseline_selects_cheapest_proved_serial_input() {
        let mut rel = pg_sys::RelOptInfo::default();
        let mut target = pg_sys::PathTarget::default();
        rel.reltarget = std::ptr::from_mut(&mut target);

        let mut first = test_leaf();
        first.parent = std::ptr::from_mut(&mut rel);
        first.pathtarget = rel.reltarget;
        first.startup_cost = 0.0;
        first.total_cost = 80.0;
        first.rows = 1_000.0;
        let mut second = test_leaf();
        second.parent = std::ptr::from_mut(&mut rel);
        second.pathtarget = rel.reltarget;
        second.startup_cost = 2.0;
        second.total_cost = 40.0;
        second.rows = 1_000.0;
        let mut gather = pg_sys::GatherPath::default();
        gather.path.type_ = NodeTag::T_GatherPath;
        gather.path.parent = std::ptr::from_mut(&mut rel);
        gather.path.pathtarget = rel.reltarget;
        gather.path.startup_cost = 1.0;
        gather.path.total_cost = 5.0;
        gather.path.rows = 1_000.0;

        // SAFETY: every pointer targets a test-owned Path or RelOptInfo that
        // remains live for this call, with parent/target links initialized above.
        let selected = unsafe {
            cheapest_serial_input_path_from_iter(
                [
                    std::ptr::from_mut(&mut first),
                    std::ptr::from_mut(&mut gather.path),
                    std::ptr::from_mut(&mut second),
                ],
                std::ptr::from_mut(&mut rel),
            )
        };
        assert_eq!(selected, std::ptr::from_mut(&mut second));
    }

    #[test]
    fn reconstructed_baseline_requires_consistent_live_group_estimate() {
        let mut rel = pg_sys::RelOptInfo::default();
        let mut leaf = test_leaf();
        let mut first = test_agg(
            std::ptr::from_mut(&mut leaf),
            10.0,
            pg_sys::AggSplit::AGGSPLIT_FINAL_DESERIAL,
        );
        first.path.parent = std::ptr::from_mut(&mut rel);
        first.numGroups = 17.0;
        let mut second = test_agg(
            std::ptr::from_mut(&mut leaf),
            20.0,
            pg_sys::AggSplit::AGGSPLIT_SIMPLE,
        );
        second.path.parent = std::ptr::from_mut(&mut rel);
        second.numGroups = 17.0;

        // SAFETY: both pointers target test-owned AggPaths with matching tags and
        // a shared live test-owned RelOptInfo parent.
        let consistent = unsafe {
            consistent_agg_num_groups_from_iter(
                [
                    std::ptr::from_mut(&mut first.path),
                    std::ptr::from_mut(&mut second.path),
                ],
                std::ptr::from_mut(&mut rel),
            )
        };
        assert_eq!(consistent, Some(17.0));

        second.numGroups = 18.0;
        // SAFETY: both pointers target test-owned AggPaths with matching tags and
        // a shared live test-owned RelOptInfo parent.
        let inconsistent = unsafe {
            consistent_agg_num_groups_from_iter(
                [
                    std::ptr::from_mut(&mut first.path),
                    std::ptr::from_mut(&mut second.path),
                ],
                std::ptr::from_mut(&mut rel),
            )
        };
        assert_eq!(inconsistent, None);

        // SAFETY: leaf and rel are test-owned and live for this call; leaf's
        // non-AggPath NodeTag was initialized by test_leaf.
        let absent = unsafe {
            consistent_agg_num_groups_from_iter(
                [std::ptr::from_mut(&mut leaf)],
                std::ptr::from_mut(&mut rel),
            )
        };
        assert_eq!(absent, None);

        second.numGroups = 0.0;
        // SAFETY: second is a live test-owned AggPath with its matching NodeTag
        // and test-owned RelOptInfo parent initialized above.
        let nonpositive = unsafe {
            consistent_agg_num_groups_from_iter(
                [std::ptr::from_mut(&mut second.path)],
                std::ptr::from_mut(&mut rel),
            )
        };
        assert_eq!(nonpositive, None);
    }

    #[test]
    fn aggregate_group_estimate_rounds_up_and_rejects_invalid_values() {
        assert_eq!(conservative_output_rows(64.0), Some(64));
        assert_eq!(conservative_output_rows(127.01), Some(128));
        assert_eq!(conservative_output_rows(f64::MIN_POSITIVE), Some(1));
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(conservative_output_rows(invalid), None);
        }
    }

    #[test]
    fn native_baseline_uses_cheapest_valid_serial_cost() {
        assert_eq!(minimum_valid_cost([Some(90.0), Some(40.0)]), Some(40.0));
        assert_eq!(minimum_valid_cost([Some(25.0), None]), Some(25.0));
        assert_eq!(minimum_valid_cost([Some(f64::NAN), Some(30.0)]), Some(30.0));
        assert_eq!(minimum_valid_cost([Some(-1.0), None]), None);
    }

    #[test]
    fn spilling_hash_reconstruction_cannot_inflate_serial_baseline() {
        let spilling_hash_cost = Some(9_000.0);
        let sorted_no_ordering_lower_bound = Some(1_200.0);

        assert_eq!(
            minimum_valid_cost([spilling_hash_cost, sorted_no_ordering_lower_bound]),
            sorted_no_ordering_lower_bound
        );
    }

    #[test]
    fn sorted_lower_bound_requires_sort_grouping_capability() {
        let hash_only = pg_sys::GROUPING_CAN_USE_HASH;
        assert!(!grouping_capability_enabled(
            hash_only,
            pg_sys::GROUPING_CAN_USE_SORT
        ));
        assert!(grouping_capability_enabled(
            hash_only | pg_sys::GROUPING_CAN_USE_SORT,
            pg_sys::GROUPING_CAN_USE_SORT
        ));
    }

    fn count_contract() -> (AggQuerySpec, AggOutputProjection) {
        let spec = AggQuerySpec {
            fact_rel: 42,
            group_keys: Vec::new(),
            measures: vec![MeasureSpec {
                expression: MeasureExpr::CountStar,
                outputs: vec![AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                }],
                filter: FilterSpec::None,
            }],
            fact_filter: FilterSpec::None,
            star_dims: Vec::new(),
            having: None,
        };
        let projection = AggOutputProjection {
            slots: vec![AggOutputSlot {
                source: AggOutputSource::Aggregate {
                    measure_index: 0,
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                },
                source_type_oid: 0,
                result_type_oid: u32::from(pg_sys::INT8OID),
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: false,
            }],
        };
        (spec, projection)
    }

    fn count_shape() -> ShapePlan {
        let (spec, projection) = count_contract();
        let mut digest_words = spec.encode_i32().expect("spec encodes");
        digest_words.extend(projection.encode_i32(&spec).expect("projection encodes"));
        let zero_cost = ShapeCost {
            fact_scan: PgCost::new(10.0),
            dimension_setup: PgCost::new(0.0),
            join_probe: PgCost::new(0.0),
            spatial_filter: PgCost::new(0.0),
            spatial_recheck_reserve: PgCost::new(0.0),
            aggregate: PgCost::new(5.0),
            additional_aggregate_launches: PgCost::ZERO,
            output_materialization: PgCost::new(1.0),
            amortized_auto_load: PgCost::new(0.0),
            total: PgCost::new(16.0),
        };
        ShapePlan {
            spec,
            projections: projection.slots,
            required_relations: vec![RequiredRelation {
                relation_oid: 42,
                attnos: Vec::new(),
            }],
            estimated_output_rows: 1,
            digest_words,
            descriptor_resolution: DescriptorResolution::Ready,
            descriptor_measures: DescriptorMeasurePlan {
                projected_measure_count: 1,
                descriptor_measure_count: 1,
                fact_filter: None,
                derived_fact_mask: None,
                derived_spatial_mask: false,
            },
            residency: ResidencyEstimate {
                relations: vec![RelationResidencyRequirement {
                    relation_oid: 42,
                    attnos: Vec::new(),
                    state: RelationResidency::Unknown,
                    estimated_rows: 100_000,
                    estimated_bytes: None,
                }],
                total_required_bytes: None,
                missing_bytes: None,
                missing_rows: 100_000,
                expected_reuses: NonZeroU32::new(4).expect("nonzero"),
                amortized_load_cost: PgCost::new(0.0),
            },
            cost: zero_cost,
            cost_gate: ShapeCostGate::Eligible,
        }
    }

    #[test]
    fn childless_path_rows_follow_the_shape_group_estimate() {
        let mut shape = grouped_count_shape();
        shape.estimated_output_rows = 64;
        assert_eq!(advertised_output_rows(&shape), 64.0);
        shape.estimated_output_rows = 128;
        assert_eq!(advertised_output_rows(&shape), 128.0);
    }

    #[test]
    fn normal_spatial_row_envelope_requires_exact_loaded_release_evidence() {
        assert!(normal_spatial_promotion_rows(Some(
            NORMAL_SPATIAL_PROMOTION_ROWS
        )));
        assert!(!normal_spatial_promotion_rows(None));
        assert!(!normal_spatial_promotion_rows(Some(
            NORMAL_SPATIAL_PROMOTION_ROWS - 1
        )));
        assert!(!normal_spatial_promotion_rows(Some(
            NORMAL_SPATIAL_PROMOTION_ROWS + 1
        )));
    }

    fn estimate(loaded: bool, bytes: u64) -> ResidentLoadEstimate {
        estimate_for(42, loaded, bytes, true)
    }

    fn estimate_for(
        relation_oid: u32,
        loaded: bool,
        bytes: u64,
        fixed_width: bool,
    ) -> ResidentLoadEstimate {
        ResidentLoadEstimate {
            relid: pg_sys::Oid::from(relation_oid),
            loaded,
            resident_dependency: loaded.then_some(ResidentDependencyStamp {
                relid: pg_sys::Oid::from(relation_oid),
                generation: 1,
                global_generation: 1,
                relfilenode: pg_sys::Oid::from(relation_oid + 100),
            }),
            resident_selected_columns: loaded.then_some(vec![1]),
            resident_rows: loaded.then_some(100_000),
            selected_columns_have_no_null_sidecars: loaded.then_some(true),
            pinned: false,
            estimated_bytes: bytes,
            fixed_width,
            last_load_ms: None,
            amortization_queries: 4,
        }
    }

    fn model() -> TypedCostModel {
        let mut limits = DeviceLimits::cpu_only();
        limits.resident_load_scan_per_row_cost = 0.004;
        limits.resident_load_per_byte_cost = 0.001;
        TypedCostModel::from_limits(&limits)
    }

    fn expected_exact_load(rows: u64, bytes: u64, fixed_width: bool) -> PgCost {
        let model = model();
        PgCost::new(unamortized_resident_load_cost(rows, bytes, fixed_width, &model) / 4.0)
    }

    fn budget_snapshot(
        other_backend_bytes: u64,
        pinned_unselected_raw_bytes: u64,
        evictable_or_replaced_local_bytes: u64,
    ) -> ResidentBudgetSnapshot {
        let current_backend_live_bytes = pinned_unselected_raw_bytes
            .checked_add(evictable_or_replaced_local_bytes)
            .expect("test snapshot fits");
        ResidentBudgetSnapshot {
            cluster_live_bytes: other_backend_bytes
                .checked_add(current_backend_live_bytes)
                .expect("test snapshot fits"),
            current_backend_live_bytes,
            other_backend_live_bytes: other_backend_bytes,
            pinned_unselected_raw_bytes,
            evictable_or_replaced_local_bytes,
        }
    }

    fn grouped_count_shape() -> ShapePlan {
        let mut shape = count_shape();
        shape.spec.group_keys.push(GroupKeyRef {
            source: GroupKeySource::FactColumn(ColumnRef {
                relation_oid: 42,
                attno: 1,
                type_oid: u32::from(pg_sys::INT4OID),
            }),
            type_oid: u32::from(pg_sys::INT4OID),
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        });
        shape.required_relations[0].attnos.push(1);
        shape.residency.relations[0].attnos.push(1);
        shape
    }

    fn dense_sum_count_shape(estimated_fact_rows: u64) -> ShapePlan {
        let mut shape = grouped_count_shape();
        let sum_column = ColumnRef {
            relation_oid: 42,
            attno: 2,
            type_oid: u32::from(pg_sys::INT4OID),
        };
        shape.spec.measures = vec![
            MeasureSpec {
                expression: MeasureExpr::Column(sum_column),
                outputs: vec![AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Sum,
                }],
                filter: FilterSpec::None,
            },
            MeasureSpec {
                expression: MeasureExpr::CountStar,
                outputs: vec![AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                }],
                filter: FilterSpec::None,
            },
        ];
        shape.required_relations[0].attnos = vec![1, 2, 3];
        shape.residency.relations[0].attnos = vec![1, 2, 3];
        shape.residency.relations[0].estimated_rows = estimated_fact_rows;
        shape.descriptor_resolution = DescriptorResolution::BeginTimeArtifacts {
            dictionary_keys: vec![DictionaryKeyRequirement {
                key_index: 0,
                source: shape.spec.group_keys[0].source.clone(),
                collation_oid: 0,
            }],
            derived_keys: Vec::new(),
            joins: Vec::new(),
            grouping_mode: DescriptorGroupingMode::DenseDictionary,
            max_group_count: 10_000,
        };
        shape.descriptor_measures.projected_measure_count = 2;
        shape.descriptor_measures.descriptor_measure_count = 2;
        let hash_cost =
            estimated_fact_rows as f64 * model().coefficients.gpu_op_cost_hash_agg.get() * 2.0;
        shape.cost.replace_aggregate(PgCost::new(hash_cost));
        shape
    }

    fn apply_resident_evidence(shape: &mut ShapePlan, resident_rows: u64, no_null_sidecars: bool) {
        let mut evidence = estimate(true, 4_096);
        evidence.resident_rows = Some(resident_rows);
        evidence.selected_columns_have_no_null_sidecars = Some(no_null_sidecars);
        apply_exact_residency(
            shape,
            &[evidence],
            AdmissionPolicy {
                auto_load: false,
                budget_bytes: u64::MAX,
                budget_snapshot: budget_snapshot(0, 0, 0),
            },
            &model(),
        )
        .expect("resident fixture fits the unbounded test budget");
    }

    #[test]
    fn dense_atomic_cost_matches_exact_sum_count_operation_count() {
        let model = model();
        let mut shape = dense_sum_count_shape(1_000_000);
        let direct = dense_atomic_sum_count_cost(
            &shape.spec,
            &shape.descriptor_resolution,
            1_000_000,
            Some(1_000_000),
            Some(true),
            &model,
        )
        .expect("exact resident dense SUM/COUNT is atomic one-shot");
        assert_eq!(
            direct,
            PgCost::new(1_000_000.0 * model.coefficients.gpu_op_cost_reduce.get() * 2.0)
        );

        let high_estimate = dense_atomic_sum_count_cost(
            &shape.spec,
            &shape.descriptor_resolution,
            2_000_000,
            Some(1_000_000),
            Some(true),
            &model,
        )
        .expect("exact resident rows, not a high estimate, size atomic work");
        assert_eq!(high_estimate, direct);

        let lhs = ColumnRef {
            relation_oid: 42,
            attno: 2,
            type_oid: u32::from(pg_sys::INT4OID),
        };
        let rhs = ColumnRef {
            relation_oid: 42,
            attno: 3,
            type_oid: u32::from(pg_sys::INT4OID),
        };
        shape.spec.measures[0].expression = MeasureExpr::Binary {
            op: BinaryMeasureOp::Mul,
            lhs,
            rhs,
        };
        let product = dense_atomic_sum_count_cost(
            &shape.spec,
            &shape.descriptor_resolution,
            1_000_000,
            Some(1_000_000),
            Some(true),
            &model,
        )
        .expect("nonnull int4 product SUM/COUNT is atomic one-shot");
        let expected_product = 1_000_000.0
            * model
                .coefficients
                .gpu_op_cost_reduce
                .get()
                .mul_add(2.0, model.coefficients.gpu_op_cost_filter.get());
        assert_eq!(product, PgCost::new(expected_product));
    }

    #[test]
    fn dense_atomic_measure_filter_costs_two_comparisons_and_independent_sum_state() {
        let model = model();
        let mut shape = dense_sum_count_shape(1_000_000);
        let value = match shape.spec.measures[0].expression {
            MeasureExpr::Column(value) => value,
            ref expression => panic!("expected direct column measure, got {expression:?}"),
        };
        shape.spec.measures[0].filter = FilterSpec::Ranges {
            input: value,
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(200),
                hi: ScalarValue::I32(800),
            }],
        };

        let filtered = dense_atomic_sum_count_cost(
            &shape.spec,
            &shape.descriptor_resolution,
            1_000_000,
            Some(1_000_000),
            Some(false),
            &model,
        )
        .expect("exact bounded int4 aggregate FILTER has a dense atomic cost");
        let expected_per_row = model
            .coefficients
            .gpu_op_cost_reduce
            .get()
            .mul_add(3.0, model.coefficients.gpu_op_cost_filter.get() * 2.0);
        assert_eq!(filtered, PgCost::new(1_000_000.0 * expected_per_row));

        shape.spec.measures[0].filter = FilterSpec::Ranges {
            input: value,
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(i32::MIN),
                hi: ScalarValue::I32(800),
            }],
        };
        assert!(
            dense_atomic_sum_count_cost(
                &shape.spec,
                &shape.descriptor_resolution,
                1_000_000,
                Some(1_000_000),
                Some(false),
                &model,
            )
            .is_none(),
            "one-sided ranges must not inherit the released filtered cost"
        );
    }

    #[test]
    fn dense_atomic_exact_rows_must_clear_the_applicable_shape_floor() {
        let model = model();
        let shape = dense_sum_count_shape(10_000_000);
        let grouped_floor = dense_atomic_fact_row_floor(&shape.spec, &model);
        assert_eq!(grouped_floor, model.planner.gpu_hash_agg_min_rows);
        let below_floor = u64::try_from(grouped_floor.get() - 1).expect("test floor fits u64");
        let at_floor = u64::try_from(grouped_floor.get()).expect("test floor fits u64");

        let cost = |resident_rows| {
            dense_atomic_sum_count_cost(
                &shape.spec,
                &shape.descriptor_resolution,
                10_000_000,
                Some(resident_rows),
                Some(true),
                &model,
            )
        };
        assert!(
            cost(below_floor).is_none(),
            "a stale-high reltuples estimate cannot satisfy the exact-row floor"
        );
        assert!(cost(at_floor).is_some(), "the exact floor is inclusive");

        let mut global = shape.clone();
        global.spec.group_keys.clear();
        global.spec.star_dims.clear();
        assert_eq!(
            dense_atomic_fact_row_floor(&global.spec, &model),
            model.planner.gpu_reduce_min_rows
        );

        let mut star = shape;
        add_star_dimension(&mut star, JoinMultiplicity::Unique);
        assert_eq!(
            dense_atomic_fact_row_floor(&star.spec, &model),
            model.planner.gpu_preagg_min_fact_rows
        );
    }

    #[test]
    fn dense_sum_count_caps_admission_but_costs_bounded_calls_above_one_shot() {
        let model = model();
        let maximum = model.executor.gpu_grouped_agg_one_shot_max_rows;
        let maximum_u64 = u64::try_from(maximum.get()).expect("test row limit fits u64");

        let mut at_maximum = dense_sum_count_shape(maximum_u64);
        apply_resident_evidence(&mut at_maximum, maximum_u64, true);
        assert_eq!(at_maximum.cost_gate, ShapeCostGate::Eligible);
        assert_eq!(at_maximum.cost.additional_aggregate_launches, PgCost::ZERO);

        let above_u64 = maximum_u64 + 1;
        let mut above_maximum = dense_sum_count_shape(above_u64);
        apply_resident_evidence(&mut above_maximum, above_u64, true);
        assert_eq!(
            above_maximum.cost_gate,
            ShapeCostGate::DenseOneShotRowsExceedDeviceMaximum {
                fact_rows: Rows::new(maximum.get() + 1),
                maximum,
            }
        );
        assert_eq!(
            planned_dense_lifecycle_calls(&above_maximum, above_u64, &model),
            Some(3),
            "two bounded accumulates plus finalize"
        );
        assert_eq!(
            above_maximum.cost.additional_aggregate_launches,
            PgCost::new(2.0 * cost::GPU_LAUNCH_OVERHEAD)
        );

        let mut first_use = dense_sum_count_shape(above_u64);
        apply_exact_residency(
            &mut first_use,
            &[estimate(false, 4_096)],
            AdmissionPolicy {
                auto_load: true,
                budget_bytes: u64::MAX,
                budget_snapshot: budget_snapshot(0, 0, 0),
            },
            &model,
        )
        .expect("first-use estimate fits the unbounded test budget");
        assert_eq!(
            first_use.cost_gate,
            ShapeCostGate::DenseOneShotRowsExceedDeviceMaximum {
                fact_rows: Rows::new(maximum.get() + 1),
                maximum,
            }
        );
        assert_eq!(
            first_use.cost.additional_aggregate_launches,
            above_maximum.cost.additional_aggregate_launches,
            "first-use admission costs the estimate-derived lifecycle"
        );

        let ten_million = dense_sum_count_shape(10_000_000);
        assert_eq!(
            planned_dense_lifecycle_calls(&ten_million, 10_000_000, &model),
            Some(11),
            "ten 1M accumulates plus finalize"
        );

        let mut star = dense_sum_count_shape(above_u64);
        add_star_dimension(&mut star, JoinMultiplicity::Unique);
        assert!(dense_atomic_sum_count_lifecycle(
            &star.spec,
            &star.descriptor_resolution,
            &model
        ));
        let count_only = count_shape();
        assert!(!dense_atomic_sum_count_lifecycle(
            &count_only.spec,
            &count_only.descriptor_resolution,
            &model
        ));
        assert!(!dense_atomic_sum_count_lifecycle(
            &h3_parent_shape().spec,
            &h3_parent_shape().descriptor_resolution,
            &model
        ));
    }

    #[test]
    fn exact_resident_rows_replace_only_the_generic_dense_row_gate() {
        let model = model();
        let floor = dense_atomic_fact_row_floor(&dense_sum_count_shape(1).spec, &model);
        let below_floor = floor.get() - 1;
        let at_floor = floor.get();

        let mut stale_high = dense_sum_count_shape(10_000_000);
        apply_resident_evidence(
            &mut stale_high,
            u64::try_from(below_floor).expect("test floor fits u64"),
            true,
        );
        assert_eq!(
            stale_high.cost_gate,
            ShapeCostGate::FactRowsBelowDeviceMinimum {
                estimated: Rows::new(below_floor),
                required: floor,
            },
            "exact small residency must replace a stale-high eligible estimate"
        );

        let mut stale_low =
            dense_sum_count_shape(u64::try_from(below_floor).expect("test floor fits u64"));
        stale_low.cost_gate = ShapeCostGate::FactRowsBelowDeviceMinimum {
            estimated: Rows::new(below_floor),
            required: floor,
        };
        apply_resident_evidence(
            &mut stale_low,
            u64::try_from(at_floor).expect("test floor fits u64"),
            true,
        );
        assert_eq!(
            stale_low.cost_gate,
            ShapeCostGate::Eligible,
            "exact in-band residency must replace a stale-low row-floor estimate"
        );

        let unrelated_gate = ShapeCostGate::GroupsExceedDeviceMaximum {
            estimated: Rows::new(11),
            maximum: Rows::new(10),
        };
        let mut unrelated =
            dense_sum_count_shape(u64::try_from(at_floor).expect("test floor fits u64"));
        unrelated.cost_gate = unrelated_gate;
        apply_resident_evidence(
            &mut unrelated,
            u64::try_from(at_floor).expect("test floor fits u64"),
            true,
        );
        assert_eq!(unrelated.cost_gate, unrelated_gate);
    }

    #[test]
    fn cheap_exact_rows_decline_below_floor_dense_and_star_before_full_estimation() {
        let model = model();
        for star in [false, true] {
            let mut shape = dense_sum_count_shape(1);
            if star {
                add_star_dimension(&mut shape, JoinMultiplicity::Unique);
            }
            let floor = dense_atomic_fact_row_floor(&shape.spec, &model);
            let below = floor.get() - 1;
            shape.cost_gate = ShapeCostGate::FactRowsBelowDeviceMinimum {
                estimated: Rows::new(below),
                required: floor,
            };
            let gate = cheap_exact_dense_row_gate_with(&shape, &model, |request| {
                assert_eq!(u32::from(request.relid), shape.spec.fact_rel);
                Some(u64::try_from(below).expect("test floor fits u64"))
            })
            .expect("fact request is representable");
            assert_eq!(
                gate,
                Some(ShapeCostGate::FactRowsBelowDeviceMinimum {
                    estimated: Rows::new(below),
                    required: floor,
                })
            );
        }
    }

    #[test]
    fn cheap_exact_rows_fall_through_when_missing_or_at_floor() {
        let model = model();
        let mut shape = dense_sum_count_shape(1);
        let floor = dense_atomic_fact_row_floor(&shape.spec, &model);
        shape.cost_gate = ShapeCostGate::FactRowsBelowDeviceMinimum {
            estimated: Rows::new(floor.get() - 1),
            required: floor,
        };
        assert_eq!(
            cheap_exact_dense_row_gate_with(&shape, &model, |_| None)
                .expect("missing evidence falls through"),
            None
        );
        assert_eq!(
            cheap_exact_dense_row_gate_with(&shape, &model, |_| {
                Some(u64::try_from(floor.get()).expect("test floor fits u64"))
            })
            .expect("in-band evidence falls through"),
            None
        );

        let mut unsupported = count_shape();
        unsupported.cost_gate = ShapeCostGate::FactRowsBelowDeviceMinimum {
            estimated: Rows::new(1),
            required: Rows::new(2),
        };
        assert_eq!(
            cheap_exact_dense_row_gate_with(&unsupported, &model, |_| {
                panic!("unsupported lifecycle must not inspect residency")
            })
            .expect("unsupported lifecycle is unchanged"),
            None
        );
    }

    #[test]
    fn dense_atomic_cost_requires_runtime_proof_boundaries() {
        let model = model();
        let mut shape = dense_sum_count_shape(1_000_000);
        let cost = |shape: &ShapePlan, rows, no_nulls| {
            dense_atomic_sum_count_cost(
                &shape.spec,
                &shape.descriptor_resolution,
                1_000_000,
                rows,
                no_nulls,
                &model,
            )
        };
        assert!(cost(&shape, None, Some(true)).is_none());
        assert!(cost(&shape, Some(1_000_000), None).is_none());
        assert!(cost(&shape, Some(1_000_001), Some(true)).is_none());
        let nullable = cost(&shape, Some(1_000_000), Some(false))
            .expect("nullable direct SUM retains its nonnull atomic update");
        assert_eq!(
            nullable,
            PgCost::new(1_000_000.0 * model.coefficients.gpu_op_cost_reduce.get() * 3.0)
        );

        shape.spec.measures[0].expression = MeasureExpr::Binary {
            op: BinaryMeasureOp::Mul,
            lhs: ColumnRef {
                relation_oid: 42,
                attno: 2,
                type_oid: u32::from(pg_sys::INT4OID),
            },
            rhs: ColumnRef {
                relation_oid: 42,
                attno: 3,
                type_oid: u32::from(pg_sys::INT4OID),
            },
        };
        assert!(
            cost(&shape, Some(1_000_000), Some(false)).is_none(),
            "nullable product operands retain ordinary hash cost"
        );

        shape.spec.measures[0].expression = MeasureExpr::Column(ColumnRef {
            relation_oid: 42,
            attno: 2,
            type_oid: u32::from(pg_sys::INT4OID),
        });
        shape.spec.measures[0].outputs.push(AggregateOutput {
            source: AggregateSource::Value,
            kind: AggregateKind::Min,
        });
        assert!(cost(&shape, Some(1_000_000), Some(true)).is_none());
        shape.spec.measures[0].outputs.pop();

        shape.spec.fact_filter = FilterSpec::Mask {
            input: ColumnRef {
                relation_oid: 42,
                attno: 4,
                type_oid: u32::from(pg_sys::BOOLOID),
            },
            kind: MaskKind::Sql,
        };
        assert!(cost(&shape, Some(1_000_000), Some(true)).is_some());
        shape.spec.fact_filter = FilterSpec::Mask {
            input: ColumnRef {
                relation_oid: 42,
                attno: 4,
                type_oid: u32::from(pg_sys::BOOLOID),
            },
            kind: MaskKind::Recheck,
        };
        assert!(cost(&shape, Some(1_000_000), Some(true)).is_none());

        shape.spec.fact_filter = FilterSpec::None;
        add_star_dimension(&mut shape, JoinMultiplicity::Counted);
        assert!(cost(&shape, Some(1_000_000), Some(true)).is_none());
    }

    #[test]
    fn exact_residency_never_uses_an_optimistic_row_estimate_for_atomic_cost() {
        let model = model();
        let mut exact = dense_sum_count_shape(1_000_000);
        apply_resident_evidence(&mut exact, 1_000_000, true);
        assert_eq!(
            exact.cost.aggregate,
            PgCost::new(1_000_000.0 * model.coefficients.gpu_op_cost_reduce.get() * 2.0)
        );

        let mut underestimated = dense_sum_count_shape(100_000);
        let ordinary = underestimated.cost.aggregate;
        apply_resident_evidence(&mut underestimated, 1_000_000, true);
        assert_eq!(
            underestimated.cost.aggregate, ordinary,
            "a low catalog estimate cannot unlock or size the one-shot discount"
        );

        let exact_floor = model.planner.gpu_hash_agg_min_rows.get() as u64;
        let mut below_floor = dense_sum_count_shape(10_000_000);
        let ordinary = below_floor.cost.aggregate;
        apply_resident_evidence(&mut below_floor, exact_floor - 1, true);
        assert_eq!(
            below_floor.cost.aggregate, ordinary,
            "a stale-high estimate cannot unlock a discount below the exact-row floor"
        );

        let mut at_floor = dense_sum_count_shape(10_000_000);
        apply_resident_evidence(&mut at_floor, exact_floor, true);
        assert_eq!(
            at_floor.cost.aggregate,
            PgCost::new(exact_floor as f64 * model.coefficients.gpu_op_cost_reduce.get() * 2.0),
            "the exact-row floor is inclusive"
        );

        let mut over_cap = dense_sum_count_shape(1_000_001);
        apply_resident_evidence(&mut over_cap, 1_000_001, true);
        assert_eq!(
            over_cap.cost.aggregate,
            PgCost::new(1_000_001.0 * model.coefficients.gpu_op_cost_reduce.get() * 2.0),
            "bounded sessions retain the exact operation-count cost"
        );
        assert_eq!(
            over_cap.cost.additional_aggregate_launches,
            PgCost::new(2.0 * cost::GPU_LAUNCH_OVERHEAD),
            "two accumulate calls plus finalize are charged beyond the base launch"
        );
    }

    fn h3_parent_shape() -> ShapePlan {
        const H3INDEXOID: u32 = 90_001;
        let mut shape = grouped_count_shape();
        shape.spec.group_keys[0] = GroupKeyRef {
            source: GroupKeySource::H3CellToParent {
                cell: ColumnRef {
                    relation_oid: 42,
                    attno: 1,
                    type_oid: H3INDEXOID,
                },
                resolution: 4,
            },
            type_oid: H3INDEXOID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        };
        shape
    }

    fn integer_measure_shape(kind: AggregateKind) -> ShapePlan {
        let mut shape = count_shape();
        shape.spec.measures[0] = MeasureSpec {
            expression: MeasureExpr::Column(ColumnRef {
                relation_oid: 42,
                attno: 2,
                type_oid: u32::from(pg_sys::INT4OID),
            }),
            outputs: vec![AggregateOutput {
                source: AggregateSource::Value,
                kind,
            }],
            filter: FilterSpec::None,
        };
        shape.projections[0] = AggOutputSlot {
            source: AggOutputSource::Aggregate {
                measure_index: 0,
                source: AggregateSource::Value,
                kind,
            },
            source_type_oid: u32::from(pg_sys::INT4OID),
            result_type_oid: u32::from(
                if matches!(kind, AggregateKind::Sum | AggregateKind::Count) {
                    pg_sys::INT8OID
                } else {
                    pg_sys::INT4OID
                },
            ),
            result_typmod: -1,
            result_collation_oid: 0,
            nullable: kind != AggregateKind::Count,
        };
        shape.required_relations[0].attnos = vec![2];
        shape.residency.relations[0].attnos = vec![2];
        shape
    }

    fn add_star_dimension(shape: &mut ShapePlan, multiplicity: JoinMultiplicity) {
        shape.spec.star_dims.push(DimSpec {
            relation_oid: 99,
            fact_key: ColumnRef {
                relation_oid: 42,
                attno: 1,
                type_oid: u32::from(pg_sys::INT4OID),
            },
            dim_key: ColumnRef {
                relation_oid: 99,
                attno: 1,
                type_oid: u32::from(pg_sys::INT4OID),
            },
            collation_oid: 0,
            multiplicity,
            filter: FilterSpec::None,
        });
    }

    #[test]
    fn selected_relation_preserves_exact_attribute_order() {
        let selected = selected_relation(&RequiredRelation {
            relation_oid: 42,
            attnos: vec![1, 3, 7],
        })
        .expect("valid attnos");
        assert_eq!(selected.relid, pg_sys::Oid::from(42u32));
        assert_eq!(selected.columns, [1, 3, 7]);
        assert_eq!(
            selected_relation(&RequiredRelation {
                relation_oid: 42,
                attnos: vec![i32::MAX],
            })
            .expect_err("oversized attno declines")
            .code(),
            "generic_residency_attno_out_of_range"
        );
    }

    #[test]
    fn exact_residency_estimate_batch_suspends_planner_hooks() {
        let shape = count_shape();
        let snapshot = exact_residency_estimates_with(&shape, |request| {
            assert!(super::super::planner_hooks_suspended());
            Ok::<_, std::convert::Infallible>(ResidentLoadEstimate {
                relid: request.relid,
                loaded: false,
                resident_dependency: None,
                resident_selected_columns: None,
                resident_rows: None,
                selected_columns_have_no_null_sidecars: None,
                pinned: false,
                estimated_bytes: 4096,
                fixed_width: true,
                last_load_ms: None,
                amortization_queries: 4,
            })
        })
        .expect("mock estimate succeeds");
        assert_eq!(snapshot.estimates, [estimate(false, 4096)]);
        assert_eq!(snapshot.requests.len(), 1);
        assert_eq!(snapshot.requests[0].relid, pg_sys::Oid::from(42_u32));
        assert!(snapshot.requests[0].columns.is_empty());
        assert!(!super::super::planner_hooks_suspended());
    }

    #[test]
    fn exact_residency_marks_hits_resident_without_load_cost() {
        let mut shape = count_shape();
        apply_exact_residency(
            &mut shape,
            &[estimate(true, 4096)],
            AdmissionPolicy {
                auto_load: false,
                budget_bytes: 4096,
                budget_snapshot: budget_snapshot(0, 0, 4096),
            },
            &model(),
        )
        .expect("resident hit fits its exact final footprint");
        assert_eq!(
            shape.residency.relations[0].state,
            RelationResidency::Resident
        );
        assert_eq!(shape.residency.total_required_bytes, Some(4096));
        assert_eq!(shape.residency.missing_bytes, Some(0));
        assert_eq!(shape.residency.missing_rows, 0);
        assert_eq!(shape.cost.amortized_auto_load, PgCost::new(0.0));
        assert_eq!(shape.cost.total, PgCost::new(16.0));
    }

    #[test]
    fn missing_residency_requires_auto_load_before_begin() {
        let mut shape = count_shape();
        let decline = apply_exact_residency(
            &mut shape,
            &[estimate(false, 4096)],
            AdmissionPolicy {
                auto_load: false,
                budget_bytes: 8192,
                budget_snapshot: budget_snapshot(0, 0, 0),
            },
            &model(),
        )
        .expect_err("missing relation must decline when auto-load is off");
        assert_eq!(decline.code(), "generic_auto_load_disabled");
        assert!(matches!(
            decline,
            AdmissionDecline::AutoLoadDisabled {
                relation_oid: 42,
                estimated_bytes: 4096
            }
        ));
    }

    #[test]
    fn missing_pinned_residency_reloads_with_auto_load_disabled() {
        let mut shape = count_shape();
        let mut pinned = estimate(false, 4096);
        pinned.pinned = true;
        apply_exact_residency(
            &mut shape,
            &[pinned],
            AdmissionPolicy {
                auto_load: false,
                budget_bytes: 8192,
                budget_snapshot: budget_snapshot(0, 0, 0),
            },
            &model(),
        )
        .expect("durable pin intent authorizes a budgeted reload");
        assert_eq!(
            shape.residency.relations[0].state,
            RelationResidency::AutoLoad
        );
        assert_eq!(shape.residency.missing_bytes, Some(4096));
        assert_eq!(shape.residency.missing_rows, 100_000);
        let expected_load = expected_exact_load(100_000, 4096, true);
        assert_eq!(shape.cost.amortized_auto_load, expected_load);
        assert_eq!(shape.cost.total, PgCost::new(16.0 + expected_load.get()));
    }

    #[test]
    fn projected_cluster_budget_is_checked_before_begin() {
        let mut shape = count_shape();
        let decline = apply_exact_residency(
            &mut shape,
            &[estimate(false, 4096)],
            AdmissionPolicy {
                auto_load: true,
                budget_bytes: 8192,
                budget_snapshot: budget_snapshot(5000, 0, 0),
            },
            &model(),
        )
        .expect_err("projected allocation exceeds budget");
        assert_eq!(decline.code(), "generic_residency_budget_exceeded");
        assert!(matches!(
            decline,
            AdmissionDecline::ResidencyBudgetExceeded {
                other_backend_live_bytes: 5000,
                selected_raw_bytes: 4096,
                derived_artifact_bytes: 0,
                projected_final_bytes: 9096,
                budget_bytes: 8192,
                ..
            }
        ));
    }

    #[test]
    fn derived_artifact_upper_bound_participates_in_tight_budget_gate() {
        let mut shape = grouped_count_shape();
        let decline = apply_exact_residency(
            &mut shape,
            &[estimate(false, 4_096)],
            AdmissionPolicy {
                auto_load: true,
                budget_bytes: 404_095,
                budget_snapshot: budget_snapshot(0, 0, 0),
            },
            &model(),
        )
        .expect_err("raw bytes fit but raw plus derived group codes do not");
        assert!(matches!(
            decline,
            AdmissionDecline::ResidencyBudgetExceeded {
                selected_raw_bytes: 4_096,
                derived_artifact_bytes: 400_000,
                projected_final_bytes: 404_096,
                budget_bytes: 404_095,
                ..
            }
        ));
    }

    #[test]
    fn warm_descriptor_artifact_is_charged_once_in_final_footprint() {
        let mut shape = grouped_count_shape();
        apply_exact_residency(
            &mut shape,
            &[estimate(true, 4_096)],
            AdmissionPolicy {
                auto_load: false,
                budget_bytes: 404_096,
                budget_snapshot: budget_snapshot(0, 0, 404_096),
            },
            &model(),
        )
        .expect("warm raw and derived charges are replaced by one final footprint");
        assert_eq!(
            shape.residency.relations[0].state,
            RelationResidency::Resident
        );
        assert_eq!(shape.residency.missing_bytes, Some(0));
    }

    #[test]
    fn partial_raw_snapshot_is_replaced_by_full_selected_estimate() {
        let mut shape = count_shape();
        apply_exact_residency(
            &mut shape,
            &[estimate(false, 4_096)],
            AdmissionPolicy {
                auto_load: true,
                budget_bytes: 4_096,
                budget_snapshot: budget_snapshot(0, 0, 2_000),
            },
            &model(),
        )
        .expect("partial selected charge is replaced instead of added twice");
        assert_eq!(
            shape.residency.relations[0].state,
            RelationResidency::AutoLoad
        );
    }

    #[test]
    fn unrelated_unpinned_local_bytes_are_reclaimable_for_admission() {
        let mut shape = count_shape();
        apply_exact_residency(
            &mut shape,
            &[estimate(false, 4_096)],
            AdmissionPolicy {
                auto_load: true,
                budget_bytes: 4_096,
                budget_snapshot: budget_snapshot(0, 0, 16_384),
            },
            &model(),
        )
        .expect("local LRU can reclaim unrelated unpinned bytes");
        assert_eq!(shape.residency.total_required_bytes, Some(4_096));
    }

    #[test]
    fn auto_load_state_and_cost_use_exact_estimate() {
        let mut shape = count_shape();
        apply_exact_residency(
            &mut shape,
            &[estimate(false, 4096)],
            AdmissionPolicy {
                auto_load: true,
                budget_bytes: 8192,
                budget_snapshot: budget_snapshot(0, 0, 0),
            },
            &model(),
        )
        .expect("load fits budget");
        assert_eq!(
            shape.residency.relations[0].state,
            RelationResidency::AutoLoad
        );
        assert_eq!(shape.residency.missing_bytes, Some(4096));
        assert_eq!(shape.residency.missing_rows, 100_000);
        let expected_load = expected_exact_load(100_000, 4096, true);
        assert_eq!(shape.cost.amortized_auto_load, expected_load);
        assert_eq!(shape.cost.total, PgCost::new(16.0 + expected_load.get()));
    }

    #[test]
    fn zero_column_load_still_charges_full_relation_scan_rows() {
        let mut shape = count_shape();
        apply_exact_residency(
            &mut shape,
            &[estimate(false, 0)],
            AdmissionPolicy {
                auto_load: true,
                budget_bytes: 0,
                budget_snapshot: budget_snapshot(0, 0, 0),
            },
            &model(),
        )
        .expect("zero-column COUNT(*) scan has no resident byte allocation");

        let expected_load = expected_exact_load(100_000, 0, true);
        assert!(expected_load.get() > 0.0);
        assert_eq!(shape.residency.missing_bytes, Some(0));
        assert_eq!(shape.residency.missing_rows, 100_000);
        assert_eq!(shape.cost.amortized_auto_load, expected_load);
    }

    #[test]
    fn mixed_fixed_and_variable_loads_sum_per_relation_before_amortization() {
        let mut shape = count_shape();
        shape.required_relations.push(RequiredRelation {
            relation_oid: 99,
            attnos: vec![1],
        });
        shape
            .residency
            .relations
            .push(RelationResidencyRequirement {
                relation_oid: 99,
                attnos: vec![1],
                state: RelationResidency::Unknown,
                estimated_rows: 50_000,
                estimated_bytes: None,
            });
        let fixed = estimate_for(42, false, 4_096, true);
        let mut variable = estimate_for(99, false, 8_192, false);
        variable.amortization_queries = 8;
        apply_exact_residency(
            &mut shape,
            &[fixed, variable],
            AdmissionPolicy {
                auto_load: true,
                budget_bytes: 20_000,
                budget_snapshot: budget_snapshot(0, 0, 0),
            },
            &model(),
        )
        .expect("mixed missing relations fit the test budget");

        let expected_load = PgCost::new(
            expected_exact_load(100_000, 4_096, true).get()
                + expected_exact_load(50_000, 8_192, false).get(),
        );
        assert_eq!(shape.residency.missing_rows, 150_000);
        assert_eq!(shape.residency.missing_bytes, Some(12_288));
        assert_eq!(shape.cost.amortized_auto_load, expected_load);
        assert!(
            expected_exact_load(50_000, 8_192, false).get()
                > expected_exact_load(50_000, 8_192, true).get()
        );
    }

    #[test]
    fn exact_residency_preserves_spatial_cost_components() {
        let mut shape = count_shape();
        shape.cost.spatial_filter = PgCost::new(2.0);
        shape.cost.spatial_recheck_reserve = PgCost::new(3.0);
        shape.cost.replace_amortized_auto_load(PgCost::ZERO);
        apply_exact_residency(
            &mut shape,
            &[estimate(false, 4_096)],
            AdmissionPolicy {
                auto_load: true,
                budget_bytes: 8_192,
                budget_snapshot: budget_snapshot(0, 0, 0),
            },
            &model(),
        )
        .expect("spatial shape load fits the test budget");

        let expected_load = expected_exact_load(100_000, 4_096, true);
        assert_eq!(shape.cost.spatial_filter, PgCost::new(2.0));
        assert_eq!(shape.cost.spatial_recheck_reserve, PgCost::new(3.0));
        assert_eq!(
            shape.cost.total,
            PgCost::new(16.0 + 2.0 + 3.0 + expected_load.get())
        );
    }

    #[test]
    fn cost_gate_requires_device_eligibility_and_native_margin() {
        let mut shape = count_shape();
        shape.cost.total = PgCost::new(10.0);
        let effective_cost = effective_path_cost(&shape, 1.0);
        let gpu_cost = effective_cost.total;
        let maximum_cost_ratio = model().planner.gpu_agg_cost_ratio;
        let equality_native = gpu_cost / maximum_cost_ratio;
        gate_cost(
            &shape,
            Some(equality_native),
            maximum_cost_ratio,
            effective_cost,
        )
        .expect("equality admits");
        let decline = gate_cost(
            &shape,
            Some(equality_native - 1.0),
            maximum_cost_ratio,
            effective_cost,
        )
        .expect_err("insufficient margin declines");
        assert_eq!(decline.code(), "generic_cost_not_competitive");

        shape.cost_gate = ShapeCostGate::FactRowsBelowDeviceMinimum {
            estimated: Rows::new(9),
            required: Rows::new(10),
        };
        assert_eq!(
            gate_cost(&shape, Some(1000.0), maximum_cost_ratio, effective_cost,)
                .expect_err("device gate declines")
                .code(),
            "generic_fact_rows_below_device_minimum"
        );
    }

    #[test]
    fn only_dense_below_floor_gate_waits_for_resident_row_refinement() {
        let model = model();
        let below_floor = ShapeCostGate::FactRowsBelowDeviceMinimum {
            estimated: Rows::new(9),
            required: Rows::new(10),
        };

        let mut count_only = count_shape();
        count_only.cost_gate = below_floor;
        assert_eq!(
            stable_device_cost_gate_before_residency(&count_only, &model),
            Some(below_floor),
            "count-only gates cannot be changed by resident row evidence"
        );

        let mut dense = dense_sum_count_shape(9);
        dense.cost_gate = below_floor;
        assert_eq!(
            stable_device_cost_gate_before_residency(&dense, &model),
            None,
            "dense SUM/COUNT must retain exact loaded-row refinement"
        );

        let mut h3 = h3_parent_shape();
        let h3_below_floor = ShapeCostGate::H3RowsBelowDeviceMinimum {
            estimated: Rows::new(99),
            required: Rows::new(100),
        };
        h3.cost_gate = h3_below_floor;
        assert_eq!(
            stable_device_cost_gate_before_residency(&h3, &model),
            Some(h3_below_floor),
            "H3 row gates do not use dense loaded-row refinement"
        );
    }

    #[test]
    fn test_force_only_bypasses_spatial_minimum_work_gates() {
        assert!(test_forceable_spatial_cost_gate(ShapeCostGate::Eligible));
        assert!(test_forceable_spatial_cost_gate(
            ShapeCostGate::FactRowsBelowDeviceMinimum {
                estimated: Rows::new(1),
                required: Rows::new(2),
            }
        ));
        assert!(test_forceable_spatial_cost_gate(
            ShapeCostGate::SpatialRowsBelowDeviceMinimum {
                estimated: Rows::new(1),
                required: Rows::new(2),
            }
        ));
        assert!(test_forceable_spatial_cost_gate(
            ShapeCostGate::SpatialVerticesBelowDeviceMinimum {
                estimated: Rows::new(4),
                required: Rows::new(8),
            }
        ));
        assert!(test_forceable_spatial_cost_gate(
            ShapeCostGate::SpatialWorkBelowDeviceMinimum {
                estimated: WorkProduct::new(4),
                required: WorkProduct::new(8),
            }
        ));

        assert!(!test_forceable_spatial_cost_gate(
            ShapeCostGate::SpatialVerticesExceedDeviceMaximum {
                estimated: Rows::new(9),
                maximum: Rows::new(8),
            }
        ));
        assert!(!test_forceable_spatial_cost_gate(
            ShapeCostGate::SpatialWorkExceedsDeviceMaximum {
                estimated: WorkProduct::new(9),
                maximum: WorkProduct::new(8),
            }
        ));
        assert!(!test_forceable_spatial_cost_gate(
            ShapeCostGate::GroupsExceedDeviceMaximum {
                estimated: Rows::new(9),
                maximum: Rows::new(8),
            }
        ));
    }

    #[test]
    fn h3_device_floor_has_a_stable_admission_decline_code() {
        let mut shape = h3_parent_shape();
        shape.cost_gate = ShapeCostGate::H3RowsBelowDeviceMinimum {
            estimated: Rows::new(99),
            required: Rows::new(100),
        };
        let decline = gate_cost(
            &shape,
            Some(1_000.0),
            model().planner.gpu_agg_cost_ratio,
            effective_path_cost(&shape, 1.0),
        )
        .expect_err("H3 input below the device floor must decline");
        assert_eq!(decline.code(), "h3_rows_below_grouped_agg_min");
        assert!(matches!(
            decline,
            AdmissionDecline::DeviceCostGate(ShapeCostGate::H3RowsBelowDeviceMinimum {
                estimated,
                required,
            }) if estimated == Rows::new(99) && required == Rows::new(100)
        ));
    }

    #[test]
    fn global_cost_multiplier_controls_gate_and_scales_auto_load() {
        let mut shape = count_shape();
        shape.cost.amortized_auto_load = PgCost::new(100.0);
        shape.cost.total = PgCost::new(116.0);
        let aggressive = effective_path_cost(&shape, 0.1);
        let neutral = effective_path_cost(&shape, 1.0);
        let conservative = effective_path_cost(&shape, 10.0);

        assert!((aggressive.startup - 0.5).abs() < 1.0e-9);
        assert!((aggressive.total - 12.1).abs() < 1.0e-9);
        assert!((neutral.startup - 5.0).abs() < 1.0e-9);
        assert!((neutral.total - 121.0).abs() < 1.0e-9);
        assert!((conservative.startup - 50.0).abs() < 1.0e-9);
        assert!((conservative.total - 1210.0).abs() < 1.0e-9);
        let without_load = effective_path_cost(&count_shape(), 0.1);
        assert!((aggressive.total - without_load.total - 10.0).abs() < 1.0e-9);

        let ratio = model().planner.gpu_agg_cost_ratio;
        gate_cost(&shape, Some(100.0), ratio, aggressive).expect("0.1 multiplier admits this cost");
        assert_eq!(
            gate_cost(&shape, Some(100.0), ratio, conservative)
                .expect_err("10.0 multiplier declines this cost")
                .code(),
            "generic_cost_not_competitive"
        );
    }

    #[test]
    fn star_shapes_use_preagg_cost_ratio() {
        let model = model();
        let mut shape = count_shape();
        assert_eq!(
            maximum_cost_ratio(&shape, &model),
            model.planner.gpu_agg_cost_ratio
        );

        add_star_dimension(&mut shape, JoinMultiplicity::Unique);
        assert_eq!(
            maximum_cost_ratio(&shape, &model),
            model.planner.gpu_preagg_cost_ratio
        );
        assert_ne!(
            model.planner.gpu_agg_cost_ratio,
            model.planner.gpu_preagg_cost_ratio
        );
    }

    #[test]
    fn planner_rejects_serial_generic_while_retaining_executor_validation() {
        let mut counted_sum = integer_measure_shape(AggregateKind::Sum);
        add_star_dimension(&mut counted_sum, JoinMultiplicity::Counted);
        let decline = validate_shape_capability(&counted_sum)
            .expect_err("counted-dimension integer SUM must decline before Begin");
        assert_eq!(decline.code(), "generic_descriptor_capability");
        assert!(matches!(
            decline,
            AdmissionDecline::DescriptorCapability { detail }
                if detail.contains("integer SUM with a counted dimension")
        ));

        let mut unique_sum = integer_measure_shape(AggregateKind::Sum);
        add_star_dimension(&mut unique_sum, JoinMultiplicity::Unique);
        validate_normal_descriptor_capability(
            &unique_sum.spec,
            &AggOutputProjection {
                slots: unique_sum.projections.clone(),
            },
        )
        .expect("the defensive executor retains unique-dimension integer SUM capability");
        assert!(matches!(
            validate_shape_capability(&unique_sum)
                .expect_err("normal planning must not admit the serial generic branch"),
            AdmissionDecline::SerialGenericKernelMode {
                mode: "serial_generic"
            }
        ));

        for kind in [AggregateKind::Count, AggregateKind::Min, AggregateKind::Max] {
            let mut counted = integer_measure_shape(kind);
            add_star_dimension(&mut counted, JoinMultiplicity::Counted);
            validate_normal_descriptor_capability(
                &counted.spec,
                &AggOutputProjection {
                    slots: counted.projections.clone(),
                },
            )
            .unwrap_or_else(|error| {
                panic!("defensive counted-dimension {kind:?} capability declined: {error}")
            });
            assert!(matches!(
                validate_shape_capability(&counted)
                    .expect_err("counted dimensions select only the serial generic branch"),
                AdmissionDecline::SerialGenericKernelMode { .. }
            ));
        }

        validate_shape_capability(&count_shape())
            .expect("canonical COUNT(*) uses the parallel dense-count branch");

        let mut range = dense_sum_count_shape(1_000_000);
        let lhs = ColumnRef {
            relation_oid: 42,
            attno: 2,
            type_oid: u32::from(pg_sys::INT4OID),
        };
        let rhs = ColumnRef {
            relation_oid: 42,
            attno: 3,
            type_oid: u32::from(pg_sys::INT4OID),
        };
        range.spec.measures[0].expression = MeasureExpr::Binary {
            op: BinaryMeasureOp::Mul,
            lhs,
            rhs,
        };
        range.projections = vec![
            AggOutputSlot {
                source: AggOutputSource::Aggregate {
                    measure_index: 0,
                    source: AggregateSource::Value,
                    kind: AggregateKind::Sum,
                },
                source_type_oid: u32::from(pg_sys::INT4OID),
                result_type_oid: u32::from(pg_sys::INT8OID),
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: true,
            },
            AggOutputSlot {
                source: AggOutputSource::Aggregate {
                    measure_index: 1,
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                },
                source_type_oid: 0,
                result_type_oid: u32::from(pg_sys::INT8OID),
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: false,
            },
        ];
        range.spec.fact_filter = FilterSpec::Ranges {
            input: lhs,
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(200),
                hi: ScalarValue::I32(800),
            }],
        };
        validate_shape_capability(&range)
            .expect("the exact fused range product uses the parallel dense-integer branch");
        let FilterSpec::Ranges { ranges, .. } = &mut range.spec.fact_filter else {
            unreachable!("range fixture")
        };
        ranges[0].lo = ScalarValue::I32(i32::MIN);
        assert!(matches!(
            validate_shape_capability(&range).expect_err("one-sided range must remain native"),
            AdmissionDecline::SerialGenericKernelMode { .. }
        ));
    }

    #[test]
    fn shape_path_contains_only_strict_contracts() {
        let (spec, projection) = count_contract();
        let words = encode_shape_path_private(&spec, &projection).expect("contract encodes");

        assert_eq!(words[0], AGG_QUERY_SPEC_SENTINEL);
        let spec_start = 1;
        let spec_len =
            crate::engine::spec::AggQuerySpec::encoded_i32_prefix_len(&words[spec_start..])
                .expect("AQS3 body has a strict length");
        let decoded_spec = crate::engine::spec::AggQuerySpec::decode_i32(
            &words[spec_start..spec_start + spec_len],
        )
        .expect("AQS3 body decodes");
        assert_eq!(decoded_spec, spec);

        let projection_sentinel = spec_start + spec_len;
        assert_eq!(words[projection_sentinel], AGG_OUTPUT_PROJECTION_SENTINEL);
        let projection_start = projection_sentinel + 1;
        let projection_len = usize::try_from(words[projection_start + 2])
            .expect("AOP2 body has a nonnegative length");
        assert_eq!(projection_start + projection_len, words.len());
        let decoded_projection =
            AggOutputProjection::decode_i32(&words[projection_start..], &decoded_spec)
                .expect("AOP2 body decodes");
        assert_eq!(decoded_projection, projection);
    }

    #[test]
    fn proof_accounts_for_device_output_without_input_lanes() {
        let proof = shape_resident_proof(&count_shape());
        assert_eq!(proof.device_columns, 1);
        assert_eq!(
            proof.operator_class,
            ResidentOperatorClass::ResidentGroupAgg
        );
        assert_ne!(proof.stage_mask & ResidentOperatorStage::Scan.bit(), 0);
        assert_ne!(
            proof.stage_mask & ResidentOperatorStage::GroupedAggregate.bit(),
            0
        );
        assert_ne!(
            proof.stage_mask & ResidentOperatorStage::FinalMaterialization.bit(),
            0
        );
        assert!(!proof.has_device_selection);
        assert!(proof.has_device_projection);
    }

    #[test]
    fn h3_parent_shape_proof_includes_the_h3_resident_stage() {
        let proof = shape_resident_proof(&h3_parent_shape());
        assert_ne!(proof.stage_mask & ResidentOperatorStage::H3.bit(), 0);
    }

    #[test]
    fn admission_decline_codes_cover_every_local_reason() {
        let row_pair = (Rows::new(1), Rows::new(2));
        let work_pair = (WorkProduct::new(1), WorkProduct::new(2));
        let cases = [
            (
                AdmissionDecline::Shape(ShapeDecline::NotSelect),
                ShapeDecline::NotSelect.code(),
            ),
            (
                AdmissionDecline::AttributeNumberOutOfRange {
                    relation_oid: 1,
                    attno: i32::MAX,
                },
                "generic_residency_attno_out_of_range",
            ),
            (
                AdmissionDecline::ResidencyEstimateFailed {
                    relation_oid: 1,
                    detail: "error".to_owned(),
                },
                "generic_residency_estimate_failed",
            ),
            (
                AdmissionDecline::MissingResidencyEstimate { relation_oid: 1 },
                "generic_residency_estimate_missing",
            ),
            (
                AdmissionDecline::UnexpectedResidencyEstimate { relation_oid: 1 },
                "generic_residency_estimate_unexpected",
            ),
            (
                AdmissionDecline::DescriptorCapability {
                    detail: "unsupported".to_owned(),
                },
                "generic_descriptor_capability",
            ),
            (
                AdmissionDecline::AutoLoadDisabled {
                    relation_oid: 1,
                    estimated_bytes: 1,
                },
                "generic_auto_load_disabled",
            ),
            (
                AdmissionDecline::ResidencyBytesOverflow,
                "generic_residency_bytes_overflow",
            ),
            (
                AdmissionDecline::ResidencyBudgetSnapshotUnavailable,
                "generic_residency_budget_snapshot_unavailable",
            ),
            (
                AdmissionDecline::ResidencyEvidenceChanged,
                "generic_residency_evidence_changed",
            ),
            (
                AdmissionDecline::DerivedArtifactEstimateUnavailable,
                "generic_derived_artifact_estimate_unavailable",
            ),
            (
                AdmissionDecline::GroupEstimateUnavailable,
                "generic_group_estimate_unavailable",
            ),
            (
                AdmissionDecline::ResidencyBudgetExceeded {
                    cluster_live_bytes: 1,
                    current_backend_live_bytes: 1,
                    other_backend_live_bytes: 0,
                    pinned_unselected_raw_bytes: 0,
                    evictable_or_replaced_local_bytes: 0,
                    selected_raw_bytes: 1,
                    derived_artifact_bytes: 1,
                    projected_final_bytes: 2,
                    budget_bytes: 1,
                },
                "generic_residency_budget_exceeded",
            ),
            (
                AdmissionDecline::DeviceCostGate(ShapeCostGate::Eligible),
                "generic_invalid_eligible_cost_gate",
            ),
            (
                AdmissionDecline::DeviceCostGate(ShapeCostGate::FactRowsBelowDeviceMinimum {
                    estimated: row_pair.0,
                    required: row_pair.1,
                }),
                "generic_fact_rows_below_device_minimum",
            ),
            (
                AdmissionDecline::DeviceCostGate(
                    ShapeCostGate::DenseOneShotRowsExceedDeviceMaximum {
                        fact_rows: row_pair.1,
                        maximum: row_pair.0,
                    },
                ),
                "generic_fact_rows_exceed_dense_one_shot_maximum",
            ),
            (
                AdmissionDecline::DeviceCostGate(ShapeCostGate::H3RowsBelowDeviceMinimum {
                    estimated: row_pair.0,
                    required: row_pair.1,
                }),
                "h3_rows_below_grouped_agg_min",
            ),
            (
                AdmissionDecline::DeviceCostGate(ShapeCostGate::SpatialRowsBelowDeviceMinimum {
                    estimated: row_pair.0,
                    required: row_pair.1,
                }),
                "postgis_rows_below_device_minimum",
            ),
            (
                AdmissionDecline::DeviceCostGate(
                    ShapeCostGate::SpatialVerticesBelowDeviceMinimum {
                        estimated: row_pair.0,
                        required: row_pair.1,
                    },
                ),
                "postgis_vertices_below_device_minimum",
            ),
            (
                AdmissionDecline::DeviceCostGate(
                    ShapeCostGate::SpatialVerticesExceedDeviceMaximum {
                        estimated: row_pair.1,
                        maximum: row_pair.0,
                    },
                ),
                "postgis_vertices_exceed_device_maximum",
            ),
            (
                AdmissionDecline::DeviceCostGate(ShapeCostGate::SpatialWorkBelowDeviceMinimum {
                    estimated: work_pair.0,
                    required: work_pair.1,
                }),
                "postgis_work_below_device_minimum",
            ),
            (
                AdmissionDecline::DeviceCostGate(ShapeCostGate::SpatialWorkExceedsDeviceMaximum {
                    estimated: work_pair.1,
                    maximum: work_pair.0,
                }),
                "postgis_work_exceeds_device_maximum",
            ),
            (
                AdmissionDecline::DeviceCostGate(ShapeCostGate::DimensionRowsExceedDeviceMaximum {
                    estimated: row_pair.1,
                    maximum: row_pair.0,
                }),
                "generic_dimension_rows_exceed_device_maximum",
            ),
            (
                AdmissionDecline::DeviceCostGate(ShapeCostGate::GroupsExceedDeviceMaximum {
                    estimated: row_pair.1,
                    maximum: row_pair.0,
                }),
                "generic_groups_exceed_device_maximum",
            ),
            (
                AdmissionDecline::NativePathUnavailable,
                "generic_native_path_unavailable",
            ),
            (
                AdmissionDecline::CostNotCompetitive {
                    gpu_cost: 2.0,
                    native_cost: 1.0,
                    required_cost: 1.0,
                },
                "generic_cost_not_competitive",
            ),
            (AdmissionDecline::PathNotAdded, "generic_path_not_added"),
        ];
        for (decline, expected) in cases {
            assert_eq!(decline.code(), expected);
            assert!(decline.to_string().starts_with(expected));
        }
    }

    #[test]
    fn pure_residency_error_guards_preserve_decline_identity() {
        let required = RequiredRelation {
            relation_oid: 42,
            attnos: vec![i32::MAX],
        };
        assert!(matches!(
            selected_relation(&required),
            Err(AdmissionDecline::AttributeNumberOutOfRange { .. })
        ));

        let shape = count_shape();
        assert!(matches!(
            exact_residency_estimates_with(&shape, |_| Err::<ResidentLoadEstimate, _>("boom")),
            Err(AdmissionDecline::ResidencyEstimateFailed { .. })
        ));
        assert!(matches!(
            add_bytes(u64::MAX, 1),
            Err(AdmissionDecline::ResidencyBytesOverflow)
        ));

        let incoherent = ExactResidencySnapshot {
            requests: vec![SelectedRelation {
                relid: pg_sys::Oid::from(42_u32),
                columns: Vec::new(),
            }],
            estimates: Vec::new(),
        };
        assert_eq!(
            require_coherent_resident_evidence(&incoherent),
            Err(AdmissionDecline::ResidencyEvidenceChanged)
        );
    }

    #[test]
    fn null_path_guards_and_filter_stages_need_no_postgres_backend() {
        let mut shape = count_shape();
        let effective = effective_path_cost(&shape, 1.0);
        // SAFETY: the null output relation is rejected before any dereference or
        // PostgreSQL allocation.
        assert!(
            !unsafe { inject_childless_shape_path(std::ptr::null_mut(), &shape, effective) }
                .expect("null relation declines")
        );
        // SAFETY: a null List is the canonical empty PostgreSQL list and the
        // function returns before invoking any backend symbol.
        assert!(unsafe { path_list_is_serial(std::ptr::null_mut(), 0) });

        shape.spec.measures[0].filter = FilterSpec::Mask {
            input: ColumnRef {
                relation_oid: shape.spec.fact_rel,
                attno: 1,
                type_oid: u32::from(pg_sys::BOOLOID),
            },
            kind: MaskKind::Sql,
        };
        let (stages, has_filter) = shape_stages(&shape);
        assert!(has_filter);
        assert!(stages.contains(&ResidentOperatorStage::Expression));
    }
}
