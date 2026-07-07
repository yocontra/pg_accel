//! Window function semantics — enum, variants, and column extraction helpers.

use pgrx::pg_sys;

use super::WindowExecState;

// ---------------------------------------------------------------------------
// Window function specification
// ---------------------------------------------------------------------------

/// Which window function to compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowFunc {
    RowNumber,
    Rank,
    DenseRank,
    Sum,
    Count,
    Lag,
    Lead,
}

impl WindowFunc {
    /// Encode as integer for `custom_private` serialization.
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::RowNumber => 0,
            Self::Rank => 1,
            Self::DenseRank => 2,
            Self::Sum => 3,
            Self::Count => 4,
            Self::Lag => 5,
            Self::Lead => 6,
        }
    }

    /// Decode from integer.
    #[must_use]
    pub const fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::RowNumber),
            1 => Some(Self::Rank),
            2 => Some(Self::DenseRank),
            3 => Some(Self::Sum),
            4 => Some(Self::Count),
            5 => Some(Self::Lag),
            6 => Some(Self::Lead),
            _ => None,
        }
    }
}

/// Convert a by-value `Datum`'s raw word to `f64` based on the column type OID.
///
/// Mirrors the per-`typid` dispatch in
/// [`crate::engine::materialize::tuple_extract::extract_f64`]: a raw
/// `Datum` word does **not** hold `f64` bits for int2/int4/int8/float4
/// columns, so reinterpreting it as `f64::from_bits` yields garbage. Each
/// supported numeric type is decoded from the low bytes of the word and
/// converted to `f64`.
///
/// Returns `None` for by-reference / unsupported types (e.g. `text`,
/// `numeric`): their `Datum` word is a pointer, and reinterpreting it as a
/// float would read a pointer value as a number. Callers must decline or
/// raise rather than fabricate a value.
#[inline]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub(super) fn datum_value_to_f64(raw: usize, typid: pg_sys::Oid) -> Option<f64> {
    match typid {
        pg_sys::FLOAT4OID => Some(f64::from(f32::from_bits(raw as u32))),
        pg_sys::FLOAT8OID => Some(f64::from_bits(raw as u64)),
        pg_sys::INT2OID => Some(f64::from(raw as u16 as i16)),
        pg_sys::INT4OID => Some(f64::from(raw as u32 as i32)),
        pg_sys::INT8OID => Some((raw as u64 as i64) as f64),
        _ => None,
    }
}

/// Look up the type OID of a 1-based attribute in a tuple descriptor.
///
/// # Safety
///
/// `tupdesc` must be a valid `TupleDesc` and `attno` in `[1, natts]`.
unsafe fn attr_type_oid(tupdesc: pg_sys::TupleDesc, attno: i32) -> pg_sys::Oid {
    // SAFETY: caller guarantees tupdesc valid and attno in range.
    let att = unsafe { &*crate::engine::pg_compat::tuple_desc_attr(tupdesc, (attno - 1) as usize) };
    att.atttypid
}

/// Compute the per-row NULL mask for a running window `SUM`.
///
/// The GPU kernel (`sycl_window_sum`, `pgaccel-kernels/src/window.cpp`)
/// computes, for row `i`, the Kahan sum over the non-NULL values in
/// `[partition_start(i) .. i]`, writing `0.0` when that range holds no
/// non-NULL value. SQL semantics say `SUM` over zero non-NULL inputs is
/// `NULL`, not `0.0`. This helper marks row `i` as NULL (`1`) until the
/// first non-NULL value is seen in its partition — identical in spirit to
/// the null tracking `Lag`/`Lead` already carry.
///
/// `partition_starts[i] == 1` marks the first row of a partition;
/// `null_mask[i] == 1` marks a NULL input value at row `i`.
pub(super) fn compute_sum_null_mask(partition_starts: &[u8], null_mask: &[u8]) -> Vec<u8> {
    let n = partition_starts.len();
    let mut result_nulls = vec![0u8; n];
    let mut seen_value = false;
    for i in 0..n {
        if partition_starts[i] == 1 {
            seen_value = false;
        }
        if null_mask.get(i).copied().unwrap_or(0) == 0 {
            seen_value = true;
        }
        result_nulls[i] = u8::from(!seen_value);
    }
    result_nulls
}

