//! Shared executor metrics for Custom Scan nodes.
//!
//! Concrete executors currently keep these counters directly on their state
//! structs for EXPLAIN integration. `ExecMetrics` is the reusable shape for
//! new or migrated executors: rows fed to a dispatch unit, logical dispatch
//! batches completed, and cumulative dispatch time in microseconds.

use std::time::Duration;

/// Cumulative executor counters surfaced through EXPLAIN ANALYZE.
///
/// A "batch" here means one logical executor dispatch unit. That may be a GPU
/// kernel launch or a fused executor pass, depending on the concrete executor.
/// The type intentionally does not record
/// global stats side effects; callers that also update `engine::stats` should
/// continue doing that at the dispatch site.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct ExecMetrics {
    /// Total input rows consumed by dispatch units.
    pub rows_dispatched: u64,
    /// Number of logical dispatch units completed.
    pub batches_executed: u64,
    /// Cumulative dispatch time in microseconds.
    pub dispatch_time_us: u64,
}

impl ExecMetrics {
    /// Construct zeroed metrics.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
        }
    }

    /// Construct metrics from explicit counter values.
    #[must_use]
    pub const fn from_parts(
        rows_dispatched: u64,
        batches_executed: u64,
        dispatch_time_us: u64,
    ) -> Self {
        Self {
            rows_dispatched,
            batches_executed,
            dispatch_time_us,
        }
    }

    /// Total rows dispatched through this executor.
    #[must_use]
    pub const fn rows_dispatched(&self) -> u64 {
        self.rows_dispatched
    }

    /// Total logical batches executed by this executor.
    #[must_use]
    pub const fn batches_executed(&self) -> u64 {
        self.batches_executed
    }

    /// Total dispatch time, in microseconds.
    #[must_use]
    pub const fn dispatch_time_us(&self) -> u64 {
        self.dispatch_time_us
    }

    /// Whether all counters are zero.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rows_dispatched == 0 && self.batches_executed == 0 && self.dispatch_time_us == 0
    }

    /// Reset all counters to zero.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Record one completed dispatch batch.
    ///
    /// `rows` may be zero for executors that account for an empty logical
    /// dispatch. Calling this method always increments `batches_executed` by
    /// one. Arithmetic saturates so accounting cannot wrap on long-lived
    /// backends.
    pub fn record_batch(&mut self, rows: u64, elapsed_us: u64) {
        self.rows_dispatched = self.rows_dispatched.saturating_add(rows);
        self.batches_executed = self.batches_executed.saturating_add(1);
        self.dispatch_time_us = self.dispatch_time_us.saturating_add(elapsed_us);
    }

    /// Record one completed dispatch batch using a `Duration`.
    pub fn record_batch_duration(&mut self, rows: u64, elapsed: Duration) {
        self.record_batch(rows, duration_micros_saturating(elapsed));
    }

    /// Merge another metrics value into this one.
    pub fn merge(&mut self, other: Self) {
        self.rows_dispatched = self.rows_dispatched.saturating_add(other.rows_dispatched);
        self.batches_executed = self.batches_executed.saturating_add(other.batches_executed);
        self.dispatch_time_us = self.dispatch_time_us.saturating_add(other.dispatch_time_us);
    }
}

fn duration_micros_saturating(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_metrics_are_zero() {
        let metrics = ExecMetrics::new();

        assert_eq!(metrics.rows_dispatched(), 0);
        assert_eq!(metrics.batches_executed(), 0);
        assert_eq!(metrics.dispatch_time_us(), 0);
        assert!(metrics.is_empty());
    }

    #[test]
    fn record_batch_accumulates_counters() {
        let mut metrics = ExecMetrics::new();

        metrics.record_batch(10, 25);
        metrics.record_batch(5, 75);

        assert_eq!(metrics.rows_dispatched(), 15);
        assert_eq!(metrics.batches_executed(), 2);
        assert_eq!(metrics.dispatch_time_us(), 100);
        assert!(!metrics.is_empty());
    }

    #[test]
    fn zero_row_batch_still_counts_as_batch() {
        let mut metrics = ExecMetrics::new();

        metrics.record_batch(0, 9);

        assert_eq!(metrics.rows_dispatched(), 0);
        assert_eq!(metrics.batches_executed(), 1);
        assert_eq!(metrics.dispatch_time_us(), 9);
    }

    #[test]
    fn reset_clears_all_counters() {
        let mut metrics = ExecMetrics::from_parts(7, 2, 99);

        metrics.reset();

        assert_eq!(metrics, ExecMetrics::new());
    }

    #[test]
    fn merge_uses_saturating_arithmetic() {
        let mut metrics = ExecMetrics::from_parts(u64::MAX - 1, u64::MAX, 10);
        let other = ExecMetrics::from_parts(10, 3, u64::MAX);

        metrics.merge(other);

        assert_eq!(metrics.rows_dispatched(), u64::MAX);
        assert_eq!(metrics.batches_executed(), u64::MAX);
        assert_eq!(metrics.dispatch_time_us(), u64::MAX);
    }

    #[test]
    fn duration_recording_saturates_to_u64() {
        let mut metrics = ExecMetrics::new();

        metrics.record_batch_duration(1, Duration::MAX);

        assert_eq!(metrics.rows_dispatched(), 1);
        assert_eq!(metrics.batches_executed(), 1);
        assert_eq!(metrics.dispatch_time_us(), u64::MAX);
    }
}
