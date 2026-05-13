//! Parallel partial-aggregate path injection with GROUP BY support.
//!
//! ## Naming note
//!
//! Despite the file name, this module does NOT use the `PreAgg` (star-join
//! fusion) strategy. The original intent was to wire up parallel partial-emit
//! atop the [`PreAgg`](crate::engine::executor::preagg) star-join executor,
//! but `PreAggExecState` opens the fact table directly via
//! `table_open(scan_oid)` (see `engine/ffi/custom_scan/mod.rs::begin_custom_scan`,
//! `GpuStrategy::PreAgg` arm). Without scan-state synchronisation between
//! workers (each would scan the entire fact table → N-fold over-aggregation),
//! parallelising the PreAgg strategy is a separate executor refactor.
//!
//! What this module DOES: mirror [`super::partial_agg::try_inject`] but with
//! GROUP BY propagation through the manually-built Finalize Agg chain. This
//! covers the common OLAP shape (`SUM(...) ... GROUP BY dim`) that
//! `partial_agg.rs` bails on.
//!
//! The architectural follow-up (parallel PreAgg with proper scan-state
//! sharing — `parallel_aware = true`, route fact-table scan through the
//! wrapped child PlanState rather than `table_open`) is tracked in
//! [`crate::engine::executor::preagg`] and TODO.md.
//!
//! ## Status
//!
//! **Implemented.** Phase II Agent P1 lands the planner-side path
//! construction. Mirrors [`super::partial_agg::try_inject`] structurally
//! (Finalize Agg → Gather → CustomPath using `agg_path_methods` +
//! `is_partial = 1`) and adds GROUP BY propagation via
//! `root.processed_groupClause` + `parse.havingQual` into
//! `pg_sys::create_agg_path` (using `AGG_HASHED` for GROUP BY,
//! `AGG_PLAIN` otherwise).
//!
//! Phase B Agent 1B previously wired the executor-side `PreAggPrivData`
//! round-trip (`partial: Option<PartialAggSpec>`, `enable_partial(spec)`
//! invocation). That wiring is intentionally retained but unused by THIS
//! module — when the PreAgg-strategy parallelisation lands later, the same
//! `PartialAggSpec` shape will flow through `serialize_preagg_private`'s
//! `partial` argument.

use pgrx::pg_sys::{self, CustomPath, List, NodeTag, Path, lappend};

use crate::engine::cost;
use crate::engine::executor::agg::partial::{PartialAggSpec, PartialColumn};
use crate::engine::executor::agg::{AggOp, GroupKeyInfo};
use crate::engine::gucs;

use super::agg_common::{self, AggClass};
use super::custom_scan;

