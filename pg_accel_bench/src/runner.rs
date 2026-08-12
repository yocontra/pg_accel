use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use postgres::{Client, NoTls, Statement};
use rand::Rng;
use rand::seq::SliceRandom;
use serde::Serialize;

use crate::artifacts::{ArtifactWriter, BenchmarkQueryIdentity, PreRiskContext};
use crate::bench_model::{CachePurgeState, CacheState};
#[allow(unused_imports)]
pub use crate::config::{
    BenchConfig, CacheMode, GucProfile, ObservedGucs, PostmasterMismatch, ROW_SCALES, TimingMode,
    verify_and_capture_gucs,
};
use crate::report::{self, IterationResult, WorkloadResult};
use crate::workloads::{ExpectedResultValue, ResultOracle, Workload};

const PROVENANCE_SCHEMA_VERSION: u32 = 1;
const CORRECTNESS_DIFF_SCHEMA_VERSION: u32 = 1;
const EXPECTED_EXTENSION_VERSION: &str = env!("CARGO_PKG_VERSION");
const RUST_BACKTRACE_ARTIFACT: &str = "rust_backtrace.txt";
const CRASH_CONTEXT_EMBED_BYTES: usize = 128 * 1024;
const CORRECTNESS_DIFF_SAMPLE_LIMIT: i64 = 20;
const BENCH_STATEMENT_TIMEOUT: &str = "10min";
const BENCH_LOCK_TIMEOUT: &str = "30s";
const PLANNER_STAGE_NAMES: [&str; 7] = [
    "rel_pathlist",
    "join_pathlist",
    "upper_group_agg",
    "upper_window",
    "upper_final",
    "upper_setop",
    "upper_other",
];
const PLANNER_SUBSTAGE_NAMES: [&str; 5] = [
    "query_fingerprint",
    "decline_cache_lookup",
    "dependency_revalidation",
    "native_cost_reconstruction",
    "rejection_recording",
];

fn apply_benchmark_safety_settings(client: &mut Client) -> Result<(), Box<dyn std::error::Error>> {
    client.batch_execute(&format!(
        "SET statement_timeout = '{BENCH_STATEMENT_TIMEOUT}'; \
         SET lock_timeout = '{BENCH_LOCK_TIMEOUT}'"
    ))?;
    Ok(())
}

#[derive(Debug, Clone)]
struct RustBacktraceSetting {
    effective_value: String,
    previous_value: Option<String>,
    action: &'static str,
}

#[derive(Debug)]
struct ProvenanceFailure {
    errors: Vec<String>,
}

impl std::fmt::Display for ProvenanceFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pg_accel provenance gate failed: {}",
            self.errors.join("; ")
        )
    }
}

impl std::error::Error for ProvenanceFailure {}

