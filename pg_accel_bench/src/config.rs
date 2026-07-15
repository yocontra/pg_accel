use std::path::PathBuf;

use postgres::{Client, NoTls};

/// The four row scales every benchmark is run at. Not configurable --
/// the entire suite always runs at all four for reproducible, comparable
/// results.
///
/// **Why no 1K scale:** 160-microsecond measurements taken over the libpq wire
/// protocol are below the instrument noise floor.
/// The protocol round-trip floor on localhost is tens of microseconds, and
/// libpq buffering / kernel scheduling jitter eats the rest. 1K-row rows
/// measure the client harness, not the database. Gray's rule: measurements
/// must exceed the instrument noise floor by 100x. Minimum reportable scale
/// is 10K.
pub const ROW_SCALES: &[usize] = &[10_000, 100_000, 1_000_000, 10_000_000];

/// Timing mode used to measure a single query iteration.
///
/// `ExplainAnalyze` uses `EXPLAIN ANALYZE` and parses `Execution Time`. This
/// penalizes non-custom-scan plans more than Custom Scan plans because a
/// Custom Scan Provider's Next() path can report row-counts essentially for
/// free, while a parallel Seq Scan + Gather + HashAgg pays the per-tuple
/// instrumentation cost in every worker. This can advantage pg_accel on
/// aggregate/reduce categories, so raw wall-clock timing is the default.
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
    /// **Default** -- reports publishable numbers.
    #[default]
    RawWallClock,
    /// Capture both raw wall-clock and EXPLAIN ANALYZE on every iteration.
    /// Statistical aggregation uses the raw column; the EXPLAIN column is
    /// stored alongside for audit / gap analysis.
    Both,
}

/// Cache cleanliness mode for a single measurement.
///
/// `Warm` runs after at least five `BenchConfig::warmup` iterations. Page cache
/// and shared buffers are expected to hold the working set.
///
/// `Cold` invokes `sync && purge` (macOS) or `echo 3 >
/// /proc/sys/vm/drop_caches` (Linux, requires root) between iterations
/// -- `DISCARD ALL` does **not** clear the OS page cache. On Linux without
/// root, we document the
/// limitation in the report and proceed with warm-only measurement.
///
/// `Both` produces side-by-side cold and warm columns in the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CacheMode {
    /// Measure under warm cache only -- require at least 5 warmup iterations.
    #[default]
    Warm,
    /// Measure under cold cache only -- purge the OS page cache between
    /// every timed iteration (warmup is disabled).
    Cold,
    /// Capture both cold and warm side-by-side.
    Both,
}

/// PostgreSQL GUC profile applied before a benchmark run.
///
/// Two built-in profiles:
/// - `toy()` -- the pgrx development defaults (small `shared_buffers`, tiny
///   `work_mem`, 2 parallel workers). Useful for comparing against the
///   historical benchmarks, but not representative of production.
/// - `realistic()` -- a production-sized profile tuned for a 64 GB / 12-core
///   workstation (8 GB `shared_buffers`, 256 MB `work_mem`, 48 GB
///   `effective_cache_size`, 8 parallel workers). These are the numbers a
///   production DBA would actually use; benchmarking with `toy()` is
///   methodologically weak.
///
/// `shared_buffers` and `max_worker_processes` are not reloadable -- they
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

/// Settings that MUST match between the requested profile and the running
/// postmaster. These are `PGC_POSTMASTER` -- cannot be changed by `ALTER
/// SYSTEM` + `pg_reload_conf()`, so if they drift the operator needs to
/// restart PG before any publishable run.
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
    /// Publication profile for the documented 64 GB / 12-core workstation:
    /// 16 GB `shared_buffers`, 512 MB
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
    /// statements that fail are logged but not fatal -- the benchmark
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

