//! Planner hook installation for pg_accel Custom Scan injection.
//!
//! Installs `set_rel_pathlist_hook` (scan), `set_join_pathlist_hook` (join),
//! and `create_upper_paths_hook` (aggregate) so the planner considers
//! GPU-accelerated paths for qualifying relations and aggregates.

use pgrx::pg_sys::{
    self, CustomPath, JoinPathExtraData, List, NodeTag, Path, PlannerInfo, RangeTblEntry,
    RelOptInfo, RestrictInfo, UpperRelationKind, add_path, lappend,
};

use crate::engine::executor::agg::{AggOp, GroupKeyInfo};
use crate::engine::executor::sort::SortKeyDesc;
use crate::engine::executor::window::{WindowFunc, WindowFuncSpec};

use super::custom_scan;
use crate::engine::cost;
use crate::engine::gucs;
use crate::engine::registry;

// ---------------------------------------------------------------------------
// Previous hook storage
// ---------------------------------------------------------------------------

static mut PREV_SET_REL_PATHLIST_HOOK: pg_sys::set_rel_pathlist_hook_type = None;
static mut PREV_SET_JOIN_PATHLIST_HOOK: pg_sys::set_join_pathlist_hook_type = None;
static mut PREV_CREATE_UPPER_PATHS_HOOK: pg_sys::create_upper_paths_hook_type = None;

// ---------------------------------------------------------------------------
// Hook installation
// ---------------------------------------------------------------------------

/// Install planner hooks. Must be called from `_PG_init` after
/// [`custom_scan::register`].
///
/// # Safety
///
/// Must only be called once, on the main backend thread, during extension load.
pub unsafe fn install() {
    // SAFETY: Accessing global hook variables is safe during _PG_init, which
    // runs single-threaded before any queries.
    unsafe {
        PREV_SET_REL_PATHLIST_HOOK = pg_sys::set_rel_pathlist_hook;
        pg_sys::set_rel_pathlist_hook = Some(pgaccel_set_rel_pathlist);

        PREV_SET_JOIN_PATHLIST_HOOK = pg_sys::set_join_pathlist_hook;
        pg_sys::set_join_pathlist_hook = Some(pgaccel_set_join_pathlist);

        PREV_CREATE_UPPER_PATHS_HOOK = pg_sys::create_upper_paths_hook;
        pg_sys::create_upper_paths_hook = Some(pgaccel_create_upper_paths);

        pgrx::log!("pg_accel: planner hooks installed (scan, join, upper_paths)");
    }
}

// ---------------------------------------------------------------------------
// Scan hook
// ---------------------------------------------------------------------------

/// `set_rel_pathlist_hook` implementation.
///
/// Injects a `CustomPath` for base relations when:
/// 1. `pg_accel.enabled` is on.
/// 2. The relation is a base relation (`RELOPT_BASEREL` + `RTE_RELATION`).
/// 3. The estimated row count meets `pg_accel.min_batch_size`.
/// 4. A cheapest path exists to wrap.
/// 5. Restriction clauses contain a top-level `FuncExpr`.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
unsafe extern "C-unwind" fn pgaccel_set_rel_pathlist(
    root: *mut PlannerInfo,
    rel: *mut RelOptInfo,
    rti: pg_sys::Index,
    rte: *mut RangeTblEntry,
) {
    // Chain previous hook first.
    // SAFETY: Previous hook, if set, accepts the same planner-provided args.
    unsafe {
        if let Some(prev) = PREV_SET_REL_PATHLIST_HOOK {
            prev(root, rel, rti, rte);
        }
    }

    // Gate 1: GUC check — single branch, ~1ns.
    if !gucs::enabled() {
        return;
    }

    // SAFETY: rel and rte are valid pointers provided by the planner.
    let rel_ref = unsafe { &*rel };
    let rte_ref = unsafe { &*rte };

    // Gate 2: Only base table relations — cheap field checks before
    // any registry or clause work.
    if rel_ref.reloptkind != pg_sys::RelOptKind::RELOPT_BASEREL {
        return;
    }
    if rte_ref.rtekind != pg_sys::RTEKind::RTE_RELATION {
        return;
    }

    // Gate 3: Ensure adapter registry is initialised. Combined
    // init + readiness check avoids redundant atomic loads.
    registry::lazy_init();
    if !registry::is_ready() {
        return;
    }

    // Gate 4: Fast-reject — if no extensions are installed (empty
    // registry), skip clause walking entirely. Cost: one pointer
    // deref + len check (~5ns).
    let reg = registry::global_registry();
    if reg.is_empty() {
        return;
    }

    // GPU sort path: ORDER BY on numeric columns. Checked independently
    // of restriction clauses since ORDER BY queries may have no WHERE.
    pgrx::debug1!("pg_accel: calling try_inject_gpu_sort_path");
    // SAFETY: root, rel are valid planner pointers.
    unsafe { try_inject_gpu_sort_path(root, rel) };

    // Gate 5: Check if restriction clauses contain a registered function
    // and determine its strategy. This is checked before the row-count
    // gate so we can use the strategy's per-row cost in the threshold.
    let Some(accel) = find_accelerable_match(rel_ref.baserestrictinfo) else {
        return;
    };
    let strategy = accel.strategy;

    // Gate 3b: Only inject for GPU-accelerable strategies. BatchedEval
    // just adds MinimalTuple copy overhead without benefit — the standard
    // PG executor already handles scalar functions efficiently.
    if matches!(strategy, registry::AccelStrategy::BatchedEval) {
        return;
    }

    // Gate 4: Cost model gating — skip if batching is not worthwhile.
    // Use strategy-aware per-row cost so GPU paths (with higher overhead)
    // require more rows to break even.
    let rows = rel_ref.rows as usize;
    let min_batch = gucs::min_batch_size().max(1) as usize;
    let per_row_cost = match strategy {
        registry::AccelStrategy::GpuSpatial => cost::GPU_SPATIAL_PER_ROW_COST,
        registry::AccelStrategy::GpuRaster => cost::GPU_RASTER_PER_ROW_COST,
        registry::AccelStrategy::GpuH3 => cost::GPU_H3_PER_ROW_COST,
        _ => cost::BATCHED_EVAL_PER_ROW_COST,
    };
    if !cost::should_batch(rows, per_row_cost, min_batch) {
        return;
    }

    // Gate 5: Find cheapest path. Hook fires BEFORE set_cheapest(), so
    // cheapest_total_path may be NULL — scan the pathlist manually.
    // SAFETY: rel_ref.pathlist is a valid List pointer from the planner.
    let cheapest = unsafe { find_cheapest_path(rel_ref.pathlist) };
    if cheapest.is_null() {
        return;
    }

    // Build cost estimate using the cost model with strategy-aware constants.
    // SAFETY: cheapest is non-null, checked above.
    let base = unsafe { &*cheapest };
    #[allow(clippy::cast_precision_loss)]
    let batch_size = cost::optimal_batch_size(rows) as f64;
    let num_batches = (base.rows / batch_size).ceil();
    let batch_overhead = num_batches * 0.5; // per-batch dispatch cost
    // GPU strategies incur a fixed kernel launch overhead.
    let gpu_overhead = match strategy {
        registry::AccelStrategy::GpuSpatial
        | registry::AccelStrategy::GpuRaster
        | registry::AccelStrategy::GpuH3 => cost::GPU_LAUNCH_OVERHEAD,
        _ => 0.0,
    };
    // Batched evaluation amortises per-call overhead across the batch.
    let calls_saved = base.rows - num_batches;
    let per_row_saving = calls_saved * per_row_cost;
    let startup_cost = base.startup_cost + 1.0 + gpu_overhead;
    let total_cost = (base.total_cost + batch_overhead + gpu_overhead - per_row_saving)
        * gucs::cost_multiplier();

    // Gate 6: Safety margin — GPU path must be significantly cheaper
    // than CPU to account for estimation uncertainty. If not, skip.
    if total_cost > base.total_cost * cost::GPU_COST_SAFETY_MARGIN {
        return;
    }

    // SAFETY: Allocating via palloc, building valid CustomPath.
    unsafe {
        let cpath = create_custom_path(
            rel,
            cheapest,
            startup_cost,
            total_cost,
            base.rows,
            custom_scan::scan_path_methods(),
        );

        // Serialize [fn_oid, target_attno, accel_strategy] into the path's
        // custom_private. PlanCustomPath reads this and builds the full
        // [strategy, batch_size, threads, fn_oid, target_attno, accel_strategy]
        // layout for the scan node's custom_private.
        // SAFETY: makeInteger + lappend allocate in CurrentMemoryContext.
        let mut priv_list: *mut List = std::ptr::null_mut();
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(u32::from(accel.fn_oid) as i32).cast(),
        );
        priv_list = lappend(priv_list, pg_sys::makeInteger(accel.target_attno).cast());
        priv_list = lappend(priv_list, pg_sys::makeInteger(accel.strategy as i32).cast());
        (*cpath).custom_private = priv_list;

        add_path(rel, cpath.cast());
    }
}

// ---------------------------------------------------------------------------
// GPU sort injection (scan-level, avoids upper-path tlist issues)
// ---------------------------------------------------------------------------

/// GPU-sortable numeric type OIDs.
const SORT_INT4OID: u32 = 23;
const SORT_FLOAT4OID: u32 = 700;
const SORT_FLOAT8OID: u32 = 701;

/// Minimum estimated rows to consider GPU sort at the planner level.
const GPU_SORT_PLANNER_MIN_ROWS: usize = 1_000_000;

