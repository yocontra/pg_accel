use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use postgres::{Client, NoTls};
use serde::Serialize;

use crate::report::{BenchReport, WorkloadResult};
use crate::runner::{self, BenchConfig, WorkloadRunCell};
use crate::stats;
use crate::workloads::{self, Workload};

pub const PARITY_CLOSE_SPEEDUP: f64 = 1.10;

#[derive(Debug, Clone)]
pub struct Fp64CalibrationOptions {
    pub multipliers: Vec<f64>,
    pub sizes: Vec<usize>,
    pub warmup: usize,
    pub seed: u64,
    pub timing_mode: runner::TimingMode,
    pub cache_mode: runner::CacheMode,
    pub guc_profile: Option<runner::GucProfile>,
    pub skip_guc_verify: bool,
    pub capture_plans: bool,
    pub artifact_root: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fp64CalibrationSummary {
    pub generated_at_unix_seconds: u64,
    pub artifact_dir: String,
    pub sizes: Vec<usize>,
    pub multipliers: Vec<f64>,
    pub selected_multiplier: Option<f64>,
    pub runner_up_multiplier: Option<f64>,
    pub selected_geomean_speedup: Option<f64>,
    pub runner_up_geomean_speedup: Option<f64>,
    pub parity_close_speedup: f64,
    pub parity_close_cells: Vec<Fp64CalibrationCell>,
    pub candidates: Vec<Fp64CandidateSummary>,
    pub fp64_disabled_proof: Option<Fp64ExplainProof>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fp64CandidateSummary {
    pub multiplier: f64,
    pub qualified: bool,
    pub geomean_speedup: Option<f64>,
    pub min_speedup: Option<f64>,
    pub cells_observed: usize,
    pub cells_expected: usize,
    pub disqualifications: Vec<String>,
    pub cells: Vec<Fp64CalibrationCell>,
}

#[derive(Debug, Clone, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct Fp64CalibrationCell {
    pub workload: String,
    pub rows: usize,
    pub speedup_median: f64,
    pub speedup_mean: f64,
    pub gpu_kernel_dispatched: bool,
    pub plan_selected: bool,
    pub function_srf_kernel_dispatched: bool,
    pub dispatch_counter_captured: bool,
    pub gpu_kernel_execution_delta: u64,
    pub pg_accel_stock_exec_delta: u64,
    pub status: Fp64CellStatus,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fp64CellStatus {
    Pass,
    Fail,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fp64ExplainProof {
    pub workload: String,
    pub rows: usize,
    pub multiplier: f64,
    pub fp64_enabled: bool,
    pub custom_scan_selected: bool,
    pub plan: String,
}

struct Fp64MultiplierWorkload {
    inner: Box<dyn Workload>,
    multiplier: f64,
}

impl Fp64MultiplierWorkload {
    fn new(inner: Box<dyn Workload>, multiplier: f64) -> Self {
        Self { inner, multiplier }
    }
}

impl Workload for Fp64MultiplierWorkload {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn description(&self) -> &'static str {
        self.inner.description()
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        self.inner.setup_sql(rows)
    }

    fn pre_query_sql(&self) -> Vec<String> {
        let mut sql = vec![format!(
            "SET pg_accel.soft_fp64_cost_multiplier = {}",
            format_multiplier(self.multiplier)
        )];
        sql.extend(self.inner.pre_query_sql());
        sql
    }

    fn query_sql(&self) -> String {
        self.inner.query_sql()
    }

    fn baseline_query_sql(&self) -> Option<String> {
        self.inner.baseline_query_sql()
    }

    fn row_scales(&self) -> &'static [usize] {
        self.inner.row_scales()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        self.inner.cleanup_sql()
    }
}

pub fn parse_multiplier_list(raw: &str) -> Result<Vec<f64>, String> {
    let mut out = Vec::new();
    for token in raw
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let value: f64 = token
            .parse()
            .map_err(|error| format!("invalid fp64 multiplier token {token:?}: {error}"))?;
        if !value.is_finite() || !(1.0..=64.0).contains(&value) {
            return Err(format!(
                "fp64 multiplier must be finite and within [1.0, 64.0]: {token}"
            ));
        }
        if !out
            .iter()
            .any(|existing: &f64| (*existing - value).abs() < f64::EPSILON)
        {
            out.push(value);
        }
    }
    if out.is_empty() {
        return Err("at least one fp64 multiplier is required".to_owned());
    }
    out.sort_by(|a, b| compare_f64_ascending(*a, *b));
    Ok(out)
}

pub fn sizes_with_optional_cap(max_size: Option<&str>) -> Result<Vec<usize>, String> {
    let sizes = max_size.map_or_else(
        || workloads::fp64_matrix::FP64_DEFAULT_ROW_SCALES.to_vec(),
        |_| workloads::fp64_matrix::FP64_MATRIX_SIZES.to_vec(),
    );
    let capped = match max_size {
        Some(raw) => {
            let cap = workloads::fp64_matrix::parse_size_token(raw)
                .ok_or_else(|| format!("invalid fp64 max-size token: {raw:?}"))?;
            sizes
                .into_iter()
                .filter(|size| *size <= cap)
                .collect::<Vec<_>>()
        }
        None => sizes,
    };
    if capped.is_empty() {
        return Err("fp64 calibration size set is empty after applying the cap".to_owned());
    }
    Ok(capped)
}

