//! Hash-join build phase — slot mapping and context setup.

use pgrx::pg_sys;

use crate::adapters::extractors::geometry::extract_geometry;
use crate::engine::gucs;
use crate::engine::materialize::tuple_extract::{self, AttExtractInfo};
use crate::engine::registry::{self, AccelStrategy};
use crate::engine::stats;
use crate::gpu::{GpuHashTable, PgaccelKeyType, three_layer};

use super::{JoinExecState, NLJ_SHAPE_BETWEEN, TlistMapEntry};

impl JoinExecState {
    /// Configure hash join context.
    ///
    /// `outer_attno` and `inner_attno` are 1-based attribute numbers for the
    /// join key columns in the outer and inner relations, respectively.
    pub fn set_hash_join_context(
        &mut self,
        outer_attno: i32,
        inner_attno: i32,
        key_type: PgaccelKeyType,
    ) {
        self.hash_outer_attno = outer_attno;
        self.hash_inner_attno = inner_attno;
        self.hash_key_type = key_type;
        tracing::debug!(
            target: "pg_accel::hash_join",
            phase = "context",
            outer_attno,
            inner_attno,
            key_type = ?key_type,
            worker_count = 1u32,
            "exec.hash_join_context"
        );
    }

    /// Configure count-only output for `COUNT(*)` over `GpuHashJoin`.
    pub fn set_hash_join_count_mode(&mut self, count_only: bool) {
        self.hash_count_only = count_only;
        tracing::debug!(
            target: "pg_accel::hash_join",
            phase = "context",
            count_only,
            "exec.hash_join_count_mode"
        );
    }

    /// Configure the one-column BETWEEN/range-containment NLJ context.
    ///
    /// The supported shape is:
    ///
    /// ```text
    /// child0.value BETWEEN child1.lo AND child1.hi
    /// ```
    pub fn set_nlj_between_context(
        &mut self,
        outer_value_attno: i32,
        inner_lo_attno: i32,
        inner_hi_attno: i32,
        key_type: PgaccelKeyType,
    ) {
        self.nlj_shape = NLJ_SHAPE_BETWEEN;
        self.nlj_outer_value_attno = outer_value_attno;
        self.nlj_inner_lo_attno = inner_lo_attno;
        self.nlj_inner_hi_attno = inner_hi_attno;
        self.nlj_key_type = key_type;
        tracing::debug!(
            target: "pg_accel::nlj_ineq",
            phase = "context",
            outer_value_attno,
            inner_lo_attno,
            inner_hi_attno,
            key_type = ?key_type,
            "exec.nlj_between_context"
        );
    }

