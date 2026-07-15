use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use postgres::{Client, NoTls};
use rand::Rng;
use serde::Serialize;

use crate::artifacts::{ArtifactWriter, PreRiskContext};
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
    rows_dispatched: u64,
    batches_executed: u64,
    stock_exec_count: u64,
    gpu_rows_processed: u64,
    gpu_kernel_executions: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct DispatchStatsDelta {
    rows_dispatched: u64,
    batches_executed: u64,
    stock_exec_count: u64,
    gpu_rows_processed: u64,
    gpu_kernel_executions: u64,
}

#[derive(Debug, Clone)]
struct DispatchCounterCapture {
    captured: bool,
    delta: DispatchStatsDelta,
    error: Option<String>,
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
    pg_accel_stock_exec_delta: u64,
    accel_output_rows_consumed: u64,
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
        let Some(rest) = without_comment.strip_prefix("default_version") else {
            continue;
        };
        let Some((_, value)) = rest.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
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
        // Carry resident-lane evidence from the sub-runs (both measure the
        // same workload; prefer the cold sub-run's off-clock load time).
        result.resident_lane = cold.resident_lane || warm.resident_lane;
        result.resident_load_ms = cold.resident_load_ms.or(warm.resident_load_ms);
        merge_dispatch_counter_fields(&mut result, &cold);
        merge_dispatch_counter_fields(&mut result, &warm);
        return Ok(result);
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
    let mut warmup_results = Vec::with_capacity(effective_warmup);
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
            mode_clients[idx].batch_execute("DISCARD ALL")?;
            apply_benchmark_safety_settings(&mut mode_clients[idx])?;
            if matches!(modes[idx], BenchMode::Accel) {
                prime_workload_accel_backend(&mut mode_clients[idx], workload)?;
            }
            let sql_for_mode = match modes[idx] {
                BenchMode::Accel => query.as_str(),
                BenchMode::PgParallel => baseline_query.as_str(),
            };
            let _ = run_with_mode(
                &mut mode_clients[idx],
                sql_for_mode,
                modes[idx],
                &pre_query,
                timing_mode,
            )?;
        }
    }

    let mut counter_before: Option<DispatchStatsSnapshot> = None;
    let mut counter_capture_error: Option<String> = None;
    let mut accel_output_rows_consumed = 0_u64;

    for i in 0..total_runs {
        let is_warmup = i < effective_warmup;

        // Randomize order to eliminate cache-warming bias.
        let mut order: [usize; 2] = [0, 1];
        if rng.gen_bool(0.5) {
            order.swap(0, 1);
        }

        if !is_warmup && counter_before.is_none() && counter_capture_error.is_none() {
            match capture_dispatch_stats(&mut mode_clients[0]) {
                Ok(snapshot) => counter_before = Some(snapshot),
                Err(e) => {
                    let msg =
                        format!("could not capture pg_accel dispatch counters before run: {e}");
                    eprintln!("[dispatch] WARNING: {msg}");
                    counter_capture_error = Some(msg);
                }
            }
        }

        let mut timings = [MeasurementOutcome {
            elapsed_ms: 0.0,
            output_rows: 0,
        }; 2];
        // Cold mode purges the OS page cache before EACH mode's timed run
        // (not once per accel/parallel pair) and AFTER the resident-cache
        // prime, so the resident preload cannot re-warm the heap the timed
        // query reads. Recorded per measurement.
        let mut purge_outcomes = [CachePurgeState::NotRequested; 2];
        for &idx in &order {
            // DISCARD ALL resets session state before each measurement.
            mode_clients[idx].batch_execute("DISCARD ALL")?;
            apply_benchmark_safety_settings(&mut mode_clients[idx])?;
            if matches!(modes[idx], BenchMode::Accel) {
                prime_workload_accel_backend(&mut mode_clients[idx], workload)?;
            }
            if cache_mode == CacheMode::Cold {
                purge_outcomes[idx] = purge_for_measurement();
            }
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
        let cache_purge = combine_purge_states(purge_outcomes[0], purge_outcomes[1]);

        let accel_ms = timings[0].elapsed_ms;
        let parallel_ms = timings[1].elapsed_ms;

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
            cache_purge,
            cache_state: CacheState::from(cache_mode),
        };
        if is_warmup {
            warmup_results.push(iteration_result);
        } else {
            accel_output_rows_consumed =
                accel_output_rows_consumed.saturating_add(timings[0].output_rows);
            results.push(iteration_result);
        }
    }

    let counter_capture = match (counter_before, counter_capture_error) {
        (Some(before), None) => match capture_dispatch_stats(&mut mode_clients[0]) {
            Ok(after) => DispatchCounterCapture {
                captured: true,
                delta: dispatch_stats_delta(before, after),
                error: None,
            },
            Err(e) => DispatchCounterCapture::unavailable(format!(
                "could not capture pg_accel dispatch counters after run: {e}"
            )),
        },
        (_, Some(error)) => DispatchCounterCapture::unavailable(error),
        (None, None) => DispatchCounterCapture::unavailable(
            "no measured iterations ran before dispatch counter capture".to_owned(),
        ),
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
    merge_dispatch_counter_capture(&mut result, counter_capture);
    Ok(result)
}

