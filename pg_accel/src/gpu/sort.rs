use super::bridge;

// ---------------------------------------------------------------------------
// Sort wrappers
//
// Key-only sort variants. The active sort path through the executor is
// `sort_kv_*` (which sorts (key, index) pairs and returns the permutation
// for tuple emission). These key-only variants are preserved as a stable
// API surface for future direct-sort callers (e.g. an in-place sort node
// whose output discards the source row order).
// ---------------------------------------------------------------------------

/// GPU-accelerated in-place sort for f32 data.
/// Returns `None` if GPU is unavailable.
#[allow(dead_code)] // reason: key-only sort wrapper; executor uses sort_kv_f32 today
pub fn sort_f32(data: &mut [f32]) -> Option<()> {
    let _span = tracing::debug_span!("gpu.sort_f32", n = data.len()).entered();
    // SAFETY: data is a valid mutable slice.
    let status = unsafe { bridge::pgaccel_sort_f32(data.as_mut_ptr(), data.len()) };
    status.is_ok().then_some(())
}

/// GPU-accelerated in-place sort for f64 data.
#[allow(dead_code)] // reason: key-only sort wrapper; executor uses sort_kv_f64 today
pub fn sort_f64(data: &mut [f64]) -> Option<()> {
    let _span = tracing::debug_span!("gpu.sort_f64", n = data.len()).entered();
    // SAFETY: data is a valid mutable slice.
    let status = unsafe { bridge::pgaccel_sort_f64(data.as_mut_ptr(), data.len()) };
    status.is_ok().then_some(())
}

/// GPU-accelerated in-place sort for i32 data.
#[allow(dead_code)] // reason: key-only sort wrapper; executor uses sort_kv_i32 today
pub fn sort_i32(data: &mut [i32]) -> Option<()> {
    let _span = tracing::debug_span!("gpu.sort_i32", n = data.len()).entered();
    // SAFETY: data is a valid mutable slice.
    let status = unsafe { bridge::pgaccel_sort_i32(data.as_mut_ptr(), data.len()) };
    status.is_ok().then_some(())
}

/// GPU-accelerated in-place sort for i64 data.
#[allow(dead_code)] // reason: key-only sort wrapper; executor uses sort_kv_i64 today
pub fn sort_i64(data: &mut [i64]) -> Option<()> {
    let _span = tracing::debug_span!("gpu.sort_i64", n = data.len()).entered();
    // SAFETY: data is a valid mutable slice.
    let status = unsafe { bridge::pgaccel_sort_i64(data.as_mut_ptr(), data.len()) };
    status.is_ok().then_some(())
}

/// GPU-accelerated key-value sort for f32 keys.
/// Sorts `keys` in place and permutes `indices` to match.
/// Returns `None` if GPU is unavailable.
pub fn sort_kv_f32(keys: &mut [f32], indices: &mut [u32]) -> Option<()> {
    let count = keys.len().min(indices.len());
    let _span = tracing::debug_span!("gpu.sort_kv_f32", n = count).entered();
    // SAFETY: keys and indices are valid mutable slices.
    let status =
        unsafe { bridge::pgaccel_sort_kv_f32(keys.as_mut_ptr(), indices.as_mut_ptr(), count) };
    status.is_ok().then_some(())
}

/// GPU-accelerated key-value sort for f64 keys.
/// Sorts `keys` in place and permutes `indices` to match.
/// Returns `None` if GPU is unavailable.
pub fn sort_kv_f64(keys: &mut [f64], indices: &mut [u32]) -> Option<()> {
    let _span = tracing::debug_span!("gpu.sort_kv_f64", n = keys.len()).entered();
    let count = keys.len().min(indices.len());
    // SAFETY: keys and indices are valid mutable slices.
    let status =
        unsafe { bridge::pgaccel_sort_kv_f64(keys.as_mut_ptr(), indices.as_mut_ptr(), count) };
    status.is_ok().then_some(())
}

/// GPU-accelerated key-value sort for i32 keys.
/// Sorts `keys` in place and permutes `indices` to match.
/// Returns `None` if GPU is unavailable.
#[allow(dead_code)]
pub fn sort_kv_i32(keys: &mut [i32], indices: &mut [u32]) -> Option<()> {
    let count = keys.len().min(indices.len());
    let _span = tracing::debug_span!("gpu.sort_kv_i32", n = count).entered();
    // SAFETY: keys and indices are valid mutable slices.
    let status =
        unsafe { bridge::pgaccel_sort_kv_i32(keys.as_mut_ptr(), indices.as_mut_ptr(), count) };
    status.is_ok().then_some(())
}

