//! Per-backend acceleration statistics.
//!
//! Each PostgreSQL backend is a separate process with a single thread, so we
//! use `thread_local!` + `RefCell` for cumulative per-backend stats. Counters
//! added for benchmark-mode dispatch assertions (planner rejects, GPU buffer
//! cache hits/misses, degenerate-guard trips) use `AtomicU64` so cheap
//! snapshots can be taken from the SRF or helper SQL functions without a
//! borrow of the thread-local.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use pgrx::prelude::*;

/// Cumulative counters for a single backend's pg_accel activity.
#[derive(Debug, Default, Clone)]
pub struct AccelStats {
    pub queries_accelerated: u64,
    pub rows_dispatched: u64,
    pub batches_executed: u64,
    /// Cumulative dispatch wall-clock time in microseconds.
    pub total_dispatch_us: u64,
    pub stock_exec_count: u64,
    pub gpu_rows_processed: u64,
    /// Rows the GPU marked uncertain. GPU-only execution should reject these
    /// instead of rechecking on CPU.
    pub gpu_uncertain_count: u64,
    pub thread_budget_exhausted_count: u64,
    pub planner_hook_calls: u64,
    pub command_type_skips: u64,
    pub window_gpu_failures: u64,
    // NOTE: there is deliberately no `gpu_kernel_executions` field here. The
    // `gpu_kernel_executions` SRF column is sourced live from the C++
    // thread-local counter via `crate::gpu::gpu_exec_count()` (see
    // `pg_accel_stats`), never from this struct. A struct field was dead —
    // written nowhere in production and only ever read back as its own
    // default zero — so it was removed rather than left as a misleading
    // always-zero counter.
}

thread_local! {
    static STATS: RefCell<AccelStats> = RefCell::new(AccelStats::default());
    static LAST_PLANNER_REJECTION_REASON: RefCell<Option<&'static str>> = const { RefCell::new(None) };
    static PLANNER_REJECTION_REASON_COUNTS: RefCell<BTreeMap<&'static str, u64>> = const { RefCell::new(BTreeMap::new()) };
}

// ---------------------------------------------------------------------------
// Process-wide atomic counters for bench-mode dispatch coverage assertions.
// ---------------------------------------------------------------------------

/// Number of paths the planner considered for GPU injection, regardless of
/// whether the injection succeeded. Denominator for the rejection ratio.
static PLANNER_CONSIDERED: AtomicU64 = AtomicU64::new(0);

/// Number of paths the planner evaluated and declined to inject. Reviewer 3
/// needs this to distinguish "GPU ran and tied" from "planner silently
/// declined to inject". See `benchmarks/action_items.md` §C3.
static PLANNER_REJECTED: AtomicU64 = AtomicU64::new(0);

/// Number of times the degenerate-geometry guard in the three-layer
/// pipeline fired. Incremented by `increment_degenerate_guard()` from
/// call sites that detect degenerate geometries before GPU dispatch.
static DEGENERATE_GUARD_TRIGGERS: AtomicU64 = AtomicU64::new(0);

/// GPU input buffer cache hits (persistent per-column device buffer cache
/// owned by Fix Agent 4). Call sites live in the executor agg/hashjoin
/// layer; this module only provides the increment helper.
static GPU_CACHE_HITS: AtomicU64 = AtomicU64::new(0);

/// GPU input buffer cache misses — a column was requested but had to be
/// uploaded fresh.
static GPU_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);

/// Cumulative microseconds spent inside the three pg_accel planner hooks
/// (`pgaccel_set_rel_pathlist`, `pgaccel_set_join_pathlist`,
/// `pgaccel_create_upper_paths`). Each hook invocation adds its elapsed
/// time once, regardless of whether a CustomPath was injected or the hook
/// fast-declined. Read via `pg_accel_planner_overhead_us()` from SQL; the
/// bench harness uses it to detect Phase 0 regressions in planner-hook
/// overhead on no-dispatch queries (SSBM-shaped joins, expression-only
/// filters, native aggregates).
static PLANNER_HOOK_TOTAL_US: AtomicU64 = AtomicU64::new(0);

/// Number of planner hook invocations that hit the O(1) early-decline
/// fast path (Phase 0 audit, 2026-05-14). Incremented by
/// `record_planner_fast_decline()` from hot decline gates like
/// "no GPU hashjoin kernel + no spatial adapters" or "GROUP BY has an
/// unsupported group-key type". Read alongside
/// `PLANNER_HOOK_TOTAL_US` to confirm the audit is working — high
/// fast-decline counter + low total microseconds means hooks bail early
/// instead of walking full pathlists.
static PLANNER_FAST_DECLINE: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Helpers to increment counters
// ---------------------------------------------------------------------------

/// Record that a query was routed through the accelerated path.
pub fn record_query_accelerated() {
    STATS.with(|s| {
        s.borrow_mut().queries_accelerated += 1;
    });
}

/// Record a completed batch execution.
pub fn record_batch(rows: u64, dispatch_us: u64) {
    STATS.with(|s| {
        let mut st = s.borrow_mut();
        st.batches_executed += 1;
        st.rows_dispatched += rows;
        st.total_dispatch_us += dispatch_us;
    });
}

/// Record a fallback to the standard PostgreSQL executor.
pub fn record_stock_exec() {
    STATS.with(|s| {
        s.borrow_mut().stock_exec_count += 1;
    });
}

