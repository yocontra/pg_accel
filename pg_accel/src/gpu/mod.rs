//! GPU kernel bridge and fallback stubs.
//!
//! Re-exports a unified API so callers don't need `cfg` at every call-site.

#[cfg(feature = "gpu")]
mod bridge;

#[cfg(not(feature = "gpu"))]
mod fallback;

pub mod three_layer;

#[cfg(test)]
mod three_layer_tests;

// Re-export types from whichever module is active.
#[cfg(feature = "gpu")]
pub use bridge::{
    PgaccelDeviceInfo, PgaccelGeomType, PgaccelGeometry, PgaccelPlatformCaps, PgaccelStatus,
};

#[cfg(not(feature = "gpu"))]
pub use fallback::{
    PgaccelDeviceInfo, PgaccelGeomType, PgaccelGeometry, PgaccelPlatformCaps, PgaccelStatus,
};

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

/// Run the GPU three-layer spatial intersection pipeline.
///
/// Returns `(definite_true, definite_false, uncertain)` index vectors,
/// or `None` if no GPU device is available (caller should use CPU fallback).
#[allow(clippy::similar_names)]
pub fn spatial_intersects_gpu(
    geoms_a: &[PgaccelGeometry],
    geoms_b: &[PgaccelGeometry],
) -> Option<(Vec<u32>, Vec<u32>, Vec<u32>)> {
    let count = geoms_a.len().min(geoms_b.len());
    if count == 0 {
        return Some((Vec::new(), Vec::new(), Vec::new()));
    }

    // Allocate output buffers sized for worst case (all pairs in one bucket).
    let mut dt_pairs = vec![0u32; count];
    let mut df_pairs = vec![0u32; count];
    let mut uc_pairs = vec![0u32; count];
    let mut dt_count: usize = 0;
    let mut df_count: usize = 0;
    let mut uc_count: usize = 0;

    #[cfg(feature = "gpu")]
    {
        // SAFETY: geoms arrays are valid slices, output buffers are
        // pre-allocated to `count` elements. The C function writes at
        // most `count` entries into each output buffer.
        let status = unsafe {
            bridge::pgaccel_spatial_intersects(
                geoms_a.as_ptr(),
                geoms_a.len(),
                geoms_b.as_ptr(),
                geoms_b.len(),
                dt_pairs.as_mut_ptr(),
                std::ptr::addr_of_mut!(dt_count),
                df_pairs.as_mut_ptr(),
                std::ptr::addr_of_mut!(df_count),
                uc_pairs.as_mut_ptr(),
                std::ptr::addr_of_mut!(uc_count),
            )
        };
        if !status.is_ok() {
            return None; // GPU call failed, caller should use CPU fallback.
        }
    }

    #[cfg(not(feature = "gpu"))]
    {
        let status = fallback::pgaccel_spatial_intersects(
            geoms_a.as_ptr(),
            geoms_a.len(),
            geoms_b.as_ptr(),
            geoms_b.len(),
            dt_pairs.as_mut_ptr(),
            std::ptr::addr_of_mut!(dt_count),
            df_pairs.as_mut_ptr(),
            std::ptr::addr_of_mut!(df_count),
            uc_pairs.as_mut_ptr(),
            std::ptr::addr_of_mut!(uc_count),
        );
        if !status.is_ok() {
            return None;
        }
    }

    dt_pairs.truncate(dt_count);
    df_pairs.truncate(df_count);
    uc_pairs.truncate(uc_count);

    Some((dt_pairs, df_pairs, uc_pairs))
}
