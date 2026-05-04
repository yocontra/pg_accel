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
//! ## Status (executor wiring landed)
//!
//! The planner-side detection from B6 is now paired with a real
//! `SrfTargetListPrivData` payload + CustomPath construction. When a
//! candidate ProjectSetPath is found, `wrap_projectset_path` builds a
//! `GpuAccelSrfTargetList` Custom Scan path that:
//!   - takes the ProjectSetPath's `subpath` as `custom_paths[0]`,
//!   - serializes the SRF metadata (fn_oid, output shape, srf_arg_attno
//!     resolved against the subpath's pathtarget, srf_tlist_pos, the
//!     per-position passthrough attno table, and the constant qual args)
//!     into the path's custom_private,
//!   - undercuts the native ProjectSetPath cost by 30% so `add_path`
//!     prefers it.
//!
//! The executor arm lives in
//! `engine::ffi::custom_scan::srf_target_list` (mirrors the
//! `function_scan` pattern from F3): drives the child via
//! `ExecProcNode`, dispatches the SRF per input row, and emits one
//! output tuple per expanded row preserving non-SRF passthrough cols.
//!
//! **Limitation (escalated per ban #9)**: multi-SRF target lists like
//! `SELECT srf1(c), srf2(c) FROM t` use cartesian product semantics
//! per `nodeProjectSet.c` — a separate executor design. The single-SRF
//! case is the primary target and what the executor + hook currently
//! support; multi-SRF queries fall through to native PG ProjectSet.

use std::ffi::c_int;

use pgrx::pg_sys::{
    self, Const, CustomPath, FuncExpr, NodeTag, Path, PathTarget, PlannerInfo, ProjectSetPath,
    RelOptInfo, UpperRelationKind, Var,
};

use super::super::custom_scan::{
    self, OutputShapeDisc, SrfTargetListPrivData, append_srf_target_list_priv,
    srf_target_list_path_methods,
};
use crate::engine::cost;
use crate::engine::gucs;
use crate::engine::registry::{self, OutputShape};
use crate::engine::stats;

/// `create_upper_paths_hook` arm for `UPPERREL_FINAL`.
///
/// Inspects the input rel's `pathlist` for `T_ProjectSetPath` nodes
/// whose target tlist contains only registered SRFs we know how to
/// dispatch, then wraps each candidate in a `GpuAccelSrfTargetList`
/// Custom Scan path via `wrap_projectset_path`. The Custom Scan
/// undercuts the native ProjectSetPath's cost so `add_path` prefers
/// it; queries fall through to native PG ProjectSet whenever any
/// detection check fails.
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
    let mut injected = 0usize;
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
             input_rel pathlist idx={} with all SRFs registered",
            i,
        );

        // Try to build the CustomPath. wrap_projectset_path returns true
        // on successful injection (path added to output_rel->pathlist).
        // SAFETY: All pointers are planner-supplied and validated above.
        if unsafe { wrap_projectset_path(root, output_rel, projectset) } {
            injected += 1;
        }
    }

    if detected > 0 {
        stats::record_planner_hook_call();
    }
    pgrx::debug1!(
        "pg_accel: srf_target_list: scanned {} paths, {} ProjectSetPath, \
         {} dispatch-eligible, {} injected as CustomPath",
        n_paths,
        considered,
        detected,
        injected,
    );
}

