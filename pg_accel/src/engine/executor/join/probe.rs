//! Hash-join probe phase — key extraction, GPU dispatch, pending-match emission.

use pgrx::pg_sys;

use crate::adapters::extractors::geometry::extract_geometry;
use crate::engine::gucs;
use crate::engine::materialize::tuple_extract::{self, AttExtractInfo};
use crate::engine::registry::{self, AccelStrategy};
use crate::engine::stats;
use crate::gpu::{GpuHashTable, PgaccelKeyType, three_layer};

use super::{
    JoinExecState, PendingMatch, hash_join_key_type_supported, hash_join_match_buffer_u32s,
    hash_join_match_count_within_capacity, hash_join_max_matches, hash_join_non_null_rows,
    hash_join_row_indices_representable, hash_join_table_capacity,
};

impl JoinExecState {
    /// GPU hash join: build hash table from inner, probe with outer.
    ///
    /// Phase 1: Consume ALL inner tuples, extract join keys, build hash table.
    /// Phase 2: For each outer batch, extract keys, probe, emit matches.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    #[allow(clippy::too_many_lines)]
    pub(super) unsafe fn next_hash_join(
        &mut self,
        outer_ps: *mut pg_sys::PlanState,
        inner_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        // SAFETY: PostgreSQL sets ParallelWorkerNumber to -1 outside
        // parallel workers and to the worker id inside one.
        self.record_hash_join_worker_metadata(unsafe { pg_sys::ParallelWorkerNumber });

        // Get the inner plan's result slot — its tuple descriptor matches
        // inner tuples. We must NOT use result_slot (scan slot) for inner
        // key extraction because it has the outer plan's descriptor.
        // SAFETY: inner_ps is a valid PlanState; ps_ResultTupleSlot is set
        // by ExecInitNode for the inner plan.
        let inner_result_slot = if inner_ps.is_null() {
            result_slot
        } else {
            let slot = unsafe { (*inner_ps).ps_ResultTupleSlot };
            if slot.is_null() {
                // Fallback: use the scan tuple slot from inner's ScanState.
                // SAFETY: inner_ps points to a valid PlanState. If it's a
                // ScanState, ss_ScanTupleSlot has the right descriptor.
                let ss = inner_ps.cast::<pg_sys::ScanState>();
                let scan_slot = unsafe { (*ss).ss_ScanTupleSlot };
                if scan_slot.is_null() {
                    result_slot
                } else {
                    scan_slot
                }
            } else {
                slot
            }
        };

        // Phase 1: Build hash table from inner side (once).
        if !self.hash_built {
            self.hash_built = true;

            // Consume all inner tuples.
            loop {
                // SAFETY: ExecProcNode pulls from inner plan.
                let inner_slot = unsafe { pg_sys::ExecProcNode(inner_ps) };
                if inner_slot.is_null() || unsafe { Self::slot_is_empty(inner_slot) } {
                    break;
                }
                // SAFETY: Copy to owned MinimalTuple.
                let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(inner_slot) };
                self.hash_inner_tuples.push(mt);

                if self.hash_inner_tuples.len().is_multiple_of(10000) {
                    pgrx::check_for_interrupts!();
                }
            }

            let inner_count = self.hash_inner_tuples.len();
            if inner_count == 0 {
                let redundant = self.record_hash_join_build_metadata(0, 0, 0);
                let telemetry = self.hash_join_telemetry();
                tracing::info!(
                    target: "pg_accel::hash_join",
                    phase = "build",
                    build_count = telemetry.build_count,
                    redundant_inner_build = redundant,
                    redundant_inner_builds = telemetry.redundant_inner_builds,
                    build_rows = 0usize,
                    build_non_null_rows = 0usize,
                    hash_table_capacity = 0usize,
                    worker_count = telemetry.worker_count,
                    worker_number = telemetry.worker_number,
                    key_type = ?self.hash_key_type,
                    "exec.hash_join_build_empty"
                );
                return std::ptr::null_mut();
            }

            if !hash_join_key_type_supported(self.hash_key_type) {
                pgrx::error!(
                    "pg_accel: hash_join key type {:?} is not implemented by the selected GPU build/probe path; planner should have declined",
                    self.hash_key_type,
                );
            }

            if !hash_join_row_indices_representable(inner_count) {
                pgrx::error!(
                    "pg_accel: hash-join inner row count {} exceeds u32 index capacity; planner should have declined",
                    inner_count
                );
            }
            let inner_count_u32 = match u32::try_from(inner_count) {
                Ok(count) => count,
                Err(_) => {
                    pgrx::error!(
                        "pg_accel: hash-join inner row count {} exceeds u32 index capacity; planner should have declined",
                        inner_count
                    );
                }
            };

            // Validate attno vs slot descriptor before key extraction.
            let inner_tupdesc = unsafe { (*inner_result_slot).tts_tupleDescriptor };
            if inner_tupdesc.is_null() {
                pgrx::error!("pg_accel: hash join inner slot has no tuple descriptor");
            }
            let inner_natts = unsafe { (*inner_tupdesc).natts };
            if self.hash_inner_attno <= 0 || self.hash_inner_attno > inner_natts {
                pgrx::error!(
                    "pg_accel: hash join inner key attno {} out of range 1..={}; refusing CPU fallback",
                    self.hash_inner_attno,
                    inner_natts,
                );
            }

            // Bulk-extract keys from inner tuples using direct MinimalTuple
            // reads (avoids per-tuple ExecForceStoreMinimalTuple overhead).
            // SAFETY: inner_result_slot has a valid tuple descriptor matching
            // the inner tuples.
            let inner_info = unsafe { AttExtractInfo::new(inner_tupdesc, self.hash_inner_attno) };
            let indices: Vec<u32> = (0..inner_count_u32).collect();

            // Extract only the key type we need — one allocation, one pass.
            let mut int32_keys: Vec<i32> = Vec::new();
            let mut long_keys: Vec<i64> = Vec::new();
            let null_mask: Vec<u8>;

            // SAFETY: hash_inner_tuples contains valid MinimalTuple pointers.
            // inner_result_slot is valid for fallback extraction.
            match self.hash_key_type {
                PgaccelKeyType::Int32 => {
                    let (k, n) = unsafe {
                        tuple_extract::extract_i32(
                            &self.hash_inner_tuples,
                            &inner_info,
                            inner_result_slot,
                        )
                    };
                    int32_keys = k;
                    null_mask = n;
                }
                PgaccelKeyType::Int64 => {
                    let (k, n) = unsafe {
                        tuple_extract::extract_i64(
                            &self.hash_inner_tuples,
                            &inner_info,
                            inner_result_slot,
                        )
                    };
                    long_keys = k;
                    null_mask = n;
                }
                PgaccelKeyType::Float64 | PgaccelKeyType::Uuid | PgaccelKeyType::Inet => {
                    pgrx::error!(
                        "pg_accel: hash_join key type {:?} not implemented; planner should have declined",
                        self.hash_key_type,
                    );
                }
            }

            let build_non_null_rows = hash_join_non_null_rows(&null_mask);
            let Some(hash_table_capacity) = hash_join_table_capacity(build_non_null_rows) else {
                pgrx::error!(
                    "pg_accel: hash-join build capacity overflow for {} non-null inner rows; planner should have declined",
                    build_non_null_rows
                );
            };
            let redundant = self.record_hash_join_build_metadata(
                inner_count,
                build_non_null_rows,
                hash_table_capacity,
            );
            let telemetry = self.hash_join_telemetry();
            tracing::info!(
                target: "pg_accel::hash_join",
                phase = "build",
                build_count = telemetry.build_count,
                redundant_inner_build = redundant,
                redundant_inner_builds = telemetry.redundant_inner_builds,
                build_rows = telemetry.build_rows,
                build_non_null_rows = telemetry.build_non_null_rows,
                hash_table_capacity = telemetry.hash_table_capacity,
                worker_count = telemetry.worker_count,
                worker_number = telemetry.worker_number,
                key_type = ?self.hash_key_type,
                "exec.hash_join_build_start"
            );

            // Build hash table via GPU.
            let keys_ptr: *const std::ffi::c_void = match self.hash_key_type {
                PgaccelKeyType::Int32 => int32_keys.as_ptr().cast(),
                PgaccelKeyType::Int64 => long_keys.as_ptr().cast(),
                PgaccelKeyType::Float64 | PgaccelKeyType::Uuid | PgaccelKeyType::Inet => {
                    pgrx::error!(
                        "pg_accel: hash_join key type {:?} not implemented",
                        self.hash_key_type,
                    )
                }
            };

            self.hash_table =
                GpuHashTable::build(keys_ptr, &null_mask, &indices, self.hash_key_type);
            let build_succeeded = self.hash_table.is_some();
            let telemetry = self.hash_join_telemetry();
            tracing::info!(
                target: "pg_accel::hash_join",
                phase = "build",
                build_succeeded,
                build_count = telemetry.build_count,
                build_rows = telemetry.build_rows,
                build_non_null_rows = telemetry.build_non_null_rows,
                hash_table_capacity = telemetry.hash_table_capacity,
                worker_count = telemetry.worker_count,
                worker_number = telemetry.worker_number,
                key_type = ?self.hash_key_type,
                "exec.hash_join_build_finish"
            );
            if !build_succeeded {
                pgrx::error!(
                    "pg_accel: GPU hash-join build failed for {} inner rows ({} non-null, planned hash capacity {}); refusing to fall back to CPU (rule 11)",
                    inner_count,
                    build_non_null_rows,
                    hash_table_capacity,
                );
            }
        }

