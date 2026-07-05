//! Borrowed descriptor scaffolding for typed GPU facade calls.
//!
//! These descriptors validate host-side buffer shape before a future facade
//! forwards pointers to the existing C ABI mirror. They are intentionally
//! additive: no existing wrapper in `gpu/mod.rs` is routed through this layer
//! yet, so behaviour remains unchanged.

use std::ffi::c_void;
use std::mem;

use super::error::{GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail};
use super::types::PgaccelValTag;

/// Maximum element count accepted by facade descriptors.
///
/// Current GPU kernels use `u32` row and pair indices in multiple places, so
/// descriptor construction rejects larger logical lengths before an FFI call
/// can silently truncate.
#[allow(dead_code)] // reason: additive facade bound; future typed callers validate against it
pub const MAX_DEVICE_LEN: usize = u32::MAX as usize;

#[allow(dead_code)] // reason: shared helper for additive facade constructors
fn descriptor_error(
    operation: GpuOperation,
    status: GpuStatusDetail,
    detail: &'static str,
) -> GpuError {
    GpuError::with_detail(GpuErrorDomain::Descriptor, operation, status, detail)
}

/// Validate that a logical buffer length can be represented by GPU kernels.
#[allow(dead_code)] // reason: public validation helper for additive facade descriptors
pub fn validate_device_len(len: usize, operation: GpuOperation) -> GpuResult<()> {
    if len > MAX_DEVICE_LEN {
        Err(descriptor_error(
            operation,
            GpuStatusDetail::CapacityOverflow,
            "length exceeds u32-indexed GPU descriptor capacity",
        ))
    } else {
        Ok(())
    }
}

#[allow(dead_code)] // reason: shared helper for additive facade constructors
fn validate_element_type<T>(operation: GpuOperation) -> GpuResult<()> {
    if mem::size_of::<T>() == 0 {
        Err(descriptor_error(
            operation,
            GpuStatusDetail::InvalidDescriptor,
            "zero-sized element types cannot be passed to GPU kernels",
        ))
    } else {
        Ok(())
    }
}

#[allow(dead_code)] // reason: shared helper for additive facade constructors
fn validate_device_slice<T>(len: usize, operation: GpuOperation) -> GpuResult<()> {
    validate_element_type::<T>(operation)?;
    validate_device_len(len, operation)?;
    if len.checked_mul(mem::size_of::<T>()).is_none() {
        Err(descriptor_error(
            operation,
            GpuStatusDetail::CapacityOverflow,
            "buffer byte length overflows usize",
        ))
    } else {
        Ok(())
    }
}

/// Validate an optional null mask.
///
/// A null mask must have one byte per row, with `1` for NULL and `0` for
/// non-NULL. This mirrors the existing kernel ABI comments.
#[allow(dead_code)] // reason: public validation helper for additive facade descriptors
pub fn validate_null_mask(
    nulls: Option<&[u8]>,
    expected_len: usize,
    operation: GpuOperation,
) -> GpuResult<()> {
    let Some(nulls) = nulls else {
        return Ok(());
    };

    if nulls.len() != expected_len {
        return Err(descriptor_error(
            operation,
            GpuStatusDetail::ShapeMismatch,
            "null mask length must match value length",
        ));
    }

    if nulls.iter().any(|&v| v > 1) {
        return Err(descriptor_error(
            operation,
            GpuStatusDetail::InvalidDescriptor,
            "null mask values must be 0 or 1",
        ));
    }

    Ok(())
}

/// Validate an optional row-selection mask.
///
/// A selection mask has one byte per row, with `1` for selected/visible rows
/// and `0` for filtered rows. `None` means every row is selected.
#[allow(dead_code)] // reason: public validation helper for upcoming masked batch consumers
pub fn validate_selection_mask(
    selection: Option<&[u8]>,
    expected_len: usize,
    operation: GpuOperation,
) -> GpuResult<()> {
    let Some(selection) = selection else {
        return Ok(());
    };

    if selection.len() != expected_len {
        return Err(descriptor_error(
            operation,
            GpuStatusDetail::ShapeMismatch,
            "selection mask length must match row count",
        ));
    }

    if selection.iter().any(|&v| v > 1) {
        return Err(descriptor_error(
            operation,
            GpuStatusDetail::InvalidDescriptor,
            "selection mask values must be 0 or 1",
        ));
    }

    Ok(())
}

