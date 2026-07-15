#!/usr/bin/env bash
# Enforce the supported PostgreSQL/pgrx toolchain and release-matrix policy.

set -euo pipefail

cd "$(dirname "$0")/.."

targets=(
    .tool-versions
    ARCHITECTURE.md
    Justfile
    NOTICE
    README.md
    CONTRIBUTING.md
    CLAUDE.md
    docs
    .github
    sql
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

legacy_pgrx_pattern='pgrx[- ]tests[^0-9]*0\.18|cargo-pgrx[^0-9]*0\.18|pgrx[^0-9]*0\.18'
if grep -RInE "$legacy_pgrx_pattern" -- "${targets[@]}"; then
    echo "error: stale pgrx 0.18 toolchain reference found" >&2
    exit 1
fi

require_exact_line() {
    local file="$1"
    local line="$2"
    if ! grep -Fqx "$line" "$file"; then
        echo "error: $file must contain exact policy line: $line" >&2
        exit 1
    fi
}

require_exact_line .tool-versions 'rust 1.96.0'
require_exact_line pg_accel/Cargo.toml 'pgrx = "=0.19.1"'
require_exact_line pg_accel/Cargo.toml 'pgrx-tests = "=0.19.1"'
require_exact_line pg_accel/Cargo.toml 'pg18 = ["pgrx/pg18", "pgrx-tests/pg18"]'
require_exact_line pg_accel/Cargo.toml 'pg19 = ["pgrx/pg19", "pgrx-tests/pg19"]'
require_exact_line scripts/pg_versions.sh 'PG_ACCEL_PGRX_VERSION="${PG_ACCEL_PGRX_VERSION:-0.19.1}"'

preview_skip_pattern='PG_ACCEL_ENABLE_PREVIEW|SKIP: PostgreSQL .*preview'
if grep -RInE "$preview_skip_pattern" -- "${targets[@]}"; then
    echo "error: supported PostgreSQL release targets must not have successful preview skips" >&2
    exit 1
fi

source scripts/pg_versions.sh
pg_accel_require_pgrx_support 18
pg_accel_require_pgrx_support 19
if pg_accel_skip_if_preview_without_pgrx 19; then
    echo "error: PostgreSQL 19 resolved through the legacy successful-skip path" >&2
    exit 1
fi

echo "pg-version-audit: PASS"
