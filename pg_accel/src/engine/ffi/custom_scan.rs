//! Custom Scan Provider for pg_accel.
//!
//! Implements the three Custom Scan vtables (`CustomPathMethods`,
//! `CustomScanMethods`, `CustomExecMethods`) that let pg_accel inject
//! GPU-accelerated executor nodes into PostgreSQL query plans.
//!
//! Strategy metadata is serialized into `custom_private` as a PG `List` of
//! `Integer` nodes so it survives plan copying and EXPLAIN output.
//!
//! `ExplainCustomScan` reports strategy info for plain EXPLAIN and adds
//! execution counters (rows dispatched, batches, dispatch time) for
//! EXPLAIN ANALYZE.

use std::ffi::c_int;

use pgrx::pg_sys;

use super::super::gucs;
use crate::engine::executor::scan::ScanExecState;
use crate::engine::registry::AccelStrategy;

// ---------------------------------------------------------------------------
// Strategy constants (used in custom_private serialization + EXPLAIN)
// ---------------------------------------------------------------------------

/// Strategy enum values stored in `custom_private`.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuStrategy {
    Scan = 0,
    Join = 1,
    Agg = 2,
    Sort = 3,
}

impl GpuStrategy {
    /// Convert from raw integer, defaulting to `Scan` for unknown values.
    #[must_use]
    pub const fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::Join,
            2 => Self::Agg,
            3 => Self::Sort,
            _ => Self::Scan,
        }
    }

    /// Human-readable name for EXPLAIN output.
    #[must_use]
    pub const fn label(self) -> &'static std::ffi::CStr {
        match self {
            Self::Scan => c"GpuScan",
            Self::Join => c"GpuJoin",
            Self::Agg => c"GpuAgg",
            Self::Sort => c"GpuSort",
        }
    }
}

// ---------------------------------------------------------------------------
// Extended scan state (embeds CustomScanState + our private data)
// ---------------------------------------------------------------------------

/// Extended executor state for pg_accel Custom Scan nodes.
///
/// PostgreSQL's `ExecInitCustomScan` calls `CreateCustomScanState` which
/// returns a pointer to the `css` field. Because `css` is the first field
/// and `#[repr(C)]` guarantees layout, PG treats this as a normal
/// `CustomScanState*` while we can upcast to access `accel`.
#[repr(C)]
struct GpuAccelScanState {
    css: pg_sys::CustomScanState,
    accel: GpuAccelState,
}

/// Per-node execution counters, config, and batch executor pointer.
#[repr(C)]
struct GpuAccelState {
    strategy: i32,
    batch_size: i32,
    expected_threads: i32,
    rows_dispatched: u64,
    batches_executed: u64,
    dispatch_time_us: u64,
    /// Pointer to heap-allocated Rust `ScanExecState`.
    /// Set in `begin_custom_scan`, freed in `end_custom_scan`.
    executor: *mut ScanExecState,
}

// ---------------------------------------------------------------------------
// Sync wrappers for static vtables (pg_sys structs contain *const c_char)
// ---------------------------------------------------------------------------

// SAFETY: All wrapped vtable structs contain only static string pointers and
// function pointers. They are const-initialised and never mutated. PG only
// accesses them from the main backend thread.
#[repr(transparent)]
struct SyncPathMethods(pg_sys::CustomPathMethods);
unsafe impl Sync for SyncPathMethods {}

#[repr(transparent)]
struct SyncScanMethods(pg_sys::CustomScanMethods);
unsafe impl Sync for SyncScanMethods {}

#[repr(transparent)]
struct SyncExecMethods(pg_sys::CustomExecMethods);
unsafe impl Sync for SyncExecMethods {}

// ---------------------------------------------------------------------------
// Static vtables
// ---------------------------------------------------------------------------

static SCAN_PATH_METHODS: SyncPathMethods = SyncPathMethods(pg_sys::CustomPathMethods {
    CustomName: c"GpuAccelScan".as_ptr(),
    PlanCustomPath: Some(plan_custom_path_scan),
    ReparameterizeCustomPathByChild: None,
});

