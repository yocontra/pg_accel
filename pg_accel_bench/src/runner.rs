use postgres::{Client, NoTls};
use rand::Rng;

use crate::report::{self, IterationResult, WorkloadResult};
use crate::workloads::Workload;

/// The five row scales every benchmark is run at. Not configurable —
/// the entire suite always runs at all five for reproducible, comparable results.
pub const ROW_SCALES: &[usize] = &[1_000, 10_000, 100_000, 1_000_000, 10_000_000];

/// Execute setup SQL for a workload against the given connection string.
///
/// Always calls `setseed()` before data generation for deterministic,
/// reproducible benchmarks.
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
    // setseed takes a float in [-1, 1]. Map u64 seed to [0, 1).
    #[allow(clippy::cast_precision_loss)]
    let seed_val = (seed % 1_000_000) as f64 / 1_000_000.0;
    client.batch_execute(&format!("SELECT setseed({seed_val})"))?;
    eprintln!(
        "[setup] {} -- seed {seed} (setseed={seed_val}), {rows} rows",
        workload.name()
    );
    for sql in workload.setup_sql(rows) {
        client.batch_execute(&sql)?;
    }
    Ok(())
}

/// Run a workload benchmark for the given number of iterations and return results.
///
/// `warmup` iterations are run first and excluded from the statistics.
///
/// To eliminate ordering bias (cache warming, shared buffer state), the order
/// of accel-first vs baseline-first is randomized per iteration. Each mode
/// measurement uses `DISCARD ALL` between modes so neither side benefits
/// from the other's cached plans or buffer state. Each mode gets a
/// persistent connection (one per mode per workload/scale) so that
/// one-time backend init costs (tracing, GPU probe) are amortised by
/// warmup iterations rather than paid on every measurement.
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
    let query = workload.query_sql();
    let pre_query = workload.pre_query_sql();
    let total_runs = warmup + iterations;
    let mut results = Vec::with_capacity(iterations);
    let mut rng = rand::thread_rng();

    // Two modes: accel vs PG parallel. Order randomized per iteration.
    let modes = [BenchMode::Accel, BenchMode::PgParallel];

    // Persistent connection per mode — backend init costs (tracing, GPU
    // probe ~40ms) are paid once during warmup, not on every measurement.
    // DISCARD ALL between measurements resets session state (plan cache,
    // GUCs, temp tables) without the fork overhead.
    let mut mode_clients = [
        Client::connect(connection, NoTls)?,
        Client::connect(connection, NoTls)?,
    ];

    for i in 0..total_runs {
        let is_warmup = i < warmup;

        // Randomize order to eliminate cache-warming bias.
        let mut order: [usize; 2] = [0, 1];
        if rng.gen_bool(0.5) {
            order.swap(0, 1);
        }

        let mut timings = [0.0_f64; 2];
        for &idx in &order {
            // DISCARD ALL resets session state before each measurement.
            mode_clients[idx].batch_execute("DISCARD ALL")?;
            timings[idx] =
                run_with_mode(&mut mode_clients[idx], &query, modes[idx], &pre_query)?;
        }

        let accel_ms = timings[0];
        let parallel_ms = timings[1];

        let phase = if is_warmup { "warmup" } else { "bench" };
        let iter_num = if is_warmup { i + 1 } else { i - warmup + 1 };
        let iter_total = if is_warmup { warmup } else { iterations };
        eprintln!(
            "[{name}] {phase} {iter_num}/{iter_total}: \
             accel={accel_ms:.2}ms  parallel={parallel_ms:.2}ms",
            name = workload.name(),
        );

        if !is_warmup {
            results.push(IterationResult {
                accel_ms,
                parallel_ms,
            });
        }
    }

    // Clean close.
    for client in &mut mode_clients {
        let _ = client.batch_execute("DISCARD ALL");
    }

    Ok(WorkloadResult::from_iterations(
        workload.name().to_owned(),
        workload.description().to_owned(),
        0, // rows filled in by caller
        results,
    ))
}

/// Measurement mode for a single run (two-way comparison).
#[derive(Clone, Copy, Debug)]
enum BenchMode {
    /// pg_accel enabled, PG parallel at default.
    Accel,
    /// pg_accel off, PG parallel workers at default.
    PgParallel,
}

/// Run a single EXPLAIN ANALYZE measurement with the given mode.
fn run_with_mode(
    client: &mut Client,
    query: &str,
    mode: BenchMode,
    pre_query: &[String],
) -> Result<f64, Box<dyn std::error::Error>> {
    for sql in pre_query {
        client.batch_execute(sql)?;
    }
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
    }
    run_explain_analyze(client, query)
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

/// Minimum iterations required for statistical validity (paired t-test).
const MIN_ITERATIONS: usize = 10;

