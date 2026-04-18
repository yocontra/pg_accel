//! Arena-based vectorized heap scan for pg_accel Custom Scan nodes.
//!
//! [`VectorizedScan`] provides the universal data pipeline shared by all
//! GPU-accelerated strategies (reduce, hash-agg, sort, window, filter).
//! It replaces PG's per-tuple `ExecProcNode` chain with a direct heap
//! walk that copies raw tuple bytes into a flat arena, then extracts
//! columns into typed contiguous arrays for GPU kernel dispatch.
//!
//! # Pipeline
//!
//! 1. `heap_getnext` → copy raw `HeapTupleHeader` bytes into arena
//! 2. Extract columns via `try_fast_read_heap_pub<T>` on arena headers
//! 3. GPU kernel dispatch on columnar arrays
//! 4. Materialize result tuples from arena as needed
//!
//! # Safety
//!
//! All public methods must be called on the main backend thread.
//! The `scan_desc` must remain valid for the lifetime of the struct.

use pgrx::pg_sys;

use crate::engine::materialize::tuple_extract::{self, AttExtractInfo};

/// Arena-based vectorized heap scan.
///
/// Scans a base table directly via `heap_getnext`, stores raw tuple
/// bytes in a flat arena, and provides columnar extraction methods.
pub struct VectorizedScan {
    /// Open table scan descriptor. Owned by the Custom Scan node's
    /// relation, NOT by this struct — do not close it here.
    scan_desc: pg_sys::TableScanDesc,

    /// Flat byte buffer holding raw `HeapTupleHeader` data for all
    /// scanned tuples. Each tuple's data is contiguous at its entry's
    /// offset.
    arena: Vec<u8>,

    /// Per-tuple metadata: `(offset_in_arena, t_len)`.
    entries: Vec<(usize, u32)>,

    /// Number of tuples currently in the arena.
    row_count: usize,

    /// True when the heap scan has reached the end of the table.
    exhausted: bool,
}

impl VectorizedScan {
    /// Create a new vectorized scan over an already-opened table scan.
    ///
    /// # Safety
    ///
    /// `scan_desc` must be a valid, open `TableScanDesc` from
    /// `table_beginscan`. Must be called on the main backend thread.
    #[must_use]
    pub unsafe fn new(scan_desc: pg_sys::TableScanDesc) -> Self {
        Self {
            scan_desc,
            arena: Vec::new(),
            entries: Vec::new(),
            row_count: 0,
            exhausted: false,
        }
    }