static JOIN_PATH_METHODS: SyncPathMethods = SyncPathMethods(pg_sys::CustomPathMethods {
    CustomName: c"GpuAccelJoin".as_ptr(),
    PlanCustomPath: Some(plan_custom_path_join),
    ReparameterizeCustomPathByChild: None,
});

static SCAN_SCAN_METHODS: SyncScanMethods = SyncScanMethods(pg_sys::CustomScanMethods {
    CustomName: c"GpuAccelScan".as_ptr(),
    CreateCustomScanState: Some(create_custom_scan_state),
});

static JOIN_SCAN_METHODS: SyncScanMethods = SyncScanMethods(pg_sys::CustomScanMethods {
    CustomName: c"GpuAccelJoin".as_ptr(),
    CreateCustomScanState: Some(create_custom_scan_state),
});

static EXEC_METHODS: SyncExecMethods = SyncExecMethods(pg_sys::CustomExecMethods {
    CustomName: c"GpuAccelScan".as_ptr(),
    BeginCustomScan: Some(begin_custom_scan),
    ExecCustomScan: Some(exec_custom_scan),
    EndCustomScan: Some(end_custom_scan),
    ReScanCustomScan: Some(rescan_custom_scan),
    MarkPosCustomScan: None,
    RestrPosCustomScan: None,
    EstimateDSMCustomScan: None,
    InitializeDSMCustomScan: None,
    ReInitializeDSMCustomScan: None,
    InitializeWorkerCustomScan: None,
    ShutdownCustomScan: None,
    ExplainCustomScan: Some(explain_custom_scan),
});

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Pointer to scan `CustomPathMethods` vtable.
#[inline]
#[must_use]
pub fn scan_path_methods() -> *const pg_sys::CustomPathMethods {
    &raw const SCAN_PATH_METHODS.0
}

/// Pointer to join `CustomPathMethods` vtable.
#[inline]
#[must_use]
pub fn join_path_methods() -> *const pg_sys::CustomPathMethods {
    &raw const JOIN_PATH_METHODS.0
}

/// Register Custom Scan methods with PostgreSQL. Must be called from `_PG_init`.
pub fn register() {
    // SAFETY: RegisterCustomScanMethods stores pointers to our static vtables
    // which live for the entire process lifetime. Called on main thread during
    // extension loading.
    unsafe {
        pg_sys::RegisterCustomScanMethods(&raw const SCAN_SCAN_METHODS.0);
        pg_sys::RegisterCustomScanMethods(&raw const JOIN_SCAN_METHODS.0);
    }
}

// ---------------------------------------------------------------------------
// PlanCustomPath callbacks
// ---------------------------------------------------------------------------

/// Convert a scan `CustomPath` into a `CustomScan` plan node.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
unsafe extern "C-unwind" fn plan_custom_path_scan(
    root: *mut pg_sys::PlannerInfo,
    rel: *mut pg_sys::RelOptInfo,
    best_path: *mut pg_sys::CustomPath,
    tlist: *mut pg_sys::List,
    clauses: *mut pg_sys::List,
    custom_plans: *mut pg_sys::List,
) -> *mut pg_sys::Plan {
    // SAFETY: all pointers originate from the planner and are valid.
    unsafe { make_custom_scan_plan(root, rel, best_path, tlist, clauses, custom_plans, true) }
}

/// Convert a join `CustomPath` into a `CustomScan` plan node.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
unsafe extern "C-unwind" fn plan_custom_path_join(
    root: *mut pg_sys::PlannerInfo,
    rel: *mut pg_sys::RelOptInfo,
    best_path: *mut pg_sys::CustomPath,
    tlist: *mut pg_sys::List,
    clauses: *mut pg_sys::List,
    custom_plans: *mut pg_sys::List,
) -> *mut pg_sys::Plan {
    // SAFETY: all pointers originate from the planner and are valid.
    unsafe { make_custom_scan_plan(root, rel, best_path, tlist, clauses, custom_plans, false) }
}

