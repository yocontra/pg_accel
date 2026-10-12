# Integer extrema: 2026-09-29

Integer-only global and grouped rollups now accept int2/int4 MIN and MAX
independently or alongside SUM/AVG/COUNT. Shared expression/filter pairs use
one state; the existing four-state and three-dictionary-key limits remain.
Independent INT4 aggregate FILTER, query-level ranges/Boolean masks, and exact
point/constant-area spatial restrictions compose with extrema. PostgreSQL
retains HAVING, ordering and limits above the aggregate.

Extrema preserve their input SQL type and return NULL for empty/all-NULL
states. Rejected measure rows still create groups, whereas WHERE rejects rows
before group creation. Atomic one-shot execution uses signed int32 min/max
atomics, endpoint sentinels and independent non-NULL counts. Requested extrema
scratch participates in exact workspace accounting. Hierarchical and bounded
partial execution merge extrema, and extrema-only states skip SUM arithmetic.
No host reduction, CPU fallback, planner forcing, cost coefficient, logical
wire version or C ABI layout change was added.

NUMERIC/INT8 extrema and decimal/mixed shapes containing integer extrema remain
native, as do unqualified expressions and joins. Existing `sum_avg` EXPLAIN
specialization labels remain stable; physical execution is
`parallel_dense_integer`.

## Verification scope

Core tests pass 1,146, benchmark tests 571, native CTests 32/32 (81.22 seconds),
coverage-tool tests 154 and documentation tests 21. Both PG18/19 Clippy builds
pass. Both strict final full SQL suites pass 86/86 files:
[PG18](sql-pg18-final/results.tsv) and [PG19](sql-pg19-final/results.tsv).
The initial 81/86 PG19 run is retained in `sql-pg19/`; all five corrected tests
also passed individually before the full reruns.

SQL123 checks 96 normally planned shapes over 1,000,017 rows, with zero through
three nullable keys, int2/int4 endpoints, one through four independent states,
independent FILTER NULLs, all-rejected measures/WHERE and combined spatial/scalar
predicates. Exact scale-sensitive rows are compared with native PostgreSQL;
spatial results additionally use a Cartesian oracle. Prepared global and
grouped HAVING/order/limit queries test updates, rollback, all-NULL and empty
input. SQL104 retains its decline/counter guard with genuinely unsupported
INT8 extrema in place of newly supported INT2/INT4 cases. SQL96/99 retain
their adjacent lifecycle/decline guards using INT8 extrema; SQL100/101 retain
their exact serial-decline/no-dispatch requirements using mixed INT8/date
COUNT plus INT4 extrema. SQL105 moves hidden MIN/MAX HAVING into positive
native-equivalence/dispatch coverage and retains standard deviation and
volatile predicates as negative cases.

Core tests cover all 31 nonempty combinations of SUM/AVG/COUNT/MIN/MAX for
INT2 and INT4, global/grouped, with/without independent filters. Native tests
cover multi-state hierarchical/atomic execution, extrema at signed endpoints,
independent NULLs including entirely NULL states, spatial-mask/range rejection,
chunked accumulation, empty reuse, malformed NULL/mask bytes and failure
publication. Every final GPU test and benchmark runs serially.

Development failures remain in `verification/`: stale native-decline
expectations, test descriptor input/count-mode assumptions, and an old native
fixture that requested atomic one-shot workspace for a multi-batch session.
The session test now requests partial workspace with session execution flags;
its correctness and failure-recovery checks are unchanged. SQL fixture attempts
also caught a missing POINT typmod and a polygon below the unchanged
spatial-work floor. The final fixture has
explicit types and sufficient polygon complexity; production gates were not
weakened. The first full PG19 suite exposed SQL96/99/100/101/105's stale extrema
decline expectations; that failed run is retained separately from final runs.
Coverage baseline updates preserve 37 semantic families and the 90%
thresholds, expanding the inventory to 86 files and 408 assertions.

This is dirty-tree local Metal evidence, not fresh upstream PostgreSQL
regression, compiler-derived coverage, CUDA or whole-release qualification.
The full TODO remains open.

## Matched local measurements

