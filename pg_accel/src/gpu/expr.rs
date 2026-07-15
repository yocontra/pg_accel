use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;

use super::{
    GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail, PgaccelBatch,
    PgaccelExprProgram, PgaccelExprUsmCol, PgaccelVal, PgaccelValTag, bridge, status_to_result,
};

fn checked_allocation_bytes<T>(len: usize) -> Option<usize> {
    len.checked_mul(std::mem::size_of::<T>())
        .filter(|bytes| *bytes != 0)
}

/// Guard for batch-consuming wrappers whose output buffers are sized by a
/// caller-supplied `num_rows`: the C kernel writes `batch.num_rows` results
/// regardless, so any mismatch would let the kernel write past the end of
/// the Rust-owned output `Vec` (heap overflow). Mismatches are rejected
/// loudly instead of trusted.
fn batch_rows_checked(func: &'static str, batch: &PgaccelBatch, num_rows: usize) -> Option<usize> {
    if batch.num_rows == num_rows {
        return Some(num_rows);
    }
    super::counters::record_kernel_failure(super::counters::GpuFailureDomain::Expr);
    tracing::error!(
        target: "pg_accel::gpu",
        func,
        batch_num_rows = batch.num_rows,
        caller_num_rows = num_rows,
        "expression batch dispatch rejected: caller-supplied num_rows does not match \
         batch.num_rows (the kernel writes batch.num_rows results; a shorter output \
         buffer would be a heap overflow)"
    );
    None
}

/// GPU-readable buffer for resident cached columns.
///
/// Host-built resident columns may be backed by shared USM on unified-memory
/// backends, while scratch/output buffers remain device allocations. The
/// pointer is intentionally not exposed as a Rust slice because callers should
/// treat resident cache storage as GPU-owned after construction.
pub struct ExprDeviceBuffer<T> {
    ptr: NonNull<T>,
    len: usize,
    _not_send_sync: PhantomData<Rc<()>>,
}

impl<T> ExprDeviceBuffer<T> {
    /// Allocate uninitialized device memory.
    ///
    /// Returns `None` for zero-sized requests, size overflow, unavailable GPU,
    /// or allocation failure. The host must not dereference the returned
    /// pointer.
    pub fn new(len: usize) -> Option<Self> {
        let bytes = checked_allocation_bytes::<T>(len)?;
        crate::ensure_backend_exit_callback();
        let mut raw = std::ptr::null_mut::<c_void>();
        // SAFETY: `raw` is a valid out pointer. The C++ side initializes the
        // GPU queue and writes a device allocation pointer on success.
        let status = unsafe { bridge::pgaccel_expr_device_alloc(bytes, &raw mut raw) };
        if !status.is_ok() {
            return None;
        }
        let ptr = NonNull::new(raw.cast::<T>())?;
        crate::note_backend_gpu_owner_acquired();
        Some(Self {
            ptr,
            len,
            _not_send_sync: PhantomData,
        })
    }

    /// Allocate resident GPU-readable memory and copy `values` into it once.
    ///
    /// Returns `None` for empty input, size overflow, unavailable GPU,
    /// allocation failure, or device-copy failure.
    pub fn copy_from_slice(values: &[T]) -> Option<Self> {
        let bytes = checked_allocation_bytes::<T>(values.len())?;
        crate::ensure_backend_exit_callback();
        let mut raw = std::ptr::null_mut::<c_void>();
        // SAFETY: source slice is valid for `bytes` and `raw` is an out pointer.
        let status = unsafe {
            bridge::pgaccel_expr_device_alloc_copy(
                values.as_ptr().cast::<c_void>(),
                bytes,
                &raw mut raw,
            )
        };
        if !status.is_ok() {
            return None;
        }
        let ptr = NonNull::new(raw.cast::<T>())?;
        crate::note_backend_gpu_owner_acquired();
        Some(Self {
            ptr,
            len: values.len(),
            _not_send_sync: PhantomData,
        })
    }

    #[must_use]
    pub fn as_ptr(&self) -> *const T {
        self.ptr.as_ptr()
    }

