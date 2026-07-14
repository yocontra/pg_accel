use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::bench_model::{CachePurgeState, CacheState};
use crate::stats;

const NO_DISPATCH_TIMING_SKEW_THRESHOLD: f64 = 0.10;
const WARMUP_JIT_POST_FIRST_WARN_MS: f64 = 1_000.0;
const WARMUP_JIT_RELATIVE_WARN_RATIO: f64 = 8.0;
pub const NO_DISPATCH_AUDIT_SCHEMA_VERSION: u32 = 1;
pub const RESIDENT_BOUNDARY_AUDIT_SCHEMA_VERSION: u32 = 1;
pub const BENCHMARK_FAILURE_LEDGER_SCHEMA_VERSION: u32 = 1;
const FUNCTION_SRF_GPU_FUNCTIONS: &[&str] = &[
    "h3_latlng_to_cell",
    "h3_grid_disk",
    "h3_grid_ring_unsafe",
    "h3_polyfill",
    "h3_cell_to_children",
    "h3_cell_to_boundary",
    "h3_cells_to_multi_polygon",
];

/// Where a native-decline reason came from. Distinguishes a real planner
/// decision from an unconfirmed static-matrix expectation so the report can
/// never launder an *expected* decline into *verified* evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclineReasonSource {
    /// Sourced from `pg_accel_last_planner_rejection_reason()` — the planner
    /// itself reported why it declined. This is confirmed evidence.
    PlannerReported,
    /// The static benchmark-threshold matrix expected a decline (no Custom
    /// Scan appeared) but the planner surfaced no reason. Unconfirmed: the
    /// report must render it as "expected, unconfirmed" and never treat it as
    /// verified.
    ExpectedUnconfirmed,
}

/// Native-decline evidence for a benchmark row, tagged with its source. The
/// runner only populates the `reason` from a real planner rejection or an
/// unconfirmed static expectation — it never synthesizes plan text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeDeclineEvidence {
    /// The decline reason string.
    pub reason: String,
    /// Whether the reason was reported by the planner or is an unconfirmed
    /// static-matrix expectation.
    pub source: DeclineReasonSource,
}

/// Summary statistics for one cache state (warm or cold) of a workload.
///
/// Populated by [`WorkloadResult::from_iterations`] by partitioning the
/// iteration vector on [`IterationResult::cache_state`]. In `CacheMode::Both`
/// both `warm` and `cold` summaries are populated so the report renders
/// medians / p-values / speedups computed over homogeneous subsamples rather
/// than the pooled bimodal mixture.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CacheModeSummary {
    /// Which cache state this summary describes.
    pub cache_state: CacheState,
    /// Number of measured iterations in this subsample.
    pub n: usize,
    pub accel_mean_ms: f64,
    pub accel_median_ms: f64,
    pub accel_stddev_ms: f64,
    pub accel_p95_ms: f64,
    pub accel_cv_pct: f64,
    pub accel_ci_95: (f64, f64),
    pub parallel_mean_ms: f64,
    pub parallel_median_ms: f64,
    pub parallel_stddev_ms: f64,
    pub parallel_p95_ms: f64,
    pub parallel_cv_pct: f64,
    pub parallel_ci_95: (f64, f64),
    /// `parallel_mean / accel_mean` over this subsample.
    pub speedup_mean_vs_parallel: f64,
    /// `parallel_median / accel_median` over this subsample.
    pub speedup_median_vs_parallel: f64,
    /// Paired t-test p-value over this subsample.
    pub p_value_vs_parallel: f64,
    /// Cohen's d over this subsample.
    pub cohens_d_vs_parallel: f64,
    /// `|d| >= 0.5` over this subsample.
    pub effect_size_meaningful: bool,
}

impl CacheModeSummary {
    /// Build a summary from a homogeneous slice of iterations (all one cache
    /// state). Returns `None` for an empty slice.
    fn from_iterations(cache_state: CacheState, iterations: &[IterationResult]) -> Option<Self> {
        if iterations.is_empty() {
            return None;
        }
        let accel: Vec<f64> = iterations.iter().map(|i| i.accel_ms).collect();
        let parallel: Vec<f64> = iterations.iter().map(|i| i.parallel_ms).collect();
        let accel_mean = stats::mean(&accel);
        let parallel_mean = stats::mean(&parallel);
        let accel_median = stats::median(&accel);
        let parallel_median = stats::median(&parallel);
        let cohens_d = stats::cohens_d(&parallel, &accel);
        Some(Self {
            cache_state,
            n: iterations.len(),
            accel_mean_ms: accel_mean,
            accel_median_ms: accel_median,
            accel_stddev_ms: stats::stddev(&accel),
            accel_p95_ms: stats::percentile(&accel, 95.0),
            accel_cv_pct: stats::cv_percent(&accel),
            accel_ci_95: stats::confidence_interval_95(&accel),
            parallel_mean_ms: parallel_mean,
            parallel_median_ms: parallel_median,
            parallel_stddev_ms: stats::stddev(&parallel),
            parallel_p95_ms: stats::percentile(&parallel, 95.0),
            parallel_cv_pct: stats::cv_percent(&parallel),
            parallel_ci_95: stats::confidence_interval_95(&parallel),
            speedup_mean_vs_parallel: if accel_mean > 0.0 {
                parallel_mean / accel_mean
            } else {
                f64::NAN
            },
            speedup_median_vs_parallel: if accel_median > 0.0 {
                parallel_median / accel_median
            } else {
                f64::NAN
            },
            p_value_vs_parallel: stats::paired_t_test_p(&accel, &parallel),
            cohens_d_vs_parallel: cohens_d,
            effect_size_meaningful: cohens_d.is_finite() && cohens_d.abs() >= 0.5,
        })
    }
}

/// Timing results for a single iteration (two-way: accel vs PG parallel).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)]
pub struct IterationResult {
    /// Execution time in milliseconds with pg_accel enabled.
    pub accel_ms: f64,
    /// Execution time in milliseconds with PG parallel workers (pg_accel off).
    pub parallel_ms: f64,
    /// Outcome of the OS page-cache purge requested for this iteration.
    #[serde(default)]
    pub cache_purge: CachePurgeState,
    /// Cache state this iteration measured under (warm vs cold). Set from the
    /// run's [`CacheMode`](crate::runner::CacheMode). In `CacheMode::Both` this
    /// is the key that separates the cold and warm subsamples so summary stats
    /// are never computed over the bimodal mixture.
    #[serde(default)]
    pub cache_state: CacheState,
}

/// Aggregated results for one workload at one row scale (two-way: accel vs PG parallel).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct WorkloadResult {
    pub name: String,
    pub description: String,
    /// Workload category (`gpu_spatial`, `gpu_h3`, `ssbm`, etc.). Used for
    /// rollup / geomean grouping in the report.
    pub category: String,
    /// Kernel class this workload exercises (`point_in_ring`, `reduce_f32`,
    /// `hashagg`, `h3_latlng`, etc.). Used by the Kernel Coverage table
    /// (action_items W11) to show that 127 workloads exercise only ~9
    /// distinct kernels.
    pub kernel_class: String,
    /// Row count this result was measured at.
    pub rows: usize,
    pub iterations: Vec<IterationResult>,
    /// Whether PostgreSQL selected a pg_accel Custom Scan plan node.
    ///
    /// This is intentionally separate from runtime GPU dispatch: aggregate
    /// wrappers can be selected and still report `GPU Dispatched: false`.
    #[serde(default)]
    pub plan_selected: bool,
    /// Whether a GPU kernel is credited as dispatched for this benchmark row.
    ///
    /// This is true for Custom Scan runtime dispatch and for recognized
    /// function/SRF GPU kernels such as H3 bulk operations.
    #[serde(default)]
    pub gpu_kernel_dispatched: bool,
    /// Whether the dispatch came from a function/SRF GPU kernel rather than a
    /// Custom Scan plan node.
    #[serde(default)]
    pub function_srf_kernel_dispatched: bool,
    /// Derived release-gate flag: no pg_accel Custom Scan was selected and no
    /// function/SRF GPU dispatch was credited for this measured row.
    #[serde(default)]
    pub planner_declined: bool,
    /// Derived release-gate flag: PostgreSQL selected a pg_accel Custom Scan
    /// node, but runtime counters did not prove GPU kernel dispatch.
    #[serde(default)]
    pub custom_scan_selected_not_dispatched: bool,
    /// Derived release-gate count for credited function/SRF kernel executions.
    ///
    /// The harness currently has one kernel counter surface, so this is the
    /// total kernel delta only for rows classified as function/SRF dispatch.
    #[serde(default)]
    pub function_kernel_count: u64,
    /// Derived release-gate alias for accel-side rows returned to PostgreSQL
    /// or the client.
    #[serde(default)]
    pub rows_returned_to_cpu: u64,
    /// Derived release-gate status for GPU-resident pipeline evidence when the
    /// plan artifact exposes it.
    #[serde(default)]
    pub gpu_resident_pipeline: GpuResidentPipelineStatus,
    /// CPU/PostgreSQL boundary that prevents the selected pg_accel plan from
    /// being treated as a GPU-resident pipeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_resident_boundary_reason: Option<String>,
    /// Warmup timings are excluded from statistics but retained in artifacts
    /// so one-time JIT/runtime latency cannot disappear from report evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warmup_iterations: Vec<IterationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup_accel_first_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup_accel_max_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup_accel_post_first_max_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup_parallel_first_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup_parallel_max_ms: Option<f64>,
    /// Whether the harness successfully captured pg_accel runtime counters
    /// around the measured accel-side executions.
    #[serde(default)]
    pub dispatch_counter_captured: bool,
    /// Delta of `pg_accel_kernel_executions()`/`pg_accel_stats()` for the
    /// accel backend across the measured workload. This is the source of
    /// truth for GPU-dispatched wins.
    #[serde(default)]
    pub gpu_kernel_execution_delta: u64,
    /// Delta of `pg_accel_stats().rows_dispatched`.
    #[serde(default)]
    pub pg_accel_rows_dispatched_delta: u64,
    /// Delta of `pg_accel_stats().batches_executed`.
    #[serde(default)]
    pub pg_accel_batches_executed_delta: u64,
    /// Delta of `pg_accel_stats().gpu_rows_processed`.
    #[serde(default)]
    pub pg_accel_gpu_rows_processed_delta: u64,
    /// Delta of `pg_accel_stats().stock_exec_count`; non-zero values are a
    /// red flag for CPU-backed pg_accel plans.
    #[serde(default)]
    pub pg_accel_stock_exec_delta: u64,
    /// Rows returned to the client or reported by EXPLAIN ANALYZE for the
    /// accel-side measured executions. Function/SRF kernel wins require this
    /// to be non-zero so a launched kernel is not counted without output
    /// consumption.
    #[serde(default)]
    pub accel_output_rows_consumed: u64,
    /// Counter-capture failure, when the SQL stats surface was unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_counter_error: Option<String>,
    // -- pg_accel stats --
    pub accel_mean_ms: f64,
    pub accel_stddev_ms: f64,
    pub accel_median_ms: f64,
    pub accel_p25_ms: f64,
    pub accel_p75_ms: f64,
    pub accel_p95_ms: f64,
    pub accel_cv_pct: f64,
    pub accel_ci_95: (f64, f64),
    pub accel_outliers: Vec<usize>,
    pub accel_min_ms: f64,
    pub accel_max_ms: f64,
    // -- PG parallel stats --
    pub parallel_mean_ms: f64,
    pub parallel_stddev_ms: f64,
    pub parallel_median_ms: f64,
    pub parallel_p25_ms: f64,
    pub parallel_p75_ms: f64,
    pub parallel_p95_ms: f64,
    pub parallel_cv_pct: f64,
    pub parallel_ci_95: (f64, f64),
    pub parallel_outliers: Vec<usize>,
    pub parallel_min_ms: f64,
    pub parallel_max_ms: f64,
    // -- Derived --
    /// `parallel_mean / accel_mean`. Values > 1 mean pg_accel is faster than PG parallel.
    pub speedup_vs_parallel: f64,
    /// `parallel_median / accel_median`. Preferred over `speedup_vs_parallel`
    /// for the headline (action_items M12 / Reviewer 1 Sin #19) — median is
    /// robust to the 22% CV cold-start contamination that inflates the mean.
    pub speedup_median_vs_parallel: f64,
    /// Paired t-test p-value: accel vs parallel.
    pub p_value_vs_parallel: f64,
    /// Cohen's d: accel vs parallel. Positive values mean parallel is larger
    /// (i.e. pg_accel is faster). See `stats::cohens_d` for the pooled-sd
    /// formula.
    pub cohens_d_vs_parallel: f64,
    /// `|d| >= 0.5`, i.e. effect size at least "medium" (action_items C9 /
    /// Reviewer 1 Sin #16). The significance gate uses BOTH this AND
    /// Bonferroni-adjusted p, so a row with a 1.2% speedup that is "p = 0"
    /// because of tiny variance is correctly reported as not meaningfully
    /// different.
    pub effect_size_meaningful: bool,
    /// Asymmetric variance ratio = max(cv_accel, cv_baseline) /
    /// min(cv_accel, cv_baseline). Values > 3 indicate the two samples
    /// have very different jitter characteristics and should be reported
    /// with an asymmetric-variance note.
    pub cv_ratio: f64,
    /// Plan diagnostic text captured once per (workload, scale) by the
    /// runner with `pg_accel.enabled = on`. Contains the first few lines of
    /// `EXPLAIN (VERBOSE, COSTS OFF)` — the portion with the Custom Scan
    /// node / GPU Dispatched tags. Used for dispatch classification and
    /// debugging.
    #[serde(default)]
    pub plan_snippet: Option<String>,
    /// Comparable plan diagnostic text captured once per (workload, scale)
    /// with `pg_accel.enabled = off` on the PG-parallel side. Used to flag
    /// no-dispatch rows whose native PostgreSQL plan shape differs from the
    /// accel-mode no-dispatch plan.
    #[serde(default)]
    pub baseline_plan_snippet: Option<String>,
    /// Relative path to the accel-vs-baseline correctness diff artifact for
    /// this workload/scale, when artifact capture was enabled.
    #[serde(default)]
    pub correctness_diff_artifact: Option<String>,
    /// Thermal state captured immediately before this workload ran.
    /// `None` on platforms where capture is unavailable.
    #[serde(default)]
    pub thermal: Option<ThermalState>,
    /// `pg_class`/`pg_stats` diagnostics captured after VACUUM ANALYZE
    /// (action_items C6 / Reviewer 2 §3(iii)). Proves the parallel
    /// baseline's planner had fresh statistics.
    #[serde(default)]
    pub table_stats: Vec<TableStats>,
    /// Benchmark sanity checks captured after setup and before timing.
    /// SSBM uses these to prove dimension filters are non-empty before
    /// no-dispatch rows are interpreted as missing GPU PreAgg work.
    #[serde(default)]
    pub sanity_checks: Vec<SanityCheck>,
    /// Wall-clock time (ms) the accel side spent loading GPU-resident caches
    /// **off** the timed region for this workload, when a resident-cache
    /// loader ran. `None` for workloads with no resident-cache prerequisite.
    /// First-class evidence: the resident lanes pre-load caches outside the
    /// clock while the PG baseline pays scan I/O on the clock, so the report
    /// must be able to surface what the accel side got for free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident_load_ms: Option<f64>,
    /// True when this workload requires a GPU-resident cache preload
    /// (`ssbm_*` / `resident_*` lanes). Marks resident-lane rows distinctly so
    /// the report never folds an off-clock-preloaded win into the headline
    /// geomean without a marker.
    #[serde(default)]
    pub resident_lane: bool,
    /// Native-decline evidence tagged with its source. Populated only from a
    /// real planner rejection (`PlannerReported`) or an unconfirmed static
    /// expectation (`ExpectedUnconfirmed`). Replaces the deleted fabricated
    /// plan-text injection: the report reads this instead of grepping the plan
    /// snippet for a synthetic reason line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_decline_evidence: Option<NativeDeclineEvidence>,
    /// Warm-only summary statistics, populated when the run measured any warm
    /// iterations. In `CacheMode::Both` this is computed over the warm
    /// subsample only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warm_summary: Option<CacheModeSummary>,
    /// Cold-only summary statistics, populated when the run measured any cold
    /// iterations. In `CacheMode::Both` this is computed over the cold
    /// subsample only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cold_summary: Option<CacheModeSummary>,
    /// For cold / both runs: whether the workload fixture fits within
    /// `shared_buffers` (`Some(true)` = data stays resident in shared_buffers
    /// even after the OS page-cache purge, so the "cold" label overstates the
    /// eviction) or exceeds it (`Some(false)` = genuinely cold). `None` when
    /// not a cold run or the comparison could not be made. Recorded so reports
    /// cannot call a run cold when shared_buffers stayed resident.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cold_shared_buffers_resident: Option<bool>,
}

/// Hardware and software profile for reproducibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub cpu: String,
    pub cpu_cores: usize,
    pub memory_gb: String,
}

/// PostgreSQL GUC settings relevant to benchmarks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GucSettings {
    pub settings: Vec<(String, String)>,
}

/// A workload+scale that crashed during benchmarking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrashedScale {
    pub workload: String,
    pub rows: usize,
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repro_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_snippet_artifact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correctness_diff_artifact: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log_tail_artifacts: Vec<String>,
}

/// Full benchmark report containing results for all workloads at all scales.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    pub hardware: Option<HardwareProfile>,
    pub gucs: Option<GucSettings>,
    pub methodology: Methodology,
    pub workloads: Vec<WorkloadResult>,
    /// Directory where report JSON/Markdown/CSV, crash lists, log tails,
    /// plan snippets, and GUC snapshots were persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_dir: Option<String>,
    /// Scales that crashed and were skipped (not included in workloads).
    #[serde(default)]
    pub crashes: Vec<CrashedScale>,
    /// Timestamp from `pg_postmaster_start_time()` at the time the run
    /// started. Proves the observed settings match a specific running
    /// postmaster (action_items C4).
    #[serde(default)]
    pub postmaster_start_time: Option<String>,
}

/// Machine-readable audit for selected pg_accel Custom Scan resident-pipeline
/// evidence in a saved benchmark report.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResidentBoundaryAudit {
    pub schema_version: u32,
    pub selected_custom_scan_rows: usize,
    pub passed_rows: usize,
    pub failed_rows: usize,
    pub ignored_non_custom_scan_rows: usize,
    pub rows: Vec<ResidentBoundaryAuditRow>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResidentBoundaryAuditRow {
    pub workload: String,
    pub rows: usize,
    pub gpu_kernel_dispatched: bool,
    pub gpu_resident_pipeline: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary_reason: Option<String>,
    pub status: ResidentBoundaryAuditStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResidentBoundaryAuditStatus {
    ReportedResidentPipeline,
    NonResidentPipeline,
    MissingResidentPipelineEvidence,
}

impl ResidentBoundaryAuditStatus {
    const fn is_failure(self) -> bool {
        !matches!(self, Self::ReportedResidentPipeline)
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::ReportedResidentPipeline => "reported_resident_pipeline",
            Self::NonResidentPipeline => "non_resident_pipeline",
            Self::MissingResidentPipelineEvidence => "missing_resident_pipeline_evidence",
        }
    }
}

impl ResidentBoundaryAudit {
    #[must_use]
    pub const fn has_failures(&self) -> bool {
        self.failed_rows > 0
    }

    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Resident Boundary Audit\n\n");
        let _ = writeln!(
            out,
            "| Selected Custom Scan rows | Passed | Failed | Ignored non-Custom-Scan rows |"
        );
        let _ = writeln!(out, "|---:|---:|---:|---:|");
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            self.selected_custom_scan_rows,
            self.passed_rows,
            self.failed_rows,
            self.ignored_non_custom_scan_rows
        );
        out.push('\n');
        out.push_str(
            "Every selected pg_accel Custom Scan must carry explicit \
             `GPU Resident Pipeline: true` proof. CPU/PostgreSQL boundary reasons are \
             preserved as diagnostics, but they are failures for SQL-plan admission.\n\n",
        );
        let _ = writeln!(
            out,
            "| Workload | Scale | GPU dispatched | GPU-resident pipeline | Status | Boundary | Detail |"
        );
        let _ = writeln!(out, "|---|---:|---|---|---|---|---|");
        for row in &self.rows {
            let boundary = row.boundary_reason.as_deref().unwrap_or("-");
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} |",
                markdown_cell(&row.workload),
                format_rows(row.rows),
                row.gpu_kernel_dispatched,
                row.gpu_resident_pipeline,
                row.status.as_str(),
                markdown_cell(boundary),
                markdown_cell(&row.detail),
            );
        }
        out
    }
}

/// Machine-readable audit for benchmark rows where pg_accel did not dispatch
/// GPU work and any timing difference is therefore not GPU performance credit.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NoDispatchAudit {
    pub schema_version: u32,
    pub timing_skew_threshold_fraction: f64,
    pub evaluated_no_dispatch_rows: usize,
    pub clean_rows: usize,
    pub warning_rows: usize,
    pub timing_skew_rows: usize,
    pub plan_mismatch_rows: usize,
    pub selected_custom_scan_not_dispatched_rows: usize,
    pub missing_plan_evidence_rows: usize,
    pub ignored_dispatching_rows: usize,
    pub rows: Vec<NoDispatchAuditRow>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NoDispatchAuditRow {
    pub workload: String,
    pub rows: usize,
    pub speedup_median_vs_parallel: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing_skew_fraction: Option<f64>,
    pub timing_skew: bool,
    pub plan_mismatch: bool,
    pub missing_plan_evidence: bool,
    pub accel_plan_signature: String,
    pub baseline_plan_signature: String,
    pub status: NoDispatchAuditStatus,
    pub action: String,
}

/// Machine-readable work queue for every benchmark row that blocks release or
/// still loses to PG-parallel. This intentionally merges multiple failure
/// sources for the same workload/scale into one row so the next engineering
/// task is visible without scraping markdown sections.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BenchmarkFailureLedger {
    pub schema_version: u32,
    pub total_measured_rows: usize,
    pub ship_gate_failure_count: usize,
    pub h3_lane_gate_failure_count: usize,
    pub crash_count: usize,
    pub rows_below_parity_count: usize,
    pub gpu_dispatched_below_parity_count: usize,
    pub native_below_parity_count: usize,
    pub planner_declined_below_parity_count: usize,
    pub stock_fallback_rows: usize,
    pub custom_scan_selected_not_dispatched_rows: usize,
    pub unique_work_items: usize,
    pub rows: Vec<BenchmarkFailureLedgerRow>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BenchmarkFailureLedgerRow {
    pub workload: String,
    pub rows: usize,
    pub category: String,
    pub kernel_class: String,
    pub classification: String,
    pub priority: String,
    pub next_track: String,
    pub sources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speedup_median_vs_parallel: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_floor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accel_median_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_median_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repro_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_snippet_artifact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correctness_diff_artifact: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub log_tail_artifacts: Vec<String>,
    pub details: Vec<String>,
}

impl BenchmarkFailureLedger {
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Benchmark Failure Ledger\n\n");
        out.push_str(
            "Release-blocking gate failures and every measured row still below \
             PostgreSQL-parallel parity. Rows are merged by workload/scale so one \
             benchmark cell has one owner even when it trips multiple checks.\n\n",
        );
        let _ = writeln!(
            out,
            "| Total rows | Ship-gate failures | H3 gate failures | Crashes | Rows below parity | GPU-dispatched below parity | Native below parity | Planner-declined below parity | Stock fallback rows | Custom Scan no-dispatch | Work items |"
        );
        let _ = writeln!(
            out,
            "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
        );
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            self.total_measured_rows,
            self.ship_gate_failure_count,
            self.h3_lane_gate_failure_count,
            self.crash_count,
            self.rows_below_parity_count,
            self.gpu_dispatched_below_parity_count,
            self.native_below_parity_count,
            self.planner_declined_below_parity_count,
            self.stock_fallback_rows,
            self.custom_scan_selected_not_dispatched_rows,
            self.unique_work_items
        );
        out.push('\n');

        let _ = writeln!(
            out,
            "| Priority | Track | Workload | Scale | Sources | Classification | Speedup | Gate | Accel ms | PG ms | Crash artifacts | Detail |"
        );
        let _ = writeln!(
            out,
            "|---|---|---|---:|---|---|---:|---:|---:|---:|---|---|"
        );
        for row in &self.rows {
            let speedup = row
                .speedup_median_vs_parallel
                .map_or_else(|| "-".to_owned(), |value| format!("{value:.2}x"));
            let gate = row
                .gate_floor
                .map_or_else(|| "-".to_owned(), |value| format!("{value:.2}x"));
            let accel = row
                .accel_median_ms
                .map_or_else(|| "-".to_owned(), |value| format!("{value:.2}"));
            let parallel = row
                .parallel_median_ms
                .map_or_else(|| "-".to_owned(), |value| format!("{value:.2}"));
            let sources = row.sources.join(", ");
            let crash_artifacts = ledger_crash_artifact_summary(row);
            let details = row.details.join("<br>");
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                markdown_cell(&row.priority),
                markdown_cell(&row.next_track),
                markdown_cell(&row.workload),
                format_rows(row.rows),
                markdown_cell(&sources),
                markdown_cell(&row.classification),
                speedup,
                gate,
                accel,
                parallel,
                markdown_cell(&crash_artifacts),
                markdown_cell(&details),
            );
        }
        out
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NoDispatchAuditStatus {
    ComparableNative,
    TimingSkew,
    PlanMismatch,
    TimingSkewAndPlanMismatch,
    SelectedCustomScanNotDispatched,
    MissingPlanEvidence,
}

impl NoDispatchAuditStatus {
    const fn is_warning(self) -> bool {
        !matches!(self, Self::ComparableNative)
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::ComparableNative => "comparable_native",
            Self::TimingSkew => "timing_skew",
            Self::PlanMismatch => "plan_mismatch",
            Self::TimingSkewAndPlanMismatch => "timing_skew_and_plan_mismatch",
            Self::SelectedCustomScanNotDispatched => "selected_custom_scan_not_dispatched",
            Self::MissingPlanEvidence => "missing_plan_evidence",
        }
    }
}

impl NoDispatchAudit {
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# No-Dispatch Audit\n\n");
        let _ = writeln!(
            out,
            "| Evaluated no-dispatch rows | Clean | Warnings | Timing skew | Plan mismatch | Selected Custom Scan no-dispatch | Missing plan evidence | Ignored dispatching rows |"
        );
        let _ = writeln!(out, "|---:|---:|---:|---:|---:|---:|---:|---:|");
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            self.evaluated_no_dispatch_rows,
            self.clean_rows,
            self.warning_rows,
            self.timing_skew_rows,
            self.plan_mismatch_rows,
            self.selected_custom_scan_not_dispatched_rows,
            self.missing_plan_evidence_rows,
            self.ignored_dispatching_rows
        );
        out.push('\n');
        let _ = writeln!(
            out,
            "Rows in this audit did not prove pg_accel GPU kernel dispatch. Timing gaps \
             here are harness, planner, cache, or native plan differences until the row is \
             re-run with matching plan evidence and dispatch counters."
        );
        out.push('\n');
        let _ = writeln!(
            out,
            "| Workload | Scale | Median speedup | Timing skew | Plan shape | Missing plan evidence | Accel plan | PG plan | Status | Action |"
        );
        let _ = writeln!(out, "|---|---:|---:|---|---|---|---|---|---|---|");
        for row in &self.rows {
            let timing = row
                .timing_skew_fraction
                .map_or_else(|| "-".to_owned(), |skew| format!("{:.1}%", skew * 100.0));
            let plan_shape = if row.plan_mismatch {
                "DIFF"
            } else if row.accel_plan_signature == "-" || row.baseline_plan_signature == "-" {
                "not captured"
            } else {
                "same"
            };
            let _ = writeln!(
                out,
                "| {} | {} | {:.2}x | {} | {} | {} | `{}` | `{}` | {} | {} |",
                markdown_cell(&row.workload),
                format_rows(row.rows),
                row.speedup_median_vs_parallel,
                timing,
                plan_shape,
                row.missing_plan_evidence,
                markdown_cell(&row.accel_plan_signature),
                markdown_cell(&row.baseline_plan_signature),
                row.status.as_str(),
                markdown_cell(&row.action),
            );
        }
        out
    }
}

/// Methodology metadata for reproducibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Methodology {
    pub iterations: usize,
    pub warmup: usize,
    pub row_scales: Vec<usize>,
    pub ordering: String,
    pub statistical_tests: Vec<String>,
    /// Short label for the timing mode (e.g. `raw-wallclock`,
    /// `explain-analyze`, `both`). Reviewers can see at a glance which
    /// mechanism produced the reported numbers.
    #[serde(default)]
    pub timing_mode: String,
    /// Short label for the cache mode (`warm`, `cold`, `both`).
    #[serde(default)]
    pub cache_mode: String,
    /// Build profile of the benchmark harness itself. Raw wall-clock timing
    /// includes client-side row draining, so debug harness binaries can
    /// materially inflate timings for high-output workloads.
    #[serde(default)]
    pub harness_profile: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct DispatchClassification {
    pub plan_selected: bool,
    pub gpu_kernel_dispatched: bool,
    pub function_srf_kernel_dispatched: bool,
    pub planner_declined: bool,
    pub custom_scan_selected_not_dispatched: bool,
    pub function_kernel_count: u64,
    pub rows_returned_to_cpu: u64,
    pub gpu_resident_pipeline: GpuResidentPipelineStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GpuResidentPipelineStatus {
    /// No explicit GPU-resident pipeline evidence was available in the
    /// captured plan artifacts.
    #[default]
    NotReported,
    /// Captured plan text explicitly reported that the selected path still
    /// crosses a CPU/PostgreSQL boundary before final output.
    NotResident,
    /// Captured plan text exposed explicit fused/PreAgg pipeline evidence.
    Reported,
    /// The row was credited through a function/SRF kernel rather than a
    /// Custom Scan pipeline.
    NotApplicableFunctionSrf,
    /// PostgreSQL planned the query natively, so no pg_accel pipeline exists.
    PlannerDeclined,
    /// A pg_accel Custom Scan was selected but runtime dispatch was not
    /// proven.
    SelectedNotDispatched,
}

impl GpuResidentPipelineStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NotReported => "not_reported",
            Self::NotResident => "not_resident",
            Self::Reported => "reported",
            Self::NotApplicableFunctionSrf => "not_applicable_function_srf",
            Self::PlannerDeclined => "planner_declined",
            Self::SelectedNotDispatched => "selected_not_dispatched",
        }
    }
}

/// Thermal state captured before a workload runs (action_items M13 /
/// Reviewer 1 Sin #18). Used to flag workloads that ran under thermal
/// throttling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalState {
    /// `CPU_Scheduler_Limit` from `pmset -g therm` on macOS (percentage;
    /// < 100 means throttled).
    pub cpu_scheduler_limit: Option<u32>,
    /// `CPU_Speed_Limit` from `pmset -g therm` on macOS (percentage;
    /// < 100 means throttled).
    pub cpu_speed_limit: Option<u32>,
    /// Raw tool output (truncated to ~400 bytes) for archival purposes.
    pub raw: String,
    /// True if either scheduler or speed limit was below 100 at capture.
    pub pressure: bool,
}

/// Post-`VACUUM (ANALYZE)` statistics for a benchmark table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableStats {
    pub relname: String,
    pub relpages: i64,
    pub reltuples: f64,
    /// Max `n_distinct` across all columns in the table. Handy summary of
    /// whether the stats collector thinks the columns are selective.
    pub max_n_distinct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SanityCheck {
    pub label: String,
    pub count: i64,
    pub passed: bool,
}

impl WorkloadResult {
    /// Build aggregated result from raw iterations (two-way: accel vs parallel),
    /// populating kernel class, dispatch status, and per-percentile statistics.
    /// dispatch status, and all per-percentile statistics.
    #[allow(clippy::too_many_arguments)]
    pub fn from_iterations(
        name: String,
        description: String,
        category: String,
        kernel_class: String,
        rows: usize,
        iterations: Vec<IterationResult>,
        gpu_kernel_dispatched: bool,
    ) -> Self {
        // Partition by cache state so summary stats are never computed over a
        // bimodal warm+cold mixture (CacheMode::Both). Each subsample gets its
        // own summary; when both are present the flat top-level fields below
        // describe the WARM subsample (the steady-state headline) rather than
        // the pooled mixture, so nothing downstream can cite a mixed median.
        let warm_iters: Vec<IterationResult> = iterations
            .iter()
            .filter(|i| i.cache_state == CacheState::Warm)
            .cloned()
            .collect();
        let cold_iters: Vec<IterationResult> = iterations
            .iter()
            .filter(|i| i.cache_state == CacheState::Cold)
            .cloned()
            .collect();
        let warm_summary = CacheModeSummary::from_iterations(CacheState::Warm, &warm_iters);
        let cold_summary = CacheModeSummary::from_iterations(CacheState::Cold, &cold_iters);

        // Primary subsample for the flat top-level statistics: when the run
        // mixed warm and cold (Both), use warm only; otherwise use everything
        // (a homogeneous single-mode run).
        let primary: &[IterationResult] = if !warm_iters.is_empty() && !cold_iters.is_empty() {
            &warm_iters
        } else {
            &iterations
        };

        let accel_times: Vec<f64> = primary.iter().map(|i| i.accel_ms).collect();
        let parallel_times: Vec<f64> = primary.iter().map(|i| i.parallel_ms).collect();

        let accel_mean = stats::mean(&accel_times);
        let parallel_mean = stats::mean(&parallel_times);
        let accel_median = stats::median(&accel_times);
        let parallel_median = stats::median(&parallel_times);

        let speedup_vs_parallel = if accel_mean > 0.0 {
            parallel_mean / accel_mean
        } else {
            f64::NAN
        };
        let speedup_median_vs_parallel = if accel_median > 0.0 {
            parallel_median / accel_median
        } else {
            f64::NAN
        };

        let cohens_d = stats::cohens_d(&parallel_times, &accel_times);
        let effect_size_meaningful = cohens_d.is_finite() && cohens_d.abs() >= 0.5;

        let cv_a = stats::cv_percent(&accel_times);
        let cv_p = stats::cv_percent(&parallel_times);
        let cv_ratio = if cv_a.is_finite() && cv_p.is_finite() && cv_a > 0.0 && cv_p > 0.0 {
            cv_a.max(cv_p) / cv_a.min(cv_p)
        } else {
            f64::NAN
        };

        Self {
            name,
            description,
            category,
            kernel_class,
            rows,
            plan_selected: gpu_kernel_dispatched,
            gpu_kernel_dispatched,
            function_srf_kernel_dispatched: false,
            planner_declined: false,
            custom_scan_selected_not_dispatched: false,
            function_kernel_count: 0,
            rows_returned_to_cpu: 0,
            gpu_resident_pipeline: GpuResidentPipelineStatus::NotReported,
            gpu_resident_boundary_reason: None,
            warmup_iterations: Vec::new(),
            warmup_accel_first_ms: None,
            warmup_accel_max_ms: None,
            warmup_accel_post_first_max_ms: None,
            warmup_parallel_first_ms: None,
            warmup_parallel_max_ms: None,
            dispatch_counter_captured: false,
            gpu_kernel_execution_delta: 0,
            pg_accel_rows_dispatched_delta: 0,
            pg_accel_batches_executed_delta: 0,
            pg_accel_gpu_rows_processed_delta: 0,
            pg_accel_stock_exec_delta: 0,
            accel_output_rows_consumed: 0,
            dispatch_counter_error: None,
            accel_mean_ms: accel_mean,
            accel_stddev_ms: stats::stddev(&accel_times),
            accel_median_ms: accel_median,
            accel_p25_ms: stats::percentile(&accel_times, 25.0),
            accel_p75_ms: stats::percentile(&accel_times, 75.0),
            accel_p95_ms: stats::percentile(&accel_times, 95.0),
            accel_cv_pct: cv_a,
            accel_ci_95: stats::confidence_interval_95(&accel_times),
            accel_outliers: stats::detect_outliers(&accel_times, 3.0),
            accel_min_ms: stats::min(&accel_times),
            accel_max_ms: stats::max(&accel_times),
            parallel_mean_ms: parallel_mean,
            parallel_stddev_ms: stats::stddev(&parallel_times),
            parallel_median_ms: parallel_median,
            parallel_p25_ms: stats::percentile(&parallel_times, 25.0),
            parallel_p75_ms: stats::percentile(&parallel_times, 75.0),
            parallel_p95_ms: stats::percentile(&parallel_times, 95.0),
            parallel_cv_pct: cv_p,
            parallel_ci_95: stats::confidence_interval_95(&parallel_times),
            parallel_outliers: stats::detect_outliers(&parallel_times, 3.0),
            parallel_min_ms: stats::min(&parallel_times),
            parallel_max_ms: stats::max(&parallel_times),
            speedup_vs_parallel,
            speedup_median_vs_parallel,
            p_value_vs_parallel: stats::paired_t_test_p(&accel_times, &parallel_times),
            cohens_d_vs_parallel: cohens_d,
            effect_size_meaningful,
            cv_ratio,
            plan_snippet: None,
            baseline_plan_snippet: None,
            correctness_diff_artifact: None,
            thermal: None,
            table_stats: Vec::new(),
            sanity_checks: Vec::new(),
            resident_load_ms: None,
            resident_lane: false,
            native_decline_evidence: None,
            warm_summary,
            cold_summary,
            cold_shared_buffers_resident: None,
            iterations,
        }
    }

