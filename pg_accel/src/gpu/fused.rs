use super::{PgaccelReduceCol, bridge};

// ---------------------------------------------------------------------------
// Fused filter + multi-reduce
// ---------------------------------------------------------------------------

/// Applies `filter_data[i] <cmp_op> cmp_val` to each row, then reduces
/// all passing rows for each column described in `cols`. When `cmp_op`
/// is [`cmp_op::ALWAYS_TRUE`], all rows pass (no filter column needed).
///
/// Returns `(results_vec, pass_count)` after GPU dispatch, or `None` when
/// the GPU path is unavailable/failed. Callers must decline or error rather
/// than compute this path on CPU under a pg_accel plan.
#[must_use]
#[allow(dead_code)] // reason: fused filter+multi-reduce wrapper; executor uses separate filter + reduce passes today
pub fn fused_filter_multi_reduce_f32(
    filter_data: Option<&[f32]>,
    n: usize,
    cmp: i32,
    cmp_val: f32,
    cols: &[PgaccelReduceCol],
    num_cols: usize,
) -> Option<(Vec<f32>, usize)> {
    let _span =
        tracing::debug_span!("gpu.fused_filter_multi_reduce_f32", n, num_cols, cmp,).entered();

    if num_cols == 0 {
        return Some((Vec::new(), 0));
    }

    let mut results = vec![0.0f32; num_cols];
    let mut pass_count: usize = 0;

    let filter_ptr = filter_data.map_or(std::ptr::null(), <[f32]>::as_ptr);

    // SAFETY: filter_ptr is valid (or null for ALWAYS_TRUE). cols is
    // a valid slice of PgaccelReduceCol descriptors with valid data
    // pointers. results and pass_count are valid output pointers.
    let status = unsafe {
        bridge::pgaccel_fused_filter_multi_reduce_f32(
            filter_ptr,
            n,
            cmp,
            cmp_val,
            cols.as_ptr(),
            num_cols,
            results.as_mut_ptr(),
            &raw mut pass_count,
        )
    };
    if status.is_ok() {
        return Some((results, pass_count));
    }

    None
}
