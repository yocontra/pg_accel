//! [`PartialEmitter`] implementations for each supported aggregate.
//!
//! Each emitter converts a [`ColumnAccumulator`] into the Datum PG's
//! combine / finalize function expects. All bodies execute on the main
//! backend thread (see [`PartialEmitter`] safety contract) because they may
//! call into PG — `palloc`, `construct_array`, and `DirectFunctionCall1Coll`
//! all require a valid `CurrentMemoryContext`.

use pgrx::pg_sys;

use super::{ColumnAccumulator, PartialEmitter};

// ---------------------------------------------------------------------------
// Helper: transmute a pgrx-generated Rust-ABI fn into the extern "C-unwind"
// pointer `DirectFunctionCall1Coll` expects. Mirrors the pattern already in
// `agg/execute.rs`.
// ---------------------------------------------------------------------------

/// Call `int8_numeric(int64)` returning a Numeric Datum.
///
/// # Safety
/// Must run on the main backend thread with a valid `CurrentMemoryContext`.
unsafe fn int8_to_numeric(v: i64) -> pg_sys::Datum {
    let d = pg_sys::Datum::from(v);
    // SAFETY: Calling PG's int8_numeric via DirectFunctionCall1Coll on the
    // main backend thread. pgrx generates the `extern "C"` function item;
    // DirectFunctionCall1Coll's declared parameter is `extern "C-unwind"`.
    // The transmute reconciles only that ABI-variant mismatch — the target
    // function is identical.
    unsafe {
        let fptr: unsafe extern "C-unwind" fn(
            *mut pg_sys::FunctionCallInfoBaseData,
        ) -> pg_sys::Datum = core::mem::transmute(pg_sys::int8_numeric as *const ());
        pg_sys::DirectFunctionCall1Coll(Some(fptr), pg_sys::InvalidOid, d)
    }
}

/// Build a Datum carrying an f64 (stored as its bit pattern).
fn f64_datum(v: f64) -> pg_sys::Datum {
    pg_sys::Datum::from(v.to_bits())
}

// ---------------------------------------------------------------------------
// ScalarPassthrough — SUM(float4|float8|int8) where transtype == rtype.
// ---------------------------------------------------------------------------

/// SUM where `transtype == result type` for pass-by-value scalars.
///
/// Handled `transtype` OIDs:
/// - `FLOAT4OID`: Datum carries the f32 bit pattern.
/// - `FLOAT8OID`: Datum carries the f64 bit pattern.
/// - `INT8OID`:   Datum carries the i64 as a raw integer.
pub struct ScalarPassthrough {
    pub transtype: pg_sys::Oid,
}

impl PartialEmitter for ScalarPassthrough {
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        if !acc.has_value {
            return (pg_sys::Datum::from(0u64), true);
        }
        let datum = match self.transtype {
            pg_sys::FLOAT4OID => {
                // FLOAT4 pass-by-value: store f32 bits in the Datum.
                let bits = (acc.sum as f32).to_bits();
                pg_sys::Datum::from(bits)
            }
            pg_sys::INT8OID => pg_sys::Datum::from(acc.sum as i64),
            // FLOAT8OID or anything else pass-by-value: f64 bits.
            _ => f64_datum(acc.sum),
        };
        (datum, false)
    }

    fn emit_type_oid(&self) -> pg_sys::Oid {
        self.transtype
    }
}

// ---------------------------------------------------------------------------
// IntegerSumPromotion — SUM(int4|int2) promoted to int8.
// ---------------------------------------------------------------------------

/// `SUM(int4)` / `SUM(int2)` — PG promotes the transtype to int8.
pub struct IntegerSumPromotion;

impl PartialEmitter for IntegerSumPromotion {
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        if !acc.has_value {
            return (pg_sys::Datum::from(0u64), true);
        }
        (pg_sys::Datum::from(acc.sum as i64), false)
    }

    fn emit_type_oid(&self) -> pg_sys::Oid {
        pg_sys::INT8OID
    }
}

// ---------------------------------------------------------------------------
// CountEmitter — COUNT(*) / COUNT(x) → int8.
// ---------------------------------------------------------------------------

/// COUNT — never returns NULL; zero rows yield `0::int8`.
pub struct CountEmitter;

impl PartialEmitter for CountEmitter {
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        (pg_sys::Datum::from(acc.count as i64), false)
    }

    fn emit_type_oid(&self) -> pg_sys::Oid {
        pg_sys::INT8OID
    }
}

// ---------------------------------------------------------------------------
// NumericSumEmitter — SUM(int8|numeric) → numeric.
// ---------------------------------------------------------------------------

/// `SUM(int8)` / `SUM(numeric)` — emits a Numeric transition state.
///
/// TODO: full NUMERIC precision requires an arbitrary-precision accumulator.
/// The current f64 accumulator is accurate for typical OLAP workloads but
/// loses ULPs near 2^53. Swap in a real numeric accumulator once `ColumnAccumulator`
/// supports it.
pub struct NumericSumEmitter;

impl PartialEmitter for NumericSumEmitter {
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        if !acc.has_value {
            return (pg_sys::Datum::from(0u64), true);
        }
        // SAFETY: Main-thread PG call; allocates in CurrentMemoryContext.
        let datum = unsafe { int8_to_numeric(acc.sum as i64) };
        (datum, false)
    }

    fn emit_type_oid(&self) -> pg_sys::Oid {
        pg_sys::NUMERICOID
    }
}

