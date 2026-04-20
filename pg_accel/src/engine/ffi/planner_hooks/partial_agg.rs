//! Parallel aggregate path injection.
//!
//! Emits a `CustomPath` with `parallel_safe=true` via `add_partial_path`
//! when the input rel has a `partial_pathlist`. Builds [`PartialAggSpec`]
//! by reading each Aggref's `aggserialfn` and `aggtranstype` via
//! [`super::super::syscache`].
//!
//! Worker 4 fills in the real body; the stub below compiles.

use pgrx::pg_sys;

#[allow(dead_code)]
pub(super) unsafe fn try_inject(
    _root: *mut pg_sys::PlannerInfo,
    _input_rel: *mut pg_sys::RelOptInfo,
    _output_rel: *mut pg_sys::RelOptInfo,
) {
    // Worker 4:
    //   - Bail if (*input_rel).partial_pathlist is NIL
    //   - Walk (*output_rel).reltarget->exprs, classify each Aggref
    //   - For each Aggref, call syscache::agg_transtype / agg_serialize_fn
    //   - Build PartialAggSpec (defined in executor::agg::partial)
    //   - Construct CustomPath, parallel_safe=true, parallel_aware=false,
    //     parallel_workers = cheapest_partial_input_path->parallel_workers
    //   - add_partial_path(output_rel, cpath.cast())
}
