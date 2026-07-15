//! Planner hook installation for pg_accel Custom Scan injection.
//!
//! Installs `set_rel_pathlist_hook` (scan), `set_join_pathlist_hook` (join),
//! and `create_upper_paths_hook` (aggregate) so the planner considers
//! GPU-accelerated paths for qualifying relations and aggregates.

use std::cell::Cell;

use pgrx::pg_sys::{
    self, CustomPath, JoinPathExtraData, List, NodeTag, Path, PlannerInfo, RangeTblEntry,
    RelOptInfo, RestrictInfo, UpperRelationKind,
};

use crate::engine::executor::sort::SortKeyDesc;
use crate::engine::executor::window::{WindowFunc, WindowFuncSpec};

use super::custom_scan;
use crate::engine::cost;
use crate::engine::gucs;
use crate::engine::registry;
use crate::engine::residency::ResidentProofSnapshot;
use crate::engine::stats;

mod decision;
mod gate;
mod generic_groupagg;
mod join_pathlist;
mod raster;
mod rel_pathlist;
pub mod shape;

pub(in crate::engine::ffi::planner_hooks) use decision::{
    DecisionFacts, PlannerDecision, PlannerDecisionRecorder, RejectionReason,
};

use gate::HookContext;
use join_pathlist::pgaccel_set_join_pathlist;
use rel_pathlist::pgaccel_set_rel_pathlist;

// ---------------------------------------------------------------------------
// Previous hook storage
// ---------------------------------------------------------------------------

pub(super) static mut PREV_SET_REL_PATHLIST_HOOK: pg_sys::set_rel_pathlist_hook_type = None;
pub(super) static mut PREV_SET_JOIN_PATHLIST_HOOK: pg_sys::set_join_pathlist_hook_type = None;
static mut PREV_CREATE_UPPER_PATHS_HOOK: pg_sys::create_upper_paths_hook_type = None;

thread_local! {
    static PLANNER_HOOK_SUSPEND_DEPTH: Cell<u32> = const { Cell::new(0) };
}

pub(crate) fn with_planner_hooks_suspended<R>(f: impl FnOnce() -> R) -> R {
    struct SuspensionGuard;

    impl Drop for SuspensionGuard {
        fn drop(&mut self) {
            PLANNER_HOOK_SUSPEND_DEPTH.with(|depth| {
                depth.set(depth.get().saturating_sub(1));
            });
        }
    }

    PLANNER_HOOK_SUSPEND_DEPTH.with(|depth| {
        depth.set(depth.get().saturating_add(1));
    });
    let _guard = SuspensionGuard;
    f()
}

#[must_use]
pub(in crate::engine::ffi::planner_hooks) fn planner_hooks_suspended() -> bool {
    PLANNER_HOOK_SUSPEND_DEPTH.with(|depth| depth.get() > 0)
}

// ---------------------------------------------------------------------------
// Planner-hook elapsed-time guard (Phase 0 overhead audit, 2026-05-14)
// ---------------------------------------------------------------------------

/// RAII guard that records elapsed wall-clock time when a planner hook
/// invocation returns, regardless of which `return` path it took.
///
/// Construct one near the top of every planner hook entry point (after
/// chaining the previous hook). On `Drop` it stamps the elapsed
/// microseconds into the `PLANNER_HOOK_TOTAL_US` counter via
/// [`stats::record_planner_hook_elapsed`]. The bench harness reads
/// `pg_accel_planner_overhead_us()` to assert that no-dispatch queries
/// (multi-dimension star joins, expression-only filters, native aggregates) stay
/// near zero overhead.
///
/// Costs ~50 ns (one `Instant::now`, one atomic add, one tracing event at
/// `trace` level which is filtered out by the default `notice` filter
/// per CLAUDE.md). Cheap enough to use on every hook invocation without
/// inflating the overhead it is trying to measure.
pub(in crate::engine::ffi::planner_hooks) struct HookElapsedGuard {
    hook: &'static str,
    start: std::time::Instant,
}

impl HookElapsedGuard {
    /// Begin timing one planner hook invocation. The matching elapsed
    /// sample is recorded automatically on `Drop`.
    #[must_use]
    pub(in crate::engine::ffi::planner_hooks) fn new(hook: &'static str) -> Self {
        Self {
            hook,
            start: std::time::Instant::now(),
        }
    }
}

impl Drop for HookElapsedGuard {
    fn drop(&mut self) {
        let elapsed_us = u64::try_from(self.start.elapsed().as_micros()).unwrap_or(u64::MAX);
        stats::record_planner_hook_elapsed(self.hook, elapsed_us);
    }
}

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

const SETOP_NO_GPU_KERNEL_REASON: &str = "setop_no_gpu_kernel";
const RECURSIVEUNION_NO_GPU_KERNEL_REASON: &str = "recursiveunion_no_gpu_kernel";

