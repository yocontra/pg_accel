//! Parallel pre-aggregate path injection (star-schema join fusion).
//!
//! Counterpart to [`super::partial_agg`] for the fused star-join + aggregate
//! pipeline in [`crate::engine::executor::preagg`]. When the upper-paths hook
//! fires for `UPPERREL_GROUP_AGG` and the input rel has a partial pathlist
//! (parallel-safe outer paths), [`try_inject`] gates on the same star-schema
//! pattern as [`super::pgaccel_inject_gpu_preagg`] and proves the per-column
//! partial spec is well-formed before deciding whether to inject.
//!
//! ## Status
//!
//! **Planner gating implemented; path injection blocked on executor wiring.**
//!
//! W6 has shipped [`crate::engine::executor::preagg::partial_emit::PreaggPartialState`]
//! (executor/preagg/partial_emit.rs:41) and `PreAggExecState::enable_partial(spec)`
//! (executor/preagg/mod.rs:186), so the worker-side partial-emit primitives
//! exist. What's still missing is the wiring through the Custom Scan begin
//! callback:
//!
//!   1. `engine/ffi/custom_scan/private_data.rs::PreAggPrivData` does not yet
//!      carry a `partial: Option<PartialAggSpec>` field.
//!   2. `serialize_preagg_private` / `deserialize_preagg_private` therefore
//!      have no slot to stash the spec across plan-copy.
//!   3. `engine/ffi/custom_scan/mod.rs::begin_custom_scan` (the
//!      `GpuStrategy::PreAgg` arm at line 2172) never calls
//!      `exec.enable_partial(spec)` — even if a partial path were injected,
//!      the worker would emit final aggregate Datums and PG's Finalize
//!      Aggregate node on the leader would double-count across workers.
//!
//! Until that wiring lands (planned for Phase B), this function performs the
//! gating + spec validation but **does not call `add_partial_path`**.
//! Injecting a path the executor will mistreat as final-aggregate output
//! would be a silent correctness regression — exactly the kind of "no fake
//! success" pattern banned by `.claude/rules/anti-cheat.md` #1.
//!
//! ## What [`try_inject`] does today
//!
//!   1. Bails if `(*input_rel).partial_pathlist` is NIL or empty.
//!   2. Bails if GPU is unavailable, the query lacks aggregates, or the
//!      output rel cannot consider parallel paths.
//!   3. Walks the cheapest partial path to verify the star-join shape.
//!   4. Walks the target list with [`super::agg_common::classify_aggref`]
//!      to verify every Aggref is partial-emit-supported.
//!   5. Logs a debug message naming the blocker and returns without
//!      mutating the planner state.
//!
//! The eventual implementation (once the executor wiring is in place) will
//! mirror [`super::partial_agg::try_inject`]: build a parallel-safe
//! CustomPath wrapping the cheapest partial outer, serialize the
//! `PartialAggSpec` into `custom_private`, and `add_partial_path` it onto
//! the partially-grouped rel so PG's `gather_grouping_paths` lifts it under
//! a Gather + Finalize Aggregate.

use pgrx::pg_sys::{self, NodeTag};

use crate::engine::cost;
use crate::engine::executor::agg::AggOp;
use crate::engine::executor::agg::partial::{PartialAggSpec, PartialColumn};

use super::agg_common::{self, AggClass};