/// Record a GPU kernel batch completion.
pub fn record_gpu_batch(rows: u64, uncertain: u64) {
    STATS.with(|s| {
        let mut st = s.borrow_mut();
        st.gpu_rows_processed += rows;
        st.gpu_uncertain_count += uncertain;
    });
}

/// Record that the thread budget was exhausted and work had to be serialised.
pub fn record_budget_exhausted() {
    STATS.with(|s| {
        s.borrow_mut().thread_budget_exhausted_count += 1;
    });
}

/// Record a planner hook invocation.
pub fn record_planner_hook_call() {
    STATS.with(|s| {
        s.borrow_mut().planner_hook_calls += 1;
    });
}

/// Record that a query was skipped due to unsupported command type.
pub fn record_command_type_skip() {
    STATS.with(|s| {
        s.borrow_mut().command_type_skips += 1;
    });
}

/// Record a window function GPU dispatch failure.
pub fn record_window_gpu_failure() {
    STATS.with(|s| {
        s.borrow_mut().window_gpu_failures += 1;
    });
}

// ---------------------------------------------------------------------------
// Bench-mode dispatch-coverage counter helpers.
//
// All of these use `Ordering::Relaxed` — they are observability counters,
// not synchronisation primitives, and the benchmark harness reads them from
// the same backend process that writes them.
// ---------------------------------------------------------------------------

/// Increment the count of planner paths considered for GPU injection.
///
/// Emits a `stats.planner_considered` tracing event with the reason and
/// estimated row count.
#[inline]
pub fn increment_planner_considered(reason: &'static str, n_rows_estimate: u64) {
    PLANNER_CONSIDERED.fetch_add(1, Ordering::Relaxed);
    tracing::trace!(
        target: "pg_accel::stats",
        reason,
        n_rows_estimate,
        "stats.planner_considered"
    );
}

/// Increment the count of planner paths that were declined.
///
/// Emits a `stats.planner_rejected` tracing event. The `reason` string
/// should identify the gate that rejected (e.g. `"rows_below_min_batch"`,
/// `"spatial_index_cheaper"`, `"command_type_skip"`) so reviewers reading
/// `pg_accel_traces.jsonl` can aggregate by reason code.
#[inline]
pub fn increment_planner_rejected(reason: &'static str, n_rows_estimate: u64) {
    PLANNER_REJECTED.fetch_add(1, Ordering::Relaxed);
    LAST_PLANNER_REJECTION_REASON.with(|slot| {
        *slot.borrow_mut() = Some(reason);
    });
    PLANNER_REJECTION_REASON_COUNTS.with(|counts| {
        let mut counts = counts.borrow_mut();
        *counts.entry(reason).or_insert(0) += 1;
    });
    tracing::info!(
        target: "pg_accel::stats",
        reason,
        n_rows_estimate,
        "stats.planner_rejected"
    );
}

/// Snapshot of the planner-considered counter.
#[inline]
#[must_use]
pub fn read_planner_considered() -> u64 {
    PLANNER_CONSIDERED.load(Ordering::Relaxed)
}

/// Snapshot of the planner-rejected counter.
#[inline]
#[must_use]
pub fn read_planner_rejected() -> u64 {
    PLANNER_REJECTED.load(Ordering::Relaxed)
}

/// Last planner rejection reason seen by this backend, if any.
#[inline]
#[must_use]
pub fn read_last_planner_rejection_reason() -> Option<&'static str> {
    LAST_PLANNER_REJECTION_REASON.with(|slot| *slot.borrow())
}

/// Count of planner rejections with a specific reason observed by this backend.
#[inline]
#[must_use]
pub fn read_planner_rejection_reason_count(reason: &str) -> u64 {
    PLANNER_REJECTION_REASON_COUNTS.with(|counts| counts.borrow().get(reason).copied().unwrap_or(0))
}

/// Increment the degenerate-guard trigger counter.
///
/// Wired from Fix Agent 1's `three_layer.rs` once its accessor lands. Until
/// then, any call site that notices a degenerate-geometry short-circuit can
/// hit this helper to keep the SRF column non-zero in tests.
#[inline]
pub fn increment_degenerate_guard() {
    DEGENERATE_GUARD_TRIGGERS.fetch_add(1, Ordering::Relaxed);
}

/// Snapshot of the degenerate-guard counter.
#[inline]
#[must_use]
pub fn read_degenerate_guard() -> u64 {
    DEGENERATE_GUARD_TRIGGERS.load(Ordering::Relaxed)
}

/// Increment the GPU input buffer cache hit counter.
///
/// Call site: Fix Agent 4's persistent GPU buffer cache, when a column upload
/// is skipped because the device buffer is already populated.
#[inline]
pub fn increment_gpu_cache_hit() {
    GPU_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
}

/// Increment the GPU input buffer cache miss counter.
///
/// Call site: Fix Agent 4's persistent GPU buffer cache, when a column upload
/// has to happen because no cached device buffer exists.
#[inline]
pub fn increment_gpu_cache_miss() {
    GPU_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
}

/// Snapshot of the GPU cache hit counter.
#[inline]
#[must_use]
pub fn read_gpu_cache_hits() -> u64 {
    GPU_CACHE_HITS.load(Ordering::Relaxed)
}

