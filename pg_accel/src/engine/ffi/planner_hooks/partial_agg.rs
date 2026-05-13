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
//! `add_path`.

use pgrx::pg_sys::{self, CustomPath, List, NodeTag, Path, lappend};

use crate::engine::cost;
use crate::engine::executor::agg::partial::{PartialAggSpec, PartialColumn};
use crate::engine::executor::agg::{AggOp, GroupKeyInfo};
use crate::engine::gucs;

use super::agg_common::{self, AggClass};
use super::custom_scan;

/// Try to inject the parallel (partial → gather → finalize) GpuAgg path chain
/// directly onto `grouped_rel->pathlist`.
///
/// Bails silently when any pre-condition isn't met (unsupported GROUP BY,
/// no partial paths on input, unclassified Aggref, GPU unavailable, etc.).
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
#[allow(dead_code)]
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

    let parse = root_ref.parse;
    if parse.is_null() {
        return;
    }
    // SAFETY: parse is a valid Query pointer.
    let query = unsafe { &*parse };

    let group_len = if query.groupClause.is_null() {
        0
    } else {
        // SAFETY: groupClause is a valid planner-owned List.
        unsafe { pg_sys::list_length(query.groupClause) }
    };
    if group_len > 1 {
        pgrx::debug1!(
            "pg_accel partial_agg: GROUP BY has {} keys; grouped partial path supports one key",
            group_len
        );
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

    let group_key_info: Option<GroupKeyInfo> = if group_len == 1 {
        let tlist = query.targetList;
        let tlist_len = if tlist.is_null() {
            0
        } else {
            // SAFETY: targetList is a valid planner-owned List.
            unsafe { pg_sys::list_length(tlist) }
        };
        // SAFETY: groupClause has one element.
        let sc =
            unsafe { pg_sys::list_nth(query.groupClause, 0).cast::<pg_sys::SortGroupClause>() };
        if sc.is_null() {
            return;
        }
        // SAFETY: sc is a valid SortGroupClause.
        let sgref = unsafe { (*sc).tleSortGroupRef };
        let mut resolved: Option<(i32, pg_sys::Oid)> = None;
        for j in 0..tlist_len {
            // SAFETY: j is in [0, tlist_len).
            let tle = unsafe { pg_sys::list_nth(tlist, j).cast::<pg_sys::TargetEntry>() };
            if tle.is_null() || unsafe { (*tle).ressortgroupref } != sgref {
                continue;
            }
            let expr = unsafe { (*tle).expr };
            if expr.is_null() {
                return;
            }
            let tag = unsafe { (*expr.cast::<pg_sys::Node>()).type_ };
            if tag != NodeTag::T_Var {
                pgrx::debug1!("pg_accel partial_agg: GROUP BY key is not a plain Var");
                return;
            }
            let var = expr.cast::<pg_sys::Var>();
            resolved = Some((i32::from(unsafe { (*var).varattno }), unsafe {
                (*var).vartype
            }));
            break;
        }
        let Some((attno, type_oid)) = resolved else {
            pgrx::debug1!("pg_accel partial_agg: could not resolve GROUP BY key");
            return;
        };
        let Some(key_type) = GroupKeyInfo::key_type_from_oid(type_oid) else {
            pgrx::debug1!("pg_accel partial_agg: unsupported GROUP BY key type");
            return;
        };
        if !matches!(key_type, 0 | 1 | 2 | 4) {
            pgrx::debug1!(
                "pg_accel partial_agg: GROUP BY key type {} cannot be emitted by grouped agg",
                key_type
            );
            return;
        }
        Some(GroupKeyInfo {
            attno,
            type_oid,
            key_type,
        })
    } else {
        None
    };
    if group_key_info.is_some() && !cost::hashagg_input_rows_safe(rows, cost::device_limits()) {
        pgrx::debug1!(
            "pg_accel partial_agg: grouped rows {} >= unsafe hashagg threshold {}",
            rows,
            cost::device_limits().gpu_hash_agg_unsafe_input_rows,
        );
        return;
    }

    let mut agg_descs: Vec<(AggOp, i32, u32)> = Vec::with_capacity(n_exprs as usize);
    let mut partial_cols: Vec<PartialColumn> = Vec::with_capacity(n_exprs as usize);
    let mut group_key_tlist_pos: i32 = -1;
    let mut has_i64_sum = false;
    for i in 0..n_exprs {
        // SAFETY: i is in [0, n_exprs).
        let expr = unsafe { pg_sys::list_nth(partial_exprs, i).cast::<pg_sys::Expr>() };
        if expr.is_null() {
            return;
        }
        // SAFETY: reading node tag.
        let tag = unsafe { (*expr.cast::<pg_sys::Node>()).type_ };
        if tag != NodeTag::T_Aggref {
            if let Some(ref gk) = group_key_info
                && tag == NodeTag::T_Var
            {
                let var = expr.cast::<pg_sys::Var>();
                if i32::from(unsafe { (*var).varattno }) == gk.attno {
                    if group_key_tlist_pos < 0 {
                        group_key_tlist_pos = i;
                    }
                    continue;
                }
            }
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

        if !aggref_ref.aggdistinct.is_null() {
            pgrx::debug1!("pg_accel partial_agg: aggregate has DISTINCT clause");
            return;
        }
        if !aggref_ref.aggorder.is_null() {
            pgrx::debug1!("pg_accel partial_agg: aggregate has ORDER BY clause");
            return;
        }
        if !aggref_ref.aggfilter.is_null() {
            pgrx::debug1!("pg_accel partial_agg: aggregate has FILTER clause");
            return;
        }

        // Per-column transtype gate.
        //
        // INTERNAL-state aggregates (AVG/STDDEV/VAR on int8/numeric/interval)
        // can't ride the Float8StatsEmitter path: `numeric_avg_accum` /
        // `int8_avg_accum` carry an opaque `NumericAggState*` /
        // `PolyNumAggState*` whose aggserialfn expects that exact struct
        // shape, NOT our generic [N, Sx, Sxx] float8[3]. Passing the wrong
        // bytes to `numeric_avg_serialize` would be UB.
        //
        // Integer AVG (int2/int4 → transtype `_int8`) carries a two-element
        // int8[] transition state; the emitter produces a 3-element float8[]
        // with different element type, so the final agg's combine function
        // would see a type-mismatched array.
        //
        // Supported today: AVG / STDDEV(_POP|_SAMP) / VAR(_POP|_SAMP) on
        // float4/float8 — PG resolves those to `float4_accum`/`float8_accum`
        // whose transtype is `_float8` (= `FLOAT8ARRAYOID`). Matches the
        // `Float8StatsEmitter` no-serialize branch exactly.
        let float_stats_op = matches!(
            op,
            AggOp::Avg | AggOp::StddevSamp | AggOp::StddevPop | AggOp::VarSamp | AggOp::VarPop
        );
        if float_stats_op && aggref_ref.aggtranstype != pg_sys::FLOAT8ARRAYOID {
            pgrx::debug1!(
                "pg_accel partial_agg: AVG/STDDEV/VAR with transtype={} not `_float8` \
                 (INTERNAL / int8[] / numeric-family) — bail, not yet supported",
                u32::from(aggref_ref.aggtranstype),
            );
            return;
        }

        // Extract attno from first arg (or 0 for COUNT(*)).
        let mut arg_type_oid = pg_sys::InvalidOid;
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
            if unsafe { (*arg_expr.cast::<pg_sys::Node>()).type_ } == NodeTag::T_Var {
                arg_type_oid = unsafe { (*arg_expr.cast::<pg_sys::Var>()).vartype };
            }
            // SAFETY: arg_expr is a valid Expr pointer.
            let extracted = unsafe { super::extract_var_attno(arg_expr) };
            (extracted, u32::from(aggref_ref.aggtype))
        };
        if matches!(op, AggOp::Sum) && arg_type_oid == pg_sys::INT8OID {
            has_i64_sum = true;
        }

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
    if has_i64_sum {
        pgrx::debug1!(
            "pg_accel partial_agg: SUM(bigint) rejected until parallel i64 reduce is stable"
        );
        return;
    }
    if group_key_info.is_some() && agg_descs.iter().any(|(op, _, _)| matches!(op, AggOp::Avg)) {
        pgrx::debug1!(
            "pg_accel partial_agg: grouped AVG rejected until grouped AVG executor mapping is fixed",
        );
        return;
    }
    if group_key_info.is_some() && group_key_tlist_pos < 0 {
        pgrx::debug1!("pg_accel partial_agg: grouped partial target lacks group key Var");
        return;
    }

    // Pick the cheapest GPU-producing partial child path to wrap. Do not wrap
    // PostgreSQL's own Parallel SeqScan/Bitmap/Join paths: that shape keeps
    // tuple production on CPU and only adds a GpuAgg frame around it.
    let cheapest_partial =
        unsafe { super::find_cheapest_gpu_producing_path(input_ref.partial_pathlist) };
    if cheapest_partial.is_null() {
        pgrx::debug1!(
            "pg_accel partial_agg: rejected because no GPU-producing partial child exists"
        );
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
    // Phase 6 calibration: read from `DeviceLimits::gpu_partial_agg_per_row`
    // rather than the legacy `0.005` literal. The old constant was 10x the
    // measured GPU reduce throughput (~50M rows/sec → ~0.0005 cost units /
    // row on M-series) and combined with the soft-fp64 32x multiplier
    // produced a per-row cost of 0.16 for fp64 partial aggs (AVG / STDDEV /
    // VAR), which added 200K cost units to the 10M-row partial-agg path
    // and made `add_path()` always discard our `Finalize → Gather →
    // GpuAccel partial` chain. See TODO Phase 6 entry "dispatch perf /
    // probe-cost amortisation". Soft-fp64 multiplier is still applied at
    // the use site.
    let limits = cost::device_limits();
    let agg_per_row_base = limits.gpu_partial_agg_per_row;
    let agg_per_row = cost::apply_fp64_penalty(agg_per_row_base, agg_uses_fp64, limits);
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
        // Plain partial agg emits 1 row per worker. Grouped partial agg emits
        // one row per local group, matching partially_grouped_rel.rows.
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
        //    (gk_attno, gk_type_oid, gk_key_type, gk2_attno=0, gk_tlist_pos)?,
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
        if let Some(ref gk) = group_key_info {
            priv_list = lappend(priv_list, pg_sys::makeInteger(1).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(gk.attno).cast());
            priv_list = lappend(
                priv_list,
                pg_sys::makeInteger(u32::from(gk.type_oid) as i32).cast(),
            );
            priv_list = lappend(priv_list, pg_sys::makeInteger(gk.key_type).cast());
            // Two-key composite grouping is intentionally gated off above.
            priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(group_key_tlist_pos).cast());
        } else {
            priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast());
        }
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

    // Wrap the Gather in a Finalize Agg path on grouped_rel. For grouped
    // agg, propagate the processed GROUP BY clause so PG builds
    // Finalize HashAgg above the Gather.
    //
    // AggClauseCosts with zero-init gives transCost.{startup,per_tuple}=0
    // and finalCost.{startup,per_tuple}=0; that's acceptable for cost-model
    // purposes — create_agg_path still costs the Gather + Agg node itself.
    let agg_final_costs = pg_sys::AggClauseCosts::default();
    let has_group_by = group_key_info.is_some();
    let agg_strategy = if has_group_by {
        pg_sys::AggStrategy::AGG_HASHED
    } else {
        pg_sys::AggStrategy::AGG_PLAIN
    };
    let group_clause = if has_group_by {
        root_ref.processed_groupClause
    } else {
        std::ptr::null_mut()
    };
    if has_group_by && group_clause.is_null() {
        pgrx::debug1!("pg_accel partial_agg: processed_groupClause missing");
        return;
    }
    let having_qual = if has_group_by {
        query.havingQual.cast::<pg_sys::List>()
    } else {
        std::ptr::null_mut()
    };
    let num_groups = if has_group_by {
        grouped_ref.rows.max(1.0)
    } else {
        1.0
    };

    // SAFETY: create_agg_path is a standard planner helper; grouped_rel and
    // gather path are valid; grouped paths carry processed groupClause.
    let final_agg = unsafe {
        pg_sys::create_agg_path(
            root,
            grouped_rel,
            gather.cast::<Path>(),
            grouped_ref.reltarget,
            agg_strategy,
            pg_sys::AggSplit::AGGSPLIT_FINAL_DESERIAL,
            group_clause,
            having_qual,
            &raw const agg_final_costs,
            num_groups,
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
         (n_aggs={}, grouped={}, workers={}, rows={})",
        agg_descs.len(),
        has_group_by,
        cpath_workers,
        cpath_rows,
    );
}