/// Build a `CustomPath` wrapping the given `ProjectSetPath`'s subpath and
/// add it to `output_rel`'s `pathlist` via `add_path`. Returns `true` on
/// successful injection.
///
/// The CustomPath competes with the native ProjectSetPath via PG's
/// add_path cost comparison; it wins when our cost estimate undercuts
/// the native plan (the GPU dispatch is dramatically cheaper for the
/// var-output H3 ops).
///
/// # Safety
///
/// All pointers must originate from the planner. Called on the main
/// backend thread.
unsafe fn wrap_projectset_path(
    _root: *mut PlannerInfo,
    output_rel: *mut RelOptInfo,
    projectset: *mut ProjectSetPath,
) -> bool {
    // SAFETY: projectset is a valid ProjectSetPath; subpath is the input
    // source path set by create_set_projection_path.
    let subpath = unsafe { (*projectset).subpath };
    if subpath.is_null() {
        return false;
    }
    // SAFETY: projectset.path.pathtarget is the upper tlist target.
    let target = unsafe { (*projectset).path.pathtarget };
    if target.is_null() {
        return false;
    }
    // SAFETY: target.exprs is a List of upper-tlist Expr nodes.
    let exprs = unsafe { (*target).exprs };
    if exprs.is_null() {
        return false;
    }
    let n_exprs = unsafe { pg_sys::list_length(exprs) };
    if n_exprs == 0 {
        return false;
    }

    // Build a child-slot lookup table: (varno, varattno) → 1-based slot
    // position. The subpath's pathtarget.exprs is the projection that
    // will become the scan slot's TupleDesc after createplan runs.
    // SAFETY: subpath.pathtarget is set by the planner.
    let sub_target = unsafe { (*subpath).pathtarget };
    let child_lookup = unsafe { build_child_var_lookup(sub_target) };
    if child_lookup.is_empty() {
        pgrx::debug1!("pg_accel: srf_target_list: subpath has no Var pathtarget; cannot wrap");
        return false;
    }

    // Walk upper tlist exprs to extract:
    //   - srf_tlist_pos: position of the SRF FuncExpr (only one allowed
    //     in single-SRF target list; multi-SRF is escalated per ban #9)
    //   - srf_fn_oid + qual_args + srf_arg_attno: from the SRF's args
    //   - passthrough_attnos: per output position, child slot attno (or 0
    //     for the SRF position itself)
    let mut srf_tlist_pos: i32 = -1;
    let mut srf_fn_oid: pg_sys::Oid = pg_sys::InvalidOid;
    let mut srf_arg_attno: i32 = 0;
    let mut srf_qual_args: Vec<(i64, u32)> = Vec::new();
    let mut passthrough_attnos: Vec<i32> = Vec::with_capacity(n_exprs as usize);
    let mut srf_count = 0;

    for i in 0..n_exprs {
        // SAFETY: i in [0, n_exprs).
        let node = unsafe { pg_sys::list_nth(exprs, i).cast::<pg_sys::Node>() };
        if node.is_null() {
            return false;
        }
        // SAFETY: node is a valid Node.
        let tag = unsafe { (*node).type_ };
        match tag {
            NodeTag::T_FuncExpr => {
                // SAFETY: tag confirmed FuncExpr.
                let funcexpr = node.cast::<FuncExpr>();
                let returns_set = unsafe { (*funcexpr).funcretset };
                if !returns_set {
                    pgrx::debug1!(
                        "pg_accel: srf_target_list: non-SRF FuncExpr in tlist at \
                         pos {}; multi-expression mixed mode not supported",
                        i,
                    );
                    return false;
                }
                srf_count += 1;
                if srf_count > 1 {
                    // Multi-SRF target list — cartesian product semantics
                    // (see nodeProjectSet.c). Per ban #9, escalate cleanly.
                    pgrx::debug1!(
                        "pg_accel: srf_target_list: multi-SRF target list (>1 SRF) \
                         not supported — cartesian product semantics need a separate \
                         executor design; declining this ProjectSetPath"
                    );
                    return false;
                }
                srf_tlist_pos = i;
                srf_fn_oid = unsafe { (*funcexpr).funcid };

                // Extract SRF args: one Var (the per-row input), zero or
                // more Const (constant kernel parameters).
                let args = unsafe { (*funcexpr).args };
                if args.is_null() {
                    return false;
                }
                let n_args = unsafe { pg_sys::list_length(args) };
                let mut found_var = false;
                for k in 0..n_args {
                    // SAFETY: k in [0, n_args).
                    let a = unsafe { pg_sys::list_nth(args, k).cast::<pg_sys::Node>() };
                    if a.is_null() {
                        return false;
                    }
                    // SAFETY: a is a valid Node.
                    match unsafe { (*a).type_ } {
                        NodeTag::T_Var => {
                            if found_var {
                                // Two Vars in the SRF — unsupported (see B6).
                                return false;
                            }
                            found_var = true;
                            // SAFETY: tag confirmed Var.
                            let v = a.cast::<Var>();
                            let varno = unsafe { (*v).varno };
                            let varattno = i32::from(unsafe { (*v).varattno });
                            // Look up the child slot attno for this Var.
                            if let Some(child_attno) =
                                lookup_child_attno(&child_lookup, varno, varattno)
                            {
                                srf_arg_attno = child_attno;
                            } else {
                                pgrx::debug1!(
                                    "pg_accel: srf_target_list: SRF arg Var(varno={}, \
                                     varattno={}) not in subpath pathtarget; declining",
                                    varno,
                                    varattno,
                                );
                                return false;
                            }
                        }
                        NodeTag::T_Const => {
                            // SAFETY: tag confirmed Const.
                            let c = a.cast::<Const>();
                            let datum = unsafe { (*c).constvalue };
                            let typid = unsafe { (*c).consttype };
                            #[allow(clippy::cast_possible_wrap)]
                            let datum_i64 = datum.value() as i64;
                            srf_qual_args.push((datum_i64, u32::from(typid)));
                        }
                        _ => {
                            // Unsupported arg shape — bail.
                            return false;
                        }
                    }
                }
                if !found_var {
                    return false;
                }
                passthrough_attnos.push(0); // SRF position marker
            }
            NodeTag::T_Var => {
                // Pass-through Var. Map to child slot attno.
                // SAFETY: tag confirmed Var.
                let v = node.cast::<Var>();
                let varno = unsafe { (*v).varno };
                let varattno = i32::from(unsafe { (*v).varattno });
                if let Some(child_attno) = lookup_child_attno(&child_lookup, varno, varattno) {
                    passthrough_attnos.push(child_attno);
                } else {
                    pgrx::debug1!(
                        "pg_accel: srf_target_list: passthrough Var(varno={}, \
                         varattno={}) not in subpath pathtarget; declining",
                        varno,
                        varattno,
                    );
                    return false;
                }
            }
            _ => {
                // Unsupported expression shape (e.g. arithmetic, OpExpr).
                pgrx::debug1!(
                    "pg_accel: srf_target_list: unsupported expr tag {:?} at tlist \
                     pos {}; declining",
                    tag,
                    i,
                );
                return false;
            }
        }
    }

    if srf_count != 1 || srf_tlist_pos < 0 || srf_arg_attno <= 0 {
        return false;
    }

    // Resolve registry entry (already validated by srfs_all_registered, but we
    // need the OutputShape encoding for the priv block).
    registry::lazy_init();
    let entry = match registry::global_registry().lookup(srf_fn_oid) {
        Some(e) => e,
        None => return false,
    };
    let (shape_disc, shape_field_count) = match entry.output_shape {
        OutputShape::Scalar => return false, // already filtered, defensive
        OutputShape::Record { field_count } => (OutputShapeDisc::Record, field_count),
        OutputShape::VarLen => (OutputShapeDisc::VarLen, 0),
    };

    let priv_data = SrfTargetListPrivData {
        fn_oid: srf_fn_oid,
        output_shape_disc: shape_disc.to_i32(),
        output_shape_field_count: shape_field_count,
        srf_arg_attno,
        srf_tlist_pos,
        passthrough_attnos,
        qual_args: srf_qual_args,
    };

    // Build the CustomPath. Cost mirrors the ProjectSetPath we're replacing
    // but undercuts by a small margin so add_path picks ours.
    // SAFETY: ProjectSetPath is non-null; reading rows + cost.
    let projset_rows = unsafe { (*projectset).path.rows };
    let projset_startup = unsafe { (*projectset).path.startup_cost };
    let projset_total = unsafe { (*projectset).path.total_cost };
    let projset_pathkeys = unsafe { (*projectset).path.pathkeys };
    let projset_parallel_safe = unsafe { (*projectset).path.parallel_safe };
    let projset_parallel_workers = unsafe { (*projectset).path.parallel_workers };

    // SAFETY: palloc0 in CurrentMemoryContext.
    let cpath = unsafe { pg_sys::palloc0(std::mem::size_of::<CustomPath>()).cast::<CustomPath>() };
    // SAFETY: cpath is freshly palloc'd and zeroed.
    unsafe {
        (*cpath).path.type_ = NodeTag::T_CustomPath;
        (*cpath).path.pathtype = NodeTag::T_CustomScan;
        (*cpath).path.parent = output_rel;
        (*cpath).path.pathtarget = target;
        (*cpath).path.param_info = std::ptr::null_mut();
        (*cpath).path.parallel_aware = false;
        (*cpath).path.parallel_safe = projset_parallel_safe;
        (*cpath).path.parallel_workers = projset_parallel_workers;
        (*cpath).path.rows = projset_rows.max(1.0);
        (*cpath).path.startup_cost = projset_startup;
        // Undercut total_cost by the GPU savings ratio so add_path prefers us.
        // 0.5x is conservative — H3 grid_disk(k=1) is ~7x amplification per
        // input row, so even a 30% per-tuple speedup pays off.
        (*cpath).path.total_cost = (projset_total * 0.7).max(projset_startup);
        (*cpath).path.pathkeys = projset_pathkeys;

        (*cpath).flags = 0;
        // Attach the subpath as our child plan (custom_paths[0]). PG calls
        // create_plan_recurse on this when building plans, populating
        // custom_plans for plan_custom_path_srf_target_list.
        (*cpath).custom_paths = pg_sys::lappend(std::ptr::null_mut(), subpath.cast());
        (*cpath).custom_restrictinfo = std::ptr::null_mut();

        // Serialize the SrfTargetListPrivData into the path's custom_private.
        let mut priv_list: *mut pg_sys::List = std::ptr::null_mut();
        priv_list = append_srf_target_list_priv(priv_list, &priv_data);
        (*cpath).custom_private = priv_list;

        (*cpath).methods = srf_target_list_path_methods();

        pg_sys::add_path(output_rel, cpath.cast());
    }
    let _ = custom_scan::scan_path_methods; // touch import
    let _ = c_int::default(); // touch c_int import

    pgrx::debug1!(
        "pg_accel: srf_target_list: injected GpuAccelSrfTargetList CustomPath \
         (fn_oid={}, shape_disc={}, srf_arg_attno={}, srf_tlist_pos={}, n_passthrough={}, \
         n_qual_args={})",
        u32::from(srf_fn_oid),
        shape_disc.to_i32(),
        srf_arg_attno,
        srf_tlist_pos,
        priv_data.passthrough_attnos.len(),
        priv_data.qual_args.len(),
    );
    true
}

