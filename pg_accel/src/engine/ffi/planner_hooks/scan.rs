//! Base-relation scan `CustomPath` injection.
//!
//! The active implementation of `set_rel_pathlist_hook` lives in
//! [`super::rel_pathlist`]. This module is the future home of the
//! scan-specific injector once `rel_pathlist.rs` is split by concern.
//!
//! ## parallel_safe policy
//!
//! Base heap scans are inherently parallel-safe: the underlying `SeqScan` /
//! `IndexScan` path has `parallel_safe=true` and the `CustomPath` simply
//! wraps it. [`super::create_custom_path`] already propagates the base path's
//! flag; no additional gating is needed here.

// (intentionally empty — active logic still lives in rel_pathlist.rs)
