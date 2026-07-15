//! Statistical analysis utilities for benchmark results.
//!
//! Public API consumed by `report` module and available for external callers.

use serde::{Deserialize, Serialize};

/// A self-contained benchmark result suitable for statistical analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct BenchmarkResult {
    /// Human-readable name of the benchmark.
    pub name: String,
    /// Which mode produced these timings (e.g. "accel", "baseline").
    pub mode: String,
    /// Number of iterations actually recorded.
    pub iterations: usize,
    /// Raw timing values in milliseconds.
    pub timings_ms: Vec<f64>,
}

impl BenchmarkResult {
    /// Create a new result, computing `iterations` from the timings length.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(name: String, mode: String, timings_ms: Vec<f64>) -> Self {
        let iterations = timings_ms.len();
        Self {
            name,
            mode,
            iterations,
            timings_ms,
        }
    }
}

/// Arithmetic mean of a slice. Returns 0.0 for empty input.
#[must_use]
pub fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let count = values.len() as f64;
    values.iter().sum::<f64>() / count
}

/// Median of a slice (sorts a copy). Returns 0.0 for empty input.
#[must_use]
pub fn median(values: &[f64]) -> f64 {
    percentile(values, 50.0)
}

/// Linear-interpolated percentile (inclusive type, identical to NumPy default).
///
/// `p` is in `[0, 100]`. Returns `0.0` for empty input.
#[must_use]
pub fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if sorted.len() == 1 {
        return sorted[0];
    }
    #[allow(clippy::cast_precision_loss)]
    let last = (sorted.len() - 1) as f64;
    let rank = (p / 100.0).clamp(0.0, 1.0) * last;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lo = rank.floor() as usize;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let hi = rank.ceil() as usize;
    let frac = rank - rank.floor();
    (sorted[hi] - sorted[lo]).mul_add(frac, sorted[lo])
}

/// Coefficient of variation (stddev / mean) in percent. Returns `NaN` if
/// mean is zero or negative (CV is undefined for non-positive means).
#[must_use]
pub fn cv_percent(values: &[f64]) -> f64 {
    let m = mean(values);
    if m <= 0.0 {
        return f64::NAN;
    }
    (stddev(values) / m) * 100.0
}

/// Sample standard deviation (Bessel-corrected, n-1 denominator).
/// Returns 0.0 for fewer than 2 values.
#[must_use]
pub fn stddev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let avg = mean(values);
    #[allow(clippy::cast_precision_loss)]
    let count = values.len() as f64;
    let variance = values.iter().map(|v| (v - avg).powi(2)).sum::<f64>() / (count - 1.0);
    variance.sqrt()
}

/// Minimum value. Returns `f64::NAN` for empty input.
#[must_use]
#[cfg_attr(not(test), allow(dead_code))]
pub fn min(values: &[f64]) -> f64 {
    values.iter().copied().reduce(f64::min).unwrap_or(f64::NAN)
}

/// Maximum value. Returns `f64::NAN` for empty input.
#[must_use]
#[cfg_attr(not(test), allow(dead_code))]
pub fn max(values: &[f64]) -> f64 {
    values.iter().copied().reduce(f64::max).unwrap_or(f64::NAN)
}

/// 95% confidence interval for the mean, using a t-distribution approximation.
///
/// Returns `(lower, upper)`. For fewer than 2 values, returns `(NaN, NaN)`.
#[must_use]
pub fn confidence_interval_95(values: &[f64]) -> (f64, f64) {
    if values.len() < 2 {
        return (f64::NAN, f64::NAN);
    }
    let avg = mean(values);
    let sd = stddev(values);
    #[allow(clippy::cast_precision_loss)]
    let count = values.len() as f64;
    let standard_error = sd / count.sqrt();
    // Use t-critical values for common sample sizes, fall back to 1.96 for large n.
    let t_crit = t_critical_95(values.len() - 1);
    let margin = t_crit * standard_error;
    (avg - margin, avg + margin)
}

/// Speedup ratio: `baseline_mean / experiment_mean`.
/// Values > 1 mean the experiment is faster than baseline.
/// Returns `NaN` if experiment mean is zero or either has no data.
#[must_use]
#[cfg_attr(not(test), allow(dead_code))]
pub fn speedup(baseline: &BenchmarkResult, experiment: &BenchmarkResult) -> f64 {
    if baseline.timings_ms.is_empty() || experiment.timings_ms.is_empty() {
        return f64::NAN;
    }
    let base_avg = mean(&baseline.timings_ms);
    let exp_avg = mean(&experiment.timings_ms);
    if exp_avg.abs() < f64::EPSILON {
        return f64::NAN;
    }
    base_avg / exp_avg
}

