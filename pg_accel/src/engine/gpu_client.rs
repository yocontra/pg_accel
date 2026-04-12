//! GPU client for forked backends.
//!
//! Forked backends cannot use GPU directly (IOKit breaks after fork).
//! This module provides an IPC layer that sends GPU work requests to the
//! Background Worker coordinator via shared memory.

use std::sync::atomic::Ordering;

use pgrx::pg_sys;

use super::gpu_bgw::{
    GPU_BGW, GpuOp, GpuRequest, GpuResponse, STATE_DONE, STATE_IDLE, STATE_PENDING,
};

// ---------------------------------------------------------------------------
// Public API: submit GPU work and wait for result
// ---------------------------------------------------------------------------

/// Submit a GPU work request to the BGW and wait for the result.
///
/// Returns `None` if the BGW is not running or the request times out.
///
/// # Safety
///
/// Must be called from the main backend thread. The DSM handle must
/// reference a valid, live DSM segment.
pub unsafe fn submit_and_wait(request: &GpuRequest) -> Option<GpuResponse> {
    tracing::trace!("submit_and_wait: op={}", request.op);
    // Check that the BGW is alive.
    let bgw_pid = {
        let state = GPU_BGW.share();
        state.bgw_pid.load(Ordering::Acquire)
    };
    if bgw_pid <= 0 {
        return None; // BGW not started yet.
    }

    let my_pid = unsafe { pg_sys::MyProcPid };

    // Acquire the request slot: spin-wait until IDLE, then write.
    // In practice there's minimal contention since queries are serialized
    // per-backend, and Metal's in-order queue serializes GPU work anyway.
    let mut attempts = 0u32;
    loop {
        {
            let state = GPU_BGW.exclusive();
            if state
                .state
                .compare_exchange(
                    STATE_IDLE,
                    STATE_PENDING,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                // We own the slot. Write our request.
                state.requester_pid.store(my_pid, Ordering::Release);
                // SAFETY: we hold exclusive lock, safe to write.
                let req_ptr = (&raw const state.request).cast_mut();
                unsafe { req_ptr.write(*request) };
                break;
            }
        }

        // Slot is busy — another backend is using it. Wait briefly.
        attempts += 1;
        if attempts > 50_000 {
            // ~5 seconds of spinning. Give up.
            pgrx::warning!("pg_accel: GPU request slot busy for too long, giving up");
            return None;
        }
        std::hint::spin_loop();
        if attempts.is_multiple_of(1000) {
            // Yield to OS every 1000 spins (~100μs).
            std::thread::yield_now();
        }
    }

    // Wake the BGW. BackendPidGetProc may return null for BGWs without
    // database connections (they're not in ProcArray). Fall back to
    // SIGUSR1 which PG uses as a generic "check your latch" signal.
    unsafe {
        let bgw_proc = pg_sys::BackendPidGetProc(bgw_pid);
        if bgw_proc.is_null() {
            // SAFETY: SIGUSR1 is the standard PG signal for latch wakeup.
            libc::kill(bgw_pid, libc::SIGUSR1);
        } else {
            pg_sys::SetLatch(&raw mut (*bgw_proc).procLatch);
        }
    }

    // Wait for the response: poll STATE_DONE with latch waits.
    let response = wait_for_response();

    // Reset state to IDLE so the next request can proceed.
    {
        let state = GPU_BGW.exclusive();
        state.state.store(STATE_IDLE, Ordering::Release);
    }

    response
}

