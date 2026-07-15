-- 01_core_engine.sql: Core engine integration tests — stats, device info, GUC toggle.
-- Verifies pg_accel extension functions exist and return reasonable values.

\echo '=== 01_core_engine ==='

BEGIN;

-- =========================================================================
-- Test 1: pg_accel_version() returns a non-empty string
-- =========================================================================
DO $$ DECLARE v text; BEGIN
    SELECT pg_accel_version() INTO v;
    IF v IS NULL OR length(v) = 0 THEN
        RAISE EXCEPTION '01_core_engine FAILED: pg_accel_version() returned NULL or empty';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:01_core_engine.assert_001'


-- =========================================================================
-- Test 2: pg_accel_stats() returns a row
-- =========================================================================
DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_accel_stats()) THEN
        RAISE EXCEPTION '01_core_engine FAILED: pg_accel_stats() returned no rows';
    END IF;
END $$;

-- =========================================================================
-- Test 3: pg_accel_reset_stats() resets counters
-- =========================================================================
SELECT pg_accel_reset_stats();

DO $$
DECLARE
    qa bigint;
    rd bigint;
BEGIN
    SELECT queries_accelerated, rows_dispatched INTO qa, rd
    FROM pg_accel_stats();
    IF qa <> 0 OR rd <> 0 THEN
        RAISE EXCEPTION '01_core_engine FAILED: counters not zero after reset (qa=%, rd=%)', qa, rd;
    END IF;
END $$;

-- =========================================================================
-- Test 4: pg_accel_device_info() returns info
-- =========================================================================
DO $$
DECLARE
    cores int;
    ver text;
BEGIN
    SELECT cpu_cores, pg_accel_version INTO cores, ver
    FROM pg_accel_device_info();
    IF cores IS NULL OR cores <= 0 THEN
        RAISE EXCEPTION '01_core_engine FAILED: cpu_cores is NULL or <= 0';
    END IF;
    IF ver IS NULL OR length(ver) = 0 THEN
        RAISE EXCEPTION '01_core_engine FAILED: pg_accel_version in device_info is empty';
    END IF;
END $$;

-- =========================================================================
-- Test 5: ON vs OFF produces identical results
-- =========================================================================
CREATE TEMP TABLE _ce_data AS SELECT generate_series(1, 10000) AS x;
ANALYZE _ce_data;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ce_off AS SELECT sum(abs(x)) AS s, count(*) AS c FROM _ce_data;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ce_on AS SELECT sum(abs(x)) AS s, count(*) AS c FROM _ce_data;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ce_on a, _ce_off b
        WHERE a.s IS DISTINCT FROM b.s
           OR a.c IS DISTINCT FROM b.c
    ) THEN
        RAISE EXCEPTION '01_core_engine FAILED: sum(abs(x)) differs between ON and OFF';
    END IF;
END $$;

DROP TABLE IF EXISTS _ce_data, _ce_off, _ce_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:01_core_engine'
