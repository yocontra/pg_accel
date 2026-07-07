//! Phase 2 correctness tests — bridge track.
//! Owned by the phase-2 bridge agent; no other agent edits this file.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod phase2_bridge {
    #[allow(unused_imports)]
    use pgrx::prelude::*;
}
