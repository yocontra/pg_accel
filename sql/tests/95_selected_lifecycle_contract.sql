-- 95_selected_lifecycle_contract.sql: lifecycle coverage for every released
-- selected resident family.  Each case binds a prepared statement, mutates a
-- pinned input, proves invalidation, refreshes it, performs safe DDL, and then
-- compares the rebuilt result with a fresh PostgreSQL/PostGIS oracle.

\echo '=== 95_selected_lifecycle_contract ==='

SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
SET plan_cache_mode = force_generic_plan;

CREATE TEMP TABLE _lifecycle_environment AS
SELECT gpu_available AS has_gpu,
       EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'h3') AS has_h3
FROM pg_accel_device_info();

CREATE TEMP TABLE _lifecycle_plan (
    family text NOT NULL,
    stage text NOT NULL,
    line text NOT NULL
);
CREATE TEMP TABLE _lifecycle_rejection (
    family text NOT NULL,
    stage text NOT NULL,
    reason text
);

CREATE FUNCTION pg_temp.lifecycle_generation(target regclass)
RETURNS bigint
LANGUAGE SQL
AS $$
    SELECT generation
    FROM pg_accel_resident_status()
    WHERE relid = target::oid
$$;

CREATE FUNCTION pg_temp.lifecycle_assert_invalidated(
    target regclass,
    previous_generation bigint,
    require_generation_advance boolean DEFAULT true
)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    status record;
BEGIN
    SELECT * INTO STRICT status
    FROM pg_accel_resident_status()
    WHERE relid = target::oid;

    IF NOT status.pinned
       OR status.raw_bytes <> 0
       OR status.derived_bytes <> 0
       OR (require_generation_advance AND status.generation <= previous_generation)
       OR (NOT require_generation_advance AND status.generation < previous_generation) THEN
        RAISE EXCEPTION
            '95 lifecycle invalidation FAILED for %: pinned %, raw %, derived %, generation % -> %',
            target, status.pinned, status.raw_bytes, status.derived_bytes,
            previous_generation, status.generation;
    END IF;
END $$;

CREATE FUNCTION pg_temp.lifecycle_refresh(target regclass, expected_rows bigint)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    refreshed_rows bigint;
    status record;
BEGIN
    SELECT pg_accel_refresh(target) INTO STRICT refreshed_rows;
    IF refreshed_rows <> expected_rows THEN
        RAISE EXCEPTION
            '95 lifecycle refresh FAILED for %: rows %, expected %',
            target, refreshed_rows, expected_rows;
    END IF;
    SELECT * INTO STRICT status
    FROM pg_accel_resident_status()
    WHERE relid = target::oid;
    IF NOT status.pinned OR status.raw_bytes <= 0 THEN
        RAISE EXCEPTION
            '95 lifecycle refresh FAILED for %: pinned %, raw %',
            target, status.pinned, status.raw_bytes;
    END IF;
END $$;

CREATE FUNCTION pg_temp.lifecycle_refresh_if_gpu(
    target regclass,
    expected_rows bigint
)
RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    IF (SELECT env.has_gpu FROM _lifecycle_environment AS env) THEN
        PERFORM pg_temp.lifecycle_refresh(target, expected_rows);
    END IF;
END $$;

CREATE FUNCTION pg_temp.lifecycle_explain(
    selected_family text,
    selected_stage text,
    statement_name text
)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    plan_row record;
BEGIN
    DELETE FROM _lifecycle_plan
    WHERE family = selected_family AND stage = selected_stage;
    DELETE FROM _lifecycle_rejection
    WHERE family = selected_family AND stage = selected_stage;
    FOR plan_row IN EXECUTE format(
        'EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF) EXECUTE %I',
        statement_name
    ) LOOP
        INSERT INTO _lifecycle_plan VALUES (
            selected_family, selected_stage, plan_row."QUERY PLAN"
        );
    END LOOP;
    INSERT INTO _lifecycle_rejection
    VALUES (
        selected_family,
        selected_stage,
        pg_accel_last_planner_rejection_reason()
    );
END $$;

CREATE FUNCTION pg_temp.lifecycle_assert_dispatch(
    selected_family text,
    selected_stage text,
    kernels_before bigint,
    require_new_dispatch boolean DEFAULT true
)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    has_gpu boolean;
    kernels_after bigint;
    stock_after bigint;
    plan_text text;
    rejection_reason text;
BEGIN
    SELECT env.has_gpu INTO STRICT has_gpu FROM _lifecycle_environment AS env;
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    SELECT string_agg(line, E'\n' ORDER BY line) INTO plan_text
    FROM _lifecycle_plan
    WHERE family = selected_family AND stage = selected_stage;
    SELECT reason INTO STRICT rejection_reason
    FROM _lifecycle_rejection
    WHERE family = selected_family AND stage = selected_stage;

    IF stock_after <> 0 THEN
        RAISE EXCEPTION
            '95 % % FAILED: stock fallback count is %',
            selected_family, selected_stage, stock_after;
    END IF;
    IF has_gpu THEN
        IF (NOT EXISTS (
            SELECT 1 FROM _lifecycle_plan
            WHERE family = selected_family AND stage = selected_stage
              AND line LIKE '%Custom Scan (%GpuAccel%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _lifecycle_plan
            WHERE family = selected_family AND stage = selected_stage
              AND line LIKE '%Plan Selected: true%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _lifecycle_plan
            WHERE family = selected_family AND stage = selected_stage
              AND line LIKE '%GPU Resident Pipeline: true%'
        )) AND require_new_dispatch THEN
            IF NOT EXISTS (
                SELECT 1 FROM _lifecycle_plan
                WHERE family = selected_family AND stage = selected_stage
                  AND line LIKE '%Custom Scan (%GpuAccel%'
            ) AND rejection_reason IN (
                'generic_cost_not_competitive',
                'no_gpu_resident_pipeline'
            )
               AND kernels_after >= kernels_before THEN
                RETURN;
            END IF;
            RAISE EXCEPTION
                '95 % % FAILED: selected resident proof incomplete (kernels % -> %, rejection %), plan:%',
                selected_family, selected_stage, kernels_before, kernels_after,
                rejection_reason, E'\n' || coalesce(plan_text, '<missing>');
        ELSIF (NOT EXISTS (
            SELECT 1 FROM _lifecycle_plan
            WHERE family = selected_family AND stage = selected_stage
              AND line LIKE '%Custom Scan (%GpuAccel%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _lifecycle_plan
            WHERE family = selected_family AND stage = selected_stage
              AND line LIKE '%Plan Selected: true%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _lifecycle_plan
            WHERE family = selected_family AND stage = selected_stage
              AND line LIKE '%GPU Resident Pipeline: true%'
        )) AND NOT require_new_dispatch THEN
            IF EXISTS (
                SELECT 1 FROM _lifecycle_plan
                WHERE family = selected_family AND stage = selected_stage
                  AND line LIKE '%Custom Scan (%GpuAccel%'
            ) OR rejection_reason IS DISTINCT FROM 'no_gpu_resident_pipeline'
               OR kernels_after < kernels_before THEN
                RAISE EXCEPTION
                    '95 % % FAILED: DDL did not select or decline exactly (kernels % -> %, rejection %), plan:%',
                    selected_family, selected_stage, kernels_before, kernels_after,
                    rejection_reason, E'\n' || coalesce(plan_text, '<missing>');
            END IF;
            RETURN;
        END IF;
        IF require_new_dispatch THEN
            IF NOT EXISTS (
                SELECT 1 FROM _lifecycle_plan
                WHERE family = selected_family AND stage = selected_stage
                  AND line LIKE '%GPU Kernel Dispatched: true%'
            ) OR kernels_after <= kernels_before THEN
                RAISE EXCEPTION
                    '95 % % FAILED: DML rebuild did not dispatch (kernels % -> %)',
                    selected_family, selected_stage, kernels_before, kernels_after;
            END IF;
        ELSIF NOT EXISTS (
            SELECT 1 FROM _lifecycle_plan
            WHERE family = selected_family AND stage = selected_stage
              AND (line LIKE '%GPU Kernel Dispatched: true%'
                   OR line LIKE '%GPU Descriptor Artifact: hit%')
        ) OR kernels_after < kernels_before THEN
            RAISE EXCEPTION
                '95 % % FAILED: DDL execution proved neither dispatch nor safe artifact reuse (kernels % -> %)',
                selected_family, selected_stage, kernels_before, kernels_after;
        END IF;
    ELSE
        IF EXISTS (
            SELECT 1 FROM _lifecycle_plan
            WHERE family = selected_family AND stage = selected_stage
              AND line LIKE '%Custom Scan (%GpuAccel%'
        ) OR kernels_after <> kernels_before THEN
            RAISE EXCEPTION
                '95 % % FAILED: unavailable host selected or dispatched (kernels % -> %)',
                selected_family, selected_stage, kernels_before, kernels_after;
        END IF;
    END IF;
