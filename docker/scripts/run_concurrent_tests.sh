#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# Concurrent correctness tests: 16 parallel connections, 4 groups
# ---------------------------------------------------------------------------

DB_HOST="${DB_HOST:-localhost}"
DB_PORT="${DB_PORT:-5488}"
DB_USER="${DB_USER:-postgres}"
DB_NAME="${DB_NAME:-pgaccel_shared}"
LOCK_FILE="/tmp/.pgaccel_reload.lock"
ITERATIONS="${ITERATIONS:-25}"

PSQL="psql -h $DB_HOST -p $DB_PORT -U $DB_USER -d $DB_NAME -v ON_ERROR_STOP=1 -t -A"

# Acquire shared flock
exec 9>"$LOCK_FILE"
flock -s 9

cleanup() {
    # Kill any remaining background jobs
    jobs -p 2>/dev/null | xargs -r kill 2>/dev/null || true
    wait 2>/dev/null || true
    exec 9>&-
}
trap cleanup EXIT

TMPDIR=$(mktemp -d)
FAILURES=0

# ---------------------------------------------------------------------------
# Check extension availability
# ---------------------------------------------------------------------------
has_postgis=true
has_h3=true

if ! $PSQL -c "SELECT 1 FROM pg_extension WHERE extname = 'postgis'" 2>/dev/null | grep -q 1; then
    echo "WARNING: PostGIS not installed — skipping spatial groups"
    has_postgis=false
fi

if ! $PSQL -c "SELECT 1 FROM pg_extension WHERE extname = 'h3'" 2>/dev/null | grep -q 1; then
    echo "WARNING: h3-pg not installed — skipping H3 group"
    has_h3=false
fi

# ---------------------------------------------------------------------------
# Ensure test data exists
# ---------------------------------------------------------------------------
if [ "$has_postgis" = true ]; then
    $PSQL <<'SQL'
CREATE TABLE IF NOT EXISTS _conc_spatial_points (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326)
);
TRUNCATE _conc_spatial_points;
INSERT INTO _conc_spatial_points (geom)
SELECT ST_SetSRID(ST_MakePoint(
    random() * 360.0 - 180.0,
    random() * 180.0 - 90.0
), 4326)
FROM generate_series(1, 5000);
SQL
fi

# ---------------------------------------------------------------------------
# Worker launcher
# ---------------------------------------------------------------------------
run_worker() {
    local group="$1"
    local worker_id="$2"
    local outfile="$TMPDIR/${group}_${worker_id}.log"

    case "$group" in
    # ------------------------------------------------------------------
    # Group 1: ST_DWithin spatial joins ON vs OFF
    # ------------------------------------------------------------------
    spatial_join)
        $PSQL >"$outfile" 2>&1 <<EOF
DO \$\$
DECLARE
    cnt_on bigint;
    cnt_off bigint;
    i int;
BEGIN
    FOR i IN 1..$ITERATIONS LOOP
        SET pg_accel.enabled = on;
        SELECT count(*) INTO cnt_on
        FROM _conc_spatial_points a
        JOIN _conc_spatial_points b ON a.id < b.id
            AND ST_DWithin(a.geom, b.geom, 1.0)
        WHERE a.id <= 200 AND b.id <= 200;

        SET pg_accel.enabled = off;
        SELECT count(*) INTO cnt_off
        FROM _conc_spatial_points a
        JOIN _conc_spatial_points b ON a.id < b.id
            AND ST_DWithin(a.geom, b.geom, 1.0)
        WHERE a.id <= 200 AND b.id <= 200;

        IF cnt_on IS DISTINCT FROM cnt_off THEN
            RAISE EXCEPTION 'spatial_join mismatch iter=% on=% off=%', i, cnt_on, cnt_off;
        END IF;
    END LOOP;
    RAISE NOTICE 'spatial_join worker $worker_id: $ITERATIONS iterations OK';
END;
\$\$;
EOF
        ;;

    # ------------------------------------------------------------------
    # Group 2: Spatial aggregates (COUNT with ST_DWithin filter)
    # ------------------------------------------------------------------
    spatial_agg)
        $PSQL >"$outfile" 2>&1 <<EOF
DO \$\$
DECLARE
    cnt_on bigint;
    cnt_off bigint;
    i int;
    ref_geom geometry;
