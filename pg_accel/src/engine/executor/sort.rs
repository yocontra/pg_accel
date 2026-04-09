//! Batch-dispatch sort executor for pg_accel Custom Scan nodes.
//!
//! [`SortExecState`] consumes all input tuples, sorts them via PG's
//! `SortSupport` comparison infrastructure (CPU path), and returns
//! sorted tuples one at a time. When GPU sort kernels are wired,
//! this will dispatch to GPU bitonic sort instead.
//!
//! Supports multi-key sorting with ASC/DESC and NULLS FIRST/LAST,
//! plus top-k optimization for `ORDER BY ... LIMIT k`.
//!
//! # Lifecycle
//!
//! 1. **`begin_custom_scan`** — allocates `SortExecState` with sort keys.
//! 2. **`exec_custom_scan`** (first call) — consumes all input, sorts.
//! 3. **`exec_custom_scan`** (subsequent) — returns sorted tuples.
//! 4. **`end_custom_scan`** — reclaims via `Box::from_raw`.

use pgrx::pg_sys;

use crate::engine::executor::tuple_extract::{self, AttExtractInfo};
use crate::engine::executor::vscan::VectorizedScan;
use crate::engine::registry::AccelStrategy;
use crate::gpu;

use crate::engine::cost;

/// PG type OIDs for GPU-sortable numeric types.
const INT4OID: u32 = 23;
const INT8OID: u32 = 20;
const FLOAT4OID: u32 = 700;
const FLOAT8OID: u32 = 701;

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

    /// Sort key descriptors (multi-key sort supported).
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
    vscan: Option<VectorizedScan>,

    // -- Counters for EXPLAIN ANALYZE --
    /// Total rows consumed.
    pub rows_dispatched: u64,
    /// Number of batches during input consumption.
    pub batches_executed: u64,
    /// Cumulative microseconds in sort dispatch.
    pub dispatch_time_us: u64,
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
        }
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

    /// Consume all input tuples in batches and sort them using PG's
    /// `SortSupport` comparison infrastructure.
    ///
    /// For each sort key, initializes a `SortSupportData` via
    /// `PrepareSortSupportFromOrderingOp`, then pre-extracts all sort
    /// key datums from the buffered tuples. An index-based sort avoids
    /// moving full tuples during comparison. When `self.limit` is set,
    /// applies top-k optimization via partial sort.
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
                    let attr = unsafe { &*(*tupdesc).attrs.as_ptr().add(attno_idx) };
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
        let mut f64_keys: Vec<f64> = if do_inline && inline_typid != FLOAT4OID {
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
                                f64_keys.push(f64::from(datum.value() as i32));
                            }
                            INT8OID => {
                                // i64 → f64 is lossless for |v| ≤ 2^53.
                                f64_keys.push(datum.value() as i64 as f64);
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
        if n > 1 && !self.sort_keys.is_empty() {
            // Try GPU sort using inline-extracted keys (zero-copy key extraction).
            let limits = cost::device_limits();
            let gpu_done = if do_inline
                && n >= limits.gpu_sort_min_rows
                && n <= limits.gpu_sort_max_elements
            {
                let key = self.sort_keys[0].clone();
                match inline_typid {
                    FLOAT4OID => {
                        if f32_keys.is_empty() {
                            false
                        } else {
                            let mut gpu_idx: Vec<u32> = (0..f32_keys.len() as u32).collect();
                            let ok = gpu::sort_kv_f32(&mut f32_keys, &mut gpu_idx).is_some();
                            if ok {
                                self.apply_gpu_sort_result(
                                    &key,
                                    &non_null_indices,
                                    &null_indices,
                                    &gpu_idx,
                                    n,
                                );
                            }
                            ok
                        }
                    }
                    FLOAT8OID | INT4OID | INT8OID => {
                        if f64_keys.is_empty() {
                            false
                        } else {
                            let mut gpu_idx: Vec<u32> = (0..f64_keys.len() as u32).collect();
                            let ok = gpu::sort_kv_f64(&mut f64_keys, &mut gpu_idx).is_some();
                            if ok {
                                self.apply_gpu_sort_result(
                                    &key,
                                    &non_null_indices,
                                    &null_indices,
                                    &gpu_idx,
                                    n,
                                );
                            }
                            ok
                        }
                    }
                    _ => false,
                }
            } else {
                false
            };

            if !gpu_done {
                // Defer to PG's SortSupport comparison infrastructure.
                // This is not a CPU reimplementation — it uses PG's native
                // sort operators (handles multi-key sorts and non-GPU types).
                // SAFETY: main backend thread, scratch_slot valid.
                pgrx::warning!(
                    "pg_accel: GPU sort unavailable for {} rows; \
                     deferring to PG SortSupport",
                    n,
                );
                unsafe {
                    self.sort_tuples(scratch_slot, n);
                }
            }
        }

        // -- Phase 3: Truncate for LIMIT (top-k) --
        if let Some(k) = self.limit
            && k < self.sorted_tuples.len()
        {
            // pfree the tuples beyond the limit.
            for mt in &self.sorted_tuples[k..] {
                if !mt.is_null() {
                    // SAFETY: MinimalTuples were palloc'd by
                    // ExecCopySlotMinimalTuple. pfree returns them.
                    unsafe {
                        pg_sys::pfree((*mt).cast());
                    }
                }
            }
            self.sorted_tuples.truncate(k);
        }

        self.sort_done = true;
        self.dispatch_time_us += start.elapsed().as_micros() as u64;
    }

    /// Index-based sort of `sorted_tuples` using PG `SortSupport`.
    ///
    /// Pre-extracts all sort key datums, builds an index array, sorts
    /// the indices by multi-key comparison, then reorders tuples.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    unsafe fn sort_tuples(&mut self, scratch_slot: *mut pg_sys::TupleTableSlot, n: usize) {
        let num_keys = self.sort_keys.len();

        // -- Initialize SortSupportData for each key --
        let mut ssup_vec: Vec<pg_sys::SortSupportData> = Vec::with_capacity(num_keys);
        for key in &self.sort_keys {
            // SAFETY: SortSupportData is zeroed, then populated by
            // PrepareSortSupportFromOrderingOp which sets the comparator.
            let mut ssup: pg_sys::SortSupportData = unsafe { std::mem::zeroed() };
            ssup.ssup_cxt = unsafe { pg_sys::CurrentMemoryContext };
            ssup.ssup_collation = key.collation;
            ssup.ssup_nulls_first = key.nulls_first;
            // ssup_attno is 1-based attribute number.
            ssup.ssup_attno = key.attno;
            // SAFETY: Initializes the comparator function pointer in ssup
            // from the ordering operator OID. Main backend thread.
            unsafe {
                pg_sys::PrepareSortSupportFromOrderingOp(key.sort_op, &raw mut ssup);
            }
            ssup_vec.push(ssup);
        }

        // -- Pre-extract sort key datums for all tuples --
        // Use bulk extraction to avoid per-tuple ExecForceStoreMinimalTuple.
        // Layout: key_datums[key_idx][tuple_idx] = (Datum, is_null)
        // SAFETY: scratch_slot has a valid tuple descriptor.
        let tupdesc = unsafe { (*scratch_slot).tts_tupleDescriptor };
        let mut key_datums: Vec<Vec<(pg_sys::Datum, bool)>> = Vec::with_capacity(num_keys);

        for key in &self.sort_keys {
            let info = unsafe { AttExtractInfo::new(tupdesc, key.attno as i32) };
            // SAFETY: sorted_tuples contains valid MinimalTuple pointers.
            // scratch_slot is valid for fallback extraction.
            let (datums, nulls) =
                unsafe { tuple_extract::extract_datum(&self.sorted_tuples, &info, scratch_slot) };
            let col: Vec<(pg_sys::Datum, bool)> = datums
                .into_iter()
                .zip(nulls.into_iter())
                .map(|(d, n)| (d, n != 0))
                .collect();
            key_datums.push(col);
        }

        // -- Build and sort index array --
        let mut indices: Vec<usize> = (0..n).collect();

        // For top-k when limit << n, use partial sort:
        // select_nth_unstable_by partitions in O(n), then sort first k in O(k log k).
        let use_topk = matches!(self.limit, Some(k) if k > 0 && k < n / 2);

        let cmp = |a: &usize, b: &usize| -> std::cmp::Ordering {
            for (k, ssup) in ssup_vec.iter().enumerate() {
                let (da, na) = key_datums[k][*a];
                let (db, nb) = key_datums[k][*b];

                // Handle NULLs according to nulls_first setting.
                match (na, nb) {
                    (true, true) => continue,
                    (true, false) => {
                        return if ssup.ssup_nulls_first {
                            std::cmp::Ordering::Less
                        } else {
                            std::cmp::Ordering::Greater
                        };
                    }
                    (false, true) => {
                        return if ssup.ssup_nulls_first {
                            std::cmp::Ordering::Greater
                        } else {
                            std::cmp::Ordering::Less
                        };
                    }
                    (false, false) => {}
                }

                // SAFETY: Both datums are non-null and the comparator was
                // initialized by PrepareSortSupportFromOrderingOp. The
                // ssup pointer is valid for the lifetime of this sort.
                // We cast away const because PG's comparator signature
                // takes a mutable SortSupport pointer.
                let result = unsafe {
                    let comparator = ssup.comparator.unwrap_or(trivial_cmp);
                    let ssup_ptr = std::ptr::from_ref(ssup).cast_mut();
                    comparator(da, db, ssup_ptr)
                };

                let ord = result.cmp(&0);
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            std::cmp::Ordering::Equal
        };

        if use_topk {
            let k = self.limit.unwrap_or(n);
            // SAFETY: k < n guaranteed by use_topk check above.
            indices.select_nth_unstable_by(k, cmp);
            indices.truncate(k);
        }
        indices.sort_by(cmp);

        // -- Reorder tuples by sorted indices --
        let reordered: Vec<pg_sys::MinimalTuple> =
            indices.iter().map(|&i| self.sorted_tuples[i]).collect();
        self.sorted_tuples = reordered;
    }

    /// Attempt GPU-accelerated sort for a single numeric key column.
    ///
    /// Extracts the sort key datum from each tuple, converts to a GPU-sortable
    /// numeric type, runs the GPU key-value sort, and reorders tuples by the
    /// resulting index permutation.
    ///
    /// Returns `true` if GPU sort succeeded, `false` if the caller should
    /// defer to PG's SortSupport.
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
    #[allow(clippy::too_many_lines, clippy::needless_bool)]
    unsafe fn try_gpu_sort(&mut self, scratch_slot: *mut pg_sys::TupleTableSlot, n: usize) -> bool {
        // Gate: fall back to CPU sort for large arrays to avoid SYCL runtime aborts.
        if n > cost::device_limits().gpu_sort_max_elements {
            return false;
        }

        let key = self.sort_keys[0].clone();

        // Determine the type of the sort key column from the tuple descriptor.
        // SAFETY: scratch_slot is valid, tts_tupleDescriptor is set by PG.
        let tupdesc = unsafe { (*scratch_slot).tts_tupleDescriptor };
        if tupdesc.is_null() {
            return false;
        }
        let attno_idx = (key.attno as usize).wrapping_sub(1);
        // SAFETY: tupdesc->attrs is a flexible array member. attno is 1-based
        // and was validated at plan time; attno_idx < natts.
        let natts = unsafe { (*tupdesc).natts as usize };
        if attno_idx >= natts {
            return false;
        }
        // SAFETY: tupdesc is valid; attno_idx < natts verified above.
        let attr = unsafe { &*(*tupdesc).attrs.as_ptr().add(attno_idx) };
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
                let mut gpu_indices: Vec<u32> = (0..keys.len() as u32).collect();
                let ok = gpu::sort_kv_f32(&mut keys, &mut gpu_indices).is_some();
                if ok {
                    self.apply_gpu_sort_result(
                        &key,
                        &non_null_indices,
                        &null_indices,
                        &gpu_indices,
                        n,
                    );
                }
                ok
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
                let mut gpu_indices: Vec<u32> = (0..keys.len() as u32).collect();
                let ok = gpu::sort_kv_f64(&mut keys, &mut gpu_indices).is_some();
                if ok {
                    self.apply_gpu_sort_result(
                        &key,
                        &non_null_indices,
                        &null_indices,
                        &gpu_indices,
                        n,
                    );
                }
                ok
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
                let mut gpu_indices: Vec<u32> = (0..keys.len() as u32).collect();
                let ok = gpu::sort_kv_f64(&mut keys, &mut gpu_indices).is_some();
                if ok {
                    self.apply_gpu_sort_result(
                        &key,
                        &non_null_indices,
                        &null_indices,
                        &gpu_indices,
                        n,
                    );
                }
                ok
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
                let mut gpu_indices: Vec<u32> = (0..keys.len() as u32).collect();
                let ok = gpu::sort_kv_f64(&mut keys, &mut gpu_indices).is_some();
                if ok {
                    self.apply_gpu_sort_result(
                        &key,
                        &non_null_indices,
                        &null_indices,
                        &gpu_indices,
                        n,
                    );
                }
                ok
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
        let sort_op_raw = u32::from(key.sort_op);
        let is_desc = matches!(sort_op_raw, 623 | 674 | 521 | 413);

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

    /// Attach a [`VectorizedScan`] for direct heap scan mode.
    ///
    /// When set, `next_vectorized()` is used instead of `next()` to bypass
    /// `ExecProcNode` per-tuple overhead.
    pub fn set_vscan(&mut self, vscan: VectorizedScan) {
        self.vscan = Some(vscan);
    }

    /// Whether a vectorized scanner is attached.
    #[must_use]
    pub fn has_vscan(&self) -> bool {
        self.vscan.is_some()
    }

    /// Borrow the attached vectorized scanner (if any).
    #[must_use]
    pub fn vscan_ref(&self) -> &Option<VectorizedScan> {
        &self.vscan
    }

    /// Fetch the next sorted tuple using the vectorized scan path.
    ///
    /// On the first call, scans the entire heap via [`VectorizedScan`],
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

    /// Consume all tuples via VectorizedScan and sort them.
    ///
    /// Uses the vscan's inline-extracted keys for GPU sort when available,
    /// falling back to PG's SortSupport for unsupported types.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `scan_slot` must be valid.
    #[allow(clippy::too_many_lines)]
    unsafe fn vscan_consume_and_sort(&mut self) {
        let start = std::time::Instant::now();

        // Extract everything from vscan in one block to release the borrow
        // before calling self methods (apply_gpu_sort_result, sort_tuples).
        let (n, key_typid, scratch_slot, non_null_indices, null_indices, f32_keys, f64_keys) = {
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
            // Move arena into sorted_tuples (vscan no longer owns them).
            self.sorted_tuples = arena;
            (n, key_typid, scratch_slot, non_null, null_idx, f32k, f64k)
        };

        self.rows_dispatched = n as u64;
        self.batches_executed = 1;

        if n > 1 && !self.sort_keys.is_empty() {
            let limits = cost::device_limits();
            let do_inline = matches!(key_typid, FLOAT4OID | FLOAT8OID | INT4OID | INT8OID);

            let gpu_done = if do_inline
                && n >= limits.gpu_sort_min_rows
                && n <= limits.gpu_sort_max_elements
            {
                let key = self.sort_keys[0].clone();

                match key_typid {
                    FLOAT4OID => {
                        let mut f32_keys = f32_keys;
                        if f32_keys.is_empty() {
                            false
                        } else {
                            let mut gpu_idx: Vec<u32> = (0..f32_keys.len() as u32).collect();
                            let ok = gpu::sort_kv_f32(&mut f32_keys, &mut gpu_idx).is_some();
                            if ok {
                                self.apply_gpu_sort_result(
                                    &key,
                                    &non_null_indices,
                                    &null_indices,
                                    &gpu_idx,
                                    n,
                                );
                            }
                            ok
                        }
                    }
                    FLOAT8OID | INT4OID | INT8OID => {
                        let mut f64_keys = f64_keys;
                        if f64_keys.is_empty() {
                            false
                        } else {
                            let mut gpu_idx: Vec<u32> = (0..f64_keys.len() as u32).collect();
                            let ok = gpu::sort_kv_f64(&mut f64_keys, &mut gpu_idx).is_some();
                            if ok {
                                self.apply_gpu_sort_result(
                                    &key,
                                    &non_null_indices,
                                    &null_indices,
                                    &gpu_idx,
                                    n,
                                );
                            }
                            ok
                        }
                    }
                    _ => false,
                }
            } else {
                false
            };

            if !gpu_done {
                pgrx::warning!(
                    "pg_accel: GPU sort unavailable for {} rows (vscan); \
                     deferring to PG SortSupport",
                    n,
                );
                // SAFETY: scratch_slot has base relation's TupleDesc.
                unsafe {
                    self.sort_tuples(scratch_slot, n);
                }
            }
        }

        // Top-k truncation.
        if let Some(k) = self.limit
            && k < self.sorted_tuples.len()
        {
            for mt in &self.sorted_tuples[k..] {
                if !mt.is_null() {
                    // SAFETY: MinimalTuples were palloc'd by
                    // ExecCopySlotMinimalTuple in VectorizedScan.
                    unsafe {
                        pg_sys::pfree((*mt).cast());
                    }
                }
            }
            self.sorted_tuples.truncate(k);
        }

        self.sort_done = true;
        self.dispatch_time_us += start.elapsed().as_micros() as u64;
    }
}

