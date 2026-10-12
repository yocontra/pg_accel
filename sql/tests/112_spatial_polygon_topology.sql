\echo '=== 112_spatial_polygon_topology ==='
SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
CREATE TEMP TABLE _topology_points (id int, geom geometry(Point,4326));
INSERT INTO _topology_points
SELECT i, CASE WHEN i % 97 = 0 THEN NULL ELSE
    ST_SetSRID(ST_MakePoint((i % 1000)::float8 / 50,
                           ((i / 1000) % 1000)::float8 / 50),4326) END
FROM generate_series(1, 1000017) g(i);
INSERT INTO _topology_points VALUES (1000018, 'SRID=4326;POINT EMPTY'::geometry), (1000019, NULL);
ANALYZE _topology_points;
CREATE TEMP TABLE _topology_polygons AS
SELECT n, ST_MakePolygon(
    ST_ExteriorRing(ST_Segmentize(ST_MakeEnvelope(5,5,15,15,4326), 10.0/n)),
    ARRAY[ST_ExteriorRing(ST_Segmentize(ST_MakeEnvelope(7,7,9,9,4326), 4.0/n)),
          ST_ExteriorRing(ST_Segmentize(ST_MakeEnvelope(11,11,13,13,4326), 4.0/n))]) AS geom
FROM (VALUES (128), (512)) subdivisions(n);
DO $$
DECLARE
    has_gpu bool;
    poly record;
    predicate text;
    condition text;
    query text;
    expected bigint;
    actual bigint;
    arithmetic bigint;
    inclusive bool;
    plan text;
    line record;
    kernels_before bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_topology_points'::regclass, ARRAY['geom']);
    END IF;
    FOR poly IN SELECT n, geom FROM _topology_polygons ORDER BY n LOOP
        IF NOT ST_IsValid(poly.geom) OR ST_NumInteriorRings(poly.geom) <> 2
           OR ST_NPoints(poly.geom) <> poly.n * 8 + 3 THEN
            RAISE EXCEPTION '112 malformed polygon fixture';
        END IF;
        FOREACH predicate IN ARRAY ARRAY['intersects', 'reversed', 'contains', 'within', 'covers', 'coveredby'] LOOP
            inclusive := predicate IN ('intersects', 'reversed', 'covers', 'coveredby');
            condition := CASE predicate
                WHEN 'intersects' THEN format('ST_Intersects(geom, %L::geometry)', ST_AsEWKT(poly.geom))
                WHEN 'reversed' THEN format('ST_Intersects(%L::geometry, geom)', ST_AsEWKT(poly.geom))
                WHEN 'contains' THEN format('ST_Contains(%L::geometry, geom)', ST_AsEWKT(poly.geom))
                WHEN 'covers' THEN format('ST_Covers(%L::geometry, geom)', ST_AsEWKT(poly.geom))
                WHEN 'coveredby' THEN format('ST_CoveredBy(geom, %L::geometry)', ST_AsEWKT(poly.geom))
                ELSE format('ST_Within(geom, %L::geometry)', ST_AsEWKT(poly.geom)) END;
            query := 'SELECT count(*) FROM _topology_points WHERE ' || condition;
            -- Independent Cartesian oracle, including shell and hole boundaries.
            IF inclusive THEN
                SELECT count(*) INTO arithmetic FROM _topology_points
                WHERE geom IS NOT NULL
                  AND ST_X(geom) BETWEEN 5 AND 15 AND ST_Y(geom) BETWEEN 5 AND 15
                  AND NOT (ST_X(geom) > 7 AND ST_X(geom) < 9 AND ST_Y(geom) > 7 AND ST_Y(geom) < 9)
                  AND NOT (ST_X(geom) > 11 AND ST_X(geom) < 13 AND ST_Y(geom) > 11 AND ST_Y(geom) < 13);
            ELSE
                SELECT count(*) INTO arithmetic FROM _topology_points
                WHERE geom IS NOT NULL
                  AND ST_X(geom) > 5 AND ST_X(geom) < 15 AND ST_Y(geom) > 5 AND ST_Y(geom) < 15
                  AND NOT (ST_X(geom) BETWEEN 7 AND 9 AND ST_Y(geom) BETWEEN 7 AND 9)
                  AND NOT (ST_X(geom) BETWEEN 11 AND 13 AND ST_Y(geom) BETWEEN 11 AND 13);
            END IF;
            PERFORM set_config('pg_accel.enabled', 'off', false);
            EXECUTE query INTO expected;
            IF expected <> arithmetic THEN
                RAISE EXCEPTION '112 native/Cartesian oracle mismatch: %', predicate;
            END IF;
            PERFORM set_config('pg_accel.enabled', 'on', false);
            kernels_before := pg_accel_kernel_executions();
            EXECUTE query INTO actual;
            IF actual <> expected THEN
                RAISE EXCEPTION '112 topology mismatch: % vertices %, actual %, expected %',
                    predicate, ST_NPoints(poly.geom), actual, expected;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) ' || query LOOP
                plan := plan || line."QUERY PLAN" || E'\n';
            END LOOP;
            IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan) = 0
                            OR pg_accel_kernel_executions() <= kernels_before) THEN
                RAISE EXCEPTION '112 topology missing dispatch: % / %', predicate, plan;
            END IF;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:112_spatial_polygon_topology.assert_001'

