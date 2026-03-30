-- 22_regression_oltp.sql: OLTP patterns that pg_accel must NOT regress or intercept
-- Verifies point lookups, simple scans, DML, and trivial queries are unaffected.

\echo '=== 22_regression_oltp ==='

BEGIN;

CREATE TEMP TABLE _ro_users (
    id serial PRIMARY KEY,
    email text NOT NULL UNIQUE,
    name text NOT NULL,
    score integer NOT NULL,
    created_at timestamp NOT NULL DEFAULT now()
);

INSERT INTO _ro_users (email, name, score)
SELECT
    'user' || i || '@example.com',
    'User ' || i,
    (random() * 100)::integer
FROM generate_series(1, 1000) AS s(i);

CREATE INDEX _ro_users_email ON _ro_users(email);
CREATE INDEX _ro_users_score ON _ro_users(score);

ANALYZE _ro_users;

-- ========== Test 1: OLTP point lookup by PK (must not regress) ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ro1_off AS
SELECT id, email, name, score FROM _ro_users WHERE id = 42;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ro1_on AS
SELECT id, email, name, score FROM _ro_users WHERE id = 42;

-- ========== Test 2: Index scan on email (must not intercept) ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ro2_off AS
SELECT id, name FROM _ro_users WHERE email = 'user500@example.com';

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ro2_on AS
SELECT id, name FROM _ro_users WHERE email = 'user500@example.com';

-- ========== Test 3: Simple integer comparison (no accelerable functions) ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ro3_off AS
SELECT id, score FROM _ro_users WHERE score > 80 ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ro3_on AS
SELECT id, score FROM _ro_users WHERE score > 80 ORDER BY id;

-- ========== Test 4: Small table full scan (below min_batch_size) ==========
CREATE TEMP TABLE _ro_tiny (id int, val text);
INSERT INTO _ro_tiny VALUES (1, 'a'), (2, 'b'), (3, 'c');
ANALYZE _ro_tiny;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ro4_off AS
SELECT * FROM _ro_tiny ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ro4_on AS
SELECT * FROM _ro_tiny ORDER BY id;

-- ========== Test 5: INSERT must not interfere ==========
SET pg_accel.enabled = on;
CREATE TEMP TABLE _ro_insert_test (id serial PRIMARY KEY, val integer);
INSERT INTO _ro_insert_test (val) SELECT i FROM generate_series(1, 100) AS s(i);

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ro5_off AS
SELECT count(*) AS cnt, sum(val) AS total FROM _ro_insert_test;

-- Re-verify with accel on
SET pg_accel.enabled = on;
CREATE TEMP TABLE _ro5_on AS
SELECT count(*) AS cnt, sum(val) AS total FROM _ro_insert_test;

-- ========== Test 6: UPDATE must not interfere ==========
SET pg_accel.enabled = on;
UPDATE _ro_insert_test SET val = val * 2 WHERE val <= 50;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ro6_off AS
SELECT count(*) AS cnt, sum(val) AS total FROM _ro_insert_test;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ro6_on AS
SELECT count(*) AS cnt, sum(val) AS total FROM _ro_insert_test;

-- ========== Test 7: DELETE must not interfere ==========
SET pg_accel.enabled = on;
DELETE FROM _ro_insert_test WHERE val > 150;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ro7_off AS
SELECT count(*) AS cnt, sum(val) AS total FROM _ro_insert_test;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ro7_on AS
SELECT count(*) AS cnt, sum(val) AS total FROM _ro_insert_test;

-- ========== Comparisons ==========
DO $$ BEGIN
    -- Test 1: PK lookup
    IF EXISTS (
        SELECT 1 FROM _ro1_on a FULL OUTER JOIN _ro1_off b USING (id)
        WHERE a.email IS DISTINCT FROM b.email
           OR a.name IS DISTINCT FROM b.name
           OR a.score IS DISTINCT FROM b.score
    ) THEN
        RAISE EXCEPTION '22_regression FAILED: test 1 (PK lookup) differs';
    END IF;

    -- Test 2: email index scan
    IF EXISTS (
        SELECT 1 FROM _ro2_on a FULL OUTER JOIN _ro2_off b USING (id)
        WHERE a.name IS DISTINCT FROM b.name
    ) THEN
        RAISE EXCEPTION '22_regression FAILED: test 2 (email index scan) differs';
    END IF;

    -- Test 3: simple int comparison
    IF EXISTS (
        SELECT 1 FROM _ro3_on a FULL OUTER JOIN _ro3_off b USING (id)
        WHERE a.score IS DISTINCT FROM b.score
    ) THEN
        RAISE EXCEPTION '22_regression FAILED: test 3 (int comparison) differs';
    END IF;

    -- Test 4: tiny table
    IF EXISTS (
        SELECT 1 FROM _ro4_on a FULL OUTER JOIN _ro4_off b USING (id)
        WHERE a.val IS DISTINCT FROM b.val
    ) THEN
        RAISE EXCEPTION '22_regression FAILED: test 4 (tiny table) differs';
    END IF;

    -- Test 5: INSERT correctness
    IF EXISTS (
        SELECT 1 FROM _ro5_on a, _ro5_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR a.total IS DISTINCT FROM b.total
    ) THEN
        RAISE EXCEPTION '22_regression FAILED: test 5 (INSERT) differs';
    END IF;

    -- Test 6: UPDATE correctness
    IF EXISTS (
        SELECT 1 FROM _ro6_on a, _ro6_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR a.total IS DISTINCT FROM b.total
    ) THEN
        RAISE EXCEPTION '22_regression FAILED: test 6 (UPDATE) differs';
    END IF;

    -- Test 7: DELETE correctness
    IF EXISTS (
        SELECT 1 FROM _ro7_on a, _ro7_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR a.total IS DISTINCT FROM b.total
    ) THEN
        RAISE EXCEPTION '22_regression FAILED: test 7 (DELETE) differs';
    END IF;
END $$;

\echo 'PASS: 22_regression_oltp (7 tests)'

DROP TABLE IF EXISTS _ro_users, _ro_tiny, _ro_insert_test,
    _ro1_off, _ro1_on, _ro2_off, _ro2_on,
    _ro3_off, _ro3_on, _ro4_off, _ro4_on,
    _ro5_off, _ro5_on, _ro6_off, _ro6_on,
    _ro7_off, _ro7_on;

COMMIT;
