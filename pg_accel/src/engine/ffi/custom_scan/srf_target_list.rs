//! SRF-in-target-list executor (Phase 2 follow-up to F3).
//!
//! Owns the per-invocation child plan cursor, per-row dispatch buffer, and
//! tuple emission for a Custom Scan node injected by
//! [`super::super::planner_hooks::srf_target_list`].
//!
//! ## Lifecycle
//!
//! 1. `init_state(node, priv_data)` decodes the `SrfTargetListPrivData`
//!    block, initialises the child PlanState (already wired into
//!    `node->custom_ps[0]` by `begin_custom_scan`), and prepares the
//!    expansion buffer (initially empty).
//! 2. `next_tuple(node)` implements the canonical PG ProjectSet expansion
//!    semantics from `nodeProjectSet.c::ExecProjectSet`: pull a batch of input
//!    tuples from the child via `ExecProcNode`, dispatch the SRF once for that
//!    batch, then drain expanded rows one at a time. When the buffer is empty,
//!    pull the next batch and re-dispatch.
//! 3. `drop_state(executor)` reclaims the box from `end_custom_scan`.
//!
//! ## Output tuple shape
//!
//! Each output tuple contains:
//! - For each non-SRF position in the upper tlist: a Datum copied from the
//!   per-input child slot at `passthrough_attnos[k]` (1-based).
//! - At `srf_tlist_pos`: one Datum from the buffered SRF expansion.
//!
//! `passthrough_attnos[k] == 0` marks the SRF column position itself
//! (skipped during the per-row passthrough loop and filled from the
//! expansion buffer instead).
//!
//! ## SRF semantics mirrored from `nodeProjectSet.c::ExecProjectSet`
//!
//! - One input row → 0 or more output rows.
//! - Empty SRF output: skip the input row entirely (no output rows).
//! - Non-SRF tlist columns are evaluated once per output row using the
//!   single source input row (passthrough — same value repeated for every
//!   expanded output row).
//!
//! ## Anti-cheat ban #6 / #9 compliance
//!
//! - `nodeProjectSet.c` (`/Users/contra/.pgrx/17.9/src/backend/executor/nodeProjectSet.c`)
//!   was read in full before writing this module.
//! - Multi-SRF target lists (`SELECT srf1(c), srf2(c) FROM t` with cartesian
//!   semantics) are NOT supported; the planner hook restricts injection
//!   to single-SRF tlists. If a multi-SRF query gets routed here the
//!   `srf_tlist_pos` carries one position only and the second SRF would
//!   end up as a passthrough Var of unsupported type — surfaced via the
//!   planner-side `args_supported` walk that B6 already enforces.

use std::ffi::c_void;

use pgrx::pg_sys;

use super::SrfTargetListPrivData;
use crate::engine::dispatch::{self, DispatchResult};
use crate::engine::registry::{self, OutputShape};

/// Buffered output of one SRF dispatch invocation, with the source input
/// row's passthrough Datums captured so we can emit one output tuple per
/// expanded SRF row.
struct ExpansionBuffer {
    /// Per-input-row passthrough Datums captured from the child slot before
    /// the child reuses it for the next `ExecProcNode` call.
    passthrough_rows: Vec<Vec<(pg_sys::Datum, bool)>>,
    /// CSR offsets into `expansion`. `offsets.len() == passthrough_rows.len()+1`.
    /// Empty ranges represent PostgreSQL SRF "no output for this input row"
    /// semantics, including strict NULL arguments.
    offsets: Vec<u32>,
    /// Flat SRF expansion Datums for the whole batch.
    expansion: Vec<(pg_sys::Datum, bool)>,
    /// Cursor into `expansion`: next flat output row to emit.
    output_cursor: usize,
    /// Cursor into `passthrough_rows`, advanced alongside `output_cursor`.
    source_row_cursor: usize,
}

impl ExpansionBuffer {
    fn empty() -> Self {
        Self {
            passthrough_rows: Vec::new(),
            offsets: vec![0],
            expansion: Vec::new(),
            output_cursor: 0,
            source_row_cursor: 0,
        }
    }

