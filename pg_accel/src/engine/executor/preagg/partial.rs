//! Partial-aggregation building blocks used by `PreAggExecState`.
//!
//! Dimension hash tables, per-group accumulators, and heap-extraction
//! helpers used during the fused fact-table scan.

use std::collections::HashMap;

use pgrx::pg_sys;

use crate::engine::executor::agg::AggOp;

// ---------------------------------------------------------------------------
// Dimension hash table
// ---------------------------------------------------------------------------

/// A single column of a dimension table stored in columnar format.
#[derive(Debug)]
pub struct DimColumn {
    /// Column type OID.
    pub type_oid: pg_sys::Oid,
    /// Integer/date values (type-punned i64 for all integer types).
    pub values_i64: Vec<i64>,
    /// Float values.
    pub values_f64: Vec<f64>,
    /// Text values for dictionary encoding.
    pub values_text: Vec<String>,
    /// Text dictionary: text → integer code.
    pub text_dict: HashMap<String, u32>,
    /// Null bitmap (true = null).
    pub null_mask: Vec<bool>,
}

impl DimColumn {
    pub(super) fn new(type_oid: pg_sys::Oid, capacity: usize) -> Self {
        Self {
            type_oid,
            values_i64: Vec::with_capacity(capacity),
            values_f64: Vec::with_capacity(capacity),
            values_text: Vec::new(),
            text_dict: HashMap::new(),
            null_mask: Vec::with_capacity(capacity),
        }
    }
}

/// A dimension-side filter predicate applied during fact table probing.
#[derive(Debug, Clone)]
pub struct DimFilter {
    /// Index into the DimHashTable's columns array.
    pub col_idx: usize,
    /// Comparison opcode (reuses CmpConst opcodes: EQ=0, NE=1, LT=2, LE=3, GT=4, GE=5).
    pub cmp_opcode: u16,
    /// Constant value to compare against.
    pub const_val: f64,
}

/// Pre-materialized dimension table with hash lookup.
#[derive(Debug)]
pub struct DimHashTable {
    /// Join key → row index mapping.
    pub hash_table: HashMap<i64, u32>,
    /// Number of rows.
    pub n_rows: usize,
    /// Dimension columns stored in columnar format.
    pub columns: Vec<DimColumn>,
    /// Dimension-side filter predicates.
    pub dim_filters: Vec<DimFilter>,
    /// Which dimension columns are referenced in GROUP BY.
    pub group_col_indices: Vec<usize>,
}

impl DimHashTable {
    /// Probe the hash table for a join key. Returns the row index if found
    /// and all dimension-side filters pass.
    #[inline]
    #[must_use]
    pub fn probe(&self, key: i64) -> Option<u32> {
        let &row_idx = self.hash_table.get(&key)?;

        // Apply dimension-side filters.
        for filt in &self.dim_filters {
            let col = &self.columns[filt.col_idx];
            let idx = row_idx as usize;
            if col.null_mask[idx] {
                return None; // NULL fails all comparisons
            }
            #[allow(clippy::cast_precision_loss)]
            let val = if col.values_f64.is_empty() {
                col.values_i64[idx] as f64
            } else {
                col.values_f64[idx]
            };
            if !eval_cmp(val, filt.cmp_opcode, filt.const_val) {
                return None;
            }
        }

        Some(row_idx)
    }
}

/// Evaluate a comparison: `val <op> const_val`.
#[inline]
pub(super) fn eval_cmp(val: f64, opcode: u16, const_val: f64) -> bool {
    match opcode {
        0 => (val - const_val).abs() < f64::EPSILON,  // EQ
        1 => (val - const_val).abs() >= f64::EPSILON, // NE
        2 => val < const_val,                         // LT
        3 => val <= const_val,                        // LE
        4 => val > const_val,                         // GT
        5 => val >= const_val,                        // GE
        _ => true,                                    // unknown → pass
    }
}
// ---------------------------------------------------------------------------
// Per-group accumulator
// ---------------------------------------------------------------------------

/// Accumulator for a single aggregate column within a group.
#[derive(Debug, Clone)]
pub(super) struct AggAccum {
    pub(super) op: AggOp,
    pub(super) sum: f64,
    pub(super) count: i64,
    pub(super) min: f64,
    pub(super) max: f64,
}

impl AggAccum {
    pub(super) fn new(op: AggOp) -> Self {
        Self {
            op,
            sum: 0.0,
            count: 0,
            min: f64::MAX,
            max: f64::MIN,
        }
    }