/// `create_upper_paths_hook` implementation.
///
/// Delegates to `pgaccel_inject_gpu_agg` for `UPPERREL_GROUP_AGG`.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
#[pgrx::pg_guard]
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
    if planner_hooks_suspended() {
        return;
    }

    // Time this invocation so the benchmark harness can detect no-dispatch
    // planner-overhead regressions through `pg_accel_planner_overhead_us()`.
    let _hook_finish = HookElapsedGuard::new("upper_paths");

    // SAFETY: root is the planner-provided pointer for this hook invocation.
    let Some(_hook_context) =
        (unsafe { HookContext::begin(root, "upper_paths", upper_stage_candidate(stage)) })
    else {
        return;
    };
    let _span = tracing::info_span!("planner.upper_paths", stage = stage).entered();

    // Dispatch by upper relation stage.
    match stage {
        pg_sys::UpperRelationKind::UPPERREL_SETOP => {
            let rows_est = rel_rows_estimate(output_rel).or_else(|| rel_rows_estimate(input_rel));
            // SAFETY: output_rel is the planner-owned upper relation supplied to this hook.
            let reason = unsafe { setop_decline_reason(output_rel) };
            stats::increment_planner_rejected(reason, rows_est.unwrap_or(0));
            stats::record_planner_fast_decline("upper_paths_setop_no_gpu_kernel");
            pgrx::debug1!(
                "pg_accel: upper_paths decline: UPPERREL_SETOP has no GPU SetOp/RecursiveUnion \
                 implementation; reason={}",
                reason
            );
            return;
        }
        // Generic resident aggregate admission performs its structural
        // preflight before device discovery, then applies GPU/residency gates.
        pg_sys::UpperRelationKind::UPPERREL_GROUP_AGG if gucs::gpu_enabled() => {
            // SAFETY: root and output_rel are the live pointers supplied for this hook call.
            let _ = unsafe { generic_groupagg::try_inject(root, output_rel) };
        }
        pg_sys::UpperRelationKind::UPPERREL_WINDOW => {
            // Segmented device kernels exist, but the legacy WindowExecState
            // materializes MinimalTuples and host result vectors. Keep both
            // full-output and reducing SQL shapes native until a downstream
            // consumer can carry an explicit resident proof. If PostgreSQL has
            // worker partial input paths, also keep the missing partial-window
            // hook visible until worker-local resident work can be injected.
            // SAFETY: input_rel is the planner-owned input relation for this hook call.
            unsafe { record_window_partial_path_no_parallel_hook(input_rel) };
            if gucs::gpu_enabled() {
                record_no_gpu_resident_pipeline_decline("upper_paths_window", input_rel);
            }
            // When `gpu_enabled` is off, pg_accel injects nothing here.
        }
        pg_sys::UpperRelationKind::UPPERREL_FINAL => {
            #[cfg(feature = "pg_test")]
            if unsafe { raster::try_force_inject(root, output_rel) } {
                return;
            }
            // Most final upper-rel hooks have no SRF work at all. Check the
            // parse flag before GPU/SPI gates so native ORDER BY/LIMIT queries
            // do not initialize the GPU runtime just to decline.
            // SAFETY: root is the live PlannerInfo pointer supplied to this hook call.
            if !unsafe { query_has_target_srfs(root) } {
                stats::record_planner_fast_decline("upper_paths_no_target_srf");
                return;
            }
            if gucs::gpu_enabled() {
                record_no_gpu_resident_pipeline_decline("upper_paths_srf_target_list", input_rel);
            }
            // When `gpu_enabled` is off, pg_accel injects nothing here.
        }
        _ => {}
    }
    let _ = extra;
}

fn rel_rows_estimate(rel: *mut RelOptInfo) -> Option<u64> {
    if rel.is_null() {
        return None;
    }
    // SAFETY: caller passed a planner-owned RelOptInfo pointer.
    Some(unsafe { (*rel).rows.max(0.0) as u64 })
}

pub(super) fn record_no_gpu_resident_pipeline_decline(context: &'static str, rel: *mut RelOptInfo) {
    let rows_est = rel_rows_estimate(rel).unwrap_or(0);
    stats::increment_planner_rejected(RejectionReason::NoGpuResidentPipeline.stats_key(), rows_est);
    stats::record_planner_fast_decline(context);
    pgrx::debug1!(
        "pg_accel: {context}: declined because pg_accel requires a proven GPU-resident pipeline"
    );
}

/// Add a pg_accel `CustomPath` only after attaching a resident-pipeline proof.
///
/// Current selected SQL admission is GPU-resident-only. Host-staged callers
/// must pass their honest nonresident proof and will be declined here; future
/// GPU-native producers pass a proof snapshot with a nonzero stage mask and
/// device columns, which then survives `PlanCustomPath` through
/// `custom_private`.
///
/// # Safety
///
/// `rel` and `cpath` must be valid planner-owned pointers in the current
/// PostgreSQL planner memory context.
#[must_use]
pub(super) unsafe fn add_gpu_path_with_resident_proof(
    context: &'static str,
    rel: *mut RelOptInfo,
    cpath: *mut CustomPath,
    proof: ResidentProofSnapshot,
) -> bool {
    if cpath.is_null() {
        return false;
    }
    if !proof.gpu_resident_pipeline() {
        record_no_gpu_resident_pipeline_decline(context, rel);
        return false;
    }
    // SAFETY: cpath was checked non-null and both pointers are planner-owned for
    // this invocation; the appended List and path remain in that planner context.
    unsafe {
        (*cpath).custom_private =
            custom_scan::append_resident_proof_snapshot((*cpath).custom_private, proof);
        pg_sys::add_path(rel, cpath.cast());
    }
    true
}

