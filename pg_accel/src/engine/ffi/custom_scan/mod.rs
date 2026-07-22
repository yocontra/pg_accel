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
use crate::engine::executor::agg::AggExecState;
use crate::engine::executor::raster::RasterExecState;
use crate::engine::registry::AccelStrategy;
use crate::engine::residency::ResidentProofSnapshot;
use crate::engine::stats;

mod dsm;
mod explain;
mod private_data;

use explain::{explain_custom_scan, resolve_thread_count};
#[cfg(feature = "pg_test")]
use private_data::CustomPrivateData;
pub(super) use private_data::append_agg_query_plan;
use private_data::deserialize_agg_query_path_contract;
use private_data::{
    MAX_PLAN_BATCH_SIZE, PlanExecMethod, RESIDENT_PROOF_TRAILER_INTS, append_plan_wire_footer,
    validate_custom_private_wire, validate_projection_tuple_desc,
};
pub(in crate::engine::ffi) use private_data::{
    append_raster_exec_plan, append_resident_proof_snapshot, raster_resident_proof,
    resident_proof_default_for_strategy,
};
use private_data::{
    deserialize_custom_private, deserialize_raster_exec_path_plan,
    deserialize_resident_proof_snapshot,
};

// ---------------------------------------------------------------------------
// Retired FunctionScan output-shape discriminants
// ---------------------------------------------------------------------------

/// Mirror of [`crate::engine::registry::OutputShape`] using a flat integer
/// representation for validating retired FunctionScan plan-private payloads.
///
/// The mapping is fixed by the `to_i32` / `from_i32` pair below and is the
/// compatibility wire format. No executable FunctionScan method table remains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OutputShapeDisc {
    /// One Datum per input row (`OutputShape::Scalar`).
    Scalar = 0,
    /// Fixed `field_count` Datums per input row (`OutputShape::Record`).
    Record = 1,
    /// CSR-style variable-length per input row (`OutputShape::VarLen`).
    VarLen = 2,
}

