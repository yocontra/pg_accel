use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::stats;

const NO_DISPATCH_TIMING_SKEW_THRESHOLD: f64 = 0.10;
const FUNCTION_SRF_GPU_FUNCTIONS: &[&str] = &[
    "h3_latlng_to_cell",
    "h3_grid_disk",
    "h3_grid_ring_unsafe",
    "h3_polyfill",
    "h3_cell_to_children",
    "h3_cell_to_boundary",
    "h3_cells_to_multi_polygon",
];

/// Timing results for a single iteration (two-way: accel vs PG parallel).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)]
pub struct IterationResult {
    /// Execution time in milliseconds with pg_accel enabled.
    pub accel_ms: f64,
    /// Execution time in milliseconds with PG parallel workers (pg_accel off).
    pub parallel_ms: f64,
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
    /// Thermal state captured immediately before this workload ran.
    /// `None` on platforms where capture is unavailable.
    #[serde(default)]
    pub thermal: Option<ThermalState>,
    /// `pg_class`/`pg_stats` diagnostics captured after VACUUM ANALYZE
    /// (action_items C6 / Reviewer 2 §3(iii)). Proves the parallel
    /// baseline's planner had fresh statistics.
    #[serde(default)]
    pub table_stats: Vec<TableStats>,
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
        let accel_times: Vec<f64> = iterations.iter().map(|i| i.accel_ms).collect();
        let parallel_times: Vec<f64> = iterations.iter().map(|i| i.parallel_ms).collect();

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
            thermal: None,
            table_stats: Vec::new(),
            iterations,
        }
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
        // h3 latlng / distance / parent (all same trig/int kernel family)
        ("h3_", "h3_latlng"),
        // reduce kernels
        ("gpu_reduce", "reduce"),
        ("reduce_sum_f32", "reduce"),
        ("reduce_sum_f64", "reduce"),
        ("reduce_sum_i64", "reduce"),
        ("reduce_min_", "reduce"),
        ("reduce_max_", "reduce"),
        ("reduce_multi", "reduce"),
        // hash aggregation
        ("gpu_hashagg", "hash_agg"),
        ("hashagg_", "hash_agg"),
        ("grouped_agg", "hash_agg"),
        ("filtered_grouped_agg", "hash_agg"),
        // sort
        ("large_sort", "sort"),
        ("gpu_sort", "sort"),
        ("sort_int", "sort"),
        ("sort_float", "sort"),
        ("spatial_sort", "sort"),
        ("topk_wide", "sort"),
        // hash join
        ("gpu_hashjoin", "hash_join"),
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
        ("mixed_join", "hash_join"),
        ("mixed_spatial_sort", "sort"),
        ("spatial_agg", "hash_agg"),
        ("oltp_point", "point_in_ring"),
        ("small_table", "unclassified"),
        // SSBM — multi-kernel star-schema queries; track as its own class
        ("ssbm_", "ssbm"),
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
            "pg_accel.workers",
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
    let counter_capture_known = dispatch_counter_capture_known(w);
    let counter_proves_kernel = w.gpu_kernel_execution_delta > 0;
    let function_candidate = known_function_srf_kernel_workload(w);
    let output_consumed = w.accel_output_rows_consumed > 0;
    let function_srf_kernel_dispatched = function_candidate
        && !plan_selected
        && counter_capture_known
        && counter_proves_kernel
        && output_consumed
        && w.pg_accel_stock_exec_delta == 0;

    let custom_scan_gpu_dispatched = counter_capture_known
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
    if plan_contains_gpu_resident_pipeline_evidence(w.plan_snippet.as_deref().unwrap_or_default())
        || full_plan.is_some_and(plan_contains_gpu_resident_pipeline_evidence)
    {
        return GpuResidentPipelineStatus::Reported;
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
        || category == "gpu_h3"
        || kernel.starts_with("h3")
        || description.to_ascii_lowercase().contains("h3_")
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

fn plan_contains_gpu_resident_pipeline_evidence(plan: &str) -> bool {
    let lower = plan.to_ascii_lowercase();
    lower.contains("strategy: gpupreagg")
        || lower.contains("gpuaccelpreagg")
        || lower.contains("fact rows scanned")
        || lower.contains("has scan expr")
        || lower.contains("pipeline fusion")
        || lower.contains("gpu-resident")
        || lower.contains("gpu resident")
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

impl BenchReport {
    fn with_normalized_dispatch(&self) -> Self {
        let mut report = self.clone();
        let full_plans = report.full_plan_artifact_text();
        for w in &mut report.workloads {
            let full_plan = full_plans
                .as_deref()
                .and_then(|plans| extract_full_plan_block(plans, &w.name, w.rows));
            let classification = w.dispatch_classification_with_plan_artifact(full_plan.as_deref());
            w.apply_dispatch_classification(classification);
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
        // H3 lane gate (TODO Phase 5 H3 winning lane protection)
        // -------------------------------------------------------------------
        //
        // Renders the `### H3 Lane Gate Failures` section when any H3 Winner
        // regressed below the uniform 1.0x floor (or failed to dispatch) or
        // any H3 Parity workload dispatched a GPU kernel. When the gate is
        // green this is a no-op; when it fails, the bench process is expected
        // to exit non-zero (see `main::cmd_run`).
        self.render_h3_lane_gate(&mut out);

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
                 PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|");
            for &s in scales {
                if let Some(w) = lookup.get(&(name.as_str(), s)) {
                    let adj_p = stats::bonferroni_adjusted_p(w.p_value_vs_parallel, n_tests);
                    let bonf_sig = adj_p.is_finite() && adj_p < 0.05;
                    let verdict = if bonf_sig && w.effect_size_meaningful {
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
                    let _ = writeln!(
                        out,
                        "| {} | {:.2}{} | {:.2}–{:.2} (p95 {:.2}) | {:.2}{} | \
                         {:.2}–{:.2} (p95 {:.2}) | **{:.2}x** | {:.2} | {} | {} |",
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
                    );
                }
            }
            out.push('\n');
        }

        // -------------------------------------------------------------------
        // Regressions: workloads significantly slower than PG parallel.
        // -------------------------------------------------------------------
        //
        // A workload makes the list only if it's BOTH slower by >10% AND
        // statistically significant after Bonferroni correction. Sorted by
        // absolute slowdown (worst first).
        let mut regressions: Vec<&WorkloadResult> = self
            .workloads
            .iter()
            .filter(|w| {
                let adj_p = stats::bonferroni_adjusted_p(w.p_value_vs_parallel, n_tests);
                w.speedup_vs_parallel < 0.90
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
                 parallel (>10% slowdown, Bonferroni-corrected p < 0.05). These are bugs to \
                 investigate, not tuning targets.\n\n",
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
                "| Workload | Scale | Error | Plan Snippet | Log Tails | Repro |"
            );
            let _ = writeln!(
                out,
                "|----------|-------|-------|--------------|-----------|-------|"
            );
            for c in &self.crashes {
                let short_err = if c.error.len() > 80 {
                    format!("{}...", &c.error[..77])
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
                    "| {} | {} | {} | {} | {} | `{}` |",
                    c.workload,
                    format_rows(c.rows),
                    short_err,
                    plan,
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
             accel_output_rows_consumed,rows,\
             baseline_dispatched,no_dispatch_timing_skew,no_dispatch_plan_mismatch,\
             accel_mean_ms,accel_stddev_ms,accel_median_ms,accel_p25_ms,accel_p75_ms,accel_p95_ms,\
             accel_cv_pct,accel_min_ms,accel_max_ms,\
             parallel_mean_ms,parallel_stddev_ms,parallel_median_ms,parallel_p25_ms,parallel_p75_ms,\
             parallel_p95_ms,parallel_cv_pct,parallel_min_ms,parallel_max_ms,\
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
            let _ = writeln!(
                out,
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},\
                 {},{},{},{},{},{},{},{},\
                 {:.4},{:.4},{:.4},{:.4},{:.4},{:.4},\
                 {:.4},{:.4},{:.4},\
                 {:.4},{:.4},{:.4},{:.4},{:.4},\
                 {:.4},{:.4},{:.4},{:.4},\
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

        // ----- CRASH summary rows (action_items L) -----
        if !report.crashes.is_empty() {
            out.push_str("### Crashed scales\n\n");
            let _ = writeln!(
                out,
                "| Workload | Scale | Error | Plan Snippet | Log Tails | Repro |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|---|");
            for c in &report.crashes {
                let short = if c.error.len() > 80 {
                    format!("{}...", &c.error[..77])
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
            .filter(|w| {
                w.plan_selected
                    && w.gpu_kernel_dispatched
                    && w.gpu_resident_pipeline == GpuResidentPipelineStatus::NotReported
            })
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
            "| GPU-dispatched Custom Scan without resident-pipeline evidence | \
             {gpu_resident_not_reported} |"
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
             GPU-resident pipeline | Kernel delta | Rows dispatched | GPU rows processed | Stock fallback |"
        );
        let _ = writeln!(out, "|---|---|---|---:|---:|---|---:|---:|---:|---:|");
        for w in &report.workloads {
            let classification = dispatch_evidence_label(w);
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                markdown_cell(&w.name),
                format_rows(w.rows),
                classification,
                w.function_kernel_count,
                w.rows_returned_to_cpu,
                w.gpu_resident_pipeline.as_str(),
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
            ">\n> Action item: delete or quarantine any pg_accel CPU fallback exposed by \
             these rows. If pg_accel cannot GPU accelerate the query, normal PostgreSQL \
             planning must decline the pg_accel path and let PostgreSQL use its native \
             plan.\n\n",
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
                "| {} | {} | {:.2}x | {} | {} | `{}` | `{}` | quarantine CPU fallback / \
                 planner-decline |",
                markdown_cell(&w.name),
                format_rows(w.rows),
                speedup,
                timing,
                plan_shape,
                markdown_cell(&first_plan_signature_line(w.plan_snippet.as_deref())),
                markdown_cell(&first_plan_signature_line(
                    w.baseline_plan_snippet.as_deref(),
                )),
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
    /// [`crate::workloads::h3_lane_class`] (the source of truth for H3 lane
    /// classification):
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
            match class {
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
                        // Parity rows have no per-Winner advisory; sentinel
                        // value `1.0` is rendered as `n/a` in the report but
                        // kept f64 for table-column homogeneity.
                        let advisory =
                            crate::workloads::h3_winner_min_warm_speedup(&w.name).unwrap_or(1.0);
                        failures.push(H3LaneGateFailure {
                            workload: w.name.clone(),
                            rows: w.rows,
                            kind: H3LaneGateFailureKind::ParityUnexpectedlyDispatched,
                            speedup_median: w.speedup_median_vs_parallel,
                            gate_floor: H3_LANE_GATE_MIN_WARM_SPEEDUP,
                            advisory_min_warm_speedup: advisory,
                        });
                    }
                }
            }
        }
        failures
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
            "Hard gate against the H3 lane classifier in \
             `pg_accel_bench/src/workloads/mod.rs::h3_lane_class`. Every \
             Winning lane must dispatch a GPU kernel and beat PG-parallel \
             parity; every Parity lane must stay native. A failure here \
             means the bench process exits non-zero — CI will fail.\n\n",
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
    std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(1)
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
            },
            workloads,
            artifact_dir: None,
            crashes: Vec::new(),
        }
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

    #[test]
    fn test_markdown_contains_header() {
        let report = mock_report(vec![mock_workload_result("wl1", 1000, 5.0, 15.0)]);
        let md = report.to_markdown();
        assert!(md.contains("# pg_accel Benchmark Report"));
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
        assert!(md.contains("quarantine CPU fallback"));
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
    fn test_h3_function_kernel_counts_as_gpu_dispatched() {
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
            "HashAggregate\n  Output: (h3_latlng_to_cell(geom, 7)), count(*)\n  \
             Group Key: h3_latlng_to_cell(bench_h3_points.geom, 7)\n"
                .to_owned(),
        );
        let report = mock_report(vec![workload]);

        let md = report.to_markdown();
        assert!(md.contains("geomean across 1 GPU-dispatched workloads"));
        assert!(md.contains("| Function/SRF kernel dispatched | 1 |"));
        assert!(!md.contains("## Non-Dispatching Workloads"));

        let csv = report.to_csv();
        assert!(csv.contains(
            "h3_bulk,gpu_h3,h3_latlng,false,true,true,false,false,1,10,\
             not_applicable_function_srf,true,1,0,0,0,0,10,100000"
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

    /// `h3_cell_to_parent` is a parity lane — pg_accel does not register it,
    /// so an h3_cell_to_parent workload in the bench should look like
    /// stock h3-pg on both sides (no Custom Scan, no kernel delta). The
    /// dispatch classifier must mark such a row as `planner_declined`.
    #[test]
    fn test_h3_cell_to_parent_parity_row_classified_as_planner_declined() {
        let mut workload = mock_workload_result("h3_cell_to_parent", 1_000_000, 10.0, 10.0);
        workload.category = "gpu_h3".to_owned();
        workload.kernel_class = "h3_cell_to_parent".to_owned();
        workload.plan_selected = false;
        workload.gpu_kernel_dispatched = false;
        workload.function_srf_kernel_dispatched = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 0;
        workload.accel_output_rows_consumed = 1;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet = Some(
            "HashAggregate\n  Output: (h3_cell_to_parent(cell, 4)), count(*)\n  \
             Group Key: h3_cell_to_parent(bench_h3_parent.cell, 4)\n"
                .to_owned(),
        );

        let classification = workload.dispatch_classification();
        assert!(
            !classification.function_srf_kernel_dispatched,
            "h3_cell_to_parent must NOT be credited as function/SRF dispatch — \
             it is a parity lane that must stay native. Got: {classification:?}"
        );
        assert!(
            !classification.gpu_kernel_dispatched,
            "h3_cell_to_parent must NOT be credited as GPU-dispatched"
        );
        assert!(
            classification.planner_declined,
            "h3_cell_to_parent native-only run must be marked planner_declined"
        );
    }

    /// If `h3_cell_to_parent` accidentally starts incrementing the kernel
    /// counter (e.g. an agent re-registers it in the h3 adapter), the row
    /// will look like a function/SRF dispatch in the report — that surfaces
    /// clearly in the dispatch classification because the report-side
    /// heuristic is purely name-prefix + counter-driven. The lane policy
    /// itself (parity vs winning) is enforced by `h3_lane_class()` in
    /// `pg_accel_bench/src/workloads/mod.rs`. This test pins what the bench
    /// report WILL show in that regression scenario so a future runner-side
    /// cross-check has a stable surface to assert against.
    #[test]
    fn test_h3_cell_to_parent_with_unexpected_kernel_delta_classifier_visible() {
        let mut workload = mock_workload_result("h3_cell_to_parent", 1_000_000, 10.0, 12.0);
        workload.category = "gpu_h3".to_owned();
        workload.kernel_class = "h3_cell_to_parent".to_owned();
        workload.plan_selected = false;
        workload.gpu_kernel_dispatched = false;
        workload.function_srf_kernel_dispatched = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet =
            Some("HashAggregate\n  Output: (h3_cell_to_parent(cell, 4)), count(*)\n".to_owned());

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
            crate::workloads::h3_lane_class("h3_cell_to_parent"),
            Some(crate::workloads::H3LaneClass::Parity),
            "h3_cell_to_parent must remain a parity lane; dispatch classifier crediting \
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

    /// `h3_cell_to_parent` is a canonical H3 Parity workload. If a row shows
    /// `gpu_kernel_dispatched=true` (a quarantine breach), the gate MUST
    /// produce a `ParityUnexpectedlyDispatched` failure.
    #[test]
    fn test_h3_parity_unexpectedly_dispatched_triggers_gate_fail() {
        let mut workload = mock_workload_result("h3_cell_to_parent", 1_000_000, 10.0, 12.0);
        workload.category = "gpu_h3".to_owned();
        workload.kernel_class = "h3_cell_to_parent".to_owned();
        // Quarantine breach: the parity-lane row fired a kernel.
        workload.plan_selected = false;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 10;
        workload.pg_accel_stock_exec_delta = 0;
        workload.plan_snippet =
            Some("HashAggregate\n  Output: (h3_cell_to_parent(cell, 4)), count(*)\n".to_owned());

        let report = mock_report(vec![workload]);
        let failures = report.evaluate_h3_lane_gate();
        assert_eq!(
            failures.len(),
            1,
            "parity-lane dispatch must produce exactly one gate failure; got: {failures:?}"
        );
        assert_eq!(failures[0].workload, "h3_cell_to_parent");
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
            wl.kernel_class = "h3_latlng".to_owned();
            wl.plan_selected = false;
            wl.dispatch_counter_captured = true;
            wl.gpu_kernel_execution_delta = 1;
            wl.accel_output_rows_consumed = 10;
            wl.pg_accel_stock_exec_delta = 0;
            wl.plan_snippet = Some(
                "HashAggregate\n  Output: (h3_latlng_to_cell(geom, 7)), count(*)\n  \
                 Group Key: h3_latlng_to_cell(bench_h3_points.geom, 7)\n"
                    .to_owned(),
            );
            workloads.push(wl);
        }
        // And an h3_cell_to_parent parity row that stays native (no kernel
        // delta) — the gate must NOT flag it.
        let mut parity = mock_workload_result("h3_cell_to_parent", 1_000_000, 10.0, 10.0);
        parity.category = "gpu_h3".to_owned();
        parity.kernel_class = "h3_cell_to_parent".to_owned();
        parity.plan_selected = false;
        parity.dispatch_counter_captured = true;
        parity.gpu_kernel_execution_delta = 0;
        parity.accel_output_rows_consumed = 1;
        parity.pg_accel_stock_exec_delta = 0;
        parity.plan_snippet =
            Some("HashAggregate\n  Output: (h3_cell_to_parent(cell, 4)), count(*)\n".to_owned());
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
    fn test_dispatch_classification_reports_gpu_resident_preagg_evidence() {
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
            GpuResidentPipelineStatus::Reported
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
        // Must NOT contain single-threaded fields
        assert!(!json_str.contains("\"single_mean_ms\""));
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
