//! Nested-loop inequality join executor.
//!
//! Minimal selected slice: one-column range containment,
//! `outer.value BETWEEN inner.lo AND inner.hi`, on inner joins only.

use pgrx::pg_sys;

use crate::engine::materialize::tuple_extract::{self, AttExtractInfo};
use crate::engine::registry::AccelStrategy;
use crate::engine::stats;
use crate::gpu::{self, NljDispatchResult, NljPair, PgaccelKeyType};

use super::{JoinExecState, NLJ_SHAPE_BETWEEN, PendingMatch};

impl JoinExecState {
    /// GPU nested-loop range containment:
    /// `outer.value BETWEEN inner.lo AND inner.hi`.
    ///
    /// The node consumes both children once, dispatches the GPU pair kernel,
    /// then yields projected joined rows from buffered MinimalTuple pairs.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `outer_ps`, `inner_ps`, and
    /// `result_slot` must be valid executor pointers.
    #[allow(clippy::too_many_lines)]
    pub(super) unsafe fn next_nlj_ineq(
        &mut self,
        outer_ps: *mut pg_sys::PlanState,
        inner_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        loop {
            if self.pending_cursor < self.pending_matches.len() {
                let m = &self.pending_matches[self.pending_cursor];
                self.pending_cursor += 1;

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
                    self.store_join_projection(result_slot);
                }
                return result_slot;
            }

            self.free_pending_matches();
            self.pending_matches.clear();
            self.pending_cursor = 0;

            if self.nlj_dispatched {
                return std::ptr::null_mut();
            }

            self.nlj_dispatched = true;
            let start = std::time::Instant::now();

            if self.strategy != AccelStrategy::GpuNestedLoopIneq
                || self.nlj_shape != NLJ_SHAPE_BETWEEN
            {
                pgrx::error!("pg_accel: unsupported NLJ inequality shape reached executor");
            }

            let outer_slot = Self::plan_result_slot(outer_ps, result_slot);
            let inner_slot = Self::plan_result_slot(inner_ps, result_slot);
            let outer_tupdesc = unsafe { (*outer_slot).tts_tupleDescriptor };
            let inner_tupdesc = unsafe { (*inner_slot).tts_tupleDescriptor };
            Self::validate_attno(outer_tupdesc, self.nlj_outer_value_attno, "outer value");
            Self::validate_attno(inner_tupdesc, self.nlj_inner_lo_attno, "inner lower");
            Self::validate_attno(inner_tupdesc, self.nlj_inner_hi_attno, "inner upper");

            let outer_tuples = unsafe { Self::collect_child_tuples(outer_ps) };
            let inner_tuples = unsafe { Self::collect_child_tuples(inner_ps) };
            self.rows_dispatched = outer_tuples.len() as u64;
            Self::validate_nlj_kernel_indices(outer_tuples.len(), inner_tuples.len());

            if outer_tuples.is_empty() || inner_tuples.is_empty() {
                Self::free_tuple_vec(outer_tuples);
                Self::free_tuple_vec(inner_tuples);
                self.batches_executed = self.batches_executed.saturating_add(1);
                return std::ptr::null_mut();
            }

            let max_pairs = self.nlj_max_pairs();
            let pairs = match self.nlj_key_type {
                PgaccelKeyType::Int32 | PgaccelKeyType::Int64 => unsafe {
                    self.dispatch_between_i64(
                        &outer_tuples,
                        &inner_tuples,
                        outer_tupdesc,
                        inner_tupdesc,
                        outer_slot,
                        inner_slot,
                        max_pairs,
                    )
                },
                PgaccelKeyType::Float64 => unsafe {
                    self.dispatch_between_f64(
                        &outer_tuples,
                        &inner_tuples,
                        outer_tupdesc,
                        inner_tupdesc,
                        outer_slot,
                        inner_slot,
                        max_pairs,
                    )
                },
                PgaccelKeyType::Uuid | PgaccelKeyType::Inet => {
                    Self::free_tuple_vec(outer_tuples);
                    Self::free_tuple_vec(inner_tuples);
                    pgrx::error!("pg_accel: unsupported NLJ key type reached executor");
                }
            };

            match pairs {
                NljDispatchResult::Pairs(pairs) => {
                    for pair in pairs {
                        let outer_idx = pair.outer as usize;
                        let inner_idx = pair.inner as usize;
                        if outer_idx < outer_tuples.len() && inner_idx < inner_tuples.len() {
                            let outer_copy = unsafe {
                                crate::engine::pg_compat::heap_copy_minimal_tuple(
                                    outer_tuples[outer_idx],
                                )
                            };
                            let inner_copy = unsafe {
                                crate::engine::pg_compat::heap_copy_minimal_tuple(
                                    inner_tuples[inner_idx],
                                )
                            };
                            self.pending_matches.push(PendingMatch {
                                outer_tuple: outer_copy,
                                inner_tuple: inner_copy,
                            });
                        }
                    }
                }
                NljDispatchResult::Overflow { observed, cap } => {
                    Self::free_tuple_vec(outer_tuples);
                    Self::free_tuple_vec(inner_tuples);
                    pgrx::error!(
                        "pg_accel: GPU NLJ produced {observed} matches above cap {cap}; refusing truncated output"
                    );
                }
            }

            Self::free_tuple_vec(outer_tuples);
            Self::free_tuple_vec(inner_tuples);

            let elapsed_us = start.elapsed().as_micros() as u64;
            self.dispatch_time_us = self.dispatch_time_us.saturating_add(elapsed_us);
            self.batches_executed = self.batches_executed.saturating_add(1);
            stats::record_batch(self.rows_dispatched, elapsed_us);
            stats::record_gpu_batch(self.rows_dispatched, 0);
        }
    }

    fn nlj_max_pairs(&self) -> usize {
        crate::engine::cost::device_limits().gpu_nlj_max_output_rows
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn dispatch_between_i64(
        &self,
        outer_tuples: &[pg_sys::MinimalTuple],
        inner_tuples: &[pg_sys::MinimalTuple],
        outer_tupdesc: pg_sys::TupleDesc,
        inner_tupdesc: pg_sys::TupleDesc,
        outer_slot: *mut pg_sys::TupleTableSlot,
        inner_slot: *mut pg_sys::TupleTableSlot,
        max_pairs: usize,
    ) -> NljDispatchResult {
        let outer_info = unsafe { AttExtractInfo::new(outer_tupdesc, self.nlj_outer_value_attno) };
        let lo_info = unsafe { AttExtractInfo::new(inner_tupdesc, self.nlj_inner_lo_attno) };
        let hi_info = unsafe { AttExtractInfo::new(inner_tupdesc, self.nlj_inner_hi_attno) };

        let (outer_keys, outer_nulls) =
            unsafe { extract_integral_as_i64(outer_tuples, &outer_info, outer_slot) };
        let (inner_lo, lo_nulls) =
            unsafe { extract_integral_as_i64(inner_tuples, &lo_info, inner_slot) };
        let (inner_hi, hi_nulls) =
            unsafe { extract_integral_as_i64(inner_tuples, &hi_info, inner_slot) };

        let (outer_keys, outer_map) = filter_i64_keys(&outer_keys, &outer_nulls);
        let (inner_lo, inner_hi, inner_map) =
            filter_i64_ranges(&inner_lo, &inner_hi, &lo_nulls, &hi_nulls);

        let dispatch = gpu::dispatch_between_i64(&outer_keys, &inner_lo, &inner_hi, max_pairs)
            .unwrap_or_else(|err| pgrx::error!("pg_accel: GPU NLJ i64 dispatch failed: {err}"));
        remap_pairs(dispatch, &outer_map, &inner_map)
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn dispatch_between_f64(
        &self,
        outer_tuples: &[pg_sys::MinimalTuple],
        inner_tuples: &[pg_sys::MinimalTuple],
        outer_tupdesc: pg_sys::TupleDesc,
        inner_tupdesc: pg_sys::TupleDesc,
        outer_slot: *mut pg_sys::TupleTableSlot,
        inner_slot: *mut pg_sys::TupleTableSlot,
        max_pairs: usize,
    ) -> NljDispatchResult {
        let outer_info = unsafe { AttExtractInfo::new(outer_tupdesc, self.nlj_outer_value_attno) };
        let lo_info = unsafe { AttExtractInfo::new(inner_tupdesc, self.nlj_inner_lo_attno) };
        let hi_info = unsafe { AttExtractInfo::new(inner_tupdesc, self.nlj_inner_hi_attno) };

        let (outer_keys, outer_nulls) =
            unsafe { tuple_extract::extract_f64(outer_tuples, &outer_info, outer_slot) };
        let (inner_lo, lo_nulls) =
            unsafe { tuple_extract::extract_f64(inner_tuples, &lo_info, inner_slot) };
        let (inner_hi, hi_nulls) =
            unsafe { tuple_extract::extract_f64(inner_tuples, &hi_info, inner_slot) };

        let (outer_keys, outer_map) = filter_f64_keys(&outer_keys, &outer_nulls);
        let (inner_lo, inner_hi, inner_map) =
            filter_f64_ranges(&inner_lo, &inner_hi, &lo_nulls, &hi_nulls);

        let dispatch = gpu::dispatch_between_f64(&outer_keys, &inner_lo, &inner_hi, max_pairs)
            .unwrap_or_else(|err| pgrx::error!("pg_accel: GPU NLJ f64 dispatch failed: {err}"));
        remap_pairs(dispatch, &outer_map, &inner_map)
    }

    unsafe fn collect_child_tuples(ps: *mut pg_sys::PlanState) -> Vec<pg_sys::MinimalTuple> {
        let mut tuples = Vec::new();
        loop {
            let slot = unsafe { pg_sys::ExecProcNode(ps) };
            if slot.is_null() || unsafe { Self::slot_is_empty(slot) } {
                break;
            }
            tuples.push(unsafe { pg_sys::ExecCopySlotMinimalTuple(slot) });
            if tuples.len().is_multiple_of(10_000) {
                pgrx::check_for_interrupts!();
            }
        }
        tuples
    }

    fn free_tuple_vec(tuples: Vec<pg_sys::MinimalTuple>) {
        for mt in tuples {
            if !mt.is_null() {
                unsafe { pg_sys::pfree(mt.cast()) };
            }
        }
    }

    fn validate_attno(tupdesc: pg_sys::TupleDesc, attno: i32, label: &str) {
        if tupdesc.is_null() {
            pgrx::error!("pg_accel: NLJ {label} slot has no tuple descriptor");
        }
        let natts = unsafe { (*tupdesc).natts };
        if attno <= 0 || attno > natts {
            pgrx::error!(
                "pg_accel: NLJ {label} attno {attno} out of range 1..={natts}; refusing CPU fallback"
            );
        }
    }

    fn validate_nlj_kernel_indices(outer_len: usize, inner_len: usize) {
        let max_index = u32::MAX as usize;
        if outer_len > max_index || inner_len > max_index {
            pgrx::error!(
                "pg_accel: NLJ input rows exceed GPU pair-index range \
                 outer_rows={outer_len} inner_rows={inner_len} max_index={max_index}; \
                 refusing truncated output"
            );
        }
    }

    fn plan_result_slot(
        ps: *mut pg_sys::PlanState,
        fallback: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        if ps.is_null() {
            return fallback;
        }
        let slot = unsafe { (*ps).ps_ResultTupleSlot };
        if !slot.is_null() {
            return slot;
        }
        let scan_slot = unsafe { (*ps.cast::<pg_sys::ScanState>()).ss_ScanTupleSlot };
        if scan_slot.is_null() {
            fallback
        } else {
            scan_slot
        }
    }

    unsafe fn store_join_projection(&self, result_slot: *mut pg_sys::TupleTableSlot) {
        unsafe {
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
    }
}

unsafe fn extract_integral_as_i64(
    tuples: &[pg_sys::MinimalTuple],
    info: &AttExtractInfo,
    slot: *mut pg_sys::TupleTableSlot,
) -> (Vec<i64>, Vec<u8>) {
    if matches!(info.typid, pg_sys::INT2OID | pg_sys::INT4OID) {
        let (values, nulls) = unsafe { tuple_extract::extract_i32(tuples, info, slot) };
        (values.into_iter().map(i64::from).collect(), nulls)
    } else {
        unsafe { tuple_extract::extract_i64(tuples, info, slot) }
    }
}

fn filter_i64_keys(values: &[i64], nulls: &[u8]) -> (Vec<i64>, Vec<u32>) {
    let mut out = Vec::with_capacity(values.len());
    let mut map = Vec::with_capacity(values.len());
    for (idx, (&value, &is_null)) in values.iter().zip(nulls).enumerate() {
        if is_null == 0 {
            out.push(value);
            map.push(idx as u32);
        }
    }
    (out, map)
}

fn filter_i64_ranges(
    lo: &[i64],
    hi: &[i64],
    lo_nulls: &[u8],
    hi_nulls: &[u8],
) -> (Vec<i64>, Vec<i64>, Vec<u32>) {
    let mut out_lo = Vec::with_capacity(lo.len());
    let mut out_hi = Vec::with_capacity(hi.len());
    let mut map = Vec::with_capacity(lo.len());
    for idx in 0..lo
        .len()
        .min(hi.len())
        .min(lo_nulls.len())
        .min(hi_nulls.len())
    {
        if lo_nulls[idx] == 0 && hi_nulls[idx] == 0 {
            out_lo.push(lo[idx]);
            out_hi.push(hi[idx]);
            map.push(idx as u32);
        }
    }
    (out_lo, out_hi, map)
}

fn filter_f64_keys(values: &[f64], nulls: &[u8]) -> (Vec<f64>, Vec<u32>) {
    let mut out = Vec::with_capacity(values.len());
    let mut map = Vec::with_capacity(values.len());
    for (idx, (&value, &is_null)) in values.iter().zip(nulls).enumerate() {
        if is_null == 0 {
            out.push(value);
            map.push(idx as u32);
        }
    }
    (out, map)
}

fn filter_f64_ranges(
    lo: &[f64],
    hi: &[f64],
    lo_nulls: &[u8],
    hi_nulls: &[u8],
) -> (Vec<f64>, Vec<f64>, Vec<u32>) {
    let mut out_lo = Vec::with_capacity(lo.len());
    let mut out_hi = Vec::with_capacity(hi.len());
    let mut map = Vec::with_capacity(lo.len());
    for idx in 0..lo
        .len()
        .min(hi.len())
        .min(lo_nulls.len())
        .min(hi_nulls.len())
    {
        if lo_nulls[idx] == 0 && hi_nulls[idx] == 0 {
            out_lo.push(lo[idx]);
            out_hi.push(hi[idx]);
            map.push(idx as u32);
        }
    }
    (out_lo, out_hi, map)
}

fn remap_pairs(
    result: NljDispatchResult,
    outer_map: &[u32],
    inner_map: &[u32],
) -> NljDispatchResult {
    match result {
        NljDispatchResult::Pairs(pairs) => NljDispatchResult::Pairs(
            pairs
                .into_iter()
                .filter_map(|pair| {
                    Some(NljPair {
                        outer: *outer_map.get(pair.outer as usize)?,
                        inner: *inner_map.get(pair.inner as usize)?,
                    })
                })
                .collect(),
        ),
        overflow @ NljDispatchResult::Overflow { .. } => overflow,
    }
}
