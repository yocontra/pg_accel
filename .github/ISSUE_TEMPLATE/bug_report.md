---
name: Bug Report
about: Report a bug in pg_accel
title: ''
labels: bug
assignees: ''
---

**Describe the bug**
A clear description of what the bug is.

**To Reproduce**
SQL or steps to reproduce:
```sql
-- Your query here
```

**Expected behavior**
What you expected to happen.

**Actual behavior**
What actually happened. Include error messages if any.

**EXPLAIN output**
If applicable, paste `EXPLAIN (ANALYZE, VERBOSE)` output:
```
-- paste here
```

**Environment**
- PostgreSQL version:
- pg_accel version:
- PostGIS version (if applicable):
- H3 version (if applicable):
- OS:
- AdaptiveCpp backend/device as reported by `pg_accel_device_info()`:

**Plan, device, residency, and counters**
```sql
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

Run these in the same backend/session as the reproduction where possible.
Do not enable a `pg_accel.test_*` GUC for a production bug report.

**Additional context**
Any other context about the problem.