/// Validate an initialized CSR layout.
///
/// Offsets must include the terminal offset, start at zero, be monotonic, and
/// point inside the values buffer.
#[allow(dead_code)] // reason: public validation helper for additive facade descriptors
pub fn validate_csr_offsets(row_offsets: &[u32], values_len: usize) -> GpuResult<()> {
    validate_device_len(values_len, GpuOperation::ValidateCsrOutput)?;
    validate_device_len(row_offsets.len(), GpuOperation::ValidateCsrOutput)?;

    let Some((&first, rest)) = row_offsets.split_first() else {
        return Err(descriptor_error(
            GpuOperation::ValidateCsrOutput,
            GpuStatusDetail::InvalidDescriptor,
            "CSR offsets must include at least the terminal offset",
        ));
    };

    if first != 0 {
        return Err(descriptor_error(
            GpuOperation::ValidateCsrOutput,
            GpuStatusDetail::InvalidDescriptor,
            "CSR offsets must start at zero",
        ));
    }

    let mut previous = first;
    for &offset in rest {
        if offset < previous {
            return Err(descriptor_error(
                GpuOperation::ValidateCsrOutput,
                GpuStatusDetail::ShapeMismatch,
                "CSR offsets must be monotonic",
            ));
        }
        previous = offset;
    }

    if previous as usize > values_len {
        return Err(descriptor_error(
            GpuOperation::ValidateCsrOutput,
            GpuStatusDetail::ShapeMismatch,
            "CSR terminal offset exceeds values length",
        ));
    }

    Ok(())
}

/// Borrowed immutable input buffer for a GPU facade call.
#[allow(dead_code)] // reason: additive typed descriptor; existing wrappers still pass slices directly
#[derive(Debug, Clone, Copy)]
pub struct DeviceInput<'a, T> {
    values: &'a [T],
    nulls: Option<&'a [u8]>,
}

#[allow(dead_code)] // reason: additive typed descriptor methods for future facade callers
impl<'a, T> DeviceInput<'a, T> {
    /// Create an input descriptor without a null mask.
    pub fn new(values: &'a [T]) -> GpuResult<Self> {
        validate_device_slice::<T>(values.len(), GpuOperation::ValidateDeviceInput)?;
        Ok(Self {
            values,
            nulls: None,
        })
    }

    /// Create an input descriptor with a per-row null mask.
    pub fn with_nulls(values: &'a [T], nulls: &'a [u8]) -> GpuResult<Self> {
        validate_device_slice::<T>(values.len(), GpuOperation::ValidateDeviceInput)?;
        validate_null_mask(Some(nulls), values.len(), GpuOperation::ValidateDeviceInput)?;
        Ok(Self {
            values,
            nulls: Some(nulls),
        })
    }

    /// Value slice backing this descriptor.
    #[must_use]
    pub const fn values(&self) -> &'a [T] {
        self.values
    }

    /// Optional null mask backing this descriptor.
    #[must_use]
    pub const fn nulls(&self) -> Option<&'a [u8]> {
        self.nulls
    }

    /// Number of logical rows/elements.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns `true` when the descriptor has no elements.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Raw pointer to the value buffer for FFI staging.
    #[must_use]
    pub const fn as_ptr(&self) -> *const T {
        self.values.as_ptr()
    }

    /// Byte length of the value buffer.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        mem::size_of_val(self.values)
    }
}

/// Borrowed mutable output buffer for a GPU facade call.
#[allow(dead_code)] // reason: additive typed descriptor; existing wrappers still pass slices directly
#[derive(Debug)]
pub struct DeviceOutput<'a, T> {
    values: &'a mut [T],
}

#[allow(dead_code)] // reason: additive typed descriptor methods for future facade callers
impl<'a, T> DeviceOutput<'a, T> {
    /// Create an output descriptor.
    pub fn new(values: &'a mut [T]) -> GpuResult<Self> {
        validate_device_slice::<T>(values.len(), GpuOperation::ValidateDeviceOutput)?;
        Ok(Self { values })
    }

