//! GPU kernel bridge and fallback stubs.

#[cfg(feature = "gpu")]
pub mod bridge;

#[cfg(not(feature = "gpu"))]
pub mod fallback;