pub fn run_fp64_calibration(
    connection: &str,
    options: &Fp64CalibrationOptions,
) -> Result<Fp64CalibrationSummary, Box<dyn std::error::Error>> {
    fs::create_dir_all(&options.artifact_root)?;

    let expected_matrix = expected_matrix_for_sizes(&options.sizes);
    let mut candidates = Vec::with_capacity(options.multipliers.len());

    for multiplier in &options.multipliers {
        eprintln!(
            "[fp64-calibrate] multiplier={} sizes={}",
            format_multiplier(*multiplier),
            options
                .sizes
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        let reports = run_candidate(connection, options, *multiplier)?;
        candidates.push(summarize_candidate(*multiplier, &reports, &expected_matrix));
    }

    let ranked = ranked_qualified_candidates(&candidates);
    let selected_multiplier = ranked.first().map(|candidate| candidate.multiplier);
    let selected_geomean_speedup = ranked
        .first()
        .and_then(|candidate| candidate.geomean_speedup);
    let runner_up_multiplier = ranked.get(1).map(|candidate| candidate.multiplier);
    let runner_up_geomean_speedup = ranked
        .get(1)
        .and_then(|candidate| candidate.geomean_speedup);
    let parity_close_cells = selected_multiplier
        .and_then(|multiplier| {
            candidates
                .iter()
                .find(|candidate| (candidate.multiplier - multiplier).abs() < f64::EPSILON)
        })
        .map_or_else(Vec::new, |candidate| {
            candidate
                .cells
                .iter()
                .filter(|cell| {
                    cell.status == Fp64CellStatus::Pass
                        && cell.speedup_median.is_finite()
                        && cell.speedup_median <= PARITY_CLOSE_SPEEDUP
                })
                .cloned()
                .collect()
        });

    let fp64_disabled_proof = match selected_multiplier {
        Some(multiplier) => Some(capture_fp64_disabled_proof(
            connection,
            multiplier,
            *options
                .sizes
                .first()
                .ok_or("fp64 calibration sizes cannot be empty")?,
            options.seed,
        )?),
        None => None,
    };

    let summary = Fp64CalibrationSummary {
        generated_at_unix_seconds: unix_now(),
        artifact_dir: options.artifact_root.display().to_string(),
        sizes: options.sizes.clone(),
        multipliers: options.multipliers.clone(),
        selected_multiplier,
        runner_up_multiplier,
        selected_geomean_speedup,
        runner_up_geomean_speedup,
        parity_close_speedup: PARITY_CLOSE_SPEEDUP,
        parity_close_cells,
        candidates,
        fp64_disabled_proof,
    };

    write_summary_artifacts(&summary)?;
    Ok(summary)
}

fn run_candidate(
    connection: &str,
    options: &Fp64CalibrationOptions,
    multiplier: f64,
) -> Result<Vec<BenchReport>, Box<dyn std::error::Error>> {
    let mut reports = Vec::new();
    for (iterations, sizes) in group_sizes_by_iterations(&options.sizes) {
        let artifact_dir = options.artifact_root.join(format!(
            "multiplier-{}-n{}",
            format_multiplier(multiplier),
            iterations
        ));
        fs::create_dir_all(&artifact_dir)?;

        let wrapped_workloads = workloads::fp64_matrix::fp64_matrix_workloads()
            .into_iter()
            .map(|workload| Fp64MultiplierWorkload::new(workload, multiplier))
            .collect::<Vec<_>>();
        let mut cells = Vec::<WorkloadRunCell<'_>>::new();
        for workload in &wrapped_workloads {
            for rows in &sizes {
                cells.push(WorkloadRunCell {
                    workload,
                    rows: *rows,
                });
            }
        }

        let config = BenchConfig {
            iterations,
            warmup: options.warmup,
            seed: options.seed,
            timing_mode: options.timing_mode,
            cache_mode: options.cache_mode,
            capture_planner_stages: false,
            plans_capture_path: if options.capture_plans {
                Some(artifact_dir.join("plans.txt"))
            } else {
                None
            },
            guc_profile: options.guc_profile.clone(),
            skip_guc_verify: options.skip_guc_verify,
            artifacts_dir: Some(artifact_dir),
        };
        reports.push(runner::run_cells_with_config(connection, &cells, &config)?);
    }
    Ok(reports)
}

fn group_sizes_by_iterations(sizes: &[usize]) -> Vec<(usize, Vec<usize>)> {
    let mut grouped: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for size in sizes {
        grouped
            .entry(workloads::fp64_matrix::n_runs_for_size(*size))
            .or_default()
            .push(*size);
    }
    grouped.into_iter().collect()
}

pub fn summarize_candidate(
    multiplier: f64,
    reports: &[BenchReport],
    expected_matrix: &BTreeSet<(String, usize)>,
) -> Fp64CandidateSummary {
    let mut cells = Vec::new();
    let mut disqualifications = Vec::new();
    let mut observed_counts = BTreeMap::<(String, usize), usize>::new();

    for report in reports {
        for crash in &report.crashes {
            disqualifications.push(format!(
                "{} @ {} rows crashed: {}",
                crash.workload, crash.rows, crash.error
            ));
        }
        for workload in &report.workloads {
            let cell = summarize_cell(workload);
            *observed_counts
                .entry((cell.workload.clone(), cell.rows))
                .or_default() += 1;
            if let Some(failure) = &cell.failure {
                disqualifications.push(format!(
                    "{} @ {} rows failed: {failure}",
                    cell.workload, cell.rows
                ));
            }
            cells.push(cell);
        }
    }

    for ((workload, rows), count) in &observed_counts {
        if !expected_matrix.contains(&(workload.clone(), *rows)) {
            disqualifications.push(format!(
                "unexpected fp64 cell observed: {workload} @ {rows}"
            ));
        }
        if *count > 1 {
            disqualifications.push(format!(
                "duplicate fp64 cell observed {count} times: {workload} @ {rows}"
            ));
        }
    }
    for (workload, rows) in expected_matrix {
        if !observed_counts.contains_key(&(workload.clone(), *rows)) {
            disqualifications.push(format!("missing fp64 cell: {workload} @ {rows}"));
        }
    }

    let expected_cells = expected_matrix.len();
    if cells.len() != expected_cells {
        disqualifications.push(format!(
            "observed {} fp64 cells, expected {expected_cells}",
            cells.len()
        ));
    }

    let speedups = cells
        .iter()
        .map(|cell| cell.speedup_median)
        .filter(|speedup| speedup.is_finite() && *speedup > 0.0)
        .collect::<Vec<_>>();
    let qualified = disqualifications.is_empty() && !cells.is_empty();
    let geomean_speedup = (qualified && !speedups.is_empty()).then(|| stats::geomean(&speedups));
    let min_speedup = qualified
        .then(|| speedups.iter().copied().reduce(f64::min))
        .flatten();

    Fp64CandidateSummary {
        multiplier,
        qualified,
        geomean_speedup,
        min_speedup,
        cells_observed: cells.len(),
        cells_expected: expected_cells,
        disqualifications,
        cells,
    }
}

fn summarize_cell(workload: &WorkloadResult) -> Fp64CalibrationCell {
    let mut failures = Vec::new();
    if !workload.dispatch_counter_captured {
        failures.push(
            workload
                .dispatch_counter_error
                .clone()
                .unwrap_or_else(|| "dispatch counter capture unavailable".to_owned()),
        );
    }
    if !workload.gpu_kernel_dispatched {
        failures.push("no credited GPU dispatch".to_owned());
    }
    if workload.gpu_kernel_dispatched
        && !workload.function_srf_kernel_dispatched
        && !workload.plan_selected
    {
        failures.push("credited GPU dispatch without Custom Scan plan selection".to_owned());
    }
    if workload.pg_accel_stock_exec_delta > 0 {
        failures.push(format!(
            "stock executor fallback delta {}",
            workload.pg_accel_stock_exec_delta
        ));
    }
    if !workload.speedup_median_vs_parallel.is_finite() || workload.speedup_median_vs_parallel < 1.0
    {
        failures.push(format!(
            "median speedup below parity: {:.4}x",
            workload.speedup_median_vs_parallel
        ));
    }

    let failure = (!failures.is_empty()).then(|| failures.join("; "));
    Fp64CalibrationCell {
        workload: workload.name.clone(),
        rows: workload.rows,
        speedup_median: workload.speedup_median_vs_parallel,
        speedup_mean: workload.speedup_vs_parallel,
        gpu_kernel_dispatched: workload.gpu_kernel_dispatched,
        plan_selected: workload.plan_selected,
        function_srf_kernel_dispatched: workload.function_srf_kernel_dispatched,
        dispatch_counter_captured: workload.dispatch_counter_captured,
        gpu_kernel_execution_delta: workload.gpu_kernel_execution_delta,
        pg_accel_stock_exec_delta: workload.pg_accel_stock_exec_delta,
        status: if failure.is_none() {
            Fp64CellStatus::Pass
        } else {
            Fp64CellStatus::Fail
        },
        failure,
    }
}

fn expected_matrix_for_sizes(sizes: &[usize]) -> BTreeSet<(String, usize)> {
    workloads::fp64_matrix::fp64_matrix_workload_names()
        .into_iter()
        .flat_map(|workload| sizes.iter().map(move |rows| (workload.to_owned(), *rows)))
        .collect()
}

fn ranked_qualified_candidates(candidates: &[Fp64CandidateSummary]) -> Vec<&Fp64CandidateSummary> {
    let mut ranked = candidates
        .iter()
        .filter(|candidate| candidate.qualified && candidate.geomean_speedup.is_some())
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| compare_candidate_rank(a, b));
    ranked
}

