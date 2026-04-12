use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::stats;

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
pub struct WorkloadResult {
    pub name: String,
    pub description: String,
    /// Workload category (`gpu_spatial`, `gpu_h3`, `ssbm`, etc.). Used for
    /// rollup / geomean grouping in the report.
    #[serde(default = "default_category")]
    pub category: String,
    /// Kernel class this workload exercises (`point_in_ring`, `reduce_f32`,
    /// `hashagg`, `h3_latlng`, etc.). Used by the Kernel Coverage table
    /// (action_items W11) to show that 127 workloads exercise only ~9
    /// distinct kernels.
    #[serde(default = "default_kernel_class")]
    pub kernel_class: String,
    /// Row count this result was measured at.
    pub rows: usize,
    pub iterations: Vec<IterationResult>,
    /// Whether this row was actually dispatched to a GPU Custom Scan path.
    ///
    /// Determined by inspecting the captured plan text for the Custom Scan
    /// node (or, once Fix Agent 6 lands `pg_accel_kernel_executions_delta`,
    /// by the counter delta during measurement). Rows where
    /// `dispatched == false` are excluded from per-category geomeans and
    /// reported in a separate "not dispatched" count.
    #[serde(default = "default_dispatched")]
    pub dispatched: bool,
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
    /// runner. Contains the first few lines of `EXPLAIN (ANALYZE, VERBOSE,
    /// BUFFERS)` — the portion with the Custom Scan node / GPU Dispatched
    /// tags. Used for the `dispatched` classification and for debugging.
    #[serde(default)]
    pub plan_snippet: Option<String>,
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
}

/// Full benchmark report containing results for all workloads at all scales.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    pub hardware: Option<HardwareProfile>,
    pub gucs: Option<GucSettings>,
    pub methodology: Methodology,
    pub workloads: Vec<WorkloadResult>,
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
    /// Source distribution for the headline speedup calculation
    /// (`median` / `mean`).
    #[serde(default)]
    pub speedup_source: String,
}

/// Default category used when deserializing legacy JSON reports that predate
/// the `category` field.
fn default_category() -> String {
    "gpu".to_owned()
}

/// Default kernel class for legacy JSON reports.
fn default_kernel_class() -> String {
    "unclassified".to_owned()
}

