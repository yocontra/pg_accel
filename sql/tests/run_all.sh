#!/usr/bin/env bash
# run_all.sh — Run all pg_accel integration test SQL files and report results.
#
# Usage: ./run_all.sh [connection_string]
#   connection_string: psql-compatible connection string. If omitted, the
#   script uses the pgrx cluster for PG_ACCEL_PG_MAJOR or the repo default.
#
# Each .sql file is self-contained: it sets up data, compares accel ON vs OFF
# results internally via DO $$ blocks, and raises an EXCEPTION on mismatch.
# A test PASSes if psql exits 0; it FAILs otherwise.

set -euo pipefail

TESTS_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$TESTS_DIR/../.." && pwd)"

# Parse connection params
if [ $# -ge 1 ]; then
    CONNSTR="$1"
else
    # shellcheck source=/dev/null
    source "$REPO_ROOT/scripts/pg_versions.sh"
    PG_MAJOR="${PG_ACCEL_PG_MAJOR:-$(pg_accel_default_pg_major)}"
    DB_HOST="${DB_HOST:-localhost}"
    DB_PORT="${DB_PORT:-$(pg_accel_pgrx_port_for_pg "$PG_MAJOR")}"
    DB_NAME="${DB_NAME:-postgres}"
    CONNSTR="host=$DB_HOST port=$DB_PORT dbname=$DB_NAME"
    if [ -n "${DB_USER:-}" ]; then
        CONNSTR="$CONNSTR user=$DB_USER"
    fi
    if [ -n "${DB_PASSWORD:-}" ]; then
        CONNSTR="$CONNSTR password=$DB_PASSWORD"
    fi
fi

FAILURES=0
PASSES=0
TOTAL=0
FAILED_TESTS=()

sql_test_strict() {
    case "${PG_ACCEL_SQL_TEST_REQUIRE_EXTENSION:-}" in
        1|true|TRUE|yes|YES|on|ON)
            return 0
            ;;
    esac
    case "${PG_ACCEL_RELEASE_MODE:-}" in
        1|true|TRUE|yes|YES|on|ON)
            return 0
            ;;
    esac
    case "${CI:-}" in
        1|true|TRUE|yes|YES|on|ON)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

redacted_connstr() {
    printf '%s\n' "$CONNSTR" | sed -E 's/(password=)[^ ]+/\1<redacted>/g'
}

has_pass_marker() {
    grep -Eq '(^PASS:|^[[:alnum:]_]+ PASSED$|^=== .* PASSED ===$)'
}

echo "========================================"
echo " pg_accel integration tests"
echo "========================================"
echo ""

# Check if pg_accel extension is installed. Local developer runs may skip when
# the extension is not installed; CI and release gates must fail loudly.
set +e
extension_check_output="$(psql "$CONNSTR" -v ON_ERROR_STOP=1 -tAc "SELECT 1 FROM pg_extension WHERE extname = 'pg_accel'" 2>&1)"
extension_check_status=$?
set -e
if [ "$extension_check_status" -ne 0 ]; then
    if sql_test_strict; then
        echo "ERROR: unable to query pg_accel extension state for SQL integration tests." >&2
        echo "       connection: $(redacted_connstr)" >&2
        echo "$extension_check_output" | tail -10 | sed 's/^/       | /' >&2
        exit "$extension_check_status"
    fi
    echo "SKIP: unable to query pg_accel extension state. Skipping all tests."
    echo "$extension_check_output" | tail -5 | sed 's/^/  | /'
    exit 0
fi

if ! printf '%s\n' "$extension_check_output" | grep -Eq '^[[:space:]]*1[[:space:]]*$'; then
    if sql_test_strict; then
        echo "ERROR: pg_accel extension is not installed for SQL integration tests." >&2
        echo "       connection: $(redacted_connstr)" >&2
        echo "       install it first with: just install-pg-accel \${PG_ACCEL_PG_MAJOR:-}" >&2
        exit 1
    fi
    echo "SKIP: pg_accel extension is not installed. Skipping all tests."
    exit 0
fi

for test_file in "$TESTS_DIR"/[0-9]*.sql; do
    [ -f "$test_file" ] || continue
    test_name=$(basename "$test_file")
    TOTAL=$((TOTAL + 1))

    # Run the test file; it will RAISE EXCEPTION on failure
    if output=$(psql "$CONNSTR" \
        -v ON_ERROR_STOP=1 \
        -f "$test_file" 2>&1); then

        # Check for an explicit PASS/PASSED echo in output.
        if has_pass_marker <<<"$output"; then
            PASSES=$((PASSES + 1))
            echo "PASS: $test_name"
        elif sql_test_strict; then
            FAILURES=$((FAILURES + 1))
            FAILED_TESTS+=("$test_name (no PASS marker)")
            echo "FAIL: $test_name (no PASS marker)"
            echo "$output" | tail -5 | sed 's/^/  | /'
            echo ""
        else
            # No PASS marker but no error either — treat as pass with warning
            PASSES=$((PASSES + 1))
            echo "PASS: $test_name (no explicit PASS marker)"
        fi
    else
        FAILURES=$((FAILURES + 1))
        FAILED_TESTS+=("$test_name")
        echo "FAIL: $test_name"
        # Show last few lines of output for diagnosis
        echo "$output" | tail -5 | sed 's/^/  | /'
        echo ""
    fi
done

if [ "$TOTAL" -eq 0 ]; then
    if sql_test_strict; then
        echo "ERROR: no SQL integration test files found in $TESTS_DIR." >&2
        exit 1
    fi
    echo "SKIP: no SQL integration test files found in $TESTS_DIR."
    exit 0
fi

echo ""
echo "========================================"
echo " Results: $PASSES passed, $FAILURES failed, $TOTAL total"
echo "========================================"

if [ $FAILURES -gt 0 ]; then
    echo ""
    echo "Failed tests:"
    for t in "${FAILED_TESTS[@]}"; do
        echo "  - $t"
    done
    exit 1
fi

exit 0
