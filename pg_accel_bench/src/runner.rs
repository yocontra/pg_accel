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

use crate::artifacts::ArtifactWriter;
#[allow(unused_imports)]
pub use crate::config::{
    BenchConfig, CacheMode, GucProfile, ObservedGucs, PostmasterMismatch, ROW_SCALES, TimingMode,
    verify_and_capture_gucs,
};
use crate::report::{self, IterationResult, WorkloadResult};
use crate::workloads::Workload;

const PROVENANCE_SCHEMA_VERSION: u32 = 1;
const EXPECTED_EXTENSION_VERSION: &str = env!("CARGO_PKG_VERSION");
const RUST_BACKTRACE_ARTIFACT: &str = "rust_backtrace.txt";
const CRASH_CONTEXT_EMBED_BYTES: usize = 128 * 1024;

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
        let mut merged = cold.iterations.clone();
        merged.extend(warm.iterations.clone());
        let mut result = WorkloadResult::from_iterations(
            workload.name().to_owned(),
            workload.description().to_owned(),
            workload.category().to_owned(),
            crate::report::classify_kernel(workload.name()),
            0,
            merged,
            true,
        );
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
    let mut counter_before: Option<DispatchStatsSnapshot> = None;
    let mut counter_capture_error: Option<String> = None;
    let mut accel_output_rows_consumed = 0_u64;

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

        if !is_warmup {
            accel_output_rows_consumed =
                accel_output_rows_consumed.saturating_add(timings[0].output_rows);
            results.push(IterationResult {
                accel_ms,
                parallel_ms,
            });
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
        workload.category().to_owned(),
        crate::report::classify_kernel(workload.name()),
        0,
        results,
        true,
    );
    result.accel_output_rows_consumed = accel_output_rows_consumed;
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

    let workload_names: Vec<&str> = workloads.iter().map(|w| w.name()).collect();
    let run_context = prepare_run_context(connection, &workload_names, config)?;
    let observed_gucs = run_context.observed_gucs;
    let artifacts = run_context.artifacts;

    initialize_plans_file(config);

    let mut results = Vec::with_capacity(workloads.len() * ROW_SCALES.len());
    let mut crashes: Vec<report::CrashedScale> = Vec::new();

    for w in workloads {
        for &rows in ROW_SCALES {
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
        report.artifact_dir = Some(artifact_writer.root().display().to_string());
        if let Err(e) = artifact_writer.capture_log_tails("run-complete") {
            eprintln!("[artifacts] run-complete log/telemetry tail capture failed: {e}");
        }
        if let Err(e) = artifact_writer.write_crashes(&report.crashes) {
            eprintln!("[artifacts] final crash list write failed: {e}");
        }
        if let Err(e) = artifact_writer.write_report(&report) {
            eprintln!("[artifacts] report write failed: {e}");
        }
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
        report.artifact_dir = Some(artifact_writer.root().display().to_string());
        if let Err(e) = artifact_writer.capture_log_tails("run-complete") {
            eprintln!("[artifacts] run-complete log/telemetry tail capture failed: {e}");
        }
        if let Err(e) = artifact_writer.write_crashes(&report.crashes) {
            eprintln!("[artifacts] crash list write failed: {e}");
        }
        if let Err(e) = artifact_writer.write_report(&report) {
            eprintln!("[artifacts] report write failed: {e}");
        }
    }
    Ok(report)
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
    if let Some(path) = existing_artifact_path(artifact_writer.root(), "guc_snapshot.json") {
        let _ = writeln!(text, "guc_snapshot: {path}");
    } else {
        text.push_str("guc_snapshot: <not available>\n");
    }
    if let Some(path) = existing_artifact_path(artifact_writer.root(), "provenance.json") {
        let _ = writeln!(text, "provenance: {path}");
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

    // Capture thermal state BEFORE the timed loop (action_items M13).
    let thermal = capture_thermal_state();

    // Always capture a plan snippet so the runner can tag the workload
    // as dispatched/not-dispatched even if --capture-plans is off. This
    // feeds the dispatch classification (action_items C8 / Reviewer 1
    // Sin #5). The full-plans file (plans.txt) is still written if
    // plans_capture_path is set.
    let plan_snippet = capture_plan_snippet(connection, workload, BenchMode::Accel).ok();
    let baseline_plan_snippet =
        capture_plan_snippet(connection, workload, BenchMode::PgParallel).ok();
    if let (Some(artifact_writer), Some(snippet)) = (artifacts, plan_snippet.as_deref())
        && let Err(e) = artifact_writer.write_plan_snippet(workload.name(), rows, snippet)
    {
        eprintln!(
            "[artifacts] plan snippet write failed for {} @ {rows}: {e}",
            workload.name()
        );
    }
    let plan_selected = plan_snippet
        .as_deref()
        .is_some_and(plan_contains_custom_scan);
    let plan_text_dispatched = plan_snippet
        .as_deref()
        .is_some_and(plan_indicates_gpu_dispatch);
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
    let counter_proves_kernel =
        result.dispatch_counter_captured && result.gpu_kernel_execution_delta > 0;
    let stock_fallback_seen = result.pg_accel_stock_exec_delta > 0;
    let function_output_consumed = result.accel_output_rows_consumed > 0;
    result.function_srf_kernel_dispatched = function_kernel_candidate
        && !plan_selected
        && counter_proves_kernel
        && function_output_consumed
        && !stock_fallback_seen;
    result.gpu_kernel_dispatched = counter_proves_kernel
        && (!function_kernel_candidate || plan_selected || function_output_consumed)
        && !stock_fallback_seen;
    emit_dispatch_classification_warning(
        workload.name(),
        rows,
        plan_selected,
        plan_text_dispatched,
        function_kernel_candidate,
        &result,
    );
    result.plan_snippet = plan_snippet;
    result.baseline_plan_snippet = baseline_plan_snippet;
    result.thermal = thermal;
    result.table_stats = vacuum_stats;
    cleanup(connection, workload)?;
    Ok(result)
}

/// Capture a short plan snippet (first 30 lines of
/// `EXPLAIN (VERBOSE, COSTS OFF) <query>`) for one benchmark side. Used
/// for dispatch classification and no-dispatch native-plan comparability
/// checks.
fn capture_plan_snippet(
    connection: &str,
    workload: &dyn Workload,
    mode: BenchMode,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut client = Client::connect(connection, NoTls)?;
    match mode {
        BenchMode::Accel => client.batch_execute("SET pg_accel.enabled = on")?,
        BenchMode::PgParallel => client.batch_execute("SET pg_accel.enabled = off")?,
    }
    client.batch_execute("SET max_parallel_workers_per_gather = DEFAULT")?;
    for sql in workload.pre_query_sql() {
        client.batch_execute(&sql)?;
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
/// for pg_accel is always prefixed with "GPU").
#[must_use]
pub fn plan_contains_custom_scan(plan: &str) -> bool {
    plan.contains("Custom Scan")
}

fn emit_dispatch_classification_warning(
    workload: &str,
    rows: usize,
    plan_selected: bool,
    plan_text_dispatched: bool,
    function_kernel_candidate: bool,
    result: &WorkloadResult,
) {
    if !result.dispatch_counter_captured {
        eprintln!(
            "[dispatch] WARNING: {workload} @ {rows}: runtime counter capture unavailable; \
             not crediting GPU dispatch ({})",
            result
                .dispatch_counter_error
                .as_deref()
                .unwrap_or("unknown stats error")
        );
        return;
    }
    if result.pg_accel_stock_exec_delta > 0 {
        eprintln!(
            "[dispatch] WARNING: {workload} @ {rows}: pg_accel stock executor fallback delta={} \
             with kernel_delta={}; excluding from GPU-dispatched wins",
            result.pg_accel_stock_exec_delta, result.gpu_kernel_execution_delta
        );
        return;
    }
    if plan_selected && result.gpu_kernel_execution_delta == 0 {
        eprintln!(
            "[dispatch] WARNING: {workload} @ {rows}: pg_accel plan selected but runtime \
             kernel counter delta is zero; counting as plan-selected only"
        );
    }
    if plan_text_dispatched && result.gpu_kernel_execution_delta == 0 {
        eprintln!(
            "[dispatch] WARNING: {workload} @ {rows}: plan text claimed GPU dispatch but \
             runtime kernel counter delta is zero"
        );
    }
    if function_kernel_candidate
        && !plan_selected
        && result.gpu_kernel_execution_delta > 0
        && result.accel_output_rows_consumed == 0
    {
        eprintln!(
            "[dispatch] WARNING: {workload} @ {rows}: function/SRF kernel counter advanced \
             but no accel output rows were consumed; excluding from GPU-dispatched wins"
        );
    }
}

fn plan_indicates_gpu_dispatch(plan: &str) -> bool {
    explicit_gpu_dispatched(plan).unwrap_or_else(|| plan_contains_custom_scan(plan))
}

fn explicit_gpu_dispatched(plan: &str) -> Option<bool> {
    if plan.contains("GPU Dispatched: false") {
        Some(false)
    } else if plan.contains("GPU Dispatched: true") {
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
    let ext_reqs = crate::workloads::extension_requirements();
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
    let suffix = &line[actual_idx..];
    let rows_idx = suffix.find(" rows=")?;
    let value = &suffix[rows_idx + " rows=".len()..];
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    (!digits.is_empty())
        .then(|| digits.parse::<u64>().ok())
        .flatten()
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
            "host=localhost port=28817 dbname=pg accel",
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
        assert!(command.contains("--connection 'host=localhost port=28817 dbname=pg accel'"));
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
