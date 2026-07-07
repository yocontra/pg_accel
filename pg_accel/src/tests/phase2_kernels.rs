//! Phase 2 correctness tests — kernels track.
//! Owned by the phase-2 kernels agent; no other agent edits this file.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod phase2_kernels {
    #[allow(unused_imports)]
    use pgrx::prelude::*;
}
