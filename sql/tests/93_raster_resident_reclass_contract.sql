-- 93_raster_resident_reclass_contract.sql: exact production boundary for the
-- three-argument resident ST_Reclass candidate. The primary fixture clears
-- the device pixel floor; boundary fixtures remain exact native declines.

\echo '=== 93_raster_resident_reclass_contract ==='

SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;

DROP TABLE IF EXISTS _raster_reclass_input;
CREATE UNLOGGED TABLE _raster_reclass_input (rast raster);
INSERT INTO _raster_reclass_input
SELECT CASE
         WHEN g % 97 = 0 THEN NULL
         ELSE ST_AddBand(
           ST_MakeEmptyRaster(32, 32, 0, 0, 1, -1, 0, 0, 4326),
           '8BUI'::text,
           CASE WHEN g % 101 = 0 THEN 255
                WHEN g % 3 = 0 THEN 7
                WHEN g % 3 = 1 THEN 9
                ELSE 0 END,
           255
         )
       END
FROM generate_series(1, 10000) AS rows(g);
ANALYZE _raster_reclass_input;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _raster_reclass_native AS
SELECT ST_Reclass(rast, '0:1,7:2,255:4', '8BUI') AS rast
FROM _raster_reclass_input;

SELECT pg_accel_pin('_raster_reclass_input'::regclass, ARRAY['rast']);
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _raster_reclass_counters AS
SELECT pg_accel_kernel_executions() AS kernels_before,
       pg_accel_planner_rejection_count('raster_cost_uncalibrated') AS declines_before;

CREATE TEMP TABLE _raster_reclass_enabled AS
SELECT ST_Reclass(rast, '0:1,7:2,255:4', '8BUI') AS rast
FROM _raster_reclass_input;

CREATE TEMP TABLE _raster_reclass_plan (line text NOT NULL);
DO $$
DECLARE
    plan_row record;
BEGIN
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT ST_Reclass(rast, '0:1,7:2,255:4', '8BUI')
        FROM _raster_reclass_input
    LOOP
        INSERT INTO _raster_reclass_plan VALUES (plan_row."QUERY PLAN");
    END LOOP;
END $$;

DO $$
DECLARE
    native_bytes bigint;
    enabled_bytes bigint;
    kernels_before bigint;
    kernels bigint;
    stock bigint;
    declines_before bigint;
    declines bigint;
