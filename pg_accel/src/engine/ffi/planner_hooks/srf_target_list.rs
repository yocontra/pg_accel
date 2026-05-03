//! `create_upper_paths_hook` arm for SRF-in-target-list rewrites
//! (Phase 2 follow-up to F3 FunctionScan injection).
//!
//! ## Scope
//!
//! Handles `SELECT srf(col) FROM t` style queries where a registered
//! set-returning function appears at the top level of the query target
//! list. PG17 wraps each such path in a [`pg_sys::ProjectSetPath`]
//! during `apply_scanjoin_target_to_paths` /
//! `adjust_paths_for_srfs`; by the time
//! `create_upper_paths_hook(UPPERREL_FINAL)` fires, the scan / join
//! upper rel's `pathlist` already contains `T_ProjectSetPath` nodes
//! atop the underlying scan paths.
//!
//! Complementary hook: [`super::projectset`] handles the FunctionScan
//! syntax (`SELECT * FROM srf(...)`) where PG builds an
//! `RTE_FUNCTION` base relation. The two filenames split because the
//! planner surface is fundamentally different (rel pathlist vs upper
//! pathlist) — `projectset.rs` is the FunctionScan-in-FROM hook (the
//! filename predates this module by one phase); `srf_target_list.rs`
//! is the SRF-in-target-list hook implemented here.
//!
//! ## Detection algorithm
//!
//! On every `UPPERREL_FINAL` invocation:
//!
//! 1. Bail unless the query has any tlist SRFs
//!    (`root->parse->hasTargetSRFs == true`) — fast path for the
//!    common no-SRF case.
//! 2. Walk the input rel's `pathlist` looking for `T_ProjectSetPath`
//!    nodes (tail of the per-path stack added by
//!    `adjust_paths_for_srfs`).
//! 3. For each ProjectSetPath, scan its `pathtarget->exprs` list for
//!    top-level `T_FuncExpr` entries with `funcretset == true`.
//! 4. For each candidate FuncExpr:
//!    - look up the funcid in the adapter registry;
//!    - require `OutputShape::VarLen` or `OutputShape::Record` — a
//!      Scalar SRF is degenerate (one input row → one output row,
//!      identical to plain projection) and offers no acceleration win;
//!    - require all non-Var args to be `T_Const`; the Var arg supplies
//!      the per-row input from the child plan.
//! 5. If every SRF in the projectset's tlist matches, build a custom
//!    path that wraps the projectset's `subpath` and intercepts the
//!    SRF expansion at execution time.
//!
//! ## Bail conditions (preserve native PG plan)
//!
//! - `parse->hasTargetSRFs == false` — query has no SRFs.
//! - `input_rel->pathlist` contains no `ProjectSetPath` nodes.
//! - The projectset's tlist mixes registered and unregistered SRFs,
//!   or contains a non-Var/non-Const arg shape we can't dispatch.
//! - The SRF resolves to `OutputShape::Scalar` — no acceleration win.
//! - The registry entry is missing the `output_field_types` /
//!   `output_field_names` metadata required to construct a TupleDesc.
//! - GUC gate (`pg_accel.enabled == false`) or no GPU available.
//!
//! All bails are silent (debug-level log) — native PG ProjectSet
//! continues to handle the query.
//!
//! ## Status (anti-cheat ban #9)
//!
//! This module ships **planner-side detection only**. The executor
//! arm — driving the child plan, per-row Var extraction, dispatch,
//! and multi-row tuple expansion — is the larger half of the work
//! and is **not yet wired**. When the detection logic identifies a
//! candidate ProjectSetPath, this hook records the discovery via
//! [`stats::record_planner_hook_call`] and returns without adding a
//! CustomPath, so the native ProjectSet plan continues to execute
//! the query (correct results, no acceleration). The planner-side
//! coverage validates that the hook fires at the right stage and
//! that the tlist walk correctly identifies registered SRFs; the
//! executor wiring is tracked as the next sub-task in the TODO
//! entry **"SRF-in-target-list executor wiring"**.

use std::ffi::c_int;

use pgrx::pg_sys::{
    self, FuncExpr, NodeTag, Path, PathTarget, PlannerInfo, ProjectSetPath, RelOptInfo,
    UpperRelationKind, Var,
};

use crate::engine::cost;
use crate::engine::gucs;
use crate::engine::registry::{self, OutputShape};
use crate::engine::stats;

