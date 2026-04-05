#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# Fuzz testing: compare pg_accel ON vs OFF with random geometries and H3 inputs
# ---------------------------------------------------------------------------

DB_HOST="${DB_HOST:-localhost}"
DB_PORT="${DB_PORT:-5488}"
DB_USER="${DB_USER:-postgres}"
DB_NAME="${DB_NAME:-pgaccel_shared}"
LOCK_FILE="/tmp/.pgaccel_reload.lock"
FUZZ_ITERATIONS="${FUZZ_ITERATIONS:-1000}"
FUZZ_SEED="${FUZZ_SEED:-0.42}"

FAILURES=0
TOTAL=0

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
    echo "WARNING: PostGIS not installed — skipping spatial fuzz tests"
    has_postgis=false
fi

if ! $PSQL -c "SELECT 1 FROM pg_extension WHERE extname = 'h3'" 2>/dev/null | grep -q 1; then
    echo "WARNING: h3-pg not installed — skipping H3 fuzz tests"
    has_h3=false
fi

# ---------------------------------------------------------------------------
# Spatial fuzz
# ---------------------------------------------------------------------------
if [ "$has_postgis" = true ]; then
    echo "=== Spatial fuzz: $FUZZ_ITERATIONS iterations (seed $FUZZ_SEED) ==="

    spatial_failures=0

    $PSQL <<'SETUP'
CREATE OR REPLACE FUNCTION _fuzz_random_coord(extreme boolean)
RETURNS double precision LANGUAGE plpgsql AS $$
BEGIN
    IF extreme THEN
        RETURN (ARRAY[-180.0, 180.0, -90.0, 90.0, 0.0])[floor(random() * 5 + 1)::int];
    END IF;
    RETURN random() * 360.0 - 180.0;
END;
$$;

CREATE OR REPLACE FUNCTION _fuzz_random_geom()
RETURNS geometry LANGUAGE plpgsql AS $$
DECLARE
    gtype int;
    nv int;
    extreme boolean;
    coords text;
    x double precision;
    y double precision;
    x0 double precision;
    y0 double precision;
    i int;
BEGIN
    gtype := floor(random() * 3 + 1)::int;  -- 1=point, 2=line, 3=polygon
    extreme := random() < 0.1;

    IF gtype = 1 THEN
        x := _fuzz_random_coord(extreme);
        y := LEAST(GREATEST(_fuzz_random_coord(extreme), -90), 90);
        RETURN ST_SetSRID(ST_MakePoint(x, y), 4326);
    ELSIF gtype = 2 THEN
        nv := floor(random() * 5 + 2)::int;
        coords := '';
        FOR i IN 1..nv LOOP
            IF i > 1 THEN coords := coords || ','; END IF;
            x := _fuzz_random_coord(extreme);
            y := LEAST(GREATEST(_fuzz_random_coord(extreme), -90), 90);
            coords := coords || x || ' ' || y;
        END LOOP;
        RETURN ST_SetSRID(ST_GeomFromText('LINESTRING(' || coords || ')'), 4326);
    ELSE
        nv := floor(random() * 18 + 3)::int;  -- 3-20 vertices
        coords := '';
        x0 := _fuzz_random_coord(extreme);
        y0 := LEAST(GREATEST(_fuzz_random_coord(extreme), -90), 90);
        FOR i IN 1..nv LOOP
            IF i > 1 THEN coords := coords || ','; END IF;
            x := x0 + (random() - 0.5) * 2.0;
            y := y0 + (random() - 0.5) * 2.0;
            y := LEAST(GREATEST(y, -90), 90);
            coords := coords || x || ' ' || y;
        END LOOP;
        -- close the ring
        coords := coords || ',' || split_part(coords, ',', 1);
        BEGIN
            RETURN ST_SetSRID(
                ST_MakeValid(ST_GeomFromText('POLYGON((' || coords || '))')),
                4326
            );
        EXCEPTION WHEN OTHERS THEN
            RETURN ST_SetSRID(ST_MakePoint(x0, y0), 4326);
        END;
    END IF;
END;
$$;
SETUP

    for i in $(seq 1 "$FUZZ_ITERATIONS"); do
        if (( i % 100 == 0 )); then
            echo "  spatial progress: $i / $FUZZ_ITERATIONS ($spatial_failures failures so far)"
        fi

        for func in ST_Intersects ST_Contains ST_Within; do
            TOTAL=$((TOTAL + 1))
            result=$($PSQL <<EOF 2>&1) || { FAILURES=$((FAILURES + 1)); spatial_failures=$((spatial_failures + 1)); continue; }
SELECT setseed($FUZZ_SEED + $i * 0.000001);
DO \$\$
DECLARE
    g1 geometry;
    g2 geometry;
    val_on boolean;
    val_off boolean;
