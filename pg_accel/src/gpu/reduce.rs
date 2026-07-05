use super::{PgaccelReduceCol, bridge};

// ---------------------------------------------------------------------------
// Reduce wrappers
// ---------------------------------------------------------------------------

/// GPU-accelerated f32 sum reduction. Returns `None` if GPU unavailable.
pub fn reduce_sum_f32(data: &[f32]) -> Option<f32> {
    let _span = tracing::debug_span!("gpu.reduce_sum_f32", n = data.len()).entered();
    let mut result: f32 = 0.0;
    // SAFETY: data is a valid slice, result is a valid pointer.
    let status =
        unsafe { bridge::pgaccel_reduce_sum_f32(data.as_ptr(), data.len(), &raw mut result) };
    status.is_ok().then_some(result)
}

/// GPU-accelerated f32 min reduction. Returns `None` if GPU unavailable.
pub fn reduce_min_f32(data: &[f32]) -> Option<f32> {
    let _span = tracing::debug_span!("gpu.reduce_min_f32", n = data.len()).entered();
    let mut result: f32 = 0.0;
    // SAFETY: data is a valid slice, result is a valid pointer.
    let status =
        unsafe { bridge::pgaccel_reduce_min_f32(data.as_ptr(), data.len(), &raw mut result) };
    status.is_ok().then_some(result)
}

/// GPU-accelerated f32 max reduction. Returns `None` if GPU unavailable.
pub fn reduce_max_f32(data: &[f32]) -> Option<f32> {
    let _span = tracing::debug_span!("gpu.reduce_max_f32", n = data.len()).entered();
    let mut result: f32 = 0.0;
    // SAFETY: data is a valid slice, result is a valid pointer.
    let status =
        unsafe { bridge::pgaccel_reduce_max_f32(data.as_ptr(), data.len(), &raw mut result) };
    status.is_ok().then_some(result)
}

/// GPU-accelerated f64 sum reduction.
pub fn reduce_sum_f64(data: &[f64]) -> Option<f64> {
    let _span = tracing::debug_span!("gpu.reduce_sum_f64", n = data.len()).entered();
    let mut result: f64 = 0.0;
    // SAFETY: data is a valid slice, result is a valid pointer.
    let status =
        unsafe { bridge::pgaccel_reduce_sum_f64(data.as_ptr(), data.len(), &raw mut result) };
    status.is_ok().then_some(result)
}

/// GPU-accelerated i64 sum reduction.
pub fn reduce_sum_i64(data: &[i64]) -> Option<i64> {
    let _span = tracing::debug_span!("gpu.reduce_sum_i64", n = data.len()).entered();
    let mut result: i64 = 0;
    // SAFETY: data is a valid slice, result is a valid pointer.
    let status =
        unsafe { bridge::pgaccel_reduce_sum_i64(data.as_ptr(), data.len(), &raw mut result) };
    status.is_ok().then_some(result)
}

/// GPU-accelerated i64 min reduction.
pub fn reduce_min_i64(data: &[i64]) -> Option<i64> {
    let _span = tracing::debug_span!("gpu.reduce_min_i64", n = data.len()).entered();
    let mut result: i64 = 0;
    // SAFETY: data is a valid slice, result is a valid pointer.
    let status =
        unsafe { bridge::pgaccel_reduce_min_i64(data.as_ptr(), data.len(), &raw mut result) };
    status.is_ok().then_some(result)
}

/// GPU-accelerated i64 max reduction.
pub fn reduce_max_i64(data: &[i64]) -> Option<i64> {
    let _span = tracing::debug_span!("gpu.reduce_max_i64", n = data.len()).entered();
    let mut result: i64 = 0;
    // SAFETY: data is a valid slice, result is a valid pointer.
    let status =
        unsafe { bridge::pgaccel_reduce_max_i64(data.as_ptr(), data.len(), &raw mut result) };
    status.is_ok().then_some(result)
}

/// GPU-accelerated f64 min reduction.
pub fn reduce_min_f64(data: &[f64]) -> Option<f64> {
    let _span = tracing::debug_span!("gpu.reduce_min_f64", n = data.len()).entered();
    let mut result: f64 = 0.0;
    // SAFETY: data is a valid slice, result is a valid pointer.
    let status =
        unsafe { bridge::pgaccel_reduce_min_f64(data.as_ptr(), data.len(), &raw mut result) };
    status.is_ok().then_some(result)
}

