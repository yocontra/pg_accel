//! Vectorized heap scan for pg_accel executor nodes.
//!
//! [`SortScan`] performs a direct heap scan via `table_scan_getnextslot`,
//! materializing all tuples into an arena of `MinimalTuple` copies. Sort key
//! columns are extracted inline during the scan to avoid a second pass.
//!
//! This replaces the `ExecProcNode`-per-tuple pattern with a bulk scan that
//! eliminates Custom Scan yield overhead (~3us/row).
//!
//! # Usage
//!
//! 1. Create with `SortScan::new(scan_desc, slot)`.
//! 2. Call `scan_all()` to materialize the entire relation.
//! 3. Access `arena()` for the collected `MinimalTuple` pointers.
//! 4. Access `keys_f32()` / `keys_f64()` for inline-extracted sort keys.
//! 5. Call `materialize(idx, slot)` to restore a specific tuple into a slot.

use pgrx::pg_sys;

use crate::engine::cost;

/// PG type OIDs for GPU-sortable numeric types.
const INT4OID: u32 = 23;
const INT8OID: u32 = 20;
const FLOAT4OID: u32 = 700;
const FLOAT8OID: u32 = 701;

/// Vectorized heap scanner that materializes an entire relation into memory.
///
/// Designed for bulk-consumption patterns (sort, window, reduce) where all
/// rows must be read before processing begins.
pub struct SortScan {
    /// Table scan descriptor from `table_beginscan`.
    scan_desc: pg_sys::TableScanDesc,

    /// Dedicated scan slot with TupleDesc matching the scanned relation.
    /// Created from the relation's TupleDesc so `table_scan_getnextslot`
    /// works correctly even when the Custom Scan result slot differs.
    scan_slot: *mut pg_sys::TupleTableSlot,

    /// Arena of materialized tuples. Each entry is a palloc'd MinimalTuple.
    arena: Vec<pg_sys::MinimalTuple>,

    /// Inline-extracted f32 sort keys (one per non-null tuple).
    keys_f32: Vec<f32>,

    /// Inline-extracted f64 sort keys (one per non-null tuple).
    keys_f64: Vec<f64>,

    /// Inline-extracted i32 sort keys (one per non-null tuple).
    keys_i32: Vec<i32>,

    /// Inline-extracted i64 sort keys (one per non-null tuple).
    keys_i64: Vec<i64>,

    /// Indices of tuples with non-null sort keys (into `arena`).
    non_null_indices: Vec<usize>,

    /// Indices of tuples with null sort keys (into `arena`).
    null_indices: Vec<usize>,

    /// Type OID of the inline-extracted sort key column (0 = no extraction).
    key_typid: u32,

    /// 1-based attribute number of the sort key column.
    key_attno: i32,
}

impl SortScan {
    /// Create a new vectorized scanner.
    ///
    /// `scan_desc` must be a valid `TableScanDesc` from `table_beginscan`.
    /// `rel` must be the opened relation (for TupleDesc to create scan slot).
    /// `key_attno` is the 1-based attribute for inline key extraction (0 to skip).
    /// `key_typid` is the PG type OID of the key column.
    ///
    /// # Safety
    ///
    /// `rel` must be a valid, open `Relation`. Must be called on the main
    /// backend thread.
    #[must_use]
    pub unsafe fn new(
        scan_desc: pg_sys::TableScanDesc,
        rel: pg_sys::Relation,
        key_attno: i32,
        key_typid: u32,
    ) -> Self {
        // SAFETY: rel is a valid Relation; rd_att is its TupleDesc.
        // MakeSingleTupleTableSlot creates a slot compatible with
        // table_scan_getnextslot.
        let tupdesc = unsafe { (*rel).rd_att };
        let scan_slot = unsafe {
            pg_sys::MakeSingleTupleTableSlot(tupdesc, &raw const pg_sys::TTSOpsBufferHeapTuple)
        };
        Self {
            scan_desc,
            scan_slot,
            arena: Vec::new(),
            keys_f32: Vec::new(),
            keys_f64: Vec::new(),
            keys_i32: Vec::new(),
            keys_i64: Vec::new(),
            non_null_indices: Vec::new(),
            null_indices: Vec::new(),
            key_typid,
            key_attno,
        }
    }

    /// Scan the entire relation, materializing all tuples into the arena.
    ///
    /// If `key_attno > 0` and `key_typid` is a GPU-sortable type, extracts
    /// the sort key column inline during the scan.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `scan_desc` must be valid.
    pub unsafe fn scan_all(&mut self) -> usize {
        let scan_slot = self.scan_slot;
        let do_inline = self.key_attno > 0
            && matches!(self.key_typid, FLOAT4OID | FLOAT8OID | INT4OID | INT8OID);

        // Pre-allocate with a reasonable estimate.
        let est = 4096;
        self.arena.reserve(est);
        if do_inline {
            match self.key_typid {
                FLOAT4OID => self.keys_f32.reserve(est),
                FLOAT8OID => self.keys_f64.reserve(est),
                INT4OID => self.keys_i32.reserve(est),
                INT8OID => self.keys_i64.reserve(est),
                _ => {}
            }
            self.non_null_indices.reserve(est);
        }

        let batch_size = cost::device_limits().gpu_sort_min_rows.max(8192);

        loop {
            let mut batch_count = 0usize;
            for _ in 0..batch_size {
                // SAFETY: scan_desc and scan_slot are valid; main backend thread.
                let got_tuple = unsafe {
                    pg_sys::table_scan_getnextslot(
                        self.scan_desc,
                        pg_sys::ScanDirection::ForwardScanDirection,
                        scan_slot,
                    )
                };
                if !got_tuple {
                    break;
                }

                // Extract sort key from the live slot before copying.
                if do_inline {
                    let idx = self.arena.len();
                    let mut is_null = false;
                    // SAFETY: scan_slot is valid and non-empty.
                    let datum = unsafe {
                        pg_sys::slot_getattr(scan_slot, self.key_attno, &raw mut is_null)
                    };
                    if is_null {
                        self.null_indices.push(idx);
                    } else {
                        self.non_null_indices.push(idx);
                        match self.key_typid {
                            FLOAT4OID => {
                                self.keys_f32.push(f32::from_bits(datum.value() as u32));
                            }
                            FLOAT8OID => {
                                self.keys_f64.push(f64::from_bits(datum.value() as u64));
                            }
                            INT4OID => {
                                self.keys_i32.push(datum.value() as i32);
                            }
                            INT8OID => {
                                self.keys_i64.push(datum.value() as i64);
                            }
                            _ => {} // unreachable due to do_inline guard
                        }
                    }
                }

                // SAFETY: scan_slot is valid and non-empty.
                let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(scan_slot) };
                self.arena.push(mt);
                batch_count += 1;
            }

            pgrx::check_for_interrupts!();

            if batch_count == 0 {
                break;
            }
        }

