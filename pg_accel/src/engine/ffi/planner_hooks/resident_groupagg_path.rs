//! Shared childless `ResidentGroupAgg` CustomPath construction.

use std::ffi::c_int;

use pgrx::pg_sys::{self, List, NodeTag, RelOptInfo};

use super::shape::{ShapeCostGate, ShapeDecline, ShapePlan};
use super::{add_gpu_path_with_resident_proof, record_no_gpu_resident_pipeline_decline};
use crate::engine::cost;
use crate::engine::executor::agg::AggOp;
use crate::engine::executor::olap::OlapAggSpec;
use crate::engine::ffi::custom_scan;
use crate::engine::residency::{
    MaterializationBoundary, ResidentOperatorClass, ResidentOperatorStage, ResidentPipelineProof,
};
use crate::engine::spec::{AggOutputProjection, FilterSpec, MeasureExpr};

const DARK_SHAPE_PATH_CONTEXT: &str = "upper_paths_shape_groupagg_dark";
const SHAPE_PATH_LEGACY_PREFIX: [c_int; 4] = [0, 0, 0, 0];
const AGG_QUERY_SPEC_SENTINEL: c_int = i32::from_be_bytes(*b"AQS2");
const AGG_OUTPUT_PROJECTION_SENTINEL: c_int = i32::from_be_bytes(*b"AOP2");

pub(super) struct ResidentGroupAggPathShape {
    pub context: &'static str,
    pub olap_spec: OlapAggSpec,
    pub agg_op: AggOp,
    pub result_oid: pg_sys::Oid,
    pub output_rows: f64,
    pub cost_per_row: f64,
    pub stages: Vec<ResidentOperatorStage>,
    pub device_columns: usize,
    pub has_filter: bool,
}

/// Inject a childless resident grouped aggregate path.
///
/// Dense groupagg, H3 grouped count, and SSBM star-schema groupagg have
/// different recognizers and executors, but the planner-visible CustomPath,
/// private-data prefix, resident proof, and final materialization boundary are
/// the same contract.
///
/// # Safety
///
/// `output_rel` must be a valid upper relation for the current planner hook.
pub(super) unsafe fn inject_childless_resident_groupagg_path(
    output_rel: *mut RelOptInfo,
    rows_est: u64,
    shape: ResidentGroupAggPathShape,
) {
    if output_rel.is_null() {
        return;
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
        (*cpath).path.rows = shape.output_rows.max(1.0);
        (*cpath).path.startup_cost = cost::GPU_LAUNCH_OVERHEAD;
        (*cpath).path.total_cost = (rows_est as f64)
            .mul_add(shape.cost_per_row, cost::GPU_LAUNCH_OVERHEAD)
            .max(1.0);
        (*cpath).path.pathkeys = std::ptr::null_mut();
        (*cpath).flags = 0;
        (*cpath).custom_paths = std::ptr::null_mut();
        (*cpath).custom_restrictinfo = std::ptr::null_mut();
        (*cpath).methods = custom_scan::agg_path_methods();

        let mut priv_list: *mut List = std::ptr::null_mut();
        priv_list = pg_sys::lappend(priv_list, pg_sys::makeInteger(1).cast());
        priv_list = pg_sys::lappend(priv_list, pg_sys::makeInteger(shape.agg_op.to_i32()).cast());
        priv_list = pg_sys::lappend(priv_list, pg_sys::makeInteger(0).cast());
        priv_list = pg_sys::lappend(
            priv_list,
            pg_sys::makeInteger(u32::from(shape.result_oid) as i32).cast(),
        );
        priv_list = pg_sys::lappend(priv_list, pg_sys::makeInteger(0).cast());
        priv_list = pg_sys::lappend(priv_list, pg_sys::makeInteger(0).cast());
        priv_list = pg_sys::lappend(priv_list, pg_sys::makeInteger(0).cast());
        priv_list = custom_scan::append_olap_agg_spec(priv_list, &shape.olap_spec);
        (*cpath).custom_private = priv_list;
    }

    let proof = ResidentPipelineProof::device_resident(
        ResidentOperatorClass::ResidentGroupAgg,
        shape.stages,
        MaterializationBoundary::FinalOutput,
        shape.device_columns,
        shape.has_filter,
        true,
    )
    .snapshot();

    let added =
        unsafe { add_gpu_path_with_resident_proof(shape.context, output_rel, cpath, proof) };
    if !added {
        record_no_gpu_resident_pipeline_decline(shape.context, output_rel);
    }
}

