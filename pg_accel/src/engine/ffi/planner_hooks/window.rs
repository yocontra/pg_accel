//! GPU window function `CustomPath` injection.
//!
//! The active implementation of `pgaccel_inject_gpu_window` lives in
//! [`super`] module top-level; this file is the future home of that function
//! once the split is finished.
//!
//! ## parallel_safe policy
//!
//! Window functions need the whole partition resident in one worker's address
//! space, so for now we pass through the input path's `parallel_safe` flag.
//! Once per-spec parallel-aware partitioning (PG's `is_parallel_safe()` walker
//! over `WindowFunc` nodes) is hooked up, this can become `true` for
//! partitions that happen to align with the parallel worker boundary.

// (intentionally empty — active logic still lives in mod.rs)
