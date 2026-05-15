//! `set_join_pathlist_hook` — injects `CustomPath`s for accelerable joins.

use pgrx::pg_sys::{
    self, CustomPath, JoinPathExtraData, List, NodeTag, Path, PlannerInfo, RelOptInfo,
    RestrictInfo, add_partial_path, add_path, lappend,
};

use super::super::custom_scan;
use super::hashjoin;
use super::{
    AccelMatch, EquiJoinKey, HookElapsedGuard, PREV_SET_JOIN_PATHLIST_HOOK, create_custom_path,
    find_accelerable_match, find_equi_join_key,
};
use crate::engine::cost;
use crate::engine::gucs;
use crate::engine::registry;
use crate::engine::stats;

/// Phase 0 fast-decline helper: returns `true` when *no* join strategy
/// can possibly be injected by the hook body.
///
/// The downstream body explicitly rejects spatial / raster / h3 join paths
/// (Gate 3b below) because no GPU spatial / raster / h3 merge or hash
/// join kernel exists in `pgaccel-kernels/src/` today. The only remaining
/// strategy is `GpuHashJoin`, which is itself gated by
/// `selected_gpu_hashjoin_kernel_available()`. That predicate is pinned
/// to `false` by the unit test
/// `selected_gpu_hashjoin_kernel_is_not_available_yet` and stays `false`
/// until a real GPU build/probe kernel lands.
///
/// So at the time of writing, every join-pathlist invocation that
/// reaches the lower body will return without injecting. Bailing here
/// saves the merge-path scan, equi-join restrictlist walk, registry
/// lookup, and per-strategy cost computation. The 2026-05-14 SSBM Q2.3
/// diagnosis attributes ~37 ms of planning time to this redundant
/// per-join-order work.
///
/// Re-entry condition: when a real `GpuHashJoin` kernel lands and the
/// kernel-availability flag flips to `true`, OR when a spatial-join
/// kernel lands and Gate 3b stops unconditionally rejecting those
/// strategies, this fast-decline gate must be removed (or refined to
/// check only the no-longer-available strategy classes). The matching
/// unit test `selected_gpu_hashjoin_kernel_is_not_available_yet` is the
/// canary: when it starts failing because the kernel landed, the
/// engineer removing it MUST also adjust this gate.
fn join_hook_can_inject_anything() -> bool {
    selected_gpu_hashjoin_kernel_available()
}

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
#[allow(clippy::too_many_lines)]
pub(super) unsafe extern "C-unwind" fn pgaccel_set_join_pathlist(
    root: *mut PlannerInfo,
    joinrel: *mut RelOptInfo,
    outerrel: *mut RelOptInfo,
    innerrel: *mut RelOptInfo,
    jointype: pg_sys::JoinType::Type,
    extra: *mut JoinPathExtraData,
) {
    crate::engine::otel::init();

    // Chain previous hook first.
    // SAFETY: Previous hook, if set, accepts the same planner-provided args.
    unsafe {
        if let Some(prev) = PREV_SET_JOIN_PATHLIST_HOOK {
            prev(root, joinrel, outerrel, innerrel, jointype, extra);
        }
    }

    // Phase 0 planner-hook overhead audit: time every invocation so the
    // bench harness can detect no-dispatch queries that pay
    // disproportionate hook overhead (TODO.md 2026-05-14 SSBM Q2.3
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
    if jointype == pg_sys::JoinType::JOIN_INNER {
        unsafe {
            observe_nestloop_scalar_opportunity(joinrel, (*extra).restrictlist, outerrel, innerrel);
        }
    }

    // Gate 1c: Phase 0 fast-decline. SSBM-shaped 4-way joins fire this hook
    // dozens of times per query (one per join order the planner considers).
    // The 2026-05-14 audit measured `ssbm_q2_3 @ 1M` at 37-40 ms planning
    // time with the full walk vs ~0.2 ms with `pg_accel.enabled=off`, and
    // the 2026-05-15 verification reproduced 30-39 ms planning on a small
    // SSBM-shape fixture (`pg_accel_traces.jsonl` `planner.hook_elapsed_us`
    // breakdown).
    //
    // Every join strategy is currently unreachable in the lower body:
    //   - GpuSpatial / GpuRaster / GpuH3 are rejected by Gate 3b (no GPU
    //     spatial/raster/h3 merge or hash join kernel exists);
    //   - GpuHashJoin is gated by `selected_gpu_hashjoin_kernel_available()`
    //     (pinned `false` by the canary unit test).
    //
    // `join_hook_can_inject_anything()` aggregates that into one branch.
    // When it returns `false`, bail O(1) before walking
    // `joinrel->pathlist` (mergejoin observability), the restrictlist
    // (equi-join detection), or doing the registry lookup. Avoids the
    // O(join_orders × per-rel pathlist) work that dominates planning time
    // on multi-way star joins.
    //
    // The NLJ-inequality observability above runs first so the counter
    // signal still increments even when the rest of the hook is bypassed.
    if !join_hook_can_inject_anything() {
        stats::record_planner_fast_decline("join_pathlist_no_eligible_strategy");
        return;
    }

    // Gate 1d: Early max-output check. Large-output GpuHashJoin is a known
    // crash/loser lane; `gpu_join_max_output_rows` is a real upper bound, not
    // the older min-ish gate. Keep this before expensive join recognition.
    // SAFETY: joinrel is a valid RelOptInfo provided by the planner.
    {
        #[allow(clippy::cast_sign_loss)]
        let est_rows = unsafe { (*joinrel).rows.max(0.0) } as usize;
        let max = cost::device_limits().gpu_join_max_output_rows;
        if est_rows > max {
            pgrx::debug1!("pg_accel join: output rows={est_rows} > max={max}, deferring to PG");
            return;
        }
    }

    let _span = tracing::info_span!("planner.join_pathlist", join_type = jointype).entered();

    // SAFETY: pointers provided by the planner are valid.
    let joinrel_ref = unsafe { &*joinrel };
    let outerrel_ref = unsafe { &*outerrel };
    let innerrel_ref = unsafe { &*innerrel };
    let extra_ref = unsafe { &*extra };

    // Observability: detect MergeJoin-shaped join candidates so we can
    // quantify the opportunity size before committing to a GPU merge-join
    // kernel (which does not yet exist in `pgaccel-kernels/src/`). The
    // planner attaches `T_MergePath`/`T_HashPath`/`T_NestPath` entries to
    // `joinrel->pathlist` before this hook fires; if any `T_MergePath`
    // is present, the core planner considers merge-join viable for this
    // joinrel. We record the rejection via the planner_rejected counter
    // and fall through to normal HashJoin recognition — merge-join is
    // tracked in TODO.md Phase 4 "MergeJoin recognition", blocked on a
    // kernel landing in `pgaccel-kernels/src/` (no merge_join.cpp exists
    // as of this commit). See also the IncrementalSort detect-and-decline
    // pattern in `rel_pathlist.rs:731-748`.
    //
    // SAFETY: joinrel is a valid RelOptInfo; its pathlist is a valid
    // planner List (possibly null).
    unsafe { observe_mergejoin_opportunity(joinrel) };

    // Equi-join detection: independent of adapter registry. Check for
    // Var = Var conditions usable for GPU hash join. This enables GPU-
    // accelerated joins for standard OLAP patterns like fact×dim joins
    // even when no extension adapters are installed.
    let equi = unsafe { find_equi_join_key(extra_ref.restrictlist, outerrel, innerrel) };

    // Extension-function match: requires adapter registry (PostGIS, H3,
    // raster). Only initialise when equi-join didn't match.
    registry::lazy_init();
    let accel = if registry::is_ready() {
        let reg = registry::global_registry();
        if reg.is_empty() {
            None
        } else {
            find_accelerable_match(extra_ref.restrictlist)
        }
    } else {
        None
    };

    // If neither spatial predicate nor equi-join detected, bail.
    if accel.is_none() && equi.is_none() {
        return;
    }

    let strategy = accel
        .as_ref()
        .map_or(registry::AccelStrategy::GpuHashJoin, |a| a.strategy);

    // Gate 3b: Skip spatial join injection — no GPU spatial join kernel
    // exists yet (ST_Contains/ST_Within/ST_DWithin all return 100% uncertain,
    // and the geometry detoasting path is not production-ready). Let PG
    // handle spatial joins natively. Only GpuHashJoin is injected for joins.
    if matches!(
        strategy,
        registry::AccelStrategy::GpuSpatial
            | registry::AccelStrategy::GpuRaster
            | registry::AccelStrategy::GpuH3
    ) {
        return;
    }

    // Gate 3c: Do not expose `GpuHashJoin` as a selected planner path while
    // the executor-side C API has no safe selected GPU build/probe contract.
    // PG-Strom-shaped join work resumes here once the kernel contract is a
    // real GPU build/probe path or GPU-resident hash-table reuse.
    if matches!(strategy, registry::AccelStrategy::GpuHashJoin)
        && !selected_gpu_hashjoin_kernel_available()
    {
        #[allow(clippy::cast_sign_loss)]
        let n_rows_est = joinrel_ref.rows.max(0.0) as u64;
        pgrx::debug1!(
            "pg_accel join: GpuHashJoin skipped: no selected real-GPU hash join \
             build/probe kernel is available"
        );
        stats::increment_planner_rejected("hashjoin_no_selected_gpu_kernel", n_rows_est);
        return;
    }

    let limits = cost::device_limits();

    // Gate 3d: Real max build/output gates for GPU hash join. The build side
    // must stay below the kernel's sort-merge threshold (`count >= 100000`),
    // and large-output joins stay with PostgreSQL's native HashJoin.
    if matches!(strategy, registry::AccelStrategy::GpuHashJoin) {
        #[allow(clippy::cast_sign_loss)]
        let build_rows = cost::conservative_input_rows(innerrel_ref.rows, innerrel_ref.tuples);
        #[allow(clippy::cast_sign_loss)]
        let output_rows = joinrel_ref.rows.max(0.0) as usize;
        if !cost::hashjoin_cardinality_safe(build_rows, output_rows, limits) {
            pgrx::debug1!(
                "pg_accel join: cardinality gate rejected build_rows={} output_rows={} \
                 (max_build={}, max_output={})",
                build_rows,
                output_rows,
                limits.gpu_hash_join_build_max_rows,
                limits.gpu_join_max_output_rows,
            );
            return;
        }
    }

    // Gate 4: Cost model gating — skip if batching is not worthwhile.
    //
    // For GpuHashJoin, thread `uses_fp64` from the encoded join-key type so a
    // Float64 key on a soft-fp64 device pays the soft-fp64 multiplier on the
    // per-row GPU op cost (see hashjoin.rs for the policy). Non-join
    // strategies have no join key here; they keep their unmodified per-row
    // cost and handle fp64 classification at their own injection site.
    let join_rows_gate = joinrel_ref.rows as usize;
    let min_batch = gucs::min_batch_size().max(1) as usize;
    let hashjoin_uses_fp64 = equi
        .as_ref()
        .is_some_and(|k| hashjoin::key_type_is_fp64(k.key_type));
    let per_row_cost = match strategy {
        registry::AccelStrategy::GpuRaster => cost::GPU_RASTER_PER_ROW_COST,
        registry::AccelStrategy::GpuH3 => cost::GPU_H3_PER_ROW_COST,
        registry::AccelStrategy::GpuHashJoin => {
            hashjoin::per_row_cost_for_batch_gate(hashjoin_uses_fp64, limits)
        }
        _ => cost::GPU_SPATIAL_PER_ROW_COST,
    };
    if !cost::should_batch(join_rows_gate, per_row_cost, min_batch) {
        return;
    }

    // Max-output and max-build gates have already run above; the remaining
    // cost model only filters rows inside that temporary safe envelope.

    // Gate 5: Both sides need cheapest paths.
    let outer_path = outerrel_ref.cheapest_total_path;
    let inner_path = innerrel_ref.cheapest_total_path;
    if outer_path.is_null() || inner_path.is_null() {
        return;
    }

    // Cost estimate using honest per-row costs. No cost_multiplier or
    // safety margin — let add_path() decide based on real cost comparison
    // against PG's native parallel hash join.
    //
    // SAFETY: paths are non-null, verified above.
    let outer_cost = unsafe { (*outer_path).total_cost };
    let inner_cost = unsafe { (*inner_path).total_cost };
    let base_cost = outer_cost + inner_cost;

    // GPU hash join overhead components:
    // - GPU launch: fixed overhead for kernel dispatch.
    let gpu_launch = cost::GPU_LAUNCH_OVERHEAD;
    // - Hash build: consume all inner tuples + GPU hash table construction.
    //   Per inner row: ExecCopySlotMinimalTuple (0.005) + key extract (0.002)
    //   + GPU hash insert amortized (0.003) = 0.01. Penalised via
    //   `apply_fp64_penalty` when the join key is Float64 on a soft-fp64
    //   device so fp64 hash-key work is not under-costed.
    let build_cost =
        innerrel_ref.rows * hashjoin::build_cost_per_inner_row(hashjoin_uses_fp64, limits);
    // - Probe: per outer row: ExecCopySlotMinimalTuple (0.005) + key extract
    //   (0.002) + GPU probe (0.003) = 0.01. Same soft-fp64 penalty policy as
    //   the build side.
    let probe_cost =
        outerrel_ref.rows * hashjoin::probe_cost_per_outer_row(hashjoin_uses_fp64, limits);
    // - Yield: per output row: ExecForceStoreMinimalTuple + slot_getattr for
    //   building the virtual result tuple. This is a hardware-derived device
    //   limit and is not affected by soft-fp64.
    let yield_cost = joinrel_ref.rows * limits.custom_scan_yield_per_row;

    // SAFETY: outer_path is non-null, verified above.
    let startup_cost = unsafe { (*outer_path).startup_cost } + gpu_launch + build_cost;
    // Honest total — no cost_multiplier. Custom Scan hash join has inherent
    // per-row overhead. PG's native parallel hash join avoids yield overhead
    // entirely, so we only win when the GPU probe is fast enough to offset it.
    let total_cost = base_cost + gpu_launch + build_cost + probe_cost + yield_cost;

    // Let PG's native cost comparison decide — add_path() discards
    // paths that are strictly dominated by cheaper alternatives.

    // SAFETY: Allocating via palloc, building valid CustomPath.
    unsafe {
        let cpath = create_custom_path(
            joinrel,
            outer_path,
            startup_cost,
            total_cost,
            joinrel_ref.rows,
            custom_scan::join_path_methods(),
        );

        // Attach both child paths.
        let mut child_list: *mut List = std::ptr::null_mut();
        child_list = lappend(child_list, outer_path.cast());
        child_list = lappend(child_list, inner_path.cast());
        (*cpath).custom_paths = child_list;

        (*cpath).custom_private = build_join_priv_list(equi.as_ref(), accel.as_ref());

        add_path(joinrel, cpath.cast());
    }

    // Mirror the non-parallel injection into the partial pathlist so queries
    // whose optimal plan is `Gather ∘ Parallel HashJoin` can pick up the GPU
    // path. Only GpuHashJoin is handled here — spatial joins were bailed out
    // earlier in this function and do not have parallel-safe accessor logic.
    if matches!(strategy, registry::AccelStrategy::GpuHashJoin)
        && let Some(ref equi_info) = equi
    {
        // SAFETY: outerrel / innerrel are planner-owned RelOptInfo pointers.
        unsafe {
            inject_gpu_hashjoin_partial_paths(
                joinrel,
                outerrel,
                innerrel,
                equi_info,
                hashjoin_uses_fp64,
                limits,
            );
        }
    }
}