/// Two-sample Welch's t-test p-value (two-tailed, approximate).
///
/// Returns a p-value. Values < 0.05 suggest statistically significant difference.
/// Returns `NaN` if either sample has fewer than 2 values.
#[must_use]
#[allow(clippy::similar_names)]
#[cfg_attr(not(test), allow(dead_code))]
pub fn welch_t_test_p(sample_a: &[f64], sample_b: &[f64]) -> f64 {
    if sample_a.len() < 2 || sample_b.len() < 2 {
        return f64::NAN;
    }
    let mean_a = mean(sample_a);
    let mean_b = mean(sample_b);
    let sd_a = stddev(sample_a);
    let sd_b = stddev(sample_b);
    #[allow(clippy::cast_precision_loss)]
    let count_a = sample_a.len() as f64;
    #[allow(clippy::cast_precision_loss)]
    let count_b = sample_b.len() as f64;

    let var_over_n_a = sd_a.powi(2) / count_a;
    let var_over_n_b = sd_b.powi(2) / count_b;
    let pooled_se = (var_over_n_a + var_over_n_b).sqrt();

    if pooled_se.abs() < f64::EPSILON {
        return if (mean_a - mean_b).abs() < f64::EPSILON {
            1.0
        } else {
            0.0
        };
    }

    let t_stat = (mean_a - mean_b).abs() / pooled_se;

    // Welch-Satterthwaite degrees of freedom
    let degrees_of_freedom = (var_over_n_a + var_over_n_b).powi(2)
        / (var_over_n_a.powi(2) / (count_a - 1.0) + var_over_n_b.powi(2) / (count_b - 1.0));

    // Approximate two-tailed p-value from t and df.
    approx_t_p_value(t_stat, degrees_of_freedom)
}

/// Paired t-test p-value (two-tailed, approximate).
///
/// Computes differences `d_i = sample_a[i] - sample_b[i]`, then tests whether
/// the mean difference is significantly different from zero.
///
/// Returns `NaN` if samples have different lengths or fewer than 2 values.
#[must_use]
pub fn paired_t_test_p(sample_a: &[f64], sample_b: &[f64]) -> f64 {
    if sample_a.len() != sample_b.len() || sample_a.len() < 2 {
        return f64::NAN;
    }
    let diffs: Vec<f64> = sample_a
        .iter()
        .zip(sample_b.iter())
        .map(|(a, b)| a - b)
        .collect();
    let mean_d = mean(&diffs);
    let sd_d = stddev(&diffs);

    if sd_d.abs() < f64::EPSILON {
        return if mean_d.abs() < f64::EPSILON {
            1.0
        } else {
            0.0
        };
    }

    #[allow(clippy::cast_precision_loss)]
    let n = diffs.len() as f64;
    let t_stat = (mean_d / (sd_d / n.sqrt())).abs();
    let df = n - 1.0;
    approx_t_p_value(t_stat, df)
}

/// Cohen's d effect size between two samples.
///
/// Uses the pooled standard deviation. Interpretation:
/// - |d| < 0.2: negligible
/// - 0.2 <= |d| < 0.5: small
/// - 0.5 <= |d| < 0.8: medium
/// - |d| >= 0.8: large
///
/// Positive d means sample_a > sample_b.
/// Returns `NaN` if either sample has fewer than 2 values.
#[must_use]
pub fn cohens_d(sample_a: &[f64], sample_b: &[f64]) -> f64 {
    if sample_a.len() < 2 || sample_b.len() < 2 {
        return f64::NAN;
    }
    let mean_a = mean(sample_a);
    let mean_b = mean(sample_b);
    let sd_a = stddev(sample_a);
    let sd_b = stddev(sample_b);
    #[allow(clippy::cast_precision_loss)]
    let n_a = sample_a.len() as f64;
    #[allow(clippy::cast_precision_loss)]
    let n_b = sample_b.len() as f64;

    // Pooled standard deviation
    #[allow(clippy::suboptimal_flops)]
    let pooled_sd =
        (((n_a - 1.0) * sd_a.powi(2) + (n_b - 1.0) * sd_b.powi(2)) / (n_a + n_b - 2.0)).sqrt();

    if pooled_sd.abs() < f64::EPSILON {
        return if (mean_a - mean_b).abs() < f64::EPSILON {
            0.0
        } else {
            f64::INFINITY.copysign(mean_a - mean_b)
        };
    }

    (mean_a - mean_b) / pooled_sd
}

/// Detect outliers: returns indices of values more than `sigma_threshold` standard
/// deviations from the mean.
#[must_use]
pub fn detect_outliers(values: &[f64], sigma_threshold: f64) -> Vec<usize> {
    if values.len() < 2 {
        return Vec::new();
    }
    let avg = mean(values);
    let sd = stddev(values);
    if sd.abs() < f64::EPSILON {
        return Vec::new();
    }
    values
        .iter()
        .enumerate()
        .filter(|(_, v)| ((*v - avg) / sd).abs() > sigma_threshold)
        .map(|(idx, _)| idx)
        .collect()
}