/// Wait for the BGW to complete our request.
///
/// Hot path spin-polls the shared-memory state word for sub-millisecond
/// round-trip latency (critical for small reductions, where BGW latency
/// dominates kernel time). After a brief spin budget we fall back to
/// WaitLatch with a short timeout so long-running GPU kernels don't
/// burn a core.
fn wait_for_response() -> Option<GpuResponse> {
    let timeout_ms = 10_000i64; // 10 second timeout
    let start = std::time::Instant::now();

    loop {
        // Spin-poll for ~100µs — this catches the common case of a tiny
        // reduce kernel that the BGW turns around in microseconds.
        for _ in 0..1000 {
            let state = GPU_BGW.share();
            if state.state.load(Ordering::Acquire) == STATE_DONE {
                let resp = state.response;
                return Some(resp);
            }
            drop(state);
            std::hint::spin_loop();
        }

        // Check timeout.
        if start.elapsed().as_millis() as i64 > timeout_ms {
            pgrx::warning!("pg_accel: GPU request timed out after {timeout_ms}ms");
            // Force-reset to IDLE so the slot isn't stuck.
            let state = GPU_BGW.exclusive();
            state.state.store(STATE_IDLE, Ordering::Release);
            return None;
        }

        // Wait on our own latch (the BGW will SetLatch us when done).
        // SAFETY: MyLatch is valid in any backend process.
        unsafe {
            pg_sys::WaitLatch(
                pg_sys::MyLatch,
                (pg_sys::WL_LATCH_SET | pg_sys::WL_TIMEOUT) as i32,
                1, // 1ms timeout between checks
                pg_sys::PG_WAIT_EXTENSION,
            );
            pg_sys::ResetLatch(pg_sys::MyLatch);
        }

        // Check for interrupts (so Ctrl-C works).
        pgrx::check_for_interrupts!();
    }
}

// ---------------------------------------------------------------------------
// Convenience: DSM helpers for building requests
// ---------------------------------------------------------------------------

/// Create a DSM segment, copy data into it, and return (handle, segment).
///
/// # Safety
///
/// Must be called from the main backend thread.
#[must_use]
pub unsafe fn create_dsm_with_data(data: &[u8]) -> Option<(u32, *mut pg_sys::dsm_segment)> {
    let size = data.len();
    if size == 0 {
        return None;
    }

    // SAFETY: dsm_create allocates a shared memory segment.
    let seg = unsafe { pg_sys::dsm_create(size, 0) };
    if seg.is_null() {
        return None;
    }

    let handle = unsafe { pg_sys::dsm_segment_handle(seg) };
    let base = unsafe { pg_sys::dsm_segment_address(seg).cast::<u8>() };

    // Copy data into DSM.
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), base, size);
    }

    Some((handle, seg))
}

/// Create a DSM segment large enough for two arrays, copy both in.
/// Returns (handle, segment, offset1, offset2).
///
/// # Safety
///
/// Must be called from the main backend thread.
#[must_use]
pub unsafe fn create_dsm_with_two_arrays(
    data1: &[u8],
    data2: &[u8],
) -> Option<(u32, *mut pg_sys::dsm_segment, u64, u64)> {
    let total = data1.len() + data2.len();
    if total == 0 {
        return None;
    }

    // SAFETY: dsm_create allocates a shared memory segment.
    let seg = unsafe { pg_sys::dsm_create(total, 0) };
    if seg.is_null() {
        return None;
    }

    let handle = unsafe { pg_sys::dsm_segment_handle(seg) };
    let base = unsafe { pg_sys::dsm_segment_address(seg).cast::<u8>() };

    let off1 = 0u64;
    let off2 = data1.len() as u64;

    unsafe {
        std::ptr::copy_nonoverlapping(data1.as_ptr(), base, data1.len());
        std::ptr::copy_nonoverlapping(data2.as_ptr(), base.add(data1.len()), data2.len());
    }

    Some((handle, seg, off1, off2))
}

// ---------------------------------------------------------------------------
// High-level GPU dispatch functions (used by gpu/mod.rs)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Reduce wrappers (all follow the same DSM → request → scalar pattern)
// ---------------------------------------------------------------------------

/// Generic f32 reduce via BGW.
///
/// # Safety
///
/// `data` must point to `count` valid f32 values.
unsafe fn bgw_reduce_f32_op(data: *const f32, count: usize, op: GpuOp) -> Option<f32> {
    let slice = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), count * 4) };
    let (handle, seg) = unsafe { create_dsm_with_data(slice)? };

    let req = GpuRequest {
        op: op as u32,
        dsm_handle: handle,
        n_rows: count as u64,
        input_offset: 0,
        input_len: (count * 4) as u64,
        ..GpuRequest::default()
    };

    let resp = unsafe { submit_and_wait(&req) };
    // SAFETY: seg is a valid DSM segment we created.
    unsafe { pg_sys::dsm_detach(seg) };

    resp.filter(|r| r.status == 0).map(|r| r.scalar_f64 as f32)
}