    pub fn set_warmup_iterations(&mut self, warmup_iterations: Vec<IterationResult>) {
        self.warmup_accel_first_ms = warmup_iterations
            .first()
            .map(|iteration| iteration.accel_ms);
        self.warmup_accel_max_ms = max_iteration_value(&warmup_iterations, |i| i.accel_ms);
        self.warmup_accel_post_first_max_ms =
            max_iteration_value(warmup_iterations.get(1..).unwrap_or(&[]), |i| i.accel_ms);
        self.warmup_parallel_first_ms = warmup_iterations
            .first()
            .map(|iteration| iteration.parallel_ms);
        self.warmup_parallel_max_ms = max_iteration_value(&warmup_iterations, |i| i.parallel_ms);
        self.warmup_iterations = warmup_iterations;
    }

    #[must_use]
    pub fn dispatch_classification(&self) -> DispatchClassification {
        infer_dispatch_classification(self, None)
    }

    fn dispatch_classification_with_plan_artifact(
        &self,
        full_plan: Option<&str>,
    ) -> DispatchClassification {
        infer_dispatch_classification(self, full_plan)
    }

    fn apply_dispatch_classification(&mut self, classification: DispatchClassification) {
        self.plan_selected = classification.plan_selected;
        self.gpu_kernel_dispatched = classification.gpu_kernel_dispatched;
        self.function_srf_kernel_dispatched = classification.function_srf_kernel_dispatched;
        self.planner_declined = classification.planner_declined;
        self.custom_scan_selected_not_dispatched =
            classification.custom_scan_selected_not_dispatched;
        self.function_kernel_count = classification.function_kernel_count;
        self.rows_returned_to_cpu = classification.rows_returned_to_cpu;
        self.gpu_resident_pipeline = classification.gpu_resident_pipeline;
    }
}

/// Map a workload name to a kernel class (action_items W11 / Reviewer 1
/// Sin #17). Reviewer 1 counts at most 9 distinct GPU kernels across 127
/// workloads.
///
/// This is intentionally a hardcoded table — the mapping is load-bearing
/// benchmark documentation, and individual workload authors shouldn't be
/// able to mislabel themselves into a friendlier kernel class. When a new
/// kernel lands, add it here.
#[must_use]
pub fn classify_kernel(name: &str) -> String {
    const TABLE: &[(&str, &str)] = &[
        // point_in_ring (ST_Intersects, ST_Contains, vertex sweeps, megapoly)
        ("vsweep_", "point_in_ring"),
        ("spatial_mega_", "point_in_ring"),
        ("spatial_filter", "point_in_ring"),
        ("spatial_complex_poly", "point_in_ring"),
        ("spatial_contains", "point_in_ring"),
        ("spatial_multi_pred", "point_in_ring"),
        ("spatial_selectivity", "point_in_ring"),
        ("spatial_sel_", "point_in_ring"),
        ("spatial_concentric", "point_in_ring"),
        ("spatial_star_", "point_in_ring"),
        ("spatial_multihole", "point_in_ring"),
        ("spatial_zigzag", "point_in_ring"),
        ("spatial_shapes", "point_in_ring"),
        ("proximity", "point_in_ring"),
        ("spatial_join", "point_in_ring"),
        ("index_recheck", "point_in_ring"),
        // h3 parent grouped-count has its own fused parent + device-hash path.
        ("h3_cell_to_parent", "h3_cell_to_parent"),
        // h3 latlng / distance / parent variants (shared H3 family)
        ("h3_", "h3_latlng"),
        // reduce kernels
        ("gpu_reduce", "reduce"),
        ("reduce_sum_f32", "reduce"),
        ("reduce_sum_f64", "reduce"),
        ("reduce_f64_sum", "resident_f64_reduce"),
        ("reduce_f64_minmax", "resident_f64_reduce"),
        ("reduce_f64_stats", "resident_f64_reduce"),
        ("reduce_sum_i64", "reduce"),
        ("reduce_min_", "reduce"),
        ("reduce_max_", "reduce"),
        ("reduce_multi", "reduce"),
        // hash aggregation
        ("hashagg_f64_aggs", "resident_f64_grouped_stats"),
        ("gpu_hashagg", "hash_agg"),
        ("hashagg_", "hash_agg"),
        ("grouped_agg", "hash_agg"),
        ("case_when_expression_grouped_agg", "hash_agg"),
        ("case_when_range_expression_grouped_agg", "hash_agg"),
        (
            "case_when_value_predicate_expression_grouped_agg",
            "hash_agg",
        ),
        (
            "case_when_null_predicate_expression_grouped_agg",
            "hash_agg",
        ),
        ("case_when_or_expression_grouped_agg", "hash_agg"),
        ("case_when_in_expression_grouped_agg", "hash_agg"),
        ("case_when_not_expression_grouped_agg", "hash_agg"),
        ("expression_grouped_agg", "hash_agg"),
        ("predicate_filter_expression_grouped_agg", "hash_agg"),
        ("filtered_grouped_agg", "hash_agg"),
        ("timeseries_sensor_rollup", "hash_agg"),
        // sort
        ("large_sort", "sort"),
        ("gpu_sort", "sort"),
        ("sort_int", "sort"),
        ("sort_float", "sort"),
        ("spatial_sort", "sort"),
        ("topk_wide", "sort"),
        // hash join
        ("gpu_hashjoin_filter", "resident_star_groupagg"),
        ("gpu_hashjoin", "hash_join"),
        ("gpu_nlj", "nested_loop_ineq"),
        ("hashjoin_", "hash_join"),
        ("hash_join", "hash_join"),
        // expression eval
        ("gpu_expr", "expr"),
        ("expr_", "expr"),
        // window functions
        ("window_", "window"),
        // raster
        ("raster_", "raster"),
        // mixed, small/oltp — split by common target
        ("mixed_megapoly", "point_in_ring"),
        ("mixed_expr", "expr"),
        ("mixed_join", "resident_star_groupagg"),
        ("mixed_spatial_sort", "sort"),
        ("spatial_agg", "hash_agg"),
        ("oltp_point", "point_in_ring"),
        ("small_table", "unclassified"),
        // SSBM — star-schema resident grouped aggregation over shared proof gates.
        ("ssbm_", "resident_star_groupagg"),
    ];

    let lower = name.to_lowercase();
    for (prefix, class) in TABLE {
        if lower.starts_with(prefix) {
            return (*class).to_owned();
        }
    }
    "unclassified".to_owned()
}

impl HardwareProfile {
    /// Auto-detect the current hardware profile.
    #[must_use]
    pub fn detect() -> Self {
        Self {
            os: detect_os(),
            os_version: detect_os_version(),
            arch: std::env::consts::ARCH.to_owned(),
            cpu: detect_cpu(),
            cpu_cores: detect_cpu_cores(),
            memory_gb: detect_memory(),
        }
    }
}

impl GucSettings {
    /// Query PostgreSQL for benchmark-relevant GUC settings.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection or query fails.
    pub fn from_connection(connection: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let mut client = postgres::Client::connect(connection, postgres::NoTls)?;
        let guc_names = [
            "pg_accel.enabled",
            "pg_accel.gpu_enabled",
            "pg_accel.min_batch_size",
            "pg_accel.kernel_timeout_ms",
            "max_parallel_workers_per_gather",
            "max_parallel_workers",
            "parallel_setup_cost",
            "parallel_tuple_cost",
            "work_mem",
            "shared_buffers",
            "effective_cache_size",
            "server_version",
        ];

        let mut settings = Vec::new();
        for name in &guc_names {
            let query = format!("SHOW {name}");
            if let Ok(Some(row)) = client.query_opt(&query, &[]) {
                let val: String = row.get(0);
                settings.push(((*name).to_owned(), val));
            }
        }
        Ok(Self { settings })
    }
}

/// Format row count for display: 1000 → "1K", 1000000 → "1M".
fn format_rows(rows: usize) -> String {
    match rows {
        r if r >= 1_000_000 && r % 1_000_000 == 0 => format!("{}M", r / 1_000_000),
        r if r >= 1_000 && r % 1_000 == 0 => format!("{}K", r / 1_000),
        r => r.to_string(),
    }
}

/// Format a p-value for the detail table.
///
/// Action_items C10 / Reviewer 1 Sin #20: print ≥6 significant figures in
/// scientific notation (`2.34e-08`, not `0.0000`). For numerical
/// underflow (raw p ≤ 1e-300), print `<1e-300`.
#[must_use]
pub fn format_pvalue(p: f64) -> String {
    if p.is_nan() {
        return "NaN".to_owned();
    }
    if p >= 1.0 {
        return "1.00".to_owned();
    }
    if p <= 1e-300 {
        return "<1e-300".to_owned();
    }
    format!("{p:.6e}")
}

/// Short wins/losses/ties decomposition of the workload set.
#[derive(Debug, Clone, Copy, Default)]
struct SignificanceCounts {
    sig_wins: usize,
    sig_losses: usize,
    total_sig: usize,
    not_significant: usize,
    effect_rejected: usize,
}

/// Compute wins/losses/ties over a slice of workloads, using
/// Bonferroni-corrected p and the |d| >= 0.5 effect-size gate. Crashed
/// scales are accounted for by the caller (they feed the family size but
/// are NOT in this slice).
fn classify_significance(
    workloads: &[&WorkloadResult],
    family_size: usize,
    alpha: f64,
) -> SignificanceCounts {
    let mut c = SignificanceCounts::default();
    for w in workloads {
        let adj = stats::bonferroni_adjusted_p(w.p_value_vs_parallel, family_size);
        let cohens_abs = w.cohens_d_vs_parallel.abs();
        let sig = adj.is_finite() && adj < alpha;
        if !sig {
            c.not_significant += 1;
            continue;
        }
        if cohens_abs < 0.5 {
            // action_items C9: Bonferroni-significant but the effect is
            // not meaningful. Report it but do NOT count it as a win or
            // loss.
            c.effect_rejected += 1;
            continue;
        }
        c.total_sig += 1;
        if w.speedup_vs_parallel > 1.0 {
            c.sig_wins += 1;
        } else {
            c.sig_losses += 1;
        }
    }
    c
}

fn infer_dispatch_classification(
    w: &WorkloadResult,
    full_plan: Option<&str>,
) -> DispatchClassification {
    let snippet = w.plan_snippet.as_deref().unwrap_or_default();
    let plan_selected = w.plan_selected
        || plan_contains_custom_scan_marker(snippet)
        || full_plan.is_some_and(plan_contains_custom_scan_marker);
    let explicit_gpu_dispatch_disabled = plan_explicit_gpu_dispatched(snippet) == Some(false)
        || full_plan.and_then(plan_explicit_gpu_dispatched) == Some(false);
    let counter_capture_known = dispatch_counter_capture_known(w);
    let counter_proves_kernel = w.gpu_kernel_execution_delta > 0;
    let function_candidate = known_function_srf_kernel_workload(w);
    let output_consumed = w.accel_output_rows_consumed > 0;
    let function_srf_kernel_dispatched = function_candidate
        && !explicit_gpu_dispatch_disabled
        && !plan_selected
        && counter_capture_known
        && counter_proves_kernel
        && output_consumed
        && w.pg_accel_stock_exec_delta == 0;

    let custom_scan_gpu_dispatched = counter_capture_known
        && !explicit_gpu_dispatch_disabled
        && counter_proves_kernel
        && (!function_candidate || plan_selected || output_consumed)
        && w.pg_accel_stock_exec_delta == 0;

    let gpu_kernel_dispatched = custom_scan_gpu_dispatched || function_srf_kernel_dispatched;
    let custom_scan_selected_not_dispatched = plan_selected && !gpu_kernel_dispatched;
    let planner_declined = !plan_selected && !function_srf_kernel_dispatched;
    let function_kernel_count = if function_srf_kernel_dispatched {
        w.gpu_kernel_execution_delta
    } else {
        0
    };
    let rows_returned_to_cpu = w.accel_output_rows_consumed;
    let gpu_resident_pipeline = classify_gpu_resident_pipeline_status(
        w,
        full_plan,
        plan_selected,
        gpu_kernel_dispatched,
        function_srf_kernel_dispatched,
        planner_declined,
        custom_scan_selected_not_dispatched,
    );

    DispatchClassification {
        plan_selected,
        gpu_kernel_dispatched,
        function_srf_kernel_dispatched,
        planner_declined,
        custom_scan_selected_not_dispatched,
        function_kernel_count,
        rows_returned_to_cpu,
        gpu_resident_pipeline,
    }
}

#[allow(clippy::fn_params_excessive_bools)]
fn classify_gpu_resident_pipeline_status(
    w: &WorkloadResult,
    full_plan: Option<&str>,
    plan_selected: bool,
    gpu_kernel_dispatched: bool,
    function_srf_kernel_dispatched: bool,
    planner_declined: bool,
    custom_scan_selected_not_dispatched: bool,
) -> GpuResidentPipelineStatus {
    if function_srf_kernel_dispatched {
        return GpuResidentPipelineStatus::NotApplicableFunctionSrf;
    }
    if planner_declined {
        return GpuResidentPipelineStatus::PlannerDeclined;
    }
    if custom_scan_selected_not_dispatched {
        return GpuResidentPipelineStatus::SelectedNotDispatched;
    }
    if !plan_selected || !gpu_kernel_dispatched {
        return GpuResidentPipelineStatus::NotReported;
    }
    let snippet = w.plan_snippet.as_deref().unwrap_or_default();
    let resident_scan_evidence = full_plan
        .map(plan_custom_scan_resident_evidence)
        .filter(|status| *status != CustomScanResidentEvidence::NoCustomScan)
        .unwrap_or_else(|| plan_custom_scan_resident_evidence(snippet));
    match resident_scan_evidence {
        CustomScanResidentEvidence::AllResident => return GpuResidentPipelineStatus::Reported,
        CustomScanResidentEvidence::NonResident => return GpuResidentPipelineStatus::NotResident,
        CustomScanResidentEvidence::MissingEvidence => {}
        CustomScanResidentEvidence::NoCustomScan => {
            if plan_contains_gpu_resident_pipeline_evidence(snippet)
                || full_plan.is_some_and(plan_contains_gpu_resident_pipeline_evidence)
            {
                return GpuResidentPipelineStatus::Reported;
            }
        }
    }
    if plan_gpu_resident_pipeline_value(snippet) == Some(false)
        || full_plan.and_then(plan_gpu_resident_pipeline_value) == Some(false)
        || extract_gpu_resident_boundary_reason(snippet).is_some()
        || full_plan
            .and_then(extract_gpu_resident_boundary_reason)
            .is_some()
        || infer_gpu_resident_boundary_reason_from_plan(snippet).is_some()
        || full_plan
            .and_then(infer_gpu_resident_boundary_reason_from_plan)
            .is_some()
        || infer_gpu_resident_boundary_reason_from_workload(w).is_some()
    {
        return GpuResidentPipelineStatus::NotResident;
    }
    GpuResidentPipelineStatus::NotReported
}

fn known_function_srf_kernel_workload(w: &WorkloadResult) -> bool {
    is_function_kernel_candidate(&w.name, &w.category, &w.kernel_class, &w.description)
}

#[must_use]
pub fn is_function_kernel_candidate(
    name: &str,
    category: &str,
    kernel_class: &str,
    description: &str,
) -> bool {
    let name = name.to_ascii_lowercase();
    let category = category.to_ascii_lowercase();
    let kernel = kernel_class.to_ascii_lowercase();
    name.starts_with("h3_")
        || name.contains("_h3_")
        || name.starts_with("raster_")
        || category == "gpu_h3"
        || category == "gpu_raster"
        || kernel.starts_with("h3")
        || kernel.starts_with("raster")
        || description.to_ascii_lowercase().contains("h3_")
        || description.to_ascii_lowercase().contains("st_mapalgebra")
        || description.to_ascii_lowercase().contains("st_slope")
        || description.to_ascii_lowercase().contains("st_reclass")
}

fn dispatch_counter_capture_known(w: &WorkloadResult) -> bool {
    w.dispatch_counter_captured
        || w.dispatch_counter_error.is_some()
        || w.gpu_kernel_execution_delta > 0
        || w.pg_accel_rows_dispatched_delta > 0
        || w.pg_accel_batches_executed_delta > 0
        || w.pg_accel_gpu_rows_processed_delta > 0
        || w.pg_accel_stock_exec_delta > 0
        || w.accel_output_rows_consumed > 0
}

#[allow(dead_code)]
fn workload_or_plan_mentions_accel_function(w: &WorkloadResult, text: &str) -> bool {
    let haystack = format!(
        "{}\n{}\n{}",
        w.name.to_ascii_lowercase(),
        w.description.to_ascii_lowercase(),
        text.to_ascii_lowercase()
    );
    FUNCTION_SRF_GPU_FUNCTIONS
        .iter()
        .any(|function| haystack.contains(function))
}

fn extract_full_plan_block(plans: &str, workload: &str, rows: usize) -> Option<String> {
    let header = format!("=== {workload} @ rows={rows} ===");
    let start = plans.find(&header)?;
    let body = &plans[start + header.len()..];
    let end = body.find("\n=== ").unwrap_or(body.len());
    let block = body[..end].trim();
    (!block.is_empty()).then(|| block.to_owned())
}

fn no_dispatch_anomaly(w: &WorkloadResult) -> bool {
    both_modes_appear_non_dispatching(w)
        && (no_dispatch_timing_skew(w) || no_dispatch_plan_mismatch(w))
}

fn both_modes_appear_non_dispatching(w: &WorkloadResult) -> bool {
    !w.dispatch_classification().gpu_kernel_dispatched && !baseline_plan_dispatches(w)
}

fn baseline_plan_dispatches(w: &WorkloadResult) -> bool {
    w.baseline_plan_snippet
        .as_deref()
        .is_some_and(plan_contains_custom_scan_marker)
}

fn no_dispatch_timing_skew(w: &WorkloadResult) -> bool {
    if !both_modes_appear_non_dispatching(w) {
        return false;
    }
    timing_skew_fraction(no_dispatch_speedup(w))
        .is_some_and(|skew| skew >= NO_DISPATCH_TIMING_SKEW_THRESHOLD)
}

fn no_dispatch_plan_mismatch(w: &WorkloadResult) -> bool {
    if !both_modes_appear_non_dispatching(w) {
        return false;
    }
    let Some(accel_plan) = w.plan_snippet.as_deref() else {
        return false;
    };
    let Some(baseline_plan) = w.baseline_plan_snippet.as_deref() else {
        return false;
    };
    let accel_sig = plan_shape_signature(accel_plan);
    let baseline_sig = plan_shape_signature(baseline_plan);
    !accel_sig.is_empty() && !baseline_sig.is_empty() && accel_sig != baseline_sig
}

fn no_dispatch_speedup(w: &WorkloadResult) -> f64 {
    if w.speedup_median_vs_parallel.is_finite() && w.speedup_median_vs_parallel > 0.0 {
        w.speedup_median_vs_parallel
    } else {
        w.speedup_vs_parallel
    }
}

fn timing_skew_fraction(speedup: f64) -> Option<f64> {
    if !speedup.is_finite() || speedup <= 0.0 {
        return None;
    }
    Some(if speedup >= 1.0 {
        speedup - 1.0
    } else {
        (1.0 / speedup) - 1.0
    })
}

fn plan_contains_custom_scan_marker(plan: &str) -> bool {
    plan.contains("Custom Scan")
}

fn plan_explicit_gpu_dispatched(plan: &str) -> Option<bool> {
    let lower = plan.to_ascii_lowercase();
    if lower.contains("gpu dispatched: false")
        || lower.contains("gpu kernel dispatched: false")
        || lower.contains("\"gpu dispatched\": false")
        || lower.contains("\"gpu dispatched\":false")
        || lower.contains("\"gpu dispatched\":\"false\"")
        || lower.contains("\"gpu dispatched\": \"false\"")
        || lower.contains("\"gpu kernel dispatched\": false")
        || lower.contains("\"gpu kernel dispatched\":false")
        || lower.contains("\"gpu kernel dispatched\":\"false\"")
        || lower.contains("\"gpu kernel dispatched\": \"false\"")
    {
        Some(false)
    } else if lower.contains("gpu dispatched: true")
        || lower.contains("gpu kernel dispatched: true")
        || lower.contains("\"gpu dispatched\": true")
        || lower.contains("\"gpu dispatched\":true")
        || lower.contains("\"gpu dispatched\":\"true\"")
        || lower.contains("\"gpu dispatched\": \"true\"")
        || lower.contains("\"gpu kernel dispatched\": true")
        || lower.contains("\"gpu kernel dispatched\":true")
        || lower.contains("\"gpu kernel dispatched\":\"true\"")
        || lower.contains("\"gpu kernel dispatched\": \"true\"")
    {
        Some(true)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CustomScanResidentEvidence {
    NoCustomScan,
    AllResident,
    NonResident,
    MissingEvidence,
}

fn plan_custom_scan_resident_evidence(plan: &str) -> CustomScanResidentEvidence {
    let blocks = pg_accel_custom_scan_blocks(plan);
    if blocks.is_empty() {
        return CustomScanResidentEvidence::NoCustomScan;
    }

    let mut missing = false;
    for block in blocks {
        let direct_lines = direct_custom_scan_property_lines(&block);
        let direct_text = direct_lines.join("\n");
        if plan_contains_gpu_resident_pipeline_evidence(&direct_text) {
            continue;
        }
        if plan_gpu_resident_pipeline_value(&direct_text) == Some(false)
            || extract_gpu_resident_boundary_reason(&direct_text).is_some()
            || infer_gpu_resident_boundary_reason_from_plan(&direct_text).is_some()
        {
            return CustomScanResidentEvidence::NonResident;
        }
        missing = true;
    }

    if missing {
        CustomScanResidentEvidence::MissingEvidence
    } else {
        CustomScanResidentEvidence::AllResident
    }
}

fn pg_accel_custom_scan_blocks(plan: &str) -> Vec<Vec<&str>> {
    let lines: Vec<&str> = plan.lines().collect();
    let mut blocks = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start_matches([' ', '-', '>']).trim_start();
        if !(trimmed.starts_with("Custom Scan (") && trimmed.contains("GpuAccel")) {
            continue;
        }
        let depth = custom_scan_leading_indent(line);
        let mut block = vec![*line];
        for next in lines.iter().skip(idx + 1) {
            if next.trim().is_empty() {
                continue;
            }
            let next_depth = custom_scan_leading_indent(next);
            if next_depth <= depth {
                break;
            }
            block.push(*next);
        }
        blocks.push(block);
    }
    blocks
}

fn direct_custom_scan_property_lines<'a>(block: &[&'a str]) -> Vec<&'a str> {
    let Some(parent_depth) = block.first().map(|line| custom_scan_leading_indent(line)) else {
        return Vec::new();
    };
    let direct_depth = block
        .iter()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| custom_scan_leading_indent(line))
        .filter(|depth| *depth > parent_depth)
        .min();
    let Some(direct_depth) = direct_depth else {
        return Vec::new();
    };
    block
        .iter()
        .skip(1)
        .copied()
        .filter(|line| custom_scan_leading_indent(line) == direct_depth)
        .collect()
}

fn custom_scan_leading_indent(line: &str) -> usize {
    line.chars()
        .take_while(|ch| matches!(ch, ' ' | '-' | '>'))
        .count()
}

fn plan_contains_gpu_resident_pipeline_evidence(plan: &str) -> bool {
    if plan_gpu_resident_pipeline_value(plan) == Some(true) {
        return plan_contains_resident_proof_evidence(plan);
    }
    if plan_gpu_resident_pipeline_value(plan) == Some(false) {
        return false;
    }
    let lower = plan.to_ascii_lowercase();
    (lower.contains("gpu resident pipeline: true")
        || lower.contains("gpu-resident pipeline: true")
        || lower.contains("gpu_resident_pipeline: true")
        || lower.contains("\"gpu resident pipeline\": true")
        || lower.contains("\"gpu resident pipeline\":true")
        || lower.contains("\"gpu_resident_pipeline\": true")
        || lower.contains("\"gpu_resident_pipeline\":true")
        || lower.contains("pipeline fusion: true"))
        && plan_contains_resident_proof_evidence(plan)
}

fn plan_contains_resident_proof_evidence(plan: &str) -> bool {
    let version = plan_integer_property(
        plan,
        &["gpu resident proof version", "gpu_resident_proof_version"],
    );
    let stage_mask = plan_integer_property(
        plan,
        &["gpu resident stage mask", "gpu_resident_stage_mask"],
    );
    let device_columns = plan_integer_property(
        plan,
        &["gpu resident device columns", "gpu_resident_device_columns"],
    );
    let operator_class = plan_text_property(
        plan,
        &["gpu resident operator class", "gpu_resident_operator_class"],
    );
    version.is_some_and(|version| version >= 2)
        && operator_class.is_some_and(|class| !class.eq_ignore_ascii_case("unspecified"))
        && stage_mask.is_some_and(|mask| mask > 0)
        && device_columns.is_some_and(|cols| cols > 0)
}

fn threshold_lane_requires_resident_groupagg_logical_spec(lane: &str) -> bool {
    lane.starts_with("resident_dense_groupagg")
        || lane.starts_with("resident_dictionary_groupagg")
        || lane.starts_with("resident_f64_reduce")
        || lane.starts_with("ssbm_q")
        || lane.starts_with("h3_latlng_to_cell_grouped_")
        || lane.starts_with("h3_cell_to_parent_grouped_count_")
        || lane == "hashjoin_filter_groupagg"
}

fn plan_contains_resident_groupagg_logical_evidence(plan: &str) -> bool {
    let operator_class = plan_text_property(
        plan,
        &["gpu resident operator class", "gpu_resident_operator_class"],
    );
    let key = plan_text_property(
        plan,
        &["gpu resident groupagg key", "gpu_resident_groupagg_key"],
    );
    let measure = plan_text_property(
        plan,
        &[
            "gpu resident groupagg measure",
            "gpu_resident_groupagg_measure",
        ],
    );
    let filter = plan_text_property(
        plan,
        &[
            "gpu resident groupagg filter",
            "gpu_resident_groupagg_filter",
        ],
    );
    let predicate_guard = plan_text_property(
        plan,
        &[
            "gpu resident groupagg predicate guard",
            "gpu_resident_groupagg_predicate_guard",
        ],
    );
    let value_predicate = plan_text_property(
        plan,
        &[
            "gpu resident groupagg value predicate",
            "gpu_resident_groupagg_value_predicate",
        ],
    );
    let predicate_ir = plan_text_property(
        plan,
        &[
            "gpu resident groupagg predicate ir",
            "gpu_resident_groupagg_predicate_ir",
        ],
    );
    let aggregate_mask = plan_integer_property(
        plan,
        &[
            "gpu resident groupagg aggregate mask",
            "gpu_resident_groupagg_aggregate_mask",
        ],
    );
    operator_class.is_some_and(|class| class.eq_ignore_ascii_case("resident_groupagg"))
        && key.is_some_and(|key| !key.eq_ignore_ascii_case("unspecified"))
        && measure.is_some_and(|measure| !measure.eq_ignore_ascii_case("unspecified"))
        && filter.is_some_and(|filter| !filter.eq_ignore_ascii_case("unspecified"))
        && predicate_guard.is_some_and(|guard| !guard.eq_ignore_ascii_case("unspecified"))
        && value_predicate.is_some_and(|predicate| !predicate.eq_ignore_ascii_case("unspecified"))
        && predicate_ir.is_some_and(|ir| !ir.eq_ignore_ascii_case("unspecified"))
        && aggregate_mask.is_some_and(|mask| mask > 0)
}

fn plan_integer_property(plan: &str, keys: &[&str]) -> Option<i64> {
    for line in plan.lines() {
        let trimmed = line.trim().trim_end_matches(',');
        let lower = trimmed.to_ascii_lowercase();
        for key in keys {
            let quoted_key = format!("\"{key}\"");
            let value = if lower.starts_with(key) {
                trimmed.split_once(':').map(|(_, value)| value)
            } else if lower.starts_with(&quoted_key) {
                trimmed.split_once(':').map(|(_, value)| value)
            } else {
                None
            };
            if let Some(value) = value.and_then(parse_plan_integer_value) {
                return Some(value);
            }
        }
    }
    json_integer_property(plan, keys)
}

fn plan_text_property(plan: &str, keys: &[&str]) -> Option<String> {
    for line in plan.lines() {
        let trimmed = line.trim().trim_end_matches(',');
        let lower = trimmed.to_ascii_lowercase();
        for key in keys {
            let quoted_key = format!("\"{key}\"");
            let value = if lower.starts_with(key) {
                trimmed.split_once(':').map(|(_, value)| value)
            } else if lower.starts_with(&quoted_key) {
                trimmed.split_once(':').map(|(_, value)| value)
            } else {
                None
            };
            if let Some(value) = value.and_then(parse_plan_text_value) {
                return Some(value);
            }
        }
    }
    json_string_property(plan, keys)
}

fn parse_plan_integer_value(value: &str) -> Option<i64> {
    value
        .trim()
        .trim_matches('"')
        .trim_end_matches('"')
        .trim()
        .parse()
        .ok()
}

fn parse_plan_text_value(value: &str) -> Option<String> {
    let value = value
        .trim()
        .trim_matches('"')
        .trim_end_matches('"')
        .trim()
        .to_owned();
    (!value.is_empty()).then_some(value)
}

fn json_bool_property(plan: &str, keys: &[&str]) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_str(plan).ok()?;
    match json_find_property(&value, keys)? {
        serde_json::Value::Bool(value) => Some(*value),
        serde_json::Value::String(value) if value.eq_ignore_ascii_case("true") => Some(true),
        serde_json::Value::String(value) if value.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

fn json_integer_property(plan: &str, keys: &[&str]) -> Option<i64> {
    let value: serde_json::Value = serde_json::from_str(plan).ok()?;
    match json_find_property(&value, keys)? {
        serde_json::Value::Number(value) => value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok())),
        serde_json::Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn json_string_property(plan: &str, keys: &[&str]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(plan).ok()?;
    match json_find_property(&value, keys)? {
        serde_json::Value::String(value) => Some(value.clone()),
        other => Some(other.to_string()),
    }
}

fn json_find_property<'a>(
    value: &'a serde_json::Value,
    keys: &[&str],
) -> Option<&'a serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let key = key.to_ascii_lowercase();
                if keys.iter().any(|expected| key == *expected) {
                    return Some(child);
                }
            }
            map.values()
                .find_map(|child| json_find_property(child, keys))
        }
        serde_json::Value::Array(values) => values
            .iter()
            .find_map(|child| json_find_property(child, keys)),
        _ => None,
    }
}

fn plan_gpu_resident_pipeline_value(plan: &str) -> Option<bool> {
    if let Some(value) =
        json_bool_property(plan, &["gpu resident pipeline", "gpu_resident_pipeline"])
    {
        return Some(value);
    }
    let lower = plan.to_ascii_lowercase();
    if lower.contains("gpu resident pipeline: false")
        || lower.contains("gpu-resident pipeline: false")
        || lower.contains("gpu_resident_pipeline: false")
        || lower.contains("\"gpu resident pipeline\": false")
        || lower.contains("\"gpu resident pipeline\":false")
        || lower.contains("\"gpu resident pipeline\":\"false\"")
        || lower.contains("\"gpu resident pipeline\": \"false\"")
        || lower.contains("\"gpu_resident_pipeline\": false")
        || lower.contains("\"gpu_resident_pipeline\":false")
        || lower.contains("\"gpu_resident_pipeline\":\"false\"")
        || lower.contains("\"gpu_resident_pipeline\": \"false\"")
    {
        Some(false)
    } else if lower.contains("gpu resident pipeline: true")
        || lower.contains("gpu-resident pipeline: true")
        || lower.contains("gpu_resident_pipeline: true")
        || lower.contains("\"gpu resident pipeline\": true")
        || lower.contains("\"gpu resident pipeline\":true")
        || lower.contains("\"gpu resident pipeline\":\"true\"")
        || lower.contains("\"gpu resident pipeline\": \"true\"")
        || lower.contains("\"gpu_resident_pipeline\": true")
        || lower.contains("\"gpu_resident_pipeline\":true")
        || lower.contains("\"gpu_resident_pipeline\":\"true\"")
        || lower.contains("\"gpu_resident_pipeline\": \"true\"")
    {
        Some(true)
    } else {
        None
    }
}

fn extract_gpu_resident_boundary_reason(plan: &str) -> Option<String> {
    if let Some(value) = json_string_property(
        plan,
        &[
            "gpu resident boundary",
            "gpu_resident_boundary",
            "gpu_resident_boundary_reason",
        ],
    ) {
        return Some(value.trim_matches('"').to_owned());
    }
    for line in plan.lines() {
        let trimmed = line.trim().trim_end_matches(',');
        let lower = trimmed.to_ascii_lowercase();
        let value = if lower.starts_with("gpu resident boundary:") {
            trimmed.split_once(':').map(|(_, value)| value)
        } else if lower.starts_with("\"gpu resident boundary\":") {
            trimmed.split_once(':').map(|(_, value)| value)
        } else if lower.starts_with("\"gpu_resident_boundary_reason\":") {
            trimmed.split_once(':').map(|(_, value)| value)
        } else {
            None
        };
        if let Some(value) = value {
            let reason = value
                .trim()
                .trim_matches('"')
                .trim_end_matches('"')
                .trim()
                .to_owned();
            if !reason.is_empty() {
                return Some(reason);
            }
        }
    }
    None
}

fn classify_gpu_resident_boundary_reason(
    w: &WorkloadResult,
    full_plan: Option<&str>,
) -> Option<String> {
    if matches!(
        w.gpu_resident_pipeline,
        GpuResidentPipelineStatus::Reported
            | GpuResidentPipelineStatus::PlannerDeclined
            | GpuResidentPipelineStatus::NotApplicableFunctionSrf
    ) {
        return None;
    }
    w.plan_snippet
        .as_deref()
        .and_then(extract_gpu_resident_boundary_reason)
        .or_else(|| full_plan.and_then(extract_gpu_resident_boundary_reason))
        .or_else(|| {
            w.plan_snippet
                .as_deref()
                .and_then(infer_gpu_resident_boundary_reason_from_plan)
        })
        .or_else(|| full_plan.and_then(infer_gpu_resident_boundary_reason_from_plan))
        .or_else(|| infer_gpu_resident_boundary_reason_from_workload(w).map(str::to_owned))
}

fn infer_gpu_resident_boundary_reason_from_plan(plan: &str) -> Option<String> {
    let lower = plan.to_ascii_lowercase();
    let strategy = if lower.contains("strategy: gpuscan")
        || lower.contains("\"strategy\": \"gpuscan\"")
        || lower.contains("\"strategy\":\"gpuscan\"")
    {
        "GpuScan"
    } else if lower.contains("strategy: gpujoin")
        || lower.contains("\"strategy\": \"gpujoin\"")
        || lower.contains("\"strategy\":\"gpujoin\"")
    {
        "GpuJoin"
    } else if lower.contains("strategy: gpuagg")
        || lower.contains("\"strategy\": \"gpuagg\"")
        || lower.contains("\"strategy\":\"gpuagg\"")
    {
        "GpuAgg"
    } else if lower.contains("strategy: gpusort")
        || lower.contains("\"strategy\": \"gpusort\"")
        || lower.contains("\"strategy\":\"gpusort\"")
    {
        "GpuSort"
    } else if lower.contains("strategy: gpuwindow")
        || lower.contains("\"strategy\": \"gpuwindow\"")
        || lower.contains("\"strategy\":\"gpuwindow\"")
    {
        "GpuWindow"
    } else if lower.contains("strategy: gpupreagg")
        || lower.contains("\"strategy\": \"gpupreagg\"")
        || lower.contains("\"strategy\":\"gpupreagg\"")
    {
        "GpuPreAgg"
    } else if lower.contains("strategy: gpufunctionscan")
        || lower.contains("\"strategy\": \"gpufunctionscan\"")
        || lower.contains("\"strategy\":\"gpufunctionscan\"")
    {
        "GpuFunctionScan"
    } else if lower.contains("strategy: gpuaccelsrftargetlist")
        || lower.contains("\"strategy\": \"gpuaccelsrftargetlist\"")
        || lower.contains("\"strategy\":\"gpuaccelsrftargetlist\"")
    {
        "GpuAccelSrfTargetList"
    } else {
        return None;
    };
    gpu_resident_boundary_reason_for_strategy(strategy).map(str::to_owned)
}