unsafe fn record_window_partial_path_no_parallel_hook(input_rel: *mut RelOptInfo) {
    if input_rel.is_null() {
        return;
    }

    // SAFETY: input_rel is a planner-owned RelOptInfo pointer.
    let input_ref = unsafe { &*input_rel };
    if input_ref.partial_pathlist.is_null()
        // SAFETY: a non-null partial_pathlist is a planner-owned PostgreSQL List.
        || unsafe { pg_sys::list_length(input_ref.partial_pathlist) } == 0
    {
        return;
    }

    #[allow(clippy::cast_sign_loss)]
    let rows = input_ref.rows.max(0.0) as u64;
    stats::increment_planner_rejected(
        RejectionReason::WindowPartialPathNoParallelHook.stats_key(),
        rows,
    );
    pgrx::debug1!(
        "pg_accel window: partial input paths exist, but no worker-local partial window hook is \
         implemented yet"
    );
}

/// Cheap parse-level test for target-list SRF work.
///
/// Used by the `UPPERREL_FINAL` hook arm to avoid expensive availability
/// checks on ordinary queries. Does not touch GPU state or the adapter
/// registry.
///
/// # Safety
///
/// `root` must be null or a valid planner-provided `PlannerInfo *`.
unsafe fn query_has_target_srfs(root: *mut PlannerInfo) -> bool {
    if root.is_null() {
        return false;
    }
    // SAFETY: caller provides a valid PlannerInfo pointer.
    let parse = unsafe { (*root).parse };
    if parse.is_null() {
        return false;
    }
    // SAFETY: parse is a valid Query pointer.
    unsafe { (*parse).hasTargetSRFs }
}

#[must_use]
const fn setop_reason_for_recursive_union(has_recursive_union: bool) -> &'static str {
    if has_recursive_union {
        RECURSIVEUNION_NO_GPU_KERNEL_REASON
    } else {
        SETOP_NO_GPU_KERNEL_REASON
    }
}

unsafe fn setop_decline_reason(output_rel: *mut RelOptInfo) -> &'static str {
    // SAFETY: the caller supplies the current planner-owned upper relation.
    let has_recursive_union =
        unsafe { rel_pathlist_contains_node_tag(output_rel, NodeTag::T_RecursiveUnionPath) };
    setop_reason_for_recursive_union(has_recursive_union)
}

unsafe fn rel_pathlist_contains_node_tag(rel: *mut RelOptInfo, tag: NodeTag) -> bool {
    if rel.is_null() {
        return false;
    }
    // SAFETY: rel is a planner-owned RelOptInfo pointer.
    let pathlist = unsafe { (*rel).pathlist };
    if pathlist.is_null() {
        return false;
    }
    // SAFETY: pathlist is a planner-owned List.
    let len = unsafe { pg_sys::list_length(pathlist) };
    for i in 0..len {
        // SAFETY: i is in [0, len).
        let path = unsafe { pg_sys::list_nth(pathlist, i).cast::<Path>() };
        if path.is_null() {
            continue;
        }
        // SAFETY: path is a valid planner Path node.
        if unsafe { (*path.cast::<pg_sys::Node>()).type_ } == tag {
            return true;
        }
    }
    false
}

fn upper_stage_candidate(stage: UpperRelationKind::Type) -> &'static str {
    match stage {
        pg_sys::UpperRelationKind::UPPERREL_GROUP_AGG => "GpuAgg",
        pg_sys::UpperRelationKind::UPPERREL_WINDOW => "GpuWindow",
        pg_sys::UpperRelationKind::UPPERREL_FINAL => "GpuSrfTargetList",
        _ => "UpperPath",
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

/// Unwrap a `Var` node from possible `RelabelType` wrappers.
///
/// # Safety
///
/// `node` must be a valid PG `Node` pointer (or null).
pub(super) unsafe fn unwrap_var(mut node: *mut pg_sys::Node) -> *mut pg_sys::Var {
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
            // SAFETY: the checked NodeTag proves relabel's layout and its arg is
            // another planner-owned node in the same expression tree.
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
            // SAFETY: the checked NodeTag proves funcexpr's layout and readability.
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
            // SAFETY: the checked NodeTag proves opexpr's layout and readability.
            let mut oid = unsafe { (*opexpr).opfuncid };
            if oid == pg_sys::InvalidOid {
                // SAFETY: opexpr is a writable planner-owned OpExpr and this hook
                // runs on the backend thread where PostgreSQL syscaches are valid.
                unsafe { pg_sys::set_opfuncid(opexpr) };
                // SAFETY: set_opfuncid initialized this field on the same OpExpr.
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod unit_tests {
    use super::*;
}

#[cfg(feature = "pg_test")]
mod tests;
