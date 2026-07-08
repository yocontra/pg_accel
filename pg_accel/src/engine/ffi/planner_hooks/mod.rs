//! Planner hook installation for pg_accel Custom Scan injection.
//!
//! Installs `set_rel_pathlist_hook` (scan), `set_join_pathlist_hook` (join),
//! and `create_upper_paths_hook` (aggregate) so the planner considers
//! GPU-accelerated paths for qualifying relations and aggregates.

use std::cell::Cell;

use pgrx::pg_sys::{
    self, CustomPath, JoinPathExtraData, List, NodeTag, Path, PlannerInfo, RangeTblEntry,
    RelOptInfo, RestrictInfo, UpperRelationKind, lappend,
};

use crate::engine::executor::agg::{
    AggOp, GroupKeyInfo, H3_LATLNG_GROUP_KEY_TYPE, H3_PARENT_GROUP_KEY_TYPE,
    is_h3_synthetic_group_key,
};
use crate::engine::executor::sort::SortKeyDesc;
use crate::engine::executor::window::{WindowFunc, WindowFuncSpec};

use super::custom_scan;
use crate::engine::cost;
use crate::engine::gucs;
use crate::engine::olap_cache;
use crate::engine::registry;
use crate::engine::residency::{
    MaterializationBoundary, ResidentOperatorClass, ResidentOperatorStage, ResidentPipelineProof,
    ResidentProofSnapshot,
};
use crate::engine::stats;
use crate::gpu::PgaccelKeyType;

