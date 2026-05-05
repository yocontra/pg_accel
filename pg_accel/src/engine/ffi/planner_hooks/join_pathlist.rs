//! `set_join_pathlist_hook` — injects `CustomPath`s for accelerable joins.

use pgrx::pg_sys::{
    self, CustomPath, JoinPathExtraData, List, NodeTag, Path, PlannerInfo, RelOptInfo,
    RestrictInfo, add_partial_path, add_path, lappend,
};

use super::super::custom_scan;
use super::hashjoin;
use super::{
    AccelMatch, EquiJoinKey, PREV_SET_JOIN_PATHLIST_HOOK, create_custom_path,
    find_accelerable_match, find_equi_join_key,
};
use crate::engine::cost;
use crate::engine::gucs;
use crate::engine::registry;
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

    // Gate 1c: Early output-rows check. Skip all expensive work (equi-join
    // detection, registry scan, restrict-list walks) when the join output is
    // too small for any GPU path to break even. This is the hot path for
    // multi-table OLAP queries where the planner evaluates many join orderings.
    // SAFETY: joinrel is a valid RelOptInfo provided by the planner.
    {
        #[allow(clippy::cast_sign_loss)]
        let est_rows = unsafe { (*joinrel).rows } as usize;
        let min = cost::device_limits().gpu_join_max_output_rows / 2;
        if est_rows < min {
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

    // Gate 3c: Minimum output rows for GPU hash join. Custom Scan yield
    // overhead (~3µs/row ≈ 0.03 cost units) makes small-output joins
    // strictly slower than PG's native HashJoin. Require at least
    // gpu_join_max_output_rows / 2 output rows for GPU to have any chance.
    if matches!(strategy, registry::AccelStrategy::GpuHashJoin) {
        let min_output = cost::device_limits().gpu_join_max_output_rows / 2;
        #[allow(clippy::cast_sign_loss)]
        let output_rows = joinrel_ref.rows as usize;
        if output_rows < min_output {
            pgrx::debug1!(
                "pg_accel join: output rows={output_rows} < min={min_output}, deferring to PG"
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
    let limits = cost::device_limits();
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

    // No max-output or max-build gates: hash table is CPU-side malloc,
    // and GPU probe handles arbitrary row counts. The cost model
    // (should_batch above) already filters unprofitable small joins.

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
    // - Yield: per output row: ExecForceStoreMinimalTuple + slot_getattr
    //   for building the virtual result tuple. Phase 6 calibration: reads
    //   `limits.custom_scan_yield_per_row` (hardware-derived; ~0.0005 / row
    //   on unified-memory M-series, 0.001 / row on discrete GPU). The
    //   previous calibration to `CUSTOM_SCAN_YIELD_COST = 0.01 / row`
    //   matched PG's `cpu_tuple_cost` default but was double-counting work
    //   that PG's stock HashJoin already amortises into the parent
    //   operator's `cpu_tuple_cost`; for a 10M-output JOIN that 0.01 / row
    //   added 100K cost units that, combined with build + probe at another
    //   100K, pushed pgaccel's path strictly above PG's stock `Parallel
    //   Hash Join` (~76K). Pure CPU-side tuple materialization — not
    //   affected by soft-fp64.
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
    use super::{JoinShape, classify_join_shape};

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
}