impl WindowExecState {
    /// Extract a column as `f64` values from stored MinimalTuples.
    ///
    /// Dispatches on the column's type OID (int2/int4/int8/float4/float8)
    /// rather than reinterpreting the raw `Datum` word as `f64` bits. NULLs
    /// yield `0.0` (callers pair this with a null mask where the value
    /// matters). By-reference / unsupported column types raise a PG error
    /// rather than fabricate a float from a pointer.
    ///
    /// # Safety
    ///
    /// Must run on the main backend thread. `tupdesc` must be valid, `attno`
    /// in range, and all tuples valid `MinimalTuple`s.
    pub(super) unsafe fn extract_f64_column(
        &self,
        attno: i32,
        tupdesc: pg_sys::TupleDesc,
    ) -> Vec<f64> {
        let n = self.tuples.len();
        let mut values = Vec::with_capacity(n);
        // SAFETY: tupdesc valid, attno in range per caller contract.
        let typid = unsafe { attr_type_oid(tupdesc, attno) };

        for mt in &self.tuples {
            let mut is_null = false;
            // SAFETY: mt is a valid MinimalTuple, tupdesc is valid.
            let datum = unsafe {
                let heap_tuple = pg_sys::heap_tuple_from_minimal_tuple(*mt);
                let d = pg_sys::heap_getattr(heap_tuple, attno, tupdesc, &mut is_null);
                pg_sys::pfree(heap_tuple.cast());
                d
            };

            if is_null {
                values.push(0.0);
            } else if let Some(v) = datum_value_to_f64(datum.value(), typid) {
                values.push(v);
            } else {
                pgrx::error!(
                    "pg_accel: window function cannot extract column attno {attno} \
                     of type OID {} as f64 (by-reference or unsupported type); \
                     refusing to reinterpret its Datum pointer as a number",
                    typid.to_u32()
                );
            }
        }

        values
    }
}

impl WindowExecState {
    /// Extract a column as `f64` values with a null mask.
    ///
    /// Same typed dispatch as [`extract_f64_column`](Self::extract_f64_column);
    /// additionally reports per-row NULLs so downstream SUM/LAG/LEAD can honor
    /// SQL NULL semantics.
    ///
    /// # Safety
    ///
    /// Must run on the main backend thread. `tupdesc` must be valid, `attno`
    /// in range, and all tuples valid `MinimalTuple`s.
    pub(super) unsafe fn extract_f64_column_with_nulls(
        &self,
        attno: i32,
        tupdesc: pg_sys::TupleDesc,
    ) -> (Vec<f64>, Vec<u8>) {
        let n = self.tuples.len();
        let mut values = Vec::with_capacity(n);
        let mut null_mask = Vec::with_capacity(n);
        // SAFETY: tupdesc valid, attno in range per caller contract.
        let typid = unsafe { attr_type_oid(tupdesc, attno) };

        for mt in &self.tuples {
            let mut is_null = false;
            // SAFETY: mt is a valid MinimalTuple, tupdesc is valid.
            let datum = unsafe {
                let heap_tuple = pg_sys::heap_tuple_from_minimal_tuple(*mt);
                let d = pg_sys::heap_getattr(heap_tuple, attno, tupdesc, &mut is_null);
                pg_sys::pfree(heap_tuple.cast());
                d
            };

            if is_null {
                values.push(0.0);
                null_mask.push(1);
            } else if let Some(v) = datum_value_to_f64(datum.value(), typid) {
                values.push(v);
                null_mask.push(0);
            } else {
                pgrx::error!(
                    "pg_accel: window function cannot extract column attno {attno} \
                     of type OID {} as f64 (by-reference or unsupported type); \
                     refusing to reinterpret its Datum pointer as a number",
                    typid.to_u32()
                );
            }
        }

        (values, null_mask)
    }
}

impl WindowExecState {
    /// Extract null mask for a column (1 = null, 0 = non-null).
    ///
    /// # Safety
    ///
    /// `tupdesc` must be valid. All tuples must be valid.
    pub(super) unsafe fn extract_null_mask(
        &self,
        attno: i32,
        tupdesc: pg_sys::TupleDesc,
    ) -> Vec<u8> {
        let n = self.tuples.len();
        let mut mask = Vec::with_capacity(n);

        for mt in &self.tuples {
            let mut is_null = false;
            // SAFETY: mt is a valid MinimalTuple, tupdesc is valid.
            unsafe {
                let heap_tuple = pg_sys::heap_tuple_from_minimal_tuple(*mt);
                pg_sys::heap_getattr(heap_tuple, attno, tupdesc, &mut is_null);
                pg_sys::pfree(heap_tuple.cast());
            };

            mask.push(u8::from(is_null));
        }

        mask
    }
}

