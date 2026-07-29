#![allow(
    clippy::needless_continue,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::literal_string_with_formatting_args,
    clippy::too_many_lines
)]
// Benchmark configuration and subprocess failures are user-facing; retain
// their source error instead of replacing it with context-free text.
#![deny(clippy::map_err_ignore)]

mod artifacts;
mod bench_model;
mod config;
mod explain_audit;
mod fp64_calibration;
mod h3_protection_test;
mod integration_connection;
mod parallel_stress_test;
mod plan_shape_test;
mod report;
#[cfg(test)]
mod resident_concurrency_test;
mod resume;
mod runner;
mod stats;
mod workloads;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::report::BenchReport;

const DEFAULT_CONNECTION: &str = "host=localhost dbname=postgres";

type ResumeWorkloadCell = (Box<dyn workloads::Workload>, usize);

#[derive(Parser)]
#[command(name = "pg_accel_bench", about = "Benchmark harness for pg_accel")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create benchmark tables and populate with test data.
    Setup {
        /// Number of rows to generate.
        #[arg(long, default_value_t = 1_000_000)]
        rows: usize,

        /// Seed for deterministic random data generation.
        #[arg(long, default_value_t = 42)]
        seed: u64,

        /// PostgreSQL connection string.
        #[arg(long, default_value = DEFAULT_CONNECTION)]
        connection: String,

        /// Workload name (omit to set up all workloads).
        #[arg(long)]
        workload: Option<String>,

        /// Filter workloads by category (comma-separated, e.g. `gpu_spatial,ssbm`).
        #[arg(long)]
        category: Option<String>,
    },

    /// Run benchmarks and print results.
    Run {
        /// Workload name (omit to run all workloads).
        #[arg(long)]
        workload: Option<String>,

        /// Filter workloads by category (comma-separated, e.g. `gpu_spatial,ssbm`).
        #[arg(long)]
        category: Option<String>,

        /// Number of iterations per workload (minimum 10 for statistical validity).
        #[arg(long, default_value_t = 10)]
        iterations: usize,

        /// Number of warmup iterations (excluded from statistics). Must
        /// be at least 5 for warm-cache measurements to amortize shader
        /// compilation and kernel-launch jitter.
        #[arg(long, default_value_t = 5)]
        warmup: usize,

        /// Seed for deterministic random data generation.
        #[arg(long, default_value_t = 42)]
        seed: u64,

        /// PostgreSQL connection string.
        #[arg(long, default_value = DEFAULT_CONNECTION)]
        connection: String,

        /// Output format: `markdown`, `json`, or `csv`.
        #[arg(long, default_value = "markdown")]
        format: ReportFormat,

        /// Validate SQL and print workload plan without executing benchmarks.
        #[arg(long)]
        dry_run: bool,

        /// Apply the publication GUC profile: 16 GB `shared_buffers`, 512 MB
        /// `work_mem`, 48 GB `effective_cache_size`,
        /// `max_worker_processes=16`, `max_parallel_workers=12`,
        /// `max_parallel_workers_per_gather=8`, and 2 GB
        /// `maintenance_work_mem`. The two postmaster settings require a full
        /// PG restart before publishable evidence can pass verification.
        #[arg(long)]
        realistic_gucs: bool,

        /// Capture `EXPLAIN (ANALYZE, VERBOSE, BUFFERS)` output once per
        /// workload/scale to `benchmarks/plans.txt`.
        #[arg(long)]
        capture_plans: bool,

        /// Timing mode: `raw` (client-side `Instant::now()` wall clock,
        /// no instrumentation overhead — **default**, used for
        /// publication-quality numbers), `explain` (EXPLAIN ANALYZE,
        /// historical default; penalizes parallel plans vs Custom Scan),
        /// or `both` (capture both per-iteration for audit).
        #[arg(long, default_value = "raw")]
        timing: TimingArg,

        /// Cache cleanliness mode: `warm` (default — after ≥5 warmup
        /// iterations), `cold` (`sync && purge` before every timed
        /// iteration), or `both` (cold+warm columns side-by-side).
        ///
        /// `DISCARD ALL` does not clear the OS page cache. Use `cold` or
        /// `both` for any report published externally.
        #[arg(long, default_value = "warm")]
        cache_mode: CacheModeArg,

        /// Skip the postmaster-GUC mismatch hard-fail.
        /// By default the harness refuses to run if `shared_buffers` or
        /// any other `PGC_POSTMASTER` setting drifts from the requested
        /// profile — because publishing a settings table that doesn't
        /// match reality is worse than no table at all. This flag is
        /// only intended for developer iteration.
        #[arg(long)]
        skip_guc_verify: bool,

        /// Directory for benchmark artifacts. If omitted, a fresh
        /// `benchmarks/artifacts/run-<timestamp>` directory is created.
        #[arg(long)]
        artifacts_dir: Option<PathBuf>,
    },

    /// Reproduce one workload at one row scale with full crash artifacts.
    CrashRepro {
        /// Workload name to run.
        #[arg(long)]
        workload: String,

        /// Row scale to reproduce (for example, 1000000).
        #[arg(long)]
        rows: usize,

        /// Number of measured iterations. Defaults to 1 for fast crash repro.
        #[arg(long, default_value_t = 1)]
        iterations: usize,

        /// Number of warmup iterations. Defaults to 0 for fast crash repro.
        #[arg(long, default_value_t = 0)]
        warmup: usize,

        /// Seed for deterministic random data generation.
        #[arg(long, default_value_t = 42)]
        seed: u64,

        /// PostgreSQL connection string.
        #[arg(long, default_value = DEFAULT_CONNECTION)]
        connection: String,

        /// Output format: `markdown`, `json`, or `csv`.
        #[arg(long, default_value = "markdown")]
        format: ReportFormat,

        /// Apply the realistic GUC profile before running.
        #[arg(long)]
        realistic_gucs: bool,

        /// Capture `EXPLAIN (ANALYZE, VERBOSE, BUFFERS)` output for this
        /// workload/scale to `plans.txt` inside the artifact directory.
        #[arg(long)]
        capture_plans: bool,

        /// Timing mode: `raw`, `explain`, or `both`.
        #[arg(long, default_value = "raw")]
        timing: TimingArg,

        /// Cache cleanliness mode: `warm`, `cold`, or `both`.
        #[arg(long, default_value = "warm")]
        cache_mode: CacheModeArg,

        /// Source distribution for report speedup labels.
        /// Skip the postmaster-GUC mismatch hard-fail.
        #[arg(long)]
        skip_guc_verify: bool,

        /// Directory for repro artifacts. If omitted, a fresh
        /// `benchmarks/artifacts/crash-repro-<timestamp>` directory is created.
        #[arg(long)]
        artifacts_dir: Option<PathBuf>,
    },

    /// Retry crashed cells from a saved benchmark artifact directory.
    #[command(visible_alias = "retry-artifacts")]
    Resume {
        /// Source artifact directory containing resume_audit_manifest.json.
        #[arg(long)]
        artifacts_dir: PathBuf,

        /// PostgreSQL connection string for the retry run.
        #[arg(long, default_value = DEFAULT_CONNECTION)]
        connection: String,

        /// Directory for retry artifacts. If omitted, a fresh
        /// `benchmarks/artifacts/resume-<timestamp>` directory is created.
        #[arg(long)]
        output_dir: Option<PathBuf>,

        /// Output format: `markdown`, `json`, or `csv`.
        #[arg(long, default_value = "markdown")]
        format: ReportFormat,

        /// Print the retry plan without executing PostgreSQL work.
        #[arg(long)]
        dry_run: bool,
    },

    /// Print a previously-stored report (reads JSON from stdin).
    Report {
        /// Output format: `markdown` or `json`.
        #[arg(long, default_value = "markdown")]
        format: ReportFormat,
    },

    /// Validate workload SQL without connecting to PostgreSQL.
    ///
    /// Checks structure, balanced parentheses, table references,
    /// cleanup completeness, and extension requirements.
    Validate {
        /// Workload name (omit to validate all workloads).
        #[arg(long)]
        workload: Option<String>,

        /// Filter workloads by category (comma-separated, e.g. `gpu_spatial,ssbm`).
        #[arg(long)]
        category: Option<String>,

        /// Number of rows (used for setup_sql generation).
        #[arg(long, default_value_t = 1_000_000)]
        rows: usize,
    },

    /// Run the Phase 9 EXPLAIN (VERBOSE) audit matrix.
    ///
    /// For every row, run `EXPLAIN (VERBOSE) <query>` and assert that a
    /// `Custom Scan (GpuAccel...)` node appears underneath a `Gather` /
    /// `Gather Merge`. Row expectations are defined in
    /// `pg_accel_bench/src/explain_audit.rs`.
    /// Exits non-zero iff any `RequiredToday` row fails.
    ExplainAudit {
        /// PostgreSQL connection string.
        #[arg(long, default_value = DEFAULT_CONNECTION)]
        connection: String,
    },

    /// Run the exhaustive Phase 6 spatial, H3, and raster domain gate.
    ///
    /// Every registered release-domain workload is checked against its native
    /// extension oracle with plan, dispatch-counter, and correctness artifacts.
    Phase6Gate {
        /// PostgreSQL connection string.
        #[arg(long, default_value = DEFAULT_CONNECTION)]
        connection: String,

        /// Directory for the aggregate Phase 6 evidence bundle. If omitted, a
        /// fresh `benchmarks/artifacts/phase6-gate-<timestamp>` directory is used.
        #[arg(long)]
        artifacts_dir: Option<PathBuf>,
    },

    /// Run the Phase 9 operator-breadth release gate at the canonical 10K scale.
    ///
    /// Every typed Phase 9 lane is checked against an exact result oracle and
    /// must produce its planner-reported native decline with zero GPU dispatch.
    Phase9Gate {
        /// PostgreSQL connection string.
        #[arg(long, default_value = DEFAULT_CONNECTION)]
        connection: String,

        /// Directory for the aggregate Phase 9 evidence bundle. If omitted, a
        /// fresh `benchmarks/artifacts/phase9-gate-<timestamp>` directory is used.
        #[arg(long)]
        artifacts_dir: Option<PathBuf>,
    },

    /// Run the fixed qualified-Metal benchmark performance ratchet.
    ///
    /// The exact winner cells, row scale, sampling counts, seed, timing mode,
    /// cache policy, and per-lane speedup floors are immutable repository
    /// policy. The command requires a release build and writes a complete
    /// benchmark evidence bundle before returning success.
    MetalShipGate {
        /// PostgreSQL connection string.
        #[arg(long, default_value = DEFAULT_CONNECTION)]
        connection: String,

        /// Directory for the Metal ship-gate evidence bundle. If omitted, a
        /// fresh `benchmarks/artifacts/metal-ship-gate-<timestamp>` directory is used.
        #[arg(long)]
        artifacts_dir: Option<PathBuf>,
    },

    /// Run the pg_accel install/provenance gate without benchmarks.
    ///
    /// Checks that the live backend is loading the expected extension SQL,
    /// control metadata, installed binary, mapped dylib, and device-limit
    /// smoke before accepting benchmark or audit evidence.
    #[command(visible_alias = "provenance-gate")]
    Provenance {
        /// PostgreSQL connection string.
        #[arg(long, default_value = DEFAULT_CONNECTION)]
        connection: String,
    },

    /// Sweep pg_accel.soft_fp64_cost_multiplier across the full fp64 matrix.
    ///
    /// Runs the immutable 8-workload fp64 matrix at the canonical sizes,
    /// disqualifies any candidate with a sub-parity or non-GPU cell, then
    /// writes selected/runner-up/parity-close/proof artifacts.
    Fp64Calibrate {
        /// PostgreSQL connection string.
        #[arg(long, default_value = DEFAULT_CONNECTION)]
        connection: String,

        /// Comma-separated candidate multipliers to sweep.
        #[arg(long, default_value = "16,24,32,40,48,56,64")]
        multipliers: String,

        /// Explicit row-size cap for the fp64 calibration matrix, e.g. 100k,
        /// 1M, 10M, or 1B.
        ///
        /// Omitted runs use the bounded 100K smoke scale. Pass a cap to opt
        /// into larger calibration evidence.
        #[arg(long)]
        max_size: Option<String>,

        /// Number of warmup iterations before each measured fp64 cell.
        #[arg(long, default_value_t = 5)]
        warmup: usize,

        /// Seed for deterministic random data generation.
        #[arg(long, default_value_t = 42)]
        seed: u64,

        /// Apply the realistic GUC profile before running.
        #[arg(long)]
        realistic_gucs: bool,

        /// Capture full EXPLAIN ANALYZE plans for every cell.
        #[arg(long)]
        capture_plans: bool,

        /// Timing mode: `raw`, `explain`, or `both`.
        #[arg(long, default_value = "raw")]
        timing: TimingArg,

        /// Cache cleanliness mode: `warm`, `cold`, or `both`.
        #[arg(long, default_value = "warm")]
        cache_mode: CacheModeArg,

        /// Skip the postmaster-GUC mismatch hard-fail.
        #[arg(long)]
        skip_guc_verify: bool,

        /// Directory for calibration artifacts.
        #[arg(long)]
        artifacts_dir: Option<PathBuf>,
    },
}

