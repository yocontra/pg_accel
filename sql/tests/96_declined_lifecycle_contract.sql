-- 96_declined_lifecycle_contract.sql: exact lifecycle contract for every
-- released structural native-decline family. Each case prepares the query
-- before DML and DDL, compares the replanned result with PostgreSQL, proves the
-- specific reason counter, and requires zero Custom Scan, dispatch, or stock
-- fallback.

\echo '=== 96_declined_lifecycle_contract ==='

SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
SET plan_cache_mode = force_generic_plan;

CREATE TEMP TABLE _decline_environment AS
SELECT gpu_available AS has_gpu,
       EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'h3') AS has_h3,
       EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'postgis_raster') AS has_raster
FROM pg_accel_device_info();

CREATE TEMP TABLE _decline_plan (
    family text NOT NULL,
    line text NOT NULL
);

CREATE FUNCTION pg_temp.decline_explain(selected_family text, statement_name text)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    plan_row record;
BEGIN
    DELETE FROM _decline_plan WHERE family = selected_family;
    FOR plan_row IN EXECUTE format(
        'EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF) EXECUTE %I',
        statement_name
    ) LOOP
        INSERT INTO _decline_plan VALUES (selected_family, plan_row."QUERY PLAN");
    END LOOP;
END $$;

CREATE FUNCTION pg_temp.decline_contract_ok(
    selected_family text,
    expected_reason text,
    native_result regclass,
    enabled_result regclass,
    kernels_before bigint,
    require_order boolean DEFAULT false
)
RETURNS boolean
LANGUAGE plpgsql
AS $$
DECLARE
    differs boolean;
    native_sequence jsonb;
    enabled_sequence jsonb;
    kernels_after bigint;
    stock_after bigint;
    reason_count bigint;
    has_gpu boolean;
    plan_text text;
BEGIN
    EXECUTE format(
        'SELECT EXISTS ((TABLE %s EXCEPT ALL TABLE %s) UNION ALL '
        || '(TABLE %s EXCEPT ALL TABLE %s))',
        native_result, enabled_result, enabled_result, native_result
    ) INTO STRICT differs;
    IF differs THEN
        RAISE EXCEPTION
            '96 % FAILED: enabled result differs from PostgreSQL',
            selected_family;
    END IF;

    IF require_order THEN
        EXECUTE format(
            'SELECT coalesce(jsonb_agg(to_jsonb(rows) ORDER BY ctid), ''[]''::jsonb) '
            || 'FROM %s AS rows',
            native_result
        ) INTO STRICT native_sequence;
        EXECUTE format(
            'SELECT coalesce(jsonb_agg(to_jsonb(rows) ORDER BY ctid), ''[]''::jsonb) '
            || 'FROM %s AS rows',
            enabled_result
        ) INTO STRICT enabled_sequence;
        IF enabled_sequence IS DISTINCT FROM native_sequence THEN
            RAISE EXCEPTION
                '96 % FAILED: ordered result differs from PostgreSQL',
                selected_family;
        END IF;
    END IF;

    SELECT env.has_gpu INTO STRICT has_gpu FROM _decline_environment AS env;
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    SELECT pg_accel_planner_rejection_count(expected_reason) INTO STRICT reason_count;
    SELECT string_agg(line, E'\n' ORDER BY line) INTO plan_text
    FROM _decline_plan WHERE family = selected_family;

    IF plan_text IS NULL THEN
        RAISE EXCEPTION '96 % FAILED: EXPLAIN produced no plan', selected_family;
    END IF;
    IF EXISTS (
        SELECT 1 FROM _decline_plan
        WHERE family = selected_family
          AND line LIKE '%Custom Scan (%GpuAccel%'
    ) THEN
        RAISE EXCEPTION
            '96 % FAILED: structural decline selected pg_accel plan:%',
            selected_family, E'\n' || plan_text;
    END IF;
    IF kernels_after <> kernels_before OR stock_after <> 0 THEN
        RAISE EXCEPTION
            '96 % FAILED: dispatch/fallback changed (kernels % -> %, stock %)',
            selected_family, kernels_before, kernels_after, stock_after;
    END IF;
    IF has_gpu AND reason_count <= 0 THEN
        RAISE EXCEPTION
            '96 % FAILED: exact reason % was not recorded; plan:%',
            selected_family, expected_reason, E'\n' || plan_text;
    END IF;
    RETURN true;