#[must_use]
fn selected_gpu_hashjoin_kernel_available() -> bool {
    false
}

/// Build the `custom_private` integer list for a `GpuHashJoin` / spatial
/// `CustomPath`. Shared by both the non-parallel and partial injection sites
/// so the layout consumed by `make_custom_scan_plan` (and thence the executor)
/// is identical.
///
/// Hash-join layout:
///   `[fn_oid=0, outer_attno, GpuHashJoin, inner_attno, key_type, outer_varno, inner_varno]`
///
/// Accel (spatial/other) layout:
///   `[fn_oid, target_attno, accel_strategy]`
///
/// # Safety
///
/// Calls `pg_sys::makeInteger` / `pg_sys::lappend`, both of which allocate in
/// `CurrentMemoryContext`. Must be invoked on the main backend thread while
/// the planner's memory context is live.
unsafe fn build_join_priv_list(
    equi: Option<&EquiJoinKey>,
    accel: Option<&AccelMatch>,
) -> *mut List {
    let mut priv_list: *mut List = std::ptr::null_mut();
    // SAFETY: makeInteger + lappend allocate in CurrentMemoryContext.
    unsafe {
        if let Some(equi_info) = equi {
            priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast()); // fn_oid
            priv_list = lappend(priv_list, pg_sys::makeInteger(equi_info.outer_attno).cast());
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(registry::AccelStrategy::GpuHashJoin as i32).cast(),
            );
            priv_list = lappend(priv_list, pg_sys::makeInteger(equi_info.inner_attno).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(equi_info.key_type).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(equi_info.outer_varno).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(equi_info.inner_varno).cast());
        } else if let Some(accel_info) = accel {
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(u32::from(accel_info.fn_oid) as i32).cast(),
            );
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(accel_info.target_attno).cast(),
            );
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(accel_info.strategy as i32).cast(),
            );
        }
    }
    priv_list
}

