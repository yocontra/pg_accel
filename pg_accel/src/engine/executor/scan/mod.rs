//! Batch-dispatch scan executor for pg_accel Custom Scan nodes.
//!
//! [`ScanExecState`] holds the Rust-side state that persists across calls
//! to `exec_custom_scan`. Since PostgreSQL calls the exec callback once per
//! tuple, the executor accumulates child tuples into batches, dispatches
//! them, and returns results one at a time from a result buffer.
//!
//! # Lifecycle
//!
//! 1. **`begin_custom_scan`** — allocates `ScanExecState` via `Box::into_raw`
//!    and stores the pointer in `GpuAccelState.executor`.
//! 2. **`exec_custom_scan`** (repeated) — delegates to [`ScanExecState::next`].
//! 3. **`end_custom_scan`** — reclaims the `ScanExecState` via `Box::from_raw`
//!    and drops it.

#![allow(clippy::needless_range_loop)]

mod arena_scan;
mod exec;

use pgrx::pg_sys;

use crate::engine::columnar::ColumnarBatchOwner;
use crate::engine::dispatch::{self, DispatchResult};
use crate::engine::expr_compiler::{self, CompiledExpr, TemplateKernel};
use crate::engine::gucs;
use crate::engine::materialize::tuple_extract::{self, AttExtractInfo};
use crate::engine::registry::AccelStrategy;
use crate::engine::stats;
use crate::gpu::{self, PgaccelExprProgram};

/// Rust-side batch executor state, stored as a raw pointer in
/// `GpuAccelState.executor` (a `*mut ScanExecState`).
///
/// This struct is **not** `repr(C)` — it lives entirely on the Rust heap
/// and is opaque to PostgreSQL.
pub struct ScanExecState {
    /// Which acceleration strategy to use for this scan node.
    strategy: AccelStrategy,

    /// Batch size (from GUC at plan time).
    batch_size: usize,

    /// Buffered tuples from the child plan. Each entry is an owned
    /// `MinimalTuple` copied from the child slot. We must copy because
    /// the child plan reuses the same `TupleTableSlot` for every
    /// `ExecProcNode` call — storing slot pointers would give N copies
    /// of the last tuple.
    tuple_buffer: Vec<pg_sys::MinimalTuple>,

    /// Per-slot result: `true` means the row passed dispatch filtering
    /// and should be returned to the parent.
    result_mask: Vec<bool>,

    /// Current read position in `tuple_buffer` / `result_mask`. Points to
    /// the next tuple to consider returning. Tuples where `result_mask` is
    /// `false` are skipped.
    result_pos: usize,

    /// Set to `true` once the child plan returns a null (empty) slot,
    /// indicating no more tuples.
    child_exhausted: bool,

    /// Qual expression state stolen from the CustomScanState. We evaluate
    /// this ourselves per-batch instead of letting ExecScan do it per-tuple.
    /// NULL means no qual (all rows pass).
    qual: *mut pg_sys::ExprState,

    /// Expression context for qual evaluation. Borrowed from the plan
    /// state — NOT owned by us. We set `ecxt_scantuple` before each
    /// qual evaluation call.
    econtext: *mut pg_sys::ExprContext,

    // -- GPU dispatch context (set via `set_gpu_context`) --
    /// Attribute number of the column to extract for GPU dispatch (1-based).
    /// Zero means no GPU column extraction is configured.
    target_attno: i32,

    /// Function OID for initialising `fn_info_buf`. Zero means not set.
    fn_oid: pg_sys::Oid,

    /// Initialised `FmgrInfo` for the accelerated function. Only valid
    /// when `fn_oid != InvalidOid`.
    fn_info_buf: pg_sys::FmgrInfo,

    /// Every constant argument captured from the accelerated function's
    /// call site, in positional order. For 2-arg predicates like
    /// `WHERE ST_Intersects(geom_col, $1)`, this is a single-entry Vec.
    /// Multi-arg ops (`ST_DWithin(geom, geom, threshold)`,
    /// `ST_Hillshade(rast, cell_x, cell_y, sun_az, sun_alt)`) carry every
    /// Const in source-list order so dispatchers can index by position.
    /// Each tuple is `(datum, is_null, type_oid)`.
    qual_datums: Vec<(pg_sys::Datum, bool, pg_sys::Oid)>,

    /// When `true`, the child plan is a GiST index scan that has already
    /// performed bbox filtering. The GPU spatial pipeline will skip Layer 1
    /// (bbox overlap test) to avoid redundant work.
    gist_recheck: bool,

    /// Compiled GPU expression for GpuExpr strategy. Set by
    /// `begin_custom_scan` after expression compilation. `None` means
    /// no expression was compiled (fall back to scalar qual).
    compiled_expr: Option<expr_compiler::CompiledExpr>,