        // Phase 2: Probe with outer tuples in batches.
        loop {
            // Drain pending matches first — build virtual tuple from both sides.
            if self.pending_cursor < self.pending_matches.len() {
                let m = &self.pending_matches[self.pending_cursor];
                self.pending_cursor += 1;

                // SAFETY: Build a virtual tuple in result_slot by extracting
                // each attribute from the appropriate child slot.
                unsafe {
                    if !self.hash_outer_slot.is_null() && !m.outer_tuple.is_null() {
                        pg_sys::ExecForceStoreMinimalTuple(
                            m.outer_tuple,
                            self.hash_outer_slot,
                            false,
                        );
                    }
                    if !self.hash_inner_slot.is_null() && !m.inner_tuple.is_null() {
                        pg_sys::ExecForceStoreMinimalTuple(
                            m.inner_tuple,
                            self.hash_inner_slot,
                            false,
                        );
                    }

                    // Clear the result slot and populate as virtual tuple.
                    pg_sys::ExecClearTuple(result_slot);
                    let natts = (*(*result_slot).tts_tupleDescriptor).natts as usize;
                    for (i, entry) in self.tlist_map.iter().enumerate() {
                        if i >= natts {
                            break;
                        }
                        let src_slot = if entry.child_idx == 1 {
                            self.hash_inner_slot
                        } else {
                            self.hash_outer_slot
                        };
                        if src_slot.is_null() || entry.child_attno <= 0 {
                            *(*result_slot).tts_isnull.add(i) = true;
                            continue;
                        }
                        let mut attr_null = false;
                        let datum = pg_sys::slot_getattr(
                            src_slot,
                            i32::from(entry.child_attno),
                            std::ptr::addr_of_mut!(attr_null),
                        );
                        *(*result_slot).tts_values.add(i) = datum;
                        *(*result_slot).tts_isnull.add(i) = attr_null;
                    }
                    pg_sys::ExecStoreVirtualTuple(result_slot);
                }
                return result_slot;
            }

            // Free owned MinimalTuples before clearing.
            for m in &self.pending_matches {
                if !m.outer_tuple.is_null() {
                    // SAFETY: outer_tuple was palloc'd by ExecCopySlotMinimalTuple.
                    unsafe { pg_sys::pfree(m.outer_tuple.cast()) };
                }
                if !m.inner_tuple.is_null() {
                    // SAFETY: inner_tuple was palloc'd by ExecCopySlotMinimalTuple.
                    unsafe { pg_sys::pfree(m.inner_tuple.cast()) };
                }
            }
            self.pending_matches.clear();
            self.pending_cursor = 0;

            if self.outer_exhausted {
                return std::ptr::null_mut();
            }

            // Collect a batch of outer tuples.
            let mut outer_tuples: Vec<pg_sys::MinimalTuple> = Vec::with_capacity(self.batch_size);

            for _ in 0..self.batch_size {
                // SAFETY: ExecProcNode pulls from outer plan.
                let outer_slot = unsafe { pg_sys::ExecProcNode(outer_ps) };
                if outer_slot.is_null() || unsafe { Self::slot_is_empty(outer_slot) } {
                    self.outer_exhausted = true;
                    break;
                }
                // SAFETY: Copy to owned MinimalTuple.
                let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(outer_slot) };
                outer_tuples.push(mt);
            }

