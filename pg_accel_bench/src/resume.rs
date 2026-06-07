use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::report::CrashedScale;
use crate::runner::{CacheMode, TimingMode};

const RESUME_AUDIT_MANIFEST_JSON: &str = "resume_audit_manifest.json";
const CRASHES_JSON: &str = "crashes.json";
const RESUME_SOURCE_JSON: &str = "resume_source.json";
const RESUME_AUDIT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryConfig {
    pub seed: u64,
    pub iterations: usize,
    pub warmup: usize,
    pub timing_mode: TimingMode,
    pub cache_mode: CacheMode,
    pub realistic_gucs: bool,
    pub skip_guc_verify: bool,
    pub capture_plans: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RetryCell {
    pub workload: String,
    pub rows: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestSummary {
    pub completed: usize,
    pub correctness: usize,
    pub pre_risk: usize,
    pub plan: usize,
    pub crash: usize,
    pub log: usize,
    pub provenance: usize,
    pub failure: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryPlan {
    pub source_dir: PathBuf,
    pub source_manifest: PathBuf,
    pub manifest_summary: ManifestSummary,
    pub cells: Vec<RetryCell>,
    pub config: Option<RetryConfig>,
}

#[derive(Deserialize)]
struct ResumeAuditManifest {
    schema_version: u32,
    inventory: ResumeArtifactInventory,
}

#[derive(Deserialize)]
struct ResumeArtifactInventory {
    #[serde(rename = "completed_artifacts")]
    completed: Vec<String>,
    #[serde(default, rename = "correctness_artifacts")]
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

#[derive(Deserialize)]
struct SavedPreRiskContext {
    workload: String,
    rows: usize,
    seed: u64,
    iterations: usize,
    warmup: usize,
    timing_mode: String,
    cache_mode: String,
    realistic_gucs: bool,
    skip_guc_verify: bool,
    capture_plans: bool,
}

#[derive(Serialize)]
struct ResumeSourceArtifact<'a> {
    source_artifact_dir: String,
    source_manifest_path: String,
    retried_cells: &'a [RetryCell],
    config: Option<RetryConfigArtifact>,
}

#[derive(Serialize)]
struct RetryConfigArtifact {
    seed: u64,
    iterations: usize,
    warmup: usize,
    timing_mode: &'static str,
    cache_mode: &'static str,
    realistic_gucs: bool,
    skip_guc_verify: bool,
    capture_plans: bool,
}

impl RetryConfig {
    #[must_use]
    pub const fn timing_arg(&self) -> &'static str {
        timing_mode_arg(self.timing_mode)
    }

    #[must_use]
    pub const fn cache_arg(&self) -> &'static str {
        cache_mode_arg(self.cache_mode)
    }
}

/// Read an artifact directory and build the retry plan for crashed cells.
///
/// The retry plan intentionally uses `pre_risk_contexts/<workload>-<rows>.json`
/// as the source of truth for seed, timing, cache, GUC, and plan-capture
/// settings. Those contexts are written before risky execution, so they
/// survive backend crashes and let the resume path avoid terminal scrollback.
pub fn load_retry_plan(source_dir: &Path) -> Result<RetryPlan, Box<dyn std::error::Error>> {
    let manifest_path = source_dir.join(RESUME_AUDIT_MANIFEST_JSON);
    let manifest_text = fs::read_to_string(&manifest_path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "resume manifest not readable at {}: {err}",
                manifest_path.display()
            ),
        )
    })?;
    let manifest: ResumeAuditManifest = serde_json::from_str(&manifest_text)?;
    if manifest.schema_version != RESUME_AUDIT_SCHEMA_VERSION {
        return Err(format!(
            "unsupported resume audit manifest schema version {} (expected {RESUME_AUDIT_SCHEMA_VERSION})",
            manifest.schema_version
        )
        .into());
    }

    let crashes_path = source_dir.join(CRASHES_JSON);
    let crashes_text = fs::read_to_string(&crashes_path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "crash inventory not readable at {}: {err}",
                crashes_path.display()
            ),
        )
    })?;
    let crashes: Vec<CrashedScale> = serde_json::from_str(&crashes_text)?;

    let mut cells = Vec::with_capacity(crashes.len());
    let mut shared_config: Option<RetryConfig> = None;
    for crash in &crashes {
        let context = load_pre_risk_context(source_dir, crash)?;
        if context.workload != crash.workload || context.rows != crash.rows {
            return Err(format!(
                "pre-risk context mismatch for {} @ {} rows: found {} @ {} rows",
                crash.workload, crash.rows, context.workload, context.rows
            )
            .into());
        }

        let config = retry_config_from_context(&context)?;
        if let Some(existing) = &shared_config {
            if existing != &config {
                return Err(format!(
                    "resume source has mixed benchmark configs; first config is {} iterations/{} warmup/seed {}, \
                     but {} @ {} rows uses {} iterations/{} warmup/seed {}",
                    existing.iterations,
                    existing.warmup,
                    existing.seed,
                    crash.workload,
                    crash.rows,
                    config.iterations,
                    config.warmup,
                    config.seed
                )
                .into());
            }
        } else {
            shared_config = Some(config);
        }

        cells.push(RetryCell {
            workload: crash.workload.clone(),
            rows: crash.rows,
        });
    }

    Ok(RetryPlan {
        source_dir: source_dir.to_path_buf(),
        source_manifest: manifest_path,
        manifest_summary: manifest.inventory.summary(),
        cells,
        config: shared_config,
    })
}

