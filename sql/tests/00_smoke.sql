-- Smoke test: verify all extensions loaded and basic functions work.
-- Self-contained: creates its own test data, no external table dependencies.

\echo '=== 00_smoke ==='

-- Verify pg_accel extension is loaded
DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_accel') THEN
        RAISE EXCEPTION '00_smoke FAILED: pg_accel extension not installed';
    END IF;
END $$;

-- Verify PostGIS extension is loaded
DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'postgis') THEN
        RAISE EXCEPTION '00_smoke FAILED: postgis extension not installed';
    END IF;
END $$;

-- Verify PostGIS basic function
DO $$ BEGIN
    IF ST_AsText(ST_MakePoint(0, 0)) IS NULL THEN
        RAISE EXCEPTION '00_smoke FAILED: ST_MakePoint returned NULL';
    END IF;
END $$;

-- Verify pg_accel GUC exists and can be toggled
SET pg_accel.enabled = on;
SET pg_accel.enabled = off;
SET pg_accel.enabled = on;

-- Verify basic accelerable function works with pg_accel on
DO $$ BEGIN
    IF abs(-42) <> 42 THEN
        RAISE EXCEPTION '00_smoke FAILED: abs(-42) did not return 42';
    END IF;
    IF sqrt(16.0) <> 4.0 THEN
        RAISE EXCEPTION '00_smoke FAILED: sqrt(16) did not return 4';
    END IF;
    IF lower('HELLO') <> 'hello' THEN
        RAISE EXCEPTION '00_smoke FAILED: lower(HELLO) did not return hello';
    END IF;
END $$;

-- Verify self-contained query with temp table
BEGIN;
CREATE TEMP TABLE _smoke_data (id serial PRIMARY KEY, x integer NOT NULL);
INSERT INTO _smoke_data (x) SELECT generate_series(1, 100);

SET pg_accel.enabled = off;
CREATE TEMP TABLE _smoke_off AS SELECT sum(abs(x)) AS s FROM _smoke_data;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _smoke_on AS SELECT sum(abs(x)) AS s FROM _smoke_data;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _smoke_on a, _smoke_off b
        WHERE a.s IS DISTINCT FROM b.s
    ) THEN
        RAISE EXCEPTION '00_smoke FAILED: sum(abs(x)) differs between ON and OFF';
    END IF;
END $$;

DROP TABLE IF EXISTS _smoke_data, _smoke_off, _smoke_on;
COMMIT;

\echo 'PASS: 00_smoke'
