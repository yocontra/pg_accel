# Checked computed integer aggregates — local characterization

This change extends the resident integer lane from direct-column measures and
the narrow product-SUM shape to two direct INT4 operands using `+`, `-` and `*`
with SUM, AVG, COUNT, MIN and MAX. Up to four states, zero through three
dictionary keys, independent aggregate FILTER, supported WHERE predicates,
spatial masks, and native HAVING/order/limits compose within the existing gates.

Each expression is evaluated at PostgreSQL INT4 width before accumulation.
Overflow raises SQLSTATE 22003 even for COUNT-only states. Either NULL operand
makes the expression NULL. WHERE and aggregate FILTER reject rows before the
arithmetic runs. Failed device execution cannot publish a partial result or
silently execute a stock fallback.

Independent arithmetic predicates cannot overwrite the right operand. The
planner uses an existing raw left operand or column state, or adds a hidden
raw-column carrier within the four-state limit. AQS6 adds the ADD wire tag and
rejects older specs; native ADD is opcode 5 with unchanged C ABI layout.
The cost model charges expression evaluation, including COUNT(expression),
without weakening device or admission thresholds.

Nested/literal expressions, floating/decimal arithmetic, multi-column Boolean
predicates, joins and shapes exceeding the state limit remain outside this
expanded lane. This is local dirty-tree Metal evidence, not release, CUDA,
concurrency or fresh upstream PostgreSQL regression qualification.

## Verification

- 1,153 Rust core tests pass. New checks cover wire/version isolation, all
  arithmetic/aggregate combinations, predicate binding/state capacity and
  expression cost for COUNT.
- 574 benchmark tests pass, including an independent Rust i64 oracle for
  mixed product SUM, difference AVG and addition extrema.
- Native CTests pass 32/32 in 121.87 seconds; grouped tests pass 45,007 checks. The added matrix exercises
  all 15 lane combinations, three operators, global/atomic/hierarchical group
  capacities, independent operand NULLs, raw predicate carriers, WHERE,
  per-measure filters, and reset/accumulate/finalize equivalence. Error cases
  check COUNT overflow and nonpublication, rejected rows and NULL operands.
- SQL126 passes on the final PG18 build: 72 selected query shapes with a separately widened
  NUMERIC arithmetic oracle, spatial-coordinate oracle, generic prepared
  DML/rollback/NULL/empty cases, all five aggregate overflow errors, recovery,
  and unsupported-expression/state-capacity rejection.
- Both extension Clippy builds, benchmark Clippy, 154 coverage-helper tests,
  21 documentation tests and formatting pass. The scope inventory is 89 SQL
  files, 416 assertions and 37 families.

The first full PG18 run passed 88/89 files. SQL97 failed because its assertion
still required the old `dense_integer_multiply_range` specialization label.
The corrected test requires the new expression label and actual dispatch for
one-sided/RHS ranges while retaining the cross-column decline and CPU-only
guards. SQL97 and SQL126 both pass on the final installed PG18 build. The
original full-run failure is retained in `sql-pg18/` and the passing reruns in
`verification/`. The final native-source change after that full run was C++
indentation only; no functional extension changes followed it.

PG18 full-run module: `c3133a9da05de312bb25326c7b31d2490f7bd5f1b59c1d554eb2f86156ea37a6`.
Final PG18 module: `55b959c365fe0e81a412635aa768d46ff296cdf9b8d24c71d4f3978367183eea`.
Final PG19 module: `44a3b1ced72fc54130a4a463e26f1b50ec42e2e4f529f564a8a9acfe0ae0f323`.

## Nonselection evidence

The initial `grouped_computed_integer` benchmark, with three computed states
plus COUNT(*) and one dictionary key, remained PostgreSQL-native at 250K and
1M rows. Its initial expected-winner annotation was incorrect. Both failed
selection records remain in `grouped_computed_integer/`; the separate
`grouped_computed_integer-diagnostic/` retains a planner-instrumented repeat,
also native. Neither run earns GPU credit. The workload remains registered as
an unqualified characterization, not a claimed winner.

An isolated EXPLAIN-only cost probe exposes an admissible GPU estimate of
12,005.96 versus the normal parallel PostgreSQL estimate of 9,633.28 for the
1M-row grouped shape. Only that diagnostic disabled parallel workers to expose
the candidate; benchmark sessions use the normal parallel baseline. The probe
does not measure performance. The global version is a separate query shape,
not a replacement for or reinterpretation of the grouped nonselection result.

PG19's final build passes the full strict suite: 89/89 files and 416/416
semantic assertions, without warning/skip evidence. Its retained inventory is
`sql-pg19/inventory/`. All 89 PG18 files have passing executions across the full
run and corrected SQL97 rerun; this is deliberately not described as a clean
single-invocation PG18 suite.

## Warm measurements

Apple M2 Max / 64 GB / Metal / PostgreSQL 18.4, release builds, five warmups and
ten paired raw wall-clock observations per scale. No test/build jobs overlap
timing. Normal parallel PostgreSQL remains the baseline; cost coefficients and
admission thresholds are unchanged apart from the explicit additional charge
for each computed integer state. Report medians exclude preload and the
separately recorded first-use refresh/lifecycle probe.

| Workload | Rows | pg_accel ms | PostgreSQL ms | Warm ratio | pg_accel CV |
|---|---:|---:|---:|---:|---:|
| Global computed measures | 250K | 2.693 | 8.078 | 3.00x | 10.38% |
| Global computed measures | 1M | 5.941 | 18.571 | 3.13x | 21.99% |
| Global computed measures, repeat | 250K | 3.271 | 8.642 | 2.64x | 10.74% |
| Global computed measures, repeat | 1M | 5.502 | 18.535 | 3.37x | 29.50% |
| Existing range/product control | 250K | 4.021 | 9.259 | 2.30x | 23.24% |
| Existing range/product control | 1M | 4.719 | 19.500 | 4.13x | 14.19% |
| Existing direct-column SUM/AVG control | 250K | 2.478 | 10.482 | 4.23x | 23.23% |
| Existing direct-column SUM/AVG control | 1M | 5.164 | 21.791 | 4.22x | 20.49% |

All eight cells match exactly, have verified `parallel_dense_integer` dispatch
for every measured iteration and full row counters, and report zero stock
fallback. Their four failure ledgers are empty. The new workloads include a
separately derived Rust i64 arithmetic oracle; SQL126 also compares widened
NUMERIC expressions. The older controls are current-build observations, not
matched before/after evidence of unchanged latency.

The global workload's initial preload is 47.85/94.50 ms at 250K/1M. Its first-use
refresh/lifecycle probe costs 20.51/61.89 ms versus native 8.53/17.09 ms;
the repeat costs 19.72/62.50 ms versus native 9.70/17.25 ms. First use still
loses. Warm CV reaches 29.50%; these are noisy local observations, not a
publication-quality latency certification or a claim that every grouped
arithmetic query accelerates. The grouped nonselection evidence above and its
failed initial winner gates are intentionally retained.

Remaining work includes properly calibrated grouped-expression admission,
cold loading, broader expression/type support, concurrency, CUDA and release
qualification. No planning documents are removed by this bounded milestone.
