# Benchmark Failure Ledger

Release-blocking gate failures and every measured row still below PostgreSQL-parallel parity. Rows are merged by workload/scale so one benchmark cell has one owner even when it trips multiple checks.

| Total rows | Ship-gate failures | H3 gate failures | Crashes | Rows below parity | GPU-dispatched below parity | Native below parity | Planner-declined below parity | Stock fallback rows | Custom Scan no-dispatch | Work items |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 2 | 0 | 0 | 0 | 1 | 0 | 1 | 1 | 0 | 0 | 1 |

| Priority | Track | Workload | Scale | Sources | Classification | Speedup | Gate | Accel ms | PG ms | Crash artifacts | Detail |
|---|---|---|---:|---|---|---:|---:|---:|---:|---|---|
| p3_parity_noise | resident_groupagg_small_input_overhead | bigint_filtered_mixed | 1M | native_below_parity | planner_declined_native | 0.99x | - | 26.81 | 26.41 | - | native/planner-declined row below 1.00x parity |
