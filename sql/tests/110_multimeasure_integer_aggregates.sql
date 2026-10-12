-- Independent exact SUM/NUMERIC AVG states across multiple int2/int4 columns.
\echo '=== 110_multimeasure_integer_aggregates ==='
SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
CREATE TEMP TABLE _multimeasure_input AS
SELECT i AS id,
       CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END AS kb,
       CASE WHEN i % 23 = 0 THEN NULL ELSE ('region-' || i % 7) END COLLATE "C" AS ks,
       date '2026-01-01' + i % 5 AS kd,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
            ELSE ((i % 65536) - 32768)::int2 END AS a,
       CASE WHEN i % 13 = 0 THEN NULL WHEN i % 2 = 0 THEN 2147483647
            ELSE -2147483648 END AS b,
       (i % 32768)::int2 AS c,
       NULL::int4 AS d
FROM generate_series(1, 300000) g(i);
ANALYZE _multimeasure_input;

DO $$
DECLARE
    has_gpu bool;
    keys text;
    outputs text;
    query text;
    plan text;
    line record;
    kernels_before bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_multimeasure_input'::regclass,
                            ARRAY['kb','ks','kd','a','b','c','d']);
    END IF;
    FOREACH keys IN ARRAY ARRAY['kb', 'ks, kd, kb'] LOOP
        FOREACH outputs IN ARRAY ARRAY[
            'sum(a) AS sa, avg(b) AS ab',
            'avg(a) AS aa, sum(b) AS sb, count(*) AS n',
            'sum(a) AS sa, avg(a) AS aa, sum(b) AS sb, avg(b) AS ab',
            'avg(c) AS ac, sum(b) AS sb, sum(a) AS sa, count(*) AS n',
            'avg(d) AS ad, sum(c) AS sc, avg(b) AS ab, sum(a) AS sa',
            'sum(a) AS sa, avg(a) AS aa, sum(b) AS sb, avg(b) AS ab,
             sum(c) AS sc, avg(c) AS ac, sum(d) AS sd, avg(d) AS ad'
        ] LOOP
            query := format('SELECT %s, %s FROM _multimeasure_input GROUP BY %s',
                            keys, outputs, keys);
            PERFORM set_config('pg_accel.enabled', 'off', false);
            EXECUTE 'CREATE TEMP TABLE _mm_expected AS ' || query;
            PERFORM set_config('pg_accel.enabled', 'on', false);
            kernels_before := pg_accel_kernel_executions();
            EXECUTE 'CREATE TEMP TABLE _mm_actual AS ' || query;
            IF EXISTS ((SELECT * FROM _mm_actual EXCEPT ALL SELECT * FROM _mm_expected)
                       UNION ALL
                       (SELECT * FROM _mm_expected EXCEPT ALL SELECT * FROM _mm_actual)) THEN
                RAISE EXCEPTION '110 multiple measure mismatch: %', query;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) ' || query LOOP
                plan := plan || line."QUERY PLAN" || E'\n';
            END LOOP;
            IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan) = 0
                            OR position('dense_multi_integer_sum_avg_plain' IN plan) = 0
                            OR position('parallel_dense_integer' IN plan) = 0
                            OR pg_accel_kernel_executions() <= kernels_before) THEN
                RAISE EXCEPTION '110 multiple measure missing dispatch: % / %', query, plan;
            END IF;
            DROP TABLE _mm_actual, _mm_expected;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:110_multimeasure_integer_aggregates.assert_001'

