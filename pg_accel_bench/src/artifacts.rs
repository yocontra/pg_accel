use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::report::{BenchReport, CrashedScale, GucSettings};

const ARTIFACT_SCHEMA_VERSION: u32 = 2;
const ARTIFACT_INDEX_SCHEMA_VERSION: u32 = 1;
const RESUME_AUDIT_MANIFEST_SCHEMA_VERSION: u32 = 1;
const ARTIFACT_INDEX_JSON: &str = "artifact_index.json";
const ARTIFACT_CHECKLIST_MD: &str = "artifact_checklist.md";
const RUN_MANIFEST_JSON: &str = "manifest.json";
const RESUME_AUDIT_MANIFEST_JSON: &str = "resume_audit_manifest.json";
const NO_DISPATCH_AUDIT_JSON: &str = "no_dispatch_audit.json";
const NO_DISPATCH_AUDIT_MD: &str = "no_dispatch_audit.md";
const RESIDENT_BOUNDARY_AUDIT_JSON: &str = "resident_boundary_audit.json";
const RESIDENT_BOUNDARY_AUDIT_MD: &str = "resident_boundary_audit.md";
const BENCHMARK_FAILURE_LEDGER_JSON: &str = "benchmark_failure_ledger.json";
const BENCHMARK_FAILURE_LEDGER_MD: &str = "benchmark_failure_ledger.md";
const LOG_TAIL_BYTES: u64 = 64 * 1024;
const LOG_DELTA_BYTES: u64 = 256 * 1024;
const MAX_LOG_CANDIDATES: usize = 32;
const MANAGED_ARTIFACT_DIRS: &[&str] = &[
    "plan_snippets",
    "log_tails",
    "crash_contexts",
    "correctness_diffs",
    "pre_risk_contexts",
];
const MANAGED_ARTIFACT_FILES: &[&str] = &[
    ARTIFACT_INDEX_JSON,
    ARTIFACT_CHECKLIST_MD,
    RUN_MANIFEST_JSON,
    RESUME_AUDIT_MANIFEST_JSON,
    NO_DISPATCH_AUDIT_JSON,
    NO_DISPATCH_AUDIT_MD,
    RESIDENT_BOUNDARY_AUDIT_JSON,
    RESIDENT_BOUNDARY_AUDIT_MD,
    BENCHMARK_FAILURE_LEDGER_JSON,
    BENCHMARK_FAILURE_LEDGER_MD,
    "README.md",
    "crashes.json",
    "crashes.md",
    "guc_snapshot.json",
    "report.json",
    "report.md",
    "report.csv",
    "provenance.json",
    "provenance-warnings.txt",
    "rust_backtrace.txt",
    "plans.txt",
];

#[derive(Clone, Debug)]
pub struct ArtifactWriter {
    root: PathBuf,
    log_candidates: Vec<PathBuf>,
    log_start_offsets: Vec<LogStartOffset>,
}

#[derive(Clone, Debug, Serialize)]
struct LogStartOffset {
    path: String,
    existed: bool,
    len_bytes: u64,
}

/// Effective accelerated/baseline SQL captured once for benchmark evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BenchmarkQueryIdentity {
    accel_query_sql: String,
    baseline_query_sql: String,
}

impl BenchmarkQueryIdentity {
    pub fn resolve(
        accel_query_sql: String,
        baseline_query_sql: Option<String>,
    ) -> io::Result<Self> {
        let baseline_query_sql = baseline_query_sql.unwrap_or_else(|| accel_query_sql.clone());
        Self::from_effective(accel_query_sql, baseline_query_sql)
    }

    pub fn from_effective(accel_query_sql: String, baseline_query_sql: String) -> io::Result<Self> {
        if accel_query_sql.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "accelerated benchmark query SQL must be nonempty",
            ));
        }
        if baseline_query_sql.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "baseline benchmark query SQL must be nonempty",
            ));
        }
        Ok(Self {
            accel_query_sql,
            baseline_query_sql,
        })
    }

    #[must_use]
    pub fn accel_query_sql(&self) -> &str {
        &self.accel_query_sql
    }

    #[must_use]
    pub fn baseline_query_sql(&self) -> &str {
        &self.baseline_query_sql
    }
}

#[derive(Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct PreRiskContext<'a> {
    pub workload: &'a str,
    pub rows: usize,
    pub seed: u64,
    pub iterations: usize,
    pub warmup: usize,
    pub timing_mode: &'a str,
    pub cache_mode: &'a str,
    pub realistic_gucs: bool,
    pub skip_guc_verify: bool,
    pub capture_plans: bool,
    pub capture_planner_stages: bool,
    pub native_parity_pairing: bool,
    pub backend_pid: Option<i32>,
    pub backend_pid_error: Option<&'a str>,
    pub setup_sql: &'a [String],
    pub pre_query_sql: &'a [String],
    pub accel_query_sql: &'a str,
    pub baseline_query_sql: Option<&'a str>,
    pub explain_sql: &'a str,
    pub explain: Option<&'a str>,
    pub explain_error: Option<&'a str>,
}

#[derive(Serialize)]
struct Manifest {
    schema_version: u32,
    created_unix_seconds: u64,
    artifact_index_path: &'static str,
    artifact_checklist_path: &'static str,
    log_tail_bytes: u64,
    log_delta_bytes: u64,
    telemetry_tail_bytes: u64,
    max_log_candidates: usize,
    log_candidates: Vec<String>,
    log_start_offsets: Vec<LogStartOffset>,
    telemetry_candidates: Vec<String>,
}

#[derive(Serialize)]
struct ArtifactIndex {
    schema_version: u32,
    generated_unix_seconds: u64,
    root: String,
    entry_count: usize,
    total_size_bytes: u64,
    entries: Vec<ArtifactIndexEntry>,
}

#[derive(Serialize)]
struct ArtifactIndexEntry {
    path: String,
    size_bytes: u64,
    modified_unix_seconds: u64,
}

#[derive(Serialize)]
struct ResumeAuditManifest {
    schema_version: u32,
    generated_unix_seconds: u64,
    artifact_root: String,
    known_paths: ResumeKnownPaths,
    bounded_log_policy: BoundedLogPolicy,
    log_offset_provenance: Vec<LogStartOffset>,
    inventory: ResumeArtifactInventory,
}

#[derive(Serialize)]
struct ResumeKnownPaths {
    #[serde(rename = "run_manifest_path")]
    run_manifest: &'static str,
    #[serde(rename = "resume_audit_manifest_path")]
    resume_audit_manifest: &'static str,
    #[serde(rename = "artifact_index_path")]
    artifact_index: &'static str,
    #[serde(rename = "artifact_checklist_path")]
    artifact_checklist: &'static str,
}

#[derive(Serialize)]
struct BoundedLogPolicy {
    log_tail_bytes: u64,
    log_delta_bytes: u64,
    telemetry_tail_bytes: u64,
    max_log_candidates: usize,
    log_candidates: Vec<String>,
    telemetry_candidates: Vec<String>,
}

#[derive(Serialize)]
struct ResumeArtifactInventory {
    #[serde(rename = "completed_artifacts")]
    completed: Vec<String>,
    #[serde(rename = "correctness_artifacts")]
    correctness: Vec<String>,
    #[serde(rename = "pre_risk_artifacts")]
    pre_risk: Vec<String>,
    #[serde(rename = "plan_artifacts")]
    plan: Vec<String>,
    #[serde(rename = "crash_artifacts")]
    crash: Vec<String>,
    #[serde(rename = "log_artifacts")]
    log: Vec<String>,
    #[serde(rename = "provenance_artifacts")]
    provenance: Vec<String>,
    #[serde(rename = "failure_artifacts")]
    failure: Vec<String>,
}

#[derive(Serialize)]
struct GucSnapshot<'a> {
    settings: &'a [(String, String)],
    postmaster_start_time: Option<&'a str>,
}

impl ArtifactWriter {
    pub fn new(root: PathBuf, log_candidates: Vec<PathBuf>) -> io::Result<Self> {
        clear_managed_artifacts(&root)?;
        fs::create_dir_all(root.join("plan_snippets"))?;
        fs::create_dir_all(root.join("log_tails"))?;

        let log_candidates = unique_paths(log_candidates);
        let log_start_offsets = capture_log_start_offsets(&log_candidates);
        let writer = Self {
            root,
            log_candidates,
            log_start_offsets,
        };
        writer.write_manifest()?;
        writer
            .write_crashes(&[])
            .map_err(|err| io::Error::other(err.to_string()))?;
        Ok(writer)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write_guc_snapshot(
        &self,
        gucs: &GucSettings,
        postmaster_start_time: Option<&str>,
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let path = self.root.join("guc_snapshot.json");
        let snapshot = GucSnapshot {
            settings: &gucs.settings,
            postmaster_start_time,
        };
        write_json(&path, &snapshot)?;
        self.write_artifact_index()?;
        Ok(path)
    }

    pub fn write_plan_snippet(
        &self,
        workload: &str,
        rows: usize,
        snippet: &str,
    ) -> io::Result<PathBuf> {
        let path = self.plan_snippet_path(workload, rows);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, snippet)?;
        self.write_artifact_index()?;
        Ok(path)
    }

