//! Phase 2 correctness tests — window track.
//! Owned by the phase-2 window agent; no other agent edits this file.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod phase2_window {
    #[allow(unused_imports)]
    use pgrx::prelude::*;
}
