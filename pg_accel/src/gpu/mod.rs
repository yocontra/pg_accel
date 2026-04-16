//! GPU kernel bridge and stubs.
//!
//! GPU kernels run directly in PG backends via AdaptiveCpp/SYCL. One source
//! tree compiles to CUDA, ROCm, Level Zero, Metal, and CPU; the SSCP runtime
//! selects the backend per device and caches compiled kernels across forks.
//!
//! Re-exports a unified API so callers don't need `cfg` at every call-site.

#[cfg(feature = "gpu")]
pub mod bridge;

mod stubs;

pub mod three_layer;

#[cfg(feature = "pg_test")]
mod three_layer_tests;

// Re-export types from whichever module is active.
#[cfg(feature = "gpu")]
#[allow(unused_imports)]
pub use bridge::{
    PgaccelAggCol, PgaccelAggFunc, PgaccelAggState, PgaccelBatch, PgaccelDeviceInfo, PgaccelExpr,
    PgaccelExprInst, PgaccelExprInstruction, PgaccelExprProgram, PgaccelGeomType, PgaccelGeometry,
    PgaccelHashTable, PgaccelKeyType, PgaccelOp, PgaccelPixelType, PgaccelPlatformCaps,
    PgaccelReclassRule, PgaccelReduceCol, PgaccelStatus, PgaccelVal, PgaccelValTag,
};

#[cfg(not(feature = "gpu"))]
#[allow(unused_imports)]
pub use stubs::{
    PgaccelAggCol, PgaccelAggFunc, PgaccelAggState, PgaccelBatch, PgaccelDeviceInfo, PgaccelExpr,
    PgaccelExprInst, PgaccelExprInstruction, PgaccelExprProgram, PgaccelGeomType, PgaccelGeometry,
    PgaccelHashTable, PgaccelKeyType, PgaccelOp, PgaccelPixelType, PgaccelPlatformCaps,
    PgaccelReclassRule, PgaccelReduceCol, PgaccelStatus, PgaccelVal, PgaccelValTag,
};

// Re-export fused ops constants from the active module.
#[cfg(feature = "gpu")]
pub use bridge::{cmp_op, reduce_op};
#[cfg(not(feature = "gpu"))]
pub use stubs::{cmp_op, reduce_op};

// ---------------------------------------------------------------------------
// Unified safe wrappers
// ---------------------------------------------------------------------------

/// Initialise the GPU runtime.
fn init() -> PgaccelStatus {
    #[cfg(feature = "gpu")]
    {
        // SAFETY: pgaccel_init is safe to call once at startup.
        unsafe { bridge::pgaccel_init() }
    }
    #[cfg(not(feature = "gpu"))]
    {
        stubs::pgaccel_init()
    }
}

/// Initialise the AdaptiveCpp/SYCL GPU runtime once per process.
///
/// AdaptiveCpp's SSCP runtime caches compiled kernels per-backend, so this
/// runs directly in every PG backend (and in test binaries). Uses PID
/// tracking to ensure `pgaccel_init()` is called at most once per process.
pub fn ensure_init() {
    use std::sync::atomic::{AtomicU32, Ordering};
    static INIT_PID: AtomicU32 = AtomicU32::new(0);

    let pid = std::process::id();
    let prev = INIT_PID.load(Ordering::Acquire);
    if prev == pid {
        return;
    }

    let status = init();
    if status != PgaccelStatus::Ok {
        pgrx::warning!(
            "pg_accel: GPU init failed (status={:?}). GPU acceleration unavailable.",
            status,
        );
    }
    INIT_PID.store(pid, Ordering::Release);
}

/// Pre-fork warmup: initialize Metal/SkyLight in the postmaster before
/// fork so forked backends can create Metal devices. Safe to call from
/// `_PG_init()` — does not spawn threads.
pub fn prefork_warmup() {
    #[cfg(feature = "gpu")]
    {
        // SAFETY: pgaccel_prefork_warmup does not spawn threads and is
        // safe to call from the postmaster during _PG_init().
        unsafe { bridge::pgaccel_prefork_warmup() }
    }
}

/// Tear down the GPU runtime.
pub fn shutdown() -> PgaccelStatus {
    #[cfg(feature = "gpu")]
    {
        // SAFETY: pgaccel_shutdown is safe if init was called.
        unsafe { bridge::pgaccel_shutdown() }
    }
    #[cfg(not(feature = "gpu"))]
    {
        stubs::pgaccel_shutdown()
    }
}

/// Return information about the selected compute device.
pub fn get_device_info() -> PgaccelDeviceInfo {
    #[cfg(feature = "gpu")]
    {
        // SAFETY: pgaccel_get_device_info returns a zeroed struct if not initialised.
        unsafe { bridge::pgaccel_get_device_info() }
    }
    #[cfg(not(feature = "gpu"))]
    {
        stubs::pgaccel_get_device_info()
    }
}

/// Human-readable device name for log messages.
pub fn device_name() -> String {
    let info = get_device_info();
    let name = info
        .device_name
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8 as char)
        .collect::<String>();
    if name.is_empty() {
        "none".to_string()
    } else {
        name
    }
}

/// Return platform-level capability flags.
pub fn get_caps() -> PgaccelPlatformCaps {
    #[cfg(feature = "gpu")]
    {
        // SAFETY: pgaccel_get_caps returns a zeroed struct if not initialised.
        unsafe { bridge::pgaccel_get_caps() }
    }
    #[cfg(not(feature = "gpu"))]
    {
        stubs::pgaccel_get_caps()
    }
}

// ---------------------------------------------------------------------------
// GPU execution observability
// ---------------------------------------------------------------------------

/// Number of kernel invocations that actually ran on GPU since last reset.
pub fn gpu_exec_count() -> u64 {
    #[cfg(feature = "gpu")]
    {
        // SAFETY: pgaccel_gpu_exec_count reads a thread-local counter.
        unsafe { bridge::pgaccel_gpu_exec_count() }
    }
    #[cfg(not(feature = "gpu"))]
    {
        stubs::pgaccel_gpu_exec_count()
    }
}

/// Reset the GPU execution counter to zero.
pub fn reset_gpu_exec_count() {
    #[cfg(feature = "gpu")]
    {
        // SAFETY: pgaccel_reset_gpu_exec_count resets a thread-local counter.
        unsafe { bridge::pgaccel_reset_gpu_exec_count() }
    }
    #[cfg(not(feature = "gpu"))]
    {
        stubs::pgaccel_reset_gpu_exec_count();
    }
}

/// Number of kernel invocations that fell back to CPU since last reset.
pub fn cpu_fallback_count() -> u64 {
    #[cfg(feature = "gpu")]
    {
        // SAFETY: pgaccel_cpu_fallback_count reads a thread-local counter.
        unsafe { bridge::pgaccel_cpu_fallback_count() }
    }
    #[cfg(not(feature = "gpu"))]
    {
        stubs::pgaccel_cpu_fallback_count()
    }
}

/// Reset the CPU fallback counter to zero.
pub fn reset_cpu_fallback_count() {
    #[cfg(feature = "gpu")]
    {
        // SAFETY: pgaccel_reset_cpu_fallback_count resets a thread-local counter.
        unsafe { bridge::pgaccel_reset_cpu_fallback_count() }
    }
    #[cfg(not(feature = "gpu"))]
    {
        stubs::pgaccel_reset_cpu_fallback_count();
    }
}

/// Assert that at least `min_count` GPU kernel executions occurred.
/// Panics with a clear message if GPU isn't actually running.
#[cfg(any(test, feature = "pg_test"))]
pub fn assert_gpu_executed(min_count: u64) {
    let count = gpu_exec_count();
    assert!(
        count >= min_count,
        "GPU EXECUTION FAILED: expected at least {min_count} GPU kernel \
         executions, got {count}. The GPU is NOT actually running — all work \
         went to CPU stubs. Check device_manager.cpp fork detection.",
    );
}

