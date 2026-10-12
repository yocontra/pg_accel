-- Inline, compressed and external text share one exact sorted dictionary
-- representation. Loading must not retain borrowed tuple/detoast storage.
\echo '=== 109_text_residency_interning ==='
SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
-- PG18 on the qualification host also exhausts local buffers with pg_accel
-- disabled when this TOAST fixture runs at effective_io_concurrency=256.
-- Keep the data/buffer budget unchanged and disable read-ahead for BOTH paths.
-- Other SQL files retain the host's default I/O settings.
SET effective_io_concurrency = 0;
CREATE TEMP TABLE _text_intern (
    id int, ktext text COLLATE "C", kcomp text COLLATE "C", kext text COLLATE "C",
    kv varchar COLLATE "C", kc char(8) COLLATE "C", amount int4
);
ALTER TABLE _text_intern ALTER COLUMN kcomp SET STORAGE MAIN;
ALTER TABLE _text_intern ALTER COLUMN kcomp SET COMPRESSION pglz;
ALTER TABLE _text_intern ALTER COLUMN kext SET STORAGE EXTERNAL;
INSERT INTO _text_intern
SELECT i,
    CASE WHEN i % 19 = 0 THEN NULL ELSE
        (ARRAY['', 'é', U&'e\0301', '東京', 'a ', 'a', '😀'])[1 + i % 7] END,
    CASE WHEN i % 19 = 0 THEN NULL WHEN i % 5000 = 0 THEN repeat('compressed-', 3000)
         ELSE 'small-' || i % 5 END,
    CASE WHEN i % 19 = 0 THEN NULL WHEN i % 5000 = 0 THEN
        (SELECT string_agg(md5(j::text), '' ORDER BY j) FROM generate_series(1, 400) AS h(j))
         ELSE 'external-' || i % 5 END,
    CASE WHEN i % 19 = 0 THEN NULL ELSE (ARRAY['', 'x', 'x ', '東京'])[1 + i % 4] END,
    CASE WHEN i % 19 = 0 THEN NULL ELSE (ARRAY['', 'x', 'x ', '東京'])[1 + i % 4] END,
    CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL ELSE i % 10001 - 5000 END
FROM generate_series(1, 300000) AS g(i);
ANALYZE _text_intern;
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM _text_intern
                   WHERE octet_length(kcomp) > 30000 AND pg_column_size(kcomp) < 1000)
       OR NOT EXISTS (SELECT 1 FROM _text_intern WHERE octet_length(kext) > 12000)
       OR NOT EXISTS (SELECT 1 FROM pg_class
                      WHERE oid = '_text_intern'::regclass AND reltoastrelid <> 0
                        AND pg_relation_size(reltoastrelid) > 0) THEN
        RAISE EXCEPTION '109 missing compressed/external text fixtures';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:109_text_residency_interning.assert_001'

DO $$
DECLARE
    has_gpu bool;
    col text;
    query text;
    plan text;
    line record;
    phase int;
    kernels_before bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_text_intern'::regclass, ARRAY['ktext','kcomp','kext','kv','kc','amount']);
    END IF;
    FOR phase IN 1..2 LOOP
        IF phase = 2 THEN
            UPDATE _text_intern SET ktext = 'new-label', kext = repeat('new-external-', 1000)
            WHERE id % 5000 = 0;
            BEGIN
                UPDATE _text_intern SET kcomp = 'rolled-back-label' WHERE id % 11 = 0;
                RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'rollback fixture';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        END IF;
        FOREACH col IN ARRAY ARRAY['ktext','kcomp','kext','kv','kc'] LOOP
            query := format('SELECT %I AS k, sum(amount) AS s, avg(amount) AS a
                             FROM _text_intern GROUP BY %I', col, col);
            PERFORM set_config('pg_accel.enabled', 'off', false);
            EXECUTE 'CREATE TEMP TABLE _text_expected AS ' || query;
            PERFORM set_config('pg_accel.enabled', 'on', false);
            kernels_before := pg_accel_kernel_executions();
            EXECUTE 'CREATE TEMP TABLE _text_actual AS ' || query;
            IF EXISTS ((SELECT * FROM _text_actual EXCEPT ALL SELECT * FROM _text_expected)
                       UNION ALL
                       (SELECT * FROM _text_expected EXCEPT ALL SELECT * FROM _text_actual)) THEN
                RAISE EXCEPTION '109 text dictionary result mismatch: % / phase %', col, phase;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) ' || query LOOP
                plan := plan || line."QUERY PLAN" || E'\n';
            END LOOP;
            IF has_gpu THEN
                IF position('GPU Kernel Dispatched: true' IN plan) = 0
                   OR pg_accel_kernel_executions() <= kernels_before THEN
                    RAISE EXCEPTION '109 text dictionary query did not dispatch: % / %', col, plan;
                END IF;
            ELSIF pg_accel_kernel_executions() <> kernels_before THEN
                RAISE EXCEPTION '109 dispatched without GPU';
            END IF;
            DROP TABLE _text_actual, _text_expected;
        END LOOP;
    END LOOP;
    IF (SELECT stock_exec_count FROM pg_accel_stats()) <> 0 THEN
        RAISE EXCEPTION '109 unexpected stock aggregate fallback';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:109_text_residency_interning.assert_002'
SELECT pg_accel_unpin('_text_intern'::regclass);
SELECT pg_accel_evict('_text_intern'::regclass);
DROP TABLE _text_intern;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
RESET effective_io_concurrency;
\echo 'PGACCEL_FILE_OK:109_text_residency_interning'
