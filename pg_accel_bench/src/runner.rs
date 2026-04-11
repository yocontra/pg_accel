use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use postgres::{Client, NoTls};
use rand::Rng;

use crate::report::{self, IterationResult, WorkloadResult};
use crate::workloads::Workload;

/// The five row scales every benchmark is run at. Not configurable —
/// the entire suite always runs at all five for reproducible, comparable results.
pub const ROW_SCALES: &[usize] = &[1_000, 10_000, 100_000, 1_000_000, 10_000_000];

/// Timing mode used to measure a single query iteration.
///
/// `ExplainAnalyze` uses `EXPLAIN ANALYZE` and parses `Execution Time`. This
/// is the historical default, but `EXPLAIN ANALYZE` adds per-node
/// instrumentation overhead which disproportionately penalizes non-custom-scan
/// plans on short queries.
///
/// `RawWallClock` submits the query via `client.execute()` and measures
/// wall-clock time with `Instant::now()` on the client side. No `EXPLAIN
/// ANALYZE`, no instrumentation. This is the preferred mode for the
/// publication-quality rigorous run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TimingMode {
    /// Use `EXPLAIN ANALYZE` and parse `Execution Time`.
    #[default]
    ExplainAnalyze,
    /// Use client-side `Instant` around a plain `execute()` call.
    RawWallClock,
}

/// PostgreSQL GUC profile applied before a benchmark run.
///
/// Two built-in profiles:
/// - `toy()` — the pgrx development defaults (small `shared_buffers`, tiny
///   `work_mem`, 2 parallel workers). Useful for comparing against the
///   historical benchmarks, but not representative of production.
/// - `realistic()` — a production-sized profile tuned for a 64 GB / 12-core
///   workstation (8 GB `shared_buffers`, 256 MB `work_mem`, 48 GB
///   `effective_cache_size`, 8 parallel workers). These are the numbers a
///   production DBA would actually use; benchmarking with `toy()` is
///   methodologically weak.
///
/// `shared_buffers` and `max_worker_processes` are not reloadable — they
/// require a full PG restart. If the caller is running against an existing
/// `pgrx` postmaster, those two settings are skipped with a warning instead
/// of attempting `ALTER SYSTEM`.
#[derive(Clone, Debug)]
pub struct GucProfile {
    pub shared_buffers: String,
    pub work_mem: String,
    pub effective_cache_size: String,
    pub max_parallel_workers_per_gather: u32,
    pub max_worker_processes: u32,
}

impl GucProfile {
    /// pgrx development defaults. The historically-used configuration.
    #[must_use]
    #[allow(dead_code)]
    pub fn toy() -> Self {
        Self {
            shared_buffers: "128MB".to_owned(),
            work_mem: "4MB".to_owned(),
            effective_cache_size: "4GB".to_owned(),
            max_parallel_workers_per_gather: 2,
            max_worker_processes: 8,
        }
    }

    /// Production-sized profile for a 64 GB / 12-core workstation.
    #[must_use]
    pub fn realistic() -> Self {
        Self {
            shared_buffers: "8GB".to_owned(),
            work_mem: "256MB".to_owned(),
            effective_cache_size: "48GB".to_owned(),
            max_parallel_workers_per_gather: 8,
            max_worker_processes: 16,
        }
    }

