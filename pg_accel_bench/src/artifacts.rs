use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::report::{BenchReport, CrashedScale, GucSettings};

const ARTIFACT_SCHEMA_VERSION: u32 = 1;
const ARTIFACT_INDEX_SCHEMA_VERSION: u32 = 1;
const ARTIFACT_INDEX_JSON: &str = "artifact_index.json";
const ARTIFACT_CHECKLIST_MD: &str = "artifact_checklist.md";
const LOG_TAIL_BYTES: u64 = 64 * 1024;
const MAX_LOG_CANDIDATES: usize = 32;

#[derive(Clone, Debug)]
pub struct ArtifactWriter {
    root: PathBuf,
    log_candidates: Vec<PathBuf>,
}

#[derive(Serialize)]
struct Manifest {
    schema_version: u32,
    created_unix_seconds: u64,
    artifact_index_path: &'static str,
    artifact_checklist_path: &'static str,
    log_tail_bytes: u64,
    telemetry_tail_bytes: u64,
    max_log_candidates: usize,
    log_candidates: Vec<String>,
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
struct GucSnapshot<'a> {
    settings: &'a [(String, String)],
    postmaster_start_time: Option<&'a str>,
}

impl ArtifactWriter {
    pub fn new(root: PathBuf, log_candidates: Vec<PathBuf>) -> io::Result<Self> {
        fs::create_dir_all(root.join("plan_snippets"))?;
        fs::create_dir_all(root.join("log_tails"))?;

        let writer = Self {
            root,
            log_candidates: unique_paths(log_candidates),
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

    #[must_use]
    pub fn existing_plan_snippet_artifact(&self, workload: &str, rows: usize) -> Option<String> {
        let path = self.plan_snippet_path(workload, rows);
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
            let source_name = candidate
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("log");
            let file_name = format!("{idx:02}-{}.tail", sanitize_label(source_name));
            let out_path = dir.join(file_name);

            let mut out = String::new();
            let _ = writeln!(out, "source: {}", candidate.display());
            let _ = writeln!(out, "tail_bytes: {LOG_TAIL_BYTES}");
            out.push_str("---\n");
            out.push_str(&tail);
            if !tail.ends_with('\n') {
                out.push('\n');
            }
            fs::write(&out_path, out)?;
            written.push(self.relative_display_path(&out_path));
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
            md.push_str("| Workload | Rows | Error | Plan Snippet | Log Tails | Repro |\n");
            md.push_str("|---|---:|---|---|---|---|\n");
            for crash in crashes {
                let plan = crash.plan_snippet_artifact.as_deref().unwrap_or("-");
                let logs = if crash.log_tail_artifacts.is_empty() {
                    "-".to_owned()
                } else {
                    crash.log_tail_artifacts.join("<br>")
                };
                let repro = crash.repro_command.as_deref().unwrap_or("-");
                let _ = writeln!(
                    md,
                    "| {} | {} | {} | {} | {} | `{}` |",
                    markdown_cell(&crash.workload),
                    crash.rows,
                    markdown_cell(&crash.error),
                    markdown_cell(plan),
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
        self.write_artifact_index()?;
        Ok(())
    }

    fn write_manifest(&self) -> io::Result<()> {
        let manifest = Manifest {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            created_unix_seconds: unix_timestamp_secs(),
            artifact_index_path: ARTIFACT_INDEX_JSON,
            artifact_checklist_path: ARTIFACT_CHECKLIST_MD,
            log_tail_bytes: LOG_TAIL_BYTES,
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
        };
        write_json_io(&self.root.join("manifest.json"), &manifest)?;

        let mut readme = String::new();
        readme.push_str("# pg_accel Benchmark Artifacts\n\n");
        let _ = writeln!(
            readme,
            "Log/telemetry tails are capped at `{LOG_TAIL_BYTES}` bytes per file, with at most \
             `{MAX_LOG_CANDIDATES}` unique candidate files considered.\n"
        );
        readme.push_str(
            "- `artifact_index.json`: machine-readable inventory of generated artifact files, \
             sizes, and modified timestamps.\n",
        );
        readme.push_str(
            "- `artifact_checklist.md`: markdown checklist view of the generated artifact \
             inventory.\n",
        );
        readme.push_str("- `report.json`, `report.md`, `report.csv`: rendered benchmark report.\n");
        readme.push_str("- `crashes.json`, `crashes.md`: crash inventory and repro commands.\n");
        readme.push_str("- `guc_snapshot.json`: PostgreSQL settings observed by the harness.\n");
        readme.push_str(
            "- `provenance.json`: pg_config, SQL metadata, and extension binary hashes.\n",
        );
        readme.push_str(
            "- `provenance-warnings.txt`: provenance gaps or hard-fail reasons, when present.\n",
        );
        readme.push_str("- `plan_snippets/`: EXPLAIN snippets captured before timed execution.\n");
        readme.push_str(
            "- `log_tails/`: bounded PostgreSQL and pg_accel log/telemetry tails per failure \
             and at run completion.\n",
        );
        fs::write(self.root.join("README.md"), readme)?;
        self.write_artifact_index()
    }

    fn write_artifact_index(&self) -> io::Result<()> {
        let mut entries = Vec::new();
        collect_artifact_entries(&self.root, &self.root, &mut entries)?;
        entries.sort_by(|left, right| left.path.cmp(&right.path));

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

    fn plan_snippet_path(&self, workload: &str, rows: usize) -> PathBuf {
        self.root
            .join("plan_snippets")
            .join(format!("{}-{rows}.txt", sanitize_label(workload)))
    }

    fn relative_display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

#[must_use]
pub fn default_run_dir(kind: &str) -> PathBuf {
    PathBuf::from("benchmarks").join("artifacts").join(format!(
        "{}-{}",
        sanitize_label(kind),
        unix_timestamp_secs()
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

    files.sort_by(|a, b| b.0.cmp(&a.0));
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
    children.sort_by_key(|entry| entry.path());

    for entry in children {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_artifact_entries(root, &path, entries)?;
        } else if file_type.is_file() {
            let relative_path = relative_artifact_path(root, &path);
            if matches!(
                relative_path.as_str(),
                ARTIFACT_INDEX_JSON | ARTIFACT_CHECKLIST_MD
            ) {
                continue;
            }
            let metadata = entry.metadata()?;
            entries.push(ArtifactIndexEntry {
                path: relative_path,
                size_bytes: metadata.len(),
                modified_unix_seconds: metadata
                    .modified()
                    .map(system_time_to_unix_secs)
                    .unwrap_or(0),
            });
        }
    }

    Ok(())
}

fn relative_artifact_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
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

    #[test]
    fn sanitize_label_keeps_safe_filename_chars() {
        assert_eq!(
            sanitize_label("gpu/hash agg @ 10M rows"),
            "gpu-hash-agg---10M-rows"
        );
        assert_eq!(sanitize_label("***"), "artifact");
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
        assert_eq!(manifest["telemetry_tail_bytes"], LOG_TAIL_BYTES);
        assert_eq!(manifest["max_log_candidates"], MAX_LOG_CANDIDATES);
        assert_eq!(
            manifest["telemetry_candidates"][0],
            telemetry.display().to_string()
        );

        let readme = fs::read_to_string(artifacts.path().join("README.md"))
            .expect("README should be readable");
        assert!(readme.contains("Log/telemetry tails are capped"));
        assert!(readme.contains(ARTIFACT_INDEX_JSON));
        assert!(readme.contains(ARTIFACT_CHECKLIST_MD));
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
        writer
            .capture_log_tails("run-complete")
            .expect("log tail should be captured");

        let index_text = fs::read_to_string(artifacts.path().join(ARTIFACT_INDEX_JSON))
            .expect("artifact index should be readable");
        let index: Value =
            serde_json::from_str(&index_text).expect("artifact index should be valid json");
        assert_eq!(index["schema_version"], ARTIFACT_INDEX_SCHEMA_VERSION);

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
        assert!(paths.contains(&"README.md"));
        assert!(paths.contains(&"crashes.json"));
        assert!(paths.contains(&"crashes.md"));
        assert!(paths.contains(&"plan_snippets/hash-join-100.txt"));
        assert!(paths.contains(&"log_tails/run-complete/00-pg_accel_otel.jsonl.tail"));
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
        assert!(checklist.contains("| [x] | `plan_snippets/hash-join-100.txt` |"));
        assert!(checklist.contains("not listed in their own file table"));
    }
}