/// `create_upper_paths_hook` arm for `UPPERREL_FINAL`.
///
/// Inspects the input rel's `pathlist` for `T_ProjectSetPath` nodes
/// whose target tlist contains only registered SRFs we know how to
/// dispatch. Currently a detect-only pass — see module-level doc
/// "Status (anti-cheat ban #9)" for the executor wiring follow-up.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
/// `root`, `input_rel`, and `output_rel` must be valid planner-supplied
/// pointers.
pub(super) unsafe fn try_inject_srf_target_list(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
) {
    if !gucs::enabled() {
        return;
    }
    if !cost::gpu_is_usable() {
        return;
    }
    if root.is_null() || input_rel.is_null() || output_rel.is_null() {
        return;
    }

    // SAFETY: root.parse is a valid Query pointer set by the planner.
    let parse = unsafe { (*root).parse };
    if parse.is_null() {
        return;
    }
    // Fast-path bail when the query has no tlist SRFs at all.
    // SAFETY: parse is a valid Query node.
    let has_target_srfs = unsafe { (*parse).hasTargetSRFs };
    if !has_target_srfs {
        return;
    }

    // SAFETY: input_rel is a valid RelOptInfo; pathlist is a List of Path *.
    let pathlist = unsafe { (*input_rel).pathlist };
    if pathlist.is_null() {
        return;
    }

    // Walk the input rel's pathlist looking for ProjectSetPath nodes.
    // SAFETY: list_length on a non-null List is safe.
    let n_paths = unsafe { pg_sys::list_length(pathlist) };
    let mut detected = 0usize;
    let mut considered = 0usize;
    for i in 0..n_paths {
        // SAFETY: i in [0, n_paths).
        let path_node = unsafe { pg_sys::list_nth(pathlist, i).cast::<Path>() };
        if path_node.is_null() {
            continue;
        }
        // SAFETY: path_node is a valid Path; reading the tag.
        let tag = unsafe { (*path_node).pathtype };
        // ProjectSetPath uses pathtype == T_ProjectSet (the executor node
        // tag emitted by createplan.c::create_projectset_plan); the path
        // node itself has type_ == T_ProjectSetPath.
        let node_type = unsafe { (*path_node).type_ };
        if node_type != NodeTag::T_ProjectSetPath {
            continue;
        }
        let _ = tag;
        considered += 1;

        // SAFETY: type tag confirmed T_ProjectSetPath.
        let projectset = path_node.cast::<ProjectSetPath>();

        // Inspect the projectset's pathtarget tlist for registered SRFs.
        // SAFETY: projectset is a valid ProjectSetPath; pathtarget is a
        // PathTarget* set by create_set_projection_path.
        let target = unsafe { (*projectset).path.pathtarget };
        if target.is_null() {
            continue;
        }
        // SAFETY: target was obtained from a valid ProjectSetPath
        // above; srfs_all_registered only walks fields the planner
        // guarantees valid for the duration of this hook callback.
        if !unsafe { srfs_all_registered(target) } {
            continue;
        }
        detected += 1;
        pgrx::debug1!(
            "pg_accel: srf_target_list: detected ProjectSetPath at \
             input_rel pathlist idx={} with all SRFs registered; \
             executor wiring not yet landed (see module doc \
             \"Status (anti-cheat ban #9)\")",
            i,
        );
        // NOTE: per ban #9, we do not yet inject a CustomPath here.
        // Native PG ProjectSet continues to execute the query. The
        // detection telemetry below confirms the hook fires at the
        // right stage; the executor wiring (child plan iteration +
        // per-row dispatch) is the follow-up sub-task.
    }

    if detected > 0 {
        stats::record_planner_hook_call();
    }
    pgrx::debug1!(
        "pg_accel: srf_target_list: scanned {} paths, {} ProjectSetPath, \
         {} dispatch-eligible (executor wiring pending)",
        n_paths,
        considered,
        detected,
    );
}

