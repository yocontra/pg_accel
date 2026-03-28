//! Core engine: FFI shims, registry types, dispatch, and executor nodes.

pub mod batch;
pub mod cost;
pub mod device_info;
pub mod dispatch;
pub mod dispatch_fallback;
pub mod ffi;
pub mod function_matcher;
pub mod gucs;
pub mod registry;
pub mod stats;
pub mod thread_budget;
pub mod thread_pool;
pub mod type_extractor;
