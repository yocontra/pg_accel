use super::paired_driver::{
    Arm, Backend, Evidence, Key, Phase, Recipe, Rows, Run, Sample, SuccessfulV2Leaf,
};
use super::paired_sink::SharedSink;
use crate::engine::{ffi::planner_hooks, residency, stats};
use pgrx::{Spi, pg_sys};

pub struct PgBackend {
    raw: Option<residency::ResidentRelationEvidence>,
    legacy_identity: Option<residency::DerivedArtifactIdentity>,
    sequence: u64,
    sink: SharedSink,
    fixture: &'static str,
    context: serde_json::Value,
    oracle: Rows,
}
impl PgBackend {
    pub(crate) fn new(sink: SharedSink, fixture: &'static str) -> Self {
        Self {
            raw: None,
            legacy_identity: None,
            sequence: 0,
            sink,
            fixture,
            context: serde_json::Value::Null,
            oracle: Vec::new(),
        }
    }
    fn key(oid: u32, value: Option<i64>) -> Result<Key, String> {
        match (oid, value) {
            (_, None) => Ok(Key::Null),
            (21, Some(v)) => Ok(Key::I16(v.try_into().map_err(|_| "INT2 range")?)),
            (23, Some(v)) => Ok(Key::I32(v.try_into().map_err(|_| "INT4 range")?)),
            (20, Some(v)) => Ok(Key::I64(v)),
            _ => Err("unsupported result key".into()),
        }
    }
}
impl Backend for PgBackend {
    fn begin_sample(
        &mut self,
        fixture: &'static str,
        phase: Phase,
        pair: usize,
        position: usize,
        arm: Arm,
    ) -> Result<(), String> {
        if fixture != self.fixture {
            return Err("fixture binding mismatch".into());
        }
        self.context = serde_json::json!({"fixture":fixture,"phase":phase,"pair":pair,"position":position,"arm":arm});
        self.sink.borrow_mut().record("begin", &self.context)
    }
    fn assert_default_gucs_and_no_overrides(&mut self) -> Result<(), String> {
        let valid = Spi::get_one::<bool>("SELECT bool_and(setting=boot_val) FROM pg_settings WHERE name IN ('pg_accel.cost_multiplier','pg_accel.min_batch_size')")
            .map_err(|e| e.to_string())? == Some(true);
        if !valid || !crate::engine::gucs::enabled() {
            return Err("nondefault admission settings".into());
        }
        crate::engine::executor::agg::paired_assert_no_overrides()
    }
    fn stock_full_result(&mut self, sql: &str) -> Result<Rows, String> {
        // Output types come from the unchanged explicit fixture, not from casts
        // applied to the tested query. Stock cast is only owned scalar decoding.
        let oid = if sql.contains("raw_where") { 21 } else { 20 };
        Spi::run("SET pg_accel.enabled=off").map_err(|e| e.to_string())?;
        let before = crate::gpu::gpu_exec_count();
        let result = Spi::connect(|client| -> Result<Rows, String> {
            let selected = client
                .select(
                    &format!("SELECT k::bigint,n::bigint FROM ({sql}) paired_stock"),
                    None,
                    &[],
                )
                .map_err(|e| e.to_string())?;
            let mut rows = Vec::new();
            for row in selected {
                let key = Self::key(oid, row.get::<i64>(1).map_err(|e| e.to_string())?)?;
                let count = row
                    .get::<i64>(2)
                    .map_err(|e| e.to_string())?
                    .ok_or("NULL COUNT")?;
                if rows.len() == super::paired_sink::ROW_LIMIT {
                    super::paired_sink::retain_row_bound(&rows, &vec![(key, count)]);
                    return Err("stock collector exceeded fixed row bound".into());
                }
                rows.push((key, count));
            }
            Ok(rows)
        });
        Spi::run("SET pg_accel.enabled=on").map_err(|e| e.to_string())?;
        if crate::gpu::gpu_exec_count() != before {
            return Err("stock oracle dispatched GPU".into());
        }
        let result = result?;
        if result.len() > super::paired_sink::ROW_LIMIT {
            return Err("stock result exceeds row bound".into());
        }
        self.sink.borrow_mut().record(
            "oracle",
            &serde_json::json!({"fixture":self.fixture,"rows":result}),
        )?;
        self.oracle.clone_from(&result);
        Ok(result)
    }
    fn capture_independent_recipes(&mut self, sql: &str) -> Result<[Recipe; 2], String> {
        let ((), candidates) = planner_hooks::capture_paired_recipes(|| {
            Spi::run(&format!("EXPLAIN (VERBOSE) {sql}"))
                .expect("capture unchanged candidate pair");
        })?;
        let [legacy, raw] = candidates;
        self.sink.borrow_mut().record("recipes",&serde_json::json!({
            "fixture":self.fixture,"sql":sql,
            "legacy_spec":legacy.spec.encode_i32().map_err(|e|e.to_string())?,
            "legacy_projection":legacy.projection.encode_i32(&legacy.spec).map_err(|e|e.to_string())?,
            "raw_spec":raw.spec.encode_i32().map_err(|e|e.to_string())?,
            "raw_projection":raw.projection.encode_i32(&raw.spec).map_err(|e|e.to_string())?,
            "legacy_cost":legacy.cost_components,"raw_cost":raw.cost_components
        }))?;
        if legacy.spec.fact_rel != raw.spec.fact_rel {
            return Err("candidate owners differ".into());
        }
        let evidence = residency::paired_raw_evidence(raw.spec.fact_rel.into())?;
        self.raw = Some(evidence);
        let default_plan = Spi::connect(|client| -> Result<Vec<String>, String> {
            client
                .select(
                    &format!("EXPLAIN (ANALYZE,VERBOSE,COSTS OFF) {sql}"),
                    None,
                    &[],
                )
                .map_err(|error| error.to_string())?
                .map(|row| {
                    row.get::<String>(1)
                        .map_err(|error| error.to_string())?
                        .ok_or("NULL plan line".into())
                })
                .collect()
        })?;
        self.sink.borrow_mut().record(
            "default_plan",
            &serde_json::json!({"fixture":self.fixture,"lines":default_plan}),
        )?;
        let make = |captured: planner_hooks::PairedRecipe, arm| Recipe {
            arm,
            key_type_oid: captured.projection.slots[0].result_type_oid,
            cost_evidence: captured.cost_components.clone(),
            relation_generation: evidence.generation,
            captured,
        };
        Ok([make(legacy, Arm::Legacy), make(raw, Arm::Raw)])
    }
    fn prepare_jit_outside_measurements(&mut self, recipes: &[Recipe; 2]) -> Result<(), String> {
        for recipe in recipes {
            self.context =
                serde_json::json!({"fixture":self.fixture,"phase":"Jit","arm":recipe.arm});
            self.sink.borrow_mut().record("begin", &self.context)?;
            let run = match self.execute_owned_to_eof(recipe) {
                Ok(run) => run,
                Err(error) => {
                    let receipt = self.sink.borrow_mut().record(
                        "result_error",
                        &serde_json::json!({
                            "context": self.context, "error": error
                        }),
                    );
                    return super::paired_driver::preserve_primary(Err(error), receipt);
                }
            };
            let verdict = super::paired_driver::validate(recipe, &run, &self.oracle)
                .and_then(|()| self.verify_closed_owner_and_raw_residency());
            let receipt = self.sink.borrow_mut().record(
                "verdict",
                &serde_json::json!({"context":self.context,"error":verdict.as_ref().err()}),
            );
            super::paired_driver::preserve_primary(verdict, receipt)?;
        }
        Ok(())
    }
    fn invalidate_owned_derived_only(&mut self, recipe: &Recipe) -> Result<(), String> {
        self.verify_closed_owner_and_raw_residency()?;
        let expected = self.raw.ok_or("raw owner not captured")?;
        if recipe.arm == Arm::Legacy {
            let identity = self
                .legacy_identity
                .as_ref()
                .ok_or("no successful Legacy artifact identity")?;
            let bytes = residency::paired_remove_exact_derived(expected, identity)?;
            self.sink.borrow_mut().record("invalidation",&serde_json::json!({"context":self.context,"bytes":bytes,"identity":identity.canonical_words()}))?;
        } else {
            // Raw owns query-local masks only; preserve the unrelated Legacy
            // artifact and prove unchanged immutable raw ownership instead.
            residency::paired_current_raw(expected)?;
        }
        Ok(())
    }
    fn execute_owned_to_eof(&mut self, recipe: &Recipe) -> Result<Run, String> {
        self.sequence = self.sequence.checked_add(1).ok_or("sequence overflow")?;
        let sequence = self.sequence;
        let error_context = format!(
            "fixture={} sequence={sequence} context={}",
            self.fixture, self.context
        );
        let before_raw = residency::paired_current_raw(self.raw.ok_or("missing raw owner")?)?;
        let before_bytes = residency::resident_live_bytes();
        let stock = stats::paired_stock_count();
        let gpu = crate::gpu::gpu_exec_count();
        let verified = stats::paired_verified_data_calls();
        let artifact = stats::read_artifact_lifecycle();
        let (output, populations) = super::paired_population::capture(|| {
            pg_sys::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| {
                // SAFETY: main-backend test entry; installed runner owns every
                // executor resource through its real PG ERROR boundary.
                unsafe {
                    super::paired_runner::paired_run_installed(
                        recipe.captured.spec.clone(),
                        recipe.captured.projection.clone(),
                        recipe.captured.selection,
                    )
                }
            }))
            .catch_others(|caught| {
                // Immediate append to captured PG log before rethrow. Never
                // turn ERROR into a timing sample or silently continue a pair.
                super::paired_sink::active_error_before_rethrow(&error_context);
                caught.rethrow()
            })
            .execute()
        });
        let after = stats::read_artifact_lifecycle();
        let after_bytes = residency::resident_live_bytes();
        self.sink.borrow_mut().record("raw",&serde_json::json!({
            "context":self.context,"sequence":sequence,"key_type":output.key_type,
            "completed":output.completed,"mode":output.mode.map(crate::gpu::GroupedAggKernelMode::label),
            "calls":output.calls,"planned":output.scheduled,"dispatch_us":output.dispatch_us,
            "policy":output.artifact_summary,"artifact_bytes":output.artifact_bytes,
            "rows":output.rows,"populations":populations,
            "preparation_ns":output.preparation_ns,"consumption_ns":output.consumption_ns,
            "descriptor_executor_ns":output.descriptor_executor_ns,"diagnostic_capture_ns":output.diagnostic_capture_ns,
            "bridge_delta":crate::gpu::gpu_exec_count()-gpu,"stock_delta":stats::paired_stock_count()-stock,
            "verified_data_calls":stats::paired_verified_data_calls()-verified,
            "construction_bytes":after.construction_bytes-artifact.construction_bytes,
            "built_artifact_preparation_us":after.construction_us-artifact.construction_us,
            "retained_bytes_before":before_bytes,"retained_bytes_after":after_bytes,
            "derived_bytes_before":before_raw.derived_bytes,
            "cost":recipe.cost_evidence
        }))?;
        let after_raw = residency::paired_current_raw(before_raw)?;
        let [owner] = output.residency.as_slice() else {
            return Err("expected one raw owner".into());
        };
        if output.key_type != recipe.key_type_oid
            || output.artifact_bytes.is_none()
            || output.artifact_summary.is_empty()
        {
            return Err("missing typed execution/artifact evidence".into());
        }
        if recipe.arm == Arm::Raw
            && (populations.actual_v2_calls == 0
                || populations.actual_v2_calls != populations.capacities.len() as u64
                || populations
                    .leaves
                    .iter()
                    .any(|leaf| leaf.input != owner.row_count))
        {
            return Err("actual v2 call/input observation mismatch".into());
        }
        let verified_data_calls = stats::paired_verified_data_calls() - verified;
        let remaining_calls = output
            .calls
            .checked_sub(verified_data_calls)
            .ok_or("physical calls exceed lifecycle")?;
        let (mask_calls, control_calls) = if recipe.arm == Arm::Raw {
            if !output.artifact_summary.contains("ephemeral_fused")
                || output.artifact_outcome == Some(residency::ArtifactEnsureOutcome::Hit)
                || after_bytes != before_bytes
                || after_raw.derived_bytes != before_raw.derived_bytes
                || populations.actual_v2_calls != verified_data_calls
            {
                return Err("Raw fused ownership/aggregate-call evidence mismatch".into());
            }
            let filter = &recipe.captured.spec.measures[0].filter;
            let masks = if *filter == crate::engine::spec::FilterSpec::None
                || *filter == recipe.captured.spec.fact_filter
            {
                1_u64
            } else {
                2_u64
            };
            let expected = masks
                * owner
                    .row_count
                    .div_ceil(crate::gpu::SQL_BOOL_MASK_CHUNK_ROWS as u64);
            if remaining_calls != expected {
                return Err("distinct-mask conversion call count mismatch".into());
            }
            (remaining_calls, 0)
        } else {
            (0, remaining_calls)
        };
        if recipe.arm == Arm::Legacy {
            self.legacy_identity = Some(output.identity.clone());
        }
        let rows = output
            .rows
            .into_iter()
            .map(|(k, n)| Ok((Self::key(output.key_type, k)?, n)))
            .collect::<Result<Rows, String>>()?;
        let evidence = Evidence {
            physical_mode: output
                .mode
                .ok_or("no actual physical mode")?
                .label()
                .to_owned(),
            completed: output.completed,
            bridge_delta: crate::gpu::gpu_exec_count() - gpu,
            scheduled_calls: output.scheduled,
            observed_calls: output.calls,
            verified_data_calls,
            mask_calls,
            control_calls,
            stock_fallback_delta: stats::paired_stock_count() - stock,
            input_rows: owner.row_count,
            generation: owner.generation,
            natural_capacity: populations.capacities.first().copied(),
            explicit_capacity_override: None,
            artifact_cache_hit: output.artifact_outcome
                == Some(residency::ArtifactEnsureOutcome::Hit),
            construction_bytes: after.construction_bytes - artifact.construction_bytes,
            built_artifact_preparation_us: after.construction_us - artifact.construction_us,
            resident_load_included: !output.loaded_relations.is_empty(),
            successful_v2_leaves: populations
                .leaves
                .into_iter()
                .map(|leaf| SuccessfulV2Leaf {
                    whole_where_rows: leaf.whole,
                    partition_rows: leaf.partition,
                })
                .collect(),
            preparation_ns: output.preparation_ns,
            dispatch_ns: Some(u128::from(output.dispatch_us) * 1000),
            consumption_ns: output.consumption_ns,
            descriptor_executor_ns: output.descriptor_executor_ns,
            diagnostic_capture_ns: output.diagnostic_capture_ns,
        };
        Ok(Run { rows, evidence })
    }
    fn retain(&mut self, s: &Sample) -> Result<(), String> {
        self.sink.borrow_mut().record(
            "computed",
            &serde_json::json!({
                "fixture":s.fixture,"phase":s.phase,"pair":s.pair,"position":s.position,
                "arm":s.arm,"evidence":s.evidence,"rows":s.rows.len()
            }),
        )
    }
    fn retain_verdict(
        &mut self,
        fixture: &'static str,
        phase: Phase,
        pair: usize,
        position: usize,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.sink.borrow_mut().record(
            "verdict",
            &serde_json::json!({
                "fixture":fixture,"phase":phase,"pair":pair,"position":position,"error":error
            }),
        )
    }
    fn retain_error(
        &mut self,
        fixture: &'static str,
        phase: Phase,
        pair: usize,
        position: usize,
        arm: Arm,
        error: &str,
    ) -> Result<(), String> {
        self.sink.borrow_mut().record("result_error",&serde_json::json!({
            "fixture":fixture,"phase":phase,"pair":pair,"position":position,"arm":arm,"error":error
        }))
    }
    fn verify_closed_owner_and_raw_residency(&mut self) -> Result<(), String> {
        let counts = super::test_executor_cleanup_counts();
        if counts.installed
            != counts
                .normal_end
                .checked_add(counts.query_reset)
                .ok_or("cleanup overflow")?
        {
            return Err("executor still installed".into());
        }
        residency::paired_current_raw(self.raw.ok_or("no raw baseline")?)?;
        Ok(())
    }
}