    /// Pre-extracted datums from the target column, captured from the
    /// child's slot during fill_batch (before the child reuses its slot).
    /// Used by dispatch_gpu_path instead of re-extracting from MinimalTuples,
    /// which fails because the Custom Scan's scan_slot TupleDesc may not
    /// match the child's MinimalTuple layout.
    datum_buffer: Vec<(pg_sys::Datum, bool)>,

    /// Table scan descriptor for direct heap scan (GpuExpr with scanrelid > 0).
    /// When non-null, fill_batch uses `table_scan_getnextslot` instead of
    /// `ExecProcNode` on a child plan. This avoids TupleDesc mismatch issues
    /// because the scan slot has the table's full TupleDesc.
    scan_desc: pg_sys::TableScanDesc,

    /// Dedicated memory context for batch MinimalTuple allocations.
    /// Reset at the start of each fill_batch cycle instead of individual
    /// pfree calls, reducing allocation overhead from O(n) to O(1).
    batch_mcxt: pg_sys::MemoryContext,

    /// Cached extraction info for inline filter columns. Initialized lazily
    /// on first call to `inline_filter_scan`, then reused across calls.
    inline_filter_infos: Option<Vec<AttExtractInfo>>,

    // -- Counters for EXPLAIN ANALYZE --
    /// Total rows pulled from child and dispatched.
    pub rows_dispatched: u64,

    /// Number of batches sent through dispatch.
    pub batches_executed: u64,

    /// Cumulative microseconds spent in dispatch.
    pub dispatch_time_us: u64,
}

impl ScanExecState {
    /// Create a new executor state for a Custom Scan node.
    ///
    /// `qual` and `econtext` are stolen from the `CustomScanState` at
    /// `begin_custom_scan` time. If `qual` is null, all rows pass.
    #[must_use]
    pub fn new(
        strategy: AccelStrategy,
        batch_size: usize,
        qual: *mut pg_sys::ExprState,
        econtext: *mut pg_sys::ExprContext,
    ) -> Self {
        Self {
            strategy,
            batch_size,
            tuple_buffer: Vec::with_capacity(batch_size),
            result_mask: Vec::with_capacity(batch_size),
            result_pos: 0,
            child_exhausted: false,
            qual,
            econtext,
            target_attno: 0,
            fn_oid: pg_sys::InvalidOid,
            // SAFETY: zero-initialised FmgrInfo is safe — all fields are
            // integers/pointers that accept zero.
            fn_info_buf: unsafe { std::mem::zeroed() },
            qual_datums: Vec::new(),
            gist_recheck: false,
            compiled_expr: None,
            datum_buffer: Vec::with_capacity(batch_size),
            scan_desc: std::ptr::null_mut(),
            batch_mcxt: std::ptr::null_mut(),
            inline_filter_infos: None,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
        }
    }

    /// Configure GPU dispatch context for spatial / H3 / raster strategies.
    ///
    /// # Safety
    ///
    /// `fn_oid` must be a valid regproc OID. Must be called on the main
    /// backend thread (calls `fmgr_info`).
    pub unsafe fn set_gpu_context(
        &mut self,
        fn_oid: pg_sys::Oid,
        target_attno: i32,
        qual_datums: Vec<(pg_sys::Datum, bool, pg_sys::Oid)>,
    ) {
        self.fn_oid = fn_oid;
        self.target_attno = target_attno;
        self.qual_datums = qual_datums;
        if fn_oid != pg_sys::InvalidOid {
            // SAFETY: Caller guarantees fn_oid is valid and we are on the
            // main backend thread.
            unsafe {
                pg_sys::fmgr_info(fn_oid, &raw mut self.fn_info_buf);
            }
        }
    }

    /// Set the compiled GPU expression for GpuExpr strategy.
    pub fn set_compiled_expr(&mut self, expr: expr_compiler::CompiledExpr) {
        self.compiled_expr = Some(expr);
    }

    /// Returns `true` if a template kernel is compiled and ready for
    /// inline evaluation during the heap walk.
    #[must_use]
    pub fn has_template_expr(&self) -> bool {
        matches!(&self.compiled_expr, Some(CompiledExpr::Template(_)))
    }

    /// Returns a clone of the compiled expression, if present.
    #[must_use]
    pub fn compiled_expr(&self) -> Option<CompiledExpr> {
        self.compiled_expr.clone()
    }

    /// Configure direct heap scan mode (GpuExpr with scanrelid > 0).
    /// When set, fill_batch uses `table_scan_getnextslot` instead of
    /// pulling from a child plan.
    pub fn set_scan_desc(&mut self, desc: pg_sys::TableScanDesc) {
        self.scan_desc = desc;

        // Create a dedicated memory context for batch MinimalTuple allocations.
        // Resetting this context is O(1) vs O(n) individual pfree calls.
        if self.batch_mcxt.is_null() {
            // SAFETY: CurrentMemoryContext is valid on the main backend thread.
            // AllocSetContextCreate is the standard PG API for creating memory contexts.
            unsafe {
                self.batch_mcxt = pg_sys::AllocSetContextCreateInternal(
                    pg_sys::CurrentMemoryContext,
                    c"pg_accel_batch".as_ptr(),
                    pg_sys::ALLOCSET_DEFAULT_MINSIZE as pg_sys::Size,
                    pg_sys::ALLOCSET_DEFAULT_INITSIZE as pg_sys::Size,
                    pg_sys::ALLOCSET_DEFAULT_MAXSIZE as pg_sys::Size,
                );
            }
        }
    }