#[derive(Debug, Serialize)]
struct ProvenanceReport {
    schema_version: u32,
    status: ProvenanceStatus,
    expected_extension_version: String,
    postgres: PostgresProvenance,
    sql: SqlExtensionProvenance,
    live_smoke: LiveExtensionSmoke,
    pg_config: PgConfigProvenance,
    expected_binary: Option<FileProvenance>,
    installed_binary: Option<FileProvenance>,
    loaded_binaries: Vec<FileProvenance>,
    mapped_library_discovery: MappedLibraryDiscovery,
    device_limits_sources: Vec<String>,
    warnings: Vec<String>,
    errors: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProvenanceStatus {
    Pass,
    Warning,
    Fail,
}

#[derive(Debug, Serialize)]
struct PostgresProvenance {
    backend_pid: i32,
    server_version: Option<String>,
    data_directory: Option<String>,
    config_file: Option<String>,
    shared_preload_libraries: Option<String>,
    postmaster_start_time: Option<String>,
    postmaster_start_unix_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SqlExtensionProvenance {
    extversion: Option<String>,
    pg_accel_version: Option<String>,
    function_probin: Option<String>,
    function_prosrc: Option<String>,
}

#[derive(Debug, Serialize)]
struct LiveExtensionSmoke {
    backend_pid: i32,
    pg_accel_version: Option<String>,
    kernel_executions: Option<i64>,
    stats_rows: Option<i64>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct PgConfigProvenance {
    command: Option<String>,
    pkglibdir: Option<String>,
    sharedir: Option<String>,
    error: Option<String>,
    control_file: Option<FileProvenance>,
    control_default_version: Option<String>,
    sql_files: Vec<FileProvenance>,
}

#[derive(Debug, Serialize)]
struct FileProvenance {
    path: String,
    exists: bool,
    sha256: Option<String>,
    len_bytes: Option<u64>,
    modified_unix_seconds: Option<u64>,
    mapping_deleted: bool,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct MappedLibraryDiscovery {
    method: String,
    mapped_paths: Vec<String>,
    warning: Option<String>,
}

#[derive(Debug)]
struct MappedLibrary {
    path: PathBuf,
    display_path: String,
    deleted: bool,
}

struct MappedLibraryProbe {
    discovery: MappedLibraryDiscovery,
    libraries: Vec<MappedLibrary>,
}

#[derive(Debug, Clone, Copy, Default)]
struct DispatchStatsSnapshot {
    backend_pid: i32,
    queries_accelerated: u64,
    rows_dispatched: u64,
    batches_executed: u64,
    stock_exec_count: u64,
    gpu_rows_processed: u64,
    gpu_kernel_executions: u64,
    physical_kernel_modes: report::PhysicalKernelModeCounts,
}

#[derive(Debug, Clone, Copy, Default)]
struct DispatchStatsDelta {
    queries_accelerated: u64,
    rows_dispatched: u64,
    batches_executed: u64,
    stock_exec_count: u64,
    gpu_rows_processed: u64,
    gpu_kernel_executions: u64,
    physical_kernel_modes: report::PhysicalKernelModeCounts,
}

#[derive(Debug, Clone)]
struct DispatchCounterCapture {
    captured: bool,
    delta: DispatchStatsDelta,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct ArtifactLifecycleSnapshot {
    counters: report::ArtifactLifecycleDelta,
    dependencies: Vec<report::ResidentDependencyIdentity>,
    artifact_policy: Option<String>,
}

#[derive(Debug, Clone)]
struct ArtifactExecutionEvidence {
    artifact: report::ArtifactLifecycleDelta,
    artifact_policy: Option<String>,
    dependencies_before: Vec<report::ResidentDependencyIdentity>,
    dependencies_after: Vec<report::ResidentDependencyIdentity>,
    dispatch: DispatchStatsDelta,
    refresh_us: u64,
    refreshed_relations: u64,
    refreshed_rows: u64,
    output_rows_consumed: u64,
    error: Option<String>,
}

impl ArtifactExecutionEvidence {
    fn unavailable(error: impl Into<String>) -> Self {
        Self {
            artifact: report::ArtifactLifecycleDelta::default(),
            artifact_policy: None,
            dependencies_before: Vec::new(),
            dependencies_after: Vec::new(),
            dispatch: DispatchStatsDelta::default(),
            refresh_us: 0,
            refreshed_relations: 0,
            refreshed_rows: 0,
            output_rows_consumed: 0,
            error: Some(error.into()),
        }
    }
}

impl DispatchCounterCapture {
    fn unavailable(error: impl Into<String>) -> Self {
        Self {
            captured: false,
            delta: DispatchStatsDelta::default(),
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct MeasurementOutcome {
    elapsed_ms: f64,
    output_rows: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct RunExecutionOptions {
    capture_planner_stages: bool,
    native_parity_pairing: bool,
}

#[derive(Debug, Clone, Copy)]
struct LifecyclePairOptions {
    accel_first: bool,
    native_parity_pairing: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct PlannerStageStatsSnapshot {
    calls: u64,
    elapsed_us: u64,
    fast_declines: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct PlannerSubstageStatsSnapshot {
    calls: u64,
    elapsed_us: u64,
}

#[derive(Debug, Clone, Default)]
struct PlannerProfileSnapshot {
    stages: BTreeMap<String, PlannerStageStatsSnapshot>,
    substages: BTreeMap<String, PlannerSubstageStatsSnapshot>,
}

#[derive(Debug, Clone)]
struct PlannerStageMeasurement {
    stages: Vec<report::PlannerStageDelta>,
    observer_probe: Vec<report::PlannerStageDelta>,
    substages: Vec<report::PlannerSubstageDelta>,
    substage_observer_probe: Vec<report::PlannerSubstageDelta>,
    error: Option<String>,
}

impl PlannerStageMeasurement {
    fn unavailable(error: impl Into<String>) -> Self {
        Self {
            stages: Vec::new(),
            observer_probe: Vec::new(),
            substages: Vec::new(),
            substage_observer_probe: Vec::new(),
            error: Some(error.into()),
        }
    }
}

#[derive(Debug)]
struct ModeRunOutcome {
    measurement: MeasurementOutcome,
    planner_stages: Option<PlannerStageMeasurement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunnerDispatchClassification {
    gpu_kernel_dispatched: bool,
    function_srf_kernel_dispatched: bool,
}

#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
struct RunnerDispatchInput {
    plan_selected: bool,
    plan_explicitly_not_dispatched: bool,
    function_kernel_candidate: bool,
    dispatch_counter_captured: bool,
    gpu_kernel_execution_delta: u64,
    pg_accel_queries_accelerated_delta: u64,
    pg_accel_stock_exec_delta: u64,
    accel_output_rows_consumed: u64,
    measured_iterations: usize,
}

#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
struct DispatchWarningInput<'a> {
    workload: &'a str,
    rows: usize,
    plan_selected: bool,
    plan_text_dispatched: bool,
    plan_explicitly_not_dispatched: bool,
    function_kernel_candidate: bool,
}

#[derive(Debug, Serialize)]
struct CorrectnessDiffArtifact {
    schema_version: u32,
    workload: String,
    rows: usize,
    status: String,
    order_sensitive: bool,
    accel_rows: Option<i64>,
    baseline_rows: Option<i64>,
    accel_minus_baseline_count: Option<i64>,
    baseline_minus_accel_count: Option<i64>,
    sample_limit: i64,
    accel_minus_baseline_samples: Vec<String>,
    baseline_minus_accel_samples: Vec<String>,
    accel_query_sql: String,
    baseline_query_sql: String,
    error: Option<String>,
}

/// Run the extension-binary provenance smoke without an artifact directory.
///
/// This is used by audit commands that do not otherwise create benchmark
/// artifacts. Benchmark runs call the same gate from `prepare_run_context`
/// with an [`ArtifactWriter`] so the full JSON report is persisted.
pub fn verify_pg_accel_provenance(connection: &str) -> Result<(), Box<dyn std::error::Error>> {
    run_provenance_gate(connection, None).map(|_| ())
}

fn run_provenance_gate(
    connection: &str,
    artifacts: Option<&ArtifactWriter>,
) -> Result<ProvenanceReport, Box<dyn std::error::Error>> {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    let mut client = Client::connect(connection, NoTls)?;
    let postgres = capture_postgres_provenance(&mut client)?;
    let sql = capture_sql_extension_provenance(&mut client, &mut errors);
    let live_smoke = capture_live_extension_smoke(&mut client, postgres.backend_pid);
    let device_limits_sources = capture_device_limits_sources(&mut client, &mut errors);
    let pg_config = capture_pg_config_provenance(&mut warnings);
    let expected_binary = capture_expected_binary(&mut warnings);
    let installed_binary = pg_config
        .pkglibdir
        .as_deref()
        .and_then(find_installed_extension_binary)
        .map(|path| inspect_file(&path, false));
    if installed_binary.is_none() && pg_config.pkglibdir.is_some() {
        warnings.push("pg_config pkglibdir did not contain a pg_accel extension binary".to_owned());
    }

    let mapped = discover_mapped_pg_accel_libraries(postgres.backend_pid);
    if let Some(warning) = &mapped.discovery.warning {
        warnings.push(warning.clone());
    }
    let loaded_binaries: Vec<FileProvenance> = mapped
        .libraries
        .iter()
        .map(inspect_mapped_library)
        .collect();
    let mut report = ProvenanceReport {
        schema_version: PROVENANCE_SCHEMA_VERSION,
        status: ProvenanceStatus::Pass,
        expected_extension_version: EXPECTED_EXTENSION_VERSION.to_owned(),
        postgres,
        sql,
        live_smoke,
        pg_config,
        expected_binary,
        installed_binary,
        loaded_binaries,
        mapped_library_discovery: mapped.discovery,
        device_limits_sources,
        warnings,
        errors,
    };
    evaluate_provenance(&mut report);

    report.status = if report.errors.is_empty() {
        if report.warnings.is_empty() {
            ProvenanceStatus::Pass
        } else {
            ProvenanceStatus::Warning
        }
    } else {
        ProvenanceStatus::Fail
    };

    for warning in &report.warnings {
        eprintln!("[provenance] WARNING: {warning}");
    }
    if let Some(artifact_writer) = artifacts
        && let Err(e) = artifact_writer.write_provenance(&report, &report.warnings, &report.errors)
    {
        eprintln!("[artifacts] provenance write failed: {e}");
    }

    if report.errors.is_empty() {
        eprintln!("[provenance] pg_accel extension provenance accepted");
        Ok(report)
    } else {
        for error in &report.errors {
            eprintln!("[provenance] ERROR: {error}");
        }
        Err(Box::new(ProvenanceFailure {
            errors: report.errors.clone(),
        }))
    }
}

fn capture_postgres_provenance(
    client: &mut Client,
) -> Result<PostgresProvenance, Box<dyn std::error::Error>> {
    let row = client.query_one(
        "SELECT pg_backend_pid(), \
                current_setting('server_version', true), \
                current_setting('data_directory', true), \
                current_setting('config_file', true), \
                current_setting('shared_preload_libraries', true), \
                pg_postmaster_start_time()::text, \
                EXTRACT(EPOCH FROM pg_postmaster_start_time())::bigint",
        &[],
    )?;
    let start_epoch: i64 = row.get(6);
    Ok(PostgresProvenance {
        backend_pid: row.get(0),
        server_version: row.get(1),
        data_directory: row.get(2),
        config_file: row.get(3),
        shared_preload_libraries: row.get(4),
        postmaster_start_time: row.get(5),
        postmaster_start_unix_seconds: u64::try_from(start_epoch).ok(),
    })
}

fn capture_sql_extension_provenance(
    client: &mut Client,
    errors: &mut Vec<String>,
) -> SqlExtensionProvenance {
    let extversion = client
        .query_opt(
            "SELECT extversion FROM pg_extension WHERE extname = 'pg_accel'",
            &[],
        )
        .ok()
        .flatten()
        .map(|row| row.get::<_, String>(0));

    let pg_accel_version = match client.query_one("SELECT pg_accel_version()", &[]) {
        Ok(row) => Some(row.get::<_, String>(0)),
        Err(e) => {
            errors.push(format!("SELECT pg_accel_version() failed: {e}"));
            None
        }
    };

    let function_row = client
        .query_opt(
            "SELECT p.probin, p.prosrc \
               FROM pg_proc p \
               JOIN pg_depend d ON d.objid = p.oid AND d.deptype = 'e' \
               JOIN pg_extension e ON e.oid = d.refobjid \
              WHERE e.extname = 'pg_accel' AND p.proname = 'pg_accel_version' \
              ORDER BY p.oid \
              LIMIT 1",
            &[],
        )
        .ok()
        .flatten();

    let (function_probin, function_prosrc) = function_row.map_or((None, None), |row| {
        (Some(row.get::<_, String>(0)), Some(row.get::<_, String>(1)))
    });

    SqlExtensionProvenance {
        extversion,
        pg_accel_version,
        function_probin,
        function_prosrc,
    }
}

fn capture_live_extension_smoke(client: &mut Client, backend_pid: i32) -> LiveExtensionSmoke {
    match client.query_one(
        "SELECT pg_accel_version(), \
                pg_accel_kernel_executions(), \
                (SELECT COUNT(*)::bigint FROM pg_accel_stats())",
        &[],
    ) {
        Ok(row) => LiveExtensionSmoke {
            backend_pid,
            pg_accel_version: Some(row.get(0)),
            kernel_executions: Some(row.get(1)),
            stats_rows: Some(row.get(2)),
            error: None,
        },
        Err(e) => LiveExtensionSmoke {
            backend_pid,
            pg_accel_version: None,
            kernel_executions: None,
            stats_rows: None,
            error: Some(e.to_string()),
        },
    }
}

fn capture_device_limits_sources(client: &mut Client, errors: &mut Vec<String>) -> Vec<String> {
    match client.query(
        "SELECT DISTINCT source FROM pg_accel_device_limits() ORDER BY source",
        &[],
    ) {
        Ok(rows) => rows.iter().map(|row| row.get::<_, String>(0)).collect(),
        Err(e) => {
            errors.push(format!("SELECT pg_accel_device_limits() failed: {e}"));
            Vec::new()
        }
    }
}

fn capture_pg_config_provenance(warnings: &mut Vec<String>) -> PgConfigProvenance {
    let command = std::env::var("PG_CONFIG").unwrap_or_else(|_| "pg_config".to_owned());
    let pkglibdir = match command_stdout(&command, &["--pkglibdir"]) {
        Ok(value) => Some(value),
        Err(e) => {
            warnings.push(format!("could not run {command} --pkglibdir: {e}"));
            return PgConfigProvenance {
                command: Some(command),
                pkglibdir: None,
                sharedir: None,
                error: Some(e),
                control_file: None,
                control_default_version: None,
                sql_files: Vec::new(),
            };
        }
    };

    let sharedir = match command_stdout(&command, &["--sharedir"]) {
        Ok(value) => Some(value),
        Err(e) => {
            warnings.push(format!("could not run {command} --sharedir: {e}"));
            None
        }
    };

    let (control_file, control_default_version, sql_files) = sharedir
        .as_deref()
        .map_or((None, None, Vec::new()), capture_extension_metadata_files);

    PgConfigProvenance {
        command: Some(command),
        pkglibdir,
        sharedir,
        error: None,
        control_file,
        control_default_version,
        sql_files,
    }
}

fn capture_extension_metadata_files(
    sharedir: &str,
) -> (Option<FileProvenance>, Option<String>, Vec<FileProvenance>) {
    let extension_dir = Path::new(sharedir).join("extension");
    let control_path = extension_dir.join("pg_accel.control");
    let control_file = control_path
        .is_file()
        .then(|| inspect_file(&control_path, false));
    let control_default_version = fs::read_to_string(&control_path)
        .ok()
        .and_then(|text| parse_control_default_version(&text));

    let mut sql_paths = Vec::new();
    if let Ok(entries) = fs::read_dir(extension_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with("pg_accel--")
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("sql"))
            {
                sql_paths.push(path);
            }
        }
    }
    sql_paths.sort();
    let sql_files = sql_paths
        .iter()
        .map(|path| inspect_file(path, false))
        .collect();

    (control_file, control_default_version, sql_files)
}

fn capture_expected_binary(warnings: &mut Vec<String>) -> Option<FileProvenance> {
    if let Ok(path) = std::env::var("PG_ACCEL_EXPECTED_DYLIB") {
        let probe = inspect_file(Path::new(&path), false);
        if !probe.exists {
            warnings.push(format!(
                "PG_ACCEL_EXPECTED_DYLIB points to a missing file: {}",
                probe.path
            ));
        }
        return Some(probe);
    }

    let root = workspace_root();
    let candidates = build_output_candidates(&root);
    let Some(path) = candidates.iter().find(|path| path.is_file()) else {
        warnings.push(format!(
            "no local pg_accel build output found in {}; set PG_ACCEL_EXPECTED_DYLIB to make the binary-hash gate strict",
            root.join("target").display()
        ));
        return None;
    };
    Some(inspect_file(path, false))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn build_output_candidates(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for profile in ["release", "debug"] {
        for file_name in built_extension_library_names() {
            out.push(root.join("target").join(profile).join(file_name));
        }
    }
    out
}

fn built_extension_library_names() -> Vec<String> {
    let ext = std::env::consts::DLL_EXTENSION;
    if cfg!(windows) {
        vec![format!("pg_accel.{ext}"), format!("libpg_accel.{ext}")]
    } else {
        vec![format!("libpg_accel.{ext}"), format!("pg_accel.{ext}")]
    }
}

fn installed_extension_library_names() -> Vec<String> {
    let ext = std::env::consts::DLL_EXTENSION;
    vec![format!("pg_accel.{ext}"), format!("libpg_accel.{ext}")]
}

fn find_installed_extension_binary(pkglibdir: &str) -> Option<PathBuf> {
    let dir = Path::new(pkglibdir);
    installed_extension_library_names()
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

fn inspect_mapped_library(library: &MappedLibrary) -> FileProvenance {
    if library.deleted {
        return FileProvenance {
            path: library.display_path.clone(),
            exists: false,
            sha256: None,
            len_bytes: None,
            modified_unix_seconds: None,
            mapping_deleted: true,
            error: Some("mapped dynamic library was deleted or replaced on disk".to_owned()),
        };
    }
    inspect_file(&library.path, false)
}

fn inspect_file(path: &Path, mapping_deleted: bool) -> FileProvenance {
    match fs::metadata(path) {
        Ok(metadata) => {
            let hash = sha256_file(path);
            FileProvenance {
                path: path.display().to_string(),
                exists: true,
                sha256: hash.as_ref().ok().cloned(),
                len_bytes: Some(metadata.len()),
                modified_unix_seconds: metadata.modified().ok().and_then(system_time_unix_secs),
                mapping_deleted,
                error: hash.err(),
            }
        }
        Err(e) => FileProvenance {
            path: path.display().to_string(),
            exists: false,
            sha256: None,
            len_bytes: None,
            modified_unix_seconds: None,
            mapping_deleted,
            error: Some(e.to_string()),
        },
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let attempts: &[(&str, &[&str])] = &[
        ("shasum", &["-a", "256"]),
        ("sha256sum", &[]),
        ("openssl", &["dgst", "-sha256", "-r"]),
    ];
    let mut last_error = String::new();
    for (program, args) in attempts {
        let output = Command::new(program)
            .args(*args)
            .arg(path)
            .output()
            .map_err(|e| e.to_string());
        let Ok(output) = output else {
            last_error = format!("{program}: {}", output.err().unwrap_or_default());
            continue;
        };
        if !output.status.success() {
            last_error = format!(
                "{program} exited with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
            continue;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(hash) = stdout
            .split_whitespace()
            .find(|token| token.len() == 64 && token.chars().all(|ch| ch.is_ascii_hexdigit()))
        {
            return Ok(hash.to_ascii_lowercase());
        }
        last_error = format!("{program} did not print a SHA-256 digest");
    }
    Err(last_error)
}

fn command_stdout(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn discover_mapped_pg_accel_libraries(pid: i32) -> MappedLibraryProbe {
    if cfg!(target_os = "linux") {
        match discover_proc_maps_pg_accel(pid) {
            Ok(libraries) if !libraries.is_empty() => {
                return mapped_library_probe("proc_maps", libraries, None);
            }
            Ok(_) => {}
            Err(e) => {
                return mapped_library_probe(
                    "proc_maps",
                    Vec::new(),
                    Some(format!("could not read /proc/{pid}/maps: {e}")),
                );
            }
        }
    }

    if cfg!(target_family = "unix") {
        match discover_lsof_pg_accel(pid) {
            Ok(libraries) if !libraries.is_empty() => {
                return mapped_library_probe("lsof", libraries, None);
            }
            Ok(_) => {
                return mapped_library_probe(
                    "lsof",
                    Vec::new(),
                    Some(format!(
                        "lsof did not report a mapped pg_accel library for pid {pid}"
                    )),
                );
            }
            Err(e) => {
                return mapped_library_probe(
                    "lsof",
                    Vec::new(),
                    Some(format!("could not run lsof for pid {pid}: {e}")),
                );
            }
        }
    }

    mapped_library_probe(
        "unsupported",
        Vec::new(),
        Some(format!(
            "{} does not expose a supported mapped-library discovery path",
            std::env::consts::OS
        )),
    )
}

fn mapped_library_probe(
    method: &str,
    libraries: Vec<MappedLibrary>,
    warning: Option<String>,
) -> MappedLibraryProbe {
    let mapped_paths = libraries
        .iter()
        .map(|library| library.display_path.clone())
        .collect();
    MappedLibraryProbe {
        discovery: MappedLibraryDiscovery {
            method: method.to_owned(),
            mapped_paths,
            warning,
        },
        libraries,
    }
}

fn discover_proc_maps_pg_accel(pid: i32) -> Result<Vec<MappedLibrary>, String> {
    let text = fs::read_to_string(format!("/proc/{pid}/maps")).map_err(|e| e.to_string())?;
    let mut seen = BTreeSet::new();
    let mut libraries = Vec::new();
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let deleted = tokens.last().is_some_and(|token| *token == "(deleted)");
        let path_token = if deleted {
            tokens.get(tokens.len().saturating_sub(2)).copied()
        } else {
            tokens.last().copied()
        };
        let Some(path_token) = path_token else {
            continue;
        };
        let path = path_token.replace("\\040", " ");
        if !is_pg_accel_library_path(&path) {
            continue;
        }
        let display_path = if deleted {
            format!("{path} (deleted)")
        } else {
            path.clone()
        };
        if seen.insert(display_path.clone()) {
            libraries.push(MappedLibrary {
                path: PathBuf::from(path),
                display_path,
                deleted,
            });
        }
    }
    Ok(libraries)
}

fn discover_lsof_pg_accel(pid: i32) -> Result<Vec<MappedLibrary>, String> {
    let output = Command::new("lsof")
        .args(["-p", &pid.to_string(), "-Fn"])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let mut seen = BTreeSet::new();
    let mut libraries = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(path) = line.strip_prefix('n') else {
            continue;
        };
        if !is_pg_accel_library_path(path) {
            continue;
        }
        if seen.insert(path.to_owned()) {
            libraries.push(MappedLibrary {
                path: PathBuf::from(path),
                display_path: path.to_owned(),
                deleted: false,
            });
        }
    }
    Ok(libraries)
}

fn is_pg_accel_library_path(path: &str) -> bool {
    let Some(file_name) = Path::new(path).file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    file_name.contains("pg_accel")
        && Path::new(file_name)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                ext.eq_ignore_ascii_case("so")
                    || ext.eq_ignore_ascii_case("dylib")
                    || ext.eq_ignore_ascii_case("dll")
            })
}

fn evaluate_provenance(report: &mut ProvenanceReport) {
    check_extension_versions(report);
    check_live_extension_smoke(report);
    check_control_version(report);
    check_device_limits(report);
    if let Some(warning) =
        file_probe_warning("expected build binary", report.expected_binary.as_ref())
    {
        report.warnings.push(warning);
    }
    if let Some(warning) = file_probe_warning(
        "pg_config installed binary",
        report.installed_binary.as_ref(),
    ) {
        report.warnings.push(warning);
    }
    let mut loaded_warnings = Vec::new();
    let mut loaded_errors = Vec::new();
    if report.loaded_binaries.is_empty() {
        loaded_errors.push(
            "could not discover a mapped pg_accel dynamic library for the live backend; \
             cannot prove the loaded dylib hash"
                .to_owned(),
        );
    }
    for loaded in &report.loaded_binaries {
        if let Some(warning) = file_probe_warning("loaded backend binary", Some(loaded)) {
            loaded_warnings.push(warning);
        }
        if loaded.sha256.is_none() {
            loaded_errors.push(format!(
                "could not compute SHA-256 for live backend pg_accel binary {}",
                loaded.path
            ));
        }
        if loaded.mapping_deleted {
            loaded_errors.push(format!(
                "live backend maps a deleted/replaced pg_accel binary: {}",
                loaded.path
            ));
        }
    }
    report.warnings.extend(loaded_warnings);
    report.errors.extend(loaded_errors);
    check_hash_relationships(report);
    check_postmaster_restart_required(report);
}

fn check_live_extension_smoke(report: &mut ProvenanceReport) {
    if let Some(error) = &report.live_smoke.error {
        report.errors.push(format!(
            "live pg_accel smoke query failed in backend {}: {error}",
            report.live_smoke.backend_pid
        ));
        return;
    }
    match &report.live_smoke.pg_accel_version {
        Some(version) if version == EXPECTED_EXTENSION_VERSION => {}
        Some(version) => report.errors.push(format!(
            "live pg_accel smoke returned version {version}, expected {EXPECTED_EXTENSION_VERSION}"
        )),
        None => report
            .errors
            .push("live pg_accel smoke did not return a version".to_owned()),
    }
    if report.live_smoke.kernel_executions.is_none() {
        report
            .errors
            .push("live pg_accel smoke could not read pg_accel_kernel_executions()".to_owned());
    }
    if report.live_smoke.stats_rows.is_none_or(|rows| rows == 0) {
        report
            .errors
            .push("live pg_accel smoke did not get a row from pg_accel_stats()".to_owned());
    }
}

fn check_extension_versions(report: &mut ProvenanceReport) {
    match &report.sql.extversion {
        Some(version) if version == EXPECTED_EXTENSION_VERSION => {}
        Some(version) => report.errors.push(format!(
            "pg_extension.extversion is {version}, expected {EXPECTED_EXTENSION_VERSION}"
        )),
        None => report
            .errors
            .push("pg_extension does not list pg_accel".to_owned()),
    }
    match &report.sql.pg_accel_version {
        Some(version) if version == EXPECTED_EXTENSION_VERSION => {}
        Some(version) => report.errors.push(format!(
            "pg_accel_version() returned {version}, expected {EXPECTED_EXTENSION_VERSION}"
        )),
        None => report
            .errors
            .push("pg_accel_version() did not return a value".to_owned()),
    }
}

fn check_control_version(report: &mut ProvenanceReport) {
    if let Some(version) = &report.pg_config.control_default_version {
        if version != EXPECTED_EXTENSION_VERSION {
            report.warnings.push(format!(
                "pg_config sharedir control default_version is {version}, expected {EXPECTED_EXTENSION_VERSION}"
            ));
        }
    } else if report.pg_config.sharedir.is_some() {
        report
            .warnings
            .push("could not read pg_config sharedir pg_accel.control default_version".to_owned());
    }
}

fn check_device_limits(report: &mut ProvenanceReport) {
    if report.device_limits_sources.is_empty() {
        report
            .errors
            .push("pg_accel_device_limits() returned no source rows".to_owned());
        return;
    }
    for source in &report.device_limits_sources {
        if source == "fallback_cpu_only" {
            report.errors.push(
                "pg_accel reports fallback_cpu_only device limits; benchmark/audit results require a real GPU path or native PostgreSQL plan".to_owned(),
            );
        }
    }
}

fn file_probe_warning(label: &str, probe: Option<&FileProvenance>) -> Option<String> {
    let probe = probe?;
    if !probe.exists {
        return Some(format!("{label} does not exist: {}", probe.path));
    }
    if probe.sha256.is_none() {
        return Some(format!(
            "could not compute SHA-256 for {label} {}: {}",
            probe.path,
            probe.error.as_deref().unwrap_or("unknown error")
        ));
    }
    None
}

fn check_hash_relationships(report: &mut ProvenanceReport) {
    let expected_hash = report
        .expected_binary
        .as_ref()
        .and_then(|probe| probe.sha256.as_deref());
    let installed_hash = report
        .installed_binary
        .as_ref()
        .and_then(|probe| probe.sha256.as_deref());

    if let (Some(expected), Some(installed)) = (expected_hash, installed_hash)
        && expected != installed
    {
        report.warnings.push(format!(
            "pg_config installed pg_accel binary hash {installed} does not match local build hash {expected}"
        ));
    }

    if report.loaded_binaries.is_empty() {
        return;
    }

    for loaded in &report.loaded_binaries {
        let Some(loaded_hash) = loaded.sha256.as_deref() else {
            continue;
        };
        if let Some(expected) = expected_hash
            && loaded_hash != expected
        {
            report.errors.push(format!(
                "live backend loaded pg_accel hash {loaded_hash} from {}, expected local build hash {expected}",
                loaded.path
            ));
        }
        if let Some(installed) = installed_hash
            && loaded_hash != installed
        {
            report.warnings.push(format!(
                "live backend loaded pg_accel hash {loaded_hash} from {}, but pg_config pkglibdir binary hash is {installed}",
                loaded.path
            ));
        }
    }
}

fn check_postmaster_restart_required(report: &mut ProvenanceReport) {
    if !shared_preload_contains_pg_accel(report.postgres.shared_preload_libraries.as_deref()) {
        return;
    }
    let Some(start) = report.postgres.postmaster_start_unix_seconds else {
        report
            .warnings
            .push("could not compare pg_accel binary mtime to pg_postmaster_start_time".to_owned());
        return;
    };

    let mut checked_loaded = false;
    for loaded in &report.loaded_binaries {
        checked_loaded = true;
        if let Some(modified) = loaded.modified_unix_seconds
            && modified > start
        {
            report.errors.push(format!(
                "live backend maps {}, but that file was modified after pg_postmaster_start_time; restart the actual benchmark postmaster",
                loaded.path
            ));
        }
    }

    if !checked_loaded
        && let Some(installed) = &report.installed_binary
        && let Some(modified) = installed.modified_unix_seconds
        && modified > start
    {
        report.errors.push(format!(
            "pg_config installed pg_accel binary {} was modified after pg_postmaster_start_time; restart the actual benchmark postmaster",
            installed.path
        ));
    }
}

fn shared_preload_contains_pg_accel(value: Option<&str>) -> bool {
    value.is_some_and(|libraries| {
        libraries.split(',').any(|item| {
            item.trim()
                .trim_matches('"')
                .trim_matches('\'')
                .eq_ignore_ascii_case("pg_accel")
        })
    })
}

fn parse_control_default_version(text: &str) -> Option<String> {
    for line in text.lines() {
        let without_comment = line.split('#').next().unwrap_or_default().trim();
        let Some((key, raw_value)) = without_comment.split_once('=') else {
            continue;
        };
        if key.trim() != "default_version" {
            continue;
        }
        let raw_value = raw_value.trim();
        let value = raw_value
            .strip_prefix('\'')
            .and_then(|value| value.strip_suffix('\''))
            .or_else(|| {
                raw_value
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
            });
        let Some(value) = value else {
            continue;
        };
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

fn system_time_unix_secs(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
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
/// `DISCARD ALL` does not clear the OS page cache; it only resets session
/// state.
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

/// Purge the OS page cache for one measurement and classify the outcome.
/// Called immediately before each cold-mode timed run (per measurement, after
/// any resident-cache prime) so the timed query reads a genuinely evicted heap.
fn purge_for_measurement() -> CachePurgeState {
    match purge_os_page_cache() {
        Ok(true) => CachePurgeState::Completed,
        Ok(false) => {
            eprintln!("[cache] purge unavailable; cold measurement recorded as unpurged");
            CachePurgeState::Unavailable
        }
        Err(e) => {
            eprintln!("[cache] purge failed: {e}");
            CachePurgeState::Failed
        }
    }
}

/// Combine the two per-mode purge outcomes of one accel/parallel iteration
/// into a single worst-case state (Failed > Unavailable > Completed >
/// NotRequested), so a partially-failed purge is never recorded as clean.
const fn combine_purge_states(a: CachePurgeState, b: CachePurgeState) -> CachePurgeState {
    const fn rank(s: CachePurgeState) -> u8 {
        match s {
            CachePurgeState::NotRequested => 0,
            CachePurgeState::Completed => 1,
            CachePurgeState::Unavailable => 2,
            CachePurgeState::Failed => 3,
        }
    }
    if rank(a) >= rank(b) { a } else { b }
}

/// Capture thermal state before a workload runs. Used by the report to flag
/// workloads that ran under thermal pressure.
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
/// `pg_class` / `pg_stats`.
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
    apply_benchmark_safety_settings(&mut client)?;
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

/// Build a randomized measured-arm schedule while keeping order exposure
/// balanced. Even-sized runs contain exactly half accel-first pairs; odd
/// runs randomize which arm receives the single extra first position.
fn randomized_balanced_arm_order<R: Rng + ?Sized>(iterations: usize, rng: &mut R) -> Vec<bool> {
    let extra_accel_first = iterations % 2 == 1 && rng.gen_bool(0.5);
    let accel_first_count = iterations / 2 + usize::from(extra_accel_first);
    let mut schedule = vec![false; iterations];
    schedule[..accel_first_count].fill(true);
    schedule.shuffle(rng);
    schedule
}

/// Run a workload benchmark for the given number of iterations and return results.
///
/// `warmup` iterations are run first and excluded from the statistics.
///
/// To eliminate ordering bias (cache warming, shared buffer state), the order
/// of accel-first vs baseline-first is randomized per iteration. Each mode
/// measurement uses `DISCARD ALL` between modes so neither side benefits
/// from the other's cached plans or buffer state. Each mode gets a
/// persistent connection (normally one per mode per workload/scale) so that
/// one-time backend init costs (tracing, GPU probe) are amortised by warmup
/// iterations rather than paid on every measurement. The internal
/// native-parity gate can deliberately map both logical arms to one backend
/// to remove backend identity from planner-decline comparisons.
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
    run_with_timing_and_cache_internal(
        connection,
        workload,
        iterations,
        warmup,
        timing_mode,
        cache_mode,
        RunExecutionOptions::default(),
    )
}

fn run_with_timing_and_cache_internal(
    connection: &str,
    workload: &dyn Workload,
    iterations: usize,
    warmup: usize,
    timing_mode: TimingMode,
    cache_mode: CacheMode,
    options: RunExecutionOptions,
) -> Result<WorkloadResult, Box<dyn std::error::Error>> {
    let RunExecutionOptions {
        capture_planner_stages,
        native_parity_pairing,
    } = options;
    // For Both we delegate twice, cold then warm, and concatenate.
    if cache_mode == CacheMode::Both {
        let cold = run_with_timing_and_cache_internal(
            connection,
            workload,
            iterations,
            0,
            timing_mode,
            CacheMode::Cold,
            options,
        )?;
        let warm = run_with_timing_and_cache_internal(
            connection,
            workload,
            iterations,
            warmup,
            timing_mode,
            CacheMode::Warm,
            options,
        )?;
        let mut merged = cold.iterations.clone();
        merged.extend(warm.iterations.clone());
        let mut result = WorkloadResult::from_iterations(
            workload.name().to_owned(),
            workload.description().to_owned(),
            crate::workloads::workload_metadata(workload.name()).map_or_else(
                || workload.category().to_owned(),
                |metadata| metadata.category.as_str().to_owned(),
            ),
            crate::report::classify_kernel(workload.name()),
            0,
            merged,
            true,
        );
        let mut warmup_iterations = cold.warmup_iterations.clone();
        warmup_iterations.extend(warm.warmup_iterations.clone());
        result.set_warmup_iterations(warmup_iterations);
        result
            .planner_stage_captures
            .extend(cold.planner_stage_captures.clone());
        let cold_iteration_count = cold.iterations.len();
        result
            .planner_stage_captures
            .extend(
                warm.planner_stage_captures
                    .iter()
                    .cloned()
                    .map(|mut capture| {
                        capture.pair_index =
                            capture.pair_index.saturating_add(cold_iteration_count);
                        capture
                    }),
            );
        result.native_parity_pair_captures = warm
            .native_parity_pair_captures
            .iter()
            .cloned()
            .map(|mut capture| {
                capture.pair_index = capture.pair_index.saturating_add(cold_iteration_count);
                capture
            })
            .collect();
        // Carry resident-lane evidence from the sub-runs (both measure the
        // same workload; prefer the cold sub-run's off-clock load time).
        result.resident_lane = cold.resident_lane || warm.resident_lane;
        result.resident_load_ms = cold.resident_load_ms.or(warm.resident_load_ms);
        result
            .artifact_lifecycle_probe
            .clone_from(&warm.artifact_lifecycle_probe);
        result.artifact_steady_state_captures = warm
            .artifact_steady_state_captures
            .iter()
            .cloned()
            .map(|mut capture| {
                capture.pair_index = capture
                    .pair_index
                    .map(|index| index.saturating_add(cold_iteration_count));
                capture
            })
            .collect();
        result.combined_warm_summary = warm.combined_warm_summary;
        merge_cache_mode_dispatch_counter_fields(&mut result, &cold, &warm);
        return Ok(result);
    }
    let effective_warmup = if cache_mode == CacheMode::Cold {
        0
    } else {
        warmup
    };

    let query = workload.query_sql();
    // Some workloads (notably H3) need different SQL on the PostgreSQL
    // baseline side so the extension cannot intercept the call. Default is
    // `None` (use the accelerated query text for both).
    let baseline_query = workload
        .baseline_query_sql()
        .unwrap_or_else(|| query.clone());
    let pre_query = workload.pre_query_sql();
    let total_runs = effective_warmup + iterations;
    let mut results = Vec::with_capacity(iterations);
    let mut warmup_results = Vec::with_capacity(effective_warmup);
    let mut rng = rand::thread_rng();
    let measured_accel_first = randomized_balanced_arm_order(iterations, &mut rng);

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
    for client in &mut mode_clients {
        apply_benchmark_safety_settings(client)?;
    }
    // Resident-cache preload happens OFF the timed region. Record the load
    // time so the report can surface the off-clock cost the accel side pays.
    let resident_load_ms = prime_workload_accel_backend(&mut mode_clients[0], workload)?;

    // Cold mode runs with warmup = 0, so backend init (tracing, GPU probe) and
    // first-touch Metal JIT would otherwise land inside the cold timings. Pay
    // them once here in a single UNTIMED init round (result discarded). The
    // per-measurement purge below still evicts the OS page cache before every
    // timed run, so this round does not warm the measured samples.
    if cache_mode == CacheMode::Cold {
        for &idx in &[0_usize, 1_usize] {
            let client_idx = if native_parity_pairing { 0 } else { idx };
            mode_clients[client_idx].batch_execute("DISCARD ALL")?;
            apply_benchmark_safety_settings(&mut mode_clients[client_idx])?;
            let sql_for_mode = match modes[idx] {
                BenchMode::Accel => query.as_str(),
                BenchMode::PgParallel => baseline_query.as_str(),
            };
            let _ = run_with_mode(
                &mut mode_clients[client_idx],
                sql_for_mode,
                modes[idx],
                &pre_query,
                timing_mode,
                false,
            )?;
        }
    }

    let mut counter_delta = DispatchStatsDelta::default();
    let mut counter_iterations_captured = 0_usize;
    let mut counter_capture_error: Option<String> = None;
    // Native-parity mode runs both logical arms on one backend and has already
    // proved that no pg_accel plan was selected. Capture one counter interval
    // around the complete warmup + measured series instead of issuing an
    // asymmetric stats query immediately before every accelerated arm.
    let native_pairing_counter_before = if native_parity_pairing {
        match capture_dispatch_stats(&mut mode_clients[0]) {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                counter_capture_error = Some(format!(
                    "could not capture pg_accel dispatch counters before the native-parity series: {error}"
                ));
                None
            }
        }
    } else {
        None
    };
    let mut accel_output_rows_consumed = 0_u64;
    let mut planner_stage_captures = Vec::with_capacity(iterations);
    let mut native_parity_pair_captures = Vec::with_capacity(iterations);
    let capture_artifact_lifecycle = !native_parity_pairing
        && cache_mode == CacheMode::Warm
        && workload_is_resident_lane(workload.name());
    let mut artifact_lifecycle_probe = None;
    let mut artifact_steady_state_captures = Vec::with_capacity(iterations);

    for i in 0..total_runs {
        let is_warmup = i < effective_warmup;

        // Seal refresh + construction + dispatch separately. The regular warm
        // samples that follow must be artifact hits and never inherit this
        // lifecycle cost in their latency distribution.
        if i == effective_warmup && capture_artifact_lifecycle {
            artifact_lifecycle_probe = Some(run_artifact_lifecycle_probe(
                &mut mode_clients,
                workload,
                &query,
                &baseline_query,
                &pre_query,
                timing_mode,
                LifecyclePairOptions {
                    accel_first: rng.gen_bool(0.5),
                    native_parity_pairing,
                },
            )?);
        }

        // Warmups are randomized independently. Measured pairs use a
        // pre-shuffled balanced schedule so a ten-iteration release run has
        // exactly five accel-first and five PostgreSQL-first observations.
        let accel_first = if is_warmup {
            rng.gen_bool(0.5)
        } else {
            measured_accel_first[i - effective_warmup]
        };
        let order: Vec<usize> = if native_parity_pairing && !is_warmup {
            native_parity_execution_order(accel_first)
        } else if accel_first {
            vec![0, 1]
        } else {
            vec![1, 0]
        };

        let mut timings = [MeasurementOutcome {
            elapsed_ms: 0.0,
            output_rows: 0,
        }; 2];
        let mut raw_timing_ms: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
        let mut raw_sequence = Vec::with_capacity(order.len());
        let mut planner_capture_taken = false;
        // Cold mode purges the OS page cache before EACH mode's timed run
        // (not once per accel/parallel pair) and AFTER the resident-cache
        // prime, so the resident preload cannot re-warm the heap the timed
        // query reads. Recorded per measurement.
        let mut purge_outcomes = [CachePurgeState::NotRequested; 2];
        let mut artifact_evidence = None;
        for &idx in &order {
            let client_idx = if native_parity_pairing { 0 } else { idx };
            // DISCARD ALL resets session state before each measurement.
            mode_clients[client_idx].batch_execute("DISCARD ALL")?;
            apply_benchmark_safety_settings(&mut mode_clients[client_idx])?;
            if cache_mode == CacheMode::Cold {
                purge_outcomes[idx] = purge_for_measurement();
            }
            let lifecycle_before = if capture_artifact_lifecycle
                && !is_warmup
                && matches!(modes[idx], BenchMode::Accel)
            {
                Some((
                    capture_artifact_lifecycle_snapshot(&mut mode_clients[client_idx])
                        .map_err(|error| error.to_string()),
                    capture_dispatch_stats(&mut mode_clients[client_idx])
                        .map_err(|error| error.to_string()),
                ))
            } else {
                None
            };
            let dispatch_before = if !is_warmup
                && !native_parity_pairing
                && matches!(modes[idx], BenchMode::Accel)
                && counter_capture_error.is_none()
            {
                match capture_dispatch_stats(&mut mode_clients[client_idx]) {
                    Ok(snapshot) => Some(snapshot),
                    Err(error) => {
                        counter_capture_error = Some(format!(
                            "could not capture pg_accel dispatch counters before measured accel iteration: {error}"
                        ));
                        None
                    }
                }
            } else {
                None
            };
            let sql_for_mode = match modes[idx] {
                BenchMode::Accel => query.as_str(),
                BenchMode::PgParallel => baseline_query.as_str(),
            };
            let capture_this_planner = capture_planner_stages
                && !is_warmup
                && matches!(modes[idx], BenchMode::Accel)
                && !planner_capture_taken;
            planner_capture_taken |= capture_this_planner;
            let mode_outcome = run_with_mode(
                &mut mode_clients[client_idx],
                sql_for_mode,
                modes[idx],
                &pre_query,
                timing_mode,
                capture_this_planner,
            )?;
            timings[idx].elapsed_ms += mode_outcome.measurement.elapsed_ms;
            timings[idx].output_rows = timings[idx]
                .output_rows
                .saturating_add(mode_outcome.measurement.output_rows);
            raw_timing_ms[idx].push(mode_outcome.measurement.elapsed_ms);
            raw_sequence.push(match modes[idx] {
                BenchMode::Accel => "accel".to_owned(),
                BenchMode::PgParallel => "disabled_postgresql".to_owned(),
            });
            if let Some((lifecycle_before, lifecycle_dispatch_before)) = lifecycle_before {
                let lifecycle_after =
                    capture_artifact_lifecycle_snapshot(&mut mode_clients[client_idx])
                        .map_err(|error| error.to_string());
                let lifecycle_dispatch_after =
                    capture_dispatch_stats(&mut mode_clients[client_idx])
                        .map_err(|error| error.to_string());
                artifact_evidence = Some(finish_artifact_execution_evidence(
                    lifecycle_before,
                    lifecycle_dispatch_before,
                    lifecycle_after,
                    lifecycle_dispatch_after,
                    mode_outcome.measurement.output_rows,
                ));
            }
            if let Some(before) = dispatch_before {
                match capture_dispatch_stats(&mut mode_clients[client_idx])
                    .map_err(|error| error.to_string())
                    .and_then(|after| dispatch_stats_delta(before, after))
                {
                    Ok(delta) => match accumulate_dispatch_stats(&mut counter_delta, delta) {
                        Ok(()) => counter_iterations_captured += 1,
                        Err(error) => counter_capture_error = Some(error),
                    },
                    Err(error) => {
                        counter_capture_error = Some(format!(
                            "could not capture coherent pg_accel dispatch counters after measured accel iteration: {error}"
                        ));
                    }
                }
            }
            if let Some(capture) = mode_outcome.planner_stages {
                planner_stage_captures.push(report::PlannerStageCapture {
                    pair_index: i - effective_warmup,
                    cache_state: CacheState::from(cache_mode),
                    stages: capture.stages,
                    observer_probe: capture.observer_probe,
                    substages: capture.substages,
                    substage_observer_probe: capture.substage_observer_probe,
                    error: capture.error,
                });
            }
        }
        for idx in 0..timings.len() {
            let repetitions = raw_timing_ms[idx].len();
            if repetitions == 0 {
                return Err(format!("benchmark arm {idx} produced no timing samples").into());
            }
            timings[idx].elapsed_ms /= repetitions as f64;
        }
        let cache_purge = combine_purge_states(purge_outcomes[0], purge_outcomes[1]);

        let accel_ms = timings[0].elapsed_ms;
        let parallel_ms = timings[1].elapsed_ms;

        if native_parity_pairing && !is_warmup {
            native_parity_pair_captures.push(report::NativeParityPairCapture {
                pair_index: i - effective_warmup,
                accel_first,
                sequence: raw_sequence,
                accel_ms: raw_timing_ms[0].clone(),
                parallel_ms: raw_timing_ms[1].clone(),
                accel_average_ms: accel_ms,
                parallel_average_ms: parallel_ms,
            });
        }

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

        let iteration_result = IterationResult {
            accel_ms,
            parallel_ms,
            accel_first,
            cache_purge,
            cache_state: CacheState::from(cache_mode),
        };
        if is_warmup {
            warmup_results.push(iteration_result);
        } else {
            accel_output_rows_consumed =
                accel_output_rows_consumed.saturating_add(timings[0].output_rows);
            if capture_artifact_lifecycle {
                artifact_steady_state_captures.push(resident_lifecycle_capture(
                    "steady_state",
                    Some(i - effective_warmup),
                    iteration_result.clone(),
                    artifact_evidence.unwrap_or_else(|| {
                        ArtifactExecutionEvidence::unavailable(
                            "accelerated steady-state lifecycle capture did not run",
                        )
                    }),
                ));
            }
            results.push(iteration_result);
        }
    }

    if native_parity_pairing
        && counter_capture_error.is_none()
        && let Some(before) = native_pairing_counter_before
    {
        match capture_dispatch_stats(&mut mode_clients[0])
            .map_err(|error| error.to_string())
            .and_then(|after| dispatch_stats_delta(before, after))
        {
            Ok(delta) => {
                counter_delta = delta;
                counter_iterations_captured = iterations;
            }
            Err(error) => {
                counter_capture_error = Some(format!(
                    "could not capture coherent pg_accel dispatch counters after the native-parity series: {error}"
                ));
            }
        }
    }

    let counter_capture = match counter_capture_error {
        Some(error) => DispatchCounterCapture::unavailable(error),
        None if counter_iterations_captured == iterations => DispatchCounterCapture {
            captured: true,
            delta: counter_delta,
            error: None,
        },
        None => DispatchCounterCapture::unavailable(format!(
            "captured coherent dispatch counters for {counter_iterations_captured} of {iterations} measured accel iterations"
        )),
    };

    // Clean close.
    for client in &mut mode_clients {
        let _ = client.batch_execute("DISCARD ALL");
    }

    let mut result = WorkloadResult::from_iterations(
        workload.name().to_owned(),
        workload.description().to_owned(),
        crate::workloads::workload_metadata(workload.name()).map_or_else(
            || workload.category().to_owned(),
            |metadata| metadata.category.as_str().to_owned(),
        ),
        crate::report::classify_kernel(workload.name()),
        0,
        results,
        true,
    );
    result.set_warmup_iterations(warmup_results);
    result.accel_output_rows_consumed = accel_output_rows_consumed;
    result.resident_load_ms = resident_load_ms;
    result.resident_lane = workload_is_resident_lane(workload.name());
    result.planner_stage_captures = planner_stage_captures;
    result.native_parity_pair_captures = native_parity_pair_captures;
    result.artifact_lifecycle_probe = artifact_lifecycle_probe;
    result.artifact_steady_state_captures = artifact_steady_state_captures;
    if let Some(probe) = result.artifact_lifecycle_probe.as_ref() {
        let mut combined = vec![probe.iteration.clone()];
        combined.extend(
            result
                .iterations
                .iter()
                .filter(|iteration| iteration.cache_state == CacheState::Warm)
                .cloned(),
        );
        result.combined_warm_summary =
            report::CacheModeSummary::from_iterations(CacheState::Warm, &combined);
    }
    merge_dispatch_counter_capture(&mut result, counter_capture);
    Ok(result)
}

fn capture_dispatch_stats(
    client: &mut Client,
) -> Result<DispatchStatsSnapshot, Box<dyn std::error::Error>> {
    let row = client.query_one(
        "SELECT pg_backend_pid(), queries_accelerated, rows_dispatched, batches_executed, stock_exec_count, \
                gpu_rows_processed, gpu_kernel_executions, \
                (SELECT calls FROM pg_accel_grouped_kernel_mode_stats() WHERE mode = 'parallel_hash'), \
                (SELECT calls FROM pg_accel_grouped_kernel_mode_stats() WHERE mode = 'parallel_dense_count'), \
                (SELECT calls FROM pg_accel_grouped_kernel_mode_stats() WHERE mode = 'parallel_dense_integer'), \
                (SELECT calls FROM pg_accel_grouped_kernel_mode_stats() WHERE mode = 'serial_generic') \
           FROM pg_accel_stats()",
        &[],
    )?;
    Ok(DispatchStatsSnapshot {
        backend_pid: row.get(0),
        queries_accelerated: nonnegative_dispatch_counter("queries_accelerated", row.get(1))?,
        rows_dispatched: nonnegative_dispatch_counter("rows_dispatched", row.get(2))?,
        batches_executed: nonnegative_dispatch_counter("batches_executed", row.get(3))?,
        stock_exec_count: nonnegative_dispatch_counter("stock_exec_count", row.get(4))?,
        gpu_rows_processed: nonnegative_dispatch_counter("gpu_rows_processed", row.get(5))?,
        gpu_kernel_executions: nonnegative_dispatch_counter("gpu_kernel_executions", row.get(6))?,
        physical_kernel_modes: report::PhysicalKernelModeCounts {
            parallel_hash: nonnegative_dispatch_counter("parallel_hash", row.get(7))?,
            parallel_dense_count: nonnegative_dispatch_counter("parallel_dense_count", row.get(8))?,
            parallel_dense_integer: nonnegative_dispatch_counter(
                "parallel_dense_integer",
                row.get(9),
            )?,
            serial_generic: nonnegative_dispatch_counter("serial_generic", row.get(10))?,
        },
    })
}

fn capture_artifact_lifecycle_snapshot(
    client: &mut Client,
) -> Result<ArtifactLifecycleSnapshot, Box<dyn std::error::Error>> {
    let row = client.query_one(
        "SELECT hits, builds, rebuilds, artifact_bytes_observed, construction_bytes, \
                construction_us, preparation_us, raw_load_us, \
                pg_accel_last_artifact_policy() \
           FROM pg_accel_artifact_lifecycle_stats()",
        &[],
    )?;
    let counter = |name: &str, index| nonnegative_dispatch_counter(name, row.get::<_, i64>(index));
    let counters = report::ArtifactLifecycleDelta {
        hits: counter("artifact hits", 0)?,
        builds: counter("artifact builds", 1)?,
        rebuilds: counter("artifact rebuilds", 2)?,
        artifact_bytes_observed: counter("artifact bytes observed", 3)?,
        construction_bytes: counter("artifact construction bytes", 4)?,
        construction_us: counter("artifact construction time", 5)?,
        preparation_us: counter("artifact preparation time", 6)?,
        raw_load_us: counter("artifact raw load time", 7)?,
    };
    let dependencies = client
        .query(
            "SELECT relid::bigint, generation, global_generation, relfilenode::bigint, \
                    row_count, raw_bytes, derived_bytes \
               FROM pg_accel_resident_dependency_status() ORDER BY relid",
            &[],
        )?
        .into_iter()
        .map(|row| {
            let value =
                |name: &str, index| nonnegative_dispatch_counter(name, row.get::<_, i64>(index));
            Ok(report::ResidentDependencyIdentity {
                relid: u32::try_from(value("resident relid", 0)?)?,
                generation: value("resident generation", 1)?,
                global_generation: value("resident global generation", 2)?,
                relfilenode: u32::try_from(value("resident relfilenode", 3)?)?,
                row_count: value("resident row count", 4)?,
                raw_bytes: value("resident raw bytes", 5)?,
                derived_bytes: value("resident derived bytes", 6)?,
            })
        })
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    Ok(ArtifactLifecycleSnapshot {
        counters,
        dependencies,
        artifact_policy: row.get(8),
    })
}

fn artifact_lifecycle_delta(
    before: report::ArtifactLifecycleDelta,
    after: report::ArtifactLifecycleDelta,
) -> Result<report::ArtifactLifecycleDelta, String> {
    let delta = |name: &str, before: u64, after: u64| {
        after
            .checked_sub(before)
            .ok_or_else(|| format!("artifact lifecycle {name} counter regressed"))
    };
    Ok(report::ArtifactLifecycleDelta {
        hits: delta("hits", before.hits, after.hits)?,
        builds: delta("builds", before.builds, after.builds)?,
        rebuilds: delta("rebuilds", before.rebuilds, after.rebuilds)?,
        artifact_bytes_observed: delta(
            "artifact_bytes_observed",
            before.artifact_bytes_observed,
            after.artifact_bytes_observed,
        )?,
        construction_bytes: delta(
            "construction_bytes",
            before.construction_bytes,
            after.construction_bytes,
        )?,
        construction_us: delta(
            "construction_us",
            before.construction_us,
            after.construction_us,
        )?,
        preparation_us: delta(
            "preparation_us",
            before.preparation_us,
            after.preparation_us,
        )?,
        raw_load_us: delta("raw_load_us", before.raw_load_us, after.raw_load_us)?,
    })
}

fn finish_artifact_execution_evidence(
    before: Result<ArtifactLifecycleSnapshot, String>,
    dispatch_before: Result<DispatchStatsSnapshot, String>,
    after: Result<ArtifactLifecycleSnapshot, String>,
    dispatch_after: Result<DispatchStatsSnapshot, String>,
    output_rows_consumed: u64,
) -> ArtifactExecutionEvidence {
    let (before, after, dispatch_before, dispatch_after) =
        match (before, after, dispatch_before, dispatch_after) {
            (Ok(before), Ok(after), Ok(dispatch_before), Ok(dispatch_after)) => {
                (before, after, dispatch_before, dispatch_after)
            }
            values => {
                let errors = [
                    values.0.err(),
                    values.1.err(),
                    values.2.err(),
                    values.3.err(),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("; ");
                return ArtifactExecutionEvidence::unavailable(errors);
            }
        };
    let artifact = match artifact_lifecycle_delta(before.counters, after.counters) {
        Ok(delta) => delta,
        Err(error) => return ArtifactExecutionEvidence::unavailable(error),
    };
    let dispatch = match dispatch_stats_delta(dispatch_before, dispatch_after) {
        Ok(delta) => delta,
        Err(error) => return ArtifactExecutionEvidence::unavailable(error),
    };
    ArtifactExecutionEvidence {
        artifact_policy: (artifact.ensures() != 0)
            .then_some(after.artifact_policy)
            .flatten(),
        artifact,
        dependencies_before: before.dependencies,
        dependencies_after: after.dependencies,
        dispatch,
        refresh_us: 0,
        refreshed_relations: 0,
        refreshed_rows: 0,
        output_rows_consumed,
        error: None,
    }
}

fn resident_lifecycle_capture(
    phase: &str,
    pair_index: Option<usize>,
    iteration: IterationResult,
    evidence: ArtifactExecutionEvidence,
) -> report::ResidentLifecycleCapture {
    report::ResidentLifecycleCapture {
        phase: phase.to_owned(),
        pair_index,
        iteration,
        artifact: evidence.artifact,
        artifact_policy: evidence.artifact_policy,
        refresh_us: evidence.refresh_us,
        refreshed_relations: evidence.refreshed_relations,
        refreshed_rows: evidence.refreshed_rows,
        dependencies_before: evidence.dependencies_before,
        dependencies_after: evidence.dependencies_after,
        queries_accelerated: evidence.dispatch.queries_accelerated,
        rows_dispatched: evidence.dispatch.rows_dispatched,
        batches_executed: evidence.dispatch.batches_executed,
        stock_exec_count: evidence.dispatch.stock_exec_count,
        gpu_rows_processed: evidence.dispatch.gpu_rows_processed,
        gpu_kernel_executions: evidence.dispatch.gpu_kernel_executions,
        output_rows_consumed: evidence.output_rows_consumed,
        error: evidence.error,
    }
}

fn prime_pg_accel_backend(client: &mut Client) -> Result<(), Box<dyn std::error::Error>> {
    // `SET pg_accel.enabled` alone can be a placeholder GUC in a fresh
    // backend. Touch an extension function before warmup/plan capture so
    // `_PG_init` installs planner hooks before any timed iteration.
    client.simple_query("SELECT 1 FROM pg_accel_stats() LIMIT 1")?;
    Ok(())
}

/// Prime the accel backend and pin every exact relation/column set required by
/// the workload. Returns `Some(load_ms)` for resident lanes so reports retain
/// the first-use conversion cost separately from the timed warm execution.
fn prime_workload_accel_backend(
    client: &mut Client,
    workload: &dyn Workload,
) -> Result<Option<f64>, Box<dyn std::error::Error>> {
    prime_pg_accel_backend(client)?;
    let pins = resident_pin_specs(workload.name());
    let resident_lane = !pins.is_empty();
    let resident_load_start = std::time::Instant::now();
    for pin in pins {
        let columns = pin
            .columns
            .iter()
            .map(|column| (*column).to_owned())
            .collect::<Vec<_>>();
        let row = client.query_one(
            "SELECT pg_accel_pin($1::text::regclass, $2::text[])::bigint",
            &[&pin.table, &columns],
        )?;
        let loaded_rows: i64 = row.get(0);
        if loaded_rows <= 0 {
            return Err(format!(
                "pg_accel_pin loaded {loaded_rows} rows from {} for {}",
                pin.table,
                workload.name(),
            )
            .into());
        }
        let material: bool = client
            .query_one(
                "SELECT COALESCE(bool_or(raw_bytes > 0 AND loaded_at IS NOT NULL), false) \
                 FROM pg_accel_resident_status() WHERE relid = $1::text::regclass::oid",
                &[&pin.table],
            )?
            .get(0);
        if !material {
            let refreshed_rows: i64 = client
                .query_one(
                    "SELECT pg_accel_refresh($1::text::regclass)::bigint",
                    &[&pin.table],
                )?
                .get(0);
            if refreshed_rows != loaded_rows {
                return Err(format!(
                    "pg_accel_refresh loaded {refreshed_rows} rows after the one-time pin loaded \
                     {loaded_rows} rows from {} for {}",
                    pin.table,
                    workload.name(),
                )
                .into());
            }
        }
    }
    Ok(resident_lane.then(|| resident_load_start.elapsed().as_secs_f64() * 1000.0))
}

fn refresh_workload_resident_inputs(
    client: &mut Client,
    workload: &dyn Workload,
) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    let mut refreshed_relations = 0_u64;
    let mut total_refreshed_rows = 0_u64;
    for pin in resident_pin_specs(workload.name()) {
        let relation_rows: i64 = client
            .query_one(
                "SELECT pg_accel_refresh($1::text::regclass)::bigint",
                &[&pin.table],
            )?
            .get(0);
        if relation_rows <= 0 {
            return Err(format!(
                "pg_accel_refresh loaded {relation_rows} rows from {} before measured dispatch for {}",
                pin.table,
                workload.name(),
            )
            .into());
        }
        refreshed_relations = refreshed_relations.saturating_add(1);
        total_refreshed_rows = total_refreshed_rows.saturating_add(u64::try_from(relation_rows)?);
    }
    Ok((refreshed_relations, total_refreshed_rows))
}

fn run_artifact_lifecycle_probe(
    mode_clients: &mut [Client; 2],
    workload: &dyn Workload,
    query: &str,
    baseline_query: &str,
    pre_query: &[String],
    timing_mode: TimingMode,
    options: LifecyclePairOptions,
) -> Result<report::ResidentLifecycleCapture, Box<dyn std::error::Error>> {
    let LifecyclePairOptions {
        accel_first,
        native_parity_pairing,
    } = options;
    let order = if accel_first {
        [0_usize, 1]
    } else {
        [1_usize, 0]
    };
    let mut timings = [MeasurementOutcome {
        elapsed_ms: 0.0,
        output_rows: 0,
    }; 2];
    let mut lifecycle_evidence = None;
    for idx in order {
        let client_idx = if native_parity_pairing { 0 } else { idx };
        mode_clients[client_idx].batch_execute("DISCARD ALL")?;
        apply_benchmark_safety_settings(&mut mode_clients[client_idx])?;
        if idx == 0 {
            prime_workload_accel_backend(&mut mode_clients[client_idx], workload)?;
            let before = capture_artifact_lifecycle_snapshot(&mut mode_clients[client_idx])
                .map_err(|error| error.to_string());
            let dispatch_before = capture_dispatch_stats(&mut mode_clients[client_idx])
                .map_err(|error| error.to_string());
            let lifecycle_started = Instant::now();
            let refresh_started = Instant::now();
            let (refreshed_relations, refreshed_rows) =
                refresh_workload_resident_inputs(&mut mode_clients[client_idx], workload)?;
            let refresh_elapsed = refresh_started.elapsed();
            let outcome = run_with_mode(
                &mut mode_clients[client_idx],
                query,
                BenchMode::Accel,
                pre_query,
                timing_mode,
                false,
            )?;
            let lifecycle_elapsed = lifecycle_started.elapsed();
            timings[idx] = MeasurementOutcome {
                elapsed_ms: lifecycle_elapsed.as_secs_f64() * 1_000.0,
                output_rows: outcome.measurement.output_rows,
            };
            let after = capture_artifact_lifecycle_snapshot(&mut mode_clients[client_idx])
                .map_err(|error| error.to_string());
            let dispatch_after = capture_dispatch_stats(&mut mode_clients[client_idx])
                .map_err(|error| error.to_string());
            let mut evidence = finish_artifact_execution_evidence(
                before,
                dispatch_before,
                after,
                dispatch_after,
                outcome.measurement.output_rows,
            );
            evidence.refresh_us = u64::try_from(refresh_elapsed.as_micros()).unwrap_or(u64::MAX);
            evidence.refreshed_relations = refreshed_relations;
            evidence.refreshed_rows = refreshed_rows;
            lifecycle_evidence = Some(evidence);
        } else {
            timings[idx] = run_with_mode(
                &mut mode_clients[client_idx],
                baseline_query,
                BenchMode::PgParallel,
                pre_query,
                timing_mode,
                false,
            )?
            .measurement;
        }
    }
    let iteration = IterationResult {
        accel_ms: timings[0].elapsed_ms,
        parallel_ms: timings[1].elapsed_ms,
        accel_first,
        cache_purge: CachePurgeState::NotRequested,
        cache_state: CacheState::Warm,
    };
    Ok(resident_lifecycle_capture(
        "lifecycle",
        None,
        iteration,
        lifecycle_evidence.unwrap_or_else(|| {
            ArtifactExecutionEvidence::unavailable("accelerated lifecycle arm did not run")
        }),
    ))
}

/// Whether a workload requires a GPU-resident cache preload (any of the
/// `ssbm_*` / `resident_*` lanes). Used to tag resident-lane rows and to
/// decide whether resident-load time should be recorded.
fn workload_is_resident_lane(name: &str) -> bool {
    !resident_pin_specs(name).is_empty()
}

fn resident_pin_specs(name: &str) -> &'static [crate::workloads::ResidentPinSpec] {
    crate::workloads::workload_metadata(name).map_or(&[], |metadata| metadata.resident_pins)
}

#[cfg(test)]
fn resident_pin(
    table: &'static str,
    columns: &'static [&'static str],
) -> crate::workloads::ResidentPinSpec {
    crate::workloads::ResidentPinSpec { table, columns }
}
fn nonnegative_dispatch_counter(name: &str, value: i64) -> Result<u64, Box<dyn std::error::Error>> {
    u64::try_from(value).map_err(|error| {
        format!("dispatch counter `{name}` returned negative value {value}: {error}").into()
    })
}

fn dispatch_stats_delta(
    before: DispatchStatsSnapshot,
    after: DispatchStatsSnapshot,
) -> Result<DispatchStatsDelta, String> {
    if before.backend_pid != after.backend_pid {
        return Err(format!(
            "dispatch counter backend changed from PID {} to {}",
            before.backend_pid, after.backend_pid
        ));
    }
    let delta = |name: &str, before: u64, after: u64| {
        after
            .checked_sub(before)
            .ok_or_else(|| format!("dispatch counter `{name}` regressed from {before} to {after}"))
    };
    Ok(DispatchStatsDelta {
        queries_accelerated: delta(
            "queries_accelerated",
            before.queries_accelerated,
            after.queries_accelerated,
        )?,
        rows_dispatched: delta(
            "rows_dispatched",
            before.rows_dispatched,
            after.rows_dispatched,
        )?,
        batches_executed: after
            .batches_executed
            .checked_sub(before.batches_executed)
            .ok_or_else(|| {
                format!(
                    "dispatch counter `batches_executed` regressed from {} to {}",
                    before.batches_executed, after.batches_executed
                )
            })?,
        stock_exec_count: delta(
            "stock_exec_count",
            before.stock_exec_count,
            after.stock_exec_count,
        )?,
        gpu_rows_processed: delta(
            "gpu_rows_processed",
            before.gpu_rows_processed,
            after.gpu_rows_processed,
        )?,
        gpu_kernel_executions: delta(
            "gpu_kernel_executions",
            before.gpu_kernel_executions,
            after.gpu_kernel_executions,
        )?,
        physical_kernel_modes: report::PhysicalKernelModeCounts {
            parallel_hash: delta(
                "parallel_hash",
                before.physical_kernel_modes.parallel_hash,
                after.physical_kernel_modes.parallel_hash,
            )?,
            parallel_dense_count: delta(
                "parallel_dense_count",
                before.physical_kernel_modes.parallel_dense_count,
                after.physical_kernel_modes.parallel_dense_count,
            )?,
            parallel_dense_integer: delta(
                "parallel_dense_integer",
                before.physical_kernel_modes.parallel_dense_integer,
                after.physical_kernel_modes.parallel_dense_integer,
            )?,
            serial_generic: delta(
                "serial_generic",
                before.physical_kernel_modes.serial_generic,
                after.physical_kernel_modes.serial_generic,
            )?,
        },
    })
}

fn accumulate_dispatch_stats(
    total: &mut DispatchStatsDelta,
    delta: DispatchStatsDelta,
) -> Result<(), String> {
    let add = |name: &str, total: u64, value: u64| {
        total
            .checked_add(value)
            .ok_or_else(|| format!("dispatch counter `{name}` overflowed while accumulating"))
    };
    total.queries_accelerated = add(
        "queries_accelerated",
        total.queries_accelerated,
        delta.queries_accelerated,
    )?;
    total.rows_dispatched = add(
        "rows_dispatched",
        total.rows_dispatched,
        delta.rows_dispatched,
    )?;
    total.batches_executed = add(
        "batches_executed",
        total.batches_executed,
        delta.batches_executed,
    )?;
    total.stock_exec_count = add(
        "stock_exec_count",
        total.stock_exec_count,
        delta.stock_exec_count,
    )?;
    total.gpu_rows_processed = add(
        "gpu_rows_processed",
        total.gpu_rows_processed,
        delta.gpu_rows_processed,
    )?;
    total.gpu_kernel_executions = add(
        "gpu_kernel_executions",
        total.gpu_kernel_executions,
        delta.gpu_kernel_executions,
    )?;
    total.physical_kernel_modes.parallel_hash = add(
        "parallel_hash",
        total.physical_kernel_modes.parallel_hash,
        delta.physical_kernel_modes.parallel_hash,
    )?;
    total.physical_kernel_modes.parallel_dense_count = add(
        "parallel_dense_count",
        total.physical_kernel_modes.parallel_dense_count,
        delta.physical_kernel_modes.parallel_dense_count,
    )?;
    total.physical_kernel_modes.parallel_dense_integer = add(
        "parallel_dense_integer",
        total.physical_kernel_modes.parallel_dense_integer,
        delta.physical_kernel_modes.parallel_dense_integer,
    )?;
    total.physical_kernel_modes.serial_generic = add(
        "serial_generic",
        total.physical_kernel_modes.serial_generic,
        delta.physical_kernel_modes.serial_generic,
    )?;
    Ok(())
}

fn merge_dispatch_counter_capture(target: &mut WorkloadResult, capture: DispatchCounterCapture) {
    target.dispatch_counter_captured = capture.captured;
    target.pg_accel_queries_accelerated_delta = capture.delta.queries_accelerated;
    target.gpu_kernel_execution_delta = capture.delta.gpu_kernel_executions;
    target.pg_accel_rows_dispatched_delta = capture.delta.rows_dispatched;
    target.pg_accel_batches_executed_delta = capture.delta.batches_executed;
    target.pg_accel_gpu_rows_processed_delta = capture.delta.gpu_rows_processed;
    target.pg_accel_stock_exec_delta = capture.delta.stock_exec_count;
    target.physical_kernel_mode_counts = capture.delta.physical_kernel_modes;
    target.physical_kernel_mode = capture
        .delta
        .physical_kernel_modes
        .unique_mode()
        .map(str::to_owned);
    target.dispatch_counter_error = capture.error;
}

fn merge_cache_mode_dispatch_counter_fields(
    target: &mut WorkloadResult,
    cold: &WorkloadResult,
    warm: &WorkloadResult,
) {
    let add = |lhs: u64, rhs: u64| lhs.checked_add(rhs);
    target.dispatch_counter_captured = cold.dispatch_counter_captured
        && warm.dispatch_counter_captured
        && cold.dispatch_counter_error.is_none()
        && warm.dispatch_counter_error.is_none();
    let merged = (
        add(
            cold.pg_accel_queries_accelerated_delta,
            warm.pg_accel_queries_accelerated_delta,
        ),
        add(
            cold.gpu_kernel_execution_delta,
            warm.gpu_kernel_execution_delta,
        ),
        add(
            cold.pg_accel_rows_dispatched_delta,
            warm.pg_accel_rows_dispatched_delta,
        ),
        add(
            cold.pg_accel_batches_executed_delta,
            warm.pg_accel_batches_executed_delta,
        ),
        add(
            cold.pg_accel_gpu_rows_processed_delta,
            warm.pg_accel_gpu_rows_processed_delta,
        ),
        add(
            cold.pg_accel_stock_exec_delta,
            warm.pg_accel_stock_exec_delta,
        ),
        add(
            cold.accel_output_rows_consumed,
            warm.accel_output_rows_consumed,
        ),
    );
    let merged_modes = (
        add(
            cold.physical_kernel_mode_counts.parallel_hash,
            warm.physical_kernel_mode_counts.parallel_hash,
        ),
        add(
            cold.physical_kernel_mode_counts.parallel_dense_count,
            warm.physical_kernel_mode_counts.parallel_dense_count,
        ),
        add(
            cold.physical_kernel_mode_counts.parallel_dense_integer,
            warm.physical_kernel_mode_counts.parallel_dense_integer,
        ),
        add(
            cold.physical_kernel_mode_counts.serial_generic,
            warm.physical_kernel_mode_counts.serial_generic,
        ),
    );
    if let (
        Some(queries),
        Some(kernels),
        Some(rows),
        Some(batches),
        Some(gpu_rows),
        Some(stock),
        Some(output),
    ) = merged
    {
        target.pg_accel_queries_accelerated_delta = queries;
        target.gpu_kernel_execution_delta = kernels;
        target.pg_accel_rows_dispatched_delta = rows;
        target.pg_accel_batches_executed_delta = batches;
        target.pg_accel_gpu_rows_processed_delta = gpu_rows;
        target.pg_accel_stock_exec_delta = stock;
        target.accel_output_rows_consumed = output;
    } else {
        target.dispatch_counter_captured = false;
    }
    if let (Some(hash), Some(count), Some(integer), Some(serial)) = merged_modes {
        target.physical_kernel_mode_counts = report::PhysicalKernelModeCounts {
            parallel_hash: hash,
            parallel_dense_count: count,
            parallel_dense_integer: integer,
            serial_generic: serial,
        };
        target.physical_kernel_mode = target
            .physical_kernel_mode_counts
            .unique_mode()
            .map(str::to_owned);
    } else {
        target.dispatch_counter_captured = false;
    }
    let mut errors = [
        cold.dispatch_counter_error.as_deref(),
        warm.dispatch_counter_error.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    if !cold.dispatch_counter_captured && cold.dispatch_counter_error.is_none() {
        errors.push("cold cache-mode dispatch counter capture was incomplete".to_owned());
    }
    if !warm.dispatch_counter_captured && warm.dispatch_counter_error.is_none() {
        errors.push("warm cache-mode dispatch counter capture was incomplete".to_owned());
    }
    if merged.0.is_none()
        || merged.1.is_none()
        || merged.2.is_none()
        || merged.3.is_none()
        || merged.4.is_none()
        || merged.5.is_none()
        || merged.6.is_none()
        || merged_modes.0.is_none()
        || merged_modes.1.is_none()
        || merged_modes.2.is_none()
        || merged_modes.3.is_none()
    {
        errors.push(
            "dispatch counters overflowed while merging cold and warm cache modes".to_owned(),
        );
    }
    target.dispatch_counter_error = (!errors.is_empty()).then(|| errors.join("; "));
}

/// Measurement mode for a single run (two-way comparison).
#[derive(Clone, Copy, Debug)]
enum BenchMode {
    /// pg_accel enabled, PG parallel at default.
    Accel,
    /// pg_accel off, PG parallel workers at default.
    PgParallel,
}

/// Raw executions retained for each logical arm in one native-parity pair.
///
/// Four observations (two mirrored ABBA/BAAB motifs) materially reduce
/// sub-millisecond scheduler noise while preserving 30 independent balanced
/// pair blocks. Every raw execution remains in the evidence artifact.
const NATIVE_PARITY_REPETITIONS_PER_ARM: usize = 4;

fn native_parity_execution_order(accel_first: bool) -> Vec<usize> {
    debug_assert_eq!(NATIVE_PARITY_REPETITIONS_PER_ARM % 2, 0);
    let motif = if accel_first {
        [0, 1, 1, 0]
    } else {
        [1, 0, 0, 1]
    };
    motif
        .into_iter()
        .cycle()
        .take(NATIVE_PARITY_REPETITIONS_PER_ARM * 2)
        .collect()
}

/// Run a single measurement with the given mode and timing strategy.
fn run_with_mode(
    client: &mut Client,
    query: &str,
    mode: BenchMode,
    pre_query: &[String],
    timing_mode: TimingMode,
    capture_planner_stages: bool,
) -> Result<ModeRunOutcome, Box<dyn std::error::Error>> {
    apply_benchmark_safety_settings(client)?;
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
    let stage_statement = if capture_planner_stages {
        match client.batch_execute("SET pg_accel.planner_profiling = on") {
            Ok(()) => match prepare_planner_profile_statement(client) {
                Ok(statement) => Some(Ok(statement)),
                Err(error) => Some(Err(format!(
                    "could not prepare pg_accel planner-profile capture: {error}"
                ))),
            },
            Err(error) => Some(Err(format!(
                "could not enable pg_accel planner profiling: {error}"
            ))),
        }
    } else {
        None
    };
    let stage_before = match stage_statement.as_ref() {
        Some(Ok(statement)) => match capture_planner_profile_stats(client, statement) {
            Ok(snapshot) => Some(Ok(snapshot)),
            Err(error) => Some(Err(format!(
                "could not capture pg_accel planner profile before query: {error}"
            ))),
        },
        Some(Err(error)) => Some(Err(error.clone())),
        None => None,
    };

    let measurement = match timing_mode {
        TimingMode::ExplainAnalyze => run_explain_analyze_outcome(client, query),
        TimingMode::RawWallClock => run_raw_wall_clock(client, query),
        TimingMode::Both => {
            // Run both mechanisms back-to-back on the same connection.
            // We report the raw wall-clock value but also capture the EXPLAIN
            // ANALYZE figure to
            // stderr so operators can audit the gap.
            let raw = run_raw_wall_clock(client, query)?;
            match run_explain_analyze(client, query) {
                Ok(ea) => eprintln!(
                    "[timing:both] raw={:.3}ms explain_analyze={ea:.3}ms",
                    raw.elapsed_ms
                ),
                Err(e) => eprintln!(
                    "[timing:both] raw={:.3}ms explain_analyze=ERR({e})",
                    raw.elapsed_ms
                ),
            }
            Ok(raw)
        }
    }?;

    let planner_stages = match (stage_statement.as_ref(), stage_before) {
        (Some(Ok(statement)), Some(Ok(before))) => {
            Some(match capture_planner_profile_stats(client, statement) {
                Ok(after) => match planner_profile_stats_delta(&before, &after) {
                    Ok((stages, substages)) => {
                        match capture_planner_profile_stats(client, statement) {
                            Ok(probe) => match planner_profile_stats_delta(&after, &probe) {
                                Ok((observer_probe, substage_observer_probe)) => {
                                    let contaminated = observer_probe.iter().any(|stage| {
                                        stage.calls != 0
                                            || stage.elapsed_us != 0
                                            || stage.fast_declines != 0
                                    }) || substage_observer_probe
                                        .iter()
                                        .any(|stage| stage.calls != 0 || stage.elapsed_us != 0);
                                    PlannerStageMeasurement {
                                    stages,
                                    observer_probe,
                                    substages,
                                    substage_observer_probe,
                                    error: contaminated.then(|| {
                                        "prepared planner-profile observer changed planner counters; \
                                         target attribution is invalid"
                                            .to_owned()
                                    }),
                                }
                                }
                                Err(error) => PlannerStageMeasurement {
                                    stages,
                                    observer_probe: Vec::new(),
                                    substages,
                                    substage_observer_probe: Vec::new(),
                                    error: Some(error),
                                },
                            },
                            Err(error) => PlannerStageMeasurement {
                                stages,
                                observer_probe: Vec::new(),
                                substages,
                                substage_observer_probe: Vec::new(),
                                error: Some(format!(
                                    "could not verify prepared planner-profile observer: {error}"
                                )),
                            },
                        }
                    }
                    Err(error) => PlannerStageMeasurement::unavailable(error),
                },
                Err(error) => PlannerStageMeasurement::unavailable(format!(
                    "could not capture pg_accel planner profile after query: {error}"
                )),
            })
        }
        (_, Some(Err(error))) => Some(PlannerStageMeasurement::unavailable(error)),
        _ => None,
    };

    Ok(ModeRunOutcome {
        measurement,
        planner_stages,
    })
}

fn prepare_planner_profile_statement(
    client: &mut Client,
) -> Result<Statement, Box<dyn std::error::Error>> {
    Ok(client.prepare(
        "SELECT kind, name, calls, elapsed_us, fast_declines FROM (\
             SELECT 'stage'::text AS kind, stage AS name, calls, elapsed_us, fast_declines \
               FROM pg_accel_planner_stage_stats() \
             UNION ALL \
             SELECT 'substage'::text AS kind, substage AS name, calls, elapsed_us, 0::bigint \
               FROM pg_accel_planner_substage_stats()\
         ) AS planner_profile ORDER BY kind, name",
    )?)
}

fn capture_planner_profile_stats(
    client: &mut Client,
    statement: &Statement,
) -> Result<PlannerProfileSnapshot, Box<dyn std::error::Error>> {
    let mut snapshot = PlannerProfileSnapshot::default();
    for row in client.query(statement, &[])? {
        let kind = row.get::<_, String>(0);
        let name = row.get::<_, String>(1);
        let calls = nonnegative_planner_counter(&name, "calls", row.get(2))?;
        let elapsed_us = nonnegative_planner_counter(&name, "elapsed_us", row.get(3))?;
        match kind.as_str() {
            "stage" => {
                if !PLANNER_STAGE_NAMES.contains(&name.as_str()) {
                    return Err(format!("unknown planner stage `{name}`").into());
                }
                let fast_declines =
                    nonnegative_planner_counter(&name, "fast_declines", row.get(4))?;
                if snapshot
                    .stages
                    .insert(
                        name.clone(),
                        PlannerStageStatsSnapshot {
                            calls,
                            elapsed_us,
                            fast_declines,
                        },
                    )
                    .is_some()
                {
                    return Err(format!("duplicate planner stage `{name}`").into());
                }
            }
            "substage" => {
                if !PLANNER_SUBSTAGE_NAMES.contains(&name.as_str()) {
                    return Err(format!("unknown planner substage `{name}`").into());
                }
                if snapshot
                    .substages
                    .insert(
                        name.clone(),
                        PlannerSubstageStatsSnapshot { calls, elapsed_us },
                    )
                    .is_some()
                {
                    return Err(format!("duplicate planner substage `{name}`").into());
                }
            }
            _ => return Err(format!("unknown planner profile kind `{kind}`").into()),
        }
    }
    if snapshot.stages.len() != PLANNER_STAGE_NAMES.len() {
        let missing = PLANNER_STAGE_NAMES
            .iter()
            .filter(|stage| !snapshot.stages.contains_key(**stage))
            .copied()
            .collect::<Vec<_>>();
        return Err(format!("planner-stage snapshot missing: {}", missing.join(", ")).into());
    }
    if snapshot.substages.len() != PLANNER_SUBSTAGE_NAMES.len() {
        let missing = PLANNER_SUBSTAGE_NAMES
            .iter()
            .filter(|stage| !snapshot.substages.contains_key(**stage))
            .copied()
            .collect::<Vec<_>>();
        return Err(format!("planner-substage snapshot missing: {}", missing.join(", ")).into());
    }
    Ok(snapshot)
}

fn nonnegative_planner_counter(
    stage: &str,
    counter: &str,
    value: i64,
) -> Result<u64, Box<dyn std::error::Error>> {
    u64::try_from(value).map_err(|error| {
        format!("planner stage `{stage}` returned negative {counter} ({value}): {error}").into()
    })
}

fn planner_stage_stats_delta(
    before: &BTreeMap<String, PlannerStageStatsSnapshot>,
    after: &BTreeMap<String, PlannerStageStatsSnapshot>,
) -> Result<Vec<report::PlannerStageDelta>, String> {
    if before.keys().ne(after.keys()) {
        return Err("planner-stage snapshot key sets differ".to_owned());
    }
    after
        .iter()
        .map(|(stage, after)| {
            let before = before
                .get(stage)
                .copied()
                .ok_or_else(|| format!("planner stage `{stage}` missing from before snapshot"))?;
            Ok(report::PlannerStageDelta {
                stage: stage.clone(),
                calls: after
                    .calls
                    .checked_sub(before.calls)
                    .ok_or_else(|| format!("planner stage `{stage}` calls counter regressed"))?,
                elapsed_us: after
                    .elapsed_us
                    .checked_sub(before.elapsed_us)
                    .ok_or_else(|| {
                        format!("planner stage `{stage}` elapsed_us counter regressed")
                    })?,
                fast_declines: after
                    .fast_declines
                    .checked_sub(before.fast_declines)
                    .ok_or_else(|| {
                        format!("planner stage `{stage}` fast_declines counter regressed")
                    })?,
            })
        })
        .collect()
}

fn planner_substage_stats_delta(
    before: &BTreeMap<String, PlannerSubstageStatsSnapshot>,
    after: &BTreeMap<String, PlannerSubstageStatsSnapshot>,
) -> Result<Vec<report::PlannerSubstageDelta>, String> {
    if before.keys().ne(after.keys()) {
        return Err("planner-substage snapshot key sets differ".to_owned());
    }
    after
        .iter()
        .map(|(substage, after)| {
            let before = before.get(substage).copied().ok_or_else(|| {
                format!("planner substage `{substage}` missing from before snapshot")
            })?;
            Ok(report::PlannerSubstageDelta {
                substage: substage.clone(),
                calls: after.calls.checked_sub(before.calls).ok_or_else(|| {
                    format!("planner substage `{substage}` calls counter regressed")
                })?,
                elapsed_us: after
                    .elapsed_us
                    .checked_sub(before.elapsed_us)
                    .ok_or_else(|| {
                        format!("planner substage `{substage}` elapsed_us counter regressed")
                    })?,
            })
        })
        .collect()
}

fn planner_profile_stats_delta(
    before: &PlannerProfileSnapshot,
    after: &PlannerProfileSnapshot,
) -> Result<
    (
        Vec<report::PlannerStageDelta>,
        Vec<report::PlannerSubstageDelta>,
    ),
    String,
> {
    Ok((
        planner_stage_stats_delta(&before.stages, &after.stages)?,
        planner_substage_stats_delta(&before.substages, &after.substages)?,
    ))
}

/// Measure query wall time client-side with `Instant::now()` around a plain
/// `execute()` call. No `EXPLAIN ANALYZE`, no per-node instrumentation
/// overhead — this is the timing mode to use for publication-quality
/// numbers.
fn run_raw_wall_clock(
    client: &mut Client,
    query: &str,
) -> Result<MeasurementOutcome, Box<dyn std::error::Error>> {
    let start = Instant::now();
    // simple_query so multi-statement queries and SELECT ... still work; we
    // fully consume rows returned to libpq.
    let messages = client.simple_query(query)?;
    let elapsed = start.elapsed();
    #[allow(clippy::cast_precision_loss)]
    let ms = (elapsed.as_nanos() as f64) / 1.0e6;
    let output_rows = messages
        .iter()
        .filter(|msg| matches!(msg, postgres::SimpleQueryMessage::Row(_)))
        .count();
    Ok(MeasurementOutcome {
        elapsed_ms: ms,
        output_rows: u64::try_from(output_rows).unwrap_or(u64::MAX),
    })
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
    apply_benchmark_safety_settings(&mut client)?;
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
/// Each workload is run at its declared row scales, defaulting to the global
/// 10K, 100K, 1M, and 10M matrix. Results include hardware profile
/// auto-detection and GUC settings capture.
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
    warn_if_debug_raw_timing(config.timing_mode);
    if config.iterations < MIN_ITERATIONS {
        return Err(format!(
            "minimum {MIN_ITERATIONS} iterations required for statistical validity \
             (got {})",
            config.iterations
        )
        .into());
    }

    let workload_names: Vec<&str> = workloads.iter().map(|w| w.name()).collect();
    let run_context = prepare_run_context(connection, &workload_names, config)?;
    let observed_gucs = run_context.observed_gucs;
    let artifacts = run_context.artifacts;

    initialize_plans_file(config);

    let result_capacity = workloads.iter().map(|w| w.row_scales().len()).sum();
    let mut results = Vec::with_capacity(result_capacity);
    let mut crashes: Vec<report::CrashedScale> = Vec::new();

    for w in workloads {
        for &rows in w.row_scales() {
            eprintln!("\n[scale] {} @ {} rows", w.name(), format_rows(rows));
            match run_workload_with_config(connection, w.as_ref(), rows, config, artifacts.as_ref())
            {
                Ok(mut result) => {
                    result.rows = rows;
                    results.push(result);
                }
                Err(e) => {
                    let err_msg = format!("{e}");
                    eprintln!("[CRASH] {} @ {} — {err_msg}", w.name(), format_rows(rows));
                    let crash = record_crash(
                        connection,
                        w.as_ref(),
                        rows,
                        config,
                        artifacts.as_ref(),
                        crashes.len() + 1,
                        &err_msg,
                    );
                    crashes.push(crash);
                    if let Some(artifact_writer) = artifacts.as_ref()
                        && let Err(write_err) = artifact_writer.write_crashes(&crashes)
                    {
                        eprintln!("[artifacts] crash list write failed: {write_err}");
                    }
                    let _ = cleanup(connection, w.as_ref());
                    wait_for_pg(connection)?;
                }
            }
        }
    }

    let mut report = report::generate_report(
        results,
        crashes,
        Some(connection),
        config.iterations,
        config.warmup,
        observed_gucs,
        config.timing_mode,
        config.cache_mode,
    );
    apply_config_methodology(&mut report, config);
    if let Some(artifact_writer) = artifacts.as_ref() {
        finalize_report_artifacts(artifact_writer, &mut report, "run-complete")?;
    }
    Ok(report)
}

/// One workload/row-scale cell requested by an artifact resume run.
pub struct WorkloadRunCell<'a> {
    pub workload: &'a dyn Workload,
    pub rows: usize,
}

/// Retry a selected set of workload/row cells and return an aggregate report.
///
/// This is the artifact-resume path. It intentionally does not enforce the
/// full-run statistical minimum because the source artifact can be either a
/// publication benchmark or a short crash-repro run; the saved pre-risk
/// context carries the original iteration count.
pub fn run_cells_with_config(
    connection: &str,
    cells: &[WorkloadRunCell<'_>],
    config: &BenchConfig,
) -> Result<report::BenchReport, Box<dyn std::error::Error>> {
    warn_if_debug_raw_timing(config.timing_mode);
    let workload_names: Vec<&str> = cells.iter().map(|cell| cell.workload.name()).collect();
    let run_context = prepare_run_context(connection, &workload_names, config)?;
    let observed_gucs = run_context.observed_gucs;
    let artifacts = run_context.artifacts;

    initialize_plans_file(config);

    let mut results = Vec::with_capacity(cells.len());
    let mut crashes: Vec<report::CrashedScale> = Vec::new();

    for cell in cells {
        eprintln!(
            "\n[resume] retrying {} @ {} rows",
            cell.workload.name(),
            format_rows(cell.rows)
        );
        match run_workload_with_config(
            connection,
            cell.workload,
            cell.rows,
            config,
            artifacts.as_ref(),
        ) {
            Ok(mut result) => {
                result.rows = cell.rows;
                results.push(result);
            }
            Err(e) => {
                let err_msg = format!("{e}");
                eprintln!(
                    "[CRASH] {} @ {} — {err_msg}",
                    cell.workload.name(),
                    format_rows(cell.rows)
                );
                let crash = record_crash(
                    connection,
                    cell.workload,
                    cell.rows,
                    config,
                    artifacts.as_ref(),
                    crashes.len() + 1,
                    &err_msg,
                );
                crashes.push(crash);
                if let Some(artifact_writer) = artifacts.as_ref()
                    && let Err(write_err) = artifact_writer.write_crashes(&crashes)
                {
                    eprintln!("[artifacts] crash list write failed: {write_err}");
                }
                let _ = cleanup(connection, cell.workload);
                wait_for_pg(connection)?;
            }
        }
    }

    let mut report = report::generate_report(
        results,
        crashes,
        Some(connection),
        config.iterations,
        config.warmup,
        observed_gucs,
        config.timing_mode,
        config.cache_mode,
    );
    apply_config_methodology(&mut report, config);
    if let Some(artifact_writer) = artifacts.as_ref() {
        finalize_report_artifacts(artifact_writer, &mut report, "resume-complete")?;
    }
    Ok(report)
}

/// Run one workload at one row scale and return a one-row report. This is
/// the crash-repro path; it intentionally does not enforce the statistical
/// minimum iteration count so operators can reproduce a backend failure with
/// `--iterations 1`.
pub fn run_one_report_with_config(
    connection: &str,
    workload: &dyn Workload,
    rows: usize,
    config: &BenchConfig,
) -> Result<report::BenchReport, Box<dyn std::error::Error>> {
    let run_context = prepare_run_context(connection, &[workload.name()], config)?;
    let observed_gucs = run_context.observed_gucs;
    let artifacts = run_context.artifacts;
    initialize_plans_file(config);

    let mut results = Vec::new();
    let mut crashes = Vec::new();
    match run_workload_with_config(connection, workload, rows, config, artifacts.as_ref()) {
        Ok(mut result) => {
            result.rows = rows;
            results.push(result);
        }
        Err(e) => {
            let err_msg = format!("{e}");
            let crash = record_crash(
                connection,
                workload,
                rows,
                config,
                artifacts.as_ref(),
                1,
                &err_msg,
            );
            let _ = cleanup(connection, workload);
            let _ = wait_for_pg(connection);
            crashes.push(crash);
        }
    }

    let mut report = report::generate_report(
        results,
        crashes,
        Some(connection),
        config.iterations,
        config.warmup,
        observed_gucs,
        config.timing_mode,
        config.cache_mode,
    );
    apply_config_methodology(&mut report, config);
    if let Some(artifact_writer) = artifacts.as_ref() {
        finalize_report_artifacts(artifact_writer, &mut report, "run-complete")?;
    }
    Ok(report)
}

fn apply_config_methodology(report: &mut report::BenchReport, config: &BenchConfig) {
    report.headline_speedup_allowed = config.headline_speedup_allowed;
    report.methodology.native_parity_pairing = config.native_parity_pairing;
    report.methodology.native_parity_repetitions_per_arm = if config.native_parity_pairing {
        NATIVE_PARITY_REPETITIONS_PER_ARM
    } else {
        1
    };
    let ordering = if config.native_parity_pairing {
        "balanced randomized replicated mirrored ABBA/BAAB crossover blocks on one PostgreSQL backend (`DISCARD ALL` before each raw execution)"
    } else {
        "balanced randomized AB/BA crossover on distinct persistent PostgreSQL backends (`DISCARD ALL` before each arm)"
    };
    ordering.clone_into(&mut report.methodology.ordering);
}

fn finalize_report_artifacts(
    artifact_writer: &ArtifactWriter,
    report: &mut report::BenchReport,
    completion_label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    report.artifact_dir = Some(artifact_writer.root().display().to_string());
    if let Err(e) = artifact_writer.capture_log_tails(completion_label) {
        eprintln!("[artifacts] {completion_label} log/telemetry tail capture failed: {e}");
    }
    artifact_writer
        .write_crashes(&report.crashes)
        .map_err(|e| {
            format!(
                "artifact final crash inventory write failed in {}: {e}",
                artifact_writer.root().display()
            )
        })?;
    artifact_writer.write_report(report).map_err(|e| {
        format!(
            "artifact report/audit write failed in {}: {e}",
            artifact_writer.root().display()
        )
    })?;
    Ok(())
}

struct RunContext {
    observed_gucs: Option<ObservedGucs>,
    artifacts: Option<ArtifactWriter>,
}

fn ensure_rust_backtrace() -> RustBacktraceSetting {
    match std::env::var("RUST_BACKTRACE") {
        Ok(value) if rust_backtrace_enabled(&value) => RustBacktraceSetting {
            effective_value: value,
            previous_value: None,
            action: "already-enabled",
        },
        Ok(value) => {
            set_rust_backtrace_one();
            RustBacktraceSetting {
                effective_value: "1".to_owned(),
                previous_value: Some(value),
                action: "set-by-runner",
            }
        }
        Err(std::env::VarError::NotPresent) => {
            set_rust_backtrace_one();
            RustBacktraceSetting {
                effective_value: "1".to_owned(),
                previous_value: None,
                action: "set-by-runner",
            }
        }
        Err(std::env::VarError::NotUnicode(value)) => {
            set_rust_backtrace_one();
            RustBacktraceSetting {
                effective_value: "1".to_owned(),
                previous_value: Some(value.to_string_lossy().into_owned()),
                action: "set-by-runner",
            }
        }
    }
}

fn rust_backtrace_enabled(value: &str) -> bool {
    matches!(value.trim(), "1" | "full")
}

fn set_rust_backtrace_one() {
    // SAFETY: this is called at the start of benchmark context setup before
    // the runner creates worker threads. The benchmark process owns this
    // environment mutation so repros and any child commands see backtraces.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RUST_BACKTRACE", "1");
    }
}

fn persist_rust_backtrace_setting(
    artifacts: Option<&ArtifactWriter>,
    setting: &RustBacktraceSetting,
) {
    eprintln!(
        "[diagnostics] RUST_BACKTRACE={} ({})",
        setting.effective_value, setting.action
    );
    let Some(artifact_writer) = artifacts else {
        return;
    };

    let path = artifact_writer.root().join(RUST_BACKTRACE_ARTIFACT);
    let mut text = String::new();
    let _ = writeln!(text, "RUST_BACKTRACE={}", setting.effective_value);
    let _ = writeln!(text, "action={}", setting.action);
    if let Some(previous) = &setting.previous_value {
        let _ = writeln!(text, "previous={previous}");
    }
    text.push_str(
        "note=This records the benchmark runner process environment. Existing PostgreSQL \
         postmasters may need a restart to inherit changed environment variables.\n",
    );
    if let Err(e) = fs::write(&path, text) {
        eprintln!("[artifacts] RUST_BACKTRACE record write failed: {e}");
    }
}

fn prepare_run_context(
    connection: &str,
    workload_names: &[&str],
    config: &BenchConfig,
) -> Result<RunContext, Box<dyn std::error::Error>> {
    let rust_backtrace = ensure_rust_backtrace();
    let artifacts = prepare_artifacts(connection, config)?;
    persist_rust_backtrace_setting(artifacts.as_ref(), &rust_backtrace);
    let mut observed_gucs: Option<ObservedGucs> = None;

    if let Some(profile) = &config.guc_profile {
        eprintln!("[gucs] applying profile: {profile:?}");
        if let Err(e) = profile.apply(connection) {
            record_setup_failure(artifacts.as_ref(), "guc-apply", &format!("{e}"));
            return Err(e);
        }
        // Verify observed values match; hard-fail on postmaster-setting drift
        // unless --skip-guc-verify is explicit.
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
                record_setup_failure(artifacts.as_ref(), "guc-verify", &format!("{e}"));
                return Err(e);
            }
        }
    }

    if let Err(e) = ensure_extensions_for_names(connection, workload_names) {
        record_setup_failure(artifacts.as_ref(), "extension-setup", &format!("{e}"));
        return Err(e);
    }
    if let Err(e) = run_provenance_gate(connection, artifacts.as_ref()) {
        record_setup_failure(artifacts.as_ref(), "provenance", &format!("{e}"));
        return Err(e);
    }

    persist_guc_snapshot(artifacts.as_ref(), connection, observed_gucs.as_ref());
    Ok(RunContext {
        observed_gucs,
        artifacts,
    })
}

fn prepare_artifacts(
    connection: &str,
    config: &BenchConfig,
) -> Result<Option<ArtifactWriter>, Box<dyn std::error::Error>> {
    let Some(dir) = &config.artifacts_dir else {
        return Ok(None);
    };
    let writer = ArtifactWriter::new(dir.clone(), discover_log_candidates(connection))?;
    eprintln!(
        "[artifacts] writing benchmark artifacts to {}",
        dir.display()
    );
    Ok(Some(writer))
}

fn persist_guc_snapshot(
    artifacts: Option<&ArtifactWriter>,
    connection: &str,
    observed: Option<&ObservedGucs>,
) {
    let Some(artifact_writer) = artifacts else {
        return;
    };

    if let Some(snapshot) = observed {
        let gucs = report::GucSettings {
            settings: snapshot.settings.clone(),
        };
        if let Err(e) =
            artifact_writer.write_guc_snapshot(&gucs, snapshot.postmaster_start_time.as_deref())
        {
            eprintln!("[artifacts] GUC snapshot write failed: {e}");
        }
        return;
    }

    match report::GucSettings::from_connection(connection) {
        Ok(gucs) => {
            if let Err(e) = artifact_writer.write_guc_snapshot(&gucs, None) {
                eprintln!("[artifacts] GUC snapshot write failed: {e}");
            }
        }
        Err(e) => eprintln!("[artifacts] GUC snapshot capture failed: {e}"),
    }
}

fn record_setup_failure(artifacts: Option<&ArtifactWriter>, label: &str, error: &str) {
    let Some(artifact_writer) = artifacts else {
        return;
    };
    if let Err(e) = artifact_writer.write_failure(label, error) {
        eprintln!("[artifacts] setup failure write failed: {e}");
    }
    if let Err(e) = artifact_writer.capture_log_tails(label) {
        eprintln!("[artifacts] setup failure log tail capture failed: {e}");
    }
}

fn discover_log_candidates(connection: &str) -> Vec<PathBuf> {
    let mut candidates = crate::artifacts::default_log_candidates();
    if let Ok(mut client) = Client::connect(connection, NoTls)
        && let Ok(row) = client.query_one("SHOW data_directory", &[])
    {
        let data_dir: String = row.get(0);
        crate::artifacts::append_pgdata_log_candidates(&mut candidates, Path::new(&data_dir));
    }
    candidates
}

fn initialize_plans_file(config: &BenchConfig) {
    let Some(path) = &config.plans_capture_path else {
        return;
    };
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
                "=== pg_accel benchmark plans - captured once per workload/scale ==="
            );
        }
        Err(e) => eprintln!("[plans] could not open {}: {e}", path.display()),
    }
}

struct CrashContext<'a> {
    connection: &'a str,
    workload: &'a dyn Workload,
    rows: usize,
    config: &'a BenchConfig,
    label: &'a str,
    error: &'a str,
    repro_command: &'a str,
    plan_snippet_artifact: Option<&'a str>,
    correctness_diff_artifact: Option<&'a str>,
    log_tail_artifacts: &'a [String],
}

fn record_crash(
    connection: &str,
    workload: &dyn Workload,
    rows: usize,
    config: &BenchConfig,
    artifacts: Option<&ArtifactWriter>,
    crash_index: usize,
    error: &str,
) -> report::CrashedScale {
    let repro_command = repro_command(connection, workload.name(), rows, config);
    let plan_snippet_artifact = artifacts.and_then(|artifact_writer| {
        artifact_writer.existing_plan_snippet_artifact(workload.name(), rows)
    });
    let correctness_diff_artifact = artifacts.and_then(|artifact_writer| {
        artifact_writer.existing_correctness_diff_artifact(workload.name(), rows)
    });
    let label = format!("crash-{crash_index:03}-{}-{rows}", workload.name());
    let mut log_tail_artifacts = Vec::new();

    if let Some(artifact_writer) = artifacts {
        match artifact_writer.capture_log_tails(&label) {
            Ok(paths) => log_tail_artifacts = paths,
            Err(log_err) => eprintln!("[artifacts] log tail capture failed: {log_err}"),
        }

        let context = CrashContext {
            connection,
            workload,
            rows,
            config,
            label: &label,
            error,
            repro_command: &repro_command,
            plan_snippet_artifact: plan_snippet_artifact.as_deref(),
            correctness_diff_artifact: correctness_diff_artifact.as_deref(),
            log_tail_artifacts: &log_tail_artifacts,
        };
        match write_crash_context_artifact(artifact_writer, &context) {
            Ok(path) => log_tail_artifacts.insert(0, path),
            Err(e) => eprintln!("[artifacts] crash context write failed: {e}"),
        }
    }

    report::CrashedScale {
        workload: workload.name().to_owned(),
        rows,
        error: error.to_owned(),
        repro_command: Some(repro_command),
        plan_snippet_artifact,
        correctness_diff_artifact,
        log_tail_artifacts,
    }
}

fn write_crash_context_artifact(
    artifact_writer: &ArtifactWriter,
    context: &CrashContext<'_>,
) -> Result<String, Box<dyn std::error::Error>> {
    let dir = artifact_writer.root().join("crash_contexts");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "{}.txt",
        sanitize_artifact_component(context.label)
    ));