/// GPU-accelerated f64 max reduction.
pub fn reduce_max_f64(data: &[f64]) -> Option<f64> {
    let _span = tracing::debug_span!("gpu.reduce_max_f64", n = data.len()).entered();
    let mut result: f64 = 0.0;
    // SAFETY: data is a valid slice, result is a valid pointer.
    let status =
        unsafe { bridge::pgaccel_reduce_max_f64(data.as_ptr(), data.len(), &raw mut result) };
    status.is_ok().then_some(result)
}

// ---------------------------------------------------------------------------
// Fused multi-aggregate reduce
// ---------------------------------------------------------------------------

/// Result of a fused f32 multi-aggregate reduce.
#[derive(Debug, Clone, Copy)]
pub struct ReduceMultiF32 {
    pub sum: f32,
    pub min: f32,
    pub max: f32,
    pub count: i64,
}

/// Result of a fused f64 multi-aggregate reduce.
#[derive(Debug, Clone, Copy)]
pub struct ReduceMultiF64 {
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub count: i64,
}

/// Result of a fused i64 multi-aggregate reduce.
#[derive(Debug, Clone, Copy)]
pub struct ReduceMultiI64 {
    pub sum: i64,
    pub min: i64,
    pub max: i64,
    pub count: i64,
}

#[allow(dead_code)] // reason: shared validator for staged masked reduce wrappers; executor wiring pending
fn optional_mask_ptr(mask: Option<&[u8]>, expected_len: usize) -> Option<*const u8> {
    match mask {
        None => Some(std::ptr::null()),
        Some(mask) if mask.len() == expected_len && mask.iter().all(|&b| b <= 1) => {
            Some(mask.as_ptr())
        }
        Some(_) => None,
    }
}

/// GPU-accelerated fused f32 SUM+MIN+MAX+COUNT in a single pass.
#[must_use]
pub fn reduce_multi_f32(data: &[f32]) -> Option<ReduceMultiF32> {
    let _span = tracing::debug_span!("gpu.reduce_multi_f32", n = data.len()).entered();
    if data.is_empty() {
        return Some(ReduceMultiF32 {
            sum: 0.0,
            min: 0.0,
            max: 0.0,
            count: 0,
        });
    }
    let mut out_sum = 0.0f32;
    let mut out_min = 0.0f32;
    let mut out_max = 0.0f32;
    let mut out_count: i64 = 0;
    // SAFETY: data is a valid slice; out_* are valid pointers.
    let status = unsafe {
        bridge::pgaccel_reduce_multi_f32(
            data.as_ptr(),
            data.len(),
            &raw mut out_sum,
            &raw mut out_min,
            &raw mut out_max,
            &raw mut out_count,
        )
    };
    status.is_ok().then_some(ReduceMultiF32 {
        sum: out_sum,
        min: out_min,
        max: out_max,
        count: out_count,
    })
}

/// GPU-accelerated masked f32 SUM+MIN+MAX+COUNT in a single pass.
///
/// `selection` uses `1 = selected`, `0 = filtered`; `value_nulls` uses
/// `1 = NULL`, `0 = non-NULL`. `None` for either mask means no rows are
/// filtered by that mask.
#[must_use]
#[allow(dead_code)] // reason: masked reduce wrapper staged for scan->batch filtered aggregate wiring
pub fn reduce_multi_masked_f32(
    data: &[f32],
    value_nulls: Option<&[u8]>,
    selection: Option<&[u8]>,
) -> Option<ReduceMultiF32> {
    let _span = tracing::debug_span!("gpu.reduce_multi_masked_f32", n = data.len()).entered();
    let value_nulls_ptr = optional_mask_ptr(value_nulls, data.len())?;
    let selection_ptr = optional_mask_ptr(selection, data.len())?;
    if data.is_empty() {
        return Some(ReduceMultiF32 {
            sum: 0.0,
            min: 0.0,
            max: 0.0,
            count: 0,
        });
    }

    let mut out_sum = 0.0f32;
    let mut out_min = 0.0f32;
    let mut out_max = 0.0f32;
    let mut out_count: i64 = 0;
    // SAFETY: data is a valid slice; optional masks are null or validated to
    // match data.len(); out_* are valid pointers.
    let status = unsafe {
        bridge::pgaccel_reduce_multi_masked_f32(
            data.as_ptr(),
            value_nulls_ptr,
            selection_ptr,
            data.len(),
            &raw mut out_sum,
            &raw mut out_min,
            &raw mut out_max,
            &raw mut out_count,
        )
    };
    status.is_ok().then_some(ReduceMultiF32 {
        sum: out_sum,
        min: out_min,
        max: out_max,
        count: out_count,
    })
}

