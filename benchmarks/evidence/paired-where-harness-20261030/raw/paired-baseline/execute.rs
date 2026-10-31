//! Childless aggregate executor state.

use pgrx::pg_sys;

use super::descriptor::{
    AdaptiveHashExecution, AdaptiveHashStep, DescriptorAggExecutionError, DescriptorAggPlan,
    DescriptorResidencyReport,
};
use super::output::DescriptorAggOutput;
use super::spool::FinalTupleSpool;
use crate::engine::residency::{ArtifactEnsureOutcome, ResidentRelationEvidence};
use crate::engine::spec::{AggOutputProjection, AggQuerySpec};
use crate::engine::stats;
use crate::gpu::GroupedAggKernelMode;

#[cfg(feature = "pg_test")]
thread_local! {
    static TEST_CANCEL_AFTER_ADAPTIVE_RETRY: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

#[cfg(feature = "pg_test")]
pub(super) fn set_test_cancel_after_adaptive_retry(calls: Option<u64>) -> Option<u64> {
    TEST_CANCEL_AFTER_ADAPTIVE_RETRY.with(|value| value.replace(calls))
}

#[cfg(feature = "pg_test")]
fn test_cancel_after_adaptive_retry(calls: u64) {
    if TEST_CANCEL_AFTER_ADAPTIVE_RETRY.with(|value| {
        if value.get().is_some_and(|limit| calls >= limit) {
            value.set(None);
            true
        } else {
            false
        }
    }) {
        // SAFETY: backend-thread test hook after a synchronous attempt returned
        // and its STORE/native leases were released. The next ordinary executor
        // interrupt boundary delivers cancellation, not this signal-only hook.
        unsafe {
            std::ptr::write_volatile(&raw mut pg_sys::QueryCancelPending, 1);
            std::ptr::write_volatile(&raw mut pg_sys::InterruptPending, 1);
        }
    }
}

struct DescriptorAggExecState {
    plan: DescriptorAggPlan,
    output: Option<DescriptorAggOutput>,
    explain_only: bool,
    preparation: DescriptorPreparationState,
    residency: Option<DescriptorResidencyReport>,
    adaptive: Option<AdaptiveHashExecution>,
    spool: Option<FinalTupleSpool>,
    completed: bool,
    query_context: pg_sys::MemoryContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DescriptorPreparationState {
    Unprepared,
    Prepared,
}

fn merge_residency_report(
    current: &mut Option<DescriptorResidencyReport>,
    latest: DescriptorResidencyReport,
) {
    if let Some(current) = current {
        current.merge(latest);
    } else {
        *current = Some(latest);
    }
}

fn record_residency_lifecycle(report: &DescriptorResidencyReport) {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let raw_load_us = if report.raw_load_ms.is_finite() && report.raw_load_ms > 0.0 {
        (report.raw_load_ms * 1_000.0).min(u64::MAX as f64) as u64
    } else {
        0
    };
    stats::record_artifact_lifecycle(
        report.artifact_outcome,
        report.artifact_bytes,
        report.preparation_time_us,
        raw_load_us,
    );
    stats::record_artifact_policy(report.artifact_policy.policy_label());
}

fn raise_descriptor_execution_error(error: DescriptorAggExecutionError) -> ! {
    match error {
        DescriptorAggExecutionError::PreparationInterrupted => {
            pgrx::error!("pg_accel: adaptive preparation interrupt escaped its service boundary")
        }
        DescriptorAggExecutionError::DateOutOfRange => {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_DATETIME_FIELD_OVERFLOW,
                "pg_accel: aggregate date expression out of range; refusing CPU fallback"
            );
        }
        DescriptorAggExecutionError::DivisionByZero => {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_DIVISION_BY_ZERO,
                "pg_accel: aggregate expression division by zero; refusing CPU fallback"
            );
        }
        DescriptorAggExecutionError::NumericOverflow => {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
                "pg_accel: generic aggregate numeric value is out of range; refusing CPU fallback"
            );
        }
        DescriptorAggExecutionError::ExternalRoutineException(message) => {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_EXTERNAL_ROUTINE_EXCEPTION,
                format!("pg_accel: H3 parent transform failed ({message}); refusing CPU fallback")
            );
        }
        DescriptorAggExecutionError::Failure(message) => {
            pgrx::error!(
                "pg_accel: generic aggregate dispatch failed ({message}); refusing CPU fallback"
            )
        }
    }
}

