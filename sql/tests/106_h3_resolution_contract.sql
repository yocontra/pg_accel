-- Every valid H3 parent resolution uses a bounded, real device COUNT pipeline.
\echo '=== 106_h3_resolution_contract ==='
SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
CREATE TEMP TABLE _h3_resolution_input (cell h3index);
INSERT INTO _h3_resolution_input
WITH seeds AS (
    SELECT array_agg(h3_cell_to_center_child(cell, 15) ORDER BY cell) AS cells
    FROM h3_get_res_0_cells() AS c(cell)
)
SELECT CASE WHEN i % 97 = 0 THEN NULL::h3index
            ELSE cells[1 + ((i - 1) % cardinality(cells))] END
FROM generate_series(1, 500000) AS g(i) CROSS JOIN seeds;
ANALYZE _h3_resolution_input;

DO $$
DECLARE
    has_gpu bool;
    resolution int;
    query text;
    line record;
    plan text;
    kernels_before bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_h3_resolution_input'::regclass, ARRAY['cell']);
    END IF;
    FOR resolution IN 0..15 LOOP
        query := format('SELECT h3_cell_to_parent(cell, %s) AS parent, count(*) AS n
                         FROM _h3_resolution_input GROUP BY 1', resolution);
        PERFORM set_config('pg_accel.enabled', 'off', false);
        EXECUTE 'CREATE TEMP TABLE _h3_expected AS ' || query;
        IF (SELECT count(*) FROM _h3_expected) <> 123
           OR (SELECT sum(n) FROM _h3_expected) <> 500000
           OR (SELECT n FROM _h3_expected WHERE parent IS NULL) <> 500000 / 97 THEN
            RAISE EXCEPTION '106 independent group/NULL/count invariant failed at %', resolution;
        END IF;
        PERFORM set_config('pg_accel.enabled', 'on', false);
        SELECT pg_accel_kernel_executions() INTO kernels_before;
        EXECUTE 'CREATE TEMP TABLE _h3_actual AS ' || query;
        IF EXISTS ((SELECT * FROM _h3_expected EXCEPT ALL SELECT * FROM _h3_actual)
                   UNION ALL
                   (SELECT * FROM _h3_actual EXCEPT ALL SELECT * FROM _h3_expected)) THEN
            RAISE EXCEPTION '106 H3 differential failed at resolution %', resolution;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) ' || query LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan) = 0
                        OR pg_accel_kernel_executions() <= kernels_before) THEN
            RAISE EXCEPTION '106 no device dispatch at resolution %: %', resolution, plan;
        ELSIF NOT has_gpu AND pg_accel_kernel_executions() <> kernels_before THEN
            RAISE EXCEPTION '106 dispatched without GPU';
        END IF;
        DROP TABLE _h3_expected, _h3_actual;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:106_h3_resolution_contract.assert_001'

-- Hidden COUNT input, native HAVING, ordering and LIMIT compose with H3 keys.
SET pg_accel.enabled = off;
CREATE TEMP TABLE _h3_expected AS
SELECT h3_cell_to_parent(cell, 9) AS parent
FROM _h3_resolution_input GROUP BY 1 HAVING count(*) > 1000
ORDER BY 1 NULLS FIRST LIMIT 17;
SET pg_accel.enabled = on;
CREATE TEMP TABLE _h3_actual AS
SELECT h3_cell_to_parent(cell, 9) AS parent
FROM _h3_resolution_input GROUP BY 1 HAVING count(*) > 1000
ORDER BY 1 NULLS FIRST LIMIT 17;
DO $$
DECLARE
    plan text := '';
    line record;
BEGIN
    IF EXISTS ((SELECT * FROM _h3_expected EXCEPT ALL SELECT * FROM _h3_actual)
               UNION ALL
               (SELECT * FROM _h3_actual EXCEPT ALL SELECT * FROM _h3_expected)) THEN
        RAISE EXCEPTION '106 hidden H3 HAVING result mismatch';
    END IF;
    FOR line IN
        EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF)
        SELECT h3_cell_to_parent(cell, 9) FROM _h3_resolution_input
        GROUP BY 1 HAVING count(*) > 1000 ORDER BY 1 NULLS FIRST LIMIT 17
    LOOP
        plan := plan || line."QUERY PLAN" || E'\n';
    END LOOP;
    IF (SELECT gpu_available FROM pg_accel_device_info())
       AND (position('GPU Kernel Dispatched: true' IN plan) = 0
            OR position('GPU Post-Aggregate Filter: PostgreSQL HAVING' IN plan) = 0) THEN
        RAISE EXCEPTION '106 hidden H3 HAVING did not dispatch: %', plan;
    END IF;
END $$;
DROP TABLE _h3_expected, _h3_actual;
\echo 'PGACCEL_ASSERT_OK:106_h3_resolution_contract.assert_002'

SET plan_cache_mode = force_custom_plan;
PREPARE _h3_resolution(int) AS
SELECT h3_cell_to_parent(cell, $1), count(*) FROM _h3_resolution_input GROUP BY 1;
DO $$
DECLARE
    resolution int;
    line record;
    plan text;
    actual_rows int;
BEGIN
    FOREACH resolution IN ARRAY ARRAY[15, 4, 0, 9] LOOP
        actual_rows := 0;
        FOR line IN EXECUTE format('EXECUTE _h3_resolution(%s)', resolution) LOOP
            actual_rows := actual_rows + 1;
        END LOOP;
        IF actual_rows <> 123 THEN
            RAISE EXCEPTION '106 prepared resolution % returned % groups', resolution, actual_rows;
        END IF;
        plan := '';
        FOR line IN EXECUTE format('EXPLAIN (ANALYZE, COSTS OFF) EXECUTE _h3_resolution(%s)', resolution) LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF (SELECT gpu_available FROM pg_accel_device_info())
           AND position('GPU Kernel Dispatched: true' IN plan) = 0 THEN
            RAISE EXCEPTION '106 prepared resolution did not dispatch: %', plan;
        END IF;
    END LOOP;
    IF (SELECT stock_exec_count FROM pg_accel_stats()) <> 0 THEN
        RAISE EXCEPTION '106 used stock fallback';
    END IF;
END $$;
DEALLOCATE _h3_resolution;
RESET plan_cache_mode;
\echo 'PGACCEL_ASSERT_OK:106_h3_resolution_contract.assert_003'

SELECT pg_accel_unpin('_h3_resolution_input'::regclass);
SELECT pg_accel_evict('_h3_resolution_input'::regclass);
DROP TABLE _h3_resolution_input;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:106_h3_resolution_contract'