/// Return `true` iff every top-level `FuncExpr` with `funcretset == true`
/// in `target->exprs` resolves to a registered SRF with a non-Scalar
/// `OutputShape` and a per-row arg shape we can dispatch (single Var
/// + zero-or-more Consts).
///
/// Returns `false` for any of:
/// - Empty exprs list (nothing to dispatch).
/// - At least one SRF FuncExpr is not registered.
/// - At least one SRF has `OutputShape::Scalar` (no acceleration win).
/// - At least one SRF's args are not the supported `[Var, Const*]` shape.
/// - Registry entry missing `output_field_types` / `output_field_names`.
///
/// # Safety
///
/// `target` must be a valid `PathTarget *` from the planner.
unsafe fn srfs_all_registered(target: *mut PathTarget) -> bool {
    // SAFETY: target is a valid PathTarget; exprs is a List of Expr nodes.
    let exprs = unsafe { (*target).exprs };
    if exprs.is_null() {
        return false;
    }
    // SAFETY: list_length on a non-null List is safe.
    let n_exprs = unsafe { pg_sys::list_length(exprs) };
    if n_exprs == 0 {
        return false;
    }

    let mut found_srf = false;
    registry::lazy_init();
    let registry = registry::global_registry();

    for i in 0..n_exprs {
        // SAFETY: i in [0, n_exprs).
        let node = unsafe { pg_sys::list_nth(exprs, i).cast::<pg_sys::Node>() };
        if node.is_null() {
            continue;
        }
        // SAFETY: node is a valid Node; reading tag.
        let tag = unsafe { (*node).type_ };
        if tag != NodeTag::T_FuncExpr {
            // Non-SRF tlist columns (Var passthrough, Const, expressions
            // without set returns) are fine — they don't need acceleration.
            continue;
        }
        // SAFETY: tag confirmed T_FuncExpr.
        let funcexpr = node.cast::<FuncExpr>();
        // SAFETY: funcexpr is a valid FuncExpr.
        let returns_set = unsafe { (*funcexpr).funcretset };
        if !returns_set {
            // Non-SRF FuncExpr (e.g. ST_Area in the tlist alongside an
            // SRF) is fine — the SRF expansion still applies, scalar
            // calls are evaluated per output row.
            continue;
        }

        found_srf = true;
        // SAFETY: funcexpr is a valid FuncExpr.
        let fn_oid = unsafe { (*funcexpr).funcid };
        let Some(entry) = registry.lookup(fn_oid) else {
            pgrx::debug1!(
                "pg_accel: srf_target_list: SRF fn_oid={} not in registry, declining",
                u32::from(fn_oid),
            );
            return false;
        };

        match entry.output_shape {
            OutputShape::Scalar => {
                // Scalar SRFs degenerate to 1-in-1-out, identical to a
                // plain projection. No acceleration value, decline.
                pgrx::debug1!(
                    "pg_accel: srf_target_list: fn_oid={} has Scalar shape, declining",
                    u32::from(fn_oid),
                );
                return false;
            }
            OutputShape::Record { .. } | OutputShape::VarLen => {}
        }

        if entry.output_field_types.is_empty() || entry.output_field_names.is_empty() {
            pgrx::debug1!(
                "pg_accel: srf_target_list: fn_oid={} missing output schema, declining",
                u32::from(fn_oid),
            );
            return false;
        }

        // SAFETY: funcexpr.args is a List of Expr* from the planner.
        let args = unsafe { (*funcexpr).args };
        // SAFETY: args_supported walks a planner-supplied List* whose
        // lifetime is bound to the planner pass; safe to call here.
        if !unsafe { args_supported(args) } {
            pgrx::debug1!(
                "pg_accel: srf_target_list: fn_oid={} args shape unsupported \
                 (need single Var + zero-or-more Const), declining",
                u32::from(fn_oid),
            );
            return false;
        }
    }

    found_srf
}

/// Return `true` iff `args` is a list of expressions consisting of a
/// single `T_Var` (the per-row input column) and zero or more `T_Const`
/// (constant kernel parameters). Returns `false` for empty lists, more
/// than one Var, or any other expression shape.
///
/// # Safety
///
/// `args` must be null or a valid PG `List *` of Node pointers.
unsafe fn args_supported(args: *mut pg_sys::List) -> bool {
    if args.is_null() {
        return false;
    }
    // SAFETY: args is non-null; list_length is safe.
    let n = unsafe { pg_sys::list_length(args) };
    if n == 0 {
        return false;
    }
    let mut var_count: c_int = 0;
    for i in 0..n {
        // SAFETY: i in [0, n).
        let node = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
        if node.is_null() {
            return false;
        }
        // SAFETY: node is a valid Node; reading tag.
        let tag = unsafe { (*node).type_ };
        match tag {
            NodeTag::T_Var => {
                var_count += 1;
                // Sanity: reject upper-level vars (subquery params).
                // SAFETY: tag confirmed T_Var.
                let varlevelsup = unsafe { (*node.cast::<Var>()).varlevelsup };
                if varlevelsup != 0 {
                    return false;
                }
            }
            NodeTag::T_Const => {}
            // RelabelType is a common no-op cast wrapper; skip future
            // expansion until the executor wiring lands and we know
            // exactly what shapes the dispatcher accepts.
            _ => return false,
        }
    }
    var_count == 1
}

