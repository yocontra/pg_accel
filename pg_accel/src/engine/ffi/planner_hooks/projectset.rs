//! `set_rel_pathlist_hook` injector for `RTE_FUNCTION` rels (Phase 2 F3).
//!
//! Catches the planner's `set_rel_pathlist_hook` callback when the relation
//! is a function-source rel (`rte->rtekind == RTE_FUNCTION`). Walks the
//! `RangeTblEntry.functions` list, extracts the first `funcexpr`, and if its
//! `funcid` is registered in `crate::engine::registry` with a non-Scalar
//! [`OutputShape`], builds a `CustomPath` carrying a
//! [`FunctionScanPrivData`] payload that the executor's `begin_custom_scan`
//! arm consumes.
//!
//! ## Scope
//!
//! **FunctionScan-only.** This hook handles `SELECT * FROM srf(...)` style
//! call sites where PG's parser builds an `RTE_FUNCTION` relation (the
//! "FunctionScan" plan node). It does **not** handle SRF-in-target-list
//! invocations like `SELECT srf(col) FROM t` — those go through the
//! `ProjectSet` plan node and a separate `create_upper_paths_hook` arm.
//! See TODO entry **"SRF-in-target-list / ProjectSet planner injection"**
//! for the follow-up; the `projectset` filename is forward-looking.
//!
//! ## Args extraction
//!
//! All `funcexpr` args must be `T_Const` (constant-folded by the planner) or
//! the hook bails out — per-row `Var` arguments cannot be served from a
//! pure FunctionScan rel (they would imply a lateral join). The captured
//! constants are serialized into [`FunctionScanPrivData::args`] as
//! `(datum_as_i64, type_oid_as_u32)` pairs preserving positional order.
//!
//! ## Cost
//!
//! The CustomPath cost mirrors PG's own `cost_functionscan` shape: a small
//! constant startup + per-row tuple cost equal to the SRF's declared
//! `prorows`. `add_path` decides whether our path wins; on a GPU-equipped
//! machine the dispatch path is dramatically cheaper than per-row
//! `FunctionCallInvoke` for the var-output H3 ops.

use std::ffi::c_int;

use pgrx::pg_sys::{self, CustomPath, NodeTag, Path, PlannerInfo, RangeTblEntry, RelOptInfo};

use super::super::custom_scan::{
    self, FunctionScanPrivData, OutputShapeDisc, append_functionscan_priv, function_path_methods,
};
use crate::engine::cost;
use crate::engine::gucs;
use crate::engine::registry::{self, OutputShape};
use crate::engine::stats;