/// Generic f64 reduce via BGW.
///
/// # Safety
///
/// `data` must point to `count` valid f64 values.
unsafe fn bgw_reduce_f64_op(data: *const f64, count: usize, op: GpuOp) -> Option<f64> {
    let slice = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), count * 8) };
    let (handle, seg) = unsafe { create_dsm_with_data(slice)? };

    let req = GpuRequest {
        op: op as u32,
        dsm_handle: handle,
        n_rows: count as u64,
        input_offset: 0,
        input_len: (count * 8) as u64,
        ..GpuRequest::default()
    };

    let resp = unsafe { submit_and_wait(&req) };
    // SAFETY: seg is a valid DSM segment we created.
    unsafe { pg_sys::dsm_detach(seg) };

    resp.filter(|r| r.status == 0).map(|r| r.scalar_f64)
}

/// Reduce sum f32 via BGW.
///
/// # Safety
///
/// `data` must point to `count` valid f32 values.
#[must_use]
pub unsafe fn bgw_reduce_sum_f32(data: *const f32, count: usize) -> Option<f32> {
    unsafe { bgw_reduce_f32_op(data, count, GpuOp::ReduceSumF32) }
}

/// Reduce min f32 via BGW.
///
/// # Safety
///
/// `data` must point to `count` valid f32 values.
#[must_use]
pub unsafe fn bgw_reduce_min_f32(data: *const f32, count: usize) -> Option<f32> {
    unsafe { bgw_reduce_f32_op(data, count, GpuOp::ReduceMinF32) }
}

/// Reduce max f32 via BGW.
///
/// # Safety
///
/// `data` must point to `count` valid f32 values.
#[must_use]
pub unsafe fn bgw_reduce_max_f32(data: *const f32, count: usize) -> Option<f32> {
    unsafe { bgw_reduce_f32_op(data, count, GpuOp::ReduceMaxF32) }
}

/// Reduce sum f64 via BGW.
///
/// # Safety
///
/// `data` must point to `count` valid f64 values.
#[must_use]
pub unsafe fn bgw_reduce_sum_f64(data: *const f64, count: usize) -> Option<f64> {
    unsafe { bgw_reduce_f64_op(data, count, GpuOp::ReduceSumF64) }
}

/// Reduce min f64 via BGW.
///
/// # Safety
///
/// `data` must point to `count` valid f64 values.
#[must_use]
pub unsafe fn bgw_reduce_min_f64(data: *const f64, count: usize) -> Option<f64> {
    unsafe { bgw_reduce_f64_op(data, count, GpuOp::ReduceMinF64) }
}

/// Reduce max f64 via BGW.
///
/// # Safety
///
/// `data` must point to `count` valid f64 values.
#[must_use]
pub unsafe fn bgw_reduce_max_f64(data: *const f64, count: usize) -> Option<f64> {
    unsafe { bgw_reduce_f64_op(data, count, GpuOp::ReduceMaxF64) }
}

/// Reduce sum i64 via BGW.
///
/// # Safety
///
/// `data` must point to `count` valid i64 values.
#[must_use]
pub unsafe fn bgw_reduce_sum_i64(data: *const i64, count: usize) -> Option<i64> {
    let slice = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), count * 8) };
    let (handle, seg) = unsafe { create_dsm_with_data(slice)? };

    let req = GpuRequest {
        op: GpuOp::ReduceSumI64 as u32,
        dsm_handle: handle,
        n_rows: count as u64,
        input_offset: 0,
        input_len: (count * 8) as u64,
        ..GpuRequest::default()
    };

    let resp = unsafe { submit_and_wait(&req) };
    // SAFETY: seg is a valid DSM segment we created.
    unsafe { pg_sys::dsm_detach(seg) };

    resp.filter(|r| r.status == 0).map(|r| r.scalar_i64)
}

// ---------------------------------------------------------------------------
// Fused multi-aggregate reduce wrappers (Fix Agent 4, 2026-04-11)
// ---------------------------------------------------------------------------

