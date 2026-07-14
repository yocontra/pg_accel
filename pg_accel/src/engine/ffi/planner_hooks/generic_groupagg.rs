//! Generic descriptor aggregate admission and childless path construction.

use std::collections::BTreeMap;
use std::ffi::c_int;
use std::num::NonZeroU32;

use pgrx::pg_sys::{self, List, NodeTag, RelOptInfo};

use super::shape::{
    RelationResidency, RelationResidencyRequirement, ResidencyEstimate, ShapeCostGate,
    ShapeDecline, ShapePlan,
};
use super::{add_gpu_path_with_resident_proof, find_cheapest_path, rel_rows_estimate};
use crate::engine::cost::{self, PgCost, TypedCostModel};
use crate::engine::executor::agg::{
    estimate_descriptor_artifact_bytes_upper_bound, validate_descriptor_capability,
};
use crate::engine::ffi::custom_scan;
use crate::engine::gucs;
use crate::engine::residency::{
    MaterializationBoundary, ResidentBudgetSnapshot, ResidentLoadEstimate, ResidentOperatorClass,
    ResidentOperatorStage, ResidentPipelineProof, SelectedRelation, estimate_selected_relation,
    resident_budget_snapshot,
};
use crate::engine::spec::{AggOutputProjection, FilterSpec, MeasureExpr};
use crate::engine::stats;