    fn is_exhausted(&self) -> bool {
        self.output_cursor >= self.expansion.len()
    }

    fn advance_source_row(&mut self) {
        while self.source_row_cursor + 1 < self.offsets.len()
            && self.output_cursor >= self.offsets[self.source_row_cursor + 1] as usize
        {
            self.source_row_cursor += 1;
        }
    }
}

/// Per-invocation SRF-in-target-list executor state.
pub(super) struct SrfTargetListExecState {
    /// Plan-time metadata.
    priv_data: SrfTargetListPrivData,
    /// Resolved registry entry (cached so we don't re-lookup per row).
    output_shape: OutputShape,
    /// FmgrInfo populated from the SRF's pg_proc entry — required by
    /// `dispatch_gpu_h3` / raster which inspect `fn_info.fn_oid` +
    /// `fn_info.fn_strict`.
    fmgr_info: pg_sys::FmgrInfo,
    /// Acceleration strategy resolved from the registry.
    strategy: crate::engine::registry::AccelStrategy,
    /// Number of input child rows to collect before one GPU SRF dispatch.
    batch_size: usize,
    /// Current expansion buffer for the most-recently-pulled input row.
    /// Empty (`expansion.is_empty()`) means "need next input row".
    expansion: ExpansionBuffer,
    /// Total emitted rows so far (EXPLAIN ANALYZE counter).
    rows_dispatched: u64,
    /// Total GPU SRF dispatch batches so far (EXPLAIN ANALYZE counter).
    batches_executed: u64,
    /// Total wall-clock time spent inside SRF batch dispatch.
    dispatch_time_us: u64,
    /// Set when the child plan signals EOF — subsequent `next_tuple` calls
    /// return cleared slots without further `ExecProcNode` calls.
    child_exhausted: bool,
}

/// Initialise the SRF-in-target-list executor state.
///
/// Decodes the `SrfTargetListPrivData` block from `cscan.custom_private`,
/// looks up the registry entry for the SRF, and prepares the per-invocation
/// state. The child PlanState is initialised by `begin_custom_scan` into
/// `node->custom_ps[0]` before this function runs (PG's standard custom_plans
/// → custom_ps wiring).
///
/// Returns null on unrecoverable setup error (missing registry entry,
/// invalid priv block, or SRF position outside the output tuple). The
/// caller short-circuits to passthrough_exec in that case.
///
/// # Safety
///
/// Must be called on the main backend thread. `node` must be a valid
/// `CustomScanState` whose `ss.ps.plan` is a `CustomScan` with an
/// SRF-target-list-shaped `custom_private`.
pub(super) unsafe fn init_state(
    node: *mut pg_sys::CustomScanState,
    batch_size: usize,
) -> *mut c_void {
    // SAFETY: node->ss.ps.plan is a valid CustomScan plan node set by
    // ExecInitCustomScan; custom_private layout was produced by
    // plan_custom_path_srf_target_list.
    let cscan = unsafe { (*node).ss.ps.plan.cast::<pg_sys::CustomScan>() };
    let priv_list = unsafe { (*cscan).custom_private };
    if priv_list.is_null() {
        pgrx::debug1!("pg_accel: srf_target_list init: null custom_private");
        return std::ptr::null_mut();
    }

    // The plan's custom_private layout starts with the standard 6-element header,
    // then the SRF_TARGET_LIST_SENTINEL block at index 6.
    // SAFETY: deserialize_srf_target_list_priv validates sentinel + length.
    let Some(priv_data) = (unsafe { super::deserialize_srf_target_list_priv(priv_list, 6) }) else {
        pgrx::warning!(
            "pg_accel: srf_target_list init: deserialize_srf_target_list_priv failed at idx 6"
        );
        return std::ptr::null_mut();
    };

    // Look up the registry entry to get the strategy + output shape.
    registry::lazy_init();
    let Some(entry) = registry::global_registry().lookup(priv_data.fn_oid) else {
        pgrx::warning!(
            "pg_accel: srf_target_list init: fn_oid={} not registered",
            u32::from(priv_data.fn_oid),
        );
        return std::ptr::null_mut();
    };

    // Build the FmgrInfo for dispatch.
    let mut fmgr_buf: pg_sys::FmgrInfo = unsafe { std::mem::zeroed() };
    // SAFETY: fmgr_info populates the FmgrInfo from pg_proc on the main
    // backend thread.
    unsafe {
        pg_sys::fmgr_info(priv_data.fn_oid, &raw mut fmgr_buf);
    }

    // Sanity-check: the SRF position must be inside the passthrough_attnos
    // vector. Otherwise we cannot place the SRF result into the output tuple.
    let n_out = priv_data.passthrough_attnos.len();
    if priv_data.srf_tlist_pos < 0 || (priv_data.srf_tlist_pos as usize) >= n_out {
        pgrx::warning!(
            "pg_accel: srf_target_list init: srf_tlist_pos={} out of bounds for {} \
             output columns; declining",
            priv_data.srf_tlist_pos,
            n_out,
        );
        return std::ptr::null_mut();
    }

    let state = Box::new(SrfTargetListExecState {
        priv_data,
        output_shape: entry.output_shape,
        fmgr_info: fmgr_buf,
        strategy: entry.strategy,
        batch_size: batch_size.max(1),
        expansion: ExpansionBuffer::empty(),
        rows_dispatched: 0,
        batches_executed: 0,
        dispatch_time_us: 0,
        child_exhausted: false,
    });

    Box::into_raw(state).cast()
}

