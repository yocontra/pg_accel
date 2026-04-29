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
use crate::engine::executor::agg::{AggExecState, AggOp, GroupKeyInfo};
use crate::engine::executor::join::JoinExecState;
use crate::engine::executor::preagg::PreAggExecState;
use crate::engine::executor::scan::ScanExecState;
use crate::engine::executor::sort::{SORT_KEY_INTS, SortExecState, SortKeyDesc};
use crate::engine::executor::sort_scan::SortScan;
use crate::engine::executor::vectorized_scan::VectorizedScan;
use crate::engine::executor::window::{
    WINDOW_SPEC_INTS, WindowExecState, WindowFunc, WindowFuncSpec,
};
use crate::engine::registry::{self, AccelStrategy};
use crate::engine::stats;
use crate::gpu::PgaccelKeyType;

mod dsm;
mod explain;
mod plan_partial_agg;
mod private_data;

use explain::{explain_custom_scan, resolve_thread_count};
#[cfg(feature = "pg_test")]
use private_data::CustomPrivateData;
pub use private_data::serialize_preagg_private;
pub(super) use private_data::{PARTIAL_SENTINEL, append_partial_spec, deserialize_partial_spec};
use private_data::{deserialize_custom_private, deserialize_preagg_private};

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
    Window = 4,
    /// Fused star-join pre-aggregation: scan + N joins + aggregate in one node.
    PreAgg = 5,
}