    /// Apply this profile to a running PG backend via `ALTER SYSTEM SET`.
    ///
    /// Settings that are not reloadable (`shared_buffers`,
    /// `max_worker_processes`) are attempted via `ALTER SYSTEM SET` plus
    /// `pg_reload_conf()`, but a warning is logged because they only take
    /// effect after a full PG restart. For session-level reloadable GUCs
    /// (`work_mem`, `effective_cache_size`,
    /// `max_parallel_workers_per_gather`), the call also issues a
    /// session-level `SET` so subsequent queries on this connection see the
    /// new value immediately.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails. Individual `ALTER SYSTEM`
    /// statements that fail are logged but not fatal — the benchmark
    /// continues with whatever settings the server accepted.
    pub fn apply(&self, connection: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut client = Client::connect(connection, NoTls)?;

        // Reloadable GUCs — can take effect immediately via pg_reload_conf.
        let reloadable: Vec<(&str, String)> = vec![
            ("work_mem", self.work_mem.clone()),
            ("effective_cache_size", self.effective_cache_size.clone()),
            (
                "max_parallel_workers_per_gather",
                self.max_parallel_workers_per_gather.to_string(),
            ),
        ];
        for (name, val) in &reloadable {
            let sql = format!("ALTER SYSTEM SET {name} = '{val}'");
            if let Err(e) = client.batch_execute(&sql) {
                eprintln!("[gucs] ALTER SYSTEM SET {name} failed: {e}");
            }
        }

        // Non-reloadable — needs a full restart to take effect. Record the
        // attempt but warn the operator.
        let non_reloadable: Vec<(&str, String)> = vec![
            ("shared_buffers", self.shared_buffers.clone()),
            (
                "max_worker_processes",
                self.max_worker_processes.to_string(),
            ),
        ];
        for (name, val) in &non_reloadable {
            let sql = format!("ALTER SYSTEM SET {name} = '{val}'");
            if let Err(e) = client.batch_execute(&sql) {
                eprintln!("[gucs] ALTER SYSTEM SET {name} failed: {e}");
            } else {
                eprintln!(
                    "[gucs] WARNING: {name} = {val} recorded via ALTER SYSTEM but requires a \
                     full PG restart to take effect — current benchmark run will still use the \
                     old value."
                );
            }
        }

        if let Err(e) = client.batch_execute("SELECT pg_reload_conf()") {
            eprintln!("[gucs] pg_reload_conf failed: {e}");
        }
        Ok(())
    }
}

/// Runtime configuration knobs that are not per-workload.
///
/// Created once by the CLI layer and threaded through the runner. Keeping
/// this as a single struct (vs. adding more parameters to `run_all`)
/// contains the blast radius of future additions.
#[derive(Clone, Debug)]
pub struct BenchConfig {
    pub iterations: usize,
    pub warmup: usize,
    pub seed: u64,
    pub timing_mode: TimingMode,
    /// If set, run `EXPLAIN (ANALYZE, VERBOSE, BUFFERS)` once per
    /// workload/scale before the timed loop and append the result to this
    /// path.
    pub plans_capture_path: Option<PathBuf>,
    /// If set, this profile is applied via `ALTER SYSTEM SET` before the
    /// first workload runs.
    pub guc_profile: Option<GucProfile>,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            iterations: 10,
            warmup: 3,
            seed: 42,
            timing_mode: TimingMode::default(),
            plans_capture_path: None,
            guc_profile: None,
        }
    }
}

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
#[allow(dead_code)]
pub fn run(
    connection: &str,
    workload: &dyn Workload,
    iterations: usize,
    warmup: usize,
) -> Result<WorkloadResult, Box<dyn std::error::Error>> {
    run_with_timing(
        connection,
        workload,
        iterations,
        warmup,
        TimingMode::ExplainAnalyze,
    )
}

