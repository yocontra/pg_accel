use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::artifacts::BenchmarkQueryIdentity;
use crate::report::CrashedScale;
use crate::runner::{CacheMode, TimingMode};

const RESUME_AUDIT_MANIFEST_JSON: &str = "resume_audit_manifest.json";
const RUN_MANIFEST_JSON: &str = "manifest.json";
const ARTIFACT_INDEX_JSON: &str = "artifact_index.json";
const ARTIFACT_CHECKLIST_MD: &str = "artifact_checklist.md";
const CRASHES_JSON: &str = "crashes.json";
const CRASHES_MD: &str = "crashes.md";
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
    pub accel_query_sql: String,
    pub baseline_query_sql: String,
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
    accel_query_sql: String,
    baseline_query_sql: String,
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
/// The saved effective query pair is also retained for exact comparison with
/// the current workload before any retry can run.
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
    validate_manifest_inventory(source_dir, &manifest.inventory)?;

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
        validate_crash_artifacts(source_dir, &manifest.inventory, crash)?;
        let context = load_pre_risk_context(source_dir, crash)?;
        let context_artifact = pre_risk_context_artifact(&crash.workload, crash.rows);
        require_inventory_path(
            &manifest.inventory,
            "pre-risk context",
            &context_artifact,
            crash,
        )?;
        if context.workload != crash.workload || context.rows != crash.rows {
            return Err(format!(
                "pre-risk context mismatch for {} @ {} rows: found {} @ {} rows",
                crash.workload, crash.rows, context.workload, context.rows
            )
            .into());
        }
        let query_identity = saved_query_identity(&context)?;

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
            accel_query_sql: query_identity.accel_query_sql().to_owned(),
            baseline_query_sql: query_identity.baseline_query_sql().to_owned(),
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

fn validate_manifest_inventory(
    source_dir: &Path,
    inventory: &ResumeArtifactInventory,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut errors = Vec::new();
    for path in [
        RUN_MANIFEST_JSON,
        RESUME_AUDIT_MANIFEST_JSON,
        ARTIFACT_INDEX_JSON,
        ARTIFACT_CHECKLIST_MD,
        CRASHES_JSON,
        CRASHES_MD,
    ] {
        if let Err(error) = validate_existing_artifact(source_dir, path) {
            errors.push(error);
        }
    }

    for (group, paths) in inventory.groups() {
        for path in paths {
            if let Err(error) = validate_existing_artifact(source_dir, path) {
                errors.push(format!("{group}: {error}"));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "resume audit manifest is incomplete:\n- {}",
            errors.join("\n- ")
        )
        .into())
    }
}

fn validate_crash_artifacts(
    source_dir: &Path,
    inventory: &ResumeArtifactInventory,
    crash: &CrashedScale,
) -> Result<(), Box<dyn std::error::Error>> {
    if crash.log_tail_artifacts.is_empty() {
        return Err(format!(
            "crash record for {} @ {} rows has no log artifacts; saved run cannot be audited without terminal scrollback",
            crash.workload, crash.rows
        )
        .into());
    }

    if let Some(path) = crash.plan_snippet_artifact.as_deref() {
        validate_linked_crash_artifact(source_dir, inventory, crash, "plan snippet", path)?;
    }
    if let Some(path) = crash.correctness_diff_artifact.as_deref() {
        validate_linked_crash_artifact(source_dir, inventory, crash, "correctness diff", path)?;
    }
    for path in &crash.log_tail_artifacts {
        validate_linked_crash_artifact(source_dir, inventory, crash, "crash log/context", path)?;
    }

    Ok(())
}

fn validate_linked_crash_artifact(
    source_dir: &Path,
    inventory: &ResumeArtifactInventory,
    crash: &CrashedScale,
    label: &str,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_existing_artifact(source_dir, path).map_err(|error| {
        format!(
            "{} referenced by crash record for {} @ {} rows is invalid: {error}",
            label, crash.workload, crash.rows
        )
    })?;
    require_inventory_path(inventory, label, path, crash)?;
    Ok(())
}

fn require_inventory_path(
    inventory: &ResumeArtifactInventory,
    label: &str,
    path: &str,
    crash: &CrashedScale,
) -> Result<(), Box<dyn std::error::Error>> {
    if inventory.contains_path(path) {
        Ok(())
    } else {
        Err(format!(
            "resume manifest does not list {label} artifact `{path}` for {} @ {} rows",
            crash.workload, crash.rows
        )
        .into())
    }
}