            if outer_tuples.is_empty() {
                return std::ptr::null_mut();
            }

            let start = std::time::Instant::now();
            let outer_count = outer_tuples.len();
            self.rows_dispatched += outer_count as u64;

            if !hash_join_row_indices_representable(outer_count) {
                for mt in outer_tuples {
                    if !mt.is_null() {
                        // SAFETY: Free owned copy.
                        unsafe { pg_sys::pfree(mt.cast()) };
                    }
                }
                pgrx::error!(
                    "pg_accel: hash-join outer batch row count {} exceeds u32 index capacity; planner should have declined",
                    outer_count
                );
            }

            // Per CLAUDE.md rule 11: no CPU fallback on GPU kernel failure.
            // Previously we silently dropped all outer tuples when the hash
            // table build failed, producing an empty join result. Raise a PG
            // ERROR so the failure is surfaced instead.
            if self.hash_table.is_none() {
                // Free outer tuples before erroring so they don't leak on
                // error-context teardown (PG will clean the MemoryContext,
                // but be explicit).
                for mt in outer_tuples {
                    if !mt.is_null() {
                        // SAFETY: Free owned copy.
                        unsafe { pg_sys::pfree(mt.cast()) };
                    }
                }
                pgrx::error!(
                    "pg_accel: GPU hash-join build failed; refusing to fall back to CPU (rule 11)"
                );
            }