END $$;

-- Grouped int4 and filtered-expression aggregate.  NULL group keys and NULL
-- measures exercise aggregate sidecars; NULL predicates remain SQL UNKNOWN.
DROP TABLE IF EXISTS _lifecycle_grouped;
CREATE UNLOGGED TABLE _lifecycle_grouped (
    id int4 PRIMARY KEY,
    g int4,
    v int4,
    price int4,
    quantity int4,
    active boolean,
    bg boolean,
    s int2
);
INSERT INTO _lifecycle_grouped
SELECT i,
       CASE WHEN i % 101 = 0 THEN NULL ELSE i % 64 END,
       CASE WHEN i % 97 = 0 THEN NULL ELSE i % 1000 END,
       CASE WHEN i % 89 = 0 THEN NULL ELSE 1 + i % 1000 END,
       CASE WHEN i % 83 = 0 THEN NULL ELSE 1 + (i / 64) % 10 END,
       CASE WHEN i % 79 = 0 THEN NULL ELSE (i % 10 = 0) END,
       CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
            ELSE ((i % 65536) - 32768)::int2 END
FROM generate_series(1, 500000) AS rows(i);
ANALYZE _lifecycle_grouped;

PREPARE _lifecycle_grouped_q AS
SELECT g, sum(v) AS total, count(*) AS rows
FROM _lifecycle_grouped GROUP BY g;
PREPARE _lifecycle_predicate_q AS
SELECT g, sum(price * quantity) AS total, count(*) AS rows
FROM _lifecycle_grouped WHERE active GROUP BY g;
PREPARE _lifecycle_range_q AS
SELECT g, sum(price * quantity) AS total, count(*) AS rows
FROM _lifecycle_grouped
WHERE price >= 200 AND price <= 800
GROUP BY g;
PREPARE _lifecycle_aggregate_filter_q AS
SELECT g,
       sum(price) FILTER (WHERE price >= 200 AND price <= 800) AS total,
       count(*) AS rows
FROM _lifecycle_grouped
GROUP BY g;
PREPARE _lifecycle_int2_count_q AS
SELECT bg, count(s) AS observed_rows
FROM _lifecycle_grouped
GROUP BY bg;

DO $$
DECLARE
    has_gpu boolean;
    generation_before bigint;
    kernels_before bigint;
BEGIN
    SELECT env.has_gpu INTO STRICT has_gpu FROM _lifecycle_environment AS env;
    IF has_gpu THEN
        PERFORM pg_accel_pin(
            '_lifecycle_grouped'::regclass,
            ARRAY['g', 'v', 'price', 'quantity', 'active', 'bg', 's']
        );
        generation_before := pg_temp.lifecycle_generation('_lifecycle_grouped');
    END IF;

    INSERT INTO _lifecycle_grouped
    SELECT i,
           CASE WHEN i % 17 = 0 THEN NULL ELSE i % 64 END,
           CASE WHEN i % 19 = 0 THEN NULL ELSE -(i % 1000) END,
           CASE WHEN i % 23 = 0 THEN NULL ELSE i % 1000 END,
           CASE WHEN i % 29 = 0 THEN NULL ELSE 2 END,
           CASE WHEN i % 31 = 0 THEN NULL ELSE true END,
           CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END,
           CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
                ELSE ((i % 65536) - 32768)::int2 END
    FROM generate_series(500001, 504096) AS rows(i);
    ANALYZE _lifecycle_grouped;
    IF has_gpu THEN
        PERFORM pg_temp.lifecycle_assert_invalidated(
            '_lifecycle_grouped', generation_before
        );
        PERFORM pg_temp.lifecycle_refresh('_lifecycle_grouped', 504096);
        generation_before := pg_temp.lifecycle_generation('_lifecycle_grouped');
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;
    PERFORM pg_temp.lifecycle_explain(
        'grouped_int4', 'prepared_after_dml', '_lifecycle_grouped_q'
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'grouped_int4', 'prepared_after_dml', kernels_before
    );
    PERFORM pg_temp.lifecycle_explain(
        'predicate_expression', 'prepared_after_dml', '_lifecycle_predicate_q'
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'predicate_expression', 'prepared_after_dml', kernels_before
    );
    PERFORM pg_temp.lifecycle_explain(
        'range_intersection', 'prepared_after_dml', '_lifecycle_range_q'
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'range_intersection', 'prepared_after_dml', kernels_before
    );
    PERFORM pg_temp.lifecycle_explain(
        'aggregate_filter', 'prepared_after_dml', '_lifecycle_aggregate_filter_q'
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'aggregate_filter', 'prepared_after_dml', kernels_before
    );
    PERFORM pg_temp.lifecycle_explain(
        'int2_count', 'prepared_after_dml', '_lifecycle_int2_count_q'
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'int2_count', 'prepared_after_dml', kernels_before
    );

    ALTER TABLE _lifecycle_grouped ADD COLUMN lifecycle_tag int4 DEFAULT 0;
    IF has_gpu THEN
        PERFORM pg_temp.lifecycle_assert_invalidated(
            '_lifecycle_grouped', generation_before, false
        );
        PERFORM pg_temp.lifecycle_refresh('_lifecycle_grouped', 504096);
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _lifecycle_grouped_native AS
SELECT g, sum(v) AS total, count(*) AS rows
FROM _lifecycle_grouped GROUP BY g;
CREATE TEMP TABLE _lifecycle_predicate_native AS
SELECT g, sum(price * quantity) AS total, count(*) AS rows
FROM _lifecycle_grouped WHERE active GROUP BY g;
CREATE TEMP TABLE _lifecycle_range_native AS
SELECT g, sum(price * quantity) AS total, count(*) AS rows
FROM _lifecycle_grouped
WHERE price >= 200 AND price <= 800
GROUP BY g;
CREATE TEMP TABLE _lifecycle_aggregate_filter_native AS
SELECT g,
       sum(price) FILTER (WHERE price >= 200 AND price <= 800) AS total,
       count(*) AS rows