impl GpuStrategy {
    /// Convert from raw integer, defaulting to `Scan` for unknown values.
    #[must_use]
    pub const fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::Join,
            2 => Self::Agg,
            3 => Self::Sort,
            4 => Self::Window,
            5 => Self::PreAgg,
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
            Self::Window => c"GpuWindow",
            Self::PreAgg => c"GpuPreAgg",
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
pub(super) struct GpuAccelScanState {
    pub(super) css: pg_sys::CustomScanState,
    pub(super) accel: GpuAccelState,
}

/// Per-node execution counters, config, and batch executor pointer.
#[repr(C)]
pub(super) struct GpuAccelState {
    pub(super) strategy: i32,
    pub(super) batch_size: i32,
    pub(super) expected_threads: i32,
    pub(super) rows_dispatched: u64,
    pub(super) batches_executed: u64,
    pub(super) dispatch_time_us: u64,
    /// Opaque pointer to heap-allocated Rust executor state.
    /// Points to either `ScanExecState` or `SortExecState` depending
    /// on `strategy`. Set in `begin_custom_scan`, freed in `end_custom_scan`.
    pub(super) executor: *mut std::ffi::c_void,
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

static SORT_PATH_METHODS: SyncPathMethods = SyncPathMethods(pg_sys::CustomPathMethods {
    CustomName: c"GpuAccelSort".as_ptr(),
    PlanCustomPath: Some(plan_custom_path_sort),
    ReparameterizeCustomPathByChild: None,
});

static AGG_PATH_METHODS: SyncPathMethods = SyncPathMethods(pg_sys::CustomPathMethods {
    CustomName: c"GpuAccelAgg".as_ptr(),
    PlanCustomPath: Some(plan_custom_path_agg),
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

static SORT_SCAN_METHODS: SyncScanMethods = SyncScanMethods(pg_sys::CustomScanMethods {
    CustomName: c"GpuAccelSort".as_ptr(),
    CreateCustomScanState: Some(create_custom_scan_state),
});

static AGG_SCAN_METHODS: SyncScanMethods = SyncScanMethods(pg_sys::CustomScanMethods {
    CustomName: c"GpuAccelAgg".as_ptr(),
    CreateCustomScanState: Some(create_custom_scan_state),
});

static WINDOW_PATH_METHODS: SyncPathMethods = SyncPathMethods(pg_sys::CustomPathMethods {
    CustomName: c"GpuAccelWindow".as_ptr(),
    PlanCustomPath: Some(plan_custom_path_window),
    ReparameterizeCustomPathByChild: None,
});

static WINDOW_SCAN_METHODS: SyncScanMethods = SyncScanMethods(pg_sys::CustomScanMethods {
    CustomName: c"GpuAccelWindow".as_ptr(),
    CreateCustomScanState: Some(create_custom_scan_state),
});

static PREAGG_PATH_METHODS: SyncPathMethods = SyncPathMethods(pg_sys::CustomPathMethods {
    CustomName: c"GpuAccelPreAgg".as_ptr(),
    PlanCustomPath: Some(plan_custom_path_preagg),
    ReparameterizeCustomPathByChild: None,
});

static PREAGG_SCAN_METHODS: SyncScanMethods = SyncScanMethods(pg_sys::CustomScanMethods {
    CustomName: c"GpuAccelPreAgg".as_ptr(),
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
    // Parallel-worker stubs. Each forked worker runs a complete, independent
    // instance of the Custom Scan — the partial output tuples are the only
    // cross-worker handoff (via Gather's shm_mq). No shared DSM state.
    EstimateDSMCustomScan: Some(dsm::estimate_dsm_custom_scan),
    InitializeDSMCustomScan: Some(dsm::initialize_dsm_custom_scan),
    ReInitializeDSMCustomScan: Some(dsm::reinitialize_dsm_custom_scan),
    InitializeWorkerCustomScan: Some(dsm::initialize_worker_custom_scan),
    ShutdownCustomScan: Some(dsm::shutdown_custom_scan),
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

/// Pointer to sort `CustomPathMethods` vtable.
#[inline]
#[must_use]
pub fn sort_path_methods() -> *const pg_sys::CustomPathMethods {
    &raw const SORT_PATH_METHODS.0
}

/// Pointer to agg `CustomPathMethods` vtable.
#[inline]
#[must_use]
pub fn agg_path_methods() -> *const pg_sys::CustomPathMethods {
    &raw const AGG_PATH_METHODS.0
}

/// Pointer to window `CustomPathMethods` vtable.
#[inline]
#[must_use]
pub fn window_path_methods() -> *const pg_sys::CustomPathMethods {
    &raw const WINDOW_PATH_METHODS.0
}

/// Pointer to preagg `CustomPathMethods` vtable.
#[inline]
#[must_use]
pub fn preagg_path_methods() -> *const pg_sys::CustomPathMethods {
    &raw const PREAGG_PATH_METHODS.0
}

/// Register Custom Scan methods with PostgreSQL. Must be called from `_PG_init`.
pub fn register() {
    // SAFETY: RegisterCustomScanMethods stores pointers to our static vtables
    // which live for the entire process lifetime. Called on main thread during
    // extension loading.
    unsafe {
        pg_sys::RegisterCustomScanMethods(&raw const SCAN_SCAN_METHODS.0);
        pg_sys::RegisterCustomScanMethods(&raw const JOIN_SCAN_METHODS.0);
        pg_sys::RegisterCustomScanMethods(&raw const SORT_SCAN_METHODS.0);
        pg_sys::RegisterCustomScanMethods(&raw const AGG_SCAN_METHODS.0);
        pg_sys::RegisterCustomScanMethods(&raw const WINDOW_SCAN_METHODS.0);
        pg_sys::RegisterCustomScanMethods(&raw const PREAGG_SCAN_METHODS.0);
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
    let _span = tracing::debug_span!("ffi.plan_custom_path_scan").entered();
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
    let _span = tracing::debug_span!("ffi.plan_custom_path_join").entered();
    tracing::info!("plan_custom_path_join: start");
    // SAFETY: all pointers originate from the planner and are valid.
    let plan =
        unsafe { make_custom_scan_plan(root, rel, best_path, tlist, clauses, custom_plans, false) };
    tracing::info!("plan_custom_path_join: end");
    plan
}

/// Convert a sort `CustomPath` into a `CustomScan` plan node.
///
/// Reads sort key descriptors from the path's `custom_private` and
/// serializes them into the plan's `custom_private` for the executor.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
unsafe extern "C-unwind" fn plan_custom_path_sort(
    _root: *mut pg_sys::PlannerInfo,
    _rel: *mut pg_sys::RelOptInfo,
    best_path: *mut pg_sys::CustomPath,
    tlist: *mut pg_sys::List,
    clauses: *mut pg_sys::List,
    custom_plans: *mut pg_sys::List,
) -> *mut pg_sys::Plan {
    let _span = tracing::debug_span!("ffi.plan_custom_path_sort").entered();
    tracing::info!("plan_custom_path_sort: start");
    // SAFETY: palloc0 returns zeroed memory in CurrentMemoryContext.
    let cscan = unsafe {
        pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomScan>()).cast::<pg_sys::CustomScan>()
    };

    // SAFETY: cscan is freshly palloc'd and zeroed; best_path is valid.
    unsafe {
        (*cscan).scan.plan.type_ = pg_sys::NodeTag::T_CustomScan;
        (*cscan).scan.plan.targetlist = tlist;
        // Set custom_scan_tlist to the child plan's targetlist so PG
        // creates a scan slot with the correct tuple descriptor.
        // Without this, scanrelid=0 + NULL tlist → 0-column slot,
        // crashing when sort_tuples calls slot_getattr.
        let child_tlist = if custom_plans.is_null() || pg_sys::list_length(custom_plans) == 0 {
            std::ptr::null_mut()
        } else {
            let child = pg_sys::list_nth(custom_plans, 0).cast::<pg_sys::Plan>();
            if child.is_null() {
                std::ptr::null_mut()
            } else {
                (*child).targetlist
            }
        };
        (*cscan).custom_scan_tlist = child_tlist;

        (*cscan).scan.plan.qual = pg_sys::extract_actual_clauses(clauses, false);
        (*cscan).scan.plan.startup_cost = (*best_path).path.startup_cost;
        (*cscan).scan.plan.total_cost = (*best_path).path.total_cost;
        (*cscan).scan.plan.plan_rows = (*best_path).path.rows;
        (*cscan).custom_plans = custom_plans;
        (*cscan).flags = (*best_path).flags;
        (*cscan).scan.scanrelid = 0;
        (*cscan).methods = &raw const SORT_SCAN_METHODS.0;

        // Serialize: [strategy=Sort, batch_size, threads, fn_oid=0,
        //             target_attno=0, accel_strategy=GpuSort, ...sort keys]
        let batch_size = gucs::min_batch_size();
        let expected_threads = resolve_thread_count();

        let mut list: *mut pg_sys::List = std::ptr::null_mut();
        list = pg_sys::lappend(list, pg_sys::makeInteger(GpuStrategy::Sort as c_int).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(batch_size).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(expected_threads).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // fn_oid
        list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // target_attno
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(AccelStrategy::GpuSort as c_int).cast(),
        );

        // Read sort keys and limit from the path's custom_private and
        // serialize them into the scan node's custom_private.
        // Path layout: [fn_oid, target_attno, accel_strategy, num_keys, ...sort_key_data..., limit_tuples]
        // Sort key data starts at index 3 (after the 3-element header).
        let path_priv = (*best_path).custom_private;
        let sort_keys = deserialize_path_sort_keys_at(path_priv, 3);
        list = serialize_sort_keys(list, &sort_keys);

        // Serialize limit_tuples (appended after sort keys by planner).
        // In the path layout, limit is at: 3 (header) + 1 (num_keys) + num_keys * SORT_KEY_INTS
        let path_limit = deserialize_path_limit_at(path_priv, 3, &sort_keys);
        list = pg_sys::lappend(list, pg_sys::makeInteger(path_limit).cast());

        (*cscan).custom_private = list;
    }

    tracing::info!("plan_custom_path_sort: end");
    cscan.cast()
}

/// Deserialize sort key descriptors from a path's `custom_private`.
///
/// Path layout: `[num_sort_keys, attno1, sort_op1, collation1, nulls_first1, ...]`
///
/// # Safety
///
/// `custom_private` must be null or a valid PG `List`.
#[allow(dead_code)]
unsafe fn deserialize_path_sort_keys(custom_private: *mut pg_sys::List) -> Vec<SortKeyDesc> {
    let mut sort_keys = vec![];
    if custom_private.is_null() {
        return sort_keys;
    }
    // SAFETY: custom_private is a valid List of Integer nodes.
    let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
    if list_len == 0 {
        return sort_keys;
    }
    let num_keys = unsafe { list_int_at(custom_private, 0) } as usize;
    let base = 1usize;
    for k in 0..num_keys {
        let offset = base + k * SORT_KEY_INTS;
        if offset + SORT_KEY_INTS > list_len {
            break;
        }
        // SAFETY: Indices are within bounds (checked above).
        let attno = unsafe { list_int_at(custom_private, offset as c_int) } as i16;
        let sort_op_raw = unsafe { list_int_at(custom_private, (offset + 1) as c_int) } as u32;
        let collation_raw = unsafe { list_int_at(custom_private, (offset + 2) as c_int) } as u32;
        let nulls_first = unsafe { list_int_at(custom_private, (offset + 3) as c_int) } != 0;
        sort_keys.push(SortKeyDesc {
            attno,
            sort_op: pg_sys::Oid::from(sort_op_raw),
            collation: pg_sys::Oid::from(collation_raw),
            nulls_first,
        });
    }
    sort_keys
}

/// Extract limit_tuples from the path's `custom_private`.
///
/// The limit is serialized after sort key data:
/// `[num_keys, ...sort_key_data..., limit_tuples]`
///
/// Returns 0 if no limit is present.
///
/// # Safety
///
/// `custom_private` must be null or a valid PG `List`.
#[allow(dead_code)]
unsafe fn deserialize_path_limit(
    custom_private: *mut pg_sys::List,
    sort_keys: &[SortKeyDesc],
) -> c_int {
    if custom_private.is_null() {
        return 0;
    }
    // SAFETY: custom_private is a valid List.
    let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
    // limit is at index: 1 (num_keys) + num_keys * SORT_KEY_INTS
    let limit_idx = 1 + sort_keys.len() * SORT_KEY_INTS;
    if limit_idx >= list_len {
        return 0;
    }
    // SAFETY: Index is within bounds (checked above).
    unsafe { list_int_at(custom_private, limit_idx as c_int) }
}

/// Extract limit_tuples from the path's `custom_private` with a base offset.
///
/// The limit is serialized after sort key data:
/// `[...header..., num_keys, ...sort_key_data..., limit_tuples]`
///
/// `header_offset` is the index where `num_keys` starts.
///
/// # Safety
///
/// `custom_private` must be null or a valid PG `List`.
unsafe fn deserialize_path_limit_at(
    custom_private: *mut pg_sys::List,
    header_offset: usize,
    sort_keys: &[SortKeyDesc],
) -> c_int {
    if custom_private.is_null() {
        return 0;
    }
    // SAFETY: custom_private is a valid List.
    let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
    // limit is at index: header_offset + 1 (num_keys) + num_keys * SORT_KEY_INTS
    let limit_idx = header_offset + 1 + sort_keys.len() * SORT_KEY_INTS;
    if limit_idx >= list_len {
        return 0;
    }
    // SAFETY: Index is within bounds (checked above).
    unsafe { list_int_at(custom_private, limit_idx as c_int) }
}

/// Deserialize sort key descriptors starting at a given index in the list.
///
/// Layout at `start_idx`: `[num_sort_keys, attno1, sort_op1, collation1, nulls_first1, ...]`
///
/// # Safety
///
/// `custom_private` must be a valid PG `List` with at least `start_idx + 1` elements.
unsafe fn deserialize_path_sort_keys_at(
    custom_private: *mut pg_sys::List,
    start_idx: usize,
) -> Vec<SortKeyDesc> {
    let mut sort_keys = vec![];
    if custom_private.is_null() {
        return sort_keys;
    }
    // SAFETY: custom_private is a valid List of Integer nodes.
    let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
    if start_idx >= list_len {
        return sort_keys;
    }
    let num_keys = unsafe { list_int_at(custom_private, start_idx as c_int) } as usize;
    let base = start_idx + 1;
    for k in 0..num_keys {
        let offset = base + k * SORT_KEY_INTS;
        if offset + SORT_KEY_INTS > list_len {
            break;
        }
        // SAFETY: Indices are within bounds (checked above).
        let attno = unsafe { list_int_at(custom_private, offset as c_int) } as i16;
        let sort_op_raw = unsafe { list_int_at(custom_private, (offset + 1) as c_int) } as u32;
        let collation_raw = unsafe { list_int_at(custom_private, (offset + 2) as c_int) } as u32;
        let nulls_first = unsafe { list_int_at(custom_private, (offset + 3) as c_int) } != 0;
        sort_keys.push(SortKeyDesc {
            attno,
            sort_op: pg_sys::Oid::from(sort_op_raw),
            collation: pg_sys::Oid::from(collation_raw),
            nulls_first,
        });
    }
    sort_keys
}

/// Shared helper: build a `CustomScan` plan from a `CustomPath`.
///
/// Convert an agg `CustomPath` into a `CustomScan` plan node.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
/// Build a `CustomScan` plan node for GPU aggregate.
///
/// The original `tlist` from the planner contains `Aggref` expressions
/// which PG's executor can only evaluate inside an `AggState`.  Since our
/// Custom Scan replaces the Agg node, we must build:
///
/// 1. **`custom_scan_tlist`** — one `TargetEntry(Var)` per aggregate
///    result column, defining the scan tuple descriptor.
/// 2. **`plan.targetlist`** — `TargetEntry(Var(INDEX_VAR))` entries that
///    project from the scan tuple, avoiding Aggref evaluation entirely.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
#[allow(clippy::too_many_lines, clippy::cast_ptr_alignment)]
unsafe extern "C-unwind" fn plan_custom_path_agg(
    _root: *mut pg_sys::PlannerInfo,
    _rel: *mut pg_sys::RelOptInfo,
    best_path: *mut pg_sys::CustomPath,
    tlist: *mut pg_sys::List,
    clauses: *mut pg_sys::List,
    custom_plans: *mut pg_sys::List,
) -> *mut pg_sys::Plan {
    let _span = tracing::debug_span!("ffi.plan_custom_path_agg").entered();
    tracing::info!("plan_custom_path_agg: start");
    // SAFETY: palloc0 returns zeroed memory in CurrentMemoryContext.
    let cscan = unsafe {
        pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomScan>()).cast::<pg_sys::CustomScan>()
    };

    // SAFETY: cscan is freshly palloc'd and zeroed; best_path is valid.
    unsafe {
        (*cscan).scan.plan.type_ = pg_sys::NodeTag::T_CustomScan;

        // Read aggregate descriptors + partial flag from the path's custom_private.
        // Path layout:
        //   [num_aggs, (op, attno, rtype)*N,
        //    has_gk, (gk_attno, gk_type_oid, gk_key_type)?  (3 payload ints when has_gk=1),
        //    self_scan_relid,
        //    is_partial,
        //    (PARTIAL_SENTINEL, n_cols, (op, attno, transtype, ser)*n_cols)?]
        //
        // The trailing PAAG sentinel block is optional — only present when
        // `partial_agg::try_inject` injected this path. Structural walk (vs
        // "last-1 / last-2") is required so the optional block doesn't shift
        // the positions of self_scan_relid / is_partial.
        let path_priv = (*best_path).custom_private;
        let num_aggs = list_int_at(path_priv, 0);
        let path_len = pg_sys::list_length(path_priv) as usize;
        let gk_base = 1 + (num_aggs as usize) * 3;
        let has_gk = if gk_base < path_len {
            list_int_at(path_priv, gk_base as c_int)
        } else {
            0
        };
        let self_scan_idx = gk_base + if has_gk != 0 { 4 } else { 1 };
        let is_partial_idx = self_scan_idx + 1;
        let self_scan_relid = if self_scan_idx < path_len {
            list_int_at(path_priv, self_scan_idx as c_int)
        } else {
            0
        };
        let is_partial = if is_partial_idx < path_len {
            list_int_at(path_priv, is_partial_idx as c_int)
        } else {
            0
        };
        let sentinel_idx = is_partial_idx + 1;
        let has_sentinel_block = sentinel_idx < path_len
            && list_int_at(path_priv, sentinel_idx as c_int) == PARTIAL_SENTINEL;

        // custom_scan_tlist: original expressions define the scan tuple layout
        //   (ExecTypeFromTL extracts types from Aggref.aggtype / Var.vartype).
        //
        // plan.targetlist shape depends on whether this is a partial-agg path:
        //   - non-partial: keep the original Aggref-bearing expressions so
        //     prepare_sort_from_pathkeys can still find group-key Vars for
        //     upper-level ORDER BY; fix_upper_expr rewrites callers to
        //     Var(INDEX_VAR, resno) referencing custom_scan_tlist.
        //   - partial: build an INDEX_VAR-only targetlist NOW so that
        //     set_plan_references never walks into raw Aggref sub-nodes (which
        //     would trigger `unrecognized node type: 9 (T_Aggref)` — or,
        //     after the planner zeroes out Aggref sub-fields in a partial
        //     context, `unrecognized node type: 0`).
        //
        // SAFETY: copyObjectImpl deep-copies the list in CurrentMemoryContext.
        (*cscan).custom_scan_tlist = pg_sys::copyObjectImpl(tlist.cast()).cast();

        // For partial paths, the Aggrefs in `tlist` are AGGSPLIT_INITIAL_SERIAL
        // (produced by make_partial_grouping_target on partially_grouped_rel's
        // reltarget). The Finalize Agg node above our Gather runs
        // `convert_combining_aggrefs` on its own tlist to build a matching
        // INITIAL_SERIAL Aggref; fix_upper_expr then looks up that Aggref in
        // our plan.targetlist via `equal()`. So partial mode needs the Aggref
        // itself in plan.targetlist — not an INDEX_VAR wrapper — otherwise
        // set_upper_references errors with "variable not found in subplan
        // target list".
        let _ = &plan_partial_agg::build_index_var_tlist; // still used by separate partial planner
        (*cscan).scan.plan.targetlist = pg_sys::copyObjectImpl(tlist.cast()).cast();

        (*cscan).scan.plan.qual = pg_sys::extract_actual_clauses(clauses, false);
        (*cscan).scan.plan.startup_cost = (*best_path).path.startup_cost;
        (*cscan).scan.plan.total_cost = (*best_path).path.total_cost;
        (*cscan).scan.plan.plan_rows = (*best_path).path.rows;
        (*cscan).custom_plans = custom_plans;
        (*cscan).flags = (*best_path).flags
            | if is_partial != 0 {
                pg_sys::CUSTOMPATH_SUPPORT_PROJECTION
            } else {
                0
            };
        (*cscan).scan.scanrelid = 0;
        (*cscan).methods = &raw const AGG_SCAN_METHODS.0;

        // Serialize: [strategy=Agg, batch_size, threads, fn_oid=0,
        //             target_attno=0, accel_strategy=GpuReduce,
        //             num_aggs, op0, attno0, op1, attno1, ...]
        let batch_size = gucs::min_batch_size();
        let expected_threads = resolve_thread_count();

        let mut list: *mut pg_sys::List = std::ptr::null_mut();
        list = pg_sys::lappend(list, pg_sys::makeInteger(GpuStrategy::Agg as c_int).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(batch_size).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(expected_threads).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // fn_oid
        list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // target_attno
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(AccelStrategy::GpuReduce as c_int).cast(),
        );
        // Build attno remap: original table attno → child slot position.
        // The child Seq Scan may project only referenced columns, so
        // the slot position differs from the original table attno when
        // unreferenced columns (e.g. a serial PK) are omitted.
        let mut attno_map: Vec<(c_int, c_int)> = Vec::new();
        if !custom_plans.is_null() && pg_sys::list_length(custom_plans) > 0 {
            let child_plan = pg_sys::list_nth(custom_plans, 0).cast::<pg_sys::Plan>();
            if !child_plan.is_null() {
                let child_tlist = (*child_plan).targetlist;
                if !child_tlist.is_null() {
                    let tlen = pg_sys::list_length(child_tlist);
                    for j in 0..tlen {
                        let tle = pg_sys::list_nth(child_tlist, j).cast::<pg_sys::TargetEntry>();
                        if tle.is_null() {
                            continue;
                        }
                        let expr = (*tle).expr;
                        if !expr.is_null()
                            && (*expr.cast::<pg_sys::Node>()).type_ == pg_sys::NodeTag::T_Var
                        {
                            let var = expr.cast::<pg_sys::Var>();
                            attno_map.push((i32::from((*var).varattno), i32::from((*tle).resno)));
                        }
                    }
                }
            }
        }
        // When self-scanning, attnos reference the base table directly —
        // no remapping needed.
        let remap_attno = |orig: c_int| -> c_int {
            if self_scan_relid > 0 {
                return orig;
            }
            for &(from, to) in &attno_map {
                if from == orig {
                    return to;
                }
            }
            orig
        };

        // Append aggregate descriptors (triples: op, attno, result_type_oid).
        // Attno is remapped from original table position to child slot position.
        list = pg_sys::lappend(list, pg_sys::makeInteger(num_aggs).cast());
        for k in 0..num_aggs {
            let op = list_int_at(path_priv, 1 + k * 3);
            let attno = list_int_at(path_priv, 2 + k * 3);
            let rtype = list_int_at(path_priv, 3 + k * 3);
            list = pg_sys::lappend(list, pg_sys::makeInteger(op).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(if attno > 0 { remap_attno(attno) } else { attno }).cast(),
            );
            list = pg_sys::lappend(list, pg_sys::makeInteger(rtype).cast());
        }

        // Forward group key info from path's custom_private.
        // Path layout: [num_aggs, (op,attno,rtype)*N, has_group_key, gk_attno,
        //               gk_type_oid, gk_key_type, gk2_attno, gk_tlist_pos, self_scan_relid]
        let gk_base = 1 + (num_aggs as usize) * 3;
        if gk_base < path_len {
            let has_gk = list_int_at(path_priv, gk_base as c_int);
            list = pg_sys::lappend(list, pg_sys::makeInteger(has_gk).cast());
            if has_gk != 0 && gk_base + 3 < path_len {
                let gk_attno = list_int_at(path_priv, (gk_base + 1) as c_int);
                let gk_type_oid = list_int_at(path_priv, (gk_base + 2) as c_int);
                let gk_key_type = list_int_at(path_priv, (gk_base + 3) as c_int);
                list = pg_sys::lappend(list, pg_sys::makeInteger(remap_attno(gk_attno)).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(gk_type_oid).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(gk_key_type).cast());
            }
        } else {
            // No group key info in path — plain aggregate.
            list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast());
        }

        // Append self-scan relid so begin_custom_scan can open the heap.
        list = pg_sys::lappend(list, pg_sys::makeInteger(self_scan_relid).cast());

        // When this path was injected on a partial (worker-side) Gather branch,
        // propagate the PartialAggSpec into the plan's custom_private using
        // the sentinel-prefixed layout. The deserializer treats absence of the
        // sentinel as `partial = None`.
        //
        // Prefer the path-attached sentinel block (carries transtype + serialize_fn
        // per column — needed for INTERNAL-state aggregates like AVG / STDDEV /
        // VAR). Fall back to the legacy triple-based reconstruction for paths
        // that predate the sentinel layout.
        if is_partial != 0 {
            // sentinel @ sentinel_idx, n_cols @ sentinel_idx + 1
            let spec_from_sentinel = if has_sentinel_block {
                deserialize_partial_spec(path_priv, sentinel_idx + 1)
            } else {
                None
            };
            let spec = match spec_from_sentinel {
                Some(s) => s,
                None => build_partial_spec_from_path(path_priv, num_aggs),
            };
            list = append_partial_spec(list, &spec);
        }

        (*cscan).custom_private = list;
    }

    tracing::info!("plan_custom_path_agg: end");
    cscan.cast()
}

/// Build per-column [`PartialEmitter`]s from a [`PartialAggSpec`].
///
/// Selects the correct concrete emitter for each op/transtype pair:
/// - `COUNT(*)` / `COUNT(x)` → `CountEmitter` (int8)
/// - `SUM(int4)` → `IntegerSumPromotion` (int8)
/// - `SUM(int8)` / `SUM(numeric)` → `NumericSumEmitter`
/// - `SUM(float4)` / `SUM(float8)` / `MIN` / `MAX` with scalar transtype →
///   `ScalarPassthrough`
/// - `AVG` / STDDEV / VAR with INTERNAL transtype → `Float8StatsEmitter`
///   (requires `serialize_fn_oid`)
fn build_partial_emitters(
    spec: &crate::engine::executor::agg::partial::PartialAggSpec,
) -> Vec<Box<dyn crate::engine::executor::agg::partial::PartialEmitter>> {
    use crate::engine::executor::agg::partial::PartialEmitter;
    use crate::engine::executor::agg::partial::emitter::{
        CountEmitter, Float8StatsEmitter, IntegerSumPromotion, NumericSumEmitter, ScalarPassthrough,
    };

    let mut out: Vec<Box<dyn PartialEmitter>> = Vec::with_capacity(spec.per_column.len());
    for col in &spec.per_column {
        let emitter: Box<dyn PartialEmitter> = match col.op {
            AggOp::Count => Box::new(CountEmitter),
            AggOp::Sum if col.transtype_oid == pg_sys::INT8OID => Box::new(IntegerSumPromotion),
            AggOp::Sum if col.transtype_oid == pg_sys::NUMERICOID => Box::new(NumericSumEmitter),
            AggOp::Avg | AggOp::StddevSamp | AggOp::StddevPop | AggOp::VarSamp | AggOp::VarPop => {
                // Float8StatsEmitter handles both transtype shapes:
                //  - INTERNAL transtype (numeric_accum, int8_accum) → serialize_fn
                //    wraps the float8[3] in a bytea, aggtype=BYTEAOID.
                //  - `_float8` transtype (float4_accum, float8_accum) → no
                //    serialize fn, emits the float8[3] array directly,
                //    aggtype=`_float8`.
                // In both cases the TupleDesc type (set by mark_partial_aggref)
                // matches the emitter output type.
                let ser = col.serialize_fn_oid.unwrap_or(pg_sys::InvalidOid);
                Box::new(Float8StatsEmitter {
                    serialize_fn_oid: ser,
                })
            }
            _ => Box::new(ScalarPassthrough {
                transtype: col.transtype_oid,
            }),
        };
        out.push(emitter);
    }
    out
}

/// Build a [`PartialAggSpec`] from a partial-agg path's `custom_private`.
///
/// Resolves each column's `aggtranstype` and optional `aggserialfn` via
/// syscache readers. We don't have direct access to the Aggref OIDs here;
/// callers should populate those during path construction — for now the
/// spec mirrors the plan's (op, attno, result_type) triples with transtype
/// defaulted to the result type and no serialize fn.
///
/// # Safety
/// `path_priv` must be the valid `custom_private` list of a partial-agg path.
unsafe fn build_partial_spec_from_path(
    path_priv: *mut pg_sys::List,
    num_aggs: c_int,
) -> crate::engine::executor::agg::partial::PartialAggSpec {
    use crate::engine::executor::agg::partial::{PartialAggSpec, PartialColumn};

    let mut per_column = Vec::with_capacity(num_aggs as usize);
    for k in 0..num_aggs {
        // SAFETY: path_priv is a valid Integer list with at least (1 + 3*num_aggs)
        // entries (the planner enforces this layout).
        let op_raw = unsafe { list_int_at(path_priv, 1 + k * 3) };
        let attno = unsafe { list_int_at(path_priv, 2 + k * 3) };
        let rtype = unsafe { list_int_at(path_priv, 3 + k * 3) } as u32;
        let op = AggOp::from_i32(op_raw);
        // Partial-agg injection path (planner_hooks) rejects INTERNAL-state
        // aggregates (AVG/STDDEV/VAR/SUM(int8)/SUM(numeric)) — the transition
        // type is therefore the same as the aggregate's result type.
        per_column.push(PartialColumn {
            op,
            attno,
            transtype_oid: pg_sys::Oid::from(rtype),
            serialize_fn_oid: None,
        });
    }
    PartialAggSpec { per_column }
}

/// Convert a window `CustomPath` into a `CustomScan` plan node.
///
/// Reads window function specs from the path's `custom_private` and
/// serializes them into the plan's `custom_private` for the executor.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
#[allow(clippy::too_many_lines, clippy::cast_ptr_alignment)]
unsafe extern "C-unwind" fn plan_custom_path_window(
    _root: *mut pg_sys::PlannerInfo,
    _rel: *mut pg_sys::RelOptInfo,
    best_path: *mut pg_sys::CustomPath,
    tlist: *mut pg_sys::List,
    clauses: *mut pg_sys::List,
    custom_plans: *mut pg_sys::List,
) -> *mut pg_sys::Plan {
    let _span = tracing::debug_span!("ffi.plan_custom_path_window").entered();
    tracing::info!("plan_custom_path_window: start");
    // SAFETY: palloc0 returns zeroed memory in CurrentMemoryContext.
    let cscan = unsafe {
        pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomScan>()).cast::<pg_sys::CustomScan>()
    };

    // SAFETY: cscan is freshly palloc'd and zeroed; best_path is valid.
    unsafe {
        (*cscan).scan.plan.type_ = pg_sys::NodeTag::T_CustomScan;

        // PG may pass an empty tlist when the CustomScan is not the top-level
        // plan node (e.g., under Limit). In that case, use root->processed_tlist
        // which has the full query target list including WindowFunc expressions.
        // apply_tlist_labeling (createplan.c:360) asserts that our plan's
        // targetlist length matches root->processed_tlist length.
        let effective_tlist = if tlist.is_null() || pg_sys::list_length(tlist) == 0 {
            (*_root).processed_tlist
        } else {
            tlist
        };

        // Use the original tlist expressions so PG's set_customscan_references
        // can map WindowFunc / Var references correctly — same approach as agg.
        // SAFETY: copyObjectImpl deep-copies the list in CurrentMemoryContext.
        (*cscan).custom_scan_tlist = pg_sys::copyObjectImpl(effective_tlist.cast()).cast();
        (*cscan).scan.plan.targetlist = pg_sys::copyObjectImpl(effective_tlist.cast()).cast();

        (*cscan).scan.plan.qual = pg_sys::extract_actual_clauses(clauses, false);
        (*cscan).scan.plan.startup_cost = (*best_path).path.startup_cost;
        (*cscan).scan.plan.total_cost = (*best_path).path.total_cost;
        (*cscan).scan.plan.plan_rows = (*best_path).path.rows;
        (*cscan).custom_plans = custom_plans;
        (*cscan).flags = (*best_path).flags;
        (*cscan).scan.scanrelid = 0;
        (*cscan).methods = &raw const WINDOW_SCAN_METHODS.0;

        // Serialize: [strategy=Window, batch_size, threads, fn_oid=0,
        //             target_attno=0, accel_strategy=GpuWindow,
        //             num_specs, spec0..., spec1..., ...]
        let batch_size = gucs::min_batch_size();
        let expected_threads = resolve_thread_count();

        let path_priv = (*best_path).custom_private;
        let path_len = if path_priv.is_null() {
            0
        } else {
            pg_sys::list_length(path_priv) as usize
        };

        let mut list: *mut pg_sys::List = std::ptr::null_mut();
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(GpuStrategy::Window as c_int).cast(),
        );
        list = pg_sys::lappend(list, pg_sys::makeInteger(batch_size).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(expected_threads).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // fn_oid
        list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // target_attno
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(AccelStrategy::GpuWindow as c_int).cast(),
        );

        // Build attno remap: original table attno → child slot position.
        // The child plan (Sort/SeqScan) may project only referenced columns,
        // so the slot position differs from the original table attno.
        let mut attno_map: Vec<(c_int, c_int)> = Vec::new();
        if !custom_plans.is_null() && pg_sys::list_length(custom_plans) > 0 {
            let child_plan = pg_sys::list_nth(custom_plans, 0).cast::<pg_sys::Plan>();
            if !child_plan.is_null() {
                let child_tlist = (*child_plan).targetlist;
                if !child_tlist.is_null() {
                    let tlen = pg_sys::list_length(child_tlist);
                    for j in 0..tlen {
                        let tle = pg_sys::list_nth(child_tlist, j).cast::<pg_sys::TargetEntry>();
                        if tle.is_null() {
                            continue;
                        }
                        let expr = (*tle).expr;
                        if !expr.is_null()
                            && (*expr.cast::<pg_sys::Node>()).type_ == pg_sys::NodeTag::T_Var
                        {
                            let var = expr.cast::<pg_sys::Var>();
                            let table_attno = i32::from((*var).varattno);
                            let slot_pos = i32::from((*tle).resno);
                            attno_map.push((table_attno, slot_pos));
                        }
                    }
                }
            }
        }
        let remap = |orig: c_int| -> c_int {
            for &(table_a, slot_p) in &attno_map {
                if table_a == orig {
                    return slot_p;
                }
            }
            orig // no remap found — use as-is
        };

        // Copy window specs from path's custom_private, remapping attnos.
        // Path layout: [num_specs, spec0..., scan_relid]
        if path_len > 0 {
            let num_specs = list_int_at(path_priv, 0);
            list = pg_sys::lappend(list, pg_sys::makeInteger(num_specs).cast());
            for k in 0..num_specs {
                let base = 1 + k * WINDOW_SPEC_INTS as c_int;
                // Fields: [func, part_attno, order_attno, value_attno,
                //          offset, default_bits, result_type]
                // Remap indices 1,2,3 (part_attno, order_attno, value_attno).
                for j in 0..WINDOW_SPEC_INTS as c_int {
                    if (base + j) < path_len as c_int {
                        let val = list_int_at(path_priv, base + j);
                        let remapped = if (1..=3).contains(&j) && val > 0 {
                            remap(val)
                        } else {
                            val
                        };
                        list = pg_sys::lappend(list, pg_sys::makeInteger(remapped).cast());
                    }
                }
            }

            // Copy scan_relid (last element in path private, after all specs).
            let scan_relid_idx = 1 + num_specs * WINDOW_SPEC_INTS as c_int;
            let scan_relid = if scan_relid_idx < path_len as c_int {
                list_int_at(path_priv, scan_relid_idx)
            } else {
                0
            };
            list = pg_sys::lappend(list, pg_sys::makeInteger(scan_relid).cast());
        }

        (*cscan).custom_private = list;
    }

    tracing::info!("plan_custom_path_window: end");
    cscan.cast()
}

/// Convert a PreAgg `CustomPath` into a `CustomScan` plan node.
///
/// The PreAgg path carries all join depths + aggregation info in
/// `custom_private`. This callback copies that into the plan node
/// and sets up inner (dimension) plans.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
unsafe extern "C-unwind" fn plan_custom_path_preagg(
    _root: *mut pg_sys::PlannerInfo,
    _rel: *mut pg_sys::RelOptInfo,
    best_path: *mut pg_sys::CustomPath,
    tlist: *mut pg_sys::List,
    clauses: *mut pg_sys::List,
    custom_plans: *mut pg_sys::List,
) -> *mut pg_sys::Plan {
    let _span = tracing::debug_span!("ffi.plan_custom_path_preagg").entered();
    tracing::info!("plan_custom_path_preagg: start");
    // SAFETY: palloc0 returns zeroed memory in CurrentMemoryContext.
    let cscan = unsafe {
        pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomScan>()).cast::<pg_sys::CustomScan>()
    };

