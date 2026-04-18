//! Late-materialization helpers.
//!
//! Column-at-a-time deserialization and tuple extraction shared across
//! scan, join, agg, and preagg executors.

pub mod deser;
pub mod tuple_extract;
