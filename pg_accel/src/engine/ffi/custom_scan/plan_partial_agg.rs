//! Partial-mode `PlanCustomPath` builder.
//!
//! For partial-agg paths, `custom_scan_tlist` holds the raw Aggref
//! expressions and `plan.targetlist` uses `INDEX_VAR` Var refs. Keeping
//! these lists distinct avoids `unrecognized node type: 9 (T_Aggref)`
//! from `set_plan_references` when the same Aggref would appear in both.

use pgrx::pg_sys;

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