    // SAFETY: cscan is freshly palloc'd and zeroed; best_path is valid.
    unsafe {
        (*cscan).scan.plan.type_ = pg_sys::NodeTag::T_CustomScan;

        // PreAgg has scanrelid=0 and the planner-supplied tlist contains raw
        // base-relation Vars (GROUP BY keys referencing fact/dim relids) plus
        // Aggrefs. For scanrelid=0 CustomScans, PG's
        // `set_customscan_references` treats the node as an upper-plan
        // projection: it builds an index from `custom_scan_tlist` and calls
        // `fix_upper_expr` on `plan.targetlist`/`plan.qual`, rewriting each
        // Var/Aggref in-place to `Var(INDEX_VAR, position_in_cst)`. Both lists
        // must hold independent copies so rewriting one does not alias the
        // other — same pattern as plan_custom_path_agg.
        // SAFETY: copyObjectImpl deep-copies in CurrentMemoryContext.
        (*cscan).custom_scan_tlist = pg_sys::copyObjectImpl(tlist.cast()).cast();
        (*cscan).scan.plan.targetlist = pg_sys::copyObjectImpl(tlist.cast()).cast();

        (*cscan).scan.plan.qual = pg_sys::extract_actual_clauses(clauses, false);
        (*cscan).scan.plan.startup_cost = (*best_path).path.startup_cost;
        (*cscan).scan.plan.total_cost = (*best_path).path.total_cost;
        (*cscan).scan.plan.plan_rows = (*best_path).path.rows;
        (*cscan).custom_plans = custom_plans;
        (*cscan).flags = (*best_path).flags;
        (*cscan).scan.scanrelid = 0;
        (*cscan).methods = &raw const PREAGG_SCAN_METHODS.0;

        // Copy custom_private from path to plan (already serialized by planner hook).
        (*cscan).custom_private = (*best_path).custom_private;
    }

