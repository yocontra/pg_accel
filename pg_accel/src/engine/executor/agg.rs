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

use crate::engine::cost;
use crate::engine::executor::tuple_extract::{self, AttExtractInfo};
use crate::engine::executor::vectorized_scan::VectorizedScan;
use crate::engine::expr_compiler::{self, CompiledExpr, TemplateKernel};
use crate::engine::registry::AccelStrategy;
use crate::gpu;
use crate::gpu::{PgaccelAggCol, PgaccelAggFunc};

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

    /// Dispatch buffered values through GPU reduce.
    ///
    /// For small batches below the GPU threshold, accumulates directly
    /// (single-pass, no GPU overhead). For Count/Passthrough ops that
    /// never need GPU, drains the buffer directly. GPU failure for
    /// reducible ops (Sum/Avg/Min/Max) logs a warning — the planner
    /// should not have injected a GpuReduce path if GPU is unavailable.
    #[allow(clippy::cast_possible_truncation)]
    fn dispatch_gpu_reduce(&mut self) {
        let n = self.gpu_values.len();
        let limits = cost::device_limits();
        if n < limits.gpu_reduce_min_rows {
            // Small batch: single-pass accumulation (no GPU overhead).
            self.drain_small_batch();
            return;
        }

        // Process in chunks to avoid SYCL runtime limits on large ranges.
        if n > limits.gpu_reduce_max_chunk {
            self.dispatch_gpu_reduce_chunked();
            return;
        }

        // Try f64 GPU path first (CUDA/ROCm with fp64 support).
        let gpu_result = match self.op {
            AggOp::Sum | AggOp::Avg => gpu::reduce_sum_f64(&self.gpu_values),
            AggOp::Min => gpu::reduce_min_f64(&self.gpu_values),
            AggOp::Max => gpu::reduce_max_f64(&self.gpu_values),
            AggOp::Count | AggOp::Passthrough => {
                // Count/Passthrough never need GPU — drain directly.
                self.drain_small_batch();
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

        if let Some(result) = f32_result {
            self.apply_gpu_result(result);
        } else {
            // GPU reduce failed for a reducible op. This should not happen
            // if the planner correctly verified GPU availability. Log a
            // warning and drain values so the query still completes, but
            // the result may differ from what a pure GPU path would produce.
            pgrx::warning!(
                "pg_accel: GPU reduce unavailable for {:?} with {} values; \
                 planner should not have injected GpuReduce path",
                self.op,
                n,
            );
            self.drain_small_batch();
        }
    }

    /// Dispatch GPU reduce in chunks, combining partial results.
    ///
    /// Each chunk is reduced on GPU independently; partial results are
    /// combined via the accumulator. If GPU fails for any chunk, a warning
    /// is logged — the planner should not have injected this path.
    #[allow(clippy::cast_possible_truncation)]
    fn dispatch_gpu_reduce_chunked(&mut self) {
        let max_chunk = cost::device_limits().gpu_reduce_max_chunk;
        let values = std::mem::take(&mut self.gpu_values);
        let mut any_gpu_failure = false;
        for chunk in values.chunks(max_chunk) {
            let gpu_result = match self.op {
                AggOp::Sum | AggOp::Avg => gpu::reduce_sum_f64(chunk),
                AggOp::Min => gpu::reduce_min_f64(chunk),
                AggOp::Max => gpu::reduce_max_f64(chunk),
                AggOp::Count | AggOp::Passthrough => None,
            };

            if let Some(partial) = gpu_result {
                self.gpu_dispatched = true;
                self.accumulate(partial);
            } else {
                // GPU failed for this chunk — use CPU Kahan accumulation.
                // On fp32-only GPUs (Metal), f64 reduce is unavailable and
                // f32 sum over large chunks loses too much precision anyway.
                any_gpu_failure = true;
                for &val in chunk {
                    self.accumulate(val);
                }
            }
        }
        if any_gpu_failure {
            pgrx::debug1!(
                "pg_accel: GPU reduce unavailable for {:?} chunks, using CPU Kahan",
                self.op,
            );
        }
        self.has_value = true;
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

    /// Drain the GPU value buffer through the scalar accumulator.
    ///
    /// Used for small batches below the GPU threshold and for
    /// Count/Passthrough ops that never need GPU dispatch.
    fn drain_small_batch(&mut self) {
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
    /// 0-based position of the group key in the output target list.
    /// The executor places the group key datum at this slot index.
    group_key_tlist_pos: usize,
    /// Cached grouped aggregation result (populated after GPU dispatch).
    grouped_result: Option<GroupedAggResult>,

    // -- Counters for EXPLAIN ANALYZE --
    /// Total rows consumed.
    pub rows_dispatched: u64,
    /// Number of batches processed.
    pub batches_executed: u64,
    /// Cumulative microseconds in dispatch.
    pub dispatch_time_us: u64,

    // -- Pipeline fusion state (scan+agg) --
    /// When `true`, this agg node performs its own heap walk instead of
    /// pulling tuples from the child plan via `ExecProcNode`. This
    /// eliminates the per-tuple `MinimalTuple` copy and slot deformation
    /// overhead.
    pub is_fused: bool,
    /// Table scan descriptor for the fused heap walk. Only valid when
    /// `is_fused` is `true`.
    fused_scan_desc: pg_sys::TableScanDesc,
    /// Compiled filter expression from the child GpuExpr scan. `None`
    /// means no filter (all rows pass).
    fused_expr: Option<expr_compiler::CompiledExpr>,
    /// Maps child-scan output attno (1-based) to base-table attno (1-based).
    /// The child scan's target list may project a subset of columns, so
    /// the agg's column attnos reference the child's output positions,
    /// not the base table. The fused path walks the base table directly
    /// and needs this mapping to find the correct columns.
    fused_attno_map: Vec<i32>,
    /// Cached extraction info for filter columns (lazily initialized).
    fused_filter_infos: Option<Vec<AttExtractInfo>>,
    /// Cached extraction info for aggregate columns (lazily initialized).
    fused_agg_infos: Option<Vec<AttExtractInfo>>,

    // -- Vectorized scan state (self-scanning pipeline) --
    /// When `Some`, this agg node scans the base table directly using
    /// the arena-based vectorized pipeline instead of pulling tuples
    /// from a child plan via `ExecProcNode`.
    vscan: Option<VectorizedScan>,
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
            group_key_tlist_pos: 0,
            grouped_result: None,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            is_fused: false,
            fused_scan_desc: std::ptr::null_mut(),
            fused_expr: None,
            fused_attno_map: Vec::new(),
            fused_filter_infos: None,
            fused_agg_infos: None,
            vscan: None,
        }
    }

    /// Consume all input tuples and produce the aggregate result.
    ///
    /// Drains the entire child plan in batches, computing running
    /// aggregates for all columns. When strategy is `GpuReduce` and the
    /// row count exceeds the device-derived reduce threshold, dispatches to GPU
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
        let _span = tracing::debug_span!("exec.agg_next").entered();
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
            let batch_count: u64;

            // Phase 1: Buffer MinimalTuples for the batch.
            let mut tuples: Vec<pg_sys::MinimalTuple> = Vec::with_capacity(self.batch_size);
            let mut last_child_slot: *mut pg_sys::TupleTableSlot = std::ptr::null_mut();

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

                // SAFETY: child_slot is valid; copies the tuple into palloc'd memory.
                let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(child_slot) };
                tuples.push(mt);
                last_child_slot = child_slot;

                // Lazily resolve type OIDs on the first row of the batch.
                if tuples.len() == 1 {
                    // SAFETY: child_slot is valid and non-null.
                    let tupdesc = unsafe { (*child_slot).tts_tupleDescriptor };
                    if !tupdesc.is_null() {
                        for col in &mut self.columns {
                            if col.type_oid == pg_sys::InvalidOid && col.attno > 0 {
                                let idx = (col.attno - 1) as usize;
                                // SAFETY: tupdesc is valid.
                                let natts = unsafe { (*tupdesc).natts as usize };
                                if idx < natts {
                                    // SAFETY: idx < natts so attrs[idx] is valid.
                                    let attr = unsafe { &*(*tupdesc).attrs.as_ptr().add(idx) };
                                    col.type_oid = attr.atttypid;
                                }
                            }
                        }
                    }
                }
            }

            batch_count = tuples.len() as u64;

            // Handle columns that don't need extraction (COUNT(*), attno <= 0).
            for col in &mut self.columns {
                if col.attno <= 0 {
                    col.count += batch_count;
                    if batch_count > 0 {
                        col.has_value = true;
                    }
                }
            }

            // Phase 2: Bulk columnar extraction for columns with attno > 0.
            if !tuples.is_empty() && !last_child_slot.is_null() {
                // SAFETY: last_child_slot is valid; tts_tupleDescriptor is set.
                let tupdesc = unsafe { (*last_child_slot).tts_tupleDescriptor };
                for (i, col) in self.columns.iter_mut().enumerate() {
                    if col.attno <= 0 {
                        continue;
                    }

                    // SAFETY: tupdesc is valid, col.attno is 1-based and within range.
                    let info = unsafe { AttExtractInfo::new(tupdesc, col.attno) };
                    // SAFETY: tuples are valid MinimalTuples, info matches schema,
                    // last_child_slot is a valid TupleTableSlot on main thread.
                    let (values, nulls) =
                        unsafe { tuple_extract::extract_f64(&tuples, &info, last_child_slot) };

                    for (j, &val) in values.iter().enumerate() {
                        if nulls[j] == 1 {
                            continue;
                        }

                        col.count += 1;
                        col.has_value = true;

                        if gpu_flags[i] {
                            col.gpu_values.push(val);
                        } else {
                            col.accumulate(val);
                        }
                    }
                }
            }

            // Free palloc'd MinimalTuples.
            for mt in &tuples {
                // SAFETY: each mt was allocated by ExecCopySlotMinimalTuple (palloc'd).
                // SAFETY: mt is *mut MinimalTupleData, cast to *mut c_void for pfree.
                unsafe { pg_sys::pfree(mt.cast()) };
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
            group_key_tlist_pos: 0,
            grouped_result: None,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            is_fused: false,
            fused_scan_desc: std::ptr::null_mut(),
            fused_expr: None,
            fused_attno_map: Vec::new(),
            fused_filter_infos: None,
            fused_agg_infos: None,
            vscan: None,
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
        group_key_tlist_pos: usize,
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
            group_key_tlist_pos,
            grouped_result: None,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            is_fused: false,
            fused_scan_desc: std::ptr::null_mut(),
            fused_expr: None,
            fused_attno_map: Vec::new(),
            fused_filter_infos: None,
            fused_agg_infos: None,
            vscan: None,
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

    // -- Pipeline fusion (scan+agg) ------------------------------------------

    /// Configure pipeline fusion: the agg walks the heap itself instead of
    /// pulling tuples from the child plan.
    ///
    /// `scan_desc` is the table scan descriptor from the child `ScanExecState`.
    /// `expr` is the compiled filter expression (or `None` for no filter).
    pub fn set_fused_context(
        &mut self,
        scan_desc: pg_sys::TableScanDesc,
        expr: Option<expr_compiler::CompiledExpr>,
        attno_map: Vec<i32>,
    ) {
        self.is_fused = true;
        self.fused_scan_desc = scan_desc;
        self.fused_expr = expr;
        self.fused_attno_map = attno_map;
    }

    // -- Vectorized scan pipeline (self-scanning) --------------------------

    /// Set the vectorized scan for self-scanning mode.
    pub fn set_vscan(&mut self, vscan: VectorizedScan) {
        self.vscan = Some(vscan);
    }

    /// Whether this agg has a vectorized scan (self-scanning mode).
    #[must_use]
    pub fn has_vscan(&self) -> bool {
        self.vscan.is_some()
    }

    /// Return the scan descriptor from the vectorized scan, if any.
    /// Used by `end_custom_scan` to close the heap scan.
    #[must_use]
    pub fn vscan_scan_desc(&self) -> pg_sys::TableScanDesc {
        self.vscan
            .as_ref()
            .map_or(std::ptr::null_mut(), |v| v.scan_desc())
    }

    /// Vectorized scan+reduce: scan the base table via arena-based
    /// vectorized pipeline, extract columns in bulk, dispatch GPU reduce.
    ///
    /// This is the universal pipeline: arena heap scan → columnar extract
    /// → GPU compute. Same architecture as hash join's bulk consume path.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `result_slot` must be
    /// a valid `TupleTableSlot`.
    #[allow(clippy::cast_precision_loss)]
    pub unsafe fn next_vectorized(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.agg_vectorized").entered();
        if self.result_returned {
            return std::ptr::null_mut();
        }

        let start = std::time::Instant::now();

        let vscan = self.vscan.as_mut().expect("vscan must be set");

        // SAFETY: scan_desc is valid, main backend thread.
        let scan_desc = vscan.scan_desc();
        let tupdesc = unsafe { (*scan_desc).rs_rd.as_ref() }
            .map(|rd| rd.rd_att)
            .unwrap_or(std::ptr::null_mut());

        // Pre-compute extraction info for each column that needs values.
        let infos: Vec<Option<AttExtractInfo>> = self
            .columns
            .iter()
            .map(|col| {
                if col.attno > 0 && !tupdesc.is_null() {
                    // SAFETY: tupdesc is valid.
                    Some(unsafe { AttExtractInfo::new(tupdesc, col.attno) })
                } else {
                    None
                }
            })
            .collect();

        // Resolve type OIDs from extraction info.
        for (col, info) in self.columns.iter_mut().zip(infos.iter()) {
            if let Some(info) = &info {
                if col.type_oid == pg_sys::InvalidOid {
                    col.type_oid = info.typid;
                }
            }
        }

        // Single-pass fused scan+extract+accumulate. No arena, no
        // intermediate buffers. Each tuple is read from the heap,
        // column values are extracted inline, and accumulated
        // immediately. This eliminates the arena copy overhead and
        // the second-pass extraction.
        let mut total = 0u64;
        loop {
            // SAFETY: scan_desc is valid; main backend thread.
            let htup = unsafe {
                pg_sys::heap_getnext(
                    scan_desc,
                    pg_sys::ScanDirection::ForwardScanDirection,
                )
            };
            if htup.is_null() {
                break;
            }

            total += 1;

            // SAFETY: htup is valid from heap_getnext.
            let hdr = unsafe { (*htup).t_data };

            for (col, info) in self.columns.iter_mut().zip(infos.iter()) {
                if col.attno <= 0 {
                    // COUNT(*)
                    col.count += 1;
                    col.has_value = true;
                    continue;
                }

                let info = match info {
                    Some(i) => i,
                    None => continue,
                };

                // SAFETY: hdr is valid, info matches schema.
                // Type-dispatch: read as native type, convert to f64.
                let val: Option<f64> = unsafe {
                    match info.typid {
                        t if t == pg_sys::FLOAT4OID => {
                            tuple_extract::try_fast_read_heap_pub::<f32>(hdr, info)
                                .map(f64::from)
                        }
                        t if t == pg_sys::INT2OID => {
                            tuple_extract::try_fast_read_heap_pub::<i16>(hdr, info)
                                .map(f64::from)
                        }
                        t if t == pg_sys::INT4OID => {
                            tuple_extract::try_fast_read_heap_pub::<i32>(hdr, info)
                                .map(f64::from)
                        }
                        t if t == pg_sys::INT8OID => {
                            tuple_extract::try_fast_read_heap_pub::<i64>(hdr, info)
                                .map(|v| v as f64)
                        }
                        _ => tuple_extract::try_fast_read_heap_pub::<f64>(hdr, info),
                    }
                };

                if let Some(v) = val {
                    col.count += 1;
                    col.has_value = true;
                    col.accumulate(v);
                }
            }

            // CHECK_FOR_INTERRUPTS every 8192 rows.
            if total % 8192 == 0 {
                pgrx::check_for_interrupts!();
            }
        }

        if total == 0 {
            self.result_returned = true;
            // SAFETY: result_slot is valid per caller contract.
            return unsafe { self.finalize_result(result_slot) };
        }

        self.rows_dispatched = total;
        self.batches_executed = 1;

        self.dispatch_time_us = start.elapsed().as_micros() as u64;
        self.result_returned = true;

        // SAFETY: result_slot is valid per caller contract.
        unsafe { self.finalize_result(result_slot) }
    }

    /// Vectorized scan+grouped agg: scan the base table via arena,
    /// extract group key + value columns in bulk, dispatch GPU hash agg.
    ///
    /// Follows the same pattern as `next_grouped`: first call consumes
    /// all input and runs GPU hash agg, subsequent calls emit groups.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `result_slot` must be
    /// a valid `TupleTableSlot`.
    pub unsafe fn next_grouped_vectorized(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.agg_grouped_vectorized").entered();

        // First call: scan all input and run hash aggregation.
        if !self.child_exhausted {
            self.child_exhausted = true;
            unsafe { self.execute_grouped_agg_vectorized() };
        }

        // Emit one result tuple per call (reuses existing emit logic).
        unsafe { self.emit_grouped_tuple(result_slot) }
    }

    /// Bulk-scan the base table via VectorizedScan and dispatch GPU hash
    /// aggregation. Stores results in `self.grouped_result`.
    #[allow(clippy::too_many_lines)]
    unsafe fn execute_grouped_agg_vectorized(&mut self) {
        let vscan = self.vscan.as_mut().expect("vscan must be set");

        // SAFETY: main backend thread, scan_desc is valid.
        let total = unsafe { vscan.scan_all() };
        if total == 0 {
            return;
        }

        let start = std::time::Instant::now();

        // SAFETY: scan_desc is valid after scan_all.
        let tupdesc = unsafe { vscan.tupdesc() };

        let group_key_info = self.group_key.as_ref().expect("grouped agg needs key");
        let key_size = group_key_info.key_size();
        let num_aggs = self.columns.len();

        // Resolve value type tags from tupdesc.
        let mut value_type_tags: Vec<i32> = vec![0; num_aggs];
        for (i, col) in self.columns.iter_mut().enumerate() {
            if col.attno > 0 {
                // SAFETY: tupdesc is valid.
                let info = unsafe { AttExtractInfo::new(tupdesc, col.attno) };
                col.type_oid = info.typid;
                value_type_tags[i] = oid_to_val_tag(col.type_oid);
            }
        }

        // Extract group key column. Use typed extraction to match kernel format.
        let gk_extract_info =
            unsafe { AttExtractInfo::new(tupdesc, group_key_info.attno) };
        let mut key_buf: Vec<u8> = Vec::with_capacity(total * key_size);
        let mut key_null_mask: Vec<u8> = Vec::with_capacity(total);

        match group_key_info.key_type {
            0 => {
                // i32 key
                let (vals, nulls) = unsafe { vscan.extract_i32(&gk_extract_info) };
                for (j, &v) in vals.iter().enumerate() {
                    key_buf.extend_from_slice(&v.to_ne_bytes());
                    key_null_mask.push(nulls[j]);
                }
            }
            1 => {
                // i64 key
                let (vals, nulls) = unsafe { vscan.extract_i64(&gk_extract_info) };
                for (j, &v) in vals.iter().enumerate() {
                    key_buf.extend_from_slice(&v.to_ne_bytes());
                    key_null_mask.push(nulls[j]);
                }
            }
            2 => {
                // f64 key
                let (vals, nulls) = unsafe { vscan.extract_f64(&gk_extract_info) };
                for (j, &v) in vals.iter().enumerate() {
                    key_buf.extend_from_slice(&v.to_ne_bytes());
                    key_null_mask.push(nulls[j]);
                }
            }
            _ => return,
        }

        // Extract value columns as typed byte buffers.
        let mut value_bufs: Vec<Vec<u8>> = vec![Vec::new(); num_aggs];
        let mut value_null_masks: Vec<Vec<u8>> = vec![Vec::new(); num_aggs];

        for (i, col) in self.columns.iter().enumerate() {
            if col.op == AggOp::Count && col.attno <= 0 {
                // COUNT(*): dummy f64 zero buffer.
                value_bufs[i] = vec![0u8; total * 8];
                value_null_masks[i] = vec![0u8; total];
                value_type_tags[i] = 5; // f64
                continue;
            }
            if col.attno <= 0 {
                value_bufs[i] = vec![0u8; total * 8];
                value_null_masks[i] = vec![1u8; total];
                value_type_tags[i] = 5;
                continue;
            }

            // SAFETY: tupdesc is valid.
            let info = unsafe { AttExtractInfo::new(tupdesc, col.attno) };

            // Extract as the native type matching the value tag.
            match value_type_tags[i] {
                2 => {
                    // Int32
                    let (vals, nulls) = unsafe { vscan.extract_i32(&info) };
                    let mut buf = Vec::with_capacity(total * 4);
                    for &v in &vals {
                        buf.extend_from_slice(&v.to_ne_bytes());
                    }
                    value_bufs[i] = buf;
                    value_null_masks[i] = nulls;
                }
                3 => {
                    // Int64
                    let (vals, nulls) = unsafe { vscan.extract_i64(&info) };
                    let mut buf = Vec::with_capacity(total * 8);
                    for &v in &vals {
                        buf.extend_from_slice(&v.to_ne_bytes());
                    }
                    value_bufs[i] = buf;
                    value_null_masks[i] = nulls;
                }
                4 => {
                    // Float32
                    let (vals, nulls) = unsafe { vscan.extract_f32(&info) };
                    let mut buf = Vec::with_capacity(total * 4);
                    for &v in &vals {
                        buf.extend_from_slice(&v.to_ne_bytes());
                    }
                    value_bufs[i] = buf;
                    value_null_masks[i] = nulls;
                }
                _ => {
                    // Float64 (default)
                    let (vals, nulls) = unsafe { vscan.extract_f64(&info) };
                    let mut buf = Vec::with_capacity(total * 8);
                    for &v in &vals {
                        buf.extend_from_slice(&v.to_ne_bytes());
                    }
                    value_bufs[i] = buf;
                    value_null_masks[i] = nulls;
                    value_type_tags[i] = 5;
                }
            }
        }

        self.rows_dispatched = total as u64;
        self.batches_executed = 1;

        // Build FFI descriptors (same as execute_grouped_agg).
        let ffi_agg_cols: Vec<PgaccelAggCol> = self
            .columns
            .iter()
            .enumerate()
            .map(|(i, col)| PgaccelAggCol {
                func: agg_op_to_ffi(col.op),
                col_idx: if col.op == AggOp::Count && col.attno <= 0 {
                    usize::MAX
                } else {
                    i
                },
            })
            .collect();

        let value_col_ptrs: Vec<*const std::ffi::c_void> = value_bufs
            .iter()
            .map(|buf| buf.as_ptr().cast::<std::ffi::c_void>())
            .collect();
        let value_null_ptrs: Vec<*const u8> =
            value_null_masks.iter().map(Vec::as_ptr).collect();

        let dispatch_start = std::time::Instant::now();

        let result = gpu::hash_agg_execute(
            key_buf.as_ptr().cast(),
            key_null_mask.as_ptr(),
            total,
            group_key_info.key_type,
            &value_col_ptrs,
            &value_null_ptrs,
            &value_type_tags,
            &ffi_agg_cols,
        );

        self.dispatch_time_us = start.elapsed().as_micros() as u64
            + dispatch_start.elapsed().as_micros() as u64;

        if let Some(hash_result) = result {
            let group_count = hash_result.group_count();
            self.gpu_dispatched = true;
            pgrx::debug1!(
                "pg_accel: hash_agg_vectorized: {} groups from {} rows",
                group_count,
                total
            );
            self.grouped_result = Some(GroupedAggResult {
                result: hash_result,
                next_group: 0,
                group_count,
                key_type: group_key_info.key_type,
            });
        } else {
            pgrx::debug1!(
                "pg_accel: hash_agg_vectorized: GPU dispatch failed for {} rows",
                total,
            );
        }
    }

    /// Build the final result tuple from accumulated column values.
    ///
    /// # Safety
    ///
    /// `result_slot` must be a valid `TupleTableSlot`.
    unsafe fn finalize_result(
        &self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
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

    /// Fused scan+agg: walk the heap directly, apply the filter predicate
    /// inline, and accumulate aggregate columns from passing `HeapTuple`s
    /// without copying to `MinimalTuple` or deforming through a slot.
    ///
    /// This eliminates three per-tuple overheads:
    /// 1. `ExecProcNode` virtual dispatch from agg to child scan
    /// 2. `ExecCopySlotMinimalTuple` (palloc + memcpy)
    /// 3. Slot deformation (`slot_getattr`)
    ///
    /// Instead, aggregate column values are extracted directly from the
    /// `HeapTuple` data area using precomputed offsets (same fast-path as
    /// `inline_filter_scan` in scan.rs).
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `self.fused_scan_desc`
    /// must be a valid `TableScanDesc`. `result_slot` must be a valid
    /// `TupleTableSlot`.
    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    pub unsafe fn next_fused(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.agg_fused").entered();
        if self.result_returned {
            return std::ptr::null_mut();
        }

        let start = std::time::Instant::now();
        let limits = cost::device_limits();
        let interrupt_interval = limits.fused_interrupt_interval;

        // Lazily build extraction info for aggregate columns.
        if self.fused_agg_infos.is_none() {
            // SAFETY: result_slot is valid per caller contract.
            let tupdesc = unsafe { (*result_slot).tts_tupleDescriptor };
            // The agg columns reference the *source table* TupleDesc, not
            // the result slot's TupleDesc. For a fused scan, the scan
            // descriptor's relation gives us the table TupleDesc.
            // SAFETY: fused_scan_desc is valid per set_fused_context.
            let rel = unsafe { (*self.fused_scan_desc).rs_rd };
            let table_tupdesc = if !rel.is_null() {
                // SAFETY: rs_rd is a valid Relation pointer.
                unsafe { (*rel).rd_att }
            } else {
                tupdesc
            };
            let mut infos = Vec::with_capacity(self.columns.len());
            for col in &mut self.columns {
                if col.attno > 0 {
                    // The agg's attno references the child scan's output
                    // position. Map it to the base table attno for direct
                    // heap extraction.
                    let table_attno = if !self.fused_attno_map.is_empty() {
                        let idx = (col.attno - 1) as usize;
                        if idx < self.fused_attno_map.len() {
                            self.fused_attno_map[idx]
                        } else {
                            col.attno
                        }
                    } else {
                        col.attno
                    };
                    // SAFETY: table_tupdesc is a valid TupleDesc.
                    let info = unsafe { AttExtractInfo::new(table_tupdesc, table_attno) };
                    // Resolve type OID from the table schema.
                    if col.type_oid == pg_sys::InvalidOid {
                        col.type_oid = info.typid;
                    }
                    infos.push(Some(info));
                } else {
                    infos.push(None);
                }
            }
            self.fused_agg_infos = Some(
                infos
                    .into_iter()
                    .map(|opt| {
                        opt.unwrap_or(
                            // COUNT(*) columns don't need extraction info.
                            // SAFETY: zero-initialized info with can_fast_extract=false.
                            AttExtractInfo::dummy(),
                        )
                    })
                    .collect(),
            );
        }

        // Lazily build extraction info for filter columns.
        if self.fused_filter_infos.is_none() {
            // SAFETY: fused_scan_desc is valid.
            let rel = unsafe { (*self.fused_scan_desc).rs_rd };
            let table_tupdesc = if !rel.is_null() {
                // SAFETY: rs_rd is a valid Relation pointer.
                unsafe { (*rel).rd_att }
            } else {
                // SAFETY: result_slot is valid.
                unsafe { (*result_slot).tts_tupleDescriptor }
            };
            let infos = match &self.fused_expr {
                Some(CompiledExpr::Template(TemplateKernel::CmpConst { col_idx, .. })) => {
                    // SAFETY: table_tupdesc is valid.
                    vec![unsafe { AttExtractInfo::new(table_tupdesc, (*col_idx + 1) as i32) }]
                }
                Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                    col1_idx,
                    col2_idx,
                    ..
                })) => vec![
                    // SAFETY: table_tupdesc is valid.
                    unsafe { AttExtractInfo::new(table_tupdesc, (*col1_idx + 1) as i32) },
                    unsafe { AttExtractInfo::new(table_tupdesc, (*col2_idx + 1) as i32) },
                ],
                _ => vec![],
            };
            self.fused_filter_infos = Some(infos);
        }

        let empty_filter = Vec::new();
        let filter_infos = self.fused_filter_infos.as_ref().unwrap_or(&empty_filter);

        // Pre-compute which columns want GPU buffering.
        let gpu_flags: Vec<bool> = self
            .columns
            .iter()
            .map(|c| c.wants_gpu_buffer(self.strategy))
            .collect();

        let mut row_count: u64 = 0;

        // Walk the heap, applying filter + accumulating in one pass.
        loop {
            // SAFETY: fused_scan_desc is valid; main backend thread.
            let htup = unsafe {
                pg_sys::heap_getnext(
                    self.fused_scan_desc,
                    pg_sys::ScanDirection::ForwardScanDirection,
                )
            };
            if htup.is_null() {
                break;
            }

            row_count += 1;

            // Periodic interrupt check (interval from DeviceLimits).
            if row_count % interrupt_interval as u64 == 0 {
                pgrx::check_for_interrupts!();
            }

            // SAFETY: htup is valid from heap_getnext.
            let t_data = unsafe { (*htup).t_data };

            // Evaluate filter predicate (if any).
            let passes = match &self.fused_expr {
                Some(CompiledExpr::Template(TemplateKernel::CmpConst {
                    cmp_opcode,
                    const_val,
                    ..
                })) => {
                    if !filter_infos.is_empty() {
                        Self::fused_eval_cmp(t_data, &filter_infos[0], *cmp_opcode, *const_val)
                    } else {
                        true
                    }
                }
                Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                    cmp1_opcode,
                    const1_val,
                    cmp2_opcode,
                    const2_val,
                    ..
                })) => {
                    if filter_infos.len() >= 2 {
                        Self::fused_eval_cmp(t_data, &filter_infos[0], *cmp1_opcode, *const1_val)
                            && Self::fused_eval_cmp(
                                t_data,
                                &filter_infos[1],
                                *cmp2_opcode,
                                *const2_val,
                            )
                    } else {
                        true
                    }
                }
                // No filter or unsupported template: all rows pass.
                None | Some(CompiledExpr::DeferToPg) | Some(CompiledExpr::Bytecode(_)) => true,
                // Other template patterns: conservatively pass.
                _ => true,
            };

            if !passes {
                continue;
            }

            // Extract aggregate column values directly from the HeapTuple.
            let empty_agg = Vec::new();
            let agg_infos = self.fused_agg_infos.as_ref().unwrap_or(&empty_agg);
            for (i, col) in self.columns.iter_mut().enumerate() {
                if col.attno <= 0 {
                    // COUNT(*): just increment.
                    col.count += 1;
                    col.has_value = true;
                    continue;
                }

                if i >= agg_infos.len() {
                    continue;
                }
                let info = &agg_infos[i];

                // Fast-extract the value from HeapTuple data.
                // SAFETY: t_data is valid from heap_getnext. info matches
                // the table schema from set_fused_context initialization.
                let val: Option<f64> = unsafe {
                    match info.typid {
                        t if t == pg_sys::FLOAT4OID => {
                            tuple_extract::try_fast_read_heap_pub::<f32>(t_data, info)
                                .map(f64::from)
                        }
                        t if t == pg_sys::INT2OID => {
                            tuple_extract::try_fast_read_heap_pub::<i16>(t_data, info)
                                .map(f64::from)
                        }
                        t if t == pg_sys::INT4OID => {
                            tuple_extract::try_fast_read_heap_pub::<i32>(t_data, info)
                                .map(f64::from)
                        }
                        t if t == pg_sys::INT8OID => {
                            tuple_extract::try_fast_read_heap_pub::<i64>(t_data, info)
                                .map(|v| v as f64)
                        }
                        _ => tuple_extract::try_fast_read_heap_pub::<f64>(t_data, info),
                    }
                };

                let Some(v) = val else {
                    // Null or not fast-extractable: skip this row for this column.
                    continue;
                };

                col.count += 1;
                col.has_value = true;

                if gpu_flags[i] {
                    col.gpu_values.push(v);
                } else {
                    col.accumulate(v);
                }
            }
        }

        self.rows_dispatched = row_count;
        self.batches_executed = 1;
        self.dispatch_time_us = start.elapsed().as_micros() as u64;

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

        pgrx::debug1!(
            "pg_accel: fused scan+agg complete: {} rows scanned, {}us",
            row_count,
            self.dispatch_time_us,
        );

        result_slot
    }

    /// Evaluate a single `col <cmp> const` predicate inline on a HeapTuple.
    ///
    /// Returns `true` if the predicate passes, `false` otherwise.
    /// Returns `true` (pass) if the value cannot be fast-extracted (conservative).
    #[inline(always)]
    fn fused_eval_cmp(
        t_data: pg_sys::HeapTupleHeader,
        info: &AttExtractInfo,
        cmp_opcode: u16,
        const_val: f64,
    ) -> bool {
        if !info.can_fast_extract() {
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

            // Group key at its correct target list position.
            let gk_pos = self.group_key_tlist_pos;
            let keys_ptr = gr.result.group_keys_ptr();
            if keys_ptr.is_null() {
                *isnull.add(gk_pos) = true;
            } else {
                *isnull.add(gk_pos) = false;
                let datum = match gr.key_type {
                    0 => {
                        // SAFETY: keys_ptr points to group_count i32 values.
                        let key = *(keys_ptr.cast::<i32>()).add(gidx);
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
                *values.add(gk_pos) = datum;
            }

            // Aggregate results at slots that are NOT the group key position.
            // agg_descs were collected skipping the group key Var, so column
            // i maps to the (i)-th non-group-key slot in the target list.
            let mut slot_idx = 0;
            for (i, col) in self.columns.iter().enumerate() {
                // Skip the group key position.
                if slot_idx == gk_pos {
                    slot_idx += 1;
                }
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
                slot_idx += 1;
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
        let _span = tracing::debug_span!("exec.agg_grouped").entered();
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

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
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
        let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Passthrough, 0)]);
        assert_eq!(state.strategy(), AccelStrategy::GpuReduce);
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

    // -- GPU reduce dispatch + small batch tests ------------------------------

    #[test]
    fn gpu_values_empty_initially() {
        let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Sum, 1)]);
        assert!(state.columns[0].gpu_values.is_empty());
        assert!(!state.gpu_dispatched);
    }

    #[test]
    fn drain_small_batch_sum() {
        let mut col = tcol(AggOp::Sum, 1);
        col.gpu_values = vec![1.0, 2.0, 3.0];
        col.has_value = true;
        col.drain_small_batch();
        assert!((col.sum - 6.0).abs() < f64::EPSILON);
        assert!(col.gpu_values.is_empty());
    }

    #[test]
    fn drain_small_batch_min() {
        let mut col = tcol(AggOp::Min, 1);
        col.gpu_values = vec![5.0, 2.0, 8.0];
        col.has_value = true;
        col.drain_small_batch();
        assert!((col.min_val - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn drain_small_batch_max() {
        let mut col = tcol(AggOp::Max, 1);
        col.gpu_values = vec![5.0, 2.0, 8.0];
        col.has_value = true;
        col.drain_small_batch();
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
        let n = (cost::device_limits().gpu_reduce_min_rows) + 100;
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
        let n = (cost::device_limits().gpu_reduce_min_rows) + 100;
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
        let n = (cost::device_limits().gpu_reduce_min_rows) + 100;
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
    fn drain_small_batch_empty_buffer_is_noop() {
        let mut col = tcol(AggOp::Sum, 1);
        col.drain_small_batch();
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

    // -- Edge case tests -------------------------------------------------------

    #[test]
    fn agg_op_from_i32_unknown_maps_to_passthrough() {
        assert_eq!(AggOp::from_i32(-1), AggOp::Passthrough);
        assert_eq!(AggOp::from_i32(6), AggOp::Passthrough);
        assert_eq!(AggOp::from_i32(i32::MAX), AggOp::Passthrough);
        assert_eq!(AggOp::from_i32(i32::MIN), AggOp::Passthrough);
    }

    #[test]
    fn agg_op_to_i32_values_are_distinct() {
        let ops = [
            AggOp::Sum,
            AggOp::Avg,
            AggOp::Min,
            AggOp::Max,
            AggOp::Count,
            AggOp::Passthrough,
        ];
        let vals: Vec<i32> = ops.iter().map(|o| o.to_i32()).collect();
        for i in 0..vals.len() {
            for j in (i + 1)..vals.len() {
                assert_ne!(
                    vals[i], vals[j],
                    "ops {:?} and {:?} collide",
                    ops[i], ops[j]
                );
            }
        }
    }

    #[test]
    fn agg_op_to_ffi_mapping() {
        assert!(matches!(agg_op_to_ffi(AggOp::Sum), PgaccelAggFunc::Sum));
        assert!(matches!(agg_op_to_ffi(AggOp::Avg), PgaccelAggFunc::Sum));
        assert!(matches!(agg_op_to_ffi(AggOp::Min), PgaccelAggFunc::Min));
        assert!(matches!(agg_op_to_ffi(AggOp::Max), PgaccelAggFunc::Max));
        assert!(matches!(agg_op_to_ffi(AggOp::Count), PgaccelAggFunc::Count));
        assert!(matches!(
            agg_op_to_ffi(AggOp::Passthrough),
            PgaccelAggFunc::Count
        ));
    }

    #[test]
    fn finalize_empty_input_all_agg_types() {
        // Empty input: SUM/AVG/MIN/MAX => NULL, COUNT => 0
        for op in [AggOp::Sum, AggOp::Avg, AggOp::Min, AggOp::Max] {
            let col = tcol(op, 1);
            let (_, is_null) = col.finalize();
            assert!(is_null, "{op:?} with no values should be NULL");
        }
        let count_col = tcol(AggOp::Count, 0);
        let (datum, is_null) = count_col.finalize();
        assert!(!is_null, "COUNT with no values should not be NULL");
        assert_eq!(datum.value(), 0, "COUNT with no values should be 0");
    }

    #[test]
    fn finalize_passthrough_with_values_still_null() {
        let mut col = tcol(AggOp::Passthrough, 1);
        col.has_value = true;
        col.count = 5;
        let (_, is_null) = col.finalize();
        assert!(is_null, "Passthrough should always return NULL");
    }

    #[test]
    fn avg_single_row_no_division_by_zero() {
        let mut col = tcol(AggOp::Avg, 1);
        col.accumulate(42.0);
        col.count = 1;
        col.has_value = true;
        let (datum, is_null) = col.finalize();
        assert!(!is_null);
        let val = f64::from_bits(datum.value() as u64);
        assert!((val - 42.0).abs() < f64::EPSILON);
    }

    #[test]
    fn avg_zero_count_with_has_value_returns_zero() {
        // Edge: has_value is true but count is 0 (shouldn't happen normally,
        // but finalize must not panic).
        let mut col = tcol(AggOp::Avg, 1);
        col.has_value = true;
        col.sum = 100.0;
        col.count = 0;
        let (datum, is_null) = col.finalize();
        assert!(!is_null);
        let val = f64::from_bits(datum.value() as u64);
        assert!((val - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn sum_alternating_positive_negative_cancellation() {
        let mut col = tcol(AggOp::Sum, 1);
        for i in 0..1000 {
            if i % 2 == 0 {
                col.accumulate(1.0);
            } else {
                col.accumulate(-1.0);
            }
        }
        assert!(
            col.sum.abs() < f64::EPSILON,
            "500 pairs of +1/-1 should cancel to 0"
        );
    }

    #[test]
    fn sum_large_alternating_values() {
        let mut col = tcol(AggOp::Sum, 1);
        for _ in 0..500 {
            col.accumulate(1e15);
            col.accumulate(-1e15);
        }
        // Kahan summation should keep this close to zero.
        assert!(
            col.sum.abs() < 1.0,
            "Kahan sum of large alternating values should be ~0, got {}",
            col.sum
        );
    }

    #[test]
    fn count_star_vs_count_col_semantics() {
        // COUNT(*) uses attno=0, COUNT(col) uses attno>0.
        // COUNT(*) column: accumulate increments count for every row (incl NULL).
        // COUNT(col) column: count only increments for non-null values.
        let star_col = AggColumn::new(AggOp::Count, 0);
        let col_col = AggColumn::new(AggOp::Count, 1);

        // attno <= 0 means COUNT(*) — no column extraction needed.
        assert!(star_col.attno <= 0);
        // attno > 0 means COUNT(col) — column extraction will skip NULLs.
        assert!(col_col.attno > 0);
    }

    #[test]
    fn min_with_negative_values() {
        let mut col = tcol(AggOp::Min, 1);
        col.accumulate(-100.0);
        col.accumulate(-200.0);
        col.accumulate(-50.0);
        assert!((col.min_val - (-200.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn max_with_negative_values() {
        let mut col = tcol(AggOp::Max, 1);
        col.accumulate(-100.0);
        col.accumulate(-200.0);
        col.accumulate(-50.0);
        assert!((col.max_val - (-50.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn min_max_single_value() {
        let mut min_col = tcol(AggOp::Min, 1);
        min_col.accumulate(7.5);
        assert!((min_col.min_val - 7.5).abs() < f64::EPSILON);

        let mut max_col = tcol(AggOp::Max, 1);
        max_col.accumulate(7.5);
        assert!((max_col.max_val - 7.5).abs() < f64::EPSILON);
    }

    #[test]
    fn accumulate_count_and_passthrough_are_noops() {
        let mut count_col = tcol(AggOp::Count, 0);
        count_col.accumulate(999.0);
        assert!((count_col.sum - 0.0).abs() < f64::EPSILON);
        assert_eq!(count_col.min_val, f64::INFINITY);
        assert_eq!(count_col.max_val, f64::NEG_INFINITY);

        let mut pt_col = tcol(AggOp::Passthrough, 0);
        pt_col.accumulate(999.0);
        assert!((pt_col.sum - 0.0).abs() < f64::EPSILON);
    }

    // -- GroupKeyInfo tests ----------------------------------------------------

    #[test]
    fn group_key_info_key_type_from_oid_int_types() {
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT2OID), Some(0));
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT4OID), Some(0));
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT8OID), Some(1));
    }

    #[test]
    fn group_key_info_key_type_from_oid_float_types() {
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::FLOAT4OID), Some(2));
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::FLOAT8OID), Some(2));
    }

    #[test]
    fn group_key_info_key_type_from_oid_unsupported_returns_none() {
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::TEXTOID), None);
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::BOOLOID), None);
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::InvalidOid), None);
    }

    #[test]
    fn group_key_info_key_sizes() {
        let i32_key = GroupKeyInfo {
            attno: 1,
            type_oid: pg_sys::INT4OID,
            key_type: 0,
        };
        assert_eq!(i32_key.key_size(), 4);

        let i64_key = GroupKeyInfo {
            attno: 1,
            type_oid: pg_sys::INT8OID,
            key_type: 1,
        };
        assert_eq!(i64_key.key_size(), 8);

        let f64_key = GroupKeyInfo {
            attno: 1,
            type_oid: pg_sys::FLOAT8OID,
            key_type: 2,
        };
        assert_eq!(f64_key.key_size(), 8);

        let unknown_key = GroupKeyInfo {
            attno: 1,
            type_oid: pg_sys::InvalidOid,
            key_type: 99,
        };
        assert_eq!(unknown_key.key_size(), 0);
    }

    #[test]
    fn group_key_info_clone() {
        let info = GroupKeyInfo {
            attno: 3,
            type_oid: pg_sys::INT4OID,
            key_type: 0,
        };
        let cloned = info.clone();
        assert_eq!(cloned.attno, 3);
        assert_eq!(cloned.type_oid, pg_sys::INT4OID);
        assert_eq!(cloned.key_type, 0);
    }

    // -- AggExecState grouped construction -------------------------------------

    #[test]
    fn new_grouped_sets_group_key() {
        let gk = GroupKeyInfo {
            attno: 1,
            type_oid: pg_sys::INT4OID,
            key_type: 0,
        };
        let state =
            AggExecState::new_grouped(AccelStrategy::GpuReduce, 256, &[(AggOp::Sum, 2, F8)], gk, 0);
        assert!(state.is_grouped());
        let info = state.group_key_info().unwrap();
        assert_eq!(info.attno, 1);
        assert_eq!(info.key_type, 0);
    }

    #[test]
    fn non_grouped_state_is_not_grouped() {
        let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Sum, 1)]);
        assert!(!state.is_grouped());
        assert!(state.group_key_info().is_none());
    }

    // -- wants_gpu_buffer tests ------------------------------------------------

    #[test]
    fn wants_gpu_buffer_only_for_gpu_reduce_numeric_ops() {
        let sum_col = AggColumn::new(AggOp::Sum, 1);
        assert!(sum_col.wants_gpu_buffer(AccelStrategy::GpuReduce));
        assert!(!sum_col.wants_gpu_buffer(AccelStrategy::GpuSpatial));
        assert!(!sum_col.wants_gpu_buffer(AccelStrategy::GpuSort));

        // COUNT and Passthrough never buffer.
        let count_col = AggColumn::new(AggOp::Count, 0);
        assert!(!count_col.wants_gpu_buffer(AccelStrategy::GpuReduce));

        let pt_col = AggColumn::new(AggOp::Passthrough, 1);
        assert!(!pt_col.wants_gpu_buffer(AccelStrategy::GpuReduce));

        // attno <= 0 never buffers.
        let zero_attno = AggColumn::new(AggOp::Sum, 0);
        assert!(!zero_attno.wants_gpu_buffer(AccelStrategy::GpuReduce));
    }

    // -- oid_to_val_tag tests --------------------------------------------------

    #[test]
    fn oid_to_val_tag_known_types() {
        assert_eq!(oid_to_val_tag(pg_sys::BOOLOID), 1);
        assert_eq!(oid_to_val_tag(pg_sys::INT2OID), 2);
        assert_eq!(oid_to_val_tag(pg_sys::INT4OID), 2);
        assert_eq!(oid_to_val_tag(pg_sys::INT8OID), 3);
        assert_eq!(oid_to_val_tag(pg_sys::FLOAT4OID), 4);
        assert_eq!(oid_to_val_tag(pg_sys::FLOAT8OID), 5);
    }

    #[test]
    fn oid_to_val_tag_unknown_returns_zero() {
        assert_eq!(oid_to_val_tag(pg_sys::TEXTOID), 0);
        assert_eq!(oid_to_val_tag(pg_sys::InvalidOid), 0);
    }

    // -- finalize result type encoding -----------------------------------------

    #[test]
    fn finalize_encodes_float4_result_type() {
        let mut col = AggColumn::with_result_type(AggOp::Sum, 1, pg_sys::Oid::from(700_u32)); // FLOAT4OID
        col.accumulate(3.14);
        col.has_value = true;
        let (datum, is_null) = col.finalize();
        assert!(!is_null);
        let bits = datum.value() as u32;
        let val = f32::from_bits(bits);
        assert!((val - 3.14_f32).abs() < 0.01);
    }

    #[test]
    fn finalize_encodes_int4_result_type() {
        let mut col = AggColumn::with_result_type(AggOp::Sum, 1, pg_sys::Oid::from(23_u32)); // INT4OID
        col.accumulate(42.0);
        col.has_value = true;
        let (datum, is_null) = col.finalize();
        assert!(!is_null);
        assert_eq!(datum.value() as i32, 42);
    }

    // -- apply_gpu_result tests ------------------------------------------------

    #[test]
    fn apply_gpu_result_sum() {
        let mut col = tcol(AggOp::Sum, 1);
        col.gpu_values = vec![1.0, 2.0]; // will be cleared
        col.apply_gpu_result(99.0);
        assert!(col.gpu_dispatched);
        assert!((col.sum - 99.0).abs() < f64::EPSILON);
        assert!(col.gpu_values.is_empty());
    }

    #[test]
    fn apply_gpu_result_min() {
        let mut col = tcol(AggOp::Min, 1);
        col.apply_gpu_result(-5.0);
        assert!(col.gpu_dispatched);
        assert!((col.min_val - (-5.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn apply_gpu_result_max() {
        let mut col = tcol(AggOp::Max, 1);
        col.apply_gpu_result(123.0);
        assert!(col.gpu_dispatched);
        assert!((col.max_val - 123.0).abs() < f64::EPSILON);
    }
}