FROM _lifecycle_grouped
GROUP BY g;
CREATE TEMP TABLE _lifecycle_int2_count_native AS
SELECT bg, count(s) AS observed_rows
FROM _lifecycle_grouped
GROUP BY bg;

SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _lifecycle_grouped_before AS
SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _lifecycle_grouped_accel AS EXECUTE _lifecycle_grouped_q;
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_grouped', 504096);
SELECT pg_temp.lifecycle_explain('grouped_int4', 'prepared_after_ddl', '_lifecycle_grouped_q');
CREATE TEMP TABLE _lifecycle_predicate_accel AS EXECUTE _lifecycle_predicate_q;
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_grouped', 504096);
SELECT pg_temp.lifecycle_explain('predicate_expression', 'prepared_after_ddl', '_lifecycle_predicate_q');
CREATE TEMP TABLE _lifecycle_range_accel AS EXECUTE _lifecycle_range_q;
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_grouped', 504096);
SELECT pg_temp.lifecycle_explain('range_intersection', 'prepared_after_ddl', '_lifecycle_range_q');
CREATE TEMP TABLE _lifecycle_aggregate_filter_accel AS EXECUTE _lifecycle_aggregate_filter_q;
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_grouped', 504096);
SELECT pg_temp.lifecycle_explain('aggregate_filter', 'prepared_after_ddl', '_lifecycle_aggregate_filter_q');
CREATE TEMP TABLE _lifecycle_int2_count_accel AS EXECUTE _lifecycle_int2_count_q;
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_grouped', 504096);
SELECT pg_temp.lifecycle_explain('int2_count', 'prepared_after_ddl', '_lifecycle_int2_count_q');

DO $$
DECLARE
    kernels_before bigint;
BEGIN
    IF EXISTS (
        (SELECT * FROM _lifecycle_grouped_native
         EXCEPT ALL SELECT * FROM _lifecycle_grouped_accel)
        UNION ALL
        (SELECT * FROM _lifecycle_grouped_accel
         EXCEPT ALL SELECT * FROM _lifecycle_grouped_native)
    ) OR EXISTS (
        (SELECT * FROM _lifecycle_predicate_native
         EXCEPT ALL SELECT * FROM _lifecycle_predicate_accel)
        UNION ALL
        (SELECT * FROM _lifecycle_predicate_accel
         EXCEPT ALL SELECT * FROM _lifecycle_predicate_native)
    ) OR EXISTS (
        (SELECT * FROM _lifecycle_range_native
         EXCEPT ALL SELECT * FROM _lifecycle_range_accel)
        UNION ALL
        (SELECT * FROM _lifecycle_range_accel
         EXCEPT ALL SELECT * FROM _lifecycle_range_native)
    ) OR EXISTS (
        (SELECT * FROM _lifecycle_aggregate_filter_native
         EXCEPT ALL SELECT * FROM _lifecycle_aggregate_filter_accel)
        UNION ALL
        (SELECT * FROM _lifecycle_aggregate_filter_accel
         EXCEPT ALL SELECT * FROM _lifecycle_aggregate_filter_native)
    ) OR EXISTS (
        (SELECT * FROM _lifecycle_int2_count_native
         EXCEPT ALL SELECT * FROM _lifecycle_int2_count_accel)
        UNION ALL
        (SELECT * FROM _lifecycle_int2_count_accel
         EXCEPT ALL SELECT * FROM _lifecycle_int2_count_native)
    ) THEN
        RAISE EXCEPTION '95 grouped/predicate/range/FILTER lifecycle FAILED: native results differ';
    END IF;
    SELECT kernels INTO STRICT kernels_before FROM _lifecycle_grouped_before;
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'grouped_int4', 'prepared_after_ddl', kernels_before, false
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'predicate_expression', 'prepared_after_ddl', kernels_before, false
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'range_intersection', 'prepared_after_ddl', kernels_before, false
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'aggregate_filter', 'prepared_after_ddl', kernels_before, false
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'int2_count', 'prepared_after_ddl', kernels_before, false
    );
    IF (SELECT env.has_gpu FROM _lifecycle_environment AS env)
       AND NOT EXISTS (
           SELECT 1 FROM _lifecycle_plan
           WHERE family = 'range_intersection'
             AND stage IN ('prepared_after_dml', 'prepared_after_ddl')
             AND line LIKE '%GPU Physical Kernel Mode: parallel_dense_integer%'
       ) THEN
        RAISE EXCEPTION
            '95 range lifecycle FAILED: physical fast path not reported';
    END IF;
    IF (SELECT env.has_gpu FROM _lifecycle_environment AS env)
       AND NOT EXISTS (
           SELECT 1 FROM _lifecycle_plan
           WHERE family = 'aggregate_filter'
             AND stage IN ('prepared_after_dml', 'prepared_after_ddl')
             AND line LIKE '%GPU Descriptor Specialization: dense_integer_column_measure_range%'
       ) THEN
        RAISE EXCEPTION
            '95 aggregate FILTER lifecycle FAILED: specialization not reported';
    END IF;
    IF (SELECT env.has_gpu FROM _lifecycle_environment AS env)
       AND NOT EXISTS (
           SELECT 1 FROM _lifecycle_plan
           WHERE family = 'int2_count'
             AND stage IN ('prepared_after_dml', 'prepared_after_ddl')
             AND line LIKE '%GPU Descriptor Specialization: dense_int2_count_plain%'
       ) THEN
        RAISE EXCEPTION
            '95 int2 COUNT lifecycle FAILED: specialization not reported';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:95_selected_lifecycle_contract.assert_001'

