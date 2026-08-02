-- 91_selected_family_contracts.sql: positive contracts for the remaining
-- production-selected resident aggregate families.

\echo '=== 91_selected_family_contracts ==='

SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;

CREATE TEMP TABLE _family_environment AS
SELECT gpu_available AS has_gpu,
       EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'h3') AS has_h3
FROM pg_accel_device_info();

CREATE TEMP TABLE _family_plan (family text NOT NULL, line text NOT NULL);

-- Predicate aggregate: exact integer expression plus a supported row filter.
CREATE TEMP TABLE _family_predicate AS
SELECT (i % 256)::int4 AS product_id,
       (1 + (i % 1000))::int4 AS price,
       (1 + ((i / 256) % 10))::int4 AS quantity,
       ((i / 256) % 10) = 0 AS active
FROM generate_series(1, 500000) AS rows(i);
ANALYZE _family_predicate;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _family_predicate_native AS
SELECT product_id, sum(price * quantity) AS total, count(*) AS rows
FROM _family_predicate
WHERE active
GROUP BY product_id;

DO $$
DECLARE
    has_gpu boolean;
    kernels_before bigint;
    kernels_after bigint;
    stock_after bigint;
    plan_row record;
BEGIN
    SELECT env.has_gpu INTO STRICT has_gpu FROM _family_environment AS env;
    IF has_gpu THEN
        PERFORM pg_accel_pin(
            '_family_predicate'::regclass,
            ARRAY['product_id', 'price', 'quantity', 'active']
        );
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    EXECUTE $query$
        CREATE TEMP TABLE _family_predicate_accel AS
        SELECT product_id, sum(price * quantity) AS total, count(*) AS rows
        FROM _family_predicate
        WHERE active
        GROUP BY product_id
    $query$;
    FOR plan_row IN EXECUTE $query$
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT product_id, sum(price * quantity), count(*)
        FROM _family_predicate
        WHERE active
        GROUP BY product_id
    $query$
    LOOP
        INSERT INTO _family_plan VALUES ('predicate', plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    IF EXISTS (
        SELECT 1
        FROM _family_predicate_native AS native
        FULL OUTER JOIN _family_predicate_accel AS accel USING (product_id)
        WHERE native.total IS DISTINCT FROM accel.total
           OR native.rows IS DISTINCT FROM accel.rows
    ) THEN
        RAISE EXCEPTION '91 predicate contract FAILED: result differs from native';
    END IF;
    IF stock_after <> 0 THEN
        RAISE EXCEPTION '91 predicate contract FAILED: stock fallback count is %', stock_after;
    END IF;
    IF has_gpu THEN
        IF NOT EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'predicate' AND line LIKE '%Custom Scan (GpuAccelAgg)%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'predicate' AND line LIKE '%GPU Resident Operator Class: resident_groupagg%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'predicate' AND line LIKE '%GPU Kernel Dispatched: true%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'predicate'
              AND line ~ 'Custom Scan \(GpuAccelAgg\).*actual rows=[1-9]'
        ) THEN
            RAISE EXCEPTION '91 predicate contract FAILED: selected-path proof is incomplete';
        END IF;
        IF kernels_after <= kernels_before THEN
            RAISE EXCEPTION '91 predicate contract FAILED: kernel counter did not increase';
        END IF;
    ELSE
        IF EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'predicate' AND line LIKE '%Custom Scan (%GpuAccel%'
        ) OR kernels_after <> kernels_before THEN
            RAISE EXCEPTION '91 predicate contract FAILED: unavailable host dispatched';
        END IF;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:91_selected_family_contracts.assert_001'

