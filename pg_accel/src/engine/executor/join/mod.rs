//! Batched hash/spatial join executor for pg_accel Custom Scan nodes.
//!
//! Implements `next` / `rescan` for three join strategies:
//! - `GpuSpatial`  — spatial nested loop with GPU predicate evaluation
//! - `GpuHashJoin` — build-side hash table with GPU probe helpers
//!
//! Selected join paths must run a GPU kernel; PostgreSQL native joins are the
//! only fallback when no GPU implementation is available.

mod build;
mod nlj;
mod probe;

use pgrx::pg_sys;

use crate::adapters::extractors::geometry::extract_geometry;
use crate::engine::gucs;
use crate::engine::materialize::tuple_extract::{self, AttExtractInfo};
use crate::engine::registry::{self, AccelStrategy};
use crate::engine::stats;
use crate::gpu::{GpuHashTable, PgaccelKeyType, three_layer};

pub(crate) const NLJ_SHAPE_BETWEEN: i32 = 2;

/// Resolve the PostGIS function name registered in the adapter to a
/// concrete [`three_layer::SpatialPredicate`].
///
/// This is an explicit allowlist: only function names wired through the GPU
/// pipeline return `Some`. Unknown or unregistered names return `None`, and
/// the caller errors because pg_accel must not run a scalar PostGIS fallback
/// inside an accelerator plan.
#[must_use]
#[allow(dead_code)] // reason: last runtime caller (next_gpu_spatial) retired with the spatial join executor; resolver retained for the Phase 6 spatial pipeline re-wiring and pinned by join/tests.rs
pub(super) fn resolve_spatial_predicate(
    fn_name: Option<&str>,
) -> Option<three_layer::SpatialPredicate> {
    match fn_name {
        Some("st_intersects") => Some(three_layer::SpatialPredicate::Intersects),
        Some("st_contains") => Some(three_layer::SpatialPredicate::Contains),
        Some("st_within") => Some(three_layer::SpatialPredicate::Within),
        _ => None,
    }
}

/// A buffered join match waiting to be returned by `next()`.
pub(super) struct PendingMatch {
    /// Owned copy of the outer tuple. Must be `pfree`d when no longer needed.
    pub(super) outer_tuple: pg_sys::MinimalTuple,
    /// Owned copy of the inner tuple that matched. Must be `pfree`d
    /// when no longer needed.
    pub(super) inner_tuple: pg_sys::MinimalTuple,
}

/// Mapping entry: (child_index 0=outer/1=inner, varattno in child plan output).
#[derive(Clone)]
pub(crate) struct TlistMapEntry {
    pub(super) child_idx: usize,
    pub(super) child_attno: i16,
}

const HASH_JOIN_MATCHES_PER_OUTER: usize = 4;
const HASH_JOIN_WORKER_COUNT: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HashJoinTelemetry {
    pub(super) build_count: u64,
    pub(super) redundant_inner_builds: u64,
    pub(super) build_rows: usize,
    pub(super) build_non_null_rows: usize,
    pub(super) hash_table_capacity: usize,
    pub(super) probe_batches: u64,
    pub(super) last_probe_rows: usize,
    pub(super) last_max_matches: usize,
    pub(super) last_match_buffer_u32s: usize,
    pub(super) last_match_count: usize,
    pub(super) worker_count: u32,
    pub(super) worker_number: i32,
}

impl Default for HashJoinTelemetry {
    fn default() -> Self {
        Self {
            build_count: 0,
            redundant_inner_builds: 0,
            build_rows: 0,
            build_non_null_rows: 0,
            hash_table_capacity: 0,
            probe_batches: 0,
            last_probe_rows: 0,
            last_max_matches: 0,
            last_match_buffer_u32s: 0,
            last_match_count: 0,
            worker_count: HASH_JOIN_WORKER_COUNT,
            worker_number: -1,
        }
    }
}

impl HashJoinTelemetry {
    #[must_use]
    pub(crate) const fn build_count(self) -> u64 {
        self.build_count
    }

    #[must_use]
    pub(crate) const fn redundant_inner_builds(self) -> u64 {
        self.redundant_inner_builds
    }

    #[must_use]
    pub(crate) const fn build_rows(self) -> usize {
        self.build_rows
    }