/// GPU-accelerated fused f64 SUM+MIN+MAX+COUNT in a single pass.
#[must_use]
pub fn reduce_multi_f64(data: &[f64]) -> Option<ReduceMultiF64> {
    let _span = tracing::debug_span!("gpu.reduce_multi_f64", n = data.len()).entered();
    if data.is_empty() {
        return Some(ReduceMultiF64 {
            sum: 0.0,
            min: 0.0,
            max: 0.0,
            count: 0,
        });
    }
    let mut out_sum = 0.0f64;
    let mut out_min = 0.0f64;
    let mut out_max = 0.0f64;
    let mut out_count: i64 = 0;
    // SAFETY: data is a valid slice; out_* are valid pointers.
    let status = unsafe {
        bridge::pgaccel_reduce_multi_f64(
            data.as_ptr(),
            data.len(),
            &raw mut out_sum,
            &raw mut out_min,
            &raw mut out_max,
            &raw mut out_count,
        )
    };
    status.is_ok().then_some(ReduceMultiF64 {
        sum: out_sum,
        min: out_min,
        max: out_max,
        count: out_count,
    })
}

/// GPU-accelerated masked f64 SUM+MIN+MAX+COUNT in a single pass.
///
/// On Metal, the current C++ implementation returns `None` because the
/// struct-valued soft-fp64 masked path is intentionally fail-closed.
#[must_use]
#[allow(dead_code)] // reason: masked reduce wrapper staged for scan->batch filtered aggregate wiring
pub fn reduce_multi_masked_f64(
    data: &[f64],
    value_nulls: Option<&[u8]>,
    selection: Option<&[u8]>,
) -> Option<ReduceMultiF64> {
    let _span = tracing::debug_span!("gpu.reduce_multi_masked_f64", n = data.len()).entered();
    let value_nulls_ptr = optional_mask_ptr(value_nulls, data.len())?;
    let selection_ptr = optional_mask_ptr(selection, data.len())?;
    if data.is_empty() {
        return Some(ReduceMultiF64 {
            sum: 0.0,
            min: 0.0,
            max: 0.0,
            count: 0,
        });
    }

    let mut out_sum = 0.0f64;
    let mut out_min = 0.0f64;
    let mut out_max = 0.0f64;
    let mut out_count: i64 = 0;
    // SAFETY: data is a valid slice; optional masks are null or validated to
    // match data.len(); out_* are valid pointers.
    let status = unsafe {
        bridge::pgaccel_reduce_multi_masked_f64(
            data.as_ptr(),
            value_nulls_ptr,
            selection_ptr,
            data.len(),
            &raw mut out_sum,
            &raw mut out_min,
            &raw mut out_max,
            &raw mut out_count,
        )
    };
    status.is_ok().then_some(ReduceMultiF64 {
        sum: out_sum,
        min: out_min,
        max: out_max,
        count: out_count,
    })
}

/// GPU-accelerated fused i64 SUM+MIN+MAX+COUNT in a single pass.
#[must_use]
pub fn reduce_multi_i64(data: &[i64]) -> Option<ReduceMultiI64> {
    let _span = tracing::debug_span!("gpu.reduce_multi_i64", n = data.len()).entered();
    if data.is_empty() {
        return Some(ReduceMultiI64 {
            sum: 0,
            min: 0,
            max: 0,
            count: 0,
        });
    }
    let mut out_sum: i64 = 0;
    let mut out_min: i64 = 0;
    let mut out_max: i64 = 0;
    let mut out_count: i64 = 0;
    // SAFETY: data is a valid slice; out_* are valid pointers.
    let status = unsafe {
        bridge::pgaccel_reduce_multi_i64(
            data.as_ptr(),
            data.len(),
            &raw mut out_sum,
            &raw mut out_min,
            &raw mut out_max,
            &raw mut out_count,
        )
    };
    status.is_ok().then_some(ReduceMultiI64 {
        sum: out_sum,
        min: out_min,
        max: out_max,
        count: out_count,
    })
}

