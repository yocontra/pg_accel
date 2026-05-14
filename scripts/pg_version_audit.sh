#!/usr/bin/env bash
# Fails when repo tooling/docs drift back to a hard-coded PostgreSQL 17 default.

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
    pg_accel_bench/src/main.rs
    pg_accel_bench/src/plan_shape_test.rs
    pg_accel_bench/src/parallel_stress_test.rs
    pg_accel_bench/src/artifacts.rs
    pg_accel_bench/scripts/load_boundaries.py
)

pattern='28817|data-17|17\.log|postgresql@17|--pg17'

if grep -RInE "$pattern" -- "${targets[@]}"; then
    echo "error: hard-coded PostgreSQL 17 default found; use scripts/pg_versions.sh or a PG matrix instead" >&2
    exit 1
fi

echo "pg-version-audit: PASS"