    #[must_use]
    pub(crate) const fn build_non_null_rows(self) -> usize {
        self.build_non_null_rows
    }

    #[must_use]
    pub(crate) const fn probe_batches(self) -> u64 {
        self.probe_batches
    }
}

#[must_use]
pub(super) fn hash_join_non_null_rows(null_mask: &[u8]) -> usize {
    null_mask
        .iter()
        .fold(0usize, |count, &is_null| count + usize::from(is_null == 0))
}

#[must_use]
pub(super) fn hash_join_table_capacity(non_null_rows: usize) -> Option<usize> {
    let load_slots = non_null_rows.checked_mul(2)?;
    let rounded = load_slots.max(2).checked_next_power_of_two()?;
    Some(rounded.max(16))
}

#[must_use]
pub(super) fn hash_join_max_matches(outer_count: usize) -> Option<usize> {
    outer_count.checked_mul(HASH_JOIN_MATCHES_PER_OUTER)
}

#[must_use]
pub(super) fn hash_join_match_buffer_u32s(max_matches: usize) -> Option<usize> {
    max_matches.checked_mul(2)
}

#[must_use]
pub(super) fn hash_join_row_indices_representable(row_count: usize) -> bool {
    u32::try_from(row_count).is_ok()
}

#[must_use]
pub(super) const fn hash_join_key_type_supported(key_type: PgaccelKeyType) -> bool {
    matches!(key_type, PgaccelKeyType::Int32 | PgaccelKeyType::Int64)
}

#[must_use]
pub(super) fn hash_join_match_count_within_capacity(
    match_count: usize,
    max_matches: usize,
) -> bool {
    match_count <= max_matches
}

/// Rust-side batch join executor state.
///
/// Not `repr(C)` — lives on the Rust heap, opaque to PG.
#[allow(clippy::struct_excessive_bools)]
pub struct JoinExecState {
    /// Acceleration strategy for this join node.
    strategy: AccelStrategy,

    /// Batch size (from GUC).
    batch_size: usize,

    /// Current outer tuple being joined against the inner side.
    /// NULL when we need to pull a new outer tuple.
    current_outer: *mut pg_sys::TupleTableSlot,

    /// Whether the outer side is exhausted.
    outer_exhausted: bool,

    /// Whether the inner side needs a rescan for the next outer tuple.
    inner_needs_rescan: bool,

    /// Qual expression for residual join conditions.
    qual: *mut pg_sys::ExprState,

    /// Expression context for qual evaluation.
    econtext: *mut pg_sys::ExprContext,

    /// Buffer of pending matches from GPU batch evaluation.
    /// For GpuSpatial, inner tuples are batched, evaluated in bulk,
    /// and matching results are queued here for one-at-a-time return.
    pending_matches: Vec<PendingMatch>,

    /// Index into `pending_matches` for the next result to yield.
    pending_cursor: usize,

    // -- GPU dispatch context (set via `set_gpu_context`) --
    /// Attribute number of the outer geometry column (1-based, 0 = not set).
    outer_attno: i32,

    /// Attribute number of the inner geometry column (1-based, 0 = not set).
    inner_attno: i32,

    /// Function OID for the spatial predicate. Zero means not set.
    fn_oid: pg_sys::Oid,

    /// Initialised `FmgrInfo` for the spatial function. Only valid when
    /// `fn_oid != InvalidOid`.
    fn_info_buf: pg_sys::FmgrInfo,

    // -- Hash join context (set via `set_hash_join_context`) --
    /// Attribute number of the join key in the outer relation (1-based, 0 = not set).
    hash_outer_attno: i32,

    /// Attribute number of the join key in the inner relation (1-based, 0 = not set).
    hash_inner_attno: i32,

    /// Key type for hash join operations.
    hash_key_type: PgaccelKeyType,

    /// GPU hash table built from inner side (set during first probe).
    hash_table: Option<GpuHashTable>,

    /// Whether the hash table has been built (inner side consumed).
    hash_built: bool,

    /// Collected inner tuples for hash join (consumed during build).
    hash_inner_tuples: Vec<pg_sys::MinimalTuple>,

    /// Mapping from scan slot attributes to child plan columns.
    /// Built during `set_tlist_mapping` from `custom_scan_tlist`.
    pub(crate) tlist_map: Vec<TlistMapEntry>,

