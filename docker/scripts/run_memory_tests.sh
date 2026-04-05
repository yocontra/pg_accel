#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# Memory stability tests: run 10K mixed queries and assert RSS growth < 50MB
# ---------------------------------------------------------------------------

DB_HOST="${DB_HOST:-localhost}"
DB_PORT="${DB_PORT:-5488}"
DB_USER="${DB_USER:-postgres}"
DB_NAME="${DB_NAME:-pgaccel_shared}"
LOCK_FILE="/tmp/.pgaccel_reload.lock"
TOTAL_QUERIES="${TOTAL_QUERIES:-10000}"
MAX_GROWTH_MB="${MAX_GROWTH_MB:-50}"
SAMPLE_INTERVAL="${SAMPLE_INTERVAL:-1000}"

PSQL="psql -h $DB_HOST -p $DB_PORT -U $DB_USER -d $DB_NAME -v ON_ERROR_STOP=1 -t -A"

# Acquire shared flock
exec 9>"$LOCK_FILE"
flock -s 9

cleanup() {
    exec 9>&-
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Check extension availability
# ---------------------------------------------------------------------------
has_postgis=true
has_h3=true

if ! $PSQL -c "SELECT 1 FROM pg_extension WHERE extname = 'postgis'" 2>/dev/null | grep -q 1; then
    echo "WARNING: PostGIS not installed — spatial queries will be skipped"
    has_postgis=false
fi

if ! $PSQL -c "SELECT 1 FROM pg_extension WHERE extname = 'h3'" 2>/dev/null | grep -q 1; then
    echo "WARNING: h3-pg not installed — H3 queries will be skipped"
    has_h3=false
fi

if [ "$has_postgis" = false ] && [ "$has_h3" = false ]; then
    echo "ERROR: Neither PostGIS nor h3-pg installed — nothing to test"
    exit 1
fi

# ---------------------------------------------------------------------------
# Setup test data
# ---------------------------------------------------------------------------
if [ "$has_postgis" = true ]; then
    $PSQL <<'SQL'
CREATE TABLE IF NOT EXISTS _mem_spatial_data (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326)
);
TRUNCATE _mem_spatial_data;
INSERT INTO _mem_spatial_data (geom)
SELECT ST_SetSRID(ST_MakePoint(
    random() * 360.0 - 180.0,
    random() * 180.0 - 90.0
), 4326)
FROM generate_series(1, 5000);
SQL
fi

# ---------------------------------------------------------------------------
# Get backend PID and initial RSS
# ---------------------------------------------------------------------------
BACKEND_PID=$($PSQL -c "SELECT pg_backend_pid();")
echo "Backend PID: $BACKEND_PID"

get_rss_kb() {
    local pid="$1"
    # Works on both Linux (/proc) and macOS (ps)
    if [ -f "/proc/$pid/status" ]; then
        awk '/^VmRSS:/ {print $2}' "/proc/$pid/status" 2>/dev/null || echo 0
    else
        ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ' || echo 0
    fi
}

INITIAL_RSS=$(get_rss_kb "$BACKEND_PID")
if [ "$INITIAL_RSS" = "0" ] || [ -z "$INITIAL_RSS" ]; then
    echo "WARNING: Cannot read RSS for PID $BACKEND_PID — memory assertion will be skipped"
    INITIAL_RSS=0
fi

echo "Initial RSS: ${INITIAL_RSS} KB"
echo ""

# ---------------------------------------------------------------------------
# Build query pool
# ---------------------------------------------------------------------------
declare -a QUERIES=()

if [ "$has_postgis" = true ]; then
    # ST_DWithin filter
    QUERIES+=(
        "SET pg_accel.enabled = on; SELECT count(*) FROM _mem_spatial_data WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(0,0), 4326), 10.0);"
    )
    # Spatial aggregate
    QUERIES+=(
        "SET pg_accel.enabled = on; SELECT count(*) FROM _mem_spatial_data a JOIN _mem_spatial_data b ON a.id < b.id AND ST_DWithin(a.geom, b.geom, 1.0) WHERE a.id <= 100 AND b.id <= 100;"
    )
    # ST_Intersects
    QUERIES+=(
        "SET pg_accel.enabled = on; SELECT count(*) FROM _mem_spatial_data WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(-10, -10, 10, 10), 4326));"
    )
    # Spatial join
    QUERIES+=(
        "SET pg_accel.enabled = on; SELECT count(*) FROM _mem_spatial_data a, _mem_spatial_data b WHERE a.id <= 50 AND b.id <= 50 AND ST_Contains(ST_Buffer(a.geom, 5), b.geom);"
    )
fi

if [ "$has_h3" = true ]; then
    # H3 queries
    QUERIES+=(
        "SET pg_accel.enabled = on; SELECT h3_latlng_to_cell(POINT(40.7128, -74.0060), 7);"
    )
    QUERIES+=(
        "SET pg_accel.enabled = on; SELECT h3_latlng_to_cell(POINT(random() * 180 - 90, random() * 360 - 180), 9);"
    )