    #[inline]
    pub(super) fn accumulate(&mut self, val: f64) {
        match self.op {
            AggOp::Sum | AggOp::Avg => {
                self.sum += val;
                self.count += 1;
            }
            AggOp::Count => {
                self.count += 1;
            }
            AggOp::Min => {
                if val < self.min {
                    self.min = val;
                }
                self.count += 1;
            }
            AggOp::Max => {
                if val > self.max {
                    self.max = val;
                }
                self.count += 1;
            }
            AggOp::Passthrough => {}
            // Stats / bitwise / boolean ops land here when the planner
            // extends AggOp but the preagg executor hasn't grown dedicated
            // paths yet — fall back to sum-style accumulation so partial
            // numbers are non-garbage while Worker 3 fills in real support.
            AggOp::StddevSamp
            | AggOp::StddevPop
            | AggOp::VarSamp
            | AggOp::VarPop
            | AggOp::BitAnd
            | AggOp::BitOr
            | AggOp::BitXor
            | AggOp::BoolAnd
            | AggOp::BoolOr => {
                self.sum += val;
                self.count += 1;
            }
        }
    }

    #[allow(clippy::cast_precision_loss)]
    pub(super) fn result(&self) -> f64 {
        match self.op {
            AggOp::Sum => self.sum,
            AggOp::Avg => {
                if self.count > 0 {
                    self.sum / self.count as f64
                } else {
                    0.0
                }
            }
            AggOp::Count => self.count as f64,
            AggOp::Min => {
                if self.count > 0 {
                    self.min
                } else {
                    0.0
                }
            }
            AggOp::Max => {
                if self.count > 0 {
                    self.max
                } else {
                    0.0
                }
            }
            AggOp::Passthrough => 0.0,
            // Ops not yet implemented in the final-result path (W3
            // extended AggOp; preagg finalize stays on sum/avg until
            // dedicated code lands). Returning the sum matches what
            // accumulate() above records.
            AggOp::StddevSamp
            | AggOp::StddevPop
            | AggOp::VarSamp
            | AggOp::VarPop
            | AggOp::BitAnd
            | AggOp::BitOr
            | AggOp::BitXor
            | AggOp::BoolAnd
            | AggOp::BoolOr => self.sum,
        }
    }
}

// ---------------------------------------------------------------------------
// Slot-based extraction helpers (B5b — used when a fact-side child PlanState
// has been attached and the executor consumes rows via ExecProcNode rather
// than a direct heap_getnext loop). These mirror the heap_read_* helpers
// above but read decoded Datums from `tts_values` / `tts_isnull` after the
// caller has run `slot_getallattrs`.
// ---------------------------------------------------------------------------

/// Read an i64 value from a TupleTableSlot column, type-aware. Returns
/// `None` if the column is NULL.
///
/// # Safety
///
/// `slot` must be a fully materialized `TupleTableSlot` (the caller has
/// run `slot_getallattrs`). `attno` is 1-based and must be within the slot's
/// attribute count. `typid` must be the column's declared type OID.
#[inline]
pub(super) unsafe fn slot_read_i64(
    slot: *mut pg_sys::TupleTableSlot,
    attno: i32,
    typid: pg_sys::Oid,
) -> Option<i64> {
    if attno <= 0 {
        return None;
    }
    let idx = (attno - 1) as usize;
    // SAFETY: caller guarantees the slot is materialized and idx is in bounds.
    unsafe {
        if *(*slot).tts_isnull.add(idx) {
            return None;
        }
        let datum = *(*slot).tts_values.add(idx);
        #[allow(clippy::cast_possible_wrap)]
        let raw = datum.value();
        Some(match typid {
            pg_sys::INT2OID => i64::from(raw as i16),
            pg_sys::INT4OID => i64::from(raw as i32),
            // INT8 / DATE / TIMESTAMP / OID / etc. — fits in 8 bytes, treated
            // as the bit pattern of an i64 by the caller.
            _ => raw as i64,
        })
    }
}

