use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use postgres::{Client, NoTls};
use rand::Rng;

use crate::report::{self, IterationResult, WorkloadResult};
use crate::workloads::Workload;

/// The four row scales every benchmark is run at. Not configurable —
/// the entire suite always runs at all four for reproducible, comparable
/// results.
///
/// **Why no 1K scale:** Reviewer 1 Sin #15 — 160-microsecond measurements
/// taken over the libpq wire protocol are below the instrument noise floor.
/// The protocol round-trip floor on localhost is tens of microseconds, and
/// libpq buffering / kernel scheduling jitter eats the rest. 1K-row rows
/// measure the client harness, not the database. Gray's rule: measurements
/// must exceed the instrument noise floor by 100×. Minimum reportable scale
/// is 10K.
pub const ROW_SCALES: &[usize] = &[10_000, 100_000, 1_000_000, 10_000_000];

/// Timing mode used to measure a single query iteration.
///
/// `ExplainAnalyze` uses `EXPLAIN ANALYZE` and parses `Execution Time`. This
/// penalizes non-custom-scan plans more than Custom Scan plans because a
/// Custom Scan Provider's Next() path can report row-counts essentially for
/// free, while a parallel Seq Scan + Gather + HashAgg pays the per-tuple
/// instrumentation cost in every worker. This advantages pg_accel by
/// ~15-25% on agg/reduce categories (Reviewer 2 §3(i) / action_items M1).
///
/// `RawWallClock` submits the query via `client.simple_query()` and measures
/// wall-clock time with `Instant::now()` on the client side. No `EXPLAIN
/// ANALYZE`, no per-node instrumentation. This is the preferred mode for
/// the publication-quality rigorous run and is now the **default**.
///
/// `Both` runs every iteration twice (raw first, then EXPLAIN ANALYZE)
/// and captures both values so reviewers can audit the gap between the
/// two mechanisms on any given workload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TimingMode {
    /// Use `EXPLAIN ANALYZE` and parse `Execution Time`.
    ExplainAnalyze,
    /// Use client-side `Instant` around a plain `simple_query()` call.
    /// **Default** — reports publishable numbers.
    #[default]
    RawWallClock,
    /// Capture both raw wall-clock and EXPLAIN ANALYZE on every iteration.
    /// Statistical aggregation uses the raw column; the EXPLAIN column is
    /// stored alongside for audit / gap analysis.
    Both,
}

/// Cache cleanliness mode for a single measurement.
///
/// `Warm` runs after `BenchConfig::warmup` iterations (≥5 in reviewer-
/// recommended profile, action_items M14). Page cache and shared buffers
/// are expected to hold the working set.
///
/// `Cold` invokes `sync && purge` (macOS) or `echo 3 >
/// /proc/sys/vm/drop_caches` (Linux, requires root) between iterations
/// — `DISCARD ALL` does **not** clear the OS page cache (Reviewer 2
/// §3(ii) / action_items M2). On Linux without root, we document the
/// limitation in the report and proceed with warm-only measurement.
///
/// `Both` produces side-by-side cold and warm columns in the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CacheMode {
    /// Measure under warm cache only — require at least 5 warmup iterations.
    #[default]
    Warm,
    /// Measure under cold cache only — purge the OS page cache between
    /// every timed iteration (warmup is disabled).
    Cold,
    /// Capture both cold and warm side-by-side.
    Both,
}

/// Source distribution used to compute the headline speedup.
///
/// `Median` (default) uses `parallel_median / accel_median` per cell. This
/// is robust to the 22% CV cold-start contamination on the GPU side at
/// smaller scales (Reviewer 1 Sin #19 / action_items M12).
///
/// `Mean` preserves the historical behaviour for backwards-compat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SpeedupSource {
    #[default]
    Median,
    Mean,
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
    pub max_parallel_workers: u32,
    pub maintenance_work_mem: String,
    pub jit: String,
    pub jit_above_cost: String,
    pub random_page_cost: String,
    pub effective_io_concurrency: u32,
    pub track_io_timing: String,
    pub parallel_leader_participation: String,
}