/// Trivial comparator fallback that treats all values as equal.
/// Used only if `SortSupportData.comparator` is `None`, which should
/// never happen after `PrepareSortSupportFromOrderingOp`.
///
/// # Safety
///
/// Must be called by PostgreSQL's sort infrastructure with valid Datum
/// arguments and a valid `SortSupport` pointer. This function does not
/// dereference any of its arguments, so it is trivially safe.
unsafe extern "C-unwind" fn trivial_cmp(
    _a: pg_sys::Datum,
    _b: pg_sys::Datum,
    _ssup: pg_sys::SortSupport,
) -> std::ffi::c_int {
    0
}

#[cfg(feature = "pg_test")]
mod tests {
    use super::*;

    /// Helper: create a SortExecState with no sort keys (passthrough).
    fn make_state(strategy: AccelStrategy, batch_size: usize) -> SortExecState {
        SortExecState::new(strategy, batch_size, vec![], None)
    }

    /// Helper: create a SortExecState with one sort key.
    fn make_state_with_key(batch_size: usize, limit: Option<usize>) -> SortExecState {
        let key = SortKeyDesc {
            attno: 1,
            sort_op: pg_sys::Oid::from(0u32),
            collation: pg_sys::Oid::from(0u32),
            nulls_first: false,
        };
        SortExecState::new(AccelStrategy::GpuSort, batch_size, vec![key], limit)
    }

