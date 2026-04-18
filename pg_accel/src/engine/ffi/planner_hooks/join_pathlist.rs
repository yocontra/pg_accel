//! `set_join_pathlist_hook` — injects `CustomPath`s for accelerable joins.

use pgrx::pg_sys::{
    self, CustomPath, JoinPathExtraData, List, NodeTag, Path, PlannerInfo, RelOptInfo,
    RestrictInfo, add_path, lappend,
};

use super::super::custom_scan;
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

    // Equi-join detection: independent of adapter registry. Check for
    // Var = Var conditions usable for GPU hash join. This enables GPU-
    // accelerated joins for standard OLAP patterns like fact×dim joins
    // even when no extension adapters are installed.
    #[cfg(feature = "gpu")]
    let equi = unsafe { find_equi_join_key(extra_ref.restrictlist, outerrel, innerrel) };

    #[cfg(not(feature = "gpu"))]
    let equi: Option<EquiJoinKey> = None;

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
    let join_rows_gate = joinrel_ref.rows as usize;
    let min_batch = gucs::min_batch_size().max(1) as usize;
    let per_row_cost = match strategy {
        registry::AccelStrategy::GpuRaster => cost::GPU_RASTER_PER_ROW_COST,
        registry::AccelStrategy::GpuH3 => cost::GPU_H3_PER_ROW_COST,
        registry::AccelStrategy::GpuHashJoin => cost::GPU_HASH_JOIN_PER_ROW_COST,
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
    //   + GPU hash insert amortized (0.003) = 0.01.
    let build_cost = innerrel_ref.rows * 0.01;
    // - Probe: per outer row: ExecCopySlotMinimalTuple (0.005) + key extract
    //   (0.002) + GPU probe (0.003) = 0.01.
    let probe_cost = outerrel_ref.rows * 0.01;
    // - Yield: per output row: ExecForceStoreMinimalTuple + slot_getattr for
    //   building virtual result tuple (~3μs/row ≈ 0.03 cost units).
    let yield_cost = joinrel_ref.rows * 0.03;

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

        // Serialize path custom_private.
        // SAFETY: makeInteger + lappend allocate in CurrentMemoryContext.
        let mut priv_list: *mut List = std::ptr::null_mut();

        if let Some(ref equi_info) = equi {
            // Hash join path: [fn_oid=0, outer_attno, GpuHashJoin, inner_attno, key_type, outer_varno, inner_varno]
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
        } else if let Some(ref accel_info) = accel {
            // Spatial/other accelerable path: [fn_oid, target_attno, accel_strategy]
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
        (*cpath).custom_private = priv_list;

        add_path(joinrel, cpath.cast());
    }
}