    pub fn write_pre_risk_context(
        &self,
        workload: &str,
        rows: usize,
        context: &PreRiskContext<'_>,
        query_identity: &BenchmarkQueryIdentity,
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        validate_pre_risk_query_identity(context, query_identity)?;
        let path = self.pre_risk_context_path(workload, rows);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_json(&path, context)?;
        self.write_artifact_index()?;
        Ok(path)
    }

    #[must_use]
    pub fn existing_pre_risk_context_artifact(
        &self,
        workload: &str,
        rows: usize,
    ) -> Option<String> {
        let path = self.pre_risk_context_path(workload, rows);
        path.is_file().then(|| self.relative_display_path(&path))
    }

    #[must_use]
    pub fn existing_plan_snippet_artifact(&self, workload: &str, rows: usize) -> Option<String> {
        let path = self.plan_snippet_path(workload, rows);
        path.is_file().then(|| self.relative_display_path(&path))
    }

    #[must_use]
    pub fn existing_correctness_diff_artifact(
        &self,
        workload: &str,
        rows: usize,
    ) -> Option<String> {
        let path = self.correctness_diff_path(workload, rows);
        path.is_file().then(|| self.relative_display_path(&path))
    }

    pub fn capture_log_tails(&self, label: &str) -> io::Result<Vec<String>> {
        let dir = self.root.join("log_tails").join(sanitize_label(label));
        fs::create_dir_all(&dir)?;

        let mut written = Vec::new();
        for (idx, candidate) in self.log_candidates.iter().enumerate() {
            if !candidate.is_file() {
                continue;
            }
            let tail = read_tail(candidate, LOG_TAIL_BYTES)?;
            let metadata = candidate.metadata()?;
            let current_len = metadata.len();
            let start_offset = self
                .log_start_offsets
                .get(idx)
                .map_or(0, |offset| offset.len_bytes);
            let delta_start = if current_len >= start_offset {
                start_offset
            } else {
                0
            };
            let delta = read_delta(candidate, delta_start, LOG_DELTA_BYTES)?;
            let source_name = candidate
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("log");
            let safe_source_name = sanitize_label(source_name);
            let tail_path = dir.join(format!("{idx:02}-{safe_source_name}.tail"));

            let mut out = String::new();
            let _ = writeln!(out, "source: {}", candidate.display());
            let _ = writeln!(out, "tail_bytes: {LOG_TAIL_BYTES}");
            out.push_str("---\n");
            out.push_str(&tail);
            if !tail.ends_with('\n') {
                out.push('\n');
            }
            fs::write(&tail_path, out)?;
            written.push(self.relative_display_path(&tail_path));

            let delta_path = dir.join(format!("{idx:02}-{safe_source_name}.delta"));
            let mut out = String::new();
            let _ = writeln!(out, "source: {}", candidate.display());
            let _ = writeln!(out, "run_start_offset_bytes: {start_offset}");
            let _ = writeln!(out, "delta_start_offset_bytes: {delta_start}");
            let _ = writeln!(out, "capture_end_offset_bytes: {current_len}");
            let _ = writeln!(out, "delta_max_bytes: {LOG_DELTA_BYTES}");
            let _ = writeln!(
                out,
                "truncated_to_last_delta_bytes: {}",
                current_len.saturating_sub(delta_start) > LOG_DELTA_BYTES
            );
            if current_len < start_offset {
                out.push_str("note: source file shrank since run start; delta starts at byte 0.\n");
            }
            out.push_str("---\n");
            out.push_str(&delta);
            if !delta.ends_with('\n') {
                out.push('\n');
            }
            fs::write(&delta_path, out)?;
            written.push(self.relative_display_path(&delta_path));
        }

        if written.is_empty() {
            let out_path = dir.join("no-log-files-found.txt");
            fs::write(
                &out_path,
                "No configured PostgreSQL or pg_accel log files were present.\n",
            )?;
            written.push(self.relative_display_path(&out_path));
        }

        self.write_artifact_index()?;
        Ok(written)
    }

    /// Read every byte appended to configured logs since this writer was
    /// created. Artifact files remain bounded by [`LOG_DELTA_BYTES`], while
    /// release gates can use this complete view to avoid scanning only a tail.
    #[cfg(test)]
    pub fn complete_log_deltas(&self) -> io::Result<Vec<(PathBuf, String)>> {
        let mut deltas = Vec::new();
        for (index, candidate) in self.log_candidates.iter().enumerate() {
            if !candidate.is_file() {
                continue;
            }
            let current_len = candidate.metadata()?.len();
            let start_offset = self
                .log_start_offsets
                .get(index)
                .map_or(0, |offset| offset.len_bytes);
            let delta_start = if current_len >= start_offset {
                start_offset
            } else {
                0
            };
            deltas.push((
                candidate.clone(),
                read_delta(candidate, delta_start, u64::MAX)?,
            ));
        }
        Ok(deltas)
    }

    pub fn write_crashes(
        &self,
        crashes: &[CrashedScale],
    ) -> Result<(), Box<dyn std::error::Error>> {
        write_json(&self.root.join("crashes.json"), crashes)?;

        let mut md = String::new();
        md.push_str("# Crash List\n\n");
        if crashes.is_empty() {
            md.push_str("No benchmark crashes recorded.\n");
        } else {
            md.push_str(
                "| Workload | Rows | Error | Plan Snippet | Correctness Diff | Log Tails | Repro |\n",
            );
            md.push_str("|---|---:|---|---|---|---|---|\n");
            for crash in crashes {
                let plan = crash.plan_snippet_artifact.as_deref().unwrap_or("-");
                let correctness = crash.correctness_diff_artifact.as_deref().unwrap_or("-");
                let logs = if crash.log_tail_artifacts.is_empty() {
                    "-".to_owned()
                } else {
                    crash.log_tail_artifacts.join("<br>")
                };
                let repro = crash.repro_command.as_deref().unwrap_or("-");
                let _ = writeln!(
                    md,
                    "| {} | {} | {} | {} | {} | {} | `{}` |",
                    markdown_cell(&crash.workload),
                    crash.rows,
                    markdown_cell(&crash.error),
                    markdown_cell(plan),
                    markdown_cell(correctness),
                    markdown_cell(&logs),
                    markdown_cell(repro),
                );
            }
        }
        fs::write(self.root.join("crashes.md"), md)?;
        self.write_artifact_index()?;
        Ok(())
    }

    pub fn write_failure(&self, label: &str, error: &str) -> io::Result<PathBuf> {
        let path = self
            .root
            .join(format!("failure-{}.txt", sanitize_label(label)));
        fs::write(&path, format!("{error}\n"))?;
        self.write_artifact_index()?;
        Ok(path)
    }

    pub fn write_provenance<T: Serialize + ?Sized>(
        &self,
        provenance: &T,
        warnings: &[String],
        errors: &[String],
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let path = self.root.join("provenance.json");
        write_json(&path, provenance)?;

        if !warnings.is_empty() || !errors.is_empty() {
            let mut text = String::new();
            if !errors.is_empty() {
                text.push_str("Errors:\n");
                for error in errors {
                    let _ = writeln!(text, "- {error}");
                }
            }
            if !warnings.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str("Warnings:\n");
                for warning in warnings {
                    let _ = writeln!(text, "- {warning}");
                }
            }
            fs::write(self.root.join("provenance-warnings.txt"), text)?;
        }

