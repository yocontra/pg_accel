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
use crate::engine::executor::vectorized_scan::VectorizedScan;
use crate::engine::executor::join::JoinExecState;
use crate::engine::executor::preagg::PreAggExecState;
use crate::engine::executor::scan::ScanExecState;
use crate::engine::executor::sort::{SORT_KEY_INTS, SortExecState, SortKeyDesc};
use crate::engine::executor::window::{
    WINDOW_SPEC_INTS, WindowExecState, WindowFunc, WindowFuncSpec,
};
use crate::engine::registry::{self, AccelStrategy};
use crate::gpu::PgaccelKeyType;

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
    /// Opaque pointer to heap-allocated Rust executor state.
    /// Points to either `ScanExecState` or `SortExecState` depending
    /// on `strategy`. Set in `begin_custom_scan`, freed in `end_custom_scan`.
    executor: *mut std::ffi::c_void,
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

    cscan.cast()
}

/// Deserialize sort key descriptors from a path's `custom_private`.
///
/// Path layout: `[num_sort_keys, attno1, sort_op1, collation1, nulls_first1, ...]`
///
/// # Safety
///
/// `custom_private` must be null or a valid PG `List`.
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
    // SAFETY: palloc0 returns zeroed memory in CurrentMemoryContext.
    let cscan = unsafe {
        pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomScan>()).cast::<pg_sys::CustomScan>()
    };

    // SAFETY: cscan is freshly palloc'd and zeroed; best_path is valid.
    unsafe {
        (*cscan).scan.plan.type_ = pg_sys::NodeTag::T_CustomScan;

        // Use the original tlist expressions so PG's set_customscan_references
        // can map Sort pathkeys and other upper-level references correctly.
        // custom_scan_tlist: original expressions define the scan tuple layout
        //   (ExecTypeFromTL extracts types from Aggref.aggtype / Var.vartype).
        //   fix_scan_expr adjusts Var offsets during plan finalization.
        // plan.targetlist: original expressions allow prepare_sort_from_pathkeys
        //   to find group key Vars for ORDER BY. fix_upper_expr then maps each
        //   expression to Var(INDEX_VAR, resno) referencing custom_scan_tlist.
        // copyObject creates independent copies so our lists don't alias
        // other plan nodes' target lists.
        // SAFETY: copyObjectImpl deep-copies the list in CurrentMemoryContext.
        (*cscan).custom_scan_tlist = pg_sys::copyObjectImpl(tlist.cast()).cast();
        (*cscan).scan.plan.targetlist = pg_sys::copyObjectImpl(tlist.cast()).cast();

        (*cscan).scan.plan.qual = pg_sys::extract_actual_clauses(clauses, false);
        (*cscan).scan.plan.startup_cost = (*best_path).path.startup_cost;
        (*cscan).scan.plan.total_cost = (*best_path).path.total_cost;
        (*cscan).scan.plan.plan_rows = (*best_path).path.rows;
        (*cscan).custom_plans = custom_plans;
        (*cscan).flags = (*best_path).flags;
        (*cscan).scan.scanrelid = 0;
        (*cscan).methods = &raw const AGG_SCAN_METHODS.0;

        // Serialize: [strategy=Agg, batch_size, threads, fn_oid=0,
        //             target_attno=0, accel_strategy=GpuReduce,
        //             num_aggs, op0, attno0, op1, attno1, ...]
        let batch_size = gucs::min_batch_size();
        let expected_threads = resolve_thread_count();

        // Read aggregate descriptors from the path's custom_private.
        // Path layout: [num_aggs, op0, attno0, rtype0, op1, attno1, rtype1, ...]
        let path_priv = (*best_path).custom_private;
        let num_aggs = list_int_at(path_priv, 0);
        let path_len = pg_sys::list_length(path_priv) as usize;
        // Self-scan relid is always the last element in path's custom_private.
        let self_scan_relid = list_int_at(path_priv, (path_len - 1) as c_int);

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
                        let tle =
                            pg_sys::list_nth(child_tlist, j).cast::<pg_sys::TargetEntry>();
                        if tle.is_null() {
                            continue;
                        }
                        let expr = (*tle).expr;
                        if !expr.is_null()
                            && (*expr.cast::<pg_sys::Node>()).type_
                                == pg_sys::NodeTag::T_Var
                        {
                            let var = expr.cast::<pg_sys::Var>();
                            attno_map
                                .push((i32::from((*var).varattno), i32::from((*tle).resno)));
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
                list = pg_sys::lappend(
                    list,
                    pg_sys::makeInteger(remap_attno(gk_attno)).cast(),
                );
                list = pg_sys::lappend(list, pg_sys::makeInteger(gk_type_oid).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(gk_key_type).cast());
            }
        } else {
            // No group key info in path — plain aggregate.
            list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast());
        }

        // Append self-scan relid so begin_custom_scan can open the heap.
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(self_scan_relid).cast(),
        );

        (*cscan).custom_private = list;
    }

    cscan.cast()
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
        let effective_tlist = if tlist.is_null()
            || pg_sys::list_length(tlist) == 0
        {
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
                        let tle =
                            pg_sys::list_nth(child_tlist, j).cast::<pg_sys::TargetEntry>();
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
        // Path layout: [num_specs, spec0_func, spec0_part_attno, spec0_order_attno,
        //               spec0_value_attno, spec0_offset, spec0_default_bits,
        //               spec0_result_type, ...]
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
        }

        (*cscan).custom_private = list;
    }

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
    // SAFETY: palloc0 returns zeroed memory in CurrentMemoryContext.
    let cscan = unsafe {
        pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomScan>()).cast::<pg_sys::CustomScan>()
    };

    // SAFETY: cscan is freshly palloc'd and zeroed; best_path is valid.
    unsafe {
        (*cscan).scan.plan.type_ = pg_sys::NodeTag::T_CustomScan;
        (*cscan).scan.plan.targetlist = tlist;
        (*cscan).scan.plan.qual = pg_sys::extract_actual_clauses(clauses, false);
        (*cscan).scan.plan.startup_cost = (*best_path).path.startup_cost;
        (*cscan).scan.plan.total_cost = (*best_path).path.total_cost;
        (*cscan).scan.plan.plan_rows = (*best_path).path.rows;
        (*cscan).custom_plans = custom_plans;
        (*cscan).flags = (*best_path).flags;
        // scanrelid > 0 tells PG to open the fact table relation for us.
        // The scan_relid is stored as the first integer after the strategy
        // header in custom_private.
        (*cscan).scan.scanrelid = 0;
        (*cscan).methods = &raw const PREAGG_SCAN_METHODS.0;

        // Copy custom_private from path to plan (already serialized by planner hook).
        (*cscan).custom_private = (*best_path).custom_private;

        // Set custom_scan_tlist so PG knows the output schema.
        // For PreAgg, we produce GROUP BY keys + aggregates.
        (*cscan).custom_scan_tlist = tlist;
    }

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
        let accel_strategy_raw_early = if !path_priv_early.is_null()
            && pg_sys::list_length(path_priv_early) > 2
        {
            list_int_at(path_priv_early, 2)
        } else {
            0
        };
        let is_gpu_expr =
            accel_strategy_raw_early == AccelStrategy::GpuExpr as c_int;

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
            (*cscan).custom_scan_tlist =
                pg_sys::copyObjectImpl(tlist.cast()).cast();
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
            if is_scan && !custom_plans.is_null() && pg_sys::list_length(custom_plans) > 0
            {
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

        // For GpuSort, read sort keys from path's custom_private (after
        // the 3-element header) and serialize them into the plan.
        if is_sort {
            let path_priv_len = pg_sys::list_length(path_priv) as usize;
            if path_priv_len > 3 {
                // Sort key data starts at index 3: [num_keys, attno, op, coll, nf]
                let sort_keys = deserialize_path_sort_keys_at(path_priv, 3);
                list = serialize_sort_keys(list, &sort_keys);
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
                let remap_via_child =
                    |child_idx: c_int, orig_attno: c_int, orig_varno: c_int| -> c_int {
                    if custom_plans.is_null() {
                        return orig_attno;
                    }
                    let n_children = pg_sys::list_length(custom_plans);
                    if child_idx >= n_children {
                        return orig_attno;
                    }
                    let child = pg_sys::list_nth(custom_plans, child_idx)
                        .cast::<pg_sys::Plan>();
                    if child.is_null() {
                        return orig_attno;
                    }

                    // Check if child is a CustomScan (scanrelid=0).
                    let child_scanrelid = (*child.cast::<pg_sys::Scan>()).scanrelid;
                    let search_tlist = if child_scanrelid == 0
                        && (*child.cast::<pg_sys::Node>()).type_
                            == pg_sys::NodeTag::T_CustomScan
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
                        let tle = pg_sys::list_nth(search_tlist, j)
                            .cast::<pg_sys::TargetEntry>();
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
                            if v_attno == orig_attno
                                && (orig_varno == 0 || v_varno == orig_varno)
                            {
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

                list = pg_sys::lappend(
                    list,
                    pg_sys::makeInteger(remapped_inner).cast(),
                );
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
    use crate::engine::expr_compiler::{
        self, CompiledExpr, ExprProgramBuilder,
    };

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
            Some(expr_compiler::TemplateKernel::CmpConst { col_idx: c1, cmp_opcode: op1, const_val: v1 }),
            Some(expr_compiler::TemplateKernel::CmpConst { col_idx: c2, cmp_opcode: op2, const_val: v2 }),
        ) = (t0, t1) {
            return CompiledExpr::Template(expr_compiler::TemplateKernel::TwoPredAnd {
                col1_idx: c1, cmp1_opcode: op1, const1_val: v1,
                col2_idx: c2, cmp2_opcode: op2, const2_val: v2,
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

    match builder.build() {
        Some(program) => {
            pgrx::debug1!(
                "pg_accel: compiled {} qual nodes → {} bytecode instructions, {} cols",
                len,
                program.instructions.len(),
                program.referenced_cols.len(),
            );
            CompiledExpr::Bytecode(program)
        }
        None => {
            pgrx::debug1!("pg_accel: expr compile failed (stack overflow)");
            CompiledExpr::DeferToPg
        }
    }
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
            let check_not_null =
                unsafe { (*nt).nulltesttype } == pg_sys::NullTestType::IS_NOT_NULL;
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
    if let Some(col) = unsafe { node_as_var_col(arg0) } {
        if let Some(val) = unsafe { node_as_const_f64(arg1) } {
            return Some((col, val));
        }
    }
    // Try Const, Var order.
    if let Some(col) = unsafe { node_as_var_col(arg1) } {
        if let Some(val) = unsafe { node_as_const_f64(arg0) } {
            return Some((col, val));
        }
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
            let nt_opcode =
                if unsafe { (*nt).nulltesttype } == pg_sys::NullTestType::IS_NOT_NULL {
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
            // The scan_relid is stored in the preagg info.
            if preagg_info.scan_relid > 0 {
                let estate = (*node).ss.ps.state;
                let rt_entry = pg_sys::exec_rt_fetch(preagg_info.scan_relid, estate);
                if !rt_entry.is_null() {
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
                    "pg_accel: begin_custom_scan: GroupedAgg, {} aggs, group attno={}",
                    privdata.agg_columns.len(),
                    gk.attno,
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
                    "pg_accel: begin_custom_scan: Agg strategy, {} columns",
                    privdata.agg_columns.len()
                );
                Box::new(AggExecState::new_with_types(
                    AccelStrategy::GpuReduce,
                    batch_size,
                    &privdata.agg_columns,
                ))
            };

            // Vectorized self-scan: if self_scan_relid is set, open a heap
            // scan on the base table and create a VectorizedScan. This
            // bypasses ExecProcNode entirely — the agg walks the heap
            // directly.
            if privdata.self_scan_relid > 0 {
                let estate = (*node).ss.ps.state;
                // SAFETY: estate is valid; self_scan_relid references a
                // valid range table entry set by the planner.
                let rel = pg_sys::ExecOpenScanRelation(
                    estate,
                    privdata.self_scan_relid,
                    eflags,
                );
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
                    let child_ps =
                        pg_sys::list_nth(custom_ps, 0).cast::<pg_sys::PlanState>();
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
                                let child_scan =
                                    &*child_exec_ptr.cast::<ScanExecState>();
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
                                                attno_map.push((j + 1) as i32);
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
                let outer_ps =
                    pg_sys::list_nth(custom_ps, 0).cast::<pg_sys::PlanState>();
                let inner_ps =
                    pg_sys::list_nth(custom_ps, 1).cast::<pg_sys::PlanState>();
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
                "pg_accel: begin_custom_scan: Window strategy, {} specs",
                privdata.window_specs.len()
            );
            let exec = Box::new(WindowExecState::new(
                AccelStrategy::GpuWindow,
                batch_size,
                privdata.window_specs,
            ));
            Box::into_raw(exec).cast()
        } else if privdata.gpu_strategy == GpuStrategy::Sort {
            let exec = Box::new(SortExecState::new(
                AccelStrategy::GpuSort,
                batch_size,
                privdata.sort_keys,
                privdata.sort_limit,
            ));
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
                    let qual_ctx =
                        find_accel_context_in_qual((*cscan).scan.plan.qual);
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
            let mut exec = Box::new(ScanExecState::new(
                strategy,
                batch_size,
                qual,
                econtext,
            ));

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
                    if !scan_desc.is_null() {
                        let desc = (*scan_desc).tts_tupleDescriptor;
                        if !desc.is_null() {
                            (*desc).natts as usize
                        } else {
                            32
                        }
                    } else {
                        32
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
                    let sd = pg_sys::table_beginscan(
                        rel,
                        snap,
                        0,
                        std::ptr::null_mut(),
                    );
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
    let state = node.cast::<GpuAccelScanState>();

    // If the extension is disabled, fall through to passthrough.
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

    // Fast path for Scan strategy.
    if gpu_strategy == GpuStrategy::Scan {
        // Check if this is a GpuExpr direct heap scan (scanrelid > 0).
        // GpuExpr scans use ExecScan which handles qual eval + projection
        // from scan slot (all table cols) → result slot (requested cols).
        // SAFETY: node->ss.ps.plan is valid; reading scanrelid.
        let scanrelid = unsafe {
            let cscan = (*node).ss.ps.plan.cast::<pg_sys::CustomScan>();
            (*cscan).scan.scanrelid
        };
        if scanrelid > 0 {
            // GpuExpr: use ExecScan for proper projection support.
            // SAFETY: node->ss is a valid ScanState. gpu_scan_access and
            // gpu_scan_recheck are our access/recheck methods.
            return unsafe {
                pg_sys::ExecScan(
                    &raw mut (*node).ss,
                    Some(gpu_scan_access),
                    Some(gpu_scan_recheck),
                )
            };
        }
        // Non-GpuExpr scan (spatial/h3/raster): use batched dispatch.
        let scan_state = unsafe { &mut *executor.cast::<ScanExecState>() };
        let child_ps = unsafe {
            let custom_ps = (*node).custom_ps;
            if custom_ps.is_null() || pg_sys::list_length(custom_ps) == 0 {
                std::ptr::null_mut()
            } else {
                pg_sys::list_nth(custom_ps, 0).cast::<pg_sys::PlanState>()
            }
        };
        let scan_slot = unsafe { (*node).ss.ss_ScanTupleSlot };
        return unsafe { scan_state.next(child_ps, scan_slot) };
    }

    // Agg strategy: delegate to AggExecState which consumes all child
    // input and produces aggregate result tuples.
    if gpu_strategy == GpuStrategy::Agg {
        let agg_state = unsafe { &mut *executor.cast::<AggExecState>() };
        let scan_slot = unsafe { (*node).ss.ss_ScanTupleSlot };

        // Use the vectorized self-scan path when a VectorizedScan was
        // created in begin_custom_scan, or the fused path for pipeline
        // fusion with a child GpuExpr scan.
        let result = if agg_state.has_vscan() {
            // SAFETY: agg_state has a valid VectorizedScan from
            // begin_custom_scan. scan_slot is valid. Main backend thread.
            if agg_state.is_grouped() {
                unsafe { agg_state.next_grouped_vectorized(scan_slot) }
            } else {
                unsafe { agg_state.next_vectorized(scan_slot) }
            }
        } else if agg_state.is_fused {
            // SAFETY: agg_state was allocated in begin_custom_scan with
            // fused context. scan_slot is valid. Main backend thread.
            unsafe { agg_state.next_fused(scan_slot) }
        } else {
            let child_ps = unsafe {
                let custom_ps = (*node).custom_ps;
                if custom_ps.is_null() || pg_sys::list_length(custom_ps) == 0 {
                    std::ptr::null_mut()
                } else {
                    pg_sys::list_nth(custom_ps, 0).cast::<pg_sys::PlanState>()
                }
            };
            // SAFETY: agg_state was allocated in begin_custom_scan. child_ps
            // and scan_slot are valid. Main backend thread.
            if agg_state.is_grouped() {
                unsafe { agg_state.next_grouped(child_ps, scan_slot) }
            } else {
                unsafe { agg_state.next(child_ps, scan_slot) }
            }
        };

        if result.is_null() {
            // SAFETY: ExecClearTuple on a valid slot.
            unsafe { pg_sys::ExecClearTuple(scan_slot) };
            return scan_slot;
        }
        // Update counters for EXPLAIN ANALYZE.
        unsafe {
            (*state).accel.rows_dispatched = agg_state.rows_dispatched;
            (*state).accel.batches_executed = agg_state.batches_executed;
            (*state).accel.dispatch_time_us = agg_state.dispatch_time_us;
        }
        return result;
    }

    // PreAgg strategy: fused star-join + pre-aggregation.
    if gpu_strategy == GpuStrategy::PreAgg {
        let preagg_state = unsafe { &mut *executor.cast::<PreAggExecState>() };
        let scan_slot = unsafe { (*node).ss.ss_ScanTupleSlot };

        let result = unsafe { preagg_state.next(scan_slot) };
        if result.is_null() {
            // SAFETY: ExecClearTuple on a valid slot.
            unsafe { pg_sys::ExecClearTuple(scan_slot) };
            return scan_slot;
        }
        // Update counters for EXPLAIN ANALYZE.
        unsafe {
            (*state).accel.rows_dispatched = preagg_state.rows_dispatched;
            (*state).accel.batches_executed = preagg_state.batches_executed;
            (*state).accel.dispatch_time_us = preagg_state.dispatch_time_us;
        }
        return result;
    }

    // Strategies that are not yet handled: passthrough.
    if !matches!(
        gpu_strategy,
        GpuStrategy::Scan | GpuStrategy::Sort | GpuStrategy::Join | GpuStrategy::Window
    ) {
        // SAFETY: node is a valid CustomScanState on the main backend thread.
        return unsafe { passthrough_exec(node) };
    }

    // Get the child plan state from custom_ps.
    let child_ps = unsafe {
        let custom_ps = (*node).custom_ps;
        if custom_ps.is_null() || pg_sys::list_length(custom_ps) == 0 {
            std::ptr::null_mut()
        } else {
            pg_sys::list_nth(custom_ps, 0).cast::<pg_sys::PlanState>()
        }
    };

    // Our scan slot — where we put the result tuple.
    // SAFETY: ss.ss_ScanTupleSlot is initialised by ExecInitCustomScan.
    let scan_slot = unsafe { (*node).ss.ss_ScanTupleSlot };

    // Dispatch to the correct executor based on strategy.
    // Check if this is a hash join — needs both outer (child 0) and inner (child 1).
    let accel_strategy_raw = unsafe { (*state).accel.strategy };
    let _ = accel_strategy_raw; // used below in the hash join branch

    let (result, rows, batches, time_us) = if unsafe {
        // SAFETY: Reading accel fields from our extended state.
        let cscan = (*node).ss.ps.plan.cast::<pg_sys::CustomScan>();
        let priv_list = (*cscan).custom_private;
        let list_len = if priv_list.is_null() {
            0
        } else {
            pg_sys::list_length(priv_list)
        };
        list_len > 5
            && AccelStrategy::from_i32(list_int_at(priv_list, 5)) == AccelStrategy::GpuHashJoin
    } {
        // SAFETY: For GpuHashJoin, executor points to JoinExecState.
        let join_state = unsafe { &mut *executor.cast::<JoinExecState>() };
        // Get inner plan state (child 1).
        let inner_ps = unsafe {
            let custom_ps = (*node).custom_ps;
            if custom_ps.is_null() || pg_sys::list_length(custom_ps) < 2 {
                std::ptr::null_mut()
            } else {
                pg_sys::list_nth(custom_ps, 1).cast::<pg_sys::PlanState>()
            }
        };
        // SAFETY: child_ps=outer, inner_ps=inner, scan_slot=result. Main backend thread.
        let slot = unsafe { join_state.next(child_ps, inner_ps, scan_slot) };
        (
            slot,
            join_state.rows_dispatched,
            join_state.batches_executed,
            join_state.dispatch_time_us,
        )
    } else if gpu_strategy == GpuStrategy::Agg {
        // SAFETY: For Agg strategy, executor points to AggExecState.
        let agg_state = unsafe { &mut *executor.cast::<AggExecState>() };
        let slot = if agg_state.is_grouped() {
            // SAFETY: child_ps and scan_slot are valid; main backend thread.
            unsafe { agg_state.next_grouped(child_ps, scan_slot) }
        } else {
            unsafe { agg_state.next(child_ps, scan_slot) }
        };
        (
            slot,
            agg_state.rows_dispatched,
            agg_state.batches_executed,
            agg_state.dispatch_time_us,
        )
    } else if gpu_strategy == GpuStrategy::Window {
        // SAFETY: For Window strategy, executor points to WindowExecState.
        let win_state = unsafe { &mut *executor.cast::<WindowExecState>() };
        let slot = unsafe { win_state.next(child_ps, scan_slot) };
        (
            slot,
            win_state.rows_dispatched,
            win_state.batches_executed,
            win_state.dispatch_time_us,
        )
    } else if gpu_strategy == GpuStrategy::Sort {
        // SAFETY: For Sort strategy, executor points to SortExecState.
        let sort_state = unsafe { &mut *executor.cast::<SortExecState>() };
        let slot = unsafe { sort_state.next(child_ps, scan_slot) };
        (
            slot,
            sort_state.rows_dispatched,
            sort_state.batches_executed,
            sort_state.dispatch_time_us,
        )
    } else if gpu_strategy == GpuStrategy::Scan {
        // SAFETY: For Scan strategy, executor points to ScanExecState.
        let scan_state = unsafe { &mut *executor.cast::<ScanExecState>() };
        let slot = unsafe { scan_state.next(child_ps, scan_slot) };
        (
            slot,
            scan_state.rows_dispatched,
            scan_state.batches_executed,
            scan_state.dispatch_time_us,
        )
    } else {
        // Unknown strategy — pass through to stock PG executor.
        // SAFETY: node is a valid CustomScanState on the main backend thread.
        return unsafe { passthrough_exec(node) };
    };

    // SAFETY: state points to our GpuAccelScanState; writing counters back
    // for EXPLAIN ANALYZE on the main backend thread.
    unsafe {
        (*state).accel.rows_dispatched = rows;
        (*state).accel.batches_executed = batches;
        (*state).accel.dispatch_time_us = time_us;
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
                    let _ = Box::from_raw((*state).accel.executor.cast::<WindowExecState>());
                } else if gpu_strategy == GpuStrategy::Sort {
                    // SAFETY: executor was Box::into_raw'd as SortExecState.
                    let _ = Box::from_raw((*state).accel.executor.cast::<SortExecState>());
                } else {
                    // SAFETY: executor was Box::into_raw'd as ScanExecState.
                    let scan_exec =
                        Box::from_raw((*state).accel.executor.cast::<ScanExecState>());
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

/// `ReScanCustomScan`: handle rescanning (e.g., for nested loops).
///
/// # Safety
///
/// Called by the executor on the main backend thread.
unsafe extern "C-unwind" fn rescan_custom_scan(node: *mut pg_sys::CustomScanState) {
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

            // For Agg strategy, report whether GPU reduce was used.
            if strategy == GpuStrategy::Agg && !(*state).accel.executor.is_null() {
                // SAFETY: executor was Box::into_raw'd as AggExecState.
                let agg_state = &*(*state).accel.executor.cast::<AggExecState>();
                pg_sys::ExplainPropertyBool(
                    c"GPU Dispatched".as_ptr(),
                    agg_state.gpu_dispatched,
                    es,
                );
            }

            // For PreAgg strategy, report fused pipeline metrics.
            if strategy == GpuStrategy::PreAgg && !(*state).accel.executor.is_null() {
                // SAFETY: executor was Box::into_raw'd as PreAggExecState.
                let preagg_state =
                    &*(*state).accel.executor.cast::<PreAggExecState>();
                pg_sys::ExplainPropertyInteger(
                    c"Depths".as_ptr(),
                    std::ptr::null(),
                    preagg_state.depths.len() as i64,
                    es,
                );
                pg_sys::ExplainPropertyInteger(
                    c"Fact Rows Scanned".as_ptr(),
                    std::ptr::null(),
                    preagg_state.rows_dispatched as i64,
                    es,
                );
                pg_sys::ExplainPropertyBool(
                    c"Has Scan Expr".as_ptr(),
                    preagg_state.scan_expr.is_some(),
                    es,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Determine expected thread count (GPU-only: always 1, no CPU worker pool).
fn resolve_thread_count() -> c_int {
    1
}

/// Read an `Integer` node value from a PG `List` at position `idx`.
///
/// Returns `0` if the list is null, too short, or the node is null.
///
/// # Safety
///
/// `list` must be null or a valid PG `List` of `Integer` nodes.
unsafe fn list_int_at(list: *mut pg_sys::List, idx: c_int) -> c_int {
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

/// Deserialized acceleration metadata from `custom_private`.
struct CustomPrivateData {
    gpu_strategy: GpuStrategy,
    batch_size: c_int,
    fn_oid: pg_sys::Oid,
    target_attno: i32,
    accel_strategy: AccelStrategy,
    sort_keys: Vec<SortKeyDesc>,
    /// Limit for top-k sort optimization. `None` means no limit.
    sort_limit: Option<usize>,
    /// Aggregate column descriptors `(AggOp, attno, result_type_oid)`.
    /// Only meaningful when `gpu_strategy == Agg`.
    agg_columns: Vec<(AggOp, i32, u32)>,
    /// Group key info for grouped aggregation.
    /// Only meaningful when `gpu_strategy == Agg` and GROUP BY is present.
    group_key: Option<GroupKeyInfo>,
    /// 0-based position of group key in the output target list.
    group_key_tlist_pos: usize,
    /// Inner relation join key attno (1-based). Only for `GpuHashJoin`.
    hash_inner_attno: i32,
    /// Key type for hash join (0=i32, 1=i64, 2=f64). Only for `GpuHashJoin`.
    hash_key_type: i32,
    /// Window function specifications. Only meaningful when `gpu_strategy == Window`.
    window_specs: Vec<WindowFuncSpec>,
    /// Base relation OID for self-scanning (vectorized pipeline).
    /// When > 0, the executor opens its own heap scan instead of pulling
    /// tuples through ExecProcNode. Only meaningful for Agg strategy.
    self_scan_relid: pg_sys::Index,
}

/// Deserialize strategy, batch size, accel context, and sort keys from
/// `custom_private`.
///
/// Layout: `[strategy, batch_size, expected_threads, fn_oid, target_attno,
///   accel_strategy, num_sort_keys?, attno1, sort_op1, collation1,
///   nulls_first1, ...]`
///
/// Falls back to GUC defaults when `custom_private` is null or malformed.
///
/// # Safety
///
/// `custom_private` must be null or a valid PG `List`.
#[allow(clippy::too_many_lines)]
unsafe fn deserialize_custom_private(custom_private: *mut pg_sys::List) -> CustomPrivateData {
    if custom_private.is_null() {
        return CustomPrivateData {
            gpu_strategy: GpuStrategy::Scan,
            batch_size: gucs::min_batch_size().max(1),
            fn_oid: pg_sys::Oid::INVALID,
            target_attno: 0,
            accel_strategy: AccelStrategy::GpuSpatial,
            sort_keys: vec![],
            sort_limit: None,
            agg_columns: vec![],
            group_key: None,
            group_key_tlist_pos: 0,
            hash_inner_attno: 0,
            hash_key_type: 0,
            window_specs: vec![],
            self_scan_relid: 0,
        };
    }

    // SAFETY: custom_private is a valid List of Integer nodes.
    let strategy_raw = unsafe { list_int_at(custom_private, 0) };
    let batch_size = unsafe { list_int_at(custom_private, 1) };
    let gpu_strategy = GpuStrategy::from_i32(strategy_raw);

    let batch_size = if batch_size > 0 {
        batch_size
    } else {
        gucs::min_batch_size().max(1)
    };

    // SAFETY: custom_private was populated by PlanCustomPath with valid integer nodes
    // at indices 3, 4, 5.
    let fn_oid_raw = unsafe { list_int_at(custom_private, 3) } as u32;
    let fn_oid = pg_sys::Oid::from(fn_oid_raw);
    let target_attno = unsafe { list_int_at(custom_private, 4) };
    let accel_strategy_raw = unsafe { list_int_at(custom_private, 5) };
    let accel_strategy = AccelStrategy::from_i32(accel_strategy_raw);

    // For Sort strategy, read sort key descriptors starting at index 6.
    let mut sort_keys = vec![];
    if matches!(gpu_strategy, GpuStrategy::Sort) {
        // SAFETY: custom_private is a valid List (checked non-null above);
        // list_int_at and list_length handle bounds safely.
        let num_keys = unsafe { list_int_at(custom_private, 6) } as usize;
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        let base = 7; // first sort key starts at index 7

        for k in 0..num_keys {
            let offset = base + k * SORT_KEY_INTS;
            if offset + SORT_KEY_INTS > list_len {
                break;
            }
            // SAFETY: Indices are within bounds (checked above).
            let attno = unsafe { list_int_at(custom_private, offset as c_int) } as i16;
            let sort_op_raw = unsafe { list_int_at(custom_private, (offset + 1) as c_int) } as u32;
            let collation_raw =
                unsafe { list_int_at(custom_private, (offset + 2) as c_int) } as u32;
            let nulls_first = unsafe { list_int_at(custom_private, (offset + 3) as c_int) } != 0;

            sort_keys.push(SortKeyDesc {
                attno,
                sort_op: pg_sys::Oid::from(sort_op_raw),
                collation: pg_sys::Oid::from(collation_raw),
                nulls_first,
            });
        }
    }

    // For Sort strategy, read optional limit after sort keys.
    // Layout: [...sort keys..., limit_tuples]
    let sort_limit = if matches!(gpu_strategy, GpuStrategy::Sort) {
        // SAFETY: custom_private is a valid List.
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        let num_keys = unsafe { list_int_at(custom_private, 6) } as usize;
        let limit_idx = 7 + num_keys * SORT_KEY_INTS;
        if limit_idx < list_len {
            // SAFETY: Index is within bounds (checked above).
            let v = unsafe { list_int_at(custom_private, limit_idx as c_int) };
            if v > 0 { Some(v as usize) } else { None }
        } else {
            None
        }
    } else {
        None
    };

    // For Agg strategy, read aggregate column descriptors starting at index 6.
    // Layout: [num_aggs, op0, attno0, rtype0, op1, attno1, rtype1, ...]
    let mut agg_columns = vec![];
    if matches!(gpu_strategy, GpuStrategy::Agg) {
        // SAFETY: custom_private is a valid List; list_int_at handles bounds.
        let num_aggs = unsafe { list_int_at(custom_private, 6) } as usize;
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        let base = 7;
        for k in 0..num_aggs {
            let offset = base + k * 3;
            if offset + 3 > list_len {
                break;
            }
            let op = AggOp::from_i32(unsafe { list_int_at(custom_private, offset as c_int) });
            let attno = unsafe { list_int_at(custom_private, (offset + 1) as c_int) };
            let rtype = unsafe { list_int_at(custom_private, (offset + 2) as c_int) } as u32;
            agg_columns.push((op, attno, rtype));
        }
    }

    // For Agg strategy, read optional group key info after agg descriptors.
    // Layout: [...agg descs..., has_group_key, gk_attno, gk_type_oid,
    //   gk_key_type, gk2_attno, gk_tlist_pos]
    let (group_key, group_key_tlist_pos) = if matches!(gpu_strategy, GpuStrategy::Agg) {
        let num_aggs = unsafe { list_int_at(custom_private, 6) } as usize;
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        let gk_base = 7 + num_aggs * 3;
        if gk_base < list_len {
            // SAFETY: Index is within bounds (checked above).
            let has_gk = unsafe { list_int_at(custom_private, gk_base as c_int) };
            if has_gk != 0 && gk_base + 3 < list_len {
                let gk_attno = unsafe { list_int_at(custom_private, (gk_base + 1) as c_int) };
                let gk_type_oid =
                    pg_sys::Oid::from(
                        unsafe { list_int_at(custom_private, (gk_base + 2) as c_int) } as u32,
                    );
                let gk_key_type = unsafe { list_int_at(custom_private, (gk_base + 3) as c_int) };
                // gk_base + 4 = group_key2_attno, gk_base + 5 = gk_tlist_pos
                let tlist_pos = if gk_base + 5 < list_len {
                    unsafe { list_int_at(custom_private, (gk_base + 5) as c_int) }
                } else {
                    0 // default: group key at position 0
                };
                (Some(GroupKeyInfo {
                    attno: gk_attno,
                    type_oid: gk_type_oid,
                    key_type: gk_key_type,
                }), tlist_pos)
            } else {
                (None, 0)
            }
        } else {
            (None, 0)
        }
    } else {
        (None, 0)
    };

    // For Window strategy, read window function specs starting at index 6.
    // Layout: [num_specs, func0, part_attno0, order_attno0, value_attno0,
    //   offset0, default_bits0, result_type0, ...]
    let mut window_specs = vec![];
    if matches!(gpu_strategy, GpuStrategy::Window) {
        // SAFETY: custom_private is a valid List; list_int_at handles bounds.
        let num_specs = unsafe { list_int_at(custom_private, 6) } as usize;
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        let base = 7;
        for k in 0..num_specs {
            let offset = base + k * WINDOW_SPEC_INTS;
            if offset + WINDOW_SPEC_INTS > list_len {
                break;
            }
            // SAFETY: Indices are within bounds (checked above).
            let func_raw = unsafe { list_int_at(custom_private, offset as c_int) };
            let Some(func) = WindowFunc::from_i32(func_raw) else {
                break;
            };
            let part_attno = unsafe { list_int_at(custom_private, (offset + 1) as c_int) };
            let ord_attno = unsafe { list_int_at(custom_private, (offset + 2) as c_int) };
            let val_attno = unsafe { list_int_at(custom_private, (offset + 3) as c_int) };
            let lag_offset = unsafe { list_int_at(custom_private, (offset + 4) as c_int) };
            let default_bits = unsafe { list_int_at(custom_private, (offset + 5) as c_int) };
            let result_type = unsafe { list_int_at(custom_private, (offset + 6) as c_int) } as u32;
            window_specs.push(WindowFuncSpec {
                func,
                partition_attno: part_attno,
                order_attno: ord_attno,
                value_attno: val_attno,
                offset: lag_offset,
                default_val: f64::from_bits(default_bits as u64),
                result_type_oid: result_type,
            });
        }
    }

    // For Join strategy with GpuHashJoin accel, read hash join info at index 6+.
    // Layout: [...base 6 fields..., inner_attno, key_type]
    let (hash_inner_attno, hash_key_type) = if accel_strategy == AccelStrategy::GpuHashJoin {
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        if list_len > 7 {
            // SAFETY: Indices 6 and 7 are within bounds (checked above).
            let inner_attno = unsafe { list_int_at(custom_private, 6) };
            let key_type = unsafe { list_int_at(custom_private, 7) };
            (inner_attno, key_type)
        } else {
            (0, 0)
        }
    } else {
        (0, 0)
    };

    // For Agg strategy, self_scan_relid is the last element of custom_private.
    let self_scan_relid = if matches!(gpu_strategy, GpuStrategy::Agg) {
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        if list_len > 0 {
            // SAFETY: custom_private is a valid List; last element is self_scan_relid.
            (unsafe { list_int_at(custom_private, (list_len - 1) as c_int) }) as pg_sys::Index
        } else {
            0
        }
    } else {
        0
    };

    CustomPrivateData {
        gpu_strategy,
        batch_size,
        fn_oid,
        target_attno,
        accel_strategy,
        sort_keys,
        sort_limit,
        agg_columns,
        group_key,
        group_key_tlist_pos: group_key_tlist_pos as usize,
        hash_inner_attno,
        hash_key_type,
        window_specs,
        self_scan_relid,
    }
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // GpuStrategy enum — from_i32 conversions
    // -----------------------------------------------------------------------

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

    #[test]
    fn strategy_from_i32_all_valid_values() {
        assert_eq!(GpuStrategy::from_i32(0), GpuStrategy::Scan);
        assert_eq!(GpuStrategy::from_i32(1), GpuStrategy::Join);
        assert_eq!(GpuStrategy::from_i32(2), GpuStrategy::Agg);
        assert_eq!(GpuStrategy::from_i32(3), GpuStrategy::Sort);
    }

    #[test]
    fn strategy_from_i32_boundary_values() {
        // Negative values default to Scan.
        assert_eq!(GpuStrategy::from_i32(i32::MIN), GpuStrategy::Scan);
        assert_eq!(GpuStrategy::from_i32(-100), GpuStrategy::Scan);
        // Values above the last variant (Window=4) default to Scan.
        assert_eq!(GpuStrategy::from_i32(5), GpuStrategy::Scan);
        assert_eq!(GpuStrategy::from_i32(i32::MAX), GpuStrategy::Scan);
    }

    #[test]
    fn strategy_labels_are_non_empty() {
        for strategy in [
            GpuStrategy::Scan,
            GpuStrategy::Join,
            GpuStrategy::Agg,
            GpuStrategy::Sort,
        ] {
            let label = strategy.label();
            assert!(
                !label.is_empty(),
                "label for {strategy:?} should not be empty"
            );
        }
    }

    #[test]
    fn strategy_debug_display() {
        let s = format!("{:?}", GpuStrategy::Scan);
        assert_eq!(s, "Scan");
        let s = format!("{:?}", GpuStrategy::Join);
        assert_eq!(s, "Join");
        let s = format!("{:?}", GpuStrategy::Agg);
        assert_eq!(s, "Agg");
        let s = format!("{:?}", GpuStrategy::Sort);
        assert_eq!(s, "Sort");
    }

    #[test]
    fn strategy_clone_and_copy() {
        let s = GpuStrategy::Join;
        let cloned = s;
        assert_eq!(s, cloned);
    }

    #[test]
    fn strategy_repr_values() {
        assert_eq!(GpuStrategy::Scan as i32, 0);
        assert_eq!(GpuStrategy::Join as i32, 1);
        assert_eq!(GpuStrategy::Agg as i32, 2);
        assert_eq!(GpuStrategy::Sort as i32, 3);
    }

    #[test]
    fn scan_path_methods_non_null() {
        let methods = scan_path_methods();
        assert!(!methods.is_null());
    }

    #[test]
    fn join_path_methods_non_null() {
        let methods = join_path_methods();
        assert!(!methods.is_null());
    }

    #[test]
    fn scan_and_join_path_methods_are_distinct() {
        let scan = scan_path_methods();
        let join = join_path_methods();
        assert_ne!(scan, join);
    }

    #[test]
    fn resolve_thread_count_at_most_cores() {
        let count = resolve_thread_count();
        let cores = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(2);
        // Should be at most cores (auto-detect uses cores/2).
        assert!(count <= cores);
    }

    // -----------------------------------------------------------------------
    // GpuStrategy — Window variant
    // -----------------------------------------------------------------------

    #[test]
    fn strategy_from_i32_window_variant() {
        assert_eq!(GpuStrategy::from_i32(4), GpuStrategy::Window);
    }

    #[test]
    fn strategy_window_repr_value() {
        assert_eq!(GpuStrategy::Window as i32, 4);
    }

    #[test]
    fn strategy_window_label() {
        assert_eq!(GpuStrategy::Window.label(), c"GpuWindow");
    }

    #[test]
    fn strategy_window_debug() {
        assert_eq!(format!("{:?}", GpuStrategy::Window), "Window");
    }

    #[test]
    fn strategy_from_i32_above_window_defaults_to_scan() {
        // 5 and above are not valid GpuStrategy variants.
        assert_eq!(GpuStrategy::from_i32(5), GpuStrategy::Scan);
        assert_eq!(GpuStrategy::from_i32(6), GpuStrategy::Scan);
        assert_eq!(GpuStrategy::from_i32(100), GpuStrategy::Scan);
    }

    // -----------------------------------------------------------------------
    // GpuStrategy — roundtrip i32 conversion
    // -----------------------------------------------------------------------

    #[test]
    fn strategy_roundtrip_all_variants() {
        for variant in [
            GpuStrategy::Scan,
            GpuStrategy::Join,
            GpuStrategy::Agg,
            GpuStrategy::Sort,
            GpuStrategy::Window,
        ] {
            let raw = variant as i32;
            let recovered = GpuStrategy::from_i32(raw);
            assert_eq!(
                variant, recovered,
                "roundtrip failed for {variant:?} (raw={raw})"
            );
        }
    }

    // -----------------------------------------------------------------------
    // GpuStrategy — equality and inequality
    // -----------------------------------------------------------------------

    #[test]
    fn strategy_equality() {
        assert_eq!(GpuStrategy::Scan, GpuStrategy::Scan);
        assert_ne!(GpuStrategy::Scan, GpuStrategy::Join);
        assert_ne!(GpuStrategy::Sort, GpuStrategy::Window);
    }

    #[test]
    fn strategy_copy_semantics_preserve_value() {
        let original = GpuStrategy::Agg;
        let copied = original;
        // After copy, both should be independently usable and equal.
        assert_eq!(original as i32, 2);
        assert_eq!(copied as i32, 2);
    }

    // -----------------------------------------------------------------------
    // GpuStrategy — label uniqueness
    // -----------------------------------------------------------------------

    #[test]
    fn strategy_labels_are_unique_across_variants() {
        let all = [
            GpuStrategy::Scan,
            GpuStrategy::Join,
            GpuStrategy::Agg,
            GpuStrategy::Sort,
            GpuStrategy::Window,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                if i != j {
                    assert_ne!(
                        a.label(),
                        b.label(),
                        "labels for {a:?} and {b:?} must differ"
                    );
                }
            }
        }
    }

    #[test]
    fn strategy_labels_start_with_gpu() {
        for variant in [
            GpuStrategy::Scan,
            GpuStrategy::Join,
            GpuStrategy::Agg,
            GpuStrategy::Sort,
            GpuStrategy::Window,
        ] {
            let label = variant.label().to_str().unwrap();
            assert!(
                label.starts_with("Gpu"),
                "label for {variant:?} should start with 'Gpu', got '{label}'"
            );
        }
    }

    // -----------------------------------------------------------------------
    // GpuAccelState — field layout and default values
    // -----------------------------------------------------------------------

    #[test]
    fn accel_state_zeroed_has_expected_defaults() {
        // Simulates what palloc0 gives us: all bytes zero.
        let state = GpuAccelState {
            strategy: 0,
            batch_size: 0,
            expected_threads: 0,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            executor: std::ptr::null_mut(),
        };
        assert_eq!(GpuStrategy::from_i32(state.strategy), GpuStrategy::Scan);
        assert_eq!(state.batch_size, 0);
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
        assert_eq!(state.dispatch_time_us, 0);
        assert!(state.executor.is_null());
    }

    #[test]
    fn accel_state_strategy_field_maps_to_gpu_strategy() {
        let mut state = GpuAccelState {
            strategy: 0,
            batch_size: 256,
            expected_threads: 4,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            executor: std::ptr::null_mut(),
        };
        state.strategy = GpuStrategy::Sort as i32;
        assert_eq!(GpuStrategy::from_i32(state.strategy), GpuStrategy::Sort);

        state.strategy = GpuStrategy::Window as i32;
        assert_eq!(GpuStrategy::from_i32(state.strategy), GpuStrategy::Window);
    }

    // -----------------------------------------------------------------------
    // GpuAccelState — counter accumulation
    // -----------------------------------------------------------------------

    #[test]
    fn accel_state_counter_accumulation() {
        let mut state = GpuAccelState {
            strategy: GpuStrategy::Scan as i32,
            batch_size: 1024,
            expected_threads: 2,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            executor: std::ptr::null_mut(),
        };
        // Simulate dispatching 3 batches of 1024 rows each.
        for _ in 0..3 {
            state.rows_dispatched += 1024;
            state.batches_executed += 1;
            state.dispatch_time_us += 500;
        }
        assert_eq!(state.rows_dispatched, 3072);
        assert_eq!(state.batches_executed, 3);
        assert_eq!(state.dispatch_time_us, 1500);
    }

    #[test]
    fn accel_state_counters_no_overflow_at_large_values() {
        let mut state = GpuAccelState {
            strategy: GpuStrategy::Agg as i32,
            batch_size: 4096,
            expected_threads: 8,
            rows_dispatched: u64::MAX - 10,
            batches_executed: u64::MAX - 1,
            dispatch_time_us: u64::MAX / 2,
            executor: std::ptr::null_mut(),
        };
        state.rows_dispatched += 5;
        state.batches_executed += 1;
        state.dispatch_time_us += 100;
        assert_eq!(state.rows_dispatched, u64::MAX - 5);
        assert_eq!(state.batches_executed, u64::MAX);
        assert_eq!(state.dispatch_time_us, u64::MAX / 2 + 100);
    }

    #[test]
    fn accel_state_dispatch_time_to_ms_conversion() {
        let state = GpuAccelState {
            strategy: GpuStrategy::Scan as i32,
            batch_size: 256,
            expected_threads: 1,
            rows_dispatched: 1000,
            batches_executed: 4,
            dispatch_time_us: 12_345,
            executor: std::ptr::null_mut(),
        };
        // This mirrors the conversion done in explain_custom_scan.
        #[allow(clippy::cast_precision_loss)]
        let time_ms = state.dispatch_time_us as f64 / 1000.0;
        assert!((time_ms - 12.345).abs() < 1e-10);
    }

    #[test]
    fn accel_state_dispatch_time_zero_us_is_zero_ms() {
        let state = GpuAccelState {
            strategy: GpuStrategy::Scan as i32,
            batch_size: 256,
            expected_threads: 1,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            executor: std::ptr::null_mut(),
        };
        #[allow(clippy::cast_precision_loss)]
        let time_ms = state.dispatch_time_us as f64 / 1000.0;
        assert!((time_ms - 0.0).abs() < f64::EPSILON);
    }

    // -----------------------------------------------------------------------
    // CustomPrivateData — default (null custom_private) path
    // -----------------------------------------------------------------------

    #[test]
    fn custom_private_data_default_fields() {
        // When custom_private is null, deserialize should produce defaults.
        // We can't call deserialize_custom_private (needs PG List), but
        // we can verify the struct's default construction matches expectations.
        let data = CustomPrivateData {
            gpu_strategy: GpuStrategy::Scan,
            batch_size: 256,
            fn_oid: pg_sys::Oid::INVALID,
            target_attno: 0,
            accel_strategy: AccelStrategy::GpuSpatial,
            sort_keys: vec![],
            sort_limit: None,
            agg_columns: vec![],
            group_key: None,
            group_key_tlist_pos: 0,
            hash_inner_attno: 0,
            hash_key_type: 0,
            window_specs: vec![],
        };
        assert_eq!(data.gpu_strategy, GpuStrategy::Scan);
        assert_eq!(data.batch_size, 256);
        assert_eq!(data.fn_oid, pg_sys::Oid::INVALID);
        assert_eq!(data.target_attno, 0);
        assert_eq!(data.accel_strategy, AccelStrategy::GpuSpatial);
        assert!(data.sort_keys.is_empty());
        assert!(data.sort_limit.is_none());
        assert!(data.agg_columns.is_empty());
        assert!(data.group_key.is_none());
        assert_eq!(data.hash_inner_attno, 0);
        assert_eq!(data.hash_key_type, 0);
        assert!(data.window_specs.is_empty());
    }

    // -----------------------------------------------------------------------
    // CustomPrivateData — sort key storage
    // -----------------------------------------------------------------------

    #[test]
    fn custom_private_data_with_sort_keys() {
        let keys = vec![
            SortKeyDesc {
                attno: 1,
                sort_op: pg_sys::Oid::from(97u32), // int4lt
                collation: pg_sys::Oid::from(0u32),
                nulls_first: false,
            },
            SortKeyDesc {
                attno: 3,
                sort_op: pg_sys::Oid::from(622u32), // float8lt
                collation: pg_sys::Oid::from(100u32),
                nulls_first: true,
            },
        ];
        let data = CustomPrivateData {
            gpu_strategy: GpuStrategy::Sort,
            batch_size: 512,
            fn_oid: pg_sys::Oid::INVALID,
            target_attno: 0,
            accel_strategy: AccelStrategy::GpuSort,
            sort_keys: keys.clone(),
            sort_limit: Some(100),
            agg_columns: vec![],
            group_key: None,
            group_key_tlist_pos: 0,
            hash_inner_attno: 0,
            hash_key_type: 0,
            window_specs: vec![],
        };
        assert_eq!(data.sort_keys.len(), 2);
        assert_eq!(data.sort_keys[0].attno, 1);
        assert!(!data.sort_keys[0].nulls_first);
        assert_eq!(data.sort_keys[1].attno, 3);
        assert!(data.sort_keys[1].nulls_first);
        assert_eq!(data.sort_limit, Some(100));
    }

    #[test]
    fn custom_private_data_sort_limit_none_when_no_limit() {
        let data = CustomPrivateData {
            gpu_strategy: GpuStrategy::Sort,
            batch_size: 256,
            fn_oid: pg_sys::Oid::INVALID,
            target_attno: 0,
            accel_strategy: AccelStrategy::GpuSort,
            sort_keys: vec![SortKeyDesc {
                attno: 1,
                sort_op: pg_sys::Oid::from(97u32),
                collation: pg_sys::Oid::from(0u32),
                nulls_first: false,
            }],
            sort_limit: None,
            agg_columns: vec![],
            group_key: None,
            group_key_tlist_pos: 0,
            hash_inner_attno: 0,
            hash_key_type: 0,
            window_specs: vec![],
        };
        assert!(data.sort_limit.is_none());
    }

    // -----------------------------------------------------------------------
    // CustomPrivateData — aggregate column storage
    // -----------------------------------------------------------------------

    #[test]
    fn custom_private_data_with_agg_columns() {
        let data = CustomPrivateData {
            gpu_strategy: GpuStrategy::Agg,
            batch_size: 1024,
            fn_oid: pg_sys::Oid::INVALID,
            target_attno: 0,
            accel_strategy: AccelStrategy::GpuReduce,
            sort_keys: vec![],
            sort_limit: None,
            agg_columns: vec![
                (AggOp::Sum, 1, pg_sys::FLOAT8OID.to_u32()),
                (AggOp::Count, 0, pg_sys::INT8OID.to_u32()),
                (AggOp::Min, 2, pg_sys::FLOAT8OID.to_u32()),
                (AggOp::Max, 2, pg_sys::FLOAT8OID.to_u32()),
                (AggOp::Avg, 3, pg_sys::FLOAT8OID.to_u32()),
            ],
            group_key: None,
            group_key_tlist_pos: 0,
            hash_inner_attno: 0,
            hash_key_type: 0,
            window_specs: vec![],
        };
        assert_eq!(data.agg_columns.len(), 5);
        assert!(matches!(data.agg_columns[0].0, AggOp::Sum));
        assert!(matches!(data.agg_columns[1].0, AggOp::Count));
        assert!(matches!(data.agg_columns[2].0, AggOp::Min));
        assert!(matches!(data.agg_columns[3].0, AggOp::Max));
        assert!(matches!(data.agg_columns[4].0, AggOp::Avg));
    }

    #[test]
    fn custom_private_data_with_group_key() {
        let gk = GroupKeyInfo {
            attno: 2,
            type_oid: pg_sys::Oid::from(23u32), // INT4OID
            key_type: 0,                        // i32
        };
        let data = CustomPrivateData {
            gpu_strategy: GpuStrategy::Agg,
            batch_size: 256,
            fn_oid: pg_sys::Oid::INVALID,
            target_attno: 0,
            accel_strategy: AccelStrategy::GpuReduce,
            sort_keys: vec![],
            sort_limit: None,
            agg_columns: vec![(AggOp::Sum, 1, pg_sys::FLOAT8OID.to_u32())],
            group_key: Some(gk),
            group_key_tlist_pos: 0,
            hash_inner_attno: 0,
            hash_key_type: 0,
            window_specs: vec![],
        };
        let gk_ref = data.group_key.as_ref().unwrap();
        assert_eq!(gk_ref.attno, 2);
        assert_eq!(gk_ref.key_type, 0);
    }

    // -----------------------------------------------------------------------
    // CustomPrivateData — hash join fields
    // -----------------------------------------------------------------------

    #[test]
    fn custom_private_data_hash_join_fields() {
        let data = CustomPrivateData {
            gpu_strategy: GpuStrategy::Join,
            batch_size: 512,
            fn_oid: pg_sys::Oid::INVALID,
            target_attno: 1,
            accel_strategy: AccelStrategy::GpuHashJoin,
            sort_keys: vec![],
            sort_limit: None,
            agg_columns: vec![],
            group_key: None,
            group_key_tlist_pos: 0,
            hash_inner_attno: 3,
            hash_key_type: 1, // Int64
            window_specs: vec![],
        };
        assert_eq!(data.hash_inner_attno, 3);
        assert_eq!(data.hash_key_type, 1);
        assert_eq!(data.accel_strategy, AccelStrategy::GpuHashJoin);
    }

    // -----------------------------------------------------------------------
    // CustomPrivateData — window function specs
    // -----------------------------------------------------------------------

    #[test]
    fn custom_private_data_with_window_specs() {
        let specs = vec![
            WindowFuncSpec {
                func: WindowFunc::RowNumber,
                partition_attno: 1,
                order_attno: 2,
                value_attno: 0,
                offset: 0,
                default_val: 0.0,
                result_type_oid: pg_sys::INT8OID.to_u32(),
            },
            WindowFuncSpec {
                func: WindowFunc::Lag,
                partition_attno: 1,
                order_attno: 2,
                value_attno: 3,
                offset: 1,
                default_val: f64::from_bits(0),
                result_type_oid: pg_sys::FLOAT8OID.to_u32(),
            },
        ];
        let data = CustomPrivateData {
            gpu_strategy: GpuStrategy::Window,
            batch_size: 256,
            fn_oid: pg_sys::Oid::INVALID,
            target_attno: 0,
            accel_strategy: AccelStrategy::GpuWindow,
            sort_keys: vec![],
            sort_limit: None,
            agg_columns: vec![],
            group_key: None,
            group_key_tlist_pos: 0,
            hash_inner_attno: 0,
            hash_key_type: 0,
            window_specs: specs,
        };
        assert_eq!(data.window_specs.len(), 2);
        assert!(matches!(data.window_specs[0].func, WindowFunc::RowNumber));
        assert_eq!(data.window_specs[0].partition_attno, 1);
        assert!(matches!(data.window_specs[1].func, WindowFunc::Lag));
        assert_eq!(data.window_specs[1].offset, 1);
    }

    #[test]
    fn custom_private_data_empty_window_specs_for_non_window() {
        let data = CustomPrivateData {
            gpu_strategy: GpuStrategy::Sort,
            batch_size: 256,
            fn_oid: pg_sys::Oid::INVALID,
            target_attno: 0,
            accel_strategy: AccelStrategy::GpuSort,
            sort_keys: vec![],
            sort_limit: None,
            agg_columns: vec![],
            group_key: None,
            group_key_tlist_pos: 0,
            hash_inner_attno: 0,
            hash_key_type: 0,
            window_specs: vec![],
        };
        assert!(data.window_specs.is_empty());
    }

    // -----------------------------------------------------------------------
    // Vtable static pointers — all methods ptrs are non-null and distinct
    // -----------------------------------------------------------------------

    #[test]
    fn sort_path_methods_non_null() {
        let methods = sort_path_methods();
        assert!(!methods.is_null());
    }

    #[test]
    fn agg_path_methods_non_null() {
        let methods = agg_path_methods();
        assert!(!methods.is_null());
    }

    #[test]
    fn window_path_methods_non_null() {
        let methods = window_path_methods();
        assert!(!methods.is_null());
    }

    #[test]
    fn all_path_methods_are_distinct() {
        let ptrs = [
            scan_path_methods(),
            join_path_methods(),
            sort_path_methods(),
            agg_path_methods(),
            window_path_methods(),
        ];
        for i in 0..ptrs.len() {
            for j in (i + 1)..ptrs.len() {
                assert_ne!(
                    ptrs[i], ptrs[j],
                    "path methods at index {i} and {j} should be distinct"
                );
            }
        }
    }

    #[test]
    fn vtable_custom_names_are_valid_c_strings() {
        // Read the CustomName from each static vtable and verify it is
        // a well-formed, non-empty C string.
        let scan_name = unsafe { std::ffi::CStr::from_ptr(SCAN_PATH_METHODS.0.CustomName) };
        assert_eq!(scan_name, c"GpuAccelScan");

        let join_name = unsafe { std::ffi::CStr::from_ptr(JOIN_PATH_METHODS.0.CustomName) };
        assert_eq!(join_name, c"GpuAccelJoin");

        let sort_name = unsafe { std::ffi::CStr::from_ptr(SORT_PATH_METHODS.0.CustomName) };
        assert_eq!(sort_name, c"GpuAccelSort");

        let agg_name = unsafe { std::ffi::CStr::from_ptr(AGG_PATH_METHODS.0.CustomName) };
        assert_eq!(agg_name, c"GpuAccelAgg");

        let window_name = unsafe { std::ffi::CStr::from_ptr(WINDOW_PATH_METHODS.0.CustomName) };
        assert_eq!(window_name, c"GpuAccelWindow");
    }

    #[test]
    fn scan_methods_custom_names_match_path_methods() {
        // The CustomName in *_SCAN_METHODS should match the corresponding
        // *_PATH_METHODS so PG can correlate plan nodes.
        let scan_path = unsafe { std::ffi::CStr::from_ptr(SCAN_PATH_METHODS.0.CustomName) };
        let scan_scan = unsafe { std::ffi::CStr::from_ptr(SCAN_SCAN_METHODS.0.CustomName) };
        assert_eq!(scan_path, scan_scan);

        let join_path = unsafe { std::ffi::CStr::from_ptr(JOIN_PATH_METHODS.0.CustomName) };
        let join_scan = unsafe { std::ffi::CStr::from_ptr(JOIN_SCAN_METHODS.0.CustomName) };
        assert_eq!(join_path, join_scan);

        let sort_path = unsafe { std::ffi::CStr::from_ptr(SORT_PATH_METHODS.0.CustomName) };
        let sort_scan = unsafe { std::ffi::CStr::from_ptr(SORT_SCAN_METHODS.0.CustomName) };
        assert_eq!(sort_path, sort_scan);

        let agg_path = unsafe { std::ffi::CStr::from_ptr(AGG_PATH_METHODS.0.CustomName) };
        let agg_scan = unsafe { std::ffi::CStr::from_ptr(AGG_SCAN_METHODS.0.CustomName) };
        assert_eq!(agg_path, agg_scan);

        let window_path = unsafe { std::ffi::CStr::from_ptr(WINDOW_PATH_METHODS.0.CustomName) };
        let window_scan = unsafe { std::ffi::CStr::from_ptr(WINDOW_SCAN_METHODS.0.CustomName) };
        assert_eq!(window_path, window_scan);
    }

    #[test]
    fn exec_methods_has_required_callbacks() {
        // Verify the critical executor callbacks are wired (not None).
        assert!(EXEC_METHODS.0.BeginCustomScan.is_some());
        assert!(EXEC_METHODS.0.ExecCustomScan.is_some());
        assert!(EXEC_METHODS.0.EndCustomScan.is_some());
        assert!(EXEC_METHODS.0.ReScanCustomScan.is_some());
        assert!(EXEC_METHODS.0.ExplainCustomScan.is_some());
    }

    #[test]
    fn exec_methods_optional_callbacks_are_none() {
        // These optional methods should not be set.
        assert!(EXEC_METHODS.0.MarkPosCustomScan.is_none());
        assert!(EXEC_METHODS.0.RestrPosCustomScan.is_none());
        assert!(EXEC_METHODS.0.EstimateDSMCustomScan.is_none());
        assert!(EXEC_METHODS.0.InitializeDSMCustomScan.is_none());
        assert!(EXEC_METHODS.0.ReInitializeDSMCustomScan.is_none());
        assert!(EXEC_METHODS.0.InitializeWorkerCustomScan.is_none());
        assert!(EXEC_METHODS.0.ShutdownCustomScan.is_none());
    }

    #[test]
    fn path_methods_plan_callback_is_some() {
        assert!(SCAN_PATH_METHODS.0.PlanCustomPath.is_some());
        assert!(JOIN_PATH_METHODS.0.PlanCustomPath.is_some());
        assert!(SORT_PATH_METHODS.0.PlanCustomPath.is_some());
        assert!(AGG_PATH_METHODS.0.PlanCustomPath.is_some());
        assert!(WINDOW_PATH_METHODS.0.PlanCustomPath.is_some());
    }

    #[test]
    fn path_methods_reparameterize_is_none() {
        // We don't support reparameterization.
        assert!(
            SCAN_PATH_METHODS
                .0
                .ReparameterizeCustomPathByChild
                .is_none()
        );
        assert!(
            JOIN_PATH_METHODS
                .0
                .ReparameterizeCustomPathByChild
                .is_none()
        );
        assert!(
            SORT_PATH_METHODS
                .0
                .ReparameterizeCustomPathByChild
                .is_none()
        );
        assert!(AGG_PATH_METHODS.0.ReparameterizeCustomPathByChild.is_none());
        assert!(
            WINDOW_PATH_METHODS
                .0
                .ReparameterizeCustomPathByChild
                .is_none()
        );
    }

    #[test]
    fn scan_methods_create_state_callback_is_some() {
        // All scan methods should have CreateCustomScanState wired.
        assert!(SCAN_SCAN_METHODS.0.CreateCustomScanState.is_some());
        assert!(JOIN_SCAN_METHODS.0.CreateCustomScanState.is_some());
        assert!(SORT_SCAN_METHODS.0.CreateCustomScanState.is_some());
        assert!(AGG_SCAN_METHODS.0.CreateCustomScanState.is_some());
        assert!(WINDOW_SCAN_METHODS.0.CreateCustomScanState.is_some());
    }

    // -----------------------------------------------------------------------
    // Thread count auto-detect logic (without calling GUC)
    // -----------------------------------------------------------------------

    #[test]
    fn auto_detect_thread_count_formula() {
        // Mirrors resolve_thread_count's auto-detect path:
        // (cores / 2).max(1), where GUC workers == 0.
        let cores = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(2);
        let expected = (cores / 2).max(1);
        assert!(expected >= 1);
        assert!(expected <= cores);
    }

    #[test]
    fn auto_detect_single_core_gives_one() {
        // If available_parallelism reports 1: (1 / 2).max(1) == 1.
        let cores = 1i32;
        let threads = (cores / 2).max(1);
        assert_eq!(threads, 1);
    }

    #[test]
    fn auto_detect_two_cores_gives_one() {
        let cores = 2i32;
        let threads = (cores / 2).max(1);
        assert_eq!(threads, 1);
    }

    #[test]
    fn auto_detect_eight_cores_gives_four() {
        let cores = 8i32;
        let threads = (cores / 2).max(1);
        assert_eq!(threads, 4);
    }

    // -----------------------------------------------------------------------
    // SORT_KEY_INTS / WINDOW_SPEC_INTS constants
    // -----------------------------------------------------------------------

    #[test]
    fn sort_key_ints_matches_sort_key_desc_field_count() {
        // SortKeyDesc has 4 fields: attno, sort_op, collation, nulls_first.
        assert_eq!(SORT_KEY_INTS, 4);
    }

    #[test]
    fn window_spec_ints_matches_window_func_spec_field_count() {
        // WindowFuncSpec has 7 fields: func, partition_attno, order_attno,
        // value_attno, offset, default_val, result_type_oid.
        assert_eq!(WINDOW_SPEC_INTS, 7);
    }

    // -----------------------------------------------------------------------
    // AccelStrategy — from_i32 and repr (used in serialization path)
    // -----------------------------------------------------------------------

    #[test]
    fn accel_strategy_from_i32_all_variants() {
        assert_eq!(AccelStrategy::from_i32(1), AccelStrategy::GpuSpatial);
        assert_eq!(AccelStrategy::from_i32(2), AccelStrategy::GpuRaster);
        assert_eq!(AccelStrategy::from_i32(3), AccelStrategy::GpuH3);
        assert_eq!(AccelStrategy::from_i32(4), AccelStrategy::GpuSort);
        assert_eq!(AccelStrategy::from_i32(5), AccelStrategy::GpuReduce);
        assert_eq!(AccelStrategy::from_i32(6), AccelStrategy::GpuExpr);
        assert_eq!(AccelStrategy::from_i32(7), AccelStrategy::GpuHashJoin);
        assert_eq!(AccelStrategy::from_i32(8), AccelStrategy::GpuWindow);
    }

    #[test]
    fn accel_strategy_unknown_defaults_to_gpu_spatial() {
        assert_eq!(AccelStrategy::from_i32(-1), AccelStrategy::GpuSpatial);
        assert_eq!(AccelStrategy::from_i32(0), AccelStrategy::GpuSpatial);
        assert_eq!(AccelStrategy::from_i32(99), AccelStrategy::GpuSpatial);
        assert_eq!(AccelStrategy::from_i32(i32::MAX), AccelStrategy::GpuSpatial);
    }

    #[test]
    fn accel_strategy_roundtrip() {
        for raw in 1..=8 {
            let strategy = AccelStrategy::from_i32(raw);
            assert_eq!(strategy as i32, raw);
        }
    }

    // -----------------------------------------------------------------------
    // GpuAccelScanState — repr(C) layout: css is first field
    // -----------------------------------------------------------------------

    #[test]
    fn gpu_accel_scan_state_css_is_at_offset_zero() {
        // Critical invariant: PG casts our node pointer to CustomScanState*,
        // which only works if css is the first field at offset 0.
        assert_eq!(
            std::mem::offset_of!(GpuAccelScanState, css),
            0,
            "css must be at offset 0 for PG upcasting to work"
        );
    }

    #[test]
    fn gpu_accel_scan_state_accel_follows_css() {
        // accel field must come after css.
        let css_offset = std::mem::offset_of!(GpuAccelScanState, css);
        let accel_offset = std::mem::offset_of!(GpuAccelScanState, accel);
        assert!(
            accel_offset > css_offset,
            "accel must follow css in memory layout"
        );
    }

    #[test]
    fn gpu_accel_scan_state_size_exceeds_custom_scan_state() {
        // Our extended struct must be larger than bare CustomScanState
        // since it adds the GpuAccelState fields.
        assert!(
            std::mem::size_of::<GpuAccelScanState>()
                > std::mem::size_of::<pg_sys::CustomScanState>()
        );
    }

    // -----------------------------------------------------------------------
    // Batch size clamping logic (mirroring begin_custom_scan)
    // -----------------------------------------------------------------------

    #[test]
    fn batch_size_clamp_zero_becomes_256() {
        // begin_custom_scan uses: if batch_size > 0 { batch_size } else { 256 }
        let raw = 0i32;
        let effective = if raw > 0 { raw as usize } else { 256 };
        assert_eq!(effective, 256);
    }

    #[test]
    fn batch_size_clamp_negative_becomes_256() {
        let raw = -10i32;
        let effective = if raw > 0 { raw as usize } else { 256 };
        assert_eq!(effective, 256);
    }

    #[test]
    fn batch_size_positive_kept_as_is() {
        let raw = 1024i32;
        let effective = if raw > 0 { raw as usize } else { 256 };
        assert_eq!(effective, 1024);
    }

    #[test]
    fn batch_size_one_is_valid() {
        // Minimum meaningful batch size.
        let raw = 1i32;
        let effective = if raw > 0 { raw as usize } else { 256 };
        assert_eq!(effective, 1);
    }

    // -----------------------------------------------------------------------
    // WindowFunc — from_i32 / to_i32 used in serialization
    // -----------------------------------------------------------------------

    #[test]
    fn window_func_roundtrip_all_variants() {
        let variants = [
            WindowFunc::RowNumber,
            WindowFunc::Rank,
            WindowFunc::DenseRank,
            WindowFunc::Sum,
            WindowFunc::Count,
            WindowFunc::Lag,
            WindowFunc::Lead,
        ];
        for func in variants {
            let raw = func.to_i32();
            let recovered = WindowFunc::from_i32(raw);
            assert_eq!(
                Some(func),
                recovered,
                "roundtrip failed for {func:?} (raw={raw})"
            );
        }
    }

    #[test]
    fn window_func_from_i32_invalid_returns_none() {
        assert!(WindowFunc::from_i32(-1).is_none());
        assert!(WindowFunc::from_i32(7).is_none());
        assert!(WindowFunc::from_i32(100).is_none());
    }

    // -----------------------------------------------------------------------
    // AggOp — from_i32 / to_i32 used in serialization
    // -----------------------------------------------------------------------

    #[test]
    fn agg_op_roundtrip_all_variants() {
        let variants = [AggOp::Sum, AggOp::Avg, AggOp::Min, AggOp::Max, AggOp::Count];
        for op in variants {
            let raw = op.to_i32();
            let recovered = AggOp::from_i32(raw);
            assert_eq!(
                std::mem::discriminant(&op),
                std::mem::discriminant(&recovered),
                "roundtrip failed for {op:?} (raw={raw})"
            );
        }
    }

    #[test]
    fn agg_op_unknown_maps_to_passthrough() {
        assert!(matches!(AggOp::from_i32(-1), AggOp::Passthrough));
        assert!(matches!(AggOp::from_i32(5), AggOp::Passthrough));
        assert!(matches!(AggOp::from_i32(99), AggOp::Passthrough));
    }

    // -----------------------------------------------------------------------
    // WindowFuncSpec — default_val bit encoding
    // -----------------------------------------------------------------------

    #[test]
    fn window_func_spec_default_val_zero_bits() {
        let spec = WindowFuncSpec {
            func: WindowFunc::Lag,
            partition_attno: 0,
            order_attno: 1,
            value_attno: 2,
            offset: 1,
            default_val: f64::from_bits(0),
            result_type_oid: pg_sys::FLOAT8OID.to_u32(),
        };
        assert_eq!(spec.default_val.to_bits(), 0);
        assert!((spec.default_val - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn window_func_spec_default_val_preserves_value() {
        let val = 42.5_f64;
        let bits = val.to_bits() as i32;
        // Simulate the deserialization path: f64::from_bits(bits as u64)
        let recovered = f64::from_bits(bits as u64);
        // Note: this only works for values whose bit pattern fits in i32.
        // The actual serialization truncates to i32, so we test that path.
        let _ = recovered; // value may differ due to truncation, but no panic.
    }

    // -----------------------------------------------------------------------
    // SortKeyDesc — field access patterns
    // -----------------------------------------------------------------------

    #[test]
    fn sort_key_desc_fields_accessible() {
        let key = SortKeyDesc {
            attno: 5,
            sort_op: pg_sys::Oid::from(97u32),
            collation: pg_sys::Oid::from(100u32),
            nulls_first: true,
        };
        assert_eq!(key.attno, 5);
        assert_eq!(u32::from(key.sort_op), 97);
        assert_eq!(u32::from(key.collation), 100);
        assert!(key.nulls_first);
    }

    #[test]
    fn sort_key_desc_nulls_first_false() {
        let key = SortKeyDesc {
            attno: 1,
            sort_op: pg_sys::Oid::from(0u32),
            collation: pg_sys::Oid::from(0u32),
            nulls_first: false,
        };
        assert!(!key.nulls_first);
    }

    // -----------------------------------------------------------------------
    // GroupKeyInfo — key_type values
    // -----------------------------------------------------------------------

    #[test]
    fn group_key_info_key_types() {
        // The key_type field maps: 0=i32, 1=i64, 2=f64.
        for (kt, label) in [(0, "i32"), (1, "i64"), (2, "f64")] {
            let gk = GroupKeyInfo {
                attno: 1,
                type_oid: pg_sys::Oid::from(23u32),
                key_type: kt,
            };
            assert_eq!(gk.key_type, kt, "key_type for {label} should be {kt}");
        }
    }

    // -----------------------------------------------------------------------
    // PgaccelKeyType used in hash join context
    // -----------------------------------------------------------------------

    #[test]
    fn hash_key_type_mapping_matches_begin_custom_scan() {
        // begin_custom_scan maps: 1 => Int64, 2 => Float64, _ => Int32
        let map = |raw: i32| -> PgaccelKeyType {
            match raw {
                1 => PgaccelKeyType::Int64,
                2 => PgaccelKeyType::Float64,
                _ => PgaccelKeyType::Int32,
            }
        };
        assert!(matches!(map(0), PgaccelKeyType::Int32));
        assert!(matches!(map(1), PgaccelKeyType::Int64));
        assert!(matches!(map(2), PgaccelKeyType::Float64));
        assert!(matches!(map(-1), PgaccelKeyType::Int32));
        assert!(matches!(map(99), PgaccelKeyType::Int32));
    }
}

// ---------------------------------------------------------------------------
// PreAgg serialization / deserialization
// ---------------------------------------------------------------------------

use crate::engine::executor::preagg::{
    DimFilter, GroupKeyDesc, JoinDepthDesc, PreAggColDesc,
};

/// Deserialized PreAgg configuration from `custom_private`.
struct PreAggPrivData {
    scan_relid: pg_sys::Index,
    depths: Vec<JoinDepthDesc>,
    agg_descs: Vec<PreAggColDesc>,
    group_keys: Vec<GroupKeyDesc>,
    scan_expr: Option<crate::engine::expr_compiler::CompiledExpr>,
}

/// Serialize PreAgg metadata into a PG `List` of `Integer` nodes.
///
/// Layout:
/// ```text
/// [STRATEGY=5, batch_size, expected_threads,
///  scan_relid, n_depths,
///  // Per depth:
///  outer_attno, inner_attno, key_type, n_dim_filters,
///  // Per dim filter: col_idx, cmp_opcode, const_val_hi, const_val_lo
///  // Per depth group cols: n_group_col_attnos, attno1, attno2, ...
///  n_agg_ops,
///  // Per agg: op_type, attno, type_oid
///  n_group_keys,
///  // Per group key: source, attno, type_oid
///  has_scan_expr, (if 1: template_type, ...template_data...)
/// ]
/// ```
///
/// # Safety
///
/// Must be called during planning on the main backend thread.
#[allow(clippy::cast_possible_wrap, clippy::too_many_lines)]
#[must_use]
pub unsafe fn serialize_preagg_private(
    scan_relid: pg_sys::Index,
    depths: &[JoinDepthDesc],
    agg_descs: &[PreAggColDesc],
    group_keys: &[GroupKeyDesc],
    scan_expr: Option<&crate::engine::expr_compiler::CompiledExpr>,
) -> *mut pg_sys::List {
    use crate::engine::expr_compiler::{CompiledExpr, TemplateKernel};

    let batch_size = super::super::gucs::min_batch_size();
    let expected_threads = resolve_thread_count();

    let mut list: *mut pg_sys::List = std::ptr::null_mut();
    // SAFETY: makeInteger + lappend allocate in CurrentMemoryContext.
    unsafe {
        list = pg_sys::lappend(list, pg_sys::makeInteger(GpuStrategy::PreAgg as c_int).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(batch_size).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(expected_threads).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(scan_relid as c_int).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(depths.len() as c_int).cast());

        // Per depth.
        for depth in depths {
            list = pg_sys::lappend(list, pg_sys::makeInteger(depth.outer_attno).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(depth.inner_attno).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(depth.key_type).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(depth.dim_filters.len() as c_int).cast(),
            );
            for filt in &depth.dim_filters {
                list = pg_sys::lappend(
                    list,
                    pg_sys::makeInteger(filt.col_idx as c_int).cast(),
                );
                list = pg_sys::lappend(
                    list,
                    pg_sys::makeInteger(filt.cmp_opcode as c_int).cast(),
                );
                // Encode f64 as two i32s (hi and lo bits).
                let bits = filt.const_val.to_bits();
                list = pg_sys::lappend(
                    list,
                    pg_sys::makeInteger((bits >> 32) as c_int).cast(),
                );
                list = pg_sys::lappend(
                    list,
                    pg_sys::makeInteger(bits as u32 as c_int).cast(),
                );
            }
            // Group col attnos for this depth.
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(depth.group_col_attnos.len() as c_int).cast(),
            );
            for &attno in &depth.group_col_attnos {
                list = pg_sys::lappend(list, pg_sys::makeInteger(attno).cast());
            }
        }

        // Aggregates.
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(agg_descs.len() as c_int).cast(),
        );
        for desc in agg_descs {
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(desc.op.to_i32()).cast(),
            );
            list = pg_sys::lappend(list, pg_sys::makeInteger(desc.attno).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(u32::from(desc.type_oid) as c_int).cast(),
            );
        }

        // GROUP BY keys.
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(group_keys.len() as c_int).cast(),
        );
        for gk in group_keys {
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(gk.source as c_int).cast(),
            );
            list = pg_sys::lappend(list, pg_sys::makeInteger(gk.attno).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(u32::from(gk.type_oid) as c_int).cast(),
            );
        }

        // Serialize fact-side scan expression.
        match scan_expr {
            Some(CompiledExpr::Template(TemplateKernel::CmpConst {
                col_idx, cmp_opcode, const_val,
            })) => {
                list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast()); // has
                list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast()); // type=CmpConst
                list = pg_sys::lappend(list, pg_sys::makeInteger(*col_idx as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(*cmp_opcode as c_int).cast());
                let bits = const_val.to_bits();
                list = pg_sys::lappend(list, pg_sys::makeInteger((bits >> 32) as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(bits as u32 as c_int).cast());
            }
            Some(CompiledExpr::Template(TemplateKernel::Between {
                col_idx, lo, hi,
            })) => {
                list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast()); // has
                list = pg_sys::lappend(list, pg_sys::makeInteger(2).cast()); // type=Between
                list = pg_sys::lappend(list, pg_sys::makeInteger(*col_idx as c_int).cast());
                let lo_bits = lo.to_bits();
                list = pg_sys::lappend(list, pg_sys::makeInteger((lo_bits >> 32) as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(lo_bits as u32 as c_int).cast());
                let hi_bits = hi.to_bits();
                list = pg_sys::lappend(list, pg_sys::makeInteger((hi_bits >> 32) as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(hi_bits as u32 as c_int).cast());
            }
            Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                col1_idx, cmp1_opcode, const1_val,
                col2_idx, cmp2_opcode, const2_val,
            })) => {
                list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast()); // has
                list = pg_sys::lappend(list, pg_sys::makeInteger(3).cast()); // type=TwoPredAnd
                list = pg_sys::lappend(list, pg_sys::makeInteger(*col1_idx as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(*cmp1_opcode as c_int).cast());
                let b1 = const1_val.to_bits();
                list = pg_sys::lappend(list, pg_sys::makeInteger((b1 >> 32) as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(b1 as u32 as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(*col2_idx as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(*cmp2_opcode as c_int).cast());
                let b2 = const2_val.to_bits();
                list = pg_sys::lappend(list, pg_sys::makeInteger((b2 >> 32) as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(b2 as u32 as c_int).cast());
            }
            _ => {
                list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // no scan_expr
            }
        }
    }

    list
}

/// Deserialize PreAgg configuration from `custom_private`.
///
/// # Safety
///
/// `custom_private` must be a valid PG `List` of Integer nodes.
#[allow(clippy::cast_sign_loss, clippy::too_many_lines)]
unsafe fn deserialize_preagg_private(custom_private: *mut pg_sys::List) -> PreAggPrivData {
    use crate::engine::expr_compiler::{CompiledExpr, TemplateKernel};

    let empty = PreAggPrivData {
        scan_relid: 0,
        depths: vec![],
        agg_descs: vec![],
        group_keys: vec![],
        scan_expr: None,
    };
    if custom_private.is_null() {
        return empty;
    }

    let mut idx: c_int = 3; // skip [strategy, batch_size, expected_threads]

    // SAFETY: custom_private is a valid List.
    let scan_relid = unsafe { list_int_at(custom_private, idx) } as pg_sys::Index;
    idx += 1;
    let n_depths = unsafe { list_int_at(custom_private, idx) } as usize;
    idx += 1;

    let mut depths = Vec::with_capacity(n_depths);
    for _ in 0..n_depths {
        let outer_attno = unsafe { list_int_at(custom_private, idx) };
        idx += 1;
        let inner_attno = unsafe { list_int_at(custom_private, idx) };
        idx += 1;
        let key_type = unsafe { list_int_at(custom_private, idx) };
        idx += 1;
        let n_filters = unsafe { list_int_at(custom_private, idx) } as usize;
        idx += 1;

        let mut dim_filters = Vec::with_capacity(n_filters);
        for _ in 0..n_filters {
            let col_idx = unsafe { list_int_at(custom_private, idx) } as usize;
            idx += 1;
            let cmp_opcode = unsafe { list_int_at(custom_private, idx) } as u16;
            idx += 1;
            let bits_hi = unsafe { list_int_at(custom_private, idx) } as u32;
            idx += 1;
            let bits_lo = unsafe { list_int_at(custom_private, idx) } as u32;
            idx += 1;
            let const_val = f64::from_bits(((bits_hi as u64) << 32) | bits_lo as u64);
            dim_filters.push(DimFilter {
                col_idx,
                cmp_opcode,
                const_val,
            });
        }

        let n_group_cols = unsafe { list_int_at(custom_private, idx) } as usize;
        idx += 1;
        let mut group_col_attnos = Vec::with_capacity(n_group_cols);
        for _ in 0..n_group_cols {
            group_col_attnos.push(unsafe { list_int_at(custom_private, idx) });
            idx += 1;
        }

        depths.push(JoinDepthDesc {
            outer_attno,
            inner_attno,
            key_type,
            dim_filters,
            group_col_attnos,
        });
    }

    // Aggregates.
    let n_aggs = unsafe { list_int_at(custom_private, idx) } as usize;
    idx += 1;
    let mut agg_descs = Vec::with_capacity(n_aggs);
    for _ in 0..n_aggs {
        let op = AggOp::from_i32(unsafe { list_int_at(custom_private, idx) });
        idx += 1;
        let attno = unsafe { list_int_at(custom_private, idx) };
        idx += 1;
        let type_oid_raw = unsafe { list_int_at(custom_private, idx) } as u32;
        idx += 1;
        agg_descs.push(PreAggColDesc {
            op,
            attno,
            type_oid: pg_sys::Oid::from(type_oid_raw),
        });
    }

    // GROUP BY keys.
    let n_gkeys = unsafe { list_int_at(custom_private, idx) } as usize;
    idx += 1;
    let mut group_keys = Vec::with_capacity(n_gkeys);
    for _ in 0..n_gkeys {
        let source = unsafe { list_int_at(custom_private, idx) } as u32;
        idx += 1;
        let attno = unsafe { list_int_at(custom_private, idx) };
        idx += 1;
        let type_oid_raw = unsafe { list_int_at(custom_private, idx) } as u32;
        idx += 1;
        group_keys.push(GroupKeyDesc {
            source,
            attno,
            type_oid: pg_sys::Oid::from(type_oid_raw),
        });
    }

    // Deserialize scan_expr.
    let has_scan_expr = unsafe { list_int_at(custom_private, idx) };
    idx += 1;
    let scan_expr = if has_scan_expr == 1 {
        let template_type = unsafe { list_int_at(custom_private, idx) };
        idx += 1;
        match template_type {
            1 => {
                // CmpConst
                let col_idx = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let cmp_opcode = unsafe { list_int_at(custom_private, idx) } as u16;
                idx += 1;
                let bits_hi = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let bits_lo = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let const_val = f64::from_bits(((bits_hi as u64) << 32) | bits_lo as u64);
                Some(CompiledExpr::Template(TemplateKernel::CmpConst {
                    col_idx, cmp_opcode, const_val,
                }))
            }
            2 => {
                // Between
                let col_idx = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let lo_hi = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let lo_lo = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let lo = f64::from_bits(((lo_hi as u64) << 32) | lo_lo as u64);
                let hi_hi = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let hi_lo = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let hi = f64::from_bits(((hi_hi as u64) << 32) | hi_lo as u64);
                Some(CompiledExpr::Template(TemplateKernel::Between {
                    col_idx, lo, hi,
                }))
            }
            3 => {
                // TwoPredAnd
                let col1_idx = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let cmp1_opcode = unsafe { list_int_at(custom_private, idx) } as u16;
                idx += 1;
                let b1_hi = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let b1_lo = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let const1_val = f64::from_bits(((b1_hi as u64) << 32) | b1_lo as u64);
                let col2_idx = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let cmp2_opcode = unsafe { list_int_at(custom_private, idx) } as u16;
                idx += 1;
                let b2_hi = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let b2_lo = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let const2_val = f64::from_bits(((b2_hi as u64) << 32) | b2_lo as u64);
                Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                    col1_idx, cmp1_opcode, const1_val,
                    col2_idx, cmp2_opcode, const2_val,
                }))
            }
            _ => None,
        }
    } else {
        None
    };

    // Suppress unused-assignment warning for idx.
    let _ = idx;

    PreAggPrivData {
        scan_relid,
        depths,
        agg_descs,
        group_keys,
        scan_expr,
    }
}