SET plan_cache_mode = force_generic_plan;
PREPARE _mm_q(numeric) AS
SELECT avg(b) AS ab, sum(c) AS sc FROM _multimeasure_input GROUP BY ks, kd, kb
HAVING avg(a) > $1 OR avg(d) IS NULL
ORDER BY ks NULLS LAST, kd, kb NULLS LAST LIMIT 13;
DO $$
DECLARE
    has_gpu bool;
    phase int;
    line record;
    plan text;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 1..4 LOOP
        IF phase = 2 THEN
            UPDATE _multimeasure_input SET ks = 'new-region', a = 17, b = NULL,
                                          c = -11, d = 42 WHERE id % 13 = 0;
            BEGIN
                UPDATE _multimeasure_input SET a = NULL, b = 0, c = 0, d = 0;
                RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'rollback fixture';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase = 3 THEN
            UPDATE _multimeasure_input SET a = NULL, b = NULL, c = NULL, d = NULL;
        ELSIF phase = 4 THEN
            DELETE FROM _multimeasure_input;
        END IF;
        PERFORM set_config('pg_accel.enabled', 'off', false);
        CREATE TEMP TABLE _mm_expected AS
        SELECT avg(b) AS ab, sum(c) AS sc FROM _multimeasure_input GROUP BY ks, kd, kb
        HAVING avg(a) > 0 OR avg(d) IS NULL
        ORDER BY ks NULLS LAST, kd, kb NULLS LAST LIMIT 13;
        PERFORM set_config('pg_accel.enabled', 'on', false);
        CREATE TEMP TABLE _mm_actual AS EXECUTE _mm_q(0);
        IF EXISTS ((SELECT * FROM _mm_actual EXCEPT ALL SELECT * FROM _mm_expected)
                   UNION ALL
                   (SELECT * FROM _mm_expected EXCEPT ALL SELECT * FROM _mm_actual)) THEN
            RAISE EXCEPTION '110 prepared multiple measure mismatch: phase %', phase;
        END IF;
        plan := '';
        FOR line IN EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) EXECUTE _mm_q(0) LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF has_gpu AND phase < 4 AND position('GPU Kernel Dispatched: true' IN plan) = 0 THEN
            RAISE EXCEPTION '110 prepared multiple measure did not redispatch: %', plan;
        END IF;
        DROP TABLE _mm_actual, _mm_expected;
    END LOOP;
    IF (SELECT stock_exec_count FROM pg_accel_stats()) <> 0 THEN
        RAISE EXCEPTION '110 unexpected stock aggregate fallback';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:110_multimeasure_integer_aggregates.assert_002'
DEALLOCATE _mm_q;
-- Exceed the one-shot row bound and the partial budget of a one-million-row
-- chunk. All four measure states must remain parallel in smaller chunks.
-- Input still fits resident memory; this is not a spill/streaming test.
INSERT INTO _multimeasure_input
SELECT i, i % 2 = 0, 'bounded-' || i % 1024, date '2026-01-01',
       CASE WHEN i % 11 = 0 THEN NULL ELSE (i % 32768)::int2 END,
       CASE WHEN i % 13 = 0 THEN NULL ELSE i % 10001 - 5000 END,
       (-i % 32768)::int2, NULL::int4
FROM generate_series(1, 1000017) g(i);
ANALYZE _multimeasure_input;
DO $$
DECLARE
    has_gpu bool;
    line record;
    plan text := '';
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    PERFORM set_config('pg_accel.enabled', 'off', false);
    CREATE TEMP TABLE _mm_expected AS
    SELECT ks, sum(a) sa, avg(b) ab, sum(c) sc, avg(d) ad
    FROM _multimeasure_input GROUP BY ks;
    PERFORM set_config('pg_accel.enabled', 'on', false);
    CREATE TEMP TABLE _mm_actual AS
    SELECT ks, sum(a) sa, avg(b) ab, sum(c) sc, avg(d) ad
    FROM _multimeasure_input GROUP BY ks;
    IF EXISTS ((SELECT * FROM _mm_actual EXCEPT ALL SELECT * FROM _mm_expected)
               UNION ALL
               (SELECT * FROM _mm_expected EXCEPT ALL SELECT * FROM _mm_actual)) THEN
        RAISE EXCEPTION '110 bounded multiple measure mismatch';
    END IF;
    FOR line IN EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF)
        SELECT ks, sum(a), avg(b), sum(c), avg(d) FROM _multimeasure_input GROUP BY ks
    LOOP
        plan := plan || line."QUERY PLAN" || E'\n';
    END LOOP;
    IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan) = 0
                   OR position('parallel_dense_integer' IN plan) = 0
                   OR coalesce(substring(plan from 'Batches: ([0-9]+)')::bigint, 0) <= 1) THEN
        RAISE EXCEPTION '110 bounded multiple measure missing parallel batches: %', plan;
    END IF;
    DROP TABLE _mm_actual, _mm_expected;
END $$;
\echo 'PGACCEL_ASSERT_OK:110_multimeasure_integer_aggregates.assert_003'
SELECT pg_accel_unpin('_multimeasure_input'::regclass);
SELECT pg_accel_evict('_multimeasure_input'::regclass);
DROP TABLE _multimeasure_input;
RESET plan_cache_mode;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:110_multimeasure_integer_aggregates'