mod agg_common;
mod decision;
mod gate;
mod hashjoin;
mod join_pathlist;
mod rel_pathlist;
mod resident_groupagg;
mod resident_groupagg_path;
mod resident_h3_groupagg;
mod resident_star_groupagg;
mod ssbm_q1;

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
/// (SSBM star joins, expression-only filters, native aggregates) stay
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

    // Phase 0 audit: time this invocation (TODO.md 2026-05-14). Bench harness
    // reads `pg_accel_planner_overhead_us()` to detect no-dispatch regressions.
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
        // GPU-resident-only admission. When `gpu_enabled` is off, this guard
        // is false and the query falls through to the `_` arm (pg_accel injects
        // nothing and PostgreSQL runs its native aggregate/scan plan).
        pg_sys::UpperRelationKind::UPPERREL_GROUP_AGG
            if gucs::gpu_enabled() && gpu_resident_pipeline_required() =>
        {
            if unsafe { ssbm_q1::try_inject_revenue_agg(root, input_rel, output_rel) }
                || unsafe { resident_h3_groupagg::try_inject(root, input_rel, output_rel) }
                || unsafe { resident_star_groupagg::try_inject(root, input_rel, output_rel) }
                || unsafe { resident_groupagg::try_inject(root, input_rel, output_rel) }
                || unsafe { ssbm_q1::try_inject_q4_grouped_profit_agg(root, input_rel, output_rel) }
                || unsafe {
                    ssbm_q1::try_inject_q2_grouped_revenue_agg(root, input_rel, output_rel)
                }
                || unsafe {
                    ssbm_q1::try_inject_q3_grouped_revenue_agg(root, input_rel, output_rel)
                }
                || unsafe { try_inject_resident_hashjoin_count(root, input_rel, output_rel) }
            {
                return;
            }
            unsafe { record_partial_agg_no_gpu_producing_child(input_rel) };
            unsafe { record_preagg_no_gpu_resident_pipeline(root, input_rel) };
            unsafe { record_fused_filter_count_decline(root, input_rel, output_rel) };
            record_no_gpu_resident_pipeline_decline("upper_paths_group_agg", input_rel);
        }
        pg_sys::UpperRelationKind::UPPERREL_WINDOW => {
            // The active window path is leader-side only. If PostgreSQL has
            // worker partial input paths, keep the missing partial-window hook
            // visible until the planner can inject worker-local window work.
            unsafe { record_window_partial_path_no_parallel_hook(input_rel) };
            if gucs::gpu_enabled() && gpu_resident_pipeline_required() {
                record_no_gpu_resident_pipeline_decline("upper_paths_window", input_rel);
            }
            // When `gpu_enabled` is off, pg_accel injects nothing here.
        }
        pg_sys::UpperRelationKind::UPPERREL_FINAL => {
            // Most final upper-rel hooks have no SRF work at all. Check the
            // parse flag before GPU/SPI gates so native ORDER BY/LIMIT queries
            // do not initialize the GPU runtime just to decline.
            if !unsafe { query_has_target_srfs(root) } {
                stats::record_planner_fast_decline("upper_paths_no_target_srf");
                return;
            }
            if gucs::gpu_enabled() && gpu_resident_pipeline_required() {
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

#[must_use]
pub(super) fn gpu_resident_pipeline_required() -> bool {
    true
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
    if gpu_resident_pipeline_required() && !proof.gpu_resident_pipeline() {
        record_no_gpu_resident_pipeline_decline(context, rel);
        return false;
    }
    unsafe {
        (*cpath).custom_private =
            custom_scan::append_resident_proof_snapshot((*cpath).custom_private, proof);
        pg_sys::add_path(rel, cpath.cast());
    }
    true
}

unsafe fn record_partial_agg_no_gpu_producing_child(input_rel: *mut RelOptInfo) {
    if input_rel.is_null() || !cost::gpu_is_usable() {
        return;
    }
    // SAFETY: input_rel is a planner-owned RelOptInfo pointer.
    let input_ref = unsafe { &*input_rel };
    if input_ref.partial_pathlist.is_null()
        || unsafe { pg_sys::list_length(input_ref.partial_pathlist) } == 0
    {
        return;
    }
    if !unsafe { find_cheapest_gpu_producing_path(input_ref.partial_pathlist) }.is_null() {
        return;
    }

    stats::increment_planner_rejected(
        RejectionReason::PartialAggNoGpuProducingChild.stats_key(),
        input_ref.rows.max(0.0) as u64,
    );
    pgrx::debug1!("pg_accel: gpu_agg partial skipped: no GPU-producing partial child path");
}

unsafe fn record_preagg_no_gpu_resident_pipeline(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
) {
    if root.is_null() || input_rel.is_null() || !cost::gpu_is_usable() {
        return;
    }
    // SAFETY: root is a planner-owned PlannerInfo pointer.
    let parse = unsafe { (*root).parse };
    if parse.is_null() {
        return;
    }
    // SAFETY: parse is a planner-owned Query pointer.
    let query = unsafe { &*parse };
    if query.groupClause.is_null() || unsafe { pg_sys::list_length(query.groupClause) } == 0 {
        return;
    }
    if !unsafe { rel_pathlist_contains_any_join_path(input_rel) } {
        return;
    }

    let rows_est = rel_rows_estimate(input_rel).unwrap_or(0);
    stats::increment_planner_rejected(
        RejectionReason::PreAggNoGpuResidentPipeline.stats_key(),
        rows_est,
    );
    pgrx::debug1!(
        "pg_accel: preagg skipped: grouped join input has no GPU-resident PreAgg pipeline yet"
    );
}

unsafe fn record_window_partial_path_no_parallel_hook(input_rel: *mut RelOptInfo) {
    if input_rel.is_null() {
        return;
    }

    // SAFETY: input_rel is a planner-owned RelOptInfo pointer.
    let input_ref = unsafe { &*input_rel };
    if input_ref.partial_pathlist.is_null()
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

const PARALLEL_FUSED_COUNT_DISABLED: &str = "parallel_fused_count_disabled";
const PARALLEL_FUSED_COUNT_UNSTABLE: &str = "parallel_fused_count_unstable";

/// Record the honest planner-rejection reason for a parallel fused `COUNT(*)`
/// candidate over one base relation.
///
/// The host-staged fused-count injection lane has been removed. This preserves
/// the observable rejection reason — `parallel_fused_count_disabled` when the
/// opt-in `pg_accel.parallel_fused_count` GUC is off, or
/// `parallel_fused_count_unstable` when it is on but the crash-gated parallel
/// lane stays closed pending a worker-stability proof. It never injects a path.
///
/// # Safety
///
/// Called from the planner hook on the main backend thread. Pointers must be
/// valid planner-provided objects.
#[allow(clippy::cast_ptr_alignment)]
unsafe fn record_fused_filter_count_decline(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    grouped_rel: *mut RelOptInfo,
) {
    if root.is_null() || input_rel.is_null() || grouped_rel.is_null() {
        return;
    }
    // SAFETY: root is a planner-owned PlannerInfo pointer.
    let root_ref = unsafe { &*root };
    // SAFETY: grouped_rel is a planner-owned RelOptInfo pointer.
    let grouped_ref = unsafe { &*grouped_rel };
    let parse = root_ref.parse;
    if parse.is_null() {
        return;
    }
    // SAFETY: parse is a planner-owned Query pointer.
    let query = unsafe { &*parse };
    if (!query.groupClause.is_null() && unsafe { pg_sys::list_length(query.groupClause) } > 0)
        || !query.havingQual.is_null()
    {
        return;
    }
    if !grouped_ref.consider_parallel {
        return;
    }
    // SAFETY: input_rel is a planner-owned RelOptInfo pointer.
    let input_ref = unsafe { &*input_rel };
    if input_ref.reloptkind != pg_sys::RelOptKind::RELOPT_BASEREL || input_ref.relid == 0 {
        return;
    }
    if input_ref.partial_pathlist.is_null()
        || unsafe { pg_sys::list_length(input_ref.partial_pathlist) } == 0
    {
        return;
    }
    let rte = if root_ref.simple_rte_array.is_null()
        || input_ref.relid as i32 >= root_ref.simple_rel_array_size
    {
        std::ptr::null_mut()
    } else {
        // SAFETY: relid is in-bounds of simple_rte_array per the check above.
        unsafe { *root_ref.simple_rte_array.offset(input_ref.relid as isize) }
    };
    if rte.is_null()
        || unsafe { (*rte).rtekind } != pg_sys::RTEKind::RTE_RELATION
        || unsafe { (*rte).relid } == pg_sys::InvalidOid
    {
        return;
    }
    let rows = input_ref.rows.max(0.0) as usize;
    // SAFETY: root/grouped relids are planner-owned; fetch_upper_rel is a
    // read-only planner lookup.
    let partially_grouped_rel = unsafe {
        pg_sys::fetch_upper_rel(
            root,
            pg_sys::UpperRelationKind::UPPERREL_PARTIAL_GROUP_AGG,
            grouped_ref.relids,
        )
    };
    if partially_grouped_rel.is_null() {
        return;
    }
    // SAFETY: fetch_upper_rel returned a planner-owned RelOptInfo.
    let partial_ref = unsafe { &*partially_grouped_rel };
    if !partial_ref.consider_parallel || partial_ref.reltarget.is_null() {
        return;
    }
    // SAFETY: reltarget is a valid PathTarget owned by the planner.
    let partial_exprs = unsafe { (*partial_ref.reltarget).exprs };
    if partial_exprs.is_null() || unsafe { pg_sys::list_length(partial_exprs) } != 1 {
        return;
    }
    // SAFETY: partial_exprs has exactly one element per the check above.
    let expr = unsafe { pg_sys::list_nth(partial_exprs, 0).cast::<pg_sys::Expr>() };
    if expr.is_null() || unsafe { (*expr.cast::<pg_sys::Node>()).type_ } != NodeTag::T_Aggref {
        return;
    }
    let aggref = expr.cast::<pg_sys::Aggref>();
    // SAFETY: expr is a T_Aggref node per the tag check above.
    let aggref_ref = unsafe { &*aggref };
    // SAFETY: aggref is a valid Aggref node.
    let Some((AggOp::Count, _class)) = (unsafe { agg_common::classify_aggref(aggref) }) else {
        return;
    };
    if !aggref_ref.aggstar
        || !aggref_ref.aggdistinct.is_null()
        || !aggref_ref.aggorder.is_null()
        || !aggref_ref.aggfilter.is_null()
    {
        return;
    }
    let rows_u64 = u64::try_from(rows).unwrap_or(u64::MAX);
    if gucs::parallel_fused_count_enabled() {
        stats::increment_planner_rejected(PARALLEL_FUSED_COUNT_UNSTABLE, rows_u64);
        pgrx::debug1!(
            "pg_accel fused-count: rejected because the opt-in parallel fused-count path is \
             crash-gated pending worker-stability proof"
        );
    } else {
        stats::increment_planner_rejected(PARALLEL_FUSED_COUNT_DISABLED, rows_u64);
        pgrx::debug1!(
            "pg_accel fused-count: rejected because pg_accel.parallel_fused_count is off"
        );
    }
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
    let has_recursive_union =
        unsafe { rel_pathlist_contains_node_tag(output_rel, NodeTag::T_RecursiveUnionPath) };
    setop_reason_for_recursive_union(has_recursive_union)
}

unsafe fn rel_pathlist_contains_any_join_path(rel: *mut RelOptInfo) -> bool {
    (unsafe { rel_pathlist_contains_node_tag(rel, NodeTag::T_HashPath) })
        || unsafe { rel_pathlist_contains_node_tag(rel, NodeTag::T_MergePath) }
        || unsafe { rel_pathlist_contains_node_tag(rel, NodeTag::T_NestPath) }
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

struct HashJoinCountCandidate {
    equi: EquiJoinKey,
    inner_rows: f64,
    output_rows: f64,
}

const HASHJOIN_COUNT_PATH_UNWRAP_DEPTH_LIMIT: u8 = 12;

/// Inject a childless resident count-only hashjoin upper path.
///
/// This path consumes preloaded device-resident outer/inner key columns and
/// emits only the final `int8` count row. It is intentionally separate from the
/// legacy count-only hashjoin path, which still pulls tuples through child
/// `ExecProcNode` plans and is therefore not admissible under GPU-resident-only
/// planner policy.
///
/// # Safety
///
/// Planner-owned pointers must be valid.
unsafe fn try_inject_resident_hashjoin_count(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
) -> bool {
    if !unsafe { query_is_single_count_star(root) } {
        return false;
    }

    let Some(candidate) = (unsafe { find_hashjoin_count_candidate(input_rel) }) else {
        stats::increment_planner_rejected(
            "resident_hashjoin_count_no_hash_path",
            rel_rows_estimate(input_rel).unwrap_or(0),
        );
        return false;
    };

    let limits = cost::device_limits();

    let Some(key_type) = resident_hashjoin_key_type(candidate.equi.key_type) else {
        let build_rows = candidate.inner_rows.max(0.0) as u64;
        stats::increment_planner_rejected("resident_hashjoin_key_type_unsupported", build_rows);
        stats::record_planner_fast_decline("upper_paths_resident_hashjoin_key_type_unsupported");
        return true;
    };

    let query = unsafe { (*root).parse };
    if query.is_null() {
        return false;
    }
    let Some(outer_rel_oid) =
        (unsafe { relation_oid_from_rtable((*query).rtable, candidate.equi.outer_varno) })
    else {
        let build_rows = candidate.inner_rows.max(0.0) as u64;
        stats::increment_planner_rejected(
            "resident_hashjoin_outer_relation_unresolved",
            build_rows,
        );
        return true;
    };
    let Some(inner_rel_oid) =
        (unsafe { relation_oid_from_rtable((*query).rtable, candidate.equi.inner_varno) })
    else {
        let build_rows = candidate.inner_rows.max(0.0) as u64;
        stats::increment_planner_rejected(
            "resident_hashjoin_inner_relation_unresolved",
            build_rows,
        );
        return true;
    };

    let cache_shape = if olap_cache::resident_hashjoin_count_cache_loaded_for(
        outer_rel_oid,
        candidate.equi.outer_attno,
        inner_rel_oid,
        candidate.equi.inner_attno,
        key_type,
    ) || olap_cache::resident_hashjoin_count_cache_loaded_for(
        inner_rel_oid,
        candidate.equi.inner_attno,
        outer_rel_oid,
        candidate.equi.outer_attno,
        key_type,
    ) {
        olap_cache::resident_hashjoin_count_cache_shape()
    } else {
        None
    };
    let Some(cache_shape) = cache_shape else {
        let build_rows = candidate.inner_rows.max(0.0) as u64;
        stats::increment_planner_rejected("resident_hashjoin_count_cache_miss", build_rows);
        stats::record_planner_fast_decline("upper_paths_resident_hashjoin_count_cache_miss");
        return true;
    };

    let build_rows = cache_shape.inner_rows;
    if build_rows < limits.hashjoin_min_build_rows {
        stats::increment_planner_rejected("hashjoin_build_below_break_even", build_rows as u64);
        stats::record_planner_fast_decline("upper_paths_resident_hashjoin_build_below_min");
        return true;
    }
    if build_rows > limits.gpu_hash_join_build_max_rows {
        stats::increment_planner_rejected("hashjoin_build_side_too_large", build_rows as u64);
        stats::record_planner_fast_decline("upper_paths_resident_hashjoin_build_too_large");
        return true;
    }

    let build_cost =
        cache_shape.inner_rows as f64 * hashjoin::build_cost_per_inner_row(false, limits);
    let probe_cost =
        cache_shape.outer_rows as f64 * hashjoin::probe_cost_per_outer_row(false, limits);
    let startup_cost = cost::GPU_LAUNCH_OVERHEAD;
    let total_cost = (cost::GPU_LAUNCH_OVERHEAD + build_cost + probe_cost).max(startup_cost);

    let cpath = unsafe { pg_sys::palloc0(std::mem::size_of::<CustomPath>()).cast::<CustomPath>() };
    unsafe {
        (*cpath).path.type_ = NodeTag::T_CustomPath;
        (*cpath).path.pathtype = NodeTag::T_CustomScan;
        (*cpath).path.parent = output_rel;
        (*cpath).path.pathtarget = (*output_rel).reltarget;
        (*cpath).path.param_info = std::ptr::null_mut();
        (*cpath).path.parallel_aware = false;
        (*cpath).path.parallel_safe = false;
        (*cpath).path.parallel_workers = 0;
        (*cpath).path.rows = 1.0;
        (*cpath).path.startup_cost = startup_cost;
        (*cpath).path.total_cost = total_cost.max(1.0);
        (*cpath).path.pathkeys = std::ptr::null_mut();
        (*cpath).flags = 0;
        (*cpath).custom_paths = std::ptr::null_mut();
        (*cpath).custom_restrictinfo = std::ptr::null_mut();
        (*cpath).methods = custom_scan::join_path_methods();

        let mut priv_list: *mut List = std::ptr::null_mut();
        priv_list = lappend(priv_list, pg_sys::makeInteger(0).cast());
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(cache_shape.outer_attno).cast(),
        );
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(registry::AccelStrategy::GpuHashJoin as i32).cast(),
        );
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(cache_shape.inner_attno).cast(),
        );
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(candidate.equi.key_type).cast(),
        );
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(candidate.equi.outer_varno).cast(),
        );
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(candidate.equi.inner_varno).cast(),
        );
        priv_list = lappend(priv_list, pg_sys::makeInteger(1).cast());
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(custom_scan::HASH_JOIN_RESIDENT_COUNT_SENTINEL).cast(),
        );
        priv_list = lappend(priv_list, pg_sys::makeInteger(1).cast());
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(u32::from(cache_shape.outer_rel_oid) as i32).cast(),
        );
        priv_list = lappend(
            priv_list,
            pg_sys::makeInteger(u32::from(cache_shape.inner_rel_oid) as i32).cast(),
        );
        (*cpath).custom_private = priv_list;
    }

    let proof = ResidentPipelineProof::device_resident(
        ResidentOperatorClass::ResidentJoin,
        vec![
            ResidentOperatorStage::Scan,
            ResidentOperatorStage::Join,
            ResidentOperatorStage::FinalMaterialization,
        ],
        MaterializationBoundary::FinalOutput,
        2,
        false,
        false,
    )
    .snapshot();

    let added = unsafe {
        add_gpu_path_with_resident_proof(
            "upper_paths_resident_hashjoin_count",
            output_rel,
            cpath,
            proof,
        )
    };
    if added {
        pgrx::debug1!(
            "pg_accel: injected resident count-only GpuHashJoin path, outer_rows={}, inner_rows={}, join_rows={:.0}",
            cache_shape.outer_rows,
            cache_shape.inner_rows,
            candidate.output_rows,
        );
    }
    added
}

