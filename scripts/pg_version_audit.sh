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

    python3 - "$root" "$view" <<'PY'
import sys
import tomllib
from pathlib import Path

root = Path(sys.argv[1])
view = sys.argv[2]


def fail(message: str) -> None:
    raise SystemExit(f"error: {view} {message}")


tool_versions_path = root / ".tool-versions"
try:
    tool_lines = tool_versions_path.read_text(encoding="utf-8").splitlines()
except OSError as error:
    fail(f"could not read .tool-versions: {error}")

rust_entries = []
for line in tool_lines:
    stripped = line.strip()
    if not stripped or stripped.startswith("#"):
        continue
    fields = stripped.split()
    if fields[0] == "rust":
        rust_entries.append(fields[1:])
if rust_entries != [["1.96.0"]]:
    fail(f"must contain exactly one Rust tool entry at 1.96.0, found {rust_entries!r}")

cargo_toml_path = root / "pg_accel" / "Cargo.toml"
try:
    with cargo_toml_path.open("rb") as cargo_toml:
        manifest = tomllib.load(cargo_toml)
except (OSError, tomllib.TOMLDecodeError) as error:
    fail(f"could not parse pg_accel/Cargo.toml: {error}")

dependencies = manifest.get("dependencies")
if not isinstance(dependencies, dict) or dependencies.get("pgrx") != "=0.19.1":
    fail("[dependencies].pgrx must equal '=0.19.1'")

dev_dependencies = manifest.get("dev-dependencies")
if not isinstance(dev_dependencies, dict) or dev_dependencies.get("pgrx-tests") != "=0.19.1":
    fail("[dev-dependencies].pgrx-tests must equal '=0.19.1'")

expected_features = {
    "default": ["pg18"],
    "pg18": ["pgrx/pg18", "pgrx-tests/pg18"],
    "pg19": ["pgrx/pg19", "pgrx-tests/pg19"],
    "pg_test": [],
}
features = manifest.get("features")
if features != expected_features:
    fail(f"[features] must equal {expected_features!r}, found {features!r}")
