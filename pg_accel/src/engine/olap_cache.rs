//! Temporary compatibility facade for the pre-v2 shape-specific caches.
//!
//! New code must use [`crate::engine::residency`]. Phase 5E removes this facade
//! together with the shape-specific loaders and recognizers.

pub use crate::engine::residency::legacy::*;