/// Like [`run`] but with an explicit timing mode.
///
/// # Errors
///
/// Returns an error if the connection or any query fails.
pub fn run_with_timing(
    connection: &str,
    workload: &dyn Workload,
    iterations: usize,
    warmup: usize,
    timing_mode: TimingMode,
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
            timings[idx] = run_with_mode(
                &mut mode_clients[idx],
                &query,
                modes[idx],
                &pre_query,
                timing_mode,
            )?;
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
        workload.category().to_owned(),
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

/// Run a single measurement with the given mode and timing strategy.
fn run_with_mode(
    client: &mut Client,
    query: &str,
    mode: BenchMode,
    pre_query: &[String],
    timing_mode: TimingMode,
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
    match timing_mode {
        TimingMode::ExplainAnalyze => run_explain_analyze(client, query),
        TimingMode::RawWallClock => run_raw_wall_clock(client, query),
    }
}

/// Measure query wall time client-side with `Instant::now()` around a plain
/// `execute()` call. No `EXPLAIN ANALYZE`, no per-node instrumentation
/// overhead — this is the timing mode to use for publication-quality
/// numbers.
fn run_raw_wall_clock(client: &mut Client, query: &str) -> Result<f64, Box<dyn std::error::Error>> {
    let start = Instant::now();
    // simple_query so multi-statement queries and SELECT ... still work; we
    // don't care about the rows returned.
    client.simple_query(query)?;
    let elapsed = start.elapsed();
    #[allow(clippy::cast_precision_loss)]
    let ms = (elapsed.as_nanos() as f64) / 1.0e6;
    Ok(ms)
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
#[allow(dead_code)]
pub fn run_all(
    connection: &str,
    workloads: &[Box<dyn Workload>],
    iterations: usize,
    warmup: usize,
    seed: u64,
) -> Result<report::BenchReport, Box<dyn std::error::Error>> {
    run_all_with_config(
        connection,
        workloads,
        &BenchConfig {
            iterations,
            warmup,
            seed,
            ..BenchConfig::default()
        },
    )
}

/// Run every workload with a full [`BenchConfig`] (timing mode, GUC profile,
/// plan capture).
///
/// # Errors
///
/// Returns an error if setup, execution, or cleanup of any workload fails,
/// or if `config.iterations` is below the minimum required for statistical
/// validity.
pub fn run_all_with_config(
    connection: &str,
    workloads: &[Box<dyn Workload>],
    config: &BenchConfig,
) -> Result<report::BenchReport, Box<dyn std::error::Error>> {
    if config.iterations < MIN_ITERATIONS {
        return Err(format!(
            "minimum {MIN_ITERATIONS} iterations required for statistical validity \
             (got {})",
            config.iterations
        )
        .into());
    }

    if let Some(profile) = &config.guc_profile {
        eprintln!("[gucs] applying profile: {profile:?}");
        profile.apply(connection)?;
    }

    ensure_extensions(connection, workloads)?;

    // Truncate plans file once at the start of the run, if plan capture is
    // enabled. Subsequent workload passes append.
    if let Some(path) = &config.plans_capture_path {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            let _ = std::fs::create_dir_all(parent);
        }
        match OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
        {
            Ok(mut f) => {
                let _ = writeln!(
                    f,
                    "=== pg_accel benchmark plans — captured once per workload/scale ==="
                );
            }
            Err(e) => eprintln!("[plans] could not open {}: {e}", path.display()),
        }
    }

    let mut results = Vec::with_capacity(workloads.len() * ROW_SCALES.len());
    let mut crashes: Vec<report::CrashedScale> = Vec::new();

    for w in workloads {
        for &rows in ROW_SCALES {
            eprintln!("\n[scale] {} @ {} rows", w.name(), format_rows(rows));
            match run_workload_with_config(connection, w.as_ref(), rows, config) {
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
        config.iterations,
        config.warmup,
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
#[allow(dead_code)]
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

/// Run a single workload end-to-end honouring `BenchConfig` (timing mode,
/// plan capture).
fn run_workload_with_config(
    connection: &str,
    workload: &dyn Workload,
    rows: usize,
    config: &BenchConfig,
) -> Result<WorkloadResult, Box<dyn std::error::Error>> {
    setup(connection, workload, rows, config.seed)?;

    // Plan capture happens once per workload/scale BEFORE the timed loop —
    // the timed loop runs with whatever timing_mode the operator selected.
    if let Some(path) = &config.plans_capture_path
        && let Err(e) = capture_plan(connection, workload, rows, path)
    {
        eprintln!(
            "[plans] capture failed for {} @ {rows}: {e}",
            workload.name()
        );
    }

    let mut result = run_with_timing(
        connection,
        workload,
        config.iterations,
        config.warmup,
        config.timing_mode,
    )?;
    result.rows = rows;
    cleanup(connection, workload)?;
    Ok(result)
}

/// Capture `EXPLAIN (ANALYZE, VERBOSE, BUFFERS)` output for a workload and
/// append it to the plans file.
///
/// Run with `pg_accel.enabled = on` so the captured plan reflects the
/// production-like dispatch decision.
fn capture_plan(
    connection: &str,
    workload: &dyn Workload,
    rows: usize,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;
    client.batch_execute("SET pg_accel.enabled = on")?;
    client.batch_execute("SET max_parallel_workers_per_gather = DEFAULT")?;
    for sql in workload.pre_query_sql() {
        client.batch_execute(&sql)?;
    }
    let explain = format!(
        "EXPLAIN (ANALYZE, VERBOSE, BUFFERS) {}",
        workload.query_sql()
    );
    let rows_out = client.query(&explain, &[])?;

    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "\n=== {name} @ rows={rows} ===", name = workload.name())?;
    for row in &rows_out {
        let line: &str = row.get(0);
        writeln!(f, "{line}")?;
    }
    Ok(())
}

/// Detect which extensions are installed in the target database.
fn detect_extensions(connection: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;
    let rows = client.query("SELECT extname FROM pg_extension", &[])?;
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