/// Snapshot of the GPU cache miss counter.
#[inline]
#[must_use]
pub fn read_gpu_cache_misses() -> u64 {
    GPU_CACHE_MISSES.load(Ordering::Relaxed)
}

/// Add an elapsed-microseconds sample for one planner hook invocation.
///
/// Call from every planner hook entry point — `pgaccel_set_rel_pathlist`,
/// `pgaccel_set_join_pathlist`, `pgaccel_create_upper_paths` — at the
/// exit point (both fast-decline and full-walk branches) so the total
/// reflects all time spent in pg_accel's planner code. The argument is
/// the elapsed `std::time::Instant` duration, converted to microseconds
/// by the caller.
///
/// Emits a `stats.planner_hook_elapsed` tracing event for debug visibility;
/// keep this at `trace` level so it does not flood the JSONL trace file
/// during normal operation (filter is `notice` by default per CLAUDE.md).
#[inline]
pub fn record_planner_hook_elapsed(hook: &'static str, elapsed_us: u64) {
    PLANNER_HOOK_TOTAL_US.fetch_add(elapsed_us, Ordering::Relaxed);
    tracing::trace!(
        target: "pg_accel::stats",
        hook,
        elapsed_us,
        "stats.planner_hook_elapsed"
    );
}

/// Record that a planner hook returned via the O(1) early-decline path.
///
/// Use from the Phase 0 fast-decline gates so the bench harness can
/// confirm the audit is producing the expected hot-path drop without
/// reading the trace file. The `reason` string identifies which fast
/// gate fired; keep it static so aggregation by reason works.
#[inline]
pub fn record_planner_fast_decline(reason: &'static str) {
    PLANNER_FAST_DECLINE.fetch_add(1, Ordering::Relaxed);
    tracing::trace!(
        target: "pg_accel::stats",
        reason,
        "stats.planner_fast_decline"
    );
}

/// Snapshot of the cumulative planner-hook elapsed-time counter.
#[inline]
#[must_use]
pub fn read_planner_hook_total_us() -> u64 {
    PLANNER_HOOK_TOTAL_US.load(Ordering::Relaxed)
}

/// Snapshot of the planner fast-decline counter.
#[inline]
#[must_use]
pub fn read_planner_fast_decline() -> u64 {
    PLANNER_FAST_DECLINE.load(Ordering::Relaxed)
}

/// Cheap snapshot of the monotonic GPU kernel execution counter.
///
/// Delegates to the C++ thread-local counter exposed via `crate::gpu`.
/// The benchmark harness calls this before and after a timed workload to
/// compute a "delta since last read" — the delta subtraction is the
/// caller's responsibility, the stats module only provides the read.
///
/// Also emits a `stats.kernel_executed` tracing event — but only when
/// the count has changed since the last snapshot, to avoid spamming the
/// trace file.
#[inline]
#[must_use]
pub fn kernel_executions_snapshot() -> u64 {
    let count = crate::gpu::gpu_exec_count();
    tracing::trace!(
        target: "pg_accel::stats",
        kernel_name = "all",
        n_rows = 0_u64,
        count,
        "stats.kernel_executed"
    );
    count
}

// ---------------------------------------------------------------------------
// SQL-callable functions
// ---------------------------------------------------------------------------

/// Returns per-domain GPU kernel-failure counters (backend-local), one row per
/// [`crate::gpu::counters::GpuFailureDomain`], plus an `unknown_status` row for
/// out-of-range raw status values from the C side. Failures are recorded at the
/// single status-conversion point in `gpu::bridge`, so every non-OK kernel
/// status is visible here regardless of how the caller degraded it.
#[pg_extern]
fn pg_accel_gpu_failures()
-> TableIterator<'static, (name!(domain, String), name!(failure_count, i64))> {
    use crate::gpu::{GpuFailureDomain as D, kernel_failure_count, unknown_status_count};
    const DOMAINS: [(D, &str); 12] = [
        (D::Runtime, "runtime"),
        (D::Spatial, "spatial"),
        (D::H3, "h3"),
        (D::Raster, "raster"),
        (D::Sort, "sort"),
        (D::Reduce, "reduce"),
        (D::Expr, "expr"),
        (D::HashAgg, "hash_agg"),
        (D::HashJoin, "hash_join"),
        (D::Window, "window"),
        (D::NestedLoop, "nested_loop"),
        (D::Memory, "memory"),
    ];
    let mut rows: Vec<(String, i64)> = DOMAINS
        .iter()
        .map(|(d, label)| {
            (
                (*label).to_string(),
                i64::try_from(kernel_failure_count(*d)).unwrap_or(i64::MAX),
            )
        })
        .collect();
    rows.push((
        "unknown_status".to_string(),
        i64::try_from(unknown_status_count()).unwrap_or(i64::MAX),
    ));
    TableIterator::new(rows)
}

