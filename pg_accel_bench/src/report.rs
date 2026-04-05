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
    /// Row count this result was measured at.
    pub rows: usize,
    pub iterations: Vec<IterationResult>,
    // -- pg_accel stats --
    pub accel_mean_ms: f64,
    pub accel_stddev_ms: f64,
    pub accel_median_ms: f64,
    pub accel_ci_95: (f64, f64),
    pub accel_outliers: Vec<usize>,
    pub accel_min_ms: f64,
    pub accel_max_ms: f64,
    // -- PG parallel stats --
    pub parallel_mean_ms: f64,
    pub parallel_stddev_ms: f64,
    pub parallel_median_ms: f64,
    pub parallel_ci_95: (f64, f64),
    pub parallel_outliers: Vec<usize>,
    pub parallel_min_ms: f64,
    pub parallel_max_ms: f64,
    // -- Derived --
    /// `parallel_mean / accel_mean`. Values > 1 mean pg_accel is faster than PG parallel.
    pub speedup_vs_parallel: f64,
    /// Paired t-test p-value: accel vs parallel.
    pub p_value_vs_parallel: f64,
    /// Cohen's d: accel vs parallel.
    pub cohens_d_vs_parallel: f64,
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
}

/// Methodology metadata for reproducibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Methodology {
    pub iterations: usize,
    pub warmup: usize,
    pub row_scales: Vec<usize>,
    pub ordering: String,
    pub statistical_tests: Vec<String>,
}

impl WorkloadResult {
    /// Build aggregated result from raw iterations (two-way: accel vs parallel).
    pub fn from_iterations(
        name: String,
        description: String,
        rows: usize,
        iterations: Vec<IterationResult>,
    ) -> Self {
        let accel_times: Vec<f64> = iterations.iter().map(|i| i.accel_ms).collect();
        let parallel_times: Vec<f64> = iterations.iter().map(|i| i.parallel_ms).collect();

        let accel_mean = stats::mean(&accel_times);
        let parallel_mean = stats::mean(&parallel_times);

        let speedup_vs_parallel = if accel_mean > 0.0 {
            parallel_mean / accel_mean
        } else {
            f64::NAN
        };

        Self {
            name,
            description,
            rows,
            accel_mean_ms: accel_mean,
            accel_stddev_ms: stats::stddev(&accel_times),
            accel_median_ms: stats::median(&accel_times),
            accel_ci_95: stats::confidence_interval_95(&accel_times),
            accel_outliers: stats::detect_outliers(&accel_times, 3.0),
            accel_min_ms: stats::min(&accel_times),
            accel_max_ms: stats::max(&accel_times),
            parallel_mean_ms: parallel_mean,
            parallel_stddev_ms: stats::stddev(&parallel_times),
            parallel_median_ms: stats::median(&parallel_times),
            parallel_ci_95: stats::confidence_interval_95(&parallel_times),
            parallel_outliers: stats::detect_outliers(&parallel_times, 3.0),
            parallel_min_ms: stats::min(&parallel_times),
            parallel_max_ms: stats::max(&parallel_times),
            speedup_vs_parallel,
            p_value_vs_parallel: stats::paired_t_test_p(&accel_times, &parallel_times),
            cohens_d_vs_parallel: stats::cohens_d(&parallel_times, &accel_times),
            iterations,
        }
    }
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