END $$;

-- One shared scalar fixture covers range, unsupported-predicate, aggregate
-- modifier, base scan/projection, sort, top-k, window, and boolean COUNT. The
-- prepared statements are all bound before the fixture is changed.
CREATE TEMP TABLE _decline_scalar (
    id int4 PRIMARY KEY,
    g int4,
    price int4,
    quantity int4,
    score float4,
    sort_key float8,
    flag boolean,
    short_value int2,
    wide_value int8
);
INSERT INTO _decline_scalar
SELECT i,
       CASE WHEN i % 107 = 0 THEN NULL ELSE i % 64 END,
       CASE WHEN i % 101 = 0 THEN NULL ELSE i % 1000 END,
       CASE WHEN i % 103 = 0 THEN NULL ELSE 1 + (i / 64) % 10 END,
       CASE WHEN i % 109 = 0 THEN NULL ELSE (i % 1000)::float4 END,
       CASE WHEN i = 113 THEN NULL ELSE i::float8 END,
       CASE WHEN i % 97 = 0 THEN NULL ELSE i % 2 = 0 END,
       CASE WHEN i % 89 = 0 THEN NULL
            ELSE ((i % 65536) - 32768)::int2 END,
       CASE WHEN i % 83 = 0 THEN NULL
            WHEN i % 2 = 0 THEN '9223372036854775807'::int8
            ELSE '-9223372036854775808'::int8 END
FROM generate_series(1, 200000) AS rows(i);
ANALYZE _decline_scalar;

PREPARE _decline_range_q AS
SELECT g, sum(price * quantity) AS total, count(*) AS rows
FROM _decline_scalar
WHERE price >= 200 AND price <= 800 AND price >= 250
GROUP BY g;
PREPARE _decline_predicate_q AS
SELECT count(*) AS rows
FROM _decline_scalar
WHERE score > 500::float4;
PREPARE _decline_modifier_filter_q AS
SELECT g, sum(price) FILTER (WHERE quantity > 0) AS total
FROM _decline_scalar GROUP BY g;
PREPARE _decline_modifier_distinct_q AS
SELECT g, count(DISTINCT price) AS distinct_prices
FROM _decline_scalar GROUP BY g;
PREPARE _decline_modifier_ordered_q AS
SELECT g, sum(price ORDER BY id) AS total
FROM _decline_scalar GROUP BY g;
PREPARE _decline_base_q AS
SELECT id, price + quantity AS projected
FROM _decline_scalar
WHERE score > 500::float4;
PREPARE _decline_sort_q AS
SELECT id, sort_key FROM _decline_scalar ORDER BY sort_key NULLS FIRST;
PREPARE _decline_topk_q AS
SELECT id, sort_key FROM _decline_scalar ORDER BY sort_key NULLS LAST LIMIT 128;
PREPARE _decline_window_q AS
SELECT id, g, price,
       sum(price) OVER (
           PARTITION BY g ORDER BY id ROWS BETWEEN 3 PRECEDING AND CURRENT ROW
       ) AS running_total
FROM _decline_scalar
ORDER BY id;
PREPARE _decline_bool_q AS
SELECT flag, count(flag) AS observed_rows
FROM _decline_scalar GROUP BY flag;
PREPARE _decline_int2_q AS
SELECT g, count(short_value) AS observed_rows
FROM _decline_scalar GROUP BY g;
PREPARE _decline_int8_q AS
SELECT g, count(wide_value) AS observed_rows
FROM _decline_scalar GROUP BY g;

