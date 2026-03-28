use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::stats;

/// Timing results for a single iteration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationResult {
    /// Execution time in milliseconds with pg_accel enabled.
    pub accel_ms: f64,
    /// Execution time in milliseconds with pg_accel disabled.
    pub baseline_ms: f64,
}

/// Aggregated results for one workload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadResult {
    pub name: String,
    pub description: String,
    pub iterations: Vec<IterationResult>,
    pub accel_mean_ms: f64,
    pub accel_stddev_ms: f64,
    pub accel_median_ms: f64,
    pub baseline_mean_ms: f64,
    pub baseline_stddev_ms: f64,
    pub baseline_median_ms: f64,
    /// `baseline_mean / accel_mean`. Values > 1 mean pg_accel is faster.
    pub speedup: f64,
    /// 95% confidence interval for accel timings: (lower, upper).
    pub accel_ci_95: (f64, f64),
    /// 95% confidence interval for baseline timings: (lower, upper).
    pub baseline_ci_95: (f64, f64),
    /// Welch's t-test p-value comparing accel vs baseline.
    pub p_value: f64,
    /// Indices of accel outlier iterations (> 3 sigma).
    pub accel_outliers: Vec<usize>,
    /// Indices of baseline outlier iterations (> 3 sigma).
    pub baseline_outliers: Vec<usize>,
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
    pub workloads: Vec<WorkloadResult>,
}

impl WorkloadResult {
    /// Build aggregated result from raw iterations.
    pub fn from_iterations(
        name: String,
        description: String,
        iterations: Vec<IterationResult>,
    ) -> Self {
        let accel_times: Vec<f64> = iterations.iter().map(|i| i.accel_ms).collect();
        let baseline_times: Vec<f64> = iterations.iter().map(|i| i.baseline_ms).collect();

        let accel_mean = stats::mean(&accel_times);
        let baseline_mean = stats::mean(&baseline_times);
        let accel_stddev = stats::stddev(&accel_times);
        let baseline_stddev = stats::stddev(&baseline_times);
        let accel_median = stats::median(&accel_times);
        let baseline_median = stats::median(&baseline_times);
        let accel_ci_95 = stats::confidence_interval_95(&accel_times);
        let baseline_ci_95 = stats::confidence_interval_95(&baseline_times);
        let p_value = stats::welch_t_test_p(&accel_times, &baseline_times);
        let accel_outliers = stats::detect_outliers(&accel_times, 3.0);
        let baseline_outliers = stats::detect_outliers(&baseline_times, 3.0);

        let speedup = if accel_mean > 0.0 {
            baseline_mean / accel_mean
        } else {
            f64::NAN
        };

        Self {
            name,
            description,
            iterations,
            accel_mean_ms: accel_mean,
            accel_stddev_ms: accel_stddev,
            accel_median_ms: accel_median,
            baseline_mean_ms: baseline_mean,
            baseline_stddev_ms: baseline_stddev,
            baseline_median_ms: baseline_median,
            speedup,
            accel_ci_95,
            baseline_ci_95,
            p_value,
            accel_outliers,
            baseline_outliers,
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
            "pg_accel.batch_size",
            "pg_accel.cost_threshold",
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

        // Results summary table
        out.push_str("## Results\n\n");
        out.push_str(
            "| Workload | Accel (ms) | Baseline (ms) | Speedup | p-value | Significant |\n",
        );
        out.push_str(
            "|----------|------------|---------------|---------|---------|-------------|\n",
        );
        for w in &self.workloads {
            let sig = if w.p_value < 0.01 {
                "YES"
            } else if w.p_value < 0.05 {
                "marginal"
            } else {
                "no"
            };
            let _ = writeln!(
                out,
                "| {} | {:.2} +/- {:.2} | {:.2} +/- {:.2} | {:.2}x | {:.4} | {} |",
                w.name,
                w.accel_mean_ms,
                w.accel_stddev_ms,
                w.baseline_mean_ms,
                w.baseline_stddev_ms,
                w.speedup,
                w.p_value,
                sig,
            );
        }
        out.push('\n');

        // Detailed per-workload sections
        out.push_str("## Detailed Results\n\n");
        for w in &self.workloads {
            let _ = writeln!(out, "### {}\n", w.name);
            let _ = writeln!(out, "**Query:** {}\n", w.description);
            let _ = writeln!(out, "| Metric | Accel | Baseline |");
            let _ = writeln!(out, "|--------|-------|----------|");
            let _ = writeln!(
                out,
                "| Mean (ms) | {:.2} | {:.2} |",
                w.accel_mean_ms, w.baseline_mean_ms
            );
            let _ = writeln!(
                out,
                "| Median (ms) | {:.2} | {:.2} |",
                w.accel_median_ms, w.baseline_median_ms
            );
            let _ = writeln!(
                out,
                "| Stddev (ms) | {:.2} | {:.2} |",
                w.accel_stddev_ms, w.baseline_stddev_ms
            );
            let _ = writeln!(
                out,
                "| 95% CI (ms) | {:.2}..{:.2} | {:.2}..{:.2} |",
                w.accel_ci_95.0, w.accel_ci_95.1, w.baseline_ci_95.0, w.baseline_ci_95.1,
            );
            let _ = writeln!(out, "| Speedup | {:.2}x | |", w.speedup);
            let _ = writeln!(out, "| p-value | {:.6} | |", w.p_value);

            if !w.accel_outliers.is_empty() {
                let _ = writeln!(
                    out,
                    "\n**Accel outliers** (>3 sigma): iterations {:?}",
                    w.accel_outliers
                );
            }
            if !w.baseline_outliers.is_empty() {
                let _ = writeln!(
                    out,
                    "\n**Baseline outliers** (>3 sigma): iterations {:?}",
                    w.baseline_outliers
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

    /// Render the report as CSV (one row per workload).
    #[must_use]
    pub fn to_csv(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "workload,accel_mean_ms,accel_stddev_ms,accel_median_ms,\
             baseline_mean_ms,baseline_stddev_ms,baseline_median_ms,\
             speedup,p_value,significant\n",
        );
        for w in &self.workloads {
            let sig = if w.p_value < 0.01 {
                "yes"
            } else if w.p_value < 0.05 {
                "marginal"
            } else {
                "no"
            };
            let _ = writeln!(
                out,
                "{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.6},{}",
                w.name,
                w.accel_mean_ms,
                w.accel_stddev_ms,
                w.accel_median_ms,
                w.baseline_mean_ms,
                w.baseline_stddev_ms,
                w.baseline_median_ms,
                w.speedup,
                w.p_value,
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
pub fn generate_report(workloads: Vec<WorkloadResult>, connection: Option<&str>) -> BenchReport {
    let hardware = Some(HardwareProfile::detect());
    let gucs = connection.and_then(|c| GucSettings::from_connection(c).ok());
    BenchReport {
        hardware,
        gucs,
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