BEGIN
    g1 := _fuzz_random_geom();
    g2 := _fuzz_random_geom();

    SET pg_accel.enabled = on;
    EXECUTE format('SELECT %s(\$1, \$2)', '$func') INTO val_on USING g1, g2;

    SET pg_accel.enabled = off;
    EXECUTE format('SELECT %s(\$1, \$2)', '$func') INTO val_off USING g1, g2;

    IF val_on IS DISTINCT FROM val_off THEN
        RAISE NOTICE 'MISMATCH seed=% func=$func g1=% g2=% on=% off=%',
            $FUZZ_SEED + $i * 0.000001,
            ST_AsText(g1), ST_AsText(g2), val_on, val_off;
        RAISE EXCEPTION 'fuzz_mismatch';
    END IF;
END;
\$\$;
SELECT 'ok';
EOF
            if echo "$result" | grep -q 'fuzz_mismatch'; then
                echo "FAIL: $func iteration $i"
                echo "  $result" | grep 'MISMATCH'
                FAILURES=$((FAILURES + 1))
                spatial_failures=$((spatial_failures + 1))
            fi
        done

        # ST_DWithin with random distance
        TOTAL=$((TOTAL + 1))
        result=$($PSQL <<EOF 2>&1) || { FAILURES=$((FAILURES + 1)); spatial_failures=$((spatial_failures + 1)); continue; }
SELECT setseed($FUZZ_SEED + $i * 0.000001);
DO \$\$
DECLARE
    g1 geometry;
    g2 geometry;
    dist double precision;
    val_on boolean;
    val_off boolean;
BEGIN
    g1 := _fuzz_random_geom();
    g2 := _fuzz_random_geom();
    dist := random() * 10.0;

    SET pg_accel.enabled = on;
    SELECT ST_DWithin(g1, g2, dist) INTO val_on;

    SET pg_accel.enabled = off;
    SELECT ST_DWithin(g1, g2, dist) INTO val_off;

    IF val_on IS DISTINCT FROM val_off THEN
        RAISE NOTICE 'MISMATCH seed=% func=ST_DWithin dist=% g1=% g2=% on=% off=%',
            $FUZZ_SEED + $i * 0.000001,
            dist, ST_AsText(g1), ST_AsText(g2), val_on, val_off;
        RAISE EXCEPTION 'fuzz_mismatch';
    END IF;
END;
\$\$;
SELECT 'ok';
EOF
        if echo "$result" | grep -q 'fuzz_mismatch'; then
            echo "FAIL: ST_DWithin iteration $i"
            echo "  $result" | grep 'MISMATCH'
            FAILURES=$((FAILURES + 1))
            spatial_failures=$((spatial_failures + 1))
        fi
    done

    # Clean up helper functions
    $PSQL -c "DROP FUNCTION IF EXISTS _fuzz_random_geom();" 2>/dev/null || true
    $PSQL -c "DROP FUNCTION IF EXISTS _fuzz_random_coord(boolean);" 2>/dev/null || true

    echo "  spatial fuzz complete: $spatial_failures failures"
fi

# ---------------------------------------------------------------------------
# H3 fuzz
# ---------------------------------------------------------------------------
if [ "$has_h3" = true ]; then
    echo "=== H3 fuzz: $FUZZ_ITERATIONS iterations x 16 resolutions ==="

    h3_failures=0

    for i in $(seq 1 "$FUZZ_ITERATIONS"); do
        if (( i % 100 == 0 )); then
            echo "  h3 progress: $i / $FUZZ_ITERATIONS ($h3_failures failures so far)"
        fi

        for res in $(seq 0 15); do
            TOTAL=$((TOTAL + 1))
            result=$($PSQL <<EOF 2>&1) || { FAILURES=$((FAILURES + 1)); h3_failures=$((h3_failures + 1)); continue; }
SELECT setseed($FUZZ_SEED + $i * 0.000001);
DO \$\$
DECLARE
    lat double precision;
    lng double precision;
    val_on h3index;
    val_off h3index;
BEGIN
    lat := random() * 180.0 - 90.0;
    lng := random() * 360.0 - 180.0;

    SET pg_accel.enabled = on;
    SELECT h3_latlng_to_cell(POINT(lat, lng), $res) INTO val_on;

    SET pg_accel.enabled = off;
    SELECT h3_latlng_to_cell(POINT(lat, lng), $res) INTO val_off;

    IF val_on IS DISTINCT FROM val_off THEN
        RAISE NOTICE 'MISMATCH seed=% lat=% lng=% res=$res on=% off=%',
            $FUZZ_SEED + $i * 0.000001, lat, lng, val_on, val_off;
        RAISE EXCEPTION 'fuzz_mismatch';
    END IF;
END;
\$\$;
SELECT 'ok';
EOF
            if echo "$result" | grep -q 'fuzz_mismatch'; then
                echo "FAIL: h3_latlng_to_cell iteration $i res=$res"
                echo "  $result" | grep 'MISMATCH'
                FAILURES=$((FAILURES + 1))
                h3_failures=$((h3_failures + 1))
            fi
        done
    done

    echo "  h3 fuzz complete: $h3_failures failures"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "=== Fuzz summary ==="
echo "$((TOTAL - FAILURES))/$TOTAL tests passed"

if [ $FAILURES -eq 0 ]; then
    echo "All fuzz tests passed."
    exit 0
else
    echo "$FAILURES fuzz test(s) FAILED."
    exit 1
fi
