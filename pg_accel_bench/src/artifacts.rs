use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::report::{BenchReport, CrashedScale, GucSettings};

const ARTIFACT_SCHEMA_VERSION: u32 = 1;
const LOG_TAIL_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug)]
pub struct ArtifactWriter {
    root: PathBuf,
    log_candidates: Vec<PathBuf>,
}

#[derive(Serialize)]
struct Manifest {
    schema_version: u32,
    created_unix_seconds: u64,
    log_tail_bytes: u64,
    log_candidates: Vec<String>,
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
        Ok(())
    }

    pub fn write_failure(&self, label: &str, error: &str) -> io::Result<PathBuf> {
        let path = self
            .root
            .join(format!("failure-{}.txt", sanitize_label(label)));
        fs::write(&path, format!("{error}\n"))?;
        Ok(path)
    }

    pub fn write_report(&self, report: &BenchReport) -> Result<(), Box<dyn std::error::Error>> {
        fs::write(self.root.join("report.json"), report.to_json()?)?;
        fs::write(self.root.join("report.md"), report.to_markdown())?;
        fs::write(self.root.join("report.csv"), report.to_csv())?;
        Ok(())
    }

    fn write_manifest(&self) -> io::Result<()> {
        let manifest = Manifest {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            created_unix_seconds: unix_timestamp_secs(),
            log_tail_bytes: LOG_TAIL_BYTES,
            log_candidates: self
                .log_candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
        };
        write_json_io(&self.root.join("manifest.json"), &manifest)?;

        let mut readme = String::new();
        readme.push_str("# pg_accel Benchmark Artifacts\n\n");
        readme.push_str("- `report.json`, `report.md`, `report.csv`: rendered benchmark report.\n");
        readme.push_str("- `crashes.json`, `crashes.md`: crash inventory and repro commands.\n");
        readme.push_str("- `guc_snapshot.json`: PostgreSQL settings observed by the harness.\n");
        readme.push_str("- `plan_snippets/`: EXPLAIN snippets captured before timed execution.\n");
        readme.push_str("- `log_tails/`: bounded PostgreSQL and pg_accel log tails per failure.\n");
        fs::write(self.root.join("README.md"), readme)
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
        out.push(home.join(".pgrx").join("17.log"));
        append_pgdata_log_candidates(&mut out, &home.join(".pgrx").join("data-17"));
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
        }
    }
    out
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
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
}
