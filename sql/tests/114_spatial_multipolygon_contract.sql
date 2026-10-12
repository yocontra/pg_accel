\echo '=== 114_spatial_multipolygon_contract ==='
SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
SET pg_accel.enabled = off;
CREATE TEMP TABLE _multi_points (id int, geom geometry(Point,4326));
INSERT INTO _multi_points
SELECT i, CASE WHEN i % 97 = 0 THEN NULL ELSE
    ST_SetSRID(ST_MakePoint((i % 1000)::float8/50,
                           ((i/1000)%1000)::float8/50),4326) END
FROM generate_series(1,1000017) g(i);
INSERT INTO _multi_points VALUES (1000018,'SRID=4326;POINT EMPTY'),(1000019,NULL);
ANALYZE _multi_points;
CREATE TEMP TABLE _multi_constant AS
SELECT ST_Segmentize('SRID=4326;MULTIPOLYGON(
 ((1 1,9 1,9 9,1 9,1 1),(3 3,7 3,7 7,3 7,3 3)),
 ((4 4,6 4,6 6,4 6,4 4)),
 ((12 2,16 2,16 8,12 8,12 2),(13 4,15 4,15 6,13 6,13 4)),
 ((17 11,19 11,19 13,17 13,17 11)))'::geometry,0.0625) AS geom;

SET plan_cache_mode=force_generic_plan;
DO $$ DECLARE area geometry; BEGIN
    SELECT geom INTO STRICT area FROM _multi_constant;
    EXECUTE format('PREPARE _multi_covers AS SELECT count(*) FROM _multi_points WHERE ST_Covers(%L::geometry,geom)',ST_AsEWKT(area));
    EXECUTE format('PREPARE _multi_coveredby AS SELECT count(*) FROM _multi_points WHERE ST_CoveredBy(geom,%L::geometry)',ST_AsEWKT(area));
END $$;

CREATE FUNCTION pg_temp.check_multi() RETURNS void LANGUAGE plpgsql AS $$
DECLARE
    area geometry;
    predicate text;
    condition text;
    query text;
    inclusive bool;
    arithmetic bigint;
    expected bigint;
    actual bigint;
    plan text;
    line record;
    kernels bigint;
    has_gpu bool;
BEGIN
    SELECT geom INTO STRICT area FROM _multi_constant;
    IF NOT ST_IsValid(area) OR ST_NumGeometries(area) <> 4 OR ST_NPoints(area) < 1025 THEN
        RAISE EXCEPTION '114 multipolygon fixture lost components or vertices';
    END IF;
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN PERFORM pg_accel_pin('_multi_points'::regclass,ARRAY['geom']); END IF;
    FOREACH predicate IN ARRAY ARRAY['intersects','reversed','contains','within','covers','coveredby'] LOOP
        inclusive := predicate IN ('intersects','reversed','covers','coveredby');
        PERFORM set_config('pg_accel.enabled','off',false);
        -- Independent Cartesian union: each shell excludes only its own hole.
        -- The island in the first hole is deliberately a separate component.
        WITH xy AS (SELECT ST_X(geom) x, ST_Y(geom) y FROM _multi_points)
        SELECT count(*) INTO arithmetic FROM xy WHERE
          CASE WHEN inclusive THEN
            ((x BETWEEN 1 AND 9 AND y BETWEEN 1 AND 9)
                AND NOT (x>3 AND x<7 AND y>3 AND y<7))
            OR (x BETWEEN 4 AND 6 AND y BETWEEN 4 AND 6)
            OR ((x BETWEEN 12 AND 16 AND y BETWEEN 2 AND 8)
                AND NOT (x>13 AND x<15 AND y>4 AND y<6))
            OR (x BETWEEN 17 AND 19 AND y BETWEEN 11 AND 13)
          ELSE
            ((x>1 AND x<9 AND y>1 AND y<9)
                AND NOT (x BETWEEN 3 AND 7 AND y BETWEEN 3 AND 7))
            OR (x>4 AND x<6 AND y>4 AND y<6)
            OR ((x>12 AND x<16 AND y>2 AND y<8)
                AND NOT (x BETWEEN 13 AND 15 AND y BETWEEN 4 AND 6))
            OR (x>17 AND x<19 AND y>11 AND y<13)
          END;
        condition := CASE predicate
            WHEN 'intersects' THEN format('ST_Intersects(geom,%L::geometry)',ST_AsEWKT(area))
            WHEN 'reversed' THEN format('ST_Intersects(%L::geometry(MultiPolygon,4326),geom)',ST_AsEWKT(area))
            WHEN 'contains' THEN format('ST_Contains(%L::geometry,geom)',ST_AsEWKT(area))
            WHEN 'covers' THEN format('ST_Covers(%L::geometry,geom)',ST_AsEWKT(area))
            WHEN 'coveredby' THEN format('ST_CoveredBy(geom,%L::geometry)',ST_AsEWKT(area))
            ELSE format('ST_Within(geom,%L::geometry)',ST_AsEWKT(area)) END;
        query := 'SELECT count(*) FROM _multi_points WHERE ' || condition;
        EXECUTE query INTO expected;
        IF expected <> arithmetic THEN
            RAISE EXCEPTION '114 native/Cartesian mismatch for %: % versus %',predicate,expected,arithmetic;
        END IF;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        EXECUTE query INTO actual;
        IF actual <> expected THEN
            RAISE EXCEPTION '114 accelerated multipolygon mismatch for %: % versus %',predicate,actual,expected;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) ' || query LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0
                       OR pg_accel_kernel_executions() <= kernels) THEN
            RAISE EXCEPTION '114 multipolygon missing dispatch: % / % / %',predicate,
                pg_accel_last_planner_rejection_reason(),plan;
        END IF;
    END LOOP;
    -- Keep selected plans through changed point values and rollback. The last
    -- native oracle above is CoveredBy, equal to Covers for these point rows.
    FOREACH query IN ARRAY ARRAY['EXECUTE _multi_covers','EXECUTE _multi_coveredby'] LOOP
        EXECUTE query INTO actual;
        IF actual <> expected THEN
            RAISE EXCEPTION '114 prepared inclusive predicate mismatch: %',query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) ' || query LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF has_gpu AND position('GPU Kernel Dispatched: true' IN plan)=0 THEN
            RAISE EXCEPTION '114 prepared inclusive predicate missing dispatch: % / %',query,plan;
        END IF;
    END LOOP;
    IF (SELECT stock_exec_count FROM pg_accel_stats()) <> 0 THEN
        RAISE EXCEPTION '114 stock executor fallback';
    END IF;
    IF EXISTS (SELECT FROM pg_backend_memory_contexts WHERE name='pg_accel spatial exact recheck') THEN
        RAISE EXCEPTION '114 exact-recheck cache leaked';
    END IF;