fn compare_candidate_rank(a: &Fp64CandidateSummary, b: &Fp64CandidateSummary) -> Ordering {
    let a_geo = a.geomean_speedup.unwrap_or(f64::NEG_INFINITY);
    let b_geo = b.geomean_speedup.unwrap_or(f64::NEG_INFINITY);
    if (a_geo - b_geo).abs() < 1.0e-12 {
        return compare_f64_ascending(a.multiplier, b.multiplier);
    }
    compare_f64_descending(a_geo, b_geo)
}

fn capture_fp64_disabled_proof(
    connection: &str,
    multiplier: f64,
    rows: usize,
    seed: u64,
) -> Result<Fp64ExplainProof, Box<dyn std::error::Error>> {
    let workload =
        Fp64MultiplierWorkload::new(Box::new(workloads::fp64_matrix::SortF64Keys), multiplier);
    runner::setup(connection, &workload, rows, seed)?;

    let plan_result = (|| -> Result<String, Box<dyn std::error::Error>> {
        let mut client = Client::connect(connection, NoTls)?;
        client.batch_execute("SELECT 1 FROM pg_accel_stats() LIMIT 1")?;
        client.batch_execute("SET pg_accel.enabled = on")?;
        client.batch_execute("SET pg_accel.fp64_enabled = false")?;
        client.batch_execute("SET max_parallel_workers_per_gather = DEFAULT")?;
        for sql in workload.pre_query_sql() {
            client.batch_execute(&sql)?;
        }
        let rows_out = client.query(
            &format!("EXPLAIN (VERBOSE, COSTS OFF) {}", workload.query_sql()),
            &[],
        )?;
        let mut plan = String::new();
        for row in rows_out {
            let line: &str = row.get(0);
            plan.push_str(line);
            plan.push('\n');
        }
        Ok(plan)
    })();

    let cleanup_result = runner::cleanup(connection, &workload);
    let plan = match (plan_result, cleanup_result) {
        (Ok(plan), Ok(())) => plan,
        (Err(plan_error), Ok(())) => return Err(plan_error),
        (Ok(_), Err(cleanup_error)) => return Err(cleanup_error),
        (Err(plan_error), Err(cleanup_error)) => {
            return Err(format!(
                "fp64_enabled=false EXPLAIN failed: {plan_error}; cleanup failed: {cleanup_error}"
            )
            .into());
        }
    };

    let custom_scan_selected = runner::plan_contains_custom_scan(&plan);
    Ok(Fp64ExplainProof {
        workload: workload.name().to_owned(),
        rows,
        multiplier,
        fp64_enabled: false,
        custom_scan_selected,
        plan,
    })
}