impl OutputShapeDisc {
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
    /// Childless resident PostGIS Raster transform backed by an exact RQS2
    /// contract and generation-stamped reconstructed-output artifact.
    Raster = 8,
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
            8 => Some(Self::Raster),
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
            Self::Raster => c"GpuRaster",
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
    pub(super) exec_method: i32,
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
    /// Opaque pointer to the selected `AggExecState` or `RasterExecState`.
    /// Set in `begin_custom_scan` and freed in `end_custom_scan`.
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
// SAFETY: CustomPathMethods is a const-initialized vtable containing only a
// static name pointer and immutable function pointers; PostgreSQL never mutates it.
unsafe impl Sync for SyncPathMethods {}

#[repr(transparent)]
struct SyncScanMethods(pg_sys::CustomScanMethods);
// SAFETY: CustomScanMethods is a const-initialized vtable containing only a
// static name pointer and immutable function pointers; PostgreSQL never mutates it.
unsafe impl Sync for SyncScanMethods {}

#[repr(transparent)]
struct SyncExecMethods(pg_sys::CustomExecMethods);
// SAFETY: CustomExecMethods is a const-initialized vtable containing only a
// static name pointer and immutable function pointers; PostgreSQL never mutates it.
unsafe impl Sync for SyncExecMethods {}

// ---------------------------------------------------------------------------
// Static vtables
// ---------------------------------------------------------------------------

static AGG_PATH_METHODS: SyncPathMethods = SyncPathMethods(pg_sys::CustomPathMethods {
    CustomName: c"GpuAccelAgg".as_ptr(),
    PlanCustomPath: Some(plan_custom_path_agg),
    ReparameterizeCustomPathByChild: None,
});

static RASTER_PATH_METHODS: SyncPathMethods = SyncPathMethods(pg_sys::CustomPathMethods {
    CustomName: c"GpuAccelRaster".as_ptr(),
    PlanCustomPath: Some(plan_custom_path_raster),
    ReparameterizeCustomPathByChild: None,
});

static AGG_SCAN_METHODS: SyncScanMethods = SyncScanMethods(pg_sys::CustomScanMethods {
    CustomName: c"GpuAccelAgg".as_ptr(),
    CreateCustomScanState: Some(create_agg_state),
});

static RASTER_SCAN_METHODS: SyncScanMethods = SyncScanMethods(pg_sys::CustomScanMethods {
    CustomName: c"GpuAccelRaster".as_ptr(),
    CreateCustomScanState: Some(create_raster_state),
});

macro_rules! exec_methods {
    ($name:ident, $label:literal) => {
        static $name: SyncExecMethods = SyncExecMethods(pg_sys::CustomExecMethods {
            CustomName: $label.as_ptr(),
            BeginCustomScan: Some(begin_custom_scan),
            ExecCustomScan: Some(exec_custom_scan),
            EndCustomScan: Some(end_custom_scan),
            ReScanCustomScan: Some(rescan_custom_scan),
            MarkPosCustomScan: None,
            RestrPosCustomScan: None,
            EstimateDSMCustomScan: Some(dsm::estimate_dsm_custom_scan),
            InitializeDSMCustomScan: Some(dsm::initialize_dsm_custom_scan),
            ReInitializeDSMCustomScan: Some(dsm::reinitialize_dsm_custom_scan),
            InitializeWorkerCustomScan: Some(dsm::initialize_worker_custom_scan),
            ShutdownCustomScan: Some(dsm::shutdown_custom_scan),
            ExplainCustomScan: Some(explain_custom_scan),
        });
    };
}

exec_methods!(AGG_EXEC_METHODS, c"GpuAccelAgg");
exec_methods!(RASTER_EXEC_METHODS, c"GpuAccelRaster");

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Pointer to agg `CustomPathMethods` vtable.
#[inline]
#[must_use]
pub fn agg_path_methods() -> *const pg_sys::CustomPathMethods {
    &raw const AGG_PATH_METHODS.0
}

/// Pointer to childless raster `CustomPathMethods` vtable.
#[inline]
#[must_use]
pub fn raster_path_methods() -> *const pg_sys::CustomPathMethods {
    &raw const RASTER_PATH_METHODS.0
}

/// Register Custom Scan methods with PostgreSQL. Must be called from `_PG_init`.
pub fn register() {
    // SAFETY: RegisterCustomScanMethods stores pointers to our static vtables
    // which live for the entire process lifetime. Called on main thread during
    // extension loading.
    unsafe {
        pg_sys::RegisterCustomScanMethods(&raw const AGG_SCAN_METHODS.0);
        pg_sys::RegisterCustomScanMethods(&raw const RASTER_SCAN_METHODS.0);
    }
}

// ---------------------------------------------------------------------------
// PlanCustomPath callbacks
// ---------------------------------------------------------------------------

/// Convert an exact childless RQS2 path into its distinct raster CustomScan.
///
/// # Safety
/// Called by the PostgreSQL planner on the main backend thread.
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn plan_custom_path_raster(
    _root: *mut pg_sys::PlannerInfo,
    _rel: *mut pg_sys::RelOptInfo,
    best_path: *mut pg_sys::CustomPath,
    tlist: *mut pg_sys::List,
    clauses: *mut pg_sys::List,
    custom_plans: *mut pg_sys::List,
) -> *mut pg_sys::Plan {
    let _span = tracing::debug_span!("ffi.plan_custom_path_raster").entered();
    // SAFETY: best_path and its private list are planner-owned values supplied
    // to this exact CustomPathMethods callback.
    let path_private = unsafe { (*best_path).custom_private };
    // SAFETY: the selected raster path owns a List<Integer> emitted by pg_accel.
    let raster_plan = unsafe { deserialize_raster_exec_path_plan(path_private) }
        .unwrap_or_else(|error| pgrx::error!("pg_accel: malformed raster RQS2 path: {error}"));

    // SAFETY: a non-null `custom_plans` is the planner-owned List supplied to
    // this callback and remains valid while its length is inspected.
    if !custom_plans.is_null() && unsafe { pg_sys::list_length(custom_plans) } != 0 {
        pgrx::error!("pg_accel: childless raster path unexpectedly has child plans");
    }
    // SAFETY: a non-null `clauses` is the planner-owned List supplied to this
    // callback and remains valid while its length is inspected.
    if !clauses.is_null() && unsafe { pg_sys::list_length(clauses) } != 0 {
        pgrx::error!("pg_accel: childless raster path unexpectedly has executor clauses");
    }
    // SAFETY: a non-null `tlist` is the planner-owned target List supplied to
    // this callback and remains valid while its cardinality is checked.
    if tlist.is_null() || unsafe { pg_sys::list_length(tlist) } != 1 {
        pgrx::error!("pg_accel: childless raster path must have exactly one output expression");
    }
    // SAFETY: the target list was checked to contain exactly one planner-owned node.
    let target = unsafe { pg_sys::list_nth(tlist, 0).cast::<pg_sys::TargetEntry>() };
    // SAFETY: the singleton list entry is a planner-owned TargetEntry; the
    // null check short-circuits before its fields are inspected.
    if target.is_null() || unsafe { (*target).resjunk || (*target).expr.is_null() } {
        pgrx::error!("pg_accel: childless raster path has an invalid output target");
    }
    // SAFETY: target is a valid TargetEntry with a non-null expression.
    let output_type = unsafe { pg_sys::exprType((*target).expr.cast()) };
    if u32::from(output_type) != raster_plan.spec().raster_type_oid {
        pgrx::error!("pg_accel: raster output target type does not match its RQS2 contract");
    }

    // SAFETY: palloc0 returns zeroed memory in CurrentMemoryContext.
    let cscan = unsafe {
        pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomScan>()).cast::<pg_sys::CustomScan>()
    };
    // SAFETY: cscan is freshly allocated; all source pointers are planner-owned.
    unsafe {
        (*cscan).scan.plan.type_ = pg_sys::NodeTag::T_CustomScan;
        (*cscan).custom_scan_tlist = pg_sys::copyObjectImpl(tlist.cast()).cast();
        (*cscan).scan.plan.targetlist = pg_sys::copyObjectImpl(tlist.cast()).cast();
        (*cscan).scan.plan.qual = std::ptr::null_mut();
        (*cscan).scan.plan.startup_cost = (*best_path).path.startup_cost;
        (*cscan).scan.plan.total_cost = (*best_path).path.total_cost;
        (*cscan).scan.plan.plan_rows = (*best_path).path.rows;
        (*cscan).custom_plans = std::ptr::null_mut();
        (*cscan).flags = (*best_path).flags;
        (*cscan).scan.scanrelid = 0;
        (*cscan).methods = &raw const RASTER_SCAN_METHODS.0;

        let mut private: *mut pg_sys::List = std::ptr::null_mut();
        private = pg_sys::lappend(
            private,
            pg_sys::makeInteger(GpuStrategy::Raster as c_int).cast(),
        );
        private = pg_sys::lappend(private, pg_sys::makeInteger(gucs::min_batch_size()).cast());
        private = pg_sys::lappend(private, pg_sys::makeInteger(resolve_thread_count()).cast());
        private = pg_sys::lappend(
            private,
            pg_sys::makeInteger(raster_plan.spec().function_oid as c_int).cast(),
        );
        private = pg_sys::lappend(
            private,
            pg_sys::makeInteger(raster_plan.spec().raster_attno).cast(),
        );
        private = pg_sys::lappend(
            private,
            pg_sys::makeInteger(AccelStrategy::GpuRaster as c_int).cast(),
        );
        private = append_raster_exec_plan(private, &raster_plan);
        seal_custom_scan_private(cscan, private, path_private, GpuStrategy::Raster);
    }
    cscan.cast()
}