/// GPU-accelerated masked i64 SUM+MIN+MAX+COUNT in a single pass.
#[must_use]
#[allow(dead_code)] // reason: masked reduce wrapper staged for scan->batch filtered aggregate wiring
pub fn reduce_multi_masked_i64(
    data: &[i64],
    value_nulls: Option<&[u8]>,
    selection: Option<&[u8]>,
) -> Option<ReduceMultiI64> {
    let _span = tracing::debug_span!("gpu.reduce_multi_masked_i64", n = data.len()).entered();
    let value_nulls_ptr = optional_mask_ptr(value_nulls, data.len())?;
    let selection_ptr = optional_mask_ptr(selection, data.len())?;
    if data.is_empty() {
        return Some(ReduceMultiI64 {
            sum: 0,
            min: 0,
            max: 0,
            count: 0,
        });
    }

    let mut out_sum: i64 = 0;
    let mut out_min: i64 = 0;
    let mut out_max: i64 = 0;
    let mut out_count: i64 = 0;
    // SAFETY: data is a valid slice; optional masks are null or validated to
    // match data.len(); out_* are valid pointers.
    let status = unsafe {
        bridge::pgaccel_reduce_multi_masked_i64(
            data.as_ptr(),
            value_nulls_ptr,
            selection_ptr,
            data.len(),
            &raw mut out_sum,
            &raw mut out_min,
            &raw mut out_max,
            &raw mut out_count,
        )
    };
    status.is_ok().then_some(ReduceMultiI64 {
        sum: out_sum,
        min: out_min,
        max: out_max,
        count: out_count,
    })
}

/// GPU-accelerated mask popcount.
#[allow(dead_code)] // reason: mask popcount wrapper; future use for COUNT(*) FILTER (...) bitmap
pub fn reduce_count(mask: &[u8]) -> Option<usize> {
    let _span = tracing::debug_span!("gpu.reduce_count", n = mask.len()).entered();
    let mut result: usize = 0;
    // SAFETY: mask is a valid slice, result is a valid pointer.
    let status =
        unsafe { bridge::pgaccel_reduce_count(mask.as_ptr(), mask.len(), &raw mut result) };
    status.is_ok().then_some(result)
}

// ---------------------------------------------------------------------------
// sum_sq and fused stats (count, sum, sum_sq) — partial-agg AVG/STDDEV/VARIANCE
// ---------------------------------------------------------------------------
//
// sum_sq accumulates Σ(x²) in double regardless of input element type so a
// large fp32 buffer still returns a numerically useful aggregate.
//
// stats fuses count + sum + sum_sq into one kernel launch over one buffer
// so partial-agg STDDEV/VARIANCE doesn't pay three reduce passes.
//
// fp64 dispatch is unconditional: on devices without native fp64 (Metal),
// the AdaptiveCpp soft-fp64 libkernel provides correct IEEE-754 double
// arithmetic transparently inside the kernel. `has_native_fp64` is now a
// cost signal only (see `PgaccelPlatformCaps::has_native_fp64`).

/// GPU-accelerated Σ(x²) reduction with f32 input and f64 accumulator.
/// Returns `None` if GPU is unavailable.
#[must_use]
#[allow(dead_code)] // reason: f32 sum_sq wrapper; executor uses reduce_stats_f32 fused path
pub fn reduce_sum_sq_f32(data: &[f32]) -> Option<f64> {
    let _span = tracing::trace_span!("gpu.reduce_sum_sq_f32", n = data.len()).entered();
    let mut result: f64 = 0.0;
    // SAFETY: data is a valid slice, result is a valid pointer.
    let status =
        unsafe { bridge::pgaccel_reduce_sum_sq_f32(data.as_ptr(), data.len(), &raw mut result) };
    status.is_ok().then_some(result)
}

/// GPU-accelerated Σ(x²) reduction with f64 input and f64 accumulator.
#[must_use]
#[allow(dead_code)] // reason: f64 sum_sq wrapper; executor uses reduce_stats_f64 fused path
pub fn reduce_sum_sq_f64(data: &[f64]) -> Option<f64> {
    let _span = tracing::trace_span!("gpu.reduce_sum_sq_f64", n = data.len()).entered();
    let mut result: f64 = 0.0;
    // SAFETY: data is a valid slice, result is a valid pointer.
    let status =
        unsafe { bridge::pgaccel_reduce_sum_sq_f64(data.as_ptr(), data.len(), &raw mut result) };
    status.is_ok().then_some(result)
}