-- Shared exact-integer fact/dimension fixture. The count query consumes one
-- dimension; the star query reaches the released 4-dimension descriptor limit.
CREATE TEMP TABLE _family_dim1 (id int4 PRIMARY KEY, label int4 NOT NULL);
CREATE TEMP TABLE _family_dim2 (id int4 PRIMARY KEY, label int4 NOT NULL);
CREATE TEMP TABLE _family_dim3 (id int4 PRIMARY KEY, label int4 NOT NULL);
CREATE TEMP TABLE _family_dim4 (id int4 PRIMARY KEY, label int4 NOT NULL);
CREATE TEMP TABLE _family_dim5 (id int4 PRIMARY KEY, label int4 NOT NULL);
INSERT INTO _family_dim1 SELECT i, i % 4 FROM generate_series(1, 100) AS rows(i);
INSERT INTO _family_dim2 SELECT i, i % 5 FROM generate_series(1, 100) AS rows(i);
INSERT INTO _family_dim3 SELECT i, i % 3 FROM generate_series(1, 100) AS rows(i);
INSERT INTO _family_dim4 SELECT i, i % 7 FROM generate_series(1, 100) AS rows(i);
INSERT INTO _family_dim5 SELECT i, i % 2 FROM generate_series(1, 100) AS rows(i);
CREATE TEMP TABLE _family_fact AS
SELECT i::int4 AS id,
       (((i - 1) % 100) + 1)::int4 AS k1,
       (((i - 1) % 100) + 1)::int4 AS k2,
       (((i - 1) % 100) + 1)::int4 AS k3,
       (((i - 1) % 100) + 1)::int4 AS k4,
       (((i - 1) % 100) + 1)::int4 AS k5,
       (1 + (i % 1000))::int4 AS amount
FROM generate_series(1, 500000) AS rows(i);
ANALYZE _family_fact;
ANALYZE _family_dim1;
ANALYZE _family_dim2;
ANALYZE _family_dim3;
ANALYZE _family_dim4;
ANALYZE _family_dim5;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _family_join_native AS
SELECT count(*) AS rows
FROM _family_fact AS fact
JOIN _family_dim1 AS dim1 ON fact.k1 = dim1.id;

DO $$
DECLARE
    has_gpu boolean;
    kernels_before bigint;
    kernels_after bigint;
    stock_after bigint;
    plan_row record;
BEGIN
    SELECT env.has_gpu INTO STRICT has_gpu FROM _family_environment AS env;
    IF has_gpu THEN
        PERFORM pg_accel_pin(
            '_family_fact'::regclass,
            ARRAY['k1', 'k2', 'k3', 'k4', 'k5', 'amount']
        );
        PERFORM pg_accel_pin('_family_dim1'::regclass, ARRAY['id', 'label']);
        PERFORM pg_accel_pin('_family_dim2'::regclass, ARRAY['id', 'label']);
        PERFORM pg_accel_pin('_family_dim3'::regclass, ARRAY['id', 'label']);
        PERFORM pg_accel_pin('_family_dim4'::regclass, ARRAY['id', 'label']);
        PERFORM pg_accel_pin('_family_dim5'::regclass, ARRAY['id', 'label']);
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;
    EXECUTE $query$
        CREATE TEMP TABLE _family_join_accel AS
        SELECT count(*) AS rows
        FROM _family_fact AS fact
        JOIN _family_dim1 AS dim1 ON fact.k1 = dim1.id
    $query$;
    FOR plan_row IN EXECUTE $query$
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT count(*)
        FROM _family_fact AS fact
        JOIN _family_dim1 AS dim1 ON fact.k1 = dim1.id
    $query$
    LOOP
        INSERT INTO _family_plan VALUES ('count_join', plan_row."QUERY PLAN");
    END LOOP;
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();

    IF (SELECT rows FROM _family_join_native)
       IS DISTINCT FROM (SELECT rows FROM _family_join_accel) THEN
        RAISE EXCEPTION '91 count-only join contract FAILED: result differs from native';
    END IF;
    IF stock_after <> 0 THEN
        RAISE EXCEPTION '91 count-only join contract FAILED: stock fallback count is %', stock_after;
    END IF;
    IF has_gpu THEN
        IF NOT EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'count_join' AND line LIKE '%Custom Scan (GpuAccelAgg)%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'count_join' AND line LIKE '%GPU Resident Operator Class: resident_groupagg%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'count_join' AND line LIKE '%GPU Kernel Dispatched: true%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'count_join'
              AND line ~ 'Custom Scan \(GpuAccelAgg\).*actual rows=[1-9]'
        ) THEN
            RAISE EXCEPTION '91 count-only join contract FAILED: selected-path proof is incomplete';
        END IF;
        IF kernels_after <= kernels_before THEN
            RAISE EXCEPTION '91 count-only join contract FAILED: kernel counter did not increase';
        END IF;
    ELSE
        IF EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'count_join' AND line LIKE '%Custom Scan (%GpuAccel%'
        ) OR kernels_after <> kernels_before THEN
            RAISE EXCEPTION '91 count-only join contract FAILED: unavailable host dispatched';
        END IF;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:91_selected_family_contracts.assert_002'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _family_star_native AS