    /// Temporary slot for loading outer MinimalTuples during hash join yield.
    hash_outer_slot: *mut pg_sys::TupleTableSlot,
    /// Temporary slot for loading inner MinimalTuples during hash join yield.
    hash_inner_slot: *mut pg_sys::TupleTableSlot,

    /// Structured hash-join metadata for traces and tests.
    hash_join_telemetry: HashJoinTelemetry,

    /// True when this Custom Scan represents `COUNT(*)` over a hash join and
    /// should emit a single aggregate row instead of joined heap tuples.
    hash_count_only: bool,

    /// True when count-only hashjoin consumes device-resident cached key columns.
    hash_resident_count: bool,

    /// Expected outer relation OID for the resident hashjoin count cache.
    hash_outer_rel_oid: pg_sys::Oid,

    /// Expected inner relation OID for the resident hashjoin count cache.
    hash_inner_rel_oid: pg_sys::Oid,

    /// Whether the count-only row has already been emitted.
    hash_count_returned: bool,

    // -- NestedLoop inequality context (set via `set_nlj_between_context`) --
    /// NLJ predicate shape. Currently only `NLJ_SHAPE_BETWEEN` is selectable.
    nlj_shape: i32,
    /// Attribute number of the value column on child 0 / outer side.
    nlj_outer_value_attno: i32,
    /// Attribute number of the lower-bound column on child 1 / inner side.
    nlj_inner_lo_attno: i32,
    /// Attribute number of the upper-bound column on child 1 / inner side.
    nlj_inner_hi_attno: i32,
    /// Key type for the NLJ dispatch.
    nlj_key_type: PgaccelKeyType,
    /// Whether the one-shot NLJ dispatch has consumed both children.
    nlj_dispatched: bool,

    // -- Counters for EXPLAIN ANALYZE --
    /// Total outer rows pulled.
    pub rows_dispatched: u64,

    /// Number of batches processed.
    pub batches_executed: u64,

    /// Cumulative microseconds spent in dispatch.
    pub dispatch_time_us: u64,
}

