//! Batch-dispatch aggregate executor for pg_accel Custom Scan nodes.
//!
//! [`AggExecState`] accumulates input tuples in batches and dispatches
//! them through the GPU reduction pipeline (for `GpuReduce` strategy)
//! or performs batched CPU aggregation.
//!
//! # Supported aggregates
//!
//! - `SUM`, `AVG`, `MIN`, `MAX`, `COUNT` via `GpuReduce` strategy.
//! - Falls back to passthrough for unsupported aggregate types.
//!
//! # Lifecycle
//!
//! 1. **`begin_custom_scan`** — allocates `AggExecState`.
//! 2. **`exec_custom_scan`** (repeated) — consumes ALL input, produces
//!    a single aggregate result tuple.
//! 3. **`end_custom_scan`** — reclaims via `Box::from_raw`.

use pgrx::pg_sys;

use crate::engine::registry::AccelStrategy;
use crate::gpu;
use crate::gpu::{PgaccelAggCol, PgaccelAggFunc};

/// Minimum row count to dispatch to GPU reduce kernels.
/// Below this threshold, CPU aggregation is faster due to GPU dispatch overhead.
const GPU_REDUCE_THRESHOLD: u64 = 10_000;

/// Minimum row count for GPU hash aggregation dispatch.
const GPU_HASH_AGG_THRESHOLD: usize = 1_000;

/// Which aggregate operation to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggOp {
    /// SUM aggregate.
    Sum,
    /// AVG aggregate (sum + count).
    Avg,
    /// MIN aggregate.
    Min,
    /// MAX aggregate.
    Max,
    /// COUNT aggregate.
    Count,
    /// Unknown / passthrough.
    Passthrough,
}