fn infer_gpu_resident_boundary_reason_from_workload(w: &WorkloadResult) -> Option<&'static str> {
    let name = w.name.as_str();
    let category = w.category.as_str();
    let kernel = w.kernel_class.as_str();
    if kernel == "sort" || name.contains("sort") || name.contains("topk") {
        gpu_resident_boundary_reason_for_strategy("GpuSort")
    } else if kernel.contains("join") || name.contains("join") || name.contains("nlj") {
        gpu_resident_boundary_reason_for_strategy("GpuJoin")
    } else if kernel == "hash_agg"
        || kernel == "reduce"
        || name.contains("agg")
        || name.contains("reduce")
    {
        gpu_resident_boundary_reason_for_strategy("GpuAgg")
    } else if kernel == "window" || name.contains("window") {
        gpu_resident_boundary_reason_for_strategy("GpuWindow")
    } else if category == "gpu_spatial"
        || category == "gpu_raster"
        || category == "gpu_h3"
        || category == "gpu_expr"
    {
        gpu_resident_boundary_reason_for_strategy("GpuScan")
    } else {
        None
    }
}

pub fn gpu_resident_boundary_reason_for_strategy(strategy: &str) -> Option<&'static str> {
    Some(match strategy {
        "GpuScan" => {
            "GpuScan consumes heap or child tuples on CPU via table_scan_getnextslot/ExecProcNode/MinimalTuple staging and emits PostgreSQL slots"
        }
        "GpuJoin" => {
            "GpuJoin collects child rows through ExecProcNode into host MinimalTuple/key buffers and reconstructs joined PostgreSQL slots"
        }
        "GpuAgg" => {
            "GpuAgg drains heap or child tuples on CPU and stages host input/key/value buffers before GPU reduce or grouped aggregation"
        }
        "GpuSort" => {
            "GpuSort materializes input tuples on CPU, sends key vectors only, reorders host MinimalTuples, and emits PostgreSQL slots"
        }
        "GpuWindow" => {
            "GpuWindow buffers input MinimalTuples, extracts host columns, stores host result vectors, and emits PostgreSQL slots"
        }
        "GpuPreAgg" => {
            "GpuPreAgg materializes dimensions in host HashMap state and scans/probes fact rows through ExecProcNode/materialized slots"
        }
        "GpuFunctionScan" => {
            "GpuFunctionScan dispatches constant arguments once, buffers host Datums, and drains output through PostgreSQL slots"
        }
        "GpuAccelSrfTargetList" => {
            "GpuAccelSrfTargetList drives ProjectSet input through ExecProcNode, buffers per-row SRF output, and emits expanded PostgreSQL tuples"
        }
        _ => return None,
    })
}

fn plan_shape_signature(plan: &str) -> Vec<String> {
    plan.lines().filter_map(normalize_plan_line).collect()
}

fn first_plan_signature_line(plan: Option<&str>) -> String {
    plan.and_then(|p| plan_shape_signature(p).into_iter().next())
        .unwrap_or_else(|| "-".to_owned())
}

fn normalize_plan_line(line: &str) -> Option<String> {
    let mut trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("->") {
        trimmed = rest.trim_start();
    }
    if trimmed.is_empty() {
        return None;
    }

    let mut lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("planning time:")
        || lower.starts_with("execution time:")
        || lower.starts_with("buffers:")
        || lower.starts_with("pg_accel planner rejection reason:")
    {
        return None;
    }

    let mut truncate_at = lower.len();
    for marker in ["  (cost=", " (cost=", "  (actual", " (actual"] {
        if let Some(idx) = lower.find(marker) {
            truncate_at = truncate_at.min(idx);
        }
    }
    lower.truncate(truncate_at);

    let normalized = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

fn markdown_cell(input: &str) -> String {
    input.replace('|', "\\|").replace('\n', "<br>")
}

fn max_iteration_value(
    iterations: &[IterationResult],
    value: impl Fn(&IterationResult) -> f64,
) -> Option<f64> {
    iterations
        .iter()
        .map(value)
        .filter(|v| v.is_finite())
        .reduce(f64::max)
}

fn format_optional_ms(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |v| format!("{v:.2}ms"))
}

/// Maximum number of plan-snippet lines shown in a rendered report. This is
/// a display-only cap: `WorkloadResult::plan_snippet` itself always holds
/// the FULL captured `EXPLAIN (VERBOSE, COSTS OFF)` text, and every
/// classification helper (dispatch detection, decline-reason extraction,
/// plan-shape comparison, etc.) reads that full text directly. Only the
/// markdown renderer truncates, so a deep plan tree cannot blow up the
/// rendered report while a Custom Scan node past line 30 is still correctly
/// classified.
const PLAN_SNIPPET_DISPLAY_MAX_LINES: usize = 30;