    /// Whether the underlying heap scan is exhausted.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Number of tuples currently held in the arena.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.row_count
    }

    /// The underlying scan descriptor.
    #[must_use]
    pub fn scan_desc(&self) -> pg_sys::TableScanDesc {
        self.scan_desc
    }

    /// Clear the arena for reuse (e.g., between batches).
    pub fn reset(&mut self) {
        self.arena.clear();
        self.entries.clear();
        self.row_count = 0;
    }

    // ------------------------------------------------------------------
    // Scanning
    // ------------------------------------------------------------------

    /// Scan up to `batch_size` tuples into the arena.
    ///
    /// Returns the number of tuples scanned (may be less if the table
    /// is exhausted). Appends to any existing arena contents.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `scan_desc` must be
    /// valid.
    pub unsafe fn scan_batch(&mut self, batch_size: usize) -> usize {
        if self.exhausted {
            return 0;
        }

        self.arena.reserve(batch_size * 64);
        self.entries.reserve(batch_size);

        let mut count = 0usize;
        while count < batch_size {
            // SAFETY: scan_desc is valid; main backend thread.
            let htup = unsafe {
                pg_sys::heap_getnext(self.scan_desc, pg_sys::ScanDirection::ForwardScanDirection)
            };
            if htup.is_null() {
                self.exhausted = true;
                break;
            }

            // SAFETY: htup is valid from heap_getnext.
            let ht = unsafe { &*htup };
            let t_len = ht.t_len;

            // Copy the raw HeapTupleHeader bytes into the arena.
            let offset = self.arena.len();
            // SAFETY: t_data points to t_len bytes of valid tuple data.
            let data_bytes =
                unsafe { std::slice::from_raw_parts(ht.t_data.cast::<u8>(), t_len as usize) };
            self.arena.extend_from_slice(data_bytes);
            self.entries.push((offset, t_len));

            count += 1;

            // CHECK_FOR_INTERRUPTS every 8192 rows.
            if count.is_multiple_of(8192) {
                pgrx::check_for_interrupts!();
            }
        }

        self.row_count += count;
        count
    }

    /// Scan ALL remaining tuples into the arena.
    ///
    /// Used by blocking operators (agg, sort, window) that need the
    /// complete dataset before computing.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    pub unsafe fn scan_all(&mut self) -> usize {
        let mut total = 0usize;
        loop {
            let n = unsafe { self.scan_batch(8192) };
            total += n;
            if self.exhausted {
                break;
            }
        }
        total
    }

    // ------------------------------------------------------------------
    // Header access
    // ------------------------------------------------------------------

    /// Get the `HeapTupleHeader` pointer for the `idx`-th scanned tuple.
    ///
    /// # Safety
    ///
    /// `idx` must be < `row_count`. The returned pointer is valid as
    /// long as the arena is not modified (no further scans or resets).
    #[inline]
    unsafe fn header(&self, idx: usize) -> pg_sys::HeapTupleHeader {
        let (offset, _) = self.entries[idx];
        self.arena[offset..].as_ptr() as pg_sys::HeapTupleHeader
    }

    /// Collect all header pointers. Must be called after scanning is
    /// complete (arena will not reallocate).
    fn collect_headers(&self) -> Vec<pg_sys::HeapTupleHeader> {
        self.entries
            .iter()
            .map(|&(offset, _)| self.arena[offset..].as_ptr() as pg_sys::HeapTupleHeader)
            .collect()
    }

    // ------------------------------------------------------------------
    // Columnar extraction from arena HeapTupleHeaders
    // ------------------------------------------------------------------

    /// Extract a column as `f64` from all scanned tuples.
    ///
    /// Handles type conversion for FLOAT4, INT2, INT4, INT8, FLOAT8.
    /// Returns `(values, null_mask)` where `null_mask[i] = 1` means null.
    ///
    /// # Safety
    ///
    /// `info` must match the schema of the scanned relation.
    #[must_use]
    pub unsafe fn extract_f64(&self, info: &AttExtractInfo) -> (Vec<f64>, Vec<u8>) {
        let headers = self.collect_headers();
        // SAFETY: headers point into valid arena data; info matches schema.
        unsafe { tuple_extract::extract_f64_from_heap_headers(&headers, info) }
    }

    /// Extract a column as `f32` from all scanned tuples.
    ///
    /// # Safety
    ///
    /// `info` must match the schema. Column must be FLOAT4.
    #[must_use]
    pub unsafe fn extract_f32(&self, info: &AttExtractInfo) -> (Vec<f32>, Vec<u8>) {
        let n = self.row_count;
        let mut values = Vec::with_capacity(n);
        let mut nulls = Vec::with_capacity(n);

        for i in 0..n {
            // SAFETY: i < row_count, arena is finalized.
            let hdr = unsafe { self.header(i) };
            let val: Option<f32> =
                unsafe { tuple_extract::try_fast_read_heap_pub::<f32>(hdr, info) };
            if let Some(v) = val {
                values.push(v);
                nulls.push(0);
            } else {
                values.push(0.0);
                nulls.push(1);
            }
        }

        (values, nulls)
    }

    /// Extract a column as `i32` from all scanned tuples.
    ///
    /// # Safety
    ///
    /// `info` must match the schema. Column must be INT4.
    #[must_use]
    pub unsafe fn extract_i32(&self, info: &AttExtractInfo) -> (Vec<i32>, Vec<u8>) {
        let n = self.row_count;
        let mut values = Vec::with_capacity(n);
        let mut nulls = Vec::with_capacity(n);

        for i in 0..n {
            // SAFETY: i < row_count, arena is finalized.
            let hdr = unsafe { self.header(i) };
            let val: Option<i32> =
                unsafe { tuple_extract::try_fast_read_heap_pub::<i32>(hdr, info) };
            if let Some(v) = val {
                values.push(v);
                nulls.push(0);
            } else {
                values.push(0);
                nulls.push(1);
            }
        }

        (values, nulls)
    }

    /// Extract a column as `i64` from all scanned tuples.
    ///
    /// # Safety
    ///
    /// `info` must match the schema. Column must be INT8.
    #[must_use]
    pub unsafe fn extract_i64(&self, info: &AttExtractInfo) -> (Vec<i64>, Vec<u8>) {
        let n = self.row_count;
        let mut values = Vec::with_capacity(n);
        let mut nulls = Vec::with_capacity(n);

        for i in 0..n {
            // SAFETY: i < row_count, arena is finalized.
            let hdr = unsafe { self.header(i) };
            let val: Option<i64> =
                unsafe { tuple_extract::try_fast_read_heap_pub::<i64>(hdr, info) };
            if let Some(v) = val {
                values.push(v);
                nulls.push(0);
            } else {
                values.push(0);
                nulls.push(1);
            }
        }

        (values, nulls)
    }

    /// Extract raw `u8` bytes for a column (for hash agg key extraction).
    ///
    /// Returns the byte buffer and null mask. Each value is `typlen` bytes.
    ///
    /// # Safety
    ///
    /// `info` must match the schema. Column must be fixed-width.
    #[must_use]
    pub unsafe fn extract_bytes(&self, info: &AttExtractInfo) -> (Vec<u8>, Vec<u8>) {
        let n = self.row_count;
        let typlen = info.typlen as usize;
        let mut bytes = Vec::with_capacity(n * typlen);
        let mut nulls = Vec::with_capacity(n);

        for i in 0..n {
            // SAFETY: i < row_count, arena is finalized.
            let hdr = unsafe { self.header(i) };
            if !info.can_fast_extract() {
                // Can't fast-extract — zero-fill and mark null.
                bytes.extend(std::iter::repeat_n(0u8, typlen));
                nulls.push(1);
                continue;
            }

            let hdr_ref = unsafe { &*hdr };
            let has_null = (hdr_ref.t_infomask & pg_sys::HEAP_HASNULL as u16) != 0;

            // Check for null via bitmap.
            // Read raw bytes at the data offset.
            let data_start = unsafe { (hdr as *const u8).add(hdr_ref.t_hoff as usize) };
            let val_ptr = data_start.wrapping_add(info.data_offset());
            let _ = has_null;

            // Check null using the typed read (returns None if null).
            let is_null = match typlen {
                4 => unsafe { tuple_extract::try_fast_read_heap_pub::<i32>(hdr, info).is_none() },
                8 => unsafe { tuple_extract::try_fast_read_heap_pub::<i64>(hdr, info).is_none() },
                2 => unsafe { tuple_extract::try_fast_read_heap_pub::<i16>(hdr, info).is_none() },
                _ => true,
            };

            if is_null {
                bytes.extend(std::iter::repeat_n(0u8, typlen));
                nulls.push(1);
            } else {
                // SAFETY: val_ptr points to typlen bytes of valid data.
                let slice = unsafe { std::slice::from_raw_parts(val_ptr, typlen) };
                bytes.extend_from_slice(slice);
                nulls.push(0);
            }
        }

        (bytes, nulls)
    }

    // ------------------------------------------------------------------
    // Materialization
    // ------------------------------------------------------------------

    /// Materialize the `idx`-th tuple from the arena as a `MinimalTuple`.
    ///
    /// The returned `MinimalTuple` is palloc'd in the current memory
    /// context.
    ///
    /// # Safety
    ///
    /// `idx` must be < `row_count`. Must be called in an appropriate
    /// memory context.
    #[must_use]
    pub unsafe fn materialize(&self, idx: usize) -> pg_sys::MinimalTuple {
        let (offset, t_len) = self.entries[idx];
        let t_data = self.arena[offset..].as_ptr() as pg_sys::HeapTupleHeader;

        // Build a stack HeapTupleData pointing into the arena.
        let mut ht_data = pg_sys::HeapTupleData {
            t_len,
            t_self: pg_sys::ItemPointerData::default(),
            t_tableOid: pg_sys::InvalidOid,
            t_data,
        };

        // SAFETY: ht_data points to valid HeapTupleHeader data in the arena.
        unsafe { pg_sys::minimal_tuple_from_heap_tuple(&raw mut ht_data) }
    }

    /// Get the `TupleDesc` for the scanned relation.
    ///
    /// # Safety
    ///
    /// `scan_desc` must be valid.
    #[must_use]
    pub unsafe fn tupdesc(&self) -> pg_sys::TupleDesc {
        // SAFETY: scan_desc is valid per struct invariant.
        let rel = unsafe { (*self.scan_desc).rs_rd };
        unsafe { (*rel).rd_att }
    }
}

impl Drop for VectorizedScan {
    fn drop(&mut self) {
        // We do NOT close scan_desc here — the Custom Scan node owns it.
        // Just release arena memory (Vec drop handles this).
    }
}
