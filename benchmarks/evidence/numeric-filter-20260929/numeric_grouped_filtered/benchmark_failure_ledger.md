# Benchmark Failure Ledger

Release-blocking gate failures and every measured row still below PostgreSQL-parallel parity. Rows are merged by workload/scale so one benchmark cell has one owner even when it trips multiple checks.

| Total rows | Ship-gate failures | H3 gate failures | Crashes | Rows below parity | GPU-dispatched below parity | Native below parity | Planner-declined below parity | Stock fallback rows | Custom Scan no-dispatch | Work items |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 2 | 0 | 0 | 0 | 2 | 0 | 2 | 2 | 0 | 0 | 2 |

| Priority | Track | Workload | Scale | Sources | Classification | Speedup | Gate | Accel ms | PG ms | Crash artifacts | Detail |
|---|---|---|---:|---|---|---:|---:|---:|---:|---|---|
| p2_architecture_gap | resident_groupagg_small_input_overhead | numeric_grouped_filtered | 250K | native_below_parity | planner_declined_native | 0.92x | - | 15.50 | 14.33 | - | native/planner-declined row below 0.98x parity |
| p2_architecture_gap | resident_groupagg_small_input_overhead | numeric_grouped_filtered | 1M | native_below_parity | planner_declined_native | 0.98x | - | 27.99 | 27.29 | - | native/planner-declined row below 0.98x parity |
