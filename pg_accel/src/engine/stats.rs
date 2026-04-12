//! Per-backend acceleration statistics.
//!
//! Each PostgreSQL backend is a separate process with a single thread, so we
//! use `thread_local!` + `RefCell` for the legacy cumulative struct. Counters
//! added for benchmark-mode dispatch assertions (planner rejects, GPU buffer
//! cache hits/misses, degenerate-guard trips) use `AtomicU64` so cheap
//! snapshots can be taken from the SRF or helper SQL functions without a
//! borrow of the thread-local.

use std::cell::RefCell;
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
    pub fallback_count: u64,
    pub gpu_rows_processed: u64,
    /// Rows that required CPU recheck after GPU evaluation.
    pub gpu_uncertain_count: u64,
    pub thread_budget_exhausted_count: u64,
    pub planner_hook_calls: u64,
    pub command_type_skips: u64,
    pub window_gpu_failures: u64,
    /// GPU kernel executions (from C++ thread-local counter).
    pub gpu_kernel_executions: u64,
    /// Rows for which GPU returned "uncertain" and PG native predicate had
    /// to be re-evaluated on the backend thread. Sourced from the C++
    /// three-layer bridge counter. Despite the historical symbol name this
    /// is NOT a CPU-fallback-for-whole-kernel metric — rule 11 forbids
    /// whole-kernel CPU fallback. It counts per-row rechecks only.
    /// Note: the C++ symbol is still `pgaccel_cpu_fallback_count`; the
    /// Rust SRF layer renames it to `recheck_count` at the SQL boundary.
    pub recheck_count: u64,
}

thread_local! {
    static STATS: RefCell<AccelStats> = RefCell::new(AccelStats::default());
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
pub fn record_fallback() {
    STATS.with(|s| {
        s.borrow_mut().fallback_count += 1;
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

/// Returns per-backend acceleration counters as a single row.
///
/// The `recheck_count` column is the number of rows for which the GPU
/// three-layer kernel returned `Uncertain` and the backend thread had to
/// re-evaluate the predicate using PG's native operator. It is NOT a
/// count of whole-kernel CPU fallbacks — rule 11 forbids those. It was
/// previously exposed as `cpu_fallback_count`, which misled reviewers
/// into thinking the extension had a GPU-bypass escape hatch.
#[pg_extern]
#[allow(clippy::type_complexity)]
fn pg_accel_stats() -> TableIterator<
    'static,
    (
        name!(queries_accelerated, i64),
        name!(rows_dispatched, i64),
        name!(batches_executed, i64),
        name!(total_dispatch_us, i64),
        name!(fallback_count, i64),
        name!(gpu_rows_processed, i64),
        name!(gpu_uncertain_count, i64),
        name!(thread_budget_exhausted_count, i64),
        name!(planner_hook_calls, i64),
        name!(command_type_skips, i64),
        name!(window_gpu_failures, i64),
        name!(gpu_kernel_executions, i64),
        name!(recheck_count, i64),
        name!(planner_considered_count, i64),
        name!(planner_rejected_count, i64),
        name!(degenerate_guard_trigger_count, i64),
        name!(gpu_cache_hit_count, i64),
        name!(gpu_cache_miss_count, i64),
    ),
> {
    let gpu_execs = crate::gpu::gpu_exec_count();
    // The C++ symbol is still `pgaccel_cpu_fallback_count`; the Rust SRF
    // layer surfaces it as `recheck_count` at the SQL boundary.
    let rechecks = crate::gpu::cpu_fallback_count();
    let planner_considered = read_planner_considered();
    let planner_rejected = read_planner_rejected();
    let degenerate_guard = read_degenerate_guard();
    let gpu_cache_hits = read_gpu_cache_hits();
    let gpu_cache_misses = read_gpu_cache_misses();
    let row = STATS.with(|s| {
        let st = s.borrow();
        (
            st.queries_accelerated as i64,
            st.rows_dispatched as i64,
            st.batches_executed as i64,
            st.total_dispatch_us as i64,
            st.fallback_count as i64,
            st.gpu_rows_processed as i64,
            st.gpu_uncertain_count as i64,
            st.thread_budget_exhausted_count as i64,
            st.planner_hook_calls as i64,
            st.command_type_skips as i64,
            st.window_gpu_failures as i64,
            gpu_execs as i64,
            rechecks as i64,
            planner_considered as i64,
            planner_rejected as i64,
            degenerate_guard as i64,
            gpu_cache_hits as i64,
            gpu_cache_misses as i64,
        )
    });
    TableIterator::new(std::iter::once(row))
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
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Reset thread-local stats before each test so ordering does not matter.
    fn reset() {
        STATS.with(|s| *s.borrow_mut() = AccelStats::default());
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

    // -- record_fallback ------------------------------------------------------

    #[test]
    fn fallback_increments() {
        reset();
        record_fallback();
        assert_eq!(snapshot().fallback_count, 1);
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
        record_fallback();
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
        assert_eq!(after.fallback_count, 0);
        assert_eq!(after.gpu_rows_processed, 0);
        assert_eq!(after.gpu_uncertain_count, 0);
        assert_eq!(after.thread_budget_exhausted_count, 0);
        assert_eq!(after.planner_hook_calls, 0);
        assert_eq!(after.command_type_skips, 0);
        assert_eq!(after.window_gpu_failures, 0);
    }

    // -- combined scenario ----------------------------------------------------

    #[test]
    fn combined_scenario() {
        reset();
        record_query_accelerated();
        record_batch(1024, 500);
        record_gpu_batch(1024, 3);
        record_batch(512, 250);
        record_fallback();
        record_budget_exhausted();

        let s = snapshot();
        assert_eq!(s.queries_accelerated, 1);
        assert_eq!(s.batches_executed, 2);
        assert_eq!(s.rows_dispatched, 1536);
        assert_eq!(s.total_dispatch_us, 750);
        assert_eq!(s.gpu_rows_processed, 1024);
        assert_eq!(s.gpu_uncertain_count, 3);
        assert_eq!(s.fallback_count, 1);
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
        assert_eq!(after_first.fallback_count, after_second.fallback_count);
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
        assert_eq!(s.fallback_count, 0);
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
            fallback_count: 1,
            gpu_rows_processed: 800,
            gpu_uncertain_count: 3,
            thread_budget_exhausted_count: 0,
            planner_hook_calls: 0,
            command_type_skips: 0,
            window_gpu_failures: 0,
            gpu_kernel_executions: 0,
            recheck_count: 0,
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
        assert_eq!(s.fallback_count, 0);
        assert_eq!(s.gpu_rows_processed, 0);
        assert_eq!(s.gpu_uncertain_count, 0);
        assert_eq!(s.thread_budget_exhausted_count, 0);
        assert_eq!(s.planner_hook_calls, 0);
        assert_eq!(s.command_type_skips, 0);
        assert_eq!(s.window_gpu_failures, 0);
        assert_eq!(s.gpu_kernel_executions, 0);
        assert_eq!(s.recheck_count, 0);
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
        let before = read_planner_rejected();
        increment_planner_rejected("test_reason", 1_000_000);
        assert!(read_planner_rejected() >= before + 1);
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
}
