#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Arm {
    Legacy,
    Raw,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Phase {
    Warmup,
    Warm,
    FreshArtifact,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub enum Key {
    Null,
    I16(i16),
    I32(i32),
    I64(i64),
}
// Preserve every produced tuple. Never silently overwrite a duplicate key.
pub type Rows = Vec<(Key, i64)>;

// Owned encoded neutral spec/projection only. Capture each candidate independently
// AFTER actual ordinary lowering; never relabel Raw words as Legacy.
#[derive(Clone)]
pub struct Recipe {
    pub arm: Arm,
    pub captured: crate::engine::ffi::planner_hooks::PairedRecipe,
    pub cost_evidence: String,
    pub relation_generation: u64,
    pub key_type_oid: u32,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SuccessfulV2Leaf {
    pub whole_where_rows: u64,
    pub partition_rows: u64,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct Evidence {
    pub physical_mode: String,
    pub completed: bool,
    pub bridge_delta: u64,
    pub scheduled_calls: u64,
    pub observed_calls: u64,
    // Data-bearing verified calls exclude RESET/FINALIZE lifecycle controls.
    pub verified_data_calls: u64,
    pub mask_calls: u64,
    pub control_calls: u64,
    pub stock_fallback_delta: u64,
    pub input_rows: u64,
    pub generation: u64,
    pub natural_capacity: Option<usize>,
    pub explicit_capacity_override: Option<usize>,
    pub artifact_cache_hit: bool,
    pub construction_bytes: u64,
    pub built_artifact_preparation_us: u64,
    pub resident_load_included: bool,
    // Must be sourced from native successful-v2 result boundary, never inferred
    // from parallel_hash or from an absent overflow-only EXPLAIN block.
    pub successful_v2_leaves: Vec<SuccessfulV2Leaf>,
    pub preparation_ns: u128,
    pub dispatch_ns: Option<u128>,
    pub consumption_ns: u128,
    pub descriptor_executor_ns: u128,
    pub diagnostic_capture_ns: u128,
}
pub struct Run {
    pub rows: Rows,
    pub evidence: Evidence,
}
#[derive(serde::Serialize)]
pub struct Sample {
    pub fixture: &'static str,
    pub phase: Phase,
    pub pair: usize,
    pub position: usize,
    pub arm: Arm,
    pub evidence: Evidence,
    pub rows: Rows,
}

/// Implementation belongs inside the test-only PG module; MUST use production
/// installed owner/error boundary/AggExecState, not execute_prepared directly.
/// No method may hold a RefCell or residency borrow over PostgreSQL service.
pub trait Backend {
    fn begin_sample(
        &mut self,
        fixture: &'static str,
        phase: Phase,
        pair: usize,
        position: usize,
        arm: Arm,
    ) -> Result<(), String>;
    fn assert_default_gucs_and_no_overrides(&mut self) -> Result<(), String>;
    fn stock_full_result(&mut self, sql: &str) -> Result<Rows, String>;
    fn capture_independent_recipes(&mut self, sql: &str) -> Result<[Recipe; 2], String>;
    fn prepare_jit_outside_measurements(&mut self, recipes: &[Recipe; 2]) -> Result<(), String>;
    /// Missing targeted invalidation implementation MUST return Err, never
    /// substitute global cleanup or falsely label a reload as artifact-only.
    fn invalidate_owned_derived_only(&mut self, recipe: &Recipe) -> Result<(), String>;
    fn execute_owned_to_eof(&mut self, recipe: &Recipe) -> Result<Run, String>;
    /// Persist every sample immediately outside the timed interval; bounded,
    /// append-only evidence output. Return Err on write failure.
    fn retain(&mut self, sample: &Sample) -> Result<(), String>;
    /// Append the verdict separately; the full raw sample must already exist.
    fn retain_verdict(
        &mut self,
        fixture: &'static str,
        phase: Phase,
        pair: usize,
        position: usize,
        error: Option<&str>,
    ) -> Result<(), String>;
    fn retain_error(
        &mut self,
        fixture: &'static str,
        phase: Phase,
        pair: usize,
        position: usize,
        arm: Arm,
        error: &str,
    ) -> Result<(), String>;
    fn verify_closed_owner_and_raw_residency(&mut self) -> Result<(), String>;
}

pub(super) fn validate(recipe: &Recipe, run: &Run, expected: &Rows) -> Result<(), String> {
    let e = &run.evidence;
    if run.rows.iter().any(|(key, _)| {
        !matches!(
            (recipe.key_type_oid, key),
            (_, Key::Null) | (21, Key::I16(_)) | (23, Key::I32(_)) | (20, Key::I64(_))
        )
    }) {
        return Err("result key did not retain its declared type".into());
    }
    if e.preparation_ns
        .checked_add(e.consumption_ns)
        .is_none_or(|n| n > e.descriptor_executor_ns)
        || e.dispatch_ns.is_none()
        || e.diagnostic_capture_ns > e.descriptor_executor_ns
    {
        return Err("invalid measured interval decomposition".into());
    }
    let mut actual = run.rows.clone();
    let mut oracle = expected.clone();
    actual.sort();
    oracle.sort();
    if actual != oracle
        || !e.completed
        || e.stock_fallback_delta != 0
        || e.explicit_capacity_override.is_some()
        || e.bridge_delta != e.scheduled_calls
        || e.observed_calls != e.scheduled_calls
        || e.verified_data_calls == 0
        || e.verified_data_calls > e.observed_calls
        || e.verified_data_calls
            .checked_add(e.mask_calls)
            .and_then(|calls| calls.checked_add(e.control_calls))
            != Some(e.observed_calls)
    {
        return Err("parity/lifecycle/owner/generation proof failed".into());
    }
    if e.generation != recipe.relation_generation {
        return Err("resident generation proof failed".into());
    }
    if recipe.arm == Arm::Raw {
        if e.physical_mode != "parallel_hash"
            || e.successful_v2_leaves.is_empty()
            || e.natural_capacity.is_none_or(|capacity| capacity == 0)
        {
            return Err("Raw did not prove actual successful v2 routing".into());
        }
        let whole = e.successful_v2_leaves[0].whole_where_rows;
        let mut sum = 0_u64;
        for leaf in &e.successful_v2_leaves {
            if leaf.whole_where_rows != whole || leaf.partition_rows > whole || whole > e.input_rows
            {
                return Err("v2 whole/leaf population mismatch".into());
            }
            sum = sum
                .checked_add(leaf.partition_rows)
                .ok_or("population overflow")?;
        }
        if sum != whole {
            return Err("v2 disjoint population conservation failed".into());
        }
    } else if !matches!(
        e.physical_mode.as_str(),
        "parallel_dense_count" | "parallel_dense_integer"
    ) {
        return Err("Legacy actual mode outside diagnosed families".into());
    }
    Ok(())
}

// Frozen balanced schedule: five AB and five BA pairs per measured phase.
pub const ORDER: [[Arm; 2]; 10] = [
    [Arm::Legacy, Arm::Raw],
    [Arm::Raw, Arm::Legacy],
    [Arm::Legacy, Arm::Raw],
    [Arm::Raw, Arm::Legacy],
    [Arm::Legacy, Arm::Raw],
    [Arm::Raw, Arm::Legacy],
    [Arm::Legacy, Arm::Raw],
    [Arm::Raw, Arm::Legacy],
    [Arm::Legacy, Arm::Raw],
    [Arm::Raw, Arm::Legacy],
];

/// Receipt failures reject otherwise successful work, but never replace an
/// already established execution/validation error. A failed sink is not retried.
pub fn preserve_primary(
    primary: Result<(), String>,
    receipt: Result<(), String>,
) -> Result<(), String> {
    preserve_primary_with(primary, receipt, |secondary| {
        report_secondary_best_effort(&mut std::io::stderr().lock(), secondary);
    })
}

fn report_secondary_best_effort(writer: &mut impl std::io::Write, secondary: &str) {
    let _ = writeln!(writer, "paired secondary receipt failure: {secondary}");
}

fn preserve_primary_with(
    primary: Result<(), String>,
    receipt: Result<(), String>,
    report_secondary: impl FnOnce(&str),
) -> Result<(), String> {
    match (primary, receipt) {
        (Err(primary), Err(secondary)) => {
            let bounded: String = secondary.chars().take(512).collect();
            report_secondary(&bounded);
            Err(primary)
        }
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), receipt) => receipt,
    }
}

pub fn run_fixture<B: Backend>(
    backend: &mut B,
    fixture: &'static str,
    sql: &str,
) -> Result<(), String> {
    backend.assert_default_gucs_and_no_overrides()?;
    let expected = backend.stock_full_result(sql)?;
    let recipes = backend.capture_independent_recipes(sql)?;
    if recipes[0].arm != Arm::Legacy || recipes[1].arm != Arm::Raw {
        return Err("exact independent candidate pair required".into());
    }
    backend.prepare_jit_outside_measurements(&recipes)?;
    for phase in [Phase::Warmup, Phase::Warm, Phase::FreshArtifact] {
        let count = if phase == Phase::Warmup { 5 } else { 10 };
        for (pair, order) in ORDER.iter().enumerate().take(count) {
            for (position, arm) in order.iter().enumerate() {
                let recipe = &recipes[usize::from(*arm == Arm::Raw)];
                backend.begin_sample(fixture, phase, pair, position, *arm)?;
                if phase == Phase::FreshArtifact
                    && let Err(error) = backend.invalidate_owned_derived_only(recipe)
                {
                    let receipt =
                        backend.retain_error(fixture, phase, pair, position, *arm, &error);
                    return preserve_primary(Err(error), receipt);
                }
                let run = match backend.execute_owned_to_eof(recipe) {
                    Ok(run) => run,
                    Err(error) => {
                        let receipt =
                            backend.retain_error(fixture, phase, pair, position, *arm, &error);
                        return preserve_primary(Err(error), receipt);
                    }
                };
                // Persist unvalidated full output and evidence first. Failure
                // is not permission to drop a sample or retry for a good one.
                backend.retain(&Sample {
                    fixture,
                    phase,
                    pair,
                    position,
                    arm: *arm,
                    evidence: run.evidence.clone(),
                    rows: run.rows.clone(),
                })?;
                let verdict = (|| {
                    validate(recipe, &run, &expected)?;
                    if phase == Phase::Warm
                        && *arm == Arm::Legacy
                        && !run.evidence.artifact_cache_hit
                    {
                        return Err("warm Legacy artifact miss; retain failure, no retry".into());
                    }
                    if phase == Phase::FreshArtifact
                        && (run.evidence.artifact_cache_hit || run.evidence.resident_load_included)
                    {
                        return Err("fresh-artifact phase mislabeled".into());
                    }
                    backend.verify_closed_owner_and_raw_residency()
                })();
                let receipt = backend.retain_verdict(
                    fixture,
                    phase,
                    pair,
                    position,
                    verdict.as_ref().err().map(String::as_str),
                );
                preserve_primary(verdict, receipt)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod receipt_tests {
    use super::{preserve_primary_with, report_secondary_best_effort};

    #[test]
    fn failed_secondary_writer_does_not_replace_primary() {
        struct FailedWriter;
        impl std::io::Write for FailedWriter {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("stderr unavailable"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("stderr unavailable"))
            }
        }
        let result =
            preserve_primary_with(Err("original".into()), Err("receipt".into()), |error| {
                report_secondary_best_effort(&mut FailedWriter, error);
            });
        assert_eq!(result, Err("original".into()));
    }

    #[test]
    fn receipt_failure_preserves_each_primary_and_rejects_success() {
        for stage in ["invalidation", "execution", "validation", "jit"] {
            let primary = format!("{stage} primary");
            let mut secondary = Vec::new();
            let result =
                preserve_primary_with(Err(primary.clone()), Err("sink failed".into()), |value| {
                    secondary.push(value.to_owned());
                });
            assert_eq!(result, Err(primary));
            assert_eq!(secondary, ["sink failed"]);
        }
        assert_eq!(
            preserve_primary_with(Ok(()), Err("sink failed".into()), |_| panic!("no primary")),
            Err("sink failed".into())
        );
        assert_eq!(
            preserve_primary_with(Ok(()), Ok(()), |_| panic!("no error")),
            Ok(())
        );
    }

    #[test]
    fn secondary_receipt_is_bounded_and_never_retried() {
        let mut reports = 0;
        let result = preserve_primary_with(Err("primary".into()), Err("x".repeat(8192)), |error| {
            reports += 1;
            assert_eq!(error.len(), 512);
        });
        assert_eq!(result, Err("primary".into()));
        assert_eq!(reports, 1);
    }
}