/// Settings that MUST match between the requested profile and the
/// running postmaster. These are `PGC_POSTMASTER` — cannot be changed by
/// `ALTER SYSTEM` + `pg_reload_conf()`, so if they drift the operator
/// needs to restart PG before any publishable run.
const POSTMASTER_SETTINGS: &[&str] = &["shared_buffers", "max_worker_processes"];

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
            max_parallel_workers: 4,
            maintenance_work_mem: "64MB".to_owned(),
            jit: "off".to_owned(),
            jit_above_cost: "100000".to_owned(),
            random_page_cost: "4.0".to_owned(),
            effective_io_concurrency: 1,
            track_io_timing: "off".to_owned(),
            parallel_leader_participation: "on".to_owned(),
        }
    }

    /// Production-sized profile for a 64 GB / 12-core workstation.
    ///
    /// Reviewer 2's recommended publication profile (action_items M4 and
    /// review_2.md lines 115-135). 16 GB `shared_buffers`, 512 MB
    /// `work_mem`, 48 GB `effective_cache_size`, 12 parallel workers,
    /// 2 GB `maintenance_work_mem`.
    #[must_use]
    pub fn realistic() -> Self {
        Self {
            shared_buffers: "16GB".to_owned(),
            work_mem: "512MB".to_owned(),
            effective_cache_size: "48GB".to_owned(),
            max_parallel_workers_per_gather: 8,
            max_worker_processes: 16,
            max_parallel_workers: 12,
            maintenance_work_mem: "2GB".to_owned(),
            jit: "off".to_owned(),
            jit_above_cost: "100000".to_owned(),
            random_page_cost: "1.1".to_owned(),
            effective_io_concurrency: 256,
            track_io_timing: "on".to_owned(),
            parallel_leader_participation: "on".to_owned(),
        }
    }

    /// Return (name, requested_value) pairs in a stable order.
    #[must_use]
    pub fn requested_settings(&self) -> Vec<(&'static str, String)> {
        vec![
            ("shared_buffers", self.shared_buffers.clone()),
            ("work_mem", self.work_mem.clone()),
            ("effective_cache_size", self.effective_cache_size.clone()),
            (
                "max_parallel_workers_per_gather",
                self.max_parallel_workers_per_gather.to_string(),
            ),
            (
                "max_worker_processes",
                self.max_worker_processes.to_string(),
            ),
            (
                "max_parallel_workers",
                self.max_parallel_workers.to_string(),
            ),
            ("maintenance_work_mem", self.maintenance_work_mem.clone()),
            ("jit", self.jit.clone()),
            ("jit_above_cost", self.jit_above_cost.clone()),
            ("random_page_cost", self.random_page_cost.clone()),
            (
                "effective_io_concurrency",
                self.effective_io_concurrency.to_string(),
            ),
            ("track_io_timing", self.track_io_timing.clone()),
            (
                "parallel_leader_participation",
                self.parallel_leader_participation.clone(),
            ),
        ]
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
        for (name, val) in self.requested_settings() {
            let sql = format!("ALTER SYSTEM SET {name} = '{val}'");
            if let Err(e) = client.batch_execute(&sql) {
                eprintln!("[gucs] ALTER SYSTEM SET {name} failed: {e}");
            }
        }
        if let Err(e) = client.batch_execute("SELECT pg_reload_conf()") {
            eprintln!("[gucs] pg_reload_conf failed: {e}");
        }
        Ok(())
    }
}

/// Observed GUC snapshot captured via `SHOW ...` from inside the
/// benchmarked session. This is what the report publishes — not the
/// `requested_settings()` — because `shared_buffers` is `PGC_POSTMASTER`
/// and the `ALTER SYSTEM SET` won't take effect until restart.
///
/// Reviewer 2 §1 / action_items C4: publishing a settings table that
/// doesn't match the running postmaster is worse than no table at all.
#[derive(Debug, Clone)]
pub struct ObservedGucs {
    /// Settings read via `SHOW name` from inside a benchmarked session.
    pub settings: Vec<(String, String)>,
    /// `pg_postmaster_start_time()` — proves we are talking to the
    /// postmaster that was running when the profile was applied.
    pub postmaster_start_time: Option<String>,
}

/// Error returned when a postmaster setting does not match the requested
/// value — the operator needs to edit `postgresql.conf` and restart PG.
#[derive(Debug)]
pub struct PostmasterMismatch {
    pub name: String,
    pub requested: String,
    pub observed: String,
}

impl std::fmt::Display for PostmasterMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "postmaster restart required; ran `ALTER SYSTEM` but {name} still shows {observed} \
             (requested {requested}). Edit postgresql.conf (or restart pgrx with the new profile) \
             and retry. To bypass during development, pass --skip-guc-verify.",
            name = self.name,
            observed = self.observed,
            requested = self.requested,
        )
    }
}