#[must_use]
const fn resident_hashjoin_key_type(key_type: i32) -> Option<PgaccelKeyType> {
    match key_type {
        0 => Some(PgaccelKeyType::Int32),
        1 => Some(PgaccelKeyType::Int64),
        _ => None,
    }
}

unsafe fn query_is_single_count_star(root: *mut PlannerInfo) -> bool {
    if root.is_null() {
        return false;
    }
    let query = unsafe { (*root).parse };
    if query.is_null() {
        return false;
    }
    let tlist = unsafe { (*query).targetList };
    if tlist.is_null() {
        return false;
    }
    let len = unsafe { pg_sys::list_length(tlist) };
    let mut seen = false;
    for i in 0..len {
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() || unsafe { (*tle).resjunk } {
            continue;
        }
        if seen {
            return false;
        }
        seen = true;
        let expr = unsafe { (*tle).expr };
        if expr.is_null() || unsafe { (*expr.cast::<pg_sys::Node>()).type_ } != NodeTag::T_Aggref {
            return false;
        }
        let agg = expr.cast::<pg_sys::Aggref>();
        let agg_ref = unsafe { &*agg };
        if !agg_ref.aggstar
            || u32::from(agg_ref.aggtype) != u32::from(pg_sys::INT8OID)
            || !agg_ref.aggdistinct.is_null()
            || !agg_ref.aggorder.is_null()
            || !agg_ref.aggfilter.is_null()
        {
            return false;
        }
        let name = unsafe { pg_sys::get_func_name(agg_ref.aggfnoid) };
        if name.is_null() {
            return false;
        }
        let name = unsafe { std::ffi::CStr::from_ptr(name) };
        if name.to_bytes() != b"count" {
            return false;
        }
    }
    seen
}