-- Count-only and four-dimension int4 star families share one fact fixture.
DROP TABLE IF EXISTS _lifecycle_fact4;
DROP TABLE IF EXISTS _lifecycle_dim4a;
DROP TABLE IF EXISTS _lifecycle_dim4b;
DROP TABLE IF EXISTS _lifecycle_dim4c;
DROP TABLE IF EXISTS _lifecycle_dim4d;
CREATE UNLOGGED TABLE _lifecycle_dim4a (id int4 PRIMARY KEY, label int4);
CREATE UNLOGGED TABLE _lifecycle_dim4b (id int4 PRIMARY KEY, label int4);
CREATE UNLOGGED TABLE _lifecycle_dim4c (id int4 PRIMARY KEY, label int4);
CREATE UNLOGGED TABLE _lifecycle_dim4d (id int4 PRIMARY KEY, label int4);
INSERT INTO _lifecycle_dim4a SELECT i, CASE WHEN i = 17 THEN NULL ELSE i % 4 END FROM generate_series(1,100) i;
INSERT INTO _lifecycle_dim4b SELECT i, CASE WHEN i = 19 THEN NULL ELSE i % 5 END FROM generate_series(1,100) i;
INSERT INTO _lifecycle_dim4c SELECT i, i % 3 FROM generate_series(1,100) i;
INSERT INTO _lifecycle_dim4d SELECT i, i % 7 FROM generate_series(1,100) i;
CREATE UNLOGGED TABLE _lifecycle_fact4 AS
SELECT i::int4 AS id,
       CASE WHEN i % 101 = 0 THEN NULL ELSE 1 + (i - 1) % 100 END::int4 AS k1,
       CASE WHEN i % 103 = 0 THEN NULL ELSE 1 + (i - 1) % 100 END::int4 AS k2,
       (1 + (i - 1) % 100)::int4 AS k3,
       (1 + (i - 1) % 100)::int4 AS k4,
       CASE WHEN i % 97 = 0 THEN NULL ELSE 1 + i % 1000 END::int4 AS amount
FROM generate_series(1, 500000) rows(i);
ANALYZE _lifecycle_fact4;
ANALYZE _lifecycle_dim4a; ANALYZE _lifecycle_dim4b;
ANALYZE _lifecycle_dim4c; ANALYZE _lifecycle_dim4d;

PREPARE _lifecycle_count4_q AS
SELECT count(*) AS rows
FROM _lifecycle_fact4 fact JOIN _lifecycle_dim4a dim ON fact.k1 = dim.id;
PREPARE _lifecycle_star4_q AS
SELECT a.label AS key1, b.label AS key2, c.label AS key3,
       sum(fact.amount) AS total, count(*) AS rows,
       min(fact.amount) AS minimum, max(fact.amount) AS maximum
FROM _lifecycle_fact4 fact
JOIN _lifecycle_dim4a a ON fact.k1 = a.id
JOIN _lifecycle_dim4b b ON fact.k2 = b.id
JOIN _lifecycle_dim4c c ON fact.k3 = c.id
JOIN _lifecycle_dim4d d ON fact.k4 = d.id
GROUP BY a.label, b.label, c.label;

DO $$
DECLARE
    has_gpu boolean;
    fact_generation bigint;
    dim_generation bigint;
    kernels_before bigint;
BEGIN
    SELECT env.has_gpu INTO STRICT has_gpu FROM _lifecycle_environment env;
    IF has_gpu THEN
        PERFORM pg_accel_pin('_lifecycle_fact4', ARRAY['k1','k2','k3','k4','amount']);
        PERFORM pg_accel_pin('_lifecycle_dim4a', ARRAY['id','label']);
        PERFORM pg_accel_pin('_lifecycle_dim4b', ARRAY['id','label']);
        PERFORM pg_accel_pin('_lifecycle_dim4c', ARRAY['id','label']);
        PERFORM pg_accel_pin('_lifecycle_dim4d', ARRAY['id','label']);
        fact_generation := pg_temp.lifecycle_generation('_lifecycle_fact4');
    END IF;
    UPDATE _lifecycle_fact4
    SET amount = CASE WHEN id % 11 = 0 THEN NULL ELSE amount + 7 END
    WHERE id <= 4096;
    ANALYZE _lifecycle_fact4;
    IF has_gpu THEN
        PERFORM pg_temp.lifecycle_assert_invalidated('_lifecycle_fact4', fact_generation);
        PERFORM pg_temp.lifecycle_refresh('_lifecycle_fact4', 500000);
        dim_generation := pg_temp.lifecycle_generation('_lifecycle_dim4a');
    END IF;
    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;
    PERFORM pg_temp.lifecycle_explain(
        'count_star_int4', 'prepared_after_dml', '_lifecycle_count4_q'
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'count_star_int4', 'prepared_after_dml', kernels_before
    );
    PERFORM pg_temp.lifecycle_explain(
        'star_join_int4', 'prepared_after_dml', '_lifecycle_star4_q'
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'star_join_int4', 'prepared_after_dml', kernels_before
    );
    ALTER TABLE _lifecycle_dim4a ADD COLUMN lifecycle_tag int4 DEFAULT 0;
    IF has_gpu THEN
        PERFORM pg_temp.lifecycle_assert_invalidated(
            '_lifecycle_dim4a', dim_generation, false
        );
        PERFORM pg_temp.lifecycle_refresh('_lifecycle_dim4a', 100);
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _lifecycle_count4_native AS
SELECT count(*) AS rows
FROM _lifecycle_fact4 fact JOIN _lifecycle_dim4a dim ON fact.k1 = dim.id;
CREATE TEMP TABLE _lifecycle_star4_native AS
SELECT a.label AS key1, b.label AS key2, c.label AS key3,
       sum(fact.amount) AS total, count(*) AS rows,
       min(fact.amount) AS minimum, max(fact.amount) AS maximum
