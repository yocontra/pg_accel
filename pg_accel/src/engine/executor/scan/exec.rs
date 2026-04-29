//! Scan exec-path methods — batch fill + dispatch + result drain.

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
        if !self.scan_desc.is_null()
            && let Some(CompiledExpr::Template(_)) = self.compiled_expr
        {
            return unsafe { self.inline_filter_scan(scan_slot) };
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
                Some(CompiledExpr::Template(TemplateKernel::CmpConst { col_idx, .. })) => {
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
            if self.rows_dispatched.is_multiple_of(65536) {
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
                        && Self::inline_eval_cmp(t_data, &infos[1], *cmp2_opcode, *const2_val)
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
                    tuple_extract::try_fast_read_heap_pub::<f32>(t_data, info).map(f64::from)
                }
                t if t == pg_sys::INT2OID => {
                    tuple_extract::try_fast_read_heap_pub::<i16>(t_data, info).map(f64::from)
                }
                t if t == pg_sys::INT4OID => {
                    tuple_extract::try_fast_read_heap_pub::<i32>(t_data, info).map(f64::from)
                }
                t if t == pg_sys::INT8OID => {
                    tuple_extract::try_fast_read_heap_pub::<i64>(t_data, info).map(|v| v as f64)
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
    pub(super) unsafe fn dispatch_batch(&mut self, scan_slot: *mut pg_sys::TupleTableSlot) {
        let batch_len = self.tuple_buffer.len();
        if batch_len == 0 {
            return;
        }
        tracing::debug!(
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

        let elapsed_us = start.elapsed().as_micros() as u64;
        self.rows_dispatched += batch_len as u64;
        self.batches_executed += 1;
        self.dispatch_time_us += elapsed_us;

        // Per-backend stats: record this batch completion on the main thread.
        stats::record_batch(batch_len as u64, elapsed_us);
        // GpuSpatial / GpuH3 / GpuRaster / GpuExpr all dispatched a GPU kernel
        // above. Count the rows fed to the GPU kernel for this batch.
        if matches!(
            self.strategy,
            AccelStrategy::GpuSpatial
                | AccelStrategy::GpuH3
                | AccelStrategy::GpuRaster
                | AccelStrategy::GpuExpr
        ) {
            stats::record_gpu_batch(batch_len as u64, 0);
        }
    }

    /// Scalar qual evaluation path (fallback for non-GPU-dispatch strategies).
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    pub(super) unsafe fn dispatch_scalar_qual(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) {
        tracing::debug!(
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
                    tracing::debug!(
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
            tracing::debug!("pg_accel: {}/{} rows passed qual", pass_count, batch_len,);
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
    pub(super) unsafe fn dispatch_gpu_path(
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
            tracing::debug!(
                "pg_accel: dispatch_gpu_path: datum_buffer size mismatch {}/{}",
                self.datum_buffer.len(),
                batch_len,
            );
            vec![(pg_sys::Datum::from(0), true); batch_len]
        };

        if !datum_batch.is_empty() {
            let (d, n) = datum_batch[0];
            tracing::debug!(
                "pg_accel: dispatch_gpu_path: first row attno={} datum={:#x} is_null={}",
                self.target_attno,
                d.value(),
                n,
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
                tracing::debug!(
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
    pub(super) unsafe fn dispatch_gpu_expr(
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
                        pgrx::error!(
                            "pg_accel: template qual GPU kernel failed; refusing CPU fallback (rule 11)"
                        );
                    }
                }
            }
            CompiledExpr::Bytecode(program) => {
                // Phase 2 dispatch re-enable: with the SYCL kernel for
                // pgaccel_expr_eval_predicate (pgaccel-kernels/src/
                // expr_eval.cpp) and the LOAD_COL dense-index remap in
                // expr_compiler::build(), the bytecode path now produces
                // correct results for the supported opcode set.
                // SAFETY: Caller guarantees main backend thread.
                let result = self.eval_bytecode_predicate(program, scan_slot, batch_len);
                match result {
                    Some(results) => {
                        // SAFETY: Caller guarantees main backend thread;
                        // scalar recheck for uncertain rows uses PG functions.
                        unsafe {
                            self.apply_three_val_results(&results, scan_slot, batch_len);
                        }
                    }
                    None => {
                        // GPU unavailable / dispatch failed: error per
                        // rule 11 (no CPU fallback) — same policy as the
                        // template path above.
                        pgrx::error!(
                            "pg_accel: bytecode qual GPU kernel failed; refusing CPU fallback (rule 11)"
                        );
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
            // Other template variants: fall back to scalar qual.
            _ => None,
        }
    }

    /// Evaluate a bytecode predicate on the current batch.
    ///
    /// Builds a columnar batch with all referenced columns (in the
    /// dense order produced by `expr_compiler::build()`'s LOAD_COL
    /// remap) and calls the GPU bytecode interpreter.
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
    #[allow(dead_code)]
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
        tracing::debug!(
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
    pub(super) fn drain_next(
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
            pg_sys::heap_getnext(self.scan_desc, pg_sys::ScanDirection::ForwardScanDirection)
        };
        if htup.is_null() {
            self.child_exhausted = true;
            unsafe { pg_sys::ExecClearTuple(scan_slot) };
            return scan_slot;
        }
        self.rows_dispatched += 1;
        if self.rows_dispatched.is_multiple_of(65536) {
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
}