#[cfg(test)]
mod tests {
    use super::{compute_sum_null_mask, datum_value_to_f64};
    use pgrx::pg_sys;

    // ---- datum_value_to_f64: typed extraction, not raw-bit reinterpret ------

    #[test]
    fn extract_int4() {
        // A raw Datum word for int4 = 5 is the integer 5, NOT f64::from_bits(5).
        let raw = 5usize;
        assert_eq!(datum_value_to_f64(raw, pg_sys::INT4OID), Some(5.0));
        assert_ne!(
            datum_value_to_f64(raw, pg_sys::INT4OID),
            Some(f64::from_bits(5))
        );
    }

    #[test]
    fn extract_int4_negative() {
        // -3 as int4 lives in the low 32 bits as a two's-complement pattern.
        let raw = (-3i32) as u32 as usize;
        assert_eq!(datum_value_to_f64(raw, pg_sys::INT4OID), Some(-3.0));
    }

    #[test]
    fn extract_int2() {
        let raw = (-7i16) as u16 as usize;
        assert_eq!(datum_value_to_f64(raw, pg_sys::INT2OID), Some(-7.0));
        assert_eq!(datum_value_to_f64(9usize, pg_sys::INT2OID), Some(9.0));
    }

    #[test]
    fn extract_int8() {
        let raw = (-123_456_789i64) as u64 as usize;
        assert_eq!(
            datum_value_to_f64(raw, pg_sys::INT8OID),
            Some(-123_456_789.0)
        );
    }

    #[test]
    fn extract_float4() {
        // float4 stores f32 bits in the low 32 bits of the Datum word.
        let v = 2.5f32;
        let raw = v.to_bits() as usize;
        assert_eq!(datum_value_to_f64(raw, pg_sys::FLOAT4OID), Some(2.5));
    }

    #[test]
    fn extract_float8() {
        let v = 3.25f64;
        let raw = v.to_bits() as usize;
        assert_eq!(datum_value_to_f64(raw, pg_sys::FLOAT8OID), Some(3.25));
    }

    #[test]
    fn extract_byref_type_declines() {
        // text/numeric are by-reference: the Datum word is a pointer and must
        // not be reinterpreted as a number.
        assert_eq!(datum_value_to_f64(0xdead_beef, pg_sys::TEXTOID), None);
        assert_eq!(datum_value_to_f64(0xdead_beef, pg_sys::NUMERICOID), None);
    }

    // ---- compute_sum_null_mask: all-NULL vs mixed-NULL partitions -----------

    #[test]
    fn sum_null_mask_all_null_partition() {
        // One partition, every input NULL → every SUM row is NULL.
        let starts = [1u8, 0, 0];
        let nulls = [1u8, 1, 1];
        assert_eq!(compute_sum_null_mask(&starts, &nulls), vec![1, 1, 1]);
    }

    #[test]
    fn sum_null_mask_mixed_partition() {
        // NULL until the first non-NULL, then non-NULL for the running sum.
        let starts = [1u8, 0, 0, 0];
        let nulls = [1u8, 1, 0, 1];
        assert_eq!(compute_sum_null_mask(&starts, &nulls), vec![1, 1, 0, 0]);
    }

    #[test]
    fn sum_null_mask_resets_per_partition() {
        // Partition A has a value; partition B is all NULL → B rows are NULL.
        let starts = [1u8, 0, 1, 0];
        let nulls = [0u8, 0, 1, 1];
        assert_eq!(compute_sum_null_mask(&starts, &nulls), vec![0, 0, 1, 1]);
    }

    #[test]
    fn sum_null_mask_no_nulls_all_have_value() {
        let starts = [1u8, 0, 1, 0];
        let nulls = [0u8, 0, 0, 0];
        assert_eq!(compute_sum_null_mask(&starts, &nulls), vec![0, 0, 0, 0]);
    }

    #[test]
    fn sum_null_mask_empty() {
        assert!(compute_sum_null_mask(&[], &[]).is_empty());
    }
}
