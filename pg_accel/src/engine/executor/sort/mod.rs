//! Batch-dispatch sort executor for pg_accel Custom Scan nodes.
//!
//! [`SortExecState`] consumes all input tuples, dispatches supported sort keys
//! to GPU kernels, and returns sorted tuples one at a time.
//!
//! The GPU path supports a single numeric key. Unsupported key shapes must be
//! rejected before executor entry; executor mismatches fail closed.
//!
//! # Lifecycle
//!
//! 1. **`begin_custom_scan`** — allocates `SortExecState` with sort keys.
//! 2. **`exec_custom_scan`** (first call) — consumes all input, sorts.
//! 3. **`exec_custom_scan`** (subsequent) — returns sorted tuples.
//! 4. **`end_custom_scan`** — reclaims via `Box::from_raw`.

mod tuplesort;

use pgrx::pg_sys;

use crate::engine::cost;
use crate::engine::executor::sort_scan::SortScan;
use crate::engine::materialize::tuple_extract::{self, AttExtractInfo};
use crate::engine::registry::AccelStrategy;
use crate::engine::stats;
use crate::gpu;

use tuplesort::{
    FLOAT4OID, FLOAT8OID, INT4OID, INT8OID, gpu_sort_chunked_f32, gpu_sort_chunked_f64,
    gpu_sort_chunked_i32, gpu_sort_chunked_i64, gpu_topk_f32, gpu_topk_f64, gpu_topk_i32,
    gpu_topk_i64,
};

/// Maximum LIMIT this executor handles with the bounded GPU top-k path.
///
/// The implementation keeps at most this many rows per relation-level top-k
/// request. Larger LIMIT values remain declined by the planner so they do not
/// silently fall back to full-output GPU sort.
pub const GPU_SORT_TOPK_MAX_LIMIT: usize = 128;

fn sort_key_is_desc(key: &SortKeyDesc) -> bool {
    // Known GT operator OIDs: float4gt(623), float8gt(674), int4gt(521), int8gt(413).
    let sort_op_raw = u32::from(key.sort_op);
    matches!(sort_op_raw, 623 | 674 | 521 | 413)
}

fn topk_tuple_indices(
    key: &SortKeyDesc,
    non_null_indices: &[usize],
    null_indices: &[usize],
    gpu_indices: &[u32],
    n: usize,
    k: usize,
) -> Vec<usize> {
    let limit = k.min(n);
    let mut selected = Vec::with_capacity(limit);

    if key.nulls_first {
        selected.extend(null_indices.iter().copied().take(limit));
    }

    let remaining = limit.saturating_sub(selected.len());
    selected.extend(
        gpu_indices
            .iter()
            .take(remaining)
            .filter_map(|&gi| non_null_indices.get(gi as usize).copied()),
    );

    if !key.nulls_first {
        let remaining = limit.saturating_sub(selected.len());
        selected.extend(null_indices.iter().copied().take(remaining));
    }

    selected
}

// ---------------------------------------------------------------------------
// Sort key descriptor
// ---------------------------------------------------------------------------

/// Describes one sort key column for the executor.
///
/// Serialized into `custom_private` at plan time (as a sequence of
/// `Integer` nodes) and deserialized in `begin_custom_scan`.
#[derive(Debug, Clone)]
pub struct SortKeyDesc {
    /// 1-based attribute number in the tuple descriptor.
    pub attno: pg_sys::AttrNumber,
    /// Ordering operator OID (e.g. `int4lt` for ASC on int4).
    pub sort_op: pg_sys::Oid,
    /// Collation OID for collatable types (0 otherwise).
    pub collation: pg_sys::Oid,
    /// `true` if NULLs should sort before non-NULLs.
    pub nulls_first: bool,
}

/// Number of `Integer` nodes per sort key in `custom_private`.
/// Layout: [attno, sort_op, collation, nulls_first_as_0_or_1].
pub const SORT_KEY_INTS: usize = 4;

// ---------------------------------------------------------------------------
// Executor state
// ---------------------------------------------------------------------------

/// Rust-side sort executor state.
pub struct SortExecState {
    /// Acceleration strategy (should be `GpuSort`).
    strategy: AccelStrategy,

    /// Batch size for input accumulation.
    batch_size: usize,

