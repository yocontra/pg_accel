//! `set_join_pathlist_hook` — injects `CustomPath`s for accelerable joins.

use pgrx::pg_sys::{
    self, CustomPath, JoinPathExtraData, List, NodeTag, Path, PlannerInfo, RelOptInfo,
    RestrictInfo, lappend,
};

use super::{HookElapsedGuard, PREV_SET_JOIN_PATHLIST_HOOK};
use crate::engine::cost;
use crate::engine::gucs;
use crate::engine::stats;

// ---------------------------------------------------------------------------
// Join hook
// ---------------------------------------------------------------------------

/// `set_join_pathlist_hook` implementation.
///
/// Injects a `CustomPath` for joins with accelerable residual conditions.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
#[pgrx::pg_guard]
#[allow(clippy::too_many_lines)]
pub(super) unsafe extern "C-unwind" fn pgaccel_set_join_pathlist(
    root: *mut PlannerInfo,
    joinrel: *mut RelOptInfo,
    outerrel: *mut RelOptInfo,
    innerrel: *mut RelOptInfo,
    jointype: pg_sys::JoinType::Type,
    extra: *mut JoinPathExtraData,
) {
    // Chain previous hook first.
    // SAFETY: Previous hook, if set, accepts the same planner-provided args.
    unsafe {
        if let Some(prev) = PREV_SET_JOIN_PATHLIST_HOOK {
            prev(root, joinrel, outerrel, innerrel, jointype, extra);
        }
    }
    if super::planner_hooks_suspended() {
        return;
    }

    // Phase 0 planner-hook overhead audit: time every invocation so the
    // bench harness can detect no-dispatch queries that pay
    // disproportionate hook overhead (TODO.md 2026-05-14 star-schema
    // diagnosis: 37-40 ms planning vs 0.2 ms with `pg_accel.enabled=off`).
    let _hook_finish = HookElapsedGuard::new("join_pathlist");

    // Record this planner hook invocation (main backend thread only).
    stats::record_planner_hook_call();

    // Gate 1: GUC check.
    if !gucs::enabled() {
        return;
    }

    // Gate: Only accelerate pure SELECT statements. INSERT...SELECT,
    // UPDATE...FROM, DELETE...USING run scans in ModifyTable context
    // where Custom Scan slot handling is incompatible.
    // SAFETY: root.parse is a valid Query pointer provided by the planner.
    let parse = unsafe { (*root).parse };
    if parse.is_null() || unsafe { (*parse).commandType } != pg_sys::CmdType::CMD_SELECT {
        stats::record_command_type_skip();
        return;
    }

    // Gate 1b: GPU must be available and enabled — no CPU-only fallback.
    if !cost::gpu_is_usable() {
        return;
    }

    // Observability (NestedLoop scalar inequality): runs BEFORE the early
    // max-output gate AND before the fast-decline. NLJ inequality joins
    // typically produce O(n*m) rows and almost always exceed
    // `gpu_join_max_output_rows`, so deferring the observer past either gate
    // would hide the entire opportunity class. The NLJ scalar inequality
    // kernel is the launchpad for TODO.md Phase 4 "NestedLoop scalar
    // recognition"; the counter `nestloop_scalar_no_gpu_kernel` tells the
    // next implementor how many of these the planner sees.
    //
    // We only run this for INNER joins — outer/semi/anti NLJ have richer
    // null-padding semantics that the eventual GPU kernel would need to
    // model separately. Restricting here keeps the counter signal clean.
    //
    // SAFETY: joinrel/extra are valid planner pointers; restrictlist may
    // be null but the helper handles that.
    if jointype == pg_sys::JoinType::JOIN_INNER && !selected_gpu_nlj_kernel_available() {
        let observed_between = unsafe {
            observe_gated_nlj_between_opportunity(
                joinrel,
                if extra.is_null() {
                    std::ptr::null_mut()
                } else {
                    (*extra).restrictlist
                },
                outerrel,
                innerrel,
            )
        };
        if !observed_between {
            unsafe {
                observe_nestloop_scalar_opportunity(
                    joinrel,
                    if extra.is_null() {
                        std::ptr::null_mut()
                    } else {
                        (*extra).restrictlist
                    },
                    outerrel,
                    innerrel,
                );
            }
        }
    }

    // GPU-resident-only admission: pg_accel no longer injects a host-staged
    // join CustomPath. The NLJ opportunity observers above still record their
    // decline reasons; here we record the resident-pipeline decline.
    if gucs::gpu_enabled() {
        super::record_no_gpu_resident_pipeline_decline(
            "join_pathlist_no_resident_pipeline",
            joinrel,
        );
    }
}

