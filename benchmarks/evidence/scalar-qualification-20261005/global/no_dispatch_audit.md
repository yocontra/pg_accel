# No-Dispatch Audit

| Evaluated no-dispatch rows | Clean | Warnings | Timing skew | Plan mismatch | Selected Custom Scan no-dispatch | Missing plan evidence | Ignored dispatching rows |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 1 | 0 | 0 | 0 | 0 | 0 | 1 |

Rows in this audit did not prove pg_accel GPU kernel dispatch. Timing gaps here are harness, planner, cache, or native plan differences until the row is re-run with matching plan evidence and dispatch counters.

| Workload | Scale | Median speedup | Timing skew | Plan shape | Missing plan evidence | Accel plan | PG plan | Status | Action |
|---|---:|---:|---|---|---|---|---|---|---|
| global_scalar_program | 1M | 1.00x | 0.3% | same | false | `finalize aggregate` | `finalize aggregate` | comparable_native | no GPU credit; use only as native-decline/stability evidence |
