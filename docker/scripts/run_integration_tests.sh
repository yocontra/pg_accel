#!/usr/bin/env bash
set -euo pipefail

DB_HOST="${DB_HOST:-localhost}"
DB_PORT="${DB_PORT:-5488}"
DB_USER="${DB_USER:-postgres}"
DB_NAME="${DB_NAME:-pgaccel_shared}"
LOCK_FILE="/tmp/.pgaccel_reload.lock"
TESTS_DIR="$(dirname "$0")/../tests"
FAILURES=0
TOTAL=0

# Acquire shared flock if lock file exists
exec 9>"$LOCK_FILE"
flock -s 9

for test_file in "$TESTS_DIR"/*.sql; do
    [ -f "$test_file" ] || continue
    test_name=$(basename "$test_file")
    TOTAL=$((TOTAL + 1))

    # Run with pg_accel ON
    result_on=$(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
        -v ON_ERROR_STOP=1 -t -A \
        -c "SET pg_accel.enabled = on;" -f "$test_file" 2>&1) || {
        echo "FAIL: $test_name (error with pg_accel ON)"
        FAILURES=$((FAILURES + 1))
        continue
    }

    # Run with pg_accel OFF
    result_off=$(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
        -v ON_ERROR_STOP=1 -t -A \
        -c "SET pg_accel.enabled = off;" -f "$test_file" 2>&1) || {
        echo "FAIL: $test_name (error with pg_accel OFF)"
        FAILURES=$((FAILURES + 1))
        continue
    }

    # Compare
    if [ "$result_on" = "$result_off" ]; then
        echo "PASS: $test_name"
    else
        echo "FAIL: $test_name (results differ)"
        echo "  ON:  $(echo "$result_on" | head -3)"
        echo "  OFF: $(echo "$result_off" | head -3)"
        FAILURES=$((FAILURES + 1))
    fi
done

# Release flock
exec 9>&-

echo ""
echo "$((TOTAL - FAILURES))/$TOTAL tests passed"
[ $FAILURES -eq 0 ] && exit 0 || exit 1
