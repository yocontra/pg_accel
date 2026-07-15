//! FunctionScan executor state (Phase 2 F3).
//!
//! Owns the per-invocation cursor and emitted-Datum buffer for a Custom
//! Scan node. Its planner injector (`planner_hooks::projectset`) was deleted
//! in the 2026-07 Phase 3 demolition, so this machinery is currently
//! planner-unreachable; it is retained for the Phase 6/9 SRF revival.
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
//! The scan slot's TupleDesc is built from the registry's concrete
//! `output_field_types` / `output_field_names`.
//!
//! For `OutputShape::Record` (e.g. `ST_SummaryStats` → 6 fields) the slot
//! holds one composite tuple per emitted row; per CLAUDE.md "Custom Scan
//! has THREE vtables" we never construct a record TupleDesc separate from
//! the scan slot's — the scan slot directly carries the multi-column
//! descriptor.

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

/// Initialise the FunctionScan executor state from the plan-priv payload.
///
/// Reads the `FUNCTIONSCAN_SENTINEL`-prefixed block from `cscan.custom_private`,
/// looks up the registry entry, builds a TupleDesc for the scan slot,
/// dispatches the call once, and stashes the buffered output. Returns a
/// non-null raw pointer suitable for `(*state).accel.executor`; selected-plan
/// setup failures are PostgreSQL errors.
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
        pgrx::error!("pg_accel: function_scan init: missing validated custom_private");
    }

    // The plan's custom_private layout is:
    //   [strategy=FunctionScan, batch_size, expected_threads, fn_oid_placeholder=0,
    //    target_attno_placeholder=0, accel_strategy_hint, FUNCTIONSCAN_SENTINEL, ...]
    // The FSCA block starts at index 6 (after the standard 6-element header).
    // SAFETY: deserialize_functionscan_priv validates sentinel + length.
    let priv_data = unsafe { deserialize_functionscan_priv(priv_list, 6) }.unwrap_or_else(|| {
        pgrx::error!(
            "pg_accel: function_scan init: validated private payload failed strict re-decode"
        )
    });

    // Look up the registry entry to get the strategy + field metadata.
    registry::lazy_init();
    let entry = registry::global_registry()
        .lookup(priv_data.fn_oid)
        .unwrap_or_else(|| {
            pgrx::error!(
                "pg_accel: function_scan init: selected function OID {} is not registered",
                u32::from(priv_data.fn_oid),
            )
        });

    // The scan slot's TupleDesc is set up by `ExecInitCustomScan`
    // (`src/backend/executor/nodeCustom.c:ExecInitScanTupleSlot`) from our
    // `custom_scan_tlist` (mirrored from the planner-supplied tlist) via
    // `ExecTypeFromTL`. The slot is created with a non-NULL tupdesc, so
    // `MakeTupleTableSlot` sets `TTS_FLAG_FIXED`
    // (`src/backend/executor/execTuples.c:MakeTupleTableSlot`). Calling
    // `ExecSetSlotDescriptor` on a fixed slot triggers
    // `Assert(!TTS_FIXED(slot))` in debug builds and pfrees+reallocates
    // tts_values/tts_isnull that are co-allocated with the slot itself
    // (corrupting heap on release). Either way, mutating the slot's
    // descriptor at BeginCustomScan time is forbidden.
    //
    // Instead we validate the slot's existing descriptor against the
    // registry's expected shape and error on mismatch — PG's
    // `ExecTypeFromTL(custom_scan_tlist)` already produced the correct
    // column count + type OIDs because `custom_scan_tlist` mirrors the
    // parser-resolved tlist for the SRF (each column expanded from the
    // function's declared output type).
    //
    // SAFETY: `node` is a valid CustomScanState; `ss_ScanTupleSlot` and
    // its `tts_tupleDescriptor` were initialised by ExecInitCustomScan on
    // the main backend thread before BeginCustomScan fires.
    unsafe {
        let scan_slot = (*node).ss.ss_ScanTupleSlot;
        if scan_slot.is_null() {
            pgrx::error!("pg_accel: function_scan init: ss_ScanTupleSlot is null");
        }
        let slot_tupdesc = (*scan_slot).tts_tupleDescriptor;
        if slot_tupdesc.is_null() {
            pgrx::error!(
                "pg_accel: function_scan init: scan slot has null tupdesc; \
                 ExecInitCustomScan should have populated it from custom_scan_tlist"
            );
        }
        let slot_natts = (*slot_tupdesc).natts;
        let expected_natts = i32::try_from(entry.output_field_types.len()).unwrap_or_else(|_| {
            pgrx::error!("pg_accel: function_scan registry output column count exceeds i32")
        });
        if slot_natts != expected_natts {
            pgrx::error!(
                "pg_accel: function_scan init: scan slot natts={} but registry expects {} \
                 columns for fn_oid={} (planner-tlist mismatch)",
                slot_natts,
                expected_natts,
                u32::from(priv_data.fn_oid),
            );
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
            pgrx::error!(
                "pg_accel: function_scan init: dispatch deferred for fn_oid={} strategy={:?}; \
                 planner must decline instead of producing a non-GPU pg_accel plan",
                u32::from(priv_data.fn_oid),
                entry.strategy,
            );
        }
        (shape, result) => {
            pgrx::error!(
                "pg_accel: function_scan init: shape/result mismatch — \
                 registry shape {:?} but dispatch returned variant index {:?}; refusing non-GPU fallback",
                shape,
                std::mem::discriminant(&result),
            );
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
    if OutputShapeDisc::from_i32(priv_data.output_shape_disc).is_none() {
        pgrx::error!(
            "pg_accel: invalid FunctionScan output shape tag {}",
            priv_data.output_shape_disc
        );
    }
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
    // `qual_datums`. Planner hooks reject NULL Const args until the private
    // data format carries const nullness explicitly.
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
    // SAFETY: node is a valid CustomScanState; ss_ScanTupleSlot was set up
    // by ExecInitCustomScan and may have had its descriptor swapped during
    // init_state.
    let slot = unsafe { (*node).ss.ss_ScanTupleSlot };
    if slot.is_null() {
        pgrx::error!("pg_accel: FunctionScan executor has no scan tuple slot");
    }
    if executor.is_null() {
        pgrx::error!("pg_accel: FunctionScan executor state is null");
    }
    // SAFETY: executor was Box::into_raw'd as FunctionScanExecState in
    // init_state. We reborrow as &mut to advance the cursor.
    let state = unsafe { &mut *executor.cast::<FunctionScanExecState>() };

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
            // Multi-column record output. PG's `ExecInitCustomScan` builds the
            // scan slot as a virtual slot (TTSOpsVirtual) — heap-tuple storage
            // would require a TTSOpsHeapTuple slot and `ExecStoreHeapTuple`
            // would `elog(ERROR, "trying to store a heap tuple into wrong type
            // of slot")` (`src/backend/executor/execTuples.c::ExecStoreHeapTuple`),
            // which historically aborted multi-cell h3_cells_to_multi_polygon.
            // Populate `tts_values` / `tts_isnull` directly and promote with
            // `ExecStoreVirtualTuple` — same pattern the Scalar / VarLen arms
            // use above.
            let fc = *field_count as usize;
            let base = row_idx.saturating_mul(fc);
            // SAFETY: slot is a valid TupleTableSlot; the tts_values /
            // tts_isnull arrays were sized for `fc` attrs by the slot's
            // tupdesc (set up at scan init from `output_field_types`).
            unsafe {
                pg_sys::ExecClearTuple(slot);
                let values = (*slot).tts_values;
                let nulls = (*slot).tts_isnull;
                if values.is_null() || nulls.is_null() {
                    return slot;
                }
                for k in 0..fc {
                    let (d, n) = datums[base + k];
                    *values.add(k) = d;
                    *nulls.add(k) = n;
                }
                pg_sys::ExecStoreVirtualTuple(slot)
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
// supporting SQL extension (h3 / postgis_raster) is `CREATE EXTENSION`-d
// inside the test function (the pgrx_tests harness does not pre-install
// adapters in the test DB).
//
// **Registry-init ordering: resolved 2026-05-02.** `lazy_init` still fires
// once per backend (typically on a SELECT inside the CREATE EXTENSION
// script, before h3 is fully loaded). What changed is that
// `AdapterRegistry::lookup` now auto-retries via
// `registry::resolve_oids_again()` on a miss (see
// `pg_accel/src/engine/registry.rs:374`), so the projectset hook's lookup
// now sees the freshly-installed h3 OIDs the second time around within
// the same planner pass.
//
// **Resolved 2026-05-03: GPU FunctionScan executor crash.** The
// `ExecSetSlotDescriptor` call site at the previous line 279 raised a
// PG `ereport(ERROR)` because `ExecInitCustomScan`
// (`src/backend/executor/nodeCustom.c`) builds the scan slot from
// `ExecTypeFromTL(custom_scan_tlist)` and `MakeTupleTableSlot`
// (`src/backend/executor/execTuples.c`) marks any slot built with a
// non-NULL tupdesc as `TTS_FLAG_FIXED`; mutating a fixed slot's
// descriptor from `BeginCustomScan` is forbidden. Fix applied in
// `plan_custom_path_function`: build `custom_scan_tlist` from the
// registry's `output_field_types`/`output_field_names` so PG's
// `ExecTypeFromTL` produces the correct descriptor on first
// construction, eliminating the need for any `BeginCustomScan`-side
// patch. The remaining `init_state` flow only validates that the slot
// descriptor matches the registry's expected column count and bails
// out cleanly on mismatch.

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::{Spi, pg_test};

    use crate::gpu;

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

    fn explain_text(query: &str) -> String {
        Spi::connect(|client| {
            let mut lines = Vec::new();
            let sql = format!("EXPLAIN (FORMAT TEXT) {query}");
            let table = client
                .select(&sql, None, &[])
                .expect("EXPLAIN query should succeed");
            for row in table {
                if let Some(line) = row.get::<String>(1).ok().flatten() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        })
    }

    fn assert_native_h3_plan(plan: &str, operation: &str) {
        assert!(!plan.is_empty(), "{operation} EXPLAIN returned no rows");
        assert!(
            !plan.contains("GpuFunctionScan")
                && !plan.contains("GpuAccelFunctionScan")
                && !plan.contains("Strategy: GpuH3"),
            "quarantined H3 topology op `{operation}` selected a GPU plan:\n{plan}",
        );
    }

    /// Boundary topology stays on h3-pg and produces an exact six-vertex
    /// polygon without incrementing pg_accel's kernel counter.
    #[pg_test]
    fn h3_cell_to_boundary_uses_native_plan_without_dispatch() {
        if !ensure_extension("h3") {
            return;
        }

        Spi::run("SELECT h3_get_resolution('8a2a1072b59ffff'::h3index)").expect("h3 ping");

        let query = "SELECT npoints(h3_cell_to_boundary('8a2a1072b59ffff'::h3index))";
        assert_native_h3_plan(&explain_text(query), "h3_cell_to_boundary");
        gpu::reset_gpu_exec_count();
        let npoints = Spi::get_one::<i32>(query)
            .expect("native h3_cell_to_boundary query should succeed")
            .expect("native boundary should not be NULL");
        assert_eq!(npoints, 6, "native H3 hex boundary must have six vertices");
        assert_eq!(
            gpu::gpu_exec_count(),
            0,
            "native h3_cell_to_boundary must record zero pg_accel dispatches",
        );
    }

    /// Grid traversal stays on h3-pg. The native k=1 disk has seven distinct
    /// cells, includes its origin, and records no pg_accel dispatch.
    #[pg_test]
    fn h3_grid_disk_uses_native_plan_without_dispatch() {
        if !ensure_extension("h3") {
            return;
        }

        Spi::run("SELECT h3_get_resolution('8928308280fffff'::h3index)").expect("h3 ping");
        let query = "SELECT count(*) FROM h3_grid_disk('8928308280fffff'::h3index, 1)";
        assert_native_h3_plan(&explain_text(query), "h3_grid_disk");
        gpu::reset_gpu_exec_count();
        let count = Spi::get_one::<i64>(query)
            .expect("native h3_grid_disk count should succeed")
            .expect("native h3_grid_disk count should not be NULL");
        let distinct = Spi::get_one::<i64>(
            "SELECT count(DISTINCT cell) FROM \
             h3_grid_disk('8928308280fffff'::h3index, 1) AS cell",
        )
        .expect("native distinct count should succeed")
        .expect("native distinct count should not be NULL");
        let contains_origin = Spi::get_one::<bool>(
            "SELECT bool_or(cell = '8928308280fffff'::h3index) FROM \
             h3_grid_disk('8928308280fffff'::h3index, 1) AS cell",
        )
        .expect("native origin membership should succeed")
        .expect("native origin membership should not be NULL");
        assert_eq!(count, 7, "native k=1 grid disk must contain seven cells");
        assert_eq!(distinct, 7, "native k=1 grid disk cells must be distinct");
        assert!(
            contains_origin,
            "native k=1 grid disk must include its origin"
        );
        assert_eq!(
            gpu::gpu_exec_count(),
            0,
            "native h3_grid_disk must record zero pg_accel dispatches",
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

    /// Repeated planning must keep the same native decline for quarantined
    /// topology functions; registry retries must not re-admit the function.
    #[pg_test]
    fn h3_topology_native_decline_is_stable_across_replanning() {
        if !ensure_extension("h3") {
            return;
        }
        Spi::run("SELECT h3_get_resolution('8a2a1072b59ffff'::h3index)").expect("h3 ping");

        let query = "SELECT count(*) FROM \
                     h3_cell_to_boundary('8a2a1072b59ffff'::h3index)";
        let first = explain_text(query);
        let second = explain_text(query);
        assert_native_h3_plan(&first, "h3_cell_to_boundary first plan");
        assert_native_h3_plan(&second, "h3_cell_to_boundary second plan");
        assert_eq!(first, second, "native topology decline plan must be stable");
    }
}
