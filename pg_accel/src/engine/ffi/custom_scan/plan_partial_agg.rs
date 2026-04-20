//! Partial-mode `PlanCustomPath` builder.
//!
//! For partial-agg paths, `custom_scan_tlist` holds the raw Aggref
//! expressions and `plan.targetlist` uses `INDEX_VAR` Var refs. Keeping
//! these lists distinct avoids `unrecognized node type: 9 (T_Aggref)`
//! from `set_plan_references` when the same Aggref would appear in both.
//!
//! Worker 5 fills in the body; the stub below compiles.

use pgrx::pg_sys;

#[allow(dead_code)]
pub(super) unsafe fn plan_partial_custom_path(
    _root: *mut pg_sys::PlannerInfo,
    _rel: *mut pg_sys::RelOptInfo,
    _best_path: *mut pg_sys::CustomPath,
    _tlist: *mut pg_sys::List,
    _clauses: *mut pg_sys::List,
    _custom_plans: *mut pg_sys::List,
) -> *mut pg_sys::Plan {
    todo!("Worker 5: build partial-agg CustomScan with INDEX_VAR targetlist")
}