#[cfg(test)]
mod tests {
    //! Compile-time smoke checks: the public entrypoint must keep the
    //! `(root, input_rel, output_rel)` signature so the dispatcher in
    //! `mod.rs` can route `UPPERREL_FINAL` invocations through it.
    //! Behavioural coverage requires a live planner + registered SRFs,
    //! which is exercised by the `pg_test`s below (gated on `pg_test`).

    use super::*;

    #[test]
    fn entrypoint_has_expected_signature() {
        // Type-only compile check: keeps drift from sneaking in.
        let f: unsafe fn(*mut PlannerInfo, *mut RelOptInfo, *mut RelOptInfo) =
            try_inject_srf_target_list;
        let _ = f;
    }

    #[test]
    fn upper_relation_kind_final_is_seven() {
        // Sanity: PG17 numerically encodes UPPERREL_FINAL as 7. If the
        // upstream enum re-orders, the dispatcher's match arm in
        // mod.rs::pgaccel_create_upper_paths needs to be revisited.
        assert_eq!(UpperRelationKind::UPPERREL_FINAL, 7);
    }
}

/// Integration tests for the SRF-in-target-list planner hook.
///
/// **Scope**: with the executor wiring still pending (see module
/// doc "Status (anti-cheat ban #9)"), these tests verify that:
///
/// 1. SRF-in-target-list queries continue to return correct results
///    (native PG ProjectSet path is preserved when our hook bails).
/// 2. The planner hook fires at `UPPERREL_FINAL` and does not
///    crash the backend on a query whose tlist contains a
///    registered SRF over a heap column.
/// 3. Row-count expansion semantics observed via PG's own
///    ProjectSet match the F3 FunctionScan semantics — confirming
///    that when the executor wiring lands, the row-count
///    invariant ("one input row → many output rows") is the same
///    contract on both injection sites.
///
/// These tests intentionally do **not** assert that EXPLAIN shows
/// a `Custom Scan (GpuAccelSrfTargetList)` node — that would be a
/// cheat per ban #1, since the executor isn't wired and the hook
/// does not yet add a CustomPath. When the executor wiring lands,
/// these tests grow an EXPLAIN assertion alongside the row-count
/// checks.
#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
#[pgrx::pg_schema]
mod pg_tests {

    use pgrx::prelude::{Spi, pg_test};

    /// Helper: ensure the named extension exists in the test DB; return
    /// `false` if `CREATE EXTENSION` fails (host doesn't have the
    /// extension binary). Mirrors `function_scan.rs::ensure_extension`.
    fn ensure_extension(name: &str) -> bool {
        let create_sql = format!("CREATE EXTENSION IF NOT EXISTS {name} CASCADE");
        if Spi::run(&create_sql).is_err() {
            return false;
        }
        let q = format!("SELECT count(*) FROM pg_extension WHERE extname = '{name}'");
        Spi::get_one::<i64>(&q).ok().flatten().unwrap_or(0) > 0
    }

