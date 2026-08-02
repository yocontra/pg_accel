-- 90_selected_path_contract.sql: external selected-path and dispatch contract.
-- A capable GPU host must select and dispatch the resident grouped aggregate.
-- A host without a GPU must remain native and must not increment the counter.

\echo '=== 90_selected_path_contract ==='

SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;

CREATE TEMP TABLE _selected_contract_data AS
SELECT (i % 64)::int4 AS g, (i % 1000)::int4 AS v
FROM generate_series(1, 500000) AS i;
ANALYZE _selected_contract_data;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _selected_contract_native AS
SELECT g, sum(v) AS total, count(*) AS rows
FROM _selected_contract_data
GROUP BY g;

CREATE TEMP TABLE _selected_contract_plan (line text);

DO $$
DECLARE
    has_gpu boolean;
    pinned_rows bigint;
    kernels_before bigint;
    kernels_after bigint;
    plan_row record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu
    FROM pg_accel_device_info();

    IF has_gpu THEN
        SELECT pg_accel_pin(
            '_selected_contract_data'::regclass,
            ARRAY['g', 'v']
        ) INTO STRICT pinned_rows;
        IF pinned_rows <> 500000 THEN
            RAISE EXCEPTION
                '90_selected_path_contract FAILED: pin returned % rows, expected 500000',
                pinned_rows;
        END IF;
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    EXECUTE $query$
        CREATE TEMP TABLE _selected_contract_accel AS
        SELECT g, sum(v) AS total, count(*) AS rows
        FROM _selected_contract_data
        GROUP BY g
    $query$;

    FOR plan_row IN EXECUTE $query$
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT g, sum(v), count(*)
        FROM _selected_contract_data
        GROUP BY g
    $query$
    LOOP
        INSERT INTO _selected_contract_plan VALUES (plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;

    IF EXISTS (
        SELECT 1
        FROM _selected_contract_native AS native
        FULL OUTER JOIN _selected_contract_accel AS accel USING (g)
        WHERE native.total IS DISTINCT FROM accel.total
           OR native.rows IS DISTINCT FROM accel.rows
    ) THEN
        RAISE EXCEPTION
            '90_selected_path_contract FAILED: enabled result differs from native';
    END IF;

    IF has_gpu THEN
        IF NOT EXISTS (
            SELECT 1 FROM _selected_contract_plan
            WHERE line LIKE '%Custom Scan (GpuAccelAgg)%'
        ) THEN
            RAISE EXCEPTION
                '90_selected_path_contract FAILED: capable host did not select GpuAccelAgg';
        END IF;
        IF NOT EXISTS (
            SELECT 1 FROM _selected_contract_plan
            WHERE line LIKE '%Plan Selected: true%'
        ) THEN
            RAISE EXCEPTION
                '90_selected_path_contract FAILED: selected plan lacks selection proof';
        END IF;
        IF NOT EXISTS (
            SELECT 1 FROM _selected_contract_plan
            WHERE line LIKE '%GPU Resident Pipeline: true%'
        ) THEN
            RAISE EXCEPTION
                '90_selected_path_contract FAILED: selected plan lacks resident proof';
        END IF;
        IF NOT EXISTS (
            SELECT 1 FROM _selected_contract_plan
            WHERE line LIKE '%GPU Kernel Dispatched: true%'
        ) THEN
            RAISE EXCEPTION
                '90_selected_path_contract FAILED: selected plan lacks dispatch proof';
        END IF;
        IF kernels_after <= kernels_before THEN
            RAISE EXCEPTION
                '90_selected_path_contract FAILED: GPU counter did not increase (% -> %)',
                kernels_before, kernels_after;
        END IF;
    ELSE
        IF EXISTS (
            SELECT 1 FROM _selected_contract_plan
            WHERE line LIKE '%Custom Scan (%GpuAccel%'
        ) THEN
            RAISE EXCEPTION
                '90_selected_path_contract FAILED: unavailable host selected pg_accel Custom Scan';
        END IF;
        IF kernels_after <> kernels_before THEN
            RAISE EXCEPTION
                '90_selected_path_contract FAILED: unavailable host dispatched a GPU kernel (% -> %)',
                kernels_before, kernels_after;
        END IF;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:90_selected_path_contract.assert_001'

DROP TABLE _selected_contract_plan;
DROP TABLE _selected_contract_accel;
DROP TABLE _selected_contract_native;
DROP TABLE _selected_contract_data;

\echo 'PGACCEL_FILE_OK:90_selected_path_contract'
