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
        loop {
            // 1. Try to return the next passing row from the current batch.
            if let Some(slot) = self.drain_next(scan_slot) {
                return slot;
            }

            // 2. If child is exhausted and no buffered results, we are done.
            if self.child_exhausted {
                return std::ptr::null_mut();
            }

            // 3. Accumulate the next batch from the child.
            // SAFETY: Caller guarantees child_plan_state is valid and we
            // are on the main backend thread.
            unsafe {
                self.fill_batch(child_plan_state);
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

    /// Pull tuples from the child plan until the batch is full or the
    /// child is exhausted.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` must be valid.
    unsafe fn fill_batch(&mut self, child_ps: *mut pg_sys::PlanState) {
        // SAFETY: Free previously-buffered MinimalTuples before clearing.
        // ExecCopySlotMinimalTuple returns palloc'd memory we own; raw
        // pointers do not call pfree on drop, so we must free explicitly.
        for &mt in &self.tuple_buffer {
            if !mt.is_null() {
                unsafe { pg_sys::pfree(mt.cast()) };
            }
        }
        self.tuple_buffer.clear();
        self.result_mask.clear();
        self.result_pos = 0;

        let target = self.batch_size.max(gucs::min_batch_size().max(1) as usize);

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

            // Copy the tuple into our own storage. The child plan reuses
            // the same TupleTableSlot for every ExecProcNode call, so
            // storing slot pointers would give N copies of the last tuple.
            // ExecCopySlotMinimalTuple returns a palloc'd copy we own.
            // SAFETY: child_slot is valid and non-empty.
            // SAFETY: ExecCopySlotMinimalTuple handles any slot type
            // internally — no ExecMaterializeSlot needed.
            let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(child_slot) };

            self.tuple_buffer.push(mt);
        }
    }

    /// Run the accumulated batch through the dispatch layer.
    ///
    /// Branches on `self.strategy`:
    /// - **`BatchedEval`**: Evaluates the stolen qual expression per-tuple
    ///   via `ExecEvalExpr` (existing scalar path).
    /// - **`GpuSpatial`**: Extracts geometry datums from the target column,
    ///   dispatches through the three-layer GPU pipeline, and uses the
    ///   boolean results as the filter mask.
    /// - **`GpuH3` / `GpuRaster` / `GpuSort` / `GpuReduce`**: Currently
    ///   fall back to the BatchedEval qual path (TODO).
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    unsafe fn dispatch_batch(&mut self, scan_slot: *mut pg_sys::TupleTableSlot) {
        let batch_len = self.tuple_buffer.len();
        if batch_len == 0 {
            return;
        }

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

    /// Scalar qual evaluation path (BatchedEval and fallback).
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    unsafe fn dispatch_scalar_qual(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) {
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

        // Extract the target column datum from each buffered tuple.
        let mut datum_batch: Vec<(pg_sys::Datum, bool)> = Vec::with_capacity(batch_len);

        for i in 0..batch_len {
            let mt = self.tuple_buffer[i];
            if mt.is_null() {
                datum_batch.push((pg_sys::Datum::from(0), true));
                continue;
            }

            // SAFETY: Store the MinimalTuple into scan_slot so we can
            // use slot_getattr to extract the target column.
            unsafe {
                pg_sys::ExecForceStoreMinimalTuple(mt, scan_slot, false);
            }

            // SAFETY: slot_getattr extracts the attribute at target_attno.
            // scan_slot is valid with a stored MinimalTuple.
            let mut is_null = false;
            let datum = unsafe {
                pg_sys::slot_getattr(
                    scan_slot,
                    self.target_attno,
                    std::ptr::addr_of_mut!(is_null),
                )
            };

            datum_batch.push((datum, is_null));
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
            DispatchResult::Fallback => {
                // GPU dispatch declined — fall back to scalar qual.
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
            CompiledExpr::CpuFallback => {
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
            CompiledExpr::Bytecode(program) => {
                let col_results = self.eval_bytecode_predicate(program, scan_slot, batch_len);
                match col_results {
                    Some(results) => {
                        // SAFETY: Caller guarantees main backend thread.
                        unsafe {
                            self.apply_three_val_results(&results, scan_slot, batch_len);
                        }
                    }
                    None => {
                        // SAFETY: Caller guarantees main backend thread.
                        unsafe { self.dispatch_scalar_qual(scan_slot, batch_len) };
                    }
                }
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
                let mut batch_owner = self.build_columnar_f64(*col_idx, scan_slot, batch_len);
                let batch = batch_owner.as_batch();
                gpu::expr_template_cmp_const(&batch, *col_idx, *cmp_opcode, *const_val, batch_len)
            }
            // For other template variants, fall back to scalar for now.
            // The CmpConst template is the most common pattern and covers
            // the majority of WHERE clause expressions.
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
    /// The column index is 0-based (GPU convention). PostgreSQL attributes
    /// are 1-based, so we add 1 for `slot_getattr`.
    fn extract_col_f64(
        &self,
        col_idx: u32,
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) -> (Vec<f64>, Vec<u8>) {
        let attno = (col_idx + 1) as i32; // PG is 1-based
        let mut values = Vec::with_capacity(batch_len);
        let mut nulls = Vec::with_capacity(batch_len);

        for i in 0..batch_len {
            let mt = self.tuple_buffer[i];
            if mt.is_null() {
                values.push(0.0);
                nulls.push(1);
                continue;
            }

            // SAFETY: mt is a valid MinimalTuple. scan_slot is valid.
            // slot_getattr extracts the attribute value. We are on
            // the main backend thread.
            let mut is_null = false;
            let datum = unsafe {
                pg_sys::ExecForceStoreMinimalTuple(mt, scan_slot, false);
                pg_sys::slot_getattr(scan_slot, attno, std::ptr::addr_of_mut!(is_null))
            };

            if is_null {
                values.push(0.0);
                nulls.push(1);
            } else {
                // SAFETY: For numeric types (int4, int8, float4, float8),
                // the datum value can be cast to f64. This is a simplification
                // that works for the common case of numeric comparisons.
                values.push(f64::from_bits(datum.value() as u64));
                nulls.push(0);
            }
        }

        (values, nulls)
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
        let state = make_state(AccelStrategy::BatchedEval, 256);
        assert!(!state.child_exhausted);
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
        assert_eq!(state.dispatch_time_us, 0);
        assert_eq!(state.strategy(), AccelStrategy::BatchedEval);
    }

    #[test]
    fn new_state_with_gpu_spatial() {
        let state = make_state(AccelStrategy::GpuSpatial, 1024);
        assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
        assert_eq!(state.batch_size, 1024);
    }

    #[test]
    fn drain_empty_returns_none() {
        let mut state = make_state(AccelStrategy::BatchedEval, 256);
        // No slots buffered, drain should return None.
        let result = state.drain_next(std::ptr::null_mut());
        assert!(result.is_none());
    }

    #[test]
    fn result_pos_advances() {
        let mut state = make_state(AccelStrategy::BatchedEval, 256);
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
        let state = make_state(AccelStrategy::BatchedEval, 256);
        assert!(state.qual.is_null());
        assert!(state.econtext.is_null());
    }

    #[test]
    fn batch_size_stored_correctly() {
        let state = make_state(AccelStrategy::BatchedEval, 1);
        assert_eq!(state.batch_size, 1);

        let state = make_state(AccelStrategy::BatchedEval, 8192);
        assert_eq!(state.batch_size, 8192);
    }

    #[test]
    fn tuple_buffer_preallocated() {
        let state = make_state(AccelStrategy::BatchedEval, 512);
        // Vec::with_capacity does not change len, only capacity.
        assert!(state.tuple_buffer.is_empty());
        assert!(state.tuple_buffer.capacity() >= 512);
    }

    #[test]
    fn result_mask_starts_empty() {
        let state = make_state(AccelStrategy::BatchedEval, 256);
        assert!(state.result_mask.is_empty());
        assert_eq!(state.result_pos, 0);
    }

    #[test]
    fn drain_next_skips_null_tuples_even_when_mask_true() {
        let mut state = make_state(AccelStrategy::BatchedEval, 256);
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
        let mut state = make_state(AccelStrategy::BatchedEval, 256);
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
        let mut state = make_state(AccelStrategy::BatchedEval, 256);
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
        let mut state = make_state(AccelStrategy::BatchedEval, 256);
        state.tuple_buffer = vec![std::ptr::null_mut(); 2];
        state.result_mask = vec![true, true];
        state.result_pos = 10; // already past end

        let result = state.drain_next(std::ptr::null_mut());
        assert!(result.is_none());
        assert_eq!(state.result_pos, 10); // unchanged
    }

    #[test]
    fn drain_next_mixed_mask_skips_false() {
        let mut state = make_state(AccelStrategy::BatchedEval, 256);
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
            AccelStrategy::BatchedEval,
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
        let state = make_state(AccelStrategy::BatchedEval, 256);
        assert!(state.qual_ptr().is_null());
        assert!(state.econtext_ptr().is_null());

        // With non-null (fake) pointers.
        let fake_qual = 0xDEAD_BEEF_usize as *mut pg_sys::ExprState;
        let fake_ctx = 0xCAFE_BABE_usize as *mut pg_sys::ExprContext;
        let state2 = ScanExecState::new(AccelStrategy::BatchedEval, 256, fake_qual, fake_ctx);
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
        let state = make_state(AccelStrategy::BatchedEval, 1);
        assert_eq!(state.batch_size, 1);
        assert!(state.tuple_buffer.capacity() >= 1);
    }
}