/// GPU-accelerated fused (count, sum, sum_sq) reduction — single pass.
/// Input is f32; accumulator is f64 inside the kernel.
/// Returns `Some((count, sum, sum_sq))` or `None` on GPU failure.
#[must_use]
#[allow(dead_code)] // reason: f32 stats wrapper; executor uses partial_emitters f64 path for STDDEV/VAR
pub fn reduce_stats_f32(data: &[f32]) -> Option<(u64, f64, f64)> {
    let _span = tracing::trace_span!("gpu.reduce_stats_f32", n = data.len()).entered();
    if data.is_empty() {
        return Some((0, 0.0, 0.0));
    }
    let mut out_count: u64 = 0;
    let mut out_sum: f64 = 0.0;
    let mut out_sum_sq: f64 = 0.0;
    // SAFETY: data is a valid slice; out_* are valid pointers.
    let status = unsafe {
        bridge::pgaccel_reduce_stats_f32(
            data.as_ptr(),
            data.len(),
            &raw mut out_count,
            &raw mut out_sum,
            &raw mut out_sum_sq,
        )
    };
    status.is_ok().then_some((out_count, out_sum, out_sum_sq))
}

// ---------------------------------------------------------------------------
// Boolean and bitwise reductions (Phase 4)
// ---------------------------------------------------------------------------
//
// Each kernel returns `None` when the GPU dispatch fails (the executor must
// raise a PG ERROR — no CPU fallback). NULL inputs are filtered out by the
// caller before the slice reaches the GPU; the empty-input → SQL NULL case
// is materialised by the executor by checking `has_value` before emitting
// the result datum (we still return the kernel identity here so callers
// pivoting to fold-into-running-state semantics see a sane value).

/// GPU-accelerated logical AND over a `bool` column (encoded as 0/1 bytes).
///
/// `data` MUST contain only 0 or 1 bytes. NULL inputs have already been
/// filtered by the caller; the kernel does not know about NULLs.
#[must_use]
pub fn reduce_bool_and(data: &[u8]) -> Option<bool> {
    let _span = tracing::debug_span!("gpu.reduce_bool_and", n = data.len()).entered();
    let mut result: u8 = 1;
    // SAFETY: data is a valid slice, result is a valid pointer.
    let status =
        unsafe { bridge::pgaccel_reduce_bool_and(data.as_ptr(), data.len(), &raw mut result) };
    status.is_ok().then_some(result != 0)
}

/// GPU-accelerated logical OR over a `bool` column (encoded as 0/1 bytes).
#[must_use]
pub fn reduce_bool_or(data: &[u8]) -> Option<bool> {
    let _span = tracing::debug_span!("gpu.reduce_bool_or", n = data.len()).entered();
    let mut result: u8 = 0;
    // SAFETY: data is a valid slice, result is a valid pointer.
    let status =
        unsafe { bridge::pgaccel_reduce_bool_or(data.as_ptr(), data.len(), &raw mut result) };
    status.is_ok().then_some(result != 0)
}

macro_rules! impl_bit_reduction {
    ($name:ident, $bridge_fn:ident, $t:ty) => {
        #[must_use]
        pub fn $name(data: &[$t]) -> Option<$t> {
            let _span =
                tracing::debug_span!(concat!("gpu.", stringify!($name)), n = data.len()).entered();
            let mut result: $t = 0;
            // SAFETY: data is a valid slice, result is a valid pointer; the
            // FFI function reads count elements through data.
            let status = unsafe { bridge::$bridge_fn(data.as_ptr(), data.len(), &raw mut result) };
            status.is_ok().then_some(result)
        }
    };
}

impl_bit_reduction!(reduce_bit_and_i16, pgaccel_reduce_bit_and_i16, i16);
impl_bit_reduction!(reduce_bit_and_i32, pgaccel_reduce_bit_and_i32, i32);
impl_bit_reduction!(reduce_bit_and_i64, pgaccel_reduce_bit_and_i64, i64);
impl_bit_reduction!(reduce_bit_or_i16, pgaccel_reduce_bit_or_i16, i16);
impl_bit_reduction!(reduce_bit_or_i32, pgaccel_reduce_bit_or_i32, i32);
impl_bit_reduction!(reduce_bit_or_i64, pgaccel_reduce_bit_or_i64, i64);
impl_bit_reduction!(reduce_bit_xor_i16, pgaccel_reduce_bit_xor_i16, i16);
impl_bit_reduction!(reduce_bit_xor_i32, pgaccel_reduce_bit_xor_i32, i32);
impl_bit_reduction!(reduce_bit_xor_i64, pgaccel_reduce_bit_xor_i64, i64);

