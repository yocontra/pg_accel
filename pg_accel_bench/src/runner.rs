use postgres::{Client, NoTls};

use crate::report::{self, IterationResult, WorkloadResult};
use crate::workloads::Workload;

/// Execute setup SQL for a workload against the given connection string.
///
/// # Errors
///
/// Returns an error if the connection or any SQL statement fails.
pub fn setup(
    connection: &str,
    workload: &dyn Workload,
    rows: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;
    for sql in workload.setup_sql(rows) {
        client.batch_execute(&sql)?;
    }
    eprintln!("[setup] {} -- created {rows} rows", workload.name());
    Ok(())
}

/// Run a workload benchmark for the given number of iterations and return results.
///
/// # Errors
///
/// Returns an error if the connection or any query fails.
pub fn run(
    connection: &str,
    workload: &dyn Workload,
    iterations: usize,
) -> Result<WorkloadResult, Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;
    let query = workload.query_sql();
    let mut results = Vec::with_capacity(iterations);

    for i in 0..iterations {
        // --- pg_accel ON ---
        client.batch_execute("SET pg_accel.enabled = on")?;
        let accel_ms = run_explain_analyze(&mut client, &query)?;

        // --- pg_accel OFF ---
        client.batch_execute("SET pg_accel.enabled = off")?;
        let baseline_ms = run_explain_analyze(&mut client, &query)?;

        eprintln!(
            "[{name}] iteration {iter}/{total}: accel={accel_ms:.2}ms  \
             baseline={baseline_ms:.2}ms",
            name = workload.name(),
            iter = i + 1,
            total = iterations,
        );

        results.push(IterationResult {
            accel_ms,
            baseline_ms,
        });
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
) -> Result<report::BenchReport, Box<dyn std::error::Error>> {
    let mut results = Vec::with_capacity(workloads.len());
    for w in workloads {
        setup(connection, w.as_ref(), rows)?;
        let result = run(connection, w.as_ref(), iterations)?;
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