/// Try to inject the parallel partial-aggregate path chain (with GROUP BY
/// support) directly onto `grouped_rel->pathlist`.
///
/// Mirrors [`super::partial_agg::try_inject`] but does NOT bail on a supported
/// `groupClause` — instead propagates the GROUP BY into the manually-built
/// Finalize Agg via `root.processed_groupClause`.
///
/// Bails silently when any pre-condition isn't met (no partial paths on
/// input, unclassified Aggref, GPU unavailable, etc.).
///
/// # Safety
///
/// Called from the planner hook on the main backend thread. All pointer
/// arguments must be valid planner-provided pointers.
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
    output_rel: *mut pg_sys::RelOptInfo,
) {
    // Gate: GPU must be available and enabled.
    if !cost::gpu_is_usable() {
        pgrx::debug1!("pg_accel preagg_partial: gpu not usable");
        return;
    }

    if root.is_null() || input_rel.is_null() || output_rel.is_null() {
        return;
    }

    // SAFETY: pointers null-checked above; planner guarantees they outlive
    // the hook invocation.
    let root_ref = unsafe { &*root };
    let input_ref = unsafe { &*input_rel };
    let grouped_rel = output_rel;
    let grouped_ref = unsafe { &*grouped_rel };

    // Gate: the output rel must be eligible for parallel paths.
    if !grouped_ref.consider_parallel {
        pgrx::debug1!("pg_accel preagg_partial: grouped_rel.consider_parallel=false");
        return;
    }

    // Gate: input rel must have a non-empty partial pathlist for us to wrap.
    if input_ref.partial_pathlist.is_null()
        || unsafe { pg_sys::list_length(input_ref.partial_pathlist) } == 0
    {
        pgrx::debug1!("pg_accel preagg_partial: input_rel has no partial paths");
        return;
    }

    // Gate: query must actually request aggregation.
    // SAFETY: root.parse is a valid Query pointer.
    let parse = root_ref.parse;
    if parse.is_null() {
        return;
    }
    // SAFETY: parse non-null per the check above.
    let query = unsafe { &*parse };
    if !query.hasAggs && query.groupClause.is_null() {
        pgrx::debug1!("pg_accel preagg_partial: query has no aggregates or GROUP BY");
        return;
    }
    let group_len = if query.groupClause.is_null() {
        0
    } else {
        // SAFETY: groupClause is a valid planner-owned List.
        unsafe { pg_sys::list_length(query.groupClause) }
    };
    if group_len > 1 {
        pgrx::debug1!(
            "pg_accel preagg_partial: GROUP BY has {} keys; partial path supports one key",
            group_len
        );
        return;
    }

    // Fetch the partial grouping rel (PG 17 doesn't fire the upper-paths
    // hook at UPPERREL_PARTIAL_GROUP_AGG, so we materialise it ourselves
    // by calling fetch_upper_rel from inside the UPPERREL_GROUP_AGG hook).
    // SAFETY: fetch_upper_rel is a standard planner helper.
    let partially_grouped_rel = unsafe {
        pg_sys::fetch_upper_rel(
            root,
            pg_sys::UpperRelationKind::UPPERREL_PARTIAL_GROUP_AGG,
            grouped_ref.relids,
        )
    };
    if partially_grouped_rel.is_null() {
        pgrx::debug1!("pg_accel preagg_partial: partially_grouped_rel is NULL");
        return;
    }
    // SAFETY: non-null after check above.
    let partial_ref = unsafe { &*partially_grouped_rel };
    if !partial_ref.consider_parallel {
        pgrx::debug1!("pg_accel preagg_partial: partially_grouped_rel.consider_parallel=false");
        return;
    }

    let partial_exprs = if partial_ref.reltarget.is_null() {
        std::ptr::null_mut()
    } else {
        // SAFETY: reltarget is non-null.
        unsafe { (*partial_ref.reltarget).exprs }
    };

    let (group_key_info, group_key_tlist_pos): (Option<GroupKeyInfo>, i32) = if group_len == 1 {
        let tlist = query.targetList;
        let tlist_len = if tlist.is_null() {
            0
        } else {
            // SAFETY: targetList is a valid planner-owned List.
            unsafe { pg_sys::list_length(tlist) }
        };
        // SAFETY: groupClause has exactly one element.
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
                pgrx::debug1!("pg_accel preagg_partial: GROUP BY key is not a plain Var");
                return;
            }
            let var = expr.cast::<pg_sys::Var>();
            resolved = Some((i32::from(unsafe { (*var).varattno }), unsafe {
                (*var).vartype
            }));
            break;
        }
        let Some((attno, type_oid)) = resolved else {
            pgrx::debug1!("pg_accel preagg_partial: could not resolve GROUP BY key");
            return;
        };
        let Some(key_type) = GroupKeyInfo::key_type_from_oid(type_oid) else {
            pgrx::debug1!("pg_accel preagg_partial: unsupported GROUP BY key type");
            return;
        };
        if !matches!(key_type, 0 | 1 | 2 | 4) {
            pgrx::debug1!(
                "pg_accel preagg_partial: GROUP BY key type {} cannot be emitted by grouped agg",
                key_type
            );
            return;
        }
        let mut tlist_pos = -1;
        if !partial_exprs.is_null() {
            let n_exprs = unsafe { pg_sys::list_length(partial_exprs) };
            for i in 0..n_exprs {
                let expr = unsafe { pg_sys::list_nth(partial_exprs, i).cast::<pg_sys::Expr>() };
                if expr.is_null()
                    || unsafe { (*expr.cast::<pg_sys::Node>()).type_ } != NodeTag::T_Var
                {
                    continue;
                }
                let var = expr.cast::<pg_sys::Var>();
                if i32::from(unsafe { (*var).varattno }) == attno {
                    tlist_pos = i;
                    break;
                }
            }
        }
        if tlist_pos < 0 {
            pgrx::debug1!("pg_accel preagg_partial: grouped partial target lacks group key Var");
            return;
        }
        (
            Some(GroupKeyInfo {
                attno,
                type_oid,
                key_type,
            }),
            tlist_pos,
        )
    } else {
        (None, -1)
    };

    // Row-count threshold (mirror partial_agg).
    let rows = input_ref.rows as usize;
    if rows < cost::device_limits().gpu_reduce_min_rows {
        pgrx::debug1!(
            "pg_accel preagg_partial: rows {} < min {}",
            rows,
            cost::device_limits().gpu_reduce_min_rows
        );
        return;
    }

    // Walk the target list and validate every Aggref is one of the
    // partial-emit-supported classes. The result feeds the
    // PartialAggSpec consumed by Float8StatsEmitter / NumericSumEmitter
    // on the worker side. GROUP BY column references (T_Var) are skipped
    // here — they don't need a PartialColumn entry, but they DO need to
    // ride through `processed_groupClause` to the Finalize Agg.
    let tlist = query.targetList;
    if tlist.is_null() {
        return;
    }
    let tlist_len = unsafe { pg_sys::list_length(tlist) };
    if tlist_len == 0 {
        return;
    }

    let mut agg_descs: Vec<(AggOp, i32, u32)> = Vec::new();
    let mut partial_cols: Vec<PartialColumn> = Vec::new();
    let mut has_group_keys = false;

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
        if tag != NodeTag::T_Aggref {
            // Non-Aggref entries (typically T_Var for GROUP BY columns)
            // ride through the Finalize Agg's processed_groupClause —
            // no PartialColumn entry needed. Note that they exist so we
            // can drop the AGG_HASHED vs AGG_PLAIN decision below.
            if tag == NodeTag::T_Var {
                has_group_keys = true;
            }
            continue;
        }

        let aggref = expr.cast::<pg_sys::Aggref>();
        // SAFETY: aggref is a valid Aggref pointer.
        let Some((op, class)) = (unsafe { agg_common::classify_aggref(aggref) }) else {
            pgrx::debug1!(
                "pg_accel preagg_partial: target list contains an Aggref classify_aggref \
                 doesn't support — bailing"
            );
            return;
        };

        // SAFETY: aggref non-null and a valid Aggref.
        let aggref_ref = unsafe { &*aggref };

        if !aggref_ref.aggdistinct.is_null() {
            pgrx::debug1!("pg_accel preagg_partial: aggregate has DISTINCT clause");
            return;
        }
        if !aggref_ref.aggorder.is_null() {
            pgrx::debug1!("pg_accel preagg_partial: aggregate has ORDER BY clause");
            return;
        }
        if !aggref_ref.aggfilter.is_null() {
            pgrx::debug1!("pg_accel preagg_partial: aggregate has FILTER clause");
            return;
        }

        // Mirror the gate from partial_agg.rs:192-203 — Float8StatsEmitter
        // requires `_float8` (FLOAT8ARRAYOID) transtype. INTERNAL transtypes
        // (numeric_avg_accum, int8_avg_accum) need dedicated emitter support
        // we don't have yet — bail rather than emit a wrong shape.
        let float_stats_op = matches!(
            op,
            AggOp::Avg | AggOp::StddevSamp | AggOp::StddevPop | AggOp::VarSamp | AggOp::VarPop
        );
        if float_stats_op && aggref_ref.aggtranstype != pg_sys::FLOAT8ARRAYOID {
            pgrx::debug1!(
                "pg_accel preagg_partial: AVG/STDDEV/VAR with transtype={} not `_float8` — bail",
                u32::from(aggref_ref.aggtranstype),
            );
            return;
        }

        let serialize_fn_oid = match class {
            AggClass::Float8Stats { serialize_fn } if serialize_fn != pg_sys::InvalidOid => {
                Some(serialize_fn)
            }
            _ => None,
        };

        // Resolve the input column attno (0 for COUNT(*)).
        let (attno, rtype) = if op == AggOp::Count && aggref_ref.aggstar {
            (0_i32, u32::from(aggref_ref.aggtype))
        } else {
            let args = aggref_ref.args;
            if args.is_null() || unsafe { pg_sys::list_length(args) } < 1 {
                return;
            }
            // SAFETY: first arg is a TargetEntry.
            let arg_tle = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::TargetEntry>() };
            if arg_tle.is_null() {
                return;
            }
            // SAFETY: arg_tle is a valid TargetEntry.
            let arg_expr = unsafe { (*arg_tle).expr };
            if arg_expr.is_null() {
                return;
            }
            // SAFETY: arg_expr is a valid Expr pointer.
            let extracted = unsafe { super::extract_var_attno(arg_expr) };
            (extracted, u32::from(aggref_ref.aggtype))
        };

        agg_descs.push((op, attno, rtype));
        partial_cols.push(PartialColumn {
            op,
            attno,
            transtype_oid: aggref_ref.aggtranstype,
            serialize_fn_oid,
        });
    }

    if partial_cols.is_empty() {
        pgrx::debug1!("pg_accel preagg_partial: no Aggrefs in target list");
        return;
    }

    // Detect GROUP BY presence from the parsed query (root.processed_groupClause
    // is what create_agg_path expects; query.groupClause is the pre-processed
    // form). When present, has_group_keys is true (set above when we saw any
    // T_Var in the target list), and we feed AGG_HASHED + the processed clause.
    let has_group_by =
        group_key_info.is_some() || (has_group_keys && !root_ref.processed_groupClause.is_null());

    // Pick the cheapest partial child path to wrap.
    // SAFETY: partial_pathlist is non-empty.
    let cheapest_partial =
        unsafe { pg_sys::list_nth(input_ref.partial_pathlist, 0).cast::<Path>() };
    if cheapest_partial.is_null() {
        return;
    }
    // SAFETY: cheapest_partial is non-null.
    let base = unsafe { &*cheapest_partial };

    // Cost: mirror partial_agg.rs cost formula.
    let gpu_overhead = cost::GPU_LAUNCH_OVERHEAD;
    let float8_u32 = u32::from(pg_sys::FLOAT8OID);
    let agg_uses_fp64 = partial_cols
        .iter()
        .any(|c| u32::from(c.transtype_oid) == float8_u32)
        || agg_descs
            .iter()
            .any(|(op, _, rtype)| matches!(op, AggOp::Avg) || *rtype == float8_u32);
    // Phase 6 calibration: read from `DeviceLimits::gpu_partial_agg_per_row`
    // (same fix as `partial_agg::try_inject` — the legacy `0.005` was 10x
    // measured GPU reduce throughput, dominating the partial-agg cost when
    // the soft-fp64 multiplier applied).
    let limits = cost::device_limits();
    let agg_per_row_base = limits.gpu_partial_agg_per_row;
    let agg_per_row = cost::apply_fp64_penalty(agg_per_row_base, agg_uses_fp64, limits);
    let reduce_cost = base.rows * agg_per_row;
    let startup_cost = base.total_cost + gpu_overhead;
    let total_cost = (base.total_cost + reduce_cost)
        .mul_add(cost::GPU_COST_SAFETY_MARGIN, gpu_overhead)
        * gucs::cost_multiplier();

    // Build the partial CustomPath (worker-side partial agg, parented at
    // `partially_grouped_rel`).
    // SAFETY: Allocating via palloc, building valid CustomPath.
    let cpath = unsafe {
        let cpath = pg_sys::palloc0(std::mem::size_of::<CustomPath>()).cast::<CustomPath>();

        (*cpath).path.type_ = NodeTag::T_CustomPath;
        (*cpath).path.pathtype = NodeTag::T_CustomScan;
        (*cpath).path.parent = partially_grouped_rel;
        // Deep-copy pathtarget so later planner mutations don't alias into
        // shared Aggref sub-trees (mirrors partial_agg.rs:291).
        (*cpath).path.pathtarget = pg_sys::copy_pathtarget(partial_ref.reltarget);
        (*cpath).path.param_info = std::ptr::null_mut();
        (*cpath).path.parallel_aware = false;
        (*cpath).path.parallel_safe = true;
        (*cpath).path.parallel_workers = base.parallel_workers.max(1);
        (*cpath).path.rows = partial_ref.rows.max(1.0);
        (*cpath).path.startup_cost = startup_cost;
        (*cpath).path.total_cost = total_cost;
        (*cpath).path.pathkeys = std::ptr::null_mut();

        (*cpath).flags = 0;
        (*cpath).custom_paths = lappend(std::ptr::null_mut(), cheapest_partial.cast());
        (*cpath).custom_restrictinfo = std::ptr::null_mut();
        (*cpath).methods = custom_scan::agg_path_methods();

        // Serialize the custom_private layout expected by plan_custom_path_agg
        // (mirrors partial_agg.rs). Layout:
        //   [num_aggs, op0, attno0, rtype0, ..., has_group_key,
        //    (gk_attno, gk_type_oid, gk_key_type, gk2_attno=0, gk_tlist_pos)?,
        //    self_scan_relid=0, is_partial=1, PARTIAL_SENTINEL block]
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
            priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast());
            priv_list = lappend(priv_list, pg_sys::makeInteger(group_key_tlist_pos).cast());
        } else {
            priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast());
        }
        // No self-scan (child is PG's parallel seq scan).
        priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast());
        // is_partial = 1.
        priv_list = lappend(priv_list, pg_sys::makeInteger(1).cast());

        // Append full PartialAggSpec (transtype + serialize_fn per column)
        // so plan_custom_path_agg can materialise Float8StatsEmitter for
        // AVG/STDDEV/VAR without re-walking Aggrefs.
        let spec = PartialAggSpec {
            per_column: partial_cols.clone(),
        };
        priv_list = custom_scan::append_partial_spec(priv_list, &spec);

        (*cpath).custom_private = priv_list;
        cpath
    };

    // Wrap the partial CustomPath in a Gather path rooted at
    // partially_grouped_rel.
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

    // Wrap the Gather in a Finalize Agg path on grouped_rel.
    //
    // GROUP BY propagation: mirror PG's own
    // `add_paths_to_grouping_rel` (planner.c:7253-7263) — the parallel
    // Finalize Agg uses `root->processed_groupClause` for groupClause and
    // the Query's havingQual for qual. AggStrategy is AGG_HASHED when
    // GROUP BY is present (typical OLAP shape), AGG_PLAIN otherwise.
    //
    // numGroups: estimate from `input_rel->rows` square-root for a rough
    // cardinality, clamped. Matches the shape used in
    // `pgaccel_inject_gpu_preagg::n_groups` (planner_hooks/mod.rs:1001-1005).
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
    let having_qual = query.havingQual.cast::<pg_sys::List>();
    let num_groups = if has_group_by {
        input_ref.rows.sqrt().clamp(1.0, 10_000.0)
    } else {
        1.0
    };

    // AggClauseCosts with zero-init gives transCost=0/finalCost=0; that's
    // acceptable — create_agg_path still costs the Gather + Agg node itself.
    let agg_final_costs = pg_sys::AggClauseCosts::default();
    // SAFETY: create_agg_path is a standard planner helper; grouped_rel and
    // gather path are valid; group_clause is either NULL (plain) or
    // root.processed_groupClause.
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
        "pg_accel preagg_partial: injected Finalize({}) -> Gather -> GpuAccel(partial) chain \
         (n_aggs={}, has_group_by={}, workers={}, rows={})",
        if has_group_by { "HashAgg" } else { "PlainAgg" },
        agg_descs.len(),
        has_group_by,
        cpath_workers,
        cpath_rows,
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// Widened from `cfg(test)` to `cfg(any(test, feature = "pg_test"))` so
// pgrx's schema generator (which builds with `feature = "pg_test"`, NOT
// `cfg(test)`) can see this module and recurse into the nested
// `#[pgrx::pg_schema] mod tests` below to register the pg_test
// functions. Without this widening, `cargo pgrx test` fails with
// "function tests.X does not exist" because the CREATE FUNCTION DDL is
// never emitted.
#[cfg(any(test, feature = "pg_test"))]
#[allow(clippy::unwrap_used)]
mod tests {
    //! Unit tests for the preagg_partial chain construction.
    //!
    //! Pure-Rust tests live here. The PG-live `pg_test` that exercises the
    //! full path injection (table fixture + EXPLAIN check) lives in the
    //! `pg_test_explain` submodule below.
    use pgrx::pg_sys;