INSERT INTO _decline_scalar
SELECT i,
       CASE WHEN i % 7 = 0 THEN NULL ELSE i % 64 END,
       CASE WHEN i % 11 = 0 THEN NULL ELSE -(i % 1000) END,
       CASE WHEN i % 13 = 0 THEN NULL ELSE 2 END,
       CASE WHEN i % 17 = 0 THEN NULL ELSE (i % 1000)::float4 END,
       i::float8,
       CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END,
       CASE WHEN i % 23 = 0 THEN NULL
            ELSE ((i % 65536) - 32768)::int2 END,
       CASE WHEN i % 29 = 0 THEN NULL
            WHEN i % 2 = 0 THEN '9223372036854775807'::int8
            ELSE '-9223372036854775808'::int8 END
FROM generate_series(200001, 201024) AS rows(i);
ALTER TABLE _decline_scalar ADD COLUMN lifecycle_tag int4 DEFAULT 0;
ANALYZE _decline_scalar;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_range_native AS
SELECT g, sum(price * quantity) AS total, count(*) AS rows
FROM _decline_scalar
WHERE price >= 200 AND price <= 800 AND price >= 250
GROUP BY g;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_range_before AS
SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_range_enabled AS EXECUTE _decline_range_q;
SELECT pg_temp.decline_explain('and_range', '_decline_range_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'and_range', 'shape_multiple_range_predicates',
        '_decline_range_native', '_decline_range_enabled',
        (SELECT kernels FROM _decline_range_before)
    ) THEN
        RAISE EXCEPTION '96 and-range contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_001'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_predicate_native AS
SELECT count(*) AS rows FROM _decline_scalar WHERE score > 500::float4;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_predicate_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_predicate_enabled AS EXECUTE _decline_predicate_q;
SELECT pg_temp.decline_explain('aggregate_predicate', '_decline_predicate_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'aggregate_predicate', 'shape_unsupported_predicate',
        '_decline_predicate_native', '_decline_predicate_enabled',
        (SELECT kernels FROM _decline_predicate_before)
    ) THEN
        RAISE EXCEPTION '96 aggregate-predicate contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_002'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_modifier_filter_native AS
SELECT g, sum(price) FILTER (WHERE quantity > 0) AS total
FROM _decline_scalar GROUP BY g;
CREATE TEMP TABLE _decline_modifier_distinct_native AS
SELECT g, count(DISTINCT price) AS distinct_prices
FROM _decline_scalar GROUP BY g;
CREATE TEMP TABLE _decline_modifier_ordered_native AS
SELECT g, sum(price ORDER BY id) AS total
FROM _decline_scalar GROUP BY g;

SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_modifier_filter_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_modifier_filter_enabled AS EXECUTE _decline_modifier_filter_q;
SELECT pg_temp.decline_explain('aggregate_modifier_filter', '_decline_modifier_filter_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'aggregate_modifier_filter', 'shape_aggregate_modifier',
        '_decline_modifier_filter_native', '_decline_modifier_filter_enabled',
        (SELECT kernels FROM _decline_modifier_filter_before)
    ) THEN
        RAISE EXCEPTION '96 aggregate FILTER contract returned false';
    END IF;
END $$;

SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_modifier_distinct_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_modifier_distinct_enabled AS EXECUTE _decline_modifier_distinct_q;
SELECT pg_temp.decline_explain('aggregate_modifier_distinct', '_decline_modifier_distinct_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'aggregate_modifier_distinct', 'shape_aggregate_modifier',
        '_decline_modifier_distinct_native', '_decline_modifier_distinct_enabled',
        (SELECT kernels FROM _decline_modifier_distinct_before)
    ) THEN
        RAISE EXCEPTION '96 aggregate DISTINCT contract returned false';
    END IF;
END $$;

SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_modifier_ordered_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_modifier_ordered_enabled AS EXECUTE _decline_modifier_ordered_q;
SELECT pg_temp.decline_explain('aggregate_modifier_ordered', '_decline_modifier_ordered_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'aggregate_modifier_ordered', 'shape_aggregate_modifier',
        '_decline_modifier_ordered_native', '_decline_modifier_ordered_enabled',
        (SELECT kernels FROM _decline_modifier_ordered_before)
    ) THEN
        RAISE EXCEPTION '96 ordered-aggregate contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_003'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_base_native AS