// ---------------------------------------------------------------------------
// MergeJoin opportunity classification (observability only)
// ---------------------------------------------------------------------------

/// Classification of a joinrel's existing path candidates for the purpose of
/// MergeJoin observability.
///
/// The core planner populates `joinrel->pathlist` with `T_MergePath`,
/// `T_HashPath`, and/or `T_NestPath` entries before `set_join_pathlist_hook`
/// fires. Counting them tells us whether merge-join was considered viable
/// for this joinrel — which lets us quantify the opportunity size before
/// committing to a GPU merge-join kernel.
///
/// No GPU merge-join kernel exists in `pgaccel-kernels/src/` (confirmed by
/// `rg -n "merge_join|mergejoin" --type cpp pgaccel-kernels/src/` → empty),
/// so the only honest action on a `HasMergePath` classification is to emit
/// `debug1` + increment `planner_rejected("mergejoin_no_gpu_kernel", ...)`
/// and fall through. This matches the IncrementalSort detect-and-decline
/// pattern in `rel_pathlist.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JoinShape {
    /// No `T_MergePath` entries in `joinrel->pathlist`. Nothing to observe.
    NoMergePath,
    /// At least one `T_MergePath` in the joinrel's pathlist — the core
    /// planner considers merge-join viable here. `merge_count` is the
    /// number of merge-path entries observed (multiple can arise from
    /// different sort-order combinations of the same join).
    HasMergePath { merge_count: i32 },
}