    /// Sort key descriptors. Selected GpuSort paths support one numeric key;
    /// non-GpuSort/native sort paths may use multiple keys.
    sort_keys: Vec<SortKeyDesc>,

    /// Optional row limit for top-k optimization. When set, only the
    /// first `limit` rows (in sort order) are kept and emitted.
    limit: Option<usize>,

    /// All materialized input tuples, stored after sorting. Owned
    /// `MinimalTuple` copies — we must copy because the child plan
    /// reuses the same `TupleTableSlot` for every `ExecProcNode` call.
    sorted_tuples: Vec<pg_sys::MinimalTuple>,

    /// Current emit position in `sorted_tuples`.
    emit_pos: usize,

    /// Whether all input has been consumed and sorted.
    sort_done: bool,

    /// Whether the child plan is exhausted.
    child_exhausted: bool,

    /// Optional vectorized scanner for direct heap scan mode.
    /// When set, `next_vectorized()` is used instead of `next()`,
    /// bypassing the child plan's `ExecProcNode` overhead.
    vscan: Option<SortScan>,

    // -- Counters for EXPLAIN ANALYZE --
    /// Total rows consumed.
    pub rows_dispatched: u64,
    /// Number of batches during input consumption.
    pub batches_executed: u64,
    /// Cumulative microseconds in sort dispatch.
    pub dispatch_time_us: u64,
    /// Whether the bounded top-k GPU kernel ran.
    pub gpu_dispatched: bool,
    /// Number of non-null key rows submitted to the top-k GPU kernel.
    pub gpu_rows_dispatched: u64,
}

impl SortExecState {
    /// Create a new sort executor state.
    ///
    /// `sort_keys` must contain at least one entry for sorting to occur.
    /// If empty, tuples pass through in input order. `limit` enables
    /// top-k optimization when `Some(k)`.
    #[must_use]
    pub fn new(
        strategy: AccelStrategy,
        batch_size: usize,
        sort_keys: Vec<SortKeyDesc>,
        limit: Option<usize>,
    ) -> Self {
        Self {
            strategy,
            batch_size,
            sort_keys,
            limit,
            sorted_tuples: Vec::new(),
            emit_pos: 0,
            sort_done: false,
            child_exhausted: false,
            vscan: None,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            gpu_dispatched: false,
            gpu_rows_dispatched: 0,
        }
    }

    fn effective_topk_limit(&self, total_rows: usize) -> usize {
        let Some(limit) = self.limit else {
            pgrx::error!(
                "pg_accel: GpuSort reached executor without a bounded LIMIT; \
                 full-output GPU sort is not an exposed planner path"
            );
        };
        if limit > GPU_SORT_TOPK_MAX_LIMIT {
            pgrx::error!(
                "pg_accel: GpuSort LIMIT {} exceeds implemented top-k limit {}; \
                 planner should have declined this shape",
                limit,
                GPU_SORT_TOPK_MAX_LIMIT,
            );
        }
        limit.min(total_rows)
    }