// ---------------------------------------------------------------------------
// Float8StatsEmitter — AVG/STDDEV/VAR over float8.
// ---------------------------------------------------------------------------

/// Emits the `float8[3] = [N, sum, sum_squared]` transition state used by
/// PG's `float8_accum` family (AVG / STDDEV / VARIANCE for float types).
///
/// If `serialize_fn_oid` is set and not `InvalidOid`, the array is passed to
/// that serialize function to produce a `bytea` (aggregates with INTERNAL
/// transtype). Otherwise the float8[] Datum is returned directly — suitable
/// for plain float8_accum (transtype IS float8[]). In both cases the public
/// `emit_type_oid()` reflects the transition-state shape PG will ship.
pub struct Float8StatsEmitter {
    /// OID of the `aggserialfn`, or `InvalidOid` if the transtype is already
    /// a float8[] (no serialize needed).
    pub serialize_fn_oid: pg_sys::Oid,
}

impl Float8StatsEmitter {
    /// True iff a real serialize function should be invoked.
    const fn has_serialize(&self) -> bool {
        // `Oid` doesn't implement const-eq; compare the inner u32.
        self.serialize_fn_oid.to_u32() != 0
    }
}

impl PartialEmitter for Float8StatsEmitter {
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        if acc.count == 0 {
            return (pg_sys::Datum::from(0u64), true);
        }
        // Build Datum[3] = [N, sum, sum_sq] as float8 bit-patterns.
        let mut elems: [pg_sys::Datum; 3] = [
            f64_datum(acc.count as f64),
            f64_datum(acc.sum),
            f64_datum(acc.sum_sq),
        ];
        // SAFETY: construct_array copies the Datum slice into a palloc'd
        // ArrayType; `elems` is a 3-element stack array of f64 Datums.
        // FLOAT8OID: pass-by-value=true, length=8, alignment 'd' (double).
        let arr_ptr = unsafe {
            pg_sys::construct_array(
                elems.as_mut_ptr(),
                3,
                pg_sys::FLOAT8OID,
                8,
                true,
                b'd' as core::ffi::c_char,
            )
        };
        if arr_ptr.is_null() {
            return (pg_sys::Datum::from(0u64), true);
        }
        let array_datum = pg_sys::Datum::from(arr_ptr as usize);

        if !self.has_serialize() {
            // float8_accum transtype IS float8[] — ship directly.
            return (array_datum, false);
        }
        // INTERNAL transtype (e.g. numeric_accum): run aggserialfn → bytea.
        // SAFETY: Main-thread PG call; serialize_fn_oid is a valid pg_proc OID.
        let out = unsafe {
            pg_sys::OidFunctionCall1Coll(self.serialize_fn_oid, pg_sys::InvalidOid, array_datum)
        };
        (out, false)
    }

    fn emit_type_oid(&self) -> pg_sys::Oid {
        if self.has_serialize() {
            pg_sys::BYTEAOID
        } else {
            pg_sys::FLOAT8ARRAYOID
        }
    }
}

// ---------------------------------------------------------------------------
// BitReductionEmitter — BIT_AND / BIT_OR on int2/int4/int8.
// ---------------------------------------------------------------------------

/// Which bitwise reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOp {
    And,
    Or,
}

/// Emits the transition-state for `BIT_AND(int)` / `BIT_OR(int)`.
///
/// `transtype` selects the width of the emitted integer (INT2/INT4/INT8).
pub struct BitReductionEmitter {
    pub transtype: pg_sys::Oid,
    pub op: BitOp,
}

impl PartialEmitter for BitReductionEmitter {
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        if !acc.has_value {
            return (pg_sys::Datum::from(0u64), true);
        }
        // The emitter itself is value-neutral between And/Or: both reduce
        // into `bit_acc`. `op` is kept on the struct for symmetry with
        // classification code and diagnostics.
        let _ = self.op;
        let datum = match self.transtype {
            pg_sys::INT2OID => pg_sys::Datum::from(acc.bit_acc as i16),
            pg_sys::INT4OID => pg_sys::Datum::from(acc.bit_acc as i32),
            // INT8OID or anything else int-ish.
            _ => pg_sys::Datum::from(acc.bit_acc),
        };
        (datum, false)
    }

    fn emit_type_oid(&self) -> pg_sys::Oid {
        self.transtype
    }
}

// ---------------------------------------------------------------------------
// BoolReductionEmitter — BOOL_AND / BOOL_OR.
// ---------------------------------------------------------------------------

/// Which boolean reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
}

/// Emits the transition-state for `BOOL_AND` / `BOOL_OR` / `EVERY`.
pub struct BoolReductionEmitter {
    pub op: BoolOp,
}

impl PartialEmitter for BoolReductionEmitter {
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        if !acc.has_value {
            return (pg_sys::Datum::from(0u64), true);
        }
        let _ = self.op;
        (pg_sys::Datum::from(acc.bool_acc as u8 as u64), false)
    }

    fn emit_type_oid(&self) -> pg_sys::Oid {
        pg_sys::BOOLOID
    }
}
