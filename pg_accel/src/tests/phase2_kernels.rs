//! Phase 2 correctness tests — kernels track.
//! Focused GPU-kernel integration coverage.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod phase2_kernels {
    #[allow(unused_imports)]
    use pgrx::prelude::*;
}
