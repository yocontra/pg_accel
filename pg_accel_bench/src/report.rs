use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::stats;

/// Timing results for a single iteration (three-way comparison).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)]
pub struct IterationResult {
    /// Execution time in milliseconds with pg_accel enabled.
    pub accel_ms: f64,
    /// Execution time in milliseconds with PG parallel workers (pg_accel off).
    pub parallel_ms: f64,
    /// Execution time in milliseconds single-threaded (pg_accel off, no parallel).
    pub single_ms: f64,
}

/// Aggregated results for one workload (three-way comparison).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadResult {
    pub name: String,
    pub description: String,
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
    // -- PG single-threaded stats --
    pub single_mean_ms: f64,
    pub single_stddev_ms: f64,
    pub single_median_ms: f64,
    pub single_ci_95: (f64, f64),
    pub single_outliers: Vec<usize>,
    pub single_min_ms: f64,
    pub single_max_ms: f64,
    // -- Derived --
    /// `single_mean / accel_mean`. Values > 1 mean pg_accel is faster than single-threaded.
    pub speedup_vs_single: f64,
    /// `parallel_mean / accel_mean`. Values > 1 mean pg_accel is faster than PG parallel.
    pub speedup_vs_parallel: f64,
    /// Paired t-test p-value: accel vs single-threaded.
    pub p_value_vs_single: f64,
    /// Paired t-test p-value: accel vs parallel.
    pub p_value_vs_parallel: f64,
    /// Cohen's d: accel vs single-threaded.
    pub cohens_d_vs_single: f64,
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

/// Full benchmark report containing results for all workloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    pub hardware: Option<HardwareProfile>,
    pub gucs: Option<GucSettings>,
    pub methodology: Methodology,
    pub workloads: Vec<WorkloadResult>,
}

/// Methodology metadata for reproducibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Methodology {
    pub iterations: usize,
    pub warmup: usize,
    pub rows: usize,
    pub ordering: String,
    pub statistical_tests: Vec<String>,
}