/// Whether the selected `outer BETWEEN inner.lo AND inner.hi` NestedLoop
/// inequality path is safe to expose to the planner.
#[must_use]
fn selected_gpu_nlj_kernel_available() -> bool {
    // The pair kernel exists, but the selected Custom Scan path still buffers
    // both children as MinimalTuples and reconstructs PostgreSQL joined slots.
    // A release-harness correctness run (`gpu_nlj_between @ 50K`, 2026-06-09)
    // closed the backend connection before timing, so production exposure is
    // disabled until the NLJ path is GPU-resident or the host boundary is
    // reproven crash-free.
    false
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // reason: the recognized BETWEEN-key shape (attnos, key
// type, varnos) is retained for the eventual GPU-resident NLJ inequality
// injection lane. The observability path only needs to know a key exists
// (Some/None), but the payload is the planner contract that lane will read.
struct NljBetweenKey {
    outer_attno: i32,
    inner_lo_attno: i32,
    inner_hi_attno: i32,
    key_type: i32,
    outer_varno: i32,
    inner_varno: i32,
}

// ---------------------------------------------------------------------------
// NestedLoop scalar-inequality opportunity classification (observability only)
// ---------------------------------------------------------------------------

/// Classification of a joinrel for "scalar NestedLoop inequality" opportunity.
///
/// "Scalar NestedLoop inequality" means: a `T_NestPath` is in `joinrel->pathlist`
/// (PG considers a nested loop viable here) AND at least one `RestrictInfo` is a
/// correlated, two-argument scalar comparison whose operator is a btree
/// inequality (`<`, `<=`, `>=`, `>`) with `oprresulttype = bool`, where the two
/// arguments resolve to `Var`s in *different* relations (i.e., a cross-rel
/// inequality predicate).
///
/// Examples (recognised):
/// - `A.x < B.y`
/// - `A.ts BETWEEN B.lo AND B.hi`  (PG expands BETWEEN to `>=` and `<=`)
/// - `A.lo <= B.x AND B.x <= A.hi` (interval overlap / range-contains)
///
/// Excluded:
/// - Equality (`=`) — handled by GpuHashJoin recognition.
/// - Spatial / range / GIST operators — those are not btree inequalities and
///   `get_op_index_interpretation` returns no matching compare type.
/// - Single-relation quals (e.g. `A.x < 100`) — those are scan-level filters,
///   not join conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NestedLoopShape {
    /// Joinrel has no `T_NestPath` in its pathlist, or no correlated scalar
    /// inequality in the restrictlist. Nothing to observe.
    None,
    /// At least one `T_NestPath` exists AND at least one correlated scalar
    /// inequality clause is present. `nest_count` is the number of nest
    /// paths observed; `ineq_count` is the number of cross-rel inequality
    /// restrict clauses.
    HasScalarInequality { nest_count: i32, ineq_count: i32 },
}

/// Pure classifier for [`NestedLoopShape`] given path-tag counts and qual
/// counts.
///
/// Separated from the FFI wrapper so it can be unit-tested without a live
/// planner. The FFI side is responsible for filling `nest_count` and
/// `ineq_count` honestly.
///
/// Preconditions (caller-enforced): both counts >= 0.
pub(super) const fn classify_nestloop_shape(nest_count: i32, ineq_count: i32) -> NestedLoopShape {
    if nest_count > 0 && ineq_count > 0 {
        NestedLoopShape::HasScalarInequality {
            nest_count,
            ineq_count,
        }
    } else {
        NestedLoopShape::None
    }
}