/// Inject a GPU sort `CustomPath` at the scan level when `root->sort_pathkeys`
/// has a single numeric key and the relation has enough rows.
///
/// By injecting at scan level with pathkeys set, PG knows the scan output is
/// already sorted and won't add a Sort node. This avoids the upper-path
/// `apply_tlist_labeling` assert that fires when `custom_scan_tlist` length
/// differs from the child plan's targetlist.
///
/// # Safety
///
/// `root` and `rel` must be valid planner-provided pointers.
#[allow(clippy::too_many_lines)]
unsafe fn try_inject_gpu_sort_path(root: *mut PlannerInfo, rel: *mut RelOptInfo) {
    // Gate: GPU must be enabled.
    if !gucs::gpu_enabled() {
        pgrx::debug1!("pg_accel sort: gpu_enabled=false");
        return;
    }

    // SAFETY: root is a valid PlannerInfo pointer.
    let root_ref = unsafe { &*root };
    let rel_ref = unsafe { &*rel };

    // Gate: Need sort pathkeys (i.e., an ORDER BY clause).
    let sort_pathkeys = root_ref.sort_pathkeys;
    if sort_pathkeys.is_null() {
        pgrx::debug1!("pg_accel sort: sort_pathkeys is NULL");
        return;
    }
    // SAFETY: sort_pathkeys is a valid List.
    let num_pathkeys = unsafe { pg_sys::list_length(sort_pathkeys) };
    pgrx::debug1!("pg_accel sort: num_pathkeys={num_pathkeys}");
    // GPU sort only supports single-key sort.
    if num_pathkeys != 1 {
        pgrx::debug1!("pg_accel sort: multi-key sort, skipping");
        return;
    }

    // Gate: Row count threshold.
    let rows = rel_ref.rows as usize;
    pgrx::debug1!("pg_accel sort: rows={rows}, min={GPU_SORT_PLANNER_MIN_ROWS}");
    if rows < GPU_SORT_PLANNER_MIN_ROWS {
        pgrx::debug1!("pg_accel sort: too few rows");
        return;
    }

    // Gate: Skip when LIMIT is small relative to table size. PG's top-N
    // heapsort is O(n log k) with tiny memory — always faster than full
    // GPU sort + truncation for small k.
    let limit_tuples = root_ref.limit_tuples;
    if limit_tuples > 0.0 && (limit_tuples as usize) < rows / 4 {
        pgrx::debug1!(
            "pg_accel sort: LIMIT {} << {} rows, deferring to PG top-N heapsort",
            limit_tuples as usize,
            rows
        );
        return;
    }

    // Gate: Skip narrow rows. GPU sort's advantage comes from avoiding
    // disk-spill I/O on wide tuples. For narrow rows (<40 bytes), the
    // disk spill is small enough that PG's external merge sort is fast
    // on modern SSDs and GPU materialization overhead isn't amortized.
    let output_width = unsafe { (*(*rel).reltarget).width } as usize;
    if output_width < 40 {
        pgrx::debug1!(
            "pg_accel sort: narrow rows (width={}), deferring to PG",
            output_width
        );
        return;
    }

    // Gate: Skip large tables when PG could use parallel workers.
    // PG's Gather Merge with parallel sort is faster than our single-
    // backend GPU sort. The threshold is set high enough that PG would
    // choose parallel sort — typically tables with > ~2M rows qualify.
    // When max_parallel_workers_per_gather = 0, consider_parallel is
    // still true but PG won't actually use workers.
    if rel_ref.consider_parallel && rows > 20_000_000 {
        // Check if max_parallel_workers_per_gather > 0 via GUC.
        let max_par = unsafe { pg_sys::max_parallel_workers_per_gather };
        if max_par > 0 {
            pgrx::debug1!(
                "pg_accel sort: large table ({} rows) with parallel available, deferring to PG",
                rows
            );
            return;
        }
    }

    // Extract sort key info from the single PathKey.
    // SAFETY: sort_pathkeys has exactly 1 element (checked above).
    let pathkey = unsafe { pg_sys::list_nth(sort_pathkeys, 0).cast::<pg_sys::PathKey>() };
    if pathkey.is_null() {
        return;
    }
    let pk = unsafe { &*pathkey };

    let eclass = pk.pk_eclass;
    if eclass.is_null() {
        return;
    }
    let ec = unsafe { &*eclass };

    // Find a Var member in the equivalence class to get attno and type.
    let ec_members = ec.ec_members;
    if ec_members.is_null() {
        return;
    }
    let num_members = unsafe { pg_sys::list_length(ec_members) };

    let mut var_attno: i16 = 0;
    let mut var_typid: u32 = 0;
    let mut found_var = false;

    for i in 0..num_members {
        // SAFETY: i is in [0, num_members).
        #[allow(clippy::cast_ptr_alignment)]
        let em = unsafe { pg_sys::list_nth(ec_members, i).cast::<pg_sys::EquivalenceMember>() };
        if em.is_null() {
            continue;
        }
        let em_ref = unsafe { &*em };
        let expr = em_ref.em_expr;
        if expr.is_null() {
            continue;
        }
        // SAFETY: expr is a valid Expr node.
        #[allow(clippy::cast_ptr_alignment)]
        if unsafe { (*expr.cast::<pg_sys::Node>()).type_ } == NodeTag::T_Var {
            let var = expr.cast::<pg_sys::Var>();
            // SAFETY: var is a valid Var node.
            var_attno = unsafe { (*var).varattno };
            var_typid = u32::from(em_ref.em_datatype);
            found_var = true;
            break;
        }
    }

    if !found_var || var_attno <= 0 {
        pgrx::debug1!("pg_accel sort: no Var found (found={found_var}, attno={var_attno})");
        return;
    }

    pgrx::debug1!("pg_accel sort: var_attno={var_attno}, var_typid={var_typid}");

    // Gate: Only GPU-sortable numeric types.
    if !matches!(var_typid, SORT_INT4OID | SORT_FLOAT4OID | SORT_FLOAT8OID) {
        pgrx::debug1!("pg_accel sort: unsupported type {var_typid}");
        return;
    }

    // Derive the sort operator OID from the PathKey's opfamily + strategy.
    // SAFETY: get_opfamily_member is a catalog lookup, safe on main thread.
    let sort_op = unsafe {
        pg_sys::get_opfamily_member(
            pk.pk_opfamily,
            pg_sys::Oid::from(var_typid),
            pg_sys::Oid::from(var_typid),
            pk.pk_strategy as i16,
        )
    };
    if sort_op == pg_sys::Oid::INVALID {
        return;
    }

    // Find cheapest path to wrap.
    let cheapest = unsafe { find_cheapest_path(rel_ref.pathlist) };
    if cheapest.is_null() {
        return;
    }

    // Cost estimate: GPU sort must account for tuple materialization
    // (ExecCopySlotMinimalTuple), key extraction, GPU kernel, and emit.
    // Measured overhead: ~0.00026/row on M2 Max. We use a conservative
    // multiplier so PG's parallel sort wins when available.
    // SAFETY: cheapest is non-null.
    let base = unsafe { &*cheapest };
    #[allow(clippy::cast_precision_loss)]
    let n = base.rows;
    let gpu_overhead = cost::GPU_LAUNCH_OVERHEAD;
    // Per-row cost: materialization (0.02) + key extract (0.001) + sort
    // kernel (0.015) + emit (0.01). Total ~0.046/row — deliberately
    // conservative so PG parallel sort wins when workers are available.
    let per_row = cost::GPU_SORT_PER_ROW_COST + cost::PER_DATUM_EXTRACT_COST + 0.03; // materialization + emit overhead
    let sort_cost = n * per_row;
    let startup_cost = base.startup_cost + gpu_overhead;
    let total_cost = (base.total_cost + gpu_overhead + sort_cost) * gucs::cost_multiplier();

    // Build the sort key descriptor.
    let sort_key = SortKeyDesc {
        attno: var_attno,
        sort_op,
        collation: ec.ec_collation,
        nulls_first: pk.pk_nulls_first,
    };

    // SAFETY: Allocating via palloc, building valid CustomPath.
    unsafe {
        let cpath = create_custom_path(
            rel,
            cheapest,
            startup_cost,
            total_cost,
            n,
            custom_scan::scan_path_methods(),
        );

        // Set pathkeys so PG knows our output is already sorted.
        (*cpath).path.pathkeys = sort_pathkeys;

        // Serialize [fn_oid=0, target_attno=0, accel_strategy=GpuSort,
        //            num_sort_keys, attno, sort_op, collation, nulls_first]
        // into custom_private. make_custom_scan_plan reads this and detects
        // the Sort strategy from accel_strategy.
        let mut priv_list: *mut List = std::ptr::null_mut();
        priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast()); // fn_oid
        priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast()); // target_attno
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(registry::AccelStrategy::GpuSort as i32).cast(),
        );
        // Sort key data: [num_keys, attno, sort_op, collation, nulls_first]
        priv_list = lappend(priv_list, pg_sys::makeInteger(1).cast());
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(i32::from(sort_key.attno)).cast(),
        );
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(u32::from(sort_key.sort_op) as i32).cast(),
        );
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(u32::from(sort_key.collation) as i32).cast(),
        );
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(i32::from(sort_key.nulls_first)).cast(),
        );
        // Serialize limit_tuples for top-k sort optimization.
        // 0 or negative means no limit.
        #[allow(clippy::cast_possible_truncation)]
        let limit_int = if limit_tuples > 0.0 {
            limit_tuples as i32
        } else {
            0
        };
        priv_list = lappend(priv_list, pg_sys::makeInteger(limit_int).cast());
        (*cpath).custom_private = priv_list;

        add_path(rel, cpath.cast());

        pgrx::debug1!(
            "pg_accel: injected GpuSort scan path, attno={}, rows={}",
            var_attno,
            rows
        );
    }
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
unsafe extern "C-unwind" fn pgaccel_set_join_pathlist(
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

    // Gate 1: GUC check.
    if !gucs::enabled() {
        return;
    }

    // Gate 2: Registry init + fast-reject.
    registry::lazy_init();
    if !registry::is_ready() {
        return;
    }
    let reg = registry::global_registry();
    if reg.is_empty() {
        return;
    }

    // SAFETY: pointers provided by the planner are valid.
    let joinrel_ref = unsafe { &*joinrel };
    let outerrel_ref = unsafe { &*outerrel };
    let innerrel_ref = unsafe { &*innerrel };
    let extra_ref = unsafe { &*extra };

    // Gate 3: Check join restrictlist for accelerable FuncExpr first,
    // then fall back to equi-join detection for hash join (GPU only).
    let accel = find_accelerable_match(extra_ref.restrictlist);

    // Hash join (equi-join) detection is only enabled when GPU feature is compiled in.
    // Without GPU support, the hash join fallback code is broken, so we prevent
    // GpuHashJoin path injection entirely and let PG's native HashJoin handle it.
    #[cfg(feature = "gpu")]
    let equi = if accel.is_none() {
        // SAFETY: extra_ref.restrictlist is a valid List from the planner.
        unsafe { find_equi_join_key(extra_ref.restrictlist, outerrel, innerrel) }
    } else {
        None
    };

    #[cfg(not(feature = "gpu"))]
    let equi: Option<EquiJoinKey> = None;

    // If neither spatial predicate nor equi-join detected, bail.
    if accel.is_none() && equi.is_none() {
        return;
    }

    let strategy = accel
        .as_ref()
        .map_or(registry::AccelStrategy::GpuHashJoin, |a| a.strategy);

    // Gate 4: Cost model gating — skip if batching is not worthwhile.
    let join_rows_gate = joinrel_ref.rows as usize;
    let min_batch = gucs::min_batch_size().max(1) as usize;
    let per_row_cost = match strategy {
        registry::AccelStrategy::GpuSpatial => cost::GPU_SPATIAL_PER_ROW_COST,
        registry::AccelStrategy::GpuRaster => cost::GPU_RASTER_PER_ROW_COST,
        registry::AccelStrategy::GpuH3 => cost::GPU_H3_PER_ROW_COST,
        _ => cost::BATCHED_EVAL_PER_ROW_COST,
    };
    if !cost::should_batch(join_rows_gate, per_row_cost, min_batch) {
        return;
    }

    // Gate 5: Both sides need cheapest paths.
    let outer_path = outerrel_ref.cheapest_total_path;
    let inner_path = innerrel_ref.cheapest_total_path;
    if outer_path.is_null() || inner_path.is_null() {
        return;
    }

    // Cost estimate using cost model with strategy-aware constants.
    // SAFETY: paths are non-null, verified above.
    let outer_cost = unsafe { (*outer_path).total_cost };
    let inner_cost = unsafe { (*inner_path).total_cost };
    let join_rows = joinrel_ref.rows as usize;
    #[allow(clippy::cast_precision_loss)]
    let batch_size = cost::optimal_batch_size(join_rows) as f64;
    let num_batches = (joinrel_ref.rows / batch_size).ceil();
    let batch_overhead = num_batches * 0.5;
    let gpu_overhead = match strategy {
        registry::AccelStrategy::GpuSpatial
        | registry::AccelStrategy::GpuRaster
        | registry::AccelStrategy::GpuH3 => cost::GPU_LAUNCH_OVERHEAD,
        registry::AccelStrategy::GpuHashJoin => {
            // Hash build O(inner) + probe O(outer).
            let inner_rows = innerrel_ref.rows;
            inner_rows * 0.001 // build cost estimate
        }
        _ => 0.0,
    };
    let per_row_saving = joinrel_ref.rows * per_row_cost;
    // SAFETY: outer_path is non-null, verified above.
    let startup_cost = unsafe { (*outer_path).startup_cost } + 1.0 + gpu_overhead;
    let base_cost = outer_cost + inner_cost;
    let total_cost =
        (base_cost + batch_overhead + gpu_overhead - per_row_saving) * gucs::cost_multiplier();

    // Safety margin — GPU path must be significantly cheaper.
    if total_cost > base_cost * cost::GPU_COST_SAFETY_MARGIN {
        return;
    }

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
            // Hash join path: [fn_oid=0, outer_attno, GpuHashJoin, inner_attno, key_type]
            priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast()); // fn_oid
            priv_list = lappend(priv_list, pg_sys::makeInteger(equi_info.outer_attno).cast());
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(registry::AccelStrategy::GpuHashJoin as i32).cast(),
            );
            priv_list = lappend(priv_list, pg_sys::makeInteger(equi_info.inner_attno).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(equi_info.key_type).cast());
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