        self.write_artifact_index()?;
        Ok(path)
    }

    pub fn write_report(&self, report: &BenchReport) -> Result<(), Box<dyn std::error::Error>> {
        fs::write(self.root.join("report.json"), report.to_json()?)?;
        fs::write(self.root.join("report.md"), report.to_markdown())?;
        fs::write(self.root.join("report.csv"), report.to_csv())?;
        let no_dispatch_audit = report.no_dispatch_audit();
        write_json(&self.root.join(NO_DISPATCH_AUDIT_JSON), &no_dispatch_audit)?;
        fs::write(
            self.root.join(NO_DISPATCH_AUDIT_MD),
            no_dispatch_audit.to_markdown(),
        )?;
        let resident_boundary_audit = report.resident_boundary_audit();
        write_json(
            &self.root.join(RESIDENT_BOUNDARY_AUDIT_JSON),
            &resident_boundary_audit,
        )?;
        fs::write(
            self.root.join(RESIDENT_BOUNDARY_AUDIT_MD),
            resident_boundary_audit.to_markdown(),
        )?;
        let failure_ledger = report.benchmark_failure_ledger();
        write_json(
            &self.root.join(BENCHMARK_FAILURE_LEDGER_JSON),
            &failure_ledger,
        )?;
        fs::write(
            self.root.join(BENCHMARK_FAILURE_LEDGER_MD),
            failure_ledger.to_markdown(),
        )?;
        self.write_artifact_index()?;
        if resident_boundary_audit.has_failures() {
            return Err(format!(
                "resident-boundary audit failed for {} selected Custom Scan row(s)",
                resident_boundary_audit.failed_rows
            )
            .into());
        }
        Ok(())
    }

    pub fn write_correctness_diff<T: Serialize + ?Sized>(
        &self,
        workload: &str,
        rows: usize,
        diff: &T,
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let path = self.correctness_diff_path(workload, rows);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_json(&path, diff)?;
        self.write_artifact_index()?;
        Ok(path)
    }

    fn write_manifest(&self) -> io::Result<()> {
        let manifest = Manifest {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            created_unix_seconds: unix_timestamp_secs(),
            artifact_index_path: ARTIFACT_INDEX_JSON,
            artifact_checklist_path: ARTIFACT_CHECKLIST_MD,
            log_tail_bytes: LOG_TAIL_BYTES,
            log_delta_bytes: LOG_DELTA_BYTES,
            telemetry_tail_bytes: LOG_TAIL_BYTES,
            max_log_candidates: MAX_LOG_CANDIDATES,
            log_candidates: self
                .log_candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            log_start_offsets: self.log_start_offsets.clone(),
            telemetry_candidates: self
                .log_candidates
                .iter()
                .filter(|p| is_telemetry_candidate(p))
                .map(|p| p.display().to_string())
                .collect(),
        };
        write_json_io(&self.root.join(RUN_MANIFEST_JSON), &manifest)?;

        let mut readme = String::new();
        readme.push_str("# pg_accel Benchmark Artifacts\n\n");
        let _ = writeln!(
            readme,
            "Log/telemetry tails are capped at `{LOG_TAIL_BYTES}` bytes per file, with at most \
             `{MAX_LOG_CANDIDATES}` unique candidate files considered.\n"
        );
        let _ = writeln!(
            readme,
            "Each log capture also writes a bounded delta capped at `{LOG_DELTA_BYTES}` bytes, \
             using the source file size observed when the artifact writer started.\n"
        );
        readme.push_str(
            "- `artifact_index.json`: machine-readable inventory of generated artifact files, \
             sizes, and modified timestamps.\n",
        );
        readme.push_str(
            "- `artifact_checklist.md`: markdown checklist view of the generated artifact \
             inventory.\n",
        );
        readme.push_str(
            "- `resume_audit_manifest.json`: durable resume/audit manifest linking \
             bounded log policy, run-start log offsets, and generated evidence files.\n",
        );
        readme.push_str("- `report.json`, `report.md`, `report.csv`: rendered benchmark report.\n");
        readme.push_str(
            "- `no_dispatch_audit.json`, `no_dispatch_audit.md`: machine-readable and \
             markdown audit for rows that did not prove pg_accel GPU dispatch and \
             therefore cannot be used as GPU performance conclusions.\n",
        );
        readme.push_str(
            "- `resident_boundary_audit.json`, `resident_boundary_audit.md`: \
             machine-readable and markdown audit for selected pg_accel Custom Scan \
             resident-pipeline / CPU-boundary evidence.\n",
        );
        readme.push_str(
            "- `benchmark_failure_ledger.json`, `benchmark_failure_ledger.md`: merged \
             work queue for release-blocking gate failures and measured rows still below \
             PostgreSQL-parallel parity.\n",
        );
        readme.push_str("- `crashes.json`, `crashes.md`: crash inventory and repro commands.\n");
        readme.push_str(
            "- `crash_contexts/`: per-crash repro context with linked plan, correctness, \
             GUC, and log excerpts.\n",
        );
        readme.push_str(
            "- `correctness_diffs/`: bounded accel-vs-baseline result diff summaries \
             captured before timing.\n",
        );
        readme.push_str("- `guc_snapshot.json`: PostgreSQL settings observed by the harness.\n");
        readme.push_str(
            "- `provenance.json`: pg_config, SQL metadata, and extension binary hashes.\n",
        );
        readme.push_str(
            "- `provenance-warnings.txt`: provenance gaps or hard-fail reasons, when present.\n",
        );
        readme.push_str("- `plan_snippets/`: EXPLAIN snippets captured before timed execution.\n");
        readme.push_str(
            "- `pre_risk_contexts/`: same-backend `pg_backend_pid()`, workload SQL, \
             config basics, and `EXPLAIN` without `ANALYZE` captured before risky execution.\n",
        );
        readme.push_str(
            "- `log_tails/`: bounded PostgreSQL and pg_accel log/telemetry tails plus \
             run-start deltas per failure and at run completion.\n",
        );
        fs::write(self.root.join("README.md"), readme)?;
        self.write_artifact_index()
    }

    fn write_artifact_index(&self) -> io::Result<()> {
        let mut entries = Vec::new();
        collect_artifact_entries(&self.root, &self.root, &mut entries)?;
        entries.retain(|entry| entry.path != RESUME_AUDIT_MANIFEST_JSON);
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        self.write_resume_audit_manifest(&entries)?;
        if let Some(entry) =
            artifact_index_entry(&self.root, &self.root.join(RESUME_AUDIT_MANIFEST_JSON))?
        {
            entries.push(entry);
            entries.sort_by(|left, right| left.path.cmp(&right.path));
        }

        let total_size_bytes = entries.iter().map(|entry| entry.size_bytes).sum();
        let index = ArtifactIndex {
            schema_version: ARTIFACT_INDEX_SCHEMA_VERSION,
            generated_unix_seconds: unix_timestamp_secs(),
            root: self.root.display().to_string(),
            entry_count: entries.len(),
            total_size_bytes,
            entries,
        };

        write_json_io(&self.root.join(ARTIFACT_INDEX_JSON), &index)?;
        fs::write(
            self.root.join(ARTIFACT_CHECKLIST_MD),
            artifact_checklist_markdown(&index),
        )
    }

    fn write_resume_audit_manifest(&self, entries: &[ArtifactIndexEntry]) -> io::Result<()> {
        let manifest = ResumeAuditManifest {
            schema_version: RESUME_AUDIT_MANIFEST_SCHEMA_VERSION,
            generated_unix_seconds: unix_timestamp_secs(),
            artifact_root: self.root.display().to_string(),
            known_paths: ResumeKnownPaths {
                run_manifest: RUN_MANIFEST_JSON,
                resume_audit_manifest: RESUME_AUDIT_MANIFEST_JSON,
                artifact_index: ARTIFACT_INDEX_JSON,
                artifact_checklist: ARTIFACT_CHECKLIST_MD,
            },
            bounded_log_policy: BoundedLogPolicy {
                log_tail_bytes: LOG_TAIL_BYTES,
                log_delta_bytes: LOG_DELTA_BYTES,
                telemetry_tail_bytes: LOG_TAIL_BYTES,
                max_log_candidates: MAX_LOG_CANDIDATES,
                log_candidates: self
                    .log_candidates
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
                telemetry_candidates: self
                    .log_candidates
                    .iter()
                    .filter(|p| is_telemetry_candidate(p))
                    .map(|p| p.display().to_string())
                    .collect(),
            },
            log_offset_provenance: self.log_start_offsets.clone(),
            inventory: resume_artifact_inventory(entries),
        };
        write_json_io(&self.root.join(RESUME_AUDIT_MANIFEST_JSON), &manifest)
    }

    fn plan_snippet_path(&self, workload: &str, rows: usize) -> PathBuf {
        self.root
            .join("plan_snippets")
            .join(format!("{}-{rows}.txt", sanitize_label(workload)))
    }

    fn pre_risk_context_path(&self, workload: &str, rows: usize) -> PathBuf {
        self.root
            .join("pre_risk_contexts")
            .join(format!("{}-{rows}.json", sanitize_label(workload)))
    }

    fn correctness_diff_path(&self, workload: &str, rows: usize) -> PathBuf {
        self.root
            .join("correctness_diffs")
            .join(format!("{}-{rows}.json", sanitize_label(workload)))
    }

    fn relative_display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

fn validate_pre_risk_query_identity(
    context: &PreRiskContext<'_>,
    expected: &BenchmarkQueryIdentity,
) -> io::Result<()> {
    let baseline_query_sql = context.baseline_query_sql.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "pre-risk context baseline_query_sql must be a nonnull string",
        )
    })?;
    if context.accel_query_sql.trim().is_empty() || baseline_query_sql.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pre-risk context query SQL must be nonempty",
        ));
    }
    if context.accel_query_sql != expected.accel_query_sql()
        || baseline_query_sql != expected.baseline_query_sql()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pre-risk context query identity does not match the resolved benchmark queries",
        ));
    }
    Ok(())
}