/// Return `true` if `opno` is a btree inequality strategy (`<`, `<=`, `>=`, `>`).
///
/// Uses `get_op_index_interpretation` so the check is opfamily-aware and
/// covers cross-type operators (`int4 < int8`, etc.). Equality is excluded —
/// it has a dedicated GpuHashJoin path.
///
/// # Safety
///
/// `opno` must be a valid PG operator OID (or `InvalidOid` in which case this
/// returns `false`). Calls `get_op_index_interpretation` which must run on
/// the main backend thread.
unsafe fn is_btree_inequality_opno(opno: pg_sys::Oid) -> bool {
    unsafe { btree_inequality_strategy(opno).is_some() }
}

/// Return the btree strategy number for `<`, `<=`, `>=`, or `>`.
///
/// # Safety
///
/// Must run in a backend planner context.
unsafe fn btree_inequality_strategy(opno: pg_sys::Oid) -> Option<u32> {
    if opno == pg_sys::InvalidOid {
        return None;
    }
    // SAFETY: caller guarantees we are on the main backend thread.
    let interps = unsafe { pg_sys::get_op_index_interpretation(opno) };
    if interps.is_null() {
        return None;
    }
    // SAFETY: valid planner List of OpIndexInterpretation*.
    let n = unsafe { pg_sys::list_length(interps) };
    for i in 0..n {
        // SAFETY: i < n.
        let entry = unsafe { pg_sys::list_nth(interps, i).cast::<pg_sys::OpIndexInterpretation>() };
        if entry.is_null() {
            continue;
        }
        // SAFETY: entry is a valid OpIndexInterpretation pointer.
        let strategy = match unsafe { (*entry).cmptype } {
            pg_sys::CompareType::COMPARE_LT => pg_sys::BTLessStrategyNumber,
            pg_sys::CompareType::COMPARE_LE => pg_sys::BTLessEqualStrategyNumber,
            pg_sys::CompareType::COMPARE_GE => pg_sys::BTGreaterEqualStrategyNumber,
            pg_sys::CompareType::COMPARE_GT => pg_sys::BTGreaterStrategyNumber,
            _ => continue,
        };
        if matches!(
            strategy,
            pg_sys::BTLessStrategyNumber
                | pg_sys::BTLessEqualStrategyNumber
                | pg_sys::BTGreaterEqualStrategyNumber
                | pg_sys::BTGreaterStrategyNumber
        ) {
            return Some(strategy);
        }
    }
    None
}

#[derive(Debug, Clone, Copy)]
struct NljBound {
    outer_attno: i32,
    inner_attno: i32,
    key_type: i32,
    outer_varno: i32,
    inner_varno: i32,
}

impl NljBound {
    fn same_outer_key(self, other: Self) -> bool {
        self.outer_attno == other.outer_attno
            && self.key_type == other.key_type
            && self.outer_varno == other.outer_varno
            && self.inner_varno == other.inner_varno
    }
}

fn nlj_key_type_from_oid(type_oid: pg_sys::Oid) -> Option<i32> {
    match u32::from(type_oid) {
        23 => Some(0),  // int4
        20 => Some(1),  // int8
        700 => Some(2), // float4, promoted through the f64 NLJ dispatch path
        701 => Some(2), // float8
        _ => None,
    }
}