/// Append the proof carried by a planner CustomPath and seal the v2 frame.
///
/// # Safety
///
/// Must run in a valid PostgreSQL planner memory context. `path_private`, when
/// non-null, must be a `List<Integer>` emitted by pg_accel.
unsafe fn append_path_resident_proof(
    list: *mut pg_sys::List,
    path_private: *mut pg_sys::List,
    strategy: GpuStrategy,
) -> *mut pg_sys::List {
    if path_private.is_null() {
        pgrx::error!("pg_accel: selected CustomPath is missing private data and resident proof");
    }
    // SAFETY: path_private is non-null and is owned by the selected CustomPath.
    let proof = unsafe { deserialize_resident_proof_snapshot(path_private) }
        .unwrap_or_else(|| pgrx::error!("pg_accel: selected CustomPath is missing resident proof"));
    // SAFETY: list and proof are planner-owned values in the active memory context.
    let list = unsafe { append_resident_proof_snapshot(list, proof) };
    let method = PlanExecMethod::for_strategy(strategy).unwrap_or_else(|| {
        pgrx::error!(
            "pg_accel: retired GPU strategy {strategy:?} has no CustomExecMethods identity"
        )
    });
    // SAFETY: list is a planner-owned List<Integer> with a canonical proof trailer.
    unsafe { append_plan_wire_footer(list, method) }
}