SELECT id, price + quantity AS projected
FROM _decline_scalar WHERE score > 500::float4;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_base_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_base_enabled AS EXECUTE _decline_base_q;
SELECT pg_temp.decline_explain('base_scan_filter_projection', '_decline_base_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'base_scan_filter_projection', 'no_gpu_resident_pipeline',
        '_decline_base_native', '_decline_base_enabled',
        (SELECT kernels FROM _decline_base_before)
    ) THEN
        RAISE EXCEPTION '96 base-path contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_004'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_sort_native AS
SELECT id, sort_key FROM _decline_scalar ORDER BY sort_key NULLS FIRST;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_sort_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_sort_enabled AS EXECUTE _decline_sort_q;
SELECT pg_temp.decline_explain('sort_full_output', '_decline_sort_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'sort_full_output', 'sort_heap_full_output',
        '_decline_sort_native', '_decline_sort_enabled',
        (SELECT kernels FROM _decline_sort_before), true
    ) THEN
        RAISE EXCEPTION '96 full-sort contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_005'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_topk_native AS
SELECT id, sort_key FROM _decline_scalar ORDER BY sort_key NULLS LAST LIMIT 128;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_topk_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_topk_enabled AS EXECUTE _decline_topk_q;
SELECT pg_temp.decline_explain('sort_topk', '_decline_topk_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'sort_topk', 'sort_standalone_topk_no_gpu_kernel',
        '_decline_topk_native', '_decline_topk_enabled',
        (SELECT kernels FROM _decline_topk_before), true
    ) THEN
        RAISE EXCEPTION '96 top-k contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_006'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_window_native AS
SELECT id, g, price,
       sum(price) OVER (
           PARTITION BY g ORDER BY id ROWS BETWEEN 3 PRECEDING AND CURRENT ROW
       ) AS running_total
FROM _decline_scalar
ORDER BY id;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_window_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_window_enabled AS EXECUTE _decline_window_q;
SELECT pg_temp.decline_explain('window', '_decline_window_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'window', 'no_gpu_resident_pipeline',
        '_decline_window_native', '_decline_window_enabled',
        (SELECT kernels FROM _decline_window_before), true
    ) THEN
        RAISE EXCEPTION '96 window contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_007'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_bool_native AS
SELECT flag, count(flag) AS observed_rows FROM _decline_scalar GROUP BY flag;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_bool_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_bool_enabled AS EXECUTE _decline_bool_q;
SELECT pg_temp.decline_explain('grouped_count_bool_adjacent', '_decline_bool_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'grouped_count_bool_adjacent', 'generic_serial_kernel_mode_unqualified',
        '_decline_bool_native', '_decline_bool_enabled',
        (SELECT kernels FROM _decline_bool_before)
    ) THEN
        RAISE EXCEPTION '96 same-column boolean COUNT contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_008'

-- Five dimensions exceed the released descriptor. NULL fact keys prove that
-- DML/DDL replan cannot turn native join NULL semantics into a GPU lane.
CREATE TEMP TABLE _decline_dim1 (id int4 PRIMARY KEY, label int4);
CREATE TEMP TABLE _decline_dim2 (id int4 PRIMARY KEY, label int4);
CREATE TEMP TABLE _decline_dim3 (id int4 PRIMARY KEY, label int4);
CREATE TEMP TABLE _decline_dim4 (id int4 PRIMARY KEY, label int4);
CREATE TEMP TABLE _decline_dim5 (id int4 PRIMARY KEY, label int4);
INSERT INTO _decline_dim1 SELECT i, i % 4 FROM generate_series(1, 100) i;
INSERT INTO _decline_dim2 SELECT i, i % 5 FROM generate_series(1, 100) i;
INSERT INTO _decline_dim3 SELECT i, i % 6 FROM generate_series(1, 100) i;
INSERT INTO _decline_dim4 SELECT i, i % 7 FROM generate_series(1, 100) i;
INSERT INTO _decline_dim5 SELECT i, i % 8 FROM generate_series(1, 100) i;
CREATE TEMP TABLE _decline_fact5 AS
SELECT i::int4 AS id,
       CASE WHEN i % 101 = 0 THEN NULL ELSE 1 + (i - 1) % 100 END::int4 AS k1,
       CASE WHEN i % 103 = 0 THEN NULL ELSE 1 + (i - 1) % 100 END::int4 AS k2,
       (1 + (i - 1) % 100)::int4 AS k3,
       (1 + (i - 1) % 100)::int4 AS k4,
       CASE WHEN i % 107 = 0 THEN NULL ELSE 1 + (i - 1) % 100 END::int4 AS k5
