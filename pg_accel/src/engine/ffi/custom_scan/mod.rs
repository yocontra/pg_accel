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
use crate::engine::executor::agg::{AggExecState, AggOp, GroupKeyInfo, is_h3_synthetic_group_key};
use crate::engine::executor::join::JoinExecState;
use crate::engine::executor::scan::ScanExecState;
use crate::engine::executor::window::{
    WINDOW_SPEC_INTS, WindowExecState, WindowFunc, WindowFuncSpec,
};
use crate::engine::registry::{self, AccelStrategy};
use crate::engine::residency::ResidentProofSnapshot;
use crate::engine::stats;
use crate::gpu::PgaccelKeyType;

mod dsm;
mod explain;
mod function_scan;
mod plan_partial_agg;
mod private_data;
mod srf_target_list;

use explain::{explain_custom_scan, resolve_thread_count};
#[cfg(feature = "pg_test")]
use private_data::CustomPrivateData;
pub(in crate::engine::ffi) use private_data::HASH_JOIN_RESIDENT_COUNT_SENTINEL;
pub(super) use private_data::{
    AGG_OLAP_SENTINEL, AGG_SCAN_EXPR_SENTINEL, PARTIAL_SENTINEL, append_olap_agg_spec,
    append_partial_spec, deserialize_partial_spec,
};
pub use private_data::{
    FUNCTIONSCAN_SENTINEL, FunctionScanPrivData, append_functionscan_priv,
    deserialize_functionscan_priv,
};
pub(in crate::engine::ffi) use private_data::{
    append_resident_proof_snapshot, resident_proof_default_for_strategy,
};
use private_data::{deserialize_custom_private, deserialize_resident_proof_snapshot};
// SRF executor (Round 3 follow-up) — public re-exports for the planner
// hook in `srf_target_list.rs` and the executor module in `srf_target_list.rs`.
pub use private_data::{
    SRF_TARGET_LIST_SENTINEL, SrfTargetListPrivData, append_srf_target_list_priv,
    deserialize_srf_target_list_priv,
};

// ---------------------------------------------------------------------------
// FunctionScan output-shape discriminants
// ---------------------------------------------------------------------------

/// Mirror of [`crate::engine::registry::OutputShape`] using a flat integer
/// representation, suitable for [`FunctionScanPrivData::output_shape_disc`].
///
/// The mapping is fixed by the `to_i32` / `from_i32` pair below and is the
/// authoritative wire format for FunctionScan plan-priv data — adding a new
/// shape requires bumping both ends of the round-trip simultaneously.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputShapeDisc {
    /// One Datum per input row (`OutputShape::Scalar`).
    Scalar = 0,
    /// Fixed `field_count` Datums per input row (`OutputShape::Record`).
    Record = 1,
    /// CSR-style variable-length per input row (`OutputShape::VarLen`).
    VarLen = 2,
}

impl OutputShapeDisc {
    /// Convert to the integer form used in FunctionScanPrivData.
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        self as i32
    }

    /// Decode from the integer form. Unknown values are invalid.
    #[must_use]
    pub const fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Scalar),
            1 => Some(Self::Record),
            2 => Some(Self::VarLen),
            _ => None,
        }
    }
}

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
    /// FunctionScan injection (Phase 2 F3): replace PG's `FunctionScan` for a
    /// registered SRF / record-returning function with a Custom Scan that
    /// dispatches the call through the GPU dispatch interface and emits the
    /// resulting `AcceleratedVarLen` / `AcceleratedRecord` shapes as scan
    /// tuples.
    FunctionScan = 6,
    /// SRF-in-target-list injection (Phase 2 F3 follow-up): replace PG's
    /// `ProjectSet` plan node for `SELECT srf(col), passthrough_cols FROM t`
    /// queries. Drives the underlying scan via `ExecProcNode` and expands
    /// each input row's SRF call into multiple output tuples while
    /// preserving non-SRF target-list columns.
    SrfTargetList = 7,
}