fn write_summary_artifacts(
    summary: &Fp64CalibrationSummary,
) -> Result<(), Box<dyn std::error::Error>> {
    let artifact_dir = PathBuf::from(&summary.artifact_dir);
    fs::create_dir_all(&artifact_dir)?;
    fs::write(
        artifact_dir.join("fp64_calibration_summary.json"),
        serde_json::to_string_pretty(summary)?,
    )?;
    fs::write(
        artifact_dir.join("fp64_calibration_summary.md"),
        summary.to_markdown(),
    )?;
    Ok(())
}

impl Fp64CalibrationSummary {
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# fp64 Cost Multiplier Calibration\n\n");
        let _ = writeln!(out, "- Artifact dir: `{}`", self.artifact_dir);
        let _ = writeln!(
            out,
            "- Sizes: `{}`",
            self.sizes
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        let _ = writeln!(
            out,
            "- Multipliers: `{}`",
            self.multipliers
                .iter()
                .map(|multiplier| format_multiplier(*multiplier))
                .collect::<Vec<_>>()
                .join(", ")
        );
        match self.selected_multiplier {
            Some(multiplier) => {
                let _ = writeln!(
                    out,
                    "- Selected: `{}` (geomean {:.4}x)",
                    format_multiplier(multiplier),
                    self.selected_geomean_speedup.unwrap_or(f64::NAN)
                );
            }
            None => out.push_str("- Selected: none\n"),
        }
        match self.runner_up_multiplier {
            Some(multiplier) => {
                let _ = writeln!(
                    out,
                    "- Runner-up: `{}` (geomean {:.4}x)",
                    format_multiplier(multiplier),
                    self.runner_up_geomean_speedup.unwrap_or(f64::NAN)
                );
            }
            None => out.push_str("- Runner-up: none\n"),
        }
        out.push('\n');

        out.push_str("## Candidate Summary\n\n");
        out.push_str("| Multiplier | Status | Geomean | Min Cell | Cells | Disqualifications |\n");
        out.push_str("|---|---|---:|---:|---:|---|\n");
        for candidate in &self.candidates {
            let status = if candidate.qualified {
                "qualified"
            } else {
                "disqualified"
            };
            let geomean = format_optional_speedup(candidate.geomean_speedup);
            let min = format_optional_speedup(candidate.min_speedup);
            let disq = if candidate.disqualifications.is_empty() {
                "-".to_owned()
            } else {
                candidate.disqualifications.join("<br>")
            };
            let _ = writeln!(
                out,
                "| `{}` | {status} | {geomean} | {min} | {}/{} | {} |",
                format_multiplier(candidate.multiplier),
                candidate.cells_observed,
                candidate.cells_expected,
                disq
            );
        }
        out.push('\n');

        out.push_str("## Parity-Close Cells\n\n");
        let _ = writeln!(
            out,
            "Cells at or below `{:.2}x` median speedup for the selected multiplier.\n",
            self.parity_close_speedup
        );
        if self.parity_close_cells.is_empty() {
            out.push_str("None.\n\n");
        } else {
            out.push_str("| Workload | Rows | Median Speedup | Mean Speedup | Dispatch |\n");
            out.push_str("|---|---:|---:|---:|---|\n");
            for cell in &self.parity_close_cells {
                let dispatch = if cell.function_srf_kernel_dispatched {
                    "function/SRF"
                } else if cell.plan_selected {
                    "Custom Scan"
                } else {
                    "counter-only"
                };
                let _ = writeln!(
                    out,
                    "| `{}` | {} | {:.4}x | {:.4}x | {} |",
                    cell.workload, cell.rows, cell.speedup_median, cell.speedup_mean, dispatch
                );
            }
            out.push('\n');
        }

        out.push_str("## fp64 Disabled EXPLAIN Proof\n\n");
        match &self.fp64_disabled_proof {
            Some(proof) => {
                let _ = writeln!(
                    out,
                    "`pg_accel.fp64_enabled=false` proof for `{}` at {} rows with multiplier `{}`: Custom Scan selected = `{}`.\n",
                    proof.workload,
                    proof.rows,
                    format_multiplier(proof.multiplier),
                    proof.custom_scan_selected
                );
                out.push_str("```text\n");
                out.push_str(&proof.plan);
                out.push_str("```\n");
            }
            None => out.push_str("No proof captured because no multiplier qualified.\n"),
        }
        out
    }

