//! Shared helpers used by both `agg.rs` (non-parallel) and `partial_agg.rs`
//! (parallel):
//! - Aggref classification (`Aggref → AggOp`)
//! - Target-list walking
//! - Cost estimation
//!
//! Worker 4 fills in by extracting from the current `pgaccel_inject_gpu_agg`
//! in `mod.rs`.
