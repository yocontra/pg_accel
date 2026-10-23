\set ON_ERROR_STOP on
SELECT version();
SELECT extname,extversion FROM pg_extension WHERE extname='pg_accel';
SELECT * FROM pg_accel_device_info();
SELECT * FROM pg_accel_device_limits();
SELECT name,setting,unit,source,boot_val FROM pg_settings WHERE name LIKE 'pg_accel.%' ORDER BY name;
DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='tests') THEN
    RAISE EXCEPTION 'requires non-pg_test production module';
  END IF;
  IF (SELECT gpu_available AND gpu_device_name='Apple M2 Max' FROM pg_accel_device_info()) IS DISTINCT FROM true THEN
    RAISE EXCEPTION 'requires actual Apple M2 Max GPU';
  END IF;
  IF EXISTS (SELECT 1 FROM pg_accel_device_limits() WHERE source <> 'hardware_derived') THEN
    RAISE EXCEPTION 'requires hardware-derived device limits';
  END IF;
  IF EXISTS (SELECT 1 FROM pg_settings WHERE name IN ('pg_accel.cost_multiplier','pg_accel.min_batch_size')
      AND (setting <> boot_val OR source <> 'default')) OR
      (SELECT count(*) FROM pg_settings WHERE name IN ('pg_accel.cost_multiplier','pg_accel.min_batch_size')) <> 2 THEN
    RAISE EXCEPTION 'requires default admission cost and minimum batch size';
  END IF;
END $$;
