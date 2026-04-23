//! Parallel aggregate path injection.
//!
//! PostgreSQL 17 does NOT fire `create_upper_paths_hook` for
//! `UPPERREL_PARTIAL_GROUP_AGG`. By the time `UPPERREL_GROUP_AGG` fires, the
//! core planner has already:
//!
//!   1. Called `gather_grouping_paths(partially_grouped_rel)` — wrapping
//!      `partially_grouped_rel->partial_pathlist` entries in `GatherPath`s
//!      that are then added to `partially_grouped_rel->pathlist`.
//!   2. Called `add_paths_to_grouping_rel(grouped_rel)` — building Finalize
//!      Agg paths on `grouped_rel->pathlist` that read from the Gathers in
//!      `partially_grouped_rel->pathlist`.
//!
//! So a partial CustomPath added to `partially_grouped_rel->partial_pathlist`
//! at this stage is orphaned: nothing wraps it in a Gather, nothing wraps it
//! in a Finalize Agg, and any reader that treats `cheapest_total_path` of the
//! partial rel as live sees a truncated/uninitialised path. That manifests as
//! `ERROR: unrecognized node type: 0`.
//!
//! This module instead builds the full chain itself:
//!
//! ```text
//! Finalize Agg (AGGSPLIT_FINAL_DESERIAL)
//!   └── Gather
//!        └── GpuAccel partial CustomPath
//!              └── cheapest partial path from input_rel (e.g. Parallel Seq Scan)
//! ```
//!
//! and adds the Finalize AggPath directly to `grouped_rel->pathlist` via
//! `add_path`. Plain aggregation only in the first iteration — GROUP BY
//! queries bail.

use pgrx::pg_sys::{self, CustomPath, List, NodeTag, Path, lappend};

use crate::engine::cost;
use crate::engine::executor::agg::AggOp;
use crate::engine::executor::agg::partial::{PartialAggSpec, PartialColumn};
use crate::engine::gucs;

use super::agg_common::{self, AggClass};
use super::custom_scan;