/// Returns per-backend acceleration counters as a single row.
#[pg_extern]
#[allow(clippy::type_complexity)]
fn pg_accel_stats() -> TableIterator<
    'static,
    (
        name!(queries_accelerated, i64),
        name!(rows_dispatched, i64),
        name!(batches_executed, i64),
        name!(total_dispatch_us, i64),
        name!(stock_exec_count, i64),
        name!(gpu_rows_processed, i64),
        name!(gpu_uncertain_count, i64),
        name!(thread_budget_exhausted_count, i64),
        name!(planner_hook_calls, i64),
        name!(command_type_skips, i64),
        name!(window_gpu_failures, i64),
        name!(gpu_kernel_executions, i64),
        name!(planner_considered_count, i64),
        name!(planner_rejected_count, i64),
        name!(degenerate_guard_trigger_count, i64),
        name!(gpu_cache_hit_count, i64),
        name!(gpu_cache_miss_count, i64),
        name!(planner_hook_total_us, i64),
        name!(planner_fast_decline_count, i64),
    ),
> {
    let gpu_execs = crate::gpu::gpu_exec_count();
    let planner_considered = read_planner_considered();
    let planner_rejected = read_planner_rejected();
    let degenerate_guard = read_degenerate_guard();
    let gpu_cache_hits = read_gpu_cache_hits();
    let gpu_cache_misses = read_gpu_cache_misses();
    let planner_total_us = read_planner_hook_total_us();
    let planner_fast_decline = read_planner_fast_decline();
    let row = STATS.with(|s| {
        let st = s.borrow();
        (
            st.queries_accelerated as i64,
            st.rows_dispatched as i64,
            st.batches_executed as i64,
            st.total_dispatch_us as i64,
            st.stock_exec_count as i64,
            st.gpu_rows_processed as i64,
            st.gpu_uncertain_count as i64,
            st.thread_budget_exhausted_count as i64,
            st.planner_hook_calls as i64,
            st.command_type_skips as i64,
            st.window_gpu_failures as i64,
            gpu_execs as i64,
            planner_considered as i64,
            planner_rejected as i64,
            degenerate_guard as i64,
            gpu_cache_hits as i64,
            gpu_cache_misses as i64,
            planner_total_us as i64,
            planner_fast_decline as i64,
        )
    });
    TableIterator::new(std::iter::once(row))
}

/// Returns the cumulative microseconds spent inside pg_accel planner hooks
/// since the backend started. Cheap atomic load. The Phase 0 planner-hook
/// overhead audit (TODO.md 2026-05-14) uses this to spot regressions in
/// no-dispatch query overhead without re-decoding the full `pg_accel_stats()`
/// SRF.
#[pg_extern]
fn pg_accel_planner_overhead_us() -> i64 {
    read_planner_hook_total_us() as i64
}

/// Returns the count of planner hook invocations that hit the Phase 0
/// O(1) early-decline fast path. Used together with
/// `pg_accel_planner_overhead_us()` to verify SSBM-shaped queries take
/// the cheap decline path. Higher fast-decline count + lower total
/// microseconds is the success signal.
#[pg_extern]
fn pg_accel_planner_fast_decline_count() -> i64 {
    read_planner_fast_decline() as i64
}

/// Returns the monotonic count of GPU kernel executions since this backend
/// started. Cheap read (single atomic load via the C++ thread-local
/// counter). The benchmark harness calls this before and after each timed
/// workload and subtracts to learn whether any GPU kernel fired. Cheaper
/// than decoding the full `pg_accel_stats()` SRF just for this one column.
#[pg_extern]
fn pg_accel_kernel_executions() -> i64 {
    kernel_executions_snapshot() as i64
}

/// Resets all per-backend acceleration counters to zero.
#[pg_extern]
fn pg_accel_reset_stats() {
    STATS.with(|s| {
        *s.borrow_mut() = AccelStats::default();
    });
    LAST_PLANNER_REJECTION_REASON.with(|slot| {
        *slot.borrow_mut() = None;
    });
    PLANNER_REJECTION_REASON_COUNTS.with(|counts| {
        counts.borrow_mut().clear();
    });
    // Reset the process-wide atomic counters too. Each PG backend is a
    // separate process, so these atomics are effectively per-backend and the
    // benchmark harness (which calls this immediately before a timed EXPLAIN
    // / query) expects a clean slate. Leaving them cumulative made the
    // planner-considered/rejected, GPU-cache, planner-overhead, degenerate-
    // guard, and fast-decline SRF columns read stale totals after a reset.
    PLANNER_CONSIDERED.store(0, Ordering::Relaxed);
    PLANNER_REJECTED.store(0, Ordering::Relaxed);
    DEGENERATE_GUARD_TRIGGERS.store(0, Ordering::Relaxed);
    GPU_CACHE_HITS.store(0, Ordering::Relaxed);
    GPU_CACHE_MISSES.store(0, Ordering::Relaxed);
    PLANNER_HOOK_TOTAL_US.store(0, Ordering::Relaxed);
    PLANNER_FAST_DECLINE.store(0, Ordering::Relaxed);
    // `gpu_kernel_executions` is intentionally NOT reset here: it is a
    // monotonic counter owned by the C++ runtime (`crate::gpu::gpu_exec_count`)
    // that the harness reads by *delta* (before/after subtraction), not by
    // absolute value, so a reset would be meaningless and cross-module.
}

/// Returns the last planner rejection reason observed by this backend.
///
/// Benchmark plan capture resets stats immediately before `EXPLAIN`, then
/// reads this value after planning so native-decline matrix rows can prove the
/// policy gate that declined a pg_accel plan.
#[pg_extern]
fn pg_accel_last_planner_rejection_reason() -> Option<String> {
    read_last_planner_rejection_reason().map(str::to_owned)
}

