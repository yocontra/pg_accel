# Full TPC-H-derived corpus implementation

2026-10-10. The harness now implements all 22 TPC-H-derived query families,
replacing the former Q1/Q6/Q12-only fixture. This is implementation evidence,
not PostgreSQL/GPU or performance qualification. The corresponding TODO
qualification item deliberately remains unchecked.

## Changes

- Eight deterministic relational tables, including primary and foreign keys,
  exact NUMERIC columns, ordinary native indexes and statistics.
- All 22 query templates, including correlated aggregate subqueries,
  EXISTS/NOT EXISTS, LEFT JOIN, DISTINCT, HAVING, CASE, multiway joins, text
  matching, date extraction and a shared-revenue CTE.
- The broad system gate requires exactly `tpch_q1` through `tpch_q22`;
  it cannot silently accept the former three-query subset. No native or slow
  family is removed. The existing characterization gate suppresses aggregate
  speedup headlines.
- Per-query pin declarations include every referenced input column. A template
  inventory test checks that none is omitted. Fixture generation, all 22 names,
  embeddable queries and cleanup are also covered by hermetic tests.
- A live PG integration test creates an isolated schema inside a transaction,
  verifies all eight table counts and nonempty/non-NULL answers at 100K, then
  rolls back. The ordinary harness retains full native/pg_accel output diffs.
  The fixture cardinality oracle does not independently prove query values.

The fixed schema and SQL are pinned in
`pg_accel_bench/src/workloads/tpch/SHA256SUMS`; its README describes ratios,
parameters and non-certification boundaries. The fixture is not DBGEN data and
the row count is not a TPC scale factor. Old subset timings are not comparable.
No production planner, executor, kernel or ABI was changed in this milestone.

## Local checks and limitations

All 589 benchmark unit tests pass, as do strict integration-feature Clippy,
formatting, 21 documentation tests and coverage-scope checks. Results are
retained in the corresponding logs. Scope stays
229 Rust files, 18 C++ sources, 35 CTests, 100 SQL files, 462 semantic assertions
and 43 semantic families with the unchanged 90% threshold. Benchmark query SQL
is not relabeled as a production SQL semantic-assertion result.

Database connection probes fail because this session cannot open local sockets.
An attempted isolated PG18 initialization also fails: `shmget` is denied during
bootstrap. `initdb18.log` retains this failed attempt. No existing database,
installed extension or running server was modified. The live PG18/19 test
attempts are retained separately and must not be counted as passes.

As a secondary diagnostic, installed DuckDB 1.5.6 executed the fixture and all
22 queries at 100K rows. Only integer division in fixture generation was
translated from `/` to `//`; the query templates were unchanged. This checks
basic query/fixture viability, **not PostgreSQL semantics or exact NUMERIC
arithmetic**: DuckDB's decimal division differs. No timings or speedups from
that engine are claimed.

The first diagnostic returned no rows for Q5 and Q18. Customer-nation arithmetic
was correlated with supplier keys, while quantities across four-row orders
never reached the large-order threshold. The fixture now decorrelates customer
nations and varies quantities by order and long-period row quotient; quantities
for a given part are not constant. Both initial and final diagnostic SQL/logs
are retained. Every final query has nonempty/non-NULL output; row counts in
Q1..Q22 order are:

```text
6, 33, 10, 5, 5, 1, 4, 2, 175, 20, 756, 2, 3, 1, 1, 11, 1, 100, 1, 1, 1, 7
```

These are diagnostic observations, not PostgreSQL golden answers. Full PG18/19
execution, exact output parity, normal plan/dispatch classification, first-use
and warm timings, 50% query acceleration, 2x full-corpus geometric mean,
concurrency and spill remain unverified. ClickBench, NYC and Landsat work is
not closed by this change. The no-GPU and seven release-audit findings remain.

## Reproduction

```sh
cargo test -p pg_accel_bench
cargo clippy -p pg_accel_bench --all-targets --features integration_tests -- -D warnings
PG_ACCEL_TEST_PG_MAJOR=18 cargo test -p pg_accel_bench --features integration_tests \
  full_tpch_corpus_executes_with_nonempty_results_in_rollback_only_schema
PG_ACCEL_TEST_PG_MAJOR=19 cargo test -p pg_accel_bench --features integration_tests \
  full_tpch_corpus_executes_with_nonempty_results_in_rollback_only_schema
duckdb -json :memory: < benchmarks/evidence/tpch-full-20261010/duckdb-smoke.sql
```

`source-snapshot.tar.gz` captures the complete dirty source state, including
the query assets. `SHA256SUMS` binds this bundle. Git metadata remains read-only,
so this milestone is local only: no commit or push was possible.
