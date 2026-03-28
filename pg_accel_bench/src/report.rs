use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

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
    pub baseline_mean_ms: f64,
    pub baseline_stddev_ms: f64,
    /// `baseline_mean / accel_mean`. Values > 1 mean pg_accel is faster.
    pub speedup: f64,
}

/// Full benchmark report containing results for all workloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    pub workloads: Vec<WorkloadResult>,
}

impl WorkloadResult {
    /// Build aggregated result from raw iterations.
    pub fn from_iterations(
        name: String,
        description: String,
        iterations: Vec<IterationResult>,
    ) -> Self {
        #[allow(clippy::cast_precision_loss)]
        let n = iterations.len() as f64;
        let accel_mean = iterations.iter().map(|i| i.accel_ms).sum::<f64>() / n;
        let baseline_mean = iterations.iter().map(|i| i.baseline_ms).sum::<f64>() / n;

        let accel_stddev = stddev(iterations.iter().map(|i| i.accel_ms), accel_mean);
        let baseline_stddev = stddev(iterations.iter().map(|i| i.baseline_ms), baseline_mean);

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
            baseline_mean_ms: baseline_mean,
            baseline_stddev_ms: baseline_stddev,
            speedup,
        }
    }
}

impl BenchReport {
    /// Render the report as a Markdown table.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("| Workload | Accel (ms) | Baseline (ms) | Speedup |\n");
        out.push_str("|----------|------------|---------------|---------|\n");
        for w in &self.workloads {
            let _ = writeln!(
                out,
                "| {} | {:.2} +/- {:.2} | {:.2} +/- {:.2} | {:.2}x |",
                w.name,
                w.accel_mean_ms,
                w.accel_stddev_ms,
                w.baseline_mean_ms,
                w.baseline_stddev_ms,
                w.speedup,
            );
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
}

/// Population standard deviation for a set of values given the mean.
fn stddev(values: impl Iterator<Item = f64>, mean: f64) -> f64 {
    let mut count: usize = 0;
    let variance_sum: f64 = values
        .map(|v| {
            count += 1;
            (v - mean).powi(2)
        })
        .sum();
    if count < 2 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = count as f64;
    (variance_sum / (n - 1.0)).sqrt()
}