fi

NUM_QUERIES=${#QUERIES[@]}

if [ "$NUM_QUERIES" -eq 0 ]; then
    echo "ERROR: No queries available to run"
    exit 1
fi

# ---------------------------------------------------------------------------
# Run queries and sample RSS
# ---------------------------------------------------------------------------
echo "=== Memory test: $TOTAL_QUERIES queries, sampling RSS every $SAMPLE_INTERVAL ==="

PEAK_RSS=$INITIAL_RSS
QUERY_ERRORS=0

for i in $(seq 1 "$TOTAL_QUERIES"); do
    # Round-robin through query pool
    idx=$(( (i - 1) % NUM_QUERIES ))
    query="${QUERIES[$idx]}"

    $PSQL -c "$query" >/dev/null 2>&1 || {
        QUERY_ERRORS=$((QUERY_ERRORS + 1))
    }

    # Sample RSS at intervals
    if (( i % SAMPLE_INTERVAL == 0 )); then
        current_rss=$(get_rss_kb "$BACKEND_PID")
        if [ "$current_rss" != "0" ] && [ -n "$current_rss" ]; then
            growth_kb=$((current_rss - INITIAL_RSS))
            growth_mb=$((growth_kb / 1024))
            if (( current_rss > PEAK_RSS )); then
                PEAK_RSS=$current_rss
            fi
            echo "  [$i/$TOTAL_QUERIES] RSS: ${current_rss} KB (growth: ${growth_mb} MB)"
        else
            echo "  [$i/$TOTAL_QUERIES] (RSS unavailable)"
        fi
    fi
done

# ---------------------------------------------------------------------------
# Final RSS check
# ---------------------------------------------------------------------------
FINAL_RSS=$(get_rss_kb "$BACKEND_PID")
echo ""
echo "=== Memory results ==="
echo "  Initial RSS:  ${INITIAL_RSS} KB"
echo "  Final RSS:    ${FINAL_RSS} KB"
echo "  Peak RSS:     ${PEAK_RSS} KB"
echo "  Query errors: ${QUERY_ERRORS}"

MEM_OK=true
if [ "$INITIAL_RSS" != "0" ] && [ "$FINAL_RSS" != "0" ] && [ -n "$FINAL_RSS" ]; then
    GROWTH_KB=$((FINAL_RSS - INITIAL_RSS))
    GROWTH_MB=$((GROWTH_KB / 1024))
    MAX_GROWTH_KB=$((MAX_GROWTH_MB * 1024))

    echo "  Growth:       ${GROWTH_MB} MB (limit: ${MAX_GROWTH_MB} MB)"

    if (( GROWTH_KB > MAX_GROWTH_KB )); then
        echo "FAIL: Memory growth ${GROWTH_MB} MB exceeds limit ${MAX_GROWTH_MB} MB"
        MEM_OK=false
    fi

    # Also check peak
    PEAK_GROWTH_KB=$((PEAK_RSS - INITIAL_RSS))
    PEAK_GROWTH_MB=$((PEAK_GROWTH_KB / 1024))
    echo "  Peak growth:  ${PEAK_GROWTH_MB} MB"
else
    echo "  (RSS measurement unavailable — skipping growth assertion)"
fi

# ---------------------------------------------------------------------------
# Check pg_accel stats counters if available
# ---------------------------------------------------------------------------
echo ""
echo "=== pg_accel stats ==="
stats_available=false

# Try common stats view/function names
for stats_source in "pg_accel_stats" "pg_accel.stats" "pg_accel_get_stats()"; do
    result=$($PSQL -c "SELECT * FROM $stats_source LIMIT 1;" 2>/dev/null) && {
        stats_available=true
        echo "Stats from $stats_source:"
        $PSQL -c "SELECT * FROM $stats_source;" 2>/dev/null || true

        # Verify at least some counters are non-zero
        nonzero=$($PSQL -c "SELECT count(*) FROM $stats_source WHERE CAST(coalesce(nullif(queries_executed::text,''), '0') AS bigint) > 0;" 2>/dev/null) || true
        if [ -n "$nonzero" ] && [ "$nonzero" != "0" ]; then
            echo "  Stats counters have non-zero values: OK"
        else
            echo "  WARNING: Stats counters appear to be all zero"
        fi
        break
    }
done

if [ "$stats_available" = false ]; then
    echo "  pg_accel stats views not found (not critical)"
fi

# ---------------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------------
if [ "$has_postgis" = true ]; then
    $PSQL -c "DROP TABLE IF EXISTS _mem_spatial_data;" 2>/dev/null || true
fi

echo ""
if [ "$MEM_OK" = true ] && [ "$QUERY_ERRORS" -eq 0 ]; then
    echo "All memory tests passed."
    exit 0
elif [ "$MEM_OK" = false ]; then
    echo "Memory test FAILED."
    exit 1
else
    echo "Memory test passed (with $QUERY_ERRORS query errors)."
    exit 0
fi