    #[must_use]
    pub fn selected_candidate(&self) -> Option<&Fp64CandidateSummary> {
        let selected = self.selected_multiplier?;
        self.candidates
            .iter()
            .find(|candidate| (candidate.multiplier - selected).abs() < f64::EPSILON)
    }
}

fn format_optional_speedup(speedup: Option<f64>) -> String {
    match speedup {
        Some(value) if value.is_finite() => format!("{value:.4}x"),
        _ => "-".to_owned(),
    }
}

fn compare_f64_ascending(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

fn compare_f64_descending(a: f64, b: f64) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

fn format_multiplier(multiplier: f64) -> String {
    if (multiplier.fract()).abs() < f64::EPSILON {
        format!("{multiplier:.0}")
    } else {
        format!("{multiplier}")
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_for(workloads: Vec<WorkloadResult>) -> BenchReport {
        BenchReport {
            hardware: None,
            gucs: None,
            methodology: crate::report::Methodology {
                iterations: 5,
                warmup: 1,
                row_scales: vec![100_000],
                ordering: "test".to_owned(),
                statistical_tests: Vec::new(),
                timing_mode: "raw-wallclock".to_owned(),
                cache_mode: "warm".to_owned(),
                harness_profile: "test".to_owned(),
            },
            workloads,
            artifact_dir: None,
            crashes: Vec::new(),
            postmaster_start_time: None,
        }
    }

    fn result(name: &str, rows: usize, accel_ms: f64, parallel_ms: f64) -> WorkloadResult {
        let iterations = (0..5)
            .map(|_| crate::report::IterationResult {
                accel_ms,
                parallel_ms,
                accel_first: false,
                cache_purge: crate::bench_model::CachePurgeState::default(),
                cache_state: crate::bench_model::CacheState::default(),
            })
            .collect::<Vec<_>>();
        let mut result = WorkloadResult::from_iterations(
            name.to_owned(),
            "test".to_owned(),
            "fp64_matrix".to_owned(),
            crate::report::classify_kernel(name),
            rows,
            iterations,
            true,
        );
        result.dispatch_counter_captured = true;
        result.gpu_kernel_dispatched = true;
        result.gpu_kernel_execution_delta = 5;
        result
    }

    fn expected(cells: &[(&str, usize)]) -> BTreeSet<(String, usize)> {
        cells
            .iter()
            .map(|(workload, rows)| ((*workload).to_owned(), *rows))
            .collect()
    }

    #[test]
    fn parse_multiplier_list_dedupes_and_sorts() {
        let parsed = parse_multiplier_list("32,16,32,24").expect("parse multipliers");
        assert_eq!(parsed, vec![16.0, 24.0, 32.0]);
    }

    #[test]
    fn parse_multiplier_list_rejects_out_of_range() {
        assert!(parse_multiplier_list("16,65").is_err());
        assert!(parse_multiplier_list("0.5").is_err());
    }

    #[test]
    fn sizes_without_cap_returns_bounded_smoke_matrix() {
        let sizes = sizes_with_optional_cap(None).expect("smoke sizes");
        assert_eq!(sizes, workloads::fp64_matrix::FP64_DEFAULT_ROW_SCALES);
    }

    #[test]
    fn explicit_max_size_expands_to_capped_canonical_matrix() {
        let sizes = sizes_with_optional_cap(Some("1B")).expect("full matrix sizes");
        assert_eq!(sizes, workloads::fp64_matrix::FP64_MATRIX_SIZES);
    }

    #[test]
    fn candidate_disqualifies_sub_parity_cell() {
        let good = result("reduce_f64_sum", 100_000, 10.0, 20.0);
        let bad = result("sort_f64_keys", 100_000, 20.0, 10.0);
        let matrix = expected(&[("reduce_f64_sum", 100_000), ("sort_f64_keys", 100_000)]);
        let candidate = summarize_candidate(32.0, &[report_for(vec![good, bad])], &matrix);
        assert!(!candidate.qualified);
        assert!(
            candidate
                .disqualifications
                .iter()
                .any(|reason| reason.contains("median speedup below parity"))
        );
    }

    #[test]
    fn candidate_disqualifies_missing_dispatch() {
        let mut row = result("reduce_f64_sum", 100_000, 10.0, 20.0);
        row.gpu_kernel_dispatched = false;
        let matrix = expected(&[("reduce_f64_sum", 100_000)]);
        let candidate = summarize_candidate(32.0, &[report_for(vec![row])], &matrix);
        assert!(!candidate.qualified);
        assert!(
            candidate
                .disqualifications
                .iter()
                .any(|reason| reason.contains("no credited GPU dispatch"))
        );
    }

    #[test]
    fn candidate_geomean_uses_qualified_cells() {
        let a = result("reduce_f64_sum", 100_000, 10.0, 20.0);
        let b = result("sort_f64_keys", 100_000, 20.0, 80.0);
        let matrix = expected(&[("reduce_f64_sum", 100_000), ("sort_f64_keys", 100_000)]);
        let candidate = summarize_candidate(32.0, &[report_for(vec![a, b])], &matrix);
        assert!(candidate.qualified);
        let geomean = candidate.geomean_speedup.expect("geomean");
        assert!((geomean - (2.0_f64 * 4.0).sqrt()).abs() < 1.0e-12);
    }

    #[test]
    fn candidate_disqualifies_counter_delta_without_custom_scan() {
        let mut row = result("reduce_f64_sum", 100_000, 10.0, 20.0);
        row.plan_selected = false;
        row.function_srf_kernel_dispatched = false;
        let matrix = expected(&[("reduce_f64_sum", 100_000)]);
        let candidate = summarize_candidate(32.0, &[report_for(vec![row])], &matrix);
        assert!(!candidate.qualified);
        assert!(
            candidate
                .disqualifications
                .iter()
                .any(|reason| reason.contains("without Custom Scan plan selection"))
        );
    }

    #[test]
    fn candidate_disqualifies_missing_and_duplicate_cells() {
        let a = result("reduce_f64_sum", 100_000, 10.0, 20.0);
        let b = result("reduce_f64_sum", 100_000, 10.0, 20.0);
        let matrix = expected(&[("reduce_f64_sum", 100_000), ("sort_f64_keys", 100_000)]);
        let candidate = summarize_candidate(32.0, &[report_for(vec![a, b])], &matrix);
        assert!(!candidate.qualified);
        assert!(
            candidate
                .disqualifications
                .iter()
                .any(|reason| reason.contains("duplicate fp64 cell observed"))
        );
        assert!(
            candidate
                .disqualifications
                .iter()
                .any(|reason| reason.contains("missing fp64 cell: sort_f64_keys @ 100000"))
        );
        assert!(
            candidate.geomean_speedup.is_none(),
            "disqualified candidates should not publish a headline geomean"
        );
    }

    #[test]
    fn ranking_tie_breaks_to_smallest_multiplier() {
        let lower = Fp64CandidateSummary {
            multiplier: 24.0,
            qualified: true,
            geomean_speedup: Some(2.0),
            min_speedup: Some(2.0),
            cells_observed: 1,
            cells_expected: 1,
            disqualifications: Vec::new(),
            cells: Vec::new(),
        };
        let higher = Fp64CandidateSummary {
            multiplier: 32.0,
            qualified: true,
            geomean_speedup: Some(2.0),
            min_speedup: Some(2.0),
            cells_observed: 1,
            cells_expected: 1,
            disqualifications: Vec::new(),
            cells: Vec::new(),
        };
        let candidates = [higher, lower];
        let ranked = ranked_qualified_candidates(&candidates);
        assert!((ranked[0].multiplier - 24.0).abs() < f64::EPSILON);
    }

    struct DummyWorkload;

    impl Workload for DummyWorkload {
        fn name(&self) -> &'static str {
            "dummy_fp64"
        }

        fn description(&self) -> &'static str {
            "dummy fp64 workload"
        }

        fn setup_sql(&self, rows: usize) -> Vec<String> {
            vec![format!("SELECT {rows}")]
        }

        fn pre_query_sql(&self) -> Vec<String> {
            vec!["SET work_mem = '4MB'".to_owned()]
        }

        fn query_sql(&self) -> String {
            "SELECT 42".to_owned()
        }

        fn baseline_query_sql(&self) -> Option<String> {
            Some("SELECT 41".to_owned())
        }

        fn row_scales(&self) -> &'static [usize] {
            &[10, 20]
        }

        fn cleanup_sql(&self) -> Vec<String> {
            vec!["SELECT 0".to_owned()]
        }
    }

    #[test]
    fn multiplier_workload_delegates_contract_and_prepends_cost_setting() {
        let workload = Fp64MultiplierWorkload::new(Box::new(DummyWorkload), 16.5);
        assert_eq!(workload.name(), "dummy_fp64");
        assert_eq!(workload.description(), "dummy fp64 workload");
        assert_eq!(workload.setup_sql(123), ["SELECT 123"]);
        assert_eq!(
            workload.pre_query_sql(),
            [
                "SET pg_accel.soft_fp64_cost_multiplier = 16.5",
                "SET work_mem = '4MB'",
            ]
        );
        assert_eq!(workload.query_sql(), "SELECT 42");
        assert_eq!(workload.baseline_query_sql().as_deref(), Some("SELECT 41"));
        assert_eq!(workload.row_scales(), &[10, 20]);
        assert_eq!(workload.cleanup_sql(), ["SELECT 0"]);
    }

    #[test]
    fn multiplier_and_size_parsers_reject_malformed_empty_and_nonfinite_inputs() {
        for raw in ["", ",,", "abc", "NaN", "inf", "-inf", "0", "65"] {
            assert!(parse_multiplier_list(raw).is_err(), "raw={raw:?}");
        }
        assert_eq!(
            parse_multiplier_list("1, 64, 1.0").expect("boundary multipliers"),
            vec![1.0, 64.0]
        );
        assert!(sizes_with_optional_cap(Some("not-a-size")).is_err());
        assert!(sizes_with_optional_cap(Some("10k")).is_err());
        assert_eq!(
            sizes_with_optional_cap(Some("1M")).expect("1M cap"),
            vec![100_000, 1_000_000]
        );
    }

    #[test]
    fn size_groups_and_expected_matrix_follow_canonical_workload_contract() {
        let grouped = group_sizes_by_iterations(&[100_000, 1_000_000_000, 1_000_000]);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0], (3, vec![1_000_000_000]));
        assert_eq!(grouped[1], (5, vec![100_000, 1_000_000]));

        let matrix = expected_matrix_for_sizes(&[100_000, 1_000_000]);
        let names = workloads::fp64_matrix::fp64_matrix_workload_names();
        assert_eq!(matrix.len(), names.len() * 2);
        for name in names {
            assert!(matrix.contains(&(name.to_owned(), 100_000)));
            assert!(matrix.contains(&(name.to_owned(), 1_000_000)));
        }
    }

    #[test]
    fn candidate_records_crashes_unexpected_cells_and_counter_failures() {
        let mut row = result("unexpected", 100_000, 10.0, 20.0);
        row.dispatch_counter_captured = false;
        row.dispatch_counter_error = Some("stats unavailable".to_owned());
        row.pg_accel_stock_exec_delta = 2;
        let mut report = report_for(vec![row]);
        report.crashes.push(crate::report::CrashedScale {
            workload: "reduce_f64_sum".to_owned(),
            rows: 100_000,
            error: "backend reset".to_owned(),
            repro_command: None,
            plan_snippet_artifact: None,
            correctness_diff_artifact: None,
            log_tail_artifacts: Vec::new(),
        });
        let matrix = expected(&[("reduce_f64_sum", 100_000)]);
        let candidate = summarize_candidate(12.0, &[report], &matrix);
        assert!(!candidate.qualified);
        let reasons = candidate.disqualifications.join(" | ");
        assert!(reasons.contains("crashed: backend reset"));
        assert!(reasons.contains("unexpected fp64 cell observed"));
        assert!(reasons.contains("stats unavailable"));
        assert!(reasons.contains("stock executor fallback delta 2"));
        assert!(reasons.contains("missing fp64 cell"));
        assert!(!reasons.contains("observed 1 fp64 cells, expected 1"));
    }

    #[test]
    fn summarize_cell_uses_default_counter_error_and_rejects_nonfinite_speedup() {
        let mut row = result("reduce_f64_sum", 100_000, 10.0, 20.0);
        row.dispatch_counter_captured = false;
        row.dispatch_counter_error = None;
        row.speedup_median_vs_parallel = f64::NAN;
        let cell = summarize_cell(&row);
        assert_eq!(cell.status, Fp64CellStatus::Fail);
        let failure = cell.failure.expect("cell failure");
        assert!(failure.contains("dispatch counter capture unavailable"));
        assert!(failure.contains("median speedup below parity: NaNx"));
    }

    fn calibration_cell(
        workload: &str,
        speedup: f64,
        plan_selected: bool,
        function_dispatch: bool,
    ) -> Fp64CalibrationCell {
        Fp64CalibrationCell {
            workload: workload.to_owned(),
            rows: 100_000,
            speedup_median: speedup,
            speedup_mean: speedup + 0.05,
            gpu_kernel_dispatched: true,
            plan_selected,
            function_srf_kernel_dispatched: function_dispatch,
            dispatch_counter_captured: true,
            gpu_kernel_execution_delta: 3,
            pg_accel_stock_exec_delta: 0,
            status: Fp64CellStatus::Pass,
            failure: None,
        }
    }

    fn candidate_summary(
        multiplier: f64,
        qualified: bool,
        geomean: Option<f64>,
        disqualifications: Vec<String>,
    ) -> Fp64CandidateSummary {
        Fp64CandidateSummary {
            multiplier,
            qualified,
            geomean_speedup: geomean,
            min_speedup: geomean,
            cells_observed: 1,
            cells_expected: 1,
            disqualifications,
            cells: Vec::new(),
        }
    }

    #[test]
    fn candidate_ranking_orders_geomean_and_filters_unqualified_entries() {
        let candidates = [
            candidate_summary(8.0, false, Some(99.0), vec!["failed".to_owned()]),
            candidate_summary(32.0, true, Some(3.0), Vec::new()),
            candidate_summary(16.0, true, Some(2.0), Vec::new()),
            candidate_summary(4.0, true, None, Vec::new()),
        ];
        let ranked = ranked_qualified_candidates(&candidates);
        assert_eq!(
            ranked
                .iter()
                .map(|candidate| candidate.multiplier)
                .collect::<Vec<_>>(),
            [32.0, 16.0]
        );
        assert_eq!(
            compare_candidate_rank(&candidates[1], &candidates[2]),
            Ordering::Less
        );
        assert_eq!(compare_f64_ascending(f64::NAN, 1.0), Ordering::Equal);
        assert_eq!(compare_f64_descending(f64::NAN, 1.0), Ordering::Equal);
    }

    #[test]
    fn markdown_and_json_artifacts_cover_selected_runner_up_and_dispatch_labels() {
        let temp = std::env::temp_dir().join(format!(
            "pg_accel_fp64_summary_{}_{}",
            std::process::id(),
            unix_now()
        ));
        let summary = Fp64CalibrationSummary {
            generated_at_unix_seconds: unix_now(),
            artifact_dir: temp.display().to_string(),
            sizes: vec![100_000, 1_000_000],
            multipliers: vec![16.0, 24.5, 32.0],
            selected_multiplier: Some(16.0),
            runner_up_multiplier: Some(24.5),
            selected_geomean_speedup: Some(2.0),
            runner_up_geomean_speedup: Some(1.5),
            parity_close_speedup: PARITY_CLOSE_SPEEDUP,
            parity_close_cells: vec![
                calibration_cell("function", 1.05, false, true),
                calibration_cell("custom", 1.08, true, false),
                calibration_cell("counter", 1.10, false, false),
            ],
            candidates: vec![
                candidate_summary(16.0, true, Some(2.0), Vec::new()),
                candidate_summary(
                    24.5,
                    false,
                    None,
                    vec!["cell failed".to_owned(), "missing dispatch".to_owned()],
                ),
            ],
            fp64_disabled_proof: Some(Fp64ExplainProof {
                workload: "sort_f64_keys".to_owned(),
                rows: 100_000,
                multiplier: 16.0,
                fp64_enabled: false,
                custom_scan_selected: false,
                plan: "Seq Scan on bench_fp64\n".to_owned(),
            }),
        };

        let markdown = summary.to_markdown();
        assert!(markdown.contains("Selected: `16` (geomean 2.0000x)"));
        assert!(markdown.contains("Runner-up: `24.5` (geomean 1.5000x)"));
        assert!(markdown.contains("function/SRF"));
        assert!(markdown.contains("Custom Scan"));
        assert!(markdown.contains("counter-only"));
        assert!(markdown.contains("cell failed<br>missing dispatch"));
        assert!(markdown.contains("Seq Scan on bench_fp64"));
        assert_eq!(
            summary
                .selected_candidate()
                .map(|candidate| candidate.multiplier),
            Some(16.0)
        );

        write_summary_artifacts(&summary).expect("summary artifacts");
        let json =
            fs::read_to_string(temp.join("fp64_calibration_summary.json")).expect("summary json");
        assert!(json.contains("\"selected_multiplier\": 16.0"));
        let persisted_markdown =
            fs::read_to_string(temp.join("fp64_calibration_summary.md")).expect("summary markdown");
        assert_eq!(persisted_markdown, markdown);
        fs::remove_dir_all(&temp).expect("remove summary test directory");
    }

    #[test]
    fn markdown_without_qualified_candidate_uses_explicit_empty_sections() {
        let summary = Fp64CalibrationSummary {
            generated_at_unix_seconds: 0,
            artifact_dir: "none".to_owned(),
            sizes: vec![100_000],
            multipliers: vec![16.0],
            selected_multiplier: None,
            runner_up_multiplier: None,
            selected_geomean_speedup: None,
            runner_up_geomean_speedup: None,
            parity_close_speedup: PARITY_CLOSE_SPEEDUP,
            parity_close_cells: Vec::new(),
            candidates: vec![candidate_summary(16.0, false, None, Vec::new())],
            fp64_disabled_proof: None,
        };
        let markdown = summary.to_markdown();
        assert!(markdown.contains("Selected: none"));
        assert!(markdown.contains("Runner-up: none"));
        assert!(markdown.contains("None."));
        assert!(markdown.contains("No proof captured"));
        assert!(summary.selected_candidate().is_none());
        assert_eq!(format_optional_speedup(None), "-");
        assert_eq!(format_optional_speedup(Some(f64::NAN)), "-");
        assert_eq!(format_optional_speedup(Some(1.23456)), "1.2346x");
        assert_eq!(format_multiplier(16.0), "16");
        assert_eq!(format_multiplier(16.25), "16.25");
    }

    #[test]
    fn empty_size_calibration_persists_an_explicit_unqualified_summary_without_running_cells() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock after epoch")
            .as_nanos();
        let artifact_root = std::env::temp_dir().join(format!(
            "pg_accel_fp64_empty_matrix_{}_{nonce}",
            std::process::id()
        ));
        let options = Fp64CalibrationOptions {
            multipliers: vec![16.0],
            sizes: Vec::new(),
            warmup: 5,
            seed: 42,
            timing_mode: runner::TimingMode::RawWallClock,
            cache_mode: runner::CacheMode::Warm,
            guc_profile: None,
            skip_guc_verify: false,
            capture_plans: false,
            artifact_root: artifact_root.clone(),
        };

        let summary = run_fp64_calibration("unused", &options)
            .expect("an empty size matrix must not construct database run cells");
        assert_eq!(summary.sizes, Vec::<usize>::new());
        assert_eq!(summary.multipliers, vec![16.0]);
        assert_eq!(summary.candidates.len(), 1);
        assert!((summary.candidates[0].multiplier - 16.0).abs() < f64::EPSILON);
        assert!(!summary.candidates[0].qualified);
        assert_eq!(summary.candidates[0].cells_observed, 0);
        assert_eq!(summary.candidates[0].cells_expected, 0);
        assert!(summary.selected_candidate().is_none());
        assert!(summary.fp64_disabled_proof.is_none());
        assert!(
            artifact_root
                .join("fp64_calibration_summary.json")
                .is_file()
        );
        assert!(artifact_root.join("fp64_calibration_summary.md").is_file());

        fs::remove_dir_all(artifact_root).expect("remove empty calibration artifacts");
    }
}
