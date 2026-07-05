-- Expression evaluator: NULL handling — IS NULL, IS NOT NULL, COALESCE.

\echo '=== 33_expr_null ==='

CREATE TEMPORARY TABLE expr_null_test (
    id serial PRIMARY KEY,
    a int4,
    b int4,
    c float8
);

INSERT INTO expr_null_test (a, b, c)
SELECT
    CASE WHEN random() < 0.3 THEN NULL ELSE (random() * 100)::int4 END,
    CASE WHEN random() < 0.3 THEN NULL ELSE (random() * 100)::int4 END,
    CASE WHEN random() < 0.3 THEN NULL ELSE random() * 100.0 END
FROM generate_series(1, 10000);

SET pg_accel.enabled = off;
CREATE TEMPORARY TABLE null_baseline AS
SELECT id, COALESCE(a, b, 0) AS coal, a IS NULL AS a_null, c IS NOT NULL AS c_nn
FROM expr_null_test
WHERE a IS NULL OR b IS NOT NULL;

SET pg_accel.enabled = on;
CREATE TEMPORARY TABLE null_gpu AS
SELECT id, COALESCE(a, b, 0) AS coal, a IS NULL AS a_null, c IS NOT NULL AS c_nn
FROM expr_null_test
WHERE a IS NULL OR b IS NOT NULL;

DO $$
DECLARE
    diff_count int;
BEGIN
    SELECT count(*) INTO diff_count
    FROM null_baseline bl
    FULL OUTER JOIN null_gpu gp USING (id)
    WHERE bl.id IS NULL OR gp.id IS NULL
       OR bl.coal IS DISTINCT FROM gp.coal
       OR bl.a_null IS DISTINCT FROM gp.a_null;

    IF diff_count > 0 THEN
        RAISE EXCEPTION '33_expr_null FAILED: % rows differ', diff_count;
    END IF;
END $$;

\echo '33_expr_null PASSED'

DROP TABLE IF EXISTS expr_null_test, null_baseline, null_gpu;
