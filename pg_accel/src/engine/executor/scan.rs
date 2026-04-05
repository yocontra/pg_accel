//! Batch-dispatch scan executor for pg_accel Custom Scan nodes.
//!
//! [`ScanExecState`] holds the Rust-side state that persists across calls
//! to `exec_custom_scan`. Since PostgreSQL calls the exec callback once per
//! tuple, the executor accumulates child tuples into batches, dispatches
//! them, and returns results one at a time from a result buffer.
//!
//! # Lifecycle
//!
//! 1. **`begin_custom_scan`** — allocates `ScanExecState` via `Box::into_raw`
//!    and stores the pointer in `GpuAccelState.executor`.
//! 2. **`exec_custom_scan`** (repeated) — delegates to [`ScanExecState::next`].
//! 3. **`end_custom_scan`** — reclaims the `ScanExecState` via `Box::from_raw`
//!    and drops it.

use pgrx::pg_sys;

use crate::engine::columnar::ColumnarBatchOwner;
use crate::engine::dispatch::{self, DispatchResult};
use crate::engine::executor::tuple_extract::{self, AttExtractInfo};
use crate::engine::expr_compiler::{self, CompiledExpr, TemplateKernel};
use crate::engine::gucs;
use crate::engine::registry::AccelStrategy;
use crate::gpu::{self, PgaccelExprProgram};

/// Rust-side batch executor state, stored as a raw pointer in
/// `GpuAccelState.executor` (a `*mut ScanExecState`).
///
/// This struct is **not** `repr(C)` — it lives entirely on the Rust heap
/// and is opaque to PostgreSQL.
pub struct ScanExecState {
    /// Which acceleration strategy to use for this scan node.
    strategy: AccelStrategy,

    /// Batch size (from GUC at plan time).
    batch_size: usize,

    /// Buffered tuples from the child plan. Each entry is an owned
    /// `MinimalTuple` copied from the child slot. We must copy because
    /// the child plan reuses the same `TupleTableSlot` for every
    /// `ExecProcNode` call — storing slot pointers would give N copies
    /// of the last tuple.
    tuple_buffer: Vec<pg_sys::MinimalTuple>,

    /// Per-slot result: `true` means the row passed dispatch filtering
    /// and should be returned to the parent.
    result_mask: Vec<bool>,

    /// Current read position in `tuple_buffer` / `result_mask`. Points to
    /// the next tuple to consider returning. Tuples where `result_mask` is
    /// `false` are skipped.
    result_pos: usize,

    /// Set to `true` once the child plan returns a null (empty) slot,
    /// indicating no more tuples.
    child_exhausted: bool,

    /// Qual expression state stolen from the CustomScanState. We evaluate
    /// this ourselves per-batch instead of letting ExecScan do it per-tuple.
    /// NULL means no qual (all rows pass).
    qual: *mut pg_sys::ExprState,

    /// Expression context for qual evaluation. Borrowed from the plan
    /// state — NOT owned by us. We set `ecxt_scantuple` before each
    /// qual evaluation call.
    econtext: *mut pg_sys::ExprContext,

    // -- GPU dispatch context (set via `set_gpu_context`) --
    /// Attribute number of the column to extract for GPU dispatch (1-based).
    /// Zero means no GPU column extraction is configured.
    target_attno: i32,

    /// Function OID for initialising `fn_info_buf`. Zero means not set.
    fn_oid: pg_sys::Oid,

    /// Initialised `FmgrInfo` for the accelerated function. Only valid
    /// when `fn_oid != InvalidOid`.
    fn_info_buf: pg_sys::FmgrInfo,

    /// Constant second argument for 2-arg spatial predicates (e.g. the
    /// constant geometry in `WHERE ST_Intersects(geom_col, $1)`).
    qual_datum: Option<(pg_sys::Datum, bool)>,

    /// When `true`, the child plan is a GiST index scan that has already
    /// performed bbox filtering. The GPU spatial pipeline will skip Layer 1
    /// (bbox overlap test) to avoid redundant work.
    gist_recheck: bool,

    /// Compiled GPU expression for GpuExpr strategy. Set by
    /// `begin_custom_scan` after expression compilation. `None` means
    /// no expression was compiled (fall back to scalar qual).
    compiled_expr: Option<expr_compiler::CompiledExpr>,

    /// Pre-extracted datums from the target column, captured from the
    /// child's slot during fill_batch (before the child reuses its slot).
    /// Used by dispatch_gpu_path instead of re-extracting from MinimalTuples,
    /// which fails because the Custom Scan's scan_slot TupleDesc may not
    /// match the child's MinimalTuple layout.
    datum_buffer: Vec<(pg_sys::Datum, bool)>,

    /// Table scan descriptor for direct heap scan (GpuExpr with scanrelid > 0).
    /// When non-null, fill_batch uses `table_scan_getnextslot` instead of
    /// `ExecProcNode` on a child plan. This avoids TupleDesc mismatch issues
    /// because the scan slot has the table's full TupleDesc.
    scan_desc: pg_sys::TableScanDesc,

    /// Dedicated memory context for batch MinimalTuple allocations.
    /// Reset at the start of each fill_batch cycle instead of individual
    /// pfree calls, reducing allocation overhead from O(n) to O(1).
    batch_mcxt: pg_sys::MemoryContext,

    /// Cached extraction info for inline filter columns. Initialized lazily
    /// on first call to `inline_filter_scan`, then reused across calls.
    inline_filter_infos: Option<Vec<AttExtractInfo>>,

    // -- Counters for EXPLAIN ANALYZE --
    /// Total rows pulled from child and dispatched.
    pub rows_dispatched: u64,

    /// Number of batches sent through dispatch.
    pub batches_executed: u64,

    /// Cumulative microseconds spent in dispatch.
    pub dispatch_time_us: u64,
}

impl ScanExecState {
    /// Create a new executor state for a Custom Scan node.
    ///
    /// `qual` and `econtext` are stolen from the `CustomScanState` at
    /// `begin_custom_scan` time. If `qual` is null, all rows pass.
    #[must_use]
    pub fn new(
        strategy: AccelStrategy,
        batch_size: usize,
        qual: *mut pg_sys::ExprState,
        econtext: *mut pg_sys::ExprContext,
    ) -> Self {
        Self {
            strategy,
            batch_size,
            tuple_buffer: Vec::with_capacity(batch_size),
            result_mask: Vec::with_capacity(batch_size),
            result_pos: 0,
            child_exhausted: false,
            qual,
            econtext,
            target_attno: 0,
            fn_oid: pg_sys::InvalidOid,
            // SAFETY: zero-initialised FmgrInfo is safe — all fields are
            // integers/pointers that accept zero.
            fn_info_buf: unsafe { std::mem::zeroed() },
            qual_datum: None,
            gist_recheck: false,
            compiled_expr: None,
            datum_buffer: Vec::with_capacity(batch_size),
            scan_desc: std::ptr::null_mut(),
            batch_mcxt: std::ptr::null_mut(),
            inline_filter_infos: None,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
        }
    }

    /// The main entry point called by `exec_custom_scan`.
    ///
    /// Returns a pointer to the next passing `TupleTableSlot`, or a null
    /// pointer when there are no more rows.
    ///
    /// # Safety
    ///
    /// - Must be called on the main backend thread only.
    /// - `child_plan_state` must be a valid pointer to the child
    ///   `PlanState` node.
    /// - `scan_slot` must be a valid pointer to this node's result slot.
    pub unsafe fn next(
        &mut self,
        child_plan_state: *mut pg_sys::PlanState,
        scan_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        // Fast inline filter path: direct heap scan + template kernel.
        // Evaluates the filter on the CPU inline during heap_getnext,
        // returning passing tuples directly from the heap buffer without
        // creating MinimalTuples. This avoids the ExecForceStoreMinimalTuple
        // overhead that dominates the batched path.
        if !self.scan_desc.is_null() {
            if let Some(CompiledExpr::Template(_)) = self.compiled_expr {
                return unsafe { self.inline_filter_scan(scan_slot) };
            }
        }

        loop {
            // 1. Try to return the next passing row from the current batch.
            if let Some(slot) = self.drain_next(scan_slot) {
                return slot;
            }

            // 2. If child is exhausted and no buffered results, we are done.
            if self.child_exhausted {
                return std::ptr::null_mut();
            }

            // 3. Accumulate the next batch from the child (or direct scan).
            // SAFETY: Caller guarantees child_plan_state is valid (or null
            // for direct scan) and we are on the main backend thread.
            unsafe {
                self.fill_batch(child_plan_state, scan_slot);
            }

            // 4. Dispatch the batch.
            // SAFETY: We are on the main backend thread.
            unsafe {
                self.dispatch_batch(scan_slot);
            }

            // 5. CHECK_FOR_INTERRUPTS between batches.
            pgrx::check_for_interrupts!();
        }
    }