impl GpuStrategy {
    /// Convert from raw integer. Unknown values are invalid.
    #[must_use]
    pub const fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Scan),
            1 => Some(Self::Join),
            2 => Some(Self::Agg),
            3 => Some(Self::Sort),
            4 => Some(Self::Window),
            5 => Some(Self::PreAgg),
            6 => Some(Self::FunctionScan),
            7 => Some(Self::SrfTargetList),
            _ => None,
        }
    }

    /// Decode state private data at execution time.
    #[must_use]
    pub fn decode(v: i32) -> Self {
        Self::from_i32(v).unwrap_or_else(|| pgrx::error!("pg_accel: invalid GPU strategy tag {v}"))
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
            Self::FunctionScan => c"GpuFunctionScan",
            Self::SrfTargetList => c"GpuAccelSrfTargetList",
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
    pub(super) parallel_worker_number: i32,
    pub(super) dsm_flags: u32,
    pub(super) dsm_state: *mut dsm::GpuAccelDsmState,
    pub(super) dsm_counters_recorded: bool,
    pub(super) parallel_agg_participants: u32,
    pub(super) parallel_agg_active_participants: u32,
    pub(super) parallel_agg_rows_dispatched: u64,
    pub(super) parallel_agg_batches_executed: u64,
    pub(super) parallel_agg_dispatch_time_us: u64,
    pub(super) resident_proof: ResidentProofSnapshot,
    /// Opaque pointer to heap-allocated Rust executor state. The concrete
    /// type depends on `strategy` (e.g. `ScanExecState`, `AggExecState`,
    /// `JoinExecState`). Set in `begin_custom_scan`, freed in `end_custom_scan`.
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

static FUNCTION_PATH_METHODS: SyncPathMethods = SyncPathMethods(pg_sys::CustomPathMethods {
    CustomName: c"GpuAccelFunctionScan".as_ptr(),
    PlanCustomPath: Some(plan_custom_path_function),
    ReparameterizeCustomPathByChild: None,
});

static FUNCTION_SCAN_METHODS: SyncScanMethods = SyncScanMethods(pg_sys::CustomScanMethods {
    CustomName: c"GpuAccelFunctionScan".as_ptr(),
    CreateCustomScanState: Some(create_custom_scan_state),
});

static SRF_TARGET_LIST_PATH_METHODS: SyncPathMethods = SyncPathMethods(pg_sys::CustomPathMethods {
    CustomName: c"GpuAccelSrfTargetList".as_ptr(),
    PlanCustomPath: Some(plan_custom_path_srf_target_list),
    ReparameterizeCustomPathByChild: None,
});

static SRF_TARGET_LIST_SCAN_METHODS: SyncScanMethods = SyncScanMethods(pg_sys::CustomScanMethods {
    CustomName: c"GpuAccelSrfTargetList".as_ptr(),
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
    // Parallel-worker DSM state coordinates worker-local scans and snapshots
    // aggregate counters for EXPLAIN after PostgreSQL tears down DSM.
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

/// Pointer to FunctionScan `CustomPathMethods` vtable (Phase 2 F3).
#[inline]
#[must_use]
pub fn function_path_methods() -> *const pg_sys::CustomPathMethods {
    &raw const FUNCTION_PATH_METHODS.0
}

/// Pointer to SRF-in-target-list `CustomPathMethods` vtable.
#[inline]
#[must_use]
pub fn srf_target_list_path_methods() -> *const pg_sys::CustomPathMethods {
    &raw const SRF_TARGET_LIST_PATH_METHODS.0
}

/// Register Custom Scan methods with PostgreSQL. Must be called from `_PG_init`.
pub fn register() {
    // SAFETY: RegisterCustomScanMethods stores pointers to our static vtables
    // which live for the entire process lifetime. Called on main thread during
    // extension loading.
    unsafe {
        pg_sys::RegisterCustomScanMethods(&raw const SCAN_SCAN_METHODS.0);
        pg_sys::RegisterCustomScanMethods(&raw const JOIN_SCAN_METHODS.0);
        pg_sys::RegisterCustomScanMethods(&raw const AGG_SCAN_METHODS.0);
        pg_sys::RegisterCustomScanMethods(&raw const WINDOW_SCAN_METHODS.0);
        pg_sys::RegisterCustomScanMethods(&raw const FUNCTION_SCAN_METHODS.0);
        pg_sys::RegisterCustomScanMethods(&raw const SRF_TARGET_LIST_SCAN_METHODS.0);
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

/// Append the proof carried by a planner CustomPath, or a conservative
/// host-staged default for older path payloads that predate proof trailers.
///
/// # Safety
///
/// Must run in a valid PostgreSQL planner memory context. `path_private`, when
/// non-null, must be a `List<Integer>` emitted by pg_accel.
unsafe fn append_path_resident_proof_or_default(
    list: *mut pg_sys::List,
    path_private: *mut pg_sys::List,
    strategy: GpuStrategy,
) -> *mut pg_sys::List {
    let proof = if path_private.is_null() {
        resident_proof_default_for_strategy(strategy)
    } else {
        unsafe { deserialize_resident_proof_snapshot(path_private) }
            .unwrap_or_else(|| resident_proof_default_for_strategy(strategy))
    };
    unsafe { append_resident_proof_snapshot(list, proof) }
}

/// Return the planned child `SeqScan` range-table index when an agg wraps a
/// direct heap scan.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
unsafe fn planned_child_seqscan_relid(custom_plans: *mut pg_sys::List) -> Option<c_int> {
    if custom_plans.is_null() || unsafe { pg_sys::list_length(custom_plans) } <= 0 {
        return None;
    }
    let child_plan = unsafe { pg_sys::list_nth(custom_plans, 0).cast::<pg_sys::Plan>() };
    if child_plan.is_null() {
        return None;
    }
    if unsafe { (*child_plan).type_ } != pg_sys::NodeTag::T_SeqScan {
        return None;
    }
    let scanrelid = unsafe { (*child_plan.cast::<pg_sys::Scan>()).scanrelid };
    c_int::try_from(scanrelid)
        .ok()
        .filter(|scanrelid| *scanrelid > 0)
}

/// Shared helper: build a `CustomScan` plan from a `CustomPath`.
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
        //    has_gk,
        //    (gk_attno, gk_type_oid, gk_key_type, gk2_attno, gk_tlist_pos)?,
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
        let group_key_payload = if has_gk != 0 {
            if gk_base + 7 >= path_len {
                pgrx::error!("pg_accel: malformed aggregate private group-key layout");
            }
            let gk_attno = list_int_at(path_priv, (gk_base + 1) as c_int);
            let gk_type_oid = list_int_at(path_priv, (gk_base + 2) as c_int);
            let gk_key_type = list_int_at(path_priv, (gk_base + 3) as c_int);
            let gk2_attno = list_int_at(path_priv, (gk_base + 4) as c_int);
            let gk_tlist_pos = list_int_at(path_priv, (gk_base + 5) as c_int);
            if gk_attno <= 0
                || !(matches!(gk_key_type, 0 | 1 | 2 | 4 | 5)
                    || is_h3_synthetic_group_key(gk_key_type))
            {
                pgrx::error!("pg_accel: invalid aggregate private group-key layout");
            }
            Some((gk_attno, gk_type_oid, gk_key_type, gk2_attno, gk_tlist_pos))
        } else {
            None
        };
        let self_scan_idx = if group_key_payload.is_some() {
            gk_base + 6
        } else {
            gk_base + 1
        };
        let is_partial_idx = self_scan_idx + 1;
        let self_scan_relid = if self_scan_idx < path_len {
            list_int_at(path_priv, self_scan_idx as c_int)
        } else {
            0
        };
        let self_scan_relid = if self_scan_relid > 0 {
            planned_child_seqscan_relid(custom_plans).unwrap_or(self_scan_relid)
        } else {
            0
        };
        let is_partial = if is_partial_idx < path_len {
            list_int_at(path_priv, is_partial_idx as c_int)
        } else {
            0
        };
        let mut trailer_idx = is_partial_idx + 1;
        let has_agg_scan_expr = trailer_idx < path_len
            && list_int_at(path_priv, trailer_idx as c_int) == AGG_SCAN_EXPR_SENTINEL;
        if has_agg_scan_expr {
            let template_type = list_int_at(path_priv, (trailer_idx + 1) as c_int);
            let trailer_len = match template_type {
                1 => 6,  // sentinel, type, col, op, const bits x2
                2 => 7,  // sentinel, type, col, lo bits x2, hi bits x2
                3 => 10, // sentinel, type, col/op/const x2
                _ => pgrx::error!("pg_accel: malformed aggregate scan-expression trailer"),
            };
            if trailer_idx + trailer_len > path_len {
                pgrx::error!("pg_accel: truncated aggregate scan-expression trailer");
            }
            trailer_idx += trailer_len;
        }
        let has_olap_block = trailer_idx < path_len
            && list_int_at(path_priv, trailer_idx as c_int) == AGG_OLAP_SENTINEL;
        if has_olap_block {
            let olap_trailer_ints = if trailer_idx + 1 < path_len {
                match list_int_at(path_priv, (trailer_idx + 1) as c_int) {
                    1 => 11,  // sentinel + Q1 payload
                    2 => 7,   // sentinel + Q2 payload
                    3 => 7,   // sentinel + Q3 payload
                    4 => 8,   // sentinel + Q4 payload
                    5 => 25,  // sentinel + legacy resident dense grouped f64 payload
                    6 => 5,   // sentinel + resident H3 grouped-count payload
                    7 => 26,  // sentinel + source-aware resident dense grouped f64 payload
                    8 => 30,  // sentinel + logical resident dense grouped f64 payload
                    9 => 34,  // sentinel + logical/source resident dense grouped f64 payload
                    10 => 19, // sentinel + resident star dimension grouped f64 payload
                    _ => pgrx::error!("pg_accel: malformed aggregate OLAP trailer kind"),
                }
            } else {
                pgrx::error!("pg_accel: truncated aggregate OLAP trailer");
            };
            if trailer_idx + olap_trailer_ints > path_len {
                pgrx::error!("pg_accel: truncated aggregate OLAP trailer");
            }
            trailer_idx += olap_trailer_ints;
        }
        let sentinel_idx = trailer_idx;
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
        (*cscan).scan.scanrelid = if has_agg_scan_expr && self_scan_relid > 0 {
            self_scan_relid as pg_sys::Index
        } else {
            0
        };
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

        // Forward group key info from path's custom_private. Plan layout:
        //   [..., has_group_key,
        //    (gk_attno, gk_type_oid, gk_key_type, gk2_attno, gk_tlist_pos)?,
        //    self_scan_relid, (PARTIAL_SENTINEL block)?]
        if let Some((gk_attno, gk_type_oid, gk_key_type, gk2_attno, gk_tlist_pos)) =
            group_key_payload
        {
            list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(remap_attno(gk_attno)).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(gk_type_oid).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(gk_key_type).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(if gk2_attno > 0 {
                    remap_attno(gk2_attno)
                } else {
                    gk2_attno
                })
                .cast(),
            );
            list = pg_sys::lappend(list, pg_sys::makeInteger(gk_tlist_pos).cast());
        } else {
            list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast());
        }

        // Append self-scan relid so begin_custom_scan can open the heap.
        list = pg_sys::lappend(list, pg_sys::makeInteger(self_scan_relid).cast());

        if sentinel_idx > is_partial_idx + 1 {
            for idx in is_partial_idx + 1..sentinel_idx {
                list = pg_sys::lappend(
                    list,
                    pg_sys::makeInteger(list_int_at(path_priv, idx as c_int)).cast(),
                );
            }
        }

        // When this path was injected on a partial (worker-side) Gather branch,
        // propagate the PartialAggSpec into the plan's custom_private using
        // the sentinel-prefixed layout. The deserializer treats absence of the
        // sentinel as `partial = None`.
        //
        if is_partial != 0 {
            // sentinel @ sentinel_idx, n_cols @ sentinel_idx + 1
            if !has_sentinel_block {
                pgrx::error!("pg_accel: partial aggregate path missing sentinel spec block");
            }
            let spec = deserialize_partial_spec(path_priv, sentinel_idx + 1).unwrap_or_else(|| {
                pgrx::error!("pg_accel: malformed partial aggregate sentinel spec block")
            });
            list = append_partial_spec(list, &spec);
        }

        list = append_path_resident_proof_or_default(list, path_priv, GpuStrategy::Agg);
        (*cscan).custom_private = list;
    }

    tracing::info!("plan_custom_path_agg: end");
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

        list = append_path_resident_proof_or_default(list, path_priv, GpuStrategy::Window);
        (*cscan).custom_private = list;
    }

    tracing::info!("plan_custom_path_window: end");
    cscan.cast()
}

