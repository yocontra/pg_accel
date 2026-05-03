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
use crate::engine::stats;

mod agg;
mod agg_common;
mod hashjoin;
mod join_pathlist;
mod partial_agg;
mod preagg_partial;
pub(super) mod projectset;
mod rel_pathlist;
mod scan;
mod sort;
mod srf_target_list;
mod window;

use join_pathlist::pgaccel_set_join_pathlist;
use rel_pathlist::pgaccel_set_rel_pathlist;
#[cfg(feature = "pg_test")]
use rel_pathlist::{SORT_FLOAT4OID, SORT_FLOAT8OID, SORT_INT4OID, SORT_INT8OID};

// ---------------------------------------------------------------------------
// Previous hook storage
// ---------------------------------------------------------------------------

pub(super) static mut PREV_SET_REL_PATHLIST_HOOK: pg_sys::set_rel_pathlist_hook_type = None;
pub(super) static mut PREV_SET_JOIN_PATHLIST_HOOK: pg_sys::set_join_pathlist_hook_type = None;
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

    // Record this planner hook invocation (main backend thread only).
    stats::record_planner_hook_call();

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
        stats::record_command_type_skip();
        return;
    }

    let _span = tracing::info_span!("planner.upper_paths", stage = stage).entered();

    // Dispatch by upper relation stage.
    match stage {
        pg_sys::UpperRelationKind::UPPERREL_GROUP_AGG => {
            // 1) Fused star-join pre-aggregation (most benefit when it matches).
            // SAFETY: All pointers are valid planner arguments.
            unsafe { preagg_partial::try_inject(root, input_rel, output_rel) };
            unsafe { pgaccel_inject_gpu_preagg(root, input_rel, output_rel) };

            // 2) Standard (non-parallel) GpuAgg path.
            // SAFETY: All pointers are valid planner arguments.
            unsafe { agg::inject(root, input_rel, output_rel) };

            // 3) Partial / parallel GpuAgg path.
            //
            // PG 17 doesn't fire the upper_paths hook for
            // `UPPERREL_PARTIAL_GROUP_AGG` directly, and by the time the
            // `UPPERREL_GROUP_AGG` hook fires, core has already run
            // `gather_grouping_paths` + `add_paths_to_grouping_rel`. Adding
            // a partial CustomPath to `partially_grouped_rel->partial_pathlist`
            // here would be orphaned (no Gather, no Finalize Agg).
            //
            // `partial_agg::try_inject` instead builds the full
            // Finalize Agg → Gather → GpuAccel(partial) chain itself and
            // adds the Finalize AggPath directly to `grouped_rel->pathlist`.
            // SAFETY: All pointers are valid planner arguments.
            unsafe { partial_agg::try_inject(root, input_rel, output_rel) };
        }
        pg_sys::UpperRelationKind::UPPERREL_WINDOW => {
            // SAFETY: All pointers are valid planner arguments.
            unsafe { pgaccel_inject_gpu_window(root, input_rel, output_rel) };
        }
        pg_sys::UpperRelationKind::UPPERREL_FINAL => {
            // SRF-in-target-list detection (Phase 2 follow-up to F3).
            // Walks the input rel's pathlist for ProjectSetPath nodes
            // wrapping registered SRFs. Currently planner-side only —
            // see `srf_target_list.rs` module doc "Status (anti-cheat
            // ban #9)" for the executor-wiring follow-up.
            // SAFETY: All pointers are valid planner arguments.
            unsafe { srf_target_list::try_inject_srf_target_list(root, input_rel, output_rel) };
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

        // Gate: Per-function rejection based on 2026-04-12 benchmark results.
        // Ranking and offset window functions (ROW_NUMBER, RANK, DENSE_RANK,
        // LAG, LEAD) consistently lose to PG parallel at all scales
        // (0.46-0.93x). Only aggregate window functions (SUM, COUNT, AVG,
        // MIN, MAX) show GPU wins. Skip injection for the losing functions.
        if matches!(
            wfunc_enum,
            WindowFunc::RowNumber
                | WindowFunc::Rank
                | WindowFunc::DenseRank
                | WindowFunc::Lag
                | WindowFunc::Lead
        ) {
            pgrx::debug1!(
                "pg_accel window: rejecting {:?} — benchmark shows GPU loses at all scales",
                wfunc_enum
            );
            continue;
        }

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

        // Extract ORDER BY attno AND its Var's vartype.
        // The vartype feeds `uses_fp64` so the cost site can apply the
        // soft-fp64 penalty on devices without native fp64 (see
        // `cost::apply_fp64_penalty`).
        let mut order_attno_type: u32 = 0;
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
                        let var = te_expr.cast::<pg_sys::Var>();
                        attno = i32::from(unsafe { (*var).varattno });
                        order_attno_type = u32::from(unsafe { (*var).vartype });
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

        // Extract value column attno AND its Var's vartype for aggregate-like
        // window funcs. The vartype feeds `uses_fp64` alongside the ORDER BY
        // vartype above.
        let mut value_attno = 0i32;
        let mut value_attno_type: u32 = 0;
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
                    value_attno_type = typid;
                }
            }
            // LAG/LEAD offset (second argument, if present and constant).
            if nargs >= 2 {
                // SAFETY: second arg is an Expr* node.
                let offset_expr = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Expr>() };
                if !offset_expr.is_null()
                    && unsafe { (*offset_expr.cast::<pg_sys::Node>()).type_ } == NodeTag::T_Const
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
                        default_val = f64::from_bits(unsafe { (*cst).constvalue.value() } as u64);
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

        // fp64 classification: set when either the ORDER BY column or the
        // value column is float8 (FLOAT8OID = 701). The cost site feeds this
        // into `cost::self_scan_cost_fp64_aware` so soft-fp64 devices see the
        // penalty. float4 stays false because AdaptiveCpp lowers float4 ops
        // natively on Metal; only float8 hits the soft-fp64 path.
        let spec_uses_fp64 = order_attno_type == WIN_FLOAT8OID || value_attno_type == WIN_FLOAT8OID;

        specs.push(WindowFuncSpec {
            func: wfunc_enum,
            partition_attno,
            order_attno,
            value_attno,
            offset: lag_offset,
            default_val,
            result_type_oid,
            uses_fp64: spec_uses_fp64,
        });
    }

    if specs.is_empty() {
        pgrx::debug1!("pg_accel window: no supported window functions found");
        return;
    }

    // Window functions require input sorted by PARTITION BY + ORDER BY.
    // PG stores the required sort keys in root->window_pathkeys. Prefer a
    // path that is ALREADY ordered by those keys (e.g. an IndexOnlyScan
    // over the PARTITION/ORDER columns) over wrapping the seqscan with a
    // fresh Sort — the pre-sorted path is often 2-3x cheaper and lets our
    // Custom Scan beat PG's WindowAgg on the same input.
    let window_pathkeys = unsafe { (*root).window_pathkeys };
    let has_window_pathkeys =
        !window_pathkeys.is_null() && unsafe { pg_sys::list_length(window_pathkeys) } > 0;

    // First try to find an already-sorted path for the window pathkeys.
    // SAFETY: get_cheapest_path_for_pathkeys is a standard PG planner
    // helper: (pathlist, pathkeys, required_outer, cost_criterion,
    // require_parallel_safe) -> Path*.
    let presorted_path = if has_window_pathkeys {
        unsafe {
            pg_sys::get_cheapest_path_for_pathkeys(
                input_ref.pathlist,
                window_pathkeys,
                std::ptr::null_mut(),
                pg_sys::CostSelector::TOTAL_COST,
                false,
            )
        }
    } else {
        std::ptr::null_mut()
    };

    let (cheapest, sorted_path) = if presorted_path.is_null() {
        // No pre-sorted path. Fall back to cheapest + explicit Sort.
        let cheapest = unsafe { find_cheapest_path(input_ref.pathlist) };
        if cheapest.is_null() {
            return;
        }
        let sorted = if has_window_pathkeys {
            // SAFETY: All pointers from the planner; create_sort_path pallocs.
            unsafe {
                pg_sys::create_sort_path(root, input_rel, cheapest, window_pathkeys, -1.0)
                    .cast::<Path>()
            }
        } else {
            cheapest
        };
        (cheapest, sorted)
    } else {
        (presorted_path, presorted_path)
    };
    let _ = cheapest;

    // Detect whether we can do a direct heap scan (vectorized path).
    // This is possible when:
    // 1. No sort is needed (no PARTITION BY, no ORDER BY), AND
    // 2. The cheapest input path is on a simple base relation
    //
    // When these conditions hold, we can scan the heap directly instead of
    // pulling tuples through ExecProcNode on a child SeqScan plan.
    let no_sort_needed =
        window_pathkeys.is_null() || unsafe { pg_sys::list_length(window_pathkeys) } == 0;

    // SAFETY: cheapest path's parent is the input_rel.
    let scan_relid: pg_sys::Index = if no_sort_needed {
        let parent = unsafe { (*cheapest).parent };
        if parent.is_null() {
            0
        } else {
            let rel = unsafe { &*parent };
            // A simple base relation has relid > 0 and reloptkind == RELOPT_BASEREL.
            if rel.relid > 0 && rel.reloptkind == pg_sys::RelOptKind::RELOPT_BASEREL {
                rel.relid
            } else {
                0
            }
        }
    } else {
        0
    };

    // Universal cost model for window path via self_scan_cost.
    // Count extracted columns: each spec contributes partition + order + value columns.
    // SAFETY: sorted_path is non-null.
    let base = unsafe { &*sorted_path };
    #[allow(clippy::cast_precision_loss)]
    let num_extract_cols: usize = specs
        .iter()
        .map(|s| {
            let mut cols = 0usize;
            if s.partition_attno > 0 {
                cols += 1;
            }
            if s.order_attno > 0 {
                cols += 1;
            }
            if s.value_attno > 0 {
                cols += 1;
            }
            cols
        })
        .sum::<usize>()
        .max(1);
    // fp64 aggregation across specs: if any spec's ORDER BY / value column
    // is float8, route through the fp64-aware cost helper. On soft-fp64
    // devices this multiplies the per-row GPU op cost by
    // `DeviceLimits::soft_fp64_cost_multiplier` (~32x on Metal) so the
    // planner does not over-estimate the GPU win and pick Custom Scan for a
    // workload that would lose to PG's vectorised CPU path.
    let window_uses_fp64 = specs.iter().any(|s| s.uses_fp64);
    let window_scan_cost = cost::self_scan_cost_fp64_aware(
        base.rows,
        num_extract_cols,
        cost::device_limits().gpu_op_cost_window * specs.len() as f64,
        window_uses_fp64,
    );
    let startup_cost = base.total_cost + cost::GPU_LAUNCH_OVERHEAD;
    let total_cost = base.total_cost + window_scan_cost;

    // Cost gate: compare against PG's cheapest NON-parallel window path.
    // Custom Scan is single-threaded; comparing against a parallel plan
    // systematically excludes us even when the GPU kernel would beat both
    // serial and parallel PG at runtime. The gate ratio is tunable via
    // DeviceLimits::gpu_window_cost_ratio.
    let output_ref = unsafe { &*output_rel };
    let pg_best_cost = unsafe { find_cheapest_nonparallel_total_cost(output_ref.pathlist) };
    let ratio = cost::device_limits().gpu_window_cost_ratio;
    if pg_best_cost > 0.0 && total_cost >= pg_best_cost * ratio {
        pgrx::debug1!(
            "pg_accel window: cost {:.1} >= {:.0}% of PG serial best {:.1}, skipping",
            total_cost,
            ratio * 100.0,
            pg_best_cost,
        );
        return;
    }

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
        // Layout: [num_specs, spec0..., scan_relid]
        // The scan_relid is appended after all specs so the existing
        // deserialization of specs (starting at index 0) is unchanged.
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
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(i32::from(spec.uses_fp64)).cast(),
            );
        }

        // Append scan_relid for direct heap scan (0 = use child plan).
        #[allow(clippy::cast_possible_wrap)]
        let relid_int = scan_relid as i32;
        priv_list = lappend(priv_list, pg_sys::makeInteger(relid_int).cast());

        (*cpath).custom_private = priv_list;

        add_path(output_rel, cpath.cast());

        pgrx::debug1!(
            "pg_accel: injected GpuWindow path for {} spec(s), rows={}, scan_relid={}",
            specs.len(),
            rows,
            scan_relid
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
    if !cost::gpu_is_usable() {
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
    #[allow(unused_assignments)]
    let mut fact_rows: f64 = 0.0;
    #[allow(unused_assignments)]
    let mut fact_relid: pg_sys::Index = 0;
    #[allow(unused_assignments)]
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
                let equi =
                    unsafe { find_equi_join_from_restrictinfo(restrict, outer_path, inner_path) };
                let Some(equi) = equi else {
                    pgrx::debug1!("pg_accel: preagg rejected: no equi-join key found");
                    return;
                };

                // SAFETY: inner_path.parent is the inner relation's RelOptInfo.
                let inner_parent = unsafe { (*inner_path).parent };
                let inner_relid: pg_sys::Index = if inner_parent.is_null() {
                    0
                } else {
                    unsafe { (*inner_parent).relid }
                };

                // Extract dimension-side filter predicates from baserestrictinfo.
                // SAFETY: inner_parent is a valid RelOptInfo.
                let dim_filters = if inner_parent.is_null() {
                    vec![]
                } else {
                    unsafe { extract_dim_filters_from_rel(inner_parent) }
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
                fact_scan_expr =
                    unsafe { compile_restrictinfo_to_template(parent_ref.baserestrictinfo) };
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
            // Reject SUM(numeric): the partial-emit path in
            // engine/executor/preagg/partial_emit.rs maps NUMERICOID → NumericSumEmitter
            // which accumulates through the f64 `ColumnAccumulator.sum`, silently
            // losing precision above 2^53. Route NUMERIC arguments back to PG.
            // Paired with the gate in agg_common::classify_aggref for the partial-agg
            // path; both sites must agree.
            if u32::from(aggfnoid) == pg_sys::F_SUM_NUMERIC {
                pgrx::debug1!(
                    "pg_accel: preagg rejected: SUM(numeric) would lose precision above 2^53"
                );
                return;
            }

            // Get the aggregate's argument attno (if any).
            let args = unsafe { (*aggref).args };
            let arg_attno = if args.is_null() || unsafe { pg_sys::list_length(args) } == 0 {
                0 // COUNT(*)
            } else {
                // First argument.
                let te = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::TargetEntry>() };
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
            let (source, attno) =
                resolve_var_to_star_source(varno, varattno, fact_relid, &inner_relids);

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

    // Gate: only inject if cheaper than PG's cheapest NON-parallel agg
    // path, scaled by gpu_preagg_cost_ratio. We compare against the serial
    // baseline rather than the parallel Gather plan so the star-join
    // fusion can actually run for SSBM and similar OLAP workloads where
    // PG's parallel HashJoin cost looks artificially cheap on paper.
    let output_ref = unsafe { &*output_rel };
    let pg_best_cost = unsafe { find_cheapest_nonparallel_total_cost(output_ref.pathlist) };
    let ratio = cost::device_limits().gpu_preagg_cost_ratio;
    if pg_best_cost > 0.0 && total_cost >= pg_best_cost * ratio {
        pgrx::debug1!(
            "pg_accel: preagg rejected: cost {:.1} >= {:.0}% of PG serial best {:.1}",
            total_cost,
            ratio * 100.0,
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

        // Resolve the fact table's RTE into a stable relation OID. The
        // range-table index (`fact_relid`) can be rewritten by
        // `set_plan_refs` for upper plans (scanrelid=0, spans a join), but
        // the OID is stable across plan rewrites. We carry both through
        // custom_private so execution can prefer the OID.
        let fact_oid = {
            let rte_array = root_ref.simple_rte_array;
            if rte_array.is_null() || fact_relid == 0 {
                pg_sys::InvalidOid
            } else {
                let rte = *rte_array.offset(fact_relid as isize);
                if rte.is_null() || (*rte).rtekind != pg_sys::RTEKind::RTE_RELATION {
                    pg_sys::InvalidOid
                } else {
                    (*rte).relid
                }
            }
        };

        // Serialize PreAgg metadata into custom_private. `partial` is None
        // here — this is the serial PreAgg planner hook
        // (`pgaccel_inject_gpu_preagg`); the parallel preagg path lives in
        // `planner_hooks/preagg_partial.rs` and passes a `Some(spec)`.
        (*cpath).custom_private = custom_scan::serialize_preagg_private(
            fact_relid,
            fact_oid,
            &depths,
            &agg_descs,
            &group_keys,
            fact_scan_expr.as_ref(),
            None,
        );

        add_path(output_rel, cpath.cast());
    }
}

/// Extract a Var's varattno as i32. Returns 0 if expr is not a Var.
/// Unwraps RelabelType / FuncExpr casts (e.g. `val::float8`) to find the
/// inner Var.
unsafe fn extract_var_attno(expr: *mut pg_sys::Expr) -> i32 {
    if expr.is_null() {
        return 0;
    }
    let tag = unsafe { (*expr.cast::<pg_sys::Node>()).type_ };
    match tag {
        NodeTag::T_Var =>
        {
            #[allow(clippy::cast_ptr_alignment)]
            i32::from(unsafe { (*expr.cast::<pg_sys::Var>()).varattno })
        }
        NodeTag::T_RelabelType => {
            // Cast that doesn't change representation (e.g. int4 → int8 in some contexts).
            // SAFETY: validated by tag check.
            let inner = unsafe { (*expr.cast::<pg_sys::RelabelType>()).arg };
            unsafe { extract_var_attno(inner) }
        }
        NodeTag::T_FuncExpr => {
            // Explicit cast like val::float8.  The first argument is the input.
            // SAFETY: validated by tag check.
            let func = expr.cast::<pg_sys::FuncExpr>();
            let args = unsafe { (*func).args };
            if args.is_null() || unsafe { pg_sys::list_length(args) } == 0 {
                return 0;
            }
            // SAFETY: args[0] is a valid Expr node.
            let first_arg = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Expr>() };
            unsafe { extract_var_attno(first_arg) }
        }
        NodeTag::T_OpExpr => {
            // For expressions like (lo_revenue - lo_supplycost), return 0
            // to signal "complex expression" which we'll handle as COUNT-like.
            0
        }
        _ => 0,
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
                2 => 4,
                3 => 5,
                4 => 2,
                5 => 3,
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
unsafe fn extract_dim_filters_from_rel(inner_rel: *mut pg_sys::RelOptInfo) -> Vec<DimFilter> {
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
                2 => 4,         // LT → GT
                3 => 5,         // LE → GE
                4 => 2,         // GT → LT
                5 => 3,         // GE → LE
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
        21 => Some(f64::from(datum.value() as i16)), // INT2
        23 => Some(f64::from(datum.value() as i32)), // INT4
        20 => Some((datum.value() as i64) as f64),   // INT8
        700 => Some(f64::from(f32::from_bits(datum.value() as u32))), // FLOAT4
        701 => Some(f64::from_bits(datum.value() as u64)), // FLOAT8
        1082 => Some(f64::from(datum.value() as i32)), // DATE (stored as int32)
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
pub(super) unsafe fn pgaccel_inject_gpu_agg(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
    is_partial: bool,
) {
    // Gate: GPU must be available and enabled.
    if !cost::gpu_is_usable() {
        pgrx::debug1!("pg_accel: gpu_agg rejected: gpu not available or disabled");
        return;
    }

    // Partial-agg gate: only inject when PG has already populated a partial
    // pathlist on the input rel. If no partial paths exist, parallel execution
    // isn't viable for this query shape — don't force it.
    if is_partial {
        let input_ref = unsafe { &*input_rel };
        if input_ref.partial_pathlist.is_null()
            || unsafe { pg_sys::list_length(input_ref.partial_pathlist) } == 0
        {
            pgrx::debug1!("pg_accel: gpu_agg partial skipped: no partial paths on input rel");
            return;
        }
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
        pgrx::debug1!(
            "pg_accel: gpu_agg rejected: GROUP BY has {} cols (max 2)",
            group_len
        );
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
        let sc =
            unsafe { pg_sys::list_nth(query.groupClause, 1).cast::<pg_sys::SortGroupClause>() };
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
    if group_key_info.is_some() && rows < cost::device_limits().gpu_hash_agg_min_rows {
        pgrx::debug1!(
            "pg_accel: gpu_agg rejected: grouped agg rows {} < min {}",
            rows,
            cost::device_limits().gpu_hash_agg_min_rows
        );
        return;
    }

    // Gate: Reject GpuAgg wrapping pathologically small multi-join outputs.
    // When the input is a join path with too few rows to amortize GPU
    // kernel launch, PG's native parallel agg wins. The original gate
    // rejected at `gpu_join_max_output_rows` (~100K) but that is the
    // *output emission* threshold — GpuAgg emits O(groups) rows, not
    // O(input) — so the relevant metric is the pre-agg input size.
    // Use `gpu_reduce_min_rows` (or `gpu_hash_agg_min_rows` for grouped)
    // as already applied above. This gate only rejects the degenerate
    // case where the join path has essentially no rows but passed the
    // earlier threshold through cardinality estimation noise.
    //
    // Relaxing this is what enables SSBM Q2/Q3/Q4 to dispatch: those
    // queries build a multi-way star join whose intermediate result is
    // in the tens of thousands of rows, still above the reduce/hashagg
    // minimum but below the old 100K join gate.
    {
        let cheapest_input = unsafe { find_cheapest_path(input_ref.pathlist) };
        if !cheapest_input.is_null() {
            // SAFETY: cheapest_input is a valid Path from the input pathlist.
            let input_tag = unsafe { (*cheapest_input.cast::<pg_sys::Node>()).type_ };
            let is_join = matches!(
                input_tag,
                NodeTag::T_HashPath | NodeTag::T_NestPath | NodeTag::T_MergePath
            );
            // Minimum input rows for a join-backed agg: use the appropriate
            // per-kernel-class threshold. Grouped agg has higher break-even
            // than plain reduce.
            let min_join_input = if group_key_info.is_some() {
                cost::device_limits().gpu_hash_agg_min_rows
            } else {
                cost::device_limits().gpu_reduce_min_rows
            };
            if is_join && rows < min_join_input {
                pgrx::debug1!(
                    "pg_accel: gpu_agg rejected: multi-join input with {} rows < min {}",
                    rows,
                    min_join_input,
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
    //
    // In non-partial mode we use `query.targetList` (AGGSPLIT_SIMPLE Aggrefs).
    // In partial mode the partial_rel's reltarget contains AGGSPLIT_INITIAL_SERIAL
    // Aggrefs (plus any group-key Vars) — a List of Expr, not List of TargetEntry.
    let partial_exprs: *mut pg_sys::List = if is_partial {
        let out_ref = unsafe { &*output_rel };
        if out_ref.reltarget.is_null() {
            return;
        }
        unsafe { (*out_ref.reltarget).exprs }
    } else {
        std::ptr::null_mut()
    };
    let tlist = if is_partial {
        partial_exprs
    } else {
        query.targetList
    };
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
    // fp64 classification for the soft-fp64 cost multiplier: set to true if
    // any aggregate in the tlist uses a float8 transition state or operates
    // on a float8 input column. AVG/STDDEV/VAR all use f64 accumulators,
    // so they count even when the result isn't FLOAT8OID.
    let mut agg_uses_fp64 = false;

    // SAFETY: tlist is a valid List of TargetEntry (non-partial) or Expr (partial).
    for i in 0..tlist_len {
        // SAFETY: i is in [0, tlist_len).
        let expr: *mut pg_sys::Expr = if is_partial {
            unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::Expr>() }
        } else {
            let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
            if tle.is_null() {
                return;
            }
            unsafe { (*tle).expr }
        };
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

        // Partial-agg gate (Phase 2): only support aggregates whose transition
        // state is a plain data type (SUM(int2/int4/float4/float8), MIN, MAX,
        // COUNT). When aggtranstype == INTERNAL (AVG, STDDEV, VARIANCE,
        // SUM(int8), SUM(numeric)) the partial output must be a serialized
        // bytea via aggserialfn — handled in Phase 4.
        if is_partial && aggref_ref.aggtranstype == pg_sys::INTERNALOID {
            pgrx::debug1!(
                "pg_accel: gpu_agg partial skipped: aggtranstype=INTERNAL (Phase 4 scope)"
            );
            return;
        }

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

        // Phase 2 partial gate (belt-and-suspenders): AVG is computed via
        // Sum/Count in our executor, but partial-mode AVG requires emitting
        // a bytea serialized state (different tuple layout). Defer to Phase 4.
        if is_partial && matches!(op, AggOp::Avg) {
            pgrx::debug1!("pg_accel: gpu_agg partial skipped: AVG requires serialize (Phase 4)");
            return;
        }

        // AVG is computed as Sum / Count. The fused scan path uses CPU-side
        // f64 Kahan accumulation, so fp64 GPU support is not required.

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

        // Extract attno — unwrap casts (RelabelType, FuncExpr) to find the
        // inner Var.  For true expression arguments (OpExpr, etc.) attno = 0
        // signals the executor to use GPU expression evaluation.
        let attno = unsafe { extract_var_attno(arg_expr) };
        let typid = if attno != 0 {
            // Plain Var (possibly wrapped in a cast): use the base column type.
            let arg_tag = unsafe { (*arg_expr.cast::<pg_sys::Node>()).type_ };
            if arg_tag == NodeTag::T_Var {
                u32::from(unsafe { (*arg_expr.cast::<pg_sys::Var>()).vartype })
            } else {
                // Cast expression — use the aggregate result type as a proxy.
                u32::from(aggref_ref.aggtype)
            }
        } else {
            // Expression argument (OpExpr, etc.) — must be numeric.
            let result_type = u32::from(aggref_ref.aggtype);
            let is_numeric_result = matches!(
                result_type,
                AGG_FLOAT4OID | AGG_FLOAT8OID | AGG_INT4OID | AGG_INT8OID
            );
            if !is_numeric_result {
                return;
            }
            AGG_FLOAT8OID
        };

        if attno != 0
            && !matches!(
                typid,
                AGG_FLOAT4OID | AGG_FLOAT8OID | AGG_INT4OID | AGG_INT8OID
            )
        {
            return;
        }
        // fp64 classification: this aggregate uses fp64 machinery if either
        //   (a) its transition state is FLOAT8OID (SUM(float8), SUM(float4),
        //       AVG/STDDEV/VAR on any numeric type — all of which store f64
        //       accumulators);
        //   (b) its input Var / expression produces FLOAT8OID; or
        //   (c) the aggregate's result type is FLOAT8OID (float8 AVG, etc.).
        // Any one triggers the soft-fp64 multiplier in the cost equation.
        let this_agg_f64 = u32::from(aggref_ref.aggtranstype) == AGG_FLOAT8OID
            || typid == AGG_FLOAT8OID
            || u32::from(aggref_ref.aggtype) == AGG_FLOAT8OID
            || matches!(op, AggOp::Avg);
        if this_agg_f64 {
            agg_uses_fp64 = true;
        }
        agg_descs.push((op, attno, u32::from(aggref_ref.aggtype)));
    }

    if agg_descs.is_empty() {
        return;
    }

    // Find cheapest input path. In partial mode we pick from partial_pathlist
    // so our CustomScan wraps a path that PG considers parallel-safe (e.g.
    // Parallel Seq Scan), matching the shape PG's own Partial Aggregate uses.
    let cheapest = if is_partial {
        unsafe { find_cheapest_path(input_ref.partial_pathlist) }
    } else {
        unsafe { find_cheapest_path(input_ref.pathlist) }
    };
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
    let child_is_our_scan =
        if unsafe { (*cheapest.cast::<pg_sys::Node>()).type_ } == NodeTag::T_CustomPath {
            let cp = cheapest.cast::<CustomPath>();
            // SAFETY: cp is a valid CustomPath (tag checked above).
            let is_ours = unsafe { (*cp).methods == custom_scan::scan_path_methods() };
            is_ours && rows >= cost::device_limits().gpu_pipeline_fusion_min_rows
        } else {
            false
        };

    // Detect self-scan opportunity: child is a plain path on a base
    // relation (SeqScan) with NO restriction quals. The executor will
    // open its own heap scan and use the vectorized pipeline instead of
    // pulling through ExecProcNode. When the relation has quals (WHERE
    // clause), we must let PG's SeqScan apply them — our fused scan
    // doesn't evaluate quals.
    let has_quals = !input_ref.baserestrictinfo.is_null()
        && unsafe { pg_sys::list_length(input_ref.baserestrictinfo) } > 0;
    // Self-scan requires: (1) base relation, (2) cheapest path is a plain
    // Path (T_Path = SeqScan), not a Sort/WindowAgg/etc. This prevents
    // self-scanning when the input needs preprocessing (e.g., window funcs).
    let is_baserel =
        input_ref.reloptkind == pg_sys::RelOptKind::RELOPT_BASEREL && input_ref.relid > 0;
    // Gate: the RTE must be a real heap relation. Function scans
    // (generate_series), VALUES lists, CTEs, and subqueries have
    // reloptkind == RELOPT_BASEREL but rtekind != RTE_RELATION.
    // ExecOpenScanRelation on a non-relation RTE raises an ERROR.
    let rte_is_relation = is_baserel
        && unsafe {
            let rte = *(*root).simple_rte_array.offset(input_ref.relid as isize);
            !rte.is_null()
                && (*rte).rtekind == pg_sys::RTEKind::RTE_RELATION
                && (*rte).relid != pg_sys::InvalidOid
        };
    let child_is_plain_path =
        unsafe { (*cheapest.cast::<pg_sys::Node>()).type_ == NodeTag::T_Path };
    let self_scan_relid: pg_sys::Index = if !child_is_our_scan
        && is_baserel
        && rte_is_relation
        && child_is_plain_path
        && rows >= cost::device_limits().gpu_reduce_min_rows
        && !has_quals
    {
        input_ref.relid
    } else {
        0
    };

    let is_vectorized = child_is_our_scan || self_scan_relid > 0;

    let gpu_overhead = cost::GPU_LAUNCH_OVERHEAD;
    // Per-row cost: vectorized paths (self-scan or fused GpuExpr child)
    // do direct heap walk + columnar extract, eliminating ExecProcNode
    // and MinimalTuple overhead. Same architecture as hash join.
    let agg_per_row_base = if is_vectorized { 0.001 } else { 0.005 };
    // Apply the soft-fp64 cost multiplier when any aggregate in the tlist
    // uses fp64 accumulation and the device lacks native fp64 (Apple
    // Silicon / Metal without hardware fp64). The emulated fp64 arithmetic
    // runs ~1/32 native fp32 throughput, so the per-row GPU cost scales up
    // accordingly. Applied to the reduce (agg) component; launch overhead
    // is unchanged. Both plain `GpuReduce` and `GpuHashAgg` paths go
    // through this site — one fix covers both.
    let agg_per_row =
        cost::apply_fp64_penalty(agg_per_row_base, agg_uses_fp64, cost::device_limits());
    // For grouped agg, add hash table build + probe + per-group
    // accumulation cost per row. GPU batched hash reduction is cheap
    // (bitonic partition + segmented reduce), so this is low. The hash
    // overhead itself is integer work (probe + compare), so it is NOT
    // subject to the fp64 penalty — only the value accumulation is.
    let hash_overhead = if group_key_info.is_some() { 0.002 } else { 0.0 };
    let reduce_cost = base.rows * (agg_per_row + hash_overhead);
    // For self-scanning paths, compute our own scan cost instead of
    // inheriting the child's SeqScan cost. Our heap walk + arena copy
    // is cheaper than PG's full tuple deformation pipeline.
    let scan_cost = if self_scan_relid > 0 {
        // heap_getnext + memcpy per row, no slot deformation
        base.rows * 0.003
    } else {
        base.total_cost
    };
    let startup_cost = scan_cost + gpu_overhead;
    // Apply GPU_COST_SAFETY_MARGIN so our batched GPU agg undercuts PG's
    // parallel plan cost. The margin reflects real-world GPU speedup vs
    // PG's optimistic parallel cost model (which assumes perfect scaling).
    let total_cost = (scan_cost + reduce_cost).mul_add(cost::GPU_COST_SAFETY_MARGIN, gpu_overhead)
        * gucs::cost_multiplier();

    // Gate: Only inject if GpuAgg is meaningfully cheaper than the existing
    // best path at this rel. In partial mode we compare against the cheapest
    // PARTIAL path (PG's own partial agg inside a worker); in non-parallel
    // mode we compare against the cheapest non-parallel pathlist entry.
    let output_ref = unsafe { &*output_rel };
    let pg_best_cost = if is_partial {
        unsafe { find_cheapest_total_cost(output_ref.partial_pathlist) }
    } else {
        unsafe { find_cheapest_nonparallel_total_cost(output_ref.pathlist) }
    };
    let ratio = cost::device_limits().gpu_agg_cost_ratio;
    if pg_best_cost > 0.0 && total_cost >= pg_best_cost * ratio {
        pgrx::debug1!(
            "pg_accel: gpu_agg{} rejected: cost {:.1} >= {:.0}% of PG best {:.1}",
            if is_partial { " partial" } else { "" },
            total_cost,
            ratio * 100.0,
            pg_best_cost,
        );
        return;
    }

    // SAFETY: Allocating via palloc, building valid CustomPath.
    unsafe {
        let cpath = pg_sys::palloc0(std::mem::size_of::<CustomPath>()).cast::<CustomPath>();

        // custom_paths[0] holds a direct pointer to the child (no copy). PG's
        // `create_customscan_plan` just iterates this list and recurses into
        // `create_plan_recurse`; it does not take ownership. Earlier iterations
        // palloc'd a shallow copy — but truncating path-subtype storage when
        // the child is a subclass (AggPath/GatherPath/…) with fields past
        // `sizeof(Path)` produced Paths whose trailing bytes were uninitialised,
        // which later surfaced as "unrecognized node type: 0" during
        // `create_plan_recurse`.
        let child_copy: *mut Path = cheapest;

        (*cpath).path.type_ = NodeTag::T_CustomPath;
        (*cpath).path.pathtype = NodeTag::T_CustomScan;
        (*cpath).path.parent = output_rel;
        // For the partial-agg path, deep-copy the pathtarget so our
        // CustomPath owns an independent Aggref tree. Aliasing
        // `output_rel->reltarget` means PG's later
        // `make_partial_grouping_target` / `prepare_sort_from_pathkeys`
        // mutations of the shared Aggref sub-tree can leave dangling or
        // zero-tagged nodes, which surface as "unrecognized node type: 0"
        // in `create_plan_recurse`. The non-partial branch below keeps the
        // direct reference because its tlist is never rewritten post-path.
        (*cpath).path.pathtarget = if is_partial {
            pg_sys::copy_pathtarget((*output_rel).reltarget)
        } else {
            (*output_rel).reltarget
        };
        (*cpath).path.param_info = std::ptr::null_mut();
        // Partial path must declare parallel_safe=true so PG can place it
        // inside a Gather; we inherit parallel_workers from the chosen
        // partial child path. In non-parallel mode we propagate the child's
        // own flag as before.
        (*cpath).path.parallel_aware = false;
        (*cpath).path.parallel_safe = if is_partial { true } else { base.parallel_safe };
        (*cpath).path.parallel_workers = if is_partial {
            base.parallel_workers.max(1)
        } else {
            0
        };
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
        // Append self-scan relid (0 = no self-scan, >0 = open this relation).
        #[allow(clippy::cast_possible_wrap)]
        {
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(self_scan_relid as std::ffi::c_int).cast(),
            );
        }
        // Append is_partial flag. Must be the new tail element — readers in
        // plan_custom_path_agg and deserialize_custom_private take it from
        // list_len - 1 and the self_scan_relid from list_len - 2.
        priv_list = lappend(priv_list, pg_sys::makeInteger(i32::from(is_partial)).cast());

        (*cpath).custom_private = priv_list;

        if is_partial {
            pg_sys::add_partial_path(output_rel, cpath.cast());
        } else {
            add_path(output_rel, cpath.cast());
        }

        pgrx::debug1!(
            "pg_accel: injected GpuReduce{} path for {} agg(s), rows={}, self_scan_relid={}",
            if is_partial { " partial" } else { "" },
            agg_descs.len(),
            rows,
            self_scan_relid,
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
pub(super) unsafe fn find_cheapest_path(pathlist: *mut List) -> *mut Path {
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
unsafe fn find_cheapest_path_filtered(pathlist: *mut List, skip_parallel: bool) -> *mut Path {
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

/// Find the total_cost of the cheapest non-parallel path in a pathlist.
/// Skips Gather/GatherMerge so the cost reflects serial execution.
///
/// This is the baseline to compare our single-threaded Custom Scan
/// against. Comparing against parallel paths systematically underestimates
/// PG (it divides work across N workers on paper) and prevents our path
/// from being injected even when the GPU kernel is genuinely faster than
/// serial PG on real hardware.
///
/// Returns 0.0 if the list is empty or contains only parallel paths.
///
/// # Safety
///
/// `pathlist` must be a valid PG `List` of `Path*` or null.
unsafe fn find_cheapest_nonparallel_total_cost(pathlist: *mut List) -> f64 {
    // SAFETY: delegates to the non-parallel cheapest path finder.
    let path = unsafe { find_cheapest_nonparallel_path(pathlist) };
    if path.is_null() {
        // No non-parallel path (everything is Gather). Fall back to the
        // absolute cheapest so the gate still has a reference cost.
        // SAFETY: pathlist is a valid List pointer.
        unsafe { find_cheapest_total_cost(pathlist) }
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
pub(super) unsafe fn find_cheapest_seqscan_path(pathlist: *mut List) -> *mut Path {
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
pub(super) unsafe fn find_cheapest_seqscan_cost(pathlist: *mut List) -> f64 {
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

/// Find the cheapest `T_BitmapHeapPath` in the pathlist.
///
/// Used by `set_rel_pathlist_hook` (TODO.md Phase 4 "BitmapHeapScan
/// injection") to wrap a bitmap-driven path as the child of a GPU
/// CustomPath when PG has pruned the seq scan in favour of a bitmap
/// path. Selective WHERE predicates routinely produce this shape (an
/// index pre-filter + heap recheck), and without this branch the
/// CustomPath wrapper never sees them.
///
/// Returns null if no `BitmapHeapPath` exists in the pathlist.
///
/// # Safety
///
/// `pathlist` must be a valid `List` pointer from the planner, or null.
pub(super) unsafe fn find_cheapest_bitmap_heap_path(pathlist: *mut List) -> *mut Path {
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
        if tag == NodeTag::T_BitmapHeapPath {
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
pub(super) unsafe fn has_cheap_spatial_index_path(pathlist: *mut List, seq_scan_cost: f64) -> bool {
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
pub(super) unsafe fn has_cheaper_spatial_index_path(pathlist: *mut List, custom_cost: f64) -> bool {
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
#[allow(dead_code)]
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
pub(super) struct AccelMatch {
    pub(super) strategy: registry::AccelStrategy,
    pub(super) fn_oid: pg_sys::Oid,
    /// 1-based attribute number of the Var argument, or 0 if none found.
    pub(super) target_attno: i32,
}

/// Extract the vertex count of the constant geometry argument in a spatial
/// FuncExpr clause.  Returns `Some(count)` when a Const argument with a
/// valid GSERIALIZED datum is found; `None` otherwise (e.g. two Var args,
/// or expression not yet folded to Const).
///
/// # Safety
///
/// `restrictinfo_list` must be null or a valid PG `List` of `RestrictInfo`.
pub(super) unsafe fn extract_const_geom_vertex_count(
    restrictinfo_list: *mut List,
) -> Option<usize> {
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
pub(super) fn try_gpu_expr_match(restrictinfo_list: *mut List) -> Option<AccelMatch> {
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
            // IN-list: the planner hook only accepts clauses that the
            // bytecode builder actually supports. `compile_node` in
            // `custom_scan.rs` does NOT emit bytecode for
            // `T_ScalarArrayOpExpr` today, so accepting the clause here
            // caused GpuExpr injection to dispatch, then fall through to
            // `CompiledExpr::DeferToPg` at executor time, which runs the
            // scalar qual per tuple inside the Custom Scan framing. At
            // 10M rows that pays the framing cost per row with zero GPU
            // benefit (observed as `expr_multi_or @ 10M = 0.16x` — a 6x
            // regression).
            //
            // Reject here so PG evaluates the IN-list natively with
            // SeqScan + parallel workers (≈ 1.00x). Once `compile_node`
            // emits an OR chain (or a dedicated IN kernel) for
            // `ScalarArrayOpExpr`, this check should be restored to the
            // element-type guard.
            //
            // NOTE: the old element-type check is preserved for reference:
            //   let saop = node.cast::<pg_sys::ScalarArrayOpExpr>();
            //   let args = (*saop).args;
            //   if args.is_null() { return false; }
            //   let scalar = list_nth(args, 0).cast::<Node>();
            //   is_gpu_compilable_expr_node(scalar)
            let _ = node;
            false
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
pub(super) fn find_accelerable_match(restrictinfo_list: *mut List) -> Option<AccelMatch> {
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
#[allow(dead_code)]
fn find_accelerable_strategy(restrictinfo_list: *mut List) -> Option<registry::AccelStrategy> {
    find_accelerable_match(restrictinfo_list).map(|m| m.strategy)
}

/// Result of detecting an equi-join condition (e.g., `a.col = b.col`).
pub(super) struct EquiJoinKey {
    /// 1-based attribute number of the outer relation's join key.
    pub(super) outer_attno: i32,
    /// 1-based attribute number of the inner relation's join key.
    pub(super) inner_attno: i32,
    /// Range table index (varno) of the outer join key variable.
    pub(super) outer_varno: i32,
    /// Range table index (varno) of the inner join key variable.
    pub(super) inner_varno: i32,
    /// Key type: 0=int32, 1=int64, 2=float64.
    pub(super) key_type: i32,
}

/// Scan a `RestrictInfo` list for an equi-join condition (`Var = Var`) where
/// the two `Var` nodes reference different relations.
///
/// Returns the outer/inner attribute numbers and key type if found.
///
/// # Safety
///
/// `restrictinfo_list` must be null or a valid PG `List` of `RestrictInfo`.
pub(super) unsafe fn find_equi_join_key(
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

        // User-facing escape valve: `pg_accel.fp64_enabled=false` fully
        // bypasses GPU injection for fp64 join keys. Soft-fp64 on Metal is
        // otherwise always correct; the cost model penalises fp64 op cost
        // on devices without native fp64 (see
        // DeviceLimits::soft_fp64_cost_multiplier) so the planner naturally
        // prefers PG when the soft-fp64 penalty wipes out the GPU win.
        if key_type == 2 && !crate::fp64_enabled() {
            pgrx::debug1!("pg_accel join: float64 key skipped — fp64_enabled=false");
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
#[allow(dead_code)]
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
pub(super) unsafe fn create_custom_path(
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
mod tests;
