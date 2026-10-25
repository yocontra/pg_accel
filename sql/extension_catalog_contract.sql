-- Run only through scripts/extension_catalog_integration.sh in its owned DB.
SELECT current_database() = :'expected_database' AS owned_database \gset
\if :owned_database
\else
  \quit 1
\endif
DO $$ BEGIN
    IF EXISTS (SELECT FROM pg_extension WHERE extname='pg_accel') THEN
        RAISE EXCEPTION 'test database must start without pg_accel SQL objects';
    END IF;
    IF position('pg_accel' IN current_setting('shared_preload_libraries'))=0 THEN
        RAISE EXCEPTION 'test requires the production shared-preloaded module';
    END IF;
END $$;
SET pg_accel.enabled=on;
SET pg_accel.gpu_enabled=on;
SET pg_accel.auto_load=on;
SET plan_cache_mode=force_generic_plan;
CREATE TEMP TABLE _catalog_input AS
SELECT i%64 g,i%1000 v FROM generate_series(1,500000) s(i);
CREATE INDEX ON _catalog_input(v);
ANALYZE _catalog_input;
SET pg_accel.enabled=off;
CREATE TEMP TABLE _catalog_expected AS
SELECT g,sum(v) total,count(*) n FROM _catalog_input GROUP BY g;
SET pg_accel.enabled=on;

-- This ceiling bounds test data, not production admission. One-shot limits
-- bound individual dispatches, so they must not cap this chunked query's rows.
CREATE FUNCTION pg_temp.catalog_fixture_rows(device_floor bigint, fixture_ceiling bigint DEFAULT 4000000)
RETURNS integer LANGUAGE plpgsql IMMUTABLE AS $$
BEGIN
    IF device_floor IS NULL OR device_floor<=0
        OR fixture_ceiling IS NULL OR fixture_ceiling<500000
        OR fixture_ceiling>4000000 OR fixture_ceiling>2147483647
        OR device_floor>fixture_ceiling/2 THEN
        RAISE EXCEPTION 'invalid or insufficient catalog fixture ceiling: floor=%, ceiling=%',
            device_floor,fixture_ceiling USING ERRCODE='22023';
    END IF;
    RETURN greatest(500000,2*device_floor)::integer;
END $$;
DO $$
DECLARE sample record; rejected bool;
BEGIN
    IF pg_temp.catalog_fixture_rows(250000)<>500000
        OR pg_temp.catalog_fixture_rows(2000000)<>4000000 THEN
        RAISE EXCEPTION 'catalog fixture sizing contract changed';
    END IF;
    FOR sample IN SELECT * FROM (VALUES
        (NULL::bigint,4000000::bigint),(0,4000000),(-1,4000000),
        (250000,NULL),(250000,499999),(2000000,3999999),
        (2000001,4000000),(9223372036854775807,4000000),
        (250000,2147483648)
    ) AS invalid(device_floor,fixture_ceiling) LOOP
        rejected := false;
        BEGIN
            PERFORM pg_temp.catalog_fixture_rows(sample.device_floor,sample.fixture_ceiling);
        EXCEPTION WHEN invalid_parameter_value THEN rejected := true;
        END;
        IF NOT rejected THEN
            RAISE EXCEPTION 'invalid catalog fixture sizing was accepted: %',sample;
        END IF;
    END LOOP;
END $$;

CREATE FUNCTION pg_temp.assert_native(query text) RETURNS void LANGUAGE plpgsql AS $$
DECLARE line record;
BEGIN
    FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP
        IF position('Custom Scan (GpuAccel' IN line."QUERY PLAN")>0 THEN
            RAISE EXCEPTION 'preload-only query selected a pg_accel node: %',query;
        END IF;
    END LOOP;