    #[must_use]
    pub fn as_mut_ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Copy the complete device buffer into preallocated host memory.
    ///
    /// This path does not allocate. `output` must match the device buffer
    /// length exactly so accounting cannot hide unused host capacity. Callers
    /// must use plain-data ABI types whose device byte patterns are valid Rust
    /// values; this module has no broader device-copy marker trait.
    pub fn copy_to_slice(&self, output: &mut [T]) -> GpuResult<()>
    where
        T: Copy,
    {
        if output.len() != self.len {
            return Err(GpuError::with_detail(
                GpuErrorDomain::Memory,
                GpuOperation::ValidateDeviceOutput,
                GpuStatusDetail::ShapeMismatch,
                "device-to-host output length does not match device buffer",
            ));
        }
        let bytes = checked_allocation_bytes::<T>(self.len).ok_or_else(|| {
            GpuError::with_detail(
                GpuErrorDomain::Memory,
                GpuOperation::ValidateDeviceOutput,
                GpuStatusDetail::CapacityOverflow,
                "device-to-host copy size overflow",
            )
        })?;
        // SAFETY: the equal-length check makes output valid for `bytes`, and
        // self owns a live device allocation of the same byte length.
        let status = unsafe {
            bridge::pgaccel_expr_device_copy_to_host(
                output.as_mut_ptr().cast::<c_void>(),
                self.ptr.as_ptr().cast::<c_void>(),
                bytes,
            )
        };
        status_to_result(
            status,
            GpuErrorDomain::Memory,
            GpuOperation::Kernel("pgaccel_expr_device_copy_to_host"),
        )
    }

    /// Copy a complete caller-owned host slice into this device allocation.
    pub fn write_from_slice(&self, input: &[T]) -> GpuResult<()>
    where
        T: Copy,
    {
        if input.len() != self.len {
            return Err(GpuError::with_detail(
                GpuErrorDomain::Memory,
                GpuOperation::ValidateDeviceInput,
                GpuStatusDetail::ShapeMismatch,
                "host-to-device input length does not match device buffer",
            ));
        }
        let bytes = checked_allocation_bytes::<T>(self.len).ok_or_else(|| {
            GpuError::with_detail(
                GpuErrorDomain::Memory,
                GpuOperation::ValidateDeviceInput,
                GpuStatusDetail::CapacityOverflow,
                "host-to-device copy size overflow",
            )
        })?;
        // SAFETY: the equal-length check makes input valid for `bytes`, and
        // self owns a live device allocation of the same byte length.
        let status = unsafe {
            bridge::pgaccel_expr_device_copy_from_host(
                self.ptr.as_ptr().cast::<c_void>(),
                input.as_ptr().cast::<c_void>(),
                bytes,
            )
        };
        status_to_result(
            status,
            GpuErrorDomain::Memory,
            GpuOperation::Kernel("pgaccel_expr_device_copy_from_host"),
        )
    }

    /// Copy the complete device buffer into owned host memory.
    pub fn copy_to_vec(&self) -> GpuResult<Vec<T>>
    where
        T: Copy + Default,
    {
        let bytes = checked_allocation_bytes::<T>(self.len).ok_or_else(|| {
            GpuError::with_detail(
                GpuErrorDomain::Memory,
                GpuOperation::ValidateDeviceOutput,
                GpuStatusDetail::CapacityOverflow,
                "device-to-host copy size overflow",
            )
        })?;
        let mut output = Vec::new();
        output.try_reserve_exact(self.len).map_err(|_| {
            GpuError::with_detail(
                GpuErrorDomain::Memory,
                GpuOperation::BuildColumnBatch,
                GpuStatusDetail::OutOfMemory,
                "device-to-host output allocation failed",
            )
        })?;
        output.resize(self.len, T::default());
        debug_assert_eq!(bytes, output.len() * std::mem::size_of::<T>());
        self.copy_to_slice(&mut output)?;
        Ok(output)
    }
}

impl<T> Drop for ExprDeviceBuffer<T> {
    fn drop(&mut self) {
        // SAFETY: pointer was returned by a pgaccel_expr_device_alloc* call
        // and is freed exactly once by this owner.
        unsafe { bridge::pgaccel_expr_device_free(self.ptr.as_ptr().cast::<c_void>()) };
    }
}

#[must_use]
pub fn expr_device_col<T>(values: &ExprDeviceBuffer<T>, tag: PgaccelValTag) -> PgaccelExprUsmCol {
    PgaccelExprUsmCol {
        values: values.as_ptr().cast::<c_void>(),
        nulls: std::ptr::null(),
        tag,
    }
}

impl From<&ExprDeviceBuffer<i32>> for PgaccelExprUsmCol {
    fn from(values: &ExprDeviceBuffer<i32>) -> Self {
        expr_device_col(values, PgaccelValTag::Int32)
    }
}

// ---------------------------------------------------------------------------
// Expression evaluator wrappers
// ---------------------------------------------------------------------------

