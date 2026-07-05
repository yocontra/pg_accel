use super::bridge;

// ---------------------------------------------------------------------------
// GPU execution observability
// ---------------------------------------------------------------------------

/// Number of kernel invocations that actually ran on GPU since last reset.
pub fn gpu_exec_count() -> u64 {
    // SAFETY: pgaccel_gpu_exec_count reads a thread-local counter.
    unsafe { bridge::pgaccel_gpu_exec_count() }
}

/// Reset the GPU execution counter to zero.
#[allow(dead_code)] // reason: Rust wrapper; reset is invoked by tests via pg_test cfg below
pub fn reset_gpu_exec_count() {
    // SAFETY: pgaccel_reset_gpu_exec_count resets a thread-local counter.
    unsafe { bridge::pgaccel_reset_gpu_exec_count() }
}

/// Assert that at least `min_count` GPU kernel executions occurred.
/// Panics with a clear message if GPU isn't actually running.
#[cfg(feature = "pg_test")]
#[allow(dead_code)] // reason: non-macOS pg_test dispatch smoke still uses this helper
pub fn assert_gpu_executed(min_count: u64) {
    let count = gpu_exec_count();
    assert!(
        count >= min_count,
        "GPU EXECUTION FAILED: expected at least {min_count} GPU kernel \
         executions, got {count}. The GPU is not actually running. Check \
         device_manager.cpp fork detection and AdaptiveCpp SSCP init.",
    );
}
