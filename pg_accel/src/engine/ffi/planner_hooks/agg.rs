//! Non-parallel aggregate path injection.
//!
//! Worker 4 fills in by extracting the non-parallel branches of
//! `pgaccel_inject_gpu_agg` (currently in `mod.rs`) into this module.
//! Calls `pg_sys::add_path` for the non-parallel `UPPERREL_GROUP_AGG` path.
