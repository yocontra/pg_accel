# Benchmark Failure Ledger

Release-blocking gate failures and every measured row still below PostgreSQL-parallel parity. Rows are merged by workload/scale so one benchmark cell has one owner even when it trips multiple checks.

| Total rows | Ship-gate failures | H3 gate failures | Crashes | Rows below parity | GPU-dispatched below parity | Native below parity | Planner-declined below parity | Stock fallback rows | Custom Scan no-dispatch | Work items |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 3 | 0 | 0 | 0 | 2 | 0 | 2 | 2 | 0 | 0 | 2 |

| Priority | Track | Workload | Scale | Sources | Classification | Speedup | Gate | Accel ms | PG ms | Crash artifacts | Detail |
|---|---|---|---:|---|---|---:|---:|---:|---:|---|---|
| p3_parity_noise | resident_groupagg_small_input_overhead | grouped_simple_case | 1M | native_below_parity | planner_declined_native | 0.99x | - | 79.65 | 78.51 | - | native/planner-declined row below 1.00x parity |
| p3_parity_noise | resident_groupagg_small_input_overhead | grouped_simple_case | 2M | native_below_parity | planner_declined_native | 0.99x | - | 122.05 | 120.73 | - | native/planner-declined row below 1.00x parity |