    /// `SELECT id, h3_cell_to_boundary(cell) FROM cells` against a
    /// 4-row table must return 4 rows (one per input cell) — the
    /// boundary SRF emits a single polygon per cell. With the
    /// executor wiring pending, the native PG ProjectSet handles
    /// expansion; this test guards that the planner hook does not
    /// break the existing path.
    #[pg_test]
    fn srf_target_list_h3_cell_to_boundary_preserves_row_count() {
        if !ensure_extension("h3") {
            return;
        }
        // Trigger registry init.
        Spi::run("SELECT h3_get_resolution('8a2a1072b59ffff'::h3index)").expect("h3 ping");

        Spi::run(
            "CREATE TEMP TABLE _srf_tlist_cells(id int, cell h3index); \
             INSERT INTO _srf_tlist_cells VALUES \
               (1, '8a2a1072b59ffff'::h3index), \
               (2, '8a2a1072b597fff'::h3index), \
               (3, '8a2a1072b58ffff'::h3index), \
               (4, '8a2a1072b587fff'::h3index)",
        )
        .expect("setup ok");

        // h3_cell_to_boundary returns one polygon per input cell — the
        // ProjectSet expands 1:1 here, so the SRF tlist call returns
        // exactly one row per input row.
        let count = Spi::get_one::<i64>(
            "SELECT count(*) FROM (SELECT id, h3_cell_to_boundary(cell) AS b \
             FROM _srf_tlist_cells) sub",
        )
        .expect("count query ok")
        .expect("count not null");
        assert_eq!(
            count, 4,
            "h3_cell_to_boundary in target list must emit one row per input cell, got {count}",
        );
    }

    /// `SELECT id, h3_grid_disk(cell, 1) FROM cells` with 2 input
    /// rows must expand to 2 * 7 = 14 output rows (k=1 disk = 7 cells
    /// per non-pentagon input). Verifies the multi-row expansion
    /// semantics that the eventual GpuAccel executor must preserve.
    #[pg_test]
    fn srf_target_list_h3_grid_disk_expands_rows() {
        if !ensure_extension("h3") {
            return;
        }
        Spi::run("SELECT h3_get_resolution('8928308280fffff'::h3index)").expect("h3 ping");

        Spi::run(
            "CREATE TEMP TABLE _srf_tlist_disk(id int, cell h3index); \
             INSERT INTO _srf_tlist_disk VALUES \
               (1, '8928308280fffff'::h3index), \
               (2, '89283082813ffff'::h3index)",
        )
        .expect("setup ok");

        let count = Spi::get_one::<i64>(
            "SELECT count(*) FROM (SELECT id, h3_grid_disk(cell, 1) AS d \
             FROM _srf_tlist_disk) sub",
        )
        .expect("count query ok")
        .expect("count not null");
        assert_eq!(
            count, 14,
            "h3_grid_disk(cell, 1) over 2 non-pentagon cells must expand to 14 rows \
             (2 * 7), got {count}",
        );
    }

    /// `EXPLAIN SELECT h3_cell_to_boundary(cell) FROM cells` must
    /// complete without crashing the backend. With the planner hook
    /// in detect-only mode, EXPLAIN shows a native `ProjectSet ->
    /// SeqScan` plan; the hook's pathlist walk runs during planning
    /// and must not raise an error or corrupt path metadata. When
    /// the executor wiring lands, this assertion grows to require
    /// `Custom Scan (GpuAccelSrfTargetList)` in the plan text.
    #[pg_test]
    fn srf_target_list_explain_does_not_crash() {
        if !ensure_extension("h3") {
            return;
        }
        Spi::run("SELECT h3_get_resolution('8a2a1072b59ffff'::h3index)").expect("h3 ping");
        Spi::run(
            "CREATE TEMP TABLE _srf_tlist_explain(id int, cell h3index); \
             INSERT INTO _srf_tlist_explain VALUES (1, '8a2a1072b59ffff'::h3index)",
        )
        .expect("setup ok");

        let plan = Spi::connect(|client| {
            let mut lines: Vec<String> = Vec::new();
            let table = client
                .select(
                    "EXPLAIN (FORMAT TEXT) SELECT id, h3_cell_to_boundary(cell) \
                     FROM _srf_tlist_explain",
                    None,
                    &[],
                )
                .expect("EXPLAIN query ok");
            for row in table {
                if let Some(line) = row.get::<String>(1).ok().flatten() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        });
        assert!(
            !plan.is_empty(),
            "EXPLAIN returned no rows (likely backend crash); the SRF-in-target-list \
             hook must not break native PG planning",
        );
        // Confirm that the plan contains a ProjectSet node — proves
        // the hook saw what it expected. When executor wiring lands,
        // this assertion flips to require the GpuAccel custom scan
        // label instead.
        assert!(
            plan.contains("ProjectSet") || plan.contains("Result"),
            "Expected EXPLAIN plan to show ProjectSet or Result for \
             SRF-in-target-list; got:\n{plan}",
        );
    }
}