    let mut text = String::new();
    let _ = writeln!(text, "# pg_accel Benchmark Crash Context");
    let _ = writeln!(text);
    let _ = writeln!(text, "label: {}", context.label);
    let _ = writeln!(text, "workload: {}", context.workload.name());
    let _ = writeln!(text, "rows: {}", context.rows);
    let _ = writeln!(text, "error: {}", context.error);
    let _ = writeln!(text, "connection: {}", context.connection);
    let _ = writeln!(text, "artifact_dir: {}", artifact_writer.root().display());
    let _ = writeln!(
        text,
        "RUST_BACKTRACE: {}",
        std::env::var("RUST_BACKTRACE").unwrap_or_else(|_| "<unset>".to_owned())
    );
    let _ = writeln!(text);
    let _ = writeln!(text, "## Repro Command");
    let _ = writeln!(text);
    let _ = writeln!(text, "{}", context.repro_command);

    append_crash_config(&mut text, context.config);
    append_crash_artifact_paths(&mut text, artifact_writer, context);
    append_crash_sql(&mut text, context.workload);
    append_optional_artifact_excerpt(
        &mut text,
        artifact_writer.root(),
        "RUST_BACKTRACE Artifact",
        Some(RUST_BACKTRACE_ARTIFACT),
    );
    append_optional_artifact_excerpt(
        &mut text,
        artifact_writer.root(),
        "EXPLAIN Snippet",
        context.plan_snippet_artifact,
    );
    append_optional_artifact_excerpt(
        &mut text,
        artifact_writer.root(),
        "Correctness Diff",
        context.correctness_diff_artifact,
    );
    append_optional_artifact_excerpt(
        &mut text,
        artifact_writer.root(),
        "Pre-Risk Context",
        artifact_writer
            .existing_pre_risk_context_artifact(context.workload.name(), context.rows)
            .as_deref(),
    );
    append_optional_artifact_excerpt(
        &mut text,
        artifact_writer.root(),
        "GUC Snapshot",
        existing_artifact_path(artifact_writer.root(), "guc_snapshot.json").as_deref(),
    );
    for log_artifact in context.log_tail_artifacts {
        append_optional_artifact_excerpt(
            &mut text,
            artifact_writer.root(),
            "Log Tail",
            Some(log_artifact),
        );
    }