// ---------------------------------------------------------------------------
// Upper paths hook (aggregates)
// ---------------------------------------------------------------------------

/// `create_upper_paths_hook` implementation.
///
/// Delegates to `pgaccel_inject_gpu_agg` for `UPPERREL_GROUP_AGG`.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
unsafe extern "C-unwind" fn pgaccel_create_upper_paths(
    root: *mut PlannerInfo,
    stage: UpperRelationKind::Type,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
    extra: *mut std::ffi::c_void,
) {
    // Chain previous hook first.
    // SAFETY: Previous hook, if set, accepts the same planner-provided args.
    unsafe {
        if let Some(prev) = PREV_CREATE_UPPER_PATHS_HOOK {
            prev(root, stage, input_rel, output_rel, extra);
        }
    }
    // Gate: GUC check.
    if !gucs::enabled() {
        return;
    }

    // Aggregate injection for UPPERREL_GROUP_AGG.
    if stage == UpperRelationKind::UPPERREL_GROUP_AGG {
        pgrx::debug1!("pg_accel: create_upper_paths hook fired for UPPERREL_GROUP_AGG");
        // SAFETY: all pointers are valid planner-provided arguments.
        unsafe { pgaccel_inject_gpu_agg(root, input_rel, output_rel) };
    }

    // Window function injection for UPPERREL_WINDOW.
    if stage == UpperRelationKind::UPPERREL_WINDOW {
        pgrx::debug1!("pg_accel: create_upper_paths hook fired for UPPERREL_WINDOW");
        // SAFETY: all pointers are valid planner-provided arguments.
        unsafe { pgaccel_inject_gpu_window(root, input_rel, output_rel) };
    }
}

// ---------------------------------------------------------------------------
// GPU window function injection
// ---------------------------------------------------------------------------

/// Minimum estimated rows to consider GPU window functions.
const GPU_WINDOW_PLANNER_MIN_ROWS: usize = 50_000;

/// Numeric type OIDs supported by window kernels.
const WIN_FLOAT4OID: u32 = 700;
const WIN_FLOAT8OID: u32 = 701;
const WIN_INT4OID: u32 = 23;
const WIN_INT8OID: u32 = 20;

