//! `AggExecState` — batch-dispatch aggregate executor state.
//!
//! Consumes all input tuples, runs reductions (GPU or CPU) on the accumulated
//! batches, and emits the aggregate result tuple(s).

use pgrx::pg_sys;

use crate::engine::cost;
use crate::engine::executor::vectorized_scan::VectorizedScan;
use crate::engine::expr_compiler::{self, CompiledExpr, TemplateKernel};
use crate::engine::materialize::tuple_extract::{self, AttExtractInfo};
use crate::engine::registry::AccelStrategy;
use crate::engine::stats;
use crate::gpu;
use crate::gpu::{PgaccelAggCol, PgaccelAggFunc};

use super::ffi_bridge::agg_op_to_ffi;
use super::keys::{GroupKeyInfo, append_key_bytes};
use super::ops::AggOp;
use super::partial::{ColumnAccumulator, PartialEmitter};
use super::values::{append_value_bytes, oid_to_val_tag};

/// PostgreSQL NUMERIC type OID (1700). `SUM(bigint)` and `SUM(int4)` return
/// this type, which is a varlena (pass-by-reference). We must allocate a
/// proper `Numeric` datum via `DirectFunctionCall1Coll` rather than storing
/// raw bits in the `Datum`, which PG would misinterpret as a pointer.
const NUMERICOID: pg_sys::Oid = pg_sys::Oid::from_u32(1700);

// ---------------------------------------------------------------------------
// Per-column accumulator
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Per-column accumulator
// ---------------------------------------------------------------------------

/// Accumulator state for a single aggregate column.
pub(super) struct AggColumn {
    pub(super) op: AggOp,
    /// 1-based attribute number (0 for COUNT(*)).
    pub(super) attno: i32,
    /// Resolved type OID of the input column (from child tuple descriptor).
    pub(super) type_oid: pg_sys::Oid,
    /// Result type OID (from Aggref.aggtype). Determines datum format in finalize.
    pub(super) result_type_oid: pg_sys::Oid,
    /// Running per-column accumulator (sum, count, min/max, `sum_sq`, …).
    pub(super) acc: ColumnAccumulator,
    /// Buffer for GPU reduce dispatch.
    pub(super) gpu_values: Vec<f64>,
    /// Whether GPU reduce was successfully used.
    pub(super) gpu_dispatched: bool,
    /// When true, Avg columns accumulate the full `[N, Sx, Sxx]` state
    /// (not just sum). Set by the partial-agg path because PG's Finalize
    /// shares one `pertrans` across aggregates that share `aggtransno`
    /// (AVG and STDDEV over the same column both use `float4_accum`/
    /// `float8_accum`), so every emitted column must carry a complete
    /// transition state — otherwise the Finalize's STDDEV reads AVG's
    /// column and sees Sxx=0.
    pub(super) needs_full_stats: bool,
}

impl AggColumn {
    pub(super) fn new(op: AggOp, attno: i32) -> Self {
        Self::with_result_type(op, attno, pg_sys::InvalidOid)
    }

    pub(super) fn with_result_type(op: AggOp, attno: i32, result_type_oid: pg_sys::Oid) -> Self {
        Self {
            op,
            attno,
            type_oid: pg_sys::InvalidOid,
            result_type_oid,
            acc: ColumnAccumulator {
                sum: 0.0,
                sum_comp: 0.0,
                sum_sq: 0.0,
                count: 0,
                min_val: f64::INFINITY,
                max_val: f64::NEG_INFINITY,
                has_value: false,
                bit_acc: 0,
                bool_acc: false,
            },
            gpu_values: Vec::new(),
            gpu_dispatched: false,
            needs_full_stats: false,
        }
    }

    /// Whether this column should buffer values for GPU reduce dispatch.
    pub(super) fn wants_gpu_buffer(&self, strategy: AccelStrategy) -> bool {
        strategy == AccelStrategy::GpuReduce
            && self.op != AggOp::Count
            && self.op != AggOp::Passthrough
            && self.attno > 0
    }

