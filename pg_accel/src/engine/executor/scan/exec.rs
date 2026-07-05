//! Scan exec-path methods — batch fill + dispatch + result drain.

use pgrx::pg_sys;

use crate::engine::columnar::{BatchSelection, ColumnarBatchOwner, CpuBoundaryReason};
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

    /// Run the accumulated batch through the dispatch layer.
    ///
    /// Branches on `self.strategy`:
    /// - **`GpuSpatial`**: Extracts geometry datums from the target column,
    ///   dispatches through the three-layer GPU pipeline, and uses the
    ///   boolean results as the filter mask.
    /// - **`GpuExpr`**: Uses the columnar expression evaluation path.
    /// - Other strategies: executor/planner bug; selected plans must not
    ///   run a CPU-only scalar path.
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

        self.clear_results();

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
                pgrx::error!(
                    "pg_accel: scan executor received {:?}; refusing CPU fallback under a GPU plan",
                    self.strategy
                );
            }
        }

        let elapsed_us = start.elapsed().as_micros() as u64;
        self.record_dispatch_batch(batch_len as u64, elapsed_us);

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

    /// GPU spatial dispatch path.
    ///
    /// Extracts the geometry column (`target_attno`) from each buffered
    /// tuple, packages them as `(Datum, bool)` pairs, and calls
    /// `dispatch::dispatch()` with the `GpuSpatial` strategy. The dispatch
    /// layer handles the GPU pipeline and returns boolean results. Runtime
    /// host-side predicate evaluation is forbidden inside pg_accel.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    pub(super) unsafe fn dispatch_gpu_path(
        &mut self,
        _scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) {
        // Guard: GPU context must be configured.
        if self.target_attno == 0 || self.fn_oid == pg_sys::InvalidOid {
            pgrx::error!("pg_accel: GPU scan context is not configured; refusing CPU fallback");
        }

        // Use pre-extracted datums from fill_batch (captured from the
        // child's slot, which has the correct TupleDesc).
        let datum_batch: Vec<(pg_sys::Datum, bool)> = if self.datum_buffer.len() == batch_len {
            self.datum_buffer.clone()
        } else {
            pgrx::error!(
                "pg_accel: dispatch_gpu_path: datum_buffer size mismatch {}/{}",
                self.datum_buffer.len(),
                batch_len,
            );
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
                &self.qual_datums,
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
                pgrx::error!(
                    "pg_accel: GPU dispatch deferred at execution time; planner must decline this path"
                );
            }
            DispatchResult::AcceleratedRecord { .. } | DispatchResult::AcceleratedVarLen { .. } => {
                // The Record / VarLen variants exist for record-returning
                // (e.g. ST_SummaryStats with 6 fp64 fields) and var-length
                // (e.g. H3 grid_disk emitting many cells per input row)
                // ops. Reaching this arm means dispatch produced one of those
                // shapes for a function in a per-row scan filter context —
                // which can't happen today: the planner only injects
                // GpuAccelScan with these strategies for predicate / scalar
                // contexts. SRF and record-returning function calls land in
                // the projection path (ProjectSet / FunctionScan) which is
                // a different planner hook entirely.
                //
                // If this arm is hit, dispatch routed the wrong way. That is
                // a planner/executor contract bug, not a reason to execute a
                // CPU fallback under a GpuAccelScan node.
                pgrx::error!(
                    "pg_accel: scan dispatch returned non-scalar shape (Record/VarLen) \
                     in per-row filter context; refusing CPU fallback"
                );
            }
        }
    }

    /// GPU expression evaluation path.
    ///
    /// Dispatches to the compiled expression (template or bytecode). The GPU
    /// program must return definite booleans; uncertain rows are planner or
    /// kernel coverage bugs, not permission to run a CPU qual inside pg_accel.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    pub(super) unsafe fn dispatch_gpu_expr(
        &mut self,
        output_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) {
        let Some(compiled) = self.compiled_expr.clone() else {
            pgrx::error!("pg_accel: GpuExpr has no compiled GPU expression; refusing CPU fallback");
        };
        let expr_slot = self.expression_extract_slot(output_slot);

        match &compiled {
            CompiledExpr::DeferToPg => {
                pgrx::error!(
                    "pg_accel: GpuExpr was marked DeferToPg; planner must decline this path"
                );
            }
            CompiledExpr::Template(kernel) => {
                // Build a columnar batch from buffered tuples.
                let col_results = self.eval_template_kernel(kernel, expr_slot, batch_len);
                match col_results {
                    Some(results) => {
                        // SAFETY: Caller guarantees main backend thread.
                        unsafe {
                            self.apply_three_val_results(&results, output_slot, batch_len);
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
                let result = self.eval_bytecode_predicate(program, expr_slot, batch_len);
                match result {
                    Some(results) => {
                        // SAFETY: Caller guarantees main backend thread.
                        unsafe {
                            self.apply_three_val_results(&results, output_slot, batch_len);
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
        &mut self,
        kernel: &TemplateKernel,
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) -> Option<Vec<i8>> {
        match kernel {
            TemplateKernel::CmpConst {
                col_idx,
                cmp_opcode,
                const_val,
            } => self.with_f64_scan_batch(scan_slot, batch_len, 1, [*col_idx], |batch_owner| {
                let batch = batch_owner.as_batch();
                gpu::expr_template_cmp_const(&batch, 0, *cmp_opcode, *const_val, batch_len)
            }),
            TemplateKernel::TwoPredAnd {
                col1_idx,
                cmp1_opcode,
                const1_val,
                col2_idx,
                cmp2_opcode,
                const2_val,
            } => {
                // Keep the scan producer benefit (both predicate columns are
                // packed once in direct heap order) but avoid the fused
                // struct-argument Metal kernel. The fused
                // pgaccel_expr_template_two_pred_and path aborts inside
                // Metal's argument encoder on macOS; two stable cmp_const
                // launches plus a deterministic three-valued AND preserves
                // correctness without CPU predicate evaluation.
                self.with_f64_scan_batch(
                    scan_slot,
                    batch_len,
                    2,
                    [*col1_idx, *col2_idx],
                    |batch_owner| {
                        let batch = batch_owner.as_batch();
                        let lhs = gpu::expr_template_cmp_const(
                            &batch,
                            0,
                            *cmp1_opcode,
                            *const1_val,
                            batch_len,
                        )?;
                        let rhs = gpu::expr_template_cmp_const(
                            &batch,
                            1,
                            *cmp2_opcode,
                            *const2_val,
                            batch_len,
                        )?;
                        Self::combine_three_val_and(&lhs, &rhs, batch_len)
                    },
                )
            }
            // Other template variants are planner/compiler bugs for selected
            // GpuExpr paths; the caller turns None into an ERROR.
            _ => None,
        }
    }

    pub(super) fn combine_three_val_and(
        lhs: &[i8],
        rhs: &[i8],
        batch_len: usize,
    ) -> Option<Vec<i8>> {
        if lhs.len() != batch_len || rhs.len() != batch_len {
            return None;
        }

        let mut out = Vec::with_capacity(batch_len);
        for i in 0..batch_len {
            let l = lhs[i];
            let r = rhs[i];
            if l < 0 || r < 0 {
                out.push(-1);
            } else if l == 1 && r == 1 {
                out.push(1);
            } else {
                out.push(0);
            }
        }
        Some(out)
    }

    pub(super) fn three_val_results_to_selection(
        results: &[i8],
        batch_len: usize,
    ) -> Option<BatchSelection> {
        if results.len() < batch_len {
            return None;
        }

        let mut mask = Vec::with_capacity(batch_len);
        for i in 0..batch_len {
            match results[i] {
                1 => mask.push(1),
                r if r < 0 => mask.push(0),
                _ => return None,
            }
        }
        Some(BatchSelection::from_mask_for_rows(mask, batch_len))
    }

    /// Evaluate a bytecode predicate on the current batch.
    ///
    /// Builds a columnar batch with all referenced columns (in the
    /// dense order produced by `expr_compiler::build()`'s LOAD_COL
    /// remap) and calls the GPU bytecode interpreter.
    ///
    /// Returns `None` if the GPU is unavailable.
    fn eval_bytecode_predicate(
        &mut self,
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

        // Build columnar batch with referenced columns only, in the dense
        // order produced by ExprProgramBuilder::build().
        self.with_f64_scan_batch(
            scan_slot,
            batch_len,
            program.num_cols,
            program
                .referenced_cols
                .iter()
                .map(|&col_idx| col_idx as u32),
            |batch_owner| {
                let batch = batch_owner.as_batch();
                gpu::expr_eval_predicate(&c_program, &batch, batch_len)
            },
        )
    }

    /// Source columns required by the currently compiled GpuExpr.
    ///
    /// Template kernels preserve predicate argument order because their C++
    /// entry points read dense packed slots by position. Bytecode programs
    /// use the compiler's sorted/deduped `referenced_cols` order, matching
    /// the LOAD_COL dense remap.
    pub(super) fn gpuexpr_source_cols(&self) -> Option<Vec<u32>> {
        match self.compiled_expr.as_ref()? {
            CompiledExpr::Template(
                TemplateKernel::CmpConst { col_idx, .. }
                | TemplateKernel::Between { col_idx, .. }
                | TemplateKernel::InList { col_idx, .. }
                | TemplateKernel::IsNull { col_idx, .. },
            ) => Some(vec![*col_idx]),
            CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                col1_idx, col2_idx, ..
            }) => Some(vec![*col1_idx, *col2_idx]),
            CompiledExpr::Bytecode(program) => Some(
                program
                    .referenced_cols
                    .iter()
                    .map(|&col_idx| col_idx as u32)
                    .collect(),
            ),
            CompiledExpr::DeferToPg => None,
        }
    }

    /// Use a direct heap-built expression batch when available; otherwise
    /// build the same dense f64 batch from the buffered MinimalTuples.
    fn with_f64_scan_batch<I, F, R>(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
        num_cols: usize,
        col_indices: I,
        f: F,
    ) -> R
    where
        I: IntoIterator<Item = u32>,
        F: FnOnce(&mut ColumnarBatchOwner) -> R,
    {
        if let Some(owner) = self
            .expr_batch_owner
            .as_mut()
            .filter(|owner| owner.num_rows == batch_len && owner.num_cols == num_cols)
        {
            return f(owner);
        }

        let mut owner =
            self.build_f64_batch_from_tuple_buffer(scan_slot, batch_len, num_cols, col_indices);
        f(&mut owner)
    }

    /// Build the dense f64 GpuExpr input batch from buffered MinimalTuples.
    fn build_f64_batch_from_tuple_buffer<I>(
        &self,
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
        num_cols: usize,
        col_indices: I,
    ) -> ColumnarBatchOwner
    where
        I: IntoIterator<Item = u32>,
    {
        let mut owner = ColumnarBatchOwner::new(batch_len, num_cols);
        owner.mark_host_boundary(CpuBoundaryReason::HostTupleReconstruction);
        for col_idx in col_indices {
            let (values, nulls) = self.extract_col_f64(col_idx, scan_slot, batch_len);
            owner.add_col_f64(values, nulls);
        }
        owner
    }

    #[cfg(feature = "pg_test")]
    #[allow(dead_code)] // reason: exercised by the scan test module under the pg_test feature
    pub(super) fn build_f64_batch_from_extracted_columns(
        batch_len: usize,
        columns: Vec<(Vec<f64>, Vec<u8>)>,
    ) -> ColumnarBatchOwner {
        let mut owner = ColumnarBatchOwner::new(batch_len, columns.len());
        owner.mark_host_boundary(CpuBoundaryReason::HostInputStaging);
        for (values, nulls) in columns {
            owner.add_col_f64(values, nulls);
        }
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

    /// Apply GPU results (+1=true, -1=false) to the result mask.
    /// Uncertain rows are rejected because pg_accel plans must not run CPU
    /// qual fallback work.
    ///
    /// # Safety (internal)
    ///
    /// Must run on the main backend thread.
    unsafe fn apply_three_val_results(
        &mut self,
        results: &[i8],
        scan_slot: *mut pg_sys::TupleTableSlot,
        batch_len: usize,
    ) {
        let Some(selection) = Self::three_val_results_to_selection(results, batch_len) else {
            let uncertain_row = results
                .iter()
                .take(batch_len)
                .position(|&r| r == 0)
                .unwrap_or(batch_len);
            let _ = scan_slot;
            pgrx::error!(
                "pg_accel: GpuExpr returned an uncertain result at row {}; refusing CPU qual fallback",
                uncertain_row,
            );
        };

        let pass_count = selection.selected_rows(batch_len);
        match selection {
            BatchSelection::AllRows => {
                self.result_mask
                    .extend(std::iter::repeat_n(true, batch_len));
            }
            BatchSelection::Mask(mask) => {
                self.result_mask.extend(mask.into_iter().map(|b| b != 0));
            }
        }
        tracing::debug!("pg_accel: GpuExpr {}/{} passed", pass_count, batch_len,);
    }

    /// Try to return the next passing tuple from the current batch.
    ///
    /// Returns `Some(slot_ptr)` for the next passing row, or `None` when
    /// the result buffer is exhausted.
    pub(super) fn drain_next(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
    ) -> Option<*mut pg_sys::TupleTableSlot> {
        // SAFETY: tuple_buffer entries were copied from PostgreSQL slots or
        // direct heap tuples and stay alive until the next batch clear/drop.
        unsafe {
            self.result_drain.drain_minimal_tuple_to_slot(
                &self.tuple_buffer,
                &self.result_mask,
                scan_slot,
            )
        }
    }
}