const GENERIC_SHAPE_PATH_CONTEXT: &str = "upper_paths_generic_groupagg";
const AGG_QUERY_SPEC_SENTINEL: c_int = i32::from_be_bytes(*b"AQS3");
const AGG_OUTPUT_PROJECTION_SENTINEL: c_int = i32::from_be_bytes(*b"AOP2");

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
    AutoLoadDisabled {
        relation_oid: u32,
        estimated_bytes: u64,
    },
    ResidencyBytesOverflow,
    ResidencyBudgetSnapshotUnavailable,
    DerivedArtifactEstimateUnavailable,
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
            Self::AutoLoadDisabled { .. } => "generic_auto_load_disabled",
            Self::ResidencyBytesOverflow => "generic_residency_bytes_overflow",
            Self::ResidencyBudgetSnapshotUnavailable => {
                "generic_residency_budget_snapshot_unavailable"
            }
            Self::DerivedArtifactEstimateUnavailable => {
                "generic_derived_artifact_estimate_unavailable"
            }
            Self::ResidencyBudgetExceeded { .. } => "generic_residency_budget_exceeded",
            Self::DeviceCostGate(ShapeCostGate::Eligible) => "generic_invalid_eligible_cost_gate",
            Self::DeviceCostGate(ShapeCostGate::FactRowsBelowDeviceMinimum { .. }) => {
                "generic_fact_rows_below_device_minimum"
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

fn exact_residency_estimates(
    shape: &ShapePlan,
) -> Result<Vec<ResidentLoadEstimate>, AdmissionDecline> {
    exact_residency_estimates_with(shape, estimate_selected_relation)
}

fn exact_residency_estimates_with<E>(
    shape: &ShapePlan,
    mut estimate: impl FnMut(&SelectedRelation) -> Result<ResidentLoadEstimate, E>,
) -> Result<Vec<ResidentLoadEstimate>, AdmissionDecline>
where
    E: std::fmt::Display,
{
    super::with_planner_hooks_suspended(|| {
        shape
            .required_relations
            .iter()
            .map(|required| {
                let selected = selected_relation(required)?;
                estimate(&selected).map_err(|error| AdmissionDecline::ResidencyEstimateFailed {
                    relation_oid: required.relation_oid,
                    detail: error.to_string(),
                })
            })
            .collect()
    })
}

fn add_bytes(total: u64, bytes: u64) -> Result<u64, AdmissionDecline> {
    total
        .checked_add(bytes)
        .ok_or(AdmissionDecline::ResidencyBytesOverflow)
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
            missing_rows = missing_rows.saturating_add(prior.estimated_rows);
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
    let load_cost = missing_rows as f64 * model.coefficients.preagg_dim_materialize_cost.get()
        / f64::from(expected_reuses.get());
    let amortized_auto_load = PgCost::new(load_cost);
    shape.residency = ResidencyEstimate {
        relations,
        total_required_bytes: Some(total_required_bytes),
        missing_bytes: Some(missing_bytes),
        missing_rows,
        expected_reuses,
        amortized_load_cost: amortized_auto_load,
    };
    shape.cost.amortized_auto_load = amortized_auto_load;
    shape.cost.total = PgCost::new(
        shape.cost.fact_scan.get()
            + shape.cost.dimension_setup.get()
            + shape.cost.join_probe.get()
            + shape.cost.aggregate.get()
            + shape.cost.output_materialization.get()
            + amortized_auto_load.get(),
    );
    Ok(())
}

fn validate_shape_capability(shape: &ShapePlan) -> Result<(), AdmissionDecline> {
    validate_descriptor_capability(
        &shape.spec,
        &AggOutputProjection {
            slots: shape.projections.clone(),
        },
    )
    .map_err(|detail| AdmissionDecline::DescriptorCapability { detail })
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

    let cpath = unsafe {
        pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomPath>()).cast::<pg_sys::CustomPath>()
    };
    unsafe {
        (*cpath).path.type_ = NodeTag::T_CustomPath;
        (*cpath).path.pathtype = NodeTag::T_CustomScan;
        (*cpath).path.parent = output_rel;
        (*cpath).path.pathtarget = (*output_rel).reltarget;
        (*cpath).path.param_info = std::ptr::null_mut();
        (*cpath).path.parallel_aware = false;
        (*cpath).path.parallel_safe = false;
        (*cpath).path.parallel_workers = 0;
        (*cpath).path.rows = (*output_rel).rows.max(1.0);
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

    Ok(unsafe {
        add_gpu_path_with_resident_proof(
            GENERIC_SHAPE_PATH_CONTEXT,
            output_rel,
            cpath,
            shape_resident_proof(shape),
        )
    })
}

unsafe fn cheapest_native_cost(output_rel: *mut RelOptInfo) -> Option<f64> {
    if output_rel.is_null() {
        return None;
    }
    let path = unsafe { find_cheapest_path((*output_rel).pathlist) };
    (!path.is_null()).then(|| unsafe { (*path).total_cost })
}

fn record_decline(decline: &AdmissionDecline, output_rel: *mut RelOptInfo) {
    let rows = rel_rows_estimate(output_rel).unwrap_or(0);
    stats::increment_planner_rejected(decline.code(), rows);
    stats::record_planner_fast_decline(decline.code());
    pgrx::debug1!("pg_accel: generic aggregate declined: {decline}");
}

/// Analyze and admit one generic descriptor aggregate path.
///
/// # Safety
///
/// `root` and `output_rel` must be valid planner-owned pointers for the active
/// `UPPERREL_GROUP_AGG` callback.
pub(super) unsafe fn try_inject(
    root: *mut pg_sys::PlannerInfo,
    output_rel: *mut RelOptInfo,
) -> bool {
    if !gucs::gpu_enabled() {
        return false;
    }
    if let Err(decline) = unsafe { super::shape::preflight_base_relations(root) } {
        record_decline(&AdmissionDecline::Shape(decline), output_rel);
        return false;
    }
    if !cost::gpu_is_usable() {
        return false;
    }
    let model = TypedCostModel::from_limits(cost::device_limits());
    let result = (|| {
        let mut shape = unsafe { super::shape::extract_shape(root, output_rel, &model) }
            .map_err(AdmissionDecline::Shape)?;
        validate_shape_capability(&shape)?;
        let estimates = exact_residency_estimates(&shape)?;
        let selected_relids = estimates
            .iter()
            .map(|estimate| estimate.relid)
            .collect::<Vec<_>>();
        let budget_snapshot = resident_budget_snapshot(&selected_relids)
            .ok_or(AdmissionDecline::ResidencyBudgetSnapshotUnavailable)?;
        apply_exact_residency(
            &mut shape,
            &estimates,
            AdmissionPolicy {
                auto_load: gucs::auto_load(),
                budget_bytes: gucs::resident_memory_budget_bytes(),
                budget_snapshot,
            },
            &model,
        )?;
        let native_cost = unsafe { cheapest_native_cost(output_rel) };
        let effective_cost = effective_path_cost(&shape, gucs::cost_multiplier());
        gate_cost(
            &shape,
            native_cost,
            maximum_cost_ratio(&shape, &model),
            effective_cost,
        )?;
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
            record_decline(&decline, output_rel);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cost::{DeviceLimits, Rows};
    use crate::engine::ffi::planner_hooks::shape::{
        DescriptorMeasurePlan, DescriptorResolution, RequiredRelation, ShapeCost,
    };
    use crate::engine::spec::{
        AggOutputSlot, AggOutputSource, AggQuerySpec, AggregateKind, AggregateOutput,
        AggregateSource, ColumnRef, DimSpec, GroupKeyEncoding, GroupKeyRef, GroupKeySource,
        JoinMultiplicity, MeasureSpec,
    };

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

    fn estimate(loaded: bool, bytes: u64) -> ResidentLoadEstimate {
        ResidentLoadEstimate {
            relid: pg_sys::Oid::from(42u32),
            loaded,
            pinned: false,
            estimated_bytes: bytes,
            last_load_ms: None,
            amortization_queries: 4,
        }
    }

    fn model() -> TypedCostModel {
        let mut limits = DeviceLimits::cpu_only();
        limits.preagg_dim_materialize_cost = 0.004;
        TypedCostModel::from_limits(&limits)
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
        let estimates = exact_residency_estimates_with(&shape, |request| {
            assert!(super::super::planner_hooks_suspended());
            Ok::<_, std::convert::Infallible>(ResidentLoadEstimate {
                relid: request.relid,
                loaded: false,
                pinned: false,
                estimated_bytes: 4096,
                last_load_ms: None,
                amortization_queries: 4,
            })
        })
        .expect("mock estimate succeeds");
        assert_eq!(estimates, [estimate(false, 4096)]);
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
        assert_eq!(shape.cost.amortized_auto_load, PgCost::new(100.0));
        assert_eq!(shape.cost.total, PgCost::new(116.0));
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
        assert_eq!(shape.cost.amortized_auto_load, PgCost::new(100.0));
        assert_eq!(shape.cost.total, PgCost::new(116.0));
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
    fn planner_and_executor_share_counted_dimension_sum_capability() {
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
        validate_shape_capability(&unique_sum).expect("unique-dimension integer SUM is supported");

        for kind in [AggregateKind::Count, AggregateKind::Min, AggregateKind::Max] {
            let mut counted = integer_measure_shape(kind);
            add_star_dimension(&mut counted, JoinMultiplicity::Counted);
            validate_shape_capability(&counted)
                .unwrap_or_else(|error| panic!("counted-dimension {kind:?} declined: {error}"));
        }
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
}
