//! Extension adapters that declare which SQL functions `pg_accel` can accelerate.
//!
//! Adapter constructor functions are not yet called from the main code path;
//! they will be wired in when the registry scanner is complete.

#[allow(dead_code)]
pub mod extractors;
#[allow(dead_code)]
pub mod h3;
#[allow(dead_code)]
pub mod pg_builtins;
#[allow(dead_code)]
pub mod postgis;
#[allow(dead_code)]
pub mod postgis_raster;
