#![allow(
    clippy::needless_continue,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::literal_string_with_formatting_args,
    clippy::too_many_lines
)]

mod report;
mod runner;
mod stats;
mod workloads;

use clap::{Parser, Subcommand};

use crate::report::BenchReport;

const DEFAULT_CONNECTION: &str = "host=localhost port=28817 dbname=postgres";

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
        /// be at least 5 for warm-cache measurements (action_items M14 /
        /// Reviewer 1 Sin #14) to amortize shader compile and kernel
        /// launch jitter.
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
        /// Reviewer 2 §3(ii) / action_items M2: `DISCARD ALL` does not
        /// clear the OS page cache. Use `cold` or `both` for any report
        /// published externally.
        #[arg(long, default_value = "warm")]
        cache_mode: CacheModeArg,

        /// Source distribution for the headline speedup calculation —
        /// `median` (default, robust to 22% CV cold-start jitter on the
        /// GPU side) or `mean` (backwards-compat with the historical
        /// report).
        #[arg(long, default_value = "median")]
        speedup_from: SpeedupSourceArg,

        /// Skip the postmaster-GUC mismatch hard-fail (action_items C4).
        /// By default the harness refuses to run if `shared_buffers` or
        /// any other `PGC_POSTMASTER` setting drifts from the requested
        /// profile — because publishing a settings table that doesn't
        /// match reality is worse than no table at all. This flag is
        /// only intended for developer iteration.
        #[arg(long)]
        skip_guc_verify: bool,
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

/// CLI wrapper for median/mean speedup selection.
#[derive(Clone, Debug)]
enum SpeedupSourceArg {
    Median,
    Mean,
}

impl std::fmt::Display for SpeedupSourceArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Median => write!(f, "median"),
            Self::Mean => write!(f, "mean"),
        }
    }
}

impl std::str::FromStr for SpeedupSourceArg {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "median" => Ok(Self::Median),
            "mean" => Ok(Self::Mean),
            other => Err(format!("unknown speedup source: {other}")),
        }
    }
}

impl From<&SpeedupSourceArg> for runner::SpeedupSource {
    fn from(value: &SpeedupSourceArg) -> Self {
        match value {
            SpeedupSourceArg::Median => Self::Median,
            SpeedupSourceArg::Mean => Self::Mean,
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
            speedup_from,
            skip_guc_verify,
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
                    &speedup_from,
                    skip_guc_verify,
                )
            }
        }
        Command::Report { format } => cmd_report(&format),
        Command::Validate {
            workload,
            category,
            rows,
        } => cmd_validate(workload.as_deref(), category.as_deref(), rows),
    }
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
    speedup_from: &SpeedupSourceArg,
    skip_guc_verify: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let workloads = resolve_workloads(workload_name, category)?;
    let config = runner::BenchConfig {
        iterations,
        warmup,
        seed,
        timing_mode: runner::TimingMode::from(timing),
        cache_mode: runner::CacheMode::from(cache_mode),
        speedup_source: runner::SpeedupSource::from(speedup_from),
        plans_capture_path: if capture_plans {
            Some(std::path::PathBuf::from("benchmarks/plans.txt"))
        } else {
            None
        },
        guc_profile: if realistic_gucs {
            Some(runner::GucProfile::realistic())
        } else {
            None
        },
        skip_guc_verify,
    };
    let report = runner::run_all_with_config(connection, &workloads, &config)?;
    print_report(&report, format)?;
    Ok(())
}

fn cmd_report(format: &ReportFormat) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::read_to_string(std::io::stdin())?;
    let report: BenchReport = serde_json::from_str(&stdin)?;
    print_report(&report, format)?;
    Ok(())
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
            wls.retain(|w| allowed.iter().any(|c| w.category() == *c));
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
    let ext_reqs = workloads::extension_requirements();
    let mut total_issues = 0;

    for w in &wls {
        let issues = workloads::validate_workload(w.as_ref(), rows);
        let name = w.name();

        // Check extension requirements
        let required_exts: Vec<&str> = ext_reqs
            .iter()
            .filter(|(wl, _)| *wl == name)
            .map(|(_, ext)| *ext)
            .collect();

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
    let ext_reqs = workloads::extension_requirements();
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
        let required_exts: Vec<&str> = ext_reqs
            .iter()
            .filter(|(wl, _)| *wl == name)
            .map(|(_, ext)| *ext)
            .collect();

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
            let preview = if sql.len() > 80 {
                format!("{}...", &sql[..77])
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