            // Extract outer keys for GPU probe.
            // Bulk-extract outer keys using direct MinimalTuple reads.
            let outer_extract_slot = if self.hash_outer_slot.is_null() {
                result_slot
            } else {
                self.hash_outer_slot
            };
            // SAFETY: outer_extract_slot has a valid tuple descriptor
            // matching the outer tuples.
            let outer_tupdesc = unsafe { (*outer_extract_slot).tts_tupleDescriptor };
            if outer_tupdesc.is_null() {
                pgrx::error!("pg_accel: hash join outer slot has no tuple descriptor");
            }
            let outer_natts = unsafe { (*outer_tupdesc).natts };
            if self.hash_outer_attno <= 0 || self.hash_outer_attno > outer_natts {
                pgrx::error!(
                    "pg_accel: hash join outer key attno {} out of range 1..={}; refusing CPU fallback",
                    self.hash_outer_attno,
                    outer_natts,
                );
            }
            let outer_info = unsafe { AttExtractInfo::new(outer_tupdesc, self.hash_outer_attno) };

            let mut o_int32_keys: Vec<i32> = Vec::new();
            let mut o_long_keys: Vec<i64> = Vec::new();
            let o_null_mask: Vec<u8>;

            // SAFETY: outer_tuples contains valid MinimalTuple pointers.
            match self.hash_key_type {
                PgaccelKeyType::Int32 => {
                    let (k, n) = unsafe {
                        tuple_extract::extract_i32(&outer_tuples, &outer_info, outer_extract_slot)
                    };
                    o_int32_keys = k;
                    o_null_mask = n;
                }
                PgaccelKeyType::Int64 => {
                    let (k, n) = unsafe {
                        tuple_extract::extract_i64(&outer_tuples, &outer_info, outer_extract_slot)
                    };
                    o_long_keys = k;
                    o_null_mask = n;
                }
                PgaccelKeyType::Float64 | PgaccelKeyType::Uuid | PgaccelKeyType::Inet => {
                    pgrx::error!(
                        "pg_accel: hash_join outer key type {:?} not implemented; planner should have declined",
                        self.hash_key_type,
                    );
                }
            }

            let o_keys_ptr: *const std::ffi::c_void = match self.hash_key_type {
                PgaccelKeyType::Int32 => o_int32_keys.as_ptr().cast(),
                PgaccelKeyType::Int64 => o_long_keys.as_ptr().cast(),
                PgaccelKeyType::Float64 | PgaccelKeyType::Uuid | PgaccelKeyType::Inet => {
                    pgrx::error!(
                        "pg_accel: hash_join outer key type {:?} not implemented",
                        self.hash_key_type,
                    )
                }
            };