impl JoinExecState {
    /// Create a new join executor state.
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
            current_outer: std::ptr::null_mut(),
            outer_exhausted: false,
            inner_needs_rescan: false,
            qual,
            econtext,
            pending_matches: Vec::new(),
            pending_cursor: 0,
            outer_attno: 0,
            inner_attno: 0,
            fn_oid: pg_sys::InvalidOid,
            // SAFETY: zero-initialised FmgrInfo is safe — all fields are
            // integers/pointers that accept zero.
            fn_info_buf: unsafe { std::mem::zeroed() },
            hash_outer_attno: 0,
            hash_inner_attno: 0,
            hash_key_type: PgaccelKeyType::Int32,
            hash_table: None,
            hash_built: false,
            hash_inner_tuples: Vec::new(),
            tlist_map: Vec::new(),
            hash_outer_slot: std::ptr::null_mut(),
            hash_inner_slot: std::ptr::null_mut(),
            hash_join_telemetry: HashJoinTelemetry::default(),
            hash_count_only: false,
            hash_resident_count: false,
            hash_outer_rel_oid: pg_sys::InvalidOid,
            hash_inner_rel_oid: pg_sys::InvalidOid,
            hash_count_returned: false,
            nlj_shape: 0,
            nlj_outer_value_attno: 0,
            nlj_inner_lo_attno: 0,
            nlj_inner_hi_attno: 0,
            nlj_key_type: PgaccelKeyType::Int32,
            nlj_dispatched: false,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
        }
    }

    /// Reset state for a rescan (e.g., from a Nested Loop).
    ///
    /// Preserves key configuration and tlist mapping but drops the
    /// hash table and resets all iteration state so the join can be
    /// re-executed from scratch.
    pub fn reset_for_rescan(&mut self) {
        self.current_outer = std::ptr::null_mut();
        self.outer_exhausted = false;
        self.inner_needs_rescan = false;
        self.free_pending_matches();
        self.pending_matches.clear();
        self.pending_cursor = 0;
        self.hash_table = None;
        self.hash_built = false;
        self.hash_count_returned = false;
        self.nlj_dispatched = false;
        // Free inner tuples from previous scan.
        for &mt in &self.hash_inner_tuples {
            if !mt.is_null() {
                // SAFETY: mt was allocated via ExecCopySlotMinimalTuple.
                unsafe { pg_sys::pfree(mt.cast()) };
            }
        }
        self.hash_inner_tuples.clear();
        self.rows_dispatched = 0;
        self.batches_executed = 0;
        self.dispatch_time_us = 0;
    }

    #[must_use]
    pub(crate) fn hash_join_telemetry(&self) -> HashJoinTelemetry {
        self.hash_join_telemetry
    }

    #[must_use]
    pub(crate) fn hash_join_count_only(&self) -> bool {
        self.hash_count_only
    }

    #[must_use]
    pub(crate) fn hash_join_reuses_build_across_probe_batches(&self) -> bool {
        self.strategy == AccelStrategy::GpuHashJoin
            && self.hash_join_telemetry.build_count == 1
            && self.hash_join_telemetry.redundant_inner_builds == 0
            && self.hash_join_telemetry.probe_batches > 1
    }

    #[must_use]
    pub(crate) const fn hash_join_shared_inner_reuse(&self) -> bool {
        false
    }

    pub(super) fn record_hash_join_worker_metadata(&mut self, worker_number: i32) {
        self.hash_join_telemetry.worker_count = HASH_JOIN_WORKER_COUNT;
        self.hash_join_telemetry.worker_number = worker_number;
    }

    pub(super) fn record_hash_join_build_metadata(
        &mut self,
        build_rows: usize,
        build_non_null_rows: usize,
        hash_table_capacity: usize,
    ) -> bool {
        let redundant = self.hash_join_telemetry.build_count > 0;
        self.hash_join_telemetry.build_count =
            self.hash_join_telemetry.build_count.saturating_add(1);
        if redundant {
            self.hash_join_telemetry.redundant_inner_builds = self
                .hash_join_telemetry
                .redundant_inner_builds
                .saturating_add(1);
        }
        self.hash_join_telemetry.build_rows = build_rows;
        self.hash_join_telemetry.build_non_null_rows = build_non_null_rows;
        self.hash_join_telemetry.hash_table_capacity = hash_table_capacity;
        redundant
    }

    pub(super) fn record_hash_join_probe_metadata(
        &mut self,
        probe_rows: usize,
        max_matches: usize,
        match_buffer_u32s: usize,
    ) {
        self.hash_join_telemetry.probe_batches =
            self.hash_join_telemetry.probe_batches.saturating_add(1);
        self.hash_join_telemetry.last_probe_rows = probe_rows;
        self.hash_join_telemetry.last_max_matches = max_matches;
        self.hash_join_telemetry.last_match_buffer_u32s = match_buffer_u32s;
        self.hash_join_telemetry.last_match_count = 0;
    }

    pub(super) fn record_hash_join_probe_result(&mut self, match_count: usize) {
        self.hash_join_telemetry.last_match_count = match_count;
    }

    fn free_pending_matches(&self) {
        for m in &self.pending_matches {
            if !m.outer_tuple.is_null() {
                // SAFETY: outer_tuple was allocated by ExecCopySlotMinimalTuple
                // or heap_copy_minimal_tuple.
                unsafe { pg_sys::pfree(m.outer_tuple.cast()) };
            }
            if !m.inner_tuple.is_null() {
                // SAFETY: inner_tuple was allocated by ExecCopySlotMinimalTuple
                // or heap_copy_minimal_tuple.
                unsafe { pg_sys::pfree(m.inner_tuple.cast()) };
            }
        }
    }

    /// Fetch the next matching join tuple.
    ///
    /// Implements a nested-loop join: for each outer tuple, scan all
    /// inner tuples and evaluate the residual qual. Returns matching
    /// (outer, inner) pairs one at a time.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. All pointers must be valid.
    pub unsafe fn next(
        &mut self,
        outer_ps: *mut pg_sys::PlanState,
        inner_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.join_next").entered();
        match self.strategy {
            AccelStrategy::GpuSpatial => {
                let _ = (outer_ps, inner_ps, result_slot);
                pgrx::error!(
                    "pg_accel: spatial join executor is not selectable until complete GPU output assembly is wired; refusing CPU fallback"
                );
            }
            AccelStrategy::GpuHashJoin => {
                // SAFETY: Caller guarantees main backend thread.
                if self.hash_count_only {
                    if self.hash_resident_count {
                        unsafe { self.next_resident_hash_join_count(result_slot) }
                    } else {
                        unsafe { self.next_hash_join_count(outer_ps, inner_ps, result_slot) }
                    }
                } else {
                    unsafe { self.next_hash_join(outer_ps, inner_ps, result_slot) }
                }
            }
            AccelStrategy::GpuNestedLoopIneq => {
                // SAFETY: Caller guarantees main backend thread.
                unsafe { self.next_nlj_ineq(outer_ps, inner_ps, result_slot) }
            }
            _ => {
                pgrx::error!(
                    "pg_accel: join executor received {:?}; refusing scalar CPU fallback",
                    self.strategy,
                );
            }
        }
    }

    /// Legacy scalar nested-loop join helper. Runtime CPU join evaluation is
    /// forbidden for pg_accel plans, so any call is a contract error.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    #[allow(dead_code)]
    unsafe fn next_scalar(
        &mut self,
        outer_ps: *mut pg_sys::PlanState,
        inner_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _ = (outer_ps, inner_ps, result_slot);
        pgrx::error!("pg_accel: scalar join helper called; refusing CPU join fallback");
    }

    /// Legacy scalar qual helper. Runtime CPU join qual evaluation is
    /// forbidden for pg_accel plans, so any call is a contract error.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `self.qual` and
    /// `self.econtext` must be valid (non-null). `self.current_outer` must
    /// point to a materialized outer slot.
    #[allow(dead_code)]
    unsafe fn eval_batch_scalar_qual(
        &mut self,
        inner_tuples: &[pg_sys::MinimalTuple],
        result_slot: *mut pg_sys::TupleTableSlot,
    ) {
        let _ = (inner_tuples, result_slot);
        pgrx::error!("pg_accel: scalar join qual helper called; refusing CPU join fallback");
    }

    /// Configure GPU dispatch context for spatial join evaluation.
    ///
    /// `outer_attno` and `inner_attno` are 1-based attribute numbers for the
    /// geometry columns in the outer and inner relations, respectively.
    ///
    /// # Safety
    ///
    /// `fn_oid` must be a valid regproc OID. Must be called on the main
    /// backend thread (calls `fmgr_info`).
    pub unsafe fn set_gpu_context(
        &mut self,
        fn_oid: pg_sys::Oid,
        outer_attno: i32,
        inner_attno: i32,
    ) {
        self.fn_oid = fn_oid;
        self.outer_attno = outer_attno;
        self.inner_attno = inner_attno;
        if fn_oid != pg_sys::InvalidOid {
            // SAFETY: Caller guarantees fn_oid is valid and we are on the
            // main backend thread.
            unsafe {
                pg_sys::fmgr_info(fn_oid, &raw mut self.fn_info_buf);
            }
        }
    }

    /// Check if a slot is empty (no valid tuple).
    ///
    /// Find the output position in a child plan for a given (varno, varattno).
    ///
    /// Searches the child plan's output list (custom_scan_tlist for CustomScans,
    /// plan.targetlist for others) for a Var matching the given varno and attno.
    /// Returns the 1-based output position, or 0 if not found.
    ///
    /// # Safety
    ///
    /// `ps` must be a valid PlanState pointer or null.
    unsafe fn find_child_output_pos(
        ps: *mut pg_sys::PlanState,
        target_varno: i32,
        target_attno: i16,
    ) -> i16 {
        if ps.is_null() {
            return 0;
        }
        // SAFETY: ps->plan is valid.
        let plan = unsafe { (*ps).plan };
        if plan.is_null() {
            return 0;
        }

        // For CustomScan nodes (scanrelid=0), check custom_scan_tlist first
        // since plan.targetlist uses INDEX_VAR references.
        let scan = plan.cast::<pg_sys::Scan>();
        let scanrelid = unsafe { (*scan).scanrelid };
        if scanrelid == 0 {
            // Try custom_scan_tlist (has original relation Vars).
            let node_tag = unsafe { (*plan.cast::<pg_sys::Node>()).type_ };
            if node_tag == pg_sys::NodeTag::T_CustomScan {
                let cscan = plan.cast::<pg_sys::CustomScan>();
                let cst = unsafe { (*cscan).custom_scan_tlist };
                if !cst.is_null() {
                    let clen = unsafe { pg_sys::list_length(cst) };
                    for j in 0..clen {
                        let tle = unsafe { pg_sys::list_nth(cst, j).cast::<pg_sys::TargetEntry>() };
                        if tle.is_null() {
                            continue;
                        }
                        let expr = unsafe { (*tle).expr };
                        if expr.is_null() {
                            continue;
                        }
                        if unsafe { (*expr.cast::<pg_sys::Node>()).type_ } != pg_sys::NodeTag::T_Var
                        {
                            continue;
                        }
                        let var = expr.cast::<pg_sys::Var>();
                        let vno = unsafe { (*var).varno };
                        let vatt = unsafe { (*var).varattno };
                        if vno == target_varno && vatt == target_attno {
                            // resno is 1-based output position.
                            return unsafe { (*tle).resno };
                        }
                    }
                }
            }
            return 0;
        }

        // For SeqScan/IndexScan (scanrelid > 0): check plan.targetlist.
        // Vars here reference the scanned table directly.
        // Only match if the target varno matches this scan's relation —
        // otherwise we'd falsely match columns from unrelated tables
        // that happen to share the same attno.
        if target_varno > 0 && scanrelid as i32 != target_varno {
            return 0;
        }
        let tlist = unsafe { (*plan).targetlist };
        if tlist.is_null() {
            return 0;
        }
        let tlen = unsafe { pg_sys::list_length(tlist) };
        for i in 0..tlen {
            let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
            if tle.is_null() {
                continue;
            }
            let expr = unsafe { (*tle).expr };
            if expr.is_null() {
                continue;
            }
            if unsafe { (*expr.cast::<pg_sys::Node>()).type_ } != pg_sys::NodeTag::T_Var {
                continue;
            }
            let var = expr.cast::<pg_sys::Var>();
            let vatt = unsafe { (*var).varattno };
            if vatt == target_attno {
                return unsafe { (*tle).resno };
            }
        }
        0
    }

    /// # Safety
    ///
    /// `slot` must be a valid, non-null `TupleTableSlot` pointer.
    unsafe fn slot_is_empty(slot: *mut pg_sys::TupleTableSlot) -> bool {
        // SAFETY: slot is non-null, caller guarantees validity.
        unsafe { (*slot).tts_flags & pg_sys::TTS_FLAG_EMPTY as u16 != 0 }
    }

    /// Returns the acceleration strategy.
    #[must_use]
    pub fn strategy(&self) -> AccelStrategy {
        self.strategy
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

    /// Reset state for a rescan (e.g., when used as inner side of a
    /// nested loop). Frees any buffered `MinimalTuple` copies and
    /// clears all iteration state.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    pub unsafe fn rescan(&mut self) {
        // SAFETY: Free owned MinimalTuples in pending_matches.
        self.free_pending_matches();
        self.pending_matches.clear();
        self.pending_cursor = 0;
        self.current_outer = std::ptr::null_mut();
        self.outer_exhausted = false;
        self.inner_needs_rescan = false;
        self.nlj_dispatched = false;

        // Reset hash join state for rescan.
        self.hash_table = None;
        self.hash_built = false;
        for mt in &self.hash_inner_tuples {
            if !mt.is_null() {
                unsafe { pg_sys::pfree((*mt).cast()) };
            }
        }
        self.hash_inner_tuples.clear();
    }
}

impl crate::engine::executor::state::ExecutorState for JoinExecState {
    unsafe fn exec(&mut self, css: *mut pg_sys::CustomScanState) -> *mut pg_sys::TupleTableSlot {
        let outer_ps = unsafe { crate::engine::executor::state::child_plan_state(css, 0) };
        let inner_ps = unsafe { crate::engine::executor::state::child_plan_state(css, 1) };
        let scan_slot = unsafe { (*css).ss.ss_ScanTupleSlot };
        unsafe { self.next(outer_ps, inner_ps, scan_slot) }
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
#[allow(dead_code)]
#[path = "tests.rs"]
mod tests;