        self.arena.len()
    }

    /// Access the materialized tuple arena.
    #[must_use]
    pub fn arena(&self) -> &[pg_sys::MinimalTuple] {
        &self.arena
    }

    /// Take ownership of the arena, leaving it empty.
    pub fn take_arena(&mut self) -> Vec<pg_sys::MinimalTuple> {
        std::mem::take(&mut self.arena)
    }

    /// Access inline-extracted f32 keys (only valid after `scan_all`
    /// when key type is FLOAT4).
    #[must_use]
    pub fn keys_f32(&self) -> &[f32] {
        &self.keys_f32
    }

    /// Take ownership of the f32 keys.
    pub fn take_keys_f32(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.keys_f32)
    }

    /// Access inline-extracted f64 keys (only valid after `scan_all`
    /// when key type is FLOAT8/INT4/INT8).
    #[must_use]
    pub fn keys_f64(&self) -> &[f64] {
        &self.keys_f64
    }

    /// Take ownership of the f64 keys.
    pub fn take_keys_f64(&mut self) -> Vec<f64> {
        std::mem::take(&mut self.keys_f64)
    }

    /// Access inline-extracted i32 keys (only valid after `scan_all`
    /// when key type is INT4).
    #[must_use]
    pub fn keys_i32(&self) -> &[i32] {
        &self.keys_i32
    }

    /// Take ownership of the i32 keys.
    pub fn take_keys_i32(&mut self) -> Vec<i32> {
        std::mem::take(&mut self.keys_i32)
    }

    /// Access inline-extracted i64 keys (only valid after `scan_all`
    /// when key type is INT8).
    #[must_use]
    pub fn keys_i64(&self) -> &[i64] {
        &self.keys_i64
    }

    /// Take ownership of the i64 keys.
    pub fn take_keys_i64(&mut self) -> Vec<i64> {
        std::mem::take(&mut self.keys_i64)
    }

    /// Access non-null key indices.
    #[must_use]
    pub fn non_null_indices(&self) -> &[usize] {
        &self.non_null_indices
    }

    /// Take ownership of non-null indices.
    pub fn take_non_null_indices(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.non_null_indices)
    }

    /// Access null key indices.
    #[must_use]
    pub fn null_indices(&self) -> &[usize] {
        &self.null_indices
    }

    /// Take ownership of null indices.
    pub fn take_null_indices(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.null_indices)
    }

    /// The key type OID being extracted.
    #[must_use]
    pub fn key_typid(&self) -> u32 {
        self.key_typid
    }

    /// Restore a specific tuple from the arena into the given slot.
    ///
    /// # Safety
    ///
    /// `idx` must be in bounds. `slot` must be a valid `TupleTableSlot`.
    /// Must be called on the main backend thread.
    pub unsafe fn materialize(
        &self,
        idx: usize,
        slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let mt = self.arena[idx];
        if mt.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: mt is a valid MinimalTuple. ExecForceStoreMinimalTuple
        // stores it into the slot. `false` = slot does not own the tuple.
        unsafe {
            pg_sys::ExecForceStoreMinimalTuple(mt, slot, false);
        }
        slot
    }

    /// Total number of materialized tuples.
    #[must_use]
    pub fn len(&self) -> usize {
        self.arena.len()
    }

    /// Whether the arena is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.arena.is_empty()
    }

    /// The scan descriptor.
    #[must_use]
    pub fn scan_desc(&self) -> pg_sys::TableScanDesc {
        self.scan_desc
    }

    /// The dedicated scan slot (has base relation's TupleDesc).
    #[must_use]
    pub fn scan_slot(&self) -> *mut pg_sys::TupleTableSlot {
        self.scan_slot
    }
}

impl Drop for SortScan {
    fn drop(&mut self) {
        // Free all materialized MinimalTuples.
        for mt in &self.arena {
            if !mt.is_null() {
                // SAFETY: MinimalTuples were palloc'd by
                // ExecCopySlotMinimalTuple.
                unsafe {
                    pg_sys::pfree((*mt).cast());
                }
            }
        }
        // Free the dedicated scan slot.
        if !self.scan_slot.is_null() {
            // SAFETY: scan_slot was created by MakeSingleTupleTableSlot.
            unsafe {
                pg_sys::ExecDropSingleTupleTableSlot(self.scan_slot);
            }
        }
    }
}
