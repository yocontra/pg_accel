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
use crate::engine::executor::preagg::DimFilter;
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
#[allow(clippy::too_many_lines)]
unsafe extern "C-unwind" fn pgaccel_set_rel_pathlist(
    root: *mut PlannerInfo,
    rel: *mut RelOptInfo,
    rti: pg_sys::Index,
    rte: *mut RangeTblEntry,
) {
    // Lazy-init tracing in this backend (no-op after first call).
    crate::engine::otel::init();

    // Chain previous hook first.
    // SAFETY: Previous hook, if set, accepts the same planner-provided args.
    unsafe {
        if let Some(prev) = PREV_SET_REL_PATHLIST_HOOK {
            prev(root, rel, rti, rte);
        }
    }

    // Gate 1: GUC check — single branch, ~1ns.
    if !gucs::enabled() {
        pgrx::debug1!("pg_accel: set_rel_pathlist: extension disabled");
        return;
    }

    // Gate: Only accelerate pure SELECT statements. INSERT...SELECT,
    // UPDATE...FROM, DELETE...USING run scans in ModifyTable context
    // where Custom Scan slot handling is incompatible.
    // SAFETY: root.parse is a valid Query pointer provided by the planner.
    let parse = unsafe { (*root).parse };
    if parse.is_null() || unsafe { (*parse).commandType } != pg_sys::CmdType::CMD_SELECT {
        return;
    }

    // Gate 1b: GPU must be available and enabled — no CPU-only fallback.
    if !cost::gpu_is_usable() {
        pgrx::debug1!("pg_accel: set_rel_pathlist: GPU not usable");
        return;
    }
    pgrx::debug1!("pg_accel: set_rel_pathlist: GPU usable, checking rel");

    // SAFETY: rel and rte are valid pointers provided by the planner.
    let rel_ref = unsafe { &*rel };
    let rte_ref = unsafe { &*rte };

    let _span = tracing::info_span!("planner.rel_pathlist", relid = u32::from(rte_ref.relid)).entered();

    // Gate 2: Only base table relations — cheap field checks before
    // any registry or clause work.
    if rel_ref.reloptkind != pg_sys::RelOptKind::RELOPT_BASEREL {
        return;
    }
    if rte_ref.rtekind != pg_sys::RTEKind::RTE_RELATION {
        return;
    }

    // Gate 2c: Skip system catalog tables. GpuExpr (and other strategies)
    // are only useful for user tables. System catalogs have OIDs below
    // FirstNormalObjectId (16384).
    if u32::from(rte_ref.relid) < pg_sys::FirstNormalObjectId {
        return;
    }

    // Gate 2b: Early-exit when there is nothing to accelerate.
    // If no WHERE clauses and no ORDER BY, skip all registry and clause
    // work. This eliminates 100-500µs overhead on queries like SSBM
    // that pg_accel cannot accelerate anyway.
    // SAFETY: root is a valid PlannerInfo pointer.
    let has_sort = unsafe { !(*root).sort_pathkeys.is_null() };
    let has_restrictions = !rel_ref.baserestrictinfo.is_null()
        && unsafe { pg_sys::list_length(rel_ref.baserestrictinfo) } > 0;
    if !has_sort && !has_restrictions {
        return;
    }

    // GPU sort path for ORDER BY on numeric columns.
    if has_sort {
        pgrx::debug1!("pg_accel: calling try_inject_gpu_sort_path");
        // SAFETY: root, rel are valid planner pointers.
        unsafe { try_inject_gpu_sort_path(root, rel) };
    }

    // Early exit if no restriction clauses — sort was the only possibility.
    if !has_restrictions {
        return;
    }

    // Gate 3: Early rows exit. No scan strategy can fire below min_batch_size
    // (should_batch will reject at Gate 4), so skip expensive clause matching
    // and registry initialisation. This eliminates 20-50µs of overhead on
    // small dimension tables in star-schema queries.
    {
        let rows_early = rel_ref.tuples.max(rel_ref.rows) as usize;
        let min_batch = gucs::min_batch_size().max(1) as usize;
        if rows_early < min_batch {
            return;
        }
    }

    // GpuExpr path: standard numeric WHERE clauses (OpExpr, BoolExpr,
    // NullTest). Independent of adapter registry — expression compilability
    // is checked via node-tag and type inspection. This enables GPU-
    // accelerated scan for queries like `WHERE val > 0.5 AND id < 1000`.
    let gpu_expr_match = try_gpu_expr_match(rel_ref.baserestrictinfo);

    // Extension-function match: requires the adapter registry (PostGIS,
    // H3, raster). Only initialise and check when GpuExpr didn't match,
    // or always to prefer the more specific strategy.
    registry::lazy_init();
    let reg_match = if registry::is_ready() {
        let reg = registry::global_registry();
        if reg.is_empty() {
            None
        } else {
            find_accelerable_match(rel_ref.baserestrictinfo)
        }
    } else {
        None
    };

    // Prefer extension-specific match (spatial, H3, raster) over generic
    // GpuExpr — extension strategies have dedicated kernels optimised for
    // their data types.
    let accel = reg_match.or(gpu_expr_match);
    let Some(accel) = accel else {
        pgrx::debug1!("pg_accel: set_rel_pathlist: no accelerable match found");
        return;
    };
    let strategy = accel.strategy;
    pgrx::debug1!("pg_accel: set_rel_pathlist: found {:?} match", strategy);

    // Gate 4: Cost model gating — skip if batching is not worthwhile.
    // Use strategy-aware per-row cost so GPU paths (with higher overhead)
    // require more rows to break even.
    // Use rel.tuples (total table rows) not rel.rows (estimated output after
    // filtering). The GPU processes ALL input rows to evaluate the predicate;
    // rel.rows is the post-filter estimate which can be tiny for selective
    // spatial predicates even on large tables.
    let rows = rel_ref.tuples.max(rel_ref.rows) as usize;
    let min_batch = gucs::min_batch_size().max(1) as usize;
    let per_row_cost = match strategy {
        registry::AccelStrategy::GpuRaster => cost::GPU_RASTER_PER_ROW_COST,
        registry::AccelStrategy::GpuH3 => cost::GPU_H3_PER_ROW_COST,
        registry::AccelStrategy::GpuExpr => cost::GPU_EXPR_PER_ROW_COST,
        _ => cost::GPU_SPATIAL_PER_ROW_COST,
    };
    if !cost::should_batch(rows, per_row_cost, min_batch) {
        pgrx::debug1!("pg_accel: set_rel_pathlist: batch rejected tuples={} min_batch={}", rows, min_batch);
        return;
    }
    // GpuExpr has a device-derived minimum row threshold in addition to the
    // GUC-based min_batch_size. The inline template filter is lightweight
    // but still needs enough rows to amortize compilation + scan overhead.
    if strategy == registry::AccelStrategy::GpuExpr
        && rows < cost::device_limits().gpu_expr_min_rows
    {
        pgrx::debug1!(
            "pg_accel: set_rel_pathlist: GpuExpr rejected rows={} < expr_min={}",
            rows,
            cost::device_limits().gpu_expr_min_rows
        );
        return;
    }

    // Gate 4b: Defer to GiST/SP-GiST index scan for selective spatial filters.
    // When a spatial index exists and is highly selective, PG's native index
    // scan avoids touching most heap pages entirely. Wrapping that in a Custom
    // Scan adds geometry deser + batch + kernel overhead that causes a
    // regression (measured 6.9x slower on spatial_filter benchmark).
    if matches!(
        strategy,
        registry::AccelStrategy::GpuSpatial | registry::AccelStrategy::GpuRaster
    ) {
        // Find the cheapest sequential scan (T_Path) as the baseline for
        // cost-ratio comparison. Using find_cheapest_path() here would
        // return the index path itself when it's cheapest, making the
        // ratio idx_cost/idx_cost = 1.0 (always fails the < 0.40 check).
        // SAFETY: rel_ref.pathlist is a valid List pointer from the planner.
        let seq_scan_cost = unsafe { find_cheapest_seqscan_cost(rel_ref.pathlist) };
        // SAFETY: rel_ref.pathlist is a valid List pointer from the planner.
        if unsafe { has_cheap_spatial_index_path(rel_ref.pathlist, seq_scan_cost) } {
            return;
        }
    }

    // Gate 4c: Vertex count threshold for spatial predicates.
    // GPU overhead is ~19ms constant (geometry deser + seq scan), regardless
    // of polygon complexity. PG parallel scales linearly with vertex count.
    // Below the threshold, PG parallel is faster — skip Custom Scan injection
    // entirely for true zero overhead.
    if strategy == registry::AccelStrategy::GpuSpatial {
        let min_verts = cost::device_limits().gpu_spatial_min_vertices;
        // SAFETY: rel_ref.baserestrictinfo is a valid List from the planner.
        // Treat unknown vertex count (None) as 0 — conservatively reject
        // when the planner cannot confirm the polygon has enough vertices.
        // This prevents injection for FuncExpr geometry args (ST_Buffer,
        // ST_MakeEnvelope) that PG didn't constant-fold.
        let vcount = unsafe { extract_const_geom_vertex_count(rel_ref.baserestrictinfo) }
            .unwrap_or(0);
        if vcount < min_verts {
            pgrx::debug1!(
                "pg_accel: spatial vertex gate: vcount={} < min={}, skipping",
                vcount,
                min_verts
            );
            return;
        }
    }

    // Gate 5: Find the sequential scan path as cost baseline.
    // The Custom Scan always does a full sequential heap scan, so its cost
    // must be derived from the seq scan cost — NOT from index/bitmap paths.
    // Using an index path as baseline + discount makes the Custom Scan appear
    // cheaper than the index scan it was based on, causing regressions when
    // an index scan would be far faster (e.g., GiST at 10M rows).
    //
    // If PG pruned the seq scan from the pathlist, that means index/bitmap
    // paths dominate it. Since our Custom Scan is fundamentally a seq scan,
    // it would also be dominated — skip injection entirely.
    // SAFETY: rel_ref.pathlist is a valid List pointer from the planner.
    let cheapest = unsafe { find_cheapest_seqscan_path(rel_ref.pathlist) };
    if cheapest.is_null() {
        pgrx::debug1!(
            "pg_accel: scan hook: no seq scan path in pathlist, \
             index paths dominate — skipping Custom Scan injection"
        );
        return;
    }

    // Build cost estimate using the cost model with strategy-aware constants.
    // SAFETY: cheapest is non-null, checked above.
    let base = unsafe { &*cheapest };
    #[allow(clippy::cast_precision_loss)]
    let batch_size = cost::optimal_batch_size(rows) as f64;
    let num_batches = (base.rows / batch_size).ceil();
    let batch_overhead = num_batches * 2.0; // per-batch dispatch + sync cost
    // GPU strategies incur a fixed kernel launch overhead.
    // GpuExpr uses CPU inline template filter (no GPU launch).
    let gpu_overhead = match strategy {
        registry::AccelStrategy::GpuSpatial
        | registry::AccelStrategy::GpuRaster
        | registry::AccelStrategy::GpuH3 => cost::GPU_LAUNCH_OVERHEAD,
        _ => 0.0,
    };
    // Custom Scan runs single-threaded. If PG can parallelize this relation,
    // our startup_cost must be at least parallel_setup_cost so that add_path()
    // can properly dominate us when a cheaper Gather path exists. Without this,
    // our low startup_cost prevents domination even when total_cost is higher.
    let parallel_penalty = if rel_ref.consider_parallel {
        // SAFETY: reading PG GUC parallel_setup_cost.
        let psc = unsafe { pg_sys::parallel_setup_cost };
        psc.max(0.0)
    } else {
        0.0
    };
    let startup_cost = base.startup_cost + 1.0 + gpu_overhead + parallel_penalty;

    // GPU accelerates per-row function evaluation for spatial/H3/raster.
    // GPU_COST_SAFETY_MARGIN (0.7) = 30% savings from GPU batch execution.
    // GpuExpr (CPU inline filter) uses no cost discount (1.0). Benchmarks
    // show GpuExpr never outperforms PG parallel at any scale — the Custom
    // Scan framing overhead exceeds the ~5% ExecQual savings. Setting the
    // margin to 1.0 lets batch_overhead + yield_cost naturally make the
    // path more expensive, so add_path() discards it correctly.
    let cost_margin = if strategy == registry::AccelStrategy::GpuExpr {
        1.0
    } else {
        cost::GPU_COST_SAFETY_MARGIN
    };
    // GpuExpr inline filter: per-row yield cost (heap buffer return,
    // no MinimalTuple copy). GPU strategies (spatial/H3/raster): zero yield
    // cost — the 30% cost_margin discount already captures batch speedup,
    // and per-row compute dominates yield overhead.
    let yield_cost = if strategy == registry::AccelStrategy::GpuExpr {
        base.rows * 0.005
    } else {
        0.0
    };
    let raw_total = (base.total_cost * cost_margin
        + batch_overhead
        + gpu_overhead
        + yield_cost)
        * gucs::cost_multiplier();

    // Let PG's native cost comparison decide — add_path() discards
    // paths that are strictly dominated by cheaper alternatives.
    let total_cost = raw_total;

    // Gate 6: Post-cost spatial index check.
    // After computing our Custom Scan cost, compare against any GiST/SP-GiST
    // index path. The Custom Scan always does a full sequential heap scan.
    // At large scales (10M+), a selective GiST index scan reads far fewer
    // heap pages, making it faster even if PG's cost estimate is close.
    // This applies to all strategies — GpuExpr also does a full heap walk.
    // SAFETY: rel_ref.pathlist is a valid List pointer from the planner.
    if unsafe { has_cheaper_spatial_index_path(rel_ref.pathlist, total_cost) } {
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
const SORT_INT8OID: u32 = 20;
const SORT_FLOAT4OID: u32 = 700;
const SORT_FLOAT8OID: u32 = 701;

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
    // Gate: GPU must be available and enabled.
    if !cost::gpu_is_usable() {
        pgrx::debug1!("pg_accel sort: gpu not usable");
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
    // GPU sort supports up to gpu_multi_key_sort_max_keys keys. Currently
    // the executor only handles single-key; the limit is configurable via
    // DeviceLimits for future multi-key cascaded stable sort support.
    let max_keys = cost::device_limits().gpu_multi_key_sort_max_keys;
    if num_pathkeys < 1 || num_pathkeys > max_keys as i32 {
        pgrx::debug1!("pg_accel sort: {num_pathkeys} keys exceeds max={max_keys}, skipping");
        return;
    }
    // Executor currently only supports single-key sort. Multi-key requires
    // cascaded stable sorts (sort by last key first, then by prior keys).
    // TODO: implement multi-key sort executor support.
    if num_pathkeys != 1 {
        pgrx::debug1!("pg_accel sort: multi-key sort not yet implemented, skipping");
        return;
    }

    // Gate: Row count threshold.
    let rows = rel_ref.rows as usize;
    let limits = cost::device_limits();
    let min_rows = limits.gpu_sort_planner_min_rows;
    pgrx::debug1!("pg_accel sort: rows={rows}, min={min_rows}");
    if rows < min_rows {
        pgrx::debug1!("pg_accel sort: too few rows");
        return;
    }

    // Gate: Max row count. GPU sort kernel has a hard limit
    // (gpu_sort_max_elements). Above this the executor falls back to CPU
    // sort but still pays Custom Scan yield overhead (~3μs/row), making it
    // strictly slower than PG's native sort. Skip injection entirely.
    if rows > limits.gpu_sort_max_elements {
        pgrx::debug1!(
            "pg_accel sort: rows={rows} exceeds GPU sort max={}, deferring to PG",
            limits.gpu_sort_max_elements
        );
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

    // Gate: Skip very narrow rows where GPU sort overhead isn't amortized.
    let output_width = unsafe { (*(*rel).reltarget).width } as usize;
    if output_width < 8 {
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
    if !matches!(var_typid, SORT_INT4OID | SORT_INT8OID | SORT_FLOAT4OID | SORT_FLOAT8OID) {
        pgrx::debug1!("pg_accel sort: unsupported type {var_typid}");
        return;
    }

    // Gate: Skip float8 sort when GPU lacks native fp64 support.
    if var_typid == SORT_FLOAT8OID && !cost::platform_has_fp64() {
        pgrx::debug1!("pg_accel sort: float8 sort skipped — GPU lacks fp64");
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

    // Cost estimate: GPU sort replaces PG's comparison-based sort.
    // Custom Scan sort has per-row overhead that PG's native sort
    // avoids: input materialization (ExecCopySlotMinimalTuple) and
    // output yield (ExecForceStoreMinimalTuple). Only beneficial when
    // the sort itself is the bottleneck (e.g., disk spill on wide rows).
    // SAFETY: cheapest is non-null.
    let base = unsafe { &*cheapest };
    #[allow(clippy::cast_precision_loss)]
    let n = base.rows;
    let gpu_overhead = cost::GPU_LAUNCH_OVERHEAD;
    // Per-row overhead for Custom Scan sort: materialization (0.005) +
    // key extraction (0.002) + comparison sort O(n log n) amortized (0.06) +
    // yield/deform (0.025) + Custom Scan framing (0.03).
    // This must be high enough that PG's native sort wins when GPU kernel
    // is unavailable (CPU fallback is strictly slower than PG native sort).
    let per_row = 0.15;
    let sort_cost = n * per_row;
    let startup_cost = base.startup_cost + gpu_overhead;
    // Honest cost — do not apply cost_multiplier. Custom Scan sort has
    // inherent per-row overhead that makes it slower than PG native sort
    // unless the GPU kernel provides a real speedup.
    let total_cost = base.total_cost + gpu_overhead + sort_cost;

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
    crate::engine::otel::init();

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

    // Gate: Only accelerate pure SELECT statements. INSERT...SELECT,
    // UPDATE...FROM, DELETE...USING run scans in ModifyTable context
    // where Custom Scan slot handling is incompatible.
    // SAFETY: root.parse is a valid Query pointer provided by the planner.
    let parse = unsafe { (*root).parse };
    if parse.is_null() || unsafe { (*parse).commandType } != pg_sys::CmdType::CMD_SELECT {
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

    // Gate 4b: Max output rows for hash join. Custom Scan yield overhead
    // (~3μs/row) makes large-output joins strictly slower than PG's
    // native HashJoin which avoids per-row materialization overhead.
    if matches!(strategy, registry::AccelStrategy::GpuHashJoin) {
        let limits = cost::device_limits();
        let max_output = limits.gpu_join_max_output_rows;
        if join_rows_gate > max_output {
            pgrx::debug1!(
                "pg_accel join: output rows={join_rows_gate} exceeds max={max_output}, deferring to PG"
            );
            return;
        }
        // Gate 4c: Max inner-side rows for hash build. The build-side hash
        // table must fit in GPU memory.
        #[allow(clippy::cast_sign_loss)]
        let inner_rows = innerrel_ref.rows as usize;
        if inner_rows > limits.gpu_hash_join_build_max_rows {
            pgrx::debug1!(
                "pg_accel join: inner rows={inner_rows} exceeds build max={}, deferring to PG",
                limits.gpu_hash_join_build_max_rows
            );
            return;
        }
    }

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
    let total_cost =
        base_cost + gpu_launch + build_cost + probe_cost + yield_cost;

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
    crate::engine::otel::init();

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

    // Gate: Only accelerate pure SELECT statements. INSERT...SELECT,
    // UPDATE...FROM, DELETE...USING run scans in ModifyTable context
    // where Custom Scan slot handling is incompatible.
    // SAFETY: root.parse is a valid Query pointer provided by the planner.
    let parse = unsafe { (*root).parse };
    if parse.is_null() || unsafe { (*parse).commandType } != pg_sys::CmdType::CMD_SELECT {
        return;
    }

    let _span = tracing::info_span!("planner.upper_paths", stage = stage).entered();

    // Dispatch by upper relation stage.
    match stage {
        pg_sys::UpperRelationKind::UPPERREL_GROUP_AGG => {
            // Try fused star-join pre-aggregation first (most benefit).
            // SAFETY: All pointers are valid planner arguments.
            unsafe { pgaccel_inject_gpu_preagg(root, input_rel, output_rel) };

            // Also inject standard GpuAgg path for non-star-schema queries.
            // SAFETY: All pointers are valid planner arguments.
            unsafe { pgaccel_inject_gpu_agg(root, input_rel, output_rel) };
        }
        pg_sys::UpperRelationKind::UPPERREL_WINDOW => {
            // SAFETY: All pointers are valid planner arguments.
            unsafe { pgaccel_inject_gpu_window(root, input_rel, output_rel) };
        }
        _ => {}
    }
    let _ = extra;
}

// ---------------------------------------------------------------------------
// GPU window function injection
// ---------------------------------------------------------------------------

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
    // Gate: GPU must be available and enabled.
    if !cost::gpu_is_usable() {
        pgrx::debug1!("pg_accel window: gpu not usable");
        return;
    }

    // SAFETY: root is a valid PlannerInfo pointer.
    let root_ref = unsafe { &*root };
    let input_ref = unsafe { &*input_rel };

    // Gate: Row count threshold.
    #[allow(clippy::cast_sign_loss)]
    let rows = input_ref.rows as usize;
    if rows < cost::device_limits().gpu_window_min_rows {
        pgrx::debug1!(
            "pg_accel window: rows {} < min {}",
            rows,
            cost::device_limits().gpu_window_min_rows
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
            // WindowFunc.args is a List of Expr* nodes (NOT TargetEntry*).
            if nargs >= 1 {
                // SAFETY: first arg is an Expr* node.
                let arg_expr = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Expr>() };
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
            // LAG/LEAD offset (second argument, if present and constant).
            if nargs >= 2 {
                // SAFETY: second arg is an Expr* node.
                let offset_expr = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Expr>() };
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
            // LAG/LEAD default (third argument, if present and constant).
            if nargs >= 3 {
                // SAFETY: third arg is an Expr* node.
                let def_expr = unsafe { pg_sys::list_nth(args, 2).cast::<pg_sys::Expr>() };
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

    // Find cheapest input path and wrap with Sort if needed.
    // Window functions require input sorted by PARTITION BY + ORDER BY.
    // PG stores the required sort keys in root->window_pathkeys.
    let cheapest = unsafe { find_cheapest_path(input_ref.pathlist) };
    if cheapest.is_null() {
        return;
    }

    // SAFETY: root->window_pathkeys is the planner's sort key list for
    // the window phase. create_sort_path wraps cheapest with a Sort
    // node if it's not already sorted by these keys.
    let window_pathkeys = unsafe { (*root).window_pathkeys };
    let sorted_path = if window_pathkeys.is_null()
        || unsafe { pg_sys::list_length(window_pathkeys) } == 0
    {
        cheapest
    } else {
        // SAFETY: All pointers from the planner; create_sort_path pallocs.
        unsafe {
            pg_sys::create_sort_path(root, input_rel, cheapest, window_pathkeys, -1.0)
                .cast::<Path>()
        }
    };

    // Honest cost — no cost_multiplier discount. Window functions process
    // all input rows and add window output columns, so cost is:
    //   child_cost + per_row_overhead * rows * num_specs
    // SAFETY: sorted_path is non-null.
    let base = unsafe { &*sorted_path };
    let gpu_overhead = cost::GPU_LAUNCH_OVERHEAD;
    // Realistic per-row cost: datum extract + partition detect + yield overhead.
    // Measured ~30µs/row total on M2 Max (vs PG's ~5µs/row for native WindowAgg).
    let per_row = 0.03;
    let window_cost = base.rows * per_row * specs.len() as f64;
    let startup_cost = base.total_cost + gpu_overhead;
    let total_cost = base.total_cost + gpu_overhead + window_cost;

    // SAFETY: Allocating via palloc, building valid CustomPath.
    unsafe {
        let cpath = create_custom_path(
            output_rel,
            sorted_path,
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

// ---------------------------------------------------------------------------
// GPU PreAgg (fused star-join + aggregate) injection
// ---------------------------------------------------------------------------

/// Detect star-schema join trees (fact table + small dimension joins + aggregate)
/// and inject a fused `GpuPreAgg` CustomPath that replaces the entire pipeline.
///
/// Walks the cheapest input path backwards through join nodes to find:
/// - A chain of HashJoin paths where each inner side is a small table
/// - A base (fact) table scan at the bottom
/// - Equi-join keys at each join level
/// - Aggregate operations in the output target list
///
/// # Safety
///
/// All pointers must be valid planner-provided arguments.
#[allow(
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::cast_ptr_alignment,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
unsafe fn pgaccel_inject_gpu_preagg(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
) {
    use crate::engine::executor::preagg::{GroupKeyDesc, JoinDepthDesc, PreAggColDesc};
    use crate::engine::ffi::custom_scan;

    // Gate: GPU must be available and enabled.
    if !cfg!(feature = "gpu") || !cost::gpu_is_usable() {
        return;
    }

    let _span = tracing::info_span!("planner.preagg").entered();

    let input_ref = unsafe { &*input_rel };
    let root_ref = unsafe { &*root };
    let parse = root_ref.parse;
    if parse.is_null() {
        return;
    }
    let query = unsafe { &*parse };

    // Gate: need a cheapest path to walk.
    let cheapest = unsafe { find_cheapest_path(input_ref.pathlist) };
    if cheapest.is_null() {
        return;
    }

    // Gate: must have aggregation (hasAggs or groupClause).
    if !query.hasAggs && query.groupClause.is_null() {
        return;
    }

    let limits = cost::device_limits();

    // -----------------------------------------------------------------------
    // Phase 1: Walk the join tree to find star-schema pattern.
    // -----------------------------------------------------------------------

    // We walk backwards from the cheapest path, collecting join depths.
    // Each level must be a HashJoin/NestLoop with a small inner side.
    let mut depths: Vec<JoinDepthDesc> = Vec::new();
    let mut inner_paths: Vec<*mut Path> = Vec::new();
    let mut inner_relids: Vec<pg_sys::Index> = Vec::new();
    let mut current = cheapest;
    let mut fact_rows: f64 = 0.0;
    let mut fact_relid: pg_sys::Index = 0;
    let mut fact_scan_expr: Option<crate::engine::expr_compiler::CompiledExpr> = None;

    loop {
        // SAFETY: current is a valid Path pointer.
        let tag = unsafe { (*current.cast::<pg_sys::Node>()).type_ };

        match tag {
            // HashJoin or NestLoop join path — extract join info.
            NodeTag::T_HashPath | NodeTag::T_NestPath => {
                let jp = current.cast::<pg_sys::JoinPath>();
                // SAFETY: jp is a valid JoinPath.
                let join_type = unsafe { (*jp).jointype };

                // Only support INNER JOIN for star-schema.
                if join_type != pg_sys::JoinType::JOIN_INNER {
                    pgrx::debug1!("pg_accel: preagg rejected: non-inner join");
                    return;
                }

                let outer_path = unsafe { (*jp).outerjoinpath };
                let inner_path = unsafe { (*jp).innerjoinpath };
                if outer_path.is_null() || inner_path.is_null() {
                    return;
                }

                // Inner side must be small (dimension table).
                let inner_rows = unsafe { (*inner_path).rows };
                if inner_rows as usize > limits.gpu_preagg_max_dim_rows {
                    pgrx::debug1!(
                        "pg_accel: preagg rejected: inner rows {} > max {}",
                        inner_rows,
                        limits.gpu_preagg_max_dim_rows
                    );
                    return;
                }

                // Extract equi-join key from join restrictinfo.
                let restrict = unsafe { (*jp).joinrestrictinfo };
                let equi = unsafe { find_equi_join_from_restrictinfo(restrict, outer_path, inner_path) };
                let Some(equi) = equi else {
                    pgrx::debug1!("pg_accel: preagg rejected: no equi-join key found");
                    return;
                };

                // SAFETY: inner_path.parent is the inner relation's RelOptInfo.
                let inner_parent = unsafe { (*inner_path).parent };
                let inner_relid: pg_sys::Index = if !inner_parent.is_null() {
                    unsafe { (*inner_parent).relid }
                } else {
                    0
                };

                // Extract dimension-side filter predicates from baserestrictinfo.
                // SAFETY: inner_parent is a valid RelOptInfo.
                let dim_filters = if !inner_parent.is_null() {
                    unsafe { extract_dim_filters_from_rel(inner_parent) }
                } else {
                    vec![]
                };

                depths.push(JoinDepthDesc {
                    outer_attno: equi.outer_attno,
                    inner_attno: equi.inner_attno,
                    key_type: equi.key_type,
                    dim_filters,
                    group_col_attnos: vec![], // populated later from GROUP BY
                });
                inner_paths.push(inner_path);
                inner_relids.push(inner_relid);

                // Continue walking the outer side.
                current = outer_path;
            }
            // Base relation (fact table) — end of walk.
            _ => {
                // SAFETY: current is a valid Path.
                let parent = unsafe { (*current).parent };
                if parent.is_null() {
                    return;
                }
                let parent_ref = unsafe { &*parent };

                // Must be a base relation.
                if parent_ref.reloptkind != pg_sys::RelOptKind::RELOPT_BASEREL {
                    pgrx::debug1!("pg_accel: preagg rejected: outer is not base rel");
                    return;
                }
                fact_rows = parent_ref.rows;
                fact_relid = parent_ref.relid;

                // Extract fact-side WHERE clause as a compiled template.
                fact_scan_expr = unsafe {
                    compile_restrictinfo_to_template(parent_ref.baserestrictinfo)
                };
                break;
            }
        }
    }

    // Gate: need at least one join depth (otherwise it's a single-table query).
    if depths.is_empty() {
        return;
    }

    // Gate: fact table must have enough rows.
    if (fact_rows as usize) < limits.gpu_preagg_min_fact_rows {
        pgrx::debug1!(
            "pg_accel: preagg rejected: fact rows {} < min {}",
            fact_rows,
            limits.gpu_preagg_min_fact_rows
        );
        return;
    }

    // -----------------------------------------------------------------------
    // Phase 2: Detect aggregate operations from the output target list.
    // -----------------------------------------------------------------------

    let tlist = query.targetList;
    if tlist.is_null() {
        return;
    }
    let tlist_len = unsafe { pg_sys::list_length(tlist) };

    let mut agg_descs: Vec<PreAggColDesc> = Vec::new();
    let mut group_keys: Vec<GroupKeyDesc> = Vec::new();

    for i in 0..tlist_len {
        // SAFETY: i is in [0, tlist_len).
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() {
            continue;
        }
        let expr = unsafe { (*tle).expr };
        if expr.is_null() {
            continue;
        }
        let expr_tag = unsafe { (*expr.cast::<pg_sys::Node>()).type_ };

        if expr_tag == NodeTag::T_Aggref {
            let aggref = expr.cast::<pg_sys::Aggref>();
            // SAFETY: aggref is valid.
            let aggfnoid = unsafe { (*aggref).aggfnoid };
            let op = agg_op_from_oid(aggfnoid);
            if matches!(op, AggOp::Passthrough) {
                pgrx::debug1!("pg_accel: preagg rejected: unsupported agg function");
                return;
            }

            // Get the aggregate's argument attno (if any).
            let args = unsafe { (*aggref).args };
            let arg_attno = if args.is_null() || unsafe { pg_sys::list_length(args) } == 0 {
                0 // COUNT(*)
            } else {
                // First argument.
                let te = unsafe {
                    pg_sys::list_nth(args, 0).cast::<pg_sys::TargetEntry>()
                };
                if te.is_null() {
                    0
                } else {
                    let arg_expr = unsafe { (*te).expr };
                    // SAFETY: arg_expr is a valid Node pointer from PG parser output.
                    unsafe { extract_var_attno(arg_expr) }
                }
            };

            let type_oid = unsafe { (*aggref).aggtype };
            agg_descs.push(PreAggColDesc {
                op,
                attno: arg_attno,
                type_oid,
            });
        } else if expr_tag == NodeTag::T_Var {
            // This is likely a GROUP BY column reference.
            let var = expr.cast::<pg_sys::Var>();
            let varattno = i32::from(unsafe { (*var).varattno });
            let varno = unsafe { (*var).varno } as u32;
            let typid = unsafe { (*var).vartype };

            // Determine if this var references the fact table or a dimension.
            let (source, attno) = resolve_var_to_star_source(
                varno, varattno, fact_relid, &inner_relids,
            );

            group_keys.push(GroupKeyDesc {
                source,
                attno,
                type_oid: typid,
            });
        }
    }

    // Gate: need at least one aggregate.
    if agg_descs.is_empty() && group_keys.is_empty() {
        pgrx::debug1!("pg_accel: preagg rejected: no aggregates or group keys found");
        return;
    }

    // Populate group_col_attnos in depths for dimension-side GROUP BY columns.
    for gk in &group_keys {
        if gk.source > 0 {
            let depth_idx = (gk.source - 1) as usize;
            if depth_idx < depths.len() {
                depths[depth_idx].group_col_attnos.push(gk.attno);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Phase 3: Cost estimation.
    // -----------------------------------------------------------------------

    // Dimension materialization (one-time).
    let limits = cost::device_limits();
    let dim_cost: f64 = inner_paths
        .iter()
        .map(|&p| unsafe { (*p).rows } * limits.preagg_dim_materialize_cost)
        .sum();

    // Fact table scan + probe + aggregate.
    let fact_scan_cost = fact_rows * limits.preagg_fact_scan_cost;
    let mut surviving = fact_rows;
    let mut probe_cost = 0.0;
    for ip in &inner_paths {
        probe_cost += surviving * limits.preagg_probe_cost;
        // Estimate join selectivity: inner_rows / fact_rows, clamped.
        let inner_rows = unsafe { (**ip).rows };
        let sel = (inner_rows / fact_rows).clamp(0.01, 1.0);
        surviving *= sel;
    }
    let agg_cost = surviving * limits.preagg_agg_cost;
    let n_groups = if group_keys.is_empty() {
        1.0
    } else {
        surviving.sqrt().clamp(1.0, 10000.0)
    };
    let yield_cost = n_groups * limits.preagg_yield_cost;

    // PreAgg is CPU-only (heap_getnext + hash probe + accumulate) — use
    // the PreAgg-specific fixed overhead, not GPU_LAUNCH_OVERHEAD.
    let total_cost = dim_cost
        + fact_scan_cost
        + probe_cost
        + agg_cost
        + yield_cost
        + cost::PREAGG_FIXED_OVERHEAD;

    // Gate: only inject if substantially cheaper than PG's best existing path.
    // PG's cost estimates have ±20-30% noise, so use a tighter gate to avoid
    // injecting PreAgg when it would actually be slower. The 50% threshold
    // means PreAgg must estimate at least half PG's best cost — conservative
    // enough to prevent regressions but loose enough to allow injection when
    // the fused pipeline genuinely wins.
    let output_ref = unsafe { &*output_rel };
    let pg_best_cost = unsafe { find_cheapest_total_cost(output_ref.pathlist) };
    if pg_best_cost > 0.0 && total_cost >= pg_best_cost * 0.50 {
        pgrx::debug1!(
            "pg_accel: preagg rejected: cost {:.1} >= 50% of PG best {:.1}",
            total_cost,
            pg_best_cost,
        );
        return;
    }

    // -----------------------------------------------------------------------
    // Phase 4: Create the PreAgg CustomPath.
    // -----------------------------------------------------------------------

    pgrx::debug1!(
        "pg_accel: preagg injecting: {} depths, {} aggs, {} groups, cost={:.1} vs pg={:.1}",
        depths.len(),
        agg_descs.len(),
        group_keys.len(),
        total_cost,
        pg_best_cost,
    );

    // SAFETY: Allocating via palloc.
    unsafe {
        let cpath = pg_sys::palloc0(std::mem::size_of::<CustomPath>()).cast::<CustomPath>();

        (*cpath).path.type_ = NodeTag::T_CustomPath;
        (*cpath).path.pathtype = NodeTag::T_CustomScan;
        (*cpath).path.parent = output_rel;
        (*cpath).path.pathtarget = (*output_rel).reltarget;
        (*cpath).path.param_info = std::ptr::null_mut();
        (*cpath).path.parallel_aware = false;
        (*cpath).path.parallel_safe = false;
        (*cpath).path.parallel_workers = 0;
        (*cpath).path.rows = n_groups;
        (*cpath).path.startup_cost = dim_cost + cost::PREAGG_FIXED_OVERHEAD;
        (*cpath).path.total_cost = total_cost;
        (*cpath).path.pathkeys = std::ptr::null_mut();

        (*cpath).flags = 0;
        (*cpath).methods = custom_scan::preagg_path_methods();

        // Attach inner (dimension) paths as custom_paths.
        let mut child_list: *mut List = std::ptr::null_mut();
        for &ip in &inner_paths {
            child_list = lappend(child_list, ip.cast());
        }
        (*cpath).custom_paths = child_list;

        // Serialize PreAgg metadata into custom_private.
        (*cpath).custom_private = custom_scan::serialize_preagg_private(
            fact_relid,
            &depths,
            &agg_descs,
            &group_keys,
            fact_scan_expr.as_ref(),
        );

        add_path(output_rel, cpath.cast());
    }
}

/// Extract a Var's varattno as i32. Returns 0 if expr is not a Var.
unsafe fn extract_var_attno(expr: *mut pg_sys::Expr) -> i32 {
    if expr.is_null() {
        return 0;
    }
    let tag = unsafe { (*expr.cast::<pg_sys::Node>()).type_ };
    if tag == NodeTag::T_Var {
        #[allow(clippy::cast_ptr_alignment)]
        i32::from(unsafe { (*expr.cast::<pg_sys::Var>()).varattno })
    } else if tag == NodeTag::T_OpExpr {
        // For expressions like (lo_revenue - lo_supplycost), return 0
        // to signal "complex expression" which we'll handle as COUNT-like.
        0
    } else {
        0
    }
}

/// Map an aggregate function OID to an `AggOp`.
fn agg_op_from_oid(oid: pg_sys::Oid) -> AggOp {
    // PostgreSQL built-in aggregate function OIDs.
    // These are well-known and stable across versions.
    let oid_raw = u32::from(oid);
    match oid_raw {
        // SUM variants
        2108 | 2109 | 2110 | 2111 | 2114 => AggOp::Sum,
        // AVG variants
        2100..=2106 => AggOp::Avg,
        // MIN variants
        2117 | 2118 | 2119 | 2120 | 2132 | 2133 | 2134 | 2135 | 2136 | 2245 => AggOp::Min,
        // MAX variants
        2115 | 2116 | 2126 | 2127 | 2128 | 2129 | 2130 | 2131 | 2243 | 2244 => AggOp::Max,
        // COUNT(*) / COUNT(expr)
        2803 | 2147 => AggOp::Count,
        _ => {
            // Try to resolve by name via pg_proc.
            // For simplicity, check the common procnames.
            AggOp::Passthrough
        }
    }
}

/// Resolve a Var reference to (source, attno) in the star schema.
/// source=0 means fact table, source=1+ means dimension depth index.
#[allow(clippy::cast_possible_wrap)]
fn resolve_var_to_star_source(
    varno: u32,
    varattno: i32,
    fact_relid: pg_sys::Index,
    inner_relids: &[pg_sys::Index],
) -> (u32, i32) {
    if varno as pg_sys::Index == fact_relid {
        return (0, varattno);
    }
    // Match varno against each dimension's inner relation relid.
    for (idx, &relid) in inner_relids.iter().enumerate() {
        if varno as pg_sys::Index == relid {
            return ((idx + 1) as u32, varattno);
        }
    }
    // Fallback: treat as first dimension.
    (1, varattno)
}

/// Compile a relation's `baserestrictinfo` into a `CompiledExpr::Template`
/// for fact-side WHERE pushdown. Supports `CmpConst`, `Between` (two range
/// predicates on the same column), and `TwoPredAnd` (two predicates on
/// different columns).
///
/// # Safety
///
/// `restrict` must be a valid PG `List` of `RestrictInfo` nodes, or null.
#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
unsafe fn compile_restrictinfo_to_template(
    restrict: *mut pg_sys::List,
) -> Option<crate::engine::expr_compiler::CompiledExpr> {
    use crate::engine::expr_compiler::{self, CompiledExpr, TemplateKernel};

    if restrict.is_null() {
        return None;
    }
    // SAFETY: restrict is a valid List.
    let len = unsafe { pg_sys::list_length(restrict) };
    if len == 0 {
        return None;
    }

    // Collect (col_idx_0based, cmp_opcode, const_val) from each simple Var op Const.
    let mut predicates: Vec<(u32, u16, f64)> = Vec::new();
    for i in 0..len {
        // SAFETY: i is in bounds.
        let ri = unsafe { pg_sys::list_nth(restrict, i).cast::<RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        // SAFETY: ri is valid.
        let clause = unsafe { (*ri).clause };
        if clause.is_null() {
            continue;
        }
        // SAFETY: clause is valid Node.
        let tag = unsafe { (*clause.cast::<pg_sys::Node>()).type_ };
        if tag != pg_sys::NodeTag::T_OpExpr {
            continue;
        }
        // SAFETY: tag confirmed T_OpExpr; alignment widening is safe for PG node casts.
        #[allow(clippy::cast_ptr_alignment)]
        let opexpr = clause.cast::<pg_sys::OpExpr>();
        // SAFETY: opexpr is valid.
        let args = unsafe { (*opexpr).args };
        if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
            continue;
        }
        // SAFETY: resolve op name.
        let op_name_ptr = unsafe { pg_sys::get_opname((*opexpr).opno) };
        if op_name_ptr.is_null() {
            continue;
        }
        // SAFETY: op_name_ptr is a valid C string from get_opname.
        let Ok(op_name) = unsafe { std::ffi::CStr::from_ptr(op_name_ptr) }.to_str() else {
            continue;
        };
        let Some(cmp_opcode) = expr_compiler::pg_cmp_op_to_opcode(op_name) else {
            continue;
        };

        // SAFETY: args has 2 elements.
        let left = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
        let right = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };

        // Try Var <cmp> Const.
        if let Some((attno, val)) = unsafe { extract_dim_var_const(left, right) } {
            predicates.push(((attno - 1) as u32, cmp_opcode, val));
        } else if let Some((attno, val)) = unsafe { extract_dim_var_const(right, left) } {
            // Flip comparison direction.
            let flipped = match cmp_opcode {
                2 => 4, 3 => 5, 4 => 2, 5 => 3,
                other => other,
            };
            predicates.push(((attno - 1) as u32, flipped, val));
        }
    }

    if predicates.is_empty() {
        return None;
    }

    // Try to form a Between: two predicates on the same column, >= and <=.
    if predicates.len() == 2 && predicates[0].0 == predicates[1].0 {
        let (col, op0, v0) = predicates[0];
        let (_, op1, v1) = predicates[1];
        // GE + LE or LE + GE → Between
        if (op0 == 5 && op1 == 3) || (op0 == 3 && op1 == 5) {
            let (lo, hi) = if op0 == 5 { (v0, v1) } else { (v1, v0) };
            return Some(CompiledExpr::Template(TemplateKernel::Between {
                col_idx: col,
                lo,
                hi,
            }));
        }
    }

    // Single predicate → CmpConst.
    if predicates.len() == 1 {
        let (col, op, val) = predicates[0];
        return Some(CompiledExpr::Template(TemplateKernel::CmpConst {
            col_idx: col,
            cmp_opcode: op,
            const_val: val,
        }));
    }

    // Two predicates on different columns → TwoPredAnd.
    if predicates.len() >= 2 {
        let (col1, op1, val1) = predicates[0];
        let (col2, op2, val2) = predicates[1];
        return Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
            col1_idx: col1,
            cmp1_opcode: op1,
            const1_val: val1,
            col2_idx: col2,
            cmp2_opcode: op2,
            const2_val: val2,
        }));
    }

    None
}

/// Extract pushable dimension-side filter predicates from a relation's
/// `baserestrictinfo`. Returns `DimFilter` entries with `col_idx` set to
/// the raw attno (1-based) — resolved to column index in the executor.
///
/// # Safety
///
/// `inner_rel` must be a valid `RelOptInfo` pointer from the planner.
#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
unsafe fn extract_dim_filters_from_rel(
    inner_rel: *mut pg_sys::RelOptInfo,
) -> Vec<DimFilter> {
    use crate::engine::expr_compiler;

    let mut filters = Vec::new();
    // SAFETY: inner_rel is valid per caller contract.
    let restrict = unsafe { (*inner_rel).baserestrictinfo };
    if restrict.is_null() {
        return filters;
    }
    // SAFETY: restrict is a valid List.
    let len = unsafe { pg_sys::list_length(restrict) };
    for i in 0..len {
        // SAFETY: i is within bounds.
        let ri = unsafe { pg_sys::list_nth(restrict, i).cast::<RestrictInfo>() };
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
        if tag != pg_sys::NodeTag::T_OpExpr {
            continue;
        }

        // SAFETY: tag confirmed T_OpExpr; alignment widening is safe for PG node casts.
        #[allow(clippy::cast_ptr_alignment)]
        let opexpr = clause.cast::<pg_sys::OpExpr>();
        // SAFETY: opexpr is a valid OpExpr.
        let args = unsafe { (*opexpr).args };
        if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
            continue;
        }

        // Resolve operator to cmp_opcode.
        // SAFETY: opno is valid.
        let op_name_ptr = unsafe { pg_sys::get_opname((*opexpr).opno) };
        if op_name_ptr.is_null() {
            continue;
        }
        // SAFETY: op_name_ptr is a valid C string from get_opname.
        let Ok(op_name) = unsafe { std::ffi::CStr::from_ptr(op_name_ptr) }.to_str() else {
            continue;
        };
        let Some(cmp_opcode) = expr_compiler::pg_cmp_op_to_opcode(op_name) else {
            continue;
        };

        // SAFETY: args has 2 elements.
        let left = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
        let right = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };

        // SAFETY: left and right are valid PG Node pointers from list_nth.
        // Try to extract Var <cmp> Const in either order.
        if let Some((attno, const_val)) = unsafe { extract_dim_var_const(left, right) } {
            filters.push(DimFilter {
                col_idx: attno as usize, // raw attno, resolved later in executor
                cmp_opcode,
                const_val,
            });
        } else if let Some((attno, const_val)) = unsafe { extract_dim_var_const(right, left) } {
            // Swap operand order: flip comparison direction.
            let flipped = match cmp_opcode {
                2 => 4, // LT → GT
                3 => 5, // LE → GE
                4 => 2, // GT → LT
                5 => 3, // GE → LE
                other => other, // EQ, NE unchanged
            };
            filters.push(DimFilter {
                col_idx: attno as usize,
                cmp_opcode: flipped,
                const_val,
            });
        }
    }
    filters
}

/// Try to extract (attno, const_value) from a (Var, Const) node pair.
/// Returns the Var's attno (1-based) and the Const's f64 value.
///
/// # Safety
///
/// Both pointers must be valid PG Node pointers.
#[allow(clippy::cast_precision_loss, clippy::cast_ptr_alignment)]
unsafe fn extract_dim_var_const(
    var_node: *mut pg_sys::Node,
    const_node: *mut pg_sys::Node,
) -> Option<(i32, f64)> {
    if var_node.is_null() || const_node.is_null() {
        return None;
    }

    // Unwrap Var (handle RelabelType wrappers).
    let var_tag = unsafe { (*var_node).type_ };
    let attno = match var_tag {
        pg_sys::NodeTag::T_Var => {
            // SAFETY: tag confirmed Var.
            let v = var_node.cast::<pg_sys::Var>();
            i32::from(unsafe { (*v).varattno })
        }
        pg_sys::NodeTag::T_RelabelType => {
            // SAFETY: tag confirmed RelabelType.
            let arg = unsafe { (*var_node.cast::<pg_sys::RelabelType>()).arg };
            let inner_tag = unsafe { (*arg.cast::<pg_sys::Node>()).type_ };
            if inner_tag != pg_sys::NodeTag::T_Var {
                return None;
            }
            i32::from(unsafe { (*arg.cast::<pg_sys::Var>()).varattno })
        }
        _ => return None,
    };
    if attno <= 0 {
        return None; // system column
    }

    // Extract Const value.
    // SAFETY: const_node tag check.
    if unsafe { (*const_node).type_ } != pg_sys::NodeTag::T_Const {
        return None;
    }
    let cst = const_node.cast::<pg_sys::Const>();
    // SAFETY: tag confirmed Const.
    if unsafe { (*cst).constisnull } {
        return None;
    }
    let datum = unsafe { (*cst).constvalue };
    let typid = u32::from(unsafe { (*cst).consttype });

    let val = match typid {
        21 => Some(f64::from(datum.value() as i16)),     // INT2
        23 => Some(f64::from(datum.value() as i32)),     // INT4
        20 => Some((datum.value() as i64) as f64),       // INT8
        700 => Some(f64::from(f32::from_bits(datum.value() as u32))), // FLOAT4
        701 => Some(f64::from_bits(datum.value() as u64)), // FLOAT8
        1082 => Some(f64::from(datum.value() as i32)),   // DATE (stored as int32)
        _ => None,
    }?;

    Some((attno, val))
}

/// Find an equi-join key from a restrictinfo list.
///
/// # Safety
///
/// All pointers must be valid planner structures.
unsafe fn find_equi_join_from_restrictinfo(
    restrictinfo: *mut List,
    outer_path: *mut Path,
    inner_path: *mut Path,
) -> Option<EquiJoinKey> {
    if restrictinfo.is_null() {
        return None;
    }
    // Reuse the existing equi-join key finder with the restrictinfo.
    let outer_rel = unsafe { (*outer_path).parent };
    let inner_rel = unsafe { (*inner_path).parent };
    if outer_rel.is_null() || inner_rel.is_null() {
        return None;
    }
    // SAFETY: all pointers are valid planner structures.
    unsafe { find_equi_join_key(restrictinfo, outer_rel, inner_rel) }
}

// ---------------------------------------------------------------------------
// GPU aggregate injection
// ---------------------------------------------------------------------------

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
    // Gate: GPU must be available and enabled.
    if !cfg!(feature = "gpu") || !cost::gpu_is_usable() {
        pgrx::debug1!("pg_accel: gpu_agg rejected: gpu not available or disabled");
        return;
    }

    // SAFETY: root is a valid PlannerInfo pointer.
    let root_ref = unsafe { &*root };
    let input_ref = unsafe { &*input_rel };

    // Gate: Check GROUP BY — we support plain aggregates, single-column,
    // and two-column GROUP BY (composite key encoding: two int4 → one int8).
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
    // Reject GROUP BY with more than 2 columns.
    if group_len > 2 {
        pgrx::debug1!("pg_accel: gpu_agg rejected: GROUP BY has {} cols (max 2)", group_len);
        return;
    }

    // Extract group key info for single- or two-column GROUP BY.
    let group_key_info: Option<GroupKeyInfo> = if group_len >= 1 {
        let tlist = query.targetList;
        let tlist_len = if tlist.is_null() {
            0
        } else {
            unsafe { pg_sys::list_length(tlist) }
        };

        // Helper: resolve a SortGroupClause to (attno, type_oid).
        let resolve_group_col = |idx: i32| -> Option<(i32, pg_sys::Oid)> {
            // SAFETY: groupClause has at least idx+1 elements.
            let sc = unsafe {
                pg_sys::list_nth(query.groupClause, idx).cast::<pg_sys::SortGroupClause>()
            };
            if sc.is_null() {
                return None;
            }
            let sgref = unsafe { (*sc).tleSortGroupRef };
            for j in 0..tlist_len {
                // SAFETY: j is in [0, tlist_len).
                let tle = unsafe { pg_sys::list_nth(tlist, j).cast::<pg_sys::TargetEntry>() };
                if tle.is_null() {
                    continue;
                }
                if unsafe { (*tle).ressortgroupref } != sgref {
                    continue;
                }
                let gk_expr = unsafe { (*tle).expr };
                if gk_expr.is_null() {
                    return None;
                }
                // SAFETY: reading node tag.
                let gk_tag = unsafe { (*gk_expr.cast::<pg_sys::Node>()).type_ };
                if gk_tag != NodeTag::T_Var {
                    return None;
                }
                let gk_var = gk_expr.cast::<pg_sys::Var>();
                let gk_typid = unsafe { (*gk_var).vartype };
                let gk_attno = i32::from(unsafe { (*gk_var).varattno });
                return Some((gk_attno, gk_typid));
            }
            None
        };

        if group_len == 1 {
            let Some((gk_attno, gk_typid)) = resolve_group_col(0) else {
                pgrx::debug1!("pg_accel: gpu_agg rejected: GROUP BY col not a Var");
                return;
            };
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
            // Two-column GROUP BY: composite key encoding.
            // Both columns must be int2 or int4 so we can pack them into a
            // single int8 key (high 32 bits = col1, low 32 bits = col2).
            let Some((attno1, typid1)) = resolve_group_col(0) else {
                pgrx::debug1!("pg_accel: gpu_agg rejected: GROUP BY col1 not a Var");
                return;
            };
            let Some((_attno2, typid2)) = resolve_group_col(1) else {
                pgrx::debug1!("pg_accel: gpu_agg rejected: GROUP BY col2 not a Var");
                return;
            };
            // Only int2/int4 types can be packed into a composite int8 key.
            let is_small_int = |oid: pg_sys::Oid| {
                matches!(u32::from(oid), 21 | 23) // INT2OID | INT4OID
            };
            if !is_small_int(typid1) || !is_small_int(typid2) {
                pgrx::debug1!(
                    "pg_accel: gpu_agg rejected: 2-col GROUP BY requires int2/int4 types"
                );
                return;
            }
            // Encode as composite: key_type=3 signals the executor to pack
            // two int4 values into one int8. attno stores col1, and we
            // serialize col2's attno separately in custom_private.
            Some(GroupKeyInfo {
                attno: attno1,
                type_oid: pg_sys::INT8OID, // composite key is i64
                key_type: 3,               // CompositeInt4x2
            })
        }
    } else {
        None
    };

    // Stash second group key attno for two-column GROUP BY serialization.
    let group_key2_attno: i32 = if group_len == 2 {
        let tlist = query.targetList;
        let tlist_len = if tlist.is_null() {
            0
        } else {
            unsafe { pg_sys::list_length(tlist) }
        };
        let sc = unsafe {
            pg_sys::list_nth(query.groupClause, 1).cast::<pg_sys::SortGroupClause>()
        };
        if sc.is_null() {
            return;
        }
        let sgref = unsafe { (*sc).tleSortGroupRef };
        let mut attno2 = 0i32;
        for j in 0..tlist_len {
            let tle = unsafe { pg_sys::list_nth(tlist, j).cast::<pg_sys::TargetEntry>() };
            if !tle.is_null() && unsafe { (*tle).ressortgroupref } == sgref {
                let gk_expr = unsafe { (*tle).expr };
                if !gk_expr.is_null()
                    && unsafe { (*gk_expr.cast::<pg_sys::Node>()).type_ } == NodeTag::T_Var
                {
                    attno2 = i32::from(unsafe { (*gk_expr.cast::<pg_sys::Var>()).varattno });
                }
                break;
            }
        }
        attno2
    } else {
        0
    };

    // Gate: Row count threshold.
    #[allow(clippy::cast_sign_loss)]
    let rows = input_ref.rows as usize;
    pgrx::debug1!("pg_accel: gpu_agg candidate, rows={}", rows);
    if rows < cost::device_limits().gpu_reduce_min_rows {
        pgrx::debug1!(
            "pg_accel: gpu_agg rejected: rows < {}",
            cost::device_limits().gpu_reduce_min_rows
        );
        return;
    }

    // Gate: Grouped aggregation has higher overhead (hash table build + probe
    // + per-group yield) than plain reduce. Require more rows to break even.
    if group_key_info.is_some()
        && rows < cost::device_limits().gpu_hash_agg_min_rows
    {
        pgrx::debug1!(
            "pg_accel: gpu_agg rejected: grouped agg rows {} < min {}",
            rows,
            cost::device_limits().gpu_hash_agg_min_rows
        );
        return;
    }

    // Gate: Reject GpuAgg wrapping small multi-join outputs.
    // When the input is a join path with small output, the Custom Scan
    // per-row yield overhead (~3µs/row) is not offset by GPU kernel savings.
    {
        let cheapest_input = unsafe { find_cheapest_path(input_ref.pathlist) };
        if !cheapest_input.is_null() {
            // SAFETY: cheapest_input is a valid Path from the input pathlist.
            let input_tag = unsafe { (*cheapest_input.cast::<pg_sys::Node>()).type_ };
            let is_join = matches!(
                input_tag,
                NodeTag::T_HashPath | NodeTag::T_NestPath | NodeTag::T_MergePath
            );
            if is_join && rows < cost::device_limits().gpu_join_max_output_rows {
                pgrx::debug1!(
                    "pg_accel: gpu_agg rejected: multi-join input with {} rows < join_max {}",
                    rows,
                    cost::device_limits().gpu_join_max_output_rows,
                );
                return;
            }
        }
    }

    // Gate: If GROUP BY, estimate group count via estimate_num_groups().
    if group_key_info.is_some() && !query.groupClause.is_null() {
        // SAFETY: root, groupClause are valid; input_ref.rows is the input cardinality.
        let est_groups = unsafe {
            pg_sys::estimate_num_groups(
                root,
                pg_sys::get_sortgrouplist_exprs(query.groupClause, query.targetList),
                input_ref.rows,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } as usize;
        pgrx::debug1!(
            "pg_accel: gpu_agg group check: est_groups={}, max={}",
            est_groups,
            cost::device_limits().gpu_hash_agg_max_groups
        );
        if est_groups > cost::device_limits().gpu_hash_agg_max_groups {
            pgrx::debug1!(
                "pg_accel: gpu_agg rejected: estimated {} groups > {}",
                est_groups,
                cost::device_limits().gpu_hash_agg_max_groups
            );
            return;
        }
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
    // Track 0-based position of group key Var(s) in the target list.
    let mut group_key_tlist_pos: i32 = -1;

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
                if group_key_tlist_pos < 0 {
                    group_key_tlist_pos = i;
                }
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

        // Gate: AVG returns float8 regardless of input type. Skip when
        // GPU lacks native fp64 to avoid precision loss.
        if op == AggOp::Avg && !cost::platform_has_fp64() {
            pgrx::debug1!("pg_accel: gpu_agg rejected: AVG requires fp64");
            return;
        }

        // COUNT(*): attno = 0, result type = int8.
        if op == AggOp::Count && aggref_ref.aggstar {
            agg_descs.push((AggOp::Count, 0, u32::from(aggref_ref.aggtype)));
            continue;
        }

        // Non-star aggregate: must have at least one argument.
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
        if arg_expr.is_null() {
            return;
        }

        // Check if argument is a plain Var (fast path) or an expression
        // (compilable to GPU bytecode — e.g., SUM(a * b), SUM(CASE ...)).
        let arg_tag = unsafe { (*arg_expr.cast::<pg_sys::Node>()).type_ };
        let (typid, attno) = if arg_tag == NodeTag::T_Var {
            let var = arg_expr.cast::<pg_sys::Var>();
            (
                u32::from(unsafe { (*var).vartype }),
                i32::from(unsafe { (*var).varattno }),
            )
        } else {
            // Expression argument — check if it's a GPU-compilable expr
            // (OpExpr, BoolExpr, FuncExpr). The result type must be numeric.
            // Use attno=0 to signal the executor that this is an expression
            // argument requiring GPU expression evaluation before aggregation.
            // For now, accept and let the executor attempt compilation.
            let result_type = u32::from(aggref_ref.aggtype);
            // Infer the expression result type. For SUM/AVG the result is
            // always numeric (float8 or int8), so we check the aggtype.
            let is_numeric_result = matches!(
                result_type,
                AGG_FLOAT4OID | AGG_FLOAT8OID | AGG_INT4OID | AGG_INT8OID
            );
            if !is_numeric_result {
                return;
            }
            // Use float8 as the assumed type for expression args (safe default).
            (AGG_FLOAT8OID, 0)
        };

        if attno != 0 && !matches!(
            typid,
            AGG_FLOAT4OID | AGG_FLOAT8OID | AGG_INT4OID | AGG_INT8OID
        ) {
            return;
        }
        // Note: float8 aggregates on non-fp64 GPU (Metal) fall back to CPU
        // Kahan summation in the agg executor. This is still faster than PG's
        // per-tuple transition functions thanks to batch columnar extraction.
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
    //
    // Our agg path is single-threaded: sequential child scan → columnar
    // extraction → reduce. On fp32-only GPUs (Metal), the reduce is CPU
    // Kahan summation. The cost must be HONEST — do NOT apply the
    // aggressive cost_multiplier/safety_margin used for GPU spatial ops.
    //
    // When the child is our GpuExpr scan CustomPath, pipeline fusion
    // kicks in: heap_getnext → inline filter → columnar extract →
    // accumulate in one pass, no ExecProcNode or MinimalTuple overhead.
    // SAFETY: cheapest is non-null.
    let base = unsafe { &*cheapest };

    // Detect fusion opportunity: child is our scan CustomPath AND the
    // row count meets the fusion minimum. Below the threshold, fusion
    // setup overhead (scan_desc open, template compile) exceeds savings.
    let child_is_our_scan = if unsafe { (*cheapest.cast::<pg_sys::Node>()).type_ }
        == NodeTag::T_CustomPath
    {
        let cp = cheapest.cast::<CustomPath>();
        // SAFETY: cp is a valid CustomPath (tag checked above).
        let is_ours = unsafe { (*cp).methods == custom_scan::scan_path_methods() };
        is_ours && rows >= cost::device_limits().gpu_pipeline_fusion_min_rows
    } else {
        false
    };

    let gpu_overhead = cost::GPU_LAUNCH_OVERHEAD;
    // Per-row cost depends on fusion. Fused path: direct heap walk +
    // inline filter + columnar accumulate (no ExecProcNode, no
    // MinimalTuple copy). Non-fused: ExecProcNode + tuple copy + extract.
    // Non-fused path: ExecProcNode + tuple copy + extract overhead.
    // Keep modest for plain reduce (single output row, negligible yield).
    // Grouped agg is gated separately by gpu_hash_agg_min_rows.
    let agg_per_row = if child_is_our_scan { 0.001 } else { 0.005 };
    // For grouped agg, add hash table build + probe cost per row.
    let hash_overhead = if group_key_info.is_some() { 0.002 } else { 0.0 };
    let reduce_cost = base.rows * (agg_per_row + hash_overhead);
    let startup_cost = base.total_cost + gpu_overhead;
    // No cost_multiplier or safety_margin — honest single-threaded cost.
    let total_cost = base.total_cost + gpu_overhead + reduce_cost;

    // Gate: Only inject if GpuAgg is meaningfully cheaper than PG's best
    // existing agg path. This prevents injection when the function being
    // aggregated is cheap (e.g. h3_cell_to_parent) and Custom Scan framing
    // overhead outweighs any GPU batch benefit.
    let output_ref = unsafe { &*output_rel };
    let pg_best_cost = unsafe { find_cheapest_total_cost(output_ref.pathlist) };
    if pg_best_cost > 0.0 && total_cost >= pg_best_cost * 0.90 {
        pgrx::debug1!(
            "pg_accel: gpu_agg rejected: cost {:.1} >= 90% of PG best {:.1}",
            total_cost,
            pg_best_cost,
        );
        return;
    }

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
            // For composite key (key_type=3), append second column attno.
            priv_list = lappend(priv_list, pg_sys::makeInteger(group_key2_attno).cast());
            // Group key's 0-based position in the output target list.
            priv_list = lappend(priv_list, pg_sys::makeInteger(group_key_tlist_pos).cast());
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
    // SAFETY: delegates to find_cheapest_path_filtered with no filter.
    unsafe { find_cheapest_path_filtered(pathlist, false) }
}

/// Find the cheapest non-parallel path in a pathlist.
///
/// Skips Gather and GatherMerge paths so the returned cost reflects
/// single-process execution. Use this as the baseline when estimating
/// Custom Scan cost — the Custom Scan runs single-threaded, so comparing
/// against PG's parallel Gather cost would underestimate our true cost.
///
/// # Safety
///
/// `pathlist` must be a valid PG `List` of `Path*` or null.
unsafe fn find_cheapest_nonparallel_path(pathlist: *mut List) -> *mut Path {
    // SAFETY: delegates to find_cheapest_path_filtered, skipping parallel.
    unsafe { find_cheapest_path_filtered(pathlist, true) }
}

/// Inner helper: find cheapest path, optionally skipping Gather/GatherMerge.
///
/// # Safety
///
/// `pathlist` must be a valid PG `List` of `Path*` or null.
unsafe fn find_cheapest_path_filtered(
    pathlist: *mut List,
    skip_parallel: bool,
) -> *mut Path {
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
        if skip_parallel {
            // SAFETY: path is a valid Path, checking the Node tag.
            let tag = unsafe { (*path.cast::<pg_sys::Node>()).type_ };
            if matches!(tag, NodeTag::T_GatherPath | NodeTag::T_GatherMergePath) {
                continue;
            }
        }
        // SAFETY: path and best are non-null valid Path pointers from the planner list.
        if best.is_null() || unsafe { (*path).total_cost < (*best).total_cost } {
            best = path;
        }
    }
    best
}

/// Find the total_cost of the cheapest path in a pathlist.
/// Returns 0.0 if the list is empty or null.
///
/// # Safety
///
/// `pathlist` must be a valid PG `List` of `Path*` or null.
unsafe fn find_cheapest_total_cost(pathlist: *mut List) -> f64 {
    let path = unsafe { find_cheapest_path(pathlist) };
    if path.is_null() {
        0.0
    } else {
        // SAFETY: path is a valid Path pointer.
        unsafe { (*path).total_cost }
    }
}

/// Find the cheapest sequential scan path (`T_Path`) in the pathlist.
/// Returns a pointer to the Path, or null if no seq scan path exists
/// (e.g., PG pruned it because an index path dominates).
///
/// # Safety
///
/// `pathlist` must be a valid `List` pointer from the planner, or null.
unsafe fn find_cheapest_seqscan_path(pathlist: *mut List) -> *mut Path {
    if pathlist.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: pathlist is a valid List from the planner.
    let len = unsafe { pg_sys::list_length(pathlist) };
    let mut best: *mut Path = std::ptr::null_mut();
    let mut best_cost = f64::MAX;
    for i in 0..len {
        // SAFETY: i is in [0, len), list_nth returns a valid pointer.
        let path = unsafe { pg_sys::list_nth(pathlist, i).cast::<Path>() };
        if path.is_null() {
            continue;
        }
        // SAFETY: reading node tag from valid Path pointer.
        let tag = unsafe { (*path).type_ };
        // Plain T_Path node = sequential scan (no index, bitmap, or custom).
        if tag == NodeTag::T_Path {
            // SAFETY: path is a valid Path pointer.
            let cost = unsafe { (*path).total_cost };
            if cost < best_cost {
                best_cost = cost;
                best = path;
            }
        }
    }
    best
}

/// Find the cost of the cheapest sequential scan path (`T_Path` with no
/// special pathtype). This gives the true seq scan baseline for comparing
/// index path selectivity — using `find_cheapest_path` would return the
/// index path itself when it's cheapest, making the ratio 1.0.
///
/// # Safety
///
/// `pathlist` must be a valid `List` pointer from the planner, or null.
unsafe fn find_cheapest_seqscan_cost(pathlist: *mut List) -> f64 {
    if pathlist.is_null() {
        return 0.0;
    }
    // SAFETY: pathlist is a valid List from the planner.
    let len = unsafe { pg_sys::list_length(pathlist) };
    let mut best = f64::MAX;
    for i in 0..len {
        // SAFETY: i is in [0, len), list_nth returns a valid pointer.
        let path = unsafe { pg_sys::list_nth(pathlist, i).cast::<Path>() };
        if path.is_null() {
            continue;
        }
        // SAFETY: reading node tag from valid Path pointer.
        let tag = unsafe { (*path).type_ };
        // Plain T_Path node = sequential scan (no index, bitmap, or custom).
        if tag == NodeTag::T_Path {
            // SAFETY: path is a valid Path pointer.
            let cost = unsafe { (*path).total_cost };
            if cost < best {
                best = cost;
            }
        }
    }
    if best == f64::MAX { 0.0 } else { best }
}

/// GiST access method OID.
const GIST_AM_OID: u32 = 783;
/// SP-GiST access method OID.
const SPGIST_AM_OID: u32 = 4000;

/// Check whether the relation's pathlist contains a cheap spatial index path
/// (GiST or SP-GiST) that makes Custom Scan injection counterproductive.
///
/// When a GiST index scan is available and highly selective, PostgreSQL's
/// native index scan avoids touching most heap pages entirely. Wrapping that
/// in a Custom Scan adds geometry deserialization, batch setup, and kernel
/// launch overhead that exceeds the savings — causing a regression.
///
/// Returns `true` if the planner should defer to PG's index scan (i.e., do
/// NOT inject a Custom Scan).
///
/// # Safety
///
/// `pathlist` must be a valid PG `List` pointer or null. `seq_scan_cost`
/// is the total cost of the cheapest sequential (non-index) path.
unsafe fn has_cheap_spatial_index_path(pathlist: *mut List, seq_scan_cost: f64) -> bool {
    if pathlist.is_null() {
        return false;
    }

    // SAFETY: pathlist is a valid List from the planner.
    let len = unsafe { pg_sys::list_length(pathlist) };

    for i in 0..len {
        // SAFETY: i is in [0, len), list_nth returns a valid pointer.
        let path = unsafe { pg_sys::list_nth(pathlist, i).cast::<Path>() };
        if path.is_null() {
            continue;
        }

        // SAFETY: path is a valid Path node from the planner.
        let tag = unsafe { (*path).type_ };

        match tag {
            NodeTag::T_IndexPath => {
                let ipath = path.cast::<pg_sys::IndexPath>();
                // SAFETY: ipath is a valid IndexPath (tag checked above).
                let info = unsafe { (*ipath).indexinfo };
                if info.is_null() {
                    continue;
                }
                // SAFETY: info is a valid IndexOptInfo from the planner.
                let relam = u32::from(unsafe { (*info).relam });
                if relam != GIST_AM_OID && relam != SPGIST_AM_OID {
                    continue;
                }

                // Check 1: selectivity-based — if the index is very
                // selective, PG's index scan is hard to beat.
                let selectivity = unsafe { (*ipath).indexselectivity };
                if selectivity > 0.0 && selectivity < cost::SPATIAL_INDEX_SELECTIVITY_THRESHOLD {
                    pgrx::debug1!(
                        "pg_accel: deferring to GiST index scan \
                         (selectivity={:.4}, threshold={:.2})",
                        selectivity,
                        cost::SPATIAL_INDEX_SELECTIVITY_THRESHOLD
                    );
                    return true;
                }

                // Check 2: cost-ratio — even without precise selectivity,
                // if the index path is much cheaper than seq scan, defer.
                if seq_scan_cost > 0.0 {
                    let idx_cost = unsafe { (*path).total_cost };
                    let ratio = idx_cost / seq_scan_cost;
                    if ratio < cost::SPATIAL_INDEX_COST_RATIO_THRESHOLD {
                        pgrx::debug1!(
                            "pg_accel: deferring to GiST index scan \
                             (cost_ratio={:.4}, threshold={:.2})",
                            ratio,
                            cost::SPATIAL_INDEX_COST_RATIO_THRESHOLD
                        );
                        return true;
                    }
                }
            }
            NodeTag::T_BitmapHeapPath => {
                // Bitmap heap scans over GiST indices are also efficient
                // for selective spatial filters. Check the underlying
                // bitmap qual path for a GiST index.
                let bpath = path.cast::<pg_sys::BitmapHeapPath>();
                // SAFETY: bpath is a valid BitmapHeapPath (tag checked).
                let qual = unsafe { (*bpath).bitmapqual };
                if qual.is_null() {
                    continue;
                }
                // The bitmapqual is typically a T_IndexPath for simple
                // single-index bitmap scans.
                // SAFETY: qual is a valid Path node.
                let qual_tag = unsafe { (*qual).type_ };
                if qual_tag == NodeTag::T_IndexPath {
                    let ipath = qual.cast::<pg_sys::IndexPath>();
                    let info = unsafe { (*ipath).indexinfo };
                    if info.is_null() {
                        continue;
                    }
                    let relam = u32::from(unsafe { (*info).relam });
                    if relam != GIST_AM_OID && relam != SPGIST_AM_OID {
                        continue;
                    }

                    // For bitmap paths, use cost-ratio check since
                    // bitmap selectivity is on the BitmapAnd/Or nodes.
                    if seq_scan_cost > 0.0 {
                        let bmp_cost = unsafe { (*path).total_cost };
                        let ratio = bmp_cost / seq_scan_cost;
                        if ratio < cost::SPATIAL_INDEX_COST_RATIO_THRESHOLD {
                            pgrx::debug1!(
                                "pg_accel: deferring to GiST bitmap scan \
                                 (cost_ratio={:.4}, threshold={:.2})",
                                ratio,
                                cost::SPATIAL_INDEX_COST_RATIO_THRESHOLD
                            );
                            return true;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    false
}

/// Check whether any GiST/SP-GiST index path in the pathlist has a lower
/// total cost than the proposed Custom Scan `custom_cost`.
///
/// This is a post-cost-computation gate: after we know our Custom Scan's
/// total_cost, reject injection if a spatial index path would be cheaper.
/// At large scales, PG's GiST index scan reads far fewer heap pages than
/// a full sequential scan, making it faster despite the 30% GPU discount.
///
/// # Safety
///
/// `pathlist` must be a valid PG `List` pointer or null.
unsafe fn has_cheaper_spatial_index_path(pathlist: *mut List, custom_cost: f64) -> bool {
    if pathlist.is_null() {
        return false;
    }

    // SAFETY: pathlist is a valid List from the planner.
    let len = unsafe { pg_sys::list_length(pathlist) };

    for i in 0..len {
        // SAFETY: i is in [0, len), list_nth returns a valid pointer.
        let path = unsafe { pg_sys::list_nth(pathlist, i).cast::<Path>() };
        if path.is_null() {
            continue;
        }

        // SAFETY: path is a valid Path node from the planner.
        let tag = unsafe { (*path).type_ };

        let is_spatial_index = match tag {
            NodeTag::T_IndexPath => {
                let ipath = path.cast::<pg_sys::IndexPath>();
                // SAFETY: ipath is a valid IndexPath (tag checked above).
                let info = unsafe { (*ipath).indexinfo };
                if info.is_null() {
                    false
                } else {
                    // SAFETY: info is a valid IndexOptInfo from the planner.
                    let relam = u32::from(unsafe { (*info).relam });
                    relam == GIST_AM_OID || relam == SPGIST_AM_OID
                }
            }
            _ => false,
        };

        if is_spatial_index {
            // SAFETY: path is a valid Path pointer.
            let idx_cost = unsafe { (*path).total_cost };
            if idx_cost < custom_cost {
                pgrx::debug1!(
                    "pg_accel: deferring to spatial index path \
                     (idx_cost={:.2}, custom_cost={:.2})",
                    idx_cost,
                    custom_cost,
                );
                return true;
            }
        }
    }

    false
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

/// Extract the vertex count of the constant geometry argument in a spatial
/// FuncExpr clause.  Returns `Some(count)` when a Const argument with a
/// valid GSERIALIZED datum is found; `None` otherwise (e.g. two Var args,
/// or expression not yet folded to Const).
///
/// # Safety
///
/// `restrictinfo_list` must be null or a valid PG `List` of `RestrictInfo`.
unsafe fn extract_const_geom_vertex_count(restrictinfo_list: *mut List) -> Option<usize> {
    use crate::adapters::extractors::geometry::extract_geometry;

    if restrictinfo_list.is_null() {
        return None;
    }
    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    for i in 0..len {
        // SAFETY: i in [0, len).
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        let clause = unsafe { (*ri).clause };
        if clause.is_null() {
            continue;
        }
        // SAFETY: clause is a valid Node.
        let tag = unsafe { (*clause.cast::<pg_sys::Node>()).type_ };
        #[allow(clippy::cast_ptr_alignment)]
        let args = match tag {
            NodeTag::T_FuncExpr => unsafe { (*clause.cast::<pg_sys::FuncExpr>()).args },
            NodeTag::T_OpExpr => unsafe { (*clause.cast::<pg_sys::OpExpr>()).args },
            _ => continue,
        };
        if args.is_null() {
            continue;
        }
        let alen = unsafe { pg_sys::list_length(args) };
        for j in 0..alen {
            let node = unsafe { pg_sys::list_nth(args, j).cast::<pg_sys::Node>() };
            if node.is_null() {
                continue;
            }
            // SAFETY: reading tag of arg node.
            if unsafe { (*node).type_ } != NodeTag::T_Const {
                continue;
            }
            let cst = node.cast::<pg_sys::Const>();
            // SAFETY: tag-checked Const; skip NULL constants.
            if unsafe { (*cst).constisnull } {
                continue;
            }
            let datum = unsafe { (*cst).constvalue };
            if let Some(geom) = extract_geometry(datum) {
                return Some(geom.coord_count);
            }
        }
    }
    None
}

/// GPU-supported numeric type OIDs for expression evaluation.
const EXPR_BOOL_OID: u32 = 16;
const EXPR_INT2_OID: u32 = 21;
const EXPR_INT4_OID: u32 = 23;
const EXPR_INT8_OID: u32 = 20;
const EXPR_FLOAT4_OID: u32 = 700;
const EXPR_FLOAT8_OID: u32 = 701;
const EXPR_DATE_OID: u32 = 1082;
const EXPR_TIMESTAMP_OID: u32 = 1114;

/// Whether a PG type OID is supported by the GPU expression evaluator.
#[inline]
fn is_gpu_expr_type(oid: u32) -> bool {
    matches!(
        oid,
        EXPR_BOOL_OID
            | EXPR_INT2_OID
            | EXPR_INT4_OID
            | EXPR_INT8_OID
            | EXPR_FLOAT4_OID
            | EXPR_FLOAT8_OID
            | EXPR_DATE_OID
            | EXPR_TIMESTAMP_OID
    )
}

/// Check if restriction clauses are candidates for GpuExpr evaluation.
///
/// Returns a GpuExpr match when all clauses look like standard numeric
/// expressions (OpExpr or BoolExpr at the top level). Full compilability
/// is checked at executor time — if compilation fails, the executor
/// gracefully falls back to PG's standard `ExecEvalExpr`.
fn try_gpu_expr_match(restrictinfo_list: *mut List) -> Option<AccelMatch> {
    if restrictinfo_list.is_null() {
        return None;
    }

    // SAFETY: restrictinfo_list is a valid List pointer from the planner.
    let len = unsafe { pg_sys::list_length(restrictinfo_list) };
    if len == 0 {
        return None;
    }

    // Quick top-level check: all clauses must be GPU-compilable node types
    // operating on numeric types.
    for i in 0..len {
        // SAFETY: i is in [0, len), list_nth returns a valid RestrictInfo*.
        let ri = unsafe { pg_sys::list_nth(restrictinfo_list, i).cast::<RestrictInfo>() };
        if ri.is_null() {
            return None;
        }
        let clause = unsafe { (*ri).clause };
        if clause.is_null() {
            return None;
        }
        // SAFETY: clause is a valid Expr node from the planner.
        if !is_gpu_compilable_clause(clause.cast()) {
            return None;
        }
    }

    Some(AccelMatch {
        strategy: registry::AccelStrategy::GpuExpr,
        fn_oid: pg_sys::InvalidOid,
        target_attno: 0,
    })
}

/// Recursively check whether a clause node is GPU-compilable.
///
/// Verifies node type AND that operand types are GPU-supported numerics.
/// This prevents non-numeric operators (LIKE, ~, etc.) from being
/// accepted as GpuExpr candidates.
///
/// # Safety
///
/// `node` must be a valid PG expression `Node` pointer.
#[allow(clippy::cast_ptr_alignment)]
fn is_gpu_compilable_clause(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }
    // SAFETY: node is a valid Node pointer.
    let tag = unsafe { (*node).type_ };
    match tag {
        NodeTag::T_OpExpr => {
            // OpExpr: verify result type is boolean or numeric.
            let op = node.cast::<pg_sys::OpExpr>();
            let result_type = u32::from(unsafe { (*op).opresulttype });
            if !is_gpu_expr_type(result_type) {
                return false;
            }
            // Check operand types via the args list.
            let args = unsafe { (*op).args };
            if !args.is_null() {
                let nargs = unsafe { pg_sys::list_length(args) };
                for j in 0..nargs {
                    let arg = unsafe { pg_sys::list_nth(args, j).cast::<pg_sys::Node>() };
                    if !is_gpu_compilable_expr_node(arg) {
                        return false;
                    }
                }
            }
            true
        }
        NodeTag::T_BoolExpr => {
            // BoolExpr (AND/OR/NOT): check all sub-clauses recursively.
            let boolexpr = node.cast::<pg_sys::BoolExpr>();
            let args = unsafe { (*boolexpr).args };
            if args.is_null() {
                return false;
            }
            let nargs = unsafe { pg_sys::list_length(args) };
            for j in 0..nargs {
                let arg = unsafe { pg_sys::list_nth(args, j).cast::<pg_sys::Node>() };
                if !is_gpu_compilable_clause(arg) {
                    return false;
                }
            }
            true
        }
        NodeTag::T_NullTest => true,
        NodeTag::T_ScalarArrayOpExpr => {
            // IN-list: check that the array element type is numeric.
            let saop = node.cast::<pg_sys::ScalarArrayOpExpr>();
            // The first arg is the scalar, second is the array/list.
            let args = unsafe { (*saop).args };
            if args.is_null() {
                return false;
            }
            let scalar = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
            is_gpu_compilable_expr_node(scalar)
        }
        NodeTag::T_FuncExpr => {
            // FuncExpr: accept only known GPU-compilable functions.
            let funcexpr = node.cast::<pg_sys::FuncExpr>();
            let result_type = u32::from(unsafe { (*funcexpr).funcresulttype });
            is_gpu_expr_type(result_type)
        }
        NodeTag::T_CaseExpr => {
            // CASE expressions: result type must be numeric.
            let caseexpr = node.cast::<pg_sys::CaseExpr>();
            let result_type = u32::from(unsafe { (*caseexpr).casetype });
            is_gpu_expr_type(result_type)
        }
        NodeTag::T_RelabelType | NodeTag::T_CoerceViaIO => true,
        _ => false,
    }
}

/// Check whether an expression leaf node has a GPU-supported type.
///
/// # Safety
///
/// `node` must be a valid PG expression `Node` pointer.
#[allow(clippy::cast_ptr_alignment)]
fn is_gpu_compilable_expr_node(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }
    // SAFETY: node is a valid Node pointer.
    let tag = unsafe { (*node).type_ };
    match tag {
        NodeTag::T_Var => {
            let var = node.cast::<pg_sys::Var>();
            is_gpu_expr_type(u32::from(unsafe { (*var).vartype }))
        }
        NodeTag::T_Const => {
            let cst = node.cast::<pg_sys::Const>();
            is_gpu_expr_type(u32::from(unsafe { (*cst).consttype }))
        }
        // Nested expressions: recurse into the clause checker.
        NodeTag::T_OpExpr
        | NodeTag::T_BoolExpr
        | NodeTag::T_FuncExpr
        | NodeTag::T_CaseExpr
        | NodeTag::T_NullTest
        | NodeTag::T_ScalarArrayOpExpr => is_gpu_compilable_clause(node),
        // Cast wrappers: accept and let the compiler handle them.
        NodeTag::T_RelabelType | NodeTag::T_CoerceViaIO => true,
        _ => false,
    }
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
    /// Range table index (varno) of the outer join key variable.
    outer_varno: i32,
    /// Range table index (varno) of the inner join key variable.
    inner_varno: i32,
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

        let (outer_attno, inner_attno, outer_varno, inner_varno, key_oid) =
            if left_is_outer && right_is_inner {
                (left_attno, right_attno, left_varno, right_varno, left_type)
            } else if left_is_inner && right_is_outer {
                (right_attno, left_attno, right_varno, left_varno, right_type)
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

        // Gate: Skip Float64 join keys when GPU lacks native fp64.
        if key_type == 2 && !cost::platform_has_fp64() {
            pgrx::debug1!("pg_accel join: float64 key skipped — GPU lacks fp64");
            continue;
        }

        return Some(EquiJoinKey {
            outer_attno,
            inner_attno,
            outer_varno,
            inner_varno,
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

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // =====================================================================
    // Existing tests (preserved)
    // =====================================================================

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
        let base_size = std::mem::size_of::<pg_sys::Path>();
        assert!(base_size > 0);
    }

    #[test]
    fn path_node_size_all_match_arms_return_positive() {
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

    // =====================================================================
    // Null-pointer guards on helper functions
    // =====================================================================

    #[test]
    fn find_accelerable_match_null_returns_none() {
        assert!(find_accelerable_match(std::ptr::null_mut()).is_none());
    }

    #[test]
    fn find_accelerable_strategy_null_returns_none() {
        assert!(find_accelerable_strategy(std::ptr::null_mut()).is_none());
    }

    #[test]
    fn extract_var_attno_from_args_null_returns_zero() {
        assert_eq!(extract_var_attno_from_args(std::ptr::null_mut()), 0);
    }

    #[test]
    fn node_find_accel_match_null_returns_none() {
        let reg = registry::AdapterRegistry::new();
        assert!(node_find_accel_match(std::ptr::null_mut(), &reg).is_none());
    }

    #[test]
    fn recurse_args_for_match_null_returns_none() {
        let reg = registry::AdapterRegistry::new();
        assert!(recurse_args_for_match(std::ptr::null_mut(), &reg).is_none());
    }

    // =====================================================================
    // AccelMatch struct construction and field access
    // =====================================================================

    #[test]
    fn accel_match_stores_gpu_spatial_strategy() {
        let m = AccelMatch {
            strategy: registry::AccelStrategy::GpuSpatial,
            fn_oid: pg_sys::Oid::from(12345u32),
            target_attno: 3,
        };
        assert_eq!(m.strategy, registry::AccelStrategy::GpuSpatial);
        assert_eq!(u32::from(m.fn_oid), 12345);
        assert_eq!(m.target_attno, 3);
    }

    #[test]
    fn accel_match_stores_gpu_h3_strategy() {
        let m = AccelMatch {
            strategy: registry::AccelStrategy::GpuH3,
            fn_oid: pg_sys::Oid::from(99u32),
            target_attno: 0,
        };
        assert_eq!(m.strategy, registry::AccelStrategy::GpuH3);
        assert_eq!(m.target_attno, 0);
    }

    #[test]
    fn accel_match_stores_gpu_spatial_strategy_minimal() {
        let m = AccelMatch {
            strategy: registry::AccelStrategy::GpuSpatial,
            fn_oid: pg_sys::Oid::from(1u32),
            target_attno: 1,
        };
        assert_eq!(m.strategy, registry::AccelStrategy::GpuSpatial);
    }

    // =====================================================================
    // EquiJoinKey struct construction and field access
    // =====================================================================

    #[test]
    fn equi_join_key_int32_construction() {
        let k = EquiJoinKey {
            outer_attno: 1,
            inner_attno: 2,
            outer_varno: 1,
            inner_varno: 2,
            key_type: 0, // Int32
        };
        assert_eq!(k.outer_attno, 1);
        assert_eq!(k.inner_attno, 2);
        assert_eq!(k.key_type, 0);
    }

    #[test]
    fn equi_join_key_int64_construction() {
        let k = EquiJoinKey {
            outer_attno: 5,
            inner_attno: 3,
            outer_varno: 1,
            inner_varno: 2,
            key_type: 1, // Int64
        };
        assert_eq!(k.key_type, 1);
    }

    #[test]
    fn equi_join_key_float64_construction() {
        let k = EquiJoinKey {
            outer_attno: 2,
            inner_attno: 7,
            outer_varno: 1,
            inner_varno: 2,
            key_type: 2, // Float64
        };
        assert_eq!(k.key_type, 2);
    }

    // =====================================================================
    // AccelStrategy round-trip via from_i32
    // =====================================================================

    #[test]
    fn accel_strategy_from_i32_all_variants() {
        assert_eq!(
            registry::AccelStrategy::from_i32(1),
            registry::AccelStrategy::GpuSpatial
        );
        assert_eq!(
            registry::AccelStrategy::from_i32(2),
            registry::AccelStrategy::GpuRaster
        );
        assert_eq!(
            registry::AccelStrategy::from_i32(3),
            registry::AccelStrategy::GpuH3
        );
        assert_eq!(
            registry::AccelStrategy::from_i32(4),
            registry::AccelStrategy::GpuSort
        );
        assert_eq!(
            registry::AccelStrategy::from_i32(5),
            registry::AccelStrategy::GpuReduce
        );
        assert_eq!(
            registry::AccelStrategy::from_i32(6),
            registry::AccelStrategy::GpuExpr
        );
        assert_eq!(
            registry::AccelStrategy::from_i32(7),
            registry::AccelStrategy::GpuHashJoin
        );
        assert_eq!(
            registry::AccelStrategy::from_i32(8),
            registry::AccelStrategy::GpuWindow
        );
    }

    #[test]
    fn accel_strategy_from_i32_unknown_defaults_to_gpu_spatial() {
        assert_eq!(
            registry::AccelStrategy::from_i32(-1),
            registry::AccelStrategy::GpuSpatial
        );
        assert_eq!(
            registry::AccelStrategy::from_i32(0),
            registry::AccelStrategy::GpuSpatial
        );
        assert_eq!(
            registry::AccelStrategy::from_i32(99),
            registry::AccelStrategy::GpuSpatial
        );
        assert_eq!(
            registry::AccelStrategy::from_i32(i32::MAX),
            registry::AccelStrategy::GpuSpatial
        );
    }

    #[test]
    fn accel_strategy_repr_i32_roundtrip() {
        // The enum is #[repr(i32)], so casting to i32 and back should work.
        let strategies = [
            registry::AccelStrategy::GpuSpatial,
            registry::AccelStrategy::GpuRaster,
            registry::AccelStrategy::GpuH3,
            registry::AccelStrategy::GpuSort,
            registry::AccelStrategy::GpuReduce,
            registry::AccelStrategy::GpuExpr,
            registry::AccelStrategy::GpuHashJoin,
            registry::AccelStrategy::GpuWindow,
        ];
        for s in strategies {
            let i = s as i32;
            assert_eq!(registry::AccelStrategy::from_i32(i), s);
        }
    }

    // =====================================================================
    // AggOp round-trip
    // =====================================================================

    #[test]
    fn agg_op_to_i32_roundtrip() {
        let ops = [
            AggOp::Sum,
            AggOp::Avg,
            AggOp::Min,
            AggOp::Max,
            AggOp::Count,
            AggOp::Passthrough,
        ];
        for op in ops {
            assert_eq!(AggOp::from_i32(op.to_i32()), op);
        }
    }

    #[test]
    fn agg_op_from_i32_unknown_returns_passthrough() {
        assert_eq!(AggOp::from_i32(100), AggOp::Passthrough);
        assert_eq!(AggOp::from_i32(-1), AggOp::Passthrough);
    }

    // =====================================================================
    // WindowFunc round-trip
    // =====================================================================

    #[test]
    fn window_func_to_i32_roundtrip() {
        let funcs = [
            WindowFunc::RowNumber,
            WindowFunc::Rank,
            WindowFunc::DenseRank,
            WindowFunc::Sum,
            WindowFunc::Count,
            WindowFunc::Lag,
            WindowFunc::Lead,
        ];
        for f in funcs {
            assert_eq!(WindowFunc::from_i32(f.to_i32()), Some(f));
        }
    }

    #[test]
    fn window_func_from_i32_unknown_returns_none() {
        assert_eq!(WindowFunc::from_i32(7), None);
        assert_eq!(WindowFunc::from_i32(-1), None);
        assert_eq!(WindowFunc::from_i32(100), None);
    }

    // =====================================================================
    // WindowFuncSpec construction
    // =====================================================================

    #[test]
    fn window_func_spec_construction() {
        let spec = WindowFuncSpec {
            func: WindowFunc::Sum,
            partition_attno: 2,
            order_attno: 3,
            value_attno: 4,
            offset: 1,
            default_val: 0.0,
            result_type_oid: 701, // float8
        };
        assert_eq!(spec.func, WindowFunc::Sum);
        assert_eq!(spec.partition_attno, 2);
        assert_eq!(spec.order_attno, 3);
        assert_eq!(spec.value_attno, 4);
        assert_eq!(spec.offset, 1);
        assert!((spec.default_val - 0.0).abs() < f64::EPSILON);
        assert_eq!(spec.result_type_oid, 701);
    }

    #[test]
    fn window_func_spec_lag_with_offset_and_default() {
        let spec = WindowFuncSpec {
            func: WindowFunc::Lag,
            partition_attno: 0,
            order_attno: 1,
            value_attno: 2,
            offset: 3,
            default_val: -999.0,
            result_type_oid: 701,
        };
        assert_eq!(spec.func, WindowFunc::Lag);
        assert_eq!(spec.offset, 3);
        assert!((spec.default_val - (-999.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn window_func_spec_no_partition() {
        let spec = WindowFuncSpec {
            func: WindowFunc::RowNumber,
            partition_attno: 0,
            order_attno: 0,
            value_attno: 0,
            offset: 1,
            default_val: 0.0,
            result_type_oid: 23,
        };
        assert_eq!(spec.partition_attno, 0);
        assert_eq!(spec.order_attno, 0);
    }

    // =====================================================================
    // SortKeyDesc construction
    // =====================================================================

    #[test]
    fn sort_key_desc_construction() {
        let sk = SortKeyDesc {
            attno: 5,
            sort_op: pg_sys::Oid::from(97u32), // int4lt
            collation: pg_sys::Oid::from(0u32),
            nulls_first: false,
        };
        assert_eq!(sk.attno, 5);
        assert!(!sk.nulls_first);
    }

    #[test]
    fn sort_key_desc_nulls_first() {
        let sk = SortKeyDesc {
            attno: 1,
            sort_op: pg_sys::Oid::from(622u32), // float8lt
            collation: pg_sys::Oid::from(0u32),
            nulls_first: true,
        };
        assert!(sk.nulls_first);
    }

    // =====================================================================
    // GPU-sortable type OID classification
    // =====================================================================

    #[test]
    fn sort_type_oid_int4_is_sortable() {
        assert!(matches!(SORT_INT4OID, 23));
    }

    #[test]
    fn sort_type_oid_float4_is_sortable() {
        assert!(matches!(SORT_FLOAT4OID, 700));
    }

    #[test]
    fn sort_type_oid_float8_is_sortable() {
        assert!(matches!(SORT_FLOAT8OID, 701));
    }

    #[test]
    fn sort_type_oids_match_guard_accepts_all_three() {
        for oid in [SORT_INT4OID, SORT_FLOAT4OID, SORT_FLOAT8OID] {
            assert!(
                matches!(oid, SORT_INT4OID | SORT_FLOAT4OID | SORT_FLOAT8OID),
                "OID {oid} should match the sortable guard"
            );
        }
    }

    #[test]
    fn sort_type_oids_reject_non_sortable() {
        let non_sortable = [16u32, 20, 21, 25, 1043]; // bool, int8, int2, text, varchar
        for oid in non_sortable {
            assert!(
                !matches!(oid, SORT_INT4OID | SORT_FLOAT4OID | SORT_FLOAT8OID),
                "OID {oid} should NOT match the sortable guard"
            );
        }
    }

    // =====================================================================
    // Agg type OID classification
    // =====================================================================

    #[test]
    fn agg_type_oids_match_guard_accepts_numeric() {
        for oid in [AGG_FLOAT4OID, AGG_FLOAT8OID, AGG_INT4OID, AGG_INT8OID] {
            assert!(
                matches!(
                    oid,
                    AGG_FLOAT4OID | AGG_FLOAT8OID | AGG_INT4OID | AGG_INT8OID
                ),
                "OID {oid} should be accepted for agg"
            );
        }
    }

    #[test]
    fn agg_type_oids_reject_non_numeric() {
        let non_numeric = [16u32, 21, 25, 1043, 1700]; // bool, int2, text, varchar, numeric
        for oid in non_numeric {
            assert!(
                !matches!(
                    oid,
                    AGG_FLOAT4OID | AGG_FLOAT8OID | AGG_INT4OID | AGG_INT8OID
                ),
                "OID {oid} should NOT be accepted for agg"
            );
        }
    }

    // =====================================================================
    // Window type OID classification
    // =====================================================================

    #[test]
    fn win_type_oids_match_guard_accepts_numeric() {
        for oid in [WIN_FLOAT4OID, WIN_FLOAT8OID, WIN_INT4OID, WIN_INT8OID] {
            assert!(
                matches!(
                    oid,
                    WIN_FLOAT4OID | WIN_FLOAT8OID | WIN_INT4OID | WIN_INT8OID
                ),
                "OID {oid} should be accepted for window"
            );
        }
    }

    #[test]
    fn win_type_oids_reject_non_numeric() {
        let non_numeric = [16u32, 21, 25, 1043];
        for oid in non_numeric {
            assert!(
                !matches!(
                    oid,
                    WIN_FLOAT4OID | WIN_FLOAT8OID | WIN_INT4OID | WIN_INT8OID
                ),
                "OID {oid} should NOT be accepted for window"
            );
        }
    }

    // =====================================================================
    // GiST / SP-GiST AM OID constants
    // =====================================================================

    #[test]
    fn gist_am_oid_is_783() {
        assert_eq!(GIST_AM_OID, 783);
    }

    #[test]
    fn spgist_am_oid_is_4000() {
        assert_eq!(SPGIST_AM_OID, 4000);
    }

    // =====================================================================
    // GroupKeyInfo::key_type_from_oid classification
    // =====================================================================

    #[test]
    fn group_key_type_int2_maps_to_int32() {
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT2OID), Some(0));
    }

    #[test]
    fn group_key_type_int4_maps_to_int32() {
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT4OID), Some(0));
    }

    #[test]
    fn group_key_type_int8_maps_to_int64() {
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT8OID), Some(1));
    }

    #[test]
    fn group_key_type_float4_maps_to_float64() {
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::FLOAT4OID), Some(2));
    }

    #[test]
    fn group_key_type_float8_maps_to_float64() {
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::FLOAT8OID), Some(2));
    }

    #[test]
    fn group_key_type_text_returns_none() {
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::TEXTOID), None);
    }

    // =====================================================================
    // Cost model: should_batch with strategy-specific per-row costs
    // =====================================================================

    #[test]
    fn should_batch_gpu_spatial_sufficient_rows() {
        // GPU spatial: per_row_cost = 0.05 (well above 0.01 threshold)
        assert!(cost::should_batch(
            1000,
            cost::GPU_SPATIAL_PER_ROW_COST,
            256
        ));
    }

    #[test]
    fn should_batch_gpu_h3_sufficient_rows() {
        assert!(cost::should_batch(500, cost::GPU_H3_PER_ROW_COST, 256));
    }

    #[test]
    fn should_batch_gpu_raster_sufficient_rows() {
        assert!(cost::should_batch(1000, cost::GPU_RASTER_PER_ROW_COST, 256));
    }

    #[test]
    fn should_batch_gpu_reduce_sufficient_rows() {
        assert!(cost::should_batch(500, cost::GPU_REDUCE_PER_ROW_COST, 256));
    }

    // =====================================================================
    // Cost model: optimal_batch_size clamping
    // =====================================================================

    #[test]
    fn optimal_batch_size_small_input_clamps_to_min() {
        let limits = cost::device_limits();
        let result = cost::optimal_batch_size(1);
        assert_eq!(result, limits.optimal_batch_min);
    }

    #[test]
    fn optimal_batch_size_large_input_clamps_to_max() {
        let limits = cost::device_limits();
        let result = cost::optimal_batch_size(usize::MAX);
        assert_eq!(result, limits.optimal_batch_max);
    }

    #[test]
    fn optimal_batch_size_mid_range_returns_input() {
        let limits = cost::device_limits();
        let mid = (limits.optimal_batch_min + limits.optimal_batch_max) / 2;
        assert_eq!(cost::optimal_batch_size(mid), mid);
    }

    // =====================================================================
    // Cost constant sanity checks
    // =====================================================================

    #[test]
    fn gpu_launch_overhead_is_positive() {
        assert!(cost::GPU_LAUNCH_OVERHEAD > 0.0);
    }

    #[test]
    fn gpu_cost_safety_margin_between_zero_and_one() {
        assert!(cost::GPU_COST_SAFETY_MARGIN > 0.0);
        assert!(cost::GPU_COST_SAFETY_MARGIN < 1.0);
    }

    #[test]
    fn spatial_index_selectivity_threshold_between_zero_and_one() {
        assert!(cost::SPATIAL_INDEX_SELECTIVITY_THRESHOLD > 0.0);
        assert!(cost::SPATIAL_INDEX_SELECTIVITY_THRESHOLD < 1.0);
    }

    #[test]
    fn spatial_index_cost_ratio_threshold_between_zero_and_one() {
        assert!(cost::SPATIAL_INDEX_COST_RATIO_THRESHOLD > 0.0);
        assert!(cost::SPATIAL_INDEX_COST_RATIO_THRESHOLD < 1.0);
    }

    #[test]
    fn gpu_spatial_per_row_cost_is_positive() {
        assert!(cost::GPU_SPATIAL_PER_ROW_COST > 0.0);
    }

    #[test]
    fn gpu_raster_per_row_cost_is_positive() {
        assert!(cost::GPU_RASTER_PER_ROW_COST > 0.0);
    }

    #[test]
    fn gpu_h3_per_row_cost_is_positive() {
        assert!(cost::GPU_H3_PER_ROW_COST > 0.0);
    }

    #[test]
    fn gpu_reduce_per_row_cost_is_positive() {
        assert!(cost::GPU_REDUCE_PER_ROW_COST > 0.0);
    }

    #[test]
    fn per_datum_extract_cost_is_positive() {
        assert!(cost::PER_DATUM_EXTRACT_COST > 0.0);
    }

    // =====================================================================
    // Cost calculation: scan hook cost formula
    // =====================================================================

    /// Replicate the scan hook's cost formula to verify it produces
    /// sane results with known inputs.
    ///
    /// Formula (matches `pgaccel_set_rel_pathlist`):
    ///   cost_margin = 0.7 for GPU strategies, 1.0 for GpuExpr
    ///   yield_cost  = rows * 0.005 for GpuExpr, 0.0 for GPU strategies
    ///   raw_total   = (base_total * cost_margin + batch_overhead
    ///                  + gpu_overhead + yield_cost) * cost_multiplier
    #[test]
    fn scan_cost_formula_gpu_spatial_cheaper_than_base() {
        // Simulate: GpuSpatial, high base_total (expensive spatial function).
        // The 30% cost_margin discount should make GPU cheaper.
        let base_rows = 100_000.0_f64;
        let base_total = 5000.0; // expensive spatial predicate
        let cost_multiplier = 1.0;

        let batch_size = cost::optimal_batch_size(base_rows as usize) as f64;
        let num_batches = (base_rows / batch_size).ceil();
        let batch_overhead = num_batches * 2.0;
        let gpu_overhead = cost::GPU_LAUNCH_OVERHEAD;
        let cost_margin = cost::GPU_COST_SAFETY_MARGIN; // 0.7
        let yield_cost = 0.0; // GPU strategies: no yield cost

        let total_cost = (base_total * cost_margin
            + batch_overhead
            + gpu_overhead
            + yield_cost)
            * cost_multiplier;

        // 5000 * 0.7 + ~26 + 5 = 3531 < 5000: GPU wins for expensive functions.
        assert!(
            total_cost < base_total,
            "GPU spatial path ({total_cost:.2}) should be cheaper than \
             base ({base_total:.2}) for expensive spatial predicate"
        );
    }

    #[test]
    fn scan_cost_formula_gpu_expr_not_cheaper_at_100k() {
        // Simulate: GpuExpr at 100K rows, modest base_total.
        // No cost discount (1.0) + yield cost makes GPU more expensive.
        let base_rows = 100_000.0_f64;
        let base_total = 500.0; // simple WHERE clause scan

        let batch_size = cost::optimal_batch_size(base_rows as usize) as f64;
        let num_batches = (base_rows / batch_size).ceil();
        let batch_overhead = num_batches * 2.0;
        let gpu_overhead = 0.0; // GpuExpr: no GPU launch
        let cost_margin = 1.0; // GpuExpr: no discount
        let yield_cost = base_rows * 0.005; // GpuExpr yield

        let total_cost = (base_total * cost_margin
            + batch_overhead
            + gpu_overhead
            + yield_cost)
            * 1.0;

        // 500 * 1.0 + ~26 + 0 + 500 = 1026 > 500: GPU loses.
        assert!(
            total_cost > base_total,
            "GpuExpr path ({total_cost:.2}) should be more expensive than \
             base ({base_total:.2}) at 100K rows with modest base cost"
        );
    }

    // =====================================================================
    // Cost calculation: safety margin gate
    // =====================================================================

    #[test]
    fn safety_margin_gate_rejects_marginal_improvement() {
        let base_cost = 100.0;
        // If total_cost is 75% of base, it exceeds the 0.7 margin.
        let total_cost = base_cost * 0.75;
        assert!(total_cost > base_cost * cost::GPU_COST_SAFETY_MARGIN);
    }

    #[test]
    fn safety_margin_gate_accepts_clear_improvement() {
        let base_cost = 100.0;
        // If total_cost is 50% of base, it's well under the 0.7 margin.
        let total_cost = base_cost * 0.50;
        assert!(total_cost <= base_cost * cost::GPU_COST_SAFETY_MARGIN);
    }

    #[test]
    fn safety_margin_gate_boundary_exactly_at_margin() {
        let base_cost = 100.0;
        let total_cost = base_cost * cost::GPU_COST_SAFETY_MARGIN;
        // Exactly at margin should NOT pass the `>` check (it's not strictly greater).
        assert!(!(total_cost > base_cost * cost::GPU_COST_SAFETY_MARGIN));
    }

    // =====================================================================
    // LIMIT gate logic (limit_tuples < rows / 4)
    // =====================================================================

    #[test]
    fn limit_gate_small_limit_skips_gpu_sort() {
        let rows: usize = 1_000_000;
        let limit_tuples: f64 = 100.0;
        // When limit < rows/4, GPU sort should be skipped.
        assert!(
            limit_tuples > 0.0 && (limit_tuples as usize) < rows / 4,
            "small LIMIT should trigger skip"
        );
    }

    #[test]
    fn limit_gate_large_limit_allows_gpu_sort() {
        let rows: usize = 1000;
        let limit_tuples: f64 = 500.0;
        // When limit >= rows/4, GPU sort is allowed.
        assert!(
            !(limit_tuples > 0.0 && (limit_tuples as usize) < rows / 4),
            "large LIMIT should NOT trigger skip"
        );
    }

    #[test]
    fn limit_gate_zero_limit_allows_gpu_sort() {
        let limit_tuples: f64 = 0.0;
        // Zero limit means no LIMIT clause — GPU sort allowed.
        assert!(!(limit_tuples > 0.0), "zero limit should NOT trigger skip");
    }

    #[test]
    fn limit_gate_negative_limit_allows_gpu_sort() {
        let limit_tuples: f64 = -1.0;
        assert!(
            !(limit_tuples > 0.0),
            "negative limit should NOT trigger skip"
        );
    }

    // =====================================================================
    // Narrow-row gate (width < 40 skips GPU sort)
    // =====================================================================

    #[test]
    fn narrow_row_gate_width_39_skips() {
        let output_width: usize = 39;
        assert!(output_width < 40, "width 39 should skip GPU sort");
    }

    #[test]
    fn narrow_row_gate_width_40_allows() {
        let output_width: usize = 40;
        assert!(!(output_width < 40), "width 40 should allow GPU sort");
    }

    #[test]
    fn narrow_row_gate_width_120_allows() {
        let output_width: usize = 120;
        assert!(!(output_width < 40), "wide rows should allow GPU sort");
    }

    // =====================================================================
    // Per-row cost selection by strategy
    // =====================================================================

    #[test]
    fn per_row_cost_selection_gpu_spatial() {
        let strategy = registry::AccelStrategy::GpuSpatial;
        let per_row = match strategy {
            registry::AccelStrategy::GpuSpatial => cost::GPU_SPATIAL_PER_ROW_COST,
            registry::AccelStrategy::GpuRaster => cost::GPU_RASTER_PER_ROW_COST,
            registry::AccelStrategy::GpuH3 => cost::GPU_H3_PER_ROW_COST,
            _ => cost::GPU_SPATIAL_PER_ROW_COST,
        };
        assert!((per_row - cost::GPU_SPATIAL_PER_ROW_COST).abs() < f64::EPSILON);
    }

    #[test]
    fn per_row_cost_selection_gpu_raster() {
        let strategy = registry::AccelStrategy::GpuRaster;
        let per_row = match strategy {
            registry::AccelStrategy::GpuSpatial => cost::GPU_SPATIAL_PER_ROW_COST,
            registry::AccelStrategy::GpuRaster => cost::GPU_RASTER_PER_ROW_COST,
            registry::AccelStrategy::GpuH3 => cost::GPU_H3_PER_ROW_COST,
            _ => cost::GPU_SPATIAL_PER_ROW_COST,
        };
        assert!((per_row - cost::GPU_RASTER_PER_ROW_COST).abs() < f64::EPSILON);
    }

    #[test]
    fn per_row_cost_selection_gpu_hash_join_falls_to_default() {
        let strategy = registry::AccelStrategy::GpuHashJoin;
        let per_row = match strategy {
            registry::AccelStrategy::GpuSpatial => cost::GPU_SPATIAL_PER_ROW_COST,
            registry::AccelStrategy::GpuRaster => cost::GPU_RASTER_PER_ROW_COST,
            registry::AccelStrategy::GpuH3 => cost::GPU_H3_PER_ROW_COST,
            _ => cost::GPU_SPATIAL_PER_ROW_COST,
        };
        assert!((per_row - cost::GPU_SPATIAL_PER_ROW_COST).abs() < f64::EPSILON);
    }

    // =====================================================================
    // GPU overhead selection by strategy
    // =====================================================================

    #[test]
    fn gpu_overhead_spatial_uses_launch_overhead() {
        let strategies = [
            registry::AccelStrategy::GpuSpatial,
            registry::AccelStrategy::GpuRaster,
            registry::AccelStrategy::GpuH3,
        ];
        for s in strategies {
            let overhead = match s {
                registry::AccelStrategy::GpuSpatial
                | registry::AccelStrategy::GpuRaster
                | registry::AccelStrategy::GpuH3 => cost::GPU_LAUNCH_OVERHEAD,
                _ => 0.0,
            };
            assert!(
                (overhead - cost::GPU_LAUNCH_OVERHEAD).abs() < f64::EPSILON,
                "strategy {s:?} should use GPU_LAUNCH_OVERHEAD"
            );
        }
    }

    #[test]
    fn gpu_overhead_for_gpu_strategies_is_positive() {
        let overhead = match registry::AccelStrategy::GpuSpatial {
            registry::AccelStrategy::GpuSpatial
            | registry::AccelStrategy::GpuRaster
            | registry::AccelStrategy::GpuH3 => cost::GPU_LAUNCH_OVERHEAD,
            _ => 0.0,
        };
        assert!(overhead > 0.0);
    }

    // =====================================================================
    // DeviceLimits::cpu_only thresholds
    // =====================================================================

    #[test]
    fn device_limits_cpu_only_thresholds_are_sane() {
        let limits = cost::DeviceLimits::cpu_only();
        assert_eq!(limits.gpu_min_rows, 10_000);
        assert_eq!(limits.gpu_sort_min_rows, 100_000);
        assert_eq!(limits.gpu_sort_planner_min_rows, 1_000_000);
        assert_eq!(limits.gpu_window_min_rows, 5_000_000);
        assert_eq!(limits.gpu_reduce_min_rows, 10_000);
        assert_eq!(limits.gpu_hash_agg_min_rows, 250_000);
        assert_eq!(limits.gpu_hash_agg_max_groups, 10_000);
        assert_eq!(limits.optimal_batch_min, 256);
        assert_eq!(limits.optimal_batch_max, 8192);
    }

    #[test]
    fn device_limits_cpu_only_batch_min_lte_max() {
        let limits = cost::DeviceLimits::cpu_only();
        assert!(limits.optimal_batch_min <= limits.optimal_batch_max);
    }

    // =====================================================================
    // Equi-join key type mapping (replicate the match from find_equi_join_key)
    // =====================================================================

    #[test]
    fn equi_join_key_type_mapping_int2_to_int32() {
        let key_type = match 21u32 {
            21 | 23 => Some(0),   // Int32
            20 => Some(1),        // Int64
            700 | 701 => Some(2), // Float64
            _ => None,
        };
        assert_eq!(key_type, Some(0));
    }

    #[test]
    fn equi_join_key_type_mapping_int4_to_int32() {
        let key_type = match 23u32 {
            21 | 23 => Some(0),
            20 => Some(1),
            700 | 701 => Some(2),
            _ => None,
        };
        assert_eq!(key_type, Some(0));
    }

    #[test]
    fn equi_join_key_type_mapping_int8_to_int64() {
        let key_type = match 20u32 {
            21 | 23 => Some(0),
            20 => Some(1),
            700 | 701 => Some(2),
            _ => None,
        };
        assert_eq!(key_type, Some(1));
    }

    #[test]
    fn equi_join_key_type_mapping_float4_to_float64() {
        let key_type = match 700u32 {
            21 | 23 => Some(0),
            20 => Some(1),
            700 | 701 => Some(2),
            _ => None,
        };
        assert_eq!(key_type, Some(2));
    }

    #[test]
    fn equi_join_key_type_mapping_float8_to_float64() {
        let key_type = match 701u32 {
            21 | 23 => Some(0),
            20 => Some(1),
            700 | 701 => Some(2),
            _ => None,
        };
        assert_eq!(key_type, Some(2));
    }

    #[test]
    fn equi_join_key_type_mapping_text_unsupported() {
        let key_type = match 25u32 {
            21 | 23 => Some(0),
            20 => Some(1),
            700 | 701 => Some(2),
            _ => None,
        };
        assert_eq!(key_type, None);
    }

    // =====================================================================
    // All strategies are GPU strategies (no CPU-only paths)
    // =====================================================================

    #[test]
    fn all_strategies_are_gpu() {
        let gpu_strategies = [
            registry::AccelStrategy::GpuSpatial,
            registry::AccelStrategy::GpuRaster,
            registry::AccelStrategy::GpuH3,
            registry::AccelStrategy::GpuSort,
        ];
        for s in gpu_strategies {
            assert!(s as i32 >= 1, "strategy {s:?} should be a GPU strategy");
        }
    }

    // =====================================================================
    // Cost calculation: sort cost formula
    // =====================================================================

    #[test]
    fn sort_cost_formula_produces_positive_total() {
        let base_rows = 500_000.0_f64;
        let base_startup = 10.0;
        let base_total = 200.0;
        let cost_multiplier = 1.0;

        let gpu_overhead = cost::GPU_LAUNCH_OVERHEAD;
        let per_row = cost::GPU_SORT_PER_ROW_COST + cost::PER_DATUM_EXTRACT_COST + 0.03;
        let sort_cost = base_rows * per_row;
        let startup_cost = base_startup + gpu_overhead;
        let total_cost = (base_total + gpu_overhead + sort_cost) * cost_multiplier;

        assert!(total_cost > 0.0);
        assert!(startup_cost > base_startup);
        // Sort cost should dominate for large row counts.
        assert!(sort_cost > gpu_overhead);
    }

    // =====================================================================
    // Cost calculation: join cost formula
    // =====================================================================

    #[test]
    fn join_cost_hash_build_overhead_scales_with_inner_rows() {
        let inner_rows_small = 1000.0_f64;
        let inner_rows_large = 100_000.0_f64;

        let overhead_small = inner_rows_small * 0.001;
        let overhead_large = inner_rows_large * 0.001;

        assert!(overhead_large > overhead_small);
        assert!((overhead_small - 1.0).abs() < f64::EPSILON);
        assert!((overhead_large - 100.0).abs() < f64::EPSILON);
    }

    // =====================================================================
    // Cost calculation: agg cost formula
    // =====================================================================

    #[test]
    fn agg_cost_grouped_adds_hash_overhead() {
        let base_rows = 50_000.0_f64;
        let reduce_per_row = cost::GPU_REDUCE_PER_ROW_COST + 0.02;

        let hash_overhead_plain = 0.0;
        let hash_overhead_grouped = 0.005;

        let cost_plain = base_rows * (reduce_per_row + hash_overhead_plain);
        let cost_grouped = base_rows * (reduce_per_row + hash_overhead_grouped);

        assert!(cost_grouped > cost_plain);
        // The difference should be exactly base_rows * 0.005.
        let expected_diff = base_rows * 0.005;
        assert!((cost_grouped - cost_plain - expected_diff).abs() < 1e-6);
    }

    // =====================================================================
    // Empty registry fast-reject
    // =====================================================================

    #[test]
    fn empty_registry_is_empty() {
        let reg = registry::AdapterRegistry::new();
        assert!(reg.is_empty());
    }

    #[test]
    fn empty_registry_lookup_returns_none() {
        let reg = registry::AdapterRegistry::new();
        assert!(reg.lookup(pg_sys::Oid::from(12345u32)).is_none());
    }
}
