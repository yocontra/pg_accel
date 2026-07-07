//! Phase 2 correctness tests — cache track.
//! Owned by the phase-2 cache agent; no other agent edits this file.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod phase2_cache {
    #[allow(unused_imports)]
    use pgrx::prelude::*;
}