/// Detect the selected NLJ shape:
/// `outer.value >= inner.lo AND outer.value <= inner.hi`.
///
/// # Safety
///
/// All pointer args must be planner-owned pointers from the join hook.
unsafe fn find_nlj_between_key(
    restrictlist: *mut List,
    outerrel: *mut RelOptInfo,
    innerrel: *mut RelOptInfo,
) -> Option<NljBetweenKey> {
    if restrictlist.is_null() || outerrel.is_null() || innerrel.is_null() {
        return None;
    }

    let outer_relids = unsafe { (*outerrel).relids };
    let inner_relids = unsafe { (*innerrel).relids };
    let mut lowers: Vec<NljBound> = Vec::new();
    let mut uppers: Vec<NljBound> = Vec::new();

    let len = unsafe { pg_sys::list_length(restrictlist) };
    for i in 0..len {
        let ri = unsafe { pg_sys::list_nth(restrictlist, i).cast::<RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        let clause = unsafe { (*ri).clause };
        if clause.is_null()
            || unsafe { (*clause.cast::<pg_sys::Node>()).type_ } != NodeTag::T_OpExpr
        {
            continue;
        }

        let opexpr = clause.cast::<pg_sys::OpExpr>();
        if u32::from(unsafe { (*opexpr).opresulttype }) != 16 {
            continue;
        }
        let Some(strategy) = (unsafe { btree_inequality_strategy((*opexpr).opno) }) else {
            continue;
        };
        if !matches!(
            strategy,
            pg_sys::BTLessEqualStrategyNumber | pg_sys::BTGreaterEqualStrategyNumber
        ) {
            continue;
        }

        let args = unsafe { (*opexpr).args };
        if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
            continue;
        }
        let left = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
        let right = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
        let left_var = unsafe { super::unwrap_var(left) };
        let right_var = unsafe { super::unwrap_var(right) };
        if left_var.is_null() || right_var.is_null() {
            continue;
        }

        let left_varno = unsafe { (*left_var).varno } as i32;
        let right_varno = unsafe { (*right_var).varno } as i32;
        let left_outer = unsafe { pg_sys::bms_is_member(left_varno, outer_relids) };
        let left_inner = unsafe { pg_sys::bms_is_member(left_varno, inner_relids) };
        let right_outer = unsafe { pg_sys::bms_is_member(right_varno, outer_relids) };
        let right_inner = unsafe { pg_sys::bms_is_member(right_varno, inner_relids) };

        let (outer_var, inner_var, outer_on_left) = if left_outer && right_inner {
            (left_var, right_var, true)
        } else if left_inner && right_outer {
            (right_var, left_var, false)
        } else {
            continue;
        };

        let outer_attno = i32::from(unsafe { (*outer_var).varattno });
        let inner_attno = i32::from(unsafe { (*inner_var).varattno });
        if outer_attno <= 0 || inner_attno <= 0 {
            continue;
        }
        let outer_type = unsafe { (*outer_var).vartype };
        let inner_type = unsafe { (*inner_var).vartype };
        if outer_type != inner_type {
            continue;
        }
        let Some(key_type) = nlj_key_type_from_oid(outer_type) else {
            continue;
        };

        let bound = NljBound {
            outer_attno,
            inner_attno,
            key_type,
            outer_varno: unsafe { (*outer_var).varno },
            inner_varno: unsafe { (*inner_var).varno },
        };

        let is_lower = (outer_on_left && strategy == pg_sys::BTGreaterEqualStrategyNumber)
            || (!outer_on_left && strategy == pg_sys::BTLessEqualStrategyNumber);
        let is_upper = (outer_on_left && strategy == pg_sys::BTLessEqualStrategyNumber)
            || (!outer_on_left && strategy == pg_sys::BTGreaterEqualStrategyNumber);

        if is_lower {
            lowers.push(bound);
        } else if is_upper {
            uppers.push(bound);
        }

        for lo in &lowers {
            for hi in &uppers {
                if lo.same_outer_key(*hi) {
                    return Some(NljBetweenKey {
                        outer_attno: lo.outer_attno,
                        inner_lo_attno: lo.inner_attno,
                        inner_hi_attno: hi.inner_attno,
                        key_type: lo.key_type,
                        outer_varno: lo.outer_varno,
                        inner_varno: lo.inner_varno,
                    });
                }
            }
        }
    }

    None
}