/// Truncate a full plan snippet for markdown display only. See
/// `PLAN_SNIPPET_DISPLAY_MAX_LINES`. Never call this before classification —
/// classification helpers must keep reading `WorkloadResult::plan_snippet`
/// untouched.
fn truncate_plan_for_display(plan: &str) -> String {
    let total = plan.lines().count();
    if total <= PLAN_SNIPPET_DISPLAY_MAX_LINES {
        return plan.to_owned();
    }
    let mut truncated = plan
        .lines()
        .take(PLAN_SNIPPET_DISPLAY_MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let _ = write!(
        truncated,
        "\n… ({} more lines)",
        total - PLAN_SNIPPET_DISPLAY_MAX_LINES
    );
    truncated
}

/// Render the per-row evidence appendix for the Detailed Results section:
/// warm/cold summaries (never a pooled median), the resident-cache preload
/// note, the cold-cache `shared_buffers`-resident warning, native-decline
/// evidence tagged with its source, and a truncated plan-snippet preview.
/// Emits nothing when none of these apply to the row.
fn render_workload_evidence_notes(w: &WorkloadResult, scale_label: &str, out: &mut String) {
    let mut notes: Vec<String> = Vec::new();

    if let (Some(warm), Some(cold)) = (&w.warm_summary, &w.cold_summary) {
        notes.push(format!(
            "Warm (n={}): accel median {:.2}ms, PG parallel median {:.2}ms, speedup {:.2}x.",
            warm.n, warm.accel_median_ms, warm.parallel_median_ms, warm.speedup_median_vs_parallel
        ));
        notes.push(format!(
            "Cold (n={}): accel median {:.2}ms, PG parallel median {:.2}ms, speedup {:.2}x.",
            cold.n, cold.accel_median_ms, cold.parallel_median_ms, cold.speedup_median_vs_parallel
        ));
    }

    if w.resident_lane {
        notes.push(w.resident_load_ms.map_or_else(
            || {
                "Resident-cache preload: required for this lane but not captured for this row."
                    .to_owned()
            },
            |ms| {
                format!(
                    "Resident-cache preload: {ms:.2}ms, paid once off the timed clock and \
                     amortized across repeated queries against the resident cache."
                )
            },
        ));
    }

    if w.cold_shared_buffers_resident == Some(true) {
        notes.push(
            "WARNING: this cold-cache fixture fit inside `shared_buffers`; PostgreSQL's buffer \
             cache stayed resident after the OS page-cache purge, so these \"cold\" numbers \
             reflect a shared_buffers-resident fixture, not genuine cold I/O."
                .to_owned(),
        );
    }

    if let Some(evidence) = &w.native_decline_evidence {
        let confirmation = match evidence.source {
            DeclineReasonSource::PlannerReported => "planner-reported, verified",
            DeclineReasonSource::ExpectedUnconfirmed => "expected, unconfirmed",
        };
        notes.push(format!(
            "Native-decline evidence: `{}` ({confirmation}).",
            evidence.reason
        ));
    }

    // Nothing noteworthy for this row (no cache split, no resident lane, no
    // cold-buffer warning, no decline evidence) — skip. The full plan
    // snippet is only worth dumping alongside evidence that needs it; the
    // Dispatch Evidence By Row table already carries a one-line plan
    // signature for every row.
    if notes.is_empty() {
        return;
    }

    if w.plan_snippet.is_some() {
        notes.push(format!(
            "Plan snippet (accel, display-truncated to {PLAN_SNIPPET_DISPLAY_MAX_LINES} lines):"
        ));
    }

    let _ = writeln!(out, "- **{scale_label}:**");
    for note in &notes {
        let _ = writeln!(out, "  - {note}");
    }
    if let Some(plan) = &w.plan_snippet {
        let _ = writeln!(out, "\n```\n{}\n```\n", truncate_plan_for_display(plan));
    }
}

fn csv_optional_f64(value: Option<f64>) -> String {
    value.map_or_else(String::new, |v| format!("{v:.4}"))
}

fn iteration_indexes_with_cache_purge(
    iterations: &[IterationResult],
    state: CachePurgeState,
) -> Vec<usize> {
    iterations
        .iter()
        .enumerate()
        .filter_map(|(idx, iteration)| (iteration.cache_purge == state).then_some(idx + 1))
        .collect()
}

fn format_iteration_indexes(indexes: &[usize]) -> String {
    if indexes.is_empty() {
        "-".to_owned()
    } else {
        indexes
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn dispatch_evidence_label(w: &WorkloadResult) -> &'static str {
    if w.function_srf_kernel_dispatched {
        "function_srf_dispatch"
    } else if w.custom_scan_selected_not_dispatched {
        "custom_scan_selected_not_dispatched"
    } else if w.planner_declined {
        "planner_declined"
    } else if w.gpu_kernel_dispatched {
        "custom_scan_dispatch"
    } else {
        "uncredited_gpu_counter_or_unknown"
    }
}

fn threshold_matrix_expectation_label(
    expectation: crate::workloads::BenchmarkLaneExpectation,
) -> String {
    match expectation {
        crate::workloads::BenchmarkLaneExpectation::GpuWinner { min_warm_speedup } => {
            format!("{} >= {min_warm_speedup:.2}x", expectation.label())
        }
        crate::workloads::BenchmarkLaneExpectation::NativeDecline { reason } => {
            let reason = expectation.decline_reason().unwrap_or(reason);
            format!("{} ({reason})", expectation.label())
        }
    }
}

fn threshold_matrix_status(
    w: &WorkloadResult,
    lane: &str,
    expectation: crate::workloads::BenchmarkLaneExpectation,
    cache_mode: &str,
) -> &'static str {
    match expectation {
        crate::workloads::BenchmarkLaneExpectation::GpuWinner { min_warm_speedup } => {
            if gpu_winner_evidence_verified(w, lane, min_warm_speedup, cache_mode) {
                "pass"
            } else {
                "FAIL"
            }
        }
        crate::workloads::BenchmarkLaneExpectation::NativeDecline { reason } => {
            if !w.plan_selected
                && !w.gpu_kernel_dispatched
                && native_decline_reason_verified(w, reason)
            {
                "pass"
            } else {
                "FAIL"
            }
        }
    }
}

fn warmup_jit_audit_reason(w: &WorkloadResult) -> Option<String> {
    let post_first_max = w.warmup_accel_post_first_max_ms?;
    let mut reasons = Vec::new();
    if post_first_max >= WARMUP_JIT_POST_FIRST_WARN_MS {
        reasons.push(format!(
            "post-first warmup max {post_first_max:.2}ms >= {WARMUP_JIT_POST_FIRST_WARN_MS:.0}ms"
        ));
    }
    if w.accel_median_ms.is_finite() && w.accel_median_ms > 0.0 {
        let ratio = post_first_max / w.accel_median_ms;
        if ratio >= WARMUP_JIT_RELATIVE_WARN_RATIO && post_first_max >= 100.0 {
            reasons.push(format!(
                "post-first warmup max is {ratio:.1}x measured accel median"
            ));
        }
    }
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

/// Warm-subsample speedup used by the ship gates below. Reads
/// `WorkloadResult::warm_summary` directly when the run measured a cache
/// split (`CacheMode::Both`); falls back to the flat top-level
/// `speedup_median_vs_parallel` for warm-only runs, where the flat field
/// already describes the same (only) subsample. This is deliberately
/// explicit rather than relying on the flat field's documented "describes
/// the warm subsample" invariant — the gate should read the warm evidence
/// directly, not through an implicit contract on an unrelated field.
fn warm_speedup_for_gate(w: &WorkloadResult) -> f64 {
    w.warm_summary
        .as_ref()
        .map_or(w.speedup_median_vs_parallel, |s| {
            s.speedup_median_vs_parallel
        })
}

fn gpu_winner_evidence_verified(
    w: &WorkloadResult,
    lane: &str,
    min_warm_speedup: f64,
    cache_mode: &str,
) -> bool {
    let classification = w.dispatch_classification();
    let threshold = min_warm_speedup.max(BENCHMARK_SHIP_GATE_MIN_SPEEDUP);
    let warm_speedup = warm_speedup_for_gate(w);
    classification.gpu_kernel_dispatched
        && w.dispatch_counter_captured
        && w.gpu_kernel_execution_delta > 0
        && classification.rows_returned_to_cpu > 0
        && warm_speedup.is_finite()
        && warm_speedup >= threshold
        && operation_cache_gate_verified(w, cache_mode)
        && (!threshold_lane_requires_resident_groupagg_logical_spec(lane)
            || w.plan_snippet
                .as_deref()
                .is_some_and(plan_contains_resident_groupagg_logical_evidence))
}

/// Whether native-decline evidence for this row is *verified* — i.e. safe to
/// pass a ship gate. Reads `WorkloadResult::native_decline_evidence` only:
/// only `DeclineReasonSource::PlannerReported` (a real
/// `pg_accel_last_planner_rejection_reason()` decline captured by the
/// runner) counts as verified. `DeclineReasonSource::ExpectedUnconfirmed`
/// (the static benchmark-threshold matrix expected a decline but the
/// planner surfaced no reason) must never pass — the report renders it as
/// "expected, unconfirmed" instead (see `native_decline_evidence_label`).
///
/// This deliberately does not grep `plan_snippet` for any decline-reason
/// text. The runner never synthesizes plan text for an unconfirmed
/// expectation (see `runner::native_decline_evidence`), so grepping the
/// plan snippet can no longer distinguish a real decline from an assumed
/// one — the tagged evidence field is the only source of truth.
fn native_decline_reason_verified(w: &WorkloadResult, reason: &str) -> bool {
    matches!(
        &w.native_decline_evidence,
        Some(NativeDeclineEvidence {
            reason: actual,
            source: DeclineReasonSource::PlannerReported,
        }) if actual == reason
    )
}

/// Render the native-decline evidence for a row, tagged with its
/// confirmation status, for markdown display. `ExpectedUnconfirmed` renders
/// as "expected, unconfirmed" so a reader never mistakes a static-matrix
/// assumption for a proven planner decision.
fn native_decline_evidence_label(w: &WorkloadResult) -> String {
    match &w.native_decline_evidence {
        Some(NativeDeclineEvidence {
            reason,
            source: DeclineReasonSource::PlannerReported,
        }) => format!("{reason} (planner-reported, verified)"),
        Some(NativeDeclineEvidence {
            reason,
            source: DeclineReasonSource::ExpectedUnconfirmed,
        }) => format!("{reason} (expected, unconfirmed)"),
        None => "-".to_owned(),
    }
}

fn operation_cache_gate_required(name: &str) -> bool {
    name.starts_with("h3_") || name.starts_with("raster_")
}

/// Verifies the h3_/raster_ cache-mode-both requirement. Requires both that
/// the methodology reports `cache_mode == "both"` AND that this row actually
/// carries a populated `warm_summary` — i.e. real warm-only medians were
/// computed for this row, not merely that the run was globally configured
/// for cache-mode both. A methodology string alone does not prove any given
/// row was measured under both cache states.
fn operation_cache_gate_verified(w: &WorkloadResult, cache_mode: &str) -> bool {
    !operation_cache_gate_required(&w.name)
        || (cache_mode.trim().eq_ignore_ascii_case("both") && w.warm_summary.is_some())
}

fn no_dispatch_audit_action(w: &WorkloadResult) -> &'static str {
    let classification = w.dispatch_classification();
    if classification.custom_scan_selected_not_dispatched {
        "quarantine CPU fallback / planner-decline"
    } else if no_dispatch_plan_mismatch(w) {
        "align native plan/GUCs before comparing"
    } else if no_dispatch_timing_skew(w) {
        "same native plan timing skew; rerun/inspect harness"
    } else if w.plan_snippet.is_none() || w.baseline_plan_snippet.is_none() {
        "capture accel and baseline plan evidence before comparing"
    } else {
        "no GPU credit; use only as native-decline/stability evidence"
    }
}

fn no_dispatch_audit_row(w: &WorkloadResult) -> Option<NoDispatchAuditRow> {
    if !both_modes_appear_non_dispatching(w) {
        return None;
    }

    let skew_fraction = timing_skew_fraction(no_dispatch_speedup(w));
    let timing_skew = skew_fraction.is_some_and(|skew| skew >= NO_DISPATCH_TIMING_SKEW_THRESHOLD);
    let plan_mismatch = no_dispatch_plan_mismatch(w);
    let missing_plan_evidence = w.plan_snippet.is_none() || w.baseline_plan_snippet.is_none();
    let classification = w.dispatch_classification();
    let status = if classification.custom_scan_selected_not_dispatched {
        NoDispatchAuditStatus::SelectedCustomScanNotDispatched
    } else if timing_skew && plan_mismatch {
        NoDispatchAuditStatus::TimingSkewAndPlanMismatch
    } else if plan_mismatch {
        NoDispatchAuditStatus::PlanMismatch
    } else if timing_skew {
        NoDispatchAuditStatus::TimingSkew
    } else if missing_plan_evidence {
        NoDispatchAuditStatus::MissingPlanEvidence
    } else {
        NoDispatchAuditStatus::ComparableNative
    };

    Some(NoDispatchAuditRow {
        workload: w.name.clone(),
        rows: w.rows,
        speedup_median_vs_parallel: no_dispatch_speedup(w),
        timing_skew_fraction: skew_fraction,
        timing_skew,
        plan_mismatch,
        missing_plan_evidence,
        accel_plan_signature: first_plan_signature_line(w.plan_snippet.as_deref()),
        baseline_plan_signature: first_plan_signature_line(w.baseline_plan_snippet.as_deref()),
        status,
        action: no_dispatch_audit_action(w).to_owned(),
    })
}

fn resident_boundary_audit_row(w: &WorkloadResult) -> ResidentBoundaryAuditRow {
    let (status, detail) = match w.gpu_resident_pipeline {
        GpuResidentPipelineStatus::Reported => (
            ResidentBoundaryAuditStatus::ReportedResidentPipeline,
            "selected Custom Scan reported a GPU-resident pipeline".to_owned(),
        ),
        _ if w.gpu_resident_boundary_reason.is_some() => (
            ResidentBoundaryAuditStatus::NonResidentPipeline,
            "selected Custom Scan recorded a CPU/PostgreSQL boundary; the hard GPU-only gate should have declined it"
                .to_owned(),
        ),
        _ => (
            ResidentBoundaryAuditStatus::MissingResidentPipelineEvidence,
            "selected Custom Scan lacks `GPU Resident Pipeline: true` proof"
                .to_owned(),
        ),
    };
    ResidentBoundaryAuditRow {
        workload: w.name.clone(),
        rows: w.rows,
        gpu_kernel_dispatched: w.gpu_kernel_dispatched,
        gpu_resident_pipeline: w.gpu_resident_pipeline.as_str().to_owned(),
        boundary_reason: w.gpu_resident_boundary_reason.clone(),
        status,
        detail,
    }
}

impl BenchReport {
    #[must_use]
    pub fn no_dispatch_audit(&self) -> NoDispatchAudit {
        let report = self.with_normalized_dispatch();
        let ignored_dispatching_rows = report
            .workloads
            .iter()
            .filter(|w| !both_modes_appear_non_dispatching(w))
            .count();
        let rows: Vec<_> = report
            .workloads
            .iter()
            .filter_map(no_dispatch_audit_row)
            .collect();
        let warning_rows = rows.iter().filter(|row| row.status.is_warning()).count();
        let timing_skew_rows = rows.iter().filter(|row| row.timing_skew).count();
        let plan_mismatch_rows = rows.iter().filter(|row| row.plan_mismatch).count();
        let selected_custom_scan_not_dispatched_rows = rows
            .iter()
            .filter(|row| row.status == NoDispatchAuditStatus::SelectedCustomScanNotDispatched)
            .count();
        let missing_plan_evidence_rows =
            rows.iter().filter(|row| row.missing_plan_evidence).count();
        NoDispatchAudit {
            schema_version: NO_DISPATCH_AUDIT_SCHEMA_VERSION,
            timing_skew_threshold_fraction: NO_DISPATCH_TIMING_SKEW_THRESHOLD,
            evaluated_no_dispatch_rows: rows.len(),
            clean_rows: rows.len().saturating_sub(warning_rows),
            warning_rows,
            timing_skew_rows,
            plan_mismatch_rows,
            selected_custom_scan_not_dispatched_rows,
            missing_plan_evidence_rows,
            ignored_dispatching_rows,
            rows,
        }
    }

    #[must_use]
    pub fn resident_boundary_audit(&self) -> ResidentBoundaryAudit {
        let report = self.with_normalized_dispatch();
        let ignored_non_custom_scan_rows =
            report.workloads.iter().filter(|w| !w.plan_selected).count();
        let rows: Vec<_> = report
            .workloads
            .iter()
            .filter(|w| w.plan_selected)
            .map(resident_boundary_audit_row)
            .collect();
        let failed_rows = rows.iter().filter(|row| row.status.is_failure()).count();
        ResidentBoundaryAudit {
            schema_version: RESIDENT_BOUNDARY_AUDIT_SCHEMA_VERSION,
            selected_custom_scan_rows: rows.len(),
            passed_rows: rows.len().saturating_sub(failed_rows),
            failed_rows,
            ignored_non_custom_scan_rows,
            rows,
        }
    }

    #[must_use]
    pub fn benchmark_failure_ledger(&self) -> BenchmarkFailureLedger {
        let report = self.with_normalized_dispatch();
        let ship_gate_failures = report.evaluate_benchmark_ship_gate();
        let h3_lane_gate_failures = report.evaluate_h3_lane_gate();
        let mut rows: BTreeMap<(String, usize), BenchmarkFailureLedgerRow> = BTreeMap::new();

        for failure in &ship_gate_failures {
            let workload = report.workload_result(&failure.workload, failure.rows);
            merge_ledger_item(
                &mut rows,
                LedgerMergeItem {
                    workload,
                    workload_name: &failure.workload,
                    row_count: failure.rows,
                    source: "benchmark_ship_gate",
                    priority: "p1_release_blocker",
                    next_track: ledger_track(
                        &failure.workload,
                        workload.map_or("", |w| w.category.as_str()),
                        workload.map_or("", |w| w.kernel_class.as_str()),
                        Some(failure.kind.label()),
                    ),
                    speedup: Some(failure.speedup_median),
                    gate_floor: Some(failure.gate_floor),
                    detail: format!("{}: {}", failure.kind.label(), failure.detail),
                },
            );
        }
        for crash in &report.crashes {
            merge_crash_ledger_item(&mut rows, crash);
        }

        for failure in &h3_lane_gate_failures {
            let workload = report.workload_result(&failure.workload, failure.rows);
            merge_ledger_item(
                &mut rows,
                LedgerMergeItem {
                    workload,
                    workload_name: &failure.workload,
                    row_count: failure.rows,
                    source: "h3_lane_gate",
                    priority: "p1_release_blocker",
                    next_track: "h3_cache_evidence_and_lane_policy",
                    speedup: Some(failure.speedup_median),
                    gate_floor: Some(failure.gate_floor),
                    detail: format!(
                        "{}: advisory warm threshold {:.2}x",
                        failure.kind.label(),
                        failure.advisory_min_warm_speedup
                    ),
                },
            );
        }

        for workload in &report.workloads {
            if workload.pg_accel_stock_exec_delta > 0 {
                merge_ledger_item(
                    &mut rows,
                    LedgerMergeItem {
                        workload: Some(workload),
                        workload_name: &workload.name,
                        row_count: workload.rows,
                        source: "stock_executor_fallback",
                        priority: "p1_release_blocker",
                        next_track: ledger_track(
                            &workload.name,
                            &workload.category,
                            &workload.kernel_class,
                            Some("stock_executor_fallback"),
                        ),
                        speedup: Some(workload.speedup_median_vs_parallel),
                        gate_floor: Some(BENCHMARK_SHIP_GATE_MIN_SPEEDUP),
                        detail: format!(
                            "pg_accel stock executor fallback count was {}",
                            workload.pg_accel_stock_exec_delta
                        ),
                    },
                );
            }

            if workload.custom_scan_selected_not_dispatched {
                merge_ledger_item(
                    &mut rows,
                    LedgerMergeItem {
                        workload: Some(workload),
                        workload_name: &workload.name,
                        row_count: workload.rows,
                        source: "custom_scan_selected_not_dispatched",
                        priority: "p1_release_blocker",
                        next_track: ledger_track(
                            &workload.name,
                            &workload.category,
                            &workload.kernel_class,
                            Some("custom_scan_selected_not_dispatched"),
                        ),
                        speedup: Some(workload.speedup_median_vs_parallel),
                        gate_floor: Some(BENCHMARK_SHIP_GATE_MIN_SPEEDUP),
                        detail: "pg_accel Custom Scan selected without credited GPU dispatch"
                            .to_owned(),
                    },
                );
            }

            if workload.speedup_median_vs_parallel.is_finite()
                && workload.speedup_median_vs_parallel < BENCHMARK_SHIP_GATE_MIN_SPEEDUP
            {
                let (source, priority, detail) = if workload.gpu_kernel_dispatched {
                    (
                        "gpu_dispatched_below_parity",
                        "p1_release_blocker",
                        "credited GPU dispatch below PG-parallel parity",
                    )
                } else if workload.speedup_median_vs_parallel < 0.98 {
                    (
                        "native_below_parity",
                        "p2_architecture_gap",
                        "native/planner-declined row below 0.98x parity",
                    )
                } else {
                    (
                        "native_below_parity",
                        "p3_parity_noise",
                        "native/planner-declined row below 1.00x parity",
                    )
                };
                merge_ledger_item(
                    &mut rows,
                    LedgerMergeItem {
                        workload: Some(workload),
                        workload_name: &workload.name,
                        row_count: workload.rows,
                        source,
                        priority,
                        next_track: ledger_track(
                            &workload.name,
                            &workload.category,
                            &workload.kernel_class,
                            Some(source),
                        ),
                        speedup: Some(workload.speedup_median_vs_parallel),
                        gate_floor: None,
                        detail: detail.to_owned(),
                    },
                );
            }
        }

        let rows_below_parity_count = report
            .workloads
            .iter()
            .filter(|w| {
                w.speedup_median_vs_parallel.is_finite()
                    && w.speedup_median_vs_parallel < BENCHMARK_SHIP_GATE_MIN_SPEEDUP
            })
            .count();
        let gpu_dispatched_below_parity_count = report
            .workloads
            .iter()
            .filter(|w| {
                w.gpu_kernel_dispatched
                    && w.speedup_median_vs_parallel.is_finite()
                    && w.speedup_median_vs_parallel < BENCHMARK_SHIP_GATE_MIN_SPEEDUP
            })
            .count();
        let native_below_parity_count = report
            .workloads
            .iter()
            .filter(|w| {
                !w.gpu_kernel_dispatched
                    && w.speedup_median_vs_parallel.is_finite()
                    && w.speedup_median_vs_parallel < BENCHMARK_SHIP_GATE_MIN_SPEEDUP
            })
            .count();
        let planner_declined_below_parity_count = report
            .workloads
            .iter()
            .filter(|w| {
                w.planner_declined
                    && w.speedup_median_vs_parallel.is_finite()
                    && w.speedup_median_vs_parallel < BENCHMARK_SHIP_GATE_MIN_SPEEDUP
            })
            .count();
        let stock_fallback_rows = report
            .workloads
            .iter()
            .filter(|w| w.pg_accel_stock_exec_delta > 0)
            .count();
        let custom_scan_selected_not_dispatched_rows = report
            .workloads
            .iter()
            .filter(|w| w.custom_scan_selected_not_dispatched)
            .count();

        let mut rows: Vec<_> = rows.into_values().collect();
        rows.sort_by(|a, b| {
            ledger_priority_rank(&a.priority)
                .cmp(&ledger_priority_rank(&b.priority))
                .then_with(|| a.next_track.cmp(&b.next_track))
                .then_with(|| a.workload.cmp(&b.workload))
                .then_with(|| a.rows.cmp(&b.rows))
        });

        BenchmarkFailureLedger {
            schema_version: BENCHMARK_FAILURE_LEDGER_SCHEMA_VERSION,
            total_measured_rows: report.workloads.len(),
            ship_gate_failure_count: ship_gate_failures.len(),
            h3_lane_gate_failure_count: h3_lane_gate_failures.len(),
            crash_count: report.crashes.len(),
            rows_below_parity_count,
            gpu_dispatched_below_parity_count,
            native_below_parity_count,
            planner_declined_below_parity_count,
            stock_fallback_rows,
            custom_scan_selected_not_dispatched_rows,
            unique_work_items: rows.len(),
            rows,
        }
    }

    fn workload_result(&self, name: &str, rows: usize) -> Option<&WorkloadResult> {
        self.workloads
            .iter()
            .find(|workload| workload.name == name && workload.rows == rows)
    }

    fn with_normalized_dispatch(&self) -> Self {
        let mut report = self.clone();
        let full_plans = report.full_plan_artifact_text();
        for w in &mut report.workloads {
            let full_plan = full_plans
                .as_deref()
                .and_then(|plans| extract_full_plan_block(plans, &w.name, w.rows));
            let classification = w.dispatch_classification_with_plan_artifact(full_plan.as_deref());
            w.apply_dispatch_classification(classification);
            w.gpu_resident_boundary_reason =
                classify_gpu_resident_boundary_reason(w, full_plan.as_deref());
        }
        report
    }

    fn full_plan_artifact_text(&self) -> Option<String> {
        let dir = self.artifact_dir.as_deref()?;
        fs::read_to_string(Path::new(dir).join("plans.txt")).ok()
    }

    /// Render the report as a Markdown document.
    ///
    /// The summary table shows speedup at each row scale. Detailed sections
    /// follow with per-scale statistics for every workload.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# pg_accel Benchmark Report\n\n");

        // Hardware profile
        if let Some(hw) = &self.hardware {
            out.push_str("## Hardware Profile\n\n");
            let _ = writeln!(out, "| Property | Value |");
            let _ = writeln!(out, "|----------|-------|");
            let _ = writeln!(out, "| OS | {} {} |", hw.os, hw.os_version);
            let _ = writeln!(out, "| Architecture | {} |", hw.arch);
            let _ = writeln!(out, "| CPU | {} |", hw.cpu);
            let _ = writeln!(out, "| CPU Cores | {} |", hw.cpu_cores);
            let _ = writeln!(out, "| Memory | {} |", hw.memory_gb);
            out.push('\n');
        }

        // -------------------------------------------------------------------
        // Headline geomean callout (action_items M7 / K / Reviewer 1 Sin #1)
        // -------------------------------------------------------------------
        //
        // The geomean table sits IMMEDIATELY after the hardware profile —
        // before settings, methodology, detailed results — so reviewers do
        // not have to scroll to find the lede. The callout line labels
        // sub-1.0x runs as "net regression" and sub-1.0x categories as
        // losers. The non-h3 geomean is printed in its own row so the reader
        // can see the trig-kernel artifact (Reviewer 1 Sin #9) isolated.
        self.render_geomean_headline(&mut out);
        self.render_dispatch_classification(&mut out);
        self.render_dispatch_evidence(&mut out);
        self.render_planner_threshold_matrix(&mut out);
        self.render_sanity_checks(&mut out);

        // -------------------------------------------------------------------
        // Kernel coverage table (action_items W11 / Reviewer 1 Sin #17)
        // -------------------------------------------------------------------
        //
        // Reviewer 1 counts at most 9 distinct GPU kernels across the
        // entire suite; the category table inflates the count because the
        // same `point_in_ring` kernel appears under 4 different category
        // labels. This table collapses everything to kernel class.
        self.render_no_dispatch_audit(&mut out);

        self.render_kernel_coverage(&mut out);

        // -------------------------------------------------------------------
        // Benchmark ship gate (TODO Phase 7 Benchmark win plan)
        // -------------------------------------------------------------------
        //
        // Renders when any selected pg_accel row crashes, misses GPU dispatch,
        // or dispatches below PostgreSQL-parallel parity. The CLI turns this
        // same predicate into a non-zero process exit for CI.
        self.render_benchmark_ship_gate(&mut out);

        // -------------------------------------------------------------------
        // H3 lane gate (TODO Phase 5 H3 winning lane protection)
        // -------------------------------------------------------------------
        //
        // Renders the `### H3 Lane Gate Failures` section when any H3 Winner
        // regressed below the uniform 1.0x floor (or failed to dispatch) or
        // any H3 Parity workload dispatched a GPU kernel. When the gate is
        // green this is a no-op; when it fails, the bench process is expected
        // to exit non-zero (see `main::cmd_run`).
        self.render_h3_lane_gate(&mut out);
        self.render_warmup_jit_audit(&mut out);
        self.render_cache_purge_audit(&mut out);

        // GUC settings
        if let Some(gucs) = &self.gucs
            && !gucs.settings.is_empty()
        {
            out.push_str("## PostgreSQL Settings\n\n");
            if let Some(ts) = &self.postmaster_start_time {
                let _ = writeln!(
                    out,
                    "_Observed from inside the benchmarked session via `SHOW`. Postmaster start: \
                     `{ts}`. Settings listed here are guaranteed to match the running postmaster \
                     — any `PGC_POSTMASTER` mismatch would have aborted the run (action_items \
                     C4)._"
                );
                out.push('\n');
            }
            let _ = writeln!(out, "| GUC | Value |");
            let _ = writeln!(out, "|-----|-------|");
            for (name, val) in &gucs.settings {
                let _ = writeln!(out, "| `{name}` | `{val}` |");
            }
            out.push('\n');
        }

        // Methodology
        out.push_str("## Methodology\n\n");
        let _ = writeln!(out, "| Parameter | Value |");
        let _ = writeln!(out, "|-----------|-------|");
        let _ = writeln!(out, "| Iterations | {} |", self.methodology.iterations);
        let _ = writeln!(out, "| Warmup iterations | {} |", self.methodology.warmup);
        if let Some(dir) = &self.artifact_dir {
            let _ = writeln!(out, "| Artifact directory | `{dir}` |");
        }
        if !self.methodology.harness_profile.is_empty() {
            let _ = writeln!(
                out,
                "| Harness build profile | `{}` |",
                self.methodology.harness_profile
            );
        }
        let scales_str: Vec<String> = self
            .methodology
            .row_scales
            .iter()
            .map(|r| format_rows(*r))
            .collect();
        let _ = writeln!(out, "| Row scales | {} |", scales_str.join(", "));
        let _ = writeln!(
            out,
            "| Measurement ordering | {} |",
            self.methodology.ordering
        );
        for test in &self.methodology.statistical_tests {
            let _ = writeln!(out, "| Statistical test | {test} |");
        }
        out.push_str("\n**Ordering note:** Measurement order (accel-first vs baseline-first) ");
        out.push_str("is randomized per iteration to eliminate cache-warming bias. ");
        out.push_str("Each mode uses a fresh connection with `DISCARD ALL` on close.\n");
        if !self.crashes.is_empty() {
            let _ = writeln!(
                out,
                "\n**Crashes:** {} scale(s) crashed and were excluded from results.",
                self.crashes.len()
            );
        }
        out.push('\n');

        // Collect unique scales and workload names (preserving order).
        let scales = &self.methodology.row_scales;
        let mut workload_names: Vec<String> = Vec::new();
        for w in &self.workloads {
            if !workload_names.contains(&w.name) {
                workload_names.push(w.name.clone());
            }
        }

        // Build lookup: (name, rows) -> &WorkloadResult
        let mut lookup: BTreeMap<(&str, usize), &WorkloadResult> = BTreeMap::new();
        for w in &self.workloads {
            lookup.insert((&w.name, w.rows), w);
        }
        let normalized_detail_report = self.with_normalized_dispatch();
        let mut normalized_lookup: BTreeMap<(&str, usize), &WorkloadResult> = BTreeMap::new();
        for w in &normalized_detail_report.workloads {
            normalized_lookup.insert((&w.name, w.rows), w);
        }

        // Build crash lookup: (name, rows) -> true
        let mut crash_lookup: std::collections::HashSet<(String, usize)> =
            std::collections::HashSet::new();
        for c in &self.crashes {
            crash_lookup.insert((c.workload.clone(), c.rows));
            // Ensure crashed workloads appear in the name list.
            if !workload_names.contains(&c.workload) {
                workload_names.push(c.workload.clone());
            }
        }

        // (The headline geomean is now rendered at the top of the report by
        // `render_geomean_headline`. We retain `n_tests` here for the
        // per-workload Bonferroni-adjusted p in the detail tables.)
        let n_tests = self.workloads.len() + self.crashes.len();

        // Summary table: workload × scale → speedup
        out.push_str("## Results\n\n");
        out.push_str(
            "All comparisons are against PostgreSQL with parallel workers enabled \
             (the default production configuration). Speedup > 1.00x means pg_accel \
             is faster.\n\n",
        );

        // Header
        out.push_str("| Workload |");
        for &s in scales {
            let _ = write!(out, " {} |", format_rows(s));
        }
        out.push('\n');
        out.push_str("|----------|");
        for _ in scales {
            out.push_str("------|");
        }
        out.push('\n');

        // Data rows. action_items L: crash cells render as `CRASH` so they
        // are visually distinct from "no result". Workloads with one or more
        // crashes get an asterisk in the row name and a `(N/M kernels stable)`
        // annotation reflecting how many of the attempted scales actually
        // produced a result. The headline rate uses median speedup.
        for name in &workload_names {
            // Count crashed scales for this workload across the requested
            // scale list. attempted = scales that have either a result or a
            // crash. stable = scales with a non-crash result.
            let mut attempted = 0usize;
            let mut stable = 0usize;
            for &s in scales {
                let has_result = lookup.contains_key(&(name.as_str(), s));
                let has_crash = crash_lookup.contains(&(name.clone(), s));
                if has_result || has_crash {
                    attempted += 1;
                }
                if has_result {
                    stable += 1;
                }
            }
            let any_crash = stable < attempted && attempted > 0;
            let display_name = if any_crash {
                format!("{name}* ({stable}/{attempted} kernels stable)")
            } else {
                name.clone()
            };
            let _ = write!(out, "| {display_name} |");
            for &s in scales {
                if let Some(w) = lookup.get(&(name.as_str(), s)) {
                    let sp = w.speedup_median_vs_parallel;
                    // Significance gate: |d| ≥ 0.5 AND Bonferroni p < 0.05.
                    let adj = stats::bonferroni_adjusted_p(w.p_value_vs_parallel, n_tests);
                    let sig = adj.is_finite() && adj < 0.05 && w.effect_size_meaningful;
                    if sig && sp > 1.005 {
                        let _ = write!(out, " **{sp:.2}x** |");
                    } else {
                        let _ = write!(out, " {sp:.2}x |");
                    }
                } else if crash_lookup.contains(&(name.clone(), s)) {
                    out.push_str(" CRASH |");
                } else {
                    out.push_str(" — |");
                }
            }
            out.push('\n');
        }
        out.push('\n');

        // Detailed per-workload sections
        out.push_str("## Detailed Results\n\n");
        for name in &workload_names {
            let _ = writeln!(out, "### {name}\n");

            // Get description from first result
            if let Some(w) = self.workloads.iter().find(|w| w.name == *name) {
                let _ = writeln!(out, "**Query:** {}\n", w.description);
            }

            // Per-scale detail table. action_items F+G+I:
            //   - median (with p25/p75/p95) is the headline timing column,
            //     mean is supplementary
            //   - Cohen's d is shown alongside Bonferroni p
            //   - the significance verdict requires BOTH |d|≥0.5 AND Bonferroni
            //   - p-values are formatted to ≥6 sig figs scientific notation
            //   - rows where cv_ratio > 3 are flagged "(asymmetric variance)"
            let _ = writeln!(
                out,
                "| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | \
                 PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | \
                 Resident Load (ms) |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|---|");
            for &s in scales {
                if let Some(w) = lookup.get(&(name.as_str(), s)) {
                    let dispatch_view = normalized_lookup
                        .get(&(name.as_str(), s))
                        .copied()
                        .unwrap_or(*w);
                    let adj_p = stats::bonferroni_adjusted_p(w.p_value_vs_parallel, n_tests);
                    let bonf_sig = adj_p.is_finite() && adj_p < 0.05;
                    let verdict = if !dispatch_view.gpu_kernel_dispatched {
                        dispatch_evidence_label(dispatch_view)
                    } else if bonf_sig && w.effect_size_meaningful {
                        if w.speedup_median_vs_parallel > 1.0 {
                            "WIN"
                        } else {
                            "LOSS"
                        }
                    } else if bonf_sig {
                        // Bonferroni passed but effect size below 0.5.
                        "p-only"
                    } else {
                        "ns"
                    };
                    let asym = if w.cv_ratio.is_finite() && w.cv_ratio > 3.0 {
                        " (asym var)"
                    } else {
                        ""
                    };
                    // Resident-cache preload cost, when this row required one
                    // (`ssbm_*` / `resident_*` lanes). Paid once, off the
                    // timed clock, and amortized across repeated queries
                    // against the same resident cache — surfaced here so the
                    // report never hides what the accel side got for free.
                    let resident_load = if w.resident_lane {
                        format_optional_ms(w.resident_load_ms)
                    } else {
                        "-".to_owned()
                    };
                    let _ = writeln!(
                        out,
                        "| {} | {:.2}{} | {:.2}–{:.2} (p95 {:.2}) | {:.2}{} | \
                         {:.2}–{:.2} (p95 {:.2}) | **{:.2}x** | {:.2} | {} | {} | {} |",
                        format_rows(s),
                        w.accel_median_ms,
                        asym,
                        w.accel_p25_ms,
                        w.accel_p75_ms,
                        w.accel_p95_ms,
                        w.parallel_median_ms,
                        asym,
                        w.parallel_p25_ms,
                        w.parallel_p75_ms,
                        w.parallel_p95_ms,
                        w.speedup_median_vs_parallel,
                        w.cohens_d_vs_parallel,
                        format_pvalue(adj_p),
                        verdict,
                        resident_load,
                    );
                }
            }
            out.push('\n');

            // Evidence appendix: warm/cold split (never pooled), cold-cache
            // shared_buffers warning, native-decline evidence source, and a
            // truncated plan-snippet preview. Rendered as notes rather than
            // extra columns on the primary table above so the headline shape
            // stays stable while the honesty-critical evidence is still
            // visible per row.
            for &s in scales {
                if let Some(w) = lookup.get(&(name.as_str(), s)) {
                    render_workload_evidence_notes(w, &format_rows(s), &mut out);
                }
            }
        }

        // -------------------------------------------------------------------
        // Regressions: workloads significantly slower than PG parallel.
        // -------------------------------------------------------------------
        //
        // A workload makes the list only if it's BOTH slower by >10% AND
        // statistically significant after Bonferroni correction. Sorted by
        // absolute slowdown (worst first).
        let normalized_for_regressions = self.with_normalized_dispatch();
        let mut regressions: Vec<&WorkloadResult> = normalized_for_regressions
            .workloads
            .iter()
            .filter(|w| {
                let adj_p = stats::bonferroni_adjusted_p(w.p_value_vs_parallel, n_tests);
                w.gpu_kernel_dispatched
                    && w.speedup_vs_parallel < 0.90
                    && adj_p.is_finite()
                    && adj_p < 0.05
                    && w.effect_size_meaningful
            })
            .collect();
        regressions.sort_by(|a, b| {
            a.speedup_vs_parallel
                .partial_cmp(&b.speedup_vs_parallel)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if !regressions.is_empty() {
            out.push_str("## Regressions\n\n");
            out.push_str(
                "Workloads where pg_accel is **statistically significantly slower** than PG \
                 parallel with credited GPU dispatch (>10% slowdown, Bonferroni-corrected \
                 p < 0.05). Planner-declined/no-dispatch rows are reported in the \
                 no-dispatch audit instead of here.\n\n",
            );
            let _ = writeln!(
                out,
                "| Workload | Scale | Speedup (median) | Cohen's d | Accel median (ms) | \
                 PG median (ms) | p (Bonferroni) |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|---|---|");
            for w in &regressions {
                let adj_p = stats::bonferroni_adjusted_p(w.p_value_vs_parallel, n_tests);
                let _ = writeln!(
                    out,
                    "| {} | {} | {:.2}x | {:.2} | {:.2} | {:.2} | {} |",
                    w.name,
                    format_rows(w.rows),
                    w.speedup_median_vs_parallel,
                    w.cohens_d_vs_parallel,
                    w.accel_median_ms,
                    w.parallel_median_ms,
                    format_pvalue(adj_p),
                );
            }
            out.push('\n');
        }

        // -------------------------------------------------------------------
        // Non-Dispatching: workloads where speedup ≈ 1.0, meaning pg_accel
        // almost certainly did not dispatch to the GPU (no custom scan, or
        // dispatch rejected). These are the most informative diagnostic rows
        // — they tell us which workloads never even got off the starting line.
        // -------------------------------------------------------------------
        let normalized_dispatch = self.with_normalized_dispatch();
        let non_dispatch: Vec<&WorkloadResult> = normalized_dispatch
            .workloads
            .iter()
            .filter(|w| !w.gpu_kernel_dispatched)
            .collect();
        if !non_dispatch.is_empty() {
            out.push_str("## Non-Dispatching Workloads\n\n");
            out.push_str(
                "Workloads where runtime counters did not prove GPU dispatch. These are not GPU \
                 performance conclusions. If the no-dispatch audit flags a row, treat it as \
                 harness/planner skew until the pg_accel-side plan either dispatches to GPU or \
                 normal PostgreSQL planning cleanly declines the pg_accel path.\n\n",
            );
            let _ = writeln!(
                out,
                "| Workload | Scale | Classification | Speedup | Accel (ms) | PG Parallel (ms) |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|---|");
            for w in &non_dispatch {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {:.2}x | {:.2} | {:.2} |",
                    w.name,
                    format_rows(w.rows),
                    dispatch_evidence_label(w),
                    w.speedup_vs_parallel,
                    w.accel_mean_ms,
                    w.parallel_mean_ms,
                );
            }
            out.push('\n');
        }

        // Crashed scales section
        if !self.crashes.is_empty() {
            out.push_str("## Crashed Scales\n\n");
            out.push_str(
                "The following workload/scale combinations crashed the PostgreSQL \
                 backend and were excluded from results. Artifact paths are relative \
                 to the artifact directory when one was configured.\n\n",
            );
            let _ = writeln!(
                out,
                "| Workload | Scale | Error | Plan Snippet | Correctness Diff | Log Tails | Repro |"
            );
            let _ = writeln!(
                out,
                "|----------|-------|-------|--------------|------------------|-----------|-------|"
            );
            for c in &self.crashes {
                // Char-boundary-safe truncation: byte-slicing at a fixed
                // offset panics if the cut falls inside a multibyte UTF-8
                // sequence.
                let short_err = if c.error.chars().count() > 80 {
                    let truncated: String = c.error.chars().take(77).collect();
                    format!("{truncated}...")
                } else {
                    c.error.clone()
                };
                let plan = c.plan_snippet_artifact.as_deref().unwrap_or("-");
                let correctness = c.correctness_diff_artifact.as_deref().unwrap_or("-");
                let logs = if c.log_tail_artifacts.is_empty() {
                    "-".to_owned()
                } else {
                    c.log_tail_artifacts.join("<br>")
                };
                let repro = c.repro_command.as_deref().unwrap_or("-");
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} | `{}` |",
                    c.workload,
                    format_rows(c.rows),
                    short_err,
                    plan,
                    correctness,
                    logs,
                    repro,
                );
            }
            out.push('\n');
        }

        out
    }

    /// Render the report as JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails (should not happen in practice).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.with_normalized_dispatch())
    }

    /// Render the report as CSV (one row per workload per scale, two-way).
    #[must_use]
    pub fn to_csv(&self) -> String {
        let report = self.with_normalized_dispatch();
        let mut out = String::new();
        out.push_str(
            "workload,category,kernel_class,plan_selected,gpu_kernel_dispatched,\
             function_srf_kernel_dispatched,planner_declined,\
             custom_scan_selected_not_dispatched,function_kernel_count,\
             rows_returned_to_cpu,gpu_resident_pipeline,dispatch_counter_captured,\
             gpu_kernel_execution_delta,pg_accel_rows_dispatched_delta,\
             pg_accel_batches_executed_delta,pg_accel_gpu_rows_processed_delta,\
             pg_accel_stock_exec_delta,\
             accel_output_rows_consumed,rows,correctness_diff_artifact,\
             warmup_count,warmup_accel_first_ms,warmup_accel_max_ms,\
             warmup_accel_post_first_max_ms,warmup_parallel_first_ms,warmup_parallel_max_ms,\
             baseline_dispatched,no_dispatch_timing_skew,no_dispatch_plan_mismatch,\
             accel_mean_ms,accel_stddev_ms,accel_median_ms,accel_p25_ms,accel_p75_ms,accel_p95_ms,\
             accel_cv_pct,accel_min_ms,accel_max_ms,\
             parallel_mean_ms,parallel_stddev_ms,parallel_median_ms,parallel_p25_ms,parallel_p75_ms,\
             parallel_p95_ms,parallel_cv_pct,parallel_min_ms,parallel_max_ms,\
             sanity_check_count,sanity_check_failed,\
             speedup_vs_parallel,speedup_median_vs_parallel,p_value_vs_parallel,\
             cohens_d_vs_parallel,effect_size_meaningful,cv_ratio,significant\n",
        );
        for w in &report.workloads {
            let baseline_dispatched = baseline_plan_dispatches(w);
            let timing_skew = no_dispatch_timing_skew(w);
            let plan_mismatch = no_dispatch_plan_mismatch(w);
            let sig = if w.p_value_vs_parallel < 0.01 {
                "yes"
            } else if w.p_value_vs_parallel < 0.05 {
                "marginal"
            } else {
                "no"
            };
            let warmup_accel_first = csv_optional_f64(w.warmup_accel_first_ms);
            let warmup_accel_max = csv_optional_f64(w.warmup_accel_max_ms);
            let warmup_accel_post_first_max = csv_optional_f64(w.warmup_accel_post_first_max_ms);
            let warmup_parallel_first = csv_optional_f64(w.warmup_parallel_first_ms);
            let warmup_parallel_max = csv_optional_f64(w.warmup_parallel_max_ms);
            let sanity_check_count = w.sanity_checks.len();
            let sanity_check_failed = w.sanity_checks.iter().filter(|check| !check.passed).count();
            let _ = writeln!(
                out,
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},\
                 {},{},{},{},{},{},{},{},{},{},{},{},{},{},{},\
                 {:.4},{:.4},{:.4},{:.4},{:.4},{:.4},\
                 {:.4},{:.4},{:.4},\
                 {:.4},{:.4},{:.4},{:.4},{:.4},\
                 {:.4},{:.4},{:.4},{:.4},{},{},\
                 {:.4},{:.4},{:.6e},\
                 {:.4},{},{:.4},{}",
                w.name,
                w.category,
                w.kernel_class,
                w.plan_selected,
                w.gpu_kernel_dispatched,
                w.function_srf_kernel_dispatched,
                w.planner_declined,
                w.custom_scan_selected_not_dispatched,
                w.function_kernel_count,
                w.rows_returned_to_cpu,
                w.gpu_resident_pipeline.as_str(),
                w.dispatch_counter_captured,
                w.gpu_kernel_execution_delta,
                w.pg_accel_rows_dispatched_delta,
                w.pg_accel_batches_executed_delta,
                w.pg_accel_gpu_rows_processed_delta,
                w.pg_accel_stock_exec_delta,
                w.accel_output_rows_consumed,
                w.rows,
                w.correctness_diff_artifact.as_deref().unwrap_or(""),
                w.warmup_iterations.len(),
                warmup_accel_first,
                warmup_accel_max,
                warmup_accel_post_first_max,
                warmup_parallel_first,
                warmup_parallel_max,
                baseline_dispatched,
                timing_skew,
                plan_mismatch,
                w.accel_mean_ms,
                w.accel_stddev_ms,
                w.accel_median_ms,
                w.accel_p25_ms,
                w.accel_p75_ms,
                w.accel_p95_ms,
                w.accel_cv_pct,
                w.accel_min_ms,
                w.accel_max_ms,
                w.parallel_mean_ms,
                w.parallel_stddev_ms,
                w.parallel_median_ms,
                w.parallel_p25_ms,
                w.parallel_p75_ms,
                w.parallel_p95_ms,
                w.parallel_cv_pct,
                w.parallel_min_ms,
                w.parallel_max_ms,
                sanity_check_count,
                sanity_check_failed,
                w.speedup_vs_parallel,
                w.speedup_median_vs_parallel,
                w.p_value_vs_parallel,
                w.cohens_d_vs_parallel,
                w.effect_size_meaningful,
                w.cv_ratio,
                sig,
            );
        }
        out
    }

    /// Render the headline geomean callout banner that sits at the top of
    /// the markdown report.
    ///
    /// action_items K + M7 + H + L + M:
    ///   - The geomean is the lede; reviewers should not have to scroll
    ///   - Sub-1.0x runs are labeled "net regression"
    ///   - Each category row shows sig wins / sig losses / total sig
    ///     using the |d| ≥ 0.5 AND Bonferroni gate
    ///   - "overall wins outside h3" appears as a separate row so the
    ///     trig-kernel artifact (Reviewer 1 Sin #9) is visible
    ///   - Crashed scales count toward the family size for Bonferroni
    ///     so adding more crashes does NOT make the surviving rows
    ///     accidentally pass significance
    ///   - Workloads where `gpu_kernel_dispatched == false` are excluded
    ///     from per-category geomeans (action_items M)
    fn render_geomean_headline(&self, out: &mut String) {
        let report = self.with_normalized_dispatch();
        // Family size for Bonferroni = attempted scales = workloads + crashes.
        // This is the same value used by the detail tables so the two views
        // never disagree.
        let family_size = report.workloads.len() + report.crashes.len();
        let speedup_label = "median speedup";
        let speedup_of = |w: &WorkloadResult| -> f64 { w.speedup_median_vs_parallel };

        // ----- Overall (dispatched only) -----
        let dispatched: Vec<&WorkloadResult> = report
            .workloads
            .iter()
            .filter(|w| w.gpu_kernel_dispatched)
            .collect();
        let dispatched_speedups: Vec<f64> = dispatched.iter().map(|w| speedup_of(w)).collect();
        let overall_gm = stats::geomean(&dispatched_speedups);
        let overall_counts = classify_significance(&dispatched, family_size, 0.05);

        let label = if overall_gm < 1.0 {
            "**NET REGRESSION**"
        } else if overall_gm < 1.05 {
            "**NEUTRAL** (within noise)"
        } else {
            "**NET SPEEDUP**"
        };
        out.push_str("## Headline\n\n");
        let _ = writeln!(
            out,
            "> {label}: overall {speedup_label} = **{overall_gm:.2}x** \
             (geomean across {n} GPU-dispatched workloads, family size = {family_size}).",
            n = dispatched.len(),
        );
        let _ = writeln!(
            out,
            ">\n> Significant wins: **{}** · Significant losses: **{}** · \
             Not significant: **{}** · Effect-size rejected: **{}**",
            overall_counts.sig_wins,
            overall_counts.sig_losses,
            overall_counts.not_significant,
            overall_counts.effect_rejected,
        );
        if !report.crashes.is_empty() {
            let _ = writeln!(
                out,
                ">\n> {n_crash} scale(s) crashed and are counted in the Bonferroni \
                 family size but not in the geomean.",
                n_crash = report.crashes.len(),
            );
        }
        out.push('\n');

        // ----- Per-category table -----
        // Group dispatched workloads by category for the breakdown.
        let mut by_cat: BTreeMap<String, Vec<&WorkloadResult>> = BTreeMap::new();
        for w in &dispatched {
            by_cat.entry(w.category.clone()).or_default().push(*w);
        }

        out.push_str("### Geomean by Category\n\n");
        out.push_str(
            "Sub-1.0x categories are losers. The `outside_h3` row excludes \
             `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock \
             aggregate so this row is the more honest non-h3 picture.\n\n",
        );
        let _ = writeln!(
            out,
            "| Category | Workloads | Geomean ({speedup_label}) | Sig Wins | \
             Sig Losses | Total Sig | Not Sig |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|---|");
        for (cat, ws) in &by_cat {
            let speedups: Vec<f64> = ws.iter().map(|w| speedup_of(w)).collect();
            let gm = stats::geomean(&speedups);
            let counts = classify_significance(ws, family_size, 0.05);
            let _ = writeln!(
                out,
                "| {cat} | {} | {gm:.2}x | {} | {} | {} | {} |",
                ws.len(),
                counts.sig_wins,
                counts.sig_losses,
                counts.total_sig,
                counts.not_significant,
            );
        }

        // outside_h3 row.
        let outside_h3: Vec<&WorkloadResult> = dispatched
            .iter()
            .copied()
            .filter(|w| w.category != "gpu_h3")
            .collect();
        let outside_speedups: Vec<f64> = outside_h3.iter().map(|w| speedup_of(w)).collect();
        let outside_gm = stats::geomean(&outside_speedups);
        let outside_counts = classify_significance(&outside_h3, family_size, 0.05);
        let _ = writeln!(
            out,
            "| **outside_h3** | **{}** | **{outside_gm:.2}x** | **{}** | **{}** | **{}** | **{}** |",
            outside_h3.len(),
            outside_counts.sig_wins,
            outside_counts.sig_losses,
            outside_counts.total_sig,
            outside_counts.not_significant,
        );

        // overall row.
        let _ = writeln!(
            out,
            "| **overall (GPU-dispatched)** | **{}** | **{overall_gm:.2}x** | **{}** | **{}** | **{}** | **{}** |",
            dispatched.len(),
            overall_counts.sig_wins,
            overall_counts.sig_losses,
            overall_counts.total_sig,
            overall_counts.not_significant,
        );
        out.push('\n');

        // ----- Per-dispatch-source split (TODO Phase 0 acceptance) ---------
        //
        // Custom Scan dispatch and Function/SRF kernel dispatch share the
        // headline geomean, but a single combined number hides which path
        // earned the win. This sub-table prints the geomean / wins / losses
        // for each dispatch source separately so reviewers can see at a
        // glance whether the headline is being carried by Custom Scan
        // execution, the function/SRF kernel hook, or both.
        let custom_scan_dispatched: Vec<&WorkloadResult> = dispatched
            .iter()
            .copied()
            .filter(|w| !w.function_srf_kernel_dispatched)
            .collect();
        let function_srf_dispatched: Vec<&WorkloadResult> = dispatched
            .iter()
            .copied()
            .filter(|w| w.function_srf_kernel_dispatched)
            .collect();

        out.push_str("### Geomean by Dispatch Source\n\n");
        out.push_str(
            "Splits the `overall (GPU-dispatched)` row into two buckets: \
             pg_accel Custom Scan execution and function/SRF kernel dispatch. \
             Custom Scan rows have a `Custom Scan` plan node; function/SRF \
             rows have non-zero `function_kernel_count` without a Custom \
             Scan node. Either bucket counts as a GPU-dispatched win, but \
             they exercise different code paths and the report must not \
             collapse them into a single bar.\n\n",
        );
        let _ = writeln!(
            out,
            "| Dispatch Source | Workloads | Geomean ({speedup_label}) | Sig Wins | \
             Sig Losses | Total Sig | Not Sig |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|---|");

        let cs_speedups: Vec<f64> = custom_scan_dispatched
            .iter()
            .map(|w| speedup_of(w))
            .collect();
        let cs_gm = stats::geomean(&cs_speedups);
        let cs_counts = classify_significance(&custom_scan_dispatched, family_size, 0.05);
        let _ = writeln!(
            out,
            "| Custom Scan dispatch | {} | {cs_gm:.2}x | {} | {} | {} | {} |",
            custom_scan_dispatched.len(),
            cs_counts.sig_wins,
            cs_counts.sig_losses,
            cs_counts.total_sig,
            cs_counts.not_significant,
        );

        let fn_speedups: Vec<f64> = function_srf_dispatched
            .iter()
            .map(|w| speedup_of(w))
            .collect();
        let fn_gm = stats::geomean(&fn_speedups);
        let fn_counts = classify_significance(&function_srf_dispatched, family_size, 0.05);
        let _ = writeln!(
            out,
            "| Function/SRF kernel dispatch | {} | {fn_gm:.2}x | {} | {} | {} | {} |",
            function_srf_dispatched.len(),
            fn_counts.sig_wins,
            fn_counts.sig_losses,
            fn_counts.total_sig,
            fn_counts.not_significant,
        );
        out.push('\n');

        // ----- Resident-cache lane split -----
        //
        // Resident lanes (`ssbm_*` / `resident_*`) pre-load GPU-resident
        // caches OFF the timed clock, while the PG-parallel baseline pays
        // scan I/O ON the clock for the same query. That is a real and
        // legitimate difference, but it must never hide inside one combined
        // geomean bar — a reviewer needs to be able to tell "this win
        // includes off-clock preload" from "this win did not". See
        // `resident_load_ms` in Detailed Results for the per-row preload
        // cost.
        let resident_dispatched: Vec<&WorkloadResult> = dispatched
            .iter()
            .copied()
            .filter(|w| w.resident_lane)
            .collect();
        let non_resident_dispatched: Vec<&WorkloadResult> = dispatched
            .iter()
            .copied()
            .filter(|w| !w.resident_lane)
            .collect();

        out.push_str("### Geomean by Resident-Cache Lane\n\n");
        out.push_str(
            "Splits the `overall (GPU-dispatched)` row into resident-cache lanes \
             (pre-load caches off the timed clock; see `resident_load_ms` per row in \
             Detailed Results) and non-resident lanes (no off-clock preload). Both \
             buckets count as GPU-dispatched wins, but only the non-resident bucket is \
             directly comparable to a PG-parallel baseline that pays scan I/O on the \
             clock.\n\n",
        );
        let _ = writeln!(
            out,
            "| Lane | Workloads | Geomean ({speedup_label}) | Sig Wins | \
             Sig Losses | Total Sig | Not Sig |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|---|");

        let resident_speedups: Vec<f64> =
            resident_dispatched.iter().map(|w| speedup_of(w)).collect();
        let resident_gm = stats::geomean(&resident_speedups);
        let resident_counts = classify_significance(&resident_dispatched, family_size, 0.05);
        let _ = writeln!(
            out,
            "| Resident-cache lane | {} | {resident_gm:.2}x | {} | {} | {} | {} |",
            resident_dispatched.len(),
            resident_counts.sig_wins,
            resident_counts.sig_losses,
            resident_counts.total_sig,
            resident_counts.not_significant,
        );

        let non_resident_speedups: Vec<f64> = non_resident_dispatched
            .iter()
            .map(|w| speedup_of(w))
            .collect();
        let non_resident_gm = stats::geomean(&non_resident_speedups);
        let non_resident_counts =
            classify_significance(&non_resident_dispatched, family_size, 0.05);
        let _ = writeln!(
            out,
            "| Non-resident lane | {} | {non_resident_gm:.2}x | {} | {} | {} | {} |",
            non_resident_dispatched.len(),
            non_resident_counts.sig_wins,
            non_resident_counts.sig_losses,
            non_resident_counts.total_sig,
            non_resident_counts.not_significant,
        );
        out.push('\n');

        // ----- CRASH summary rows (action_items L) -----
        if !report.crashes.is_empty() {
            out.push_str("### Crashed scales\n\n");
            let _ = writeln!(
                out,
                "| Workload | Scale | Error | Plan Snippet | Log Tails | Repro |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|---|");
            for c in &report.crashes {
                // Char-boundary-safe truncation: byte-slicing at a fixed
                // offset panics if the cut falls inside a multibyte UTF-8
                // sequence (e.g. an error message containing non-ASCII
                // identifiers or copy-pasted terminal output).
                let short = if c.error.chars().count() > 80 {
                    let truncated: String = c.error.chars().take(77).collect();
                    format!("{truncated}...")
                } else {
                    c.error.clone()
                };
                let plan = c.plan_snippet_artifact.as_deref().unwrap_or("-");
                let logs = if c.log_tail_artifacts.is_empty() {
                    "-".to_owned()
                } else {
                    c.log_tail_artifacts.join("<br>")
                };
                let repro = c.repro_command.as_deref().unwrap_or("-");
                let _ = writeln!(
                    out,
                    "| {} | {} | CRASH: {} | {} | {} | `{}` |",
                    c.workload,
                    format_rows(c.rows),
                    short,
                    plan,
                    logs,
                    repro,
                );
            }
            out.push('\n');
        }
    }

    fn render_planner_threshold_matrix(&self, out: &mut String) {
        let report = self.with_normalized_dispatch();
        let mut rows: Vec<(
            &WorkloadResult,
            crate::workloads::BenchmarkThresholdMatrixEntry,
        )> = report
            .workloads
            .iter()
            .filter_map(|w| {
                crate::workloads::benchmark_threshold_matrix_entry(&w.name, w.rows)
                    .map(|entry| (w, entry))
            })
            .collect();
        rows.sort_by(|(a, left), (b, right)| {
            left.lane
                .cmp(right.lane)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.rows.cmp(&b.rows))
        });
        if rows.is_empty() {
            return;
        }

        out.push_str("## Planner Threshold Matrix\n\n");
        out.push_str(
            "Each row ties planner admission to a concrete release-lane matrix cell: \
             row count, type, cardinality, selectivity, result count, index/pruning \
             shape, retained prepared geometry, batch count, row width, output size, \
             dispatch/output proof, correctness proof, cache/warm-run proof, and \
             measured break-even basis. Expected GPU winners must dispatch, consume \
             output, and meet their warm-run threshold; native-decline cells must not \
             select pg_accel.\n\n",
        );
        let _ = writeln!(
            out,
            "| Lane | Workload | Scale | Type | Cardinality | Selectivity | Result Count | \
             Index/Pruning | Prepared Geometry | Batches | Row Width | Output | \
             Dispatch/Output Evidence | Correctness Evidence | Cache Gate | \
             Threshold Basis | Expected | Observed | Decline Evidence | Speedup | Status |"
        );
        let _ = writeln!(
            out,
            "|---|---|---:|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---:|---|"
        );
        for (workload, entry) in rows {
            let status = threshold_matrix_status(
                workload,
                entry.lane,
                entry.expectation,
                &self.methodology.cache_mode,
            );
            let expected = threshold_matrix_expectation_label(entry.expectation);
            let observed = dispatch_evidence_label(workload);
            let decline_evidence = native_decline_evidence_label(workload);
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.2}x | {} |",
                markdown_cell(entry.lane),
                markdown_cell(entry.workload),
                format_rows(entry.rows),
                markdown_cell(entry.data_type),
                markdown_cell(entry.cardinality),
                markdown_cell(entry.selectivity),
                markdown_cell(&entry.result_count),
                markdown_cell(entry.index_pruning_shape),
                markdown_cell(entry.prepared_geometry),
                markdown_cell(&entry.batch_count),
                markdown_cell(entry.row_width),
                markdown_cell(entry.output_size),
                markdown_cell(entry.dispatch_evidence),
                markdown_cell(entry.correctness_evidence),
                markdown_cell(entry.cache_gate),
                markdown_cell(entry.threshold_basis),
                markdown_cell(&expected),
                observed,
                markdown_cell(&decline_evidence),
                workload.speedup_median_vs_parallel,
                status,
            );
        }
        out.push('\n');
    }

    fn render_cache_purge_audit(&self, out: &mut String) {
        let mut rows: Vec<(&WorkloadResult, Vec<usize>, Vec<usize>)> = Vec::new();
        for workload in &self.workloads {
            let unavailable = iteration_indexes_with_cache_purge(
                &workload.iterations,
                CachePurgeState::Unavailable,
            );
            let failed =
                iteration_indexes_with_cache_purge(&workload.iterations, CachePurgeState::Failed);
            if !unavailable.is_empty() || !failed.is_empty() {
                rows.push((workload, unavailable, failed));
            }
        }
        if rows.is_empty() {
            return;
        }

        out.push_str("## Cache Purge Audit\n\n");
        out.push_str(
            "Cold-cache iterations whose OS page-cache purge could not be proven are \
             marked here and in `report.json` so skips/failures do not depend on \
             terminal scrollback.\n\n",
        );
        let _ = writeln!(
            out,
            "| Workload | Scale | Unavailable purge iterations | Failed purge iterations |"
        );
        let _ = writeln!(out, "|---|---:|---|---|");
        for (workload, unavailable, failed) in rows {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} |",
                markdown_cell(&workload.name),
                format_rows(workload.rows),
                markdown_cell(&format_iteration_indexes(&unavailable)),
                markdown_cell(&format_iteration_indexes(&failed)),
            );
        }
        out.push('\n');
    }

    fn render_warmup_jit_audit(&self, out: &mut String) {
        let mut rows: Vec<(&WorkloadResult, String)> = self
            .workloads
            .iter()
            .filter_map(|workload| {
                warmup_jit_audit_reason(workload).map(|reason| (workload, reason))
            })
            .collect();
        rows.sort_by(|(left, _), (right, _)| {
            right
                .warmup_accel_post_first_max_ms
                .partial_cmp(&left.warmup_accel_post_first_max_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if rows.is_empty() {
            return;
        }

        out.push_str("## Warmup/JIT Audit\n\n");
        out.push_str(
            "Warmup timings are excluded from published statistics, but retained in \
             `report.json`. Rows here had post-first warmup spikes large enough to \
             suggest recurring JIT/runtime latency rather than a single cold compile.\n\n",
        );
        let _ = writeln!(
            out,
            "| Workload | Scale | Warmups | First accel | Max accel | Post-first max accel | Measured accel median | Reason |"
        );
        let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---|");
        for (workload, reason) in rows {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                markdown_cell(&workload.name),
                format_rows(workload.rows),
                workload.warmup_iterations.len(),
                format_optional_ms(workload.warmup_accel_first_ms),
                format_optional_ms(workload.warmup_accel_max_ms),
                format_optional_ms(workload.warmup_accel_post_first_max_ms),
                format_optional_ms(Some(workload.accel_median_ms)),
                markdown_cell(&reason),
            );
        }
        out.push('\n');
    }

    fn render_sanity_checks(&self, out: &mut String) {
        let rows: Vec<(&WorkloadResult, &SanityCheck)> = self
            .workloads
            .iter()
            .flat_map(|workload| {
                workload
                    .sanity_checks
                    .iter()
                    .map(move |check| (workload, check))
            })
            .collect();
        if rows.is_empty() {
            return;
        }

        let failed = rows.iter().filter(|(_, check)| !check.passed).count();
        out.push_str("## Benchmark Sanity Checks\n\n");
        let _ = writeln!(
            out,
            "Captured after setup and before timing. Zero-row dimension filters are release-blocking \
             because they can make no-dispatch SSBM rows look like GPU performance results. \
             Failed checks: **{failed}**.\n"
        );
        let _ = writeln!(out, "| Workload | Scale | Check | Count | Status |");
        let _ = writeln!(out, "|---|---|---|---:|---|");
        for (workload, check) in rows {
            let status = if check.passed { "pass" } else { "FAIL" };
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                workload.name,
                format_rows(workload.rows),
                markdown_cell(&check.label),
                check.count,
                status,
            );
        }
        out.push('\n');
    }

    fn render_dispatch_classification(&self, out: &mut String) {
        let report = self.with_normalized_dispatch();
        let total = report.workloads.len();
        let plan_selected = report.workloads.iter().filter(|w| w.plan_selected).count();
        let gpu_kernel_dispatched = report
            .workloads
            .iter()
            .filter(|w| w.gpu_kernel_dispatched)
            .count();
        let function_srf_kernel_dispatched = report
            .workloads
            .iter()
            .filter(|w| w.function_srf_kernel_dispatched)
            .count();
        let counter_capture = report
            .workloads
            .iter()
            .filter(|w| w.dispatch_counter_captured)
            .count();
        let kernel_counter_positive = report
            .workloads
            .iter()
            .filter(|w| w.gpu_kernel_execution_delta > 0)
            .count();
        let stock_exec_positive = report
            .workloads
            .iter()
            .filter(|w| w.pg_accel_stock_exec_delta > 0)
            .count();
        let plan_only = report
            .workloads
            .iter()
            .filter(|w| w.custom_scan_selected_not_dispatched)
            .count();
        let planner_declined = report
            .workloads
            .iter()
            .filter(|w| w.planner_declined)
            .count();
        let function_kernel_count: u64 = report
            .workloads
            .iter()
            .map(|w| w.function_kernel_count)
            .sum();
        let rows_returned_to_cpu: u64 = report
            .workloads
            .iter()
            .map(|w| w.rows_returned_to_cpu)
            .sum();
        let gpu_resident_reported = report
            .workloads
            .iter()
            .filter(|w| w.gpu_resident_pipeline == GpuResidentPipelineStatus::Reported)
            .count();
        let gpu_resident_not_reported = report
            .workloads
            .iter()
            .filter(|w| w.plan_selected && w.gpu_kernel_dispatched)
            .filter(|w| w.gpu_resident_pipeline != GpuResidentPipelineStatus::Reported)
            .count();
        let gpu_resident_boundary_recorded = report
            .workloads
            .iter()
            .filter(|w| w.plan_selected && w.gpu_resident_boundary_reason.is_some())
            .count();

        out.push_str("## Dispatch Classification\n\n");
        out.push_str(
            "Plan selection and runtime GPU work are counted separately. Runtime counter deltas \
             are the source of truth for GPU-dispatched geomeans; plan-only pg_accel rows and \
             rows with stock executor fallback are excluded. Release-gate fields in this section \
             are derived from the existing workload counters and captured plan text.\n\n",
        );
        let _ = writeln!(out, "| Classification | Workloads |");
        let _ = writeln!(out, "|---|---:|");
        let _ = writeln!(out, "| Total measured rows | {total} |");
        let _ = writeln!(
            out,
            "| pg_accel Custom Scan plan selected | {plan_selected} |"
        );
        let _ = writeln!(out, "| GPU kernel dispatched | {gpu_kernel_dispatched} |");
        let _ = writeln!(
            out,
            "| Function/SRF kernel dispatched | {function_srf_kernel_dispatched} |"
        );
        let _ = writeln!(
            out,
            "| Runtime counter capture available | {counter_capture} |"
        );
        let _ = writeln!(
            out,
            "| Kernel counter delta > 0 | {kernel_counter_positive} |"
        );
        let _ = writeln!(
            out,
            "| pg_accel stock executor fallback delta > 0 | {stock_exec_positive} |"
        );
        let _ = writeln!(
            out,
            "| Custom Scan selected but no GPU dispatch | {plan_only} |"
        );
        let _ = writeln!(
            out,
            "| Planner declined/no credited pg_accel path | {planner_declined} |"
        );
        let _ = writeln!(
            out,
            "| Function/SRF kernel count | {function_kernel_count} |"
        );
        let _ = writeln!(out, "| Rows returned to CPU | {rows_returned_to_cpu} |");
        let _ = writeln!(
            out,
            "| GPU-resident pipeline reported | {gpu_resident_reported} |"
        );
        let _ = writeln!(
            out,
            "| GPU-dispatched Custom Scan without resident-pipeline proof | \
             {gpu_resident_not_reported} |"
        );
        let _ = writeln!(
            out,
            "| Custom Scan rows with recorded CPU boundary | \
             {gpu_resident_boundary_recorded} |"
        );
        out.push('\n');
    }

    fn render_dispatch_evidence(&self, out: &mut String) {
        let report = self.with_normalized_dispatch();
        if report.workloads.is_empty() {
            return;
        }

        out.push_str("### Dispatch Evidence By Row\n\n");
        out.push_str(
            "Each measured row is assigned one explicit release-gate classification. \
             `rows_returned_to_cpu` is the accel-side output consumption count; \
             `function_kernel_count` is populated only for credited function/SRF dispatch.\n\n",
        );
        let _ = writeln!(
            out,
            "| Workload | Scale | Classification | Function kernel count | Rows returned to CPU | \
             Correctness diff | GPU-resident pipeline | Resident boundary | Kernel delta | Rows dispatched | GPU rows processed | Stock fallback |"
        );
        let _ = writeln!(
            out,
            "|---|---|---|---:|---:|---|---|---|---:|---:|---:|---:|"
        );
        for w in &report.workloads {
            let classification = dispatch_evidence_label(w);
            let resident_boundary = w.gpu_resident_boundary_reason.as_deref().unwrap_or("-");
            let correctness_diff = w.correctness_diff_artifact.as_deref().unwrap_or("-");
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                markdown_cell(&w.name),
                format_rows(w.rows),
                classification,
                w.function_kernel_count,
                w.rows_returned_to_cpu,
                markdown_cell(correctness_diff),
                w.gpu_resident_pipeline.as_str(),
                markdown_cell(resident_boundary),
                w.gpu_kernel_execution_delta,
                w.pg_accel_rows_dispatched_delta,
                w.pg_accel_gpu_rows_processed_delta,
                w.pg_accel_stock_exec_delta,
            );
        }
        out.push('\n');
    }

    /// Render warnings for rows where both benchmark modes appear to have
    /// stayed on PostgreSQL-native plans, but the comparison is still not
    /// trustworthy because timing or plan shape diverged materially.
    fn render_no_dispatch_audit(&self, out: &mut String) {
        let report = self.with_normalized_dispatch();
        let mut anomalies: Vec<&WorkloadResult> = report
            .workloads
            .iter()
            .filter(|w| no_dispatch_anomaly(w))
            .collect();
        anomalies.sort_by(|a, b| {
            timing_skew_fraction(no_dispatch_speedup(b))
                .unwrap_or(0.0)
                .partial_cmp(&timing_skew_fraction(no_dispatch_speedup(a)).unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if anomalies.is_empty() {
            return;
        }

        out.push_str("## No-Dispatch Timing Audit\n\n");
        let _ = writeln!(
            out,
            "> WARNING: {} no-dispatch comparison(s) have >= {:.0}% timing skew or \
             materially different native plan shapes. Do not use these rows as GPU \
             performance conclusions.",
            anomalies.len(),
            NO_DISPATCH_TIMING_SKEW_THRESHOLD * 100.0,
        );
        out.push_str(
            ">\n> Action item: for Custom Scan rows, delete or quarantine any pg_accel CPU \
             fallback. For planner-declined rows with matching native plan shape, treat the \
             row as harness timing skew and rerun before drawing conclusions.\n\n",
        );
        let _ = writeln!(
            out,
            "| Workload | Scale | Median speedup | Timing skew | Plan shape | Accel plan | \
             PG plan | Action |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|---|---|");
        for w in anomalies {
            let speedup = no_dispatch_speedup(w);
            let skew = timing_skew_fraction(speedup).unwrap_or(f64::NAN) * 100.0;
            let timing = if no_dispatch_timing_skew(w) {
                format!("{skew:.1}%")
            } else {
                "<threshold".to_owned()
            };
            let plan_shape = if no_dispatch_plan_mismatch(w) {
                "DIFF"
            } else if w.plan_snippet.is_some() && w.baseline_plan_snippet.is_some() {
                "same"
            } else {
                "not captured"
            };
            let _ = writeln!(
                out,
                "| {} | {} | {:.2}x | {} | {} | `{}` | `{}` | {} |",
                markdown_cell(&w.name),
                format_rows(w.rows),
                speedup,
                timing,
                plan_shape,
                markdown_cell(&first_plan_signature_line(w.plan_snippet.as_deref())),
                markdown_cell(&first_plan_signature_line(
                    w.baseline_plan_snippet.as_deref(),
                )),
                no_dispatch_audit_action(w),
            );
        }
        out.push('\n');
    }

    /// Render the kernel coverage table — collapses workloads by the
    /// kernel class they exercise. action_items O / W11.
    ///
    /// Reviewer 1 Sin #17: the suite has ~127 workloads but only ~9 GPU
    /// kernels actually under test. The category table inflates this
    /// because the same `point_in_ring` kernel shows up under several
    /// category labels. Grouping by `kernel_class` makes the real
    /// coverage matrix obvious.
    fn render_kernel_coverage(&self, out: &mut String) {
        let report = self.with_normalized_dispatch();
        out.push_str("## Kernel Coverage\n\n");
        out.push_str(
            "Workloads grouped by the GPU kernel class they exercise. \
             A high workload count under a single kernel class means lots \
             of redundant variations of the same code path. Use this table \
             when adding new tests — prefer kernels with low coverage.\n\n",
        );
        let family_size = report.workloads.len() + report.crashes.len();
        let speedup_of = |w: &WorkloadResult| -> f64 { w.speedup_median_vs_parallel };

        // Group dispatched workloads by kernel_class.
        let mut by_kernel: BTreeMap<String, Vec<&WorkloadResult>> = BTreeMap::new();
        for w in &report.workloads {
            if !w.gpu_kernel_dispatched {
                continue;
            }
            by_kernel.entry(w.kernel_class.clone()).or_default().push(w);
        }

        let _ = writeln!(
            out,
            "| Kernel Class | Workloads | Distinct Scales | Geomean | \
             Sig Wins | Sig Losses |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|");
        for (kernel, ws) in &by_kernel {
            let speedups: Vec<f64> = ws.iter().map(|w| speedup_of(w)).collect();
            let gm = stats::geomean(&speedups);
            let counts = classify_significance(ws, family_size, 0.05);
            let mut scales: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
            for w in ws {
                scales.insert(w.rows);
            }
            let _ = writeln!(
                out,
                "| `{kernel}` | {} | {} | {gm:.2}x | {} | {} |",
                ws.len(),
                scales.len(),
                counts.sig_wins,
                counts.sig_losses,
            );
        }
        out.push('\n');
    }

    /// Evaluate the H3 lane gate against the report's workloads.
    ///
    /// The gate enforces three contracts derived from
    /// [`crate::workloads::benchmark_threshold_matrix_entry`] for the measured
    /// row scale, falling back to [`crate::workloads::h3_lane_class`] only when
    /// a matrix entry is unavailable:
    ///
    /// 1. **Winner below floor**: every H3 [`Winning`](crate::workloads::H3LaneClass::Winning)
    ///    row that DID dispatch a GPU kernel must have a median speedup of at
    ///    least [`H3_LANE_GATE_MIN_WARM_SPEEDUP`]. This is the Phase 0 ship bar
    ///    — a Winner that drops below PG-parallel parity is, by definition,
    ///    not winning.
    /// 2. **Winner missed dispatch**: every H3 Winner row that did NOT dispatch
    ///    a GPU kernel fails the gate. A Winner that silently routes to the
    ///    PG-native plan is the failure mode `test_h3_bulk_with_zero_kernel_delta_is_not_dispatched`
    ///    exists to catch — and the report-level gate makes it a CI failure.
    /// 3. **Parity dispatched unexpectedly**: every H3 [`Parity`](crate::workloads::H3LaneClass::Parity)
    ///    row that DID dispatch a GPU kernel fails the gate. Parity ops are
    ///    deliberately not registered for normal planner exposure; an
    ///    accidental re-registration is a quarantine breach.
    ///
    /// Returns an empty vector when every H3 row passes (or when no H3 rows
    /// are present — the gate is a release-time tool, not a unit-test tool).
    #[must_use]
    pub fn evaluate_h3_lane_gate(&self) -> Vec<H3LaneGateFailure> {
        let report = self.with_normalized_dispatch();
        let mut failures = Vec::new();
        for w in &report.workloads {
            let Some(class) = crate::workloads::h3_lane_class(&w.name) else {
                continue;
            };
            let matrix_expectation =
                crate::workloads::benchmark_threshold_matrix_entry(&w.name, w.rows)
                    .map(|entry| entry.expectation);
            let advisory_min_warm_speedup =
                crate::workloads::h3_winner_min_warm_speedup(&w.name).unwrap_or(1.0);
            match matrix_expectation {
                Some(crate::workloads::BenchmarkLaneExpectation::GpuWinner {
                    min_warm_speedup,
                }) => {
                    if !w.gpu_kernel_dispatched {
                        failures.push(H3LaneGateFailure {
                            workload: w.name.clone(),
                            rows: w.rows,
                            kind: H3LaneGateFailureKind::WinnerMissedDispatch,
                            speedup_median: w.speedup_median_vs_parallel,
                            gate_floor: H3_LANE_GATE_MIN_WARM_SPEEDUP,
                            advisory_min_warm_speedup: min_warm_speedup,
                        });
                    } else if !w.speedup_median_vs_parallel.is_finite()
                        || w.speedup_median_vs_parallel < H3_LANE_GATE_MIN_WARM_SPEEDUP
                    {
                        failures.push(H3LaneGateFailure {
                            workload: w.name.clone(),
                            rows: w.rows,
                            kind: H3LaneGateFailureKind::WinnerBelowFloor,
                            speedup_median: w.speedup_median_vs_parallel,
                            gate_floor: H3_LANE_GATE_MIN_WARM_SPEEDUP,
                            advisory_min_warm_speedup: min_warm_speedup,
                        });
                    }
                }
                Some(crate::workloads::BenchmarkLaneExpectation::NativeDecline { .. }) => {
                    if w.gpu_kernel_dispatched {
                        failures.push(H3LaneGateFailure {
                            workload: w.name.clone(),
                            rows: w.rows,
                            kind: H3LaneGateFailureKind::ParityUnexpectedlyDispatched,
                            speedup_median: w.speedup_median_vs_parallel,
                            gate_floor: H3_LANE_GATE_MIN_WARM_SPEEDUP,
                            advisory_min_warm_speedup,
                        });
                    }
                }
                None => match class {
                    crate::workloads::H3LaneClass::Winning { min_warm_speedup } => {
                        if !w.gpu_kernel_dispatched {
                            failures.push(H3LaneGateFailure {
                                workload: w.name.clone(),
                                rows: w.rows,
                                kind: H3LaneGateFailureKind::WinnerMissedDispatch,
                                speedup_median: w.speedup_median_vs_parallel,
                                gate_floor: H3_LANE_GATE_MIN_WARM_SPEEDUP,
                                advisory_min_warm_speedup: min_warm_speedup,
                            });
                        } else if !w.speedup_median_vs_parallel.is_finite()
                            || w.speedup_median_vs_parallel < H3_LANE_GATE_MIN_WARM_SPEEDUP
                        {
                            failures.push(H3LaneGateFailure {
                                workload: w.name.clone(),
                                rows: w.rows,
                                kind: H3LaneGateFailureKind::WinnerBelowFloor,
                                speedup_median: w.speedup_median_vs_parallel,
                                gate_floor: H3_LANE_GATE_MIN_WARM_SPEEDUP,
                                advisory_min_warm_speedup: min_warm_speedup,
                            });
                        }
                    }
                    crate::workloads::H3LaneClass::Parity => {
                        if w.gpu_kernel_dispatched {
                            failures.push(H3LaneGateFailure {
                                workload: w.name.clone(),
                                rows: w.rows,
                                kind: H3LaneGateFailureKind::ParityUnexpectedlyDispatched,
                                speedup_median: w.speedup_median_vs_parallel,
                                gate_floor: H3_LANE_GATE_MIN_WARM_SPEEDUP,
                                advisory_min_warm_speedup,
                            });
                        }
                    }
                },
            }
        }
        failures
    }

    /// Evaluate the benchmark-wide ship gate.
    ///
    /// This is the generic Phase 7 ratchet: any selected pg_accel row that
    /// crashes, fails to prove GPU dispatch, or dispatches below PostgreSQL
    /// parallel parity is a release-blocking failure. Expected-lane selection
    /// requirements that are workload-family-specific, such as H3 Winners,
    /// remain in their dedicated lane gates.
    #[must_use]
    pub fn evaluate_benchmark_ship_gate(&self) -> Vec<BenchmarkShipGateFailure> {
        let report = self.with_normalized_dispatch();
        let mut failures = Vec::new();

        for c in &report.crashes {
            failures.push(BenchmarkShipGateFailure {
                workload: c.workload.clone(),
                rows: c.rows,
                kind: BenchmarkShipGateFailureKind::Crash,
                speedup_median: f64::NAN,
                gate_floor: BENCHMARK_SHIP_GATE_MIN_SPEEDUP,
                detail: c.error.clone(),
            });
        }

        for w in &report.workloads {
            if w.custom_scan_selected_not_dispatched {
                failures.push(BenchmarkShipGateFailure {
                    workload: w.name.clone(),
                    rows: w.rows,
                    kind: BenchmarkShipGateFailureKind::SelectedPlanMissedDispatch,
                    speedup_median: w.speedup_median_vs_parallel,
                    gate_floor: BENCHMARK_SHIP_GATE_MIN_SPEEDUP,
                    detail: "pg_accel Custom Scan selected without credited GPU kernel dispatch"
                        .to_owned(),
                });
                continue;
            }

            if let Some(entry) = crate::workloads::benchmark_threshold_matrix_entry(&w.name, w.rows)
            {
                match entry.expectation {
                    crate::workloads::BenchmarkLaneExpectation::GpuWinner { min_warm_speedup } => {
                        let classification = w.dispatch_classification();
                        if !w.gpu_kernel_dispatched {
                            failures.push(BenchmarkShipGateFailure {
                                workload: w.name.clone(),
                                rows: w.rows,
                                kind: BenchmarkShipGateFailureKind::ExpectedWinnerMissedSelection,
                                speedup_median: w.speedup_median_vs_parallel,
                                gate_floor: BENCHMARK_SHIP_GATE_MIN_SPEEDUP,
                                detail: format!(
                                    "expected GPU winner missed dispatch/selection in lane `{}` \
                                     ({})",
                                    entry.lane, entry.threshold_basis
                                ),
                            });
                            continue;
                        }
                        if !w.dispatch_counter_captured
                            || w.gpu_kernel_execution_delta == 0
                            || classification.rows_returned_to_cpu == 0
                        {
                            failures.push(BenchmarkShipGateFailure {
                                workload: w.name.clone(),
                                rows: w.rows,
                                kind: BenchmarkShipGateFailureKind::ExpectedWinnerMissingEvidence,
                                speedup_median: w.speedup_median_vs_parallel,
                                gate_floor: min_warm_speedup.max(BENCHMARK_SHIP_GATE_MIN_SPEEDUP),
                                detail: format!(
                                    "expected GPU winner in lane `{}` lacks dispatch-counter or \
                                     output-consumption evidence ({})",
                                    entry.lane, entry.dispatch_evidence
                                ),
                            });
                            continue;
                        }
                        if threshold_lane_requires_resident_groupagg_logical_spec(entry.lane)
                            && !w
                                .plan_snippet
                                .as_deref()
                                .is_some_and(plan_contains_resident_groupagg_logical_evidence)
                        {
                            failures.push(BenchmarkShipGateFailure {
                                workload: w.name.clone(),
                                rows: w.rows,
                                kind: BenchmarkShipGateFailureKind::ExpectedWinnerMissingGroupAggLogicalSpec,
                                speedup_median: w.speedup_median_vs_parallel,
                                gate_floor: min_warm_speedup.max(BENCHMARK_SHIP_GATE_MIN_SPEEDUP),
                                detail: format!(
                                    "resident grouped winner in lane `{}` did not prove the shared \
                                     ResidentGroupAgg logical spec in EXPLAIN",
                                    entry.lane
                                ),
                            });
                            continue;
                        }
                        let winner_threshold =
                            min_warm_speedup.max(BENCHMARK_SHIP_GATE_MIN_SPEEDUP);
                        if !w.speedup_median_vs_parallel.is_finite()
                            || w.speedup_median_vs_parallel < winner_threshold
                        {
                            failures.push(BenchmarkShipGateFailure {
                                workload: w.name.clone(),
                                rows: w.rows,
                                kind: BenchmarkShipGateFailureKind::ExpectedWinnerBelowThreshold,
                                speedup_median: w.speedup_median_vs_parallel,
                                gate_floor: winner_threshold,
                                detail: format!(
                                    "expected GPU winner in lane `{}` below warm-run threshold \
                                     ({})",
                                    entry.lane, entry.cache_gate
                                ),
                            });
                            continue;
                        }
                        if !operation_cache_gate_verified(w, &report.methodology.cache_mode) {
                            failures.push(BenchmarkShipGateFailure {
                                workload: w.name.clone(),
                                rows: w.rows,
                                kind: BenchmarkShipGateFailureKind::ExpectedWinnerMissingCacheEvidence,
                                speedup_median: w.speedup_median_vs_parallel,
                                gate_floor: winner_threshold,
                                detail: format!(
                                    "expected GPU winner in lane `{}` requires cache-mode both \
                                     evidence for bounded cold-start cost ({})",
                                    entry.lane, entry.cache_gate
                                ),
                            });
                            continue;
                        }
                    }
                    crate::workloads::BenchmarkLaneExpectation::NativeDecline { reason } => {
                        if w.gpu_kernel_dispatched {
                            failures.push(BenchmarkShipGateFailure {
                                workload: w.name.clone(),
                                rows: w.rows,
                                kind: BenchmarkShipGateFailureKind::NativeDeclineUnexpectedDispatch,
                                speedup_median: w.speedup_median_vs_parallel,
                                gate_floor: BENCHMARK_SHIP_GATE_MIN_SPEEDUP,
                                detail: format!(
                                    "native-decline lane `{}` unexpectedly dispatched GPU work; \
                                     expected decline reason `{reason}`",
                                    entry.lane
                                ),
                            });
                            continue;
                        }
                        if !native_decline_reason_verified(w, reason) {
                            failures.push(BenchmarkShipGateFailure {
                                workload: w.name.clone(),
                                rows: w.rows,
                                kind: BenchmarkShipGateFailureKind::NativeDeclineReasonMissing,
                                speedup_median: w.speedup_median_vs_parallel,
                                gate_floor: BENCHMARK_SHIP_GATE_MIN_SPEEDUP,
                                detail: format!(
                                    "native-decline lane `{}` did not prove expected planner \
                                     rejection reason `{reason}`",
                                    entry.lane
                                ),
                            });
                            continue;
                        }
                    }
                }
            }

            if w.plan_selected
                && w.gpu_kernel_dispatched
                && w.gpu_resident_pipeline != GpuResidentPipelineStatus::Reported
            {
                failures.push(BenchmarkShipGateFailure {
                    workload: w.name.clone(),
                    rows: w.rows,
                    kind: BenchmarkShipGateFailureKind::SelectedPlanNotGpuResident,
                    speedup_median: w.speedup_median_vs_parallel,
                    gate_floor: BENCHMARK_SHIP_GATE_MIN_SPEEDUP,
                    detail: "selected GPU-dispatched Custom Scan did not report \
                             `GPU Resident Pipeline: true`"
                        .to_owned(),
                });
                continue;
            }

            if w.gpu_kernel_dispatched
                && (!w.speedup_median_vs_parallel.is_finite()
                    || w.speedup_median_vs_parallel < BENCHMARK_SHIP_GATE_MIN_SPEEDUP)
            {
                failures.push(BenchmarkShipGateFailure {
                    workload: w.name.clone(),
                    rows: w.rows,
                    kind: BenchmarkShipGateFailureKind::DispatchedBelowParity,
                    speedup_median: w.speedup_median_vs_parallel,
                    gate_floor: BENCHMARK_SHIP_GATE_MIN_SPEEDUP,
                    detail: "credited GPU dispatch below PostgreSQL parallel parity".to_owned(),
                });
            }
        }

        failures
    }

    /// Append the `### Benchmark Ship Gate Failures` section to `out` when the
    /// generic release gate produced any failures.
    fn render_benchmark_ship_gate(&self, out: &mut String) {
        let failures = self.evaluate_benchmark_ship_gate();
        if failures.is_empty() {
            return;
        }
        out.push_str("### Benchmark Ship Gate Failures\n\n");
        out.push_str(
            "Hard release gate for selected pg_accel benchmark rows. Any crash, \
             selected Custom Scan without credited GPU dispatch, expected GPU \
             winner that stays native, expected winner missing dispatch/output \
             evidence, threshold evidence, or required cache-mode evidence, \
             native-decline lane that dispatches, selected GPU-dispatched Custom \
             Scan without `GPU Resident Pipeline: true`, or credited GPU dispatch \
             below PostgreSQL-parallel parity exits non-zero in the CLI.\n\n",
        );
        let _ = writeln!(
            out,
            "Gate floor: **{BENCHMARK_SHIP_GATE_MIN_SPEEDUP:.2}x** median speedup."
        );
        out.push('\n');
        let _ = writeln!(
            out,
            "| Workload | Scale | Failure | Observed Speedup | Gate Floor | Detail |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|");
        for f in &failures {
            let observed = if f.speedup_median.is_finite() {
                format!("{:.2}x", f.speedup_median)
            } else {
                "n/a".to_owned()
            };
            let detail = f.detail.replace('|', "\\|");
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {:.2}x | {} |",
                f.workload,
                format_rows(f.rows),
                f.kind.label(),
                observed,
                f.gate_floor,
                detail,
            );
        }
        out.push('\n');
    }

    /// Append the `### H3 Lane Gate Failures` section to `out` when the gate
    /// produced any failures. Renders nothing when the gate passes.
    fn render_h3_lane_gate(&self, out: &mut String) {
        let failures = self.evaluate_h3_lane_gate();
        if failures.is_empty() {
            return;
        }
        out.push_str("### H3 Lane Gate Failures\n\n");
        out.push_str(
            "Hard gate against the H3 row-scale threshold matrix, with \
             `pg_accel_bench/src/workloads/mod.rs::h3_lane_class` as the \
             fallback classifier. Expected Winning rows must dispatch a GPU \
             kernel and beat PG-parallel parity; native-decline rows must stay \
             native. A failure here means the bench process exits non-zero — \
             CI will fail.\n\n",
        );
        let _ = writeln!(
            out,
            "Gate floor: **{H3_LANE_GATE_MIN_WARM_SPEEDUP:.2}x** (uniform across \
             all H3 Winners; per-Winner advisory thresholds shown below)."
        );
        out.push('\n');
        let _ = writeln!(
            out,
            "| Workload | Scale | Failure | Observed Speedup | Gate Floor | Per-Winner Advisory |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|");
        for f in &failures {
            let observed = if f.speedup_median.is_finite() {
                format!("{:.2}x", f.speedup_median)
            } else {
                "n/a".to_owned()
            };
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {:.2}x | {:.2}x |",
                f.workload,
                format_rows(f.rows),
                f.kind.label(),
                observed,
                f.gate_floor,
                f.advisory_min_warm_speedup,
            );
        }
        out.push('\n');
    }
}

