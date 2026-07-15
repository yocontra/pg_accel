-- 21_join_types.sql: Hash join, nested loop, self-join, LEFT JOIN spatial
-- Tests specific join strategies with accelerable expressions.

\echo '=== 21_join_types ==='

BEGIN;

CREATE TEMP TABLE _jt_customers (
    id serial PRIMARY KEY,
    name text NOT NULL,
    geom geometry(Point, 4326),
    tier integer NOT NULL
);

CREATE TEMP TABLE _jt_orders (
    id serial PRIMARY KEY,
    customer_id integer NOT NULL,
    amount integer NOT NULL,
    label text NOT NULL
);

INSERT INTO _jt_customers (name, geom, tier)
SELECT
    'cust_' || i::text,
    ST_SetSRID(ST_MakePoint(-74.0 + random() * 0.1, 40.7 + random() * 0.1), 4326),
    (i % 5)
FROM generate_series(1, 500) AS s(i);

INSERT INTO _jt_orders (customer_id, amount, label)
SELECT
    (random() * 499 + 1)::integer,
    (random() * 2000 - 500)::integer,
    CASE (i % 3) WHEN 0 THEN 'retail' WHEN 1 THEN 'WHOLESALE' ELSE 'Online' END
FROM generate_series(1, 5000) AS s(i);

-- Service zones for spatial LEFT JOIN
CREATE TEMP TABLE _jt_zones (
    id serial PRIMARY KEY,
    geom geometry(Polygon, 4326) NOT NULL,
    zone_name text NOT NULL
);

INSERT INTO _jt_zones (geom, zone_name)
SELECT
    ST_SetSRID(ST_MakeEnvelope(
        -74.0 + (i % 3) * 0.03,
        40.7 + (i / 3) * 0.03,
        -74.0 + (i % 3) * 0.03 + 0.04,
        40.7 + (i / 3) * 0.03 + 0.04
    ), 4326),
    'zone_' || i::text
FROM generate_series(0, 8) AS s(i);

CREATE INDEX _jt_cust_idx ON _jt_customers(id);
CREATE INDEX _jt_ord_cust_idx ON _jt_orders(customer_id);
CREATE INDEX _jt_cust_gist ON _jt_customers USING gist(geom);
CREATE INDEX _jt_zones_gist ON _jt_zones USING gist(geom);

ANALYZE _jt_customers;
ANALYZE _jt_orders;
ANALYZE _jt_zones;

-- ========== Test 1: Hash join with residual accelerable predicate ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _jt1_off AS
SELECT c.id AS cid, lower(c.name) AS cname,
    sum(abs(o.amount)) AS total_abs,
    count(*) AS cnt
FROM _jt_customers c
JOIN _jt_orders o ON o.customer_id = c.id
WHERE abs(o.amount) > 200
GROUP BY c.id, c.name
ORDER BY c.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _jt1_on AS
SELECT c.id AS cid, lower(c.name) AS cname,
    sum(abs(o.amount)) AS total_abs,
    count(*) AS cnt
FROM _jt_customers c
JOIN _jt_orders o ON o.customer_id = c.id
WHERE abs(o.amount) > 200
GROUP BY c.id, c.name
ORDER BY c.id;

-- ========== Test 2: Nested loop with index (small driving table) ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _jt2_off AS
SELECT c.id AS cid, upper(c.name) AS cname, o.id AS oid, abs(o.amount) AS abs_amt
FROM _jt_customers c
JOIN _jt_orders o ON o.customer_id = c.id
WHERE c.tier = 0 AND c.id <= 20
ORDER BY c.id, o.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _jt2_on AS
SELECT c.id AS cid, upper(c.name) AS cname, o.id AS oid, abs(o.amount) AS abs_amt
FROM _jt_customers c
JOIN _jt_orders o ON o.customer_id = c.id
WHERE c.tier = 0 AND c.id <= 20
ORDER BY c.id, o.id;

-- ========== Test 3: Spatial join (the money query) ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _jt3_off AS
SELECT c.id AS cid, z.zone_name,
    ST_Distance(c.geom, ST_Centroid(z.geom)) AS dist_to_center
FROM _jt_customers c
JOIN _jt_zones z ON ST_Contains(z.geom, c.geom)
ORDER BY cid, z.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _jt3_on AS
SELECT c.id AS cid, z.zone_name,
    ST_Distance(c.geom, ST_Centroid(z.geom)) AS dist_to_center
FROM _jt_customers c
JOIN _jt_zones z ON ST_Contains(z.geom, c.geom)
ORDER BY cid, z.id;

