//! Partial-mode `PlanCustomPath` builder.
//!
//! For partial-agg paths, `custom_scan_tlist` holds the raw Aggref
//! expressions and `plan.targetlist` uses `INDEX_VAR` Var refs. Keeping
//! these lists distinct avoids `unrecognized node type: 9 (T_Aggref)`
//! from `set_plan_references` when the same Aggref would appear in both.

use std::ffi::c_int;

use pgrx::pg_sys;

/// Build a partial-mode CustomScan plan node from a `CustomPath`.
///
/// The key difference from the non-partial path is the targetlist shape:
///
/// - `custom_scan_tlist`: the raw `tlist` the planner handed us — contains
///   Aggref nodes. `ExecTypeFromTL` reads Aggref.aggtype to build the scan
///   tuple descriptor.
/// - `plan.targetlist`: shallow rewrite of `tlist` where every expression is
///   replaced with a `Var(varno = INDEX_VAR, varattno = i+1)`. This keeps the
///   outer plan's `set_plan_references` / `fix_upper_expr` pass from walking
///   into Aggref sub-nodes (which triggers `unrecognized node type: 9`).
///
/// Non-partial `plan_custom_path_agg` (`super::plan_custom_path_agg`) emits
/// the same tlist in both slots and relies on `fix_scan_expr` to shift Vars.
///
/// # Safety
/// Called by the planner on the main backend thread.
#[allow(dead_code)]
pub(super) unsafe fn plan_partial_custom_path(
    _root: *mut pg_sys::PlannerInfo,
    _rel: *mut pg_sys::RelOptInfo,
    best_path: *mut pg_sys::CustomPath,
    tlist: *mut pg_sys::List,
    clauses: *mut pg_sys::List,
    custom_plans: *mut pg_sys::List,
) -> *mut pg_sys::Plan {
    let _span = tracing::debug_span!("ffi.plan_partial_custom_path").entered();

    // SAFETY: palloc0 returns zeroed memory in CurrentMemoryContext.
    let cscan = unsafe {
        pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomScan>()).cast::<pg_sys::CustomScan>()
    };

    // SAFETY: cscan is freshly palloc'd; inputs are planner-owned and valid.
    unsafe {
        (*cscan).scan.plan.type_ = pg_sys::NodeTag::T_CustomScan;

        // custom_scan_tlist: the raw list (Aggref-bearing expressions).
        // copyObject gives us independent copies so our lists don't alias
        // other plan nodes'.
        (*cscan).custom_scan_tlist = pg_sys::copyObjectImpl(tlist.cast()).cast();
        // plan.targetlist: INDEX_VAR references into custom_scan_tlist.
        (*cscan).scan.plan.targetlist = build_index_var_tlist(tlist);

        (*cscan).scan.plan.qual = pg_sys::extract_actual_clauses(clauses, false);
        (*cscan).scan.plan.startup_cost = (*best_path).path.startup_cost;
        (*cscan).scan.plan.total_cost = (*best_path).path.total_cost;
        (*cscan).scan.plan.plan_rows = (*best_path).path.rows;

        (*cscan).scan.scanrelid = 0;
        (*cscan).flags = (*best_path).flags | pg_sys::CUSTOMPATH_SUPPORT_PROJECTION;
        (*cscan).methods = &raw const super::AGG_SCAN_METHODS.0;
        (*cscan).custom_plans = custom_plans;

        // Forward the path's custom_private unchanged; `plan_custom_path_agg`
        // owns the per-plan encoding and the caller sets that up.
        (*cscan).custom_private = (*best_path).custom_private;
    }

    cscan.cast()
}

/// Build `[Var(INDEX_VAR, i+1, expr_type, expr_collation), ...]` one entry
/// per `TargetEntry` in `raw`. Each entry's `resname` is preserved so
/// EXPLAIN still prints the original column aliases.
///
/// # Safety
/// `raw` must be a valid `List *` of `TargetEntry *`.
#[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
pub(super) unsafe fn build_index_var_tlist(raw: *mut pg_sys::List) -> *mut pg_sys::List {
    let mut result: *mut pg_sys::List = std::ptr::null_mut();
    if raw.is_null() {
        return result;
    }
    // SAFETY: raw is a valid List; list_length returns its length.
    let n = unsafe { pg_sys::list_length(raw) };
    for i in 0..n {
        // SAFETY: i is within bounds; list_nth returns the pointer element.
        let entry = unsafe { pg_sys::list_nth(raw, i).cast::<pg_sys::TargetEntry>() };
        if entry.is_null() {
            continue;
        }
        let expr = unsafe { (*entry).expr };
        if expr.is_null() {
            continue;
        }
        let expr_ty = unsafe { pg_sys::exprType(expr.cast()) };
        let expr_typmod = unsafe { pg_sys::exprTypmod(expr.cast()) };
        let expr_coll = unsafe { pg_sys::exprCollation(expr.cast()) };
        // SAFETY: makeVar allocates in CurrentMemoryContext.
        let var = unsafe {
            pg_sys::makeVar(
                pg_sys::INDEX_VAR as ::core::ffi::c_int,
                (i + 1) as pg_sys::AttrNumber,
                expr_ty,
                expr_typmod,
                expr_coll,
                0,
            )
        };
        let resname = unsafe { (*entry).resname };
        // SAFETY: var is valid; makeTargetEntry allocates in
        // CurrentMemoryContext.
        let te = unsafe {
            pg_sys::makeTargetEntry(
                var.cast::<pg_sys::Expr>(),
                (i + 1) as pg_sys::AttrNumber,
                resname,
                false,
            )
        };
        // SAFETY: lappend allocates in CurrentMemoryContext.
        result = unsafe { pg_sys::lappend(result, te.cast()) };
    }
    result
}

// Silence "unused" for the c_int import when this file is used purely for
// planning-time dispatch (no arithmetic visible here).
#[allow(dead_code)]
const _UNUSED_C_INT: c_int = 0;