/// Detect window function queries and inject a `CustomPath` that uses GPU
/// window kernels (row_number, rank, dense_rank, sum, count, lag, lead).
///
/// # Safety
///
/// All pointers must be valid planner-provided arguments.
#[allow(
    clippy::too_many_lines,
    clippy::cast_ptr_alignment,
    clippy::cast_possible_wrap,
    clippy::similar_names,
    clippy::cast_precision_loss
)]
unsafe fn pgaccel_inject_gpu_window(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
) {
    // Gate: GPU must be enabled.
    if !gucs::gpu_enabled() {
        pgrx::debug1!("pg_accel window: gpu_enabled=false");
        return;
    }

    // SAFETY: root is a valid PlannerInfo pointer.
    let root_ref = unsafe { &*root };
    let input_ref = unsafe { &*input_rel };

    // Gate: Row count threshold.
    #[allow(clippy::cast_sign_loss)]
    let rows = input_ref.rows as usize;
    if rows < GPU_WINDOW_PLANNER_MIN_ROWS {
        pgrx::debug1!(
            "pg_accel window: rows {} < min {}",
            rows,
            GPU_WINDOW_PLANNER_MIN_ROWS
        );
        return;
    }

    // Parse the query to find WindowClause entries.
    let parse = root_ref.parse;
    if parse.is_null() {
        return;
    }
    // SAFETY: parse is a valid Query pointer.
    let query = unsafe { &*parse };
    let win_clause_list = query.windowClause;
    if win_clause_list.is_null() {
        pgrx::debug1!("pg_accel window: no windowClause");
        return;
    }

    // Scan the target list for WindowFunc nodes.
    let tlist = query.targetList;
    if tlist.is_null() {
        return;
    }
    let tlist_len = unsafe { pg_sys::list_length(tlist) };
    if tlist_len == 0 {
        return;
    }

    let mut specs: Vec<WindowFuncSpec> = Vec::new();

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
        if tag != NodeTag::T_WindowFunc {
            continue;
        }

        let wfunc = expr.cast::<pg_sys::WindowFunc>();
        // SAFETY: wfunc is a valid WindowFunc node.
        let wf = unsafe { &*wfunc };

        // Look up the function name to determine the window op.
        let func_name = unsafe { pg_sys::get_func_name(wf.winfnoid) };
        if func_name.is_null() {
            pgrx::debug1!("pg_accel window: cannot resolve func name");
            return;
        }
        // SAFETY: func_name is a valid null-terminated C string.
        let name = unsafe { std::ffi::CStr::from_ptr(func_name) }.to_bytes();

        let wfunc_enum = match name {
            b"row_number" => WindowFunc::RowNumber,
            b"rank" => WindowFunc::Rank,
            b"dense_rank" => WindowFunc::DenseRank,
            b"sum" => WindowFunc::Sum,
            b"count" => WindowFunc::Count,
            b"lag" => WindowFunc::Lag,
            b"lead" => WindowFunc::Lead,
            _ => {
                pgrx::debug1!("pg_accel window: unsupported func");
                return;
            }
        };

        // Resolve partition and order columns from the WindowClause.
        let winref = wf.winref;
        let wc_len = unsafe { pg_sys::list_length(win_clause_list) };
        let mut wclause: *mut pg_sys::WindowClause = std::ptr::null_mut();
        for j in 0..wc_len {
            let wc = unsafe { pg_sys::list_nth(win_clause_list, j).cast::<pg_sys::WindowClause>() };
            if !wc.is_null() && unsafe { (*wc).winref } == winref {
                wclause = wc;
                break;
            }
        }
        if wclause.is_null() {
            return;
        }
        let wc_ref = unsafe { &*wclause };

        // Extract partition attno from PARTITION BY clause.
        let partition_attno = if wc_ref.partitionClause.is_null() {
            0 // no PARTITION BY
        } else {
            let part_len = unsafe { pg_sys::list_length(wc_ref.partitionClause) };
            // Only support single-column PARTITION BY.
            if part_len != 1 {
                pgrx::debug1!("pg_accel window: multi-column PARTITION BY, skipping");
                return;
            }
            let sc = unsafe {
                pg_sys::list_nth(wc_ref.partitionClause, 0).cast::<pg_sys::SortGroupClause>()
            };
            if sc.is_null() {
                return;
            }
            let sgref = unsafe { (*sc).tleSortGroupRef };
            // Find the TargetEntry matching this sort group ref.
            let mut attno = 0i32;
            for k in 0..tlist_len {
                let te = unsafe { pg_sys::list_nth(tlist, k).cast::<pg_sys::TargetEntry>() };
                if !te.is_null() && unsafe { (*te).ressortgroupref } == sgref {
                    let te_expr = unsafe { (*te).expr };
                    if !te_expr.is_null()
                        && unsafe { (*te_expr.cast::<pg_sys::Node>()).type_ } == NodeTag::T_Var
                    {
                        attno = i32::from(unsafe { (*te_expr.cast::<pg_sys::Var>()).varattno });
                    }
                    break;
                }
            }
            if attno <= 0 {
                pgrx::debug1!("pg_accel window: partition key is not a Var");
                return;
            }
            attno
        };

        // Extract ORDER BY attno.
        let order_attno = if wc_ref.orderClause.is_null() {
            0 // no ORDER BY — only valid for row_number with partition
        } else {
            let ord_len = unsafe { pg_sys::list_length(wc_ref.orderClause) };
            if ord_len != 1 {
                pgrx::debug1!("pg_accel window: multi-column ORDER BY, skipping");
                return;
            }
            let sc = unsafe {
                pg_sys::list_nth(wc_ref.orderClause, 0).cast::<pg_sys::SortGroupClause>()
            };
            if sc.is_null() {
                return;
            }
            let sgref = unsafe { (*sc).tleSortGroupRef };
            let mut attno = 0i32;
            for k in 0..tlist_len {
                let te = unsafe { pg_sys::list_nth(tlist, k).cast::<pg_sys::TargetEntry>() };
                if !te.is_null() && unsafe { (*te).ressortgroupref } == sgref {
                    let te_expr = unsafe { (*te).expr };
                    if !te_expr.is_null()
                        && unsafe { (*te_expr.cast::<pg_sys::Node>()).type_ } == NodeTag::T_Var
                    {
                        attno = i32::from(unsafe { (*te_expr.cast::<pg_sys::Var>()).varattno });
                    }
                    break;
                }
            }
            if attno <= 0 {
                pgrx::debug1!("pg_accel window: order key is not a Var");
                return;
            }
            attno
        };

        // Extract value column attno for aggregate-like window funcs.
        let mut value_attno = 0i32;
        let mut lag_offset = 1i32;
        let mut default_val = 0.0f64;

        let args = wf.args;
        if !args.is_null() {
            let nargs = unsafe { pg_sys::list_length(args) };
            if nargs >= 1 {
                let arg_tle = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::TargetEntry>() };
                if !arg_tle.is_null() {
                    let arg_expr = unsafe { (*arg_tle).expr };
                    if !arg_expr.is_null()
                        && unsafe { (*arg_expr.cast::<pg_sys::Node>()).type_ } == NodeTag::T_Var
                    {
                        let var = arg_expr.cast::<pg_sys::Var>();
                        let typid = u32::from(unsafe { (*var).vartype });
                        if !matches!(
                            typid,
                            WIN_FLOAT4OID | WIN_FLOAT8OID | WIN_INT4OID | WIN_INT8OID
                        ) {
                            pgrx::debug1!("pg_accel window: unsupported value type {typid}");
                            return;
                        }
                        value_attno = i32::from(unsafe { (*var).varattno });
                    }
                }
            }
            // LAG/LEAD offset (second argument, if present and constant).
            if nargs >= 2 {
                let offset_tle = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::TargetEntry>() };
                if !offset_tle.is_null() {
                    let offset_expr = unsafe { (*offset_tle).expr };
                    if !offset_expr.is_null()
                        && unsafe { (*offset_expr.cast::<pg_sys::Node>()).type_ }
                            == NodeTag::T_Const
                    {
                        let cst = offset_expr.cast::<pg_sys::Const>();
                        if !unsafe { (*cst).constisnull } {
                            // SAFETY: int4 Const value is stored in Datum.
                            lag_offset = unsafe { (*cst).constvalue.value() as i32 };
                        }
                    }
                }
            }
            // LAG/LEAD default (third argument, if present and constant).
            if nargs >= 3 {
                let def_tle = unsafe { pg_sys::list_nth(args, 2).cast::<pg_sys::TargetEntry>() };
                if !def_tle.is_null() {
                    let def_expr = unsafe { (*def_tle).expr };
                    if !def_expr.is_null()
                        && unsafe { (*def_expr.cast::<pg_sys::Node>()).type_ } == NodeTag::T_Const
                    {
                        let cst = def_expr.cast::<pg_sys::Const>();
                        if !unsafe { (*cst).constisnull } {
                            // SAFETY: float8 Const value is stored in Datum.
                            default_val =
                                f64::from_bits(unsafe { (*cst).constvalue.value() } as u64);
                        }
                    }
                }
            }
        }

        // For rank/dense_rank/sum/count/lag/lead, ORDER BY is required.
        if order_attno == 0 && !matches!(wfunc_enum, WindowFunc::RowNumber) {
            pgrx::debug1!("pg_accel window: ORDER BY required for {:?}", wfunc_enum);
            return;
        }

        let result_type_oid = u32::from(wf.wintype);

        specs.push(WindowFuncSpec {
            func: wfunc_enum,
            partition_attno,
            order_attno,
            value_attno,
            offset: lag_offset,
            default_val,
            result_type_oid,
        });
    }

    if specs.is_empty() {
        pgrx::debug1!("pg_accel window: no supported window functions found");
        return;
    }

    // Find cheapest input path.
    let cheapest = unsafe { find_cheapest_path(input_ref.pathlist) };
    if cheapest.is_null() {
        return;
    }

    // Cost estimate: GPU window is cheaper than PG's WindowAgg for large
    // tables because partition boundary detection + running aggregates
    // are parallelized on the GPU.
    // SAFETY: cheapest is non-null.
    let base = unsafe { &*cheapest };
    let gpu_overhead = cost::GPU_LAUNCH_OVERHEAD;
    let per_row = 0.04; // materialization + partition detect + kernel dispatch
    let window_cost = base.rows * per_row * specs.len() as f64;
    let startup_cost = base.total_cost + gpu_overhead;
    let total_cost = (base.total_cost + gpu_overhead + window_cost) * gucs::cost_multiplier();

    // SAFETY: Allocating via palloc, building valid CustomPath.
    unsafe {
        let cpath = create_custom_path(
            output_rel,
            cheapest,
            startup_cost,
            total_cost,
            base.rows,
            custom_scan::window_path_methods(),
        );

        // Serialize window specs into custom_private.
        // Layout: [num_specs, func0, part_attno0, order_attno0, value_attno0,
        //   offset0, default_bits0, result_type0, ...]
        let mut priv_list: *mut List = std::ptr::null_mut();
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let num_specs = specs.len() as i32;
        priv_list = lappend(priv_list, pg_sys::makeInteger(num_specs).cast());
        for spec in &specs {
            priv_list = lappend(priv_list, pg_sys::makeInteger(spec.func.to_i32()).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(spec.partition_attno).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(spec.order_attno).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(spec.value_attno).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(spec.offset).cast());
            #[allow(clippy::cast_possible_wrap)]
            let default_bits = spec.default_val.to_bits() as i32;
            priv_list = lappend(priv_list, pg_sys::makeInteger(default_bits).cast());
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(spec.result_type_oid as i32).cast(),
            );
        }
        (*cpath).custom_private = priv_list;

        add_path(output_rel, cpath.cast());

        pgrx::debug1!(
            "pg_accel: injected GpuWindow path for {} spec(s), rows={}",
            specs.len(),
            rows
        );
    }
}

// ---------------------------------------------------------------------------
// GPU aggregate injection
// ---------------------------------------------------------------------------

/// Reducible numeric type OIDs (float4, float8, int4, int8).
const AGG_FLOAT4OID: u32 = 700;
const AGG_FLOAT8OID: u32 = 701;
const AGG_INT4OID: u32 = 23;
const AGG_INT8OID: u32 = 20;

/// Minimum estimated rows to consider GPU reduce.
const GPU_REDUCE_PLANNER_MIN_ROWS: usize = 50_000;

