-- Expression evaluator: SQL three-valued boolean logic.

\echo '=== 32_expr_boolean ==='

CREATE TEMPORARY TABLE expr_bool_test (
    id serial PRIMARY KEY,
    a int4,
    b int4,
    c boolean,
    d boolean
);

INSERT INTO expr_bool_test (a, b, c, d)
SELECT
    (random() * 100)::int4,
    (random() * 100)::int4,
    CASE WHEN random() < 0.33 THEN NULL
         WHEN random() < 0.5 THEN true ELSE false END,
    CASE WHEN random() < 0.33 THEN NULL
         WHEN random() < 0.5 THEN true ELSE false END
FROM generate_series(1, 10000);

-- Test three-valued logic: NULL AND FALSE = FALSE, NULL OR TRUE = TRUE
SET pg_accel.enabled = off;
CREATE TEMPORARY TABLE bool_baseline AS
SELECT id FROM expr_bool_test
WHERE (a > 50 AND b < 80) OR (c AND NOT d) OR (c IS NULL AND a > 90);

SET pg_accel.enabled = on;
CREATE TEMPORARY TABLE bool_gpu AS
SELECT id FROM expr_bool_test
WHERE (a > 50 AND b < 80) OR (c AND NOT d) OR (c IS NULL AND a > 90);

DO $$
DECLARE
    bl int; gp int;
BEGIN
    SELECT count(*) INTO bl FROM bool_baseline;
    SELECT count(*) INTO gp FROM bool_gpu;
    IF bl <> gp THEN
        RAISE EXCEPTION '32_expr_boolean FAILED: baseline=% gpu=%', bl, gp;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:32_expr_boolean.assert_001'



DROP TABLE IF EXISTS expr_bool_test, bool_baseline, bool_gpu;

\echo 'PGACCEL_FILE_OK:32_expr_boolean'