/// Attach proof/framing and validate the complete CustomScan wire while the
/// planner's target list is still available.
unsafe fn seal_custom_scan_private(
    cscan: *mut pg_sys::CustomScan,
    list: *mut pg_sys::List,
    path_private: *mut pg_sys::List,
    strategy: GpuStrategy,
) {
    let method = PlanExecMethod::for_strategy(strategy).unwrap_or_else(|| {
        pgrx::error!("pg_accel: retired GPU strategy {strategy:?} cannot seal a plan")
    });
    // SAFETY: all pointers are planner-owned and strategy selected this path.
    let list = unsafe { append_path_resident_proof(list, path_private, strategy) };
    // SAFETY: cscan is the planner-owned node being sealed.
    unsafe {
        (*cscan).custom_private = list;
    }
    // SAFETY: cscan now owns the complete List<Integer> frame and both target lists.
    unsafe { validate_custom_private_wire(cscan, method) }.unwrap_or_else(|error| {
        pgrx::error!("pg_accel: planner emitted invalid CustomScan private data: {error}");
    });
}

/// Convert a strict childless descriptor aggregate path into a `CustomScan`.
///
/// The AQS3/AOP2 contract is copied into framed plan-private data. No child
/// plan or retired aggregate metadata is accepted.
///
/// # Safety
///
/// Called by the PostgreSQL planner on the main backend thread.
#[pgrx::pg_guard]
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

        let path_priv = (*best_path).custom_private;
        let (spec, projection) =
            deserialize_agg_query_path_contract(path_priv).unwrap_or_else(|error| {
                pgrx::error!("pg_accel: malformed aggregate AQS3/AOP2 path contract: {error}")
            });
        if !custom_plans.is_null() && pg_sys::list_length(custom_plans) != 0 {
            pgrx::error!("pg_accel: descriptor aggregate path unexpectedly has child plans");
        }

        // The descriptor executor materializes the complete output itself.
        // Preserve the planner's ordered expressions on both target lists so
        // the wire projection binds to the concrete tuple descriptor.
        // SAFETY: copyObjectImpl deep-copies the list in CurrentMemoryContext.
        (*cscan).custom_scan_tlist = pg_sys::copyObjectImpl(tlist.cast()).cast();
        (*cscan).scan.plan.targetlist = pg_sys::copyObjectImpl(tlist.cast()).cast();

        (*cscan).scan.plan.qual = pg_sys::extract_actual_clauses(clauses, false);
        (*cscan).scan.plan.startup_cost = (*best_path).path.startup_cost;
        (*cscan).scan.plan.total_cost = (*best_path).path.total_cost;
        (*cscan).scan.plan.plan_rows = (*best_path).path.rows;
        (*cscan).custom_plans = std::ptr::null_mut();
        (*cscan).flags = (*best_path).flags;
        (*cscan).scan.scanrelid = 0;
        (*cscan).methods = &raw const AGG_SCAN_METHODS.0;

        // Serialize the common header followed immediately by AQS3/AOP2.
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
        list = append_agg_query_plan(list, &spec, &projection, cscan);

        seal_custom_scan_private(cscan, list, path_priv, GpuStrategy::Agg);
    }

    tracing::info!("plan_custom_path_agg: end");
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
unsafe fn create_custom_scan_state(
    cscan: *mut pg_sys::CustomScan,
    method: PlanExecMethod,
    exec_methods: *const pg_sys::CustomExecMethods,
) -> *mut pg_sys::Node {
    let _span = tracing::debug_span!("ffi.create_custom_scan_state").entered();
    if cscan.is_null() {
        pgrx::error!("pg_accel: CreateCustomScanState received a null plan");
    }
    // Validate the full frame and the serialized method identity before an
    // executor state exists. This prevents a copied/corrupt plan from selecting
    // one vtable while its payload names another concrete Rust state type.
    // SAFETY: cscan was checked non-null and is owned by the executor callback.
    unsafe { validate_custom_private_wire(cscan, method) }
        .unwrap_or_else(|error| pgrx::error!("pg_accel: invalid CustomScan private data: {error}"));
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
        (*state).css.methods = exec_methods;
        (*state).accel.exec_method = method as i32;
        (*state).accel.executor = std::ptr::null_mut();
        (*state).accel.parallel_worker_number = -1;
        (*state).accel.resident_proof = ResidentProofSnapshot::not_proven();

        // All scan strategies use VirtualTupleTableSlot (PG default).
        // ExecForceStoreHeapTuple triggers heap_deform_tuple (~50ns) which
        // populates tts_values/tts_isnull for correct aggregate evaluation.
    }

    state.cast()
}