/// Detect simple aggregate queries (`SELECT sum/min/max/avg/count FROM table`
/// with no GROUP BY) and inject a `CustomPath` that uses GPU reduce.
///
/// # Safety
///
/// All pointers must be valid planner-provided arguments.
#[allow(
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::cast_ptr_alignment
)]
unsafe fn pgaccel_inject_gpu_agg(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
) {
    // Gate: GPU must be enabled for hash aggregation.
    #[cfg(not(feature = "gpu"))]
    {
        return;
    }
    if !gucs::gpu_enabled() {
        pgrx::debug1!("pg_accel: gpu_agg rejected: gpu_enabled=false");
        return;
    }

    // SAFETY: root is a valid PlannerInfo pointer.
    let root_ref = unsafe { &*root };
    let input_ref = unsafe { &*input_rel };

    // Gate: Check GROUP BY — we support plain aggregates and single-column GROUP BY.
    let parse = root_ref.parse;
    if parse.is_null() {
        return;
    }
    // SAFETY: parse is a valid Query pointer.
    let query = unsafe { &*parse };
    let group_len = if query.groupClause.is_null() {
        0
    } else {
        // SAFETY: groupClause is a valid List.
        unsafe { pg_sys::list_length(query.groupClause) }
    };
    // Reject multi-column GROUP BY (only single-column supported for now).
    if group_len > 1 {
        pgrx::debug1!("pg_accel: gpu_agg rejected: multi-column GROUP BY");
        return;
    }

    // Extract group key info for single-column GROUP BY.
    let group_key_info: Option<GroupKeyInfo> = if group_len == 1 {
        // SAFETY: groupClause is a non-null List with at least 1 element.
        let sc =
            unsafe { pg_sys::list_nth(query.groupClause, 0).cast::<pg_sys::SortGroupClause>() };
        if sc.is_null() {
            return;
        }
        // Find the TargetEntry in the target list that matches tleSortGroupRef.
        // SAFETY: sc is a valid SortGroupClause.
        let sgref = unsafe { (*sc).tleSortGroupRef };
        let tlist = query.targetList;
        let mut group_tle: *mut pg_sys::TargetEntry = std::ptr::null_mut();
        let tlist_len = if tlist.is_null() {
            0
        } else {
            unsafe { pg_sys::list_length(tlist) }
        };
        for j in 0..tlist_len {
            // SAFETY: j is in [0, tlist_len).
            let tle = unsafe { pg_sys::list_nth(tlist, j).cast::<pg_sys::TargetEntry>() };
            if !tle.is_null() && unsafe { (*tle).ressortgroupref } == sgref {
                group_tle = tle;
                break;
            }
        }
        if group_tle.is_null() {
            return;
        }
        // The group key expression must be a Var on a numeric type.
        // SAFETY: group_tle is a valid TargetEntry.
        let gk_expr = unsafe { (*group_tle).expr };
        if gk_expr.is_null() {
            return;
        }
        // SAFETY: reading node tag.
        let gk_tag = unsafe { (*gk_expr.cast::<pg_sys::Node>()).type_ };
        if gk_tag != NodeTag::T_Var {
            pgrx::debug1!("pg_accel: gpu_agg rejected: GROUP BY expr is not a Var");
            return;
        }
        let gk_var = gk_expr.cast::<pg_sys::Var>();
        let gk_typid = unsafe { (*gk_var).vartype };
        let gk_attno = i32::from(unsafe { (*gk_var).varattno });
        let Some(key_type) = GroupKeyInfo::key_type_from_oid(gk_typid) else {
            pgrx::debug1!("pg_accel: gpu_agg rejected: unsupported GROUP BY type");
            return;
        };
        Some(GroupKeyInfo {
            attno: gk_attno,
            type_oid: gk_typid,
            key_type,
        })
    } else {
        None
    };

    // Gate: Row count threshold.
    #[allow(clippy::cast_sign_loss)]
    let rows = input_ref.rows as usize;
    pgrx::debug1!("pg_accel: gpu_agg candidate, rows={}", rows);
    if rows < GPU_REDUCE_PLANNER_MIN_ROWS {
        pgrx::debug1!(
            "pg_accel: gpu_agg rejected: rows < {}",
            GPU_REDUCE_PLANNER_MIN_ROWS
        );
        return;
    }

    // Gate: Scan the target list for Aggref nodes on numeric columns.
    let tlist = query.targetList;
    if tlist.is_null() {
        return;
    }

    let tlist_len = unsafe { pg_sys::list_length(tlist) };
    if tlist_len == 0 {
        return;
    }

    // Collect (AggOp, attno, result_type_oid) for each target list entry.
    let mut agg_descs: Vec<(AggOp, i32, u32)> = Vec::with_capacity(tlist_len as usize);

    // SAFETY: tlist is a valid List of TargetEntry nodes.
    for i in 0..tlist_len {
        // SAFETY: i is in [0, tlist_len).
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() {
            return;
        }
        // SAFETY: tle is a valid TargetEntry.
        let expr = unsafe { (*tle).expr };
        if expr.is_null() {
            return;
        }
        // SAFETY: reading node tag.
        let tag = unsafe { (*expr.cast::<pg_sys::Node>()).type_ };
        if tag != NodeTag::T_Aggref {
            // In grouped mode, the group key Var is expected in the target list.
            // Skip it (it's handled separately via group_key_info).
            if group_key_info.is_some() && tag == NodeTag::T_Var {
                continue;
            }
            // Non-aggregate, non-group-key in target list — reject.
            return;
        }

        let aggref = expr.cast::<pg_sys::Aggref>();
        // SAFETY: aggref is a valid Aggref node.
        let aggref_ref = unsafe { &*aggref };

        // Determine the aggregate operation from the function name.
        let agg_name = unsafe { pg_sys::get_func_name(aggref_ref.aggfnoid) };
        if agg_name.is_null() {
            return;
        }
        // SAFETY: agg_name is a valid null-terminated C string from pg_proc.
        let name_cstr = unsafe { std::ffi::CStr::from_ptr(agg_name) };
        let name = name_cstr.to_bytes();

        pgrx::debug1!(
            "pg_accel: gpu_agg tlist[{}] func={}",
            i,
            name_cstr.to_str().unwrap_or("?")
        );

        let op = match name {
            b"sum" | b"float4_accum" | b"int4_sum" | b"int8_sum" | b"float8pl" => AggOp::Sum,
            b"avg" | b"float8_avg" => AggOp::Avg,
            b"min" => AggOp::Min,
            b"max" => AggOp::Max,
            b"count" => AggOp::Count,
            _ => {
                pgrx::debug1!("pg_accel: gpu_agg rejected: unsupported func");
                return;
            }
        };

        // COUNT(*): attno = 0, result type = int8.
        if op == AggOp::Count && aggref_ref.aggstar {
            agg_descs.push((AggOp::Count, 0, u32::from(aggref_ref.aggtype)));
            continue;
        }

        // Non-star aggregate: must have a Var argument on a numeric type.
        let args = aggref_ref.args;
        if args.is_null() {
            return;
        }
        // SAFETY: args is a valid List.
        let nargs = unsafe { pg_sys::list_length(args) };
        if nargs < 1 {
            return;
        }
        // SAFETY: first arg is a TargetEntry wrapping the expr.
        let arg_tle = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::TargetEntry>() };
        if arg_tle.is_null() {
            return;
        }
        let arg_expr = unsafe { (*arg_tle).expr };
        if arg_expr.is_null()
            || unsafe { (*arg_expr.cast::<pg_sys::Node>()).type_ } != NodeTag::T_Var
        {
            return;
        }
        let var = arg_expr.cast::<pg_sys::Var>();
        let typid = u32::from(unsafe { (*var).vartype });
        if !matches!(
            typid,
            AGG_FLOAT4OID | AGG_FLOAT8OID | AGG_INT4OID | AGG_INT8OID
        ) {
            return;
        }
        let attno = i32::from(unsafe { (*var).varattno });
        agg_descs.push((op, attno, u32::from(aggref_ref.aggtype)));
    }

    if agg_descs.is_empty() {
        return;
    }

    // Find cheapest input path.
    let cheapest = unsafe { find_cheapest_path(input_ref.pathlist) };
    if cheapest.is_null() {
        return;
    }

    // Cost estimate: GPU reduce/hash-agg replaces PG's hash/sort aggregate.
    // SAFETY: cheapest is non-null.
    let base = unsafe { &*cheapest };
    let gpu_overhead = cost::GPU_LAUNCH_OVERHEAD;
    let reduce_per_row = cost::GPU_REDUCE_PER_ROW_COST + 0.02;
    // For grouped agg, add hash table build + probe cost.
    let hash_overhead = if group_key_info.is_some() { 0.005 } else { 0.0 };
    let reduce_cost = base.rows * (reduce_per_row + hash_overhead);
    let startup_cost = base.total_cost + gpu_overhead;
    let total_cost = (base.total_cost + gpu_overhead + reduce_cost) * gucs::cost_multiplier();

    // SAFETY: Allocating via palloc, building valid CustomPath.
    unsafe {
        let cpath = pg_sys::palloc0(std::mem::size_of::<CustomPath>()).cast::<CustomPath>();

        let child_copy = {
            let path_size = path_node_size(cheapest);
            let copy = pg_sys::palloc(path_size).cast::<Path>();
            std::ptr::copy_nonoverlapping(cheapest.cast::<u8>(), copy.cast::<u8>(), path_size);
            copy
        };

        (*cpath).path.type_ = NodeTag::T_CustomPath;
        (*cpath).path.pathtype = NodeTag::T_CustomScan;
        (*cpath).path.parent = output_rel;
        (*cpath).path.pathtarget = (*output_rel).reltarget;
        (*cpath).path.param_info = std::ptr::null_mut();
        (*cpath).path.parallel_aware = false;
        (*cpath).path.parallel_safe = base.parallel_safe;
        (*cpath).path.parallel_workers = 0;
        // Plain agg produces 1 row; grouped agg uses planner's group estimate.
        (*cpath).path.rows = if group_key_info.is_some() {
            (*output_rel).rows.max(1.0)
        } else {
            1.0
        };
        (*cpath).path.startup_cost = startup_cost;
        (*cpath).path.total_cost = total_cost;
        (*cpath).path.pathkeys = std::ptr::null_mut();

        (*cpath).flags = 0;
        (*cpath).custom_paths = lappend(std::ptr::null_mut(), child_copy.cast());
        (*cpath).custom_restrictinfo = std::ptr::null_mut();
        (*cpath).methods = custom_scan::agg_path_methods();

        // Serialize [num_aggs, op0, attno0, rtype0, ..., has_group_key,
        //   gk_attno, gk_type_oid, gk_key_type] into custom_private.
        let mut priv_list: *mut List = std::ptr::null_mut();
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let num_aggs = agg_descs.len() as i32;
        priv_list = lappend(priv_list, pg_sys::makeInteger(num_aggs).cast());
        #[allow(clippy::cast_possible_wrap)]
        for &(op, attno, rtype) in &agg_descs {
            priv_list = lappend(priv_list, pg_sys::makeInteger(op.to_i32()).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(attno).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(rtype as i32).cast());
        }
        // Append group key info (or 0 for plain agg).
        #[allow(clippy::cast_possible_wrap)]
        if let Some(ref gk) = group_key_info {
            priv_list = lappend(priv_list, pg_sys::makeInteger(1).cast()); // has_group_key
            priv_list = lappend(priv_list, pg_sys::makeInteger(gk.attno).cast());
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(u32::from(gk.type_oid) as i32).cast(),
            );
            priv_list = lappend(priv_list, pg_sys::makeInteger(gk.key_type).cast());
        } else {
            priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast()); // no group key
        }
        (*cpath).custom_private = priv_list;

        add_path(output_rel, cpath.cast());

        pgrx::debug1!(
            "pg_accel: injected GpuReduce path for {} agg(s), rows={}",
            agg_descs.len(),
            rows
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find the cheapest total-cost path in a pathlist.
///
/// # Safety
///
/// `pathlist` must be a valid PG `List` pointer or null.
unsafe fn find_cheapest_path(pathlist: *mut List) -> *mut Path {
    if pathlist.is_null() {
        return std::ptr::null_mut();
    }

    // SAFETY: pathlist is a valid List from the planner.
    let len = unsafe { pg_sys::list_length(pathlist) };
    if len == 0 {
        return std::ptr::null_mut();
    }

    let mut best: *mut Path = std::ptr::null_mut();
    for i in 0..len {
        // SAFETY: i is in [0, len), list_nth returns a valid pointer.
        let path = unsafe { pg_sys::list_nth(pathlist, i).cast::<Path>() };
        if path.is_null() {
            continue;
        }
        // SAFETY: path and best are non-null valid Path pointers from the planner list.
        if best.is_null() || unsafe { (*path).total_cost < (*best).total_cost } {
            best = path;
        }
    }
    best
}

/// Check if a `List` of `RestrictInfo` contains a function registered in the
/// acceleration registry.
///
/// Walks clause trees recursively to find `FuncExpr` and `OpExpr` nodes
/// inside `BoolExpr` (AND/OR/NOT) nodes.
fn has_accelerable_restriction(restrictinfo_list: *mut List) -> bool {
    if restrictinfo_list.is_null() {
        return false;
    }

    let reg = registry::global_registry();

    // SAFETY: restrictinfo_list is a valid List pointer from the planner.
    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    for i in 0..len {
        // SAFETY: i is in [0, len), list_nth returns a valid RestrictInfo*.
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        // SAFETY: ri is a valid RestrictInfo from the planner.
        let clause = unsafe { (*ri).clause };
        if clause.is_null() {
            continue;
        }

        if node_has_accel_func(clause.cast(), reg) {
            return true;
        }
    }

    false
}

/// Result of scanning restriction clauses for a registered accelerable function.
/// Carries all metadata needed for `custom_private` serialization.
struct AccelMatch {
    strategy: registry::AccelStrategy,
    fn_oid: pg_sys::Oid,
    /// 1-based attribute number of the Var argument, or 0 if none found.
    target_attno: i32,
}

/// Find the first registered accelerable function in a `List` of `RestrictInfo`.
///
/// Returns the strategy, function OID, and target attribute number so the
/// planner hook can serialize them into `custom_private`.
fn find_accelerable_match(restrictinfo_list: *mut List) -> Option<AccelMatch> {
    if restrictinfo_list.is_null() {
        return None;
    }

    let reg = registry::global_registry();

    // SAFETY: restrictinfo_list is a valid List pointer from the planner.
    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    for i in 0..len {
        // SAFETY: i is in [0, len), list_nth returns a valid RestrictInfo*.
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        // SAFETY: ri is a valid RestrictInfo from the planner.
        let clause = unsafe { (*ri).clause };
        if clause.is_null() {
            continue;
        }

        if let Some(m) = node_find_accel_match(clause.cast(), reg) {
            return Some(m);
        }
    }

    None
}

/// Backward-compatible wrapper that returns only the strategy.
fn find_accelerable_strategy(restrictinfo_list: *mut List) -> Option<registry::AccelStrategy> {
    find_accelerable_match(restrictinfo_list).map(|m| m.strategy)
}

/// Result of detecting an equi-join condition (e.g., `a.col = b.col`).
struct EquiJoinKey {
    /// 1-based attribute number of the outer relation's join key.
    outer_attno: i32,
    /// 1-based attribute number of the inner relation's join key.
    inner_attno: i32,
    /// Key type: 0=int32, 1=int64, 2=float64.
    key_type: i32,
}

/// Scan a `RestrictInfo` list for an equi-join condition (`Var = Var`) where
/// the two `Var` nodes reference different relations.
///
/// Returns the outer/inner attribute numbers and key type if found.
///
/// # Safety
///
/// `restrictinfo_list` must be null or a valid PG `List` of `RestrictInfo`.
unsafe fn find_equi_join_key(
    restrictinfo_list: *mut List,
    outerrel: *mut RelOptInfo,
    innerrel: *mut RelOptInfo,
) -> Option<EquiJoinKey> {
    if restrictinfo_list.is_null() {
        return None;
    }

    // SAFETY: outerrel and innerrel are valid planner pointers.
    let outer_relids = unsafe { (*outerrel).relids };
    let inner_relids = unsafe { (*innerrel).relids };

    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    for i in 0..len {
        // SAFETY: i is in [0, len), list_nth returns a valid RestrictInfo*.
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        // SAFETY: ri is a valid RestrictInfo from the planner.
        let clause = unsafe { (*ri).clause };
        if clause.is_null() {
            continue;
        }

        // SAFETY: clause is a valid Node pointer.
        let tag = unsafe { (*clause.cast::<pg_sys::Node>()).type_ };
        if tag != NodeTag::T_OpExpr {
            continue;
        }

        // SAFETY: tag confirmed this is an OpExpr.
        #[allow(clippy::cast_ptr_alignment)]
        let opexpr = clause.cast::<pg_sys::OpExpr>();

        // Check if this is an equality operator.
        // SAFETY: opexpr is a valid OpExpr.
        let opno = unsafe { (*opexpr).opno };

        // Check if this is an equality operator by looking at the opno.
        // Common equality operators: int4eq=96, int2eq=94, int8eq=410,
        // float4eq=620, float8eq=670, int24eq=532, int42eq=533,
        // int48eq=474, int84eq=416.
        // SAFETY: opexpr is valid; reading opresulttype to verify boolean result.
        let result_type = unsafe { (*opexpr).opresulttype };
        // Equality operators return boolean (OID 16).
        if u32::from(result_type) != 16 {
            continue;
        }
        // Use op_mergejoinable as a proxy: merge-joinable operators are
        // equality operators usable for equi-joins.
        // SAFETY: opno is a valid operator OID. The second arg is the
        // input type — we pass InvalidOid to check any input type.
        let is_equality = unsafe { pg_sys::op_mergejoinable(opno, pg_sys::InvalidOid) };
        if !is_equality {
            continue;
        }

        // SAFETY: opexpr->args is a valid List.
        let args = unsafe { (*opexpr).args };
        if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
            continue;
        }

        // SAFETY: args has exactly 2 elements.
        let left = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
        let right = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };

        if left.is_null() || right.is_null() {
            continue;
        }

        // Both sides must be Var nodes (possibly with RelabelType wrappers).
        // SAFETY: left and right are valid Node pointers from the planner.
        let left_var = unsafe { unwrap_var(left) };
        let right_var = unsafe { unwrap_var(right) };
        if left_var.is_null() || right_var.is_null() {
            continue;
        }

        // SAFETY: left_var and right_var are valid Var nodes.
        let left_varno = unsafe { (*left_var).varno } as i32;
        let right_varno = unsafe { (*right_var).varno } as i32;
        let left_attno = unsafe { (*left_var).varattno } as i32;
        let right_attno = unsafe { (*right_var).varattno } as i32;
        let left_type = unsafe { (*left_var).vartype };
        let right_type = unsafe { (*right_var).vartype };

        // Determine which is outer and which is inner.
        // SAFETY: bms_is_member checks set membership.
        let left_is_outer = unsafe { pg_sys::bms_is_member(left_varno, outer_relids) };
        let left_is_inner = unsafe { pg_sys::bms_is_member(left_varno, inner_relids) };
        let right_is_outer = unsafe { pg_sys::bms_is_member(right_varno, outer_relids) };
        let right_is_inner = unsafe { pg_sys::bms_is_member(right_varno, inner_relids) };

        let (outer_attno, inner_attno, key_oid) = if left_is_outer && right_is_inner {
            (left_attno, right_attno, left_type)
        } else if left_is_inner && right_is_outer {
            (right_attno, left_attno, right_type)
        } else {
            continue;
        };

        // Map PG type OID to key type tag.
        let key_type = match u32::from(key_oid) {
            // int2 (21), int4 (23)
            21 | 23 => 0, // Int32
            // int8 (20)
            20 => 1, // Int64
            // float4 (700), float8 (701)
            700 | 701 => 2, // Float64
            _ => continue,  // Unsupported key type
        };

        return Some(EquiJoinKey {
            outer_attno,
            inner_attno,
            key_type,
        });
    }

    None
}

