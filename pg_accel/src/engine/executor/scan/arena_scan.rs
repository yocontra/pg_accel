//! Heap scan mechanics — arena fill + per-row materialization.

use pgrx::pg_sys;

use crate::engine::gucs;

use super::ScanExecState;

impl ScanExecState {
    /// Pull tuples from the child plan (or direct heap scan) until the
    /// batch is full or the source is exhausted.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` must be valid
    /// when using child-plan mode (scan_desc is null). `scan_slot` must be
    /// valid when using direct-scan mode (scan_desc is non-null).
    pub(super) unsafe fn fill_batch(
        &mut self,
        child_ps: *mut pg_sys::PlanState,
        scan_slot: *mut pg_sys::TupleTableSlot,
    ) {
        // Free previously-buffered allocations.
        if self.batch_mcxt.is_null() {
            // Fallback for non-direct-scan paths: individual pfree.
            for &mt in &self.tuple_buffer {
                if !mt.is_null() {
                    // SAFETY: mt was palloc'd by ExecCopySlotMinimalTuple.
                    unsafe { pg_sys::pfree(mt.cast()) };
                }
            }
            // Free previously-copied datums (varlena from datumCopy).
            for &(datum, is_null) in &self.datum_buffer {
                if !is_null && datum.value() != 0 {
                    unsafe { pg_sys::pfree(datum.cast_mut_ptr()) };
                }
            }
        } else {
            // SAFETY: batch_mcxt is a valid PG memory context we own.
            // MemoryContextReset frees all allocations in O(1) by resetting
            // the context's block list, instead of O(n) individual pfree calls.
            unsafe { pg_sys::MemoryContextReset(self.batch_mcxt) };
        }
        self.clear_batch_buffers();

        let target = self.batch_size.max(gucs::min_batch_size().max(1) as usize);

        if self.scan_desc.is_null() {
            // Child plan scan (spatial/h3/raster).
            unsafe { self.fill_batch_child(child_ps, target) };
        } else {
            // Direct heap scan (GpuExpr with scanrelid > 0).
            // table_scan_getnextslot writes the heap tuple into scan_slot,
            // which has the table's full TupleDesc. MinimalTuples copied
            // from this slot match the TupleDesc used by extract_col_f64.
            unsafe { self.fill_batch_direct(scan_slot, target) };
        }
    }

    /// Direct heap scan path for fill_batch.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `scan_slot` and
    /// `self.scan_desc` must be valid.
    unsafe fn fill_batch_direct(&mut self, _scan_slot: *mut pg_sys::TupleTableSlot, target: usize) {
        // Switch to batch memory context for all MinimalTuple allocations.
        // SAFETY: batch_mcxt was created in set_scan_desc and is valid.
        let old_mcxt = if self.batch_mcxt.is_null() {
            std::ptr::null_mut()
        } else {
            unsafe { pg_sys::MemoryContextSwitchTo(self.batch_mcxt) }
        };

        while self.tuple_buffer.len() < target {
            // SAFETY: scan_desc is valid; main backend thread.
            let htup = unsafe {
                pg_sys::heap_getnext(self.scan_desc, pg_sys::ScanDirection::ForwardScanDirection)
            };
            if htup.is_null() {
                self.child_exhausted = true;
                break;
            }

            // Convert HeapTuple → MinimalTuple directly without slot
            // deformation. This is much faster than ExecForceStoreHeapTuple
            // (which deforms all columns) + ExecCopySlotMinimalTuple (which
            // repackages the deformed datums). The direct conversion just
            // copies raw tuple bytes with a header adjustment.
            // SAFETY: htup is a valid HeapTuple from heap_getnext.
            // Allocation goes into batch_mcxt (if set), freed in bulk on next cycle.
            let mt = unsafe { pg_sys::minimal_tuple_from_heap_tuple(htup) };
            self.tuple_buffer.push(mt);
        }

        // Restore previous memory context.
        if !old_mcxt.is_null() {
            // SAFETY: old_mcxt is the context we saved above.
            unsafe { pg_sys::MemoryContextSwitchTo(old_mcxt) };
        }
    }

    /// Child plan scan path for fill_batch.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` must be valid.
    unsafe fn fill_batch_child(&mut self, child_ps: *mut pg_sys::PlanState, target: usize) {
        let extract_attno = self.target_attno;

        while self.tuple_buffer.len() < target {
            // SAFETY: ExecProcNode is the standard PG API for pulling a
            // tuple from a plan node. We are on the main backend thread.
            let child_slot = unsafe { pg_sys::ExecProcNode(child_ps) };

            if child_slot.is_null() {
                self.child_exhausted = true;
                break;
            }

            // SAFETY: child_slot is non-null. TTS_EMPTY checks whether the
            // slot has a valid tuple. In PG, an empty slot signals end of
            // scan.
            let is_empty = unsafe { (*child_slot).tts_flags & pg_sys::TTS_FLAG_EMPTY as u16 != 0 };
            if is_empty {
                self.child_exhausted = true;
                break;
            }

            // Extract target column datum from the child's slot NOW, before
            // the child reuses the slot on the next ExecProcNode call.
            if extract_attno > 0 {
                // Check that the child slot has enough attributes.
                let child_desc = unsafe { (*child_slot).tts_tupleDescriptor };
                let child_natts = if child_desc.is_null() {
                    0
                } else {
                    unsafe { (*child_desc).natts }
                };
                if extract_attno <= child_natts {
                    let mut is_null = false;
                    // SAFETY: child_slot is valid and non-empty; slot_getattr
                    // deforms the tuple to extract the requested attribute.
                    let datum = unsafe {
                        pg_sys::slot_getattr(
                            child_slot,
                            extract_attno,
                            std::ptr::addr_of_mut!(is_null),
                        )
                    };
                    if !is_null && datum.value() != 0 {
                        // Deep copy varlena datum — the child reuses its slot.
                        // SAFETY: pg_detoast_datum_copy returns a palloc'd copy.
                        let varlena_ptr = datum.cast_mut_ptr::<pg_sys::varlena>();
                        let copied = unsafe { pg_sys::pg_detoast_datum_copy(varlena_ptr) };
                        self.datum_buffer.push((pg_sys::Datum::from(copied), false));
                    } else {
                        self.datum_buffer.push((pg_sys::Datum::from(0), true));
                    }
                } else {
                    if self.tuple_buffer.is_empty() {
                        tracing::debug!(
                            "pg_accel: fill_batch: attno={} > child_natts={}, skipping",
                            extract_attno,
                            child_natts,
                        );
                    }
                    self.datum_buffer.push((pg_sys::Datum::from(0), true));
                }
            }

            // Copy the tuple into our own storage for returning to parent.
            // SAFETY: child_slot is valid and non-empty.
            let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(child_slot) };

            self.tuple_buffer.push(mt);
        }
    }
}
