# Exact bigint aggregation: 2026-09-29

Bigint column SUM/AVG now uses the exact decimal GPU accumulator, without
converting resident bigint buffers into decimal buffers. SUM and AVG return
PostgreSQL NUMERIC, including sums beyond int64. The device decomposes each
64-bit magnitude into at most five base-10000 digits, including INT64_MIN.
The fixed nine-limb state occupies 40 bytes; shared counts remain independent
of digit accumulation. PostgreSQL's numeric finalization preserves AVG scale.

The family composes with zero through three nullable dictionary keys, up to
four states, mixed NUMERIC/int2/int4 aggregates, independent INT4 FILTER,
query-level INT4 ranges or Boolean masks, and qualified spatial restrictions
including spatial-plus-scalar WHERE. Native HAVING/order/limit remains above
the aggregate. No CPU aggregate fallback, host digit conversion, wire/ABI
layout change, planner forcing or cost-coefficient tuning was added. Costing
charges five digits per bigint SUM/AVG input row. Dictionary estimates and
the exact resident snapshot participate in admission as for NUMERIC inputs.

Bigint expressions, bigint extrema, decimal states mixed with extrema,
DISTINCT, high-cardinality/spilling and general joins remain unqualified.
The existing four-state budget includes hidden WHERE inputs. Bigint COUNT
alone retains the prior integer count family; in a decimal query it uses a
count-only state without reading or summing the bigint values.

## Correctness and qualification

SQL124 exercises 120 normally planned shapes over 1,000,017 rows on both
PG18 and PG19. It compares scale-sensitive JSON row text exactly with native
PostgreSQL and requires `parallel_dense_numeric` dispatch. Values include
both int64 endpoints, independent NULL patterns, sums beyond int64, independent
filters, rejected WHERE/measure rows and mixed accumulator types. Prepared
global/grouped queries cover updates, subtransaction rollback, all-NULL and
empty input, HAVING and ordering. Focused runs pass on both PostgreSQL majors.

The native test uses an independent 128-bit host oracle for positive/negative
overflow-sized sums, INT64_MIN, cancellation, trailing zero digits, all-NULL
groups, independent filters, mixed packed decimal states, global/grouped
execution, repeated single-row chunked lifecycles and malformed state widths.
Rust tests check raw-buffer binding, state widths, independent predicates,
projection result types and still-unsupported expressions/extrema.

Core tests pass 1,147; benchmark tests 572; native CTests 32/32 (78.58 seconds,
25,848 grouped-aggregate checks); coverage-tool tests 154; documentation tests
21. PG18/19 extension Clippy and benchmark Clippy pass. Coverage inventory is
87 SQL files, 410 assertions and 37 families, with unchanged 90% thresholds.
Both complete strict SQL suites pass 87/87 files:
[PG18](sql-pg18/results.tsv) and [PG19](sql-pg19/results.tsv).
Verification logs include an early focused PG19 run interrupted by a local
extension reinstall; it is not a passing run. The focused `*-final.log` files
and the two complete suite directories contain the successful qualification.
The legacy unpinned `reduce_sum_i64` boundary now reports
`generic_numeric_estimate_unavailable`, not unsupported arithmetic. A live
EXPLAIN with default auto-load enabled verifies that reason; explicitly
disabling auto-load instead produces `generic_auto_load_disabled`.

Installed production module SHA-256:

- PG18: `00bc1b33fa818b3b98823346af812e0d61ded429c42edbfde512024e7e7c53bb`
- PG19: `6a1359c6593c8235fa23e8d3a845ebe9d6d9aaa63ec0126f4f55e0c46b754a86`

The release build used by PG18 measurements matches its installed module.
This is local dirty-tree Metal evidence, not fresh upstream PostgreSQL
regression, CUDA, concurrent-backend, cold/indexed baseline or release
qualification. The overall TODO remains open.

## Matched local measurements

Apple M2 Max / 64 GB, Metal, PostgreSQL 18.4; five warmups and ten paired raw
observations per cell against the configured parallel PostgreSQL baseline.
All eight cells match PostgreSQL exactly. The six new bigint cells additionally
verify a Rust-derived 128-bit fixture SUM/count oracle. Seven cells select and
verify `parallel_dense_numeric`, process every measured input row on the GPU
and record zero stock fallback. The eighth cleanly chooses PostgreSQL.

| Workload | Rows | pg_accel ms | PostgreSQL ms | Warm ratio | Separate load ms |
|---|---:|---:|---:|---:|---:|
| Bigint global SUM/AVG | 250K | 4.328 | 7.714 | 1.78x | 46.577 |
| Bigint global SUM/AVG | 1M | 8.430 | 15.170 | 1.80x | 91.218 |
| Bigint three-key rollup | 250K | 3.749 | 12.059 | 3.22x | 58.193 |
| Bigint three-key rollup | 1M | 6.600 | 28.486 | 4.32x | 127.511 |
| Bigint filtered/mixed | 250K | 4.185 | 10.946 | 2.62x | 58.520 |
| Bigint filtered/mixed | 1M | 26.805 | 26.408 | 0.99x; native, no GPU credit | 142.667 |
| Existing NUMERIC global control | 250K | 5.218 | 8.508 | 1.63x | 77.360 |
| Existing NUMERIC global control | 1M | 9.168 | 17.562 | 1.92x | 208.472 |

The declined 1M filtered/mixed cell has matching native plan signatures and
no selected-but-undispatched scan. Its failure ledger retains one
`p3_parity_noise` item for a 1.5% native timing difference; it is not a GPU
performance result. The other three workload ledgers are empty. No admission
gates or coefficients were weakened to obtain a dispatch.

First-use refresh plus execution still loses: at 1M, global bigint takes
71.828 versus 15.763 ms (59.377 ms refresh); the three-key query takes 123.698
versus 27.698 ms (92.052 ms refresh, 22.278 ms artifact preparation). The 250K
filtered/mixed query takes 33.223 versus 11.200 ms. Separate initial load is
also excluded from warm medians. GPU-cell variation is 10.78–20.70% CV; these
are local warm characterizations, not a universal speedup or a before/after
performance claim. The NUMERIC control confirms a current warm win, not the
absence of all possible regressions.
