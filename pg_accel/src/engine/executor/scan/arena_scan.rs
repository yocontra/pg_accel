//! Heap scan mechanics — arena fill + per-row materialization.

use pgrx::pg_sys;

use crate::engine::columnar::ColumnarBatchOwner;
use crate::engine::dispatch::{self, DispatchResult};
use crate::engine::expr_compiler::{self, CompiledExpr, TemplateKernel};
use crate::engine::gucs;
use crate::engine::materialize::tuple_extract::{self, AttExtractInfo};
use crate::engine::registry::AccelStrategy;
use crate::engine::stats;
use crate::gpu::{self, PgaccelExprProgram};

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
        self.tuple_buffer.clear();
        self.datum_buffer.clear();
        self.result_mask.clear();
        self.result_pos = 0;

        let target = self.batch_size.max(gucs::min_batch_size().max(1) as usize);

        if self.scan_desc.is_null() {
            // Child plan scan (spatial/h3/raster or legacy).
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

    /// Deferred materialization: scan heap, extract filter columns directly
    /// from HeapTuple headers, run GPU template filter, then only create
    /// MinimalTuples for rows that pass. This avoids creating MinimalTuples
    /// for non-passing rows, reducing palloc overhead proportional to
    /// filter selectivity.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `self.scan_desc` and
    /// `scan_slot` must be valid. `self.compiled_expr` must be `Some(Template(_))`.
    #[allow(dead_code)]
    unsafe fn fill_and_filter_direct(&mut self, scan_slot: *mut pg_sys::TupleTableSlot) {
        // Reset batch state (no pfree needed — we haven't created MTs yet
        // for non-passing rows, and passing MTs are in batch_mcxt).
        if !self.batch_mcxt.is_null() {
            // SAFETY: batch_mcxt is valid.
            unsafe { pg_sys::MemoryContextReset(self.batch_mcxt) };
        }
        self.tuple_buffer.clear();
        self.datum_buffer.clear();
        self.result_mask.clear();
        self.result_pos = 0;

        let target = self.batch_size.max(gucs::min_batch_size().max(1) as usize);

        // Get the template kernel from compiled_expr.
        let kernel = match &self.compiled_expr {
            Some(CompiledExpr::Template(k)) => k.clone(),
            _ => return,
        };

        let tupdesc = unsafe { (*scan_slot).tts_tupleDescriptor };

        // Phase 1: Scan heap tuples. For each, extract filter column f64
        // directly from the HeapTuple header and store raw tuple data in
        // a flat arena buffer.
        let mut arena: Vec<u8> = Vec::with_capacity(target * 64);
        // (offset_in_arena, t_len) for each scanned tuple.
        let mut entries: Vec<(usize, u32)> = Vec::with_capacity(target);
        // HeapTupleHeader pointers (into arena) for column extraction.
        let mut headers: Vec<pg_sys::HeapTupleHeader> = Vec::with_capacity(target);

        let mut count = 0usize;
        while count < target {
            // SAFETY: scan_desc is valid; main backend thread.
            let htup = unsafe {
                pg_sys::heap_getnext(self.scan_desc, pg_sys::ScanDirection::ForwardScanDirection)
            };
            if htup.is_null() {
                self.child_exhausted = true;
                break;
            }

            // SAFETY: htup is valid from heap_getnext.
            let ht = unsafe { &*htup };
            let t_data = ht.t_data;
            let t_len = ht.t_len;

            // Copy the tuple's t_data bytes into our arena.
            let offset = arena.len();
            let data_bytes =
                unsafe { std::slice::from_raw_parts(t_data.cast::<u8>(), t_len as usize) };
            arena.extend_from_slice(data_bytes);
            entries.push((offset, t_len));

            // SAFETY: The arena bytes are a copy of valid HeapTupleHeader data.
            let hdr_ptr = arena[offset..].as_ptr() as pg_sys::HeapTupleHeader;
            headers.push(hdr_ptr);

            count += 1;
        }

        if count == 0 {
            return;
        }

        // Re-derive header pointers after arena is finalized (no more
        // reallocations). The pointers stored during push may be invalid
        // if the arena Vec reallocated.
        headers.clear();
        for &(offset, _) in &entries {
            let hdr_ptr = arena[offset..].as_ptr() as pg_sys::HeapTupleHeader;
            headers.push(hdr_ptr);
        }

        // Phase 2: Extract filter columns and run GPU template kernel.
        let gpu_results = self.eval_template_on_headers(&kernel, tupdesc, &headers, count);

        // Phase 3: Create MinimalTuples only for passing rows.
        // Switch to batch_mcxt for allocations.
        let old_mcxt = if self.batch_mcxt.is_null() {
            std::ptr::null_mut()
        } else {
            unsafe { pg_sys::MemoryContextSwitchTo(self.batch_mcxt) }
        };

        let start_us = std::time::Instant::now();

        if let Some(results) = gpu_results {
            let mut pass_count = 0u64;
            let mut recheck_count = 0u64;

            for i in 0..count {
                let r = results.get(i).copied().unwrap_or(-1);
                match r {
                    1 => {
                        // Definite TRUE — materialize this row.
                        let (offset, t_len) = entries[i];
                        let mt = unsafe { self.materialize_from_arena(&arena, offset, t_len) };
                        self.tuple_buffer.push(mt);
                        self.result_mask.push(true);
                        pass_count += 1;
                    }
                    r if r < 0 => {
                        // Definite FALSE — skip, no MT created.
                    }
                    _ => {
                        // Uncertain — materialize and CPU recheck.
                        recheck_count += 1;
                        let (offset, t_len) = entries[i];
                        let mt = unsafe { self.materialize_from_arena(&arena, offset, t_len) };
                        let passed = unsafe { self.cpu_recheck_tuple(mt, scan_slot) };
                        if passed {
                            self.tuple_buffer.push(mt);
                            self.result_mask.push(true);
                            pass_count += 1;
                        } else if !self.batch_mcxt.is_null() {
                            // batch_mcxt will bulk-free; no individual pfree needed.
                        } else {
                            // SAFETY: mt was palloc'd.
                            unsafe { pg_sys::pfree(mt.cast()) };
                        }
                    }
                }
            }
            tracing::debug!(
                "pg_accel: deferred GpuExpr {}/{} passed ({} rechecked)",
                pass_count,
                count,
                recheck_count,
            );
            self.rows_dispatched += count as u64;
            self.batches_executed += 1;

            // Per-backend stats: record this inline GpuExpr batch completion.
            // record_batch is called below after dispatch_time_us is updated,
            // alongside a GPU-row record (all `count` rows were pushed to the
            // template kernel).
            let elapsed_us = start_us.elapsed().as_micros() as u64;
            stats::record_batch(count as u64, elapsed_us);
            stats::record_gpu_batch(count as u64, recheck_count);
        } else {
            // GPU unavailable — materialize all and use scalar qual.
            // This is a real fallback from a GPU path into PG's expression
            // evaluator; record it for diagnostics.
            stats::record_fallback();
            for i in 0..count {
                let (offset, t_len) = entries[i];
                let mt = unsafe { self.materialize_from_arena(&arena, offset, t_len) };
                self.tuple_buffer.push(mt);
            }
            // Restore mcxt before scalar qual (which may allocate).
            if !old_mcxt.is_null() {
                unsafe { pg_sys::MemoryContextSwitchTo(old_mcxt) };
            }
            unsafe { self.dispatch_scalar_qual(scan_slot, count) };
            return;
        }

        self.dispatch_time_us += start_us.elapsed().as_micros() as u64;

        // Restore previous memory context.
        if !old_mcxt.is_null() {
            unsafe { pg_sys::MemoryContextSwitchTo(old_mcxt) };
        }
    }

    /// Create a MinimalTuple from arena data (raw HeapTupleHeader bytes).
    ///
    /// # Safety
    ///
    /// Arena must contain valid HeapTuple header + data at the given offset.
    /// Must be called in the appropriate memory context.
    #[inline]
    #[allow(dead_code)]
    unsafe fn materialize_from_arena(
        &self,
        arena: &[u8],
        offset: usize,
        t_len: u32,
    ) -> pg_sys::MinimalTuple {
        // Build a temporary HeapTupleData on the stack pointing into arena.
        // SAFETY: arena[offset..offset+t_len] contains valid HeapTupleHeader bytes.
        let t_data = arena[offset..].as_ptr() as pg_sys::HeapTupleHeader;
        let mut ht_data = pg_sys::HeapTupleData {
            t_len,
            t_self: pg_sys::ItemPointerData::default(),
            t_tableOid: pg_sys::InvalidOid,
            t_data,
        };
        // SAFETY: ht_data points to valid HeapTuple header data in the arena.
        unsafe { pg_sys::minimal_tuple_from_heap_tuple(&raw mut ht_data) }
    }

    /// CPU-recheck a single tuple via the scalar qual expression.
    ///
    /// # Safety
    ///
    /// `mt` must be valid. `scan_slot`, `self.qual`, `self.econtext` must be valid.
    /// Must be on main backend thread.
    #[inline]
    #[allow(dead_code)]
    unsafe fn cpu_recheck_tuple(
        &self,
        mt: pg_sys::MinimalTuple,
        scan_slot: *mut pg_sys::TupleTableSlot,
    ) -> bool {
        if self.qual.is_null() || self.econtext.is_null() {
            return true;
        }
        // SAFETY: mt, scan_slot, qual, econtext are valid; main thread.
        unsafe {
            pg_sys::ExecForceStoreMinimalTuple(mt, scan_slot, false);
            (*self.econtext).ecxt_scantuple = scan_slot;
        }
        let mut is_null = false;
        let result = unsafe {
            pg_sys::ExecEvalExpr(self.qual, self.econtext, std::ptr::addr_of_mut!(is_null))
        };
        let passed = !is_null && result.value() != 0;
        // SAFETY: Reset per-tuple memory to prevent leaks.
        unsafe {
            pg_sys::MemoryContextReset((*self.econtext).ecxt_per_tuple_memory);
        }
        passed
    }

    /// Evaluate a template kernel on HeapTuple headers (for deferred materialization).
    #[allow(dead_code)]
    fn eval_template_on_headers(
        &self,
        kernel: &TemplateKernel,
        tupdesc: pg_sys::TupleDesc,
        headers: &[pg_sys::HeapTupleHeader],
        batch_len: usize,
    ) -> Option<Vec<i8>> {
        match kernel {
            TemplateKernel::CmpConst {
                col_idx,
                cmp_opcode,
                const_val,
            } => {
                let info = unsafe { AttExtractInfo::new(tupdesc, (*col_idx + 1) as i32) };
                let (values, nulls) =
                    unsafe { tuple_extract::extract_f64_from_heap_headers(headers, &info) };
                let mut batch_owner = ColumnarBatchOwner::new(batch_len, 1);
                batch_owner.add_col_f64(values, nulls);
                let batch = batch_owner.as_batch();
                gpu::expr_template_cmp_const(&batch, 0, *cmp_opcode, *const_val, batch_len)
            }
            TemplateKernel::TwoPredAnd {
                col1_idx,
                cmp1_opcode,
                const1_val,
                col2_idx,
                cmp2_opcode,
                const2_val,
            } => {
                let info1 = unsafe { AttExtractInfo::new(tupdesc, (*col1_idx + 1) as i32) };
                let info2 = unsafe { AttExtractInfo::new(tupdesc, (*col2_idx + 1) as i32) };
                let r1 = {
                    let (values, nulls) =
                        unsafe { tuple_extract::extract_f64_from_heap_headers(headers, &info1) };
                    let mut bo = ColumnarBatchOwner::new(batch_len, 1);
                    bo.add_col_f64(values, nulls);
                    let batch = bo.as_batch();
                    gpu::expr_template_cmp_const(&batch, 0, *cmp1_opcode, *const1_val, batch_len)
                };
                let r2 = {
                    let (values, nulls) =
                        unsafe { tuple_extract::extract_f64_from_heap_headers(headers, &info2) };
                    let mut bo = ColumnarBatchOwner::new(batch_len, 1);
                    bo.add_col_f64(values, nulls);
                    let batch = bo.as_batch();
                    gpu::expr_template_cmp_const(&batch, 0, *cmp2_opcode, *const2_val, batch_len)
                };
                match (r1, r2) {
                    (Some(a), Some(b)) => {
                        let combined: Vec<i8> = a
                            .iter()
                            .zip(b.iter())
                            .map(|(&x, &y)| {
                                if x < 0 || y < 0 {
                                    -1
                                } else {
                                    i8::from(x > 0 && y > 0)
                                }
                            })
                            .collect();
                        Some(combined)
                    }
                    _ => None,
                }
            }
            // Other template variants not yet supported in deferred path.
            _ => None,
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