impl WorkloadResult {
    /// Build aggregated result from raw iterations (three-way).
    pub fn from_iterations(
        name: String,
        description: String,
        iterations: Vec<IterationResult>,
    ) -> Self {
        let accel_times: Vec<f64> = iterations.iter().map(|i| i.accel_ms).collect();
        let parallel_times: Vec<f64> = iterations.iter().map(|i| i.parallel_ms).collect();
        let single_times: Vec<f64> = iterations.iter().map(|i| i.single_ms).collect();

        let accel_mean = stats::mean(&accel_times);
        let parallel_mean = stats::mean(&parallel_times);
        let single_mean = stats::mean(&single_times);

        let speedup_vs_single = if accel_mean > 0.0 {
            single_mean / accel_mean
        } else {
            f64::NAN
        };
        let speedup_vs_parallel = if accel_mean > 0.0 {
            parallel_mean / accel_mean
        } else {
            f64::NAN
        };

        Self {
            name,
            description,
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
            single_mean_ms: single_mean,
            single_stddev_ms: stats::stddev(&single_times),
            single_median_ms: stats::median(&single_times),
            single_ci_95: stats::confidence_interval_95(&single_times),
            single_outliers: stats::detect_outliers(&single_times, 3.0),
            single_min_ms: stats::min(&single_times),
            single_max_ms: stats::max(&single_times),
            speedup_vs_single,
            speedup_vs_parallel,
            p_value_vs_single: stats::paired_t_test_p(&accel_times, &single_times),
            p_value_vs_parallel: stats::paired_t_test_p(&accel_times, &parallel_times),
            cohens_d_vs_single: stats::cohens_d(&single_times, &accel_times),
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

impl BenchReport {
    /// Render the report as a Markdown document.
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
        let _ = writeln!(out, "| Rows per table | {} |", self.methodology.rows);
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
        out.push_str("Plan caches are flushed between measurements via `DISCARD PLANS`.\n\n");

        // Results summary table (three-way)
        out.push_str("## Results\n\n");
        out.push_str(
            "| Workload | Accel (ms) | PG Parallel (ms) | PG Single (ms) | vs Single | vs Parallel | Sig? |\n",
        );
        out.push_str(
            "|----------|------------|-------------------|----------------|-----------|-------------|------|\n",
        );
        for w in &self.workloads {
            let sig = if w.p_value_vs_single < 0.01 {
                "YES"
            } else if w.p_value_vs_single < 0.05 {
                "marginal"
            } else {
                "no"
            };
            let _ = writeln!(
                out,
                "| {} | {:.2} +/- {:.2} | {:.2} +/- {:.2} | {:.2} +/- {:.2} | {:.2}x | {:.2}x | {} |",
                w.name,
                w.accel_mean_ms,
                w.accel_stddev_ms,
                w.parallel_mean_ms,
                w.parallel_stddev_ms,
                w.single_mean_ms,
                w.single_stddev_ms,
                w.speedup_vs_single,
                w.speedup_vs_parallel,
                sig,
            );
        }
        out.push('\n');

        // Detailed per-workload sections
        out.push_str("## Detailed Results\n\n");
        for w in &self.workloads {
            let _ = writeln!(out, "### {}\n", w.name);
            let _ = writeln!(out, "**Query:** {}\n", w.description);
            let _ = writeln!(out, "| Metric | Accel | PG Parallel | PG Single |");
            let _ = writeln!(out, "|--------|-------|-------------|-----------|");
            let _ = writeln!(
                out,
                "| Mean (ms) | {:.2} | {:.2} | {:.2} |",
                w.accel_mean_ms, w.parallel_mean_ms, w.single_mean_ms
            );
            let _ = writeln!(
                out,
                "| Median (ms) | {:.2} | {:.2} | {:.2} |",
                w.accel_median_ms, w.parallel_median_ms, w.single_median_ms
            );
            let _ = writeln!(
                out,
                "| Stddev (ms) | {:.2} | {:.2} | {:.2} |",
                w.accel_stddev_ms, w.parallel_stddev_ms, w.single_stddev_ms
            );
            let _ = writeln!(
                out,
                "| 95% CI (ms) | {:.2}..{:.2} | {:.2}..{:.2} | {:.2}..{:.2} |",
                w.accel_ci_95.0,
                w.accel_ci_95.1,
                w.parallel_ci_95.0,
                w.parallel_ci_95.1,
                w.single_ci_95.0,
                w.single_ci_95.1,
            );
            let _ = writeln!(
                out,
                "| Min (ms) | {:.2} | {:.2} | {:.2} |",
                w.accel_min_ms, w.parallel_min_ms, w.single_min_ms
            );
            let _ = writeln!(
                out,
                "| Max (ms) | {:.2} | {:.2} | {:.2} |",
                w.accel_max_ms, w.parallel_max_ms, w.single_max_ms
            );
            let _ = writeln!(
                out,
                "| Speedup vs single | {:.2}x | {:.2}x | 1.00x |",
                w.speedup_vs_single,
                if w.parallel_mean_ms > 0.0 {
                    w.single_mean_ms / w.parallel_mean_ms
                } else {
                    f64::NAN
                },
            );
            let _ = writeln!(
                out,
                "| p-value (vs single) | {:.6} | | |",
                w.p_value_vs_single
            );
            let _ = writeln!(
                out,
                "| p-value (vs parallel) | {:.6} | | |",
                w.p_value_vs_parallel
            );
            let _ = writeln!(
                out,
                "| Cohen's d (vs single) | {:.4} | | |",
                w.cohens_d_vs_single
            );
            let _ = writeln!(
                out,
                "| Cohen's d (vs parallel) | {:.4} | | |",
                w.cohens_d_vs_parallel
            );

            if !w.accel_outliers.is_empty() {
                let _ = writeln!(
                    out,
                    "\n**Accel outliers** (>3 sigma): iterations {:?}",
                    w.accel_outliers
                );
            }
            if !w.parallel_outliers.is_empty() {
                let _ = writeln!(
                    out,
                    "\n**Parallel outliers** (>3 sigma): iterations {:?}",
                    w.parallel_outliers
                );
            }
            if !w.single_outliers.is_empty() {
                let _ = writeln!(
                    out,
                    "\n**Single outliers** (>3 sigma): iterations {:?}",
                    w.single_outliers
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

    /// Render the report as CSV (one row per workload, three-way).
    #[must_use]
    pub fn to_csv(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "workload,\
             accel_mean_ms,accel_stddev_ms,accel_median_ms,accel_min_ms,accel_max_ms,\
             parallel_mean_ms,parallel_stddev_ms,parallel_median_ms,parallel_min_ms,parallel_max_ms,\
             single_mean_ms,single_stddev_ms,single_median_ms,single_min_ms,single_max_ms,\
             speedup_vs_single,speedup_vs_parallel,\
             p_value_vs_single,p_value_vs_parallel,\
             cohens_d_vs_single,cohens_d_vs_parallel,significant\n",
        );
        for w in &self.workloads {
            let sig = if w.p_value_vs_single < 0.01 {
                "yes"
            } else if w.p_value_vs_single < 0.05 {
                "marginal"
            } else {
                "no"
            };
            let _ = writeln!(
                out,
                "{},{:.4},{:.4},{:.4},{:.4},{:.4},\
                 {:.4},{:.4},{:.4},{:.4},{:.4},\
                 {:.4},{:.4},{:.4},{:.4},{:.4},\
                 {:.4},{:.4},{:.6},{:.6},{:.4},{:.4},{}",
                w.name,
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
                w.single_mean_ms,
                w.single_stddev_ms,
                w.single_median_ms,
                w.single_min_ms,
                w.single_max_ms,
                w.speedup_vs_single,
                w.speedup_vs_parallel,
                w.p_value_vs_single,
                w.p_value_vs_parallel,
                w.cohens_d_vs_single,
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
    connection: Option<&str>,
    iterations: usize,
    warmup: usize,
    rows: usize,
) -> BenchReport {
    let hardware = Some(HardwareProfile::detect());
    let gucs = connection.and_then(|c| GucSettings::from_connection(c).ok());
    let methodology = Methodology {
        iterations,
        warmup,
        rows,
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

    /// Build a simple mock `WorkloadResult` for testing report formatting.
    ///
    /// `single_ms` is set to `baseline_ms * 1.5` to simulate parallel being
    /// faster than single-threaded.
    fn mock_workload_result(name: &str, accel_ms: f64, baseline_ms: f64) -> WorkloadResult {
        let iterations: Vec<IterationResult> = (0..10)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let jitter = (i as f64) * 0.1;
                IterationResult {
                    accel_ms: accel_ms + jitter,
                    parallel_ms: baseline_ms + jitter,
                    single_ms: baseline_ms * 1.5 + jitter,
                }
            })
            .collect();
        WorkloadResult::from_iterations(
            name.to_owned(),
            format!("Mock workload: {name}"),
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
                rows: 100_000,
                ordering: "randomized".to_owned(),
                statistical_tests: vec!["Paired t-test".to_owned()],
            },
            workloads,
        }
    }

    // -----------------------------------------------------------------------
    // WorkloadResult::from_iterations
    // -----------------------------------------------------------------------

    #[test]
    fn test_workload_result_from_iterations_basic() {
        let result = mock_workload_result("test_wl", 10.0, 20.0);
        assert_eq!(result.name, "test_wl");
        assert!(
            result.speedup_vs_single > 1.0,
            "single>accel should give speedup>1"
        );
        assert!(
            result.speedup_vs_parallel > 1.0,
            "parallel>accel should give speedup>1"
        );
        assert_eq!(result.iterations.len(), 10);
    }

    #[test]
    fn test_workload_result_single_iteration() {
        let iters = vec![IterationResult {
            accel_ms: 5.0,
            parallel_ms: 10.0,
            single_ms: 20.0,
        }];
        let result = WorkloadResult::from_iterations("single".to_owned(), "desc".to_owned(), iters);
        assert!((result.accel_mean_ms - 5.0).abs() < f64::EPSILON);
        assert!((result.parallel_mean_ms - 10.0).abs() < f64::EPSILON);
        assert!((result.single_mean_ms - 20.0).abs() < f64::EPSILON);
        assert!((result.speedup_vs_single - 4.0).abs() < f64::EPSILON);
        assert!((result.speedup_vs_parallel - 2.0).abs() < f64::EPSILON);
        assert!((result.accel_stddev_ms - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_workload_result_identical_times() {
        let iters: Vec<IterationResult> = (0..5)
            .map(|_| IterationResult {
                accel_ms: 10.0,
                parallel_ms: 10.0,
                single_ms: 10.0,
            })
            .collect();
        let result =
            WorkloadResult::from_iterations("identical".to_owned(), "desc".to_owned(), iters);
        assert!((result.speedup_vs_single - 1.0).abs() < f64::EPSILON);
        assert!((result.speedup_vs_parallel - 1.0).abs() < f64::EPSILON);
        assert!((result.accel_stddev_ms).abs() < f64::EPSILON);
        assert!((result.parallel_stddev_ms).abs() < f64::EPSILON);
        assert!((result.single_stddev_ms).abs() < f64::EPSILON);
        assert!(result.accel_outliers.is_empty());
    }

    // -----------------------------------------------------------------------
    // Markdown output
    // -----------------------------------------------------------------------

    #[test]
    fn test_markdown_contains_header() {
        let report = mock_report(vec![mock_workload_result("wl1", 5.0, 15.0)]);
        let md = report.to_markdown();
        assert!(md.contains("# pg_accel Benchmark Report"));
    }

    #[test]
    fn test_markdown_contains_hardware_profile() {
        let report = mock_report(vec![mock_workload_result("wl1", 5.0, 15.0)]);
        let md = report.to_markdown();
        assert!(md.contains("## Hardware Profile"));
        assert!(md.contains("Test CPU"));
        assert!(md.contains("x86_64"));
        assert!(md.contains("32 GB"));
    }

    #[test]
    fn test_markdown_contains_guc_settings() {
        let report = mock_report(vec![mock_workload_result("wl1", 5.0, 15.0)]);
        let md = report.to_markdown();
        assert!(md.contains("## PostgreSQL Settings"));
        assert!(md.contains("work_mem"));
        assert!(md.contains("256MB"));
    }

    #[test]
    fn test_markdown_contains_methodology() {
        let report = mock_report(vec![mock_workload_result("wl1", 5.0, 15.0)]);
        let md = report.to_markdown();
        assert!(md.contains("## Methodology"));
        assert!(md.contains("| Iterations | 30 |"));
        assert!(md.contains("| Warmup iterations | 5 |"));
    }

    #[test]
    fn test_markdown_contains_workload_results() {
        let report = mock_report(vec![
            mock_workload_result("fast_wl", 5.0, 15.0),
            mock_workload_result("slow_wl", 50.0, 50.0),
        ]);
        let md = report.to_markdown();
        assert!(md.contains("fast_wl"));
        assert!(md.contains("slow_wl"));
        assert!(md.contains("## Results"));
        assert!(md.contains("## Detailed Results"));
    }

    #[test]
    fn test_markdown_no_hardware_section_when_none() {
        let report = BenchReport {
            hardware: None,
            gucs: None,
            methodology: Methodology {
                iterations: 10,
                warmup: 2,
                rows: 1000,
                ordering: "randomized".to_owned(),
                statistical_tests: vec![],
            },
            workloads: vec![mock_workload_result("wl", 10.0, 20.0)],
        };
        let md = report.to_markdown();
        assert!(!md.contains("## Hardware Profile"));
        assert!(!md.contains("## PostgreSQL Settings"));
    }

    #[test]
    fn test_markdown_empty_workloads() {
        let report = mock_report(vec![]);
        let md = report.to_markdown();
        assert!(md.contains("## Results"));
        // Table header present but no data rows
        assert!(md.contains("| Workload |"));
    }

    // -----------------------------------------------------------------------
    // JSON output
    // -----------------------------------------------------------------------

    #[test]
    #[allow(clippy::expect_used)]
    fn test_json_roundtrip() {
        let report = mock_report(vec![mock_workload_result("json_wl", 8.0, 16.0)]);
        let json_str = report.to_json().expect("serialization should succeed");
        let deserialized: BenchReport =
            serde_json::from_str(&json_str).expect("deserialization should succeed");
        assert_eq!(deserialized.workloads.len(), 1);
        assert_eq!(deserialized.workloads[0].name, "json_wl");
        assert!(
            (deserialized.workloads[0].speedup_vs_single - report.workloads[0].speedup_vs_single)
                .abs()
                < f64::EPSILON
        );
    }

    #[test]
    #[allow(clippy::expect_used)]
    fn test_json_contains_all_fields() {
        let report = mock_report(vec![mock_workload_result("fields", 10.0, 20.0)]);
        let json_str = report.to_json().expect("serialization should succeed");
        assert!(json_str.contains("\"accel_mean_ms\""));
        assert!(json_str.contains("\"parallel_mean_ms\""));
        assert!(json_str.contains("\"single_mean_ms\""));
        assert!(json_str.contains("\"speedup_vs_single\""));
        assert!(json_str.contains("\"speedup_vs_parallel\""));
        assert!(json_str.contains("\"p_value_vs_single\""));
        assert!(json_str.contains("\"accel_ci_95\""));
        assert!(json_str.contains("\"hardware\""));
        assert!(json_str.contains("\"methodology\""));
    }

    // -----------------------------------------------------------------------
    // CSV output
    // -----------------------------------------------------------------------

    #[test]
    fn test_csv_header_row() {
        let report = mock_report(vec![mock_workload_result("csv_wl", 10.0, 20.0)]);
        let csv = report.to_csv();
        let lines: Vec<&str> = csv.lines().collect();
        assert!(lines.len() >= 2, "CSV should have header + data");
        assert!(lines[0].starts_with("workload,"));
        assert!(lines[0].contains("accel_mean_ms"));
        assert!(lines[0].contains("parallel_mean_ms"));
        assert!(lines[0].contains("single_mean_ms"));
        assert!(lines[0].contains("speedup_vs_single"));
        assert!(lines[0].contains("speedup_vs_parallel"));
        assert!(lines[0].contains("significant"));
    }

    #[test]
    fn test_csv_data_row_count() {
        let report = mock_report(vec![
            mock_workload_result("wl_a", 5.0, 10.0),
            mock_workload_result("wl_b", 20.0, 20.0),
            mock_workload_result("wl_c", 30.0, 15.0),
        ]);
        let csv = report.to_csv();
        let lines: Vec<&str> = csv.lines().collect();
        // 1 header + 3 data rows
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn test_csv_workload_name_in_row() {
        let report = mock_report(vec![mock_workload_result("my_workload", 10.0, 20.0)]);
        let csv = report.to_csv();
        let data_line = csv.lines().nth(1);
        assert!(
            data_line.is_some_and(|l| l.starts_with("my_workload,")),
            "data row should start with workload name"
        );
    }

    #[test]
    fn test_csv_significance_labels() {
        // p < 0.01 -> "yes", p in [0.01, 0.05) -> "marginal", p >= 0.05 -> "no"
        // With very different accel/baseline, p should be small -> "yes"
        let report = mock_report(vec![mock_workload_result("sig_test", 1.0, 100.0)]);
        let csv = report.to_csv();
        let data_line = csv.lines().nth(1).unwrap_or("");
        assert!(
            data_line.ends_with(",yes") || data_line.ends_with(",marginal"),
            "large difference should be significant: {data_line}"
        );
    }

    #[test]
    fn test_csv_empty_workloads() {
        let report = mock_report(vec![]);
        let csv = report.to_csv();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 1, "only header row when no workloads");
    }

    #[test]
    fn test_csv_column_count_matches_header() {
        let report = mock_report(vec![mock_workload_result("col_test", 10.0, 20.0)]);
        let csv = report.to_csv();
        let lines: Vec<&str> = csv.lines().collect();
        let header_cols = lines[0].split(',').count();
        let data_cols = lines[1].split(',').count();
        assert_eq!(
            header_cols, data_cols,
            "header has {header_cols} columns but data has {data_cols}"
        );
    }

    // -----------------------------------------------------------------------
    // generate_report (without PG connection)
    // -----------------------------------------------------------------------

    #[test]
    fn test_generate_report_no_connection() {
        let workloads = vec![mock_workload_result("gen_test", 10.0, 20.0)];
        let report = generate_report(workloads, None, 30, 5, 100_000);
        assert!(report.hardware.is_some());
        assert!(report.gucs.is_none()); // no connection -> no GUCs
        assert_eq!(report.methodology.iterations, 30);
        assert_eq!(report.methodology.warmup, 5);
        assert_eq!(report.methodology.rows, 100_000);
        assert_eq!(report.workloads.len(), 1);
    }
}
