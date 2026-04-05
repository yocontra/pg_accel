//! Extension adapters that declare which SQL functions `pg_accel` can accelerate.
//!
//! Each sub-module exposes an `adapter()` constructor returning an
//! [`ExtensionAdapter`](crate::engine::registry::ExtensionAdapter). The
//! registry calls these during [`lazy_init`](crate::engine::registry::lazy_init)
//! and probes each adapter's `version_query` to decide whether to activate it.

#[allow(dead_code)]
pub mod extractors;
pub mod h3;
pub mod postgis;
pub mod postgis_raster;
