//! Heap scan mechanics — arena fill + per-row materialization.

use pgrx::pg_sys;

use crate::engine::columnar::{ColumnarBatchOwner, CpuBoundaryReason};
use crate::engine::gucs;
use crate::engine::materialize::tuple_extract::AttExtractInfo;

use super::ScanExecState;

struct DirectExprColumn {
    attno: i32,
    typid: pg_sys::Oid,
    values: Vec<f64>,
    nulls: Vec<u8>,
}

impl DirectExprColumn {
    fn new(info: AttExtractInfo, capacity: usize) -> Self {
        Self {
            attno: (info.att_index() + 1) as i32,
            typid: info.typid,
            values: Vec::with_capacity(capacity),
            nulls: Vec::with_capacity(capacity),
        }
    }

    #[allow(clippy::cast_precision_loss)]
    unsafe fn push_slot_value(&mut self, scan_slot: *mut pg_sys::TupleTableSlot) -> bool {
        let mut is_null = false;
        // SAFETY: the direct-scan slot is live and `attno` was derived from its
        // matching tuple descriptor before this column collector was created.
        let datum = unsafe { pg_sys::slot_getattr(scan_slot, self.attno, &raw mut is_null) };
        if is_null {
            self.values.push(0.0);
            self.nulls.push(1);
            return true;
        }

        let value = match self.typid {
            oid if oid == pg_sys::FLOAT4OID => f64::from(f32::from_bits(datum.value() as u32)),
            oid if oid == pg_sys::INT2OID => (datum.value() as i16) as f64,
            oid if oid == pg_sys::INT4OID => (datum.value() as i32) as f64,
            oid if oid == pg_sys::INT8OID => (datum.value() as i64) as f64,
            oid if oid == pg_sys::FLOAT8OID => f64::from_bits(datum.value() as u64),
            _ => return false,
        };
        self.values.push(value);
        self.nulls.push(0);
        true
    }
}

impl ScanExecState {
    /// Pull tuples from the child plan (or direct heap scan) until the
    /// batch is full or the source is exhausted.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` must be valid
    /// when using child-plan mode (scan_desc is null). `output_slot` must be
    /// valid; direct-scan mode uses the executor's private relation slot.
    pub(super) unsafe fn fill_batch(
        &mut self,
        child_ps: *mut pg_sys::PlanState,
        output_slot: *mut pg_sys::TupleTableSlot,
    ) {
        let direct_scan_slot = self.direct_scan_slot();
        if !direct_scan_slot.is_null() {
            // Clear before resetting the batch memory context. The direct
            // scan slot may still reference the prior batch through fallback
            // extraction or the previous table_scan_getnextslot call.
            // SAFETY: the non-null executor-owned slot remains live until this
            // scan state is dropped and is cleared before its context reset.
            unsafe { pg_sys::ExecClearTuple(direct_scan_slot) };
        }
        let direct_minimal_slot = self.direct_minimal_slot();
        if !direct_minimal_slot.is_null() {
            // SAFETY: the non-null executor-owned slot remains live until this
            // scan state is dropped and is cleared before its context reset.
            unsafe { pg_sys::ExecClearTuple(direct_minimal_slot) };
        }

        // Free previously-buffered allocations.
        if self.batch_mcxt.is_null() {
            // Fallback for non-direct-scan paths: individual pfree.
            for &mt in &self.tuple_buffer {
                if !mt.is_null() {
                    // SAFETY: mt was palloc'd by ExecCopySlotMinimalTuple.
                    unsafe { pg_sys::pfree(mt.cast()) };
                }
            }
            // Free previously-copied datums (varlena from datumCopy).
            for &(datum, is_null) in &self.datum_buffer {
                if !is_null && datum.value() != 0 {
                    // SAFETY: non-null entries in `datum_buffer` are palloc'd
                    // copies returned by pg_detoast_datum_copy and owned here.
                    unsafe { pg_sys::pfree(datum.cast_mut_ptr()) };
                }
            }
        } else {
            // SAFETY: batch_mcxt is a valid PG memory context we own.
            // MemoryContextReset frees all allocations in O(1) by resetting
            // the context's block list, instead of O(n) individual pfree calls.
            unsafe { pg_sys::MemoryContextReset(self.batch_mcxt) };
        }
        self.clear_batch_buffers();

        let target = self.batch_size.max(gucs::min_batch_size().max(1) as usize);

        if self.scan_desc.is_null() {
            // Child plan scan (spatial/h3/raster).
            // SAFETY: child-plan mode requires the caller-provided `child_ps`
            // to remain live on this backend thread for the fill operation.
            unsafe { self.fill_batch_child(child_ps, target) };
        } else {
            // Direct heap scan (GpuExpr with scanrelid > 0).
            // table_scan_getnextslot writes the heap tuple into scan_slot,
            // which has the table's full TupleDesc. MinimalTuples copied
            // from this slot match the TupleDesc used by extract_col_f64.
            let scan_slot = if direct_scan_slot.is_null() {
                output_slot
            } else {
                direct_scan_slot
            };
            // SAFETY: direct mode proves `self.scan_desc` is live, and the
            // selected executor slot remains valid for the table scan.
            unsafe { self.fill_batch_direct(scan_slot, target) };
        }
    }