/// Returns the number of times this backend has observed a planner rejection
/// with the given reason since `pg_accel_reset_stats()`.
#[pg_extern]
fn pg_accel_planner_rejection_count(reason: String) -> i64 {
    read_planner_rejection_reason_count(&reason) as i64
}

/// Returns the effective [`DeviceLimits`](crate::engine::cost::DeviceLimits)
/// for this backend as one row per field.
///
/// The `source` column is either `hardware_derived` (values came from
/// [`DeviceLimits::from_profile`](crate::engine::cost::DeviceLimits::from_profile)
/// applied to the detected platform profile) or `fallback_cpu_only` (no GPU
/// was detected so [`DeviceLimits::cpu_only`](crate::engine::cost::DeviceLimits::cpu_only)
/// was used). Benchmarks and dispatch tracing should use this function to
/// discover the real thresholds on the current machine — the constants listed
/// in the `cpu_only()` fallback at `engine/cost/device_limits.rs` are only
/// active when there is no GPU.
#[pg_extern]
#[allow(clippy::type_complexity, clippy::too_many_lines)]
fn pg_accel_device_limits() -> TableIterator<
    'static,
    (
        name!(name, String),
        name!(value, String),
        name!(source, String),
    ),
> {
    let limits = crate::engine::cost::device_limits();
    let source = crate::engine::cost::device_limits_source()
        .as_str()
        .to_owned();

    // Keep ordering aligned with the struct declaration in
    // `engine/cost/device_limits.rs` so readers can cross-reference.
    let rows: Vec<(String, String)> = vec![
        ("gpu_min_rows".into(), limits.gpu_min_rows.to_string()),
        (
            "gpu_sort_min_rows".into(),
            limits.gpu_sort_min_rows.to_string(),
        ),
        (
            "gpu_sort_planner_min_rows".into(),
            limits.gpu_sort_planner_min_rows.to_string(),
        ),
        (
            "gpu_window_min_rows".into(),
            limits.gpu_window_min_rows.to_string(),
        ),
        (
            "gpu_reduce_min_rows".into(),
            limits.gpu_reduce_min_rows.to_string(),
        ),
        (
            "gpu_hash_agg_min_rows".into(),
            limits.gpu_hash_agg_min_rows.to_string(),
        ),
        (
            "gpu_hash_agg_max_groups".into(),
            limits.gpu_hash_agg_max_groups.to_string(),
        ),
        (
            "gpu_reduce_max_chunk".into(),
            limits.gpu_reduce_max_chunk.to_string(),
        ),
        (
            "gpu_sort_max_elements".into(),
            limits.gpu_sort_max_elements.to_string(),
        ),
        (
            "gpu_join_max_output_rows".into(),
            limits.gpu_join_max_output_rows.to_string(),
        ),
        (
            "gpu_spatial_min_vertices".into(),
            limits.gpu_spatial_min_vertices.to_string(),
        ),
        (
            "gpu_spatial_max_output_fraction".into(),
            limits.gpu_spatial_max_output_fraction.to_string(),
        ),
        (
            "gpu_expr_min_rows".into(),
            limits.gpu_expr_min_rows.to_string(),
        ),
        (
            "gpu_hash_join_build_max_rows".into(),
            limits.gpu_hash_join_build_max_rows.to_string(),
        ),
        (
            "gpu_pipeline_fusion_min_rows".into(),
            limits.gpu_pipeline_fusion_min_rows.to_string(),
        ),
        (
            "gpu_preagg_min_fact_rows".into(),
            limits.gpu_preagg_min_fact_rows.to_string(),
        ),
        (
            "gpu_preagg_max_dim_rows".into(),
            limits.gpu_preagg_max_dim_rows.to_string(),
        ),
        (
            "preagg_dim_materialize_cost".into(),
            limits.preagg_dim_materialize_cost.to_string(),
        ),
        (
            "preagg_fact_scan_cost".into(),
            limits.preagg_fact_scan_cost.to_string(),
        ),
        (
            "preagg_probe_cost".into(),
            limits.preagg_probe_cost.to_string(),
        ),
        ("preagg_agg_cost".into(), limits.preagg_agg_cost.to_string()),
        (
            "preagg_yield_cost".into(),
            limits.preagg_yield_cost.to_string(),
        ),
        (
            "optimal_batch_min".into(),
            limits.optimal_batch_min.to_string(),
        ),
        (
            "optimal_batch_max".into(),
            limits.optimal_batch_max.to_string(),
        ),
        (
            "fused_interrupt_interval".into(),
            limits.fused_interrupt_interval.to_string(),
        ),
        (
            "gpu_op_cost_reduce".into(),
            limits.gpu_op_cost_reduce.to_string(),
        ),
        (
            "gpu_op_cost_hash_agg".into(),
            limits.gpu_op_cost_hash_agg.to_string(),
        ),
        (
            "gpu_op_cost_sort".into(),
            limits.gpu_op_cost_sort.to_string(),
        ),
        (
            "gpu_op_cost_window".into(),
            limits.gpu_op_cost_window.to_string(),
        ),
        (
            "gpu_op_cost_filter".into(),
            limits.gpu_op_cost_filter.to_string(),
        ),
        (
            "gpu_hashjoin_build_per_row".into(),
            limits.gpu_hashjoin_build_per_row.to_string(),
        ),
        (
            "gpu_hashjoin_probe_per_row".into(),
            limits.gpu_hashjoin_probe_per_row.to_string(),
        ),
        (
            "custom_scan_yield_per_row".into(),
            limits.custom_scan_yield_per_row.to_string(),
        ),
        (
            "gpu_partial_agg_per_row".into(),
            limits.gpu_partial_agg_per_row.to_string(),
        ),
        (
            "gpu_agg_cost_ratio".into(),
            limits.gpu_agg_cost_ratio.to_string(),
        ),
        (
            "gpu_window_cost_ratio".into(),
            limits.gpu_window_cost_ratio.to_string(),
        ),
        (
            "gpu_preagg_cost_ratio".into(),
            limits.gpu_preagg_cost_ratio.to_string(),
        ),
        (
            "reduce_f32_break_even_rows".into(),
            limits.reduce_f32_break_even_rows.to_string(),
        ),
        (
            "reduce_f64_break_even_rows".into(),
            limits.reduce_f64_break_even_rows.to_string(),
        ),
        (
            "reduce_i64_break_even_rows".into(),
            limits.reduce_i64_break_even_rows.to_string(),
        ),
        (
            "reduce_bit_break_even_rows".into(),
            limits.reduce_bit_break_even_rows.to_string(),
        ),
        (
            "reduce_bool_break_even_rows".into(),
            limits.reduce_bool_break_even_rows.to_string(),
        ),
        (
            "hashagg_min_rows_per_group".into(),
            limits.hashagg_min_rows_per_group.to_string(),
        ),
        (
            "hashagg_max_state_bytes_per_group".into(),
            limits.hashagg_max_state_bytes_per_group.to_string(),
        ),
        (
            "sort_break_even_rows_int".into(),
            limits.sort_break_even_rows_int.to_string(),
        ),
        (
            "sort_break_even_rows_float".into(),
            limits.sort_break_even_rows_float.to_string(),
        ),
        (
            "spatial_point_in_ring_break_even_verts_x_rows".into(),
            limits
                .spatial_point_in_ring_break_even_verts_x_rows
                .to_string(),
        ),
        (
            "spatial_point_in_ring_max_verts_x_rows".into(),
            limits.spatial_point_in_ring_max_verts_x_rows.to_string(),
        ),
        (
            "window_min_partition_rows".into(),
            limits.window_min_partition_rows.to_string(),
        ),
        (
            "expr_min_predicate_complexity_x_rows".into(),
            limits.expr_min_predicate_complexity_x_rows.to_string(),
        ),
        (
            "hashjoin_min_build_rows".into(),
            limits.hashjoin_min_build_rows.to_string(),
        ),
        (
            "gpu_nlj_min_outer_rows".into(),
            limits.gpu_nlj_min_outer_rows.to_string(),
        ),
        (
            "gpu_nlj_min_inner_rows".into(),
            limits.gpu_nlj_min_inner_rows.to_string(),
        ),
        (
            "gpu_nlj_max_output_rows".into(),
            limits.gpu_nlj_max_output_rows.to_string(),
        ),
        (
            "gpu_nlj_per_pair_cost".into(),
            limits.gpu_nlj_per_pair_cost.to_string(),
        ),
        (
            "gpu_spatial_pairwise_max_rows".into(),
            limits.gpu_spatial_pairwise_max_rows.to_string(),
        ),
        ("has_native_fp64".into(), limits.has_native_fp64.to_string()),
        (
            "soft_fp64_cost_multiplier".into(),
            limits.soft_fp64_cost_multiplier.to_string(),
        ),
    ];

    TableIterator::new(rows.into_iter().map(move |(n, v)| (n, v, source.clone())))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used, dead_code)]
