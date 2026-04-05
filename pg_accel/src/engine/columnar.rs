//! Columnar batch builder for GPU expression evaluation.
//!
//! Converts `Vec<MinimalTuple>` into a columnar `PgaccelBatch` suitable for
//! the GPU expression evaluator. Only columns referenced by the expression
//! program are transposed — unreferenced columns are skipped entirely.

use crate::gpu::{PgaccelBatch, PgaccelValTag};

/// Owns the columnar data arrays backing a [`PgaccelBatch`].
///
/// The `PgaccelBatch` struct contains raw pointers into these arrays.
/// This struct must outlive any `PgaccelBatch` created from it.
pub struct ColumnarBatchOwner {
    pub num_rows: usize,
    pub num_cols: usize,
    /// Per-column value arrays (type-erased). Each `Vec<u8>` is a byte buffer
    /// holding `num_rows` elements of the column's native type.
    pub col_data_bufs: Vec<Vec<u8>>,
    /// Per-column null bitmaps. `col_nulls[c][r] == 1` means row `r` of
    /// column `c` is NULL.
    pub col_null_bufs: Vec<Vec<u8>>,
    /// Per-column type tags.
    pub col_types: Vec<PgaccelValTag>,
    // Pointer arrays that PgaccelBatch.col_data / col_nulls point into.
    col_data_ptrs: Vec<*const std::ffi::c_void>,
    col_null_ptrs: Vec<*const u8>,
}

impl ColumnarBatchOwner {
    /// Create an empty batch with pre-allocated column slots.
    #[must_use]
    pub fn new(num_rows: usize, num_cols: usize) -> Self {
        Self {
            num_rows,
            num_cols,
            col_data_bufs: Vec::with_capacity(num_cols),
            col_null_bufs: Vec::with_capacity(num_cols),
            col_types: Vec::with_capacity(num_cols),
            col_data_ptrs: Vec::with_capacity(num_cols),
            col_null_ptrs: Vec::with_capacity(num_cols),
        }
    }

    /// Add a column of `i32` values.
    pub fn add_col_i32(&mut self, values: Vec<i32>, nulls: Vec<u8>) {
        let byte_buf = unsafe {
            let ptr = values.as_ptr().cast::<u8>();
            let len = values.len() * std::mem::size_of::<i32>();
            std::slice::from_raw_parts(ptr, len).to_vec()
        };
        std::mem::forget(values);
        self.col_data_bufs.push(byte_buf);
        self.col_null_bufs.push(nulls);
        self.col_types.push(PgaccelValTag::Int32);
    }

    /// Add a column of `i64` values.
    pub fn add_col_i64(&mut self, values: Vec<i64>, nulls: Vec<u8>) {
        let byte_buf = unsafe {
            let ptr = values.as_ptr().cast::<u8>();
            let len = values.len() * std::mem::size_of::<i64>();
            std::slice::from_raw_parts(ptr, len).to_vec()
        };
        std::mem::forget(values);
        self.col_data_bufs.push(byte_buf);
        self.col_null_bufs.push(nulls);
        self.col_types.push(PgaccelValTag::Int64);
    }

    /// Add a column of `f32` values.
    pub fn add_col_f32(&mut self, values: Vec<f32>, nulls: Vec<u8>) {
        let byte_buf = unsafe {
            let ptr = values.as_ptr().cast::<u8>();
            let len = values.len() * std::mem::size_of::<f32>();
            std::slice::from_raw_parts(ptr, len).to_vec()
        };
        std::mem::forget(values);
        self.col_data_bufs.push(byte_buf);
        self.col_null_bufs.push(nulls);
        self.col_types.push(PgaccelValTag::Float32);
    }

    /// Add a column of `f64` values.
    pub fn add_col_f64(&mut self, values: Vec<f64>, nulls: Vec<u8>) {
        let byte_buf = unsafe {
            let ptr = values.as_ptr().cast::<u8>();
            let len = values.len() * std::mem::size_of::<f64>();
            std::slice::from_raw_parts(ptr, len).to_vec()
        };
        std::mem::forget(values);
        self.col_data_bufs.push(byte_buf);
        self.col_null_bufs.push(nulls);
        self.col_types.push(PgaccelValTag::Float64);
    }

    /// Add a column of `bool` values.
    pub fn add_col_bool(&mut self, values: Vec<bool>, nulls: Vec<u8>) {
        let byte_buf = unsafe {
            let ptr = values.as_ptr().cast::<u8>();
            std::slice::from_raw_parts(ptr, values.len()).to_vec()
        };
        std::mem::forget(values);
        self.col_data_bufs.push(byte_buf);
        self.col_null_bufs.push(nulls);
        self.col_types.push(PgaccelValTag::Bool);
    }