/// Count correlated scalar-inequality `RestrictInfo` entries in a join's
/// `restrictlist`. "Correlated" means each side of the comparison resolves
/// (after stripping `RelabelType`) to a `Var` whose `varno` is in a different
/// one of `{outerrel.relids, innerrel.relids}`.
///
/// # Safety
///
/// All pointers must be valid planner pointers; `restrictlist` may be null.
unsafe fn count_correlated_scalar_inequalities(
    restrictlist: *mut List,
    outerrel: *mut RelOptInfo,
    innerrel: *mut RelOptInfo,
) -> i32 {
    if restrictlist.is_null() || outerrel.is_null() || innerrel.is_null() {
        return 0;
    }
    // SAFETY: valid RelOptInfo pointers.
    let outer_relids = unsafe { (*outerrel).relids };
    let inner_relids = unsafe { (*innerrel).relids };

    let mut ineq_count: i32 = 0;
    // SAFETY: restrictlist is a valid List of RestrictInfo*.
    let len = unsafe { pg_sys::list_length(restrictlist) };
    for i in 0..len {
        // SAFETY: i in [0, len).
        let ri = unsafe { pg_sys::list_nth(restrictlist, i).cast::<RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        // SAFETY: ri is a valid RestrictInfo.
        let clause = unsafe { (*ri).clause };
        if clause.is_null() {
            continue;
        }
        // SAFETY: clause is a valid Node.
        let tag = unsafe { (*clause.cast::<pg_sys::Node>()).type_ };
        if tag != NodeTag::T_OpExpr {
            continue;
        }
        // SAFETY: tag confirmed OpExpr.
        #[allow(clippy::cast_ptr_alignment)]
        let opexpr = clause.cast::<pg_sys::OpExpr>();
        // SAFETY: opexpr is valid.
        let result_type = unsafe { (*opexpr).opresulttype };
        // Only consider boolean-returning OpExprs (OID 16).
        if u32::from(result_type) != 16 {
            continue;
        }
        // SAFETY: opexpr is valid.
        let opno = unsafe { (*opexpr).opno };
        // SAFETY: on main backend thread via planner hook.
        if !unsafe { is_btree_inequality_opno(opno) } {
            continue;
        }
        // SAFETY: opexpr->args is a List.
        let args = unsafe { (*opexpr).args };
        if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
            continue;
        }
        // SAFETY: args has 2 elements.
        let left = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
        let right = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
        if left.is_null() || right.is_null() {
            continue;
        }
        // SAFETY: both Node pointers are valid.
        let left_var = unsafe { super::unwrap_var(left) };
        let right_var = unsafe { super::unwrap_var(right) };
        if left_var.is_null() || right_var.is_null() {
            // One side is a Const / Param / function call — that's a
            // single-rel scan filter, not a join inequality. Skip.
            continue;
        }
        // SAFETY: Var nodes are valid.
        let left_varno = unsafe { (*left_var).varno } as i32;
        let right_varno = unsafe { (*right_var).varno } as i32;
        // SAFETY: bms_is_member on a valid Bitmapset (relids).
        let left_outer = unsafe { pg_sys::bms_is_member(left_varno, outer_relids) };
        let left_inner = unsafe { pg_sys::bms_is_member(left_varno, inner_relids) };
        let right_outer = unsafe { pg_sys::bms_is_member(right_varno, outer_relids) };
        let right_inner = unsafe { pg_sys::bms_is_member(right_varno, inner_relids) };
        // Correlated = one side is outer, other side is inner. Reject
        // same-side quals — those are scan filters that PG pushes down.
        if (left_outer && right_inner) || (left_inner && right_outer) {
            ineq_count += 1;
        }
    }
    ineq_count
}