#[pgrx::pg_schema]
mod tests {
    use super::*;

    /// Reset thread-local stats before each test so ordering does not matter.
    fn reset() {
        STATS.with(|s| *s.borrow_mut() = AccelStats::default());
        LAST_PLANNER_REJECTION_REASON.with(|slot| *slot.borrow_mut() = None);
        PLANNER_REJECTION_REASON_COUNTS.with(|counts| counts.borrow_mut().clear());
    }

    fn snapshot() -> AccelStats {
        STATS.with(|s| s.borrow().clone())
    }

    // -- record_query_accelerated ---------------------------------------------

    #[test]
    fn query_accelerated_increments() {
        reset();
        record_query_accelerated();
        record_query_accelerated();
        assert_eq!(snapshot().queries_accelerated, 2);
    }

    // -- record_batch ---------------------------------------------------------

    #[test]
    fn batch_records_rows_and_time() {
        reset();
        record_batch(500, 1200);
        record_batch(300, 800);
        let s = snapshot();
        assert_eq!(s.batches_executed, 2);
        assert_eq!(s.rows_dispatched, 800);
        assert_eq!(s.total_dispatch_us, 2000);
    }

    // -- record_stock_exec ------------------------------------------------------

    #[test]
    fn fallback_increments() {
        reset();
        record_stock_exec();
        assert_eq!(snapshot().stock_exec_count, 1);
    }

    // -- record_gpu_batch -----------------------------------------------------