/// Assert that no kernel invocations fell back to CPU.
#[cfg(any(test, feature = "pg_test"))]
pub fn assert_no_cpu_fallbacks() {
    let count = cpu_fallback_count();
    assert!(
        count == 0,
        "CPU FALLBACK DETECTED: {count} kernel(s) fell back to CPU instead \
         of GPU. Check stderr for pgaccel WARNING messages with kernel names.",
    );
}

/// Run the GPU three-layer spatial intersection pipeline.
///
/// Returns `(definite_true, definite_false, uncertain)` as vectors of
/// `(idx_a, idx_b)` pair indices, or `None` if no GPU device is available
/// (caller should use CPU stubs).
///
/// The C++ kernel evaluates all `count_a × count_b` pairs and partitions
/// them into the three buckets.  Each output pair is two consecutive `u32`
/// values `(i, j)` written by the C side.
#[allow(clippy::similar_names, clippy::type_complexity)]
pub fn spatial_intersects_gpu(
    geoms_a: &[PgaccelGeometry],
    geoms_b: &[PgaccelGeometry],
) -> Option<(Vec<(u32, u32)>, Vec<(u32, u32)>, Vec<(u32, u32)>)> {
    let count_a = geoms_a.len();
    let count_b = geoms_b.len();
    let _span = tracing::info_span!("gpu.spatial_intersects", count_a, count_b,).entered();
    if count_a == 0 || count_b == 0 {
        return Some((Vec::new(), Vec::new(), Vec::new()));
    }

    // The C++ kernel writes (i, j) pair indices — 2 u32 values per pair.
    // Worst case: all pairs land in one bucket = count_a * count_b pairs.
    let total_pairs = count_a.checked_mul(count_b)?;
    // Cap pair buffer at 128 MB (3 buffers × buf_len × 4 bytes each).
    // Beyond this, defer to PG recheck — the transfer overhead dominates.
    const MAX_PAIRS: usize = 8_000_000; // 3 × 16M × 4 = ~192 MB total
    if total_pairs > MAX_PAIRS {
        return None;
    }

    #[cfg(not(feature = "gpu"))]
    {
        // GPU disabled at compile time — no-op, defer to PG executor.
        None
    }

    #[cfg(feature = "gpu")]
    {
        let buf_len = total_pairs * 2;
        let mut dt_buf = vec![0u32; buf_len];
        let mut df_buf = vec![0u32; buf_len];
        let mut uc_buf = vec![0u32; buf_len];
        let mut dt_count: usize = 0;
        let mut df_count: usize = 0;
        let mut uc_count: usize = 0;

        // SAFETY: geoms arrays are valid slices.  Output buffers are
        // pre-allocated to `total_pairs * 2` u32 elements each.  The C
        // function writes at most `total_pairs` pairs (2 u32 each) into
        // each buffer and sets the count to the number of pairs written.
        let status = unsafe {
            bridge::pgaccel_spatial_intersects(
                geoms_a.as_ptr(),
                count_a,
                geoms_b.as_ptr(),
                count_b,
                dt_buf.as_mut_ptr(),
                std::ptr::addr_of_mut!(dt_count),
                df_buf.as_mut_ptr(),
                std::ptr::addr_of_mut!(df_count),
                uc_buf.as_mut_ptr(),
                std::ptr::addr_of_mut!(uc_count),
            )
        };
        if !status.is_ok() {
            pgrx::debug1!(
                "pg_accel: spatial_intersects_gpu bridge returned {:?} for {}x{} pairs",
                status,
                count_a,
                count_b,
            );
            return None;
        }

        // Each count is the number of PAIRS; each pair is 2 consecutive u32s.
        let parse_pairs = |buf: &[u32], pair_count: usize| -> Vec<(u32, u32)> {
            buf[..pair_count * 2]
                .chunks_exact(2)
                .map(|c| (c[0], c[1]))
                .collect()
        };

        Some((
            parse_pairs(&dt_buf, dt_count),
            parse_pairs(&df_buf, df_count),
            parse_pairs(&uc_buf, uc_count),
        ))
    }
}

// ---------------------------------------------------------------------------
// Bulk point-in-polygon (fast path)
// ---------------------------------------------------------------------------

/// Dedicated bulk point-in-polygon test. Takes a flat array of point (x,y)
/// pairs and a single polygon. Returns per-point results: 1=inside,
/// -1=outside, 0=uncertain/boundary.
///
/// Returns `None` if GPU bridge is unavailable.
#[allow(clippy::too_many_arguments)]
pub fn point_in_polygon_bulk(
    points_xy: &[f32],
    poly_bbox: &[f32; 4],
    poly_coords: &[f32],
    poly_coord_count: usize,
    ring_offsets: &[u32],
    ring_count: usize,
) -> Option<Vec<i8>> {
    let point_count = points_xy.len() / 2;
    let _span =
        tracing::info_span!("gpu.point_in_polygon_bulk", point_count, ring_count,).entered();
    if point_count == 0 {
        return Some(Vec::new());
    }

    #[cfg(feature = "gpu")]
    {
        let mut results = vec![0i8; point_count];
        // SAFETY: all slices are valid; results is pre-allocated to point_count.
        let status = unsafe {
            bridge::pgaccel_point_in_polygon_bulk(
                points_xy.as_ptr(),
                point_count,
                poly_bbox.as_ptr(),
                poly_coords.as_ptr(),
                poly_coord_count,
                ring_offsets.as_ptr(),
                ring_count,
                results.as_mut_ptr(),
            )
        };
        if status.is_ok() {
            return Some(results);
        }
    }

    #[cfg(not(feature = "gpu"))]
    {
        let _ = (
            points_xy,
            poly_bbox,
            poly_coords,
            poly_coord_count,
            ring_offsets,
            ring_count,
        );
    }

    None
}

// ---------------------------------------------------------------------------
// Sort wrappers
// ---------------------------------------------------------------------------

/// GPU-accelerated in-place sort for f32 data.
/// Returns `None` if GPU is unavailable.
pub fn sort_f32(data: &mut [f32]) -> Option<()> {
    let _span = tracing::debug_span!("gpu.sort_f32", n = data.len()).entered();
    #[cfg(feature = "gpu")]
    {
        // SAFETY: data is a valid mutable slice.
        let status = unsafe { bridge::pgaccel_sort_f32(data.as_mut_ptr(), data.len()) };
        status.is_ok().then_some(())
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_sort_f32(data.as_mut_ptr(), data.len());
        status.is_ok().then_some(())
    }
}

/// GPU-accelerated in-place sort for f64 data.
pub fn sort_f64(data: &mut [f64]) -> Option<()> {
    let _span = tracing::debug_span!("gpu.sort_f64", n = data.len()).entered();
    #[cfg(feature = "gpu")]
    {
        // SAFETY: data is a valid mutable slice.
        let status = unsafe { bridge::pgaccel_sort_f64(data.as_mut_ptr(), data.len()) };
        status.is_ok().then_some(())
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_sort_f64(data.as_mut_ptr(), data.len());
        status.is_ok().then_some(())
    }
}

/// GPU-accelerated in-place sort for i32 data.
pub fn sort_i32(data: &mut [i32]) -> Option<()> {
    let _span = tracing::debug_span!("gpu.sort_i32", n = data.len()).entered();
    #[cfg(feature = "gpu")]
    {
        // SAFETY: data is a valid mutable slice.
        let status = unsafe { bridge::pgaccel_sort_i32(data.as_mut_ptr(), data.len()) };
        status.is_ok().then_some(())
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_sort_i32(data.as_mut_ptr(), data.len());
        status.is_ok().then_some(())
    }
}

/// GPU-accelerated in-place sort for i64 data.
pub fn sort_i64(data: &mut [i64]) -> Option<()> {
    let _span = tracing::debug_span!("gpu.sort_i64", n = data.len()).entered();
    #[cfg(feature = "gpu")]
    {
        // SAFETY: data is a valid mutable slice.
        let status = unsafe { bridge::pgaccel_sort_i64(data.as_mut_ptr(), data.len()) };
        status.is_ok().then_some(())
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_sort_i64(data.as_mut_ptr(), data.len());
        status.is_ok().then_some(())
    }
}