            // Probe: max matches. For equijoins, each outer typically matches
            // 0-1 inner rows, but duplicates can inflate this. Use 4× outer
            // as a reasonable upper bound (covers moderate skew).
            let Some(max_matches) = hash_join_max_matches(outer_count) else {
                for mt in outer_tuples {
                    if !mt.is_null() {
                        // SAFETY: Free owned copy.
                        unsafe { pg_sys::pfree(mt.cast()) };
                    }
                }
                pgrx::error!(
                    "pg_accel: hash-join max_matches overflow for {} outer rows; planner should have declined",
                    outer_count
                );
            };
            let Some(match_buffer_u32s) = hash_join_match_buffer_u32s(max_matches) else {
                for mt in outer_tuples {
                    if !mt.is_null() {
                        // SAFETY: Free owned copy.
                        unsafe { pg_sys::pfree(mt.cast()) };
                    }
                }
                pgrx::error!(
                    "pg_accel: hash-join match buffer overflow for max_matches={}; planner should have declined",
                    max_matches
                );
            };
            self.record_hash_join_probe_metadata(outer_count, max_matches, match_buffer_u32s);
            let telemetry = self.hash_join_telemetry();
            tracing::debug!(
                target: "pg_accel::hash_join",
                phase = "probe",
                probe_batches = telemetry.probe_batches,
                probe_rows = telemetry.last_probe_rows,
                max_matches = telemetry.last_max_matches,
                match_buffer_u32s = telemetry.last_match_buffer_u32s,
                build_rows = telemetry.build_rows,
                build_non_null_rows = telemetry.build_non_null_rows,
                hash_table_capacity = telemetry.hash_table_capacity,
                worker_count = telemetry.worker_count,
                worker_number = telemetry.worker_number,
                key_type = ?self.hash_key_type,
                "exec.hash_join_probe_start"
            );
            let Some(ht) = self.hash_table.as_ref() else {
                pgrx::error!(
                    "pg_accel: GPU hash-join build failed; refusing to fall back to CPU (rule 11)"
                );
            };
            let probe_result = ht.probe(o_keys_ptr, &o_null_mask, max_matches);

            if let Some(pairs) = probe_result {
                let match_count = pairs.len();
                if !hash_join_match_count_within_capacity(match_count, max_matches) {
                    pgrx::error!(
                        "pg_accel: hash-join probe returned {} matches, exceeding max_matches={}; refusing unsafe match buffer consumption",
                        match_count,
                        max_matches,
                    );
                }
                self.record_hash_join_probe_result(match_count);
                let telemetry = self.hash_join_telemetry();
                tracing::debug!(
                    target: "pg_accel::hash_join",
                    phase = "probe",
                    probe_batches = telemetry.probe_batches,
                    probe_rows = telemetry.last_probe_rows,
                    match_count = telemetry.last_match_count,
                    max_matches = telemetry.last_max_matches,
                    match_buffer_u32s = telemetry.last_match_buffer_u32s,
                    worker_count = telemetry.worker_count,
                    worker_number = telemetry.worker_number,
                    "exec.hash_join_probe_finish"
                );
                for (outer_idx, inner_idx) in pairs {
                    let outer_idx = outer_idx as usize;
                    let inner_idx = inner_idx as usize;
                    if inner_idx < self.hash_inner_tuples.len() && outer_idx < outer_tuples.len() {
                        let inner_mt = self.hash_inner_tuples[inner_idx];
                        let outer_mt = outer_tuples[outer_idx];
                        if !inner_mt.is_null() && !outer_mt.is_null() {
                            // SAFETY: Copy both tuples for buffering.
                            let inner_copy = unsafe { pg_sys::heap_copy_minimal_tuple(inner_mt) };
                            let outer_copy = unsafe { pg_sys::heap_copy_minimal_tuple(outer_mt) };
                            self.pending_matches.push(PendingMatch {
                                outer_tuple: outer_copy,
                                inner_tuple: inner_copy,
                            });
                        }
                    }
                }
            } else {
                // Per CLAUDE.md rule 11: no CPU fallback on GPU kernel
                // failure. Previously we silently dropped the outer batch,
                // producing missing join rows. Raise a PG ERROR so the
                // failure is surfaced instead of yielding wrong results.
                let telemetry = self.hash_join_telemetry();
                tracing::warn!(
                    target: "pg_accel::hash_join",
                    phase = "probe",
                    probe_rows = telemetry.last_probe_rows,
                    max_matches = telemetry.last_max_matches,
                    match_buffer_u32s = telemetry.last_match_buffer_u32s,
                    build_rows = telemetry.build_rows,
                    build_non_null_rows = telemetry.build_non_null_rows,
                    hash_table_capacity = telemetry.hash_table_capacity,
                    worker_count = telemetry.worker_count,
                    worker_number = telemetry.worker_number,
                    "exec.hash_join_probe_failed"
                );
                pgrx::error!(
                    "pg_accel: GPU hash-join probe kernel failed on batch of {} outer tuples (max_matches={}, match_buffer_u32s={}); refusing to fall back to CPU (rule 11)",
                    outer_tuples.len(),
                    max_matches,
                    match_buffer_u32s,
                );
            }