/// Build a lookup from `(varno, varattno)` to 1-based child-slot position
/// from the subpath's `pathtarget.exprs` list. The slot position is the
/// 1-based index in the pathtarget exprs list (which becomes the scan
/// slot's TupleDesc attno after createplan runs).
///
/// Returns an empty Vec if the pathtarget is null or empty.
///
/// # Safety
/// `target` must be a valid `PathTarget *` from the planner.
unsafe fn build_child_var_lookup(target: *mut PathTarget) -> Vec<(i32, i32, i32)> {
    let mut out = Vec::new();
    if target.is_null() {
        return out;
    }
    // SAFETY: target.exprs is a List of expression nodes.
    let exprs = unsafe { (*target).exprs };
    if exprs.is_null() {
        return out;
    }
    let n = unsafe { pg_sys::list_length(exprs) };
    for i in 0..n {
        // SAFETY: i in [0, n).
        let node = unsafe { pg_sys::list_nth(exprs, i).cast::<pg_sys::Node>() };
        if node.is_null() {
            continue;
        }
        // SAFETY: node is a valid Node.
        let tag = unsafe { (*node).type_ };
        if tag == NodeTag::T_Var {
            // SAFETY: tag confirmed Var.
            let v = node.cast::<Var>();
            let varno = unsafe { (*v).varno };
            let varattno = i32::from(unsafe { (*v).varattno });
            // 1-based slot attno is i+1.
            out.push((varno, varattno, i + 1));
        }
    }
    out
}

