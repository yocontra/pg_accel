-- Expression evaluator: CASE WHEN expressions.

\echo '=== 34_expr_case ==='

CREATE TEMPORARY TABLE expr_case_test (
    id serial PRIMARY KEY,
    val int4,
    cat text
);

INSERT INTO expr_case_test (val, cat)
SELECT
    (random() * 200)::int4 - 100,
    CASE WHEN random() < 0.25 THEN 'a'
         WHEN random() < 0.5 THEN 'b'
         WHEN random() < 0.75 THEN 'c'
         ELSE 'd' END
FROM generate_series(1, 10000);

SET pg_accel.enabled = off;
CREATE TEMPORARY TABLE case_baseline AS
SELECT id,
    CASE WHEN val > 50 THEN 'high'
         WHEN val > 0 THEN 'mid'
         WHEN val > -50 THEN 'low'
         ELSE 'very_low' END AS bucket
FROM expr_case_test;

SET pg_accel.enabled = on;
CREATE TEMPORARY TABLE case_gpu AS
SELECT id,
    CASE WHEN val > 50 THEN 'high'
         WHEN val > 0 THEN 'mid'
         WHEN val > -50 THEN 'low'
         ELSE 'very_low' END AS bucket
FROM expr_case_test;

DO $$
DECLARE
    diff_count int;
BEGIN
    SELECT count(*) INTO diff_count
    FROM case_baseline bl
    FULL OUTER JOIN case_gpu gp USING (id)
    WHERE bl.id IS NULL OR gp.id IS NULL
       OR bl.bucket IS DISTINCT FROM gp.bucket;

    IF diff_count > 0 THEN
        RAISE EXCEPTION '34_expr_case FAILED: % rows differ', diff_count;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:34_expr_case.assert_001'



DROP TABLE IF EXISTS expr_case_test, case_baseline, case_gpu;

\echo 'PGACCEL_FILE_OK:34_expr_case'