END $$;
SELECT pg_temp.check_multi();
\echo 'PGACCEL_ASSERT_OK:114_spatial_multipolygon_contract.assert_001'

-- Change points onto component/hole boundaries, into a hole and onto an island.
BEGIN;
UPDATE _multi_points SET geom = CASE id % 4
    WHEN 0 THEN ST_SetSRID(ST_MakePoint(1,5),4326)
    WHEN 1 THEN ST_SetSRID(ST_MakePoint(3,5),4326)
    WHEN 2 THEN ST_SetSRID(ST_MakePoint(3.5,5),4326)
    ELSE ST_SetSRID(ST_MakePoint(5,5),4326) END WHERE geom IS NOT NULL;
SELECT pg_temp.check_multi();
ROLLBACK;
DO $$ BEGIN
    IF (SELECT count(*) FROM _multi_points) <> 1000019 THEN
        RAISE EXCEPTION '114 rollback fixture changed row count';
    END IF;
    PERFORM pg_temp.check_multi();
END $$;
\echo 'PGACCEL_ASSERT_OK:114_spatial_multipolygon_contract.assert_002'

-- The existing PostGIS validity gate must still decline nested/overlapping
-- shells. Keep exact native results and the explicit rejection, not a GPU claim.
DO $$
DECLARE
    area geometry;
    predicate text;
    query text;
    expected bigint;
    actual bigint;
    arithmetic bigint;
    plan text;
    line record;
    has_gpu bool;
    kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    area := ST_Segmentize(ST_Collect(ST_MakeEnvelope(1,1,9,9,4326),
                                    ST_MakeEnvelope(3,3,7,7,4326)),0.03125);
    IF ST_IsValid(area) OR ST_NumGeometries(area) <> 2 THEN
        RAISE EXCEPTION '114 overlapping-component negative fixture changed';
    END IF;
    IF has_gpu THEN
        PERFORM pg_accel_pin('_multi_points'::regclass,ARRAY['geom']);
    END IF;
    FOREACH predicate IN ARRAY ARRAY['ST_Intersects','ST_Contains','ST_Covers','ST_CoveredBy'] LOOP
        PERFORM set_config('pg_accel.enabled','off',false);
        query := format('SELECT count(*) FROM _multi_points WHERE %s(%L::geometry,geom)',
                        predicate,ST_AsEWKT(area));
        IF predicate='ST_CoveredBy' THEN
            query := format('SELECT count(*) FROM _multi_points WHERE ST_CoveredBy(geom,%L::geometry)',ST_AsEWKT(area));
        END IF;
        EXECUTE query INTO expected;
        SELECT count(*) INTO arithmetic FROM _multi_points WHERE
          CASE WHEN predicate='ST_Intersects' THEN
            ST_X(geom) BETWEEN 1 AND 9 AND ST_Y(geom) BETWEEN 1 AND 9
          ELSE ST_X(geom)>1 AND ST_X(geom)<9 AND ST_Y(geom)>1 AND ST_Y(geom)<9 END;
        -- Covers on invalid geometry has no independent mathematical oracle;
        -- retain native parity and the mandatory planner decline below.
        IF predicate IN ('ST_Intersects','ST_Contains') AND expected <> arithmetic THEN
            RAISE EXCEPTION '114 native overlapping-component oracle mismatch';
        END IF;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        EXECUTE query INTO actual;
        IF actual <> expected THEN RAISE EXCEPTION '114 overlapping component mismatch'; END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) ' || query LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF has_gpu AND (position('GpuAccel' IN plan)<>0
                        OR pg_accel_kernel_executions()<>kernels
                        OR pg_accel_last_planner_rejection_reason()<>'shape_invalid_spatial_constant') THEN
            RAISE EXCEPTION '114 invalid multipolygon did not remain native: % / % / %',
                predicate,pg_accel_last_planner_rejection_reason(),left(plan,4096);
        END IF;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:114_spatial_multipolygon_contract.assert_003'

SELECT pg_accel_unpin('_multi_points'::regclass);
SELECT pg_accel_evict('_multi_points'::regclass);
DROP FUNCTION pg_temp.check_multi();
DEALLOCATE _multi_covers;
DEALLOCATE _multi_coveredby;
RESET plan_cache_mode;
DROP TABLE _multi_points, _multi_constant;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:114_spatial_multipolygon_contract'