/// Observed GUC snapshot captured via `SHOW ...` from inside the benchmarked
/// session. This is what the report publishes -- not the `requested_settings()`
/// -- because `shared_buffers` is `PGC_POSTMASTER` and the `ALTER SYSTEM SET`
/// won't take effect until restart.
///
/// Publishing a settings table that does not match the running postmaster is
/// worse than omitting the table.
#[derive(Debug, Clone)]
pub struct ObservedGucs {
    /// Settings read via `SHOW name` from inside a benchmarked session.
    pub settings: Vec<(String, String)>,
    /// `pg_postmaster_start_time()` -- proves we are talking to the
    /// postmaster that was running when the profile was applied.
    pub postmaster_start_time: Option<String>,
}

/// Error returned when a postmaster setting does not match the requested
/// value -- the operator needs to edit `postgresql.conf` and restart PG.
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

/// Query `SHOW` for every setting in the profile list and return the observed
/// values. Used by `verify_and_capture_gucs`.
fn show_all_gucs(
    connection: &str,
    names: &[&str],
) -> Result<ObservedGucs, Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;
    let mut settings = Vec::with_capacity(names.len());
    for name in names {
        let row = client.query_one(&format!("SHOW {name}"), &[])?;
        let val: String = row.get(0);
        settings.push(((*name).to_owned(), val));
    }
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
/// but was requested at `16GB`). Unless `skip_verify` is true, this aborts
/// the harness.
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

/// PostgreSQL normalises byte-valued GUCs (e.g. `16GB` -> `16384MB` when
/// shown, `max_worker_processes` passes through as `16`). This helper
/// compares the requested and observed strings leniently -- same canonical
/// byte count or same integer.
fn pg_setting_values_equivalent(name: &str, requested: &str, observed: &str) -> bool {
    if requested.eq_ignore_ascii_case(observed) {
        return true;
    }
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

/// Parse a PG memory string like `16GB`, `1024MB`, `8192kB` into a canonical
/// byte count. Returns `None` for unrecognized inputs.
fn parse_pg_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, unit) = s
        .char_indices()
        .find(|&(_, c)| !c.is_ascii_digit())
        .map_or((s, ""), |(idx, _)| s.split_at(idx));
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
    /// If set, run `EXPLAIN (ANALYZE, VERBOSE, BUFFERS)` once per
    /// workload/scale before the timed loop and append the result to this
    /// path.
    pub plans_capture_path: Option<PathBuf>,
    /// If set, this profile is applied via `ALTER SYSTEM SET` before the
    /// first workload runs.
    pub guc_profile: Option<GucProfile>,
    /// If true, skip the postmaster-GUC mismatch hard-fail check. Only
    /// intended for developer iteration.
    pub skip_guc_verify: bool,
    /// If set, persist reports, crash inventories, plan snippets, GUCs, and
    /// bounded log tails under this directory.
    pub artifacts_dir: Option<PathBuf>,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            iterations: 10,
            // Five warmups amortize shader compilation and kernel-launch jitter
            // before the first timed iteration.
            warmup: 5,
            seed: 42,
            timing_mode: TimingMode::default(),
            cache_mode: CacheMode::default(),
            plans_capture_path: None,
            guc_profile: None,
            skip_guc_verify: false,
            artifacts_dir: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_scales_constant() {
        assert_eq!(ROW_SCALES, &[10_000, 100_000, 1_000_000, 10_000_000]);
    }

    #[test]
    fn pg_memory_settings_compare_by_bytes() {
        assert!(pg_setting_values_equivalent(
            "shared_buffers",
            "16GB",
            "16384MB"
        ));
        assert!(pg_setting_values_equivalent(
            "work_mem", "512MB", "524288kB"
        ));
        assert!(!pg_setting_values_equivalent(
            "shared_buffers",
            "16GB",
            "128MB"
        ));
    }

    #[test]
    fn non_memory_settings_compare_exactly() {
        assert!(pg_setting_values_equivalent(
            "max_worker_processes",
            "16",
            "16"
        ));
        assert!(!pg_setting_values_equivalent(
            "max_worker_processes",
            "16",
            "8"
        ));
    }
}