unsafe fn relation_oid_from_rtable(rtable: *mut pg_sys::List, varno: i32) -> Option<pg_sys::Oid> {
    if rtable.is_null() || varno <= 0 {
        return None;
    }
    let index = varno.checked_sub(1)?;
    if index >= unsafe { pg_sys::list_length(rtable) } {
        return None;
    }
    let rte = unsafe { pg_sys::list_nth(rtable, index).cast::<RangeTblEntry>() };
    if rte.is_null()
        || unsafe { (*rte).rtekind } != pg_sys::RTEKind::RTE_RELATION
        || unsafe { (*rte).relid } == pg_sys::InvalidOid
    {
        return None;
    }
    Some(unsafe { (*rte).relid })
}

/// Find the native HashPath shape that the count-only hash-join path can
/// replace, unwrapping the aggregate/gather/project wrappers PG builds for
/// parallel `COUNT(*)` joins.
///
/// # Safety
///
/// `input_rel` must be a valid upper-path input relation.
unsafe fn find_hashjoin_count_candidate(
    input_rel: *mut RelOptInfo,
) -> Option<HashJoinCountCandidate> {
    if input_rel.is_null() {
        return None;
    }
    let input_ref = unsafe { &*input_rel };
    let pathlist = input_ref.pathlist;
    if pathlist.is_null() {
        return None;
    }

    let n = unsafe { pg_sys::list_length(pathlist) };
    let mut best: Option<HashJoinCountCandidate> = None;
    let mut best_cost = f64::INFINITY;

    for i in 0..n {
        let path = unsafe { pg_sys::list_nth(pathlist, i).cast::<Path>() };
        if path.is_null() {
            continue;
        }

        let Some(candidate) = (unsafe { find_hashjoin_count_candidate_in_path(path, 0) }) else {
            continue;
        };

        let path_cost = unsafe { (*path).total_cost };
        if path_cost < best_cost {
            best_cost = path_cost;
            best = Some(candidate);
        }
    }

    best
}

