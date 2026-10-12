use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;

use super::{
    GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail, PgaccelExprUsmCol,
    PgaccelVal, PgaccelValTag, bridge, status_to_result,
};

fn checked_allocation_bytes<T>(len: usize) -> Option<usize> {
    len.checked_mul(std::mem::size_of::<T>())
        .filter(|bytes| *bytes != 0)
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

/// Query-owned exact SQL mask for a resident Boolean column. The charge is
/// acquired before allocation and released after the device buffer. Malformed
/// input bytes remain marker 2, which the grouped SQL-mask consumer rejects.
pub struct SqlBoolMask {
    buffer: ExprDeviceBuffer<i8>,
    _charge: crate::engine::residency::ExecutionMemoryCharge,
}

pub const SQL_BOOL_MASK_CHUNK_ROWS: usize = 65_536;

/// Populate an already allocated/charged device mask. Cached artifact builders
/// reserve its bytes with the whole artifact before allocating this buffer;
/// query-local callers use SqlBoolMask instead. This function allocates nothing.
///
/// Read-only polling between synchronous calls. `false` means interrupted:
/// output may be partial and must be discarded, never published. The callback
/// must not invoke PostgreSQL or re-enter residency while inputs are borrowed.
pub fn write_sql_bool_mask_with_poll(
    values: &ExprDeviceBuffer<u8>,
    nulls: Option<&ExprDeviceBuffer<u8>>,
    output: &ExprDeviceBuffer<i8>,
    pending: impl FnMut() -> bool,
) -> GpuResult<bool> {
    let rows = values.len();
    if output.len() != rows || nulls.is_some_and(|nulls| nulls.len() != rows) {
        return Err(GpuError::with_detail(
            GpuErrorDomain::Expression,
            GpuOperation::ValidateDeviceOutput,
            GpuStatusDetail::ShapeMismatch,
            "SQL Boolean mask input/output/NULL lengths differ",
        ));
    }
    sql_bool_mask_chunks(rows, pending, |first, count| {
        // SAFETY: typed buffers span rows and each chunk is in bounds. Native
        // validation checks alignment and aliasing before writes. The call
        // waits before returning; all owners stay live throughout.
        let status = unsafe {
            bridge::pgaccel_bool_sql_mask(
                values.as_ptr().add(first),
                nulls.map_or(std::ptr::null(), |nulls| nulls.as_ptr().add(first)),
                count,
                output.as_mut_ptr().add(first),
            )
        };
        status_to_result(
            status,
            GpuErrorDomain::Expression,
            GpuOperation::Kernel("bool_sql_mask"),
        )
    })
}

fn sql_bool_mask_chunks<E>(
    rows: usize,
    mut pending: impl FnMut() -> bool,
    mut launch: impl FnMut(usize, usize) -> Result<(), E>,
) -> Result<bool, E> {
    // Chunk boundaries are multiples of four for the native word-owner
    // guarantee. Only read-only signal polling occurs under the input borrow.
    for first in (0..rows).step_by(SQL_BOOL_MASK_CHUNK_ROWS) {
        if pending() {
            return Ok(false);
        }
        launch(first, (rows - first).min(SQL_BOOL_MASK_CHUNK_ROWS))?;
    }
    Ok(!pending())
}

impl SqlBoolMask {
    /// None means a signal was observed; partial buffers and their charge have
    /// already dropped. The caller must deliver interrupts outside input borrows.
    pub fn from_resident_with_poll(
        values: &ExprDeviceBuffer<u8>,
        nulls: Option<&ExprDeviceBuffer<u8>>,
        mut pending: impl FnMut() -> bool,
    ) -> GpuResult<Option<Self>> {
        let rows = values.len();
        if nulls.is_some_and(|nulls| nulls.len() != rows) {
            return Err(GpuError::with_detail(
                GpuErrorDomain::Expression,
                GpuOperation::ValidateDeviceOutput,
                GpuStatusDetail::ShapeMismatch,
                "SQL Boolean mask input/NULL lengths differ",
            ));
        }
        if pending() {
            return Ok(None);
        }
        let charge = crate::engine::residency::ExecutionMemoryCharge::reserve(
            rows as u64,
            crate::engine::gucs::resident_memory_budget_bytes(),
        )
        .map_err(|_| {
            GpuError::with_detail(
                GpuErrorDomain::Memory,
                GpuOperation::BuildColumnBatch,
                GpuStatusDetail::OutOfMemory,
                "SQL Boolean mask exceeds the resident execution budget",
            )
        })?;
        let buffer = ExprDeviceBuffer::<i8>::new(rows).ok_or_else(|| {
            GpuError::with_detail(
                GpuErrorDomain::Memory,
                GpuOperation::BuildColumnBatch,
                GpuStatusDetail::OutOfMemory,
                "could not allocate SQL Boolean mask",
            )
        })?;
        if !write_sql_bool_mask_with_poll(values, nulls, &buffer, pending)? {
            return Ok(None);
        }
        Ok(Some(Self {
            buffer,
            _charge: charge,
        }))
    }

    #[must_use]
    pub fn as_ptr(&self) -> *const i8 {
        self.buffer.as_ptr()
    }
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
        self.copy_prefix_to_slice(output)
    }

    /// Copy an initialized prefix into preallocated host memory. An empty
    /// prefix is a no-op; the caller still accounts for all retained capacity.
    /// As with `copy_to_slice`, T must be a plain-data ABI type.
    pub fn copy_prefix_to_slice(&self, output: &mut [T]) -> GpuResult<()>
    where
        T: Copy,
    {
        if output.len() > self.len {
            return Err(GpuError::with_detail(
                GpuErrorDomain::Memory,
                GpuOperation::ValidateDeviceOutput,
                GpuStatusDetail::ShapeMismatch,
                "device-to-host prefix exceeds device buffer",
            ));
        }
        if output.is_empty() {
            return Ok(());
        }
        let bytes = checked_allocation_bytes::<T>(output.len()).ok_or_else(|| {
            GpuError::with_detail(
                GpuErrorDomain::Memory,
                GpuOperation::ValidateDeviceOutput,
                GpuStatusDetail::CapacityOverflow,
                "device-to-host copy size overflow",
            )
        })?;
        // SAFETY: output is valid for `bytes`, and the prefix-length check
        // bounds the copy within the live device allocation owned by self.
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
        self.write_prefix_from_slice(input)
    }

    /// Overwrite a prefix without modifying the rest of this allocation.
    /// An empty prefix is a no-op; this does not resize the owned buffer.
    pub fn write_prefix_from_slice(&self, input: &[T]) -> GpuResult<()>
    where
        T: Copy,
    {
        if input.len() > self.len {
            return Err(GpuError::with_detail(
                GpuErrorDomain::Memory,
                GpuOperation::ValidateDeviceInput,
                GpuStatusDetail::ShapeMismatch,
                "host-to-device prefix exceeds device buffer",
            ));
        }
        if input.is_empty() {
            return Ok(());
        }
        let bytes = checked_allocation_bytes::<T>(input.len()).ok_or_else(|| {
            GpuError::with_detail(
                GpuErrorDomain::Memory,
                GpuOperation::ValidateDeviceInput,
                GpuStatusDetail::CapacityOverflow,
                "host-to-device copy size overflow",
            )
        })?;
        // SAFETY: input is valid for `bytes`, and the prefix-length check
        // bounds the copy within the live device allocation owned by self.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_bool_mask_rejects_bad_lengths_and_budget_before_device_access() {
        use crate::engine::residency::{ExecutionMemoryCharge, resident_live_bytes};
        // Deliberately not device allocations. These failures must be checked
        // before touching either pointer, and ManuallyDrop avoids native free.
        let values = std::mem::ManuallyDrop::new(ExprDeviceBuffer::<u8> {
            ptr: NonNull::dangling(),
            len: 3,
            _not_send_sync: PhantomData,
        });
        let nulls = std::mem::ManuallyDrop::new(ExprDeviceBuffer::<u8> {
            ptr: NonNull::dangling(),
            len: 2,
            _not_send_sync: PhantomData,
        });
        let baseline = resident_live_bytes();
        let error = SqlBoolMask::from_resident_with_poll(&values, Some(&nulls), || false)
            .err()
            .expect("shape rejection");
        assert_eq!(error.status, GpuStatusDetail::ShapeMismatch);
        assert_eq!(resident_live_bytes(), baseline);
        let budget = crate::engine::gucs::resident_memory_budget_bytes();
        let held =
            ExecutionMemoryCharge::reserve(budget - baseline - 2, budget).expect("leave two bytes");
        let before = resident_live_bytes();
        let error = SqlBoolMask::from_resident_with_poll(&values, None, || false)
            .err()
            .expect("three-byte mask cannot fit");
        assert_eq!(error.status, GpuStatusDetail::OutOfMemory);
        assert_eq!(
            resident_live_bytes(),
            before,
            "failed reservation must not leak"
        );
        drop(held);
        assert_eq!(resident_live_bytes(), baseline);
    }

    #[test]
    fn preallocated_sql_mask_validates_all_lengths_without_ffi_or_charges() {
        let values = std::mem::ManuallyDrop::new(ExprDeviceBuffer::<u8> {
            ptr: NonNull::dangling(),
            len: 3,
            _not_send_sync: PhantomData,
        });
        let nulls = std::mem::ManuallyDrop::new(ExprDeviceBuffer::<u8> {
            ptr: NonNull::dangling(),
            len: 2,
            _not_send_sync: PhantomData,
        });
        for rows in [0, 2, 3, 4] {
            let output = std::mem::ManuallyDrop::new(ExprDeviceBuffer::<i8> {
                ptr: NonNull::dangling(),
                len: rows,
                _not_send_sync: PhantomData,
            });
            let before = crate::engine::residency::resident_live_bytes();
            assert_eq!(
                write_sql_bool_mask_with_poll(&values, Some(&nulls), &output, || false)
                    .expect_err("bad input/output lengths")
                    .status,
                GpuStatusDetail::ShapeMismatch
            );
            if rows != 3 {
                assert_eq!(
                    write_sql_bool_mask_with_poll(&values, None, &output, || false)
                        .expect_err("bad output length")
                        .status,
                    GpuStatusDetail::ShapeMismatch
                );
            }
            assert_eq!(crate::engine::residency::resident_live_bytes(), before);
        }
    }

    #[test]
    fn query_local_sql_mask_interrupt_precedes_reservation_and_ffi() {
        let values = std::mem::ManuallyDrop::new(ExprDeviceBuffer::<u8> {
            ptr: NonNull::dangling(),
            len: 65_537,
            _not_send_sync: PhantomData,
        });
        let before = crate::engine::residency::resident_live_bytes();
        assert!(
            SqlBoolMask::from_resident_with_poll(&values, None, || true)
                .expect("signal stops before allocation")
                .is_none()
        );
        assert_eq!(crate::engine::residency::resident_live_bytes(), before);
        let nulls = std::mem::ManuallyDrop::new(ExprDeviceBuffer::<u8> {
            ptr: NonNull::dangling(),
            len: 1,
            _not_send_sync: PhantomData,
        });
        let error = SqlBoolMask::from_resident_with_poll(&values, Some(&nulls), || {
            panic!("input shape must validate before polling")
        })
        .err()
        .expect("bad shape is not a completed mask or a signal");
        assert_eq!(error.status, GpuStatusDetail::ShapeMismatch);
        assert_eq!(crate::engine::residency::resident_live_bytes(), before);
    }

    #[test]
    fn sql_mask_polling_covers_empty_initial_middle_and_final_boundaries() {
        for rows in [0_usize, 1, 65_536, 65_537, 1_000_017] {
            let chunks = rows.div_ceil(SQL_BOOL_MASK_CHUNK_ROWS);
            for stop in 0..=chunks + 1 {
                let mut polls = 0;
                let mut ranges = Vec::new();
                let complete = sql_bool_mask_chunks(
                    rows,
                    || {
                        let pending = polls == stop;
                        polls += 1;
                        pending
                    },
                    |first, count| {
                        ranges.push((first, count));
                        Ok::<_, ()>(())
                    },
                )
                .expect("launches succeed");
                assert_eq!(complete, stop > chunks);
                assert_eq!(polls, (stop + 1).min(chunks + 1));
                assert_eq!(ranges.len(), stop.min(chunks));
                for (index, (first, count)) in ranges.into_iter().enumerate() {
                    assert_eq!(first, index * SQL_BOOL_MASK_CHUNK_ROWS);
                    assert_eq!(first % 4, 0);
                    assert_eq!(count, (rows - first).min(SQL_BOOL_MASK_CHUNK_ROWS));
                }
            }
        }
    }

    #[test]
    fn sql_mask_launch_failure_stops_without_rewriting_error() {
        let mut launches = 0;
        let result = sql_bool_mask_chunks(
            3 * SQL_BOOL_MASK_CHUNK_ROWS,
            || false,
            |_, _| {
                launches += 1;
                if launches == 2 {
                    Err("device error")
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(result, Err("device error"));
        assert_eq!(launches, 2);
    }

    #[test]
    fn sql_mask_prelaunch_interrupt_never_touches_device_buffers() {
        let values = std::mem::ManuallyDrop::new(ExprDeviceBuffer::<u8> {
            ptr: NonNull::dangling(),
            len: 65_537,
            _not_send_sync: PhantomData,
        });
        let output = std::mem::ManuallyDrop::new(ExprDeviceBuffer::<i8> {
            ptr: NonNull::dangling(),
            len: values.len(),
            _not_send_sync: PhantomData,
        });
        let before = crate::engine::residency::resident_live_bytes();
        assert!(
            !write_sql_bool_mask_with_poll(&values, None, &output, || true)
                .expect("interrupt before any FFI")
        );
        assert_eq!(crate::engine::residency::resident_live_bytes(), before);
    }

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

    #[test]
    fn prefix_copies_bound_capacity_and_skip_empty_transfers_before_ffi() {
        let buffer = std::mem::ManuallyDrop::new(ExprDeviceBuffer::<u64> {
            ptr: NonNull::dangling(),
            len: 2,
            _not_send_sync: PhantomData,
        });
        assert!(buffer.copy_prefix_to_slice(&mut []).is_ok());
        assert!(buffer.write_prefix_from_slice(&[]).is_ok());
        for error in [
            buffer
                .copy_prefix_to_slice(&mut [0; 3])
                .expect_err("oversized download must fail before FFI"),
            buffer
                .write_prefix_from_slice(&[0; 3])
                .expect_err("oversized upload must fail before FFI"),
        ] {
            assert_eq!(error.domain, GpuErrorDomain::Memory);
            assert_eq!(error.status, GpuStatusDetail::ShapeMismatch);
        }
        assert_eq!(buffer.len(), 2);
    }
}
