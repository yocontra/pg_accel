#!/usr/bin/env bash
# run_all.sh — Run all pg_accel integration test SQL files and report results.
#
# Usage: ./run_all.sh [connection_string]
#   connection_string: psql-compatible connection string (default: localhost:5488/pgaccel_shared)
#
# Each .sql file is self-contained: it sets up data, compares accel ON vs OFF
# results internally via DO $$ blocks, and raises an EXCEPTION on mismatch.
# A test PASSes if psql exits 0; it FAILs otherwise.

set -euo pipefail

TESTS_DIR="$(cd "$(dirname "$0")" && pwd)"

# Parse connection params
if [ $# -ge 1 ]; then
    CONNSTR="$1"
else
    DB_HOST="${DB_HOST:-localhost}"
    DB_PORT="${DB_PORT:-5488}"
    DB_USER="${DB_USER:-postgres}"
    DB_NAME="${DB_NAME:-pgaccel_shared}"
    CONNSTR="host=$DB_HOST port=$DB_PORT user=$DB_USER dbname=$DB_NAME"
fi

FAILURES=0
PASSES=0
TOTAL=0
FAILED_TESTS=()

echo "========================================"
echo " pg_accel integration tests"
echo "========================================"
echo ""

# Check if pg_accel extension is installed; skip gracefully if not.
if ! psql "$CONNSTR" -tAc "SELECT 1 FROM pg_extension WHERE extname = 'pg_accel'" 2>/dev/null | grep -q 1; then
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

        # Check for PASS echo in output
        if echo "$output" | grep -q "^PASS:"; then
            PASSES=$((PASSES + 1))
            echo "PASS: $test_name"
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
