//! [`PartialEmitter`] implementations for each supported aggregate.
//!
//! Worker 2 fills in real bodies. The stubs below compile and expose the
//! public surface so other workers can import and reference these types.

use pgrx::pg_sys;

use super::{ColumnAccumulator, PartialEmitter};

/// SUM/MIN/MAX where `transtype == result type`.
pub struct ScalarPassthrough {
    pub transtype: pg_sys::Oid,
}

impl PartialEmitter for ScalarPassthrough {
    unsafe fn emit(&self, _acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        todo!("Worker 2: ScalarPassthrough::emit")
    }
    fn emit_type_oid(&self) -> pg_sys::Oid {
        self.transtype
    }
}

/// `SUM(int4)` promotes to int8.
pub struct IntegerSumPromotion;

impl PartialEmitter for IntegerSumPromotion {
    unsafe fn emit(&self, _acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        todo!("Worker 2: IntegerSumPromotion::emit")
    }
    fn emit_type_oid(&self) -> pg_sys::Oid {
        pg_sys::INT8OID
    }
}

/// `COUNT(*)` / `COUNT(x)` → int8.
pub struct CountEmitter;

impl PartialEmitter for CountEmitter {
    unsafe fn emit(&self, _acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        todo!("Worker 2: CountEmitter::emit")
    }
    fn emit_type_oid(&self) -> pg_sys::Oid {
        pg_sys::INT8OID
    }
}

/// `SUM(int8)` / `SUM(numeric)` — emits numeric transition state.
pub struct NumericSumEmitter;

impl PartialEmitter for NumericSumEmitter {
    unsafe fn emit(&self, _acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        todo!("Worker 2: NumericSumEmitter::emit")
    }
    fn emit_type_oid(&self) -> pg_sys::Oid {
        pg_sys::NUMERICOID
    }
}

/// `AVG` / `STDDEV` / `VARIANCE` — serializes `(count, sum, sum_sq)` via
/// the aggregate's `aggserialfn` to produce a bytea.
pub struct Float8StatsEmitter {
    pub serialize_fn_oid: pg_sys::Oid,
}

impl PartialEmitter for Float8StatsEmitter {
    unsafe fn emit(&self, _acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        todo!("Worker 2: Float8StatsEmitter::emit — construct float8[3] + call serialize_fn")
    }
    fn emit_type_oid(&self) -> pg_sys::Oid {
        pg_sys::BYTEAOID
    }
}