/// Recursively unwrap transparent upper/planner paths until a HashPath is
/// found. PG 18 commonly presents count joins as
/// `AggPath -> GatherPath -> AggPath -> HashPath`; treating that as the same
/// semantic candidate lets the resident GPU path replace the whole stack.
///
/// # Safety
///
/// `path`, when non-null, must point to a planner-owned Path node.
unsafe fn find_hashjoin_count_candidate_in_path(
    path: *mut Path,
    depth: u8,
) -> Option<HashJoinCountCandidate> {
    if path.is_null() || depth > HASHJOIN_COUNT_PATH_UNWRAP_DEPTH_LIMIT {
        return None;
    }

    let tag = unsafe { (*path.cast::<pg_sys::Node>()).type_ };
    match tag {
        NodeTag::T_HashPath => unsafe { hashjoin_count_candidate_from_hash_path(path) },
        NodeTag::T_AggPath => unsafe {
            let agg = path.cast::<pg_sys::AggPath>();
            find_hashjoin_count_candidate_in_path((*agg).subpath, depth + 1)
        },
        NodeTag::T_GatherPath => unsafe {
            let gather = path.cast::<pg_sys::GatherPath>();
            find_hashjoin_count_candidate_in_path((*gather).subpath, depth + 1)
        },
        NodeTag::T_GatherMergePath => unsafe {
            let gather = path.cast::<pg_sys::GatherMergePath>();
            find_hashjoin_count_candidate_in_path((*gather).subpath, depth + 1)
        },
        NodeTag::T_ProjectionPath => unsafe {
            let projection = path.cast::<pg_sys::ProjectionPath>();
            find_hashjoin_count_candidate_in_path((*projection).subpath, depth + 1)
        },
        NodeTag::T_ProjectSetPath => unsafe {
            let project_set = path.cast::<pg_sys::ProjectSetPath>();
            find_hashjoin_count_candidate_in_path((*project_set).subpath, depth + 1)
        },
        NodeTag::T_MaterialPath => unsafe {
            let material = path.cast::<pg_sys::MaterialPath>();
            find_hashjoin_count_candidate_in_path((*material).subpath, depth + 1)
        },
        NodeTag::T_MemoizePath => unsafe {
            let memoize = path.cast::<pg_sys::MemoizePath>();
            find_hashjoin_count_candidate_in_path((*memoize).subpath, depth + 1)
        },
        NodeTag::T_UniquePath => unsafe {
            let unique = path.cast::<pg_sys::UniquePath>();
            find_hashjoin_count_candidate_in_path((*unique).subpath, depth + 1)
        },
        NodeTag::T_SortPath => unsafe {
            let sort = path.cast::<pg_sys::SortPath>();
            find_hashjoin_count_candidate_in_path((*sort).subpath, depth + 1)
        },
        NodeTag::T_IncrementalSortPath => unsafe {
            let sort = path.cast::<pg_sys::IncrementalSortPath>();
            find_hashjoin_count_candidate_in_path((*sort).spath.subpath, depth + 1)
        },
        NodeTag::T_GroupPath => unsafe {
            let group = path.cast::<pg_sys::GroupPath>();
            find_hashjoin_count_candidate_in_path((*group).subpath, depth + 1)
        },
        NodeTag::T_UpperUniquePath => unsafe {
            let unique = path.cast::<pg_sys::UpperUniquePath>();
            find_hashjoin_count_candidate_in_path((*unique).subpath, depth + 1)
        },
        NodeTag::T_WindowAggPath => unsafe {
            let window = path.cast::<pg_sys::WindowAggPath>();
            find_hashjoin_count_candidate_in_path((*window).subpath, depth + 1)
        },
        _ => None,
    }
}

