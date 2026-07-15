-- Expression evaluator: arithmetic operations.
-- Tests GPU expression evaluation correctness by comparing
-- pg_accel.enabled = on vs off results.

\echo '=== 30_expr_arithmetic ==='

-- Setup
CREATE TEMPORARY TABLE expr_arith_test (
    id serial PRIMARY KEY,
    a int4,
    b int4,
    c int8,
    d float8
);

INSERT INTO expr_arith_test (a, b, c, d)
SELECT
    (random() * 1000)::int4 - 500,
    (random() * 1000)::int4 - 500,
    (random() * 100000)::int8 - 50000,
    random() * 1000.0 - 500.0
FROM generate_series(1, 10000);

-- Baseline results with pg_accel off
SET pg_accel.enabled = off;

CREATE TEMPORARY TABLE arith_baseline AS
SELECT id, a + b AS add_ab, a - b AS sub_ab, a * 2 AS mul_a2,
       c + 100 AS add_c, d * 2.5 AS mul_d,
       -a AS neg_a, abs(a) AS abs_a
FROM expr_arith_test
WHERE a + b > 100;

-- GPU results
SET pg_accel.enabled = on;

CREATE TEMPORARY TABLE arith_gpu AS
SELECT id, a + b AS add_ab, a - b AS sub_ab, a * 2 AS mul_a2,
       c + 100 AS add_c, d * 2.5 AS mul_d,
       -a AS neg_a, abs(a) AS abs_a
FROM expr_arith_test
WHERE a + b > 100;

-- Compare
DO $$
DECLARE
    diff_count int;
BEGIN
    SELECT count(*) INTO diff_count
    FROM arith_baseline b
    FULL OUTER JOIN arith_gpu g USING (id)
    WHERE b.id IS NULL OR g.id IS NULL
       OR b.add_ab IS DISTINCT FROM g.add_ab
       OR b.sub_ab IS DISTINCT FROM g.sub_ab
       OR b.mul_a2 IS DISTINCT FROM g.mul_a2;

    IF diff_count > 0 THEN
        RAISE EXCEPTION '30_expr_arithmetic FAILED: % rows differ', diff_count;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:30_expr_arithmetic.assert_001'



DROP TABLE IF EXISTS expr_arith_test, arith_baseline, arith_gpu;

\echo 'PGACCEL_FILE_OK:30_expr_arithmetic'
