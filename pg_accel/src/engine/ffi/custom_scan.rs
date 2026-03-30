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
        let path_priv = (*best_path).custom_private;
        let sort_keys = deserialize_path_sort_keys(path_priv);
        list = serialize_sort_keys(list, &sort_keys);

        // Serialize limit_tuples (appended after sort keys by planner).
        // Path layout: [num_keys, ...sort_key_data..., limit_tuples]
        let path_limit = deserialize_path_limit(path_priv, &sort_keys);
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

        // Build custom_scan_tlist and plan.targetlist from the Aggref tlist.
        // Each Aggref becomes a simple Var so PG never tries to evaluate
        // the aggregate expression itself.
        let tlist_len = if tlist.is_null() {
            0
        } else {
            pg_sys::list_length(tlist)
        };

        let mut scan_tlist: *mut pg_sys::List = std::ptr::null_mut();
        let mut plan_tlist: *mut pg_sys::List = std::ptr::null_mut();

        for i in 0..tlist_len {
            let tle = pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>();
            let resno = (i + 1) as i16;

            // Determine result type from the Aggref (or fall back to
            // FLOAT8 if the expression isn't an Aggref).
            let (vartype, vartypmod, varcollid) = if tle.is_null() {
                (pg_sys::FLOAT8OID, -1i32, pg_sys::InvalidOid)
            } else {
                let expr = (*tle).expr;
                if !expr.is_null()
                    && (*expr.cast::<pg_sys::Node>()).type_ == pg_sys::NodeTag::T_Aggref
                {
                    let aggref = expr.cast::<pg_sys::Aggref>();
                    ((*aggref).aggtype, -1i32, (*aggref).aggcollid)
                } else {
                    (pg_sys::FLOAT8OID, -1i32, pg_sys::InvalidOid)
                }
            };

            // Preserve original TargetEntry metadata for plan.targetlist.
            let (orig_resno, orig_name, orig_junk) = if tle.is_null() {
                (resno, std::ptr::null_mut(), false)
            } else {
                ((*tle).resno, (*tle).resname, (*tle).resjunk)
            };

            // Both custom_scan_tlist and plan.targetlist use
            // Var(INDEX_VAR, attno). PG's set_customscan_references
            // processes custom_scan_tlist via fix_scan_list (which
            // leaves INDEX_VAR alone) then matches plan.targetlist
            // Vars against it via fix_upper_expr / tlist_member_match_var.
            let scan_var =
                pg_sys::makeVar(pg_sys::INDEX_VAR, resno, vartype, vartypmod, varcollid, 0);
            let scan_tle =
                pg_sys::makeTargetEntry(scan_var.cast(), resno, std::ptr::null_mut(), false);
            scan_tlist = pg_sys::lappend(scan_tlist, scan_tle.cast());

            let plan_var =
                pg_sys::makeVar(pg_sys::INDEX_VAR, resno, vartype, vartypmod, varcollid, 0);
            let plan_tle =
                pg_sys::makeTargetEntry(plan_var.cast(), orig_resno, orig_name, orig_junk);
            plan_tlist = pg_sys::lappend(plan_tlist, plan_tle.cast());
        }

        (*cscan).custom_scan_tlist = scan_tlist;
        (*cscan).scan.plan.targetlist = plan_tlist;

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
        // Append aggregate descriptors (triples: op, attno, result_type_oid).
        list = pg_sys::lappend(list, pg_sys::makeInteger(num_aggs).cast());
        for k in 0..num_aggs {
            let op = list_int_at(path_priv, 1 + k * 3);
            let attno = list_int_at(path_priv, 2 + k * 3);
            let rtype = list_int_at(path_priv, 3 + k * 3);
            list = pg_sys::lappend(list, pg_sys::makeInteger(op).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(attno).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(rtype).cast());
        }

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

        // Build custom_scan_tlist and plan.targetlist from the window tlist.
        // Window functions add extra output columns; we need to map them
        // through as simple Vars so PG doesn't try to evaluate WindowFunc
        // expressions itself.
        let tlist_len = if tlist.is_null() {
            0
        } else {
            pg_sys::list_length(tlist)
        };

        let mut scan_tlist: *mut pg_sys::List = std::ptr::null_mut();
        let mut plan_tlist: *mut pg_sys::List = std::ptr::null_mut();

        for i in 0..tlist_len {
            let tle = pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>();
            let resno = (i + 1) as i16;

            let (vartype, vartypmod, varcollid) = if tle.is_null() {
                (pg_sys::FLOAT8OID, -1i32, pg_sys::InvalidOid)
            } else {
                let expr = (*tle).expr;
                if expr.is_null() {
                    (pg_sys::FLOAT8OID, -1i32, pg_sys::InvalidOid)
                } else {
                    let tag = (*expr.cast::<pg_sys::Node>()).type_;
                    if tag == pg_sys::NodeTag::T_WindowFunc {
                        let wf = expr.cast::<pg_sys::WindowFunc>();
                        ((*wf).wintype, -1i32, (*wf).wincollid)
                    } else {
                        // Use exprType for regular expressions.
                        (pg_sys::exprType(expr.cast()), -1i32, pg_sys::InvalidOid)
                    }
                }
            };

            let (orig_resno, orig_name, orig_junk) = if tle.is_null() {
                (resno, std::ptr::null_mut(), false)
            } else {
                ((*tle).resno, (*tle).resname, (*tle).resjunk)
            };

            let scan_var =
                pg_sys::makeVar(pg_sys::INDEX_VAR, resno, vartype, vartypmod, varcollid, 0);
            let scan_tle =
                pg_sys::makeTargetEntry(scan_var.cast(), resno, std::ptr::null_mut(), false);
            scan_tlist = pg_sys::lappend(scan_tlist, scan_tle.cast());

            let plan_var =
                pg_sys::makeVar(pg_sys::INDEX_VAR, resno, vartype, vartypmod, varcollid, 0);
            let plan_tle =
                pg_sys::makeTargetEntry(plan_var.cast(), orig_resno, orig_name, orig_junk);
            plan_tlist = pg_sys::lappend(plan_tlist, plan_tle.cast());
        }

        (*cscan).custom_scan_tlist = scan_tlist;
        (*cscan).scan.plan.targetlist = plan_tlist;

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

        // Copy window specs from path's custom_private.
        // Path layout: [num_specs, spec0_func, spec0_part_attno, spec0_order_attno,
        //               spec0_value_attno, spec0_offset, spec0_default_bits,
        //               spec0_result_type, ...]
        if path_len > 0 {
            let num_specs = list_int_at(path_priv, 0);
            list = pg_sys::lappend(list, pg_sys::makeInteger(num_specs).cast());
            for k in 0..num_specs {
                let base = 1 + k * WINDOW_SPEC_INTS as c_int;
                for j in 0..WINDOW_SPEC_INTS as c_int {
                    if (base + j) < path_len as c_int {
                        let val = list_int_at(path_priv, base + j);
                        list = pg_sys::lappend(list, pg_sys::makeInteger(val).cast());
                    }
                }
            }
        }

        (*cscan).custom_private = list;
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

        if is_scan {
            // SAFETY: custom_plans contains child Plan nodes created by PG.
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
            (*cscan).custom_scan_tlist = if child_tlist.is_null() {
                // Fallback: build physical tlist from relation.
                // SAFETY: root and rel are valid planner pointers.
                pg_sys::build_physical_tlist(root, rel)
            } else {
                child_tlist
            };
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

        // scanrelid=0 for both scan and join nodes: we always wrap a child
        // plan rather than scanning the relation directly.
        (*cscan).scan.scanrelid = 0;
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
        if accel_strategy_raw == AccelStrategy::GpuHashJoin as c_int {
            let path_priv_len = pg_sys::list_length(path_priv) as usize;
            if path_priv_len > 4 {
                let inner_attno = list_int_at(path_priv, 3);
                let key_type = list_int_at(path_priv, 4);
                list = pg_sys::lappend(list, pg_sys::makeInteger(inner_attno).cast());
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
    pgrx::debug1!("pg_accel: begin_custom_scan");
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
        pgrx::debug1!(
            "pg_accel: begin_custom_scan: {} child plan(s)",
            if (*node).custom_ps.is_null() {
                0
            } else {
                pg_sys::list_length((*node).custom_ps)
            }
        );
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
    //
    // SAFETY: node points to our extended GpuAccelScanState.
    unsafe {
        (*state).accel.executor = if privdata.gpu_strategy == GpuStrategy::Agg {
            let exec = if let Some(gk) = privdata.group_key {
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
            pgrx::debug1!(
                "pg_accel: begin_custom_scan: GpuHashJoin outer_attno={}, inner_attno={}, key_type={}",
                privdata.target_attno,
                privdata.hash_inner_attno,
                privdata.hash_key_type,
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
            let qual_ctx = find_accel_context_in_qual((*cscan).scan.plan.qual);

            let (strategy, fn_oid, target_attno, qual_datum) =
                if privdata.fn_oid == pg_sys::Oid::INVALID {
                    // Fallback: discover everything from qual list.
                    qual_ctx.map_or(
                        (AccelStrategy::BatchedEval, pg_sys::Oid::INVALID, 0, None),
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
                };

            pgrx::debug1!(
                "pg_accel: begin_custom_scan: accel strategy = {:?}",
                strategy
            );

            let mut exec = Box::new(ScanExecState::new(
                strategy,
                batch_size,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ));

            // For GpuExpr strategy, attempt expression compilation.
            // Template match is tried first (fastest), then bytecode,
            // then CPU fallback if neither works.
            if strategy == AccelStrategy::GpuExpr {
                // For now, store CpuFallback — actual PG expression tree
                // walking requires ExprState traversal which is wired
                // separately. The dispatch_gpu_expr path handles CpuFallback
                // by delegating to dispatch_scalar_qual.
                exec.set_compiled_expr(crate::engine::expr_compiler::CompiledExpr::CpuFallback);
                pgrx::debug1!("pg_accel: begin_custom_scan: GpuExpr compiled (CpuFallback)");
            }

            // Wire the GPU context so dispatch can route to the correct
            // GPU kernel (spatial, H3, raster) instead of BatchedEval.
            if strategy != AccelStrategy::BatchedEval && fn_oid != pg_sys::Oid::INVALID {
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
        pgrx::debug1!("pg_accel: exec_custom_scan — executor is null, passthrough");
        // SAFETY: node is a valid CustomScanState on the main backend thread.
        return unsafe { passthrough_exec(node) };
    }

    // Get the child plan state from custom_ps.
    // SAFETY: custom_ps was populated by begin_custom_scan.
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
    // SAFETY: ss.ss_ScanTupleSlot is initialised by ExecInitCustomScan.
    let scan_slot = unsafe { (*node).ss.ss_ScanTupleSlot };

    // SAFETY: state points to our GpuAccelScanState with valid accel fields.
    let gpu_strategy = GpuStrategy::from_i32(unsafe { (*state).accel.strategy });

    // Dispatch to the correct executor based on strategy.
    // SAFETY: executor was allocated with the matching type in begin_custom_scan.
    // We are on the main backend thread. All pointers are valid.
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
    } else {
        // SAFETY: For non-Sort strategies, executor points to ScanExecState.
        let scan_state = unsafe { &mut *executor.cast::<ScanExecState>() };
        let slot = unsafe { scan_state.next(child_ps, scan_slot) };
        (
            slot,
            scan_state.rows_dispatched,
            scan_state.batches_executed,
            scan_state.dispatch_time_us,
        )
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
                if gpu_strategy == GpuStrategy::Agg {
                    // SAFETY: executor was Box::into_raw'd as AggExecState.
                    let _ = Box::from_raw((*state).accel.executor.cast::<AggExecState>());
                } else if gpu_strategy == GpuStrategy::Window {
                    // SAFETY: executor was Box::into_raw'd as WindowExecState.
                    let _ = Box::from_raw((*state).accel.executor.cast::<WindowExecState>());
                } else if gpu_strategy == GpuStrategy::Sort {
                    // SAFETY: executor was Box::into_raw'd as SortExecState.
                    let _ = Box::from_raw((*state).accel.executor.cast::<SortExecState>());
                } else {
                    // SAFETY: executor was Box::into_raw'd as ScanExecState.
                    let _ = Box::from_raw((*state).accel.executor.cast::<ScanExecState>());
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

/// `ReScanCustomScan`: handle rescanning (e.g., for nested loops).
///
/// # Safety
///
/// Called by the executor on the main backend thread.
unsafe extern "C-unwind" fn rescan_custom_scan(node: *mut pg_sys::CustomScanState) {
    let state = node.cast::<GpuAccelScanState>();

    // SAFETY: Drop the old executor state and create a fresh one.
    // For ScanExecState, preserve qual/econtext pointers (owned by PG).
    // For SortExecState, preserve sort key descriptors.
    // All pointer accesses are to our GpuAccelScanState on the main thread.
    unsafe {
        let gpu_strategy = GpuStrategy::from_i32((*state).accel.strategy);
        let batch_size = if (*state).accel.batch_size > 0 {
            (*state).accel.batch_size as usize
        } else {
            256
        };

        if gpu_strategy == GpuStrategy::Agg {
            // Preserve aggregate column descriptors and group key from the old state.
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
        } else {
            let (qual, econtext, old_strategy, old_fn_oid, old_attno, old_qual_datum) =
                if (*state).accel.executor.is_null() {
                    (
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        AccelStrategy::BatchedEval,
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
            // Re-wire GPU context preserved from the previous scan state.
            if old_fn_oid != pg_sys::InvalidOid && old_strategy != AccelStrategy::BatchedEval {
                // SAFETY: old_fn_oid was validated during the initial
                // set_gpu_context call. We are on the main backend thread.
                exec.set_gpu_context(old_fn_oid, old_attno, old_qual_datum);
            }
            (*state).accel.executor = Box::into_raw(exec).cast();
        }

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
    /// Inner relation join key attno (1-based). Only for `GpuHashJoin`.
    hash_inner_attno: i32,
    /// Key type for hash join (0=i32, 1=i64, 2=f64). Only for `GpuHashJoin`.
    hash_key_type: i32,
    /// Window function specifications. Only meaningful when `gpu_strategy == Window`.
    window_specs: Vec<WindowFuncSpec>,
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
            accel_strategy: AccelStrategy::BatchedEval,
            sort_keys: vec![],
            sort_limit: None,
            agg_columns: vec![],
            group_key: None,
            hash_inner_attno: 0,
            hash_key_type: 0,
            window_specs: vec![],
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
    // Layout: [...agg descs..., has_group_key, gk_attno, gk_type_oid, gk_key_type]
    let group_key = if matches!(gpu_strategy, GpuStrategy::Agg) {
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
                Some(GroupKeyInfo {
                    attno: gk_attno,
                    type_oid: gk_type_oid,
                    key_type: gk_key_type,
                })
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
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
        hash_inner_attno,
        hash_key_type,
        window_specs,
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
        // Large positive values default to Scan.
        assert_eq!(GpuStrategy::from_i32(4), GpuStrategy::Scan);
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
}