    /// Direct table scan path for fill_batch.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `scan_slot` and
    /// `self.scan_desc` must be valid.
    unsafe fn fill_batch_direct(&mut self, scan_slot: *mut pg_sys::TupleTableSlot, target: usize) {
        // SAFETY: direct scan mode supplies a live slot whose descriptor
        // matches the relation represented by `self.scan_desc`.
        let mut direct_expr_columns = unsafe { self.begin_direct_expr_columns(scan_slot, target) };

        // Switch to batch memory context for all MinimalTuple allocations.
        // SAFETY: batch_mcxt was created in set_scan_desc and is valid.
        let old_mcxt = if self.batch_mcxt.is_null() {
            std::ptr::null_mut()
        } else {
            // SAFETY: `batch_mcxt` is an executor-owned live memory context;
            // the returned prior context is restored before this method exits.
            unsafe { pg_sys::MemoryContextSwitchTo(self.batch_mcxt) }
        };

        while self.tuple_buffer.len() < target {
            let got_tuple = unsafe {
                // SAFETY: direct mode keeps `scan_desc` and `scan_slot` live on
                // the backend thread for PostgreSQL's forward table scan.
                pg_sys::table_scan_getnextslot(
                    self.scan_desc,
                    pg_sys::ScanDirection::ForwardScanDirection,
                    scan_slot,
                )
            };
            if !got_tuple {
                self.child_exhausted = true;
                break;
            }

            if let Some(columns) = direct_expr_columns.as_mut() {
                let mut ok = true;
                for column in columns {
                    // SAFETY: `scan_slot` contains the row just fetched, and
                    // each collector's attno came from its tuple descriptor.
                    if !unsafe { column.push_slot_value(scan_slot) } {
                        ok = false;
                        break;
                    }
                }
                if !ok {
                    direct_expr_columns = None;
                }
            }

            // Copy the live table-scan slot for final PostgreSQL output.
            // Expression input columns were already captured above, before
            // table_scan_getnextslot reuses the slot for the next row.
            // SAFETY: scan_slot is valid and non-empty.
            let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(scan_slot) };
            self.tuple_buffer.push(mt);
        }

        // Restore previous memory context.
        if !old_mcxt.is_null() {
            // SAFETY: old_mcxt is the context we saved above.
            unsafe { pg_sys::MemoryContextSwitchTo(old_mcxt) };
        }