-- Recheck every non-NULL point: shell boundary, hole boundary, hole interior.
-- PostGIS must preserve the topological distinction after residency invalidation.
DO $$
DECLARE
    has_gpu bool;
    poly geometry;
    query text;
    inclusive bool;
    expected bigint;
    actual bigint;
    phase int;
    line record;
    plan text;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT geom INTO STRICT poly FROM _topology_polygons WHERE n = 128;
    UPDATE _topology_points SET geom = CASE id % 3
        WHEN 0 THEN ST_SetSRID(ST_MakePoint(5,10),4326)
        WHEN 1 THEN ST_SetSRID(ST_MakePoint(7,8),4326)
        ELSE ST_SetSRID(ST_MakePoint(8,8),4326) END WHERE geom IS NOT NULL;
    BEGIN
        UPDATE _topology_points SET geom = NULL;
        RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'rollback fixture';
    EXCEPTION WHEN no_data_found THEN NULL;
    END;
    FOR phase IN 1..2 LOOP
        IF phase = 2 THEN UPDATE _topology_points SET geom = NULL; END IF;
        FOREACH inclusive IN ARRAY ARRAY[true, false] LOOP
            query := CASE WHEN inclusive
                THEN format('SELECT count(*) FROM _topology_points WHERE ST_Intersects(geom,%L::geometry)', ST_AsEWKT(poly))
                ELSE format('SELECT count(*) FROM _topology_points WHERE ST_Contains(%L::geometry,geom)', ST_AsEWKT(poly)) END;
            PERFORM set_config('pg_accel.enabled', 'off', false);
            EXECUTE query INTO expected;
            PERFORM set_config('pg_accel.enabled', 'on', false);
            EXECUTE query INTO actual;
            IF actual <> expected OR (NOT inclusive AND actual <> 0) THEN
                RAISE EXCEPTION '112 boundary/rollback mismatch: phase %, inclusive %', phase, inclusive;
            END IF;
            -- DML invalidates the exact residency proof. Check correctness
            -- above before explicitly reloading this resident-only lane.
            IF has_gpu THEN
                PERFORM pg_accel_pin('_topology_points'::regclass, ARRAY['geom']);
                EXECUTE query INTO actual;
                IF actual <> expected THEN
                    RAISE EXCEPTION '112 reloaded boundary mismatch: phase %, inclusive %', phase, inclusive;
                END IF;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) ' || query LOOP
                plan := plan || line."QUERY PLAN" || E'\n';
            END LOOP;
            IF has_gpu AND position('GPU Kernel Dispatched: true' IN plan) = 0 THEN
                RAISE EXCEPTION '112 changed topology missing dispatch: phase %, inclusive %, reason %, %', phase, inclusive, pg_accel_last_planner_rejection_reason(), plan;
            END IF;
        END LOOP;
    END LOOP;
    IF (SELECT stock_exec_count FROM pg_accel_stats()) <> 0 THEN
        RAISE EXCEPTION '112 unexpected stock aggregate fallback';
    END IF;
    IF EXISTS (SELECT FROM pg_backend_memory_contexts
               WHERE name = 'pg_accel spatial exact recheck') THEN
        RAISE EXCEPTION '112 exact-recheck prepared cache survived query finalization';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:112_spatial_polygon_topology.assert_002'
SELECT pg_accel_unpin('_topology_points'::regclass);
SELECT pg_accel_evict('_topology_points'::regclass);
DROP TABLE _topology_points, _topology_polygons;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:112_spatial_polygon_topology'
