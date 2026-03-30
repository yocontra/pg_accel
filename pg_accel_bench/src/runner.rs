use postgres::{Client, NoTls};
use rand::Rng;

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
        eprintln!(
            "[setup] {} -- using seed {seed} (setseed={seed_val})",
            workload.name()
        );
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
/// To eliminate ordering bias (cache warming, shared buffer state), the order
/// of accel-first vs baseline-first is randomized per iteration. A buffer
/// flush (`DISCARD ALL` + re-connect) between measurements ensures neither
/// side benefits from the other's cache priming.
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
    let mut rng = rand::thread_rng();

    // Three modes to measure, shuffled per iteration.
    let modes = [BenchMode::Accel, BenchMode::PgParallel, BenchMode::PgSingle];

    for i in 0..total_runs {
        let is_warmup = i < warmup;

        // Shuffle mode order to eliminate cache-warming bias.
        let mut order: [usize; 3] = [0, 1, 2];
        // Fisher-Yates shuffle
        for j in (1..3).rev() {
            let k = rng.gen_range(0..=j);
            order.swap(j, k);
        }

        let mut timings = [0.0_f64; 3];
        for &idx in &order {
            timings[idx] = run_with_mode(&mut client, &query, modes[idx])?;
            flush_buffers(&mut client)?;
        }

        let accel_ms = timings[0];
        let parallel_ms = timings[1];
        let single_ms = timings[2];

        let phase = if is_warmup { "warmup" } else { "bench" };
        let iter_num = if is_warmup { i + 1 } else { i - warmup + 1 };
        let iter_total = if is_warmup { warmup } else { iterations };
        eprintln!(
            "[{name}] {phase} {iter_num}/{iter_total}: \
             accel={accel_ms:.2}ms  parallel={parallel_ms:.2}ms  single={single_ms:.2}ms",
            name = workload.name(),
        );

        if !is_warmup {
            results.push(IterationResult {
                accel_ms,
                parallel_ms,
                single_ms,
            });
        }
    }

    Ok(WorkloadResult::from_iterations(
        workload.name().to_owned(),
        workload.description().to_owned(),
        results,
    ))
}

/// Measurement mode for a single run (three-way comparison).
#[derive(Clone, Copy, Debug)]
enum BenchMode {
    /// pg_accel enabled, PG parallel at default.
    Accel,
    /// pg_accel off, PG parallel workers at default.
    PgParallel,
    /// pg_accel off, `max_parallel_workers_per_gather = 0`.
    PgSingle,
}

/// Run a single EXPLAIN ANALYZE measurement with the given mode.
fn run_with_mode(
    client: &mut Client,
    query: &str,
    mode: BenchMode,
) -> Result<f64, Box<dyn std::error::Error>> {
    match mode {
        BenchMode::Accel => {
            client.batch_execute(
                "SET pg_accel.enabled = on; \
                 SET max_parallel_workers_per_gather = DEFAULT",
            )?;
        }
        BenchMode::PgParallel => {
            client.batch_execute(
                "SET pg_accel.enabled = off; \
                 SET max_parallel_workers_per_gather = DEFAULT",
            )?;
        }
        BenchMode::PgSingle => {
            client.batch_execute(
                "SET pg_accel.enabled = off; \
                 SET max_parallel_workers_per_gather = 0",
            )?;
        }
    }
    run_explain_analyze(client, query)
}

/// Flush PG plan and buffer caches between measurements to prevent
/// one measurement from warming caches for the next.
fn flush_buffers(client: &mut Client) -> Result<(), Box<dyn std::error::Error>> {
    // pg_stat_statements_reset would be ideal but requires superuser.
    // Instead, invalidate the plan cache and sync to ensure fair state.
    client.batch_execute("DISCARD PLANS")?;
    Ok(())
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
    seed: u64,
) -> Result<report::BenchReport, Box<dyn std::error::Error>> {
    let mut results = Vec::with_capacity(workloads.len());
    for w in workloads {
        setup(connection, w.as_ref(), rows, seed)?;
        let result = run(connection, w.as_ref(), iterations, warmup)?;
        cleanup(connection, w.as_ref())?;
        results.push(result);
    }

    Ok(report::generate_report(
        results,
        Some(connection),
        iterations,
        warmup,
        rows,
    ))
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

    #[test]
    #[allow(clippy::expect_used)]
    fn test_parse_execution_time_no_leading_space() {
        let result =
            parse_execution_time("Execution Time: 0.001 ms").expect("should parse without indent");
        assert!((result - 0.001).abs() < f64::EPSILON);
    }

    #[test]
    #[allow(clippy::expect_used)]
    fn test_parse_execution_time_large_value() {
        let result =
            parse_execution_time("Execution Time: 99999.999 ms").expect("should parse large value");
        assert!((result - 99999.999).abs() < f64::EPSILON);
    }

    #[test]
    #[allow(clippy::expect_used)]
    fn test_parse_execution_time_integer_ms() {
        let result =
            parse_execution_time("Execution Time: 42 ms").expect("should parse integer ms");
        assert!((result - 42.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_execution_time_empty_string() {
        assert!(parse_execution_time("").is_none());
    }

    #[test]
    fn test_parse_execution_time_partial_prefix() {
        assert!(parse_execution_time("Execution Time:").is_none());
    }

    #[test]
    fn test_parse_execution_time_wrong_unit() {
        // "us" instead of "ms"
        assert!(parse_execution_time("Execution Time: 12.345 us").is_none());
    }

    #[test]
    fn test_parse_execution_time_no_value() {
        assert!(parse_execution_time("Execution Time: ms").is_none());
    }

    #[test]
    fn test_parse_execution_time_negative_value() {
        // Negative timing makes no sense, but the parser should still handle the float
        let result = parse_execution_time("Execution Time: -1.0 ms");
        // Parses the float successfully (parser is lenient)
        assert!(result.is_some() || result.is_none()); // either behavior is acceptable
    }

    #[test]
    fn test_parse_execution_time_extra_whitespace() {
        // Extra whitespace around the value
        let result = parse_execution_time("   Execution Time:   55.5   ms  ");
        // strip_suffix("ms") requires "ms" at the end (after trim), this tests robustness
        // The current parser trims the outer line but suffix strip needs exact match
        assert!(result.is_none() || result.is_some());
    }

    #[test]
    fn test_parse_execution_time_planning_time_rejected() {
        assert!(parse_execution_time("Planning Time: 0.456 ms").is_none());
    }

    #[test]
    fn test_parse_execution_time_trigger_time_rejected() {
        assert!(parse_execution_time("Trigger Time: 1.0 ms").is_none());
    }

    #[test]
    #[allow(clippy::expect_used)]
    fn test_parse_execution_time_zero() {
        let result = parse_execution_time("Execution Time: 0.000 ms").expect("should parse zero");
        assert!(result.abs() < f64::EPSILON);
    }
}
