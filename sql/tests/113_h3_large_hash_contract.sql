-- Large-input and high-cardinality H3 COUNT, including parallel finalization.
\echo '=== 113_h3_large_hash_contract ==='
SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
SET pg_accel.enabled = off;
CREATE TEMP TABLE _h3_large_input (cell h3index);
INSERT INTO _h3_large_input
SELECT CASE WHEN i % 97 = 0 THEN NULL::h3index
            ELSE h3_cell_to_center_child(
                CASE WHEN i % 2 = 0 THEN '8001fffffffffff'::h3index
                     ELSE '8003fffffffffff'::h3index END, 15) END
FROM generate_series(1,2000019) g(i);
ANALYZE _h3_large_input;

CREATE FUNCTION pg_temp.check_large_h3(expected_rows bigint, expected_groups bigint,
                                      resolution int) RETURNS void LANGUAGE plpgsql AS $$
DECLARE
    query text;
    bit_query text;
    plan text := '';
    line record;
    kernels bigint;
    eligible bool;
BEGIN
    query := format('SELECT h3_cell_to_parent(cell,%s) AS parent, count(*) AS n
                     FROM _h3_large_input GROUP BY 1', resolution);
    bit_query := format('SELECT CASE WHEN cell IS NULL THEN NULL::bigint ELSE
                       ((cell::bigint & ~(15::bigint << 52)) | (%s::bigint << 52) |
                        ((1::bigint << %s)-1)) END AS parent, count(*) n
                       FROM _h3_large_input GROUP BY 1', resolution, (15-resolution)*3);
    PERFORM set_config('pg_accel.enabled','off',false);
    EXECUTE 'CREATE TEMP TABLE _h3_large_expected AS ' || query;
    EXECUTE 'CREATE TEMP TABLE _h3_large_bits AS ' || bit_query;
    IF (SELECT count(*) FROM _h3_large_expected) <> expected_groups
       OR (SELECT sum(n) FROM _h3_large_expected) <> expected_rows
       OR EXISTS ((SELECT parent::bigint,n FROM _h3_large_expected EXCEPT ALL SELECT * FROM _h3_large_bits)
                  UNION ALL
                  (SELECT * FROM _h3_large_bits EXCEPT ALL SELECT parent::bigint,n FROM _h3_large_expected)) THEN
        RAISE EXCEPTION '113 independent H3 input/output invariant failed';
    END IF;
    SELECT gpu_available AND expected_rows <=
      (SELECT value::bigint FROM pg_accel_device_limits() WHERE name='gpu_h3_max_groups')
      INTO STRICT eligible FROM pg_accel_device_info();
    IF (SELECT gpu_available FROM pg_accel_device_info()) THEN
        PERFORM pg_accel_pin('_h3_large_input'::regclass,ARRAY['cell']);
    END IF;
    PERFORM set_config('pg_accel.enabled','on',false);
    SELECT pg_accel_kernel_executions() INTO kernels;
    EXECUTE 'CREATE TEMP TABLE _h3_large_actual AS ' || query;
    IF EXISTS ((SELECT * FROM _h3_large_expected EXCEPT ALL SELECT * FROM _h3_large_actual)
               UNION ALL
               (SELECT * FROM _h3_large_actual EXCEPT ALL SELECT * FROM _h3_large_expected)) THEN
        RAISE EXCEPTION '113 large H3 differential failed';
    END IF;
    FOR line IN EXECUTE 'EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) ' || query LOOP
        plan := plan || line."QUERY PLAN" || E'\n';
    END LOOP;
    IF eligible AND (position('GPU Kernel Dispatched: true' IN plan)=0
                     OR pg_accel_kernel_executions() <= kernels) THEN
        RAISE EXCEPTION '113 eligible large H3 query did not dispatch: %', plan;
    END IF;
    IF (SELECT stock_exec_count FROM pg_accel_stats()) <> 0 THEN
        RAISE EXCEPTION '113 used stock executor fallback';
    END IF;
    DROP TABLE _h3_large_expected, _h3_large_bits, _h3_large_actual;
END $$;

SELECT pg_temp.check_large_h3(2000019,3,9);
BEGIN;
UPDATE _h3_large_input SET cell=NULL WHERE cell IS NOT NULL;
SELECT pg_temp.check_large_h3(2000019,1,9);
ROLLBACK;
SELECT pg_temp.check_large_h3(2000019,3,9);
\echo 'PGACCEL_ASSERT_OK:113_h3_large_hash_contract.assert_001'

SELECT pg_accel_unpin('_h3_large_input'::regclass);
SELECT pg_accel_evict('_h3_large_input'::regclass);
SET pg_accel.enabled = off;
TRUNCATE _h3_large_input;
-- Base cell zero is hexagonal: 7^6 distinct children, six records per child.
INSERT INTO _h3_large_input
SELECT h3_cell_to_center_child(child,15)
FROM h3_cell_to_children('8001fffffffffff'::h3index,6) child
CROSS JOIN generate_series(1,6);
INSERT INTO _h3_large_input VALUES (NULL);
ANALYZE _h3_large_input;
DO $$ BEGIN
    IF (SELECT count(DISTINCT cell) FROM _h3_large_input) <> 117649 THEN
        RAISE EXCEPTION '113 high-cardinality fixture did not retain every child';
    END IF;
    PERFORM pg_temp.check_large_h3(705895,117650,6);
    PERFORM pg_temp.check_large_h3(705895,117650,6);
END $$;
\echo 'PGACCEL_ASSERT_OK:113_h3_large_hash_contract.assert_002'

SELECT pg_accel_unpin('_h3_large_input'::regclass);
SELECT pg_accel_evict('_h3_large_input'::regclass);
DROP FUNCTION pg_temp.check_large_h3(bigint,bigint,int);
DROP TABLE _h3_large_input;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:113_h3_large_hash_contract'