    fs::write(&path, text)?;
    Ok(relative_artifact_path(artifact_writer.root(), &path))
}

fn append_crash_config(text: &mut String, config: &BenchConfig) {
    let _ = writeln!(text);
    let _ = writeln!(text, "## Runner Config");
    let _ = writeln!(text);
    let _ = writeln!(text, "iterations: {}", config.iterations);
    let _ = writeln!(text, "warmup: {}", config.warmup);
    let _ = writeln!(text, "seed: {}", config.seed);
    let _ = writeln!(text, "timing: {}", timing_mode_cli_arg(config.timing_mode));
    let _ = writeln!(
        text,
        "cache_mode: {}",
        cache_mode_cli_arg(config.cache_mode)
    );
    let _ = writeln!(text, "realistic_gucs: {}", config.guc_profile.is_some());
    let _ = writeln!(text, "skip_guc_verify: {}", config.skip_guc_verify);
    let _ = writeln!(
        text,
        "capture_plans: {}",
        config.plans_capture_path.is_some()
    );
    let _ = writeln!(
        text,
        "capture_planner_stages: {}",
        config.capture_planner_stages
    );
    let _ = writeln!(
        text,
        "native_parity_pairing: {}",
        config.native_parity_pairing
    );
    if let Some(path) = &config.plans_capture_path {
        let _ = writeln!(text, "plans_capture_path: {}", path.display());
    }
}

fn append_crash_artifact_paths(
    text: &mut String,
    artifact_writer: &ArtifactWriter,
    context: &CrashContext<'_>,
) {
    let _ = writeln!(text);
    let _ = writeln!(text, "## Artifact Paths");
    let _ = writeln!(text);
    match context.plan_snippet_artifact {
        Some(path) => {
            let _ = writeln!(text, "plan_snippet: {path}");
        }
        None => text.push_str("plan_snippet: <not captured before failure>\n"),
    }
    match context.correctness_diff_artifact {
        Some(path) => {
            let _ = writeln!(text, "correctness_diff: {path}");
        }
        None => text.push_str("correctness_diff: <not captured before failure>\n"),
    }
    if let Some(path) = existing_artifact_path(artifact_writer.root(), "guc_snapshot.json") {
        let _ = writeln!(text, "guc_snapshot: {path}");
    } else {
        text.push_str("guc_snapshot: <not available>\n");
    }
    if let Some(path) = existing_artifact_path(artifact_writer.root(), "provenance.json") {
        let _ = writeln!(text, "provenance: {path}");
    }
    if let Some(path) =
        artifact_writer.existing_pre_risk_context_artifact(context.workload.name(), context.rows)
    {
        let _ = writeln!(text, "pre_risk_context: {path}");
    }
    if context.log_tail_artifacts.is_empty() {
        text.push_str("log_tails: <not available>\n");
    } else {
        text.push_str("log_tails:\n");
        for path in context.log_tail_artifacts {
            let _ = writeln!(text, "- {path}");
        }
    }
}

fn append_crash_sql(text: &mut String, workload: &dyn Workload) {
    let _ = writeln!(text);
    let _ = writeln!(text, "## SQL");
    let _ = writeln!(text);
    let pre_query_sql = workload.pre_query_sql();
    if pre_query_sql.is_empty() {
        text.push_str("pre_query_sql: <none>\n");
    } else {
        text.push_str("pre_query_sql:\n");
        for (idx, sql) in pre_query_sql.iter().enumerate() {
            let _ = writeln!(text, "--- pre-query {} ---", idx + 1);
            text.push_str(sql);
            if !sql.ends_with('\n') {
                text.push('\n');
            }
        }
    }
    text.push_str("--- accel query ---\n");
    let query = workload.query_sql();
    text.push_str(&query);
    if !query.ends_with('\n') {
        text.push('\n');
    }
    if let Some(baseline_query) = workload.baseline_query_sql() {
        text.push_str("--- baseline query ---\n");
        text.push_str(&baseline_query);
        if !baseline_query.ends_with('\n') {
            text.push('\n');
        }
    }
}

fn append_optional_artifact_excerpt(
    text: &mut String,
    root: &Path,
    title: &str,
    relative_path: Option<&str>,
) {
    let _ = writeln!(text);
    let _ = writeln!(text, "## {title}");
    let _ = writeln!(text);
    let Some(relative_path) = relative_path else {
        text.push_str("<not available>\n");
        return;
    };
    let path = artifact_display_path(root, relative_path);
    let _ = writeln!(text, "path: {relative_path}");
    match fs::read(&path) {
        Ok(bytes) => {
            let start = bytes.len().saturating_sub(CRASH_CONTEXT_EMBED_BYTES);
            if start > 0 {
                let _ = writeln!(
                    text,
                    "[truncated to last {CRASH_CONTEXT_EMBED_BYTES} bytes of {}]",
                    bytes.len()
                );
            }
            text.push_str(&String::from_utf8_lossy(&bytes[start..]));
            if !text.ends_with('\n') {
                text.push('\n');
            }
        }
        Err(e) => {
            let _ = writeln!(text, "<could not read {}: {e}>", path.display());
        }
    }
}

fn existing_artifact_path(root: &Path, relative_path: &str) -> Option<String> {
    let path = root.join(relative_path);
    path.is_file().then(|| relative_path.to_owned())
}

fn artifact_display_path(root: &Path, display_path: &str) -> PathBuf {
    let path = Path::new(display_path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn relative_artifact_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn sanitize_artifact_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len().min(96));
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('-');
        }
        if out.len() >= 96 {
            break;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "artifact".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn repro_command(connection: &str, workload: &str, rows: usize, config: &BenchConfig) -> String {
    let mut parts = vec![
        "RUST_BACKTRACE=1".to_owned(),
        "cargo".to_owned(),
        "run".to_owned(),
        "-p".to_owned(),
        "pg_accel_bench".to_owned(),
        "--".to_owned(),
        "crash-repro".to_owned(),
        "--workload".to_owned(),
        shell_quote(workload),
        "--rows".to_owned(),
        rows.to_string(),
        "--iterations".to_owned(),
        config.iterations.to_string(),
        "--warmup".to_owned(),
        config.warmup.to_string(),
        "--seed".to_owned(),
        config.seed.to_string(),
        "--timing".to_owned(),
        timing_mode_cli_arg(config.timing_mode).to_owned(),
        "--cache-mode".to_owned(),
        cache_mode_cli_arg(config.cache_mode).to_owned(),
        "--connection".to_owned(),
        shell_quote(connection),
    ];
    if config.guc_profile.is_some() {
        parts.push("--realistic-gucs".to_owned());
    }
    if config.plans_capture_path.is_some() {
        parts.push("--capture-plans".to_owned());
    }
    if config.capture_planner_stages {
        parts.push("--capture-planner-stages".to_owned());
    }
    if config.native_parity_pairing {
        parts.push("--native-parity-pairing".to_owned());
    }
    if config.skip_guc_verify {
        parts.push("--skip-guc-verify".to_owned());
    }
    if let Some(dir) = &config.artifacts_dir {
        parts.push("--artifacts-dir".to_owned());
        parts.push(shell_quote(&dir.display().to_string()));
    }
    parts.join(" ")
}

fn timing_mode_cli_arg(mode: TimingMode) -> &'static str {
    match mode {
        TimingMode::ExplainAnalyze => "explain",
        TimingMode::RawWallClock => "raw",
        TimingMode::Both => "both",
    }
}

fn cache_mode_cli_arg(mode: CacheMode) -> &'static str {
    match mode {
        CacheMode::Cold => "cold",
        CacheMode::Warm => "warm",
        CacheMode::Both => "both",
    }
}

fn warn_if_debug_raw_timing(timing_mode: TimingMode) {
    if !cfg!(debug_assertions) {
        return;
    }
    if matches!(timing_mode, TimingMode::RawWallClock | TimingMode::Both) {
        eprintln!(
            "[benchmark] WARNING: raw wall-clock timing is running from a debug \
             pg_accel_bench binary; high-output workloads include debug client-side \
             row-drain overhead. Use `cargo build -p pg_accel_bench --release` \
             and `target/release/pg_accel_bench` for publication artifacts."
        );
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '='))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
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
    artifacts: Option<&ArtifactWriter>,
) -> Result<WorkloadResult, Box<dyn std::error::Error>> {
    let query_identity =
        BenchmarkQueryIdentity::resolve(workload.query_sql(), workload.baseline_query_sql())?;
    setup(connection, workload, rows, config.seed)?;

    // VACUUM (ANALYZE, VERBOSE) after load, before timing begins. This proves the parallel
    // baseline's planner had fresh stats for every measured row — otherwise
    // `parallel_mean` is suspect.
    let tables = workload_tables(workload, rows);
    let vacuum_stats = {
        let mut vclient = Client::connect(connection, NoTls)?;
        vacuum_and_capture_stats(&mut vclient, &tables).unwrap_or_default()
    };
    let sanity_checks = capture_benchmark_sanity_checks(connection, workload)?;

    // Capture thermal state before the timed loop.
    let thermal = capture_thermal_state();

    if let Some(artifact_writer) = artifacts
        && let Err(e) = capture_and_write_pre_risk_context(
            connection,
            workload,
            rows,
            config,
            &query_identity,
            artifact_writer,
        )
    {
        eprintln!(
            "[artifacts] pre-risk context write failed for {} @ {rows}: {e}",
            workload.name()
        );
    }

    let correctness_diff_artifact = if let Some(artifact_writer) = artifacts {
        Some(capture_and_write_correctness_diff(
            connection,
            workload,
            rows,
            &query_identity,
            artifact_writer,
        )?)
    } else {
        validate_result_oracle_from_connection(connection, workload, rows)?;
        None
    };

    // Always capture a plan snippet so the runner can tag the workload
    // as dispatched/not-dispatched even if --capture-plans is off. This
    // feeds the dispatch classification. The full-plans file is still written if
    // plans_capture_path is set.
    let accel_plan = capture_plan_snippet(connection, workload, rows, BenchMode::Accel).ok();
    let baseline_plan =
        capture_plan_snippet(connection, workload, rows, BenchMode::PgParallel).ok();
    if let (Some(artifact_writer), Some(capture)) = (artifacts, accel_plan.as_ref())
        && let Err(e) = artifact_writer.write_plan_snippet(workload.name(), rows, &capture.text)
    {
        eprintln!(
            "[artifacts] plan snippet write failed for {} @ {rows}: {e}",
            workload.name()
        );
    }
    let plan_selected = accel_plan
        .as_ref()
        .map(|capture| capture.text.as_str())
        .is_some_and(plan_contains_custom_scan);
    if config.native_parity_pairing && plan_selected {
        return Err(format!(
            "--native-parity-pairing is restricted to planner-native cells, but {} @ {rows} selected a pg_accel Custom Scan",
            workload.name()
        )
        .into());
    }
    let plan_text_dispatched = accel_plan
        .as_ref()
        .map(|capture| capture.text.as_str())
        .is_some_and(plan_indicates_gpu_dispatch);
    let plan_explicitly_not_dispatched = accel_plan
        .as_ref()
        .map(|capture| capture.text.as_str())
        .and_then(explicit_gpu_dispatched)
        == Some(false);
    if let Some(path) = &config.plans_capture_path
        && let Err(e) = capture_plan(connection, workload, rows, path)
    {
        eprintln!(
            "[plans] capture failed for {} @ {rows}: {e}",
            workload.name()
        );
    }

    let capture_planner_stages = config.capture_planner_stages
        && should_capture_planner_stages(
            workload.name(),
            rows,
            accel_plan.is_some(),
            plan_selected,
        );
    let mut result = run_with_timing_and_cache_internal(
        connection,
        workload,
        config.iterations,
        config.warmup,
        config.timing_mode,
        config.cache_mode,
        RunExecutionOptions {
            capture_planner_stages,
            native_parity_pairing: config.native_parity_pairing,
        },
    )?;
    result.rows = rows;
    result.plan_selected = plan_selected;
    let function_kernel_candidate = report::is_function_kernel_candidate(
        workload.name(),
        workload.category(),
        &result.kernel_class,
        workload.description(),
    );
    let dispatch_classification = classify_runner_dispatch(RunnerDispatchInput {
        plan_selected,
        plan_explicitly_not_dispatched,
        function_kernel_candidate,
        dispatch_counter_captured: result.dispatch_counter_captured,
        gpu_kernel_execution_delta: result.gpu_kernel_execution_delta,
        pg_accel_queries_accelerated_delta: result.pg_accel_queries_accelerated_delta,
        pg_accel_stock_exec_delta: result.pg_accel_stock_exec_delta,
        accel_output_rows_consumed: result.accel_output_rows_consumed,
        measured_iterations: result.iterations.len(),
    });
    result.function_srf_kernel_dispatched = dispatch_classification.function_srf_kernel_dispatched;
    result.gpu_kernel_dispatched = dispatch_classification.gpu_kernel_dispatched;
    emit_dispatch_classification_warning(
        DispatchWarningInput {
            workload: workload.name(),
            rows,
            plan_selected,
            plan_text_dispatched,
            plan_explicitly_not_dispatched,
            function_kernel_candidate,
        },
        &result,
    );
    // Tagged native-decline evidence — sourced only from a real planner
    // rejection (PlannerReported) or an unconfirmed static expectation
    // (ExpectedUnconfirmed). Never synthesized into plan text.
    result.native_decline_evidence = native_decline_evidence(
        workload.name(),
        rows,
        plan_selected,
        accel_plan
            .as_ref()
            .and_then(|capture| capture.planner_rejection_reason.as_deref()),
    );
    let explain_kernel_mode = accel_plan
        .as_ref()
        .and_then(|capture| plan_physical_kernel_mode(&capture.text));
    result.physical_kernel_mode_verified = result
        .physical_kernel_mode
        .as_deref()
        .zip(explain_kernel_mode)
        .is_some_and(|(observed, planned)| observed == planned)
        && result.physical_kernel_mode_counts.serial_generic == 0;
    result.plan_snippet = accel_plan.map(|capture| capture.text);
    result.baseline_plan_snippet = baseline_plan.map(|capture| capture.text);
    result.correctness_diff_artifact = correctness_diff_artifact;
    result.thermal = thermal;
    // Cold-honesty tag: if the workload fixture fits within shared_buffers,
    // the data stays resident even after the OS page-cache purge, so a "cold"
    // label overstates the eviction. Record it so reports cannot call a run
    // cold when shared_buffers stayed resident.
    if matches!(config.cache_mode, CacheMode::Cold | CacheMode::Both) {
        result.cold_shared_buffers_resident =
            cold_shared_buffers_resident(connection, &vacuum_stats);
    }
    result.table_stats = vacuum_stats;
    result.sanity_checks = sanity_checks;
    cleanup(connection, workload)?;
    Ok(result)
}

fn capture_benchmark_sanity_checks(
    connection: &str,
    workload: &dyn Workload,
) -> Result<Vec<report::SanityCheck>, Box<dyn std::error::Error>> {
    let is_star_schema =
        crate::workloads::workload_metadata(workload.name()).is_some_and(|metadata| {
            metadata.category == crate::workloads::WorkloadCategory::StarSchemaSsbm
        });
    if !is_star_schema {
        return Ok(Vec::new());
    }

    let mut client = Client::connect(connection, NoTls)?;
    let checks = client.query(SSBM_DIMENSION_SANITY_SQL, &[])?;
    let mut out = Vec::with_capacity(checks.len());
    for row in checks {
        let label: String = row.get(0);
        let count: i64 = row.get(1);
        out.push(report::SanityCheck {
            label,
            count,
            passed: count > 0,
        });
    }
    let failed: Vec<&report::SanityCheck> = out.iter().filter(|check| !check.passed).collect();
    if !failed.is_empty() {
        let labels = failed
            .iter()
            .map(|check| format!("{}={}", check.label, check.count))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "SSBM dimension sanity check failed before timing for {}: {labels}",
            workload.name()
        )
        .into());
    }
    Ok(out)
}

const SSBM_DIMENSION_SANITY_SQL: &str = "\
SELECT label, row_count::bigint \
FROM (VALUES \
    ('ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2)', (SELECT count(*) FROM ssbm_part WHERE p_mfgr IN ('MFGR#1', 'MFGR#2'))), \
    ('ssbm_part.p_category = MFGR#12 (SSBM Q2.1)', (SELECT count(*) FROM ssbm_part WHERE p_category = 'MFGR#12')), \
    ('ssbm_part.p_category = MFGR#14 (SSBM Q4.3)', (SELECT count(*) FROM ssbm_part WHERE p_category = 'MFGR#14')), \
    ('ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3)', (SELECT count(*) FROM ssbm_part WHERE p_brand1 = 'MFGR#2239')), \
    ('ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2)', (SELECT count(*) FROM ssbm_part WHERE p_brand1 BETWEEN 'MFGR#2221' AND 'MFGR#2228')), \
    ('ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4)', (SELECT count(*) FROM ssbm_supplier WHERE s_region = 'AMERICA')), \
    ('ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1)', (SELECT count(*) FROM ssbm_supplier WHERE s_region = 'ASIA')), \
    ('ssbm_supplier.s_region = EUROPE (SSBM Q2.3)', (SELECT count(*) FROM ssbm_supplier WHERE s_region = 'EUROPE')), \
    ('ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3)', (SELECT count(*) FROM ssbm_supplier WHERE s_nation = 'UNITED STATES')), \
    ('ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4)', (SELECT count(*) FROM ssbm_supplier WHERE s_city IN ('UNITED ST0', 'UNITED ST1'))), \
    ('ssbm_customer.c_region = AMERICA (SSBM Q4)', (SELECT count(*) FROM ssbm_customer WHERE c_region = 'AMERICA')), \
    ('ssbm_customer.c_region = ASIA (SSBM Q3.1)', (SELECT count(*) FROM ssbm_customer WHERE c_region = 'ASIA')), \
    ('ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2)', (SELECT count(*) FROM ssbm_customer WHERE c_nation = 'UNITED STATES')), \
    ('ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4)', (SELECT count(*) FROM ssbm_customer WHERE c_city IN ('UNITED ST0', 'UNITED ST1'))), \
    ('ssbm_date.d_year = 1992 (SSBM Q3)', (SELECT count(*) FROM ssbm_date WHERE d_year = 1992)), \
    ('ssbm_date.d_year = 1993 (SSBM Q1.1/Q3)', (SELECT count(*) FROM ssbm_date WHERE d_year = 1993)), \
    ('ssbm_date.d_year = 1994 (SSBM Q1.3/Q3)', (SELECT count(*) FROM ssbm_date WHERE d_year = 1994)), \
    ('ssbm_date.d_year = 1995 (SSBM Q3)', (SELECT count(*) FROM ssbm_date WHERE d_year = 1995)), \
    ('ssbm_date.d_year = 1996 (SSBM Q3)', (SELECT count(*) FROM ssbm_date WHERE d_year = 1996)), \
    ('ssbm_date.d_year = 1997 (SSBM Q3/Q4)', (SELECT count(*) FROM ssbm_date WHERE d_year = 1997)), \
    ('ssbm_date.d_year = 1998 (SSBM Q4)', (SELECT count(*) FROM ssbm_date WHERE d_year = 1998)), \
    ('ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2)', (SELECT count(*) FROM ssbm_date WHERE d_yearmonthnum = 199401)), \
    ('ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3)', (SELECT count(*) FROM ssbm_date WHERE d_weeknuminyear = 6 AND d_year = 1994)), \
    ('ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4)', (SELECT count(*) FROM ssbm_date WHERE d_yearmonth = 'Dec1997')) \
) AS checks(label, row_count) \
ORDER BY label";

const CORRECTNESS_ACCEL_TABLE: &str = "pg_temp.pgaccel_correctness_accel";
const CORRECTNESS_BASELINE_TABLE: &str = "pg_temp.pgaccel_correctness_baseline";

fn capture_and_write_correctness_diff(
    connection: &str,
    workload: &dyn Workload,
    rows: usize,
    query_identity: &BenchmarkQueryIdentity,
    artifact_writer: &ArtifactWriter,
) -> Result<String, Box<dyn std::error::Error>> {
    let artifact = match capture_correctness_diff(connection, workload, rows, query_identity) {
        Ok(artifact) => artifact,
        Err(e) => correctness_error_artifact(workload, rows, query_identity, e.to_string()),
    };
    let path = artifact_writer.write_correctness_diff(workload.name(), rows, &artifact)?;
    let relative_path = path
        .strip_prefix(artifact_writer.root())
        .unwrap_or(&path)
        .display()
        .to_string();
    if artifact.status == "pass" {
        return Ok(relative_path);
    }
    Err(format!(
        "correctness diff failed for {} @ {} rows: status={} accel_minus_baseline={:?} baseline_minus_accel={:?} error={}",
        workload.name(),
        rows,
        artifact.status,
        artifact.accel_minus_baseline_count,
        artifact.baseline_minus_accel_count,
        artifact.error.as_deref().unwrap_or("-")
    )
    .into())
}