    /// Mirror the `has_group_by` predicate in [`super::try_inject`]:
    /// true when target list contains any T_Var (GROUP BY column) OR
    /// when the parsed query has a non-NULL `groupClause` AND the planner
    /// has resolved `processed_groupClause`.
    #[cfg(test)]
    fn has_group_by(
        has_group_keys_in_tlist: bool,
        query_group_clause_nonnull: bool,
        root_processed_clause_nonnull: bool,
    ) -> bool {
        has_group_keys_in_tlist || (query_group_clause_nonnull && root_processed_clause_nonnull)
    }

    /// Mirror the AggStrategy / numGroups decision in [`super::try_inject`].
    #[cfg(test)]
    fn agg_strategy_for_group_by(has_group_by: bool) -> pg_sys::AggStrategy::Type {
        if has_group_by {
            pg_sys::AggStrategy::AGG_HASHED
        } else {
            pg_sys::AggStrategy::AGG_PLAIN
        }
    }

    #[cfg(test)]
    fn num_groups_for_group_by(rows: f64, has_group_by: bool) -> f64 {
        if has_group_by {
            rows.sqrt().clamp(1.0, 10_000.0)
        } else {
            1.0
        }
    }

    #[test]
    fn group_by_predicate_true_when_tlist_has_var() {
        // Target list contains a T_Var (GROUP BY column reference).
        assert!(has_group_by(true, false, false));
        assert!(has_group_by(true, true, true));
    }

