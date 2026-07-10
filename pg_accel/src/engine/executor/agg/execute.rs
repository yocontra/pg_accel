//! Childless aggregate executor state.

use pgrx::pg_sys;

use super::descriptor::{DescriptorAggPlan, DescriptorResidencyReport};
use super::output::DescriptorAggOutput;
use crate::engine::executor::olap::{OlapAggExecState, OlapAggSpec};
use crate::engine::residency::{ArtifactEnsureOutcome, ResidentRelationEvidence};
use crate::engine::spec::{AggOutputProjection, AggQuerySpec};

enum AggExecMode {
    Legacy(Box<OlapAggExecState>),
    Descriptor(Box<DescriptorAggExecState>),
}

struct DescriptorAggExecState {
    plan: DescriptorAggPlan,
    output: Option<DescriptorAggOutput>,
    explain_only: bool,
    residency: Option<DescriptorResidencyReport>,
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

/// Rust-side aggregate executor state shared by the legacy OLAP and neutral
/// descriptor contracts. Both modes are childless and read only resident data.
pub struct AggExecState {
    /// Whether a device aggregate dispatch completed (for EXPLAIN ANALYZE).
    pub gpu_dispatched: bool,

    // -- Counters for EXPLAIN ANALYZE --
    /// Total fact rows presented to the resident dispatch.
    pub rows_dispatched: u64,
    /// Number of device aggregate dispatches.
    pub batches_executed: u64,
    /// Cumulative microseconds in dispatch.
    pub dispatch_time_us: u64,

    mode: AggExecMode,
}

impl AggExecState {
    /// Create a legacy resident OLAP aggregate executor.
    #[must_use]
    pub fn new_olap(spec: OlapAggSpec) -> Self {
        Self {
            gpu_dispatched: false,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            mode: AggExecMode::Legacy(Box::new(OlapAggExecState::new(spec))),
        }
    }

    /// Validate a strict neutral contract and prepare its dependency-stamped
    /// derived artifact during BeginCustomScan.
    pub fn new_descriptor(
        spec: AggQuerySpec,
        projection: AggOutputProjection,
        explain_only: bool,
    ) -> Result<Self, String> {
        let plan = DescriptorAggPlan::new(spec, projection)?;
        let residency = (!explain_only)
            .then(|| plan.ensure_artifact())
            .transpose()?;
        Ok(Self {
            gpu_dispatched: false,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            mode: AggExecMode::Descriptor(Box::new(DescriptorAggExecState {
                plan,
                output: None,
                explain_only,
                residency,
            })),
        })
    }

    /// Legacy logical spec used by the existing EXPLAIN renderer.
    #[must_use]
    pub fn olap_spec(&self) -> Option<OlapAggSpec> {
        match &self.mode {
            AggExecMode::Legacy(olap) => Some(olap.spec()),
            AggExecMode::Descriptor(_) => None,
        }
    }

    /// Borrow the neutral logical contract for generic EXPLAIN rendering.
    #[must_use]
    pub fn descriptor_contract(&self) -> Option<(&AggQuerySpec, &AggOutputProjection)> {
        match &self.mode {
            AggExecMode::Legacy(_) => None,
            AggExecMode::Descriptor(descriptor) => {
                Some((descriptor.plan.spec(), descriptor.plan.projection()))
            }
        }
    }

    /// Exact dependency evidence captured while preparing a descriptor plan.
    #[must_use]
    pub fn descriptor_residency_evidence(&self) -> Option<&[ResidentRelationEvidence]> {
        match &self.mode {
            AggExecMode::Legacy(_) => None,
            AggExecMode::Descriptor(descriptor) => descriptor
                .residency
                .as_ref()
                .map(|residency| residency.relations.as_slice()),
        }
    }

    /// Derived-artifact cache outcome observed by the descriptor executor.
    #[must_use]
    pub fn descriptor_artifact_outcome(&self) -> Option<ArtifactEnsureOutcome> {
        match &self.mode {
            AggExecMode::Legacy(_) => None,
            AggExecMode::Descriptor(descriptor) => descriptor
                .residency
                .as_ref()
                .map(|residency| residency.artifact_outcome),
        }
    }

