-- Exact decimal residency is an input capability, not yet aggregate admission.
\echo '=== 116_numeric_residency ==='
SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
CREATE TEMP TABLE _numeric_resident(v numeric, g int4);
INSERT INTO _numeric_resident
SELECT CASE WHEN i%17=0 THEN NULL ELSE (i*100003)::numeric/power(10::numeric,i%31) END,
       i%11 FROM generate_series(1,10017) g(i);
INSERT INTO _numeric_resident VALUES ('NaN',0),('Infinity',1),('-Infinity',2),(0.0000,3);
ANALYZE _numeric_resident;
DO $$
DECLARE
    has_gpu bool;
    phase int;
    expected_bytes bigint;
    actual_bytes bigint;
    kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 1..4 LOOP
        IF phase=2 THEN
            UPDATE _numeric_resident SET v=-v*100000.00001 WHERE v<1;
            BEGIN
                UPDATE _numeric_resident SET v='NaN';
                RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase=3 THEN UPDATE _numeric_resident SET v=NULL;
        ELSIF phase=4 THEN DELETE FROM _numeric_resident;
        END IF;
        IF has_gpu THEN
            PERFORM pg_accel_pin('_numeric_resident'::regclass, ARRAY['v']);
            SELECT count(*)*16+coalesce(sum(octet_length(numeric_send(v))-8),0)
                   + CASE WHEN count(*)<>count(v) THEN count(*) ELSE 0 END
              INTO STRICT expected_bytes FROM _numeric_resident;
            SELECT raw_bytes INTO STRICT actual_bytes FROM pg_accel_resident_status()
              WHERE relid='_numeric_resident'::regclass;
            IF expected_bytes<>actual_bytes THEN
                RAISE EXCEPTION '116 NUMERIC byte charge: phase % expected % got %',phase,expected_bytes,actual_bytes;
            END IF;
        END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        CREATE TEMP TABLE _numeric_expected AS SELECT g,sum(v) s,avg(v) a,count(v) n FROM _numeric_resident GROUP BY g;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        CREATE TEMP TABLE _numeric_actual AS SELECT g,sum(v) s,avg(v) a,count(v) n FROM _numeric_resident GROUP BY g;
        IF EXISTS ((SELECT * FROM _numeric_actual EXCEPT ALL SELECT * FROM _numeric_expected)
                   UNION ALL (SELECT * FROM _numeric_expected EXCEPT ALL SELECT * FROM _numeric_actual)) THEN
            RAISE EXCEPTION '116 NUMERIC result mismatch: phase %',phase;
        END IF;
        IF pg_accel_kernel_executions()<>kernels THEN
            RAISE EXCEPTION '116 NUMERIC aggregate dispatched before exact kernel qualification';
        END IF;
        DROP TABLE _numeric_expected,_numeric_actual;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:116_numeric_residency.assert_001'
SELECT pg_accel_unpin('_numeric_resident'::regclass);
SELECT pg_accel_evict('_numeric_resident'::regclass);
DROP TABLE _numeric_resident;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:116_numeric_residency'