END $$;
CREATE FUNCTION pg_temp.assert_selected(query text) RETURNS void LANGUAGE plpgsql AS $$
DECLARE line record; plan text := ''; has_gpu bool; before_count bigint;
    after_count bigint; rejection text; device_limits jsonb;
    row_floor bigint; one_shot_cap bigint; desired_rows integer; existing_rows bigint;
    prior_enabled text;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT jsonb_agg(to_jsonb(l) ORDER BY name) INTO device_limits FROM pg_accel_device_limits() l;
    IF has_gpu THEN
        SELECT value::bigint INTO STRICT row_floor FROM pg_accel_device_limits()
            WHERE name='gpu_hash_agg_min_rows';
        SELECT value::bigint INTO STRICT one_shot_cap FROM pg_accel_device_limits()
            WHERE name='gpu_grouped_agg_one_shot_max_rows';
        desired_rows := pg_temp.catalog_fixture_rows(row_floor);
        prior_enabled := current_setting('pg_accel.enabled');
        PERFORM set_config('pg_accel.enabled','off',true);
        SELECT count(*) INTO existing_rows FROM _catalog_input;
        IF existing_rows<desired_rows THEN
            INSERT INTO _catalog_input
                SELECT i%64,i%1000 FROM generate_series((existing_rows+1)::integer,desired_rows) s(i);
            ANALYZE _catalog_input;
            -- Fixture growth and its native oracle roll back together with
            -- CREATE EXTENSION; no requested GPU policy is changed.
            TRUNCATE _catalog_expected;
            INSERT INTO _catalog_expected SELECT g,sum(v),count(*) FROM _catalog_input GROUP BY g;
        END IF;
        PERFORM set_config('pg_accel.enabled',prior_enabled,true);
        RAISE NOTICE 'catalog fixture rows=%, device floor=%, one-shot cap=%, limits=%',
            greatest(existing_rows,desired_rows),row_floor,one_shot_cap,device_limits;
        PERFORM pg_accel_pin('_catalog_input'::regclass,ARRAY['g','v']);
    END IF;
    before_count := pg_accel_kernel_executions();
    FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP
        plan := plan||line."QUERY PLAN"||E'\n';
    END LOOP;
    rejection := pg_accel_last_planner_rejection_reason();
    after_count := pg_accel_kernel_executions();
    IF has_gpu AND (position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0
        OR after_count<=before_count) THEN
        RAISE EXCEPTION 'installed extension did not resume normal GPU selection: rejection=%, kernels=%->%, limits=%; plan=%',
            rejection,before_count,after_count,device_limits,plan;
    ELSIF NOT has_gpu AND (position('Custom Scan (GpuAccel' IN plan)>0
        OR after_count<>before_count) THEN
        RAISE EXCEPTION 'no-device catalog lifecycle must remain native: %',plan;
    END IF;
    CREATE TEMP TABLE _catalog_actual AS SELECT g,sum(v) total,count(*) n FROM _catalog_input GROUP BY g;
    IF EXISTS ((SELECT * FROM _catalog_actual EXCEPT ALL SELECT * FROM _catalog_expected)
        UNION ALL (SELECT * FROM _catalog_expected EXCEPT ALL SELECT * FROM _catalog_actual)) THEN
        RAISE EXCEPTION 'extension catalog lifecycle changed aggregate output';
    END IF;
    DROP TABLE _catalog_actual;
END $$;

-- An identically named non-member must not establish extension presence.
CREATE SCHEMA spoof;
CREATE FUNCTION spoof.pg_accel_residency_invalidate() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'spoofed invalidator called'; END $$;
SET search_path=spoof,public;
SELECT pg_temp.assert_native('SELECT g,sum(v),count(*) FROM _catalog_input GROUP BY g');
SET enable_seqscan=off;
SELECT pg_temp.assert_native('SELECT count(*) FROM _catalog_input WHERE v>1');
RESET enable_seqscan;
RESET search_path;
\echo 'PGACCEL_CATALOG_OK:preload_only'

-- A negative lookup must not survive CREATE EXTENSION, nor may a positive
-- lookup survive rollback of that installation in the same backend.
BEGIN;
CREATE EXTENSION pg_accel;
SELECT pg_temp.assert_selected('SELECT g,sum(v),count(*) FROM _catalog_input GROUP BY g');
ROLLBACK;
SELECT pg_temp.assert_native('SELECT g,sum(v),count(*) FROM _catalog_input GROUP BY g');
\echo 'PGACCEL_CATALOG_OK:create_rollback'

CREATE EXTENSION pg_accel;
SELECT pg_temp.assert_selected('SELECT g,sum(v),count(*) FROM _catalog_input GROUP BY g');
PREPARE _catalog_prepared AS SELECT g,sum(v),count(*) FROM _catalog_input GROUP BY g;
SELECT pg_temp.assert_selected('EXECUTE _catalog_prepared');
\echo 'PGACCEL_CATALOG_OK:installed'

BEGIN;
DROP EXTENSION pg_accel CASCADE;
SELECT pg_temp.assert_native('SELECT g,sum(v),count(*) FROM _catalog_input GROUP BY g');
SELECT pg_temp.assert_native('EXECUTE _catalog_prepared');
ROLLBACK;
SELECT pg_temp.assert_selected('SELECT g,sum(v),count(*) FROM _catalog_input GROUP BY g');
SELECT pg_temp.assert_selected('EXECUTE _catalog_prepared');
\echo 'PGACCEL_CATALOG_OK:drop_rollback'

DROP EXTENSION pg_accel CASCADE;
SELECT pg_temp.assert_native('SELECT g,sum(v),count(*) FROM _catalog_input GROUP BY g');
SELECT pg_temp.assert_native('EXECUTE _catalog_prepared');
CREATE EXTENSION pg_accel;
SELECT pg_temp.assert_selected('SELECT g,sum(v),count(*) FROM _catalog_input GROUP BY g');
DEALLOCATE _catalog_prepared;
\echo 'PGACCEL_CATALOG_OK:drop_recreate'