/// Uniform floor enforced by the H3 lane gate.
///
/// Set to `1.0x` so a Winner that drops below PG-parallel parity always
/// fails. Per-Winner advisory thresholds (from
/// [`crate::workloads::h3_winner_min_warm_speedup`]) are reported next to
/// each failing row but do NOT participate in the pass/fail decision —
/// the gate boundary stays at the Phase 0 ship bar.
pub const H3_LANE_GATE_MIN_WARM_SPEEDUP: f64 = 1.0;

/// Generic release gate floor for selected GPU benchmark rows.
///
/// Any credited GPU dispatch below PostgreSQL parallel parity fails the
/// benchmark process. Workloads that intentionally stay native are excluded
/// because they do not dispatch a GPU kernel and are handled by planner
/// decline evidence instead.
pub const BENCHMARK_SHIP_GATE_MIN_SPEEDUP: f64 = 1.0;

/// Kind of failure observed by [`BenchReport::evaluate_benchmark_ship_gate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchmarkShipGateFailureKind {
    /// The benchmark harness recorded a crash for this workload/scale.
    Crash,
    /// A pg_accel Custom Scan was selected but no GPU kernel dispatch was
    /// credited by plan/counter evidence.
    SelectedPlanMissedDispatch,
    /// A selected GPU-dispatched Custom Scan did not report
    /// `GPU Resident Pipeline: true`.
    SelectedPlanNotGpuResident,
    /// A GPU-dispatched row fell below the PostgreSQL-parallel parity floor.
    DispatchedBelowParity,
    /// A benchmark matrix cell declared as a GPU winner stayed native or did
    /// not prove credited GPU dispatch.
    ExpectedWinnerMissedSelection,
    /// A benchmark matrix cell declared as a GPU winner dispatched but did not
    /// show the required dispatch counter and consumed-output evidence.
    ExpectedWinnerMissingEvidence,
    /// A benchmark matrix cell declared as a GPU winner dispatched but missed
    /// its operation-specific warm-run threshold.
    ExpectedWinnerBelowThreshold,
    /// A resident grouped-aggregate winner did not prove the shared logical
    /// groupagg spec in its plan artifact.
    ExpectedWinnerMissingGroupAggLogicalSpec,
    /// A benchmark matrix cell declared as an H3/raster GPU winner did not use
    /// a cache mode that proves bounded cold-start cost.
    ExpectedWinnerMissingCacheEvidence,
    /// A benchmark matrix cell declared as native-decline dispatched GPU work.
    NativeDeclineUnexpectedDispatch,
    /// A native-decline matrix cell did not prove its expected planner reason.
    NativeDeclineReasonMissing,
}

impl BenchmarkShipGateFailureKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Crash => "crash",
            Self::SelectedPlanMissedDispatch => "selected_plan_missed_dispatch",
            Self::SelectedPlanNotGpuResident => "selected_plan_not_gpu_resident",
            Self::DispatchedBelowParity => "dispatched_below_parity",
            Self::ExpectedWinnerMissedSelection => "expected_winner_missed_selection",
            Self::ExpectedWinnerMissingEvidence => "expected_winner_missing_evidence",
            Self::ExpectedWinnerBelowThreshold => "expected_winner_below_threshold",
            Self::ExpectedWinnerMissingGroupAggLogicalSpec => {
                "expected_winner_missing_groupagg_logical_spec"
            }
            Self::ExpectedWinnerMissingCacheEvidence => "expected_winner_missing_cache_evidence",
            Self::NativeDeclineUnexpectedDispatch => "native_decline_unexpected_dispatch",
            Self::NativeDeclineReasonMissing => "native_decline_reason_missing",
        }
    }
}

/// One benchmark ship-gate failure record.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkShipGateFailure {
    pub workload: String,
    pub rows: usize,
    pub kind: BenchmarkShipGateFailureKind,
    pub speedup_median: f64,
    pub gate_floor: f64,
    pub detail: String,
}

/// Kind of failure observed by [`BenchReport::evaluate_h3_lane_gate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H3LaneGateFailureKind {
    /// An H3 Winner dispatched a GPU kernel but its median speedup is below
    /// the gate floor (or non-finite).
    WinnerBelowFloor,
    /// An H3 Winner did NOT dispatch a GPU kernel at all.
    WinnerMissedDispatch,
    /// An H3 Parity workload dispatched a GPU kernel under normal planner
    /// exposure (a quarantine breach).
    ParityUnexpectedlyDispatched,
}

impl H3LaneGateFailureKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::WinnerBelowFloor => "winner_below_floor",
            Self::WinnerMissedDispatch => "winner_missed_dispatch",
            Self::ParityUnexpectedlyDispatched => "parity_unexpectedly_dispatched",
        }
    }
}

/// One H3 lane gate failure record.
#[derive(Debug, Clone, PartialEq)]
pub struct H3LaneGateFailure {
    pub workload: String,
    pub rows: usize,
    pub kind: H3LaneGateFailureKind,
    pub speedup_median: f64,
    pub gate_floor: f64,
    pub advisory_min_warm_speedup: f64,
}

struct LedgerMergeItem<'a> {
    workload: Option<&'a WorkloadResult>,
    workload_name: &'a str,
    row_count: usize,
    source: &'a str,
    priority: &'a str,
    next_track: &'a str,
    speedup: Option<f64>,
    gate_floor: Option<f64>,
    detail: String,
}

fn merge_ledger_item(
    rows: &mut BTreeMap<(String, usize), BenchmarkFailureLedgerRow>,
    item: LedgerMergeItem<'_>,
) {
    let LedgerMergeItem {
        workload,
        workload_name,
        row_count,
        source,
        priority,
        next_track,
        speedup,
        gate_floor,
        detail,
    } = item;
    let key = (workload_name.to_owned(), row_count);
    let entry = rows
        .entry(key)
        .or_insert_with(|| BenchmarkFailureLedgerRow {
            workload: workload_name.to_owned(),
            rows: row_count,
            category: workload.map_or_else(|| "unknown".to_owned(), |w| w.category.clone()),
            kernel_class: workload.map_or_else(|| "unknown".to_owned(), |w| w.kernel_class.clone()),
            classification: workload.map_or_else(
                || "crash_or_missing_result".to_owned(),
                ledger_classification,
            ),
            priority: priority.to_owned(),
            next_track: next_track.to_owned(),
            sources: Vec::new(),
            speedup_median_vs_parallel: workload
                .and_then(|w| finite_option(w.speedup_median_vs_parallel))
                .or_else(|| speedup.and_then(finite_option)),
            gate_floor: gate_floor.and_then(finite_option),
            accel_median_ms: workload.and_then(|w| finite_option(w.accel_median_ms)),
            parallel_median_ms: workload.and_then(|w| finite_option(w.parallel_median_ms)),
            repro_command: None,
            plan_snippet_artifact: None,
            correctness_diff_artifact: None,
            log_tail_artifacts: Vec::new(),
            details: Vec::new(),
        });

    push_unique(&mut entry.sources, source);
    push_unique_string(&mut entry.details, detail);
    if ledger_priority_rank(priority) < ledger_priority_rank(&entry.priority) {
        priority.clone_into(&mut entry.priority);
        next_track.clone_into(&mut entry.next_track);
    }
    if entry.speedup_median_vs_parallel.is_none() {
        entry.speedup_median_vs_parallel = speedup.and_then(finite_option);
    }
    if let Some(gate_floor) = gate_floor.and_then(finite_option) {
        entry.gate_floor = Some(
            entry
                .gate_floor
                .map_or(gate_floor, |old| old.max(gate_floor)),
        );
    }
}

fn merge_crash_ledger_item(
    rows: &mut BTreeMap<(String, usize), BenchmarkFailureLedgerRow>,
    crash: &CrashedScale,
) {
    merge_ledger_item(
        rows,
        LedgerMergeItem {
            workload: None,
            workload_name: &crash.workload,
            row_count: crash.rows,
            source: "crash_inventory",
            priority: "p1_release_blocker",
            next_track: ledger_track(&crash.workload, "", "", Some("crash")),
            speedup: None,
            gate_floor: Some(BENCHMARK_SHIP_GATE_MIN_SPEEDUP),
            detail: crash_ledger_detail(crash),
        },
    );

    if let Some(entry) = rows.get_mut(&(crash.workload.clone(), crash.rows)) {
        entry.repro_command.clone_from(&crash.repro_command);
        entry
            .plan_snippet_artifact
            .clone_from(&crash.plan_snippet_artifact);
        entry
            .correctness_diff_artifact
            .clone_from(&crash.correctness_diff_artifact);
        entry
            .log_tail_artifacts
            .clone_from(&crash.log_tail_artifacts);
    }
}

fn crash_ledger_detail(crash: &CrashedScale) -> String {
    let mut parts = Vec::new();
    if let Some(plan) = &crash.plan_snippet_artifact {
        parts.push(format!("plan={plan}"));
    }
    if let Some(correctness) = &crash.correctness_diff_artifact {
        parts.push(format!("correctness={correctness}"));
    }
    if !crash.log_tail_artifacts.is_empty() {
        parts.push(format!("logs={}", crash.log_tail_artifacts.join(",")));
    }
    if let Some(repro) = &crash.repro_command {
        parts.push(format!("repro={repro}"));
    }

    if parts.is_empty() {
        "crash inventory has no attached artifacts".to_owned()
    } else {
        format!("crash inventory artifacts: {}", parts.join("; "))
    }
}

fn ledger_crash_artifact_summary(row: &BenchmarkFailureLedgerRow) -> String {
    let mut parts = Vec::new();
    if let Some(plan) = &row.plan_snippet_artifact {
        parts.push(format!("plan: {plan}"));
    }
    if let Some(correctness) = &row.correctness_diff_artifact {
        parts.push(format!("correctness: {correctness}"));
    }
    if !row.log_tail_artifacts.is_empty() {
        parts.push(format!("logs: {}", row.log_tail_artifacts.join(", ")));
    }
    if let Some(repro) = &row.repro_command {
        parts.push(format!("repro: `{repro}`"));
    }

    if parts.is_empty() {
        "-".to_owned()
    } else {
        parts.join("<br>")
    }
}

fn push_unique(items: &mut Vec<String>, item: &str) {
    if !items.iter().any(|existing| existing == item) {
        items.push(item.to_owned());
    }
}

fn push_unique_string(items: &mut Vec<String>, item: String) {
    if !items.iter().any(|existing| existing == &item) {
        items.push(item);
    }
}