SELECT dim1.label AS key1, dim2.label AS key2, dim3.label AS key3,
       sum(fact.amount) AS total, count(*) AS rows,
       min(fact.amount) AS minimum, max(fact.amount) AS maximum
FROM _family_fact AS fact
JOIN _family_dim1 AS dim1 ON fact.k1 = dim1.id
JOIN _family_dim2 AS dim2 ON fact.k2 = dim2.id
JOIN _family_dim3 AS dim3 ON fact.k3 = dim3.id
JOIN _family_dim4 AS dim4 ON fact.k4 = dim4.id
GROUP BY dim1.label, dim2.label, dim3.label;

DO $$
DECLARE
    has_gpu boolean;
    kernels_before bigint;
    kernels_after bigint;
    stock_after bigint;
    plan_row record;
BEGIN
    SELECT env.has_gpu INTO STRICT has_gpu FROM _family_environment AS env;
    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;
    EXECUTE $query$
        CREATE TEMP TABLE _family_star_accel AS
        SELECT dim1.label AS key1, dim2.label AS key2, dim3.label AS key3,
               sum(fact.amount) AS total, count(*) AS rows,
               min(fact.amount) AS minimum, max(fact.amount) AS maximum
        FROM _family_fact AS fact
        JOIN _family_dim1 AS dim1 ON fact.k1 = dim1.id
        JOIN _family_dim2 AS dim2 ON fact.k2 = dim2.id
        JOIN _family_dim3 AS dim3 ON fact.k3 = dim3.id
        JOIN _family_dim4 AS dim4 ON fact.k4 = dim4.id
        GROUP BY dim1.label, dim2.label, dim3.label
    $query$;
    FOR plan_row IN EXECUTE $query$
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT dim1.label, dim2.label, dim3.label,
               sum(fact.amount), count(*), min(fact.amount), max(fact.amount)
        FROM _family_fact AS fact
        JOIN _family_dim1 AS dim1 ON fact.k1 = dim1.id
        JOIN _family_dim2 AS dim2 ON fact.k2 = dim2.id
        JOIN _family_dim3 AS dim3 ON fact.k3 = dim3.id
        JOIN _family_dim4 AS dim4 ON fact.k4 = dim4.id
        GROUP BY dim1.label, dim2.label, dim3.label
    $query$
    LOOP
        INSERT INTO _family_plan VALUES ('star', plan_row."QUERY PLAN");
    END LOOP;
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();

    IF EXISTS (
        SELECT 1
        FROM _family_star_native AS native
        FULL OUTER JOIN _family_star_accel AS accel USING (key1, key2, key3)
        WHERE native.total IS DISTINCT FROM accel.total
           OR native.rows IS DISTINCT FROM accel.rows
           OR native.minimum IS DISTINCT FROM accel.minimum
           OR native.maximum IS DISTINCT FROM accel.maximum
    ) THEN
        RAISE EXCEPTION '91 star contract FAILED: result differs from native';
    END IF;
    IF stock_after <> 0 THEN
        RAISE EXCEPTION '91 star contract FAILED: stock fallback count is %', stock_after;
    END IF;
    IF has_gpu THEN
        IF NOT EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'star' AND line LIKE '%Custom Scan (GpuAccelAgg)%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'star' AND line LIKE '%GPU Resident Operator Class: resident_groupagg%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'star' AND line LIKE '%GPU Kernel Dispatched: true%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'star'
              AND line ~ 'Custom Scan \(GpuAccelAgg\).*actual rows=[1-9]'
        ) THEN
            RAISE EXCEPTION '91 star contract FAILED: selected-path proof is incomplete';
        END IF;
        IF kernels_after <= kernels_before THEN
            RAISE EXCEPTION '91 star contract FAILED: kernel counter did not increase';
        END IF;
    ELSE
        IF EXISTS (
            SELECT 1 FROM _family_plan
            WHERE family = 'star' AND line LIKE '%Custom Scan (%GpuAccel%'
        ) OR kernels_after <> kernels_before THEN
            RAISE EXCEPTION '91 star contract FAILED: unavailable host dispatched';
        END IF;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:91_selected_family_contracts.assert_003'