        // GUC settings
        if let Some(gucs) = &self.gucs
            && !gucs.settings.is_empty()
        {
            out.push_str("## PostgreSQL Settings\n\n");
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
        let scales_str: Vec<String> = self.methodology.row_scales.iter().map(|r| format_rows(*r)).collect();
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

        // Data rows
        for name in &workload_names {
            let _ = write!(out, "| {name} |");
            for &s in scales {
                if let Some(w) = lookup.get(&(name.as_str(), s)) {
                    let sp = w.speedup_vs_parallel;
                    let sig = w.p_value_vs_parallel < 0.05;
                    if sig && sp > 1.005 {
                        let _ = write!(out, " **{sp:.2}x** |");
                    } else if sig && sp < 0.995 {
                        let _ = write!(out, " {sp:.2}x |");
                    } else {
                        let _ = write!(out, " {sp:.2}x |");
                    }
                } else if crash_lookup.contains(&(name.clone(), s)) {
                    out.push_str(" crash |");
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

            // Per-scale table
            let _ = writeln!(out, "| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |");
            let _ = writeln!(out, "|-------|------------|-------------------|---------|-------------|");
            for &s in scales {
                if let Some(w) = lookup.get(&(name.as_str(), s)) {
                    let sig = if w.p_value_vs_parallel < 0.01 {
                        "YES"
                    } else if w.p_value_vs_parallel < 0.05 {
                        "marginal"
                    } else {
                        "no"
                    };
                    let _ = writeln!(
                        out,
                        "| {} | {:.2} +/- {:.2} | {:.2} +/- {:.2} | **{:.2}x** | {} |",
                        format_rows(s),
                        w.accel_mean_ms,
                        w.accel_stddev_ms,
                        w.parallel_mean_ms,
                        w.parallel_stddev_ms,
                        w.speedup_vs_parallel,
                        sig,
                    );
                }
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
            "workload,rows,\
             accel_mean_ms,accel_stddev_ms,accel_median_ms,accel_min_ms,accel_max_ms,\
             parallel_mean_ms,parallel_stddev_ms,parallel_median_ms,parallel_min_ms,parallel_max_ms,\
             speedup_vs_parallel,p_value_vs_parallel,cohens_d_vs_parallel,significant\n",
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
                "{},{},{:.4},{:.4},{:.4},{:.4},{:.4},\
                 {:.4},{:.4},{:.4},{:.4},{:.4},\
                 {:.4},{:.6},{:.4},{}",
                w.name,
                w.rows,
                w.accel_mean_ms,
                w.accel_stddev_ms,
                w.accel_median_ms,
                w.accel_min_ms,
                w.accel_max_ms,
                w.parallel_mean_ms,
                w.parallel_stddev_ms,
                w.parallel_median_ms,
                w.parallel_min_ms,
                w.parallel_max_ms,
                w.speedup_vs_parallel,
                w.p_value_vs_parallel,
                w.cohens_d_vs_parallel,
                sig,
            );
        }
        out
    }
}

/// Generate a full report from workload results, with hardware and GUC info.
///
/// # Errors
///
/// Returns an error if GUC detection fails (non-fatal: report still generated
/// without GUCs).
pub fn generate_report(
    workloads: Vec<WorkloadResult>,
    crashes: Vec<CrashedScale>,
    connection: Option<&str>,
    iterations: usize,
    warmup: usize,
) -> BenchReport {
    let hardware = Some(HardwareProfile::detect());
    let gucs = connection.and_then(|c| GucSettings::from_connection(c).ok());

    // Collect unique row scales from results (preserving order).
    let mut row_scales: Vec<usize> = Vec::new();
    for w in &workloads {
        if !row_scales.contains(&w.rows) {
            row_scales.push(w.rows);
        }
    }
    row_scales.sort_unstable();

    let methodology = Methodology {
        iterations,
        warmup,
        row_scales,
        ordering: "randomized per iteration (accel-first vs baseline-first)".to_owned(),
        statistical_tests: vec![
            "Paired t-test (two-tailed, p < 0.05)".to_owned(),
            "Cohen's d effect size".to_owned(),
            "95% CI via t-distribution".to_owned(),
            "Outlier detection (> 3 sigma)".to_owned(),
        ],
    };
    BenchReport {
        hardware,
        gucs,
        methodology,
        workloads,
        crashes,
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

    fn mock_workload_result(name: &str, rows: usize, accel_ms: f64, baseline_ms: f64) -> WorkloadResult {
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
            methodology: Methodology {
                iterations: 30,
                warmup: 5,
                row_scales: vec![1_000, 10_000, 100_000, 1_000_000],
                ordering: "randomized".to_owned(),
                statistical_tests: vec!["Paired t-test".to_owned()],
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
        let result = WorkloadResult::from_iterations("one".to_owned(), "desc".to_owned(), 1000, iters);
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
        assert!(lines[0].starts_with("workload,rows,"));
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