-- ========== Test 4: Self-join with accelerable expression ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _jt4_off AS
SELECT a.id AS id_a, b.id AS id_b,
    abs(a.amount - b.amount) AS diff
FROM _jt_orders a
JOIN _jt_orders b ON a.customer_id = b.customer_id AND a.id < b.id
WHERE a.customer_id <= 5
ORDER BY id_a, id_b;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _jt4_on AS
SELECT a.id AS id_a, b.id AS id_b,
    abs(a.amount - b.amount) AS diff
FROM _jt_orders a
JOIN _jt_orders b ON a.customer_id = b.customer_id AND a.id < b.id
WHERE a.customer_id <= 5
ORDER BY id_a, id_b;

-- ========== Test 5: LEFT JOIN with spatial predicate (NULLs for unmatched) ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _jt5_off AS
SELECT c.id AS cid, lower(c.name) AS cname,
    z.zone_name,
    CASE WHEN z.id IS NULL THEN true ELSE false END AS unzoned
FROM _jt_customers c
LEFT JOIN _jt_zones z ON ST_Contains(z.geom, c.geom)
ORDER BY cid, z.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _jt5_on AS
SELECT c.id AS cid, lower(c.name) AS cname,
    z.zone_name,
    CASE WHEN z.id IS NULL THEN true ELSE false END AS unzoned
FROM _jt_customers c
LEFT JOIN _jt_zones z ON ST_Contains(z.geom, c.geom)
ORDER BY cid, z.id;

-- ========== Comparisons ==========
DO $$ BEGIN
    -- Test 1: hash join
    IF EXISTS (
        SELECT 1 FROM _jt1_on a FULL OUTER JOIN _jt1_off b USING (cid)
        WHERE a.cname IS DISTINCT FROM b.cname
           OR a.total_abs IS DISTINCT FROM b.total_abs
           OR a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '21_joins FAILED: test 1 (hash join + residual) differs';
    END IF;

    -- Test 2: nested loop
    IF EXISTS (
        SELECT 1 FROM _jt2_on a FULL OUTER JOIN _jt2_off b ON a.cid = b.cid AND a.oid = b.oid
        WHERE a.cname IS DISTINCT FROM b.cname
           OR a.abs_amt IS DISTINCT FROM b.abs_amt
    ) THEN
        RAISE EXCEPTION '21_joins FAILED: test 2 (nested loop) differs';
    END IF;

    -- Test 3: spatial join
    IF EXISTS (
        (SELECT cid, zone_name FROM _jt3_on EXCEPT SELECT cid, zone_name FROM _jt3_off)
        UNION ALL
        (SELECT cid, zone_name FROM _jt3_off EXCEPT SELECT cid, zone_name FROM _jt3_on)
    ) THEN
        RAISE EXCEPTION '21_joins FAILED: test 3 (spatial join) differs';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _jt3_on a FULL OUTER JOIN _jt3_off b ON a.cid = b.cid AND a.zone_name = b.zone_name
        WHERE a.dist_to_center IS DISTINCT FROM b.dist_to_center
    ) THEN
        RAISE EXCEPTION '21_joins FAILED: test 3 (spatial join distances) differs';
    END IF;

    -- Test 4: self-join
    IF EXISTS (
        SELECT 1 FROM _jt4_on a FULL OUTER JOIN _jt4_off b ON a.id_a = b.id_a AND a.id_b = b.id_b
        WHERE a.diff IS DISTINCT FROM b.diff
    ) THEN
        RAISE EXCEPTION '21_joins FAILED: test 4 (self-join) differs';
    END IF;

    -- Test 5: LEFT JOIN spatial
    IF (SELECT count(*) FROM _jt5_on) <> (SELECT count(*) FROM _jt5_off) THEN
        RAISE EXCEPTION '21_joins FAILED: test 5 (LEFT JOIN) row count differs';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _jt5_on a FULL OUTER JOIN _jt5_off b ON a.cid = b.cid
            AND a.zone_name IS NOT DISTINCT FROM b.zone_name
        WHERE a.cname IS DISTINCT FROM b.cname
           OR a.unzoned IS DISTINCT FROM b.unzoned
    ) THEN
        RAISE EXCEPTION '21_joins FAILED: test 5 (LEFT JOIN spatial) differs';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:21_join_types.assert_001'



DROP TABLE IF EXISTS _jt_customers, _jt_orders, _jt_zones,
    _jt1_off, _jt1_on, _jt2_off, _jt2_on,
    _jt3_off, _jt3_on, _jt4_off, _jt4_on,
    _jt5_off, _jt5_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:21_join_types'