/// Rust-side childless aggregate executor for the neutral descriptor contract.
pub struct AggExecState {
    /// Whether a device aggregate dispatch completed (for EXPLAIN ANALYZE).
    pub gpu_dispatched: bool,

    // -- Counters for EXPLAIN ANALYZE --
    /// Total fact rows presented to the resident dispatch.
    pub rows_dispatched: u64,
    /// Number of device aggregate dispatches.
    pub batches_executed: u64,
    /// Exact lifecycle calls predicted by the selected execution branch.
    pub planned_batches: u64,
    /// Cumulative microseconds in dispatch.
    pub dispatch_time_us: u64,

    physical_kernel_mode: Option<GroupedAggKernelMode>,

    descriptor: Box<DescriptorAggExecState>,
}

impl AggExecState {
    /// Validate a strict neutral contract without beginning fallible artifact
    /// preparation. The caller must install this state into its query-context
    /// cleanup owner before calling [`Self::prepare_descriptor`], because that
    /// method may raise a PostgreSQL ERROR or cancellation.
    pub(crate) fn new_selected_descriptor_unprepared(
        spec: AggQuerySpec,
        projection: AggOutputProjection,
        explain_only: bool,
        selection: super::AggExecutionSelection,
    ) -> Result<Self, String> {
        let plan = stats::profile_descriptor_stage(stats::DescriptorExecStage::PlanBuild, || {
            DescriptorAggPlan::new_selected(spec, projection, selection)
        })?;
        let adaptive = plan
            .uses_adaptive_hash_count()
            .then(AdaptiveHashExecution::default);
        Ok(Self {
            gpu_dispatched: false,
            rows_dispatched: 0,
            batches_executed: 0,
            planned_batches: 0,
            dispatch_time_us: 0,
            physical_kernel_mode: None,
            descriptor: Box::new(DescriptorAggExecState {
                plan,
                output: None,
                explain_only,
                preparation: DescriptorPreparationState::Unprepared,
                residency: None,
                adaptive,
                spool: None,
                completed: false,
                query_context: std::ptr::null_mut(),
            }),
        })
    }

    /// Prepare the dependency-stamped artifact after the executor has an
    /// abort-safe owner. This may raise a PostgreSQL ERROR or cancellation;
    /// calling it before ownership is installed would bypass Rust cleanup.
    pub(crate) fn prepare_descriptor(&mut self) {
        if self.descriptor.explain_only
            || self.descriptor.preparation == DescriptorPreparationState::Prepared
        {
            return;
        }
        let residency =
            stats::profile_descriptor_stage(stats::DescriptorExecStage::ArtifactEnsure, || {
                self.descriptor.plan.ensure_artifact()
            })
            .unwrap_or_else(|error| raise_descriptor_execution_error(error));
        record_residency_lifecycle(&residency);
        self.descriptor.residency = Some(residency);
        self.descriptor.preparation = DescriptorPreparationState::Prepared;
    }

    /// Borrow the neutral logical contract for generic EXPLAIN rendering.
    #[must_use]
    pub fn descriptor_contract(&self) -> Option<(&AggQuerySpec, &AggOutputProjection)> {
        Some((
            self.descriptor.plan.spec(),
            self.descriptor.plan.projection(),
        ))
    }

    /// Physical branch admitted by the logical descriptor contract.
    #[must_use]
    pub fn descriptor_planned_kernel_mode(&self) -> GroupedAggKernelMode {
        self.descriptor.plan.planned_kernel_mode()
    }

