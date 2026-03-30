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