/// `set_rel_pathlist_hook` arm for `RTE_FUNCTION` relations.
///
/// Chained from [`super::rel_pathlist::pgaccel_set_rel_pathlist`] for
/// non-base-relation rtekinds; the chain order is hook-prev → base-rel
/// injection → this fn.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
pub(super) unsafe fn try_inject_function_scan(
    _root: *mut PlannerInfo,
    rel: *mut RelOptInfo,
    _rti: pg_sys::Index,
    rte: *mut RangeTblEntry,
) {
    // Gate: GUC + GPU availability.
    if !gucs::enabled() {
        pgrx::debug1!("pg_accel: projectset: extension disabled, skipping");
        return;
    }
    if !cost::gpu_is_usable() {
        pgrx::debug1!("pg_accel: projectset: gpu_is_usable=false, skipping");
        return;
    }
    if rel.is_null() || rte.is_null() {
        return;
    }

    // SAFETY: rel + rte are valid planner pointers.
    let rte_ref = unsafe { &*rte };
    if rte_ref.rtekind != pg_sys::RTEKind::RTE_FUNCTION {
        return;
    }
    pgrx::debug1!("pg_accel: projectset: RTE_FUNCTION rel detected");

    // SAFETY: rte_ref.functions is a List of RangeTblFunction nodes (or NIL).
    let functions = rte_ref.functions;
    if functions.is_null() {
        return;
    }
    // Multiple functions in `ROWS FROM (a(), b())` is a multi-set form we
    // don't support — only inject for single-function FunctionScans.
    let n_funcs = unsafe { pg_sys::list_length(functions) };
    if n_funcs != 1 {
        return;
    }

    // SAFETY: idx 0 is in [0, n_funcs).
    let rtfunc_node = unsafe { pg_sys::list_nth(functions, 0) };
    if rtfunc_node.is_null() {
        return;
    }
    // SAFETY: list_nth returned non-null; tag confirms RangeTblFunction.
    let rtfunc = rtfunc_node.cast::<pg_sys::RangeTblFunction>();
    let funcexpr_node = unsafe { (*rtfunc).funcexpr };
    if funcexpr_node.is_null() {
        return;
    }

    // SAFETY: funcexpr_node is a valid Node pointer; reading its tag.
    let tag = unsafe { (*funcexpr_node).type_ };
    if tag != pg_sys::NodeTag::T_FuncExpr {
        // Could be an OpExpr or unresolved expression; we only support a
        // direct FuncExpr at the head of the FunctionScan.
        return;
    }
    // SAFETY: tag confirmed T_FuncExpr.
    let funcexpr = funcexpr_node.cast::<pg_sys::FuncExpr>();
    let fn_oid = unsafe { (*funcexpr).funcid };

    // Look up the function in the adapter registry.
    registry::lazy_init();
    let Some(entry) = registry::global_registry().lookup(fn_oid) else {
        pgrx::debug1!(
            "pg_accel: projectset: fn_oid={} not in registry; skipping",
            u32::from(fn_oid)
        );
        return;
    };
    pgrx::debug1!(
        "pg_accel: projectset: matched registered fn_oid={} name={} shape={:?}",
        u32::from(fn_oid),
        entry.name,
        entry.output_shape,
    );

    // Only fire for non-Scalar shapes — Scalar SRFs would emit one row per
    // input row, but a FunctionScan rel has no input row, so a Scalar shape
    // would degrade to "1 input → 1 output" which is identical to PG's own
    // FunctionScan behaviour and offers no acceleration win.
    let (shape_disc, shape_field_count) = match entry.output_shape {
        OutputShape::Scalar => return,
        OutputShape::Record { field_count } => (OutputShapeDisc::Record, field_count),
        OutputShape::VarLen => (OutputShapeDisc::VarLen, 0),
    };

    // Sanity-check that the registry carries the field metadata we need to
    // build a TupleDesc downstream. Without it the executor cannot build a
    // scan slot — bail honestly rather than mid-execution.
    if entry.output_field_types.is_empty() || entry.output_field_names.is_empty() {
        pgrx::debug1!(
            "pg_accel: projectset hook: registry entry for fn_oid={} missing \
             output_field_types/names; declining to inject CustomPath",
            u32::from(fn_oid),
        );
        return;
    }
    if entry.output_field_types.len() != entry.output_field_names.len() {
        pgrx::debug1!(
            "pg_accel: projectset hook: output_field_types ({}) and \
             output_field_names ({}) length mismatch for fn_oid={}; bailing",
            entry.output_field_types.len(),
            entry.output_field_names.len(),
            u32::from(fn_oid),
        );
        return;
    }

    // Extract every constant argument from the funcexpr's args list.
    // SRF-from-FunctionScan args MUST be plan-time constants — a Var arg
    // would imply a lateral join (`FROM t, srf(t.col)`) which the
    // dispatcher cannot batch as a single call.
    // SAFETY: funcexpr.args is a List of Node pointers from the planner.
    let args_list = unsafe { (*funcexpr).args };
    let n_args: usize = if args_list.is_null() {
        0
    } else {
        // SAFETY: args_list is non-null and was confirmed a valid List by the
        // null guard above; list_length on a valid List is safe.
        let len = unsafe { pg_sys::list_length(args_list) };
        usize::try_from(len).unwrap_or(0)
    };

    // Verify every arg is a Const (or NULL list) — abort otherwise.
    let mut serialized_args: Vec<(i64, u32)> = Vec::with_capacity(n_args);
    for i in 0..n_args {
        // SAFETY: i is in [0, n_args).
        let arg_node = unsafe { pg_sys::list_nth(args_list, i as c_int).cast::<pg_sys::Node>() };
        if arg_node.is_null() {
            return;
        }
        // SAFETY: arg_node is a valid Node; reading the tag.
        if unsafe { (*arg_node).type_ } != pg_sys::NodeTag::T_Const {
            pgrx::debug1!(
                "pg_accel: projectset hook: non-Const arg {} for fn_oid={}; \
                 declining (lateral SRF args not supported)",
                i,
                u32::from(fn_oid),
            );
            return;
        }
        // SAFETY: tag confirmed Const.
        let cst = arg_node.cast::<pg_sys::Const>();
        let datum = unsafe { (*cst).constvalue };
        let typ_oid = unsafe { (*cst).consttype };
        // PG Datum is `usize` on 64-bit; store as i64 to fit the
        // (datum_as_i64, type_oid_as_u32) tuple expected by FunctionScanPrivData.
        #[allow(clippy::cast_possible_wrap)]
        let datum_i64 = datum.value() as i64;
        serialized_args.push((datum_i64, u32::from(typ_oid)));
    }

    let priv_data = FunctionScanPrivData {
        fn_oid,
        output_shape_disc: shape_disc.to_i32(),
        output_shape_field_count: shape_field_count,
        args: serialized_args,
    };

    // Build a CustomPath. Function rels typically have a small `tuples`
    // estimate (PG defaults to 1000 for unknown SRFs); use that as the row
    // estimate so add_path's cost comparison is honest.
    // SAFETY: rel is a valid RelOptInfo pointer.
    let rel_ref = unsafe { &*rel };
    let path_rows = rel_ref.rows.max(1.0);

    // SAFETY: palloc0 returns zeroed memory in CurrentMemoryContext.
    let cpath = unsafe { pg_sys::palloc0(std::mem::size_of::<CustomPath>()).cast::<CustomPath>() };

    // SAFETY: cpath is freshly palloc'd and zeroed.
    unsafe {
        (*cpath).path.type_ = NodeTag::T_CustomPath;
        (*cpath).path.pathtype = NodeTag::T_CustomScan;
        (*cpath).path.parent = rel;
        (*cpath).path.pathtarget = (*rel).reltarget;
        (*cpath).path.param_info = std::ptr::null_mut();
        (*cpath).path.parallel_aware = false;
        // FunctionScan with constant args is parallel_safe (no per-row var ref).
        (*cpath).path.parallel_safe = false;
        (*cpath).path.parallel_workers = 0;
        (*cpath).path.rows = path_rows;
        // Cost: small constant + per-row dispatch overhead. We undercut PG's
        // FunctionScan only when the GPU dispatch is non-trivial; for the
        // var-output H3 ops with 100+ output cells this is dramatically
        // cheaper than per-row FunctionCallInvoke.
        (*cpath).path.startup_cost = 1.0;
        #[allow(clippy::cast_precision_loss)]
        let total = 1.0_f64 + path_rows * 0.005_f64; // 5µs per emitted tuple
        (*cpath).path.total_cost = total * gucs::cost_multiplier();
        (*cpath).path.pathkeys = std::ptr::null_mut();

        (*cpath).flags = 0;
        // No child plan: FunctionScan dispatches the funcexpr itself.
        (*cpath).custom_paths = std::ptr::null_mut();
        (*cpath).custom_restrictinfo = std::ptr::null_mut();

        // Serialize the FunctionScanPrivData into the path's custom_private.
        // Layout starts at index 0 with the FUNCTIONSCAN_SENTINEL marker so
        // the deserializer can confirm the block format.
        let mut priv_list: *mut pg_sys::List = std::ptr::null_mut();
        priv_list = append_functionscan_priv(priv_list, &priv_data);
        (*cpath).custom_private = priv_list;

        (*cpath).methods = function_path_methods();

        pg_sys::add_path(rel, cpath.cast());
    }

    stats::record_planner_hook_call();
    pgrx::debug1!(
        "pg_accel: projectset hook: injected GpuFunctionScan path for fn_oid={} \
         shape_disc={} field_count={} n_args={}",
        u32::from(fn_oid),
        shape_disc.to_i32(),
        shape_field_count,
        n_args,
    );
    let _ = custom_scan::scan_path_methods; // touch import for clarity
}
