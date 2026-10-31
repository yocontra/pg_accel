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
    DenseCountWork, DenseExecutionDecision, H3PlanningMemory, dense_count_work_requirements,
    dense_descriptor_memory_requirements, descriptor_program_clone_bytes,
    estimate_descriptor_artifact_bytes_upper_bound, h3_planning_memory,
    is_weighted_global_count_descriptor, numeric_descriptor_memory_requirements,
    planned_descriptor_kernel_mode, scalar_metadata_peak_bytes, select_dense_execution,
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
use crate::engine::spec::{
    AggOutputProjection, AggregateKind, FilterSpec, GroupKeyEncoding, MeasureExpr,
};
use crate::engine::stats;
use crate::gpu::GroupedAggKernelMode;

#[cfg(feature = "pg_test")]
mod paired_recipes;
#[cfg(feature = "pg_test")]
pub use paired_recipes::{PairedRecipe, capture_paired_recipes};

const GENERIC_SHAPE_PATH_CONTEXT: &str = "upper_paths_generic_groupagg";
use crate::engine::ffi::custom_scan::{AGG_OUTPUT_PROJECTION_SENTINEL, AGG_QUERY_SPEC_SENTINEL};
const WEIGHTED_GLOBAL_COUNT_MIN_FACT_ROWS: u64 = 1_000_000;

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
    SpatialResidencyRequired {
        exact_rows: Option<u64>,
    },
    NumericEstimateUnavailable {
        detail: String,
    },
    ScalarEstimateUnavailable {
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
        retained_execution_bytes: u64,
        evictable_or_replaced_local_bytes: u64,
        selected_raw_bytes: u64,
        derived_artifact_bytes: u64,
        prospective_execution_bytes: u64,
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
            Self::NumericEstimateUnavailable { .. } => "generic_numeric_estimate_unavailable",
            Self::ScalarEstimateUnavailable { .. } => "generic_scalar_estimate_unavailable",
            Self::AttributeNumberOutOfRange { .. } => "generic_residency_attno_out_of_range",
            Self::ResidencyEstimateFailed { .. } => "generic_residency_estimate_failed",
            Self::MissingResidencyEstimate { .. } => "generic_residency_estimate_missing",
            Self::UnexpectedResidencyEstimate { .. } => "generic_residency_estimate_unexpected",
            Self::DescriptorCapability { .. } => "generic_descriptor_capability",
            Self::SpatialResidencyRequired { .. } => "generic_spatial_residency_required",
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
    mut policy: AdmissionPolicy,
    model: &TypedCostModel,
) -> Result<(), AdmissionDecline> {
    let idle = crate::gpu::grouped_agg::idle_grouped_execution_bytes()
        .map_err(|_| AdmissionDecline::ResidencyBudgetSnapshotUnavailable)?;
    policy.budget_snapshot = policy
        .budget_snapshot
        .discount_idle_execution(idle)
        .ok_or(AdmissionDecline::ResidencyBudgetSnapshotUnavailable)?;
    let h3_memory = if let Some(estimate) = estimates
        .iter()
        .find(|estimate| u32::from(estimate.relid) == shape.spec.fact_rel)
    {
        let estimated_rows = shape
            .residency
            .relations
            .iter()
            .find(|relation| relation.relation_oid == shape.spec.fact_rel)
            .map_or(0, |relation| relation.estimated_rows);
        if shape.execution_selection()
            == crate::engine::executor::agg::AggExecutionSelection::RawIntegerCount
        {
            Some(
                crate::engine::executor::agg::raw_count_planning_memory(
                    &shape.spec,
                    estimate,
                    estimated_rows,
                    model.memory.gpu_hash_agg_max_groups.get(),
                )
                .map_err(|detail| AdmissionDecline::ScalarEstimateUnavailable { detail })?,
            )
        } else {
            h3_planning_memory(
                &shape.spec,
                &AggOutputProjection {
                    slots: shape.projections.clone(),
                },
                estimate,
                estimated_rows,
                model.memory.gpu_h3_max_groups.get(),
            )
            .map_err(|detail| AdmissionDecline::ScalarEstimateUnavailable { detail })?
        }
    } else {
        None
    };
    apply_exact_residency_with_memory(
        shape,
        estimates,
        policy,
        model,
        h3_memory,
        |shape, estimates, model, headroom| {
            let mut decision = None;
            let mut count_work = None;
            let bytes = scalar_planning_memory_with(
                shape,
                estimates,
                model,
                |spec, nullable, groups, rows, max_chunk, one_shot| {
                    if shape.supports_budget_selected_dense() {
                        decision = select_dense_execution(
                            spec, nullable, groups, rows, max_chunk, one_shot, headroom,
                        )?;
                        if let Some(selected) = decision {
                            if shape.execution_selection()
                                == crate::engine::executor::agg::AggExecutionSelection::Legacy
                                && selected.native_mode == GroupedAggKernelMode::ParallelDenseCount
                                && DenseCountWork::supports(spec)
                            {
                                count_work = Some(dense_count_work_requirements(
                                    spec, nullable, groups, rows, selected,
                                )?);
                            }
                            return Ok(selected.memory);
                        }
                        // Keep the existing complete budget-decline evidence
                        // when no legal candidate fits. This is not dispatch or
                        // fallback; the default requirement necessarily exceeds
                        // the same headroom (including metadata below).
                    }
                    dense_descriptor_memory_requirements(
                        spec, nullable, groups, rows, max_chunk, one_shot,
                    )
                },
            )?;
            Ok((bytes, decision, count_work))
        },
    )
}

#[cfg(test)]
fn apply_exact_residency_with_scalar_memory(
    shape: &mut ShapePlan,
    estimates: &[ResidentLoadEstimate],
    policy: AdmissionPolicy,
    model: &TypedCostModel,
    scalar_memory: impl FnOnce(
        &ShapePlan,
        &[ResidentLoadEstimate],
        &TypedCostModel,
        u64,
    ) -> Result<(u64, Option<DenseExecutionDecision>), AdmissionDecline>,
) -> Result<(), AdmissionDecline> {
    apply_exact_residency_with_memory(shape, estimates, policy, model, None, |s, e, m, h| {
        scalar_memory(s, e, m, h).map(|(bytes, decision)| (bytes, decision, None))
    })
}