Metal on Apple M2 Max / 64 GB, PostgreSQL 18.4, five warmups and ten paired
observations against the configured parallel PostgreSQL baseline. All eight
cells match PostgreSQL exactly, verify `parallel_dense_integer`, consume the
results, process the full measured input on the GPU, and record zero fallback.
All four failure ledgers are empty. The synthetic control also checks its
independent Rust-derived result oracle.

| Query | Rows | pg_accel median | PostgreSQL median | Warm speedup | Separate load |
|---|---:|---:|---:|---:|---:|
| [Passenger extrema/total/average](nyc_taxi_passenger_extrema/report.md) | 1M | 4.77 ms | 19.62 ms | 4.11x | 82.83 ms |
| Passenger extrema/total/average | 12,748,986 | 35.53 ms | 175.67 ms | 4.94x | 703.51 ms |
| [JFK passenger extrema/total/average](nyc_taxi_airport_passenger_extrema/report.md) | 1M | 8.44 ms | 38.62 ms | 4.58x | 236.18 ms |
| JFK passenger extrema/total/average | 12,748,986 | 37.58 ms | 251.96 ms | 6.70x | 2663.54 ms |
| [Existing multi-state control](grouped_multimeasure_integer/report.md) | 250K | 8.61 ms | 12.87 ms | 1.49x | 65.61 ms |
| Existing multi-state control | 1M | 8.99 ms | 33.70 ms | 3.75x | 167.60 ms |
| [Existing JFK integer control](nyc_taxi_airport_passengers/report.md) | 1M | 8.03 ms | 38.79 ms | 4.83x | 241.32 ms |
| Existing JFK integer control | 12,748,986 | 34.70 ms | 252.28 ms | 7.27x | 2663.93 ms |

The new query groups by vendor and calculates MIN/MAX/SUM/AVG(passenger_count)
plus COUNT(*). Its JFK variant adds the pinned constant MultiPolygon spatial
predicate. Both keep every original corpus row/outlier at import; every
execution reaggregates the input. Only the spatial mask/dictionary artifacts
are cached, not aggregate results.

First use still loses. Full-month refresh-plus-execution costs 744.61 ms versus
174.97 ms native without the spatial filter, and 6003.71 versus 267.92 ms for
JFK. The JFK probe includes 2719.28 ms refresh and 3253.12 ms fresh artifact
construction. The 1M probes also lose: 62.77 versus 19.74 ms and 329.20 versus
38.28 ms. These costs are excluded from warm medians. Accelerator CV is 12.65%
and 19.60% on the complete-month new queries; the 1M non-spatial CV is 21.03%.
Controls characterize this shared-host build, not a paired before/after
improvement. These are not cold-cache, indexed-spatial, concurrency or release
claims.

## Build provenance

The dirty tree is based on `5c7fbd0a6381f823befd823ec47137a8a602e94d`.
Installed module SHA-256 values are
`d2f367594d75e4c9ac0f29818627ef13d2927e106f177a5a8bc65e07d8af87e4` (PG18)
and `76b98802b338fc4c8e004cb5a9327b5e55848cf006cca7c1fc8366617173db67`
(PG19). Repo-owned postmasters retain their inherited 4096-descriptor limit;
no global host setting was changed.

Reproduce after matching installation and pinned-corpus import:

```sh
PG_CONFIG=/Users/contra/Projects/pg_accel/.pgaccel/postgres/install/18.4/bin/pg_config \
PG_ACCEL_PG_MAJOR=18 target/release/pg_accel_bench run \
  --workload nyc_taxi_passenger_extrema \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --cache-mode warm --timing raw --iterations 10 --warmup 5 \
  --capture-plans --artifacts-dir <fresh-directory>
```

Use each linked workload's name, running separately without concurrent tests,
builds or other benchmarks. The harness owns/recreates `bench_nyc_taxi` and
`bench_multimeasure_sales`; use a development database without unrelated data
under those names. Each report directory retains raw samples, plans, fixture
provenance, effective settings and lifecycle costs. `verification/` retains
test/build logs, initial failures and the final reviewed SQL inventory.