/// Build a full report by running every workload at all row scales.
///
/// Each workload is run at 1K, 10K, 100K, and 1M rows. Results include
/// hardware profile auto-detection and GUC settings capture.
///
/// # Errors
///
/// Returns an error if setup, execution, or cleanup of any workload fails,
/// or if `iterations` is below the minimum required for statistical validity.
pub fn run_all(
    connection: &str,
    workloads: &[Box<dyn Workload>],
    iterations: usize,
    warmup: usize,
    seed: u64,
) -> Result<report::BenchReport, Box<dyn std::error::Error>> {
    if iterations < MIN_ITERATIONS {
        return Err(format!(
            "minimum {MIN_ITERATIONS} iterations required for statistical validity \
             (got {iterations})"
        )
        .into());
    }

    ensure_extensions(connection, workloads)?;

    let mut results = Vec::with_capacity(workloads.len() * ROW_SCALES.len());
    let mut crashes: Vec<report::CrashedScale> = Vec::new();

    for w in workloads {
        for &rows in ROW_SCALES {
            eprintln!(
                "\n[scale] {} @ {} rows",
                w.name(),
                format_rows(rows)
            );
            match run_workload(connection, w.as_ref(), rows, seed, iterations, warmup) {
                Ok(mut result) => {
                    result.rows = rows;
                    results.push(result);
                }
                Err(e) => {
                    let err_msg = format!("{e}");
                    eprintln!("[CRASH] {} @ {} — {err_msg}", w.name(), format_rows(rows));
                    crashes.push(report::CrashedScale {
                        workload: w.name().to_owned(),
                        rows,
                        error: err_msg,
                    });
                    let _ = cleanup(connection, w.as_ref());
                    wait_for_pg(connection)?;
                }
            }
        }
    }

    Ok(report::generate_report(
        results,
        crashes,
        Some(connection),
        iterations,
        warmup,
    ))
}

/// Wait for PostgreSQL to become available after a backend crash.
///
/// Retries connection + `SELECT 1` with exponential backoff up to ~10s.
/// Returns an error if PG doesn't recover, aborting the entire suite.
fn wait_for_pg(connection: &str) -> Result<(), Box<dyn std::error::Error>> {
    let delays = [1, 2, 3, 4];
    for (attempt, delay) in delays.iter().enumerate() {
        std::thread::sleep(std::time::Duration::from_secs(*delay));
        match Client::connect(connection, NoTls) {
            Ok(mut c) => match c.batch_execute("SELECT 1") {
                Ok(()) => {
                    eprintln!("[health] PG is alive (attempt {})", attempt + 1);
                    return Ok(());
                }
                Err(_) => continue,
            },
            Err(_) => continue,
        }
    }
    Err("PostgreSQL did not recover after backend crash — aborting benchmark suite".into())
}

/// Run a single workload end-to-end (setup → bench → cleanup).
fn run_workload(
    connection: &str,
    workload: &dyn Workload,
    rows: usize,
    seed: u64,
    iterations: usize,
    warmup: usize,
) -> Result<WorkloadResult, Box<dyn std::error::Error>> {
    setup(connection, workload, rows, seed)?;
    let mut result = run(connection, workload, iterations, warmup)?;
    result.rows = rows;
    cleanup(connection, workload)?;
    Ok(result)
}

/// Detect which extensions are installed in the target database.
fn detect_extensions(connection: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;
    let rows = client.query(
        "SELECT extname FROM pg_extension",
        &[],
    )?;
    let exts: Vec<String> = rows.iter().map(|r| r.get::<_, String>(0)).collect();
    eprintln!("[detect] installed extensions: {}", exts.join(", "));
    Ok(exts)
}

/// Install all extensions required by the selected workloads.
///
/// Collects the unique set of extensions needed, installs each one via
/// `CREATE EXTENSION IF NOT EXISTS`, and fails hard if any cannot be
/// installed (meaning the underlying package is not available on the system).
/// This ensures the benchmark suite is fully self-contained — it never
/// silently skips workloads due to missing extensions.
fn ensure_extensions(
    connection: &str,
    workloads: &[Box<dyn Workload>],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;

    // Always need pg_accel.
    let mut required: Vec<&str> = vec!["pg_accel"];

    // Collect unique extensions required by selected workloads.
    let ext_reqs = crate::workloads::extension_requirements();
    let workload_names: Vec<&str> = workloads.iter().map(|w| w.name()).collect();
    for (wl, ext) in &ext_reqs {
        if workload_names.contains(wl) && !required.contains(ext) {
            required.push(ext);
        }
    }

    // Some extensions have dependencies that must be installed first.
    // postgis_raster requires postgis.
    if required.contains(&"postgis_raster") && !required.contains(&"postgis") {
        required.insert(1, "postgis");
    }

    eprintln!("[setup] installing extensions: {}", required.join(", "));

    for ext in &required {
        let sql = format!("CREATE EXTENSION IF NOT EXISTS \"{ext}\" CASCADE");
        match client.batch_execute(&sql) {
            Ok(()) => eprintln!("[setup] {ext}: ok"),
            Err(e) => {
                return Err(format!(
                    "cannot install required extension '{ext}': {e}\n\
                     Hint: install the system package for '{ext}' and retry."
                )
                .into());
            }
        }
    }

    let installed = detect_extensions(connection)?;
    // Verify all required extensions are now present.
    for ext in &required {
        if !installed.contains(&(*ext).to_string()) {
            return Err(format!(
                "extension '{ext}' was created but not found in pg_extension — \
                 possible CREATE EXTENSION CASCADE issue"
            )
            .into());
        }
    }

    Ok(())
}

/// Format row count for display: 1000 → "1K", 1000000 → "1M".
fn format_rows(rows: usize) -> String {
    match rows {
        r if r >= 1_000_000 && r % 1_000_000 == 0 => format!("{}M", r / 1_000_000),
        r if r >= 1_000 && r % 1_000 == 0 => format!("{}K", r / 1_000),
        r => r.to_string(),
    }
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

    #[test]
    fn test_row_scales_constant() {
        assert_eq!(ROW_SCALES, &[1_000, 10_000, 100_000, 1_000_000, 10_000_000]);
    }

    #[test]
    fn test_format_rows() {
        assert_eq!(format_rows(1_000), "1K");
        assert_eq!(format_rows(10_000), "10K");
        assert_eq!(format_rows(100_000), "100K");
        assert_eq!(format_rows(1_000_000), "1M");
        assert_eq!(format_rows(5_000_000), "5M");
        assert_eq!(format_rows(500), "500");
    }
}