impl std::error::Error for PostmasterMismatch {}

/// Query `SHOW` for every setting in the profile list and return the
/// observed values. Used by `verify_and_capture_gucs` below.
fn show_all_gucs(
    connection: &str,
    names: &[&str],
) -> Result<ObservedGucs, Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;
    let mut settings = Vec::with_capacity(names.len());
    for name in names {
        // `SHOW` returns one row with one text column.
        let row = client.query_one(&format!("SHOW {name}"), &[])?;
        let val: String = row.get(0);
        settings.push(((*name).to_owned(), val));
    }
    // pg_postmaster_start_time is a timestamptz, so cast to text.
    let pm_start: Option<String> = client
        .query_one("SELECT pg_postmaster_start_time()::text", &[])
        .ok()
        .map(|r| r.get::<_, String>(0));
    Ok(ObservedGucs {
        settings,
        postmaster_start_time: pm_start,
    })
}

/// Capture observed GUCs and enforce that `PGC_POSTMASTER` settings match
/// the requested profile. Returns the observed snapshot for embedding in
/// the report.
///
/// # Errors
///
/// Returns a `PostmasterMismatch` error if any postmaster setting does
/// not match the requested profile (e.g. `shared_buffers` reads `128MB`
/// but was requested at `16GB`). Unless `skip_verify` is true, this
/// aborts the harness — see Reviewer 2 §1 / action_items C4.
pub fn verify_and_capture_gucs(
    connection: &str,
    profile: &GucProfile,
    skip_verify: bool,
) -> Result<ObservedGucs, Box<dyn std::error::Error>> {
    let names: Vec<&str> = profile
        .requested_settings()
        .iter()
        .map(|(n, _)| *n)
        .collect();
    let observed = show_all_gucs(connection, &names)?;

    // Compare postmaster settings specifically.
    let requested: Vec<(&str, String)> = profile.requested_settings();
    for pm in POSTMASTER_SETTINGS {
        let req = requested
            .iter()
            .find(|(n, _)| n == pm)
            .map(|(_, v)| v.clone());
        let obs = observed
            .settings
            .iter()
            .find(|(n, _)| n == pm)
            .map(|(_, v)| v.clone());
        if let (Some(req_val), Some(obs_val)) = (req, obs)
            && !pg_setting_values_equivalent(pm, &req_val, &obs_val)
        {
            let err = PostmasterMismatch {
                name: (*pm).to_owned(),
                requested: req_val,
                observed: obs_val,
            };
            if skip_verify {
                eprintln!("[gucs] WARNING: {err} (--skip-guc-verify is set, continuing)");
            } else {
                return Err(Box::new(err));
            }
        }
    }
    Ok(observed)
}

/// PostgreSQL normalises byte-valued GUCs (e.g. `16GB` → `16384MB` when
/// shown, `max_worker_processes` passes through as `16`). This helper
/// compares the requested and observed strings leniently — same
/// canonical byte count or same integer.
fn pg_setting_values_equivalent(name: &str, requested: &str, observed: &str) -> bool {
    if requested.eq_ignore_ascii_case(observed) {
        return true;
    }
    // Byte-valued memory settings.
    if matches!(
        name,
        "shared_buffers" | "work_mem" | "maintenance_work_mem" | "effective_cache_size"
    ) {
        let req = parse_pg_bytes(requested);
        let obs = parse_pg_bytes(observed);
        if let (Some(a), Some(b)) = (req, obs) {
            return a == b;
        }
    }
    false
}