/// Shared helper: build a `CustomScan` plan from a `CustomPath`.
///
/// Serializes strategy metadata into `custom_private` as a `List` of
/// `Integer` nodes: [strategy, batch_size, expected_threads].
///
/// # Safety
///
/// All pointer arguments must originate from the PostgreSQL planner.
unsafe fn make_custom_scan_plan(
    _root: *mut pg_sys::PlannerInfo,
    _rel: *mut pg_sys::RelOptInfo,
    best_path: *mut pg_sys::CustomPath,
    tlist: *mut pg_sys::List,
    clauses: *mut pg_sys::List,
    custom_plans: *mut pg_sys::List,
    is_scan: bool,
) -> *mut pg_sys::Plan {
    // SAFETY: palloc0 returns zeroed memory in CurrentMemoryContext.
    let cscan = unsafe {
        pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomScan>()).cast::<pg_sys::CustomScan>()
    };

    // SAFETY: cscan is freshly palloc'd and zeroed.
    unsafe {
        (*cscan).scan.plan.type_ = pg_sys::NodeTag::T_CustomScan;
        (*cscan).scan.plan.targetlist = tlist;
        // SAFETY: extract_actual_clauses strips RestrictInfo wrappers,
        // returning the actual qual expressions for ExecInitCustomScan
        // to compile into ExprState for per-tuple evaluation.
        (*cscan).scan.plan.qual = pg_sys::extract_actual_clauses(clauses, false);
        (*cscan).scan.plan.startup_cost = (*best_path).path.startup_cost;
        (*cscan).scan.plan.total_cost = (*best_path).path.total_cost;
        (*cscan).scan.plan.plan_rows = (*best_path).path.rows;
        (*cscan).custom_plans = custom_plans;
        (*cscan).flags = (*best_path).flags;

        if is_scan {
            (*cscan).scan.scanrelid = (*best_path).path.parent.read().relid;
            (*cscan).methods = &raw const SCAN_SCAN_METHODS.0;
        } else {
            (*cscan).scan.scanrelid = 0; // joins don't have a single base relid
            (*cscan).methods = &raw const JOIN_SCAN_METHODS.0;
        }

        // Serialize strategy info into custom_private.
        let strategy = if is_scan {
            GpuStrategy::Scan as c_int
        } else {
            GpuStrategy::Join as c_int
        };
        let batch_size = gucs::min_batch_size();
        let expected_threads = resolve_thread_count();

        // SAFETY: makeInteger + lappend allocate in CurrentMemoryContext.
        let mut list: *mut pg_sys::List = std::ptr::null_mut();
        list = pg_sys::lappend(list, pg_sys::makeInteger(strategy).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(batch_size).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(expected_threads).cast());
        (*cscan).custom_private = list;
    }

    cscan.cast()
}

// ---------------------------------------------------------------------------
// CreateCustomScanState callback
// ---------------------------------------------------------------------------

/// Allocate an extended `GpuAccelScanState` so we have room for our private
/// counters alongside PG's `CustomScanState`.
///
/// # Safety
///
/// Called by the executor on the main backend thread.
unsafe extern "C-unwind" fn create_custom_scan_state(
    cscan: *mut pg_sys::CustomScan,
) -> *mut pg_sys::Node {
    // SAFETY: palloc0 returns zeroed, maximally-aligned memory. We allocate
    // our extended struct which has CustomScanState as first field.
    #[allow(clippy::cast_ptr_alignment)]
    let state = unsafe {
        pg_sys::palloc0(std::mem::size_of::<GpuAccelScanState>()).cast::<GpuAccelScanState>()
    };

    // SAFETY: state is freshly allocated and zeroed.
    unsafe {
        (*state).css.ss.ps.type_ = pg_sys::NodeTag::T_CustomScanState;
        (*state).css.flags = (*cscan).flags;
        (*state).css.methods = &raw const EXEC_METHODS.0;
        (*state).accel.executor = std::ptr::null_mut();
    }

    state.cast()
}

