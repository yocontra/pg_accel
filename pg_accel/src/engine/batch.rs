//! Batch accumulator for collecting rows before dispatch.
//!
//! The [`BatchAccumulator`] buffers `(Datum, is_null)` pairs until a
//! configurable batch size is reached, at which point the caller can
//! flush the batch to downstream processing (GPU dispatch, batched eval,
//! etc.).  NULLs are stored as-is — filtering happens at the dispatch
//! level, not here.

use pgrx::pg_sys;

/// Collects `(Datum, is_null)` tuples into fixed-size batches.
///
/// Datums are stored by value (they are pointer-sized integers), so
/// there is no allocation beyond the backing `Vec`.
pub struct BatchAccumulator {
    buffer: Vec<(pg_sys::Datum, bool)>,
    batch_size: usize,
    total_flushed: usize,
}

impl BatchAccumulator {
    /// Creates a new accumulator that will signal readiness every
    /// `batch_size` rows.
    #[must_use]
    pub fn new(batch_size: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(batch_size),
            batch_size,
            total_flushed: 0,
        }
    }

    /// Appends a single datum (possibly NULL) to the buffer.
    pub fn push(&mut self, datum: pg_sys::Datum, is_null: bool) {
        self.buffer.push((datum, is_null));
    }

    /// Returns `true` when the buffer has accumulated at least
    /// `batch_size` rows and is ready to be flushed.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.buffer.len() >= self.batch_size
    }

    /// Drains the buffer and returns the batch as a `Vec`.
    ///
    /// Increments `total_flushed` by the number of rows returned.
    /// The internal buffer is left empty and ready for the next batch.
    pub fn flush(&mut self) -> Vec<(pg_sys::Datum, bool)> {
        self.total_flushed += self.buffer.len();
        std::mem::take(&mut self.buffer)
    }

    /// Returns any remaining rows that did not fill a complete batch.
    ///
    /// Call this after the last row has been pushed (e.g. when a LIMIT
    /// is reached or the scan completes) to retrieve the partial tail
    /// batch.  Increments `total_flushed` accordingly.
    pub fn finish(&mut self) -> Vec<(pg_sys::Datum, bool)> {
        self.flush()
    }

    /// Total number of rows that have been flushed (via [`flush`] or
    /// [`finish`]) since creation.
    #[must_use]
    pub fn total_flushed(&self) -> usize {
        self.total_flushed
    }

    /// Number of rows currently buffered (not yet flushed).
    #[must_use]
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Returns `true` when the buffer holds no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a non-null datum from a usize value.
    fn datum(val: usize) -> (pg_sys::Datum, bool) {
        (pg_sys::Datum::from(val), false)
    }

    /// Helper: create a null datum.
    fn null_datum() -> (pg_sys::Datum, bool) {
        (pg_sys::Datum::from(0), true)
    }

    #[test]
    fn three_flushes_for_5000_rows_batch_2000() {
        let mut acc = BatchAccumulator::new(2000);

        // Push 5000 rows.
        for i in 0..5000 {
            acc.push(pg_sys::Datum::from(i), false);
        }

        // First flush — should contain first 2000 but since we pushed
        // all at once, buffer has 5000. Each flush drains everything,
        // so we simulate the realistic pattern: push until full, flush,
        // repeat.
        drop(acc);

        // Realistic pattern: push row-by-row, flush when full.
        let mut acc = BatchAccumulator::new(2000);
        let mut flushes: Vec<Vec<(pg_sys::Datum, bool)>> = Vec::new();

        for i in 0..5000 {
            acc.push(pg_sys::Datum::from(i), false);
            if acc.is_full() {
                flushes.push(acc.flush());
            }
        }
        // Remaining partial batch.
        let tail = acc.finish();
        if !tail.is_empty() {
            flushes.push(tail);
        }

        assert_eq!(flushes.len(), 3);
        assert_eq!(flushes[0].len(), 2000);
        assert_eq!(flushes[1].len(), 2000);
        assert_eq!(flushes[2].len(), 1000);
        assert_eq!(acc.total_flushed(), 5000);
    }

    #[test]
    fn single_finish_for_partial_batch() {
        let mut acc = BatchAccumulator::new(2000);

        for i in 0..100 {
            acc.push(pg_sys::Datum::from(i), false);
        }

        assert!(!acc.is_full());
        let batch = acc.finish();
        assert_eq!(batch.len(), 100);
        assert_eq!(acc.total_flushed(), 100);
        assert!(acc.is_empty());
    }

    #[test]
    fn null_rows_tracked() {
        let mut acc = BatchAccumulator::new(10);

        acc.push(pg_sys::Datum::from(1_usize), false);
        acc.push(pg_sys::Datum::from(0_usize), true); // NULL
        acc.push(pg_sys::Datum::from(2_usize), false);
        acc.push(pg_sys::Datum::from(0_usize), true); // NULL

        let batch = acc.finish();
        assert_eq!(batch.len(), 4);

        let nulls: Vec<_> = batch.iter().filter(|(_, is_null)| *is_null).collect();
        assert_eq!(nulls.len(), 2);

        let non_nulls: Vec<_> = batch.iter().filter(|(_, is_null)| !is_null).collect();
        assert_eq!(non_nulls.len(), 2);
    }

    #[test]
    fn zero_rows_empty_finish() {
        let mut acc = BatchAccumulator::new(100);
        let batch = acc.finish();
        assert!(batch.is_empty());
        assert_eq!(acc.total_flushed(), 0);
        assert!(acc.is_empty());
    }

    #[test]
    fn batch_size_one_flushes_every_row() {
        let mut acc = BatchAccumulator::new(1);
        let mut flushes = Vec::new();

        for i in 0..5 {
            acc.push(pg_sys::Datum::from(i), false);
            if acc.is_full() {
                flushes.push(acc.flush());
            }
        }

        assert_eq!(flushes.len(), 5);
        for batch in &flushes {
            assert_eq!(batch.len(), 1);
        }
        assert_eq!(acc.total_flushed(), 5);
        assert!(acc.is_empty());
    }

    #[test]
    fn len_and_is_empty() {
        let mut acc = BatchAccumulator::new(10);
        assert!(acc.is_empty());
        assert_eq!(acc.len(), 0);

        acc.push(datum(1).0, datum(1).1);
        assert!(!acc.is_empty());
        assert_eq!(acc.len(), 1);

        let _ = acc.flush();
        assert!(acc.is_empty());
        assert_eq!(acc.len(), 0);
    }

    #[test]
    fn total_flushed_accumulates() {
        let mut acc = BatchAccumulator::new(3);

        for i in 0..3 {
            acc.push(pg_sys::Datum::from(i), false);
        }
        let _ = acc.flush();
        assert_eq!(acc.total_flushed(), 3);

        for i in 0..3 {
            acc.push(pg_sys::Datum::from(i), false);
        }
        let _ = acc.flush();
        assert_eq!(acc.total_flushed(), 6);

        acc.push(pg_sys::Datum::from(0_usize), true);
        let _ = acc.finish();
        assert_eq!(acc.total_flushed(), 7);
    }

    #[test]
    fn null_passthrough_preserves_datum_value() {
        let mut acc = BatchAccumulator::new(10);

        // Even for NULLs, the datum value is preserved (caller may
        // use it for diagnostics or toast pointer cleanup).
        let sentinel = pg_sys::Datum::from(0xDEAD_BEEF_usize);
        acc.push(sentinel, true);

        let batch = acc.finish();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].0, sentinel);
        assert!(batch[0].1);
    }

    #[test]
    fn limit_scenario_stops_early() {
        // Simulate LIMIT 50 with batch_size=2000: caller stops pushing
        // after 50 rows and calls finish().
        let mut acc = BatchAccumulator::new(2000);

        for i in 0..50 {
            acc.push(pg_sys::Datum::from(i), false);
        }

        assert!(!acc.is_full());
        let batch = acc.finish();
        assert_eq!(batch.len(), 50);
        assert_eq!(acc.total_flushed(), 50);
    }
}