PY

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

        require_shell_value() {
            local name="$1"
            local expected="$2"
            local actual="${!name-}"
            if [ "$actual" != "$expected" ]; then
                echo "error: $view $name must equal [$expected], found [$actual]" >&2
                exit 1
            fi
        }

        require_shell_value PG_ACCEL_DEFAULT_PG_MAJOR "18"
        require_shell_value PG_ACCEL_SUPPORTED_PG_MAJORS "18 19"
        require_shell_value PG_ACCEL_SOURCE_PG_MAJORS "18 19"
        require_shell_value PG_ACCEL_PREVIEW_PG_MAJORS "19"
        require_shell_value PG_ACCEL_PGRX_VERSION "0.19.1"

        require_helper_output() {
            local label="$1"
            local expected="$2"
            shift 2
            local actual
            if ! actual="$("$@")"; then
                echo "error: $view $label returned a failure status" >&2
                exit 1
            fi
            if [ "$actual" != "$expected" ]; then
                echo "error: $view $label must output [$expected], found [$actual]" >&2
                exit 1
            fi
        }

        require_helper_status() {
            local label="$1"
            local expected="$2"
            shift 2
            local actual
            if "$@" >/dev/null 2>&1; then
                actual=0
            else
                actual=$?
            fi
            if [ "$actual" -ne "$expected" ]; then
                echo "error: $view $label must return $expected, found $actual" >&2
                exit 1
            fi
        }

        require_skip_return() {
            local pg="$1"
            local probe
            local status
            if probe="$(
                set +e
                pg_accel_skip_if_preview_without_pgrx "$pg" >/dev/null 2>&1
                printf 'returned:%s' "$?"
            )"; then
                status=0
            else
                status=$?
            fi
            if [ "$status" -ne 0 ] || [ "$probe" != "returned:1" ]; then
                echo "error: $view preview helper must normally return 1 for PostgreSQL $pg" >&2
                exit 1
            fi
        }

        require_skip_hard_exit() {
            local pg="$1"
            local probe
            local status
            if probe="$(
                set +e
                pg_accel_skip_if_preview_without_pgrx "$pg" >/dev/null 2>&1
                printf 'returned:%s' "$?"
            )"; then
                status=0
            else
                status=$?
            fi
            if [ "$status" -ne 1 ] || [ -n "$probe" ]; then
                echo "error: $view preview helper must hard-exit 1 for unsupported PostgreSQL $pg" >&2
                exit 1
            fi
        }

        require_helper_output \
            "pg_accel_supported_pg_majors" $'18\n19' \
            pg_accel_supported_pg_majors
        require_helper_output \
            "pg_accel_source_pg_majors" $'18\n19' \
            pg_accel_source_pg_majors
        require_helper_output "pg_accel_default_pg_major" "18" pg_accel_default_pg_major
        require_helper_output \
            "pg_accel_highest_buildable_pg_major" "19" \
            pg_accel_highest_buildable_pg_major
        require_helper_output \
            "pg_accel_buildable_default_pg_major" "18" \
            pg_accel_buildable_default_pg_major

        require_helper_output "pg_accel_pgrx_feature_for_pg 18" "pg18" pg_accel_pgrx_feature_for_pg 18
        require_helper_output "pg_accel_pgrx_feature_for_pg pg18" "pg18" pg_accel_pgrx_feature_for_pg pg18
        require_helper_output "pg_accel_pgrx_feature_for_pg 19" "pg19" pg_accel_pgrx_feature_for_pg 19
        require_helper_output "pg_accel_pgrx_feature_for_pg pg19" "pg19" pg_accel_pgrx_feature_for_pg pg19
        require_helper_output "pg_accel_pgrx_feature_for_pg 17" "pg17" pg_accel_pgrx_feature_for_pg 17
        require_helper_output "pg_accel_pgrx_feature_for_pg 20" "pg20" pg_accel_pgrx_feature_for_pg 20

        require_helper_status "pg_accel_is_supported_pg 18" 0 pg_accel_is_supported_pg 18
        require_helper_status "pg_accel_is_supported_pg pg18" 0 pg_accel_is_supported_pg pg18
        require_helper_status "pg_accel_is_supported_pg 19" 0 pg_accel_is_supported_pg 19
        require_helper_status "pg_accel_is_supported_pg pg19" 0 pg_accel_is_supported_pg pg19
        require_helper_status "pg_accel_is_supported_pg 17" 1 pg_accel_is_supported_pg 17
        require_helper_status "pg_accel_is_supported_pg 20" 1 pg_accel_is_supported_pg 20
        require_helper_status "pg_accel_require_supported_pg 18" 0 pg_accel_require_supported_pg 18
        require_helper_status "pg_accel_require_supported_pg 19" 0 pg_accel_require_supported_pg 19
        require_helper_status "pg_accel_require_supported_pg 17" 1 pg_accel_require_supported_pg 17
        require_helper_status "pg_accel_require_supported_pg 20" 1 pg_accel_require_supported_pg 20

        require_helper_status "pg_accel_is_preview_pg 18" 1 pg_accel_is_preview_pg 18
        require_helper_status "pg_accel_is_preview_pg pg18" 1 pg_accel_is_preview_pg pg18
        require_helper_status "pg_accel_is_preview_pg 19" 0 pg_accel_is_preview_pg 19
        require_helper_status "pg_accel_is_preview_pg pg19" 0 pg_accel_is_preview_pg pg19
        require_helper_status "pg_accel_is_preview_pg 17" 1 pg_accel_is_preview_pg 17
        require_helper_status "pg_accel_is_preview_pg 20" 1 pg_accel_is_preview_pg 20

        require_helper_status "pg_accel_pgrx_supports_pg 18" 0 pg_accel_pgrx_supports_pg 18
        require_helper_status "pg_accel_pgrx_supports_pg 19" 0 pg_accel_pgrx_supports_pg 19
        require_helper_status "pg_accel_pgrx_supports_pg 17" 1 pg_accel_pgrx_supports_pg 17
        require_helper_status "pg_accel_pgrx_supports_pg 20" 1 pg_accel_pgrx_supports_pg 20
        require_helper_status "pg_accel_require_pgrx_support 18" 0 pg_accel_require_pgrx_support 18
        require_helper_status "pg_accel_require_pgrx_support 19" 0 pg_accel_require_pgrx_support 19
        require_helper_status "pg_accel_require_pgrx_support 17" 1 pg_accel_require_pgrx_support 17
        require_helper_status "pg_accel_require_pgrx_support 20" 1 pg_accel_require_pgrx_support 20

        require_skip_return 18
        require_skip_return 19
        require_skip_hard_exit 17
        require_skip_hard_exit 20
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