// ---------------------------------------------------------------------------
// Executor callbacks
// ---------------------------------------------------------------------------

/// `BeginCustomScan`: one-time init before the first tuple fetch.
///
/// PG's `ExecInitCustomScan` already initialized child plan states in
/// `css.custom_ps` before calling this. We read `custom_private` from the
/// plan node to populate our accel state, then allocate a `ScanExecState`.
///
/// # Safety
///
/// Called by the executor on the main backend thread.
unsafe extern "C-unwind" fn begin_custom_scan(
    node: *mut pg_sys::CustomScanState,
    _estate: *mut pg_sys::EState,
    _eflags: c_int,
) {
    // SAFETY: node points to our GpuAccelScanState (extended struct). The
    // plan node's custom_private was set by make_custom_scan_plan.
    let state = node.cast::<GpuAccelScanState>();
    let cscan = unsafe { (*node).ss.ps.plan.cast::<pg_sys::CustomScan>() };

    // Read strategy metadata from custom_private.
    unsafe {
        let priv_list = (*cscan).custom_private;
        if !priv_list.is_null() && pg_sys::list_length(priv_list) >= 3 {
            (*state).accel.strategy = pg_sys::list_nth_int(priv_list, 0);
            (*state).accel.batch_size = pg_sys::list_nth_int(priv_list, 1);
            (*state).accel.expected_threads = pg_sys::list_nth_int(priv_list, 2);
        } else {
            (*state).accel.strategy = 0;
            (*state).accel.batch_size = 256;
            (*state).accel.expected_threads = 1;
        }
    }

    // Allocate the Rust-side batch executor state.
    //
    // Steal the qual from the plan state so we evaluate it ourselves in
    // batches, rather than letting ExecScan do it one tuple at a time.
    // SAFETY: node points to our extended GpuAccelScanState whose first
    // field is CustomScanState. ss.ps.qual and ss.ps.ps_ExprContext are
    // initialised by ExecInitCustomScan before calling BeginCustomScan.
    let (batch_size, qual, econtext) = unsafe {
        let bs = (*state).accel.batch_size;
        let batch_size = if bs > 0 { bs as usize } else { 256 };

        // Steal the qual: take it from the plan state and NULL it out so
        // PG's ExecScan won't double-evaluate it.
        let qual = (*node).ss.ps.qual;
        (*node).ss.ps.qual = std::ptr::null_mut();

        let econtext = (*node).ss.ps.ps_ExprContext;

        (batch_size, qual, econtext)
    };

    let exec_state = Box::new(ScanExecState::new(
        AccelStrategy::BatchedEval,
        batch_size,
        qual,
        econtext,
    ));
    unsafe {
        (*state).accel.executor = Box::into_raw(exec_state);
        (*state).accel.rows_dispatched = 0;
        (*state).accel.batches_executed = 0;
        (*state).accel.dispatch_time_us = 0;
    }
}