    /// Create an output descriptor that must match an expected logical length.
    pub fn for_len(values: &'a mut [T], expected_len: usize) -> GpuResult<Self> {
        validate_device_slice::<T>(values.len(), GpuOperation::ValidateDeviceOutput)?;
        validate_device_len(expected_len, GpuOperation::ValidateDeviceOutput)?;
        if values.len() != expected_len {
            return Err(descriptor_error(
                GpuOperation::ValidateDeviceOutput,
                GpuStatusDetail::ShapeMismatch,
                "output length must match expected length",
            ));
        }
        Ok(Self { values })
    }

    /// Number of output elements.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns `true` when the descriptor has no elements.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Mutable raw pointer to the output buffer for FFI staging.
    #[must_use]
    pub fn as_mut_ptr(&mut self) -> *mut T {
        self.values.as_mut_ptr()
    }

    /// Mutable value slice backing this descriptor.
    #[must_use]
    pub fn values_mut(&mut self) -> &mut [T] {
        self.values
    }
}

/// Borrowed mutable CSR output buffers.
#[allow(dead_code)] // reason: additive typed descriptor; existing wrappers still pass raw CSR pieces
#[derive(Debug)]
pub struct CsrOutput<'a, T> {
    values: &'a mut [T],
    row_offsets: &'a mut [u32],
}

#[allow(dead_code)] // reason: additive typed descriptor methods for future facade callers
impl<'a, T> CsrOutput<'a, T> {
    /// Create a CSR output descriptor for caller-allocated buffers.
    ///
    /// This validates capacities only; use [`Self::from_initialized`] or
    /// [`Self::validate_initialized`] after offsets have been populated.
    pub fn with_capacity(values: &'a mut [T], row_offsets: &'a mut [u32]) -> GpuResult<Self> {
        validate_device_slice::<T>(values.len(), GpuOperation::ValidateCsrOutput)?;
        validate_device_len(row_offsets.len(), GpuOperation::ValidateCsrOutput)?;
        if row_offsets.is_empty() {
            return Err(descriptor_error(
                GpuOperation::ValidateCsrOutput,
                GpuStatusDetail::InvalidDescriptor,
                "CSR offsets must include at least the terminal offset",
            ));
        }
        Ok(Self {
            values,
            row_offsets,
        })
    }

    /// Create a CSR output descriptor whose offsets are already initialized.
    pub fn from_initialized(values: &'a mut [T], row_offsets: &'a mut [u32]) -> GpuResult<Self> {
        validate_device_slice::<T>(values.len(), GpuOperation::ValidateCsrOutput)?;
        validate_csr_offsets(row_offsets, values.len())?;
        Ok(Self {
            values,
            row_offsets,
        })
    }

    /// Validate currently initialized offsets against the values buffer.
    pub fn validate_initialized(&self) -> GpuResult<()> {
        validate_csr_offsets(self.row_offsets, self.values.len())
    }

    /// Number of logical CSR rows.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.row_offsets.len().saturating_sub(1)
    }

    /// Value buffer capacity in elements.
    #[must_use]
    pub const fn values_len(&self) -> usize {
        self.values.len()
    }

    /// Mutable value pointer for FFI staging.
    #[must_use]
    pub fn values_mut_ptr(&mut self) -> *mut T {
        self.values.as_mut_ptr()
    }

    /// Mutable row-offset pointer for FFI staging.
    #[must_use]
    pub fn row_offsets_mut_ptr(&mut self) -> *mut u32 {
        self.row_offsets.as_mut_ptr()
    }
}