    tracing::info!("plan_custom_path_preagg: end");
    cscan.cast()
}

/// Serializes strategy metadata into `custom_private` as a `List` of
/// `Integer` nodes: [strategy, batch_size, expected_threads].
///
/// # Safety
///
/// All pointer arguments must originate from the PostgreSQL planner.
unsafe fn make_custom_scan_plan(
    root: *mut pg_sys::PlannerInfo,
    rel: *mut pg_sys::RelOptInfo,
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

        // The Custom Scan wraps a child plan (e.g. SeqScan) whose targetlist
        // may be optimised to only include needed columns. We use scanrelid=0
        // with custom_scan_tlist set to the child's targetlist so PG creates
        // scan_slot matching the child's actual output. plan.targetlist is the
        // planner-provided expression list; PG maps Var references through
        // custom_scan_tlist automatically.
        //
        // For join nodes, custom_scan_tlist is also used (already scanrelid=0).
        (*cscan).scan.plan.targetlist = tlist;

        // Read accel_strategy early — needed to decide scanrelid and tlist.
        let path_priv_early = (*best_path).custom_private;
        let accel_strategy_raw_early =
            if !path_priv_early.is_null() && pg_sys::list_length(path_priv_early) > 2 {
                list_int_at(path_priv_early, 2)
            } else {
                0
            };
        let is_gpu_expr = accel_strategy_raw_early == AccelStrategy::GpuExpr as c_int;

        // custom_scan_tlist defines which columns the scan slot has.
        // For GpuExpr scan: scanrelid > 0 (direct heap scan) — PG uses
        // the table's TupleDesc directly, custom_scan_tlist MUST be NIL.
        // Having both scanrelid > 0 AND custom_scan_tlist != NIL crashes
        // in set_customscan_references (setrefs.c).
        // For other scan nodes: use build_physical_tlist to include ALL
        // table columns so we can extract geometry for GPU dispatch.
        // For join nodes: use a copy of the planner-provided tlist.
        if is_scan && is_gpu_expr {
            // GpuExpr: scanrelid > 0, PG handles TupleDesc from relation.
            (*cscan).custom_scan_tlist = std::ptr::null_mut();
        } else if is_scan {
            // Non-GpuExpr scan (spatial/h3/raster): use build_physical_tlist.
            // SAFETY: root and rel are valid planner pointers.
            (*cscan).custom_scan_tlist = pg_sys::build_physical_tlist(root, rel);
        } else {
            // SAFETY: copyObjectImpl deep-copies in CurrentMemoryContext.
            (*cscan).custom_scan_tlist = pg_sys::copyObjectImpl(tlist.cast()).cast();
        }
        // SAFETY: extract_actual_clauses strips RestrictInfo wrappers,
        // returning the actual qual expressions for ExecInitCustomScan
        // to compile into ExprState for per-tuple evaluation.
        (*cscan).scan.plan.qual = pg_sys::extract_actual_clauses(clauses, false);
        (*cscan).scan.plan.startup_cost = (*best_path).path.startup_cost;
        (*cscan).scan.plan.total_cost = (*best_path).path.total_cost;
        (*cscan).scan.plan.plan_rows = (*best_path).path.rows;
        (*cscan).custom_plans = custom_plans;
        (*cscan).flags = (*best_path).flags;

        // For GpuExpr scan: use scanrelid > 0 (direct heap scan, no child).
        // PG opens the table for us and the scan slot gets the table's
        // full TupleDesc. No child plan column mismatch issues.
        //
        // For other scan nodes: keep scanrelid=0 with child plan. Strip
        // the child's qual so we evaluate the filter ourselves.
        if is_scan && is_gpu_expr {
            (*cscan).scan.scanrelid = (*rel).relid;
            (*cscan).custom_plans = std::ptr::null_mut();
        } else {
            (*cscan).scan.scanrelid = 0;
            // Strip child qual for non-GpuExpr scans (GPU evaluates filter).
            if is_scan && !custom_plans.is_null() && pg_sys::list_length(custom_plans) > 0 {
                let child = pg_sys::list_nth(custom_plans, 0).cast::<pg_sys::Plan>();
                if !child.is_null() {
                    (*child).qual = std::ptr::null_mut();
                    // SAFETY: build_physical_tlist creates a tlist with all
                    // physical columns from the relation.
                    (*child).targetlist = pg_sys::build_physical_tlist(root, rel);
                }
            }
        }
        if is_scan {
            (*cscan).methods = &raw const SCAN_SCAN_METHODS.0;
        } else {
            (*cscan).methods = &raw const JOIN_SCAN_METHODS.0;
        }

        // Serialize strategy info into custom_private.
        // Layout: [strategy, batch_size, expected_threads, fn_oid,
        //          target_attno, accel_strategy, ...sort keys if Sort]
        let batch_size = gucs::min_batch_size();
        let expected_threads = resolve_thread_count();

        // Read fn_oid, target_attno, accel_strategy from the path's
        // custom_private (serialized by the planner hook).
        // Layout on path: [fn_oid, target_attno, accel_strategy, ...sort keys]
        let path_priv = (*best_path).custom_private;
        let fn_oid_raw = list_int_at(path_priv, 0);
        let target_attno = list_int_at(path_priv, 1);
        let accel_strategy_raw = list_int_at(path_priv, 2);

        // If accel_strategy is GpuSort, use GpuStrategy::Sort instead of
        // Scan/Join so the executor picks the sort code path.
        let is_sort = accel_strategy_raw == AccelStrategy::GpuSort as c_int;
        let strategy = if is_sort {
            GpuStrategy::Sort as c_int
        } else if is_scan {
            GpuStrategy::Scan as c_int
        } else {
            GpuStrategy::Join as c_int
        };

        // SAFETY: makeInteger + lappend allocate in CurrentMemoryContext.
        let mut list: *mut pg_sys::List = std::ptr::null_mut();
        list = pg_sys::lappend(list, pg_sys::makeInteger(strategy).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(batch_size).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(expected_threads).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(fn_oid_raw).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(target_attno).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(accel_strategy_raw).cast());

        // For GpuSort, read sort keys, limit, and self_scan_relid from
        // the path's custom_private (after the 3-element header) and
        // serialize them into the plan's custom_private.
        if is_sort {
            let path_priv_len = pg_sys::list_length(path_priv) as usize;
            if path_priv_len > 3 {
                // Sort key data starts at index 3: [num_keys, attno, op, coll, nf]
                let sort_keys = deserialize_path_sort_keys_at(path_priv, 3);
                list = serialize_sort_keys(list, &sort_keys);

                // Serialize limit_tuples (after sort keys in path layout).
                let path_limit = deserialize_path_limit_at(path_priv, 3, &sort_keys);
                list = pg_sys::lappend(list, pg_sys::makeInteger(path_limit).cast());

                // Serialize self_scan_relid for VectorizedScan.
                // In the path layout: limit is at 3 + 1 + num_keys * SORT_KEY_INTS,
                // self_scan_relid is one after that.
                let relid_idx = 3 + 1 + sort_keys.len() * SORT_KEY_INTS + 1;
                let self_scan_relid = if relid_idx < path_priv_len {
                    list_int_at(path_priv, relid_idx as c_int)
                } else {
                    0
                };
                list = pg_sys::lappend(list, pg_sys::makeInteger(self_scan_relid).cast());
            }
        }

        // For GpuHashJoin, serialize inner_attno and key_type after the
        // base 6 fields. Path layout: [fn_oid, outer_attno, strategy,
        //                               inner_attno, key_type]
        //
        // The planner's varattno values reference original table columns,
        // but child plans may project only needed columns. We remap
        // outer_attno and inner_attno to match child plan output positions.
        if accel_strategy_raw == AccelStrategy::GpuHashJoin as c_int {
            let path_priv_len = pg_sys::list_length(path_priv) as usize;
            if path_priv_len > 4 {
                let raw_inner_attno = list_int_at(path_priv, 3);
                let key_type = list_int_at(path_priv, 4);

                // Read original varnos for disambiguating attno matches
                // in multi-table child outputs (e.g., nested CustomScans).
                let outer_varno = if path_priv_len > 5 {
                    list_int_at(path_priv, 5)
                } else {
                    0
                };
                let inner_varno = if path_priv_len > 6 {
                    list_int_at(path_priv, 6)
                } else {
                    0
                };

                // Build attno→resno mapping from child plan output lists.
                // Child 0 = outer, child 1 = inner.
                // For CustomScan children (scanrelid=0), use custom_scan_tlist
                // since plan.targetlist has INDEX_VAR references.
                // For SeqScan/other children, use plan.targetlist directly.
                // `orig_varno` disambiguates when multiple tables share attno values.
                let remap_via_child = |child_idx: c_int,
                                       orig_attno: c_int,
                                       orig_varno: c_int|
                 -> c_int {
                    if custom_plans.is_null() {
                        return orig_attno;
                    }
                    let n_children = pg_sys::list_length(custom_plans);
                    if child_idx >= n_children {
                        return orig_attno;
                    }
                    let child = pg_sys::list_nth(custom_plans, child_idx).cast::<pg_sys::Plan>();
                    if child.is_null() {
                        return orig_attno;
                    }

                    // Check if child is a CustomScan (scanrelid=0).
                    let child_scanrelid = (*child.cast::<pg_sys::Scan>()).scanrelid;
                    let search_tlist = if child_scanrelid == 0
                        && (*child.cast::<pg_sys::Node>()).type_ == pg_sys::NodeTag::T_CustomScan
                    {
                        // For CustomScan: search custom_scan_tlist which
                        // has original relation Vars.
                        (*child.cast::<pg_sys::CustomScan>()).custom_scan_tlist
                    } else {
                        // For regular nodes: use plan.targetlist.
                        (*child).targetlist
                    };

                    if search_tlist.is_null() {
                        return orig_attno;
                    }
                    let tlen = pg_sys::list_length(search_tlist);
                    for j in 0..tlen {
                        let tle = pg_sys::list_nth(search_tlist, j).cast::<pg_sys::TargetEntry>();
                        if tle.is_null() {
                            continue;
                        }
                        let expr = (*tle).expr;
                        if expr.is_null() {
                            continue;
                        }
                        if (*expr.cast::<pg_sys::Node>()).type_ == pg_sys::NodeTag::T_Var {
                            let var = expr.cast::<pg_sys::Var>();
                            let v_attno = i32::from((*var).varattno);
                            let v_varno = (*var).varno;
                            // Match on attno AND varno (when varno is known).
                            if v_attno == orig_attno && (orig_varno == 0 || v_varno == orig_varno) {
                                return i32::from((*tle).resno);
                            }
                        }
                    }
                    orig_attno
                };

                // Remap outer_attno (already at index 4 in list = target_attno).
                // We need to fix index 4 in the already-built list. Instead,
                // we remap and store the corrected values at indices 6+.
                let remapped_outer = remap_via_child(0, target_attno, outer_varno);
                let remapped_inner = remap_via_child(1, raw_inner_attno, inner_varno);

                // Overwrite target_attno (index 4) with remapped outer attno.
                // SAFETY: index 4 is within list bounds (we appended 6 items).
                let cell4 = pg_sys::list_nth(list, 4).cast::<pg_sys::Integer>();
                (*cell4).ival = remapped_outer;

                list = pg_sys::lappend(list, pg_sys::makeInteger(remapped_inner).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(key_type).cast());
            }
        }

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
    let _span = tracing::debug_span!("ffi.create_custom_scan_state").entered();
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

        // All scan strategies use VirtualTupleTableSlot (PG default).
        // ExecForceStoreHeapTuple triggers heap_deform_tuple (~50ns) which
        // populates tts_values/tts_isnull for correct aggregate evaluation.
    }

    state.cast()
}

// ---------------------------------------------------------------------------
// GPU context discovery from plan quals
// ---------------------------------------------------------------------------

/// Result of scanning a qual tree for a registered accelerable function.
struct AccelContext {
    fn_oid: pg_sys::Oid,
    strategy: AccelStrategy,
    /// 1-based attribute number of the Var argument, or 0 if none found.
    target_attno: i32,
    /// Constant second argument datum (e.g. the constant geometry in
    /// `WHERE ST_Intersects(geom_col, $const)`). `None` when both args
    /// are column references or no constant is found.
    qual_datum: Option<(pg_sys::Datum, bool)>,
}

/// Walk a list of qual expressions (from `plan.qual`) to find the first
/// function registered in the acceleration registry. Returns the function
/// OID, its strategy, and the `attno` of any `Var` argument (the column
/// reference to extract for GPU dispatch).
///
/// # Safety
///
/// `qual_list` must be null or a valid PG `List` of expression nodes.
unsafe fn find_accel_context_in_qual(qual_list: *mut pg_sys::List) -> Option<AccelContext> {
    if qual_list.is_null() {
        return None;
    }

    let reg = registry::global_registry();

    // SAFETY: qual_list is a valid List from the plan node.
    let len = unsafe { pg_sys::list_length(qual_list) };
    for i in 0..len {
        // SAFETY: i is in [0, len).
        let node = unsafe { pg_sys::list_nth(qual_list, i).cast::<pg_sys::Node>() };
        if let Some(ctx) = find_accel_in_node(node, reg) {
            return Some(ctx);
        }
    }

    None
}

/// Recursively search a single expression node for a registered function.
/// When found, also extract the `attno` from the first `Var` argument.
///
/// Handles `FuncExpr`, `OpExpr`, and `BoolExpr` (AND/OR/NOT).
#[allow(clippy::cast_ptr_alignment)]
fn find_accel_in_node(
    node: *mut pg_sys::Node,
    reg: &registry::AdapterRegistry,
) -> Option<AccelContext> {
    if node.is_null() {
        return None;
    }

    // SAFETY: node is a valid PG Node; reading its tag.
    let tag = unsafe { (*node).type_ };

    match tag {
        pg_sys::NodeTag::T_FuncExpr => {
            // SAFETY: tag confirmed FuncExpr.
            let funcexpr = node.cast::<pg_sys::FuncExpr>();
            let oid = unsafe { (*funcexpr).funcid };
            if let Some(entry) = reg.lookup(oid) {
                let args = unsafe { (*funcexpr).args };
                let attno = extract_var_attno(args);
                // SAFETY: args is a valid List; extract_const_datum reads Const nodes.
                let qual_datum = unsafe { extract_const_datum(args) };
                return Some(AccelContext {
                    fn_oid: oid,
                    strategy: entry.strategy,
                    target_attno: attno,
                    qual_datum,
                });
            }
            // SAFETY: tag confirmed FuncExpr; reading args list.
            recurse_args(unsafe { (*funcexpr).args }, reg)
        }
        pg_sys::NodeTag::T_OpExpr => {
            // SAFETY: tag confirmed OpExpr; force-resolve opfuncid.
            let opexpr = node.cast::<pg_sys::OpExpr>();
            unsafe { pg_sys::set_opfuncid(opexpr) };
            let oid = unsafe { (*opexpr).opfuncid };
            if let Some(entry) = reg.lookup(oid) {
                // SAFETY: tag confirmed OpExpr; reading args list.
                let args = unsafe { (*opexpr).args };
                let attno = extract_var_attno(args);
                // SAFETY: args is a valid List; extract_const_datum reads Const nodes.
                let qual_datum = unsafe { extract_const_datum(args) };
                return Some(AccelContext {
                    fn_oid: oid,
                    strategy: entry.strategy,
                    target_attno: attno,
                    qual_datum,
                });
            }
            // SAFETY: tag confirmed OpExpr; reading args list for recursion.
            recurse_args(unsafe { (*opexpr).args }, reg)
        }
        pg_sys::NodeTag::T_BoolExpr => {
            // SAFETY: tag confirmed BoolExpr; reading args list.
            let args = unsafe { (*node.cast::<pg_sys::BoolExpr>()).args };
            recurse_args(args, reg)
        }
        _ => None,
    }
}

/// Recurse into a `List` of expression nodes looking for an accelerable
/// function.
fn recurse_args(args: *mut pg_sys::List, reg: &registry::AdapterRegistry) -> Option<AccelContext> {
    if args.is_null() {
        return None;
    }
    // SAFETY: args is a valid non-null List.
    let len = unsafe { pg_sys::list_length(args) };
    for j in 0..len {
        // SAFETY: j is in [0, len).
        let child = unsafe { pg_sys::list_nth(args, j).cast::<pg_sys::Node>() };
        if let Some(ctx) = find_accel_in_node(child, reg) {
            return Some(ctx);
        }
    }
    None
}

/// Extract the `varattno` (1-based column number) from the first `Var` node
/// found in a function's argument list. Returns 0 if no `Var` is found.
#[allow(clippy::cast_ptr_alignment)]
fn extract_var_attno(args: *mut pg_sys::List) -> i32 {
    if args.is_null() {
        return 0;
    }
    // SAFETY: args is a valid List from the planner.
    let len = unsafe { pg_sys::list_length(args) };
    for i in 0..len {
        // SAFETY: i is in [0, len).
        let node = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
        if node.is_null() {
            continue;
        }
        // SAFETY: reading the node tag.
        if unsafe { (*node).type_ } == pg_sys::NodeTag::T_Var {
            // SAFETY: tag confirmed Var; reading varattno.
            let var = node.cast::<pg_sys::Var>();
            return i32::from(unsafe { (*var).varattno });
        }
    }
    0
}

