-- 06_mixed_queries.sql: JOINs, subqueries, CTEs with accelerable functions
-- Verifies correctness in complex query shapes.

\echo '=== 06_mixed_queries ==='

BEGIN;

CREATE TEMP TABLE _mq_orders (
    id serial PRIMARY KEY,
    customer_id integer NOT NULL,
    amount integer NOT NULL,
    label text NOT NULL
);

CREATE TEMP TABLE _mq_customers (
    id serial PRIMARY KEY,
    name text NOT NULL,
    score double precision NOT NULL
);

INSERT INTO _mq_customers (name, score)
SELECT
    CASE (i % 4)
        WHEN 0 THEN 'Alice'
        WHEN 1 THEN 'BOB'
        WHEN 2 THEN 'Charlie'
        ELSE 'Diana'
    END,
    random() * 100.0 + 0.01
FROM generate_series(1, 200) AS s(i);

INSERT INTO _mq_orders (customer_id, amount, label)
SELECT
    (random() * 199 + 1)::integer,
    (random() * 2000 - 1000)::integer,
    CASE (i % 3)
        WHEN 0 THEN 'Purchase'
        WHEN 1 THEN 'REFUND'
        ELSE 'adjustment'
    END
FROM generate_series(1, 5000) AS s(i);

ANALYZE _mq_orders;
ANALYZE _mq_customers;

-- Baseline: accel OFF
SET pg_accel.enabled = off;

-- Test 1: JOIN with accelerable functions on both sides
CREATE TEMP TABLE _mq1_off AS
SELECT
    o.id,
    abs(o.amount)     AS abs_amt,
    lower(c.name)     AS lower_name,
    sqrt(c.score)     AS sqrt_score,
    upper(o.label)    AS upper_label
FROM _mq_orders o
JOIN _mq_customers c ON c.id = o.customer_id
ORDER BY o.id;

-- Test 2: Subquery with accelerable function
CREATE TEMP TABLE _mq2_off AS
SELECT id, abs_amt FROM (
    SELECT id, abs(amount) AS abs_amt
    FROM _mq_orders
    WHERE abs(amount) > 500
) sub
ORDER BY id;

-- Test 3: CTE with accelerable functions
CREATE TEMP TABLE _mq3_off AS
WITH enriched AS (
    SELECT
        o.id,
        abs(o.amount) AS abs_amt,
        lower(c.name) AS cust_name,
        sqrt(c.score) AS root_score
    FROM _mq_orders o
    JOIN _mq_customers c ON c.id = o.customer_id
),
aggregated AS (
    SELECT
        cust_name,
        sum(abs_amt)    AS total_abs,
        avg(root_score) AS avg_root,
        count(*)        AS cnt
    FROM enriched
    GROUP BY cust_name
)
SELECT * FROM aggregated
ORDER BY cust_name;

-- Test 4: LEFT JOIN with NULLs from missing join matches
CREATE TEMP TABLE _mq4_off AS
SELECT
    c.id AS cust_id,
    lower(c.name) AS lower_name,
    coalesce(sum(abs(o.amount)), 0) AS total_abs_amt,
    count(o.id) AS order_count
FROM _mq_customers c
LEFT JOIN _mq_orders o ON o.customer_id = c.id
GROUP BY c.id, c.name
ORDER BY c.id;

-- Test 5: Correlated subquery
CREATE TEMP TABLE _mq5_off AS
SELECT
    c.id,
    lower(c.name) AS lower_name,
    (SELECT sum(abs(o.amount)) FROM _mq_orders o WHERE o.customer_id = c.id) AS total
FROM _mq_customers c
ORDER BY c.id;

-- Test: accel ON
SET pg_accel.enabled = on;

CREATE TEMP TABLE _mq1_on AS
SELECT
    o.id,
    abs(o.amount)     AS abs_amt,
    lower(c.name)     AS lower_name,
    sqrt(c.score)     AS sqrt_score,
    upper(o.label)    AS upper_label
FROM _mq_orders o
JOIN _mq_customers c ON c.id = o.customer_id
ORDER BY o.id;