    /// Build-generated specialization admitted for the complete descriptor.
    #[must_use]
    pub fn descriptor_specialization_label(&self) -> &'static str {
        self.descriptor.plan.specialization().label()
    }

    /// Whether this backend had already resolved the exact semantic identity.
    #[must_use]
    pub fn descriptor_specialization_cache_outcome_label(&self) -> &'static str {
        self.descriptor.plan.specialization_cache_outcome().label()
    }

    /// Cost inputs and decision used by begin-time derived-artifact admission.
    #[must_use]
    pub fn descriptor_artifact_policy_summary(&self) -> String {
        self.descriptor.plan.artifact_policy_evidence().map_or_else(
            || "not initialized (EXPLAIN ONLY)".to_owned(),
            |policy| policy.summary(),
        )
    }

    /// Native branch verified immediately before successful dispatch.
    #[must_use]
    pub const fn descriptor_dispatched_kernel_mode(&self) -> Option<GroupedAggKernelMode> {
        self.physical_kernel_mode
    }

    /// Exact lifecycle call count selected before the first native submission.
    #[must_use]
    pub const fn descriptor_planned_batches(&self) -> u64 {
        self.planned_batches
    }

    /// Logical input population is separate from repeated physical scans.
    /// Return only dynamic executions so canonical one-shot EXPLAIN is stable.
    pub fn descriptor_adaptive_evidence(&self) -> Option<(u64, u64, u64, u64, u64, bool)> {
        let adaptive = self.descriptor.adaptive.as_ref()?;
        (self.descriptor.completed && adaptive.overflow_attempts != 0).then(|| {
            (
                adaptive.fact_rows as u64,
                adaptive.scanned_rows,
                adaptive.aggregate_calls,
                adaptive.overflow_attempts,
                adaptive.completed_leaves(),
                self.descriptor
                    .spool
                    .as_ref()
                    .is_some_and(FinalTupleSpool::spilled),
            )
        })
    }

    /// Exact dependency evidence captured while preparing a descriptor plan.
    #[must_use]
    pub fn descriptor_residency_evidence(&self) -> Option<&[ResidentRelationEvidence]> {
        self.descriptor
            .residency
            .as_ref()
            .map(|residency| residency.relations.as_slice())
    }

    /// Derived-artifact cache outcome observed by the descriptor executor.
    #[must_use]
    pub fn descriptor_artifact_outcome(&self) -> Option<ArtifactEnsureOutcome> {
        self.descriptor
            .residency
            .as_ref()
            .map(|residency| residency.artifact_outcome)
    }

    /// Exact byte charge for this descriptor's derived artifact.
    #[must_use]
    pub fn descriptor_artifact_bytes(&self) -> Option<u64> {
        self.descriptor
            .residency
            .as_ref()
            .map(|residency| residency.artifact_bytes)
    }

    /// Relations loaded or reloaded while preparing this descriptor.
    #[must_use]
    pub fn descriptor_loaded_relations(&self) -> Option<&[pg_sys::Oid]> {
        self.descriptor
            .residency
            .as_ref()
            .map(|residency| residency.loaded_relations.as_slice())
    }

    /// Raw relation staging time charged to this descriptor's first use.
    #[must_use]
    pub fn descriptor_raw_load_ms(&self) -> Option<f64> {
        self.descriptor
            .residency
            .as_ref()
            .map(|residency| residency.raw_load_ms)
    }

    /// Total begin-time residency and derived-artifact preparation time.
    #[must_use]
    pub fn descriptor_preparation_time_us(&self) -> Option<u64> {
        self.descriptor
            .residency
            .as_ref()
            .map(|residency| residency.preparation_time_us)
    }

    /// Reset cursor/device state for PostgreSQL ExecReScan.
    pub fn reset_for_rescan(&mut self) {
        // SAFETY: ReScan is wrapped by the installed-owner PG error boundary;
        // query context remains live until explicit close completes.
        unsafe { self.close_transport() };
        self.descriptor.spool = None;
        self.descriptor.adaptive = self
            .descriptor
            .plan
            .uses_adaptive_hash_count()
            .then(AdaptiveHashExecution::default);
        self.descriptor.completed = false;
        self.gpu_dispatched = false;
        self.rows_dispatched = 0;
        self.batches_executed = 0;
        self.planned_batches = 0;
        self.dispatch_time_us = 0;
        self.physical_kernel_mode = None;
        self.descriptor.output = None;
        if !self.descriptor.explain_only {
            self.descriptor.preparation = DescriptorPreparationState::Unprepared;
            let residency =
                stats::profile_descriptor_stage(stats::DescriptorExecStage::ArtifactEnsure, || {
                    self.descriptor.plan.ensure_artifact()
                })
                .unwrap_or_else(|error| raise_descriptor_execution_error(error));
            record_residency_lifecycle(&residency);
            merge_residency_report(&mut self.descriptor.residency, residency);
            self.descriptor.preparation = DescriptorPreparationState::Prepared;
        }
    }

    /// Produce the next resident aggregate result tuple, or NULL at EOF.
    ///
    /// # Safety
    /// Must be called on the main backend thread with a valid result slot.
    pub unsafe fn next(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        if self.descriptor.explain_only {
            return std::ptr::null_mut();
        }
        if self.descriptor.preparation != DescriptorPreparationState::Prepared {
            pgrx::error!(
                "pg_accel: generic aggregate execution started without Begin-time artifact preparation"
            );
        }
        if self.descriptor.adaptive.is_some() {
            // SAFETY: this installed state owns all batches/masks/spool across
            // PG calls; caller provides the live result slot and error boundary.
            return unsafe { self.next_adaptive(result_slot) };
        }
        if self.descriptor.output.is_none() {
            let dispatch = self
                .descriptor
                .plan
                .execute_prepared()
                .unwrap_or_else(|error| raise_descriptor_execution_error(error));
            self.dispatch_time_us = dispatch.dispatch_time_us;
            self.rows_dispatched = u64::try_from(dispatch.fact_rows).unwrap_or_else(|_| {
                pgrx::error!(
                    "pg_accel: generic aggregate fact row count exceeds EXPLAIN counter capacity"
                )
            });
            self.batches_executed = dispatch.batches_executed;
            self.planned_batches = dispatch.planned_batches;
            if self.planned_batches != self.batches_executed {
                pgrx::error!(
                    "pg_accel: grouped aggregate completed {} lifecycle calls after planning {}; refusing inconsistent execution evidence",
                    self.batches_executed,
                    self.planned_batches
                );
            }
            self.physical_kernel_mode = Some(dispatch.kernel_mode);
            stats::record_grouped_dispatch(
                dispatch.kernel_mode,
                dispatch.batches_executed,
                dispatch.physical_mode_calls,
                u64::try_from(dispatch.fact_rows).unwrap_or(u64::MAX),
                dispatch.dispatch_time_us,
            );
            self.gpu_dispatched = true;
            if let Some(latest) = dispatch.residency {
                merge_residency_report(&mut self.descriptor.residency, latest);
            }
            self.descriptor.output = Some(dispatch.output);
        }
        // SAFETY: output was synchronously detached from the device call and
        // caller supplies the initialized result slot.
        stats::profile_descriptor_stage(
            stats::DescriptorExecStage::TupleMaterialization,
            || unsafe {
                self.descriptor
                    .output
                    .as_mut()
                    .expect("descriptor output assigned above")
                    .next(result_slot)
            },
        )
    }

    pub(crate) fn set_query_context(&mut self, context: pg_sys::MemoryContext) {
        self.descriptor.query_context = context;
    }

    /// No PG call: safe both before and after child-context deletion.
    pub(crate) fn abandon_transport(&mut self) {
        if let Some(spool) = &mut self.descriptor.spool {
            spool.abandon();
        }
        self.descriptor.query_context = std::ptr::null_mut();
    }

    /// Explicit fallible normal close while installed ownership remains live.
    pub(crate) unsafe fn close_transport(&mut self) {
        if let Some(spool) = &mut self.descriptor.spool {
            // SAFETY: normal End/ReScan/EOF runs with live query context.
            unsafe { spool.close() };
        }
    }

    unsafe fn next_adaptive(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        if !self.descriptor.completed {
            loop {
                pgrx::check_for_interrupts!();
                let step = self.descriptor.plan.next_adaptive_hash(
                    self.descriptor
                        .adaptive
                        .as_mut()
                        .expect("adaptive owner installed"),
                );
                let step = match step {
                    Ok(step) => step,
                    Err(DescriptorAggExecutionError::PreparationInterrupted) => {
                        // The descriptor callback and all STORE/ephemeral guards
                        // have returned; its partial mask and charge are gone.
                        // Deliver PostgreSQL's original error, or retry a benign
                        // interrupt with fresh protected binding and real counts.
                        pgrx::check_for_interrupts!();
                        continue;
                    }
                    Err(error) => raise_descriptor_execution_error(error),
                };
                match step {
                    AdaptiveHashStep::Retry => {
                        #[cfg(feature = "pg_test")]
                        test_cancel_after_adaptive_retry(
                            self.descriptor
                                .adaptive
                                .as_ref()
                                .expect("adaptive owner")
                                .aggregate_calls,
                        );
                        continue;
                    }
                    AdaptiveHashStep::Leaf {
                        output,
                        needs_spool,
                        done,
                    } => {
                        // Install the output before any fallible PostgreSQL call.
                        self.descriptor.output = Some(output);
                        if needs_spool {
                            if self.descriptor.spool.is_none() {
                                self.descriptor.spool = Some(FinalTupleSpool::default());
                                // SAFETY: owner is installed before context/store
                                // creation; no native frame or STORE lease survives.
                                unsafe {
                                    self.descriptor
                                        .spool
                                        .as_mut()
                                        .expect("spool installed")
                                        .begin(self.descriptor.query_context, result_slot);
                                };
                            }
                            loop {
                                // SAFETY: completed leaf output and supplied
                                // slot remain installed/live through materialization.
                                let tuple = unsafe {
                                    self.descriptor
                                        .output
                                        .as_mut()
                                        .expect("leaf installed")
                                        .next(result_slot)
                                };
                                if tuple.is_null() {
                                    break;
                                }
                                // SAFETY: copy final tuple only; no GPU/store borrow.
                                unsafe {
                                    self.descriptor
                                        .spool
                                        .as_mut()
                                        .expect("spool installed")
                                        .put(tuple);
                                    pg_sys::ExecClearTuple(result_slot);
                                }
                                pgrx::check_for_interrupts!();
                            }
                            self.descriptor.output = None;
                        }
                        pgrx::check_for_interrupts!();
                        if !done {
                            continue;
                        }
                        if let Some(spool) = &mut self.descriptor.spool {
                            // SAFETY: every disjoint leaf completed and population
                            // totals were validated; rewind precedes Ready.
                            unsafe { spool.finish() };
                        }
                        let adaptive = self.descriptor.adaptive.as_mut().expect("adaptive owner");
                        self.descriptor
                            .plan
                            .finish_adaptive_hash(adaptive)
                            .unwrap_or_else(|error| raise_descriptor_execution_error(error));
                        self.rows_dispatched = adaptive.scanned_rows;
                        self.batches_executed = adaptive.calls;
                        self.planned_batches = adaptive.scheduled_calls;
                        self.dispatch_time_us = adaptive.dispatch_time_us;
                        self.physical_kernel_mode = Some(GroupedAggKernelMode::ParallelHash);
                        stats::record_grouped_dispatch(
                            GroupedAggKernelMode::ParallelHash,
                            adaptive.calls,
                            adaptive.aggregate_calls,
                            adaptive.fact_rows as u64,
                            adaptive.dispatch_time_us,
                        );
                        if let Some(report) = adaptive.residency.take() {
                            merge_residency_report(&mut self.descriptor.residency, report);
                        }
                        self.gpu_dispatched = true;
                        self.descriptor.completed = true;
                        break;
                    }
                }
            }
        }
        if let Some(spool) = &mut self.descriptor.spool {
            // SAFETY: Ready is published only after complete output transport;
            // copy=true reads independent slot tuples in the caller's context.
            if unsafe { spool.next(result_slot) } {
                return result_slot;
            }
            // SAFETY: EOF normal close remains inside installed error boundary.
            unsafe { spool.close() };
            return std::ptr::null_mut();
        }
        // SAFETY: one-shot success retains the original direct output transport.
        unsafe {
            self.descriptor
                .output
                .as_mut()
                .expect("complete one-shot output")
                .next(result_slot)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(
        outcome: ArtifactEnsureOutcome,
        relid: u32,
        raw_load_ms: f64,
        preparation_time_us: u64,
    ) -> DescriptorResidencyReport {
        DescriptorResidencyReport {
            artifact_outcome: outcome,
            artifact_policy: super::super::descriptor::test_artifact_policy_evidence(),
            relations: Vec::new(),
            loaded_relations: vec![pg_sys::Oid::from(relid)],
            artifact_bytes: u64::from(relid),
            raw_load_ms,
            preparation_time_us,
        }
    }

    #[test]
    fn rescan_residency_accounting_preserves_first_use_costs() {
        let mut accumulated = Some(report(ArtifactEnsureOutcome::Built, 10, 4.25, 6_000));
        merge_residency_report(
            &mut accumulated,
            report(ArtifactEnsureOutcome::Hit, 20, 1.75, 2_000),
        );

        let accumulated = accumulated.expect("merged residency report");
        assert_eq!(accumulated.artifact_outcome, ArtifactEnsureOutcome::Built);
        assert_eq!(
            accumulated.loaded_relations,
            [pg_sys::Oid::from(10_u32), pg_sys::Oid::from(20_u32)]
        );
        assert_eq!(accumulated.raw_load_ms, 6.0);
        assert_eq!(accumulated.preparation_time_us, 8_000);
        assert_eq!(accumulated.artifact_bytes, 20);
    }
}