-- The immediately adjacent five-dimension shape exceeds the released ABI.
DO $$
DECLARE
    has_gpu boolean;
    kernels_before bigint;
    kernels_after bigint;
    stock_after bigint;
    exact_declines bigint;
    plan_row record;
BEGIN
    SELECT env.has_gpu INTO STRICT has_gpu FROM _family_environment AS env;
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;
    FOR plan_row IN EXECUTE $query$
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT dim1.label, count(*)
        FROM _family_fact AS fact
        JOIN _family_dim1 AS dim1 ON fact.k1 = dim1.id
        JOIN _family_dim2 AS dim2 ON fact.k2 = dim2.id
        JOIN _family_dim3 AS dim3 ON fact.k3 = dim3.id
        JOIN _family_dim4 AS dim4 ON fact.k4 = dim4.id
        JOIN _family_dim5 AS dim5 ON fact.k5 = dim5.id
        GROUP BY dim1.label
    $query$
    LOOP
        INSERT INTO _family_plan VALUES ('five_dims', plan_row."QUERY PLAN");
    END LOOP;
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    SELECT pg_accel_planner_rejection_count('shape_too_many_relations')
    INTO STRICT exact_declines;

    IF EXISTS (
        SELECT 1 FROM _family_plan
        WHERE family = 'five_dims' AND line LIKE '%Custom Scan (%GpuAccel%'
    ) THEN
        RAISE EXCEPTION '91 five-dimension decline FAILED: selected pg_accel plan';
    END IF;
    IF kernels_after <> kernels_before OR stock_after <> 0 THEN
        RAISE EXCEPTION '91 five-dimension decline FAILED: dispatch or fallback occurred';
    END IF;
    IF has_gpu AND exact_declines <= 0 THEN
        RAISE EXCEPTION
            '91 five-dimension decline FAILED: shape_too_many_relations was not recorded';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:91_selected_family_contracts.assert_004'

-- H3 is optional in the extension matrix. When installed, prove the one
-- production-selected H3 shape; absence is a silent, asserted inventory case.
DO $$
DECLARE
    has_gpu boolean;
    has_h3 boolean;
    kernels_before bigint;
    kernels_after bigint;
    stock_after bigint;
    plan_row record;
