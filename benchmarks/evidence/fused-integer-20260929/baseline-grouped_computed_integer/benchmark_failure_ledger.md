# Benchmark Failure Ledger

Release-blocking gate failures and every measured row still below PostgreSQL-parallel parity. Rows are merged by workload/scale so one benchmark cell has one owner even when it trips multiple checks.

| Total rows | Ship-gate failures | H3 gate failures | Crashes | Rows below parity | GPU-dispatched below parity | Native below parity | Planner-declined below parity | Stock fallback rows | Custom Scan no-dispatch | Work items |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 2 | 2 | 0 | 0 | 2 | 0 | 2 | 2 | 0 | 0 | 2 |

| Priority | Track | Workload | Scale | Sources | Classification | Speedup | Gate | Accel ms | PG ms | Crash artifacts | Detail |
|---|---|---|---:|---|---|---:|---:|---:|---:|---|---|
| p1_release_blocker | resident_groupagg_small_input_overhead | grouped_computed_integer | 250K | benchmark_ship_gate, native_below_parity | planner_declined_native | 0.98x | 1.00x | 11.17 | 10.95 | - | expected_winner_missed_selection: expected GPU winner missed dispatch/selection in lane `resident_computed_integer_aggregate` (computed-measure resident integer aggregation warm gate)<br>native/planner-declined row below 1.00x parity |
| p1_release_blocker | resident_groupagg_small_input_overhead | grouped_computed_integer | 1M | benchmark_ship_gate, native_below_parity | planner_declined_native | 0.99x | 1.00x | 24.28 | 24.06 | - | expected_winner_missed_selection: expected GPU winner missed dispatch/selection in lane `resident_computed_integer_aggregate` (computed-measure resident integer aggregation warm gate)<br>native/planner-declined row below 1.00x parity |