    /// Exact byte charge for this descriptor's derived artifact.
    #[must_use]
    pub fn descriptor_artifact_bytes(&self) -> Option<u64> {
        match &self.mode {
            AggExecMode::Legacy(_) => None,
            AggExecMode::Descriptor(descriptor) => descriptor
                .residency
                .as_ref()
                .map(|residency| residency.artifact_bytes),
        }
    }

    /// Relations loaded or reloaded while preparing this descriptor.
    #[must_use]
    pub fn descriptor_loaded_relations(&self) -> Option<&[pg_sys::Oid]> {
        match &self.mode {
            AggExecMode::Legacy(_) => None,
            AggExecMode::Descriptor(descriptor) => descriptor
                .residency
                .as_ref()
                .map(|residency| residency.loaded_relations.as_slice()),
        }
    }

    /// Raw relation staging time charged to this descriptor's first use.
    #[must_use]
    pub fn descriptor_raw_load_ms(&self) -> Option<f64> {
        match &self.mode {
            AggExecMode::Legacy(_) => None,
            AggExecMode::Descriptor(descriptor) => descriptor
                .residency
                .as_ref()
                .map(|residency| residency.raw_load_ms),
        }
    }

    /// Total begin-time residency and derived-artifact preparation time.
    #[must_use]
    pub fn descriptor_preparation_time_us(&self) -> Option<u64> {
        match &self.mode {
            AggExecMode::Legacy(_) => None,
            AggExecMode::Descriptor(descriptor) => descriptor
                .residency
                .as_ref()
                .map(|residency| residency.preparation_time_us),
        }
    }

    /// Reset cursor/device state for PostgreSQL ExecReScan.
    pub fn reset_for_rescan(&mut self) {
        self.gpu_dispatched = false;
        self.rows_dispatched = 0;
        self.batches_executed = 0;
        self.dispatch_time_us = 0;
        match &mut self.mode {
            AggExecMode::Legacy(olap) => {
                **olap = OlapAggExecState::new(olap.spec());
            }
            AggExecMode::Descriptor(descriptor) => {
                descriptor.output = None;
                if !descriptor.explain_only {
                    let residency = descriptor.plan.ensure_artifact().unwrap_or_else(|error| {
                        pgrx::error!(
                            "pg_accel: generic aggregate rescan preparation failed ({error}); refusing CPU fallback"
                        )
                    });
                    merge_residency_report(&mut descriptor.residency, residency);
                }
            }
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
        match &mut self.mode {
            AggExecMode::Legacy(olap) => {
                let was_dispatched = olap.gpu_dispatched();
                // SAFETY: caller upholds the slot/main-thread contract.
                let result = unsafe { olap.next(result_slot) };
                if !was_dispatched && olap.gpu_dispatched() {
                    self.rows_dispatched = olap.rows_dispatched();
                    self.batches_executed = olap.batches_executed();
                    self.dispatch_time_us = olap.dispatch_time_us();
                    self.gpu_dispatched = true;
                }
                result
            }
            AggExecMode::Descriptor(descriptor) => {
                if descriptor.explain_only {
                    return std::ptr::null_mut();
                }
                if descriptor.output.is_none() {
                    let dispatch = descriptor.plan.execute().unwrap_or_else(|error| {
                        pgrx::error!(
                            "pg_accel: generic aggregate dispatch failed ({error}); refusing CPU fallback"
                        )
                    });
                    self.dispatch_time_us = dispatch.dispatch_time_us;
                    self.rows_dispatched = u64::try_from(dispatch.fact_rows).unwrap_or_else(|_| {
                        pgrx::error!(
                            "pg_accel: generic aggregate fact row count exceeds EXPLAIN counter capacity"
                        )
                    });
                    self.batches_executed = 1;
                    self.gpu_dispatched = true;
                    if let Some(latest) = dispatch.residency {
                        merge_residency_report(&mut descriptor.residency, latest);
                    }
                    descriptor.output = Some(dispatch.output);
                }
                // SAFETY: output was synchronously detached from the device
                // call and caller supplies the initialized result slot.
                unsafe {
                    descriptor
                        .output
                        .as_mut()
                        .expect("descriptor output assigned above")
                        .next(result_slot)
                }
            }
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
