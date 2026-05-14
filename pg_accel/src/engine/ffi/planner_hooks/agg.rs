//! Non-parallel aggregate path injection.
//!
//! Thin wrapper over the shared `pgaccel_inject_gpu_agg` in `mod.rs`. Builds
//! a `CustomPath` with `parallel_safe = child_path.parallel_safe` (never forced
//! to true) and calls `add_path`.
//!
//! The partial/parallel path lives in `partial_agg.rs`.

use pgrx::pg_sys::{self, NodeTag};

use super::agg_common;

/// Inject the non-parallel GpuAgg `CustomPath` into `output_rel`.
///
/// Matches the current contract: `CustomPrivateData { is_partial: false, ... }`.
///
/// # Safety
///
/// Called from the planner hook on the main backend thread. All pointer args
/// must be valid planner-provided arguments.
pub(super) unsafe fn inject(
    root: *mut pg_sys::PlannerInfo,
    input_rel: *mut pg_sys::RelOptInfo,
    output_rel: *mut pg_sys::RelOptInfo,
) {
    // SAFETY: root is a planner-provided pointer for this hook invocation.
    if unsafe { grouped_query_has_avg(root) } && !agg_common::grouped_avg_finalize_supported() {
        pgrx::debug1!(
            "pg_accel: gpu_agg rejected: grouped AVG is hard-gated until hash-agg emits true averages"
        );
        return;
    }

    // SAFETY: delegates to the shared implementation with is_partial=false.
    unsafe {
        super::pgaccel_inject_gpu_agg(root, input_rel, output_rel, false);
    }
}

/// True when the current grouped query contains a built-in AVG aggregate.
///
/// The grouped finalize-mode executor has only one result lane per aggregate,
/// and `AggOp::Avg` currently maps to the SUM lane. This front-door policy
/// gate declines the pg_accel path before the shared planner body can build a
/// `GpuAccelAgg` candidate that would emit a raw sum.
unsafe fn grouped_query_has_avg(root: *mut pg_sys::PlannerInfo) -> bool {
    if root.is_null() {
        return false;
    }
    // SAFETY: root is a planner-provided pointer.
    let parse = unsafe { (*root).parse };
    if parse.is_null() {
        return false;
    }
    // SAFETY: parse is a valid Query.
    let query = unsafe { &*parse };
    let group_len = if query.groupClause.is_null() {
        0
    } else {
        // SAFETY: groupClause is a valid planner-owned List.
        unsafe { pg_sys::list_length(query.groupClause) }
    };
    if group_len == 0 {
        return false;
    }

    // SAFETY: targetList is a planner-owned expression tree.
    let target_has_avg = unsafe { target_list_has_avg(query.targetList) };
    // SAFETY: havingQual is a planner-owned expression tree.
    let having_has_avg = unsafe { node_has_avg_aggref(query.havingQual.cast::<pg_sys::Node>()) };
    target_has_avg || having_has_avg
}

unsafe fn target_list_has_avg(tlist: *mut pg_sys::List) -> bool {
    if tlist.is_null() {
        return false;
    }
    // SAFETY: tlist is a planner-owned List.
    let len = unsafe { pg_sys::list_length(tlist) };
    for i in 0..len {
        // SAFETY: i is in [0, len).
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() {
            continue;
        }
        // SAFETY: tle is a valid TargetEntry.
        let expr = unsafe { (*tle).expr };
        // SAFETY: expr belongs to a planner-owned TargetEntry.
        if unsafe { node_has_avg_aggref(expr.cast::<pg_sys::Node>()) } {
            return true;
        }
    }
    false
}

unsafe fn node_has_avg_aggref(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }
    // SAFETY: node is a valid planner expression node.
    let tag = unsafe { (*node).type_ };
    #[allow(clippy::cast_ptr_alignment)]
    match tag {
        NodeTag::T_Aggref => {
            let aggref = node.cast::<pg_sys::Aggref>();
            // SAFETY: tag confirms this is an Aggref.
            unsafe { agg_common::aggref_is_avg(aggref) }
        }
        NodeTag::T_OpExpr => {
            // SAFETY: tag confirms this is an OpExpr.
            let args = unsafe { (*node.cast::<pg_sys::OpExpr>()).args };
            // SAFETY: args belongs to the tag-checked OpExpr.
            unsafe { list_has_avg_aggref(args) }
        }
        NodeTag::T_BoolExpr => {
            // SAFETY: tag confirms this is a BoolExpr.
            let args = unsafe { (*node.cast::<pg_sys::BoolExpr>()).args };
            // SAFETY: args belongs to the tag-checked BoolExpr.
            unsafe { list_has_avg_aggref(args) }
        }
        NodeTag::T_FuncExpr => {
            // SAFETY: tag confirms this is a FuncExpr.
            let args = unsafe { (*node.cast::<pg_sys::FuncExpr>()).args };
            // SAFETY: args belongs to the tag-checked FuncExpr.
            unsafe { list_has_avg_aggref(args) }
        }
        NodeTag::T_RelabelType => {
            // SAFETY: tag confirms this is a RelabelType.
            let arg = unsafe { (*node.cast::<pg_sys::RelabelType>()).arg };
            // SAFETY: arg belongs to the tag-checked RelabelType.
            unsafe { node_has_avg_aggref(arg.cast::<pg_sys::Node>()) }
        }
        NodeTag::T_CoerceViaIO => {
            // SAFETY: tag confirms this is a CoerceViaIO.
            let arg = unsafe { (*node.cast::<pg_sys::CoerceViaIO>()).arg };
            // SAFETY: arg belongs to the tag-checked CoerceViaIO.
            unsafe { node_has_avg_aggref(arg.cast::<pg_sys::Node>()) }
        }
        NodeTag::T_NullTest => {
            // SAFETY: tag confirms this is a NullTest.
            let arg = unsafe { (*node.cast::<pg_sys::NullTest>()).arg };
            // SAFETY: arg belongs to the tag-checked NullTest.
            unsafe { node_has_avg_aggref(arg.cast::<pg_sys::Node>()) }
        }
        _ => false,
    }
}

unsafe fn list_has_avg_aggref(list: *mut pg_sys::List) -> bool {
    if list.is_null() {
        return false;
    }
    // SAFETY: list is a planner-owned List.
    let len = unsafe { pg_sys::list_length(list) };
    for i in 0..len {
        // SAFETY: i is in [0, len).
        let child = unsafe { pg_sys::list_nth(list, i).cast::<pg_sys::Node>() };
        // SAFETY: child is an element of a planner-owned List.
        if unsafe { node_has_avg_aggref(child) } {
            return true;
        }
    }
    false
}
