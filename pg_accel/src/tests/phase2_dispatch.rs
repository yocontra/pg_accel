//! Phase 2 correctness tests — dispatch track.
//! Owned by the phase-2 dispatch agent; no other agent edits this file.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod phase2_dispatch {
    #[allow(unused_imports)]
    use pgrx::prelude::*;
}
