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
ARTIFACT_DIR="${PG_ACCEL_SQL_TEST_ARTIFACT_DIR:-}"
EXPECTED_KERNEL_TIMEOUT_MS="${PG_ACCEL_SQL_TEST_EXPECT_KERNEL_TIMEOUT_MS:-}"
RESULTS_FILE=""
SESSION_PROFILE_FILE=""

if [ -n "$ARTIFACT_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/logs"
    RESULTS_FILE="$ARTIFACT_DIR/results.tsv"
    SESSION_PROFILE_FILE="$ARTIFACT_DIR/session-profile.tsv"
    printf 'file\tstatus\texit_code\tlog\n' > "$RESULTS_FILE"
fi

record_result() {
    local test_name="$1"
    local status="$2"
    local exit_code="$3"
    local output="$4"
    [ -n "$RESULTS_FILE" ] || return 0
    local log="logs/${test_name}.log"
    printf '%s\n' "$output" > "$ARTIFACT_DIR/$log"
    printf '%s\t%s\t%s\t%s\n' "$test_name" "$status" "$exit_code" "$log" >> "$RESULTS_FILE"
}

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

completion_marker_count() {
    local test_name="$1"
    local stem="${test_name%.sql}"
    grep -Fxc "PGACCEL_FILE_OK:${stem}" || true
}

has_forbidden_release_evidence() {
    grep -Eiq '(^|[[:space:]])WARNING:|(^|[^[:alnum:]_])SKIP(PED)?([^[:alnum:]_]|$)|caught.*exception'
}

capture_expected_session_profile() {
    [ -n "$EXPECTED_KERNEL_TIMEOUT_MS" ] || return 0
    if ! [[ "$EXPECTED_KERNEL_TIMEOUT_MS" =~ ^[0-9]+$ ]]; then
        echo "ERROR: expected SQL test kernel timeout must be an integer." >&2
        return 2
    fi

    local output status expected
    set +e
    output="$(psql "$CONNSTR" -X -v ON_ERROR_STOP=1 -At -F $'\t' \
        -c "SELECT name, setting, COALESCE(unit, ''), source FROM pg_settings WHERE name = 'pg_accel.kernel_timeout_ms'" \
        2>&1)"
    status=$?
    set -e
    expected=$'pg_accel.kernel_timeout_ms\t'"${EXPECTED_KERNEL_TIMEOUT_MS}"$'\tms\tclient'
    if [ "$status" -ne 0 ] || [ "$output" != "$expected" ]; then
        echo "ERROR: SQL test session does not have the exact expected kernel timeout profile." >&2
        printf '%s\n' "$output" | tail -5 | sed 's/^/       | /' >&2
        return 1
    fi
    if [ -n "$SESSION_PROFILE_FILE" ]; then
        printf '%s\n' "$output" > "$SESSION_PROFILE_FILE"
    fi
    echo "SQL session profile: pg_accel.kernel_timeout_ms=${EXPECTED_KERNEL_TIMEOUT_MS}ms source=client"
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

capture_expected_session_profile

for test_file in "$TESTS_DIR"/[0-9]*.sql; do
    [ -f "$test_file" ] || continue
    test_name=$(basename "$test_file")
    TOTAL=$((TOTAL + 1))

    # Run the test file; it will RAISE EXCEPTION on failure. In artifact mode,
    # retain the complete psql output and exit status for the SQL coverage
    # inventory instead of reducing evidence to a terminal-only pass count.
    set +e
    output=$(psql "$CONNSTR" \
        -v ON_ERROR_STOP=1 \
        -f "$test_file" 2>&1)
    psql_status=$?
    set -e
    result_status="fail"
    if [ "$psql_status" -eq 0 ]; then
        # A file completion marker proves only that psql reached the end of the
        # file. Semantic assertion IDs are validated separately against the
        # fixed manifest and never inferred from this marker.
        marker_count="$(completion_marker_count "$test_name" <<<"$output")"
        if sql_test_strict && has_forbidden_release_evidence <<<"$output"; then
            FAILURES=$((FAILURES + 1))
            FAILED_TESTS+=("$test_name (warning/skip/caught exception evidence)")
            echo "FAIL: $test_name (warning/skip/caught exception evidence)"
            echo "$output" | tail -10 | sed 's/^/  | /'
            echo ""
        elif [ "$marker_count" -eq 1 ]; then
            PASSES=$((PASSES + 1))
            result_status="pass"
            echo "PASS: $test_name"
        elif sql_test_strict; then
            FAILURES=$((FAILURES + 1))
            FAILED_TESTS+=("$test_name (file completion markers=$marker_count)")
            echo "FAIL: $test_name (file completion markers=$marker_count)"
            echo "$output" | tail -5 | sed 's/^/  | /'
            echo ""
        else
            # Local non-strict runs keep the historical permissive behavior.
            PASSES=$((PASSES + 1))
            result_status="pass"
            echo "PASS: $test_name (completion markers=$marker_count)"
        fi
    else
        FAILURES=$((FAILURES + 1))
        FAILED_TESTS+=("$test_name")
        echo "FAIL: $test_name"
        # Show last few lines of output for diagnosis
        echo "$output" | tail -5 | sed 's/^/  | /'
        echo ""
    fi
    record_result "$test_name" "$result_status" "$psql_status" "$output"
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