/// Extract the constant (`Const`) datum from a function's argument list.
/// For 2-arg spatial predicates like `ST_Contains(geom_col, $const_geom)`,
/// this returns the constant geometry datum that the GPU pipeline needs as
/// the second input to form geometry pairs.
///
/// Returns `None` if no `Const` node is found (e.g. both args are `Var`).
#[allow(clippy::cast_ptr_alignment)]
unsafe fn extract_const_datum(args: *mut pg_sys::List) -> Option<(pg_sys::Datum, bool)> {
    if args.is_null() {
        return None;
    }
    // SAFETY: args is a valid List from the planner/executor.
    let len = unsafe { pg_sys::list_length(args) };
    for i in 0..len {
        // SAFETY: i is in [0, len).
        let node = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
        if node.is_null() {
            continue;
        }
        // SAFETY: reading the node tag.
        if unsafe { (*node).type_ } == pg_sys::NodeTag::T_Const {
            // SAFETY: tag confirmed Const; reading constvalue and constisnull.
            let cst = node.cast::<pg_sys::Const>();
            let datum = unsafe { (*cst).constvalue };
            let is_null = unsafe { (*cst).constisnull };
            return Some((datum, is_null));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Expression compiler: PG qual list → GPU bytecode
// ---------------------------------------------------------------------------

/// Compile a PG qual list into a `CompiledExpr` for GPU evaluation.
///
/// Walks the plan's `qual` (`List *` of expression nodes) and attempts to
/// produce either a template kernel (fastest) or general bytecode. Falls
/// back to `DeferToPg` for unsupported node types.
///
/// # Safety
///
/// `qual` must be a valid PG `List *` of expression nodes, or null.
/// Called on the main backend thread during `BeginCustomScan`.
unsafe fn compile_qual_list(
    qual: *mut pg_sys::List,
    num_cols: usize,
) -> crate::engine::expr_compiler::CompiledExpr {
    use crate::engine::expr_compiler::{self, CompiledExpr, ExprProgramBuilder};

    if qual.is_null() {
        return CompiledExpr::DeferToPg;
    }

    // SAFETY: qual is a valid List.
    let len = unsafe { pg_sys::list_length(qual) };
    if len == 0 {
        return CompiledExpr::DeferToPg;
    }

    // Try template match for single-clause quals first.
    if len == 1 {
        // SAFETY: qual has exactly 1 element.
        let node = unsafe { pg_sys::list_nth(qual, 0).cast::<pg_sys::Node>() };
        if let Some(tmpl) = unsafe { try_template_match(node) } {
            return CompiledExpr::Template(tmpl);
        }
    }

    // Try two-predicate AND template: col1 <cmp1> const1 AND col2 <cmp2> const2.
    // Handles both: single BoolExpr(AND) wrapping two OpExpr (len==1),
    // and PG's implicit AND with two separate OpExpr nodes (len==2).
    if len == 1 {
        let node = unsafe { pg_sys::list_nth(qual, 0).cast::<pg_sys::Node>() };
        if let Some(tmpl) = unsafe { try_two_pred_and_template(node) } {
            return CompiledExpr::Template(tmpl);
        }
    }
    if len == 2 {
        let n0 = unsafe { pg_sys::list_nth(qual, 0).cast::<pg_sys::Node>() };
        let n1 = unsafe { pg_sys::list_nth(qual, 1).cast::<pg_sys::Node>() };
        let t0 = unsafe { try_template_match(n0) };
        let t1 = unsafe { try_template_match(n1) };
        if let (
            Some(expr_compiler::TemplateKernel::CmpConst {
                col_idx: c1,
                cmp_opcode: op1,
                const_val: v1,
            }),
            Some(expr_compiler::TemplateKernel::CmpConst {
                col_idx: c2,
                cmp_opcode: op2,
                const_val: v2,
            }),
        ) = (t0, t1)
        {
            return CompiledExpr::Template(expr_compiler::TemplateKernel::TwoPredAnd {
                col1_idx: c1,
                cmp1_opcode: op1,
                const1_val: v1,
                col2_idx: c2,
                cmp2_opcode: op2,
                const2_val: v2,
            });
        }
    }

    // General bytecode compilation: walk all qual nodes as an implicit AND.
    let mut builder = ExprProgramBuilder::new(num_cols);
    let mut first = true;

    for i in 0..len {
        // SAFETY: i is in [0, len).
        let node = unsafe { pg_sys::list_nth(qual, i).cast::<pg_sys::Node>() };
        if !unsafe { compile_node(node, &mut builder) } {
            pgrx::debug1!("pg_accel: expr compile bail at qual node {i}");
            return CompiledExpr::DeferToPg;
        }
        if !first {
            // Implicit AND between qual list elements.
            builder.emit_binop(expr_compiler::opcode::AND);
        }
        first = false;
    }

    // Phase 2 dispatch re-enable: the SYCL kernel for
    // pgaccel_expr_eval_predicate (pgaccel-kernels/src/expr_eval.cpp)
    // is now wired through the executor's dispatch_gpu_expr path. The
    // LOAD_COL dense-index remap in expr_compiler::build() ensures the
    // kernel sees a batch indexed by `referenced_cols.len()` (matching
    // the executor's ColumnarBatchOwner shape).
    if let Some(program) = builder.build() {
        pgrx::debug1!(
            "pg_accel: compiled {} qual nodes → {} bytecode instructions, {} dense cols",
            len,
            program.instructions.len(),
            program.referenced_cols.len(),
        );
        return CompiledExpr::Bytecode(program);
    }
    CompiledExpr::DeferToPg
}

/// Try to match a single qual node as a template kernel.
///
/// Matches: `col <cmp> const`, `col BETWEEN lo AND hi`, `col IS [NOT] NULL`.
///
/// # Safety
///
/// `node` must be a valid PG `Node *`.
unsafe fn try_template_match(
    node: *mut pg_sys::Node,
) -> Option<crate::engine::expr_compiler::TemplateKernel> {
    use crate::engine::expr_compiler::{self, TemplateKernel};

    if node.is_null() {
        return None;
    }

    // SAFETY: node is valid; reading tag.
    let tag = unsafe { (*node).type_ };

    match tag {
        // col <cmp> const
        pg_sys::NodeTag::T_OpExpr => {
            let opexpr = node.cast::<pg_sys::OpExpr>();
            // SAFETY: tag confirmed OpExpr.
            let args = unsafe { (*opexpr).args };
            if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
                return None;
            }

            // SAFETY: resolve operator name from opno.
            let op_name_ptr = unsafe { pg_sys::get_opname((*opexpr).opno) };
            if op_name_ptr.is_null() {
                return None;
            }
            let op_name = unsafe { std::ffi::CStr::from_ptr(op_name_ptr) }
                .to_str()
                .ok()?;
            let cmp_opcode = expr_compiler::pg_cmp_op_to_opcode(op_name)?;

            // SAFETY: args has 2 elements.
            let arg0 = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
            let arg1 = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };

            // Pattern: Var <cmp> Const
            let (col_idx, const_val) = unsafe { extract_var_const_pair(arg0, arg1) }?;

            Some(TemplateKernel::CmpConst {
                col_idx,
                cmp_opcode,
                const_val,
            })
        }
        // col IS [NOT] NULL
        pg_sys::NodeTag::T_NullTest => {
            // SAFETY: tag confirmed NullTest.
            let nt = node.cast::<pg_sys::NullTest>();
            let arg = unsafe { (*nt).arg.cast::<pg_sys::Node>() };
            let col_idx = unsafe { node_as_var_col(arg) }?;
            let check_not_null = unsafe { (*nt).nulltesttype } == pg_sys::NullTestType::IS_NOT_NULL;
            Some(TemplateKernel::IsNull {
                col_idx,
                check_not_null,
            })
        }
        _ => None,
    }
}

/// Try to match a BoolExpr AND with exactly two CmpConst children.
///
/// # Safety
///
/// `node` must be a valid PG `Node *`.
unsafe fn try_two_pred_and_template(
    node: *mut pg_sys::Node,
) -> Option<crate::engine::expr_compiler::TemplateKernel> {
    use crate::engine::expr_compiler::TemplateKernel;

    if node.is_null() {
        return None;
    }
    // SAFETY: reading tag.
    if unsafe { (*node).type_ } != pg_sys::NodeTag::T_BoolExpr {
        return None;
    }
    let boolexpr = node.cast::<pg_sys::BoolExpr>();
    // SAFETY: tag confirmed BoolExpr.
    if unsafe { (*boolexpr).boolop } != pg_sys::BoolExprType::AND_EXPR {
        return None;
    }
    let args = unsafe { (*boolexpr).args };
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
        return None;
    }

    // SAFETY: args has 2 elements.
    let n0 = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
    let n1 = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };

    // Both children must be CmpConst-matchable OpExprs.
    let t0 = unsafe { try_template_match(n0) };
    let t1 = unsafe { try_template_match(n1) };

    match (t0, t1) {
        (
            Some(TemplateKernel::CmpConst {
                col_idx: c1,
                cmp_opcode: op1,
                const_val: v1,
            }),
            Some(TemplateKernel::CmpConst {
                col_idx: c2,
                cmp_opcode: op2,
                const_val: v2,
            }),
        ) => Some(TemplateKernel::TwoPredAnd {
            col1_idx: c1,
            cmp1_opcode: op1,
            const1_val: v1,
            col2_idx: c2,
            cmp2_opcode: op2,
            const2_val: v2,
        }),
        _ => None,
    }
}

/// Extract a (Var, Const) pair from two nodes, returning (col_idx, f64 value).
/// Handles both `Var <op> Const` and `Const <op> Var` orderings.
///
/// # Safety
///
/// Both nodes must be valid PG `Node *`.
unsafe fn extract_var_const_pair(
    arg0: *mut pg_sys::Node,
    arg1: *mut pg_sys::Node,
) -> Option<(u32, f64)> {
    // Try Var, Const order.
    if let Some(col) = unsafe { node_as_var_col(arg0) }
        && let Some(val) = unsafe { node_as_const_f64(arg1) }
    {
        return Some((col, val));
    }
    // Try Const, Var order.
    if let Some(col) = unsafe { node_as_var_col(arg1) }
        && let Some(val) = unsafe { node_as_const_f64(arg0) }
    {
        return Some((col, val));
    }
    None
}

/// If the node is a `Var`, return its 0-based column index.
/// Unwraps `RelabelType` wrappers.
///
/// # Safety
///
/// `node` must be a valid PG `Node *`.
unsafe fn node_as_var_col(node: *mut pg_sys::Node) -> Option<u32> {
    if node.is_null() {
        return None;
    }
    // SAFETY: reading tag.
    let tag = unsafe { (*node).type_ };
    match tag {
        pg_sys::NodeTag::T_Var => {
            // SAFETY: tag confirmed Var.
            let var = node.cast::<pg_sys::Var>();
            let attno = unsafe { (*var).varattno };
            if attno <= 0 {
                return None; // system column
            }
            Some((attno - 1) as u32) // 1-based → 0-based
        }
        pg_sys::NodeTag::T_RelabelType => {
            // SAFETY: tag confirmed RelabelType; unwrap and recurse.
            let arg = unsafe { (*node.cast::<pg_sys::RelabelType>()).arg };
            unsafe { node_as_var_col(arg.cast::<pg_sys::Node>()) }
        }
        _ => None,
    }
}