FROM generate_series(1, 100000) AS rows(i);
ANALYZE _decline_fact5;
ANALYZE _decline_dim1; ANALYZE _decline_dim2; ANALYZE _decline_dim3;
ANALYZE _decline_dim4; ANALYZE _decline_dim5;

PREPARE _decline_relation_limit_q AS
SELECT d1.label, count(*) AS rows
FROM _decline_fact5 AS fact
JOIN _decline_dim1 AS d1 ON fact.k1 = d1.id
JOIN _decline_dim2 AS d2 ON fact.k2 = d2.id
JOIN _decline_dim3 AS d3 ON fact.k3 = d3.id
JOIN _decline_dim4 AS d4 ON fact.k4 = d4.id
JOIN _decline_dim5 AS d5 ON fact.k5 = d5.id
GROUP BY d1.label;
INSERT INTO _decline_fact5 VALUES (100001, NULL, 1, 1, 1, 1), (100002, 1, 1, 1, 1, NULL);
ALTER TABLE _decline_fact5 ADD COLUMN lifecycle_tag int4 DEFAULT 0;
ANALYZE _decline_fact5;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_relation_limit_native AS
SELECT d1.label, count(*) AS rows
FROM _decline_fact5 AS fact
JOIN _decline_dim1 AS d1 ON fact.k1 = d1.id
JOIN _decline_dim2 AS d2 ON fact.k2 = d2.id
JOIN _decline_dim3 AS d3 ON fact.k3 = d3.id
JOIN _decline_dim4 AS d4 ON fact.k4 = d4.id
JOIN _decline_dim5 AS d5 ON fact.k5 = d5.id
GROUP BY d1.label;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_relation_limit_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_relation_limit_enabled AS EXECUTE _decline_relation_limit_q;
SELECT pg_temp.decline_explain('aggregate_relation_limit', '_decline_relation_limit_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'aggregate_relation_limit', 'shape_too_many_relations',
        '_decline_relation_limit_native', '_decline_relation_limit_enabled',
        (SELECT kernels FROM _decline_relation_limit_before)
    ) THEN
        RAISE EXCEPTION '96 relation-limit contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_009'

-- Row-returning equality and BETWEEN joins exercise exact operator-specific
-- counters. Both fixtures include NULL keys/bounds and are mutated after
-- PREPARE. Join-method GUCs make the intended native path deterministic.
CREATE TEMP TABLE _decline_join_outer (
    id int4 PRIMARY KEY,
    equality_key int4,
    value int4
);
INSERT INTO _decline_join_outer
SELECT i,
       CASE WHEN i % 101 = 0 THEN NULL ELSE i % 1000 END,
       CASE WHEN i % 103 = 0 THEN NULL ELSE i % 10000 END
FROM generate_series(1, 100000) AS rows(i);
CREATE TEMP TABLE _decline_join_inner (
    equality_key int4,
    payload int4,
    lo int4,
    hi int4
);
INSERT INTO _decline_join_inner
SELECT CASE WHEN i % 97 = 0 THEN NULL ELSE i END,
       i * 2,
       CASE WHEN i % 89 = 0 THEN NULL ELSE i * 10 END,
       CASE WHEN i % 83 = 0 THEN NULL ELSE i * 10 + 25 END