fn finite_option(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn ledger_classification(workload: &WorkloadResult) -> String {
    if workload.gpu_kernel_dispatched {
        if workload.function_srf_kernel_dispatched {
            "function_srf_gpu_dispatch".to_owned()
        } else {
            "custom_scan_gpu_dispatch".to_owned()
        }
    } else if workload.custom_scan_selected_not_dispatched {
        "custom_scan_selected_not_dispatched".to_owned()
    } else if workload.planner_declined {
        "planner_declined_native".to_owned()
    } else if workload.plan_selected {
        "custom_scan_selected_without_gpu_credit".to_owned()
    } else {
        "native_or_unclassified".to_owned()
    }
}

fn ledger_priority_rank(priority: &str) -> u8 {
    match priority {
        "p1_release_blocker" => 0,
        "p2_architecture_gap" => 1,
        "p3_parity_noise" => 2,
        _ => 3,
    }
}

fn ledger_track(
    workload: &str,
    category: &str,
    kernel_class: &str,
    failure_hint: Option<&str>,
) -> &'static str {
    if failure_hint.is_some_and(|hint| hint.contains("cache"))
        || workload.starts_with("h3_")
        || category == "gpu_h3"
    {
        return "h3_cache_evidence_and_lane_policy";
    }
    if failure_hint.is_some_and(|hint| hint.contains("hashjoin"))
        || category == "gpu_hashjoin"
        || kernel_class == "hash_join"
        || workload.contains("hashjoin")
        || workload.contains("hash_join")
        || workload == "mixed_join_agg"
    {
        return "resident_hashjoin_and_join_aggregate";
    }
    if workload == "filtered_grouped_agg" && failure_hint == Some("gpu_dispatched_below_parity") {
        return "resident_groupagg_small_input_overhead";
    }
    if category == "gpu_hashagg"
        || kernel_class == "hash_agg"
        || workload.contains("grouped_agg")
        || workload.starts_with("hashagg_")
        || workload == "timeseries_sensor_rollup"
        || workload == "dictionary_grouped_agg"
    {
        return "resident_groupagg_small_input_overhead";
    }
    if category == "gpu_reduce" || category == "gpu_expr" || kernel_class == "reduce" {
        return "resident_scan_reduce_expression_fusion";
    }
    if category == "gpu_window" || kernel_class == "window" {
        return "resident_segmented_window";
    }
    if category == "gpu_sort" || kernel_class == "sort" || workload.contains("topk") {
        return "resident_sort_topk";
    }
    if category == "gpu_spatial" || kernel_class == "point_in_ring" {
        return "geo_spatial_resident_pipeline";
    }
    if category == "gpu_raster" || kernel_class == "raster" {
        return "raster_resident_pipeline";
    }
    if category == "mixed" {
        return "mixed_workload_resident_fusion";
    }
    "benchmark_gate_policy_or_parity_noise"
}

/// Build a benchmark report, carrying the observed-GUC snapshot, timing mode,
/// and cache mode through so renderers can label columns correctly.
#[allow(
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    clippy::option_if_let_else
)]
pub fn generate_report(
    workloads: Vec<WorkloadResult>,
    crashes: Vec<CrashedScale>,
    connection: Option<&str>,
    iterations: usize,
    warmup: usize,
    observed: Option<crate::runner::ObservedGucs>,
    timing_mode: crate::runner::TimingMode,
    cache_mode: crate::runner::CacheMode,
) -> BenchReport {
    let hardware = Some(HardwareProfile::detect());

    // Prefer the observed snapshot (from inside the benchmarked session)
    // over the fallback settings-table query, because the observed
    // snapshot was already validated against the requested profile.
    let gucs = if let Some(snapshot) = &observed {
        Some(GucSettings {
            settings: snapshot.settings.clone(),
        })
    } else {
        connection.and_then(|c| GucSettings::from_connection(c).ok())
    };
    let postmaster_start_time = observed
        .as_ref()
        .and_then(|s| s.postmaster_start_time.clone());

    // Collect unique row scales from results (preserving order).
    let mut row_scales: Vec<usize> = Vec::new();
    for w in &workloads {
        if !row_scales.contains(&w.rows) {
            row_scales.push(w.rows);
        }
    }
    row_scales.sort_unstable();

    let timing_label = match timing_mode {
        crate::runner::TimingMode::RawWallClock => "raw-wallclock",
        crate::runner::TimingMode::ExplainAnalyze => "explain-analyze",
        crate::runner::TimingMode::Both => "both",
    };
    let cache_label = match cache_mode {
        crate::runner::CacheMode::Warm => "warm",
        crate::runner::CacheMode::Cold => "cold",
        crate::runner::CacheMode::Both => "both",
    };
    let harness_profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let methodology = Methodology {
        iterations,
        warmup,
        row_scales,
        ordering: "randomized per iteration (accel-first vs baseline-first)".to_owned(),
        statistical_tests: vec![
            "Paired t-test (two-tailed, p < 0.05)".to_owned(),
            "Bonferroni correction (family-wise alpha)".to_owned(),
            "Cohen's d effect size (|d| >= 0.5 gate, action_items C9)".to_owned(),
            "95% CI via t-distribution".to_owned(),
            "Outlier detection (> 3 sigma)".to_owned(),
        ],
        timing_mode: timing_label.to_owned(),
        cache_mode: cache_label.to_owned(),
        harness_profile: harness_profile.to_owned(),
    };
    let report = BenchReport {
        hardware,
        gucs,
        methodology,
        workloads,
        artifact_dir: None,
        crashes,
        postmaster_start_time,
    };
    report.with_normalized_dispatch()
}

// ---------------------------------------------------------------------------
// Platform detection helpers
// ---------------------------------------------------------------------------

fn detect_os() -> String {
    std::env::consts::OS.to_owned()
}

fn detect_os_version() -> String {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map_or_else(|| "unknown".to_owned(), |s| s.trim().to_owned())
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|content| {
                content
                    .lines()
                    .find(|l| l.starts_with("PRETTY_NAME="))
                    .map(|l| {
                        l.trim_start_matches("PRETTY_NAME=")
                            .trim_matches('"')
                            .to_owned()
                    })
            })
            .unwrap_or_else(|| "unknown".to_owned())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "unknown".to_owned()
    }
}

fn detect_cpu() -> String {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map_or_else(|| "unknown".to_owned(), |s| s.trim().to_owned())
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|content| {
                content
                    .lines()
                    .find(|l| l.starts_with("model name"))
                    .and_then(|l| l.split(':').nth(1))
                    .map(|s| s.trim().to_owned())
            })
            .unwrap_or_else(|| "unknown".to_owned())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "unknown".to_owned()
    }
}

fn detect_cpu_cores() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