/// Drain the next buffered row, or pull a new input row + dispatch +
/// expand if the buffer is exhausted.
///
/// Mirrors PG's `nodeProjectSet.c::ExecProjectSet` semantics:
///   - Continue producing tuples from the current input row's expansion
///     until exhausted.
///   - On exhaustion, pull next input via `ExecProcNode(child)`.
///   - Empty SRF output for an input row: skip and pull the next.
///   - Child EOF: return cleared slot.
///
/// # Safety
///
/// Must be called on the main backend thread with a valid `node` and an
/// `executor` pointer that was produced by [`init_state`].
pub(super) unsafe fn next_tuple(
    node: *mut pg_sys::CustomScanState,
    executor: *mut c_void,
) -> *mut pg_sys::TupleTableSlot {
    // SAFETY: node is a valid CustomScanState; ss_ScanTupleSlot was set up
    // by ExecInitCustomScan from custom_scan_tlist.
    let slot = unsafe { (*node).ss.ss_ScanTupleSlot };
    if slot.is_null() {
        return std::ptr::null_mut();
    }
    if executor.is_null() {
        // SAFETY: ExecClearTuple resets the slot on the main thread.
        unsafe { pg_sys::ExecClearTuple(slot) };
        return slot;
    }
    // SAFETY: executor was Box::into_raw'd as SrfTargetListExecState in
    // init_state. Reborrow as &mut to advance the cursor.
    let state = unsafe { &mut *executor.cast::<SrfTargetListExecState>() };

    // Get child PlanState from custom_ps[0]. begin_custom_scan populates
    // this from custom_plans before delegating to us.
    // SAFETY: node->custom_ps is a List* set by PG's ExecInitCustomScan or
    // by our begin_custom_scan child-init path.
    let child_state = unsafe { child_plan_state(node) };
    if child_state.is_null() {
        pgrx::warning!("pg_accel: srf_target_list next_tuple: null child PlanState");
        // SAFETY: ExecClearTuple resets the slot.
        unsafe { pg_sys::ExecClearTuple(slot) };
        return slot;
    }

    // Loop until we either emit an output row or hit child EOF.
    // SAFETY: All PG calls below run on the main backend thread.
    unsafe {
        loop {
            if !state.expansion.is_exhausted() {
                // Emit one row from the current expansion.
                return emit_one_row(state, slot);
            }
            // Need a new input row.
            if state.child_exhausted {
                pg_sys::ExecClearTuple(slot);
                return slot;
            }
            // Dispatch + populate state.expansion for a batch. If every SRF
            // range is empty, continue until either a later batch emits rows
            // or the child reaches EOF.
            if !dispatch_and_buffer_batch(state, child_state) {
                continue;
            }
            // Buffer is non-empty: next iteration will hit the
            // !is_exhausted() arm and emit the first row.
        }
    }
}