FROM generate_series(0, 999) AS rows(i);
ANALYZE _decline_join_outer;
ANALYZE _decline_join_inner;

PREPARE _decline_hash_join_q AS
SELECT outer_row.id, outer_row.equality_key, inner_row.payload
FROM _decline_join_outer AS outer_row
JOIN _decline_join_inner AS inner_row
  ON outer_row.equality_key = inner_row.equality_key
WHERE outer_row.id <= 50000;
PREPARE _decline_nlj_q AS
SELECT outer_row.id, outer_row.value, inner_row.lo, inner_row.hi
FROM _decline_join_outer AS outer_row
JOIN _decline_join_inner AS inner_row
  ON outer_row.value BETWEEN inner_row.lo AND inner_row.hi
WHERE outer_row.id <= 5000;

INSERT INTO _decline_join_outer VALUES (100001, NULL, NULL), (100002, 1, 15);
INSERT INTO _decline_join_inner VALUES (NULL, -1, NULL, NULL), (1, -2, 10, 20);
ALTER TABLE _decline_join_outer ADD COLUMN lifecycle_tag int4 DEFAULT 0;
ALTER TABLE _decline_join_inner ADD COLUMN lifecycle_tag int4 DEFAULT 0;
ANALYZE _decline_join_outer;
ANALYZE _decline_join_inner;

SET enable_hashjoin = on;
SET enable_mergejoin = off;
SET enable_nestloop = off;
SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_hash_join_native AS
SELECT outer_row.id, outer_row.equality_key, inner_row.payload
FROM _decline_join_outer AS outer_row
JOIN _decline_join_inner AS inner_row
  ON outer_row.equality_key = inner_row.equality_key
WHERE outer_row.id <= 50000;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_hash_join_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_hash_join_enabled AS EXECUTE _decline_hash_join_q;
SELECT pg_temp.decline_explain('row_returning_hash_join', '_decline_hash_join_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'row_returning_hash_join', 'hashjoin_no_selected_gpu_kernel',
        '_decline_hash_join_native', '_decline_hash_join_enabled',
        (SELECT kernels FROM _decline_hash_join_before)
    ) THEN
        RAISE EXCEPTION '96 row-returning hash-join contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_010'

SET enable_hashjoin = off;
SET enable_mergejoin = off;
SET enable_nestloop = on;
SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_nlj_native AS
SELECT outer_row.id, outer_row.value, inner_row.lo, inner_row.hi
FROM _decline_join_outer AS outer_row
JOIN _decline_join_inner AS inner_row
  ON outer_row.value BETWEEN inner_row.lo AND inner_row.hi
WHERE outer_row.id <= 5000;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_nlj_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_nlj_enabled AS EXECUTE _decline_nlj_q;
SELECT pg_temp.decline_explain('row_returning_inequality_join', '_decline_nlj_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'row_returning_inequality_join', 'nlj_between_host_boundary_unsafe',
        '_decline_nlj_native', '_decline_nlj_enabled',
        (SELECT kernels FROM _decline_nlj_before)
    ) THEN
        RAISE EXCEPTION '96 row-returning inequality-join contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_011'
RESET enable_hashjoin;
RESET enable_mergejoin;
RESET enable_nestloop;

-- H3 scalar and LATERAL SRF native boundaries. The shared fixture contains
-- NULL points, resolutions, and h3index values. The NULL-resolution query has
-- its own exact unsupported-shape counter while the valid scalar predicate
-- must retain the declared no-resident-consumer reason.
CREATE TEMP TABLE _decline_h3 (
    id int4 PRIMARY KEY,
    geom point,
    resolution int4,
    cell h3index
);
INSERT INTO _decline_h3
SELECT i,
       CASE WHEN i % 101 = 0 THEN NULL
            ELSE point(-122.0 + (i % 100)::float8 / 10000.0,
                       37.0 + (i / 100)::float8 / 10000.0) END,
       CASE WHEN i % 103 = 0 THEN NULL ELSE 7 END,
       CASE WHEN i % 107 = 0 THEN NULL ELSE '8928308280fffff'::h3index END
