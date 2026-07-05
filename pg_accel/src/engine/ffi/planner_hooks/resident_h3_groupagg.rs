//! Planner recognizer for resident H3 grouped-count OLAP aggregates.

use std::ffi::CStr;

use pgrx::pg_sys::{self, List, NodeTag, PlannerInfo, RelOptInfo};

use super::rel_rows_estimate;
use super::resident_groupagg_path::{
    ResidentGroupAggPathShape, inject_childless_resident_groupagg_path,
};
use crate::engine::executor::agg::AggOp;
use crate::engine::executor::olap::{OlapAggSpec, ResidentH3GroupedCountSpec};
use crate::engine::olap_cache::{self, ResidentH3GroupedCountKind};
use crate::engine::residency::ResidentOperatorStage;
use crate::engine::stats;

const POINTOID_RAW: u32 = 600;
const H3_GROUPED_COUNT_MIN_INPUT_ROWS: u64 = 25_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecognizedH3GroupAgg {
    rel_oid: pg_sys::Oid,
    kind: ResidentH3GroupedCountKind,
    resolution: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct H3FuncGroup {
    varno: pg_sys::Index,
    attno: i32,
    rel_oid: pg_sys::Oid,
    kind: ResidentH3GroupedCountKind,
    resolution: i32,
}

/// Try to inject a childless resident H3 grouped COUNT(*) aggregate.
///
/// # Safety
///
/// Planner pointers must be valid for the current `UPPERREL_GROUP_AGG` hook.
pub(super) unsafe fn try_inject(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
) -> bool {
    let Some(spec) = (unsafe { recognize(root) }) else {
        return false;
    };
    let rows_est = rel_rows_estimate(input_rel).unwrap_or(0);

    if rows_est < H3_GROUPED_COUNT_MIN_INPUT_ROWS {
        stats::increment_planner_rejected("h3_rows_below_grouped_agg_min", rows_est);
        stats::record_planner_fast_decline("upper_paths_resident_h3_groupagg_rows_below_min");
        pgrx::debug1!(
            "pg_accel: upper_paths resident H3 groupagg decline: rows_est={} below \
             grouped-count resident floor {}",
            rows_est,
            H3_GROUPED_COUNT_MIN_INPUT_ROWS,
        );
        return true;
    }

    if !olap_cache::resident_h3_groupagg_cache_loaded_for(spec.rel_oid, spec.kind, spec.resolution)
    {
        stats::increment_planner_rejected("recognized_resident_h3_groupagg_no_cache", rows_est);
        stats::record_planner_fast_decline("upper_paths_resident_h3_groupagg_no_cache");
        pgrx::debug1!(
            "pg_accel: upper_paths resident H3 groupagg decline: rel_oid={} kind={:?} \
             but backend-local resident cache is not loaded",
            u32::from(spec.rel_oid),
            spec.kind,
        );
        return true;
    }

    if output_rel.is_null() {
        return true;
    }

    let olap_spec = OlapAggSpec::ResidentH3GroupedCount(ResidentH3GroupedCountSpec {
        rel_oid: spec.rel_oid,
        kind: spec.kind,
        resolution: spec.resolution,
    });

    let device_columns = match spec.kind {
        ResidentH3GroupedCountKind::LatLngToCell => 4,
        ResidentH3GroupedCountKind::CellToParent => 1,
    };
    unsafe {
        inject_childless_resident_groupagg_path(
            output_rel,
            rows_est,
            ResidentGroupAggPathShape {
                context: "upper_paths_resident_h3_groupagg",
                olap_spec,
                agg_op: AggOp::Count,
                result_oid: pg_sys::INT8OID,
                output_rows: (*output_rel).rows.max(1.0),
                cost_per_row: 0.00004,
                stages: vec![
                    ResidentOperatorStage::Scan,
                    ResidentOperatorStage::H3,
                    ResidentOperatorStage::GroupedAggregate,
                    ResidentOperatorStage::FinalMaterialization,
                ],
                device_columns,
                has_filter: false,
            },
        );
    }
    true
}

unsafe fn recognize(root: *mut PlannerInfo) -> Option<RecognizedH3GroupAgg> {
    if root.is_null() || unsafe { (*root).parse }.is_null() {
        return None;
    }
    let query = unsafe { &*(*root).parse };
    if !query.hasAggs
        || query.groupClause.is_null()
        || unsafe { pg_sys::list_length(query.groupClause) } != 1
        || !unsafe { query_has_no_quals(query) }
    {
        return None;
    }

    let group = unsafe { single_h3_group_expr(query)? };
    if group.attno <= 0 {
        return None;
    }
    if !unsafe { target_is_group_count(query.targetList, query.rtable, group)? } {
        return None;
    }
    Some(RecognizedH3GroupAgg {
        rel_oid: group.rel_oid,
        kind: group.kind,
        resolution: group.resolution,
    })
}

unsafe fn single_h3_group_expr(query: &pg_sys::Query) -> Option<H3FuncGroup> {
    let sc = unsafe { pg_sys::list_nth(query.groupClause, 0).cast::<pg_sys::SortGroupClause>() };
    if sc.is_null() {
        return None;
    }
    let sgref = unsafe { (*sc).tleSortGroupRef };
    let tlist = query.targetList;
    if tlist.is_null() {
        return None;
    }
    let len = unsafe { pg_sys::list_length(tlist) };
    for i in 0..len {
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() || unsafe { (*tle).ressortgroupref } != sgref {
            continue;
        }
        return unsafe { h3_group_from_expr((*tle).expr.cast::<pg_sys::Node>(), query.rtable) };
    }
    None
}

unsafe fn h3_group_from_expr(
    node: *mut pg_sys::Node,
    rtable: *mut pg_sys::List,
) -> Option<H3FuncGroup> {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_FuncExpr {
        return None;
    }
    let func = node.cast::<pg_sys::FuncExpr>();
    let name = unsafe { function_name((*func).funcid)? };
    if name != "h3_latlng_to_cell" && name != "h3_cell_to_parent" {
        return None;
    }
    let args = unsafe { (*func).args };
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
        return None;
    }
    let arg0 = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
    let arg1 = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
    if arg0.is_null()
        || arg1.is_null()
        || unsafe { (*arg0).type_ } != NodeTag::T_Var
        || unsafe { (*arg1).type_ } != NodeTag::T_Const
    {
        return None;
    }
    let var = arg0.cast::<pg_sys::Var>();
    let resolution = unsafe { const_i32(arg1)? };
    if !(0..=15).contains(&resolution) {
        return None;
    }
    let kind = if name == "h3_latlng_to_cell" {
        if u32::from(unsafe { (*var).vartype }) != POINTOID_RAW {
            return None;
        }
        ResidentH3GroupedCountKind::LatLngToCell
    } else {
        if unsafe { (*var).vartype } != unsafe { (*func).funcresulttype } {
            return None;
        }
        ResidentH3GroupedCountKind::CellToParent
    };
    let varno = pg_sys::Index::try_from(unsafe { (*var).varno }).ok()?;
    let rel_oid = unsafe { relation_oid_from_rtable(rtable, varno)? };
    Some(H3FuncGroup {
        varno,
        attno: i32::from(unsafe { (*var).varattno }),
        rel_oid,
        kind,
        resolution,
    })
}