fn clear_managed_artifacts(root: &Path) -> io::Result<()> {
    if !root.exists() {
        return Ok(());
    }

    for dirname in MANAGED_ARTIFACT_DIRS {
        remove_managed_path(&root.join(dirname))?;
    }
    for filename in MANAGED_ARTIFACT_FILES {
        remove_managed_path(&root.join(filename))?;
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with("failure-")
            && Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
        {
            remove_managed_path(&entry.path())?;
        }
    }

    Ok(())
}

fn remove_managed_path(path: &Path) -> io::Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };

    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[must_use]
pub fn default_run_dir(kind: &str) -> PathBuf {
    default_run_dir_for_time(kind, SystemTime::now(), std::process::id())
}

fn default_run_dir_for_time(kind: &str, time: SystemTime, pid: u32) -> PathBuf {
    let duration = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0));
    PathBuf::from("benchmarks").join("artifacts").join(format!(
        "{}-{}-{}-{:09}",
        sanitize_label(kind),
        duration.as_secs(),
        pid,
        duration.subsec_nanos()
    ))
}

#[must_use]
pub fn default_log_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        let pg_major = std::env::var("PG_ACCEL_PG_MAJOR")
            .or_else(|_| std::env::var("PG_ACCEL_DEFAULT_PG_MAJOR"))
            .unwrap_or_else(|_| "19".to_owned());
        out.push(home.join(".pgrx").join(format!("{pg_major}.log")));
        append_pgdata_log_candidates(
            &mut out,
            &home.join(".pgrx").join(format!("data-{pg_major}")),
        );
    }
    if let Some(pgdata) = std::env::var_os("PGDATA") {
        append_pgdata_log_candidates(&mut out, &PathBuf::from(pgdata));
    }
    out.push(PathBuf::from("/tmp/pg_accel_panic.log"));
    unique_paths(out)
}

pub fn append_pgdata_log_candidates(out: &mut Vec<PathBuf>, data_dir: &Path) {
    out.push(data_dir.join("pg_accel_panic.log"));
    out.push(data_dir.join("pg_accel_traces.jsonl"));
    out.push(data_dir.join("pg_accel_otel.jsonl"));
    append_recent_log_files(out, data_dir);
    append_recent_log_files(out, &data_dir.join("log"));
    append_recent_log_files(out, &data_dir.join("pg_log"));
}

fn append_recent_log_files(out: &mut Vec<PathBuf>, dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || !is_log_like(&path) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        files.push((modified, path));
    }

    files.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    out.extend(files.into_iter().take(3).map(|(_, path)| path));
}

fn is_log_like(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| matches!(ext, "log" | "csv" | "jsonl"))
}

fn is_telemetry_candidate(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext == "jsonl")
}

fn collect_artifact_entries(
    root: &Path,
    dir: &Path,
    entries: &mut Vec<ArtifactIndexEntry>,
) -> io::Result<()> {
    let mut children = Vec::new();
    for entry in fs::read_dir(dir)? {
        children.push(entry?);
    }
    children.sort_by_key(std::fs::DirEntry::path);

    for entry in children {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_artifact_entries(root, &path, entries)?;
        } else if file_type.is_file()
            && let Some(entry) = artifact_index_entry(root, &path)?
        {
            entries.push(entry);
        }
    }

    Ok(())
}

fn artifact_index_entry(root: &Path, path: &Path) -> io::Result<Option<ArtifactIndexEntry>> {
    let relative_path = relative_artifact_path(root, path);
    if matches!(
        relative_path.as_str(),
        ARTIFACT_INDEX_JSON | ARTIFACT_CHECKLIST_MD
    ) {
        return Ok(None);
    }
    let metadata = path.metadata()?;
    Ok(Some(ArtifactIndexEntry {
        path: relative_path,
        size_bytes: metadata.len(),
        modified_unix_seconds: metadata.modified().map_or(0, system_time_to_unix_secs),
    }))
}

fn relative_artifact_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn resume_artifact_inventory(entries: &[ArtifactIndexEntry]) -> ResumeArtifactInventory {
    ResumeArtifactInventory {
        completed: inventory_paths(entries, |path| {
            matches!(
                path,
                "report.json"
                    | "report.md"
                    | "report.csv"
                    | "guc_snapshot.json"
                    | NO_DISPATCH_AUDIT_JSON
                    | NO_DISPATCH_AUDIT_MD
                    | RESIDENT_BOUNDARY_AUDIT_JSON
                    | RESIDENT_BOUNDARY_AUDIT_MD
                    | BENCHMARK_FAILURE_LEDGER_JSON
                    | BENCHMARK_FAILURE_LEDGER_MD
            )
        }),
        correctness: inventory_paths(entries, |path| path.starts_with("correctness_diffs/")),
        pre_risk: inventory_paths(entries, |path| path.starts_with("pre_risk_contexts/")),
        plan: inventory_paths(entries, |path| path.starts_with("plan_snippets/")),
        crash: inventory_paths(entries, |path| {
            matches!(path, "crashes.json" | "crashes.md")
                || path.starts_with("crash_contexts/")
                || path.starts_with("log_tails/crash-")
        }),
        log: inventory_paths(entries, |path| path.starts_with("log_tails/")),
        provenance: inventory_paths(entries, |path| {
            matches!(path, "provenance.json" | "provenance-warnings.txt")
        }),
        failure: inventory_paths(entries, |path| {
            matches!(
                path,
                BENCHMARK_FAILURE_LEDGER_JSON | BENCHMARK_FAILURE_LEDGER_MD
            ) || path.starts_with("failure-")
        }),
    }
}

fn inventory_paths(
    entries: &[ArtifactIndexEntry],
    mut include: impl FnMut(&str) -> bool,
) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| include(&entry.path))
        .map(|entry| entry.path.clone())
        .collect()
}

fn artifact_checklist_markdown(index: &ArtifactIndex) -> String {
    let mut md = String::new();
    md.push_str("# Artifact Checklist\n\n");
    let _ = writeln!(
        md,
        "Generated at `{}` Unix seconds. Indexed `{}` files totaling `{}` bytes.",
        index.generated_unix_seconds, index.entry_count, index.total_size_bytes
    );
    md.push('\n');
    md.push_str(
        "`artifact_index.json` and `artifact_checklist.md` are regenerated from this inventory \
         and are not listed in their own file table.\n\n",
    );
    md.push_str("| Present | File | Size (bytes) | Modified (unix seconds) |\n");
    md.push_str("|---|---|---:|---:|\n");
    for entry in &index.entries {
        let _ = writeln!(
            md,
            "| [x] | `{}` | {} | {} |",
            markdown_cell(&entry.path),
            entry.size_bytes,
            entry.modified_unix_seconds,
        );
    }
    md
}

fn read_tail(path: &Path, max_bytes: u64) -> io::Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start))?;

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0
        && let Some(first_newline) = text.find('\n')
    {
        text.drain(..=first_newline);
    }
    Ok(text)
}

fn read_delta(path: &Path, start_offset: u64, max_bytes: u64) -> io::Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut start = start_offset.min(len);
    if len.saturating_sub(start) > max_bytes {
        start = len - max_bytes;
    }
    file.seek(SeekFrom::Start(start))?;

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > start_offset
        && let Some(first_newline) = text.find('\n')
    {
        text.drain(..=first_newline);
    }
    Ok(text)
}

fn capture_log_start_offsets(candidates: &[PathBuf]) -> Vec<LogStartOffset> {
    candidates
        .iter()
        .map(|path| match path.metadata() {
            Ok(metadata) if metadata.is_file() => LogStartOffset {
                path: path.display().to_string(),
                existed: true,
                len_bytes: metadata.len(),
            },
            _ => LogStartOffset {
                path: path.display().to_string(),
                existed: false,
                len_bytes: 0,
            },
        })
        .collect()
}

fn write_json<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

fn write_json_io<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let json = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    fs::write(path, json)
}

fn unique_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for path in paths {
        let key = path.display().to_string();
        if seen.insert(key) {
            out.push(path);
            if out.len() >= MAX_LOG_CANDIDATES {
                break;
            }
        }
    }
    out
}

fn unix_timestamp_secs() -> u64 {
    system_time_to_unix_secs(SystemTime::now())
}