FROM generate_series(1, 100000) AS rows(i);
ANALYZE _decline_h3;

PREPARE _decline_h3_scalar_q AS
SELECT id, h3_latlng_to_cell(geom, 7) AS cell
FROM _decline_h3
WHERE h3_latlng_to_cell(geom, 7) IS NOT NULL;
PREPARE _decline_h3_null_resolution_q AS
SELECT id, h3_latlng_to_cell(geom, resolution) AS cell
FROM _decline_h3
WHERE h3_latlng_to_cell(geom, resolution) IS NOT NULL;
PREPARE _decline_h3_srf_q AS
SELECT source.id, expanded.cell
FROM _decline_h3 AS source
CROSS JOIN LATERAL h3_grid_disk(source.cell, 1) AS expanded(cell)
WHERE source.id <= 2048;

INSERT INTO _decline_h3 VALUES
    (100001, NULL, NULL, NULL),
    (100002, point(-122.0, 37.0), 7, '8928308280fffff'::h3index);
ALTER TABLE _decline_h3 ADD COLUMN lifecycle_tag int4 DEFAULT 0;
ANALYZE _decline_h3;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_h3_scalar_native AS
SELECT id, h3_latlng_to_cell(geom, 7) AS cell
FROM _decline_h3
WHERE h3_latlng_to_cell(geom, 7) IS NOT NULL;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_h3_scalar_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_h3_scalar_enabled AS EXECUTE _decline_h3_scalar_q;
SELECT pg_temp.decline_explain('h3_scalar', '_decline_h3_scalar_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'h3_scalar', 'h3_latlng_scalar_predicate_no_gpu_pipeline',
        '_decline_h3_scalar_native', '_decline_h3_scalar_enabled',
        (SELECT kernels FROM _decline_h3_scalar_before)
    ) THEN
        RAISE EXCEPTION '96 H3 scalar contract returned false';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_h3_null_resolution_native AS
SELECT id, h3_latlng_to_cell(geom, resolution) AS cell
FROM _decline_h3
WHERE h3_latlng_to_cell(geom, resolution) IS NOT NULL;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_h3_null_resolution_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_h3_null_resolution_enabled AS EXECUTE _decline_h3_null_resolution_q;
SELECT pg_temp.decline_explain('h3_null_resolution', '_decline_h3_null_resolution_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'h3_null_resolution', 'h3_latlng_unsupported_shape',
        '_decline_h3_null_resolution_native', '_decline_h3_null_resolution_enabled',
        (SELECT kernels FROM _decline_h3_null_resolution_before)
    ) THEN
        RAISE EXCEPTION '96 H3 NULL-resolution contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_012'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_h3_srf_native AS
SELECT source.id, expanded.cell
FROM _decline_h3 AS source
CROSS JOIN LATERAL h3_grid_disk(source.cell, 1) AS expanded(cell)
WHERE source.id <= 2048;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_h3_srf_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_h3_srf_enabled AS EXECUTE _decline_h3_srf_q;
SELECT pg_temp.decline_explain('h3_lateral_srf', '_decline_h3_srf_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'h3_lateral_srf', 'h3_lateral_srf_no_batched_expansion',
        '_decline_h3_srf_native', '_decline_h3_srf_enabled',
        (SELECT kernels FROM _decline_h3_srf_before)
    ) THEN
        RAISE EXCEPTION '96 H3 LATERAL-SRF contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_013'

-- Five-argument PostGIS ST_Reclass is outside the released exact resident
-- subset. Compare lossless WKB text so raster equality operators are not
-- assumed by the generic result checker.
CREATE TEMP TABLE _decline_raster (
    id int4 PRIMARY KEY,
    rast raster
);
INSERT INTO _decline_raster
SELECT i,
       CASE WHEN i % 31 = 0 THEN NULL
            ELSE ST_AddBand(
                ST_MakeEmptyRaster(4, 4, 0, 0, 1, -1, 0, 0, 4326),
                '8BUI'::text,
                CASE WHEN i % 3 = 0 THEN 7 ELSE 0 END,
                255
            ) END