fn validate_existing_artifact(source_dir: &Path, path: &str) -> Result<(), String> {
    validate_relative_artifact_path(path)?;
    let artifact_path = source_dir.join(path);
    if artifact_path.is_file() {
        Ok(())
    } else {
        Err(format!(
            "artifact `{path}` is missing at {}",
            artifact_path.display()
        ))
    }
}

fn validate_relative_artifact_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("artifact path is empty".to_owned());
    }
    let path_ref = Path::new(path);
    if path_ref.is_absolute() {
        return Err(format!("artifact path `{path}` must be relative"));
    }
    for component in path_ref.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(format!(
                "artifact path `{path}` must not contain parent/current directory components"
            ));
        }
    }
    Ok(())
}

fn load_pre_risk_context(
    source_dir: &Path,
    crash: &CrashedScale,
) -> Result<SavedPreRiskContext, Box<dyn std::error::Error>> {
    let context_artifact = pre_risk_context_artifact(&crash.workload, crash.rows);
    let context_path = source_dir.join(&context_artifact);
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
    serde_json::from_str(&context_text).map_err(|err| {
        format!(
            "invalid pre-risk context for {} @ {} rows at {}: {err}",
            crash.workload,
            crash.rows,
            context_path.display()
        )
        .into()
    })
}

fn saved_query_identity(
    context: &SavedPreRiskContext,
) -> Result<BenchmarkQueryIdentity, Box<dyn std::error::Error>> {
    BenchmarkQueryIdentity::from_effective(
        context.accel_query_sql.clone(),
        context.baseline_query_sql.clone(),
    )
    .map_err(|err| {
        format!(
            "invalid saved query identity for {} @ {} rows: {err}",
            context.workload, context.rows
        )
        .into()
    })
}

pub fn validate_retry_cell_query_identity(
    cell: &RetryCell,
    current: &BenchmarkQueryIdentity,
) -> Result<(), Box<dyn std::error::Error>> {
    let saved = BenchmarkQueryIdentity::from_effective(
        cell.accel_query_sql.clone(),
        cell.baseline_query_sql.clone(),
    )?;
    if &saved == current {
        return Ok(());
    }

    let mut mismatched_fields = Vec::new();
    if saved.accel_query_sql() != current.accel_query_sql() {
        mismatched_fields.push("accel_query_sql");
    }
    if saved.baseline_query_sql() != current.baseline_query_sql() {
        mismatched_fields.push("baseline_query_sql");
    }
    Err(format!(
        "resume query identity mismatch for {} @ {} rows: saved {} does not exactly match the current resolved workload query identity",
        cell.workload,
        cell.rows,
        mismatched_fields.join(" and ")
    )
    .into())
}

