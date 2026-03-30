-- 17_multitable_agg.sql: GROUP BY across JOINs with accelerable functions
-- Tests aggregation correctness when accelerable expressions span multiple tables.

\echo '=== 17_multitable_agg ==='

BEGIN;

CREATE TEMP TABLE _mt_products (
    id serial PRIMARY KEY,
    name text NOT NULL,
    price integer NOT NULL,
    weight double precision NOT NULL
);

CREATE TEMP TABLE _mt_orders (
    id serial PRIMARY KEY,
    product_id integer NOT NULL,
    quantity integer NOT NULL,
    discount double precision NOT NULL
);

INSERT INTO _mt_products (name, price, weight)
SELECT
    CASE (i % 6)
        WHEN 0 THEN 'Widget'
        WHEN 1 THEN 'GADGET'
        WHEN 2 THEN 'Sprocket'
        WHEN 3 THEN 'FLANGE'
        WHEN 4 THEN 'bearing'
        ELSE 'Cog'
    END || '_' || i::text,
    (random() * 1000 - 200)::integer,   -- can be negative (returned items)
    random() * 50.0 + 0.1
FROM generate_series(1, 100) AS s(i);

INSERT INTO _mt_orders (product_id, quantity, discount)
SELECT
    (random() * 99 + 1)::integer,
    (random() * 20 + 1)::integer,
    random() * 0.5
FROM generate_series(1, 5000);

ANALYZE _mt_products;
ANALYZE _mt_orders;

-- ========== Test 1: JOIN + GROUP BY + accelerable agg ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _mt1_off AS
SELECT
    p.id AS pid,
    lower(p.name) AS pname,
    sum(abs(p.price) * o.quantity) AS total_value,
    avg(sqrt(p.weight)) AS avg_root_weight,
    count(*) AS order_cnt
FROM _mt_products p
JOIN _mt_orders o ON o.product_id = p.id
GROUP BY p.id, p.name
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _mt1_on AS
SELECT
    p.id AS pid,
    lower(p.name) AS pname,
    sum(abs(p.price) * o.quantity) AS total_value,
    avg(sqrt(p.weight)) AS avg_root_weight,
    count(*) AS order_cnt
FROM _mt_products p
JOIN _mt_orders o ON o.product_id = p.id
GROUP BY p.id, p.name
ORDER BY p.id;

-- ========== Test 2: JOIN + HAVING + accelerable filter ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _mt2_off AS
SELECT
    p.id AS pid,
    sum(abs(p.price)) AS sum_abs_price,
    max(length(p.name)) AS max_name_len
FROM _mt_products p
JOIN _mt_orders o ON o.product_id = p.id
GROUP BY p.id
HAVING sum(abs(p.price)) > 1000
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _mt2_on AS
SELECT
    p.id AS pid,
    sum(abs(p.price)) AS sum_abs_price,
    max(length(p.name)) AS max_name_len
FROM _mt_products p
JOIN _mt_orders o ON o.product_id = p.id
GROUP BY p.id
HAVING sum(abs(p.price)) > 1000
ORDER BY p.id;

-- ========== Test 3: Three-way JOIN with accelerable projections ==========
CREATE TEMP TABLE _mt_categories (
    id serial PRIMARY KEY,
    cat_name text NOT NULL
);
INSERT INTO _mt_categories (cat_name)
VALUES ('hardware'), ('SOFTWARE'), ('Misc');

CREATE TEMP TABLE _mt_prod_cat (
    product_id integer NOT NULL,
    category_id integer NOT NULL
);
INSERT INTO _mt_prod_cat (product_id, category_id)
SELECT id, (id % 3) + 1 FROM _mt_products;

ANALYZE _mt_categories;
ANALYZE _mt_prod_cat;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _mt3_off AS
SELECT
    upper(c.cat_name) AS category,
    count(DISTINCT p.id) AS product_cnt,
    sum(abs(p.price)) AS total_abs_price,
    avg(sqrt(p.weight)) AS avg_root_wt
FROM _mt_categories c
JOIN _mt_prod_cat pc ON pc.category_id = c.id
JOIN _mt_products p ON p.id = pc.product_id
GROUP BY c.cat_name
ORDER BY category;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _mt3_on AS
SELECT
    upper(c.cat_name) AS category,
    count(DISTINCT p.id) AS product_cnt,
    sum(abs(p.price)) AS total_abs_price,
    avg(sqrt(p.weight)) AS avg_root_wt
FROM _mt_categories c
JOIN _mt_prod_cat pc ON pc.category_id = c.id
JOIN _mt_products p ON p.id = pc.product_id
GROUP BY c.cat_name
ORDER BY category;

-- ========== Comparisons ==========
DO $$ BEGIN
    -- Test 1
    IF EXISTS (
        SELECT 1 FROM _mt1_on a FULL OUTER JOIN _mt1_off b USING (pid)
        WHERE a.pname IS DISTINCT FROM b.pname
           OR a.total_value IS DISTINCT FROM b.total_value
           OR a.avg_root_weight IS DISTINCT FROM b.avg_root_weight
           OR a.order_cnt IS DISTINCT FROM b.order_cnt
    ) THEN
        RAISE EXCEPTION '17_multitable FAILED: test 1 (JOIN+GROUP BY) results differ';
    END IF;

    -- Test 2
    IF EXISTS (
        SELECT 1 FROM _mt2_on a FULL OUTER JOIN _mt2_off b USING (pid)
        WHERE a.sum_abs_price IS DISTINCT FROM b.sum_abs_price
           OR a.max_name_len IS DISTINCT FROM b.max_name_len
    ) THEN
        RAISE EXCEPTION '17_multitable FAILED: test 2 (JOIN+HAVING) results differ';
    END IF;

    -- Test 3
    IF EXISTS (
        SELECT 1 FROM _mt3_on a FULL OUTER JOIN _mt3_off b USING (category)
        WHERE a.product_cnt IS DISTINCT FROM b.product_cnt
           OR a.total_abs_price IS DISTINCT FROM b.total_abs_price
           OR a.avg_root_wt IS DISTINCT FROM b.avg_root_wt
    ) THEN
        RAISE EXCEPTION '17_multitable FAILED: test 3 (3-way JOIN) results differ';
    END IF;
END $$;

\echo 'PASS: 17_multitable_agg (3 tests)'

DROP TABLE IF EXISTS _mt_products, _mt_orders, _mt_categories, _mt_prod_cat,
    _mt1_off, _mt1_on, _mt2_off, _mt2_on, _mt3_off, _mt3_on;

COMMIT;