CREATE TEMP TABLE _mq2_on AS
SELECT id, abs_amt FROM (
    SELECT id, abs(amount) AS abs_amt
    FROM _mq_orders
    WHERE abs(amount) > 500
) sub
ORDER BY id;

CREATE TEMP TABLE _mq3_on AS
WITH enriched AS (
    SELECT
        o.id,
        abs(o.amount) AS abs_amt,
        lower(c.name) AS cust_name,
        sqrt(c.score) AS root_score
    FROM _mq_orders o
    JOIN _mq_customers c ON c.id = o.customer_id
),
aggregated AS (
    SELECT
        cust_name,
        sum(abs_amt)    AS total_abs,
        avg(root_score) AS avg_root,
        count(*)        AS cnt
    FROM enriched
    GROUP BY cust_name
)
SELECT * FROM aggregated
ORDER BY cust_name;

CREATE TEMP TABLE _mq4_on AS
SELECT
    c.id AS cust_id,
    lower(c.name) AS lower_name,
    coalesce(sum(abs(o.amount)), 0) AS total_abs_amt,
    count(o.id) AS order_count
FROM _mq_customers c
LEFT JOIN _mq_orders o ON o.customer_id = c.id
GROUP BY c.id, c.name
ORDER BY c.id;

CREATE TEMP TABLE _mq5_on AS
SELECT
    c.id,
    lower(c.name) AS lower_name,
    (SELECT sum(abs(o.amount)) FROM _mq_orders o WHERE o.customer_id = c.id) AS total
FROM _mq_customers c
ORDER BY c.id;

-- Compare all
DO $$ BEGIN
    -- Test 1: JOIN
    IF EXISTS (
        SELECT 1 FROM _mq1_on a FULL OUTER JOIN _mq1_off b USING (id)
        WHERE a.abs_amt     IS DISTINCT FROM b.abs_amt
           OR a.lower_name  IS DISTINCT FROM b.lower_name
           OR a.sqrt_score  IS DISTINCT FROM b.sqrt_score
           OR a.upper_label IS DISTINCT FROM b.upper_label
    ) THEN
        RAISE EXCEPTION '06_mixed FAILED: test 1 (JOIN) results differ';
    END IF;

    -- Test 2: Subquery
    IF EXISTS (
        SELECT 1 FROM _mq2_on a FULL OUTER JOIN _mq2_off b USING (id)
        WHERE a.abs_amt IS DISTINCT FROM b.abs_amt
    ) THEN
        RAISE EXCEPTION '06_mixed FAILED: test 2 (subquery) results differ';
    END IF;

    -- Test 3: CTE
    IF EXISTS (
        SELECT 1 FROM _mq3_on a FULL OUTER JOIN _mq3_off b USING (cust_name)
        WHERE a.total_abs IS DISTINCT FROM b.total_abs
           OR a.avg_root  IS DISTINCT FROM b.avg_root
           OR a.cnt       IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '06_mixed FAILED: test 3 (CTE) results differ';
    END IF;

    -- Test 4: LEFT JOIN
    IF EXISTS (
        SELECT 1 FROM _mq4_on a FULL OUTER JOIN _mq4_off b USING (cust_id)
        WHERE a.lower_name    IS DISTINCT FROM b.lower_name
           OR a.total_abs_amt IS DISTINCT FROM b.total_abs_amt
           OR a.order_count   IS DISTINCT FROM b.order_count
    ) THEN
        RAISE EXCEPTION '06_mixed FAILED: test 4 (LEFT JOIN) results differ';
    END IF;

    -- Test 5: Correlated subquery
    IF EXISTS (
        SELECT 1 FROM _mq5_on a FULL OUTER JOIN _mq5_off b USING (id)
        WHERE a.lower_name IS DISTINCT FROM b.lower_name
           OR a.total      IS DISTINCT FROM b.total
    ) THEN
        RAISE EXCEPTION '06_mixed FAILED: test 5 (correlated subquery) results differ';
    END IF;
END $$;

\echo 'PASS: 06_mixed_queries'

DROP TABLE IF EXISTS _mq_orders, _mq_customers,
    _mq1_off, _mq1_on, _mq2_off, _mq2_on, _mq3_off, _mq3_on,
    _mq4_off, _mq4_on, _mq5_off, _mq5_on;

COMMIT;