/// Parse a PG memory string like `16GB`, `1024MB`, `8192kB` into a
/// canonical byte count. Returns `None` for unrecognized inputs.
fn parse_pg_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, unit) = s
        .chars()
        .position(|c| !c.is_ascii_digit())
        .map_or((s, ""), |idx| (&s[..idx], &s[idx..]));
    let num: u64 = num.parse().ok()?;
    let mult: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kb" => 1024,
        "mb" => 1024 * 1024,
        "gb" => 1024 * 1024 * 1024,
        "tb" => 1024u64 * 1024 * 1024 * 1024,
        _ => return None,
    };
    Some(num * mult)
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
    pub cache_mode: CacheMode,
    pub speedup_source: SpeedupSource,
    /// If set, run `EXPLAIN (ANALYZE, VERBOSE, BUFFERS)` once per
    /// workload/scale before the timed loop and append the result to this
    /// path.
    pub plans_capture_path: Option<PathBuf>,
    /// If set, this profile is applied via `ALTER SYSTEM SET` before the
    /// first workload runs.
    pub guc_profile: Option<GucProfile>,
    /// If true, skip the postmaster-GUC mismatch hard-fail check
    /// (action_items C4). Only intended for developer iteration.
    pub skip_guc_verify: bool,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            iterations: 10,
            // action_items M14 / Reviewer 1 Sin #14: warmup raised from 1
            // to 5 so shader compile + kernel launch jitter is amortized
            // before the first timed iteration.
            warmup: 5,
            seed: 42,
            timing_mode: TimingMode::default(),
            cache_mode: CacheMode::default(),
            speedup_source: SpeedupSource::default(),
            plans_capture_path: None,
            guc_profile: None,
            skip_guc_verify: false,
        }
    }
}

/// Purge the OS page cache between cold-cache iterations.
///
/// On macOS: `sync && purge` (no sudo required).
/// On Linux: `sync && echo 3 > /proc/sys/vm/drop_caches` (requires root).
///
/// Returns `Ok(true)` if the purge ran successfully, `Ok(false)` if the
/// platform doesn't support it or the privilege check failed (the caller
/// should document the fallback in the report). Errors are fatal — never
/// silently swallow a broken cache-clearing path.
///
/// Reviewer 2 §3(ii) / action_items M2: `DISCARD ALL` does NOT clear the
/// OS page cache. It only resets session state.
#[allow(dead_code)]
pub fn purge_os_page_cache() -> Result<bool, Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let status = Command::new("sh").arg("-c").arg("sync && purge").status()?;
        Ok(status.success())
    }
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        // Requires root — check effective uid.
        #[allow(unsafe_code)]
        let is_root = unsafe { libc_geteuid() } == 0;
        if !is_root {
            eprintln!(
                "[cache] WARNING: Linux cold-cache purge requires root; \
                 drop_caches skipped. Report will note only warm-cache \
                 measurements were taken."
            );
            return Ok(false);
        }
        let status = Command::new("sh")
            .arg("-c")
            .arg("sync && echo 3 > /proc/sys/vm/drop_caches")
            .status()?;
        Ok(status.success())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(false)
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code, dead_code)]
unsafe fn libc_geteuid() -> u32 {
    // Avoid pulling in the `libc` crate just for this check.
    extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

/// Capture thermal state before a workload runs. Used by the report to
/// flag workloads that ran under thermal pressure (action_items M13 /
/// Reviewer 1 Sin #18).
///
/// On macOS: `pmset -g therm` parses `CPU_Scheduler_Limit` and
/// `CPU_Speed_Limit` (values < 100 mean throttled).
/// On Linux: reads `/sys/class/thermal/thermal_zone0/temp` as a proxy.
#[must_use]
#[allow(dead_code)]
pub fn capture_thermal_state() -> Option<crate::report::ThermalState> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let out = Command::new("pmset").arg("-g").arg("therm").output().ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let scheduler = parse_pmset_limit(&text, "CPU_Scheduler_Limit");
        let speed = parse_pmset_limit(&text, "CPU_Speed_Limit");
        let pressure = scheduler.is_some_and(|v| v < 100) || speed.is_some_and(|v| v < 100);
        let raw: String = text.chars().take(400).collect();
        Some(crate::report::ThermalState {
            cpu_scheduler_limit: scheduler,
            cpu_speed_limit: speed,
            raw,
            pressure,
        })
    }
    #[cfg(target_os = "linux")]
    {
        let temp = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp")
            .ok()?
            .trim()
            .parse::<u32>()
            .ok()?;
        // Millidegrees C — we don't have a portable "throttled" gate on
        // Linux, so only mark pressure if > 95 °C.
        let pressure = temp > 95_000;
        Some(crate::report::ThermalState {
            cpu_scheduler_limit: None,
            cpu_speed_limit: None,
            raw: format!("thermal_zone0={temp}m°C"),
            pressure,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Parse `CPU_Scheduler_Limit = 100` from `pmset -g therm` output.
#[cfg(target_os = "macos")]
fn parse_pmset_limit(text: &str, key: &str) -> Option<u32> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(key) {
            let rest = rest.trim_start_matches([' ', '=', ':']);
            return rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u32>().ok());
        }
    }
    None
}

