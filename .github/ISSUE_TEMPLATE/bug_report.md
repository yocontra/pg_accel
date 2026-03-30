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
- OS:
- GPU backend (Metal/OpenCL/CUDA):

**GUC settings**
```sql
SHOW pg_accel.enabled;
SHOW pg_accel.gpu_enabled;
SHOW pg_accel.min_batch_size;
SHOW pg_accel.workers;
```

**Additional context**
Any other context about the problem.