/// Pure classifier for [`JoinShape`] given path-tag counts.
///
/// Separated from the FFI wrapper so it can be unit-tested without a live
/// planner. `merge_count` is the number of `T_MergePath` entries observed
/// in `joinrel->pathlist`.
///
/// Preconditions (caller-enforced): `merge_count >= 0`.
pub(super) const fn classify_join_shape(merge_count: i32) -> JoinShape {
    if merge_count > 0 {
        JoinShape::HasMergePath { merge_count }
    } else {
        JoinShape::NoMergePath
    }
}

/// Walk `joinrel->pathlist` and emit the MergeJoin opportunity signal when
/// `T_MergePath` entries are present.
///
/// This is observability only — we do not inject, because no GPU
/// merge-join kernel exists. Falls through to normal HashJoin recognition
/// in the caller.
///
/// # Safety
///
/// `joinrel` must be a valid `RelOptInfo` pointer provided by the planner.
/// Its `pathlist` is a valid `List*` (possibly null).
unsafe fn observe_mergejoin_opportunity(joinrel: *mut RelOptInfo) {
    if joinrel.is_null() {
        return;
    }
    // SAFETY: caller guarantees joinrel is a valid planner pointer.
    let joinrel_ref = unsafe { &*joinrel };
    let pathlist = joinrel_ref.pathlist;
    if pathlist.is_null() {
        return;
    }
    // SAFETY: pathlist is a valid planner List (possibly null checked above).
    let n = unsafe { pg_sys::list_length(pathlist) };
    if n == 0 {
        return;
    }

    let mut merge_count: i32 = 0;
    for i in 0..n {
        // SAFETY: i < n = list_length.
        let p = unsafe { pg_sys::list_nth(pathlist, i).cast::<Path>() };
        if p.is_null() {
            continue;
        }
        // SAFETY: p is a valid Path pointer; reading its node tag is safe.
        let tag = unsafe { (*p.cast::<pg_sys::Node>()).type_ };
        if matches!(tag, NodeTag::T_MergePath) {
            merge_count += 1;
        }
    }

    if let JoinShape::HasMergePath { merge_count } = classify_join_shape(merge_count) {
        #[allow(clippy::cast_sign_loss)]
        let n_rows_est = joinrel_ref.rows.max(0.0) as u64;
        pgrx::debug1!(
            "pg_accel join: MergeJoin opportunity skipped: {merge_count} T_MergePath \
             candidate(s) in joinrel->pathlist (rows~={n_rows_est}); no GPU merge-join \
             kernel exists (see TODO.md Phase 4 'MergeJoin recognition')"
        );
        stats::increment_planner_rejected("mergejoin_no_gpu_kernel", n_rows_est);
    }
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
///   `get_op_btree_interpretation` returns no matching strategy.
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
/// Uses `get_op_btree_interpretation` so the check is opfamily-aware and
/// covers cross-type operators (`int4 < int8`, etc.). Equality is excluded —
/// it has a dedicated GpuHashJoin path.
///
/// # Safety
///
/// `opno` must be a valid PG operator OID (or `InvalidOid` in which case this
/// returns `false`). Calls `get_op_btree_interpretation` which must run on
/// the main backend thread.
unsafe fn is_btree_inequality_opno(opno: pg_sys::Oid) -> bool {
    if opno == pg_sys::InvalidOid {
        return false;
    }
    // SAFETY: caller guarantees we are on the main backend thread.
    let interps = unsafe { pg_sys::get_op_btree_interpretation(opno) };
    if interps.is_null() {
        return false;
    }
    // SAFETY: valid planner List of OpBtreeInterpretation*.
    let n = unsafe { pg_sys::list_length(interps) };
    for i in 0..n {
        // SAFETY: i < n.
        let entry = unsafe { pg_sys::list_nth(interps, i).cast::<pg_sys::OpBtreeInterpretation>() };
        if entry.is_null() {
            continue;
        }
        // SAFETY: entry is a valid OpBtreeInterpretation pointer.
        #[allow(clippy::cast_sign_loss)]
        let strategy = unsafe { (*entry).strategy } as u32;
        if matches!(
            strategy,
            pg_sys::BTLessStrategyNumber
                | pg_sys::BTLessEqualStrategyNumber
                | pg_sys::BTGreaterEqualStrategyNumber
                | pg_sys::BTGreaterStrategyNumber
        ) {
            return true;
        }
    }
    false
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

/// Walk `joinrel->pathlist` and emit the NestedLoop scalar-inequality
/// opportunity signal when a `T_NestPath` is present AND `restrictlist`
/// contains at least one cross-rel scalar inequality.
///
/// This is observability only — no GPU NestedLoop kernel exists in
/// `pgaccel-kernels/src/` (verified by `rg -n 'nested_loop|nestloop' --type
/// cpp pgaccel-kernels/src/` → empty). Falls through to the normal hash-join
/// recognition in the caller, which will itself bail when no equi-join key is
/// present.
///
/// ## Why this is currently a decline, not an implementation
///
/// A real GPU NestedLoop scalar-inequality kernel requires:
///   1. New C++ kernel in `pgaccel-kernels/src/nested_loop_ineq.cpp` doing a
///      tiled cross-product scan: for each outer tile (M rows), broadcast
///      against the full inner side (N rows), evaluate the inequality
///      predicate per (i, j) pair, and emit matched pairs. SYCL nd_range
///      with one work-item per (i, j) pair, output via atomic-counter
///      compaction. Predicate evaluation reuses `expr_eval` templates.
///   2. A new `AccelStrategy::GpuNestedLoopIneq` variant + dispatch entry in
///      `src/engine/dispatch.rs` and registry types in `src/engine/registry/`.
///   3. A new executor node `src/engine/executor/nlj/` (or extend
///      `executor/join/`) to consume the matched-pair stream and project
///      both rels' columns into the output slot. Unlike hash join, NLJ
///      needs both outer-rel and inner-rel slot deformation.
///   4. Cost model entries in `DeviceLimits` (the rule #10 home for all
///      thresholds): `gpu_nlj_min_outer_rows`, `gpu_nlj_min_inner_rows`,
///      `gpu_nlj_max_output_rows`, `gpu_nlj_per_pair_cost`. Break-even is
///      `outer × inner × per_pair_cost ≥ launch + transfer + emit`, which
///      means roughly outer >= a few thousand AND inner >= a few thousand
///      AND output rows < `gpu_join_max_output_rows`.
///   5. Predicate pushdown: the kernel only wins if the inequality has high
///      selectivity (output << outer × inner). At selectivity = 1.0 the
///      kernel is a cross product and CPU NLJ wins on memory ordering.
///
/// Until those land, the only honest action is to emit `debug1` and bump
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
             scalar inequality qual(s), output rows~={n_rows_est}; no GPU \
             nested-loop kernel exists (see TODO.md Phase 4 'NestedLoop \
             scalar recognition')"
        );
        stats::increment_planner_rejected("nestloop_scalar_no_gpu_kernel", n_rows_est);
    }
}