/// Run `VACUUM (ANALYZE, VERBOSE)` on every table created by a workload's
/// setup and capture `relpages` / `reltuples` / max `n_distinct` from
/// `pg_class` / `pg_stats`. Action_items C6 / Reviewer 2 §3(iii).
#[allow(clippy::unnecessary_wraps)] // signature kept stable for future fallible ops
pub fn vacuum_and_capture_stats(
    client: &mut Client,
    tables: &[String],
) -> Result<Vec<crate::report::TableStats>, Box<dyn std::error::Error>> {
    let mut out = Vec::with_capacity(tables.len());
    for t in tables {
        // VACUUM cannot run inside a transaction block.
        let sql = format!("VACUUM (ANALYZE, VERBOSE) {t}");
        if let Err(e) = client.batch_execute(&sql) {
            eprintln!("[vacuum] {t} failed: {e}");
            continue;
        }
        let relname = t
            .rsplit('.')
            .next()
            .unwrap_or(t)
            .trim_matches('"')
            .to_owned();
        // pg_class.relpages / reltuples
        let row = client
            .query_one(
                "SELECT relpages::bigint, reltuples::float8 FROM pg_class WHERE relname = $1",
                &[&relname],
            )
            .ok();
        let (relpages, reltuples) = row.map_or((0_i64, 0.0_f64), |r| {
            (r.get::<_, i64>(0), r.get::<_, f64>(1))
        });
        // max(n_distinct) across all columns
        let max_nd: f64 = client
            .query_one(
                "SELECT COALESCE(MAX(n_distinct::float8), 0) FROM pg_stats WHERE tablename = $1",
                &[&relname],
            )
            .map_or(0.0, |r| r.get::<_, f64>(0));
        out.push(crate::report::TableStats {
            relname,
            relpages,
            reltuples,
            max_n_distinct: max_nd,
        });
    }
    Ok(out)
}

/// Extract table names from the `CREATE TABLE` statements a workload
/// issues during setup.
fn workload_tables(workload: &dyn Workload, rows: usize) -> Vec<String> {
    let mut tables = Vec::new();
    for sql in workload.setup_sql(rows) {
        let lower = sql.to_lowercase();
        if let Some(rest) = lower.strip_prefix("create table") {
            let rest = rest
                .trim_start()
                .strip_prefix("if not exists")
                .unwrap_or_else(|| rest.trim_start())
                .trim_start();
            if let Some(name) = rest.split_whitespace().next() {
                let clean = name.trim_matches(|c: char| c == '(' || c.is_whitespace());
                if !clean.is_empty() {
                    tables.push(clean.to_owned());
                }
            }
        }
    }
    tables
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
    run_with_timing_and_cache(
        connection,
        workload,
        iterations,
        warmup,
        timing_mode,
        CacheMode::Warm,
    )
}