macro_rules! create_state_callback {
    ($function:ident, $method:expr, $exec_methods:ident) => {
        #[pgrx::pg_guard]
        unsafe extern "C-unwind" fn $function(cscan: *mut pg_sys::CustomScan) -> *mut pg_sys::Node {
            // SAFETY: PostgreSQL invokes this callback with the CustomScan node
            // registered for this exact method table.
            unsafe { create_custom_scan_state(cscan, $method, &raw const $exec_methods.0) }
        }
    };
}

create_state_callback!(create_agg_state, PlanExecMethod::Agg, AGG_EXEC_METHODS);
create_state_callback!(
    create_raster_state,
    PlanExecMethod::Raster,
    RASTER_EXEC_METHODS
);

/// Reset per-run counters while preserving the selected plan contract.
fn reset_observability(state: &mut GpuAccelState) {
    state.rows_dispatched = 0;
    state.batches_executed = 0;
    state.dispatch_time_us = 0;
    state.dsm_counters_recorded = false;
    state.parallel_agg_participants = 0;
    state.parallel_agg_active_participants = 0;
    state.parallel_agg_rows_dispatched = 0;
    state.parallel_agg_batches_executed = 0;
    state.parallel_agg_dispatch_time_us = 0;
}

/// Initialize one childless resident executor.
///
/// # Safety
/// Called by PostgreSQL for one of the two registered method tables.
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn begin_custom_scan(
    node: *mut pg_sys::CustomScanState,
    _estate: *mut pg_sys::EState,
    eflags: c_int,
) {
    crate::engine::otel::init();
    stats::record_query_accelerated();

    let state = node.cast::<GpuAccelScanState>();
    // SAFETY: ExecInitCustomScan installed this plan for our extended state.
    let cscan = unsafe { (*node).ss.ps.plan.cast::<pg_sys::CustomScan>() };
    // SAFETY: create_custom_scan_state stored a validated method identity.
    let method_raw = unsafe { (*state).accel.exec_method };
    let method = PlanExecMethod::from_i32(method_raw)
        .unwrap_or_else(|| pgrx::error!("pg_accel: invalid executor-method identity {method_raw}"));
    // SAFETY: cscan is the initialized plan owned by this state.
    let private = unsafe { deserialize_custom_private(cscan, method) };

    if !matches!(private.gpu_strategy, GpuStrategy::Agg | GpuStrategy::Raster) {
        pgrx::error!(
            "pg_accel: retired {:?} strategy has no registered executor",
            private.gpu_strategy
        );
    }
    // Both selected architectures source data directly from pinned resident
    // relations. A child plan would be a second, host-staged source of truth.
    // SAFETY: node and cscan are live executor/planner objects.
    if unsafe {
        (!(*node).custom_ps.is_null() && pg_sys::list_length((*node).custom_ps) != 0)
            || (!(*cscan).custom_plans.is_null() && pg_sys::list_length((*cscan).custom_plans) != 0)
    } {
        pgrx::error!("pg_accel: resident Custom Scan unexpectedly has child plans");
    }

    if let Some(projection) = &private.agg_output_projection {
        // SAFETY: ExecInitCustomScan initialized both slots.
        let slots = unsafe {
            [
                ("scan", (*node).ss.ss_ScanTupleSlot),
                ("result", (*node).ss.ps.ps_ResultTupleSlot),
            ]
        };
        for (name, slot) in slots {
            if slot.is_null() {
                pgrx::error!("pg_accel: aggregate output projection has no {name} tuple slot");
            }
            // SAFETY: the non-null slot is owned by this initialized state.
            unsafe { validate_projection_tuple_desc(projection, (*slot).tts_tupleDescriptor) }
                .unwrap_or_else(|error| {
                    pgrx::error!(
                        "pg_accel: aggregate output projection/{name} TupleDesc mismatch: {error}"
                    );
                });
        }
    }

    // SAFETY: state is our extended allocation.
    unsafe {
        (*state).accel.strategy = private.gpu_strategy as c_int;
        (*state).accel.batch_size = private.batch_size;
        (*state).accel.expected_threads = private.expected_threads;
        (*state).accel.resident_proof = private.resident_proof;
        reset_observability(&mut (*state).accel);
    }

    let executor = match private.gpu_strategy {
        GpuStrategy::Agg => {
            let (spec, projection) = private
                .agg_query_spec
                .zip(private.agg_output_projection)
                .unwrap_or_else(|| {
                    pgrx::error!("pg_accel: aggregate plan is missing its AQS3/AOP2 contract")
                });
            let explain_only = (eflags as u32 & pg_sys::EXEC_FLAG_EXPLAIN_ONLY) != 0;
            let exec = AggExecState::new_descriptor(spec, projection, explain_only).unwrap_or_else(
                |error| {
                    pgrx::error!(
                        "pg_accel: generic aggregate Begin failed ({error}); refusing CPU fallback"
                    )
                },
            );
            Box::into_raw(Box::new(exec)).cast()
        }
        GpuStrategy::Raster => {
            let plan = private.raster_exec_plan.unwrap_or_else(|| {
                pgrx::error!("pg_accel: raster CustomScan is missing its exact RQS2 contract")
            });
            // SAFETY: node is an initialized executor state.
            let estate = unsafe { (*node).ss.ps.state };
            if estate.is_null() {
                pgrx::error!("pg_accel: raster CustomScan has no executor state");
            }
            // SAFETY: the non-null EState owns its query memory context.
            let query_context = unsafe { (*estate).es_query_cxt };
            if query_context.is_null() {
                pgrx::error!("pg_accel: raster CustomScan has no query memory context");
            }
            // SAFETY: the scan slot and query context belong to this node.
            let exec = unsafe {
                RasterExecState::begin(plan, (*node).ss.ss_ScanTupleSlot, query_context)
            }
            .unwrap_or_else(|error| {
                pgrx::error!(
                    "pg_accel: raster BeginCustomScan failed ({error}); refusing CPU fallback"
                )
            });
            // Raster executes eagerly during Begin.
            // SAFETY: state is our extended allocation.
            unsafe {
                (*state).accel.rows_dispatched =
                    crate::engine::executor::ExecutorState::rows_dispatched(&exec);
                (*state).accel.batches_executed =
                    crate::engine::executor::ExecutorState::batches_executed(&exec);
                (*state).accel.dispatch_time_us =
                    crate::engine::executor::ExecutorState::dispatch_time_us(&exec);
            }
            Box::into_raw(Box::new(exec)).cast()
        }
        _ => unreachable!("retired strategies rejected above"),
    };
    // SAFETY: state is live and executor owns its concrete box.
    unsafe {
        (*state).accel.executor = executor;
    }
}

