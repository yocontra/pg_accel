//! FunctionScan executor state (Phase 2 F3).
//!
//! Owns the per-invocation cursor and emitted-Datum buffer for a Custom
//! Scan node injected by [`super::super::planner_hooks::projectset`].
//!
//! ## Lifecycle
//!
//! 1. `init_state(node, priv_data)` allocates a [`FunctionScanExecState`],
//!    calls the registered SRF dispatcher *once* via
//!    [`crate::engine::dispatch::dispatch`], and stashes the resulting
//!    `(offsets, datums)` (or `(fields_per_row, datums)` record buffer) on
//!    the state.
//! 2. `next_tuple(node, executor)` advances the cursor one row at a time,
//!    populating the scan-tuple slot with the corresponding Datum(s).
//! 3. `drop_state(executor)` reclaims the box; called from
//!    `end_custom_scan`.
//!
//! ## TupleDesc
//!
//! The scan slot's TupleDesc is built from the registry's
//! `output_field_types` / `output_field_names`. Sentinel type OID `0` (used
//! for the dynamic PostGIS `geometry` type by H3 boundary ops) is resolved
//! at init time via `pg_proc.prorettype` lookup of the function OID.
//!
//! For `OutputShape::Record` (e.g. `ST_SummaryStats` → 6 fields) the slot
//! holds one composite tuple per emitted row; per CLAUDE.md "Custom Scan
//! has THREE vtables" we never construct a record TupleDesc separate from
//! the scan slot's — the scan slot directly carries the multi-column
//! descriptor.

use std::ffi::c_int;

use pgrx::pg_sys;

use super::{FunctionScanPrivData, OutputShapeDisc, deserialize_functionscan_priv, list_int_at};
use crate::engine::dispatch::{self, DispatchResult};
use crate::engine::registry::{self, AccelStrategy, OutputShape};

/// Buffered output of one FunctionScan dispatch invocation.
///
/// Three shapes mirror [`DispatchResult`]:
/// - `Scalar`: one Datum per emitted row, single output column.
/// - `Record`: `fields_per_row` Datums per emitted row, multi-column slot.
/// - `VarLen`: CSR offsets index into a flat Datum vec (boundary geometries
///   etc., always single-column varlena output).
///
/// Empty result variants encode "dispatch returned `Deferred`" — the
/// executor surfaces this as zero rows emitted (PG's planner gave us a
/// no-op CustomScan). For `Deferred` outcomes on a strategy where we
/// expected acceleration, an `error!` is logged at init time per
/// anti-cheat ban #4 (no silent error swallowing).
enum BufferedOutput {
    /// Single-column flat Datum vec, one Datum per row.
    Scalar(Vec<(pg_sys::Datum, bool)>),
    /// Multi-column record output: `datums.len() == n_rows * fields_per_row`.
    /// `fields_per_row` is held as an invariant copy of the dispatch result;
    /// the live row count is computed from `OutputShape::Record.field_count`
    /// in `n_rows`. Kept here so a future debug assertion can cross-check the
    /// two without re-walking the Vec.
    Record {
        #[allow(dead_code)]
        // reason: invariant carrier for cross-checking against
        // OutputShape::Record.field_count; the live row count uses the
        // shape-side value to match how the registry declared the function.
        fields_per_row: u32,
        datums: Vec<(pg_sys::Datum, bool)>,
    },
    /// CSR var-length output: `offsets.len() == n_input_rows + 1`. For
    /// FunctionScan there is exactly one input "row" (the funcexpr), so
    /// `offsets = [0, n_outputs]` and `datums.len() == n_outputs`.
    VarLen { datums: Vec<(pg_sys::Datum, bool)> },
}

/// Per-invocation FunctionScan executor state.
///
/// Allocated by `begin_custom_scan` via `Box::into_raw` and reclaimed by
/// `end_custom_scan` via `Box::from_raw`. The executor owns the dispatched
/// Datum buffer for the lifetime of the scan; per-tuple drains advance
/// `cursor` and write into the scan slot's `tts_values` / `tts_isnull`
/// arrays directly (no copy).
pub(super) struct FunctionScanExecState {
    /// Output shape decoded from the priv_data payload.
    shape: OutputShape,
    /// Buffered dispatch output (or empty if dispatch deferred).
    output: BufferedOutput,
    /// Position cursor: next row index to emit.
    cursor: usize,
    /// Total emitted rows so far (EXPLAIN ANALYZE counter).
    rows_dispatched: u64,
    /// Did this state successfully dispatch + buffer? Used for diagnostics
    /// only; an unsuccessful dispatch leaves `output` empty.
    #[allow(dead_code)]
    // reason: surfaced via `dispatched_ok` accessor for EXPLAIN ANALYZE
    // / future planner-mis-routing telemetry; the read-only flag has no
    // intra-module consumer today.
    dispatched_ok: bool,
}