/// `ExecCustomScan`: fetch the next tuple via batch dispatch.
///
/// Delegates to `ScanExecState::next` which handles batch accumulation,
/// dispatch, and one-at-a-time result draining.
///
/// # Safety
///
/// Called by the executor on the main backend thread.
unsafe extern "C-unwind" fn exec_custom_scan(
    node: *mut pg_sys::CustomScanState,
) -> *mut pg_sys::TupleTableSlot {
    let state = node.cast::<GpuAccelScanState>();

    // If the extension is disabled, fall through to passthrough.
    if !gucs::enabled() {
        return unsafe { passthrough_exec(node) };
    }

    // SAFETY: executor was allocated in begin_custom_scan.
    let executor = unsafe { (*state).accel.executor };
    if executor.is_null() {
        return unsafe { passthrough_exec(node) };
    }

    // SAFETY: executor is a valid pointer allocated in begin_custom_scan.
    let exec_state = unsafe { &mut *executor };

    // Get the child plan state from custom_ps.
    // SAFETY: custom_ps was populated by ExecInitCustomScan.
    let child_ps = unsafe {
        let custom_ps = (*node).custom_ps;
        if custom_ps.is_null() || pg_sys::list_length(custom_ps) == 0 {
            return std::ptr::null_mut();
        }
        pg_sys::list_nth(custom_ps, 0).cast::<pg_sys::PlanState>()
    };
    if child_ps.is_null() {
        return std::ptr::null_mut();
    }

    // Our scan slot — where we put the result tuple.
    // SAFETY: ss.ss_ScanTupleSlot is initialised by ExecInitCustomScan and
    // is a MinimalTupleTableSlot when scanrelid > 0 (base relation scan).
    // We must NOT use ps_ResultTupleSlot here because ExecStoreMinimalTuple
    // requires a MinimalTupleTableSlot — using a virtual slot would crash.
    let scan_slot = unsafe { (*node).ss.ss_ScanTupleSlot };

    // SAFETY: We are on the main backend thread. All pointers are valid.
    let result = unsafe { exec_state.next(child_ps, scan_slot) };

    // Sync counters back for EXPLAIN ANALYZE.
    unsafe {
        (*state).accel.rows_dispatched = exec_state.rows_dispatched;
        (*state).accel.batches_executed = exec_state.batches_executed;
        (*state).accel.dispatch_time_us = exec_state.dispatch_time_us;
    }

    result
}

/// `EndCustomScan`: clean up child plan states and free executor.
///
/// # Safety
///
/// Called by the executor on the main backend thread.
unsafe extern "C-unwind" fn end_custom_scan(node: *mut pg_sys::CustomScanState) {
    let state = node.cast::<GpuAccelScanState>();

    // Reclaim the Rust executor state to prevent leaks.
    unsafe {
        if !(*state).accel.executor.is_null() {
            // SAFETY: executor was allocated via Box::into_raw in begin_custom_scan.
            let _ = Box::from_raw((*state).accel.executor);
            (*state).accel.executor = std::ptr::null_mut();
        }
    }

    // SAFETY: Walk custom_ps and call ExecEndNode on each child.
    unsafe {
        let custom_ps = (*node).custom_ps;
        if !custom_ps.is_null() {
            let len = pg_sys::list_length(custom_ps);
            for i in 0..len {
                let child = pg_sys::list_nth(custom_ps, i).cast::<pg_sys::PlanState>();
                if !child.is_null() {
                    pg_sys::ExecEndNode(child);
                }
            }
        }
    }
}

/// `ReScanCustomScan`: handle rescanning (e.g., for nested loops).
///
/// # Safety
///
/// Called by the executor on the main backend thread.
unsafe extern "C-unwind" fn rescan_custom_scan(node: *mut pg_sys::CustomScanState) {
    let state = node.cast::<GpuAccelScanState>();

    // Drop the old executor state and create a fresh one, preserving the
    // qual and econtext pointers (they are owned by PG, not us).
    unsafe {
        let (qual, econtext) = if (*state).accel.executor.is_null() {
            (std::ptr::null_mut(), std::ptr::null_mut())
        } else {
            // SAFETY: executor was allocated via Box::into_raw.
            let old = Box::from_raw((*state).accel.executor);
            (old.qual_ptr(), old.econtext_ptr())
        };

        let batch_size = if (*state).accel.batch_size > 0 {
            (*state).accel.batch_size as usize
        } else {
            256
        };
        let exec_state = Box::new(ScanExecState::new(
            AccelStrategy::BatchedEval,
            batch_size,
            qual,
            econtext,
        ));
        (*state).accel.executor = Box::into_raw(exec_state);

        (*state).accel.rows_dispatched = 0;
        (*state).accel.batches_executed = 0;
        (*state).accel.dispatch_time_us = 0;
    }

    // SAFETY: Rescan all children.
    unsafe {
        let custom_ps = (*node).custom_ps;
        if !custom_ps.is_null() {
            let len = pg_sys::list_length(custom_ps);
            for i in 0..len {
                let child = pg_sys::list_nth(custom_ps, i).cast::<pg_sys::PlanState>();
                if !child.is_null() {
                    pg_sys::ExecReScan(child);
                }
            }
        }
    }
}