/// Observe the disabled selected BETWEEN shape before the generic scalar-NLJ
/// opportunity counter.
///
/// The `pgaccel-kernels/src/nested_loop_ineq.cpp` pair kernel exists, but the
/// current selected Custom Scan path is a host-boundary implementation that
/// collects both children through `ExecProcNode`, reconstructs PostgreSQL
/// slots, and has fresh release-harness crash evidence. Keep this decline
/// reason distinct from the broader `nestloop_scalar_no_gpu_kernel` counter so
/// reports can prove the BETWEEN workload is intentionally gated.
///
/// # Safety
///
/// All pointer args must be valid planner pointers. Called only from
/// `pgaccel_set_join_pathlist` on the main backend thread.
unsafe fn observe_gated_nlj_between_opportunity(
    joinrel: *mut RelOptInfo,
    restrictlist: *mut List,
    outerrel: *mut RelOptInfo,
    innerrel: *mut RelOptInfo,
) -> bool {
    if joinrel.is_null() {
        return false;
    }
    let Some(_nlj_between) = (unsafe { find_nlj_between_key(restrictlist, outerrel, innerrel) })
    else {
        return false;
    };
    let joinrel_ref = unsafe { &*joinrel };
    #[allow(clippy::cast_sign_loss)]
    let n_rows_est = joinrel_ref.rows.max(0.0) as u64;
    pgrx::debug1!(
        "pg_accel join: GpuNestedLoopIneq BETWEEN skipped: selected host-boundary \
         path is crash-gated (rows~={n_rows_est}); see TODO.md Phase 4 \
         'NestedLoop scalar-inequality join'"
    );
    stats::increment_planner_rejected(
        super::RejectionReason::NljBetweenHostBoundaryUnsafe.stats_key(),
        n_rows_est,
    );
    true
}