/// Look up a `(varno, varattno)` in the child slot mapping. Returns the
/// 1-based child slot attno, or None if not found.
fn lookup_child_attno(lookup: &[(i32, i32, i32)], varno: i32, varattno: i32) -> Option<i32> {
    for &(vn, va, slot_pos) in lookup {
        if vn == varno && va == varattno {
            return Some(slot_pos);
        }
    }
    None
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

/// Integration tests for the SRF-in-target-list planner hook + executor.
///
/// **Scope** (executor wiring landed): these tests verify that
///
/// 1. SRF-in-target-list queries return correct row-count expansion
///    (one input row → many output rows) when our planner injects the
///    `GpuAccelSrfTargetList` Custom Scan.
/// 2. The EXPLAIN plan shows `Custom Scan (GpuAccelSrfTargetList)`
///    instead of native `ProjectSet -> SeqScan` — confirms the planner
///    hook actually wraps + add_path picks our cost.
/// 3. Passthrough columns survive expansion: a Var passthrough column
///    repeats its value once per expanded SRF output row.
#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::{Spi, pg_test};

    use super::*;

    // -------------------------------------------------------------------
    // Pure-Rust compile-time / sanity checks. Run via `cargo test`; do
    // not require a live PG instance.
    // -------------------------------------------------------------------

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

    // -------------------------------------------------------------------
    // pg_test integration tests. Require a live pgrx PG instance and
    // the h3 extension installed. Skipped silently when h3 is missing.
    // -------------------------------------------------------------------

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

    /// `SELECT id, h3_grid_disk(cell, 1) FROM cells` with 2 input
    /// rows must expand to 2 * 7 = 14 output rows (k=1 disk = 7 cells
    /// per non-pentagon input). Verifies the multi-row expansion
    /// semantics — one input row → many output rows.
    #[pg_test]
    fn pg_test_srf_tlist_h3_grid_disk_expands() {
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
             (2 * 7); got {count}. Native PG ProjectSet and our injected \
             GpuAccelSrfTargetList both must satisfy this row-count contract.",
        );
    }

    /// `EXPLAIN SELECT id, h3_grid_disk(cell, 1) FROM h3_cells` plan
    /// must contain `Custom Scan (GpuAccelSrfTargetList)` confirming our
    /// planner hook injection won the add_path comparison. If the plan
    /// still shows native `ProjectSet -> SeqScan`, the hook either
    /// bailed or our cost estimate didn't undercut PG's.
    #[pg_test]
    fn pg_test_srf_tlist_explain_shows_custom_scan() {
        if !ensure_extension("h3") {
            return;
        }
        Spi::run("SELECT h3_get_resolution('8928308280fffff'::h3index)").expect("h3 ping");
        Spi::run(
            "CREATE TEMP TABLE _srf_tlist_explain2(id int, cell h3index); \
             INSERT INTO _srf_tlist_explain2 VALUES (1, '8928308280fffff'::h3index)",
        )
        .expect("setup ok");

        let plan = Spi::connect(|client| {
            let mut lines: Vec<String> = Vec::new();
            let table = client
                .select(
                    "EXPLAIN (FORMAT TEXT) SELECT id, h3_grid_disk(cell, 1) \
                     FROM _srf_tlist_explain2",
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
             hook + executor wiring must not break planning",
        );
        // The injected Custom Scan node uses the GpuAccelSrfTargetList
        // CustomName from SRF_TARGET_LIST_PATH_METHODS. EXPLAIN renders
        // CustomScan nodes as "Custom Scan (<CustomName>)".
        assert!(
            plan.contains("GpuAccelSrfTargetList"),
            "Expected EXPLAIN plan to contain Custom Scan (GpuAccelSrfTargetList) \
             after planner hook + executor wiring landed; got:\n{plan}",
        );
        // Defensive: we should NOT see the bare native `ProjectSet ->`
        // form — if we do, the hook didn't replace it.
        assert!(
            !plan.contains("ProjectSet"),
            "Expected EXPLAIN plan to NOT contain native 'ProjectSet' (replaced by \
             GpuAccelSrfTargetList); got:\n{plan}",
        );
    }

    /// `SELECT id, h3_grid_disk(cell, 1) FROM h3_cells` — verify that the
    /// passthrough `id` Var column is preserved per expanded row.
    /// Mirrors PG's `nodeProjectSet.c` semantics: every output row gets
    /// the source input row's passthrough Datum repeated.
    ///
    /// History (2026-05-03): originally `#[ignore]`-d with a SIGABRT report
    /// in the docstring. The SIGABRT was actually two separate bugs:
    /// (a) the SRF executor at `custom_scan/srf_target_list.rs` always emitted
    /// the right schema and per-row data — confirmed by a direct
    /// `SELECT id FROM (...)` returning correct ids; (b) `count(DISTINCT id)`
    /// over our SRF Custom Scan returned 14 instead of 2 because
    /// `pgaccel_inject_gpu_agg` in `planner_hooks/mod.rs` accepted Aggrefs
    /// with `aggdistinct != NIL` / `aggorder != NIL` / `aggfilter != NIL`
    /// and silently downgraded them to plain reductions. The same wrong
    /// result reproduced on a plain table without any SRF. The fix is the
    /// `aggdistinct` / `aggorder` / `aggfilter` reject in `gpu_agg`'s tlist
    /// scan — see commit fix(srf): SRF passthrough columns + agg DISTINCT gate.
    #[pg_test]
    fn pg_test_srf_tlist_passthrough_cols() {
        if !ensure_extension("h3") {
            return;
        }
        Spi::run("SELECT h3_get_resolution('8928308280fffff'::h3index)").expect("h3 ping");

        Spi::run(
            "CREATE TEMP TABLE _srf_tlist_pass(id int, cell h3index); \
             INSERT INTO _srf_tlist_pass VALUES \
               (42, '8928308280fffff'::h3index), \
               (99, '89283082813ffff'::h3index)",
        )
        .expect("setup ok");

        // For id=42, k=1 grid disk emits 7 output rows. Every one must
        // carry id=42 in the passthrough column.
        let n_for_42 = Spi::get_one::<i64>(
            "SELECT count(*) FROM (SELECT id, h3_grid_disk(cell, 1) AS d \
             FROM _srf_tlist_pass WHERE id = 42) sub WHERE id = 42",
        )
        .expect("count for id=42 ok")
        .expect("not null");
        assert_eq!(
            n_for_42, 7,
            "Expected 7 expanded rows with passthrough id=42 from h3_grid_disk(cell, 1); \
             got {n_for_42}. If passthrough is broken, id won't match the WHERE filter \
             on the inner query and the count drops.",
        );

        // Sanity: the passthrough is per-row, not random — every output
        // row must equal its source input row's id. Verify by selecting
        // distinct id values from the expanded result.
        let distinct_ids = Spi::get_one::<i64>(
            "SELECT count(DISTINCT id) FROM (SELECT id, h3_grid_disk(cell, 1) AS d \
             FROM _srf_tlist_pass) sub",
        )
        .expect("distinct ids ok")
        .expect("not null");
        assert_eq!(
            distinct_ids, 2,
            "Expected exactly 2 distinct passthrough ids in expanded output \
             (matching the 2 input rows); got {distinct_ids}.",
        );
    }
}