pub fn write_resume_source_artifact(output_dir: &Path, plan: &RetryPlan) -> io::Result<PathBuf> {
    fs::create_dir_all(output_dir)?;
    let path = output_dir.join(RESUME_SOURCE_JSON);
    let artifact = ResumeSourceArtifact {
        source_artifact_dir: plan.source_dir.display().to_string(),
        source_manifest_path: plan.source_manifest.display().to_string(),
        retried_cells: &plan.cells,
        config: plan.config.as_ref().map(RetryConfigArtifact::from),
    };
    let json = serde_json::to_string_pretty(&artifact).map_err(io::Error::other)?;
    fs::write(&path, json)?;
    Ok(path)
}

fn load_pre_risk_context(
    source_dir: &Path,
    crash: &CrashedScale,
) -> Result<SavedPreRiskContext, Box<dyn std::error::Error>> {
    let context_path = source_dir.join("pre_risk_contexts").join(format!(
        "{}-{}.json",
        sanitize_label(&crash.workload),
        crash.rows
    ));
    let context_text = fs::read_to_string(&context_path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "missing pre-risk context for {} @ {} rows at {}: {err}",
                crash.workload,
                crash.rows,
                context_path.display()
            ),
        )
    })?;
    Ok(serde_json::from_str(&context_text)?)
}

fn retry_config_from_context(
    context: &SavedPreRiskContext,
) -> Result<RetryConfig, Box<dyn std::error::Error>> {
    Ok(RetryConfig {
        seed: context.seed,
        iterations: context.iterations,
        warmup: context.warmup,
        timing_mode: parse_timing_mode(&context.timing_mode)?,
        cache_mode: parse_cache_mode(&context.cache_mode)?,
        realistic_gucs: context.realistic_gucs,
        skip_guc_verify: context.skip_guc_verify,
        capture_plans: context.capture_plans,
    })
}

fn parse_timing_mode(value: &str) -> Result<TimingMode, String> {
    match value.to_ascii_lowercase().as_str() {
        "raw" | "raw-wallclock" | "wall" | "wall-clock" => Ok(TimingMode::RawWallClock),
        "explain" | "explain-analyze" | "analyze" => Ok(TimingMode::ExplainAnalyze),
        "both" => Ok(TimingMode::Both),
        other => Err(format!("unknown timing mode in pre-risk context: {other}")),
    }
}

fn parse_cache_mode(value: &str) -> Result<CacheMode, String> {
    match value.to_ascii_lowercase().as_str() {
        "warm" => Ok(CacheMode::Warm),
        "cold" => Ok(CacheMode::Cold),
        "both" => Ok(CacheMode::Both),
        other => Err(format!("unknown cache mode in pre-risk context: {other}")),
    }
}

const fn timing_mode_arg(mode: TimingMode) -> &'static str {
    match mode {
        TimingMode::ExplainAnalyze => "explain",
        TimingMode::RawWallClock => "raw",
        TimingMode::Both => "both",
    }
}

const fn cache_mode_arg(mode: CacheMode) -> &'static str {
    match mode {
        CacheMode::Cold => "cold",
        CacheMode::Warm => "warm",
        CacheMode::Both => "both",
    }
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

impl ResumeArtifactInventory {
    fn summary(&self) -> ManifestSummary {
        ManifestSummary {
            completed: self.completed.len(),
            correctness: self.correctness.len(),
            pre_risk: self.pre_risk.len(),
            plan: self.plan.len(),
            crash: self.crash.len(),
            log: self.log.len(),
            provenance: self.provenance.len(),
            failure: self.failure.len(),
        }
    }
}