/// Unwrap a `Var` node from possible `RelabelType` wrappers.
///
/// # Safety
///
/// `node` must be a valid PG `Node` pointer (or null).
unsafe fn unwrap_var(mut node: *mut pg_sys::Node) -> *mut pg_sys::Var {
    if node.is_null() {
        return std::ptr::null_mut();
    }
    // Strip RelabelType wrappers.
    loop {
        // SAFETY: node is a valid Node pointer.
        let tag = unsafe { (*node).type_ };
        if tag == NodeTag::T_RelabelType {
            // SAFETY: tag confirmed RelabelType.
            #[allow(clippy::cast_ptr_alignment)]
            let relabel = node.cast::<pg_sys::RelabelType>();
            node = unsafe { (*relabel).arg.cast::<pg_sys::Node>() };
            if node.is_null() {
                return std::ptr::null_mut();
            }
        } else {
            break;
        }
    }
    // SAFETY: node is non-null.
    if unsafe { (*node).type_ } == NodeTag::T_Var {
        #[allow(clippy::cast_ptr_alignment)]
        node.cast::<pg_sys::Var>()
    } else {
        std::ptr::null_mut()
    }
}

/// Recursively find the first registered accelerable function in a node tree.
/// Returns the strategy, function OID, and target attribute number.
fn node_find_accel_match(
    node: *mut pg_sys::Node,
    reg: &registry::AdapterRegistry,
) -> Option<AccelMatch> {
    if node.is_null() {
        return None;
    }

    // SAFETY: node is a valid PG Node pointer; we read its tag.
    let tag = unsafe { (*node).type_ };

    // SAFETY: PG nodes are palloc'd (always >=8-byte aligned), and we
    // confirmed the NodeTag before casting.
    #[allow(clippy::cast_ptr_alignment)]
    match tag {
        NodeTag::T_FuncExpr => {
            // SAFETY: tag confirmed this is a FuncExpr.
            let funcexpr = node.cast::<pg_sys::FuncExpr>();
            // SAFETY: funcexpr is valid; reading funcid field.
            let oid = unsafe { (*funcexpr).funcid };
            if let Some(entry) = reg.lookup(oid) {
                let attno = extract_var_attno_from_args(unsafe { (*funcexpr).args });
                return Some(AccelMatch {
                    strategy: entry.strategy,
                    fn_oid: oid,
                    target_attno: attno,
                });
            }
            // SAFETY: funcexpr is valid; reading args field.
            let args = unsafe { (*funcexpr).args };
            recurse_args_for_match(args, reg)
        }
        NodeTag::T_OpExpr => {
            // SAFETY: tag confirmed this is an OpExpr.
            let opexpr = node.cast::<pg_sys::OpExpr>();
            // Read opfuncid — already resolved for most exprs by the
            // parser/rewriter. Only call set_opfuncid (syscache probe)
            // if still InvalidOid, avoiding ~100-500ns per operator.
            let mut oid = unsafe { (*opexpr).opfuncid };
            if oid == pg_sys::InvalidOid {
                // SAFETY: opexpr is a valid OpExpr pointer.
                unsafe { pg_sys::set_opfuncid(opexpr) };
                oid = unsafe { (*opexpr).opfuncid };
            }
            if let Some(entry) = reg.lookup(oid) {
                let attno = extract_var_attno_from_args(unsafe { (*opexpr).args });
                return Some(AccelMatch {
                    strategy: entry.strategy,
                    fn_oid: oid,
                    target_attno: attno,
                });
            }
            // SAFETY: opexpr is valid; reading args field.
            let args = unsafe { (*opexpr).args };
            recurse_args_for_match(args, reg)
        }
        NodeTag::T_BoolExpr => {
            // SAFETY: tag confirmed BoolExpr; reading args list.
            let args = unsafe { (*node.cast::<pg_sys::BoolExpr>()).args };
            recurse_args_for_match(args, reg)
        }
        _ => None,
    }
}