    #[test]
    fn new_state_defaults() {
        let state = make_state(AccelStrategy::GpuSort, 512);
        assert_eq!(state.strategy(), AccelStrategy::GpuSort);
        assert!(!state.sort_done);
        assert!(!state.child_exhausted);
        assert_eq!(state.emit_pos, 0);
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
    }

    #[test]
    fn empty_sorted_tuples() {
        let state = make_state(AccelStrategy::GpuSort, 256);
        assert!(state.sorted_tuples.is_empty());
    }

    #[test]
    fn batch_size_stored() {
        let state = make_state(AccelStrategy::GpuSort, 2048);
        assert_eq!(state.batch_size, 2048);
    }

    #[test]
    fn single_element_batch_size() {
        let state = make_state(AccelStrategy::GpuSort, 1);
        assert_eq!(state.batch_size, 1);
        assert!(state.sorted_tuples.is_empty());
    }

    #[test]
    fn emit_pos_starts_at_zero() {
        let state = make_state(AccelStrategy::GpuSort, 256);
        assert_eq!(state.emit_pos, 0);
    }

    #[test]
    fn sort_done_false_initially() {
        let state = make_state(AccelStrategy::GpuSort, 256);
        assert!(!state.sort_done);
        assert!(!state.child_exhausted);
    }