/// Evaluate a predicate expression on a columnar batch via GPU.
///
/// Returns a vector of three-result values per row:
/// +1 = TRUE, -1 = FALSE, 0 = UNCERTAIN. Selected pg_accel callers reject
/// uncertain rows rather than evaluating the predicate on CPU.
/// Returns `None` if the GPU is unavailable.
pub fn expr_eval_predicate(
    program: &PgaccelExprProgram,
    batch: &PgaccelBatch,
    num_rows: usize,
) -> Option<Vec<i8>> {
    let num_rows = batch_rows_checked("pgaccel_expr_eval_predicate", batch, num_rows)?;
    let mut results = vec![0i8; num_rows];

    // SAFETY: program and batch are valid references. results is pre-allocated
    // to batch.num_rows (validated above), which is exactly what the kernel writes.
    let status = unsafe {
        bridge::pgaccel_expr_eval_predicate(
            std::ptr::from_ref(program),
            std::ptr::from_ref(batch),
            results.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(results)
}

/// Evaluate a projection expression on a columnar batch via GPU.
///
/// Returns `(output_values, uncertain_mask)` or `None` if GPU unavailable.
#[allow(dead_code)] // reason: projection wrapper paired with predicate; executor only consumes predicate today
pub fn expr_eval_project(
    program: &PgaccelExprProgram,
    batch: &PgaccelBatch,
    num_rows: usize,
) -> Option<(Vec<PgaccelVal>, Vec<u8>)> {
    let num_rows = batch_rows_checked("pgaccel_expr_eval_project", batch, num_rows)?;
    let mut output = vec![PgaccelVal::null(); num_rows];
    let mut uncertain = vec![0u8; num_rows];

    // SAFETY: program and batch are valid references. output/uncertain are
    // pre-allocated to batch.num_rows (validated above).
    let status = unsafe {
        bridge::pgaccel_expr_eval_project(
            std::ptr::from_ref(program),
            std::ptr::from_ref(batch),
            output.as_mut_ptr(),
            uncertain.as_mut_ptr(),
        )
    };
    status.is_ok().then_some((output, uncertain))
}

/// Template: evaluate `col <cmp> const` on a batch.
///
/// Returns three-result vector or `None` if GPU unavailable.
pub fn expr_template_cmp_const(
    batch: &PgaccelBatch,
    col_idx: u32,
    cmp_opcode: u16,
    const_val: f64,
    num_rows: usize,
) -> Option<Vec<i8>> {
    let num_rows = batch_rows_checked("pgaccel_expr_template_cmp_const", batch, num_rows)?;
    let mut results = vec![0i8; num_rows];

    // SAFETY: batch is a valid reference. results is pre-allocated to
    // batch.num_rows (validated above).
    let status = unsafe {
        bridge::pgaccel_expr_template_cmp_const(
            std::ptr::from_ref(batch),
            col_idx,
            cmp_opcode,
            const_val,
            results.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(results)
}

/// Template: evaluate `col1 <cmp1> const1 AND col2 <cmp2> const2` on a
/// batch via the struct-packed kernel (single dispatch, no Rust-side AND
/// combiner). Three-valued: `+1=TRUE, -1=FALSE, 0=UNCERTAIN`.
#[allow(dead_code)] // reason: fused Metal launch aborts on macOS; executor uses two cmp_const launches
#[allow(clippy::too_many_arguments)]
pub fn expr_template_two_pred_and(
    batch: &PgaccelBatch,
    col1_idx: u32,
    cmp1_opcode: u16,
    const1_val: f64,
    col2_idx: u32,
    cmp2_opcode: u16,
    const2_val: f64,
    num_rows: usize,
) -> Option<Vec<i8>> {
    let num_rows = batch_rows_checked("pgaccel_expr_template_two_pred_and", batch, num_rows)?;
    let mut results = vec![0i8; num_rows];
    // SAFETY: batch is a valid reference; results is pre-allocated to
    // batch.num_rows (validated above), which is exactly what the kernel writes.
    let status = unsafe {
        bridge::pgaccel_expr_template_two_pred_and(
            std::ptr::from_ref(batch),
            col1_idx,
            cmp1_opcode,
            const1_val,
            col2_idx,
            cmp2_opcode,
            const2_val,
            results.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_allocation_bytes_rejects_zero_and_overflow() {
        assert_eq!(checked_allocation_bytes::<u8>(0), None);
        assert_eq!(
            checked_allocation_bytes::<u64>(usize::MAX / std::mem::size_of::<u64>() + 1),
            None
        );
        assert_eq!(checked_allocation_bytes::<u32>(4), Some(16));
    }

    #[test]
    fn preallocated_copies_reject_mismatched_lengths_before_ffi() {
        let buffer = std::mem::ManuallyDrop::new(ExprDeviceBuffer::<u8> {
            ptr: NonNull::dangling(),
            len: 2,
            _not_send_sync: PhantomData,
        });
        let error = buffer
            .copy_to_slice(&mut [0_u8; 1])
            .expect_err("short D2H destination must fail");
        assert_eq!(error.domain, GpuErrorDomain::Memory);
        assert_eq!(error.status, GpuStatusDetail::ShapeMismatch);
        let error = buffer
            .write_from_slice(&[0_u8; 1])
            .expect_err("short H2D source must fail");
        assert_eq!(error.domain, GpuErrorDomain::Memory);
        assert_eq!(error.status, GpuStatusDetail::ShapeMismatch);
    }
}