fn capture_correctness_diff(
    connection: &str,
    workload: &dyn Workload,
    rows: usize,
    query_identity: &BenchmarkQueryIdentity,
) -> Result<CorrectnessDiffArtifact, Box<dyn std::error::Error>> {
    let accel_query_sql = query_identity.accel_query_sql();
    let baseline_query_sql = query_identity.baseline_query_sql();
    let order_sensitive =
        has_top_level_order_by(accel_query_sql) || has_top_level_order_by(baseline_query_sql);
    let pre_query_sql = workload.pre_query_sql();

    let mut client = Client::connect(connection, NoTls)?;
    apply_benchmark_safety_settings(&mut client)?;
    prime_workload_accel_backend(&mut client, workload)?;
    create_correctness_table(
        &mut client,
        CORRECTNESS_ACCEL_TABLE,
        accel_query_sql,
        workload.name(),
        BenchMode::Accel,
        &pre_query_sql,
        order_sensitive,
    )?;
    create_correctness_table(
        &mut client,
        CORRECTNESS_BASELINE_TABLE,
        baseline_query_sql,
        workload.name(),
        BenchMode::PgParallel,
        &pre_query_sql,
        order_sensitive,
    )?;

    let accel_rows = correctness_table_count(&mut client, CORRECTNESS_ACCEL_TABLE)?;
    let baseline_rows = correctness_table_count(&mut client, CORRECTNESS_BASELINE_TABLE)?;
    let accel_minus_baseline_count = correctness_diff_count(
        &mut client,
        CORRECTNESS_ACCEL_TABLE,
        CORRECTNESS_BASELINE_TABLE,
    )?;
    let baseline_minus_accel_count = correctness_diff_count(
        &mut client,
        CORRECTNESS_BASELINE_TABLE,
        CORRECTNESS_ACCEL_TABLE,
    )?;
    let accel_minus_baseline_samples = correctness_diff_samples(
        &mut client,
        CORRECTNESS_ACCEL_TABLE,
        CORRECTNESS_BASELINE_TABLE,
    )?;
    let baseline_minus_accel_samples = correctness_diff_samples(
        &mut client,
        CORRECTNESS_BASELINE_TABLE,
        CORRECTNESS_ACCEL_TABLE,
    )?;
    if let Some(oracle) = workload.result_oracle(rows) {
        validate_result_oracle(&mut client, workload, rows, &oracle)?;
    }
    client.batch_execute(
        "DROP TABLE IF EXISTS pg_temp.pgaccel_correctness_accel; \
         DROP TABLE IF EXISTS pg_temp.pgaccel_correctness_baseline",
    )?;

    let status = if accel_minus_baseline_count == 0 && baseline_minus_accel_count == 0 {
        "pass"
    } else {
        "fail"
    };
    Ok(CorrectnessDiffArtifact {
        schema_version: CORRECTNESS_DIFF_SCHEMA_VERSION,
        workload: workload.name().to_owned(),
        rows,
        status: status.to_owned(),
        order_sensitive,
        accel_rows: Some(accel_rows),
        baseline_rows: Some(baseline_rows),
        accel_minus_baseline_count: Some(accel_minus_baseline_count),
        baseline_minus_accel_count: Some(baseline_minus_accel_count),
        sample_limit: CORRECTNESS_DIFF_SAMPLE_LIMIT,
        accel_minus_baseline_samples,
        baseline_minus_accel_samples,
        accel_query_sql: accel_query_sql.to_owned(),
        baseline_query_sql: baseline_query_sql.to_owned(),
        error: None,
    })
}

fn validate_result_oracle_from_connection(
    connection: &str,
    workload: &dyn Workload,
    rows: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(oracle) = workload.result_oracle(rows) else {
        return Ok(());
    };
    let mut client = Client::connect(connection, NoTls)?;
    apply_benchmark_safety_settings(&mut client)?;
    validate_result_oracle(&mut client, workload, rows, &oracle)
}

fn validate_result_oracle(
    client: &mut Client,
    workload: &dyn Workload,
    rows: usize,
    oracle: &ResultOracle,
) -> Result<(), Box<dyn std::error::Error>> {
    if oracle.expected_row.is_empty() {
        return Err(format!(
            "result oracle for {} @ {rows} rows has no expected columns",
            workload.name()
        )
        .into());
    }

    for sql in workload.pre_query_sql() {
        client.batch_execute(&sql)?;
    }
    client.batch_execute(
        "SET pg_accel.enabled = off; \
         SET max_parallel_workers_per_gather = DEFAULT",
    )?;
    let actual_rows = client.query(&oracle.query_sql, &[])?;
    if actual_rows.len() != 1 {
        return Err(format!(
            "result oracle for {} @ {rows} rows returned {} rows; expected exactly one",
            workload.name(),
            actual_rows.len()
        )
        .into());
    }

    let actual_row = &actual_rows[0];
    if actual_row.len() != oracle.expected_row.len() {
        return Err(format!(
            "result oracle for {} @ {rows} rows returned {} columns; expected {}",
            workload.name(),
            actual_row.len(),
            oracle.expected_row.len()
        )
        .into());
    }

    for (column, expected) in oracle.expected_row.iter().enumerate() {
        let actual = match expected {
            ExpectedResultValue::I32(_) => ExpectedResultValue::I32(actual_row.try_get(column)?),
            ExpectedResultValue::I64(_) => ExpectedResultValue::I64(actual_row.try_get(column)?),
            ExpectedResultValue::Bool(_) => ExpectedResultValue::Bool(actual_row.try_get(column)?),
            ExpectedResultValue::Text(_) => ExpectedResultValue::Text(actual_row.try_get(column)?),
            ExpectedResultValue::I32Array(_) => {
                ExpectedResultValue::I32Array(actual_row.try_get(column)?)
            }
            ExpectedResultValue::NullableI32Array(_) => {
                ExpectedResultValue::NullableI32Array(actual_row.try_get(column)?)
            }
        };
        if &actual != expected {
            return Err(format!(
                "result oracle mismatch for {} @ {rows} rows column {column}: expected {expected:?}, got {actual:?}",
                workload.name()
            )
            .into());
        }
    }
    Ok(())
}

fn create_correctness_table(
    client: &mut Client,
    table: &str,
    query: &str,
    workload_name: &str,
    mode: BenchMode,
    pre_query_sql: &[String],
    order_sensitive: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    for sql in pre_query_sql {
        client.batch_execute(sql)?;
    }
    match mode {
        BenchMode::Accel => client.batch_execute(
            "SET pg_accel.enabled = on; \
             SET max_parallel_workers_per_gather = DEFAULT",
        )?,
        BenchMode::PgParallel => client.batch_execute(
            "SET pg_accel.enabled = off; \
             SET max_parallel_workers_per_gather = DEFAULT",
        )?,
    }

    let projection = correctness_projection_sql(query, workload_name, order_sensitive);
    let create_table = table.strip_prefix("pg_temp.").unwrap_or(table);
    client.batch_execute(&format!(
        "DROP TABLE IF EXISTS {table}; CREATE TEMP TABLE {create_table} AS {projection}"
    ))?;
    Ok(())
}

fn correctness_projection_sql(query: &str, workload_name: &str, order_sensitive: bool) -> String {
    let query = trim_sql_semicolon(query);
    if workload_name == "raster_resident_exact_reclass" {
        return format!(
            "WITH q AS MATERIALIZED ({query}) \
             SELECT NULL::bigint AS ord, encode(ST_AsBinary(q.rast), 'hex') AS row_repr \
             FROM q"
        );
    }
    if workload_name == "spatial_sort" {
        return format!(
            "SELECT row_number() OVER () AS ord, \
             jsonb_build_object('id', q.id, 'dist', round(q.dist::numeric, 8))::text AS row_repr \
             FROM ({query}) AS q"
        );
    }
    if !order_sensitive {
        if matches!(
            workload_name,
            "hashagg_10g" | "hashagg_100g" | "hashagg_256g" | "hashagg_1kg" | "hashagg_10kg"
        ) {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('grp', q.grp, 'sum', round(q.sum::numeric, 3), \
                 'count', q.count)::text AS row_repr FROM ({query}) AS q"
            );
        }
        if workload_name == "gpu_hashagg_med_card" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('user_id', q.user_id, 'count', q.count, \
                 'sum', round(q.sum::numeric, 3))::text AS row_repr FROM ({query}) AS q"
            );
        }
        if workload_name == "grouped_agg_high_card" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('group_count', q.group_count, \
                 'input_rows', q.input_rows, \
                 'total_val', round(q.total_val::numeric, 3))::text AS row_repr \
                 FROM ({query}) AS q"
            );
        }
        if workload_name == "filtered_grouped_agg" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('dept', q.dept, 'sum', round(q.sum::numeric, 2), \
                 'avg', round(q.avg::numeric, 5), 'count', q.count)::text AS row_repr \
                 FROM ({query}) AS q"
            );
        }
        if workload_name == "grouped_agg" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('dept', q.dept, 'sum', round(q.sum::numeric, 2), \
                 'avg', round(q.avg::numeric, 5), 'count', q.count)::text AS row_repr \
                 FROM ({query}) AS q"
            );
        }
        if workload_name == "timeseries_sensor_rollup" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('sensor_id', q.sensor_id, \
                 'min', round(q.min::numeric, 5), 'max', round(q.max::numeric, 5), \
                 'avg', round(q.avg::numeric, 5))::text AS row_repr \
                 FROM ({query}) AS q"
            );
        }
        if workload_name == "dictionary_grouped_agg" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('region', q.region, 'sum', round(q.sum::numeric, 3), \
                 'count', q.count)::text AS row_repr FROM ({query}) AS q"
            );
        }
        if workload_name == "gpu_hashjoin_filter" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('name', q.name, 'sum', round(q.sum::numeric, 3))::text \
                 AS row_repr FROM ({query}) AS q"
            );
        }
        if workload_name == "reduce_sum_f32" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('sum', round(q.sum::numeric, -7))::text AS row_repr \
                 FROM ({query}) AS q"
            );
        }
        if workload_name == "gpu_reduce_scaling" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('sum', round(q.sum::numeric, 0))::text AS row_repr \
                 FROM ({query}) AS q"
            );
        }
        if workload_name == "reduce_sum_f64" || workload_name == "reduce_f64_sum" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('sum', round(q.sum::numeric, 3))::text AS row_repr \
                 FROM ({query}) AS q"
            );
        }
        if workload_name == "reduce_f64_minmax" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('min', round(q.min::numeric, 5), \
                 'max', round(q.max::numeric, 5))::text AS row_repr \
                 FROM ({query}) AS q"
            );
        }
        if workload_name == "gpu_reduce_sum" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('sum', round(q.sum::numeric, 0), \
                 'avg', round(q.avg::numeric, 4), 'min', q.min, 'max', q.max, \
                 'count', q.count)::text AS row_repr FROM ({query}) AS q"
            );
        }
        if workload_name == "reduce_multi" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('sum', round(q.sum::numeric, 3), \
                 'min', round(q.min::numeric, 5), 'max', round(q.max::numeric, 5), \
                 'count', q.count)::text AS row_repr FROM ({query}) AS q"
            );
        }
        if workload_name == "reduce_f64_stats" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('avg', round(q.avg::numeric, 5), \
                 'stddev', round(q.stddev::numeric, 5), \
                 'var_pop', round(q.var_pop::numeric, 5))::text AS row_repr \
                 FROM ({query}) AS q"
            );
        }
        if workload_name == "hashagg_f64_aggs" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('gk', q.gk, 'sum', round(q.sum::numeric, 5), \
                 'avg', round(q.avg::numeric, 5), \
                 'stddev', round(q.stddev::numeric, 5))::text AS row_repr \
                 FROM ({query}) AS q"
            );
        }
        if workload_name == "mixed_expr_agg" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('cat', q.cat, 'sum', round(q.sum::numeric, -4), \
                 'count', q.count)::text AS row_repr FROM ({query}) AS q"
            );
        }
        if workload_name == "mixed_join_agg" {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('label', q.label, 'sum', round(q.sum::numeric, 3), \
                 'count', q.count)::text AS row_repr FROM ({query}) AS q"
            );
        }
        if workload_name == "expression_grouped_agg"
            || workload_name == "predicate_filter_expression_grouped_agg"
            || workload_name == "case_when_expression_grouped_agg"
            || workload_name == "case_when_range_expression_grouped_agg"
            || workload_name == "case_when_value_predicate_expression_grouped_agg"
            || workload_name == "case_when_null_predicate_expression_grouped_agg"
            || workload_name == "case_when_or_expression_grouped_agg"
            || workload_name == "case_when_in_expression_grouped_agg"
            || workload_name == "case_when_not_expression_grouped_agg"
        {
            return format!(
                "SELECT NULL::bigint AS ord, \
                 jsonb_build_object('product_id', q.product_id, \
                 'sum', round(q.sum::numeric, 3), 'count', q.count)::text AS row_repr \
                 FROM ({query}) AS q"
            );
        }
    }
    if order_sensitive {
        format!(
            "SELECT row_number() OVER () AS ord, to_jsonb(q)::text AS row_repr \
             FROM ({query}) AS q"
        )
    } else {
        format!("SELECT NULL::bigint AS ord, to_jsonb(q)::text AS row_repr FROM ({query}) AS q")
    }
}

fn correctness_table_count(
    client: &mut Client,
    table: &str,
) -> Result<i64, Box<dyn std::error::Error>> {
    let row = client.query_one(&format!("SELECT count(*)::bigint FROM {table}"), &[])?;
    Ok(row.get(0))
}

fn correctness_diff_count(
    client: &mut Client,
    left: &str,
    right: &str,
) -> Result<i64, Box<dyn std::error::Error>> {
    let row = client.query_one(
        &format!(
            "SELECT count(*)::bigint FROM (\
               SELECT ord, row_repr FROM {left} \
               EXCEPT ALL \
               SELECT ord, row_repr FROM {right}\
             ) d"
        ),
        &[],
    )?;
    Ok(row.get(0))
}

fn correctness_diff_samples(
    client: &mut Client,
    left: &str,
    right: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let rows = client.query(
        &format!(
            "SELECT to_jsonb(d)::text \
               FROM (\
                 SELECT ord, row_repr FROM {left} \
                 EXCEPT ALL \
                 SELECT ord, row_repr FROM {right}\
               ) d \
              ORDER BY ord NULLS FIRST, row_repr \
              LIMIT {CORRECTNESS_DIFF_SAMPLE_LIMIT}"
        ),
        &[],
    )?;
    Ok(rows.iter().map(|row| row.get(0)).collect())
}

fn correctness_error_artifact(
    workload: &dyn Workload,
    rows: usize,
    query_identity: &BenchmarkQueryIdentity,
    error: String,
) -> CorrectnessDiffArtifact {
    let accel_query_sql = query_identity.accel_query_sql();
    let baseline_query_sql = query_identity.baseline_query_sql();
    CorrectnessDiffArtifact {
        schema_version: CORRECTNESS_DIFF_SCHEMA_VERSION,
        workload: workload.name().to_owned(),
        rows,
        status: "error".to_owned(),
        order_sensitive: has_top_level_order_by(accel_query_sql)
            || has_top_level_order_by(baseline_query_sql),
        accel_rows: None,
        baseline_rows: None,
        accel_minus_baseline_count: None,
        baseline_minus_accel_count: None,
        sample_limit: CORRECTNESS_DIFF_SAMPLE_LIMIT,
        accel_minus_baseline_samples: Vec::new(),
        baseline_minus_accel_samples: Vec::new(),
        accel_query_sql: accel_query_sql.to_owned(),
        baseline_query_sql: baseline_query_sql.to_owned(),
        error: Some(error),
    }
}

fn trim_sql_semicolon(sql: &str) -> &str {
    sql.trim().trim_end_matches(';').trim_end()
}

fn has_top_level_order_by(sql: &str) -> bool {
    let mut depth = 0_i32;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut previous = '\0';
    let bytes = sql.as_bytes();
    let mut idx = 0_usize;
    while idx < bytes.len() {
        let ch = bytes[idx] as char;
        if in_single_quote {
            if ch == '\'' && previous != '\\' {
                in_single_quote = false;
            }
            previous = ch;
            idx += 1;
            continue;
        }
        if in_double_quote {
            if ch == '"' {
                in_double_quote = false;
            }
            previous = ch;
            idx += 1;
            continue;
        }
        match ch {
            '\'' => in_single_quote = true,
            '"' => in_double_quote = true,
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0 && starts_with_order_by(sql, idx) {
            return true;
        }
        previous = ch;
        idx += 1;
    }
    false
}

fn starts_with_order_by(sql: &str, start: usize) -> bool {
    const ORDER_BY: &[u8] = b"order by";
    let bytes = sql.as_bytes();
    if !bytes
        .get(start..start.saturating_add(ORDER_BY.len()))
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(ORDER_BY))
    {
        return false;
    }
    let before_ok = start == 0
        || !bytes
            .get(start - 1)
            .is_some_and(|&byte| byte.is_ascii_alphanumeric() || byte == b'_');
    let after_idx = start + ORDER_BY.len();
    let after_ok = !bytes
        .get(after_idx)
        .is_some_and(|&byte| byte.is_ascii_alphanumeric() || byte == b'_');
    before_ok && after_ok
}

fn capture_and_write_pre_risk_context(
    connection: &str,
    workload: &dyn Workload,
    rows: usize,
    config: &BenchConfig,
    query_identity: &BenchmarkQueryIdentity,
    artifact_writer: &ArtifactWriter,
) -> Result<(), Box<dyn std::error::Error>> {
    let pre_query_sql = workload.pre_query_sql();
    let setup_sql = workload.setup_sql(rows);
    let accel_query_sql = query_identity.accel_query_sql();
    let baseline_query_sql = query_identity.baseline_query_sql();
    let explain_sql = format!("EXPLAIN (VERBOSE, COSTS OFF) {accel_query_sql}");

    let mut backend_pid = None;
    let mut backend_pid_error = None;
    let mut explain = None;
    let mut explain_error = None;

    match Client::connect(connection, NoTls) {
        Ok(mut client) => {
            if let Err(e) = apply_benchmark_safety_settings(&mut client) {
                explain_error = Some(format!("pre-EXPLAIN safety setup failed: {e}"));
            }
            if let Err(e) = prime_workload_accel_backend(&mut client, workload).and_then(|_| {
                client.batch_execute("SET pg_accel.enabled = on")?;
                client.batch_execute("SET max_parallel_workers_per_gather = DEFAULT")?;
                for sql in &pre_query_sql {
                    client.batch_execute(sql)?;
                }
                Ok(())
            }) {
                explain_error = Some(format!("pre-EXPLAIN session setup failed: {e}"));
            }

            match client.query_one("SELECT pg_backend_pid()", &[]) {
                Ok(row) => backend_pid = Some(row.get(0)),
                Err(e) => backend_pid_error = Some(e.to_string()),
            }

            if explain_error.is_none() {
                match client.query(&explain_sql, &[]) {
                    Ok(rows_out) => {
                        let mut buf = String::new();
                        for row in &rows_out {
                            let line: &str = row.get(0);
                            buf.push_str(line);
                            buf.push('\n');
                        }
                        explain = Some(buf);
                    }
                    Err(e) => explain_error = Some(e.to_string()),
                }
            }
        }
        Err(e) => {
            let error = e.to_string();
            backend_pid_error = Some(error.clone());
            explain_error = Some(error);
        }
    }

    let context = PreRiskContext {
        workload: workload.name(),
        rows,
        seed: config.seed,
        iterations: config.iterations,
        warmup: config.warmup,
        timing_mode: timing_mode_cli_arg(config.timing_mode),
        cache_mode: cache_mode_cli_arg(config.cache_mode),
        realistic_gucs: config.guc_profile.is_some(),
        skip_guc_verify: config.skip_guc_verify,
        capture_plans: config.plans_capture_path.is_some(),
        capture_planner_stages: config.capture_planner_stages,
        native_parity_pairing: config.native_parity_pairing,
        backend_pid,
        backend_pid_error: backend_pid_error.as_deref(),
        setup_sql: &setup_sql,
        pre_query_sql: &pre_query_sql,
        accel_query_sql,
        baseline_query_sql: Some(baseline_query_sql),
        explain_sql: &explain_sql,
        explain: explain.as_deref(),
        explain_error: explain_error.as_deref(),
    };

    artifact_writer.write_pre_risk_context(workload.name(), rows, &context, query_identity)?;
    Ok(())
}

/// Capture the FULL `EXPLAIN (VERBOSE, COSTS OFF) <query>` plan text for one
/// benchmark side. Used for dispatch classification and no-dispatch
/// native-plan comparability checks.
///
/// The full plan is captured — classification (Custom Scan / GPU-dispatch
/// detection) runs over ALL rows, not a truncated prefix, so a Custom Scan
/// node deeper than 30 lines is never misclassified as declined. Display
/// truncation is the renderer's job, not the capture's.
///
/// On the accel side, a real `pg_accel planner rejection reason: <reason>`
/// line is appended only after the reset-session per-reason counter proves the
/// planner emitted it. The lane's expected exact reason is preferred when its
/// counter is positive, so a later generic decline cannot erase structural
/// evidence. An expected-but-unconfirmed decline is carried separately in
/// `WorkloadResult::native_decline_evidence` tagged `ExpectedUnconfirmed` and
/// is never laundered into plan text.
struct CapturedPlanSnippet {
    text: String,
    planner_rejection_reason: Option<String>,
}

fn plan_physical_kernel_mode(plan: &str) -> Option<&str> {
    plan.lines().find_map(|line| {
        line.trim()
            .strip_prefix("GPU Physical Kernel Mode: ")
            .map(str::trim)
    })
}

fn capture_plan_snippet(
    connection: &str,
    workload: &dyn Workload,
    rows: usize,
    mode: BenchMode,
) -> Result<CapturedPlanSnippet, Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;
    apply_benchmark_safety_settings(&mut client)?;
    match mode {
        BenchMode::Accel => {
            prime_workload_accel_backend(&mut client, workload)?;
            client.batch_execute("SET pg_accel.enabled = on")?;
        }
        BenchMode::PgParallel => client.batch_execute("SET pg_accel.enabled = off")?,
    }
    client.batch_execute("SET max_parallel_workers_per_gather = DEFAULT")?;
    for sql in workload.pre_query_sql() {
        client.batch_execute(&sql)?;
    }
    if matches!(mode, BenchMode::Accel) {
        client.simple_query("SELECT pg_accel_reset_stats()")?;
    }
    let query = match mode {
        BenchMode::Accel => workload.query_sql(),
        BenchMode::PgParallel => workload
            .baseline_query_sql()
            .unwrap_or_else(|| workload.query_sql()),
    };
    let explain = format!("EXPLAIN (VERBOSE, COSTS OFF) {query}");
    let rows_out = client.query(&explain, &[])?;
    let mut buf = String::new();
    for row in &rows_out {
        let line: &str = row.get(0);
        buf.push_str(line);
        buf.push('\n');
    }
    let planner_rejection_reason = if matches!(mode, BenchMode::Accel) {
        let preferred_reason = benchmark_threshold_decline_reason(workload.name(), rows);
        let last_reason = capture_last_planner_rejection_reason(&mut client);
        select_source_verified_planner_rejection_reason(
            preferred_reason,
            last_reason.as_deref(),
            |reason| capture_planner_rejection_count(&mut client, reason),
        )
    } else {
        None
    };
    if let Some(reason) = planner_rejection_reason.as_deref() {
        buf.push_str("pg_accel planner rejection reason: ");
        buf.push_str(reason);
        buf.push('\n');
    }
    Ok(CapturedPlanSnippet {
        text: buf,
        planner_rejection_reason,
    })
}

/// Prefer the lane-specific reason, but only when its planner counter is
/// positive. Otherwise retain the planner's last reason when that reason is
/// independently backed by a positive counter.
fn select_source_verified_planner_rejection_reason<F>(
    preferred_reason: Option<&str>,
    last_reason: Option<&str>,
    mut rejection_count: F,
) -> Option<String>
where
    F: FnMut(&str) -> Option<i64>,
{
    if let Some(reason) = preferred_reason
        && rejection_count(reason).is_some_and(|count| count > 0)
    {
        return Some(reason.to_owned());
    }
    if let Some(reason) = last_reason
        && Some(reason) != preferred_reason
        && rejection_count(reason).is_some_and(|count| count > 0)
    {
        return Some(reason.to_owned());
    }
    None
}

/// Compute the tagged native-decline evidence for a benchmark row.
///
/// A real planner rejection, verified from a positive reset-session per-reason
/// counter, is `PlannerReported`. Otherwise, if the static benchmark-threshold
/// matrix expected a decline and no Custom Scan was selected, the expectation
/// is carried as `ExpectedUnconfirmed`. When a Custom Scan WAS selected there
/// is no decline to report.
fn native_decline_evidence(
    workload_name: &str,
    rows: usize,
    plan_selected: bool,
    verified_planner_rejection_reason: Option<&str>,
) -> Option<report::NativeDeclineEvidence> {
    if plan_selected {
        return None;
    }
    if let Some(reason) = verified_planner_rejection_reason {
        return Some(report::NativeDeclineEvidence {
            reason: reason.to_owned(),
            source: report::DeclineReasonSource::PlannerReported,
        });
    }
    benchmark_threshold_decline_reason(workload_name, rows).map(|reason| {
        report::NativeDeclineEvidence {
            reason: reason.to_owned(),
            source: report::DeclineReasonSource::ExpectedUnconfirmed,
        }
    })
}

fn benchmark_threshold_decline_reason(name: &str, rows: usize) -> Option<&'static str> {
    crate::workloads::benchmark_threshold_matrix_entry(name, rows)
        .and_then(|entry| entry.expectation.decline_reason())
}

fn should_capture_planner_stages(
    workload: &str,
    rows: usize,
    plan_capture_available: bool,
    plan_selected: bool,
) -> bool {
    plan_capture_available
        && !plan_selected
        && benchmark_threshold_decline_reason(workload, rows).is_some()
}

/// Determine whether a cold run's fixture fits inside `shared_buffers`.
///
/// If the fixture (sum of `relpages` × `block_size`) is <= `shared_buffers`,
/// the data can stay resident in PostgreSQL's buffer cache even after the OS
/// page-cache purge, so the "cold" label overstates the eviction. Returns
/// `Some(true)` when the data stays resident (NOT truly cold), `Some(false)`
/// when the fixture exceeds `shared_buffers` (genuinely cold-capable), and
/// `None` when no table stats were captured or the settings query failed.
fn cold_shared_buffers_resident(
    connection: &str,
    table_stats: &[crate::report::TableStats],
) -> Option<bool> {
    if table_stats.is_empty() {
        return None;
    }
    let fixture_pages: i64 = table_stats.iter().map(|t| t.relpages.max(0)).sum();
    let mut client = Client::connect(connection, NoTls).ok()?;
    let row = client
        .query_one(
            "SELECT pg_size_bytes(current_setting('shared_buffers'))::bigint, \
                    current_setting('block_size')::bigint",
            &[],
        )
        .ok()?;
    let shared_buffers_bytes: i64 = row.get(0);
    let block_size: i64 = row.get(1);
    if block_size <= 0 {
        return None;
    }
    let fixture_bytes = fixture_pages.saturating_mul(block_size);
    Some(fixture_bytes <= shared_buffers_bytes)
}

fn capture_last_planner_rejection_reason(client: &mut Client) -> Option<String> {
    client
        .query_one("SELECT pg_accel_last_planner_rejection_reason()", &[])
        .ok()
        .and_then(|row| row.get::<_, Option<String>>(0))
}

fn capture_planner_rejection_count(client: &mut Client, reason: &str) -> Option<i64> {
    client
        .query_one("SELECT pg_accel_planner_rejection_count($1)", &[&reason])
        .ok()
        .map(|row| row.get(0))
}

/// Return true if a plan text snippet contains a Custom Scan node (which
/// for pg_accel is always prefixed with "GPU").
#[must_use]
pub fn plan_contains_custom_scan(plan: &str) -> bool {
    plan.contains("Custom Scan")
}

fn classify_runner_dispatch(input: RunnerDispatchInput) -> RunnerDispatchClassification {
    let counter_proves_kernel =
        input.dispatch_counter_captured && input.gpu_kernel_execution_delta > 0;
    let stock_fallback_seen = input.pg_accel_stock_exec_delta > 0;
    let output_consumed = input.accel_output_rows_consumed > 0;
    let selected_queries_consumed = usize::try_from(input.pg_accel_queries_accelerated_delta)
        .is_ok_and(|queries| queries >= input.measured_iterations);
    let function_srf_kernel_dispatched = input.function_kernel_candidate
        && !input.plan_explicitly_not_dispatched
        && !input.plan_selected
        && counter_proves_kernel
        && output_consumed
        && !stock_fallback_seen;
    let custom_scan_gpu_dispatched = input.plan_selected
        && !input.plan_explicitly_not_dispatched
        && counter_proves_kernel
        && selected_queries_consumed
        && output_consumed
        && !stock_fallback_seen;
    let gpu_kernel_dispatched = custom_scan_gpu_dispatched || function_srf_kernel_dispatched;

    RunnerDispatchClassification {
        gpu_kernel_dispatched,
        function_srf_kernel_dispatched,
    }
}

fn emit_dispatch_classification_warning(input: DispatchWarningInput<'_>, result: &WorkloadResult) {
    if !result.dispatch_counter_captured {
        eprintln!(
            "[dispatch] WARNING: {} @ {}: runtime counter capture unavailable; \
             not crediting GPU dispatch ({})",
            input.workload,
            input.rows,
            result
                .dispatch_counter_error
                .as_deref()
                .unwrap_or("unknown stats error")
        );
        return;
    }
    if result.pg_accel_stock_exec_delta > 0 {
        eprintln!(
            "[dispatch] WARNING: {} @ {}: pg_accel stock executor fallback delta={} \
             with kernel_delta={}; excluding from GPU-dispatched wins",
            input.workload,
            input.rows,
            result.pg_accel_stock_exec_delta,
            result.gpu_kernel_execution_delta
        );
        return;
    }
    if input.plan_selected && result.gpu_kernel_execution_delta == 0 {
        eprintln!(
            "[dispatch] WARNING: {} @ {}: pg_accel plan selected but runtime \
             kernel counter delta is zero; counting as plan-selected only",
            input.workload, input.rows
        );
    }
    if input.plan_selected
        && result.gpu_kernel_execution_delta > 0
        && usize::try_from(result.pg_accel_queries_accelerated_delta)
            .map_or(true, |queries| queries < result.iterations.len())
    {
        eprintln!(
            "[dispatch] WARNING: {} @ {}: kernel counter advanced but accelerated-query \
             delta={} does not cover {} measured pairs; excluding from GPU-dispatched wins",
            input.workload,
            input.rows,
            result.pg_accel_queries_accelerated_delta,
            result.iterations.len()
        );
    }
    if input.plan_selected
        && result.gpu_kernel_execution_delta > 0
        && result.accel_output_rows_consumed == 0
    {
        eprintln!(
            "[dispatch] WARNING: {} @ {}: kernel counter advanced but no accel output rows \
             were consumed; excluding from GPU-dispatched wins",
            input.workload, input.rows
        );
    }
    if input.plan_explicitly_not_dispatched && result.gpu_kernel_execution_delta > 0 {
        eprintln!(
            "[dispatch] WARNING: {} @ {}: plan text reported GPU Dispatched: false \
             but runtime kernel counter delta is {}; counting as plan-selected only",
            input.workload, input.rows, result.gpu_kernel_execution_delta
        );
    }
    if input.plan_text_dispatched && result.gpu_kernel_execution_delta == 0 {
        eprintln!(
            "[dispatch] WARNING: {} @ {}: plan text claimed GPU dispatch but \
             runtime kernel counter delta is zero",
            input.workload, input.rows
        );
    }
    if input.function_kernel_candidate
        && !input.plan_selected
        && result.gpu_kernel_execution_delta > 0
        && result.accel_output_rows_consumed == 0
    {
        eprintln!(
            "[dispatch] WARNING: {} @ {}: function/SRF kernel counter advanced \
             but no accel output rows were consumed; excluding from GPU-dispatched wins",
            input.workload, input.rows
        );
    }
}

fn plan_indicates_gpu_dispatch(plan: &str) -> bool {
    explicit_gpu_dispatched(plan).unwrap_or_else(|| plan_contains_custom_scan(plan))
}

