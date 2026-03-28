mod report;
mod runner;
mod stats;
mod workloads;

use clap::{Parser, Subcommand};

use crate::report::BenchReport;

const DEFAULT_CONNECTION: &str = "host=localhost port=5488 user=postgres dbname=pgaccel_a9";

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
        #[arg(long, default_value_t = 100_000)]
        rows: usize,

        /// PostgreSQL connection string.
        #[arg(long, default_value = DEFAULT_CONNECTION)]
        connection: String,

        /// Workload name (omit to set up all workloads).
        #[arg(long)]
        workload: Option<String>,
    },

    /// Run benchmarks and print results.
    Run {
        /// Workload name (omit to run all workloads).
        #[arg(long)]
        workload: Option<String>,

        /// Number of iterations per workload.
        #[arg(long, default_value_t = 5)]
        iterations: usize,

        /// Number of rows for setup (used if tables don't exist yet).
        #[arg(long, default_value_t = 100_000)]
        rows: usize,

        /// PostgreSQL connection string.
        #[arg(long, default_value = DEFAULT_CONNECTION)]
        connection: String,

        /// Output format: `markdown` or `json`.
        #[arg(long, default_value = "markdown")]
        format: ReportFormat,
    },

    /// Print a previously-stored report (reads JSON from stdin).
    Report {
        /// Output format: `markdown` or `json`.
        #[arg(long, default_value = "markdown")]
        format: ReportFormat,
    },
}

#[derive(Clone, Debug)]
enum ReportFormat {
    Markdown,
    Json,
}

impl std::fmt::Display for ReportFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Markdown => write!(f, "markdown"),
            Self::Json => write!(f, "json"),
        }
    }
}

impl std::str::FromStr for ReportFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "markdown" | "md" => Ok(Self::Markdown),
            "json" => Ok(Self::Json),
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
            connection,
            workload,
        } => cmd_setup(&connection, rows, workload.as_deref()),
        Command::Run {
            workload,
            iterations,
            rows,
            connection,
            format,
        } => cmd_run(&connection, workload.as_deref(), iterations, rows, &format),
        Command::Report { format } => cmd_report(&format),
    }
}

fn cmd_setup(
    connection: &str,
    rows: usize,
    workload_name: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let workloads = resolve_workloads(workload_name)?;
    for w in &workloads {
        runner::setup(connection, w.as_ref(), rows)?;
    }
    Ok(())
}

fn cmd_run(
    connection: &str,
    workload_name: Option<&str>,
    iterations: usize,
    rows: usize,
    format: &ReportFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let workloads = resolve_workloads(workload_name)?;
    let report = runner::run_all(connection, &workloads, rows, iterations)?;
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
) -> Result<Vec<Box<dyn workloads::Workload>>, Box<dyn std::error::Error>> {
    match name {
        Some(n) => {
            let w = workloads::find_workload(n).ok_or_else(|| format!("unknown workload: {n}"))?;
            Ok(vec![w])
        }
        None => Ok(workloads::all_workloads()),
    }
}

fn print_report(
    report: &BenchReport,
    format: &ReportFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    match format {
        ReportFormat::Markdown => print!("{}", report.to_markdown()),
        ReportFormat::Json => println!("{}", report.to_json()?),
    }
    Ok(())
}
