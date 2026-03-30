-- Expression evaluator: comparison operators + NaN handling.

\echo '=== 31_expr_comparison ==='

CREATE TEMPORARY TABLE expr_cmp_test (
    id serial PRIMARY KEY,
    a float8,
    b float8,
    c int4
);

INSERT INTO expr_cmp_test (a, b, c)
SELECT
    CASE WHEN random() < 0.05 THEN 'NaN'::float8
         ELSE random() * 1000.0 - 500.0 END,
    CASE WHEN random() < 0.05 THEN 'NaN'::float8
         ELSE random() * 1000.0 - 500.0 END,
    (random() * 200)::int4 - 100
FROM generate_series(1, 10000);

-- NaN = NaN should be TRUE in PG
SET pg_accel.enabled = off;
CREATE TEMPORARY TABLE cmp_baseline AS
SELECT id FROM expr_cmp_test
WHERE a = a OR a > b OR c >= 50 OR a <> b;

SET pg_accel.enabled = on;
CREATE TEMPORARY TABLE cmp_gpu AS
SELECT id FROM expr_cmp_test
WHERE a = a OR a > b OR c >= 50 OR a <> b;

DO $$
DECLARE
    bl int; gp int;
BEGIN
    SELECT count(*) INTO bl FROM cmp_baseline;
    SELECT count(*) INTO gp FROM cmp_gpu;
    IF bl <> gp THEN
        RAISE EXCEPTION '31_expr_comparison FAILED: baseline=% gpu=%', bl, gp;
    END IF;
END $$;

\echo '31_expr_comparison PASSED'

DROP TABLE IF EXISTS expr_cmp_test, cmp_baseline, cmp_gpu;
