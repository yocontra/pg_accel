#!/usr/bin/env bash
set -euo pipefail

source scripts/pg_versions.sh
pg="${1:-$(pg_accel_default_pg_major)}"
min_lines="${COVERAGE_MIN_LINES:-90}"
artifact_dir="${COVERAGE_ARTIFACT_DIR:-artifacts/coverage}"
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
features="pg${pg} pg_test"
test_threads="${RUST_TEST_THREADS:-1}"
cat > "$artifact_dir/coverage-scope.txt" <<EOF
Scope: Rust workspace coverage from cargo-llvm-cov.
PostgreSQL major: ${pg}
Features: ${features}
Rust test threads: ${test_threads}
Command: cargo llvm-cov --workspace --locked --no-default-features --features "${features}" --all-targets --no-report -- --test-threads=${test_threads}

This artifact includes Rust code reached by normal Rust tests and pgrx pg_test
tests. It does not instrument pgaccel-kernels C++/SYCL sources, standalone SQL
harness files, shell scripts, benchmark artifacts, or GPU runtime/toolchain
code. The release coverage gate is not satisfied until this Rust gate reaches
the threshold and separate C++/SQL coverage or equivalent release evidence is
published.
EOF
scripts/setup_pg_extensions.sh "$pg"
cargo llvm-cov clean --workspace
RUST_TEST_THREADS="$test_threads" cargo llvm-cov \
    --workspace \
    --locked \
    --no-default-features \
    --features "$features" \
    --all-targets \
    --no-report \
    -- \
    --test-threads="$test_threads"

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