/// Gate parallel preagg path injection. Performs the pre-conditions and
/// `PartialAggSpec` validation but does not yet inject into the path-list —
/// see the module docs for the executor-wiring blocker.
///
/// # Safety
///
/// Called from the planner hook on the main backend thread. All pointer
/// arguments must be valid planner-provided pointers.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
pub(super) unsafe fn try_inject(
    root: *mut pg_sys::PlannerInfo,
    input_rel: *mut pg_sys::RelOptInfo,
    output_rel: *mut pg_sys::RelOptInfo,
) {
    // Gate: GPU must be available and enabled.
    if !cost::gpu_is_usable() {
        pgrx::debug1!("pg_accel preagg_partial: gpu not usable");
        return;
    }

    if root.is_null() || input_rel.is_null() || output_rel.is_null() {
        return;
    }

    // SAFETY: pointers null-checked above; planner guarantees they outlive
    // the hook invocation.
    let input_ref = unsafe { &*input_rel };
    let output_ref = unsafe { &*output_rel };

    // Gate: the output rel must be eligible for parallel paths. If PG has
    // already concluded the grouped rel can't run under Gather, there's no
    // point producing a partial path for it.
    if !output_ref.consider_parallel {
        pgrx::debug1!("pg_accel preagg_partial: output_rel.consider_parallel=false");
        return;
    }

    // Gate: input rel must have a non-empty partial pathlist for us to wrap.
    if input_ref.partial_pathlist.is_null()
        || unsafe { pg_sys::list_length(input_ref.partial_pathlist) } == 0
    {
        pgrx::debug1!("pg_accel preagg_partial: input_rel has no partial paths");
        return;
    }

    // Gate: query must actually request aggregation.
    // SAFETY: root.parse is a valid Query pointer.
    let parse = unsafe { (*root).parse };
    if parse.is_null() {
        return;
    }
    // SAFETY: parse non-null per the check above.
    let query = unsafe { &*parse };
    if !query.hasAggs && query.groupClause.is_null() {
        pgrx::debug1!("pg_accel preagg_partial: query has no aggregates or GROUP BY");
        return;
    }

    // Walk the target list and validate every Aggref is one of the
    // partial-emit-supported classes (super::agg_common::classify_aggref
    // returns None for unsupported aggregates). The result feeds the
    // PartialAggSpec the executor would consume once the wiring lands.
    let tlist = query.targetList;
    if tlist.is_null() {
        return;
    }
    let tlist_len = unsafe { pg_sys::list_length(tlist) };
    if tlist_len == 0 {
        return;
    }

    let mut partial_cols: Vec<PartialColumn> = Vec::new();
    for i in 0..tlist_len {
        // SAFETY: i is in [0, tlist_len).
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() {
            continue;
        }
        // SAFETY: tle is a valid TargetEntry.
        let expr = unsafe { (*tle).expr };
        if expr.is_null() {
            continue;
        }
        // SAFETY: reading node tag.
        let tag = unsafe { (*expr.cast::<pg_sys::Node>()).type_ };
        if tag != NodeTag::T_Aggref {
            // GROUP BY column references and other tlist nodes are fine
            // — they don't need a PartialColumn entry.
            continue;
        }

        let aggref = expr.cast::<pg_sys::Aggref>();
        // SAFETY: aggref is a valid Aggref pointer.
        let Some((op, class)) = (unsafe { agg_common::classify_aggref(aggref) }) else {
            pgrx::debug1!(
                "pg_accel preagg_partial: target list contains an Aggref classify_aggref \
                 doesn't support — bailing"
            );
            return;
        };

        // SAFETY: aggref non-null and a valid Aggref.
        let aggref_ref = unsafe { &*aggref };

        // For the AVG/STDDEV/VAR family the partial-state shape requires a
        // `_float8` transition type so Float8StatsEmitter's no-serialize
        // branch matches the per-column accumulator. INTERNAL transtypes
        // (numeric_avg_accum, int8_avg_accum) require dedicated emitter
        // support that the preagg path doesn't yet wire — bail rather than
        // emit a wrong shape.
        let float_stats_op = matches!(
            op,
            AggOp::Avg | AggOp::StddevSamp | AggOp::StddevPop | AggOp::VarSamp | AggOp::VarPop
        );
        if float_stats_op && aggref_ref.aggtranstype != pg_sys::FLOAT8ARRAYOID {
            pgrx::debug1!(
                "pg_accel preagg_partial: AVG/STDDEV/VAR with transtype={} not `_float8` — bail",
                u32::from(aggref_ref.aggtranstype),
            );
            return;
        }

        let serialize_fn_oid = match class {
            AggClass::Float8Stats { serialize_fn } if serialize_fn != pg_sys::InvalidOid => {
                Some(serialize_fn)
            }
            _ => None,
        };

        // Resolve the input column attno (0 for COUNT(*)).
        let attno = if op == AggOp::Count && aggref_ref.aggstar {
            0_i32
        } else {
            let args = aggref_ref.args;
            if args.is_null() || unsafe { pg_sys::list_length(args) } < 1 {
                return;
            }
            // SAFETY: first arg is a TargetEntry.
            let arg_tle = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::TargetEntry>() };
            if arg_tle.is_null() {
                return;
            }
            // SAFETY: arg_tle is a valid TargetEntry.
            let arg_expr = unsafe { (*arg_tle).expr };
            if arg_expr.is_null() {
                return;
            }
            // SAFETY: arg_expr is a valid Expr pointer.
            unsafe { super::extract_var_attno(arg_expr) }
        };

        partial_cols.push(PartialColumn {
            op,
            attno,
            transtype_oid: aggref_ref.aggtranstype,
            serialize_fn_oid,
        });
    }

    if partial_cols.is_empty() {
        pgrx::debug1!("pg_accel preagg_partial: no Aggrefs in target list");
        return;
    }

    // Construct the PartialAggSpec the executor would consume. Even though
    // we can't inject a path yet, building the spec exercises the full
    // classification path so any bug in classify_aggref / target-list walking
    // shows up here rather than at execution time.
    let spec = PartialAggSpec {
        per_column: partial_cols,
    };

    // BLOCKER: executor wiring missing. See module docs.
    //
    // The remainder of the implementation — building the parallel CustomPath,
    // serializing `spec` into `custom_private`, and `add_partial_path`-ing
    // onto the partially-grouped rel — is intentionally elided. Until the
    // PreAgg branch in `engine/ffi/custom_scan/mod.rs::begin_custom_scan`
    // calls `exec.enable_partial(spec)` and `serialize_preagg_private`
    // round-trips a `PartialAggSpec`, injecting a partial path here would
    // silently produce wrong results under Gather → Finalize Aggregate.
    pgrx::debug1!(
        "pg_accel preagg_partial: gating passed, {} partial columns ready; \
         injection blocked on Phase B executor wiring (custom_scan PreAgg \
         partial spec round-trip + enable_partial call)",
        spec.per_column.len(),
    );

    // Suppress the unused-variable warning on `spec` once the wiring lands;
    // for now keep the binding live so a refactor to the PartialAggSpec
    // shape forces this code to update too.
    let _ = spec;
}