/// Return one tuple from the selected resident executor.
///
/// # Safety
/// Called by PostgreSQL for a state initialized by begin_custom_scan.
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn exec_custom_scan(
    node: *mut pg_sys::CustomScanState,
) -> *mut pg_sys::TupleTableSlot {
    if !gucs::enabled() {
        pgrx::error!("pg_accel: Custom Scan reached execution while pg_accel.enabled=off");
    }
    let state = node.cast::<GpuAccelScanState>();
    // SAFETY: state is our extended allocation.
    let executor = unsafe { (*state).accel.executor };
    if executor.is_null() {
        pgrx::error!("pg_accel: resident Custom Scan has no executor");
    }

    // SAFETY: strategy and executor were installed together in Begin.
    unsafe {
        let strategy = GpuStrategy::decode((*state).accel.strategy);
        let dyn_state: &mut dyn crate::engine::executor::ExecutorState = match strategy {
            GpuStrategy::Agg => &mut *executor.cast::<AggExecState>(),
            GpuStrategy::Raster => &mut *executor.cast::<RasterExecState>(),
            _ => pgrx::error!("pg_accel: retired {strategy:?} strategy reached resident executor"),
        };
        let slot = dyn_state.exec(node);
        (*state).accel.rows_dispatched = dyn_state.rows_dispatched();
        (*state).accel.batches_executed = dyn_state.batches_executed();
        (*state).accel.dispatch_time_us = dyn_state.dispatch_time_us();
        dsm::record_parallel_agg_counters_once(
            state,
            strategy,
            dyn_state.rows_dispatched(),
            dyn_state.batches_executed(),
            dyn_state.dispatch_time_us(),
        );
        slot
    }
}

