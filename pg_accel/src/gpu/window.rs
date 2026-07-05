use super::bridge;

// ---------------------------------------------------------------------------
// Window function wrappers
// ---------------------------------------------------------------------------

/// GPU-accelerated ROW_NUMBER within partitions.
/// Returns `None` if GPU is unavailable.
pub fn window_row_number(partition_starts: &[u8], results: &mut [i64]) -> Option<()> {
    let _span = tracing::debug_span!("gpu.window_row_number", n = partition_starts.len()).entered();
    let count = partition_starts.len().min(results.len());
    if count == 0 {
        return Some(());
    }
    // SAFETY: slices are valid and count is within bounds.
    let status = unsafe {
        bridge::pgaccel_window_row_number(partition_starts.as_ptr(), count, results.as_mut_ptr())
    };
    status.is_ok().then_some(())
}

/// GPU-accelerated RANK within partitions.
/// Returns `None` if GPU is unavailable.
pub fn window_rank(partition_starts: &[u8], sort_keys: &[f64], results: &mut [i64]) -> Option<()> {
    let _span = tracing::debug_span!("gpu.window_rank", n = partition_starts.len()).entered();
    let count = partition_starts
        .len()
        .min(sort_keys.len())
        .min(results.len());
    if count == 0 {
        return Some(());
    }
    // SAFETY: slices are valid and count is within bounds.
    let status = unsafe {
        bridge::pgaccel_window_rank(
            partition_starts.as_ptr(),
            sort_keys.as_ptr(),
            count,
            results.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(())
}

/// GPU-accelerated DENSE_RANK within partitions.
/// Returns `None` if GPU is unavailable.
pub fn window_dense_rank(
    partition_starts: &[u8],
    sort_keys: &[f64],
    results: &mut [i64],
) -> Option<()> {
    let _span = tracing::debug_span!("gpu.window_dense_rank", n = partition_starts.len()).entered();
    let count = partition_starts
        .len()
        .min(sort_keys.len())
        .min(results.len());
    if count == 0 {
        return Some(());
    }
    // SAFETY: slices are valid and count is within bounds.
    let status = unsafe {
        bridge::pgaccel_window_dense_rank(
            partition_starts.as_ptr(),
            sort_keys.as_ptr(),
            count,
            results.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(())
}

/// GPU-accelerated running SUM within partitions.
/// `null_mask` may be empty (no nulls). Returns `None` if GPU unavailable.
pub fn window_sum(
    partition_starts: &[u8],
    values: &[f64],
    null_mask: &[u8],
    results: &mut [f64],
) -> Option<()> {
    let _span = tracing::debug_span!("gpu.window_sum", n = partition_starts.len()).entered();
    let count = partition_starts.len().min(values.len()).min(results.len());
    if count == 0 {
        return Some(());
    }
    let nm_ptr = if null_mask.is_empty() {
        std::ptr::null()
    } else {
        null_mask.as_ptr()
    };
    // SAFETY: slices are valid; nm_ptr is null or valid.
    let status = unsafe {
        bridge::pgaccel_window_sum(
            partition_starts.as_ptr(),
            values.as_ptr(),
            nm_ptr,
            count,
            results.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(())
}

/// GPU-accelerated running COUNT within partitions.
/// `null_mask` may be empty (no nulls). Returns `None` if GPU unavailable.
pub fn window_count(partition_starts: &[u8], null_mask: &[u8], results: &mut [i64]) -> Option<()> {
    let _span = tracing::debug_span!("gpu.window_count", n = partition_starts.len()).entered();
    let count = partition_starts.len().min(results.len());
    if count == 0 {
        return Some(());
    }
    let nm_ptr = if null_mask.is_empty() {
        std::ptr::null()
    } else {
        null_mask.as_ptr()
    };
    // SAFETY: slices are valid; nm_ptr is null or valid.
    let status = unsafe {
        bridge::pgaccel_window_count(
            partition_starts.as_ptr(),
            nm_ptr,
            count,
            results.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(())
}

/// GPU-accelerated LAG within partitions.
/// Returns `None` if GPU is unavailable.
#[allow(clippy::too_many_arguments)]
pub fn window_lag(
    partition_starts: &[u8],
    values: &[f64],
    null_mask: &[u8],
    offset: i32,
    default_val: f64,
    results: &mut [f64],
    result_nulls: &mut [u8],
) -> Option<()> {
    let _span =
        tracing::debug_span!("gpu.window_lag", n = partition_starts.len(), offset).entered();
    let count = partition_starts
        .len()
        .min(values.len())
        .min(results.len())
        .min(result_nulls.len());
    if count == 0 {
        return Some(());
    }
    let nm_ptr = if null_mask.is_empty() {
        std::ptr::null()
    } else {
        null_mask.as_ptr()
    };
    // SAFETY: slices are valid; nm_ptr is null or valid.
    let status = unsafe {
        bridge::pgaccel_window_lag(
            partition_starts.as_ptr(),
            values.as_ptr(),
            nm_ptr,
            count,
            offset,
            default_val,
            results.as_mut_ptr(),
            result_nulls.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(())
}

/// GPU-accelerated LEAD within partitions.
/// Returns `None` if GPU is unavailable.
#[allow(clippy::too_many_arguments)]
pub fn window_lead(
    partition_starts: &[u8],
    values: &[f64],
    null_mask: &[u8],
    offset: i32,
    default_val: f64,
    results: &mut [f64],
    result_nulls: &mut [u8],
) -> Option<()> {
    let _span =
        tracing::debug_span!("gpu.window_lead", n = partition_starts.len(), offset).entered();
    let count = partition_starts
        .len()
        .min(values.len())
        .min(results.len())
        .min(result_nulls.len());
    if count == 0 {
        return Some(());
    }
    let nm_ptr = if null_mask.is_empty() {
        std::ptr::null()
    } else {
        null_mask.as_ptr()
    };
    // SAFETY: slices are valid; nm_ptr is null or valid.
    let status = unsafe {
        bridge::pgaccel_window_lead(
            partition_starts.as_ptr(),
            values.as_ptr(),
            nm_ptr,
            count,
            offset,
            default_val,
            results.as_mut_ptr(),
            result_nulls.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(())
}
