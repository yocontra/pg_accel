//! Phase 2 engine-hardening integration tests (Agent 2F).
//!
//! These `#[pg_test]`s run against a live PostgreSQL backend and exercise the
//! runtime-observable behaviour of the Phase 2 fixes in `lib.rs`,
//! `engine/gucs.rs`, `engine/thread_budget.rs`, `engine/registry.rs`, and
//! `engine/stats.rs`. Pure-logic coverage (NaN clamps, saturating budget
//! math) lives in `#[test]` unit tests co-located with each source module;
//! this file covers the SQL / backend surface those units cannot reach.

// NOTE: the module MUST be named `tests` — the pgrx test runner hard-codes the
// SQL schema `tests` when invoking each `#[pg_test]` wrapper
// (`pgrx-tests/src/framework.rs`: `let schema = "tests";`). A differently named
// `#[pg_schema]` module would place the generated SQL functions in the wrong
// schema and every test would fail with "function tests.<name>() does not
// exist". pgrx emits `CREATE SCHEMA IF NOT EXISTS tests`, so this coexists with
// the sibling `tests` module in `mod.rs`.
#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    // -- Fix 2: min_batch_size can be RAISED, not only lowered ----------------

    #[pg_test]
    fn min_batch_size_can_be_raised_above_default() {
        // The default is 65536. Before the fix, max_val == default, so this
        // SET was rejected — meaning the GPU-dispatch floor could only ever be
        // lowered (the anti-cheat rule #3 direction). It must now succeed.
        Spi::run("SET pg_accel.min_batch_size = 1000000")
            .expect("raising min_batch_size above the 65536 default must succeed");
        let v = Spi::get_one::<i32>("SELECT current_setting('pg_accel.min_batch_size')::int4")
            .ok()
            .flatten()
            .expect("min_batch_size should read back");
        assert_eq!(v, 1_000_000);

        // The documented new ceiling (16 MiB rows) must be accepted exactly.
        Spi::run("SET pg_accel.min_batch_size = 16777216")
            .expect("min_batch_size = 16777216 (new max) must be accepted");
    }

    #[pg_test]
    fn min_batch_size_rejects_above_new_ceiling() {
        use pgrx::prelude::{PgSqlErrorCode, PgTryBuilder};
        // One past the ceiling must be rejected by PG's range check.
        let rejected = PgTryBuilder::new(|| {
            Spi::run("SET pg_accel.min_batch_size = 16777217")
                .expect("SET should either succeed or raise INVALID_PARAMETER_VALUE");
            false
        })
        .catch_when(PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, |_| true)
        .execute();
        assert!(
            rejected,
            "min_batch_size = 16777217 must be rejected; ceiling is 16777216"
        );
    }

    // -- Fix 1: cost_multiplier range is enforced at SET ----------------------

    #[pg_test]
    fn cost_multiplier_range_enforced() {
        use pgrx::prelude::{PgSqlErrorCode, PgTryBuilder};
        // In-range values succeed.
        Spi::run("SET pg_accel.cost_multiplier = 2.5").expect("in-range cost_multiplier");
        // Above the registered 10.0 ceiling is rejected.
        let hi = PgTryBuilder::new(|| {
            Spi::run("SET pg_accel.cost_multiplier = 20.0").expect("SET");
            false
        })
        .catch_when(PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, |_| true)
        .execute();
        assert!(hi, "cost_multiplier = 20.0 must be rejected (ceiling 10.0)");
        // Below the 0.1 floor is rejected.
        let lo = PgTryBuilder::new(|| {
            Spi::run("SET pg_accel.cost_multiplier = 0.01").expect("SET");
            false
        })
        .catch_when(PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, |_| true)
        .execute();
        assert!(lo, "cost_multiplier = 0.01 must be rejected (floor 0.1)");
    }

    // -- Fix 5: pg_accel_reset_stats resets the process-wide atomics ----------

    #[pg_test]
    fn reset_stats_zeros_process_atomics() {
        use crate::engine::stats;

        // Prime the atomic-backed counters that a trivial catalog query will
        // not otherwise disturb (GPU-cache and degenerate-guard counters only
        // move on real dispatch), so the reset assertion is deterministic.
        stats::increment_gpu_cache_hit();
        stats::increment_gpu_cache_miss();
        stats::increment_gpu_cache_miss();
        stats::increment_degenerate_guard();

        assert!(stats::read_gpu_cache_hits() >= 1);
        assert!(stats::read_gpu_cache_misses() >= 1);
        assert!(stats::read_degenerate_guard() >= 1);

        // Before the fix, reset_stats cleared only the thread-locals, leaving
        // these process-wide atomics cumulative.
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset_stats should run");

        assert_eq!(
            stats::read_gpu_cache_hits(),
            0,
            "gpu_cache_hits must be zeroed by pg_accel_reset_stats"
        );
        assert_eq!(
            stats::read_gpu_cache_misses(),
            0,
            "gpu_cache_misses must be zeroed by pg_accel_reset_stats"
        );
        assert_eq!(
            stats::read_degenerate_guard(),
            0,
            "degenerate_guard must be zeroed by pg_accel_reset_stats"
        );
    }

    // -- Fix 1: soft_fp64_cost_multiplier stays finite and in-range -----------

    #[pg_test]
    fn soft_fp64_cost_multiplier_is_finite_and_bounded() {
        // The getter clamps to [1.0, 64.0] and rejects non-finite inputs.
        // Whatever the current session setting, the effective value must be a
        // finite number inside the documented band.
        let m = crate::soft_fp64_cost_multiplier();
        assert!(m.is_finite(), "soft_fp64_cost_multiplier must be finite");
        assert!(
            (1.0..=64.0).contains(&m),
            "soft_fp64_cost_multiplier {m} out of [1.0, 64.0]"
        );
    }
}