/// Encode `AggOp` as an integer for serialization into `custom_private`.
impl AggOp {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::Sum => 0,
            Self::Avg => 1,
            Self::Min => 2,
            Self::Max => 3,
            Self::Count => 4,
            Self::Passthrough => 5,
        }
    }

    #[must_use]
    pub const fn from_i32(v: i32) -> Self {
        match v {
            0 => Self::Sum,
            1 => Self::Avg,
            2 => Self::Min,
            3 => Self::Max,
            4 => Self::Count,
            _ => Self::Passthrough,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-column accumulator
// ---------------------------------------------------------------------------

/// Accumulator state for a single aggregate column.
struct AggColumn {
    op: AggOp,
    /// 1-based attribute number (0 for COUNT(*)).
    attno: i32,
    /// Resolved type OID of the input column (from child tuple descriptor).
    type_oid: pg_sys::Oid,
    /// Result type OID (from Aggref.aggtype). Determines datum format in finalize.
    result_type_oid: pg_sys::Oid,
    /// Running sum (Kahan compensated).
    sum: f64,
    /// Kahan compensation term.
    sum_comp: f64,
    /// Running count.
    count: u64,
    /// Running min.
    min_val: f64,
    /// Running max.
    max_val: f64,
    /// Whether any non-null value has been seen.
    has_value: bool,
    /// Buffer for GPU reduce dispatch.
    gpu_values: Vec<f64>,
    /// Whether GPU reduce was successfully used.
    gpu_dispatched: bool,
}

impl AggColumn {
    fn new(op: AggOp, attno: i32) -> Self {
        Self::with_result_type(op, attno, pg_sys::InvalidOid)
    }

    fn with_result_type(op: AggOp, attno: i32, result_type_oid: pg_sys::Oid) -> Self {
        Self {
            op,
            attno,
            type_oid: pg_sys::InvalidOid,
            result_type_oid,
            sum: 0.0,
            sum_comp: 0.0,
            count: 0,
            min_val: f64::INFINITY,
            max_val: f64::NEG_INFINITY,
            has_value: false,
            gpu_values: Vec::new(),
            gpu_dispatched: false,
        }
    }

    /// Whether this column should buffer values for GPU reduce dispatch.
    fn wants_gpu_buffer(&self, strategy: AccelStrategy) -> bool {
        strategy == AccelStrategy::GpuReduce
            && self.op != AggOp::Count
            && self.op != AggOp::Passthrough
            && self.attno > 0
    }

    /// Add a single value using Kahan summation for SUM/AVG.
    fn accumulate(&mut self, val: f64) {
        match self.op {
            AggOp::Sum | AggOp::Avg => {
                let y = val - self.sum_comp;
                let t = self.sum + y;
                self.sum_comp = (t - self.sum) - y;
                self.sum = t;
            }
            AggOp::Min => {
                if val < self.min_val {
                    self.min_val = val;
                }
            }
            AggOp::Max => {
                if val > self.max_val {
                    self.max_val = val;
                }
            }
            AggOp::Count | AggOp::Passthrough => {}
        }
    }

    /// Dispatch buffered values through GPU reduce, falling back to CPU.
    #[allow(clippy::cast_possible_truncation)]
    fn dispatch_gpu_reduce(&mut self) {
        let n = self.gpu_values.len();
        if (n as u64) < GPU_REDUCE_THRESHOLD {
            self.fallback_cpu_accumulate();
            return;
        }

        // Try f64 GPU path first (CUDA/ROCm with fp64 support).
        let gpu_result = match self.op {
            AggOp::Sum | AggOp::Avg => gpu::reduce_sum_f64(&self.gpu_values),
            AggOp::Min => gpu::reduce_min_f64(&self.gpu_values),
            AggOp::Max => gpu::reduce_max_f64(&self.gpu_values),
            AggOp::Count | AggOp::Passthrough => {
                self.fallback_cpu_accumulate();
                return;
            }
        };

        if let Some(result) = gpu_result {
            self.apply_gpu_result(result);
            return;
        }

        // f64 unsupported (e.g. Metal) — try f32 path with precision loss.
        #[allow(clippy::cast_possible_truncation)]
        let f32_values: Vec<f32> = self.gpu_values.iter().map(|&v| v as f32).collect();

        let f32_result = match self.op {
            AggOp::Sum | AggOp::Avg => gpu::reduce_sum_f32(&f32_values).map(f64::from),
            AggOp::Min => gpu::reduce_min_f32(&f32_values).map(f64::from),
            AggOp::Max => gpu::reduce_max_f32(&f32_values).map(f64::from),
            AggOp::Count | AggOp::Passthrough => None,
        };

        match f32_result {
            Some(result) => self.apply_gpu_result(result),
            None => self.fallback_cpu_accumulate(),
        }
    }

    fn apply_gpu_result(&mut self, result: f64) {
        self.gpu_dispatched = true;
        match self.op {
            AggOp::Sum | AggOp::Avg => self.sum = result,
            AggOp::Min => self.min_val = result,
            AggOp::Max => self.max_val = result,
            AggOp::Count | AggOp::Passthrough => {}
        }
        self.gpu_values = Vec::new();
    }

    fn fallback_cpu_accumulate(&mut self) {
        for val in std::mem::take(&mut self.gpu_values) {
            self.accumulate(val);
        }
    }

    /// Convert a Datum to f64 using the resolved type OID.
    #[allow(clippy::cast_precision_loss)]
    fn datum_to_f64(&self, datum: pg_sys::Datum) -> f64 {
        let raw = datum.value();
        match self.type_oid {
            pg_sys::INT2OID => (raw as i16) as f64,
            pg_sys::INT4OID => (raw as i32) as f64,
            pg_sys::INT8OID => (raw as i64) as f64,
            pg_sys::FLOAT4OID => f32::from_bits(raw as u32) as f64,
            // float8 and unknown: treat as f64 bits.
            _ => f64::from_bits(raw as u64),
        }
    }

    /// Produce the final `(Datum, is_null)` for this column.
    ///
    /// Uses `result_type_oid` to produce correctly-typed datums that match
    /// the Var type declared in the Custom Scan's targetlist. PG interprets
    /// the datum according to that type, so we must encode it correctly.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    fn finalize(&self) -> (pg_sys::Datum, bool) {
        if !self.has_value {
            return match self.op {
                AggOp::Count => (pg_sys::Datum::from(0_i64), false),
                _ => (pg_sys::Datum::from(0), true),
            };
        }

        let raw_f64 = match self.op {
            AggOp::Count => return (pg_sys::Datum::from(self.count as i64), false),
            AggOp::Sum => self.sum,
            AggOp::Avg => {
                if self.count > 0 {
                    self.sum / self.count as f64
                } else {
                    0.0
                }
            }
            AggOp::Min => self.min_val,
            AggOp::Max => self.max_val,
            AggOp::Passthrough => return (pg_sys::Datum::from(0), true),
        };

        // Encode the f64 result into the correct datum format for the
        // declared result type. PG pass-by-value types store bits directly
        // in the Datum.
        let datum = match self.result_type_oid {
            pg_sys::FLOAT4OID => pg_sys::Datum::from((raw_f64 as f32).to_bits()),
            pg_sys::INT2OID => pg_sys::Datum::from(raw_f64 as i16),
            pg_sys::INT4OID => pg_sys::Datum::from(raw_f64 as i32),
            pg_sys::INT8OID => pg_sys::Datum::from(raw_f64 as i64),
            // FLOAT8OID and anything else: store as f64 bits.
            _ => pg_sys::Datum::from(raw_f64.to_bits()),
        };
        (datum, false)
    }
}

// ---------------------------------------------------------------------------
// Group key info for grouped aggregation
// ---------------------------------------------------------------------------

/// Describes the GROUP BY key column for GPU hash aggregation.
#[derive(Debug, Clone)]
pub struct GroupKeyInfo {
    /// 1-based attribute number of the group key column.
    pub attno: i32,
    /// Type OID of the group key column.
    pub type_oid: pg_sys::Oid,
    /// FFI key type tag (0=i32, 1=i64, 2=f64).
    pub key_type: i32,
}

impl GroupKeyInfo {
    /// Map a PG type OID to an FFI key type tag.
    #[must_use]
    pub fn key_type_from_oid(type_oid: pg_sys::Oid) -> Option<i32> {
        match type_oid {
            pg_sys::INT2OID | pg_sys::INT4OID => Some(0), // Int32
            pg_sys::INT8OID => Some(1),                   // Int64
            pg_sys::FLOAT4OID | pg_sys::FLOAT8OID => Some(2), // Float64
            _ => None,
        }
    }

    /// Size in bytes of one key value.
    #[must_use]
    pub const fn key_size(&self) -> usize {
        match self.key_type {
            0 => 4,     // i32
            1 | 2 => 8, // i64 or f64
            _ => 0,
        }
    }
}

/// Cached results from GPU hash aggregation, emitted one tuple at a time.
struct GroupedAggResult {
    /// The GPU hash aggregation result handle.
    result: gpu::HashAggResult,
    /// Index of the next group to emit.
    next_group: usize,
    /// Total number of groups.
    group_count: usize,
    /// FFI key type tag.
    key_type: i32,
}

// ---------------------------------------------------------------------------
// Multi-aggregate executor state
// ---------------------------------------------------------------------------

#[allow(clippy::struct_excessive_bools)]
/// Rust-side aggregate executor state.
///
/// Supports multiple aggregate columns in a single pass over the child
/// plan. Each column accumulates independently; GPU reduce is dispatched
/// per-column after all input is consumed.
pub struct AggExecState {
    /// Acceleration strategy.
    strategy: AccelStrategy,

    /// Batch size for accumulation.
    batch_size: usize,

    /// Per-aggregate-column accumulators.
    columns: Vec<AggColumn>,

    /// Whether all input has been consumed and the result returned.
    result_returned: bool,

    /// Whether the child plan is exhausted.
    child_exhausted: bool,

    /// Whether any column used GPU reduce (for EXPLAIN ANALYZE).
    pub gpu_dispatched: bool,

    // -- Group-by state --
    /// Group key info (present when GROUP BY is active).
    group_key: Option<GroupKeyInfo>,
    /// Cached grouped aggregation result (populated after GPU dispatch).
    grouped_result: Option<GroupedAggResult>,

    // -- Counters for EXPLAIN ANALYZE --
    /// Total rows consumed.
    pub rows_dispatched: u64,
    /// Number of batches processed.
    pub batches_executed: u64,
    /// Cumulative microseconds in dispatch.
    pub dispatch_time_us: u64,
}

impl AggExecState {
    /// Create a new aggregate executor state.
    ///
    /// `agg_descs` is a slice of `(AggOp, attno)` pairs —
    /// one per aggregate in the query's target list. `attno` is the 1-based
    /// attribute number (0 for `COUNT(*)`).
    #[must_use]
    pub fn new(strategy: AccelStrategy, batch_size: usize, agg_descs: &[(AggOp, i32)]) -> Self {
        let columns = agg_descs
            .iter()
            .map(|&(op, attno)| AggColumn::new(op, attno))
            .collect();
        Self {
            strategy,
            batch_size,
            columns,
            result_returned: false,
            child_exhausted: false,
            gpu_dispatched: false,
            group_key: None,
            grouped_result: None,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
        }
    }

    /// Consume all input tuples and produce the aggregate result.
    ///
    /// Drains the entire child plan in batches, computing running
    /// aggregates for all columns. When strategy is `GpuReduce` and the
    /// row count exceeds [`GPU_REDUCE_THRESHOLD`], dispatches to GPU
    /// reduce kernels per column. Returns the final result tuple via
    /// `result_slot`. Subsequent calls return NULL (aggregate produces
    /// exactly one row).
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` and
    /// `result_slot` must be valid pointers.
    #[allow(clippy::too_many_lines)]
    pub unsafe fn next(
        &mut self,
        child_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        if self.result_returned {
            return std::ptr::null_mut();
        }

        // Pre-compute which columns want GPU buffering.
        let gpu_flags: Vec<bool> = self
            .columns
            .iter()
            .map(|c| c.wants_gpu_buffer(self.strategy))
            .collect();

        // Consume all input in batches.
        while !self.child_exhausted {
            let start = std::time::Instant::now();
            let mut batch_count: u64 = 0;

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

                batch_count += 1;

                // Process each aggregate column for this row.
                for (i, col) in self.columns.iter_mut().enumerate() {
                    // COUNT(*): count all rows including NULLs.
                    if col.op == AggOp::Count && col.attno <= 0 {
                        col.count += 1;
                        col.has_value = true;
                        continue;
                    }

                    // No column to extract.
                    if col.attno <= 0 {
                        col.count += 1;
                        col.has_value = true;
                        continue;
                    }

                    // Lazily resolve type OID from child slot descriptor.
                    if col.type_oid == pg_sys::InvalidOid {
                        // SAFETY: child_slot and tupleDescriptor are valid.
                        let tupdesc = unsafe { (*child_slot).tts_tupleDescriptor };
                        if !tupdesc.is_null() {
                            let idx = (col.attno - 1) as usize;
                            let natts = unsafe { (*tupdesc).natts as usize };
                            if idx < natts {
                                let attr = unsafe { &*(*tupdesc).attrs.as_ptr().add(idx) };
                                col.type_oid = attr.atttypid;
                            }
                        }
                    }

                    // Extract the target column datum.
                    let mut is_null: bool = false;
                    // SAFETY: child_slot is valid; attno is 1-based.
                    let datum =
                        unsafe { pg_sys::slot_getattr(child_slot, col.attno, &raw mut is_null) };

                    if is_null {
                        continue;
                    }

                    col.count += 1;
                    col.has_value = true;

                    let val = col.datum_to_f64(datum);

                    if gpu_flags[i] {
                        col.gpu_values.push(val);
                    } else {
                        col.accumulate(val);
                    }
                }
            }

            self.rows_dispatched += batch_count;
            self.batches_executed += 1;
            self.dispatch_time_us += start.elapsed().as_micros() as u64;

            pgrx::check_for_interrupts!();
        }

        // Dispatch GPU reduce for columns that buffered values.
        for col in &mut self.columns {
            if !col.gpu_values.is_empty() {
                col.dispatch_gpu_reduce();
                if col.gpu_dispatched {
                    self.gpu_dispatched = true;
                }
            }
        }

        self.result_returned = true;

        // Build a virtual tuple with all aggregate results.
        // SAFETY: result_slot is a valid TupleTableSlot pointer.
        unsafe {
            pg_sys::ExecClearTuple(result_slot);
            let values = (*result_slot).tts_values;
            let isnull = (*result_slot).tts_isnull;
            for (i, col) in self.columns.iter().enumerate() {
                let (datum, null) = col.finalize();
                *values.add(i) = datum;
                *isnull.add(i) = null;
            }
            pg_sys::ExecStoreVirtualTuple(result_slot);
        }

        result_slot
    }

    /// Returns the acceleration strategy.
    #[must_use]
    pub fn strategy(&self) -> AccelStrategy {
        self.strategy
    }

    /// Returns the number of aggregate columns.
    #[must_use]
    pub fn num_columns(&self) -> usize {
        self.columns.len()
    }

    /// Create a new aggregate executor with result type OIDs.
    ///
    /// `agg_descs` is a slice of `(AggOp, attno, result_type_oid)` triples.
    /// The result type OID (from `Aggref.aggtype`) determines how the
    /// accumulator value is encoded into a Datum in `finalize()`.
    #[must_use]
    pub fn new_with_types(
        strategy: AccelStrategy,
        batch_size: usize,
        agg_descs: &[(AggOp, i32, u32)],
    ) -> Self {
        let columns = agg_descs
            .iter()
            .map(|&(op, attno, rtype)| {
                AggColumn::with_result_type(op, attno, pg_sys::Oid::from(rtype))
            })
            .collect();
        Self {
            strategy,
            batch_size,
            columns,
            result_returned: false,
            child_exhausted: false,
            gpu_dispatched: false,
            group_key: None,
            grouped_result: None,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
        }
    }

    /// Create a new grouped aggregate executor with result type OIDs.
    ///
    /// `group_key_info` describes the GROUP BY column.
    /// `agg_descs` is a slice of `(AggOp, attno, result_type_oid)` triples.
    #[must_use]
    pub fn new_grouped(
        strategy: AccelStrategy,
        batch_size: usize,
        agg_descs: &[(AggOp, i32, u32)],
        group_key_info: GroupKeyInfo,
    ) -> Self {
        let columns = agg_descs
            .iter()
            .map(|&(op, attno, rtype)| {
                AggColumn::with_result_type(op, attno, pg_sys::Oid::from(rtype))
            })
            .collect();
        Self {
            strategy,
            batch_size,
            columns,
            result_returned: false,
            child_exhausted: false,
            gpu_dispatched: false,
            group_key: Some(group_key_info),
            grouped_result: None,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
        }
    }

    /// Returns whether this is a grouped aggregation.
    #[must_use]
    pub fn is_grouped(&self) -> bool {
        self.group_key.is_some()
    }

    /// Returns the group key info, if present.
    #[must_use]
    pub fn group_key_info(&self) -> Option<&GroupKeyInfo> {
        self.group_key.as_ref()
    }

    /// Returns the aggregate descriptors `(AggOp, attno, result_type_oid)` for rescan.
    #[must_use]
    pub fn agg_descs(&self) -> Vec<(AggOp, i32, u32)> {
        self.columns
            .iter()
            .map(|c| (c.op, c.attno, u32::from(c.result_type_oid)))
            .collect()
    }

    /// Emit the next grouped result tuple, or null if exhausted.
    ///
    /// After `execute_grouped_agg` populates `grouped_result`, this method
    /// is called repeatedly to emit one (group_key, agg_results...) tuple
    /// per group.
    ///
    /// # Safety
    ///
    /// `result_slot` must be a valid `TupleTableSlot` pointer.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    unsafe fn emit_grouped_tuple(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let gr = match self.grouped_result.as_mut() {
            Some(gr) if gr.next_group < gr.group_count => gr,
            _ => return std::ptr::null_mut(),
        };

        let gidx = gr.next_group;
        gr.next_group += 1;

        let Some(group_key_info) = &self.group_key else {
            return std::ptr::null_mut();
        };

        // SAFETY: result_slot is a valid TupleTableSlot pointer.
        unsafe {
            pg_sys::ExecClearTuple(result_slot);
            let values = (*result_slot).tts_values;
            let isnull = (*result_slot).tts_isnull;

            // Slot 0: group key value.
            let keys_ptr = gr.result.group_keys_ptr();
            if keys_ptr.is_null() {
                *isnull.add(0) = true;
            } else {
                *isnull.add(0) = false;
                let datum = match gr.key_type {
                    0 => {
                        // SAFETY: keys_ptr points to group_count i32 values.
                        let key = *(keys_ptr.cast::<i32>()).add(gidx);
                        // Encode back to the declared result type.
                        match group_key_info.type_oid {
                            pg_sys::INT2OID => pg_sys::Datum::from(key as i16),
                            _ => pg_sys::Datum::from(key),
                        }
                    }
                    1 => {
                        // SAFETY: keys_ptr points to group_count i64 values.
                        let key = *(keys_ptr.cast::<i64>()).add(gidx);
                        pg_sys::Datum::from(key)
                    }
                    2 => {
                        // SAFETY: keys_ptr points to group_count f64 values.
                        let key = *(keys_ptr.cast::<f64>()).add(gidx);
                        match group_key_info.type_oid {
                            pg_sys::FLOAT4OID => pg_sys::Datum::from((key as f32).to_bits()),
                            _ => pg_sys::Datum::from(key.to_bits()),
                        }
                    }
                    _ => pg_sys::Datum::from(0),
                };
                *values.add(0) = datum;
            }

            // Slots 1..N: aggregate results.
            for (i, col) in self.columns.iter().enumerate() {
                let slot_idx = i + 1;
                if let Some(raw_f64) = gr.result.results(i).and_then(|r| r.get(gidx).copied()) {
                    let datum = if col.op == AggOp::Count {
                        pg_sys::Datum::from(raw_f64 as i64)
                    } else {
                        match col.result_type_oid {
                            pg_sys::FLOAT4OID => pg_sys::Datum::from((raw_f64 as f32).to_bits()),
                            pg_sys::INT2OID => pg_sys::Datum::from(raw_f64 as i16),
                            pg_sys::INT4OID => pg_sys::Datum::from(raw_f64 as i32),
                            pg_sys::INT8OID => pg_sys::Datum::from(raw_f64 as i64),
                            _ => pg_sys::Datum::from(raw_f64.to_bits()),
                        }
                    };
                    *values.add(slot_idx) = datum;
                    *isnull.add(slot_idx) = false;
                } else {
                    *isnull.add(slot_idx) = true;
                }
            }

            pg_sys::ExecStoreVirtualTuple(result_slot);
        }

        result_slot
    }

    /// Execute grouped aggregation via GPU hash agg.
    ///
    /// Consumes all child tuples, extracts columnar arrays, and calls
    /// `gpu::hash_agg_execute`. Populates `self.grouped_result`.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` must be valid.
    #[allow(
        clippy::too_many_lines,
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss
    )]
    unsafe fn execute_grouped_agg(&mut self, child_ps: *mut pg_sys::PlanState) {
        let group_key_info = match &self.group_key {
            Some(info) => info.clone(),
            None => return,
        };

        let key_size = group_key_info.key_size();
        if key_size == 0 {
            return;
        }

        // Buffers for columnar extraction.
        let num_aggs = self.columns.len();
        let mut key_buf: Vec<u8> = Vec::new();
        let mut key_null_mask: Vec<u8> = Vec::new();
        let mut value_bufs: Vec<Vec<u8>> = vec![Vec::new(); num_aggs];
        let mut value_null_masks: Vec<Vec<u8>> = vec![Vec::new(); num_aggs];
        let mut value_type_tags: Vec<i32> = vec![0; num_aggs];

        // Value type tags are resolved from child tupdesc on first row.
        let mut value_types_resolved = false;

        let mut row_count: usize = 0;

        // Consume all input.
        let start = std::time::Instant::now();
        loop {
            // SAFETY: ExecProcNode pulls the next child tuple.
            let child_slot = unsafe { pg_sys::ExecProcNode(child_ps) };
            if child_slot.is_null() {
                break;
            }

            // SAFETY: child_slot is non-null.
            let is_empty = unsafe { (*child_slot).tts_flags & pg_sys::TTS_FLAG_EMPTY as u16 != 0 };
            if is_empty {
                break;
            }

            // Resolve value type tags from child tupdesc on first row.
            if !value_types_resolved {
                // SAFETY: child_slot and tupleDescriptor are valid.
                let tupdesc = unsafe { (*child_slot).tts_tupleDescriptor };
                if !tupdesc.is_null() {
                    let natts = unsafe { (*tupdesc).natts as usize };
                    for (i, col) in self.columns.iter_mut().enumerate() {
                        if col.attno > 0 {
                            let idx = (col.attno - 1) as usize;
                            if idx < natts {
                                let attr = unsafe { &*(*tupdesc).attrs.as_ptr().add(idx) };
                                col.type_oid = attr.atttypid;
                                value_type_tags[i] = oid_to_val_tag(attr.atttypid);
                            }
                        }
                    }
                }
                value_types_resolved = true;
            }

            row_count += 1;

            // Extract group key.
            let mut key_is_null: bool = false;
            // SAFETY: child_slot is valid; attno is 1-based.
            let key_datum = unsafe {
                pg_sys::slot_getattr(child_slot, group_key_info.attno, &raw mut key_is_null)
            };

            key_null_mask.push(u8::from(key_is_null));

            if key_is_null {
                key_buf.extend_from_slice(&vec![0u8; key_size]);
            } else {
                append_key_bytes(
                    &mut key_buf,
                    key_datum,
                    group_key_info.key_type,
                    group_key_info.type_oid,
                );
            }

            // Extract value columns for each aggregate.
            for (i, col) in self.columns.iter().enumerate() {
                if col.op == AggOp::Count && col.attno <= 0 {
                    // COUNT(*): no value column, just pad with zeros.
                    value_bufs[i].extend_from_slice(&0f64.to_ne_bytes());
                    value_null_masks[i].push(0);
                    continue;
                }

                if col.attno <= 0 {
                    value_bufs[i].extend_from_slice(&0f64.to_ne_bytes());
                    value_null_masks[i].push(1);
                    continue;
                }

                let mut val_is_null: bool = false;
                // SAFETY: child_slot is valid.
                let val_datum =
                    unsafe { pg_sys::slot_getattr(child_slot, col.attno, &raw mut val_is_null) };

                value_null_masks[i].push(u8::from(val_is_null));

                if val_is_null {
                    value_bufs[i].extend_from_slice(&0f64.to_ne_bytes());
                } else {
                    append_value_bytes(
                        &mut value_bufs[i],
                        val_datum,
                        value_type_tags[i],
                        col.type_oid,
                    );
                }
            }

            if row_count.is_multiple_of(self.batch_size) {
                pgrx::check_for_interrupts!();
            }
        }

        self.rows_dispatched = row_count as u64;
        self.batches_executed = 1;
        self.dispatch_time_us = start.elapsed().as_micros() as u64;

        if row_count == 0 {
            return;
        }

        // Build FFI agg_col descriptors.
        let ffi_agg_cols: Vec<PgaccelAggCol> = self
            .columns
            .iter()
            .enumerate()
            .map(|(i, col)| PgaccelAggCol {
                func: agg_op_to_ffi(col.op),
                col_idx: if col.op == AggOp::Count && col.attno <= 0 {
                    usize::MAX // COUNT(*)
                } else {
                    i
                },
            })
            .collect();

        // Build pointer arrays for FFI.
        let value_col_ptrs: Vec<*const std::ffi::c_void> = value_bufs
            .iter()
            .map(|buf| buf.as_ptr().cast::<std::ffi::c_void>())
            .collect();
        let value_null_ptrs: Vec<*const u8> = value_null_masks.iter().map(Vec::as_ptr).collect();

        let dispatch_start = std::time::Instant::now();

        let result = gpu::hash_agg_execute(
            key_buf.as_ptr().cast(),
            key_null_mask.as_ptr(),
            row_count,
            group_key_info.key_type,
            &value_col_ptrs,
            &value_null_ptrs,
            &value_type_tags,
            &ffi_agg_cols,
        );

        self.dispatch_time_us += dispatch_start.elapsed().as_micros() as u64;

        if let Some(hash_result) = result {
            let group_count = hash_result.group_count();
            self.gpu_dispatched = true;
            pgrx::debug1!(
                "pg_accel: hash_agg: {} groups from {} rows",
                group_count,
                row_count
            );
            self.grouped_result = Some(GroupedAggResult {
                result: hash_result,
                next_group: 0,
                group_count,
                key_type: group_key_info.key_type,
            });
        } else {
            pgrx::debug1!("pg_accel: hash_agg: GPU dispatch failed, no results");
        }
    }

    /// Grouped-mode `next`: consume all input, then emit one group per call.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. All pointers must be valid.
    pub unsafe fn next_grouped(
        &mut self,
        child_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        // First call: consume all input and run hash aggregation.
        if !self.child_exhausted {
            self.child_exhausted = true;
            // SAFETY: caller guarantees child_ps is valid.
            unsafe { self.execute_grouped_agg(child_ps) };
        }

        // Emit one result tuple per call.
        // SAFETY: caller guarantees result_slot is valid.
        unsafe { self.emit_grouped_tuple(result_slot) }
    }
}

