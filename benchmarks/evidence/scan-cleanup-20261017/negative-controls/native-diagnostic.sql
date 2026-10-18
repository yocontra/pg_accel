\set ON_ERROR_STOP on
CREATE FUNCTION tests.test_native_caught_error_snapshot_diagnostic() RETURNS void
AS 'pg_accel', 'test_native_caught_error_snapshot_diagnostic_wrapper' LANGUAGE c;
CREATE FUNCTION tests.test_native_sql_exception_snapshot_diagnostic() RETURNS void
AS 'pg_accel', 'test_native_sql_exception_snapshot_diagnostic_wrapper' LANGUAGE c;
BEGIN;
\echo NATIVE_RUST_CATCH_BEGIN
SELECT tests.test_native_caught_error_snapshot_diagnostic();
\echo NATIVE_RUST_CATCH_END
\echo NATIVE_SQL_EXCEPTION_BEGIN
SELECT tests.test_native_sql_exception_snapshot_diagnostic();
\echo NATIVE_SQL_EXCEPTION_END
ROLLBACK;