/// GPU-accelerated key-value sort for f32 keys.
/// Sorts `keys` in place and permutes `indices` to match.
/// Returns `None` if GPU is unavailable.
pub fn sort_kv_f32(keys: &mut [f32], indices: &mut [u32]) -> Option<()> {
    let count = keys.len().min(indices.len());
    let _span = tracing::debug_span!("gpu.sort_kv_f32", n = count).entered();
    #[cfg(feature = "gpu")]
    {
        // SAFETY: keys and indices are valid mutable slices.
        let status =
            unsafe { bridge::pgaccel_sort_kv_f32(keys.as_mut_ptr(), indices.as_mut_ptr(), count) };
        status.is_ok().then_some(())
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_sort_kv_f32(keys.as_mut_ptr(), indices.as_mut_ptr(), count);
        status.is_ok().then_some(())
    }
}

/// GPU-accelerated key-value sort for f64 keys.
/// Sorts `keys` in place and permutes `indices` to match.
/// Returns `None` if GPU is unavailable.
pub fn sort_kv_f64(keys: &mut [f64], indices: &mut [u32]) -> Option<()> {
    let _span = tracing::debug_span!("gpu.sort_kv_f64", n = keys.len()).entered();
    let count = keys.len().min(indices.len());
    #[cfg(feature = "gpu")]
    {
        // SAFETY: keys and indices are valid mutable slices.
        let status =
            unsafe { bridge::pgaccel_sort_kv_f64(keys.as_mut_ptr(), indices.as_mut_ptr(), count) };
        status.is_ok().then_some(())
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_sort_kv_f64(keys.as_mut_ptr(), indices.as_mut_ptr(), count);
        status.is_ok().then_some(())
    }
}

// ---------------------------------------------------------------------------
// Device capability helpers
// ---------------------------------------------------------------------------

/// Metal has no fp64 support. Always returns `false`.
#[allow(dead_code)]
const fn device_has_fp64() -> bool {
    false
}

// ---------------------------------------------------------------------------
// Reduce wrappers
// ---------------------------------------------------------------------------

/// GPU-accelerated f32 sum reduction. Returns `None` if GPU unavailable.
pub fn reduce_sum_f32(data: &[f32]) -> Option<f32> {
    let _span = tracing::debug_span!("gpu.reduce_sum_f32", n = data.len()).entered();
    let mut result: f32 = 0.0;
    #[cfg(feature = "gpu")]
    {
        // SAFETY: data is a valid slice, result is a valid pointer.
        let status =
            unsafe { bridge::pgaccel_reduce_sum_f32(data.as_ptr(), data.len(), &raw mut result) };
        status.is_ok().then_some(result)
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_reduce_sum_f32(data.as_ptr(), data.len(), &raw mut result);
        status.is_ok().then_some(result)
    }
}

/// GPU-accelerated f32 min reduction. Returns `None` if GPU unavailable.
pub fn reduce_min_f32(data: &[f32]) -> Option<f32> {
    let _span = tracing::debug_span!("gpu.reduce_min_f32", n = data.len()).entered();
    let mut result: f32 = 0.0;
    #[cfg(feature = "gpu")]
    {
        // SAFETY: data is a valid slice, result is a valid pointer.
        let status =
            unsafe { bridge::pgaccel_reduce_min_f32(data.as_ptr(), data.len(), &raw mut result) };
        status.is_ok().then_some(result)
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_reduce_min_f32(data.as_ptr(), data.len(), &raw mut result);
        status.is_ok().then_some(result)
    }
}

/// GPU-accelerated f32 max reduction. Returns `None` if GPU unavailable.
pub fn reduce_max_f32(data: &[f32]) -> Option<f32> {
    let _span = tracing::debug_span!("gpu.reduce_max_f32", n = data.len()).entered();
    let mut result: f32 = 0.0;
    #[cfg(feature = "gpu")]
    {
        // SAFETY: data is a valid slice, result is a valid pointer.
        let status =
            unsafe { bridge::pgaccel_reduce_max_f32(data.as_ptr(), data.len(), &raw mut result) };
        status.is_ok().then_some(result)
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_reduce_max_f32(data.as_ptr(), data.len(), &raw mut result);
        status.is_ok().then_some(result)
    }
}

/// GPU-accelerated f64 sum reduction.
pub fn reduce_sum_f64(data: &[f64]) -> Option<f64> {
    let _span = tracing::debug_span!("gpu.reduce_sum_f64", n = data.len()).entered();
    let mut result: f64 = 0.0;
    #[cfg(feature = "gpu")]
    {
        // SAFETY: data is a valid slice, result is a valid pointer.
        let status =
            unsafe { bridge::pgaccel_reduce_sum_f64(data.as_ptr(), data.len(), &raw mut result) };
        if status.is_ok() {
            return Some(result);
        }
        // fp32-only device (Metal): cast to f32 and use f32 kernel.
        let f32_data: Vec<f32> = data.iter().map(|&v| v as f32).collect();
        reduce_sum_f32(&f32_data).map(f64::from)
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_reduce_sum_f64(data.as_ptr(), data.len(), &raw mut result);
        status.is_ok().then_some(result)
    }
}

/// GPU-accelerated i64 sum reduction.
pub fn reduce_sum_i64(data: &[i64]) -> Option<i64> {
    let _span = tracing::debug_span!("gpu.reduce_sum_i64", n = data.len()).entered();
    let mut result: i64 = 0;
    #[cfg(feature = "gpu")]
    {
        // SAFETY: data is a valid slice, result is a valid pointer.
        let status =
            unsafe { bridge::pgaccel_reduce_sum_i64(data.as_ptr(), data.len(), &raw mut result) };
        status.is_ok().then_some(result)
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_reduce_sum_i64(data.as_ptr(), data.len(), &raw mut result);
        status.is_ok().then_some(result)
    }
}

/// GPU-accelerated f64 min reduction.
pub fn reduce_min_f64(data: &[f64]) -> Option<f64> {
    let _span = tracing::debug_span!("gpu.reduce_min_f64", n = data.len()).entered();
    let mut result: f64 = 0.0;
    #[cfg(feature = "gpu")]
    {
        // SAFETY: data is a valid slice, result is a valid pointer.
        let status =
            unsafe { bridge::pgaccel_reduce_min_f64(data.as_ptr(), data.len(), &raw mut result) };
        if status.is_ok() {
            return Some(result);
        }
        let f32_data: Vec<f32> = data.iter().map(|&v| v as f32).collect();
        reduce_min_f32(&f32_data).map(f64::from)
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_reduce_min_f64(data.as_ptr(), data.len(), &raw mut result);
        status.is_ok().then_some(result)
    }
}

