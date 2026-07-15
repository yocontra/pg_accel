//! Cursor for draining masked executor result buffers.
//!
//! Several executor nodes buffer rows, keep a parallel pass/fail mask, and
//! return one passing row per PostgreSQL executor call. `ResultDrain` owns only
//! the cursor policy: missing mask entries are treated as `false`, failed rows
//! are skipped, and the cursor always advances past every inspected row.

use pgrx::pg_sys;

/// Stateful cursor over a result buffer and pass mask.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct ResultDrain {
    position: usize,
}

impl ResultDrain {
    /// Construct a drain positioned at the start of a buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self { position: 0 }
    }

    /// Current cursor position in the source buffer.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.position
    }

    /// Reset the cursor to the start of a buffer.
    pub fn reset(&mut self) {
        self.position = 0;
    }

    /// Set the cursor position explicitly.
    ///
    /// This is intended for rescan or LIMIT-like state transfer. Positions past
    /// the end of the next source slice are legal and will drain as exhausted.
    pub fn set_position(&mut self, position: usize) {
        self.position = position;
    }

    /// Number of source rows not yet inspected.
    #[must_use]
    pub fn remaining<T>(&self, rows: &[T]) -> usize {
        rows.len().saturating_sub(self.position)
    }

    /// Whether the cursor has reached or passed the end of `rows`.
    #[must_use]
    pub fn is_exhausted<T>(&self, rows: &[T]) -> bool {
        self.position >= rows.len()
    }

    /// Return the next passing, present row index.
    ///
    /// `mask[idx] == true` marks a row as passing. If `mask` is shorter than
    /// `rows`, missing mask entries are treated as `false`, matching the
    /// defensive behavior used by the current scan drain path. `is_present`
    /// lets callers skip null tuple pointers or other tombstones without
    /// coupling this cursor to a concrete row representation.
    pub fn next_index<T>(
        &mut self,
        rows: &[T],
        mask: &[bool],
        mut is_present: impl FnMut(&T) -> bool,
    ) -> Option<usize> {
        while self.position < rows.len() {
            let idx = self.position;
            self.position += 1;

            let passed = mask.get(idx).copied().unwrap_or(false);
            if passed && is_present(&rows[idx]) {
                return Some(idx);
            }
        }

        None
    }

    /// Return the next passing, present row by reference.
    pub fn next_item<'a, T>(
        &mut self,
        rows: &'a [T],
        mask: &[bool],
        is_present: impl FnMut(&T) -> bool,
    ) -> Option<(usize, &'a T)> {
        let idx = self.next_index(rows, mask, is_present)?;
        Some((idx, &rows[idx]))
    }

    /// Store the next passing `MinimalTuple` into `slot`.
    ///
    /// This helper mirrors the common PostgreSQL executor drain operation while
    /// keeping the cursor and mask policy testable through `next_index`.
    ///
    /// # Safety
    ///
    /// Must run on the main PostgreSQL backend thread. `slot` must be a valid
    /// `TupleTableSlot`. Every non-null tuple in `tuples` that can pass `mask`
    /// must be a valid `MinimalTuple` whose lifetime outlives the slot's use.
    /// The slot is told it does not own the tuple, so the caller remains
    /// responsible for eventually freeing or bulk-resetting tuple storage.
    pub unsafe fn drain_minimal_tuple_to_slot(
        &mut self,
        tuples: &[pg_sys::MinimalTuple],
        mask: &[bool],
        slot: *mut pg_sys::TupleTableSlot,
    ) -> Option<*mut pg_sys::TupleTableSlot> {
        let idx = self.next_index(tuples, mask, |tuple| !tuple.is_null())?;

        unsafe {
            // SAFETY: `next_index` selected a non-null tuple whose storage
            // outlives the valid slot; ownership remains with the caller.
            pg_sys::ExecForceStoreMinimalTuple(tuples[idx], slot, false);
        }

        Some(slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_source_returns_none() {
        let mut drain = ResultDrain::new();
        let rows: [Option<i32>; 0] = [];
        let mask: [bool; 0] = [];

        assert_eq!(drain.next_index(&rows, &mask, Option::is_some), None);
        assert_eq!(drain.position(), 0);
        assert!(drain.is_exhausted(&rows));
    }

    #[test]
    fn skips_false_mask_entries() {
        let mut drain = ResultDrain::new();
        let rows = [Some(10), Some(20), Some(30)];
        let mask = [false, true, true];

        assert_eq!(drain.next_index(&rows, &mask, Option::is_some), Some(1));
        assert_eq!(drain.position(), 2);
        assert_eq!(drain.next_index(&rows, &mask, Option::is_some), Some(2));
        assert_eq!(drain.position(), 3);
        assert_eq!(drain.next_index(&rows, &mask, Option::is_some), None);
    }

    #[test]
    fn skips_rows_that_are_not_present() {
        let mut drain = ResultDrain::new();
        let rows = [None, Some(20), None, Some(40)];
        let mask = [true, true, true, true];

        assert_eq!(drain.next_index(&rows, &mask, Option::is_some), Some(1));
        assert_eq!(drain.next_index(&rows, &mask, Option::is_some), Some(3));
        assert_eq!(drain.next_index(&rows, &mask, Option::is_some), None);
    }

    #[test]
    fn missing_mask_entries_are_false() {
        let mut drain = ResultDrain::new();
        let rows = [Some(10), Some(20), Some(30)];
        let mask = [false, true];

        assert_eq!(drain.next_index(&rows, &mask, Option::is_some), Some(1));
        assert_eq!(drain.next_index(&rows, &mask, Option::is_some), None);
        assert_eq!(drain.position(), 3);
    }

    #[test]
    fn reset_rewinds_cursor() {
        let mut drain = ResultDrain::new();
        let rows = [Some(10), Some(20)];
        let mask = [true, true];

        assert_eq!(drain.next_index(&rows, &mask, Option::is_some), Some(0));
        drain.reset();

        assert_eq!(drain.next_index(&rows, &mask, Option::is_some), Some(0));
    }

    #[test]
    fn explicit_position_can_skip_prefix() {
        let mut drain = ResultDrain::new();
        let rows = [Some(10), Some(20), Some(30)];
        let mask = [true, true, true];

        drain.set_position(2);

        assert_eq!(drain.remaining(&rows), 1);
        assert_eq!(
            drain.next_item(&rows, &mask, Option::is_some),
            Some((2, &Some(30)))
        );
    }
}
