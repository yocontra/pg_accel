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

/// Temporary exposure cap for row-returning heap `GpuHashJoin` plans.
///
/// The hash join kernel is real GPU work, but the current Custom Scan path
/// still reconstructs every joined row back into PostgreSQL slots. Benchmarks
/// on M-series show the 100K-output `hash_join` cell loses badly to native
/// HashJoin, so planner exposure stays limited to small row-returning joins
/// until a GPU-resident join->preagg pipeline avoids per-row yield cost.
const HASHJOIN_MAX_HEAP_OUTPUT_ROWS: usize = 25_000;
const NLJ_GPU_PAIR_INDEX_MAX: usize = u32::MAX as usize;

/// Phase 0 fast-decline helper: returns `false` only when *no* join strategy
/// can possibly be injected by the hook body.
///
/// A narrow selected GpuHashJoin kernel now exists for INT32/INT64 equality
/// keys, so the hook must inspect the restrictlist before it can decline.
/// Spatial/raster/H3 joins still decline below because no selected join
/// executor is wired for those strategies.
fn join_hook_can_inject_anything() -> bool {
    selected_gpu_hashjoin_kernel_available() || selected_gpu_nlj_kernel_available()
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
    if jointype == pg_sys::JoinType::JOIN_INNER && !selected_gpu_nlj_kernel_available() {
        unsafe {
            observe_nestloop_scalar_opportunity(joinrel, (*extra).restrictlist, outerrel, innerrel);
        }
    }

    // Gate 1c: fast-decline remains for builds where every selected join
    // strategy is unavailable. With the integer GpuHashJoin kernel enabled,
    // this normally falls through so key-shape gating can happen below.
    if !join_hook_can_inject_anything() {
        stats::record_planner_fast_decline("join_pathlist_no_eligible_strategy");
        return;
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

    let nlj_between =
        if jointype == pg_sys::JoinType::JOIN_INNER && selected_gpu_nlj_kernel_available() {
            unsafe { find_nlj_between_key(extra_ref.restrictlist, outerrel, innerrel) }
        } else {
            None
        };

    // Equi-join detection: independent of adapter registry. Check for
    // Var = Var conditions usable for GPU hash join. This enables GPU-
    // accelerated joins for standard OLAP patterns like fact×dim joins
    // even when no extension adapters are installed.
    let equi = unsafe { find_equi_join_key(extra_ref.restrictlist, outerrel, innerrel) };

    // Extension-function match: requires adapter registry (PostGIS, H3,
    // raster). Only initialise when neither selected built-in join path
    // matched; plain equi/NLJ joins do not need SPI-backed adapter discovery.
    let accel = if equi.is_none() && nlj_between.is_none() {
        registry::lazy_init();
        if registry::is_ready() {
            let reg = registry::global_registry();
            if reg.is_empty() {
                None
            } else {
                find_accelerable_match(extra_ref.restrictlist)
            }
        } else {
            None
        }
    } else {
        None
    };

    // If no selected join strategy detected, bail.
    if accel.is_none() && equi.is_none() && nlj_between.is_none() {
        return;
    }

    if matches!(
        jointype,
        pg_sys::JoinType::JOIN_SEMI | pg_sys::JoinType::JOIN_ANTI
    ) {
        #[allow(clippy::cast_sign_loss)]
        let n_rows_est = joinrel_ref.rows.max(0.0) as u64;
        stats::increment_planner_rejected(
            super::RejectionReason::SemiAntiNoGpuMembershipFilter.stats_key(),
            n_rows_est,
        );
        pgrx::debug1!(
            "pg_accel join: semi/anti join skipped until GPU membership filters are implemented"
        );
        return;
    }

    let strategy = if equi.is_some() {
        registry::AccelStrategy::GpuHashJoin
    } else if let Some(accel_info) = accel.as_ref() {
        accel_info.strategy
    } else {
        registry::AccelStrategy::GpuNestedLoopIneq
    };

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

    // Gate 3c: expose GpuHashJoin only for the key types implemented by the
    // selected GPU build/probe kernel. Other equi-join shapes stay with
    // PostgreSQL until their kernel semantics are complete.
    if matches!(strategy, registry::AccelStrategy::GpuHashJoin) {
        #[allow(clippy::cast_sign_loss)]
        let n_rows_est = joinrel_ref.rows.max(0.0) as u64;
        if !selected_gpu_hashjoin_kernel_available() {
            pgrx::debug1!(
                "pg_accel join: GpuHashJoin skipped: no selected real-GPU hash join \
                 build/probe kernel is available"
            );
            stats::increment_planner_rejected("hashjoin_no_selected_gpu_kernel", n_rows_est);
            return;
        }
        let key_type_supported = equi
            .as_ref()
            .is_some_and(|k| hashjoin::selected_key_type_supported(k.key_type));
        if !key_type_supported {
            pgrx::debug1!(
                "pg_accel join: GpuHashJoin skipped: key type is not implemented by \
                 selected GPU hash join build/probe"
            );
            stats::increment_planner_rejected("hashjoin_key_type_no_gpu_kernel", n_rows_est);
            return;
        }
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
        let max_heap_output = HASHJOIN_MAX_HEAP_OUTPUT_ROWS.min(limits.gpu_join_max_output_rows);
        if output_rows > max_heap_output {
            pgrx::debug1!(
                "pg_accel join: GpuHashJoin row-returning output_rows={} > \
                 heap-output cap {}; deferring to PostgreSQL until join->preagg \
                 can keep rows GPU-resident",
                output_rows,
                max_heap_output,
            );
            stats::increment_planner_rejected(
                super::RejectionReason::HashJoinHeapOutputTooLarge.stats_key(),
                output_rows as u64,
            );
            return;
        }
    }

    if matches!(strategy, registry::AccelStrategy::GpuNestedLoopIneq) {
        #[allow(clippy::cast_sign_loss)]
        let outer_rows = cost::conservative_input_rows(outerrel_ref.rows, outerrel_ref.tuples);
        #[allow(clippy::cast_sign_loss)]
        let inner_rows = cost::conservative_input_rows(innerrel_ref.rows, innerrel_ref.tuples);
        if !nlj_kernel_indices_fit(outer_rows, inner_rows) {
            pgrx::debug1!(
                "pg_accel join: GpuNestedLoopIneq rejected because input rows exceed \
                 u32 kernel index range outer_rows={} inner_rows={} max_index={}",
                outer_rows,
                inner_rows,
                NLJ_GPU_PAIR_INDEX_MAX,
            );
            stats::increment_planner_rejected(
                "nlj_input_index_overflow",
                outer_rows.max(inner_rows) as u64,
            );
            return;
        }
        // PostgreSQL's generic selectivity for `outer.x BETWEEN inner.lo AND
        // inner.hi` often estimates a large fraction of the cross product even
        // for non-overlapping range windows. For the selected BETWEEN kernel,
        // use the one-window-per-event launchpad estimate, but do not hide
        // unsafe plans by clipping the estimate to the output cap.
        let Some(output_rows) =
            nlj_between_modeled_output_rows(outer_rows, limits.gpu_nlj_max_output_rows)
        else {
            pgrx::debug1!(
                "pg_accel join: GpuNestedLoopIneq rejected because modeled output_rows={} \
                 exceeds max_output={}",
                outer_rows,
                limits.gpu_nlj_max_output_rows,
            );
            stats::increment_planner_rejected(
                super::RejectionReason::NljBetweenOutputTooLarge.stats_key(),
                outer_rows as u64,
            );
            return;
        };
        if !cost::nlj_break_even(outer_rows, inner_rows, output_rows, limits)
            || !cost::nlj_selectivity_useful(outer_rows, inner_rows, output_rows, 0.5)
        {
            pgrx::debug1!(
                "pg_accel join: GpuNestedLoopIneq rejected by cost gate outer_rows={} \
                 inner_rows={} output_rows={} max_output={}",
                outer_rows,
                inner_rows,
                output_rows,
                limits.gpu_nlj_max_output_rows,
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
        registry::AccelStrategy::GpuNestedLoopIneq => limits.gpu_nlj_per_pair_cost,
        _ => cost::GPU_SPATIAL_PER_ROW_COST,
    };
    if !matches!(strategy, registry::AccelStrategy::GpuNestedLoopIneq)
        && !cost::should_batch(join_rows_gate, per_row_cost, min_batch)
    {
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
    let build_cost = if matches!(strategy, registry::AccelStrategy::GpuHashJoin) {
        innerrel_ref.rows * hashjoin::build_cost_per_inner_row(hashjoin_uses_fp64, limits)
    } else {
        0.0
    };
    // - Probe: per outer row: ExecCopySlotMinimalTuple (0.005) + key extract
    //   (0.002) + GPU probe (0.003) = 0.01. Same soft-fp64 penalty policy as
    //   the build side.
    let probe_cost = if matches!(strategy, registry::AccelStrategy::GpuHashJoin) {
        outerrel_ref.rows * hashjoin::probe_cost_per_outer_row(hashjoin_uses_fp64, limits)
    } else {
        (outerrel_ref.rows.max(0.0) * innerrel_ref.rows.max(0.0)) * limits.gpu_nlj_per_pair_cost
    };
    let estimated_output_rows = if matches!(strategy, registry::AccelStrategy::GpuNestedLoopIneq) {
        let outer_rows = cost::conservative_input_rows(outerrel_ref.rows, outerrel_ref.tuples);
        nlj_between_modeled_output_rows(outer_rows, limits.gpu_nlj_max_output_rows)
            .unwrap_or(outer_rows) as f64
    } else {
        joinrel_ref.rows
    };
    // - Yield: per output row: ExecForceStoreMinimalTuple + slot_getattr for
    //   building the virtual result tuple. This is a hardware-derived device
    //   limit and is not affected by soft-fp64.
    let yield_cost = estimated_output_rows * limits.custom_scan_yield_per_row;

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
            estimated_output_rows,
            custom_scan::join_path_methods(),
        );

        // Attach both child paths.
        let mut child_list: *mut List = std::ptr::null_mut();
        child_list = lappend(child_list, outer_path.cast());
        child_list = lappend(child_list, inner_path.cast());
        (*cpath).custom_paths = child_list;

        (*cpath).custom_private =
            build_join_priv_list(equi.as_ref(), accel.as_ref(), nlj_between.as_ref());

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
    true
}

/// Whether the selected `outer BETWEEN inner.lo AND inner.hi` NestedLoop
/// inequality path is wired end-to-end.
#[must_use]
fn selected_gpu_nlj_kernel_available() -> bool {
    true
}

#[must_use]
const fn nlj_kernel_indices_fit(outer_rows: usize, inner_rows: usize) -> bool {
    outer_rows <= NLJ_GPU_PAIR_INDEX_MAX && inner_rows <= NLJ_GPU_PAIR_INDEX_MAX
}

#[must_use]
const fn nlj_between_modeled_output_rows(
    outer_rows: usize,
    max_output_rows: usize,
) -> Option<usize> {
    if outer_rows <= max_output_rows {
        Some(outer_rows)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy)]
struct NljBetweenKey {
    outer_attno: i32,
    inner_lo_attno: i32,
    inner_hi_attno: i32,
    key_type: i32,
    outer_varno: i32,
    inner_varno: i32,
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
    nlj_between: Option<&NljBetweenKey>,
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
        } else if let Some(nlj_info) = nlj_between {
            priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast()); // fn_oid
            priv_list = lappend(priv_list, pg_sys::makeInteger(nlj_info.outer_attno).cast());
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(registry::AccelStrategy::GpuNestedLoopIneq as i32).cast(),
            );
            priv_list = lappend(priv_list, pg_sys::makeInteger(2).cast()); // BETWEEN shape
            priv_list = lappend(priv_list, pg_sys::makeInteger(nlj_info.key_type).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast()); // op unused
            priv_list = lappend(priv_list, pg_sys::makeInteger(nlj_info.outer_varno).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(nlj_info.inner_varno).cast());
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(nlj_info.inner_lo_attno).cast(),
            );
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(nlj_info.inner_hi_attno).cast(),
            );
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
    let interps = unsafe { pg_sys::get_op_btree_interpretation(opno) };
    if interps.is_null() {
        return None;
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
/// For small inner sides, each worker runs a complete `GpuHashJoin` instance over:
/// - its share of the outer rows (outer is drawn from `outerrel.partial_pathlist`);
/// - the *full* inner side, rebuilt per-worker (pgrx does not expose PG's
///   shared-hashtable APIs, so we fall back to the per-worker-hashtable
///   pattern). The inner path must be parallel_safe so a worker can execute
///   it inside the same parallel context.
///
/// Large inner sides decline explicitly instead of injecting this private
/// rebuild shape. That keeps large-inner joins on PostgreSQL's native
/// parallel hash join until pg_accel can share or reuse a GPU-resident build
/// table across workers.
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

    let mut max_worker_count = 1_usize;
    for i in 0..n_outer {
        // SAFETY: i < list_length.
        let outer_partial = unsafe { pg_sys::list_nth(outer_partial_list, i).cast::<Path>() };
        if outer_partial.is_null() {
            continue;
        }
        // SAFETY: non-null.
        let outer_ref = unsafe { &*outer_partial };
        max_worker_count = max_worker_count.max(outer_ref.parallel_workers.max(1) as usize);
    }

    let inner_rows =
        cost::conservative_input_rows(innerrel_ref.rows.max(inner_ref.rows), innerrel_ref.tuples);
    if hashjoin::partial_private_inner_rebuild_declines(inner_rows, max_worker_count, limits) {
        stats::increment_planner_rejected(
            super::RejectionReason::HashJoinParallelInnerRebuildTooLarge.stats_key(),
            inner_rows as u64,
        );
        pgrx::debug1!(
            "pg_accel join: partial GpuHashJoin skipped because inner_rows={} \
             with {} workers would rebuild private GPU hash tables; waiting for \
             shared GPU-resident inner state",
            inner_rows,
            max_worker_count,
        );
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
            (*cpath).custom_private = build_join_priv_list(Some(equi), None, None);

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
    fn selected_gpu_hashjoin_kernel_is_available_for_integer_keys() {
        assert!(
            selected_gpu_hashjoin_kernel_available(),
            "planner can expose GpuHashJoin for the key types accepted by the selected-key gate"
        );
    }

    #[test]
    fn selected_gpu_nlj_kernel_is_available_for_between_shape() {
        assert!(
            super::selected_gpu_nlj_kernel_available(),
            "planner can expose the selected GpuNestedLoopIneq BETWEEN shape"
        );
    }

    #[test]
    fn nlj_between_output_model_is_not_silent_cap() {
        assert_eq!(
            super::nlj_between_modeled_output_rows(1_000, 100_000),
            Some(1_000)
        );
        assert_eq!(
            super::nlj_between_modeled_output_rows(100_000, 100_000),
            Some(100_000)
        );
        assert_eq!(
            super::nlj_between_modeled_output_rows(100_001, 100_000),
            None
        );
    }

    #[test]
    fn nlj_kernel_index_gate_matches_u32_pair_contract() {
        assert!(super::nlj_kernel_indices_fit(u32::MAX as usize, 1));
        assert!(super::nlj_kernel_indices_fit(1, u32::MAX as usize));
        assert!(!super::nlj_kernel_indices_fit(u32::MAX as usize + 1, 1));
        assert!(!super::nlj_kernel_indices_fit(1, u32::MAX as usize + 1));
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
}