/// Release one concrete resident executor.
///
/// # Safety
/// Called by PostgreSQL after execution for our extended state.
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn end_custom_scan(node: *mut pg_sys::CustomScanState) {
    let state = node.cast::<GpuAccelScanState>();
    // SAFETY: executor was allocated as the concrete type selected by strategy.
    unsafe {
        let executor = (*state).accel.executor;
        if executor.is_null() {
            return;
        }
        match GpuStrategy::decode((*state).accel.strategy) {
            GpuStrategy::Agg => drop(Box::from_raw(executor.cast::<AggExecState>())),
            GpuStrategy::Raster => drop(Box::from_raw(executor.cast::<RasterExecState>())),
            strategy => pgrx::error!(
                "pg_accel: retired {strategy:?} strategy has a resident executor pointer"
            ),
        }
        (*state).accel.executor = std::ptr::null_mut();
    }
}

/// Reset an aggregate or raster executor without changing its exact contract.
///
/// # Safety
/// Called by PostgreSQL for an initialized resident executor.
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn rescan_custom_scan(node: *mut pg_sys::CustomScanState) {
    let state = node.cast::<GpuAccelScanState>();
    // SAFETY: strategy and concrete executor were installed together.
    unsafe {
        if (*state).accel.executor.is_null() {
            pgrx::error!("pg_accel: resident rescan found no executor state");
        }
        match GpuStrategy::decode((*state).accel.strategy) {
            GpuStrategy::Agg => {
                (*(*state).accel.executor.cast::<AggExecState>()).reset_for_rescan();
            }
            GpuStrategy::Raster => {
                (*(*state).accel.executor.cast::<RasterExecState>()).reset_for_rescan();
            }
            strategy => {
                pgrx::error!("pg_accel: retired {strategy:?} strategy reached resident rescan");
            }
        }
        reset_observability(&mut (*state).accel);
        if GpuStrategy::decode((*state).accel.strategy) == GpuStrategy::Raster {
            let raster = &*(*state).accel.executor.cast::<RasterExecState>();
            (*state).accel.rows_dispatched =
                crate::engine::executor::ExecutorState::rows_dispatched(raster);
            (*state).accel.batches_executed =
                crate::engine::executor::ExecutorState::batches_executed(raster);
            (*state).accel.dispatch_time_us =
                crate::engine::executor::ExecutorState::dispatch_time_us(raster);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[cfg(feature = "pg_test")]
mod tests;