fn system_time_to_unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn sanitize_label(input: &str) -> String {
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

fn markdown_cell(input: &str) -> String {
    input.replace('|', "\\|").replace('\n', "<br>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench_model::{CachePurgeState, CacheState};
    use crate::report::{
        IterationResult, Methodology, NO_DISPATCH_AUDIT_SCHEMA_VERSION,
        RESIDENT_BOUNDARY_AUDIT_SCHEMA_VERSION, WorkloadResult,
    };
    use serde_json::Value;

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
                "pg_accel_artifacts_{label}_{}_{}",
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

    fn test_pre_risk_context<'a>(
        accel_query_sql: &'a str,
        baseline_query_sql: Option<&'a str>,
    ) -> PreRiskContext<'a> {
        PreRiskContext {
            workload: "query_identity",
            rows: 100,
            seed: 42,
            iterations: 1,
            warmup: 0,
            timing_mode: "raw",
            cache_mode: "warm",
            realistic_gucs: false,
            skip_guc_verify: false,
            capture_plans: false,
            capture_planner_stages: false,
            native_parity_pairing: false,
            backend_pid: None,
            backend_pid_error: None,
            setup_sql: &[],
            pre_query_sql: &[],
            accel_query_sql,
            baseline_query_sql,
            explain_sql: "EXPLAIN (VERBOSE, COSTS OFF) SELECT 1",
            explain: None,
            explain_error: None,
        }
    }

    fn mock_report_with_selected_boundary() -> BenchReport {
        let iterations = (0..5)
            .map(|_| IterationResult {
                accel_ms: 10.0,
                parallel_ms: 50.0,
                accel_first: false,
                cache_purge: CachePurgeState::NotRequested,
                cache_state: CacheState::Warm,
            })
            .collect();
        let mut workload = WorkloadResult::from_iterations(
            "artifact_boundary".to_owned(),
            "artifact resident-boundary test".to_owned(),
            "gpu".to_owned(),
            "unclassified".to_owned(),
            100_000,
            iterations,
            true,
        );
        workload.plan_selected = true;
        workload.dispatch_counter_captured = true;
        workload.gpu_kernel_execution_delta = 1;
        workload.pg_accel_queries_accelerated_delta = 5;
        workload.accel_output_rows_consumed = 10;
        workload.plan_snippet = Some(
            "Custom Scan (GpuAccelAgg)\n  Strategy: GpuAgg\n  GPU Kernel Dispatched: true\n  \
             GPU Resident Pipeline: true\n  \
             GPU Resident Proof Version: 2\n  \
             GPU Resident Operator Class: resident_groupagg\n  \
             GPU Resident GroupAgg Key: resident_i32\n  \
             GPU Resident GroupAgg Measure: direct_column\n  \
             GPU Resident GroupAgg Filter: none\n  \
             GPU Resident GroupAgg Predicate Guard: none\n  \
             GPU Resident GroupAgg Value Predicate: none\n  \
             GPU Resident GroupAgg Predicate IR: guard=none;value=none\n  \
             GPU Resident GroupAgg Aggregate Mask: 3\n  \
             GPU Resident Stage Mask: 5\n  \
             GPU Resident Device Columns: 1\n"
                .to_owned(),
        );

        BenchReport {
            hardware: None,
            gucs: None,
            methodology: Methodology {
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
    fn sanitize_label_keeps_safe_filename_chars() {
        assert_eq!(
            sanitize_label("gpu/hash agg @ 10M rows"),
            "gpu-hash-agg---10M-rows"
        );
        assert_eq!(sanitize_label("***"), "artifact");
    }

    #[test]
    fn default_run_dir_includes_pid_and_subsecond_time() {
        let base = UNIX_EPOCH + Duration::new(1_234, 567);

        assert_eq!(
            default_run_dir_for_time("run", base, 42),
            PathBuf::from("benchmarks/artifacts/run-1234-42-000000567")
        );
        assert_ne!(
            default_run_dir_for_time("run", base, 42),
            default_run_dir_for_time("run", base + Duration::new(0, 1), 42)
        );
        assert_ne!(
            default_run_dir_for_time("run", base, 42),
            default_run_dir_for_time("run", base, 43)
        );
    }

    #[test]
    fn markdown_cell_escapes_table_breakers() {
        assert_eq!(markdown_cell("a|b\nc"), "a\\|b<br>c");
    }

    #[test]
    fn manifest_records_artifact_and_tail_limits() {
        let artifacts = TestDir::new("manifest");
        let sources = TestDir::new("sources");
        let telemetry = sources.path().join("pg_accel_otel.jsonl");
        fs::write(&telemetry, "{}\n").expect("telemetry source should be written");

        let _writer = ArtifactWriter::new(artifacts.path().to_path_buf(), vec![telemetry.clone()])
            .expect("artifact writer should initialize");

        let manifest_text = fs::read_to_string(artifacts.path().join("manifest.json"))
            .expect("manifest should be readable");
        let manifest: Value =
            serde_json::from_str(&manifest_text).expect("manifest should be valid json");
        assert_eq!(manifest["schema_version"], ARTIFACT_SCHEMA_VERSION);
        assert_eq!(manifest["artifact_index_path"], ARTIFACT_INDEX_JSON);
        assert_eq!(manifest["artifact_checklist_path"], ARTIFACT_CHECKLIST_MD);
        assert_eq!(manifest["log_tail_bytes"], LOG_TAIL_BYTES);
        assert_eq!(manifest["log_delta_bytes"], LOG_DELTA_BYTES);
        assert_eq!(manifest["telemetry_tail_bytes"], LOG_TAIL_BYTES);
        assert_eq!(manifest["max_log_candidates"], MAX_LOG_CANDIDATES);
        assert_eq!(
            manifest["log_start_offsets"][0]["path"],
            telemetry.display().to_string()
        );
        assert_eq!(manifest["log_start_offsets"][0]["existed"], true);
        assert_eq!(manifest["log_start_offsets"][0]["len_bytes"], 3);
        assert_eq!(
            manifest["telemetry_candidates"][0],
            telemetry.display().to_string()
        );

        let readme = fs::read_to_string(artifacts.path().join("README.md"))
            .expect("README should be readable");
        assert!(readme.contains("Log/telemetry tails are capped"));
        assert!(readme.contains("bounded delta"));
        assert!(readme.contains(ARTIFACT_INDEX_JSON));
        assert!(readme.contains(ARTIFACT_CHECKLIST_MD));
        assert!(readme.contains(RESUME_AUDIT_MANIFEST_JSON));
        assert!(readme.contains(NO_DISPATCH_AUDIT_JSON));
        assert!(readme.contains(RESIDENT_BOUNDARY_AUDIT_JSON));
        assert!(readme.contains("crash_contexts/"));
        assert!(readme.contains("correctness_diffs/"));
        assert!(readme.contains("pre_risk_contexts/"));

        let resume_text = fs::read_to_string(artifacts.path().join(RESUME_AUDIT_MANIFEST_JSON))
            .expect("resume audit manifest should be readable");
        let resume: Value =
            serde_json::from_str(&resume_text).expect("resume audit manifest should be valid json");
        assert_eq!(
            resume["schema_version"],
            RESUME_AUDIT_MANIFEST_SCHEMA_VERSION
        );
        assert_eq!(
            resume["known_paths"]["run_manifest_path"],
            RUN_MANIFEST_JSON
        );
        assert_eq!(
            resume["known_paths"]["resume_audit_manifest_path"],
            RESUME_AUDIT_MANIFEST_JSON
        );
        assert_eq!(
            resume["known_paths"]["artifact_index_path"],
            ARTIFACT_INDEX_JSON
        );
        assert_eq!(
            resume["known_paths"]["artifact_checklist_path"],
            ARTIFACT_CHECKLIST_MD
        );
        assert_eq!(
            resume["bounded_log_policy"]["log_tail_bytes"],
            LOG_TAIL_BYTES
        );
        assert_eq!(
            resume["bounded_log_policy"]["log_delta_bytes"],
            LOG_DELTA_BYTES
        );
        assert_eq!(
            resume["bounded_log_policy"]["telemetry_candidates"][0],
            telemetry.display().to_string()
        );
        assert_eq!(
            resume["log_offset_provenance"][0]["path"],
            telemetry.display().to_string()
        );
        assert_eq!(
            resume["inventory"]["crash_artifacts"],
            serde_json::json!(["crashes.json", "crashes.md"])
        );
        assert_eq!(
            resume["inventory"]["correctness_artifacts"],
            serde_json::json!([])
        );
    }

    #[test]
    fn writer_start_clears_stale_managed_artifacts_only() {
        let artifacts = TestDir::new("managed-clean");
        fs::create_dir_all(artifacts.path().join("correctness_diffs"))
            .expect("correctness dir should be created");
        fs::create_dir_all(artifacts.path().join("plan_snippets"))
            .expect("plan dir should be created");
        fs::write(
            artifacts
                .path()
                .join("correctness_diffs/stale-workload-100.json"),
            r#"{"status":"pass"}"#,
        )
        .expect("stale correctness should be written");
        fs::write(
            artifacts
                .path()
                .join("plan_snippets/stale-workload-100.txt"),
            "stale plan\n",
        )
        .expect("stale plan should be written");
        fs::write(
            artifacts.path().join("failure-stale.txt"),
            "stale failure\n",
        )
        .expect("stale failure should be written");
        fs::write(artifacts.path().join("report.json"), "{}\n")
            .expect("stale report should be written");
        fs::write(artifacts.path().join("resume_source.json"), "{}\n")
            .expect("resume source should be written");
        fs::write(artifacts.path().join("notes.txt"), "keep me\n")
            .expect("unmanaged note should be written");

        let writer = ArtifactWriter::new(artifacts.path().to_path_buf(), Vec::new())
            .expect("artifact writer should initialize");

        assert!(
            !artifacts
                .path()
                .join("correctness_diffs/stale-workload-100.json")
                .exists()
        );
        assert!(
            !artifacts
                .path()
                .join("plan_snippets/stale-workload-100.txt")
                .exists()
        );
        assert!(!artifacts.path().join("failure-stale.txt").exists());
        assert!(!artifacts.path().join("report.json").exists());
        assert!(artifacts.path().join("resume_source.json").is_file());
        assert!(artifacts.path().join("notes.txt").is_file());
        assert!(artifacts.path().join("manifest.json").is_file());
        assert!(artifacts.path().join("crashes.json").is_file());
        assert_eq!(
            writer.existing_correctness_diff_artifact("stale/workload", 100),
            None
        );
    }

    #[test]
    fn write_report_generates_release_audit_artifacts() {
        let artifacts = TestDir::new("resident-boundary-report");
        let writer = ArtifactWriter::new(artifacts.path().to_path_buf(), Vec::new())
            .expect("artifact writer should initialize");
        let report = mock_report_with_selected_boundary();

        writer
            .write_report(&report)
            .expect("report with selected boundary should write");

        let no_dispatch_json_path = artifacts.path().join(NO_DISPATCH_AUDIT_JSON);
        let no_dispatch_md_path = artifacts.path().join(NO_DISPATCH_AUDIT_MD);
        assert!(no_dispatch_json_path.is_file());
        assert!(no_dispatch_md_path.is_file());
        let audit_json_path = artifacts.path().join(RESIDENT_BOUNDARY_AUDIT_JSON);
        let audit_md_path = artifacts.path().join(RESIDENT_BOUNDARY_AUDIT_MD);
        assert!(audit_json_path.is_file());
        assert!(audit_md_path.is_file());

        let no_dispatch_text = fs::read_to_string(&no_dispatch_json_path)
            .expect("no-dispatch audit json should be readable");
        let no_dispatch: Value =
            serde_json::from_str(&no_dispatch_text).expect("no-dispatch audit should be json");
        assert_eq!(
            no_dispatch["schema_version"],
            NO_DISPATCH_AUDIT_SCHEMA_VERSION
        );
        assert_eq!(no_dispatch["evaluated_no_dispatch_rows"], 0);

        let no_dispatch_md = fs::read_to_string(&no_dispatch_md_path)
            .expect("no-dispatch audit markdown should be readable");
        assert!(no_dispatch_md.contains("No-Dispatch Audit"));

        let audit_text = fs::read_to_string(&audit_json_path)
            .expect("resident boundary audit json should be readable");
        let audit: Value =
            serde_json::from_str(&audit_text).expect("resident boundary audit should be json");
        assert_eq!(
            audit["schema_version"],
            RESIDENT_BOUNDARY_AUDIT_SCHEMA_VERSION
        );
        assert_eq!(audit["selected_custom_scan_rows"], 1);
        assert_eq!(audit["failed_rows"], 0);
        assert_eq!(audit["rows"][0]["status"], "reported_resident_pipeline");

        let audit_md = fs::read_to_string(&audit_md_path)
            .expect("resident boundary audit markdown should be readable");
        assert!(audit_md.contains("Resident Boundary Audit"));
        assert!(audit_md.contains("reported_resident_pipeline"));

        let failure_ledger_json_path = artifacts.path().join(BENCHMARK_FAILURE_LEDGER_JSON);
        let failure_ledger_md_path = artifacts.path().join(BENCHMARK_FAILURE_LEDGER_MD);
        assert!(failure_ledger_json_path.is_file());
        assert!(failure_ledger_md_path.is_file());
        let failure_ledger_text = fs::read_to_string(&failure_ledger_json_path)
            .expect("benchmark failure ledger json should be readable");
        let failure_ledger: Value = serde_json::from_str(&failure_ledger_text)
            .expect("benchmark failure ledger should be json");
        assert_eq!(
            failure_ledger["schema_version"],
            crate::report::BENCHMARK_FAILURE_LEDGER_SCHEMA_VERSION
        );
        let failure_ledger_md = fs::read_to_string(&failure_ledger_md_path)
            .expect("benchmark failure ledger markdown should be readable");
        assert!(failure_ledger_md.contains("Benchmark Failure Ledger"));

        let index_text = fs::read_to_string(artifacts.path().join(ARTIFACT_INDEX_JSON))
            .expect("artifact index should be readable");
        let index: Value =
            serde_json::from_str(&index_text).expect("artifact index should be valid json");
        let paths: Vec<&str> = index["entries"]
            .as_array()
            .expect("artifact index entries should be an array")
            .iter()
            .map(|entry| {
                entry["path"]
                    .as_str()
                    .expect("artifact index path should be a string")
            })
            .collect();
        assert!(paths.contains(&NO_DISPATCH_AUDIT_JSON));
        assert!(paths.contains(&NO_DISPATCH_AUDIT_MD));
        assert!(paths.contains(&RESIDENT_BOUNDARY_AUDIT_JSON));
        assert!(paths.contains(&RESIDENT_BOUNDARY_AUDIT_MD));
        assert!(paths.contains(&BENCHMARK_FAILURE_LEDGER_JSON));
        assert!(paths.contains(&BENCHMARK_FAILURE_LEDGER_MD));

        let resume_text = fs::read_to_string(artifacts.path().join(RESUME_AUDIT_MANIFEST_JSON))
            .expect("resume audit manifest should be readable");
        let resume: Value =
            serde_json::from_str(&resume_text).expect("resume audit manifest should be valid json");
        let completed: Vec<&str> = resume["inventory"]["completed_artifacts"]
            .as_array()
            .expect("completed artifacts should be an array")
            .iter()
            .map(|path| path.as_str().expect("completed artifact should be string"))
            .collect();
        assert!(completed.contains(&NO_DISPATCH_AUDIT_JSON));
        assert!(completed.contains(&NO_DISPATCH_AUDIT_MD));
        assert!(completed.contains(&RESIDENT_BOUNDARY_AUDIT_JSON));
        assert!(completed.contains(&RESIDENT_BOUNDARY_AUDIT_MD));
        let failures: Vec<&str> = resume["inventory"]["failure_artifacts"]
            .as_array()
            .expect("failure artifacts should be an array")
            .iter()
            .map(|path| path.as_str().expect("failure artifact should be string"))
            .collect();
        assert!(failures.contains(&BENCHMARK_FAILURE_LEDGER_JSON));
        assert!(failures.contains(&BENCHMARK_FAILURE_LEDGER_MD));
    }

    #[test]
    fn benchmark_query_identity_resolves_default_and_explicit_baselines_exactly() {
        let default = BenchmarkQueryIdentity::resolve("SELECT 1".to_owned(), None)
            .expect("default baseline should resolve");
        assert_eq!(default.accel_query_sql(), "SELECT 1");
        assert_eq!(default.baseline_query_sql(), "SELECT 1");

        let explicit = BenchmarkQueryIdentity::resolve(
            "SELECT accel_fn()".to_owned(),
            Some("SELECT native_fn()".to_owned()),
        )
        .expect("explicit baseline should resolve");
        assert_eq!(explicit.accel_query_sql(), "SELECT accel_fn()");
        assert_eq!(explicit.baseline_query_sql(), "SELECT native_fn()");

        assert!(BenchmarkQueryIdentity::resolve(" \n".to_owned(), None).is_err());
        assert!(
            BenchmarkQueryIdentity::resolve("SELECT 1".to_owned(), Some("\t".to_owned())).is_err()
        );
    }

    #[test]
    fn pre_risk_writer_rejects_null_or_mismatched_query_identity() {
        let artifacts = TestDir::new("query_identity_rejection");
        let writer = ArtifactWriter::new(artifacts.path().to_path_buf(), Vec::new())
            .expect("artifact writer should initialize");
        let identity = BenchmarkQueryIdentity::resolve("SELECT 1".to_owned(), None)
            .expect("query identity should resolve");

        let null_baseline = test_pre_risk_context("SELECT 1", None);
        let error = writer
            .write_pre_risk_context("query_identity", 100, &null_baseline, &identity)
            .expect_err("null risk baseline must fail");
        assert!(error.to_string().contains("nonnull string"));

        let mismatched = test_pre_risk_context("SELECT 1", Some("SELECT 2"));
        let error = writer
            .write_pre_risk_context("query_identity", 100, &mismatched, &identity)
            .expect_err("mismatched risk baseline must fail");
        assert!(error.to_string().contains("does not match"));
        assert!(
            !artifacts
                .path()
                .join("pre_risk_contexts/query_identity-100.json")
                .exists(),
            "invalid context must not be written"
        );

        let explicit_identity = BenchmarkQueryIdentity::resolve(
            "SELECT accel_fn()".to_owned(),
            Some("SELECT native_fn()".to_owned()),
        )
        .expect("explicit query identity should resolve");
        let exact = test_pre_risk_context("SELECT accel_fn()", Some("SELECT native_fn()"));
        let path = writer
            .write_pre_risk_context("query_identity", 100, &exact, &explicit_identity)
            .expect("exact explicit risk identity should be written");
        let value: Value = serde_json::from_str(
            &fs::read_to_string(path).expect("explicit risk context should be readable"),
        )
        .expect("explicit risk context should be valid json");
        assert_eq!(value["accel_query_sql"], "SELECT accel_fn()");
        assert_eq!(value["baseline_query_sql"], "SELECT native_fn()");
    }

    #[test]
    fn artifact_index_tracks_generated_files_with_sizes_and_timestamps() {
        let artifacts = TestDir::new("index");
        let sources = TestDir::new("log_source");
        let telemetry = sources.path().join("pg_accel_otel.jsonl");
        fs::write(&telemetry, "line one\nline two\n").expect("telemetry source should be written");

        let writer = ArtifactWriter::new(artifacts.path().to_path_buf(), vec![telemetry])
            .expect("artifact writer should initialize");
        writer
            .write_plan_snippet("hash/join", 100, "Custom Scan\n")
            .expect("plan snippet should be written");
        let query_identity =
            BenchmarkQueryIdentity::resolve("SELECT count(*) FROM bench".to_owned(), None)
                .expect("query identity should resolve");
        let pre_query_sql = vec!["SET work_mem = '4MB'".to_owned()];
        let context = PreRiskContext {
            workload: "hash/join",
            rows: 100,
            seed: 42,
            iterations: 1,
            warmup: 0,
            timing_mode: "raw",
            cache_mode: "warm",
            realistic_gucs: false,
            skip_guc_verify: false,
            capture_plans: false,
            capture_planner_stages: false,
            native_parity_pairing: false,
            backend_pid: Some(1234),
            backend_pid_error: None,
            setup_sql: &[],
            pre_query_sql: &pre_query_sql,
            accel_query_sql: "SELECT count(*) FROM bench",
            baseline_query_sql: Some("SELECT count(*) FROM bench"),
            explain_sql: "EXPLAIN (VERBOSE, COSTS OFF) SELECT count(*) FROM bench",
            explain: Some("Aggregate\n"),
            explain_error: None,
        };
        writer
            .write_pre_risk_context("hash/join", 100, &context, &query_identity)
            .expect("pre-risk context should be written");
        writer
            .write_correctness_diff(
                "hash/join",
                100,
                &serde_json::json!({
                    "status": "pass",
                    "accel_minus_baseline_count": 0,
                    "baseline_minus_accel_count": 0
                }),
            )
            .expect("correctness diff should be written");
        writer
            .capture_log_tails("run-complete")
            .expect("log tail should be captured");

        let index_text = fs::read_to_string(artifacts.path().join(ARTIFACT_INDEX_JSON))
            .expect("artifact index should be readable");
        let index: Value =
            serde_json::from_str(&index_text).expect("artifact index should be valid json");
        assert_eq!(index["schema_version"], ARTIFACT_INDEX_SCHEMA_VERSION);

        let context_text = fs::read_to_string(
            artifacts
                .path()
                .join("pre_risk_contexts/hash-join-100.json"),
        )
        .expect("pre-risk context should be readable");
        let context_json: Value =
            serde_json::from_str(&context_text).expect("pre-risk context should be valid json");
        assert_eq!(
            context_json["accel_query_sql"],
            "SELECT count(*) FROM bench"
        );
        assert_eq!(
            context_json["baseline_query_sql"],
            "SELECT count(*) FROM bench"
        );

        let entries = index["entries"]
            .as_array()
            .expect("artifact index entries should be an array");
        let paths: Vec<&str> = entries
            .iter()
            .map(|entry| {
                entry["path"]
                    .as_str()
                    .expect("artifact index path should be a string")
            })
            .collect();
        assert!(paths.contains(&"manifest.json"));
        assert!(paths.contains(&RESUME_AUDIT_MANIFEST_JSON));
        assert!(paths.contains(&"README.md"));
        assert!(paths.contains(&"crashes.json"));
        assert!(paths.contains(&"crashes.md"));
        assert!(paths.contains(&"plan_snippets/hash-join-100.txt"));
        assert!(paths.contains(&"pre_risk_contexts/hash-join-100.json"));
        assert!(paths.contains(&"correctness_diffs/hash-join-100.json"));
        assert!(paths.contains(&"log_tails/run-complete/00-pg_accel_otel.jsonl.tail"));
        assert!(paths.contains(&"log_tails/run-complete/00-pg_accel_otel.jsonl.delta"));
        assert!(!paths.contains(&ARTIFACT_INDEX_JSON));
        assert!(!paths.contains(&ARTIFACT_CHECKLIST_MD));

        for entry in entries {
            assert!(
                entry["size_bytes"].as_u64().expect("size should be a u64") > 0,
                "entry should include a non-zero size: {entry:?}"
            );
            assert!(
                entry["modified_unix_seconds"]
                    .as_u64()
                    .expect("modified timestamp should be a u64")
                    > 0,
                "entry should include a modified timestamp: {entry:?}"
            );
        }

        let checklist = fs::read_to_string(artifacts.path().join(ARTIFACT_CHECKLIST_MD))
            .expect("artifact checklist should be readable");
        assert!(checklist.contains("| [x] | `manifest.json` |"));
        assert!(checklist.contains("| [x] | `resume_audit_manifest.json` |"));
        assert!(checklist.contains("| [x] | `plan_snippets/hash-join-100.txt` |"));
        assert!(checklist.contains("| [x] | `pre_risk_contexts/hash-join-100.json` |"));
        assert!(checklist.contains("| [x] | `correctness_diffs/hash-join-100.json` |"));
        assert!(checklist.contains("not listed in their own file table"));

        assert_eq!(
            writer.existing_pre_risk_context_artifact("hash/join", 100),
            Some("pre_risk_contexts/hash-join-100.json".to_owned())
        );
        assert_eq!(
            writer.existing_correctness_diff_artifact("hash/join", 100),
            Some("correctness_diffs/hash-join-100.json".to_owned())
        );
    }

    #[test]
    fn crashes_markdown_links_correctness_diff_artifact() {
        let artifacts = TestDir::new("crash_correctness");
        let writer = ArtifactWriter::new(artifacts.path().to_path_buf(), Vec::new())
            .expect("artifact writer should initialize");

        writer
            .write_crashes(&[CrashedScale {
                workload: "bad_correctness".to_owned(),
                rows: 100,
                error: "correctness diff failed".to_owned(),
                repro_command: None,
                plan_snippet_artifact: None,
                correctness_diff_artifact: Some(
                    "correctness_diffs/bad-correctness-100.json".to_owned(),
                ),
                log_tail_artifacts: Vec::new(),
            }])
            .expect("crashes should be written");

        let markdown =
            fs::read_to_string(artifacts.path().join("crashes.md")).expect("crashes markdown");
        assert!(markdown.contains("Correctness Diff"));
        assert!(markdown.contains("correctness_diffs/bad-correctness-100.json"));
    }

    #[test]
    fn log_delta_capture_starts_at_writer_creation_offset() {
        let artifacts = TestDir::new("delta");
        let sources = TestDir::new("log_delta_source");
        let log = sources.path().join("postgres.log");
        fs::write(&log, "stale panic\n").expect("initial log source should be written");

        let writer = ArtifactWriter::new(artifacts.path().to_path_buf(), vec![log.clone()])
            .expect("artifact writer should initialize");
        fs::write(&log, "stale panic\nfresh panic\n")
            .expect("updated log source should be written");

        let captured = writer
            .capture_log_tails("crash-001")
            .expect("log artifacts should be captured");
        assert_eq!(
            captured,
            vec![
                "log_tails/crash-001/00-postgres.log.tail".to_owned(),
                "log_tails/crash-001/00-postgres.log.delta".to_owned(),
            ]
        );

        let tail = fs::read_to_string(artifacts.path().join(&captured[0]))
            .expect("tail artifact should be readable");
        assert!(tail.contains("stale panic"));
        assert!(tail.contains("fresh panic"));

        let delta = fs::read_to_string(artifacts.path().join(&captured[1]))
            .expect("delta artifact should be readable");
        assert!(delta.contains("run_start_offset_bytes: 12"));
        assert!(!delta.contains("stale panic"));
        assert!(delta.contains("fresh panic"));

        let complete = writer
            .complete_log_deltas()
            .expect("complete log delta should be readable");
        assert_eq!(complete, vec![(log, "fresh panic\n".to_owned())]);

        let index_text = fs::read_to_string(artifacts.path().join(ARTIFACT_INDEX_JSON))
            .expect("artifact index should be readable");
        let index: Value =
            serde_json::from_str(&index_text).expect("artifact index should be valid json");
        let paths: Vec<&str> = index["entries"]
            .as_array()
            .expect("entries should be an array")
            .iter()
            .map(|entry| entry["path"].as_str().expect("path should be a string"))
            .collect();
        assert!(paths.contains(&"log_tails/crash-001/00-postgres.log.tail"));
        assert!(paths.contains(&"log_tails/crash-001/00-postgres.log.delta"));

        let resume_text = fs::read_to_string(artifacts.path().join(RESUME_AUDIT_MANIFEST_JSON))
            .expect("resume audit manifest should be readable");
        let resume: Value =
            serde_json::from_str(&resume_text).expect("resume audit manifest should be valid json");
        assert_eq!(
            resume["inventory"]["crash_artifacts"],
            serde_json::json!([
                "crashes.json",
                "crashes.md",
                "log_tails/crash-001/00-postgres.log.delta",
                "log_tails/crash-001/00-postgres.log.tail",
            ])
        );
    }

    #[test]
    fn resume_inventory_categorizes_sorted_artifact_paths() {
        let entries = vec![
            ArtifactIndexEntry {
                path: "README.md".to_owned(),
                size_bytes: 1,
                modified_unix_seconds: 1,
            },
            ArtifactIndexEntry {
                path: "crash_contexts/crash-001.txt".to_owned(),
                size_bytes: 1,
                modified_unix_seconds: 1,
            },
            ArtifactIndexEntry {
                path: "crashes.json".to_owned(),
                size_bytes: 1,
                modified_unix_seconds: 1,
            },
            ArtifactIndexEntry {
                path: "failure-setup.txt".to_owned(),
                size_bytes: 1,
                modified_unix_seconds: 1,
            },
            ArtifactIndexEntry {
                path: "correctness_diffs/hash-join-100.json".to_owned(),
                size_bytes: 1,
                modified_unix_seconds: 1,
            },
            ArtifactIndexEntry {
                path: "log_tails/crash-002/00-postgres.log.delta".to_owned(),
                size_bytes: 1,
                modified_unix_seconds: 1,
            },
            ArtifactIndexEntry {
                path: "log_tails/run-complete/00-postgres.log.tail".to_owned(),
                size_bytes: 1,
                modified_unix_seconds: 1,
            },
            ArtifactIndexEntry {
                path: "plan_snippets/hash-join-100.txt".to_owned(),
                size_bytes: 1,
                modified_unix_seconds: 1,
            },
            ArtifactIndexEntry {
                path: "pre_risk_contexts/hash-join-100.json".to_owned(),
                size_bytes: 1,
                modified_unix_seconds: 1,
            },
            ArtifactIndexEntry {
                path: "provenance-warnings.txt".to_owned(),
                size_bytes: 1,
                modified_unix_seconds: 1,
            },
            ArtifactIndexEntry {
                path: "no_dispatch_audit.json".to_owned(),
                size_bytes: 1,
                modified_unix_seconds: 1,
            },
            ArtifactIndexEntry {
                path: "report.json".to_owned(),
                size_bytes: 1,
                modified_unix_seconds: 1,
            },
        ];

        let inventory = resume_artifact_inventory(&entries);
        assert_eq!(
            inventory.completed,
            vec!["no_dispatch_audit.json", "report.json"]
        );
        assert_eq!(
            inventory.correctness,
            vec!["correctness_diffs/hash-join-100.json"]
        );
        assert_eq!(
            inventory.pre_risk,
            vec!["pre_risk_contexts/hash-join-100.json"]
        );
        assert_eq!(inventory.plan, vec!["plan_snippets/hash-join-100.txt"]);
        assert_eq!(
            inventory.crash,
            vec![
                "crash_contexts/crash-001.txt",
                "crashes.json",
                "log_tails/crash-002/00-postgres.log.delta"
            ]
        );
        assert_eq!(
            inventory.log,
            vec![
                "log_tails/crash-002/00-postgres.log.delta",
                "log_tails/run-complete/00-postgres.log.tail",
            ]
        );
        assert_eq!(inventory.provenance, vec!["provenance-warnings.txt"]);
        assert_eq!(inventory.failure, vec!["failure-setup.txt"]);
    }

    #[test]
    fn provenance_crash_and_managed_cleanup_artifacts_preserve_diagnostics() {
        let artifacts = TestDir::new("provenance-cleanup");
        let writer = ArtifactWriter::new(artifacts.path().to_path_buf(), Vec::new())
            .expect("artifact writer should initialize");

        let provenance = serde_json::json!({"status": "fail", "probe": "semantic"});
        let path = writer
            .write_provenance(
                &provenance,
                &["module timestamp differs".to_owned()],
                &["module digest differs".to_owned()],
            )
            .expect("provenance should be persisted");
        assert_eq!(path, artifacts.path().join("provenance.json"));
        let warnings = fs::read_to_string(artifacts.path().join("provenance-warnings.txt"))
            .expect("diagnostic text should be readable");
        assert!(warnings.contains("Errors:\n- module digest differs"));
        assert!(warnings.contains("Warnings:\n- module timestamp differs"));

        writer
            .write_crashes(&[CrashedScale {
                workload: "failing|workload".to_owned(),
                rows: 42,
                error: "backend\nterminated".to_owned(),
                repro_command: Some("pg_accel_bench crash-repro".to_owned()),
                plan_snippet_artifact: Some("plan.txt".to_owned()),
                correctness_diff_artifact: Some("correctness.json".to_owned()),
                log_tail_artifacts: vec!["one.log".to_owned(), "two.log".to_owned()],
            }])
            .expect("complete crash evidence should be persisted");
        let crash_markdown = fs::read_to_string(artifacts.path().join("crashes.md"))
            .expect("crash markdown should be readable");
        assert!(crash_markdown.contains("failing\\|workload"));
        assert!(crash_markdown.contains("backend<br>terminated"));
        assert!(crash_markdown.contains("one.log<br>two.log"));

        fs::create_dir_all(artifacts.path().join("correctness_diffs/nested"))
            .expect("managed directory should be created");
        fs::write(artifacts.path().join("report.json"), "stale")
            .expect("managed file should be created");
        fs::write(artifacts.path().join("failure-old.txt"), "stale")
            .expect("managed failure should be created");
        fs::write(artifacts.path().join("keep.me"), "operator-owned")
            .expect("unmanaged file should be created");
        clear_managed_artifacts(artifacts.path()).expect("managed cleanup should succeed");
        assert!(!artifacts.path().join("correctness_diffs").exists());
        assert!(!artifacts.path().join("report.json").exists());
        assert!(!artifacts.path().join("failure-old.txt").exists());
        assert!(artifacts.path().join("keep.me").is_file());
        remove_managed_path(&artifacts.path().join("already-absent"))
            .expect("absent managed paths are idempotent");
    }

    #[test]
    fn log_discovery_and_bounded_readers_handle_rotation_and_truncation() {
        let fixture = TestDir::new("log-discovery");
        let data = fixture.path().join("data");
        fs::create_dir_all(data.join("log")).expect("log directory should be created");
        fs::create_dir_all(data.join("pg_log")).expect("pg_log directory should be created");
        for relative in [
            "postgresql.log",
            "events.csv",
            "pg_accel_otel.jsonl",
            "log/newest.log",
            "pg_log/older.log",
        ] {
            let path = data.join(relative);
            fs::write(path, format!("source={relative}\n")).expect("log fixture should be written");
        }
        fs::write(data.join("ignored.txt"), "not a log").expect("non-log fixture should exist");

        let mut candidates = Vec::new();
        append_pgdata_log_candidates(&mut candidates, &data);
        assert!(candidates.contains(&data.join("pg_accel_panic.log")));
        assert!(candidates.contains(&data.join("pg_accel_traces.jsonl")));
        assert!(
            candidates
                .iter()
                .any(|path| path.ends_with("postgresql.log"))
        );
        assert!(!candidates.iter().any(|path| path.ends_with("ignored.txt")));
        assert!(is_log_like(Path::new("server.log")));
        assert!(!is_log_like(Path::new("server.txt")));
        assert!(is_telemetry_candidate(Path::new("trace.jsonl")));
        assert!(!is_telemetry_candidate(Path::new("trace.log")));

        let bounded = fixture.path().join("bounded.log");
        fs::write(&bounded, "old-line\nnew-line-one\nnew-line-two\n")
            .expect("bounded reader fixture should be written");
        assert_eq!(
            read_tail(&bounded, 18).expect("tail should read"),
            "new-line-two\n"
        );
        assert_eq!(
            read_delta(&bounded, 0, 18).expect("delta should read"),
            "new-line-two\n"
        );
        assert_eq!(
            read_delta(&bounded, u64::MAX, 18).expect("oversized offset should clamp"),
            ""
        );
        let offsets = capture_log_start_offsets(&[bounded, fixture.path().join("missing.log")]);
        assert!(offsets[0].existed);
        assert_eq!(offsets[0].len_bytes, 35);
        assert!(!offsets[1].existed);
        assert_eq!(offsets[1].len_bytes, 0);
    }
}
