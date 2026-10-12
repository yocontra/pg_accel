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
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN PERFORM pg_accel_pin('_catalog_input'::regclass,ARRAY['g','v']); END IF;
    before_count := pg_accel_kernel_executions();
    FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP
        plan := plan||line."QUERY PLAN"||E'\n';
    END LOOP;
    IF has_gpu AND (position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0
        OR pg_accel_kernel_executions()<=before_count) THEN
        RAISE EXCEPTION 'installed extension did not resume normal GPU selection: %',plan;
    ELSIF NOT has_gpu AND (position('Custom Scan (GpuAccel' IN plan)>0
        OR pg_accel_kernel_executions()<>before_count) THEN
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