#[derive(Clone, Debug)]
enum ReportFormat {
    Markdown,
    Json,
    Csv,
}

/// CLI wrapper around [`runner::TimingMode`] so clap can parse `--timing raw`
/// without us leaking the runner type into the CLI surface.
#[derive(Clone, Debug)]
enum TimingArg {
    Explain,
    Raw,
    Both,
}

impl std::fmt::Display for TimingArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Explain => write!(f, "explain"),
            Self::Raw => write!(f, "raw"),
            Self::Both => write!(f, "both"),
        }
    }
}

impl std::str::FromStr for TimingArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "explain" | "explain-analyze" | "analyze" => Ok(Self::Explain),
            "raw" | "wall" | "wall-clock" => Ok(Self::Raw),
            "both" => Ok(Self::Both),
            other => Err(format!("unknown timing mode: {other}")),
        }
    }
}

impl From<&TimingArg> for runner::TimingMode {
    fn from(value: &TimingArg) -> Self {
        match value {
            TimingArg::Explain => Self::ExplainAnalyze,
            TimingArg::Raw => Self::RawWallClock,
            TimingArg::Both => Self::Both,
        }
    }
}

/// CLI wrapper for cold/warm/both cache mode.
#[derive(Clone, Debug)]
enum CacheModeArg {
    Cold,
    Warm,
    Both,
}

impl std::fmt::Display for CacheModeArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cold => write!(f, "cold"),
            Self::Warm => write!(f, "warm"),
            Self::Both => write!(f, "both"),
        }
    }
}

impl std::str::FromStr for CacheModeArg {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "cold" => Ok(Self::Cold),
            "warm" => Ok(Self::Warm),
            "both" => Ok(Self::Both),
            other => Err(format!("unknown cache mode: {other}")),
        }
    }
}

impl From<&CacheModeArg> for runner::CacheMode {
    fn from(value: &CacheModeArg) -> Self {
        match value {
            CacheModeArg::Cold => Self::Cold,
            CacheModeArg::Warm => Self::Warm,
            CacheModeArg::Both => Self::Both,
        }
    }
}

impl std::fmt::Display for ReportFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Markdown => write!(f, "markdown"),
            Self::Json => write!(f, "json"),
            Self::Csv => write!(f, "csv"),
        }
    }
}

impl std::str::FromStr for ReportFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "markdown" | "md" => Ok(Self::Markdown),
            "json" => Ok(Self::Json),
            "csv" => Ok(Self::Csv),
            other => Err(format!("unknown format: {other}")),
        }
    }
}

fn main() {
    let cli = Cli::parse();

    if let Err(e) = dispatch(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn dispatch(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::Setup {
            rows,
            seed,
            connection,
            workload,
            category,
        } => cmd_setup(
            &connection,
            rows,
            seed,
            workload.as_deref(),
            category.as_deref(),
        ),
        Command::Run {
            workload,
            category,
            iterations,
            warmup,
            seed,
            connection,
            format,
            dry_run,
            realistic_gucs,
            capture_plans,
            timing,
            cache_mode,
            skip_guc_verify,
            artifacts_dir,
        } => {
            if dry_run {
                cmd_dry_run(workload.as_deref(), category.as_deref())
            } else {
                cmd_run(
                    &connection,
                    workload.as_deref(),
                    category.as_deref(),
                    iterations,
                    warmup,
                    seed,
                    &format,
                    realistic_gucs,
                    capture_plans,
                    &timing,
                    &cache_mode,
                    skip_guc_verify,
                    artifacts_dir,
                )
            }
        }
        Command::CrashRepro {
            workload,
            rows,
            iterations,
            warmup,
            seed,
            connection,
            format,
            realistic_gucs,
            capture_plans,
            timing,
            cache_mode,
            skip_guc_verify,
            artifacts_dir,
        } => cmd_crash_repro(
            &connection,
            &workload,
            rows,
            iterations,
            warmup,
            seed,
            &format,
            realistic_gucs,
            capture_plans,
            &timing,
            &cache_mode,
            skip_guc_verify,
            artifacts_dir,
        ),
        Command::Resume {
            artifacts_dir,
            connection,
            output_dir,
            format,
            dry_run,
        } => cmd_resume(&connection, &artifacts_dir, output_dir, &format, dry_run),
        Command::Report { format } => cmd_report(&format),
        Command::Validate {
            workload,
            category,
            rows,
        } => cmd_validate(workload.as_deref(), category.as_deref(), rows),
        Command::ExplainAudit { connection } => cmd_explain_audit(&connection),
        Command::Phase6Gate {
            connection,
            artifacts_dir,
        } => cmd_phase6_gate(&connection, artifacts_dir),
        Command::Phase9Gate {
            connection,
            artifacts_dir,
        } => cmd_phase9_gate(&connection, artifacts_dir),
        Command::MetalShipGate {
            connection,
            artifacts_dir,
        } => cmd_metal_ship_gate(&connection, artifacts_dir),
        Command::Provenance { connection } => cmd_provenance(&connection),
        Command::Fp64Calibrate {
            connection,
            multipliers,
            max_size,
            warmup,
            seed,
            realistic_gucs,
            capture_plans,
            timing,
            cache_mode,
            skip_guc_verify,
            artifacts_dir,
        } => cmd_fp64_calibrate(
            &connection,
            &multipliers,
            max_size.as_deref(),
            warmup,
            seed,
            realistic_gucs,
            capture_plans,
            &timing,
            &cache_mode,
            skip_guc_verify,
            artifacts_dir,
        ),
    }
}

fn cmd_provenance(connection: &str) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("[provenance] checking pg_accel install provenance on {connection}");
    match runner::verify_pg_accel_provenance(connection) {
        Ok(()) => {
            eprintln!("[provenance] success: live backend passed the pg_accel provenance gate");
            Ok(())
        }
        Err(e) => {
            eprintln!(
                "[provenance] failure: live backend did not pass the pg_accel provenance gate"
            );
            Err(e)
        }
    }
}

fn cmd_explain_audit(connection: &str) -> Result<(), Box<dyn std::error::Error>> {
    runner::verify_pg_accel_provenance(connection)?;
    let all_passed = explain_audit::run_audit(connection)?;
    if all_passed {
        Ok(())
    } else {
        Err("explain-audit: at least one RequiredToday row failed".into())
    }
}

fn phase6_gate_workloads() -> Result<Vec<Box<dyn workloads::Workload>>, Box<dyn std::error::Error>>
{
    workloads::PHASE6_DOMAIN_CONTRACTS
        .iter()
        .map(|contract| {
            workloads::find_workload(contract.workload).ok_or_else(|| {
                format!(
                    "Phase 6 domain contract references missing workload `{}`",
                    contract.workload
                )
                .into()
            })
        })
        .collect()
}

fn enforce_phase6_matrix_complete(report: &BenchReport) -> Result<(), Box<dyn std::error::Error>> {
    let expected = workloads::PHASE6_DOMAIN_CONTRACTS
        .iter()
        .flat_map(|contract| {
            contract
                .verification_rows
                .iter()
                .map(move |rows| (contract.workload, *rows))
        })
        .collect::<BTreeSet<_>>();
    let observed = report
        .workloads
        .iter()
        .map(|result| (result.name.as_str(), result.rows))
        .collect::<Vec<_>>();
    let mut gaps = Vec::new();

    for (workload, rows) in &expected {
        let matches = observed
            .iter()
            .filter(|cell| cell.0 == *workload && cell.1 == *rows)
            .count();
        if matches != 1 {
            gaps.push(format!(
                "`{workload}` @ {rows}: expected one result, found {matches}"
            ));
        }
    }
    for (workload, rows) in &observed {
        if !expected.contains(&(*workload, *rows)) {
            gaps.push(format!("unexpected result `{workload}` @ {rows}"));
        }
    }
    for crash in &report.crashes {
        gaps.push(format!(
            "{} @ {} crashed: {}",
            crash.workload, crash.rows, crash.error
        ));
    }

    if gaps.is_empty() {
        Ok(())
    } else {
        Err(format!("Phase 6 domain matrix is incomplete:\n{}", gaps.join("\n")).into())
    }
}