    /// Build the scan-slot → child-plan attribute mapping from
    /// `custom_scan_tlist` and child plan states. Must be called from
    /// `begin_custom_scan` after child PlanStates are initialized.
    ///
    /// # Safety
    ///
    /// `cscan`, `outer_ps`, and `inner_ps` must be valid planner/executor
    /// pointers on the main backend thread.
    pub unsafe fn init_hash_join_slots(
        &mut self,
        cscan: *mut pg_sys::CustomScan,
        outer_ps: *mut pg_sys::PlanState,
        inner_ps: *mut pg_sys::PlanState,
    ) {
        // Read child plans' scanrelid to map Var.varno → child index.
        // Build mapping from custom_scan_tlist.
        // custom_scan_tlist Vars have original relation varnos and attnos.
        // We search each child plan's output list to find where each
        // (varno, varattno) lands, handling both SeqScan children (with
        // scanrelid) and nested CustomScan children (scanrelid=0).
        // SAFETY: cscan is valid.
        let tlist = unsafe { (*cscan).custom_scan_tlist };
        if !tlist.is_null() {
            // SAFETY: `tlist` is the live PostgreSQL List owned by `cscan`.
            let tlen = unsafe { pg_sys::list_length(tlist) };
            for j in 0..tlen {
                // SAFETY: `j` is bounded by list_length, so this cell exists;
                // custom_scan_tlist cells contain TargetEntry pointers.
                let tle = unsafe { pg_sys::list_nth(tlist, j).cast::<pg_sys::TargetEntry>() };
                if tle.is_null() {
                    self.tlist_map.push(TlistMapEntry {
                        child_idx: 0,
                        child_attno: 0,
                    });
                    continue;
                }
                // SAFETY: a non-null custom_scan_tlist entry has TargetEntry
                // layout, so its expression field is valid to read.
                let expr = unsafe { (*tle).expr };
                if !expr.is_null()
                    && unsafe {
                        // SAFETY: every PostgreSQL Expr begins with a NodeTag;
                        // the null check proves the node pointer is readable.
                        (*expr.cast::<pg_sys::Node>()).type_
                    } == pg_sys::NodeTag::T_Var
                {
                    let var = expr.cast::<pg_sys::Var>();
                    // SAFETY: the checked T_Var tag proves `expr` has Var layout.
                    let varno = unsafe { (*var).varno };
                    // SAFETY: the checked T_Var tag proves `expr` has Var layout.
                    let varattno = unsafe { (*var).varattno };

                    // Handle already-remapped Vars (INNER_VAR/OUTER_VAR).
                    if varno == pg_sys::INNER_VAR {
                        self.tlist_map.push(TlistMapEntry {
                            child_idx: 1,
                            child_attno: varattno,
                        });
                        continue;
                    }
                    if varno == pg_sys::OUTER_VAR {
                        self.tlist_map.push(TlistMapEntry {
                            child_idx: 0,
                            child_attno: varattno,
                        });
                        continue;
                    }

                    // Original relation varno — search each child for this
                    // (varno, varattno) pair to determine child index and
                    // remapped output position.
                    // SAFETY: outer_ps and inner_ps are valid PlanState ptrs.
                    let inner_pos =
                        unsafe { Self::find_child_output_pos(inner_ps, varno, varattno) };
                    if inner_pos > 0 {
                        self.tlist_map.push(TlistMapEntry {
                            child_idx: 1,
                            child_attno: inner_pos,
                        });
                    } else {
                        // SAFETY: `outer_ps` is the live outer child PlanState;
                        // `varno` and `varattno` came from the checked Var node.
                        let outer_pos =
                            unsafe { Self::find_child_output_pos(outer_ps, varno, varattno) };
                        self.tlist_map.push(TlistMapEntry {
                            child_idx: 0,
                            child_attno: if outer_pos > 0 { outer_pos } else { varattno },
                        });
                    }
                } else {
                    self.tlist_map.push(TlistMapEntry {
                        child_idx: 0,
                        child_attno: 0,
                    });
                }
            }
        }

        // Create temporary slots from child plan descriptors.
        // SAFETY: Child plan states have valid result slots.
        if !outer_ps.is_null() {
            let outer_desc = unsafe {
                // SAFETY: the caller supplies a live outer PlanState. Its
                // initialized result slot, or scan slot when absent, owns the
                // tuple descriptor used to construct our minimal-tuple slot.
                let slot = (*outer_ps).ps_ResultTupleSlot;
                if slot.is_null() {
                    let ss = outer_ps.cast::<pg_sys::ScanState>();
                    (*(*ss).ss_ScanTupleSlot).tts_tupleDescriptor
                } else {
                    (*slot).tts_tupleDescriptor
                }
            };
            if !outer_desc.is_null() {
                self.hash_outer_slot = unsafe {
                    // SAFETY: `outer_desc` is a live child descriptor and the
                    // selected PostgreSQL slot ops accept MinimalTuples.
                    pg_sys::MakeSingleTupleTableSlot(
                        outer_desc,
                        &raw const pg_sys::TTSOpsMinimalTuple,
                    )
                };
            }
        }
        if !inner_ps.is_null() {
            let inner_desc = unsafe {
                // SAFETY: the caller supplies a live inner PlanState. Its
                // initialized result slot, or scan slot when absent, owns the
                // tuple descriptor used to construct our minimal-tuple slot.
                let slot = (*inner_ps).ps_ResultTupleSlot;
                if slot.is_null() {
                    let ss = inner_ps.cast::<pg_sys::ScanState>();
                    (*(*ss).ss_ScanTupleSlot).tts_tupleDescriptor
                } else {
                    (*slot).tts_tupleDescriptor
                }
            };
            if !inner_desc.is_null() {
                self.hash_inner_slot = unsafe {
                    // SAFETY: `inner_desc` is a live child descriptor and the
                    // selected PostgreSQL slot ops accept MinimalTuples.
                    pg_sys::MakeSingleTupleTableSlot(
                        inner_desc,
                        &raw const pg_sys::TTSOpsMinimalTuple,
                    )
                };
            }
        }
        tracing::debug!(
            target: "pg_accel::hash_join",
            phase = "slots",
            tlist_map_len = self.tlist_map.len(),
            has_outer_slot = !self.hash_outer_slot.is_null(),
            has_inner_slot = !self.hash_inner_slot.is_null(),
            "exec.hash_join_slots"
        );
    }
}