/// Try to inject the parallel (partial → gather → finalize) GpuAgg path chain
/// directly onto `grouped_rel->pathlist`.
///
/// Bails silently when any pre-condition isn't met (GROUP BY present, no
/// partial paths on input, unclassified Aggref, GPU unavailable, etc.).
///
/// # Safety
///
/// Called from the planner hook on the main backend thread. All pointer args
/// must be valid planner-provided arguments.
#[allow(
    clippy::too_many_lines,
    clippy::cast_ptr_alignment,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub(super) unsafe fn try_inject(
    root: *mut pg_sys::PlannerInfo,
    input_rel: *mut pg_sys::RelOptInfo,
    grouped_rel: *mut pg_sys::RelOptInfo,
) {
    // Gate: GPU must be available and enabled.
    if !cost::gpu_is_usable() {
        pgrx::debug1!("pg_accel partial_agg: gpu not usable");
        return;
    }

    // SAFETY: root and grouped_rel are valid planner-provided pointers.
    let root_ref = unsafe { &*root };
    let grouped_ref = unsafe { &*grouped_rel };

    // First-iteration scope: plain aggregates only. Grouped parallel agg
    // needs extra wiring (groupClause propagation + distinct_rel handling)
    // and will land in a follow-up.
    let parse = root_ref.parse;
    if parse.is_null() {
        return;
    }
    // SAFETY: parse is a valid Query pointer.
    let query = unsafe { &*parse };
    if !query.groupClause.is_null() {
        pgrx::debug1!("pg_accel partial_agg: GROUP BY present — first iteration is plain-agg only");
        return;
    }

    if !grouped_ref.consider_parallel {
        pgrx::debug1!("pg_accel partial_agg: grouped_rel.consider_parallel=false");
        return;
    }

    // Fetch the partial grouping rel (PG 17 doesn't fire the hook at
    // UPPERREL_PARTIAL_GROUP_AGG so we have to grab it by relids).
    // SAFETY: fetch_upper_rel is a standard planner helper.
    let partially_grouped_rel = unsafe {
        pg_sys::fetch_upper_rel(
            root,
            pg_sys::UpperRelationKind::UPPERREL_PARTIAL_GROUP_AGG,
            grouped_ref.relids,
        )
    };
    if partially_grouped_rel.is_null() {
        pgrx::debug1!("pg_accel partial_agg: partially_grouped_rel is NULL");
        return;
    }
    // SAFETY: non-null after check above.
    let partial_ref = unsafe { &*partially_grouped_rel };
    if !partial_ref.consider_parallel {
        pgrx::debug1!("pg_accel partial_agg: partially_grouped_rel.consider_parallel=false");
        return;
    }

    // Need a partial child path to wrap.
    // SAFETY: input_rel is a valid RelOptInfo.
    let input_ref = unsafe { &*input_rel };
    if input_ref.partial_pathlist.is_null()
        || unsafe { pg_sys::list_length(input_ref.partial_pathlist) } == 0
    {
        pgrx::debug1!("pg_accel partial_agg: input_rel has no partial paths");
        return;
    }

    // Row-count threshold.
    let rows = input_ref.rows as usize;
    if rows < cost::device_limits().gpu_reduce_min_rows {
        pgrx::debug1!(
            "pg_accel partial_agg: rows {} < min {}",
            rows,
            cost::device_limits().gpu_reduce_min_rows
        );
        return;
    }

    // Classify aggregates from the partial rel's reltarget (AGGSPLIT_INITIAL_SERIAL
    // Aggrefs). Collect (AggOp, attno, result_type_oid) for each.
    if partial_ref.reltarget.is_null() {
        return;
    }
    // SAFETY: reltarget is non-null.
    let partial_exprs = unsafe { (*partial_ref.reltarget).exprs };
    if partial_exprs.is_null() {
        return;
    }
    let n_exprs = unsafe { pg_sys::list_length(partial_exprs) };
    if n_exprs == 0 {
        return;
    }

    let mut agg_descs: Vec<(AggOp, i32, u32)> = Vec::with_capacity(n_exprs as usize);
    let mut partial_cols: Vec<PartialColumn> = Vec::with_capacity(n_exprs as usize);
    for i in 0..n_exprs {
        // SAFETY: i is in [0, n_exprs).
        let expr = unsafe { pg_sys::list_nth(partial_exprs, i).cast::<pg_sys::Expr>() };
        if expr.is_null() {
            return;
        }
        // SAFETY: reading node tag.
        let tag = unsafe { (*expr.cast::<pg_sys::Node>()).type_ };
        if tag != NodeTag::T_Aggref {
            pgrx::debug1!("pg_accel partial_agg: partial tlist has non-Aggref node");
            return;
        }
        let aggref = expr.cast::<pg_sys::Aggref>();
        // SAFETY: classify_aggref accepts a valid Aggref pointer.
        let Some((op, class)) = (unsafe { agg_common::classify_aggref(aggref) }) else {
            pgrx::debug1!("pg_accel partial_agg: unrecognized Aggref");
            return;
        };
        // SAFETY: aggref is valid.
        let aggref_ref = unsafe { &*aggref };

        // INTERNAL-state aggregates can't ride the Float8StatsEmitter path:
        // `numeric_accum` / `int8_accum` carry an opaque `NumericAggState*`
        // whose aggserialfn expects that exact struct shape, NOT our generic
        // [N, Sx, Sxx] float8[3]. Passing the wrong bytes is UB.
        //
        // For Phase A, only float4/float8 stats (transtype = `_float8`) are
        // supported. Integer / numeric stats fall back to PG's native plan.
        if aggref_ref.aggtranstype == pg_sys::INTERNALOID {
            pgrx::debug1!(
                "pg_accel partial_agg: INTERNAL transtype (numeric-family) — bail, \
                 not yet supported in partial path"
            );
            return;
        }
        // `class` is still only used for the `_` binding above; keep a no-op
        // to silence unused-variable linting in case the enum grows.
        let _ = &class;

        // Extract attno from first arg (or 0 for COUNT(*)).
        let (attno, rtype) = if op == AggOp::Count && aggref_ref.aggstar {
            (0i32, u32::from(aggref_ref.aggtype))
        } else {
            let args = aggref_ref.args;
            if args.is_null() || unsafe { pg_sys::list_length(args) } < 1 {
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
            // SAFETY: arg_expr is a valid Expr pointer.
            let extracted = unsafe { super::extract_var_attno(arg_expr) };
            (extracted, u32::from(aggref_ref.aggtype))
        };

        let serialize_fn_oid = match class {
            AggClass::Float8Stats { serialize_fn } if serialize_fn != pg_sys::InvalidOid => {
                Some(serialize_fn)
            }
            _ => None,
        };

        agg_descs.push((op, attno, rtype));
        partial_cols.push(PartialColumn {
            op,
            attno,
            transtype_oid: aggref_ref.aggtranstype,
            serialize_fn_oid,
        });
    }

    if agg_descs.is_empty() {
        return;
    }

    // Pick the cheapest partial child path to wrap.
    // SAFETY: partial_pathlist is non-empty.
    let cheapest_partial =
        unsafe { pg_sys::list_nth(input_ref.partial_pathlist, 0).cast::<Path>() };
    if cheapest_partial.is_null() {
        return;
    }
    // SAFETY: cheapest_partial is non-null.
    let base = unsafe { &*cheapest_partial };

    // Cost: mirror the non-partial formula but use the partial base's costs.
    let gpu_overhead = cost::GPU_LAUNCH_OVERHEAD;
    // Soft-fp64 classification: any partial column using a float8 transition
    // state (SUM(float8), SUM(float4) (promotes to float8), AVG/STDDEV/VAR
    // which carry the accumulator in f64) triggers the soft-fp64 multiplier
    // when the device lacks native fp64. Mirrors the classification in the
    // non-parallel body (mod.rs::pgaccel_inject_gpu_agg); both sites must
    // agree or the planner will prefer the wrong split.
    let float8_u32 = u32::from(pg_sys::FLOAT8OID);
    let agg_uses_fp64 = partial_cols
        .iter()
        .any(|c| u32::from(c.transtype_oid) == float8_u32)
        || agg_descs
            .iter()
            .any(|(op, _, rtype)| matches!(op, AggOp::Avg) || *rtype == float8_u32);
    let agg_per_row_base = 0.005_f64;
    let agg_per_row =
        cost::apply_fp64_penalty(agg_per_row_base, agg_uses_fp64, cost::device_limits());
    let reduce_cost = base.rows * agg_per_row;
    let startup_cost = base.total_cost + gpu_overhead;
    let total_cost = (base.total_cost + reduce_cost)
        .mul_add(cost::GPU_COST_SAFETY_MARGIN, gpu_overhead)
        * gucs::cost_multiplier();

    // Build the partial CustomPath.
    // SAFETY: Allocating via palloc, building valid CustomPath.
    let cpath = unsafe {
        let cpath = pg_sys::palloc0(std::mem::size_of::<CustomPath>()).cast::<CustomPath>();

        (*cpath).path.type_ = NodeTag::T_CustomPath;
        (*cpath).path.pathtype = NodeTag::T_CustomScan;
        (*cpath).path.parent = partially_grouped_rel;
        // Deep-copy pathtarget so later planner mutations don't alias into
        // shared Aggref sub-trees (produced "unrecognized node type: 0" before).
        (*cpath).path.pathtarget = pg_sys::copy_pathtarget(partial_ref.reltarget);
        (*cpath).path.param_info = std::ptr::null_mut();
        (*cpath).path.parallel_aware = false;
        (*cpath).path.parallel_safe = true;
        (*cpath).path.parallel_workers = base.parallel_workers.max(1);
        // Plain partial agg emits 1 row per worker — final rows=parallel_workers;
        // but PG's gather_grouping_paths convention uses partially_grouped_rel.rows.
        (*cpath).path.rows = partial_ref.rows.max(1.0);
        (*cpath).path.startup_cost = startup_cost;
        (*cpath).path.total_cost = total_cost;
        (*cpath).path.pathkeys = std::ptr::null_mut();

        (*cpath).flags = 0;
        (*cpath).custom_paths = lappend(std::ptr::null_mut(), cheapest_partial.cast());
        (*cpath).custom_restrictinfo = std::ptr::null_mut();
        (*cpath).methods = custom_scan::agg_path_methods();

        // Serialize the custom_private layout expected by plan_custom_path_agg:
        //   [num_aggs, op0, attno0, rtype0, ..., has_group_key=0,
        //    self_scan_relid=0, is_partial=1]
        let mut priv_list: *mut List = std::ptr::null_mut();
        #[allow(clippy::cast_possible_truncation)]
        let num_aggs = agg_descs.len() as i32;
        priv_list = lappend(priv_list, pg_sys::makeInteger(num_aggs).cast());
        for &(op, attno, rtype) in &agg_descs {
            priv_list = lappend(priv_list, pg_sys::makeInteger(op.to_i32()).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(attno).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(rtype as i32).cast());
        }
        // No group key (first iteration).
        priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast());
        // No self-scan (child is PG's parallel seq scan).
        priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast());
        // is_partial = 1.
        priv_list = lappend(priv_list, pg_sys::makeInteger(1).cast());

        // Append full PartialAggSpec (transtype + serialize_fn per column) so
        // plan_custom_path_agg can materialize Float8StatsEmitter for
        // AVG/STDDEV/VAR without re-walking Aggrefs.
        let spec = PartialAggSpec {
            per_column: partial_cols.clone(),
        };
        priv_list = custom_scan::append_partial_spec(priv_list, &spec);

        (*cpath).custom_private = priv_list;
        cpath
    };

    // Wrap the partial CustomPath in a Gather path rooted at
    // partially_grouped_rel. PG's create_gather_path accepts a mut rows pointer
    // that it fills with the cumulative gathered row count.
    // SAFETY: All pointers validated above; create_gather_path is a standard
    // planner helper callable on the main backend thread.
    let gather = unsafe {
        let mut rows_out: f64 = (*cpath).path.rows;
        pg_sys::create_gather_path(
            root,
            partially_grouped_rel,
            cpath.cast::<Path>(),
            partial_ref.reltarget,
            std::ptr::null_mut(),
            &raw mut rows_out,
        )
    };
    if gather.is_null() {
        return;
    }

    // Wrap the Gather in a Finalize Agg path on grouped_rel. For plain agg
    // (no GROUP BY), numGroups = 1 and groupClause = NULL.
    //
    // AggClauseCosts with zero-init gives transCost.{startup,per_tuple}=0
    // and finalCost.{startup,per_tuple}=0; that's acceptable for cost-model
    // purposes — create_agg_path still costs the Gather + Agg node itself.
    let agg_final_costs = pg_sys::AggClauseCosts::default();
    // SAFETY: create_agg_path is a standard planner helper; grouped_rel and
    // gather path are valid; numGroups=1 for plain aggregate.
    let final_agg = unsafe {
        pg_sys::create_agg_path(
            root,
            grouped_rel,
            gather.cast::<Path>(),
            grouped_ref.reltarget,
            pg_sys::AggStrategy::AGG_PLAIN,
            pg_sys::AggSplit::AGGSPLIT_FINAL_DESERIAL,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw const agg_final_costs,
            1.0_f64,
        )
    };
    if final_agg.is_null() {
        return;
    }

    // SAFETY: add_path accepts any Path subclass pointer on grouped_rel.
    unsafe {
        pg_sys::add_path(grouped_rel, final_agg.cast::<Path>());
    }

    // SAFETY: cpath is a live CustomPath pointer we just allocated.
    let (cpath_workers, cpath_rows) =
        unsafe { ((*cpath).path.parallel_workers, (*cpath).path.rows) };
    pgrx::debug1!(
        "pg_accel partial_agg: injected Finalize(Agg) -> Gather -> GpuAccel(partial) chain \
         (n_aggs={}, workers={}, rows={})",
        agg_descs.len(),
        cpath_workers,
        cpath_rows,
    );
}