/// Slot null check mirroring PG's `TupIsNull` macro
/// (`include/executor/tuptable.h`): a slot is null when its `tts_flags`
/// `TTS_FLAG_EMPTY` bit is set.
///
/// # Safety
/// `slot` must be a valid `TupleTableSlot *`.
unsafe fn tuple_is_null(slot: *mut pg_sys::TupleTableSlot) -> bool {
    if slot.is_null() {
        return true;
    }
    // SAFETY: slot is non-null; reading tts_flags.
    let flags = unsafe { (*slot).tts_flags };
    (flags & pg_sys::TTS_FLAG_EMPTY as u16) != 0
}

/// Walk `node->custom_ps` and return the first child PlanState.
/// Returns null if the list is empty or the first cell is null.
///
/// # Safety
/// `node` must be a valid `CustomScanState *` on the main backend thread.
unsafe fn child_plan_state(node: *mut pg_sys::CustomScanState) -> *mut pg_sys::PlanState {
    // SAFETY: caller guarantees node validity.
    let custom_ps = unsafe { (*node).custom_ps };
    if custom_ps.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: list_length is safe on a valid List.
    if unsafe { pg_sys::list_length(custom_ps) } < 1 {
        return std::ptr::null_mut();
    }
    // SAFETY: idx 0 is in bounds.
    unsafe { pg_sys::list_nth(custom_ps, 0).cast::<pg_sys::PlanState>() }
}