/// GPU-accelerated f64 max reduction.
pub fn reduce_max_f64(data: &[f64]) -> Option<f64> {
    let _span = tracing::debug_span!("gpu.reduce_max_f64", n = data.len()).entered();
    let mut result: f64 = 0.0;
    #[cfg(feature = "gpu")]
    {
        // SAFETY: data is a valid slice, result is a valid pointer.
        let status =
            unsafe { bridge::pgaccel_reduce_max_f64(data.as_ptr(), data.len(), &raw mut result) };
        if status.is_ok() {
            return Some(result);
        }
        let f32_data: Vec<f32> = data.iter().map(|&v| v as f32).collect();
        reduce_max_f32(&f32_data).map(f64::from)
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_reduce_max_f64(data.as_ptr(), data.len(), &raw mut result);
        status.is_ok().then_some(result)
    }
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
    #[cfg(feature = "gpu")]
    {
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
    #[cfg(not(feature = "gpu"))]
    {
        let _ = (&mut out_sum, &mut out_min, &mut out_max, &mut out_count);
        None
    }
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
    #[cfg(feature = "gpu")]
    {
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
        if status.is_ok() {
            return Some(ReduceMultiF64 {
                sum: out_sum,
                min: out_min,
                max: out_max,
                count: out_count,
            });
        }
        // fp32-only device: cast and use f32 kernel.
        let f32_data: Vec<f32> = data.iter().map(|&v| v as f32).collect();
        reduce_multi_f32(&f32_data).map(|r| ReduceMultiF64 {
            sum: f64::from(r.sum),
            min: f64::from(r.min),
            max: f64::from(r.max),
            count: r.count,
        })
    }
    #[cfg(not(feature = "gpu"))]
    {
        let _ = (&mut out_sum, &mut out_min, &mut out_max, &mut out_count);
        None
    }
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
    #[cfg(feature = "gpu")]
    {
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
    #[cfg(not(feature = "gpu"))]
    {
        let _ = (&mut out_sum, &mut out_min, &mut out_max, &mut out_count);
        None
    }
}

/// GPU-accelerated mask popcount.
pub fn reduce_count(mask: &[u8]) -> Option<usize> {
    let _span = tracing::debug_span!("gpu.reduce_count", n = mask.len()).entered();
    let mut result: usize = 0;
    #[cfg(feature = "gpu")]
    {
        // SAFETY: mask is a valid slice, result is a valid pointer.
        let status =
            unsafe { bridge::pgaccel_reduce_count(mask.as_ptr(), mask.len(), &raw mut result) };
        status.is_ok().then_some(result)
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_reduce_count(mask.as_ptr(), mask.len(), &raw mut result);
        status.is_ok().then_some(result)
    }
}

// ---------------------------------------------------------------------------
// H3 wrappers
// ---------------------------------------------------------------------------

/// GPU-accelerated bulk H3 resolution extraction.
pub fn h3_get_resolution_bulk(cells: &[u64]) -> Option<Vec<i32>> {
    let mut resolutions = vec![0i32; cells.len()];
    #[cfg(feature = "gpu")]
    {
        // SAFETY: cells and resolutions are valid slices of matching length.
        let status = unsafe {
            bridge::pgaccel_h3_get_resolution_bulk(
                cells.as_ptr(),
                cells.len(),
                resolutions.as_mut_ptr(),
            )
        };
        // SAFETY: pool_reset frees C++ arena allocations from this dispatch.
        unsafe {
            bridge::pgaccel_pool_reset();
        }
        status.is_ok().then_some(resolutions)
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_h3_get_resolution_bulk(
            cells.as_ptr(),
            cells.len(),
            resolutions.as_mut_ptr(),
        );
        status.is_ok().then_some(resolutions)
    }
}

/// GPU-accelerated bulk H3 cell-to-parent.
pub fn h3_cell_to_parent_bulk(cells: &[u64], parent_res: i32) -> Option<Vec<u64>> {
    let mut parents = vec![0u64; cells.len()];
    #[cfg(feature = "gpu")]
    {
        // SAFETY: cells and parents are valid slices of matching length.
        let status = unsafe {
            bridge::pgaccel_h3_cell_to_parent_bulk(
                cells.as_ptr(),
                cells.len(),
                parent_res,
                parents.as_mut_ptr(),
            )
        };
        // SAFETY: pool_reset frees C++ arena allocations from this dispatch.
        unsafe {
            bridge::pgaccel_pool_reset();
        }
        status.is_ok().then_some(parents)
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_h3_cell_to_parent_bulk(
            cells.as_ptr(),
            cells.len(),
            parent_res,
            parents.as_mut_ptr(),
        );
        status.is_ok().then_some(parents)
    }
}

/// GPU-accelerated bulk H3 grid distance.
pub fn h3_grid_distance_bulk(cells_a: &[u64], cells_b: &[u64]) -> Option<Vec<i32>> {
    let count = cells_a.len().min(cells_b.len());
    let mut distances = vec![0i32; count];
    #[cfg(feature = "gpu")]
    {
        // SAFETY: input slices and output buffer are valid and correctly sized.
        let status = unsafe {
            bridge::pgaccel_h3_grid_distance_bulk(
                cells_a.as_ptr(),
                cells_b.as_ptr(),
                count,
                distances.as_mut_ptr(),
            )
        };
        // SAFETY: pool_reset frees C++ arena allocations from this dispatch.
        unsafe {
            bridge::pgaccel_pool_reset();
        }
        status.is_ok().then_some(distances)
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_h3_grid_distance_bulk(
            cells_a.as_ptr(),
            cells_b.as_ptr(),
            count,
            distances.as_mut_ptr(),
        );
        status.is_ok().then_some(distances)
    }
}

/// GPU-accelerated bulk H3 lat/lng to cell index conversion.
///
/// Converts arrays of latitude/longitude (in degrees) at the given H3
/// `resolution` into H3 cell indices. Returns `None` on GPU failure.
/// Invalid coordinates produce `cell_id = 0` (caller should treat as NULL).
pub fn h3_lat_lng_to_cell_bulk(lats: &[f64], lngs: &[f64], resolution: i32) -> Option<Vec<u64>> {
    let count = lats.len().min(lngs.len());
    let mut cell_ids = vec![0u64; count];
    let mut valid = vec![0u8; count];
    #[cfg(feature = "gpu")]
    {
        // SAFETY: input slices and output buffers are valid and correctly sized.
        let status = unsafe {
            bridge::pgaccel_h3_lat_lng_to_cell_bulk(
                lats.as_ptr().cast(),
                lngs.as_ptr().cast(),
                count,
                resolution,
                1, // use_fp64 = true for precision
                cell_ids.as_mut_ptr(),
                valid.as_mut_ptr(),
            )
        };
        if !status.is_ok() {
            // SAFETY: pool_reset frees C++ arena allocations from this dispatch.
            unsafe {
                bridge::pgaccel_pool_reset();
            }
            return None;
        }
        // SAFETY: pool_reset frees C++ arena allocations from this dispatch.
        unsafe {
            bridge::pgaccel_pool_reset();
        }
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_h3_lat_lng_to_cell_bulk(
            lats.as_ptr().cast(),
            lngs.as_ptr().cast(),
            count,
            resolution,
            1,
            cell_ids.as_mut_ptr(),
            valid.as_mut_ptr(),
        );
        if !status.is_ok() {
            return None;
        }
    }
    // Zero out invalid entries so callers can treat 0 as NULL.
    for i in 0..count {
        if valid[i] == 0 {
            cell_ids[i] = 0;
        }
    }
    Some(cell_ids)
}

// ---------------------------------------------------------------------------
// Raster wrappers
// ---------------------------------------------------------------------------

/// GPU-accelerated map algebra.
///
/// Evaluates a bytecode expression per pixel across one or more bands.
/// `band_pixels` is a slice of per-band pixel buffers (each as raw bytes),
/// `pixel_count` is the number of pixels per band, and `pixel_type` is the
/// FFI pixel type code (`PgaccelPixelType as i32`).
///
/// Returns the output pixel buffer and updated nodata mask, or `None` if
/// the GPU is unavailable.
#[allow(clippy::too_many_arguments)]
pub fn map_algebra(
    band_pixels: &[*const std::ffi::c_void],
    pixel_count: usize,
    pixel_type: i32,
    expr: &PgaccelExpr,
    output_pixels: &mut [u8],
    nodata_mask: &mut [u8],
) -> Option<()> {
    if pixel_count == 0 {
        return Some(());
    }

    #[cfg(feature = "gpu")]
    {
        // SAFETY: band_pixels contains valid pointers to pixel data.
        // output_pixels and nodata_mask are pre-allocated by the caller.
        // expr is a valid PgaccelExpr with correct inst_count/band_count.
        let status = unsafe {
            bridge::pgaccel_map_algebra(
                band_pixels.as_ptr(),
                pixel_count,
                pixel_type,
                std::ptr::from_ref(expr),
                output_pixels.as_mut_ptr().cast::<std::ffi::c_void>(),
                nodata_mask.as_mut_ptr(),
            )
        };
        status.is_ok().then_some(())
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_map_algebra(
            band_pixels.as_ptr(),
            pixel_count,
            pixel_type,
            std::ptr::from_ref(expr),
            output_pixels.as_mut_ptr().cast::<std::ffi::c_void>(),
            nodata_mask.as_mut_ptr(),
        );
        status.is_ok().then_some(())
    }
}

/// GPU-accelerated raster clip.
///
/// Clips a raster band to a polygon ring.  Pixels outside the ring are
/// marked as NODATA in the output mask.  Pixel data is copied to
/// `output_pixels`; the nodata mask is updated in place.
///
/// Returns `None` if the GPU is unavailable.
#[allow(clippy::too_many_arguments)]
pub fn raster_clip(
    rast_pixels: *const std::ffi::c_void,
    width: usize,
    height: usize,
    origin_x: f64,
    origin_y: f64,
    scale_x: f64,
    scale_y: f64,
    pixel_type: i32,
    clip_ring_xy: &[f32],
    output_pixels: &mut [u8],
    nodata_mask: &mut [u8],
) -> Option<()> {
    let vertex_count = clip_ring_xy.len() / 2;
    if width == 0 || height == 0 || vertex_count < 3 {
        return Some(());
    }

    #[cfg(feature = "gpu")]
    {
        // SAFETY: rast_pixels is a valid pointer to pixel data.
        // clip_ring_xy is a valid flat f32 slice of (x,y) pairs.
        // output_pixels and nodata_mask are pre-allocated by the caller.
        let status = unsafe {
            bridge::pgaccel_raster_clip(
                rast_pixels,
                width,
                height,
                origin_x,
                origin_y,
                scale_x,
                scale_y,
                pixel_type,
                clip_ring_xy.as_ptr(),
                vertex_count,
                output_pixels.as_mut_ptr().cast::<std::ffi::c_void>(),
                nodata_mask.as_mut_ptr(),
            )
        };
        status.is_ok().then_some(())
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_raster_clip(
            rast_pixels,
            width,
            height,
            origin_x,
            origin_y,
            scale_x,
            scale_y,
            pixel_type,
            clip_ring_xy.as_ptr(),
            vertex_count,
            output_pixels.as_mut_ptr().cast::<std::ffi::c_void>(),
            nodata_mask.as_mut_ptr(),
        );
        status.is_ok().then_some(())
    }
}

/// GPU-accelerated raster reclassification.
///
/// Applies a set of value-range rules to reclassify pixel values.
/// Returns `None` if the GPU is unavailable.
pub fn raster_reclass(
    input_pixels: *const std::ffi::c_void,
    pixel_count: usize,
    input_type: i32,
    rules: &[PgaccelReclassRule],
    output_type: i32,
    output_pixels: &mut [u8],
) -> Option<()> {
    if pixel_count == 0 {
        return Some(());
    }

    #[cfg(feature = "gpu")]
    {
        // SAFETY: input_pixels is a valid pointer to pixel data.
        // rules is a valid slice. output_pixels is pre-allocated.
        let status = unsafe {
            bridge::pgaccel_raster_reclass(
                input_pixels,
                pixel_count,
                input_type,
                rules.as_ptr(),
                rules.len(),
                output_type,
                output_pixels.as_mut_ptr().cast::<std::ffi::c_void>(),
            )
        };
        status.is_ok().then_some(())
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_raster_reclass(
            input_pixels,
            pixel_count,
            input_type,
            rules.as_ptr(),
            rules.len(),
            output_type,
            output_pixels.as_mut_ptr().cast::<std::ffi::c_void>(),
        );
        status.is_ok().then_some(())
    }
}

// ---------------------------------------------------------------------------
// Expression evaluator wrappers
// ---------------------------------------------------------------------------

/// Evaluate a predicate expression on a columnar batch via GPU.
///
/// Returns a vector of three-result values per row:
/// +1 = TRUE, -1 = FALSE, 0 = UNCERTAIN (CPU recheck needed).
/// Returns `None` if the GPU is unavailable.
pub fn expr_eval_predicate(
    program: &PgaccelExprProgram,
    batch: &PgaccelBatch,
    num_rows: usize,
) -> Option<Vec<i8>> {
    let mut results = vec![0i8; num_rows];

    #[cfg(feature = "gpu")]
    {
        // SAFETY: program and batch are valid references. results is pre-allocated.
        let status = unsafe {
            bridge::pgaccel_expr_eval_predicate(
                std::ptr::from_ref(program),
                std::ptr::from_ref(batch),
                results.as_mut_ptr(),
            )
        };
        status.is_ok().then_some(results)
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_expr_eval_predicate(
            std::ptr::from_ref(program),
            std::ptr::from_ref(batch),
            results.as_mut_ptr(),
        );
        status.is_ok().then_some(results)
    }
}

/// Evaluate a projection expression on a columnar batch via GPU.
///
/// Returns `(output_values, uncertain_mask)` or `None` if GPU unavailable.
pub fn expr_eval_project(
    program: &PgaccelExprProgram,
    batch: &PgaccelBatch,
    num_rows: usize,
) -> Option<(Vec<PgaccelVal>, Vec<u8>)> {
    let mut output = vec![PgaccelVal::null(); num_rows];
    let mut uncertain = vec![0u8; num_rows];

    #[cfg(feature = "gpu")]
    {
        // SAFETY: program and batch are valid references. output/uncertain pre-allocated.
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
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_expr_eval_project(
            std::ptr::from_ref(program),
            std::ptr::from_ref(batch),
            output.as_mut_ptr(),
            uncertain.as_mut_ptr(),
        );
        status.is_ok().then_some((output, uncertain))
    }
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
    let mut results = vec![0i8; num_rows];

    #[cfg(feature = "gpu")]
    {
        // SAFETY: batch is a valid reference. results is pre-allocated.
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
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_expr_template_cmp_const(
            std::ptr::from_ref(batch),
            col_idx,
            cmp_opcode,
            const_val,
            results.as_mut_ptr(),
        );
        status.is_ok().then_some(results)
    }
}

// ---------------------------------------------------------------------------
// Hash aggregation wrappers
// ---------------------------------------------------------------------------

/// Opaque wrapper around GPU hash aggregation state.
///
/// Owns the underlying C++ allocation and frees it on drop.
pub struct HashAggResult {
    state: *mut PgaccelAggState,
}

impl Drop for HashAggResult {
    fn drop(&mut self) {
        if !self.state.is_null() {
            #[cfg(feature = "gpu")]
            {
                // SAFETY: state was returned by pgaccel_hash_agg_execute and is non-null.
                unsafe { bridge::pgaccel_agg_free(self.state) };
            }
            #[cfg(not(feature = "gpu"))]
            {
                stubs::pgaccel_agg_free(self.state);
            }
        }
    }
}

impl HashAggResult {
    /// Number of distinct groups.
    #[must_use]
    pub fn group_count(&self) -> usize {
        #[cfg(feature = "gpu")]
        {
            // SAFETY: state is a valid PgaccelAggState pointer (or null, handled by C).
            unsafe { bridge::pgaccel_agg_group_count(self.state) }
        }
        #[cfg(not(feature = "gpu"))]
        {
            stubs::pgaccel_agg_group_count(self.state)
        }
    }

    /// Raw pointer to the contiguous group key buffer.
    ///
    /// The buffer contains `group_count` keys packed according to `key_type`.
    #[must_use]
    pub fn group_keys_ptr(&self) -> *const std::ffi::c_void {
        #[cfg(feature = "gpu")]
        {
            // SAFETY: state is a valid PgaccelAggState pointer.
            unsafe { bridge::pgaccel_agg_get_group_keys(self.state) }
        }
        #[cfg(not(feature = "gpu"))]
        {
            stubs::pgaccel_agg_get_group_keys(self.state)
        }
    }

    /// Get aggregate results for one aggregate column as a slice.
    ///
    /// Returns `None` if the pointer is null.
    #[must_use]
    pub fn results(&self, agg_idx: usize) -> Option<&[f64]> {
        let count = self.group_count();
        if count == 0 {
            return Some(&[]);
        }
        #[cfg(feature = "gpu")]
        let ptr = unsafe {
            // SAFETY: state is valid; agg_idx bounds are checked by C side.
            bridge::pgaccel_agg_get_results(self.state, agg_idx)
        };
        #[cfg(not(feature = "gpu"))]
        let ptr = stubs::pgaccel_agg_get_results(self.state, agg_idx);

        if ptr.is_null() {
            return None;
        }
        // SAFETY: ptr points to count f64 values owned by state (valid until drop).
        Some(unsafe { std::slice::from_raw_parts(ptr, count) })
    }

    /// Get per-group row counts as a slice.
    ///
    /// Returns `None` if the pointer is null.
    #[must_use]
    pub fn counts(&self) -> Option<&[i64]> {
        let count = self.group_count();
        if count == 0 {
            return Some(&[]);
        }
        #[cfg(feature = "gpu")]
        let ptr = unsafe {
            // SAFETY: state is valid.
            bridge::pgaccel_agg_get_counts(self.state)
        };
        #[cfg(not(feature = "gpu"))]
        let ptr = stubs::pgaccel_agg_get_counts(self.state);

        if ptr.is_null() {
            return None;
        }
        // SAFETY: ptr points to count i64 values owned by state (valid until drop).
        Some(unsafe { std::slice::from_raw_parts(ptr, count) })
    }
}

/// Execute GPU hash aggregation on columnar data.
///
/// # Arguments
///
/// * `group_keys` — raw pointer to contiguous group key values
/// * `group_null_mask` — null mask for group keys (1 = null)
/// * `row_count` — number of rows
/// * `key_type` — key type tag (0=i32, 1=i64, 2=f64)
/// * `value_cols` — per-aggregate column data pointers
/// * `value_nulls` — per-aggregate null masks
/// * `value_types` — per-aggregate value type tags
/// * `agg_cols` — aggregate column descriptors
///
/// Returns `None` if the GPU is unavailable or the call fails.
#[allow(clippy::too_many_arguments)]
pub fn hash_agg_execute(
    group_keys: *const std::ffi::c_void,
    group_null_mask: *const u8,
    row_count: usize,
    key_type: i32,
    value_cols: &[*const std::ffi::c_void],
    value_nulls: &[*const u8],
    value_types: &[i32],
    agg_cols: &[PgaccelAggCol],
) -> Option<HashAggResult> {
    let _span = tracing::info_span!(
        "gpu.hash_agg",
        n_rows = row_count,
        n_aggs = agg_cols.len(),
        key_type,
    )
    .entered();
    if row_count == 0 || agg_cols.is_empty() {
        return None;
    }

    #[cfg(feature = "gpu")]
    let state = unsafe {
        // SAFETY: All pointers are caller-provided and valid for row_count elements.
        // value_cols, value_nulls, value_types have num_aggs elements each.
        bridge::pgaccel_hash_agg_execute(
            group_keys,
            group_null_mask,
            row_count,
            key_type,
            value_cols.as_ptr(),
            value_nulls.as_ptr(),
            value_types.as_ptr(),
            agg_cols.as_ptr(),
            agg_cols.len(),
        )
    };

    #[cfg(not(feature = "gpu"))]
    let state = stubs::pgaccel_hash_agg_execute(
        group_keys,
        group_null_mask,
        row_count,
        key_type,
        value_cols.as_ptr(),
        value_nulls.as_ptr(),
        value_types.as_ptr(),
        agg_cols.as_ptr(),
        agg_cols.len(),
    );

    if state.is_null() {
        return None;
    }
    Some(HashAggResult { state })
}

// ---------------------------------------------------------------------------
// Hash join wrappers
// ---------------------------------------------------------------------------

/// RAII wrapper around a GPU-side hash table.
pub struct GpuHashTable {
    ht: *mut PgaccelHashTable,
}

// SAFETY: The hash table handle is only accessed from the main backend thread
// via the safe wrapper methods. No concurrent access occurs.
unsafe impl Send for GpuHashTable {}

impl Drop for GpuHashTable {
    fn drop(&mut self) {
        if !self.ht.is_null() {
            #[cfg(feature = "gpu")]
            {
                // SAFETY: ht was returned by pgaccel_hash_join_build and is non-null.
                unsafe { bridge::pgaccel_hash_join_free(self.ht) };
            }
            #[cfg(not(feature = "gpu"))]
            {
                stubs::pgaccel_hash_join_free(self.ht);
            }
        }
    }
}

impl GpuHashTable {
    /// Build a hash table from inner relation keys.
    ///
    /// * `keys` — pointer to typed key array (int32/int64/float64)
    /// * `null_mask` — per-row null mask (1 = null, excluded from table)
    /// * `indices` — original row indices for mapping probe results back
    /// * `key_type` — the key type tag
    ///
    /// Returns `None` if the GPU is unavailable or the build fails.
    pub fn build(
        keys: *const std::ffi::c_void,
        null_mask: &[u8],
        indices: &[u32],
        key_type: PgaccelKeyType,
    ) -> Option<Self> {
        let count = null_mask.len().min(indices.len());
        if count == 0 {
            return None;
        }

        #[cfg(feature = "gpu")]
        let ht = unsafe {
            // SAFETY: keys points to `count` elements of the specified type.
            // null_mask and indices are valid slices with at least `count` elements.
            bridge::pgaccel_hash_join_build(
                keys,
                null_mask.as_ptr(),
                indices.as_ptr(),
                count,
                key_type,
            )
        };

        #[cfg(not(feature = "gpu"))]
        let ht = stubs::pgaccel_hash_join_build(
            keys,
            null_mask.as_ptr(),
            indices.as_ptr(),
            count,
            key_type,
        );

        if ht.is_null() {
            None
        } else {
            Some(Self { ht })
        }
    }

    /// Probe the hash table with outer relation keys.
    ///
    /// Returns matched `(outer_idx, inner_idx)` pairs, or `None` on failure.
    pub fn probe(
        &self,
        outer_keys: *const std::ffi::c_void,
        outer_null_mask: &[u8],
        max_matches: usize,
    ) -> Option<Vec<(u32, u32)>> {
        if self.ht.is_null() || outer_null_mask.is_empty() {
            return None;
        }
        let outer_count = outer_null_mask.len();
        let buf_len = max_matches * 2;
        let mut match_pairs = vec![0u32; buf_len];
        let mut match_count: usize = 0;

        #[cfg(feature = "gpu")]
        let status = unsafe {
            // SAFETY: ht is non-null (checked above). outer_keys points to
            // outer_count elements. match_pairs has capacity for max_matches*2.
            bridge::pgaccel_hash_join_probe(
                self.ht,
                outer_keys,
                outer_null_mask.as_ptr(),
                outer_count,
                match_pairs.as_mut_ptr(),
                max_matches,
                std::ptr::addr_of_mut!(match_count),
            )
        };

        #[cfg(not(feature = "gpu"))]
        let status = stubs::pgaccel_hash_join_probe(
            self.ht,
            outer_keys,
            outer_null_mask.as_ptr(),
            outer_count,
            match_pairs.as_mut_ptr(),
            max_matches,
            std::ptr::addr_of_mut!(match_count),
        );

        if !status.is_ok() {
            return None;
        }

        let pairs = match_pairs[..match_count * 2]
            .chunks_exact(2)
            .map(|c| (c[0], c[1]))
            .collect();
        Some(pairs)
    }
}

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
    #[cfg(feature = "gpu")]
    {
        // SAFETY: slices are valid and count is within bounds.
        let status = unsafe {
            bridge::pgaccel_window_row_number(
                partition_starts.as_ptr(),
                count,
                results.as_mut_ptr(),
            )
        };
        status.is_ok().then_some(())
    }
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_window_row_number(
            partition_starts.as_ptr(),
            count,
            results.as_mut_ptr(),
        );
        status.is_ok().then_some(())
    }
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
    #[cfg(feature = "gpu")]
    {
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
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_window_rank(
            partition_starts.as_ptr(),
            sort_keys.as_ptr(),
            count,
            results.as_mut_ptr(),
        );
        status.is_ok().then_some(())
    }
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
    #[cfg(feature = "gpu")]
    {
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
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_window_dense_rank(
            partition_starts.as_ptr(),
            sort_keys.as_ptr(),
            count,
            results.as_mut_ptr(),
        );
        status.is_ok().then_some(())
    }
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
    #[cfg(feature = "gpu")]
    {
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
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_window_sum(
            partition_starts.as_ptr(),
            values.as_ptr(),
            nm_ptr,
            count,
            results.as_mut_ptr(),
        );
        status.is_ok().then_some(())
    }
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
    #[cfg(feature = "gpu")]
    {
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
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_window_count(
            partition_starts.as_ptr(),
            nm_ptr,
            count,
            results.as_mut_ptr(),
        );
        status.is_ok().then_some(())
    }
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
    #[cfg(feature = "gpu")]
    {
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
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_window_lag(
            partition_starts.as_ptr(),
            values.as_ptr(),
            nm_ptr,
            count,
            offset,
            default_val,
            results.as_mut_ptr(),
            result_nulls.as_mut_ptr(),
        );
        status.is_ok().then_some(())
    }
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
    #[cfg(feature = "gpu")]
    {
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
    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_window_lead(
            partition_starts.as_ptr(),
            values.as_ptr(),
            nm_ptr,
            count,
            offset,
            default_val,
            results.as_mut_ptr(),
            result_nulls.as_mut_ptr(),
        );
        status.is_ok().then_some(())
    }
}