BEGIN
    SELECT env.has_gpu, env.has_h3 INTO STRICT has_gpu, has_h3
    FROM _family_environment AS env;
    IF has_h3 THEN
        EXECUTE $setup$
            CREATE TEMP TABLE _family_h3 (cell h3index);
            WITH seeds AS (
                SELECT array_agg(cell) AS cells
                FROM h3_grid_disk(
                    h3_cell_to_parent('8928308280fffff'::h3index, 2), 2
                ) AS disk(cell)
            )
            INSERT INTO _family_h3
            SELECT CASE WHEN i % 97 = 0
                        THEN NULL::h3index
                        ELSE cells[1 + ((i - 1) % cardinality(cells))]
                   END
            FROM generate_series(1, 500000) AS rows(i) CROSS JOIN seeds;
            ANALYZE _family_h3
        $setup$;
        PERFORM set_config('pg_accel.enabled', 'off', false);
        EXECUTE $query$
            CREATE TEMP TABLE _family_h3_native AS
            SELECT h3_cell_to_parent(cell, 0) AS parent, count(*) AS rows
            FROM _family_h3
            GROUP BY 1
        $query$;
        IF has_gpu THEN
            PERFORM pg_accel_pin('_family_h3'::regclass, ARRAY['cell']);
        END IF;
        PERFORM set_config('pg_accel.enabled', 'on', false);
        PERFORM pg_accel_reset_stats();
        SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;
        EXECUTE $query$
            CREATE TEMP TABLE _family_h3_accel AS
            SELECT h3_cell_to_parent(cell, 0) AS parent, count(*) AS rows
            FROM _family_h3
            GROUP BY 1
        $query$;
        FOR plan_row IN EXECUTE $query$
            EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
            SELECT h3_cell_to_parent(cell, 0), count(*)
            FROM _family_h3
            GROUP BY 1
        $query$
        LOOP
            INSERT INTO _family_plan VALUES ('h3_parent', plan_row."QUERY PLAN");
        END LOOP;
        SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
        SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();

        IF EXISTS (
            SELECT 1
            FROM _family_h3_native AS native
            FULL OUTER JOIN _family_h3_accel AS accel
              ON coalesce(native.parent::text, '<NULL>')
               = coalesce(accel.parent::text, '<NULL>')
            WHERE native.rows IS DISTINCT FROM accel.rows
        ) THEN
            RAISE EXCEPTION '91 H3 parent contract FAILED: result differs from native';
        END IF;
        IF stock_after <> 0 THEN
            RAISE EXCEPTION '91 H3 parent contract FAILED: stock fallback count is %', stock_after;
        END IF;
        IF has_gpu THEN
            IF NOT EXISTS (
                SELECT 1 FROM _family_plan
                WHERE family = 'h3_parent' AND line LIKE '%Custom Scan (GpuAccelAgg)%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _family_plan
                WHERE family = 'h3_parent' AND line LIKE '%GPU Resident Operator Class: resident_groupagg%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _family_plan
                WHERE family = 'h3_parent' AND line LIKE '%GPU Kernel Dispatched: true%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _family_plan
                WHERE family = 'h3_parent'
                  AND line ~ 'Custom Scan \(GpuAccelAgg\).*actual rows=[1-9]'
            ) THEN
                RAISE EXCEPTION '91 H3 parent contract FAILED: selected-path proof is incomplete';
            END IF;
            IF kernels_after <= kernels_before THEN
                RAISE EXCEPTION '91 H3 parent contract FAILED: kernel counter did not increase';
            END IF;
        ELSE
            IF EXISTS (
                SELECT 1 FROM _family_plan
                WHERE family = 'h3_parent' AND line LIKE '%Custom Scan (%GpuAccel%'
            ) OR kernels_after <> kernels_before THEN
                RAISE EXCEPTION '91 H3 parent contract FAILED: unavailable host dispatched';
            END IF;
        END IF;
    ELSE
        IF to_regtype('h3index') IS NOT NULL THEN
            RAISE EXCEPTION '91 H3 parent contract FAILED: h3index exists without h3 extension';
        END IF;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:91_selected_family_contracts.assert_005'

DROP TABLE _family_plan;
DROP TABLE _family_environment;
DROP TABLE _family_predicate_native;
DROP TABLE _family_predicate_accel;
DROP TABLE _family_predicate;
DROP TABLE _family_join_native;
DROP TABLE _family_join_accel;
DROP TABLE _family_star_native;
DROP TABLE _family_star_accel;
DROP TABLE _family_fact;
DROP TABLE _family_dim1;
DROP TABLE _family_dim2;
DROP TABLE _family_dim3;
DROP TABLE _family_dim4;
DROP TABLE _family_dim5;

\echo 'PGACCEL_FILE_OK:91_selected_family_contracts'