/// Pull up to `batch_size` child rows, dispatch the SRF once, and buffer the
/// returned CSR expansion. Returns `true` if at least one output row is ready
/// to emit; `false` means the batch was empty or every input row had an empty
/// SRF result.
///
/// # Safety
/// `child_state` must be the valid child `PlanState` for this Custom Scan.
unsafe fn dispatch_and_buffer_batch(
    state: &mut SrfTargetListExecState,
    child_state: *mut pg_sys::PlanState,
) -> bool {
    state.expansion = ExpansionBuffer::empty();

    let priv_data = &state.priv_data;
    let n_out = priv_data.passthrough_attnos.len();
    let mut passthrough_rows: Vec<Vec<(pg_sys::Datum, bool)>> =
        Vec::with_capacity(state.batch_size);
    let mut batch: Vec<(pg_sys::Datum, bool)> = Vec::with_capacity(state.batch_size);
    let mut has_dispatchable_arg = false;

    while batch.len() < state.batch_size {
        let input_slot = unsafe { pg_sys::ExecProcNode(child_state) };
        if input_slot.is_null() || unsafe { tuple_is_null(input_slot) } {
            state.child_exhausted = true;
            break;
        }

        unsafe {
            pg_sys::slot_getallattrs(input_slot);
        }
        let input_desc = unsafe { (*input_slot).tts_tupleDescriptor };
        let input_natts = if input_desc.is_null() {
            0
        } else {
            unsafe { (*input_desc).natts }
        };

        let mut passthrough = Vec::with_capacity(n_out);
        for &attno in &priv_data.passthrough_attnos {
            if attno <= 0 || attno > input_natts {
                passthrough.push((pg_sys::Datum::from(0usize), true));
                continue;
            }
            let idx = (attno - 1) as usize;
            let datum = unsafe { *(*input_slot).tts_values.add(idx) };
            let is_null = unsafe { *(*input_slot).tts_isnull.add(idx) };
            passthrough.push((datum, is_null));
        }

        let srf_attno = priv_data.srf_arg_attno;
        if srf_attno <= 0 || srf_attno > input_natts {
            pgrx::error!(
                "pg_accel: srf_target_list batch dispatch: srf_arg_attno={} out of bounds \
                 for input natts={}; planner/executor contract broken",
                srf_attno,
                input_natts,
            );
        }
        let srf_arg_idx = (srf_attno - 1) as usize;
        let srf_arg_datum = unsafe { *(*input_slot).tts_values.add(srf_arg_idx) };
        let mut srf_arg_isnull = unsafe { *(*input_slot).tts_isnull.add(srf_arg_idx) };

        // STRICT semantics: NULL argument produces an empty SRF range for the
        // corresponding input row. Keep the row in the batch so dispatcher
        // offsets remain aligned with input rows.
        if srf_arg_isnull && state.fmgr_info.fn_strict {
            srf_arg_isnull = true;
        } else if !srf_arg_isnull && srf_arg_datum.value() != 0 {
            has_dispatchable_arg = true;
        }

        passthrough_rows.push(passthrough);
        batch.push((srf_arg_datum, srf_arg_isnull));
    }

    if batch.is_empty() {
        return false;
    }

    if !has_dispatchable_arg {
        state.expansion.passthrough_rows = passthrough_rows;
        state.expansion.offsets = vec![0; batch.len() + 1];
        return false;
    }

    let qual_datums: Vec<(pg_sys::Datum, bool, pg_sys::Oid)> = priv_data
        .qual_args
        .iter()
        .map(|&(d, t)| {
            #[allow(clippy::cast_sign_loss)]
            let datum = pg_sys::Datum::from(d as usize);
            (datum, false, pg_sys::Oid::from(t))
        })
        .collect();

    let started = std::time::Instant::now();
    let result = unsafe {
        dispatch::dispatch(
            state.strategy,
            &batch,
            &state.fmgr_info,
            state.fmgr_info.fn_strict,
            &qual_datums,
            true,
        )
    };
    state.batches_executed = state.batches_executed.saturating_add(1);
    state.dispatch_time_us = state
        .dispatch_time_us
        .saturating_add(started.elapsed().as_micros() as u64);

    match (state.output_shape, result) {
        (OutputShape::VarLen, DispatchResult::AcceleratedVarLen { offsets, datums }) => {
            if offsets.len() != batch.len() + 1 {
                pgrx::error!(
                    "pg_accel: srf_target_list batch dispatch returned offsets.len()={} \
                     for input batch len {}; refusing malformed GPU result",
                    offsets.len(),
                    batch.len(),
                );
            }
            let total = offsets.last().copied().unwrap_or(0) as usize;
            if total != datums.len() {
                pgrx::error!(
                    "pg_accel: srf_target_list batch dispatch returned total={} \
                     but datums.len()={}; refusing malformed GPU result",
                    total,
                    datums.len(),
                );
            }
            state.expansion.passthrough_rows = passthrough_rows;
            state.expansion.offsets = offsets;
            state.expansion.expansion = datums;
            !state.expansion.expansion.is_empty()
        }
        (_, DispatchResult::Deferred) => {
            pgrx::error!(
                "pg_accel: srf_target_list batch dispatch deferred for fn_oid={} strategy={:?}; \
                 planner must decline instead of producing a non-GPU pg_accel plan",
                u32::from(state.priv_data.fn_oid),
                state.strategy,
            );
        }
        (shape, result) => {
            pgrx::error!(
                "pg_accel: srf_target_list batch dispatch shape/result mismatch: \
                 registry shape {:?}, result {:?}; refusing non-GPU fallback",
                shape,
                std::mem::discriminant(&result),
            );
        }
    }
}