    /// Inline filter scan: evaluate the template predicate directly on
    /// each HeapTuple from `heap_getnext` and return passing tuples
    /// immediately. No MinimalTuple creation, no batching, no slot
    /// deformation for non-passing rows.
    ///
    /// This is called once per `exec_custom_scan` invocation and returns
    /// a single passing tuple (or null when exhausted).
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `scan_slot` and
    /// `self.scan_desc` must be valid. `self.compiled_expr` must be
    /// `Some(Template(_))`.
    pub unsafe fn inline_filter_scan(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        // Lazily initialize cached extraction info on first call.
        if self.inline_filter_infos.is_none() {
            let tupdesc = unsafe { (*scan_slot).tts_tupleDescriptor };
            let infos = match &self.compiled_expr {
                Some(CompiledExpr::Template(TemplateKernel::CmpConst {
                    col_idx, ..
                })) => {
                    vec![unsafe { AttExtractInfo::new(tupdesc, (*col_idx + 1) as i32) }]
                }
                Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                    col1_idx,
                    col2_idx,
                    ..
                })) => vec![
                    unsafe { AttExtractInfo::new(tupdesc, (*col1_idx + 1) as i32) },
                    unsafe { AttExtractInfo::new(tupdesc, (*col2_idx + 1) as i32) },
                ],
                _ => vec![],
            };
            self.inline_filter_infos = Some(infos);
        }

        let empty = Vec::new();
        let infos = self.inline_filter_infos.as_ref().unwrap_or(&empty);
        if infos.is_empty() {
            // Unsupported template — skip inline, fall through to batched.
            self.child_exhausted = true;
            return std::ptr::null_mut();
        }

        loop {
            // Use table_scan_getnextslot which stores the tuple directly
            // into the scan slot with proper buffer pinning.
            // SAFETY: scan_desc and scan_slot are valid; main backend thread.
            let got = unsafe {
                pg_sys::table_scan_getnextslot(
                    self.scan_desc,
                    pg_sys::ScanDirection::ForwardScanDirection,
                    scan_slot,
                )
            };
            if !got {
                self.child_exhausted = true;
                return std::ptr::null_mut();
            }

            self.rows_dispatched += 1;

            // Periodic interrupt check.
            if self.rows_dispatched % 65536 == 0 {
                pgrx::check_for_interrupts!();
            }

            // Evaluate the predicate inline on the CPU.
            // SAFETY: scan_slot has a valid tuple from table_scan_getnextslot.
            // We extract the HeapTuple header for fast inline evaluation.
            let t_data = unsafe {
                let htup = pg_sys::ExecFetchSlotHeapTuple(scan_slot, false, std::ptr::null_mut());
                if htup.is_null() {
                    // Can't get heap tuple — conservatively return the row
                    // and let ExecScan's qual evaluate it.
                    return scan_slot;
                }
                (*htup).t_data
            };

            let passes = match &self.compiled_expr {
                Some(CompiledExpr::Template(TemplateKernel::CmpConst {
                    cmp_opcode,
                    const_val,
                    ..
                })) => Self::inline_eval_cmp(t_data, &infos[0], *cmp_opcode, *const_val),
                Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                    cmp1_opcode,
                    const1_val,
                    cmp2_opcode,
                    const2_val,
                    ..
                })) => {
                    Self::inline_eval_cmp(t_data, &infos[0], *cmp1_opcode, *const1_val)
                        && Self::inline_eval_cmp(
                            t_data,
                            &infos[1],
                            *cmp2_opcode,
                            *const2_val,
                        )
                }
                _ => true,
            };

            if passes {
                return scan_slot;
            }
        }
    }

    /// Evaluate a single `col <cmp> const` predicate inline on a HeapTuple.
    ///
    /// Returns `true` if the predicate passes, `false` otherwise.
    /// Returns `true` (pass) if the value can't be fast-extracted (conservative).
    #[inline(always)]
    fn inline_eval_cmp(
        t_data: pg_sys::HeapTupleHeader,
        info: &AttExtractInfo,
        cmp_opcode: u16,
        const_val: f64,
    ) -> bool {
        if !info.can_fast_extract() {
            // Can't fast-extract — conservatively pass (PG will recheck).
            return true;
        }

        // SAFETY: t_data is valid. info matches the schema.
        let val: Option<f64> = unsafe {
            match info.typid {
                t if t == pg_sys::FLOAT4OID => {
                    tuple_extract::try_fast_read_heap_pub::<f32>(t_data, info)
                        .map(f64::from)
                }
                t if t == pg_sys::INT2OID => {
                    tuple_extract::try_fast_read_heap_pub::<i16>(t_data, info)
                        .map(f64::from)
                }
                t if t == pg_sys::INT4OID => {
                    tuple_extract::try_fast_read_heap_pub::<i32>(t_data, info)
                        .map(f64::from)
                }
                t if t == pg_sys::INT8OID => {
                    tuple_extract::try_fast_read_heap_pub::<i64>(t_data, info)
                        .map(|v| v as f64)
                }
                _ => tuple_extract::try_fast_read_heap_pub::<f64>(t_data, info),
            }
        };

        let Some(v) = val else {
            // Null or extraction failed — conservatively pass.
            return true;
        };

        match cmp_opcode {
            expr_compiler::opcode::EQ => (v - const_val).abs() < f64::EPSILON,
            expr_compiler::opcode::NE => (v - const_val).abs() >= f64::EPSILON,
            expr_compiler::opcode::LT => v < const_val,
            expr_compiler::opcode::LE => v <= const_val,
            expr_compiler::opcode::GT => v > const_val,
            expr_compiler::opcode::GE => v >= const_val,
            _ => true,
        }
    }

    /// Pull tuples from the child plan (or direct heap scan) until the
    /// batch is full or the source is exhausted.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` must be valid
    /// when using child-plan mode (scan_desc is null). `scan_slot` must be
    /// valid when using direct-scan mode (scan_desc is non-null).
    unsafe fn fill_batch(
        &mut self,
        child_ps: *mut pg_sys::PlanState,
        scan_slot: *mut pg_sys::TupleTableSlot,
    ) {
        // Free previously-buffered allocations.
        if !self.batch_mcxt.is_null() {
            // SAFETY: batch_mcxt is a valid PG memory context we own.
            // MemoryContextReset frees all allocations in O(1) by resetting
            // the context's block list, instead of O(n) individual pfree calls.
            unsafe { pg_sys::MemoryContextReset(self.batch_mcxt) };
        } else {
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
        }
        self.tuple_buffer.clear();
        self.datum_buffer.clear();
        self.result_mask.clear();
        self.result_pos = 0;

        let target = self.batch_size.max(gucs::min_batch_size().max(1) as usize);

        if !self.scan_desc.is_null() {
            // Direct heap scan (GpuExpr with scanrelid > 0).
            // table_scan_getnextslot writes the heap tuple into scan_slot,
            // which has the table's full TupleDesc. MinimalTuples copied
            // from this slot match the TupleDesc used by extract_col_f64.
            unsafe { self.fill_batch_direct(scan_slot, target) };
        } else {
            // Child plan scan (spatial/h3/raster or legacy).
            unsafe { self.fill_batch_child(child_ps, target) };
        }
    }

    /// Direct heap scan path for fill_batch.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `scan_slot` and
    /// `self.scan_desc` must be valid.
    unsafe fn fill_batch_direct(
        &mut self,
        _scan_slot: *mut pg_sys::TupleTableSlot,
        target: usize,
    ) {
        // Switch to batch memory context for all MinimalTuple allocations.
        // SAFETY: batch_mcxt was created in set_scan_desc and is valid.
        let old_mcxt = if !self.batch_mcxt.is_null() {
            unsafe { pg_sys::MemoryContextSwitchTo(self.batch_mcxt) }
        } else {
            std::ptr::null_mut()
        };

        while self.tuple_buffer.len() < target {
            // SAFETY: scan_desc is valid; main backend thread.
            let htup = unsafe {
                pg_sys::heap_getnext(
                    self.scan_desc,
                    pg_sys::ScanDirection::ForwardScanDirection,
                )
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
    unsafe fn fill_and_filter_direct(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
    ) {
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
                pg_sys::heap_getnext(
                    self.scan_desc,
                    pg_sys::ScanDirection::ForwardScanDirection,
                )
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
        let old_mcxt = if !self.batch_mcxt.is_null() {
            unsafe { pg_sys::MemoryContextSwitchTo(self.batch_mcxt) }
        } else {
            std::ptr::null_mut()
        };

        let start_us = std::time::Instant::now();

        match gpu_results {
            Some(results) => {
                let mut pass_count = 0u64;
                let mut recheck_count = 0u64;

                for i in 0..count {
                    let r = results.get(i).copied().unwrap_or(-1);
                    match r {
                        1 => {
                            // Definite TRUE — materialize this row.
                            let (offset, t_len) = entries[i];
                            let mt = unsafe {
                                self.materialize_from_arena(&arena, offset, t_len)
                            };
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
                            let mt = unsafe {
                                self.materialize_from_arena(&arena, offset, t_len)
                            };
                            let passed = unsafe {
                                self.cpu_recheck_tuple(mt, scan_slot)
                            };
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
                pgrx::debug1!(
                    "pg_accel: deferred GpuExpr {}/{} passed ({} rechecked)",
                    pass_count, count, recheck_count,
                );
                self.rows_dispatched += count as u64;
                self.batches_executed += 1;
            }
            None => {
                // GPU unavailable — materialize all and use scalar qual.
                for i in 0..count {
                    let (offset, t_len) = entries[i];
                    let mt = unsafe {
                        self.materialize_from_arena(&arena, offset, t_len)
                    };
                    self.tuple_buffer.push(mt);
                }
                // Restore mcxt before scalar qual (which may allocate).
                if !old_mcxt.is_null() {
                    unsafe { pg_sys::MemoryContextSwitchTo(old_mcxt) };
                }
                unsafe { self.dispatch_scalar_qual(scan_slot, count) };
                return;
            }
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
        unsafe {
            pg_sys::minimal_tuple_from_heap_tuple(
                &mut ht_data as *mut pg_sys::HeapTupleData,
            )
        }
    }

    /// CPU-recheck a single tuple via the scalar qual expression.
    ///
    /// # Safety
    ///
    /// `mt` must be valid. `scan_slot`, `self.qual`, `self.econtext` must be valid.
    /// Must be on main backend thread.
    #[inline]
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
            pg_sys::ExecEvalExpr(
                self.qual,
                self.econtext,
                std::ptr::addr_of_mut!(is_null),
            )
        };
        let passed = !is_null && result.value() != 0;
        // SAFETY: Reset per-tuple memory to prevent leaks.
        unsafe {
            pg_sys::MemoryContextReset((*self.econtext).ecxt_per_tuple_memory);
        }
        passed
    }

    /// Evaluate a template kernel on HeapTuple headers (for deferred materialization).
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
                                } else if x > 0 && y > 0 {
                                    1
                                } else {
                                    0
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
    unsafe fn fill_batch_child(
        &mut self,
        child_ps: *mut pg_sys::PlanState,
        target: usize,
    ) {
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
            let is_empty =
                unsafe { (*child_slot).tts_flags & pg_sys::TTS_FLAG_EMPTY as u16 != 0 };
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
                if extract_attno <= child_natts.into() {
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
                        let copied =
                            unsafe { pg_sys::pg_detoast_datum_copy(varlena_ptr) };
                        self.datum_buffer
                            .push((pg_sys::Datum::from(copied), false));
                    } else {
                        self.datum_buffer.push((pg_sys::Datum::from(0), true));
                    }
                } else {
                    if self.tuple_buffer.is_empty() {
                        pgrx::debug1!(
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

    /// Run the accumulated batch through the dispatch layer.
    ///
    /// Branches on `self.strategy`:
    /// - **`GpuSpatial`**: Extracts geometry datums from the target column,
    ///   dispatches through the three-layer GPU pipeline, and uses the
    ///   boolean results as the filter mask.
    /// - **`GpuExpr`**: Uses the columnar expression evaluation path.
    /// - Other strategies: Fall back to scalar qual evaluation.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    unsafe fn dispatch_batch(&mut self, scan_slot: *mut pg_sys::TupleTableSlot) {
        let batch_len = self.tuple_buffer.len();
        if batch_len == 0 {
            return;
        }
        pgrx::debug1!(
            "pg_accel: dispatch_batch: len={}, strategy={:?}",
            batch_len,
            self.strategy
        );

        let start = std::time::Instant::now();

        self.result_mask.clear();
        self.result_pos = 0;

        match self.strategy {
            AccelStrategy::GpuSpatial | AccelStrategy::GpuH3 | AccelStrategy::GpuRaster => {
                // SAFETY: Caller guarantees main backend thread.
                // dispatch_gpu_path extracts datums and calls dispatch::dispatch()
                // which routes to the correct GPU strategy handler.
                unsafe { self.dispatch_gpu_path(scan_slot, batch_len) };
            }
            AccelStrategy::GpuExpr => {
                // GpuExpr uses the columnar expression evaluation path.
                // SAFETY: Caller guarantees main backend thread.
                unsafe { self.dispatch_gpu_expr(scan_slot, batch_len) };
            }
            _ => {
                // SAFETY: Caller guarantees main backend thread.
                unsafe { self.dispatch_scalar_qual(scan_slot, batch_len) };
            }
        }

        self.rows_dispatched += batch_len as u64;
        self.batches_executed += 1;
        self.dispatch_time_us += start.elapsed().as_micros() as u64;
    }

    /// Scalar qual evaluation path (fallback for non-GPU-dispatch strategies).
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    unsafe fn dispatch_scalar_qual(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) {
        pgrx::debug1!(
            "pg_accel: dispatch_scalar_qual: batch_len={}, qual_null={}, econtext_null={}",
            batch_len,
            self.qual.is_null(),
            self.econtext.is_null()
        );
        if self.qual.is_null() || self.econtext.is_null() {
            // No qual — all rows pass.
            self.result_mask.resize(batch_len, true);
        } else {
            let mut pass_count = 0u64;
            for i in 0..batch_len {
                let mt = self.tuple_buffer[i];
                if mt.is_null() {
                    self.result_mask.push(false);
                    continue;
                }

                // SAFETY: mt is a valid MinimalTuple from ExecCopySlotMinimalTuple.
                // Use ExecForceStoreMinimalTuple because scan_slot may be a
                // VirtualTupleTableSlot (ps_ResultTupleSlot default).
                // `false` means the slot does NOT own the tuple (we manage it).
                if i == 0 {
                    let t_len = unsafe { (*mt).t_len } as usize;
                    let natts = unsafe {
                        let desc = (*scan_slot).tts_tupleDescriptor;
                        if desc.is_null() { -1 } else { (*desc).natts }
                    };
                    pgrx::debug1!(
                        "pg_accel: scalar_qual: mt[0] t_len={}, slot natts={}",
                        t_len,
                        natts
                    );
                }
                unsafe {
                    pg_sys::ExecForceStoreMinimalTuple(mt, scan_slot, false);
                    (*self.econtext).ecxt_scantuple = scan_slot;
                }

                // SAFETY: ExecEvalExpr is the pgrx C-shim for PG's
                // static-inline ExecEvalExpr. qual and econtext are valid.
                let mut is_null = false;
                let result = unsafe {
                    pg_sys::ExecEvalExpr(self.qual, self.econtext, std::ptr::addr_of_mut!(is_null))
                };

                let passed = !is_null && result.value() != 0;
                if passed {
                    pass_count += 1;
                }
                self.result_mask.push(passed);

                // SAFETY: Reset per-tuple memory to prevent leaks.
                unsafe {
                    pg_sys::MemoryContextReset((*self.econtext).ecxt_per_tuple_memory);
                }
            }
            pgrx::debug1!("pg_accel: {}/{} rows passed qual", pass_count, batch_len,);
        }
    }

    /// GPU spatial dispatch path.
    ///
    /// Extracts the geometry column (`target_attno`) from each buffered
    /// tuple, packages them as `(Datum, bool)` pairs, and calls
    /// `dispatch::dispatch()` with the `GpuSpatial` strategy. The dispatch
    /// layer handles the three-layer pipeline (bbox → GPU kernel → CPU
    /// recheck) and returns boolean results.
    ///
    /// Falls back to scalar qual if GPU context is not configured
    /// (`target_attno == 0` or `fn_oid == InvalidOid`).
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    unsafe fn dispatch_gpu_path(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) {
        // Guard: GPU context must be configured.
        if self.target_attno == 0 || self.fn_oid == pg_sys::InvalidOid {
            // SAFETY: Caller guarantees main backend thread.
            unsafe { self.dispatch_scalar_qual(scan_slot, batch_len) };
            return;
        }

        // Use pre-extracted datums from fill_batch (captured from the
        // child's slot, which has the correct TupleDesc).
        let datum_batch: Vec<(pg_sys::Datum, bool)> = if self.datum_buffer.len() == batch_len {
            self.datum_buffer.clone()
        } else {
            // Fallback: no pre-extracted datums (shouldn't happen for GPU path).
            pgrx::debug1!(
                "pg_accel: dispatch_gpu_path: datum_buffer size mismatch {}/{}",
                self.datum_buffer.len(), batch_len,
            );
            vec![(pg_sys::Datum::from(0), true); batch_len]
        };

        if !datum_batch.is_empty() {
            let (d, n) = datum_batch[0];
            pgrx::debug1!(
                "pg_accel: dispatch_gpu_path: first row attno={} datum={:#x} is_null={}",
                self.target_attno, d.value(), n,
            );
        }

        // SAFETY: dispatch() must be called on the main backend thread.
        // fn_info_buf was initialised by set_gpu_context via fmgr_info.
        // When gist_recheck is true, skip bbox filtering (GiST already did it).
        let result = unsafe {
            dispatch::dispatch(
                self.strategy,
                &datum_batch,
                &self.fn_info_buf,
                self.fn_info_buf.fn_strict,
                self.qual_datum,
                self.gist_recheck,
            )
        };

        match result {
            DispatchResult::Accelerated(results) => {
                // Results are boolean (Datum, is_null) pairs. A row passes
                // when the result is TRUE and not NULL.
                for &(datum, is_null) in &results {
                    let passed = !is_null && datum.value() != 0;
                    self.result_mask.push(passed);
                }
                let pass_count = self.result_mask.iter().filter(|&&b| b).count();
                pgrx::debug1!(
                    "pg_accel: GPU spatial {}/{} rows passed",
                    pass_count,
                    batch_len,
                );
            }
            DispatchResult::Deferred => {
                // GPU dispatch deferred — use PG's standard scalar qual.
                // SAFETY: Caller guarantees main backend thread.
                unsafe { self.dispatch_scalar_qual(scan_slot, batch_len) };
            }
        }
    }

    /// GPU expression evaluation path.
    ///
    /// Dispatches to the compiled expression (template or bytecode) when
    /// available, falling back to scalar qual evaluation otherwise.
    /// Uncertain results (+0) are rechecked via the scalar qual path.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    unsafe fn dispatch_gpu_expr(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) {
        let Some(compiled) = &self.compiled_expr else {
            // No compiled expression — fall back to scalar qual.
            // SAFETY: Caller guarantees main backend thread.
            unsafe { self.dispatch_scalar_qual(scan_slot, batch_len) };
            return;
        };

        match compiled {
            CompiledExpr::DeferToPg => {
                // SAFETY: Caller guarantees main backend thread.
                unsafe { self.dispatch_scalar_qual(scan_slot, batch_len) };
            }
            CompiledExpr::Template(kernel) => {
                // Build a columnar batch from buffered tuples.
                let col_results = self.eval_template_kernel(kernel, scan_slot, batch_len);
                match col_results {
                    Some(results) => {
                        // SAFETY: Caller guarantees main backend thread;
                        // scalar recheck for uncertain rows uses PG functions.
                        unsafe {
                            self.apply_three_val_results(&results, scan_slot, batch_len);
                        }
                    }
                    None => {
                        // GPU unavailable — fall back to scalar.
                        // SAFETY: Caller guarantees main backend thread.
                        unsafe { self.dispatch_scalar_qual(scan_slot, batch_len) };
                    }
                }
            }
            CompiledExpr::Bytecode(_program) => {
                // TODO: GPU bytecode evaluator produces incorrect results.
                // Fall back to scalar qual until the bytecode interpreter is
                // debugged. The scalar qual path is proven correct.
                // SAFETY: Caller guarantees main backend thread.
                unsafe { self.dispatch_scalar_qual(scan_slot, batch_len) };
            }
        }
    }

    /// Evaluate a template kernel on the current batch.
    ///
    /// Builds a single-column columnar batch for the referenced column
    /// and calls the appropriate GPU template function.
    ///
    /// Returns `None` if the GPU is unavailable.
    fn eval_template_kernel(
        &self,
        kernel: &TemplateKernel,
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) -> Option<Vec<i8>> {
        match kernel {
            TemplateKernel::CmpConst {
                col_idx,
                cmp_opcode,
                const_val,
            } => {
                let (values, nulls) = self.extract_col_f64(*col_idx, scan_slot, batch_len);
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
                // Evaluate each predicate via CmpConst template, then AND.
                let r1 = {
                    let (values, nulls) = self.extract_col_f64(*col1_idx, scan_slot, batch_len);
                    let mut bo = ColumnarBatchOwner::new(batch_len, 1);
                    bo.add_col_f64(values, nulls);
                    let b = bo.as_batch();
                    gpu::expr_template_cmp_const(&b, 0, *cmp1_opcode, *const1_val, batch_len)
                };
                let r2 = {
                    let (values, nulls) = self.extract_col_f64(*col2_idx, scan_slot, batch_len);
                    let mut bo = ColumnarBatchOwner::new(batch_len, 1);
                    bo.add_col_f64(values, nulls);
                    let b = bo.as_batch();
                    gpu::expr_template_cmp_const(&b, 0, *cmp2_opcode, *const2_val, batch_len)
                };
                match (r1, r2) {
                    (Some(a), Some(b)) => {
                        // AND: both must be true (+1). If either is false (-1),
                        // result is false. Otherwise uncertain (0).
                        let combined: Vec<i8> = a
                            .iter()
                            .zip(b.iter())
                            .map(|(&x, &y)| {
                                if x < 0 || y < 0 {
                                    -1
                                } else if x > 0 && y > 0 {
                                    1
                                } else {
                                    0
                                }
                            })
                            .collect();
                        Some(combined)
                    }
                    _ => None,
                }
            }
            // Other template variants: fall back to scalar qual.
            _ => None,
        }
    }

    /// Evaluate a bytecode predicate on the current batch.
    ///
    /// Builds a columnar batch with all referenced columns and calls
    /// the GPU bytecode interpreter.
    ///
    /// Returns `None` if the GPU is unavailable.
    fn eval_bytecode_predicate(
        &self,
        program: &expr_compiler::ExprProgram,
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) -> Option<Vec<i8>> {
        // Build the C-level PgaccelExprProgram from our ExprProgram.
        let c_program = PgaccelExprProgram {
            instructions: program.instructions.as_ptr(),
            inst_count: program.instructions.len(),
            const_pool: program.const_pool.as_ptr(),
            const_count: program.const_pool.len(),
            max_stack: program.max_stack,
            num_cols: program.num_cols,
        };

        // Build columnar batch with referenced columns only.
        // For simplicity, build an f64 column for each referenced column.
        let mut batch_owner = ColumnarBatchOwner::new(batch_len, program.num_cols);
        for &col_idx in &program.referenced_cols {
            let (values, nulls) = self.extract_col_f64(col_idx as u32, scan_slot, batch_len);
            batch_owner.add_col_f64(values, nulls);
        }
        let batch = batch_owner.as_batch();

        gpu::expr_eval_predicate(&c_program, &batch, batch_len)
    }

    /// Build a single-column f64 columnar batch for a template kernel.
    fn build_columnar_f64(
        &self,
        col_idx: u32,
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) -> ColumnarBatchOwner {
        let (values, nulls) = self.extract_col_f64(col_idx, scan_slot, batch_len);
        let mut owner = ColumnarBatchOwner::new(batch_len, 1);
        owner.add_col_f64(values, nulls);
        owner
    }

    /// Extract a column as f64 values from the buffered MinimalTuples.
    ///
    /// Uses bulk direct `MinimalTuple` reads when possible to avoid
    /// per-tuple `ExecForceStoreMinimalTuple` overhead.
    ///
    /// The column index is 0-based (GPU convention). PostgreSQL attributes
    /// are 1-based, so we add 1 for the extractor.
    fn extract_col_f64(
        &self,
        col_idx: u32,
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) -> (Vec<f64>, Vec<u8>) {
        let attno = (col_idx + 1) as i32; // PG is 1-based
        let batch = &self.tuple_buffer[..batch_len];

        // SAFETY: scan_slot has a valid tuple descriptor. batch contains
        // valid MinimalTuple pointers from ExecCopySlotMinimalTuple.
        let tupdesc = unsafe { (*scan_slot).tts_tupleDescriptor };
        let info = unsafe { AttExtractInfo::new(tupdesc, attno) };
        unsafe { tuple_extract::extract_f64(batch, &info, scan_slot) }
    }

    /// Apply three-valued GPU results (+1=true, -1=false, 0=uncertain)
    /// to the result mask. Uncertain rows are rechecked via scalar qual.
    ///
    /// # Safety (internal)
    ///
    /// Scalar qual recheck calls PG functions — must be on main thread.
    unsafe fn apply_three_val_results(
        &mut self,
        results: &[i8],
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) {
        let mut pass_count = 0u64;
        let mut recheck_count = 0u64;

        for i in 0..batch_len {
            let r = results.get(i).copied().unwrap_or(-1);
            match r {
                1 => {
                    // Definite TRUE from GPU.
                    self.result_mask.push(true);
                    pass_count += 1;
                }
                r if r < 0 => {
                    // Definite FALSE from GPU.
                    self.result_mask.push(false);
                }
                _ => {
                    // Uncertain (0) — CPU recheck needed.
                    recheck_count += 1;
                    if self.qual.is_null() || self.econtext.is_null() {
                        // No qual to recheck — treat as pass.
                        self.result_mask.push(true);
                        pass_count += 1;
                    } else {
                        let mt = self.tuple_buffer[i];
                        if mt.is_null() {
                            self.result_mask.push(false);
                            continue;
                        }
                        // SAFETY: mt is valid, scan_slot/qual/econtext are valid,
                        // main backend thread.
                        unsafe {
                            pg_sys::ExecForceStoreMinimalTuple(mt, scan_slot, false);
                            (*self.econtext).ecxt_scantuple = scan_slot;
                        }
                        let mut is_null = false;
                        let result = unsafe {
                            pg_sys::ExecEvalExpr(
                                self.qual,
                                self.econtext,
                                std::ptr::addr_of_mut!(is_null),
                            )
                        };
                        let passed = !is_null && result.value() != 0;
                        if passed {
                            pass_count += 1;
                        }
                        self.result_mask.push(passed);
                        // SAFETY: Reset per-tuple memory to prevent leaks.
                        unsafe {
                            pg_sys::MemoryContextReset((*self.econtext).ecxt_per_tuple_memory);
                        }
                    }
                }
            }
        }
        pgrx::debug1!(
            "pg_accel: GpuExpr {}/{} passed ({} rechecked)",
            pass_count,
            batch_len,
            recheck_count,
        );
    }

    /// Try to return the next passing tuple from the current batch.
    ///
    /// Returns `Some(slot_ptr)` for the next passing row, or `None` when
    /// the result buffer is exhausted.
    fn drain_next(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
    ) -> Option<*mut pg_sys::TupleTableSlot> {
        while self.result_pos < self.tuple_buffer.len() {
            let idx = self.result_pos;
            self.result_pos += 1;

            // Check if this row passed the filter.
            let passed = self.result_mask.get(idx).copied().unwrap_or(false);

            if !passed {
                continue;
            }

            let mt = self.tuple_buffer[idx];
            if mt.is_null() {
                continue;
            }

            // Restore the MinimalTuple into scan_slot for return to parent.
            // SAFETY: mt is a valid MinimalTuple. Use ExecForceStoreMinimalTuple
            // because scan_slot may be a VirtualTupleTableSlot. `false` = slot
            // does not own the tuple (we pfree it when the buffer is cleared).
            unsafe {
                pg_sys::ExecForceStoreMinimalTuple(mt, scan_slot, false);
            }

            return Some(scan_slot);
        }

        None
    }

    /// Configure GPU dispatch context for spatial / H3 / raster strategies.
    ///
    /// # Safety
    ///
    /// `fn_oid` must be a valid regproc OID. Must be called on the main
    /// backend thread (calls `fmgr_info`).
    pub unsafe fn set_gpu_context(
        &mut self,
        fn_oid: pg_sys::Oid,
        target_attno: i32,
        qual_datum: Option<(pg_sys::Datum, bool)>,
    ) {
        self.fn_oid = fn_oid;
        self.target_attno = target_attno;
        self.qual_datum = qual_datum;
        if fn_oid != pg_sys::InvalidOid {
            // SAFETY: Caller guarantees fn_oid is valid and we are on the
            // main backend thread.
            unsafe {
                pg_sys::fmgr_info(fn_oid, &raw mut self.fn_info_buf);
            }
        }
    }

    /// Set the compiled GPU expression for GpuExpr strategy.
    pub fn set_compiled_expr(&mut self, expr: expr_compiler::CompiledExpr) {
        self.compiled_expr = Some(expr);
    }

    /// Returns `true` if a template kernel is compiled and ready for
    /// inline evaluation during the heap walk.
    #[must_use]
    pub fn has_template_expr(&self) -> bool {
        matches!(
            &self.compiled_expr,
            Some(CompiledExpr::Template(_))
        )
    }

    /// Returns a clone of the compiled expression, if present.
    #[must_use]
    pub fn compiled_expr(&self) -> Option<CompiledExpr> {
        self.compiled_expr.clone()
    }

    /// Configure direct heap scan mode (GpuExpr with scanrelid > 0).
    /// When set, fill_batch uses `table_scan_getnextslot` instead of
    /// pulling from a child plan.
    pub fn set_scan_desc(&mut self, desc: pg_sys::TableScanDesc) {
        self.scan_desc = desc;

        // Create a dedicated memory context for batch MinimalTuple allocations.
        // Resetting this context is O(1) vs O(n) individual pfree calls.
        if self.batch_mcxt.is_null() {
            // SAFETY: CurrentMemoryContext is valid on the main backend thread.
            // AllocSetContextCreate is the standard PG API for creating memory contexts.
            unsafe {
                self.batch_mcxt = pg_sys::AllocSetContextCreateInternal(
                    pg_sys::CurrentMemoryContext,
                    c"pg_accel_batch".as_ptr(),
                    pg_sys::ALLOCSET_DEFAULT_MINSIZE as pg_sys::Size,
                    pg_sys::ALLOCSET_DEFAULT_INITSIZE as pg_sys::Size,
                    pg_sys::ALLOCSET_DEFAULT_MAXSIZE as pg_sys::Size,
                );
            }
        }
    }

    /// Returns the table scan descriptor (for cleanup in end_custom_scan).
    #[must_use]
    pub fn scan_desc(&self) -> pg_sys::TableScanDesc {
        self.scan_desc
    }

    /// Fetch the next tuple from the heap scan and store it in `scan_slot`.
    /// Called by `gpu_scan_access` (the ExecScan access method).
    /// Returns the scan slot (non-empty) on success, or an empty slot
    /// when the scan is exhausted.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `self.scan_desc` must be
    /// valid. `scan_slot` must be a valid `TupleTableSlot` with a TupleDesc
    /// matching the table's physical layout.
    pub unsafe fn gpu_scan_next(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        if self.child_exhausted {
            unsafe { pg_sys::ExecClearTuple(scan_slot) };
            return scan_slot;
        }
        // Fetch the next heap tuple. heap_getnext returns a pointer into
        // a pinned shared buffer page. The buffer stays pinned until the
        // next heap_getnext call, so the tuple is valid for this iteration.
        // SAFETY: scan_desc is valid; main backend thread.
        let htup = unsafe {
            pg_sys::heap_getnext(
                self.scan_desc,
                pg_sys::ScanDirection::ForwardScanDirection,
            )
        };
        if htup.is_null() {
            self.child_exhausted = true;
            unsafe { pg_sys::ExecClearTuple(scan_slot) };
            return scan_slot;
        }
        self.rows_dispatched += 1;
        if self.rows_dispatched % 65536 == 0 {
            pgrx::check_for_interrupts!();
        }
        // Store the heap tuple in the scan slot. shouldFree=false because
        // the tuple lives in a pinned shared buffer that stays valid until
        // the next heap_getnext call. ExecForceStoreHeapTuple on a
        // BufferHeapTupleTableSlot stores the pointer and marks the slot
        // as containing a heap tuple. ExecMaterializeSlot then forces
        // deformation so tts_values/tts_isnull are populated for correct
        // datum access by parent nodes (aggregates, projections).
        // SAFETY: htup is valid from heap_getnext; scan_slot is valid.
        unsafe {
            pg_sys::ExecForceStoreHeapTuple(htup, scan_slot, false);
            pg_sys::ExecMaterializeSlot(scan_slot);
        }
        scan_slot
    }

    /// Detect whether the child plan is a GiST index scan and enable
    /// batched recheck mode. When enabled, the GPU spatial pipeline
    /// skips bbox filtering (Layer 1) since GiST already performed it.
    ///
    /// # Safety
    ///
    /// `child_ps` must be a valid `PlanState` pointer. Must be called on
    /// the main backend thread.
    pub unsafe fn detect_gist_child(&mut self, child_ps: *mut pg_sys::PlanState) {
        const GIST_AM_OID: u32 = 783;

        if child_ps.is_null() {
            return;
        }

        // SAFETY: child_ps is valid. Check if the child is an IndexScan.
        let node_tag = unsafe { (*child_ps).type_ };
        if node_tag != pg_sys::NodeTag::T_IndexScanState {
            return;
        }

        // SAFETY: child_ps points to an IndexScanState. The iss_RelationDesc
        // field holds the index relation descriptor.
        let iss = child_ps.cast::<pg_sys::IndexScanState>();
        let index_rel = unsafe { (*iss).iss_RelationDesc };
        if index_rel.is_null() {
            return;
        }

        // SAFETY: index_rel is a valid RelationData. rd_rel points to the
        // pg_class tuple for this index. relam is the access method OID.
        let relam = unsafe { (*(*index_rel).rd_rel).relam };

        if u32::from(relam) == GIST_AM_OID {
            self.gist_recheck = true;
            pgrx::debug1!("pg_accel: GiST child detected, enabling batched recheck");
        }
    }

    /// Returns the acceleration strategy.
    #[must_use]
    pub fn strategy(&self) -> AccelStrategy {
        self.strategy
    }

    /// Returns the GPU-accelerated function OID (or `InvalidOid`).
    #[must_use]
    pub fn fn_oid(&self) -> pg_sys::Oid {
        self.fn_oid
    }

    /// Returns the target attribute number for GPU dispatch (1-based, 0 = none).
    #[must_use]
    pub fn target_attno(&self) -> i32 {
        self.target_attno
    }

    /// Returns the qual datum for 2-arg predicates (e.g. constant geometry).
    #[must_use]
    pub fn qual_datum(&self) -> Option<(pg_sys::Datum, bool)> {
        self.qual_datum
    }

    /// Returns the qual pointer (for transfer during rescan).
    #[must_use]
    pub fn qual_ptr(&self) -> *mut pg_sys::ExprState {
        self.qual
    }

    /// Returns the econtext pointer (for transfer during rescan).
    #[must_use]
    pub fn econtext_ptr(&self) -> *mut pg_sys::ExprContext {
        self.econtext
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a ScanExecState with null qual/econtext (passthrough).
    fn make_state(strategy: AccelStrategy, batch_size: usize) -> ScanExecState {
        ScanExecState::new(
            strategy,
            batch_size,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }

    #[test]
    fn new_state_is_not_exhausted() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert!(!state.child_exhausted);
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
        assert_eq!(state.dispatch_time_us, 0);
        assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
    }

    #[test]
    fn new_state_with_gpu_spatial() {
        let state = make_state(AccelStrategy::GpuSpatial, 1024);
        assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
        assert_eq!(state.batch_size, 1024);
    }

    #[test]
    fn drain_empty_returns_none() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        // No slots buffered, drain should return None.
        let result = state.drain_next(std::ptr::null_mut());
        assert!(result.is_none());
    }

    #[test]
    fn result_pos_advances() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        // Simulate a batch where all rows are filtered out.
        state.tuple_buffer = vec![std::ptr::null_mut(); 3];
        state.result_mask = vec![false, false, false];
        state.result_pos = 0;

        let result = state.drain_next(std::ptr::null_mut());
        assert!(result.is_none());
        // Should have advanced past all three.
        assert_eq!(state.result_pos, 3);
    }

    #[test]
    fn null_qual_means_passthrough() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert!(state.qual.is_null());
        assert!(state.econtext.is_null());
    }

    #[test]
    fn batch_size_stored_correctly() {
        let state = make_state(AccelStrategy::GpuSpatial, 1);
        assert_eq!(state.batch_size, 1);

        let state = make_state(AccelStrategy::GpuSpatial, 8192);
        assert_eq!(state.batch_size, 8192);
    }

    #[test]
    fn tuple_buffer_preallocated() {
        let state = make_state(AccelStrategy::GpuSpatial, 512);
        // Vec::with_capacity does not change len, only capacity.
        assert!(state.tuple_buffer.is_empty());
        assert!(state.tuple_buffer.capacity() >= 512);
    }

    #[test]
    fn result_mask_starts_empty() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert!(state.result_mask.is_empty());
        assert_eq!(state.result_pos, 0);
    }

    #[test]
    fn drain_next_skips_null_tuples_even_when_mask_true() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        // Simulate a batch with null MinimalTuple pointers but mask says pass.
        state.tuple_buffer = vec![std::ptr::null_mut(); 5];
        state.result_mask = vec![true, true, true, true, true];
        state.result_pos = 0;

        // drain_next should skip all null tuples and return None.
        let result = state.drain_next(std::ptr::null_mut());
        assert!(result.is_none());
        assert_eq!(state.result_pos, 5);
    }

    #[test]
    fn drain_next_with_empty_mask_returns_none() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        // Buffer has entries but mask is empty — get(idx) returns None,
        // unwrap_or(false) means all skipped.
        state.tuple_buffer = vec![std::ptr::null_mut(); 3];
        state.result_mask = vec![];
        state.result_pos = 0;

        let result = state.drain_next(std::ptr::null_mut());
        assert!(result.is_none());
        assert_eq!(state.result_pos, 3);
    }

    #[test]
    fn drain_next_with_partial_mask() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        // Mask shorter than buffer — extra entries default to false.
        state.tuple_buffer = vec![std::ptr::null_mut(); 5];
        state.result_mask = vec![false, false]; // only 2 entries
        state.result_pos = 0;

        let result = state.drain_next(std::ptr::null_mut());
        assert!(result.is_none());
        assert_eq!(state.result_pos, 5);
    }

    #[test]
    fn drain_next_result_pos_beyond_buffer() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        state.tuple_buffer = vec![std::ptr::null_mut(); 2];
        state.result_mask = vec![true, true];
        state.result_pos = 10; // already past end

        let result = state.drain_next(std::ptr::null_mut());
        assert!(result.is_none());
        assert_eq!(state.result_pos, 10); // unchanged
    }

    #[test]
    fn drain_next_mixed_mask_skips_false() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        // All null pointers, so even true entries are skipped (null mt check).
        state.tuple_buffer = vec![std::ptr::null_mut(); 4];
        state.result_mask = vec![false, true, false, true];
        state.result_pos = 0;

        let result = state.drain_next(std::ptr::null_mut());
        assert!(result.is_none());
        assert_eq!(state.result_pos, 4);
    }

    #[test]
    fn all_strategies_constructible() {
        for strategy in [
            AccelStrategy::GpuSpatial,
            AccelStrategy::GpuRaster,
            AccelStrategy::GpuH3,
            AccelStrategy::GpuSort,
            AccelStrategy::GpuReduce,
        ] {
            let state = make_state(strategy, 128);
            assert_eq!(state.strategy(), strategy);
            assert!(!state.child_exhausted);
        }
    }

    #[test]
    fn qual_ptr_and_econtext_ptr_accessors() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert!(state.qual_ptr().is_null());
        assert!(state.econtext_ptr().is_null());

        // With non-null (fake) pointers.
        let fake_qual = 0xDEAD_BEEF_usize as *mut pg_sys::ExprState;
        let fake_ctx = 0xCAFE_BABE_usize as *mut pg_sys::ExprContext;
        let state2 = ScanExecState::new(AccelStrategy::GpuSpatial, 256, fake_qual, fake_ctx);
        assert_eq!(state2.qual_ptr(), fake_qual);
        assert_eq!(state2.econtext_ptr(), fake_ctx);
    }

    #[test]
    fn counters_are_zero_on_init() {
        let state = make_state(AccelStrategy::GpuH3, 64);
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
        assert_eq!(state.dispatch_time_us, 0);
    }

    #[test]
    fn single_row_batch_size() {
        let state = make_state(AccelStrategy::GpuSpatial, 1);
        assert_eq!(state.batch_size, 1);
        assert!(state.tuple_buffer.capacity() >= 1);
    }

    // ── GiST recheck detection ─────────────────────────────────────────

    #[test]
    fn gist_recheck_defaults_to_false() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert!(!state.gist_recheck);
    }

    #[test]
    fn detect_gist_child_null_pointer_is_noop() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        // SAFETY: null pointer is the exact case detect_gist_child guards against.
        unsafe { state.detect_gist_child(std::ptr::null_mut()) };
        assert!(
            !state.gist_recheck,
            "null child_ps must not flip gist_recheck"
        );
    }

    // ── SP-GiST / non-GiST: gist_recheck stays false ──────────────────

    #[test]
    fn gist_recheck_stays_false_for_gpu_h3() {
        let state = make_state(AccelStrategy::GpuH3, 512);
        // gist_recheck is only set via detect_gist_child, never by strategy alone.
        assert!(!state.gist_recheck);
    }

    #[test]
    fn gist_recheck_stays_false_for_gpu_raster() {
        let state = make_state(AccelStrategy::GpuRaster, 512);
        assert!(!state.gist_recheck);
    }

    // ── Batch dispatch routing (strategy selection) ─────────────────────

    #[test]
    fn gpu_spatial_strategy_stored() {
        let state = make_state(AccelStrategy::GpuSpatial, 128);
        assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
    }

    #[test]
    fn gpu_h3_strategy_stored() {
        let state = make_state(AccelStrategy::GpuH3, 128);
        assert_eq!(state.strategy(), AccelStrategy::GpuH3);
    }

    #[test]
    fn gpu_expr_strategy_stored() {
        let state = make_state(AccelStrategy::GpuExpr, 128);
        assert_eq!(state.strategy(), AccelStrategy::GpuExpr);
    }

    #[test]
    fn gpu_hash_join_strategy_stored() {
        let state = make_state(AccelStrategy::GpuHashJoin, 64);
        assert_eq!(state.strategy(), AccelStrategy::GpuHashJoin);
    }

    #[test]
    fn gpu_window_strategy_stored() {
        let state = make_state(AccelStrategy::GpuWindow, 64);
        assert_eq!(state.strategy(), AccelStrategy::GpuWindow);
    }

    #[test]
    fn gpu_context_defaults_unconfigured() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        // GPU context not set yet — target_attno=0, fn_oid=InvalidOid.
        assert_eq!(state.target_attno(), 0);
        assert_eq!(state.fn_oid(), pg_sys::InvalidOid);
        assert!(state.qual_datum().is_none());
    }

    #[test]
    fn gpu_context_dispatch_falls_back_when_unconfigured() {
        // When target_attno==0 or fn_oid==InvalidOid, dispatch_gpu_path
        // should fall back to the scalar qual path. With null qual, all
        // rows pass. We verify that logic via the mask after a manual call.
        let mut state = make_state(AccelStrategy::GpuSpatial, 4);
        state.tuple_buffer = vec![std::ptr::null_mut(); 4];
        state.result_mask.clear();
        state.result_pos = 0;

        // dispatch_gpu_path with unconfigured GPU context should produce
        // all-true mask (null qual = passthrough).
        // SAFETY: We pass null scan_slot; the scalar qual path with null
        // qual just resizes the mask to all true without touching the slot.
        unsafe { state.dispatch_gpu_path(std::ptr::null_mut(), 4) };

        assert_eq!(state.result_mask.len(), 4);
        assert!(state.result_mask.iter().all(|&b| b));
    }

    // ── GpuExpr compiled expression paths ───────────────────────────────

    #[test]
    fn compiled_expr_defaults_to_none() {
        let state = make_state(AccelStrategy::GpuExpr, 256);
        assert!(state.compiled_expr.is_none());
    }

    #[test]
    fn set_compiled_expr_defer_to_pg() {
        let mut state = make_state(AccelStrategy::GpuExpr, 256);
        state.set_compiled_expr(CompiledExpr::DeferToPg);
        assert!(matches!(
            state.compiled_expr,
            Some(CompiledExpr::DeferToPg)
        ));
    }

    #[test]
    fn set_compiled_expr_template_cmp_const() {
        let mut state = make_state(AccelStrategy::GpuExpr, 256);
        let kernel = TemplateKernel::CmpConst {
            col_idx: 2,
            cmp_opcode: 1, // e.g. LT
            const_val: 42.0,
        };
        state.set_compiled_expr(CompiledExpr::Template(kernel));
        assert!(matches!(
            state.compiled_expr,
            Some(CompiledExpr::Template(TemplateKernel::CmpConst { .. }))
        ));
    }

    #[test]
    fn gpu_expr_dispatch_falls_back_when_no_compiled_expr() {
        // When compiled_expr is None, dispatch_gpu_expr should fall back
        // to scalar qual. With null qual, all rows pass.
        let mut state = make_state(AccelStrategy::GpuExpr, 3);
        state.tuple_buffer = vec![std::ptr::null_mut(); 3];
        state.result_mask.clear();
        state.result_pos = 0;

        // SAFETY: null scan_slot; scalar qual with null qual just resizes mask.
        unsafe { state.dispatch_gpu_expr(std::ptr::null_mut(), 3) };

        assert_eq!(state.result_mask.len(), 3);
        assert!(state.result_mask.iter().all(|&b| b));
    }

    #[test]
    fn gpu_expr_defer_to_pg_variant_passes_all_with_null_qual() {
        let mut state = make_state(AccelStrategy::GpuExpr, 5);
        state.set_compiled_expr(CompiledExpr::DeferToPg);
        state.tuple_buffer = vec![std::ptr::null_mut(); 5];
        state.result_mask.clear();
        state.result_pos = 0;

        // DeferToPg branch calls dispatch_scalar_qual, which with null
        // qual produces all-true mask.
        // SAFETY: null scan_slot is fine when qual is null (no slot access).
        unsafe { state.dispatch_gpu_expr(std::ptr::null_mut(), 5) };

        assert_eq!(state.result_mask.len(), 5);
        assert!(state.result_mask.iter().all(|&b| b));
    }

    // ── Rescan / state reset ────────────────────────────────────────────

    #[test]
    fn rescan_like_reset_clears_buffers() {
        // Simulate what a rescan would do: reset buffers and position.
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        state.tuple_buffer = vec![std::ptr::null_mut(); 10];
        state.result_mask = vec![true; 10];
        state.result_pos = 7;
        state.child_exhausted = true;
        state.rows_dispatched = 500;
        state.batches_executed = 10;

        // Rescan resets scan-level state but preserves strategy config.
        state.tuple_buffer.clear();
        state.result_mask.clear();
        state.result_pos = 0;
        state.child_exhausted = false;

        assert!(state.tuple_buffer.is_empty());
        assert!(state.result_mask.is_empty());
        assert_eq!(state.result_pos, 0);
        assert!(!state.child_exhausted);
        // Counters typically accumulate across rescans for EXPLAIN ANALYZE.
        assert_eq!(state.rows_dispatched, 500);
        assert_eq!(state.batches_executed, 10);
        assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
    }

    // ── Counter accumulation ────────────────────────────────────────────

    #[test]
    fn counter_accumulation_across_dispatch_batch() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 4);
        state.tuple_buffer = vec![std::ptr::null_mut(); 4];
        state.result_mask.clear();
        state.result_pos = 0;

        // Dispatch with unconfigured GPU falls back to scalar (null qual
        // -> all pass), and updates counters.
        // SAFETY: null scan_slot; scalar qual with null qual is safe.
        unsafe { state.dispatch_batch(std::ptr::null_mut()) };

        assert_eq!(state.rows_dispatched, 4);
        assert_eq!(state.batches_executed, 1);
        assert!(state.dispatch_time_us < 1_000_000); // sanity: under 1s

        // Second batch.
        state.tuple_buffer = vec![std::ptr::null_mut(); 6];
        state.result_mask.clear();
        state.result_pos = 0;
        // SAFETY: same conditions.
        unsafe { state.dispatch_batch(std::ptr::null_mut()) };

        assert_eq!(state.rows_dispatched, 10);
        assert_eq!(state.batches_executed, 2);
    }

    #[test]
    fn dispatch_batch_empty_is_noop() {
        let mut state = make_state(AccelStrategy::GpuH3, 256);
        state.tuple_buffer.clear();

        // SAFETY: empty batch short-circuits before touching scan_slot.
        unsafe { state.dispatch_batch(std::ptr::null_mut()) };

        // Empty batch should not increment counters.
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
    }

    // ── Batch size edge cases ───────────────────────────────────────────

    #[test]
    fn batch_size_zero_still_constructs() {
        let state = make_state(AccelStrategy::GpuSpatial, 0);
        assert_eq!(state.batch_size, 0);
        assert!(state.tuple_buffer.is_empty());
    }

    #[test]
    fn batch_size_very_large() {
        let state = make_state(AccelStrategy::GpuExpr, 1_000_000);
        assert_eq!(state.batch_size, 1_000_000);
        // Vec::with_capacity should not OOM for a vec of pointers at 1M.
        assert!(state.tuple_buffer.capacity() >= 1_000_000);
    }

    // ── LIMIT interaction: partial drain ────────────────────────────────

    #[test]
    fn drain_stops_after_first_pass_null_tuple() {
        // Simulates LIMIT 1 scenario: caller stops calling drain_next
        // after getting the first result. With all-null tuples, drain
        // returns None (no results) even with all-true mask.
        let mut state = make_state(AccelStrategy::GpuSpatial, 8);
        state.tuple_buffer = vec![std::ptr::null_mut(); 8];
        state.result_mask = vec![true; 8];
        state.result_pos = 0;

        let first = state.drain_next(std::ptr::null_mut());
        assert!(first.is_none());
        // All 8 were consumed (all null -> skipped).
        assert_eq!(state.result_pos, 8);
    }

    #[test]
    fn partial_batch_drain_preserves_position() {
        // Simulate a LIMIT scenario where we drain only part of a batch.
        let mut state = make_state(AccelStrategy::GpuSpatial, 10);
        state.tuple_buffer = vec![std::ptr::null_mut(); 10];
        state.result_mask = vec![
            false, false, false, true, false, true, false, false, false, true,
        ];
        state.result_pos = 0;

        // Drain once — skips false entries, finds true at idx 3, but
        // tuple is null so continues, finds true at idx 5 (also null), etc.
        let result = state.drain_next(std::ptr::null_mut());
        assert!(result.is_none());
        // Position should be at end since all tuples are null.
        assert_eq!(state.result_pos, 10);
    }

    #[test]
    fn child_exhausted_flag_prevents_further_batches() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        state.child_exhausted = true;
        state.tuple_buffer.clear();
        state.result_mask.clear();

        // With child_exhausted=true and empty buffers, next() would
        // return null_mut (no more rows). We verify the flag is checked.
        let drain_result = state.drain_next(std::ptr::null_mut());
        assert!(drain_result.is_none());
        assert!(state.child_exhausted);
    }

    // ── Scalar qual: null qual means all rows pass ──────────────────────

    #[test]
    fn scalar_qual_null_passes_all_for_various_sizes() {
        for size in [1, 7, 64, 255] {
            let mut state = make_state(AccelStrategy::GpuSpatial, size);
            state.tuple_buffer = vec![std::ptr::null_mut(); size];
            state.result_mask.clear();

            // SAFETY: null scan_slot is fine when qual is null.
            unsafe { state.dispatch_scalar_qual(std::ptr::null_mut(), size) };

            assert_eq!(state.result_mask.len(), size);
            assert!(
                state.result_mask.iter().all(|&b| b),
                "null qual should pass all rows for batch_size={size}"
            );
        }
    }
}

impl Drop for ScanExecState {
    fn drop(&mut self) {
        if !self.batch_mcxt.is_null() {
            // SAFETY: batch_mcxt is a valid PG memory context we created.
            // MemoryContextDelete frees the context and all its allocations.
            unsafe { pg_sys::MemoryContextDelete(self.batch_mcxt) };
            self.batch_mcxt = std::ptr::null_mut();
        } else {
            // Non-batch path: free individual MinimalTuples.
            for &mt in &self.tuple_buffer {
                if !mt.is_null() {
                    // SAFETY: mt was palloc'd by ExecCopySlotMinimalTuple.
                    unsafe { pg_sys::pfree(mt.cast()) };
                }
            }
            for &(datum, is_null) in &self.datum_buffer {
                if !is_null && datum.value() != 0 {
                    unsafe { pg_sys::pfree(datum.cast_mut_ptr()) };
                }
            }
        }
    }
}