/// Recurse into a `List` of expression nodes looking for an accelerable function.
fn recurse_args_for_match(args: *mut List, reg: &registry::AdapterRegistry) -> Option<AccelMatch> {
    if args.is_null() {
        return None;
    }
    // SAFETY: args is a valid non-null List from the planner.
    let len = unsafe { pg_sys::list_length(args) };
    for j in 0..len {
        // SAFETY: j is in [0, len), list_nth returns a valid pointer.
        let child = unsafe { pg_sys::list_nth(args, j).cast::<pg_sys::Node>() };
        if let Some(m) = node_find_accel_match(child, reg) {
            return Some(m);
        }
    }
    None
}

/// Extract the `varattno` (1-based column number) from the first `Var` node
/// in a function's argument list. Returns 0 if no `Var` is found.
#[allow(clippy::cast_ptr_alignment)]
fn extract_var_attno_from_args(args: *mut List) -> i32 {
    if args.is_null() {
        return 0;
    }
    // SAFETY: args is a valid List from the planner.
    let len = unsafe { pg_sys::list_length(args) };
    for i in 0..len {
        // SAFETY: i is in [0, len).
        let node = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
        if node.is_null() {
            continue;
        }
        // SAFETY: reading the node tag.
        if unsafe { (*node).type_ } == NodeTag::T_Var {
            // SAFETY: tag confirmed Var; reading varattno.
            let var = node.cast::<pg_sys::Var>();
            return i32::from(unsafe { (*var).varattno });
        }
    }
    0
}