/// Convert a FunctionScan `CustomPath` into a `CustomScan` plan node
/// (Phase 2 F3).
///
/// The path's `custom_private` already carries the serialized
/// [`FunctionScanPrivData`] block (sentinel-prefixed) — we pass it through
/// to the executor unchanged. Plan-side responsibilities:
///
/// 1. Set `scanrelid = 0` so PG treats this as a synthetic scan (no
///    underlying RTE_RELATION). The executor builds the scan slot from the
///    registry's `output_field_types` / `output_field_names`.
/// 2. Use the planner-supplied `tlist` for both `plan.targetlist` and
///    `custom_scan_tlist` (independent copies) so `setrefs` rewrites Vars
///    in the targetlist without aliasing the scan-slot descriptor.
/// 3. No child plan: the FunctionScan's funcexpr is invoked by our
///    dispatcher directly, not by ExecProcNode.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
/// Build a `custom_scan_tlist` for FunctionScan from the registry-derived
/// output schema. Each registry column becomes a `TargetEntry` wrapping a
/// `Var(varno = INDEX_VAR, varattno = i+1, type = output_field_types[i])`.
///
/// Returns `None` if the registry lookup fails (caller falls back to the
/// planner's own tlist).
///
/// # Safety
///
/// Called from the planner on the main backend thread. Allocates Var /
/// TargetEntry / List nodes via `palloc` in `CurrentMemoryContext`.
unsafe fn build_function_scan_tlist(
    priv_data: Option<&FunctionScanPrivData>,
) -> Option<*mut pg_sys::List> {
    let priv_data = priv_data?;
    crate::engine::registry::lazy_init();
    let entry = crate::engine::registry::global_registry().lookup(priv_data.fn_oid)?;
    let n = entry.output_field_types.len();
    if n == 0 || n != entry.output_field_names.len() {
        return None;
    }

    let mut list: *mut pg_sys::List = std::ptr::null_mut();
    for (i, (&t, &name)) in entry
        .output_field_types
        .iter()
        .zip(entry.output_field_names.iter())
        .enumerate()
    {
        let typ_oid = pg_sys::Oid::from(t);
        if typ_oid == pg_sys::InvalidOid {
            return None;
        }
        // SAFETY: makeVar allocates in CurrentMemoryContext. INDEX_VAR is
        // the placeholder varno PG expects when the parent will rewrite
        // Var refs through custom_scan_tlist via INDEX_VAR offsets
        // (createplan.c:set_customscan_references).
        let var = unsafe {
            pg_sys::makeVar(
                pg_sys::INDEX_VAR as c_int,
                pg_sys::AttrNumber::from((i + 1) as i16),
                typ_oid,
                -1,
                pg_sys::InvalidOid,
                0,
            )
        };
        let cname = std::ffi::CString::new(name).ok()?;
        // SAFETY: var is freshly allocated; makeTargetEntry takes ownership
        // of the resname pointer (palloc'd via pstrdup).
        let resname = unsafe { pg_sys::pstrdup(cname.as_ptr()) };
        let te = unsafe {
            pg_sys::makeTargetEntry(
                var.cast::<pg_sys::Expr>(),
                pg_sys::AttrNumber::from((i + 1) as i16),
                resname,
                false,
            )
        };
        // SAFETY: lappend allocates in CurrentMemoryContext.
        list = unsafe { pg_sys::lappend(list, te.cast()) };
    }
    Some(list)
}

unsafe extern "C-unwind" fn plan_custom_path_function(
    _root: *mut pg_sys::PlannerInfo,
    _rel: *mut pg_sys::RelOptInfo,
    best_path: *mut pg_sys::CustomPath,
    tlist: *mut pg_sys::List,
    clauses: *mut pg_sys::List,
    _custom_plans: *mut pg_sys::List,
) -> *mut pg_sys::Plan {
    let _span = tracing::debug_span!("ffi.plan_custom_path_function").entered();
    // SAFETY: palloc0 returns zeroed memory in CurrentMemoryContext.
    let cscan = unsafe {
        pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomScan>()).cast::<pg_sys::CustomScan>()
    };

    // Build `custom_scan_tlist` from the registry's full output schema rather
    // than copying the planner-supplied `tlist`. Rationale: the planner's
    // `tlist` is pruned to only what the parent expression references — for
    // `count(*) FROM srf(...)` it is empty (`natts=0`). PG's
    // `ExecInitCustomScan`
    // (`src/backend/executor/nodeCustom.c:ExecInitScanTupleSlot`) builds the
    // scan slot's `TupleDesc` from `ExecTypeFromTL(custom_scan_tlist)` and
    // marks it `TTS_FLAG_FIXED` (`MakeTupleTableSlot` in
    // `src/backend/executor/execTuples.c`), so the descriptor MUST be
    // correct at plan time — `BeginCustomScan` cannot patch it
    // (`ExecSetSlotDescriptor` asserts `!TTS_FIXED`). The registry-derived
    // schema (`output_field_types` / `output_field_names`) is the source of
    // truth for what the dispatcher will emit.
    //
    // SAFETY: best_path was validated by the planner; deserialize reads
    // sentinel-tagged Integer nodes and returns Option.
    let priv_data = unsafe { deserialize_functionscan_priv((*best_path).custom_private, 0) };

    // SAFETY: cscan is freshly palloc'd and zeroed; best_path + tlist are
    // valid planner pointers.
    unsafe {
        (*cscan).scan.plan.type_ = pg_sys::NodeTag::T_CustomScan;
        // `plan.targetlist` is the planner's own list (parent uses
        // `INDEX_VAR` offsets into `custom_scan_tlist` to resolve references).
        // SAFETY: copyObjectImpl deep-copies in CurrentMemoryContext.
        (*cscan).scan.plan.targetlist = pg_sys::copyObjectImpl(tlist.cast()).cast();
        // SAFETY: builds TargetEntry/Var nodes from registry metadata. Falls
        // back to copying the planner tlist if registry lookup fails (which
        // preserves the previous behaviour for unknown fn_oids and lets
        // begin_custom_scan validation surface the error).
        (*cscan).custom_scan_tlist = build_function_scan_tlist(priv_data.as_ref())
            .unwrap_or_else(|| pg_sys::copyObjectImpl(tlist.cast()).cast());
        (*cscan).scan.plan.qual = pg_sys::extract_actual_clauses(clauses, false);
        (*cscan).scan.plan.startup_cost = (*best_path).path.startup_cost;
        (*cscan).scan.plan.total_cost = (*best_path).path.total_cost;
        (*cscan).scan.plan.plan_rows = (*best_path).path.rows;
        (*cscan).custom_plans = std::ptr::null_mut();
        (*cscan).flags = (*best_path).flags;
        (*cscan).scan.scanrelid = 0;
        (*cscan).methods = &raw const FUNCTION_SCAN_METHODS.0;

        // Top-level FunctionScan strategy header so deserialize_custom_private
        // sees a uniform layout. The FunctionScanPrivData payload starts
        // immediately after.
        let batch_size = gucs::min_batch_size();
        let expected_threads = resolve_thread_count();
        let mut list: *mut pg_sys::List = std::ptr::null_mut();
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(GpuStrategy::FunctionScan as c_int).cast(),
        );
        list = pg_sys::lappend(list, pg_sys::makeInteger(batch_size).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(expected_threads).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // fn_oid placeholder (read from FSCA block)
        list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // target_attno placeholder
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(AccelStrategy::GpuH3 as c_int).cast(), // strategy hint, real one resolved at exec
        );

        // Copy the FUNCTIONSCAN_SENTINEL-prefixed payload from the path.
        // The planner hook stored it at index 0 of the path's custom_private.
        let path_priv = (*best_path).custom_private;
        if !path_priv.is_null() {
            let n = pg_sys::list_length(path_priv);
            for i in 0..n {
                let cell = pg_sys::list_nth(path_priv, i);
                list = pg_sys::lappend(list, cell);
            }
        }

        list = append_path_resident_proof_or_default(list, path_priv, GpuStrategy::FunctionScan);
        (*cscan).custom_private = list;
    }

    cscan.cast()
}