FROM _lifecycle_fact4 fact
JOIN _lifecycle_dim4a a ON fact.k1 = a.id
JOIN _lifecycle_dim4b b ON fact.k2 = b.id
JOIN _lifecycle_dim4c c ON fact.k3 = c.id
JOIN _lifecycle_dim4d d ON fact.k4 = d.id
GROUP BY a.label, b.label, c.label;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _lifecycle_int4_before AS SELECT pg_accel_kernel_executions() kernels;
CREATE TEMP TABLE _lifecycle_count4_accel AS EXECUTE _lifecycle_count4_q;
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_fact4', 500000);
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_dim4a', 100);
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_dim4b', 100);
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_dim4c', 100);
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_dim4d', 100);
SELECT pg_temp.lifecycle_explain('count_star_int4', 'prepared_after_ddl', '_lifecycle_count4_q');
CREATE TEMP TABLE _lifecycle_star4_accel AS EXECUTE _lifecycle_star4_q;
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_fact4', 500000);
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_dim4a', 100);
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_dim4b', 100);
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_dim4c', 100);
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_dim4d', 100);
SELECT pg_temp.lifecycle_explain('star_join_int4', 'prepared_after_ddl', '_lifecycle_star4_q');

DO $$
DECLARE kernels_before bigint;
BEGIN
    IF EXISTS (
        (SELECT * FROM _lifecycle_count4_native EXCEPT ALL SELECT * FROM _lifecycle_count4_accel)
        UNION ALL
        (SELECT * FROM _lifecycle_count4_accel EXCEPT ALL SELECT * FROM _lifecycle_count4_native)
    ) OR EXISTS (
        (SELECT * FROM _lifecycle_star4_native EXCEPT ALL SELECT * FROM _lifecycle_star4_accel)
        UNION ALL
        (SELECT * FROM _lifecycle_star4_accel EXCEPT ALL SELECT * FROM _lifecycle_star4_native)
    ) THEN
        RAISE EXCEPTION '95 int4 join lifecycle FAILED: native results differ';
    END IF;
    SELECT kernels INTO STRICT kernels_before FROM _lifecycle_int4_before;
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'count_star_int4', 'prepared_after_ddl', kernels_before, false
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'star_join_int4', 'prepared_after_ddl', kernels_before, false
    );
END $$;
\echo 'PGACCEL_ASSERT_OK:95_selected_lifecycle_contract.assert_002'

-- Two unique int8 membership dimensions with nullable fact keys and repeated,
-- non-null int4 payloads. This is the exact released INT8 star family.
DROP TABLE IF EXISTS _lifecycle_fact8;
DROP TABLE IF EXISTS _lifecycle_dim8a;
DROP TABLE IF EXISTS _lifecycle_dim8b;
CREATE UNLOGGED TABLE _lifecycle_dim8a (id int8 PRIMARY KEY, label int4 NOT NULL);
CREATE UNLOGGED TABLE _lifecycle_dim8b (id int8 PRIMARY KEY, label int4 NOT NULL);
INSERT INTO _lifecycle_dim8a SELECT i, i % 7 FROM generate_series(1,100) i;
INSERT INTO _lifecycle_dim8b SELECT i, i % 11 FROM generate_series(1,100) i;
CREATE UNLOGGED TABLE _lifecycle_fact8 AS
SELECT i::int4 AS id,
       CASE WHEN i % 101 = 0 THEN NULL ELSE 1 + (i - 1) % 100 END::int8 AS k1,
       CASE WHEN i % 103 = 0 THEN NULL ELSE 1 + (i + 7) % 100 END::int8 AS k2,
       CASE WHEN i % 97 = 0 THEN NULL ELSE 1 + i % 1000 END::int4 AS amount
FROM generate_series(1, 500000) rows(i);
ANALYZE _lifecycle_fact8; ANALYZE _lifecycle_dim8a; ANALYZE _lifecycle_dim8b;

PREPARE _lifecycle_star8_q AS
SELECT a.label AS key1, b.label AS key2, sum(fact.amount) AS total, count(*) AS rows
FROM _lifecycle_fact8 fact
JOIN _lifecycle_dim8a a ON fact.k1 = a.id
JOIN _lifecycle_dim8b b ON fact.k2 = b.id
GROUP BY a.label, b.label;

DO $$
DECLARE
    has_gpu boolean;
    fact_generation bigint;
    dim_generation bigint;
    kernels_before bigint;
BEGIN
    SELECT env.has_gpu INTO STRICT has_gpu FROM _lifecycle_environment env;
    IF has_gpu THEN
        PERFORM pg_accel_pin('_lifecycle_fact8', ARRAY['k1','k2','amount']);
        PERFORM pg_accel_pin('_lifecycle_dim8a', ARRAY['id','label']);
        PERFORM pg_accel_pin('_lifecycle_dim8b', ARRAY['id','label']);
        fact_generation := pg_temp.lifecycle_generation('_lifecycle_fact8');
    END IF;
    UPDATE _lifecycle_fact8
    SET k1 = CASE WHEN id % 13 = 0 THEN NULL ELSE k1 END,
        k2 = CASE WHEN id % 19 = 0 THEN NULL ELSE k2 END,
        amount = CASE WHEN id % 17 = 0 THEN NULL ELSE amount + 3 END
    WHERE id <= 4096;
    ANALYZE _lifecycle_fact8;
    IF has_gpu THEN
        PERFORM pg_temp.lifecycle_assert_invalidated('_lifecycle_fact8', fact_generation);
        PERFORM pg_temp.lifecycle_refresh('_lifecycle_fact8', 500000);
        dim_generation := pg_temp.lifecycle_generation('_lifecycle_dim8a');
    END IF;
    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;
    PERFORM pg_temp.lifecycle_explain(
        'star_join_int8_membership', 'prepared_after_dml', '_lifecycle_star8_q'
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'star_join_int8_membership', 'prepared_after_dml', kernels_before
    );
    ALTER TABLE _lifecycle_dim8a ADD COLUMN lifecycle_tag int4 DEFAULT 0;
    IF has_gpu THEN
        PERFORM pg_temp.lifecycle_assert_invalidated(
            '_lifecycle_dim8a', dim_generation, false
        );
        PERFORM pg_temp.lifecycle_refresh('_lifecycle_dim8a', 100);
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _lifecycle_star8_native AS
SELECT a.label AS key1, b.label AS key2, sum(fact.amount) AS total, count(*) AS rows
FROM _lifecycle_fact8 fact
JOIN _lifecycle_dim8a a ON fact.k1 = a.id
JOIN _lifecycle_dim8b b ON fact.k2 = b.id
GROUP BY a.label, b.label;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _lifecycle_int8_before AS SELECT pg_accel_kernel_executions() kernels;
CREATE TEMP TABLE _lifecycle_star8_accel AS EXECUTE _lifecycle_star8_q;
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_fact8', 500000);
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_dim8a', 100);
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_dim8b', 100);
SELECT pg_temp.lifecycle_explain('star_join_int8_membership', 'prepared_after_ddl', '_lifecycle_star8_q');

