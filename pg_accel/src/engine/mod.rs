//! Core engine: FFI shims, registry types, dispatch, and executor nodes.

pub mod batch;
pub mod cost;
pub mod ffi;
pub mod function_matcher;
pub mod gucs;
pub mod registry;
pub mod thread_budget;
pub mod thread_pool;
pub mod type_extractor;