    /// Add a single value using Kahan summation for SUM/AVG.
    pub(super) fn accumulate(&mut self, val: f64) {
        match self.op {
            AggOp::Sum | AggOp::Avg => {
                let y = val - self.acc.sum_comp;
                let t = self.acc.sum + y;
                self.acc.sum_comp = (t - self.acc.sum) - y;
                self.acc.sum = t;
                if self.op == AggOp::Avg && self.needs_full_stats {
                    self.acc.sum_sq += val * val;
                }
            }
            AggOp::Min => {
                if val < self.acc.min_val {
                    self.acc.min_val = val;
                }
            }
            AggOp::Max => {
                if val > self.acc.max_val {
                    self.acc.max_val = val;
                }
            }
            AggOp::StddevSamp | AggOp::StddevPop | AggOp::VarSamp | AggOp::VarPop => {
                // Kahan-compensated sum, plus sum-of-squares for stats.
                let y = val - self.acc.sum_comp;
                let t = self.acc.sum + y;
                self.acc.sum_comp = (t - self.acc.sum) - y;
                self.acc.sum = t;
                self.acc.sum_sq += val * val;
            }
            AggOp::Count
            | AggOp::Passthrough
            | AggOp::BitAnd
            | AggOp::BitOr
            | AggOp::BoolAnd
            | AggOp::BoolOr => {}
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
    pub(super) fn dispatch_gpu_reduce(&mut self) {
        let n = self.gpu_values.len();
        let limits = cost::device_limits();
        if n < limits.gpu_reduce_min_rows {
            // Small batch: single-pass accumulation (no GPU overhead).
            self.drain_small_batch();
            return;
        }

        // Process in chunks to avoid GPU runtime limits on large ranges.
        if n > limits.gpu_reduce_max_chunk {
            self.dispatch_gpu_reduce_chunked();
            return;
        }

        // Try f64 GPU path first (CUDA/ROCm with fp64 support).
        let use_stats_for_avg = self.op == AggOp::Avg && self.needs_full_stats;
        let gpu_result = match self.op {
            AggOp::Sum => gpu::reduce_sum_f64(&self.gpu_values),
            AggOp::Avg if !use_stats_for_avg => gpu::reduce_sum_f64(&self.gpu_values),
            AggOp::Min => gpu::reduce_min_f64(&self.gpu_values),
            AggOp::Max => gpu::reduce_max_f64(&self.gpu_values),
            AggOp::Avg | AggOp::StddevSamp | AggOp::StddevPop | AggOp::VarSamp | AggOp::VarPop => {
                // Stats ops use reduce_stats kernel which returns
                // (count, sum, sum_sq). Dispatch separately and short-circuit.
                // AVG shares this path when running under a partial-agg plan
                // (see AggColumn.needs_full_stats) because PG's Finalize shares
                // one pertrans between AVG and STDDEV on the same column.
                if let Some((c, s, ss)) = gpu::reduce_stats_f64(&self.gpu_values) {
                    self.gpu_dispatched = true;
                    self.acc.sum = s;
                    self.acc.sum_sq = ss;
                    if self.acc.count == 0 {
                        self.acc.count = c;
                    }
                    self.acc.has_value = true;
                    self.gpu_values = Vec::new();
                } else {
                    pgrx::error!(
                        "pg_accel: GPU reduce_stats kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                        self.op,
                    );
                }
                return;
            }
            AggOp::Count
            | AggOp::Passthrough
            | AggOp::BitAnd
            | AggOp::BitOr
            | AggOp::BoolAnd
            | AggOp::BoolOr => {
                // Count/Passthrough/bitwise/bool never need GPU — drain directly.
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
            AggOp::StddevSamp
            | AggOp::StddevPop
            | AggOp::VarSamp
            | AggOp::VarPop
            | AggOp::Count
            | AggOp::Passthrough
            | AggOp::BitAnd
            | AggOp::BitOr
            | AggOp::BoolAnd
            | AggOp::BoolOr => None,
        };

        if let Some(result) = f32_result {
            self.apply_gpu_result(result);
        } else {
            // GPU reduce failed for a reducible op. Per CLAUDE.md rule 11,
            // no CPU fallback — raise a PG ERROR so the caller knows the
            // query cannot be answered accurately rather than silently
            // substituting a scalar accumulator (which has produced wrong
            // results, e.g. SUM=0).
            pgrx::error!(
                "pg_accel: GPU reduce kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                self.op,
            );
        }
    }

    /// Dispatch GPU reduce in chunks, combining partial results.
    ///
    /// Each chunk is reduced on GPU independently; partial results are
    /// combined via the accumulator. If GPU fails for any chunk, a warning
    /// is logged — the planner should not have injected this path.
    pub(super) fn dispatch_gpu_reduce_chunked(&mut self) {
        let max_chunk = cost::device_limits().gpu_reduce_max_chunk;
        let values = std::mem::take(&mut self.gpu_values);
        let use_stats_for_avg = self.op == AggOp::Avg && self.needs_full_stats;
        for chunk in values.chunks(max_chunk) {
            // The f64 wrappers in `gpu::` auto-cast to f32 on devices
            // without fp64 (Metal), so we always call the f64 entry point
            // here — no two-attempt retry needed. Precision tradeoff: on
            // fp32-only devices the per-chunk partial is folded into the
            // outer Kahan accumulator, bounding the drift.
            let gpu_result = match self.op {
                AggOp::Sum => gpu::reduce_sum_f64(chunk),
                AggOp::Avg if !use_stats_for_avg => gpu::reduce_sum_f64(chunk),
                AggOp::Min => gpu::reduce_min_f64(chunk),
                AggOp::Max => gpu::reduce_max_f64(chunk),
                AggOp::Avg
                | AggOp::StddevSamp
                | AggOp::StddevPop
                | AggOp::VarSamp
                | AggOp::VarPop => {
                    // Stats kernel returns (count, sum, sum_sq) per chunk;
                    // fold chunk partials into the accumulator directly.
                    if let Some((c, s, ss)) = gpu::reduce_stats_f64(chunk) {
                        self.gpu_dispatched = true;
                        // Kahan-fold the chunk's sum into the running sum.
                        let y = s - self.acc.sum_comp;
                        let t = self.acc.sum + y;
                        self.acc.sum_comp = (t - self.acc.sum) - y;
                        self.acc.sum = t;
                        self.acc.sum_sq += ss;
                        self.acc.count = self.acc.count.saturating_add(c);
                    } else {
                        pgrx::error!(
                            "pg_accel: GPU reduce_stats kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                            self.op,
                        );
                    }
                    None
                }
                AggOp::Count
                | AggOp::Passthrough
                | AggOp::BitAnd
                | AggOp::BitOr
                | AggOp::BoolAnd
                | AggOp::BoolOr => None,
            };

            if let Some(partial) = gpu_result {
                self.gpu_dispatched = true;
                self.accumulate(partial);
            } else if !use_stats_for_avg
                && matches!(self.op, AggOp::Sum | AggOp::Avg | AggOp::Min | AggOp::Max)
            {
                // Per CLAUDE.md rule 11: no CPU fallback on GPU kernel
                // failure. Raise a PG ERROR instead of silently folding the
                // chunk into the scalar accumulator. (Stats-variant Avg lives
                // in the stats branch above and raises its own error on
                // failure, so skip the duplicate.)
                pgrx::error!(
                    "pg_accel: GPU reduce kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                    self.op,
                );
            }
        }
        self.acc.has_value = true;
    }

    pub(super) fn apply_gpu_result(&mut self, result: f64) {
        self.gpu_dispatched = true;
        match self.op {
            AggOp::Sum | AggOp::Avg => self.acc.sum = result,
            AggOp::Min => self.acc.min_val = result,
            AggOp::Max => self.acc.max_val = result,
            AggOp::StddevSamp
            | AggOp::StddevPop
            | AggOp::VarSamp
            | AggOp::VarPop
            | AggOp::Count
            | AggOp::Passthrough
            | AggOp::BitAnd
            | AggOp::BitOr
            | AggOp::BoolAnd
            | AggOp::BoolOr => {}
        }
        self.gpu_values = Vec::new();
    }

    /// Drain the GPU value buffer through the scalar accumulator.
    ///
    /// Used for small batches below the GPU threshold and for
    /// Count/Passthrough ops that never need GPU dispatch.
    pub(super) fn drain_small_batch(&mut self) {
        for val in std::mem::take(&mut self.gpu_values) {
            self.accumulate(val);
        }
    }

    /// Convert a Datum to f64 using the resolved type OID.
    #[allow(clippy::cast_precision_loss, dead_code)]
    pub(super) fn datum_to_f64(&self, datum: pg_sys::Datum) -> f64 {
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
    pub(super) fn finalize(&self) -> (pg_sys::Datum, bool) {
        if !self.acc.has_value {
            return match self.op {
                AggOp::Count => (pg_sys::Datum::from(0_i64), false),
                _ => (pg_sys::Datum::from(0), true),
            };
        }

        let raw_f64 = match self.op {
            AggOp::Count => return (pg_sys::Datum::from(self.acc.count as i64), false),
            AggOp::Sum => self.acc.sum,
            AggOp::Avg => {
                if self.acc.count > 0 {
                    self.acc.sum / self.acc.count as f64
                } else {
                    0.0
                }
            }
            AggOp::Min => self.acc.min_val,
            AggOp::Max => self.acc.max_val,
            AggOp::VarPop => {
                if self.acc.count > 0 {
                    let n = self.acc.count as f64;
                    let mean = self.acc.sum / n;
                    mean.mul_add(-mean, self.acc.sum_sq / n)
                } else {
                    0.0
                }
            }
            AggOp::VarSamp => {
                if self.acc.count > 1 {
                    let n = self.acc.count as f64;
                    let mean = self.acc.sum / n;
                    let var_pop = mean.mul_add(-mean, self.acc.sum_sq / n);
                    var_pop * (n / (n - 1.0))
                } else {
                    return (pg_sys::Datum::from(0), true);
                }
            }
            AggOp::StddevPop => {
                if self.acc.count > 0 {
                    let n = self.acc.count as f64;
                    let mean = self.acc.sum / n;
                    mean.mul_add(-mean, self.acc.sum_sq / n).max(0.0).sqrt()
                } else {
                    0.0
                }
            }
            AggOp::StddevSamp => {
                if self.acc.count > 1 {
                    let n = self.acc.count as f64;
                    let mean = self.acc.sum / n;
                    let var_pop = mean.mul_add(-mean, self.acc.sum_sq / n).max(0.0);
                    (var_pop * (n / (n - 1.0))).sqrt()
                } else {
                    return (pg_sys::Datum::from(0), true);
                }
            }
            AggOp::BitAnd | AggOp::BitOr | AggOp::BoolAnd | AggOp::BoolOr | AggOp::Passthrough => {
                return (pg_sys::Datum::from(0), true);
            }
        };

        // Encode the f64 result into the correct datum format for the
        // declared result type. PG pass-by-value types store bits directly
        // in the Datum. Pass-by-reference types (NUMERIC) need allocation.
        let datum = match self.result_type_oid {
            pg_sys::FLOAT4OID => pg_sys::Datum::from((raw_f64 as f32).to_bits()),
            pg_sys::INT2OID => pg_sys::Datum::from(raw_f64 as i16),
            pg_sys::INT4OID => pg_sys::Datum::from(raw_f64 as i32),
            pg_sys::INT8OID => pg_sys::Datum::from(raw_f64 as i64),
            // NUMERICOID (1700): SUM(bigint), SUM(int4), etc. return numeric.
            // Numeric is pass-by-reference (varlena), so we must allocate a
            // proper Numeric datum. Convert via float8 -> numeric using PG's
            // own `float8_numeric` cast function.
            oid if oid == NUMERICOID => {
                // SAFETY: float8_numeric is a stable PG cast function.
                // The f64 bits are stored in the Datum as FLOAT8OID encoding.
                // DirectFunctionCall1Coll allocates in CurrentMemoryContext.
                let f8_datum = pg_sys::Datum::from(raw_f64.to_bits());
                // SAFETY: Calling PG's float8_numeric via DirectFunctionCall1Coll
                // on the main backend thread. The result is a palloc'd Numeric.
                // Cast needed: pgrx generates Rust-ABI fn items but
                // DirectFunctionCall1Coll expects extern "C-unwind".
                unsafe {
                    let fptr: unsafe extern "C-unwind" fn(
                        *mut pg_sys::FunctionCallInfoBaseData,
                    ) -> pg_sys::Datum = core::mem::transmute(pg_sys::float8_numeric as *const ());
                    pg_sys::DirectFunctionCall1Coll(Some(fptr), pg_sys::InvalidOid, f8_datum)
                }
            }
            // FLOAT8OID and anything else: store as f64 bits.
            _ => pg_sys::Datum::from(raw_f64.to_bits()),
        };
        (datum, false)
    }
}

// ---------------------------------------------------------------------------
// Grouped-agg result container
// ---------------------------------------------------------------------------

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
    pub(super) strategy: AccelStrategy,

    /// Batch size for accumulation.
    pub(super) batch_size: usize,

    /// Per-aggregate-column accumulators.
    pub(super) columns: Vec<AggColumn>,

    /// Whether all input has been consumed and the result returned.
    pub(super) result_returned: bool,

    /// Whether the child plan is exhausted.
    pub(super) child_exhausted: bool,

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

    /// When `Some`, this executor is the worker-side of a parallel plan
    /// (`Finalize Aggregate → Gather → pg_accel CustomScan`). Partial mode
    /// emits transition-state tuples (types match `aggtranstype`) instead
    /// of finalized aggregate values — the Finalize Aggregate combines
    /// them across workers and runs `aggfinalfn`.
    ///
    /// Populated from [`super::partial::PartialAggSpec`] at
    /// `begin_custom_scan`. `None` on non-parallel paths.
    pub partial_emitters: Option<Vec<Box<dyn PartialEmitter>>>,
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
            partial_emitters: None,
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

            let batch_count: u64 = tuples.len() as u64;

            // Handle columns that don't need extraction (COUNT(*), attno <= 0).
            for col in &mut self.columns {
                if col.attno <= 0 {
                    col.acc.count += batch_count;
                    if batch_count > 0 {
                        col.acc.has_value = true;
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

                        col.acc.count += 1;
                        col.acc.has_value = true;

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

        // Try fused multi-reduce first (single GPU pass for all columns),
        // then fall back to per-column dispatch for any columns not handled.
        self.try_fused_multi_reduce();

        // Per-column fallback for columns not handled by fused path.
        for col in &mut self.columns {
            if !col.gpu_values.is_empty() {
                col.dispatch_gpu_reduce();
                if col.gpu_dispatched {
                    self.gpu_dispatched = true;
                }
            }
        }

        self.result_returned = true;

        // Build a virtual tuple with all aggregate results. Dispatches
        // through `finalize_result` so partial-agg paths (worker-side of
        // parallel plans) emit transition-state datums via the
        // `PartialEmitter` trait.
        // SAFETY: main backend thread; result_slot valid per caller.
        unsafe {
            self.finalize_result(result_slot);
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
            partial_emitters: None,
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
            partial_emitters: None,
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

    /// Enable full `[N, Sx, Sxx]` state accumulation on every Avg column.
    ///
    /// Required for partial-agg plans: PG's Finalize Aggregate shares one
    /// `pertrans` slot between AVG and STDDEV/VAR on the same column (see
    /// `prepagg.c::find_compatible_trans` — they share `aggtransfn`,
    /// `aggcombinefn`, `aggtranstype`, and `agginitval`). The partial output
    /// for each shared-transno column must therefore carry the complete
    /// `float8_accum` state, even when the local aggregate is only AVG.
    pub fn enable_full_stats_for_avg(&mut self) {
        for col in &mut self.columns {
            if col.op == AggOp::Avg {
                col.needs_full_stats = true;
            }
        }
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
            .map_or(std::ptr::null_mut(), VectorizedScan::scan_desc)
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

        // Record a query acceleration attempt once per executor instance.
        stats::record_query_accelerated();

        let start = std::time::Instant::now();

        let vscan = self.vscan.as_mut().expect("vscan must be set");

        // SAFETY: scan_desc is valid, main backend thread.
        let scan_desc = vscan.scan_desc();
        let tupdesc =
            unsafe { (*scan_desc).rs_rd.as_ref() }.map_or(std::ptr::null_mut(), |rd| rd.rd_att);

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
            if let Some(info) = &info
                && col.type_oid == pg_sys::InvalidOid
            {
                col.type_oid = info.typid;
            }
        }

        // Pre-compute which columns want GPU buffering. Columns flagged
        // here push their values into `gpu_values` for a post-scan GPU
        // reduce dispatch. Non-flagged columns (Count, Passthrough, or
        // non-numeric types) use the scalar accumulator.
        let gpu_flags: Vec<bool> = self
            .columns
            .iter()
            .map(|c| c.wants_gpu_buffer(self.strategy))
            .collect();

        // Single-pass fused scan+extract+accumulate. No arena, no
        // intermediate buffers. Each tuple is read from the heap,
        // column values are extracted inline, and buffered (for GPU
        // reduce) or accumulated (scalar Kahan) depending on the
        // column's GPU eligibility.
        let mut total = 0u64;
        loop {
            // SAFETY: scan_desc is valid; main backend thread.
            let htup = unsafe {
                pg_sys::heap_getnext(scan_desc, pg_sys::ScanDirection::ForwardScanDirection)
            };
            if htup.is_null() {
                break;
            }

            total += 1;

            // SAFETY: htup is valid from heap_getnext.
            let hdr = unsafe { (*htup).t_data };

            for (i, (col, info)) in self.columns.iter_mut().zip(infos.iter()).enumerate() {
                if col.attno <= 0 {
                    // COUNT(*)
                    col.acc.count += 1;
                    col.acc.has_value = true;
                    continue;
                }

                let info = match info {
                    Some(inf) => inf,
                    None => continue,
                };

                // SAFETY: hdr is valid, info matches schema.
                // Type-dispatch: read as native type, convert to f64.
                let val: Option<f64> = unsafe {
                    match info.typid {
                        t if t == pg_sys::FLOAT4OID => {
                            tuple_extract::try_fast_read_heap_pub::<f32>(hdr, info).map(f64::from)
                        }
                        t if t == pg_sys::INT2OID => {
                            tuple_extract::try_fast_read_heap_pub::<i16>(hdr, info).map(f64::from)
                        }
                        t if t == pg_sys::INT4OID => {
                            tuple_extract::try_fast_read_heap_pub::<i32>(hdr, info).map(f64::from)
                        }
                        t if t == pg_sys::INT8OID => {
                            tuple_extract::try_fast_read_heap_pub::<i64>(hdr, info)
                                .map(|v| v as f64)
                        }
                        _ => tuple_extract::try_fast_read_heap_pub::<f64>(hdr, info),
                    }
                };

                if let Some(v) = val {
                    col.acc.count += 1;
                    col.acc.has_value = true;
                    if gpu_flags[i] {
                        col.gpu_values.push(v);
                    } else {
                        col.accumulate(v);
                    }
                }
            }

            // CHECK_FOR_INTERRUPTS every 8192 rows.
            if total.is_multiple_of(8192) {
                pgrx::check_for_interrupts!();
            }
        }

        if total == 0 {
            self.result_returned = true;
            self.dispatch_time_us = start.elapsed().as_micros() as u64;
            stats::record_batch(0, self.dispatch_time_us);
            // SAFETY: result_slot is valid per caller contract.
            return unsafe { self.finalize_result(result_slot) };
        }

        self.rows_dispatched = total;
        self.batches_executed = 1;

        // Try fused multi-reduce first (single GPU pass for all eligible
        // columns), then fall back to per-column dispatch for any columns
        // not handled. `dispatch_gpu_reduce` handles small batches via
        // `drain_small_batch`, so sub-threshold queries still produce
        // correct results without GPU overhead.
        self.try_fused_multi_reduce();

        for col in &mut self.columns {
            if !col.gpu_values.is_empty() {
                col.dispatch_gpu_reduce();
                if col.gpu_dispatched {
                    self.gpu_dispatched = true;
                }
            }
        }

        self.dispatch_time_us = start.elapsed().as_micros() as u64;
        self.result_returned = true;

        stats::record_batch(total, self.dispatch_time_us);
        if self.gpu_dispatched {
            stats::record_gpu_batch(total, 0);
        }

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
            stats::record_query_accelerated();
            self.child_exhausted = true;
            unsafe { self.execute_grouped_agg_vectorized() };
            stats::record_batch(self.rows_dispatched, self.dispatch_time_us);
            if self.gpu_dispatched {
                stats::record_gpu_batch(self.rows_dispatched, 0);
            }
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
        let gk_extract_info = unsafe { AttExtractInfo::new(tupdesc, group_key_info.attno) };
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
        let value_null_ptrs: Vec<*const u8> = value_null_masks.iter().map(Vec::as_ptr).collect();

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

        self.dispatch_time_us =
            start.elapsed().as_micros() as u64 + dispatch_start.elapsed().as_micros() as u64;

        if let Some(hash_result) = result {
            let group_count = hash_result.group_count();
            self.gpu_dispatched = true;
            tracing::debug!(
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
            // Per CLAUDE.md rule 11: GPU hash aggregate must succeed. Leaving
            // `grouped_result = None` silently produced zero rows for the
            // query. Raise a PG ERROR so the failure surfaces instead.
            pgrx::error!(
                "pg_accel: GPU hash-agg kernel failed; refusing to fall back to CPU (rule 11). rows={}",
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
        if self.partial_emitters.is_some() {
            // SAFETY: main backend thread; result_slot valid per caller.
            unsafe { self.finalize_partial(result_slot) };
        } else {
            // SAFETY: main backend thread; result_slot valid per caller.
            unsafe { self.finalize_simple(result_slot) };
        }
        result_slot
    }

    /// Emit finalized aggregate values (non-parallel path).
    ///
    /// # Safety
    /// Must be called on the main backend thread. `result_slot` must be a
    /// valid `TupleTableSlot` pointer.
    unsafe fn finalize_simple(&self, result_slot: *mut pg_sys::TupleTableSlot) {
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
    }

    /// Emit per-column partial-state datums for the Finalize Aggregate node
    /// to combine across workers.
    ///
    /// # Safety
    /// Must be called on the main backend thread. `result_slot` must be a
    /// valid `TupleTableSlot` pointer. Requires `partial_emitters` to be
    /// `Some` — the caller in [`finalize_result`] guards this.
    unsafe fn finalize_partial(&self, scan_slot: *mut pg_sys::TupleTableSlot) {
        // SAFETY: callers hold the main backend thread invariant.
        let emitters = self
            .partial_emitters
            .as_ref()
            .expect("finalize_partial called with None emitters");
        // SAFETY: scan_slot is a valid TupleTableSlot pointer.
        unsafe {
            pg_sys::ExecClearTuple(scan_slot);
            for (i, (col, emitter)) in self.columns.iter().zip(emitters.iter()).enumerate() {
                let (datum, isnull) = emitter.emit(&col.acc);
                (*scan_slot).tts_values.add(i).write(datum);
                (*scan_slot).tts_isnull.add(i).write(isnull);
            }
            pg_sys::ExecStoreVirtualTuple(scan_slot);
        }
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

        // Record a query acceleration attempt once per executor instance.
        stats::record_query_accelerated();

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
            let table_tupdesc = if rel.is_null() {
                tupdesc
            } else {
                // SAFETY: rs_rd is a valid Relation pointer.
                unsafe { (*rel).rd_att }
            };
            let mut infos = Vec::with_capacity(self.columns.len());
            for col in &mut self.columns {
                if col.attno > 0 {
                    // The agg's attno references the child scan's output
                    // position. Map it to the base table attno for direct
                    // heap extraction.
                    let table_attno = if self.fused_attno_map.is_empty() {
                        col.attno
                    } else {
                        let idx = (col.attno - 1) as usize;
                        if idx < self.fused_attno_map.len() {
                            self.fused_attno_map[idx]
                        } else {
                            col.attno
                        }
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
            let table_tupdesc = if rel.is_null() {
                // SAFETY: result_slot is valid.
                unsafe { (*result_slot).tts_tupleDescriptor }
            } else {
                // SAFETY: rs_rd is a valid Relation pointer.
                unsafe { (*rel).rd_att }
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
            if row_count.is_multiple_of(interrupt_interval as u64) {
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
                    if filter_infos.is_empty() {
                        true
                    } else {
                        Self::fused_eval_cmp(t_data, &filter_infos[0], *cmp_opcode, *const_val)
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
                None | Some(CompiledExpr::DeferToPg | CompiledExpr::Bytecode(_)) => true,
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
                    col.acc.count += 1;
                    col.acc.has_value = true;
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

                col.acc.count += 1;
                col.acc.has_value = true;

                if gpu_flags[i] {
                    col.gpu_values.push(v);
                } else {
                    col.accumulate(v);
                }
            }
        }

        self.rows_dispatched = row_count;
        self.batches_executed = 1;

        // Try fused multi-reduce first (single GPU pass for all columns),
        // then fall back to per-column dispatch for any columns not handled.
        self.try_fused_multi_reduce();

        // Per-column fallback for columns not handled by fused path.
        for col in &mut self.columns {
            if !col.gpu_values.is_empty() {
                col.dispatch_gpu_reduce();
                if col.gpu_dispatched {
                    self.gpu_dispatched = true;
                }
            }
        }

        self.dispatch_time_us = start.elapsed().as_micros() as u64;
        self.result_returned = true;

        stats::record_batch(row_count, self.dispatch_time_us);
        if self.gpu_dispatched {
            stats::record_gpu_batch(row_count, 0);
        }

        // Build a virtual tuple with all aggregate results. Dispatches
        // through `finalize_result` so partial-agg paths (worker-side of
        // parallel plans) emit transition-state datums via the
        // `PartialEmitter` trait.
        // SAFETY: main backend thread; result_slot valid per caller.
        unsafe {
            self.finalize_result(result_slot);
        }

        tracing::debug!(
            "pg_accel: fused scan+agg complete: {} rows scanned, {}us",
            row_count,
            self.dispatch_time_us,
        );

        result_slot
    }

    /// Attempt a single fused GPU pass that reduces all eligible columns
    /// simultaneously via the fused `reduce_multi_f32/f64` kernel.
    ///
    /// Fix Agent 4 (2026-04-11): this version detects groups of
    /// aggregate columns that all reference the **same** input column but
    /// compute different functions (SUM/MIN/MAX/COUNT) and collapses them
    /// into a single GPU kernel launch. The old implementation routed
    /// through `gpu::fused_filter_multi_reduce_f32`, which launched
    /// separate kernels per aggregate. The new path uses the
    /// `reduce_multi_*` kernel, which runs a single-pass tree reduction
    /// producing (sum, min, max, count) in one Metal kernel launch.
    ///
    /// Benefit: for a query like `SELECT SUM(x), MIN(x), MAX(x), COUNT(*)
    /// FROM t`, the previous code paid 4x the dispatch cost
    /// (one per aggregate) — this collapses to 1.
    ///
    /// Only eligible for non-grouped reduce strategies. Columns
    /// successfully reduced are drained; columns not part of any fusable
    /// group are left for per-column dispatch.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn try_fused_multi_reduce(&mut self) {
        // Only attempt fused path for GpuReduce strategy, non-grouped.
        if self.strategy != AccelStrategy::GpuReduce || self.group_key.is_some() {
            return;
        }

        let reduce_f32_break_even_rows = cost::device_limits().reduce_f32_break_even_rows;

        // Group eligible columns by their source attno so we only fuse
        // aggregates that read the same input. Count/Passthrough are not
        // eligible (they don't buffer values).
        let mut groups: std::collections::HashMap<i32, Vec<usize>> =
            std::collections::HashMap::new();
        for (i, c) in self.columns.iter().enumerate() {
            // reduce_multi_f64 returns sum/min/max/count only — no sum_sq.
            // Avg columns running under a partial plan need the full stats
            // state, so they stay on the per-column reduce_stats path.
            let avg_needs_stats = c.op == AggOp::Avg && c.needs_full_stats;
            if !c.gpu_values.is_empty()
                && !avg_needs_stats
                && matches!(c.op, AggOp::Sum | AggOp::Avg | AggOp::Min | AggOp::Max)
            {
                groups.entry(c.attno).or_default().push(i);
            }
        }

        for (attno, col_indices) in groups {
            // Skip groups with only one aggregate — they go through the
            // existing per-column path (which is already optimal for a
            // single SUM or MIN call).
            if col_indices.len() < 2 {
                continue;
            }

            // All columns in the group share an attno so their gpu_values
            // are populated from the same tuple stream — lengths must match.
            let n = self.columns[col_indices[0]].gpu_values.len();
            if n < reduce_f32_break_even_rows {
                continue;
            }
            let same_len = col_indices
                .iter()
                .all(|&i| self.columns[i].gpu_values.len() == n);
            if !same_len {
                continue;
            }

            let _span =
                tracing::debug_span!("gpu.reduce_multi", attno, n, num_aggs = col_indices.len(),)
                    .entered();

            // Dispatch the fused f64 multi-reduce. On Metal the wrapper
            // auto-casts to f32 inside gpu/mod.rs.
            let slice = self.columns[col_indices[0]].gpu_values.as_slice();
            let fused = gpu::reduce_multi_f64(slice);

            let Some(result) = fused else {
                tracing::warn!(
                    "pg_accel: fused multi-reduce failed for attno={attno}, n={n}; \
                     falling back to per-column dispatch"
                );
                continue;
            };

            // Apply the shared result to every aggregate in this group.
            for &col_idx in &col_indices {
                let col = &mut self.columns[col_idx];
                match col.op {
                    AggOp::Sum | AggOp::Avg => col.acc.sum = result.sum,
                    AggOp::Min => col.acc.min_val = result.min,
                    AggOp::Max => col.acc.max_val = result.max,
                    _ => {}
                }
                // Ensure count is correct for AVG finalize.
                if matches!(col.op, AggOp::Avg) {
                    #[allow(clippy::cast_sign_loss)]
                    let c = result.count.max(0) as u64;
                    if col.acc.count == 0 {
                        col.acc.count = c;
                    }
                }
                col.gpu_dispatched = true;
                col.acc.has_value = true;
                col.gpu_values.clear();
            }
            self.gpu_dispatched = true;
            tracing::debug!(
                "pg_accel: fused multi-reduce dispatched attno={attno}, \
                 {} aggregates, {} rows",
                col_indices.len(),
                n,
            );
        }
        // Columns not covered by any fusable group retain their buffers
        // and fall through to per-column dispatch.
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
            expr_compiler::opcode::ALWAYS_TRUE => true,
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
                            // NUMERICOID: allocate a proper Numeric varlena via
                            // PG's float8_numeric cast. See `finalize()` for details.
                            oid if oid == NUMERICOID => {
                                let f8_datum = pg_sys::Datum::from(raw_f64.to_bits());
                                // SAFETY: float8_numeric on main backend thread,
                                // result is palloc'd in CurrentMemoryContext.
                                // Cast needed: see finalize() comment.
                                let fptr: unsafe extern "C-unwind" fn(
                                    *mut pg_sys::FunctionCallInfoBaseData,
                                )
                                    -> pg_sys::Datum =
                                    core::mem::transmute(pg_sys::float8_numeric as *const ());
                                pg_sys::DirectFunctionCall1Coll(
                                    Some(fptr),
                                    pg_sys::InvalidOid,
                                    f8_datum,
                                )
                            }
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
            tracing::debug!(
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
            // Per CLAUDE.md rule 11: GPU hash aggregate must succeed. Leaving
            // `grouped_result = None` silently produced zero rows for the
            // query. Raise a PG ERROR so the failure surfaces instead.
            pgrx::error!(
                "pg_accel: GPU hash-agg kernel failed; refusing to fall back to CPU (rule 11). rows={}",
                row_count,
            );
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