DO $$
DECLARE kernels_before bigint;
BEGIN
    IF EXISTS (
        (SELECT * FROM _lifecycle_star8_native EXCEPT ALL SELECT * FROM _lifecycle_star8_accel)
        UNION ALL
        (SELECT * FROM _lifecycle_star8_accel EXCEPT ALL SELECT * FROM _lifecycle_star8_native)
    ) THEN
        RAISE EXCEPTION '95 int8 membership lifecycle FAILED: native results differ';
    END IF;
    SELECT kernels INTO STRICT kernels_before FROM _lifecycle_int8_before;
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'star_join_int8_membership', 'prepared_after_ddl', kernels_before, false
    );
END $$;
\echo 'PGACCEL_ASSERT_OK:95_selected_lifecycle_contract.assert_003'

-- Optional H3 parent grouped-count lifecycle.  The NULL cell group must remain
-- present and prepared execution must rebuild after both relation mutations.
DO $$
DECLARE
    has_gpu boolean;
    has_h3 boolean;
    generation_before bigint;
    kernels_before bigint;
    differs boolean;
BEGIN
    SELECT env.has_gpu, env.has_h3 INTO STRICT has_gpu, has_h3
    FROM _lifecycle_environment env;
    IF has_h3 THEN
        EXECUTE $setup$
            DROP TABLE IF EXISTS _lifecycle_h3;
            CREATE UNLOGGED TABLE _lifecycle_h3 (id int4 PRIMARY KEY, cell h3index);
            WITH seeds AS (
                SELECT array_agg(cell) AS cells
                FROM h3_grid_disk(
                    h3_cell_to_parent('8928308280fffff'::h3index, 2), 2
                ) AS disk(cell)
            )
            INSERT INTO _lifecycle_h3
            SELECT i,
                   CASE WHEN i % 97 = 0 THEN NULL::h3index
                        ELSE cells[1 + ((i - 1) % cardinality(cells))]
                   END
            FROM generate_series(1, 500000) rows(i) CROSS JOIN seeds;
            ANALYZE _lifecycle_h3;
            PREPARE _lifecycle_h3_q AS
            SELECT h3_cell_to_parent(cell, 0) AS parent, count(*) AS rows
            FROM _lifecycle_h3 GROUP BY 1
        $setup$;
        IF has_gpu THEN
            PERFORM pg_accel_pin('_lifecycle_h3'::regclass, ARRAY['cell']);
            generation_before := pg_temp.lifecycle_generation('_lifecycle_h3');
        END IF;
        EXECUTE 'UPDATE _lifecycle_h3 SET cell = NULL WHERE id <= 4096 AND id % 13 = 0';
        EXECUTE 'ANALYZE _lifecycle_h3';
        IF has_gpu THEN
            PERFORM pg_temp.lifecycle_assert_invalidated(
                '_lifecycle_h3', generation_before
            );
            PERFORM pg_temp.lifecycle_refresh('_lifecycle_h3', 500000);
            generation_before := pg_temp.lifecycle_generation('_lifecycle_h3');
        END IF;
        PERFORM set_config('pg_accel.enabled', 'on', false);
        PERFORM pg_accel_reset_stats();
        SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;
        PERFORM pg_temp.lifecycle_explain(
            'h3_parent', 'prepared_after_dml', '_lifecycle_h3_q'
        );
        PERFORM pg_temp.lifecycle_assert_dispatch(
            'h3_parent', 'prepared_after_dml', kernels_before
        );
        EXECUTE 'ALTER TABLE _lifecycle_h3 ADD COLUMN lifecycle_tag int4 DEFAULT 0';
        IF has_gpu THEN
            PERFORM pg_temp.lifecycle_assert_invalidated(
                '_lifecycle_h3', generation_before, false
            );
            PERFORM pg_temp.lifecycle_refresh('_lifecycle_h3', 500000);
        END IF;

        PERFORM set_config('pg_accel.enabled', 'off', false);
        EXECUTE $native$
            CREATE TEMP TABLE _lifecycle_h3_native AS
            SELECT h3_cell_to_parent(cell, 0) AS parent, count(*) AS rows
            FROM _lifecycle_h3 GROUP BY 1
        $native$;
        PERFORM set_config('pg_accel.enabled', 'on', false);
        PERFORM pg_accel_reset_stats();
        SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;
        EXECUTE 'CREATE TEMP TABLE _lifecycle_h3_accel AS EXECUTE _lifecycle_h3_q';
        PERFORM pg_temp.lifecycle_refresh_if_gpu('_lifecycle_h3', 500000);
        PERFORM pg_temp.lifecycle_explain(
            'h3_parent', 'prepared_after_ddl', '_lifecycle_h3_q'
        );
        EXECUTE $compare$
            SELECT EXISTS (
                (SELECT parent::text, rows FROM _lifecycle_h3_native
                 EXCEPT ALL
                 SELECT parent::text, rows FROM _lifecycle_h3_accel)
                UNION ALL
                (SELECT parent::text, rows FROM _lifecycle_h3_accel
                 EXCEPT ALL
                 SELECT parent::text, rows FROM _lifecycle_h3_native)
            )
        $compare$ INTO STRICT differs;
        IF differs THEN
            RAISE EXCEPTION '95 H3 parent lifecycle FAILED: native results differ';
        END IF;
        PERFORM pg_temp.lifecycle_assert_dispatch(
            'h3_parent', 'prepared_after_ddl', kernels_before, false
        );
    ELSIF to_regtype('h3index') IS NOT NULL THEN
        RAISE EXCEPTION '95 H3 lifecycle FAILED: h3index exists without h3 extension';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:95_selected_lifecycle_contract.assert_004'

-- Resident spatial COUNT(*) with NULL geometries, generic-plan reuse, DML
-- invalidation, and an ADD COLUMN catalog invalidation.
DROP TABLE IF EXISTS _lifecycle_spatial;
CREATE UNLOGGED TABLE _lifecycle_spatial (
    id int8 PRIMARY KEY,
    geom geometry(Point, 4326)
);
INSERT INTO _lifecycle_spatial
SELECT i,
       CASE WHEN i % 97 = 0 THEN NULL
            ELSE ST_SetSRID(
                ST_MakePoint(
                    (i % 1000)::float8 / 50.0,
                    ((i / 1000) % 1000)::float8 / 50.0
                ), 4326
            )::geometry(Point, 4326)
       END
FROM generate_series(1, 1000000) rows(i);
ANALYZE _lifecycle_spatial;

PREPARE _lifecycle_spatial_q AS
SELECT count(*) AS rows
FROM _lifecycle_spatial
WHERE ST_Intersects(
    geom,
    ST_Segmentize(ST_MakeEnvelope(5, 5, 15, 15, 4326), 0.0390625)
);

DO $$
DECLARE
    has_gpu boolean;
    generation_before bigint;
    kernels_before bigint;