    #[test]
    fn gpu_batch_records_rows_and_uncertain() {
        reset();
        record_gpu_batch(1000, 42);
        record_gpu_batch(2000, 8);
        let s = snapshot();
        assert_eq!(s.gpu_rows_processed, 3000);
        assert_eq!(s.gpu_uncertain_count, 50);
    }

    // -- record_budget_exhausted ----------------------------------------------

    #[test]
    fn budget_exhausted_increments() {
        reset();
        record_budget_exhausted();
        record_budget_exhausted();
        record_budget_exhausted();
        assert_eq!(snapshot().thread_budget_exhausted_count, 3);
    }

    // -- reset ----------------------------------------------------------------

    #[test]
    fn reset_zeros_all_counters() {
        reset();
        record_query_accelerated();
        record_batch(100, 50);
        record_stock_exec();
        record_gpu_batch(200, 10);
        record_budget_exhausted();

        // Verify non-zero before reset.
        let before = snapshot();
        assert!(before.queries_accelerated > 0);
        assert!(before.rows_dispatched > 0);

        reset();
        let after = snapshot();
        assert_eq!(after.queries_accelerated, 0);
        assert_eq!(after.rows_dispatched, 0);
        assert_eq!(after.batches_executed, 0);
        assert_eq!(after.total_dispatch_us, 0);
        assert_eq!(after.stock_exec_count, 0);
        assert_eq!(after.gpu_rows_processed, 0);
        assert_eq!(after.gpu_uncertain_count, 0);
        assert_eq!(after.thread_budget_exhausted_count, 0);
        assert_eq!(after.planner_hook_calls, 0);
        assert_eq!(after.command_type_skips, 0);
        assert_eq!(after.window_gpu_failures, 0);
        assert_eq!(read_last_planner_rejection_reason(), None);
        assert_eq!(read_planner_rejection_reason_count("test_reason"), 0);
    }

    // -- combined scenario ----------------------------------------------------

    #[test]
    fn combined_scenario() {
        reset();
        record_query_accelerated();
        record_batch(1024, 500);
        record_gpu_batch(1024, 3);
        record_batch(512, 250);
        record_stock_exec();
        record_budget_exhausted();

        let s = snapshot();
        assert_eq!(s.queries_accelerated, 1);
        assert_eq!(s.batches_executed, 2);
        assert_eq!(s.rows_dispatched, 1536);
        assert_eq!(s.total_dispatch_us, 750);
        assert_eq!(s.gpu_rows_processed, 1024);
        assert_eq!(s.gpu_uncertain_count, 3);
        assert_eq!(s.stock_exec_count, 1);
        assert_eq!(s.thread_budget_exhausted_count, 1);
    }

    // -- reset idempotency ----------------------------------------------------

    #[test]
    fn reset_twice_same_state() {
        reset();
        record_query_accelerated();
        record_batch(100, 50);

        reset();
        let after_first = snapshot();

        reset();
        let after_second = snapshot();

        assert_eq!(
            after_first.queries_accelerated,
            after_second.queries_accelerated
        );
        assert_eq!(after_first.rows_dispatched, after_second.rows_dispatched);
        assert_eq!(after_first.batches_executed, after_second.batches_executed);
        assert_eq!(
            after_first.total_dispatch_us,
            after_second.total_dispatch_us
        );
        assert_eq!(after_first.stock_exec_count, after_second.stock_exec_count);
        assert_eq!(
            after_first.gpu_rows_processed,
            after_second.gpu_rows_processed
        );
        assert_eq!(
            after_first.gpu_uncertain_count,
            after_second.gpu_uncertain_count
        );
        assert_eq!(
            after_first.thread_budget_exhausted_count,
            after_second.thread_budget_exhausted_count
        );
    }

    // -- multiple counter fields independently --------------------------------

    #[test]
    fn counters_are_independent() {
        reset();
        record_query_accelerated();
        let s = snapshot();
        assert_eq!(s.queries_accelerated, 1);
        assert_eq!(s.rows_dispatched, 0);
        assert_eq!(s.batches_executed, 0);
        assert_eq!(s.stock_exec_count, 0);
        assert_eq!(s.gpu_rows_processed, 0);
    }

    #[test]
    fn gpu_batch_does_not_affect_regular_batch() {
        reset();
        record_gpu_batch(500, 10);
        let s = snapshot();
        assert_eq!(s.gpu_rows_processed, 500);
        assert_eq!(s.gpu_uncertain_count, 10);
        // Regular batch counters untouched.
        assert_eq!(s.batches_executed, 0);
        assert_eq!(s.rows_dispatched, 0);
        assert_eq!(s.total_dispatch_us, 0);
    }

    // -- Debug formatting -----------------------------------------------------

    #[test]
    fn accel_stats_debug_format() {
        let s = AccelStats {
            queries_accelerated: 5,
            rows_dispatched: 1000,
            batches_executed: 2,
            total_dispatch_us: 500,
            stock_exec_count: 1,
            gpu_rows_processed: 800,
            gpu_uncertain_count: 3,
            thread_budget_exhausted_count: 0,
            planner_hook_calls: 0,
            command_type_skips: 0,
            window_gpu_failures: 0,
        };
        let dbg = format!("{s:?}");
        assert!(dbg.contains("queries_accelerated: 5"));
        assert!(dbg.contains("rows_dispatched: 1000"));
        assert!(dbg.contains("gpu_rows_processed: 800"));
    }

    // -- Default trait --------------------------------------------------------

