//! Columnar batch builder for GPU expression evaluation and future
//! GPU-resident operator handoff.
//!
//! Converts `Vec<MinimalTuple>` into a columnar `PgaccelBatch` suitable for
//! the GPU expression evaluator. Only columns referenced by the expression
//! program are transposed — unreferenced columns are skipped entirely.

use std::ffi::c_void;

pub use crate::engine::residency::{
    BatchResidency, CpuBoundaryReason, DeviceBufferRef, MaterializationBoundary,
    ResidentOperatorClass, ResidentOperatorStage, ResidentPipelineProof,
};
use crate::gpu::{PgaccelBatch, PgaccelValTag};

/// Row-selection metadata for a batch.
///
/// Selection masks use the opposite polarity from SQL null masks: `1` means
/// the row is selected and may be consumed by a downstream operator; `0` means
/// filtered out. A null pointer at FFI boundaries means all rows are selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchSelection {
    /// Every row in the batch is selected.
    AllRows,
    /// Byte-per-row mask where `1` means selected and `0` means filtered.
    Mask(Vec<u8>),
}

impl BatchSelection {
    #[must_use]
    pub fn from_mask(mask: Vec<u8>) -> Self {
        assert!(
            mask.iter().all(|&b| b <= 1),
            "batch selection-mask values must be 0 or 1"
        );
        if mask.iter().all(|&b| b == 1) {
            Self::AllRows
        } else {
            Self::Mask(mask)
        }
    }

    #[must_use]
    pub fn from_mask_for_rows(mask: Vec<u8>, num_rows: usize) -> Self {
        assert_eq!(
            mask.len(),
            num_rows,
            "batch selection-mask count must match batch row count"
        );
        Self::from_mask(mask)
    }

    #[must_use]
    pub fn len(&self, num_rows: usize) -> usize {
        match self {
            Self::AllRows => num_rows,
            Self::Mask(mask) => mask.len(),
        }
    }

    #[must_use]
    pub const fn is_all_rows(&self) -> bool {
        matches!(self, Self::AllRows)
    }

    #[must_use]
    pub fn selected_rows(&self, num_rows: usize) -> usize {
        match self {
            Self::AllRows => num_rows,
            Self::Mask(mask) => mask.iter().filter(|&&b| b != 0).count(),
        }
    }

    #[must_use]
    pub fn as_slice(&self) -> Option<&[u8]> {
        match self {
            Self::AllRows => None,
            Self::Mask(mask) => Some(mask),
        }
    }

    #[must_use]
    pub fn as_ptr(&self) -> *const u8 {
        match self {
            Self::AllRows => std::ptr::null(),
            Self::Mask(mask) => mask.as_ptr(),
        }
    }
}

/// Type-preserving column storage.
///
/// Keeping typed vectors instead of byte buffers is load-bearing: kernel FFI
/// pointers must remain correctly aligned for the element type, and dropping
/// the owner must release the original typed allocation exactly once.
#[derive(Debug)]
pub enum ColumnarColumnData {
    Bool(Vec<u8>),
    Int32(Vec<i32>),
    Int64(Vec<i64>),
    Float32(Vec<f32>),
    Float64(Vec<f64>),
    Date(Vec<i32>),
    Timestamp(Vec<i64>),
}