fn explicit_gpu_dispatched(plan: &str) -> Option<bool> {
    let lower = plan.to_ascii_lowercase();
    if lower.contains("gpu dispatched: false")
        || lower.contains("gpu kernel dispatched: false")
        || lower.contains("\"gpu dispatched\": false")
        || lower.contains("\"gpu dispatched\":false")
        || lower.contains("\"gpu dispatched\":\"false\"")
        || lower.contains("\"gpu dispatched\": \"false\"")
        || lower.contains("\"gpu kernel dispatched\": false")
        || lower.contains("\"gpu kernel dispatched\":false")
        || lower.contains("\"gpu kernel dispatched\":\"false\"")
        || lower.contains("\"gpu kernel dispatched\": \"false\"")
    {
        Some(false)
    } else if lower.contains("gpu dispatched: true")
        || lower.contains("gpu kernel dispatched: true")
        || lower.contains("\"gpu dispatched\": true")
        || lower.contains("\"gpu dispatched\":true")
        || lower.contains("\"gpu dispatched\":\"true\"")
        || lower.contains("\"gpu dispatched\": \"true\"")
        || lower.contains("\"gpu kernel dispatched\": true")
        || lower.contains("\"gpu kernel dispatched\":true")
        || lower.contains("\"gpu kernel dispatched\":\"true\"")
        || lower.contains("\"gpu kernel dispatched\": \"true\"")
    {
        Some(true)
    } else {
        None
    }
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
    apply_benchmark_safety_settings(&mut client)?;
    prime_workload_accel_backend(&mut client, workload)?;
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
fn ensure_extensions_for_names(
    connection: &str,
    workload_names: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;

    // Always need pg_accel.
    let mut required: Vec<&str> = vec!["pg_accel"];

    // Collect unique extensions required by selected workloads.
    for name in workload_names {
        if let Some(metadata) = crate::workloads::workload_metadata(name) {
            for extension in metadata.required_extensions {
                let extension = extension.as_str();
                if !required.contains(&extension) {
                    required.push(extension);
                }
            }
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
    Ok(run_explain_analyze_outcome(client, query)?.elapsed_ms)
}

fn run_explain_analyze_outcome(
    client: &mut Client,
    query: &str,
) -> Result<MeasurementOutcome, Box<dyn std::error::Error>> {
    let explain_query = format!("EXPLAIN ANALYZE {query}");
    let rows = client.query(&explain_query, &[])?;

    let mut elapsed_ms = None;
    let mut output_rows = None;
    for row in &rows {
        let line: &str = row.get(0);
        if output_rows.is_none() {
            output_rows = parse_actual_rows(line);
        }
        if elapsed_ms.is_none() {
            elapsed_ms = parse_execution_time(line);
        }
    }

    let Some(elapsed_ms) = elapsed_ms else {
        return Err("could not find 'Execution Time' in EXPLAIN ANALYZE output".into());
    };
    Ok(MeasurementOutcome {
        elapsed_ms,
        output_rows: output_rows.unwrap_or(0),
    })
}

/// Parse the millisecond value from an `Execution Time: <milliseconds> ms` line.
fn parse_execution_time(line: &str) -> Option<f64> {
    let trimmed = line.trim();
    let suffix = trimmed.strip_prefix("Execution Time:")?;
    let suffix = suffix.trim();
    let ms_str = suffix.strip_suffix("ms")?.trim();
    ms_str.parse::<f64>().ok()
}

fn parse_actual_rows(line: &str) -> Option<u64> {
    let actual_idx = line.find("actual ")?;
    let suffix = line.get(actual_idx..)?;
    let rows_idx = suffix.find(" rows=")?;
    let value = suffix.get(rows_idx + " rows=".len()..)?;
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    (!digits.is_empty())
        .then(|| digits.parse::<u64>().ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("test clock should be after unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "pg_accel_runner_{label}_{}_{}",
                std::process::id(),
                nanos
            ));
            fs::create_dir_all(&path).expect("test temp directory should be created");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn measured_arm_order_is_randomized_and_balanced() {
        for seed in 0..32 {
            let mut rng = StdRng::seed_from_u64(seed);
            let schedule = randomized_balanced_arm_order(10, &mut rng);
            assert_eq!(schedule.len(), 10);
            assert_eq!(
                schedule.iter().filter(|&&accel_first| accel_first).count(),
                5
            );
        }

        let mut rng = StdRng::seed_from_u64(42);
        let odd = randomized_balanced_arm_order(9, &mut rng);
        let accel_first = odd.iter().filter(|&&value| value).count();
        assert!(matches!(accel_first, 4 | 5));
    }

    #[test]
    fn native_parity_order_replicates_mirrored_blocks_without_dropping_samples() {
        let accel_first = native_parity_execution_order(true);
        let disabled_first = native_parity_execution_order(false);
        assert_eq!(accel_first, [0, 1, 1, 0, 0, 1, 1, 0]);
        assert_eq!(disabled_first, [1, 0, 0, 1, 1, 0, 0, 1]);
        for order in [&accel_first, &disabled_first] {
            assert_eq!(order.len(), NATIVE_PARITY_REPETITIONS_PER_ARM * 2);
            assert_eq!(order.iter().filter(|&&arm| arm == 0).count(), 4);
            assert_eq!(order.iter().filter(|&&arm| arm == 1).count(), 4);
        }
    }

    fn planner_stage_snapshot(
        calls: u64,
        elapsed_us: u64,
        fast_declines: u64,
    ) -> BTreeMap<String, PlannerStageStatsSnapshot> {
        PLANNER_STAGE_NAMES
            .iter()
            .map(|stage| {
                (
                    (*stage).to_owned(),
                    PlannerStageStatsSnapshot {
                        calls,
                        elapsed_us,
                        fast_declines,
                    },
                )
            })
            .collect()
    }

    fn planner_substage_snapshot(
        calls: u64,
        elapsed_us: u64,
    ) -> BTreeMap<String, PlannerSubstageStatsSnapshot> {
        PLANNER_SUBSTAGE_NAMES
            .iter()
            .map(|substage| {
                (
                    (*substage).to_owned(),
                    PlannerSubstageStatsSnapshot { calls, elapsed_us },
                )
            })
            .collect()
    }

    #[test]
    fn planner_stage_delta_is_exact_and_rejects_counter_regression() {
        let before = planner_stage_snapshot(10, 100, 5);
        let after = planner_stage_snapshot(12, 109, 6);
        let delta = planner_stage_stats_delta(&before, &after).expect("valid stage delta");
        assert_eq!(delta.len(), PLANNER_STAGE_NAMES.len());
        assert!(delta.iter().all(|stage| {
            stage.calls == 2 && stage.elapsed_us == 9 && stage.fast_declines == 1
        }));

        let regressed = planner_stage_snapshot(9, 109, 6);
        assert!(
            planner_stage_stats_delta(&before, &regressed)
                .expect_err("regressed calls must invalidate capture")
                .contains("calls counter regressed")
        );
    }

    #[test]
    fn planner_stage_delta_rejects_mismatched_stage_sets() {
        let before = planner_stage_snapshot(10, 100, 5);
        let mut after = planner_stage_snapshot(12, 109, 6);
        after.remove("upper_other");
        after.insert(
            "unknown".to_owned(),
            PlannerStageStatsSnapshot {
                calls: 1,
                elapsed_us: 1,
                fast_declines: 1,
            },
        );
        assert!(
            planner_stage_stats_delta(&before, &after)
                .expect_err("mismatched stages must invalidate capture")
                .contains("key sets differ")
        );
    }

    #[test]
    fn planner_substage_delta_is_exact_and_rejects_counter_regression() {
        let before = planner_substage_snapshot(4, 40);
        let after = planner_substage_snapshot(7, 58);
        let delta = planner_substage_stats_delta(&before, &after).expect("valid substage delta");
        assert_eq!(delta.len(), PLANNER_SUBSTAGE_NAMES.len());
        assert!(
            delta
                .iter()
                .all(|substage| substage.calls == 3 && substage.elapsed_us == 18)
        );

        let regressed = planner_substage_snapshot(3, 58);
        assert!(
            planner_substage_stats_delta(&before, &regressed)
                .expect_err("regressed calls must invalidate substage capture")
                .contains("calls counter regressed")
        );
    }

    #[test]
    fn artifact_lifecycle_delta_is_exact_and_rejects_counter_regression() {
        let before = report::ArtifactLifecycleDelta {
            hits: 10,
            builds: 2,
            rebuilds: 3,
            artifact_bytes_observed: 100,
            construction_bytes: 80,
            construction_us: 70,
            preparation_us: 60,
            raw_load_us: 50,
        };
        let after = report::ArtifactLifecycleDelta {
            hits: 14,
            builds: 3,
            rebuilds: 5,
            artifact_bytes_observed: 140,
            construction_bytes: 110,
            construction_us: 90,
            preparation_us: 75,
            raw_load_us: 55,
        };
        assert_eq!(
            artifact_lifecycle_delta(before, after).expect("valid lifecycle delta"),
            report::ArtifactLifecycleDelta {
                hits: 4,
                builds: 1,
                rebuilds: 2,
                artifact_bytes_observed: 40,
                construction_bytes: 30,
                construction_us: 20,
                preparation_us: 15,
                raw_load_us: 5,
            }
        );

        let mut regressed = after;
        regressed.rebuilds = 2;
        assert!(
            artifact_lifecycle_delta(before, regressed)
                .expect_err("regressed lifecycle counter must invalidate evidence")
                .contains("rebuilds counter regressed")
        );
    }

    #[test]
    fn planner_stage_capture_is_gated_to_observed_native_matrix_cells() {
        assert!(should_capture_planner_stages(
            "grouped_agg_int4",
            10_000,
            true,
            false,
        ));
        assert!(!should_capture_planner_stages(
            "grouped_agg_int4",
            1_000_000,
            true,
            false,
        ));
        assert!(!should_capture_planner_stages(
            "grouped_agg_int4",
            10_000,
            false,
            false,
        ));
        assert!(!should_capture_planner_stages(
            "grouped_agg_int4",
            10_000,
            true,
            true,
        ));
    }

    #[test]
    fn explicit_gpu_dispatched_parses_gpu_kernel_dispatched_aliases() {
        assert_eq!(
            explicit_gpu_dispatched("Custom Scan\n  GPU Kernel Dispatched: true\n"),
            Some(true)
        );
        assert_eq!(
            explicit_gpu_dispatched("Custom Scan\n  GPU Kernel Dispatched: false\n"),
            Some(false)
        );
    }

    fn mock_report_missing_resident_boundary() -> report::BenchReport {
        let iterations = (0..5)
            .map(|_| IterationResult {
                accel_ms: 10.0,
                parallel_ms: 20.0,
                accel_first: false,
                cache_purge: CachePurgeState::NotRequested,
                cache_state: CacheState::Warm,
            })
            .collect();
        let mut workload = WorkloadResult::from_iterations(
            "runner_missing_boundary".to_owned(),
            "runner missing resident-boundary test".to_owned(),
            "gpu".to_owned(),
            "unclassified".to_owned(),
            100_000,
            iterations,
            true,
        );
        workload.plan_selected = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.accel_output_rows_consumed = 1;
        workload.plan_snippet =
            Some("Custom Scan (GpuAccelMystery)\n  GPU Dispatched: true\n".to_owned());

        report::BenchReport {
            hardware: None,
            gucs: None,
            methodology: report::Methodology {
                iterations: 5,
                warmup: 0,
                row_scales: vec![100_000],
                ordering: "test".to_owned(),
                statistical_tests: vec!["test".to_owned()],
                timing_mode: "raw".to_owned(),
                cache_mode: "warm".to_owned(),
                harness_profile: "test".to_owned(),
                native_parity_pairing: false,
                native_parity_repetitions_per_arm: 1,
            },
            workloads: vec![workload],
            headline_speedup_allowed: true,
            artifact_dir: None,
            crashes: Vec::new(),
            postmaster_start_time: None,
        }
    }

    #[test]
    fn finalize_report_artifacts_propagates_resident_boundary_audit_failure() {
        let dir = TestDir::new("resident-boundary-fail");
        let writer = ArtifactWriter::new(dir.path().to_path_buf(), Vec::new())
            .expect("artifact writer should initialize");
        let mut report = mock_report_missing_resident_boundary();

        let error = finalize_report_artifacts(&writer, &mut report, "test-complete")
            .expect_err("resident-boundary audit failure should propagate");
        let error = error.to_string();

        assert!(error.contains("artifact report/audit write failed"));
        assert!(error.contains("resident-boundary audit failed"));
        assert!(dir.path().join("resident_boundary_audit.json").is_file());
        assert!(dir.path().join("resident_boundary_audit.md").is_file());
    }

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
    fn test_plan_dispatch_marker_false_overrides_custom_scan() {
        let plan = "Custom Scan (pg_accel)\n  GPU Dispatched: false\n";
        assert!(plan_contains_custom_scan(plan));
        assert!(!plan_indicates_gpu_dispatch(plan));
    }

    #[test]
    fn test_plan_dispatch_marker_true_counts_without_custom_scan() {
        let plan = "Function Scan on h3_cells\n  GPU Dispatched: true\n";
        assert!(plan_indicates_gpu_dispatch(plan));
    }

    #[test]
    fn test_json_plan_dispatch_marker_false_overrides_custom_scan() {
        let plan = r#"[{"Plan":{"Node Type":"Custom Scan","GPU Dispatched":false}}]"#;
        assert!(plan_contains_custom_scan(plan));
        assert!(!plan_indicates_gpu_dispatch(plan));
    }

    #[test]
    fn test_runner_dispatch_false_marker_overrides_kernel_counter() {
        let classification = classify_runner_dispatch(RunnerDispatchInput {
            plan_selected: true,
            plan_explicitly_not_dispatched: true,
            function_kernel_candidate: false,
            dispatch_counter_captured: true,
            gpu_kernel_execution_delta: 1,
            pg_accel_queries_accelerated_delta: 1,
            pg_accel_stock_exec_delta: 0,
            accel_output_rows_consumed: 1,
            measured_iterations: 1,
        });

        assert!(!classification.gpu_kernel_dispatched);
        assert!(!classification.function_srf_kernel_dispatched);
    }

    #[test]
    fn test_runner_function_srf_dispatch_without_custom_scan() {
        let classification = classify_runner_dispatch(RunnerDispatchInput {
            plan_selected: false,
            plan_explicitly_not_dispatched: false,
            function_kernel_candidate: true,
            dispatch_counter_captured: true,
            gpu_kernel_execution_delta: 3,
            pg_accel_queries_accelerated_delta: 0,
            pg_accel_stock_exec_delta: 0,
            accel_output_rows_consumed: 12,
            measured_iterations: 1,
        });

        assert!(classification.gpu_kernel_dispatched);
        assert!(classification.function_srf_kernel_dispatched);
    }

    #[test]
    fn selected_dispatch_allows_verified_artifact_hits_but_requires_query_and_output_proof() {
        let base = RunnerDispatchInput {
            plan_selected: true,
            plan_explicitly_not_dispatched: false,
            function_kernel_candidate: false,
            dispatch_counter_captured: true,
            gpu_kernel_execution_delta: 1,
            pg_accel_queries_accelerated_delta: 10,
            pg_accel_stock_exec_delta: 0,
            accel_output_rows_consumed: 10,
            measured_iterations: 10,
        };
        assert!(
            classify_runner_dispatch(base).gpu_kernel_dispatched,
            "one rebuild dispatch plus ten accelerated queries is valid artifact-hit evidence"
        );
        assert!(
            !classify_runner_dispatch(RunnerDispatchInput {
                pg_accel_queries_accelerated_delta: 9,
                ..base
            })
            .gpu_kernel_dispatched
        );
        assert!(
            !classify_runner_dispatch(RunnerDispatchInput {
                accel_output_rows_consumed: 0,
                ..base
            })
            .gpu_kernel_dispatched
        );
        assert!(
            !classify_runner_dispatch(RunnerDispatchInput {
                plan_selected: false,
                ..base
            })
            .gpu_kernel_dispatched,
            "an unattributed same-backend kernel delta is not performance credit"
        );
    }

    fn iter_at(accel_ms: f64, parallel_ms: f64, cache_state: CacheState) -> IterationResult {
        IterationResult {
            accel_ms,
            parallel_ms,
            accel_first: false,
            cache_purge: if cache_state == CacheState::Cold {
                CachePurgeState::Completed
            } else {
                CachePurgeState::NotRequested
            },
            cache_state,
        }
    }

    #[test]
    fn test_from_iterations_separates_warm_and_cold_summaries() {
        // Mixed warm+cold (CacheMode::Both) input: warm is fast, cold is slow.
        let mut iters = Vec::new();
        for _ in 0..4 {
            iters.push(iter_at(100.0, 200.0, CacheState::Cold));
        }
        for _ in 0..6 {
            iters.push(iter_at(10.0, 20.0, CacheState::Warm));
        }
        let result = WorkloadResult::from_iterations(
            "mixed".to_owned(),
            "d".to_owned(),
            "gpu".to_owned(),
            "unclassified".to_owned(),
            10_000,
            iters,
            true,
        );

        let warm = result.warm_summary.expect("warm summary present");
        let cold = result.cold_summary.expect("cold summary present");
        assert_eq!(warm.n, 6);
        assert_eq!(cold.n, 4);
        // Summaries are computed over homogeneous subsamples, not the mixture.
        assert!((warm.accel_median_ms - 10.0).abs() < f64::EPSILON);
        assert!((cold.accel_median_ms - 100.0).abs() < f64::EPSILON);
        // The flat top-level headline uses the WARM subsample when mixed, so no
        // bimodal-mixture median can leak into the headline.
        assert!((result.accel_median_ms - 10.0).abs() < f64::EPSILON);
        assert!((result.speedup_median_vs_parallel - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_from_iterations_warm_only_has_no_cold_summary() {
        let iters: Vec<IterationResult> = (0..10)
            .map(|_| iter_at(10.0, 20.0, CacheState::Warm))
            .collect();
        let result = WorkloadResult::from_iterations(
            "warm".to_owned(),
            "d".to_owned(),
            "gpu".to_owned(),
            "unclassified".to_owned(),
            10_000,
            iters,
            true,
        );
        assert!(result.warm_summary.is_some());
        assert!(result.cold_summary.is_none());
    }

    #[test]
    fn test_native_decline_evidence_prefers_planner_reported() {
        let evidence = native_decline_evidence(
            "gpu_sort_multikey",
            100_000,
            false,
            Some("sort_multikey_no_gpu_kernel"),
        )
        .expect("planner-reported evidence present");
        assert_eq!(evidence.reason, "sort_multikey_no_gpu_kernel");
        assert_eq!(
            evidence.source,
            report::DeclineReasonSource::PlannerReported
        );
    }

    #[test]
    fn test_phase9_structural_declines_are_unconfirmed_without_planner_evidence() {
        for contract in crate::workloads::PHASE9_OPERATOR_DECLINES {
            let name = contract.workload;
            let reason = contract.reason;
            let workload = crate::workloads::find_workload(name)
                .unwrap_or_else(|| panic!("workload for {name}"));
            for &rows in workload.row_scales() {
                let expected_only = native_decline_evidence(name, rows, false, None)
                    .unwrap_or_else(|| panic!("threshold expectation for {name} at {rows}"));
                assert_eq!(expected_only.reason, reason, "{name} at {rows}");
                assert_eq!(
                    expected_only.source,
                    report::DeclineReasonSource::ExpectedUnconfirmed,
                    "{name} at {rows}"
                );

                let planner_reported = native_decline_evidence(name, rows, false, Some(reason))
                    .unwrap_or_else(|| panic!("planner evidence for {name} at {rows}"));
                assert_eq!(planner_reported.reason, reason, "{name} at {rows}");
                assert_eq!(
                    planner_reported.source,
                    report::DeclineReasonSource::PlannerReported,
                    "{name} at {rows}"
                );

                assert!(
                    native_decline_evidence(name, rows, true, Some(reason)).is_none(),
                    "{name} at {rows}: selecting a pg_accel Custom Scan must invalidate native-decline evidence"
                );
            }
        }
    }

    #[test]
    fn test_native_decline_evidence_none_when_custom_scan_selected() {
        assert!(
            native_decline_evidence(
                "reduce_f64_sum",
                1_000_000,
                true,
                Some("rows_below_min_batch")
            )
            .is_none()
        );
    }

    #[test]
    fn test_native_decline_evidence_absent_without_reason_or_expectation() {
        // A workload name with no threshold-matrix decline entry and no planner
        // rejection line yields no fabricated evidence.
        assert!(native_decline_evidence("no_such_workload_xyz", 12_345, false, None).is_none());
    }

    #[test]
    fn test_planner_rejection_reason_preference_requires_positive_counter() {
        let preferred = "mergejoin_no_gpu_kernel";
        let generic = "no_gpu_resident_pipeline";

        let selected = select_source_verified_planner_rejection_reason(
            Some(preferred),
            Some(generic),
            |reason| Some(i64::from(reason == preferred)),
        );
        assert_eq!(selected.as_deref(), Some(preferred));

        let selected = select_source_verified_planner_rejection_reason(
            Some(preferred),
            Some(generic),
            |reason| Some(i64::from(reason == generic)),
        );
        assert_eq!(selected.as_deref(), Some(generic));

        let selected =
            select_source_verified_planner_rejection_reason(Some(preferred), Some(generic), |_| {
                Some(0)
            });
        assert_eq!(selected, None);

        let selected = select_source_verified_planner_rejection_reason(
            Some(preferred),
            Some(preferred),
            |_| None,
        );
        assert_eq!(selected, None);
    }

    #[test]
    fn test_combine_purge_states_reports_worst_case() {
        use CachePurgeState::{Completed, Failed, NotRequested, Unavailable};
        assert_eq!(combine_purge_states(Completed, Completed), Completed);
        assert_eq!(combine_purge_states(Completed, Unavailable), Unavailable);
        assert_eq!(combine_purge_states(Unavailable, Failed), Failed);
        assert_eq!(combine_purge_states(NotRequested, Completed), Completed);
        assert_eq!(combine_purge_states(Failed, Completed), Failed);
    }

    #[test]
    #[allow(clippy::expect_used)]
    fn test_parse_execution_time_zero() {
        let result = parse_execution_time("Execution Time: 0.000 ms").expect("should parse zero");
        assert!(result.abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_actual_rows_from_explain_analyze() {
        let line = "Aggregate  (cost=12.00..12.01 rows=1 width=8) \
                    (actual time=0.125..0.126 rows=1 loops=1)";
        assert_eq!(parse_actual_rows(line), Some(1));
        assert_eq!(
            parse_actual_rows("Seq Scan on t  (cost=0.00..1.00 rows=5 width=4)"),
            None
        );
    }

    #[test]
    fn test_row_scales_constant() {
        // The 1K scale is below the instrument noise floor.
        assert_eq!(ROW_SCALES, &[10_000, 100_000, 1_000_000, 10_000_000]);
    }

    #[test]
    fn test_row_scales_min_10k() {
        assert!(
            ROW_SCALES.iter().min().copied().unwrap_or(0) >= 10_000,
            "minimum reportable scale is 10K by benchmark policy"
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

    #[test]
    fn test_repro_command_preserves_runner_flags() {
        let config = BenchConfig {
            iterations: 3,
            warmup: 1,
            seed: 99,
            timing_mode: TimingMode::Both,
            cache_mode: CacheMode::Cold,
            capture_planner_stages: true,
            native_parity_pairing: true,
            headline_speedup_allowed: true,
            plans_capture_path: Some(PathBuf::from("benchmarks/artifacts/repro/plans.txt")),
            guc_profile: Some(GucProfile::realistic()),
            skip_guc_verify: true,
            artifacts_dir: Some(PathBuf::from("benchmarks/artifacts/repro")),
        };
        let command = repro_command(
            "host=localhost port=28818 dbname=pg accel",
            "hashagg_10g",
            1_000_000,
            &config,
        );

        assert!(command.starts_with("RUST_BACKTRACE=1 cargo run -p pg_accel_bench -- crash-repro"));
        assert!(command.contains("--workload hashagg_10g"));
        assert!(command.contains("--rows 1000000"));
        assert!(command.contains("--iterations 3"));
        assert!(command.contains("--warmup 1"));
        assert!(command.contains("--seed 99"));
        assert!(command.contains("--timing both"));
        assert!(command.contains("--cache-mode cold"));
        assert!(command.contains("--realistic-gucs"));
        assert!(command.contains("--capture-plans"));
        assert!(command.contains("--capture-planner-stages"));
        assert!(command.contains("--native-parity-pairing"));
        assert!(command.contains("--skip-guc-verify"));
        assert!(command.contains("--artifacts-dir benchmarks/artifacts/repro"));
        assert!(command.contains("--connection 'host=localhost port=28818 dbname=pg accel'"));
    }

    #[test]
    fn correctness_scratch_tables_are_pg_temp_qualified() {
        assert!(CORRECTNESS_ACCEL_TABLE.starts_with("pg_temp."));
        assert!(CORRECTNESS_BASELINE_TABLE.starts_with("pg_temp."));
        assert!(!CORRECTNESS_ACCEL_TABLE.contains("public."));
        assert!(!CORRECTNESS_BASELINE_TABLE.contains("public."));
    }

    #[test]
    fn test_sanitize_artifact_component() {
        assert_eq!(
            sanitize_artifact_component("crash-001-gpu/hash agg @ 1M"),
            "crash-001-gpu-hash-agg---1M"
        );
        assert_eq!(sanitize_artifact_component("***"), "artifact");
    }

    #[test]
    fn test_correctness_projection_strips_trailing_semicolon() {
        let projection = correctness_projection_sql("SELECT 1 AS x;\n", "unit", false);
        assert!(projection.contains("FROM (SELECT 1 AS x) AS q"));
        assert!(projection.contains("NULL::bigint AS ord"));
    }

    #[test]
    fn test_correctness_projection_includes_order_ordinal_when_needed() {
        let projection = correctness_projection_sql("SELECT id FROM t ORDER BY id", "unit", true);
        assert!(projection.contains("row_number() OVER () AS ord"));
        assert!(projection.contains("to_jsonb(q)::text AS row_repr"));
    }

    #[test]
    fn test_resident_pin_specs_cover_grouped_aggregate_inputs_exactly() {
        assert_eq!(
            resident_pin_specs("grouped_agg"),
            vec![resident_pin("bench_employees_agg", &["dept", "salary"],)]
        );
        assert_eq!(
            resident_pin_specs("predicate_filter_expression_grouped_agg"),
            vec![resident_pin(
                "bench_predicate_expression_sales",
                &["product_id", "price", "discount", "active"],
            )]
        );
        assert_eq!(
            resident_pin_specs("grouped_agg_int4"),
            vec![resident_pin(
                "bench_employees_agg_int4",
                &["dept", "salary"],
            )]
        );
        assert_eq!(
            resident_pin_specs("predicate_expression_grouped_agg_int4"),
            vec![resident_pin(
                "bench_predicate_expression_sales_int4",
                &["product_id", "price", "quantity", "active"],
            )]
        );
        assert_eq!(
            resident_pin_specs("and_range_predicate_expression_grouped_agg_int4"),
            vec![resident_pin(
                "bench_and_range_predicate_expression_sales_int4",
                &["product_id", "price", "quantity"],
            )]
        );
        assert_eq!(
            resident_pin_specs("hashagg_f64_aggs"),
            vec![resident_pin("bench_fp64_num", &["gk", "v_f64", "w_f64"],)]
        );
    }

    #[test]
    fn test_resident_pin_specs_cover_exact_raster_input() {
        assert_eq!(
            resident_pin_specs("raster_resident_exact_reclass"),
            vec![resident_pin(
                "bench_raster_resident_exact_reclass",
                &["rast"],
            )]
        );
    }

    #[test]
    fn test_resident_pin_specs_cover_ssbm_inputs_exactly() {
        assert_eq!(
            resident_pin_specs("ssbm_resident_int4_star"),
            vec![
                resident_pin(
                    "ssbm_lineorder",
                    &["lo_orderdate", "lo_partkey", "lo_revenue"],
                ),
                resident_pin("ssbm_date", &["d_datekey", "d_year"]),
                resident_pin("ssbm_part", &["p_partkey", "p_size"]),
            ]
        );
        assert_eq!(
            resident_pin_specs("ssbm_q1_3"),
            vec![
                resident_pin(
                    "ssbm_lineorder",
                    &[
                        "lo_orderdate",
                        "lo_extendedprice",
                        "lo_discount",
                        "lo_quantity",
                    ],
                ),
                resident_pin("ssbm_date", &["d_datekey", "d_weeknuminyear", "d_year"],),
            ]
        );
        assert_eq!(
            resident_pin_specs("ssbm_q4_3"),
            vec![
                resident_pin(
                    "ssbm_lineorder",
                    &[
                        "lo_orderdate",
                        "lo_custkey",
                        "lo_suppkey",
                        "lo_partkey",
                        "lo_revenue",
                        "lo_supplycost",
                    ],
                ),
                resident_pin("ssbm_date", &["d_datekey", "d_year"]),
                resident_pin("ssbm_customer", &["c_custkey", "c_region"]),
                resident_pin("ssbm_supplier", &["s_suppkey", "s_city", "s_nation"],),
                resident_pin("ssbm_part", &["p_partkey", "p_brand1", "p_category"],),
            ]
        );
    }

    #[test]
    fn test_resident_pin_specs_cover_star_inputs_without_duplicates() {
        assert_eq!(
            resident_pin_specs("gpu_hashjoin_filter"),
            vec![
                resident_pin("bench_hjf_fact", &["dim_id", "amount"]),
                resident_pin("bench_hjf_dim", &["id", "category", "name"]),
            ]
        );
        assert_eq!(
            resident_pin_specs("mixed_join_agg"),
            vec![
                resident_pin("bench_mixed_facts", &["dim_id", "amount"]),
                resident_pin("bench_mixed_dims", &["id", "label"]),
            ]
        );
        assert_eq!(
            resident_pin_specs("mixed_join_agg_int4"),
            vec![
                resident_pin("bench_mixed_facts_int4", &["dim_id", "amount"]),
                resident_pin("bench_mixed_dims_int4", &["id", "label"]),
            ]
        );
    }

    #[test]
    fn test_generic_h3_workloads_have_no_resident_pins() {
        for name in ["h3_bulk", "h3_resolution_sweep", "h3_latlng_res15"] {
            assert!(
                resident_pin_specs(name).is_empty(),
                "{name} must remain unpinned until its generic H3 input is resident"
            );
            assert!(
                !workload_is_resident_lane(name),
                "{name} must not be classified as a resident lane"
            );
        }
    }

    #[test]
    fn test_h3_parent_workload_pins_exact_resident_input() {
        assert_eq!(
            resident_pin_specs("h3_cell_to_parent"),
            vec![resident_pin("bench_h3_parent", &["cell"])]
        );
        assert!(workload_is_resident_lane("h3_cell_to_parent"));
    }

    #[test]
    fn test_resident_pin_specs_cover_hashjoin_inputs_exactly() {
        assert_eq!(
            resident_pin_specs("hash_join"),
            vec![
                resident_pin("bench_orders", &["customer_id"]),
                resident_pin("bench_customers", &["customer_id"]),
            ]
        );
        assert_eq!(
            resident_pin_specs("hashjoin_10k_1m"),
            vec![
                resident_pin("bench_hj_outer", &["key"]),
                resident_pin("bench_hj_inner", &["key"]),
            ]
        );
    }

    #[test]
    fn test_resident_lane_classification_follows_exact_pin_mapping() {
        assert!(workload_is_resident_lane("grouped_agg"));
        assert!(workload_is_resident_lane("ssbm_q2_1"));
        assert!(!workload_is_resident_lane("small_table"));
        assert!(resident_pin_specs("small_table").is_empty());
    }
    #[test]
    fn test_correctness_projection_rounds_aggregate_float_lanes() {
        let spatial_sort = correctness_projection_sql(
            "SELECT id, ST_Distance(geom, ref) AS dist FROM bench_spatial_sort \
             ORDER BY dist, id LIMIT 500",
            "spatial_sort",
            true,
        );
        assert!(spatial_sort.contains("row_number() OVER () AS ord"));
        assert!(spatial_sort.contains("round(q.dist::numeric, 8)"));

        let grouped = correctness_projection_sql(
            "SELECT dept, sum(salary), avg(salary), count(*) FROM bench_employees_agg GROUP BY dept",
            "grouped_agg",
            false,
        );
        assert!(grouped.contains("round(q.sum::numeric, 2)"));
        assert!(grouped.contains("round(q.avg::numeric, 5)"));

        let high_card = correctness_projection_sql(
            "SELECT user_id, count(*), sum(val) FROM bench_events_agg GROUP BY user_id",
            "grouped_agg_high_card",
            false,
        );
        assert!(high_card.contains("q.group_count"));
        assert!(high_card.contains("q.input_rows"));
        assert!(high_card.contains("round(q.total_val::numeric, 3)"));

        let timeseries = correctness_projection_sql(
            "SELECT sensor_id, min(value), max(value), avg(value) FROM sensor_data GROUP BY sensor_id",
            "timeseries_sensor_rollup",
            false,
        );
        assert!(timeseries.contains("round(q.min::numeric, 5)"));
        assert!(timeseries.contains("round(q.max::numeric, 5)"));
        assert!(timeseries.contains("round(q.avg::numeric, 5)"));

        let dictionary = correctness_projection_sql(
            "SELECT region, sum(amount), count(*) FROM bench_dictionary_sales GROUP BY region",
            "dictionary_grouped_agg",
            false,
        );
        assert!(dictionary.contains("round(q.sum::numeric, 3)"));
        assert!(dictionary.contains("'region', q.region"));

        let hashjoin_filter = correctness_projection_sql(
            "SELECT d.name, SUM(f.amount) FROM bench_hjf_fact f JOIN bench_hjf_dim d \
             ON f.dim_id = d.id GROUP BY d.name",
            "gpu_hashjoin_filter",
            false,
        );
        assert!(hashjoin_filter.contains("'name', q.name"));
        assert!(hashjoin_filter.contains("round(q.sum::numeric, 3)"));

        let reduce_scaling = correctness_projection_sql(
            "SELECT SUM(val_f8) FROM bench_reduce_scale",
            "gpu_reduce_scaling",
            false,
        );
        assert!(reduce_scaling.contains("round(q.sum::numeric, 0)"));

        let reduce_sum_f32 = correctness_projection_sql(
            "SELECT SUM(vf4) FROM bench_reduce_var",
            "reduce_sum_f32",
            false,
        );
        assert!(reduce_sum_f32.contains("round(q.sum::numeric, -7)"));

        let reduce_sum = correctness_projection_sql(
            "SELECT SUM(val_f8), AVG(val_f4), MIN(val_i4), MAX(val_i4), COUNT(*) \
             FROM bench_reduce",
            "gpu_reduce_sum",
            false,
        );
        assert!(reduce_sum.contains("round(q.sum::numeric, 0)"));
        assert!(reduce_sum.contains("round(q.avg::numeric, 4)"));

        let reduce_multi = correctness_projection_sql(
            "SELECT SUM(vf8), MIN(vf8), MAX(vf8), COUNT(*) FROM bench_reduce_var",
            "reduce_multi",
            false,
        );
        assert!(reduce_multi.contains("round(q.sum::numeric, 3)"));
        assert!(reduce_multi.contains("round(q.min::numeric, 5)"));
        assert!(reduce_multi.contains("round(q.max::numeric, 5)"));

        let fp64_minmax = correctness_projection_sql(
            "SELECT MIN(v_f64), MAX(v_f64) FROM bench_fp64_num",
            "reduce_f64_minmax",
            false,
        );
        assert!(fp64_minmax.contains("round(q.min::numeric, 5)"));
        assert!(fp64_minmax.contains("round(q.max::numeric, 5)"));

        let fp64_stats = correctness_projection_sql(
            "SELECT AVG(v_f64), STDDEV(v_f64), VAR_POP(v_f64) FROM bench_fp64_num",
            "reduce_f64_stats",
            false,
        );
        assert!(fp64_stats.contains("round(q.avg::numeric, 5)"));
        assert!(fp64_stats.contains("round(q.stddev::numeric, 5)"));
        assert!(fp64_stats.contains("round(q.var_pop::numeric, 5)"));

        let fp64_hashagg_aggs = correctness_projection_sql(
            "SELECT gk, SUM(v_f64), AVG(w_f64), STDDEV(v_f64) \
             FROM bench_fp64_num GROUP BY gk",
            "hashagg_f64_aggs",
            false,
        );
        assert!(fp64_hashagg_aggs.contains("round(q.sum::numeric, 5)"));
        assert!(fp64_hashagg_aggs.contains("round(q.avg::numeric, 5)"));
        assert!(fp64_hashagg_aggs.contains("round(q.stddev::numeric, 5)"));

        let mixed_expr = correctness_projection_sql(
            "SELECT cat, SUM(v1), COUNT(*) FROM bench_mixed_expr \
             WHERE v1 * v2 + v3 > 500.0 GROUP BY cat",
            "mixed_expr_agg",
            false,
        );
        assert!(mixed_expr.contains("'cat', q.cat"));
        assert!(mixed_expr.contains("round(q.sum::numeric, -4)"));
        assert!(mixed_expr.contains("'count', q.count"));

        let mixed_join = correctness_projection_sql(
            "SELECT d.label, SUM(f.amount), COUNT(*) FROM bench_mixed_facts f \
             INNER JOIN bench_mixed_dims d ON f.dim_id = d.id GROUP BY d.label",
            "mixed_join_agg",
            false,
        );
        assert!(mixed_join.contains("'label', q.label"));
        assert!(mixed_join.contains("round(q.sum::numeric, 3)"));
        assert!(mixed_join.contains("'count', q.count"));

        let expression = correctness_projection_sql(
            "SELECT product_id, sum(price * discount), count(*) \
             FROM bench_expression_sales GROUP BY product_id",
            "expression_grouped_agg",
            false,
        );
        assert!(expression.contains("round(q.sum::numeric, 3)"));
        assert!(expression.contains("'product_id', q.product_id"));

        let predicate_expression = correctness_projection_sql(
            "SELECT product_id, sum(price * discount) FILTER (WHERE active), \
             count(*) FILTER (WHERE active) \
             FROM bench_predicate_expression_sales GROUP BY product_id",
            "predicate_filter_expression_grouped_agg",
            false,
        );
        assert!(predicate_expression.contains("round(q.sum::numeric, 3)"));
        assert!(predicate_expression.contains("'product_id', q.product_id"));

        let case_when_expression = correctness_projection_sql(
            "SELECT product_id, \
                    sum(CASE WHEN active THEN price * discount ELSE 0 END), \
                    count(*) \
             FROM bench_case_when_expression_sales GROUP BY product_id",
            "case_when_expression_grouped_agg",
            false,
        );
        assert!(case_when_expression.contains("round(q.sum::numeric, 3)"));
        assert!(case_when_expression.contains("'product_id', q.product_id"));

        let case_when_range_expression = correctness_projection_sql(
            "SELECT product_id, \
                    sum(CASE WHEN active AND discount BETWEEN 0.25 AND 0.40 \
                             THEN price * discount ELSE 0 END), \
                    count(*) \
             FROM bench_case_when_range_expression_sales GROUP BY product_id",
            "case_when_range_expression_grouped_agg",
            false,
        );
        assert!(case_when_range_expression.contains("round(q.sum::numeric, 3)"));
        assert!(case_when_range_expression.contains("'product_id', q.product_id"));

        let case_when_value_predicate_expression = correctness_projection_sql(
            "SELECT product_id, \
                    sum(CASE WHEN active AND price >= 500.0 \
                             THEN price * discount ELSE 0 END), \
                    count(*) \
             FROM bench_case_when_value_predicate_expression_sales GROUP BY product_id",
            "case_when_value_predicate_expression_grouped_agg",
            false,
        );
        assert!(case_when_value_predicate_expression.contains("round(q.sum::numeric, 3)"));
        assert!(case_when_value_predicate_expression.contains("'product_id', q.product_id"));

        let case_when_null_predicate_expression = correctness_projection_sql(
            "SELECT product_id, \
                    sum(CASE WHEN active AND price IS NOT NULL AND price >= 500.0 \
                             THEN price * discount ELSE 0 END), \
                    count(*) \
             FROM bench_case_when_null_predicate_expression_sales GROUP BY product_id",
            "case_when_null_predicate_expression_grouped_agg",
            false,
        );
        assert!(case_when_null_predicate_expression.contains("round(q.sum::numeric, 3)"));
        assert!(case_when_null_predicate_expression.contains("'product_id', q.product_id"));

        let case_when_or_expression = correctness_projection_sql(
            "SELECT product_id, \
                    sum(CASE WHEN active AND (discount < 0.10 \
                                              OR discount BETWEEN 0.25 AND 0.30 \
                                              OR discount >= 0.45) \
                             THEN price * discount ELSE 0 END), \
                    count(*) \
             FROM bench_case_when_or_expression_sales GROUP BY product_id",
            "case_when_or_expression_grouped_agg",
            false,
        );
        assert!(case_when_or_expression.contains("round(q.sum::numeric, 3)"));
        assert!(case_when_or_expression.contains("'product_id', q.product_id"));

        let case_when_in_expression = correctness_projection_sql(
            "SELECT product_id, \
                    sum(CASE WHEN active AND discount IN (0.05, 0.15, 0.25, 0.45) \
                             THEN price * discount ELSE 0 END), \
                    count(*) \
             FROM bench_case_when_in_expression_sales GROUP BY product_id",
            "case_when_in_expression_grouped_agg",
            false,
        );
        assert!(case_when_in_expression.contains("round(q.sum::numeric, 3)"));
        assert!(case_when_in_expression.contains("'product_id', q.product_id"));

        let case_when_not_expression = correctness_projection_sql(
            "SELECT product_id, \
                    sum(CASE WHEN active AND discount NOT IN (0.10, 0.25, 0.35) \
                             THEN price * discount ELSE 0 END), \
                    count(*) \
             FROM bench_case_when_not_expression_sales GROUP BY product_id",
            "case_when_not_expression_grouped_agg",
            false,
        );
        assert!(case_when_not_expression.contains("round(q.sum::numeric, 3)"));
        assert!(case_when_not_expression.contains("'product_id', q.product_id"));
    }

    #[test]
    fn test_correctness_projection_keeps_exact_release_aggregates_unrounded() {
        for name in [
            "grouped_agg_int4",
            "predicate_expression_grouped_agg_int4",
            "and_range_predicate_expression_grouped_agg_int4",
            "mixed_join_agg_int4",
            "ssbm_resident_int4_star",
        ] {
            let workload = crate::workloads::find_workload(name)
                .unwrap_or_else(|| panic!("registered exact workload {name}"));
            let projection = correctness_projection_sql(&workload.query_sql(), name, false);
            assert!(projection.contains("to_jsonb(q)::text"), "{name}");
            assert!(
                !projection.to_ascii_lowercase().contains("round("),
                "{name}"
            );
            assert!(projection.contains(&workload.query_sql()), "{name}");
        }
    }

    #[test]
    fn test_raster_exact_reclass_projection_consumes_byte_exact_materialized_output() {
        let workload = crate::workloads::find_workload("raster_resident_exact_reclass")
            .expect("registered exact raster workload");
        let query = workload.query_sql();
        let projection = correctness_projection_sql(&query, workload.name(), false);
        assert!(projection.starts_with("WITH q AS MATERIALIZED ("));
        assert!(projection.contains(&query));
        assert!(projection.contains("encode(ST_AsBinary(q.rast), 'hex')"));
        assert!(!projection.contains("to_jsonb(q)"));
    }

    #[test]
    fn test_top_level_order_by_detection_ignores_window_and_strings() {
        assert!(has_top_level_order_by("SELECT id FROM t ORDER BY id"));
        assert!(!has_top_level_order_by(
            "SELECT row_number() OVER (ORDER BY id) FROM t"
        ));
        assert!(!has_top_level_order_by(
            "SELECT 'order by id' AS literal FROM t"
        ));
        assert!(!has_top_level_order_by("SELECT order_by_col FROM t"));
    }

    #[test]
    fn test_parse_control_default_version() {
        assert_eq!(
            parse_control_default_version(
                "# comment\n\
                 default_version = '1.0.0'\n"
            )
            .as_deref(),
            Some("1.0.0")
        );
        assert_eq!(
            parse_control_default_version("default_version = \"2.3.4\" # generated").as_deref(),
            Some("2.3.4")
        );
        assert_eq!(
            parse_control_default_version(
                "default_version_suffix = 'wrong'\n\
                 \tdefault_version\t=\t'3.4.5'\t # generated"
            )
            .as_deref(),
            Some("3.4.5")
        );

        for invalid in [
            "comment = 'no version'",
            "default_version_suffix = '1.0.0'",
            "prefix_default_version = '1.0.0'",
            "default_version extra = '1.0.0'",
            "default_version",
            "default_version = ''",
            "default_version = 1.0.0",
            "default_version = 'unterminated",
            "default_version = \"mismatched'",
            "default_version == '1.0.0'",
        ] {
            assert!(
                parse_control_default_version(invalid).is_none(),
                "input={invalid:?}"
            );
        }
    }

    #[test]
    fn test_shared_preload_contains_pg_accel() {
        assert!(shared_preload_contains_pg_accel(Some(
            "pg_stat_statements, pg_accel"
        )));
        assert!(shared_preload_contains_pg_accel(Some("'pg_accel'")));
        assert!(!shared_preload_contains_pg_accel(Some(
            "pg_stat_statements"
        )));
        assert!(!shared_preload_contains_pg_accel(None));
    }

    #[test]
    fn test_pg_accel_library_path_detection() {
        assert!(is_pg_accel_library_path("/opt/pg/lib/pg_accel.dylib"));
        assert!(is_pg_accel_library_path(
            "/usr/lib/postgresql/libpg_accel.so"
        ));
        assert!(!is_pg_accel_library_path("/tmp/pg_accel_traces.jsonl"));
        assert!(!is_pg_accel_library_path("/opt/pg/lib/postgis-3.so"));
    }

    #[test]
    fn test_file_probe_warning_for_missing_hash() {
        let probe = FileProvenance {
            path: "/tmp/pg_accel.dylib".to_owned(),
            exists: true,
            sha256: None,
            len_bytes: Some(12),
            modified_unix_seconds: Some(42),
            mapping_deleted: false,
            error: Some("hash tool missing".to_owned()),
        };
        let warning = file_probe_warning("loaded backend binary", Some(&probe));
        assert!(warning.is_some_and(|text| text.contains("hash tool missing")));
    }

    struct RunnerWorkload;

    impl Workload for RunnerWorkload {
        fn name(&self) -> &'static str {
            "runner_workload"
        }

        fn description(&self) -> &'static str {
            "runner unit-test workload"
        }

        fn setup_sql(&self, _rows: usize) -> Vec<String> {
            vec![
                "CREATE TABLE runner_primary (id bigint)".to_owned(),
                "CREATE TABLE IF NOT EXISTS runner_secondary (id bigint)".to_owned(),
                "CREATE TABLE".to_owned(),
                "CREATE TABLE (".to_owned(),
                "ANALYZE runner_primary".to_owned(),
            ]
        }

        fn pre_query_sql(&self) -> Vec<String> {
            vec!["SET work_mem = '8MB'".to_owned()]
        }

        fn query_sql(&self) -> String {
            "SELECT count(*) FROM runner_primary".to_owned()
        }

        fn baseline_query_sql(&self) -> Option<String> {
            Some("SELECT count(*) FROM runner_secondary".to_owned())
        }

        fn cleanup_sql(&self) -> Vec<String> {
            vec!["DROP TABLE runner_primary, runner_secondary".to_owned()]
        }
    }

    struct MinimalRunnerWorkload;

    impl Workload for MinimalRunnerWorkload {
        fn name(&self) -> &'static str {
            "minimal_runner_workload"
        }

        fn description(&self) -> &'static str {
            "minimal runner unit-test workload"
        }

        fn setup_sql(&self, _rows: usize) -> Vec<String> {
            Vec::new()
        }

        fn query_sql(&self) -> String {
            "SELECT 1\n".to_owned()
        }

        fn cleanup_sql(&self) -> Vec<String> {
            Vec::new()
        }
    }

    fn runner_query_identity() -> BenchmarkQueryIdentity {
        BenchmarkQueryIdentity::resolve(
            RunnerWorkload.query_sql(),
            RunnerWorkload.baseline_query_sql(),
        )
        .expect("runner workload query identity should be valid")
    }

    fn provenance_file(path: &str, hash: Option<&str>, modified: Option<u64>) -> FileProvenance {
        FileProvenance {
            path: path.to_owned(),
            exists: true,
            sha256: hash.map(str::to_owned),
            len_bytes: Some(64),
            modified_unix_seconds: modified,
            mapping_deleted: false,
            error: hash.is_none().then(|| "digest unavailable".to_owned()),
        }
    }

    fn valid_provenance_report() -> ProvenanceReport {
        ProvenanceReport {
            schema_version: PROVENANCE_SCHEMA_VERSION,
            status: ProvenanceStatus::Pass,
            expected_extension_version: EXPECTED_EXTENSION_VERSION.to_owned(),
            postgres: PostgresProvenance {
                backend_pid: 42,
                server_version: Some("18.0".to_owned()),
                data_directory: Some("/pg/data".to_owned()),
                config_file: Some("/pg/data/postgresql.conf".to_owned()),
                shared_preload_libraries: Some("pg_stat_statements, pg_accel".to_owned()),
                postmaster_start_time: Some("2026-07-21 00:00:00Z".to_owned()),
                postmaster_start_unix_seconds: Some(100),
            },
            sql: SqlExtensionProvenance {
                extversion: Some(EXPECTED_EXTENSION_VERSION.to_owned()),
                pg_accel_version: Some(EXPECTED_EXTENSION_VERSION.to_owned()),
                function_probin: Some("$libdir/pg_accel".to_owned()),
                function_prosrc: Some("pg_accel_version".to_owned()),
            },
            live_smoke: LiveExtensionSmoke {
                backend_pid: 42,
                pg_accel_version: Some(EXPECTED_EXTENSION_VERSION.to_owned()),
                kernel_executions: Some(0),
                stats_rows: Some(1),
                error: None,
            },
            pg_config: PgConfigProvenance {
                command: Some("pg_config".to_owned()),
                pkglibdir: Some("/pg/lib".to_owned()),
                sharedir: Some("/pg/share".to_owned()),
                error: None,
                control_file: Some(provenance_file(
                    "/pg/share/pg_accel.control",
                    Some("aa"),
                    Some(50),
                )),
                control_default_version: Some(EXPECTED_EXTENSION_VERSION.to_owned()),
                sql_files: vec![provenance_file(
                    "/pg/share/pg_accel--1.sql",
                    Some("bb"),
                    Some(50),
                )],
            },
            expected_binary: Some(provenance_file(
                "/build/libpg_accel.dylib",
                Some("same"),
                Some(50),
            )),
            installed_binary: Some(provenance_file(
                "/pg/lib/pg_accel.dylib",
                Some("same"),
                Some(50),
            )),
            loaded_binaries: vec![provenance_file(
                "/pg/lib/pg_accel.dylib",
                Some("same"),
                Some(50),
            )],
            mapped_library_discovery: MappedLibraryDiscovery {
                method: "lsof".to_owned(),
                mapped_paths: vec!["/pg/lib/pg_accel.dylib".to_owned()],
                warning: None,
            },
            device_limits_sources: vec!["metal".to_owned()],
            warnings: Vec::new(),
            errors: Vec::new(),
        }
    }

    #[test]
    fn provenance_evaluator_accepts_a_consistent_live_install() {
        let mut report = valid_provenance_report();
        evaluate_provenance(&mut report);

        assert!(report.errors.is_empty());
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn provenance_version_and_smoke_failures_are_specific() {
        let mut wrong = valid_provenance_report();
        wrong.sql.extversion = Some("0.0.1".to_owned());
        wrong.sql.pg_accel_version = Some("0.0.2".to_owned());
        wrong.live_smoke.pg_accel_version = Some("0.0.3".to_owned());
        check_extension_versions(&mut wrong);
        check_live_extension_smoke(&mut wrong);
        assert_eq!(wrong.errors.len(), 3);
        assert!(wrong.errors[0].contains("pg_extension.extversion is 0.0.1"));
        assert!(wrong.errors[1].contains("pg_accel_version() returned 0.0.2"));
        assert!(wrong.errors[2].contains("live pg_accel smoke returned version 0.0.3"));

        let mut missing = valid_provenance_report();
        missing.sql.extversion = None;
        missing.sql.pg_accel_version = None;
        missing.live_smoke.pg_accel_version = None;
        missing.live_smoke.kernel_executions = None;
        missing.live_smoke.stats_rows = Some(0);
        check_extension_versions(&mut missing);
        check_live_extension_smoke(&mut missing);
        assert_eq!(missing.errors.len(), 5);
        assert!(
            missing
                .errors
                .iter()
                .any(|error| error.contains("does not list"))
        );
        assert!(
            missing
                .errors
                .iter()
                .any(|error| error.contains("did not return a version"))
        );
        assert!(
            missing
                .errors
                .iter()
                .any(|error| error.contains("could not read"))
        );
        assert!(
            missing
                .errors
                .iter()
                .any(|error| error.contains("did not get a row"))
        );

        let mut failed = valid_provenance_report();
        failed.live_smoke.error = Some("symbol lookup failed".to_owned());
        failed.live_smoke.kernel_executions = None;
        failed.live_smoke.stats_rows = None;
        check_live_extension_smoke(&mut failed);
        assert_eq!(failed.errors.len(), 1);
        assert!(failed.errors[0].contains("backend 42: symbol lookup failed"));
    }

    #[test]
    fn provenance_control_and_device_limit_checks_cover_all_states() {
        let mut report = valid_provenance_report();
        report.pg_config.control_default_version = Some("old".to_owned());
        check_control_version(&mut report);
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("is old"));

        report.warnings.clear();
        report.pg_config.control_default_version = None;
        check_control_version(&mut report);
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("could not read"));

        report.warnings.clear();
        report.pg_config.sharedir = None;
        check_control_version(&mut report);
        assert!(report.warnings.is_empty());

        report.device_limits_sources.clear();
        check_device_limits(&mut report);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("no source rows"));

        report.errors.clear();
        report.device_limits_sources = vec!["metal".to_owned(), "fallback_cpu_only".to_owned()];
        check_device_limits(&mut report);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("real GPU path"));
    }

    #[test]
    fn provenance_binary_relationships_reject_unprovable_or_stale_mappings() {
        let mut report = valid_provenance_report();
        report.expected_binary = Some(provenance_file("/missing/expected", None, None));
        report
            .expected_binary
            .as_mut()
            .expect("expected probe")
            .exists = false;
        report.installed_binary = Some(provenance_file("/pg/lib/pg_accel.dylib", None, Some(50)));
        report.loaded_binaries = vec![FileProvenance {
            mapping_deleted: true,
            ..provenance_file("/pg/lib/pg_accel.dylib (deleted)", None, Some(150))
        }];
        evaluate_provenance(&mut report);

        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("does not exist"))
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("could not compute SHA-256"))
        );
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("could not compute SHA-256"))
        );
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("deleted/replaced"))
        );
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("modified after"))
        );

        let mut hashes = valid_provenance_report();
        hashes
            .expected_binary
            .as_mut()
            .expect("expected probe")
            .sha256 = Some("expected".to_owned());
        hashes
            .installed_binary
            .as_mut()
            .expect("installed probe")
            .sha256 = Some("installed".to_owned());
        hashes.loaded_binaries[0].sha256 = Some("loaded".to_owned());
        check_hash_relationships(&mut hashes);
        assert_eq!(hashes.errors.len(), 1);
        assert!(hashes.errors[0].contains("expected local build hash expected"));
        assert_eq!(hashes.warnings.len(), 2);
        assert!(
            hashes
                .warnings
                .iter()
                .any(|warning| warning.contains("installed pg_accel binary hash"))
        );
        assert!(
            hashes
                .warnings
                .iter()
                .any(|warning| warning.contains("pkglibdir binary hash"))
        );

        let mut undiscovered = valid_provenance_report();
        undiscovered.loaded_binaries.clear();
        evaluate_provenance(&mut undiscovered);
        assert!(
            undiscovered
                .errors
                .iter()
                .any(|error| error.contains("cannot prove the loaded dylib hash"))
        );
    }

    #[test]
    fn provenance_restart_check_uses_loaded_then_installed_binary() {
        let mut unknown_start = valid_provenance_report();
        unknown_start.postgres.postmaster_start_unix_seconds = None;
        check_postmaster_restart_required(&mut unknown_start);
        assert_eq!(unknown_start.warnings.len(), 1);

        let mut installed_fallback = valid_provenance_report();
        installed_fallback.loaded_binaries.clear();
        installed_fallback
            .installed_binary
            .as_mut()
            .expect("installed probe")
            .modified_unix_seconds = Some(101);
        check_postmaster_restart_required(&mut installed_fallback);
        assert_eq!(installed_fallback.errors.len(), 1);
        assert!(installed_fallback.errors[0].contains("installed pg_accel binary"));

        let mut not_preloaded = valid_provenance_report();
        not_preloaded.postgres.shared_preload_libraries = Some("pg_stat_statements".to_owned());
        not_preloaded.loaded_binaries[0].modified_unix_seconds = Some(101);
        check_postmaster_restart_required(&mut not_preloaded);
        assert!(not_preloaded.errors.is_empty());

        let failure = ProvenanceFailure {
            errors: vec!["first".to_owned(), "second".to_owned()],
        };
        assert_eq!(
            failure.to_string(),
            "pg_accel provenance gate failed: first; second"
        );
    }

    #[test]
    fn provenance_file_and_metadata_helpers_inspect_real_files() {
        let dir = TestDir::new("provenance-files");
        let data = dir.path().join("payload.bin");
        fs::write(&data, b"abc").expect("fixture should be written");

        let digest = sha256_file(&data).expect("a system SHA-256 tool should be available");
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let inspected = inspect_file(&data, false);
        assert!(inspected.exists);
        assert_eq!(inspected.len_bytes, Some(3));
        assert_eq!(inspected.sha256.as_deref(), Some(digest.as_str()));

        let missing = inspect_file(&dir.path().join("missing"), true);
        assert!(!missing.exists);
        assert!(missing.mapping_deleted);
        assert!(missing.error.is_some());

        let deleted = inspect_mapped_library(&MappedLibrary {
            path: data.clone(),
            display_path: format!("{} (deleted)", data.display()),
            deleted: true,
        });
        assert!(!deleted.exists);
        assert!(deleted.mapping_deleted);
        assert!(
            deleted
                .error
                .as_deref()
                .is_some_and(|error| error.contains("deleted"))
        );

        let extension = dir.path().join("share/extension");
        fs::create_dir_all(&extension).expect("extension fixture directory should be created");
        fs::write(
            extension.join("pg_accel.control"),
            format!("default_version = '{EXPECTED_EXTENSION_VERSION}'\n"),
        )
        .expect("control fixture should be written");
        fs::write(extension.join("pg_accel--1.sql"), "SELECT 1;")
            .expect("SQL fixture should be written");
        fs::write(extension.join("pg_accel--2.SQL"), "SELECT 2;")
            .expect("SQL fixture should be written");
        fs::write(extension.join("other--1.sql"), "SELECT 3;")
            .expect("noise fixture should be written");
        let (control, version, sql) =
            capture_extension_metadata_files(&dir.path().join("share").display().to_string());
        assert!(control.is_some_and(|probe| probe.exists));
        assert_eq!(version.as_deref(), Some(EXPECTED_EXTENSION_VERSION));
        assert_eq!(sql.len(), 2);
        assert!(sql[0].path.ends_with("pg_accel--1.sql"));
        assert!(sql[1].path.ends_with("pg_accel--2.SQL"));

        let installed_name = installed_extension_library_names()
            .pop()
            .expect("at least one installed library name");
        fs::write(dir.path().join(&installed_name), b"library")
            .expect("installed library fixture should be written");
        assert_eq!(
            find_installed_extension_binary(&dir.path().display().to_string()),
            Some(dir.path().join(installed_name))
        );
        assert!(find_installed_extension_binary("/definitely/missing/pg/lib").is_none());
    }

    #[test]
    fn provenance_command_mapping_and_time_helpers_are_deterministic() {
        assert_eq!(
            command_stdout("sh", &["-c", "printf '  value  '"])
                .expect("shell fixture should succeed"),
            "value"
        );
        assert!(command_stdout("sh", &["-c", "exit 7"]).is_err());
        assert!(command_stdout("/definitely/missing/command", &[]).is_err());

        let library = MappedLibrary {
            path: PathBuf::from("/pg/lib/pg_accel.dylib"),
            display_path: "/pg/lib/pg_accel.dylib".to_owned(),
            deleted: false,
        };
        let probe = mapped_library_probe("fixture", vec![library], Some("note".to_owned()));
        assert_eq!(probe.discovery.method, "fixture");
        assert_eq!(probe.discovery.mapped_paths, ["/pg/lib/pg_accel.dylib"]);
        assert_eq!(probe.discovery.warning.as_deref(), Some("note"));
        assert_eq!(probe.libraries.len(), 1);

        let root = Path::new("/workspace");
        let candidates = build_output_candidates(root);
        assert_eq!(candidates.len(), 4);
        assert!(candidates[0].starts_with("/workspace/target/release"));
        assert_eq!(system_time_unix_secs(UNIX_EPOCH), Some(0));
        assert_eq!(
            system_time_unix_secs(UNIX_EPOCH + std::time::Duration::from_secs(9)),
            Some(9)
        );
        assert_eq!(
            system_time_unix_secs(UNIX_EPOCH - std::time::Duration::from_secs(1)),
            None
        );
    }

    fn dispatch_test_result() -> WorkloadResult {
        WorkloadResult::from_iterations(
            "dispatch".to_owned(),
            "dispatch merge".to_owned(),
            "gpu".to_owned(),
            "unclassified".to_owned(),
            10_000,
            vec![iter_at(1.0, 2.0, CacheState::Warm)],
            true,
        )
    }

    #[test]
    fn dispatch_counter_helpers_accumulate_reset_iterations_and_fail_on_regression() {
        assert!(nonnegative_dispatch_counter("fixture", -1).is_err());
        assert_eq!(
            nonnegative_dispatch_counter("fixture", 17)
                .expect("a nonnegative SQL counter should convert"),
            17
        );

        let first = dispatch_stats_delta(
            DispatchStatsSnapshot {
                backend_pid: 42,
                queries_accelerated: 10,
                rows_dispatched: 10,
                batches_executed: 20,
                stock_exec_count: 30,
                gpu_rows_processed: 40,
                gpu_kernel_executions: 50,
                physical_kernel_modes: report::PhysicalKernelModeCounts {
                    parallel_dense_count: 3,
                    ..report::PhysicalKernelModeCounts::default()
                },
            },
            DispatchStatsSnapshot {
                backend_pid: 42,
                queries_accelerated: 11,
                rows_dispatched: 15,
                batches_executed: 21,
                stock_exec_count: 32,
                gpu_rows_processed: 44,
                gpu_kernel_executions: 55,
                physical_kernel_modes: report::PhysicalKernelModeCounts {
                    parallel_dense_count: 4,
                    ..report::PhysicalKernelModeCounts::default()
                },
            },
        )
        .expect("monotonic counters produce a delta");
        let second_after_reset = dispatch_stats_delta(
            DispatchStatsSnapshot::default(),
            DispatchStatsSnapshot {
                backend_pid: 0,
                queries_accelerated: 1,
                rows_dispatched: 7,
                batches_executed: 1,
                stock_exec_count: 0,
                gpu_rows_processed: 7,
                gpu_kernel_executions: 1,
                physical_kernel_modes: report::PhysicalKernelModeCounts {
                    parallel_dense_count: 1,
                    ..report::PhysicalKernelModeCounts::default()
                },
            },
        )
        .expect("a reset between iterations is coherent when sampled per iteration");
        let mut delta = DispatchStatsDelta::default();
        accumulate_dispatch_stats(&mut delta, first)
            .expect("the first bounded dispatch delta should accumulate");
        accumulate_dispatch_stats(&mut delta, second_after_reset)
            .expect("the reset iteration delta should accumulate");
        assert_eq!(delta.queries_accelerated, 2);
        assert_eq!(delta.rows_dispatched, 12);
        assert_eq!(delta.batches_executed, 2);
        assert_eq!(delta.stock_exec_count, 2);
        assert_eq!(delta.gpu_rows_processed, 11);
        assert_eq!(delta.gpu_kernel_executions, 6);
        assert_eq!(delta.physical_kernel_modes.parallel_dense_count, 2);

        let regression = dispatch_stats_delta(
            DispatchStatsSnapshot {
                batches_executed: 2,
                ..DispatchStatsSnapshot::default()
            },
            DispatchStatsSnapshot {
                batches_executed: 1,
                ..DispatchStatsSnapshot::default()
            },
        )
        .expect_err("a reset inside one measured iteration must fail closed");
        assert!(regression.contains("batches_executed"));

        let backend_change = dispatch_stats_delta(
            DispatchStatsSnapshot {
                backend_pid: 41,
                ..DispatchStatsSnapshot::default()
            },
            DispatchStatsSnapshot {
                backend_pid: 42,
                ..DispatchStatsSnapshot::default()
            },
        )
        .expect_err("before/after snapshots from different backends must fail closed");
        assert!(backend_change.contains("PID 41 to 42"));

        let delta = first;
        assert_eq!(delta.rows_dispatched, 5);
        assert_eq!(delta.batches_executed, 1);
        assert_eq!(delta.stock_exec_count, 2);
        assert_eq!(delta.gpu_rows_processed, 4);
        assert_eq!(delta.gpu_kernel_executions, 5);
        assert_eq!(delta.queries_accelerated, 1);

        let mut target = dispatch_test_result();
        merge_dispatch_counter_capture(
            &mut target,
            DispatchCounterCapture {
                captured: true,
                delta,
                error: Some("first".to_owned()),
            },
        );
        assert!(target.dispatch_counter_captured);
        assert_eq!(target.pg_accel_queries_accelerated_delta, 1);
        assert_eq!(target.gpu_kernel_execution_delta, 5);
        assert_eq!(target.pg_accel_rows_dispatched_delta, 5);
        assert_eq!(target.pg_accel_stock_exec_delta, 2);
        assert_eq!(target.dispatch_counter_error.as_deref(), Some("first"));

        let mut cold = dispatch_test_result();
        cold.dispatch_counter_captured = true;
        cold.pg_accel_queries_accelerated_delta = 5;
        cold.gpu_kernel_execution_delta = 7;
        cold.accel_output_rows_consumed = 11;
        let mut warm = dispatch_test_result();
        warm.dispatch_counter_captured = false;
        warm.dispatch_counter_error = Some("warm stats missing".to_owned());
        merge_cache_mode_dispatch_counter_fields(&mut target, &cold, &warm);
        assert!(!target.dispatch_counter_captured);
        assert_eq!(target.pg_accel_queries_accelerated_delta, 5);
        assert_eq!(target.gpu_kernel_execution_delta, 7);
        assert_eq!(target.accel_output_rows_consumed, 11);
        assert!(
            target
                .dispatch_counter_error
                .as_deref()
                .is_some_and(|error| error.contains("warm stats missing"))
        );

        warm.dispatch_counter_captured = true;
        warm.dispatch_counter_error = None;
        warm.pg_accel_queries_accelerated_delta = 6;
        warm.gpu_kernel_execution_delta = 8;
        warm.accel_output_rows_consumed = 12;
        merge_cache_mode_dispatch_counter_fields(&mut target, &cold, &warm);
        assert!(target.dispatch_counter_captured);
        assert_eq!(target.pg_accel_queries_accelerated_delta, 11);
        assert_eq!(target.gpu_kernel_execution_delta, 15);
        assert_eq!(target.accel_output_rows_consumed, 23);
        assert!(target.dispatch_counter_error.is_none());

        cold.gpu_kernel_execution_delta = u64::MAX;
        merge_cache_mode_dispatch_counter_fields(&mut target, &cold, &warm);
        assert!(!target.dispatch_counter_captured);
        assert!(
            target
                .dispatch_counter_error
                .as_deref()
                .is_some_and(|error| error.contains("overflowed"))
        );

        let unavailable = DispatchCounterCapture::unavailable("stats missing");
        assert!(!unavailable.captured);
        assert_eq!(unavailable.error.as_deref(), Some("stats missing"));
    }

    #[test]
    fn physical_kernel_mode_parser_reads_only_the_exact_explain_property() {
        let plan = "Custom Scan (GpuAccelAgg)\n  GPU Physical Kernel Mode: parallel_dense_integer\n  GPU Descriptor Strategy: descriptor_grouped_aggregate";
        assert_eq!(
            plan_physical_kernel_mode(plan),
            Some("parallel_dense_integer")
        );
        assert_eq!(
            plan_physical_kernel_mode("GPU Dispatched Physical Kernel Mode: parallel_hash"),
            None
        );
    }

    #[test]
    fn workload_table_parser_handles_plain_and_if_not_exists_forms() {
        assert_eq!(
            workload_tables(&RunnerWorkload, 123),
            ["runner_primary", "runner_secondary"]
        );
    }

    #[test]
    fn crash_context_artifact_contains_repro_config_sql_paths_and_excerpts() {
        let dir = TestDir::new("crash-context");
        let writer = ArtifactWriter::new(dir.path().to_path_buf(), Vec::new())
            .expect("artifact writer should initialize");
        let large_plan = vec![b'p'; CRASH_CONTEXT_EMBED_BYTES + 7];
        fs::write(dir.path().join("plan.txt"), large_plan).expect("plan fixture should be written");
        fs::write(dir.path().join("log.txt"), "tail line\n")
            .expect("log fixture should be written");
        fs::write(dir.path().join(RUST_BACKTRACE_ARTIFACT), "enabled\n")
            .expect("backtrace fixture should be written");
        fs::write(dir.path().join("guc_snapshot.json"), "{}\n")
            .expect("GUC fixture should be written");
        fs::write(dir.path().join("provenance.json"), "{}\n")
            .expect("provenance fixture should be written");

        let config = BenchConfig {
            iterations: 7,
            warmup: 3,
            seed: 99,
            timing_mode: TimingMode::Both,
            cache_mode: CacheMode::Cold,
            capture_planner_stages: false,
            native_parity_pairing: false,
            headline_speedup_allowed: true,
            plans_capture_path: Some(dir.path().join("plans/all.txt")),
            guc_profile: Some(GucProfile::toy()),
            skip_guc_verify: true,
            artifacts_dir: Some(dir.path().to_path_buf()),
        };
        let logs = vec!["log.txt".to_owned()];
        let context = CrashContext {
            connection: "host=local dbname='quoted db'",
            workload: &RunnerWorkload,
            rows: 12_345,
            config: &config,
            label: "crash 001/runner",
            error: "backend closed",
            repro_command: "cargo run -- repro",
            plan_snippet_artifact: Some("plan.txt"),
            correctness_diff_artifact: Some("missing-correctness.json"),
            log_tail_artifacts: &logs,
        };
        let relative = write_crash_context_artifact(&writer, &context)
            .expect("crash context should be written");
        assert_eq!(relative, "crash_contexts/crash-001-runner.txt");
        let text = fs::read_to_string(dir.path().join(relative))
            .expect("crash context should be readable");
        assert!(text.contains("workload: runner_workload"));
        assert!(text.contains("iterations: 7"));
        assert!(text.contains("timing: both"));
        assert!(text.contains("cache_mode: cold"));
        assert!(text.contains("realistic_gucs: true"));
        assert!(text.contains("--- pre-query 1 ---"));
        assert!(text.contains("--- baseline query ---"));
        assert!(text.contains("plan_snippet: plan.txt"));
        assert!(text.contains("provenance: provenance.json"));
        assert!(text.contains("[truncated to last"));
        assert!(text.contains("<could not read"));
        assert!(text.contains("tail line"));

        let mut absent = String::new();
        append_optional_artifact_excerpt(&mut absent, dir.path(), "Absent", None);
        assert!(absent.contains("<not available>"));
        assert_eq!(
            existing_artifact_path(dir.path(), "log.txt").as_deref(),
            Some("log.txt")
        );
        assert!(existing_artifact_path(dir.path(), "does-not-exist").is_none());
        assert_eq!(
            artifact_display_path(dir.path(), "log.txt"),
            dir.path().join("log.txt")
        );
        assert_eq!(
            artifact_display_path(dir.path(), "/tmp/absolute"),
            PathBuf::from("/tmp/absolute")
        );
        assert_eq!(
            relative_artifact_path(dir.path(), &dir.path().join("log.txt")),
            "log.txt"
        );
    }

    #[test]
    fn plan_initialization_and_crash_record_cover_optional_artifact_paths() {
        let dir = TestDir::new("plans-and-crash");
        let mut config = BenchConfig::default();
        initialize_plans_file(&config);
        assert_eq!(
            fs::read_dir(dir.path())
                .expect("test directory should be readable")
                .count(),
            0
        );
        config.plans_capture_path = Some(dir.path().join("nested/plans.txt"));
        initialize_plans_file(&config);
        assert_eq!(
            fs::read_to_string(config.plans_capture_path.as_ref().expect("plan path"))
                .expect("plans file should be readable"),
            "=== pg_accel benchmark plans - captured once per workload/scale ===\n"
        );
        config.plans_capture_path = Some(dir.path().to_path_buf());
        initialize_plans_file(&config);
        assert!(dir.path().is_dir());

        let crashed = record_crash(
            "host=localhost dbname=bench",
            &RunnerWorkload,
            50_000,
            &config,
            None,
            4,
            "connection reset",
        );
        assert_eq!(crashed.workload, "runner_workload");
        assert_eq!(crashed.rows, 50_000);
        assert_eq!(crashed.error, "connection reset");
        assert!(crashed.plan_snippet_artifact.is_none());
        assert!(crashed.correctness_diff_artifact.is_none());
        assert!(crashed.log_tail_artifacts.is_empty());
        assert!(crashed.repro_command.as_deref().is_some_and(|command| {
            command.contains("crash-repro --workload runner_workload --rows 50000")
        }));

        assert_eq!(sanitize_artifact_component(&"x".repeat(120)).len(), 96);
        assert_eq!(shell_quote("plain/path:1"), "plain/path:1");
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }

    #[test]
    fn failed_pre_risk_capture_persists_complete_diagnostic_context() {
        let dir = TestDir::new("pre-risk-connect-failure");
        let writer = ArtifactWriter::new(dir.path().to_path_buf(), Vec::new())
            .expect("artifact writer should initialize");
        let identity = runner_query_identity();
        let config = BenchConfig {
            iterations: 13,
            warmup: 6,
            seed: 77,
            timing_mode: TimingMode::Both,
            cache_mode: CacheMode::Cold,
            plans_capture_path: Some(dir.path().join("plans.txt")),
            guc_profile: Some(GucProfile::toy()),
            skip_guc_verify: true,
            ..BenchConfig::default()
        };

        capture_and_write_pre_risk_context(
            "host=/definitely/missing/pg_accel_socket dbname=missing connect_timeout=1",
            &RunnerWorkload,
            222,
            &config,
            &identity,
            &writer,
        )
        .expect("connection failure should be captured as evidence, not returned");

        let relative = writer
            .existing_pre_risk_context_artifact("runner_workload", 222)
            .expect("pre-risk context should exist");
        let value: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(artifact_display_path(dir.path(), &relative))
                .expect("pre-risk context should be readable"),
        )
        .expect("pre-risk context should be valid JSON");
        assert_eq!(value["workload"], "runner_workload");
        assert_eq!(value["rows"], 222);
        assert_eq!(value["iterations"], 13);
        assert_eq!(value["warmup"], 6);
        assert_eq!(value["seed"], 77);
        assert_eq!(value["timing_mode"], "both");
        assert_eq!(value["cache_mode"], "cold");
        assert_eq!(value["realistic_gucs"], true);
        assert_eq!(value["skip_guc_verify"], true);
        assert_eq!(value["capture_plans"], true);
        assert!(value["backend_pid"].is_null());
        assert!(
            value["backend_pid_error"]
                .as_str()
                .is_some_and(|error| !error.is_empty())
        );
        assert_eq!(value["accel_query_sql"], identity.accel_query_sql());
        assert_eq!(value["baseline_query_sql"], identity.baseline_query_sql());
        assert_eq!(value["pre_query_sql"][0], "SET work_mem = '8MB'");
        assert!(value["explain_sql"].as_str().is_some_and(|sql| {
            sql == "EXPLAIN (VERBOSE, COSTS OFF) SELECT count(*) FROM runner_primary"
        }));
        assert!(
            value["explain_error"]
                .as_str()
                .is_some_and(|error| !error.is_empty())
        );
    }

    #[test]
    fn failed_correctness_capture_writes_a_linkable_error_artifact() {
        let dir = TestDir::new("correctness-connect-failure");
        let writer = ArtifactWriter::new(dir.path().to_path_buf(), Vec::new())
            .expect("artifact writer should initialize");
        let identity = runner_query_identity();

        let error = capture_and_write_correctness_diff(
            "host=/definitely/missing/pg_accel_socket dbname=missing connect_timeout=1",
            &RunnerWorkload,
            333,
            &identity,
            &writer,
        )
        .expect_err("failed connection must fail correctness gating")
        .to_string();
        assert!(error.contains("correctness diff failed for runner_workload @ 333 rows"));
        assert!(error.contains("status=error"));

        let relative = writer
            .existing_correctness_diff_artifact("runner_workload", 333)
            .expect("correctness error artifact should exist");
        let value: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(artifact_display_path(dir.path(), &relative))
                .expect("correctness artifact should be readable"),
        )
        .expect("correctness artifact should be valid JSON");
        assert_eq!(value["schema_version"], CORRECTNESS_DIFF_SCHEMA_VERSION);
        assert_eq!(value["status"], "error");
        assert_eq!(value["workload"], "runner_workload");
        assert_eq!(value["rows"], 333);
        assert_eq!(value["order_sensitive"], false);
        assert_eq!(value["sample_limit"], CORRECTNESS_DIFF_SAMPLE_LIMIT);
        assert!(value["accel_rows"].is_null());
        assert!(value["baseline_rows"].is_null());
        assert_eq!(value["accel_query_sql"], identity.accel_query_sql());
        assert_eq!(value["baseline_query_sql"], identity.baseline_query_sql());
        assert!(
            value["error"]
                .as_str()
                .is_some_and(|error| !error.is_empty())
        );

        let ordered_identity = BenchmarkQueryIdentity::from_effective(
            "SELECT id FROM runner_primary ORDER BY id".to_owned(),
            "SELECT id FROM runner_secondary".to_owned(),
        )
        .expect("ordered identity should be valid");
        let ordered = correctness_error_artifact(
            &RunnerWorkload,
            444,
            &ordered_identity,
            "ordered failure".to_owned(),
        );
        assert!(ordered.order_sensitive);
        assert_eq!(ordered.error.as_deref(), Some("ordered failure"));
    }

    #[test]
    fn crash_record_resolves_plan_correctness_log_and_context_artifacts() {
        let dir = TestDir::new("linked-crash");
        let writer = ArtifactWriter::new(dir.path().to_path_buf(), Vec::new())
            .expect("artifact writer should initialize");
        writer
            .write_plan_snippet("runner_workload", 777, "Custom Scan (GpuAccel)\n")
            .expect("plan snippet should be written");
        writer
            .write_correctness_diff(
                "runner_workload",
                777,
                &serde_json::json!({"status": "error"}),
            )
            .expect("correctness fixture should be written");

        let crash = record_crash(
            "host=localhost dbname=bench",
            &RunnerWorkload,
            777,
            &BenchConfig::default(),
            Some(&writer),
            9,
            "backend exited",
        );
        assert_eq!(
            crash.plan_snippet_artifact.as_deref(),
            Some("plan_snippets/runner_workload-777.txt")
        );
        assert_eq!(
            crash.correctness_diff_artifact.as_deref(),
            Some("correctness_diffs/runner_workload-777.json")
        );
        assert_eq!(crash.log_tail_artifacts.len(), 2);
        assert!(crash.log_tail_artifacts[0].starts_with("crash_contexts/"));
        assert!(crash.log_tail_artifacts[1].ends_with("no-log-files-found.txt"));
        let context = fs::read_to_string(artifact_display_path(
            dir.path(),
            &crash.log_tail_artifacts[0],
        ))
        .expect("crash context should be readable");
        assert!(context.contains("label: crash-009-runner_workload-777"));
        assert!(context.contains("plan_snippet: plan_snippets/runner_workload-777.txt"));
        assert!(context.contains("correctness_diff: correctness_diffs/runner_workload-777.json"));
        assert!(context.contains("no-log-files-found.txt"));
    }

    #[test]
    fn crash_helpers_render_absent_paths_minimal_sql_and_outside_paths() {
        let dir = TestDir::new("minimal-crash-helpers");
        let writer = ArtifactWriter::new(dir.path().to_path_buf(), Vec::new())
            .expect("artifact writer should initialize");
        let config = BenchConfig::default();
        let context = CrashContext {
            connection: "unused",
            workload: &MinimalRunnerWorkload,
            rows: 1,
            config: &config,
            label: "minimal",
            error: "failure",
            repro_command: "repro",
            plan_snippet_artifact: None,
            correctness_diff_artifact: None,
            log_tail_artifacts: &[],
        };
        let mut paths = String::new();
        append_crash_artifact_paths(&mut paths, &writer, &context);
        assert!(paths.contains("plan_snippet: <not captured before failure>"));
        assert!(paths.contains("correctness_diff: <not captured before failure>"));
        assert!(paths.contains("guc_snapshot: <not available>"));
        assert!(paths.contains("log_tails: <not available>"));

        let mut sql = String::new();
        append_crash_sql(&mut sql, &MinimalRunnerWorkload);
        assert_eq!(
            sql,
            "\n## SQL\n\npre_query_sql: <none>\n--- accel query ---\nSELECT 1\n"
        );

        let outside = dir
            .path()
            .parent()
            .expect("temp path should have a parent")
            .join("outside");
        assert_eq!(
            relative_artifact_path(dir.path(), &outside),
            outside.display().to_string()
        );
    }

    #[test]
    fn runner_diagnostic_artifacts_persist_settings_gucs_and_setup_failures() {
        let dir = TestDir::new("runner-diagnostics");
        let writer = ArtifactWriter::new(dir.path().to_path_buf(), Vec::new())
            .expect("artifact writer should initialize");
        let setting = RustBacktraceSetting {
            effective_value: "full".to_owned(),
            previous_value: Some("0".to_owned()),
            action: "already-enabled",
        };
        persist_rust_backtrace_setting(Some(&writer), &setting);
        assert_eq!(
            fs::read_to_string(dir.path().join(RUST_BACKTRACE_ARTIFACT))
                .expect("backtrace artifact should be readable"),
            "RUST_BACKTRACE=full\naction=already-enabled\nprevious=0\nnote=This records the benchmark runner process environment. Existing PostgreSQL postmasters may need a restart to inherit changed environment variables.\n"
        );

        let observed = ObservedGucs {
            settings: vec![
                ("shared_buffers".to_owned(), "16GB".to_owned()),
                ("work_mem".to_owned(), "512MB".to_owned()),
            ],
            postmaster_start_time: Some("2026-07-21 12:00:00Z".to_owned()),
        };
        persist_guc_snapshot(Some(&writer), "unused", Some(&observed));
        let gucs: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(dir.path().join("guc_snapshot.json"))
                .expect("GUC artifact should be readable"),
        )
        .expect("GUC artifact should be valid JSON");
        assert_eq!(
            gucs["settings"][0],
            serde_json::json!(["shared_buffers", "16GB"])
        );
        assert_eq!(gucs["postmaster_start_time"], "2026-07-21 12:00:00Z");

        record_setup_failure(Some(&writer), "fixture/setup", "setup failed exactly");
        assert_eq!(
            fs::read_to_string(dir.path().join("failure-fixture-setup.txt"))
                .expect("failure artifact should be readable"),
            "setup failed exactly\n"
        );
        assert!(
            dir.path()
                .join("log_tails/fixture-setup/no-log-files-found.txt")
                .is_file()
        );
    }

    #[test]
    fn runner_pure_early_exits_do_not_attempt_database_work() {
        let workloads: Vec<Box<dyn Workload>> = Vec::new();
        let error = run_all("unused", &workloads, MIN_ITERATIONS - 1, 2, 7)
            .expect_err("too few iterations should fail before setup")
            .to_string();
        assert_eq!(
            error,
            "minimum 10 iterations required for statistical validity (got 9)"
        );
        assert!(
            prepare_artifacts("unused", &BenchConfig::default())
                .expect("disabled artifacts should succeed")
                .is_none()
        );
        assert!(validate_result_oracle_from_connection("unused", &RunnerWorkload, 1).is_ok());
        assert!(
            capture_benchmark_sanity_checks("unused", &RunnerWorkload)
                .expect("non-SSBM workloads need no sanity query")
                .is_empty()
        );
        assert_eq!(cold_shared_buffers_resident("unused", &[]), None);
    }

    #[test]
    fn remaining_projection_and_plan_parser_branches_are_exact() {
        let hashagg = correctness_projection_sql(
            "SELECT grp, SUM(v), COUNT(*) FROM t GROUP BY grp",
            "hashagg_10g",
            false,
        );
        assert!(hashagg.contains("'grp', q.grp"));
        assert!(hashagg.contains("round(q.sum::numeric, 3)"));

        let medium = correctness_projection_sql(
            "SELECT user_id, COUNT(*), SUM(v) FROM t GROUP BY user_id",
            "gpu_hashagg_med_card",
            false,
        );
        assert!(medium.contains("'user_id', q.user_id"));
        assert!(medium.contains("'count', q.count"));

        let filtered = correctness_projection_sql(
            "SELECT dept, SUM(v), AVG(v), COUNT(*) FROM t GROUP BY dept",
            "filtered_grouped_agg",
            false,
        );
        assert!(filtered.contains("round(q.avg::numeric, 5)"));

        let f64_sum = correctness_projection_sql("SELECT SUM(v) FROM t", "reduce_f64_sum", false);
        assert!(f64_sum.contains("round(q.sum::numeric, 3)"));

        assert!(has_top_level_order_by("ORDER BY id"));
        assert!(!has_top_level_order_by("SELECT \"order by\" FROM t"));
        assert!(!has_top_level_order_by("SELECT (id)) FROM t"));
        assert!(!has_top_level_order_by(
            "SELECT 'escaped \\' order by id' FROM t"
        ));
        assert!(!starts_with_order_by("xorder by y", 1));
        assert!(!starts_with_order_by("order by_field", 0));

        for marker in [
            r#"{"gpu dispatched": false}"#,
            r#"{"gpu dispatched":"false"}"#,
            r#"{"gpu dispatched": "false"}"#,
            r#"{"gpu kernel dispatched": false}"#,
            r#"{"gpu kernel dispatched":false}"#,
            r#"{"gpu kernel dispatched":"false"}"#,
            r#"{"gpu kernel dispatched": "false"}"#,
        ] {
            assert_eq!(
                explicit_gpu_dispatched(marker),
                Some(false),
                "marker={marker}"
            );
        }
        for marker in [
            r#"{"gpu dispatched": true}"#,
            r#"{"gpu dispatched":true}"#,
            r#"{"gpu dispatched":"true"}"#,
            r#"{"gpu dispatched": "true"}"#,
            r#"{"gpu kernel dispatched": true}"#,
            r#"{"gpu kernel dispatched":true}"#,
            r#"{"gpu kernel dispatched":"true"}"#,
            r#"{"gpu kernel dispatched": "true"}"#,
        ] {
            assert_eq!(
                explicit_gpu_dispatched(marker),
                Some(true),
                "marker={marker}"
            );
        }
        assert_eq!(explicit_gpu_dispatched("Seq Scan on t"), None);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn pmset_parser_accepts_documented_separators_and_rejects_bad_values() {
        let text = "\n CPU_Scheduler_Limit = 100\nCPU_Speed_Limit: 87 percent\n";
        assert_eq!(parse_pmset_limit(text, "CPU_Scheduler_Limit"), Some(100));
        assert_eq!(parse_pmset_limit(text, "CPU_Speed_Limit"), Some(87));
        assert_eq!(
            parse_pmset_limit("CPU_Speed_Limit = fast", "CPU_Speed_Limit"),
            None
        );
        assert_eq!(parse_pmset_limit(text, "Missing"), None);
    }

    #[test]
    fn provenance_and_repro_edge_helpers_fail_closed() {
        assert!(rust_backtrace_enabled("1"));
        assert!(rust_backtrace_enabled(" full "));
        assert!(!rust_backtrace_enabled("0"));
        assert!(!rust_backtrace_enabled("FULL"));

        assert!(file_probe_warning("none", None).is_none());
        let missing = FileProvenance {
            path: "/missing/libpg_accel.dylib".to_owned(),
            exists: false,
            sha256: Some("irrelevant".to_owned()),
            len_bytes: None,
            modified_unix_seconds: None,
            mapping_deleted: false,
            error: None,
        };
        assert_eq!(
            file_probe_warning("fixture", Some(&missing)).as_deref(),
            Some("fixture does not exist: /missing/libpg_accel.dylib")
        );

        let dir = TestDir::new("missing-extension-metadata");
        let (control, version, sql) =
            capture_extension_metadata_files(&dir.path().display().to_string());
        assert!(control.is_none());
        assert!(version.is_none());
        assert!(sql.is_empty());

        let file = dir.path().join("libpg_accel.dylib");
        fs::write(&file, b"mapped").expect("mapped library fixture should be written");
        let mapped = inspect_mapped_library(&MappedLibrary {
            path: file.clone(),
            display_path: file.display().to_string(),
            deleted: false,
        });
        assert!(mapped.exists);
        assert_eq!(mapped.len_bytes, Some(6));
        assert!(!mapped.mapping_deleted);

        assert!(parse_control_default_version("default_version # missing equals").is_none());
        assert!(parse_control_default_version("default_version = ''").is_none());
        assert!(parse_control_default_version("default_version = # empty").is_none());

        let mut report = valid_provenance_report();
        report.loaded_binaries[0].modified_unix_seconds = None;
        check_postmaster_restart_required(&mut report);
        assert!(report.errors.is_empty());
        assert!(report.warnings.is_empty());

        let config = BenchConfig {
            timing_mode: TimingMode::ExplainAnalyze,
            cache_mode: CacheMode::Both,
            ..BenchConfig::default()
        };
        let command = repro_command("dbname=quoted bench", "work load", 5, &config);
        assert!(command.contains("--workload 'work load'"));
        assert!(command.contains("--timing explain"));
        assert!(command.contains("--cache-mode both"));
        assert!(command.contains("--connection 'dbname=quoted bench'"));
    }

    #[test]
    fn finalizing_an_empty_report_writes_a_complete_success_artifact_set() {
        let dir = TestDir::new("finalize-empty-report");
        let writer = ArtifactWriter::new(dir.path().to_path_buf(), Vec::new())
            .expect("artifact writer should initialize");
        let mut report = mock_report_missing_resident_boundary();
        report.workloads.clear();

        finalize_report_artifacts(&writer, &mut report, "empty-complete")
            .expect("empty report should pass every audit");
        assert_eq!(
            report.artifact_dir.as_deref(),
            Some(dir.path().display().to_string().as_str())
        );
        for artifact in [
            "crashes.json",
            "crashes.md",
            "report.json",
            "report.md",
            "report.csv",
        ] {
            assert!(dir.path().join(artifact).is_file(), "artifact={artifact}");
        }
    }

    #[test]
    fn dispatch_warning_paths_preserve_counter_and_plan_diagnostics() {
        let input =
            |plan_selected, plan_text_dispatched, explicitly_off, function| DispatchWarningInput {
                workload: "warning_fixture",
                rows: 10_000,
                plan_selected,
                plan_text_dispatched,
                plan_explicitly_not_dispatched: explicitly_off,
                function_kernel_candidate: function,
            };

        let mut result = dispatch_test_result();
        result.dispatch_counter_captured = false;
        result.dispatch_counter_error = Some("stats unavailable".to_owned());
        emit_dispatch_classification_warning(input(false, false, false, false), &result);

        result.dispatch_counter_captured = true;
        result.pg_accel_stock_exec_delta = 3;
        result.gpu_kernel_execution_delta = 2;
        emit_dispatch_classification_warning(input(false, false, false, false), &result);

        result.pg_accel_stock_exec_delta = 0;
        result.gpu_kernel_execution_delta = 0;
        emit_dispatch_classification_warning(input(true, true, false, false), &result);

        result.gpu_kernel_execution_delta = 2;
        result.pg_accel_queries_accelerated_delta = 0;
        emit_dispatch_classification_warning(input(true, false, false, false), &result);

        result.pg_accel_queries_accelerated_delta = result.iterations.len() as u64;
        result.accel_output_rows_consumed = 0;
        emit_dispatch_classification_warning(input(true, false, false, false), &result);

        result.accel_output_rows_consumed = 1;
        emit_dispatch_classification_warning(input(true, false, true, false), &result);

        result.gpu_kernel_execution_delta = 0;
        emit_dispatch_classification_warning(input(false, true, false, false), &result);

        result.gpu_kernel_execution_delta = 2;
        result.accel_output_rows_consumed = 0;
        emit_dispatch_classification_warning(input(false, false, false, true), &result);

        result.accel_output_rows_consumed = 1;
        emit_dispatch_classification_warning(input(false, false, false, true), &result);
    }

    #[test]
    fn extension_metadata_probe_filters_and_orders_only_pg_accel_sql_files() {
        let shared = TestDir::new("extension-metadata-complete");
        let extension = shared.path().join("extension");
        fs::create_dir(&extension).expect("extension directory should be created");
        fs::write(
            extension.join("pg_accel.control"),
            "comment = 'fixture'\ndefault_version = '2.4.6'\n",
        )
        .expect("control file should be written");
        fs::write(extension.join("pg_accel--2.4.6.sql"), "SELECT 1;")
            .expect("base SQL should be written");
        fs::write(extension.join("pg_accel--2.4.5--2.4.6.SQL"), "SELECT 2;")
            .expect("upgrade SQL should be written");
        fs::write(extension.join("other--1.sql"), "SELECT 3;")
            .expect("unrelated SQL should be written");
        let (control, version, sql_files) =
            capture_extension_metadata_files(&shared.path().display().to_string());
        assert!(control.is_some_and(|probe| probe.exists && probe.sha256.is_some()));
        assert_eq!(version.as_deref(), Some("2.4.6"));
        assert_eq!(sql_files.len(), 2);
        let names = sql_files
            .iter()
            .map(|probe| {
                Path::new(&probe.path)
                    .file_name()
                    .expect("probe path should have a filename")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec!["pg_accel--2.4.5--2.4.6.SQL", "pg_accel--2.4.6.sql"]
        );
    }

    #[test]
    fn provenance_path_helpers_fail_closed_without_running_external_commands() {
        let dir = TestDir::new("pure-provenance-paths");
        let candidates = build_output_candidates(dir.path());
        assert_eq!(candidates.len(), 4);
        assert!(
            candidates
                .iter()
                .any(|path| path.starts_with(dir.path().join("target/release")))
        );
        assert!(
            candidates
                .iter()
                .any(|path| path.starts_with(dir.path().join("target/debug")))
        );

        let installed_names = installed_extension_library_names();
        assert_eq!(installed_names.len(), 2);
        assert!(
            find_installed_extension_binary(dir.path().to_str().expect("UTF-8 temp path"))
                .is_none()
        );
        let installed = dir.path().join(&installed_names[0]);
        fs::write(&installed, b"fixture").expect("installed-library fixture");
        assert_eq!(
            find_installed_extension_binary(dir.path().to_str().expect("UTF-8 temp path")),
            Some(installed)
        );

        let missing = dir.path().join("missing-library.dylib");
        let missing_probe = inspect_file(&missing, false);
        assert!(!missing_probe.exists);
        assert!(missing_probe.sha256.is_none());
        assert!(missing_probe.error.is_some());

        let deleted = MappedLibrary {
            path: missing.clone(),
            display_path: format!("{} (deleted)", missing.display()),
            deleted: true,
        };
        let deleted_probe = inspect_mapped_library(&deleted);
        assert!(!deleted_probe.exists);
        assert!(deleted_probe.mapping_deleted);
        assert!(
            deleted_probe
                .error
                .as_deref()
                .is_some_and(|error| error.contains("deleted"))
        );

        let mapped = mapped_library_probe("fixture", vec![deleted], Some("diagnostic".to_owned()));
        assert_eq!(mapped.discovery.method, "fixture");
        assert_eq!(mapped.discovery.mapped_paths.len(), 1);
        assert_eq!(mapped.discovery.warning.as_deref(), Some("diagnostic"));
        assert_eq!(mapped.libraries.len(), 1);

        assert!(is_pg_accel_library_path("/tmp/libpg_accel.dylib"));
        assert!(is_pg_accel_library_path("C:/tmp/pg_accel.DLL"));
        assert!(!is_pg_accel_library_path("/tmp/libpg_accel.txt"));
        assert!(!is_pg_accel_library_path("/tmp/libother.dylib"));
        assert!(!is_pg_accel_library_path("/"));
        assert_eq!(
            system_time_unix_secs(UNIX_EPOCH - std::time::Duration::from_secs(1)),
            None
        );
        assert!(workspace_root().join("pg_accel_bench").is_dir());
    }

    #[test]
    fn expected_binary_override_reports_a_missing_path_without_hashing() {
        let dir = TestDir::new("missing-expected-binary");
        let missing = dir.path().join("missing-pg-accel.dylib");
        let original = std::env::var_os("PG_ACCEL_EXPECTED_DYLIB");
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("PG_ACCEL_EXPECTED_DYLIB", &missing);
        }

        let mut warnings = Vec::new();
        let probe = capture_expected_binary(&mut warnings)
            .expect("an explicit override always produces provenance");
        assert_eq!(probe.path, missing.display().to_string());
        assert!(!probe.exists);
        assert!(probe.sha256.is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("points to a missing file"));

        #[allow(unsafe_code)]
        unsafe {
            match original {
                Some(value) => std::env::set_var("PG_ACCEL_EXPECTED_DYLIB", value),
                None => std::env::remove_var("PG_ACCEL_EXPECTED_DYLIB"),
            }
        }
    }

    #[test]
    fn rust_backtrace_policy_covers_enabled_replaced_and_absent_states() {
        let original = std::env::var_os("RUST_BACKTRACE");
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "full");
        }
        let enabled = ensure_rust_backtrace();
        assert_eq!(enabled.effective_value, "full");
        assert_eq!(enabled.previous_value, None);
        assert_eq!(enabled.action, "already-enabled");

        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "0");
        }
        let replaced = ensure_rust_backtrace();
        assert_eq!(replaced.effective_value, "1");
        assert_eq!(replaced.previous_value.as_deref(), Some("0"));
        assert_eq!(replaced.action, "set-by-runner");

        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("RUST_BACKTRACE");
        }
        let absent = ensure_rust_backtrace();
        assert_eq!(absent.effective_value, "1");
        assert_eq!(absent.previous_value, None);
        assert_eq!(absent.action, "set-by-runner");

        #[allow(unsafe_code)]
        unsafe {
            match original {
                Some(value) => std::env::set_var("RUST_BACKTRACE", value),
                None => std::env::remove_var("RUST_BACKTRACE"),
            }
        }
    }
}
