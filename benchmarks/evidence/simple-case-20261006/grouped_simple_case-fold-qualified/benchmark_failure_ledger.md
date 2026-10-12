# Benchmark Failure Ledger

Release-blocking gate failures and every measured row still below PostgreSQL-parallel parity. Rows are merged by workload/scale so one benchmark cell has one owner even when it trips multiple checks.

| Total rows | Ship-gate failures | H3 gate failures | Crashes | Rows below parity | GPU-dispatched below parity | Native below parity | Planner-declined below parity | Stock fallback rows | Custom Scan no-dispatch | Work items |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 3 | 0 | 0 | 0 | 3 | 0 | 3 | 3 | 0 | 0 | 3 |

| Priority | Track | Workload | Scale | Sources | Classification | Speedup | Gate | Accel ms | PG ms | Crash artifacts | Detail |
|---|---|---|---:|---|---|---:|---:|---:|---:|---|---|
| p2_architecture_gap | resident_groupagg_small_input_overhead | grouped_simple_case | 250K | native_below_parity | planner_declined_native | 0.95x | - | 21.13 | 20.03 | - | native/planner-declined row below 0.98x parity |
| p2_architecture_gap | resident_groupagg_small_input_overhead | grouped_simple_case | 1M | native_below_parity | planner_declined_native | 0.97x | - | 57.69 | 55.87 | - | native/planner-declined row below 0.98x parity |
| p2_architecture_gap | resident_groupagg_small_input_overhead | grouped_simple_case | 2M | native_below_parity | planner_declined_native | 0.96x | - | 98.08 | 94.09 | - | native/planner-declined row below 0.98x parity |
