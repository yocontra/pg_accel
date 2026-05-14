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