/// GPU-accelerated fused (count, sum, sum_sq) reduction — single pass.
/// Input is f64.
#[must_use]
pub fn reduce_stats_f64(data: &[f64]) -> Option<(u64, f64, f64)> {
    let _span = tracing::trace_span!("gpu.reduce_stats_f64", n = data.len()).entered();
    if data.is_empty() {
        return Some((0, 0.0, 0.0));
    }
    let mut out_count: u64 = 0;
    let mut out_sum: f64 = 0.0;
    let mut out_sum_sq: f64 = 0.0;
    // SAFETY: data is a valid slice; out_* are valid pointers.
    let status = unsafe {
        bridge::pgaccel_reduce_stats_f64(
            data.as_ptr(),
            data.len(),
            &raw mut out_count,
            &raw mut out_sum,
            &raw mut out_sum_sq,
        )
    };
    status.is_ok().then_some((out_count, out_sum, out_sum_sq))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn optional_mask_ptr_accepts_none_as_null() {
        assert_eq!(optional_mask_ptr(None, 3), Some(std::ptr::null()));
    }

    #[test]
    fn optional_mask_ptr_accepts_binary_matching_mask() {
        let mask = [1_u8, 0, 1];
        let ptr = optional_mask_ptr(Some(&mask), 3).expect("mask should validate");

        assert_eq!(ptr, mask.as_ptr());
    }

    #[test]
    fn optional_mask_ptr_rejects_mismatched_mask() {
        let mask = [1_u8, 0];

        assert_eq!(optional_mask_ptr(Some(&mask), 3), None);
    }

    #[test]
    fn optional_mask_ptr_rejects_non_binary_mask() {
        let mask = [1_u8, 2, 0];

        assert_eq!(optional_mask_ptr(Some(&mask), 3), None);
    }

    #[test]
    fn masked_multi_empty_inputs_return_zero_identity_without_dispatch() {
        assert_eq!(reduce_multi_masked_f32(&[], None, None).unwrap().count, 0);
        assert_eq!(reduce_multi_masked_f64(&[], None, None).unwrap().count, 0);
        assert_eq!(reduce_multi_masked_i64(&[], None, None).unwrap().count, 0);
    }

    #[test]
    fn masked_multi_empty_inputs_still_validate_masks() {
        let mask = [1_u8];

        assert!(reduce_multi_masked_i64(&[], Some(&mask), None).is_none());
        assert!(reduce_multi_masked_i64(&[], None, Some(&mask)).is_none());
    }

    #[test]
    fn masked_multi_f32_applies_selection_and_nulls_when_gpu_available() {
        crate::gpu::ensure_init();

        let data = [5.0_f32, -2.0, 7.0, 10.0, -4.0];
        let selection = [1_u8, 0, 1, 0, 1];
        let value_nulls = [0_u8, 0, 0, 0, 1];
        let Some(result) = reduce_multi_masked_f32(&data, Some(&value_nulls), Some(&selection))
        else {
            eprintln!("skipping masked f32 reduce wrapper test: GPU unavailable");
            return;
        };

        assert_eq!(result.count, 2);
        assert!((result.sum - 12.0).abs() <= f32::EPSILON);
        assert_eq!(result.min, 5.0);
        assert_eq!(result.max, 7.0);
    }

    #[test]
    fn masked_multi_i64_applies_selection_and_nulls_when_gpu_available() {
        crate::gpu::ensure_init();

        let data = [5_i64, -2, 7, 10, -4];
        let selection = [1_u8, 0, 1, 0, 1];
        let value_nulls = [0_u8, 0, 0, 0, 1];
        let Some(result) = reduce_multi_masked_i64(&data, Some(&value_nulls), Some(&selection))
        else {
            eprintln!("skipping masked i64 reduce wrapper test: GPU unavailable");
            return;
        };

        assert_eq!(result.count, 2);
        assert_eq!(result.sum, 12);
        assert_eq!(result.min, 5);
        assert_eq!(result.max, 7);
    }
}