            // Free outer tuples (we only need inner tuples for future probes).
            for mt in outer_tuples {
                if !mt.is_null() {
                    // SAFETY: Free owned copy.
                    unsafe { pg_sys::pfree(mt.cast()) };
                }
            }

            let elapsed_us = start.elapsed().as_micros() as u64;
            self.dispatch_time_us += elapsed_us;
            self.batches_executed += 1;

            // Per-backend stats: hash-join probe batch. Count outer rows as
            // dispatched + GPU-processed (the probe kernel ran on all of them).
            stats::record_batch(outer_count as u64, elapsed_us);
            stats::record_gpu_batch(outer_count as u64, 0);

            pgrx::check_for_interrupts!();
            // Loop back to drain pending_matches.
        }
    }

    /// GPU hash join count-only path: build from inner, probe outer batches,
    /// and emit one `COUNT(*)` tuple without materializing joined rows.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    #[allow(clippy::too_many_lines)]
    pub(super) unsafe fn next_hash_join_count(
        &mut self,
        outer_ps: *mut pg_sys::PlanState,
        inner_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        if self.hash_count_returned {
            return std::ptr::null_mut();
        }

        self.record_hash_join_worker_metadata(unsafe { pg_sys::ParallelWorkerNumber });

        let inner_result_slot = if inner_ps.is_null() {
            result_slot
        } else {
            let slot = unsafe { (*inner_ps).ps_ResultTupleSlot };
            if slot.is_null() {
                let ss = inner_ps.cast::<pg_sys::ScanState>();
                let scan_slot = unsafe { (*ss).ss_ScanTupleSlot };
                if scan_slot.is_null() {
                    result_slot
                } else {
                    scan_slot
                }
            } else {
                slot
            }
        };

        if !self.hash_built {
            self.hash_built = true;

            loop {
                let inner_slot = unsafe { pg_sys::ExecProcNode(inner_ps) };
                if inner_slot.is_null() || unsafe { Self::slot_is_empty(inner_slot) } {
                    break;
                }
                let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(inner_slot) };
                self.hash_inner_tuples.push(mt);

                if self.hash_inner_tuples.len().is_multiple_of(10000) {
                    pgrx::check_for_interrupts!();
                }
            }

            let inner_count = self.hash_inner_tuples.len();
            if !hash_join_key_type_supported(self.hash_key_type) {
                pgrx::error!(
                    "pg_accel: hash_join key type {:?} is not implemented by the selected GPU count path; planner should have declined",
                    self.hash_key_type,
                );
            }
            if inner_count == 0 {
                self.record_hash_join_build_metadata(0, 0, 0);
                self.hash_count_returned = true;
                unsafe { Self::emit_hash_join_count(result_slot, 0) };
                return result_slot;
            }
            if !hash_join_row_indices_representable(inner_count) {
                pgrx::error!(
                    "pg_accel: hash-join inner row count {} exceeds u32 index capacity; planner should have declined",
                    inner_count
                );
            }
            let inner_count_u32 = match u32::try_from(inner_count) {
                Ok(count) => count,
                Err(_) => {
                    pgrx::error!(
                        "pg_accel: hash-join inner row count {} exceeds u32 index capacity; planner should have declined",
                        inner_count
                    );
                }
            };

            let inner_tupdesc = unsafe { (*inner_result_slot).tts_tupleDescriptor };
            if inner_tupdesc.is_null() {
                pgrx::error!("pg_accel: hash join inner slot has no tuple descriptor");
            }
            let inner_natts = unsafe { (*inner_tupdesc).natts };
            if self.hash_inner_attno <= 0 || self.hash_inner_attno > inner_natts {
                pgrx::error!(
                    "pg_accel: hash join inner key attno {} out of range 1..={}; refusing CPU fallback",
                    self.hash_inner_attno,
                    inner_natts,
                );
            }

            let inner_info = unsafe { AttExtractInfo::new(inner_tupdesc, self.hash_inner_attno) };
            let indices: Vec<u32> = (0..inner_count_u32).collect();
            let mut int32_keys: Vec<i32> = Vec::new();
            let mut long_keys: Vec<i64> = Vec::new();
            let null_mask: Vec<u8>;

            match self.hash_key_type {
                PgaccelKeyType::Int32 => {
                    let (k, n) = unsafe {
                        tuple_extract::extract_i32(
                            &self.hash_inner_tuples,
                            &inner_info,
                            inner_result_slot,
                        )
                    };
                    int32_keys = k;
                    null_mask = n;
                }
                PgaccelKeyType::Int64 => {
                    let (k, n) = unsafe {
                        tuple_extract::extract_i64(
                            &self.hash_inner_tuples,
                            &inner_info,
                            inner_result_slot,
                        )
                    };
                    long_keys = k;
                    null_mask = n;
                }
                PgaccelKeyType::Float64 | PgaccelKeyType::Uuid | PgaccelKeyType::Inet => {
                    pgrx::error!(
                        "pg_accel: hash_join key type {:?} not implemented; planner should have declined",
                        self.hash_key_type,
                    );
                }
            }

            let build_non_null_rows = hash_join_non_null_rows(&null_mask);
            let Some(hash_table_capacity) = hash_join_table_capacity(build_non_null_rows) else {
                pgrx::error!(
                    "pg_accel: hash-join build capacity overflow for {} non-null inner rows; planner should have declined",
                    build_non_null_rows
                );
            };
            self.record_hash_join_build_metadata(
                inner_count,
                build_non_null_rows,
                hash_table_capacity,
            );
            let keys_ptr: *const std::ffi::c_void = match self.hash_key_type {
                PgaccelKeyType::Int32 => int32_keys.as_ptr().cast(),
                PgaccelKeyType::Int64 => long_keys.as_ptr().cast(),
                PgaccelKeyType::Float64 | PgaccelKeyType::Uuid | PgaccelKeyType::Inet => {
                    pgrx::error!(
                        "pg_accel: hash_join key type {:?} not implemented",
                        self.hash_key_type,
                    )
                }
            };

            self.hash_table =
                GpuHashTable::build(keys_ptr, &null_mask, &indices, self.hash_key_type);
            if self.hash_table.is_none() {
                pgrx::error!(
                    "pg_accel: GPU hash-join count build failed for {} inner rows ({} non-null, planned hash capacity {}); refusing to fall back to CPU",
                    inner_count,
                    build_non_null_rows,
                    hash_table_capacity,
                );
            }
        }

        let mut total_matches: usize = 0;
        loop {
            let mut outer_tuples: Vec<pg_sys::MinimalTuple> = Vec::with_capacity(self.batch_size);
            for _ in 0..self.batch_size {
                let outer_slot = unsafe { pg_sys::ExecProcNode(outer_ps) };
                if outer_slot.is_null() || unsafe { Self::slot_is_empty(outer_slot) } {
                    self.outer_exhausted = true;
                    break;
                }
                let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(outer_slot) };
                outer_tuples.push(mt);
            }

            if outer_tuples.is_empty() {
                break;
            }

            let start = std::time::Instant::now();
            let outer_count = outer_tuples.len();
            self.rows_dispatched = self.rows_dispatched.saturating_add(outer_count as u64);

            if !hash_join_row_indices_representable(outer_count) {
                for mt in outer_tuples {
                    if !mt.is_null() {
                        unsafe { pg_sys::pfree(mt.cast()) };
                    }
                }
                pgrx::error!(
                    "pg_accel: hash-join outer batch row count {} exceeds u32 index capacity; planner should have declined",
                    outer_count
                );
            }

            let outer_extract_slot = if self.hash_outer_slot.is_null() {
                result_slot
            } else {
                self.hash_outer_slot
            };
            let outer_tupdesc = unsafe { (*outer_extract_slot).tts_tupleDescriptor };
            if outer_tupdesc.is_null() {
                pgrx::error!("pg_accel: hash join outer slot has no tuple descriptor");
            }
            let outer_natts = unsafe { (*outer_tupdesc).natts };
            if self.hash_outer_attno <= 0 || self.hash_outer_attno > outer_natts {
                pgrx::error!(
                    "pg_accel: hash join outer key attno {} out of range 1..={}; refusing CPU fallback",
                    self.hash_outer_attno,
                    outer_natts,
                );
            }
            let outer_info = unsafe { AttExtractInfo::new(outer_tupdesc, self.hash_outer_attno) };

            let mut o_int32_keys: Vec<i32> = Vec::new();
            let mut o_long_keys: Vec<i64> = Vec::new();
            let o_null_mask: Vec<u8>;
            match self.hash_key_type {
                PgaccelKeyType::Int32 => {
                    let (k, n) = unsafe {
                        tuple_extract::extract_i32(&outer_tuples, &outer_info, outer_extract_slot)
                    };
                    o_int32_keys = k;
                    o_null_mask = n;
                }
                PgaccelKeyType::Int64 => {
                    let (k, n) = unsafe {
                        tuple_extract::extract_i64(&outer_tuples, &outer_info, outer_extract_slot)
                    };
                    o_long_keys = k;
                    o_null_mask = n;
                }
                PgaccelKeyType::Float64 | PgaccelKeyType::Uuid | PgaccelKeyType::Inet => {
                    pgrx::error!(
                        "pg_accel: hash_join outer key type {:?} not implemented; planner should have declined",
                        self.hash_key_type,
                    );
                }
            }

            let o_keys_ptr: *const std::ffi::c_void = match self.hash_key_type {
                PgaccelKeyType::Int32 => o_int32_keys.as_ptr().cast(),
                PgaccelKeyType::Int64 => o_long_keys.as_ptr().cast(),
                PgaccelKeyType::Float64 | PgaccelKeyType::Uuid | PgaccelKeyType::Inet => {
                    pgrx::error!(
                        "pg_accel: hash_join outer key type {:?} not implemented",
                        self.hash_key_type,
                    )
                }
            };

            self.record_hash_join_probe_metadata(outer_count, 0, 0);
            let Some(ht) = self.hash_table.as_ref() else {
                pgrx::error!(
                    "pg_accel: GPU hash-join count build failed; refusing to fall back to CPU"
                );
            };
            let Some(batch_matches) = ht.count_matches(o_keys_ptr, &o_null_mask) else {
                for mt in outer_tuples {
                    if !mt.is_null() {
                        unsafe { pg_sys::pfree(mt.cast()) };
                    }
                }
                pgrx::error!(
                    "pg_accel: GPU hash-join count probe failed on batch of {} outer tuples; refusing CPU fallback",
                    outer_count,
                );
            };
            self.record_hash_join_probe_result(batch_matches);
            total_matches = if let Some(total) = total_matches.checked_add(batch_matches) {
                total
            } else {
                for mt in outer_tuples {
                    if !mt.is_null() {
                        unsafe { pg_sys::pfree(mt.cast()) };
                    }
                }
                pgrx::error!("pg_accel: hash-join COUNT(*) overflowed usize");
            };

            for mt in outer_tuples {
                if !mt.is_null() {
                    unsafe { pg_sys::pfree(mt.cast()) };
                }
            }

            let elapsed_us = start.elapsed().as_micros() as u64;
            self.dispatch_time_us = self.dispatch_time_us.saturating_add(elapsed_us);
            self.batches_executed = self.batches_executed.saturating_add(1);
            stats::record_batch(outer_count as u64, elapsed_us);
            stats::record_gpu_batch(outer_count as u64, 0);

            pgrx::check_for_interrupts!();
            if self.outer_exhausted {
                break;
            }
        }

        self.hash_count_returned = true;
        if total_matches > i64::MAX as usize {
            pgrx::error!("pg_accel: hash-join COUNT(*) result exceeds int8");
        }
        unsafe { Self::emit_hash_join_count(result_slot, total_matches as i64) };
        result_slot
    }

    unsafe fn emit_hash_join_count(result_slot: *mut pg_sys::TupleTableSlot, count: i64) {
        if result_slot.is_null() {
            pgrx::error!("pg_accel: hash-join count output slot is null");
        }
        unsafe {
            pg_sys::ExecClearTuple(result_slot);
            let tupdesc = (*result_slot).tts_tupleDescriptor;
            if tupdesc.is_null() || (*tupdesc).natts < 1 {
                pgrx::error!("pg_accel: hash-join count output slot has no count column");
            }
            *(*result_slot).tts_values = pg_sys::Datum::from(count);
            *(*result_slot).tts_isnull = false;
            let natts = (*tupdesc).natts as usize;
            for i in 1..natts {
                *(*result_slot).tts_values.add(i) = pg_sys::Datum::from(0);
                *(*result_slot).tts_isnull.add(i) = true;
            }
            pg_sys::ExecStoreVirtualTuple(result_slot);
        }
    }
}