    #[test]
    fn group_by_predicate_true_when_clause_resolved() {
        // Even without target-list T_Var (e.g. SELECT count(*) ... GROUP BY x),
        // a non-NULL processed clause counts.
        assert!(has_group_by(false, true, true));
    }

    #[test]
    fn group_by_predicate_false_when_neither_signal_present() {
        // Plain aggregate: no T_Var, no groupClause.
        assert!(!has_group_by(false, false, false));
        // groupClause set but planner hasn't resolved it yet (rare; guard
        // against feeding a NULL processed clause to create_agg_path).
        assert!(!has_group_by(false, true, false));
        assert!(!has_group_by(false, false, true));
    }

    #[test]
    fn agg_strategy_hashed_when_group_by_present() {
        assert_eq!(
            agg_strategy_for_group_by(true),
            pg_sys::AggStrategy::AGG_HASHED,
        );
    }

    #[test]
    fn agg_strategy_plain_when_no_group_by() {
        assert_eq!(
            agg_strategy_for_group_by(false),
            pg_sys::AggStrategy::AGG_PLAIN,
        );
    }

    #[test]
    fn num_groups_one_when_no_group_by() {
        // Plain aggregate emits exactly one row.
        assert!((num_groups_for_group_by(1_000_000.0, false) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn num_groups_sqrt_estimate_when_group_by_clamped_to_min() {
        // Tiny relation: sqrt(4) = 2, above the clamp floor of 1.
        let n = num_groups_for_group_by(4.0, true);
        assert!((n - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn num_groups_sqrt_estimate_when_group_by_clamped_to_max() {
        // Massive relation: sqrt(10^9) ~ 31_623, clamped to 10_000.
        let n = num_groups_for_group_by(1e9, true);
        assert!((n - 10_000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn num_groups_sqrt_within_clamp_range() {
        // 1M rows → sqrt = 1_000, inside [1, 10_000].
        let n = num_groups_for_group_by(1_000_000.0, true);
        assert!((n - 1_000.0).abs() < 1e-6);
    }

    /// Regression guard: the chain construction must use AGGSPLIT_FINAL_DESERIAL
    /// (NOT AGGSPLIT_INITIAL_SERIAL or AGGSPLIT_SIMPLE) for the Finalize Agg.
    /// `_DESERIAL` because workers emit serialized partial state via
    /// `Float8StatsEmitter` etc., and the Finalize Agg must deserialise before
    /// combining.
    #[test]
    fn finalize_agg_split_is_final_deserial_constant() {
        // Smoke: the constant exists and is distinct from SIMPLE/INITIAL_SERIAL.
        let final_deserial = pg_sys::AggSplit::AGGSPLIT_FINAL_DESERIAL;
        let simple = pg_sys::AggSplit::AGGSPLIT_SIMPLE;
        let initial_serial = pg_sys::AggSplit::AGGSPLIT_INITIAL_SERIAL;
        assert_ne!(final_deserial, simple);
        assert_ne!(final_deserial, initial_serial);
    }

    // -----------------------------------------------------------------------
    // PG-live tests
    //
    // These exercise `try_inject` against a real planner via Spi-driven
    // EXPLAIN queries. Observable two ways: (a) `pg_accel_stats()` counters
    // set by the hook, or (b) the EXPLAIN output structure (Finalize Agg →
    // Gather → CustomScan (GpuAccel)). We use (b) — it tests the end-to-end
    // shape: planner gating + chain construction + downstream
    // `set_customscan_references` happiness.
    //
    // The nested `mod tests` is required so pgrx's pg_test framework, which
    // hard-codes its SQL function lookup as `tests."<funcname>"()` (see
    // pgrx-tests-0.17.0/src/framework.rs:122 and
    // custom_scan/tests.rs:1311-1326), resolves the function. The pgrx
    // schema generator picks up the inner module's name (= `tests`) from
    // the `#[pgrx::pg_schema]` attribute. Earlier siblings of the outer
    // mod (e.g. `preagg_explain`) caused every pg_test invocation to fail
    // with "function tests.X does not exist".
    // -----------------------------------------------------------------------
    #[cfg(feature = "pg_test")]
    #[pgrx::pg_schema]
    mod tests {
        use pgrx::prelude::{Spi, pg_test};

        /// Drive a query whose star-shape would normally trigger the
        /// `pgaccel_inject_gpu_preagg` hook AND whose grouped_rel is
        /// parallel-eligible. We don't strictly need the EXPLAIN to show our
        /// custom path (PG may pick its own parallel HashAgg if cheaper); we
        /// just need the SQL to plan + execute without crashing the planner.
        ///
        /// This is the fork-safety smoke for the new `add_path` call: if the
        /// chain construction produces a malformed Path subclass, PG's planner
        /// asserts at `create_plan` time and the backend dies. Surviving the
        /// query proves the chain is at minimum well-formed.
        #[pg_test]
        fn preagg_partial_chain_does_not_crash_planner_with_group_by() {
            // Setup: a small fact table with enough rows to clear the
            // `gpu_reduce_min_rows` gate (default 5_000-25_000 depending on
            // device profile). 50_000 is comfortably above any reasonable
            // device's minimum.
            Spi::run(
                "DROP TABLE IF EXISTS pgaccel_preagg_partial_smoke; \
             CREATE UNLOGGED TABLE pgaccel_preagg_partial_smoke (\
                k int4, v float8, region text); \
             INSERT INTO pgaccel_preagg_partial_smoke \
             SELECT i % 100, random() * 1000.0, ('region_' || (i % 10))::text \
             FROM generate_series(1, 50000) i; \
             ANALYZE pgaccel_preagg_partial_smoke;",
            )
            .expect("setup table");

            // Query with GROUP BY — exercises the AGG_HASHED branch.
            let row_count: i64 = Spi::get_one::<i64>(
                "SELECT count(*) FROM (\
                SELECT region, sum(v) AS s \
                FROM pgaccel_preagg_partial_smoke \
                GROUP BY region\
             ) t",
            )
            .expect("aggregating select with GROUP BY")
            .expect("row_count non-NULL");
            // 10 distinct regions, so the outer count should be 10.
            assert_eq!(row_count, 10);

            // Plain aggregate without GROUP BY — exercises the AGG_PLAIN branch.
            let one_row: f64 =
                Spi::get_one::<f64>("SELECT sum(v) FROM pgaccel_preagg_partial_smoke")
                    .expect("plain aggregate select")
                    .expect("sum non-NULL");
            assert!(one_row.is_finite() && one_row > 0.0);

            Spi::run("DROP TABLE pgaccel_preagg_partial_smoke;").expect("drop");
        }
    }
}
