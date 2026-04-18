//! Partition/frame boundary detection for window functions.

use pgrx::pg_sys;

use super::WindowExecState;

impl WindowExecState {
    /// Build partition boundary markers by comparing partition key values.
    ///
    /// Returns a `u8` array where `1` marks the start of a new partition.
    ///
    /// # Safety
    ///
    /// `tupdesc` must be a valid `TupleDesc`. All tuples must be valid.
    pub(super) unsafe fn build_partition_starts(
        &self,
        attno: i32,
        tupdesc: pg_sys::TupleDesc,
    ) -> Vec<u8> {
        let n = self.tuples.len();
        let mut starts = vec![0u8; n];
        starts[0] = 1; // First row always starts a partition.

        let mut prev_datum: pg_sys::Datum = pg_sys::Datum::from(0);
        let mut prev_null = true;

        for (i, mt) in self.tuples.iter().enumerate() {
            let mut is_null = false;
            // SAFETY: mt is a valid MinimalTuple, tupdesc is valid.
            let datum = unsafe {
                let heap_tuple = pg_sys::heap_tuple_from_minimal_tuple(*mt);
                let d = pg_sys::heap_getattr(heap_tuple, attno, tupdesc, &mut is_null);
                pg_sys::pfree(heap_tuple.cast());
                d
            };

            if i == 0 {
                prev_datum = datum;
                prev_null = is_null;
                continue;
            }

            // New partition if null-ness changed, or both non-null and values differ.
            let new_partition = if is_null != prev_null {
                true
            } else if is_null {
                // Both NULL — same partition (PG treats NULLs as equal in PARTITION BY).
                false
            } else {
                datum.value() != prev_datum.value()
            };

            if new_partition {
                starts[i] = 1;
            }

            prev_datum = datum;
            prev_null = is_null;
        }

        starts
    }
}