impl From<&RetryConfig> for RetryConfigArtifact {
    fn from(value: &RetryConfig) -> Self {
        Self {
            seed: value.seed,
            iterations: value.iterations,
            warmup: value.warmup,
            timing_mode: value.timing_arg(),
            cache_mode: value.cache_arg(),
            realistic_gucs: value.realistic_gucs,
            skip_guc_verify: value.skip_guc_verify,
            capture_plans: value.capture_plans,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

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
                "pg_accel_resume_{label}_{}_{}",
                std::process::id(),
                nanos
            ));
            fs::create_dir_all(path.join("pre_risk_contexts"))
                .expect("test artifact directory should be created");
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
    fn load_retry_plan_uses_pre_risk_context_for_crashed_cells() {
        let dir = TestDir::new("plan");
        write_manifest(dir.path());
        fs::write(
            dir.path().join(CRASHES_JSON),
            serde_json::to_string_pretty(&vec![CrashedScale {
                workload: "gpu/hash agg".to_owned(),
                rows: 100_000,
                error: "backend disconnected".to_owned(),
                repro_command: None,
                plan_snippet_artifact: None,
                correctness_diff_artifact: None,
                log_tail_artifacts: Vec::new(),
            }])
            .expect("crash json should serialize"),
        )
        .expect("crash inventory should be written");
        fs::write(
            dir.path()
                .join("pre_risk_contexts")
                .join("gpu-hash-agg-100000.json"),
            json!({
                "workload": "gpu/hash agg",
                "rows": 100_000,
                "seed": 7,
                "iterations": 11,
                "warmup": 6,
                "timing_mode": "raw",
                "cache_mode": "warm",
                "realistic_gucs": true,
                "skip_guc_verify": true,
                "capture_plans": true
            })
            .to_string(),
        )
        .expect("pre-risk context should be written");

        let plan = load_retry_plan(dir.path()).expect("retry plan should load");
        assert_eq!(
            plan.cells,
            vec![RetryCell {
                workload: "gpu/hash agg".to_owned(),
                rows: 100_000,
            }]
        );
        assert_eq!(
            plan.config,
            Some(RetryConfig {
                seed: 7,
                iterations: 11,
                warmup: 6,
                timing_mode: TimingMode::RawWallClock,
                cache_mode: CacheMode::Warm,
                realistic_gucs: true,
                skip_guc_verify: true,
                capture_plans: true,
            })
        );
        assert_eq!(plan.manifest_summary.crash, 2);
    }

    #[test]
    fn write_resume_source_records_source_manifest_and_cells() {
        let source = TestDir::new("source");
        write_manifest(source.path());
        fs::write(source.path().join(CRASHES_JSON), "[]").expect("crashes should be written");
        let mut plan = load_retry_plan(source.path()).expect("empty plan should load");
        plan.cells = vec![RetryCell {
            workload: "h3_bulk".to_owned(),
            rows: 100_000,
        }];
        plan.config = Some(RetryConfig {
            seed: 42,
            iterations: 10,
            warmup: 5,
            timing_mode: TimingMode::Both,
            cache_mode: CacheMode::Cold,
            realistic_gucs: false,
            skip_guc_verify: false,
            capture_plans: true,
        });

        let output = TestDir::new("output");
        let path = write_resume_source_artifact(output.path(), &plan)
            .expect("resume source artifact should be written");
        let text = fs::read_to_string(path).expect("resume source artifact should be readable");
        let value: serde_json::Value =
            serde_json::from_str(&text).expect("resume source artifact should be valid json");
        assert_eq!(value["retried_cells"][0]["workload"], "h3_bulk");
        assert_eq!(value["config"]["timing_mode"], "both");
        assert_eq!(value["config"]["cache_mode"], "cold");
        assert_eq!(
            value["source_manifest_path"],
            source
                .path()
                .join(RESUME_AUDIT_MANIFEST_JSON)
                .display()
                .to_string()
        );
    }

    #[test]
    fn missing_pre_risk_context_is_a_hard_error_for_crash_retry() {
        let dir = TestDir::new("missing-context");
        write_manifest(dir.path());
        fs::write(
            dir.path().join(CRASHES_JSON),
            serde_json::to_string_pretty(&vec![CrashedScale {
                workload: "h3_bulk".to_owned(),
                rows: 100_000,
                error: "backend disconnected".to_owned(),
                repro_command: None,
                plan_snippet_artifact: None,
                correctness_diff_artifact: None,
                log_tail_artifacts: Vec::new(),
            }])
            .expect("crash json should serialize"),
        )
        .expect("crash inventory should be written");

        let err = load_retry_plan(dir.path()).expect_err("missing context should fail");
        assert!(err.to_string().contains("missing pre-risk context"));
    }

    fn write_manifest(root: &Path) {
        fs::write(
            root.join(RESUME_AUDIT_MANIFEST_JSON),
            json!({
                "schema_version": RESUME_AUDIT_SCHEMA_VERSION,
                "inventory": {
                    "completed_artifacts": ["report.json"],
                    "pre_risk_artifacts": [],
                    "plan_artifacts": [],
                    "crash_artifacts": ["crashes.json", "crashes.md"],
                    "log_artifacts": [],
                    "provenance_artifacts": ["provenance.json"],
                    "failure_artifacts": []
                }
            })
            .to_string(),
        )
        .expect("manifest should be written");
    }
}