// ---------------------------------------------------------------------------
// Helper functions for columnar extraction
// ---------------------------------------------------------------------------

/// Map a PG type OID to the C value type tag used by `pgaccel_hash_agg`.
///
/// Tags: 0=Null, 1=Bool, 2=Int32, 3=Int64, 4=Float32, 5=Float64.
fn oid_to_val_tag(type_oid: pg_sys::Oid) -> i32 {
    match type_oid {
        pg_sys::BOOLOID => 1,
        pg_sys::INT2OID | pg_sys::INT4OID => 2,
        pg_sys::INT8OID => 3,
        pg_sys::FLOAT4OID => 4,
        pg_sys::FLOAT8OID => 5,
        _ => 0,
    }
}

/// Map `AggOp` to the FFI `PgaccelAggFunc` enum.
const fn agg_op_to_ffi(op: AggOp) -> PgaccelAggFunc {
    match op {
        AggOp::Sum | AggOp::Avg => PgaccelAggFunc::Sum,
        AggOp::Min => PgaccelAggFunc::Min,
        AggOp::Max => PgaccelAggFunc::Max,
        AggOp::Count | AggOp::Passthrough => PgaccelAggFunc::Count,
    }
}

/// Append a group key datum as raw bytes into `buf`.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn append_key_bytes(buf: &mut Vec<u8>, datum: pg_sys::Datum, key_type: i32, type_oid: pg_sys::Oid) {
    let raw = datum.value();
    match key_type {
        0 => {
            // i32 key
            let val: i32 = match type_oid {
                pg_sys::INT2OID => (raw as i16) as i32,
                _ => raw as i32,
            };
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        1 => {
            // i64 key
            let val = raw as i64;
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        2 => {
            // f64 key
            let val: f64 = match type_oid {
                pg_sys::FLOAT4OID => f64::from(f32::from_bits(raw as u32)),
                _ => f64::from_bits(raw as u64),
            };
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        _ => {}
    }
}

/// Append a value datum as typed bytes into `buf`.
///
/// The value is stored according to the C value type tag so the kernel
/// can read it with the correct stride.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn append_value_bytes(
    buf: &mut Vec<u8>,
    datum: pg_sys::Datum,
    val_tag: i32,
    type_oid: pg_sys::Oid,
) {
    let raw = datum.value();
    match val_tag {
        1 => {
            // Bool → stored as bool (1 byte) padded? No — the C kernel reads typed arrays.
            // val_tag=1 means bool, but C reads as `bool*` which is 1 byte.
            buf.push(u8::from(raw != 0));
        }
        2 => {
            // Int32
            let val: i32 = match type_oid {
                pg_sys::INT2OID => (raw as i16) as i32,
                _ => raw as i32,
            };
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        3 => {
            // Int64
            let val = raw as i64;
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        4 => {
            // Float32
            let val = f32::from_bits(raw as u32);
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        5 => {
            // Float64
            let val = f64::from_bits(raw as u64);
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        _ => {
            // Unknown type — store 8 zero bytes.
            buf.extend_from_slice(&[0u8; 8]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Result type OIDs for test convenience.
    const F8: u32 = 701; // FLOAT8OID
    const I8: u32 = 20; // INT8OID

    /// Test helper: create AggColumn with FLOAT8 result type.
    fn tcol(op: AggOp, attno: i32) -> AggColumn {
        AggColumn::with_result_type(op, attno, pg_sys::Oid::from(F8))
    }

    #[test]
    fn new_state_defaults() {
        let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Sum, 1)]);
        assert_eq!(state.strategy(), AccelStrategy::GpuReduce);
        assert_eq!(state.num_columns(), 1);
        assert!(!state.result_returned);
        assert!(!state.child_exhausted);
    }

    #[test]
    fn multi_column_construction() {
        let descs = vec![
            (AggOp::Sum, 1, 701),
            (AggOp::Min, 1, 701),
            (AggOp::Max, 1, 701),
            (AggOp::Count, 0, 20),
        ];
        let state = AggExecState::new_with_types(AccelStrategy::GpuReduce, 256, &descs);
        assert_eq!(state.num_columns(), 4);
        assert_eq!(state.columns[0].op, AggOp::Sum);
        assert_eq!(state.columns[1].op, AggOp::Min);
        assert_eq!(state.columns[2].op, AggOp::Max);
        assert_eq!(state.columns[3].op, AggOp::Count);
    }

    #[test]
    fn agg_descs_roundtrip() {
        let descs = vec![(AggOp::Avg, 2, 701), (AggOp::Count, 0, 20)];
        let state = AggExecState::new_with_types(AccelStrategy::GpuReduce, 128, &descs);
        assert_eq!(state.agg_descs(), descs);
    }

    #[test]
    fn count_starts_at_zero() {
        let state = AggExecState::new(AccelStrategy::GpuReduce, 1024, &[(AggOp::Count, 0)]);
        assert_eq!(state.columns[0].count, 0);
    }

    #[test]
    fn min_max_initial_values() {
        let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Min, 1)]);
        assert_eq!(state.columns[0].min_val, f64::INFINITY);
        assert_eq!(state.columns[0].max_val, f64::NEG_INFINITY);
    }

    #[test]
    fn all_agg_ops_constructible() {
        for op in [
            AggOp::Sum,
            AggOp::Avg,
            AggOp::Min,
            AggOp::Max,
            AggOp::Count,
            AggOp::Passthrough,
        ] {
            let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(op, 1)]);
            assert_eq!(state.columns[0].op, op);
        }
    }

    #[test]
    fn has_value_false_initially() {
        let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Avg, 1)]);
        assert!(!state.columns[0].has_value);
    }

    #[test]
    fn result_returned_false_initially() {
        let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Count, 0)]);
        assert!(!state.result_returned);
    }

    #[test]
    fn batch_size_stored() {
        let state = AggExecState::new(AccelStrategy::GpuReduce, 4096, &[(AggOp::Sum, 1)]);
        assert_eq!(state.batch_size, 4096);
    }

    #[test]
    fn counters_zero_on_init() {
        let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Max, 1)]);
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
        assert_eq!(state.dispatch_time_us, 0);
    }

    #[test]
    fn kahan_compensation_starts_zero() {
        let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Avg, 1)]);
        assert_eq!(state.columns[0].sum_comp, 0.0);
    }

    #[test]
    fn min_max_boundary_values() {
        let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Min, 1)]);
        assert!(state.columns[0].min_val.is_infinite() && state.columns[0].min_val > 0.0);
        assert!(state.columns[0].max_val.is_infinite() && state.columns[0].max_val < 0.0);
    }

    #[test]
    fn passthrough_agg_op() {
        let state = AggExecState::new(AccelStrategy::BatchedEval, 256, &[(AggOp::Passthrough, 0)]);
        assert_eq!(state.strategy(), AccelStrategy::BatchedEval);
        assert_eq!(state.columns[0].op, AggOp::Passthrough);
    }

    #[test]
    fn agg_op_debug_display() {
        let op = AggOp::Sum;
        let debug_str = format!("{op:?}");
        assert_eq!(debug_str, "Sum");
    }

    #[test]
    fn agg_op_clone_and_copy() {
        let op = AggOp::Avg;
        let cloned = op;
        assert_eq!(op, cloned);
    }

    #[test]
    fn all_strategies_constructible_for_agg() {
        for strategy in [
            AccelStrategy::BatchedEval,
            AccelStrategy::GpuSpatial,
            AccelStrategy::GpuRaster,
            AccelStrategy::GpuH3,
            AccelStrategy::GpuSort,
            AccelStrategy::GpuReduce,
        ] {
            let state = AggExecState::new(strategy, 128, &[(AggOp::Count, 0)]);
            assert_eq!(state.strategy(), strategy);
        }
    }

    #[test]
    fn large_batch_agg() {
        let state = AggExecState::new(AccelStrategy::GpuReduce, 1_000_000, &[(AggOp::Sum, 1)]);
        assert_eq!(state.batch_size, 1_000_000);
        assert!(!state.child_exhausted);
    }

    // -- AggColumn accumulate + finalize (unit-testable without PG) -----------

    #[test]
    fn accumulate_sum_basic() {
        let mut col = tcol(AggOp::Sum, 1);
        col.accumulate(1.0);
        col.accumulate(2.0);
        col.accumulate(3.0);
        assert!((col.sum - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accumulate_sum_kahan_precision() {
        let mut col = tcol(AggOp::Sum, 1);
        col.accumulate(1e16);
        for _ in 0..10_000 {
            col.accumulate(1.0);
        }
        col.accumulate(-1e16);
        assert!(
            (col.sum - 10_000.0).abs() < 1.0,
            "Kahan sum should be ~10000, got {}",
            col.sum
        );
    }

    #[test]
    fn accumulate_min() {
        let mut col = tcol(AggOp::Min, 1);
        col.accumulate(5.0);
        col.accumulate(2.0);
        col.accumulate(8.0);
        assert!((col.min_val - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accumulate_max() {
        let mut col = tcol(AggOp::Max, 1);
        col.accumulate(5.0);
        col.accumulate(2.0);
        col.accumulate(8.0);
        assert!((col.max_val - 8.0).abs() < f64::EPSILON);
    }

    #[test]
    fn finalize_count_no_values() {
        let col = tcol(AggOp::Count, 0);
        let (datum, is_null) = col.finalize();
        assert!(!is_null);
        assert_eq!(datum.value(), 0);
    }

    #[test]
    fn finalize_count_with_values() {
        let mut col = tcol(AggOp::Count, 0);
        col.count = 42;
        col.has_value = true;
        let (datum, is_null) = col.finalize();
        assert!(!is_null);
        assert_eq!(datum.value(), 42);
    }

    #[test]
    fn finalize_sum_no_values_is_null() {
        let col = tcol(AggOp::Sum, 1);
        let (_, is_null) = col.finalize();
        assert!(is_null);
    }

    #[test]
    fn finalize_sum_with_values() {
        let mut col = tcol(AggOp::Sum, 1);
        col.accumulate(3.0);
        col.accumulate(7.0);
        col.has_value = true;
        let (datum, is_null) = col.finalize();
        assert!(!is_null);
        let val = f64::from_bits(datum.value() as u64);
        assert!((val - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn finalize_avg_with_values() {
        let mut col = tcol(AggOp::Avg, 1);
        col.accumulate(2.0);
        col.accumulate(4.0);
        col.accumulate(6.0);
        col.count = 3;
        col.has_value = true;
        let (datum, is_null) = col.finalize();
        assert!(!is_null);
        let val = f64::from_bits(datum.value() as u64);
        assert!((val - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn finalize_min_with_values() {
        let mut col = tcol(AggOp::Min, 1);
        col.accumulate(10.0);
        col.accumulate(3.0);
        col.has_value = true;
        let (datum, is_null) = col.finalize();
        assert!(!is_null);
        let val = f64::from_bits(datum.value() as u64);
        assert!((val - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn finalize_passthrough_returns_null() {
        let col = tcol(AggOp::Passthrough, 0);
        let (_, is_null) = col.finalize();
        assert!(is_null);
    }

    // -- GPU reduce dispatch + fallback tests --------------------------------

    #[test]
    fn gpu_values_empty_initially() {
        let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Sum, 1)]);
        assert!(state.columns[0].gpu_values.is_empty());
        assert!(!state.gpu_dispatched);
    }

    #[test]
    fn fallback_cpu_accumulate_sum() {
        let mut col = tcol(AggOp::Sum, 1);
        col.gpu_values = vec![1.0, 2.0, 3.0];
        col.has_value = true;
        col.fallback_cpu_accumulate();
        assert!((col.sum - 6.0).abs() < f64::EPSILON);
        assert!(col.gpu_values.is_empty());
    }

    #[test]
    fn fallback_cpu_accumulate_min() {
        let mut col = tcol(AggOp::Min, 1);
        col.gpu_values = vec![5.0, 2.0, 8.0];
        col.has_value = true;
        col.fallback_cpu_accumulate();
        assert!((col.min_val - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn fallback_cpu_accumulate_max() {
        let mut col = tcol(AggOp::Max, 1);
        col.gpu_values = vec![5.0, 2.0, 8.0];
        col.has_value = true;
        col.fallback_cpu_accumulate();
        assert!((col.max_val - 8.0).abs() < f64::EPSILON);
    }

    #[test]
    fn dispatch_gpu_reduce_below_threshold_falls_back() {
        let mut col = tcol(AggOp::Sum, 1);
        col.gpu_values = vec![1.0, 2.0, 3.0];
        col.has_value = true;
        col.dispatch_gpu_reduce();
        assert!(!col.gpu_dispatched);
        assert!((col.sum - 6.0).abs() < f64::EPSILON);
        assert!(col.gpu_values.is_empty());
    }

    #[test]
    fn dispatch_gpu_reduce_above_threshold_attempts_gpu() {
        let mut col = tcol(AggOp::Sum, 1);
        let n = (GPU_REDUCE_THRESHOLD as usize) + 100;
        col.gpu_values = vec![1.0; n];
        col.has_value = true;
        col.dispatch_gpu_reduce();
        #[cfg(not(feature = "gpu"))]
        {
            assert!(!col.gpu_dispatched);
            #[allow(clippy::cast_precision_loss)]
            let expected = n as f64;
            assert!((col.sum - expected).abs() < 1.0);
        }
        assert!(col.gpu_values.is_empty());
    }

    #[test]
    fn dispatch_gpu_reduce_min_above_threshold() {
        let mut col = tcol(AggOp::Min, 1);
        let n = (GPU_REDUCE_THRESHOLD as usize) + 100;
        let mut vals = vec![100.0; n];
        vals[n / 2] = -42.0;
        col.gpu_values = vals;
        col.has_value = true;
        col.dispatch_gpu_reduce();
        #[cfg(not(feature = "gpu"))]
        {
            assert!(!col.gpu_dispatched);
            assert!((col.min_val - (-42.0)).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn dispatch_gpu_reduce_max_above_threshold() {
        let mut col = tcol(AggOp::Max, 1);
        let n = (GPU_REDUCE_THRESHOLD as usize) + 100;
        let mut vals = vec![1.0; n];
        vals[n / 3] = 9999.0;
        col.gpu_values = vals;
        col.has_value = true;
        col.dispatch_gpu_reduce();
        #[cfg(not(feature = "gpu"))]
        {
            assert!(!col.gpu_dispatched);
            assert!((col.max_val - 9999.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn dispatch_gpu_reduce_avg_uses_sum_path() {
        let mut col = tcol(AggOp::Avg, 1);
        col.gpu_values = vec![2.0, 4.0, 6.0];
        col.count = 3;
        col.has_value = true;
        col.dispatch_gpu_reduce();
        assert!(!col.gpu_dispatched);
        assert!((col.sum - 12.0).abs() < f64::EPSILON);
        let (datum, is_null) = col.finalize();
        assert!(!is_null);
        let avg = f64::from_bits(datum.value() as u64);
        assert!((avg - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn dispatch_gpu_reduce_count_does_not_buffer() {
        let col = tcol(AggOp::Count, 0);
        assert!(col.gpu_values.is_empty());
    }

    #[test]
    fn dispatch_gpu_reduce_passthrough_does_not_buffer() {
        let col = tcol(AggOp::Passthrough, 0);
        assert!(col.gpu_values.is_empty());
    }

    #[test]
    fn fallback_cpu_accumulate_empty_buffer_is_noop() {
        let mut col = tcol(AggOp::Sum, 1);
        col.fallback_cpu_accumulate();
        assert!((col.sum - 0.0).abs() < f64::EPSILON);
        assert!(col.gpu_values.is_empty());
    }

    // -- AggOp serialization roundtrip ----------------------------------------

    #[test]
    fn agg_op_roundtrip() {
        for op in [
            AggOp::Sum,
            AggOp::Avg,
            AggOp::Min,
            AggOp::Max,
            AggOp::Count,
            AggOp::Passthrough,
        ] {
            assert_eq!(AggOp::from_i32(op.to_i32()), op);
        }
    }
}