/// Build the resident/legacy count candidate from a concrete inner HashPath.
///
/// # Safety
///
/// `path` must point to a valid PG HashPath node.
unsafe fn hashjoin_count_candidate_from_hash_path(
    path: *mut Path,
) -> Option<HashJoinCountCandidate> {
    if path.is_null() {
        return None;
    }
    let hash_path = path.cast::<pg_sys::HashPath>();
    let jp = unsafe { &(*hash_path).jpath };
    if jp.jointype != pg_sys::JoinType::JOIN_INNER {
        return None;
    }
    let outer_path = jp.outerjoinpath;
    let inner_path = jp.innerjoinpath;
    if outer_path.is_null() || inner_path.is_null() {
        return None;
    }
    let equi =
        unsafe { find_equi_join_from_restrictinfo(jp.joinrestrictinfo, outer_path, inner_path) }?;
    if !hashjoin::selected_key_type_supported(equi.key_type) {
        return None;
    }

    let inner_rows = unsafe {
        (*hash_path)
            .inner_rows_total
            .max((*inner_path).rows)
            .max(0.0)
    };

    let _ = (outer_path, inner_path);
    Some(HashJoinCountCandidate {
        equi,
        inner_rows,
        output_rows: unsafe { (*path).rows.max(0.0) },
    })
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

/// Find the cheapest path that produces data through a pg_accel GPU CustomPath.
///
/// `GpuAgg` may consume a child only when that child has already moved the
/// expensive scan/join/filter work into a GPU pipeline. Otherwise the aggregate
/// path is just a wrapper around PostgreSQL tuple production and is slower than
/// core PG parallel aggregation.
///
/// # Safety
///
/// `pathlist` must be a valid PG `List` of `Path*` or null.
pub(super) unsafe fn find_cheapest_gpu_producing_path(pathlist: *mut List) -> *mut Path {
    if pathlist.is_null() {
        return std::ptr::null_mut();
    }

    // SAFETY: pathlist is a valid List from the planner.
    let len = unsafe { pg_sys::list_length(pathlist) };
    let mut best: *mut Path = std::ptr::null_mut();
    for i in 0..len {
        // SAFETY: i is in bounds for pathlist.
        let path = unsafe { pg_sys::list_nth(pathlist, i).cast::<Path>() };
        if path.is_null() || !gpu_agg_child_is_gpu_producing(path) {
            continue;
        }
        if best.is_null() || unsafe { (*path).total_cost < (*best).total_cost } {
            best = path;
        }
    }
    best
}

/// True when `path` is a pg_accel child that has already moved scan/join work
/// into the GPU path. `GpuAgg` is allowed to wrap these children, but must not
/// wrap plain PostgreSQL CPU filter/join/scan paths.
#[must_use]
pub(super) fn gpu_agg_child_is_gpu_producing(path: *mut Path) -> bool {
    custom_path_uses_methods(path, custom_scan::scan_path_methods())
        || custom_path_uses_methods(path, custom_scan::join_path_methods())
}

#[must_use]
fn custom_path_uses_methods(path: *mut Path, methods: *const pg_sys::CustomPathMethods) -> bool {
    if path.is_null() {
        return false;
    }
    if unsafe { (*path.cast::<pg_sys::Node>()).type_ } != NodeTag::T_CustomPath {
        return false;
    }
    let cp = path.cast::<CustomPath>();
    unsafe { (*cp).methods == methods }
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

/// GPU-supported numeric type OIDs for expression evaluation.
const EXPR_BOOL_OID: u32 = 16;
const EXPR_INT2_OID: u32 = 21;
const EXPR_INT4_OID: u32 = 23;
const EXPR_INT8_OID: u32 = 20;
const EXPR_FLOAT4_OID: u32 = 700;
const EXPR_FLOAT8_OID: u32 = 701;
const EXPR_DATE_OID: u32 = 1082;
const EXPR_TIMESTAMP_OID: u32 = 1114;

/// Unsupported type families that must be declined explicitly at planning
/// time. These are intentionally policy rejections, not executor fallbacks:
/// pg_accel does not ship production-safe kernels/parsers for them today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UnsupportedTypePolicy {
    Json,
    Jsonb,
    Array,
    Interval,
    Domain,
    Composite,
    Custom,
}

impl UnsupportedTypePolicy {
    #[must_use]
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Jsonb => "jsonb",
            Self::Array => "array",
            Self::Interval => "interval",
            Self::Domain => "domain",
            Self::Composite => "composite",
            Self::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GpuTypeSupport {
    Supported,
    ExplicitReject(UnsupportedTypePolicy),
    UnsupportedOther,
}

impl GpuTypeSupport {
    #[must_use]
    const fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }

    #[must_use]
    const fn rejection(self) -> Option<UnsupportedTypePolicy> {
        match self {
            Self::ExplicitReject(policy) => Some(policy),
            Self::Supported | Self::UnsupportedOther => None,
        }
    }
}

#[must_use]
const fn is_builtin_array_oid(oid: u32) -> bool {
    matches!(
        oid,
        // Common built-in array OIDs from pg_type.dat. Dynamic/user arrays
        // are caught at planner time through get_element_type().
        1000 | 1001
            | 1002
            | 1003
            | 1005
            | 1007
            | 1009
            | 1014
            | 1015
            | 1016
            | 1017
            | 1021
            | 1022
            | 1028
            | 1033
            | 1115
            | 1182
            | 1183
            | 1185
            | 1187
            | 1231
            | 1270
            | 2951
            | 3807
    )
}

#[must_use]
const fn builtin_rejected_type_policy(oid: u32) -> Option<UnsupportedTypePolicy> {
    match oid {
        114 => Some(UnsupportedTypePolicy::Json),
        3802 => Some(UnsupportedTypePolicy::Jsonb),
        1186 => Some(UnsupportedTypePolicy::Interval),
        2249 => Some(UnsupportedTypePolicy::Composite),
        _ if is_builtin_array_oid(oid) => Some(UnsupportedTypePolicy::Array),
        _ => None,
    }
}

#[must_use]
fn gpu_supported_scalar_type_policy(oid: u32) -> GpuTypeSupport {
    if matches!(
        oid,
        EXPR_BOOL_OID
            | EXPR_INT2_OID
            | EXPR_INT4_OID
            | EXPR_INT8_OID
            | EXPR_FLOAT4_OID
            | EXPR_FLOAT8_OID
            | EXPR_DATE_OID
            | EXPR_TIMESTAMP_OID
    ) {
        return GpuTypeSupport::Supported;
    }

    if let Some(policy) = builtin_rejected_type_policy(oid) {
        return GpuTypeSupport::ExplicitReject(policy);
    }

    if oid >= pg_sys::FirstNormalObjectId {
        return GpuTypeSupport::ExplicitReject(UnsupportedTypePolicy::Custom);
    }

    GpuTypeSupport::UnsupportedOther
}

/// Planner-time type policy with catalog-backed detection for dynamic array,
/// domain, and composite types. Must run on the main backend thread.
///
/// # Safety
///
/// Calls PostgreSQL catalog helpers; caller must be in planner/backend context.
unsafe fn planner_type_policy(oid: pg_sys::Oid) -> GpuTypeSupport {
    let oid_raw = u32::from(oid);
    if gpu_supported_scalar_type_policy(oid_raw).is_supported() {
        return GpuTypeSupport::Supported;
    }
    if let Some(policy) = builtin_rejected_type_policy(oid_raw) {
        return GpuTypeSupport::ExplicitReject(policy);
    }
    if oid == pg_sys::InvalidOid {
        return GpuTypeSupport::UnsupportedOther;
    }

    // SAFETY: lsyscache type helpers are catalog lookups on the main backend
    // thread. They return InvalidOid / '\0' for invalid or missing types.
    let elem_oid = unsafe { pg_sys::get_element_type(oid) };
    if elem_oid != pg_sys::InvalidOid {
        return GpuTypeSupport::ExplicitReject(UnsupportedTypePolicy::Array);
    }

    // SAFETY: same catalog-lookup contract as above.
    let typtype = unsafe { pg_sys::get_typtype(oid) } as u8;
    if typtype == pg_sys::TYPTYPE_DOMAIN {
        return GpuTypeSupport::ExplicitReject(UnsupportedTypePolicy::Domain);
    }
    if typtype == pg_sys::TYPTYPE_COMPOSITE {
        return GpuTypeSupport::ExplicitReject(UnsupportedTypePolicy::Composite);
    }

    // SAFETY: type_is_rowtype includes RECORDOID and domains over composite.
    if unsafe { pg_sys::type_is_rowtype(oid) } {
        return GpuTypeSupport::ExplicitReject(UnsupportedTypePolicy::Composite);
    }

    if oid_raw >= pg_sys::FirstNormalObjectId {
        return GpuTypeSupport::ExplicitReject(UnsupportedTypePolicy::Custom);
    }

    GpuTypeSupport::UnsupportedOther
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

        // Map PG type OID to key type tag. INT2 deliberately stays out of
        // selected hash-join exposure: the join executor's INT32 extraction
        // lane reads int4-width values, so accepting int2 here would route to
        // a mismatched key buffer.
        let key_type = match u32::from(key_oid) {
            // int4 (23)
            23 => 0, // Int32
            // int8 (20)
            20 => 1, // Int64
            // float4 (700), float8 (701)
            700 | 701 => 2, // Float64
            _ => {
                // SAFETY: planner hook runs on the main backend thread.
                if let Some(policy) = unsafe { planner_type_policy(key_oid).rejection() } {
                    pgrx::debug1!(
                        "pg_accel join: rejected unsupported {} key type oid={}",
                        policy.label(),
                        u32::from(key_oid)
                    );
                }
                continue;
            }
        };

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod unit_tests {
    use super::*;
}

#[cfg(feature = "pg_test")]
mod tests;