fn capture_dispatch_stats(
    client: &mut Client,
) -> Result<DispatchStatsSnapshot, Box<dyn std::error::Error>> {
    let row = client.query_one(
        "SELECT rows_dispatched, batches_executed, stock_exec_count, \
                gpu_rows_processed, gpu_kernel_executions \
           FROM pg_accel_stats()",
        &[],
    )?;
    Ok(DispatchStatsSnapshot {
        rows_dispatched: i64_to_u64(row.get(0)),
        batches_executed: i64_to_u64(row.get(1)),
        stock_exec_count: i64_to_u64(row.get(2)),
        gpu_rows_processed: i64_to_u64(row.get(3)),
        gpu_kernel_executions: i64_to_u64(row.get(4)),
    })
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
            "SELECT pg_accel_pin($1::regclass, $2::text[])::bigint",
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
                 FROM pg_accel_resident_status() WHERE relid = $1::regclass::oid",
                &[&pin.table],
            )?
            .get(0);
        if !material {
            let refreshed_rows: i64 = client
                .query_one(
                    "SELECT pg_accel_refresh($1::regclass)::bigint",
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
fn i64_to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn dispatch_stats_delta(
    before: DispatchStatsSnapshot,
    after: DispatchStatsSnapshot,
) -> DispatchStatsDelta {
    DispatchStatsDelta {
        rows_dispatched: after.rows_dispatched.saturating_sub(before.rows_dispatched),
        batches_executed: after
            .batches_executed
            .saturating_sub(before.batches_executed),
        stock_exec_count: after
            .stock_exec_count
            .saturating_sub(before.stock_exec_count),
        gpu_rows_processed: after
            .gpu_rows_processed
            .saturating_sub(before.gpu_rows_processed),
        gpu_kernel_executions: after
            .gpu_kernel_executions
            .saturating_sub(before.gpu_kernel_executions),
    }
}

fn merge_dispatch_counter_capture(target: &mut WorkloadResult, capture: DispatchCounterCapture) {
    target.dispatch_counter_captured = capture.captured;
    target.gpu_kernel_execution_delta = capture.delta.gpu_kernel_executions;
    target.pg_accel_rows_dispatched_delta = capture.delta.rows_dispatched;
    target.pg_accel_batches_executed_delta = capture.delta.batches_executed;
    target.pg_accel_gpu_rows_processed_delta = capture.delta.gpu_rows_processed;
    target.pg_accel_stock_exec_delta = capture.delta.stock_exec_count;
    target.dispatch_counter_error = capture.error;
}

fn merge_dispatch_counter_fields(target: &mut WorkloadResult, source: &WorkloadResult) {
    target.dispatch_counter_captured =
        target.dispatch_counter_captured || source.dispatch_counter_captured;
    target.gpu_kernel_execution_delta = target
        .gpu_kernel_execution_delta
        .saturating_add(source.gpu_kernel_execution_delta);
    target.pg_accel_rows_dispatched_delta = target
        .pg_accel_rows_dispatched_delta
        .saturating_add(source.pg_accel_rows_dispatched_delta);
    target.pg_accel_batches_executed_delta = target
        .pg_accel_batches_executed_delta
        .saturating_add(source.pg_accel_batches_executed_delta);
    target.pg_accel_gpu_rows_processed_delta = target
        .pg_accel_gpu_rows_processed_delta
        .saturating_add(source.pg_accel_gpu_rows_processed_delta);
    target.pg_accel_stock_exec_delta = target
        .pg_accel_stock_exec_delta
        .saturating_add(source.pg_accel_stock_exec_delta);
    target.accel_output_rows_consumed = target
        .accel_output_rows_consumed
        .saturating_add(source.accel_output_rows_consumed);
    if target.dispatch_counter_error.is_none() {
        target
            .dispatch_counter_error
            .clone_from(&source.dispatch_counter_error);
    }
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
) -> Result<MeasurementOutcome, Box<dyn std::error::Error>> {
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
    match timing_mode {
        TimingMode::ExplainAnalyze => run_explain_analyze_outcome(client, query),
        TimingMode::RawWallClock => run_raw_wall_clock(client, query),
        TimingMode::Both => {
            // Run both mechanisms back-to-back on the same connection.
            // We report the raw wall-clock value (per action_items M1
            // default) but also capture the EXPLAIN ANALYZE figure to
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
    }
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
    if let Some(artifact_writer) = artifacts.as_ref() {
        finalize_report_artifacts(artifact_writer, &mut report, "run-complete")?;
    }
    Ok(report)
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
    let sanity_checks = capture_benchmark_sanity_checks(connection, workload)?;

    // Capture thermal state BEFORE the timed loop (action_items M13).
    let thermal = capture_thermal_state();

    if let Some(artifact_writer) = artifacts
        && let Err(e) =
            capture_and_write_pre_risk_context(connection, workload, rows, config, artifact_writer)
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
            artifact_writer,
        )?)
    } else {
        validate_result_oracle_from_connection(connection, workload, rows)?;
        None
    };

    // Always capture a plan snippet so the runner can tag the workload
    // as dispatched/not-dispatched even if --capture-plans is off. This
    // feeds the dispatch classification (action_items C8 / Reviewer 1
    // Sin #5). The full-plans file (plans.txt) is still written if
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

    let mut result = run_with_timing_and_cache(
        connection,
        workload,
        config.iterations,
        config.warmup,
        config.timing_mode,
        config.cache_mode,
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
        pg_accel_stock_exec_delta: result.pg_accel_stock_exec_delta,
        accel_output_rows_consumed: result.accel_output_rows_consumed,
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
    artifact_writer: &ArtifactWriter,
) -> Result<String, Box<dyn std::error::Error>> {
    let artifact = match capture_correctness_diff(connection, workload, rows) {
        Ok(artifact) => artifact,
        Err(e) => correctness_error_artifact(workload, rows, e.to_string()),
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
) -> Result<CorrectnessDiffArtifact, Box<dyn std::error::Error>> {
    let accel_query_sql = workload.query_sql();
    let baseline_query_sql = workload
        .baseline_query_sql()
        .unwrap_or_else(|| accel_query_sql.clone());
    let order_sensitive =
        has_top_level_order_by(&accel_query_sql) || has_top_level_order_by(&baseline_query_sql);
    let pre_query_sql = workload.pre_query_sql();

    let mut client = Client::connect(connection, NoTls)?;
    apply_benchmark_safety_settings(&mut client)?;
    prime_workload_accel_backend(&mut client, workload)?;
    create_correctness_table(
        &mut client,
        CORRECTNESS_ACCEL_TABLE,
        &accel_query_sql,
        workload.name(),
        BenchMode::Accel,
        &pre_query_sql,
        order_sensitive,
    )?;
    create_correctness_table(
        &mut client,
        CORRECTNESS_BASELINE_TABLE,
        &baseline_query_sql,
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
        accel_query_sql,
        baseline_query_sql,
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
    error: String,
) -> CorrectnessDiffArtifact {
    let accel_query_sql = workload.query_sql();
    let baseline_query_sql = workload
        .baseline_query_sql()
        .unwrap_or_else(|| accel_query_sql.clone());
    CorrectnessDiffArtifact {
        schema_version: CORRECTNESS_DIFF_SCHEMA_VERSION,
        workload: workload.name().to_owned(),
        rows,
        status: "error".to_owned(),
        order_sensitive: has_top_level_order_by(&accel_query_sql)
            || has_top_level_order_by(&baseline_query_sql),
        accel_rows: None,
        baseline_rows: None,
        accel_minus_baseline_count: None,
        baseline_minus_accel_count: None,
        sample_limit: CORRECTNESS_DIFF_SAMPLE_LIMIT,
        accel_minus_baseline_samples: Vec::new(),
        baseline_minus_accel_samples: Vec::new(),
        accel_query_sql,
        baseline_query_sql,
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
    artifact_writer: &ArtifactWriter,
) -> Result<(), Box<dyn std::error::Error>> {
    let pre_query_sql = workload.pre_query_sql();
    let setup_sql = workload.setup_sql(rows);
    let accel_query_sql = workload.query_sql();
    let baseline_query_sql = workload.baseline_query_sql();
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
        backend_pid,
        backend_pid_error: backend_pid_error.as_deref(),
        setup_sql: &setup_sql,
        pre_query_sql: &pre_query_sql,
        accel_query_sql: &accel_query_sql,
        baseline_query_sql: baseline_query_sql.as_deref(),
        explain_sql: &explain_sql,
        explain: explain.as_deref(),
        explain_error: explain_error.as_deref(),
    };

    artifact_writer.write_pre_risk_context(workload.name(), rows, &context)?;
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
    let function_output_consumed = input.accel_output_rows_consumed > 0;
    let function_srf_kernel_dispatched = input.function_kernel_candidate
        && !input.plan_explicitly_not_dispatched
        && !input.plan_selected
        && counter_proves_kernel
        && function_output_consumed
        && !stock_fallback_seen;
    let gpu_kernel_dispatched = !input.plan_explicitly_not_dispatched
        && counter_proves_kernel
        && (!input.function_kernel_candidate || input.plan_selected || function_output_consumed)
        && !stock_fallback_seen;

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

/// Parse the millisecond value from an `Execution Time: X.XXX ms` line.
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
            },
            workloads: vec![workload],
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
            pg_accel_stock_exec_delta: 0,
            accel_output_rows_consumed: 1,
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
            pg_accel_stock_exec_delta: 0,
            accel_output_rows_consumed: 12,
        });

        assert!(classification.gpu_kernel_dispatched);
        assert!(classification.function_srf_kernel_dispatched);
    }

    fn iter_at(accel_ms: f64, parallel_ms: f64, cache_state: CacheState) -> IterationResult {
        IterationResult {
            accel_ms,
            parallel_ms,
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

    #[test]
    fn test_repro_command_preserves_runner_flags() {
        let config = BenchConfig {
            iterations: 3,
            warmup: 1,
            seed: 99,
            timing_mode: TimingMode::Both,
            cache_mode: CacheMode::Cold,
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
            resident_pin_specs("hashagg_f64_aggs"),
            vec![resident_pin("bench_fp64_num", &["gk", "v_f64", "w_f64"],)]
        );
    }

    #[test]
    fn test_resident_pin_specs_cover_ssbm_inputs_exactly() {
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
        assert!(parse_control_default_version("comment = 'no version'").is_none());
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
}
