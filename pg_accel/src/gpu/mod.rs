//! GPU kernel bridge and fallback stubs.
//!
//! Re-exports a unified API so callers don't need `cfg` at every call-site.

#[cfg(feature = "gpu")]
mod bridge;

#[cfg(not(feature = "gpu"))]
mod fallback;

// Re-export types from whichever module is active.
#[cfg(feature = "gpu")]
pub use bridge::{PgaccelDeviceInfo, PgaccelPlatformCaps, PgaccelStatus};

#[cfg(not(feature = "gpu"))]
pub use fallback::{PgaccelDeviceInfo, PgaccelPlatformCaps, PgaccelStatus};

// ---------------------------------------------------------------------------
// Unified safe wrappers
// ---------------------------------------------------------------------------

/// Initialise the GPU runtime.
pub fn init() -> PgaccelStatus {
    #[cfg(feature = "gpu")]
    {
        // SAFETY: pgaccel_init is safe to call once at startup.
        unsafe { bridge::pgaccel_init() }
    }
    #[cfg(not(feature = "gpu"))]
    {
        fallback::pgaccel_init()
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
        fallback::pgaccel_shutdown()
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
        fallback::pgaccel_get_device_info()
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
        fallback::pgaccel_get_caps()
    }
}