/// Convert an SRF-in-target-list `CustomPath` into a `CustomScan` plan node.
///
/// The path's `custom_private` carries a `SrfTargetListPrivData` block
/// (sentinel-prefixed at index 0) and `custom_paths[0]` holds the underlying
/// scan path that produces the per-row inputs. The planner has already
/// produced the lower plan via `create_plan_recurse`; we receive it via
/// `custom_plans[0]` and stash it on `cscan.custom_plans` so `ExecInitNode`
/// runs through it during executor init.
///
/// Plan-side responsibilities:
///
/// 1. `scanrelid = 0` (synthetic scan, no underlying RTE_RELATION).
/// 2. `custom_scan_tlist` mirrors the planner-supplied `tlist` (the upper
///    target list with passthrough Vars + the SRF FuncExpr position) — this
///    is what becomes the output tuple shape.
/// 3. `custom_plans` holds the child scan plan (used by `begin_custom_scan`
///    to run `ExecInitNode` and by `next_tuple` to drain via `ExecProcNode`).
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
unsafe extern "C-unwind" fn plan_custom_path_srf_target_list(
    _root: *mut pg_sys::PlannerInfo,
    _rel: *mut pg_sys::RelOptInfo,
    best_path: *mut pg_sys::CustomPath,
    tlist: *mut pg_sys::List,
    clauses: *mut pg_sys::List,
    custom_plans: *mut pg_sys::List,
) -> *mut pg_sys::Plan {
    let _span = tracing::debug_span!("ffi.plan_custom_path_srf_target_list").entered();
    // SAFETY: palloc0 returns zeroed memory in CurrentMemoryContext.
    let cscan = unsafe {
        pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomScan>()).cast::<pg_sys::CustomScan>()
    };

    // SAFETY: cscan is freshly palloc'd and zeroed; best_path + tlist + custom_plans
    // are valid planner pointers.
    unsafe {
        (*cscan).scan.plan.type_ = pg_sys::NodeTag::T_CustomScan;
        // The planner-supplied tlist is the upper target list (including the
        // SRF FuncExpr position). It defines our output tuple shape.
        // SAFETY: copyObjectImpl deep-copies the list in CurrentMemoryContext.
        (*cscan).scan.plan.targetlist = pg_sys::copyObjectImpl(tlist.cast()).cast();
        // custom_scan_tlist also gets a copy so PG's setrefs pass can rewrite
        // independently of plan.targetlist.
        (*cscan).custom_scan_tlist = pg_sys::copyObjectImpl(tlist.cast()).cast();
        (*cscan).scan.plan.qual = pg_sys::extract_actual_clauses(clauses, false);
        (*cscan).scan.plan.startup_cost = (*best_path).path.startup_cost;
        (*cscan).scan.plan.total_cost = (*best_path).path.total_cost;
        (*cscan).scan.plan.plan_rows = (*best_path).path.rows;
        // custom_plans carries the child scan plan PG built from
        // best_path.custom_paths[0]. begin_custom_scan calls ExecInitNode on it.
        (*cscan).custom_plans = custom_plans;
        (*cscan).flags = (*best_path).flags;
        (*cscan).scan.scanrelid = 0;
        (*cscan).methods = &raw const SRF_TARGET_LIST_SCAN_METHODS.0;

        // Top-level header so deserialize_custom_private routes us through the
        // SrfTargetList arm. The SrfTargetListPrivData payload follows.
        let batch_size = gucs::min_batch_size();
        let expected_threads = resolve_thread_count();
        let mut list: *mut pg_sys::List = std::ptr::null_mut();
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(GpuStrategy::SrfTargetList as c_int).cast(),
        );
        list = pg_sys::lappend(list, pg_sys::makeInteger(batch_size).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(expected_threads).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // fn_oid placeholder (read from STLS block)
        list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // target_attno placeholder
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(AccelStrategy::GpuH3 as c_int).cast(), // strategy hint
        );

        // Copy the SRF_TARGET_LIST_SENTINEL-prefixed payload from the path.
        let path_priv = (*best_path).custom_private;
        if !path_priv.is_null() {
            let n = pg_sys::list_length(path_priv);
            for i in 0..n {
                let cell = pg_sys::list_nth(path_priv, i);
                list = pg_sys::lappend(list, cell);
            }
        }

        list = append_path_resident_proof_or_default(list, path_priv, GpuStrategy::SrfTargetList);
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
                    strip_child_cpu_quals(child);
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

        // GpuSort was retired: no planner path-creator serializes it anymore.
        // Fail loudly if a stale path payload still carries it.
        if accel_strategy_raw == AccelStrategy::GpuSort as c_int {
            pgrx::error!("pg_accel: GpuSort strategy retired; planner must not create sort paths");
        }
        let strategy = if is_scan {
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
                let count_only = if path_priv_len > 7 {
                    list_int_at(path_priv, 7)
                } else {
                    0
                };
                let (resident_count, resident_outer_rel_oid, resident_inner_rel_oid) =
                    if path_priv_len > 11
                        && list_int_at(path_priv, 8) == HASH_JOIN_RESIDENT_COUNT_SENTINEL
                    {
                        (
                            list_int_at(path_priv, 9),
                            list_int_at(path_priv, 10),
                            list_int_at(path_priv, 11),
                        )
                    } else {
                        (0, 0, 0)
                    };

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
                list = pg_sys::lappend(list, pg_sys::makeInteger(count_only).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(resident_count).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(resident_outer_rel_oid).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(resident_inner_rel_oid).cast());
            }
        }

        // For GpuNestedLoopIneq, serialize the constrained BETWEEN/range
        // payload after the base 6 fields. Path layout:
        // [fn_oid=0, outer_value_attno, strategy, shape, key_type, op,
        //  outer_varno, inner_varno, inner_lo_attno, inner_hi_attno]
        if accel_strategy_raw == AccelStrategy::GpuNestedLoopIneq as c_int {
            let path_priv_len = pg_sys::list_length(path_priv) as usize;
            if path_priv_len > 9 {
                let shape = list_int_at(path_priv, 3);
                let key_type = list_int_at(path_priv, 4);
                let op = list_int_at(path_priv, 5);
                let outer_varno = list_int_at(path_priv, 6);
                let inner_varno = list_int_at(path_priv, 7);
                let raw_inner_lo_attno = list_int_at(path_priv, 8);
                let raw_inner_hi_attno = list_int_at(path_priv, 9);

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

                    let child_scanrelid = (*child.cast::<pg_sys::Scan>()).scanrelid;
                    let search_tlist = if child_scanrelid == 0
                        && (*child.cast::<pg_sys::Node>()).type_ == pg_sys::NodeTag::T_CustomScan
                    {
                        (*child.cast::<pg_sys::CustomScan>()).custom_scan_tlist
                    } else {
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
                            if v_attno == orig_attno && (orig_varno == 0 || v_varno == orig_varno) {
                                return i32::from((*tle).resno);
                            }
                        }
                    }
                    orig_attno
                };

                let remapped_outer = remap_via_child(0, target_attno, outer_varno);
                let remapped_lo = remap_via_child(1, raw_inner_lo_attno, inner_varno);
                let remapped_hi = remap_via_child(1, raw_inner_hi_attno, inner_varno);

                let cell4 = pg_sys::list_nth(list, 4).cast::<pg_sys::Integer>();
                (*cell4).ival = remapped_outer;

                list = pg_sys::lappend(list, pg_sys::makeInteger(shape).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(key_type).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(op).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(remapped_lo).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(remapped_hi).cast());
            }
        }

        list =
            append_path_resident_proof_or_default(list, path_priv, GpuStrategy::decode(strategy));
        (*cscan).custom_private = list;
    }

    cscan.cast()
}