fn enforce_phase6_evidence(report: &BenchReport) -> Result<(), Box<dyn std::error::Error>> {
    let mut failures = Vec::new();
    for result in &report.workloads {
        let Some(contract) = workloads::PHASE6_DOMAIN_CONTRACTS.iter().find(|contract| {
            contract.workload == result.name && contract.verification_rows.contains(&result.rows)
        }) else {
            failures.push(format!(
                "{} @ {} has no typed Phase 6 contract",
                result.name, result.rows
            ));
            continue;
        };

        if result.correctness_diff_artifact.is_none() {
            failures.push(format!(
                "{} @ {} has no native-oracle correctness artifact",
                result.name, result.rows
            ));
        }
        if result.plan_snippet.is_none() || result.baseline_plan_snippet.is_none() {
            failures.push(format!(
                "{} @ {} is missing accel or native plan evidence",
                result.name, result.rows
            ));
        }
        if !result.dispatch_counter_captured {
            failures.push(format!(
                "{} @ {} has no dispatch-counter evidence: {}",
                result.name,
                result.rows,
                result
                    .dispatch_counter_error
                    .as_deref()
                    .unwrap_or("unknown error")
            ));
        }
        if result.pg_accel_stock_exec_delta != 0 {
            failures.push(format!(
                "{} @ {} used {} stock executor fallback(s)",
                result.name, result.rows, result.pg_accel_stock_exec_delta
            ));
        }
        let verified_native_decline = matches!(
            result.native_decline_evidence.as_ref(),
            Some(report::NativeDeclineEvidence {
                source: report::DeclineReasonSource::PlannerReported,
                ..
            })
        );
        if !result.gpu_kernel_dispatched && !verified_native_decline {
            failures.push(format!(
                "{} @ {} stayed native without a planner-reported decline",
                result.name, result.rows
            ));
        }

        match contract.oracle {
            workloads::Phase6DomainOracle::PostgisExactRecheck => {
                if result.gpu_kernel_dispatched {
                    let proves_spatial_descriptor = result
                        .plan_snippet
                        .as_deref()
                        .is_some_and(|plan| plan.contains("GPU Descriptor Filter: spatial("));
                    if !proves_spatial_descriptor {
                        failures.push(format!(
                            "{} @ {} dispatched without a spatial descriptor/recheck contract",
                            result.name, result.rows
                        ));
                    }
                }
            }
            workloads::Phase6DomainOracle::NativeH3FailClosed => {
                if result.plan_selected
                    || result.gpu_kernel_dispatched
                    || result.gpu_kernel_execution_delta != 0
                {
                    failures.push(format!(
                        "{} @ {} violated the fail-closed H3 topology lane",
                        result.name, result.rows
                    ));
                }
            }
            workloads::Phase6DomainOracle::NativeH3
            | workloads::Phase6DomainOracle::PostgisRaster => {}
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("Phase 6 evidence gate failed:\n{}", failures.join("\n")).into())
    }
}

fn cmd_phase6_gate(
    connection: &str,
    artifacts_dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let workloads = phase6_gate_workloads()?;
    let artifact_root = artifacts_dir.unwrap_or_else(|| artifacts::default_run_dir("phase6-gate"));
    let mut cells = Vec::new();
    for (contract, workload) in workloads::PHASE6_DOMAIN_CONTRACTS.iter().zip(&workloads) {
        for rows in contract.verification_rows {
            cells.push(runner::WorkloadRunCell {
                workload: workload.as_ref(),
                rows: *rows,
            });
        }
    }
    let config = runner::BenchConfig {
        iterations: 1,
        warmup: 0,
        seed: 42,
        timing_mode: runner::TimingMode::RawWallClock,
        cache_mode: runner::CacheMode::Both,
        plans_capture_path: Some(artifact_root.join("plans.txt")),
        guc_profile: None,
        skip_guc_verify: false,
        artifacts_dir: Some(artifact_root),
    };

    let report = runner::run_cells_with_config(connection, &cells, &config)?;
    print_report(&report, &ReportFormat::Markdown)?;
    enforce_phase6_matrix_complete(&report)?;
    enforce_phase6_evidence(&report)?;
    enforce_benchmark_ship_gate(&report)?;
    enforce_h3_lane_gate(&report)?;
    eprintln!(
        "[phase6-gate] PASS: all {} domain cells produced native-oracle, plan, dispatch, and decline/recheck evidence",
        cells.len()
    );
    Ok(())
}

const PHASE9_VERIFICATION_ROWS: usize = 10_000;

fn phase9_gate_workloads() -> Result<Vec<Box<dyn workloads::Workload>>, Box<dyn std::error::Error>>
{
    workloads::PHASE9_OPERATOR_DECLINES
        .iter()
        .map(|contract| {
            workloads::find_workload(contract.workload).ok_or_else(|| {
                format!(
                    "Phase 9 lane `{}` references missing workload `{}`",
                    contract.lane.as_str(),
                    contract.workload
                )
                .into()
            })
        })
        .collect()
}

fn enforce_phase9_matrix_complete(report: &BenchReport) -> Result<(), Box<dyn std::error::Error>> {
    let mut gaps = Vec::new();
    for contract in workloads::PHASE9_OPERATOR_DECLINES {
        let matches = report
            .workloads
            .iter()
            .filter(|result| {
                result.name == contract.workload && result.rows == PHASE9_VERIFICATION_ROWS
            })
            .count();
        if matches != 1 {
            gaps.push(format!(
                "{} (`{}`): expected one {}-row result, found {matches}",
                contract.lane.as_str(),
                contract.workload,
                PHASE9_VERIFICATION_ROWS
            ));
        }
    }
    for crash in &report.crashes {
        gaps.push(format!(
            "{}: crashed at {} rows ({})",
            crash.workload, crash.rows, crash.error
        ));
    }
    if gaps.is_empty() {
        return Ok(());
    }
    Err(format!(
        "Phase 9 operator matrix is incomplete:\n{}",
        gaps.join("\n")
    )
    .into())
}

fn cmd_phase9_gate(
    connection: &str,
    artifacts_dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let workloads = phase9_gate_workloads()?;
    let artifact_root = artifacts_dir.unwrap_or_else(|| artifacts::default_run_dir("phase9-gate"));
    let cells = workloads
        .iter()
        .map(|workload| runner::WorkloadRunCell {
            workload: workload.as_ref(),
            rows: PHASE9_VERIFICATION_ROWS,
        })
        .collect::<Vec<_>>();
    let config = runner::BenchConfig {
        iterations: 1,
        warmup: 0,
        seed: 42,
        timing_mode: runner::TimingMode::RawWallClock,
        cache_mode: runner::CacheMode::Warm,
        plans_capture_path: Some(artifact_root.join("plans.txt")),
        guc_profile: None,
        skip_guc_verify: false,
        artifacts_dir: Some(artifact_root),
    };

    let report = runner::run_cells_with_config(connection, &cells, &config)?;
    print_report(&report, &ReportFormat::Markdown)?;
    enforce_phase9_matrix_complete(&report)?;
    enforce_benchmark_ship_gate(&report)?;
    eprintln!(
        "[phase9-gate] PASS: all {} operator lanes produced exact-oracle and planner/dispatch evidence at {} rows",
        workloads::PHASE9_OPERATOR_DECLINES.len(),
        PHASE9_VERIFICATION_ROWS
    );
    Ok(())
}

const METAL_SHIP_GATE_ITERATIONS: usize = 10;
const METAL_SHIP_GATE_WARMUP: usize = 5;
const METAL_SHIP_GATE_SEED: u64 = 42;
const METAL_SHIP_GATE_CONTRACT_FILE: &str = "metal_ship_gate_contract.json";

fn metal_ship_gate_workloads()
-> Result<Vec<Box<dyn workloads::Workload>>, Box<dyn std::error::Error>> {
    let mut resolved = Vec::with_capacity(workloads::METAL_SHIP_GATE_CELLS.len());
    let mut seen = BTreeSet::new();
    for contract in workloads::METAL_SHIP_GATE_CELLS {
        if !seen.insert((contract.workload, contract.rows)) {
            return Err(format!(
                "Metal ship-gate contract contains duplicate cell `{}` @ {}",
                contract.workload, contract.rows
            )
            .into());
        }
        let workload = workloads::find_workload(contract.workload).ok_or_else(|| {
            format!(
                "Metal ship-gate contract references missing workload `{}`",
                contract.workload
            )
        })?;
        if !workload.row_scales().contains(&contract.rows) {
            return Err(format!(
                "Metal ship-gate workload `{}` does not register the required {}-row scale",
                contract.workload, contract.rows
            )
            .into());
        }
        let entry = workloads::benchmark_threshold_matrix_entry(contract.workload, contract.rows)
            .ok_or_else(|| {
            format!(
                "Metal ship-gate cell `{}` @ {} has no threshold-matrix entry",
                contract.workload, contract.rows
            )
        })?;
        let workloads::BenchmarkLaneExpectation::GpuWinner { min_warm_speedup } = entry.expectation
        else {
            return Err(format!(
                "Metal ship-gate cell `{}` @ {} is not classified as a GPU winner",
                contract.workload, contract.rows
            )
            .into());
        };
        if !min_warm_speedup.is_finite()
            || min_warm_speedup < report::BENCHMARK_SHIP_GATE_MIN_SPEEDUP
        {
            return Err(format!(
                "Metal ship-gate cell `{}` @ {} has invalid warm-speedup floor {min_warm_speedup}",
                contract.workload, contract.rows
            )
            .into());
        }
        resolved.push(workload);
    }
    Ok(resolved)
}

fn write_metal_ship_gate_contract(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if root.exists() {
        if !root.is_dir() {
            return Err(format!(
                "Metal ship-gate artifact path `{}` exists and is not a directory",
                root.display()
            )
            .into());
        }
        if fs::read_dir(root)?.next().transpose()?.is_some() {
            return Err(format!(
                "Metal ship-gate artifact directory `{}` is not empty; evidence bundles are immutable",
                root.display()
            )
            .into());
        }
    }

    let mut cells = Vec::with_capacity(workloads::METAL_SHIP_GATE_CELLS.len());
    for contract in workloads::METAL_SHIP_GATE_CELLS {
        let entry = workloads::benchmark_threshold_matrix_entry(contract.workload, contract.rows)
            .ok_or_else(|| {
            format!(
                "Metal ship-gate cell `{}` @ {} has no threshold-matrix entry",
                contract.workload, contract.rows
            )
        })?;
        let workloads::BenchmarkLaneExpectation::GpuWinner { min_warm_speedup } = entry.expectation
        else {
            return Err(format!(
                "Metal ship-gate cell `{}` @ {} is not classified as a GPU winner",
                contract.workload, contract.rows
            )
            .into());
        };
        cells.push(serde_json::json!({
            "workload": contract.workload,
            "rows": contract.rows,
            "lane": entry.lane,
            "min_warm_speedup_vs_postgresql_parallel": min_warm_speedup,
        }));
    }

    let manifest = serde_json::json!({
        "schema_version": 1,
        "gate": "qualified_metal_benchmark_ship_gate",
        "comparison": "postgresql_parallel",
        "iterations": METAL_SHIP_GATE_ITERATIONS,
        "warmup": METAL_SHIP_GATE_WARMUP,
        "seed": METAL_SHIP_GATE_SEED,
        "timing_mode": "raw-wallclock",
        "cache_mode": "both",
        "required_harness_profile": "release",
        "generic_min_speedup": report::BENCHMARK_SHIP_GATE_MIN_SPEEDUP,
        "threshold_source": "benchmark_threshold_matrix_entry",
        "cells": cells,
    });
    fs::create_dir_all(root)?;
    let mut contents = serde_json::to_vec_pretty(&manifest)?;
    contents.push(b'\n');
    fs::write(root.join(METAL_SHIP_GATE_CONTRACT_FILE), contents)?;
    Ok(())
}

