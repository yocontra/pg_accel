//! Phase 2 correctness tests — engine track.
//! Owned by the phase-2 engine agent; no other agent edits this file.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod phase2_engine {
    #[allow(unused_imports)]
    use pgrx::prelude::*;
}
