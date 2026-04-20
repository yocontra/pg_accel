//! GPU sort `CustomPath` injection.
//!
//! Scan-level sort injection currently lives in
//! [`super::rel_pathlist::try_inject_gpu_sort_path`]. This module will own
//! that logic once the split is finished.
//!
//! ## parallel_safe policy
//!
//! `parallel_safe = (*input_path).parallel_safe`. GPU sort is always
//! parallel-safe given a parallel-safe child (it's a pure projection of
//! already-materialised rows).

// (intentionally empty — active logic still lives in rel_pathlist.rs)