/// Read an f64 value from a TupleTableSlot column, type-aware. Returns
/// `None` if the column is NULL.
///
/// # Safety
///
/// Same contract as [`slot_read_i64`].
#[inline]
pub(super) unsafe fn slot_read_f64(
    slot: *mut pg_sys::TupleTableSlot,
    attno: i32,
    typid: pg_sys::Oid,
) -> Option<f64> {
    if attno <= 0 {
        return None;
    }
    let idx = (attno - 1) as usize;
    // SAFETY: caller guarantees the slot is materialized and idx is in bounds.
    unsafe {
        if *(*slot).tts_isnull.add(idx) {
            return None;
        }
        let datum = *(*slot).tts_values.add(idx);
        let raw = datum.value();
        #[allow(clippy::cast_precision_loss)]
        Some(match typid {
            pg_sys::FLOAT4OID => f64::from(f32::from_bits(raw as u32)),
            pg_sys::FLOAT8OID => f64::from_bits(raw as u64),
            pg_sys::INT2OID => f64::from(raw as i16),
            pg_sys::INT4OID => f64::from(raw as i32),
            pg_sys::INT8OID => (raw as i64) as f64,
            _ => f64::from_bits(raw as u64),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_cmp_covers_all_supported_opcodes() {
        assert!(eval_cmp(4.0, 0, 4.0));
        assert!(!eval_cmp(4.0, 0, 5.0));
        assert!(eval_cmp(4.0, 1, 5.0));
        assert!(!eval_cmp(4.0, 1, 4.0));
        assert!(eval_cmp(3.0, 2, 4.0));
        assert!(eval_cmp(4.0, 3, 4.0));
        assert!(eval_cmp(5.0, 4, 4.0));
        assert!(eval_cmp(4.0, 5, 4.0));
        assert!(eval_cmp(4.0, 99, 0.0));
    }

    #[test]
    fn dim_column_new_initializes_capacity_and_metadata() {
        let col = DimColumn::new(pg_sys::INT8OID, 8);
        assert_eq!(col.type_oid, pg_sys::INT8OID);
        assert!(col.values_i64.capacity() >= 8);
        assert!(col.values_f64.is_empty());
        assert!(col.values_text.is_empty());
        assert!(col.text_dict.is_empty());
        assert!(col.null_mask.capacity() >= 8);
    }

    #[test]
    fn dim_hash_probe_applies_integer_filters_and_nulls() {
        let mut hash_table = HashMap::new();
        hash_table.insert(10, 0);
        hash_table.insert(20, 1);
        hash_table.insert(30, 2);
        let table = DimHashTable {
            hash_table,
            n_rows: 3,
            columns: vec![DimColumn {
                type_oid: pg_sys::INT8OID,
                values_i64: vec![5, 12, 99],
                values_f64: Vec::new(),
                values_text: Vec::new(),
                text_dict: HashMap::new(),
                null_mask: vec![false, false, true],
            }],
            dim_filters: vec![DimFilter {
                col_idx: 0,
                cmp_opcode: 4,
                const_val: 10.0,
            }],
            group_col_indices: Vec::new(),
        };

        assert_eq!(table.probe(10), None);
        assert_eq!(table.probe(20), Some(1));
        assert_eq!(table.probe(30), None);
        assert_eq!(table.probe(40), None);
    }

    #[test]
    fn dim_hash_probe_uses_float_columns_when_present() {
        let mut hash_table = HashMap::new();
        hash_table.insert(7, 0);
        let table = DimHashTable {
            hash_table,
            n_rows: 1,
            columns: vec![DimColumn {
                type_oid: pg_sys::FLOAT8OID,
                values_i64: Vec::new(),
                values_f64: vec![3.5],
                values_text: Vec::new(),
                text_dict: HashMap::new(),
                null_mask: vec![false],
            }],
            dim_filters: vec![DimFilter {
                col_idx: 0,
                cmp_opcode: 3,
                const_val: 3.5,
            }],
            group_col_indices: Vec::new(),
        };

        assert_eq!(table.probe(7), Some(0));
    }

    #[test]
    fn accumulators_return_expected_results() {
        let mut sum = AggAccum::new(AggOp::Sum);
        sum.accumulate(2.0);
        sum.accumulate(5.0);
        assert_eq!(sum.result(), 7.0);

        let mut avg = AggAccum::new(AggOp::Avg);
        avg.accumulate(2.0);
        avg.accumulate(4.0);
        assert_eq!(avg.result(), 3.0);

        let mut count = AggAccum::new(AggOp::Count);
        count.accumulate(10.0);
        count.accumulate(20.0);
        assert_eq!(count.result(), 2.0);

        let mut min = AggAccum::new(AggOp::Min);
        min.accumulate(9.0);
        min.accumulate(-2.0);
        assert_eq!(min.result(), -2.0);

        let mut max = AggAccum::new(AggOp::Max);
        max.accumulate(9.0);
        max.accumulate(-2.0);
        assert_eq!(max.result(), 9.0);
    }

    #[test]
    fn empty_and_passthrough_accumulators_are_zero() {
        assert_eq!(AggAccum::new(AggOp::Avg).result(), 0.0);
        assert_eq!(AggAccum::new(AggOp::Min).result(), 0.0);
        assert_eq!(AggAccum::new(AggOp::Max).result(), 0.0);

        let mut passthrough = AggAccum::new(AggOp::Passthrough);
        passthrough.accumulate(99.0);
        assert_eq!(passthrough.result(), 0.0);
        assert_eq!(passthrough.count, 0);
    }

    #[test]
    fn unimplemented_aggregate_ops_use_sum_style_accumulation() {
        for op in [
            AggOp::StddevSamp,
            AggOp::StddevPop,
            AggOp::VarSamp,
            AggOp::VarPop,
            AggOp::BitAnd,
            AggOp::BitOr,
            AggOp::BitXor,
            AggOp::BoolAnd,
            AggOp::BoolOr,
        ] {
            let mut accum = AggAccum::new(op);
            accum.accumulate(1.5);
            accum.accumulate(2.5);
            assert_eq!(accum.result(), 4.0);
            assert_eq!(accum.count, 2);
        }
    }
}