    /// Finalize pointer arrays and return a `PgaccelBatch` that borrows
    /// from this owner.
    ///
    /// The returned batch is valid as long as `self` is not dropped or modified.
    pub fn as_batch(&mut self) -> PgaccelBatch {
        self.col_data_ptrs.clear();
        self.col_null_ptrs.clear();

        for buf in &self.col_data_bufs {
            self.col_data_ptrs
                .push(buf.as_ptr().cast::<std::ffi::c_void>());
        }
        for buf in &self.col_null_bufs {
            self.col_null_ptrs.push(buf.as_ptr());
        }

        PgaccelBatch {
            num_rows: self.num_rows,
            num_cols: self.col_data_bufs.len(),
            col_data: self.col_data_ptrs.as_ptr().cast(),
            col_nulls: self.col_null_ptrs.as_ptr().cast(),
            col_types: self.col_types.as_ptr(),
        }
    }
}

/// Map a PostgreSQL type OID to a `PgaccelValTag`.
///
/// Returns `None` for types not supported by the GPU expression evaluator.
#[must_use]
pub fn pg_type_to_val_tag(type_oid: pgrx::pg_sys::Oid) -> Option<PgaccelValTag> {
    use pgrx::pg_sys;
    match type_oid {
        pg_sys::BOOLOID => Some(PgaccelValTag::Bool),
        pg_sys::INT2OID | pg_sys::INT4OID => Some(PgaccelValTag::Int32),
        pg_sys::INT8OID => Some(PgaccelValTag::Int64),
        pg_sys::FLOAT4OID => Some(PgaccelValTag::Float32),
        pg_sys::FLOAT8OID => Some(PgaccelValTag::Float64),
        pg_sys::DATEOID => Some(PgaccelValTag::Date),
        pg_sys::TIMESTAMPOID | pg_sys::TIMESTAMPTZOID => Some(PgaccelValTag::Timestamp),
        _ => None,
    }
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::gpu::PgaccelValTag;

    // -----------------------------------------------------------------------
    // ColumnarBatchOwner::new
    // -----------------------------------------------------------------------

    #[test]
    fn new_zero_rows_zero_cols() {
        let owner = ColumnarBatchOwner::new(0, 0);
        assert_eq!(owner.num_rows, 0);
        assert_eq!(owner.num_cols, 0);
        assert!(owner.col_data_bufs.is_empty());
        assert!(owner.col_null_bufs.is_empty());
        assert!(owner.col_types.is_empty());
    }

    #[test]
    fn new_preallocates_capacity() {
        let owner = ColumnarBatchOwner::new(1024, 5);
        assert_eq!(owner.num_rows, 1024);
        assert_eq!(owner.num_cols, 5);
        assert!(owner.col_data_bufs.capacity() >= 5);
        assert!(owner.col_null_bufs.capacity() >= 5);
        assert!(owner.col_types.capacity() >= 5);
    }

    // -----------------------------------------------------------------------
    // add_col_* — single column
    // -----------------------------------------------------------------------

    #[test]
    fn add_col_i32_stores_bytes_and_tag() {
        let mut owner = ColumnarBatchOwner::new(3, 1);
        let values = vec![10_i32, 20, 30];
        let nulls = vec![0_u8, 0, 0];
        owner.add_col_i32(values, nulls);

        assert_eq!(owner.col_types.len(), 1);
        assert_eq!(owner.col_types[0], PgaccelValTag::Int32);
        assert_eq!(owner.col_data_bufs[0].len(), 3 * std::mem::size_of::<i32>());
    }

    #[test]
    fn add_col_i64_stores_bytes_and_tag() {
        let mut owner = ColumnarBatchOwner::new(2, 1);
        owner.add_col_i64(vec![100_i64, 200], vec![0, 0]);

        assert_eq!(owner.col_types[0], PgaccelValTag::Int64);
        assert_eq!(owner.col_data_bufs[0].len(), 2 * std::mem::size_of::<i64>());
    }

    #[test]
    fn add_col_f32_stores_bytes_and_tag() {
        let mut owner = ColumnarBatchOwner::new(2, 1);
        owner.add_col_f32(vec![1.5_f32, 2.5], vec![0, 0]);

        assert_eq!(owner.col_types[0], PgaccelValTag::Float32);
        assert_eq!(owner.col_data_bufs[0].len(), 2 * std::mem::size_of::<f32>());
    }

    #[test]
    fn add_col_f64_stores_bytes_and_tag() {
        let mut owner = ColumnarBatchOwner::new(2, 1);
        owner.add_col_f64(vec![3.14_f64, 2.71], vec![0, 0]);

        assert_eq!(owner.col_types[0], PgaccelValTag::Float64);
        assert_eq!(owner.col_data_bufs[0].len(), 2 * std::mem::size_of::<f64>());
    }

    #[test]
    fn add_col_bool_stores_bytes_and_tag() {
        let mut owner = ColumnarBatchOwner::new(3, 1);
        owner.add_col_bool(vec![true, false, true], vec![0, 0, 0]);

        assert_eq!(owner.col_types[0], PgaccelValTag::Bool);
        assert_eq!(owner.col_data_bufs[0].len(), 3);
    }

    // -----------------------------------------------------------------------
    // add_col_* — multiple columns
    // -----------------------------------------------------------------------

    #[test]
    fn multiple_columns_mixed_types() {
        let mut owner = ColumnarBatchOwner::new(2, 3);
        owner.add_col_i32(vec![1, 2], vec![0, 0]);
        owner.add_col_f64(vec![1.0, 2.0], vec![0, 0]);
        owner.add_col_bool(vec![true, false], vec![0, 0]);

        assert_eq!(owner.col_types.len(), 3);
        assert_eq!(owner.col_types[0], PgaccelValTag::Int32);
        assert_eq!(owner.col_types[1], PgaccelValTag::Float64);
        assert_eq!(owner.col_types[2], PgaccelValTag::Bool);
        assert_eq!(owner.col_data_bufs.len(), 3);
        assert_eq!(owner.col_null_bufs.len(), 3);
    }

    // -----------------------------------------------------------------------
    // add_col_* — empty (0 rows)
    // -----------------------------------------------------------------------

    #[test]
    fn add_col_i32_zero_rows() {
        let mut owner = ColumnarBatchOwner::new(0, 1);
        owner.add_col_i32(vec![], vec![]);

        assert_eq!(owner.col_data_bufs[0].len(), 0);
        assert_eq!(owner.col_null_bufs[0].len(), 0);
        assert_eq!(owner.col_types[0], PgaccelValTag::Int32);
    }

    // -----------------------------------------------------------------------
    // add_col_* — large batch (8192 rows)
    // -----------------------------------------------------------------------

    #[test]
    fn add_col_i32_large_batch() {
        let n = 8192;
        let mut owner = ColumnarBatchOwner::new(n, 1);
        let values: Vec<i32> = (0..n as i32).collect();
        let nulls = vec![0_u8; n];
        owner.add_col_i32(values, nulls);

        assert_eq!(owner.col_data_bufs[0].len(), n * std::mem::size_of::<i32>());
        assert_eq!(owner.col_null_bufs[0].len(), n);
    }

    // -----------------------------------------------------------------------
    // Null buffer correctness
    // -----------------------------------------------------------------------

    #[test]
    fn null_buffer_all_null() {
        let mut owner = ColumnarBatchOwner::new(4, 1);
        let nulls = vec![1_u8; 4];
        owner.add_col_i32(vec![0, 0, 0, 0], nulls);

        assert!(owner.col_null_bufs[0].iter().all(|&b| b == 1));
    }

    #[test]
    fn null_buffer_no_null() {
        let mut owner = ColumnarBatchOwner::new(4, 1);
        let nulls = vec![0_u8; 4];
        owner.add_col_i32(vec![1, 2, 3, 4], nulls);

        assert!(owner.col_null_bufs[0].iter().all(|&b| b == 0));
    }

    #[test]
    fn null_buffer_mixed_pattern() {
        let mut owner = ColumnarBatchOwner::new(6, 1);
        let nulls = vec![0_u8, 1, 0, 1, 1, 0];
        owner.add_col_i32(vec![10, 0, 20, 0, 0, 30], nulls);

        assert_eq!(owner.col_null_bufs[0], vec![0, 1, 0, 1, 1, 0]);
    }

    // -----------------------------------------------------------------------
    // as_batch — pointer stability and structure
    // -----------------------------------------------------------------------

    #[test]
    fn as_batch_returns_correct_dimensions() {
        let mut owner = ColumnarBatchOwner::new(5, 2);
        owner.add_col_i32(vec![1, 2, 3, 4, 5], vec![0; 5]);
        owner.add_col_f64(vec![1.0, 2.0, 3.0, 4.0, 5.0], vec![0; 5]);

        let batch = owner.as_batch();
        assert_eq!(batch.num_rows, 5);
        assert_eq!(batch.num_cols, 2);
    }

    #[test]
    fn as_batch_pointers_are_non_null() {
        let mut owner = ColumnarBatchOwner::new(3, 1);
        owner.add_col_i32(vec![1, 2, 3], vec![0; 3]);

        let batch = owner.as_batch();
        assert!(!batch.col_data.is_null());
        assert!(!batch.col_nulls.is_null());
        assert!(!batch.col_types.is_null());
    }

    #[test]
    fn as_batch_pointer_stability_across_calls() {
        let mut owner = ColumnarBatchOwner::new(2, 1);
        owner.add_col_i32(vec![10, 20], vec![0, 0]);

        let batch1 = owner.as_batch();
        let ptr1_data = batch1.col_data;

        let batch2 = owner.as_batch();
        let ptr2_data = batch2.col_data;

        // The pointer arrays are rebuilt each call, but the underlying data
        // buffers should remain at the same address since owner is not moved.
        assert_eq!(ptr1_data, ptr2_data);
    }

    #[test]
    fn as_batch_col_types_readable() {
        let mut owner = ColumnarBatchOwner::new(1, 2);
        owner.add_col_i64(vec![42], vec![0]);
        owner.add_col_bool(vec![true], vec![0]);

        let batch = owner.as_batch();
        // SAFETY: col_types points into owner.col_types which is still alive.
        let types = unsafe { std::slice::from_raw_parts(batch.col_types, 2) };
        assert_eq!(types[0], PgaccelValTag::Int64);
        assert_eq!(types[1], PgaccelValTag::Bool);
    }

    // -----------------------------------------------------------------------
    // pg_type_to_val_tag — every known mapping
    // -----------------------------------------------------------------------

    #[test]
    fn pg_type_bool_maps_to_bool() {
        assert_eq!(
            pg_type_to_val_tag(pgrx::pg_sys::BOOLOID),
            Some(PgaccelValTag::Bool)
        );
    }

    #[test]
    fn pg_type_int2_and_int4_map_to_int32() {
        assert_eq!(
            pg_type_to_val_tag(pgrx::pg_sys::INT2OID),
            Some(PgaccelValTag::Int32)
        );
        assert_eq!(
            pg_type_to_val_tag(pgrx::pg_sys::INT4OID),
            Some(PgaccelValTag::Int32)
        );
    }

    #[test]
    fn pg_type_int8_maps_to_int64() {
        assert_eq!(
            pg_type_to_val_tag(pgrx::pg_sys::INT8OID),
            Some(PgaccelValTag::Int64)
        );
    }

    #[test]
    fn pg_type_float4_maps_to_float32() {
        assert_eq!(
            pg_type_to_val_tag(pgrx::pg_sys::FLOAT4OID),
            Some(PgaccelValTag::Float32)
        );
    }

    #[test]
    fn pg_type_float8_maps_to_float64() {
        assert_eq!(
            pg_type_to_val_tag(pgrx::pg_sys::FLOAT8OID),
            Some(PgaccelValTag::Float64)
        );
    }

    #[test]
    fn pg_type_date_maps_to_date() {
        assert_eq!(
            pg_type_to_val_tag(pgrx::pg_sys::DATEOID),
            Some(PgaccelValTag::Date)
        );
    }

    #[test]
    fn pg_type_timestamp_maps_to_timestamp() {
        assert_eq!(
            pg_type_to_val_tag(pgrx::pg_sys::TIMESTAMPOID),
            Some(PgaccelValTag::Timestamp)
        );
        assert_eq!(
            pg_type_to_val_tag(pgrx::pg_sys::TIMESTAMPTZOID),
            Some(PgaccelValTag::Timestamp)
        );
    }

    #[test]
    fn pg_type_unknown_oid_returns_none() {
        assert_eq!(pg_type_to_val_tag(pgrx::pg_sys::Oid::from(99999_u32)), None);
    }

    // -----------------------------------------------------------------------
    // Type size assertions
    // -----------------------------------------------------------------------

    #[test]
    fn type_sizes_match_expectations() {
        assert_eq!(std::mem::size_of::<i32>(), 4);
        assert_eq!(std::mem::size_of::<i64>(), 8);
        assert_eq!(std::mem::size_of::<f32>(), 4);
        assert_eq!(std::mem::size_of::<f64>(), 8);
        assert_eq!(std::mem::size_of::<bool>(), 1);
    }

    // -----------------------------------------------------------------------
    // ValTag enum coverage
    // -----------------------------------------------------------------------

    #[test]
    fn val_tag_all_variants_distinct() {
        let variants = [
            PgaccelValTag::Null,
            PgaccelValTag::Bool,
            PgaccelValTag::Int32,
            PgaccelValTag::Int64,
            PgaccelValTag::Float32,
            PgaccelValTag::Float64,
            PgaccelValTag::Date,
            PgaccelValTag::Timestamp,
        ];
        // All variants should be distinct via PartialEq.
        for (i, a) in variants.iter().enumerate() {
            for (j, b) in variants.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn val_tag_debug_is_not_empty() {
        let tag = PgaccelValTag::Int32;
        let debug = format!("{tag:?}");
        assert!(!debug.is_empty());
        assert!(debug.contains("Int32"));
    }

    #[test]
    fn val_tag_clone_and_copy() {
        let tag = PgaccelValTag::Float64;
        let cloned = tag.clone();
        let copied = tag;
        assert_eq!(tag, cloned);
        assert_eq!(tag, copied);
    }
}