BEGIN
    IF EXISTS (
        (SELECT encode(ST_AsBinary(rast), 'hex')
         FROM _raster_reclass_native
         EXCEPT ALL
         SELECT encode(ST_AsBinary(rast), 'hex')
         FROM _raster_reclass_enabled)
        UNION ALL
        (SELECT encode(ST_AsBinary(rast), 'hex')
         FROM _raster_reclass_enabled
         EXCEPT ALL
         SELECT encode(ST_AsBinary(rast), 'hex')
         FROM _raster_reclass_native)
    ) THEN
        RAISE EXCEPTION '93 raster contract FAILED: reconstructed WKB differs from PostGIS';
    END IF;

    SELECT COALESCE(sum(octet_length(ST_AsBinary(rast))), 0)
    INTO STRICT native_bytes FROM _raster_reclass_native;
    SELECT COALESCE(sum(octet_length(ST_AsBinary(rast))), 0)
    INTO STRICT enabled_bytes FROM _raster_reclass_enabled;
    IF native_bytes <= 0 OR enabled_bytes IS DISTINCT FROM native_bytes THEN
        RAISE EXCEPTION
            '93 raster contract FAILED: reconstructed byte counts differ (native %, enabled %)',
            native_bytes, enabled_bytes;
    END IF;

    IF (SELECT count(*) FROM _raster_reclass_enabled) <> 10000
       OR (SELECT count(*) FROM _raster_reclass_enabled WHERE rast IS NULL) <> 103 THEN
        RAISE EXCEPTION '93 raster contract FAILED: NULL row semantics changed';
    END IF;
    IF (SELECT count(*) FROM _raster_reclass_enabled
        WHERE ST_NumBands(rast) > 0
          AND ST_BandNoDataValue(rast, 1) IS NOT NULL) <> 0 THEN
        RAISE EXCEPTION '93 raster contract FAILED: output nodata metadata changed';
    END IF;
    IF (SELECT count(*) FROM _raster_reclass_enabled
        WHERE ST_NumBands(rast) > 0 AND ST_Value(rast, 1, 1, 1) = 2) <> 3266 THEN
        RAISE EXCEPTION '93 raster contract FAILED: mapped pixel semantics changed';
    END IF;
    IF (SELECT count(*) FROM _raster_reclass_enabled
        WHERE ST_NumBands(rast) > 0 AND ST_Value(rast, 1, 1, 1) = 4) <> 98 THEN
        RAISE EXCEPTION '93 raster contract FAILED: source nodata mapping changed';
    END IF;
    IF (SELECT count(*) FROM _raster_reclass_enabled
        WHERE ST_NumBands(rast) > 0 AND ST_Value(rast, 1, 1, 1) = 0) <> 3266 THEN
        RAISE EXCEPTION '93 raster contract FAILED: unmatched pixel semantics changed';
    END IF;

    SELECT counters.kernels_before, counters.declines_before
    INTO STRICT kernels_before, declines_before
    FROM _raster_reclass_counters AS counters;
    SELECT pg_accel_kernel_executions() INTO STRICT kernels;
    SELECT stock_exec_count INTO STRICT stock FROM pg_accel_stats();
    SELECT pg_accel_planner_rejection_count('raster_cost_uncalibrated')
    INTO STRICT declines;
    IF NOT EXISTS (
        SELECT 1 FROM _raster_reclass_plan
        WHERE line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels <= kernels_before OR stock <> 0 OR declines <> declines_before THEN
        RAISE EXCEPTION
            '93 raster contract FAILED: expected selected exact dispatch (kernels % -> %, stock %, declines % -> %)',
            kernels_before, kernels, stock, declines_before, declines;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:93_raster_resident_reclass_contract.assert_001'

DROP TABLE IF EXISTS _raster_reclass_missing_input;
CREATE UNLOGGED TABLE _raster_reclass_missing_input AS
SELECT ST_MakeEmptyRaster(2, 2, 0, 0, 1, -1, 0, 0, 4326) AS rast;
SELECT pg_accel_pin('_raster_reclass_missing_input'::regclass, ARRAY['rast']);

SET pg_accel.enabled = off;
CREATE TEMP TABLE _raster_reclass_missing_native AS
SELECT ST_Reclass(rast, '0:1,7:2,255:4', '8BUI') AS rast
FROM _raster_reclass_missing_input;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _raster_reclass_missing_counters AS
SELECT pg_accel_kernel_executions() AS kernels_before,
       pg_accel_planner_rejection_count('raster_selected_band_missing') AS declines_before;
CREATE TEMP TABLE _raster_reclass_missing_enabled AS
SELECT ST_Reclass(rast, '0:1,7:2,255:4', '8BUI') AS rast
FROM _raster_reclass_missing_input;

DO $$
DECLARE
    kernels_before bigint;
    kernels bigint;
    declines_before bigint;
    declines bigint;
    stock bigint;
BEGIN
    IF EXISTS (
        (SELECT encode(ST_AsBinary(rast), 'hex')
         FROM _raster_reclass_missing_native
         EXCEPT ALL
         SELECT encode(ST_AsBinary(rast), 'hex')
         FROM _raster_reclass_missing_enabled)
        UNION ALL
        (SELECT encode(ST_AsBinary(rast), 'hex')
         FROM _raster_reclass_missing_enabled
         EXCEPT ALL
         SELECT encode(ST_AsBinary(rast), 'hex')
         FROM _raster_reclass_missing_native)
    ) OR (SELECT count(*) FROM _raster_reclass_missing_enabled
          WHERE rast IS NOT NULL AND ST_NumBands(rast) = 0) <> 1 THEN
        RAISE EXCEPTION '93 raster contract FAILED: missing-band native parity changed';
    END IF;
    SELECT counters.kernels_before, counters.declines_before
    INTO STRICT kernels_before, declines_before
    FROM _raster_reclass_missing_counters AS counters;
    SELECT pg_accel_kernel_executions() INTO STRICT kernels;
    SELECT pg_accel_planner_rejection_count('raster_selected_band_missing')
    INTO STRICT declines;
    SELECT stock_exec_count INTO STRICT stock FROM pg_accel_stats();
    IF kernels <> kernels_before OR stock <> 0 OR declines <= declines_before THEN
        RAISE EXCEPTION
            '93 raster contract FAILED: missing band did not decline exactly (kernels % -> %, stock %, declines % -> %)',
            kernels_before, kernels, stock, declines_before, declines;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:93_raster_resident_reclass_contract.assert_002'

CREATE TEMP TABLE _raster_reclass_errors (
    mode text PRIMARY KEY,
    sqlstate text NOT NULL,
    message text NOT NULL
);
CREATE TEMP TABLE _raster_reclass_malformed_counters AS
SELECT pg_accel_kernel_executions() AS kernels_before;

DO $$
DECLARE
    mode text;
BEGIN
    FOREACH mode IN ARRAY ARRAY['off', 'on']
    LOOP
        PERFORM set_config('pg_accel.enabled', mode, false);
        BEGIN
            EXECUTE $query$
                SELECT ST_Reclass(rast, 'not-a-rule', '8BUI')
                FROM _raster_reclass_input
                LIMIT 1
            $query$;
            INSERT INTO _raster_reclass_errors VALUES (mode, '00000', 'no error');
        EXCEPTION WHEN OTHERS THEN
            INSERT INTO _raster_reclass_errors VALUES (mode, SQLSTATE, SQLERRM);
        END;
    END LOOP;
END $$;

DO $$
DECLARE
    kernels_before bigint;
    kernels bigint;
    stock bigint;
    shape_declines bigint;
    native_sqlstate text;
    native_message text;
    enabled_sqlstate text;
    enabled_message text;
BEGIN
    SELECT sqlstate, message INTO STRICT native_sqlstate, native_message
    FROM _raster_reclass_errors WHERE mode = 'off';
    SELECT sqlstate, message INTO STRICT enabled_sqlstate, enabled_message
    FROM _raster_reclass_errors WHERE mode = 'on';
    IF enabled_sqlstate IS DISTINCT FROM native_sqlstate
       OR enabled_message IS DISTINCT FROM native_message THEN
        RAISE EXCEPTION '93 raster contract FAILED: malformed-rule behavior differs';
    END IF;
    SELECT counters.kernels_before INTO STRICT kernels_before
    FROM _raster_reclass_malformed_counters AS counters;
    SELECT pg_accel_kernel_executions() INTO STRICT kernels;
    SELECT stock_exec_count INTO STRICT stock FROM pg_accel_stats();
    SELECT pg_accel_planner_rejection_count('raster_unsupported_shape')
    INTO STRICT shape_declines;
    IF kernels <> kernels_before OR stock <> 0 OR shape_declines <= 0 THEN
        RAISE EXCEPTION
            '93 raster contract FAILED: malformed rule boundary changed (kernels % -> %, stock %, declines %)',
            kernels_before, kernels, stock, shape_declines;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:93_raster_resident_reclass_contract.assert_003'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _raster_reclass_null_arg_counters AS
SELECT pg_accel_kernel_executions() AS kernels_before;
CREATE TEMP TABLE _raster_reclass_null_arg_native AS
SELECT ST_Reclass(rast, NULL::text, '8BUI') AS rast
FROM _raster_reclass_input;
SET pg_accel.enabled = on;
CREATE TEMP TABLE _raster_reclass_null_arg_enabled AS
SELECT ST_Reclass(rast, NULL::text, '8BUI') AS rast
FROM _raster_reclass_input;

DO $$
DECLARE
    kernels_before bigint;
    kernels bigint;
    stock bigint;
BEGIN
    IF (SELECT count(*) FROM _raster_reclass_null_arg_native) <> 10000
       OR (SELECT count(*) FROM _raster_reclass_null_arg_native WHERE rast IS NOT NULL) <> 0
       OR EXISTS (
           (SELECT encode(ST_AsBinary(rast), 'hex')
            FROM _raster_reclass_null_arg_native
            EXCEPT ALL
            SELECT encode(ST_AsBinary(rast), 'hex')
            FROM _raster_reclass_null_arg_enabled)
           UNION ALL
           (SELECT encode(ST_AsBinary(rast), 'hex')
            FROM _raster_reclass_null_arg_enabled
            EXCEPT ALL
            SELECT encode(ST_AsBinary(rast), 'hex')
            FROM _raster_reclass_null_arg_native)
       ) THEN
        RAISE EXCEPTION '93 raster contract FAILED: strict NULL argument behavior differs';
    END IF;
    SELECT counters.kernels_before INTO STRICT kernels_before
    FROM _raster_reclass_null_arg_counters AS counters;
    SELECT pg_accel_kernel_executions() INTO STRICT kernels;
    SELECT stock_exec_count INTO STRICT stock FROM pg_accel_stats();
    IF kernels <> kernels_before OR stock <> 0 THEN
        RAISE EXCEPTION
            '93 raster contract FAILED: strict NULL argument dispatched/fell back (kernels % -> %, stock %)',
            kernels_before, kernels, stock;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:93_raster_resident_reclass_contract.assert_004'

DROP TABLE _raster_reclass_null_arg_enabled;
DROP TABLE _raster_reclass_null_arg_native;
DROP TABLE _raster_reclass_null_arg_counters;
DROP TABLE _raster_reclass_errors;
DROP TABLE _raster_reclass_malformed_counters;
DROP TABLE _raster_reclass_missing_enabled;
DROP TABLE _raster_reclass_missing_native;
DROP TABLE _raster_reclass_missing_counters;
DROP TABLE _raster_reclass_missing_input;
DROP TABLE _raster_reclass_plan;
DROP TABLE _raster_reclass_enabled;
DROP TABLE _raster_reclass_native;
DROP TABLE _raster_reclass_counters;
DROP TABLE _raster_reclass_input;

\echo 'PGACCEL_FILE_OK:93_raster_resident_reclass_contract'
