use postgres::{Client, NoTls};

use crate::report::{self, IterationResult, WorkloadResult};
use crate::workloads::Workload;

/// Execute setup SQL for a workload against the given connection string.
///
/// If `seed > 0`, sets `setseed(seed)` before data generation for
/// deterministic, reproducible benchmarks.
///
/// # Errors
///
/// Returns an error if the connection or any SQL statement fails.
pub fn setup(
    connection: &str,
    workload: &dyn Workload,
    rows: usize,
    seed: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;
    if seed > 0 {
        // setseed takes a float in [-1, 1]. Map u64 seed to [0, 1).
        #[allow(clippy::cast_precision_loss)]
        let seed_val = (seed % 1_000_000) as f64 / 1_000_000.0;
        client.batch_execute(&format!("SELECT setseed({seed_val})"))?;
        eprintln!("[setup] {} -- using seed {seed} (setseed={seed_val})", workload.name());
    }
    for sql in workload.setup_sql(rows) {
        client.batch_execute(&sql)?;
    }
    eprintln!("[setup] {} -- created {rows} rows", workload.name());
    Ok(())
}

/// Run a workload benchmark for the given number of iterations and return results.
///
/// `warmup` iterations are run first and excluded from the statistics.
///
/// # Errors
///
/// Returns an error if the connection or any query fails.
pub fn run(
    connection: &str,
    workload: &dyn Workload,
    iterations: usize,
    warmup: usize,
) -> Result<WorkloadResult, Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;
    let query = workload.query_sql();
    let total_runs = warmup + iterations;
    let mut results = Vec::with_capacity(iterations);

    for i in 0..total_runs {
        let is_warmup = i < warmup;

        // --- pg_accel ON ---
        client.batch_execute("SET pg_accel.enabled = on")?;
        let accel_ms = run_explain_analyze(&mut client, &query)?;

        // --- pg_accel OFF ---
        client.batch_execute("SET pg_accel.enabled = off")?;
        let baseline_ms = run_explain_analyze(&mut client, &query)?;

        let phase = if is_warmup { "warmup" } else { "bench" };
        let iter_num = if is_warmup { i + 1 } else { i - warmup + 1 };
        let iter_total = if is_warmup { warmup } else { iterations };
        eprintln!(
            "[{name}] {phase} {iter_num}/{iter_total}: accel={accel_ms:.2}ms  \
             baseline={baseline_ms:.2}ms",
            name = workload.name(),
        );

        if !is_warmup {
            results.push(IterationResult {
                accel_ms,
                baseline_ms,
            });
        }
    }

    Ok(WorkloadResult::from_iterations(
        workload.name().to_owned(),
        workload.description().to_owned(),
        results,
    ))
}

/// Cleanup benchmark tables.
///
/// # Errors
///
/// Returns an error if the connection or any SQL statement fails.
pub fn cleanup(
    connection: &str,
    workload: &dyn Workload,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;
    for sql in workload.cleanup_sql() {
        client.batch_execute(&sql)?;
    }
    eprintln!("[cleanup] {} -- tables dropped", workload.name());
    Ok(())
}

/// Build a full report by running every workload in the given list.
///
/// Includes hardware profile auto-detection and GUC settings capture.
///
/// # Errors
///
/// Returns an error if setup, execution, or cleanup of any workload fails.
pub fn run_all(
    connection: &str,
    workloads: &[Box<dyn Workload>],
    rows: usize,
    iterations: usize,
    warmup: usize,
) -> Result<report::BenchReport, Box<dyn std::error::Error>> {
    let mut results = Vec::with_capacity(workloads.len());
    for w in workloads {
        setup(connection, w.as_ref(), rows, 0)?;
        let result = run(connection, w.as_ref(), iterations, warmup)?;
        cleanup(connection, w.as_ref())?;
        results.push(result);
    }

    Ok(report::generate_report(results, Some(connection)))
}

/// Run `EXPLAIN ANALYZE` on a query and parse the execution time from output.
///
/// PostgreSQL returns the execution time in a line like:
///   `Execution Time: 12.345 ms`
fn run_explain_analyze(
    client: &mut Client,
    query: &str,
) -> Result<f64, Box<dyn std::error::Error>> {
    let explain_query = format!("EXPLAIN ANALYZE {query}");
    let rows = client.query(&explain_query, &[])?;

    for row in &rows {
        let line: &str = row.get(0);
        if let Some(ms) = parse_execution_time(line) {
            return Ok(ms);
        }
    }

    Err("could not find 'Execution Time' in EXPLAIN ANALYZE output".into())
}

/// Parse the millisecond value from an `Execution Time: X.XXX ms` line.
fn parse_execution_time(line: &str) -> Option<f64> {
    let trimmed = line.trim();
    let suffix = trimmed.strip_prefix("Execution Time:")?;
    let suffix = suffix.trim();
    let ms_str = suffix.strip_suffix("ms")?.trim();
    ms_str.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::expect_used)]
    fn test_parse_execution_time() {
        assert!(
            (parse_execution_time("  Execution Time: 12.345 ms").expect("should parse") - 12.345)
                .abs()
                < f64::EPSILON
        );
        assert!(parse_execution_time("Planning Time: 0.1 ms").is_none());
        assert!(parse_execution_time("garbage").is_none());
    }
}
