# TPC-H-derived query corpus, revision 2

The `tpch` category contains all 22 query families. These are deterministic
PostgreSQL-generated workloads, **not DBGEN data, a TPC scale factor, an audited
TPC-H benchmark, or a QphH result**. The query-family reference is the
[TPC-H specification](https://www.tpc.org/TPC_Documents_Current_Versions/pdf/tpc-h_v2.18.0.pdf).
The synthetic three-query revision is superseded; its timings are not directly
comparable to this fixture.

`fixture.sql` defines all eight tables, primary/foreign keys and ordinary native
indexes. `rows` means the exact number of lineitems. Orders are `ceil(rows/4)`
(minimum one), customers are `ceil(orders/10)` (minimum 100), parts are
`ceil(rows/20)` (minimum 100), and suppliers are `ceil(rows/1000)` (minimum 25).
Every part has four distinct supplier offers. There are 25 nations and five
regions. All numeric generators use int8 before multiplication and exact
PostgreSQL NUMERIC division. Dimensions are intentionally synthetic and not
official scale-factor ratios or distributions.

The data includes customers with no orders, excluded order/supplier comments,
multiple suppliers per order, late and on-time receipts, varied quantities for
the same part, and high-quantity orders. Customer nations are decorrelated from
supplier-key arithmetic. These properties prevent trivially empty answers for
Q5, Q17, Q18, Q21 and Q22 at the 100K characterization scale. Smaller scales
may legitimately have empty groups or NULL scalar sums.

Queries use fixed parameters, explicit joins, a shared CTE instead of persistent
view DDL for Q15, and deterministic tie breakers for limited output. Each query
is a single embeddable statement with no trailing semicolon. No query is removed
because it stays native or loses. The system-workload gate requires exactly
`tpch_q1` through `tpch_q22`, while ordinary category/individual CLI filters
remain available for diagnostics. Every referenced column is declared in the
warm-residency pin registry; preload/first-use accounting stays in the existing
harness. This is not evidence that those shapes are GPU-supported.

The fixture oracle independently checks the eight table cardinalities; it is
**not an independent oracle for the query values**. The standard harness still
compares complete native and pg_accel query outputs. The opt-in integration test
builds a fresh schema inside a transaction, runs every query with pg_accel off,
requires nonempty/non-NULL results at 100K, then rolls everything back:

```sh
PG_ACCEL_TEST_PG_MAJOR=18 cargo test -p pg_accel_bench --features integration_tests \
  full_tpch_corpus_executes_with_nonempty_results_in_rollback_only_schema
PG_ACCEL_TEST_PG_MAJOR=19 cargo test -p pg_accel_bench --features integration_tests \
  full_tpch_corpus_executes_with_nonempty_results_in_rollback_only_schema
```

`SHA256SUMS` pins the fixture and all 22 SQL files. From the repository root:

```sh
shasum -a 256 -c pg_accel_bench/src/workloads/tpch/SHA256SUMS
```

Initial validation and explicit remaining PostgreSQL/performance gaps are in
`benchmarks/evidence/tpch-full-20261010/README.md`. This corpus alone does not
satisfy the TODO's half-accelerated or 2x full-corpus targets, nor its concurrency,
spill, ClickBench, NYC or Landsat requirements.
