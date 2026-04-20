//! Parallel pre-aggregate path injection (star-schema join fusion).
//!
//! Counterpart to `partial_agg.rs` for the fused star-join + aggregate
//! pipeline in [`crate::engine::executor::preagg`]. Emits a partial-state
//! variant of the PreAgg CustomPath via `add_partial_path`.
//!
//! ## Status
//!
//! Stub. The preagg executor (owned by W6) doesn't yet expose a
//! partial-state emit path, so injecting a parallel preagg plan would build
//! final aggregate tuples inside each worker — wrong for `Gather → Finalize
//! Aggregate`. Once W6 lands the partial emitter, this function will:
//!
//!   1. Bail if `(*input_rel).partial_pathlist` is NIL.
//!   2. Walk the star-join tree like [`super::pgaccel_inject_gpu_preagg`].
//!   3. Build a `PartialAggSpec` for each PreAggColDesc via
//!      [`super::agg_common::classify_aggref`].
//!   4. Serialize `CustomPrivateData { partial: Some(spec), … }` and call
//!      `pg_sys::add_partial_path(output_rel, cpath.cast())`.

use pgrx::pg_sys;

/// TODO: wire to the preagg executor once W6 adds a partial-state emit path.
#[allow(dead_code, clippy::unused_async, clippy::missing_const_for_fn)]
pub(super) unsafe fn try_inject(
    _root: *mut pg_sys::PlannerInfo,
    _input_rel: *mut pg_sys::RelOptInfo,
    _output_rel: *mut pg_sys::RelOptInfo,
) {
    // no-op until W6 lands the partial emitter
}