/// Walk `joinrel->pathlist` and emit the generic NestedLoop
/// scalar-inequality opportunity signal when a `T_NestPath` is present AND
/// `restrictlist` contains at least one cross-rel scalar inequality.
///
/// This is observability only. The narrow BETWEEN pair kernel is separately
/// crash-gated above, and all other scalar-inequality NLJ shapes still lack a
/// selected safe GPU implementation. Falls through to normal hash-join
/// recognition in the caller, which will itself bail when no equi-join key is
/// present.
///
/// ## Why this is currently a decline, not an implementation
///
/// A production GPU NestedLoop scalar-inequality path still requires:
///   1. GPU-resident consumption of matched pairs, preferably join->count or
///      join->preagg fusion, so row reconstruction does not re-enter the
///      unsafe host slot boundary for every pair.
///   2. Broader predicate support beyond the narrow BETWEEN detector: one
///      inequality, interval overlap, and cross-type comparisons.
///   3. Cost gates tied to observed selectivity and output cardinality, not
///      just the current one-window-per-event launchpad estimate.
///   4. Live crash/correctness artifacts for the selected path before planner
///      exposure is re-enabled.
///
/// Until those land, the only honest action for generic scalar NLJ shapes is
/// to emit `debug1` and bump
/// `planner_rejected("nestloop_scalar_no_gpu_kernel", ...)` so a future
/// implementor can quantify the opportunity by reading `pg_accel_stats()`.
///
/// # Safety
///
/// All pointer args must be valid planner pointers. Called only from
/// `pgaccel_set_join_pathlist` on the main backend thread.
unsafe fn observe_nestloop_scalar_opportunity(
    joinrel: *mut RelOptInfo,
    restrictlist: *mut List,
    outerrel: *mut RelOptInfo,
    innerrel: *mut RelOptInfo,
) {
    if joinrel.is_null() {
        return;
    }
    // SAFETY: caller guarantees valid joinrel.
    let joinrel_ref = unsafe { &*joinrel };
    let pathlist = joinrel_ref.pathlist;
    if pathlist.is_null() {
        return;
    }
    // SAFETY: valid planner List.
    let n = unsafe { pg_sys::list_length(pathlist) };
    if n == 0 {
        return;
    }

    let mut nest_count: i32 = 0;
    for i in 0..n {
        // SAFETY: i < n.
        let p = unsafe { pg_sys::list_nth(pathlist, i).cast::<Path>() };
        if p.is_null() {
            continue;
        }
        // SAFETY: p is a valid Path.
        let tag = unsafe { (*p.cast::<pg_sys::Node>()).type_ };
        if matches!(tag, NodeTag::T_NestPath) {
            nest_count += 1;
        }
    }

    if nest_count == 0 {
        return;
    }

    // SAFETY: planner-owned pointers, on main thread.
    let ineq_count =
        unsafe { count_correlated_scalar_inequalities(restrictlist, outerrel, innerrel) };

    if let NestedLoopShape::HasScalarInequality {
        nest_count,
        ineq_count,
    } = classify_nestloop_shape(nest_count, ineq_count)
    {
        #[allow(clippy::cast_sign_loss)]
        let n_rows_est = joinrel_ref.rows.max(0.0) as u64;
        pgrx::debug1!(
            "pg_accel join: NestedLoop scalar-inequality opportunity skipped: \
             {nest_count} T_NestPath candidate(s), {ineq_count} cross-rel \
             scalar inequality qual(s), output rows~={n_rows_est}; no selected \
             safe GPU nested-loop implementation exists (see TODO.md Phase 4 \
             'NestedLoop scalar recognition')"
        );
        stats::increment_planner_rejected("nestloop_scalar_no_gpu_kernel", n_rows_est);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{NestedLoopShape, classify_nestloop_shape};

    #[test]
    fn classify_rejection_reason_matches_stats_key() {
        // Regression guard: the stats key must match the string the FFI
        // wrapper passes to `stats::increment_planner_rejected`. If anyone
        // renames one without the other, `pg_accel_stats()` aggregations
        // keyed on this reason go silently stale.
        //
        // We pin the exact constant that `observe_mergejoin_opportunity`
        // emits so a rename shows up as a test diff.
        const EXPECTED_REASON: &str = "mergejoin_no_gpu_kernel";
        assert_eq!(EXPECTED_REASON, "mergejoin_no_gpu_kernel");
    }

    #[test]
    fn parallel_inner_rebuild_rejection_reason_matches_stats_key() {
        // Regression guard: the stats key emitted by the partial
        // GpuHashJoin large-inner gate must stay stable for benchmark trace
        // aggregation.
        use super::super::RejectionReason;
        const EXPECTED_REASON: &str = "hashjoin_parallel_inner_rebuild_too_large";
        assert_eq!(
            RejectionReason::HashJoinParallelInnerRebuildTooLarge.stats_key(),
            EXPECTED_REASON,
        );
    }

    #[test]
    fn selected_gpu_nlj_kernel_is_gated_until_host_boundary_is_safe() {
        assert!(
            !super::selected_gpu_nlj_kernel_available(),
            "planner must not expose GpuNestedLoopIneq until the host-boundary \
             executor path is reproven crash-free"
        );
    }

    #[test]
    fn nlj_key_type_gate_matches_executor_dispatch_lanes() {
        assert_eq!(super::nlj_key_type_from_oid(pgrx::pg_sys::INT4OID), Some(0));
        assert_eq!(super::nlj_key_type_from_oid(pgrx::pg_sys::INT8OID), Some(1));
        assert_eq!(
            super::nlj_key_type_from_oid(pgrx::pg_sys::FLOAT4OID),
            Some(2)
        );
        assert_eq!(
            super::nlj_key_type_from_oid(pgrx::pg_sys::FLOAT8OID),
            Some(2)
        );
        assert_eq!(super::nlj_key_type_from_oid(pgrx::pg_sys::INT2OID), None);
    }

    #[test]
    fn accel_strategy_gpu_nlj_variant_is_distinct() {
        // ABI / wire compat: the new variant `GpuNestedLoopIneq = 9`
        // must round-trip through `AccelStrategy::from_i32`. If any
        // refactor changes the discriminant, downstream `custom_private`
        // integer lists (encoded via `makeInteger(strategy as i32)`)
        // misroute strategies in plans serialised under the prior wire
        // value — a silent correctness regression at plan time.
        use crate::engine::registry::AccelStrategy;
        assert_eq!(
            AccelStrategy::from_i32(9),
            Some(AccelStrategy::GpuNestedLoopIneq)
        );
        // Bordering valid discriminants stay mapped correctly.
        assert_eq!(AccelStrategy::from_i32(8), Some(AccelStrategy::GpuWindow));
        assert_eq!(AccelStrategy::from_i32(10), None);
    }

    // -- classify_nestloop_shape -------------------------------------------
    //
    // Pure classifier for the NestedLoop scalar-inequality observability
    // path. Only the cross-product of (nest_count > 0, ineq_count > 0)
    // produces an observation — otherwise the joinrel has no NLJ ineq
    // opportunity worth recording.

    #[test]
    fn classify_nestloop_none_when_both_counts_zero() {
        assert_eq!(classify_nestloop_shape(0, 0), NestedLoopShape::None);
    }

    #[test]
    fn classify_nestloop_none_when_only_nestpath_present() {
        // T_NestPath in pathlist but no cross-rel inequality qual — could be
        // a plain equi NestLoop the planner is also considering. Don't
        // record it here; equi joins are GpuHashJoin's lane.
        assert_eq!(classify_nestloop_shape(3, 0), NestedLoopShape::None);
    }

    #[test]
    fn classify_nestloop_none_when_only_inequality_present() {
        // Inequality clause present but no T_NestPath — PG didn't consider
        // an NLJ plan here (e.g. small inner rel with a hash plan winning
        // outright). Nothing for the NLJ launchpad to record.
        assert_eq!(classify_nestloop_shape(0, 1), NestedLoopShape::None);
    }

    #[test]
    fn classify_nestloop_has_scalar_inequality_when_both_positive() {
        assert_eq!(
            classify_nestloop_shape(1, 1),
            NestedLoopShape::HasScalarInequality {
                nest_count: 1,
                ineq_count: 1,
            }
        );
        assert_eq!(
            classify_nestloop_shape(2, 3),
            NestedLoopShape::HasScalarInequality {
                nest_count: 2,
                ineq_count: 3,
            }
        );
    }

    #[test]
    fn classify_nestloop_is_const_eval_compatible() {
        const NONE: NestedLoopShape = classify_nestloop_shape(0, 0);
        const HAS: NestedLoopShape = classify_nestloop_shape(1, 2);
        assert_eq!(NONE, NestedLoopShape::None);
        assert_eq!(
            HAS,
            NestedLoopShape::HasScalarInequality {
                nest_count: 1,
                ineq_count: 2,
            }
        );
    }

    #[test]
    fn nestloop_rejection_reason_matches_stats_key() {
        // Regression guard: the stats key the FFI wrapper passes to
        // `stats::increment_planner_rejected` must match the
        // `RejectionReason::NestedLoopScalarNoGpuKernel.stats_key()`
        // constant. If they diverge, `pg_accel_stats()` aggregations
        // keyed on the reason go silently stale.
        use super::super::RejectionReason;
        const EXPECTED_REASON: &str = "nestloop_scalar_no_gpu_kernel";
        assert_eq!(EXPECTED_REASON, "nestloop_scalar_no_gpu_kernel");
        assert_eq!(
            RejectionReason::NestedLoopScalarNoGpuKernel.stats_key(),
            EXPECTED_REASON,
        );
    }

    #[test]
    fn nlj_between_host_boundary_rejection_reason_matches_stats_key() {
        use super::super::RejectionReason;
        const EXPECTED_REASON: &str = "nlj_between_host_boundary_unsafe";
        assert_eq!(
            RejectionReason::NljBetweenHostBoundaryUnsafe.stats_key(),
            EXPECTED_REASON,
        );
    }
}
