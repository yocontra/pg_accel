//! Typed benchmark measurement records.
//!
//! The runner still aggregates into the historical report structs today. This
//! module defines the lower-level record shape that future runner code can
//! emit directly, preserving details that are currently collapsed during
//! measurement such as cache purge outcome, timing source, and dispatch proof.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use crate::runner::{CacheMode, TimingMode};

/// Timing method used for a recorded measurement value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingSource {
    /// Client-side wall clock around a plain SQL query.
    RawWallClock,
    /// PostgreSQL `EXPLAIN ANALYZE` execution time.
    ExplainAnalyze,
}

impl From<TimingMode> for TimingSource {
    fn from(value: TimingMode) -> Self {
        match value {
            TimingMode::RawWallClock | TimingMode::Both => Self::RawWallClock,
            TimingMode::ExplainAnalyze => Self::ExplainAnalyze,
        }
    }
}

/// Cache state targeted by a measurement iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CacheState {
    /// Warm cache: no OS page-cache purge was requested for this measurement.
    #[default]
    Warm,
    /// Cold cache: the OS page cache was purged before this measurement.
    Cold,
}

impl From<CacheMode> for CacheState {
    fn from(value: CacheMode) -> Self {
        match value {
            CacheMode::Warm | CacheMode::Both => Self::Warm,
            CacheMode::Cold => Self::Cold,
        }
    }
}

/// Outcome of an attempted OS page-cache purge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CachePurgeState {
    /// No purge was requested for this iteration.
    #[default]
    NotRequested,
    /// The purge command completed successfully.
    Completed,
    /// The platform or privileges did not allow a real purge.
    Unavailable,
    /// The purge command failed and the run should be treated as invalid.
    Failed,
}

impl CachePurgeState {
    #[must_use]
    pub const fn from_attempt(requested: bool, result: Result<bool, ()>) -> Self {
        if !requested {
            return Self::NotRequested;
        }
        match result {
            Ok(true) => Self::Completed,
            Ok(false) => Self::Unavailable,
            Err(()) => Self::Failed,
        }
    }
}

/// Which side of a paired benchmark sample was measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuerySide {
    PgAccel,
    PgParallel,
}

/// Evidence used to classify whether pg_accel actually ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DispatchProof {
    /// PostgreSQL selected a `Custom Scan` node in the accel-mode plan.
    pub plan_selected: bool,
    /// Runtime counters showed at least one GPU kernel execution.
    pub kernel_executed: bool,
    /// Function/SRF path used a GPU kernel without a Custom Scan node.
    pub function_srf_kernel: bool,
    /// Optional compact EXPLAIN snippet that supports the classification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explain_snippet: Option<String>,
    /// Optional stats-counter delta that supports the classification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats_delta: Option<u64>,
}

impl DispatchProof {
    #[must_use]
    pub const fn no_dispatch() -> Self {
        Self {
            plan_selected: false,
            kernel_executed: false,
            function_srf_kernel: false,
            explain_snippet: None,
            stats_delta: None,
        }
    }

    #[must_use]
    pub const fn dispatched(&self) -> bool {
        self.kernel_executed || self.function_srf_kernel
    }
}

/// One measured query execution before statistical aggregation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IterationSample {
    pub workload: String,
    pub rows: usize,
    pub iteration_index: usize,
    pub side: QuerySide,
    pub cache_state: CacheState,
    pub cache_purge: CachePurgeState,
    pub timing_source: TimingSource,
    pub elapsed_ms: f64,
    pub dispatch_proof: DispatchProof,
}

impl IterationSample {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workload: impl Into<String>,
        rows: usize,
        iteration_index: usize,
        side: QuerySide,
        cache_state: CacheState,
        cache_purge: CachePurgeState,
        timing_source: TimingSource,
        elapsed_ms: f64,
        dispatch_proof: DispatchProof,
    ) -> Self {
        Self {
            workload: workload.into(),
            rows,
            iteration_index,
            side,
            cache_state,
            cache_purge,
            timing_source,
            elapsed_ms,
            dispatch_proof,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_purge_state_preserves_unavailable() {
        assert_eq!(
            CachePurgeState::from_attempt(true, Ok(false)),
            CachePurgeState::Unavailable
        );
    }

    #[test]
    fn dispatch_proof_distinguishes_plan_from_kernel() {
        assert!(!DispatchProof::no_dispatch().dispatched());

        let proof = DispatchProof {
            plan_selected: true,
            kernel_executed: false,
            function_srf_kernel: false,
            explain_snippet: None,
            stats_delta: Some(0),
        };
        assert!(!proof.dispatched());
        assert!(proof.plan_selected);
    }

    #[test]
    fn iteration_sample_round_trips_json() {
        let sample = IterationSample::new(
            "h3_bulk",
            100_000,
            1,
            QuerySide::PgAccel,
            CacheState::Warm,
            CachePurgeState::NotRequested,
            TimingSource::RawWallClock,
            12.5,
            DispatchProof {
                plan_selected: false,
                kernel_executed: true,
                function_srf_kernel: true,
                explain_snippet: None,
                stats_delta: Some(1),
            },
        );

        let json = serde_json::to_string(&sample).expect("serialize sample");
        let decoded: IterationSample = serde_json::from_str(&json).expect("decode sample");
        assert_eq!(decoded, sample);
    }
}
