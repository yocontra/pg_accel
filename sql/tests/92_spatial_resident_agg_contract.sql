-- 92_spatial_resident_agg_contract.sql: production selected-path contract for
-- the exact resident ST_Intersects(point column, one-ring polygon constant)
-- COUNT(*) lane. Other spatial predicates and aggregate shapes remain native.

\echo '=== 92_spatial_resident_agg_contract ==='

SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;

CREATE TEMP TABLE _spatial_resident_contract (
    id int8 PRIMARY KEY,
    geom geometry(Point, 4326)
);
INSERT INTO _spatial_resident_contract (id, geom)
SELECT i,
       CASE WHEN i % 97 = 0 THEN NULL
            ELSE ST_SetSRID(
                   ST_MakePoint(
                       (i % 1000)::float8 / 50.0,
                       ((i / 1000) % 1000)::float8 / 50.0
                   ),
                   4326
                 )::geometry(Point, 4326)
       END
FROM generate_series(1, 1000000) AS rows(i);
ANALYZE _spatial_resident_contract;

CREATE TEMP TABLE _spatial_resident_plan (line text NOT NULL);

DO $$
DECLARE
    has_gpu boolean;
    pinned_rows bigint;
    expected_count bigint;
    native_count bigint;
    enabled_count bigint;
    kernels_before bigint;
    kernels_after bigint;
    stock_after bigint;
    plan_row record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT count(*) INTO STRICT expected_count
    FROM _spatial_resident_contract
    WHERE id % 97 <> 0
      AND id % 1000 BETWEEN 250 AND 750
      AND (id / 1000) % 1000 BETWEEN 250 AND 750;

    PERFORM set_config('pg_accel.enabled', 'off', false);
    SELECT count(*) INTO STRICT native_count
    FROM _spatial_resident_contract
    WHERE ST_Intersects(
        geom,
        ST_Segmentize(ST_MakeEnvelope(5, 5, 15, 15, 4326), 0.0390625)
    );
    IF native_count IS DISTINCT FROM expected_count THEN
        RAISE EXCEPTION
            '92 spatial contract FAILED: PostGIS count %, arithmetic oracle %',
            native_count, expected_count;
    END IF;

    IF has_gpu THEN
        SELECT pg_accel_pin(
            '_spatial_resident_contract'::regclass,
            ARRAY['geom']
        ) INTO STRICT pinned_rows;
        IF pinned_rows <> 1000000 THEN
            RAISE EXCEPTION
                '92 spatial contract FAILED: pin returned % rows, expected 1000000',
                pinned_rows;
        END IF;
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    SELECT count(*) INTO STRICT enabled_count
    FROM _spatial_resident_contract
    WHERE ST_Intersects(
        geom,
        ST_Segmentize(ST_MakeEnvelope(5, 5, 15, 15, 4326), 0.0390625)
    );
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT count(*)
        FROM _spatial_resident_contract
        WHERE ST_Intersects(
            geom,
            ST_Segmentize(ST_MakeEnvelope(5, 5, 15, 15, 4326), 0.0390625)
        )
    LOOP
        INSERT INTO _spatial_resident_plan VALUES (plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();

    IF enabled_count IS DISTINCT FROM native_count THEN
        RAISE EXCEPTION
            '92 spatial contract FAILED: enabled count %, native count %',
            enabled_count, native_count;
    END IF;
    IF has_gpu THEN
        IF NOT EXISTS (
            SELECT 1 FROM _spatial_resident_plan
            WHERE line LIKE '%Custom Scan (GpuAccelAgg)%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _spatial_resident_plan
            WHERE line LIKE '%Plan Selected: true%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _spatial_resident_plan
            WHERE line LIKE '%GPU Resident Pipeline: true%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _spatial_resident_plan
            WHERE line LIKE '%GPU Kernel Dispatched: true%'
        ) THEN
            RAISE EXCEPTION
                '92 spatial contract FAILED: capable host lacks selected resident dispatch proof';
        END IF;
        IF kernels_after <= kernels_before OR stock_after <> 0 THEN
            RAISE EXCEPTION
                '92 spatial contract FAILED: dispatch/fallback mismatch (kernels % -> %, stock %)',
                kernels_before, kernels_after, stock_after;
        END IF;
    ELSE
        IF EXISTS (
            SELECT 1 FROM _spatial_resident_plan
            WHERE line LIKE '%Custom Scan (%GpuAccel%'
        ) OR kernels_after <> kernels_before OR stock_after <> 0 THEN
            RAISE EXCEPTION
                '92 spatial contract FAILED: unavailable host selected/dispatched (kernels % -> %, stock %)',
                kernels_before, kernels_after, stock_after;
        END IF;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:92_spatial_resident_agg_contract.assert_001'

DROP TABLE _spatial_resident_plan;
DROP TABLE _spatial_resident_contract;

\echo 'PGACCEL_FILE_OK:92_spatial_resident_agg_contract'
