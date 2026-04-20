//! GPU hash-join `CustomPath` injection.
//!
//! The active implementation lives in [`super::join_pathlist`]; this file is
//! the future home of the hashjoin-specific injector once the split is
//! finished.
//!
//! ## parallel_safe policy
//!
//! `parallel_safe = outer.parallel_safe && inner.parallel_safe`. When either
//! side is not parallel-safe the join cannot be executed inside a worker.

// (intentionally empty — active logic still lives in join_pathlist.rs)