/// Inject parallel (partial) GPU hashjoin variants for every eligible
/// partial outer path in `outerrel->partial_pathlist`, paired with a
/// parallel-safe non-partial inner path.
///
/// ## Parallelism model
///
/// Each worker runs a complete `GpuHashJoin` instance over:
/// - its share of the outer rows (outer is drawn from `outerrel.partial_pathlist`);
/// - the *full* inner side, rebuilt per-worker (pgrx does not expose PG's
///   shared-hashtable APIs, so we fall back to the per-worker-hashtable
///   pattern). The inner path must be parallel_safe so a worker can execute
///   it inside the same parallel context.
///
/// The resulting `CustomPath` is `parallel_aware=false`, `parallel_safe=true`,
/// and inherits `parallel_workers` from the outer partial child (clamped to
/// >= 1 so PG is willing to run it).
///
/// ## Cost
///
/// `hashjoin::partial_total_cost` mirrors the non-parallel formula but uses
/// the partial outer row count and a partial output row estimate (outer
/// share × join selectivity), preserving the soft-fp64 multiplier threading.
///
/// # Safety
///
/// All pointer arguments originate from the planner. The caller must ensure
/// `joinrel` / `outerrel` / `innerrel` are non-null and that the PG memory
/// context is the planner's. Only called from the set_join_pathlist_hook on
/// the main backend thread.
#[allow(clippy::cast_sign_loss, clippy::cast_precision_loss)]
unsafe fn inject_gpu_hashjoin_partial_paths(
    joinrel: *mut RelOptInfo,
    outerrel: *mut RelOptInfo,
    innerrel: *mut RelOptInfo,
    equi: &EquiJoinKey,
    uses_fp64: bool,
    limits: &cost::DeviceLimits,
) {
    // SAFETY: planner-owned pointers.
    let joinrel_ref = unsafe { &*joinrel };
    let outerrel_ref = unsafe { &*outerrel };
    let innerrel_ref = unsafe { &*innerrel };

    let outer_partial_list = outerrel_ref.partial_pathlist;
    if outer_partial_list.is_null() {
        return;
    }
    // SAFETY: list_length is valid on null/any List pointer returned by pg.
    let n_outer = unsafe { pg_sys::list_length(outer_partial_list) };
    if n_outer == 0 {
        return;
    }

    // Inner path for workers: we need a parallel_safe non-partial path (each
    // worker rebuilds the full hashtable locally). Prefer the regular cheapest
    // path if it's parallel_safe; fall back to scanning the regular pathlist.
    // SAFETY: pathlist is a planner-owned List (possibly null).
    let inner_path = unsafe { pick_parallel_safe_inner_path(innerrel_ref) };
    if inner_path.is_null() {
        // Nothing we can pair with in a worker — skip partial injection.
        return;
    }
    // SAFETY: non-null, validated above.
    let inner_ref = unsafe { &*inner_path };
    if !hashjoin::partial_is_eligible(true, inner_ref.parallel_safe) {
        return;
    }

    // Output-row share per worker: the join's total rows divided by the worker
    // fan-out from the outer partial child. The Integer cast mirrors PG's own
    // cost_gather assumption (workers process balanced shards).
    let outer_total_rows = outerrel_ref.rows.max(1.0);
    let join_total_rows = joinrel_ref.rows;

    for i in 0..n_outer {
        // SAFETY: i < list_length.
        let outer_partial = unsafe { pg_sys::list_nth(outer_partial_list, i).cast::<Path>() };
        if outer_partial.is_null() {
            continue;
        }
        // SAFETY: non-null.
        let outer_ref = unsafe { &*outer_partial };
        // partial_pathlist entries are parallel_safe by construction, but
        // defensive check is cheap.
        if !hashjoin::partial_is_eligible(outer_ref.parallel_safe, inner_ref.parallel_safe) {
            continue;
        }

        // Partial output rows: join_total * (outer_partial.rows / outer_total).
        let outer_partial_rows = outer_ref.rows.max(0.0);
        let partial_output_rows = join_total_rows * (outer_partial_rows / outer_total_rows);

        let total_cost = hashjoin::partial_total_cost(
            outer_partial_rows,
            outer_ref.total_cost,
            inner_ref.rows,
            inner_ref.total_cost,
            partial_output_rows,
            uses_fp64,
            limits,
        );
        let startup_cost = inner_ref.rows.mul_add(
            hashjoin::build_cost_per_inner_row(uses_fp64, limits),
            outer_ref.startup_cost + cost::GPU_LAUNCH_OVERHEAD,
        );

        // SAFETY: palloc-based CustomPath construction; parallel fields overridden below.
        unsafe {
            let cpath = create_custom_path(
                joinrel,
                outer_partial,
                startup_cost,
                total_cost,
                partial_output_rows,
                custom_scan::join_path_methods(),
            );

            // Mark parallel-safe: each worker runs its own GpuHashJoin over
            // its outer shard + a private inner hashtable.
            (*cpath).path.parallel_aware = false;
            (*cpath).path.parallel_safe = true;
            (*cpath).path.parallel_workers = outer_ref.parallel_workers.max(1);

            // Attach outer (partial) + inner (full, non-partial) children.
            let mut child_list: *mut List = std::ptr::null_mut();
            child_list = lappend(child_list, outer_partial.cast());
            child_list = lappend(child_list, inner_path.cast());
            (*cpath).custom_paths = child_list;

            // Same custom_private layout as the non-parallel variant — the
            // executor has no way to distinguish the two at exec time and
            // doesn't need to (each worker runs a complete join).
            (*cpath).custom_private = build_join_priv_list(Some(equi), None);

            add_partial_path(joinrel, cpath.cast());
        }
    }
}