/// Apply Bonferroni correction to a raw p-value.
///
/// Multiplies the p-value by the number of tests and clamps to `[0, 1]`.
/// This is the standard Bonferroni family-wise error rate correction — for
/// `n` independent tests, we adjust alpha by dividing by `n` (equivalently,
/// multiply p by `n`) so the family-wise alpha stays at the target level.
///
/// Returns `NaN` if the input p is `NaN`. Returns `1.0` if `n_tests == 0`
/// or the adjusted value exceeds 1.
#[must_use]
pub fn bonferroni_adjusted_p(p: f64, n_tests: usize) -> f64 {
    if p.is_nan() {
        return f64::NAN;
    }
    if n_tests == 0 {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = n_tests as f64;
    (p * n).clamp(0.0, 1.0)
}

/// Geometric mean of a slice of speedup ratios.
///
/// Computes `exp(mean(ln(x)))`. Non-finite or non-positive inputs are skipped
/// with a warning printed to stderr. Returns `1.0` for an empty effective
/// input (the "no effect" neutral element for ratios).
///
/// The geometric mean is the correct summary statistic for speedup ratios:
/// arithmetic mean of ratios is biased upward and gives nonsense results
/// (e.g. 0.5x and 2.0x should average to 1.0, not 1.25).
#[must_use]
pub fn geomean(speedups: &[f64]) -> f64 {
    if speedups.is_empty() {
        return 1.0;
    }
    let mut log_sum = 0.0_f64;
    let mut count = 0_usize;
    let mut skipped = 0_usize;
    for &v in speedups {
        if v.is_finite() && v > 0.0 {
            log_sum += v.ln();
            count += 1;
        } else {
            skipped += 1;
        }
    }
    if skipped > 0 {
        eprintln!(
            "[stats::geomean] skipped {skipped} non-finite/non-positive value(s) of {}",
            speedups.len()
        );
    }
    if count == 0 {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = count as f64;
    (log_sum / n).exp()
}

/// Compute geometric mean speedup grouped by workload category, but only
/// over workloads matching a caller-provided filter.
///
/// Returns a map from category name to its geomean. Categories with no valid
/// entries after the filter are omitted.
#[must_use]
#[allow(dead_code)] // public API, used by consumers via render_geomean_headline path
pub fn geomean_by_category_filtered<F>(
    results: &[crate::report::WorkloadResult],
    mut filter: F,
) -> std::collections::BTreeMap<String, f64>
where
    F: FnMut(&crate::report::WorkloadResult) -> bool,
{
    use std::collections::BTreeMap;
    let mut buckets: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for w in results {
        if filter(w) {
            buckets
                .entry(w.category.clone())
                .or_default()
                .push(w.speedup_vs_parallel);
        }
    }
    let mut out: BTreeMap<String, f64> = BTreeMap::new();
    for (cat, speedups) in buckets {
        if !speedups.is_empty() {
            out.insert(cat, geomean(&speedups));
        }
    }
    out
}

/// Compute geometric mean speedup grouped by workload category.
///
/// Returns a map from category name to its geomean. Categories with no valid
/// speedups are omitted. Uses `crate::report::WorkloadResult::category` and
/// `speedup_vs_parallel` as inputs.
#[must_use]
#[allow(dead_code)] // public API, used by consumers and tests
pub fn geomean_by_category(
    results: &[crate::report::WorkloadResult],
) -> std::collections::BTreeMap<String, f64> {
    use std::collections::BTreeMap;
    let mut buckets: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for w in results {
        buckets
            .entry(w.category.clone())
            .or_default()
            .push(w.speedup_vs_parallel);
    }
    let mut out: BTreeMap<String, f64> = BTreeMap::new();
    for (cat, speedups) in buckets {
        let gm = geomean(&speedups);
        out.insert(cat, gm);
    }
    out
}

/// Count the number of workloads that remain statistically significant
/// after Bonferroni correction at the given alpha level.
///
/// The correction uses `n_tests = results.len()` — every workload in the
/// set counts against the family-wise error rate.
#[must_use]
#[allow(dead_code)] // public API, used by external tooling
pub fn significant_after_bonferroni(
    results: &[crate::report::WorkloadResult],
    alpha: f64,
) -> usize {
    let n = results.len();
    results
        .iter()
        .filter(|w| {
            let adj = bonferroni_adjusted_p(w.p_value_vs_parallel, n);
            adj.is_finite() && adj < alpha
        })
        .count()
}

/// Format a set of `BenchmarkResult`s as a markdown comparison table.
#[must_use]
#[cfg_attr(not(test), allow(dead_code))]
pub fn format_markdown_table(results: &[BenchmarkResult]) -> String {
    use std::fmt::Write as _;

    if results.is_empty() {
        return String::from("_(no results)_\n");
    }

    let mut out = String::new();
    out.push_str(
        "| Benchmark | Mode | Iterations | Mean (ms) | Median (ms) \
         | Stddev (ms) | 95% CI (ms) |\n",
    );
    out.push_str(
        "|-----------|------|------------|-----------|------------- \
         |-------------|-------------|\n",
    );
    for r in results {
        let avg = mean(&r.timings_ms);
        let med = median(&r.timings_ms);
        let sd = stddev(&r.timings_ms);
        let (ci_lo, ci_hi) = confidence_interval_95(&r.timings_ms);
        let _ = writeln!(
            out,
            "| {} | {} | {} | {:.2} | {:.2} | {:.2} | {:.2}..{:.2} |",
            r.name, r.mode, r.iterations, avg, med, sd, ci_lo, ci_hi,
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Look up t-critical value for 95% two-tailed test given degrees of freedom.
/// Uses a small table for common df, falls back to 1.96 for large df.
fn t_critical_95(df: usize) -> f64 {
    // Two-tailed 95% (alpha=0.05) t-critical values
    match df {
        1 => 12.706,
        2 => 4.303,
        3 => 3.182,
        4 => 2.776,
        5 => 2.571,
        6 => 2.447,
        7 => 2.365,
        8 => 2.306,
        9 => 2.262,
        10 => 2.228,
        11 => 2.201,
        12 => 2.179,
        13 => 2.160,
        14 => 2.145,
        15 => 2.131,
        16..=20 => 2.086,
        21..=30 => 2.042,
        31..=60 => 2.000,
        61..=120 => 1.980,
        _ => 1.960,
    }
}

/// Approximate two-tailed p-value for Student's t distribution.
///
/// Uses the relationship between the t-distribution CDF and the regularized
/// incomplete beta function.
#[allow(clippy::suboptimal_flops)]
fn approx_t_p_value(t_stat: f64, df: f64) -> f64 {
    if df <= 0.0 || t_stat.is_nan() || df.is_nan() {
        return f64::NAN;
    }
    // For large df, t-distribution converges to normal.
    if df > 200.0 {
        return 2.0 * normal_sf(t_stat);
    }
    // Approximation via the regularized incomplete beta function:
    // p = I_x(a, b) where x = df/(df + t^2), a = df/2, b = 0.5
    let x_val = df / (df + t_stat * t_stat);
    let alpha = df / 2.0;
    let beta = 0.5;
    regularized_incomplete_beta(x_val, alpha, beta)
}

/// Survival function of the standard normal distribution: P(Z > z).
/// Uses an approximation of erfc.
fn normal_sf(z_score: f64) -> f64 {
    0.5 * erfc_approx(z_score / std::f64::consts::SQRT_2)
}

/// Approximate erfc using Horner form of Abramowitz & Stegun 7.1.26.
#[allow(clippy::suboptimal_flops)]
fn erfc_approx(x_val: f64) -> f64 {
    if x_val < 0.0 {
        return 2.0 - erfc_approx(-x_val);
    }
    let t_val = 1.0 / (1.0 + 0.327_591_1 * x_val);
    let poly = t_val
        * (0.254_829_592
            + t_val
                * ((-0.284_496_736)
                    + t_val
                        * (1.421_413_741 + t_val * ((-1.453_152_027) + t_val * 1.061_405_429))));
    poly * (-x_val * x_val).exp()
}

/// Regularized incomplete beta function I_x(a, b) via continued fraction
/// (Lentz's method). Good enough for our t-test p-values.
#[allow(clippy::many_single_char_names, clippy::suboptimal_flops)]
fn regularized_incomplete_beta(x_val: f64, alpha: f64, beta: f64) -> f64 {
    if x_val <= 0.0 {
        return 0.0;
    }
    if x_val >= 1.0 {
        return 1.0;
    }

    // Use the symmetry relation if x > (a+1)/(a+b+2) for better convergence.
    if x_val > (alpha + 1.0) / (alpha + beta + 2.0) {
        return 1.0 - regularized_incomplete_beta(1.0 - x_val, beta, alpha);
    }

    let ln_prefix =
        alpha * x_val.ln() + beta * (1.0 - x_val).ln() - ln_beta(alpha, beta) - alpha.ln();
    let prefix = ln_prefix.exp();

    // Lentz continued fraction for I_x(a,b)
    let mut cf_c = 1.0;
    let mut cf_d = 1.0 - (alpha + beta) * x_val / (alpha + 1.0);
    if cf_d.abs() < 1e-30 {
        cf_d = 1e-30;
    }
    cf_d = 1.0 / cf_d;
    let mut cf_f = cf_d;

    for step in 1..=200 {
        let mf = f64::from(step);

        // Even step
        let two_m = 2.0 * mf;
        let numer_even = mf * (beta - mf) * x_val / ((alpha + two_m - 1.0) * (alpha + two_m));
        cf_d = 1.0 + numer_even * cf_d;
        if cf_d.abs() < 1e-30 {
            cf_d = 1e-30;
        }
        cf_c = 1.0 + numer_even / cf_c;
        if cf_c.abs() < 1e-30 {
            cf_c = 1e-30;
        }
        cf_d = 1.0 / cf_d;
        cf_f *= cf_c * cf_d;

        // Odd step
        let numer_odd =
            -(alpha + mf) * (alpha + beta + mf) * x_val / ((alpha + two_m) * (alpha + two_m + 1.0));
        cf_d = 1.0 + numer_odd * cf_d;
        if cf_d.abs() < 1e-30 {
            cf_d = 1e-30;
        }
        cf_c = 1.0 + numer_odd / cf_c;
        if cf_c.abs() < 1e-30 {
            cf_c = 1e-30;
        }
        cf_d = 1.0 / cf_d;
        let delta = cf_c * cf_d;
        cf_f *= delta;

        if (delta - 1.0).abs() < 1e-10 {
            break;
        }
    }

    prefix * cf_f
}

/// Natural log of the beta function: ln(B(a,b)) = ln(Gamma(a)) + ln(Gamma(b)) - ln(Gamma(a+b)).
fn ln_beta(alpha: f64, beta: f64) -> f64 {
    ln_gamma(alpha) + ln_gamma(beta) - ln_gamma(alpha + beta)
}

/// Lanczos approximation of ln(Gamma(x)) for x > 0.
#[allow(clippy::excessive_precision, clippy::suboptimal_flops)]
fn ln_gamma(x_val: f64) -> f64 {
    // Lanczos coefficients (g=7, n=9)
    const COEFFS: [f64; 9] = [
        0.999_999_999_999_809_93,
        676.520_368_121_885_1,
        -1_259.139_216_722_403,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    const LG: f64 = 7.0;

    if x_val < 0.5 {
        // Reflection formula
        let pi = std::f64::consts::PI;
        return pi.ln() - (pi * x_val).sin().ln() - ln_gamma(1.0 - x_val);
    }

    let shifted = x_val - 1.0;
    let mut sum = COEFFS[0];
    for (idx, &coeff) in COEFFS.iter().enumerate().skip(1) {
        #[allow(clippy::cast_precision_loss)]
        let idx_f = idx as f64;
        sum += coeff / (shifted + idx_f);
    }
    let t_val = shifted + LG + 0.5;
    (shifted + 0.5).mul_add(t_val.ln(), -t_val) + 0.5 * (2.0 * std::f64::consts::PI).ln() + sum.ln()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f64 = 1e-6;

    #[test]
    fn test_mean_empty() {
        assert!((mean(&[]) - 0.0).abs() < EPSILON);
    }

    #[test]
    fn test_mean_values() {
        assert!((mean(&[1.0, 2.0, 3.0, 4.0, 5.0]) - 3.0).abs() < EPSILON);
    }

    #[test]
    fn test_mean_single() {
        assert!((mean(&[42.0]) - 42.0).abs() < EPSILON);
    }

    #[test]
    fn test_median_odd() {
        assert!((median(&[3.0, 1.0, 2.0]) - 2.0).abs() < EPSILON);
    }

    #[test]
    fn test_median_even() {
        assert!((median(&[4.0, 1.0, 3.0, 2.0]) - 2.5).abs() < EPSILON);
    }

    #[test]
    fn test_median_empty() {
        assert!((median(&[]) - 0.0).abs() < EPSILON);
    }

    #[test]
    fn test_median_single() {
        assert!((median(&[7.0]) - 7.0).abs() < EPSILON);
    }

    #[test]
    fn test_stddev_empty() {
        assert!((stddev(&[]) - 0.0).abs() < EPSILON);
    }

    #[test]
    fn test_stddev_single() {
        assert!((stddev(&[5.0]) - 0.0).abs() < EPSILON);
    }

    #[test]
    fn test_stddev_known() {
        // Sample stddev of [2, 4, 4, 4, 5, 5, 7, 9] = 2.13809
        let vals = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let sd = stddev(&vals);
        assert!((sd - 2.138_089_935_299_395).abs() < 1e-4, "got {sd}");
    }

    #[test]
    fn test_confidence_interval_95_few_values() {
        let (lo, hi) = confidence_interval_95(&[1.0]);
        assert!(lo.is_nan());
        assert!(hi.is_nan());
    }

    #[test]
    fn test_confidence_interval_95_known() {
        // 10 identical values -> stddev=0, CI collapses to mean
        let vals = vec![5.0; 10];
        let (lo, hi) = confidence_interval_95(&vals);
        assert!((lo - 5.0).abs() < EPSILON);
        assert!((hi - 5.0).abs() < EPSILON);
    }

    #[test]
    fn test_confidence_interval_95_range() {
        let vals = [10.0, 12.0, 11.0, 13.0, 9.0, 11.0, 10.0, 12.0, 11.0, 10.0];
        let (lo, hi) = confidence_interval_95(&vals);
        let avg = mean(&vals);
        // CI should be centered around mean and lo < mean < hi
        assert!(lo < avg, "lo={lo} should be < mean={avg}");
        assert!(hi > avg, "hi={hi} should be > mean={avg}");
        // CI should be reasonable (within a few ms of mean for this data)
        assert!(avg - lo < 3.0, "margin too wide: {}", avg - lo);
    }

    #[test]
    fn test_speedup_basic() {
        let baseline =
            BenchmarkResult::new("test".to_owned(), "baseline".to_owned(), vec![100.0, 100.0]);
        let experiment =
            BenchmarkResult::new("test".to_owned(), "accel".to_owned(), vec![50.0, 50.0]);
        let ratio = speedup(&baseline, &experiment);
        assert!(
            (ratio - 2.0).abs() < EPSILON,
            "expected 2x speedup, got {ratio}"
        );
    }

    #[test]
    fn test_speedup_empty() {
        let baseline = BenchmarkResult::new("a".to_owned(), "b".to_owned(), vec![]);
        let experiment = BenchmarkResult::new("a".to_owned(), "e".to_owned(), vec![1.0]);
        assert!(speedup(&baseline, &experiment).is_nan());
    }

    #[test]
    fn test_welch_identical() {
        // Identical samples -> p should be close to 1.0
        let sample_a = [10.0, 10.0, 10.0, 10.0, 10.0];
        let sample_b = [10.0, 10.0, 10.0, 10.0, 10.0];
        let p_val = welch_t_test_p(&sample_a, &sample_b);
        assert!(
            (p_val - 1.0).abs() < 0.01,
            "identical samples should have p~1.0, got {p_val}"
        );
    }

    #[test]
    fn test_welch_very_different() {
        // Very different samples -> p should be very small
        let sample_a = [1.0, 1.1, 0.9, 1.0, 1.05, 0.95, 1.0, 1.0, 1.02, 0.98];
        let sample_b = [
            100.0, 100.1, 99.9, 100.0, 100.05, 99.95, 100.0, 100.0, 100.02, 99.98,
        ];
        let p_val = welch_t_test_p(&sample_a, &sample_b);
        assert!(
            p_val < 0.001,
            "very different samples should have p<<0.01, got {p_val}"
        );
    }

    #[test]
    fn test_detect_outliers_none() {
        let vals = [10.0, 10.1, 9.9, 10.0, 10.05];
        assert!(detect_outliers(&vals, 3.0).is_empty());
    }

    #[test]
    fn test_detect_outliers_present() {
        // Use 2-sigma threshold with a clear outlier.
        let vals = [10.0, 10.1, 9.9, 10.0, 10.05, 9.95, 10.0, 10.0, 10.1, 100.0];
        let outliers = detect_outliers(&vals, 2.0);
        assert_eq!(outliers, vec![9]);
    }

    #[test]
    fn test_cohens_d_identical() {
        let a = [10.0, 10.0, 10.0, 10.0, 10.0];
        let b = [10.0, 10.0, 10.0, 10.0, 10.0];
        assert!((cohens_d(&a, &b) - 0.0).abs() < EPSILON);
    }

    #[test]
    fn test_cohens_d_large_effect() {
        // Very different samples should produce large |d|
        let a = [100.0, 101.0, 99.0, 100.0, 100.5];
        let b = [10.0, 11.0, 9.0, 10.0, 10.5];
        let d = cohens_d(&a, &b);
        assert!(d > 10.0, "expected large positive d, got {d}");
    }

    #[test]
    fn test_cohens_d_direction() {
        let a = [50.0, 51.0, 49.0];
        let b = [10.0, 11.0, 9.0];
        let d = cohens_d(&a, &b);
        assert!(d > 0.0, "a > b should give positive d, got {d}");
        let d_rev = cohens_d(&b, &a);
        assert!(d_rev < 0.0, "b < a should give negative d, got {d_rev}");
    }

    #[test]
    fn test_min_max() {
        let vals = [3.0, 1.0, 4.0, 1.5, 9.0, 2.6];
        assert!((min(&vals) - 1.0).abs() < EPSILON);
        assert!((max(&vals) - 9.0).abs() < EPSILON);
    }

    #[test]
    fn test_min_max_empty() {
        assert!(min(&[]).is_nan());
        assert!(max(&[]).is_nan());
    }

    #[test]
    fn test_format_markdown_table_empty() {
        assert_eq!(format_markdown_table(&[]), "_(no results)_\n");
    }

    #[test]
    fn test_paired_t_identical() {
        let a = [10.0, 10.0, 10.0, 10.0, 10.0];
        let b = [10.0, 10.0, 10.0, 10.0, 10.0];
        let p = paired_t_test_p(&a, &b);
        assert!(
            (p - 1.0).abs() < 0.01,
            "identical paired samples should have p~1.0, got {p}"
        );
    }

    #[test]
    fn test_paired_t_consistently_different() {
        // Every a[i] is much larger than b[i] -> very small p
        let a = [
            100.0, 101.0, 99.5, 100.5, 100.2, 99.8, 100.1, 100.3, 99.9, 100.4,
        ];
        let b = [10.0, 11.0, 9.5, 10.5, 10.2, 9.8, 10.1, 10.3, 9.9, 10.4];
        let p = paired_t_test_p(&a, &b);
        assert!(
            p < 0.001,
            "consistently different pairs should have p<<0.01, got {p}"
        );
    }

    #[test]
    fn test_paired_t_mixed_differences() {
        // Some a[i] > b[i], some a[i] < b[i] -> moderate p
        let a = [10.0, 12.0, 9.0, 11.0, 10.5];
        let b = [11.0, 10.0, 12.0, 9.0, 10.5];
        let p = paired_t_test_p(&a, &b);
        assert!(p > 0.1, "mixed differences should have moderate p, got {p}");
    }

    #[test]
    fn test_paired_t_different_lengths() {
        let a = [1.0, 2.0];
        let b = [1.0, 2.0, 3.0];
        assert!(paired_t_test_p(&a, &b).is_nan());
    }

    #[test]
    fn test_paired_t_too_few() {
        let a = [1.0];
        let b = [2.0];
        assert!(paired_t_test_p(&a, &b).is_nan());
    }

    #[test]
    fn test_format_markdown_table_content() {
        let results = vec![BenchmarkResult::new(
            "test_wl".to_owned(),
            "accel".to_owned(),
            vec![10.0, 12.0, 11.0],
        )];
        let table = format_markdown_table(&results);
        assert!(table.contains("test_wl"));
        assert!(table.contains("accel"));
        assert!(table.contains("| 3 |")); // iterations
    }

    // -----------------------------------------------------------------------
    // Edge cases: single value
    // -----------------------------------------------------------------------

    #[test]
    fn test_mean_single_value() {
        assert!((mean(&[99.9]) - 99.9).abs() < EPSILON);
    }

    #[test]
    fn test_median_single_value() {
        assert!((median(&[42.0]) - 42.0).abs() < EPSILON);
    }

    #[test]
    fn test_stddev_two_values() {
        // stddev of [0, 10] with Bessel correction: sqrt((25+25)/1) = sqrt(50)
        let sd = stddev(&[0.0, 10.0]);
        let expected = (50.0_f64).sqrt(); // 7.0711...
        assert!(
            (sd - expected).abs() < 1e-4,
            "got {sd}, expected {expected}"
        );
    }

    #[test]
    fn test_ci_95_two_values() {
        let (lo, hi) = confidence_interval_95(&[10.0, 20.0]);
        let avg = mean(&[10.0, 20.0]);
        assert!(lo < avg);
        assert!(hi > avg);
    }

    // -----------------------------------------------------------------------
    // Edge cases: identical values
    // -----------------------------------------------------------------------

    #[test]
    fn test_stddev_identical_values() {
        let vals = vec![7.0; 100];
        assert!(stddev(&vals).abs() < EPSILON);
    }

    #[test]
    fn test_detect_outliers_identical_values() {
        let vals = vec![5.0; 20];
        assert!(detect_outliers(&vals, 3.0).is_empty());
    }

    #[test]
    fn test_paired_t_identical_constant_samples() {
        // All values the same in both -> p should be 1.0
        let a = vec![5.0; 10];
        let b = vec![5.0; 10];
        let p = paired_t_test_p(&a, &b);
        assert!(
            (p - 1.0).abs() < 0.01,
            "identical constant paired samples: p={p}"
        );
    }

    // -----------------------------------------------------------------------
    // Edge cases: very different values
    // -----------------------------------------------------------------------

    #[test]
    fn test_stddev_very_spread() {
        let vals = [1.0, 1_000_000.0];
        let sd = stddev(&vals);
        assert!(sd > 100_000.0, "stddev should be large: {sd}");
    }

    #[test]
    fn test_cohens_d_zero_variance_different_means() {
        // All values identical within each group but groups differ
        let a = vec![100.0; 5];
        let b = vec![50.0; 5];
        let d = cohens_d(&a, &b);
        // pooled_sd is 0, means differ -> should return infinity
        assert!(d.is_infinite(), "expected infinity, got {d}");
        assert!(d > 0.0, "a > b should give positive direction");
    }

    #[test]
    fn test_cohens_d_single_value_each() {
        let a = [10.0];
        let b = [20.0];
        let d = cohens_d(&a, &b);
        assert!(d.is_nan(), "fewer than 2 values should give NaN: {d}");
    }

    #[test]
    fn test_welch_single_values() {
        let a = [10.0];
        let b = [20.0];
        assert!(welch_t_test_p(&a, &b).is_nan());
    }

    // -----------------------------------------------------------------------
    // Edge cases: min/max
    // -----------------------------------------------------------------------

    #[test]
    fn test_min_single() {
        assert!((min(&[42.0]) - 42.0).abs() < EPSILON);
    }

    #[test]
    fn test_max_single() {
        assert!((max(&[42.0]) - 42.0).abs() < EPSILON);
    }

    #[test]
    fn test_min_max_negative() {
        let vals = [-5.0, -1.0, -10.0, -3.0];
        assert!((min(&vals) - (-10.0)).abs() < EPSILON);
        assert!((max(&vals) - (-1.0)).abs() < EPSILON);
    }

    // -----------------------------------------------------------------------
    // Edge cases: speedup
    // -----------------------------------------------------------------------

    #[test]
    fn test_speedup_zero_experiment() {
        let baseline = BenchmarkResult::new("a".to_owned(), "b".to_owned(), vec![100.0, 100.0]);
        let experiment = BenchmarkResult::new("a".to_owned(), "e".to_owned(), vec![0.0, 0.0]);
        assert!(speedup(&baseline, &experiment).is_nan());
    }

    #[test]
    fn test_speedup_equal() {
        let baseline = BenchmarkResult::new("a".to_owned(), "b".to_owned(), vec![50.0, 50.0]);
        let experiment = BenchmarkResult::new("a".to_owned(), "e".to_owned(), vec![50.0, 50.0]);
        let ratio = speedup(&baseline, &experiment);
        assert!(
            (ratio - 1.0).abs() < EPSILON,
            "equal times should give 1x: {ratio}"
        );
    }

    // -----------------------------------------------------------------------
    // detect_outliers edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_detect_outliers_empty() {
        assert!(detect_outliers(&[], 3.0).is_empty());
    }

    #[test]
    fn test_detect_outliers_single_value() {
        assert!(detect_outliers(&[5.0], 3.0).is_empty());
    }

    #[test]
    fn test_detect_outliers_all_outliers_impossible() {
        // If all values are the same, none are outliers
        let vals = vec![10.0; 5];
        assert!(detect_outliers(&vals, 0.0).is_empty());
    }

    // -----------------------------------------------------------------------
    // format_markdown_table edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_markdown_table_multiple_rows() {
        let results = vec![
            BenchmarkResult::new("wl_a".to_owned(), "accel".to_owned(), vec![10.0, 12.0]),
            BenchmarkResult::new("wl_a".to_owned(), "baseline".to_owned(), vec![20.0, 22.0]),
        ];
        let table = format_markdown_table(&results);
        assert!(table.contains("accel"));
        assert!(table.contains("baseline"));
        // Both rows present (skip header + separator)
        assert_eq!(table.lines().skip(2).count(), 2);
    }

    // -----------------------------------------------------------------------
    // Bonferroni / geomean
    // -----------------------------------------------------------------------

    #[test]
    fn test_bonferroni_basic() {
        assert!((bonferroni_adjusted_p(0.01, 10) - 0.1).abs() < EPSILON);
    }

    #[test]
    fn test_bonferroni_clamped_to_one() {
        assert!((bonferroni_adjusted_p(0.5, 10) - 1.0).abs() < EPSILON);
    }

    #[test]
    fn test_bonferroni_zero_tests() {
        assert!((bonferroni_adjusted_p(0.01, 0) - 1.0).abs() < EPSILON);
    }

    #[test]
    fn test_bonferroni_nan_passthrough() {
        assert!(bonferroni_adjusted_p(f64::NAN, 5).is_nan());
    }

    #[test]
    fn test_geomean_empty_is_one() {
        assert!((geomean(&[]) - 1.0).abs() < EPSILON);
    }

    #[test]
    fn test_geomean_symmetric_ratios() {
        // 0.5x and 2.0x should geomean to 1.0 — the whole point of geomean for ratios.
        assert!((geomean(&[0.5, 2.0]) - 1.0).abs() < EPSILON);
    }

    #[test]
    fn test_geomean_skips_nonfinite() {
        // Non-finite / non-positive inputs are skipped with a warning.
        let gm = geomean(&[2.0, f64::NAN, -1.0, 2.0]);
        assert!((gm - 2.0).abs() < EPSILON);
    }

    #[test]
    fn test_geomean_all_invalid_returns_one() {
        assert!((geomean(&[f64::NAN, -1.0, 0.0]) - 1.0).abs() < EPSILON);
    }
}