BEGIN
    SELECT geom INTO ref_geom FROM _conc_spatial_points WHERE id = 1;

    FOR i IN 1..$ITERATIONS LOOP
        SET pg_accel.enabled = on;
        SELECT count(*) INTO cnt_on
        FROM _conc_spatial_points
        WHERE ST_DWithin(geom, ref_geom, 5.0);

        SET pg_accel.enabled = off;
        SELECT count(*) INTO cnt_off
        FROM _conc_spatial_points
        WHERE ST_DWithin(geom, ref_geom, 5.0);

        IF cnt_on IS DISTINCT FROM cnt_off THEN
            RAISE EXCEPTION 'spatial_agg mismatch iter=% on=% off=%', i, cnt_on, cnt_off;
        END IF;
    END LOOP;
    RAISE NOTICE 'spatial_agg worker $worker_id: $ITERATIONS iterations OK';
END;
\$\$;
EOF
        ;;

    # ------------------------------------------------------------------
    # Group 3: H3 queries ON vs OFF
    # ------------------------------------------------------------------
    h3_query)
        $PSQL >"$outfile" 2>&1 <<EOF
DO \$\$
DECLARE
    val_on h3index;
    val_off h3index;
    lat double precision;
    lng double precision;
    i int;
BEGIN
    FOR i IN 1..$ITERATIONS LOOP
        lat := random() * 180.0 - 90.0;
        lng := random() * 360.0 - 180.0;

        SET pg_accel.enabled = on;
        SELECT h3_latlng_to_cell(POINT(lat, lng), 7) INTO val_on;

        SET pg_accel.enabled = off;
        SELECT h3_latlng_to_cell(POINT(lat, lng), 7) INTO val_off;

        IF val_on IS DISTINCT FROM val_off THEN
            RAISE EXCEPTION 'h3 mismatch iter=% lat=% lng=% on=% off=%',
                i, lat, lng, val_on, val_off;
        END IF;
    END LOOP;
    RAISE NOTICE 'h3_query worker $worker_id: $ITERATIONS iterations OK';
END;
\$\$;
EOF
        ;;

    # ------------------------------------------------------------------
    # Group 4: Small queries that should NOT use Custom Scan
    # ------------------------------------------------------------------
    small_query)
        $PSQL >"$outfile" 2>&1 <<EOF
DO \$\$
DECLARE
    cnt bigint;
    i int;
BEGIN
    FOR i IN 1..$ITERATIONS LOOP
        -- Small result set: should fall below Custom Scan cost threshold
        SET pg_accel.enabled = on;
        SELECT count(*) INTO cnt
        FROM _conc_spatial_points
        WHERE id <= 5;

        IF cnt != 5 THEN
            RAISE EXCEPTION 'small_query unexpected count iter=% cnt=%', i, cnt;
        END IF;
    END LOOP;
    RAISE NOTICE 'small_query worker $worker_id: $ITERATIONS iterations OK';
END;
\$\$;
EOF
        ;;
    esac

    return $?
}

# ---------------------------------------------------------------------------
# Launch 16 workers
# ---------------------------------------------------------------------------
echo "=== Concurrent tests: 16 workers x $ITERATIONS iterations ==="
echo "    4 groups: spatial_join, spatial_agg, h3_query, small_query"

PIDS=()

for group in spatial_join spatial_agg h3_query small_query; do
    # Skip groups if extensions missing
    if [ "$group" = "spatial_join" ] || [ "$group" = "spatial_agg" ]; then
        [ "$has_postgis" = false ] && continue
    fi
    if [ "$group" = "h3_query" ]; then
        [ "$has_h3" = false ] && continue
    fi
    # small_query only needs the table which requires PostGIS
    if [ "$group" = "small_query" ] && [ "$has_postgis" = false ]; then
        continue
    fi

    for w in 0 1 2 3; do
        run_worker "$group" "$w" &
        PIDS+=($!)
    done
done

echo "  launched ${#PIDS[@]} workers, waiting..."

# ---------------------------------------------------------------------------
# Collect results
# ---------------------------------------------------------------------------
for pid in "${PIDS[@]}"; do
    if ! wait "$pid"; then
        FAILURES=$((FAILURES + 1))
    fi
done

# Print logs
echo ""
for logfile in "$TMPDIR"/*.log; do
    [ -f "$logfile" ] || continue
    name=$(basename "$logfile" .log)
    content=$(cat "$logfile")
    if [ -n "$content" ]; then
        if echo "$content" | grep -qi 'error\|exception\|mismatch'; then
            echo "FAIL [$name]: $content"
        else
            echo "PASS [$name]"
        fi
    fi
done

# ---------------------------------------------------------------------------
# Cleanup test table
# ---------------------------------------------------------------------------
$PSQL -c "DROP TABLE IF EXISTS _conc_spatial_points;" 2>/dev/null || true

rm -rf "$TMPDIR"

echo ""
if [ $FAILURES -eq 0 ]; then
    echo "All concurrent tests passed."
    exit 0
else
    echo "$FAILURES worker(s) FAILED."
    exit 1
fi
