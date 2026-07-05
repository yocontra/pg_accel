//! Shared childless `ResidentGroupAgg` CustomPath construction.

use pgrx::pg_sys::{self, List, NodeTag, RelOptInfo};

use super::{add_gpu_path_with_resident_proof, record_no_gpu_resident_pipeline_decline};
use crate::engine::cost;
use crate::engine::executor::agg::AggOp;
use crate::engine::executor::olap::OlapAggSpec;
use crate::engine::ffi::custom_scan;
use crate::engine::residency::{
    MaterializationBoundary, ResidentOperatorClass, ResidentOperatorStage, ResidentPipelineProof,
};

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