    fn non_null_topk_needed(
        key: &SortKeyDesc,
        null_count: usize,
        non_null_count: usize,
        k: usize,
    ) -> usize {
        if k == 0 {
            return 0;
        }
        if key.nulls_first {
            k.saturating_sub(null_count).min(non_null_count)
        } else {
            k.min(non_null_count)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_gpu_topk(
        &mut self,
        key_typid: u32,
        non_null_indices: &[usize],
        null_indices: &[usize],
        f32_keys: &[f32],
        f64_keys: &[f64],
        i32_keys: &[i32],
        i64_keys: &[i64],
        n: usize,
    ) -> u64 {
        let key = self.sort_keys[0].clone();
        let k = self.effective_topk_limit(n);
        let non_null_needed =
            Self::non_null_topk_needed(&key, null_indices.len(), non_null_indices.len(), k);
        let descending = sort_key_is_desc(&key);

        let gpu_idx = if non_null_needed == 0 {
            Vec::new()
        } else {
            match key_typid {
                FLOAT4OID => gpu_topk_f32(f32_keys, non_null_needed, descending).unwrap_or_else(|| {
                    pgrx::error!(
                        "pg_accel: topk_kv_f32 GPU kernel failed; refusing CPU fallback (rule 11)"
                    );
                }),
                FLOAT8OID => gpu_topk_f64(f64_keys, non_null_needed, descending).unwrap_or_else(|| {
                    pgrx::error!(
                        "pg_accel: topk_kv_f64 GPU kernel failed; refusing CPU fallback (rule 11)"
                    );
                }),
                INT4OID => gpu_topk_i32(i32_keys, non_null_needed, descending).unwrap_or_else(|| {
                    pgrx::error!(
                        "pg_accel: topk_kv_i32 GPU kernel failed; refusing CPU fallback (rule 11)"
                    );
                }),
                INT8OID => gpu_topk_i64(i64_keys, non_null_needed, descending).unwrap_or_else(|| {
                    pgrx::error!(
                        "pg_accel: topk_kv_i64 GPU kernel failed; refusing CPU fallback (rule 11)"
                    );
                }),
                _ => {
                    pgrx::error!(
                        "pg_accel: unsupported GpuSort key type {} reached top-k executor",
                        key_typid,
                    );
                }
            }
        };

        if gpu_idx.len() < non_null_needed {
            pgrx::error!(
                "pg_accel: top-k GPU kernel returned {} indices for requested {}; \
                 refusing to emit an incomplete ORDER BY result",
                gpu_idx.len(),
                non_null_needed,
            );
        }

        self.apply_gpu_topk_result(&key, non_null_indices, null_indices, &gpu_idx, n, k);

        self.gpu_dispatched = non_null_needed > 0;
        self.gpu_rows_dispatched = if self.gpu_dispatched {
            non_null_indices.len() as u64
        } else {
            0
        };
        self.gpu_rows_dispatched
    }

    /// Fetch the next sorted tuple.
    ///
    /// On the first call, consumes all input from the child plan,
    /// sorts the tuples, and begins emitting. Subsequent calls emit
    /// the next sorted tuple until exhausted.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` and
    /// `result_slot` must be valid pointers.
    pub unsafe fn next(
        &mut self,
        child_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.sort_next").entered();
        // Phase 1: Consume all input and sort (once).
        if !self.sort_done {
            // SAFETY: child_ps and result_slot are valid, main backend thread.
            unsafe {
                self.consume_and_sort(child_ps, result_slot);
            }
        }

        // Phase 2: Emit sorted tuples one at a time.
        if self.emit_pos >= self.sorted_tuples.len() {
            return std::ptr::null_mut();
        }

        let mt = self.sorted_tuples[self.emit_pos];
        self.emit_pos += 1;

        if mt.is_null() {
            return std::ptr::null_mut();
        }

        // SAFETY: mt is a valid MinimalTuple. Restore into result_slot.
        // Use ExecForceStoreMinimalTuple because result_slot may be a
        // VirtualTupleTableSlot (ps_ResultTupleSlot default).
        // `false` = slot does not own the tuple.
        unsafe {
            pg_sys::ExecForceStoreMinimalTuple(mt, result_slot, false);
        }
        result_slot
    }

    /// Consume all input tuples in batches and sort them using GPU kernels
    /// when this is a planner-selected `GpuSort` path.
    ///
    /// Extracts the single supported numeric key and dispatches to GPU.
    /// Unsupported keys, failed kernels, and planner/executor mismatches error.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` and
    /// `scratch_slot` must be valid.
    #[allow(clippy::too_many_lines)]
    unsafe fn consume_and_sort(
        &mut self,
        child_ps: *mut pg_sys::PlanState,
        scratch_slot: *mut pg_sys::TupleTableSlot,
    ) {
        let start = std::time::Instant::now();

        // Determine if we can do inline key extraction during consumption.
        // This avoids a second pass over all tuples for GPU sort.
        let inline_gpu =
            self.sort_keys.len() == 1 && matches!(self.strategy, AccelStrategy::GpuSort);

        let (inline_typid, inline_attno) = if inline_gpu {
            let key = &self.sort_keys[0];
            // SAFETY: scratch_slot is valid, tts_tupleDescriptor set by PG.
            let tupdesc = unsafe { (*scratch_slot).tts_tupleDescriptor };
            if tupdesc.is_null() {
                (0, 0)
            } else {
                let attno_idx = (key.attno as usize).wrapping_sub(1);
                let natts = unsafe { (*tupdesc).natts as usize };
                if attno_idx < natts {
                    let attr =
                        unsafe { &*crate::engine::pg_compat::tuple_desc_attr(tupdesc, attno_idx) };
                    (u32::from(attr.atttypid), key.attno as i32)
                } else {
                    (0, 0)
                }
            }
        } else {
            (0, 0)
        };

        // Pre-allocate key buffers for inline extraction.
        let do_inline = matches!(inline_typid, FLOAT4OID | FLOAT8OID | INT4OID | INT8OID);
        let mut f32_keys: Vec<f32> = if do_inline && inline_typid == FLOAT4OID {
            Vec::with_capacity(1024)
        } else {
            Vec::new()
        };
        let mut f64_keys: Vec<f64> = if do_inline && inline_typid == FLOAT8OID {
            Vec::with_capacity(1024)
        } else {
            Vec::new()
        };
        let mut i32_keys: Vec<i32> = if do_inline && inline_typid == INT4OID {
            Vec::with_capacity(1024)
        } else {
            Vec::new()
        };
        let mut i64_keys: Vec<i64> = if do_inline && inline_typid == INT8OID {
            Vec::with_capacity(1024)
        } else {
            Vec::new()
        };
        let mut null_indices: Vec<usize> = Vec::new();
        let mut non_null_indices: Vec<usize> = Vec::new();

        // -- Phase 1: Consume all input (with optional inline key extraction) --
        while !self.child_exhausted {
            for _ in 0..self.batch_size {
                // SAFETY: ExecProcNode pulls the next child tuple.
                let child_slot = unsafe { pg_sys::ExecProcNode(child_ps) };
                if child_slot.is_null() {
                    self.child_exhausted = true;
                    break;
                }

                // SAFETY: child_slot is non-null.
                let is_empty =
                    unsafe { (*child_slot).tts_flags & pg_sys::TTS_FLAG_EMPTY as u16 != 0 };
                if is_empty {
                    self.child_exhausted = true;
                    break;
                }

                // Extract sort key from the LIVE child slot before copying.
                // This is free — the slot is already populated by ExecProcNode.
                if do_inline {
                    let idx = self.sorted_tuples.len();
                    let mut is_null = false;
                    // SAFETY: child_slot is valid and non-empty.
                    let datum =
                        unsafe { pg_sys::slot_getattr(child_slot, inline_attno, &raw mut is_null) };
                    if is_null {
                        null_indices.push(idx);
                    } else {
                        non_null_indices.push(idx);
                        match inline_typid {
                            FLOAT4OID => {
                                f32_keys.push(f32::from_bits(datum.value() as u32));
                            }
                            FLOAT8OID => {
                                f64_keys.push(f64::from_bits(datum.value() as u64));
                            }
                            INT4OID => {
                                i32_keys.push(datum.value() as i32);
                            }
                            INT8OID => {
                                i64_keys.push(datum.value() as i64);
                            }
                            _ => unreachable!(),
                        }
                    }
                }

                // SAFETY: child_slot is valid and non-empty.
                let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(child_slot) };
                self.sorted_tuples.push(mt);
                self.rows_dispatched += 1;
            }

            self.batches_executed += 1;
            pgrx::check_for_interrupts!();
        }

        let n = self.sorted_tuples.len();

        // -- Phase 2: Sort --
        let mut gpu_rows_sorted: u64 = 0;
        if n > 1 && !self.sort_keys.is_empty() {
            // Planner-selected GpuSort is now a bounded top-k shape only.
            // The kernel keeps per-tile candidates on GPU and sorts only
            // those candidates, then the executor retains the selected tuples.
            let gpu_done = if do_inline {
                gpu_rows_sorted = self.dispatch_gpu_topk(
                    inline_typid,
                    &non_null_indices,
                    &null_indices,
                    &f32_keys,
                    &f64_keys,
                    &i32_keys,
                    &i64_keys,
                    n,
                );
                true
            } else {
                false
            };

            if !gpu_done {
                pgrx::error!(
                    "pg_accel: GPU top-k sort could not dispatch for {n} rows \
                     (strategy=GpuSort, type={inline_typid}). \
                     Refusing to continue."
                );
            }
        }

        self.sort_done = true;
        let elapsed_us = start.elapsed().as_micros() as u64;
        self.dispatch_time_us += elapsed_us;

        // Per-backend stats: record the sort batch completion. The sort
        // executor consumes + sorts as a single logical batch per query, so
        // record once here. n is the total row count consumed.
        stats::record_batch(n as u64, elapsed_us);
        if gpu_rows_sorted > 0 {
            stats::record_gpu_batch(gpu_rows_sorted, 0);
        }
    }

    /// Attempt GPU-accelerated sort for a single numeric key column.
    ///
    /// Extracts the sort key datum from each tuple, converts to a GPU-sortable
    /// numeric type, runs the GPU key-value sort, and reorders tuples by the
    /// resulting index permutation.
    ///
    /// Returns `true` if GPU sort succeeded. `false` means the GPU path was
    /// unsupported or failed; callers must error.
    ///
    /// # Supported types
    ///
    /// - `int4` (OID 23): extracted as i32, promoted to f64 (lossless for
    ///   all i32 values since f64 has 53-bit mantissa).
    /// - `float4` (OID 700): extracted as f32, sorted natively.
    /// - `float8` (OID 701): extracted as f64, sorted natively.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `scratch_slot` must be valid.
    #[allow(clippy::too_many_lines, clippy::needless_bool, dead_code)]
    unsafe fn try_gpu_sort(&mut self, scratch_slot: *mut pg_sys::TupleTableSlot, n: usize) -> bool {
        // No max-elements gate: gpu_sort_chunked handles arbitrary sizes.
        let key = self.sort_keys[0].clone();

        // Determine the type of the sort key column from the tuple descriptor.
        // SAFETY: scratch_slot is valid, tts_tupleDescriptor is set by PG.
        let tupdesc = unsafe { (*scratch_slot).tts_tupleDescriptor };
        if tupdesc.is_null() {
            return false;
        }
        let attno_idx = (key.attno as usize).wrapping_sub(1);
        // attno is 1-based and was validated at plan time; attno_idx < natts.
        let natts = unsafe { (*tupdesc).natts as usize };
        if attno_idx >= natts {
            return false;
        }
        // SAFETY: tupdesc is valid; attno_idx < natts verified above.
        let attr = unsafe { &*crate::engine::pg_compat::tuple_desc_attr(tupdesc, attno_idx) };
        let typid = u32::from(attr.atttypid);

        // Single-pass: extract keys AND partition into non-null/null in one
        // pass over the tuples, avoiding the double ExecForceStoreMinimalTuple
        // overhead that was previously required.
        let mut null_indices: Vec<usize> = Vec::with_capacity(16);
        let mut non_null_indices: Vec<usize> = Vec::with_capacity(n);

        match typid {
            FLOAT4OID => {
                // Bulk-extract f32 keys using direct MinimalTuple reads.
                let info = unsafe { AttExtractInfo::new(tupdesc, key.attno as i32) };
                let (all_vals, all_nulls) =
                    unsafe { tuple_extract::extract_f32(&self.sorted_tuples, &info, scratch_slot) };
                let mut keys: Vec<f32> = Vec::with_capacity(n);
                for (i, (&val, &is_null)) in all_vals.iter().zip(all_nulls.iter()).enumerate() {
                    if is_null != 0 {
                        null_indices.push(i);
                    } else {
                        non_null_indices.push(i);
                        keys.push(val);
                    }
                }
                if keys.is_empty() {
                    return false;
                }
                if let Some(gpu_indices) = gpu_sort_chunked_f32(&keys) {
                    self.apply_gpu_sort_result(
                        &key,
                        &non_null_indices,
                        &null_indices,
                        &gpu_indices,
                        n,
                    );
                    true
                } else {
                    false
                }
            }
            FLOAT8OID => {
                let info = unsafe { AttExtractInfo::new(tupdesc, key.attno as i32) };
                let (all_vals, all_nulls) =
                    unsafe { tuple_extract::extract_f64(&self.sorted_tuples, &info, scratch_slot) };
                let mut keys: Vec<f64> = Vec::with_capacity(n);
                for (i, (&val, &is_null)) in all_vals.iter().zip(all_nulls.iter()).enumerate() {
                    if is_null != 0 {
                        null_indices.push(i);
                    } else {
                        non_null_indices.push(i);
                        keys.push(val);
                    }
                }
                if keys.is_empty() {
                    return false;
                }
                if let Some(gpu_indices) = gpu_sort_chunked_f64(&keys) {
                    self.apply_gpu_sort_result(
                        &key,
                        &non_null_indices,
                        &null_indices,
                        &gpu_indices,
                        n,
                    );
                    true
                } else {
                    false
                }
            }
            INT4OID => {
                // Bulk-extract i32 keys, promote to f64 (lossless).
                let info = unsafe { AttExtractInfo::new(tupdesc, key.attno as i32) };
                let (all_vals, all_nulls) =
                    unsafe { tuple_extract::extract_i32(&self.sorted_tuples, &info, scratch_slot) };
                let mut keys: Vec<f64> = Vec::with_capacity(n);
                for (i, (&val, &is_null)) in all_vals.iter().zip(all_nulls.iter()).enumerate() {
                    if is_null != 0 {
                        null_indices.push(i);
                    } else {
                        non_null_indices.push(i);
                        keys.push(f64::from(val));
                    }
                }
                if keys.is_empty() {
                    return false;
                }
                if let Some(gpu_indices) = gpu_sort_chunked_f64(&keys) {
                    self.apply_gpu_sort_result(
                        &key,
                        &non_null_indices,
                        &null_indices,
                        &gpu_indices,
                        n,
                    );
                    true
                } else {
                    false
                }
            }
            INT8OID => {
                // Bulk-extract i64 keys, promote to f64 (lossless for |v| ≤ 2^53).
                let info = unsafe { AttExtractInfo::new(tupdesc, key.attno as i32) };
                let (all_vals, all_nulls) =
                    unsafe { tuple_extract::extract_i64(&self.sorted_tuples, &info, scratch_slot) };
                let mut keys: Vec<f64> = Vec::with_capacity(n);
                for (i, (&val, &is_null)) in all_vals.iter().zip(all_nulls.iter()).enumerate() {
                    if is_null != 0 {
                        null_indices.push(i);
                    } else {
                        non_null_indices.push(i);
                        #[allow(clippy::cast_precision_loss)]
                        keys.push(val as f64);
                    }
                }
                if keys.is_empty() {
                    return false;
                }
                if let Some(gpu_indices) = gpu_sort_chunked_f64(&keys) {
                    self.apply_gpu_sort_result(
                        &key,
                        &non_null_indices,
                        &null_indices,
                        &gpu_indices,
                        n,
                    );
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Reorder `sorted_tuples` using GPU sort results.
    ///
    /// Takes the GPU-produced index permutation over non-null rows and
    /// stitches the final order: sorted non-nulls + nulls positioned
    /// by NULLS FIRST/LAST setting.
    fn apply_gpu_sort_result(
        &mut self,
        key: &SortKeyDesc,
        non_null_indices: &[usize],
        null_indices: &[usize],
        gpu_indices: &[u32],
        n: usize,
    ) {
        // GPU sorted ascending with NaN as largest. If DESC, reverse.
        // Known GT operator OIDs: float4gt(623), float8gt(674), int4gt(521), int8gt(413).
        let is_desc = sort_key_is_desc(key);

        let sorted_non_null: Vec<pg_sys::MinimalTuple> = if is_desc {
            gpu_indices
                .iter()
                .rev()
                .map(|&gi| self.sorted_tuples[non_null_indices[gi as usize]])
                .collect()
        } else {
            gpu_indices
                .iter()
                .map(|&gi| self.sorted_tuples[non_null_indices[gi as usize]])
                .collect()
        };

        let null_tuples: Vec<pg_sys::MinimalTuple> = null_indices
            .iter()
            .map(|&i| self.sorted_tuples[i])
            .collect();

        let mut reordered = Vec::with_capacity(n);
        if key.nulls_first {
            reordered.extend_from_slice(&null_tuples);
            reordered.extend_from_slice(&sorted_non_null);
        } else {
            reordered.extend_from_slice(&sorted_non_null);
            reordered.extend_from_slice(&null_tuples);
        }
        self.sorted_tuples = reordered;
    }

    /// Retain only the bounded top-k tuple set selected by the GPU top-k path.
    ///
    /// `gpu_indices` is ordered in final non-null key order and indexes into
    /// `non_null_indices`, not directly into `sorted_tuples`.
    fn apply_gpu_topk_result(
        &mut self,
        key: &SortKeyDesc,
        non_null_indices: &[usize],
        null_indices: &[usize],
        gpu_indices: &[u32],
        n: usize,
        k: usize,
    ) {
        let selected_indices =
            topk_tuple_indices(key, non_null_indices, null_indices, gpu_indices, n, k);
        let mut keep = vec![false; n];
        for &idx in &selected_indices {
            if idx < keep.len() {
                keep[idx] = true;
            }
        }

        let selected: Vec<pg_sys::MinimalTuple> = selected_indices
            .iter()
            .filter_map(|&idx| self.sorted_tuples.get(idx).copied())
            .collect();

        for (idx, mt) in self.sorted_tuples.iter().enumerate() {
            if !keep.get(idx).copied().unwrap_or(false) && !mt.is_null() {
                // SAFETY: MinimalTuples were palloc'd by ExecCopySlotMinimalTuple.
                unsafe {
                    pg_sys::pfree((*mt).cast());
                }
            }
        }

        self.sorted_tuples = selected;
    }

    /// Returns the acceleration strategy.
    #[must_use]
    pub fn strategy(&self) -> AccelStrategy {
        self.strategy
    }

    /// Returns the sort key descriptors.
    #[must_use]
    pub fn sort_keys(&self) -> &[SortKeyDesc] {
        &self.sort_keys
    }

    /// Returns the bounded top-k limit, if any.
    #[must_use]
    pub fn limit(&self) -> Option<usize> {
        self.limit
    }

    /// Number of input heap tuples copied/materialized before top-k pruning.
    #[must_use]
    pub fn input_rows_materialized(&self) -> u64 {
        self.rows_dispatched
    }

    /// Number of tuple copies retained for emission after top-k pruning.
    #[must_use]
    pub fn retained_output_tuples(&self) -> usize {
        self.sorted_tuples.len()
    }

    /// Number of materialized tuple copies freed/dropped after top-k selection.
    #[must_use]
    pub fn rows_pruned_after_topk(&self) -> u64 {
        let retained = u64::try_from(self.retained_output_tuples()).unwrap_or(u64::MAX);
        self.rows_dispatched.saturating_sub(retained)
    }

    /// Whether this node copied the full input before emitting top-k output.
    #[must_use]
    pub fn full_input_materialized(&self) -> bool {
        self.rows_dispatched > 0
    }

    /// Attach a [`SortScan`] for direct heap scan mode.
    ///
    /// When set, `next_vectorized()` is used instead of `next()` to bypass
    /// `ExecProcNode` per-tuple overhead.
    pub fn set_vscan(&mut self, vscan: SortScan) {
        self.vscan = Some(vscan);
    }

    /// Whether a vectorized scanner is attached.
    #[must_use]
    pub fn has_vscan(&self) -> bool {
        self.vscan.is_some()
    }

    /// Borrow the attached vectorized scanner (if any).
    #[must_use]
    pub fn vscan_ref(&self) -> &Option<SortScan> {
        &self.vscan
    }

    /// Fetch the next sorted tuple using the vectorized scan path.
    ///
    /// On the first call, scans the entire heap via [`SortScan`],
    /// extracts sort key columns inline, dispatches GPU sort on keys +
    /// indices, and prepares the sorted tuple order. Subsequent calls
    /// emit tuples from the arena in sorted order via `materialize()`.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `result_slot` must be
    /// a valid `TupleTableSlot`. `self.vscan` must be `Some`.
    pub unsafe fn next_vectorized(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.sort_next_vectorized").entered();

        // Phase 1: Scan all rows and sort (once).
        if !self.sort_done {
            // SAFETY: vscan is Some (caller guarantees).
            unsafe {
                self.vscan_consume_and_sort();
            }
        }

        // Phase 2: Emit sorted tuples one at a time.
        if self.emit_pos >= self.sorted_tuples.len() {
            return std::ptr::null_mut();
        }

        let mt = self.sorted_tuples[self.emit_pos];
        self.emit_pos += 1;

        if mt.is_null() {
            return std::ptr::null_mut();
        }

        // SAFETY: mt is a valid MinimalTuple from the vscan arena.
        // ExecForceStoreMinimalTuple stores it into result_slot.
        // `false` = slot does not own the tuple.
        unsafe {
            pg_sys::ExecForceStoreMinimalTuple(mt, result_slot, false);
        }
        result_slot
    }

    /// Consume all tuples via SortScan and sort them.
    ///
    /// Uses the vscan's inline-extracted keys for GPU sort. If a
    /// planner-selected `GpuSort` path cannot dispatch, execution errors
    /// instead of sorting on CPU inside pg_accel.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `scan_slot` must be valid.
    #[allow(clippy::too_many_lines)]
    unsafe fn vscan_consume_and_sort(&mut self) {
        let start = std::time::Instant::now();

        // Extract everything from vscan in one block to release the borrow
        // before calling self methods.
        let (
            n,
            key_typid,
            _scratch_slot,
            non_null_indices,
            null_indices,
            f32_keys,
            f64_keys,
            i32_keys,
            i64_keys,
        ) = {
            let vscan = self.vscan.as_mut().expect("vscan must be set");
            // SAFETY: vscan has its own scan slot and valid scan_desc.
            let n = unsafe { vscan.scan_all() };
            let key_typid = vscan.key_typid();
            let scratch_slot = vscan.scan_slot();
            let arena = vscan.take_arena();
            let non_null = vscan.take_non_null_indices();
            let null_idx = vscan.take_null_indices();
            let f32k = vscan.take_keys_f32();
            let f64k = vscan.take_keys_f64();
            let i32k = vscan.take_keys_i32();
            let i64k = vscan.take_keys_i64();
            // Move arena into sorted_tuples (vscan no longer owns them).
            self.sorted_tuples = arena;
            (
                n,
                key_typid,
                scratch_slot,
                non_null,
                null_idx,
                f32k,
                f64k,
                i32k,
                i64k,
            )
        };

        self.rows_dispatched = n as u64;
        self.batches_executed = 1;

        let mut gpu_rows_sorted: u64 = 0;
        if n > 1 && !self.sort_keys.is_empty() {
            let do_inline = matches!(key_typid, FLOAT4OID | FLOAT8OID | INT4OID | INT8OID);

            enum SortOutcome {
                InputGate,
                Dispatched,
            }

            let outcome = if do_inline {
                gpu_rows_sorted = self.dispatch_gpu_topk(
                    key_typid,
                    &non_null_indices,
                    &null_indices,
                    &f32_keys,
                    &f64_keys,
                    &i32_keys,
                    &i64_keys,
                    n,
                );
                SortOutcome::Dispatched
            } else {
                SortOutcome::InputGate
            };

            match outcome {
                SortOutcome::Dispatched => {}
                SortOutcome::InputGate => {
                    pgrx::error!(
                        "pg_accel: GpuSort could not dispatch {} rows with key type {}; \
                         refusing to continue",
                        n,
                        key_typid,
                    );
                }
            }
        }

        self.sort_done = true;
        let elapsed_us = start.elapsed().as_micros() as u64;
        self.dispatch_time_us += elapsed_us;

        // Per-backend stats: single logical batch per vectorized sort.
        stats::record_batch(n as u64, elapsed_us);
        if gpu_rows_sorted > 0 {
            stats::record_gpu_batch(gpu_rows_sorted, 0);
        }
    }
}

impl crate::engine::executor::state::ExecutorState for SortExecState {
    unsafe fn exec(&mut self, css: *mut pg_sys::CustomScanState) -> *mut pg_sys::TupleTableSlot {
        let scan_slot = unsafe { (*css).ss.ss_ScanTupleSlot };
        if self.has_vscan() {
            unsafe { self.next_vectorized(scan_slot) }
        } else {
            let child_ps = unsafe { crate::engine::executor::state::child_plan_state(css, 0) };
            unsafe { self.next(child_ps, scan_slot) }
        }
    }
    fn rows_dispatched(&self) -> u64 {
        self.rows_dispatched
    }
    fn batches_executed(&self) -> u64 {
        self.batches_executed
    }
    fn dispatch_time_us(&self) -> u64 {
        self.dispatch_time_us
    }
}

#[cfg(feature = "pg_test")]
#[allow(dead_code)]
#[path = "tests.rs"]
mod tests;