/// Encode the path-private payload consumed by the aggregate `PlanCustomPath`
/// callback.
///
/// The four leading zeroes are the canonical empty legacy aggregate payload:
/// `[num_aggs, has_group_key, self_scan_relid, is_partial]`. The strict AQS2
/// and AOP2 blocks follow it. A resident-proof trailer is deliberately not
/// included here; [`add_gpu_path_with_resident_proof`] appends that exactly
/// once when the path is admitted.
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
    let mut words = Vec::with_capacity(
        SHAPE_PATH_LEGACY_PREFIX.len() + spec_words.len() + projection_words.len() + 2,
    );
    words.extend(SHAPE_PATH_LEGACY_PREFIX);
    words.push(AGG_QUERY_SPEC_SENTINEL);
    words.extend(spec_words);
    words.push(AGG_OUTPUT_PROJECTION_SENTINEL);
    words.extend(projection_words);
    Ok(words)
}

fn shape_stages(shape: &ShapePlan) -> (Vec<ResidentOperatorStage>, bool) {
    let has_filter = shape.spec.fact_filter != FilterSpec::None
        || shape
            .spec
            .measures
            .iter()
            .any(|measure| measure.filter != FilterSpec::None)
        || shape
            .spec
            .star_dims
            .iter()
            .any(|dimension| dimension.filter != FilterSpec::None);
    let has_expression = has_filter
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
    // COUNT(*) can derive its row count from relation residency without a
    // value lane. Its aggregate output state is still a device-owned buffer.
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

/// Build and add the neutral descriptor-driven aggregate path.
///
/// This is intentionally a dark Phase 5D entry point: no live planner hook
/// calls it until the descriptor executor and Begin-time artifact resolution
/// are complete. The path is childless and carries only the strict AQS2/AOP2
/// contract plus its resident-pipeline proof.
///
/// # Safety
///
/// `output_rel` must be a valid upper relation in the active PostgreSQL
/// planner memory context.
#[allow(dead_code)] // Dark until the Phase 5 cut-over replaces the legacy recognizer chain.
pub(super) unsafe fn inject_childless_shape_path(
    output_rel: *mut RelOptInfo,
    shape: &ShapePlan,
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
            "shape digest words do not match the serialized AQS2/AOP2 contract".to_owned(),
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
        (*cpath).path.startup_cost = cost::GPU_LAUNCH_OVERHEAD;
        (*cpath).path.total_cost = (cost::GPU_LAUNCH_OVERHEAD + shape.cost.total.get()).max(1.0);
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

    let proof = shape_resident_proof(shape);
    Ok(unsafe {
        add_gpu_path_with_resident_proof(DARK_SHAPE_PATH_CONTEXT, output_rel, cpath, proof)
    })
}

#[cfg(test)]
mod dark_shape_path_tests {
    use std::num::NonZeroU32;

    use super::*;
    use crate::engine::cost::PgCost;
    use crate::engine::ffi::planner_hooks::shape::{
        DescriptorMeasurePlan, DescriptorResolution, RequiredRelation, ResidencyEstimate, ShapeCost,
    };
    use crate::engine::spec::{
        AggOutputSlot, AggOutputSource, AggQuerySpec, AggregateKind, AggregateOutput,
        AggregateSource, MeasureSpec,
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
            fact_scan: PgCost::new(0.0),
            dimension_setup: PgCost::new(0.0),
            join_probe: PgCost::new(0.0),
            aggregate: PgCost::new(0.0),
            output_materialization: PgCost::new(0.0),
            amortized_auto_load: PgCost::new(0.0),
            total: PgCost::new(0.0),
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
            },
            residency: ResidencyEstimate {
                relations: Vec::new(),
                total_required_bytes: Some(0),
                missing_bytes: Some(0),
                missing_rows: 0,
                expected_reuses: NonZeroU32::MIN,
                amortized_load_cost: PgCost::new(0.0),
            },
            cost: zero_cost,
            cost_gate: ShapeCostGate::Eligible,
        }
    }

    #[test]
    fn dark_shape_path_uses_empty_legacy_prefix_then_strict_contracts() {
        let (spec, projection) = count_contract();
        let words = encode_shape_path_private(&spec, &projection).expect("contract encodes");

        assert_eq!(&words[..4], &SHAPE_PATH_LEGACY_PREFIX);
        assert_eq!(words[4], AGG_QUERY_SPEC_SENTINEL);
        let spec_start = 5;
        let spec_len =
            crate::engine::spec::AggQuerySpec::encoded_i32_prefix_len(&words[spec_start..])
                .expect("AQS2 body has a strict length");
        let decoded_spec = crate::engine::spec::AggQuerySpec::decode_i32(
            &words[spec_start..spec_start + spec_len],
        )
        .expect("AQS2 body decodes");
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
    fn outer_contract_sentinels_are_stable_ascii_words() {
        assert_eq!(AGG_QUERY_SPEC_SENTINEL, 0x4151_5332);
        assert_eq!(AGG_OUTPUT_PROJECTION_SENTINEL, 0x414f_5032);
    }

    #[test]
    fn count_star_proof_accounts_for_device_output_without_input_lanes() {
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
}