unsafe fn target_is_group_count(
    tlist: *mut List,
    rtable: *mut pg_sys::List,
    group: H3FuncGroup,
) -> Option<bool> {
    if tlist.is_null() || unsafe { pg_sys::list_length(tlist) } != 2 {
        return Some(false);
    }
    let first = unsafe { pg_sys::list_nth(tlist, 0).cast::<pg_sys::TargetEntry>() };
    let second = unsafe { pg_sys::list_nth(tlist, 1).cast::<pg_sys::TargetEntry>() };
    if first.is_null()
        || second.is_null()
        || unsafe { (*first).resjunk }
        || unsafe { (*second).resjunk }
    {
        return Some(false);
    }
    let same_group = unsafe {
        h3_group_from_expr((*first).expr.cast::<pg_sys::Node>(), rtable).is_some_and(|candidate| {
            candidate.varno == group.varno
                && candidate.attno == group.attno
                && candidate.rel_oid == group.rel_oid
                && candidate.kind == group.kind
                && candidate.resolution == group.resolution
        })
    };
    Some(same_group && unsafe { aggref_is_count_star((*second).expr.cast::<pg_sys::Node>()) })
}

unsafe fn aggref_is_count_star(node: *mut pg_sys::Node) -> bool {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Aggref {
        return false;
    }
    let agg = node.cast::<pg_sys::Aggref>();
    if !unsafe { (*agg).aggstar }
        || !unsafe { (*agg).aggdistinct }.is_null()
        || !unsafe { (*agg).aggorder }.is_null()
        || !unsafe { (*agg).aggfilter }.is_null()
    {
        return false;
    }
    let name_ptr = unsafe { pg_sys::get_func_name((*agg).aggfnoid) };
    if name_ptr.is_null() {
        return false;
    }
    unsafe { CStr::from_ptr(name_ptr).to_bytes() == b"count" }
}

unsafe fn const_i32(node: *mut pg_sys::Node) -> Option<i32> {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Const {
        return None;
    }
    let cst = node.cast::<pg_sys::Const>();
    if unsafe { (*cst).constisnull } {
        return None;
    }
    Some(unsafe { (*cst).constvalue.value() as i32 })
}

unsafe fn query_has_no_quals(query: &pg_sys::Query) -> bool {
    query.jointree.is_null() || unsafe { (*query.jointree).quals }.is_null()
}

unsafe fn function_name(funcid: pg_sys::Oid) -> Option<String> {
    let ptr = unsafe { pg_sys::get_func_name(funcid) };
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .ok()
        .map(ToOwned::to_owned)
}

unsafe fn relation_oid_from_rtable(
    rtable: *mut pg_sys::List,
    varno: pg_sys::Index,
) -> Option<pg_sys::Oid> {
    if rtable.is_null() || varno == 0 {
        return None;
    }
    let index = i32::try_from(varno).ok()?.checked_sub(1)?;
    if index >= unsafe { pg_sys::list_length(rtable) } {
        return None;
    }
    let rte = unsafe { pg_sys::list_nth(rtable, index).cast::<pg_sys::RangeTblEntry>() };
    if rte.is_null()
        || unsafe { (*rte).rtekind } != pg_sys::RTEKind::RTE_RELATION
        || unsafe { (*rte).relid } == pg_sys::InvalidOid
    {
        return None;
    }
    Some(unsafe { (*rte).relid })
}