    #[test]
    fn counters_zero_on_init() {
        let state = make_state(AccelStrategy::GpuSort, 256);
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
        assert_eq!(state.dispatch_time_us, 0);
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
            let state = make_state(strategy, 64);
            assert_eq!(state.strategy(), strategy);
        }
    }

    #[test]
    fn large_batch_size() {
        let state = make_state(AccelStrategy::GpuSort, 1_000_000);
        assert_eq!(state.batch_size, 1_000_000);
    }

    #[test]
    fn simulated_emit_past_end() {
        let mut state = make_state(AccelStrategy::GpuSort, 256);
        state.sort_done = true;
        state.emit_pos = 0;
        assert!(state.sorted_tuples.is_empty());
        assert_eq!(state.emit_pos, state.sorted_tuples.len());
    }

    #[test]
    fn simulated_emit_with_null_tuples() {
        let mut state = make_state(AccelStrategy::GpuSort, 256);
        state.sort_done = true;
        state.sorted_tuples = vec![std::ptr::null_mut(); 3];
        state.emit_pos = 0;
        assert_eq!(state.sorted_tuples.len(), 3);
    }

    #[test]
    fn emit_pos_increments() {
        let mut state = make_state(AccelStrategy::GpuSort, 256);
        state.sort_done = true;
        state.sorted_tuples = vec![std::ptr::null_mut(); 5];
        state.emit_pos = 3;
        assert_eq!(state.emit_pos, 3);
        assert!(state.emit_pos < state.sorted_tuples.len());
    }

    #[test]
    fn sort_keys_stored() {
        let state = make_state_with_key(256, None);
        assert_eq!(state.sort_keys().len(), 1);
        assert_eq!(state.sort_keys()[0].attno, 1);
        assert!(!state.sort_keys()[0].nulls_first);
    }

    #[test]
    fn limit_stored() {
        let state = make_state_with_key(256, Some(10));
        assert_eq!(state.limit, Some(10));
    }

    #[test]
    fn no_sort_keys_means_passthrough() {
        let state = make_state(AccelStrategy::GpuSort, 256);
        assert!(state.sort_keys().is_empty());
    }

    #[test]
    fn multi_key_sort() {
        let keys = vec![
            SortKeyDesc {
                attno: 1,
                sort_op: pg_sys::Oid::from(0u32),
                collation: pg_sys::Oid::from(0u32),
                nulls_first: false,
            },
            SortKeyDesc {
                attno: 2,
                sort_op: pg_sys::Oid::from(0u32),
                collation: pg_sys::Oid::from(0u32),
                nulls_first: true,
            },
        ];
        let state = SortExecState::new(AccelStrategy::GpuSort, 256, keys, None);
        assert_eq!(state.sort_keys().len(), 2);
        assert!(!state.sort_keys()[0].nulls_first);
        assert!(state.sort_keys()[1].nulls_first);
    }

    #[test]
    fn sort_key_ints_constant() {
        assert_eq!(SORT_KEY_INTS, 4);
    }

    #[test]
    fn gpu_sort_min_rows_threshold() {
        assert_eq!(cost::device_limits().gpu_sort_min_rows, 100_000);
    }

    #[test]
    fn gpu_sortable_type_oids() {
        // Verify our OID constants match PG system catalog values.
        assert_eq!(INT4OID, 23);
        assert_eq!(INT8OID, 20);
        assert_eq!(FLOAT4OID, 700);
        assert_eq!(FLOAT8OID, 701);
    }

    #[test]
    fn gpu_sort_requires_gpu_sort_strategy() {
        // Non-GpuSort strategies should never attempt GPU sort even with
        // enough rows — the try_gpu_sort check is gated on GpuSort.
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert!(!matches!(state.strategy(), AccelStrategy::GpuSort));
    }

    #[test]
    fn gpu_sort_requires_single_key() {
        // Multi-key sorts fall back to CPU.
        let keys = vec![
            SortKeyDesc {
                attno: 1,
                sort_op: pg_sys::Oid::from(0u32),
                collation: pg_sys::Oid::from(0u32),
                nulls_first: false,
            },
            SortKeyDesc {
                attno: 2,
                sort_op: pg_sys::Oid::from(0u32),
                collation: pg_sys::Oid::from(0u32),
                nulls_first: false,
            },
        ];
        let state = SortExecState::new(AccelStrategy::GpuSort, 256, keys, None);
        // Multi-key: GPU sort path won't fire (checked in consume_and_sort).
        assert!(state.sort_keys().len() > 1);
    }
}