/// GPU-accelerated key-value sort for i64 keys.
/// Sorts `keys` in place and permutes `indices` to match.
/// Returns `None` if GPU is unavailable.
#[allow(dead_code)]
pub fn sort_kv_i64(keys: &mut [i64], indices: &mut [u32]) -> Option<()> {
    let count = keys.len().min(indices.len());
    let _span = tracing::debug_span!("gpu.sort_kv_i64", n = count).entered();
    // SAFETY: keys and indices are valid mutable slices.
    let status =
        unsafe { bridge::pgaccel_sort_kv_i64(keys.as_mut_ptr(), indices.as_mut_ptr(), count) };
    status.is_ok().then_some(())
}

fn topk_out_len(input_len: usize, k: usize) -> usize {
    input_len.min(k)
}

/// GPU-accelerated bounded top-k for f32 keys.
/// Returns indices into `keys` in requested ORDER BY order.
pub fn topk_kv_f32(keys: &[f32], k: usize, largest: bool) -> Option<Vec<u32>> {
    let out_len = topk_out_len(keys.len(), k);
    if out_len == 0 {
        return Some(Vec::new());
    }
    let _span = tracing::debug_span!("gpu.topk_kv_f32", n = keys.len(), k, largest).entered();
    let mut out = vec![0u32; out_len];
    let mut out_count = 0usize;
    // SAFETY: keys is a valid input slice; out has capacity for min(n, k).
    let status = unsafe {
        bridge::pgaccel_topk_kv_f32(
            keys.as_ptr(),
            keys.len(),
            k,
            u8::from(largest),
            out.as_mut_ptr(),
            &raw mut out_count,
        )
    };
    if status.is_ok() {
        out.truncate(out_count.min(out_len));
        Some(out)
    } else {
        None
    }
}

/// GPU-accelerated bounded top-k for f64 keys.
/// Returns indices into `keys` in requested ORDER BY order.
pub fn topk_kv_f64(keys: &[f64], k: usize, largest: bool) -> Option<Vec<u32>> {
    let out_len = topk_out_len(keys.len(), k);
    if out_len == 0 {
        return Some(Vec::new());
    }
    let _span = tracing::debug_span!("gpu.topk_kv_f64", n = keys.len(), k, largest).entered();
    let mut out = vec![0u32; out_len];
    let mut out_count = 0usize;
    // SAFETY: keys is a valid input slice; out has capacity for min(n, k).
    let status = unsafe {
        bridge::pgaccel_topk_kv_f64(
            keys.as_ptr(),
            keys.len(),
            k,
            u8::from(largest),
            out.as_mut_ptr(),
            &raw mut out_count,
        )
    };
    if status.is_ok() {
        out.truncate(out_count.min(out_len));
        Some(out)
    } else {
        None
    }
}

/// GPU-accelerated bounded top-k for i32 keys.
/// Returns indices into `keys` in requested ORDER BY order.
pub fn topk_kv_i32(keys: &[i32], k: usize, largest: bool) -> Option<Vec<u32>> {
    let out_len = topk_out_len(keys.len(), k);
    if out_len == 0 {
        return Some(Vec::new());
    }
    let _span = tracing::debug_span!("gpu.topk_kv_i32", n = keys.len(), k, largest).entered();
    let mut out = vec![0u32; out_len];
    let mut out_count = 0usize;
    // SAFETY: keys is a valid input slice; out has capacity for min(n, k).
    let status = unsafe {
        bridge::pgaccel_topk_kv_i32(
            keys.as_ptr(),
            keys.len(),
            k,
            u8::from(largest),
            out.as_mut_ptr(),
            &raw mut out_count,
        )
    };
    if status.is_ok() {
        out.truncate(out_count.min(out_len));
        Some(out)
    } else {
        None
    }
}

/// GPU-accelerated bounded top-k for i64 keys.
/// Returns indices into `keys` in requested ORDER BY order.
pub fn topk_kv_i64(keys: &[i64], k: usize, largest: bool) -> Option<Vec<u32>> {
    let out_len = topk_out_len(keys.len(), k);
    if out_len == 0 {
        return Some(Vec::new());
    }
    let _span = tracing::debug_span!("gpu.topk_kv_i64", n = keys.len(), k, largest).entered();
    let mut out = vec![0u32; out_len];
    let mut out_count = 0usize;
    // SAFETY: keys is a valid input slice; out has capacity for min(n, k).
    let status = unsafe {
        bridge::pgaccel_topk_kv_i64(
            keys.as_ptr(),
            keys.len(),
            k,
            u8::from(largest),
            out.as_mut_ptr(),
            &raw mut out_count,
        )
    };
    if status.is_ok() {
        out.truncate(out_count.min(out_len));
        Some(out)
    } else {
        None
    }
}