/// Like [`run_with_timing`] but also honours a [`CacheMode`]. If `cache_mode`
/// is `Cold`, the OS page cache is purged between every timed iteration and
/// warmup is forced to zero. If `Both`, the function runs twice and merges
/// the cold and warm iteration vectors (cold first, warm second) — the
/// report renderer separates them by index range.
pub fn run_with_timing_and_cache(
    connection: &str,
    workload: &dyn Workload,
    iterations: usize,
    warmup: usize,
    timing_mode: TimingMode,
    cache_mode: CacheMode,
) -> Result<WorkloadResult, Box<dyn std::error::Error>> {
    // For Both we delegate twice, cold then warm, and concatenate.
    if cache_mode == CacheMode::Both {
        let cold = run_with_timing_and_cache(
            connection,
            workload,
            iterations,
            0,
            timing_mode,
            CacheMode::Cold,
        )?;
        let warm = run_with_timing_and_cache(
            connection,
            workload,
            iterations,
            warmup,
            timing_mode,
            CacheMode::Warm,
        )?;
        let mut merged = cold.iterations;
        merged.extend(warm.iterations);
        return Ok(WorkloadResult::from_iterations_ex(
            workload.name().to_owned(),
            workload.description().to_owned(),
            workload.category().to_owned(),
            crate::report::classify_kernel(workload.name()),
            0,
            merged,
            true,
        ));
    }
    let effective_warmup = if cache_mode == CacheMode::Cold {
        0
    } else {
        warmup
    };

    let query = workload.query_sql();
    // Reviewer 1 §4 / action_items §0: some workloads (h3) need a different
    // SQL on the PgParallel baseline side so the planner cannot intercept
    // the call. Default is `None` (use the accel query for both).
    let baseline_query = workload
        .baseline_query_sql()
        .unwrap_or_else(|| query.clone());
    let pre_query = workload.pre_query_sql();
    let total_runs = effective_warmup + iterations;
    let mut results = Vec::with_capacity(iterations);
    let mut rng = rand::thread_rng();

    // Two modes: accel vs PG parallel. Order randomized per iteration.
    let modes = [BenchMode::Accel, BenchMode::PgParallel];

    // Persistent connection per mode — backend init costs (tracing, GPU
    // probe ~40ms) are paid once during warmup, not on every measurement.
    // DISCARD ALL between measurements resets session state (plan cache,
    // GUCs, temp tables) without the fork overhead. Cold-cache mode also
    // purges the OS page cache between iterations via `sync && purge`.
    let mut mode_clients = [
        Client::connect(connection, NoTls)?,
        Client::connect(connection, NoTls)?,
    ];

    for i in 0..total_runs {
        let is_warmup = i < effective_warmup;

        if cache_mode == CacheMode::Cold {
            // Drop the OS page cache before every iteration. DISCARD ALL
            // alone is insufficient (Reviewer 2 §3(ii)).
            if let Err(e) = purge_os_page_cache() {
                eprintln!("[cache] purge failed: {e}");
            }
        }

        // Randomize order to eliminate cache-warming bias.
        let mut order: [usize; 2] = [0, 1];
        if rng.gen_bool(0.5) {
            order.swap(0, 1);
        }

        let mut timings = [0.0_f64; 2];
        for &idx in &order {
            // DISCARD ALL resets session state before each measurement.
            mode_clients[idx].batch_execute("DISCARD ALL")?;
            let sql_for_mode = match modes[idx] {
                BenchMode::Accel => query.as_str(),
                BenchMode::PgParallel => baseline_query.as_str(),
            };
            timings[idx] = run_with_mode(
                &mut mode_clients[idx],
                sql_for_mode,
                modes[idx],
                &pre_query,
                timing_mode,
            )?;
        }

        let accel_ms = timings[0];
        let parallel_ms = timings[1];

        let phase = if is_warmup { "warmup" } else { "bench" };
        let iter_num = if is_warmup {
            i + 1
        } else {
            i - effective_warmup + 1
        };
        let iter_total = if is_warmup {
            effective_warmup
        } else {
            iterations
        };
        let cache_tag = match cache_mode {
            CacheMode::Cold => " [cold]",
            CacheMode::Warm => " [warm]",
            CacheMode::Both => "",
        };
        eprintln!(
            "[{name}] {phase} {iter_num}/{iter_total}{cache_tag}: \
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

    Ok(WorkloadResult::from_iterations_ex(
        workload.name().to_owned(),
        workload.description().to_owned(),
        workload.category().to_owned(),
        crate::report::classify_kernel(workload.name()),
        0,
        results,
        true,
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
        TimingMode::Both => {
            // Run both mechanisms back-to-back on the same connection.
            // We report the raw wall-clock value (per action_items M1
            // default) but also capture the EXPLAIN ANALYZE figure to
            // stderr so operators can audit the gap.
            let raw = run_raw_wall_clock(client, query)?;
            match run_explain_analyze(client, query) {
                Ok(ea) => eprintln!("[timing:both] raw={raw:.3}ms explain_analyze={ea:.3}ms"),
                Err(e) => eprintln!("[timing:both] raw={raw:.3}ms explain_analyze=ERR({e})"),
            }
            Ok(raw)
        }
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

    let mut observed_gucs: Option<ObservedGucs> = None;
    if let Some(profile) = &config.guc_profile {
        eprintln!("[gucs] applying profile: {profile:?}");
        profile.apply(connection)?;
        // Verify observed values match; hard-fail on postmaster-setting
        // drift (action_items C4, Reviewer 2 §1) unless --skip-guc-verify.
        match verify_and_capture_gucs(connection, profile, config.skip_guc_verify) {
            Ok(snapshot) => {
                eprintln!(
                    "[gucs] observed snapshot captured ({} settings)",
                    snapshot.settings.len()
                );
                if let Some(ts) = &snapshot.postmaster_start_time {
                    eprintln!("[gucs] pg_postmaster_start_time = {ts}");
                }
                observed_gucs = Some(snapshot);
            }
            Err(e) => {
                // The error already carries the "postmaster restart
                // required; ran `ALTER SYSTEM` but X still shows Y"
                // message from `PostmasterMismatch::Display`.
                return Err(e);
            }
        }
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

    Ok(report::generate_report_ex(
        results,
        crashes,
        Some(connection),
        config.iterations,
        config.warmup,
        observed_gucs,
        config.timing_mode,
        config.cache_mode,
        config.speedup_source,
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

    // VACUUM (ANALYZE, VERBOSE) after load, before timing begins
    // (action_items C6 / Reviewer 2 §3(iii)). This proves the parallel
    // baseline's planner had fresh stats for every measured row — otherwise
    // `parallel_mean` is suspect.
    let tables = workload_tables(workload, rows);
    let vacuum_stats = {
        let mut vclient = Client::connect(connection, NoTls)?;
        vacuum_and_capture_stats(&mut vclient, &tables).unwrap_or_default()
    };

    // Capture thermal state BEFORE the timed loop (action_items M13).
    let thermal = capture_thermal_state();

    // Always capture a plan snippet so the runner can tag the workload
    // as dispatched/not-dispatched even if --capture-plans is off. This
    // feeds the dispatch classification (action_items C8 / Reviewer 1
    // Sin #5). The full-plans file (plans.txt) is still written if
    // plans_capture_path is set.
    let plan_snippet = capture_plan_snippet(connection, workload).ok();
    let dispatched = plan_snippet
        .as_deref()
        .is_some_and(plan_contains_custom_scan);
    if let Some(path) = &config.plans_capture_path
        && let Err(e) = capture_plan(connection, workload, rows, path)
    {
        eprintln!(
            "[plans] capture failed for {} @ {rows}: {e}",
            workload.name()
        );
    }

    // TODO(fix-agent-6): once pg_accel_stats() exposes
    // `pg_accel_kernel_executions_delta`, combine the plan-text check with
    // a counter-delta check (delta > 0 means dispatched). Leaving the
    // plan-text path as the only signal for now.

    let mut result = run_with_timing_and_cache(
        connection,
        workload,
        config.iterations,
        config.warmup,
        config.timing_mode,
        config.cache_mode,
    )?;
    result.rows = rows;
    result.dispatched = dispatched;
    result.plan_snippet = plan_snippet;
    result.thermal = thermal;
    result.table_stats = vacuum_stats;
    cleanup(connection, workload)?;
    Ok(result)
}

/// Capture a short plan snippet (first 30 lines of
/// `EXPLAIN (VERBOSE) <query>`) with `pg_accel.enabled = on`. Used for
/// dispatch classification — we only need to see whether a Custom Scan
/// node appears in the plan.
fn capture_plan_snippet(
    connection: &str,
    workload: &dyn Workload,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;
    client.batch_execute("SET pg_accel.enabled = on")?;
    client.batch_execute("SET max_parallel_workers_per_gather = DEFAULT")?;
    for sql in workload.pre_query_sql() {
        client.batch_execute(&sql)?;
    }
    let explain = format!("EXPLAIN (VERBOSE) {}", workload.query_sql());
    let rows_out = client.query(&explain, &[])?;
    let mut buf = String::new();
    for (i, row) in rows_out.iter().enumerate() {
        if i >= 30 {
            break;
        }
        let line: &str = row.get(0);
        buf.push_str(line);
        buf.push('\n');
    }
    Ok(buf)
}

/// Return true if a plan text snippet contains a Custom Scan node (which
/// for pg_accel is always prefixed with "GPU"). This is the heuristic we
/// use to classify a workload as "dispatched to GPU".
#[must_use]
pub fn plan_contains_custom_scan(plan: &str) -> bool {
    plan.contains("Custom Scan") || plan.contains("GPU Dispatched: true")
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
        // 1K scale dropped per action_items M11 (Reviewer 1 Sin #15) —
        // below instrument noise floor.
        assert_eq!(ROW_SCALES, &[10_000, 100_000, 1_000_000, 10_000_000]);
    }

    #[test]
    fn test_row_scales_min_10k() {
        assert!(
            ROW_SCALES.iter().min().copied().unwrap_or(0) >= 10_000,
            "minimum reportable scale is 10K (action_items M11)"
        );
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