fn detect_memory() -> String {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map_or_else(
                || "unknown".to_owned(),
                |bytes| format!("{} GB", bytes / (1024 * 1024 * 1024)),
            )
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|content| {
                content
                    .lines()
                    .find(|l| l.starts_with("MemTotal:"))
                    .and_then(|l| {
                        l.split_whitespace()
                            .nth(1)
                            .and_then(|s| s.parse::<u64>().ok())
                    })
                    .map(|kb| format!("{} GB", kb / (1024 * 1024)))
            })
            .unwrap_or_else(|| "unknown".to_owned())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "unknown".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_workload_result(
        name: &str,
        rows: usize,
        accel_ms: f64,
        baseline_ms: f64,
    ) -> WorkloadResult {
        let iterations: Vec<IterationResult> = (0..10)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let jitter = (i as f64) * 0.1;
                IterationResult {
                    accel_ms: accel_ms + jitter,
                    parallel_ms: baseline_ms + jitter,
                    cache_purge: CachePurgeState::NotRequested,
                    cache_state: CacheState::Warm,
                }
            })
            .collect();
        WorkloadResult::from_iterations(
            name.to_owned(),
            format!("Mock workload: {name}"),
            "gpu".to_owned(),
            classify_kernel("unclassified"),
            rows,
            iterations,
            true,
        )
    }

    #[test]
    fn test_classify_kernel_separates_h3_parent_count() {
        assert_eq!(classify_kernel("h3_cell_to_parent"), "h3_cell_to_parent");
        assert_eq!(classify_kernel("h3_bulk"), "h3_latlng");
        assert_eq!(classify_kernel("reduce_f64_sum"), "resident_f64_reduce");
        assert_eq!(classify_kernel("reduce_f64_minmax"), "resident_f64_reduce");
        assert_eq!(classify_kernel("reduce_f64_stats"), "resident_f64_reduce");
        assert_eq!(
            classify_kernel("hashagg_f64_aggs"),
            "resident_f64_grouped_stats"
        );
        assert_eq!(
            classify_kernel("case_when_expression_grouped_agg"),
            "hash_agg"
        );
        assert_eq!(
            classify_kernel("case_when_range_expression_grouped_agg"),
            "hash_agg"
        );
        assert_eq!(
            classify_kernel("case_when_value_predicate_expression_grouped_agg"),
            "hash_agg"
        );
        assert_eq!(
            classify_kernel("case_when_null_predicate_expression_grouped_agg"),
            "hash_agg"
        );
        assert_eq!(
            classify_kernel("case_when_or_expression_grouped_agg"),
            "hash_agg"
        );
        assert_eq!(
            classify_kernel("case_when_in_expression_grouped_agg"),
            "hash_agg"
        );
        assert_eq!(
            classify_kernel("case_when_not_expression_grouped_agg"),
            "hash_agg"
        );
        assert_eq!(classify_kernel("ssbm_q4_3"), "resident_star_groupagg");
        assert_eq!(classify_kernel("mixed_join_agg"), "resident_star_groupagg");
        assert_eq!(
            classify_kernel("gpu_hashjoin_filter"),
            "resident_star_groupagg"
        );
    }

    fn mock_report(workloads: Vec<WorkloadResult>) -> BenchReport {
        BenchReport {
            hardware: Some(HardwareProfile {
                os: "linux".to_owned(),
                os_version: "6.1".to_owned(),
                arch: "x86_64".to_owned(),
                cpu: "Test CPU".to_owned(),
                cpu_cores: 8,
                memory_gb: "32 GB".to_owned(),
            }),
            gucs: Some(GucSettings {
                settings: vec![
                    ("work_mem".to_owned(), "256MB".to_owned()),
                    ("shared_buffers".to_owned(), "4GB".to_owned()),
                ],
            }),
            postmaster_start_time: None,
            methodology: Methodology {
                iterations: 30,
                warmup: 5,
                row_scales: vec![1_000, 10_000, 100_000, 1_000_000, 10_000_000],
                ordering: "randomized".to_owned(),
                statistical_tests: vec!["Paired t-test".to_owned()],
                timing_mode: "raw".to_owned(),
                cache_mode: "warm".to_owned(),
                harness_profile: "test".to_owned(),
            },
            workloads,
            artifact_dir: None,
            crashes: Vec::new(),
        }
    }

    fn with_cache_mode(mut report: BenchReport, mode: &str) -> BenchReport {
        report.methodology.cache_mode = mode.to_owned();
        report
    }

    fn mark_no_dispatch(
        mut result: WorkloadResult,
        accel_plan: &str,
        baseline_plan: &str,
    ) -> WorkloadResult {
        result.plan_selected = false;
        result.gpu_kernel_dispatched = false;
        result.function_srf_kernel_dispatched = false;
        result.plan_snippet = Some(accel_plan.to_owned());
        result.baseline_plan_snippet = Some(baseline_plan.to_owned());
        result
    }

    #[test]
    fn test_workload_result_from_iterations_basic() {
        let result = mock_workload_result("test_wl", 100_000, 10.0, 20.0);
        assert_eq!(result.name, "test_wl");
        assert_eq!(result.rows, 100_000);
        assert!(result.speedup_vs_parallel > 1.0);
        assert_eq!(result.iterations.len(), 10);
    }

    #[test]
    fn test_workload_result_single_iteration() {
        let iters = vec![IterationResult {
            accel_ms: 5.0,
            parallel_ms: 10.0,
            cache_purge: CachePurgeState::NotRequested,
            cache_state: CacheState::Warm,
        }];
        let result = WorkloadResult::from_iterations(
            "one".to_owned(),
            "desc".to_owned(),
            "gpu".to_owned(),
            classify_kernel("unclassified"),
            1000,
            iters,
            true,
        );
        assert!((result.accel_mean_ms - 5.0).abs() < f64::EPSILON);
        assert!((result.speedup_vs_parallel - 2.0).abs() < f64::EPSILON);
    }

    /// Minimal `CacheModeSummary` builder for tests that need to plant a
    /// specific warm/cold subsample summary directly, independent of
    /// `WorkloadResult::from_iterations`.
    fn mock_cache_mode_summary(cache_state: CacheState, speedup_median: f64) -> CacheModeSummary {
        CacheModeSummary {
            cache_state,
            n: 5,
            accel_mean_ms: 10.0,
            accel_median_ms: 10.0,
            accel_stddev_ms: 0.0,
            accel_p95_ms: 10.0,
            accel_cv_pct: 0.0,
            accel_ci_95: (10.0, 10.0),
            parallel_mean_ms: 10.0 * speedup_median,
            parallel_median_ms: 10.0 * speedup_median,
            parallel_stddev_ms: 0.0,
            parallel_p95_ms: 10.0 * speedup_median,
            parallel_cv_pct: 0.0,
            parallel_ci_95: (10.0 * speedup_median, 10.0 * speedup_median),
            speedup_mean_vs_parallel: speedup_median,
            speedup_median_vs_parallel: speedup_median,
            p_value_vs_parallel: 0.0,
            cohens_d_vs_parallel: 5.0,
            effect_size_meaningful: true,
        }
    }

    /// `warm_speedup_for_gate` (and therefore `gpu_winner_evidence_verified`)
    /// must read `WorkloadResult::warm_summary` directly rather than the flat
    /// `speedup_median_vs_parallel`. Plant a flat speedup that would FAIL a
    /// 1.5x gate and a `warm_summary` speedup that would PASS it — the gate
    /// must follow the warm summary.
    #[test]
    fn test_warm_speedup_for_gate_reads_warm_summary_over_flat_field() {
        let mut w = mock_workload_result("gpu_reduce_sum", 1_000_000, 10.0, 20.0);
        w.speedup_median_vs_parallel = 0.5; // would fail a 1.5x gate if read directly
        w.warm_summary = Some(mock_cache_mode_summary(CacheState::Warm, 2.0));
        assert!((warm_speedup_for_gate(&w) - 2.0).abs() < f64::EPSILON);

        // Warm-only runs (no `warm_summary` populated) fall back to the flat
        // field, which already describes the only subsample measured.
        let mut warm_only = mock_workload_result("gpu_reduce_sum", 1_000_000, 10.0, 20.0);
        warm_only.warm_summary = None;
        assert!(
            (warm_speedup_for_gate(&warm_only) - warm_only.speedup_median_vs_parallel).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn test_gpu_winner_evidence_verified_gates_on_warm_summary_not_flat_speedup() {
        let mut w = mock_workload_result("gpu_reduce_sum", 1_000_000, 10.0, 20.0);
        w.plan_selected = true;
        w.gpu_kernel_dispatched = true;
        w.dispatch_counter_captured = true;
        w.gpu_kernel_execution_delta = 1;
        w.rows_returned_to_cpu = 100;
        w.accel_output_rows_consumed = 100;
        // Flat field alone would fail a 1.5x gate...
        w.speedup_median_vs_parallel = 0.5;
        // ...but the warm-subsample evidence clears it, and the gate must
        // follow the warm-subsample evidence.
        w.warm_summary = Some(mock_cache_mode_summary(CacheState::Warm, 2.0));
        assert!(gpu_winner_evidence_verified(
            &w,
            "unrelated_lane",
            1.5,
            "warm"
        ));

        // With no warm_summary at all, the flat field (0.5x) governs and the
        // gate correctly fails.
        w.warm_summary = None;
        assert!(!gpu_winner_evidence_verified(
            &w,
            "unrelated_lane",
            1.5,
            "warm"
        ));
    }

    /// The h3_/raster_ cache-mode-both gate must require a populated
    /// `warm_summary` on the row itself, not merely a `cache_mode == "both"`
    /// methodology string. A degenerate row (e.g. every iteration was
    /// classified Cold, or the row was hand-built without a warm subsample)
    /// must not launder a global "both" config into a false per-row pass.
    #[test]
    fn test_operation_cache_gate_verified_requires_row_level_warm_summary() {
        let mut w = mock_workload_result("h3_bulk", 1_000_000, 10.0, 50.0);
        w.category = "gpu_h3".to_owned();
        assert!(
            w.warm_summary.is_some(),
            "sanity: mock_workload_result's all-Warm iterations must populate warm_summary"
        );
        assert!(operation_cache_gate_verified(&w, "both"));

        w.warm_summary = None;
        assert!(
            !operation_cache_gate_verified(&w, "both"),
            "cache_mode=both alone must not satisfy the gate without a row-level warm_summary"
        );

        // And the existing requirement still holds: cache_mode must actually
        // be "both", regardless of warm_summary.
        w.warm_summary = Some(mock_cache_mode_summary(CacheState::Warm, 2.0));
        assert!(!operation_cache_gate_verified(&w, "warm"));
    }

    /// `native_decline_reason_verified` must read only
    /// `WorkloadResult::native_decline_evidence`. Plan-snippet text — even if
    /// it happens to mention a decline-sounding reason — is not evidence by
    /// itself; only the tagged evidence field, sourced from a real planner
    /// rejection, counts.
    #[test]
    fn test_native_decline_reason_verified_ignores_plan_snippet_text() {
        let mut w = mock_workload_result("gpu_sort_multikey", 100_000, 10.0, 10.0);
        w.plan_snippet = Some(
            "Sort\n  -> Seq Scan on bench_sort_multi\n  Note: sort_multikey_no_gpu_kernel"
                .to_owned(),
        );
        assert!(w.native_decline_evidence.is_none());
        assert!(!native_decline_reason_verified(
            &w,
            "sort_multikey_no_gpu_kernel"
        ));

        w.native_decline_evidence = Some(NativeDeclineEvidence {
            reason: "sort_multikey_no_gpu_kernel".to_owned(),
            source: DeclineReasonSource::PlannerReported,
        });
        assert!(native_decline_reason_verified(
            &w,
            "sort_multikey_no_gpu_kernel"
        ));
    }

    /// `ExpectedUnconfirmed` decline evidence must never verify, and must
    /// render as "expected, unconfirmed" via `native_decline_evidence_label`.
    #[test]
    fn test_native_decline_reason_verified_rejects_unconfirmed_and_mismatched_reason() {
        let mut w = mock_workload_result("gpu_sort_multikey", 100_000, 10.0, 10.0);
        w.native_decline_evidence = Some(NativeDeclineEvidence {
            reason: "sort_multikey_no_gpu_kernel".to_owned(),
            source: DeclineReasonSource::ExpectedUnconfirmed,
        });
        assert!(!native_decline_reason_verified(
            &w,
            "sort_multikey_no_gpu_kernel"
        ));
        assert_eq!(
            native_decline_evidence_label(&w),
            "sort_multikey_no_gpu_kernel (expected, unconfirmed)"
        );

        // A mismatched reason string must not verify even when the source is
        // PlannerReported.
        w.native_decline_evidence = Some(NativeDeclineEvidence {
            reason: "some_other_reason".to_owned(),
            source: DeclineReasonSource::PlannerReported,
        });
        assert!(!native_decline_reason_verified(
            &w,
            "sort_multikey_no_gpu_kernel"
        ));

        w.native_decline_evidence = None;
        assert_eq!(native_decline_evidence_label(&w), "-");
    }

    /// The crash-summary error-message truncation at
    /// `render_geomean_headline` must be char-boundary safe: byte-slicing at
    /// a fixed offset panics if the cut falls inside a multibyte UTF-8
    /// sequence. Use a string whose 77th *byte* lands inside a multibyte
    /// character to prove the fix.
    #[test]
    fn test_markdown_crash_summary_truncates_multibyte_error_without_panicking() {
        // 76 ASCII chars, then a 3-byte UTF-8 character (e), so byte offset
        // 77 falls inside the multibyte sequence — the old `&s[..77]`
        // byte-slice would panic here.
        let prefix: String = "a".repeat(76);
        let error = format!("{prefix}\u{2705} trailing detail that pushes this past 80 chars");
        assert!(
            error.len() > 80,
            "fixture must exceed the truncation threshold"
        );

        let mut workload = mock_workload_result("wl_crash", 1000, 5.0, 15.0);
        workload.name = "wl_ok".to_owned();
        let mut report = mock_report(vec![workload]);
        report.crashes.push(CrashedScale {
            workload: "wl_crash".to_owned(),
            rows: 1000,
            error,
            repro_command: None,
            plan_snippet_artifact: None,
            correctness_diff_artifact: None,
            log_tail_artifacts: Vec::new(),
        });

        // Must not panic.
        let md = report.to_markdown();
        assert!(md.contains("### Crashed scales"));
        assert!(md.contains("CRASH:"));
    }

    /// Plan-snippet display truncation is renderer-only: a full plan beyond
    /// `PLAN_SNIPPET_DISPLAY_MAX_LINES` is truncated with a trailing marker,
    /// but classification helpers (exercised via `native_decline_reason_verified`
    /// and `dispatch_classification`, tested elsewhere) always see the FULL
    /// text — only `truncate_plan_for_display` itself shortens anything.
    #[test]
    fn test_truncate_plan_for_display_caps_long_plans_and_preserves_short_ones() {
        let short_plan = "Seq Scan on bench\n  Filter: x > 1\n";
        assert_eq!(truncate_plan_for_display(short_plan), short_plan);

        let long_plan = (0..50)
            .map(|i| format!("  -> Node {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let truncated = truncate_plan_for_display(&long_plan);
        assert!(truncated.lines().count() > PLAN_SNIPPET_DISPLAY_MAX_LINES);
        assert!(truncated.contains("(20 more lines)"));
        assert!(truncated.contains("Node 0"));
        assert!(!truncated.contains("Node 49"));
    }

    /// Detailed Results must render warm/cold summaries separately (never a
    /// pooled median), the resident-cache preload note, the cold-cache
    /// `shared_buffers`-resident warning, and native-decline evidence with
    /// its source label.
    #[test]
    fn test_markdown_detailed_results_renders_cache_split_and_resident_evidence() {
        let mut workload =
            mock_workload_result("resident_star_groupagg_case", 1_000_000, 10.0, 20.0);
        workload.warm_summary = Some(mock_cache_mode_summary(CacheState::Warm, 4.0));
        workload.cold_summary = Some(mock_cache_mode_summary(CacheState::Cold, 1.2));
        workload.resident_lane = true;
        workload.resident_load_ms = Some(42.5);
        workload.cold_shared_buffers_resident = Some(true);
        workload.native_decline_evidence = Some(NativeDeclineEvidence {
            reason: "some_reason".to_owned(),
            source: DeclineReasonSource::PlannerReported,
        });
        workload.plan_snippet = Some("Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n".to_owned());

        let report = mock_report(vec![workload]);
        let md = report.to_markdown();

        assert!(
            md.contains("Warm (n=5)"),
            "must render warm summary; md:\n{md}"
        );
        assert!(
            md.contains("Cold (n=5)"),
            "must render cold summary; md:\n{md}"
        );
        assert!(
            md.contains("42.50ms"),
            "resident_load_ms must appear in Detailed Results; md:\n{md}"
        );
        assert!(
            md.contains("shared_buffers"),
            "cold_shared_buffers_resident must render a warning; md:\n{md}"
        );
        assert!(
            md.contains("`some_reason` (planner-reported, verified)"),
            "native-decline evidence must render with its source label; md:\n{md}"
        );
    }

    #[test]
    fn test_report_renders_cache_purge_audit_rows() {
        let mut workload = mock_workload_result("gpu_reduce_sum", 10_000, 10.0, 20.0);
        workload.iterations[0].cache_purge = CachePurgeState::Unavailable;
        workload.iterations[3].cache_purge = CachePurgeState::Failed;
        let report = with_cache_mode(mock_report(vec![workload]), "cold");

        let markdown = report.to_markdown();

        assert!(markdown.contains("## Cache Purge Audit"));
        assert!(markdown.contains("| gpu_reduce_sum | 10K | 1 | 4 |"));
    }

    #[test]
    fn test_warmup_iterations_populate_summary_fields() {
        let mut workload = mock_workload_result("h3_bulk", 100_000, 10.0, 20.0);
        workload.set_warmup_iterations(vec![
            IterationResult {
                accel_ms: 250.0,
                parallel_ms: 40.0,
                cache_purge: CachePurgeState::NotRequested,
                cache_state: CacheState::Warm,
            },
            IterationResult {
                accel_ms: 30.0,
                parallel_ms: 35.0,
                cache_purge: CachePurgeState::NotRequested,
                cache_state: CacheState::Warm,
            },
            IterationResult {
                accel_ms: 80.0,
                parallel_ms: 32.0,
                cache_purge: CachePurgeState::NotRequested,
                cache_state: CacheState::Warm,
            },
        ]);

        assert_eq!(workload.warmup_iterations.len(), 3);
        assert_eq!(workload.warmup_accel_first_ms, Some(250.0));
        assert_eq!(workload.warmup_accel_max_ms, Some(250.0));
        assert_eq!(workload.warmup_accel_post_first_max_ms, Some(80.0));
        assert_eq!(workload.warmup_parallel_first_ms, Some(40.0));
        assert_eq!(workload.warmup_parallel_max_ms, Some(40.0));
    }

    #[test]
    fn test_report_renders_warmup_jit_audit_for_post_first_spike() {
        let mut workload = mock_workload_result("h3_bulk", 100_000, 10.0, 20.0);
        workload.set_warmup_iterations(vec![
            IterationResult {
                accel_ms: 50.0,
                parallel_ms: 40.0,
                cache_purge: CachePurgeState::NotRequested,
                cache_state: CacheState::Warm,
            },
            IterationResult {
                accel_ms: 1_500.0,
                parallel_ms: 42.0,
                cache_purge: CachePurgeState::NotRequested,
                cache_state: CacheState::Warm,
            },
        ]);
        let report = mock_report(vec![workload]);

        let markdown = report.to_markdown();

        assert!(markdown.contains("## Warmup/JIT Audit"));
        assert!(markdown.contains("| h3_bulk | 100K | 2 | 50.00ms | 1500.00ms | 1500.00ms |"));
        assert!(markdown.contains("post-first warmup max 1500.00ms"));
    }

    #[test]
    fn test_report_renders_harness_profile() {
        let report = mock_report(vec![mock_workload_result("h3_bulk", 100_000, 10.0, 20.0)]);

        let json = report.to_json().expect("json");
        let markdown = report.to_markdown();
        let value: serde_json::Value = serde_json::from_str(&json).expect("json value");

        assert_eq!(value["methodology"]["harness_profile"], "test");
        assert!(markdown.contains("| Harness build profile | `test` |"));
    }

    #[test]
    fn test_markdown_contains_header() {
        let report = mock_report(vec![mock_workload_result("wl1", 1000, 5.0, 15.0)]);
        let md = report.to_markdown();
        assert!(md.contains("# pg_accel Benchmark Report"));
    }

    #[test]
    fn test_markdown_renders_sanity_checks() {
        let mut result = mock_workload_result("ssbm_q2_3", 100_000, 10.0, 20.0);
        result.category = "ssbm".to_owned();
        result.sanity_checks = vec![SanityCheck {
            label: "ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3)".to_owned(),
            count: 25,
            passed: true,
        }];
        let report = mock_report(vec![result]);
        let md = report.to_markdown();
        assert!(md.contains("## Benchmark Sanity Checks"));
        assert!(md.contains("ssbm_part.p_brand1 = MFGR#2239"));
        assert!(md.contains("| ssbm_q2_3 | 100K |"));
    }

    #[test]
    fn test_markdown_renders_planner_threshold_matrix() {
        // No real planner rejection reason was captured for this row — the
        // static matrix merely *expects* a decline. That must render as
        // "expected, unconfirmed" and FAIL the row's status, never be
        // laundered into a passing "verified" decline.
        let mut workload = mark_no_dispatch(
            mock_workload_result("gpu_sort_multikey", 100_000, 10.0, 10.0),
            "Sort\n  Sort Key: key1, key2\n  -> Seq Scan on bench_sort_multi",
            "Sort\n  Sort Key: key1, key2\n  -> Seq Scan on bench_sort_multi",
        );
        workload.native_decline_evidence = Some(NativeDeclineEvidence {
            reason: "sort_multikey_no_gpu_kernel".to_owned(),
            source: DeclineReasonSource::ExpectedUnconfirmed,
        });
        let report = mock_report(vec![workload]);
        let md = report.to_markdown();

        assert!(md.contains("## Planner Threshold Matrix"));
        assert!(md.contains("Prepared Geometry"));
        assert!(md.contains("Index/Pruning"));
        assert!(md.contains("sort_multikey_no_gpu_kernel"));
        assert!(md.contains("| standalone_heap_sort | gpu_sort_multikey | 100K |"));
        assert!(
            md.contains("expected, unconfirmed"),
            "unconfirmed decline evidence must render honestly; md:\n{md}"
        );
        assert!(
            md.contains("| FAIL |"),
            "unconfirmed decline evidence must never pass the threshold-matrix status; md:\n{md}"
        );
    }

    #[test]
    fn test_markdown_contains_row_scales() {
        let report = mock_report(vec![mock_workload_result("wl1", 1000, 5.0, 15.0)]);
        let md = report.to_markdown();
        assert!(md.contains("Row scales"));
        assert!(md.contains("1K"));
        assert!(md.contains("1M"));
    }

    #[test]
    fn test_markdown_multi_scale_table() {
        let report = mock_report(vec![
            mock_workload_result("wl1", 1_000, 5.0, 15.0),
            mock_workload_result("wl1", 1_000_000, 5.0, 15.0),
        ]);
        let md = report.to_markdown();
        // Summary table should have scale columns
        assert!(md.contains("| 1K |"));
        assert!(md.contains("| 1M |"));
    }

    #[test]
    fn test_markdown_warns_on_no_dispatch_timing_skew() {
        let plan = "Aggregate\n  Output: count(*)\n  -> Seq Scan on public.bench\n";
        let workload = mark_no_dispatch(
            mock_workload_result("native_skew", 100_000, 20.0, 10.0),
            plan,
            plan,
        );
        let report = mock_report(vec![workload]);
        let md = report.to_markdown();

        assert!(md.contains("## No-Dispatch Timing Audit"));
        assert!(md.contains("native_skew"));
        assert!(md.contains("same native plan timing skew; rerun/inspect harness"));
    }

    #[test]
    fn test_markdown_warns_on_no_dispatch_plan_mismatch() {
        let accel_plan = "Aggregate\n  Output: count(*)\n  -> Seq Scan on public.bench\n";
        let baseline_plan = "Finalize Aggregate\n  -> Gather\n        Workers Planned: 2\n        -> Partial Aggregate\n             -> Parallel Seq Scan on public.bench\n";
        let workload = mark_no_dispatch(
            mock_workload_result("native_plan_diff", 100_000, 10.0, 10.0),
            accel_plan,
            baseline_plan,
        );
        let report = mock_report(vec![workload]);
        let md = report.to_markdown();

        assert!(md.contains("native_plan_diff"));
        assert!(md.contains("| DIFF |"));
    }

    #[test]
    fn test_markdown_omits_no_dispatch_audit_for_comparable_native_plan() {
        let plan = "Aggregate\n  Output: count(*)\n  -> Seq Scan on public.bench\n";
        let workload = mark_no_dispatch(
            mock_workload_result("native_same", 100_000, 10.0, 10.0),
            plan,
            plan,
        );
        let report = mock_report(vec![workload]);
        let md = report.to_markdown();

        assert!(!md.contains("## No-Dispatch Timing Audit"));
    }

    #[test]
    fn test_no_dispatch_plan_shape_ignores_pg_accel_decline_diagnostics() {
        // A real `pg_accel planner rejection reason:` line (sourced from
        // `pg_accel_last_planner_rejection_reason()`) is still stripped from
        // the plan-shape signature so it does not itself cause a plan
        // mismatch against the baseline plan, which never carries it.
        let baseline_plan = "Aggregate\n  Output: count(*)\n  -> Seq Scan on public.bench\n";
        let accel_plan = "Aggregate\n  Output: count(*)\n  -> Seq Scan on public.bench\n\
             pg_accel planner rejection reason: nestloop_scalar_no_gpu_kernel";
        let mut workload = mark_no_dispatch(
            mock_workload_result("native_decline_with_evidence", 50_000, 10.0, 10.0),
            accel_plan,
            baseline_plan,
        );
        workload.native_decline_evidence = Some(NativeDeclineEvidence {
            reason: "nestloop_scalar_no_gpu_kernel".to_owned(),
            source: DeclineReasonSource::PlannerReported,
        });
        let audit = mock_report(vec![workload]).no_dispatch_audit();

        assert_eq!(audit.evaluated_no_dispatch_rows, 1);
        assert_eq!(audit.warning_rows, 0);
        assert_eq!(audit.plan_mismatch_rows, 0);
        assert_eq!(
            audit.rows[0].status,
            NoDispatchAuditStatus::ComparableNative
        );
    }

    #[test]
    fn test_no_dispatch_audit_classifies_native_evidence_quality() {
        let native_plan = "Aggregate\n  Output: count(*)\n  -> Seq Scan on public.bench\n";
        let different_plan = "Finalize Aggregate\n  -> Gather\n        Workers Planned: 2\n        -> Partial Aggregate\n             -> Parallel Seq Scan on public.bench\n";
        let clean = mark_no_dispatch(
            mock_workload_result("native_clean", 100_000, 10.0, 10.0),
            native_plan,
            native_plan,
        );
        let skew = mark_no_dispatch(
            mock_workload_result("native_skew", 100_000, 20.0, 10.0),
            native_plan,
            native_plan,
        );
        let mismatch = mark_no_dispatch(
            mock_workload_result("native_mismatch", 100_000, 10.0, 10.0),
            native_plan,
            different_plan,
        );
        let mut missing_plan = mock_workload_result("native_missing_plan", 100_000, 10.0, 10.0);
        missing_plan.plan_selected = false;
        missing_plan.gpu_kernel_dispatched = false;
        let mut missing_plan_skew =
            mock_workload_result("native_missing_plan_skew", 100_000, 20.0, 10.0);
        missing_plan_skew.plan_selected = false;
        missing_plan_skew.gpu_kernel_dispatched = false;
        let selected_not_dispatched = mark_no_dispatch(
            mock_workload_result("custom_scan_cpu_fallback", 100_000, 10.0, 10.0),
            "Custom Scan (GpuAccelScan)\n  GPU Dispatched: false\n",
            native_plan,
        );
        let mut dispatching = mock_workload_result("gpu_dispatch", 100_000, 10.0, 50.0);
        dispatching.dispatch_counter_captured = true;
        dispatching.gpu_kernel_execution_delta = 1;

        let audit = mock_report(vec![
            clean,
            skew,
            mismatch,
            missing_plan,
            missing_plan_skew,
            selected_not_dispatched,
            dispatching,
        ])
        .no_dispatch_audit();

        assert_eq!(audit.schema_version, NO_DISPATCH_AUDIT_SCHEMA_VERSION);
        assert_eq!(audit.evaluated_no_dispatch_rows, 6);
        assert_eq!(audit.clean_rows, 1);
        assert_eq!(audit.warning_rows, 5);
        assert_eq!(audit.timing_skew_rows, 2);
        assert_eq!(audit.plan_mismatch_rows, 2);
        assert_eq!(audit.selected_custom_scan_not_dispatched_rows, 1);
        assert_eq!(audit.missing_plan_evidence_rows, 2);
        assert_eq!(audit.ignored_dispatching_rows, 1);
        assert!(audit.warning_rows > 0);
        assert_eq!(
            audit
                .rows
                .iter()
                .find(|row| row.workload == "native_clean")
                .expect("clean row should be present")
                .status,
            NoDispatchAuditStatus::ComparableNative
        );
        assert_eq!(
            audit
                .rows
                .iter()
                .find(|row| row.workload == "native_skew")
                .expect("skew row should be present")
                .status,
            NoDispatchAuditStatus::TimingSkew
        );
        assert_eq!(
            audit
                .rows
                .iter()
                .find(|row| row.workload == "native_mismatch")
                .expect("mismatch row should be present")
                .status,
            NoDispatchAuditStatus::PlanMismatch
        );
        assert_eq!(
            audit
                .rows
                .iter()
                .find(|row| row.workload == "native_missing_plan")
                .expect("missing-plan row should be present")
                .status,
            NoDispatchAuditStatus::MissingPlanEvidence
        );
        let missing_plan_skew = audit
            .rows
            .iter()
            .find(|row| row.workload == "native_missing_plan_skew")
            .expect("missing-plan timing-skew row should be present");
        assert_eq!(missing_plan_skew.status, NoDispatchAuditStatus::TimingSkew);
        assert!(missing_plan_skew.missing_plan_evidence);
        assert_eq!(
            audit
                .rows
                .iter()
                .find(|row| row.workload == "custom_scan_cpu_fallback")
                .expect("selected no-dispatch row should be present")
                .status,
            NoDispatchAuditStatus::SelectedCustomScanNotDispatched
        );

        let md = audit.to_markdown();
        assert!(md.contains("No-Dispatch Audit"));
        assert!(md.contains("custom_scan_cpu_fallback"));
        assert!(md.contains("selected_custom_scan_not_dispatched"));
        assert!(md.contains("no GPU credit; use only as native-decline/stability evidence"));
    }

    #[test]
    fn test_markdown_excludes_planner_declined_rows_from_regressions() {
        let plan = "Limit\n  -> Sort\n       Sort Key: bench.val\n       -> Seq Scan on bench\n";
        let workload = mark_no_dispatch(
            mock_workload_result("topk_wide", 1_000_000, 30.0, 15.0),
            plan,
            plan,
        );
        let report = mock_report(vec![workload]);
        let md = report.to_markdown();

        assert!(md.contains("## No-Dispatch Timing Audit"));
        assert!(md.contains("topk_wide"));
        assert!(md.contains("| 1M |"));
        assert!(md.contains("| planner_declined |"));
        assert!(md.contains("same native plan timing skew; rerun/inspect harness"));
        assert!(!md.contains("## Regressions"));
    }

    #[test]
    fn test_h3_resident_groupagg_counts_as_custom_scan_gpu_dispatched() {
        let mut workload = mock_workload_result("h3_bulk", 100_000, 10.0, 50.0);
        workload.category = "gpu_h3".to_owned();
        workload.kernel_class = "h3_latlng".to_owned();
        workload.plan_selected = false;
        workload.gpu_kernel_dispatched = false;
        workload.function_srf_kernel_dispatched = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: true\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_groupagg\n  \
             GPU Resident GroupAgg Key: h3index\n  \
             GPU Resident GroupAgg Measure: count_star\n  \
             GPU Resident GroupAgg Filter: none\n  \
             GPU Resident GroupAgg Predicate Guard: none\n  \
             GPU Resident GroupAgg Value Predicate: none\n  \
             GPU Resident GroupAgg Predicate IR: guard=none;value=none\n  \
             GPU Resident GroupAgg Aggregate Mask: 8\n  \
             GPU Resident Stage Mask: 17417\n  \
             GPU Resident Device Columns: 4"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        let md = report.to_markdown();
        assert!(md.contains("geomean across 1 GPU-dispatched workloads"));
        assert!(md.contains("| pg_accel Custom Scan plan selected | 1 |"));
        assert!(md.contains("| Function/SRF kernel dispatched | 0 |"));
        assert!(!md.contains("## Non-Dispatching Workloads"));

        let csv = report.to_csv();
        assert!(csv.contains(
            "h3_bulk,gpu_h3,h3_latlng,true,true,false,false,false,0,10,\
             reported,true,1,0,0,0,0,10,100000"
        ));
    }

    #[test]
    fn test_h3_function_kernel_requires_output_consumption() {
        let mut workload = mock_workload_result("h3_bulk", 100_000, 10.0, 50.0);
        workload.category = "gpu_h3".to_owned();
        workload.kernel_class = "h3_latlng".to_owned();
        workload.plan_selected = false;
        workload.gpu_kernel_dispatched = false;
        workload.function_srf_kernel_dispatched = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 0;
        let report = mock_report(vec![workload]);

        let md = report.to_markdown();
        assert!(md.contains("geomean across 0 GPU-dispatched workloads"));
        assert!(md.contains("| Function/SRF kernel dispatched | 0 |"));
    }

    #[test]
    fn test_benchmark_ship_gate_flags_crashes() {
        let mut report = mock_report(Vec::new());
        report.crashes.push(CrashedScale {
            workload: "gpu_hashjoin_large_build".to_owned(),
            rows: 100_000,
            error: "backend disconnected".to_owned(),
            repro_command: None,
            plan_snippet_artifact: None,
            correctness_diff_artifact: None,
            log_tail_artifacts: Vec::new(),
        });

        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].kind, BenchmarkShipGateFailureKind::Crash);
    }

    #[test]
    fn test_crashed_scales_render_correctness_diff_artifact() {
        let mut report = mock_report(Vec::new());
        report.crashes.push(CrashedScale {
            workload: "bad_correctness".to_owned(),
            rows: 100_000,
            error: "correctness diff failed".to_owned(),
            repro_command: None,
            plan_snippet_artifact: None,
            correctness_diff_artifact: Some(
                "correctness_diffs/bad-correctness-100000.json".to_owned(),
            ),
            log_tail_artifacts: Vec::new(),
        });

        let md = report.to_markdown();
        assert!(md.contains("Correctness Diff"));
        assert!(md.contains("correctness_diffs/bad-correctness-100000.json"));
    }

    #[test]
    fn test_benchmark_ship_gate_flags_selected_plan_without_dispatch() {
        let mut workload = mock_workload_result("gpu_reduce_sum", 100_000, 10.0, 20.0);
        workload.plan_selected = true;
        workload.gpu_kernel_dispatched = false;
        workload.dispatch_counter_captured = true;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuReduce\n  GPU Dispatched: false".to_owned(),
        );
        let report = mock_report(vec![workload]);

        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::SelectedPlanMissedDispatch
        );
        assert!(
            report
                .to_markdown()
                .contains("### Benchmark Ship Gate Failures")
        );
    }

    #[test]
    fn test_benchmark_ship_gate_flags_selected_plan_without_resident_proof() {
        let mut workload = mock_workload_result("generic_gpu_dispatch", 100_000, 10.0, 20.0);
        workload.plan_selected = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet =
            Some("Custom Scan (GpuAccelMystery)\n  GPU Dispatched: true\n".to_owned());
        let report = mock_report(vec![workload]);

        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::SelectedPlanNotGpuResident
        );
        assert!(
            report
                .to_markdown()
                .contains("selected_plan_not_gpu_resident")
        );
    }

    #[test]
    fn test_benchmark_ship_gate_flags_selected_plan_with_inferred_boundary() {
        let mut workload = mock_workload_result("generic_gpu_dispatch", 100_000, 10.0, 20.0);
        workload.plan_selected = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: true\n".to_owned(),
        );
        let report = mock_report(vec![workload]);

        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::SelectedPlanNotGpuResident
        );
    }

    #[test]
    fn test_benchmark_ship_gate_allows_selected_plan_with_reported_resident_pipeline() {
        let mut workload = mock_workload_result("generic_gpu_dispatch", 100_000, 10.0, 20.0);
        workload.plan_selected = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelScan)\n  GPU Dispatched: true\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_expression\n  \
             GPU Resident Stage Mask: 3\n  \
             GPU Resident Device Columns: 2\n"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        assert!(report.evaluate_benchmark_ship_gate().is_empty());
    }

    #[test]
    fn test_benchmark_ship_gate_flags_dispatched_rows_below_parity() {
        let mut workload = mock_workload_result("generic_gpu_dispatch", 100_000, 20.0, 10.0);
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: true\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_groupagg\n  \
             GPU Resident Stage Mask: 5\n  \
             GPU Resident Device Columns: 1"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::DispatchedBelowParity
        );
        assert!(failures[0].speedup_median < BENCHMARK_SHIP_GATE_MIN_SPEEDUP);
    }

    #[test]
    fn test_benchmark_ship_gate_flags_expected_winner_missing_evidence() {
        let mut workload = mock_workload_result("grouped_agg", 1_000_000, 10.0, 20.0);
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 0;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: true\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_groupagg\n  \
             GPU Resident Stage Mask: 5\n  \
             GPU Resident Device Columns: 2"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::ExpectedWinnerMissingEvidence
        );
        assert!(failures[0].detail.contains("dispatch-counter"));
    }

    #[test]
    fn test_benchmark_ship_gate_allows_groupagg_winner_with_logical_spec() {
        let mut workload = mock_workload_result("grouped_agg", 1_000_000, 10.0, 20.0);
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: true\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_groupagg\n  \
             GPU Resident GroupAgg Key: resident_i32\n  \
             GPU Resident GroupAgg Measure: direct_column\n  \
             GPU Resident GroupAgg Filter: none\n  \
             GPU Resident GroupAgg Predicate Guard: none\n  \
             GPU Resident GroupAgg Value Predicate: none\n  \
             GPU Resident GroupAgg Predicate IR: guard=none;value=none\n  \
             GPU Resident GroupAgg Aggregate Mask: 3\n  \
             GPU Resident Stage Mask: 5\n  \
             GPU Resident Device Columns: 2"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        assert!(report.evaluate_benchmark_ship_gate().is_empty());
    }

    #[test]
    fn test_benchmark_ship_gate_requires_groupagg_logical_spec_for_winners() {
        let mut workload = mock_workload_result("grouped_agg", 1_000_000, 10.0, 20.0);
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: true\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_groupagg\n  \
             GPU Resident Stage Mask: 5\n  \
             GPU Resident Device Columns: 2"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::ExpectedWinnerMissingGroupAggLogicalSpec
        );
        assert!(failures[0].detail.contains("ResidentGroupAgg logical spec"));
    }

    #[test]
    fn test_benchmark_ship_gate_requires_groupagg_predicate_ir_for_winners() {
        let mut workload = mock_workload_result("grouped_agg", 1_000_000, 10.0, 20.0);
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: true\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_groupagg\n  \
             GPU Resident GroupAgg Key: resident_i32\n  \
             GPU Resident GroupAgg Measure: direct_column\n  \
             GPU Resident GroupAgg Filter: none\n  \
             GPU Resident GroupAgg Predicate Guard: none\n  \
             GPU Resident GroupAgg Value Predicate: none\n  \
             GPU Resident GroupAgg Aggregate Mask: 3\n  \
             GPU Resident Stage Mask: 5\n  \
             GPU Resident Device Columns: 2"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::ExpectedWinnerMissingGroupAggLogicalSpec
        );
        assert!(failures[0].detail.contains("ResidentGroupAgg logical spec"));
    }

    #[test]
    fn test_benchmark_ship_gate_requires_hashjoin_filter_groupagg_logical_spec() {
        let mut workload = mock_workload_result("gpu_hashjoin_filter", 1_000_000, 10.0, 20.0);
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: true\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_groupagg\n  \
             GPU Resident Stage Mask: 7\n  \
             GPU Resident Device Columns: 4"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::ExpectedWinnerMissingGroupAggLogicalSpec
        );
        assert!(failures[0].detail.contains("hashjoin_filter_groupagg"));
    }

    #[test]
    fn test_benchmark_ship_gate_requires_ssbm_groupagg_logical_spec_for_winners() {
        let mut workload = mock_workload_result("ssbm_q4_3", 1_000_000, 10.0, 20.0);
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: true\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_groupagg\n  \
             GPU Resident Stage Mask: 13\n  \
             GPU Resident Device Columns: 5"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::ExpectedWinnerMissingGroupAggLogicalSpec
        );
        assert!(failures[0].detail.contains("ssbm_q4_grouped_profit"));
    }

    #[test]
    fn test_benchmark_ship_gate_allows_ssbm_q1_winner_with_groupagg_logical_spec() {
        let mut workload = mock_workload_result("ssbm_q1_1", 1_000_000, 10.0, 20.0);
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 1;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: true\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_groupagg\n  \
             GPU Resident GroupAgg Key: single_group\n  \
             GPU Resident GroupAgg Measure: ssbm_discounted_revenue\n  \
             GPU Resident GroupAgg Filter: ssbm_date_fact_predicate\n  \
             GPU Resident GroupAgg Predicate Guard: ssbm_date_fact_predicate\n  \
             GPU Resident GroupAgg Value Predicate: none\n  \
             GPU Resident GroupAgg Predicate IR: guard=ssbm_date_fact_predicate;value=none\n  \
             GPU Resident GroupAgg Aggregate Mask: 1\n  \
             GPU Resident Stage Mask: 13\n  \
             GPU Resident Device Columns: 5"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        assert!(report.evaluate_benchmark_ship_gate().is_empty());
    }

    #[test]
    fn test_benchmark_ship_gate_allows_ssbm_q4_winner_with_groupagg_logical_spec() {
        let mut workload = mock_workload_result("ssbm_q4_3", 1_000_000, 10.0, 20.0);
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: true\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_groupagg\n  \
             GPU Resident GroupAgg Key: ssbm_year_geo_part\n  \
             GPU Resident GroupAgg Measure: ssbm_profit_revenue_minus_supplycost\n  \
             GPU Resident GroupAgg Filter: ssbm_star_join_membership\n  \
             GPU Resident GroupAgg Predicate Guard: ssbm_star_join_membership\n  \
             GPU Resident GroupAgg Value Predicate: none\n  \
             GPU Resident GroupAgg Predicate IR: guard=ssbm_star_join_membership;value=none\n  \
             GPU Resident GroupAgg Aggregate Mask: 1\n  \
             GPU Resident Stage Mask: 29\n  \
             GPU Resident Device Columns: 10"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        assert!(report.evaluate_benchmark_ship_gate().is_empty());
    }

    #[test]
    fn test_benchmark_ship_gate_flags_expected_winner_below_threshold() {
        let mut workload = mock_workload_result("h3_bulk", 1_000_000, 10.0, 12.0);
        workload.category = "gpu_h3".to_owned();
        workload.kernel_class = "h3_latlng".to_owned();
        workload.plan_selected = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: true\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_groupagg\n  \
             GPU Resident GroupAgg Key: h3index\n  \
             GPU Resident GroupAgg Measure: count_star\n  \
             GPU Resident GroupAgg Filter: none\n  \
             GPU Resident GroupAgg Predicate Guard: none\n  \
             GPU Resident GroupAgg Value Predicate: none\n  \
             GPU Resident GroupAgg Predicate IR: guard=none;value=none\n  \
             GPU Resident GroupAgg Aggregate Mask: 8\n  \
             GPU Resident Stage Mask: 17417\n  \
             GPU Resident Device Columns: 4"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::ExpectedWinnerBelowThreshold
        );
        assert!((failures[0].gate_floor - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_benchmark_ship_gate_requires_h3_raster_cache_mode_both_for_winners() {
        let mut workload = mock_workload_result("h3_bulk", 1_000_000, 10.0, 50.0);
        workload.category = "gpu_h3".to_owned();
        workload.kernel_class = "h3_latlng".to_owned();
        workload.plan_selected = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: true\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_groupagg\n  \
             GPU Resident GroupAgg Key: h3index\n  \
             GPU Resident GroupAgg Measure: count_star\n  \
             GPU Resident GroupAgg Filter: none\n  \
             GPU Resident GroupAgg Predicate Guard: none\n  \
             GPU Resident GroupAgg Value Predicate: none\n  \
             GPU Resident GroupAgg Predicate IR: guard=none;value=none\n  \
             GPU Resident GroupAgg Aggregate Mask: 8\n  \
             GPU Resident Stage Mask: 17417\n  \
             GPU Resident Device Columns: 4"
                .to_owned(),
        );

        let warm_report = mock_report(vec![workload.clone()]);
        let failures = warm_report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::ExpectedWinnerMissingCacheEvidence
        );
        assert!(failures[0].detail.contains("cache-mode both"));

        let cache_both_report = with_cache_mode(mock_report(vec![workload]), "both");
        assert!(cache_both_report.evaluate_benchmark_ship_gate().is_empty());
    }

    #[test]
    fn test_benchmark_ship_gate_flags_expected_winner_missed_selection() {
        let workload = mark_no_dispatch(
            mock_workload_result("grouped_agg", 1_000_000, 10.0, 20.0),
            "HashAggregate\n  -> Seq Scan on bench_grouped_agg",
            "HashAggregate\n  -> Seq Scan on bench_grouped_agg",
        );
        let report = mock_report(vec![workload]);

        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::ExpectedWinnerMissedSelection
        );
    }

    #[test]
    fn test_benchmark_ship_gate_flags_native_decline_unexpected_dispatch() {
        let mut workload = mock_workload_result("gpu_sort_multikey", 100_000, 10.0, 20.0);
        workload.plan_selected = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelSort)\n  Strategy: GpuSort\n  GPU Dispatched: true".to_owned(),
        );
        let report = mock_report(vec![workload]);

        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::NativeDeclineUnexpectedDispatch
        );
    }

    #[test]
    fn test_benchmark_ship_gate_allows_planner_declined_native_rows() {
        // Honest evidence: the planner itself reported this decline reason
        // (`DeclineReasonSource::PlannerReported`), so the ship gate passes.
        let mut workload = mark_no_dispatch(
            mock_workload_result("mergejoin_decline", 100_000, 10.0, 10.0),
            "Aggregate\n  ->  Merge Join",
            "Aggregate\n  ->  Merge Join",
        );
        workload.native_decline_evidence = Some(NativeDeclineEvidence {
            reason: "mergejoin_no_gpu_kernel".to_owned(),
            source: DeclineReasonSource::PlannerReported,
        });
        let report = mock_report(vec![workload]);

        assert!(report.evaluate_benchmark_ship_gate().is_empty());
        assert!(
            !report
                .to_markdown()
                .contains("### Benchmark Ship Gate Failures")
        );
    }

    #[test]
    fn test_benchmark_ship_gate_requires_spatial_decline_reason() {
        let missing_reason = mark_no_dispatch(
            mock_workload_result("spatial_sel_10pct", 1_000_000, 10.0, 10.0),
            "Finalize Aggregate\n  ->  Gather\n        ->  Parallel Seq Scan",
            "Finalize Aggregate\n  ->  Gather\n        ->  Parallel Seq Scan",
        );
        let report = mock_report(vec![missing_reason]);
        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::NativeDeclineReasonMissing
        );

        // The runner tags a real `pg_accel planner rejection reason:` line
        // (captured from `pg_accel_last_planner_rejection_reason()`) into
        // `native_decline_evidence` as `PlannerReported` — that tagged field,
        // not the raw plan text, is what the ship gate reads.
        let mut with_reason = mark_no_dispatch(
            mock_workload_result("spatial_sel_10pct", 1_000_000, 10.0, 10.0),
            "Finalize Aggregate\n  ->  Gather\n        ->  Parallel Seq Scan\n\
             pg_accel planner rejection reason: spatial_no_registered_gpu_predicate",
            "Finalize Aggregate\n  ->  Gather\n        ->  Parallel Seq Scan",
        );
        with_reason.native_decline_evidence = Some(NativeDeclineEvidence {
            reason: "spatial_no_registered_gpu_predicate".to_owned(),
            source: DeclineReasonSource::PlannerReported,
        });
        let report = mock_report(vec![with_reason]);
        assert!(report.evaluate_benchmark_ship_gate().is_empty());
    }

    #[test]
    fn test_benchmark_ship_gate_requires_threshold_decline_reason_evidence() {
        let missing_generic_reason = mark_no_dispatch(
            mock_workload_result("gpu_sort_multikey", 100_000, 10.0, 10.0),
            "Sort\n  Sort Key: key1, key2\n  -> Seq Scan on bench_sort_multi",
            "Sort\n  Sort Key: key1, key2\n  -> Seq Scan on bench_sort_multi",
        );
        let report = mock_report(vec![missing_generic_reason]);
        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::NativeDeclineReasonMissing
        );

        // Planner-reported evidence with the matching reason passes.
        let mut with_generic_reason = mark_no_dispatch(
            mock_workload_result("gpu_sort_multikey", 100_000, 10.0, 10.0),
            "Sort\n  Sort Key: key1, key2\n  -> Seq Scan on bench_sort_multi",
            "Sort\n  Sort Key: key1, key2\n  -> Seq Scan on bench_sort_multi",
        );
        with_generic_reason.native_decline_evidence = Some(NativeDeclineEvidence {
            reason: "sort_multikey_no_gpu_kernel".to_owned(),
            source: DeclineReasonSource::PlannerReported,
        });
        let report = mock_report(vec![with_generic_reason]);
        assert!(report.evaluate_benchmark_ship_gate().is_empty());

        // An *unconfirmed* expectation — the static matrix expected this
        // decline but the planner never reported a reason — must still fail
        // the gate. Only a confirmed planner-reported reason may pass.
        let mut with_unconfirmed_reason = mark_no_dispatch(
            mock_workload_result("gpu_sort_multikey", 100_000, 10.0, 10.0),
            "Sort\n  Sort Key: key1, key2\n  -> Seq Scan on bench_sort_multi",
            "Sort\n  Sort Key: key1, key2\n  -> Seq Scan on bench_sort_multi",
        );
        with_unconfirmed_reason.native_decline_evidence = Some(NativeDeclineEvidence {
            reason: "sort_multikey_no_gpu_kernel".to_owned(),
            source: DeclineReasonSource::ExpectedUnconfirmed,
        });
        let report = mock_report(vec![with_unconfirmed_reason]);
        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(
            failures.len(),
            1,
            "unconfirmed expected-decline evidence must not satisfy the ship gate"
        );
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::NativeDeclineReasonMissing
        );

        let missing_reason = mark_no_dispatch(
            mock_workload_result("h3_bulk", 10_000, 10.0, 10.0),
            "HashAggregate\n  -> Seq Scan on bench_h3_points",
            "HashAggregate\n  -> Seq Scan on bench_h3_points",
        );
        let report = mock_report(vec![missing_reason]);
        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::NativeDeclineReasonMissing
        );

        let mut with_reason = mark_no_dispatch(
            mock_workload_result("h3_bulk", 10_000, 10.0, 10.0),
            "HashAggregate\n  -> Seq Scan on bench_h3_points",
            "HashAggregate\n  -> Seq Scan on bench_h3_points",
        );
        with_reason.native_decline_evidence = Some(NativeDeclineEvidence {
            reason: "h3_rows_below_grouped_agg_min".to_owned(),
            source: DeclineReasonSource::PlannerReported,
        });
        let report = mock_report(vec![with_reason]);
        assert!(report.evaluate_benchmark_ship_gate().is_empty());
    }

    // -----------------------------------------------------------------------
    // Phase 5 H3 winning-lane protection — dispatch classification gates
    //
    // These tests pin the report classifier behavior for the H3 winning lanes
    // documented in TODO.md Phase 5. They are unit tests against
    // `dispatch_classification`, not live-PG runs — so they fail loudly if
    // anyone weakens the criterion for crediting an H3 winner OR weakens
    // the rejection of a parity-only row that lacks dispatch evidence.
    // -----------------------------------------------------------------------

    /// Build the "happy path" H3 winning-lane mock the protection tests share.
    /// Caller can mutate the returned result to flip individual fields and
    /// verify the classifier responds correctly.
    fn mock_h3_winning_workload(name: &str, rows: usize) -> WorkloadResult {
        let mut wl = mock_workload_result(name, rows, 10.0, 100.0);
        wl.category = "gpu_h3".to_owned();
        wl.kernel_class = "h3_latlng".to_owned();
        wl.plan_selected = false;
        wl.gpu_kernel_dispatched = false;
        wl.function_srf_kernel_dispatched = false;
        wl.dispatch_counter_captured = true;
        wl.gpu_kernel_execution_delta = 1;
        wl.accel_output_rows_consumed = 10;
        wl.pg_accel_stock_exec_delta = 0;
        wl.plan_snippet = Some(
            "HashAggregate\n  Output: (h3_latlng_to_cell(geom, 7)), count(*)\n  \
             Group Key: h3_latlng_to_cell(bench_h3_points.geom, 7)\n"
                .to_owned(),
        );
        wl
    }

    #[test]
    fn test_threshold_matrix_renders_evidence_columns() {
        let workload = mock_h3_winning_workload("h3_bulk", 1_000_000);
        let report = mock_report(vec![workload]);
        let md = report.to_markdown();

        assert!(md.contains("## Planner Threshold Matrix"));
        assert!(md.contains("Dispatch/Output Evidence"));
        assert!(md.contains("Correctness Evidence"));
        assert!(md.contains("Cache Gate"));
        assert!(md.contains("resident H3 GpuAgg"));
    }

    /// `h3_bulk` is the canonical H3 winning lane (~6s accel vs 90s PG
    /// parallel @ 10M on 2026-05-14). A representative happy-path measurement
    /// MUST be credited as `function_srf_kernel_dispatched` and counted in the
    /// GPU-dispatched geomean. Regression here would silently drop the bulk
    /// win from the headline.
    #[test]
    fn test_h3_bulk_winning_lane_credited_as_function_srf_dispatch() {
        let workload = mock_h3_winning_workload("h3_bulk", 1_000_000);
        let classification = workload.dispatch_classification();
        assert!(
            classification.function_srf_kernel_dispatched,
            "h3_bulk happy-path must be credited as function/SRF dispatch; \
             got classification={classification:?}"
        );
        assert!(
            classification.gpu_kernel_dispatched,
            "h3_bulk happy-path must be credited as GPU-dispatched (function/SRF \
             path), got classification={classification:?}"
        );
        assert!(
            !classification.planner_declined,
            "h3_bulk happy-path must NOT be marked planner_declined; \
             got classification={classification:?}"
        );
        assert_eq!(
            classification.function_kernel_count, 1,
            "h3_bulk happy-path kernel delta=1 must carry into function_kernel_count"
        );
    }

    /// `h3_resolution_sweep` is the second canonical H3 winning lane
    /// (~0.32s accel vs 8.4s PG parallel @ 1M on 2026-05-14). Same gate.
    #[test]
    fn test_h3_resolution_sweep_winning_lane_credited_as_function_srf_dispatch() {
        let workload = mock_h3_winning_workload("h3_resolution_sweep", 1_000_000);
        let classification = workload.dispatch_classification();
        assert!(
            classification.function_srf_kernel_dispatched,
            "h3_resolution_sweep happy-path must be credited as function/SRF dispatch"
        );
        assert!(
            classification.gpu_kernel_dispatched,
            "h3_resolution_sweep happy-path must be credited as GPU-dispatched"
        );
    }

    /// If the kernel counter delta is zero for an H3 winning-lane workload,
    /// the row must NOT be credited as GPU-dispatched. This is the regression
    /// gate: if the function/SRF dispatch hook breaks and stops incrementing
    /// the counter, `h3_bulk` falls out of the headline geomean.
    #[test]
    fn test_h3_bulk_with_zero_kernel_delta_is_not_dispatched() {
        let mut workload = mock_h3_winning_workload("h3_bulk", 1_000_000);
        workload.gpu_kernel_execution_delta = 0;
        let classification = workload.dispatch_classification();
        assert!(
            !classification.function_srf_kernel_dispatched,
            "h3_bulk with zero kernel delta must NOT be credited as function/SRF dispatch; \
             this gate detects the case where the function dispatch hook silently \
             stopped firing. Got: {classification:?}"
        );
        assert!(
            !classification.gpu_kernel_dispatched,
            "h3_bulk with zero kernel delta must NOT be credited as GPU-dispatched"
        );
        assert!(
            classification.planner_declined,
            "h3_bulk with no dispatch evidence must be classified as planner_declined; \
             a regression would silently route h3_bulk to native PG without alerting"
        );
    }

    /// Same gate for `h3_resolution_sweep`.
    #[test]
    fn test_h3_resolution_sweep_with_zero_kernel_delta_is_not_dispatched() {
        let mut workload = mock_h3_winning_workload("h3_resolution_sweep", 1_000_000);
        workload.gpu_kernel_execution_delta = 0;
        let classification = workload.dispatch_classification();
        assert!(
            !classification.function_srf_kernel_dispatched,
            "h3_resolution_sweep with zero kernel delta must NOT be credited as dispatch"
        );
        assert!(classification.planner_declined);
    }

    /// If a stock-executor fallback fires (`pg_accel_stock_exec_delta > 0`),
    /// the row must NOT be credited as GPU-dispatched even when the kernel
    /// counter is positive. This catches the silent "GPU ran AND CPU
    /// fell back" path which is a Phase 1 anti-cheat rail.
    #[test]
    fn test_h3_bulk_with_stock_exec_fallback_is_not_dispatched() {
        let mut workload = mock_h3_winning_workload("h3_bulk", 1_000_000);
        workload.pg_accel_stock_exec_delta = 1;
        let classification = workload.dispatch_classification();
        assert!(
            !classification.function_srf_kernel_dispatched,
            "h3_bulk that also fell back to the stock executor must NOT be credited"
        );
        assert!(!classification.gpu_kernel_dispatched);
    }

    /// The `h3_cell_to_parent` benchmark is the fused grouped COUNT(*) shape,
    /// not standalone scalar exposure. A selected Custom Scan with a kernel
    /// delta must be credited as GPU dispatch rather than function/SRF
    /// fallback.
    #[test]
    fn test_h3_cell_to_parent_grouped_count_row_classified_as_gpu_dispatched() {
        let mut workload = mock_workload_result("h3_cell_to_parent", 1_000_000, 10.0, 3.0);
        workload.category = "gpu_h3".to_owned();
        workload.kernel_class = "h3_cell_to_parent".to_owned();
        workload.plan_selected = true;
        workload.gpu_kernel_dispatched = true;
        workload.function_srf_kernel_dispatched = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 2;
        workload.accel_output_rows_consumed = 32;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAgg)\n  Output: (h3_cell_to_parent(cell, 4)), count(*)\n  \
             Group Key: h3_cell_to_parent(bench_h3_parent.cell, 4)\n  \
             GPU Kernel Dispatched: true\n"
                .to_owned(),
        );

        let classification = workload.dispatch_classification();
        assert!(
            !classification.function_srf_kernel_dispatched,
            "grouped parent-count should be a Custom Scan dispatch, not function/SRF dispatch"
        );
        assert!(
            classification.gpu_kernel_dispatched,
            "selected grouped parent-count row must be credited as GPU-dispatched"
        );
        assert!(
            !classification.planner_declined,
            "selected grouped parent-count row must not be marked planner_declined"
        );
    }

    /// If a quarantined scalar H3 lane accidentally starts incrementing the
    /// kernel counter (e.g. an agent re-registers it in the h3 adapter), the row
    /// will look like a function/SRF dispatch in the report — that surfaces
    /// clearly in the dispatch classification because the report-side
    /// heuristic is purely name-prefix + counter-driven. The lane policy
    /// itself (parity vs winning) is enforced by `h3_lane_class()` in
    /// `pg_accel_bench/src/workloads/mod.rs`. This test pins what the bench
    /// report WILL show in that regression scenario so a future runner-side
    /// cross-check has a stable surface to assert against.
    #[test]
    fn test_h3_grid_distance_with_unexpected_kernel_delta_classifier_visible() {
        let mut workload = mock_workload_result("h3_grid_distance", 1_000_000, 10.0, 12.0);
        workload.category = "gpu_h3".to_owned();
        workload.kernel_class = "h3_grid_distance".to_owned();
        workload.plan_selected = false;
        workload.gpu_kernel_dispatched = false;
        workload.function_srf_kernel_dispatched = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet =
            Some("Aggregate\n  Output: avg(h3_grid_distance(cell_a, cell_b))\n".to_owned());

        let classification = workload.dispatch_classification();
        assert!(
            classification.function_srf_kernel_dispatched,
            "report classifier must surface parity-lane kernel deltas as function/SRF \
             dispatch so a regression in the h3 adapter quarantine (where a parity-lane \
             scalar op accidentally gets re-registered and fires a kernel) is loudly \
             visible in the bench report. The lane policy itself (parity vs winning) \
             is enforced by h3_lane_class() in workloads/mod.rs."
        );
        // The lane policy is the orthogonal gate; assert it disagrees with the
        // dispatch classification so a future cross-check can flag this as a
        // protected-lane violation.
        assert_eq!(
            crate::workloads::h3_lane_class("h3_grid_distance"),
            Some(crate::workloads::H3LaneClass::Parity),
            "h3_grid_distance must remain a parity lane; dispatch classifier crediting \
             it as function/SRF is a regression signal, not an admission of winning"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 5 H3 winning-lane protection — report-level hard gate
    //
    // These three tests pin `BenchReport::evaluate_h3_lane_gate` to the
    // contract documented on the function: a Winner that regresses below
    // `H3_LANE_GATE_MIN_WARM_SPEEDUP` (1.0x), a Winner that fails to
    // dispatch, and a Parity row that dispatches all surface as gate
    // failures. The bench process turns these into a non-zero exit code in
    // `main::enforce_h3_lane_gate`, so a regression that prints red text in
    // the report can no longer leave CI green.
    // -----------------------------------------------------------------------

    /// `h3_bulk` is a canonical H3 Winner. Dispatched, but with a
    /// `speedup_median_vs_parallel` of 0.5x (well below the 1.0x gate floor)
    /// MUST produce exactly one `WinnerBelowFloor` failure.
    #[test]
    fn test_h3_winner_below_threshold_triggers_gate_fail() {
        // 20ms accel vs 10ms parallel → median speedup ~0.5x
        let mut workload = mock_workload_result("h3_bulk", 1_000_000, 20.0, 10.0);
        workload.category = "gpu_h3".to_owned();
        workload.kernel_class = "h3_latlng".to_owned();
        // The gate reads `gpu_kernel_dispatched` AFTER `with_normalized_dispatch`
        // re-runs the classifier, so seed the inputs the classifier consumes.
        workload.plan_selected = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet = Some(
            "HashAggregate\n  Output: (h3_latlng_to_cell(geom, 7)), count(*)\n  \
             Group Key: h3_latlng_to_cell(bench_h3_points.geom, 7)\n"
                .to_owned(),
        );

        let report = mock_report(vec![workload]);
        let failures = report.evaluate_h3_lane_gate();
        assert_eq!(
            failures.len(),
            1,
            "regressed h3_bulk Winner must produce exactly one gate failure; got: {failures:?}"
        );
        assert_eq!(failures[0].workload, "h3_bulk");
        assert_eq!(failures[0].kind, H3LaneGateFailureKind::WinnerBelowFloor);
        assert!(
            failures[0].speedup_median < H3_LANE_GATE_MIN_WARM_SPEEDUP,
            "failure record must carry the observed sub-floor speedup; got {sp}",
            sp = failures[0].speedup_median,
        );

        // The markdown must render the `### H3 Lane Gate Failures` section.
        let md = report.to_markdown();
        assert!(
            md.contains("### H3 Lane Gate Failures"),
            "markdown must surface the gate failure section; full md:\n{md}"
        );
        assert!(
            md.contains("winner_below_floor"),
            "failure section must label the failure kind; full md:\n{md}"
        );
        assert!(
            md.contains("h3_bulk"),
            "failure section must name the failing workload; full md:\n{md}"
        );
    }

    /// `h3_grid_distance` is a canonical H3 Parity workload. If a row shows
    /// `gpu_kernel_dispatched=true` (a quarantine breach), the gate MUST
    /// produce a `ParityUnexpectedlyDispatched` failure.
    #[test]
    fn test_h3_parity_unexpectedly_dispatched_triggers_gate_fail() {
        let mut workload = mock_workload_result("h3_grid_distance", 1_000_000, 10.0, 12.0);
        workload.category = "gpu_h3".to_owned();
        workload.kernel_class = "h3_grid_distance".to_owned();
        // Quarantine breach: the parity-lane row fired a kernel.
        workload.plan_selected = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet =
            Some("Aggregate\n  Output: avg(h3_grid_distance(cell_a, cell_b))\n".to_owned());

        let report = mock_report(vec![workload]);
        let failures = report.evaluate_h3_lane_gate();
        assert_eq!(
            failures.len(),
            1,
            "parity-lane dispatch must produce exactly one gate failure; got: {failures:?}"
        );
        assert_eq!(failures[0].workload, "h3_grid_distance");
        assert_eq!(
            failures[0].kind,
            H3LaneGateFailureKind::ParityUnexpectedlyDispatched
        );

        let md = report.to_markdown();
        assert!(
            md.contains("### H3 Lane Gate Failures"),
            "markdown must surface gate failure section; md:\n{md}"
        );
        assert!(
            md.contains("parity_unexpectedly_dispatched"),
            "failure section must label parity breach kind; md:\n{md}"
        );
    }

    #[test]
    fn test_h3_below_grouped_floor_passes_as_native_decline() {
        let mut workload = mock_workload_result("h3_bulk", 10_000, 10.0, 30.0);
        workload.category = "gpu_h3".to_owned();
        workload.kernel_class = "h3_latlng".to_owned();
        workload.plan_selected = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 0;
        workload.accel_output_rows_consumed = 9_998;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet =
            Some("HashAggregate\n  Output: (h3_latlng_to_cell(geom, 7)), count(*)\n".to_owned());
        // Honest evidence: the planner itself reported the expected decline
        // reason. Without this field populated (or with it tagged
        // `ExpectedUnconfirmed`) the generic ship gate must NOT pass — see
        // `test_h3_below_grouped_floor_native_decline_requires_confirmed_evidence`.
        workload.native_decline_evidence = Some(NativeDeclineEvidence {
            reason: "h3_rows_below_grouped_agg_min".to_owned(),
            source: DeclineReasonSource::PlannerReported,
        });

        let report = mock_report(vec![workload]);
        assert!(
            report.evaluate_benchmark_ship_gate().is_empty(),
            "below-floor h3_bulk row must satisfy the generic native-decline ship gate \
             when the planner reported the expected decline reason"
        );
        let failures = report.evaluate_h3_lane_gate();
        assert!(
            failures.is_empty(),
            "below-floor h3_bulk row must follow the threshold matrix native-decline expectation; got: {failures:?}"
        );

        let md = report.to_markdown();
        assert!(
            !md.contains("### H3 Lane Gate Failures"),
            "below-floor native-decline row must not render an H3 gate failure; md:\n{md}"
        );
    }

    /// Counterpart to the test above: without a planner-reported decline
    /// reason, the same below-floor `h3_bulk` row must NOT pass the generic
    /// ship gate — neither when no evidence at all was captured, nor when
    /// the evidence is present but tagged `ExpectedUnconfirmed` (the static
    /// matrix assumed the decline; the planner never confirmed it).
    #[test]
    fn test_h3_below_grouped_floor_native_decline_requires_confirmed_evidence() {
        let base = || {
            let mut workload = mock_workload_result("h3_bulk", 10_000, 10.0, 30.0);
            workload.category = "gpu_h3".to_owned();
            workload.kernel_class = "h3_latlng".to_owned();
            workload.plan_selected = false;
            workload.dispatch_counter_captured = true;
            workload.gpu_kernel_execution_delta = 0;
            workload.accel_output_rows_consumed = 9_998;
            workload.pg_accel_stock_exec_delta = 0;
            workload.plan_snippet = Some(
                "HashAggregate\n  Output: (h3_latlng_to_cell(geom, 7)), count(*)\n".to_owned(),
            );
            workload
        };

        let no_evidence = base();
        let report = mock_report(vec![no_evidence]);
        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(
            failures.len(),
            1,
            "missing decline evidence must fail the generic ship gate"
        );
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::NativeDeclineReasonMissing
        );

        let mut unconfirmed = base();
        unconfirmed.native_decline_evidence = Some(NativeDeclineEvidence {
            reason: "h3_rows_below_grouped_agg_min".to_owned(),
            source: DeclineReasonSource::ExpectedUnconfirmed,
        });
        let report = mock_report(vec![unconfirmed]);
        let failures = report.evaluate_benchmark_ship_gate();
        assert_eq!(
            failures.len(),
            1,
            "unconfirmed expected-decline evidence must fail the generic ship gate"
        );
        assert_eq!(
            failures[0].kind,
            BenchmarkShipGateFailureKind::NativeDeclineReasonMissing
        );
    }

    #[test]
    fn test_benchmark_ship_gate_allows_small_raster_native_decline() {
        let mut workload = mark_no_dispatch(
            mock_workload_result("raster_reclass", 100, 10.0, 10.0),
            "Aggregate\n  ->  Seq Scan on bench_raster_tiles",
            "Aggregate\n  ->  Seq Scan on bench_raster_tiles",
        );
        workload.native_decline_evidence = Some(NativeDeclineEvidence {
            reason: "shape_unsupported_rte".to_owned(),
            source: DeclineReasonSource::PlannerReported,
        });
        let report = mock_report(vec![workload]);

        assert!(
            report.evaluate_benchmark_ship_gate().is_empty(),
            "small raster native-decline row must satisfy the operation-specific threshold matrix \
             when the planner reported the expected decline reason"
        );
    }

    /// Happy path: every H3 Winner dispatches AND beats the gate floor; no
    /// Parity row dispatches. The gate produces zero failures and the
    /// `### H3 Lane Gate Failures` section is omitted from the markdown.
    #[test]
    fn test_h3_winners_all_above_threshold_passes() {
        // Build a representative happy-path Winner for every name in
        // `h3_winning_lane_names()`. Each is 10ms accel vs 50ms parallel →
        // median speedup ~5x, well above the 1.0x gate floor and the highest
        // per-Winner advisory threshold (1.5x).
        let mut workloads = Vec::new();
        for name in crate::workloads::h3_winning_lane_names() {
            let mut wl = mock_workload_result(name, 1_000_000, 10.0, 50.0);
            wl.category = if name == "h3_fp64_ops" {
                "fp64_matrix".to_owned()
            } else {
                "gpu_h3".to_owned()
            };
            wl.kernel_class = if name == "h3_cell_to_parent" {
                "h3_cell_to_parent".to_owned()
            } else {
                "h3_latlng".to_owned()
            };
            wl.plan_selected = name == "h3_cell_to_parent";
            wl.dispatch_counter_captured = true;
            wl.gpu_kernel_execution_delta = 1;
            wl.accel_output_rows_consumed = 10;
            wl.pg_accel_stock_exec_delta = 0;
            wl.plan_snippet = Some(if name == "h3_cell_to_parent" {
                "Custom Scan (GpuAgg)\n  Output: (h3_cell_to_parent(cell, 4)), count(*)\n  \
                 Group Key: h3_cell_to_parent(bench_h3_parent.cell, 4)\n"
                    .to_owned()
            } else {
                "HashAggregate\n  Output: (h3_latlng_to_cell(geom, 7)), count(*)\n  \
                 Group Key: h3_latlng_to_cell(bench_h3_points.geom, 7)\n"
                    .to_owned()
            });
            workloads.push(wl);
        }
        // And an h3_grid_distance parity row that stays native (no kernel
        // delta) — the gate must NOT flag it.
        let mut parity = mock_workload_result("h3_grid_distance", 1_000_000, 10.0, 10.0);
        parity.category = "gpu_h3".to_owned();
        parity.kernel_class = "h3_grid_distance".to_owned();
        parity.plan_selected = false;
        parity.dispatch_counter_captured = true;
        parity.gpu_kernel_execution_delta = 0;
        parity.accel_output_rows_consumed = 1;
        parity.pg_accel_stock_exec_delta = 0;
        parity.plan_snippet =
            Some("Aggregate\n  Output: avg(h3_grid_distance(cell_a, cell_b))\n".to_owned());
        workloads.push(parity);

        let report = mock_report(workloads);
        let failures = report.evaluate_h3_lane_gate();
        assert!(
            failures.is_empty(),
            "happy-path H3 lane suite must produce no gate failures; got: {failures:?}"
        );

        // The markdown must NOT render the failure section when the gate is
        // green — otherwise the section becomes noise that reviewers learn to
        // ignore.
        let md = report.to_markdown();
        assert!(
            !md.contains("### H3 Lane Gate Failures"),
            "markdown must omit the gate failure section when no failures exist; md:\n{md}"
        );
    }

    /// Phase 5 acceptance: every canonical H3 winning lane must be classified
    /// as Winning by `h3_lane_class` (a cross-module pin so the report tests
    /// stay aligned with the workload registry).
    #[test]
    fn test_h3_winning_lanes_have_lane_class_winning() {
        for name in crate::workloads::h3_winning_lane_names() {
            match crate::workloads::h3_lane_class(name) {
                Some(crate::workloads::H3LaneClass::Winning { min_warm_speedup }) => {
                    assert!(
                        min_warm_speedup >= 1.0,
                        "H3 winning lane `{name}` must require >= 1.0x speedup; \
                         got min_warm_speedup={min_warm_speedup}"
                    );
                }
                other => panic!(
                    "report-side winning-lane gate failed: `{name}` not classified \
                     as Winning by h3_lane_class(); got {other:?}"
                ),
            }
        }
    }

    #[test]
    fn test_geomean_by_dispatch_source_splits_custom_scan_vs_function_srf() {
        // Custom Scan win: reduce_sum_i64-style row with a Custom Scan plan
        // and runtime kernel delta.
        let mut custom_scan_workload =
            mock_workload_result("gpu_reduce_sum_i64", 1_000_000, 5.0, 50.0);
        custom_scan_workload.category = "gpu_reduce".to_owned();
        custom_scan_workload.kernel_class = "reduce".to_owned();
        custom_scan_workload.plan_selected = true;
        custom_scan_workload.gpu_kernel_dispatched = true;
        custom_scan_workload.function_srf_kernel_dispatched = false;
        custom_scan_workload.dispatch_counter_captured = true;
        custom_scan_workload.gpu_kernel_execution_delta = 1;
        custom_scan_workload.pg_accel_rows_dispatched_delta = 1_000_000;
        custom_scan_workload.pg_accel_gpu_rows_processed_delta = 1_000_000;
        custom_scan_workload.accel_output_rows_consumed = 1;
        custom_scan_workload.plan_snippet =
            Some("Custom Scan (GpuAccelReduce)\n  Strategy: GpuReduce\n".to_owned());

        // Function/SRF win: H3 bulk row with no Custom Scan plan but kernel
        // delta and consumed output.
        let mut function_srf_workload = mock_workload_result("h3_bulk", 1_000_000, 7.0, 70.0);
        function_srf_workload.category = "gpu_h3".to_owned();
        function_srf_workload.kernel_class = "h3_latlng".to_owned();
        function_srf_workload.plan_selected = false;
        function_srf_workload.gpu_kernel_dispatched = false;
        function_srf_workload.function_srf_kernel_dispatched = false;
        function_srf_workload.dispatch_counter_captured = true;
        function_srf_workload.gpu_kernel_execution_delta = 1;
        function_srf_workload.accel_output_rows_consumed = 12;
        function_srf_workload.plan_snippet = Some(
            "HashAggregate\n  Output: (h3_latlng_to_cell(geom, 7)), count(*)\n  \
             Group Key: h3_latlng_to_cell(bench_h3_points.geom, 7)\n"
                .to_owned(),
        );

        let report = mock_report(vec![custom_scan_workload, function_srf_workload]);
        let md = report.to_markdown();

        // The split table must exist and credit each path separately.
        assert!(
            md.contains("### Geomean by Dispatch Source"),
            "report missing Geomean by Dispatch Source section: {md}"
        );
        // One Custom Scan workload, one function/SRF workload.
        assert!(
            md.contains("| Custom Scan dispatch | 1 |"),
            "Custom Scan dispatch row count wrong: {md}"
        );
        assert!(
            md.contains("| Function/SRF kernel dispatch | 1 |"),
            "Function/SRF kernel dispatch row count wrong: {md}"
        );
        // The combined headline still counts both.
        assert!(
            md.contains("geomean across 2 GPU-dispatched workloads"),
            "combined headline must still see both workloads: {md}"
        );
    }

    #[test]
    fn test_geomean_by_dispatch_source_renders_zero_buckets() {
        // No GPU-dispatched workloads at all: both buckets must still render
        // with zero counts and a NaN geomean so the report shape is stable.
        let mut workload = mock_workload_result("native_only", 100_000, 10.0, 10.0);
        workload.plan_selected = false;
        workload.gpu_kernel_dispatched = false;
        workload.function_srf_kernel_dispatched = false;
        workload.dispatch_counter_captured = true;
        workload.plan_snippet = Some("Aggregate\n  -> Seq Scan on public.bench\n".to_owned());
        let report = mock_report(vec![workload]);

        let md = report.to_markdown();
        assert!(
            md.contains("### Geomean by Dispatch Source"),
            "split table must render even when nothing is dispatched: {md}"
        );
        assert!(
            md.contains("| Custom Scan dispatch | 0 |"),
            "Custom Scan dispatch zero bucket missing: {md}"
        );
        assert!(
            md.contains("| Function/SRF kernel dispatch | 0 |"),
            "Function/SRF kernel dispatch zero bucket missing: {md}"
        );
    }

    #[test]
    fn test_cpu_backed_pg_accel_plan_is_not_a_win() {
        let mut workload = mock_workload_result("spatial_filter", 100_000, 10.0, 50.0);
        workload.plan_selected = true;
        workload.gpu_kernel_dispatched = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.pg_accel_stock_exec_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.plan_snippet = Some("Custom Scan (GpuAccelScan)\n".to_owned());
        let report = mock_report(vec![workload]);

        let md = report.to_markdown();
        assert!(md.contains("geomean across 0 GPU-dispatched workloads"));
        assert!(md.contains("| pg_accel stock executor fallback delta > 0 | 1 |"));
    }

    #[test]
    fn test_explicit_gpu_dispatched_false_is_plan_selected_only() {
        let mut workload = mock_workload_result("spatial_sel_90pct", 100_000, 20.0, 10.0);
        workload.plan_selected = true;
        workload.gpu_kernel_dispatched = true;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  -> Seq Scan on public.bench\n  \
             GPU Dispatched: false\n"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        let md = report.to_markdown();
        assert!(md.contains("geomean across 0 GPU-dispatched workloads"));
        assert!(md.contains("| pg_accel Custom Scan plan selected | 1 |"));
        assert!(md.contains("| Custom Scan selected but no GPU dispatch | 1 |"));

        let csv = report.to_csv();
        assert!(csv.contains(
            "spatial_sel_90pct,gpu,unclassified,true,false,false,false,true,0,0,\
             selected_not_dispatched,false,0,0,0,0,0,0,100000"
        ));
    }

    #[test]
    fn test_plan_explicit_gpu_kernel_dispatched_aliases_are_parsed() {
        assert_eq!(
            plan_explicit_gpu_dispatched("Custom Scan\n  GPU Kernel Dispatched: true\n"),
            Some(true)
        );
        assert_eq!(
            plan_explicit_gpu_dispatched("Custom Scan\n  GPU Kernel Dispatched: false\n"),
            Some(false)
        );
    }

    #[test]
    fn test_dispatch_classification_marks_planner_decline() {
        let mut workload = mock_workload_result("native_only", 100_000, 10.0, 10.0);
        workload.plan_selected = false;
        workload.gpu_kernel_dispatched = false;
        workload.dispatch_counter_captured = true;
        workload.plan_snippet = Some("Aggregate\n  -> Seq Scan on public.bench\n".to_owned());

        let classification = workload.dispatch_classification();

        assert!(classification.planner_declined);
        assert!(!classification.plan_selected);
        assert!(!classification.gpu_kernel_dispatched);
        assert_eq!(
            classification.gpu_resident_pipeline,
            GpuResidentPipelineStatus::PlannerDeclined
        );
    }

    #[test]
    fn test_dispatch_classification_marks_custom_scan_no_dispatch() {
        let mut workload = mock_workload_result("plan_only", 100_000, 10.0, 10.0);
        workload.plan_selected = false;
        workload.gpu_kernel_dispatched = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 1;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: false\n".to_owned(),
        );

        let classification = workload.dispatch_classification();

        assert!(classification.plan_selected);
        assert!(!classification.gpu_kernel_dispatched);
        assert!(classification.custom_scan_selected_not_dispatched);
        assert_eq!(
            classification.gpu_resident_pipeline,
            GpuResidentPipelineStatus::SelectedNotDispatched
        );
    }

    #[test]
    fn test_dispatch_classification_honors_json_gpu_dispatched_false() {
        let mut workload = mock_workload_result("json_plan_only", 100_000, 10.0, 10.0);
        workload.plan_selected = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 2;
        workload.accel_output_rows_consumed = 1;
        workload.plan_snippet =
            Some(r#"[{"Plan":{"Node Type":"Custom Scan","GPU Dispatched":false}}]"#.to_owned());

        let classification = workload.dispatch_classification();

        assert!(classification.plan_selected);
        assert!(!classification.gpu_kernel_dispatched);
        assert!(classification.custom_scan_selected_not_dispatched);
        assert_eq!(classification.function_kernel_count, 0);
    }

    #[test]
    fn test_full_plan_gpu_dispatched_false_overrides_counter_delta() {
        let mut workload = mock_workload_result("artifact_plan_only", 100_000, 10.0, 10.0);
        workload.plan_selected = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 2;
        workload.accel_output_rows_consumed = 1;
        workload.plan_snippet = Some("Custom Scan (GpuAccelAgg)\n".to_owned());
        let full_plan = "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: false\n";

        let classification = workload.dispatch_classification_with_plan_artifact(Some(full_plan));

        assert!(classification.plan_selected);
        assert!(!classification.gpu_kernel_dispatched);
        assert!(classification.custom_scan_selected_not_dispatched);
    }

    #[test]
    fn test_dispatch_classification_counts_function_kernel_rows() {
        let mut workload = mock_workload_result("h3_bulk", 100_000, 10.0, 50.0);
        workload.category = "gpu_h3".to_owned();
        workload.kernel_class = "h3_latlng".to_owned();
        workload.plan_selected = false;
        workload.gpu_kernel_dispatched = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 3;
        workload.accel_output_rows_consumed = 12;

        let classification = workload.dispatch_classification();

        assert!(classification.gpu_kernel_dispatched);
        assert!(classification.function_srf_kernel_dispatched);
        assert_eq!(classification.function_kernel_count, 3);
        assert_eq!(classification.rows_returned_to_cpu, 12);
        assert_eq!(
            classification.gpu_resident_pipeline,
            GpuResidentPipelineStatus::NotApplicableFunctionSrf
        );
    }

    #[test]
    fn test_dispatch_classification_marks_selected_strategy_not_resident_without_true_audit() {
        let mut workload = mock_workload_result("ssbm_q1", 1_000_000, 10.0, 50.0);
        workload.plan_selected = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 2;
        workload.pg_accel_rows_dispatched_delta = 1_000_000;
        workload.pg_accel_gpu_rows_processed_delta = 1_000_000;
        workload.accel_output_rows_consumed = 10;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelPreAgg)\n  Strategy: GpuPreAgg\n  Fact Rows Scanned: 1000000\n"
                .to_owned(),
        );

        let classification = workload.dispatch_classification();

        assert!(classification.plan_selected);
        assert!(classification.gpu_kernel_dispatched);
        assert_eq!(
            classification.gpu_resident_pipeline,
            GpuResidentPipelineStatus::NotResident
        );
    }

    #[test]
    fn test_dispatch_classification_rejects_bare_gpu_resident_true_without_proof() {
        let mut workload = mock_workload_result("ssbm_q1", 1_000_000, 10.0, 50.0);
        workload.plan_selected = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 2;
        workload.accel_output_rows_consumed = 10;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelPreAgg)\n  Strategy: GpuPreAgg\n  GPU Resident Pipeline: true\n"
                .to_owned(),
        );

        let classification = workload.dispatch_classification();

        assert_eq!(
            classification.gpu_resident_pipeline,
            GpuResidentPipelineStatus::NotResident
        );
    }

    #[test]
    fn test_dispatch_classification_honors_versioned_gpu_resident_proof() {
        let mut workload = mock_workload_result("ssbm_q1", 1_000_000, 10.0, 50.0);
        workload.plan_selected = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 2;
        workload.accel_output_rows_consumed = 10;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelPreAgg)\n  Strategy: GpuPreAgg\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_preagg\n  \
             GPU Resident Stage Mask: 33\n  \
             GPU Resident Device Columns: 4\n"
                .to_owned(),
        );

        let classification = workload.dispatch_classification();

        assert_eq!(
            classification.gpu_resident_pipeline,
            GpuResidentPipelineStatus::Reported
        );
    }

    #[test]
    fn test_json_gpu_resident_proof_metadata_is_parsed_structurally() {
        let plan = r#"[{"Plan":{"Node Type":"Custom Scan","GPU Resident Pipeline":true,"GPU Resident Proof Version":2,"GPU Resident Operator Class":"resident_preagg","GPU Resident Stage Mask":33,"GPU Resident Device Columns":4}}]"#;

        assert!(plan_contains_gpu_resident_pipeline_evidence(plan));
    }

    #[test]
    fn test_dispatch_classification_does_not_let_child_resident_proof_satisfy_parent() {
        let mut workload = mock_workload_result("nested_custom_scan", 1_000_000, 10.0, 50.0);
        workload.plan_selected = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 2;
        workload.accel_output_rows_consumed = 10;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  \
             GPU Resident Pipeline: false\n  \
             GPU Resident Boundary: GpuAgg drains heap or child tuples on CPU\n  \
             -> Custom Scan (GpuAccelScan)\n       Strategy: GpuScan\n       \
             GPU Resident Pipeline: true\n       GPU Resident Proof Version: 2\n       \
             GPU Resident Operator Class: resident_expression\n       \
             GPU Resident Stage Mask: 3\n       GPU Resident Device Columns: 2\n"
                .to_owned(),
        );

        let classification = workload.dispatch_classification();

        assert_eq!(
            classification.gpu_resident_pipeline,
            GpuResidentPipelineStatus::NotResident
        );
    }

    #[test]
    fn test_report_preserves_gpu_resident_boundary_reason() {
        let mut workload = mock_workload_result("gpu_reduce_sum", 100_000, 10.0, 50.0);
        workload.plan_selected = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 2;
        workload.accel_output_rows_consumed = 1;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Resident Pipeline: false\n  \
             GPU Resident Boundary: GpuAgg drains heap or child tuples on CPU and stages host input/key/value buffers before GPU reduce or grouped aggregation\n"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        let json = report.to_json().expect("json");
        let markdown = report.to_markdown();

        assert!(json.contains("\"gpu_resident_pipeline\": \"not_resident\""));
        assert!(json.contains("\"gpu_resident_boundary_reason\""));
        assert!(markdown.contains(
            "GpuAgg drains heap or child tuples on CPU and stages host input/key/value buffers"
        ));
        assert!(markdown.contains("| Custom Scan rows with recorded CPU boundary | 1 |"));
    }

    #[test]
    fn test_report_preserves_boundary_reason_for_selected_not_dispatched_rows() {
        let mut workload = mock_workload_result("plan_only", 100_000, 10.0, 10.0);
        workload.plan_selected = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 1;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: false\n  \
             GPU Resident Pipeline: false\n  \
             GPU Resident Boundary: GpuAgg drains heap or child tuples on CPU and stages host input/key/value buffers before GPU reduce or grouped aggregation\n"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        let json = report.to_json().expect("json");
        let markdown = report.to_markdown();

        assert!(json.contains("\"gpu_resident_pipeline\": \"selected_not_dispatched\""));
        assert!(json.contains("\"gpu_resident_boundary_reason\""));
        assert!(markdown.contains("custom_scan_selected_not_dispatched"));
        assert!(markdown.contains(
            "GpuAgg drains heap or child tuples on CPU and stages host input/key/value buffers"
        ));
    }

    #[test]
    fn test_resident_boundary_audit_flags_selected_custom_scan_boundary() {
        let mut workload = mock_workload_result("generic_gpu_dispatch", 100_000, 10.0, 20.0);
        workload.plan_selected = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Dispatched: true\n".to_owned(),
        );
        let audit = mock_report(vec![workload]).resident_boundary_audit();

        assert_eq!(audit.schema_version, RESIDENT_BOUNDARY_AUDIT_SCHEMA_VERSION);
        assert_eq!(audit.selected_custom_scan_rows, 1);
        assert_eq!(audit.passed_rows, 0);
        assert_eq!(audit.failed_rows, 1);
        assert!(audit.has_failures());
        assert_eq!(
            audit.rows[0].status,
            ResidentBoundaryAuditStatus::NonResidentPipeline
        );
        assert_eq!(audit.rows[0].gpu_resident_pipeline, "not_resident");
        assert!(
            audit.rows[0]
                .boundary_reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with("GpuAgg drains heap or child tuples"))
        );
    }

    #[test]
    fn test_resident_boundary_audit_flags_selected_custom_scan_missing_evidence() {
        let mut workload = mock_workload_result("generic_gpu_dispatch", 100_000, 10.0, 20.0);
        workload.plan_selected = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet =
            Some("Custom Scan (GpuAccelMystery)\n  GPU Dispatched: true\n".to_owned());
        let audit = mock_report(vec![workload]).resident_boundary_audit();

        assert_eq!(audit.selected_custom_scan_rows, 1);
        assert_eq!(audit.passed_rows, 0);
        assert_eq!(audit.failed_rows, 1);
        assert!(audit.has_failures());
        assert_eq!(
            audit.rows[0].status,
            ResidentBoundaryAuditStatus::MissingResidentPipelineEvidence
        );
        assert_eq!(audit.rows[0].gpu_resident_pipeline, "not_reported");
        assert!(audit.rows[0].boundary_reason.is_none());
        assert!(
            audit
                .to_markdown()
                .contains("missing_resident_pipeline_evidence")
        );
    }

    #[test]
    fn test_resident_boundary_audit_ignores_function_srf_dispatch_rows() {
        let mut workload = mock_workload_result("h3_bulk", 100_000, 10.0, 50.0);
        workload.category = "gpu_h3".to_owned();
        workload.kernel_class = "h3_latlng".to_owned();
        workload.plan_selected = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        let audit = mock_report(vec![workload]).resident_boundary_audit();

        assert_eq!(audit.selected_custom_scan_rows, 0);
        assert_eq!(audit.ignored_non_custom_scan_rows, 1);
        assert!(audit.rows.is_empty());
    }

    #[test]
    fn test_gpu_resident_false_does_not_count_as_positive_evidence() {
        let plan =
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Resident Pipeline: false\n";

        assert!(!plan_contains_gpu_resident_pipeline_evidence(plan));
        assert_eq!(plan_gpu_resident_pipeline_value(plan), Some(false));
        assert_eq!(
            infer_gpu_resident_boundary_reason_from_plan(plan).as_deref(),
            Some(
                "GpuAgg drains heap or child tuples on CPU and stages host input/key/value buffers before GPU reduce or grouped aggregation"
            )
        );
    }

    #[test]
    fn test_boundary_reason_strategy_map_covers_explain_labels() {
        let expected = [
            (
                "GpuScan",
                "GpuScan consumes heap or child tuples on CPU via table_scan_getnextslot/ExecProcNode/MinimalTuple staging and emits PostgreSQL slots",
            ),
            (
                "GpuJoin",
                "GpuJoin collects child rows through ExecProcNode into host MinimalTuple/key buffers and reconstructs joined PostgreSQL slots",
            ),
            (
                "GpuAgg",
                "GpuAgg drains heap or child tuples on CPU and stages host input/key/value buffers before GPU reduce or grouped aggregation",
            ),
            (
                "GpuSort",
                "GpuSort materializes input tuples on CPU, sends key vectors only, reorders host MinimalTuples, and emits PostgreSQL slots",
            ),
            (
                "GpuWindow",
                "GpuWindow buffers input MinimalTuples, extracts host columns, stores host result vectors, and emits PostgreSQL slots",
            ),
            (
                "GpuPreAgg",
                "GpuPreAgg materializes dimensions in host HashMap state and scans/probes fact rows through ExecProcNode/materialized slots",
            ),
            (
                "GpuFunctionScan",
                "GpuFunctionScan dispatches constant arguments once, buffers host Datums, and drains output through PostgreSQL slots",
            ),
            (
                "GpuAccelSrfTargetList",
                "GpuAccelSrfTargetList drives ProjectSet input through ExecProcNode, buffers per-row SRF output, and emits expanded PostgreSQL tuples",
            ),
        ];

        for (strategy, reason) in expected {
            assert_eq!(
                gpu_resident_boundary_reason_for_strategy(strategy),
                Some(reason)
            );
            assert!(
                reason.contains("CPU")
                    || reason.contains("ExecProcNode")
                    || reason.contains("host")
                    || reason.contains("PostgreSQL slots")
                    || reason.contains("PostgreSQL tuples"),
                "{strategy} boundary reason must identify the CPU/PostgreSQL boundary: {reason}"
            );
        }
        assert_eq!(
            gpu_resident_boundary_reason_for_strategy("GpuUnknown"),
            None
        );
    }

    #[test]
    #[allow(clippy::expect_used)]
    fn test_json_roundtrip() {
        let report = mock_report(vec![mock_workload_result("json_wl", 100_000, 8.0, 16.0)]);
        let json_str = report.to_json().expect("serialization should succeed");
        let deserialized: BenchReport =
            serde_json::from_str(&json_str).expect("deserialization should succeed");
        assert_eq!(deserialized.workloads.len(), 1);
        assert_eq!(deserialized.workloads[0].rows, 100_000);
    }

    #[test]
    #[allow(clippy::expect_used)]
    fn test_json_contains_all_fields() {
        let report = mock_report(vec![mock_workload_result("fields", 100_000, 10.0, 20.0)]);
        let json_str = report.to_json().expect("serialization should succeed");
        assert!(json_str.contains("\"rows\""));
        assert!(json_str.contains("\"accel_mean_ms\""));
        assert!(json_str.contains("\"parallel_mean_ms\""));
        assert!(json_str.contains("\"row_scales\""));
        assert!(json_str.contains("\"planner_declined\""));
        assert!(json_str.contains("\"function_kernel_count\""));
        assert!(json_str.contains("\"rows_returned_to_cpu\""));
        assert!(json_str.contains("\"gpu_resident_pipeline\""));
        assert!(json_str.contains("\"correctness_diff_artifact\""));
        // Must NOT contain single-threaded fields
        assert!(!json_str.contains("\"single_mean_ms\""));
    }

    #[test]
    fn test_benchmark_failure_ledger_merges_gate_and_parity_sources() {
        let mut workload = mock_workload_result("filtered_grouped_agg", 10_000, 1.48, 0.56);
        workload.category = "mixed".to_owned();
        workload.kernel_class = "hash_agg".to_owned();
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 10;
        workload.accel_output_rows_consumed = 51;
        workload.rows_returned_to_cpu = 51;
        workload.gpu_resident_pipeline = GpuResidentPipelineStatus::Reported;
        workload.plan_snippet = Some(
            "Custom Scan (pg_accel)\n\
             GPU Resident Operator Class: resident_groupagg\n\
             GPU Resident GroupAgg Key: dept\n\
             GPU Resident GroupAgg Measure: salary\n\
             GPU Resident GroupAgg Filter: active\n\
             GPU Resident GroupAgg Predicate Guard: boolean_filter\n\
             GPU Resident GroupAgg Value Predicate: aggregate_filter\n\
             GPU Resident GroupAgg Predicate IR: filter(active)\n\
             GPU Resident GroupAgg Aggregate Mask: 7\n"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        let ledger = report.benchmark_failure_ledger();

        assert_eq!(
            ledger.schema_version,
            BENCHMARK_FAILURE_LEDGER_SCHEMA_VERSION
        );
        assert_eq!(ledger.rows_below_parity_count, 1);
        assert_eq!(ledger.gpu_dispatched_below_parity_count, 1);
        assert_eq!(ledger.unique_work_items, 1);
        let row = &ledger.rows[0];
        assert_eq!(row.workload, "filtered_grouped_agg");
        assert_eq!(row.priority, "p1_release_blocker");
        assert_eq!(row.next_track, "resident_groupagg_small_input_overhead");
        assert!(row.sources.contains(&"benchmark_ship_gate".to_owned()));
        assert!(
            row.sources
                .contains(&"gpu_dispatched_below_parity".to_owned())
        );
        assert!(row.details.iter().any(|detail| {
            detail.contains("expected_winner_below_threshold")
                || detail.contains("credited GPU dispatch below PG-parallel parity")
        }));
        assert!(ledger.to_markdown().contains("Benchmark Failure Ledger"));
    }

    #[test]
    fn test_benchmark_failure_ledger_preserves_crash_inventory_artifacts() {
        let mut report = mock_report(Vec::new());
        report.crashes.push(CrashedScale {
            workload: "gpu_hashjoin_large_build".to_owned(),
            rows: 100_000,
            error: "backend disconnected".to_owned(),
            repro_command: Some(
                "cargo run -p pg_accel_bench -- crash-repro --workload gpu_hashjoin_large_build \
                 --rows 100000"
                    .to_owned(),
            ),
            plan_snippet_artifact: Some(
                "plan_snippets/gpu_hashjoin_large_build-100000.txt".to_owned(),
            ),
            correctness_diff_artifact: Some(
                "correctness_diffs/gpu_hashjoin_large_build-100000.json".to_owned(),
            ),
            log_tail_artifacts: vec![
                "log_tails/postgres-gpu_hashjoin_large_build-100000.log".to_owned(),
            ],
        });

        let ledger = report.benchmark_failure_ledger();

        assert_eq!(ledger.crash_count, 1);
        assert_eq!(ledger.ship_gate_failure_count, 1);
        assert_eq!(ledger.unique_work_items, 1);
        let row = &ledger.rows[0];
        assert_eq!(row.classification, "crash_or_missing_result");
        assert!(row.sources.contains(&"benchmark_ship_gate".to_owned()));
        assert!(row.sources.contains(&"crash_inventory".to_owned()));
        assert_eq!(
            row.plan_snippet_artifact.as_deref(),
            Some("plan_snippets/gpu_hashjoin_large_build-100000.txt")
        );
        assert_eq!(
            row.correctness_diff_artifact.as_deref(),
            Some("correctness_diffs/gpu_hashjoin_large_build-100000.json")
        );
        assert_eq!(
            row.log_tail_artifacts,
            vec!["log_tails/postgres-gpu_hashjoin_large_build-100000.log".to_owned()]
        );
        assert!(
            row.repro_command
                .as_deref()
                .is_some_and(|cmd| cmd.contains("crash-repro"))
        );
        let md = ledger.to_markdown();
        assert!(md.contains("Crash artifacts"));
        assert!(md.contains("plan_snippets/gpu_hashjoin_large_build-100000.txt"));
        assert!(md.contains("correctness_diffs/gpu_hashjoin_large_build-100000.json"));
        assert!(md.contains("log_tails/postgres-gpu_hashjoin_large_build-100000.log"));
        assert!(md.contains("crash-repro"));
    }

    #[test]
    fn test_csv_header_row() {
        let report = mock_report(vec![mock_workload_result("csv_wl", 100_000, 10.0, 20.0)]);
        let csv = report.to_csv();
        let lines: Vec<&str> = csv.lines().collect();
        assert!(
            lines[0]
                .starts_with("workload,category,kernel_class,plan_selected,gpu_kernel_dispatched,")
        );
        assert!(lines[0].contains("no_dispatch_timing_skew"));
        assert!(lines[0].contains("planner_declined"));
        assert!(lines[0].contains("function_kernel_count"));
        assert!(lines[0].contains("rows_returned_to_cpu"));
        assert!(lines[0].contains("gpu_resident_pipeline"));
        assert!(lines[0].contains("correctness_diff_artifact"));
        assert!(lines[0].contains("sanity_check_count"));
        assert!(lines[0].contains("sanity_check_failed"));
        assert!(lines[0].contains("accel_mean_ms"));
    }

    #[test]
    fn test_csv_includes_rows() {
        let report = mock_report(vec![
            mock_workload_result("wl", 1_000, 5.0, 10.0),
            mock_workload_result("wl", 1_000_000, 5.0, 10.0),
        ]);
        let csv = report.to_csv();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3); // header + 2 data rows
        assert!(lines[1].contains(",1000,"));
        assert!(lines[2].contains(",1000000,"));
    }

    #[test]
    fn test_csv_column_count_matches_header() {
        let report = mock_report(vec![mock_workload_result("col_test", 100_000, 10.0, 20.0)]);
        let csv = report.to_csv();
        let lines: Vec<&str> = csv.lines().collect();
        let header_cols = lines[0].split(',').count();
        let data_cols = lines[1].split(',').count();
        assert_eq!(header_cols, data_cols);
    }

    #[test]
    fn test_correctness_diff_artifact_renders_in_reports() {
        let mut workload = mock_workload_result("diff_artifact", 100_000, 10.0, 20.0);
        workload.correctness_diff_artifact =
            Some("correctness_diffs/diff-artifact-100000.json".to_owned());
        let report = mock_report(vec![workload]);

        let json = report.to_json().expect("json");
        let csv = report.to_csv();
        let markdown = report.to_markdown();

        assert!(json.contains("correctness_diffs/diff-artifact-100000.json"));
        assert!(csv.contains("correctness_diffs/diff-artifact-100000.json"));
        assert!(markdown.contains("correctness_diffs/diff-artifact-100000.json"));
    }

    #[test]
    fn test_generate_report_no_connection() {
        let workloads = vec![
            mock_workload_result("gen_test", 1_000, 10.0, 20.0),
            mock_workload_result("gen_test", 1_000_000, 10.0, 20.0),
        ];
        let report = generate_report(
            workloads,
            Vec::new(),
            None,
            30,
            5,
            None,
            crate::runner::TimingMode::RawWallClock,
            crate::runner::CacheMode::Warm,
        );
        assert!(report.hardware.is_some());
        assert!(report.gucs.is_none());
        assert_eq!(report.methodology.row_scales, vec![1_000, 1_000_000]);
        assert_eq!(report.workloads.len(), 2);
    }

    #[test]
    fn test_format_rows() {
        assert_eq!(format_rows(1_000), "1K");
        assert_eq!(format_rows(10_000), "10K");
        assert_eq!(format_rows(100_000), "100K");
        assert_eq!(format_rows(1_000_000), "1M");
    }
}
