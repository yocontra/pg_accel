#!/usr/bin/env bash
set -euo pipefail

pg="${1:-17}"
min_lines="${COVERAGE_MIN_LINES:-90}"
artifact_dir="${COVERAGE_ARTIFACT_DIR:-artifacts/coverage}"

source scripts/pg_versions.sh
pg="${pg#pg}"
pg_accel_require_supported_pg "$pg"
if pg_accel_skip_if_preview_without_pgrx "$pg"; then
    echo "coverage: skipping PostgreSQL $pg because pgrx support is unavailable"
    exit 0
fi
pg_accel_require_pgrx_pg_config "$pg"

if ! cargo llvm-cov --version >/dev/null 2>&1; then
    echo "error: cargo-llvm-cov is not installed. Run: cargo install cargo-llvm-cov --locked" >&2
    exit 1
fi

mkdir -p "$artifact_dir"
cargo llvm-cov clean --workspace
cargo llvm-cov \
    --workspace \
    --locked \
    --no-default-features \
    --features "pg$pg" \
    --all-targets \
    --no-report

cargo llvm-cov report \
    --lcov \
    --output-path "$artifact_dir/lcov.info"
cargo llvm-cov report \
    --json \
    --output-path "$artifact_dir/coverage.json"
cargo llvm-cov report \
    --summary-only \
    --fail-under-lines "$min_lines" \
    | tee "$artifact_dir/coverage-summary.txt"
