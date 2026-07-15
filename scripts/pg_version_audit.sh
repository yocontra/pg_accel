#!/usr/bin/env bash
# Enforce the supported PostgreSQL/pgrx toolchain and release-matrix policy.

set -euo pipefail

cd "$(dirname "$0")/.."

# Scan current normative surfaces. Historical changelogs and review backlogs
# are intentionally excluded so past-version evidence remains intact.
targets=(
    .tool-versions
    .claude/rules
    .claude/skills
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

audit_banned_pattern() {
    local pattern="$1"
    local message="$2"
    shift 2
    local found=0
    local status

    if grep -RIniE "$pattern" -- "$@"; then
        found=1
    else
        status=$?
        if [ "$status" -ne 1 ]; then
            echo "error: worktree version-policy scan failed with status $status" >&2
            exit 1
        fi
    fi

    if git grep --cached -n -i -E "$pattern" -- "$@"; then
        found=1
    else
        status=$?
        if [ "$status" -ne 1 ]; then
            echo "error: Git-index version-policy scan failed with status $status" >&2
            exit 1
        fi
    fi

    if [ "$found" -ne 0 ]; then
        echo "error: $message" >&2
        exit 1
    fi
}

stale_pg_pattern='PG[[:space:]_-]*17|PostgreSQL[[:space:]_-]*17|28817|data-17|17\.log|postgresql@17|postgresql-17|--pg[[:space:]_-]*17|--features[[:space:]]+pg[[:space:]_-]*17|PG_ACCEL_PG17_VERSION|coverage-pg[[:space:]_-]*17|pg_accel_pgrx_port_for_pg[[:space:]]+17'
audit_banned_pattern \
    "$stale_pg_pattern" \
    "PostgreSQL 17 support reference found; pg_accel supports PostgreSQL 18+ only" \
    "${targets[@]}"

# The pgrx 0.16 mention in the extension skill documents an API removal; 0.18
# was the stale active toolchain pin and must not reappear as current policy.
legacy_pgrx_pattern='pgrx[- ]tests[^0-9]*0\.18|cargo-pgrx[^0-9]*0\.18|pgrx[^0-9]*0\.18'
audit_banned_pattern \
    "$legacy_pgrx_pattern" \
    "stale pgrx 0.18 toolchain reference found" \
    "${targets[@]}"

audit_banned_pattern \
    'artifacts/pg_accel--[0-9]+\.[0-9]+\.[0-9]+-pg' \
    "CI schema artifact paths must derive the package version from Cargo metadata" \
    .github/workflows

require_exact_line() {
    local root="$1"
    local view="$2"
    local file="$3"
    local line="$4"
    if [ ! -f "$root/$file" ]; then
        echo "error: $view is missing required policy file: $file" >&2
        exit 1
    fi
    if ! grep -Fqx "$line" "$root/$file"; then
        echo "error: $view $file must contain exact policy line: $line" >&2
        exit 1
    fi
}

validate_contract() {
    local root="$1"
    local view="$2"

    require_exact_line "$root" "$view" .tool-versions 'rust 1.96.0'
    require_exact_line "$root" "$view" pg_accel/Cargo.toml 'pgrx = "=0.19.1"'
    require_exact_line "$root" "$view" pg_accel/Cargo.toml 'pgrx-tests = "=0.19.1"'
    require_exact_line "$root" "$view" pg_accel/Cargo.toml 'pg18 = ["pgrx/pg18", "pgrx-tests/pg18"]'
    require_exact_line "$root" "$view" pg_accel/Cargo.toml 'pg19 = ["pgrx/pg19", "pgrx-tests/pg19"]'
    require_exact_line "$root" "$view" scripts/pg_versions.sh 'PG_ACCEL_PGRX_VERSION="${PG_ACCEL_PGRX_VERSION:-0.19.1}"'

    (
        cd "$root"
        unset PG_ACCEL_DEFAULT_PG_MAJOR
        unset PG_ACCEL_SUPPORTED_PG_MAJORS
        unset PG_ACCEL_PREVIEW_PG_MAJORS
        unset PG_ACCEL_PGRX_VERSION
        unset PG_ACCEL_SOURCE_PG_MAJORS
        export PG_ACCEL_REPO_ROOT="$root"
        source scripts/pg_versions.sh

        if ! pg_accel_require_pgrx_support 18; then
            echo "error: $view does not satisfy the PostgreSQL 18 pgrx contract" >&2
            exit 1
        fi
        if ! pg_accel_require_pgrx_support 19; then
            echo "error: $view does not satisfy the PostgreSQL 19 pgrx contract" >&2
            exit 1
        fi
        if pg_accel_skip_if_preview_without_pgrx 19; then
            echo "error: $view resolves PostgreSQL 19 through a successful preview skip" >&2
            exit 1
        fi
    )
}

preview_skip_pattern='PG_ACCEL_ENABLE_PREVIEW|SKIP: PostgreSQL .*preview'
audit_banned_pattern \
    "$preview_skip_pattern" \
    "supported PostgreSQL release targets must not have successful preview skips" \
    "${targets[@]}"

repo_root="$(pwd -P)"
index_root="$(mktemp -d "${TMPDIR:-/tmp}/pg-accel-version-audit.XXXXXX")"
cleanup() {
    rm -rf "$index_root"
}
trap cleanup EXIT

if ! git checkout-index --all --prefix="$index_root/"; then
    echo "error: could not materialize the Git index for version-policy validation" >&2
    exit 1
fi

validate_contract "$repo_root" "worktree"
validate_contract "$index_root" "Git index"

echo "pg-version-audit: PASS"