/// Default dispatched value for legacy JSON reports (assume true so old
/// archives still aggregate).
const fn default_dispatched() -> bool {
    true
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
    /// Build aggregated result from raw iterations (two-way: accel vs parallel).
    #[allow(dead_code)] // legacy shim; call sites migrated to from_iterations_ex
    pub fn from_iterations(
        name: String,
        description: String,
        category: String,
        rows: usize,
        iterations: Vec<IterationResult>,
    ) -> Self {
        Self::from_iterations_ex(
            name,
            description,
            category,
            classify_kernel("unclassified"),
            rows,
            iterations,
            true,
        )
    }

    /// Extended constructor used by the runner: populates kernel class,
    /// dispatch status, and all per-percentile statistics.
    #[allow(clippy::too_many_arguments)]
    pub fn from_iterations_ex(
        name: String,
        description: String,
        category: String,
        kernel_class: String,
        rows: usize,
        iterations: Vec<IterationResult>,
        dispatched: bool,
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
            dispatched,
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
            thermal: None,
            table_stats: Vec::new(),
            iterations,
        }
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

impl BenchReport {
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

        // -------------------------------------------------------------------
        // Kernel coverage table (action_items W11 / Reviewer 1 Sin #17)
        // -------------------------------------------------------------------
        //
        // Reviewer 1 counts at most 9 distinct GPU kernels across the
        // entire suite; the category table inflates the count because the
        // same `point_in_ring` kernel appears under 4 different category
        // labels. This table collapses everything to kernel class.
        self.render_kernel_coverage(&mut out);

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
        // produced a result. The headline rate uses speedup-from-median or
        // speedup-from-mean as configured.
        let use_median = self.methodology.speedup_source == "median";
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
                    let sp = if use_median {
                        w.speedup_median_vs_parallel
                    } else {
                        w.speedup_vs_parallel
                    };
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
            let _ = writeln!(
                out,
                "|---|---|---|---|---|---|---|---|---|"
            );
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
        // action_items M: prefer the explicit `dispatched` flag (set from
        // captured plan text) when available; fall back to the speedup
        // heuristic for legacy result archives where the field is missing.
        let non_dispatch: Vec<&WorkloadResult> = self
            .workloads
            .iter()
            .filter(|w| !w.dispatched || (w.speedup_vs_parallel - 1.0).abs() < 0.02)
            .collect();
        if !non_dispatch.is_empty() {
            out.push_str("## Non-Dispatching Workloads\n\n");
            out.push_str(
                "Workloads where `|speedup − 1| < 0.02`. pg_accel almost certainly did not \
                 dispatch a GPU path for these — check `benchmarks/plans.txt` (or run with \
                 `--capture-plans`) to confirm whether a Custom Scan node appears in the \
                 plan. If it does not, the planner hook is declining the path.\n\n",
            );
            let _ = writeln!(
                out,
                "| Workload | Scale | Speedup | Accel (ms) | PG Parallel (ms) |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|");
            for w in &non_dispatch {
                let _ = writeln!(
                    out,
                    "| {} | {} | {:.2}x | {:.2} | {:.2} |",
                    w.name,
                    format_rows(w.rows),
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
                 backend and were excluded from results.\n\n",
            );
            let _ = writeln!(out, "| Workload | Scale | Error |");
            let _ = writeln!(out, "|----------|-------|-------|");
            for c in &self.crashes {
                let short_err = if c.error.len() > 80 {
                    format!("{}...", &c.error[..77])
                } else {
                    c.error.clone()
                };
                let _ = writeln!(
                    out,
                    "| {} | {} | {} |",
                    c.workload,
                    format_rows(c.rows),
                    short_err,
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
        serde_json::to_string_pretty(self)
    }

    /// Render the report as CSV (one row per workload per scale, two-way).
    #[must_use]
    pub fn to_csv(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "workload,category,kernel_class,dispatched,rows,\
             accel_mean_ms,accel_stddev_ms,accel_median_ms,accel_p25_ms,accel_p75_ms,accel_p95_ms,\
             accel_cv_pct,accel_min_ms,accel_max_ms,\
             parallel_mean_ms,parallel_stddev_ms,parallel_median_ms,parallel_p25_ms,parallel_p75_ms,\
             parallel_p95_ms,parallel_cv_pct,parallel_min_ms,parallel_max_ms,\
             speedup_vs_parallel,speedup_median_vs_parallel,p_value_vs_parallel,\
             cohens_d_vs_parallel,effect_size_meaningful,cv_ratio,significant\n",
        );
        for w in &self.workloads {
            let sig = if w.p_value_vs_parallel < 0.01 {
                "yes"
            } else if w.p_value_vs_parallel < 0.05 {
                "marginal"
            } else {
                "no"
            };
            let _ = writeln!(
                out,
                "{},{},{},{},{},\
                 {:.4},{:.4},{:.4},{:.4},{:.4},{:.4},\
                 {:.4},{:.4},{:.4},\
                 {:.4},{:.4},{:.4},{:.4},{:.4},\
                 {:.4},{:.4},{:.4},{:.4},\
                 {:.4},{:.4},{:.6e},\
                 {:.4},{},{:.4},{}",
                w.name,
                w.category,
                w.kernel_class,
                w.dispatched,
                w.rows,
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
    ///   - Workloads where `dispatched == false` are excluded from
    ///     per-category geomeans (action_items M)
    fn render_geomean_headline(&self, out: &mut String) {
        // Family size for Bonferroni = attempted scales = workloads + crashes.
        // This is the same value used by the detail tables so the two views
        // never disagree.
        let family_size = self.workloads.len() + self.crashes.len();
        let speedup_label = if self.methodology.speedup_source == "median" {
            "median speedup"
        } else {
            "mean speedup"
        };
        let speedup_of = |w: &WorkloadResult| -> f64 {
            if self.methodology.speedup_source == "median" {
                w.speedup_median_vs_parallel
            } else {
                w.speedup_vs_parallel
            }
        };

        // ----- Overall (dispatched only) -----
        let dispatched: Vec<&WorkloadResult> =
            self.workloads.iter().filter(|w| w.dispatched).collect();
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
             (geomean across {n} dispatched workloads, family size = {family_size}).",
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
        if !self.crashes.is_empty() {
            let _ = writeln!(
                out,
                ">\n> {n_crash} scale(s) crashed and are counted in the Bonferroni \
                 family size but not in the geomean.",
                n_crash = self.crashes.len(),
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
            "| **overall (dispatched)** | **{}** | **{overall_gm:.2}x** | **{}** | **{}** | **{}** | **{}** |",
            dispatched.len(),
            overall_counts.sig_wins,
            overall_counts.sig_losses,
            overall_counts.total_sig,
            overall_counts.not_significant,
        );
        out.push('\n');

        // ----- CRASH summary rows (action_items L) -----
        if !self.crashes.is_empty() {
            out.push_str("### Crashed scales\n\n");
            let _ = writeln!(out, "| Workload | Scale | Error |");
            let _ = writeln!(out, "|---|---|---|");
            for c in &self.crashes {
                let short = if c.error.len() > 80 {
                    format!("{}...", &c.error[..77])
                } else {
                    c.error.clone()
                };
                let _ = writeln!(
                    out,
                    "| {} | {} | CRASH: {} |",
                    c.workload,
                    format_rows(c.rows),
                    short,
                );
            }
            out.push('\n');
        }
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
        out.push_str("## Kernel Coverage\n\n");
        out.push_str(
            "Workloads grouped by the GPU kernel class they exercise. \
             A high workload count under a single kernel class means lots \
             of redundant variations of the same code path. Use this table \
             when adding new tests — prefer kernels with low coverage.\n\n",
        );
        let family_size = self.workloads.len() + self.crashes.len();
        let speedup_of = |w: &WorkloadResult| -> f64 {
            if self.methodology.speedup_source == "median" {
                w.speedup_median_vs_parallel
            } else {
                w.speedup_vs_parallel
            }
        };

        // Group dispatched workloads by kernel_class.
        let mut by_kernel: BTreeMap<String, Vec<&WorkloadResult>> = BTreeMap::new();
        for w in &self.workloads {
            if !w.dispatched {
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
            let mut scales: std::collections::BTreeSet<usize> =
                std::collections::BTreeSet::new();
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
}

/// Generate a full report from workload results, with hardware and GUC info.
///
/// # Errors
///
/// Returns an error if GUC detection fails (non-fatal: report still generated
/// without GUCs).
#[allow(dead_code)] // legacy shim; call sites now use generate_report_ex directly
pub fn generate_report(
    workloads: Vec<WorkloadResult>,
    crashes: Vec<CrashedScale>,
    connection: Option<&str>,
    iterations: usize,
    warmup: usize,
) -> BenchReport {
    generate_report_ex(
        workloads,
        crashes,
        connection,
        iterations,
        warmup,
        None,
        crate::runner::TimingMode::RawWallClock,
        crate::runner::CacheMode::Warm,
        crate::runner::SpeedupSource::Median,
    )
}

/// Extended entrypoint used by `run_all_with_config` — carries the
/// observed-GUC snapshot, timing mode, cache mode, and speedup source
/// through into the report so the renderer can label the columns
/// correctly (action_items C4 / M1 / M2 / M12).
#[allow(
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    clippy::option_if_let_else
)]
pub fn generate_report_ex(
    workloads: Vec<WorkloadResult>,
    crashes: Vec<CrashedScale>,
    connection: Option<&str>,
    iterations: usize,
    warmup: usize,
    observed: Option<crate::runner::ObservedGucs>,
    timing_mode: crate::runner::TimingMode,
    cache_mode: crate::runner::CacheMode,
    speedup_source: crate::runner::SpeedupSource,
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
    let postmaster_start_time = observed.as_ref().and_then(|s| s.postmaster_start_time.clone());

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
    let speedup_label = match speedup_source {
        crate::runner::SpeedupSource::Median => "median",
        crate::runner::SpeedupSource::Mean => "mean",
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
        speedup_source: speedup_label.to_owned(),
    };
    BenchReport {
        hardware,
        gucs,
        methodology,
        workloads,
        crashes,
        postmaster_start_time,
    }
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
            rows,
            iterations,
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
                speedup_source: "median".to_owned(),
            },
            workloads,
            crashes: Vec::new(),
        }
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
            1000,
            iters,
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
        // Must NOT contain single-threaded fields
        assert!(!json_str.contains("\"single_mean_ms\""));
    }

    #[test]
    fn test_csv_header_row() {
        let report = mock_report(vec![mock_workload_result("csv_wl", 100_000, 10.0, 20.0)]);
        let csv = report.to_csv();
        let lines: Vec<&str> = csv.lines().collect();
        assert!(lines[0].starts_with("workload,category,kernel_class,dispatched,rows,"));
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
        let report = generate_report(workloads, Vec::new(), None, 30, 5);
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