        self.expr_batch_owner = direct_expr_columns.and_then(|columns| {
            let row_count = self.tuple_buffer.len();
            let mut owner = ColumnarBatchOwner::new(row_count, columns.len());
            owner.mark_host_boundary(CpuBoundaryReason::HostInputStaging);
            for column in columns {
                if column.values.len() != row_count || column.nulls.len() != row_count {
                    return None;
                }
                owner.add_col_f64(column.values, column.nulls);
            }
            Some(owner)
        });
    }

    unsafe fn begin_direct_expr_columns(
        &self,
        scan_slot: *mut pg_sys::TupleTableSlot,
        target: usize,
    ) -> Option<Vec<DirectExprColumn>> {
        if self.scan_desc.is_null() {
            return None;
        }
        let source_cols = self.gpuexpr_source_cols()?;
        if source_cols.is_empty() {
            return None;
        }

        // SAFETY: direct scan mode supplies the live relation slot established
        // by the caller; reading its descriptor does not outlive the slot.
        let tupdesc = unsafe { (*scan_slot).tts_tupleDescriptor };
        if tupdesc.is_null() {
            return None;
        }

        let mut columns = Vec::with_capacity(source_cols.len());
        for col_idx in source_cols {
            let attno = (col_idx + 1) as i32;
            // SAFETY: `tupdesc` is the live scan-slot descriptor and `attno`
            // comes from the planned source-column set for that descriptor.
            let info = unsafe { AttExtractInfo::new(tupdesc, attno) };
            if !matches!(
                info.typid,
                oid if oid == pg_sys::FLOAT4OID
                    || oid == pg_sys::FLOAT8OID
                    || oid == pg_sys::INT2OID
                    || oid == pg_sys::INT4OID
                    || oid == pg_sys::INT8OID
            ) {
                return None;
            }
            columns.push(DirectExprColumn::new(info, target));
        }
        Some(columns)
    }

    /// Child plan scan path for fill_batch.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` must be valid.
    unsafe fn fill_batch_child(&mut self, child_ps: *mut pg_sys::PlanState, target: usize) {
        let extract_attno = self.target_attno;

        while self.tuple_buffer.len() < target {
            // SAFETY: ExecProcNode is the standard PG API for pulling a
            // tuple from a plan node. We are on the main backend thread.
            let child_slot = unsafe { pg_sys::ExecProcNode(child_ps) };

            if child_slot.is_null() {
                self.child_exhausted = true;
                break;
            }

            // SAFETY: child_slot is non-null. TTS_EMPTY checks whether the
            // slot has a valid tuple. In PG, an empty slot signals end of
            // scan.
            let is_empty = unsafe { (*child_slot).tts_flags & pg_sys::TTS_FLAG_EMPTY as u16 != 0 };
            if is_empty {
                self.child_exhausted = true;
                break;
            }

            // Extract target column datum from the child's slot NOW, before
            // the child reuses the slot on the next ExecProcNode call.
            if extract_attno > 0 {
                // Check that the child slot has enough attributes.
                // SAFETY: ExecProcNode returned this non-null, non-empty live
                // slot and it remains valid until the next child pull.
                let child_desc = unsafe { (*child_slot).tts_tupleDescriptor };
                let child_natts = if child_desc.is_null() {
                    0
                } else {
                    // SAFETY: the branch proves the live child descriptor is
                    // non-null before reading its attribute count.
                    unsafe { (*child_desc).natts }
                };
                if extract_attno <= child_natts {
                    let mut is_null = false;
                    // SAFETY: child_slot is valid and non-empty; slot_getattr
                    // deforms the tuple to extract the requested attribute.
                    let datum = unsafe {
                        pg_sys::slot_getattr(
                            child_slot,
                            extract_attno,
                            std::ptr::addr_of_mut!(is_null),
                        )
                    };
                    if !is_null && datum.value() != 0 {
                        // Deep copy varlena datum — the child reuses its slot.
                        // SAFETY: pg_detoast_datum_copy returns a palloc'd copy.
                        let varlena_ptr = datum.cast_mut_ptr::<pg_sys::varlena>();
                        // SAFETY: this non-null attribute is a varlena target;
                        // PostgreSQL returns a new palloc-owned detoasted copy.
                        let copied = unsafe { pg_sys::pg_detoast_datum_copy(varlena_ptr) };
                        self.datum_buffer.push((pg_sys::Datum::from(copied), false));
                    } else {
                        self.datum_buffer.push((pg_sys::Datum::from(0), true));
                    }
                } else {
                    if self.tuple_buffer.is_empty() {
                        tracing::debug!(
                            "pg_accel: fill_batch: attno={} > child_natts={}, skipping",
                            extract_attno,
                            child_natts,
                        );
                    }
                    self.datum_buffer.push((pg_sys::Datum::from(0), true));
                }
            }

            // Copy the tuple into our own storage for returning to parent.
            // SAFETY: child_slot is valid and non-empty.
            let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(child_slot) };

            self.tuple_buffer.push(mt);
        }
    }
}