/// Result of a fused multi-aggregate reduce: SUM/MIN/MAX/COUNT in one pass.
#[derive(Debug, Clone, Copy)]
pub struct ReduceMultiF32 {
    pub sum: f32,
    pub min: f32,
    pub max: f32,
    pub count: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct ReduceMultiF64 {
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub count: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct ReduceMultiI64 {
    pub sum: i64,
    pub min: i64,
    pub max: i64,
    pub count: i64,
}

/// Fused multi-aggregate reduce (f32) via BGW.
///
/// Single kernel launch computes SUM+MIN+MAX+COUNT over `data` in one pass,
/// replacing four sequential per-op BGW round-trips.
///
/// # Safety
///
/// `data` must point to `count` valid f32 values.
#[must_use]
pub unsafe fn bgw_reduce_multi_f32(data: *const f32, count: usize) -> Option<ReduceMultiF32> {
    if count == 0 {
        return Some(ReduceMultiF32 {
            sum: 0.0,
            min: 0.0,
            max: 0.0,
            count: 0,
        });
    }
    let slice = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), count * 4) };
    let (handle, seg) = unsafe { create_dsm_with_data(slice)? };

    let req = GpuRequest {
        op: GpuOp::ReduceMultiF32 as u32,
        dsm_handle: handle,
        n_rows: count as u64,
        input_offset: 0,
        input_len: (count * 4) as u64,
        ..GpuRequest::default()
    };

    let resp = unsafe { submit_and_wait(&req) };
    // SAFETY: seg is a valid DSM segment we created.
    unsafe { pg_sys::dsm_detach(seg) };

    let r = resp.filter(|r| r.status == 0)?;
    // Worker packs: scalar_f64=SUM, scalar_i64=COUNT,
    //               scalar2_f64=MIN, scalar3_f64=MAX (all as f64/f32).
    Some(ReduceMultiF32 {
        sum: r.scalar_f64 as f32,
        min: r.scalar2_f64 as f32,
        max: r.scalar3_f64 as f32,
        count: r.scalar_i64,
    })
}

/// Fused multi-aggregate reduce (f64) via BGW.
///
/// # Safety
///
/// `data` must point to `count` valid f64 values.
#[must_use]
pub unsafe fn bgw_reduce_multi_f64(data: *const f64, count: usize) -> Option<ReduceMultiF64> {
    if count == 0 {
        return Some(ReduceMultiF64 {
            sum: 0.0,
            min: 0.0,
            max: 0.0,
            count: 0,
        });
    }
    let slice = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), count * 8) };
    let (handle, seg) = unsafe { create_dsm_with_data(slice)? };

    let req = GpuRequest {
        op: GpuOp::ReduceMultiF64 as u32,
        dsm_handle: handle,
        n_rows: count as u64,
        input_offset: 0,
        input_len: (count * 8) as u64,
        ..GpuRequest::default()
    };

    let resp = unsafe { submit_and_wait(&req) };
    // SAFETY: seg is a valid DSM segment we created.
    unsafe { pg_sys::dsm_detach(seg) };

    let r = resp.filter(|r| r.status == 0)?;
    Some(ReduceMultiF64 {
        sum: r.scalar_f64,
        min: r.scalar2_f64,
        max: r.scalar3_f64,
        count: r.scalar_i64,
    })
}

/// Fused multi-aggregate reduce (i64) via BGW.
///
/// # Safety
///
/// `data` must point to `count` valid i64 values.
#[must_use]
pub unsafe fn bgw_reduce_multi_i64(data: *const i64, count: usize) -> Option<ReduceMultiI64> {
    if count == 0 {
        return Some(ReduceMultiI64 {
            sum: 0,
            min: 0,
            max: 0,
            count: 0,
        });
    }
    let slice = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), count * 8) };
    let (handle, seg) = unsafe { create_dsm_with_data(slice)? };

    let req = GpuRequest {
        op: GpuOp::ReduceMultiI64 as u32,
        dsm_handle: handle,
        n_rows: count as u64,
        input_offset: 0,
        input_len: (count * 8) as u64,
        ..GpuRequest::default()
    };

    let resp = unsafe { submit_and_wait(&req) };
    // SAFETY: seg is a valid DSM segment we created.
    unsafe { pg_sys::dsm_detach(seg) };

    let r = resp.filter(|r| r.status == 0)?;
    // Worker packed i64 SUM/MIN/MAX into the f64 bit pattern slots.
    let sum: i64 = i64::from_ne_bytes(r.scalar_f64.to_ne_bytes());
    let min: i64 = i64::from_ne_bytes(r.scalar2_f64.to_ne_bytes());
    let max: i64 = i64::from_ne_bytes(r.scalar3_f64.to_ne_bytes());
    Some(ReduceMultiI64 {
        sum,
        min,
        max,
        count: r.scalar_i64,
    })
}