BEGIN
    SELECT env.has_gpu INTO STRICT has_gpu FROM _lifecycle_environment env;
    IF has_gpu THEN
        PERFORM pg_accel_pin('_lifecycle_spatial', ARRAY['geom']);
        generation_before := pg_temp.lifecycle_generation('_lifecycle_spatial');
    END IF;
    UPDATE _lifecycle_spatial
    SET geom = CASE WHEN id % 11 = 0 THEN NULL
                    ELSE ST_SetSRID(ST_MakePoint(10, 10), 4326)::geometry(Point, 4326)
               END
    WHERE id <= 4096;
    ANALYZE _lifecycle_spatial;
    IF has_gpu THEN
        PERFORM pg_temp.lifecycle_assert_invalidated(
            '_lifecycle_spatial', generation_before
        );
        PERFORM pg_temp.lifecycle_refresh('_lifecycle_spatial', 1000000);
        generation_before := pg_temp.lifecycle_generation('_lifecycle_spatial');
    END IF;
    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;
    PERFORM pg_temp.lifecycle_explain(
        'spatial_aggregate', 'prepared_after_dml', '_lifecycle_spatial_q'
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'spatial_aggregate', 'prepared_after_dml', kernels_before
    );
    ALTER TABLE _lifecycle_spatial ADD COLUMN lifecycle_tag int4 DEFAULT 0;
    IF has_gpu THEN
        PERFORM pg_temp.lifecycle_assert_invalidated(
            '_lifecycle_spatial', generation_before, false
        );
        PERFORM pg_temp.lifecycle_refresh('_lifecycle_spatial', 1000000);
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _lifecycle_spatial_native AS
SELECT count(*) AS rows
FROM _lifecycle_spatial
WHERE ST_Intersects(
    geom,
    ST_Segmentize(ST_MakeEnvelope(5, 5, 15, 15, 4326), 0.0390625)
);
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _lifecycle_spatial_before AS SELECT pg_accel_kernel_executions() kernels;
CREATE TEMP TABLE _lifecycle_spatial_accel AS EXECUTE _lifecycle_spatial_q;
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_spatial', 1000000);
SELECT pg_temp.lifecycle_explain('spatial_aggregate', 'prepared_after_ddl', '_lifecycle_spatial_q');

DO $$
DECLARE kernels_before bigint;
BEGIN
    IF (SELECT rows FROM _lifecycle_spatial_native)
       IS DISTINCT FROM (SELECT rows FROM _lifecycle_spatial_accel) THEN
        RAISE EXCEPTION '95 spatial lifecycle FAILED: native results differ';
    END IF;
    SELECT kernels INTO STRICT kernels_before FROM _lifecycle_spatial_before;
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'spatial_aggregate', 'prepared_after_ddl', kernels_before, false
    );
END $$;
\echo 'PGACCEL_ASSERT_OK:95_selected_lifecycle_contract.assert_005'

-- Exact raster ST_Reclass.  DML changes source values without changing the
-- selected pixel-count band; ADD COLUMN must invalidate and safely rebuild the
-- pinned raster column used by the prepared statement.
DROP TABLE IF EXISTS _lifecycle_raster;
CREATE UNLOGGED TABLE _lifecycle_raster (id int4 PRIMARY KEY, rast raster);
INSERT INTO _lifecycle_raster
SELECT g,
       CASE WHEN g % 97 = 0 THEN NULL
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
FROM generate_series(1, 10000) rows(g);
ANALYZE _lifecycle_raster;

PREPARE _lifecycle_raster_q AS
SELECT ST_Reclass(rast, '0:1,7:2,255:4', '8BUI') AS rast
FROM _lifecycle_raster;

DO $$
DECLARE
    has_gpu boolean;
    generation_before bigint;
    kernels_before bigint;
BEGIN
    SELECT env.has_gpu INTO STRICT has_gpu FROM _lifecycle_environment env;
    IF has_gpu THEN
        PERFORM pg_accel_pin('_lifecycle_raster', ARRAY['rast']);
        generation_before := pg_temp.lifecycle_generation('_lifecycle_raster');
    END IF;
    UPDATE _lifecycle_raster
    SET rast = CASE WHEN id % 97 = 0 THEN NULL
                    ELSE ST_AddBand(
                        ST_MakeEmptyRaster(32, 32, 0, 0, 1, -1, 0, 0, 4326),
                        '8BUI'::text,
                        CASE WHEN id % 2 = 0 THEN 7 ELSE 9 END,
                        255
                    )
               END
    WHERE id <= 1024;
    ANALYZE _lifecycle_raster;
    IF has_gpu THEN
        PERFORM pg_temp.lifecycle_assert_invalidated(
            '_lifecycle_raster', generation_before
        );
        PERFORM pg_temp.lifecycle_refresh('_lifecycle_raster', 10000);
        generation_before := pg_temp.lifecycle_generation('_lifecycle_raster');
    END IF;
    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;
    PERFORM pg_temp.lifecycle_explain(
        'raster_reclass', 'prepared_after_dml', '_lifecycle_raster_q'
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'raster_reclass', 'prepared_after_dml', kernels_before
    );
    ALTER TABLE _lifecycle_raster ADD COLUMN lifecycle_tag int4 DEFAULT 0;
    IF has_gpu THEN
        PERFORM pg_temp.lifecycle_assert_invalidated(
            '_lifecycle_raster', generation_before, false
        );
        PERFORM pg_temp.lifecycle_refresh('_lifecycle_raster', 10000);
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _lifecycle_raster_native AS
SELECT ST_Reclass(rast, '0:1,7:2,255:4', '8BUI') AS rast
FROM _lifecycle_raster;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _lifecycle_raster_before AS SELECT pg_accel_kernel_executions() kernels;
CREATE TEMP TABLE _lifecycle_raster_accel AS EXECUTE _lifecycle_raster_q;
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_raster', 10000);
SELECT pg_temp.lifecycle_explain('raster_reclass', 'prepared_after_ddl', '_lifecycle_raster_q');

DO $$
DECLARE kernels_before bigint;
BEGIN
    IF EXISTS (
        (SELECT encode(ST_AsBinary(rast), 'hex') FROM _lifecycle_raster_native
         EXCEPT ALL
         SELECT encode(ST_AsBinary(rast), 'hex') FROM _lifecycle_raster_accel)
        UNION ALL
        (SELECT encode(ST_AsBinary(rast), 'hex') FROM _lifecycle_raster_accel
         EXCEPT ALL
         SELECT encode(ST_AsBinary(rast), 'hex') FROM _lifecycle_raster_native)
    ) THEN
        RAISE EXCEPTION '95 raster lifecycle FAILED: native WKB differs';
    END IF;
    IF (SELECT count(*) FROM _lifecycle_raster_accel) <> 10000
       OR (SELECT count(*) FROM _lifecycle_raster_accel WHERE rast IS NULL)
          <> (SELECT count(*) FROM _lifecycle_raster_native WHERE rast IS NULL) THEN
        RAISE EXCEPTION '95 raster lifecycle FAILED: row/NULL semantics differ';
    END IF;
    SELECT kernels INTO STRICT kernels_before FROM _lifecycle_raster_before;
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'raster_reclass', 'prepared_after_ddl', kernels_before, false
    );
