#!/usr/bin/env bash
# Fails when repo tooling/docs drift back to hard-coded PostgreSQL 17 support.

set -euo pipefail

cd "$(dirname "$0")/.."

targets=(
    Justfile
    README.md
    CONTRIBUTING.md
    CLAUDE.md
    docs
    .github
    sql
    NOTICE
    pg_accel/Cargo.toml
    scripts/pg_source.sh
    scripts/pg_versions.sh
    scripts/coverage_gate.sh
    scripts/cuda_stress_gate.sh
    pg_accel_bench/src/h3_protection_test.rs
    pg_accel_bench/src/integration_connection.rs
    pg_accel_bench/src/main.rs
    pg_accel_bench/src/plan_shape_test.rs
    pg_accel_bench/src/parallel_stress_test.rs
    pg_accel_bench/src/artifacts.rs
    pg_accel_bench/scripts/load_boundaries.py
)

pattern='pg17|PG17|PostgreSQL 17|28817|data-17|17\.log|postgresql@17|postgresql-17|--pg17|--features pg17|PG_ACCEL_PG17_VERSION|coverage-pg17|pg_accel_pgrx_port_for_pg[[:space:]]+17'

if grep -RInE "$pattern" -- "${targets[@]}"; then
    echo "error: PostgreSQL 17 support reference found; pg_accel supports PostgreSQL 18+ only" >&2
    exit 1
fi

echo "pg-version-audit: PASS"