FROM generate_series(1, 256) AS rows(i);
ANALYZE _decline_raster;

PREPARE _decline_raster_q AS
SELECT id,
       ST_Reclass(rast, 1, '0:1,7:2,255:4', '8BUI', 0) AS rast
FROM _decline_raster;
INSERT INTO _decline_raster VALUES (257, NULL);
ALTER TABLE _decline_raster ADD COLUMN lifecycle_tag int4 DEFAULT 0;
ANALYZE _decline_raster;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_raster_native_raw AS
SELECT id,
       ST_Reclass(rast, 1, '0:1,7:2,255:4', '8BUI', 0) AS rast
FROM _decline_raster;
CREATE TEMP TABLE _decline_raster_native AS
SELECT id, encode(ST_AsBinary(rast), 'hex') AS bytes
FROM _decline_raster_native_raw;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_raster_before AS SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_raster_enabled_raw AS EXECUTE _decline_raster_q;
CREATE TEMP TABLE _decline_raster_enabled AS
SELECT id, encode(ST_AsBinary(rast), 'hex') AS bytes
FROM _decline_raster_enabled_raw;
SELECT pg_temp.decline_explain('raster', '_decline_raster_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'raster', 'raster_unsupported_shape',
        '_decline_raster_native', '_decline_raster_enabled',
        (SELECT kernels FROM _decline_raster_before)
    ) THEN
        RAISE EXCEPTION '96 raster contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_014'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_int2_native AS
SELECT g, count(short_value) AS observed_rows
FROM _decline_scalar GROUP BY g;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_int2_before AS
SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_int2_enabled AS EXECUTE _decline_int2_q;
SELECT pg_temp.decline_explain('grouped_count_int2_adjacent', '_decline_int2_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'grouped_count_int2_adjacent', 'generic_serial_kernel_mode_unqualified',
        '_decline_int2_native', '_decline_int2_enabled',
        (SELECT kernels FROM _decline_int2_before)
    ) THEN
        RAISE EXCEPTION '96 adjacent INT2 COUNT contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_015'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _decline_int8_native AS
SELECT g, count(wide_value) AS observed_rows
FROM _decline_scalar GROUP BY g;
SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
CREATE TEMP TABLE _decline_int8_before AS
SELECT pg_accel_kernel_executions() AS kernels;
CREATE TEMP TABLE _decline_int8_enabled AS EXECUTE _decline_int8_q;
SELECT pg_temp.decline_explain('grouped_count_int8_adjacent', '_decline_int8_q');
DO $$
BEGIN
    IF NOT pg_temp.decline_contract_ok(
        'grouped_count_int8_adjacent', 'generic_serial_kernel_mode_unqualified',
        '_decline_int8_native', '_decline_int8_enabled',
        (SELECT kernels FROM _decline_int8_before)
    ) THEN
        RAISE EXCEPTION '96 adjacent INT8 COUNT contract returned false';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:96_declined_lifecycle_contract.assert_016'

DEALLOCATE ALL;
DROP TABLE _decline_scalar CASCADE;
DROP TABLE _decline_fact5 CASCADE;
DROP TABLE _decline_dim1 CASCADE;
DROP TABLE _decline_dim2 CASCADE;
DROP TABLE _decline_dim3 CASCADE;
DROP TABLE _decline_dim4 CASCADE;
DROP TABLE _decline_dim5 CASCADE;
DROP TABLE _decline_join_outer CASCADE;
DROP TABLE _decline_join_inner CASCADE;
DROP TABLE _decline_h3 CASCADE;
DROP TABLE _decline_raster CASCADE;

\echo 'PGACCEL_FILE_OK:96_declined_lifecycle_contract'
