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

        /// Apply the realistic GUC profile (8 GB `shared_buffers`, 256 MB
        /// `work_mem`, 48 GB `effective_cache_size`, 8 parallel workers)
        /// before running. Non-reloadable settings (`shared_buffers`,
        /// `max_worker_processes`) log a warning — they require a full PG
        /// restart to take effect.
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

    let workload_boxes = resolve_resume_workloads(&plan)?;
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
/// gate exists to close (see TODO.md Phase 5 H3 winning lane protection).
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

    #[test]
    fn h3_repro_rows_must_match_workload_scales() {
        let h3_srf = workloads::find_workload("h3_srf_grid_disk")
            .expect("h3_srf_grid_disk should be registered");
        assert!(validate_supported_repro_rows(h3_srf.as_ref(), 10_000, "crash-repro").is_ok());
        assert!(validate_supported_repro_rows(h3_srf.as_ref(), 10_000_000, "crash-repro").is_err());

        let reduce = workloads::find_workload("gpu_reduce_sum")
            .expect("gpu_reduce_sum should be registered");
        assert!(
            validate_supported_repro_rows(reduce.as_ref(), 12_345, "crash-repro").is_ok(),
            "non-H3 crash repro keeps arbitrary row-count support"
        );
    }
}
