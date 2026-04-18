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

impl WindowExecState {
    /// Extract a column as `f64` values from stored MinimalTuples.
    ///
    /// # Safety
    ///
    /// `tupdesc` must be valid. All tuples must be valid.
    pub(super) unsafe fn extract_f64_column(
        &self,
        attno: i32,
        tupdesc: pg_sys::TupleDesc,
    ) -> Vec<f64> {
        let n = self.tuples.len();
        let mut values = Vec::with_capacity(n);

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
            } else {
                values.push(f64::from_bits(datum.value() as u64));
            }
        }

        values
    }
}

impl WindowExecState {
    /// Extract a column as `f64` values with null mask.
    ///
    /// # Safety
    ///
    /// `tupdesc` must be valid. All tuples must be valid.
    pub(super) unsafe fn extract_f64_column_with_nulls(
        &self,
        attno: i32,
        tupdesc: pg_sys::TupleDesc,
    ) -> (Vec<f64>, Vec<u8>) {
        let n = self.tuples.len();
        let mut values = Vec::with_capacity(n);
        let mut null_mask = Vec::with_capacity(n);

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
            } else {
                values.push(f64::from_bits(datum.value() as u64));
                null_mask.push(0);
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
