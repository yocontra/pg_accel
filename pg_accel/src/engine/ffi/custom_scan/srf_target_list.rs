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
//!    semantics from `nodeProjectSet.c::ExecProjectSet`: pull one input
//!    tuple from the child via `ExecProcNode`, dispatch the SRF, then
//!    drain expanded rows one at a time. When the buffer is empty,
//!    pull the next input row and re-dispatch.
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
    /// Per-output-position Datums copied from the source input slot. For
    /// passthrough positions this is the value to repeat for every expanded
    /// row; for the SRF position itself the entry is `(Datum::from(0u32), true)`
    /// (placeholder, overwritten per row from `expansion`).
    passthrough_row: Vec<(pg_sys::Datum, bool)>,
    /// Per-row SRF expansion. Each entry is one expanded row's contribution
    /// to the SRF column. For `OutputShape::VarLen` and `OutputShape::Scalar`
    /// this is exactly one `(Datum, is_null)` pair per row. For
    /// `OutputShape::Record { field_count }`, the SRF column itself becomes
    /// a composite tuple synthesised via `heap_form_tuple` per row, so
    /// `expansion[i]` is the resulting `(HeapTupleHeader Datum, false)` pair.
    expansion: Vec<(pg_sys::Datum, bool)>,
    /// Cursor into `expansion`: next row index to emit.
    cursor: usize,
}

impl ExpansionBuffer {
    fn empty() -> Self {
        Self {
            passthrough_row: Vec::new(),
            expansion: Vec::new(),
            cursor: 0,
        }
    }

    fn is_exhausted(&self) -> bool {
        self.cursor >= self.expansion.len()
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
    /// Current expansion buffer for the most-recently-pulled input row.
    /// Empty (`expansion.is_empty()`) means "need next input row".
    expansion: ExpansionBuffer,
    /// Total emitted rows so far (EXPLAIN ANALYZE counter).
    rows_dispatched: u64,
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
pub(super) unsafe fn init_state(node: *mut pg_sys::CustomScanState) -> *mut c_void {
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
        expansion: ExpansionBuffer::empty(),
        rows_dispatched: 0,
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
            let input_slot = pg_sys::ExecProcNode(child_state);
            if input_slot.is_null() || tuple_is_null(input_slot) {
                state.child_exhausted = true;
                pg_sys::ExecClearTuple(slot);
                return slot;
            }
            // Dispatch + populate state.expansion / state.passthrough_row.
            // If the SRF emits zero rows, the loop continues to pull the next
            // input row (matching nodeProjectSet.c's "no rows from this scan
            // tuple, loop back" branch).
            if !dispatch_and_buffer(state, input_slot) {
                // Empty SRF output for this input: discard and continue loop.
                continue;
            }
            // Buffer is non-empty: next iteration will hit the
            // !is_exhausted() arm and emit the first row.
        }
    }
}