/// Borrowed typed column data supported by the expression batch ABI.
#[allow(dead_code)] // reason: additive typed descriptor; existing expression batching uses current owner type
#[derive(Debug, Clone, Copy)]
pub enum ColumnData<'a> {
    Bool(&'a [bool]),
    Int32(&'a [i32]),
    Int64(&'a [i64]),
    Float32(&'a [f32]),
    Float64(&'a [f64]),
    Date(&'a [i32]),
    Timestamp(&'a [i64]),
}

#[allow(dead_code)] // reason: additive typed descriptor methods for future facade callers
impl ColumnData<'_> {
    /// Number of rows in this column.
    #[must_use]
    pub const fn len(self) -> usize {
        match self {
            Self::Bool(values) => values.len(),
            Self::Int32(values) | Self::Date(values) => values.len(),
            Self::Int64(values) | Self::Timestamp(values) => values.len(),
            Self::Float32(values) => values.len(),
            Self::Float64(values) => values.len(),
        }
    }

    /// Returns `true` when the column has no rows.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// FFI value tag for this column.
    #[must_use]
    pub const fn value_tag(self) -> PgaccelValTag {
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

    /// Raw pointer to the column data for FFI staging.
    #[must_use]
    pub const fn as_ptr(self) -> *const c_void {
        match self {
            Self::Bool(values) => values.as_ptr().cast::<c_void>(),
            Self::Int32(values) | Self::Date(values) => values.as_ptr().cast::<c_void>(),
            Self::Int64(values) | Self::Timestamp(values) => values.as_ptr().cast::<c_void>(),
            Self::Float32(values) => values.as_ptr().cast::<c_void>(),
            Self::Float64(values) => values.as_ptr().cast::<c_void>(),
        }
    }
}

/// One typed column plus its optional null mask.
#[allow(dead_code)] // reason: additive typed descriptor; existing expression batching uses current owner type
#[derive(Debug, Clone, Copy)]
pub struct Column<'a> {
    data: ColumnData<'a>,
    nulls: Option<&'a [u8]>,
}

#[allow(dead_code)] // reason: additive typed descriptor methods for future facade callers
impl<'a> Column<'a> {
    /// Create a typed column descriptor.
    pub fn new(data: ColumnData<'a>, nulls: Option<&'a [u8]>) -> GpuResult<Self> {
        validate_device_len(data.len(), GpuOperation::BuildColumnBatch)?;
        validate_null_mask(nulls, data.len(), GpuOperation::BuildColumnBatch)?;
        Ok(Self { data, nulls })
    }

    /// Typed data backing this column.
    #[must_use]
    pub const fn data(&self) -> ColumnData<'a> {
        self.data
    }

    /// Optional null mask.
    #[must_use]
    pub const fn nulls(&self) -> Option<&'a [u8]> {
        self.nulls
    }

    /// Number of rows in this column.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns `true` when the column has no rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// FFI value tag for this column.
    #[must_use]
    pub const fn value_tag(&self) -> PgaccelValTag {
        self.data.value_tag()
    }
}

/// Borrowed column batch descriptor for expression kernels.
#[allow(dead_code)] // reason: additive typed descriptor; existing expression batching uses current owner type
#[derive(Debug, Clone)]
pub struct ColumnBatch<'a> {
    num_rows: usize,
    columns: Vec<Column<'a>>,
    selection: Option<&'a [u8]>,
}

#[allow(dead_code)] // reason: additive typed descriptor methods for future facade callers
impl<'a> ColumnBatch<'a> {
    /// Create a column batch with an explicit row count.
    pub fn try_new(num_rows: usize, columns: Vec<Column<'a>>) -> GpuResult<Self> {
        Self::try_new_with_selection(num_rows, columns, None)
    }

    /// Create a column batch with an explicit row count and selection mask.
    pub fn try_new_with_selection(
        num_rows: usize,
        columns: Vec<Column<'a>>,
        selection: Option<&'a [u8]>,
    ) -> GpuResult<Self> {
        validate_device_len(num_rows, GpuOperation::BuildColumnBatch)?;
        validate_device_len(columns.len(), GpuOperation::BuildColumnBatch)?;
        validate_selection_mask(selection, num_rows, GpuOperation::BuildColumnBatch)?;

        if columns.iter().any(|column| column.len() != num_rows) {
            return Err(descriptor_error(
                GpuOperation::BuildColumnBatch,
                GpuStatusDetail::ShapeMismatch,
                "all columns must match the batch row count",
            ));
        }

        Ok(Self {
            num_rows,
            columns,
            selection,
        })
    }

    /// Create a column batch and infer the row count from the first column.
    pub fn from_columns(columns: Vec<Column<'a>>) -> GpuResult<Self> {
        let num_rows = columns.first().map_or(0, Column::len);
        Self::try_new(num_rows, columns)
    }

    /// Create a column batch, inferring row count, with a selection mask.
    pub fn from_columns_with_selection(
        columns: Vec<Column<'a>>,
        selection: Option<&'a [u8]>,
    ) -> GpuResult<Self> {
        let num_rows = columns.first().map_or(0, Column::len);
        Self::try_new_with_selection(num_rows, columns, selection)
    }

    /// Number of rows in the batch.
    #[must_use]
    pub const fn num_rows(&self) -> usize {
        self.num_rows
    }

    /// Number of columns in the batch.
    #[must_use]
    pub fn num_cols(&self) -> usize {
        self.columns.len()
    }

    /// Borrow columns in this batch.
    #[must_use]
    pub fn columns(&self) -> &[Column<'a>] {
        &self.columns
    }

    /// Optional row-selection mask (`1 = selected`, `0 = filtered`).
    #[must_use]
    pub const fn selection(&self) -> Option<&'a [u8]> {
        self.selection
    }

    /// Number of selected rows represented by the optional selection mask.
    #[must_use]
    pub fn selected_rows(&self) -> usize {
        self.selection.map_or(self.num_rows, |selection| {
            selection.iter().filter(|&&b| b != 0).count()
        })
    }

    /// Returns `true` when there are no rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.num_rows == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err_status<T>(result: GpuResult<T>) -> GpuStatusDetail {
        match result {
            Ok(_) => panic!("expected descriptor validation to fail"),
            Err(err) => err.status,
        }
    }

    #[test]
    fn validate_device_len_rejects_u32_overflow() {
        assert_eq!(
            err_status(validate_device_len(
                MAX_DEVICE_LEN + 1,
                GpuOperation::ValidateDeviceInput,
            )),
            GpuStatusDetail::CapacityOverflow
        );
    }

    #[test]
    fn device_input_rejects_mismatched_null_mask() {
        let values = [1_i32, 2, 3];
        let nulls = [0_u8, 1];

        assert_eq!(
            err_status(DeviceInput::with_nulls(&values, &nulls)),
            GpuStatusDetail::ShapeMismatch
        );
    }

    #[test]
    fn device_input_rejects_non_binary_null_mask_values() {
        let values = [1_i32, 2, 3];
        let nulls = [0_u8, 2, 1];

        assert_eq!(
            err_status(DeviceInput::with_nulls(&values, &nulls)),
            GpuStatusDetail::InvalidDescriptor
        );
    }

    #[test]
    fn device_input_accepts_matching_null_mask() {
        let values = [1_i32, 2, 3];
        let nulls = [0_u8, 1, 0];
        let input = DeviceInput::with_nulls(&values, &nulls)
            .expect("matching input and null mask should validate");

        assert_eq!(input.len(), 3);
        assert_eq!(input.nulls(), Some(nulls.as_slice()));
    }

    #[test]
    fn device_output_rejects_wrong_expected_len() {
        let mut values = [0_i32; 2];

        assert_eq!(
            err_status(DeviceOutput::for_len(&mut values, 3)),
            GpuStatusDetail::ShapeMismatch
        );
    }

    #[test]
    fn csr_offsets_must_not_be_empty() {
        let mut values = [0_i32; 0];
        let mut row_offsets = [];

        assert_eq!(
            err_status(CsrOutput::with_capacity(&mut values, &mut row_offsets)),
            GpuStatusDetail::InvalidDescriptor
        );
    }

    #[test]
    fn csr_offsets_must_start_at_zero() {
        let mut values = [0_i32; 4];
        let mut row_offsets = [1_u32, 4];

        assert_eq!(
            err_status(CsrOutput::from_initialized(&mut values, &mut row_offsets,)),
            GpuStatusDetail::InvalidDescriptor
        );
    }

    #[test]
    fn csr_offsets_must_be_monotonic() {
        let mut values = [0_i32; 4];
        let mut row_offsets = [0_u32, 3, 2];

        assert_eq!(
            err_status(CsrOutput::from_initialized(&mut values, &mut row_offsets,)),
            GpuStatusDetail::ShapeMismatch
        );
    }

    #[test]
    fn csr_terminal_offset_must_fit_values() {
        let mut values = [0_i32; 3];
        let mut row_offsets = [0_u32, 2, 4];

        assert_eq!(
            err_status(CsrOutput::from_initialized(&mut values, &mut row_offsets,)),
            GpuStatusDetail::ShapeMismatch
        );
    }

    #[test]
    fn csr_initialized_accepts_valid_empty_layout() {
        let mut values = [0_i32; 0];
        let mut row_offsets = [0_u32];
        let csr = CsrOutput::from_initialized(&mut values, &mut row_offsets)
            .expect("empty CSR with terminal zero should validate");

        assert_eq!(csr.row_count(), 0);
        assert_eq!(csr.values_len(), 0);
    }

    #[test]
    fn column_batch_accepts_matching_columns() {
        let ints = [1_i32, 2, 3];
        let floats = [1.0_f64, 2.0, 3.0];
        let nulls = [0_u8, 0, 1];
        let columns = vec![
            Column::new(ColumnData::Int32(&ints), None).expect("int column should validate"),
            Column::new(ColumnData::Float64(&floats), Some(&nulls))
                .expect("float column should validate"),
        ];

        let batch = ColumnBatch::from_columns(columns).expect("matching columns should validate");

        assert_eq!(batch.num_rows(), 3);
        assert_eq!(batch.num_cols(), 2);
        assert_eq!(batch.columns()[1].value_tag(), PgaccelValTag::Float64);
    }

    #[test]
    fn column_batch_accepts_selection_mask() {
        let ints = [1_i32, 2, 3, 4];
        let selection = [1_u8, 0, 1, 0];
        let columns =
            vec![Column::new(ColumnData::Int32(&ints), None).expect("int column should validate")];

        let batch = ColumnBatch::from_columns_with_selection(columns, Some(&selection))
            .expect("selection mask should validate");

        assert_eq!(batch.num_rows(), 4);
        assert_eq!(batch.selection(), Some(selection.as_slice()));
        assert_eq!(batch.selected_rows(), 2);
    }

    #[test]
    fn column_batch_without_selection_treats_all_rows_selected() {
        let ints = [1_i32, 2, 3];
        let columns =
            vec![Column::new(ColumnData::Int32(&ints), None).expect("int column should validate")];

        let batch = ColumnBatch::from_columns(columns).expect("column batch should validate");

        assert_eq!(batch.selection(), None);
        assert_eq!(batch.selected_rows(), 3);
    }

    #[test]
    fn column_batch_rejects_mismatched_selection_mask() {
        let ints = [1_i32, 2, 3];
        let selection = [1_u8, 0];
        let columns =
            vec![Column::new(ColumnData::Int32(&ints), None).expect("int column should validate")];

        assert_eq!(
            err_status(ColumnBatch::from_columns_with_selection(
                columns,
                Some(&selection)
            )),
            GpuStatusDetail::ShapeMismatch
        );
    }

    #[test]
    fn column_batch_rejects_non_binary_selection_mask() {
        let ints = [1_i32, 2, 3];
        let selection = [1_u8, 2, 0];
        let columns =
            vec![Column::new(ColumnData::Int32(&ints), None).expect("int column should validate")];

        assert_eq!(
            err_status(ColumnBatch::from_columns_with_selection(
                columns,
                Some(&selection)
            )),
            GpuStatusDetail::InvalidDescriptor
        );
    }

    #[test]
    fn column_batch_rejects_mismatched_column_lengths() {
        let ints = [1_i32, 2, 3];
        let floats = [1.0_f64, 2.0];
        let columns = vec![
            Column::new(ColumnData::Int32(&ints), None).expect("int column should validate"),
            Column::new(ColumnData::Float64(&floats), None).expect("float column should validate"),
        ];

        assert_eq!(
            err_status(ColumnBatch::try_new(3, columns)),
            GpuStatusDetail::ShapeMismatch
        );
    }

    #[test]
    fn column_rejects_bad_null_mask() {
        let values = [1_i64, 2, 3];
        let nulls = [0_u8, 1];

        assert_eq!(
            err_status(Column::new(ColumnData::Int64(&values), Some(&nulls))),
            GpuStatusDetail::ShapeMismatch
        );
    }
}
