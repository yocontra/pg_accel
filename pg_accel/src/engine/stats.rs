//! Per-backend acceleration statistics.
//!
//! Each PostgreSQL backend is a separate process with a single thread, so we
//! use `thread_local!` + `RefCell` instead of shared memory or atomics.

use std::cell::RefCell;

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
}

thread_local! {
    static STATS: RefCell<AccelStats> = RefCell::new(AccelStats::default());
}

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

// ---------------------------------------------------------------------------
// SQL-callable functions
// ---------------------------------------------------------------------------

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
        name!(fallback_count, i64),
        name!(gpu_rows_processed, i64),
        name!(gpu_uncertain_count, i64),
        name!(thread_budget_exhausted_count, i64),
    ),
> {
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
        )
    });
    TableIterator::new(std::iter::once(row))
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
    }
}