    #[test]
    fn default_stats_all_zero() {
        let s = AccelStats::default();
        assert_eq!(s.queries_accelerated, 0);
        assert_eq!(s.rows_dispatched, 0);
        assert_eq!(s.batches_executed, 0);
        assert_eq!(s.total_dispatch_us, 0);
        assert_eq!(s.stock_exec_count, 0);
        assert_eq!(s.gpu_rows_processed, 0);
        assert_eq!(s.gpu_uncertain_count, 0);
        assert_eq!(s.thread_budget_exhausted_count, 0);
        assert_eq!(s.planner_hook_calls, 0);
        assert_eq!(s.command_type_skips, 0);
        assert_eq!(s.window_gpu_failures, 0);
    }

    // -- atomic bench-mode counters ------------------------------------------

    #[test]
    fn planner_considered_counter_increments() {
        let before = read_planner_considered();
        increment_planner_considered("test_reason", 1_000_000);
        assert!(read_planner_considered() >= before + 1);
    }

    #[test]
    fn planner_rejected_counter_increments() {
        reset();
        let before = read_planner_rejected();
        increment_planner_rejected("test_reason", 1_000_000);
        assert!(read_planner_rejected() >= before + 1);
        assert_eq!(read_last_planner_rejection_reason(), Some("test_reason"));
        assert_eq!(read_planner_rejection_reason_count("test_reason"), 1);
        assert_eq!(read_planner_rejection_reason_count("other_reason"), 0);
        increment_planner_rejected("test_reason", 1);
        increment_planner_rejected("other_reason", 1);
        assert_eq!(read_planner_rejection_reason_count("test_reason"), 2);
        assert_eq!(read_planner_rejection_reason_count("other_reason"), 1);
    }

    #[test]
    fn degenerate_guard_counter_increments() {
        let before = read_degenerate_guard();
        increment_degenerate_guard();
        assert!(read_degenerate_guard() >= before + 1);
    }

    #[test]
    fn gpu_cache_counters_increment() {
        let hits_before = read_gpu_cache_hits();
        let misses_before = read_gpu_cache_misses();
        increment_gpu_cache_hit();
        increment_gpu_cache_miss();
        assert!(read_gpu_cache_hits() >= hits_before + 1);
        assert!(read_gpu_cache_misses() >= misses_before + 1);
    }

    // -- pg_accel_device_limits SRF -----------------------------------------
    //
    // `device_limits()` returns `cpu_only()` under `#[cfg(test)]` (see
    // `engine/cost/device_limits.rs:474-478`), so the limit values checked
    // here are the documented fallback constants, not hardware-derived.
    // The bounds match the clamp ranges in `from_profile` so the asserts
    // stay valid on any hardware that runs the pg_test SRF variant.

    /// Asserts the fallback `gpu_reduce_min_rows` respects the documented
    /// clamp bounds from `DeviceLimits::from_profile`
    /// (`engine/cost/device_limits.rs:210`: `.clamp(5_000, 200_000)`).
    #[test]
    fn device_limits_fallback_reduce_min_rows_in_clamp_bounds() {
        let limits = crate::engine::cost::device_limits();
        assert!(
            (5_000..=200_000).contains(&limits.gpu_reduce_min_rows),
            "gpu_reduce_min_rows ({}) must lie within from_profile clamp \
             bounds [5_000, 200_000] even in the cpu_only fallback",
            limits.gpu_reduce_min_rows
        );
    }

    /// Asserts `device_limits_source()` reports `fallback_cpu_only` under
    /// `#[cfg(test)]` — cross-checks the documentation claim that tests see
    /// the fallback constants, not hardware-derived values.
    #[test]
    fn device_limits_source_is_fallback_under_cfg_test() {
        assert_eq!(
            crate::engine::cost::device_limits_source().as_str(),
            "fallback_cpu_only"
        );
    }

    /// End-to-end `#[pg_test]`: run the SQL SRF and verify it returns at
    /// least one row with a non-empty `source` column and the
    /// `gpu_reduce_min_rows` row falls inside the clamp bounds.
    #[pg_test]
    fn pg_accel_device_limits_returns_rows() {
        let count = Spi::get_one::<i64>("SELECT COUNT(*) FROM pg_accel_device_limits()")
            .expect("pg_accel_device_limits() should succeed")
            .expect("pg_accel_device_limits() should return a row count");
        assert!(count > 40, "expected >40 limit rows, got {count}");

        let source = Spi::get_one::<String>("SELECT source FROM pg_accel_device_limits() LIMIT 1")
            .expect("source column query should succeed")
            .expect("source column should be non-NULL");
        assert!(
            source == "hardware_derived" || source == "fallback_cpu_only",
            "unexpected source value: {source}"
        );

        let reduce_min = Spi::get_one::<String>(
            "SELECT value FROM pg_accel_device_limits() \
             WHERE name = 'gpu_reduce_min_rows'",
        )
        .expect("gpu_reduce_min_rows lookup should succeed")
        .expect("gpu_reduce_min_rows row should exist");
        let n: usize = reduce_min
            .parse()
            .expect("gpu_reduce_min_rows value should parse as usize");
        assert!(
            (5_000..=200_000).contains(&n),
            "gpu_reduce_min_rows = {n} out of documented clamp bounds"
        );
    }
}
