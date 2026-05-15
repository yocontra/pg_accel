//! Partial-state emission path for the PreAgg executor under parallel Gather.
//!
//! When a pg_accel PreAgg Custom Scan runs inside a parallel Gather, each
//! worker must emit *transition-state* tuples instead of final aggregate
//! Datums — otherwise the leader's Finalize Aggregate node would
//! double-count across workers.
//!
//! This module reuses [`crate::engine::executor::agg::partial::PartialEmitter`]
//! (W2's contract) to convert per-column accumulators into the Datum shape
//! PG's combine function expects (typically int8 for COUNT, bytea for
//! AVG/STDDEV/VAR, passthrough for SUM/MIN/MAX).
//!
//! The existing sibling module `preagg::partial` holds the fact-side
//! accumulation primitives (dim hash tables, `AggAccum`, heap readers). This
//! module wires those accumulators into the
//! per-column `PartialEmitter` contract when `CustomPrivateData.partial ==
//! Some(spec)`.

use pgrx::pg_sys;

use crate::engine::executor::agg::AggOp;
use crate::engine::executor::agg::partial::emitter::{
    CountEmitter, Float8StatsEmitter, IntegerSumPromotion, NumericSumEmitter, ScalarPassthrough,
};
use crate::engine::executor::agg::partial::{
    ColumnAccumulator, PartialAggSpec, PartialColumn, PartialEmitter,
};

use super::partial::AggAccum;

// ---------------------------------------------------------------------------
// PreaggPartialState
// ---------------------------------------------------------------------------

/// Worker-side partial-state scaffolding attached to `PreAggExecState` when
/// the plan was injected via `add_partial_path`. When present, the executor
/// emits transition-state tuples instead of final aggregate values.
///
/// The vector lengths are all equal to `agg_descs.len()` in the parent
/// `PreAggExecState` — one entry per output aggregate column.
pub(super) struct PreaggPartialState {
    /// One accumulator per output agg column.
    pub per_column_accumulators: Vec<ColumnAccumulator>,
    /// One emitter per output agg column, picked by `build_emitters`.
    pub emitters: Vec<Box<dyn PartialEmitter>>,
}

impl PreaggPartialState {
    /// Construct a fresh partial-emit state matching the per-column spec.
    #[must_use]
    pub(super) fn new(spec: &PartialAggSpec) -> Self {
        let n = spec.per_column.len();
        let per_column_accumulators = vec![ColumnAccumulator::default(); n];
        let emitters = build_emitters(&spec.per_column);
        Self {
            per_column_accumulators,
            emitters,
        }
    }

    /// Copy plain (ungrouped) state from the parent executor's
    /// `AggAccum` vector. Used when the existing fact-scan loop already
    /// populated `plain_accums` — we mirror those scalars into the
    /// per-column accumulators used by the partial emitters.
    pub(super) fn mirror_plain(&mut self, plain_accums: &[AggAccum]) {
        for (dst, src) in self
            .per_column_accumulators
            .iter_mut()
            .zip(plain_accums.iter())
        {
            dst.sum = src.sum;
            dst.count = u64::try_from(src.count).unwrap_or(0);
            dst.min_val = src.min;
            dst.max_val = src.max;
            dst.has_value = src.count > 0;
        }
    }

    /// Emit one row's worth of partial-state Datums into `scan_slot`.
    /// For grouped aggregation, callers must have written group-key Datums
    /// into the leading slot attributes before invoking this routine; this
    /// method only fills the aggregate columns.
    ///
    /// # Safety
    ///
    /// `scan_slot` must be a valid `TupleTableSlot` with `tts_values` /
    /// `tts_isnull` arrays large enough to hold `group_keys + emitters`
    /// columns. Must be called on the main backend thread.
    pub(super) unsafe fn finalize_partial(
        &self,
        scan_slot: *mut pg_sys::TupleTableSlot,
        group_col_offset: usize,
    ) {
        // SAFETY: caller guarantees slot validity.
        let values = unsafe { (*scan_slot).tts_values };
        let isnull = unsafe { (*scan_slot).tts_isnull };

        for (i, (acc, emitter)) in self
            .per_column_accumulators
            .iter()
            .zip(self.emitters.iter())
            .enumerate()
        {
            // SAFETY: PartialEmitter::emit is main-thread-only (rule #1).
            let (datum, is_null) = unsafe { emitter.emit(acc) };
            let col = group_col_offset + i;
            // SAFETY: indexing into slot attr arrays sized by planner.
            unsafe {
                *values.add(col) = datum;
                *isnull.add(col) = is_null;
            }
        }
    }
}

/// Pick the concrete [`PartialEmitter`] for each output agg column, keyed
/// on the declared `AggOp` + `transtype_oid`.
fn build_emitters(columns: &[PartialColumn]) -> Vec<Box<dyn PartialEmitter>> {
    columns
        .iter()
        .map(|c| -> Box<dyn PartialEmitter> {
            match c.op {
                AggOp::Count => Box::new(CountEmitter),
                AggOp::Sum => match c.transtype_oid {
                    pg_sys::INT8OID => Box::new(IntegerSumPromotion),
                    pg_sys::NUMERICOID => Box::new(NumericSumEmitter),
                    t => Box::new(ScalarPassthrough { transtype: t }),
                },
                AggOp::Avg
                | AggOp::StddevSamp
                | AggOp::StddevPop
                | AggOp::VarSamp
                | AggOp::VarPop => Box::new(Float8StatsEmitter {
                    serialize_fn_oid: c.serialize_fn_oid.unwrap_or(pg_sys::InvalidOid),
                }),
                AggOp::Min | AggOp::Max => Box::new(ScalarPassthrough {
                    transtype: c.transtype_oid,
                }),
                // BIT_* / BOOL_* / Passthrough fall back to scalar
                // passthrough at the transtype. When W3 wires dedicated
                // combine functions, swap in a typed emitter.
                AggOp::BitAnd
                | AggOp::BitOr
                | AggOp::BitXor
                | AggOp::BoolAnd
                | AggOp::BoolOr
                | AggOp::Passthrough => Box::new(ScalarPassthrough {
                    transtype: c.transtype_oid,
                }),
            }
        })
        .collect()
}