/// Remove PostgreSQL CPU qualifier state from a child plan whose predicate is
/// owned by pg_accel's GPU scan executor.
///
/// `BitmapHeapScan.bitmapqualorig` is deliberately left intact. PostgreSQL's
/// bitmap heap scan re-evaluates `bitmapqualorig` for tuples coming from
/// lossy TIDBitmap pages (`src/backend/executor/nodeBitmapHeapscan.c`,
/// `BitmapHeapRecheck`; `bitmapqualorig` is documented in
/// `src/include/nodes/plannodes.h` as "index quals, in standard expr form" —
/// when `work_mem` forces the bitmap lossy, every tuple on a matched page is
/// returned and only the `bitmapqualorig` recheck filters non-matching rows).
/// Those original index quals were consumed by the bitmap index path and are
/// NOT part of the clause list hoisted into the CustomScan's own
/// `plan.qual`, so nulling `bitmapqualorig` here removed the only place they
/// were evaluated and let non-matching tuples from lossy pages into the
/// result set.
///
/// # Safety
///
/// `child` must point to a valid PostgreSQL `Plan` node.
unsafe fn strip_child_cpu_quals(child: *mut pg_sys::Plan) {
    unsafe {
        (*child).qual = std::ptr::null_mut();
    }
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
        (*state).accel.parallel_worker_number = -1;
        (*state).accel.resident_proof = ResidentProofSnapshot::not_proven();

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
    /// Every constant argument captured from the function's args list, in
    /// positional order. For 2-arg predicates like
    /// `ST_Intersects(geom_col, $const)`, this carries one element. Multi-
    /// arg ops (e.g. `ST_DWithin(g, g, threshold)`,
    /// `ST_Hillshade(r, cx, cy, az, alt)`) push every Const in the order
    /// they appeared. Empty when every argument is a `Var`.
    qual_datums: Vec<(pg_sys::Datum, bool, pg_sys::Oid)>,
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
                let qual_datums = unsafe { extract_const_datum(args) };
                return Some(AccelContext {
                    fn_oid: oid,
                    strategy: entry.strategy,
                    target_attno: attno,
                    qual_datums,
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
                let qual_datums = unsafe { extract_const_datum(args) };
                return Some(AccelContext {
                    fn_oid: oid,
                    strategy: entry.strategy,
                    target_attno: attno,
                    qual_datums,
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

/// Extract every constant (`Const`) datum from a function's argument list,
/// preserving the positional order in which the args were supplied. For
/// 2-arg spatial predicates like `ST_Contains(geom_col, $const_geom)`, this
/// returns a single-element Vec carrying the constant geometry; for
/// multi-arg ops like `ST_DWithin(geom_col, $const_geom, $threshold)` or
/// `ST_Hillshade(rast, cell_x, cell_y, sun_az, sun_alt)`, every Const node
/// is captured in argument order so the dispatcher can index `qual_datums[i]`
/// by position.
///
/// Each entry is `(datum, is_null, type_oid)`. `Var` arguments are skipped
/// (the `target_attno` extractor handles them). Returns an empty Vec if no
/// `Const` is present.
#[allow(clippy::cast_ptr_alignment)]
unsafe fn extract_const_datum(args: *mut pg_sys::List) -> Vec<(pg_sys::Datum, bool, pg_sys::Oid)> {
    let mut out = Vec::new();
    if args.is_null() {
        return out;
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
            // SAFETY: tag confirmed Const; reading constvalue, constisnull,
            // and consttype.
            let cst = node.cast::<pg_sys::Const>();
            let datum = unsafe { (*cst).constvalue };
            let is_null = unsafe { (*cst).constisnull };
            let typid = unsafe { (*cst).consttype };
            out.push((datum, is_null, typid));
        }
    }
    out
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

            // Pattern: Var <cmp> Const, or Const <cmp> Var with opcode flip.
            let (col_idx, const_val, flip_cmp) = unsafe { extract_var_const_pair(arg0, arg1) }?;
            let cmp_opcode = if flip_cmp {
                expr_compiler::flip_cmp_opcode(cmp_opcode)
            } else {
                cmp_opcode
            };

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

/// Extract a (Var, Const) pair from two nodes, returning
/// (col_idx, f64 value, comparison_needs_flip).
/// Handles both `Var <op> Const` and `Const <op> Var` orderings.
///
/// # Safety
///
/// Both nodes must be valid PG `Node *`.
unsafe fn extract_var_const_pair(
    arg0: *mut pg_sys::Node,
    arg1: *mut pg_sys::Node,
) -> Option<(u32, f64, bool)> {
    // Try Var, Const order.
    if let Some(col) = unsafe { node_as_var_col(arg0) }
        && let Some(val) = unsafe { node_as_const_f64(arg1) }
    {
        return Some((col, val, false));
    }
    // Try Const, Var order.
    if let Some(col) = unsafe { node_as_var_col(arg1) }
        && let Some(val) = unsafe { node_as_const_f64(arg0) }
    {
        return Some((col, val, true));
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
    crate::engine::otel::init();
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

    // When scanrelid > 0, PostgreSQL does not initialise custom_ps from
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
        (*state).accel.resident_proof = privdata.resident_proof;
    }

    // FunctionScan: short-circuit before allocating scan/aggregate/sort
    // executor states (the FunctionScan flow doesn't share their
    // ScanExecState / AggExecState shapes). init_state builds the TupleDesc,
    // dispatches once, and stashes the per-row cursor on a dedicated state.
    if privdata.gpu_strategy == GpuStrategy::FunctionScan {
        // SAFETY: node is a valid CustomScanState on the main backend thread.
        let exec = unsafe { function_scan::init_state(node) };
        // SAFETY: state points to our extended GpuAccelScanState; writing
        // executor pointer + zero counters.
        unsafe {
            (*state).accel.executor = exec;
            (*state).accel.rows_dispatched = 0;
            (*state).accel.batches_executed = 0;
            (*state).accel.dispatch_time_us = 0;
        }
        return;
    }

    // SrfTargetList: short-circuit before allocating scan/aggregate/sort
    // executor states. Drives the child scan via ExecProcNode (already
    // initialised into custom_ps[0] by the loop above) and expands the SRF
    // per row using a buffered cursor.
    if privdata.gpu_strategy == GpuStrategy::SrfTargetList {
        // SAFETY: node is a valid CustomScanState on the main backend thread.
        let exec =
            unsafe { srf_target_list::init_state(node, privdata.batch_size.max(1) as usize) };
        // SAFETY: state points to our extended GpuAccelScanState; writing
        // executor pointer + zero counters.
        unsafe {
            (*state).accel.executor = exec;
            (*state).accel.rows_dispatched = 0;
            (*state).accel.batches_executed = 0;
            (*state).accel.dispatch_time_us = 0;
        }
        return;
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
            // GpuPreAgg retired: no planner path-creator emits it and the
            // private-data reader rejects PreAgg payloads before this point.
            pgrx::error!("pg_accel: GpuPreAgg strategy retired; no planner path-creator emits it");
        } else if privdata.gpu_strategy == GpuStrategy::Agg {
            // Only the resident OLAP aggregate survives: the planner path
            // creator (resident_groupagg_path.rs) always serializes an OLAP
            // spec. Host-staged grouped/ungrouped/fused/vectorized agg
            // executors were retired with their planner injectors.
            let Some(olap_spec) = privdata.olap_agg else {
                pgrx::error!(
                    "pg_accel: non-resident GpuAgg retired; plan carries no OLAP agg spec"
                );
            };
            pgrx::debug1!("pg_accel: begin_custom_scan: resident OLAP Agg");
            Box::into_raw(Box::new(AggExecState::new_olap(olap_spec))).cast()
        } else if privdata.accel_strategy == AccelStrategy::GpuHashJoin {
            // Hash join: create a JoinExecState with hash join context.
            if let Some(reason) = privdata.hash_join_validation_error() {
                pgrx::error!(
                    "pg_accel: malformed GpuHashJoin private data ({reason}; outer_attno={}, inner_attno={}, key_type={}); refusing CPU fallback",
                    privdata.target_attno,
                    privdata.hash_inner_attno,
                    privdata.hash_key_type,
                );
            }
            let key_type = match privdata.hash_key_type {
                1 => PgaccelKeyType::Int64,
                2 => PgaccelKeyType::Float64,
                0 => PgaccelKeyType::Int32,
                _ => unreachable!("hash_join_validation_error rejects invalid key types"),
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
            exec.set_hash_join_count_mode(privdata.hash_count_only);
            exec.set_hash_join_resident_count_context(
                privdata.hash_resident_count,
                privdata.hash_outer_rel_oid,
                privdata.hash_inner_rel_oid,
            );

            // Initialize tlist mapping and temp slots for combined output.
            let custom_ps = (*node).custom_ps;
            if !privdata.hash_resident_count
                && !custom_ps.is_null()
                && pg_sys::list_length(custom_ps) >= 2
            {
                let outer_ps = pg_sys::list_nth(custom_ps, 0).cast::<pg_sys::PlanState>();
                let inner_ps = pg_sys::list_nth(custom_ps, 1).cast::<pg_sys::PlanState>();
                exec.init_hash_join_slots(cscan, outer_ps, inner_ps);
            }

            pgrx::debug1!(
                "pg_accel: begin_custom_scan: GpuHashJoin outer_attno={}, inner_attno={}, key_type={}, resident_count={}, tlist_map={}",
                privdata.target_attno,
                privdata.hash_inner_attno,
                privdata.hash_key_type,
                privdata.hash_resident_count,
                exec.tlist_map.len(),
            );
            Box::into_raw(exec).cast()
        } else if privdata.accel_strategy == AccelStrategy::GpuNestedLoopIneq {
            if let Some(reason) = privdata.nlj_validation_error() {
                pgrx::error!(
                    "pg_accel: malformed GpuNestedLoopIneq private data ({reason}; outer_attno={}, inner_lo_attno={}, inner_hi_attno={}, key_type={}, shape={}); refusing CPU fallback",
                    privdata.target_attno,
                    privdata.nlj_inner_lo_attno,
                    privdata.nlj_inner_hi_attno,
                    privdata.nlj_key_type,
                    privdata.nlj_shape,
                );
            }
            let key_type = match privdata.nlj_key_type {
                1 => PgaccelKeyType::Int64,
                2 => PgaccelKeyType::Float64,
                0 => PgaccelKeyType::Int32,
                _ => unreachable!("nlj_validation_error rejects invalid key types"),
            };
            let mut exec = Box::new(JoinExecState::new(
                AccelStrategy::GpuNestedLoopIneq,
                batch_size,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ));
            exec.set_nlj_between_context(
                privdata.target_attno,
                privdata.nlj_inner_lo_attno,
                privdata.nlj_inner_hi_attno,
                key_type,
            );

            let custom_ps = (*node).custom_ps;
            if !custom_ps.is_null() && pg_sys::list_length(custom_ps) >= 2 {
                let outer_ps = pg_sys::list_nth(custom_ps, 0).cast::<pg_sys::PlanState>();
                let inner_ps = pg_sys::list_nth(custom_ps, 1).cast::<pg_sys::PlanState>();
                exec.init_hash_join_slots(cscan, outer_ps, inner_ps);
            }

            pgrx::debug1!(
                "pg_accel: begin_custom_scan: GpuNestedLoopIneq BETWEEN outer_attno={}, inner_lo_attno={}, inner_hi_attno={}, key_type={}, tlist_map={}",
                privdata.target_attno,
                privdata.nlj_inner_lo_attno,
                privdata.nlj_inner_hi_attno,
                privdata.nlj_key_type,
                exec.tlist_map.len(),
            );
            Box::into_raw(exec).cast()
        } else if privdata.gpu_strategy == GpuStrategy::Join {
            pgrx::error!(
                "pg_accel: non-hash join strategy {:?} reached Custom Scan begin; \
                 planner must decline until a complete GPU join executor is wired",
                privdata.accel_strategy,
            );
        } else if privdata.gpu_strategy == GpuStrategy::Window {
            pgrx::debug1!(
                "pg_accel: begin_custom_scan: Window strategy, {} specs, scan_relid={}",
                privdata.window_specs.len(),
                privdata.window_scan_relid
            );
            let mut exec = Box::new(WindowExecState::new(batch_size, privdata.window_specs));

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
            pgrx::error!("pg_accel: GpuSort strategy retired; no planner path-creator emits it");
        } else {
            // Use fn_oid + target_attno + accel_strategy from custom_private
            // (serialized by the planner hook). Fall back to qual discovery
            // only if the planner didn't serialize them (fn_oid == InvalidOid).
            // Always scan the qual tree to extract every constant datum
            // argument (e.g. the fixed geometry in ST_Contains(col, $const)
            // or the threshold in ST_DWithin(col, $const, $threshold)).
            // The planner serializes fn_oid/attno/strategy into custom_private,
            // but qual_datums cannot be serialized — they must be extracted
            // at executor init time from the qual expressions.
            //
            // SAFETY: cscan is a valid CustomScan; plan.qual was set by
            // make_custom_scan_plan via extract_actual_clauses.
            // GpuExpr doesn't need qual tree discovery (no fn_oid or
            // qual_datums). Skip find_accel_context_in_qual which calls
            // set_opfuncid on OpExpr nodes and can crash for plain WHERE.
            let (strategy, fn_oid, target_attno, qual_datums) =
                if privdata.accel_strategy == AccelStrategy::GpuExpr {
                    (AccelStrategy::GpuExpr, pg_sys::Oid::INVALID, 0, Vec::new())
                } else {
                    let qual_ctx = find_accel_context_in_qual((*cscan).scan.plan.qual);
                    if privdata.fn_oid == pg_sys::Oid::INVALID {
                        // Fallback: discover everything from qual list.
                        qual_ctx.map_or(
                            (
                                AccelStrategy::GpuSpatial,
                                pg_sys::Oid::INVALID,
                                0,
                                Vec::new(),
                            ),
                            |ctx| (ctx.strategy, ctx.fn_oid, ctx.target_attno, ctx.qual_datums),
                        )
                    } else {
                        // Use planner-serialized fn_oid/attno/strategy, but take
                        // qual_datums from the qual tree discovery.
                        let qds = qual_ctx.map(|ctx| ctx.qual_datums).unwrap_or_default();
                        (
                            privdata.accel_strategy,
                            privdata.fn_oid,
                            privdata.target_attno,
                            qds,
                        )
                    }
                };

            pgrx::debug1!(
                "pg_accel: begin_custom_scan: accel strategy = {:?}",
                strategy
            );

            // GpuExpr compiles the qual to GPU bytecode/template form and
            // clears PostgreSQL's ExprState so the selected Custom Scan never
            // evaluates the predicate on CPU.
            let qual = if strategy == AccelStrategy::GpuExpr {
                std::ptr::null_mut()
            } else {
                (*node).ss.ps.qual
            };
            let econtext = (*node).ss.ps.ps_ExprContext;
            let mut exec = Box::new(ScanExecState::new(strategy, batch_size, qual, econtext));
            if (*state).accel.parallel_worker_number >= 0 {
                exec.mark_parallel_worker(
                    (*state).accel.parallel_worker_number,
                    (*state).accel.dsm_flags,
                );
            }

            // For GpuExpr strategy, compile the qual list to GPU bytecode.
            // Template match is tried first (fastest), then bytecode. If
            // neither works, the planner should not have selected GpuExpr.
            if strategy == AccelStrategy::GpuExpr {
                // SAFETY: cscan.scan.plan.qual is a valid List * set by
                // make_custom_scan_plan. tupdesc provides num_cols.
                let plan_qual = (*cscan).scan.plan.qual;
                let scanrelid = (*cscan).scan.scanrelid;
                // For direct scan (scanrelid > 0), use the opened relation's
                // TupleDesc. The Custom Scan result slot may be projected and
                // can be too narrow for base-table Var references.
                let num_cols = {
                    if scanrelid > 0 {
                        let rel = (*node).ss.ss_currentRelation;
                        if rel.is_null() || (*rel).rd_att.is_null() {
                            32
                        } else {
                            (*(*rel).rd_att).natts as usize
                        }
                    } else {
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
                if matches!(
                    compiled,
                    crate::engine::expr_compiler::CompiledExpr::DeferToPg
                ) {
                    pgrx::error!(
                        "pg_accel: GpuExpr cannot compile this qual to a GPU program; refusing CPU fallback"
                    );
                }
                exec.set_compiled_expr(compiled);
                (*node).ss.ps.qual = std::ptr::null_mut();

                // For GpuExpr with scanrelid > 0 (direct heap scan),
                // initialise a table scan so fill_batch can pull tuples.
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
                    exec.set_scan_desc(sd, rel);
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
                let n_qual = qual_datums.len();
                exec.set_gpu_context(fn_oid, target_attno, qual_datums);
                pgrx::debug1!(
                    "pg_accel: set_gpu_context fn_oid={}, attno={}, n_qual_datums={}",
                    u32::from(fn_oid),
                    target_attno,
                    n_qual,
                );

                // Detect GiST index child. When the child is a GiST
                // IndexScan, bbox filtering has already been done by the
                // index, so the GPU path can skip Layer 1 without invoking
                // any CPU predicate recheck.
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
        (*state).accel.dsm_counters_recorded = false;
        (*state).accel.parallel_agg_participants = 0;
        (*state).accel.parallel_agg_active_participants = 0;
        (*state).accel.parallel_agg_rows_dispatched = 0;
        (*state).accel.parallel_agg_batches_executed = 0;
        (*state).accel.parallel_agg_dispatch_time_us = 0;
    }
}

/// Called by the executor on the main backend thread.
unsafe extern "C-unwind" fn exec_custom_scan(
    node: *mut pg_sys::CustomScanState,
) -> *mut pg_sys::TupleTableSlot {
    let _span = tracing::debug_span!("ffi.exec_custom_scan").entered();
    let state = node.cast::<GpuAccelScanState>();

    if !gucs::enabled() {
        pgrx::error!(
            "pg_accel: Custom Scan reached execution while pg_accel.enabled=off; refusing CPU passthrough"
        );
    }

    // SAFETY: executor was allocated in begin_custom_scan.
    let executor = unsafe { (*state).accel.executor };
    if executor.is_null() {
        pgrx::error!("pg_accel: Custom Scan has no GPU executor; refusing CPU passthrough");
    }

    // SAFETY: state points to our GpuAccelScanState with valid accel fields.
    let gpu_strategy = GpuStrategy::decode(unsafe { (*state).accel.strategy });

    // FunctionScan strategy (Phase 2 F3): one-shot dispatch + per-output-row
    // tuple emission. Routed through the dedicated function_scan module which
    // owns the cursor + emitted-Datum buffer.
    if gpu_strategy == GpuStrategy::FunctionScan {
        // SAFETY: executor was Box::into_raw'd as FunctionScanExecState in
        // begin_custom_scan. Per-row counters live on the same struct.
        let slot = unsafe { function_scan::next_tuple(node, executor) };
        // Propagate the per-row counter to GpuAccelState so EXPLAIN ANALYZE
        // sees the right "rows" attribute on the FunctionScan node.
        // SAFETY: state is our extended GpuAccelScanState, see top of fn.
        unsafe {
            (*state).accel.rows_dispatched = function_scan::rows_dispatched(executor);
        }
        return slot;
    }

    // SrfTargetList strategy (Phase 2 F3 follow-up): per-input-row dispatch
    // + per-output-row tuple emission. Routed through the dedicated
    // srf_target_list module which drives the child scan via ExecProcNode.
    if gpu_strategy == GpuStrategy::SrfTargetList {
        // SAFETY: executor was Box::into_raw'd as SrfTargetListExecState in
        // begin_custom_scan.
        let slot = unsafe { srf_target_list::next_tuple(node, executor) };
        // SAFETY: propagate row counter for EXPLAIN ANALYZE.
        unsafe {
            (*state).accel.rows_dispatched = srf_target_list::rows_dispatched(executor);
            (*state).accel.batches_executed = srf_target_list::batches_executed(executor);
            (*state).accel.dispatch_time_us = srf_target_list::dispatch_time_us(executor);
        }
        return slot;
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
            GpuStrategy::PreAgg => pgrx::error!(
                "pg_accel: GpuPreAgg strategy retired; begin_custom_scan rejects it before exec"
            ),
            GpuStrategy::Sort => pgrx::error!(
                "pg_accel: GpuSort strategy retired; begin_custom_scan rejects it before exec"
            ),
            GpuStrategy::Window => &mut *executor.cast::<WindowExecState>(),
            GpuStrategy::Join => &mut *executor.cast::<JoinExecState>(),
            GpuStrategy::FunctionScan => unreachable!(
                "FunctionScan handled by short-circuit above; dyn ExecutorState match unreachable"
            ),
            GpuStrategy::SrfTargetList => unreachable!(
                "SrfTargetList handled by short-circuit above; dyn ExecutorState match unreachable"
            ),
        };
        let slot = dyn_state.exec(node);
        // SAFETY: state points to our GpuAccelScanState; writing counters for
        // EXPLAIN ANALYZE on the main backend thread.
        (*state).accel.rows_dispatched = dyn_state.rows_dispatched();
        (*state).accel.batches_executed = dyn_state.batches_executed();
        (*state).accel.dispatch_time_us = dyn_state.dispatch_time_us();
        dsm::record_parallel_agg_counters_once(
            state,
            gpu_strategy,
            dyn_state.rows_dispatched(),
            dyn_state.batches_executed(),
            dyn_state.dispatch_time_us(),
        );
        slot
    };

    // Apply ps_ProjInfo projection when the scan slot can hold a broader
    // tuple than plan.targetlist. ExecInitCustomScan builds pi_state for
    // this map; GpuExpr reaches this path after the GPU predicate has
    // selected rows, with PostgreSQL's CPU qual cleared.
    if !result.is_null() && matches!(gpu_strategy, GpuStrategy::Scan | GpuStrategy::Window) {
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
            // SAFETY: proj_info is valid and its expression context now
            // points at the tuple produced by the custom scan executor.
            return unsafe { pg_sys::ExecProject(proj_info) };
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
            let gpu_strategy = GpuStrategy::decode((*state).accel.strategy);

            if gpu_strategy == GpuStrategy::Join {
                // SAFETY: executor was Box::into_raw'd as JoinExecState.
                let _ = Box::from_raw((*state).accel.executor.cast::<JoinExecState>());
            } else if GpuStrategy::decode((*state).accel.strategy) == GpuStrategy::FunctionScan {
                // FunctionScan owns its own boxed state (FunctionScanExecState);
                // delegate to the dedicated drop helper.
                // SAFETY: executor was Box::into_raw'd as FunctionScanExecState.
                function_scan::drop_state((*state).accel.executor);
            } else if GpuStrategy::decode((*state).accel.strategy) == GpuStrategy::SrfTargetList {
                // SrfTargetList owns its own boxed state (SrfTargetListExecState);
                // delegate to the dedicated drop helper. The child PlanState is
                // ended below by the standard custom_ps walk.
                // SAFETY: executor was Box::into_raw'd as SrfTargetListExecState.
                srf_target_list::drop_state((*state).accel.executor);
            } else {
                let gpu_strategy = GpuStrategy::decode((*state).accel.strategy);
                if gpu_strategy == GpuStrategy::PreAgg {
                    // GpuPreAgg retired: begin_custom_scan errors before an
                    // executor can be allocated, so a non-null executor with
                    // PreAgg strategy is a state-corruption bug.
                    pgrx::error!(
                        "pg_accel: GpuPreAgg strategy retired; no PreAgg executor state can exist"
                    );
                } else if gpu_strategy == GpuStrategy::Agg {
                    // SAFETY: executor was Box::into_raw'd as AggExecState.
                    // The resident OLAP agg owns no heap scan descriptors.
                    let agg = Box::from_raw((*state).accel.executor.cast::<AggExecState>());
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
                    // GpuSort retired: begin_custom_scan errors before an
                    // executor can be allocated, so a non-null executor with
                    // Sort strategy is a state-corruption bug.
                    pgrx::error!(
                        "pg_accel: GpuSort strategy retired; no Sort executor state can exist"
                    );
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
        let gpu_strategy = GpuStrategy::decode((*state).accel.strategy);
        let batch_size = if (*state).accel.batch_size > 0 {
            (*state).accel.batch_size as usize
        } else {
            256
        };

        // FunctionScan: just reset the cursor; do not rebuild the buffered
        // dispatch (constant args ⇒ same output is correct).
        if gpu_strategy == GpuStrategy::FunctionScan {
            function_scan::rescan((*state).accel.executor);
            return;
        }

        // SrfTargetList: clear the per-row expansion buffer; the surrounding
        // rescan_custom_scan walks custom_ps and rescans the child plan, so the
        // next next_tuple call will re-pull the first input row.
        if gpu_strategy == GpuStrategy::SrfTargetList {
            srf_target_list::rescan((*state).accel.executor);
            return;
        }

        if gpu_strategy == GpuStrategy::Agg {
            // Rescan: rebuild the resident OLAP executor from its spec. Only
            // the resident OLAP agg survives — begin_custom_scan rejects any
            // Agg plan without an OLAP spec before an executor can exist.
            let old_olap = if (*state).accel.executor.is_null() {
                None
            } else {
                // SAFETY: executor was Box::into_raw'd as AggExecState.
                let old = Box::from_raw((*state).accel.executor.cast::<AggExecState>());
                (*state).accel.executor = std::ptr::null_mut();
                old.olap_spec()
            };
            let Some(olap) = old_olap else {
                pgrx::error!(
                    "pg_accel: non-resident GpuAgg retired; rescan found no OLAP agg spec"
                );
            };
            (*state).accel.executor = Box::into_raw(Box::new(AggExecState::new_olap(olap))).cast();
        } else if gpu_strategy == GpuStrategy::Sort {
            // GpuSort retired: begin_custom_scan errors before an executor
            // can be allocated, so rescan can never see this strategy.
            pgrx::error!("pg_accel: GpuSort strategy retired; rescan cannot rebuild it");
        } else if gpu_strategy == GpuStrategy::Window {
            let window_specs = if (*state).accel.executor.is_null() {
                vec![]
            } else {
                // SAFETY: executor was Box::into_raw'd as WindowExecState.
                let old = Box::from_raw((*state).accel.executor.cast::<WindowExecState>());
                old.specs().to_vec()
            };
            let exec = Box::new(WindowExecState::new(batch_size, window_specs));
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
            let (qual, econtext, old_strategy, old_fn_oid, old_attno, old_qual_datums) =
                if (*state).accel.executor.is_null() {
                    (
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        AccelStrategy::GpuSpatial,
                        pg_sys::InvalidOid,
                        0i32,
                        Vec::new(),
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
                        old.qual_datums().to_vec(),
                    )
                };
            let mut exec = Box::new(ScanExecState::new(old_strategy, batch_size, qual, econtext));
            if (*state).accel.parallel_worker_number >= 0 {
                exec.mark_parallel_worker(
                    (*state).accel.parallel_worker_number,
                    (*state).accel.dsm_flags,
                );
            }
            if old_fn_oid != pg_sys::InvalidOid {
                // SAFETY: old_fn_oid was validated during the initial
                // set_gpu_context call. We are on the main backend thread.
                exec.set_gpu_context(old_fn_oid, old_attno, old_qual_datums);
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
        (*state).accel.dsm_counters_recorded = false;
        (*state).accel.parallel_agg_participants = 0;
        (*state).accel.parallel_agg_active_participants = 0;
        (*state).accel.parallel_agg_rows_dispatched = 0;
        (*state).accel.parallel_agg_batches_executed = 0;
        (*state).accel.parallel_agg_dispatch_time_us = 0;
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

#[cfg(feature = "pg_test")]
mod tests;