/// `ExplainCustomScan`: emit EXPLAIN output.
///
/// Always shows Strategy, Batch Size, Expected Threads. When `EXPLAIN ANALYZE`,
/// also shows Rows Dispatched, Batches, and Dispatch Time.
///
/// # Safety
///
/// Called by the executor on the main backend thread.
unsafe extern "C-unwind" fn explain_custom_scan(
    node: *mut pg_sys::CustomScanState,
    _ancestors: *mut pg_sys::List,
    es: *mut pg_sys::ExplainState,
) {
    let state = node.cast::<GpuAccelScanState>();

    // SAFETY: state is our extended struct, es is a valid ExplainState.
    unsafe {
        let strategy = GpuStrategy::from_i32((*state).accel.strategy);

        pg_sys::ExplainPropertyText(c"Strategy".as_ptr(), strategy.label().as_ptr(), es);
        pg_sys::ExplainPropertyInteger(
            c"Batch Size".as_ptr(),
            std::ptr::null(),
            i64::from((*state).accel.batch_size),
            es,
        );
        pg_sys::ExplainPropertyInteger(
            c"Expected Threads".as_ptr(),
            std::ptr::null(),
            i64::from((*state).accel.expected_threads),
            es,
        );

        // Execution stats only with EXPLAIN ANALYZE.
        if (*es).analyze {
            pg_sys::ExplainPropertyInteger(
                c"Rows Dispatched".as_ptr(),
                std::ptr::null(),
                (*state).accel.rows_dispatched as i64,
                es,
            );
            pg_sys::ExplainPropertyInteger(
                c"Batches".as_ptr(),
                std::ptr::null(),
                (*state).accel.batches_executed as i64,
                es,
            );

            #[allow(clippy::cast_precision_loss)]
            let time_ms = (*state).accel.dispatch_time_us as f64 / 1000.0;
            pg_sys::ExplainPropertyFloat(c"Dispatch Time".as_ptr(), c"ms".as_ptr(), time_ms, 3, es);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Determine expected thread count based on GUC settings.
fn resolve_thread_count() -> c_int {
    let workers = gucs::workers();
    if workers > 0 {
        return workers;
    }
    // Auto-detect: use half of available cores, minimum 1.
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as c_int)
        .unwrap_or(2);
    (cores / 2).max(1)
}

/// Passthrough execution: pull one tuple from the child and return it
/// directly, bypassing batch dispatch.
///
/// # Safety
///
/// Must be called on the main backend thread.
unsafe fn passthrough_exec(node: *mut pg_sys::CustomScanState) -> *mut pg_sys::TupleTableSlot {
    // SAFETY: PG's ExecInitCustomScan stored child plan states in custom_ps.
    unsafe {
        let custom_ps = (*node).custom_ps;
        if custom_ps.is_null() || pg_sys::list_length(custom_ps) == 0 {
            return std::ptr::null_mut();
        }
        let child_state = pg_sys::list_nth(custom_ps, 0).cast::<pg_sys::PlanState>();
        if child_state.is_null() {
            return std::ptr::null_mut();
        }
        pg_sys::ExecProcNode(child_state)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_labels() {
        assert_eq!(GpuStrategy::Scan.label(), c"GpuScan");
        assert_eq!(GpuStrategy::Join.label(), c"GpuJoin");
        assert_eq!(GpuStrategy::Agg.label(), c"GpuAgg");
        assert_eq!(GpuStrategy::Sort.label(), c"GpuSort");
    }

    #[test]
    fn strategy_from_i32_unknown_defaults_to_scan() {
        assert_eq!(GpuStrategy::from_i32(99), GpuStrategy::Scan);
        assert_eq!(GpuStrategy::from_i32(-1), GpuStrategy::Scan);
    }

    #[test]
    fn resolve_thread_count_returns_positive() {
        assert!(resolve_thread_count() >= 1);
    }
}