fn enforce_metal_ship_gate_complete(
    report: &BenchReport,
) -> Result<(), Box<dyn std::error::Error>> {
    let expected = workloads::METAL_SHIP_GATE_CELLS
        .iter()
        .map(|cell| (cell.workload, cell.rows))
        .collect::<BTreeSet<_>>();
    let observed = report
        .workloads
        .iter()
        .map(|result| (result.name.as_str(), result.rows))
        .collect::<Vec<_>>();
    let mut gaps = Vec::new();

    for (workload, rows) in &expected {
        let matches = observed
            .iter()
            .filter(|cell| cell.0 == *workload && cell.1 == *rows)
            .count();
        if matches != 1 {
            gaps.push(format!(
                "`{workload}` @ {rows}: expected one result, found {matches}"
            ));
        }
    }
    for result in &report.workloads {
        if !expected.contains(&(result.name.as_str(), result.rows)) {
            gaps.push(format!(
                "unexpected result `{}` @ {}",
                result.name, result.rows
            ));
            continue;
        }
        if result.plan_snippet.is_none() || result.baseline_plan_snippet.is_none() {
            gaps.push(format!(
                "`{}` @ {} is missing accelerated or PostgreSQL plan evidence",
                result.name, result.rows
            ));
        }
        if result.correctness_diff_artifact.is_none() {
            gaps.push(format!(
                "`{}` @ {} is missing its correctness-diff artifact",
                result.name, result.rows
            ));
        }
        if result.pg_accel_stock_exec_delta != 0 {
            gaps.push(format!(
                "`{}` @ {} used {} stock-executor fallback(s)",
                result.name, result.rows, result.pg_accel_stock_exec_delta
            ));
        }
    }
    for crash in &report.crashes {
        gaps.push(format!(
            "{} @ {} crashed: {}",
            crash.workload, crash.rows, crash.error
        ));
    }

    if report.methodology.iterations != METAL_SHIP_GATE_ITERATIONS {
        gaps.push(format!(
            "expected {} measured iterations, report recorded {}",
            METAL_SHIP_GATE_ITERATIONS, report.methodology.iterations
        ));
    }
    if report.methodology.warmup != METAL_SHIP_GATE_WARMUP {
        gaps.push(format!(
            "expected {} warmups, report recorded {}",
            METAL_SHIP_GATE_WARMUP, report.methodology.warmup
        ));
    }
    if report.methodology.row_scales != [1_000_000] {
        gaps.push(format!(
            "expected only the 1M row scale, report recorded {:?}",
            report.methodology.row_scales
        ));
    }
    if report.methodology.timing_mode != "raw-wallclock" {
        gaps.push(format!(
            "expected raw-wallclock timing, report recorded `{}`",
            report.methodology.timing_mode
        ));
    }
    if report.methodology.cache_mode != "both" {
        gaps.push(format!(
            "expected cache mode both, report recorded `{}`",
            report.methodology.cache_mode
        ));
    }
    if report.methodology.harness_profile != "release" {
        gaps.push(format!(
            "expected a release benchmark harness, report recorded `{}`",
            report.methodology.harness_profile
        ));
    }

    if gaps.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Metal benchmark ship-gate evidence is incomplete:\n{}",
            gaps.join("\n")
        )
        .into())
    }
}