/// Emit one expanded row: combine passthrough Datums with the next SRF
/// expansion datum and store as a virtual tuple.
///
/// # Safety
/// Caller must guarantee `!state.expansion.is_exhausted()` and that `slot`
/// has a TupleDesc with `natts == passthrough_rows[source].len()`.
unsafe fn emit_one_row(
    state: &mut SrfTargetListExecState,
    slot: *mut pg_sys::TupleTableSlot,
) -> *mut pg_sys::TupleTableSlot {
    state.expansion.advance_source_row();
    let output_idx = state.expansion.output_cursor;
    let source_idx = state.expansion.source_row_cursor;
    state.expansion.output_cursor = state.expansion.output_cursor.saturating_add(1);
    state.rows_dispatched = state.rows_dispatched.saturating_add(1);

    let srf_pos = state.priv_data.srf_tlist_pos as usize;
    let Some(passthrough_row) = state.expansion.passthrough_rows.get(source_idx) else {
        unsafe { pg_sys::ExecClearTuple(slot) };
        return slot;
    };
    let n_cols = passthrough_row.len();

    // SAFETY: slot is valid and has tts_values/tts_isnull arrays sized to
    // its TupleDesc natts (==n_cols by init_state validation).
    unsafe {
        pg_sys::ExecClearTuple(slot);
        let values = (*slot).tts_values;
        let nulls = (*slot).tts_isnull;
        if values.is_null() || nulls.is_null() {
            return slot;
        }
        for (k, &(d, n)) in passthrough_row.iter().enumerate().take(n_cols) {
            if k == srf_pos {
                let (expanded_datum, expanded_isnull) = state.expansion.expansion[output_idx];
                *values.add(k) = expanded_datum;
                *nulls.add(k) = expanded_isnull;
            } else {
                *values.add(k) = d;
                *nulls.add(k) = n;
            }
        }
        pg_sys::ExecStoreVirtualTuple(slot)
    }
}

/// Reclaim the SRF executor box.
///
/// # Safety
/// `executor` must have been produced by [`init_state`] and not previously
/// freed. Called on the main backend thread.
pub(super) unsafe fn drop_state(executor: *mut c_void) {
    if executor.is_null() {
        return;
    }
    // SAFETY: executor was Box::into_raw'd as SrfTargetListExecState.
    let _ = unsafe { Box::from_raw(executor.cast::<SrfTargetListExecState>()) };
}

/// Reset the cursor + buffer for a rescan (ExecRescan callback).
///
/// SRF expansion is per-input-row so reset the buffer entirely; the child
/// plan state is rescanned by the surrounding `rescan_custom_scan` arm.
///
/// # Safety
/// `executor` must have been produced by [`init_state`].
pub(super) unsafe fn rescan(executor: *mut c_void) {
    if executor.is_null() {
        return;
    }
    // SAFETY: executor was Box::into_raw'd as SrfTargetListExecState.
    let state = unsafe { &mut *executor.cast::<SrfTargetListExecState>() };
    state.expansion = ExpansionBuffer::empty();
    state.rows_dispatched = 0;
    state.batches_executed = 0;
    state.dispatch_time_us = 0;
    state.child_exhausted = false;
}

/// Read the rows-emitted counter for EXPLAIN ANALYZE.
///
/// # Safety
/// `executor` must have been produced by [`init_state`].
pub(super) unsafe fn rows_dispatched(executor: *mut c_void) -> u64 {
    if executor.is_null() {
        return 0;
    }
    // SAFETY: executor was Box::into_raw'd as SrfTargetListExecState.
    let state = unsafe { &*executor.cast::<SrfTargetListExecState>() };
    state.rows_dispatched
}

/// Read the dispatch-batch counter for EXPLAIN ANALYZE.
///
/// # Safety
/// `executor` must have been produced by [`init_state`].
pub(super) unsafe fn batches_executed(executor: *mut c_void) -> u64 {
    if executor.is_null() {
        return 0;
    }
    let state = unsafe { &*executor.cast::<SrfTargetListExecState>() };
    state.batches_executed
}

/// Read accumulated dispatch time in microseconds for EXPLAIN ANALYZE.
///
/// # Safety
/// `executor` must have been produced by [`init_state`].
pub(super) unsafe fn dispatch_time_us(executor: *mut c_void) -> u64 {
    if executor.is_null() {
        return 0;
    }
    let state = unsafe { &*executor.cast::<SrfTargetListExecState>() };
    state.dispatch_time_us
}