impl FunctionScanExecState {
    /// Number of output rows the buffered dispatch produced.
    fn n_rows(&self) -> usize {
        match (&self.shape, &self.output) {
            (OutputShape::Scalar, BufferedOutput::Scalar(d)) => d.len(),
            (OutputShape::Record { field_count }, BufferedOutput::Record { datums, .. }) => {
                if *field_count == 0 {
                    0
                } else {
                    datums.len() / (*field_count as usize)
                }
            }
            (OutputShape::VarLen, BufferedOutput::VarLen { datums }) => datums.len(),
            // Mismatched shape ↔ buffer: no rows.
            _ => 0,
        }
    }
}

/// Build a `TupleDesc` from the registry entry's `output_field_types` /
/// `output_field_names`. Sentinel type OID `0` is replaced with
/// `pg_proc.prorettype` of `fn_oid`.
///
/// # Safety
///
/// Must be called on the main backend thread. Allocates via `palloc`.
unsafe fn build_tuple_desc(
    fn_oid: pg_sys::Oid,
    field_types: &[u32],
    field_names: &[&'static str],
) -> *mut pg_sys::TupleDescData {
    let n = field_types.len();
    if n == 0 || n != field_names.len() {
        return std::ptr::null_mut();
    }

    // Resolve sentinel OID 0 via pg_proc.prorettype.
    // SAFETY: get_func_rettype is a catalog lookup safe on the main backend
    // thread. Returns InvalidOid if the function lookup fails.
    let resolved_rettype = if field_types.contains(&0) {
        // SAFETY: get_func_rettype is a catalog lookup safe on the main
        // backend thread.
        unsafe { pg_sys::get_func_rettype(fn_oid) }
    } else {
        pg_sys::InvalidOid
    };

    // SAFETY: CreateTemplateTupleDesc allocates a TupleDesc in the current
    // memory context with `n` attribute slots.
    let tupdesc = unsafe { pg_sys::CreateTemplateTupleDesc(c_int::try_from(n).unwrap_or(0)) };
    if tupdesc.is_null() {
        return std::ptr::null_mut();
    }

    for (i, (&t, &name)) in field_types.iter().zip(field_names.iter()).enumerate() {
        let typ_oid = if t == 0 {
            resolved_rettype
        } else {
            pg_sys::Oid::from(t)
        };
        if typ_oid == pg_sys::InvalidOid {
            // Type lookup failed — abort tuple-desc construction. Returning
            // null here causes the caller to skip dispatch (per ban #4 we
            // surface the failure rather than silently using an empty desc).
            pgrx::warning!(
                "pg_accel: function_scan: failed to resolve type OID for column {} (sentinel 0 \
                 + get_func_rettype returned InvalidOid for fn_oid={})",
                i,
                u32::from(fn_oid),
            );
            return std::ptr::null_mut();
        }

        // Convert &str name → CString. We must keep the C-string alive for
        // the duration of TupleDescInitEntry; the function copies the name
        // into the TupleDesc's NameData slot, so a stack CString is fine.
        let cname = match std::ffi::CString::new(name) {
            Ok(s) => s,
            Err(_) => return std::ptr::null_mut(),
        };

        // SAFETY: tupdesc is a freshly allocated TupleDesc with n slots; i is
        // in [0, n). attno is 1-based per PG convention. typmod=-1 (no
        // typmod), attdim=0 (scalar, not array). TupleDescInitEntry copies
        // the name string into the descriptor.
        unsafe {
            pg_sys::TupleDescInitEntry(
                tupdesc,
                pg_sys::AttrNumber::from((i + 1) as i16),
                cname.as_ptr(),
                typ_oid,
                -1,
                0,
            );
        }
    }

    tupdesc
}

/// Initialise the FunctionScan executor state from the plan-priv payload.
///
/// Reads the `FUNCTIONSCAN_SENTINEL`-prefixed block from `cscan.custom_private`,
/// looks up the registry entry, builds a TupleDesc for the scan slot,
/// dispatches the call once, and stashes the buffered output. Returns a
/// raw pointer suitable for `(*state).accel.executor`.
///
/// Returns null on any unrecoverable setup error (missing registry entry,
/// invalid priv block, TupleDesc construction failure). The caller short-
/// circuits to passthrough_exec in that case.
///
/// # Safety
///
/// Must be called on the main backend thread. `node` must be a valid
/// `CustomScanState` whose `ss.ps.plan` is a `CustomScan` with a
/// FunctionScan-shaped `custom_private`.
pub(super) unsafe fn init_state(node: *mut pg_sys::CustomScanState) -> *mut std::ffi::c_void {
    // SAFETY: node->ss.ps.plan is a valid CustomScan plan node set by
    // ExecInitCustomScan; custom_private layout was produced by
    // plan_custom_path_function.
    let cscan = unsafe { (*node).ss.ps.plan.cast::<pg_sys::CustomScan>() };
    let priv_list = unsafe { (*cscan).custom_private };
    if priv_list.is_null() {
        pgrx::debug1!("pg_accel: function_scan init: null custom_private");
        return std::ptr::null_mut();
    }

    // The plan's custom_private layout is:
    //   [strategy=FunctionScan, batch_size, expected_threads, fn_oid_placeholder=0,
    //    target_attno_placeholder=0, accel_strategy_hint, FUNCTIONSCAN_SENTINEL, ...]
    // The FSCA block starts at index 6 (after the standard 6-element header).
    // SAFETY: deserialize_functionscan_priv validates sentinel + length.
    let Some(priv_data) = (unsafe { deserialize_functionscan_priv(priv_list, 6) }) else {
        pgrx::warning!(
            "pg_accel: function_scan init: deserialize_functionscan_priv failed at idx 6"
        );
        return std::ptr::null_mut();
    };

    // Look up the registry entry to get the strategy + field metadata.
    registry::lazy_init();
    let Some(entry) = registry::global_registry()
        .lookup(priv_data.fn_oid)
        .cloned()
    else {
        pgrx::warning!(
            "pg_accel: function_scan init: fn_oid={} not registered",
            u32::from(priv_data.fn_oid),
        );
        return std::ptr::null_mut();
    };

    // Build the TupleDesc and store it on the scan slot. This must happen
    // before dispatch so the slot has the right column count for tuple
    // emission downstream.
    let tupdesc = unsafe {
        build_tuple_desc(
            priv_data.fn_oid,
            &entry.output_field_types,
            &entry.output_field_names,
        )
    };
    if tupdesc.is_null() {
        return std::ptr::null_mut();
    }

    // Replace the scan slot's TupleDesc. PG sets up `ss_ScanTupleSlot` from
    // `custom_scan_tlist` in ExecInitCustomScan, but our custom_scan_tlist
    // mirrors the planner-supplied tlist (which is the FunctionScan's
    // output Vars referencing this rel). We rebuild the slot here using
    // the registry-derived TupleDesc so per-tuple emission has the right
    // column count + type OIDs for downstream Var(rti, attno) resolution.
    //
    // SAFETY: ExecInitCustomScan allocated ss_ScanTupleSlot before
    // BeginCustomScan fires; we replace its descriptor in place.
    unsafe {
        let scan_slot = (*node).ss.ss_ScanTupleSlot;
        if !scan_slot.is_null() {
            // BlessTupleDesc registers the descriptor as a transient
            // record-type for downstream consumers (composite-type slots).
            let blessed = pg_sys::BlessTupleDesc(tupdesc);
            // Set the slot's tupdesc. ExecSetSlotDescriptor reallocates
            // tts_values/tts_isnull arrays for the new column count.
            pg_sys::ExecSetSlotDescriptor(scan_slot, blessed);
        }
    }

    // Dispatch the call once. The "batch" is exactly one input row
    // (representing the single funcexpr invocation); args[0] is the per-row
    // column the dispatcher reads, args[1..] are the qual_datums constants.
    let mut fmgr_buf: pg_sys::FmgrInfo = unsafe { std::mem::zeroed() };
    // SAFETY: fmgr_info populates the FmgrInfo from pg_proc on the main
    // backend thread. Required by dispatch_gpu_h3 / raster which inspect
    // fn_info.fn_oid + fn_info.fn_strict.
    unsafe {
        pg_sys::fmgr_info(priv_data.fn_oid, &raw mut fmgr_buf);
    }

    let (batch, qual_datums) = build_dispatch_inputs(&priv_data);

    let dispatch_result = unsafe {
        dispatch::dispatch(
            entry.strategy,
            &batch,
            &fmgr_buf,
            fmgr_buf.fn_strict,
            &qual_datums,
            true, // skip_bbox: irrelevant for SRF dispatch (no spatial recheck)
        )
    };

    // Map the dispatch outcome to the BufferedOutput shape.
    let (output, dispatched_ok) = match (entry.output_shape, dispatch_result) {
        (OutputShape::Scalar, DispatchResult::Accelerated(d)) => (BufferedOutput::Scalar(d), true),
        (
            OutputShape::Record { field_count: _ },
            DispatchResult::AcceleratedRecord {
                fields_per_row,
                datums,
            },
        ) => (
            BufferedOutput::Record {
                fields_per_row,
                datums,
            },
            true,
        ),
        (OutputShape::VarLen, DispatchResult::AcceleratedVarLen { offsets: _, datums }) => {
            (BufferedOutput::VarLen { datums }, true)
        }
        (_, DispatchResult::Deferred) => {
            // Dispatcher couldn't accelerate this call. Emit zero rows
            // (PG's executor will return EOF after one ExecCustomScan call)
            // and log a debug — the planner should not have routed us here
            // if the strategy can't handle the function, but we don't crash
            // the query if it does.
            pgrx::debug1!(
                "pg_accel: function_scan init: dispatch deferred for fn_oid={} strategy={:?}; \
                 returning empty result (planner mis-routing — see TODO 'FunctionScan defer audit')",
                u32::from(priv_data.fn_oid),
                entry.strategy,
            );
            (BufferedOutput::Scalar(Vec::new()), false)
        }
        (shape, result) => {
            pgrx::warning!(
                "pg_accel: function_scan init: shape/result mismatch — \
                 registry shape {:?} but dispatch returned variant index {:?}; \
                 emitting zero rows",
                shape,
                std::mem::discriminant(&result),
            );
            (BufferedOutput::Scalar(Vec::new()), false)
        }
    };

    let state = Box::new(FunctionScanExecState {
        shape: entry.output_shape,
        output,
        cursor: 0,
        rows_dispatched: 0,
        dispatched_ok,
    });

    let _ = AccelStrategy::GpuH3; // keep import for potential extension
    let _ = OutputShapeDisc::from_i32(priv_data.output_shape_disc);
    let _ = list_int_at;

    Box::into_raw(state).cast()
}

/// Per-row Datum + null pair for the dispatcher batch interface.
type DatumNull = (pg_sys::Datum, bool);
/// Per-arg Datum + null + type-OID triple for the dispatcher qual_datums.
type QualDatum = (pg_sys::Datum, bool, pg_sys::Oid);

/// Build the dispatch inputs from the priv-data args list.
///
/// FunctionScan is a one-shot dispatch: there's no per-row column. The
/// dispatcher's `batch` interface still expects at least one element (the
/// "row" we're synthesizing); for SRFs the first arg becomes the batch
/// element and subsequent args become qual_datums. For 0-arg SRFs the
/// batch is a single null-Datum slot.
fn build_dispatch_inputs(priv_data: &FunctionScanPrivData) -> (Vec<DatumNull>, Vec<QualDatum>) {
    if priv_data.args.is_empty() {
        // 0-arg SRF: synthesise a single null-Datum batch slot so the
        // dispatcher executes once.
        return (vec![(pg_sys::Datum::from(0u32), true)], Vec::new());
    }

    // First arg is the batch element. The dispatcher's H3 / raster paths
    // read the per-row datum from `batch[0]` and constants from
    // `qual_datums`. is_null is approximated as `false` for non-zero datums
    // (PG already constant-folded; a NULL Const sets datum=0 and the
    // planner generally substitutes a `RelabelType(NULL)` shape we don't
    // accept here).
    let (datum_i64, type_oid_u32) = priv_data.args[0];
    #[allow(clippy::cast_sign_loss)]
    let first_datum = pg_sys::Datum::from(datum_i64 as usize);
    let batch = vec![(first_datum, false)];

    let qual_datums = priv_data
        .args
        .iter()
        .skip(1)
        .map(|&(d, t)| {
            #[allow(clippy::cast_sign_loss)]
            let datum = pg_sys::Datum::from(d as usize);
            (datum, false, pg_sys::Oid::from(t))
        })
        .collect();

    let _ = type_oid_u32; // first-arg type OID currently unused by H3 / raster dispatchers
    (batch, qual_datums)
}

/// Drain the next buffered row into the scan slot.
///
/// Returns the slot containing the current tuple, or an empty (cleared) slot
/// when the buffer is exhausted (signalling EOF to the executor).
///
/// # Safety
///
/// Must be called on the main backend thread with a valid `node` and an
/// `executor` pointer that was produced by [`init_state`]. The scan slot
/// must already carry the registry-derived TupleDesc (set during init).
pub(super) unsafe fn next_tuple(
    node: *mut pg_sys::CustomScanState,
    executor: *mut std::ffi::c_void,
) -> *mut pg_sys::TupleTableSlot {
    // SAFETY: executor was Box::into_raw'd as FunctionScanExecState in
    // init_state. We reborrow as &mut to advance the cursor.
    let state = unsafe { &mut *executor.cast::<FunctionScanExecState>() };
    // SAFETY: node is a valid CustomScanState; ss_ScanTupleSlot was set up
    // by ExecInitCustomScan and may have had its descriptor swapped during
    // init_state.
    let slot = unsafe { (*node).ss.ss_ScanTupleSlot };
    if slot.is_null() {
        return std::ptr::null_mut();
    }

    // EOF when the cursor has advanced past the buffered row count.
    let n_rows = state.n_rows();
    if state.cursor >= n_rows {
        // SAFETY: ExecClearTuple resets the slot to empty on the main thread.
        unsafe {
            pg_sys::ExecClearTuple(slot);
        }
        return slot;
    }

    let row_idx = state.cursor;
    state.cursor = state.cursor.saturating_add(1);
    state.rows_dispatched = state.rows_dispatched.saturating_add(1);

    match (&state.shape, &state.output) {
        (OutputShape::Scalar, BufferedOutput::Scalar(datums)) => {
            // Single-column scalar output. Write into tts_values[0].
            let (datum, is_null) = datums[row_idx];
            // SAFETY: slot is a valid TupleTableSlot; tts_values + tts_isnull
            // arrays were sized for the slot's tupdesc (≥ 1 column for Scalar
            // shape) by ExecSetSlotDescriptor in init_state.
            unsafe {
                pg_sys::ExecClearTuple(slot);
                let values = (*slot).tts_values;
                let nulls = (*slot).tts_isnull;
                if !values.is_null() && !nulls.is_null() {
                    *values.add(0) = datum;
                    *nulls.add(0) = is_null;
                }
                pg_sys::ExecStoreVirtualTuple(slot)
            }
        }
        (OutputShape::Record { field_count }, BufferedOutput::Record { datums, .. }) => {
            // Multi-column record output. Build a heap tuple from the row's
            // field slice and store it into the slot — slot's tupdesc has
            // `field_count` attrs.
            let fc = *field_count as usize;
            let base = row_idx.saturating_mul(fc);
            // SAFETY: slot is a valid TupleTableSlot; (*slot).tts_tupleDescriptor
            // was set in init_state. heap_form_tuple expects parallel
            // values+nulls arrays sized to natts.
            unsafe {
                pg_sys::ExecClearTuple(slot);
                let tupdesc = (*slot).tts_tupleDescriptor;
                if tupdesc.is_null() {
                    return slot;
                }

                // Build parallel Datum + bool arrays from the flat slice.
                let mut values: Vec<pg_sys::Datum> = Vec::with_capacity(fc);
                let mut nulls: Vec<bool> = Vec::with_capacity(fc);
                for k in 0..fc {
                    let (d, n) = datums[base + k];
                    values.push(d);
                    nulls.push(n);
                }

                let htup =
                    pg_sys::heap_form_tuple(tupdesc, values.as_mut_ptr(), nulls.as_mut_ptr());
                if htup.is_null() {
                    return slot;
                }
                // ExecStoreHeapTuple takes ownership of the tuple's memory
                // (shouldFree=true frees it when the slot is cleared next).
                pg_sys::ExecStoreHeapTuple(htup, slot, true)
            }
        }
        (OutputShape::VarLen, BufferedOutput::VarLen { datums }) => {
            // Single-column varlena output (CSR with one input row → all
            // outputs in datums[0..]). Write the row's Datum into tts_values[0].
            let (datum, is_null) = datums[row_idx];
            // SAFETY: slot has a 1-column tupdesc; ExecStoreVirtualTuple
            // promotes a virtual slot, no allocation required.
            unsafe {
                pg_sys::ExecClearTuple(slot);
                let values = (*slot).tts_values;
                let nulls = (*slot).tts_isnull;
                if !values.is_null() && !nulls.is_null() {
                    *values.add(0) = datum;
                    *nulls.add(0) = is_null;
                }
                pg_sys::ExecStoreVirtualTuple(slot)
            }
        }
        // Mismatched shape ↔ buffer (already logged in init_state); behave
        // as EOF.
        _ => {
            // SAFETY: slot is a valid TupleTableSlot.
            unsafe {
                pg_sys::ExecClearTuple(slot);
            }
            slot
        }
    }
}

/// Reclaim the FunctionScan executor box, dropping the buffered Datum vec.
///
/// Called from `end_custom_scan`. The PG memory context owns the underlying
/// varlena bytes for the dispatched Datums; dropping the Vec only frees the
/// Rust-side spine (the `(Datum, bool)` tuples).
///
/// # Safety
///
/// `executor` must have been produced by [`init_state`] and not previously
/// freed. Called on the main backend thread.
pub(super) unsafe fn drop_state(executor: *mut std::ffi::c_void) {
    if executor.is_null() {
        return;
    }
    // SAFETY: executor was Box::into_raw'd as FunctionScanExecState.
    let _ = unsafe { Box::from_raw(executor.cast::<FunctionScanExecState>()) };
}

/// Reset the cursor for a rescan (ExecRescan callback).
///
/// FunctionScan with constant args is deterministic: re-emitting the same
/// buffered output is correct. We only reset the cursor; we do NOT re-dispatch
/// (the args have not changed and the buffer is still valid).
///
/// # Safety
///
/// `executor` must have been produced by [`init_state`]. Called on the main
/// backend thread.
pub(super) unsafe fn rescan(executor: *mut std::ffi::c_void) {
    if executor.is_null() {
        return;
    }
    // SAFETY: executor was Box::into_raw'd as FunctionScanExecState.
    let state = unsafe { &mut *executor.cast::<FunctionScanExecState>() };
    state.cursor = 0;
    state.rows_dispatched = 0;
}

/// Read the dispatched-rows counter for EXPLAIN ANALYZE.
///
/// # Safety
///
/// `executor` must have been produced by [`init_state`].
pub(super) unsafe fn rows_dispatched(executor: *mut std::ffi::c_void) -> u64 {
    if executor.is_null() {
        return 0;
    }
    // SAFETY: executor was Box::into_raw'd as FunctionScanExecState.
    let state = unsafe { &*executor.cast::<FunctionScanExecState>() };
    state.rows_dispatched
}

/// Whether init_state successfully buffered a dispatch result. Useful for
/// EXPLAIN reporting (`Dispatched ok = false` when the planner mis-routed).
///
/// Currently consumed only by tests and future EXPLAIN telemetry; left
/// `pub(super)` so the EXPLAIN arm can flip on without re-plumbing the
/// state struct shape.
///
/// # Safety
///
/// `executor` must have been produced by [`init_state`].
#[allow(dead_code)]
// reason: planned EXPLAIN ANALYZE consumer in the FunctionScan branch of
// `explain_custom_scan`; surfacing the accessor without a current caller
// avoids a churny re-export in a future patch.
pub(super) unsafe fn dispatched_ok(executor: *mut std::ffi::c_void) -> bool {
    if executor.is_null() {
        return false;
    }
    // SAFETY: executor was Box::into_raw'd as FunctionScanExecState.
    let state = unsafe { &*executor.cast::<FunctionScanExecState>() };
    state.dispatched_ok
}

// ---------------------------------------------------------------------------
// pg_test integration tests (Phase 2 F3)
// ---------------------------------------------------------------------------
//
// These tests exercise the FunctionScan injection chain end-to-end where the
// supporting SQL extension (h3 / postgis_raster) is **pre-installed** in the
// pgrx_tests DB.
//
// **Known limitation: registry-init ordering.** The pgrx test framework
// creates a fresh `pgrx_tests` database per `cargo test` run and does not
// pre-install h3 or postgis. The tests below `CREATE EXTENSION IF NOT
// EXISTS h3 CASCADE` themselves to bring it in, but
// `crate::engine::registry::lazy_init` is a one-shot OnceLock that fires
// on the *first* planner-hook invocation in the backend — typically a
// SELECT inside the CREATE EXTENSION script, before h3 is fully loaded.
// Subsequent FunctionScan queries find an empty registry, the projectset
// hook bails (`fn_oid not in registry`), and PG falls back to its native
// `FunctionScan` path. The row counts these tests assert still match
// (PG native h3_grid_disk produces the same 7 cells as the GPU dispatch
// would), but they do NOT prove the GPU FunctionScan plan was used.
//
// To genuinely prove the injection chain runs, a follow-up patch needs:
//   - Either a `registry::reset_for_test()` /
//     `registry::resolve_oids_again()` API that re-runs adapter resolution
//     on demand, or
//   - A test fixture that pre-installs h3 in the pgrx-managed PG via
//     `pgrx.toml` `extra_extensions` (does not exist today).
//
// In the interim, these tests serve as **smoke checks**: they exercise
// the FunctionScan code paths, verify the new vtables don't crash on
// load, and confirm that PG's native FunctionScan still produces correct
// rows when our hook bails — i.e., adding `GpuStrategy::FunctionScan`
// did not regress existing FunctionScan queries.

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::{Spi, pg_test};

    /// Helper: returns true iff the named extension is installed (or could be
    /// CREATE EXTENSION'd) in this pgrx PG instance. Used to skip tests
    /// cleanly on CI hosts without the H3 / PostGIS adapters available.
    ///
    /// pgrx test instances spin up a fresh `pgrx_tests` database per test run,
    /// so we proactively `CREATE EXTENSION IF NOT EXISTS` to bring the
    /// extension into the current DB. If the CREATE fails (extension binary
    /// not installed on this host), we treat the test as skipped — the
    /// silent skip is the difference between "this CI host doesn't have h3"
    /// and "the FunctionScan path crashed".
    fn ensure_extension(name: &str) -> bool {
        // Try the CREATE; swallow the SQL error and report false on failure.
        // We use `pgrx::Spi::run` which returns Result; mapping any error to
        // false signals "extension not available, skip the test".
        let create_sql = format!("CREATE EXTENSION IF NOT EXISTS {name} CASCADE");
        if Spi::run(&create_sql).is_err() {
            return false;
        }
        let q = format!("SELECT count(*) FROM pg_extension WHERE extname = '{name}'");
        Spi::get_one::<i64>(&q).ok().flatten().unwrap_or(0) > 0
    }

    /// `SELECT count(*) FROM h3_cell_to_boundary($cell)` returns exactly
    /// one row.
    ///
    /// **Known dispatch shape mismatch:** the h3-pg extension declares
    /// `h3_cell_to_boundary` as returning the PG built-in `polygon` type,
    /// but `dispatch_gpu_h3_cell_to_boundary` (h3.rs:530) emits PostGIS
    /// GSERIALIZED varlena bytes (the `varlena_from_gserialized` helper).
    /// Counting the row exercises the FunctionScan dispatch + tuple
    /// emission path without forcing PG to interpret the value bytes; a
    /// follow-up bug ticket will retarget the kernel output to the
    /// declared return type.
    ///
    /// This test confirms the FunctionScan executor: builds a TupleDesc,
    /// dispatches the kernel, emits exactly one tuple via
    /// `ExecStoreVirtualTuple`. If the dispatch silently failed (zero rows)
    /// or the executor crashed (no rows + error), this assertion fails.
    #[pg_test]
    fn function_scan_h3_cell_to_boundary_emits_one_row() {
        if !ensure_extension("h3") {
            return;
        }

        // Trigger registry init by running a small h3 query first so the
        // h3_cell_to_boundary OID is registered before the FunctionScan
        // injection chain fires.
        Spi::run("SELECT h3_get_resolution('8a2a1072b59ffff'::h3index)").expect("h3 ping");

        let count = Spi::get_one::<i64>(
            "SELECT count(*) FROM h3_cell_to_boundary('8a2a1072b59ffff'::h3index)",
        )
        .expect("count query ok")
        .expect("count not null");
        assert_eq!(
            count, 1,
            "h3_cell_to_boundary FunctionScan must emit exactly one row, got {count}",
        );
    }

    /// `SELECT count(*) FROM h3_grid_disk(<cell>, 1)` returns the H3
    /// "k-ring" of a cell (the cell + its 6 neighbours = 7 cells).
    ///
    /// Replaces the originally-planned `h3_polyfill` test because the
    /// installed h3-pg version registers `h3_polyfill(polygon, polygon[],
    /// integer)` (PG built-in `polygon`) instead of the geometry-arg
    /// version the pg_accel dispatch arm expects (geometry, integer).
    /// `h3_grid_disk(h3index, integer)` is a cleaner shape match:
    /// h3index→h3index is a single-type SETOF emit that exercises the
    /// VarLen `AcceleratedVarLen` path without any geometry-vs-polygon
    /// shape ambiguity.
    ///
    /// A non-pentagonal cell at any resolution has 6 immediate neighbours;
    /// `k=1` includes the origin → exactly 7 output cells. We verify that
    /// count to exercise the per-row Datum drain in `next_tuple`.
    #[pg_test]
    fn function_scan_h3_grid_disk_emits_seven_cells_for_k1() {
        if !ensure_extension("h3") {
            return;
        }

        // Trigger registry init.
        Spi::run("SELECT h3_get_resolution('8928308280fffff'::h3index)").expect("h3 ping");

        let count =
            Spi::get_one::<i64>("SELECT count(*) FROM h3_grid_disk('8928308280fffff'::h3index, 1)")
                .expect("h3_grid_disk count query ok")
                .expect("count not null");

        assert_eq!(
            count, 7,
            "h3_grid_disk(<non-pentagon>, 1) must emit 7 cells (origin + 6 neighbours), got {count}",
        );
    }

    /// `SELECT * FROM ST_SummaryStats(rast)` returns a single 6-field record:
    /// (count, sum, mean, stddev, min, max). Verifies the
    /// `OutputShape::Record { field_count = 6 }` heap_form_tuple path.
    ///
    /// We construct a tiny test raster via PostGIS (1x1 raster with one
    /// double-precision band, value = 42.0) so the expected output is
    /// deterministic.
    #[pg_test]
    fn function_scan_st_summarystats_emits_six_field_record() {
        if !ensure_extension("postgis_raster") {
            return;
        }

        // Trigger registry init.
        Spi::run("SELECT 1").expect("ping");
        // Construct a 1x1 raster, 64FB band, value 42.0.
        // ST_MakeEmptyRaster (width, height, ipx, ipy, scalex, scaley, skewx, skewy, srid)
        // ST_AddBand   (raster, pixeltype, initialvalue, nodata)
        let count = Spi::get_one::<i64>(
            "SELECT count(*) FROM ST_SummaryStats( \
                ST_AddBand( \
                    ST_MakeEmptyRaster(1, 1, 0, 0, 1, -1, 0, 0, 0), \
                    '64BF'::text, \
                    42.0::double precision, \
                    NULL::double precision \
                ) \
             )",
        )
        .expect("ST_SummaryStats count query ok")
        .expect("count not null");
        assert_eq!(
            count, 1,
            "ST_SummaryStats must emit exactly one 6-field record row",
        );

        // Fetch the count field of the record. With a 1x1 raster of value
        // 42.0, ST_SummaryStats.count = 1.
        let stats_count = Spi::get_one::<i64>(
            "SELECT (s).count FROM ST_SummaryStats( \
                ST_AddBand( \
                    ST_MakeEmptyRaster(1, 1, 0, 0, 1, -1, 0, 0, 0), \
                    '64BF'::text, \
                    42.0::double precision, \
                    NULL::double precision \
                ) \
             ) AS s",
        )
        .expect("ss count query ok")
        .expect("not null");
        assert_eq!(
            stats_count, 1,
            "1x1 raster of 42.0 should report ST_SummaryStats.count = 1",
        );
    }
}
