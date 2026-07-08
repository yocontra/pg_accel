//! Resident OLAP aggregate executor state.
//!
//! Only the resident OLAP aggregate survives the Phase 3 demolition: the
//! planner's sole Agg path creator (`resident_groupagg_path.rs`) always
//! serializes an [`OlapAggSpec`], and `begin_custom_scan` rejects any Agg
//! plan without one. The host-staged aggregate machinery that used to live
//! here (child-plan ExecProcNode draining, grouped hash aggregation,
//! vectorized self-scan, fused filter-count/filter-reduce pipelines, and
//! worker-side partial emission) was deleted with its planner injectors.
//!
//! [`AggExecState`] is a thin adapter between the Custom Scan FFI layer and
//! [`OlapAggExecState`] (`executor/olap.rs`), which owns the device-resident
//! dispatch. The adapter mirrors the OLAP executor's counters into the
//! fields EXPLAIN ANALYZE reads.

use pgrx::pg_sys;

use crate::engine::executor::olap::{OlapAggExecState, OlapAggSpec};

/// Rust-side aggregate executor state for the resident OLAP submode.
pub struct AggExecState {
    /// Whether the OLAP dispatch ran on the GPU (for EXPLAIN ANALYZE).
    pub gpu_dispatched: bool,

    // -- Counters for EXPLAIN ANALYZE --
    /// Total rows consumed by the OLAP dispatch.
    pub rows_dispatched: u64,
    /// Number of batches processed.
    pub batches_executed: u64,
    /// Cumulative microseconds in dispatch.
    pub dispatch_time_us: u64,

    /// Resident OLAP aggregate submode. This GpuAgg owns no child plan and
    /// dispatches directly from a resident cache/source.
    olap: OlapAggExecState,
}

impl AggExecState {
    /// Create a resident OLAP aggregate executor from its plan spec.
    #[must_use]
    pub fn new_olap(spec: OlapAggSpec) -> Self {
        Self {
            gpu_dispatched: false,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            olap: OlapAggExecState::new(spec),
        }
    }

    /// The plan spec this executor was built from (used by rescan and
    /// EXPLAIN's logical-spec rendering).
    #[must_use]
    pub fn olap_spec(&self) -> Option<OlapAggSpec> {
        Some(self.olap.spec())
    }

    /// Produce the next OLAP result tuple (or null when exhausted), mirroring
    /// the OLAP executor's counters into the EXPLAIN-visible fields after the
    /// dispatch completes.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `result_slot` must be a
    /// valid `TupleTableSlot`.
    pub unsafe fn next_olap(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let was_dispatched = self.olap.gpu_dispatched();
        // SAFETY: caller upholds the main-thread + valid-slot contract.
        let result = unsafe { self.olap.next(result_slot) };
        if !was_dispatched && self.olap.gpu_dispatched() {
            self.rows_dispatched = self.olap.rows_dispatched();
            self.batches_executed = self.olap.batches_executed();
            self.dispatch_time_us = self.olap.dispatch_time_us();
            self.gpu_dispatched = true;
        }
        result
    }
}
