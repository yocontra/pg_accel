//! Phase 2 correctness tests — window track.
//! Focused window integration coverage.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod phase2_window {
    #[allow(unused_imports)]
    use pgrx::prelude::*;
}