fn apply_exact_residency_with_memory(
    shape: &mut ShapePlan,
    estimates: &[ResidentLoadEstimate],
    policy: AdmissionPolicy,
    model: &TypedCostModel,
    h3_memory: Option<H3PlanningMemory>,
    scalar_memory: impl FnOnce(
        &ShapePlan,
        &[ResidentLoadEstimate],
        &TypedCostModel,
        u64,
    ) -> Result<
        (u64, Option<DenseExecutionDecision>, Option<DenseCountWork>),
        AdmissionDecline,
    >,
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
        .map(|relation| {
            let rows = if h3_memory.is_some() {
                estimates
                    .iter()
                    .find(|estimate| {
                        estimate.loaded && u32::from(estimate.relid) == relation.relation_oid
                    })
                    .and_then(|estimate| estimate.resident_rows)
                    .unwrap_or(relation.estimated_rows)
            } else {
                relation.estimated_rows
            };
            (relation.relation_oid, rows)
        })
        .collect::<BTreeMap<_, _>>();
    let derived_artifact_bytes = if shape.execution_selection()
        == crate::engine::executor::agg::AggExecutionSelection::RawIntegerCount
        || (crate::engine::executor::agg::canonical_h3_count(&shape.spec)
            && !matches!(shape.spec.fact_filter, FilterSpec::None))
    {
        // Raw keys and WHERE-bearing H3 borrow resident lanes. WHERE forces
        // fused H3 evaluation; distinct masks are charged in execution memory.
        0
    } else {
        estimate_descriptor_artifact_bytes_upper_bound(&shape.spec, &relation_rows)
            .ok_or(AdmissionDecline::DerivedArtifactEstimateUnavailable)?
            .checked_add(
                descriptor_program_clone_bytes(&shape.spec)
                    .map_err(|detail| AdmissionDecline::ScalarEstimateUnavailable { detail })?,
            )
            .ok_or(AdmissionDecline::ResidencyBytesOverflow)?
    };
    let numeric_resources = numeric_planning_resources(shape, estimates, model)?;
    let owner_bytes = policy
        .budget_snapshot
        .projected_final_bytes(total_required_bytes, derived_artifact_bytes)
        .ok_or(AdmissionDecline::ResidencyBytesOverflow)?;
    // An already impossible fixed base still follows the exact decline path
    // below. It does not receive fake headroom through unsigned underflow.
    let headroom = policy.budget_bytes.saturating_sub(owner_bytes);
    let (scalar_execution_bytes, dense_decision, dense_count_work) = if let Some(memory) = h3_memory
    {
        (memory.bytes, None, None)
    } else {
        scalar_memory(shape, estimates, model, headroom)?
    };
    let prospective_execution_bytes = numeric_resources
        .as_ref()
        .map_or(0, |resources| resources.bytes)
        .checked_add(
            scalar_metadata_peak_bytes(&shape.spec)
                .map_err(|detail| AdmissionDecline::ScalarEstimateUnavailable { detail })?,
        )
        .and_then(|bytes| bytes.checked_add(scalar_execution_bytes))
        .ok_or(AdmissionDecline::ResidencyBytesOverflow)?;
    let projected_final_bytes = owner_bytes
        .checked_add(prospective_execution_bytes)
        .ok_or(AdmissionDecline::ResidencyBytesOverflow)?;
    if projected_final_bytes > policy.budget_bytes {
        return Err(AdmissionDecline::ResidencyBudgetExceeded {
            cluster_live_bytes: policy.budget_snapshot.cluster_live_bytes,
            current_backend_live_bytes: policy.budget_snapshot.current_backend_live_bytes,
            other_backend_live_bytes: policy.budget_snapshot.other_backend_live_bytes,
            pinned_unselected_raw_bytes: policy.budget_snapshot.pinned_unselected_raw_bytes,
            retained_execution_bytes: policy.budget_snapshot.retained_execution_bytes,
            evictable_or_replaced_local_bytes: policy
                .budget_snapshot
                .evictable_or_replaced_local_bytes,
            selected_raw_bytes: total_required_bytes,
            derived_artifact_bytes,
            prospective_execution_bytes,
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
    if shape.execution_selection()
        == crate::engine::executor::agg::AggExecutionSelection::RawIntegerCount
    {
        let estimated = Rows::new(usize::try_from(lifecycle_fact_rows).unwrap_or(usize::MAX));
        shape.cost_gate = if estimated < model.planner.gpu_hash_agg_min_rows {
            ShapeCostGate::FactRowsBelowDeviceMinimum {
                estimated,
                required: model.planner.gpu_hash_agg_min_rows,
            }
        } else {
            ShapeCostGate::Eligible
        };
    }
    if let Some(memory) = h3_memory
        && !memory.cached_owner
    {
        if shape.execution_selection()
            == crate::engine::executor::agg::AggExecutionSelection::RawIntegerCount
        {
            shape
                .cost
                .charge_hash_filter_conversion(&shape.spec, lifecycle_fact_rows, model);
        } else {
            shape
                .cost
                .charge_h3_filter_conversion(&shape.spec, lifecycle_fact_rows, model);
        }
    }
    if let Some(resolution) = shape
        .spec
        .group_keys
        .iter()
        .find_map(|key| match key.source {
            crate::engine::spec::GroupKeySource::H3CellToParent { resolution, .. } => {
                Some(resolution)
            }
            _ => None,
        })
    {
        let bound = h3_memory.map_or_else(
            || {
                super::shape::h3_group_bound(lifecycle_fact_rows, resolution)
                    .expect("validated H3 resolution")
                    .max(1)
            },
            |memory| memory.capacity as u64,
        );
        let maximum = model.memory.gpu_h3_max_groups;
        let bound = if crate::engine::executor::agg::canonical_h3_count(&shape.spec) {
            bound.min(maximum.get() as u64)
        } else {
            bound
        };
        let estimated = Rows::new(usize::try_from(bound).unwrap_or(usize::MAX));
        if estimated > maximum {
            return Err(AdmissionDecline::DeviceCostGate(
                ShapeCostGate::GroupsExceedDeviceMaximum { estimated, maximum },
            ));
        }
        if matches!(
            shape.cost_gate,
            ShapeCostGate::GroupsExceedDeviceMaximum { .. }
        ) {
            shape.cost_gate = ShapeCostGate::Eligible;
        }
    }
    if let Some(resources) = numeric_resources {
        shape.cost.replace_aggregate(resources.aggregate);
        shape
            .cost
            .replace_additional_aggregate_launches(resources.launches);
    } else if let Some(decision) = dense_decision {
        if let Some(work) = dense_count_work {
            #[cfg(feature = "pg_test")]
            if CANDIDATE_OBSERVATIONS.with(|slot| slot.borrow().is_some()) {
                pgrx::notice!(
                    "candidate COUNT work: decision={decision:?}; work={work:?}; before={:?}",
                    shape.cost
                );
            }
            if work.calls != decision.calls || work.facts.rows != lifecycle_fact_rows {
                return Err(AdmissionDecline::ScalarEstimateUnavailable {
                    detail: "dense COUNT pricing disagrees with selected lifecycle".into(),
                });
            }
            shape
                .cost
                .replace_aggregate(dense_count_aggregate_cost(work, model)?);
        } else if let Some(reads) =
            dense_integer_count_where_reads(shape, decision, lifecycle_fact_rows)
        {
            #[cfg(feature = "pg_test")]
            if CANDIDATE_OBSERVATIONS.with(|slot| slot.borrow().is_some()) {
                pgrx::notice!(
                    "candidate integer WHERE work: decision={decision:?}; reads={reads}; coefficient={}; before={:?}",
                    model.coefficients.gpu_op_cost_filter.get(),
                    shape.cost
                );
            }
            // Integer COUNT workers visit each input once across chunks. This
            // is consumer work, not cached host-mask construction or launches.
            let mask_read_cost = reads as f64 * model.coefficients.gpu_op_cost_filter.get();
            shape
                .cost
                .replace_aggregate(PgCost::new(shape.cost.aggregate.get() + mask_read_cost));
        }
        shape
            .cost
            .replace_selected_dense_lifecycle_calls(decision.calls)
            .ok_or(AdmissionDecline::ResidencyBytesOverflow)?;
    } else if let Some(calls) = planned_dense_lifecycle_calls(shape, lifecycle_fact_rows, model) {
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
    if shape_kernel_mode(shape) == GroupedAggKernelMode::ParallelDenseNumeric
        && matches!(
            shape.cost_gate,
            ShapeCostGate::Eligible | ShapeCostGate::FactRowsBelowDeviceMinimum { .. }
        )
    {
        let rows = Rows::new(usize::try_from(lifecycle_fact_rows).unwrap_or(usize::MAX));
        let required = super::shape::numeric_fact_row_floor(model);
        shape.cost_gate = if rows < required {
            ShapeCostGate::FactRowsBelowDeviceMinimum {
                estimated: rows,
                required,
            }
        } else {
            ShapeCostGate::Eligible
        };
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
        } else if dense_decision.is_none()
            && lifecycle_fact_rows > model.executor.gpu_grouped_agg_one_shot_max_rows
        {
            // A successful budget-selected bounded lifecycle already proves
            // every call respects this cap. It is not a total-query row limit.
            // Families without that proof retain the conservative guard.
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

struct NumericPlanningResources {
    bytes: u64,
    aggregate: PgCost,
    launches: PgCost,
}

fn dense_integer_count_where_reads(
    shape: &ShapePlan,
    decision: DenseExecutionDecision,
    rows: u64,
) -> Option<u64> {
    use crate::engine::spec::{AggregateSource, MaskKind};
    let spec = &shape.spec;
    let [measure] = spec.measures.as_slice() else {
        return None;
    };
    (shape.execution_selection() == crate::engine::executor::agg::AggExecutionSelection::Legacy
        && decision.native_mode == GroupedAggKernelMode::ParallelDenseInteger
        && spec.star_dims.is_empty()
        && matches!(spec.fact_filter, FilterSpec::Mask { input, kind: MaskKind::Sql }
            if input.relation_oid == spec.fact_rel && input.type_oid == 16
                && input.attno > 0 && i16::try_from(input.attno).is_ok())
        && matches!(
            measure.expression,
            MeasureExpr::Column(_) | MeasureExpr::Bytecode { .. }
        )
        && matches!(measure.outputs.as_slice(), [output]
            if output.kind == AggregateKind::Count && output.source == AggregateSource::Value))
    .then_some(rows)
}

fn dense_count_aggregate_cost(
    work: DenseCountWork,
    model: &TypedCostModel,
) -> Result<PgCost, AdmissionDecline> {
    let unavailable = |detail| AdmissionDecline::ScalarEstimateUnavailable { detail };
    let (parallel, checks, serial) = work.work_products().map_err(unavailable)?;
    // Deliberately separate multiply/add rounding. Launches, mask construction,
    // loading and output remain in their existing independent cost components.
    #[allow(clippy::suboptimal_flops)]
    let aggregate = parallel as f64 * model.coefficients.gpu_op_cost_reduce.get()
        + checks as f64 * model.coefficients.gpu_op_cost_filter.get()
        + serial as f64 * model.coefficients.gpu_dense_count_serial_work_cost.get();
    if !aggregate.is_finite() || aggregate < 0.0 {
        return Err(unavailable(
            "dense COUNT aggregate cost is not finite/nonnegative".into(),
        ));
    }
    Ok(PgCost::new(aggregate))
}

fn scalar_planning_memory_with(
    shape: &ShapePlan,
    estimates: &[ResidentLoadEstimate],
    model: &TypedCostModel,
    memory_query: impl FnOnce(
        &crate::engine::spec::AggQuerySpec,
        &[i16],
        usize,
        usize,
        usize,
        usize,
    ) -> Result<crate::gpu::GroupedAggMemoryRequirements, String>,
) -> Result<u64, AdmissionDecline> {
    let scalar = shape
        .spec
        .measures
        .iter()
        .any(|measure| matches!(measure.expression, MeasureExpr::Bytecode { .. }));
    if !scalar
        && (!shape.spec.star_dims.is_empty()
            || !matches!(
                shape_kernel_mode(shape),
                GroupedAggKernelMode::ParallelDenseInteger
                    | GroupedAggKernelMode::ParallelDenseCount
            ))
    {
        return Ok(0);
    }
    let unavailable = |detail: &str| AdmissionDecline::ScalarEstimateUnavailable {
        detail: detail.to_owned(),
    };
    let estimate = estimates
        .iter()
        .find(|estimate| u32::from(estimate.relid) == shape.spec.fact_rel)
        .ok_or_else(|| {
            unavailable("scalar layout forecast requires a complete resident snapshot")
        })?;
    // Scalar admission retains its exact resident metadata contract. Plain
    // fixed-width cold loading uses the same base-row estimate as raw loading,
    // conservatively budgeting NULL lanes until exact metadata is available.
    if scalar && !estimate.loaded {
        return Err(unavailable(
            "scalar layout forecast requires a complete resident snapshot",
        ));
    }
    let cold_nullable;
    let (rows, nullable) = if estimate.loaded {
        (
            estimate
                .resident_rows
                .ok_or_else(|| unavailable("missing dense resident row count"))?,
            estimate
                .resident_nullable_columns
                .as_deref()
                .ok_or_else(|| unavailable("missing dense per-column NULL metadata"))?,
        )
    } else {
        let relation = shape
            .residency
            .relations
            .iter()
            .find(|relation| relation.relation_oid == shape.spec.fact_rel)
            .ok_or_else(|| unavailable("missing dense base-row estimate"))?;
        cold_nullable = shape
            .required_relations
            .iter()
            .find(|relation| relation.relation_oid == shape.spec.fact_rel)
            .ok_or_else(|| unavailable("missing dense selected columns"))?
            .attnos
            .iter()
            .map(|attno| {
                i16::try_from(*attno).map_err(|_| unavailable("dense selected column exceeds i16"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        (relation.estimated_rows, cold_nullable.as_slice())
    };
    let rows =
        usize::try_from(rows).map_err(|_| unavailable("scalar resident rows exceed usize"))?;
    let cardinalities = shape
        .estimated_dense_key_cardinalities
        .as_deref()
        .ok_or_else(|| unavailable("missing scalar dense-slot estimates"))?;
    let nullable_keys = shape
        .spec
        .group_keys
        .iter()
        .map(|key| {
            let crate::engine::spec::GroupKeySource::FactColumn(column) = key.source else {
                return Err(unavailable("scalar grouping requires fact-column keys"));
            };
            Ok(nullable
                .iter()
                .any(|attno| i32::from(*attno) == column.attno))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let groups = super::shape::dense_slot_product(cardinalities, &nullable_keys)
        .and_then(|groups| usize::try_from(groups).ok())
        .ok_or_else(|| unavailable("scalar dense-slot estimates mismatch or overflow"))?;
    let maximum = match &shape.descriptor_resolution {
        DescriptorResolution::BeginTimeArtifacts {
            max_group_count, ..
        } => model
            .memory
            .gpu_hash_agg_max_groups
            .get()
            .min(*max_group_count),
        DescriptorResolution::Ready => model.memory.gpu_hash_agg_max_groups.get(),
    };
    if groups > maximum {
        return Err(unavailable(
            "scalar dense-slot estimate exceeds dictionary capacity",
        ));
    }
    let memory = memory_query(
        &shape.spec,
        nullable,
        groups,
        rows,
        model.executor.gpu_reduce_max_chunk.get(),
        model.executor.gpu_grouped_agg_one_shot_max_rows.get(),
    )
    .map_err(|detail| AdmissionDecline::ScalarEstimateUnavailable { detail })?;
    let bytes = u64::try_from(memory.total_bytes)
        .map_err(|_| unavailable("scalar execution bytes exceed u64"))?;
    // Spatial preparation reserves a transient program owner in addition to
    // the published clone already included in derived_artifact_bytes. Cover
    // that staged overlap as well as scalar execution, conservatively even
    // though construction and aggregate dispatch are separate phases.
    let transient_program_bytes = if shape.spec.fact_filter.spatial().is_some() {
        descriptor_program_clone_bytes(&shape.spec)
            .map_err(|detail| AdmissionDecline::ScalarEstimateUnavailable { detail })?
    } else {
        0
    };
    bytes
        .checked_add(transient_program_bytes)
        .ok_or_else(|| unavailable("scalar spatial execution bytes exceed u64"))
}

/// Exact resident decimal work combined with the planner's dense-slot estimate.
/// Memory is queried from the native layout without allocating or dispatching.
/// Runtime reservations still enforce the cap if cardinality estimates change.
fn numeric_planning_resources(
    shape: &ShapePlan,
    estimates: &[ResidentLoadEstimate],
    model: &TypedCostModel,
) -> Result<Option<NumericPlanningResources>, AdmissionDecline> {
    numeric_planning_resources_with(
        shape,
        estimates,
        model,
        numeric_descriptor_memory_requirements,
    )
}

fn numeric_planning_resources_with(
    shape: &ShapePlan,
    estimates: &[ResidentLoadEstimate],
    model: &TypedCostModel,
    memory_query: impl FnOnce(
        &crate::engine::spec::AggQuerySpec,
        &[crate::engine::residency::ResidentNumericColumnStats],
        &[i16],
        usize,
    ) -> Result<crate::gpu::GroupedAggMemoryRequirements, String>,
) -> Result<Option<NumericPlanningResources>, AdmissionDecline> {
    if shape_kernel_mode(shape) != GroupedAggKernelMode::ParallelDenseNumeric {
        return Ok(None);
    }
    let unavailable = |detail: &str| AdmissionDecline::NumericEstimateUnavailable {
        detail: detail.to_owned(),
    };
    let estimate = estimates
        .iter()
        .find(|estimate| u32::from(estimate.relid) == shape.spec.fact_rel && estimate.loaded)
        .ok_or_else(|| unavailable("NUMERIC requires a complete resident snapshot"))?;
    let rows = estimate
        .resident_rows
        .ok_or_else(|| unavailable("missing exact NUMERIC row count"))?;
    let columns = estimate
        .resident_numeric_columns
        .as_deref()
        .ok_or_else(|| unavailable("missing resident NUMERIC digit metadata"))?;
    let nullable = estimate
        .resident_nullable_columns
        .as_deref()
        .ok_or_else(|| unavailable("missing per-column NULL metadata"))?;
    let cardinalities = shape
        .estimated_dense_key_cardinalities
        .as_deref()
        .ok_or_else(|| unavailable("missing per-key dense-slot estimates"))?;
    let nullable_keys = shape
        .spec
        .group_keys
        .iter()
        .map(|key| {
            let crate::engine::spec::GroupKeySource::FactColumn(column) = key.source else {
                return Err(unavailable("NUMERIC grouping requires fact-column keys"));
            };
            Ok(nullable
                .iter()
                .any(|attno| i32::from(*attno) == column.attno))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let groups = super::shape::dense_slot_product(cardinalities, &nullable_keys)
        .ok_or_else(|| unavailable("NUMERIC dense-slot estimates mismatch or overflow"))?;
    let groups = usize::try_from(groups)
        .map_err(|_| unavailable("NUMERIC dense-slot estimate exceeds usize"))?;
    let configured_maximum = model.memory.gpu_hash_agg_max_groups.get();
    let maximum = match &shape.descriptor_resolution {
        DescriptorResolution::BeginTimeArtifacts {
            max_group_count, ..
        } => configured_maximum.min(*max_group_count),
        DescriptorResolution::Ready => configured_maximum,
    };
    if groups > maximum {
        // Per-key statistics and NULL evidence are not structural shape facts;
        // keep this decline transient rather than caching joint output rows.
        return Err(unavailable(
            "NUMERIC dense-slot estimate exceeds dictionary capacity",
        ));
    }
    let memory = memory_query(&shape.spec, columns, nullable, groups)
        .map_err(|detail| AdmissionDecline::NumericEstimateUnavailable { detail })?;
    let mut digit_count = 0_u64;
    let mut max_digits_per_row = 1_u64;
    let mut values = 0_u64;
    for measure in &shape.spec.measures {
        if let Some(column) = match measure.expression {
            MeasureExpr::Column(column) => Some(column),
            _ => measure.filtered_count_input(),
        } {
            values += 1;
            if column.type_oid == u32::from(pg_sys::INT8OID) {
                if measure
                    .outputs
                    .iter()
                    .any(|output| matches!(output.kind, AggregateKind::Sum | AggregateKind::Avg))
                {
                    digit_count = digit_count
                        .checked_add(
                            rows.checked_mul(5)
                                .ok_or_else(|| unavailable("INT8 digit-work overflow"))?,
                        )
                        .ok_or_else(|| unavailable("INT8 digit-work overflow"))?;
                    max_digits_per_row += 5;
                }
                continue;
            }
            if column.type_oid != u32::from(pg_sys::NUMERICOID) {
                if rows > (i64::MAX as u64) / (1_u64 << 31)
                    && measure.outputs.iter().any(|output| {
                        matches!(output.kind, AggregateKind::Sum | AggregateKind::Avg)
                    })
                {
                    return Err(unavailable(
                        "mixed integer SUM exceeds exact prefix-safe row bound",
                    ));
                }
                continue;
            }
            let stats = columns
                .iter()
                .find(|stats| i32::from(stats.attno) == column.attno)
                .ok_or_else(|| unavailable("NUMERIC column metadata disappeared"))?;
            if measure
                .outputs
                .iter()
                .any(|output| matches!(output.kind, AggregateKind::Sum | AggregateKind::Avg))
            {
                digit_count = digit_count
                    .checked_add(stats.digit_count)
                    .ok_or_else(|| unavailable("NUMERIC digit-work overflow"))?;
                max_digits_per_row += u64::from(stats.bounds.max_ndigits);
            }
        }
    }
    let chunk = model
        .executor
        .gpu_reduce_max_chunk
        .get()
        .min(model.executor.gpu_grouped_agg_one_shot_max_rows.get()) as u64;
    let (row_work, additional_launches) =
        numeric_work_units(rows, values, digit_count, max_digits_per_row, chunk).ok_or_else(
            || unavailable("NUMERIC row/chunk work exceeds its exact lifecycle domain"),
        )?;
    let mut predicate_work = 0_u64;
    for filter in std::iter::once(&shape.spec.fact_filter)
        .chain(shape.spec.measures.iter().map(|measure| &measure.filter))
    {
        if let Some((_, ranges, accept_null)) = filter.scalar().range_parts() {
            // The numeric kernel specializes singleton ranges to equality on
            // the host. Charge one comparison for those, two for intervals;
            // retain the worst case over all ranges and all input rows.
            let comparisons = ranges
                .iter()
                .try_fold(u64::from(accept_null), |count, range| {
                    count.checked_add(if range.lo == range.hi { 1 } else { 2 })
                })
                .and_then(|count| count.checked_mul(rows))
                .ok_or_else(|| unavailable("NUMERIC predicate work overflow"))?;
            predicate_work = predicate_work
                .checked_add(comparisons)
                .ok_or_else(|| unavailable("NUMERIC predicate/transition work overflow"))?;
        }
        if matches!(filter, FilterSpec::Mask { .. }) || filter.spatial().is_some() {
            predicate_work = predicate_work
                .checked_add(rows)
                .ok_or_else(|| unavailable("NUMERIC SQL-mask work overflow"))?;
        }
    }
    // Keys are already dense dictionary codes: a decimal limb update performs
    // reduction/atomic work, not a hash-table build and probe. Charging the
    // hash coefficient once per digit overprices measured dense decimal work.
    let aggregate = (memory.workspace_bytes as f64 / 8.0).mul_add(
        model.coefficients.gpu_op_cost_reduce.get(),
        row_work as f64 * model.coefficients.gpu_op_cost_reduce.get(),
    );
    let aggregate =
        (predicate_work as f64).mul_add(model.coefficients.gpu_op_cost_filter.get(), aggregate);
    Ok(Some(NumericPlanningResources {
        bytes: memory.total_bytes as u64,
        aggregate: PgCost::new(aggregate),
        launches: PgCost::new(additional_launches as f64 * cost::GPU_LAUNCH_OVERHEAD),
    }))
}

fn numeric_work_units(
    rows: u64,
    values: u64,
    digits: u64,
    max_digits_per_row: u64,
    chunk: u64,
) -> Option<(u64, u64)> {
    if rows > i64::MAX as u64 / 10000
        || !(1..=4).contains(&values)
        || chunk == 0
        || max_digits_per_row == 0
    {
        return None;
    }
    let row_work = rows.checked_mul(values + 1)?.checked_add(digits)?;
    // Match the native NUMERIC launch digit cap, preserving outer chunk tails.
    let launch_rows = (1_048_576 / max_digits_per_row).max(1);
    let data_launches = (rows / chunk)
        .checked_mul(chunk.div_ceil(launch_rows))?
        .checked_add((rows % chunk).div_ceil(launch_rows))?;
    // The base dispatch charge already covers the single finalization call.
    let extra_lifecycle_calls = rows.div_ceil(chunk);
    Some((row_work, data_launches.checked_add(extra_lifecycle_calls)?))
}

fn planned_dense_lifecycle_calls(
    shape: &ShapePlan,
    fact_rows: u64,
    model: &TypedCostModel,
) -> Option<u64> {
    if (shape.spec.fact_filter.spatial().is_some()
        && !shape
            .spec
            .measures
            .iter()
            .any(|measure| matches!(measure.expression, MeasureExpr::Bytecode { .. })))
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
    let result = if shape.execution_selection()
        == crate::engine::executor::agg::AggExecutionSelection::RawIntegerCount
    {
        shape
            .execution_selection()
            .validate(&shape.spec, &projection)
    } else if shape.spec.fact_filter.spatial().is_some() {
        validate_normal_spatial_candidate_capability(&shape.spec, &projection)
    } else {
        validate_normal_descriptor_capability(&shape.spec, &projection)
    };
    result.map_err(|detail| AdmissionDecline::DescriptorCapability { detail })?;
    if is_weighted_global_count_descriptor(&shape.spec) {
        let estimated_fact_rows = shape
            .residency
            .relations
            .iter()
            .find(|relation| relation.relation_oid == shape.spec.fact_rel)
            .map_or(0, |relation| relation.estimated_rows);
        if estimated_fact_rows < WEIGHTED_GLOBAL_COUNT_MIN_FACT_ROWS {
            // The physical kernel is exact at every scale, but normal planning
            // releases this counted-join lane only where it has independent
            // winning evidence. Preserve the historical structural decline
            // below that envelope instead of letting a later cost comparison
            // silently change the registered native-parity reason.
            return Err(AdmissionDecline::SerialGenericKernelMode {
                mode: GroupedAggKernelMode::SerialGeneric.label(),
            });
        }
    }
    let mode = shape_kernel_mode(shape);
    if mode == GroupedAggKernelMode::SerialGeneric {
        return Err(AdmissionDecline::SerialGenericKernelMode { mode: mode.label() });
    }
    Ok(())
}

fn normal_spatial_promotion_rows(exact_rows: Option<u64>) -> bool {
    // Residency, vertex-work and memory gates remain separate. An exact row
    // count proves the snapshot; it is not a benchmark-fixture identity.
    exact_rows.is_some_and(|rows| rows > 0 && usize::try_from(rows).is_ok())
}

fn validate_normal_spatial_residency_envelope(
    shape: &ShapePlan,
    snapshot: &ExactResidencySnapshot,
) -> Result<(), AdmissionDecline> {
    if shape.spec.fact_filter.spatial().is_none() {
        return Ok(());
    }
    let exact_rows = snapshot
        .estimates
        .iter()
        .find(|estimate| u32::from(estimate.relid) == shape.spec.fact_rel)
        .filter(|estimate| estimate.loaded)
        .and_then(|estimate| estimate.resident_rows);
    if !normal_spatial_promotion_rows(exact_rows) {
        // A missing/invalidated resident snapshot is transient, not a
        // structural descriptor failure. Never cache it as query identity:
        // a later explicit pin must allow this same query to be reconsidered.
        return Err(AdmissionDecline::SpatialResidencyRequired { exact_rows });
    }
    Ok(())
}

fn stable_device_cost_gate_before_residency(
    shape: &ShapePlan,
    model: &TypedCostModel,
) -> Option<ShapeCostGate> {
    match shape.cost_gate {
        ShapeCostGate::Eligible => None,
        ShapeCostGate::GroupsExceedDeviceMaximum { .. }
            if shape.spec.group_keys.iter().any(|key| {
                matches!(
                    key.source,
                    crate::engine::spec::GroupKeySource::H3CellToParent { .. }
                )
            }) =>
        {
            None
        }
        ShapeCostGate::FactRowsBelowDeviceMinimum { .. }
            if shape_kernel_mode(shape) == GroupedAggKernelMode::ParallelDenseNumeric
                || shape.execution_selection()
                    == crate::engine::executor::agg::AggExecutionSelection::RawIntegerCount
                || dense_atomic_sum_count_lifecycle(
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
    gucs::test_force_spatial_groupagg() && shape.spec.fact_filter.spatial().is_some()
}

#[cfg(any(test, feature = "pg_test"))]
fn test_force_numeric_groupagg(shape: &ShapePlan) -> bool {
    gucs::test_force_numeric_groupagg()
        && shape.spec.measures.iter().any(|measure| {
            matches!(measure.expression, MeasureExpr::Column(column)
                if column.type_oid == u32::from(pg_sys::NUMERICOID))
        })
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

/// Charge native HAVING on every pre-filter output group. The GPU multiplier
/// must not discount work executed by PostgreSQL's expression evaluator.
unsafe fn add_native_having_cost(
    root: *mut pg_sys::PlannerInfo,
    output_rows: u64,
    mut cost: EffectivePathCost,
) -> EffectivePathCost {
    // SAFETY: caller supplies the live upper-path PlannerInfo and Query.
    let having = unsafe { (*(*root).parse).havingQual };
    if !having.is_null() {
        let mut qual_cost = pg_sys::QualCost::default();
        // SAFETY: planner preprocessing canonicalizes HAVING to a qual List;
        // qual_cost is writable storage for PostgreSQL's own cost estimator.
        unsafe { pg_sys::cost_qual_eval(&raw mut qual_cost, having.cast(), root) };
        cost.startup += qual_cost.startup;
        cost.total += qual_cost
            .per_tuple
            .mul_add(output_rows as f64, qual_cost.startup);
    }
    cost
}

fn maximum_cost_ratio(shape: &ShapePlan, model: &TypedCostModel) -> f64 {
    if shape.spec.star_dims.is_empty() {
        model.planner.gpu_agg_cost_ratio
    } else {
        model.planner.gpu_preagg_cost_ratio
    }
}

#[cfg(test)]
fn encode_shape_path_private(
    spec: &crate::engine::spec::AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<Vec<c_int>, ShapeDecline> {
    encode_selected_shape_path_private(
        spec,
        projection,
        crate::engine::executor::agg::AggExecutionSelection::Legacy,
    )
}

fn encode_selected_shape_path_private(
    spec: &crate::engine::spec::AggQuerySpec,
    projection: &AggOutputProjection,
    selection: crate::engine::executor::agg::AggExecutionSelection,
) -> Result<Vec<c_int>, ShapeDecline> {
    selection
        .validate(spec, projection)
        .map_err(ShapeDecline::InvalidSpec)?;
    let spec_words = spec
        .encode_i32()
        .map_err(|error| ShapeDecline::Codec(error.to_string()))?;
    let projection_words = projection
        .encode_i32(spec)
        .map_err(|error| ShapeDecline::Codec(error.to_string()))?;
    let mut words = Vec::with_capacity(spec_words.len() + projection_words.len() + 2);
    words.extend_from_slice(selection.prefix());
    words.push(AGG_QUERY_SPEC_SENTINEL);
    words.extend(spec_words);
    words.push(AGG_OUTPUT_PROJECTION_SENTINEL);
    words.extend(projection_words);
    Ok(words)
}

fn shape_stages(shape: &ShapePlan) -> (Vec<ResidentOperatorStage>, bool) {
    let has_spatial = shape.spec.fact_filter.spatial().is_some()
        || shape
            .spec
            .measures
            .iter()
            .any(|measure| measure.filter.spatial().is_some())
        || shape
            .spec
            .star_dims
            .iter()
            .any(|dimension| dimension.filter.spatial().is_some());
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
    let selection = shape.execution_selection();
    let words = encode_selected_shape_path_private(&shape.spec, &projection, selection)?;
    let mut digest_words = selection.prefix().to_vec();
    digest_words.extend(shape.spec.encode_i32().map_err(|error| {
        ShapeDecline::Codec(format!(
            "shape spec changed before path construction: {error}"
        ))
    })?);
    digest_words.extend(projection.encode_i32(&shape.spec).map_err(|error| {
        ShapeDecline::Codec(format!(
            "shape projection changed before path construction: {error}"
        ))
    })?);
    if digest_words != shape.digest_words {
        return Err(ShapeDecline::Codec(
            "shape digest words do not match the serialized AQS6/AOP2 contract".to_owned(),
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

const RAW_CANDIDATE_NOT_APPLICABLE: &str = "raw_count_candidate_not_applicable";

fn report_candidate_decline(raw_count: bool, decline: &AdmissionDecline) -> bool {
    // A nonmatching additional raw shape is not a whole-query rejection.
    // Keep the legacy candidate's diagnostic (notably H3 cost/budget declines).
    // Once raw extraction succeeds, capability/resource/cost failures remain
    // genuine candidate diagnostics and are always reported.
    !raw_count
        || !matches!(decline, AdmissionDecline::Shape(shape) if !matches!(shape, ShapeDecline::Codec(_)))
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
    // Preserve the shared cheap rejection boundary before GPU discovery or
    // native-cost reconstruction. Only actual shared shape errors cache both.
    // SAFETY: root belongs to this live main-backend planner invocation.
    unsafe { super::shape::mark_permission_role_dependency(root) };
    // SAFETY: same live planner tree, observed before either candidate adds paths.
    let query_fingerprint = unsafe { decline_cache::query_fingerprint(root) };
    let structural_keys = [false, true].map(|raw| {
        query_fingerprint.as_ref().map(|fingerprint| {
            decline_cache_key(
                Rc::clone(fingerprint),
                input_rel,
                output_rel,
                DeclinePolicyKey::structural(),
            )
            .with_raw_count_candidate(raw)
        })
    });
    let structural_hits = structural_keys
        .each_ref()
        .map(|key| key.as_ref().and_then(decline_cache::lookup));
    if let [Some(legacy), Some(_raw)] = structural_hits {
        record_cached_decline(legacy, output_rel);
        return false;
    }
    // SAFETY: shared relation preflight reads only live planner/catalog metadata.
    if let Err(decline) = unsafe { super::shape::preflight_base_relations(root) } {
        let decline = AdmissionDecline::Shape(decline);
        for key in structural_keys.into_iter().flatten() {
            decline_cache::insert(key, decline.code(), Vec::new());
        }
        record_decline(&decline, output_rel);
        return false;
    }
    if !cost::gpu_is_usable() {
        return false;
    }
    // Capture native estimates before either candidate mutates output paths.
    // SAFETY: hook supplies planner-owned live relation/tree pointers.
    let group_estimate = unsafe { consistent_agg_group_estimate(output_rel) };
    let native_cost = {
        let _profile = PlannerSubstageGuard::new(stats::PlannerSubstage::NativeCostReconstruction);
        // SAFETY: same live upper-path hook arguments, before path injection.
        group_estimate.and_then(|estimate| unsafe {
            cheapest_native_cost(root, input_rel, output_rel, extra, estimate.num_groups)
        })
    };
    // Both candidates run even if one declines or succeeds. Their caches and
    // cost checks are independent; PostgreSQL chooses the cheapest added path.
    // SAFETY: all pointers remain live across both synchronous candidate checks.
    let mut candidate_inputs = super::shape::CandidateShapeInputs::default();
    #[cfg(feature = "pg_test")]
    {
        candidate_inputs.verify_fresh_equivalence =
            CANDIDATE_INPUT_OBSERVATIONS.with(|slot| slot.borrow().is_some());
    }
    let legacy = unsafe {
        try_inject_candidate(
            root,
            input_rel,
            output_rel,
            (group_estimate, native_cost),
            false,
            query_fingerprint.clone(),
            &mut candidate_inputs,
        )
    };
    // SAFETY: same planner lifetime; native comparator was captured above.
    let raw = unsafe {
        try_inject_candidate(
            root,
            input_rel,
            output_rel,
            (group_estimate, native_cost),
            true,
            query_fingerprint,
            &mut candidate_inputs,
        )
    };
    #[cfg(feature = "pg_test")]
    CANDIDATE_INPUT_OBSERVATIONS.with(|slot| {
        if let Some(observations) = slot.borrow_mut().as_mut() {
            observations.push(candidate_inputs.observations);
        }
    });
    legacy || raw
}

unsafe fn try_inject_candidate(
    root: *mut pg_sys::PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
    native_estimate: (Option<AggGroupEstimate>, Option<f64>),
    raw_count: bool,
    query_fingerprint: Option<Rc<QueryFingerprint>>,
    candidate_inputs: &mut super::shape::CandidateShapeInputs,
) -> bool {
    let (group_estimate, native_cost) = native_estimate;
    let structural_cache_key = query_fingerprint.as_ref().map(|fingerprint| {
        decline_cache_key(
            Rc::clone(fingerprint),
            input_rel,
            output_rel,
            DeclinePolicyKey::structural(),
        )
        .with_raw_count_candidate(raw_count)
    });
    if let Some(reason) = structural_cache_key
        .as_ref()
        .and_then(decline_cache::lookup)
    {
        if reason != RAW_CANDIDATE_NOT_APPLICABLE {
            record_cached_decline(reason, output_rel);
        }
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
        .with_raw_count_candidate(raw_count)
    });
    if let Some(reason) = device_cache_key.as_ref().and_then(decline_cache::lookup) {
        record_cached_decline(reason, output_rel);
        return false;
    }
    // Use the aggregate paths PostgreSQL just built as the single source of
    // truth for both admission and the injected path's output cardinality.
    // SAFETY: output_rel is the live planner-owned upper relation.
    // Include PostgreSQL's exact native comparator in the late cost-decline
    // identity. This retains planner-policy and parameter sensitivity while
    // allowing a hit to skip descriptor and residency reconstruction.
    // SAFETY: all pointers are the live upper-path hook arguments.
    let cost_cache_key = device_cache_key
        .clone()
        .map(|key| key.with_native_cost(native_cost));
    if let Some(reason) = cost_cache_key.as_ref().and_then(decline_cache::lookup) {
        record_cached_decline(reason, output_rel);
        return false;
    }
    #[cfg(feature = "pg_test")]
    let mut observed_cost = None;
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
            super::shape::extract_selected_shape_with_inputs(
                root,
                output_rel,
                group_estimate.output_rows,
                &model,
                raw_count,
                Some(candidate_inputs),
            )
        }
        .map_err(AdmissionDecline::Shape)?;

        #[cfg(any(test, feature = "pg_test"))]
        if test_force_spatial_groupagg(&shape) || test_force_numeric_groupagg(&shape) {
            let validate = if test_force_numeric_groupagg(&shape) {
                crate::engine::executor::agg::validate_test_forced_numeric_capability
            } else {
                crate::engine::executor::agg::validate_test_forced_spatial_capability
            };
            validate(
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
        #[cfg(feature = "pg_test")]
        paired_recipes::paired_capture_recipe(&shape);
        // SAFETY: root and parse belong to this live upper-path invocation.
        let effective_cost = unsafe {
            add_native_having_cost(
                root,
                shape.estimated_output_rows,
                effective_path_cost(&shape, gucs::cost_multiplier()),
            )
        };
        #[cfg(feature = "pg_test")]
        {
            if CANDIDATE_OBSERVATIONS.with(|slot| slot.borrow().is_some()) {
                pgrx::notice!(
                    "candidate raw={raw_count}; final components={:?}; effective={effective_cost:?}",
                    shape.cost
                );
            }
            observed_cost = Some((
                format!("{:?}", shape_kernel_mode(&shape)),
                effective_cost.total,
                shape.cost.additional_aggregate_launches.get(),
            ));
        }
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
    #[cfg(feature = "pg_test")]
    CANDIDATE_OBSERVATIONS.with(|observations| {
        if let Some(observations) = observations.borrow_mut().as_mut() {
            observations.push(CandidateObservation {
                raw_count,
                native_cost,
                cost: observed_cost,
                outcome: result
                    .as_ref()
                    .map_or_else(|error| error.code(), |()| "added"),
            });
        }
    });
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
                let reason = if report_candidate_decline(raw_count, &decline) {
                    decline.code()
                } else {
                    RAW_CANDIDATE_NOT_APPLICABLE
                };
                decline_cache::insert(key, reason, dependencies);
            }
            if report_candidate_decline(raw_count, &decline) {
                record_decline(&decline, output_rel);
            }
            false
        }
    }
}

// Test-only observation of real default admission; this does not alter candidate
// order, costs, cache identities, path pruning, or production EXPLAIN output.
#[cfg(feature = "pg_test")]
pub fn capture_candidate_observations<T>(run: impl FnOnce() -> T) -> (T, String) {
    struct Restore(Option<Vec<CandidateObservation>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CANDIDATE_OBSERVATIONS.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore =
        Restore(CANDIDATE_OBSERVATIONS.with(|slot| slot.borrow_mut().replace(Vec::new())));
    let result = run();
    let observations = CANDIDATE_OBSERVATIONS.with(|slot| format!("{:?}", slot.borrow()));
    (result, observations)
}

#[cfg(feature = "pg_test")]
#[derive(Debug)]
struct CandidateObservation {
    raw_count: bool,
    native_cost: Option<f64>,
    cost: Option<(String, f64, f64)>,
    outcome: &'static str,
}

#[cfg(feature = "pg_test")]
thread_local! {
    static CANDIDATE_OBSERVATIONS: std::cell::RefCell<Option<Vec<CandidateObservation>>> = const {
        std::cell::RefCell::new(None)
    };
    static CANDIDATE_INPUT_OBSERVATIONS: std::cell::RefCell<Option<Vec<super::shape::CandidateInputObservations>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(feature = "pg_test")]
#[pgrx::pg_schema]
mod tests {
    use super::{CANDIDATE_INPUT_OBSERVATIONS, CANDIDATE_OBSERVATIONS};
    use pgrx::prelude::*;
    use std::fmt::Write as _;

    struct ResetObserver;
    impl Drop for ResetObserver {
        fn drop(&mut self) {
            CANDIDATE_OBSERVATIONS.with(|slot| *slot.borrow_mut() = None);
            CANDIDATE_INPUT_OBSERVATIONS.with(|slot| *slot.borrow_mut() = None);
        }
    }

    #[pg_test]
    fn pg_raw_count_dense_controls_preserve_independent_candidates() {
        observe_count_candidates(false);
    }

    #[pg_test]
    fn pg_raw_count_sparse_star_candidate_cost_observations() {
        observe_count_candidates(true);
    }

    #[pg_test]
    fn pg_raw_count_shared_inputs_equivalence_and_nonshareable_filters_having() {
        let _reset = ResetObserver;
        Spi::run("SET pg_accel.enabled=on; SET pg_accel.gpu_enabled=on; CREATE TEMP TABLE shared_input_fixture AS SELECT CASE WHEN i%97=0 THEN NULL ELSE (i%64)::int4 END k, (i%2)::int4 k2, CASE WHEN i%7=0 THEN NULL ELSE i%2=0 END value, CASE WHEN i%11=0 THEN NULL ELSE i%3<>0 END qualifies FROM generate_series(1,1000017)i; ANALYZE shared_input_fixture; SELECT pg_accel_pin('shared_input_fixture',ARRAY['k','k2','value','qualifies'])")
            .expect("shared-input fixture");
        for (query, shared) in [
            (
                "SELECT k AS key_alias,count(*) AS n FROM shared_input_fixture GROUP BY k",
                true,
            ),
            (
                "SELECT k,count(k) AS n FROM shared_input_fixture GROUP BY k",
                true,
            ),
            (
                "SELECT k,count(*) AS n FROM shared_input_fixture WHERE k>=0 GROUP BY k",
                true,
            ),
            (
                "SELECT k,count(value) FILTER(WHERE qualifies) AS n FROM shared_input_fixture GROUP BY k",
                false,
            ),
            (
                "SELECT k,count(*) FILTER(WHERE qualifies) AS n FROM shared_input_fixture GROUP BY k",
                false,
            ),
            (
                "SELECT k,count(k) FILTER(WHERE k>0) AS n FROM shared_input_fixture GROUP BY k",
                false,
            ),
            (
                "SELECT k,count(*) AS n FROM shared_input_fixture GROUP BY k HAVING count(*)>0",
                false,
            ),
            (
                "SELECT k FROM shared_input_fixture GROUP BY k HAVING count(*)>0",
                false,
            ),
            (
                "SELECT k,count(*) AS n FROM shared_input_fixture GROUP BY k HAVING count(*)::text LIKE '%'",
                false,
            ),
        ] {
            CANDIDATE_INPUT_OBSERVATIONS.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
            CANDIDATE_OBSERVATIONS.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
            Spi::run(&format!("EXPLAIN {query}")).expect("candidate extraction");
            let inputs = CANDIDATE_INPUT_OBSERVATIONS
                .with(|slot| slot.borrow_mut().take().expect("input observer"));
            let candidates = CANDIDATE_OBSERVATIONS
                .with(|slot| slot.borrow_mut().take().expect("candidate observer"));
            pgrx::notice!("shared-input query={query} inputs={inputs:?} candidates={candidates:?}");
            assert_eq!(candidates.len(), 2, "{query}: {candidates:?}");
            assert!(!candidates[0].raw_count && candidates[1].raw_count);
            assert_eq!(candidates[0].native_cost, candidates[1].native_cost);
            if query.contains("FILTER(WHERE qualifies)") {
                assert_eq!(candidates[1].outcome, "added", "{query}: {candidates:?}");
                if query.contains("count(value)") {
                    assert!(
                        inputs[0].legacy_bytecode_without_filter,
                        "original SQL FILTER must remain nonshareable even after bytecode lowering: {inputs:?}"
                    );
                }
            } else if !query.contains("FILTER") {
                assert_eq!(candidates[0].outcome, "added", "{query}: {candidates:?}");
            }
            assert_eq!(inputs.len(), 1, "{query}: {inputs:?}");
            assert_eq!(inputs[0].fresh_legacy, 1, "{query}: {inputs:?}");
            assert_eq!(inputs[0].clones, usize::from(shared), "{query}: {inputs:?}");
            assert_eq!(
                inputs[0].reused_raw,
                usize::from(shared),
                "{query}: {inputs:?}"
            );
            assert_eq!(
                inputs[0].fresh_raw,
                usize::from(!shared),
                "{query}: {inputs:?}"
            );
            // Full two-way native parity, independent of descriptor lowering.
            Spi::run(&format!("CREATE TEMP TABLE shared_actual AS {query}; SET pg_accel.enabled=off; CREATE TEMP TABLE shared_native AS {query}"))
                .expect("result materializations");
            let differences = Spi::get_one::<i64>("SELECT count(*) FROM ((TABLE shared_actual EXCEPT ALL TABLE shared_native) UNION ALL (TABLE shared_native EXCEPT ALL TABLE shared_actual)) d")
                .expect("full parity query").expect("difference count");
            assert_eq!(differences, 0, "{query}");
            Spi::run("DROP TABLE shared_actual,shared_native; SET pg_accel.enabled=on")
                .expect("case cleanup");
        }
        // LIKE's catalog support function is nonzero: this HAVING really takes
        // the cost_qual_eval support-callback path excluded from reuse.
        assert!(Spi::get_one::<bool>("SELECT prosupport<>0 AND provolatile<>'v' FROM pg_proc WHERE oid='textlike(text,text)'::regprocedure")
            .expect("support catalog").expect("textlike identity"));
        Spi::run("SELECT pg_accel_unpin('shared_input_fixture'); DROP TABLE shared_input_fixture")
            .expect("fixture cleanup");
    }

    #[pg_test]
    fn pg_raw_count_shared_inputs_keep_raw_cache_hit_independent() {
        let _reset = ResetObserver;
        Spi::run("SET pg_accel.enabled=on; SET pg_accel.gpu_enabled=on; CREATE TEMP TABLE shared_cache_fixture AS SELECT (i%64)::int4 k,(i%3)::int4 k2 FROM generate_series(1,1000017)i; ANALYZE shared_cache_fixture; SELECT pg_accel_pin('shared_cache_fixture',ARRAY['k','k2'])")
            .expect("cache fixture");
        for repeat in 0..2 {
            CANDIDATE_INPUT_OBSERVATIONS.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
            CANDIDATE_OBSERVATIONS.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
            Spi::run("EXPLAIN SELECT k,k2,count(*) FROM shared_cache_fixture GROUP BY k,k2")
                .expect("two-key legacy candidate");
            let inputs = CANDIDATE_INPUT_OBSERVATIONS
                .with(|slot| slot.borrow_mut().take().expect("input observer"));
            let candidates = CANDIDATE_OBSERVATIONS
                .with(|slot| slot.borrow_mut().take().expect("candidate observer"));
            assert_eq!(inputs.len(), 1, "{inputs:?}");
            assert_eq!(inputs[0].clones, 1, "{inputs:?}");
            assert_eq!(inputs[0].fresh_legacy, 1, "{inputs:?}");
            assert_eq!(inputs[0].fresh_raw, 0, "{inputs:?}");
            assert_eq!(inputs[0].reused_raw, usize::from(repeat == 0), "{inputs:?}");
            assert!(!candidates[0].raw_count);
            assert_eq!(candidates[0].outcome, "added", "{candidates:?}");
            assert_eq!(candidates.len(), if repeat == 0 { 2 } else { 1 });
        }
        Spi::run("SELECT pg_accel_unpin('shared_cache_fixture'); DROP TABLE shared_cache_fixture")
            .expect("cache fixture cleanup");
    }

    fn observe_count_candidates(sparse: bool) {
        let _reset = ResetObserver;
        let mut receipts = String::new();
        let mut later_declines = 0;
        let mut legacy_cache_hits = 0;
        Spi::run("RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; RESET pg_accel.resident_memory_budget_mb; SET pg_accel.enabled=on; SET pg_accel.gpu_enabled=on")
            .expect("default candidate admission");
        for kind in ["int2", "int4", "int8"] {
            for rows in [1_000_017, 2_000_017] {
                let groups = if sparse { 16_388 } else { 64 };
                let key = if sparse {
                    match kind {
                        "int2" => "g*3-24582",
                        "int4" => "g*200003-1638844582",
                        _ => "g*100000000003-819400000024582",
                    }
                } else {
                    "g-32"
                };
                // Exact arithmetic-v1 64-group benchmark control, including its
                // unused Boolean columns and nullable skewed key distribution.
                Spi::run(&format!("CREATE TEMP TABLE bench_raw_integer_count(k {kind},value boolean,qualifies boolean); INSERT INTO bench_raw_integer_count SELECT CASE WHEN i%97=0 THEN NULL::{kind} ELSE ({key})::{kind} END, CASE WHEN i%7=0 THEN NULL::boolean ELSE i%2=0 END, CASE WHEN g=0 THEN false WHEN i%11=0 THEN NULL::boolean ELSE i%3<>0 END FROM (SELECT i,CASE WHEN i>{groups} AND i%5=0 THEN 0 ELSE (i-1)%{groups} END g FROM generate_series(1::bigint,{rows})s(i))fixture; ANALYZE bench_raw_integer_count; SELECT pg_accel_pin('bench_raw_integer_count',ARRAY['k'])"))
                    .expect("exact low-cardinality control fixture");
                let mut first_legacy_cost_declined = false;
                for repeat in 0..3 {
                    // Costs are device-derived. Observe a genuine first-pass
                    // decline instead of prescribing one from the row count.
                    let cached_legacy_decline = first_legacy_cost_declined && repeat > 0;
                    CANDIDATE_OBSERVATIONS.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
                    CANDIDATE_INPUT_OBSERVATIONS.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
                    Spi::run(
                        "EXPLAIN SELECT k,count(*) AS n FROM bench_raw_integer_count GROUP BY k",
                    )
                    .expect("default repeated planning");
                    let observations = CANDIDATE_OBSERVATIONS
                        .with(|slot| slot.borrow_mut().take().expect("active observer"));
                    let inputs = CANDIDATE_INPUT_OBSERVATIONS
                        .with(|slot| slot.borrow_mut().take().expect("active input observer"));
                    assert_eq!(inputs.len(), 1, "{inputs:?}");
                    if cached_legacy_decline {
                        legacy_cache_hits += 1;
                        assert_eq!(inputs[0].clones, 0, "{inputs:?}");
                        assert_eq!(
                            inputs[0].fresh_legacy, 0,
                            "cached legacy decline: {inputs:?}"
                        );
                        assert_eq!(inputs[0].reused_raw, 0, "{inputs:?}");
                        assert_eq!(
                            inputs[0].fresh_raw, 1,
                            "raw must still extract after legacy cache decline: {inputs:?}"
                        );
                    } else {
                        assert_eq!(inputs[0].clones, 1, "{inputs:?}");
                        assert_eq!(inputs[0].fresh_legacy, 1, "{inputs:?}");
                        assert_eq!(inputs[0].reused_raw, 1, "{inputs:?}");
                        assert_eq!(inputs[0].fresh_raw, 0, "{inputs:?}");
                    }
                    pgrx::notice!(
                        "candidate control kind={kind} rows={rows} repeat={repeat}: {observations:?}"
                    );
                    writeln!(
                        receipts,
                        "kind={kind} rows={rows} repeat={repeat}: {observations:?}"
                    )
                    .expect("format candidate receipt");
                    if cached_legacy_decline {
                        assert_eq!(observations.len(), 1, "{observations:?}");
                        assert!(observations[0].raw_count);
                    } else {
                        assert_eq!(observations.len(), 2, "{observations:?}");
                        assert!(!observations[0].raw_count);
                        assert!(observations[1].raw_count);
                        assert_eq!(observations[0].native_cost, observations[1].native_cost);
                        if repeat == 0 && observations[0].outcome == "generic_cost_not_competitive"
                        {
                            first_legacy_cost_declined = true;
                            later_declines += 1;
                            assert!(
                                observations[0].cost.is_some(),
                                "decline occurs after neutral extraction and independent costing"
                            );
                        }
                    }
                    for observation in &observations {
                        if sparse && !observation.raw_count {
                            continue;
                        }
                        assert_eq!(observation.outcome, "added", "{observations:?}");
                        let (_, total, launches) =
                            observation.cost.as_ref().expect("exact candidate cost");
                        assert!(total.is_finite() && *total > 0.0);
                        assert!(launches.is_finite() && *launches >= 0.0);
                    }
                    if !sparse {
                        assert_ne!(
                            observations[0].cost.as_ref().expect("legacy cost").0,
                            observations[1].cost.as_ref().expect("raw cost").0
                        );
                    }
                }
                Spi::run("SELECT pg_accel_unpin('bench_raw_integer_count'); DROP TABLE bench_raw_integer_count")
                    .expect("release exact control fixture");
            }
        }
        pgrx::notice!(
            "shared-input coverage: later_legacy_cost_declines={later_declines} cached_legacy_fresh_raw={legacy_cache_hits}"
        );
        writeln!(receipts, "shared-input coverage: later_legacy_cost_declines={later_declines} cached_legacy_fresh_raw={legacy_cache_hits}").expect("coverage receipt");
        if let Ok(path) = std::env::var("PG_ACCEL_CANDIDATE_OBSERVATION_PATH") {
            std::fs::write(path, receipts).expect("durable test-owned candidate receipt");
        }
    }
}

#[cfg(test)]
mod unit_tests {
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

    #[test]
    fn nonmatching_raw_candidate_does_not_replace_legacy_diagnostics() {
        let ineligible = AdmissionDecline::Shape(ShapeDecline::InvalidSpec("not a raw key".into()));
        assert!(!report_candidate_decline(true, &ineligible));
        assert!(report_candidate_decline(false, &ineligible));
        assert!(report_candidate_decline(
            true,
            &AdmissionDecline::Shape(ShapeDecline::Codec("malformed path serialization".into()))
        ));
        let cost = AdmissionDecline::CostNotCompetitive {
            gpu_cost: 2.0,
            native_cost: 1.0,
            required_cost: 0.8,
        };
        assert!(report_candidate_decline(true, &cost));
        assert!(report_candidate_decline(
            true,
            &AdmissionDecline::DescriptorCapability {
                detail: "eligible raw runtime contract failed".into(),
            }
        ));
        assert_ne!(RAW_CANDIDATE_NOT_APPLICABLE, cost.code());
    }

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
            estimated_dense_key_cardinalities: Some(vec![]),
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
    fn normal_spatial_row_envelope_requires_exact_loaded_evidence_not_a_fixture_size() {
        assert!(normal_spatial_promotion_rows(Some(1_000_000)));
        assert!(!normal_spatial_promotion_rows(None));
        assert!(!normal_spatial_promotion_rows(Some(0)));
        assert!(normal_spatial_promotion_rows(Some(999_999)));
        assert!(normal_spatial_promotion_rows(Some(1_000_001)));
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
            resident_required_columns: loaded.then_some(vec![(1, pg_sys::INT4OID)]),
            resident_rows: loaded.then_some(100_000),
            selected_columns_have_no_null_sidecars: loaded.then_some(true),
            resident_numeric_columns: loaded.then(Vec::new),
            resident_nullable_columns: loaded.then(Vec::new),
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

    #[test]
    fn dense_count_placeholder_pricing_uses_selected_physical_candidate() {
        let rows = 1_000_017;
        let mut dense = dense_sum_count_shape(rows);
        dense.spec.measures.remove(0);
        dense.descriptor_measures.projected_measure_count = 1;
        dense.descriptor_measures.descriptor_measure_count = 1;
        dense.required_relations[0].attnos = vec![1];
        dense.residency.relations[0].attnos = vec![1];
        dense.estimated_dense_key_cardinalities = Some(vec![64]);
        assert_eq!(dense.spec.group_keys[0].encoding, GroupKeyEncoding::Hash);
        assert!(dense.supports_budget_selected_dense());
        let mut raw = dense.clone();
        raw.descriptor_resolution = DescriptorResolution::BeginTimeArtifacts {
            dictionary_keys: Vec::new(),
            derived_keys: Vec::new(),
            joins: Vec::new(),
            grouping_mode: DescriptorGroupingMode::Hash,
            max_group_count: 100_000,
        };
        assert_eq!(dense.spec, raw.spec);
        assert!(!raw.supports_budget_selected_dense());
        assert_eq!(
            raw.execution_selection(),
            crate::engine::executor::agg::AggExecutionSelection::RawIntegerCount
        );
        let model = model();
        let mut evidence = estimate(true, 4096);
        evidence.resident_rows = Some(rows);
        let selected = select_dense_execution(
            &dense.spec,
            &[],
            64,
            rows as usize,
            model.executor.gpu_reduce_max_chunk.get(),
            model.executor.gpu_grouped_agg_one_shot_max_rows.get(),
            u64::MAX,
        )
        .expect("real metadata decision")
        .expect("budget fits");
        let expected = dense_count_aggregate_cost(
            dense_count_work_requirements(&dense.spec, &[], 64, rows as usize, selected)
                .expect("real selected work facts"),
            &model,
        )
        .expect("bounded cost");
        let old = dense.cost.aggregate;
        let raw_old = raw.cost.aggregate;
        let policy = AdmissionPolicy {
            auto_load: false,
            budget_bytes: u64::MAX,
            budget_snapshot: budget_snapshot(0, 0, 0),
        };
        for _ in 0..2 {
            apply_exact_residency(&mut dense, std::slice::from_ref(&evidence), policy, &model)
                .expect("dense placeholder admission");
            assert_eq!(dense.cost.aggregate, expected);
            assert_ne!(dense.cost.aggregate, old);
            apply_exact_residency(&mut raw, std::slice::from_ref(&evidence), policy, &model)
                .expect("raw independent admission");
            assert_eq!(raw.cost.aggregate, raw_old);
        }
        raw.descriptor_resolution = DescriptorResolution::Ready;
        assert!(!raw.supports_budget_selected_dense());
    }

    #[test]
    fn dense_count_aggregate_prices_only_checked_work() {
        let work = DenseCountWork {
            calls: 3,
            facts: crate::engine::spec::abi::PgaccelGroupedAggCountWorkV1 {
                row_counter_updates: 100,
                value_null_checks: 20,
                serial_partial_counter_reads: 7,
                ..Default::default()
            },
        };
        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_op_cost_reduce = 2.0;
        limits.gpu_op_cost_filter = 3.0;
        limits.gpu_dense_count_serial_work_cost = 5.0;
        let model = TypedCostModel::from_limits(&limits);
        assert_eq!(
            dense_count_aggregate_cost(work, &model)
                .expect("bounded cost")
                .get(),
            295.0
        );
        let mut more_calls = work;
        more_calls.calls = 100;
        assert_eq!(
            dense_count_aggregate_cost(more_calls, &model)
                .expect("calls are priced separately")
                .get(),
            295.0
        );
        limits.gpu_dense_count_serial_work_cost = 10.0;
        assert_eq!(
            dense_count_aggregate_cost(work, &TypedCostModel::from_limits(&limits))
                .expect("doubled serial cost")
                .get(),
            330.0,
            "doubling serial price changes only the seven serial work units"
        );
        limits.gpu_dense_count_serial_work_cost = 0.0;
        assert_eq!(
            dense_count_aggregate_cost(work, &TypedCostModel::from_limits(&limits))
                .expect("zero serial cost")
                .get(),
            260.0
        );
        let mut overflowing = work;
        overflowing.facts.global_counter_clear_entries = u64::MAX;
        assert!(dense_count_aggregate_cost(overflowing, &model).is_err());
        let mut masked = work;
        masked.facts.sql_mask_checks = 17;
        assert_eq!(
            dense_count_aggregate_cost(masked, &model)
                .expect("SQL reads")
                .get(),
            346.0
        );
    }

    #[test]
    fn dense_integer_where_count_reads_follow_selected_workers_not_calls() {
        let mut shape = dense_sum_count_shape(2051);
        shape.spec.measures.truncate(1);
        shape.spec.measures[0].outputs[0].kind = AggregateKind::Count;
        shape.spec.fact_filter = FilterSpec::Mask {
            input: ColumnRef {
                relation_oid: 42,
                attno: 3,
                type_oid: 16,
            },
            kind: crate::engine::spec::MaskKind::Sql,
        };
        let mut decision = select_dense_execution(&shape.spec, &[], 65, 2051, 1024, 1, u64::MAX)
            .expect("native metadata")
            .expect("fits");
        // The direct COUNT descriptor naturally selects COUNT. The helper must
        // reject it: its exact work facts already contain the SQL checks.
        assert_eq!(
            dense_integer_count_where_reads(&shape, decision, 2051),
            None
        );
        decision.native_mode = GroupedAggKernelMode::ParallelDenseInteger;
        for calls in [1, 4, 29] {
            decision.calls = calls;
            for rows in [0, 2051, 1_000_017, 2_000_017] {
                assert_eq!(
                    dense_integer_count_where_reads(&shape, decision, rows),
                    Some(rows)
                );
            }
        }
        let original = shape.clone();
        shape.spec.measures[0].expression = MeasureExpr::Bytecode {
            inputs: vec![],
            program: vec![
                crate::engine::spec::scalar::SCALAR_PROGRAM_MAGIC,
                2,
                0,
                1,
                42,
            ],
            result_type_oid: u32::from(pg_sys::INT4OID),
        };
        let actual = select_dense_execution(&shape.spec, &[], 65, 2051, 1024, 1, u64::MAX)
            .expect("native scalar COUNT metadata")
            .expect("fits");
        assert_eq!(
            actual.native_mode,
            GroupedAggKernelMode::ParallelDenseInteger
        );
        assert_eq!(
            dense_integer_count_where_reads(&shape, actual, 2051),
            Some(2051)
        );
        shape = original.clone();
        shape.spec.fact_filter = FilterSpec::None;
        assert_eq!(
            dense_integer_count_where_reads(&shape, decision, 2051),
            None
        );
        shape = original.clone();
        shape.spec.measures[0].outputs[0].kind = AggregateKind::Sum;
        assert_eq!(
            dense_integer_count_where_reads(&shape, decision, 2051),
            None
        );
        shape = original.clone();
        shape.spec.measures.push(shape.spec.measures[0].clone());
        assert_eq!(
            dense_integer_count_where_reads(&shape, decision, 2051),
            None
        );
        shape = original;
        for mode in [
            GroupedAggKernelMode::ParallelDenseCount,
            GroupedAggKernelMode::ParallelDenseNumeric,
            GroupedAggKernelMode::ParallelHash,
        ] {
            decision.native_mode = mode;
            assert_eq!(
                dense_integer_count_where_reads(&shape, decision, 2051),
                None
            );
        }
    }

    fn expected_exact_load(rows: u64, bytes: u64, fixed_width: bool) -> PgCost {
        let model = model();
        PgCost::new(unamortized_resident_load_cost(rows, bytes, fixed_width, &model) / 4.0)
    }

    fn count_native_execution_bytes() -> u64 {
        scalar_planning_memory_with(
            &count_shape(),
            &[estimate(true, 0)],
            &model(),
            dense_descriptor_memory_requirements,
        )
        .expect("native COUNT execution quote")
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
            retained_execution_bytes: 0,
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
        shape.estimated_dense_key_cardinalities = Some(vec![100]);
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
    fn dense_sum_count_admits_proved_bounded_calls_above_one_shot() {
        let model = model();
        let maximum = model.executor.gpu_grouped_agg_one_shot_max_rows;
        let maximum_u64 = u64::try_from(maximum.get()).expect("test row limit fits u64");

        assert_eq!(
            dense_lifecycle_call_count(312_500, Rows::new(64_000), Rows::new(1_000_000),),
            Some(6),
            "costing must retain the one-CU synchronous cap after bounded execution starts"
        );

        let mut at_maximum = dense_sum_count_shape(maximum_u64);
        apply_resident_evidence(&mut at_maximum, maximum_u64, true);
        assert_eq!(at_maximum.cost_gate, ShapeCostGate::Eligible);
        assert_eq!(at_maximum.cost.additional_aggregate_launches, PgCost::ZERO);

        let above_u64 = maximum_u64 + 1;
        let mut above_maximum = dense_sum_count_shape(above_u64);
        apply_resident_evidence(&mut above_maximum, above_u64, true);
        assert_eq!(above_maximum.cost_gate, ShapeCostGate::Eligible);
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
        assert_eq!(first_use.cost_gate, ShapeCostGate::Eligible);
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
    fn one_compute_unit_dense_admission_requires_a_budget_proved_bounded_lifecycle() {
        let limits = DeviceLimits::from_profile(&cost::PlatformProfile {
            cpu_cores: 8,
            has_gpu: true,
            estimated_gpu_gflops: 2.0,
            compute_units: 1,
            device_name: "one-compute-unit fixture".into(),
            backend_name: "metal".into(),
            gpu_max_alloc_bytes: 3_758_096_384,
            has_native_fp64: false,
        });
        let model = TypedCostModel::from_limits(&limits);
        let floor = model.planner.gpu_hash_agg_min_rows;
        let cap = model.executor.gpu_grouped_agg_one_shot_max_rows;
        assert!(floor > cap, "CI device has no one-shot admission interval");
        let rows = (floor.get() as u64)
            .checked_mul(2)
            .expect("fixture row domain");
        let policy = AdmissionPolicy {
            auto_load: false,
            budget_bytes: u64::MAX,
            budget_snapshot: budget_snapshot(0, 0, 0),
        };
        let mut evidence = estimate(true, rows * 8);
        evidence.resident_rows = Some(rows);
        let mut shape = dense_sum_count_shape(rows);
        apply_exact_residency(&mut shape, &[evidence.clone()], policy, &model)
            .expect("native metadata selector proves bounded memory");
        assert_eq!(shape.cost_gate, ShapeCostGate::Eligible);
        let calls = dense_lifecycle_call_count(rows, cap, model.executor.gpu_reduce_max_chunk)
            .expect("checked compute-derived call count");
        assert!(calls > 1);
        assert_eq!(
            shape.cost.additional_aggregate_launches,
            PgCost::new((calls - 1) as f64 * cost::GPU_LAUNCH_OVERHEAD)
        );

        let mut unproved = dense_sum_count_shape(rows);
        apply_exact_residency_with_scalar_memory(
            &mut unproved,
            &[evidence.clone()],
            policy,
            &model,
            |_, _, _, _| Ok((0, None)),
        )
        .expect("isolate unproved lifecycle guard");
        assert_eq!(
            unproved.cost_gate,
            ShapeCostGate::DenseOneShotRowsExceedDeviceMaximum {
                fact_rows: Rows::new(rows as usize),
                maximum: cap,
            }
        );

        let below = floor.get() as u64 - 1;
        let mut shape = dense_sum_count_shape(below);
        evidence.resident_rows = Some(below);
        apply_exact_residency(&mut shape, &[evidence.clone()], policy, &model)
            .expect("bounded memory does not bypass row floor");
        assert_eq!(
            shape.cost_gate,
            ShapeCostGate::FactRowsBelowDeviceMinimum {
                estimated: Rows::new(below as usize),
                required: floor,
            }
        );

        let mut shape = dense_sum_count_shape(rows);
        evidence.resident_rows = Some(rows);
        assert!(matches!(
            apply_exact_residency(
                &mut shape,
                &[evidence],
                AdmissionPolicy {
                    budget_bytes: 0,
                    ..policy
                },
                &model
            ),
            Err(AdmissionDecline::ResidencyBudgetExceeded { .. })
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

    #[test]
    fn h3_parent_forecast_uses_loaded_rows_and_exact_execution_budget() {
        for catalog_rows in [1, 1_000_000_000] {
            for short in [false, true] {
                let mut shape = h3_parent_shape();
                shape.residency.relations[0].estimated_rows = catalog_rows;
                shape.cost_gate = ShapeCostGate::Eligible;
                let mut evidence = estimate(true, 4_096);
                evidence.resident_rows = Some(1_000);
                let total = 4_096 + 8_000 + 71;
                let result = apply_exact_residency_with_memory(
                    &mut shape,
                    &[evidence],
                    AdmissionPolicy {
                        auto_load: false,
                        budget_bytes: total - u64::from(short),
                        budget_snapshot: budget_snapshot(0, 0, 0),
                    },
                    &model(),
                    Some(H3PlanningMemory {
                        cached_owner: false,
                        bytes: 71,
                        capacity: 4,
                    }),
                    |_, _, _, _| panic!("hash must use its exact memory quote"),
                );
                if short {
                    assert!(matches!(
                        result,
                        Err(AdmissionDecline::ResidencyBudgetExceeded {
                            selected_raw_bytes: 4_096,
                            derived_artifact_bytes: 8_000,
                            prospective_execution_bytes: 71,
                            projected_final_bytes: 12_167,
                            ..
                        })
                    ));
                } else {
                    assert_eq!(result, Ok(()), "catalog row estimate {catalog_rows}");
                }
            }
        }
    }

    #[test]
    fn h3_adaptive_capacity_bounds_the_seed_without_rejecting_larger_inputs() {
        for exact_rows in [499_999, 500_000, 500_001] {
            let mut shape = h3_parent_shape();
            let GroupKeySource::H3CellToParent { resolution, .. } =
                &mut shape.spec.group_keys[0].source
            else {
                panic!("H3 fixture");
            };
            *resolution = 15;
            let mut model = model();
            model.memory.gpu_h3_max_groups = Rows::new(500_000);
            shape.cost_gate = ShapeCostGate::GroupsExceedDeviceMaximum {
                estimated: Rows::new(1_000_000),
                maximum: Rows::new(500_000),
            };
            assert_eq!(
                stable_device_cost_gate_before_residency(&shape, &model),
                None
            );
            let mut evidence = estimate(true, 4_096);
            evidence.resident_rows = Some(exact_rows);
            // This unit isolates the adaptive initial-capacity consumer from the live
            // catalog/current-artifact resolver. The latter is covered by
            // backend tests; invoking SPI from an ordinary Rust test is invalid.
            let result = apply_exact_residency_with_memory(
                &mut shape,
                &[evidence],
                AdmissionPolicy {
                    auto_load: false,
                    budget_bytes: u64::MAX,
                    budget_snapshot: budget_snapshot(0, 0, 0),
                },
                &model,
                Some(H3PlanningMemory {
                    cached_owner: false,
                    bytes: 0,
                    capacity: usize::try_from(exact_rows.min(500_000))
                        .expect("bounded fixture capacity"),
                }),
                |_, _, _, _| panic!("hash quote must bypass dense memory"),
            );
            assert_eq!(result, Ok(()));
            assert_eq!(shape.cost_gate, ShapeCostGate::Eligible);
        }
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
                resident_required_columns: None,
                resident_rows: None,
                selected_columns_have_no_null_sidecars: None,
                resident_numeric_columns: None,
                resident_nullable_columns: None,
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
                budget_bytes: 4096 + count_native_execution_bytes(),
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
                budget_bytes: 4096 + count_native_execution_bytes(),
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
                projected_final_bytes,
                prospective_execution_bytes,
                budget_bytes: 8192,
                ..
            } if prospective_execution_bytes == count_native_execution_bytes()
                && projected_final_bytes == 9096 + prospective_execution_bytes
        ));
    }

    #[test]
    fn numeric_work_accounts_for_digits_chunk_tails_and_limits() {
        for rows in [0, 1, 99, 4093, 100017] {
            for chunk in [1, 7, 4093, 1000000] {
                for max_digits in [1, 5, 36865, 4 * 65535 + 1] {
                    let mut data_launches = 0;
                    let mut calls = 0;
                    let mut remaining = rows;
                    while remaining != 0 {
                        let batch = remaining.min(chunk);
                        let launch_rows = (1_048_576 / max_digits).max(1);
                        let mut batch_rows = batch;
                        while batch_rows != 0 {
                            batch_rows -= batch_rows.min(launch_rows);
                            data_launches += 1;
                        }
                        calls += 1;
                        remaining -= batch;
                    }
                    assert_eq!(
                        numeric_work_units(rows, 2, rows * 3, max_digits, chunk),
                        Some((rows * 6, data_launches + calls))
                    );
                }
            }
        }
        assert!(numeric_work_units(u64::MAX, 1, 0, 1, 1).is_none());
        assert!(numeric_work_units(1, 0, 0, 1, 1).is_none());
        assert!(numeric_work_units(1, 5, 0, 1, 1).is_none());
        assert!(numeric_work_units(1, 1, u64::MAX, 1, 1).is_none());
        assert!(numeric_work_units(1, 1, 0, 0, 1).is_none());
        assert!(numeric_work_units(1, 1, 0, 1, 0).is_none());
    }

    #[test]
    fn numeric_planning_charges_exact_digits_memory_and_rejects_missing_evidence() {
        let mut shape = count_shape();
        shape.spec.measures[0].expression = MeasureExpr::Column(ColumnRef {
            relation_oid: 42,
            attno: 1,
            type_oid: 1700,
        });
        shape.spec.measures[0].outputs =
            [AggregateKind::Sum, AggregateKind::Avg, AggregateKind::Count]
                .into_iter()
                .map(|kind| AggregateOutput {
                    source: AggregateSource::Value,
                    kind,
                })
                .collect();
        let mut estimate = estimate(true, 4096);
        estimate.resident_numeric_columns =
            Some(vec![crate::engine::residency::ResidentNumericColumnStats {
                attno: 1,
                digit_count: 200000,
                bounds: crate::engine::residency::NumericDigitBounds {
                    min_weight: -1,
                    max_weight: 0,
                    max_ndigits: 2,
                },
            }]);
        let model = model();
        let query = |spec: &crate::engine::spec::AggQuerySpec,
                     columns: &[crate::engine::residency::ResidentNumericColumnStats],
                     nullable: &[i16],
                     groups| {
            assert_eq!(
                spec.measures.len(),
                1,
                "shared SUM/AVG/COUNT is one transition state"
            );
            assert_eq!(columns[0].digit_count, 200000);
            assert!(nullable.is_empty());
            assert_eq!(groups, 1);
            Ok(crate::gpu::GroupedAggMemoryRequirements {
                workspace_bytes: 4096,
                output_bytes: 49,
                total_bytes: 4145,
            })
        };
        let result = numeric_planning_resources_with(&shape, &[estimate.clone()], &model, query)
            .expect("numeric resources")
            .expect("numeric shape");
        assert_eq!(result.bytes, 4145);
        assert_eq!(
            result.aggregate.get(),
            512.0_f64.mul_add(
                model.coefficients.gpu_op_cost_reduce.get(),
                400000.0 * model.coefficients.gpu_op_cost_reduce.get()
            )
        );
        assert!(result.launches.get() > 0.0);
        let mut count_only = shape.clone();
        count_only.spec.measures[0]
            .outputs
            .retain(|output| output.kind == AggregateKind::Count);
        let count_resources =
            numeric_planning_resources_with(&count_only, &[estimate.clone()], &model, query)
                .expect("count-only pricing")
                .expect("numeric resources");
        assert_eq!(
            count_resources.aggregate.get(),
            512.0_f64.mul_add(
                model.coefficients.gpu_op_cost_reduce.get(),
                200000.0 * model.coefficients.gpu_op_cost_reduce.get()
            )
        );
        let mut wide_count_estimate = estimate.clone();
        let wide_stats = &mut wide_count_estimate
            .resident_numeric_columns
            .as_mut()
            .expect("numeric stats")[0];
        wide_stats.digit_count = u64::MAX;
        wide_stats.bounds.max_ndigits = 36864;
        let count_query = |spec: &crate::engine::spec::AggQuerySpec,
                           _: &[crate::engine::residency::ResidentNumericColumnStats],
                           nullable: &[i16],
                           groups| {
            // Query only tests the cost model; native layout tests separately
            // prove count-only workspace is also independent of digit extents.
            query(
                spec,
                estimate
                    .resident_numeric_columns
                    .as_ref()
                    .expect("metadata"),
                nullable,
                groups,
            )
        };
        let wide_resources = numeric_planning_resources_with(
            &count_only,
            &[wide_count_estimate],
            &model,
            count_query,
        )
        .expect("count-only ignores digit volume")
        .expect("numeric resources");
        assert_eq!(wide_resources.aggregate, count_resources.aggregate);
        assert_eq!(wide_resources.launches, count_resources.launches);
        let mut mixed = shape.clone();
        let mut integer = mixed.spec.measures[0].clone();
        integer.expression = MeasureExpr::Column(ColumnRef {
            relation_oid: 42,
            attno: 2,
            type_oid: 23,
        });
        mixed.spec.measures.push(integer);
        let mixed_query = |_: &crate::engine::spec::AggQuerySpec,
                           _: &[crate::engine::residency::ResidentNumericColumnStats],
                           _: &[i16],
                           _: usize| {
            Ok(crate::gpu::GroupedAggMemoryRequirements {
                workspace_bytes: 8192,
                output_bytes: 98,
                total_bytes: 8290,
            })
        };
        let mixed_cost =
            numeric_planning_resources_with(&mixed, &[estimate.clone()], &model, mixed_query)
                .expect("integer needs no decimal metadata")
                .expect("mixed numeric mode");
        assert!(mixed_cost.aggregate.get() > result.aggregate.get());
        let mut bigint = shape.clone();
        bigint.spec.measures[0].expression = MeasureExpr::Column(ColumnRef {
            relation_oid: 42,
            attno: 1,
            type_oid: 20,
        });
        let mut bigint_estimate = estimate.clone();
        bigint_estimate.resident_numeric_columns = Some(vec![]);
        let bigint_query = |_: &crate::engine::spec::AggQuerySpec,
                            columns: &[crate::engine::residency::ResidentNumericColumnStats],
                            _: &[i16],
                            groups: usize| {
            assert!(
                columns.is_empty(),
                "raw bigint has no resident decimal metadata"
            );
            assert_eq!(groups, 1);
            Ok(crate::gpu::GroupedAggMemoryRequirements {
                workspace_bytes: 8192,
                output_bytes: 98,
                total_bytes: 8290,
            })
        };
        let bigint_cost = numeric_planning_resources_with(
            &bigint,
            &[bigint_estimate.clone()],
            &model,
            bigint_query,
        )
        .expect("raw bigint pricing")
        .expect("decimal accumulator family");
        assert_eq!(
            bigint_cost.aggregate.get(),
            (8192.0_f64 / 8.0).mul_add(
                model.coefficients.gpu_op_cost_reduce.get(),
                bigint_estimate.resident_rows.expect("rows") as f64
                    * 7.0
                    * model.coefficients.gpu_op_cost_reduce.get(),
            ),
            "one row plus one value plus five conservative digit transitions"
        );
        bigint_estimate.resident_rows = Some((i64::MAX as u64) / (1_u64 << 31) + 1);
        assert!(
            numeric_planning_resources_with(
                &bigint,
                &[bigint_estimate.clone()],
                &model,
                bigint_query,
            )
            .is_ok(),
            "bigint decimal sums do not inherit the INT4 prefix bound"
        );
        bigint_estimate.resident_rows = Some(i64::MAX as u64 / 10000 + 1);
        assert!(
            numeric_planning_resources_with(&bigint, &[bigint_estimate], &model, bigint_query,)
                .is_err(),
            "decimal lifecycle bound remains enforced"
        );
        let mut where_shape = mixed.clone();
        where_shape.spec.fact_filter = FilterSpec::Ranges {
            input: ColumnRef {
                relation_oid: 42,
                attno: 2,
                type_oid: 23,
            },
            ranges: vec![
                crate::engine::spec::ScalarRange {
                    lo: crate::engine::spec::ScalarValue::I32(1),
                    hi: crate::engine::spec::ScalarValue::I32(1),
                },
                crate::engine::spec::ScalarRange {
                    lo: crate::engine::spec::ScalarValue::I32(3),
                    hi: crate::engine::spec::ScalarValue::I32(5),
                },
            ],
        };
        let where_cost =
            numeric_planning_resources_with(&where_shape, &[estimate.clone()], &model, mixed_query)
                .expect("WHERE pricing")
                .expect("numeric mode");
        assert_eq!(
            where_cost.aggregate.get(),
            (3.0 * estimate.resident_rows.expect("rows") as f64).mul_add(
                model.coefficients.gpu_op_cost_filter.get(),
                mixed_cost.aggregate.get()
            )
        );
        let mut too_many = estimate.clone();
        too_many.resident_rows = Some((i64::MAX as u64) / (1_u64 << 31) + 1);
        assert!(
            numeric_planning_resources_with(&mixed, &[too_many], &model, mixed_query,).is_err()
        );
        let mut repriced = model.clone();
        repriced.coefficients.gpu_op_cost_hash_agg = PgCost::new(1000.0);
        let dense = numeric_planning_resources_with(&shape, &[estimate.clone()], &repriced, query)
            .expect("dense decimal cost")
            .expect("numeric resources");
        assert_eq!(
            dense.aggregate, result.aggregate,
            "dense limbs do not build/probe a hash table"
        );
        repriced.coefficients.gpu_op_cost_reduce =
            PgCost::new(model.coefficients.gpu_op_cost_reduce.get() * 2.0);
        let repriced =
            numeric_planning_resources_with(&shape, &[estimate.clone()], &repriced, query)
                .expect("repriced decimal cost")
                .expect("numeric resources");
        assert_eq!(repriced.aggregate.get(), result.aggregate.get() * 2.0);
        assert_eq!(repriced.launches, result.launches);
        let mut filtered = shape.clone();
        filtered.spec.measures[0].filter = FilterSpec::Ranges {
            input: ColumnRef {
                relation_oid: 42,
                attno: 2,
                type_oid: 23,
            },
            ranges: vec![crate::engine::spec::ScalarRange {
                lo: crate::engine::spec::ScalarValue::I32(1),
                hi: crate::engine::spec::ScalarValue::I32(3),
            }],
        };
        let filtered =
            numeric_planning_resources_with(&filtered, &[estimate.clone()], &model, query)
                .expect("numeric predicate costing")
                .expect("numeric resources");
        assert_eq!(
            filtered.bytes, result.bytes,
            "fused predicates allocate no mask"
        );
        assert_eq!(
            filtered.launches, result.launches,
            "fused predicates add no launch"
        );
        assert_eq!(
            filtered.aggregate.get(),
            (estimate.resident_rows.expect("exact rows") as f64 * 2.0).mul_add(
                model.coefficients.gpu_op_cost_filter.get(),
                result.aggregate.get()
            )
        );
        let mut singleton_shape = shape.clone();
        singleton_shape.spec.measures[0].filter = FilterSpec::Ranges {
            input: ColumnRef {
                relation_oid: 42,
                attno: 2,
                type_oid: 23,
            },
            ranges: vec![
                crate::engine::spec::ScalarRange {
                    lo: crate::engine::spec::ScalarValue::I32(i32::MIN),
                    hi: crate::engine::spec::ScalarValue::I32(i32::MIN),
                },
                crate::engine::spec::ScalarRange {
                    lo: crate::engine::spec::ScalarValue::I32(1),
                    hi: crate::engine::spec::ScalarValue::I32(2),
                },
                crate::engine::spec::ScalarRange {
                    lo: crate::engine::spec::ScalarValue::I32(i32::MAX),
                    hi: crate::engine::spec::ScalarValue::I32(i32::MAX),
                },
            ],
        };
        let singleton =
            numeric_planning_resources_with(&singleton_shape, &[estimate.clone()], &model, query)
                .expect("singleton and interval predicate costing")
                .expect("numeric resources");
        assert_eq!(singleton.bytes, result.bytes);
        assert_eq!(singleton.launches, result.launches);
        assert_eq!(
            singleton.aggregate.get(),
            (estimate.resident_rows.expect("exact rows") as f64 * 4.0).mul_add(
                model.coefficients.gpu_op_cost_filter.get(),
                result.aggregate.get()
            ),
            "two singleton equalities plus one inclusive interval cost four comparisons"
        );
        let mut grouped = shape.clone();
        grouped.descriptor_resolution = dense_sum_count_shape(100_000).descriptor_resolution;
        grouped.spec.group_keys = (2..=4)
            .map(|attno| GroupKeyRef {
                source: GroupKeySource::FactColumn(ColumnRef {
                    relation_oid: 42,
                    attno,
                    type_oid: u32::from(pg_sys::INT4OID),
                }),
                type_oid: u32::from(pg_sys::INT4OID),
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            })
            .collect();
        grouped.estimated_output_rows = 11;
        grouped.estimated_dense_key_cardinalities = Some(vec![10; 3]);
        let mut grouped_estimate = estimate.clone();
        grouped_estimate.resident_nullable_columns = Some(vec![2, 4]);
        let result = numeric_planning_resources_with(
            &grouped,
            &[grouped_estimate.clone()],
            &model,
            |_, _, nullable, groups| {
                assert_eq!(nullable, [2, 4]);
                assert_eq!(groups, 1210, "independent domains plus resident NULL codes");
                Err("native layout rejected".to_owned())
            },
        );
        assert!(
            matches!(result, Err(AdmissionDecline::NumericEstimateUnavailable { detail })
            if detail == "native layout rejected")
        );
        let mut tight_model = model.clone();
        tight_model.memory.gpu_hash_agg_max_groups = Rows::new(1209);
        assert!(
            numeric_planning_resources_with(
                &grouped,
                &[grouped_estimate.clone()],
                &tight_model,
                |_, _, _, _| panic!("capacity gate must use dense slots, not 11 output rows")
            )
            .is_err()
        );
        for cardinalities in [vec![10], vec![u64::MAX; 3]] {
            grouped.estimated_dense_key_cardinalities = Some(cardinalities);
            assert!(
                numeric_planning_resources_with(
                    &grouped,
                    &[grouped_estimate.clone()],
                    &model,
                    |_, _, _, _| panic!("invalid dense capacity must fail before native query")
                )
                .is_err()
            );
        }
        for missing in 0..4 {
            let mut incomplete = estimate.clone();
            match missing {
                0 => incomplete.loaded = false,
                1 => incomplete.resident_rows = None,
                2 => incomplete.resident_numeric_columns = None,
                _ => incomplete.resident_nullable_columns = None,
            }
            assert!(
                numeric_planning_resources_with(
                    &shape,
                    &[incomplete],
                    &model,
                    |_, _, _, _| panic!("missing metadata must fail before native query")
                )
                .is_err()
            );
        }
        shape.estimated_dense_key_cardinalities = None;
        assert!(
            numeric_planning_resources_with(&shape, &[estimate], &model, |_, _, _, _| panic!(
                "missing capacity must not use joint output rows"
            ))
            .is_err()
        );
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
    fn retained_execution_memory_participates_in_planner_budget_gate() {
        let mut shape = count_shape();
        let mut snapshot = budget_snapshot(0, 0, 0);
        snapshot.retained_execution_bytes = 4097;
        snapshot.current_backend_live_bytes = 4097;
        snapshot.cluster_live_bytes = 4097;
        let decline = apply_exact_residency(
            &mut shape,
            &[estimate(false, 4096)],
            AdmissionPolicy {
                auto_load: true,
                budget_bytes: 8192,
                budget_snapshot: snapshot,
            },
            &model(),
        )
        .expect_err("retained executor allocations cannot be reclaimed by the relation LRU");
        assert!(matches!(
            decline,
            AdmissionDecline::ResidencyBudgetExceeded {
                retained_execution_bytes: 4097,
                selected_raw_bytes: 4096,
                projected_final_bytes,
                prospective_execution_bytes,
                ..
            } if prospective_execution_bytes == count_native_execution_bytes()
                && projected_final_bytes == 8193 + prospective_execution_bytes
        ));
    }

    #[test]
    fn budget_selected_dense_calls_replace_old_one_shot_cost_without_double_charging_owners() {
        let mut shape = count_shape();
        let rows = 2051;
        shape.residency.relations[0].estimated_rows = rows;
        let mut estimate = estimate(true, 4096);
        estimate.resident_rows = Some(rows);
        let mut model = model();
        model.executor.gpu_reduce_max_chunk = Rows::new(2051);
        model.executor.gpu_grouped_agg_one_shot_max_rows = Rows::new(2051);
        let memory =
            dense_descriptor_memory_requirements(&shape.spec, &[], 1, rows as usize, 1024, 1024)
                .expect("one partial block bounded layout");
        let budget = 4096 + u64::try_from(memory.total_bytes).expect("layout bytes");
        apply_exact_residency(
            &mut shape,
            &[estimate],
            AdmissionPolicy {
                auto_load: false,
                budget_bytes: budget,
                budget_snapshot: budget_snapshot(0, 0, 0),
            },
            &model,
        )
        .expect("smaller chunk fits exact raw plus execution budget");
        assert_eq!(
            shape.cost.additional_aggregate_launches,
            PgCost::new(3.0 * cost::GPU_LAUNCH_OVERHEAD),
            "three accumulate calls and finalize, not previous one-shot cost"
        );
        assert_eq!(shape.residency.total_required_bytes, Some(4096));
    }

    #[test]
    fn scalar_execution_and_metadata_peak_participate_in_planner_budget_before_costing() {
        let mut shape = count_shape();
        shape.spec.measures[0].expression = MeasureExpr::Bytecode {
            inputs: vec![],
            program: vec![
                crate::engine::spec::scalar::SCALAR_PROGRAM_MAGIC,
                2,
                0,
                1,
                42,
            ],
            result_type_oid: u32::from(pg_sys::INT4OID),
        };
        let policy = AdmissionPolicy {
            auto_load: true,
            budget_bytes: 4096 + 20 + 240 + 8192 - 1,
            budget_snapshot: budget_snapshot(0, 0, 0),
        };
        let decline = apply_exact_residency_with_scalar_memory(
            &mut shape,
            &[estimate(true, 4096)],
            policy,
            &model(),
            |_, _, _, _| Ok((8192, None)),
        )
        .expect_err("native layout plus four scalar snapshots must fit too");
        assert!(matches!(
            decline,
            AdmissionDecline::ResidencyBudgetExceeded {
                prospective_execution_bytes: 8432,
                derived_artifact_bytes: 20,
                projected_final_bytes: 12548,
                ..
            }
        ));
        let MeasureExpr::Bytecode { program, .. } = &mut shape.spec.measures[0].expression else {
            unreachable!()
        };
        program[0] = 0;
        let decline = apply_exact_residency_with_scalar_memory(
            &mut shape,
            &[estimate(false, 4096)],
            AdmissionPolicy {
                auto_load: true,
                budget_bytes: u64::MAX,
                budget_snapshot: budget_snapshot(0, 0, 0),
            },
            &model(),
            |_, _, _, _| Ok((8192, None)),
        )
        .expect_err("opaque words cannot be budgeted as scalar programs");
        assert_eq!(decline.code(), "generic_scalar_estimate_unavailable");
    }

    #[test]
    fn dense_count_forecast_covers_loaded_and_conservative_cold_inputs() {
        let shape = count_shape();
        let model = model();
        for loaded in [false, true] {
            let evidence = estimate(loaded, 4096);
            let bytes = scalar_planning_memory_with(
                &shape,
                &[evidence],
                &model,
                |_, nullable, groups, rows, _, _| {
                    assert_eq!(groups, 1);
                    assert_eq!(rows, 100_000);
                    assert!(nullable.is_empty());
                    Ok(crate::gpu::GroupedAggMemoryRequirements {
                        workspace_bytes: 8000,
                        output_bytes: 192,
                        total_bytes: 8192,
                    })
                },
            )
            .expect("COUNT must quote workspace even without scalar bytecode");
            assert_eq!(bytes, 8192);
        }
        let mut integer = dense_sum_count_shape(123_456);
        integer.estimated_dense_key_cardinalities = Some(vec![16]);
        scalar_planning_memory_with(
            &integer,
            &[estimate(false, 4096)],
            &model,
            |_, nullable, groups, rows, _, _| {
                assert_eq!(nullable, &[1, 2, 3]);
                assert_eq!((groups, rows), (17, 123_456));
                Ok(crate::gpu::GroupedAggMemoryRequirements {
                    workspace_bytes: 8000,
                    output_bytes: 192,
                    total_bytes: 8192,
                })
            },
        )
        .expect("cold fixed-width forecast reserves potential NULL lanes");
    }

    #[test]
    fn dense_forecast_admission_exact_fit_and_one_byte_under() {
        let model = model();
        let snapshot = ResidentBudgetSnapshot {
            retained_execution_bytes: 400,
            current_backend_live_bytes: 1000,
            cluster_live_bytes: 1300,
            ..budget_snapshot(300, 200, 400)
        }
        .discount_idle_execution(150)
        .expect("local idle credit");
        let exact = snapshot.projected_final_bytes(4096, 0).expect("footprint") + 8192;
        for (budget, fits) in [(exact, true), (exact - 1, false)] {
            let result = apply_exact_residency_with_scalar_memory(
                &mut count_shape(),
                &[estimate(true, 4096)],
                AdmissionPolicy {
                    auto_load: false,
                    budget_bytes: budget,
                    budget_snapshot: snapshot,
                },
                &model,
                |_, _, _, _| Ok((8192, None)),
            );
            if fits {
                result.expect("exact raw + active + foreign + native layout fit");
            } else {
                assert!(
                    matches!(result, Err(AdmissionDecline::ResidencyBudgetExceeded {
                    prospective_execution_bytes: 8192, projected_final_bytes, ..
                }) if projected_final_bytes == exact)
                );
            }
        }
    }

    #[test]
    fn scalar_layout_planning_requires_exact_rows_nulls_and_dense_slots() {
        let mut shape = count_shape();
        shape.spec.measures[0].expression = MeasureExpr::Bytecode {
            inputs: vec![],
            program: vec![
                crate::engine::spec::scalar::SCALAR_PROGRAM_MAGIC,
                2,
                0,
                1,
                42,
            ],
            result_type_oid: u32::from(pg_sys::INT4OID),
        };
        let estimate = estimate(true, 4096);
        let model = model();
        let quote = scalar_planning_memory_with(
            &shape,
            std::slice::from_ref(&estimate),
            &model,
            |spec, nullable, groups, rows, chunk, one_shot| {
                assert_eq!(spec, &shape.spec);
                assert!(nullable.is_empty());
                assert_eq!((groups, rows), (1, 100_000));
                assert_eq!(chunk, model.executor.gpu_reduce_max_chunk.get());
                assert_eq!(
                    one_shot,
                    model.executor.gpu_grouped_agg_one_shot_max_rows.get()
                );
                Ok(crate::gpu::GroupedAggMemoryRequirements {
                    workspace_bytes: 8000,
                    output_bytes: 192,
                    total_bytes: 8192,
                })
            },
        )
        .expect("native layout quote");
        assert_eq!(quote, 8192);
        let mut spatial = shape.clone();
        let metadata = crate::engine::spec::SpatialValueMetadata {
            kind: crate::engine::spec::SpatialValueKind::Geometry,
            typmod: -1,
            srid: Some(4326),
        };
        // This test exercises resource accounting, not polygon validation;
        // descriptor tests separately qualify the exact geometry contract.
        spatial.spec.fact_filter = FilterSpec::Spatial {
            predicate: crate::engine::spec::SpatialPredicateKind::Intersects,
            left: crate::engine::spec::SpatialOperand::Column {
                column: crate::engine::spec::ColumnRef {
                    relation_oid: spatial.spec.fact_rel,
                    attno: 1,
                    type_oid: 9000,
                },
                metadata,
            },
            right: crate::engine::spec::SpatialOperand::Constant {
                metadata,
                bytes: Box::new([]),
            },
            distance: None,
        };
        let spatial_quote = scalar_planning_memory_with(
            &spatial,
            std::slice::from_ref(&estimate),
            &model,
            |_, _, _, _, _, _| {
                Ok(crate::gpu::GroupedAggMemoryRequirements {
                    workspace_bytes: 8000,
                    output_bytes: 192,
                    total_bytes: 8192,
                })
            },
        )
        .expect("spatial staged program overlap");
        assert_eq!(spatial_quote, quote + 20);
        let rows = u64::try_from(model.executor.gpu_grouped_agg_one_shot_max_rows.get())
            .expect("test rows")
            + 1;
        assert_eq!(
            planned_dense_lifecycle_calls(&spatial, rows, &model),
            planned_dense_lifecycle_calls(&shape, rows, &model),
            "spatial scalar costs the same bounded aggregation lifecycle"
        );
        assert!(planned_dense_lifecycle_calls(&spatial, rows, &model).is_some());
        for missing in 0..3 {
            let mut incomplete = estimate.clone();
            match missing {
                0 => incomplete.loaded = false,
                1 => incomplete.resident_rows = None,
                _ => incomplete.resident_nullable_columns = None,
            }
            assert!(
                scalar_planning_memory_with(
                    &shape,
                    &[incomplete],
                    &model,
                    |_, _, _, _, _, _| panic!(
                        "incomplete resident evidence must fail before layout query"
                    )
                )
                .is_err()
            );
        }
        shape.estimated_dense_key_cardinalities = None;
        assert!(
            scalar_planning_memory_with(&shape, &[estimate], &model, |_, _, _, _, _, _| panic!(
                "missing dense slots must fail before layout query"
            ))
            .is_err()
        );
        assert_eq!(
            scalar_planning_memory_with(
                &grouped_count_shape(),
                &[],
                &model,
                |_, _, _, _, _, _| panic!("hash shapes do not query dense layouts")
            )
            .expect("hash allocation is outside dense forecast"),
            0
        );
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
                budget_bytes: 4_096 + count_native_execution_bytes(),
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
                budget_bytes: 4_096 + count_native_execution_bytes(),
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
                budget_bytes: 4096 + count_native_execution_bytes(),
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
                budget_bytes: count_native_execution_bytes(),
                budget_snapshot: budget_snapshot(0, 0, 0),
            },
            &model(),
        )
        .expect("zero-column COUNT(*) has no raw allocation but needs native execution memory");

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
                budget_bytes: 4_096 + count_native_execution_bytes(),
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

        let mut weighted_count = count_shape();
        add_star_dimension(&mut weighted_count, JoinMultiplicity::Counted);
        weighted_count.residency.relations[0].estimated_rows =
            WEIGHTED_GLOBAL_COUNT_MIN_FACT_ROWS - 1;
        assert!(matches!(
            validate_shape_capability(&weighted_count)
                .expect_err("weighted COUNT(*) below its release floor must remain native"),
            AdmissionDecline::SerialGenericKernelMode {
                mode: "serial_generic"
            }
        ));
        weighted_count.residency.relations[0].estimated_rows = WEIGHTED_GLOBAL_COUNT_MIN_FACT_ROWS;
        validate_shape_capability(&weighted_count)
            .expect("unfiltered global COUNT(*) uses the weighted parallel dense-count branch");

        weighted_count.spec.fact_filter = FilterSpec::Mask {
            input: ColumnRef {
                relation_oid: 42,
                attno: 2,
                type_oid: u32::from(pg_sys::BOOLOID),
            },
            kind: crate::engine::spec::MaskKind::Sql,
        };
        assert!(matches!(
            validate_shape_capability(&weighted_count)
                .expect_err("filtered counted COUNT(*) must remain native"),
            AdmissionDecline::SerialGenericKernelMode { .. }
        ));

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
        validate_shape_capability(&range)
            .expect("one-sided ranges use the computed-integer parallel lane");
    }

    #[test]
    fn shape_path_contains_only_strict_contracts() {
        let (spec, projection) = count_contract();
        let words = encode_shape_path_private(&spec, &projection).expect("contract encodes");

        assert_eq!(words[0], AGG_QUERY_SPEC_SENTINEL);
        let spec_start = 1;
        let spec_len =
            crate::engine::spec::AggQuerySpec::encoded_i32_prefix_len(&words[spec_start..])
                .expect("AQS6 body has a strict length");
        let decoded_spec = crate::engine::spec::AggQuerySpec::decode_i32(
            &words[spec_start..spec_start + spec_len],
        )
        .expect("AQS6 body decodes");
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
                AdmissionDecline::SpatialResidencyRequired { exact_rows: None },
                "generic_spatial_residency_required",
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
                    retained_execution_bytes: 0,
                    evictable_or_replaced_local_bytes: 0,
                    selected_raw_bytes: 1,
                    derived_artifact_bytes: 1,
                    prospective_execution_bytes: 0,
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