fn cmd_metal_ship_gate(
    connection: &str,
    artifacts_dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(debug_assertions) {
        return Err(
            "Metal benchmark ship gate requires a release harness; run with `cargo run --release`"
                .into(),
        );
    }

    let workloads = metal_ship_gate_workloads()?;
    let artifact_root =
        artifacts_dir.unwrap_or_else(|| artifacts::default_run_dir("metal-ship-gate"));
    write_metal_ship_gate_contract(&artifact_root)?;
    let cells = workloads::METAL_SHIP_GATE_CELLS
        .iter()
        .zip(&workloads)
        .map(|(contract, workload)| runner::WorkloadRunCell {
            workload: workload.as_ref(),
            rows: contract.rows,
        })
        .collect::<Vec<_>>();
    let config = runner::BenchConfig {
        iterations: METAL_SHIP_GATE_ITERATIONS,
        warmup: METAL_SHIP_GATE_WARMUP,
        seed: METAL_SHIP_GATE_SEED,
        timing_mode: runner::TimingMode::RawWallClock,
        cache_mode: runner::CacheMode::Both,
        plans_capture_path: Some(artifact_root.join("plans.txt")),
        guc_profile: None,
        skip_guc_verify: false,
        artifacts_dir: Some(artifact_root),
    };

    let report = runner::run_cells_with_config(connection, &cells, &config)?;
    print_report(&report, &ReportFormat::Markdown)?;
    enforce_metal_ship_gate_complete(&report)?;
    enforce_benchmark_ship_gate(&report)?;
    enforce_h3_lane_gate(&report)?;
    eprintln!(
        "[metal-ship-gate] PASS: all {} fixed winner cells selected, dispatched, met their warm-speedup floors, and produced complete evidence",
        cells.len()
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_fp64_calibrate(
    connection: &str,
    multipliers: &str,
    max_size: Option<&str>,
    warmup: usize,
    seed: u64,
    realistic_gucs: bool,
    capture_plans: bool,
    timing: &TimingArg,
    cache_mode: &CacheModeArg,
    skip_guc_verify: bool,
    artifacts_dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let multipliers = fp64_calibration::parse_multiplier_list(multipliers)?;
    let sizes = fp64_calibration::sizes_with_optional_cap(max_size)?;
    let artifact_root =
        artifacts_dir.unwrap_or_else(|| artifacts::default_run_dir("fp64-calibrate"));

    let options = fp64_calibration::Fp64CalibrationOptions {
        multipliers,
        sizes,
        warmup,
        seed,
        timing_mode: runner::TimingMode::from(timing),
        cache_mode: runner::CacheMode::from(cache_mode),
        guc_profile: if realistic_gucs {
            Some(runner::GucProfile::realistic())
        } else {
            None
        },
        skip_guc_verify,
        capture_plans,
        artifact_root,
    };
    let summary = fp64_calibration::run_fp64_calibration(connection, &options)?;

    print!("{}", summary.to_markdown());
    if summary.selected_candidate().is_none() {
        return Err("fp64 calibration: no multiplier satisfied every fp64 matrix cell".into());
    }
    if summary
        .fp64_disabled_proof
        .as_ref()
        .is_some_and(|proof| proof.custom_scan_selected)
    {
        return Err(
            "fp64 calibration: pg_accel.fp64_enabled=false proof still selected Custom Scan".into(),
        );
    }
    Ok(())
}

fn cmd_setup(
    connection: &str,
    rows: usize,
    seed: u64,
    workload_name: Option<&str>,
    category: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let workloads = resolve_workloads(workload_name, category)?;
    for w in &workloads {
        runner::setup(connection, w.as_ref(), rows, seed)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_run(
    connection: &str,
    workload_name: Option<&str>,
    category: Option<&str>,
    iterations: usize,
    warmup: usize,
    seed: u64,
    format: &ReportFormat,
    realistic_gucs: bool,
    capture_plans: bool,
    timing: &TimingArg,
    cache_mode: &CacheModeArg,
    skip_guc_verify: bool,
    artifacts_dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let workloads = resolve_workloads(workload_name, category)?;
    let run_artifacts = artifacts_dir.unwrap_or_else(|| artifacts::default_run_dir("run"));
    let config = runner::BenchConfig {
        iterations,
        warmup,
        seed,
        timing_mode: runner::TimingMode::from(timing),
        cache_mode: runner::CacheMode::from(cache_mode),
        plans_capture_path: if capture_plans {
            Some(run_artifacts.join("plans.txt"))
        } else {
            None
        },
        guc_profile: if realistic_gucs {
            Some(runner::GucProfile::realistic())
        } else {
            None
        },
        skip_guc_verify,
        artifacts_dir: Some(run_artifacts),
    };
    let report = runner::run_all_with_config(connection, &workloads, &config)?;
    print_report(&report, format)?;
    enforce_benchmark_ship_gate(&report)?;
    enforce_h3_lane_gate(&report)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_crash_repro(
    connection: &str,
    workload_name: &str,
    rows: usize,
    iterations: usize,
    warmup: usize,
    seed: u64,
    format: &ReportFormat,
    realistic_gucs: bool,
    capture_plans: bool,
    timing: &TimingArg,
    cache_mode: &CacheModeArg,
    skip_guc_verify: bool,
    artifacts_dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut workloads = resolve_workloads(Some(workload_name), None)?;
    let workload = workloads
        .pop()
        .ok_or_else(|| format!("unknown workload: {workload_name}"))?;
    validate_supported_repro_rows(workload.as_ref(), rows, "crash-repro")?;
    let repro_artifacts =
        artifacts_dir.unwrap_or_else(|| artifacts::default_run_dir("crash-repro"));
    let config = runner::BenchConfig {
        iterations,
        warmup,
        seed,
        timing_mode: runner::TimingMode::from(timing),
        cache_mode: runner::CacheMode::from(cache_mode),
        plans_capture_path: if capture_plans {
            Some(repro_artifacts.join("plans.txt"))
        } else {
            None
        },
        guc_profile: if realistic_gucs {
            Some(runner::GucProfile::realistic())
        } else {
            None
        },
        skip_guc_verify,
        artifacts_dir: Some(repro_artifacts),
    };
    let report = runner::run_one_report_with_config(connection, workload.as_ref(), rows, &config)?;
    print_report(&report, format)?;
    if !report.crashes.is_empty() {
        return Err("crash-repro: workload failed; see artifact directory for logs".into());
    }
    enforce_benchmark_ship_gate(&report)?;
    enforce_h3_lane_gate(&report)?;
    Ok(())
}

fn cmd_resume(
    connection: &str,
    artifacts_dir: &Path,
    output_dir: Option<PathBuf>,
    format: &ReportFormat,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let plan = resume::load_retry_plan(artifacts_dir)?;
    print_resume_plan(&plan);
    if plan.cells.is_empty() {
        eprintln!(
            "[resume] no crashed cells found in {}",
            artifacts_dir.display()
        );
        return Ok(());
    }

    let retry_config = plan
        .config
        .clone()
        .ok_or("resume plan has retry cells but no saved benchmark config")?;
    let retry_artifacts = output_dir.unwrap_or_else(|| artifacts::default_run_dir("resume"));
    let workload_boxes = resolve_resume_workloads(&plan)?;

    if dry_run {
        eprintln!(
            "[resume] dry run: would retry {} cell(s) into {}",
            plan.cells.len(),
            retry_artifacts.display()
        );
        return Ok(());
    }

    fs::create_dir_all(&retry_artifacts)?;
    let source_artifact = resume::write_resume_source_artifact(&retry_artifacts, &plan)?;
    eprintln!(
        "[resume] source linkage written to {}",
        source_artifact.display()
    );

    let cells: Vec<runner::WorkloadRunCell<'_>> = workload_boxes
        .iter()
        .map(|(workload, rows)| runner::WorkloadRunCell {
            workload: workload.as_ref(),
            rows: *rows,
        })
        .collect();
    let config = runner::BenchConfig {
        iterations: retry_config.iterations,
        warmup: retry_config.warmup,
        seed: retry_config.seed,
        timing_mode: retry_config.timing_mode,
        cache_mode: retry_config.cache_mode,
        plans_capture_path: if retry_config.capture_plans {
            Some(retry_artifacts.join("plans.txt"))
        } else {
            None
        },
        guc_profile: if retry_config.realistic_gucs {
            Some(runner::GucProfile::realistic())
        } else {
            None
        },
        skip_guc_verify: retry_config.skip_guc_verify,
        artifacts_dir: Some(retry_artifacts),
    };

    let report = runner::run_cells_with_config(connection, &cells, &config)?;
    print_report(&report, format)?;
    if !report.crashes.is_empty() {
        return Err(format!(
            "resume: {} retried cell(s) still crashed; see retry artifact directory",
            report.crashes.len()
        )
        .into());
    }
    enforce_benchmark_ship_gate(&report)?;
    enforce_h3_lane_gate(&report)?;
    Ok(())
}

fn cmd_report(format: &ReportFormat) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::read_to_string(std::io::stdin())?;
    let report: BenchReport = serde_json::from_str(&stdin)?;
    print_report(&report, format)?;
    enforce_benchmark_ship_gate(&report)?;
    enforce_h3_lane_gate(&report)?;
    Ok(())
}

/// Hard-fail the bench process when the generic benchmark ship gate produces
/// any failure.
fn enforce_benchmark_ship_gate(report: &BenchReport) -> Result<(), Box<dyn std::error::Error>> {
    let failures = report.evaluate_benchmark_ship_gate();
    if failures.is_empty() {
        return Ok(());
    }
    eprintln!(
        "[benchmark-ship-gate] FAIL: {} benchmark ship gate failure(s) — see \
         `### Benchmark Ship Gate Failures` in the markdown report for details. \
         Gate floor: {floor:.2}x.",
        failures.len(),
        floor = report::BENCHMARK_SHIP_GATE_MIN_SPEEDUP,
    );
    for f in &failures {
        eprintln!(
            "[benchmark-ship-gate]   {} @ {} rows — {} (observed speedup_median={:.4}x, floor={:.2}x)",
            f.workload,
            f.rows,
            f.kind.label(),
            f.speedup_median,
            f.gate_floor,
        );
    }
    Err(format!(
        "benchmark ship gate: {} failure(s); see report `### Benchmark Ship Gate Failures` section",
        failures.len()
    )
    .into())
}

/// Hard-fail the bench process when the H3 lane gate produces any failure.
///
/// The markdown renderer already appended an `### H3 Lane Gate Failures`
/// section to the report; this function turns that visual signal into a
/// non-zero process exit code so CI catches the regression. Without this,
/// a Winner regressing below `H3_LANE_GATE_MIN_WARM_SPEEDUP` would print
/// red text and still let CI go green — exactly the regression vector the
/// durable H3 winning-lane contract exists to close.
fn enforce_h3_lane_gate(report: &BenchReport) -> Result<(), Box<dyn std::error::Error>> {
    let failures = report.evaluate_h3_lane_gate();
    if failures.is_empty() {
        return Ok(());
    }
    eprintln!(
        "[h3-lane-gate] FAIL: {} H3 lane gate failure(s) — see `### H3 Lane Gate Failures` \
         in the markdown report for details. Gate floor: {floor:.2}x.",
        failures.len(),
        floor = report::H3_LANE_GATE_MIN_WARM_SPEEDUP,
    );
    for f in &failures {
        eprintln!(
            "[h3-lane-gate]   {} @ {} rows — {} (observed speedup_median={:.4}x, floor={:.2}x)",
            f.workload,
            f.rows,
            f.kind.label(),
            f.speedup_median,
            f.gate_floor,
        );
    }
    Err(format!(
        "h3 lane gate: {} failure(s); see report `### H3 Lane Gate Failures` section",
        failures.len()
    )
    .into())
}

fn print_resume_plan(plan: &resume::RetryPlan) {
    let summary = &plan.manifest_summary;
    eprintln!(
        "[resume] source={} manifest={} completed={} correctness={} pre-risk={} plans={} crash={} logs={} provenance={} failures={}",
        plan.source_dir.display(),
        plan.source_manifest.display(),
        summary.completed,
        summary.correctness,
        summary.pre_risk,
        summary.plan,
        summary.crash,
        summary.log,
        summary.provenance,
        summary.failure,
    );
    if let Some(config) = &plan.config {
        eprintln!(
            "[resume] config: iterations={} warmup={} seed={} timing={} cache={} realistic_gucs={} skip_guc_verify={} capture_plans={}",
            config.iterations,
            config.warmup,
            config.seed,
            config.timing_arg(),
            config.cache_arg(),
            config.realistic_gucs,
            config.skip_guc_verify,
            config.capture_plans,
        );
    }
    for cell in &plan.cells {
        eprintln!(
            "[resume] retry cell: {} @ {} rows",
            cell.workload, cell.rows
        );
    }
}

fn resolve_resume_workloads(
    plan: &resume::RetryPlan,
) -> Result<Vec<ResumeWorkloadCell>, Box<dyn std::error::Error>> {
    let mut out = Vec::with_capacity(plan.cells.len());
    for cell in &plan.cells {
        let workload = workloads::find_workload(&cell.workload).ok_or_else(|| {
            format!(
                "resume artifact references unknown workload: {}",
                cell.workload
            )
        })?;
        validate_supported_repro_rows(workload.as_ref(), cell.rows, "resume")?;
        let current_query_identity = artifacts::BenchmarkQueryIdentity::resolve(
            workload.query_sql(),
            workload.baseline_query_sql(),
        )?;
        resume::validate_retry_cell_query_identity(cell, &current_query_identity)?;
        out.push((workload, cell.rows));
    }
    Ok(out)
}

fn validate_supported_repro_rows(
    workload: &dyn workloads::Workload,
    rows: usize,
    context: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let is_h3 = workloads::workload_metadata(workload.name())
        .is_some_and(|metadata| metadata.category == workloads::WorkloadCategory::GpuH3);
    if !is_h3 || workload.row_scales().contains(&rows) {
        return Ok(());
    }

    let supported = workload
        .row_scales()
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "{context}: refusing unsupported H3 row scale for {}: rows={rows}; supported row scales: [{supported}]",
        workload.name()
    )
    .into())
}

fn resolve_workloads(
    name: Option<&str>,
    category: Option<&str>,
) -> Result<Vec<Box<dyn workloads::Workload>>, Box<dyn std::error::Error>> {
    if let Some(n) = name {
        let w = workloads::find_workload(n).ok_or_else(|| format!("unknown workload: {n}"))?;
        Ok(vec![w])
    } else {
        let mut wls = workloads::all_workloads();
        if let Some(cats) = category {
            let allowed: Vec<&str> = cats.split(',').map(str::trim).collect();
            wls.retain(|w| {
                let category = workloads::workload_metadata(w.name())
                    .map_or_else(|| w.category(), |metadata| metadata.category.as_str());
                allowed.contains(&category)
            });
            if wls.is_empty() {
                return Err(format!("no workloads match category filter: {cats}").into());
            }
        }
        Ok(wls)
    }
}

fn cmd_validate(
    workload_name: Option<&str>,
    category: Option<&str>,
    rows: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let wls = resolve_workloads(workload_name, category)?;
    let mut total_issues = 0;

    for w in &wls {
        let issues = workloads::validate_workload(w.as_ref(), rows);
        let name = w.name();

        // Check extension requirements
        let required_exts = workloads::workload_metadata(name).map_or_else(Vec::new, |metadata| {
            metadata
                .required_extensions
                .iter()
                .map(|extension| extension.as_str())
                .collect::<Vec<_>>()
        });

        if issues.is_empty() && required_exts.is_empty() {
            eprintln!("[validate] {name}: OK");
        } else if issues.is_empty() {
            eprintln!(
                "[validate] {name}: OK (requires extensions: {})",
                required_exts.join(", ")
            );
        } else {
            for issue in &issues {
                eprintln!("[validate] {issue}");
            }
            total_issues += issues.len();
        }
    }

    if total_issues > 0 {
        Err(format!("{total_issues} validation issue(s) found").into())
    } else {
        eprintln!("[validate] all {} workload(s) passed validation", wls.len());
        Ok(())
    }
}

fn cmd_dry_run(
    workload_name: Option<&str>,
    category: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let wls = resolve_workloads(workload_name, category)?;
    let sample_rows = runner::ROW_SCALES[0];

    // First validate
    let mut total_issues = 0;
    for w in &wls {
        let issues = workloads::validate_workload(w.as_ref(), sample_rows);
        for issue in &issues {
            eprintln!("[dry-run] WARNING: {issue}");
        }
        total_issues += issues.len();
    }

    if total_issues > 0 {
        return Err(format!("dry run aborted: {total_issues} validation issue(s)").into());
    }

    // Print execution plan
    println!("=== Dry Run: Benchmark Execution Plan ===\n");
    println!(
        "Row scales: {}\n",
        runner::ROW_SCALES
            .iter()
            .map(|r| format!("{r}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for w in &wls {
        let name = w.name();
        let required_exts = workloads::workload_metadata(name).map_or_else(Vec::new, |metadata| {
            metadata
                .required_extensions
                .iter()
                .map(|extension| extension.as_str())
                .collect::<Vec<_>>()
        });

        println!("Workload: {name}");
        println!("  Description: {}", w.description());
        if !required_exts.is_empty() {
            println!("  Required extensions: {}", required_exts.join(", "));
        }

        println!(
            "  Setup ({} statements @ {sample_rows} rows):",
            w.setup_sql(sample_rows).len()
        );
        for (i, sql) in w.setup_sql(sample_rows).iter().enumerate() {
            let preview = if sql.chars().count() > 80 {
                format!("{}...", sql.chars().take(77).collect::<String>())
            } else {
                sql.clone()
            };
            println!("    [{i}] {preview}");
        }

        let query = w.query_sql();
        println!("  Query: {query}");

        println!("  Cleanup ({} statements):", w.cleanup_sql().len());
        for (i, sql) in w.cleanup_sql().iter().enumerate() {
            println!("    [{i}] {sql}");
        }
        println!();
    }

    println!("=== All {len} workload(s) validated ===", len = wls.len());
    Ok(())
}

fn print_report(
    report: &BenchReport,
    format: &ReportFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    match format {
        ReportFormat::Markdown => print!("{}", report.to_markdown()),
        ReportFormat::Json => println!("{}", report.to_json()?),
        ReportFormat::Csv => print!("{}", report.to_csv()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn measured_result(name: &str, rows: usize) -> report::WorkloadResult {
        let category = workloads::workload_metadata(name).map_or_else(
            || "test".to_owned(),
            |metadata| metadata.category.as_str().to_owned(),
        );
        let iterations = vec![
            report::IterationResult {
                accel_ms: 5.0,
                parallel_ms: 10.0,
                accel_first: true,
                cache_purge: bench_model::CachePurgeState::NotRequested,
                cache_state: bench_model::CacheState::Warm,
            },
            report::IterationResult {
                accel_ms: 6.0,
                parallel_ms: 12.0,
                accel_first: false,
                cache_purge: bench_model::CachePurgeState::NotRequested,
                cache_state: bench_model::CacheState::Warm,
            },
        ];
        report::WorkloadResult::from_iterations(
            name.to_owned(),
            "test workload".to_owned(),
            category,
            report::classify_kernel(name),
            rows,
            iterations,
            false,
        )
    }

    fn evidenced_result(name: &str, rows: usize) -> report::WorkloadResult {
        let mut result = measured_result(name, rows);
        result.plan_snippet = Some("Seq Scan on benchmark_input".to_owned());
        result.baseline_plan_snippet = Some("Seq Scan on benchmark_input".to_owned());
        result.correctness_diff_artifact = Some("correctness.json".to_owned());
        result.dispatch_counter_captured = true;
        result.native_decline_evidence = Some(report::NativeDeclineEvidence {
            reason: "test_planner_decline".to_owned(),
            source: report::DeclineReasonSource::PlannerReported,
        });
        result
    }

    fn bench_report(workloads: Vec<report::WorkloadResult>) -> BenchReport {
        let row_scales = workloads
            .iter()
            .map(|result| result.rows)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        BenchReport {
            hardware: None,
            gucs: None,
            methodology: report::Methodology {
                iterations: 2,
                warmup: 0,
                row_scales,
                ordering: "paired".to_owned(),
                statistical_tests: Vec::new(),
                timing_mode: "raw-wallclock".to_owned(),
                cache_mode: "warm".to_owned(),
                harness_profile: "debug".to_owned(),
            },
            workloads,
            artifact_dir: None,
            crashes: Vec::new(),
            postmaster_start_time: None,
        }
    }

    fn crashed_scale(workload: &str, rows: usize, error: &str) -> report::CrashedScale {
        report::CrashedScale {
            workload: workload.to_owned(),
            rows,
            error: error.to_owned(),
            repro_command: None,
            plan_snippet_artifact: None,
            correctness_diff_artifact: None,
            log_tail_artifacts: Vec::new(),
        }
    }

    fn complete_phase6_report() -> BenchReport {
        let mut results = Vec::new();
        for contract in workloads::PHASE6_DOMAIN_CONTRACTS {
            for rows in contract.verification_rows {
                let mut result = evidenced_result(contract.workload, *rows);
                if contract.oracle == workloads::Phase6DomainOracle::PostgisExactRecheck {
                    result.plan_selected = true;
                    result.gpu_kernel_dispatched = true;
                    result.gpu_kernel_execution_delta = 1;
                    result.native_decline_evidence = None;
                    result.plan_snippet = Some(
                        "Custom Scan (pg_accel)\nGPU Descriptor Filter: spatial(exact_recheck)"
                            .to_owned(),
                    );
                }
                results.push(result);
            }
        }
        bench_report(results)
    }

    fn complete_phase9_report() -> BenchReport {
        bench_report(
            workloads::PHASE9_OPERATOR_DECLINES
                .iter()
                .map(|contract| measured_result(contract.workload, PHASE9_VERIFICATION_ROWS))
                .collect(),
        )
    }

    fn complete_metal_report() -> BenchReport {
        let results = workloads::METAL_SHIP_GATE_CELLS
            .iter()
            .map(|contract| evidenced_result(contract.workload, contract.rows))
            .collect();
        let mut report = bench_report(results);
        report.methodology.iterations = METAL_SHIP_GATE_ITERATIONS;
        report.methodology.warmup = METAL_SHIP_GATE_WARMUP;
        report.methodology.row_scales = vec![1_000_000];
        report.methodology.cache_mode = "both".to_owned();
        report.methodology.harness_profile = "release".to_owned();
        report
    }

    fn gate_error(result: Result<(), Box<dyn std::error::Error>>) -> String {
        result.expect_err("gate should fail").to_string()
    }

    fn unique_temp_path(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("pg-accel-{label}-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn run_help_matches_publication_guc_profile() {
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("run")
            .expect("run subcommand")
            .render_long_help()
            .to_string();

        for expected in [
            "16 GB `shared_buffers`",
            "512 MB `work_mem`",
            "48 GB `effective_cache_size`",
            "`max_worker_processes=16`",
            "`max_parallel_workers=12`",
            "`max_parallel_workers_per_gather=8`",
            "2 GB `maintenance_work_mem`",
        ] {
            assert!(
                help.contains(expected),
                "run --help is missing publication profile token {expected:?}:\n{help}"
            );
        }
        for retired in ["8 GB `shared_buffers`", "256 MB `work_mem`"] {
            assert!(
                !help.contains(retired),
                "run --help still contains retired profile token {retired:?}:\n{help}"
            );
        }
    }

    #[test]
    fn cli_value_parsers_accept_aliases_and_reject_unknown_values() {
        assert!(matches!(
            "EXPLAIN-ANALYZE".parse::<TimingArg>(),
            Ok(TimingArg::Explain)
        ));
        assert!(matches!(
            "analyze".parse::<TimingArg>(),
            Ok(TimingArg::Explain)
        ));
        assert!(matches!("wall".parse::<TimingArg>(), Ok(TimingArg::Raw)));
        assert!(matches!(
            "WALL-CLOCK".parse::<TimingArg>(),
            Ok(TimingArg::Raw)
        ));
        assert!(matches!("both".parse::<TimingArg>(), Ok(TimingArg::Both)));
        assert_eq!(TimingArg::Explain.to_string(), "explain");
        assert_eq!(TimingArg::Raw.to_string(), "raw");
        assert_eq!(TimingArg::Both.to_string(), "both");
        assert_eq!(
            "sampled"
                .parse::<TimingArg>()
                .expect_err("unknown timing mode should fail"),
            "unknown timing mode: sampled"
        );
        assert_eq!(
            runner::TimingMode::from(&TimingArg::Explain),
            runner::TimingMode::ExplainAnalyze
        );
        assert_eq!(
            runner::TimingMode::from(&TimingArg::Raw),
            runner::TimingMode::RawWallClock
        );
        assert_eq!(
            runner::TimingMode::from(&TimingArg::Both),
            runner::TimingMode::Both
        );

        assert!(matches!(
            "COLD".parse::<CacheModeArg>(),
            Ok(CacheModeArg::Cold)
        ));
        assert!(matches!(
            "warm".parse::<CacheModeArg>(),
            Ok(CacheModeArg::Warm)
        ));
        assert!(matches!(
            "both".parse::<CacheModeArg>(),
            Ok(CacheModeArg::Both)
        ));
        assert_eq!(CacheModeArg::Cold.to_string(), "cold");
        assert_eq!(CacheModeArg::Warm.to_string(), "warm");
        assert_eq!(CacheModeArg::Both.to_string(), "both");
        assert_eq!(
            "mixed"
                .parse::<CacheModeArg>()
                .expect_err("unknown cache mode should fail"),
            "unknown cache mode: mixed"
        );
        assert_eq!(
            runner::CacheMode::from(&CacheModeArg::Cold),
            runner::CacheMode::Cold
        );
        assert_eq!(
            runner::CacheMode::from(&CacheModeArg::Warm),
            runner::CacheMode::Warm
        );
        assert_eq!(
            runner::CacheMode::from(&CacheModeArg::Both),
            runner::CacheMode::Both
        );

        assert!(matches!(
            "MD".parse::<ReportFormat>(),
            Ok(ReportFormat::Markdown)
        ));
        assert!(matches!(
            "json".parse::<ReportFormat>(),
            Ok(ReportFormat::Json)
        ));
        assert!(matches!(
            "CSV".parse::<ReportFormat>(),
            Ok(ReportFormat::Csv)
        ));
        assert_eq!(ReportFormat::Markdown.to_string(), "markdown");
        assert_eq!(ReportFormat::Json.to_string(), "json");
        assert_eq!(ReportFormat::Csv.to_string(), "csv");
        assert_eq!(
            "html"
                .parse::<ReportFormat>()
                .expect_err("unknown report format should fail"),
            "unknown format: html"
        );
    }

    #[test]
    fn phase6_matrix_and_evidence_accept_complete_typed_report() {
        let report = complete_phase6_report();
        enforce_phase6_matrix_complete(&report).expect("complete Phase 6 matrix");
        enforce_phase6_evidence(&report).expect("complete Phase 6 evidence");
    }

    #[test]
    fn phase6_matrix_reports_missing_duplicate_unexpected_and_crashed_cells() {
        let mut report = complete_phase6_report();
        let missing = report.workloads.remove(0);
        let duplicate = report.workloads[0].clone();
        let duplicate_name = duplicate.name.clone();
        let duplicate_rows = duplicate.rows;
        report.workloads.push(duplicate);
        report
            .workloads
            .push(measured_result("unexpected_phase6_cell", 777));
        report
            .crashes
            .push(crashed_scale("crashed_phase6_cell", 888, "backend exited"));

        let error = gate_error(enforce_phase6_matrix_complete(&report));
        assert!(error.starts_with("Phase 6 domain matrix is incomplete:"));
        assert!(error.contains(&format!(
            "`{}` @ {}: expected one result, found 0",
            missing.name, missing.rows
        )));
        assert!(error.contains(&format!(
            "`{duplicate_name}` @ {duplicate_rows}: expected one result, found 2"
        )));
        assert!(error.contains("unexpected result `unexpected_phase6_cell` @ 777"));
        assert!(error.contains("crashed_phase6_cell @ 888 crashed: backend exited"));
    }

    #[test]
    fn phase6_evidence_reports_all_release_contract_violations() {
        let native = workloads::PHASE6_DOMAIN_CONTRACTS
            .iter()
            .find(|contract| contract.oracle == workloads::Phase6DomainOracle::NativeH3)
            .expect("native H3 Phase 6 contract");
        let mut missing = measured_result(native.workload, native.verification_rows[0]);
        missing.dispatch_counter_error = Some("counter unavailable".to_owned());
        missing.pg_accel_stock_exec_delta = 2;

        let spatial = workloads::PHASE6_DOMAIN_CONTRACTS
            .iter()
            .find(|contract| contract.oracle == workloads::Phase6DomainOracle::PostgisExactRecheck)
            .expect("spatial Phase 6 contract");
        let mut spatial_without_descriptor =
            evidenced_result(spatial.workload, spatial.verification_rows[0]);
        spatial_without_descriptor.plan_selected = true;
        spatial_without_descriptor.gpu_kernel_dispatched = true;
        spatial_without_descriptor.gpu_kernel_execution_delta = 1;
        spatial_without_descriptor.native_decline_evidence = None;
        spatial_without_descriptor.plan_snippet = Some("Custom Scan (pg_accel)".to_owned());

        let fail_closed = workloads::PHASE6_DOMAIN_CONTRACTS
            .iter()
            .find(|contract| contract.oracle == workloads::Phase6DomainOracle::NativeH3FailClosed)
            .expect("fail-closed H3 Phase 6 contract");
        let mut violated_fail_closed =
            evidenced_result(fail_closed.workload, fail_closed.verification_rows[0]);
        violated_fail_closed.plan_selected = true;
        violated_fail_closed.gpu_kernel_dispatched = true;
        violated_fail_closed.gpu_kernel_execution_delta = 3;

        let report = bench_report(vec![
            missing,
            spatial_without_descriptor,
            violated_fail_closed,
            evidenced_result("untyped_phase6_workload", 999),
        ]);
        let error = gate_error(enforce_phase6_evidence(&report));

        assert!(error.starts_with("Phase 6 evidence gate failed:"));
        assert!(error.contains(&format!(
            "{} @ {} has no native-oracle correctness artifact",
            native.workload, native.verification_rows[0]
        )));
        assert!(error.contains(&format!(
            "{} @ {} is missing accel or native plan evidence",
            native.workload, native.verification_rows[0]
        )));
        assert!(error.contains("has no dispatch-counter evidence: counter unavailable"));
        assert!(error.contains("used 2 stock executor fallback(s)"));
        assert!(error.contains("stayed native without a planner-reported decline"));
        assert!(error.contains("dispatched without a spatial descriptor/recheck contract"));
        assert!(error.contains("violated the fail-closed H3 topology lane"));
        assert!(error.contains("untyped_phase6_workload @ 999 has no typed Phase 6 contract"));
    }

    #[test]
    fn phase9_matrix_accepts_complete_report_and_aggregates_failures() {
        let complete = complete_phase9_report();
        enforce_phase9_matrix_complete(&complete).expect("complete Phase 9 matrix");

        let mut incomplete = complete;
        let missing = incomplete.workloads.remove(0);
        let duplicate = incomplete.workloads[0].clone();
        let duplicate_name = duplicate.name.clone();
        incomplete.workloads.push(duplicate);
        incomplete.crashes.push(crashed_scale(
            "phase9_crash",
            PHASE9_VERIFICATION_ROWS,
            "signal 11",
        ));

        let error = gate_error(enforce_phase9_matrix_complete(&incomplete));
        assert!(error.starts_with("Phase 9 operator matrix is incomplete:"));
        assert!(error.contains(&format!("`{}`", missing.name)));
        assert!(error.contains("found 0"));
        assert!(error.contains(&format!("`{duplicate_name}`")));
        assert!(error.contains("found 2"));
        assert!(error.contains("phase9_crash: crashed at 10000 rows (signal 11)"));
    }

    #[test]
    fn metal_ship_gate_accepts_complete_release_evidence() {
        enforce_metal_ship_gate_complete(&complete_metal_report())
            .expect("complete Metal ship-gate evidence");
    }

    #[test]
    fn metal_ship_gate_aggregates_cell_evidence_crash_and_methodology_failures() {
        let mut report = complete_metal_report();
        let missing = report.workloads.remove(0);
        let duplicate = report.workloads[0].clone();
        let duplicate_name = duplicate.name.clone();
        report.workloads.push(duplicate);

        let evidence_target = &mut report.workloads[1];
        let evidence_name = evidence_target.name.clone();
        evidence_target.plan_snippet = None;
        evidence_target.baseline_plan_snippet = None;
        evidence_target.correctness_diff_artifact = None;
        evidence_target.pg_accel_stock_exec_delta = 4;
        report
            .workloads
            .push(evidenced_result("unexpected_metal_cell", 1_000_000));
        report
            .crashes
            .push(crashed_scale("metal_crash", 1_000_000, "device lost"));
        report.methodology.iterations = 3;
        report.methodology.warmup = 1;
        report.methodology.row_scales = vec![10_000, 1_000_000];
        report.methodology.timing_mode = "explain-analyze".to_owned();
        report.methodology.cache_mode = "warm".to_owned();
        report.methodology.harness_profile = "debug".to_owned();

        let error = gate_error(enforce_metal_ship_gate_complete(&report));
        assert!(error.starts_with("Metal benchmark ship-gate evidence is incomplete:"));
        assert!(error.contains(&format!(
            "`{}` @ {}: expected one result, found 0",
            missing.name, missing.rows
        )));
        assert!(error.contains(&format!(
            "`{duplicate_name}` @ 1000000: expected one result, found 2"
        )));
        assert!(error.contains("unexpected result `unexpected_metal_cell` @ 1000000"));
        assert!(error.contains(&format!(
            "`{evidence_name}` @ 1000000 is missing accelerated or PostgreSQL plan evidence"
        )));
        assert!(error.contains(&format!(
            "`{evidence_name}` @ 1000000 is missing its correctness-diff artifact"
        )));
        assert!(error.contains("used 4 stock-executor fallback(s)"));
        assert!(error.contains("metal_crash @ 1000000 crashed: device lost"));
        assert!(error.contains("expected 10 measured iterations, report recorded 3"));
        assert!(error.contains("expected 5 warmups, report recorded 1"));
        assert!(error.contains("expected only the 1M row scale"));
        assert!(error.contains("expected raw-wallclock timing, report recorded `explain-analyze`"));
        assert!(error.contains("expected cache mode both, report recorded `warm`"));
        assert!(error.contains("expected a release benchmark harness, report recorded `debug`"));
    }

    #[test]
    fn phase6_gate_registry_resolves_every_typed_cell() {
        let gate_workloads = phase6_gate_workloads().expect("Phase 6 workloads should resolve");
        assert_eq!(
            gate_workloads.len(),
            workloads::PHASE6_DOMAIN_CONTRACTS.len()
        );
        let cell_count = gate_workloads
            .iter()
            .zip(workloads::PHASE6_DOMAIN_CONTRACTS)
            .map(|(workload, contract)| {
                assert_eq!(workload.name(), contract.workload);
                assert_eq!(workload.category(), contract.category.as_str());
                contract.verification_rows.len()
            })
            .sum::<usize>();
        assert_eq!(cell_count, 36);
    }

    #[test]
    fn phase9_gate_registry_is_live_at_canonical_scale() {
        let gate_workloads = phase9_gate_workloads().expect("Phase 9 workloads should resolve");
        assert_eq!(
            gate_workloads.len(),
            workloads::PHASE9_OPERATOR_DECLINES.len()
        );
        for (workload, contract) in gate_workloads
            .iter()
            .zip(workloads::PHASE9_OPERATOR_DECLINES)
        {
            assert_eq!(workload.name(), contract.workload);
            assert!(
                workload.row_scales().contains(&PHASE9_VERIFICATION_ROWS),
                "{} must support the canonical Phase 9 scale",
                contract.workload
            );
            assert!(
                workload.result_oracle(PHASE9_VERIFICATION_ROWS).is_some(),
                "{} must provide an exact Phase 9 oracle",
                contract.workload
            );
        }
    }

    #[test]
    fn metal_ship_gate_registry_resolves_the_exact_contract() {
        let gate_workloads =
            metal_ship_gate_workloads().expect("Metal ship-gate workloads should resolve");
        assert_eq!(gate_workloads.len(), workloads::METAL_SHIP_GATE_CELLS.len());
        for (workload, contract) in gate_workloads.iter().zip(workloads::METAL_SHIP_GATE_CELLS) {
            assert_eq!(workload.name(), contract.workload);
            assert!(workload.row_scales().contains(&contract.rows));
            assert!(matches!(
                workloads::benchmark_threshold_matrix_entry(contract.workload, contract.rows)
                    .map(|entry| entry.expectation),
                Some(workloads::BenchmarkLaneExpectation::GpuWinner { .. })
            ));
        }
    }

    #[test]
    fn metal_ship_gate_manifest_is_deterministic_and_immutable() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "pg-accel-metal-ship-gate-test-{}-{nonce}",
            std::process::id()
        ));
        let left = base.join("left");
        let right = base.join("right");

        write_metal_ship_gate_contract(&left).expect("first contract manifest");
        write_metal_ship_gate_contract(&right).expect("second contract manifest");
        let left_bytes = fs::read(left.join(METAL_SHIP_GATE_CONTRACT_FILE)).expect("left manifest");
        let right_bytes =
            fs::read(right.join(METAL_SHIP_GATE_CONTRACT_FILE)).expect("right manifest");
        assert_eq!(left_bytes, right_bytes);

        let manifest: serde_json::Value =
            serde_json::from_slice(&left_bytes).expect("valid contract JSON");
        assert_eq!(manifest["schema_version"], 1);
        assert_eq!(manifest["gate"], "qualified_metal_benchmark_ship_gate");
        assert_eq!(manifest["comparison"], "postgresql_parallel");
        assert_eq!(manifest["iterations"], METAL_SHIP_GATE_ITERATIONS);
        assert_eq!(manifest["warmup"], METAL_SHIP_GATE_WARMUP);
        assert_eq!(manifest["seed"], METAL_SHIP_GATE_SEED);
        assert_eq!(manifest["timing_mode"], "raw-wallclock");
        assert_eq!(manifest["cache_mode"], "both");
        assert_eq!(manifest["required_harness_profile"], "release");
        assert_eq!(
            manifest["generic_min_speedup"].as_f64(),
            Some(report::BENCHMARK_SHIP_GATE_MIN_SPEEDUP)
        );
        assert_eq!(
            manifest["threshold_source"],
            "benchmark_threshold_matrix_entry"
        );
        let cells = manifest["cells"].as_array().expect("contract cells array");
        assert_eq!(cells.len(), workloads::METAL_SHIP_GATE_CELLS.len());
        for (cell, contract) in cells.iter().zip(workloads::METAL_SHIP_GATE_CELLS) {
            let entry =
                workloads::benchmark_threshold_matrix_entry(contract.workload, contract.rows)
                    .expect("Metal cell threshold entry");
            let workloads::BenchmarkLaneExpectation::GpuWinner { min_warm_speedup } =
                entry.expectation
            else {
                panic!("Metal cell must be a GPU winner");
            };
            assert_eq!(cell["workload"], contract.workload);
            assert_eq!(cell["rows"], contract.rows);
            assert_eq!(cell["lane"], entry.lane);
            assert_eq!(
                cell["min_warm_speedup_vs_postgresql_parallel"].as_f64(),
                Some(min_warm_speedup)
            );
        }
        assert!(
            write_metal_ship_gate_contract(&left)
                .expect_err("non-empty evidence directory must fail")
                .to_string()
                .contains("evidence bundles are immutable")
        );

        fs::remove_dir_all(base).expect("remove manifest test directories");
    }

    #[test]
    fn metal_ship_gate_manifest_rejects_files_and_accepts_existing_empty_directories() {
        let base = unique_temp_path("metal-contract-paths");
        fs::create_dir_all(&base).expect("create contract path test root");

        let occupied_file = base.join("occupied");
        fs::write(&occupied_file, b"not a directory").expect("create occupied file");
        let error = write_metal_ship_gate_contract(&occupied_file)
            .expect_err("existing file path must fail")
            .to_string();
        assert!(error.contains("exists and is not a directory"));

        let empty_dir = base.join("empty");
        fs::create_dir(&empty_dir).expect("create empty evidence directory");
        write_metal_ship_gate_contract(&empty_dir)
            .expect("existing empty evidence directory should be writable");
        assert!(empty_dir.join(METAL_SHIP_GATE_CONTRACT_FILE).is_file());

        fs::remove_dir_all(base).expect("remove contract path test root");
    }

    #[test]
    fn workload_resolution_supports_name_category_all_and_useful_errors() {
        let named = resolve_workloads(Some("gpu_reduce_sum"), Some("not_a_category"))
            .expect("exact workload name should resolve");
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].name(), "gpu_reduce_sum");

        let h3 =
            resolve_workloads(None, Some(" gpu_h3 ")).expect("trimmed category should resolve");
        assert!(!h3.is_empty());
        assert!(h3.iter().all(|workload| {
            workloads::workload_metadata(workload.name())
                .is_some_and(|metadata| metadata.category == workloads::WorkloadCategory::GpuH3)
        }));

        let all = resolve_workloads(None, None).expect("unfiltered registry should resolve");
        assert_eq!(all.len(), workloads::all_workloads().len());

        let unknown = resolve_workloads(Some("not_a_real_workload"), None)
            .err()
            .expect("unknown workload should fail")
            .to_string();
        assert_eq!(unknown, "unknown workload: not_a_real_workload");
        let no_category = resolve_workloads(None, Some("not_a_category"))
            .err()
            .expect("unknown category should fail")
            .to_string();
        assert_eq!(
            no_category,
            "no workloads match category filter: not_a_category"
        );
    }

    #[test]
    fn h3_repro_rows_must_match_workload_scales() {
        let h3_srf = workloads::find_workload("h3_srf_grid_disk")
            .expect("h3_srf_grid_disk should be registered");
        assert!(validate_supported_repro_rows(h3_srf.as_ref(), 10_000, "crash-repro").is_ok());
        let error = validate_supported_repro_rows(h3_srf.as_ref(), 10_000_000, "crash-repro")
            .expect_err("unregistered H3 row scale must fail")
            .to_string();
        let supported = h3_srf
            .row_scales()
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(
            error,
            format!(
                "crash-repro: refusing unsupported H3 row scale for h3_srf_grid_disk: rows=10000000; supported row scales: [{supported}]"
            )
        );

        let reduce = workloads::find_workload("gpu_reduce_sum")
            .expect("gpu_reduce_sum should be registered");
        assert!(
            validate_supported_repro_rows(reduce.as_ref(), 12_345, "crash-repro").is_ok(),
            "non-H3 crash repro keeps arbitrary row-count support"
        );
    }

    #[test]
    fn pure_cli_validation_rendering_and_release_gate_wrappers_are_executable() {
        cmd_validate(Some("gpu_reduce_sum"), None, 10_000)
            .expect("a registered release workload should validate without a database");
        cmd_validate(None, Some("gpu_h3"), 10_000)
            .expect("the registered H3 category should validate without a database");
        cmd_dry_run(Some("gpu_reduce_sum"), None)
            .expect("dry-run should render a validated workload without executing it");

        let empty = bench_report(Vec::new());
        for format in [
            ReportFormat::Markdown,
            ReportFormat::Json,
            ReportFormat::Csv,
        ] {
            print_report(&empty, &format).expect("every report format should render");
        }
        enforce_benchmark_ship_gate(&empty).expect("an empty synthetic report has no failures");
        enforce_h3_lane_gate(&empty).expect("an empty synthetic report has no H3 failures");

        let mut generic = evidenced_result("grouped_agg_int4", 1_000_000);
        generic.plan_selected = true;
        generic.gpu_kernel_dispatched = true;
        generic.gpu_kernel_execution_delta = 2;
        generic.native_decline_evidence = None;
        generic.speedup_median_vs_parallel = 0.8;
        let generic_error = enforce_benchmark_ship_gate(&bench_report(vec![generic]))
            .expect_err("a selected sub-floor lane must fail the process wrapper")
            .to_string();
        assert!(generic_error.contains("benchmark ship gate"));

        let mut h3 = evidenced_result("h3_cell_to_parent", 100_000);
        h3.plan_selected = true;
        h3.gpu_kernel_dispatched = true;
        h3.gpu_kernel_execution_delta = 2;
        h3.native_decline_evidence = None;
        h3.speedup_median_vs_parallel = 0.8;
        let h3_error = enforce_h3_lane_gate(&bench_report(vec![h3]))
            .expect_err("a selected sub-floor H3 winner must fail its lane wrapper")
            .to_string();
        assert!(h3_error.contains("h3 lane gate"));
    }

    #[test]
    fn resume_resolution_revalidates_registry_rows_and_query_identity() {
        let workload = workloads::find_workload("gpu_reduce_sum")
            .expect("gpu_reduce_sum should be registered");
        let identity = artifacts::BenchmarkQueryIdentity::resolve(
            workload.query_sql(),
            workload.baseline_query_sql(),
        )
        .expect("registered workload should expose a valid query identity");
        let mut plan = resume::RetryPlan {
            source_dir: PathBuf::from("source-artifacts"),
            source_manifest: PathBuf::from("source-artifacts/resume_audit_manifest.json"),
            manifest_summary: resume::ManifestSummary {
                completed: 1,
                correctness: 2,
                pre_risk: 3,
                plan: 4,
                crash: 5,
                log: 6,
                provenance: 7,
                failure: 8,
            },
            cells: vec![resume::RetryCell {
                workload: workload.name().to_owned(),
                rows: 10_000,
                accel_query_sql: identity.accel_query_sql().to_owned(),
                baseline_query_sql: identity.baseline_query_sql().to_owned(),
            }],
            config: Some(resume::RetryConfig {
                seed: 42,
                iterations: 10,
                warmup: 5,
                timing_mode: runner::TimingMode::RawWallClock,
                cache_mode: runner::CacheMode::Warm,
                realistic_gucs: true,
                skip_guc_verify: false,
                capture_plans: true,
            }),
        };

        print_resume_plan(&plan);
        let resolved = resolve_resume_workloads(&plan)
            .expect("saved identity should resolve to the same registered workload");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0.name(), "gpu_reduce_sum");
        assert_eq!(resolved[0].1, 10_000);

        plan.cells[0].baseline_query_sql.push_str(" /* changed */");
        let mismatch = resolve_resume_workloads(&plan)
            .err()
            .expect("resume must reject query drift")
            .to_string();
        assert!(mismatch.contains("query identity"), "{mismatch}");

        plan.cells[0].workload = "missing_resume_workload".to_owned();
        let unknown = resolve_resume_workloads(&plan)
            .err()
            .expect("resume must reject an unknown workload")
            .to_string();
        assert!(unknown.contains("unknown workload"), "{unknown}");
    }

    fn dispatch_failure(command: Command) -> String {
        dispatch(Cli { command })
            .expect_err("fixture command should fail before external work")
            .to_string()
    }

    #[test]
    fn dispatcher_preserves_pure_validation_boundaries_for_external_commands() {
        let setup = dispatch_failure(Command::Setup {
            rows: 10_000,
            seed: 42,
            connection: "unused".to_owned(),
            workload: Some("missing_setup_workload".to_owned()),
            category: None,
        });
        assert_eq!(setup, "unknown workload: missing_setup_workload");

        dispatch(Cli {
            command: Command::Run {
                workload: Some("gpu_reduce_sum".to_owned()),
                category: None,
                iterations: 10,
                warmup: 5,
                seed: 42,
                connection: "unused".to_owned(),
                format: ReportFormat::Markdown,
                dry_run: true,
                realistic_gucs: false,
                capture_plans: false,
                timing: TimingArg::Raw,
                cache_mode: CacheModeArg::Warm,
                skip_guc_verify: false,
                artifacts_dir: None,
            },
        })
        .expect("run --dry-run must stop before database execution");

        let run = dispatch_failure(Command::Run {
            workload: Some("missing_run_workload".to_owned()),
            category: None,
            iterations: 10,
            warmup: 5,
            seed: 42,
            connection: "unused".to_owned(),
            format: ReportFormat::Json,
            dry_run: false,
            realistic_gucs: true,
            capture_plans: true,
            timing: TimingArg::Both,
            cache_mode: CacheModeArg::Both,
            skip_guc_verify: true,
            artifacts_dir: Some(PathBuf::from("unused")),
        });
        assert_eq!(run, "unknown workload: missing_run_workload");

        let crash = dispatch_failure(Command::CrashRepro {
            workload: "missing_crash_workload".to_owned(),
            rows: 1,
            iterations: 1,
            warmup: 0,
            seed: 7,
            connection: "unused".to_owned(),
            format: ReportFormat::Csv,
            realistic_gucs: false,
            capture_plans: false,
            timing: TimingArg::Explain,
            cache_mode: CacheModeArg::Cold,
            skip_guc_verify: false,
            artifacts_dir: None,
        });
        assert_eq!(crash, "unknown workload: missing_crash_workload");

        let missing_resume = unique_temp_path("dispatch-missing-resume");
        let resume = dispatch_failure(Command::Resume {
            artifacts_dir: missing_resume.clone(),
            connection: "unused".to_owned(),
            output_dir: None,
            format: ReportFormat::Markdown,
            dry_run: true,
        });
        assert!(resume.contains("resume manifest not readable"), "{resume}");
        assert!(
            resume.contains(&missing_resume.display().to_string()),
            "{resume}"
        );

        dispatch(Cli {
            command: Command::Validate {
                workload: Some("gpu_reduce_sum".to_owned()),
                category: None,
                rows: 10_000,
            },
        })
        .expect("registered workload validation is process-free");

        let multiplier = dispatch_failure(Command::Fp64Calibrate {
            connection: "unused".to_owned(),
            multipliers: "not-a-number".to_owned(),
            max_size: None,
            warmup: 5,
            seed: 42,
            realistic_gucs: false,
            capture_plans: false,
            timing: TimingArg::Raw,
            cache_mode: CacheModeArg::Warm,
            skip_guc_verify: false,
            artifacts_dir: None,
        });
        assert!(
            multiplier.contains("invalid fp64 multiplier"),
            "{multiplier}"
        );

        let size = dispatch_failure(Command::Fp64Calibrate {
            connection: "unused".to_owned(),
            multipliers: "16".to_owned(),
            max_size: Some("not-a-size".to_owned()),
            warmup: 5,
            seed: 42,
            realistic_gucs: true,
            capture_plans: true,
            timing: TimingArg::Both,
            cache_mode: CacheModeArg::Both,
            skip_guc_verify: true,
            artifacts_dir: Some(PathBuf::from("unused")),
        });
        assert!(size.contains("invalid fp64 max-size token"), "{size}");

        if cfg!(debug_assertions) {
            let metal = dispatch_failure(Command::MetalShipGate {
                connection: "unused".to_owned(),
                artifacts_dir: None,
            });
            assert!(metal.contains("requires a release harness"), "{metal}");
        }
    }
}