    /// Returns the table scan descriptor (for cleanup in end_custom_scan).
    #[must_use]
    pub fn scan_desc(&self) -> pg_sys::TableScanDesc {
        self.scan_desc
    }

    /// Detect whether the child plan is a GiST index scan and enable
    /// batched recheck mode. When enabled, the GPU spatial pipeline
    /// skips bbox filtering (Layer 1) since GiST already performed it.
    ///
    /// # Safety
    ///
    /// `child_ps` must be a valid `PlanState` pointer. Must be called on
    /// the main backend thread.
    pub unsafe fn detect_gist_child(&mut self, child_ps: *mut pg_sys::PlanState) {
        const GIST_AM_OID: u32 = 783;

        if child_ps.is_null() {
            return;
        }

        // SAFETY: child_ps is valid. Check if the child is an IndexScan.
        let node_tag = unsafe { (*child_ps).type_ };
        if node_tag != pg_sys::NodeTag::T_IndexScanState {
            return;
        }

        // SAFETY: child_ps points to an IndexScanState. The iss_RelationDesc
        // field holds the index relation descriptor.
        let iss = child_ps.cast::<pg_sys::IndexScanState>();
        let index_rel = unsafe { (*iss).iss_RelationDesc };
        if index_rel.is_null() {
            return;
        }

        // SAFETY: index_rel is a valid RelationData. rd_rel points to the
        // pg_class tuple for this index. relam is the access method OID.
        let relam = unsafe { (*(*index_rel).rd_rel).relam };

        if u32::from(relam) == GIST_AM_OID {
            self.gist_recheck = true;
            tracing::debug!("pg_accel: GiST child detected, enabling batched recheck");
        }
    }

    /// Returns the acceleration strategy.
    #[must_use]
    pub fn strategy(&self) -> AccelStrategy {
        self.strategy
    }

    /// Returns the GPU-accelerated function OID (or `InvalidOid`).
    #[must_use]
    pub fn fn_oid(&self) -> pg_sys::Oid {
        self.fn_oid
    }

    /// Returns the target attribute number for GPU dispatch (1-based, 0 = none).
    #[must_use]
    pub fn target_attno(&self) -> i32 {
        self.target_attno
    }

    /// Returns the captured constant arguments for the accelerated function
    /// in positional source-list order. Empty when the call site has no
    /// `Const` args (every argument was a `Var`). Callers like the spatial
    /// dispatcher index by position (`[0]` = constant geometry,
    /// `[1]` = ST_DWithin threshold, etc.).
    #[must_use]
    pub fn qual_datums(&self) -> &[(pg_sys::Datum, bool, pg_sys::Oid)] {
        &self.qual_datums
    }

    /// Returns the qual pointer (for transfer during rescan).
    #[must_use]
    pub fn qual_ptr(&self) -> *mut pg_sys::ExprState {
        self.qual
    }

    /// Returns the econtext pointer (for transfer during rescan).
    #[must_use]
    pub fn econtext_ptr(&self) -> *mut pg_sys::ExprContext {
        self.econtext
    }
}

impl Drop for ScanExecState {
    fn drop(&mut self) {
        if self.batch_mcxt.is_null() {
            // Non-batch path: free individual MinimalTuples.
            for &mt in &self.tuple_buffer {
                if !mt.is_null() {
                    // SAFETY: mt was palloc'd by ExecCopySlotMinimalTuple.
                    unsafe { pg_sys::pfree(mt.cast()) };
                }
            }
            for &(datum, is_null) in &self.datum_buffer {
                if !is_null && datum.value() != 0 {
                    unsafe { pg_sys::pfree(datum.cast_mut_ptr()) };
                }
            }
        } else {
            // SAFETY: batch_mcxt is a valid PG memory context we created.
            // MemoryContextDelete frees the context and all its allocations.
            unsafe { pg_sys::MemoryContextDelete(self.batch_mcxt) };
            self.batch_mcxt = std::ptr::null_mut();
        }
    }
}

impl crate::engine::executor::state::ExecutorState for ScanExecState {
    unsafe fn exec(&mut self, css: *mut pg_sys::CustomScanState) -> *mut pg_sys::TupleTableSlot {
        let child_ps = unsafe { crate::engine::executor::state::child_plan_state(css, 0) };
        let scan_slot = unsafe { (*css).ss.ss_ScanTupleSlot };
        unsafe { self.next(child_ps, scan_slot) }
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
#[path = "tests.rs"]
mod tests;