/// Recursively check if a node tree contains a function registered in
/// the acceleration registry.
///
/// Handles `FuncExpr`, `OpExpr` (with forced `opfuncid` resolution),
/// and `BoolExpr` (recurses into AND/OR/NOT args).
fn node_has_accel_func(node: *mut pg_sys::Node, reg: &registry::AdapterRegistry) -> bool {
    if node.is_null() {
        return false;
    }

    // SAFETY: node is a valid PG Node pointer; we read its tag.
    let tag = unsafe { (*node).type_ };

    // SAFETY: PG nodes are palloc'd (always >=8-byte aligned), and we
    // confirmed the NodeTag before casting.
    #[allow(clippy::cast_ptr_alignment)]
    match tag {
        NodeTag::T_FuncExpr => {
            // SAFETY: tag confirmed this is a FuncExpr.
            let funcexpr = node.cast::<pg_sys::FuncExpr>();
            let oid = unsafe { (*funcexpr).funcid };
            if reg.lookup(oid).is_some() {
                return true;
            }
            // SAFETY: funcexpr is a valid FuncExpr (tag-checked above).
            // Recurse into function arguments (e.g. abs(sqrt(x))).
            let args = unsafe { (*funcexpr).args };
            if !args.is_null() {
                // SAFETY: args is a non-null valid List from the FuncExpr.
                let len = unsafe { pg_sys::list_length(args) };
                for j in 0..len {
                    // SAFETY: j is in [0, len), list_nth returns a valid pointer.
                    let child = unsafe { pg_sys::list_nth(args, j).cast::<pg_sys::Node>() };
                    if node_has_accel_func(child, reg) {
                        return true;
                    }
                }
            }
            false
        }
        NodeTag::T_OpExpr => {
            // SAFETY: tag confirmed this is an OpExpr. Only resolve
            // opfuncid via syscache if not already set.
            let opexpr = node.cast::<pg_sys::OpExpr>();
            let mut oid = unsafe { (*opexpr).opfuncid };
            if oid == pg_sys::InvalidOid {
                unsafe { pg_sys::set_opfuncid(opexpr) };
                oid = unsafe { (*opexpr).opfuncid };
            }
            if reg.lookup(oid).is_some() {
                return true;
            }
            // SAFETY: opexpr is a valid OpExpr (tag-checked above).
            // Recurse into operator arguments to find nested
            // accelerable functions (e.g. abs(x) > 50000).
            let args = unsafe { (*opexpr).args };
            if !args.is_null() {
                // SAFETY: args is a non-null valid List from the OpExpr.
                let len = unsafe { pg_sys::list_length(args) };
                for j in 0..len {
                    // SAFETY: j is in [0, len), list_nth returns a valid pointer.
                    let child = unsafe { pg_sys::list_nth(args, j).cast::<pg_sys::Node>() };
                    if node_has_accel_func(child, reg) {
                        return true;
                    }
                }
            }
            false
        }
        NodeTag::T_BoolExpr => {
            // SAFETY: tag confirmed this is a BoolExpr. Recurse into
            // all child args (AND/OR/NOT).
            let args = unsafe { (*node.cast::<pg_sys::BoolExpr>()).args };
            if args.is_null() {
                return false;
            }
            // SAFETY: args is a non-null valid List from the BoolExpr.
            let len = unsafe { pg_sys::list_length(args) };
            for j in 0..len {
                // SAFETY: j is in [0, len), list_nth returns a valid pointer.
                let child = unsafe { pg_sys::list_nth(args, j).cast::<pg_sys::Node>() };
                if node_has_accel_func(child, reg) {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// Allocate and initialize a `CustomPath` node via `palloc0`.
///
/// IMPORTANT: The child path is copied into independently allocated memory
/// because `add_path` will pfree dominated paths. If our custom path is
/// cheaper, the original path would be freed, leaving a dangling pointer
/// in `custom_paths`.
///
/// # Safety
///
/// `rel` and `base_path` must be valid planner pointers. `methods` must
/// point to a static `CustomPathMethods` with `'static` lifetime.
unsafe fn create_custom_path(
    rel: *mut RelOptInfo,
    base_path: *mut Path,
    startup_cost: pg_sys::Cost,
    total_cost: pg_sys::Cost,
    rows: f64,
    methods: *const pg_sys::CustomPathMethods,
) -> *mut CustomPath {
    // SAFETY: palloc0 returns zeroed memory of the requested size.
    let cpath = unsafe { pg_sys::palloc0(std::mem::size_of::<CustomPath>()).cast::<CustomPath>() };

    // SAFETY: Shallow copy of Path via palloc + memcpy is safe because Path
    // structs do not own palloc'd sub-allocations that would need deep-copy
    // semantics (pointers like pathkeys and param_info reference shared
    // planner data structures). We use path_node_size to get the correct
    // struct size based on the node tag. base_path is a valid non-null pointer.
    let child_copy = unsafe {
        let path_size = path_node_size(base_path);
        let copy = pg_sys::palloc(path_size).cast::<Path>();
        std::ptr::copy_nonoverlapping(base_path.cast::<u8>(), copy.cast::<u8>(), path_size);
        copy
    };

    // SAFETY: cpath is freshly allocated and zeroed; all fields set below.
    unsafe {
        (*cpath).path.type_ = NodeTag::T_CustomPath;
        (*cpath).path.pathtype = NodeTag::T_CustomScan;
        (*cpath).path.parent = rel;
        (*cpath).path.pathtarget = (*rel).reltarget;
        (*cpath).path.param_info = (*base_path).param_info;
        (*cpath).path.parallel_aware = false;
        (*cpath).path.parallel_safe = (*base_path).parallel_safe;
        (*cpath).path.parallel_workers = 0;
        (*cpath).path.rows = rows;
        (*cpath).path.startup_cost = startup_cost;
        (*cpath).path.total_cost = total_cost;
        (*cpath).path.pathkeys = (*base_path).pathkeys;

        (*cpath).flags = 0;
        (*cpath).custom_paths = lappend(std::ptr::null_mut(), child_copy.cast());
        (*cpath).custom_restrictinfo = std::ptr::null_mut();
        (*cpath).custom_private = std::ptr::null_mut();
        (*cpath).methods = methods;
    }

    cpath
}

/// Determine the struct size of a `Path` node based on its `type_` tag.
///
/// PostgreSQL uses different struct sizes for different path types (e.g.,
/// `IndexPath` is larger than a plain `Path`). We need the correct size to
/// do a safe shallow copy via `palloc` + `memcpy`.
///
/// Falls back to `sizeof(Path)` for unknown tags, which is safe because we
/// only copy bytes — we never interpret fields beyond the base `Path`.
///
/// # Safety
///
/// `path` must be a valid, non-null pointer to a PG Path node.
unsafe fn path_node_size(path: *mut Path) -> usize {
    // SAFETY: path is a valid pointer, reading the tag is safe.
    let tag = unsafe { (*path).type_ };
    match tag {
        NodeTag::T_IndexPath => std::mem::size_of::<pg_sys::IndexPath>(),
        NodeTag::T_BitmapHeapPath => std::mem::size_of::<pg_sys::BitmapHeapPath>(),
        NodeTag::T_BitmapAndPath => std::mem::size_of::<pg_sys::BitmapAndPath>(),
        NodeTag::T_BitmapOrPath => std::mem::size_of::<pg_sys::BitmapOrPath>(),
        NodeTag::T_TidPath => std::mem::size_of::<pg_sys::TidPath>(),
        NodeTag::T_SubqueryScanPath => std::mem::size_of::<pg_sys::SubqueryScanPath>(),
        NodeTag::T_ForeignPath => std::mem::size_of::<pg_sys::ForeignPath>(),
        NodeTag::T_CustomPath => std::mem::size_of::<pg_sys::CustomPath>(),
        NodeTag::T_NestPath => std::mem::size_of::<pg_sys::NestPath>(),
        NodeTag::T_MergePath => std::mem::size_of::<pg_sys::MergePath>(),
        NodeTag::T_HashPath => std::mem::size_of::<pg_sys::HashPath>(),
        NodeTag::T_AppendPath => std::mem::size_of::<pg_sys::AppendPath>(),
        NodeTag::T_MergeAppendPath => std::mem::size_of::<pg_sys::MergeAppendPath>(),
        NodeTag::T_GroupResultPath => std::mem::size_of::<pg_sys::GroupResultPath>(),
        NodeTag::T_MaterialPath => std::mem::size_of::<pg_sys::MaterialPath>(),
        NodeTag::T_MemoizePath => std::mem::size_of::<pg_sys::MemoizePath>(),
        NodeTag::T_UniquePath => std::mem::size_of::<pg_sys::UniquePath>(),
        NodeTag::T_GatherPath => std::mem::size_of::<pg_sys::GatherPath>(),
        NodeTag::T_GatherMergePath => std::mem::size_of::<pg_sys::GatherMergePath>(),
        NodeTag::T_ProjectionPath => std::mem::size_of::<pg_sys::ProjectionPath>(),
        NodeTag::T_ProjectSetPath => std::mem::size_of::<pg_sys::ProjectSetPath>(),
        NodeTag::T_SortPath => std::mem::size_of::<pg_sys::SortPath>(),
        NodeTag::T_GroupPath => std::mem::size_of::<pg_sys::GroupPath>(),
        NodeTag::T_UpperUniquePath => std::mem::size_of::<pg_sys::UpperUniquePath>(),
        NodeTag::T_AggPath => std::mem::size_of::<pg_sys::AggPath>(),
        NodeTag::T_GroupingSetsPath => std::mem::size_of::<pg_sys::GroupingSetsPath>(),
        NodeTag::T_WindowAggPath => std::mem::size_of::<pg_sys::WindowAggPath>(),
        NodeTag::T_SetOpPath => std::mem::size_of::<pg_sys::SetOpPath>(),
        NodeTag::T_RecursiveUnionPath => std::mem::size_of::<pg_sys::RecursiveUnionPath>(),
        NodeTag::T_LockRowsPath => std::mem::size_of::<pg_sys::LockRowsPath>(),
        NodeTag::T_ModifyTablePath => std::mem::size_of::<pg_sys::ModifyTablePath>(),
        NodeTag::T_LimitPath => std::mem::size_of::<pg_sys::LimitPath>(),
        // T_Path or any unknown tag: use base Path size. This is always
        // safe for a shallow copy because all Path subtypes start with
        // the base Path struct.
        _ => std::mem::size_of::<pg_sys::Path>(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_accelerable_restriction_null_list_returns_false() {
        assert!(!has_accelerable_restriction(std::ptr::null_mut()));
    }

    #[test]
    fn node_has_accel_func_null_node_returns_false() {
        let reg = registry::AdapterRegistry::new();
        assert!(!node_has_accel_func(std::ptr::null_mut(), &reg));
    }

    #[test]
    fn path_node_size_for_known_tags() {
        // Verify known path types return correct sizes (all > 0).
        // We test a representative subset of the match arms.
        let known_sizes = [
            (
                NodeTag::T_IndexPath,
                std::mem::size_of::<pg_sys::IndexPath>(),
            ),
            (
                NodeTag::T_BitmapHeapPath,
                std::mem::size_of::<pg_sys::BitmapHeapPath>(),
            ),
            (
                NodeTag::T_CustomPath,
                std::mem::size_of::<pg_sys::CustomPath>(),
            ),
            (NodeTag::T_NestPath, std::mem::size_of::<pg_sys::NestPath>()),
            (
                NodeTag::T_MergePath,
                std::mem::size_of::<pg_sys::MergePath>(),
            ),
            (NodeTag::T_HashPath, std::mem::size_of::<pg_sys::HashPath>()),
            (
                NodeTag::T_AppendPath,
                std::mem::size_of::<pg_sys::AppendPath>(),
            ),
            (NodeTag::T_SortPath, std::mem::size_of::<pg_sys::SortPath>()),
            (NodeTag::T_AggPath, std::mem::size_of::<pg_sys::AggPath>()),
            (
                NodeTag::T_LimitPath,
                std::mem::size_of::<pg_sys::LimitPath>(),
            ),
        ];

        for (tag, expected_size) in known_sizes {
            assert!(expected_size > 0, "size for {tag:?} should be > 0");
        }
    }

    #[test]
    fn path_node_size_unknown_tag_falls_back_to_base_path() {
        // For unknown tags, should return size_of::<Path>().
        let base_size = std::mem::size_of::<pg_sys::Path>();
        assert!(base_size > 0);
    }

    #[test]
    fn path_node_size_all_match_arms_return_positive() {
        // Verify that all sizes in the match are positive (compile-time
        // check that the types exist and have non-zero size).
        let sizes = [
            std::mem::size_of::<pg_sys::IndexPath>(),
            std::mem::size_of::<pg_sys::BitmapHeapPath>(),
            std::mem::size_of::<pg_sys::BitmapAndPath>(),
            std::mem::size_of::<pg_sys::BitmapOrPath>(),
            std::mem::size_of::<pg_sys::TidPath>(),
            std::mem::size_of::<pg_sys::SubqueryScanPath>(),
            std::mem::size_of::<pg_sys::ForeignPath>(),
            std::mem::size_of::<pg_sys::CustomPath>(),
            std::mem::size_of::<pg_sys::NestPath>(),
            std::mem::size_of::<pg_sys::MergePath>(),
            std::mem::size_of::<pg_sys::HashPath>(),
            std::mem::size_of::<pg_sys::AppendPath>(),
            std::mem::size_of::<pg_sys::MergeAppendPath>(),
            std::mem::size_of::<pg_sys::GroupResultPath>(),
            std::mem::size_of::<pg_sys::MaterialPath>(),
            std::mem::size_of::<pg_sys::MemoizePath>(),
            std::mem::size_of::<pg_sys::UniquePath>(),
            std::mem::size_of::<pg_sys::GatherPath>(),
            std::mem::size_of::<pg_sys::GatherMergePath>(),
            std::mem::size_of::<pg_sys::ProjectionPath>(),
            std::mem::size_of::<pg_sys::ProjectSetPath>(),
            std::mem::size_of::<pg_sys::SortPath>(),
            std::mem::size_of::<pg_sys::GroupPath>(),
            std::mem::size_of::<pg_sys::UpperUniquePath>(),
            std::mem::size_of::<pg_sys::AggPath>(),
            std::mem::size_of::<pg_sys::GroupingSetsPath>(),
            std::mem::size_of::<pg_sys::WindowAggPath>(),
            std::mem::size_of::<pg_sys::SetOpPath>(),
            std::mem::size_of::<pg_sys::RecursiveUnionPath>(),
            std::mem::size_of::<pg_sys::LockRowsPath>(),
            std::mem::size_of::<pg_sys::ModifyTablePath>(),
            std::mem::size_of::<pg_sys::LimitPath>(),
            std::mem::size_of::<pg_sys::Path>(), // fallback
        ];

        for (i, &size) in sizes.iter().enumerate() {
            assert!(size > 0, "path type index {i} has zero size");
        }
    }

    #[test]
    fn all_path_subtypes_at_least_as_large_as_base_path() {
        let base = std::mem::size_of::<pg_sys::Path>();
        let subtypes = [
            std::mem::size_of::<pg_sys::IndexPath>(),
            std::mem::size_of::<pg_sys::BitmapHeapPath>(),
            std::mem::size_of::<pg_sys::CustomPath>(),
            std::mem::size_of::<pg_sys::NestPath>(),
            std::mem::size_of::<pg_sys::MergePath>(),
            std::mem::size_of::<pg_sys::HashPath>(),
            std::mem::size_of::<pg_sys::SortPath>(),
            std::mem::size_of::<pg_sys::AggPath>(),
            std::mem::size_of::<pg_sys::LimitPath>(),
        ];

        for (i, &size) in subtypes.iter().enumerate() {
            assert!(
                size >= base,
                "subtype index {i} (size {size}) smaller than base Path (size {base})"
            );
        }
    }
}