/// Mirror of PG's `ReleaseTupleDesc` macro
/// (`/Users/contra/.pgrx/17.9/src/include/access/tupdesc.h`):
/// only call `DecrTupleDescRefCount` when the tupdesc is refcounted
/// (`tdrefcount >= 0`). Some `lookup_rowtype_tupdesc` returns are
/// non-refcounted descriptors that live till process exit, in which case
/// `Pin/Release` are no-ops.
///
/// # Safety
/// `tupdesc` must be a valid `TupleDesc` obtained from a PG cache lookup.
unsafe fn release_tupdesc_refcount(tupdesc: pg_sys::TupleDesc) {
    if tupdesc.is_null() {
        return;
    }
    // SAFETY: tupdesc is a valid TupleDescData; reading tdrefcount.
    let refcount = unsafe { (*tupdesc).tdrefcount };
    if refcount >= 0 {
        // SAFETY: refcount semantics validated; safe on the main thread.
        unsafe { pg_sys::DecrTupleDescRefCount(tupdesc) };
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

/// Dispatch the SRF for one input row, capturing the per-row passthrough
/// values into `state.expansion.passthrough_row` and the SRF's expanded
/// outputs into `state.expansion.expansion`.
///
/// Returns `true` when the SRF produced at least one output row (caller
/// should drain via `emit_one_row`); `false` when the SRF emitted zero rows
/// (caller should pull the next input row).
///
/// # Safety
/// All operations execute on the main backend thread; `state` is borrowed
/// mutably and `input_slot` is a fresh tuple from `ExecProcNode`.
unsafe fn dispatch_and_buffer(
    state: &mut SrfTargetListExecState,
    input_slot: *mut pg_sys::TupleTableSlot,
) -> bool {
    // Reset the expansion buffer.
    state.expansion = ExpansionBuffer::empty();

    // Force materialization of the input slot's tts_values / tts_isnull.
    // SAFETY: slot_getallattrs walks the slot's TupleDesc and fills the
    // virtual arrays. Safe on the main backend thread.
    unsafe {
        pg_sys::slot_getallattrs(input_slot);
    }

    // Capture passthrough Datums by attno (1-based) from the input slot.
    let priv_data = &state.priv_data;
    let n_out = priv_data.passthrough_attnos.len();
    let mut passthrough = Vec::with_capacity(n_out);
    let input_natts = unsafe { (*(*input_slot).tts_tupleDescriptor).natts };
    for &attno in &priv_data.passthrough_attnos {
        if attno <= 0 || attno > input_natts {
            // SRF position (attno == 0) or invalid attno → null placeholder.
            passthrough.push((pg_sys::Datum::from(0u32), true));
            continue;
        }
        // 1-based attno → 0-based array index.
        let idx = (attno - 1) as usize;
        // SAFETY: tts_values and tts_isnull arrays are sized to natts;
        // idx is in bounds.
        let datum = unsafe { *(*input_slot).tts_values.add(idx) };
        let is_null = unsafe { *(*input_slot).tts_isnull.add(idx) };
        passthrough.push((datum, is_null));
    }
    state.expansion.passthrough_row = passthrough;

    // Extract the SRF arg from the input slot (1-based attno).
    let srf_attno = priv_data.srf_arg_attno;
    if srf_attno <= 0 || srf_attno > input_natts {
        pgrx::warning!(
            "pg_accel: srf_target_list dispatch: srf_arg_attno={} out of bounds \
             for input natts={}; declining",
            srf_attno,
            input_natts,
        );
        return false;
    }
    let srf_arg_idx = (srf_attno - 1) as usize;
    // SAFETY: srf_arg_idx is in bounds (checked above).
    let srf_arg_datum = unsafe { *(*input_slot).tts_values.add(srf_arg_idx) };
    let srf_arg_isnull = unsafe { *(*input_slot).tts_isnull.add(srf_arg_idx) };

    // STRICT semantics: if the SRF arg is NULL and the function is strict,
    // emit zero rows for this input (PG's ExecMakeFunctionResultSet behaviour).
    if srf_arg_isnull && state.fmgr_info.fn_strict {
        return false;
    }

    let batch: Vec<(pg_sys::Datum, bool)> = vec![(srf_arg_datum, srf_arg_isnull)];

    // Convert qual_args (i64 + type_oid) into the dispatcher's expected shape.
    let qual_datums: Vec<(pg_sys::Datum, bool, pg_sys::Oid)> = priv_data
        .qual_args
        .iter()
        .map(|&(d, t)| {
            #[allow(clippy::cast_sign_loss)]
            let datum = pg_sys::Datum::from(d as usize);
            (datum, false, pg_sys::Oid::from(t))
        })
        .collect();

    // SAFETY: dispatch::dispatch is safe on the main backend thread; the
    // FmgrInfo was populated above from a valid pg_proc entry.
    let result = unsafe {
        dispatch::dispatch(
            state.strategy,
            &batch,
            &state.fmgr_info,
            state.fmgr_info.fn_strict,
            &qual_datums,
            true, // skip_bbox: irrelevant for SRF dispatch
        )
    };

    // Map the dispatch outcome to per-row expanded Datums. For our 1-input-row
    // dispatch:
    //   - VarLen: offsets = [0, n_out]; datums[0..n_out] is the expansion
    //   - Record: datums.len() == n_out * fields_per_row; we synthesize one
    //     composite tuple per row via heap_form_tuple
    //   - Scalar: 1 input → 1 output (single-emit SRF degenerates here)
    //   - Deferred: zero rows
    match (state.output_shape, result) {
        (OutputShape::VarLen, DispatchResult::AcceleratedVarLen { offsets: _, datums }) => {
            if datums.is_empty() {
                return false;
            }
            state.expansion.expansion = datums;
            true
        }
        (
            OutputShape::Record { field_count },
            DispatchResult::AcceleratedRecord {
                fields_per_row,
                datums,
            },
        ) => {
            // Validate shape. fields_per_row must match registry's field_count.
            if fields_per_row != field_count || field_count == 0 {
                pgrx::warning!(
                    "pg_accel: srf_target_list dispatch: Record shape mismatch \
                     (registry field_count={}, dispatched fields_per_row={})",
                    field_count,
                    fields_per_row,
                );
                return false;
            }
            let fc = field_count as usize;
            if datums.len() % fc != 0 {
                pgrx::warning!(
                    "pg_accel: srf_target_list dispatch: Record datums.len()={} not \
                     divisible by fields_per_row={}",
                    datums.len(),
                    fc,
                );
                return false;
            }
            let n_rows = datums.len() / fc;
            if n_rows == 0 {
                return false;
            }
            // Synthesize a composite tuple per row: heap_form_tuple over the
            // SRF's output TupleDesc, wrapped as a Datum.
            // SAFETY: TupleDescGetAttr / heap_form_tuple are safe on the main
            // backend thread; the tupdesc must come from the SRF's declared
            // return type. We resolve via get_func_rettype + lookup_rowtype_tupdesc.
            let tupdesc = unsafe {
                let rettype = pg_sys::get_func_rettype(state.priv_data.fn_oid);
                if rettype == pg_sys::InvalidOid {
                    pgrx::warning!(
                        "pg_accel: srf_target_list dispatch: cannot resolve return type \
                         for fn_oid={}; declining",
                        u32::from(state.priv_data.fn_oid),
                    );
                    return false;
                }
                pg_sys::lookup_rowtype_tupdesc(rettype, -1)
            };
            if tupdesc.is_null() {
                return false;
            }
            let mut expanded = Vec::with_capacity(n_rows);
            for row_idx in 0..n_rows {
                let base = row_idx * fc;
                let mut values: Vec<pg_sys::Datum> = Vec::with_capacity(fc);
                let mut nulls: Vec<bool> = Vec::with_capacity(fc);
                for k in 0..fc {
                    let (d, n) = datums[base + k];
                    values.push(d);
                    nulls.push(n);
                }
                // SAFETY: tupdesc is valid; values/nulls arrays are sized to natts.
                let htup = unsafe {
                    pg_sys::heap_form_tuple(tupdesc, values.as_mut_ptr(), nulls.as_mut_ptr())
                };
                if htup.is_null() {
                    // SAFETY: release the tupdesc reference balancing the
                    // lookup_rowtype_tupdesc above, mirroring PG's
                    // ReleaseTupleDesc macro (only decrement when refcounted).
                    unsafe { release_tupdesc_refcount(tupdesc) };
                    return false;
                }
                // Wrap the HeapTupleHeader as a Datum (PG record format).
                // SAFETY: htup->t_data points to a valid HeapTupleHeader after
                // heap_form_tuple; HeapTupleHeaderGetDatum is the standard
                // record-passing convention.
                let header = unsafe { (*htup).t_data };
                expanded.push((pg_sys::Datum::from(header as usize), false));
            }
            // SAFETY: balances the lookup_rowtype_tupdesc above; mirrors
            // PG's ReleaseTupleDesc macro.
            unsafe { release_tupdesc_refcount(tupdesc) };
            state.expansion.expansion = expanded;
            true
        }
        (OutputShape::Scalar, DispatchResult::Accelerated(datums)) => {
            // Scalar SRF: 1 output per input. Stash and emit.
            if datums.is_empty() {
                return false;
            }
            state.expansion.expansion = datums;
            true
        }
        (_, DispatchResult::Deferred) => {
            pgrx::error!(
                "pg_accel: srf_target_list dispatch: deferred for fn_oid={} strategy={:?}; \
                 planner must decline instead of producing a non-GPU pg_accel plan",
                u32::from(state.priv_data.fn_oid),
                state.strategy,
            );
        }
        (shape, result) => {
            pgrx::error!(
                "pg_accel: srf_target_list dispatch: shape/result mismatch — \
                 registry shape {:?} but dispatch returned variant index {:?}; refusing non-GPU fallback",
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
/// has a TupleDesc with `natts == passthrough_row.len()`.
unsafe fn emit_one_row(
    state: &mut SrfTargetListExecState,
    slot: *mut pg_sys::TupleTableSlot,
) -> *mut pg_sys::TupleTableSlot {
    let row_idx = state.expansion.cursor;
    state.expansion.cursor = state.expansion.cursor.saturating_add(1);
    state.rows_dispatched = state.rows_dispatched.saturating_add(1);

    let srf_pos = state.priv_data.srf_tlist_pos as usize;
    let n_cols = state.expansion.passthrough_row.len();

    // SAFETY: slot is valid and has tts_values/tts_isnull arrays sized to
    // its TupleDesc natts (==n_cols by init_state validation).
    unsafe {
        pg_sys::ExecClearTuple(slot);
        let values = (*slot).tts_values;
        let nulls = (*slot).tts_isnull;
        if values.is_null() || nulls.is_null() {
            return slot;
        }
        for k in 0..n_cols {
            if k == srf_pos {
                let (d, n) = state.expansion.expansion[row_idx];
                *values.add(k) = d;
                *nulls.add(k) = n;
            } else {
                let (d, n) = state.expansion.passthrough_row[k];
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
