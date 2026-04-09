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

        /// Number of warmup iterations (excluded from statistics).
        #[arg(long, default_value_t = 3)]
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

fn cmd_run(
    connection: &str,
    workload_name: Option<&str>,
    category: Option<&str>,
    iterations: usize,
    warmup: usize,
    seed: u64,
    format: &ReportFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let workloads = resolve_workloads(workload_name, category)?;
    let report = runner::run_all(connection, &workloads, iterations, warmup, seed)?;
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
