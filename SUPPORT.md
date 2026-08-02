# Support

pg_accel has not made a public release. Support is currently best effort on
the repository's active development branch; there is no compatibility or
response-time guarantee.

## Before opening an issue

Confirm that the behavior is inside the production planner surface documented
in the [capability matrix](README.md#capability-matrix). Kernel, adapter, or ABI
presence does not mean a SQL shape is production-selectable. The validated GPU
development target is Metal on Apple Silicon. Linux without a GPU is a
build/load and native-decline target. CUDA/NVIDIA is not currently validated.

Search existing [issues](https://github.com/yocontra/pg_accel/issues), then use
the bug-report template for reproducible failures or the feature-request
template for unsupported shapes. Questions about builds, documented behavior,
or prospective support may use a regular issue.

## Failure reports

Run diagnostics in the same PostgreSQL backend that reproduced the failure and
attach the smallest SQL example, complete error text, and relevant server-log
excerpt. Remove credentials and production data.

```sql
SELECT version();
SELECT * FROM pg_accel_device_info();
SELECT * FROM pg_accel_device_limits() ORDER BY name;
SELECT * FROM pg_accel_resident_status();
SELECT pg_accel_resident_live_bytes();
SELECT * FROM pg_accel_stats();
SELECT * FROM pg_accel_gpu_failures();
SELECT pg_accel_last_planner_rejection_reason();

SELECT name, setting, unit, context, source
FROM pg_settings
WHERE name LIKE 'pg_accel.%'
  AND name NOT LIKE 'pg_accel.test_%'
ORDER BY name;
```

Also include:

- the pg_accel commit or release tag and installation method;
- operating system, CPU/device model, PostgreSQL major, and relevant PostGIS or
  H3 versions;
- `EXPLAIN (ANALYZE, VERBOSE, COSTS OFF)` when the query completes safely;
- whether the failure persists after a PostgreSQL restart and with a minimal
  fresh table;
- crash reports or backend-disconnect logs when applicable.

Do not reproduce a destructive query against important data solely to collect
diagnostics. Do not enable `pg_accel.test_*` GUCs for a production report.

## Security reports

Potential vulnerabilities follow [SECURITY.md](SECURITY.md). Do not disclose
them in a public issue, discussion, or pull request.
