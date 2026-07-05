use super::{PgaccelDeviceInfo, PgaccelPlatformCaps, PgaccelStatus, bridge};

// ---------------------------------------------------------------------------
// Unified safe wrappers
// ---------------------------------------------------------------------------

/// Initialise the GPU runtime.
fn init() -> PgaccelStatus {
    // SAFETY: pgaccel_init is safe to call once at startup.
    unsafe { bridge::pgaccel_init() }
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
    // SAFETY: pgaccel_prefork_warmup does not spawn threads and is
    // safe to call from the postmaster during _PG_init().
    unsafe { bridge::pgaccel_prefork_warmup() }
}

/// Tear down the GPU runtime.
#[allow(dead_code)] // reason: Rust wrapper for pgaccel_shutdown FFI; called at extension teardown
pub fn shutdown() -> PgaccelStatus {
    // SAFETY: pgaccel_shutdown is safe if init was called.
    unsafe { bridge::pgaccel_shutdown() }
}

/// Return information about the selected compute device.
pub fn get_device_info() -> PgaccelDeviceInfo {
    // SAFETY: pgaccel_get_device_info returns a zeroed struct if not initialised.
    unsafe { bridge::pgaccel_get_device_info() }
}

/// Human-readable device name for log messages.
#[allow(dead_code)] // reason: diagnostic API; used by future log/EXPLAIN VERBOSE callers
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
#[allow(dead_code)] // reason: Rust wrapper for pgaccel_get_caps FFI; preserved as stable surface
pub fn get_caps() -> PgaccelPlatformCaps {
    // SAFETY: pgaccel_get_caps returns a zeroed struct if not initialised.
    unsafe { bridge::pgaccel_get_caps() }
}

// Historical note: two helpers used to gate every f64 dispatch on native-fp64
// hardware and have been deleted. fp64 is always available via the
// AdaptiveCpp soft-fp64 libkernel on Metal, and natively on CUDA/ROCm/L0;
// `has_native_fp64` on `PgaccelPlatformCaps` / `PgaccelDeviceInfo` is now a
// cost-model signal only, not a dispatch skip-gate. Call sites below dispatch
// the fp64 kernel unconditionally.