/// Sort key-value f32 via BGW. Keys and indices are modified in-place.
///
/// # Safety
///
/// `keys` and `indices` must point to `count` valid elements each.
pub unsafe fn bgw_sort_kv_f32(keys: *mut f32, indices: *mut u32, count: usize) -> bool {
    let keys_bytes = unsafe { std::slice::from_raw_parts(keys as *const u8, count * 4) };
    let idx_bytes = unsafe { std::slice::from_raw_parts(indices as *const u8, count * 4) };

    let Some((handle, seg, off1, off2)) =
        (unsafe { create_dsm_with_two_arrays(keys_bytes, idx_bytes) })
    else {
        return false;
    };

    let req = GpuRequest {
        op: GpuOp::SortKvF32 as u32,
        dsm_handle: handle,
        n_rows: count as u64,
        input_offset: off1,
        input_len: (count * 4) as u64,
        input2_offset: off2,
        input2_len: (count * 4) as u64,
        ..GpuRequest::default()
    };

    let resp = unsafe { submit_and_wait(&req) };

    if let Some(r) = resp.filter(|r| r.status == 0) {
        // Copy sorted results back from DSM.
        let base = unsafe { pg_sys::dsm_segment_address(seg) as *const u8 };
        unsafe {
            std::ptr::copy_nonoverlapping(base.add(off1 as usize), keys.cast::<u8>(), count * 4);
            std::ptr::copy_nonoverlapping(base.add(off2 as usize), indices.cast::<u8>(), count * 4);
        }
        let _ = r;
        // SAFETY: seg is a valid DSM segment we created.
        unsafe { pg_sys::dsm_detach(seg) };
        pgrx::log!("pg_accel: bgw_sort_kv_f32 succeeded, copied {count} elements back");
        true
    } else {
        pgrx::log!(
            "pg_accel: bgw_sort_kv_f32 FAILED, resp={:?}",
            resp.map(|r| r.status)
        );
        // SAFETY: seg is a valid DSM segment we created.
        unsafe { pg_sys::dsm_detach(seg) };
        false
    }
}

/// Sort key-value f64 via BGW. Keys and indices are modified in-place.
///
/// # Safety
///
/// `keys` and `indices` must point to `count` valid elements each.
pub unsafe fn bgw_sort_kv_f64(keys: *mut f64, indices: *mut u32, count: usize) -> bool {
    let keys_bytes = unsafe { std::slice::from_raw_parts(keys as *const u8, count * 8) };
    let idx_bytes = unsafe { std::slice::from_raw_parts(indices as *const u8, count * 4) };

    let Some((handle, seg, off1, off2)) =
        (unsafe { create_dsm_with_two_arrays(keys_bytes, idx_bytes) })
    else {
        return false;
    };

    let req = GpuRequest {
        op: GpuOp::SortKvF64 as u32,
        dsm_handle: handle,
        n_rows: count as u64,
        input_offset: off1,
        input_len: (count * 8) as u64,
        input2_offset: off2,
        input2_len: (count * 4) as u64,
        ..GpuRequest::default()
    };

    let resp = unsafe { submit_and_wait(&req) };

    if let Some(r) = resp.filter(|r| r.status == 0) {
        // SAFETY: DSM segment is valid; copy sorted results back.
        let base = unsafe { pg_sys::dsm_segment_address(seg) as *const u8 };
        unsafe {
            std::ptr::copy_nonoverlapping(base.add(off1 as usize), keys.cast::<u8>(), count * 8);
            std::ptr::copy_nonoverlapping(base.add(off2 as usize), indices.cast::<u8>(), count * 4);
        }
        let _ = r;
        // SAFETY: seg is a valid DSM segment we attached.
        unsafe { pg_sys::dsm_detach(seg) };
        true
    } else {
        // SAFETY: seg is a valid DSM segment we attached.
        unsafe { pg_sys::dsm_detach(seg) };
        false
    }
}

/// Check if the GPU BGW is running and available.
pub fn bgw_is_available() -> bool {
    let state = GPU_BGW.share();
    let pid = state.bgw_pid.load(Ordering::Acquire);
    if pid <= 0 {
        return false;
    }
    // Verify the process is still alive via kill(pid, 0) signal check.
    // BackendPidGetProc doesn't work for BGWs without database connections.
    // SAFETY: kill with signal 0 is a standard POSIX liveness check.

    (unsafe { libc::kill(pid, 0) } == 0)
}