impl ColumnarColumnData {
    #[must_use]
    pub const fn tag(&self) -> PgaccelValTag {
        match self {
            Self::Bool(_) => PgaccelValTag::Bool,
            Self::Int32(_) => PgaccelValTag::Int32,
            Self::Int64(_) => PgaccelValTag::Int64,
            Self::Float32(_) => PgaccelValTag::Float32,
            Self::Float64(_) => PgaccelValTag::Float64,
            Self::Date(_) => PgaccelValTag::Date,
            Self::Timestamp(_) => PgaccelValTag::Timestamp,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Bool(values) => values.len(),
            Self::Int32(values) | Self::Date(values) => values.len(),
            Self::Int64(values) | Self::Timestamp(values) => values.len(),
            Self::Float32(values) => values.len(),
            Self::Float64(values) => values.len(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub const fn element_size(&self) -> usize {
        match self {
            Self::Bool(_) => std::mem::size_of::<u8>(),
            Self::Int32(_) | Self::Date(_) => std::mem::size_of::<i32>(),
            Self::Int64(_) | Self::Timestamp(_) => std::mem::size_of::<i64>(),
            Self::Float32(_) => std::mem::size_of::<f32>(),
            Self::Float64(_) => std::mem::size_of::<f64>(),
        }
    }

    #[must_use]
    pub const fn element_align(&self) -> usize {
        match self {
            Self::Bool(_) => std::mem::align_of::<u8>(),
            Self::Int32(_) | Self::Date(_) => std::mem::align_of::<i32>(),
            Self::Int64(_) | Self::Timestamp(_) => std::mem::align_of::<i64>(),
            Self::Float32(_) => std::mem::align_of::<f32>(),
            Self::Float64(_) => std::mem::align_of::<f64>(),
        }
    }

    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.len() * self.element_size()
    }

    #[must_use]
    pub fn as_ptr(&self) -> *const c_void {
        match self {
            Self::Bool(values) => values.as_ptr().cast(),
            Self::Int32(values) | Self::Date(values) => values.as_ptr().cast(),
            Self::Int64(values) | Self::Timestamp(values) => values.as_ptr().cast(),
            Self::Float32(values) => values.as_ptr().cast(),
            Self::Float64(values) => values.as_ptr().cast(),
        }
    }

    #[must_use]
    pub fn as_f32_slice(&self) -> Option<&[f32]> {
        match self {
            Self::Float32(values) => Some(values),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_f64_slice(&self) -> Option<&[f64]> {
        match self {
            Self::Float64(values) => Some(values),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_i64_slice(&self) -> Option<&[i64]> {
        match self {
            Self::Int64(values) | Self::Timestamp(values) => Some(values),
            _ => None,
        }
    }
}

/// Null storage for one column.
#[derive(Debug)]
pub enum ColumnarNulls {
    /// No rows are NULL. FFI emits a null pointer for this column, matching
    /// the C expression contract.
    AllValid,
    /// Byte-per-row mask where `1` means NULL and `0` means non-NULL.
    Mask(Vec<u8>),
}

impl ColumnarNulls {
    #[must_use]
    pub fn from_mask(mask: Vec<u8>) -> Self {
        assert!(
            mask.iter().all(|&b| b <= 1),
            "column null-mask values must be 0 or 1"
        );
        if mask.iter().all(|&b| b == 0) {
            Self::AllValid
        } else {
            Self::Mask(mask)
        }
    }

    #[must_use]
    pub fn from_mask_for_rows(mask: Vec<u8>, num_rows: usize) -> Self {
        assert_eq!(
            mask.len(),
            num_rows,
            "column null-mask count must match batch row count"
        );
        Self::from_mask(mask)
    }

    #[must_use]
    pub fn len(&self, num_rows: usize) -> usize {
        match self {
            Self::AllValid => num_rows,
            Self::Mask(mask) => mask.len(),
        }
    }

    #[must_use]
    pub fn is_empty(&self, num_rows: usize) -> bool {
        self.len(num_rows) == 0
    }

    #[must_use]
    pub const fn is_all_valid(&self) -> bool {
        matches!(self, Self::AllValid)
    }

    #[must_use]
    pub fn as_slice(&self) -> Option<&[u8]> {
        match self {
            Self::AllValid => None,
            Self::Mask(mask) => Some(mask),
        }
    }

    #[must_use]
    pub fn as_ptr(&self) -> *const u8 {
        match self {
            Self::AllValid => std::ptr::null(),
            Self::Mask(mask) => mask.as_ptr(),
        }
    }
}

/// One typed column plus null metadata.
#[derive(Debug)]
pub struct ColumnarColumn {
    data: ColumnarColumnData,
    nulls: ColumnarNulls,
}

impl ColumnarColumn {
    #[must_use]
    pub fn tag(&self) -> PgaccelValTag {
        self.data.tag()
    }

    #[must_use]
    pub fn data(&self) -> &ColumnarColumnData {
        &self.data
    }

    #[must_use]
    pub fn nulls(&self) -> Option<&[u8]> {
        self.nulls.as_slice()
    }

    #[must_use]
    pub fn has_explicit_null_mask(&self) -> bool {
        !self.nulls.is_all_valid()
    }

    #[must_use]
    pub fn null_ptr(&self) -> *const u8 {
        self.nulls.as_ptr()
    }
}

/// Owns the columnar data arrays backing a [`PgaccelBatch`].
///
/// The `PgaccelBatch` struct contains raw pointers into these arrays.
/// This struct must outlive any `PgaccelBatch` created from it.
pub struct ColumnarBatchOwner {
    pub num_rows: usize,
    /// Expected number of columns. `as_batch` asserts that all expected
    /// columns were populated so kernels do not see a partially-built batch.
    pub num_cols: usize,
    pub residency: BatchResidency,
    pub cpu_boundary: CpuBoundaryReason,
    resident_proof: ResidentPipelineProof,
    selection: BatchSelection,
    columns: Vec<ColumnarColumn>,
    /// Per-column type tags.
    pub col_types: Vec<PgaccelValTag>,
    // Pointer arrays that PgaccelBatch.col_data / col_nulls point into.
    col_data_ptrs: Vec<*const c_void>,
    col_null_ptrs: Vec<*const u8>,
}

impl ColumnarBatchOwner {
    /// Create an empty batch with pre-allocated column slots.
    #[must_use]
    pub fn new(num_rows: usize, num_cols: usize) -> Self {
        Self {
            num_rows,
            num_cols,
            residency: BatchResidency::HostColumnar,
            cpu_boundary: CpuBoundaryReason::HostInputStaging,
            resident_proof: ResidentPipelineProof::host_staged(CpuBoundaryReason::HostInputStaging),
            selection: BatchSelection::AllRows,
            columns: Vec::with_capacity(num_cols),
            col_types: Vec::with_capacity(num_cols),
            col_data_ptrs: Vec::with_capacity(num_cols),
            col_null_ptrs: Vec::with_capacity(num_cols),
        }
    }

    #[must_use]
    pub const fn resident_proof(&self) -> &ResidentPipelineProof {
        &self.resident_proof
    }

    #[must_use]
    pub fn gpu_resident_pipeline(&self) -> bool {
        self.residency == BatchResidency::DeviceResident
            && self.cpu_boundary == CpuBoundaryReason::None
            && self.resident_proof.gpu_resident_pipeline()
    }

    #[must_use]
    pub const fn cpu_boundary_label(&self) -> &'static str {
        self.cpu_boundary.label()
    }

    pub fn mark_host_boundary(&mut self, reason: CpuBoundaryReason) {
        assert!(
            reason.blocks_resident_pipeline(),
            "host boundary must block a resident pipeline"
        );
        self.residency = BatchResidency::HostColumnar;
        self.cpu_boundary = reason;
        self.resident_proof = ResidentPipelineProof::host_staged(reason);
    }

    pub fn mark_device_resident(&mut self, proof: ResidentPipelineProof) {
        assert!(
            proof.gpu_resident_pipeline(),
            "device-resident batch requires a complete resident proof"
        );
        self.residency = BatchResidency::DeviceResident;
        self.cpu_boundary = CpuBoundaryReason::None;
        self.resident_proof = proof;
    }

    fn push_column(&mut self, data: ColumnarColumnData, nulls: ColumnarNulls) {
        assert!(
            self.columns.len() < self.num_cols,
            "columnar batch received more columns than declared"
        );
        assert_eq!(
            data.len(),
            self.num_rows,
            "column value count must match batch row count"
        );
        assert_eq!(
            nulls.len(self.num_rows),
            self.num_rows,
            "column null-mask count must match batch row count"
        );

        self.col_types.push(data.tag());
        self.columns.push(ColumnarColumn { data, nulls });
    }

    #[must_use]
    pub fn actual_num_cols(&self) -> usize {
        self.columns.len()
    }

    #[must_use]
    pub fn column(&self, idx: usize) -> Option<&ColumnarColumn> {
        self.columns.get(idx)
    }

    #[must_use]
    pub fn column_f32(&self, idx: usize) -> Option<&[f32]> {
        self.column(idx)?.data().as_f32_slice()
    }

    #[must_use]
    pub fn column_f64(&self, idx: usize) -> Option<&[f64]> {
        self.column(idx)?.data().as_f64_slice()
    }

    #[must_use]
    pub fn column_i64(&self, idx: usize) -> Option<&[i64]> {
        self.column(idx)?.data().as_i64_slice()
    }

    #[must_use]
    pub const fn selection(&self) -> &BatchSelection {
        &self.selection
    }

    #[must_use]
    pub fn selection_mask(&self) -> Option<&[u8]> {
        self.selection.as_slice()
    }

    #[must_use]
    pub fn selection_ptr(&self) -> *const u8 {
        self.selection.as_ptr()
    }

    #[must_use]
    pub fn selected_rows(&self) -> usize {
        self.selection.selected_rows(self.num_rows)
    }

    pub fn set_selection_mask(&mut self, mask: Vec<u8>) {
        self.selection = BatchSelection::from_mask_for_rows(mask, self.num_rows);
    }

    pub fn clear_selection_mask(&mut self) {
        self.selection = BatchSelection::AllRows;
    }

    /// Add a column of `i32` values.
    pub fn add_col_i32(&mut self, values: Vec<i32>, nulls: Vec<u8>) {
        self.push_column(
            ColumnarColumnData::Int32(values),
            ColumnarNulls::from_mask_for_rows(nulls, self.num_rows),
        );
    }

    /// Add an all-valid column of `i32` values.
    #[allow(dead_code)] // reason: all-valid i32 constructor is pinned by batch-contract tests
    pub fn add_col_i32_all_valid(&mut self, values: Vec<i32>) {
        self.push_column(ColumnarColumnData::Int32(values), ColumnarNulls::AllValid);
    }

    /// Add a column of `i64` values.
    pub fn add_col_i64(&mut self, values: Vec<i64>, nulls: Vec<u8>) {
        self.push_column(
            ColumnarColumnData::Int64(values),
            ColumnarNulls::from_mask_for_rows(nulls, self.num_rows),
        );
    }

    /// Add an all-valid column of `i64` values.
    pub fn add_col_i64_all_valid(&mut self, values: Vec<i64>) {
        self.push_column(ColumnarColumnData::Int64(values), ColumnarNulls::AllValid);
    }

    /// Add a column of `f32` values.
    pub fn add_col_f32(&mut self, values: Vec<f32>, nulls: Vec<u8>) {
        self.push_column(
            ColumnarColumnData::Float32(values),
            ColumnarNulls::from_mask_for_rows(nulls, self.num_rows),
        );
    }

    /// Add an all-valid column of `f32` values.
    pub fn add_col_f32_all_valid(&mut self, values: Vec<f32>) {
        self.push_column(ColumnarColumnData::Float32(values), ColumnarNulls::AllValid);
    }

    /// Add a column of `f64` values.
    pub fn add_col_f64(&mut self, values: Vec<f64>, nulls: Vec<u8>) {
        self.push_column(
            ColumnarColumnData::Float64(values),
            ColumnarNulls::from_mask_for_rows(nulls, self.num_rows),
        );
    }

    /// Add an all-valid column of `f64` values.
    pub fn add_col_f64_all_valid(&mut self, values: Vec<f64>) {
        self.push_column(ColumnarColumnData::Float64(values), ColumnarNulls::AllValid);
    }

    /// Add a column of `bool` values.
    pub fn add_col_bool(&mut self, values: Vec<bool>, nulls: Vec<u8>) {
        let normalized = values.into_iter().map(u8::from).collect();
        self.push_column(
            ColumnarColumnData::Bool(normalized),
            ColumnarNulls::from_mask_for_rows(nulls, self.num_rows),
        );
    }

    /// Add an all-valid column of `bool` values.
    #[allow(dead_code)] // reason: bool all-valid constructor is pinned by batch-contract tests
    pub fn add_col_bool_all_valid(&mut self, values: Vec<bool>) {
        let normalized = values.into_iter().map(u8::from).collect();
        self.push_column(
            ColumnarColumnData::Bool(normalized),
            ColumnarNulls::AllValid,
        );
    }

    /// Add a PostgreSQL `date` column (`DateADT`, days since PG epoch).
    #[allow(dead_code)] // reason: date column ABI is retained while planner exposure stays disabled
    pub fn add_col_date(&mut self, values: Vec<i32>, nulls: Vec<u8>) {
        self.push_column(
            ColumnarColumnData::Date(values),
            ColumnarNulls::from_mask_for_rows(nulls, self.num_rows),
        );
    }

    /// Add a PostgreSQL timestamp/timestamptz column (`Timestamp`, microseconds).
    #[allow(dead_code)] // reason: timestamp column ABI is retained while planner exposure stays disabled
    pub fn add_col_timestamp(&mut self, values: Vec<i64>, nulls: Vec<u8>) {
        self.push_column(
            ColumnarColumnData::Timestamp(values),
            ColumnarNulls::from_mask_for_rows(nulls, self.num_rows),
        );
    }

    /// Finalize pointer arrays and return a `PgaccelBatch` that borrows
    /// from this owner.
    ///
    /// The returned batch is valid as long as `self` is not dropped or modified.
    pub fn as_batch(&mut self) -> PgaccelBatch {
        assert_eq!(
            self.columns.len(),
            self.num_cols,
            "columnar batch must be fully populated before FFI handoff"
        );

        self.col_data_ptrs.clear();
        self.col_null_ptrs.clear();

        for col in &self.columns {
            self.col_data_ptrs.push(col.data.as_ptr());
            self.col_null_ptrs.push(col.null_ptr());
        }

        PgaccelBatch {
            num_rows: self.num_rows,
            num_cols: self.columns.len(),
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

    // -----------------------------------------------------------------------
    // ColumnarBatchOwner::new
    // -----------------------------------------------------------------------

    #[test]
    fn new_zero_rows_zero_cols() {
        let owner = ColumnarBatchOwner::new(0, 0);
        assert_eq!(owner.num_rows, 0);
        assert_eq!(owner.num_cols, 0);
        assert_eq!(owner.actual_num_cols(), 0);
        assert!(owner.col_types.is_empty());
        assert_eq!(owner.residency, BatchResidency::HostColumnar);
        assert_eq!(owner.cpu_boundary, CpuBoundaryReason::HostInputStaging);
        assert!(!owner.gpu_resident_pipeline());
        assert_eq!(
            owner.resident_proof().cpu_boundary(),
            CpuBoundaryReason::HostInputStaging
        );
    }

    #[test]
    fn new_preallocates_capacity() {
        let owner = ColumnarBatchOwner::new(1024, 5);
        assert_eq!(owner.num_rows, 1024);
        assert_eq!(owner.num_cols, 5);
        assert_eq!(owner.actual_num_cols(), 0);
        assert!(owner.col_types.capacity() >= 5);
    }

    #[test]
    fn host_boundary_labels_block_resident_pipeline() {
        let mut owner = ColumnarBatchOwner::new(2, 1);
        owner.add_col_i32_all_valid(vec![1, 2]);

        owner.mark_host_boundary(CpuBoundaryReason::ExecProcNodeInput);

        assert_eq!(owner.residency, BatchResidency::HostColumnar);
        assert_eq!(owner.cpu_boundary, CpuBoundaryReason::ExecProcNodeInput);
        assert!(!owner.gpu_resident_pipeline());
        assert!(
            owner
                .cpu_boundary_label()
                .contains("PostgreSQL child executor")
        );
    }

    #[test]
    fn device_resident_proof_marks_pipeline_selectable() {
        let mut owner = ColumnarBatchOwner::new(3, 1);
        owner.add_col_f32_all_valid(vec![1.0, 2.0, 3.0]);
        let proof = ResidentPipelineProof::device_resident(
            ResidentOperatorClass::ResidentExpression,
            vec![
                ResidentOperatorStage::Scan,
                ResidentOperatorStage::Expression,
                ResidentOperatorStage::Aggregate,
                ResidentOperatorStage::FinalMaterialization,
            ],
            MaterializationBoundary::FinalOutput,
            1,
            true,
            false,
        );

        owner.mark_device_resident(proof);

        assert_eq!(owner.residency, BatchResidency::DeviceResident);
        assert_eq!(owner.cpu_boundary, CpuBoundaryReason::None);
        assert!(owner.gpu_resident_pipeline());
        assert_eq!(owner.resident_proof().device_columns(), 1);
        assert!(owner.resident_proof().has_device_selection());
        assert_eq!(
            owner.resident_proof().materialization(),
            MaterializationBoundary::FinalOutput
        );
    }

    #[test]
    #[should_panic(
        expected = "resident pipeline proof must include at least one GPU operator stage"
    )]
    fn device_resident_proof_rejects_empty_stage_list() {
        let _ = ResidentPipelineProof::device_resident(
            ResidentOperatorClass::ResidentSource,
            Vec::new(),
            MaterializationBoundary::FinalOutput,
            1,
            false,
            false,
        );
    }

    #[test]
    #[should_panic(
        expected = "resident pipeline proof cannot include an intermediate host boundary"
    )]
    fn device_resident_proof_rejects_host_intermediate_materialization() {
        let _ = ResidentPipelineProof::device_resident(
            ResidentOperatorClass::ResidentSource,
            vec![ResidentOperatorStage::Scan],
            MaterializationBoundary::HostIntermediate(CpuBoundaryReason::HostTupleReconstruction),
            1,
            false,
            false,
        );
    }

    #[test]
    #[should_panic(expected = "resident pipeline proof must name a reusable operator class")]
    fn device_resident_proof_requires_operator_class() {
        let _ = ResidentPipelineProof::device_resident(
            ResidentOperatorClass::Unspecified,
            vec![ResidentOperatorStage::Scan],
            MaterializationBoundary::None,
            1,
            false,
            false,
        );
    }

    #[test]
    #[should_panic(expected = "resident pipeline proof must own at least one device column/buffer")]
    fn device_resident_proof_requires_device_buffers() {
        let _ = ResidentPipelineProof::device_resident(
            ResidentOperatorClass::ResidentSource,
            vec![ResidentOperatorStage::Scan],
            MaterializationBoundary::None,
            0,
            false,
            false,
        );
    }

    #[test]
    fn device_buffer_ref_allows_zero_length_null_pointer_only() {
        let empty = DeviceBufferRef::new(
            std::ptr::null(),
            0,
            Some(PgaccelValTag::Float32),
            BatchResidency::DeviceResident,
            ResidentOperatorStage::Scan,
        );
        let invalid = DeviceBufferRef::new(
            std::ptr::null(),
            4,
            Some(PgaccelValTag::Float32),
            BatchResidency::DeviceResident,
            ResidentOperatorStage::Scan,
        );

        assert!(empty.is_valid());
        assert!(!invalid.is_valid());
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
        let col = owner.column(0).unwrap();
        assert_eq!(col.tag(), PgaccelValTag::Int32);
        assert_eq!(col.data().byte_len(), 3 * std::mem::size_of::<i32>());
        assert!(owner.selection().is_all_rows());
        assert_eq!(owner.selected_rows(), 3);
        assert!(owner.selection_ptr().is_null());
    }

    #[test]
    fn add_col_i64_stores_bytes_and_tag() {
        let mut owner = ColumnarBatchOwner::new(2, 1);
        owner.add_col_i64(vec![100_i64, 200], vec![0, 0]);

        assert_eq!(owner.col_types[0], PgaccelValTag::Int64);
        assert_eq!(
            owner.column(0).unwrap().data().byte_len(),
            2 * std::mem::size_of::<i64>()
        );
    }

    #[test]
    fn add_col_f32_stores_bytes_and_tag() {
        let mut owner = ColumnarBatchOwner::new(2, 1);
        owner.add_col_f32(vec![1.5_f32, 2.5], vec![0, 0]);

        assert_eq!(owner.col_types[0], PgaccelValTag::Float32);
        assert_eq!(
            owner.column(0).unwrap().data().byte_len(),
            2 * std::mem::size_of::<f32>()
        );
    }

    #[test]
    fn add_col_f64_stores_bytes_and_tag() {
        let mut owner = ColumnarBatchOwner::new(2, 1);
        owner.add_col_f64(vec![3.125_f64, 2.75], vec![0, 0]);

        assert_eq!(owner.col_types[0], PgaccelValTag::Float64);
        assert_eq!(
            owner.column(0).unwrap().data().byte_len(),
            2 * std::mem::size_of::<f64>()
        );
    }

    #[test]
    fn add_col_bool_stores_bytes_and_tag() {
        let mut owner = ColumnarBatchOwner::new(3, 1);
        owner.add_col_bool(vec![true, false, true], vec![0, 0, 0]);

        assert_eq!(owner.col_types[0], PgaccelValTag::Bool);
        assert_eq!(owner.column(0).unwrap().data().byte_len(), 3);
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
        assert_eq!(owner.actual_num_cols(), 3);
    }

    // -----------------------------------------------------------------------
    // add_col_* — empty (0 rows)
    // -----------------------------------------------------------------------

    #[test]
    fn add_col_i32_zero_rows() {
        let mut owner = ColumnarBatchOwner::new(0, 1);
        owner.add_col_i32(vec![], vec![]);

        assert_eq!(owner.column(0).unwrap().data().byte_len(), 0);
        assert!(owner.column(0).unwrap().nulls().is_none());
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

        assert_eq!(
            owner.column(0).unwrap().data().byte_len(),
            n * std::mem::size_of::<i32>()
        );
        assert!(owner.column(0).unwrap().nulls().is_none());
    }

    // -----------------------------------------------------------------------
    // Null buffer correctness
    // -----------------------------------------------------------------------

    #[test]
    fn null_buffer_all_null() {
        let mut owner = ColumnarBatchOwner::new(4, 1);
        let nulls = vec![1_u8; 4];
        owner.add_col_i32(vec![0, 0, 0, 0], nulls);

        assert!(
            owner
                .column(0)
                .unwrap()
                .nulls()
                .unwrap()
                .iter()
                .all(|&b| b == 1)
        );
    }

    #[test]
    fn null_buffer_no_null() {
        let mut owner = ColumnarBatchOwner::new(4, 1);
        let nulls = vec![0_u8; 4];
        owner.add_col_i32(vec![1, 2, 3, 4], nulls);

        assert!(owner.column(0).unwrap().nulls().is_none());
        assert!(!owner.column(0).unwrap().has_explicit_null_mask());
    }

    #[test]
    fn null_buffer_mixed_pattern() {
        let mut owner = ColumnarBatchOwner::new(6, 1);
        let nulls = vec![0_u8, 1, 0, 1, 1, 0];
        owner.add_col_i32(vec![10, 0, 20, 0, 0, 30], nulls);

        assert_eq!(
            owner.column(0).unwrap().nulls().unwrap(),
            &[0, 1, 0, 1, 1, 0]
        );
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

    #[test]
    fn as_batch_data_pointer_reads_typed_values() {
        let mut owner = ColumnarBatchOwner::new(2, 1);
        owner.add_col_f64(vec![3.5, 7.25], vec![0, 0]);

        let batch = owner.as_batch();
        // SAFETY: the batch borrows one typed f64 column from owner.
        let col_ptrs = unsafe { std::slice::from_raw_parts(batch.col_data, 1) };
        // SAFETY: col_ptrs[0] points to two f64 values owned by owner.
        let values = unsafe { std::slice::from_raw_parts(col_ptrs[0].cast::<f64>(), 2) };

        assert_eq!(values, &[3.5, 7.25]);
    }

    #[test]
    fn as_batch_data_pointer_preserves_native_alignment() {
        let mut owner = ColumnarBatchOwner::new(2, 1);
        owner.add_col_f64(vec![1.0, 2.0], vec![0, 0]);

        let batch = owner.as_batch();
        // SAFETY: the batch borrows one typed f64 column from owner.
        let col_ptrs = unsafe { std::slice::from_raw_parts(batch.col_data, 1) };
        let ptr = col_ptrs[0] as usize;

        assert_eq!(ptr % std::mem::align_of::<f64>(), 0);
        assert_eq!(
            owner.column(0).unwrap().data().element_align(),
            std::mem::align_of::<f64>()
        );
    }

    #[test]
    fn as_batch_mixed_null_pointer_array_preserves_column_order() {
        let mut owner = ColumnarBatchOwner::new(3, 3);
        owner.add_col_i32(vec![1, 2, 3], vec![0, 0, 0]);
        owner.add_col_f64(vec![1.0, 0.0, 3.0], vec![0, 1, 0]);
        owner.add_col_i64(vec![10, 20, 30], vec![0, 0, 0]);

        let batch = owner.as_batch();
        // SAFETY: batch pointer arrays borrow three entries from owner.
        let null_ptrs = unsafe { std::slice::from_raw_parts(batch.col_nulls, 3) };
        let types = unsafe { std::slice::from_raw_parts(batch.col_types, 3) };

        assert!(null_ptrs[0].is_null());
        assert!(!null_ptrs[1].is_null());
        assert!(null_ptrs[2].is_null());
        assert_eq!(
            types,
            &[
                PgaccelValTag::Int32,
                PgaccelValTag::Float64,
                PgaccelValTag::Int64
            ]
        );
    }

    #[test]
    fn as_batch_underlying_value_and_null_entries_stable_across_calls() {
        let mut owner = ColumnarBatchOwner::new(3, 1);
        owner.add_col_f64(vec![1.0, 0.0, 3.0], vec![0, 1, 0]);

        let batch1 = owner.as_batch();
        // SAFETY: batch1 borrows one data/null entry from owner.
        let data_ptr1 = unsafe { std::slice::from_raw_parts(batch1.col_data, 1) }[0];
        let null_ptr1 = unsafe { std::slice::from_raw_parts(batch1.col_nulls, 1) }[0];

        let batch2 = owner.as_batch();
        // SAFETY: batch2 borrows one data/null entry from the same owner.
        let data_ptr2 = unsafe { std::slice::from_raw_parts(batch2.col_data, 1) }[0];
        let null_ptr2 = unsafe { std::slice::from_raw_parts(batch2.col_nulls, 1) }[0];

        assert_eq!(data_ptr1, data_ptr2);
        assert_eq!(null_ptr1, null_ptr2);
        assert!(!null_ptr1.is_null());
    }

    #[test]
    #[should_panic(expected = "columnar batch must be fully populated before FFI handoff")]
    fn as_batch_rejects_partially_populated_batch() {
        let mut owner = ColumnarBatchOwner::new(3, 2);
        owner.add_col_f64(vec![1.0, 2.0, 3.0], vec![0, 0, 0]);

        let _ = owner.as_batch();
    }

    #[test]
    #[should_panic(expected = "column null-mask count must match batch row count")]
    fn add_col_rejects_mismatched_null_mask_len() {
        let mut owner = ColumnarBatchOwner::new(3, 1);
        owner.add_col_f64(vec![1.0, 2.0, 3.0], vec![0, 0]);
    }

    #[test]
    fn all_valid_columns_emit_null_mask_null_pointer() {
        let mut owner = ColumnarBatchOwner::new(3, 1);
        owner.add_col_i64_all_valid(vec![1, 2, 3]);

        let batch = owner.as_batch();
        // SAFETY: the batch borrows one null pointer slot from owner.
        let null_ptrs = unsafe { std::slice::from_raw_parts(batch.col_nulls, 1) };

        assert!(owner.column(0).unwrap().nulls().is_none());
        assert!(!owner.column(0).unwrap().has_explicit_null_mask());
        assert!(null_ptrs[0].is_null());
    }

    #[test]
    fn zero_masks_collapse_to_all_valid_null_pointer() {
        let mut owner = ColumnarBatchOwner::new(3, 1);
        owner.add_col_f32(vec![1.0, 2.0, 3.0], vec![0, 0, 0]);

        let batch = owner.as_batch();
        // SAFETY: the batch borrows one null pointer slot from owner.
        let null_ptrs = unsafe { std::slice::from_raw_parts(batch.col_nulls, 1) };

        assert!(owner.column(0).unwrap().nulls().is_none());
        assert!(null_ptrs[0].is_null());
    }

    #[test]
    fn mixed_null_masks_emit_non_null_pointer() {
        let mut owner = ColumnarBatchOwner::new(3, 1);
        owner.add_col_f32(vec![1.0, 0.0, 3.0], vec![0, 1, 0]);

        let batch = owner.as_batch();
        // SAFETY: the batch borrows one null pointer slot from owner.
        let null_ptrs = unsafe { std::slice::from_raw_parts(batch.col_nulls, 1) };
        // SAFETY: null_ptrs[0] points to the explicit three-byte null mask.
        let mask = unsafe { std::slice::from_raw_parts(null_ptrs[0], 3) };

        assert!(!null_ptrs[0].is_null());
        assert_eq!(mask, &[0, 1, 0]);
    }

    // -----------------------------------------------------------------------
    // Selection mask metadata
    // -----------------------------------------------------------------------

    #[test]
    fn selection_mask_filters_rows_with_one_selected_polarity() {
        let mut owner = ColumnarBatchOwner::new(4, 1);
        owner.add_col_i32_all_valid(vec![10, 20, 30, 40]);

        owner.set_selection_mask(vec![1, 0, 1, 0]);

        assert_eq!(owner.selection_mask().unwrap(), &[1, 0, 1, 0]);
        assert_eq!(owner.selected_rows(), 2);
        assert!(!owner.selection_ptr().is_null());
    }

    #[test]
    fn all_selected_mask_collapses_to_null_selection_pointer() {
        let mut owner = ColumnarBatchOwner::new(3, 1);
        owner.add_col_i64_all_valid(vec![1, 2, 3]);

        owner.set_selection_mask(vec![1, 1, 1]);

        assert!(owner.selection().is_all_rows());
        assert!(owner.selection_mask().is_none());
        assert!(owner.selection_ptr().is_null());
        assert_eq!(owner.selected_rows(), 3);
    }

    #[test]
    fn clear_selection_mask_restores_all_rows() {
        let mut owner = ColumnarBatchOwner::new(3, 1);
        owner.add_col_f64_all_valid(vec![1.0, 2.0, 3.0]);
        owner.set_selection_mask(vec![0, 1, 0]);

        owner.clear_selection_mask();

        assert!(owner.selection().is_all_rows());
        assert_eq!(owner.selected_rows(), 3);
    }

    #[test]
    #[should_panic(expected = "batch selection-mask count must match batch row count")]
    fn selection_mask_rejects_mismatched_len() {
        let mut owner = ColumnarBatchOwner::new(3, 1);
        owner.add_col_f32_all_valid(vec![1.0, 2.0, 3.0]);

        owner.set_selection_mask(vec![1, 0]);
    }

    #[test]
    #[should_panic(expected = "batch selection-mask values must be 0 or 1")]
    fn selection_mask_rejects_non_binary_values() {
        let mut owner = ColumnarBatchOwner::new(2, 1);
        owner.add_col_i32_all_valid(vec![1, 2]);

        owner.set_selection_mask(vec![1, 2]);
    }

    #[test]
    #[should_panic(expected = "column null-mask values must be 0 or 1")]
    fn non_binary_null_masks_are_rejected() {
        let mut owner = ColumnarBatchOwner::new(2, 1);
        owner.add_col_i32(vec![1, 2], vec![0, 2]);
    }

    #[test]
    fn bool_columns_are_normalized_to_bytes() {
        let mut owner = ColumnarBatchOwner::new(3, 1);
        owner.add_col_bool(vec![true, false, true], vec![0, 0, 1]);

        let batch = owner.as_batch();
        // SAFETY: the batch borrows one byte-backed bool column from owner.
        let col_ptrs = unsafe { std::slice::from_raw_parts(batch.col_data, 1) };
        // SAFETY: col_ptrs[0] points to three u8 values owned by owner.
        let values = unsafe { std::slice::from_raw_parts(col_ptrs[0].cast::<u8>(), 3) };

        assert_eq!(values, &[1, 0, 1]);
        assert_eq!(owner.column(0).unwrap().nulls().unwrap(), &[0, 0, 1]);
    }

    #[test]
    fn typed_column_accessors_return_matching_slices() {
        let mut owner = ColumnarBatchOwner::new(2, 3);
        owner.add_col_f32_all_valid(vec![1.25, 2.5]);
        owner.add_col_f64_all_valid(vec![3.5, 4.75]);
        owner.add_col_i64_all_valid(vec![5, 6]);

        assert_eq!(owner.column_f32(0).unwrap(), &[1.25, 2.5]);
        assert_eq!(owner.column_f64(1).unwrap(), &[3.5, 4.75]);
        assert_eq!(owner.column_i64(2).unwrap(), &[5, 6]);
        assert!(owner.column_f64(0).is_none());
    }

    #[test]
    fn all_valid_typed_helpers_emit_null_pointers_for_each_h3_type() {
        let mut owner = ColumnarBatchOwner::new(2, 3);
        owner.add_col_f32_all_valid(vec![1.0, 2.0]);
        owner.add_col_f64_all_valid(vec![3.0, 4.0]);
        owner.add_col_i64_all_valid(vec![5, 6]);

        let batch = owner.as_batch();
        // SAFETY: the batch borrows three null pointer slots from owner.
        let null_ptrs = unsafe { std::slice::from_raw_parts(batch.col_nulls, 3) };

        assert!(null_ptrs.iter().all(|ptr| ptr.is_null()));
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
        let cloned = tag;
        let copied = tag;
        assert_eq!(tag, cloned);
        assert_eq!(tag, copied);
    }
}