// ---------------------------------------------------------------------------
// Fused filter + multi-reduce
// ---------------------------------------------------------------------------

/// Applies `filter_data[i] <cmp_op> cmp_val` to each row, then reduces
/// all passing rows for each column described in `cols`. When `cmp_op`
/// is [`cmp_op::ALWAYS_TRUE`], all rows pass (no filter column needed).
///
/// Returns `(results_vec, pass_count)` or `None` on GPU failure.
#[must_use]
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

    #[cfg(feature = "gpu")]
    {
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
    }

    #[cfg(not(feature = "gpu"))]
    {
        let status = stubs::pgaccel_fused_filter_multi_reduce_f32(
            filter_ptr,
            n,
            cmp,
            cmp_val,
            cols.as_ptr(),
            num_cols,
            results.as_mut_ptr(),
            &raw mut pass_count,
        );
        if status.is_ok() {
            return Some((results, pass_count));
        }
    }

    None
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod mod_tests {
    use std::collections::HashSet;
    use std::{mem, ptr};

    use super::three_layer::{
        ExtractedGeometry, GeomType, PredicateResult, SpatialPredicate, SpatialResult,
    };
    use super::*;

    // -----------------------------------------------------------------------
    // Safe wrapper functions — stubs path (no GPU library linked)
    // -----------------------------------------------------------------------

    #[cfg(not(feature = "gpu"))]
    #[test]
    fn init_returns_error_no_device() {
        assert_eq!(init(), PgaccelStatus::ErrorNoDevice);
    }

    #[cfg(not(feature = "gpu"))]
    #[test]
    fn shutdown_returns_error_no_device() {
        assert_eq!(shutdown(), PgaccelStatus::ErrorNoDevice);
    }

    #[test]
    fn get_device_info_returns_valid_struct() {
        let info = get_device_info();
        // With gpu feature, real device may be present; without, all zeroed.
        #[cfg(not(feature = "gpu"))]
        {
            assert!(!info.has_fp64);
            assert!(!info.has_atomic64);
            assert!(!info.is_unified_memory);
            assert_eq!(info.max_alloc_bytes, 0);
            assert_eq!(info.compute_units, 0);
        }
        #[cfg(feature = "gpu")]
        {
            // Just verify we can read the struct without crashing.
            let _ = info.compute_units;
            let _ = info.max_alloc_bytes;
        }
    }

    #[test]
    fn get_caps_returns_valid_struct() {
        let caps = get_caps();
        #[cfg(not(feature = "gpu"))]
        {
            assert!(!caps.has_fp64);
            assert!(!caps.has_atomic64);
            assert!(!caps.has_ooo_queue);
            assert!(!caps.is_unified_memory);
            assert_eq!(caps.max_alloc_bytes, 0);
            assert_eq!(caps.compute_units, 0);
        }
        #[cfg(feature = "gpu")]
        {
            let _ = caps.compute_units;
            let _ = caps.max_alloc_bytes;
        }
    }

    // -----------------------------------------------------------------------
    // spatial_intersects_gpu
    // -----------------------------------------------------------------------

    #[test]
    fn spatial_intersects_gpu_empty_slices_returns_empty_vecs() {
        let result = spatial_intersects_gpu(&[], &[]);
        let (dt, df, uc) = result.unwrap();
        assert!(dt.is_empty());
        assert!(df.is_empty());
        assert!(uc.is_empty());
    }

    #[cfg(not(feature = "gpu"))]
    #[test]
    fn spatial_intersects_gpu_nonempty_returns_none_without_gpu() {
        // Build a minimal geometry descriptor (pointers unused by stubs).
        let geom = PgaccelGeometry {
            geom_type: PgaccelGeomType::Point,
            bbox: ptr::null(),
            coords: ptr::null(),
            coord_count: 1,
            ring_offsets: ptr::null(),
            ring_count: 0,
        };
        let geoms_a = [geom.clone()];
        let geoms_b = [geom];
        let result = spatial_intersects_gpu(&geoms_a, &geoms_b);
        // Fallback returns ErrorNoDevice, so the wrapper returns None.
        assert!(result.is_none());
    }

    #[cfg(not(feature = "gpu"))]
    #[test]
    fn spatial_intersects_gpu_mismatched_lengths_uses_min() {
        // With count = min(2, 1) = 1 > 0, so it hits the stubs path
        // which returns ErrorNoDevice -> None.
        let geom = PgaccelGeometry {
            geom_type: PgaccelGeomType::Polygon,
            bbox: ptr::null(),
            coords: ptr::null(),
            coord_count: 0,
            ring_offsets: ptr::null(),
            ring_count: 0,
        };
        let geoms_a = [geom.clone(), geom.clone()];
        let geoms_b = [geom];
        let result = spatial_intersects_gpu(&geoms_a, &geoms_b);
        assert!(result.is_none());
    }

    #[test]
    fn spatial_intersects_gpu_one_empty_returns_empty_vecs() {
        // min(0, 1) = 0, so the early-return path fires.
        let geom = PgaccelGeometry {
            geom_type: PgaccelGeomType::Point,
            bbox: ptr::null(),
            coords: ptr::null(),
            coord_count: 1,
            ring_offsets: ptr::null(),
            ring_count: 0,
        };
        let result = spatial_intersects_gpu(&[], &[geom]);
        let (dt, df, uc) = result.unwrap();
        assert!(dt.is_empty());
        assert!(df.is_empty());
        assert!(uc.is_empty());
    }

    // -----------------------------------------------------------------------
    // SpatialPredicate enum
    // -----------------------------------------------------------------------

    #[test]
    fn spatial_predicate_intersects_debug() {
        let p = SpatialPredicate::Intersects;
        let dbg = format!("{p:?}");
        assert!(dbg.contains("Intersects"));
    }

    #[test]
    fn spatial_predicate_contains_eq() {
        assert_eq!(SpatialPredicate::Contains, SpatialPredicate::Contains);
        assert_ne!(SpatialPredicate::Contains, SpatialPredicate::Within);
    }

    #[test]
    fn spatial_predicate_within_clone() {
        let p = SpatialPredicate::Within;
        let cloned = p;
        assert_eq!(cloned, SpatialPredicate::Within);
    }

    #[test]
    fn spatial_predicate_dwithin_stores_distance() {
        let p = SpatialPredicate::DWithin(100.5);
        if let SpatialPredicate::DWithin(d) = p {
            assert!((d - 100.5).abs() < f64::EPSILON);
        } else {
            panic!("expected DWithin variant");
        }
    }

    #[test]
    fn spatial_predicate_all_variants_are_distinct() {
        let variants: Vec<SpatialPredicate> = vec![
            SpatialPredicate::Intersects,
            SpatialPredicate::Contains,
            SpatialPredicate::Within,
            SpatialPredicate::DWithin(0.0),
        ];
        for (i, a) in variants.iter().enumerate() {
            for (j, b) in variants.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "variants at {i} and {j} should differ");
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // ExtractedGeometry
    // -----------------------------------------------------------------------

    #[test]
    fn extracted_geometry_construction_with_bbox() {
        let geom = ExtractedGeometry {
            bbox: [1.0, 2.0, 3.0, 4.0],
            coords: vec![1.0, 2.0, 3.0, 4.0],
            coord_count: 2,
            geom_type: GeomType::LineString,
            ring_offsets: Vec::new(),
        };
        assert_eq!(geom.bbox[0], 1.0);
        assert_eq!(geom.bbox[3], 4.0);
        assert_eq!(geom.coord_count, 2);
        assert_eq!(geom.coords.len(), 4);
        assert_eq!(geom.geom_type, GeomType::LineString);
    }

    #[test]
    fn extracted_geometry_empty_coords() {
        let geom = ExtractedGeometry {
            bbox: [0.0, 0.0, 0.0, 0.0],
            coords: vec![],
            coord_count: 0,
            geom_type: GeomType::Unknown,
            ring_offsets: Vec::new(),
        };
        assert!(geom.coords.is_empty());
        assert_eq!(geom.coord_count, 0);
        assert_eq!(geom.geom_type, GeomType::Unknown);
    }

    #[test]
    fn extracted_geometry_point_has_degenerate_bbox() {
        let geom = ExtractedGeometry {
            bbox: [5.5, 3.3, 5.5, 3.3],
            coords: vec![5.5, 3.3],
            coord_count: 1,
            geom_type: GeomType::Point,
            ring_offsets: Vec::new(),
        };
        // Point bbox: xmin == xmax, ymin == ymax
        assert_eq!(geom.bbox[0], geom.bbox[2]);
        assert_eq!(geom.bbox[1], geom.bbox[3]);
    }

    #[test]
    fn extracted_geometry_clone() {
        let geom = ExtractedGeometry {
            bbox: [1.0, 2.0, 3.0, 4.0],
            coords: vec![1.0, 2.0, 3.0, 4.0],
            coord_count: 2,
            geom_type: GeomType::Polygon,
            ring_offsets: vec![0],
        };
        let cloned = geom.clone();
        assert_eq!(cloned.coord_count, geom.coord_count);
        assert_eq!(cloned.coords, geom.coords);
        assert_eq!(cloned.bbox, geom.bbox);
    }

    #[test]
    fn extracted_geometry_debug_output() {
        let geom = ExtractedGeometry {
            bbox: [0.0; 4],
            coords: vec![],
            coord_count: 0,
            geom_type: GeomType::Point,
            ring_offsets: Vec::new(),
        };
        let dbg = format!("{geom:?}");
        assert!(dbg.contains("ExtractedGeometry"));
        assert!(dbg.contains("Point"));
    }

    // -----------------------------------------------------------------------
    // PredicateResult
    // -----------------------------------------------------------------------

    #[test]
    fn predicate_result_true_variant() {
        let r = PredicateResult::True;
        assert_eq!(r, PredicateResult::True);
        assert_ne!(r, PredicateResult::False);
        assert_ne!(r, PredicateResult::Uncertain);
    }

    #[test]
    fn predicate_result_false_variant() {
        let r = PredicateResult::False;
        assert_eq!(r, PredicateResult::False);
        assert_ne!(r, PredicateResult::True);
    }

    #[test]
    fn predicate_result_uncertain_variant() {
        let r = PredicateResult::Uncertain;
        assert_eq!(r, PredicateResult::Uncertain);
        assert_ne!(r, PredicateResult::False);
    }

    #[test]
    fn predicate_result_clone_and_copy() {
        let r = PredicateResult::True;
        let cloned = r;
        assert_eq!(r, cloned);
    }

    #[test]
    fn predicate_result_debug_output() {
        assert!(format!("{:?}", PredicateResult::True).contains("True"));
        assert!(format!("{:?}", PredicateResult::False).contains("False"));
        assert!(format!("{:?}", PredicateResult::Uncertain).contains("Uncertain"));
    }

    // -----------------------------------------------------------------------
    // SpatialResult construction
    // -----------------------------------------------------------------------

    #[test]
    fn spatial_result_construction_and_field_access() {
        let sr = SpatialResult {
            definite_true: vec![0, 1, 2],
            definite_false: vec![3, 4],
            uncertain: vec![5],
        };
        assert_eq!(sr.definite_true.len(), 3);
        assert_eq!(sr.definite_false.len(), 2);
        assert_eq!(sr.uncertain.len(), 1);
        assert_eq!(sr.uncertain[0], 5);
    }

    #[test]
    fn spatial_result_empty() {
        let sr = SpatialResult {
            definite_true: vec![],
            definite_false: vec![],
            uncertain: vec![],
        };
        assert!(sr.definite_true.is_empty());
        assert!(sr.definite_false.is_empty());
        assert!(sr.uncertain.is_empty());
    }

    #[test]
    fn spatial_result_clone() {
        let sr = SpatialResult {
            definite_true: vec![10],
            definite_false: vec![20, 30],
            uncertain: vec![],
        };
        let cloned = sr.clone();
        assert_eq!(cloned.definite_true, sr.definite_true);
        assert_eq!(cloned.definite_false, sr.definite_false);
    }

    // -----------------------------------------------------------------------
    // GeomType enum
    // -----------------------------------------------------------------------

    #[test]
    fn geom_type_all_variants_distinct() {
        let variants = [
            GeomType::Point,
            GeomType::LineString,
            GeomType::Polygon,
            GeomType::Unknown,
        ];
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
    fn geom_type_debug_output() {
        assert!(format!("{:?}", GeomType::Point).contains("Point"));
        assert!(format!("{:?}", GeomType::LineString).contains("LineString"));
        assert!(format!("{:?}", GeomType::Polygon).contains("Polygon"));
        assert!(format!("{:?}", GeomType::Unknown).contains("Unknown"));
    }

    // -----------------------------------------------------------------------
    // Sort wrappers — stubs returns None
    // -----------------------------------------------------------------------

    #[cfg(not(feature = "gpu"))]
    #[test]
    fn sort_f32_returns_none_without_gpu() {
        let mut data = [3.0f32, 1.0, 2.0];
        assert!(sort_f32(&mut data).is_none());
    }

    #[cfg(not(feature = "gpu"))]
    #[test]
    fn sort_i64_returns_none_without_gpu() {
        let mut data = [3i64, 1, 2];
        assert!(sort_i64(&mut data).is_none());
    }

    #[cfg(not(feature = "gpu"))]
    #[test]
    fn reduce_sum_f32_returns_none_without_gpu() {
        let data = [1.0f32, 2.0, 3.0];
        assert!(reduce_sum_f32(&data).is_none());
    }

    #[cfg(not(feature = "gpu"))]
    #[test]
    fn reduce_count_returns_none_without_gpu() {
        let mask = [1u8, 0, 1, 1];
        assert!(reduce_count(&mask).is_none());
    }

    #[cfg(not(feature = "gpu"))]
    #[test]
    fn h3_get_resolution_returns_none_without_gpu() {
        let cells = [0x8928308280fffffu64];
        assert!(h3_get_resolution_bulk(&cells).is_none());
    }
}