fn pre_risk_context_artifact(workload: &str, rows: usize) -> String {
    format!("pre_risk_contexts/{}-{rows}.json", sanitize_label(workload))
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

    fn groups(&self) -> [(&'static str, &[String]); 8] {
        [
            ("completed_artifacts", &self.completed),
            ("correctness_artifacts", &self.correctness),
            ("pre_risk_artifacts", &self.pre_risk),
            ("plan_artifacts", &self.plan),
            ("crash_artifacts", &self.crash),
            ("log_artifacts", &self.log),
            ("provenance_artifacts", &self.provenance),
            ("failure_artifacts", &self.failure),
        ]
    }

    fn contains_path(&self, path: &str) -> bool {
        self.groups()
            .into_iter()
            .flat_map(|(_, paths)| paths.iter())
            .any(|candidate| candidate == path)
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

    fn saved_context_value() -> serde_json::Value {
        json!({
            "workload": "query_identity",
            "rows": 100,
            "seed": 42,
            "iterations": 10,
            "warmup": 5,
            "timing_mode": "raw",
            "cache_mode": "warm",
            "realistic_gucs": false,
            "skip_guc_verify": false,
            "capture_plans": true,
            "accel_query_sql": "SELECT accel_fn()",
            "baseline_query_sql": "SELECT native_fn()"
        })
    }

    fn retry_cell(accel_query_sql: &str, baseline_query_sql: &str) -> RetryCell {
        RetryCell {
            workload: "query_identity".to_owned(),
            rows: 100,
            accel_query_sql: accel_query_sql.to_owned(),
            baseline_query_sql: baseline_query_sql.to_owned(),
        }
    }

    #[test]
    fn saved_query_identity_requires_present_nonnull_nonempty_sql() {
        for field in ["accel_query_sql", "baseline_query_sql"] {
            let mut missing = saved_context_value();
            missing
                .as_object_mut()
                .expect("saved context should be an object")
                .remove(field);
            assert!(
                serde_json::from_value::<SavedPreRiskContext>(missing).is_err(),
                "missing {field} must fail"
            );

            let mut null = saved_context_value();
            null[field] = serde_json::Value::Null;
            assert!(
                serde_json::from_value::<SavedPreRiskContext>(null).is_err(),
                "null {field} must fail"
            );

            let mut empty = saved_context_value();
            empty[field] = serde_json::Value::String(" \n\t".to_owned());
            let context: SavedPreRiskContext =
                serde_json::from_value(empty).expect("empty SQL is syntactically a string");
            let error = saved_query_identity(&context).expect_err("empty SQL must fail closed");
            assert!(error.to_string().contains("must be nonempty"));
        }
    }

    #[test]
    fn resume_query_identity_accepts_exact_default_and_explicit_pairs() {
        let default = retry_cell("SELECT 1", "SELECT 1");
        let current_default = BenchmarkQueryIdentity::resolve("SELECT 1".to_owned(), None)
            .expect("default identity should resolve");
        validate_retry_cell_query_identity(&default, &current_default)
            .expect("matching default identity should pass");

        let explicit = retry_cell("SELECT accel_fn()", "SELECT native_fn()");
        let current_explicit = BenchmarkQueryIdentity::resolve(
            "SELECT accel_fn()".to_owned(),
            Some("SELECT native_fn()".to_owned()),
        )
        .expect("explicit identity should resolve");
        validate_retry_cell_query_identity(&explicit, &current_explicit)
            .expect("matching explicit identity should pass");
    }

    #[test]
    fn resume_query_identity_rejects_tampered_swapped_and_override_drift() {
        let explicit = retry_cell("SELECT accel_fn()", "SELECT native_fn()");
        let hostile_current = [
            BenchmarkQueryIdentity::from_effective(
                "SELECT tampered_fn()".to_owned(),
                "SELECT native_fn()".to_owned(),
            )
            .expect("tampered accel identity should resolve"),
            BenchmarkQueryIdentity::from_effective(
                "SELECT accel_fn()".to_owned(),
                "SELECT tampered_fn()".to_owned(),
            )
            .expect("tampered baseline identity should resolve"),
            BenchmarkQueryIdentity::from_effective(
                "SELECT native_fn()".to_owned(),
                "SELECT accel_fn()".to_owned(),
            )
            .expect("swapped identity should resolve"),
        ];
        for current in hostile_current {
            let error = validate_retry_cell_query_identity(&explicit, &current)
                .expect_err("changed query identity must fail closed");
            assert!(error.to_string().contains("does not exactly match"));
        }

        let saved_default = retry_cell("SELECT accel_fn()", "SELECT accel_fn()");
        let current_explicit = BenchmarkQueryIdentity::resolve(
            "SELECT accel_fn()".to_owned(),
            Some("SELECT native_fn()".to_owned()),
        )
        .expect("explicit identity should resolve");
        let error = validate_retry_cell_query_identity(&saved_default, &current_explicit)
            .expect_err("default-to-explicit drift must fail closed");
        assert!(error.to_string().contains("baseline_query_sql"));
    }

    #[test]
    fn load_retry_plan_uses_pre_risk_context_for_crashed_cells() {
        let dir = TestDir::new("plan");
        let pre_risk = "pre_risk_contexts/gpu-hash-agg-100000.json";
        let crash_context = "crash_contexts/crash-001-gpu-hash-agg-100000.txt";
        let crash_log = "log_tails/crash-001-gpu-hash-agg-100000/00-postgres.log.tail";
        write_manifest(
            dir.path(),
            &[pre_risk],
            &[],
            &[],
            &[crash_context, crash_log],
            &[crash_log],
        );
        fs::write(
            dir.path().join(CRASHES_JSON),
            serde_json::to_string_pretty(&vec![CrashedScale {
                workload: "gpu/hash agg".to_owned(),
                rows: 100_000,
                error: "backend disconnected".to_owned(),
                repro_command: None,
                plan_snippet_artifact: None,
                correctness_diff_artifact: None,
                log_tail_artifacts: vec![crash_context.to_owned(), crash_log.to_owned()],
            }])
            .expect("crash json should serialize"),
        )
        .expect("crash inventory should be written");
        fs::write(
            dir.path().join(pre_risk),
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
                "capture_plans": true,
                "accel_query_sql": "SELECT count(*) FROM bench",
                "baseline_query_sql": "SELECT count(*) FROM bench"
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
                accel_query_sql: "SELECT count(*) FROM bench".to_owned(),
                baseline_query_sql: "SELECT count(*) FROM bench".to_owned(),
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
        assert_eq!(plan.manifest_summary.crash, 4);
        assert_eq!(plan.manifest_summary.pre_risk, 1);
        assert_eq!(plan.manifest_summary.log, 1);
    }

    #[test]
    fn write_resume_source_records_source_manifest_and_cells() {
        let source = TestDir::new("source");
        write_manifest(source.path(), &[], &[], &[], &[], &[]);
        fs::write(source.path().join(CRASHES_JSON), "[]").expect("crashes should be written");
        let mut plan = load_retry_plan(source.path()).expect("empty plan should load");
        plan.cells = vec![RetryCell {
            workload: "h3_bulk".to_owned(),
            rows: 100_000,
            accel_query_sql: "SELECT accel_fn()".to_owned(),
            baseline_query_sql: "SELECT native_fn()".to_owned(),
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
        assert_eq!(
            value["retried_cells"][0]["baseline_query_sql"],
            "SELECT native_fn()"
        );
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
        let crash_log = "log_tails/crash-001-h3-bulk-100000/00-postgres.log.tail";
        write_manifest(dir.path(), &[], &[], &[], &[crash_log], &[crash_log]);
        fs::write(
            dir.path().join(CRASHES_JSON),
            serde_json::to_string_pretty(&vec![CrashedScale {
                workload: "h3_bulk".to_owned(),
                rows: 100_000,
                error: "backend disconnected".to_owned(),
                repro_command: None,
                plan_snippet_artifact: None,
                correctness_diff_artifact: None,
                log_tail_artifacts: vec![crash_log.to_owned()],
            }])
            .expect("crash json should serialize"),
        )
        .expect("crash inventory should be written");

        let err = load_retry_plan(dir.path()).expect_err("missing context should fail");
        assert!(err.to_string().contains("missing pre-risk context"));
    }

    #[test]
    fn crash_retry_without_log_artifacts_is_a_hard_error() {
        let dir = TestDir::new("missing-logs");
        write_manifest(dir.path(), &[], &[], &[], &[], &[]);
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

        let err = load_retry_plan(dir.path()).expect_err("missing logs should fail");
        assert!(err.to_string().contains("has no log artifacts"));
    }

    #[test]
    fn missing_linked_crash_artifact_is_a_hard_error() {
        let dir = TestDir::new("missing-linked");
        let crash_log = "log_tails/crash-001-h3-bulk-100000/00-postgres.log.tail";
        write_manifest(dir.path(), &[], &[], &[], &[], &[]);
        fs::write(
            dir.path().join(CRASHES_JSON),
            serde_json::to_string_pretty(&vec![CrashedScale {
                workload: "h3_bulk".to_owned(),
                rows: 100_000,
                error: "backend disconnected".to_owned(),
                repro_command: None,
                plan_snippet_artifact: None,
                correctness_diff_artifact: None,
                log_tail_artifacts: vec![crash_log.to_owned()],
            }])
            .expect("crash json should serialize"),
        )
        .expect("crash inventory should be written");

        let err = load_retry_plan(dir.path()).expect_err("missing linked artifact should fail");
        let err = err.to_string();
        assert!(err.contains("crash log/context"));
        assert!(err.contains("missing"));
    }

    #[test]
    fn stale_manifest_without_pre_risk_entry_is_a_hard_error() {
        let dir = TestDir::new("stale-manifest");
        let crash_log = "log_tails/crash-001-h3-bulk-100000/00-postgres.log.tail";
        write_manifest(dir.path(), &[], &[], &[], &[crash_log], &[crash_log]);
        fs::write(
            dir.path().join(CRASHES_JSON),
            serde_json::to_string_pretty(&vec![CrashedScale {
                workload: "h3_bulk".to_owned(),
                rows: 100_000,
                error: "backend disconnected".to_owned(),
                repro_command: None,
                plan_snippet_artifact: None,
                correctness_diff_artifact: None,
                log_tail_artifacts: vec![crash_log.to_owned()],
            }])
            .expect("crash json should serialize"),
        )
        .expect("crash inventory should be written");
        fs::write(
            dir.path()
                .join("pre_risk_contexts")
                .join("h3_bulk-100000.json"),
            json!({
                "workload": "h3_bulk",
                "rows": 100_000,
                "seed": 42,
                "iterations": 10,
                "warmup": 5,
                "timing_mode": "both",
                "cache_mode": "warm",
                "realistic_gucs": false,
                "skip_guc_verify": false,
                "capture_plans": true,
                "accel_query_sql": "SELECT accel_fn()",
                "baseline_query_sql": "SELECT native_fn()"
            })
            .to_string(),
        )
        .expect("pre-risk context should be written");

        let err = load_retry_plan(dir.path()).expect_err("stale manifest should fail");
        assert!(err.to_string().contains("does not list pre-risk context"));
    }

    #[test]
    fn manifest_inventory_missing_artifact_is_a_hard_error() {
        let dir = TestDir::new("missing-inventory-artifact");
        write_base_files(dir.path());
        fs::write(
            dir.path().join(RESUME_AUDIT_MANIFEST_JSON),
            json!({
                "schema_version": RESUME_AUDIT_SCHEMA_VERSION,
                "inventory": {
                    "completed_artifacts": [],
                    "correctness_artifacts": ["correctness_diffs/missing.json"],
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

        let err = load_retry_plan(dir.path()).expect_err("missing inventory file should fail");
        let err = err.to_string();
        assert!(err.contains("resume audit manifest is incomplete"));
        assert!(err.contains("correctness_artifacts"));
    }

    #[test]
    fn parsers_cover_aliases_unknown_modes_and_safe_artifact_labels() {
        for raw in ["raw", "RAW-WALLCLOCK", "wall", "wall-clock"] {
            assert_eq!(parse_timing_mode(raw), Ok(TimingMode::RawWallClock));
        }
        for raw in ["explain", "EXPLAIN-ANALYZE", "analyze"] {
            assert_eq!(parse_timing_mode(raw), Ok(TimingMode::ExplainAnalyze));
        }
        assert_eq!(parse_timing_mode("both"), Ok(TimingMode::Both));
        assert!(parse_timing_mode("cpu-time").is_err());
        assert_eq!(parse_cache_mode("WARM"), Ok(CacheMode::Warm));
        assert_eq!(parse_cache_mode("cold"), Ok(CacheMode::Cold));
        assert_eq!(parse_cache_mode("both"), Ok(CacheMode::Both));
        assert!(parse_cache_mode("mixed").is_err());

        assert_eq!(timing_mode_arg(TimingMode::ExplainAnalyze), "explain");
        assert_eq!(timing_mode_arg(TimingMode::RawWallClock), "raw");
        assert_eq!(timing_mode_arg(TimingMode::Both), "both");
        assert_eq!(cache_mode_arg(CacheMode::Cold), "cold");
        assert_eq!(cache_mode_arg(CacheMode::Warm), "warm");
        assert_eq!(cache_mode_arg(CacheMode::Both), "both");

        assert_eq!(sanitize_label(" -- "), "artifact");
        assert_eq!(sanitize_label("gpu/hash agg"), "gpu-hash-agg");
        assert_eq!(sanitize_label("safe_name.1"), "safe_name.1");
        assert_eq!(sanitize_label(&"x".repeat(200)).len(), 96);
        assert_eq!(
            pre_risk_context_artifact("gpu/hash agg", 123),
            "pre_risk_contexts/gpu-hash-agg-123.json"
        );
    }

    #[test]
    fn artifact_path_validation_rejects_escape_and_non_normal_components() {
        assert!(validate_relative_artifact_path("artifact.json").is_ok());
        for path in [
            "",
            "/tmp/outside",
            "../outside",
            "a/../outside",
            "./artifact",
        ] {
            assert!(
                validate_relative_artifact_path(path).is_err(),
                "path={path:?}"
            );
        }
        let dir = TestDir::new("relative-artifacts");
        assert!(validate_existing_artifact(dir.path(), "missing.json").is_err());
        assert!(validate_existing_artifact(dir.path(), "../outside").is_err());
        write_artifact(dir.path(), "nested/present.json");
        assert!(validate_existing_artifact(dir.path(), "nested/present.json").is_ok());
    }

    #[test]
    fn retry_plan_rejects_missing_invalid_and_wrong_schema_manifests() {
        let missing = TestDir::new("missing-manifest");
        let error = load_retry_plan(missing.path()).expect_err("missing manifest");
        assert!(error.to_string().contains("resume manifest not readable"));

        let invalid = TestDir::new("invalid-manifest");
        fs::write(invalid.path().join(RESUME_AUDIT_MANIFEST_JSON), "not json")
            .expect("invalid manifest");
        assert!(load_retry_plan(invalid.path()).is_err());

        let wrong_schema = TestDir::new("wrong-schema");
        write_base_files(wrong_schema.path());
        fs::write(
            wrong_schema.path().join(RESUME_AUDIT_MANIFEST_JSON),
            json!({
                "schema_version": RESUME_AUDIT_SCHEMA_VERSION + 1,
                "inventory": {
                    "completed_artifacts": [],
                    "correctness_artifacts": [],
                    "pre_risk_artifacts": [],
                    "plan_artifacts": [],
                    "crash_artifacts": [],
                    "log_artifacts": [],
                    "provenance_artifacts": [],
                    "failure_artifacts": []
                }
            })
            .to_string(),
        )
        .expect("wrong schema manifest");
        let error = load_retry_plan(wrong_schema.path()).expect_err("wrong schema");
        assert!(
            error
                .to_string()
                .contains("unsupported resume audit manifest schema")
        );
    }

    #[test]
    fn retry_plan_rejects_invalid_crash_json_and_context_identity_mismatch() {
        let invalid_crashes = TestDir::new("invalid-crashes");
        write_manifest(invalid_crashes.path(), &[], &[], &[], &[], &[]);
        fs::write(invalid_crashes.path().join(CRASHES_JSON), "not json")
            .expect("invalid crash json");
        assert!(load_retry_plan(invalid_crashes.path()).is_err());

        let mismatch = TestDir::new("context-mismatch");
        let pre_risk = "pre_risk_contexts/h3_bulk-100000.json";
        let crash_log = "log_tails/h3_bulk.log";
        write_manifest(
            mismatch.path(),
            &[pre_risk],
            &[],
            &[],
            &[crash_log],
            &[crash_log],
        );
        fs::write(
            mismatch.path().join(CRASHES_JSON),
            serde_json::to_string(&vec![CrashedScale {
                workload: "h3_bulk".to_owned(),
                rows: 100_000,
                error: "reset".to_owned(),
                repro_command: None,
                plan_snippet_artifact: None,
                correctness_diff_artifact: None,
                log_tail_artifacts: vec![crash_log.to_owned()],
            }])
            .expect("crash json"),
        )
        .expect("write crashes");
        let mut context = saved_context_value();
        context["workload"] = json!("other_workload");
        context["rows"] = json!(100_000);
        fs::write(mismatch.path().join(pre_risk), context.to_string())
            .expect("write mismatched context");
        let error = load_retry_plan(mismatch.path()).expect_err("context mismatch");
        assert!(error.to_string().contains("pre-risk context mismatch"));
    }

    #[test]
    fn retry_plan_rejects_mixed_configs_across_crashed_cells() {
        let dir = TestDir::new("mixed-configs");
        let first_context = "pre_risk_contexts/first-100.json";
        let second_context = "pre_risk_contexts/second-200.json";
        let first_log = "log_tails/first.log";
        let second_log = "log_tails/second.log";
        write_manifest(
            dir.path(),
            &[first_context, second_context],
            &[],
            &[],
            &[first_log, second_log],
            &[first_log, second_log],
        );
        let crashes = vec![
            CrashedScale {
                workload: "first".to_owned(),
                rows: 100,
                error: "reset".to_owned(),
                repro_command: None,
                plan_snippet_artifact: None,
                correctness_diff_artifact: None,
                log_tail_artifacts: vec![first_log.to_owned()],
            },
            CrashedScale {
                workload: "second".to_owned(),
                rows: 200,
                error: "reset".to_owned(),
                repro_command: None,
                plan_snippet_artifact: None,
                correctness_diff_artifact: None,
                log_tail_artifacts: vec![second_log.to_owned()],
            },
        ];
        fs::write(
            dir.path().join(CRASHES_JSON),
            serde_json::to_string(&crashes).expect("crashes"),
        )
        .expect("write crashes");
        for (path, workload, rows, iterations) in [
            (first_context, "first", 100, 3),
            (second_context, "second", 200, 4),
        ] {
            fs::write(
                dir.path().join(path),
                json!({
                    "workload": workload,
                    "rows": rows,
                    "seed": 42,
                    "iterations": iterations,
                    "warmup": 1,
                    "timing_mode": "both",
                    "cache_mode": "cold",
                    "realistic_gucs": false,
                    "skip_guc_verify": false,
                    "capture_plans": true,
                    "accel_query_sql": "SELECT 1",
                    "baseline_query_sql": "SELECT 1"
                })
                .to_string(),
            )
            .expect("write context");
        }
        let error = load_retry_plan(dir.path()).expect_err("mixed configs");
        assert!(error.to_string().contains("mixed benchmark configs"));
        assert!(error.to_string().contains("second @ 200 rows"));
    }

    #[test]
    fn optional_crash_artifacts_must_exist_and_be_in_manifest() {
        let dir = TestDir::new("optional-linked");
        let crash = CrashedScale {
            workload: "linked".to_owned(),
            rows: 100,
            error: "reset".to_owned(),
            repro_command: None,
            plan_snippet_artifact: Some("plans/linked.txt".to_owned()),
            correctness_diff_artifact: Some("correctness/linked.json".to_owned()),
            log_tail_artifacts: vec!["logs/linked.txt".to_owned()],
        };
        for path in [
            "plans/linked.txt",
            "correctness/linked.json",
            "logs/linked.txt",
        ] {
            write_artifact(dir.path(), path);
        }
        let inventory = ResumeArtifactInventory {
            completed: Vec::new(),
            correctness: vec!["correctness/linked.json".to_owned()],
            pre_risk: Vec::new(),
            plan: vec!["plans/linked.txt".to_owned()],
            crash: vec!["logs/linked.txt".to_owned()],
            log: vec!["logs/linked.txt".to_owned()],
            provenance: Vec::new(),
            failure: Vec::new(),
        };
        validate_crash_artifacts(dir.path(), &inventory, &crash)
            .expect("all optional linked artifacts should validate");
    }

    fn write_manifest(
        root: &Path,
        pre_risk: &[&str],
        plan: &[&str],
        correctness: &[&str],
        crash_extra: &[&str],
        log: &[&str],
    ) {
        write_base_files(root);
        for path in pre_risk
            .iter()
            .chain(plan)
            .chain(correctness)
            .chain(crash_extra)
            .chain(log)
        {
            write_artifact(root, path);
        }

        let mut crash_artifacts = vec![CRASHES_JSON, CRASHES_MD];
        crash_artifacts.extend_from_slice(crash_extra);

        fs::write(
            root.join(RESUME_AUDIT_MANIFEST_JSON),
            json!({
                "schema_version": RESUME_AUDIT_SCHEMA_VERSION,
                "inventory": {
                    "completed_artifacts": [],
                    "correctness_artifacts": correctness,
                    "pre_risk_artifacts": pre_risk,
                    "plan_artifacts": plan,
                    "crash_artifacts": crash_artifacts,
                    "log_artifacts": log,
                    "provenance_artifacts": [],
                    "failure_artifacts": []
                }
            })
            .to_string(),
        )
        .expect("manifest should be written");
    }

    fn write_base_files(root: &Path) {
        fs::write(root.join(RUN_MANIFEST_JSON), "{}\n").expect("run manifest should be written");
        fs::write(root.join(ARTIFACT_INDEX_JSON), "{}\n")
            .expect("artifact index should be written");
        fs::write(root.join(ARTIFACT_CHECKLIST_MD), "# Artifact Checklist\n")
            .expect("artifact checklist should be written");
        fs::write(root.join(CRASHES_JSON), "[]\n").expect("crashes json should be written");
        fs::write(root.join(CRASHES_MD), "# Crash List\n").expect("crashes md should be written");
    }

    fn write_artifact(root: &Path, relative: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("artifact parent should be created");
        }
        fs::write(path, "artifact\n").expect("artifact should be written");
    }
}