/// If the node is a `Const`, return its value as f64.
/// Handles int4, int8, float4, float8, int2.
///
/// # Safety
///
/// `node` must be a valid PG `Node *`.
unsafe fn node_as_const_f64(node: *mut pg_sys::Node) -> Option<f64> {
    if node.is_null() {
        return None;
    }
    // SAFETY: reading tag.
    if unsafe { (*node).type_ } != pg_sys::NodeTag::T_Const {
        return None;
    }
    let cst = node.cast::<pg_sys::Const>();
    // SAFETY: tag confirmed Const.
    if unsafe { (*cst).constisnull } {
        return None;
    }
    let datum = unsafe { (*cst).constvalue };
    let typid = u32::from(unsafe { (*cst).consttype });

    // PG type OIDs for numeric types.
    const INT2OID: u32 = 21;
    const INT4OID: u32 = 23;
    const INT8OID: u32 = 20;
    const FLOAT4OID: u32 = 700;
    const FLOAT8OID: u32 = 701;
    const BOOLOID: u32 = 16;

    match typid {
        INT2OID => Some(f64::from(datum.value() as i16)),
        INT4OID => Some(f64::from(datum.value() as i32)),
        INT8OID => Some((datum.value() as i64) as f64),
        FLOAT4OID => Some(f64::from(f32::from_bits(datum.value() as u32))),
        FLOAT8OID => Some(f64::from_bits(datum.value() as u64)),
        BOOLOID => Some(if datum.value() != 0 { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Compile a single PG expression node to bytecode via the builder.
/// Returns `false` if the node type is unsupported.
///
/// # Safety
///
/// `node` must be a valid PG `Node *`.
unsafe fn compile_node(
    node: *mut pg_sys::Node,
    builder: &mut crate::engine::expr_compiler::ExprProgramBuilder,
) -> bool {
    use crate::engine::expr_compiler::{self, opcode};

    if node.is_null() {
        return false;
    }

    // SAFETY: node is valid; reading tag.
    let tag = unsafe { (*node).type_ };

    match tag {
        pg_sys::NodeTag::T_Var => {
            let col = match unsafe { node_as_var_col(node) } {
                Some(c) => c,
                None => return false,
            };
            builder.emit_load_col(col);
            true
        }
        pg_sys::NodeTag::T_Const => {
            // SAFETY: tag confirmed Const.
            let cst = node.cast::<pg_sys::Const>();
            if unsafe { (*cst).constisnull } {
                builder.emit_load_null();
                return true;
            }
            match unsafe { node_as_const_f64(node) } {
                Some(val) => {
                    builder.emit_load_const(crate::gpu::PgaccelVal::from_f64(val));
                    true
                }
                None => false,
            }
        }
        pg_sys::NodeTag::T_OpExpr => {
            // SAFETY: tag confirmed OpExpr.
            let opexpr = node.cast::<pg_sys::OpExpr>();
            let args = unsafe { (*opexpr).args };
            if args.is_null() {
                return false;
            }
            let nargs = unsafe { pg_sys::list_length(args) };
            if nargs != 2 {
                return false;
            }

            // Resolve operator name.
            // SAFETY: opno is a valid operator OID.
            let op_name_ptr = unsafe { pg_sys::get_opname((*opexpr).opno) };
            if op_name_ptr.is_null() {
                return false;
            }
            let op_name = match unsafe { std::ffi::CStr::from_ptr(op_name_ptr) }.to_str() {
                Ok(s) => s,
                Err(_) => return false,
            };

            // Compile both arguments.
            // SAFETY: args has 2 elements.
            let arg0 = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
            let arg1 = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };

            if !unsafe { compile_node(arg0, builder) } {
                return false;
            }
            if !unsafe { compile_node(arg1, builder) } {
                return false;
            }

            // Try comparison first, then arithmetic.
            if let Some(cmp) = expr_compiler::pg_cmp_op_to_opcode(op_name) {
                builder.emit_binop(cmp);
                return true;
            }

            // Arithmetic — use f64 variants since all columns extracted as f64.
            match op_name {
                "+" => builder.emit_binop(opcode::ADD_F64),
                "-" => builder.emit_binop(opcode::SUB_F64),
                "*" => builder.emit_binop(opcode::MUL_F64),
                "/" => builder.emit_binop(opcode::DIV_F64),
                "%" => builder.emit_binop(opcode::MOD_I64),
                _ => return false,
            }
            true
        }
        pg_sys::NodeTag::T_FuncExpr => {
            // SAFETY: tag confirmed FuncExpr.
            let funcexpr = node.cast::<pg_sys::FuncExpr>();
            let funcid = unsafe { (*funcexpr).funcid };

            // Look up function name in pg_proc.
            // SAFETY: funcid is a valid regproc OID.
            let name_ptr = unsafe { pg_sys::get_func_name(funcid) };
            if name_ptr.is_null() {
                return false;
            }
            let func_name = match unsafe { std::ffi::CStr::from_ptr(name_ptr) }.to_str() {
                Ok(s) => s,
                Err(_) => return false,
            };

            // Check if it's a GPU-supported math function.
            let (math_opcode, is_binary) = match expr_compiler::math_func_opcode(func_name) {
                Some(pair) => pair,
                None => return false,
            };

            let args = unsafe { (*funcexpr).args };
            if args.is_null() {
                return false;
            }
            let nargs = unsafe { pg_sys::list_length(args) };

            if is_binary {
                if nargs != 2 {
                    return false;
                }
                let a0 = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
                let a1 = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
                if !unsafe { compile_node(a0, builder) } {
                    return false;
                }
                if !unsafe { compile_node(a1, builder) } {
                    return false;
                }
                builder.emit_binop(math_opcode);
            } else {
                if nargs != 1 {
                    return false;
                }
                let a0 = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
                if !unsafe { compile_node(a0, builder) } {
                    return false;
                }
                builder.emit_unaryop(math_opcode);
            }
            true
        }
        pg_sys::NodeTag::T_BoolExpr => {
            // SAFETY: tag confirmed BoolExpr.
            let boolexpr = node.cast::<pg_sys::BoolExpr>();
            let boolop = unsafe { (*boolexpr).boolop };
            let args = unsafe { (*boolexpr).args };
            if args.is_null() {
                return false;
            }
            let nargs = unsafe { pg_sys::list_length(args) };

            match boolop {
                pg_sys::BoolExprType::AND_EXPR | pg_sys::BoolExprType::OR_EXPR => {
                    if nargs < 2 {
                        return false;
                    }
                    let combine_op = if boolop == pg_sys::BoolExprType::AND_EXPR {
                        opcode::AND
                    } else {
                        opcode::OR
                    };

                    // Compile first arg.
                    let first = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
                    if !unsafe { compile_node(first, builder) } {
                        return false;
                    }
                    // Compile remaining args, emitting AND/OR between each pair.
                    for i in 1..nargs {
                        let child = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
                        if !unsafe { compile_node(child, builder) } {
                            return false;
                        }
                        builder.emit_binop(combine_op);
                    }
                    true
                }
                pg_sys::BoolExprType::NOT_EXPR => {
                    if nargs != 1 {
                        return false;
                    }
                    let child = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
                    if !unsafe { compile_node(child, builder) } {
                        return false;
                    }
                    builder.emit_unaryop(opcode::NOT);
                    true
                }
                _ => false,
            }
        }
        pg_sys::NodeTag::T_NullTest => {
            // SAFETY: tag confirmed NullTest.
            let nt = node.cast::<pg_sys::NullTest>();
            let arg = unsafe { (*nt).arg.cast::<pg_sys::Node>() };
            if !unsafe { compile_node(arg, builder) } {
                return false;
            }
            let nt_opcode = if unsafe { (*nt).nulltesttype } == pg_sys::NullTestType::IS_NOT_NULL {
                opcode::IS_NOT_NULL
            } else {
                opcode::IS_NULL
            };
            builder.emit_unaryop(nt_opcode);
            true
        }
        pg_sys::NodeTag::T_RelabelType => {
            // Unwrap type relabeling and compile the inner expression.
            // SAFETY: tag confirmed RelabelType.
            let arg = unsafe { (*node.cast::<pg_sys::RelabelType>()).arg };
            unsafe { compile_node(arg.cast::<pg_sys::Node>(), builder) }
        }
        _ => false,
    }
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
#[allow(clippy::too_many_lines)]
unsafe extern "C-unwind" fn begin_custom_scan(
    node: *mut pg_sys::CustomScanState,
    _estate: *mut pg_sys::EState,
    eflags: c_int,
) {
    let _span = tracing::debug_span!("ffi.begin_custom_scan").entered();
    // Record that a query was routed through the accelerated custom scan path.
    // This runs once per CustomScan node init on the main backend thread.
    stats::record_query_accelerated();

    // SAFETY: node points to our GpuAccelScanState (extended struct). The
    // plan node's custom_private was set by make_custom_scan_plan.
    let state = node.cast::<GpuAccelScanState>();
    // SAFETY: node is our extended GpuAccelScanState; ps.plan points to
    // the CustomScan plan node set by ExecInitCustomScan.
    let cscan = unsafe { (*node).ss.ps.plan.cast::<pg_sys::CustomScan>() };

    // When scanrelid > 0, PG17 does NOT initialise custom_ps from
    // custom_plans. We need to manually init child plan states so we
    // can pull tuples from the child Seq Scan.
    // SAFETY: node, cscan, and child plans originate from PG executor init.
    // ExecInitNode is called on the main backend thread with valid pointers.
    unsafe {
        if (*node).custom_ps.is_null() {
            let plans = (*cscan).custom_plans;
            if !plans.is_null() {
                let len = pg_sys::list_length(plans);
                for i in 0..len {
                    let child_plan = pg_sys::list_nth(plans, i).cast::<pg_sys::Plan>();
                    if !child_plan.is_null() {
                        let child_ps =
                            pg_sys::ExecInitNode(child_plan, (*node).ss.ps.state, eflags);
                        (*node).custom_ps = pg_sys::lappend((*node).custom_ps, child_ps.cast());
                    }
                }
            }
        }
    }

    // Deserialize strategy and config from custom_private.
    // Layout: [strategy, batch_size, expected_threads, fn_oid,
    //          target_attno, accel_strategy, ...sort keys if Sort].
    // Fall back to GUC values if custom_private is missing or malformed.
    // SAFETY: cscan is a valid CustomScan plan node; custom_private was
    // populated by make_custom_scan_plan with Integer nodes.
    let privdata = unsafe { deserialize_custom_private((*cscan).custom_private) };

    // SAFETY: state points to our GpuAccelScanState, allocated and zeroed
    // in create_custom_scan_state.
    unsafe {
        (*state).accel.strategy = privdata.gpu_strategy as c_int;
        (*state).accel.batch_size = privdata.batch_size.max(1);
        (*state).accel.expected_threads = resolve_thread_count();
    }

    let batch_size = if privdata.batch_size > 0 {
        privdata.batch_size as usize
    } else {
        256
    };

    // Allocate the appropriate Rust-side executor state based on strategy.
    //
    // The child plan (Seq Scan) already evaluates the WHERE clause, so
    // we do NOT steal the qual for re-evaluation in the custom scan.

    // SAFETY: node points to our extended GpuAccelScanState.
    unsafe {
        (*state).accel.executor = if privdata.gpu_strategy == GpuStrategy::PreAgg {
            // Deserialize PreAgg configuration from custom_private.
            let preagg_info = deserialize_preagg_private((*cscan).custom_private);

            let mut exec = Box::new(PreAggExecState::new(
                preagg_info.depths,
                preagg_info.agg_descs,
                preagg_info.group_keys,
                preagg_info.scan_expr,
            ));

            // Materialize dimension tables from child plan states.
            let custom_ps = (*node).custom_ps;
            if !custom_ps.is_null() {
                let n_children = pg_sys::list_length(custom_ps);
                let mut child_states: Vec<*mut pg_sys::PlanState> = Vec::new();
                for i in 0..n_children {
                    // SAFETY: custom_ps[i] is a valid PlanState.
                    let child = pg_sys::list_nth(custom_ps, i).cast::<pg_sys::PlanState>();
                    child_states.push(child);
                }
                // SAFETY: child_states are valid PlanState pointers.
                exec.materialize_dimensions(&child_states);
            }

            // Open the fact table for direct heap scan.
            //
            // Prefer the stable relation OID stored at planning time.
            // The range-table index (scan_relid) can be rewritten by
            // `set_plan_refs` for upper plans (scanrelid=0 spans a join),
            // making it an unsafe index into `estate->es_range_table` at
            // execution time. The OID is stable and always valid.
            //
            // When the OID is present we `table_open(oid, AccessShareLock)`
            // directly — `end_custom_scan` will `table_close` it. Otherwise,
            // if the RTE index is valid, fall back to `ExecOpenScanRelation`
            // (which the executor's own cleanup path will close).
            let estate = (*node).ss.ps.state;
            if preagg_info.scan_oid != pg_sys::InvalidOid {
                // SAFETY: OID is valid (resolved at plan time from an
                // RTE_RELATION entry). Main backend thread.
                let rel = pg_sys::table_open(
                    preagg_info.scan_oid,
                    pg_sys::AccessShareLock as pg_sys::LOCKMODE,
                );
                let snap = (*estate).es_snapshot;
                // SAFETY: rel and snap are valid; main backend thread.
                let sd = pg_sys::table_beginscan(rel, snap, 0, std::ptr::null_mut());
                exec.set_scan_desc(sd);
                exec.set_scan_rel(rel);
            } else if preagg_info.scan_relid > 0 {
                // Guard: the RTE must be a real heap relation. Non-relation
                // RTEs (function, VALUES, CTE, subquery, or entries
                // invalidated by setrefs) passed to ExecOpenScanRelation
                // raise ERROR → SIGABRT. If the relid is not a valid heap
                // relation, skip the manual self-scan; the executor will
                // consume rows via custom_ps / child plans instead (not a
                // CPU fallback — still GPU-executed).
                let rt_entry = pg_sys::exec_rt_fetch(preagg_info.scan_relid, estate);
                if !rt_entry.is_null()
                    && (*rt_entry).rtekind == pg_sys::RTEKind::RTE_RELATION
                    && (*rt_entry).relid != pg_sys::InvalidOid
                {
                    let rel = pg_sys::ExecOpenScanRelation(estate, preagg_info.scan_relid, eflags);
                    let snap = (*estate).es_snapshot;
                    // SAFETY: rel and snap are valid; main backend thread.
                    let sd = pg_sys::table_beginscan(rel, snap, 0, std::ptr::null_mut());
                    exec.set_scan_desc(sd);
                }
            }

            pgrx::debug1!(
                "pg_accel: begin_custom_scan: PreAgg, {} depths, {} aggs, {} group keys",
                exec.depths.len(),
                exec.agg_descs.len(),
                exec.group_keys.len(),
            );
            Box::into_raw(exec).cast()
        } else if privdata.gpu_strategy == GpuStrategy::Agg {
            let mut exec = if let Some(gk) = privdata.group_key {
                pgrx::debug1!(
                    "pg_accel: begin_custom_scan: GroupedAgg, {} aggs, group attno={}, partial={}",
                    privdata.agg_columns.len(),
                    gk.attno,
                    privdata.partial.is_some(),
                );
                Box::new(AggExecState::new_grouped(
                    AccelStrategy::GpuReduce,
                    batch_size,
                    &privdata.agg_columns,
                    gk,
                    privdata.group_key_tlist_pos,
                ))
            } else {
                pgrx::debug1!(
                    "pg_accel: begin_custom_scan: Agg strategy, {} columns, partial={}",
                    privdata.agg_columns.len(),
                    privdata.partial.is_some(),
                );
                Box::new(AggExecState::new_with_types(
                    AccelStrategy::GpuReduce,
                    batch_size,
                    &privdata.agg_columns,
                ))
            };
            // Build per-column emitters for partial (worker-side) paths.
            if let Some(ref spec) = privdata.partial {
                exec.partial_emitters = Some(build_partial_emitters(spec));
                exec.enable_full_stats_for_avg();
            }

            // Vectorized self-scan: if self_scan_relid is set, open a heap
            // scan on the base table and create a VectorizedScan. This
            // bypasses ExecProcNode entirely — the agg walks the heap
            // directly.
            if privdata.self_scan_relid > 0 {
                let estate = (*node).ss.ps.state;
                // Guard: the RTE must be a real heap relation. Non-relation
                // RTEs (function, VALUES, CTE, subquery) passed to
                // ExecOpenScanRelation raise ERROR → panic → SIGABRT.
                let rt_entry = pg_sys::exec_rt_fetch(privdata.self_scan_relid, estate);
                if !rt_entry.is_null()
                    && (*rt_entry).rtekind == pg_sys::RTEKind::RTE_RELATION
                    && (*rt_entry).relid != pg_sys::InvalidOid
                {
                    // SAFETY: estate is valid; self_scan_relid references a
                    // valid RTE_RELATION range table entry set by the planner.
                    let rel =
                        pg_sys::ExecOpenScanRelation(estate, privdata.self_scan_relid, eflags);
                    let snap = (*estate).es_snapshot;
                    // SAFETY: rel and snap are valid; main backend thread.
                    let sd = pg_sys::table_beginscan(rel, snap, 0, std::ptr::null_mut());
                    // SAFETY: sd is a valid, open TableScanDesc.
                    let vscan = VectorizedScan::new(sd);
                    exec.set_vscan(vscan);
                    pgrx::debug1!(
                        "pg_accel: begin_custom_scan: Agg self-scan on relid {}",
                        privdata.self_scan_relid,
                    );
                }
            }

            // Pipeline fusion: if the child is a GpuExpr scan with a direct
            // heap scan and a compiled template expression, the agg can walk
            // the heap itself and extract aggregate columns directly from
            // HeapTuples — skipping ExecProcNode, MinimalTuple copy, and
            // slot deformation entirely.
            if !exec.is_grouped() {
                let custom_ps = (*node).custom_ps;
                if !custom_ps.is_null() && pg_sys::list_length(custom_ps) > 0 {
                    // SAFETY: custom_ps[0] is a valid PlanState — the child
                    // Custom Scan node initialised by ExecInitNode.
                    let child_ps = pg_sys::list_nth(custom_ps, 0).cast::<pg_sys::PlanState>();
                    // Verify the child is actually a CustomScanState with our
                    // exec methods before casting to GpuAccelScanState. Without
                    // this check, a SeqScan child would be misinterpreted as
                    // our extended state, reading garbage memory.
                    if !child_ps.is_null()
                        && (*child_ps.cast::<pg_sys::Node>()).type_
                            == pg_sys::NodeTag::T_CustomScanState
                    {
                        let child_css = child_ps.cast::<pg_sys::CustomScanState>();
                        // SAFETY: child_css is a valid CustomScanState (tag
                        // verified above). Only proceed if it uses our exec
                        // methods vtable.
                        if (*child_css).methods == &raw const EXEC_METHODS.0 {
                            let child_state = child_css.cast::<GpuAccelScanState>();
                            let child_strategy =
                                GpuStrategy::from_i32((*child_state).accel.strategy);
                            if child_strategy == GpuStrategy::Scan {
                                let child_exec_ptr = (*child_state).accel.executor;
                                if !child_exec_ptr.is_null() {
                                    // SAFETY: child executor was allocated as
                                    // ScanExecState in the Scan branch of
                                    // begin_custom_scan.
                                    let child_scan = &*child_exec_ptr.cast::<ScanExecState>();
                                    let sd = child_scan.scan_desc();
                                    if !sd.is_null() && child_scan.has_template_expr() {
                                        let compiled = child_scan.compiled_expr();
                                        // Build attno map: child scan output position →
                                        // base table attno. The child's plan target list
                                        // entries are Vars referencing the base table.
                                        let mut attno_map = Vec::new();
                                        let child_plan = (*child_ps).plan;
                                        if !child_plan.is_null() {
                                            let tlist = (*child_plan).targetlist;
                                            if !tlist.is_null() {
                                                let n = pg_sys::list_length(tlist);
                                                for j in 0..n {
                                                    let tle = pg_sys::list_nth(tlist, j)
                                                        .cast::<pg_sys::TargetEntry>();
                                                    if !tle.is_null() {
                                                        let expr = (*tle).expr;
                                                        if !expr.is_null() {
                                                            let tag = (*expr
                                                                .cast::<pg_sys::Node>())
                                                            .type_;
                                                            if tag == pg_sys::NodeTag::T_Var {
                                                                let var =
                                                                    expr.cast::<pg_sys::Var>();
                                                                attno_map.push(i32::from(
                                                                    (*var).varattno,
                                                                ));
                                                                continue;
                                                            }
                                                        }
                                                    }
                                                    // Non-Var entry: use 1-based identity.
                                                    attno_map.push(j + 1);
                                                }
                                            }
                                        }
                                        pgrx::debug1!(
                                            "pg_accel: pipeline fusion attno_map={:?}",
                                            attno_map,
                                        );
                                        exec.set_fused_context(sd, compiled, attno_map);
                                        pgrx::debug1!(
                                            "pg_accel: begin_custom_scan: \
                                         pipeline fusion scan+agg activated",
                                        );
                                    }
                                }
                            }
                        } // methods check
                    }
                }
            }

            Box::into_raw(exec).cast()
        } else if privdata.accel_strategy == AccelStrategy::GpuHashJoin {
            // Hash join: create a JoinExecState with hash join context.
            let key_type = match privdata.hash_key_type {
                1 => PgaccelKeyType::Int64,
                2 => PgaccelKeyType::Float64,
                _ => PgaccelKeyType::Int32,
            };
            let mut exec = Box::new(JoinExecState::new(
                AccelStrategy::GpuHashJoin,
                batch_size,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ));
            exec.set_hash_join_context(
                privdata.target_attno, // outer_attno
                privdata.hash_inner_attno,
                key_type,
            );

            // Initialize tlist mapping and temp slots for combined output.
            let custom_ps = (*node).custom_ps;
            if !custom_ps.is_null() && pg_sys::list_length(custom_ps) >= 2 {
                let outer_ps = pg_sys::list_nth(custom_ps, 0).cast::<pg_sys::PlanState>();
                let inner_ps = pg_sys::list_nth(custom_ps, 1).cast::<pg_sys::PlanState>();
                exec.init_hash_join_slots(cscan, outer_ps, inner_ps);
            }

            pgrx::debug1!(
                "pg_accel: begin_custom_scan: GpuHashJoin outer_attno={}, inner_attno={}, key_type={}, tlist_map={}",
                privdata.target_attno,
                privdata.hash_inner_attno,
                privdata.hash_key_type,
                exec.tlist_map.len(),
            );
            Box::into_raw(exec).cast()
        } else if privdata.gpu_strategy == GpuStrategy::Window {
            pgrx::debug1!(
                "pg_accel: begin_custom_scan: Window strategy, {} specs, scan_relid={}",
                privdata.window_specs.len(),
                privdata.window_scan_relid
            );
            let mut exec = Box::new(WindowExecState::new(
                AccelStrategy::GpuWindow,
                batch_size,
                privdata.window_specs,
            ));

            // Open direct heap scan when scan_relid > 0 (vectorized path).
            if privdata.window_scan_relid > 0 {
                // SAFETY: estate is valid; scan_relid is a valid RT index.
                let estate = (*node).ss.ps.state;
                // Guard: the RTE must be a real heap relation. Non-relation
                // RTEs (function, VALUES, CTE, subquery, or entries
                // invalidated by setrefs) passed to ExecOpenScanRelation
                // raise ERROR → SIGABRT.
                let rt_entry = pg_sys::exec_rt_fetch(privdata.window_scan_relid, estate);
                if !rt_entry.is_null()
                    && (*rt_entry).rtekind == pg_sys::RTEKind::RTE_RELATION
                    && (*rt_entry).relid != pg_sys::InvalidOid
                {
                    // SAFETY: estate and scan_relid are valid; main backend thread.
                    let rel =
                        pg_sys::ExecOpenScanRelation(estate, privdata.window_scan_relid, eflags);
                    let snap = (*estate).es_snapshot;
                    // SAFETY: rel and snap are valid; main backend thread.
                    let sd = pg_sys::table_beginscan(rel, snap, 0, std::ptr::null_mut());
                    exec.set_scan_desc(sd);
                    pgrx::debug1!(
                        "pg_accel: begin_custom_scan: Window vectorized scan, relid={}",
                        privdata.window_scan_relid
                    );
                }
            }

            Box::into_raw(exec).cast()
        } else if privdata.gpu_strategy == GpuStrategy::Sort {
            let mut exec = Box::new(SortExecState::new(
                AccelStrategy::GpuSort,
                batch_size,
                privdata.sort_keys.clone(),
                privdata.sort_limit,
            ));

            // Wire VectorizedScan when self_scan_relid > 0 (direct heap scan).
            // SAFETY: All pointer dereferences below are valid because:
            // - node is a valid CustomScanState set by ExecInitCustomScan
            // - estate, exec_rt_fetch, ExecOpenScanRelation, table_beginscan
            //   are PG APIs called on the main backend thread
            // - rel->rd_att is a valid TupleDesc for the opened relation
            // The enclosing block is already unsafe.
            if privdata.self_scan_relid > 0 {
                let estate = (*node).ss.ps.state;
                // Guard: the RTE must be a real heap relation. Non-relation
                // RTEs (function, VALUES, CTE, subquery, or entries
                // invalidated by setrefs) passed to ExecOpenScanRelation
                // raise ERROR → SIGABRT. Reproduces with `count(*) FROM
                // (SELECT k FROM t ORDER BY k) sq` when the planner picks
                // GpuSort under the count subquery — the outer plan's RTE
                // doesn't survive into the inner scan's range table.
                let rt_entry = pg_sys::exec_rt_fetch(privdata.self_scan_relid, estate);
                if !rt_entry.is_null()
                    && (*rt_entry).rtekind == pg_sys::RTEKind::RTE_RELATION
                    && (*rt_entry).relid != pg_sys::InvalidOid
                {
                    let rel =
                        pg_sys::ExecOpenScanRelation(estate, privdata.self_scan_relid, eflags);
                    let snap = (*estate).es_snapshot;
                    let sd = pg_sys::table_beginscan(rel, snap, 0, std::ptr::null_mut());

                    // Determine sort key type for inline extraction.
                    let (key_attno, key_typid) = if privdata.sort_keys.is_empty() {
                        (0, 0)
                    } else {
                        let sk = &privdata.sort_keys[0];
                        let tupdesc = (*rel).rd_att;
                        let attno_idx = (sk.attno as usize).wrapping_sub(1);
                        if tupdesc.is_null() {
                            (0, 0)
                        } else {
                            let natts = (*tupdesc).natts as usize;
                            if attno_idx < natts {
                                let attr = &*(*tupdesc).attrs.as_ptr().add(attno_idx);
                                (i32::from(sk.attno), u32::from(attr.atttypid))
                            } else {
                                (0, 0)
                            }
                        }
                    };

                    // SAFETY: rel is a valid, open Relation.
                    let vscan = SortScan::new(sd, rel, key_attno, key_typid);
                    exec.set_vscan(vscan);
                    pgrx::debug1!(
                        "pg_accel: begin_custom_scan: Sort VectorizedScan, \
                         relid={}, key_attno={}, key_typid={}",
                        privdata.self_scan_relid,
                        key_attno,
                        key_typid,
                    );
                }
            }

            Box::into_raw(exec).cast()
        } else {
            // Use fn_oid + target_attno + accel_strategy from custom_private
            // (serialized by the planner hook). Fall back to qual discovery
            // only if the planner didn't serialize them (fn_oid == InvalidOid).
            // Always scan the qual tree to extract the constant datum
            // argument (e.g. the fixed geometry in ST_Contains(col, $const)).
            // The planner serializes fn_oid/attno/strategy into custom_private,
            // but qual_datum cannot be serialized — it must be extracted at
            // executor init time from the qual expressions.
            //
            // SAFETY: cscan is a valid CustomScan; plan.qual was set by
            // make_custom_scan_plan via extract_actual_clauses.
            // GpuExpr doesn't need qual tree discovery (no fn_oid or
            // qual_datum). Skip find_accel_context_in_qual which calls
            // set_opfuncid on OpExpr nodes and can crash for plain WHERE.
            let (strategy, fn_oid, target_attno, qual_datum) =
                if privdata.accel_strategy == AccelStrategy::GpuExpr {
                    (AccelStrategy::GpuExpr, pg_sys::Oid::INVALID, 0, None)
                } else {
                    let qual_ctx = find_accel_context_in_qual((*cscan).scan.plan.qual);
                    if privdata.fn_oid == pg_sys::Oid::INVALID {
                        // Fallback: discover everything from qual list.
                        qual_ctx.map_or(
                            (AccelStrategy::GpuSpatial, pg_sys::Oid::INVALID, 0, None),
                            |ctx| (ctx.strategy, ctx.fn_oid, ctx.target_attno, ctx.qual_datum),
                        )
                    } else {
                        // Use planner-serialized fn_oid/attno/strategy, but take
                        // qual_datum from the qual tree discovery.
                        let qd = qual_ctx.and_then(|ctx| ctx.qual_datum);
                        (
                            privdata.accel_strategy,
                            privdata.fn_oid,
                            privdata.target_attno,
                            qd,
                        )
                    }
                };

            pgrx::debug1!(
                "pg_accel: begin_custom_scan: accel strategy = {:?}",
                strategy
            );

            // Grab the compiled qual ExprState and ExprContext from PG's
            // CustomScanState so the scalar qual fallback works when the
            // GPU dispatch returns Deferred.
            let qual = (*node).ss.ps.qual;
            let econtext = (*node).ss.ps.ps_ExprContext;
            let mut exec = Box::new(ScanExecState::new(strategy, batch_size, qual, econtext));

            // For GpuExpr strategy, compile the qual list to GPU bytecode.
            // Template match is tried first (fastest), then bytecode,
            // then defer to PG if neither works.
            if strategy == AccelStrategy::GpuExpr {
                // SAFETY: cscan.scan.plan.qual is a valid List * set by
                // make_custom_scan_plan. tupdesc provides num_cols.
                let plan_qual = (*cscan).scan.plan.qual;
                // For direct scan (scanrelid > 0), use the scan slot's
                // TupleDesc (= table TupleDesc). For child-plan mode,
                // use the child's result type.
                let num_cols = {
                    let scan_desc = (*node).ss.ss_ScanTupleSlot;
                    if scan_desc.is_null() {
                        32
                    } else {
                        let desc = (*scan_desc).tts_tupleDescriptor;
                        if desc.is_null() {
                            32
                        } else {
                            (*desc).natts as usize
                        }
                    }
                };
                let compiled = compile_qual_list(plan_qual, num_cols);
                pgrx::debug1!(
                    "pg_accel: begin_custom_scan: GpuExpr compiled = {}, num_cols={}",
                    match &compiled {
                        crate::engine::expr_compiler::CompiledExpr::Template(_) => "Template",
                        crate::engine::expr_compiler::CompiledExpr::Bytecode(_) => "Bytecode",
                        crate::engine::expr_compiler::CompiledExpr::DeferToPg => "DeferToPg",
                    },
                    num_cols
                );
                exec.set_compiled_expr(compiled);

                // For GpuExpr with scanrelid > 0 (direct heap scan),
                // initialise a table scan so fill_batch can pull tuples.
                // NOTE: Do NOT clear ps.qual here. ExecScan evaluates the
                // qual after gpu_scan_access returns each tuple. Clearing
                // it would bypass WHERE clause evaluation entirely.
                let scanrelid = (*cscan).scan.scanrelid;
                if scanrelid > 0 {
                    let rel = (*node).ss.ss_currentRelation;
                    let estate = (*node).ss.ps.state;
                    let snapshot = (*estate).es_snapshot;
                    pgrx::debug1!(
                        "pg_accel: table_beginscan: rel={:?}, estate={:?}, snapshot={:?}",
                        rel,
                        estate,
                        snapshot,
                    );
                    // Use GetActiveSnapshot if es_snapshot is null.
                    let snap = if snapshot.is_null() {
                        pg_sys::GetActiveSnapshot()
                    } else {
                        snapshot
                    };
                    // SAFETY: rel was opened by ExecOpenScanRelation during
                    // ExecInitCustomScan. snap is a valid snapshot.
                    let sd = pg_sys::table_beginscan(rel, snap, 0, std::ptr::null_mut());
                    exec.set_scan_desc(sd);
                    pgrx::debug1!(
                        "pg_accel: begin_custom_scan: GpuExpr direct heap scan, scanrelid={}",
                        scanrelid
                    );
                }
            }

            // Wire the GPU context so dispatch can route to the correct
            // GPU kernel (spatial, H3, raster).
            if fn_oid != pg_sys::Oid::INVALID {
                // SAFETY: fn_oid was looked up in the registry by the
                // planner hook and is a valid regproc OID. We are on the
                // main backend thread (called by the executor).
                exec.set_gpu_context(fn_oid, target_attno, qual_datum);
                pgrx::debug1!(
                    "pg_accel: set_gpu_context fn_oid={}, attno={}, has_qual_datum={}",
                    u32::from(fn_oid),
                    target_attno,
                    qual_datum.is_some()
                );

                // Detect GiST index child for batched recheck.
                // When the child is a GiST IndexScan, bbox filtering
                // has already been done by the index — skip Layer 1.
                if strategy == AccelStrategy::GpuSpatial
                    && !(*node).custom_ps.is_null()
                    && pg_sys::list_length((*node).custom_ps) > 0
                {
                    let child_ps =
                        pg_sys::list_nth((*node).custom_ps, 0).cast::<pg_sys::PlanState>();
                    // SAFETY: child_ps was initialized above via
                    // ExecInitNode. detect_gist_child reads the node
                    // tag and index AM OID — safe on main backend thread.
                    exec.detect_gist_child(child_ps);
                }

                // Datum extraction is now done in fill_batch from the child's
                // slot directly, so no extraction slot is needed.
            }

            Box::into_raw(exec).cast()
        };
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
/// Access method for ExecScan: fetches the next HeapTuple from our scan
/// descriptor and stores it in the scan slot. ExecScan handles qual
/// evaluation and projection.
///
/// # Safety
///
/// Called by ExecScan on the main backend thread. `node` is a valid ScanState
/// embedded in our GpuAccelScanState.
unsafe extern "C-unwind" fn gpu_scan_access(
    node: *mut pg_sys::ScanState,
) -> *mut pg_sys::TupleTableSlot {
    let _span = tracing::debug_span!("ffi.gpu_scan_access").entered();
    // Upcast ScanState → GpuAccelScanState to reach our executor.
    let state = node.cast::<GpuAccelScanState>();
    let executor = unsafe { (*state).accel.executor };
    if executor.is_null() {
        let scan_slot = unsafe { (*node).ss_ScanTupleSlot };
        unsafe { pg_sys::ExecClearTuple(scan_slot) };
        return scan_slot;
    }
    let scan_state = unsafe { &mut *executor.cast::<ScanExecState>() };
    let scan_slot = unsafe { (*node).ss_ScanTupleSlot };
    // Always use gpu_scan_next which calls table_scan_getnextslot for
    // proper buffer pinning. ExecScan handles qual evaluation via the
    // plan's qual list (compiled to ExprState by ExecInitCustomScan).
    // SAFETY: scan_desc and scan_slot are valid; main backend thread.
    unsafe { scan_state.gpu_scan_next(scan_slot) }
}

/// Recheck method for ExecScan — always returns true (no recheck needed
/// since our scan returns actual heap tuples, not index pointers).
///
/// # Safety
///
/// Called by ExecScan on the main backend thread.
unsafe extern "C-unwind" fn gpu_scan_recheck(
    _node: *mut pg_sys::ScanState,
    _slot: *mut pg_sys::TupleTableSlot,
) -> bool {
    true
}

/// Called by the executor on the main backend thread.
unsafe extern "C-unwind" fn exec_custom_scan(
    node: *mut pg_sys::CustomScanState,
) -> *mut pg_sys::TupleTableSlot {
    let _span = tracing::debug_span!("ffi.exec_custom_scan").entered();
    let state = node.cast::<GpuAccelScanState>();

    // Fall through to passthrough when the extension is disabled.
    if !gucs::enabled() {
        // SAFETY: node is a valid CustomScanState on the main backend thread.
        return unsafe { passthrough_exec(node) };
    }

    // SAFETY: executor was allocated in begin_custom_scan.
    let executor = unsafe { (*state).accel.executor };
    if executor.is_null() {
        // SAFETY: node is a valid CustomScanState on the main backend thread.
        return unsafe { passthrough_exec(node) };
    }

    // SAFETY: state points to our GpuAccelScanState with valid accel fields.
    let gpu_strategy = GpuStrategy::from_i32(unsafe { (*state).accel.strategy });

    // GpuExpr direct heap scan (scanrelid > 0) bypasses the trait dispatch
    // and uses ExecScan for qual eval + scan→result slot projection.
    if gpu_strategy == GpuStrategy::Scan {
        // SAFETY: node->ss.ps.plan is a valid CustomScan plan node.
        let scanrelid = unsafe {
            let cscan = (*node).ss.ps.plan.cast::<pg_sys::CustomScan>();
            (*cscan).scan.scanrelid
        };
        if scanrelid > 0 {
            // SAFETY: node->ss is a valid ScanState; gpu_scan_access and
            // gpu_scan_recheck are our own callbacks on the main thread.
            return unsafe {
                pg_sys::ExecScan(
                    &raw mut (*node).ss,
                    Some(gpu_scan_access),
                    Some(gpu_scan_recheck),
                )
            };
        }
    }

    // Dispatch through the unified ExecutorState trait. Each state owns its
    // internal branching (has_vscan, is_fused, is_grouped, has_scan_desc)
    // rather than leaking those into the ffi layer.
    //
    // SAFETY: For each strategy, `executor` was Box::into_raw'd in
    // begin_custom_scan as the matching concrete state type. The cast
    // pattern mirrors how end_custom_scan reclaims the box.
    let result = unsafe {
        let dyn_state: &mut dyn crate::engine::executor::ExecutorState = match gpu_strategy {
            GpuStrategy::Scan => &mut *executor.cast::<ScanExecState>(),
            GpuStrategy::Agg => &mut *executor.cast::<AggExecState>(),
            GpuStrategy::PreAgg => &mut *executor.cast::<PreAggExecState>(),
            GpuStrategy::Sort => &mut *executor.cast::<SortExecState>(),
            GpuStrategy::Window => &mut *executor.cast::<WindowExecState>(),
            GpuStrategy::Join => &mut *executor.cast::<JoinExecState>(),
        };
        let slot = dyn_state.exec(node);
        // SAFETY: state points to our GpuAccelScanState; writing counters for
        // EXPLAIN ANALYZE on the main backend thread.
        (*state).accel.rows_dispatched = dyn_state.rows_dispatched();
        (*state).accel.batches_executed = dyn_state.batches_executed();
        (*state).accel.dispatch_time_us = dyn_state.dispatch_time_us();
        slot
    };

    // Apply ps_ProjInfo projection for Sort/Window, whose scan slot holds
    // the full child tuple (scanrelid=0, custom_scan_tlist broader than
    // plan.targetlist). ExecInitCustomScan builds pi_state for this map.
    if !result.is_null() && matches!(gpu_strategy, GpuStrategy::Sort | GpuStrategy::Window) {
        // SAFETY: node is a valid CustomScanState; proj_info may be null
        // when scan slot schema already matches output.
        let proj_info = unsafe { (*node).ss.ps.ps_ProjInfo };
        if !proj_info.is_null() {
            // SAFETY: proj_info is a valid ProjectionInfo set by PG.
            let econtext = unsafe { (*proj_info).pi_exprContext };
            let scan_slot = unsafe { (*node).ss.ss_ScanTupleSlot };
            // SAFETY: econtext is a valid ExprContext; scan_slot holds the
            // current tuple produced by the trait dispatch above.
            unsafe {
                (*econtext).ecxt_scantuple = scan_slot;
            }
            // Inline ExecProject body.
            // SAFETY: pi_state and pi_exprContext are valid; main thread.
            let result_slot = unsafe {
                let expr_state = &raw mut (*proj_info).pi_state;
                let mut is_null = false;
                pg_sys::ExecEvalExprSwitchContext(expr_state, econtext, &raw mut is_null);
                (*expr_state).resultslot
            };
            return result_slot;
        }
    }

    result
}

/// `EndCustomScan`: clean up child plan states and free executor.
///
/// # Safety
///
/// Called by the executor on the main backend thread.
unsafe extern "C-unwind" fn end_custom_scan(node: *mut pg_sys::CustomScanState) {
    let _span = tracing::debug_span!("ffi.end_custom_scan").entered();
    let state = node.cast::<GpuAccelScanState>();

    // SAFETY: Reclaim the Rust executor state to prevent leaks.
    // Must drop the correct type based on strategy. The executor pointer
    // was Box::into_raw'd in begin_custom_scan with the matching type.
    unsafe {
        if !(*state).accel.executor.is_null() {
            // Check if this is a hash join by reading accel_strategy from custom_private.
            let cscan = (*node).ss.ps.plan.cast::<pg_sys::CustomScan>();
            let priv_list = (*cscan).custom_private;
            let is_hash_join = if !priv_list.is_null() && pg_sys::list_length(priv_list) > 5 {
                AccelStrategy::from_i32(list_int_at(priv_list, 5)) == AccelStrategy::GpuHashJoin
            } else {
                false
            };

            if is_hash_join {
                // SAFETY: executor was Box::into_raw'd as JoinExecState.
                let _ = Box::from_raw((*state).accel.executor.cast::<JoinExecState>());
            } else {
                let gpu_strategy = GpuStrategy::from_i32((*state).accel.strategy);
                if gpu_strategy == GpuStrategy::PreAgg {
                    // SAFETY: executor was Box::into_raw'd as PreAggExecState.
                    let preagg = Box::from_raw((*state).accel.executor.cast::<PreAggExecState>());
                    // End direct heap scan if one was started.
                    if !preagg.scan_desc.is_null() {
                        // SAFETY: scan_desc was created by table_beginscan.
                        pg_sys::table_endscan(preagg.scan_desc);
                    }
                    // Close the relation if we opened it via `table_open`.
                    // When scan_rel is null the relation was acquired via
                    // `ExecOpenScanRelation`, and the executor framework
                    // closes it — we must not double-close.
                    if !preagg.scan_rel.is_null() {
                        // SAFETY: scan_rel was opened via table_open with
                        // AccessShareLock in begin_custom_scan; main backend
                        // thread.
                        pg_sys::table_close(
                            preagg.scan_rel,
                            pg_sys::AccessShareLock as pg_sys::LOCKMODE,
                        );
                    }
                    drop(preagg);
                } else if gpu_strategy == GpuStrategy::Agg {
                    // SAFETY: executor was Box::into_raw'd as AggExecState.
                    let agg = Box::from_raw((*state).accel.executor.cast::<AggExecState>());
                    // End the vectorized heap scan if one was started.
                    if agg.has_vscan() {
                        let sd = agg.vscan_scan_desc();
                        if !sd.is_null() {
                            // SAFETY: sd was created by table_beginscan in
                            // begin_custom_scan; main backend thread.
                            pg_sys::table_endscan(sd);
                        }
                    }
                    drop(agg);
                } else if gpu_strategy == GpuStrategy::Window {
                    // SAFETY: executor was Box::into_raw'd as WindowExecState.
                    let win = Box::from_raw((*state).accel.executor.cast::<WindowExecState>());
                    // End heap scan before dropping — table_endscan must be
                    // called before PG's ExecCloseScanRelation closes the rel.
                    if win.has_scan_desc() {
                        let sd = win.scan_desc();
                        if !sd.is_null() {
                            // SAFETY: sd was created by table_beginscan
                            // in begin_custom_scan; main backend thread.
                            pg_sys::table_endscan(sd);
                        }
                    }
                    drop(win);
                } else if gpu_strategy == GpuStrategy::Sort {
                    // SAFETY: executor was Box::into_raw'd as SortExecState.
                    let sort_exec = Box::from_raw((*state).accel.executor.cast::<SortExecState>());
                    // End VectorizedScan's heap scan before dropping.
                    // table_endscan must be called before the relation is
                    // closed by PG's ExecCloseScanRelation.
                    if let Some(vscan) = sort_exec.vscan_ref() {
                        let sd = vscan.scan_desc();
                        if !sd.is_null() {
                            // SAFETY: sd was created by table_beginscan
                            // in begin_custom_scan; main backend thread.
                            pg_sys::table_endscan(sd);
                        }
                    }
                    drop(sort_exec);
                } else {
                    // SAFETY: executor was Box::into_raw'd as ScanExecState.
                    let scan_exec = Box::from_raw((*state).accel.executor.cast::<ScanExecState>());
                    // End direct heap scan if one was started.
                    let sd = scan_exec.scan_desc();
                    if !sd.is_null() {
                        // SAFETY: sd was created by table_beginscan in
                        // begin_custom_scan; main backend thread.
                        pg_sys::table_endscan(sd);
                    }
                    drop(scan_exec);
                }
            }
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

/// Drop the old executor state and create a fresh one, preserving
/// strategy-specific descriptors (qual pointers, sort keys, etc.).
///
/// # Safety
///
/// Caller must ensure `state` points to a valid `GpuAccelScanState` on the
/// main backend thread. The executor pointer is replaced atomically.
unsafe fn reset_executor_state(state: *mut GpuAccelScanState) {
    // SAFETY: All field accesses are to our GpuAccelScanState. The executor
    // pointer was previously produced by Box::into_raw for the matching type.
    unsafe {
        let gpu_strategy = GpuStrategy::from_i32((*state).accel.strategy);
        let batch_size = if (*state).accel.batch_size > 0 {
            (*state).accel.batch_size as usize
        } else {
            256
        };

        if gpu_strategy == GpuStrategy::Agg {
            // Rescan: rebuild the executor from scratch but preserve agg
            // descriptors and group-key info. Partial emitters are thrown away
            // and not rebuilt here — rescan of a partial-agg path (worker-side
            // of a Gather) is not a path the planner exercises.
            let (old_descs, old_group_key) = if (*state).accel.executor.is_null() {
                (vec![], None)
            } else {
                // SAFETY: executor was Box::into_raw'd as AggExecState.
                let old = Box::from_raw((*state).accel.executor.cast::<AggExecState>());
                let descs = old.agg_descs();
                let gk = old.group_key_info().cloned();
                (descs, gk)
            };
            let exec = old_group_key.map_or_else(
                || {
                    Box::new(AggExecState::new_with_types(
                        AccelStrategy::GpuReduce,
                        batch_size,
                        &old_descs,
                    ))
                },
                |gk| {
                    Box::new(AggExecState::new_grouped(
                        AccelStrategy::GpuReduce,
                        batch_size,
                        &old_descs,
                        gk,
                        0, // rescan: use default position
                    ))
                },
            );
            (*state).accel.executor = Box::into_raw(exec).cast();
        } else if gpu_strategy == GpuStrategy::Sort {
            let sort_keys = if (*state).accel.executor.is_null() {
                vec![]
            } else {
                // SAFETY: executor was Box::into_raw'd as SortExecState.
                let old = Box::from_raw((*state).accel.executor.cast::<SortExecState>());
                old.sort_keys().to_vec()
            };
            let exec = Box::new(SortExecState::new(
                AccelStrategy::GpuSort,
                batch_size,
                sort_keys,
                None,
            ));
            (*state).accel.executor = Box::into_raw(exec).cast();
        } else if gpu_strategy == GpuStrategy::Window {
            let window_specs = if (*state).accel.executor.is_null() {
                vec![]
            } else {
                // SAFETY: executor was Box::into_raw'd as WindowExecState.
                let old = Box::from_raw((*state).accel.executor.cast::<WindowExecState>());
                old.specs().to_vec()
            };
            let exec = Box::new(WindowExecState::new(
                AccelStrategy::GpuWindow,
                batch_size,
                window_specs,
            ));
            (*state).accel.executor = Box::into_raw(exec).cast();
        } else if gpu_strategy == GpuStrategy::Join {
            // Hash join / spatial join: preserve key config, drop hash table.
            if !(*state).accel.executor.is_null() {
                // SAFETY: executor was Box::into_raw'd as JoinExecState.
                let mut old = Box::from_raw((*state).accel.executor.cast::<JoinExecState>());
                old.reset_for_rescan();
                (*state).accel.executor = Box::into_raw(old).cast();
            }
        } else {
            let (qual, econtext, old_strategy, old_fn_oid, old_attno, old_qual_datum) =
                if (*state).accel.executor.is_null() {
                    (
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        AccelStrategy::GpuSpatial,
                        pg_sys::InvalidOid,
                        0i32,
                        None,
                    )
                } else {
                    // SAFETY: executor was Box::into_raw'd as ScanExecState.
                    let old = Box::from_raw((*state).accel.executor.cast::<ScanExecState>());
                    (
                        old.qual_ptr(),
                        old.econtext_ptr(),
                        old.strategy(),
                        old.fn_oid(),
                        old.target_attno(),
                        old.qual_datum(),
                    )
                };
            let mut exec = Box::new(ScanExecState::new(old_strategy, batch_size, qual, econtext));
            if old_fn_oid != pg_sys::InvalidOid {
                // SAFETY: old_fn_oid was validated during the initial
                // set_gpu_context call. We are on the main backend thread.
                exec.set_gpu_context(old_fn_oid, old_attno, old_qual_datum);
            }
            (*state).accel.executor = Box::into_raw(exec).cast();
        }
    }
}

// ---------------------------------------------------------------------------
// Parallel-worker DSM callbacks — see `dsm.rs`.
// ---------------------------------------------------------------------------

/// # Safety
///
/// Called by the executor on the main backend thread.
unsafe extern "C-unwind" fn rescan_custom_scan(node: *mut pg_sys::CustomScanState) {
    let _span = tracing::debug_span!("ffi.rescan_custom_scan").entered();
    let state = node.cast::<GpuAccelScanState>();

    // SAFETY: All pointer accesses are to our GpuAccelScanState on the main
    // backend thread. See `reset_executor_state` for per-strategy details.
    unsafe {
        reset_executor_state(state);
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read an `Integer` node value from a PG `List` at position `idx`.
///
/// Returns `0` if the list is null, too short, or the node is null.
///
/// # Safety
///
/// `list` must be null or a valid PG `List` of `Integer` nodes.
pub(super) unsafe fn list_int_at(list: *mut pg_sys::List, idx: c_int) -> c_int {
    // SAFETY: list_length is safe on null (returns 0 in PG) but our
    // null guard is explicit for clarity.
    if list.is_null() || unsafe { pg_sys::list_length(list) } <= idx {
        return 0;
    }
    // SAFETY: list_nth returns the idx-th cell's pointer field.
    let node = unsafe { pg_sys::list_nth(list, idx) };
    if node.is_null() {
        return 0;
    }
    // SAFETY: node is a valid Integer (T_Integer) node. Access the
    // `ival` field directly — `intVal` is a C macro, not exported.
    unsafe { (*node.cast::<pg_sys::Integer>()).ival }
}

/// Serialize sort key descriptors into a PG `List` of `Integer` nodes,
/// appending to the base `[strategy, batch_size, expected_threads]`.
///
/// Appends: `[num_keys, attno1, sort_op1, collation1, nulls_first1, ...]`
///
/// # Safety
///
/// Must be called in a valid PG memory context.
unsafe fn serialize_sort_keys(
    mut list: *mut pg_sys::List,
    sort_keys: &[SortKeyDesc],
) -> *mut pg_sys::List {
    // SAFETY: makeInteger + lappend allocate in CurrentMemoryContext.
    unsafe {
        list = pg_sys::lappend(list, pg_sys::makeInteger(sort_keys.len() as c_int).cast());
        for key in sort_keys {
            list = pg_sys::lappend(list, pg_sys::makeInteger(c_int::from(key.attno)).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(u32::from(key.sort_op) as c_int).cast(),
            );
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(u32::from(key.collation) as c_int).cast(),
            );
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(c_int::from(key.nulls_first)).cast(),
            );
        }
    }
    list
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

#[cfg(feature = "pg_test")]
mod tests;