END $$;
\echo 'PGACCEL_ASSERT_OK:95_selected_lifecycle_contract.assert_006'

-- Nullable boolean COUNT uses a distinct nullable boolean fact key and
-- measure. The all-NULL measure case remains an active SQL group whose COUNT
-- is zero; prepared execution must rebuild after DML and safe DDL.
DROP TABLE IF EXISTS _lifecycle_bool_count;
CREATE UNLOGGED TABLE _lifecycle_bool_count (
    id int4 PRIMARY KEY,
    bool_key boolean,
    observed boolean
);
INSERT INTO _lifecycle_bool_count
SELECT i,
       CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL ELSE i % 3 = 0 END
FROM generate_series(1, 995904) AS rows(i);
ANALYZE _lifecycle_bool_count;

PREPARE _lifecycle_bool_count_q AS
SELECT bool_key, count(observed) AS observed_rows
FROM _lifecycle_bool_count GROUP BY bool_key;

DO $$
DECLARE
    has_gpu boolean;
    generation_before bigint;
    kernels_before bigint;
BEGIN
    SELECT env.has_gpu INTO STRICT has_gpu FROM _lifecycle_environment AS env;
    IF has_gpu THEN
        PERFORM pg_accel_pin(
            '_lifecycle_bool_count'::regclass,
            ARRAY['bool_key', 'observed']
        );
        generation_before := pg_temp.lifecycle_generation('_lifecycle_bool_count');
    END IF;

    INSERT INTO _lifecycle_bool_count
    SELECT i,
           CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END,
           CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL ELSE i % 3 = 0 END
    FROM generate_series(995905, 1000000) AS rows(i);
    ANALYZE _lifecycle_bool_count;
    IF has_gpu THEN
        PERFORM pg_temp.lifecycle_assert_invalidated(
            '_lifecycle_bool_count', generation_before
        );
        PERFORM pg_temp.lifecycle_refresh('_lifecycle_bool_count', 1000000);
        generation_before := pg_temp.lifecycle_generation('_lifecycle_bool_count');
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;
    PERFORM pg_temp.lifecycle_explain(
        'grouped_bool_count', 'prepared_after_dml', '_lifecycle_bool_count_q'
    );
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'grouped_bool_count', 'prepared_after_dml', kernels_before
    );

    ALTER TABLE _lifecycle_bool_count ADD COLUMN lifecycle_tag int4 DEFAULT 0;
    IF has_gpu THEN
        PERFORM pg_temp.lifecycle_assert_invalidated(
            '_lifecycle_bool_count', generation_before, false
        );
        PERFORM pg_temp.lifecycle_refresh('_lifecycle_bool_count', 1000000);
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _lifecycle_bool_count_native AS EXECUTE _lifecycle_bool_count_q;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _lifecycle_bool_count_before AS
SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _lifecycle_bool_count_accel AS EXECUTE _lifecycle_bool_count_q;
SELECT pg_temp.lifecycle_refresh_if_gpu('_lifecycle_bool_count', 1000000);
SELECT pg_temp.lifecycle_explain(
    'grouped_bool_count', 'prepared_after_ddl', '_lifecycle_bool_count_q'
);

DO $$
DECLARE
    has_gpu boolean;
    kernels_before bigint;
BEGIN
    SELECT env.has_gpu INTO STRICT has_gpu FROM _lifecycle_environment AS env;
    IF EXISTS (
        (SELECT * FROM _lifecycle_bool_count_native
         EXCEPT ALL SELECT * FROM _lifecycle_bool_count_accel)
        UNION ALL
        (SELECT * FROM _lifecycle_bool_count_accel
         EXCEPT ALL SELECT * FROM _lifecycle_bool_count_native)
    ) THEN
        RAISE EXCEPTION '95 grouped bool COUNT lifecycle FAILED: native results differ';
    END IF;
    SELECT kernels INTO STRICT kernels_before FROM _lifecycle_bool_count_before;
    PERFORM pg_temp.lifecycle_assert_dispatch(
        'grouped_bool_count', 'prepared_after_ddl', kernels_before, false
    );
    IF has_gpu AND NOT EXISTS (
        SELECT 1 FROM _lifecycle_plan
        WHERE family = 'grouped_bool_count'
          AND stage IN ('prepared_after_dml', 'prepared_after_ddl')
          AND line LIKE '%GPU Physical Kernel Mode: parallel_dense_count%'
    ) THEN
        RAISE EXCEPTION
            '95 grouped bool COUNT lifecycle FAILED: physical fast path not reported';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:95_selected_lifecycle_contract.assert_007'

DEALLOCATE _lifecycle_grouped_q;
DEALLOCATE _lifecycle_predicate_q;
DEALLOCATE _lifecycle_range_q;
DEALLOCATE _lifecycle_aggregate_filter_q;
DEALLOCATE _lifecycle_int2_count_q;
DEALLOCATE _lifecycle_count4_q;
DEALLOCATE _lifecycle_star4_q;
DEALLOCATE _lifecycle_star8_q;
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM pg_prepared_statements WHERE name = '_lifecycle_h3_q') THEN
        EXECUTE 'DEALLOCATE _lifecycle_h3_q';
    END IF;
END $$;
DEALLOCATE _lifecycle_spatial_q;
DEALLOCATE _lifecycle_raster_q;
DEALLOCATE _lifecycle_bool_count_q;
RESET plan_cache_mode;

DROP TABLE IF EXISTS _lifecycle_h3;
DROP TABLE _lifecycle_raster;
DROP TABLE _lifecycle_spatial;
DROP TABLE _lifecycle_fact8;
DROP TABLE _lifecycle_dim8a;
DROP TABLE _lifecycle_dim8b;
DROP TABLE _lifecycle_fact4;
DROP TABLE _lifecycle_dim4a;
DROP TABLE _lifecycle_dim4b;
DROP TABLE _lifecycle_dim4c;
DROP TABLE _lifecycle_dim4d;
DROP TABLE _lifecycle_grouped;
DROP TABLE _lifecycle_bool_count;

\echo 'PGACCEL_FILE_OK:95_selected_lifecycle_contract'
