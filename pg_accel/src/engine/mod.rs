//! Core engine: FFI shims, registry types, dispatch, and executor nodes.

pub mod batch;
pub mod columnar;
pub mod cost;
pub mod device_info;
pub mod dispatch;
pub mod executor;
pub mod expr_compiler;
pub mod ffi;
pub mod function_matcher;
pub mod gucs;
pub mod otel;
pub mod panic_hook;
pub mod registry;
pub mod stats;
pub mod thread_budget;
pub mod type_extractor;
