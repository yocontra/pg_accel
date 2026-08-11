//! Childless aggregate executor state.

use pgrx::pg_sys;

use super::descriptor::{
    DescriptorAggExecutionError, DescriptorAggPlan, DescriptorResidencyReport,
};
use super::output::DescriptorAggOutput;
use crate::engine::residency::{ArtifactEnsureOutcome, ResidentRelationEvidence};
use crate::engine::spec::{AggOutputProjection, AggQuerySpec};
use crate::engine::stats;
use crate::gpu::GroupedAggKernelMode;

struct DescriptorAggExecState {
    plan: DescriptorAggPlan,
    output: Option<DescriptorAggOutput>,
    explain_only: bool,
    preparation: DescriptorPreparationState,
    residency: Option<DescriptorResidencyReport>,
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
    pub(crate) fn new_descriptor_unprepared(
        spec: AggQuerySpec,
        projection: AggOutputProjection,
        explain_only: bool,
    ) -> Result<Self, String> {
        let plan = stats::profile_descriptor_stage(stats::DescriptorExecStage::PlanBuild, || {
            DescriptorAggPlan::new(spec, projection)
        })?;
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
