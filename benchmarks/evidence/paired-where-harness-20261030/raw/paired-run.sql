\set ON_ERROR_STOP on
CREATE EXTENSION pg_accel;
SELECT pg_catalog.to_regprocedure('paired_tests.run_paired_where_candidates()') IS NOT NULL AS paired_export_present;
SELECT name,setting,boot_val FROM pg_settings WHERE name LIKE 'pg_accel.%' ORDER BY name;
SELECT * FROM pg_accel_device_info();
SELECT * FROM pg_accel_device_limits();
SELECT paired_tests.run_paired_where_candidates() AS paired_complete;