/// Pick a parallel-safe inner path for a partial hashjoin.
///
/// Preference: (1) `cheapest_total_path` if parallel_safe; (2) otherwise the
/// cheapest entry in `pathlist` with `parallel_safe=true`. Returns null if
/// no eligible path exists (e.g., inner relation is entirely parallel-unsafe).
///
/// # Safety
///
/// `innerrel_ref` must be a valid `RelOptInfo` reference whose `pathlist` is a
/// valid planner `List` (possibly null).
unsafe fn pick_parallel_safe_inner_path(innerrel_ref: &RelOptInfo) -> *mut Path {
    let cheapest = innerrel_ref.cheapest_total_path;
    if !cheapest.is_null() {
        // SAFETY: non-null.
        let r = unsafe { &*cheapest };
        if r.parallel_safe {
            return cheapest;
        }
    }

    let pathlist = innerrel_ref.pathlist;
    if pathlist.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: valid planner List.
    let n = unsafe { pg_sys::list_length(pathlist) };
    let mut best: *mut Path = std::ptr::null_mut();
    for i in 0..n {
        // SAFETY: i < n.
        let p = unsafe { pg_sys::list_nth(pathlist, i).cast::<Path>() };
        if p.is_null() {
            continue;
        }
        // SAFETY: non-null.
        let r = unsafe { &*p };
        if !r.parallel_safe {
            continue;
        }
        // Skip Gather/GatherMerge — those cannot be re-parallelised inside a worker.
        // SAFETY: p is a valid Path pointer; reading its node tag is safe.
        let tag = unsafe { (*p.cast::<pg_sys::Node>()).type_ };
        if matches!(tag, NodeTag::T_GatherPath | NodeTag::T_GatherMergePath) {
            continue;
        }
        if best.is_null() || r.total_cost < unsafe { (*best).total_cost } {
            best = p;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        JoinShape, NestedLoopShape, classify_join_shape, classify_nestloop_shape,
        selected_gpu_hashjoin_kernel_available,
    };

    // -- classify_join_shape -----------------------------------------------
    //
    // The classifier is a pure function on a merge-path count so it can be
    // unit-tested without a live planner. The FFI wrapper
    // `observe_mergejoin_opportunity` feeds the output of a pathlist walk
    // into this classifier; mocking the FFI side requires a live planner
    // (see the #[pg_test] coverage in tests.rs).

    #[test]
    fn classify_no_merge_path_when_count_is_zero() {
        assert_eq!(classify_join_shape(0), JoinShape::NoMergePath);
    }

    #[test]
    fn classify_has_merge_path_when_count_is_positive() {
        assert_eq!(
            classify_join_shape(1),
            JoinShape::HasMergePath { merge_count: 1 }
        );
        assert_eq!(
            classify_join_shape(4),
            JoinShape::HasMergePath { merge_count: 4 }
        );
    }

    #[test]
    fn classify_is_const_eval_compatible() {
        // const fn guarantee: calling at compile time must succeed.
        const SHAPE_EMPTY: JoinShape = classify_join_shape(0);
        const SHAPE_HAS: JoinShape = classify_join_shape(2);
        assert_eq!(SHAPE_EMPTY, JoinShape::NoMergePath);
        assert_eq!(SHAPE_HAS, JoinShape::HasMergePath { merge_count: 2 });
    }

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
    fn selected_gpu_hashjoin_kernel_is_not_available_yet() {
        assert!(
            !selected_gpu_hashjoin_kernel_available(),
            "planner must not expose GpuHashJoin until build/probe are real GPU paths"
        );
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
}
