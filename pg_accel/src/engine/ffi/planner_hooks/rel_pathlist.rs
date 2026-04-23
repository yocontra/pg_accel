//! `set_rel_pathlist_hook` — injects `CustomPath`s for base relations.
//!
//! Entry points:
//! - `pgaccel_set_rel_pathlist` — main hook fn (registered in `install()`)
//! - `try_inject_gpu_sort_path` — scan-level GPU sort injection helper

use pgrx::pg_sys::{
    self, CustomPath, List, NodeTag, Path, PlannerInfo, RangeTblEntry, RelOptInfo, add_path,
    lappend,
};

use super::super::custom_scan;
use super::{
    AccelMatch, PREV_SET_REL_PATHLIST_HOOK, create_custom_path, extract_const_geom_vertex_count,
    find_accelerable_match, find_cheapest_path, find_cheapest_seqscan_cost,
    find_cheapest_seqscan_path, has_cheap_spatial_index_path, has_cheaper_spatial_index_path,
    try_gpu_expr_match,
};
use crate::engine::cost;
use crate::engine::executor::sort::SortKeyDesc;
use crate::engine::gucs;
use crate::engine::registry;
use crate::engine::stats;

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
pub(super) unsafe extern "C-unwind" fn pgaccel_set_rel_pathlist(
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

    // Record this planner hook invocation (main backend thread only).
    stats::record_planner_hook_call();

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
        stats::record_command_type_skip();
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

    let _span =
        tracing::info_span!("planner.rel_pathlist", relid = u32::from(rte_ref.relid)).entered();

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
    let per_row_cost_base = match strategy {
        registry::AccelStrategy::GpuRaster => cost::GPU_RASTER_PER_ROW_COST,
        registry::AccelStrategy::GpuH3 => cost::GPU_H3_PER_ROW_COST,
        registry::AccelStrategy::GpuExpr => cost::GPU_EXPR_PER_ROW_COST,
        _ => cost::GPU_SPATIAL_PER_ROW_COST,
    };
    // fp64 classification via the adapter helper (`adapters::uses_fp64`).
    // Spatial always uses fp64 (recheck path); H3 uses fp64 only for
    // `h3_latlng_to_cell`; raster/expr report false. For strategies where
    // the function name can't be resolved (e.g. fn_oid = InvalidOid on a
    // synthetic GpuExpr match), default to `false`. The helper centralises
    // the per-function classification so it's consistent with the adapters'
    // own declarations in `pg_accel/src/adapters/*.rs`.
    let fn_uses_fp64: bool = {
        let fn_oid = accel.fn_oid;
        if fn_oid == pg_sys::Oid::INVALID {
            false
        } else {
            // SAFETY: get_func_name is a catalog lookup safe on the main
            // backend thread. Returns null for unknown OIDs, caller frees
            // via pfree (but we don't free here — pstrdup-backed pointer
            // lives for the current memory context).
            let name_ptr = unsafe { pg_sys::get_func_name(fn_oid) };
            if name_ptr.is_null() {
                false
            } else {
                // SAFETY: name_ptr is a null-terminated C string from pg_proc.
                let name_cstr = unsafe { std::ffi::CStr::from_ptr(name_ptr) };
                match name_cstr.to_str() {
                    Ok(s) => crate::adapters::uses_fp64(strategy, s),
                    Err(_) => false,
                }
            }
        }
    };
    let per_row_cost =
        cost::apply_fp64_penalty(per_row_cost_base, fn_uses_fp64, cost::device_limits());
    if !cost::should_batch(rows, per_row_cost, min_batch) {
        pgrx::debug1!(
            "pg_accel: set_rel_pathlist: batch rejected tuples={} min_batch={}",
            rows,
            min_batch
        );
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

    // Gate 4a-raster: Raster operations need significantly more rows to
    // amortize GPU overhead than spatial predicates. Benchmark (2026-04-12)
    // shows 0.62x at 10K but 1.38x at 100K. Require 5x the device-derived
    // gpu_min_rows (typically 50K) before injecting a GpuRaster path.
    if strategy == registry::AccelStrategy::GpuRaster {
        let raster_min = cost::device_limits().gpu_min_rows * 5;
        if rows < raster_min {
            pgrx::debug1!(
                "pg_accel: set_rel_pathlist: GpuRaster rejected rows={} < raster_min={}",
                rows,
                raster_min
            );
            return;
        }
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

    // Gate 4c: Vertex count thresholds for spatial predicates.
    //
    // Two-sided gate:
    //
    // - LOWER gate: GPU overhead is ~19ms constant (geometry deser + seq
    //   scan), regardless of polygon complexity. PG parallel scales linearly
    //   with vertex count. Below the threshold, PG parallel is faster.
    //
    // - UPPER gate: The relevant work metric for point-in-polygon is the
    //   product `vertex_count * row_count`. Megapoly fixtures (e.g.
    //   `scale_1m_mega500v` with a 1M-vertex polygon and 10M rows) push
    //   this product into the 10^13 range, which fp32 PIP kernels can't
    //   amortize against transfer. Above the max product the megapoly
    //   loser path is strictly worse than PG parallel — skip injection
    //   entirely.
    //
    // Both thresholds come from `DeviceLimits` per CLAUDE.md rule 10;
    // they are calibrated from the 2026-04-11 benchmark run and derived
    // from the hardware profile in `from_profile`.
    if strategy == registry::AccelStrategy::GpuSpatial {
        let dl = cost::device_limits();
        let min_verts = dl.gpu_spatial_min_vertices;
        let min_product = dl.spatial_point_in_ring_break_even_verts_x_rows;
        let max_product = dl.spatial_point_in_ring_max_verts_x_rows;
        // SAFETY: rel_ref.baserestrictinfo is a valid List from the planner.
        // Treat unknown vertex count (None) as 0 — conservatively reject
        // when the planner cannot confirm the polygon has enough vertices.
        // This prevents injection for FuncExpr geometry args (ST_Buffer,
        // ST_MakeEnvelope) that PG didn't constant-fold.
        let vcount =
            unsafe { extract_const_geom_vertex_count(rel_ref.baserestrictinfo) }.unwrap_or(0);
        if vcount < min_verts {
            pgrx::debug1!(
                "pg_accel: spatial vertex gate: vcount={} < min={}, skipping",
                vcount,
                min_verts
            );
            return;
        }
        // Compute the work product as u64 to avoid overflow at 10^13.
        let work_product = (vcount as u64).saturating_mul(rows as u64);
        if work_product < min_product {
            pgrx::debug1!(
                "pg_accel: spatial work-product gate: vcount={} * rows={} = {} < min {}, \
                 skipping (below break-even)",
                vcount,
                rows,
                work_product,
                min_product
            );
            return;
        }
        if work_product > max_product {
            pgrx::debug1!(
                "pg_accel: spatial work-product gate: vcount={} * rows={} = {} > max {}, \
                 skipping (megapoly loser path)",
                vcount,
                rows,
                work_product,
                max_product
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
    // Custom Scan runs single-threaded. Do NOT add parallel_setup_cost to
    // our startup — we already compare against PG's cheapest NON-parallel
    // seqscan downstream (find_cheapest_seqscan_path). Adding the parallel
    // penalty here was a double charge that pushed us out of contention
    // for every parallelizable relation, which is exactly the set of
    // queries we want to accelerate.
    let startup_cost = base.startup_cost + 1.0 + gpu_overhead;

    // All GPU strategies get a cost-margin discount vs PG's serial seqscan.
    // Compute-bound spatial/h3/raster operations get a more aggressive margin
    // because GPU batched SIMD evaluation is dramatically faster than PG's
    // per-row function call loop (even parallelized across workers), and PG's
    // parallel cost model divides by worker count which underestimates the
    // gap. GpuExpr uses the lighter margin since the inline template is
    // only modestly faster than PG's expression evaluation.
    let cost_margin = match strategy {
        registry::AccelStrategy::GpuSpatial
        | registry::AccelStrategy::GpuRaster
        | registry::AccelStrategy::GpuH3 => cost::GPU_COST_SAFETY_MARGIN * 0.5,
        _ => cost::GPU_COST_SAFETY_MARGIN,
    };
    // Soft-fp64 per-row overhead: when the classified function uses fp64
    // and the device lacks native fp64, add a per-row penalty equal to the
    // unpenalised per-row cost multiplied by `(multiplier - 1.0)`. This is
    // the extra work the emulation performs vs native fp32. Zero when
    // `fn_uses_fp64 == false` or the device has native fp64 (the penalty
    // simplifies to `per_row_cost_base * (multiplier - 1.0)` only when the
    // penalty was actually applied above; otherwise per_row_cost equals
    // per_row_cost_base and the delta is zero).
    #[allow(clippy::cast_precision_loss)]
    let soft_fp64_overhead = rows as f64 * (per_row_cost - per_row_cost_base);
    let raw_total =
        (base.total_cost.mul_add(cost_margin, batch_overhead) + gpu_overhead + soft_fp64_overhead)
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
pub(super) const SORT_INT4OID: u32 = 23;
pub(super) const SORT_INT8OID: u32 = 20;
pub(super) const SORT_FLOAT4OID: u32 = 700;
pub(super) const SORT_FLOAT8OID: u32 = 701;

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

    // No max-rows gate: the executor handles arbitrary row counts via
    // chunked GPU sort (sort in chunks of gpu_sort_max_elements, then
    // k-way merge). GPU sort is beneficial at any scale.

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

    // Width is used for cost estimation but not as a hard gate —
    // the cost model decides whether GPU sort is beneficial.
    let _output_width = unsafe { (*(*rel).reltarget).width } as usize;

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
    if !matches!(
        var_typid,
        SORT_INT4OID | SORT_INT8OID | SORT_FLOAT4OID | SORT_FLOAT8OID
    ) {
        pgrx::debug1!("pg_accel sort: unsupported type {var_typid}");
        return;
    }

    // Gate: User-facing escape valve. `pg_accel.fp64_enabled=false` fully
    // bypasses GPU injection for fp64 sort keys — PG native plan runs
    // untouched. Soft-fp64 on Metal is otherwise always correct; the
    // planner's soft-fp64 cost multiplier (see DeviceLimits) steers
    // cost-based decisions at runtime.
    let uses_fp64 = var_typid == SORT_FLOAT8OID;
    if uses_fp64 && !crate::fp64_enabled() {
        pgrx::debug1!("pg_accel sort: float8 sort skipped — fp64_enabled=false");
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

    // Universal cost model for self-scanning sort path.
    // Sort extracts 1 key column; output includes all columns (yield overhead
    // accounted for in scan_cost component of self_scan_cost).
    // SAFETY: cheapest is non-null.
    let base = unsafe { &*cheapest };
    #[allow(clippy::cast_precision_loss)]
    let n = base.rows;
    let sort_scan_cost = cost::self_scan_cost_fp64_aware(
        n,
        1, // single sort key column
        cost::device_limits().gpu_op_cost_sort,
        uses_fp64,
    );
    let startup_cost = base.startup_cost + cost::GPU_LAUNCH_OVERHEAD;
    // Honest cost — no cost_multiplier. Let add_path() decide.
    let total_cost = base.total_cost + sort_scan_cost;

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

        // Serialize self_scan_relid for VectorizedScan. When the child is a
        // plain SeqScan on a base relation, we can bypass ExecProcNode and
        // scan the heap directly.
        let self_scan_relid = if cheapest.is_null() {
            0
        } else {
            // SAFETY: cheapest is the child path; check if it's a simple
            // Path on a base relation (i.e., parent is a baserel).
            let parent = (*cheapest).parent;
            if !parent.is_null()
                && (*parent).rtekind == pg_sys::RTEKind::RTE_RELATION
                && (*parent).relid > 0
            {
                (*parent).relid as i32
            } else {
                0
            }
        };
        priv_list = lappend(priv_list, pg_sys::makeInteger(self_scan_relid).cast());
        (*cpath).custom_private = priv_list;

        add_path(rel, cpath.cast());

        pgrx::debug1!(
            "pg_accel: injected GpuSort scan path, attno={}, rows={}",
            var_attno,
            rows
        );
    }
}
